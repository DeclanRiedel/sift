use serde::{Deserialize, Serialize};

use crate::crdt::{CrdtUpdate, DocumentVersion, ReplicaId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RoomPresence {
    pub attachment_id: i64,
    pub principal_id: i64,
    pub client_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RoomQueryStatus {
    Ok,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RoomQueryResult {
    pub room_id: i64,
    pub actor_principal_id: i64,
    pub connection_profile_id: Option<i64>,
    pub sql_text: String,
    pub row_count: Option<i64>,
    pub status: RoomQueryStatus,
    pub error_message: Option<String>,
}

/// Stable error codes for the collaborative document protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DocumentErrorCode {
    InvalidCrdtUpdate,
    CrdtDependenciesMissing,
    ReplicaInUse,
    DocumentVersionNotFound,
    DocumentTooLarge,
    RoomConnectionNotFound,
    RoomConnectionBroken,
    RoomResultNotFound,
    RoomResultExpired,
    Forbidden,
    NotFound,
    Internal,
}

/// Whether a chunked transfer carries a full snapshot or an update range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DocumentTransferKind {
    Snapshot,
    Update,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RoomClientMessage {
    Reauthenticate {
        access_token: crate::RedactedString,
    },
    Attach {
        client_id: String,
    },
    Detach,
    PresencePing,
    /// Ask the server to bring this replica up to date. `known_version` is the
    /// client's encoded Loro version vector; an empty vector requests a full
    /// snapshot bootstrap.
    DocumentSync {
        request_id: String,
        document_id: i64,
        replica_id: ReplicaId,
        known_version: DocumentVersion,
    },
    /// Submit a native Loro update authored by `replica_id`. The server durably
    /// sequences it before acknowledging and rebroadcasting.
    DocumentUpdate {
        request_id: String,
        update_id: String,
        document_id: i64,
        replica_id: ReplicaId,
        update: CrdtUpdate,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RoomServerMessage {
    Authenticated {
        expires_at: chrono::DateTime<chrono::Utc>,
    },
    Attached {
        attachment_id: i64,
        presence: Vec<RoomPresence>,
    },
    Presence {
        presence: Vec<RoomPresence>,
    },
    QueryResult {
        result: RoomQueryResult,
    },
    /// One decoded chunk of a snapshot or update transfer. Chunks for a transfer
    /// share `transfer_id`, arrive in `index` order, and total `count`.
    DocumentChunk {
        request_id: String,
        document_id: i64,
        transfer_id: String,
        index: u32,
        count: u32,
        payload_kind: DocumentTransferKind,
        payload: CrdtUpdate,
        snapshot_seq: i64,
        server_version: DocumentVersion,
    },
    /// Terminal marker for a `DocumentSync`: the client now holds everything
    /// through `server_version`.
    DocumentSynced {
        request_id: String,
        document_id: i64,
        server_version: DocumentVersion,
    },
    /// Durable acknowledgement to the submitter after the update commits.
    DocumentUpdateAck {
        request_id: String,
        update_id: String,
        document_id: i64,
        server_seq: i64,
        version_fingerprint: String,
    },
    /// Committed update rebroadcast to the room after durable commit.
    DocumentUpdateCommitted {
        document_id: i64,
        replica_id: ReplicaId,
        server_seq: i64,
        update: CrdtUpdate,
        server_version: DocumentVersion,
    },
    /// The receiver fell behind or the runtime restarted; it must resynchronize
    /// from its current version without reconnecting.
    ResyncRequired {
        runtime_epoch: String,
        event_seq: u64,
    },
    /// Structured collaborative-document error. `request_id` echoes the client
    /// message it answers, when there is one.
    DocumentError {
        request_id: Option<String>,
        document_id: i64,
        code: DocumentErrorCode,
        message: String,
    },
    Error {
        message: String,
    },
    RateLimited {
        retry_after_ms: u64,
    },
}
