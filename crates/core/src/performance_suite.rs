//! Bounded explicit matrices and validation of portable, unattested lineage.
use sift_protocol::{BenchmarkReport, BenchmarkSuiteRequest};
use std::collections::HashSet;

pub const MAX_CASES: usize = 32;
pub const MAX_ATTEMPTS: usize = 4096;

pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub fn validate(request: &BenchmarkSuiteRequest) -> Result<usize, &'static str> {
    request.limits.validate()?;
    if request.run_id.is_nil() || !request.workload_confirmed {
        return Err(
            "A non-nil suite identity and explicit whole-workload confirmation are required",
        );
    }
    if request.queries.is_empty() || request.queries.len() > MAX_CASES {
        return Err("A suite requires 1–32 queries and 1–32 total cases");
    }
    let mut queries = HashSet::new();
    let mut count = 0;
    let mut sql_bytes = 0usize;
    let mut parameter_bytes = 0usize;
    for query in &request.queries {
        if !valid_id(&query.id)
            || !queries.insert(&query.id)
            || query.sql.trim().is_empty()
            || query.cases.is_empty()
        {
            return Err("Queries require unique bounded IDs, nonblank SQL and explicit cases");
        }
        let mut cases = HashSet::new();
        for case in &query.cases {
            count += 1;
            if count > MAX_CASES
                || !valid_id(&case.id)
                || !cases.insert(&case.id)
                || case.params.len() > 256
            {
                return Err("Cases require unique bounded IDs, at most 256 parameters and at most 32 total cases");
            }
            let limits = case.limits.unwrap_or(request.limits);
            limits.validate()?;
            if limits.total_budget_ms > request.limits.total_budget_ms {
                return Err("Case budget must not exceed the shared suite budget");
            }
            sql_bytes = sql_bytes.saturating_add(query.sql.len());
            parameter_bytes = parameter_bytes.saturating_add(
                serde_json::to_vec(&case.params)
                    .map_err(|_| "Cannot encode suite parameters")?
                    .len(),
            );
            if sql_bytes > 1024 * 1024 || parameter_bytes > 256 * 1024 {
                return Err("Expanded suite SQL exceeds 1 MiB or parameter data exceeds 256 KiB");
            }
        }
    }
    schedule_limits(&effective_limits(request))?;
    Ok(count)
}

pub fn effective_limits(request: &BenchmarkSuiteRequest) -> Vec<sift_protocol::BenchmarkLimits> {
    request
        .queries
        .iter()
        .flat_map(|query| query.cases.iter())
        .map(|case| case.limits.unwrap_or(request.limits))
        .collect()
}

pub fn schedule_limits(
    limits: &[sift_protocol::BenchmarkLimits],
) -> Result<Vec<u16>, &'static str> {
    if !(1..=MAX_CASES).contains(&limits.len()) {
        return Err("A suite requires 1–32 cases");
    }
    let mut order = Vec::new();
    for (index, limits) in limits.iter().enumerate() {
        limits.validate()?;
        let attempts = (limits.warmups as usize) + (limits.iterations as usize);
        if attempts > MAX_ATTEMPTS - order.len() {
            return Err("A suite permits at most 4096 total attempts");
        }
        order.extend(std::iter::repeat(index as u16).take(attempts));
    }
    Ok(order)
}

pub fn schedule(count: usize, warmups: u32, iterations: u32) -> Result<Vec<u16>, &'static str> {
    let attempts = (warmups as usize)
        .checked_add(iterations as usize)
        .ok_or("Invalid suite attempts")?;
    let total = count
        .checked_mul(attempts)
        .ok_or("Invalid suite attempts")?;
    if !(1..=MAX_CASES).contains(&count) || iterations == 0 || total > MAX_ATTEMPTS {
        return Err("A suite requires 1–32 cases and at most 4096 total attempts");
    }
    Ok((0..count)
        .flat_map(|index| std::iter::repeat(index as u16).take(attempts))
        .collect())
}

pub fn validate_context(report: &BenchmarkReport) -> Result<(), &'static str> {
    let Some(context) = &report.suite_context else {
        return Ok(());
    };
    let planned = match context.version {
        1 if context.case_limits.is_empty() && context.shared_total_budget_ms.is_none() => {
            schedule(
                context.case_count as usize,
                report.warmups,
                report.requested_iterations,
            )?
        }
        2 => {
            let shared = context
                .shared_total_budget_ms
                .ok_or("Suite shared budget is missing")?;
            let limits = context
                .case_limits
                .get(context.case_index as usize)
                .ok_or("Suite case limits are missing")?;
            if !(1..=3_600_000).contains(&shared)
                || context.case_limits.len() != context.case_count as usize
                || context
                    .case_limits
                    .iter()
                    .any(|limits| limits.total_budget_ms > shared)
                || limits.warmups != report.warmups
                || limits.iterations != report.requested_iterations
                || limits.query_timeout_ms != report.query_timeout_ms
                || limits.total_budget_ms != report.total_budget_ms
                || limits.delay_ms != report.delay_ms
            {
                return Err("Suite case limits differ from its report or shared budget");
            }
            schedule_limits(&context.case_limits)?
        }
        _ => return Err("Invalid suite lineage version or fields"),
    };
    if report.pair_context.is_some()
        || context.run_id.is_nil()
        || !valid_id(&context.query_id)
        || !valid_id(&context.case_id)
        || context.case_index >= context.case_count
        || context.actual_order.len() > planned.len()
        || context.actual_order != planned[..context.actual_order.len()]
        || context
            .actual_order
            .iter()
            .filter(|&&i| i == context.case_index)
            .count()
            != report.samples.len()
        || (context.suite_completed
            && (!report.completed || context.actual_order.len() != planned.len()))
    {
        return Err("Invalid suite lineage or incomplete claimed schedule");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_protocol::*;
    fn request() -> BenchmarkSuiteRequest {
        BenchmarkSuiteRequest {
            run_id: uuid::Uuid::new_v4(),
            workload_confirmed: true,
            limits: BenchmarkLimits {
                warmups: 1,
                iterations: 2,
                ..Default::default()
            },
            queries: vec![BenchmarkSuiteQuery {
                id: "query".into(),
                sql: "SELECT $1".into(),
                cases: vec![
                    BenchmarkParameterCase {
                        limits: None,
                        id: "one".into(),
                        params: vec![Value::Int64(1)],
                    },
                    BenchmarkParameterCase {
                        limits: None,
                        id: "two".into(),
                        params: vec![Value::Int64(2)],
                    },
                ],
            }],
        }
    }
    #[test]
    fn explicit_matrix_bounds_and_stable_case_schedule() {
        let source = request();
        assert_eq!(validate(&source).unwrap(), 2);
        assert_eq!(schedule(2, 1, 2).unwrap(), vec![0, 0, 0, 1, 1, 1]);
        for invalid in 0..8 {
            let mut request = source.clone();
            match invalid {
                0 => request.workload_confirmed = false,
                1 => request.queries[0].cases[1].id = "one".into(),
                2 => request.queries[0].id = "bad/id".into(),
                3 => request.limits.iterations = 4096,
                4 => request.queries[0].sql = "x".repeat(512 * 1024 + 1),
                5 => request.queries[0].cases[0].params = vec![Value::Null; 257],
                6 => request.queries[0].cases[0].params = vec![Value::Text("x".repeat(256 * 1024))],
                _ => request.queries[0].cases.clear(),
            }
            assert!(validate(&request).is_err(), "case {invalid}");
        }
        assert!(schedule(32, 0, 128).is_ok());
        assert!(schedule(32, 1, 128).is_err());
        assert!(schedule(33, 0, 1).is_err());
    }
    #[test]
    fn heterogeneous_limits_validate_schedule_and_both_lineage_versions() {
        let mut request = request();
        let custom = BenchmarkLimits {
            warmups: 0,
            iterations: 1,
            query_timeout_ms: 10,
            total_budget_ms: 100,
            delay_ms: 0,
        };
        request.queries[0].cases[1].limits = Some(custom);
        assert_eq!(validate(&request).unwrap(), 2);
        let limits = effective_limits(&request);
        assert_eq!(limits, vec![request.limits, custom]);
        assert_eq!(schedule_limits(&limits).unwrap(), vec![0, 0, 0, 1]);
        for invalid in 0..4 {
            let mut bad = request.clone();
            let limits = bad.queries[0].cases[1].limits.as_mut().unwrap();
            match invalid {
                0 => limits.iterations = 0,
                1 => limits.total_budget_ms = request.limits.total_budget_ms + 1,
                2 => limits.delay_ms = limits.total_budget_ms,
                _ => limits.iterations = 4096,
            }
            assert!(validate(&bad).is_err());
        }
        let mut report: BenchmarkReport = serde_json::from_value(serde_json::json!({
            "version":2,"run_id":uuid::Uuid::new_v4(),"engine":"postgres","sql":"SELECT 1",
            "captured_at":"2026-11-06T00:00:00Z","warmups":0,"requested_iterations":1,
            "query_timeout_ms":10,"total_budget_ms":100,"delay_ms":0,"parameter_count":0,
            "completed":true,"warnings":[],"median_ns":1.0,
            "samples":[{"ordinal":0,"warmup":false,"outcome":"success","elapsed_ns":1,"rows":1}]
        }))
        .unwrap();
        report.suite_context = Some(Box::new(BenchmarkSuiteContext {
            version: 2,
            run_id: request.run_id,
            query_id: "query".into(),
            case_id: "two".into(),
            case_index: 1,
            case_count: 2,
            actual_order: vec![0, 0, 0, 1],
            suite_completed: true,
            case_limits: limits,
            shared_total_budget_ms: Some(request.limits.total_budget_ms),
        }));
        validate_context(&report).unwrap();
        for invalid in 0..6 {
            let mut bad = report.clone();
            let context = bad.suite_context.as_mut().unwrap();
            match invalid {
                0 => context.case_limits[1].query_timeout_ms += 1,
                1 => context.case_limits[0].iterations += 1,
                2 => context.shared_total_budget_ms = Some(1),
                3 => {
                    context.case_limits.pop();
                }
                4 => context.actual_order[0] = 1,
                _ => context.version = 1,
            }
            assert!(validate_context(&bad).is_err(), "corruption {invalid}");
        }
        let context = report.suite_context.as_mut().unwrap();
        context.version = 1;
        context.case_limits.clear();
        context.shared_total_budget_ms = None;
        context.actual_order = vec![0, 1];
        validate_context(&report).unwrap();
        let serialized = serde_json::to_value(&report).unwrap();
        assert!(serialized["suite_context"].get("case_limits").is_none());
        let legacy: BenchmarkReport = serde_json::from_value(serialized).unwrap();
        validate_context(&legacy).unwrap();
        let imported = crate::performance::artifact::import(
            crate::performance::artifact::export(&legacy, true),
            uuid::Uuid::new_v4(),
        )
        .unwrap();
        validate_context(&imported).unwrap();
    }
}
