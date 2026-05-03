//! Shared state cloned into every axum handler.

use std::sync::Arc;

use quiver_recommender::embed::Embedder;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use tokio::sync::OnceCell;

/// `OnceCell` cell for the lazy-loaded fastembed model. Built on a blocking
/// thread at startup; handlers read it via [`AppState::embedder`].
pub type EmbedderCell = Arc<OnceCell<Arc<Embedder>>>;

#[derive(Clone)]
pub struct AppState {
    pub pool: Pool<SqliteConnectionManager>,
    pub embedder: EmbedderCell,
}

impl AppState {
    /// Returns the embedder if it has finished loading, `None` otherwise.
    pub fn embedder(&self) -> Option<Arc<Embedder>> {
        self.embedder.get().cloned()
    }
}
