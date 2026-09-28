use serde::{Deserialize, Serialize};

/// Bounded read-only snapshot of SQL Server instance configuration.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SqlServerSettingsReport {
    pub state: SqlServerSettingsState,
    pub settings: Vec<SqlServerSetting>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SqlServerSettingsState {
    Available,
    PermissionRequired,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SqlServerSetting {
    pub name: String,
    pub configured_value: i64,
    pub effective_value: i64,
    pub minimum: i64,
    pub maximum: i64,
    pub is_dynamic: bool,
    pub is_advanced: bool,
    pub description: String,
}
