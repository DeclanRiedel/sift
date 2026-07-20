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
    #[error("too many authentication attempts")]
    TooManyAuthAttempts,

    #[error("metadata unavailable")]
    MetadataUnavailable,

    #[error("service draining")]
    ServiceDraining,

    #[error(
        "unsupported protocol version `{requested}`; server speaks `{}`",
        sift_protocol::PROTOCOL_VERSION
    )]
    UnsupportedProtocolVersion { requested: String },

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
                Code::ResultTooLarge => (StatusCode::PAYLOAD_TOO_LARGE, "result_too_large"),
                Code::EditConflict => (StatusCode::CONFLICT, "edit_conflict"),
                Code::EditNoRowIdentity => (StatusCode::UNPROCESSABLE_ENTITY, "no_row_identity"),
                Code::UnsupportedResultShape => {
                    (StatusCode::UNPROCESSABLE_ENTITY, "unsupported_result_shape")
                }
                Code::CursorNotFound | Code::TransactionNotFound => {
                    (StatusCode::NOT_FOUND, "not_found")
                }
                Code::CursorEvicted => (StatusCode::GONE, "cursor_evicted"),
                Code::CursorLimitReached => (StatusCode::TOO_MANY_REQUESTS, "cursor_limit_reached"),
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
            ApiError::TooManyAuthAttempts => {
                (StatusCode::TOO_MANY_REQUESTS, "too_many_auth_attempts")
            }
            ApiError::MetadataUnavailable => {
                (StatusCode::SERVICE_UNAVAILABLE, "metadata_unavailable")
            }
            ApiError::ServiceDraining => (StatusCode::SERVICE_UNAVAILABLE, "service_draining"),
            ApiError::UnsupportedProtocolVersion { .. } => {
                (StatusCode::BAD_REQUEST, "unsupported_protocol_version")
            }
            ApiError::Metadata(error) => match error {
                MetadataError::ConnectionProfileNotFound(_)
                | MetadataError::RoomNotFound(_)
                | MetadataError::DocumentNotFound(_)
                | MetadataError::RoomAttachmentNotFound(_)
                | MetadataError::SavedQueryNotFound(_)
                | MetadataError::PrincipalNotFound(_)
                | MetadataError::AuthIdentityNotFound(_)
                | MetadataError::GithubAllowlistNotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
                MetadataError::FinalInstanceAdmin => (StatusCode::CONFLICT, "conflict"),
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
                | MetadataError::InvalidAuthTokenKey
                | MetadataError::SecretStore(_)
                | MetadataError::Io(_)
                | MetadataError::BlockingTask(_) => {
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
        // Correlation ID is set on the request task by the middleware; echo it
        // in the error body too so a client sees the same id it gets in the
        // response header and the server logs/audit carry.
        let correlation_id = crate::correlation::current();
        tracing::warn!(%status, %kind, %message, correlation_id = ?correlation_id, "api error");
        let body = serde_json::json!({
            "kind": kind,
            "message": message,
            "correlation_id": correlation_id,
        });
        (status, Json(body)).into_response()
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
