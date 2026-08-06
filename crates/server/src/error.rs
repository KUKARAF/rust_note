//! Application-wide error type and its `IntoResponse` mapping.

use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("internal error: {0}")]
    Internal(#[from] anyhow::Error),

    #[error("not found")]
    NotFound,

    #[error("unauthorized")]
    Unauthorized,

    #[error("forbidden")]
    Forbidden,

    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("payload too large")]
    PayloadTooLarge,
}

/// Body of every non-2xx JSON error response.
///
/// The frontend (`web/src/lib/api/client.ts`) only surfaces `payload.message`
/// to the user, so it's important every error response is actually
/// `application/json` with a `message` field - a plain-text body (Axum's
/// default for `(StatusCode, String)`) is silently discarded client-side.
#[derive(Debug, Serialize)]
struct ErrorBody {
    message: String,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match &self {
            AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            AppError::NotFound => StatusCode::NOT_FOUND,
            AppError::Unauthorized => StatusCode::UNAUTHORIZED,
            AppError::Forbidden => StatusCode::FORBIDDEN,
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::Conflict(_) => StatusCode::CONFLICT,
            AppError::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        };

        if let AppError::Internal(err) = &self {
            tracing::error!(error = ?err, "internal error");
        }

        let message = match &self {
            // Don't leak internal error details to clients.
            AppError::Internal(_) => "internal error".to_string(),
            other => other.to_string(),
        };

        (status, Json(ErrorBody { message })).into_response()
    }
}

/// Extractor rejections (e.g. malformed/missing JSON body) also go through
/// Axum's default plain-text `IntoResponse`; convert them into the same JSON
/// error shape so the frontend can surface the (often genuinely useful)
/// deserialize diagnostic instead of discarding it.
impl From<JsonRejection> for AppError {
    fn from(rejection: JsonRejection) -> Self {
        // A body rejected for exceeding the configured size limit (dos-04)
        // must surface as 413, not be flattened into a generic 400 — the
        // limit is enforced at the JSON extractor, and `WithRejection` routes
        // its rejection through here.
        if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
            return AppError::PayloadTooLarge;
        }
        AppError::BadRequest(rejection.body_text())
    }
}

pub type AppResult<T> = Result<T, AppError>;
