//! End-to-end SSE test.
//!
//! Spawns a real axum server on an ephemeral loopback port, opens the
//! `/api/suggestions/stream` endpoint with `reqwest`, inserts a row into
//! `agent_suggestions`, and asserts that an `event: suggestion` payload
//! arrives within 4 seconds (the polling interval is 1s, so 4s is plenty
//! of headroom for slower CI).

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use futures_util::StreamExt;
use quiver_core::tool::{ToolMeta, ToolType};
use quiver_storage::{pool, suggestions, tools};
use quiver_web::{AppState, routes};
use tokio::sync::OnceCell;

fn seed_tool(state: &AppState, id: &str) {
    let now = Utc::now();
    let conn = state.pool.get().unwrap();
    tools::upsert(
        &conn,
        &ToolMeta {
            id: id.into(),
            r#type: ToolType::Skill,
            name: id.into(),
            source_repo: None,
            install_path: None,
            description: Some("for sse test".into()),
            long_description: None,
            category: None,
            triggers: vec![],
            examples: vec![],
            invocation: None,
            requires: vec![],
            enabled: true,
            added_at: now,
            last_seen_at: now,
            last_used_at: None,
        },
    )
    .unwrap();
}

#[tokio::test]
async fn sse_emits_event_for_inserted_suggestion() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("quiver.sqlite");
    let pool = pool::open_pool(&path).unwrap();
    let state = AppState {
        pool: pool.clone(),
        embedder: Arc::new(OnceCell::new()),
    };
    seed_tool(&state, "skill:caveman");

    let app = routes::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // Open the SSE stream first so the server is past the bootstrap_max_id
    // snapshot before we insert.
    let url = format!("http://{addr}/api/suggestions/stream");
    let resp = reqwest::Client::new()
        .get(&url)
        .header("Accept", "text/event-stream")
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let mut stream = resp.bytes_stream();

    // Brief delay so the stream's first interval tick has armed.
    tokio::time::sleep(Duration::from_millis(200)).await;

    {
        let conn = pool.get().unwrap();
        suggestions::record(
            &conn,
            "sess-test",
            "skill:caveman",
            Some("be terse"),
            Some(0.91),
            Utc::now(),
        )
        .unwrap();
    }

    let mut buf = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    while tokio::time::Instant::now() < deadline {
        let next = tokio::time::timeout(Duration::from_millis(500), stream.next()).await;
        match next {
            Ok(Some(Ok(bytes))) => {
                buf.extend_from_slice(&bytes);
                let s = String::from_utf8_lossy(&buf);
                if s.contains("event: suggestion") && s.contains("skill:caveman") {
                    server.abort();
                    return;
                }
            },
            Ok(Some(Err(e))) => panic!("stream error: {e}"),
            Ok(None) => panic!("stream ended"),
            Err(_) => continue,
        }
    }
    server.abort();
    let s = String::from_utf8_lossy(&buf);
    panic!("no suggestion event received within 4s. Buffer:\n{s}");
}
