//! Transaction model. Begin / commit / rollback are on the core trait;
//! savepoints are on engine-specific ext traits because their naming
//! semantics diverge (PG anonymous + rollback-to-name vs SQL Server
//! `SAVE TRANSACTION n` / `ROLLBACK TRANSACTION n`).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TxId(pub u64);

impl TxId {
    pub fn new(id: u64) -> Self {
        Self(id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationLevel {
    ReadUncommitted,
    ReadCommitted,
    RepeatableRead,
    Snapshot,
    Serializable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessMode {
    ReadWrite,
    ReadOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxMode {
    pub isolation: IsolationLevel,
    pub access: AccessMode,
}

impl Default for TxMode {
    fn default() -> Self {
        Self {
            isolation: IsolationLevel::ReadCommitted,
            access: AccessMode::ReadWrite,
        }
    }
}
