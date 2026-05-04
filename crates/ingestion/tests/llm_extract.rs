//! Integration tests for §5 LLM-assisted metadata extraction.
//!
//! Two layers:
//!   * end-to-end `ingest_local` + `enrich_with_llm` against the
//!     `doc_with_readme` fixture using `RegexExtractor` (offline, no network).
//!   * `ClaudeExtractor` in API mode pointed at a tiny `axum` mock server
//!     bound to a random localhost port (mirrors the pattern used in
//!     `crates/web/tests/sse.rs`).

use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use axum::{Json, Router, http::StatusCode, routing::post};
use serde_json::json;

use quiver_ingestion::github_repo::{enrich_with_llm, ingest_local};
use quiver_ingestion::llm_extract::{
    ClaudeBackend, ClaudeExtractor, MetadataExtractor, RegexExtractor,
};

fn fixtures_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/github_repos")
}

#[tokio::test]
async fn enrich_doc_only_with_regex_populates_triggers_and_examples() {
    let root = fixtures_root().join("doc_with_readme");
    let url = "https://github.com/example/widget-tool";
    let mut tools = ingest_local(&root, url).expect("ingest_local");
    assert_eq!(tools.len(), 1);
    assert!(tools[0].triggers.is_empty(), "pre-enrich triggers empty");

    enrich_with_llm(&mut tools, &RegexExtractor).await;

    let t = &tools[0];
    assert_eq!(
        t.triggers.len(),
        4,
        "regex should pull all 4 bullets, got {:?}",
        t.triggers
    );
    assert!(t.triggers[0].starts_with("Bootstrap"));
    assert_eq!(t.examples.len(), 2);
    assert_eq!(t.examples[0]["lang"], json!("bash"));
    assert_eq!(t.category.as_deref(), Some("cli"));
}

#[tokio::test]
async fn enrich_skips_when_long_description_is_empty() {
    let root = fixtures_root().join("skill_bundle");
    let url = "https://github.com/example/skill-bundle";
    let mut tools = ingest_local(&root, url).expect("ingest_local");
    // Skill bundle parser fills long_description from SKILL.md body, so this
    // exercises the "already has body" path: enrichment runs, regex finds
    // nothing matching its headings, output stays empty.
    enrich_with_llm(&mut tools, &RegexExtractor).await;
    for t in &tools {
        // No "## When to use" / "## Examples" headings in the test fixtures.
        assert!(t.triggers.is_empty());
        assert!(t.examples.is_empty());
    }
}

// ── Mock Anthropic API server ──────────────────────────────────────────────

async fn spawn_mock_anthropic(
    response_body: serde_json::Value,
    delay: Option<Duration>,
) -> SocketAddr {
    let app = Router::new().route(
        "/v1/messages",
        post(move |Json(_): Json<serde_json::Value>| {
            let body = response_body.clone();
            let delay = delay;
            async move {
                if let Some(d) = delay {
                    tokio::time::sleep(d).await;
                }
                (StatusCode::OK, Json(body))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

fn extractor_for(addr: SocketAddr) -> ClaudeExtractor {
    ClaudeExtractor::new(ClaudeBackend::Api {
        api_key: "test-key".into(),
        base_url: format!("http://{addr}/v1/messages"),
    })
}

#[tokio::test]
async fn claude_api_parses_valid_json_payload() {
    let payload = json!({
        "content": [
            {
                "type": "text",
                "text": "{\"triggers\":[\"do x\",\"do y\"],\"examples\":[\"echo hi\"],\"category\":\"cli\"}"
            }
        ]
    });
    let addr = spawn_mock_anthropic(payload, None).await;
    let ex = extractor_for(addr);

    let m = ex.extract("widget", "some readme text").await.unwrap();
    assert_eq!(m.triggers, vec!["do x".to_string(), "do y".to_string()]);
    assert_eq!(m.examples.len(), 1);
    assert_eq!(m.examples[0]["body"], json!("echo hi"));
    assert_eq!(m.category.as_deref(), Some("cli"));
}

#[tokio::test]
async fn claude_api_handles_malformed_text_gracefully() {
    let payload = json!({
        "content": [{ "type": "text", "text": "not valid json at all" }]
    });
    let addr = spawn_mock_anthropic(payload, None).await;
    let ex = extractor_for(addr);

    let m = ex.extract("x", "readme").await.unwrap();
    assert!(m.is_empty(), "malformed json should return empty: {m:?}");
}

#[tokio::test]
async fn claude_api_times_out_on_slow_server() {
    let payload = json!({ "content": [{ "type": "text", "text": "{}" }] });
    let addr = spawn_mock_anthropic(payload, Some(Duration::from_secs(2))).await;
    let ex = extractor_for(addr).with_timeout(Duration::from_millis(150));

    let err = ex
        .extract("x", "readme")
        .await
        .expect_err("expected timeout error");
    assert!(
        err.to_string().contains("timeout"),
        "unexpected err: {err:#}"
    );
}
