//! Reviewed SQL Server import controls. Data and credentials are never progress fields.
use serde::{Deserialize, Serialize};
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SqlServerIdentityMode {
    #[default]
    Reject,
    Generate,
    Preserve,
}
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SqlServerComputedMode {
    #[default]
    Reject,
    Omit,
}
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct SqlServerColumnMode {
    pub identity: SqlServerIdentityMode,
    pub computed: SqlServerComputedMode,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SqlServerImportConfirmation {
    pub precondition: String,
    pub confirmed: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SqlServerImportRequest {
    pub run_id: uuid::Uuid,
    pub mode: SqlServerColumnMode,
    #[serde(default)]
    pub confirmation: Option<SqlServerImportConfirmation>,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SqlServerImportReview {
    pub precondition: String,
    pub database: String,
    pub schema: String,
    pub table: String,
    pub mode: SqlServerColumnMode,
    pub imported_columns: Vec<String>,
    pub omitted_columns: Vec<String>,
    pub identity_column: Option<String>,
    #[serde(default)]
    pub target_columns: Vec<SqlServerImportColumnReview>,
    pub warning: String,
    #[serde(default)]
    pub cleanup_pending: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SqlServerImportColumnReview {
    pub source: String,
    pub target: String,
    pub native_type: String,
    pub max_length_bytes: i32,
    pub precision: u8,
    pub scale: u8,
    pub nullable: bool,
    pub identity: bool,
    pub computed: bool,
    pub omitted: bool,
    pub conversion_type: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImportStage {
    Parsing,
    Connecting,
    Reviewing,
    Inserting,
    Committing,
    CleaningUp,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ImportProgress {
    pub run_id: uuid::Uuid,
    pub stage: ImportStage,
    pub rows_total: u64,
    pub rows_attempted: u64,
    pub rows_committed: u64,
    pub cancellation_requested: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ImportCancelResponse {
    pub cancel_requested: bool,
}
