//! Explicit, bounded query benchmarking. Timings are server-observed full drain,
//! not database CPU time, network-to-desktop time or UI rendering time.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkRequest {
    pub run_id: uuid::Uuid,
    pub sql: String,
    #[serde(default)]
    pub params: Vec<crate::Value>,
    pub warmups: u32,
    pub iterations: u32,
    pub query_timeout_ms: u64,
    pub total_budget_ms: u64,
    #[serde(default)]
    pub delay_ms: u64,
    /// Explicit acknowledgement that repeated reads can load the database and
    /// invoke externally visible functions. Does not override server policy.
    pub workload_confirmed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkOutcome {
    Success,
    Failed,
    TimedOut,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BenchmarkSample {
    pub ordinal: u32,
    pub warmup: bool,
    pub outcome: BenchmarkOutcome,
    pub elapsed_ns: u64,
    pub first_row_ns: Option<u64>,
    /// Complete row count is unavailable when an iteration does not finish.
    pub rows: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BenchmarkReport {
    pub version: u16,
    pub run_id: uuid::Uuid,
    pub engine: crate::Engine,
    pub sql: String,
    pub captured_at: chrono::DateTime<chrono::Utc>,
    pub warmups: u32,
    pub requested_iterations: u32,
    pub query_timeout_ms: u64,
    pub total_budget_ms: u64,
    pub delay_ms: u64,
    /// Values are not included in exported reports.
    pub parameter_count: usize,
    pub samples: Vec<BenchmarkSample>,
    pub completed: bool,
    pub warnings: Vec<String>,
    pub median_ns: Option<f64>,
    pub mean_ns: Option<f64>,
    pub min_ns: Option<u64>,
    pub max_ns: Option<u64>,
    pub standard_deviation_ns: Option<f64>,
    pub p95_ns: Option<u64>,
    pub p99_ns: Option<u64>,
}
