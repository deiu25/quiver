use std::path::Path;

use anyhow::Context;
use refinery::Migration;
use rusqlite::Connection;

pub mod tools;

const M001: &str = include_str!("../migrations/001_init.sql");
const M002: &str = include_str!("../migrations/002_fts.sql");

fn migrations() -> anyhow::Result<Vec<Migration>> {
    Ok(vec![
        Migration::unapplied("V1__init", M001).context("parse V1__init")?,
        Migration::unapplied("V2__fts", M002).context("parse V2__fts")?,
    ])
}

/// Open a SQLite DB at `path` and run all pending migrations (001 + 002).
/// Migration 003 (`tools_vec`) is deferred until the `sqlite-vec` extension
/// is wired — see PLAN.md §6 and §3.
pub fn open(path: &Path) -> anyhow::Result<Connection> {
    let mut conn = Connection::open(path)
        .with_context(|| format!("open sqlite at {}", path.display()))?;
    let migs = migrations()?;
    refinery::Runner::new(&migs)
        .run(&mut conn)
        .context("run refinery migrations")?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_names(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master \
                 WHERE type IN ('table','view') ORDER BY name",
            )
            .unwrap();
        stmt.query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    #[test]
    fn open_creates_expected_tables() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("toolhub.sqlite")).unwrap();
        let names = table_names(&conn);
        for expected in ["tools", "usage_events", "tool_scores", "sources", "tools_fts"] {
            assert!(
                names.contains(&expected.to_string()),
                "missing {expected} in {names:?}"
            );
        }
    }

    #[test]
    fn open_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("toolhub.sqlite");
        let _c1 = open(&path).unwrap();
        let _c2 = open(&path).unwrap();
    }
}
