use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkMedianMethod {
    BinomialOrderStatisticsBonferroniV1,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BenchmarkMedianInterval {
    pub samples: usize,
    /// One-based inclusive order-statistic ranks, without interpolation.
    pub lower_rank: usize,
    pub upper_rank: usize,
    pub lower_ns: u64,
    pub upper_ns: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BenchmarkNumericInterval {
    pub lower: f64,
    pub upper: f64,
}
/// Conditional IID-model bounds, never a controlled regression verdict.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BenchmarkMedianUncertainty {
    pub method: BenchmarkMedianMethod,
    /// Target lower coverage under the assumptions; discrete coverage can be higher.
    pub joint_confidence_level: f64,
    pub population_confidence_level: f64,
    pub baseline: Option<BenchmarkMedianInterval>,
    pub candidate: Option<BenchmarkMedianInterval>,
    pub delta_ns: Option<BenchmarkNumericInterval>,
    pub delta_percent: Option<BenchmarkNumericInterval>,
    pub unavailable_reason: Option<String>,
    pub relative_unavailable_reason: Option<String>,
    pub assumptions: String,
}
