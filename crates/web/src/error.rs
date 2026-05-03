//! Web error type + IntoResponse impl.

use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

#[derive(Debug, thiserror::Error)]
pub enum WebError {
    #[error("not found")]
    NotFound,
    #[error("embedder still warming up")]
    EmbedderNotReady,
    #[error("internal: {0}")]
    Internal(#[from] anyhow::Error),
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        match self {
            WebError::NotFound => (StatusCode::NOT_FOUND, "not found").into_response(),
            WebError::EmbedderNotReady => (
                StatusCode::SERVICE_UNAVAILABLE,
                [(header::RETRY_AFTER, "5")],
                "<div class=\"card muted\">Embedder warming up — try again in a few seconds.</div>",
            )
                .into_response(),
            WebError::Internal(err) => {
                tracing::error!(target: "quiver::web", "{err:#}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("internal error: {err}"),
                )
                    .into_response()
            },
        }
    }
}

impl From<rusqlite::Error> for WebError {
    fn from(err: rusqlite::Error) -> Self {
        WebError::Internal(err.into())
    }
}

impl From<r2d2::Error> for WebError {
    fn from(err: r2d2::Error) -> Self {
        WebError::Internal(err.into())
    }
}

impl From<tokio::task::JoinError> for WebError {
    fn from(err: tokio::task::JoinError) -> Self {
        WebError::Internal(err.into())
    }
}

pub type WebResult<T> = Result<T, WebError>;
