//! Sources page + POST /api/sources/sync.

use std::path::PathBuf;

use askama::Template;
use axum::Router;
use axum::extract::State;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use quiver_storage::sources::{self, SourceRow};

use crate::error::{WebError, WebResult};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/sources", get(sources_page))
        .route("/api/sources/sync", post(sync_sources))
}

#[derive(Template)]
#[template(path = "sources.html")]
struct SourcesPage {
    active: &'static str,
    sources: Vec<SourceView>,
}

#[derive(Template)]
#[template(path = "sources_sync_result.html")]
struct SyncResult {
    ok: bool,
    unique: usize,
    skipped: usize,
    catalog_total: usize,
    error: String,
}

pub struct SourceView {
    pub id: String,
    pub type_name: String,
    pub location: String,
    pub last_pulled_str: String,
    pub commit_short: String,
}

impl From<SourceRow> for SourceView {
    fn from(s: SourceRow) -> Self {
        SourceView {
            id: s.id,
            type_name: s.r#type,
            location: s.location,
            last_pulled_str: s
                .last_pulled_at
                .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "—".to_string()),
            commit_short: s
                .last_commit_sha
                .map(|sha| sha.chars().take(7).collect::<String>())
                .unwrap_or_else(|| "—".to_string()),
        }
    }
}

async fn sources_page(State(state): State<AppState>) -> WebResult<Response> {
    let rows: Vec<SourceView> = tokio::task::spawn_blocking(move || -> WebResult<_> {
        let conn = state.pool.get()?;
        Ok(sources::list(&conn)?
            .into_iter()
            .map(SourceView::from)
            .collect())
    })
    .await??;

    render(SourcesPage {
        active: "sources",
        sources: rows,
    })
}

async fn sync_sources(State(state): State<AppState>) -> WebResult<Response> {
    let Some(embedder) = state.embedder() else {
        return Err(WebError::EmbedderNotReady);
    };
    let pool = state.pool.clone();

    let result = tokio::task::spawn_blocking(
        move || -> anyhow::Result<quiver_ingestion::sync::SyncReport> {
            let conn = pool.get()?;
            let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
            quiver_ingestion::sync::run_sync(&conn, &embedder, &home)
        },
    )
    .await?;

    match result {
        Ok(r) => render(SyncResult {
            ok: true,
            unique: r.unique,
            skipped: r.skipped,
            catalog_total: r.catalog_total,
            error: String::new(),
        }),
        Err(err) => render(SyncResult {
            ok: false,
            unique: 0,
            skipped: 0,
            catalog_total: 0,
            error: format!("{err:#}"),
        }),
    }
}

fn render<T: Template>(t: T) -> WebResult<Response> {
    match t.render() {
        Ok(html) => Ok(Html(html).into_response()),
        Err(err) => Err(WebError::Internal(anyhow::anyhow!("render: {err}"))),
    }
}
