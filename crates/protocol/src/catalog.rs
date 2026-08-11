//! Revisioned catalog graph wire contract (ADR-033).
//!
//! Introspection, normalization, caching, and authorization live outside the
//! protocol crate. These types are pure serde data shared by every client.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    ColumnMetadata, ConstraintInfo, IndexInfo, MigrationOptions, Nullability, ObjectKind,
    ProviderRef, TriggerInfo, TypeRef,
};

#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(transparent)]
pub struct CatalogObjectId(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct CatalogRevision(pub u64);

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogGraphOptions {
    #[serde(default)]
    pub kinds: Option<Vec<CatalogNodeKind>>,
    #[serde(default)]
    pub schemas: Option<Vec<String>>,
    #[serde(default)]
    pub include_definitions: bool,
    #[serde(default)]
    pub max_nodes: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogGraphRequest {
    #[serde(default)]
    pub options: CatalogGraphOptions,
    /// Force a fresh provider build and synchronously invalidate every schema-
    /// derived cache for this connection specification before reading.
    #[serde(default)]
    pub refresh: bool,
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
pub enum CatalogNodeKind {
    Catalog,
    Schema,
    Table,
    View,
    MaterializedView,
    ForeignTable,
    PartitionedTable,
    TableValuedFunction,
    ScalarFunction,
    Procedure,
    Synonym,
    Sequence,
    Trigger,
    Type,
    Extension,
    Column,
    Index,
    Constraint,
}

impl From<ObjectKind> for CatalogNodeKind {
    fn from(value: ObjectKind) -> Self {
        match value {
            ObjectKind::Table => Self::Table,
            ObjectKind::View => Self::View,
            ObjectKind::MaterializedView => Self::MaterializedView,
            ObjectKind::ForeignTable => Self::ForeignTable,
            ObjectKind::PartitionedTable => Self::PartitionedTable,
            ObjectKind::TableValuedFunction => Self::TableValuedFunction,
            ObjectKind::ScalarFunction => Self::ScalarFunction,
            ObjectKind::Procedure => Self::Procedure,
            ObjectKind::Synonym => Self::Synonym,
            ObjectKind::Sequence => Self::Sequence,
            ObjectKind::Trigger => Self::Trigger,
            ObjectKind::Type => Self::Type,
            ObjectKind::Extension => Self::Extension,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CatalogNodeDetails {
    None,
    Object {
        #[serde(default)]
        routine_args: Option<Vec<String>>,
    },
    Column {
        column: ColumnMetadata,
    },
    Index {
        index: IndexInfo,
    },
    Constraint {
        constraint: ConstraintInfo,
    },
    Trigger {
        trigger: TriggerInfo,
    },
    Routine {
        #[serde(default)]
        arguments: Vec<String>,
        #[serde(default)]
        return_type: Option<TypeRef>,
    },
    Type {
        #[serde(default)]
        base_type: Option<TypeRef>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CatalogCompleteness {
    Complete,
    Partial,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogNode {
    pub id: CatalogObjectId,
    #[serde(default)]
    pub native_id: Option<String>,
    pub kind: CatalogNodeKind,
    pub name: String,
    pub qualified_name: String,
    #[serde(default)]
    pub parent_id: Option<CatalogObjectId>,
    #[serde(default)]
    pub ordinal: Option<u32>,
    #[serde(default)]
    pub definition_digest: Option<String>,
    pub completeness: CatalogCompleteness,
    pub details: CatalogNodeDetails,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, serde_json::Value>,
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
pub enum CatalogEdgeKind {
    Contains,
    DependsOn,
    ForeignKey,
    UsesType,
    ReadsFrom,
    WritesTo,
    Calls,
    TriggerOn,
    OwnsSequence,
    Indexes,
    Constrains,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CatalogEdgeCertainty {
    CatalogProven,
    Parsed,
    Unresolved,
    Inaccessible,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogColumnPair {
    pub from: CatalogObjectId,
    pub to: CatalogObjectId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogEdge {
    pub from: CatalogObjectId,
    #[serde(default)]
    pub to: Option<CatalogObjectId>,
    pub kind: CatalogEdgeKind,
    pub certainty: CatalogEdgeCertainty,
    #[serde(default)]
    pub referenced_path: Option<String>,
    #[serde(default)]
    pub column_pairs: Vec<CatalogColumnPair>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CatalogCoverageState {
    Complete,
    Partial,
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogCoverageFailure {
    pub stage: String,
    #[serde(default)]
    pub schema: Option<String>,
    pub code: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogCoverage {
    pub state: CatalogCoverageState,
    #[serde(default)]
    pub requested_kinds: Vec<CatalogNodeKind>,
    #[serde(default)]
    pub covered_schemas: Vec<String>,
    #[serde(default)]
    pub omitted_schemas: Vec<String>,
    #[serde(default)]
    pub truncated_at_nodes: Option<u32>,
    #[serde(default)]
    pub failures: Vec<CatalogCoverageFailure>,
}

impl CatalogCoverage {
    pub fn complete() -> Self {
        Self {
            state: CatalogCoverageState::Complete,
            requested_kinds: Vec::new(),
            covered_schemas: Vec::new(),
            omitted_schemas: Vec::new(),
            truncated_at_nodes: None,
            failures: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogGraphData {
    pub coverage: CatalogCoverage,
    #[serde(default)]
    pub nodes: Vec<CatalogNode>,
    #[serde(default)]
    pub edges: Vec<CatalogEdge>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogGraph {
    pub revision: CatalogRevision,
    pub content_digest: String,
    pub invalidation_epoch: u64,
    pub captured_at: chrono::DateTime<chrono::Utc>,
    pub provider: ProviderRef,
    pub database_identity: String,
    pub data: CatalogGraphData,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogDiagramRequest {
    pub expected_revision: CatalogRevision,
    #[serde(default)]
    pub schemas: Vec<String>,
    #[serde(default)]
    pub object_ids: Vec<CatalogObjectId>,
    #[serde(default)]
    pub edge_kinds: Vec<CatalogEdgeKind>,
    #[serde(default)]
    pub neighborhood_depth: u8,
    #[serde(default)]
    pub include_columns: bool,
    #[serde(default)]
    pub include_routines: bool,
    #[serde(default)]
    pub max_nodes: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogDiagram {
    pub catalog_revision: CatalogRevision,
    pub catalog_digest: String,
    pub nodes: Vec<CatalogNode>,
    pub edges: Vec<CatalogEdge>,
    pub omitted_nodes: u32,
    pub omitted_edges: u32,
    pub inaccessible_boundaries: u32,
    pub partial: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CatalogDiagramMutation {
    RenameObject {
        object_id: CatalogObjectId,
        new_name: String,
    },
    AddForeignKey {
        table_id: CatalogObjectId,
        name: String,
        columns: Vec<CatalogObjectId>,
        referenced_table_id: CatalogObjectId,
        referenced_columns: Vec<CatalogObjectId>,
    },
    DropForeignKey {
        constraint_id: CatalogObjectId,
    },
    ChangeColumn {
        column_id: CatalogObjectId,
        #[serde(default)]
        type_ref: Option<TypeRef>,
        #[serde(default)]
        nullability: Option<Nullability>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PreviewCatalogDiagramMutationRequest {
    pub expected_catalog_revision: CatalogRevision,
    pub mutation: CatalogDiagramMutation,
    #[serde(default)]
    pub options: MigrationOptions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct CatalogSnapshotId(pub Uuid);

impl std::fmt::Display for CatalogSnapshotId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateCatalogSnapshotRequest {
    pub expected_catalog_revision: CatalogRevision,
    #[serde(default)]
    pub options: CatalogGraphOptions,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub accept_partial: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogSnapshot {
    pub id: CatalogSnapshotId,
    pub tenant_id: i64,
    #[serde(default)]
    pub connection_profile_id: Option<i64>,
    pub creator_principal_id: i64,
    #[serde(default)]
    pub description: Option<String>,
    pub graph: CatalogGraph,
    pub format_version: u32,
    pub revision: u64,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogSnapshotSummary {
    pub id: CatalogSnapshotId,
    pub tenant_id: i64,
    #[serde(default)]
    pub connection_profile_id: Option<i64>,
    pub creator_principal_id: i64,
    #[serde(default)]
    pub description: Option<String>,
    pub catalog_revision: CatalogRevision,
    pub content_digest: String,
    pub coverage: CatalogCoverage,
    pub retained_bytes: u64,
    pub format_version: u32,
    pub revision: u64,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeleteCatalogSnapshotRequest {
    pub expected_revision: u64,
}
