use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RestoreFileMove {
    pub logical_name: String,
    pub destination: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum SqlServerRecoveryRequest {
    Backup {
        database: String,
        archive_path: String,
        #[serde(default)]
        apply: bool,
    },
    Restore {
        database: String,
        archive_path: String,
        backup_set: u32,
        moves: Vec<RestoreFileMove>,
        #[serde(default)]
        apply: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SqlServerRecoveryReport {
    pub applied: bool,
    pub sql: String,
    pub source_database: Option<String>,
    pub warnings: Vec<crate::DriverWarning>,
}
