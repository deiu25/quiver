//! `sources` table accessors.
//!
//! Phase 3 only inserts/updates rows; Phase 5 will use the row to drive
//! GitHub re-pull logic.

use chrono::Utc;
use rusqlite::{Connection, params};

pub fn upsert(conn: &Connection, id: &str, type_: &str, location: &str) -> anyhow::Result<()> {
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO sources (id, type, location, last_pulled_at, last_commit_sha)
         VALUES (?, ?, ?, ?, NULL)
         ON CONFLICT(id) DO UPDATE SET
             type = excluded.type,
             location = excluded.location,
             last_pulled_at = excluded.last_pulled_at",
        params![id, type_, location, now],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open;

    #[test]
    fn upsert_inserts_then_updates_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("t.sqlite")).unwrap();
        upsert(&conn, "gh:foo/bar", "github", "https://github.com/foo/bar").unwrap();
        upsert(&conn, "gh:foo/bar", "github", "https://github.com/foo/bar2").unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM sources", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);

        let loc: String = conn
            .query_row(
                "SELECT location FROM sources WHERE id = ?",
                params!["gh:foo/bar"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(loc, "https://github.com/foo/bar2");
    }
}
