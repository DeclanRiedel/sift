//! Authenticated private snapshot storage, independent of live SQL sessions.
use super::*;
use sift_protocol::{
    BenchmarkLimits, BenchmarkOutcome, BenchmarkReport, ListBenchmarkRunsRequest,
    SaveBenchmarkRunRequest, SavedBenchmarkRun, SavedBenchmarkRunSummary,
};

async fn audited<T: Send + 'static>(
    state: AppState,
    operation: Operation,
    actor: i64,
    future: impl std::future::Future<Output = ApiResult<T>> + Send + 'static,
) -> ApiResult<T> {
    // Finish persistence and auditing even if the HTTP caller disconnects.
    tokio::spawn(async move {
        let result = future.await;
        finish_operation_as(&state.sessions, operation, result, Some(actor), |_| None)
    })
    .await
    .map_err(|_| ApiError::Internal("saved benchmark task failed".into()))?
}

pub(super) async fn save_benchmark_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(tenant): Path<i64>,
    Json(mut request): Json<SaveBenchmarkRunRequest>,
) -> ApiResult<Json<SavedBenchmarkRun>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let tenant = tenant_id(tenant)?;
    ensure_tenant(&auth, tenant)?;
    let operation = Operation::SaveBenchmarkRun {
        tenant_id: tenant.0,
    };
    let saved = audited(state, operation, auth.principal_id.0, async move {
        if request.name.trim().is_empty() || request.name.len() > 200 {
            return Err(ApiError::BadRequest("name must be 1–200 bytes".into()));
        }
        normalize_report(&mut request.report)?;
        let saved = SavedBenchmarkRun {
            id: uuid::Uuid::new_v4(),
            saved_at: chrono::Utc::now(),
            name: request.name.trim().to_string(),
            report: request.report,
        };
        metadata
            .save_benchmark_run(tenant, auth.principal_id, saved)
            .await
            .map_err(Into::into)
    })
    .await?;
    Ok(Json(saved))
}

pub(super) async fn list_benchmark_runs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(tenant): Path<i64>,
    Query(request): Query<ListBenchmarkRunsRequest>,
) -> ApiResult<Json<CursorPage<SavedBenchmarkRunSummary>>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let tenant = tenant_id(tenant)?;
    ensure_tenant(&auth, tenant)?;
    let limit = request.limit.unwrap_or(50).clamp(1, 50);
    let page = audited(
        state,
        Operation::ListBenchmarkRuns {
            tenant_id: tenant.0,
        },
        auth.principal_id.0,
        async move {
            let mut items = metadata
                .list_benchmark_runs(tenant, auth.principal_id, request.cursor, limit + 1)
                .await?;
            let more = items.len() > limit as usize;
            items.truncate(limit as usize);
            let next_cursor = more.then(|| items.last().unwrap().id.to_string());
            Ok(CursorPage { items, next_cursor })
        },
    )
    .await?;
    Ok(Json(page))
}

pub(super) async fn get_benchmark_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((tenant, id)): Path<(i64, uuid::Uuid)>,
) -> ApiResult<Json<SavedBenchmarkRun>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let tenant = tenant_id(tenant)?;
    ensure_tenant(&auth, tenant)?;
    Ok(Json(
        audited(
            state,
            Operation::GetBenchmarkRun {
                tenant_id: tenant.0,
            },
            auth.principal_id.0,
            async move {
                metadata
                    .get_benchmark_run(tenant, auth.principal_id, id)
                    .await
                    .map_err(Into::into)
            },
        )
        .await?,
    ))
}

pub(super) async fn delete_benchmark_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((tenant, id)): Path<(i64, uuid::Uuid)>,
) -> ApiResult<Json<serde_json::Value>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let tenant = tenant_id(tenant)?;
    ensure_tenant(&auth, tenant)?;
    audited(
        state,
        Operation::DeleteBenchmarkRun {
            tenant_id: tenant.0,
        },
        auth.principal_id.0,
        async move {
            metadata
                .delete_benchmark_run(tenant, auth.principal_id, id)
                .await
                .map_err(Into::into)
        },
    )
    .await?;
    Ok(Json(serde_json::json!({"deleted":true})))
}

fn normalize_report(report: &mut BenchmarkReport) -> ApiResult<()> {
    use sift_core::performance::{summarize, SampleOutcome, SamplePhase, TimingSample};
    let limits = BenchmarkLimits {
        warmups: report.warmups,
        iterations: report.requested_iterations,
        query_timeout_ms: report.query_timeout_ms,
        total_budget_ms: report.total_budget_ms,
        delay_ms: report.delay_ms,
    };
    limits
        .validate()
        .map_err(|error| ApiError::BadRequest(error.into()))?;
    if report.version != 1
        || report.sql.len() > 1024 * 1024
        || report.warnings.len() > 128
        || report.warnings.iter().any(|warning| warning.len() > 4096)
        || report.samples.len() > (limits.warmups + limits.iterations) as usize
    {
        return Err(ApiError::BadRequest(
            "unsupported or oversized benchmark report".into(),
        ));
    }
    let mut samples = Vec::with_capacity(report.samples.len());
    for (index, sample) in report.samples.iter().enumerate() {
        if sample.ordinal as usize != index
            || sample.warmup != (index < limits.warmups as usize)
            || sample.first_row_ns.is_some_and(|ns| ns > sample.elapsed_ns)
            || (sample.outcome != BenchmarkOutcome::Success
                && (sample.rows.is_some() || sample.first_row_ns.is_some()))
        {
            return Err(ApiError::BadRequest("inconsistent benchmark sample".into()));
        }
        samples.push(TimingSample {
            phase: if sample.warmup {
                SamplePhase::Warmup
            } else {
                SamplePhase::Measured
            },
            outcome: match sample.outcome {
                BenchmarkOutcome::Success => SampleOutcome::Success,
                BenchmarkOutcome::Failed => SampleOutcome::Failed,
                BenchmarkOutcome::TimedOut => SampleOutcome::TimedOut,
                BenchmarkOutcome::Cancelled => SampleOutcome::Cancelled,
            },
            elapsed_ns: Some(sample.elapsed_ns),
        });
    }
    report.completed = report.samples.len() == (limits.warmups + limits.iterations) as usize
        && report
            .samples
            .iter()
            .all(|s| s.outcome == BenchmarkOutcome::Success);
    let distribution = summarize(&samples).distribution;
    report.median_ns = distribution.as_ref().map(|d| d.median_ns);
    report.mean_ns = distribution.as_ref().map(|d| d.mean_ns);
    report.min_ns = distribution.as_ref().map(|d| d.min_ns);
    report.max_ns = distribution.as_ref().map(|d| d.max_ns);
    report.standard_deviation_ns = distribution.as_ref().and_then(|d| d.standard_deviation_ns);
    report.p95_ns = distribution.as_ref().and_then(|d| d.p95_ns);
    report.p99_ns = distribution.as_ref().and_then(|d| d.p99_ns);
    Ok(())
}
