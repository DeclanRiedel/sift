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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify_results: Option<BenchmarkResultCheck>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<BenchmarkResultVerification>,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BenchmarkPairReport {
    pub baseline: crate::BenchmarkReport,
    pub candidate: crate::BenchmarkReport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkResultOrder {
    Ordered,
    Unordered,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkResultCheck {
    pub order: BenchmarkResultOrder,
    pub max_rows: u32,
    pub max_bytes: u64,
}
impl Default for BenchmarkResultCheck {
    fn default() -> Self {
        Self {
            order: BenchmarkResultOrder::Ordered,
            max_rows: 10_000,
            max_bytes: 4 * 1024 * 1024,
        }
    }
}
impl BenchmarkResultCheck {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(1..=100_000).contains(&self.max_rows)
            || !(1024..=16 * 1024 * 1024).contains(&self.max_bytes)
        {
            return Err("Result verification requires 1..100000 rows and 1024..16777216 canonical bytes per result");
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkVerificationOutcome {
    Matched,
    Different,
    LimitExceeded,
    Unsupported,
    Failed,
    Cancelled,
    TimedOut,
    AccessChanged,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkVerificationSnapshot {
    ReadOnlyRepeatableRead,
    ReadOnlySerializable,
    IndependentReads,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkResultObservation {
    pub rows: u64,
    pub columns: u32,
    pub canonical_bytes: u64,
}
/// Row-free, unattested decoded-result observations; never execution authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkResultVerification {
    pub version: u16,
    pub config: BenchmarkResultCheck,
    pub snapshot: BenchmarkVerificationSnapshot,
    pub outcome: BenchmarkVerificationOutcome,
    pub baseline: Option<BenchmarkResultObservation>,
    pub candidate: Option<BenchmarkResultObservation>,
}
