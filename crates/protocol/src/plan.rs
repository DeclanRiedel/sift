//! Execution plans (Phase D).
//!
//! A query's execution plan captured as an engine-neutral, typed [`PlanNode`]
//! tree. Per the design in `docs/PLANS/execution-plans.md` (ADR-025): Postgres
//! `EXPLAIN (FORMAT JSON)` and SQL Server showplan XML both normalize into the
//! same tree — a small typed core plus an `extra` map for engine-specific
//! attributes, plus the untouched raw plan on the response.
//!
//! Pure serde: capture + parsing live in the server (`crates/server/src/plan.rs`).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{ConnectionId, DriverWarning, Engine, Value};

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
