//! `/suggestions` page (initial render) + the SSE stream endpoint.

use askama::Template;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use chrono::Utc;
use quiver_storage::suggestions;

use crate::error::{WebError, WebResult};
use crate::sse::{SuggestionRowView, suggestions_stream};
use crate::state::AppState;

const INITIAL_LIMIT: usize = 50;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/suggestions", get(suggestions_page))
        .route("/api/suggestions/stream", get(suggestions_stream))
        .route("/api/suggestions/:id/accept", post(accept_suggestion))
}

#[derive(Template)]
#[template(path = "suggestions.html")]
struct SuggestionsPage {
    active: &'static str,
    rows: Vec<SuggestionRowView>,
}

async fn suggestions_page(State(state): State<AppState>) -> WebResult<Response> {
    let rows: Vec<SuggestionRowView> = tokio::task::spawn_blocking(move || -> WebResult<_> {
        let conn = state.pool.get()?;
        let mut all = suggestions::list(&conn, None)?;
        all.truncate(INITIAL_LIMIT);
        // Newest at the top of #feed; rendering goes top-down so we reverse
        // the DESC list back to ASC and rely on hx-swap="afterbegin" only for
        // SSE inserts.
        Ok(all
            .into_iter()
            .map(|s| SuggestionRowView {
                id: s.id,
                session_id: s.session_id,
                tool_id: s.tool_id,
                task_text: s.task_text.unwrap_or_default(),
                score_str: s
                    .score
                    .map(|sc| format!("{sc:.3}"))
                    .unwrap_or_else(|| "—".to_string()),
                suggested_str: short_time(&s.suggested_at),
                accepted: s.accepted,
                accepted_str: s.accepted_at.as_deref().map(short_time).unwrap_or_default(),
                oob: false,
            })
            .collect())
    })
    .await??;

    render(SuggestionsPage {
        active: "suggestions",
        rows,
    })
}

#[derive(Template)]
#[template(path = "suggestion_row.html")]
struct SuggestionRowTpl {
    row: SuggestionRowView,
}

async fn accept_suggestion(
    Path(id): Path<i64>,
    State(state): State<AppState>,
) -> WebResult<Response> {
    let row = tokio::task::spawn_blocking(move || -> WebResult<Option<SuggestionRowView>> {
        let conn = state.pool.get()?;
        suggestions::mark_accepted_by_id(&conn, id, Utc::now())?;
        let fetched = suggestions::find_by_id(&conn, id)?;
        Ok(fetched.map(|s| SuggestionRowView {
            id: s.id,
            session_id: s.session_id,
            tool_id: s.tool_id,
            task_text: s.task_text.unwrap_or_default(),
            score_str: s
                .score
                .map(|sc| format!("{sc:.3}"))
                .unwrap_or_else(|| "—".to_string()),
            suggested_str: short_time(&s.suggested_at),
            accepted: s.accepted,
            accepted_str: s.accepted_at.as_deref().map(short_time).unwrap_or_default(),
            oob: false,
        }))
    })
    .await??;

    match row {
        Some(view) => render(SuggestionRowTpl { row: view }),
        None => Ok((StatusCode::NOT_FOUND, "suggestion not found").into_response()),
    }
}

fn short_time(rfc3339: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .map(|t| t.format("%H:%M:%S").to_string())
        .unwrap_or_else(|_| rfc3339.to_string())
}

fn render<T: Template>(t: T) -> WebResult<Response> {
    match t.render() {
        Ok(html) => Ok(Html(html).into_response()),
        Err(err) => Err(WebError::Internal(anyhow::anyhow!("render: {err}"))),
    }
}
