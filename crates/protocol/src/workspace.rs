use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

macro_rules! integer_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
        pub struct $name(pub i64);
    };
}

integer_id!(WorkspaceId);
integer_id!(WorkspaceNodeId);
integer_id!(WorkspaceCheckpointId);
integer_id!(ProjectionBindingId);
integer_id!(RepositoryBindingId);
integer_id!(DdlSourceId);
integer_id!(WorkspaceArtifactId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Default)]
pub struct WorkspaceRevision(pub u64);

/// A slash-separated path relative to a workspace root.
///
/// Deserialization deliberately remains a pure serde operation. Servers must
/// call [`WorkspacePath::is_valid`] at every untrusted boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct WorkspacePath(pub String);

impl WorkspacePath {
    pub const MAX_BYTES: usize = 4096;

    pub fn new(path: impl Into<String>) -> Result<Self, &'static str> {
        let path = Self(path.into());
        if path.is_valid() {
            Ok(path)
        } else {
            Err("workspace path must be a normalized, non-empty relative path")
        }
    }

    pub fn is_valid(&self) -> bool {
        let path = self.0.as_str();
        !path.is_empty()
            && path.len() <= Self::MAX_BYTES
            && !path.starts_with('/')
            && !path.ends_with('/')
            && !path.contains('\\')
            && !path.contains('\0')
            && path
                .split('/')
                .all(|component| !component.is_empty() && !matches!(component, "." | ".."))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceNodeKind {
    Folder,
    SqlDocument,
    Artifact,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceCapabilities {
    pub virtual_tree: bool,
    pub filesystem_projection: bool,
    pub git: bool,
    pub git_network: bool,
    pub scheduling: bool,
    pub transfer_recipes: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub room_id: i64,
    pub name: String,
    pub revision: WorkspaceRevision,
    pub capabilities: WorkspaceCapabilities,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceNode {
    pub id: WorkspaceNodeId,
    pub workspace_id: WorkspaceId,
    pub parent_id: Option<WorkspaceNodeId>,
    pub path: WorkspacePath,
    pub kind: WorkspaceNodeKind,
    pub document_id: Option<i64>,
    pub artifact_id: Option<WorkspaceArtifactId>,
    pub revision: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceCheckpointReason {
    Automatic,
    Named,
    BeforeReconcile,
    BeforeRun,
    BeforeVcs,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceCheckpoint {
    pub id: WorkspaceCheckpointId,
    pub workspace_id: WorkspaceId,
    pub workspace_revision: WorkspaceRevision,
    pub reason: WorkspaceCheckpointReason,
    pub name: Option<String>,
    pub created_by: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionMode {
    ReadOnly,
    ReadWrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionHealth {
    Ready,
    Disabled,
    Missing,
    ReadOnly,
    Conflicted,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProjectionBinding {
    pub id: ProjectionBindingId,
    pub workspace_id: WorkspaceId,
    pub adapter_id: String,
    pub mode: ProjectionMode,
    pub last_workspace_revision: Option<WorkspaceRevision>,
    pub adapter_generation: String,
    pub health: ProjectionHealth,
    pub revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReconcileState {
    Unchanged,
    WorkspaceOnly,
    ProjectionOnly,
    BothChanged,
    Renamed,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReconcileEntry {
    pub node_id: Option<WorkspaceNodeId>,
    pub path: WorkspacePath,
    pub previous_path: Option<WorkspacePath>,
    pub state: ReconcileState,
    pub workspace_digest: Option<String>,
    pub projection_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReconcilePlan {
    pub binding_id: ProjectionBindingId,
    pub workspace_revision: WorkspaceRevision,
    pub entries: Vec<ReconcileEntry>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReconcileResolution {
    ImportProjection,
    MaterializeWorkspace,
    KeepBoth,
    Abandon,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DdlSourceCoverage {
    Complete,
    Partial,
    Stale,
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DdlSource {
    pub id: DdlSourceId,
    pub workspace_id: WorkspaceId,
    pub name: String,
    pub dialect_id: String,
    pub roots: Vec<WorkspaceNodeId>,
    pub model_revision: u64,
    pub coverage: DdlSourceCoverage,
    pub diagnostic_count: u32,
    pub revision: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_paths_are_normalized_relative_paths() {
        for valid in ["query.sql", "ddl/tables/users.sql", "space ok/a.sql"] {
            assert!(WorkspacePath::new(valid).is_ok(), "{valid}");
        }
        for invalid in [
            "",
            "/query.sql",
            "query.sql/",
            "a//b",
            ".",
            "..",
            "a/../b",
            "a\\b",
        ] {
            assert!(WorkspacePath::new(invalid).is_err(), "{invalid}");
        }
    }
}
