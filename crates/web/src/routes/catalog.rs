//! Stub — fleshed out in step 4.

use axum::Router;
use axum::routing::get;

use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/catalog", get(placeholder))
}

async fn placeholder() -> &'static str {
    "catalog (placeholder — step 4)"
}
