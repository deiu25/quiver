//! Live recommend box: page + htmx fragment endpoint.

use askama::Template;
use axum::Router;
use axum::extract::State;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Form, http::StatusCode};
use serde::Deserialize;
use toolhub_agent::recommend::{RecHit, top_k};

use crate::error::{WebError, WebResult};
use crate::state::AppState;

const TOP_K: usize = 3;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/recommend", get(recommend_page))
        .route("/api/recommend", post(recommend_fragment))
}

#[derive(Template)]
#[template(path = "recommend.html")]
struct RecommendPage {
    active: &'static str,
}

#[derive(Template)]
#[template(path = "recommend_results.html")]
struct RecommendResults {
    hits: Vec<RecHitView>,
}

pub struct RecHitView {
    pub tool_id: String,
    pub score_str: String,
    pub description: Option<String>,
    pub invocation: Option<String>,
}

impl From<RecHit> for RecHitView {
    fn from(h: RecHit) -> Self {
        RecHitView {
            tool_id: h.tool_id,
            score_str: format!("{:.3}", h.score),
            description: h.description,
            invocation: h.invocation,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct RecForm {
    #[serde(default)]
    pub task: String,
}

async fn recommend_page() -> WebResult<Response> {
    render(RecommendPage {
        active: "recommend",
    })
}

async fn recommend_fragment(
    State(state): State<AppState>,
    Form(form): Form<RecForm>,
) -> WebResult<Response> {
    let task = form.task.trim().to_string();
    if task.is_empty() {
        // Empty input: clear the results pane.
        return Ok((StatusCode::OK, Html(String::new())).into_response());
    }
    let Some(embedder) = state.embedder() else {
        return Err(WebError::EmbedderNotReady);
    };

    let hits = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<RecHit>> {
        let conn = state.pool.get()?;
        top_k(&conn, &embedder, &task, TOP_K)
    })
    .await?
    .map_err(WebError::Internal)?;

    render(RecommendResults {
        hits: hits.into_iter().map(RecHitView::from).collect(),
    })
}

fn render<T: Template>(t: T) -> WebResult<Response> {
    match t.render() {
        Ok(html) => Ok(Html(html).into_response()),
        Err(err) => Err(WebError::Internal(anyhow::anyhow!("render: {err}"))),
    }
}
