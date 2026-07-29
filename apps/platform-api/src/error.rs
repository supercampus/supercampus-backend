use axum::{Json, http::StatusCode, response::IntoResponse};
use serde::Serialize;

#[derive(Debug)]
pub enum ApiError {
    BadRequest(String),
    Unauthorized,
    NotFound(String),
    Internal,
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
    code: &'static str,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let (status, code, error) = match self {
            Self::BadRequest(error) => (StatusCode::BAD_REQUEST, "bad_request", error),
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "Authentication is required".into(),
            ),
            Self::NotFound(error) => (StatusCode::NOT_FOUND, "not_found", error),
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "The service could not complete the request".into(),
            ),
        };
        (status, Json(ErrorBody { error, code })).into_response()
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        tracing::error!(error = ?error, "request failed");
        Self::Internal
    }
}
