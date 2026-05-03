//! SSE stream for the live suggestions feed.
//!
//! Polls `agent_suggestions` once per second. New rows emit an
//! `event: suggestion` whose payload prepends to the feed. Acceptance flips
//! emit the same fragment with `hx-swap-oob` so htmx replaces the existing
//! row in place. A `ping` event every 15s keeps proxies from idling out.

use std::convert::Infallible;
use std::time::Duration;

use askama::Template;
use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use chrono::{DateTime, Utc};
use futures_util::stream::Stream;
use rusqlite::{Connection, params};
use tokio::time::interval;

use crate::error::WebError;
use crate::state::AppState;

const POLL_INTERVAL_SECS: u64 = 1;
const PING_EVERY_TICKS: u64 = 15;

/// Row shape used by the SSE template — separate from the storage row so we
/// can pre-format the timestamp and keep template logic trivial.
#[derive(Clone)]
pub struct SuggestionRowView {
    pub id: i64,
    pub session_id: String,
    pub tool_id: String,
    pub task_text: String,
    pub score_str: String,
    pub suggested_str: String,
    pub accepted: bool,
    pub accepted_str: String,
    /// When true, the rendered fragment carries `hx-swap-oob="outerHTML"`
    /// so htmx replaces the existing `#suggestion-{id}` row in place.
    pub oob: bool,
}

#[derive(Template)]
#[template(path = "suggestion_row.html")]
struct SuggestionRowTpl<'a> {
    row: &'a SuggestionRowView,
}

pub async fn suggestions_stream(
    State(state): State<AppState>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, WebError> {
    let bootstrap_pool = state.pool.clone();
    let last_id_init = tokio::task::spawn_blocking(move || -> anyhow::Result<i64> {
        let conn = bootstrap_pool.get()?;
        bootstrap_max_id(&conn)
    })
    .await?
    .map_err(WebError::Internal)?;

    let pool = state.pool.clone();
    let stream = async_stream::stream! {
        let mut last_id: i64 = last_id_init;
        let mut last_acc_check: DateTime<Utc> = Utc::now();
        let mut tick = interval(Duration::from_secs(POLL_INTERVAL_SECS));
        let mut counter: u64 = 0;
        loop {
            tick.tick().await;
            counter = counter.wrapping_add(1);

            let pool = pool.clone();
            let cursor_id = last_id;
            let cursor_acc = last_acc_check;
            let polled = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<SuggestionRowView>> {
                let conn = pool.get()?;
                poll_since(&conn, cursor_id, cursor_acc)
            })
            .await;

            let rows = match polled {
                Ok(Ok(rows)) => rows,
                Ok(Err(err)) => {
                    tracing::warn!(target: "toolhub::web::sse", "poll: {err:#}");
                    Vec::new()
                },
                Err(err) => {
                    tracing::warn!(target: "toolhub::web::sse", "join: {err:#}");
                    Vec::new()
                },
            };

            for row in &rows {
                if !row.oob && row.id > last_id {
                    last_id = row.id;
                }
                let html = match (SuggestionRowTpl { row }).render() {
                    Ok(h) => h,
                    Err(err) => {
                        tracing::warn!(target: "toolhub::web::sse", "render: {err:#}");
                        continue;
                    },
                };
                yield Ok::<_, Infallible>(Event::default().event("suggestion").data(html));
            }
            last_acc_check = Utc::now();

            if counter.is_multiple_of(PING_EVERY_TICKS) {
                yield Ok(Event::default().event("ping").data(""));
            }
        }
    };

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

fn bootstrap_max_id(conn: &Connection) -> anyhow::Result<i64> {
    let id: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(id), 0) FROM agent_suggestions",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    Ok(id)
}

fn poll_since(
    conn: &Connection,
    last_id: i64,
    last_acc_check: DateTime<Utc>,
) -> anyhow::Result<Vec<SuggestionRowView>> {
    let cutoff = last_acc_check.to_rfc3339();
    let mut stmt = conn.prepare(
        "SELECT id, session_id, tool_id, task_text, score, suggested_at,
                accepted, accepted_at
         FROM agent_suggestions
         WHERE id > ?1
            OR (accepted = 1 AND accepted_at >= ?2)
         ORDER BY id ASC",
    )?;
    let rows = stmt
        .query_map(params![last_id, cutoff], |r| {
            let id: i64 = r.get(0)?;
            let session_id: String = r.get(1)?;
            let tool_id: String = r.get(2)?;
            let task_text: Option<String> = r.get(3)?;
            let score: Option<f64> = r.get(4)?;
            let suggested_at: String = r.get(5)?;
            let accepted: bool = r.get::<_, i64>(6)? != 0;
            let accepted_at: Option<String> = r.get(7)?;
            let oob = id <= last_id;
            Ok(SuggestionRowView {
                id,
                session_id,
                tool_id,
                task_text: task_text.unwrap_or_default(),
                score_str: score
                    .map(|s| format!("{s:.3}"))
                    .unwrap_or_else(|| "—".to_string()),
                suggested_str: short_time(&suggested_at),
                accepted,
                accepted_str: accepted_at.as_deref().map(short_time).unwrap_or_default(),
                oob,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn short_time(rfc3339: &str) -> String {
    DateTime::parse_from_rfc3339(rfc3339)
        .map(|t| t.format("%H:%M:%S").to_string())
        .unwrap_or_else(|_| rfc3339.to_string())
}
