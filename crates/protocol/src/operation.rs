//! Public operation vocabulary. HTTP and WebSocket routes are transport
//! mappings of these operations; adding ad-hoc verbs outside this enum is a
//! protocol break.

use serde::{Deserialize, Serialize};

use crate::{
    BeginTransactionRequest, BulkInsertRequest, CancelRequest, ConnectionId, EndTransactionRequest,
    ExecuteRequestHttp, OpenConnectionRequest, OpenSessionRequest, SavepointRequest, SchemaScope,
    SessionId, TextDocumentOperation,
};

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Operation {
    OpenSession {
        request: OpenSessionRequest,
    },
    ListSessions,
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
    ExecuteQuery {
        session: SessionId,
        request: ExecuteRequestHttp,
    },
    CancelQuery {
        session: SessionId,
        request: CancelRequest,
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
