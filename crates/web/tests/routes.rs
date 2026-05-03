//! Integration tests for the read-only HTML routes.
//!
//! Each test opens a fresh tempdir DB via [`toolhub_storage::pool::open_pool`],
//! seeds a couple of tools, builds the live axum router via
//! [`toolhub_web::routes::router`], and exercises a single endpoint with
//! `Router::oneshot`. No real Embedder is loaded — routes that need one
//! (`/api/recommend`, `/api/sources/sync`) are not exercised here.

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use chrono::Utc;
use tokio::sync::OnceCell;
use toolhub_core::tool::{ToolMeta, ToolType};
use toolhub_storage::{pool, suggestions, tools};
use toolhub_web::{AppState, routes};
use tower::util::ServiceExt;

fn sample(id: &str, name: &str, ttype: ToolType, desc: &str) -> ToolMeta {
    let now = Utc::now();
    ToolMeta {
        id: id.into(),
        r#type: ttype,
        name: name.into(),
        source_repo: None,
        install_path: None,
        description: Some(desc.into()),
        long_description: Some(format!("{desc} (long body)")),
        category: None,
        // Triggers feed into the substring haystack — keep them empty in the
        // helper so each test can either accept the default (no triggers,
        // search hits only name/desc/id) or build a fully-custom ToolMeta.
        triggers: vec![],
        examples: vec![],
        invocation: Some(format!("/{name}")),
        requires: vec![],
        enabled: true,
        added_at: now,
        last_seen_at: now,
        last_used_at: None,
    }
}

fn build_state(seed: &[ToolMeta]) -> (tempfile::TempDir, AppState) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("toolhub.sqlite");
    let pool = pool::open_pool(&path).unwrap();
    {
        let conn = pool.get().unwrap();
        for meta in seed {
            tools::upsert(&conn, meta).unwrap();
        }
    }
    let state = AppState {
        pool,
        embedder: Arc::new(OnceCell::new()),
    };
    (dir, state)
}

async fn body_string(resp: axum::http::Response<Body>) -> (StatusCode, String) {
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 2 * 1024 * 1024).await.unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

async fn get(state: AppState, uri: &str) -> (StatusCode, String) {
    let app = routes::router(state);
    let resp = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    body_string(resp).await
}

#[tokio::test]
async fn root_redirects_to_catalog() {
    let (_d, state) = build_state(&[]);
    let app = routes::router(state);
    let resp = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        resp.headers().get("location").unwrap().to_str().unwrap(),
        "/catalog"
    );
}

#[tokio::test]
async fn catalog_lists_seeded_tools() {
    let (_d, state) = build_state(&[
        sample(
            "skill:design-md",
            "design-md",
            ToolType::Skill,
            "design tokens",
        ),
        sample("skill:caveman", "caveman", ToolType::Skill, "be terse"),
    ]);
    let (status, body) = get(state, "/catalog").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Catalog"));
    assert!(body.contains("design-md"));
    assert!(body.contains("caveman"));
    // Total badge.
    assert!(body.contains("(2)"));
}

#[tokio::test]
async fn catalog_list_fragment_filters_by_substring() {
    let (_d, state) = build_state(&[
        sample(
            "skill:design-md",
            "design-md",
            ToolType::Skill,
            "design tokens",
        ),
        sample("skill:caveman", "caveman", ToolType::Skill, "be terse"),
    ]);
    let (status, body) = get(state, "/catalog/list?q=design").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("design-md"));
    assert!(!body.contains("caveman"));
}

#[tokio::test]
async fn catalog_list_fragment_filters_by_type() {
    let (_d, state) = build_state(&[
        sample("skill:a", "a", ToolType::Skill, "x"),
        sample("plugin:b", "b", ToolType::Plugin, "x"),
    ]);
    let (status, body) = get(state, "/catalog/list?type=plugin").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("plugin:b"));
    assert!(!body.contains("skill:a"));
}

#[tokio::test]
async fn tool_detail_renders_for_known_id() {
    let (_d, state) = build_state(&[sample(
        "skill:design-md",
        "design-md",
        ToolType::Skill,
        "design tokens",
    )]);
    let (status, body) = get(state, "/tool/skill:design-md").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("design-md"));
    assert!(body.contains("design tokens"));
    assert!(body.contains("Invocation"));
}

#[tokio::test]
async fn tool_detail_404_for_unknown_id() {
    let (_d, state) = build_state(&[]);
    let (status, _body) = get(state, "/tool/skill:does-not-exist").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn stats_renders_empty_dashboard() {
    let (_d, state) = build_state(&[]);
    let (status, body) = get(state, "/stats").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Acceptance"));
    assert!(body.contains("Top tools"));
    assert!(body.contains("Dead weight"));
    assert!(body.contains("Sources"));
}

#[tokio::test]
async fn sources_renders_empty_list() {
    let (_d, state) = build_state(&[]);
    let (status, body) = get(state, "/sources").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Sources"));
    assert!(body.contains("No GitHub sources"));
}

#[tokio::test]
async fn suggestions_page_includes_initial_rows() {
    let (_d, state) = build_state(&[sample(
        "skill:caveman",
        "caveman",
        ToolType::Skill,
        "be terse",
    )]);
    {
        let conn = state.pool.get().unwrap();
        suggestions::record(
            &conn,
            "sess-1",
            "skill:caveman",
            Some("compress this"),
            Some(0.82),
            Utc::now(),
        )
        .unwrap();
    }
    let (status, body) = get(state, "/suggestions").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Suggestions"));
    assert!(body.contains("skill:caveman"));
    assert!(body.contains("compress this"));
}

#[tokio::test]
async fn static_asset_serves_css() {
    let (_d, state) = build_state(&[]);
    let (status, body) = get(state, "/static/app.css").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("--bg") || body.contains("body"));
}
