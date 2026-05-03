//! Top-level router. Each page module owns its sub-router.

use axum::Router;
use axum::routing::get;

use crate::assets::serve_static;
use crate::state::AppState;

pub mod catalog;
pub mod recommend;
pub mod sources;
pub mod stats;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route(
            "/",
            get(|| async { axum::response::Redirect::to("/catalog") }),
        )
        .merge(catalog::routes())
        .merge(recommend::routes())
        .merge(sources::routes())
        .merge(stats::routes())
        .route("/static/*path", get(serve_static))
        .with_state(state)
}
