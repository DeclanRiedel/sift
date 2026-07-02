//! Public operation vocabulary. HTTP and WebSocket routes are transport
//! mappings of these operations; adding ad-hoc verbs outside this enum is a
//! protocol break.

use serde::{Deserialize, Serialize};

use crate::{
    BeginTransactionRequest, CancelRequest, ConnectionId, EndTransactionRequest,
    ExecuteRequestHttp, OpenConnectionRequest, OpenSessionRequest, SchemaScope, SessionId,
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
}
