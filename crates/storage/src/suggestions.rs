//! `agent_suggestions` accessors. Phase 6.
//!
//! The daily-task agent writes one row per top-1 recommendation it makes for
//! a watched session. When the user invokes the suggested tool within
//! `acceptance_window_minutes`, `mark_accepted` flips `accepted=1`. Digest
//! summarises acceptance via `acceptance_stats`.

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuggestionRow {
    pub id: i64,
    pub session_id: String,
    pub tool_id: String,
    pub task_text: Option<String>,
    pub score: Option<f64>,
    pub suggested_at: String,
    pub accepted: bool,
    pub accepted_at: Option<String>,
    /// Phase 8 v3 (strict mode): policy band the row was emitted at —
    /// `'hint' | 'strong' | 'mandatory'`. None for legacy rows.
    pub level: Option<String>,
    /// Stable digest of `(tool_name, salient input)` used by the
    /// PreToolUse single-veto-per-tuple rule. None for UserPromptSubmit-origin
    /// rows.
    pub task_signature: Option<String>,
    pub vetoed: bool,
    pub bypassed: bool,
    pub nudged: bool,
    pub false_positive: bool,
}

/// Insert a top-1 suggestion. Returns the new row id.
///
/// `level` and `task_signature` are optional Phase 8 v3 strict-mode fields:
/// callers that don't care (legacy callers, `quiver agent` advisory mode)
/// may pass `None`.
#[allow(clippy::too_many_arguments)]
pub fn record(
    conn: &Connection,
    session_id: &str,
    tool_id: &str,
    task_text: Option<&str>,
    score: Option<f64>,
    suggested_at: DateTime<Utc>,
    level: Option<&str>,
    task_signature: Option<&str>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO agent_suggestions
            (session_id, tool_id, task_text, score, suggested_at, accepted,
             level, task_signature)
         VALUES (?, ?, ?, ?, ?, 0, ?, ?)",
        params![
            session_id,
            tool_id,
            task_text,
            score,
            suggested_at.to_rfc3339(),
            level,
            task_signature,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Mark every still-pending suggestion for this `(session_id, tool_id)` pair
/// whose `suggested_at` is within `window_minutes` as accepted. Returns the
/// number of rows updated. Idempotent: a second matching `tool_use` is a
/// no-op because `accepted=0` is part of the predicate.
pub fn mark_accepted(
    conn: &Connection,
    session_id: &str,
    tool_id: &str,
    accepted_at: DateTime<Utc>,
    window_minutes: i64,
) -> Result<usize> {
    let cutoff = (accepted_at - Duration::minutes(window_minutes)).to_rfc3339();
    let n = conn.execute(
        "UPDATE agent_suggestions
            SET accepted = 1, accepted_at = ?
          WHERE session_id = ?
            AND tool_id = ?
            AND accepted = 0
            AND suggested_at >= ?",
        params![accepted_at.to_rfc3339(), session_id, tool_id, cutoff],
    )?;
    Ok(n)
}

/// Manually flip a single suggestion to `accepted=1`. Used by the web UI's
/// Accept button to backfill rows that the live `mark_accepted` window
/// missed (e.g. `quiver agent` was not running when the user invoked the
/// suggested tool). Idempotent — returns `Ok(false)` if the row was already
/// accepted or no row matches the id.
pub fn mark_accepted_by_id(conn: &Connection, id: i64, accepted_at: DateTime<Utc>) -> Result<bool> {
    let n = conn.execute(
        "UPDATE agent_suggestions
            SET accepted = 1, accepted_at = ?
          WHERE id = ?
            AND accepted = 0",
        params![accepted_at.to_rfc3339(), id],
    )?;
    Ok(n > 0)
}

/// Fetch a single suggestion by id, or `None` if absent.
pub fn find_by_id(conn: &Connection, id: i64) -> Result<Option<SuggestionRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, session_id, tool_id, task_text, score, suggested_at,
                accepted, accepted_at,
                level, task_signature, vetoed, bypassed, nudged, false_positive
         FROM agent_suggestions
         WHERE id = ?",
    )?;
    let mut rows = stmt.query_map(params![id], |row| {
        Ok(SuggestionRow {
            id: row.get(0)?,
            session_id: row.get(1)?,
            tool_id: row.get(2)?,
            task_text: row.get(3)?,
            score: row.get(4)?,
            suggested_at: row.get(5)?,
            accepted: row.get::<_, i64>(6)? != 0,
            accepted_at: row.get(7)?,
            level: row.get(8)?,
            task_signature: row.get(9)?,
            vetoed: row.get::<_, i64>(10)? != 0,
            bypassed: row.get::<_, i64>(11)? != 0,
            nudged: row.get::<_, i64>(12)? != 0,
            false_positive: row.get::<_, i64>(13)? != 0,
        })
    })?;
    match rows.next() {
        Some(r) => Ok(Some(r?)),
        None => Ok(None),
    }
}

/// Returns `true` if Quiver has already vetoed this `(session, signature)`
/// tuple. Powers the single-veto-per-tuple rule: a re-invocation of the same
/// denied tool call must pass through.
pub fn is_vetoed(conn: &Connection, session_id: &str, task_signature: &str) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM agent_suggestions
          WHERE session_id = ?
            AND task_signature = ?
            AND vetoed = 1",
        params![session_id, task_signature],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Mark a suggestion row as `vetoed=1`. The PreToolUse hook calls this once
/// it has emitted a `permissionDecision: deny` for the row's tuple.
pub fn mark_vetoed(conn: &Connection, id: i64) -> Result<bool> {
    let n = conn.execute(
        "UPDATE agent_suggestions SET vetoed = 1 WHERE id = ? AND vetoed = 0",
        params![id],
    )?;
    Ok(n > 0)
}

/// Mark a suggestion row as `bypassed=1`. Set when the model retries the
/// same tool call after a veto — strong false-positive signal.
pub fn mark_bypassed(conn: &Connection, id: i64) -> Result<bool> {
    let n = conn.execute(
        "UPDATE agent_suggestions SET bypassed = 1 WHERE id = ? AND bypassed = 0",
        params![id],
    )?;
    Ok(n > 0)
}

/// Mark a suggestion row as `nudged=1`. The Stop hook calls this after
/// emitting a `decision: block` so we never block twice for the same row.
pub fn mark_nudged(conn: &Connection, id: i64) -> Result<bool> {
    let n = conn.execute(
        "UPDATE agent_suggestions SET nudged = 1 WHERE id = ? AND nudged = 0",
        params![id],
    )?;
    Ok(n > 0)
}

/// Manually flag a row as a false positive (web UI, CLI). Feeds future
/// auto-tuner.
pub fn set_false_positive(conn: &Connection, id: i64) -> Result<bool> {
    let n = conn.execute(
        "UPDATE agent_suggestions SET false_positive = 1
          WHERE id = ? AND false_positive = 0",
        params![id],
    )?;
    Ok(n > 0)
}

/// Find the most recent unaccepted, un-nudged mandatory-band suggestion in
/// `session_id` whose `suggested_at` falls inside the last `window_minutes`.
/// Powers the Stop circuit-breaker.
pub fn pending_mandatory_for_session(
    conn: &Connection,
    session_id: &str,
    window_minutes: i64,
    now: DateTime<Utc>,
) -> Result<Option<SuggestionRow>> {
    let cutoff = (now - Duration::minutes(window_minutes)).to_rfc3339();
    let mut stmt = conn.prepare(
        "SELECT id, session_id, tool_id, task_text, score, suggested_at,
                accepted, accepted_at,
                level, task_signature, vetoed, bypassed, nudged, false_positive
           FROM agent_suggestions
          WHERE session_id = ?
            AND level = 'mandatory'
            AND accepted = 0
            AND nudged = 0
            AND suggested_at >= ?
          ORDER BY suggested_at DESC
          LIMIT 1",
    )?;
    let mut rows = stmt.query_map(params![session_id, cutoff], |row| {
        Ok(SuggestionRow {
            id: row.get(0)?,
            session_id: row.get(1)?,
            tool_id: row.get(2)?,
            task_text: row.get(3)?,
            score: row.get(4)?,
            suggested_at: row.get(5)?,
            accepted: row.get::<_, i64>(6)? != 0,
            accepted_at: row.get(7)?,
            level: row.get(8)?,
            task_signature: row.get(9)?,
            vetoed: row.get::<_, i64>(10)? != 0,
            bypassed: row.get::<_, i64>(11)? != 0,
            nudged: row.get::<_, i64>(12)? != 0,
            false_positive: row.get::<_, i64>(13)? != 0,
        })
    })?;
    match rows.next() {
        Some(r) => Ok(Some(r?)),
        None => Ok(None),
    }
}

/// Look up the most recent veto row for the given `(session, signature)`
/// tuple. Used by the PreToolUse path to flip `bypassed=1` when the model
/// retries the same denied call.
pub fn find_vetoed_row(
    conn: &Connection,
    session_id: &str,
    task_signature: &str,
) -> Result<Option<SuggestionRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, session_id, tool_id, task_text, score, suggested_at,
                accepted, accepted_at,
                level, task_signature, vetoed, bypassed, nudged, false_positive
           FROM agent_suggestions
          WHERE session_id = ?
            AND task_signature = ?
            AND vetoed = 1
          ORDER BY suggested_at DESC
          LIMIT 1",
    )?;
    let mut rows = stmt.query_map(params![session_id, task_signature], |row| {
        Ok(SuggestionRow {
            id: row.get(0)?,
            session_id: row.get(1)?,
            tool_id: row.get(2)?,
            task_text: row.get(3)?,
            score: row.get(4)?,
            suggested_at: row.get(5)?,
            accepted: row.get::<_, i64>(6)? != 0,
            accepted_at: row.get(7)?,
            level: row.get(8)?,
            task_signature: row.get(9)?,
            vetoed: row.get::<_, i64>(10)? != 0,
            bypassed: row.get::<_, i64>(11)? != 0,
            nudged: row.get::<_, i64>(12)? != 0,
            false_positive: row.get::<_, i64>(13)? != 0,
        })
    })?;
    match rows.next() {
        Some(r) => Ok(Some(r?)),
        None => Ok(None),
    }
}

/// `(suggested_count, accepted_count)` since `cutoff`.
pub fn acceptance_stats(conn: &Connection, since: DateTime<Utc>) -> Result<(i64, i64)> {
    let cutoff = since.to_rfc3339();
    let row = conn.query_row(
        "SELECT
            COUNT(*) AS n,
            COALESCE(SUM(accepted), 0) AS k
         FROM agent_suggestions
         WHERE suggested_at >= ?",
        params![cutoff],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
    )?;
    Ok(row)
}

pub fn list(conn: &Connection, session_id: Option<&str>) -> Result<Vec<SuggestionRow>> {
    let (sql, params): (&str, Vec<Box<dyn rusqlite::ToSql>>) = match session_id {
        Some(sid) => (
            "SELECT id, session_id, tool_id, task_text, score, suggested_at,
                    accepted, accepted_at,
                    level, task_signature, vetoed, bypassed, nudged, false_positive
             FROM agent_suggestions
             WHERE session_id = ?
             ORDER BY suggested_at DESC",
            vec![Box::new(sid.to_string())],
        ),
        None => (
            "SELECT id, session_id, tool_id, task_text, score, suggested_at,
                    accepted, accepted_at,
                    level, task_signature, vetoed, bypassed, nudged, false_positive
             FROM agent_suggestions
             ORDER BY suggested_at DESC",
            vec![],
        ),
    };
    let mut stmt = conn.prepare(sql)?;
    let param_refs: Vec<&dyn rusqlite::ToSql> = params
        .iter()
        .map(|b| b.as_ref() as &dyn rusqlite::ToSql)
        .collect();
    let rows = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok(SuggestionRow {
                id: row.get(0)?,
                session_id: row.get(1)?,
                tool_id: row.get(2)?,
                task_text: row.get(3)?,
                score: row.get(4)?,
                suggested_at: row.get(5)?,
                accepted: row.get::<_, i64>(6)? != 0,
                accepted_at: row.get(7)?,
                level: row.get(8)?,
                task_signature: row.get(9)?,
                vetoed: row.get::<_, i64>(10)? != 0,
                bypassed: row.get::<_, i64>(11)? != 0,
                nudged: row.get::<_, i64>(12)? != 0,
                false_positive: row.get::<_, i64>(13)? != 0,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open;

    fn tmp_conn() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("t.sqlite")).unwrap();
        (dir, conn)
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

    #[test]
    fn record_then_mark_accepted_flips_row() {
        let (_d, conn) = tmp_conn();
        seed_tool(&conn, "skill:caveman");
        let suggested = Utc::now();
        record(
            &conn,
            "sess-1",
            "skill:caveman",
            Some("be terse"),
            Some(0.9),
            suggested,
            None,
            None,
        )
        .unwrap();

        let n = mark_accepted(&conn, "sess-1", "skill:caveman", suggested, 60).unwrap();
        assert_eq!(n, 1);

        let rows = list(&conn, Some("sess-1")).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].accepted);
        assert!(rows[0].accepted_at.is_some());
    }

    #[test]
    fn mark_accepted_ignores_old_suggestions() {
        let (_d, conn) = tmp_conn();
        seed_tool(&conn, "skill:caveman");
        let old = Utc::now() - Duration::hours(2);
        record(
            &conn,
            "sess-1",
            "skill:caveman",
            None,
            None,
            old,
            None,
            None,
        )
        .unwrap();

        // Only suggestions in the last 60 min count; this one is 2h old.
        let n = mark_accepted(&conn, "sess-1", "skill:caveman", Utc::now(), 60).unwrap();
        assert_eq!(n, 0);

        let rows = list(&conn, None).unwrap();
        assert!(!rows[0].accepted);
    }

    #[test]
    fn mark_accepted_is_idempotent() {
        let (_d, conn) = tmp_conn();
        seed_tool(&conn, "skill:x");
        let ts = Utc::now();
        record(&conn, "s", "skill:x", None, None, ts, None, None).unwrap();
        assert_eq!(mark_accepted(&conn, "s", "skill:x", ts, 60).unwrap(), 1);
        // Already accepted — second call is a no-op.
        assert_eq!(mark_accepted(&conn, "s", "skill:x", ts, 60).unwrap(), 0);
    }

    #[test]
    fn mark_accepted_by_id_flips_pending_row() {
        let (_d, conn) = tmp_conn();
        seed_tool(&conn, "skill:caveman");
        let suggested = Utc::now() - Duration::hours(3); // outside window
        let id = record(
            &conn,
            "sess-1",
            "skill:caveman",
            None,
            None,
            suggested,
            None,
            None,
        )
        .unwrap();

        // Window-based call would not flip it (too old).
        assert_eq!(
            mark_accepted(&conn, "sess-1", "skill:caveman", Utc::now(), 60).unwrap(),
            0
        );
        // But manual by-id does.
        assert!(mark_accepted_by_id(&conn, id, Utc::now()).unwrap());

        let row = find_by_id(&conn, id).unwrap().unwrap();
        assert!(row.accepted);
        assert!(row.accepted_at.is_some());
    }

    #[test]
    fn mark_accepted_by_id_is_idempotent() {
        let (_d, conn) = tmp_conn();
        seed_tool(&conn, "skill:x");
        let id = record(&conn, "s", "skill:x", None, None, Utc::now(), None, None).unwrap();
        assert!(mark_accepted_by_id(&conn, id, Utc::now()).unwrap());
        // Second call: row already accepted — no-op, returns false.
        assert!(!mark_accepted_by_id(&conn, id, Utc::now()).unwrap());
    }

    #[test]
    fn mark_accepted_by_id_unknown_id_returns_false() {
        let (_d, conn) = tmp_conn();
        assert!(!mark_accepted_by_id(&conn, 9999, Utc::now()).unwrap());
        assert!(find_by_id(&conn, 9999).unwrap().is_none());
    }

    #[test]
    fn acceptance_stats_counts_correctly() {
        let (_d, conn) = tmp_conn();
        seed_tool(&conn, "skill:a");
        seed_tool(&conn, "skill:b");
        let now = Utc::now();
        record(&conn, "s1", "skill:a", None, None, now, None, None).unwrap();
        record(&conn, "s1", "skill:b", None, None, now, None, None).unwrap();
        record(&conn, "s2", "skill:a", None, None, now, None, None).unwrap();
        mark_accepted(&conn, "s1", "skill:a", now, 60).unwrap();

        let (n, k) = acceptance_stats(&conn, now - Duration::hours(1)).unwrap();
        assert_eq!(n, 3);
        assert_eq!(k, 1);
    }

    #[test]
    fn record_persists_level_and_signature() {
        let (_d, conn) = tmp_conn();
        seed_tool(&conn, "skill:caveman");
        let id = record(
            &conn,
            "s",
            "skill:caveman",
            Some("be terse"),
            Some(0.82),
            Utc::now(),
            Some("mandatory"),
            Some("Bash:cargo test"),
        )
        .unwrap();
        let row = find_by_id(&conn, id).unwrap().unwrap();
        assert_eq!(row.level.as_deref(), Some("mandatory"));
        assert_eq!(row.task_signature.as_deref(), Some("Bash:cargo test"));
        assert!(!row.vetoed);
        assert!(!row.bypassed);
        assert!(!row.nudged);
        assert!(!row.false_positive);
    }

    #[test]
    fn mark_vetoed_and_is_vetoed_round_trip() {
        let (_d, conn) = tmp_conn();
        seed_tool(&conn, "skill:caveman");
        let id = record(
            &conn,
            "s",
            "skill:caveman",
            None,
            Some(0.82),
            Utc::now(),
            Some("mandatory"),
            Some("Bash:cargo test"),
        )
        .unwrap();
        assert!(!is_vetoed(&conn, "s", "Bash:cargo test").unwrap());
        assert!(mark_vetoed(&conn, id).unwrap());
        assert!(is_vetoed(&conn, "s", "Bash:cargo test").unwrap());
        // Idempotent — second call returns false (no rows updated).
        assert!(!mark_vetoed(&conn, id).unwrap());
    }

    #[test]
    fn find_vetoed_row_returns_most_recent() {
        let (_d, conn) = tmp_conn();
        seed_tool(&conn, "skill:caveman");
        let now = Utc::now();
        let older = record(
            &conn,
            "s",
            "skill:caveman",
            None,
            Some(0.8),
            now - Duration::minutes(30),
            Some("mandatory"),
            Some("Bash:foo"),
        )
        .unwrap();
        let newer = record(
            &conn,
            "s",
            "skill:caveman",
            None,
            Some(0.9),
            now,
            Some("mandatory"),
            Some("Bash:foo"),
        )
        .unwrap();
        mark_vetoed(&conn, older).unwrap();
        mark_vetoed(&conn, newer).unwrap();

        let row = find_vetoed_row(&conn, "s", "Bash:foo").unwrap().unwrap();
        assert_eq!(row.id, newer);
    }

    #[test]
    fn mark_bypassed_and_nudged_idempotent() {
        let (_d, conn) = tmp_conn();
        seed_tool(&conn, "skill:caveman");
        let id = record(
            &conn,
            "s",
            "skill:caveman",
            None,
            Some(0.82),
            Utc::now(),
            Some("mandatory"),
            None,
        )
        .unwrap();
        assert!(mark_bypassed(&conn, id).unwrap());
        assert!(!mark_bypassed(&conn, id).unwrap());
        assert!(mark_nudged(&conn, id).unwrap());
        assert!(!mark_nudged(&conn, id).unwrap());
        assert!(set_false_positive(&conn, id).unwrap());
        assert!(!set_false_positive(&conn, id).unwrap());

        let row = find_by_id(&conn, id).unwrap().unwrap();
        assert!(row.bypassed);
        assert!(row.nudged);
        assert!(row.false_positive);
    }

    #[test]
    fn pending_mandatory_returns_only_unaccepted_unnudged_in_window() {
        let (_d, conn) = tmp_conn();
        seed_tool(&conn, "skill:a");
        seed_tool(&conn, "skill:b");
        seed_tool(&conn, "skill:c");
        seed_tool(&conn, "skill:d");
        seed_tool(&conn, "skill:e");
        let now = Utc::now();
        // Hint band — ineligible.
        record(
            &conn,
            "s",
            "skill:a",
            None,
            Some(0.5),
            now,
            Some("hint"),
            None,
        )
        .unwrap();
        // Accepted — ineligible.
        let acc = record(
            &conn,
            "s",
            "skill:b",
            None,
            Some(0.85),
            now,
            Some("mandatory"),
            None,
        )
        .unwrap();
        mark_accepted_by_id(&conn, acc, now).unwrap();
        // Already nudged — ineligible.
        let nud = record(
            &conn,
            "s",
            "skill:c",
            None,
            Some(0.9),
            now,
            Some("mandatory"),
            None,
        )
        .unwrap();
        mark_nudged(&conn, nud).unwrap();
        // Outside window.
        record(
            &conn,
            "s",
            "skill:d",
            None,
            Some(0.85),
            now - Duration::hours(3),
            Some("mandatory"),
            None,
        )
        .unwrap();
        // Eligible — pick this one.
        let want = record(
            &conn,
            "s",
            "skill:e",
            None,
            Some(0.9),
            now - Duration::minutes(5),
            Some("mandatory"),
            None,
        )
        .unwrap();

        let row = pending_mandatory_for_session(&conn, "s", 60, now)
            .unwrap()
            .unwrap();
        assert_eq!(row.id, want);
        assert_eq!(row.tool_id, "skill:e");
    }
}
