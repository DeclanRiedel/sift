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
    /// Waiting lock, when the provider exposes one in its process snapshot.
    #[serde(default)]
    pub lock_wait: Option<DatabaseLockWait>,
    /// First sixteen granted locks in deterministic order. The source can
    /// expose more, as indicated by `held_locks_truncated`.
    #[serde(default)]
    pub held_locks: Vec<DatabaseHeldLock>,
    #[serde(default)]
    pub held_locks_truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DatabaseLockWait {
    pub resource: String,
    pub mode: String,
    #[serde(default)]
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DatabaseHeldLock {
    pub resource: String,
    pub mode: String,
}

/// A retained SQL Server system_health deadlock event. The server projects a
/// bounded summary and never sends the raw graph or statement text.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DatabaseDeadlockEvent {
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    pub participants: Vec<DatabaseDeadlockParticipant>,
    pub participants_truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DatabaseDeadlockParticipant {
    pub process_id: i64,
    pub victim: bool,
    pub wait_resource: Option<String>,
    pub lock_mode: Option<String>,
    pub wait_ms: Option<u64>,
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
