use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sift_protocol::{ConnectionSpec, Engine};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct TenantId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct PrincipalId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct AuthIdentityId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct GithubAllowlistId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct TenantInvitationId(pub i64);

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct SavedQueryId(pub i64);

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuthIdentityMethod {
    LocalBypass,
    Password,
    Github,
    Oidc,
    Legacy,
}

pub(crate) fn parse_auth_identity_method(value: String) -> crate::Result<AuthIdentityMethod> {
    match value.as_str() {
        "local_bypass" => Ok(AuthIdentityMethod::LocalBypass),
        "password" => Ok(AuthIdentityMethod::Password),
        "github" => Ok(AuthIdentityMethod::Github),
        "oidc" => Ok(AuthIdentityMethod::Oidc),
        "legacy" => Ok(AuthIdentityMethod::Legacy),
        _ => Err(crate::MetadataError::InvalidEnum {
            field: "auth_identity.method",
            value,
        }),
    }
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
    pub avatar_url: Option<String>,
    pub disabled_at: Option<DateTime<Utc>>,
    pub is_instance_admin: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewPasswordPrincipal<'a> {
    pub username: &'a str,
    pub display_name: &'a str,
    pub email: Option<&'a str>,
    pub is_instance_admin: bool,
}

#[derive(Debug, Clone)]
pub struct PasswordIdentity {
    pub identity: AuthIdentity,
    pub principal: Principal,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AuthIdentity {
    pub id: AuthIdentityId,
    pub principal_id: PrincipalId,
    pub method: AuthIdentityMethod,
    pub issuer: String,
    pub subject: String,
    pub provider_login: Option<String>,
    /// Opaque `SecretStore` handle. Never a password verifier or plaintext.
    pub credential_handle: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub disabled_at: Option<DateTime<Utc>>,
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

/// A named, reusable SQL snippet a principal or tenant has saved.
/// Sharing model: `owner_principal_id = Some` → personal (only the
/// owner sees/edits); `None` → tenant-shared (any tenant member sees;
/// only tenant admins edit).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SavedQuery {
    pub id: SavedQueryId,
    pub tenant_id: TenantId,
    /// `None` means the query is tenant-shared. `Some` means it's a
    /// personal query owned by that principal.
    pub owner_principal_id: Option<PrincipalId>,
    pub name: String,
    pub sql_text: String,
    pub connection_profile_id: Option<ConnectionProfileId>,
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NewSavedQuery {
    pub tenant_id: TenantId,
    /// `None` = tenant-shared (visible to whole tenant); `Some` =
    /// personal (only that principal sees it).
    pub owner_principal_id: Option<PrincipalId>,
    pub name: String,
    pub sql_text: String,
    pub connection_profile_id: Option<ConnectionProfileId>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Partial update for a saved query. Fields set to `Some` are applied;
/// `None` leaves the existing value untouched. Tags is `Option<Vec>`
/// so callers can clear tags by sending `Some(vec![])`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct UpdateSavedQuery {
    pub name: Option<String>,
    pub sql_text: Option<String>,
    pub connection_profile_id: Option<Option<ConnectionProfileId>>,
    pub tags: Option<Vec<String>>,
}

/// Query filter for `list_saved_queries`. `q` is a full-text search
/// pattern over name + sql_text (FTS5 MATCH); `tag` restricts to
/// entries whose `tags` array contains all of these values;
/// `scope` narrows visibility to personal-only or shared-only.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SavedQueryFilter {
    pub tenant_id: TenantId,
    pub q: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub scope: Option<SavedQueryScope>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SavedQueryScope {
    /// Only the caller's personal queries.
    Personal,
    /// Only tenant-shared queries.
    Shared,
    /// Personal + shared, as visible to the caller.
    All,
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
