//! Explicit parameter cases. Bind values are request-only.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkParameterCase {
    pub id: String,
    #[serde(default)]
    pub params: Vec<crate::Value>,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkSuiteQuery {
    pub id: String,
    pub sql: String,
    pub cases: Vec<BenchmarkParameterCase>,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkSuiteRequest {
    pub run_id: uuid::Uuid,
    pub queries: Vec<BenchmarkSuiteQuery>,
    pub limits: crate::BenchmarkLimits,
    pub workload_confirmed: bool,
}
/// Caller-supplied lineage remains unattested after saving/importing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkSuiteContext {
    pub version: u16,
    pub run_id: uuid::Uuid,
    pub query_id: String,
    pub case_id: String,
    pub case_index: u16,
    pub case_count: u16,
    /// Actual dispatch order, including warmups and unsuccessful attempts.
    pub actual_order: Vec<u16>,
    pub suite_completed: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BenchmarkSuiteReport {
    pub run_id: uuid::Uuid,
    /// Stable flattened query/case order. Each report carries suite context.
    pub cases: Vec<crate::BenchmarkReport>,
    pub completed: bool,
}
