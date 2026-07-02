//! Server-level API error → HTTP status code mapping. Driver errors map by
//! `Code`; everything else maps to internal-server-error with a sanitized
//! message (never leak `Debug` of internal types across the wire).

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use sift_protocol::{Code, DriverError};

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("driver error: {0}")]
    Driver(#[from] DriverError),

    #[error("session not found: {0}")]
    SessionNotFound(sift_protocol::SessionId),

    #[error("connection not found: {0}")]
    ConnectionNotFound(sift_protocol::ConnectionId),

    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("internal error: {0}")]
    Internal(String),
}

impl ApiError {
    fn status_and_code(&self) -> (StatusCode, &'static str) {
        match self {
            ApiError::Driver(de) => match de.code {
                Code::ConnectionFailed | Code::AuthFailed | Code::PoolExhausted => {
                    (StatusCode::BAD_GATEWAY, "driver_unreachable")
                }
                Code::QueryTimedOut => (StatusCode::GATEWAY_TIMEOUT, "query_timeout"),
                Code::QueryCanceled => (StatusCode::REQUEST_TIMEOUT, "query_canceled"),
                Code::SyntaxError
                | Code::UndefinedObject
                | Code::DuplicateObject
                | Code::InvalidParameterValue => (StatusCode::BAD_REQUEST, "query_invalid"),
                Code::UnsupportedForEngine => {
                    (StatusCode::UNPROCESSABLE_ENTITY, "unsupported_for_engine")
                }
                Code::CursorNotFound | Code::TransactionNotFound => {
                    (StatusCode::NOT_FOUND, "not_found")
                }
                Code::Other { .. } | Code::DriverInternal => {
                    (StatusCode::INTERNAL_SERVER_ERROR, "driver_internal")
                }
            },
            ApiError::SessionNotFound(_) | ApiError::ConnectionNotFound(_) => {
                (StatusCode::NOT_FOUND, "not_found")
            }
            ApiError::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            ApiError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, kind) = self.status_and_code();
        let message = self.to_string();
        tracing::warn!(%status, %kind, %message, "api error");
        let body = serde_json::json!({
            "kind": kind,
            "message": message,
        });
        (status, Json(body)).into_response()
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
