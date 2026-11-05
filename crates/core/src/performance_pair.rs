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
    if context.version != 1
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
                && a.order == b.order
                && a.actual_order == b.actual_order
                && a.pair_completed
                && b.pair_completed
        }
        _ => false,
    }
}

pub fn comparison_ready(report: &BenchmarkReport) -> bool {
    validate_context(report).is_ok()
        && report
            .pair_context
            .as_ref()
            .map_or(true, |context| context.pair_completed)
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
