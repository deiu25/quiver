//! Integration tests for the Phase 6 Haiku task classifier.
//!
//! Spawns an `axum`-bound mock Anthropic server on a random localhost port
//! (mirrors the pattern used in `crates/ingestion/tests/llm_extract.rs`) and
//! drives `HaikuClassifier` end-to-end through the API path.

use std::net::SocketAddr;
use std::time::Duration;

use axum::{Json, Router, http::StatusCode, routing::post};
use serde_json::{Value, json};

use quiver_agent::classify::ClaudeBackend;
use quiver_agent::{ClassifiedTask, HaikuClassifier, NoopClassifier, TaskClassifier};

async fn spawn_mock(response_body: Value, delay: Option<Duration>) -> SocketAddr {
    let app = Router::new().route(
        "/v1/messages",
        post(move |Json(_): Json<Value>| {
            let body = response_body.clone();
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

fn classifier_for(addr: SocketAddr) -> HaikuClassifier {
    HaikuClassifier::new(ClaudeBackend::Api {
        api_key: "test-key".into(),
        base_url: format!("http://{addr}/v1/messages"),
    })
}

#[tokio::test]
async fn noop_classifier_passes_through() {
    let out = NoopClassifier.classify("write a tailwind config").await;
    assert!(out.is_task);
    assert_eq!(out.query, "write a tailwind config");
}

#[tokio::test]
async fn haiku_extracts_rewritten_query_for_real_task() {
    let payload = json!({
        "content": [
            {
                "type": "text",
                "text": "{\"is_task\":true,\"query\":\"extract design tokens from competitor site\"}"
            }
        ]
    });
    let addr = spawn_mock(payload, None).await;
    let c = classifier_for(addr);

    let out = c
        .classify("hey can you grab design tokens from competitor.example.com please")
        .await;
    assert!(out.is_task);
    assert_eq!(out.query, "extract design tokens from competitor site");
}

#[tokio::test]
async fn haiku_marks_chitchat_non_task() {
    let payload = json!({
        "content": [
            { "type": "text", "text": "{\"is_task\":false,\"query\":\"\"}" }
        ]
    });
    let addr = spawn_mock(payload, None).await;
    let c = classifier_for(addr);

    let out = c.classify("thanks!").await;
    assert!(!out.is_task);
    assert_eq!(out.query, "");
}

#[tokio::test]
async fn haiku_passthrough_on_malformed_response() {
    let payload = json!({
        "content": [{ "type": "text", "text": "not valid json at all" }]
    });
    let addr = spawn_mock(payload, None).await;
    let c = classifier_for(addr);

    let raw = "wire up auth middleware";
    let out = c.classify(raw).await;
    // Garbage response ⇒ never silently drop ⇒ passthrough.
    assert!(out.is_task);
    assert_eq!(out.query, raw);
}

#[tokio::test]
async fn haiku_passthrough_on_timeout() {
    let payload = json!({
        "content": [{ "type": "text", "text": "{\"is_task\":true,\"query\":\"x\"}" }]
    });
    let addr = spawn_mock(payload, Some(Duration::from_secs(2))).await;
    let c = classifier_for(addr).with_timeout(Duration::from_millis(150));

    let raw = "real task that should not be lost";
    let out = c.classify(raw).await;
    assert!(out.is_task, "timeout must not drop a real task");
    assert_eq!(out.query, raw);
}

#[tokio::test]
async fn haiku_passthrough_on_empty_query_string() {
    // Defensive: model returned is_task=true but forgot to fill the query —
    // engine should still get something useful (the raw text).
    let payload = json!({
        "content": [
            { "type": "text", "text": "{\"is_task\":true,\"query\":\"\"}" }
        ]
    });
    let addr = spawn_mock(payload, None).await;
    let c = classifier_for(addr);

    let raw = "do the thing";
    let out = c.classify(raw).await;
    assert!(out.is_task);
    assert_eq!(out.query, raw);
}

#[tokio::test]
async fn classified_task_passthrough_helper() {
    let c = ClassifiedTask::passthrough("anything");
    assert!(c.is_task);
    assert_eq!(c.query, "anything");
}
