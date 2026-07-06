use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sift_protocol::{ConnectionSpec, Engine};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TenantId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PrincipalId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ApiTokenId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConnectionProfileId(pub i64);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TenantKind {
    Personal,
    Team,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MembershipRole {
    Owner,
    Admin,
    Member,
    Viewer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialMode {
    Shared,
    PerUser,
    Broker,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tenant {
    pub id: TenantId,
    pub name: String,
    pub kind: TenantKind,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Principal {
    pub id: PrincipalId,
    pub external_id: String,
    pub display_name: String,
    pub email: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantMembership {
    pub tenant: Tenant,
    pub principal_id: PrincipalId,
    pub role: MembershipRole,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewConnectionProfile {
    pub name: String,
    pub engine: Engine,
    pub spec: ConnectionSpec,
    pub credential_mode: CredentialMode,
    pub tags: Vec<String>,
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
