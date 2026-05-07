//! Integration tests for npm registry enrichment of MCP servers.
//!
//! Spins up a tiny axum server that mimics
//! `https://registry.npmjs.org/<pkg>/latest` and exercises:
//!   * cache miss + online → HTTP fetch + cache upsert
//!   * cache hit → no HTTP traffic
//!   * cache expiry (TTL) → re-fetch
//!   * offline mode + cache miss → `None`
//!   * 404 from registry → `Err`

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use chrono::{Duration, Utc};
use serde_json::{Value, json};

use quiver_ingestion::mcp_json::{NpmEnrichment, parse_mcp_servers};
use quiver_ingestion::mcp_npm::{NetworkMode, enrich_via_cache};
use quiver_storage::mcp_npm::{NpmCacheRow, upsert};
use quiver_storage::open;

#[derive(Clone)]
struct MockState {
    body: Arc<Value>,
    hits: Arc<AtomicUsize>,
    status: StatusCode,
}

async fn handler(State(state): State<MockState>) -> impl IntoResponse {
    state.hits.fetch_add(1, Ordering::SeqCst);
    if state.status != StatusCode::OK {
        return (state.status, "not found").into_response();
    }
    (state.status, axum::Json((*state.body).clone())).into_response()
}

async fn spawn_mock(body: Value, status: StatusCode) -> (SocketAddr, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    // Match anything — npm registry path includes scoped names that
    // confuse `:param` style routes (`@scope/pkg/latest` is 3 segments).
    let app = Router::new()
        .route("/*path", get(handler))
        .with_state(MockState {
            body: Arc::new(body),
            hits: hits.clone(),
            status,
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, hits)
}

fn fresh_db() -> rusqlite::Connection {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.keep().join("quiver.sqlite");
    open(&path).unwrap()
}

fn registry_payload() -> Value {
    json!({
        "name": "@context7/mcp-server",
        "version": "1.0.0",
        "description": "Up-to-date library documentation via Context7",
        "keywords": ["mcp", "documentation", "library", "context7"],
        "repository": { "type": "git", "url": "git+https://github.com/upstash/context7.git" },
        "homepage": "https://context7.com",
        "readme": "# context7\n\nFetch latest library docs."
    })
}

#[tokio::test]
async fn cache_miss_online_fetches_and_persists() {
    let (addr, hits) = spawn_mock(registry_payload(), StatusCode::OK).await;
    let conn = fresh_db();
    let base = format!("http://{addr}");

    let m = enrich_via_cache(&conn, &base, "@context7/mcp-server", NetworkMode::Online)
        .await
        .unwrap()
        .expect("metadata");
    assert_eq!(m.package, "@context7/mcp-server");
    assert_eq!(
        m.description.as_deref(),
        Some("Up-to-date library documentation via Context7")
    );
    assert!(m.keywords.contains(&"mcp".to_string()));
    assert_eq!(
        m.repository.as_deref(),
        Some("https://github.com/upstash/context7")
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    // Second call hits the cache, no extra HTTP.
    let m2 = enrich_via_cache(&conn, &base, "@context7/mcp-server", NetworkMode::Online)
        .await
        .unwrap()
        .expect("cached metadata");
    assert_eq!(m2.description, m.description);
    assert_eq!(hits.load(Ordering::SeqCst), 1, "cache should suppress HTTP");
}

#[tokio::test]
async fn offline_mode_cache_miss_returns_none() {
    let (addr, hits) = spawn_mock(registry_payload(), StatusCode::OK).await;
    let conn = fresh_db();
    let base = format!("http://{addr}");

    let m = enrich_via_cache(&conn, &base, "@context7/mcp-server", NetworkMode::Offline)
        .await
        .unwrap();
    assert!(m.is_none(), "offline + cache miss returns None");
    assert_eq!(hits.load(Ordering::SeqCst), 0, "no HTTP in offline mode");
}

#[tokio::test]
async fn expired_cache_row_triggers_refetch() {
    let (addr, hits) = spawn_mock(registry_payload(), StatusCode::OK).await;
    let conn = fresh_db();
    let base = format!("http://{addr}");

    // Seed an old row directly.
    upsert(
        &conn,
        &NpmCacheRow {
            package: "@context7/mcp-server".into(),
            fetched_at: Utc::now() - Duration::days(60),
            description: Some("stale".into()),
            keywords: vec![],
            repository: None,
            homepage: None,
            readme: None,
        },
    )
    .unwrap();

    let m = enrich_via_cache(&conn, &base, "@context7/mcp-server", NetworkMode::Online)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        m.description.as_deref(),
        Some("Up-to-date library documentation via Context7")
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1, "expired row → re-fetched");
}

#[tokio::test]
async fn registry_404_writes_tombstone_and_suppresses_retry() {
    let (addr, hits) = spawn_mock(json!({}), StatusCode::NOT_FOUND).await;
    let conn = fresh_db();
    let base = format!("http://{addr}");

    // First call: registry returns 404 → caller gets Ok(None), tombstone
    // upserted.
    let res = enrich_via_cache(&conn, &base, "@nope/missing", NetworkMode::Online)
        .await
        .unwrap();
    assert!(res.is_none(), "404 maps to Ok(None) for the caller");
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    // Direct check: storage now has a tombstone for the package.
    use quiver_storage::mcp_npm::{CacheStatus, DEFAULT_TTL_DAYS, get};
    let status = get(&conn, "@nope/missing", DEFAULT_TTL_DAYS).unwrap();
    assert_eq!(status, CacheStatus::NotFound);

    // Second call within TTL: tombstone hit, no extra HTTP.
    let res2 = enrich_via_cache(&conn, &base, "@nope/missing", NetworkMode::Online)
        .await
        .unwrap();
    assert!(res2.is_none());
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "tombstone must suppress retry within TTL"
    );
}

#[tokio::test]
async fn parse_mcp_servers_uses_npm_when_provided() {
    let (addr, hits) = spawn_mock(registry_payload(), StatusCode::OK).await;
    let conn = fresh_db();
    let base = format!("http://{addr}");
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/mcp_servers.json");

    let metas = parse_mcp_servers(
        &fixture,
        Some(NpmEnrichment {
            conn: &conn,
            network: NetworkMode::Online,
            registry_base: &base,
        }),
    )
    .await
    .unwrap();
    assert_eq!(metas.len(), 2);
    let context7 = metas.iter().find(|m| m.id == "mcp:context7").unwrap();
    assert!(
        context7
            .description
            .as_ref()
            .unwrap()
            .contains("Up-to-date library documentation"),
        "description: {:?}",
        context7.description
    );
    assert!(context7.triggers.contains(&"documentation".to_string()));
    assert_eq!(
        context7.source_repo.as_deref(),
        Some("https://github.com/upstash/context7")
    );
    // The second fixture entry uses `echo`, which is not an npm runner →
    // no HTTP for it. Total HTTP should be 1.
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn parse_mcp_servers_offline_with_npm_keeps_stub() {
    let (_addr, hits) = spawn_mock(registry_payload(), StatusCode::OK).await;
    let conn = fresh_db();
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/mcp_servers.json");

    let metas = parse_mcp_servers(
        &fixture,
        Some(NpmEnrichment {
            conn: &conn,
            network: NetworkMode::Offline,
            registry_base: "http://127.0.0.1:1", // never reached
        }),
    )
    .await
    .unwrap();
    let context7 = metas.iter().find(|m| m.id == "mcp:context7").unwrap();
    assert!(context7.triggers.is_empty());
    assert!(context7.source_repo.is_none());
    assert!(
        context7
            .description
            .as_ref()
            .unwrap()
            .starts_with("MCP server: ")
    );
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}
