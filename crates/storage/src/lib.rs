use std::path::Path;
use std::sync::Once;

use anyhow::Context;
use refinery::Migration;
use rusqlite::Connection;

pub mod embeddings;
pub mod fts;
pub mod mcp_npm;
pub mod pool;
pub mod scores;
pub mod sources;
pub mod suggestions;
pub mod tools;
pub mod usage;

const M001: &str = include_str!("../migrations/001_init.sql");
const M002: &str = include_str!("../migrations/002_fts.sql");
const M003: &str = include_str!("../migrations/003_vec.sql");
const M004: &str = include_str!("../migrations/004_embeddings.sql");
const M005: &str = include_str!("../migrations/005_usage_uuid.sql");
const M006: &str = include_str!("../migrations/006_agent_suggestions.sql");
const M007: &str = include_str!("../migrations/007_mcp_npm_cache.sql");

fn migrations() -> anyhow::Result<Vec<Migration>> {
    Ok(vec![
        Migration::unapplied("V1__init", M001).context("parse V1__init")?,
        Migration::unapplied("V2__fts", M002).context("parse V2__fts")?,
        Migration::unapplied("V3__vec", M003).context("parse V3__vec")?,
        Migration::unapplied("V4__embeddings", M004).context("parse V4__embeddings")?,
        Migration::unapplied("V5__usage_uuid", M005).context("parse V5__usage_uuid")?,
        Migration::unapplied("V6__agent_suggestions", M006)
            .context("parse V6__agent_suggestions")?,
        Migration::unapplied("V7__mcp_npm_cache", M007).context("parse V7__mcp_npm_cache")?,
    ])
}

static VEC_INIT: Once = Once::new();

/// Register the sqlite-vec extension as an auto-extension so every
/// `Connection::open` after this point loads `vec0`. Idempotent.
pub fn ensure_vec_extension() {
    VEC_INIT.call_once(|| {
        // sqlite_vec::sqlite3_vec_init has the exact extension entry-point
        // signature `unsafe extern "C" fn(*mut sqlite3, *mut *mut c_char,
        // *const sqlite3_api_routines) -> c_int`. Cast through *const () to
        // bridge potential c_char vs i8/u8 platform differences without
        // dragging in libsqlite3-sys directly.
        type ExtInit = unsafe extern "C" fn(
            *mut rusqlite::ffi::sqlite3,
            *mut *mut std::os::raw::c_char,
            *const rusqlite::ffi::sqlite3_api_routines,
        ) -> std::os::raw::c_int;
        unsafe {
            let init: ExtInit = std::mem::transmute(sqlite_vec::sqlite3_vec_init as *const ());
            rusqlite::ffi::sqlite3_auto_extension(Some(init));
        }
    });
}

/// Open a SQLite DB at `path` and run all pending migrations (001 + 002).
/// Migration 003 (`tools_vec`) is deferred until the `sqlite-vec` extension
/// is wired — see PLAN.md §6 and §3.
pub fn open(path: &Path) -> anyhow::Result<Connection> {
    ensure_vec_extension();
    let mut conn =
        Connection::open(path).with_context(|| format!("open sqlite at {}", path.display()))?;
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
        let conn = open(&dir.path().join("quiver.sqlite")).unwrap();
        let names = table_names(&conn);
        for expected in [
            "tools",
            "usage_events",
            "tool_scores",
            "sources",
            "tools_fts",
        ] {
            assert!(
                names.contains(&expected.to_string()),
                "missing {expected} in {names:?}"
            );
        }
    }

    #[test]
    fn open_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("quiver.sqlite");
        let _c1 = open(&path).unwrap();
        let _c2 = open(&path).unwrap();
    }
}
