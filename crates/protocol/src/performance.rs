//! Explicit, bounded query benchmarking. Timings are server-observed full drain,
//! not database CPU time, network-to-desktop time or UI rendering time.
use serde::{Deserialize, Serialize};

/// User-saved snapshot, not a server attestation or a rerunnable definition.
/// Bind values are deliberately absent. SQL and names may be sensitive.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveBenchmarkRunRequest {
    pub name: String,
    pub report: BenchmarkReport,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SavedBenchmarkRun {
    pub id: uuid::Uuid,
    pub saved_at: chrono::DateTime<chrono::Utc>,
    pub name: String,
    pub report: BenchmarkReport,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SavedBenchmarkRunSummary {
    pub id: uuid::Uuid,
    pub saved_at: chrono::DateTime<chrono::Utc>,
    pub name: String,
    pub engine: Option<crate::Engine>,
    pub payload_available: bool,
    pub completed: bool,
    pub median_ns: Option<f64>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListBenchmarkRunsRequest {
    pub cursor: Option<uuid::Uuid>,
    pub limit: Option<u32>,
}

/// Serial-run budgets. Execution must additionally enforce deployment policy,
/// query permissions and read-only protections; these limits are not a sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkLimits {
    pub warmups: u32,
    pub iterations: u32,
    pub query_timeout_ms: u64,
    pub total_budget_ms: u64,
    pub delay_ms: u64,
}

impl Default for BenchmarkLimits {
    fn default() -> Self {
        Self {
            warmups: 2,
            iterations: 10,
            query_timeout_ms: 30_000,
            total_budget_ms: 120_000,
            delay_ms: 0,
        }
    }
}

impl BenchmarkLimits {
    /// Hard ceilings bound the future runner's sample count and wall time.
    /// The total budget may intentionally stop a run before all iterations.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.warmups > 100 {
            return Err("warm-ups must not exceed 100");
        }
        if !(1..=10_000).contains(&self.iterations) {
            return Err("measured iterations must be between 1 and 10000");
        }
        if !(1..=3_600_000).contains(&self.total_budget_ms) {
            return Err("total budget must be between 1 ms and one hour");
        }
        if self.query_timeout_ms == 0 || self.query_timeout_ms > self.total_budget_ms {
            return Err("query timeout must be positive and no greater than the total budget");
        }
        if self.delay_ms >= self.total_budget_ms {
            return Err("inter-query delay must be smaller than the total budget");
        }
        Ok(())
    }
}

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
