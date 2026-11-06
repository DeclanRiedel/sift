//! Explicit parameter cases. Bind values are request-only.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkParameterCase {
    pub id: String,
    #[serde(default)]
    pub params: Vec<crate::Value>,
    /// Complete case override; omission inherits suite defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<crate::BenchmarkLimits>,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkSuiteQuery {
    pub id: String,
    pub sql: String,
    pub cases: Vec<BenchmarkParameterCase>,
}
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BenchmarkSuiteOrder {
    #[default]
    Serial,
    RoundRobin,
    RandomizedRoundsV1 {
        seed: u64,
    },
}
impl BenchmarkSuiteOrder {
    pub fn is_serial(&self) -> bool {
        matches!(self, Self::Serial)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkSuiteRequest {
    #[serde(default, skip_serializing_if = "BenchmarkSuiteOrder::is_serial")]
    pub order: BenchmarkSuiteOrder,
    pub run_id: uuid::Uuid,
    pub queries: Vec<BenchmarkSuiteQuery>,
    pub limits: crate::BenchmarkLimits,
    pub workload_confirmed: bool,
}
/// Caller-supplied lineage remains unattested after saving/importing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkSuiteContext {
    /// Version 3: exact dispatch algorithm and seed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<BenchmarkSuiteOrder>,
    pub version: u16,
    pub run_id: uuid::Uuid,
    pub query_id: String,
    pub case_id: String,
    pub case_index: u16,
    pub case_count: u16,
    /// Actual dispatch order, including warmups and unsuccessful attempts.
    pub actual_order: Vec<u16>,
    pub suite_completed: bool,
    /// Version 2: effective limits in flattened case order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub case_limits: Vec<crate::BenchmarkLimits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_total_budget_ms: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BenchmarkSuiteReport {
    pub run_id: uuid::Uuid,
    /// Stable flattened query/case order. Each report carries suite context.
    pub cases: Vec<crate::BenchmarkReport>,
    pub completed: bool,
}
