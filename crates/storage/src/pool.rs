//! r2d2 connection pool for the Quiver SQLite DB.
//!
//! The pool is wired so every connection it hands out has the `sqlite-vec`
//! extension auto-loaded (via [`crate::ensure_vec_extension`]) and finds the
//! schema already migrated. Open the pool once at startup and clone it into
//! axum/tower handlers.

use std::path::Path;

use anyhow::Context;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;

/// Open and migrate the DB at `path`, returning a ready-to-use connection
/// pool. Migrations run on a single bootstrap connection that is dropped
/// before the pool is built, so pooled handles never see a half-applied DB.
pub fn open_pool(path: &Path) -> anyhow::Result<Pool<SqliteConnectionManager>> {
    crate::ensure_vec_extension();
    // Bootstrap: run migrations on a single connection. `crate::open` is
    // idempotent (refinery skips already-applied migrations), so this is
    // cheap on subsequent calls too.
    let _bootstrap =
        crate::open(path).with_context(|| format!("bootstrap migrate {}", path.display()))?;
    drop(_bootstrap);

    let manager = SqliteConnectionManager::file(path);
    let pool = Pool::builder()
        .max_size(8)
        .build(manager)
        .with_context(|| format!("build r2d2 pool for {}", path.display()))?;
    Ok(pool)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_pool_creates_db_and_runs_migrations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("quiver.sqlite");

        let pool = open_pool(&path).unwrap();
        let conn = pool.get().unwrap();

        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master \
                 WHERE type IN ('table','view') ORDER BY name",
            )
            .unwrap();
        let names: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();

        for expected in [
            "tools",
            "usage_events",
            "tool_scores",
            "sources",
            "tools_fts",
            "agent_suggestions",
        ] {
            assert!(
                names.contains(&expected.to_string()),
                "missing {expected} in {names:?}"
            );
        }
    }

    #[test]
    fn open_pool_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("quiver.sqlite");

        let _p1 = open_pool(&path).unwrap();
        let _p2 = open_pool(&path).unwrap();
    }

    #[test]
    fn pooled_connection_can_run_vec_extension_query() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("quiver.sqlite");

        let pool = open_pool(&path).unwrap();
        let conn = pool.get().unwrap();

        // tools_vec is the sqlite-vec virtual table; if the extension didn't
        // load on this pooled connection, this query would error.
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tools_vec", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn pool_hands_out_independent_connections() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("quiver.sqlite");

        let pool = open_pool(&path).unwrap();
        let c1 = pool.get().unwrap();
        let c2 = pool.get().unwrap();

        // Both must be usable concurrently for read.
        c1.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
            .unwrap();
        c2.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
            .unwrap();
    }
}
