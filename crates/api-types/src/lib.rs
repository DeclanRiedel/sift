//! Pure request, response, and read-model types for Sift's public HTTP API.
//!
//! This crate deliberately has no I/O, runtime, storage, or operating-system
//! dependencies. Client applications should not need `sift-metadata` merely to
//! deserialize a server response.

mod http;

pub use http::*;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sift_protocol::{ConnectionPolicy, Engine, ProviderId, TenantResourceLimits};

macro_rules! id_type {
    ($($name:ident),+ $(,)?) => {
        $(
            #[derive(
                Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema,
            )]
            pub struct $name(pub i64);
        )+
    };
}

id_type!(
    TenantId,
    PrincipalId,
    GithubAllowlistId,
    TenantInvitationId,
    PrincipalKeyId,
    ApiTokenId,
    ConnectionProfileId,
    RoomId,
    DocumentId,
    QueryHistoryId,
    OperationAuditId,
    SavedQueryId,
    VaultId,
    VaultItemId,
);

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Vault {
    pub id: VaultId,
    pub tenant_id: TenantId,
    pub scope: sift_protocol::VaultScope,
    pub owner_principal_id: Option<PrincipalId>,
    pub name: String,
    pub revision: u64,
    pub effective_capabilities: sift_protocol::VaultCapabilities,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VaultGrant {
    pub vault_id: VaultId,
    pub principal_id: PrincipalId,
    pub capabilities: sift_protocol::VaultCapabilities,
    pub revision: u64,
    pub created_by: PrincipalId,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VaultItemMetadata {
    Connection {
        provider_id: sift_protocol::ProviderId,
        #[serde(default)]
        configuration: serde_json::Value,
    },
    Login {
        #[serde(default)]
        username: String,
        #[serde(default)]
        url: Option<String>,
    },
    Token {
        #[serde(default)]
        service: String,
        #[serde(default)]
        expires_at: Option<DateTime<Utc>>,
    },
    SecureNote,
}

impl VaultItemMetadata {
    pub const fn kind(&self) -> sift_protocol::VaultItemKind {
        match self {
            Self::Connection { .. } => sift_protocol::VaultItemKind::Connection,
            Self::Login { .. } => sift_protocol::VaultItemKind::Login,
            Self::Token { .. } => sift_protocol::VaultItemKind::Token,
            Self::SecureNote => sift_protocol::VaultItemKind::SecureNote,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VaultSecretStatus {
    Missing,
    Configured,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VaultItem {
    pub id: VaultItemId,
    pub vault_id: VaultId,
    pub kind: sift_protocol::VaultItemKind,
    pub label: String,
    pub metadata: VaultItemMetadata,
    pub secret_status: VaultSecretStatus,
    pub head_version: u64,
    pub revision: u64,
    pub created_by: PrincipalId,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VaultItemVersion {
    pub item_id: VaultItemId,
    pub version: u64,
    pub metadata: VaultItemMetadata,
    pub secret_configured: bool,
    pub change_summary: String,
    pub created_by: PrincipalId,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TenantKind {
    Personal,
    Team,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MembershipRole {
    Owner,
    Admin,
    Member,
    Viewer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CredentialMode {
    Shared,
    PerUser,
    Broker,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RoomKind {
    Personal,
    Shared,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RoomRole {
    Owner,
    Editor,
    Viewer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CrdtType {
    Loro,
    Automerge,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueryStatus {
    Ok,
    Error,
    Canceled,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Tenant {
    pub id: TenantId,
    pub name: String,
    pub kind: TenantKind,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GithubAllowlistEntry {
    pub id: GithubAllowlistId,
    pub normalized_login: String,
    pub target_principal_id: Option<PrincipalId>,
    pub created_by: PrincipalId,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub consumed_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TenantInvitation {
    pub id: TenantInvitationId,
    pub tenant_id: TenantId,
    pub intended_role: MembershipRole,
    pub created_by: PrincipalId,
    pub target_principal_id: Option<PrincipalId>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub consumed_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PrincipalKey {
    pub id: PrincipalKeyId,
    pub principal_id: PrincipalId,
    pub public_key: Vec<u8>,
    pub fingerprint: String,
    pub label: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TenantMembership {
    pub tenant: Tenant,
    pub principal_id: PrincipalId,
    pub role: MembershipRole,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ApiTokenRow {
    pub id: ApiTokenId,
    pub principal_id: PrincipalId,
    pub tenant_id: Option<TenantId>,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ConnectionProfile {
    pub id: ConnectionProfileId,
    pub tenant_id: TenantId,
    pub name: String,
    pub provider_id: ProviderId,
    pub configuration: serde_json::Value,
    #[serde(skip, default)]
    #[schemars(skip)]
    pub semantic_engine: Option<Engine>,
    pub credential_mode: CredentialMode,
    #[serde(skip)]
    #[schemars(skip)]
    pub shared_secret_handle: Option<String>,
    pub tags: Vec<String>,
    pub policy: ConnectionPolicy,
    pub created_by: PrincipalId,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TenantLimitOverride {
    pub tenant_id: TenantId,
    pub limits: TenantResourceLimits,
    pub updated_by: PrincipalId,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Room {
    pub id: RoomId,
    pub tenant_id: TenantId,
    pub name: String,
    pub kind: RoomKind,
    pub created_by: PrincipalId,
    pub bound_connection_profile_id: Option<ConnectionProfileId>,
    pub bound_connection_by: Option<PrincipalId>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RoomMember {
    pub room_id: RoomId,
    pub principal_id: PrincipalId,
    pub role: RoomRole,
    pub joined_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Document {
    pub id: DocumentId,
    pub room_id: RoomId,
    pub kind: String,
    pub title: String,
    pub crdt_type: CrdtType,
    pub crdt_state: Vec<u8>,
    pub crdt_format_version: i64,
    pub snapshot_seq: i64,
    pub next_update_seq: i64,
    pub snapshot_version: Vec<u8>,
    pub position: i64,
    pub connection_profile_id: Option<ConnectionProfileId>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct QueryHistory {
    pub id: QueryHistoryId,
    pub principal_id: PrincipalId,
    pub room_id: Option<RoomId>,
    pub connection_profile_id: Option<ConnectionProfileId>,
    pub sql_text: String,
    pub started_at: DateTime<Utc>,
    pub duration_ms: Option<i64>,
    pub row_count: Option<i64>,
    pub status: QueryStatus,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    #[serde(default)]
    pub variable_descriptors: Vec<sift_protocol::RedactedSqlVariableDescriptor>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OperationAudit {
    pub id: OperationAuditId,
    pub at: DateTime<Utc>,
    pub actor_principal_id: Option<PrincipalId>,
    pub action: String,
    pub target: String,
    pub target_id: Option<i64>,
    pub status: String,
    pub result_code: Option<String>,
    pub row_count: Option<i64>,
    pub error_message: Option<String>,
    pub correlation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SavedQuery {
    pub id: SavedQueryId,
    pub tenant_id: TenantId,
    pub owner_principal_id: Option<PrincipalId>,
    pub name: String,
    pub sql_text: String,
    pub connection_profile_id: Option<ConnectionProfileId>,
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SavedQueryScope {
    Personal,
    Shared,
    All,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_bearing_requests_redact_debug_output() {
        let request = SetVcsCredentialRequest {
            expected_revision: 7,
            username: sift_protocol::RedactedString("alice".into()),
            password: sift_protocol::RedactedString("secret".into()),
        };
        let debug = format!("{request:?}");
        assert!(!debug.contains("alice"));
        assert!(!debug.contains("secret"));

        let step_up = VaultRevealStepUpRequest {
            password: "step-up-sentinel".into(),
        };
        assert!(!format!("{step_up:?}").contains("step-up-sentinel"));

        let lease = VaultRevealStepUpResponse {
            lease: "lease-sentinel".into(),
            expires_in_seconds: 60,
        };
        assert!(!format!("{lease:?}").contains("lease-sentinel"));

        let revealed = RevealVaultSecretResponse {
            item_id: 1,
            value: serde_json::json!("reveal-sentinel"),
            expires_in_seconds: 30,
        };
        assert!(!format!("{revealed:?}").contains("reveal-sentinel"));

        let connection = UpsertConnectionProfileRequest {
            tenant_id: 1,
            name: "Warehouse".into(),
            provider_id: sift_protocol::ProviderId::new("sift/postgres").unwrap(),
            configuration: serde_json::json!({"host": "db.internal"}),
            credentials: Some(serde_json::json!({"password": "connection-sentinel"})),
            vault_id: Some(2),
            credential_mode: CredentialMode::Shared,
            tags: Vec::new(),
        };
        assert!(!format!("{connection:?}").contains("connection-sentinel"));
    }
}
