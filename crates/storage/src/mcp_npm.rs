//! Cache layer for npm registry metadata used during MCP server ingestion.
//!
//! Schema: see `migrations/007_mcp_npm_cache.sql`. Rows live keyed by the
//! npm package name (e.g. `@context7/mcp-server`). The TTL check happens
//! in [`get`] — a stale row returns `None` so the caller refetches and
//! [`upsert`]s a fresh copy.

use anyhow::Context;
use chrono::{DateTime, Duration, Utc};
use rusqlite::{Connection, params};

/// Default cache lifetime — 30 days. Long enough to keep registry traffic
/// negligible while still picking up new package descriptions within a
/// reasonable window.
pub const DEFAULT_TTL_DAYS: i64 = 30;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NpmCacheRow {
    pub package: String,
    pub fetched_at: DateTime<Utc>,
    pub description: Option<String>,
    pub keywords: Vec<String>,
    pub repository: Option<String>,
    pub homepage: Option<String>,
    pub readme: Option<String>,
}

/// Look up a cached row. Returns `None` for a cache miss OR for a row
/// older than `ttl_days`.
pub fn get(conn: &Connection, package: &str, ttl_days: i64) -> anyhow::Result<Option<NpmCacheRow>> {
    let mut stmt = conn.prepare(
        "SELECT package, fetched_at, description, keywords_json, \
                repository, homepage, readme \
         FROM mcp_npm_cache WHERE package = ?1",
    )?;
    let mut rows = stmt.query(params![package])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let parsed = row_to_cache_row(row)?;
    if is_expired(parsed.fetched_at, ttl_days) {
        return Ok(None);
    }
    Ok(Some(parsed))
}

/// Upsert a fresh row. Caller is responsible for setting `fetched_at` to
/// `Utc::now()` (or whichever clock is being injected for tests).
pub fn upsert(conn: &Connection, row: &NpmCacheRow) -> anyhow::Result<()> {
    let keywords_json = serde_json::to_string(&row.keywords).context("encode keywords as JSON")?;
    conn.execute(
        "INSERT INTO mcp_npm_cache \
            (package, fetched_at, description, keywords_json, repository, homepage, readme) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
         ON CONFLICT(package) DO UPDATE SET \
            fetched_at  = excluded.fetched_at, \
            description = excluded.description, \
            keywords_json = excluded.keywords_json, \
            repository  = excluded.repository, \
            homepage    = excluded.homepage, \
            readme      = excluded.readme",
        params![
            row.package,
            row.fetched_at.to_rfc3339(),
            row.description,
            keywords_json,
            row.repository,
            row.homepage,
            row.readme,
        ],
    )?;
    Ok(())
}

fn is_expired(fetched_at: DateTime<Utc>, ttl_days: i64) -> bool {
    Utc::now() - fetched_at > Duration::days(ttl_days)
}

fn row_to_cache_row(row: &rusqlite::Row<'_>) -> anyhow::Result<NpmCacheRow> {
    let package: String = row.get(0)?;
    let fetched_at_str: String = row.get(1)?;
    let description: Option<String> = row.get(2)?;
    let keywords_json: Option<String> = row.get(3)?;
    let repository: Option<String> = row.get(4)?;
    let homepage: Option<String> = row.get(5)?;
    let readme: Option<String> = row.get(6)?;

    let fetched_at = DateTime::parse_from_rfc3339(&fetched_at_str)
        .with_context(|| format!("parse fetched_at {fetched_at_str}"))?
        .with_timezone(&Utc);
    let keywords: Vec<String> = match keywords_json {
        Some(raw) => serde_json::from_str(&raw).unwrap_or_default(),
        None => Vec::new(),
    };

    Ok(NpmCacheRow {
        package,
        fetched_at,
        description,
        keywords,
        repository,
        homepage,
        readme,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open;
    use chrono::TimeZone;

    fn fresh_conn() -> Connection {
        let dir = tempfile::tempdir().unwrap();
        // Leak the tempdir; only need it for the lifetime of this conn,
        // which is bounded by the test scope.
        let path = dir.keep().join("quiver.sqlite");
        open(&path).unwrap()
    }

    fn sample(package: &str, ago_days: i64) -> NpmCacheRow {
        NpmCacheRow {
            package: package.into(),
            fetched_at: Utc::now() - Duration::days(ago_days),
            description: Some("a sample package".into()),
            keywords: vec!["mcp".into(), "test".into()],
            repository: Some("https://github.com/x/y".into()),
            homepage: None,
            readme: Some("# x".into()),
        }
    }

    #[test]
    fn upsert_then_get_round_trips() {
        let conn = fresh_conn();
        let row = sample("@scope/pkg", 0);
        upsert(&conn, &row).unwrap();
        let got = get(&conn, "@scope/pkg", DEFAULT_TTL_DAYS).unwrap().unwrap();
        assert_eq!(got.package, row.package);
        assert_eq!(got.description, row.description);
        assert_eq!(got.keywords, row.keywords);
        assert_eq!(got.repository, row.repository);
        assert_eq!(got.readme, row.readme);
    }

    #[test]
    fn get_returns_none_for_missing_package() {
        let conn = fresh_conn();
        assert!(get(&conn, "missing", 30).unwrap().is_none());
    }

    #[test]
    fn get_returns_none_for_expired_row() {
        let conn = fresh_conn();
        let row = sample("@expired/pkg", 31);
        upsert(&conn, &row).unwrap();
        assert!(get(&conn, "@expired/pkg", 30).unwrap().is_none());
    }

    #[test]
    fn upsert_replaces_existing_row() {
        let conn = fresh_conn();
        let mut row = sample("@scope/pkg", 0);
        upsert(&conn, &row).unwrap();
        row.description = Some("updated".into());
        row.fetched_at = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        upsert(&conn, &row).unwrap();
        // ttl=99999 ensures the 2026-01-01 row is still considered fresh
        // for the purposes of the round-trip assertion.
        let got = get(&conn, "@scope/pkg", 99_999).unwrap().unwrap();
        assert_eq!(got.description.as_deref(), Some("updated"));
        assert_eq!(got.fetched_at.to_rfc3339(), row.fetched_at.to_rfc3339());
    }
}
