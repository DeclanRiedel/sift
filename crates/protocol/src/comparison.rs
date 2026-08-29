//! Bounded table and retained-result comparison wire contract.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    CatalogObjectId, CatalogRevision, ColumnMetadata, ConnectionId, CursorId, RoomResultId, Value,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct ComparisonId(pub Uuid);

impl std::fmt::Display for ComparisonId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CompareSource {
    Table {
        connection: ConnectionId,
        catalog_revision: CatalogRevision,
        object_id: CatalogObjectId,
        #[serde(default)]
        filter: Option<ComparePredicate>,
    },
    QueryResult {
        cursor_id: CursorId,
        result_set: u32,
        schema_digest: String,
    },
    RoomResult {
        room_id: i64,
        result_id: RoomResultId,
        result_set: u32,
        schema_digest: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ComparePredicateOperator {
    Eq,
    NotEq,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
}

/// A deliberately small predicate AST. Values become driver bind parameters;
/// column names are resolved against the exact catalog object before rendering.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ComparePredicate {
    Compare {
        column: String,
        operator: ComparePredicateOperator,
        value: Value,
    },
    IsNull {
        column: String,
        #[serde(default)]
        negated: bool,
    },
    And {
        predicates: Vec<ComparePredicate>,
    },
    Or {
        predicates: Vec<ComparePredicate>,
    },
    Not {
        predicate: Box<ComparePredicate>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompareColumnPair {
    pub left: String,
    pub right: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CompareKey {
    Explicit {
        columns: Vec<CompareColumnPair>,
    },
    #[default]
    Infer,
    RowOrdinal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UnicodeNormalization {
    Nfc,
    Nfkc,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ColumnTolerance {
    pub column: String,
    #[serde(default)]
    pub numeric_absolute: Option<f64>,
    #[serde(default)]
    pub numeric_relative: Option<f64>,
    #[serde(default)]
    pub timestamp_microseconds: Option<u64>,
    #[serde(default)]
    pub unicode_normalization: Option<UnicodeNormalization>,
    #[serde(default)]
    pub case_fold: bool,
    #[serde(default)]
    pub trim_outer_whitespace: bool,
    #[serde(default)]
    pub binary_digest: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StartComparisonRequest {
    pub left: CompareSource,
    pub right: CompareSource,
    #[serde(default)]
    pub column_mappings: Vec<CompareColumnPair>,
    #[serde(default)]
    pub key: CompareKey,
    #[serde(default)]
    pub tolerances: Vec<ColumnTolerance>,
    #[serde(default)]
    pub max_source_rows: Option<u32>,
    #[serde(default)]
    pub max_diff_rows: Option<u32>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CompareColumnStatus {
    Mapped,
    MissingLeft,
    MissingRight,
    Incompatible,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompareColumn {
    #[serde(default)]
    pub left: Option<ColumnMetadata>,
    #[serde(default)]
    pub right: Option<ColumnMetadata>,
    pub status: CompareColumnStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResolvedCompareKey {
    pub columns: Vec<CompareColumnPair>,
    #[serde(default)]
    pub inferred_constraint: Option<CatalogObjectId>,
    pub row_ordinal: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonStatus {
    Running,
    Complete,
    Truncated,
    Canceled,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RowDiffKind {
    Added,
    Removed,
    Changed,
    Incomparable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CellComparisonStatus {
    Equal,
    TolerantEqual,
    Unequal,
    Incomparable,
    ConversionFailed,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CellDiff {
    pub column: CompareColumnPair,
    pub status: CellComparisonStatus,
    #[serde(default)]
    pub left: Option<Value>,
    #[serde(default)]
    pub right: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RowDiff {
    pub key: Vec<Value>,
    pub occurrence: u32,
    pub kind: RowDiffKind,
    pub duplicate_key: bool,
    #[serde(default)]
    pub cells: Vec<CellDiff>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComparisonSummary {
    pub comparison_id: ComparisonId,
    pub status: ComparisonStatus,
    pub result_digest: String,
    pub left_rows: u64,
    pub right_rows: u64,
    pub equal_rows: u64,
    pub changed_rows: u64,
    pub added_rows: u64,
    pub removed_rows: u64,
    pub incomparable_rows: u64,
    pub duplicate_key_groups: u64,
    pub retained_diff_rows: u32,
    pub columns: Vec<CompareColumn>,
    pub key: ResolvedCompareKey,
    pub tolerances: Vec<ColumnTolerance>,
    pub patch_eligible: bool,
    #[serde(default)]
    pub patch_refusal_reasons: Vec<String>,
    #[serde(default)]
    pub failure_code: Option<String>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComparisonPageRequest {
    #[serde(default)]
    pub after: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComparisonPage {
    pub comparison_id: ComparisonId,
    pub status: ComparisonStatus,
    pub rows: Vec<RowDiff>,
    #[serde(default)]
    pub next: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CancelComparisonResponse {
    pub comparison_id: ComparisonId,
    pub status: ComparisonStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PrepareComparisonPatchRequest {
    pub expected_catalog_revision: CatalogRevision,
    #[serde(default)]
    pub max_statements: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComparisonPatchPreparation {
    pub comparison_id: ComparisonId,
    pub eligible: bool,
    #[serde(default)]
    pub refusal_reasons: Vec<String>,
    /// Patch application remains on the existing optimistic edit path.
    #[serde(default)]
    pub edit_plan: Option<crate::EditPlan>,
    /// Exact bounded optimistic edit set accepted by the existing edit-apply
    /// endpoint. Present iff `eligible` is true.
    #[serde(default)]
    pub edit_set: Option<crate::EditSet>,
}
