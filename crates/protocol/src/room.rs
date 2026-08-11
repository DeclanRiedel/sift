use serde::{Deserialize, Serialize};

use crate::crdt::{CrdtCursor, CrdtUpdate, DocumentVersion, ReplicaId, RoomResultId};
use crate::Page;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RoomSelection {
    pub anchor: CrdtCursor,
    pub head: CrdtCursor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RoomPresence {
    pub attachment_id: i64,
    pub principal_id: i64,
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_document_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<RoomSelection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RoomQueryStatus {
    Running,
    Ok,
    Error,
    Canceled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RoomQueryResult {
    pub result_id: RoomResultId,
    pub room_id: i64,
    pub actor_principal_id: i64,
    pub connection_profile_id: Option<i64>,
    pub row_count: Option<i64>,
    pub page_count: u64,
    /// One digest per retained result set, in `NextResult` order.
    #[serde(default)]
    pub schema_digests: Vec<String>,
    pub status: RoomQueryStatus,
    pub error_message: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RoomResultPage {
    pub seq: u64,
    pub page: Page,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RoomResultPages {
    pub result_id: RoomResultId,
    pub pages: Vec<RoomResultPage>,
    pub next_seq: u64,
    pub done: bool,
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
    PresenceHeartbeat,
    PresenceUpdate {
        active_document_id: Option<i64>,
        selection: Option<RoomSelection>,
    },
    /// Backward-compatible alias for clients built before leased presence.
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
    /// The authoritative virtual tree or its checkpoint history changed.
    /// Clients refetch using `revision`; SQL text still synchronizes through
    /// the document messages below.
    WorkspaceChanged {
        workspace_id: i64,
        revision: u64,
        checkpoints_changed: bool,
    },
    DdlSourceChanged {
        workspace_id: i64,
        source_id: i64,
        revision: u64,
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
