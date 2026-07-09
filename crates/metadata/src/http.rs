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

/// Body for POST /v1/metadata/saved-queries. `owner_principal_id`
/// governs visibility: `Some` = personal to that principal, `None` =
/// tenant-shared. The server enforces that a caller cannot create a
/// personal query owned by a different principal.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateSavedQueryRequest {
    pub tenant_id: i64,
    #[serde(default)]
    pub owner_principal_id: Option<i64>,
    pub name: String,
    pub sql_text: String,
    #[serde(default)]
    pub connection_profile_id: Option<i64>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Body for PUT /v1/metadata/saved-queries/:id. All fields optional;
/// unset ones are left untouched. `connection_profile_id` uses a
/// double Option so callers can distinguish "leave alone" (absent)
/// from "clear it" (present with null).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct UpdateSavedQueryRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub sql_text: Option<String>,
    #[serde(default, deserialize_with = "sq_deserialize_conn_profile")]
    pub connection_profile_id: Option<Option<i64>>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

fn sq_deserialize_conn_profile<'de, D>(deserializer: D) -> Result<Option<Option<i64>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // `Some(None)` = the JSON key was present and set to null → clear.
    // `Some(Some(id))` = present and set to a number → assign.
    // `None` = absent → leave unchanged.
    let opt = <Option<Option<i64>> as serde::Deserialize>::deserialize(deserializer)?;
    Ok(opt)
}
