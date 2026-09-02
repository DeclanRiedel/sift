//! Server-level API error → HTTP status code mapping. Driver errors map by
//! `Code`; everything else maps to internal-server-error with a sanitized
//! message (never leak `Debug` of internal types across the wire).

use axum::http::{header::RETRY_AFTER, HeaderValue, StatusCode};
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

    #[error("conflict: {0}")]
    Conflict(String),

    #[error(
        "edit {edit_index} affected {affected_rows} rows (expected {expected_rows}); the row changed or no longer matches"
    )]
    EditConflict {
        edit_index: usize,
        affected_rows: u64,
        expected_rows: u64,
    },

    #[error("unauthorized")]
    Unauthorized,

    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("too many authentication attempts")]
    TooManyAuthAttempts,

    #[error("rate limit exceeded")]
    RateLimited { retry_after_secs: u64 },

    #[error("tenant resource exhausted: {resource:?}")]
    TenantResourceExhausted {
        resource: sift_protocol::TenantResource,
        retry_after_secs: Option<u64>,
        durable: bool,
    },

    #[error("metadata unavailable")]
    MetadataUnavailable,

    #[error("service draining")]
    ServiceDraining,

    #[error(
        "unsupported protocol version `{requested}`; server speaks `{}`",
        sift_protocol::PROTOCOL_VERSION
    )]
    UnsupportedProtocolVersion { requested: String },

    #[error("protocol handshake required")]
    ProtocolHandshakeRequired,

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
                Code::ConnectionInvalidated => (StatusCode::GONE, "connection_invalidated"),
                Code::SyntaxError
                | Code::UndefinedObject
                | Code::DuplicateObject
                | Code::InvalidParameterValue => (StatusCode::BAD_REQUEST, "query_invalid"),
                Code::UnsupportedForEngine => {
                    (StatusCode::UNPROCESSABLE_ENTITY, "unsupported_for_engine")
                }
                Code::ResultTooLarge => (StatusCode::PAYLOAD_TOO_LARGE, "result_too_large"),
                Code::RateLimited => (StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
                Code::TenantResourceExhausted => {
                    (StatusCode::TOO_MANY_REQUESTS, "tenant_resource_exhausted")
                }
                Code::EditConflict => (StatusCode::CONFLICT, "edit_conflict"),
                Code::EditNoRowIdentity => (StatusCode::UNPROCESSABLE_ENTITY, "no_row_identity"),
                Code::UnsupportedResultShape => {
                    (StatusCode::UNPROCESSABLE_ENTITY, "unsupported_result_shape")
                }
                Code::SemanticDocumentNotFound => {
                    (StatusCode::NOT_FOUND, "semantic_document_not_found")
                }
                Code::SemanticRevisionConflict => {
                    (StatusCode::CONFLICT, "semantic_revision_conflict")
                }
                Code::InvalidTextRange => (StatusCode::BAD_REQUEST, "invalid_text_range"),
                Code::DialectUnavailable => {
                    (StatusCode::UNPROCESSABLE_ENTITY, "dialect_unavailable")
                }
                Code::SemanticLimitExceeded => {
                    (StatusCode::PAYLOAD_TOO_LARGE, "semantic_limit_exceeded")
                }
                Code::SemanticTimedOut => (StatusCode::GATEWAY_TIMEOUT, "semantic_timed_out"),
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
            ApiError::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
            ApiError::EditConflict { .. } => (StatusCode::CONFLICT, "edit_conflict"),
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            ApiError::Forbidden(_) => (StatusCode::FORBIDDEN, "forbidden"),
            ApiError::TooManyAuthAttempts => {
                (StatusCode::TOO_MANY_REQUESTS, "too_many_auth_attempts")
            }
            ApiError::RateLimited { .. } => (StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
            ApiError::TenantResourceExhausted { durable, .. } => (
                if *durable {
                    StatusCode::CONFLICT
                } else {
                    StatusCode::TOO_MANY_REQUESTS
                },
                "tenant_resource_exhausted",
            ),
            ApiError::MetadataUnavailable => {
                (StatusCode::SERVICE_UNAVAILABLE, "metadata_unavailable")
            }
            ApiError::ServiceDraining => (StatusCode::SERVICE_UNAVAILABLE, "service_draining"),
            ApiError::UnsupportedProtocolVersion { .. } => {
                (StatusCode::UPGRADE_REQUIRED, "unsupported_protocol_version")
            }
            ApiError::ProtocolHandshakeRequired => {
                (StatusCode::UPGRADE_REQUIRED, "protocol_handshake_required")
            }
            ApiError::Metadata(error) => match error {
                MetadataError::ConnectionProfileNotFound(_)
                | MetadataError::RoomNotFound(_)
                | MetadataError::RoomMemberNotFound { .. }
                | MetadataError::DocumentNotFound(_)
                | MetadataError::RoomAttachmentNotFound(_)
                | MetadataError::SavedQueryNotFound(_)
                | MetadataError::SqlSnippetNotFound(_)
                | MetadataError::WorkspaceNotFound(_)
                | MetadataError::WorkspaceNodeNotFound(_)
                | MetadataError::WorkspaceCheckpointNotFound(_)
                | MetadataError::ProjectionBindingNotFound(_)
                | MetadataError::DdlSourceNotFound(_)
                | MetadataError::RepositoryBindingNotFound(_)
                | MetadataError::RunConfigurationNotFound(_)
                | MetadataError::RunNotFound(_)
                | MetadataError::RunScheduleNotFound(_)
                | MetadataError::TransferRecipeNotFound(_)
                | MetadataError::WorkspaceArtifactNotFound(_)
                | MetadataError::CatalogSnapshotNotFound
                | MetadataError::MigrationRunNotFound
                | MetadataError::PlanCaptureNotFound
                | MetadataError::PrincipalNotFound(_)
                | MetadataError::AuthIdentityNotFound(_)
                | MetadataError::AuthSessionNotFound(_)
                | MetadataError::PrincipalKeyNotFound(_)
                | MetadataError::GithubAllowlistNotFound(_)
                | MetadataError::ExtensionNotFound(_)
                | MetadataError::ExtensionStorageNamespaceNotFound
                | MetadataError::VaultNotFound(_)
                | MetadataError::VaultItemNotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
                MetadataError::InstanceCredentialSlotNotFound(_) => {
                    (StatusCode::NOT_FOUND, "instance_credential_not_found")
                }
                MetadataError::ConnectionProfileLimitReached(_) => {
                    (StatusCode::CONFLICT, "tenant_resource_exhausted")
                }
                MetadataError::VaultQuotaExceeded(_) => {
                    (StatusCode::CONFLICT, "vault_quota_exceeded")
                }
                MetadataError::CatalogSnapshotLimitReached
                | MetadataError::PlanCaptureLimitReached => {
                    (StatusCode::CONFLICT, "tenant_resource_exhausted")
                }
                MetadataError::FinalInstanceAdmin
                | MetadataError::FinalAuthIdentity
                | MetadataError::FinalTenantOwner
                | MetadataError::FinalRoomOwner(_)
                | MetadataError::ConnectionProfileManaged(_)
                | MetadataError::InstanceManifestConflict(_)
                | MetadataError::InstanceDestroyApprovalRequired(_)
                | MetadataError::InstancePreventDestroy(_) => (StatusCode::CONFLICT, "conflict"),
                MetadataError::PolicyRevisionConflict { .. } => {
                    (StatusCode::CONFLICT, "policy_revision_conflict")
                }
                MetadataError::SavedQueryRevisionConflict { .. } => {
                    (StatusCode::CONFLICT, "saved_query_revision_conflict")
                }
                MetadataError::SqlSnippetRevisionConflict { .. } => {
                    (StatusCode::CONFLICT, "sql_snippet_revision_conflict")
                }
                MetadataError::WorkspaceRevisionConflict { .. } => {
                    (StatusCode::CONFLICT, "workspace_revision_conflict")
                }
                MetadataError::ProjectionRevisionConflict { .. } => {
                    (StatusCode::CONFLICT, "projection_revision_conflict")
                }
                MetadataError::DdlSourceRevisionConflict { .. } => {
                    (StatusCode::CONFLICT, "ddl_source_revision_conflict")
                }
                MetadataError::RepositoryRevisionConflict { .. } => {
                    (StatusCode::CONFLICT, "repository_revision_conflict")
                }
                MetadataError::RunConfigurationRevisionConflict { .. } => {
                    (StatusCode::CONFLICT, "run_configuration_revision_conflict")
                }
                MetadataError::RunScheduleRevisionConflict { .. } => {
                    (StatusCode::CONFLICT, "run_schedule_revision_conflict")
                }
                MetadataError::TransferRecipeRevisionConflict { .. } => {
                    (StatusCode::CONFLICT, "transfer_recipe_revision_conflict")
                }
                MetadataError::VaultRevisionConflict { .. } => {
                    (StatusCode::CONFLICT, "vault_revision_conflict")
                }
                MetadataError::InvalidRunTransition => {
                    (StatusCode::CONFLICT, "invalid_run_transition")
                }
                MetadataError::WorkspacePathConflict => {
                    (StatusCode::CONFLICT, "workspace_path_conflict")
                }
                MetadataError::WorkspaceDocumentManaged => {
                    (StatusCode::CONFLICT, "workspace_document_managed")
                }
                MetadataError::WorkspaceLimitReached => {
                    (StatusCode::CONFLICT, "workspace_limit_reached")
                }
                MetadataError::CatalogSnapshotRevisionConflict { .. } => {
                    (StatusCode::CONFLICT, "catalog_snapshot_revision_conflict")
                }
                MetadataError::PlanCaptureRevisionConflict { .. } => {
                    (StatusCode::CONFLICT, "plan_capture_revision_conflict")
                }
                MetadataError::MigrationRunTerminal => {
                    (StatusCode::CONFLICT, "migration_run_terminal")
                }
                MetadataError::ExtensionRevisionConflict { .. } => {
                    (StatusCode::CONFLICT, "extension_revision_conflict")
                }
                MetadataError::ExtensionStorageRevisionConflict => {
                    (StatusCode::CONFLICT, "extension_storage_revision_conflict")
                }
                MetadataError::InvalidOperationApproval => {
                    (StatusCode::CONFLICT, "invalid_operation_approval")
                }
                MetadataError::ExtensionStorageQuotaExceeded { .. } => {
                    (StatusCode::CONFLICT, "extension_storage_quota_exceeded")
                }
                MetadataError::ExtensionVersionDigestConflict { .. }
                | MetadataError::ExtensionContributionConflict(_)
                | MetadataError::ExtensionRollbackUnavailable(_) => {
                    (StatusCode::CONFLICT, "extension_conflict")
                }
                MetadataError::TenantAdminRequired
                | MetadataError::TenantMemberRequired
                | MetadataError::InstanceAdminRequired
                | MetadataError::TenantMembershipRequired { .. }
                | MetadataError::RoomOwnerRequired { .. } => (StatusCode::FORBIDDEN, "forbidden"),
                MetadataError::SqlSnippetPermissionDenied => (StatusCode::FORBIDDEN, "forbidden"),
                MetadataError::VaultPermissionDenied => (StatusCode::FORBIDDEN, "forbidden"),
                MetadataError::TenantMismatch(_, _) => (StatusCode::FORBIDDEN, "forbidden"),
                MetadataError::MissingCredential(_, _)
                | MetadataError::BrokerCredentialUnsupported(_)
                | MetadataError::BrokerCredentialModeUnsupported => {
                    (StatusCode::UNPROCESSABLE_ENTITY, "metadata_unavailable")
                }
                MetadataError::CredentialModeMismatch { .. } => {
                    (StatusCode::UNPROCESSABLE_ENTITY, "credential_mode_mismatch")
                }
                MetadataError::InvalidEnum { .. }
                | MetadataError::InvalidPlanCaptureRetention
                | MetadataError::InvalidCredentialObject
                | MetadataError::InlineCredentialsRequireSharedMode
                | MetadataError::InvalidTimestamp { .. }
                | MetadataError::InvalidOAuthAttempt
                | MetadataError::InvalidTenantInvitation
                | MetadataError::InvalidKeyChallenge
                | MetadataError::InvalidSshProxyCapability
                | MetadataError::InvalidPasswordReset
                | MetadataError::ExtensionStorageInvalidKey
                | MetadataError::InvalidCatalogSnapshotDescription
                | MetadataError::InvalidWorkspaceName
                | MetadataError::InvalidWorkspacePath
                | MetadataError::InvalidWorkspaceNode
                | MetadataError::InvalidWorkspaceCheckpoint
                | MetadataError::InvalidWorkspaceBatch
                | MetadataError::InvalidProjectionBinding
                | MetadataError::InvalidDdlSource
                | MetadataError::InvalidRepositoryBinding
                | MetadataError::InvalidRunConfiguration
                | MetadataError::InvalidRunSchedule
                | MetadataError::InvalidTransferRecipe
                | MetadataError::InvalidSqlSnippet(_)
                | MetadataError::InstanceConfig(_)
                | MetadataError::InstanceCredentialInvalid { .. }
                | MetadataError::InvalidVaultInput(_)
                | MetadataError::Json(_) => (StatusCode::BAD_REQUEST, "bad_request"),
                MetadataError::VaultSecretMissing => {
                    (StatusCode::UNPROCESSABLE_ENTITY, "vault_secret_missing")
                }
                MetadataError::VaultSecretNotRevealable => (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "vault_secret_not_revealable",
                ),
                MetadataError::ExtensionStorageValueTooLarge { .. }
                | MetadataError::CatalogSnapshotTooLarge { .. }
                | MetadataError::PlanCaptureTooLarge { .. } => {
                    (StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large")
                }
                MetadataError::Sqlite(_)
                | MetadataError::Migration(_)
                | MetadataError::MigrationRequired { .. }
                | MetadataError::InvalidMigrationHistory(_)
                | MetadataError::AutomaticMigrationBlocked { .. }
                | MetadataError::BinaryTooOld { .. }
                | MetadataError::MigrationInProgress(_)
                | MetadataError::MigrationLockMismatch
                | MetadataError::FileBackedStoreRequired
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

    fn retry_after_secs(&self) -> Option<u64> {
        match self {
            Self::RateLimited { retry_after_secs } => Some(*retry_after_secs),
            Self::TenantResourceExhausted {
                retry_after_secs, ..
            } => *retry_after_secs,
            _ => None,
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
        let retry_after = self.retry_after_secs();
        let edit_conflict = match &self {
            Self::EditConflict {
                edit_index,
                affected_rows,
                expected_rows,
            } => Some(sift_protocol::EditConflict {
                edit_index: *edit_index,
                affected_rows: *affected_rows,
                expected_rows: *expected_rows,
            }),
            _ => None,
        };
        let body = sift_protocol::ApiErrorResponse {
            kind: kind.to_string(),
            message,
            correlation_id,
            retry_after_secs: retry_after,
            edit_conflict,
        };
        let mut response = (status, Json(body)).into_response();
        if let Some(seconds) = retry_after {
            if let Ok(value) = HeaderValue::from_str(&seconds.to_string()) {
                response.headers_mut().insert(RETRY_AFTER, value);
            }
        }
        response
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

/// Document a default error response (the sanitized `ApiErrorResponse` body)
/// for every handler that returns `ApiResult<_>`. aide walks the handler
/// return type, so this attaches the error schema to each generated operation
/// without a per-route declaration.
impl aide::OperationOutput for ApiError {
    type Inner = sift_protocol::ApiErrorResponse;

    fn operation_response(
        ctx: &mut aide::r#gen::GenContext,
        operation: &mut aide::openapi::Operation,
    ) -> Option<aide::openapi::Response> {
        <axum::Json<sift_protocol::ApiErrorResponse> as aide::OperationOutput>::operation_response(
            ctx, operation,
        )
    }

    fn inferred_responses(
        ctx: &mut aide::r#gen::GenContext,
        operation: &mut aide::openapi::Operation,
    ) -> Vec<(Option<u16>, aide::openapi::Response)> {
        Self::operation_response(ctx, operation)
            .map(|response| vec![(None, response)])
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_sets_retry_after() {
        let response = ApiError::RateLimited {
            retry_after_secs: 3,
        }
        .into_response();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()[RETRY_AFTER], "3");
    }

    #[test]
    fn durable_tenant_exhaustion_is_a_conflict_without_retry_hint() {
        let response = ApiError::TenantResourceExhausted {
            resource: sift_protocol::TenantResource::ConnectionProfiles,
            retry_after_secs: None,
            durable: true,
        }
        .into_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert!(!response.headers().contains_key(RETRY_AFTER));
    }
}
