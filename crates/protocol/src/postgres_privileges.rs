use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PostgresPrivilegeQuery {
    pub role: Option<String>,
    #[serde(default)]
    pub offset: u32,
    pub limit: Option<u32>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PostgresPrivilegeObjectKind {
    Database,
    Schema,
    Table,
    Column,
    Sequence,
    Routine,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PostgresPrivilegeCheck {
    pub privilege: String,
    pub allowed: bool,
    pub grantable: bool,
}
/// Native ACL evidence; effective checks also include ownership and inheritance.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PostgresPrivilegeGrant {
    pub grantor_oid: u32,
    pub grantor: Option<String>,
    /// Zero identifies PUBLIC, independently of any role name.
    pub grantee_oid: u32,
    pub grantee: Option<String>,
    pub privilege: String,
    pub grantable: bool,
    /// PUBLIC, the selected role, or native pg_has_role(subject, grantee, USAGE).
    /// Includes native superuser semantics; does not identify a membership path.
    pub applies_to_subject: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PostgresPrivilegeAcl {
    pub owner_oid: u32,
    pub owner: Option<String>,
    /// The stored ACL was NULL and PostgreSQL's built-in defaults were expanded.
    pub defaulted: bool,
    pub grants: Vec<PostgresPrivilegeGrant>,
    pub truncated: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PostgresPrivilegeObject {
    pub kind: PostgresPrivilegeObjectKind,
    pub object_oid: u32,
    pub column_number: Option<i16>,
    pub schema: Option<String>,
    pub relation: Option<String>,
    pub name: String,
    pub row_security_enabled: bool,
    pub row_security_forced: bool,
    pub checks: Vec<PostgresPrivilegeCheck>,
    /// None when an older server did not provide native ACL provenance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acl: Option<PostgresPrivilegeAcl>,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PostgresPrivilegeReport {
    pub database: String,
    pub actor: String,
    pub role: String,
    pub role_superuser: bool,
    pub role_bypass_rls: bool,
    pub objects: Vec<PostgresPrivilegeObject>,
    pub next_offset: Option<u32>,
    pub page_limit_reached: bool,
    pub warning: String,
}
