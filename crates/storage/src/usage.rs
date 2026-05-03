//! `usage_events` writer + `tool_scores` aggregator. Phase 4.
//!
//! `insert_event` is idempotent on `uuid` — re-running `toolhub score` on the
//! same JSONL files MUST NOT double-count. `recompute_scores` rebuilds
//! `tool_scores` from scratch (DELETE + INSERT inside a transaction): the
//! table is a derived projection, so a clean rebuild is simpler than
//! incremental updates and stays correct as `usage_events` evolves.

use anyhow::Context;
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use toolhub_core::usage::{Outcome, UsageEvent};

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

/// Rebuild `tool_scores` from `usage_events`. Returns the number of tool rows
/// written. Intended to be called after a batch of `insert_event` calls.
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

    let now = Utc::now().to_rfc3339();
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM tool_scores", [])
        .context("clear tool_scores")?;
    for (tool_id, acc) in &accs {
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
        tx.execute(
            "INSERT INTO tool_scores
                (tool_id, success_rate, sample_size, avg_cost_usd,
                 median_duration_ms, score_updated_at)
             VALUES (?, ?, ?, ?, ?, ?)",
            params![
                tool_id,
                success_rate,
                acc.total as i64,
                avg_cost,
                median_dur,
                now,
            ],
        )?;
    }
    tx.commit()?;
    Ok(accs.len())
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
}
