use serde::{Deserialize, Serialize};

use crate::Engine;

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DatabaseProcess {
    pub engine: Engine,
    pub process_id: i64,
    pub user: Option<String>,
    pub database: Option<String>,
    pub state: Option<String>,
    pub statement: Option<String>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub transaction_started_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub state_changed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub wait: Option<String>,
    #[serde(default)]
    pub blocked_by: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct KillProcessRequest {
    pub process_id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct KillProcessResponse {
    pub process_id: i64,
    pub terminated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProcessAlertKind {
    LongRunningQuery,
    IdleInTransaction,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProcessAlert {
    pub kind: ProcessAlertKind,
    pub process_id: i64,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub age_seconds: u64,
    /// False indicates the previously observed condition has resolved.
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProcessAlertSample {
    pub sampled_at: chrono::DateTime<chrono::Utc>,
    pub observed_processes: usize,
    /// At the sample cap, missing rows are not treated as resolved alerts.
    pub incomplete: bool,
    pub changes: Vec<ProcessAlert>,
}
