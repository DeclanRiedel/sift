use serde::{Deserialize, Serialize};

/// Scoped file work on the managed connection's configured SQLite root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum SqliteMaintenanceAction {
    Create { path: String },
    Backup { path: String },
    Vacuum,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SqliteMaintenanceRequest {
    pub action: SqliteMaintenanceAction,
    #[serde(default)]
    pub apply: bool,
    #[serde(default)]
    pub confirm_write: bool,
    #[serde(default)]
    pub backup_verified: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SqliteMaintenanceReport {
    pub action: SqliteMaintenanceAction,
    pub applied: bool,
    pub root_id: String,
    pub source_file: String,
    pub destination_file: Option<String>,
    pub source_bytes: u64,
    /// Conservative extra free-space target for in-place VACUUM, if selected.
    pub estimated_extra_bytes: Option<u64>,
    pub backup_file: Option<String>,
    pub backup_expectation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_token: Option<String>,
}
