//! Portable private measurement exchange. Import never executes a workload.
use sift_protocol::{BenchmarkArtifact, BenchmarkOutcome, BenchmarkReport};

pub fn export(report: &BenchmarkReport, redact_sql: bool) -> BenchmarkArtifact {
    let mut report = report.clone();
    if redact_sql {
        report.sql.clear();
    }
    BenchmarkArtifact {
        artifact_version: 1,
        sql_redacted: redact_sql,
        report,
    }
}

pub fn import(
    mut artifact: BenchmarkArtifact,
    imported_run_id: uuid::Uuid,
) -> Result<BenchmarkReport, &'static str> {
    if artifact.artifact_version != 1 || !matches!(artifact.report.version, 1 | 2) {
        return Err("unsupported benchmark artifact/report version");
    }
    if artifact.sql_redacted && !artifact.report.sql.is_empty() {
        return Err("SQL-redacted artifact must contain no SQL");
    }
    if imported_run_id == artifact.report.run_id || artifact.report.warnings.len() >= 128 {
        return Err("import needs a fresh run identity and room for source provenance warning");
    }
    artifact.report.warnings.push(format!(
        "Imported user-supplied measurement from source run {}. Local run {} identifies an imported snapshot, not a new execution; source provenance is unverified.",
        artifact.report.run_id, imported_run_id,
    ));
    artifact.report.run_id = imported_run_id;
    Ok(artifact.report)
}

/// Only numbers and fixed labels are emitted, avoiding spreadsheet formula text.
/// Blank optional fields retain unavailable clocks and incomplete row counts.
pub fn samples_csv(report: &BenchmarkReport) -> String {
    use std::fmt::Write;
    let mut output = String::from("ordinal,warmup,outcome,client_elapsed_ns,database_execution_ns,first_row_ns,full_consumption_ns,rows\n");
    let optional = |value: Option<u64>| value.map(|value| value.to_string()).unwrap_or_default();
    for sample in &report.samples {
        let outcome = match sample.outcome {
            BenchmarkOutcome::Success => "success",
            BenchmarkOutcome::Failed => "failed",
            BenchmarkOutcome::TimedOut => "timed_out",
            BenchmarkOutcome::Cancelled => "cancelled",
        };
        writeln!(
            output,
            "{},{},{},{},{},{},{},{}",
            sample.ordinal,
            sample.warmup,
            outcome,
            sample.client_elapsed_ns.unwrap_or(sample.elapsed_ns),
            optional(sample.database_execution_ns),
            optional(sample.first_row_ns),
            optional(sample.full_consumption_ns),
            optional(sample.rows)
        )
        .expect("string write");
    }
    output
}

/// Compact shareable text omits SQL, names and free-form environment/warnings.
/// Statistics are recomputed from samples rather than trusting saved summaries.
pub fn readable_report(report: &BenchmarkReport) -> String {
    let statistics = super::benchmark_statistics(report);
    let mut output=format!(
        "Sift benchmark measurement\nEngine: {}\nRun: {}\nReport version: {}\nTiming: server-side client elapsed (ns)\nCompleted: {}\nMeasured successful: {}\nFailed: {}\nTimed out: {}\nCancelled: {}\nMedian: {}\nWarm-ups excluded. Failed samples excluded from latency; outcome counts retained.\nSnapshot is user-saved, not a provenance attestation. Cache/load/data equivalence unproven.\n",
        report.engine, report.run_id, report.version, report.completed,
        statistics.successful, statistics.failed, statistics.timed_out, statistics.cancelled,
        statistics.median_ns.map_or_else(|| "unavailable".into(), |value| format!("{value:.2}")),
    );
    if let Some(pair) = &report.pair_context {
        output.push_str(&format!("Paired population: {:?}\nPair: {}\nOrder: {:?}\nPair completed: {}\nActual order: {}\nShared-session/cache effects possible; pair context is not an attestation.\n",pair.population,pair.run_id,pair.order,pair.pair_completed,pair.actual_order.iter().map(|population|if *population==sift_protocol::BenchmarkPopulation::Baseline {"A"}else{"B"}).collect::<Vec<_>>().join(" ")));
    }
    if let Some(suite) = &report.suite_context {
        output.push_str(&format!("Suite: {}\nCase index: {} of {}\nSuite completed: {}\nActual attempts: {}\nSerial cases share session/cache state; suite context is not an attestation.\n",suite.run_id,suite.case_index,suite.case_count,suite.suite_completed,suite.actual_order.len()));
    }
    output
}
