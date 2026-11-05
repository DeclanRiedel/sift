use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SqlServerIndexAction {
    Rebuild,
    Reorganize,
    UpdateStatistics,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SqlServerIndexRequest {
    pub schema: String,
    pub table: String,
    pub index: String,
    pub action: SqlServerIndexAction,
    #[serde(default)]
    pub apply: bool,
    #[serde(default)]
    pub production_confirmed: bool,
    #[serde(default)]
    pub backup_verified: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precondition: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SqlServerIndexReport {
    pub sql: String,
    pub applied: bool,
    pub database: String,
    pub precondition: String,
    pub warning: String,
}
