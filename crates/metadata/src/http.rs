//! HTTP request/response DTOs shared by the server and the client SDK.
//!
//! Extracted from the two sides to prevent silent wire-shape drift: prior
//! to this module the SDK re-declared each request struct in parallel
//! with the server's private copy, and a rename on either side would
//! only surface at runtime.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sift_protocol::{
    ProviderId, Workspace, WorkspaceCheckpointId, WorkspaceCheckpointReason, WorkspaceNode,
    WorkspaceNodeId, WorkspaceNodeKind, WorkspacePath, WorkspaceRevision,
};

use crate::{ApiTokenRow, CredentialMode, RoomKind, RoomRole};

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
pub struct BindRoomConnectionRequest {
    pub connection_profile_id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateDocumentRequest {
    pub kind: String,
    pub title: String,
    /// Optional initial SQL text. The server builds the canonical Loro snapshot;
    /// clients no longer choose a CRDT backend or supply raw snapshot bytes.
    #[serde(default)]
    pub initial_text: Option<String>,
    pub position: i64,
    pub connection_profile_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateWorkspaceRequest {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UpdateWorkspaceRequest {
    pub expected_revision: WorkspaceRevision,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ExpectedWorkspaceRevisionRequest {
    pub expected_revision: WorkspaceRevision,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateWorkspaceNodeRequest {
    pub expected_workspace_revision: WorkspaceRevision,
    pub parent_id: Option<WorkspaceNodeId>,
    pub path: WorkspacePath,
    pub kind: WorkspaceNodeKind,
    #[serde(default)]
    pub initial_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MoveWorkspaceNodeRequest {
    pub expected_workspace_revision: WorkspaceRevision,
    pub parent_id: Option<WorkspaceNodeId>,
    pub path: WorkspacePath,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceBatchMutationRequest {
    pub expected_workspace_revision: WorkspaceRevision,
    pub mutations: Vec<WorkspaceBatchMutationItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum WorkspaceBatchMutationItem {
    Create {
        parent_id: Option<WorkspaceNodeId>,
        path: WorkspacePath,
        kind: WorkspaceNodeKind,
        #[serde(default)]
        initial_text: Option<String>,
    },
    Move {
        node_id: WorkspaceNodeId,
        parent_id: Option<WorkspaceNodeId>,
        path: WorkspacePath,
    },
    Delete {
        node_id: WorkspaceNodeId,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateWorkspaceCheckpointRequest {
    pub expected_workspace_revision: WorkspaceRevision,
    pub reason: WorkspaceCheckpointReason,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RestoreWorkspaceCheckpointRequest {
    pub expected_workspace_revision: WorkspaceRevision,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceCheckpointPageQuery {
    pub before_id: Option<WorkspaceCheckpointId>,
    #[serde(default = "default_workspace_checkpoint_page_limit")]
    pub limit: u32,
}

fn default_workspace_checkpoint_page_limit() -> u32 {
    50
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceTreeResponse {
    pub workspace: Workspace,
    pub nodes: Vec<WorkspaceNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UpdateDocumentSnapshotRequest {
    pub crdt_state: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UpsertConnectionProfileRequest {
    pub tenant_id: i64,
    pub name: String,
    pub provider_id: ProviderId,
    pub configuration: serde_json::Value,
    #[serde(default)]
    pub credentials: Option<serde_json::Value>,
    pub credential_mode: CredentialMode,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SetCredentialRequest {
    pub credentials: serde_json::Value,
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
    pub expected_revision: u64,
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
