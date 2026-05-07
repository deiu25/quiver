//! Cache layer for npm registry metadata used during MCP server ingestion.
//!
//! Schema: `mcp_npm_cache` table — see migrations 007 + 008. Rows are
//! keyed by the npm package name (e.g. `@context7/mcp-server`).
//!
//! Two row flavours share the table:
//!
//!   * **Hit row** — `not_found = 0`, body fields populated. Returned by
//!     [`get`] as [`CacheStatus::Found`] until older than `ttl_days`.
//!   * **Tombstone** — `not_found = 1`, body fields empty. Written when
//!     the registry returned 404 so we don't re-hit the network for a
//!     known-bad package on every sync. Returned as
//!     [`CacheStatus::NotFound`] until expired; expired rows promote to
//!     [`CacheStatus::Miss`] so the caller refetches in case the package
//!     was since published.

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

/// Result of a cache probe. Distinguishes between "have positive
/// metadata" (Hit), "have a recent 404 tombstone" (NotFound), and
/// "nothing on file or expired" (Miss).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheStatus {
    Found(NpmCacheRow),
    NotFound,
    Miss,
}

/// Look up a cached row.
///
/// * Hit row → [`CacheStatus::Found`] when fresh, [`CacheStatus::Miss`]
///   when expired.
/// * Tombstone (404) → [`CacheStatus::NotFound`] when fresh,
///   [`CacheStatus::Miss`] when expired (gives the package a chance to
///   appear on npm).
/// * No row → [`CacheStatus::Miss`].
pub fn get(conn: &Connection, package: &str, ttl_days: i64) -> anyhow::Result<CacheStatus> {
    let mut stmt = conn.prepare(
        "SELECT package, fetched_at, description, keywords_json, \
                repository, homepage, readme, not_found \
         FROM mcp_npm_cache WHERE package = ?1",
    )?;
    let mut rows = stmt.query(params![package])?;
    let Some(row) = rows.next()? else {
        return Ok(CacheStatus::Miss);
    };
    let (parsed, not_found) = row_to_cache_row(row)?;
    if is_expired(parsed.fetched_at, ttl_days) {
        return Ok(CacheStatus::Miss);
    }
    if not_found {
        Ok(CacheStatus::NotFound)
    } else {
        Ok(CacheStatus::Found(parsed))
    }
}

/// Upsert a hit row. Always clears `not_found` (a fresh successful
/// fetch supersedes any prior tombstone).
pub fn upsert(conn: &Connection, row: &NpmCacheRow) -> anyhow::Result<()> {
    let keywords_json = serde_json::to_string(&row.keywords).context("encode keywords as JSON")?;
    conn.execute(
        "INSERT INTO mcp_npm_cache \
            (package, fetched_at, description, keywords_json, repository, homepage, readme, not_found) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0) \
         ON CONFLICT(package) DO UPDATE SET \
            fetched_at  = excluded.fetched_at, \
            description = excluded.description, \
            keywords_json = excluded.keywords_json, \
            repository  = excluded.repository, \
            homepage    = excluded.homepage, \
            readme      = excluded.readme, \
            not_found   = 0",
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

/// Upsert a tombstone for a package the registry refuses (404). Wipes
/// any stale body fields so a future debugger doesn't get confused.
pub fn upsert_tombstone(
    conn: &Connection,
    package: &str,
    fetched_at: DateTime<Utc>,
) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO mcp_npm_cache \
            (package, fetched_at, description, keywords_json, repository, homepage, readme, not_found) \
         VALUES (?1, ?2, NULL, '[]', NULL, NULL, NULL, 1) \
         ON CONFLICT(package) DO UPDATE SET \
            fetched_at    = excluded.fetched_at, \
            description   = NULL, \
            keywords_json = '[]', \
            repository    = NULL, \
            homepage      = NULL, \
            readme        = NULL, \
            not_found     = 1",
        params![package, fetched_at.to_rfc3339()],
    )?;
    Ok(())
}

fn is_expired(fetched_at: DateTime<Utc>, ttl_days: i64) -> bool {
    Utc::now() - fetched_at > Duration::days(ttl_days)
}

fn row_to_cache_row(row: &rusqlite::Row<'_>) -> anyhow::Result<(NpmCacheRow, bool)> {
    let package: String = row.get(0)?;
    let fetched_at_str: String = row.get(1)?;
    let description: Option<String> = row.get(2)?;
    let keywords_json: Option<String> = row.get(3)?;
    let repository: Option<String> = row.get(4)?;
    let homepage: Option<String> = row.get(5)?;
    let readme: Option<String> = row.get(6)?;
    let not_found_int: i64 = row.get(7)?;

    let fetched_at = DateTime::parse_from_rfc3339(&fetched_at_str)
        .with_context(|| format!("parse fetched_at {fetched_at_str}"))?
        .with_timezone(&Utc);
    let keywords: Vec<String> = match keywords_json {
        Some(raw) => serde_json::from_str(&raw).unwrap_or_default(),
        None => Vec::new(),
    };

    Ok((
        NpmCacheRow {
            package,
            fetched_at,
            description,
            keywords,
            repository,
            homepage,
            readme,
        },
        not_found_int != 0,
    ))
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
        let got = match get(&conn, "@scope/pkg", DEFAULT_TTL_DAYS).unwrap() {
            CacheStatus::Found(r) => r,
            other => panic!("expected Found, got {other:?}"),
        };
        assert_eq!(got.package, row.package);
        assert_eq!(got.description, row.description);
        assert_eq!(got.keywords, row.keywords);
        assert_eq!(got.repository, row.repository);
        assert_eq!(got.readme, row.readme);
    }

    #[test]
    fn get_returns_miss_for_missing_package() {
        let conn = fresh_conn();
        assert_eq!(get(&conn, "missing", 30).unwrap(), CacheStatus::Miss);
    }

    #[test]
    fn get_returns_miss_for_expired_hit_row() {
        let conn = fresh_conn();
        let row = sample("@expired/pkg", 31);
        upsert(&conn, &row).unwrap();
        assert_eq!(
            get(&conn, "@expired/pkg", 30).unwrap(),
            CacheStatus::Miss,
            "expired hit row promotes to Miss so caller refetches"
        );
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
        let got = match get(&conn, "@scope/pkg", 99_999).unwrap() {
            CacheStatus::Found(r) => r,
            other => panic!("expected Found, got {other:?}"),
        };
        assert_eq!(got.description.as_deref(), Some("updated"));
        assert_eq!(got.fetched_at.to_rfc3339(), row.fetched_at.to_rfc3339());
    }

    #[test]
    fn upsert_tombstone_returns_not_found_until_expired() {
        let conn = fresh_conn();
        upsert_tombstone(&conn, "@nope/missing", Utc::now()).unwrap();
        assert_eq!(
            get(&conn, "@nope/missing", DEFAULT_TTL_DAYS).unwrap(),
            CacheStatus::NotFound
        );
    }

    #[test]
    fn expired_tombstone_promotes_to_miss() {
        let conn = fresh_conn();
        upsert_tombstone(&conn, "@expired/tombstone", Utc::now() - Duration::days(60)).unwrap();
        assert_eq!(
            get(&conn, "@expired/tombstone", DEFAULT_TTL_DAYS).unwrap(),
            CacheStatus::Miss,
            "expired tombstone gets retried — package may have been published since"
        );
    }

    #[test]
    fn successful_upsert_clears_existing_tombstone() {
        let conn = fresh_conn();
        upsert_tombstone(&conn, "@scope/pkg", Utc::now() - Duration::days(1)).unwrap();
        // Now the package "appeared" — upsert real metadata.
        upsert(&conn, &sample("@scope/pkg", 0)).unwrap();
        match get(&conn, "@scope/pkg", DEFAULT_TTL_DAYS).unwrap() {
            CacheStatus::Found(r) => assert_eq!(r.description.as_deref(), Some("a sample package")),
            other => panic!("expected Found after upsert clearing tombstone, got {other:?}"),
        }
    }
}
