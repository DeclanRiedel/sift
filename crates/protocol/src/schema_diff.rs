//! Portable normalized schema-diff contract (ADR-033).

use serde::{Deserialize, Serialize};

use crate::{
    CatalogCoverage, CatalogGraphOptions, CatalogNode, CatalogObjectId, CatalogRevision,
    CatalogSnapshotId,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CatalogSourceRef {
    Live {
        expected_revision: CatalogRevision,
        #[serde(default)]
        options: CatalogGraphOptions,
    },
    Snapshot {
        snapshot_id: CatalogSnapshotId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenameMapping {
    pub from: CatalogObjectId,
    pub to: CatalogObjectId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SchemaDiffRequest {
    pub from: CatalogSourceRef,
    pub to: CatalogSourceRef,
    #[serde(default)]
    pub accepted_renames: Vec<RenameMapping>,
    #[serde(default)]
    pub max_changes: Option<u32>,
}

#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(transparent)]
pub struct SchemaChangeId(pub String);

impl std::fmt::Display for SchemaChangeId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SchemaChangeKind {
    Create,
    Drop,
    Rename,
    Move,
    Alter,
    Unknown,
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SchemaChangeRisk {
    Safe,
    Locking,
    DataRewrite,
    DataLoss,
    Privilege,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SchemaChangeReversibility {
    Exact,
    Lossy,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SchemaFieldChange {
    pub field: String,
    #[serde(default)]
    pub before: Option<serde_json::Value>,
    #[serde(default)]
    pub after: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SchemaChange {
    pub id: SchemaChangeId,
    pub kind: SchemaChangeKind,
    #[serde(default)]
    pub object_before: Option<CatalogNode>,
    #[serde(default)]
    pub object_after: Option<CatalogNode>,
    #[serde(default)]
    pub field_changes: Vec<SchemaFieldChange>,
    #[serde(default)]
    pub prerequisites: Vec<SchemaChangeId>,
    #[serde(default)]
    pub dependency_group: Option<String>,
    pub risk: SchemaChangeRisk,
    pub reversibility: SchemaChangeReversibility,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenameSuggestion {
    pub from: CatalogObjectId,
    pub to: CatalogObjectId,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SchemaDiffCoverage {
    pub from: CatalogCoverage,
    pub to: CatalogCoverage,
    pub definitive_drops: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SchemaDiff {
    pub from: CatalogSourceRef,
    pub to: CatalogSourceRef,
    pub digest: String,
    pub coverage: SchemaDiffCoverage,
    #[serde(default)]
    pub changes: Vec<SchemaChange>,
    #[serde(default)]
    pub rename_suggestions: Vec<RenameSuggestion>,
    #[serde(default)]
    pub warnings: Vec<String>,
    pub partial: bool,
}
