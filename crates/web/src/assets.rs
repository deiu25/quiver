//! Embedded static asset handler. `rust-embed` bakes everything under
//! `crates/web/static/` into the binary in release mode, and reads from disk
//! when the `debug-embed` feature is on (dev hot-reload).

use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "static/"]
struct Assets;

pub async fn serve_static(Path(path): Path<String>) -> Response {
    match Assets::get(&path) {
        Some(file) => {
            let mime = mime_guess::from_path(&path).first_or_octet_stream();
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, mime.as_ref())],
                file.data.into_owned(),
            )
                .into_response()
        },
        None => (StatusCode::NOT_FOUND, "asset not found").into_response(),
    }
}
