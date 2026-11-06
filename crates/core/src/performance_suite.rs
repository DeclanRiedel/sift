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
    schedule(count, request.limits.warmups, request.limits.iterations)?;
    Ok(count)
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
    let planned = schedule(
        context.case_count as usize,
        report.warmups,
        report.requested_iterations,
    )?;
    if report.pair_context.is_some()
        || context.version != 1
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
                        id: "one".into(),
                        params: vec![Value::Int64(1)],
                    },
                    BenchmarkParameterCase {
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
}
