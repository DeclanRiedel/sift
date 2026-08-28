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

/// Editable desired-state document for a server launched from an instance root.
/// Host filesystem paths are intentionally not part of the public response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InstanceConfigurationDocument {
    pub manifest: String,
    pub manifest_id: String,
    pub name: String,
    /// Digest of the exact source bytes, used for optimistic concurrency.
    pub source_revision: String,
    pub configuration_digest: String,
    pub lock_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UpdateInstanceConfigurationRequest {
    pub manifest: String,
    pub expected_source_revision: String,
}

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

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct CloneWorkspaceRepositoryRequest {
    pub root_handle: String,
    pub url: String,
    pub username: RedactedString,
    pub password: RedactedString,
}

impl std::fmt::Debug for CloneWorkspaceRepositoryRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CloneWorkspaceRepositoryRequest")
            .field("root_handle", &self.root_handle)
            .field("url", &"[REDACTED]")
            .field("username", &"[REDACTED]")
            .field("password", &"[REDACTED]")
            .finish()
    }
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
pub struct VcsHunkRequest {
    pub expected_revision: u64,
    pub side: VcsDiffSide,
    pub path: WorkspacePath,
    pub hunk_id: String,
    /// Zero-based authoritative hunk-line indices. `None` applies the complete
    /// hunk; a non-empty list applies only selected addition/deletion lines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_indices: Option<Vec<u32>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VcsDiscardRequest {
    pub expected_revision: u64,
    pub path: WorkspacePath,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VcsRevertHunkRequest {
    pub expected_revision: u64,
    pub side: VcsDiffSide,
    pub path: WorkspacePath,
    pub hunk_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VcsCommitRequest {
    pub expected_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_head: Option<String>,
    pub message: String,
    pub author_name: String,
    pub author_email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VcsUncommitRequest {
    pub expected_revision: u64,
    pub expected_head: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VcsCreateBranchRequest {
    pub expected_revision: u64,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_id: Option<sift_protocol::WorkspaceCheckpointId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VcsSwitchBranchRequest {
    pub expected_revision: u64,
    pub target: String,
    #[serde(default)]
    pub detached: bool,
    #[serde(default)]
    pub checkpoint_changes: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VcsRenameBranchRequest {
    pub expected_revision: u64,
    pub old: String,
    pub new: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VcsDeleteBranchRequest {
    pub expected_revision: u64,
    pub name: String,
    #[serde(default)]
    pub force: bool,
    #[serde(default)]
    pub confirm_unmerged: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VcsSetUpstreamRequest {
    pub expected_revision: u64,
    pub branch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VcsHistoryQuery {
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default = "default_vcs_history_limit")]
    pub limit: u32,
    #[serde(default)]
    pub query: Option<String>,
}

fn default_vcs_history_limit() -> u32 {
    50
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VcsHistoricalFileQuery {
    pub path: WorkspacePath,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VcsCompareQuery {
    pub base: String,
    pub target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VcsRestoreHistoricalFileRequest {
    pub expected_revision: u64,
    pub commit: String,
    pub path: WorkspacePath,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VcsRevertCommitRequest {
    pub expected_revision: u64,
    pub commit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VcsConflictQuery {
    pub path: WorkspacePath,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VcsBeginConflictResolutionRequest {
    pub expected_revision: u64,
    pub path: WorkspacePath,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VcsResolveConflictRequest {
    pub expected_revision: u64,
    pub path: WorkspacePath,
    pub region_id: String,
    pub resolution: sift_protocol::VcsConflictResolution,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VcsMarkConflictResolvedRequest {
    pub expected_revision: u64,
    pub path: WorkspacePath,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VcsRepositoryOperationRequest {
    pub expected_revision: u64,
    pub kind: sift_protocol::VcsRepositoryOperationKind,
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
pub struct VcsRemoteMutationRequest {
    pub expected_revision: u64,
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VcsRemoteRenameRequest {
    pub expected_revision: u64,
    pub old_name: String,
    pub new_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VcsRemoteDeleteRequest {
    pub expected_revision: u64,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VcsCredentialTestRequest {
    pub expected_revision: u64,
    #[serde(default = "default_git_remote")]
    pub remote: String,
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateTransferRecipeRequest {
    pub name: String,
    pub direction: sift_protocol::TransferDirection,
    pub source: sift_protocol::TransferEndpoint,
    pub sink: sift_protocol::TransferEndpoint,
    pub format_id: String,
    #[serde(default = "default_format_version")]
    pub format_version: String,
    #[serde(default = "default_options_object")]
    pub options: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UpdateTransferRecipeRequest {
    pub expected_revision: u64,
    #[serde(flatten)]
    pub recipe: CreateTransferRecipeRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ExpectedTransferRecipeRevisionRequest {
    pub expected_revision: u64,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct ExecuteTransferRecipeRequest {
    pub session_id: sift_protocol::SessionId,
    pub connection_id: sift_protocol::ConnectionId,
    #[serde(default)]
    pub sql: Option<String>,
    #[serde(default)]
    pub params: Vec<sift_protocol::Value>,
    #[serde(default)]
    pub data: Option<Vec<u8>>,
    #[serde(default)]
    pub table: Option<sift_protocol::ObjectPath>,
    #[serde(default)]
    pub sheet: Option<String>,
    #[serde(default)]
    pub create_table: bool,
    #[serde(default)]
    pub conflict_policy: Option<sift_protocol::CsvConflictPolicy>,
}

impl std::fmt::Debug for ExecuteTransferRecipeRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExecuteTransferRecipeRequest")
            .field("session_id", &self.session_id)
            .field("connection_id", &self.connection_id)
            .field("sql", &self.sql.as_ref().map(|_| "[REDACTED]"))
            .field("data", &self.data.as_ref().map(Vec::len))
            .field("table", &self.table)
            .field("sheet", &self.sheet)
            .field("create_table", &self.create_table)
            .field("conflict_policy", &self.conflict_policy)
            .finish()
    }
}

fn default_format_version() -> String {
    "1".into()
}
fn default_options_object() -> serde_json::Value {
    serde_json::json!({})
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
