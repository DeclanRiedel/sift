use serde::{Deserialize, Serialize};

/// Read-only snapshot of SQL Server Agent jobs visible to the connection's login.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentJobsReport {
    pub state: AgentJobsState,
    pub jobs: Vec<AgentJob>,
    /// True when more than 100 jobs were visible.
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentJobsState {
    Available,
    PermissionRequired,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentJob {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub owner: Option<String>,
    pub last_outcome: Option<AgentJobOutcome>,
    /// Server-local wall time from Agent history, with no timezone claim.
    pub last_run_local: Option<String>,
    pub last_duration_seconds: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentJobOutcome {
    Failed,
    Succeeded,
    Retry,
    Canceled,
    InProgress,
    Unknown,
}
