//! HTTP request/response DTOs shared by the server and the client SDK.
//!
//! Extracted from the two sides to prevent silent wire-shape drift: prior
//! to this module the SDK re-declared each request struct in parallel
//! with the server's private copy, and a rename on either side would
//! only surface at runtime.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sift_protocol::{ConnectionSpec, Engine};

use crate::{ApiTokenRow, CrdtType, CredentialMode, RoomKind, RoomRole};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateRoomRequest {
    pub tenant_id: i64,
    pub name: String,
    pub kind: RoomKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AddRoomMemberRequest {
    pub principal_id: i64,
    pub role: RoomRole,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateDocumentRequest {
    pub kind: String,
    pub title: String,
    pub crdt_type: CrdtType,
    pub crdt_state: Vec<u8>,
    pub position: i64,
    pub connection_profile_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UpdateDocumentSnapshotRequest {
    pub crdt_state: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UpsertConnectionProfileRequest {
    pub tenant_id: i64,
    pub name: String,
    pub engine: Engine,
    pub spec: ConnectionSpec,
    pub credential_mode: CredentialMode,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SetCredentialRequest {
    pub secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OpenConnectionFromProfileRequest {
    pub tenant_id: i64,
    pub profile_id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct IssueTokenRequest {
    pub name: String,
    pub tenant_id: Option<i64>,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct IssueTokenResponse {
    pub token: ApiTokenRow,
    pub plaintext: String,
}
