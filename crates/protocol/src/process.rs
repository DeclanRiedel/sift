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
