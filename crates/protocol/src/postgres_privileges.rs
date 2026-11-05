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
