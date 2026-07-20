use serde::{Deserialize, Serialize};

use crate::{ConnectionId, Engine, SessionId, TxId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    OpenSession,
    ListSessions,
    ListAvailableOperations,
    CloseSession,
    OpenConnection,
    CloseConnection,
    RefreshSchema,
    GenerateDdl,
    ExecuteQuery,
    ExportQuery,
    Complete,
    CancelQuery,
    PreviewEdits,
    ApplyEdits,
    SearchSchema,
    SearchData,
    Explain,
    ListProcesses,
    KillProcess,
    ImportCsv,
    BulkInsert,
    BeginTransaction,
    ListTransactions,
    PreviewTransaction,
    CommitTransaction,
    RollbackTransaction,
    Savepoint,
    RollbackToSavepoint,
    ReleaseSavepoint,
    Metadata,
    AttachRoom,
    DetachRoom,
    ApplyDocumentOperation,
}

impl OperationKind {
    pub const ALL: [Self; 33] = [
        Self::OpenSession,
        Self::ListSessions,
        Self::ListAvailableOperations,
        Self::CloseSession,
        Self::OpenConnection,
        Self::CloseConnection,
        Self::RefreshSchema,
        Self::GenerateDdl,
        Self::ExecuteQuery,
        Self::ExportQuery,
        Self::Complete,
        Self::CancelQuery,
        Self::PreviewEdits,
        Self::ApplyEdits,
        Self::SearchSchema,
        Self::SearchData,
        Self::Explain,
        Self::ListProcesses,
        Self::KillProcess,
        Self::ImportCsv,
        Self::BulkInsert,
        Self::BeginTransaction,
        Self::ListTransactions,
        Self::PreviewTransaction,
        Self::CommitTransaction,
        Self::RollbackTransaction,
        Self::Savepoint,
        Self::RollbackToSavepoint,
        Self::ReleaseSavepoint,
        Self::Metadata,
        Self::AttachRoom,
        Self::DetachRoom,
        Self::ApplyDocumentOperation,
    ];

    pub fn destructive(self) -> bool {
        matches!(
            self,
            Self::ApplyEdits
                | Self::KillProcess
                | Self::ImportCsv
                | Self::BulkInsert
                | Self::CommitTransaction
                | Self::RollbackTransaction
                | Self::Metadata
                | Self::ApplyDocumentOperation
        )
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct OperationCapabilityContext {
    #[serde(default)]
    pub session: Option<SessionId>,
    #[serde(default)]
    pub connection: Option<ConnectionId>,
    #[serde(default)]
    pub transaction: Option<TxId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct OperationCapability {
    pub operation: OperationKind,
    pub available: bool,
    pub reason: Option<String>,
    pub destructive: bool,
    pub engine: Option<Engine>,
}
