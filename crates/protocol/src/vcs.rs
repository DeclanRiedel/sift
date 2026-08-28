use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{RepositoryBindingId, WorkspaceId, WorkspacePath, WorkspaceRevision};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VcsFileState {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Unmerged,
    Untracked,
    Ignored,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VcsStageState {
    Unstaged,
    Staged,
    PartiallyStaged,
    Conflict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VcsStatusEntry {
    pub path: WorkspacePath,
    pub previous_path: Option<WorkspacePath>,
    pub state: VcsFileState,
    pub stage: VcsStageState,
    #[serde(default)]
    pub conflict: Option<VcsConflictKind>,
    #[serde(default)]
    pub pending: Option<VcsPendingOperation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VcsConflictKind {
    BothAdded,
    BothDeleted,
    BothModified,
    AddedByUs,
    AddedByThem,
    DeletedByUs,
    DeletedByThem,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VcsPendingOperation {
    Stage,
    Unstage,
    Discard,
    Revert,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VcsUpstreamStatus {
    pub remote: String,
    pub branch: String,
    pub ahead: u32,
    pub behind: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RepositoryBinding {
    pub id: RepositoryBindingId,
    pub workspace_id: WorkspaceId,
    pub projection_id: crate::ProjectionBindingId,
    pub adapter_id: String,
    pub repository_identity: String,
    pub adapter_generation: String,
    pub executable_version: String,
    pub network_enabled: bool,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub credential_handle_present: bool,
    pub revision: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VcsStatus {
    pub binding_id: RepositoryBindingId,
    pub workspace_revision: WorkspaceRevision,
    pub binding_revision: u64,
    pub head_oid: Option<String>,
    pub branch: Option<String>,
    pub upstream: Option<VcsUpstreamStatus>,
    #[serde(default)]
    pub operation: Option<VcsRepositoryOperationState>,
    pub entries: Vec<VcsStatusEntry>,
    pub truncated: bool,
    pub observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VcsRepositoryOperationKind {
    Merge,
    Rebase,
    CherryPick,
    Revert,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VcsRepositoryOperationState {
    pub kind: VcsRepositoryOperationKind,
    pub current_commit: Option<String>,
    pub step: Option<u32>,
    pub total_steps: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VcsConflictResolution {
    Ours,
    Theirs,
    Both,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VcsConflictRegion {
    pub id: String,
    pub base: Option<String>,
    pub ours: Option<String>,
    pub theirs: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VcsConflictFile {
    pub path: WorkspacePath,
    pub kind: VcsConflictKind,
    pub binary: bool,
    pub regions: Vec<VcsConflictRegion>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VcsDiffSide {
    HeadToIndex,
    IndexToWorktree,
    HeadToWorktree,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VcsDiffFile {
    pub path: WorkspacePath,
    pub previous_path: Option<WorkspacePath>,
    pub state: VcsFileState,
    pub old_digest: Option<String>,
    pub new_digest: Option<String>,
    pub binary: bool,
    pub additions: u32,
    pub deletions: u32,
    /// Textual hunks are populated for a path-scoped diff request. Project
    /// summaries keep this empty and load the selected file lazily.
    #[serde(default)]
    pub hunks: Vec<VcsDiffHunk>,
    #[serde(default)]
    pub content_truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VcsDiffLineKind {
    Context,
    Addition,
    Deletion,
    NoNewline,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VcsDiffLine {
    pub kind: VcsDiffLineKind,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VcsDiffHunk {
    /// Stable within one diff side and file content; suitable for selection and
    /// refresh reconciliation, never accepted as authorization by itself.
    pub id: String,
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    pub header: String,
    pub lines: Vec<VcsDiffLine>,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VcsDiff {
    pub binding_id: RepositoryBindingId,
    pub side: VcsDiffSide,
    #[serde(default)]
    pub base_revision: Option<String>,
    #[serde(default)]
    pub target_revision: Option<String>,
    pub files: Vec<VcsDiffFile>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VcsBranch {
    pub name: String,
    pub head: Option<String>,
    pub current: bool,
    pub remote: bool,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VcsRemote {
    pub name: String,
    pub fetch_url: String,
    pub push_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VcsRefChange {
    pub name: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VcsCommitSummary {
    pub oid: String,
    pub parents: Vec<String>,
    pub author_name: String,
    pub author_email: String,
    pub authored_at: DateTime<Utc>,
    pub refs: Vec<String>,
    pub subject: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VcsHistoryPage {
    pub commits: Vec<VcsCommitSummary>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VcsCommitFile {
    pub path: WorkspacePath,
    pub previous_path: Option<WorkspacePath>,
    pub state: VcsFileState,
    pub additions: Option<u32>,
    pub deletions: Option<u32>,
    pub binary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VcsCommitDetail {
    pub commit: VcsCommitSummary,
    pub message: String,
    pub files: Vec<VcsCommitFile>,
    pub files_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VcsHistoricalFile {
    pub commit: String,
    pub path: WorkspacePath,
    pub text: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VcsCommitResult {
    pub binding_id: RepositoryBindingId,
    pub checkpoint_id: crate::WorkspaceCheckpointId,
    pub workspace_revision: WorkspaceRevision,
    pub commit: String,
    pub branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VcsHeadMutationResult {
    pub binding_id: RepositoryBindingId,
    pub checkpoint_id: crate::WorkspaceCheckpointId,
    pub workspace_revision: WorkspaceRevision,
    pub previous_head: String,
    pub head: Option<String>,
    pub branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VcsWorktreeMutationResult {
    pub binding_id: RepositoryBindingId,
    pub checkpoint_id: crate::WorkspaceCheckpointId,
    pub workspace_revision: WorkspaceRevision,
    pub path: WorkspacePath,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VcsRemoteResult {
    pub binding_id: RepositoryBindingId,
    pub operation: String,
    pub head: Option<String>,
    pub updated_refs: Vec<String>,
    #[serde(default)]
    pub ref_changes: Vec<VcsRefChange>,
}
