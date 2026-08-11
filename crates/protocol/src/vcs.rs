use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{RepositoryBindingId, WorkspaceId, WorkspacePath, WorkspaceRevision};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
    pub credential_handle_present: bool,
    pub revision: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VcsStatus {
    pub binding_id: RepositoryBindingId,
    pub workspace_revision: WorkspaceRevision,
    pub head_oid: Option<String>,
    pub branch: Option<String>,
    pub upstream: Option<VcsUpstreamStatus>,
    pub entries: Vec<VcsStatusEntry>,
    pub truncated: bool,
    pub observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VcsDiff {
    pub binding_id: RepositoryBindingId,
    pub side: VcsDiffSide,
    pub files: Vec<VcsDiffFile>,
    pub truncated: bool,
}
