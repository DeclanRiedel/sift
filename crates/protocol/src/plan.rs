//! Execution plans.
//!
//! A query's execution plan captured as an engine-neutral, typed [`PlanNode`]
//! tree. Postgres `EXPLAIN (FORMAT JSON)` and SQL Server showplan XML both
//! normalize into the same tree — a small typed core plus an `extra` map for engine-specific
//! attributes, plus the untouched raw plan on the response.
//!
//! Pure serde: capture + parsing live in the server (`crates/server/src/plan.rs`).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use uuid::Uuid;

use crate::{
    CatalogRevision, ConnectionId, DriverWarning, Engine, ProviderRef, SemanticDocumentId, Value,
};

/// One node in an execution plan. The typed fields are the common core both
/// engines expose; everything else lives in `extra`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PlanNode {
    /// Operator name, e.g. `Seq Scan` / `Hash Join` (PG) or
    /// `Clustered Index Scan` (SQL Server).
    pub op: String,
    /// Target relation / index / object, when the node has one.
    #[serde(default)]
    pub relation: Option<String>,
    /// Estimated output rows.
    #[serde(default)]
    pub est_rows: Option<f64>,
    /// Estimated cost (PG total cost / SQL Server estimated subtree cost).
    /// Engine-relative — not comparable across engines.
    #[serde(default)]
    pub est_cost: Option<f64>,
    /// Actual output rows (ANALYZE only).
    #[serde(default)]
    pub actual_rows: Option<f64>,
    /// Actual total time in milliseconds (ANALYZE only).
    #[serde(default)]
    pub actual_ms: Option<f64>,
    /// Engine-specific attributes carried through verbatim.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub children: Vec<PlanNode>,
}

impl PlanNode {
    pub fn new(op: impl Into<String>) -> Self {
        Self {
            op: op.into(),
            relation: None,
            est_rows: None,
            est_cost: None,
            actual_rows: None,
            actual_ms: None,
            extra: BTreeMap::new(),
            children: Vec::new(),
        }
    }
}

/// Body of `POST /v1/sessions/:id/connections/:conn_id/explain`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExplainRequest {
    pub connection: ConnectionId,
    pub sql: String,
    /// Bind parameters, threaded through the normal execute path.
    #[serde(default)]
    pub params: Vec<Value>,
    /// When true, actually run the statement to collect runtime counters. For
    /// non-SELECT statements the server runs it inside a rolled-back
    /// transaction so side effects are discarded.
    #[serde(default)]
    pub analyze: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExplainResponse {
    pub engine: Engine,
    pub analyzed: bool,
    pub root: PlanNode,
    /// Untouched engine plan (JSON for Postgres, XML for SQL Server).
    pub raw: String,
    #[serde(default)]
    pub warnings: Vec<DriverWarning>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct PlanCaptureId(pub Uuid);

impl std::fmt::Display for PlanCaptureId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CaptureSemanticPlanRequest {
    pub document_id: SemanticDocumentId,
    pub revision: u64,
    pub statement_id: String,
    pub catalog_revision: CatalogRevision,
    #[serde(default)]
    pub analyze: bool,
    #[serde(default)]
    pub params: Vec<Value>,
    #[serde(default)]
    pub include_raw_response: bool,
    #[serde(default)]
    pub source: Option<crate::VersionedExecutionContext>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanCapture {
    pub id: PlanCaptureId,
    pub tenant_id: i64,
    pub connection_profile_id: i64,
    pub creator_principal_id: i64,
    pub provider: ProviderRef,
    pub server_version: String,
    pub engine: Engine,
    pub source_digest: String,
    pub document_revision: u64,
    pub statement_id: String,
    pub statement_fingerprint: String,
    pub catalog_revision: CatalogRevision,
    pub analyzed: bool,
    pub captured_at: chrono::DateTime<chrono::Utc>,
    pub duration_ms: u64,
    pub root: PlanNode,
    #[serde(default)]
    pub warnings: Vec<DriverWarning>,
    pub complete: bool,
    pub revision: u64,
    #[serde(default)]
    pub raw_response: Option<String>,
    #[serde(default)]
    pub source: Option<crate::VersionedExecutionContext>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanCaptureSummary {
    pub id: PlanCaptureId,
    pub tenant_id: i64,
    pub connection_profile_id: i64,
    pub creator_principal_id: i64,
    pub provider: ProviderRef,
    pub server_version: String,
    pub engine: Engine,
    pub source_digest: String,
    pub document_revision: u64,
    pub statement_id: String,
    pub statement_fingerprint: String,
    pub catalog_revision: CatalogRevision,
    pub analyzed: bool,
    pub captured_at: chrono::DateTime<chrono::Utc>,
    pub duration_ms: u64,
    pub root_operator: String,
    pub complete: bool,
    pub revision: u64,
    #[serde(default)]
    pub source: Option<crate::VersionedExecutionContext>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListPlanCapturesRequest {
    #[serde(default)]
    pub source_digest: Option<String>,
    #[serde(default)]
    pub cursor: Option<PlanCaptureId>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComparePlanCapturesRequest {
    pub left: PlanCaptureId,
    pub right: PlanCaptureId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanChangeKind {
    Added,
    Removed,
    Modified,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanNodeChange {
    pub path: Vec<u32>,
    pub kind: PlanChangeKind,
    #[serde(default)]
    pub left_operator: Option<String>,
    #[serde(default)]
    pub right_operator: Option<String>,
    #[serde(default)]
    pub estimated_rows_delta: Option<f64>,
    #[serde(default)]
    pub estimated_rows_ratio: Option<f64>,
    #[serde(default)]
    pub estimated_cost_delta: Option<f64>,
    #[serde(default)]
    pub actual_rows_delta: Option<f64>,
    #[serde(default)]
    pub actual_ms_delta: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanCaptureComparison {
    pub left: PlanCaptureId,
    pub right: PlanCaptureId,
    pub engine: Engine,
    pub operator_changes: u32,
    pub cardinality_changes: u32,
    pub cost_changes: u32,
    pub runtime_changes: u32,
    pub changes: Vec<PlanNodeChange>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeletePlanCaptureRequest {
    pub expected_revision: u64,
}
