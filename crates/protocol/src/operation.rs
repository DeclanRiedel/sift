//! Public operation vocabulary. HTTP and WebSocket routes are transport
//! mappings of these operations; adding ad-hoc verbs outside this enum is a
//! protocol break.

use serde::{Deserialize, Serialize};

use crate::OperationKind;
use crate::{
    completion::CompletionRequest, BeginTransactionRequest, BulkInsertRequest, CancelRequest,
    ConnectionId, EndTransactionRequest, ExecuteRequestHttp, KillProcessRequest,
    OpenConnectionRequest, OpenSessionRequest, SavepointRequest, SchemaScope, SessionId,
    TextDocumentOperation, TransactionPreviewRequest,
};

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Operation {
    OpenSession {
        request: OpenSessionRequest,
    },
    ListSessions,
    ListAvailableOperations {
        context: crate::OperationCapabilityContext,
    },
    CloseSession {
        session: SessionId,
    },
    OpenConnection {
        session: SessionId,
        request: OpenConnectionRequest,
    },
    CloseConnection {
        session: SessionId,
        connection: ConnectionId,
    },
    RefreshSchema {
        session: SessionId,
        connection: ConnectionId,
        scope: SchemaScope,
    },
    GenerateDdl {
        session: SessionId,
        connection: ConnectionId,
    },
    ExecuteQuery {
        session: SessionId,
        request: ExecuteRequestHttp,
    },
    ExportQuery {
        session: SessionId,
        connection: ConnectionId,
    },
    Complete {
        session: SessionId,
        connection: ConnectionId,
        request: CompletionRequest,
    },
    CancelQuery {
        session: SessionId,
        request: CancelRequest,
    },
    /// Generate (preview) inline-edit DML without executing it.
    PreviewEdits {
        session: SessionId,
        connection: ConnectionId,
    },
    /// Apply an inline-edit set transactionally.
    ApplyEdits {
        session: SessionId,
        connection: ConnectionId,
    },
    /// Fuzzy schema search (object + column names).
    SearchSchema {
        session: SessionId,
        connection: ConnectionId,
    },
    /// Bounded live data search (row contents).
    SearchData {
        session: SessionId,
        connection: ConnectionId,
    },
    /// Capture a query's execution plan (EXPLAIN).
    Explain {
        session: SessionId,
        connection: ConnectionId,
    },
    ListProcesses {
        session: SessionId,
        connection: ConnectionId,
    },
    KillProcess {
        session: SessionId,
        connection: ConnectionId,
        request: KillProcessRequest,
    },
    ImportCsv {
        session: SessionId,
        connection: ConnectionId,
        table: String,
        create_table: bool,
        conflict_policy: crate::CsvConflictPolicy,
    },
    BulkInsert {
        session: SessionId,
        connection: ConnectionId,
        request: BulkInsertRequest,
    },
    BeginTransaction {
        session: SessionId,
        request: BeginTransactionRequest,
    },
    ListTransactions {
        session: SessionId,
    },
    PreviewTransaction {
        session: SessionId,
        request: TransactionPreviewRequest,
    },
    CommitTransaction {
        session: SessionId,
        request: EndTransactionRequest,
    },
    RollbackTransaction {
        session: SessionId,
        request: EndTransactionRequest,
    },
    Savepoint {
        session: SessionId,
        request: SavepointRequest,
    },
    RollbackToSavepoint {
        session: SessionId,
        request: SavepointRequest,
    },
    ReleaseSavepoint {
        session: SessionId,
        request: SavepointRequest,
    },
    /// Catch-all for CRUD-shaped metadata mutations (rooms, documents,
    /// connection profiles, tokens). `action`/`target` are intentionally
    /// free-form strings — the audit sink treats them as opaque tags,
    /// not a bounded vocabulary. Consumers that need to switch on them
    /// should either narrow to the specific enum variants above or
    /// treat unrecognized (action, target) tuples as `Other`.
    Metadata {
        action: String,
        target: String,
        id: Option<i64>,
    },
    AttachRoom {
        room_id: i64,
        attachment_id: i64,
        client_id: String,
    },
    DetachRoom {
        room_id: i64,
        attachment_id: i64,
    },
    ApplyDocumentOperation {
        room_id: i64,
        document_id: i64,
        operation_id: String,
        operation: TextDocumentOperation,
    },
}

/// Sanitized projection of an [`Operation`] for the durable audit log. Carries
/// only *what* and *where* — never request bodies, SQL text, or bind values —
/// so persisting it cannot leak query parameters or secrets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationSummary {
    pub action: String,
    pub target: String,
    pub target_id: Option<i64>,
}

impl Operation {
    pub fn kind(&self) -> OperationKind {
        match self {
            Self::OpenSession { .. } => OperationKind::OpenSession,
            Self::ListSessions => OperationKind::ListSessions,
            Self::ListAvailableOperations { .. } => OperationKind::ListAvailableOperations,
            Self::CloseSession { .. } => OperationKind::CloseSession,
            Self::OpenConnection { .. } => OperationKind::OpenConnection,
            Self::CloseConnection { .. } => OperationKind::CloseConnection,
            Self::RefreshSchema { .. } => OperationKind::RefreshSchema,
            Self::GenerateDdl { .. } => OperationKind::GenerateDdl,
            Self::ExecuteQuery { .. } => OperationKind::ExecuteQuery,
            Self::ExportQuery { .. } => OperationKind::ExportQuery,
            Self::Complete { .. } => OperationKind::Complete,
            Self::CancelQuery { .. } => OperationKind::CancelQuery,
            Self::PreviewEdits { .. } => OperationKind::PreviewEdits,
            Self::ApplyEdits { .. } => OperationKind::ApplyEdits,
            Self::SearchSchema { .. } => OperationKind::SearchSchema,
            Self::SearchData { .. } => OperationKind::SearchData,
            Self::Explain { .. } => OperationKind::Explain,
            Self::ListProcesses { .. } => OperationKind::ListProcesses,
            Self::KillProcess { .. } => OperationKind::KillProcess,
            Self::ImportCsv { .. } => OperationKind::ImportCsv,
            Self::BulkInsert { .. } => OperationKind::BulkInsert,
            Self::BeginTransaction { .. } => OperationKind::BeginTransaction,
            Self::ListTransactions { .. } => OperationKind::ListTransactions,
            Self::PreviewTransaction { .. } => OperationKind::PreviewTransaction,
            Self::CommitTransaction { .. } => OperationKind::CommitTransaction,
            Self::RollbackTransaction { .. } => OperationKind::RollbackTransaction,
            Self::Savepoint { .. } => OperationKind::Savepoint,
            Self::RollbackToSavepoint { .. } => OperationKind::RollbackToSavepoint,
            Self::ReleaseSavepoint { .. } => OperationKind::ReleaseSavepoint,
            Self::Metadata { .. } => OperationKind::Metadata,
            Self::AttachRoom { .. } => OperationKind::AttachRoom,
            Self::DetachRoom { .. } => OperationKind::DetachRoom,
            Self::ApplyDocumentOperation { .. } => OperationKind::ApplyDocumentOperation,
        }
    }

    /// Sanitized `(action, target, target_id)` view for audit records.
    pub fn audit_summary(&self) -> OperationSummary {
        let summary = |action: &str, target: &str, target_id: Option<i64>| OperationSummary {
            action: action.to_string(),
            target: target.to_string(),
            target_id,
        };
        match self {
            Operation::OpenSession { .. } => summary("open", "session", None),
            Operation::ListSessions => summary("list", "session", None),
            Operation::ListAvailableOperations { .. } => {
                summary("list_available", "operation", None)
            }
            Operation::CloseSession { session } => {
                summary("close", "session", Some(session.0 as i64))
            }
            Operation::OpenConnection { session, .. } => {
                summary("open", "connection", Some(session.0 as i64))
            }
            Operation::CloseConnection { connection, .. } => {
                summary("close", "connection", Some(connection.0 as i64))
            }
            Operation::RefreshSchema { connection, .. } => {
                summary("refresh", "schema", Some(connection.0 as i64))
            }
            Operation::GenerateDdl { connection, .. } => {
                summary("generate", "ddl", Some(connection.0 as i64))
            }
            Operation::ExecuteQuery { session, .. } => {
                summary("execute", "query", Some(session.0 as i64))
            }
            Operation::ExportQuery { connection, .. } => {
                summary("export", "query", Some(connection.0 as i64))
            }
            Operation::Complete { session, .. } => {
                summary("complete", "query", Some(session.0 as i64))
            }
            Operation::CancelQuery { session, .. } => {
                summary("cancel", "query", Some(session.0 as i64))
            }
            Operation::PreviewEdits { connection, .. } => {
                summary("preview", "edits", Some(connection.0 as i64))
            }
            Operation::ApplyEdits { connection, .. } => {
                summary("apply", "edits", Some(connection.0 as i64))
            }
            Operation::SearchSchema { connection, .. } => {
                summary("search", "schema", Some(connection.0 as i64))
            }
            Operation::SearchData { connection, .. } => {
                summary("search", "data", Some(connection.0 as i64))
            }
            Operation::Explain { connection, .. } => {
                summary("explain", "query", Some(connection.0 as i64))
            }
            Operation::ListProcesses { connection, .. } => {
                summary("list", "process", Some(connection.0 as i64))
            }
            Operation::KillProcess { request, .. } => {
                summary("kill", "process", Some(request.process_id))
            }
            Operation::ImportCsv { connection, .. } => {
                summary("import", "table", Some(connection.0 as i64))
            }
            Operation::BulkInsert { connection, .. } => {
                summary("bulk_insert", "connection", Some(connection.0 as i64))
            }
            Operation::BeginTransaction { session, .. } => {
                summary("begin", "transaction", Some(session.0 as i64))
            }
            Operation::ListTransactions { session } => {
                summary("list", "transaction", Some(session.0 as i64))
            }
            Operation::PreviewTransaction { session, .. } => {
                summary("preview", "transaction", Some(session.0 as i64))
            }
            Operation::CommitTransaction { session, .. } => {
                summary("commit", "transaction", Some(session.0 as i64))
            }
            Operation::RollbackTransaction { session, .. } => {
                summary("rollback", "transaction", Some(session.0 as i64))
            }
            Operation::Savepoint { session, .. } => {
                summary("savepoint", "transaction", Some(session.0 as i64))
            }
            Operation::RollbackToSavepoint { session, .. } => summary(
                "rollback_to_savepoint",
                "transaction",
                Some(session.0 as i64),
            ),
            Operation::ReleaseSavepoint { session, .. } => {
                summary("release_savepoint", "transaction", Some(session.0 as i64))
            }
            Operation::Metadata { action, target, id } => summary(action, target, *id),
            Operation::AttachRoom { room_id, .. } => summary("attach", "room", Some(*room_id)),
            Operation::DetachRoom { room_id, .. } => summary("detach", "room", Some(*room_id)),
            Operation::ApplyDocumentOperation { document_id, .. } => {
                summary("apply_operation", "document", Some(*document_id))
            }
        }
    }
}
