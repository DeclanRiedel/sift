use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{WorkspaceArtifactId, WorkspaceId, WorkspaceNodeId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct TransferRecipeId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TransferDirection {
    Import,
    Export,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TransferEndpoint {
    Query,
    Table,
    Upload,
    WorkspaceNode { node_id: WorkspaceNodeId },
    Artifact,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TransferRecipe {
    pub id: TransferRecipeId,
    pub workspace_id: WorkspaceId,
    pub name: String,
    pub direction: TransferDirection,
    pub source: TransferEndpoint,
    pub sink: TransferEndpoint,
    pub format_id: String,
    pub format_version: String,
    pub options: serde_json::Value,
    pub revision: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceArtifact {
    pub id: WorkspaceArtifactId,
    pub workspace_id: WorkspaceId,
    pub content_type: String,
    pub digest: String,
    pub byte_len: u64,
    pub expires_at: Option<DateTime<Utc>>,
    pub pinned: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TransferExecutionResult {
    Artifact { artifact: WorkspaceArtifact },
    Import { result: crate::CsvImportResponse },
}
