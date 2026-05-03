//! `sources` table accessors.
//!
//! Phase 3 added the bare `upsert` so the MCP `add_source` stub could record
//! a row. Phase 5 added `upsert_full` (writes `last_commit_sha`), plus `list`,
//! `get`, and `delete` so `quiver update`/`remove` can drive the re-pull
//! lifecycle.

use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRow {
    pub id: String,
    pub r#type: String,
    pub location: String,
    pub last_pulled_at: Option<DateTime<Utc>>,
    pub last_commit_sha: Option<String>,
}

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

/// Insert or update a source with full provenance — the timestamp the user
/// pulled it and the upstream commit sha. `update` uses this on every re-pull;
/// `add` uses it on the first ingestion.
pub fn upsert_full(
    conn: &Connection,
    id: &str,
    type_: &str,
    location: &str,
    last_pulled_at: DateTime<Utc>,
    last_commit_sha: Option<&str>,
) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO sources (id, type, location, last_pulled_at, last_commit_sha)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET
             type = excluded.type,
             location = excluded.location,
             last_pulled_at = excluded.last_pulled_at,
             last_commit_sha = excluded.last_commit_sha",
        params![
            id,
            type_,
            location,
            last_pulled_at.to_rfc3339(),
            last_commit_sha
        ],
    )?;
    Ok(())
}

fn parse_ts(s: &str) -> anyhow::Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(s)?.with_timezone(&Utc))
}

pub fn list(conn: &Connection) -> anyhow::Result<Vec<SourceRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, type, location, last_pulled_at, last_commit_sha
         FROM sources ORDER BY id",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = Vec::with_capacity(rows.len());
    for (id, ty, loc, pulled, sha) in rows {
        out.push(SourceRow {
            id,
            r#type: ty,
            location: loc,
            last_pulled_at: pulled.as_deref().map(parse_ts).transpose()?,
            last_commit_sha: sha,
        });
    }
    Ok(out)
}

pub fn get(conn: &Connection, id: &str) -> anyhow::Result<Option<SourceRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, type, location, last_pulled_at, last_commit_sha
         FROM sources WHERE id = ?",
    )?;
    let mut rows = stmt.query(params![id])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let pulled: Option<String> = row.get(3)?;
    Ok(Some(SourceRow {
        id: row.get(0)?,
        r#type: row.get(1)?,
        location: row.get(2)?,
        last_pulled_at: pulled.as_deref().map(parse_ts).transpose()?,
        last_commit_sha: row.get(4)?,
    }))
}

/// Delete a source row by id. Returns the number of rows deleted (0 or 1).
/// Caller is responsible for removing tools whose `source_repo` matches.
pub fn delete(conn: &Connection, id: &str) -> anyhow::Result<usize> {
    let n = conn.execute("DELETE FROM sources WHERE id = ?", params![id])?;
    Ok(n)
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

    #[test]
    fn upsert_full_round_trip_through_get_and_list() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("t.sqlite")).unwrap();
        let pulled = Utc::now();
        upsert_full(
            &conn,
            "gh:a/b",
            "github",
            "https://github.com/a/b",
            pulled,
            Some("deadbeefcafe"),
        )
        .unwrap();

        let row = get(&conn, "gh:a/b").unwrap().unwrap();
        assert_eq!(row.r#type, "github");
        assert_eq!(row.location, "https://github.com/a/b");
        assert_eq!(row.last_commit_sha.as_deref(), Some("deadbeefcafe"));
        assert!(row.last_pulled_at.is_some());

        upsert_full(
            &conn,
            "gh:a/b",
            "github",
            "https://github.com/a/b",
            pulled,
            Some("newsha"),
        )
        .unwrap();
        let row2 = get(&conn, "gh:a/b").unwrap().unwrap();
        assert_eq!(row2.last_commit_sha.as_deref(), Some("newsha"));

        upsert_full(
            &conn,
            "gh:c/d",
            "github",
            "https://github.com/c/d",
            pulled,
            None,
        )
        .unwrap();
        let all = list(&conn).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, "gh:a/b");
        assert_eq!(all[1].id, "gh:c/d");
    }

    #[test]
    fn delete_returns_count_and_get_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("t.sqlite")).unwrap();
        upsert(&conn, "gh:foo/bar", "github", "https://github.com/foo/bar").unwrap();
        assert_eq!(delete(&conn, "gh:foo/bar").unwrap(), 1);
        assert_eq!(delete(&conn, "gh:foo/bar").unwrap(), 0);
        assert!(get(&conn, "gh:foo/bar").unwrap().is_none());
    }
}
