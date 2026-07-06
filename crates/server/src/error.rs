//! Server-level API error → HTTP status code mapping. Driver errors map by
//! `Code`; everything else maps to internal-server-error with a sanitized
//! message (never leak `Debug` of internal types across the wire).

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use sift_metadata::MetadataError;
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

    #[error("unauthorized")]
    Unauthorized,

    #[error("forbidden: {0}")]
    Forbidden(String),

    #[error("metadata unavailable")]
    MetadataUnavailable,

    #[error("metadata error: {0}")]
    Metadata(#[from] MetadataError),

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
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            ApiError::Forbidden(_) => (StatusCode::FORBIDDEN, "forbidden"),
            ApiError::MetadataUnavailable => {
                (StatusCode::SERVICE_UNAVAILABLE, "metadata_unavailable")
            }
            ApiError::Metadata(error) => match error {
                MetadataError::ConnectionProfileNotFound(_)
                | MetadataError::RoomNotFound(_)
                | MetadataError::DocumentNotFound(_)
                | MetadataError::RoomAttachmentNotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
                MetadataError::TenantMismatch(_, _) => (StatusCode::FORBIDDEN, "forbidden"),
                MetadataError::MissingCredential(_, _)
                | MetadataError::BrokerCredentialUnsupported(_) => {
                    (StatusCode::UNPROCESSABLE_ENTITY, "metadata_unavailable")
                }
                MetadataError::InvalidEnum { .. }
                | MetadataError::InvalidTimestamp { .. }
                | MetadataError::Json(_) => (StatusCode::BAD_REQUEST, "bad_request"),
                MetadataError::Sqlite(_)
                | MetadataError::Migration(_)
                | MetadataError::PasswordHash(_)
                | MetadataError::SecretStore(_) => {
                    (StatusCode::INTERNAL_SERVER_ERROR, "metadata_internal")
                }
            },
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
