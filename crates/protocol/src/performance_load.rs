//! Concurrent benchmark observations. No execution authority or provenance proof.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkLoadFailurePolicy {
    StopRun,
    RetireWorker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkLoadConfig {
    pub workers: u16,
    /// Global start ceiling, shared by all workers and cases; no catch-up bursts.
    pub max_starts_per_second: u16,
    pub failure_policy: BenchmarkLoadFailurePolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkLoadWindow {
    /// Monotonic offsets from run setup; excludes setup, warmups and cleanup.
    pub start_ns: u64,
    pub end_ns: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkLoadTrace {
    pub worker: u16,
    /// Index in the suite's shared actual_order, including warmups and failures.
    pub dispatch_index: u16,
    pub start_ns: u64,
    pub end_ns: u64,
    /// Worker time waiting for dispatch admission, including pacing.
    pub admission_wait_ns: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkLoadContext {
    pub config: BenchmarkLoadConfig,
    /// None when setup/warmups stopped before the measurement barrier.
    pub measured_window: Option<BenchmarkLoadWindow>,
    /// Case-local traces in sample ordinal order, never SQL/binds/rows/errors.
    pub traces: Vec<BenchmarkLoadTrace>,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkLoadOutcomes {
    pub successful: u32,
    pub failed: u32,
    pub timed_out: u32,
    pub cancelled: u32,
    pub unattempted: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkLoadSummary {
    pub warmup: BenchmarkLoadOutcomes,
    pub measured: BenchmarkLoadOutcomes,
    pub measured_window_ns: Option<u64>,
    /// Rates in thousandths of attempts per second, rounded down.
    pub attempts_per_second_milli: Option<u64>,
    pub successes_per_second_milli: Option<u64>,
    /// Measured client attempts in flight, not native database saturation.
    pub client_peak_concurrency: u16,
    /// Busy client worker time / configured worker capacity, in thousandths.
    pub client_busy_permille: Option<u16>,
    /// Measured admission waits only. None when no measured attempt was admitted.
    pub admission_wait_mean_ns: Option<u64>,
    pub admission_wait_max_ns: Option<u64>,
}
