//! Controlled two-workload benchmarks. Bind values appear only in requests.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkWorkload {
    pub sql: String,
    #[serde(default)]
    pub params: Vec<crate::Value>,
}
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BenchmarkPairOrder {
    #[default]
    Alternating,
    RandomizedV1 {
        seed: u64,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkPopulation {
    Baseline,
    Candidate,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkPairRequest {
    pub run_id: uuid::Uuid,
    pub baseline: BenchmarkWorkload,
    pub candidate: BenchmarkWorkload,
    pub limits: crate::BenchmarkLimits,
    #[serde(default)]
    pub order: BenchmarkPairOrder,
    pub workload_confirmed: bool,
}
/// Unattested measurement context, never execution authority or raw bind data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkPairContext {
    pub version: u16,
    pub run_id: uuid::Uuid,
    pub population: BenchmarkPopulation,
    pub order: BenchmarkPairOrder,
    /// Actual dispatch order across both populations, including warmups/failures.
    pub actual_order: Vec<BenchmarkPopulation>,
    pub pair_completed: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BenchmarkPairReport {
    pub baseline: crate::BenchmarkReport,
    pub candidate: crate::BenchmarkReport,
}
