use serde::{Deserialize, Serialize};

/// Bounded, read-only snapshot of the current SQL Server database's Query Store.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct QueryStoreReport {
    pub database: String,
    pub state: QueryStoreState,
    pub plans: Vec<QueryStorePlan>,
    /// The result contains at most 100 plans; true means more were available.
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueryStoreState {
    ReadWrite,
    ReadOnly,
    ReadCaptureSecondary,
    Off,
    Error,
    PermissionRequired,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct QueryStorePlan {
    pub query_id: i64,
    pub plan_id: i64,
    /// SQL text is capped to 2048 UTF-16 characters by the catalog query.
    pub sql_text: String,
    pub executions: i64,
    pub average_duration_ms: f64,
    pub last_execution_at: Option<chrono::DateTime<chrono::Utc>>,
}
