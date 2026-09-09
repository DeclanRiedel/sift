use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum PostgresMaintenanceAction {
    Vacuum {
        #[serde(default)]
        analyze: bool,
    },
    Analyze,
    ReindexTable {
        #[serde(default)]
        concurrently: bool,
    },
    ReindexIndex {
        #[serde(default)]
        concurrently: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PostgresMaintenanceRequest {
    pub action: PostgresMaintenanceAction,
    pub schema: String,
    pub name: String,
    /// False previews the generated SQL without sending it to the database.
    #[serde(default)]
    pub apply: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PostgresMaintenanceReport {
    pub sql: String,
    pub applied: bool,
    pub warnings: Vec<crate::DriverWarning>,
}
