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
    DdlSourceMapping, ProjectionBindingId, ProjectionMode, ProviderId, ReconcileEntry,
    ReconcileResolution, RedactedString, VcsDiffSide, Workspace, WorkspaceCheckpointId,
    WorkspaceCheckpointReason, WorkspaceNode, WorkspaceNodeId, WorkspaceNodeKind, WorkspacePath,
    WorkspaceRevision,
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
pub struct BindWorkspaceProjectionRequest {
    pub root_handle: String,
    pub mode: ProjectionMode,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ExpectedProjectionRevisionRequest {
    pub expected_revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ApplyWorkspaceProjectionRequest {
    pub binding_revision: u64,
    pub workspace_revision: WorkspaceRevision,
    pub resolutions: Vec<ProjectionResolutionRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ProjectionResolutionRequest {
    pub observed: ReconcileEntry,
    pub resolution: ReconcileResolution,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateDdlSourceRequest {
    pub name: String,
    pub dialect_id: String,
    pub roots: Vec<WorkspaceNodeId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UpdateDdlSourceRequest {
    pub expected_revision: u64,
    pub name: String,
    pub dialect_id: String,
    pub roots: Vec<WorkspaceNodeId>,
    #[serde(default)]
    pub mappings: Vec<DdlSourceMapping>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ExpectedDdlSourceRevisionRequest {
    pub expected_revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BindRepositoryRequest {
    pub projection_id: ProjectionBindingId,
    #[serde(default)]
    pub initialize: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ExpectedRepositoryRevisionRequest {
    pub expected_revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VcsDiffQuery {
    #[serde(default = "default_vcs_diff_side")]
    pub side: VcsDiffSide,
    #[serde(default)]
    pub path: Option<WorkspacePath>,
}

fn default_vcs_diff_side() -> VcsDiffSide {
    VcsDiffSide::IndexToWorktree
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VcsPathsRequest {
    pub expected_revision: u64,
    pub paths: Vec<WorkspacePath>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VcsCommitRequest {
    pub expected_revision: u64,
    pub message: String,
    pub author_name: String,
    pub author_email: String,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct SetVcsCredentialRequest {
    pub expected_revision: u64,
    pub username: RedactedString,
    pub password: RedactedString,
}

impl std::fmt::Debug for SetVcsCredentialRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SetVcsCredentialRequest")
            .field("expected_revision", &self.expected_revision)
            .field("username", &"[REDACTED]")
            .field("password", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VcsRemoteRequest {
    pub expected_revision: u64,
    #[serde(default = "default_git_remote")]
    pub remote: String,
    #[serde(default)]
    pub branch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateRunConfigurationRequest {
    pub name: String,
    pub scripts: Vec<sift_protocol::RunScriptStep>,
    pub connection_profile_id: i64,
    pub target_schema: Option<String>,
    #[serde(default)]
    pub variables: Vec<sift_protocol::RunVariableDefinition>,
    #[serde(default)]
    pub pre_tasks: Vec<sift_protocol::RunPreTask>,
    pub transaction_policy: sift_protocol::RunTransactionPolicy,
    pub error_policy: sift_protocol::RunErrorPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UpdateRunConfigurationRequest {
    pub expected_revision: u64,
    #[serde(flatten)]
    pub configuration: CreateRunConfigurationRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ExpectedRunConfigurationRevisionRequest {
    pub expected_revision: u64,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct StartRunRequest {
    pub expected_configuration_revision: u64,
    #[serde(default)]
    pub variables: std::collections::BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

impl std::fmt::Debug for StartRunRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StartRunRequest")
            .field(
                "expected_configuration_revision",
                &self.expected_configuration_revision,
            )
            .field("variables", &"[REDACTED]")
            .field("timeout_secs", &self.timeout_secs)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RunLogQuery {
    #[serde(default)]
    pub after: u64,
    #[serde(default = "default_run_log_limit")]
    pub limit: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateRunScheduleRequest {
    pub cron: String,
    pub timezone: String,
    pub misfire_policy: sift_protocol::ScheduleMisfirePolicy,
    pub concurrency_policy: sift_protocol::ScheduleConcurrencyPolicy,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UpdateRunScheduleRequest {
    pub expected_revision: u64,
    #[serde(flatten)]
    pub schedule: CreateRunScheduleRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ScheduleOccurrenceQuery {
    #[serde(default = "default_schedule_occurrence_limit")]
    pub limit: u32,
}

fn default_true() -> bool {
    true
}

fn default_schedule_occurrence_limit() -> u32 {
    100
}

fn default_run_log_limit() -> u32 {
    200
}

fn default_git_remote() -> String {
    "origin".into()
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
