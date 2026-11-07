//! Deterministic paired schedules and validation of unattested measurement lineage.
use sift_protocol::{
    BenchmarkLimits, BenchmarkPairOrder, BenchmarkPopulation as P, BenchmarkReport,
};

pub fn schedule(
    limits: &BenchmarkLimits,
    order: BenchmarkPairOrder,
) -> Result<Vec<P>, &'static str> {
    limits.validate()?;
    let mut result = Vec::with_capacity(2 * (limits.warmups + limits.iterations) as usize);
    let mut random = match order {
        BenchmarkPairOrder::Alternating => 0,
        BenchmarkPairOrder::RandomizedV1 { seed } => seed,
    };
    for rounds in [limits.warmups, limits.iterations] {
        for round in 0..rounds {
            let candidate_first = match order {
                BenchmarkPairOrder::Alternating => round % 2 == 1,
                BenchmarkPairOrder::RandomizedV1 { .. } => {
                    random = random.wrapping_add(0x9e3779b97f4a7c15);
                    let mut value = random;
                    value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
                    value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
                    (value ^ (value >> 31)) & 1 == 1
                }
            };
            result.extend(if candidate_first {
                [P::Candidate, P::Baseline]
            } else {
                [P::Baseline, P::Candidate]
            });
        }
    }
    Ok(result)
}

pub fn validate_context(report: &BenchmarkReport) -> Result<(), &'static str> {
    crate::performance_suite::validate_context(report)?;
    let Some(context) = &report.pair_context else {
        return Ok(());
    };
    let limits = BenchmarkLimits {
        warmups: report.warmups,
        iterations: report.requested_iterations,
        query_timeout_ms: report.query_timeout_ms,
        total_budget_ms: report.total_budget_ms,
        delay_ms: report.delay_ms,
    };
    let planned = schedule(&limits, context.order)?;
    validate_verification(context, report)?;
    if !matches!(context.version, 1 | 2)
        || context.run_id.is_nil()
        || context.actual_order.len() > planned.len()
        || context.actual_order != planned[..context.actual_order.len()]
        || context
            .actual_order
            .iter()
            .filter(|p| **p == context.population)
            .count()
            != report.samples.len()
        || (context.pair_completed
            && (!report.completed || context.actual_order.len() != planned.len()))
    {
        return Err("Invalid paired benchmark context or incomplete claimed schedule");
    }
    Ok(())
}

pub fn complementary(left: &BenchmarkReport, right: &BenchmarkReport) -> bool {
    match (&left.pair_context, &right.pair_context) {
        (Some(a), Some(b)) => {
            a.run_id == b.run_id
                && a.population != b.population
                && a.version == b.version
                && a.verification == b.verification
                && a.order == b.order
                && a.actual_order == b.actual_order
                && a.pair_completed
                && b.pair_completed
        }
        _ => false,
    }
}

pub fn comparison_compatible(left: &BenchmarkReport, right: &BenchmarkReport) -> bool {
    match (&left.pair_context, &right.pair_context) {
        (Some(a), Some(b)) if a.run_id == b.run_id && a.population != b.population => {
            complementary(left, right)
        }
        _ => true,
    }
}

pub fn comparison_ready(report: &BenchmarkReport) -> bool {
    validate_context(report).is_ok()
        && report
            .suite_context
            .as_ref()
            .map_or(true, |context| context.suite_completed)
        && report
            .pair_context
            .as_ref()
            .map_or(true, |context| context.pair_completed)
}

fn validate_verification(
    context: &sift_protocol::BenchmarkPairContext,
    report: &BenchmarkReport,
) -> Result<(), &'static str> {
    use sift_protocol::BenchmarkVerificationOutcome as O;
    let Some(check) = &context.verification else {
        return if context.version == 1 {
            Ok(())
        } else {
            Err("Missing paired verification")
        };
    };
    check.config.validate()?;
    use sift_protocol::{BenchmarkVerificationSnapshot as S, Engine};
    if !matches!(
        (report.engine, check.snapshot),
        (Engine::Postgres, S::ReadOnlyRepeatableRead)
            | (Engine::Sqlite, S::ReadOnlySerializable)
            | (Engine::SqlServer, S::IndependentReads)
    ) {
        return Err("Invalid paired snapshot policy for engine");
    }

    if context.version != 2 || check.version != 1 {
        return Err("Unsupported paired verification version");
    }
    for observation in [check.baseline, check.candidate].into_iter().flatten() {
        if observation.rows > u64::from(check.config.max_rows)
            || !(1..=2000).contains(&observation.columns)
            || observation.canonical_bytes == 0
            || observation.canonical_bytes > check.config.max_bytes
        {
            return Err("Invalid paired result observation");
        }
    }
    if matches!(check.outcome, O::Matched | O::Different) {
        if check.baseline.is_none()
            || check.candidate.is_none()
            || (check.outcome == O::Matched && check.baseline != check.candidate)
        {
            return Err("Missing or inconsistent paired result observations");
        }
    } else if check.candidate.is_some() {
        return Err("Unexpected completed candidate observation");
    }
    if check.outcome != O::Matched
        && (!report.samples.is_empty()
            || report.completed
            || context.pair_completed
            || !context.actual_order.is_empty())
    {
        return Err("Unverified paired workload contains timed attempts");
    }
    Ok(())
}
/// Fixed row-free summary; saved context remains caller supplied and unattested.
pub fn describe_verification(check: &sift_protocol::BenchmarkResultVerification) -> String {
    format!("Decoded-result check: {:?} ({:?}; {:?}); A {} rows, B {} rows; limits {} rows / {} canonical bytes per result. Untimed fingerprint observations; semantic and later timed-data equivalence remain unproven.",check.outcome,check.config.order,check.snapshot,check.baseline.map_or_else(||"unavailable".into(),|o|o.rows.to_string()),check.candidate.map_or_else(||"unavailable".into(),|o|o.rows.to_string()),check.config.max_rows,check.config.max_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schedules_balance_rounds_reset_alternation_and_replay_seed_zero() {
        let limits = BenchmarkLimits {
            warmups: 2,
            iterations: 3,
            ..Default::default()
        };
        assert_eq!(
            schedule(&limits, BenchmarkPairOrder::Alternating).unwrap(),
            [
                P::Baseline,
                P::Candidate,
                P::Candidate,
                P::Baseline,
                P::Baseline,
                P::Candidate,
                P::Candidate,
                P::Baseline,
                P::Baseline,
                P::Candidate
            ]
        );
        let random = schedule(&limits, BenchmarkPairOrder::RandomizedV1 { seed: 0 }).unwrap();
        assert_eq!(
            random,
            schedule(&limits, BenchmarkPairOrder::RandomizedV1 { seed: 0 }).unwrap()
        );
        assert_eq!(
            random,
            [
                P::Candidate,
                P::Baseline,
                P::Baseline,
                P::Candidate,
                P::Candidate,
                P::Baseline,
                P::Baseline,
                P::Candidate,
                P::Candidate,
                P::Baseline
            ]
        );
        for round in random.chunks_exact(2) {
            assert_ne!(round[0], round[1]);
        }
        assert_ne!(
            random,
            schedule(&limits, BenchmarkPairOrder::RandomizedV1 { seed: 1 }).unwrap()
        );
        assert!(schedule(
            &BenchmarkLimits {
                iterations: 0,
                ..limits
            },
            BenchmarkPairOrder::Alternating
        )
        .is_err());
    }
}
