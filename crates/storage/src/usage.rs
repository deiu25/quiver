//! `usage_events` writer + `tool_scores` aggregator. Phase 4.
//!
//! `insert_event` is idempotent on `uuid` — re-running `quiver score` on the
//! same JSONL files MUST NOT double-count. `recompute_scores` rebuilds
//! `tool_scores` from scratch (DELETE + INSERT inside a transaction): the
//! table is a derived projection, so a clean rebuild is simpler than
//! incremental updates and stays correct as `usage_events` evolves.

use anyhow::Context;
use chrono::{DateTime, Utc};
use quiver_core::usage::{Outcome, UsageEvent};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

/// Default half-life (in days) used to decay FP/bypass demerit weights.
/// Override at runtime via `QUIVER_DEMERIT_HALFLIFE_DAYS`.
pub const DEFAULT_DEMERIT_HALFLIFE_DAYS: f64 = 14.0;

/// Maximum number of distinct task signatures stored per tool in
/// `tool_scores.demerit_signatures_json`. The reranker reads this list at
/// query time, so capping it keeps the JSON small and rerank-cheap.
pub const DEFAULT_DEMERIT_TOP_N_SIGS: usize = 20;

/// Read `QUIVER_DEMERIT_HALFLIFE_DAYS` (default 14, clamped > 0). Operators
/// can shorten or extend the decay window without rebuilding.
pub fn demerit_halflife_days() -> f64 {
    std::env::var("QUIVER_DEMERIT_HALFLIFE_DAYS")
        .ok()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|v| *v > 0.0)
        .unwrap_or(DEFAULT_DEMERIT_HALFLIFE_DAYS)
}

/// Per-tool demerit aggregate consumed by the reranker.
#[derive(Debug, Clone, Serialize, Default)]
pub struct DemeritAggregate {
    pub demerit_count: f64,
    /// (signature, decayed_weight) pairs, sorted by weight descending,
    /// truncated to at most `DEFAULT_DEMERIT_TOP_N_SIGS` entries.
    pub signatures: Vec<(String, f64)>,
}

/// JSON shape for `tool_scores.demerit_signatures_json` entries.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct DemeritSignature {
    pub sig: String,
    pub weight: f64,
}

/// INSERT OR IGNORE — returns true if the row was new, false if `uuid`
/// collided with an existing event. Events without `uuid` are always inserted
/// (no dedupe key), so callers should prefer to set `uuid` whenever possible.
///
/// `usage_events.tool_id` has a FOREIGN KEY into `tools(id)` and SQLite
/// foreign-key enforcement is on for this connection — callers MUST upsert a
/// matching `tools` row before inserting events for it. The session_jsonl
/// replay ingestor skips events for un-catalogued tool ids.
pub fn insert_event(conn: &Connection, e: &UsageEvent) -> anyhow::Result<bool> {
    let occurred = e.occurred_at.to_rfc3339();
    let outcome = e.outcome.as_str();
    let n = conn.execute(
        "INSERT OR IGNORE INTO usage_events
            (uuid, tool_id, session_id, project, task_text, outcome,
             duration_ms, cost_usd, occurred_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            e.uuid,
            e.tool_id,
            e.session_id,
            e.project,
            e.task_text,
            outcome,
            e.duration_ms,
            e.cost_usd,
            occurred,
        ],
    )?;
    Ok(n > 0)
}

pub fn count_events(conn: &Connection) -> anyhow::Result<i64> {
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM usage_events", [], |r| r.get(0))?;
    Ok(n)
}

#[derive(Debug, Clone)]
pub struct EventRow {
    pub id: i64,
    pub uuid: Option<String>,
    pub tool_id: String,
    pub session_id: Option<String>,
    pub project: Option<String>,
    pub task_text: Option<String>,
    pub outcome: Option<Outcome>,
    pub duration_ms: Option<i64>,
    pub cost_usd: Option<f64>,
    pub occurred_at: String,
}

pub fn list_events(
    conn: &Connection,
    tool_id: Option<&str>,
    limit: usize,
) -> anyhow::Result<Vec<EventRow>> {
    let (sql, params): (&str, Vec<Box<dyn rusqlite::ToSql>>) = match tool_id {
        Some(id) => (
            "SELECT id, uuid, tool_id, session_id, project, task_text, outcome,
                    duration_ms, cost_usd, occurred_at
             FROM usage_events
             WHERE tool_id = ?
             ORDER BY occurred_at DESC
             LIMIT ?",
            vec![Box::new(id.to_string()), Box::new(limit as i64)],
        ),
        None => (
            "SELECT id, uuid, tool_id, session_id, project, task_text, outcome,
                    duration_ms, cost_usd, occurred_at
             FROM usage_events
             ORDER BY occurred_at DESC
             LIMIT ?",
            vec![Box::new(limit as i64)],
        ),
    };
    let mut stmt = conn.prepare(sql)?;
    let param_refs: Vec<&dyn rusqlite::ToSql> = params
        .iter()
        .map(|b| b.as_ref() as &dyn rusqlite::ToSql)
        .collect();
    let rows = stmt
        .query_map(param_refs.as_slice(), |row| {
            let outcome: Option<String> = row.get(6)?;
            Ok(EventRow {
                id: row.get(0)?,
                uuid: row.get(1)?,
                tool_id: row.get(2)?,
                session_id: row.get(3)?,
                project: row.get(4)?,
                task_text: row.get(5)?,
                outcome: outcome.as_deref().and_then(Outcome::parse),
                duration_ms: row.get(7)?,
                cost_usd: row.get(8)?,
                occurred_at: row.get(9)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[derive(Debug, Clone, Copy)]
struct ScoreAcc {
    successes: u64,
    total: u64,
    cost_sum: f64,
    cost_count: u64,
}

impl ScoreAcc {
    fn new() -> Self {
        Self {
            successes: 0,
            total: 0,
            cost_sum: 0.0,
            cost_count: 0,
        }
    }
}

/// Aggregate `false_positive`/`bypassed` rows from `agent_suggestions` into
/// per-tool decayed demerit weights. Phase 9 auto-tuner.
///
/// Both signals weigh `1.0` (per the user-confirmed equal-weight design):
/// `false_positive=1` is a manual flag from the web UI, `bypassed=1` is
/// recorded by the PreToolUse hook the second time the model retries a
/// vetoed call. A row with both flags still counts once — we OR the flags
/// rather than sum.
///
/// Time decay: `weight = exp(-Δt_days * ln(2) / halflife_days)`. Half-life
/// defaults to `DEFAULT_DEMERIT_HALFLIFE_DAYS` (14 days) and is overridable
/// via `QUIVER_DEMERIT_HALFLIFE_DAYS`.
///
/// Per-tool the helper returns `(demerit_count, signatures)`:
/// - `demerit_count` is the sum of decayed weights across **all** rows.
/// - `signatures` is a sorted (descending) `Vec<(sig, weight)>` of at most
///   `top_n_per_tool` entries, where each `weight` is the **max** decayed
///   weight observed across multiple rows sharing the same signature
///   (avoids double-counting the same task pattern).
pub fn aggregate_demerits(
    conn: &Connection,
    halflife_days: f64,
    top_n_per_tool: usize,
    now: DateTime<Utc>,
) -> anyhow::Result<std::collections::HashMap<String, DemeritAggregate>> {
    use std::collections::HashMap;

    if halflife_days <= 0.0 {
        return Ok(HashMap::new());
    }
    let decay_k = std::f64::consts::LN_2 / halflife_days;

    let mut stmt = conn.prepare(
        "SELECT tool_id, task_signature, suggested_at
         FROM agent_suggestions
         WHERE (false_positive = 1 OR bypassed = 1)
           AND task_signature IS NOT NULL",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);

    // Per-tool: sum of decayed weights → demerit_count.
    // Per-(tool, sig): max decayed weight (so retried failures don't
    // multi-count the same signature into the JSON list).
    let mut totals: HashMap<String, f64> = HashMap::new();
    let mut sig_max: HashMap<String, HashMap<String, f64>> = HashMap::new();

    for (tool_id, sig, suggested_at) in rows {
        let Ok(ts) = DateTime::parse_from_rfc3339(&suggested_at) else {
            continue;
        };
        let dt_days = (now - ts.with_timezone(&Utc)).num_seconds() as f64 / 86_400.0;
        // Future timestamps clamp to "now" — don't reward clock skew.
        let dt_days = dt_days.max(0.0);
        let weight = (-decay_k * dt_days).exp();

        *totals.entry(tool_id.clone()).or_insert(0.0) += weight;

        let entry = sig_max
            .entry(tool_id)
            .or_default()
            .entry(sig)
            .or_insert(0.0);
        if weight > *entry {
            *entry = weight;
        }
    }

    let mut out: HashMap<String, DemeritAggregate> = HashMap::new();
    for (tool_id, count) in totals {
        let mut sigs: Vec<(String, f64)> = sig_max
            .get(&tool_id)
            .map(|m| m.iter().map(|(s, w)| (s.clone(), *w)).collect())
            .unwrap_or_default();
        sigs.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        sigs.truncate(top_n_per_tool);
        out.insert(
            tool_id,
            DemeritAggregate {
                demerit_count: count,
                signatures: sigs,
            },
        );
    }
    Ok(out)
}

/// Rebuild `tool_scores` from `usage_events` and `agent_suggestions`.
///
/// Returns the number of tool rows written. Intended to be called after a
/// batch of `insert_event` calls or after the agent loop has flipped
/// `false_positive`/`bypassed` flags.
///
/// Phase 9: also aggregates negative-feedback signals (`false_positive`,
/// `bypassed`) from `agent_suggestions` into the new `demerit_*` columns
/// so the recommender's `DemeritReranker` can apply a permanent
/// time-decayed haircut to repeatedly-wrong tools.
pub fn recompute_scores(conn: &mut Connection) -> anyhow::Result<usize> {
    // Pull every event in one query, aggregate Rust-side (SQLite has no median).
    let mut stmt =
        conn.prepare("SELECT tool_id, outcome, cost_usd, duration_ms FROM usage_events")?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<f64>>(2)?,
                row.get::<_, Option<i64>>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);

    use std::collections::HashMap;
    let mut accs: HashMap<String, ScoreAcc> = HashMap::new();
    let mut durations: HashMap<String, Vec<i64>> = HashMap::new();

    for (tool_id, outcome, cost, dur) in rows {
        let acc = accs.entry(tool_id.clone()).or_insert_with(ScoreAcc::new);
        acc.total += 1;
        if outcome.as_deref() == Some("success") {
            acc.successes += 1;
        }
        if let Some(c) = cost {
            acc.cost_sum += c;
            acc.cost_count += 1;
        }
        if let Some(d) = dur {
            durations.entry(tool_id).or_default().push(d);
        }
    }

    let now_dt = Utc::now();
    let now = now_dt.to_rfc3339();

    // Phase 9: aggregate FP+bypass demerits before opening the write
    // transaction so the read holds no write lock. The result is keyed by
    // tool_id; tools without negative feedback simply aren't in the map.
    let halflife = demerit_halflife_days();
    let mut demerits = aggregate_demerits(conn, halflife, DEFAULT_DEMERIT_TOP_N_SIGS, now_dt)?;

    let tx = conn.transaction()?;
    tx.execute("DELETE FROM tool_scores", [])
        .context("clear tool_scores")?;

    // Union of tools touched by usage events and tools that earned demerits
    // — a tool can have FP feedback without a single usage_events row.
    let mut all_tool_ids: std::collections::BTreeSet<String> = accs.keys().cloned().collect();
    all_tool_ids.extend(demerits.keys().cloned());

    for tool_id in &all_tool_ids {
        let acc = accs.get(tool_id).copied().unwrap_or_else(ScoreAcc::new);
        let success_rate = if acc.total == 0 {
            None
        } else {
            Some(acc.successes as f64 / acc.total as f64)
        };
        let avg_cost = if acc.cost_count == 0 {
            None
        } else {
            Some(acc.cost_sum / acc.cost_count as f64)
        };
        let median_dur = durations.get(tool_id).map(|v| {
            let mut s = v.clone();
            s.sort_unstable();
            s[s.len() / 2]
        });

        let demerit = demerits.remove(tool_id).unwrap_or_default();
        let demerit_sigs_json = if demerit.signatures.is_empty() {
            None
        } else {
            let payload: Vec<DemeritSignature> = demerit
                .signatures
                .iter()
                .map(|(s, w)| DemeritSignature {
                    sig: s.clone(),
                    weight: *w,
                })
                .collect();
            Some(serde_json::to_string(&payload).context("encode demerit_signatures_json")?)
        };

        tx.execute(
            "INSERT INTO tool_scores
                (tool_id, success_rate, sample_size, avg_cost_usd,
                 median_duration_ms, score_updated_at,
                 demerit_count, demerit_updated_at, demerit_signatures_json)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                tool_id,
                success_rate,
                acc.total as i64,
                avg_cost,
                median_dur,
                now,
                demerit.demerit_count,
                now,
                demerit_sigs_json,
            ],
        )?;
    }
    tx.commit()?;
    Ok(all_tool_ids.len())
}

/// Tools that have NOT been used in the last `older_than_days` days.
pub fn dead_weight(
    conn: &Connection,
    older_than_days: u32,
) -> anyhow::Result<Vec<(String, String, Option<String>)>> {
    let cutoff = format!("-{older_than_days} days");
    let mut stmt = conn.prepare(
        "SELECT t.id, t.name,
                (SELECT MAX(occurred_at) FROM usage_events WHERE tool_id = t.id) AS last_used
         FROM tools t
         WHERE NOT EXISTS (
             SELECT 1 FROM usage_events u
             WHERE u.tool_id = t.id
               AND u.occurred_at >= datetime('now', ?1)
         )
         ORDER BY t.id",
    )?;
    let rows = stmt
        .query_map([cutoff], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Most-recent `occurred_at` for a tool, used by `stats --tool`.
pub fn last_used(conn: &Connection, tool_id: &str) -> anyhow::Result<Option<String>> {
    let v: Option<String> = conn
        .query_row(
            "SELECT MAX(occurred_at) FROM usage_events WHERE tool_id = ?",
            params![tool_id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open;
    use chrono::TimeZone;

    fn evt(
        uuid: &str,
        tool: &str,
        outcome: Outcome,
        cost: Option<f64>,
        dur: Option<i64>,
    ) -> UsageEvent {
        UsageEvent {
            uuid: Some(uuid.to_string()),
            tool_id: tool.to_string(),
            session_id: Some("sess-1".into()),
            project: Some("quiver".into()),
            task_text: Some("a task".into()),
            outcome,
            duration_ms: dur,
            cost_usd: cost,
            occurred_at: Utc.with_ymd_and_hms(2026, 5, 3, 12, 0, 0).unwrap(),
        }
    }

    fn seed_tool(conn: &Connection, id: &str) {
        conn.execute(
            "INSERT OR IGNORE INTO tools (id, type, name, triggers, examples, requires,
                                          enabled, added_at, last_seen_at)
             VALUES (?, 'skill', ?, '[]', '[]', '[]', 1,
                     '2026-05-03T00:00:00+00:00', '2026-05-03T00:00:00+00:00')",
            params![id, id],
        )
        .unwrap();
    }

    fn tmp_conn() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("t.sqlite")).unwrap();
        (dir, conn)
    }

    #[test]
    fn insert_dedupes_on_uuid() {
        let (_d, conn) = tmp_conn();
        seed_tool(&conn, "skill:caveman");
        let e = evt("u1", "skill:caveman", Outcome::Success, None, None);
        assert!(insert_event(&conn, &e).unwrap());
        assert!(!insert_event(&conn, &e).unwrap());
        assert_eq!(count_events(&conn).unwrap(), 1);
    }

    #[test]
    fn list_events_filters_by_tool() {
        let (_d, conn) = tmp_conn();
        seed_tool(&conn, "skill:a");
        seed_tool(&conn, "skill:b");
        insert_event(&conn, &evt("u1", "skill:a", Outcome::Success, None, None)).unwrap();
        insert_event(&conn, &evt("u2", "skill:b", Outcome::Failure, None, None)).unwrap();
        let all = list_events(&conn, None, 10).unwrap();
        assert_eq!(all.len(), 2);
        let only_b = list_events(&conn, Some("skill:b"), 10).unwrap();
        assert_eq!(only_b.len(), 1);
        assert_eq!(only_b[0].tool_id, "skill:b");
        assert_eq!(only_b[0].outcome, Some(Outcome::Failure));
    }

    #[test]
    fn recompute_scores_aggregates_correctly() {
        let (_d, mut conn) = tmp_conn();
        seed_tool(&conn, "skill:x");
        seed_tool(&conn, "skill:y");
        insert_event(
            &conn,
            &evt("u1", "skill:x", Outcome::Success, Some(0.10), Some(100)),
        )
        .unwrap();
        insert_event(
            &conn,
            &evt("u2", "skill:x", Outcome::Success, Some(0.20), Some(200)),
        )
        .unwrap();
        insert_event(
            &conn,
            &evt("u3", "skill:x", Outcome::Failure, Some(0.30), Some(300)),
        )
        .unwrap();
        insert_event(&conn, &evt("u4", "skill:y", Outcome::Failure, None, None)).unwrap();
        let touched = recompute_scores(&mut conn).unwrap();
        assert_eq!(touched, 2);
        let scores = crate::scores::list(&conn, Some("skill:x")).unwrap();
        assert_eq!(scores.len(), 1);
        let row = &scores[0];
        assert!((row.success_rate.unwrap() - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(row.sample_size, Some(3));
        assert!((row.avg_cost_usd.unwrap() - 0.20).abs() < 1e-9);
        assert_eq!(row.median_duration_ms, Some(200));
    }

    #[test]
    fn recompute_clears_stale_rows() {
        let (_d, mut conn) = tmp_conn();
        seed_tool(&conn, "skill:x");
        insert_event(&conn, &evt("u1", "skill:x", Outcome::Success, None, None)).unwrap();
        recompute_scores(&mut conn).unwrap();
        conn.execute("DELETE FROM usage_events", []).unwrap();
        let touched = recompute_scores(&mut conn).unwrap();
        assert_eq!(touched, 0);
        assert!(crate::scores::list(&conn, None).unwrap().is_empty());
    }

    // ----------------------------------------------------------------------
    // Phase 9 — auto-tuner demerit aggregation
    // ----------------------------------------------------------------------

    fn insert_suggestion(
        conn: &Connection,
        tool_id: &str,
        sig: Option<&str>,
        suggested_at: &str,
        false_positive: bool,
        bypassed: bool,
    ) -> i64 {
        conn.execute(
            "INSERT INTO agent_suggestions
                (session_id, tool_id, task_text, score, suggested_at,
                 accepted, accepted_at, level, task_signature,
                 vetoed, bypassed, nudged, false_positive)
             VALUES ('sess', ?, NULL, NULL, ?, 0, NULL, 'strong', ?,
                     0, ?, 0, ?)",
            params![
                tool_id,
                suggested_at,
                sig,
                bypassed as i64,
                false_positive as i64,
            ],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn recompute_scores_aggregates_false_positives() {
        let (_d, mut conn) = tmp_conn();
        seed_tool(&conn, "skill:fp");
        let now = Utc::now().to_rfc3339();
        insert_suggestion(&conn, "skill:fp", Some("Bash:foo"), &now, true, false);
        insert_suggestion(&conn, "skill:fp", Some("Bash:bar"), &now, true, false);
        insert_suggestion(&conn, "skill:fp", Some("Bash:baz"), &now, true, false);

        recompute_scores(&mut conn).unwrap();
        let rows = crate::scores::list(&conn, Some("skill:fp")).unwrap();
        assert_eq!(rows.len(), 1);
        // 3 fresh FP rows ≈ 3.0 (decay ≈ 0 on the order of seconds).
        assert!(
            (rows[0].demerit_count - 3.0).abs() < 0.01,
            "got {}",
            rows[0].demerit_count
        );
        assert!(rows[0].demerit_updated_at.is_some());
        let json = rows[0].demerit_signatures_json.as_deref().unwrap();
        let parsed: Vec<DemeritSignature> = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.len(), 3);
    }

    #[test]
    fn recompute_scores_aggregates_bypassed_equal_to_fp() {
        // Equal-weight design: one FP + one bypass on different tools but
        // identical timestamps must produce identical demerit_count.
        let (_d, mut conn) = tmp_conn();
        seed_tool(&conn, "skill:fp");
        seed_tool(&conn, "skill:by");
        let now = Utc::now().to_rfc3339();
        insert_suggestion(&conn, "skill:fp", Some("Bash:x"), &now, true, false);
        insert_suggestion(&conn, "skill:by", Some("Bash:y"), &now, false, true);

        recompute_scores(&mut conn).unwrap();
        let fp = crate::scores::list(&conn, Some("skill:fp")).unwrap()[0].demerit_count;
        let by = crate::scores::list(&conn, Some("skill:by")).unwrap()[0].demerit_count;
        assert!((fp - by).abs() < 1e-9, "fp={fp}, by={by}");
    }

    #[test]
    fn recompute_scores_decays_old_demerits() {
        // 14-day half-life default. A row 28 days old (2× halflife) decays
        // to 1/4. A 14-day-old row decays to 1/2. Fresh row ≈ 1.
        let (_d, mut conn) = tmp_conn();
        seed_tool(&conn, "skill:old");
        let now = Utc::now();
        let two_halflives_ago = (now - chrono::Duration::days(28)).to_rfc3339();
        insert_suggestion(
            &conn,
            "skill:old",
            Some("Bash:x"),
            &two_halflives_ago,
            true,
            false,
        );

        recompute_scores(&mut conn).unwrap();
        let row = &crate::scores::list(&conn, Some("skill:old")).unwrap()[0];
        // exp(-2 ln2) = 0.25
        assert!(
            (row.demerit_count - 0.25).abs() < 0.005,
            "got {}",
            row.demerit_count
        );
    }

    #[test]
    fn recompute_scores_writes_top_signatures_per_tool() {
        let (_d, mut conn) = tmp_conn();
        seed_tool(&conn, "skill:many");
        let now = Utc::now();
        for i in 0..25 {
            // Stagger timestamps so weights differ → ordering is well-defined.
            let ts = (now - chrono::Duration::hours(i)).to_rfc3339();
            let sig = format!("Bash:cmd{i}");
            insert_suggestion(&conn, "skill:many", Some(&sig), &ts, true, false);
        }
        recompute_scores(&mut conn).unwrap();
        let row = &crate::scores::list(&conn, Some("skill:many")).unwrap()[0];
        let sigs: Vec<DemeritSignature> =
            serde_json::from_str(row.demerit_signatures_json.as_deref().unwrap()).unwrap();
        assert_eq!(sigs.len(), DEFAULT_DEMERIT_TOP_N_SIGS);
        // Sorted by weight descending → cmd0 (most recent) first.
        assert_eq!(sigs[0].sig, "Bash:cmd0");
        assert!(sigs[0].weight > sigs[19].weight);
    }

    #[test]
    fn recompute_scores_skips_null_signatures() {
        // UserPromptSubmit-origin rows have signature=NULL. They must NOT
        // count as demerits even if false_positive=1. With a usage event
        // the tool still gets a tool_scores row, but demerit_count = 0.
        let (_d, mut conn) = tmp_conn();
        seed_tool(&conn, "skill:null");
        let now = Utc::now().to_rfc3339();
        insert_suggestion(&conn, "skill:null", None, &now, true, false);
        insert_event(
            &conn,
            &evt("u-null", "skill:null", Outcome::Success, None, None),
        )
        .unwrap();

        recompute_scores(&mut conn).unwrap();
        let rows = crate::scores::list(&conn, Some("skill:null")).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].demerit_count, 0.0);
        assert!(rows[0].demerit_signatures_json.is_none());
    }

    #[test]
    fn recompute_scores_keeps_tool_with_only_demerits() {
        // A tool can have FP feedback without any usage_events row. The
        // recompute must still write the demerit fields.
        let (_d, mut conn) = tmp_conn();
        seed_tool(&conn, "skill:demerit-only");
        let now = Utc::now().to_rfc3339();
        insert_suggestion(
            &conn,
            "skill:demerit-only",
            Some("Bash:z"),
            &now,
            true,
            false,
        );

        let touched = recompute_scores(&mut conn).unwrap();
        assert_eq!(touched, 1);
        let rows = crate::scores::list(&conn, Some("skill:demerit-only")).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].sample_size.is_none() || rows[0].sample_size == Some(0));
        assert!(rows[0].demerit_count > 0.5);
    }

    #[test]
    fn aggregate_demerits_respects_env_halflife() {
        let (_d, conn) = tmp_conn();
        seed_tool(&conn, "skill:hl");
        let now = Utc::now();
        let seven_days_ago = (now - chrono::Duration::days(7)).to_rfc3339();
        insert_suggestion(
            &conn,
            "skill:hl",
            Some("Bash:x"),
            &seven_days_ago,
            true,
            false,
        );

        // halflife=7 days → 7-day-old row weighs 0.5.
        let agg = aggregate_demerits(&conn, 7.0, DEFAULT_DEMERIT_TOP_N_SIGS, now).unwrap();
        let w = agg.get("skill:hl").unwrap().demerit_count;
        assert!((w - 0.5).abs() < 0.01, "halflife=7 → got {w}");

        // halflife=14 days → 7-day-old row weighs sqrt(0.5) ≈ FRAC_1_SQRT_2.
        let agg = aggregate_demerits(&conn, 14.0, DEFAULT_DEMERIT_TOP_N_SIGS, now).unwrap();
        let w = agg.get("skill:hl").unwrap().demerit_count;
        assert!(
            (w - std::f64::consts::FRAC_1_SQRT_2).abs() < 0.01,
            "halflife=14 → got {w}"
        );
    }

    #[test]
    fn list_demerits_returns_only_nonzero_sorted_desc() {
        let (_d, mut conn) = tmp_conn();
        seed_tool(&conn, "skill:a");
        seed_tool(&conn, "skill:b");
        seed_tool(&conn, "skill:c");
        let now = Utc::now().to_rfc3339();
        // a: 3 FP (count ≈ 3.0)
        for _ in 0..3 {
            insert_suggestion(&conn, "skill:a", Some("Bash:a"), &now, true, false);
        }
        // b: 1 FP (count ≈ 1.0)
        insert_suggestion(&conn, "skill:b", Some("Bash:b"), &now, true, false);
        // c: no demerits
        insert_event(&conn, &evt("u-c", "skill:c", Outcome::Success, None, None)).unwrap();

        recompute_scores(&mut conn).unwrap();
        let list = crate::scores::list_demerits(&conn, 10).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].tool_id, "skill:a");
        assert_eq!(list[1].tool_id, "skill:b");
        assert!(list[0].demerit_count > list[1].demerit_count);
    }
}
