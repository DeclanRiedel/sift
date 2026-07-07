//! `sift-protocol` — pure serde types, no I/O (ADR-004).
//!
//! The public contract consumed by the server, the desktop binary, and the
//! future wasm web client. Holds operation enums, request/response structs,
//! error codes, and serde models — and nothing else. No `tokio`, no
//! networking, no filesystem.

/// Current wire protocol version. Sent by the server as
/// `X-Sift-Protocol-Version` on every HTTP response.
pub const PROTOCOL_VERSION: &str = "1";

pub mod column;
pub mod connection;
pub mod engine;
pub mod error;
pub mod operation;
pub mod result;
pub mod room;
pub mod schema;
pub mod session;
pub mod tx;
pub mod value;

pub use column::{
    EngineColumnFacets, MssqlColumnFacets, Nullability, PgColumnFacets, PrimitiveType,
    TypeCategory, TypeRef,
};
pub use connection::{
    AccessMode as ConnAccessMode, EngineConnectionSpec, MssqlConnectionSpec, PgConnectionSpec,
    ServerInfo, SslMode,
};
pub use engine::Engine;
pub use error::{Code, DriverError, DriverWarning};
pub use operation::{Operation, OperationSummary};
pub use result::{CursorId, ExecuteRequest, Page, Row};
pub use room::{
    DocumentOperationEnvelope, RoomClientMessage, RoomPresence, RoomQueryResult, RoomQueryStatus,
    RoomServerMessage, TextDocumentOperation,
};
pub use schema::{
    CatalogTree, ConstraintInfo, ConstraintKind, IndexInfo, IndexKind, ObjectInfo, ObjectKind,
    ObjectPath, SchemaDepth, SchemaFilter, SchemaScope, SchemaSnapshot, SchemaTree, TriggerEvent,
    TriggerInfo, TriggerTiming,
};
pub use session::{
    Ack, AuditEntry, BeginTransactionRequest, BulkInsertFormat, BulkInsertRequest,
    BulkInsertResponse, CancelRequest, ConnectionId, ConnectionInfo, EndTransactionRequest,
    ExecuteRequestHttp, ExecuteResponse, Health, OpenConnectionRequest, OpenSessionRequest,
    OperationAuditEntry, OperationStatus, Readiness, SavepointRequest, SessionId, SessionInfo,
    TransactionInfo, TxHandleRef, WsClientMessage, WsServerMessage,
};
pub use tx::{AccessMode as TxAccessMode, IsolationLevel, TxId, TxMode};

/// Re-export of [`ConnectionSpec`].
pub use connection::ConnectionSpec;

/// Re-export of [`ColumnMetadata`].
pub use column::ColumnMetadata;

/// Re-export of [`Value`].
pub use value::Value;
