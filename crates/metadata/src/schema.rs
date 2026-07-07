use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sift_protocol::{ConnectionSpec, Engine};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct TenantId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct PrincipalId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct ApiTokenId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct ConnectionProfileId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct RoomId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct DocumentId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct RoomAttachmentId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct QueryHistoryId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct OperationAuditId(pub i64);

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
pub struct Principal {
    pub id: PrincipalId,
    pub external_id: String,
    pub display_name: String,
    pub email: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
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
    pub engine: Engine,
    pub spec: ConnectionSpec,
    pub credential_mode: CredentialMode,
    pub shared_secret_handle: Option<String>,
    pub tags: Vec<String>,
    pub created_by: PrincipalId,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NewConnectionProfile {
    pub name: String,
    pub engine: Engine,
    pub spec: ConnectionSpec,
    pub credential_mode: CredentialMode,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Room {
    pub id: RoomId,
    pub tenant_id: TenantId,
    pub name: String,
    pub kind: RoomKind,
    pub created_by: PrincipalId,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NewRoom {
    pub name: String,
    pub kind: RoomKind,
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
    pub position: i64,
    pub connection_profile_id: Option<ConnectionProfileId>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NewDocument {
    pub kind: String,
    pub title: String,
    pub crdt_type: CrdtType,
    pub crdt_state: Vec<u8>,
    pub position: i64,
    pub connection_profile_id: Option<ConnectionProfileId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RoomAttachment {
    pub id: RoomAttachmentId,
    pub room_id: RoomId,
    pub principal_id: PrincipalId,
    pub client_id: String,
    pub attached_at: DateTime<Utc>,
    pub detached_at: Option<DateTime<Utc>>,
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
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NewQueryHistory {
    pub principal_id: PrincipalId,
    pub room_id: Option<RoomId>,
    pub connection_profile_id: Option<ConnectionProfileId>,
    pub sql_text: String,
    pub duration_ms: Option<i64>,
    pub row_count: Option<i64>,
    pub status: QueryStatus,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

/// A durable operation-audit row: who did what, to which resource, and how it
/// resolved. Never carries request bodies, SQL text, or bind values.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OperationAudit {
    pub id: OperationAuditId,
    pub at: DateTime<Utc>,
    pub actor_principal_id: Option<PrincipalId>,
    pub action: String,
    pub target: String,
    pub target_id: Option<i64>,
    /// `"succeeded"` or `"failed"`.
    pub status: String,
    /// Driver/error code for failures, where available.
    pub result_code: Option<String>,
    pub row_count: Option<i64>,
    /// Sanitized failure message; never includes bind values or secrets.
    pub error_message: Option<String>,
    /// Request correlation ID tying this row to logs and the client request.
    pub correlation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NewOperationAudit {
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

impl CredentialMode {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::PerUser => "per_user",
            Self::Broker => "broker",
        }
    }
}

impl MembershipRole {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Admin => "admin",
            Self::Member => "member",
            Self::Viewer => "viewer",
        }
    }
}

impl TenantKind {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Personal => "personal",
            Self::Team => "team",
        }
    }
}

impl RoomKind {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Personal => "personal",
            Self::Shared => "shared",
        }
    }
}

impl RoomRole {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Editor => "editor",
            Self::Viewer => "viewer",
        }
    }
}

impl CrdtType {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Loro => "loro",
            Self::Automerge => "automerge",
        }
    }
}

impl QueryStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Canceled => "canceled",
        }
    }
}

pub(crate) fn parse_tenant_kind(value: String) -> crate::Result<TenantKind> {
    match value.as_str() {
        "personal" => Ok(TenantKind::Personal),
        "team" => Ok(TenantKind::Team),
        _ => Err(crate::MetadataError::InvalidEnum {
            field: "tenant.kind",
            value,
        }),
    }
}

pub(crate) fn parse_role(value: String) -> crate::Result<MembershipRole> {
    match value.as_str() {
        "owner" => Ok(MembershipRole::Owner),
        "admin" => Ok(MembershipRole::Admin),
        "member" => Ok(MembershipRole::Member),
        "viewer" => Ok(MembershipRole::Viewer),
        _ => Err(crate::MetadataError::InvalidEnum {
            field: "membership.role",
            value,
        }),
    }
}

pub(crate) fn parse_credential_mode(value: String) -> crate::Result<CredentialMode> {
    match value.as_str() {
        "shared" => Ok(CredentialMode::Shared),
        "per_user" => Ok(CredentialMode::PerUser),
        "broker" => Ok(CredentialMode::Broker),
        _ => Err(crate::MetadataError::InvalidEnum {
            field: "connection_profile.credential_mode",
            value,
        }),
    }
}

pub(crate) fn parse_room_kind(value: String) -> crate::Result<RoomKind> {
    match value.as_str() {
        "personal" => Ok(RoomKind::Personal),
        "shared" => Ok(RoomKind::Shared),
        _ => Err(crate::MetadataError::InvalidEnum {
            field: "room.kind",
            value,
        }),
    }
}

pub(crate) fn parse_room_role(value: String) -> crate::Result<RoomRole> {
    match value.as_str() {
        "owner" => Ok(RoomRole::Owner),
        "editor" => Ok(RoomRole::Editor),
        "viewer" => Ok(RoomRole::Viewer),
        _ => Err(crate::MetadataError::InvalidEnum {
            field: "room_member.role",
            value,
        }),
    }
}

pub(crate) fn parse_crdt_type(value: String) -> crate::Result<CrdtType> {
    match value.as_str() {
        "loro" => Ok(CrdtType::Loro),
        "automerge" => Ok(CrdtType::Automerge),
        _ => Err(crate::MetadataError::InvalidEnum {
            field: "document.crdt_type",
            value,
        }),
    }
}

pub(crate) fn parse_query_status(value: String) -> crate::Result<QueryStatus> {
    match value.as_str() {
        "ok" => Ok(QueryStatus::Ok),
        "error" => Ok(QueryStatus::Error),
        "canceled" => Ok(QueryStatus::Canceled),
        _ => Err(crate::MetadataError::InvalidEnum {
            field: "query_history.status",
            value,
        }),
    }
}
