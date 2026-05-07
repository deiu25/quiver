//! `/vetoes` page — recent PreToolUse denies, with bypass + false-positive
//! state. Powers manual threshold tuning + feeds the auto-tuner data set.

use askama::Template;
use axum::Router;
use axum::extract::State;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use quiver_storage::suggestions;

use crate::error::{WebError, WebResult};
use crate::sse::SuggestionRowView;
use crate::state::AppState;
use crate::views::enforce_label;

const INITIAL_LIMIT: usize = 100;

pub fn routes() -> Router<AppState> {
    Router::new().route("/vetoes", get(vetoes_page))
}

#[derive(Template)]
#[template(path = "vetoes.html")]
struct VetoesPage {
    active: &'static str,
    enforce: &'static str,
    rows: Vec<SuggestionRowView>,
    total: usize,
    bypassed: usize,
    false_positives: usize,
}

async fn vetoes_page(State(state): State<AppState>) -> WebResult<Response> {
    let rows: Vec<SuggestionRowView> = tokio::task::spawn_blocking(move || -> WebResult<_> {
        let conn = state.pool.get()?;
        Ok(suggestions::list_vetoed(&conn, INITIAL_LIMIT)?
            .into_iter()
            .map(crate::routes::suggestions::row_to_view)
            .collect())
    })
    .await??;

    let total = rows.len();
    let bypassed = rows.iter().filter(|r| r.bypassed).count();
    let false_positives = rows.iter().filter(|r| r.false_positive).count();

    render(VetoesPage {
        active: "vetoes",
        enforce: enforce_label(),
        rows,
        total,
        bypassed,
        false_positives,
    })
}

fn render<T: Template>(t: T) -> WebResult<Response> {
    match t.render() {
        Ok(html) => Ok(Html(html).into_response()),
        Err(err) => Err(WebError::Internal(anyhow::anyhow!("render: {err}"))),
    }
}
