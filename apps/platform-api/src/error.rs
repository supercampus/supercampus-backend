use axum::{
    Json,
    http::{StatusCode, header},
    response::IntoResponse,
};
use serde::Serialize;

#[derive(Debug)]
pub enum ApiError {
    BadRequest(String),
    Conflict(String),
    Unauthorized,
    InvalidCredentials,
    AccessTokenExpired,
    InvalidAccessToken,
    SessionInactive,
    InvalidRefreshToken,
    RefreshTokenReuse,
    Forbidden,
    NotFound(String),
    ServiceUnavailable(String),
    BadGateway(String),
    PaymentProviderUnauthorized(String),
    PaymentProvider(String),
    Internal,
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
    code: &'static str,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let (status, code, error, bearer_error) = match self {
            Self::BadRequest(error) => (StatusCode::BAD_REQUEST, "bad_request", error, None),
            Self::Conflict(error) => (StatusCode::CONFLICT, "conflict", error, None),
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "Authentication is required".into(),
                Some("invalid_token"),
            ),
            Self::InvalidCredentials => (
                StatusCode::UNAUTHORIZED,
                "invalid_credentials",
                "The email or password is incorrect".into(),
                None,
            ),
            Self::AccessTokenExpired => (
                StatusCode::UNAUTHORIZED,
                "access_token_expired",
                "The access token has expired".into(),
                Some("invalid_token"),
            ),
            Self::InvalidAccessToken => (
                StatusCode::UNAUTHORIZED,
                "invalid_access_token",
                "The access token is invalid".into(),
                Some("invalid_token"),
            ),
            Self::SessionInactive => (
                StatusCode::UNAUTHORIZED,
                "session_inactive",
                "The session is no longer active".into(),
                Some("invalid_token"),
            ),
            Self::InvalidRefreshToken => (
                StatusCode::UNAUTHORIZED,
                "invalid_refresh_token",
                "The refresh session is invalid or expired".into(),
                None,
            ),
            Self::RefreshTokenReuse => (
                StatusCode::UNAUTHORIZED,
                "refresh_token_reuse",
                "Refresh token reuse was detected and the session was revoked".into(),
                None,
            ),
            Self::Forbidden => (
                StatusCode::FORBIDDEN,
                "forbidden",
                "This session cannot access the requested tenant or resource".into(),
                None,
            ),
            Self::NotFound(error) => (StatusCode::NOT_FOUND, "not_found", error, None),
            Self::ServiceUnavailable(error) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "service_unavailable",
                error,
                None,
            ),
            Self::BadGateway(error) => (StatusCode::BAD_GATEWAY, "upstream_error", error, None),
            Self::PaymentProviderUnauthorized(error) => (
                StatusCode::UNAUTHORIZED,
                "payment_provider_auth_failed",
                error,
                None,
            ),
            Self::PaymentProvider(error) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "payment_provider_error",
                error,
                None,
            ),
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "The service could not complete the request".into(),
                None,
            ),
        };
        let mut response = (status, Json(ErrorBody { error, code })).into_response();
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            "no-store".parse().expect("static cache-control is valid"),
        );
        if let Some(error) = bearer_error {
            let value = format!("Bearer error=\"{error}\", error_description=\"{code}\"");
            if let Ok(value) = value.parse() {
                response
                    .headers_mut()
                    .insert(header::WWW_AUTHENTICATE, value);
            }
        }
        response
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        tracing::error!(error = ?error, "request failed");
        if error
            .chain()
            .find_map(|cause| cause.downcast_ref::<sqlx::Error>())
            .is_some_and(is_transient_database_error)
        {
            return transient_database_response();
        }
        Self::Internal
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(error: sqlx::Error) -> Self {
        tracing::error!(error = ?error, "database request failed");
        if is_transient_database_error(&error) {
            transient_database_response()
        } else {
            Self::Internal
        }
    }
}

fn is_transient_database_error(error: &sqlx::Error) -> bool {
    matches!(
        error,
        sqlx::Error::Io(_)
            | sqlx::Error::Tls(_)
            | sqlx::Error::PoolTimedOut
            | sqlx::Error::PoolClosed
            | sqlx::Error::WorkerCrashed
    )
}

fn transient_database_response() -> ApiError {
    ApiError::ServiceUnavailable("The database is temporarily busy. Please retry shortly".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_timeout_is_reported_as_service_unavailable() {
        let response = ApiError::from(sqlx::Error::PoolTimedOut).into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn wrapped_pool_timeout_is_reported_as_service_unavailable() {
        let error = anyhow::Error::new(sqlx::Error::PoolTimedOut)
            .context("failed while loading tenant data");
        let response = ApiError::from(error).into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn query_shape_errors_remain_internal_errors() {
        let response = ApiError::from(sqlx::Error::RowNotFound).into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
