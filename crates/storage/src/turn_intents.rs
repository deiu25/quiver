//! `turn_intents` accessors. Phase 8 v5 — async LLM intent cache.
//!
//! Stores the LLM-classified read-only-vs-mutation verdict for a
//! UserPromptSubmit prompt so the subsequent PreToolUse hook can read it
//! without re-running the classifier on the critical path. The detached
//! `quiver hook classify-intent` child writes one row per turn; PreToolUse
//! reads the freshest row inside a TTL (default 600 s) and suppresses the
//! veto when `is_mutation=false`.

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnIntentRow {
    pub session_id: String,
    pub prompt_hash: String,
    pub is_mutation: bool,
    pub classifier: String,
    pub reason: Option<String>,
    /// Unix seconds.
    pub classified_at: i64,
}

/// UPSERT a row for `(session_id, prompt_hash)`. Last write wins.
pub fn record(
    conn: &Connection,
    session_id: &str,
    prompt_hash: &str,
    is_mutation: bool,
    classifier: &str,
    reason: Option<&str>,
    classified_at: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO turn_intents
            (session_id, prompt_hash, is_mutation, classifier, reason, classified_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(session_id, prompt_hash) DO UPDATE SET
             is_mutation   = excluded.is_mutation,
             classifier    = excluded.classifier,
             reason        = excluded.reason,
             classified_at = excluded.classified_at",
        params![
            session_id,
            prompt_hash,
            is_mutation as i64,
            classifier,
            reason,
            classified_at,
        ],
    )?;
    Ok(())
}

/// Latest verdict for a session that is at most `max_age_secs` old. `None`
/// when no matching row exists or every row is stale. `now` is the caller's
/// view of unix-seconds (lets tests pin a clock).
pub fn get_latest(
    conn: &Connection,
    session_id: &str,
    now: i64,
    max_age_secs: i64,
) -> Result<Option<TurnIntentRow>> {
    let cutoff = now.saturating_sub(max_age_secs);
    let row = conn
        .query_row(
            "SELECT session_id, prompt_hash, is_mutation, classifier, reason, classified_at
             FROM turn_intents
             WHERE session_id = ? AND classified_at >= ?
             ORDER BY classified_at DESC
             LIMIT 1",
            params![session_id, cutoff],
            |r| {
                Ok(TurnIntentRow {
                    session_id: r.get(0)?,
                    prompt_hash: r.get(1)?,
                    is_mutation: r.get::<_, i64>(2)? != 0,
                    classifier: r.get(3)?,
                    reason: r.get(4)?,
                    classified_at: r.get(5)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

/// Cheap fnv1a-64 of the trimmed prompt. Used as a stable cache key without
/// putting the raw prompt on disk beyond the `reason` column.
pub fn prompt_hash(prompt: &str) -> String {
    let trimmed = prompt.trim();
    let mut h: u64 = 0xcbf29ce484222325;
    for b in trimmed.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_test() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::open(&dir.path().join("q.sqlite")).unwrap();
        (dir, conn)
    }

    #[test]
    fn record_roundtrips_all_fields() {
        let (_d, conn) = open_test();
        record(
            &conn,
            "s1",
            "abc",
            false,
            "sonnet-api",
            Some("explain"),
            100,
        )
        .unwrap();
        let r = get_latest(&conn, "s1", 100, 600).unwrap().unwrap();
        assert_eq!(r.session_id, "s1");
        assert_eq!(r.prompt_hash, "abc");
        assert!(!r.is_mutation);
        assert_eq!(r.classifier, "sonnet-api");
        assert_eq!(r.reason.as_deref(), Some("explain"));
        assert_eq!(r.classified_at, 100);
    }

    #[test]
    fn upsert_overwrites_on_same_pk() {
        let (_d, conn) = open_test();
        record(&conn, "s1", "abc", false, "sonnet-api", None, 100).unwrap();
        record(&conn, "s1", "abc", true, "sonnet-cli", Some("retry"), 200).unwrap();
        let r = get_latest(&conn, "s1", 200, 600).unwrap().unwrap();
        assert!(r.is_mutation);
        assert_eq!(r.classifier, "sonnet-cli");
        assert_eq!(r.reason.as_deref(), Some("retry"));
        assert_eq!(r.classified_at, 200);
    }

    #[test]
    fn get_latest_returns_freshest_row() {
        let (_d, conn) = open_test();
        record(&conn, "s1", "old", true, "sonnet-api", None, 50).unwrap();
        record(&conn, "s1", "new", false, "sonnet-api", None, 100).unwrap();
        let r = get_latest(&conn, "s1", 100, 600).unwrap().unwrap();
        assert_eq!(r.prompt_hash, "new");
        assert_eq!(r.classified_at, 100);
        assert!(!r.is_mutation);
    }

    #[test]
    fn get_latest_skips_rows_older_than_ttl() {
        let (_d, conn) = open_test();
        record(&conn, "s1", "stale", false, "sonnet-api", None, 50).unwrap();
        // now=1000, max_age=300 ⇒ cutoff=700, row at 50 is older.
        let r = get_latest(&conn, "s1", 1000, 300).unwrap();
        assert!(r.is_none(), "stale rows must be ignored");
    }

    #[test]
    fn get_latest_isolates_by_session() {
        let (_d, conn) = open_test();
        record(&conn, "s1", "h1", false, "sonnet-api", None, 100).unwrap();
        record(&conn, "s2", "h2", true, "sonnet-api", None, 100).unwrap();
        let r1 = get_latest(&conn, "s1", 100, 600).unwrap().unwrap();
        assert!(!r1.is_mutation);
        let r2 = get_latest(&conn, "s2", 100, 600).unwrap().unwrap();
        assert!(r2.is_mutation);
        let none = get_latest(&conn, "s3", 100, 600).unwrap();
        assert!(none.is_none(), "unknown session ⇒ None");
    }

    #[test]
    fn prompt_hash_is_deterministic_and_trim_stable() {
        assert_eq!(prompt_hash("explain auth"), prompt_hash("explain auth"));
        assert_eq!(
            prompt_hash("  explain auth  \n"),
            prompt_hash("explain auth")
        );
        assert_ne!(prompt_hash("explain auth"), prompt_hash("explain billing"));
    }

    #[test]
    fn prompt_hash_is_16_hex_chars() {
        let h = prompt_hash("anything");
        assert_eq!(h.len(), 16);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
