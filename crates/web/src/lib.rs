//! ToolHub web UI — local dashboard served by `toolhub serve`.
//!
//! Single binary, single crate, single static-asset bundle. axum on
//! 127.0.0.1, askama-rendered HTML, htmx for interactivity, SSE for the
//! suggestions feed. No JS build step, no Node, no frontend framework.

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tokio::net::TcpListener;
use tokio::sync::OnceCell;

mod assets;
pub mod error;
pub mod routes;
mod state;
mod views;

pub use state::{AppState, EmbedderCell};

/// Configuration handed to [`serve`].
#[derive(Debug, Clone)]
pub struct WebConfig {
    pub db_path: PathBuf,
    pub host: IpAddr,
    pub port: u16,
    /// Open the user's default browser at the listen URL once the server
    /// is up. Best-effort; failures are logged and ignored.
    pub open: bool,
}

/// Build state, bind the listener, run axum until Ctrl-C.
pub async fn serve(cfg: WebConfig) -> anyhow::Result<()> {
    if let Some(parent) = cfg.db_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create parent dir for {}", cfg.db_path.display()))?;
    }
    let pool = toolhub_storage::pool::open_pool(&cfg.db_path)
        .with_context(|| format!("open pool {}", cfg.db_path.display()))?;

    let embedder: EmbedderCell = Arc::new(OnceCell::new());
    spawn_embedder_init(embedder.clone());

    let state = AppState { pool, embedder };
    let app = routes::router(state);

    let addr = std::net::SocketAddr::new(cfg.host, cfg.port);
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    let bound = listener.local_addr().unwrap_or(addr);
    tracing::info!(target: "toolhub::web", "listening on http://{bound}");
    eprintln!("toolhub serve listening on http://{bound}");

    if cfg.open {
        let url = format!("http://{bound}");
        std::thread::spawn(move || {
            // Tiny delay so the listener is accepting before the browser
            // races us to /catalog.
            std::thread::sleep(Duration::from_millis(200));
            if let Err(err) = webbrowser::open(&url) {
                tracing::warn!(target: "toolhub::web", "open browser: {err:#}");
            }
        });
    }

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("axum serve")?;
    Ok(())
}

fn spawn_embedder_init(cell: EmbedderCell) {
    tokio::task::spawn_blocking(move || match toolhub_recommender::embed::Embedder::new() {
        Ok(emb) => {
            if cell.set(Arc::new(emb)).is_err() {
                tracing::warn!(target: "toolhub::web", "embedder cell already set");
            }
            tracing::info!(target: "toolhub::web", "embedder ready");
        },
        Err(err) => {
            tracing::error!(target: "toolhub::web", "embedder init failed: {err:#}");
        },
    });
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!(target: "toolhub::web", "shutdown signal received");
}
