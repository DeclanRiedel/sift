use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SqlServerPrivilegeQuery {
    pub principal: Option<String>,
    #[serde(default)]
    pub offset: u32,
    pub limit: Option<u32>,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SqlServerPrivilegeObjectKind {
    Database,
    Schema,
    Table,
    View,
    Column,
    Sequence,
    Routine,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SqlServerPrivilegeCheck {
    pub privilege: String,
    /// None is native NULL/unavailable, never an inferred denial.
    pub allowed: Option<bool>,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SqlServerPrivilegeObject {
    pub kind: SqlServerPrivilegeObjectKind,
    pub object_id: i32,
    pub column_number: Option<i32>,
    pub schema: Option<String>,
    pub relation: Option<String>,
    pub name: String,
    pub checks: Vec<SqlServerPrivilegeCheck>,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SqlServerPrivilegeReport {
    pub database: String,
    pub actor: String,
    pub principal: String,
    pub principal_id: i32,
    pub database_user_token: bool,
    pub objects: Vec<SqlServerPrivilegeObject>,
    #[serde(default)]
    pub next_offset: Option<u32>,
    #[serde(default)]
    pub page_limit_reached: bool,
    #[serde(default)]
    pub warning: String,
}
