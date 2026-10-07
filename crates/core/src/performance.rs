//! Pure benchmark accounting. Execution, clocks and persistence belong to callers.
//!
//! Summaries deliberately cover one timing dimension and one execution mode.
//! A caller must not mix instrumented profiles with ordinary benchmark samples.

pub use sift_protocol::BenchmarkLimits;

/// Completed warm-ups remain inspectable but never enter measured statistics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SamplePhase {
    Warmup,
    Measured,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleOutcome {
    Success,
    Failed,
    TimedOut,
    Cancelled,
}

/// Nanoseconds avoid losing sub-millisecond query measurements at capture time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimingSample {
    pub phase: SamplePhase,
    pub outcome: SampleOutcome,
    /// None means this timing dimension was unavailable, never zero duration.
    pub elapsed_ns: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TimingDistribution {
    pub count: usize,
    pub min_ns: u64,
    pub max_ns: u64,
    pub mean_ns: f64,
    pub median_ns: f64,
    /// Sample standard deviation (n - 1); unavailable for a singleton.
    pub standard_deviation_ns: Option<f64>,
    /// Nearest-rank percentile; withheld below 100 successful timed samples.
    /// This floor is a display policy, not a statistical confidence guarantee.
    pub p95_ns: Option<u64>,
    /// Withheld below 1,000 samples to avoid implying reliable tail coverage.
    pub p99_ns: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TimingSummary {
    pub warmups: usize,
    pub successful: usize,
    pub failed: usize,
    pub timed_out: usize,
    pub cancelled: usize,
    pub unavailable: usize,
    pub distribution: Option<TimingDistribution>,
}

/// Summarize raw samples without dropping failure counts or treating timeouts
/// as successful latency samples. No outliers are removed.
pub fn summarize(samples: &[TimingSample]) -> TimingSummary {
    let mut summary = TimingSummary::default();
    let mut timings = Vec::new();
    for sample in samples {
        if sample.phase == SamplePhase::Warmup {
            summary.warmups += 1;
            continue;
        }
        match sample.outcome {
            SampleOutcome::Success => {
                summary.successful += 1;
                if let Some(elapsed) = sample.elapsed_ns {
                    timings.push(elapsed);
                } else {
                    summary.unavailable += 1;
                }
            }
            SampleOutcome::Failed => summary.failed += 1,
            SampleOutcome::TimedOut => summary.timed_out += 1,
            SampleOutcome::Cancelled => summary.cancelled += 1,
        }
    }
    if timings.is_empty() {
        return summary;
    }
    timings.sort_unstable();
    // Welford avoids overflow of an integer sum and catastrophic cancellation
    // from subtracting two large squared sums.
    let mut mean = 0.0;
    let mut m2 = 0.0;
    for (index, value) in timings.iter().enumerate() {
        let value = *value as f64;
        let delta = value - mean;
        mean += delta / (index + 1) as f64;
        m2 += delta * (value - mean);
    }
    let count = timings.len();
    let median = if count % 2 == 0 {
        timings[count / 2 - 1] as f64 / 2.0 + timings[count / 2] as f64 / 2.0
    } else {
        timings[count / 2] as f64
    };
    let percentile = |percent: usize| {
        // ceil(count * percent / 100), without overflowing the multiplication.
        let rank = count / 100 * percent + ((count % 100) * percent).div_ceil(100);
        timings[rank - 1]
    };
    summary.distribution = Some(TimingDistribution {
        count,
        min_ns: timings[0],
        max_ns: timings[count - 1],
        mean_ns: mean,
        median_ns: median,
        standard_deviation_ns: (count > 1).then(|| (m2.max(0.0) / (count - 1) as f64).sqrt()),
        p95_ns: (count >= 100).then(|| percentile(95)),
        p99_ns: (count >= 1_000).then(|| percentile(99)),
    });
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_budgets_are_bounded_before_execution() {
        let defaults = BenchmarkLimits::default();
        assert!(defaults.validate().is_ok());
        for invalid in [
            BenchmarkLimits {
                warmups: 101,
                ..defaults
            },
            BenchmarkLimits {
                iterations: 0,
                ..defaults
            },
            BenchmarkLimits {
                iterations: u32::MAX,
                ..defaults
            },
            BenchmarkLimits {
                query_timeout_ms: 0,
                ..defaults
            },
            BenchmarkLimits {
                total_budget_ms: u64::MAX,
                ..defaults
            },
            BenchmarkLimits {
                total_budget_ms: 1,
                ..defaults
            },
            BenchmarkLimits {
                delay_ms: defaults.total_budget_ms,
                ..defaults
            },
        ] {
            assert!(invalid.validate().is_err(), "{invalid:?}");
        }
        assert!(BenchmarkLimits {
            warmups: 0,
            iterations: 1,
            query_timeout_ms: 1,
            total_budget_ms: 1,
            delay_ms: 0
        }
        .validate()
        .is_ok());
    }

    fn success(ns: u64) -> TimingSample {
        TimingSample {
            phase: SamplePhase::Measured,
            outcome: SampleOutcome::Success,
            elapsed_ns: Some(ns),
        }
    }

    #[test]
    fn outcomes_and_warmups_do_not_bias_latency() {
        let mut samples = vec![success(10), success(20), success(30), success(40)];
        samples.push(TimingSample {
            phase: SamplePhase::Warmup,
            ..success(999)
        });
        for outcome in [
            SampleOutcome::Failed,
            SampleOutcome::TimedOut,
            SampleOutcome::Cancelled,
        ] {
            samples.push(TimingSample {
                outcome,
                ..success(999)
            });
        }
        samples.push(TimingSample {
            elapsed_ns: None,
            ..success(0)
        });
        let summary = summarize(&samples);
        assert_eq!(
            (summary.warmups, summary.successful, summary.unavailable),
            (1, 5, 1)
        );
        assert_eq!(
            (summary.failed, summary.timed_out, summary.cancelled),
            (1, 1, 1)
        );
        let stats = summary.distribution.unwrap();
        assert_eq!((stats.count, stats.min_ns, stats.max_ns), (4, 10, 40));
        assert_eq!((stats.mean_ns, stats.median_ns), (25.0, 25.0));
        assert!((stats.standard_deviation_ns.unwrap() - (500.0_f64 / 3.0).sqrt()).abs() < 1e-9);
        assert_eq!((stats.p95_ns, stats.p99_ns), (None, None));
    }

    #[test]
    fn empty_missing_zero_and_large_timings_remain_distinct() {
        assert_eq!(summarize(&[]), TimingSummary::default());
        assert!(summarize(&[TimingSample {
            elapsed_ns: None,
            ..success(0)
        }])
        .distribution
        .is_none());
        let zero = summarize(&[success(0)]).distribution.unwrap();
        assert_eq!(zero.mean_ns, 0.0);
        assert_eq!(zero.standard_deviation_ns, None);
        let large = summarize(&[success(u64::MAX), success(u64::MAX)])
            .distribution
            .unwrap();
        assert_eq!(large.median_ns, u64::MAX as f64);
        assert_eq!(large.standard_deviation_ns, Some(0.0));
    }

    #[test]
    fn tail_percentiles_require_samples_and_use_nearest_rank() {
        let samples: Vec<_> = (1..=1000).rev().map(success).collect();
        let all = summarize(&samples).distribution.unwrap();
        assert_eq!((all.p95_ns, all.p99_ns), (Some(950), Some(990)));
        let hundred = summarize(&samples[900..]).distribution.unwrap();
        assert_eq!((hundred.p95_ns, hundred.p99_ns), (Some(95), None));
        assert_eq!(summarize(&samples[..99]).distribution.unwrap().p95_ns, None);
        assert_eq!(
            summarize(&[success(1), success(9), success(3)])
                .distribution
                .unwrap()
                .median_ns,
            3.0
        );
    }
}

/// Recompute from samples: imported snapshot summaries are not trusted evidence.
pub fn benchmark_statistics(
    report: &sift_protocol::BenchmarkReport,
) -> sift_protocol::BenchmarkStatistics {
    let samples = report
        .samples
        .iter()
        .map(|sample| TimingSample {
            phase: if sample.warmup {
                SamplePhase::Warmup
            } else {
                SamplePhase::Measured
            },
            outcome: match sample.outcome {
                sift_protocol::BenchmarkOutcome::Success => SampleOutcome::Success,
                sift_protocol::BenchmarkOutcome::Failed => SampleOutcome::Failed,
                sift_protocol::BenchmarkOutcome::TimedOut => SampleOutcome::TimedOut,
                sift_protocol::BenchmarkOutcome::Cancelled => SampleOutcome::Cancelled,
            },
            elapsed_ns: Some(sample.elapsed_ns),
        })
        .collect::<Vec<_>>();
    let summary = summarize(&samples);
    sift_protocol::BenchmarkStatistics {
        successful: summary.successful,
        failed: summary.failed,
        timed_out: summary.timed_out,
        cancelled: summary.cancelled,
        median_ns: summary.distribution.map(|stats| stats.median_ns),
    }
}

pub fn compare_benchmarks(
    base: &sift_protocol::BenchmarkReport,
    current: &sift_protocol::BenchmarkReport,
) -> sift_protocol::BenchmarkComparison {
    let baseline = benchmark_statistics(base);
    let candidate = benchmark_statistics(current);
    let mut warnings = vec!["User-saved snapshots, not server attestations; profile, data/schema equivalence, cache state and server load are not established. Differences are descriptive, not a causal performance verdict.".into()];
    let matching = base.engine == current.engine
        && base.completed
        && current.completed
        && base.warmups == current.warmups
        && base.requested_iterations == current.requested_iterations
        && base.query_timeout_ms == current.query_timeout_ms
        && base.total_budget_ms == current.total_budget_ms
        && base.delay_ms == current.delay_ms
        && base.parameter_count == current.parameter_count;
    if !matching {
        warnings.push(
            "Different engines/configurations or incomplete runs: numeric comparison withheld."
                .into(),
        );
    }
    if base.sql != current.sql {
        warnings.push("SQL differs between snapshots.".into());
    }
    if base.parameter_count > 0 || current.parameter_count > 0 {
        warnings.push("Bind values are absent and cannot be compared.".into());
    }
    if base.environment.is_none()
        || current.environment.is_none()
        || serde_json::to_value(&base.environment).ok()
            != serde_json::to_value(&current.environment).ok()
    {
        warnings.push("Execution environment metadata is missing or differs.".into());
    }
    if baseline.failed
        + baseline.timed_out
        + baseline.cancelled
        + candidate.failed
        + candidate.timed_out
        + candidate.cancelled
        > 0
    {
        warnings.push(
            "Unsuccessful samples excluded from latency statistics; outcome counts retained."
                .into(),
        );
    }
    let delta = baseline
        .median_ns
        .zip(candidate.median_ns)
        .filter(|(a, _)| matching && *a > 0.0);
    sift_protocol::BenchmarkComparison {
        timing: "server_side_client_elapsed_ns".into(),
        baseline,
        candidate,
        delta_ns: delta.map(|(a, b)| b - a),
        delta_percent: delta.map(|(a, b)| (b / a - 1.0) * 100.0),
        warnings,
    }
}

#[cfg(test)]
mod comparison_tests {
    use super::*;
    fn report() -> sift_protocol::BenchmarkReport {
        serde_json::from_value(serde_json::json!({
            "version":2,"run_id":uuid::Uuid::new_v4(),"engine":"postgres","sql":"SELECT 1",
            "captured_at":"2026-10-07T00:00:00Z","warmups":1,"requested_iterations":2,"query_timeout_ms":1000,
            "total_budget_ms":5000,"delay_ms":0,"parameter_count":1,"completed":true,"warnings":[],
            "samples":[{"ordinal":1,"warmup":true,"outcome":"success","elapsed_ns":999999,"first_row_ns":null,"rows":1},
                {"ordinal":2,"warmup":false,"outcome":"success","elapsed_ns":10,"first_row_ns":null,"rows":1},
                {"ordinal":3,"warmup":false,"outcome":"success","elapsed_ns":30,"first_row_ns":null,"rows":1}],
            "median_ns":9999,"mean_ns":9999,"min_ns":9999,"max_ns":9999,"standard_deviation_ns":null,"p95_ns":null,"p99_ns":null
        })).unwrap()
    }
    #[test]
    fn recomputes_samples_and_withholds_incompatible_or_zero_baselines() {
        let base = report();
        let mut current = base.clone();
        current.samples[1].elapsed_ns = 20;
        current.samples[2].elapsed_ns = 60;
        let comparison = compare_benchmarks(&base, &current);
        assert_eq!(comparison.baseline.median_ns, Some(20.0));
        assert_eq!(comparison.delta_percent, Some(100.0));
        assert!(comparison
            .warnings
            .iter()
            .any(|warning| warning.contains("Bind values")));
        current.completed = false;
        assert_eq!(compare_benchmarks(&base, &current).delta_ns, None);
        current.completed = true;
        current.engine = sift_protocol::Engine::Sqlite;
        assert_eq!(compare_benchmarks(&base, &current).delta_ns, None);
        current = base.clone();
        current.samples[1].elapsed_ns = 0;
        current.samples[2].elapsed_ns = 0;
        assert_eq!(compare_benchmarks(&current, &base).delta_percent, None);
        current = base.clone();
        current.samples[1].outcome = sift_protocol::BenchmarkOutcome::TimedOut;
        let compared = compare_benchmarks(&base, &current);
        assert_eq!(compared.candidate.timed_out, 1);
        assert_eq!(compared.candidate.median_ns, Some(30.0));
    }
}
