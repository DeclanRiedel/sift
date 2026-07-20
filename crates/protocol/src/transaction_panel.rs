use serde::{Deserialize, Serialize};

use crate::{ConnectionId, TransactionInfo, TxId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SavepointState {
    Active,
    Released,
    Invalidated,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SavepointInfo {
    pub name: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub state: SavepointState,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TransactionState {
    pub transaction: TransactionInfo,
    pub savepoints: Vec<SavepointInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TransactionEndAction {
    Commit,
    Rollback,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TransactionPreviewRequest {
    pub connection: ConnectionId,
    pub tx_id: TxId,
    pub action: TransactionEndAction,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TransactionPreview {
    pub transaction: TransactionInfo,
    pub action: TransactionEndAction,
    pub age_seconds: u64,
    pub active_savepoints: usize,
    pub closes_savepoints: usize,
    pub destructive: bool,
}
