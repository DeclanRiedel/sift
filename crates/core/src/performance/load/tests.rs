use super::*;

const MS: u64 = 1_000_000;

fn config() -> BenchmarkLoadConfig {
    BenchmarkLoadConfig {
        workers: 2,
        max_starts_per_second: 100,
        failure_policy: BenchmarkLoadFailurePolicy::StopRun,
    }
}

fn limits(warmups: u32, iterations: u32) -> BenchmarkLimits {
    BenchmarkLimits {
        warmups,
        iterations,
        ..Default::default()
    }
}

fn observation(
    ordinal: u32,
    worker: u16,
    dispatch_index: u16,
    warmup: bool,
    (start_ms, end_ms): (u64, u64),
    outcome: BenchmarkOutcome,
) -> (BenchmarkSample, BenchmarkLoadTrace) {
    let elapsed = (end_ms - start_ms) * MS;
    (
        BenchmarkSample {
            ordinal,
            warmup,
            outcome,
            elapsed_ns: elapsed,
            database_execution_ns: None,
            client_elapsed_ns: Some(elapsed),
            first_row_ns: None,
            full_consumption_ns: (outcome == BenchmarkOutcome::Success).then_some(elapsed),
            rows: (outcome == BenchmarkOutcome::Success).then_some(1),
        },
        BenchmarkLoadTrace {
            worker,
            dispatch_index,
            start_ns: start_ms * MS,
            end_ns: end_ms * MS,
            admission_wait_ns: 0,
        },
    )
}

struct Fixture {
    samples: Vec<BenchmarkSample>,
    context: BenchmarkLoadContext,
}
impl Fixture {
    fn case(&self, index: u16) -> LoadCase<'_> {
        LoadCase {
            index,
            samples: &self.samples,
            context: &self.context,
        }
    }
    fn new(
        config: BenchmarkLoadConfig,
        window_ms: Option<(u64, u64)>,
        observations: impl IntoIterator<Item = (BenchmarkSample, BenchmarkLoadTrace)>,
    ) -> Self {
        let (samples, traces) = observations.into_iter().unzip();
        Self {
            samples,
            context: BenchmarkLoadContext {
                config,
                measured_window: window_ms.map(|(start, end)| BenchmarkLoadWindow {
                    start_ns: start * MS,
                    end_ns: end * MS,
                }),
                traces,
            },
        }
    }
}

#[test]
fn multiplied_workload_and_phase_schedules_are_bounded_before_execution() {
    let mut load = config();
    load.workers = 8;
    assert_eq!(planned_attempts(&[limits(100, 412)], load), Ok(4096));
    assert!(planned_attempts(&[limits(100, 413)], load).is_err());
    assert!(planned_attempts(&vec![limits(0, 1); 33], load).is_err());
    assert!(planned_attempts(&[], load).is_err());
    for workers in [0, 9, u16::MAX] {
        assert!(validate_config(BenchmarkLoadConfig { workers, ..load }).is_err());
    }
    for rate in [0, 1001, u16::MAX] {
        assert!(validate_config(BenchmarkLoadConfig {
            max_starts_per_second: rate,
            ..load
        })
        .is_err());
    }
    assert_eq!(
        minimum_start_spacing_ns(BenchmarkLoadConfig {
            max_starts_per_second: 3,
            ..load
        }),
        Ok(333_333_334)
    );
    let cases = [limits(2, 1), limits(0, 3), limits(1, 2)];
    assert_eq!(worker_phase(&cases, load, true), Ok(vec![0, 2, 0]));
    assert_eq!(
        worker_phase(&cases, load, false),
        Ok(vec![0, 1, 2, 1, 2, 1])
    );
}

#[test]
fn measured_metrics_exclude_warmups_and_use_shared_window_and_worker_capacity() {
    let mut fixture = Fixture::new(
        config(),
        Some((200, 700)),
        [
            observation(0, 0, 0, true, (0, 100), BenchmarkOutcome::Success),
            observation(1, 1, 1, true, (10, 110), BenchmarkOutcome::Success),
            observation(2, 0, 2, false, (200, 400), BenchmarkOutcome::Success),
            observation(3, 1, 3, false, (210, 510), BenchmarkOutcome::Success),
            observation(4, 0, 4, false, (410, 610), BenchmarkOutcome::Success),
            observation(5, 1, 5, false, (520, 620), BenchmarkOutcome::Success),
        ],
    );
    for trace in &mut fixture.context.traces[3..] {
        trace.admission_wait_ns = 10 * MS;
    }
    let case = fixture.case(0);
    validate_suite(&[limits(1, 2)], &[0; 6], &[fixture.case(0)], true).unwrap();
    let summary = summarize_case(&[limits(1, 2)], &[0; 6], &case).unwrap();
    assert_eq!(summary.warmup.successful, 2);
    assert_eq!(summary.measured.successful, 4);
    assert_eq!(summary.measured.unattempted, 0);
    assert_eq!(summary.successes_per_second_milli, Some(8000));
    assert_eq!(summary.client_peak_concurrency, 2);
    assert_eq!(summary.client_busy_permille, Some(800));
    assert_eq!(summary.admission_wait_mean_ns, Some(7_500_000));
    assert_eq!(summary.admission_wait_max_ns, Some(10 * MS));
}

#[test]
fn full_suite_checks_cross_case_overlap_and_aggregates_occupancy_without_latencies() {
    let mut first = Fixture::new(
        config(),
        Some((0, 60)),
        [
            observation(0, 0, 0, false, (0, 20), BenchmarkOutcome::Success),
            observation(1, 1, 1, false, (10, 30), BenchmarkOutcome::Success),
        ],
    );
    let second = Fixture::new(
        config(),
        Some((0, 60)),
        [
            observation(0, 0, 2, false, (20, 40), BenchmarkOutcome::Success),
            observation(1, 1, 3, false, (30, 50), BenchmarkOutcome::Success),
        ],
    );
    let limits = [limits(0, 1); 2];
    let order = [0, 0, 1, 1];
    let summary = summarize_suite(&limits, &order, &[first.case(0), second.case(1)], true).unwrap();
    assert_eq!(summary.client_peak_concurrency, 2);
    assert_eq!(summary.client_busy_permille, Some(666));
    assert_eq!(summary.successes_per_second_milli, Some(66_666));
    assert_eq!(summary.measured.successful, 4);
    // Each saved case alone is internally valid, but worker 0 overlaps cases.
    first.context.traces[0].end_ns = 25 * MS;
    first.samples[0].elapsed_ns = 25 * MS;
    first.samples[0].client_elapsed_ns = Some(25 * MS);
    first.samples[0].full_consumption_ns = Some(25 * MS);
    validate_case(&limits, &order, &first.case(0)).unwrap();
    validate_case(&limits, &order, &second.case(1)).unwrap();
    assert!(validate_suite(&limits, &order, &[first.case(0), second.case(1)], false).is_err());
}

#[test]
fn retire_worker_keeps_errors_and_unattempted_work_and_stop_run_refuses_later_admission() {
    let mut fixture = Fixture::new(
        BenchmarkLoadConfig {
            failure_policy: BenchmarkLoadFailurePolicy::RetireWorker,
            ..config()
        },
        Some((0, 60)),
        [
            observation(0, 0, 0, false, (0, 50), BenchmarkOutcome::Success),
            observation(1, 1, 1, false, (10, 20), BenchmarkOutcome::Failed),
            observation(2, 0, 2, false, (50, 60), BenchmarkOutcome::Success),
        ],
    );
    let limits = [limits(0, 2)];
    let summary = summarize_suite(&limits, &[0; 3], &[fixture.case(0)], false).unwrap();
    assert_eq!(summary.measured.successful, 2);
    assert_eq!(summary.measured.failed, 1);
    assert_eq!(summary.measured.unattempted, 1);
    assert_eq!(summary.attempts_per_second_milli, Some(50_000));
    assert_eq!(summary.successes_per_second_milli, Some(33_333));
    assert!(validate_suite(&limits, &[0; 3], &[fixture.case(0)], true).is_err());
    fixture.context.config.failure_policy = BenchmarkLoadFailurePolicy::StopRun;
    assert!(validate_case(&limits, &[0; 3], &fixture.case(0)).is_err());
    assert!(validate_suite(&limits, &[0; 3], &[fixture.case(0)], false).is_err());
    fixture.samples.truncate(2);
    fixture.context.traces.truncate(2);
    validate_suite(&limits, &[0; 2], &[fixture.case(0)], false).unwrap();
    // A zero-duration failure cannot invalidate its own dispatch.
    fixture.context.traces[1].end_ns = 10 * MS;
    fixture.samples[1].elapsed_ns = 0;
    fixture.samples[1].client_elapsed_ns = Some(0);
    validate_suite(&limits, &[0; 2], &[fixture.case(0)], false).unwrap();
}

#[test]
fn complete_suite_refuses_a_worker_that_reorders_cases_or_a_cross_case_rate_burst() {
    let first = Fixture::new(
        config(),
        Some((0, 40)),
        [
            observation(0, 0, 2, false, (20, 30), BenchmarkOutcome::Success),
            observation(1, 1, 3, false, (30, 40), BenchmarkOutcome::Success),
        ],
    );
    let second = Fixture::new(
        config(),
        Some((0, 40)),
        [
            observation(0, 0, 0, false, (0, 10), BenchmarkOutcome::Success),
            observation(1, 1, 1, false, (10, 20), BenchmarkOutcome::Success),
        ],
    );
    let limits = [limits(0, 1); 2];
    let order = [1, 1, 0, 0];
    validate_case(&limits, &order, &first.case(0)).unwrap();
    validate_case(&limits, &order, &second.case(1)).unwrap();
    assert!(validate_suite(&limits, &order, &[first.case(0), second.case(1)], true).is_err());
    // Per-case intervals satisfy the rate ceiling, but cross-case admissions
    // at 10 and 19 ms violate the global 10 ms minimum spacing.
    let first = Fixture::new(
        config(),
        Some((0, 40)),
        [
            observation(0, 0, 0, false, (0, 10), BenchmarkOutcome::Success),
            observation(1, 1, 1, false, (10, 20), BenchmarkOutcome::Success),
        ],
    );
    let second = Fixture::new(
        config(),
        Some((0, 40)),
        [
            observation(0, 0, 2, false, (19, 29), BenchmarkOutcome::Success),
            observation(1, 1, 3, false, (30, 40), BenchmarkOutcome::Success),
        ],
    );
    let order = [0, 0, 1, 1];
    validate_case(&limits, &order, &first.case(0)).unwrap();
    validate_case(&limits, &order, &second.case(1)).unwrap();
    assert!(validate_suite(&limits, &order, &[first.case(0), second.case(1)], true).is_err());
}

#[test]
fn failed_warmups_and_setup_stops_never_manufacture_measured_rates() {
    let mut fixture = Fixture::new(
        config(),
        None,
        [observation(
            0,
            0,
            0,
            true,
            (0, 1),
            BenchmarkOutcome::TimedOut,
        )],
    );
    let limits = [limits(1, 1)];
    let summary = summarize_suite(&limits, &[0], &[fixture.case(0)], false).unwrap();
    assert_eq!(summary.warmup.timed_out, 1);
    assert_eq!(summary.warmup.unattempted, 1);
    assert_eq!(summary.measured.unattempted, 2);
    assert_eq!(summary.successes_per_second_milli, None);
    assert_eq!(summary.client_busy_permille, None);
    assert_eq!(summary.admission_wait_mean_ns, None);
    fixture.context.measured_window = Some(BenchmarkLoadWindow {
        start_ns: MS,
        end_ns: MS,
    });
    assert!(validate_suite(&limits, &[0], &[fixture.case(0)], false).is_err());
    let empty = Fixture::new(config(), None, []);
    let summary = summarize_suite(&limits, &[], &[empty.case(0)], false).unwrap();
    assert_eq!(summary.warmup.unattempted, 2);
    assert_eq!(summary.measured.unattempted, 2);
}

#[test]
fn corrupted_trace_identity_clocks_rates_and_sample_aliases_are_rejected() {
    let original = Fixture::new(
        config(),
        Some((0, 30)),
        [
            observation(0, 0, 0, false, (0, 20), BenchmarkOutcome::Success),
            observation(1, 1, 1, false, (10, 30), BenchmarkOutcome::Success),
        ],
    );
    let limits = [limits(0, 1)];
    validate_suite(&limits, &[0; 2], &[original.case(0)], true).unwrap();
    for change in 0..10 {
        let mut fixture = Fixture {
            samples: original.samples.clone(),
            context: original.context.clone(),
        };
        match change {
            0 => fixture.context.traces[1].dispatch_index = 0,
            1 => fixture.context.traces[1].worker = 2,
            2 => fixture.context.traces[1].start_ns = 9 * MS,
            3 => fixture.samples[0].client_elapsed_ns = None,
            4 => fixture.samples[0].ordinal = 1,
            5 => fixture.samples[0].elapsed_ns += 1,
            6 => fixture.context.measured_window = None,
            7 => fixture.context.traces[0].admission_wait_ns = 1,
            8 => fixture.samples[0].rows = None,
            9 => fixture.context.traces[1].dispatch_index = u16::MAX,
            _ => unreachable!(),
        }
        assert!(
            validate_suite(&limits, &[0; 2], &[fixture.case(0)], true).is_err(),
            "accepted corruption {change}"
        );
    }
}

#[test]
fn half_open_occupancy_and_extreme_clocks_do_not_overflow_or_invent_zero_rates() {
    let mut fixture = Fixture::new(
        BenchmarkLoadConfig {
            workers: 1,
            ..config()
        },
        Some((0, 20)),
        [
            observation(0, 0, 0, false, (0, 10), BenchmarkOutcome::Success),
            observation(1, 0, 1, false, (10, 20), BenchmarkOutcome::Success),
        ],
    );
    let limits = [limits(0, 2)];
    let summary = summarize_suite(&limits, &[0; 2], &[fixture.case(0)], true).unwrap();
    assert_eq!(summary.client_peak_concurrency, 1);
    assert_eq!(summary.client_busy_permille, Some(1000));
    fixture.samples.truncate(1);
    fixture.context.traces.truncate(1);
    fixture.samples[0].elapsed_ns = u64::MAX;
    fixture.samples[0].client_elapsed_ns = Some(u64::MAX);
    fixture.samples[0].full_consumption_ns = Some(u64::MAX);
    fixture.context.traces[0].end_ns = u64::MAX;
    fixture.context.measured_window.as_mut().unwrap().end_ns = u64::MAX;
    let summary = summarize_suite(&limits, &[0], &[fixture.case(0)], false).unwrap();
    assert_eq!(summary.client_busy_permille, Some(1000));
    assert_eq!(summary.successes_per_second_milli, Some(0));
    fixture.samples[0].elapsed_ns = 0;
    fixture.samples[0].client_elapsed_ns = Some(0);
    fixture.samples[0].full_consumption_ns = Some(0);
    fixture.context.traces[0].end_ns = 0;
    fixture.context.measured_window.as_mut().unwrap().end_ns = 0;
    let summary = summarize_suite(&limits, &[0], &[fixture.case(0)], false).unwrap();
    assert_eq!(summary.client_peak_concurrency, 0);
    assert_eq!(summary.client_busy_permille, None);
    assert_eq!(summary.successes_per_second_milli, None);
}
