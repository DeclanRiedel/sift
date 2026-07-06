//! Server-facing protocol types: session and connection ids, open/info
//! envelopes, and the execute response shape. These belong in `protocol`
//! (pure serde, ADR-004) so the desktop binary, future wasm client, and
//! the server all share them.

use serde::{Deserialize, Serialize};

use crate::{
    ColumnMetadata, ConnectionSpec, CursorId, DriverWarning, Engine, Operation, Page, Row, TxId,
    TxMode, Value,
};

/// Opaque session id. Stable for the lifetime of the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionId(pub u64);

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Opaque connection id. Unique within a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ConnectionId(pub u64);

impl std::fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Body of `POST /v1/sessions`. Tags optional — the server ignores them for
/// now; clients use them to label sessions in UI.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct OpenSessionRequest {
    #[serde(default)]
    pub tag: Option<String>,
}

/// Body of `POST /v1/sessions/:id/connections`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct OpenConnectionRequest {
    pub engine: Engine,
    #[serde(flatten)]
    pub spec: ConnectionSpec,
}

/// Server-reported session metadata. Returned by `GET /v1/sessions/:id` and
/// `POST /v1/sessions`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionInfo {
    pub id: SessionId,
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(default)]
    pub tag: Option<String>,
    pub connections: Vec<ConnectionInfo>,
}

/// Server-reported connection metadata.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ConnectionInfo {
    pub id: ConnectionId,
    pub engine: Engine,
    /// Display name — host/database for PG/SQL Server.
    pub display_name: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Body of `POST /v1/sessions/:id/queries`. Sync HTTP path returns the whole
/// result inline; WS streaming path (PHASE0 step 10) replaces this with a
/// streamed page consumer.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExecuteRequestHttp {
    pub connection: ConnectionId,
    pub sql: String,
    #[serde(default)]
    pub params: Vec<Value>,
    /// Optional transaction to run under. None = autocommit.
    #[serde(default)]
    pub tx: Option<TxHandleRef>,
    /// Optional metadata room context for query history attribution.
    #[serde(default)]
    pub room_id: Option<i64>,
    /// Optional metadata connection profile context for query history attribution.
    #[serde(default)]
    pub connection_profile_id: Option<i64>,
}

/// Reference to an open transaction. Returned by the (TBD) transactions
/// endpoint; carried back by the client on subsequent queries.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TxHandleRef {
    pub tx_id: TxId,
    pub connection: ConnectionId,
    pub mode: TxMode,
}

/// Body of `POST /v1/sessions/:id/transactions`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BeginTransactionRequest {
    pub connection: ConnectionId,
    #[serde(default)]
    pub mode: TxMode,
}

/// Body of transaction-ending endpoints.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EndTransactionRequest {
    pub connection: ConnectionId,
    pub tx_id: TxId,
}

/// Server-visible transaction metadata.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TransactionInfo {
    pub tx_id: TxId,
    pub connection: ConnectionId,
    pub mode: TxMode,
    pub opened_at: chrono::DateTime<chrono::Utc>,
}

/// Sync execute response. The HTTP surface drains the driver's page stream
/// into `rows`; `has_more` is always `false` in the sync path (the WS
/// streaming surface uses `cursor_id` to page future results, PHASE0 #10).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExecuteResponse {
    pub cursor_id: CursorId,
    pub columns: Vec<ColumnMetadata>,
    pub rows: Vec<Row>,
    pub affected_rows: Option<u64>,
    pub warnings: Vec<DriverWarning>,
    pub has_more: bool,
}

/// Body of `POST /v1/sessions/:id/queries/:cursor/cancel`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CancelRequest {
    pub connection: ConnectionId,
    pub cursor: CursorId,
}

/// Body of `POST /v1/sessions/:id/connections/:conn_id/bulk-insert`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BulkInsertRequest {
    pub table: String,
    #[schemars(with = "Vec<u8>")]
    pub data: Vec<u8>,
    #[serde(default)]
    pub format: BulkInsertFormat,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum BulkInsertFormat {
    #[default]
    Csv,
    Native,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BulkInsertResponse {
    pub rows_inserted: u64,
}

/// Generic ok-ack body.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Ack {
    pub ok: bool,
}

/// Server-reported health. `GET /v1/health`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Health {
    pub status: String,
    pub version: String,
    pub engines: Vec<Engine>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AuditEntry {
    pub at: chrono::DateTime<chrono::Utc>,
    pub method: String,
    pub path: String,
    pub status: u16,
    pub duration_ms: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct OperationAuditEntry {
    pub at: chrono::DateTime<chrono::Utc>,
    pub operation: Operation,
    pub status: OperationStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    Succeeded,
    Failed,
}

/// WebSocket client → server messages. The streaming surface is intentionally
/// protocol-owned: external clients can consume it without importing server
/// internals.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WsClientMessage {
    Execute {
        request_id: String,
        connection: ConnectionId,
        sql: String,
        #[serde(default)]
        params: Vec<Value>,
    },
    Listen {
        request_id: String,
        connection: ConnectionId,
        channels: Vec<String>,
    },
    Ack {
        cursor_id: CursorId,
        seq: u64,
    },
    Cancel {
        connection: ConnectionId,
        cursor_id: CursorId,
    },
}

/// WebSocket server → client messages. Each `Page` must be acked by
/// `(cursor_id, seq)` before the server sends the next page, providing the
/// Phase 0 backpressure contract.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WsServerMessage {
    Started {
        request_id: String,
        cursor_id: CursorId,
    },
    Page {
        cursor_id: CursorId,
        seq: u64,
        page: Page,
    },
    Notification {
        request_id: String,
        channel: String,
        payload: String,
    },
    Error {
        request_id: Option<String>,
        message: String,
    },
}
