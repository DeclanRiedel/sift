//! Bounded concurrent-load schedules, observation validation and client metrics.
//! Clocks, admission, connections and execution remain the runner's responsibility.
#[cfg(test)]
mod tests;
use crate::performance_suite::{MAX_ATTEMPTS, MAX_CASES};
use sift_protocol::{
    BenchmarkLimits, BenchmarkLoadConfig, BenchmarkLoadContext, BenchmarkLoadFailurePolicy,
    BenchmarkLoadOutcomes, BenchmarkLoadSummary, BenchmarkLoadTrace, BenchmarkLoadWindow,
    BenchmarkOutcome, BenchmarkSample,
};

pub const MAX_WORKERS: u16 = 8;
pub const MAX_STARTS_PER_SECOND: u16 = 1000;

pub fn validate_config(config: BenchmarkLoadConfig) -> Result<(), &'static str> {
    if !(1..=MAX_WORKERS).contains(&config.workers)
        || !(1..=MAX_STARTS_PER_SECOND).contains(&config.max_starts_per_second)
    {
        return Err("Load requires 1–8 workers and 1–1000 global starts per second");
    }
    Ok(())
}

/// Both phases replicate every case on every worker; no connection replacement.
pub fn planned_attempts(
    limits: &[BenchmarkLimits],
    config: BenchmarkLoadConfig,
) -> Result<usize, &'static str> {
    validate_config(config)?;
    if !(1..=MAX_CASES).contains(&limits.len()) {
        return Err("Load requires 1–32 cases");
    }
    let mut total = 0usize;
    for limit in limits {
        limit.validate()?;
        total += (limit.warmups as usize + limit.iterations as usize) * config.workers as usize;
        if total > MAX_ATTEMPTS {
            return Err("Load permits at most 4096 attempts across all cases and workers");
        }
    }
    Ok(total)
}

/// Deterministic per-worker round-robin case order for a single phase. Workers
/// share a phase barrier; the actual concurrent dispatch order is observed.
pub fn worker_phase(
    limits: &[BenchmarkLimits],
    config: BenchmarkLoadConfig,
    warmup: bool,
) -> Result<Vec<u16>, &'static str> {
    planned_attempts(limits, config)?;
    let counts = limits
        .iter()
        .map(|limit| {
            if warmup {
                limit.warmups
            } else {
                limit.iterations
            }
        })
        .collect::<Vec<_>>();
    Ok((0..counts.iter().copied().max().unwrap_or(0))
        .flat_map(|round| {
            counts
                .iter()
                .enumerate()
                .filter_map(move |(case, &count)| (round < count).then_some(case as u16))
        })
        .collect())
}

pub fn minimum_start_spacing_ns(config: BenchmarkLoadConfig) -> Result<u64, &'static str> {
    validate_config(config)?;
    Ok(1_000_000_000u64.div_ceil(u64::from(config.max_starts_per_second)))
}

/// All arguments are bounded before allocations. A case saved alone can validate
/// its own traces; only a complete suite can validate cross-case worker overlap.
pub struct LoadCase<'a> {
    pub index: u16,
    pub samples: &'a [BenchmarkSample],
    pub context: &'a BenchmarkLoadContext,
}

pub fn validate_case(
    limits: &[BenchmarkLimits],
    actual_order: &[u16],
    case: &LoadCase<'_>,
) -> Result<(), &'static str> {
    let context = case.context;
    let planned = planned_attempts(limits, context.config)?;
    if context.worker_environments.len() != context.config.workers as usize
        || context
            .worker_environments
            .iter()
            .flatten()
            .any(|environment| {
                environment
                    .server_version
                    .as_ref()
                    .is_some_and(|version| version.len() > 256)
                    || [
                        &environment.preparation,
                        &environment.connection_reuse,
                        &environment.isolation,
                        &environment.session_settings,
                        &environment.cache_state,
                    ]
                    .iter()
                    .any(|text| text.len() > 128)
                    || environment
                        .observed_settings
                        .as_ref()
                        .is_some_and(|settings| !settings.is_bounded())
            })
    {
        return Err("Concurrent worker environment observations are not bounded");
    }
    let limit = limits
        .get(case.index as usize)
        .ok_or("Load case index is invalid")?;
    if actual_order.len() > planned
        || actual_order
            .iter()
            .any(|&index| index as usize >= limits.len())
        || case.samples.len() != context.traces.len()
        || case.samples.len()
            != actual_order
                .iter()
                .filter(|&&index| index == case.index)
                .count()
    {
        return Err("Load traces, samples and dispatch cases disagree");
    }
    if context
        .measured_window
        .is_some_and(|window| window.end_ns < window.start_ns)
    {
        return Err("Load measurement window is reversed");
    }
    let mut worker_counts =
        vec![(0u32, 0u32, None::<(u64, bool)>); context.config.workers as usize];
    let mut previous_dispatch: Option<u16> = None;
    let mut previous_trace: Option<(u64, u16)> = None;
    let mut measured_seen = false;
    let spacing = minimum_start_spacing_ns(context.config)?;
    for (ordinal, (sample, trace)) in case.samples.iter().zip(&context.traces).enumerate() {
        if sample.ordinal as usize != ordinal
            || trace.worker >= context.config.workers
            || trace.end_ns < trace.start_ns
            || trace.admission_wait_ns > trace.start_ns
            || sample.elapsed_ns != trace.end_ns - trace.start_ns
            || sample.client_elapsed_ns != Some(sample.elapsed_ns)
            || sample.database_execution_ns.is_some()
            || sample
                .first_row_ns
                .is_some_and(|first| first > sample.elapsed_ns)
            || sample
                .full_consumption_ns
                .is_some_and(|full| full > sample.elapsed_ns)
            || sample
                .first_row_ns
                .zip(sample.full_consumption_ns)
                .is_some_and(|(first, full)| first > full)
            || (sample.rows == Some(0) && sample.first_row_ns.is_some())
            || (sample.outcome == BenchmarkOutcome::Success
                && (sample.rows.is_none() || sample.full_consumption_ns.is_none()))
            || (sample.outcome != BenchmarkOutcome::Success
                && (sample.rows.is_some()
                    || sample.first_row_ns.is_some()
                    || sample.full_consumption_ns.is_some()))
            || actual_order.get(trace.dispatch_index as usize) != Some(&case.index)
            || previous_dispatch.is_some_and(|previous| trace.dispatch_index <= previous)
        {
            return Err("Load sample clocks, outcomes or dispatch identity are inconsistent");
        }
        if let Some((start, dispatch)) = previous_trace {
            if trace.start_ns < start
                || trace.start_ns - start < spacing * u64::from(trace.dispatch_index - dispatch)
            {
                return Err("Load dispatches violate the global start-rate ceiling");
            }
        }
        previous_trace = Some((trace.start_ns, trace.dispatch_index));
        previous_dispatch = Some(trace.dispatch_index);
        let (warmups, measured, previous) = &mut worker_counts[trace.worker as usize];
        if let Some((end, successful)) = *previous {
            if trace.start_ns < end || !successful {
                return Err("Load worker overlaps or continues after an unsuccessful attempt");
            }
        }
        *previous = Some((trace.end_ns, sample.outcome == BenchmarkOutcome::Success));
        if sample.warmup {
            if measured_seen || *warmups >= limit.warmups {
                return Err("Load warmup order or per-worker count is invalid");
            }
            *warmups += 1;
            if context.measured_window.is_some_and(|window| {
                trace.end_ns > window.start_ns || sample.outcome != BenchmarkOutcome::Success
            }) {
                return Err("Load measurement began before successful warmups completed");
            }
        } else {
            measured_seen = true;
            let window = context
                .measured_window
                .ok_or("Load measured attempt has no window")?;
            if *warmups != limit.warmups
                || *measured >= limit.iterations
                || trace.start_ns < window.start_ns
                || trace.end_ns > window.end_ns
            {
                return Err("Load measured attempt exceeds phase bounds or per-worker counts");
            }
            *measured += 1;
        }
    }
    if context.measured_window.is_some()
        && worker_counts
            .iter()
            .any(|(warmups, _, _)| *warmups != limit.warmups)
    {
        return Err("Load measurement requires every worker's successful warmups");
    }
    if observed_stop(case).is_some_and(|(stop, failed_index)| {
        context
            .traces
            .iter()
            .any(|trace| trace.dispatch_index > failed_index && trace.start_ns >= stop)
    }) {
        return Err("Load case dispatch follows an observed stop-run failure");
    }
    Ok(())
}

fn observed_stop(case: &LoadCase<'_>) -> Option<(u64, u16)> {
    case.samples
        .iter()
        .zip(&case.context.traces)
        .filter(|(sample, _)| {
            sample.outcome != BenchmarkOutcome::Success
                && (sample.warmup
                    || case.context.config.failure_policy == BenchmarkLoadFailurePolicy::StopRun)
        })
        .map(|(_, trace)| (trace.end_ns, trace.dispatch_index))
        .min()
}

/// Validates the exact global stream, both phase barriers, fixed per-worker case
/// schedule, start ceiling, failure policy and any claimed complete workload.
pub fn validate_suite(
    limits: &[BenchmarkLimits],
    actual_order: &[u16],
    cases: &[LoadCase<'_>],
    completed: bool,
) -> Result<(), &'static str> {
    let first = cases
        .first()
        .ok_or("Load suite requires case observations")?;
    let config = first.context.config;
    let planned = planned_attempts(limits, config)?;
    if cases.len() != limits.len() || actual_order.len() > planned {
        return Err("Load suite case coverage is incomplete");
    }
    let mut dispatches = vec![None; actual_order.len()];
    for (index, case) in cases.iter().enumerate() {
        if case.index as usize != index
            || case.context.config != config
            || case.context.measured_window != first.context.measured_window
            || case.context.worker_environments != first.context.worker_environments
        {
            return Err("Load cases disagree about configuration, phase or identity");
        }
        validate_case(limits, actual_order, case)?;
        for (sample, trace) in case.samples.iter().zip(&case.context.traces) {
            let slot = &mut dispatches[trace.dispatch_index as usize];
            if slot.replace((case.index, sample, trace)).is_some() {
                return Err("Load dispatch identity is repeated");
            }
        }
    }
    let warmup_schedule = worker_phase(limits, config, true)?;
    let measured_schedule = worker_phase(limits, config, false)?;
    let mut worker_states = vec![(0usize, 0usize, None::<(u64, bool)>); config.workers as usize];
    let mut previous_dispatch: Option<(u64, bool)> = None;
    // Outcomes are recorded in dispatch order, not completion order. Find the
    // earliest observed stop first so later-dispatched, faster failures count.
    let stop_ns = cases.iter().filter_map(observed_stop).min();
    let spacing = minimum_start_spacing_ns(config)?;
    for dispatch in dispatches {
        let (index, sample, trace) = dispatch.ok_or("Load dispatch has no observation")?;
        if stop_ns.is_some_and(|(stop, failed_index)| {
            trace.dispatch_index > failed_index && trace.start_ns >= stop
        }) {
            return Err("Load continues dispatch after a stop-run failure");
        }
        if let Some((start, warmup)) = previous_dispatch {
            if trace.start_ns < start || trace.start_ns - start < spacing {
                return Err("Load global dispatch clock or rate ceiling is invalid");
            }
            if !warmup && sample.warmup {
                return Err("Load warmups follow measured attempts");
            }
        }
        previous_dispatch = Some((trace.start_ns, sample.warmup));
        let (warmup_position, measured_position, previous) =
            &mut worker_states[trace.worker as usize];
        if previous.is_some_and(|(end, successful)| trace.start_ns < end || !successful) {
            return Err("Load worker overlaps across cases or continues after failure");
        }
        *previous = Some((trace.end_ns, sample.outcome == BenchmarkOutcome::Success));
        let (schedule, position) = if sample.warmup {
            (&warmup_schedule, warmup_position)
        } else {
            (&measured_schedule, measured_position)
        };
        if schedule.get(*position) != Some(&index) {
            return Err("Load worker departed from its fixed case schedule");
        }
        *position += 1;
    }
    if completed
        && (actual_order.len() != planned
            || first.context.measured_window.is_none()
            || cases.iter().any(|case| {
                case.samples
                    .iter()
                    .any(|sample| sample.outcome != BenchmarkOutcome::Success)
            }))
    {
        return Err("Load claimed completion without every successful attempt");
    }
    Ok(())
}

fn observe(outcomes: &mut BenchmarkLoadOutcomes, outcome: BenchmarkOutcome) {
    match outcome {
        BenchmarkOutcome::Success => outcomes.successful += 1,
        BenchmarkOutcome::Failed => outcomes.failed += 1,
        BenchmarkOutcome::TimedOut => outcomes.timed_out += 1,
        BenchmarkOutcome::Cancelled => outcomes.cancelled += 1,
    }
    outcomes.unattempted -= 1;
}

/// Recompute case metrics from validated raw observations, never saved summaries.
/// The denominator is the suite-wide measured window, including idle time.
pub fn summarize_case(
    limits: &[BenchmarkLimits],
    actual_order: &[u16],
    case: &LoadCase<'_>,
) -> Result<BenchmarkLoadSummary, &'static str> {
    validate_case(limits, actual_order, case)?;
    let limit = limits[case.index as usize];
    Ok(summarize_observations(
        limit.warmups * u32::from(case.context.config.workers),
        limit.iterations * u32::from(case.context.config.workers),
        case.context.config,
        case.context.measured_window,
        case.samples.iter().zip(&case.context.traces),
    ))
}

/// Recompute a private case's load metrics without trusting saved summaries.
pub fn report_summary(
    report: &sift_protocol::BenchmarkReport,
) -> Result<Option<BenchmarkLoadSummary>, &'static str> {
    let Some(suite) = report.suite_context.as_ref() else {
        return Ok(None);
    };
    let Some(load) = suite.load.as_ref() else {
        return Ok(None);
    };
    crate::performance_suite::validate_context(report)?;
    summarize_case(
        &suite.case_limits,
        &suite.actual_order,
        &LoadCase {
            index: suite.case_index,
            samples: &report.samples,
            context: load,
        },
    )
    .map(Some)
}

pub fn describe_summary(summary: &BenchmarkLoadSummary) -> String {
    let rate = |value: Option<u64>| {
        value.map_or_else(
            || "unavailable".into(),
            |value| format!("{}.{:03}/s", value / 1000, value % 1000),
        )
    };
    let ns =
        |value: Option<u64>| value.map_or_else(|| "unavailable".into(), |value| value.to_string());
    format!("Measured: {} successful, {} failed, {} timed out, {} cancelled, {} unattempted. Attempts: {}; successes: {} (shared measured suite window). Client peak: {}; client busy: {}. Admission wait mean/max: {}/{} ns. Database CPU/IO/saturation unavailable; outstanding native work after observer timeout is outside these clocks.",
        summary.measured.successful, summary.measured.failed, summary.measured.timed_out, summary.measured.cancelled, summary.measured.unattempted,
        rate(summary.attempts_per_second_milli), rate(summary.successes_per_second_milli), summary.client_peak_concurrency,
        summary.client_busy_permille.map_or_else(|| "unavailable".into(), |value| format!("{}.{:01}%",value/10,value%10)),
        ns(summary.admission_wait_mean_ns), ns(summary.admission_wait_max_ns))
}

/// Aggregate only outcomes, throughput and client occupancy across cases. This
/// deliberately exposes no pooled latency distribution for different workloads.
pub fn summarize_suite(
    limits: &[BenchmarkLimits],
    actual_order: &[u16],
    cases: &[LoadCase<'_>],
    completed: bool,
) -> Result<BenchmarkLoadSummary, &'static str> {
    validate_suite(limits, actual_order, cases, completed)?;
    let context = cases[0].context;
    Ok(summarize_observations(
        limits
            .iter()
            .map(|limit| limit.warmups * u32::from(context.config.workers))
            .sum(),
        limits
            .iter()
            .map(|limit| limit.iterations * u32::from(context.config.workers))
            .sum(),
        context.config,
        context.measured_window,
        cases
            .iter()
            .flat_map(|case| case.samples.iter().zip(&case.context.traces)),
    ))
}

fn summarize_observations<'a>(
    requested_warmups: u32,
    requested_measured: u32,
    config: BenchmarkLoadConfig,
    window: Option<BenchmarkLoadWindow>,
    observations: impl Iterator<Item = (&'a BenchmarkSample, &'a BenchmarkLoadTrace)>,
) -> BenchmarkLoadSummary {
    let mut warmup = BenchmarkLoadOutcomes {
        unattempted: requested_warmups,
        ..Default::default()
    };
    let mut measured = BenchmarkLoadOutcomes {
        unattempted: requested_measured,
        ..Default::default()
    };
    let mut events = Vec::new();
    let mut busy_ns = 0u128;
    let mut wait_ns = 0u128;
    let mut wait_max = None;
    for (sample, trace) in observations {
        observe(
            if sample.warmup {
                &mut warmup
            } else {
                &mut measured
            },
            sample.outcome,
        );
        if !sample.warmup {
            busy_ns += u128::from(sample.elapsed_ns);
            wait_ns += u128::from(trace.admission_wait_ns);
            wait_max = Some(wait_max.unwrap_or(0).max(trace.admission_wait_ns));
            // Half-open intervals: completion at t precedes admission at t.
            // A zero-duration observation consumes no measurable busy interval.
            if trace.end_ns > trace.start_ns {
                events.push((trace.start_ns, 1i16));
                events.push((trace.end_ns, -1i16));
            }
        }
    }
    events.sort_unstable();
    let (mut active, mut peak) = (0i16, 0i16);
    for (_, delta) in events {
        active += delta;
        peak = peak.max(active);
    }
    let attempts = measured.successful + measured.failed + measured.timed_out + measured.cancelled;
    let window_ns = window.map(|window| window.end_ns - window.start_ns);
    let rate = |count: u32| {
        window_ns.filter(|&window| window > 0).and_then(|window| {
            u64::try_from(u128::from(count) * 1_000_000_000_000 / u128::from(window)).ok()
        })
    };
    BenchmarkLoadSummary {
        warmup,
        measured,
        measured_window_ns: window_ns,
        attempts_per_second_milli: rate(attempts),
        successes_per_second_milli: rate(measured.successful),
        client_peak_concurrency: peak as u16,
        client_busy_permille: window_ns.filter(|&window| window > 0).map(|window| {
            (busy_ns * 1000 / (u128::from(window) * u128::from(config.workers))) as u16
        }),
        admission_wait_mean_ns: (attempts > 0).then(|| (wait_ns / u128::from(attempts)) as u64),
        admission_wait_max_ns: wait_max,
    }
}
