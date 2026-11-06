use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{WorkspaceArtifactId, WorkspaceId, WorkspaceNodeId};

pub const CSV_QUARANTINE_CONTENT_TYPE: &str = "application/vnd.sift.csv-quarantine+json";

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
    Artifact {
        artifact: WorkspaceArtifact,
    },
    Import {
        result: crate::CsvImportResponse,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        quarantine_artifact: Option<WorkspaceArtifact>,
    },
    /// Validation completed without reading/writing database rows or artifacts.
    Validated {
        direction: TransferDirection,
        format_id: String,
        resume_from_row: u64,
    },
}

/// Uploaded content and candidate mappings are never included in audit records.
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PreviewTransferSourceRequest {
    pub expected_revision: u64,
    pub format_version: String,
    pub options: serde_json::Value,
    pub data: Vec<u8>,
    #[serde(default)]
    pub sheet: Option<String>,
}
impl std::fmt::Debug for PreviewTransferSourceRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreviewTransferSourceRequest")
            .field("expected_revision", &self.expected_revision)
            .field("format_version", &self.format_version)
            .field("data_bytes", &self.data.len())
            .finish_non_exhaustive()
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TransferSourceColumn {
    pub name: String,
    /// Source representation, not a promised destination SQL declaration.
    pub source_type: String,
    pub nullable: bool,
    pub selectable: bool,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TransferSampleRepresentation {
    CsvTextBeforeDestinationInference,
    TypedParquet,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TransferMappedPreview {
    Ready {
        columns: Vec<TransferSourceColumn>,
        sample_rows: Vec<crate::Row>,
        representation: TransferSampleRepresentation,
        /// True when the 20-row or byte budget excludes additional source rows.
        sample_rows_limited: bool,
    },
    Unavailable {
        reason: String,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TransferSourcePreview {
    pub recipe_id: TransferRecipeId,
    pub recipe_revision: u64,
    pub format_id: String,
    pub format_version: String,
    pub source_digest: String,
    pub source_columns: Vec<TransferSourceColumn>,
    pub source_rows: u64,
    pub mapping: TransferMappedPreview,
}
