//! Audited private benchmark definitions and explicit parameter-aware dispatch.
use super::*;
use sift_protocol::{
    ListBenchmarkDefinitionsRequest, RunBenchmarkDefinitionRequest, SaveBenchmarkDefinitionRequest,
    SavedBenchmarkDefinition, SavedBenchmarkDefinitionSummary, UpdateBenchmarkDefinitionRequest,
};

fn validate_definition(request: &SaveBenchmarkDefinitionRequest) -> ApiResult<()> {
    if request.name.trim().is_empty()
        || request.name.len() > 200
        || request.sql.is_empty()
        || request.sql.len() > 1024 * 1024
        || request.parameter_count > 256
    {
        return Err(ApiError::BadRequest(
            "invalid benchmark definition name, SQL or parameter count".into(),
        ));
    }
    request
        .limits
        .validate()
        .map_err(|error| ApiError::BadRequest(error.into()))?;
    crate::session::benchmark::validate_read(request.engine, &request.sql)?;
    Ok(())
}

pub(super) async fn save_benchmark_definition(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(tenant): Path<i64>,
    Json(request): Json<SaveBenchmarkDefinitionRequest>,
) -> ApiResult<Json<SavedBenchmarkDefinition>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let tenant = tenant_id(tenant)?;
    ensure_tenant(&auth, tenant)?;
    let saved = benchmark_library::audited(
        state,
        Operation::SaveBenchmarkDefinition {
            tenant_id: tenant.0,
        },
        auth.principal_id.0,
        async move {
            validate_definition(&request)?;
            let now = chrono::Utc::now();
            metadata
                .save_benchmark_definition(
                    tenant,
                    auth.principal_id,
                    SavedBenchmarkDefinition {
                        id: uuid::Uuid::new_v4(),
                        revision: 1,
                        created_at: now,
                        updated_at: now,
                        name: request.name.trim().to_owned(),
                        engine: request.engine,
                        sql: request.sql,
                        parameter_count: request.parameter_count,
                        limits: request.limits,
                    },
                )
                .await
                .map_err(Into::into)
        },
    )
    .await?;
    Ok(Json(saved))
}

pub(super) async fn list_benchmark_definitions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(tenant): Path<i64>,
    Query(request): Query<ListBenchmarkDefinitionsRequest>,
) -> ApiResult<Json<CursorPage<SavedBenchmarkDefinitionSummary>>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let tenant = tenant_id(tenant)?;
    ensure_tenant(&auth, tenant)?;
    let limit = request.limit.unwrap_or(50).clamp(1, 50);
    let page = benchmark_library::audited(
        state,
        Operation::ListBenchmarkDefinitions {
            tenant_id: tenant.0,
        },
        auth.principal_id.0,
        async move {
            let mut items = metadata
                .list_benchmark_definitions(tenant, auth.principal_id, request.cursor, limit + 1)
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

pub(super) async fn get_benchmark_definition(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((tenant, id)): Path<(i64, uuid::Uuid)>,
) -> ApiResult<Json<SavedBenchmarkDefinition>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let tenant = tenant_id(tenant)?;
    ensure_tenant(&auth, tenant)?;
    Ok(Json(
        benchmark_library::audited(
            state,
            Operation::GetBenchmarkDefinition {
                tenant_id: tenant.0,
            },
            auth.principal_id.0,
            async move {
                metadata
                    .get_benchmark_definition(tenant, auth.principal_id, id)
                    .await
                    .map_err(Into::into)
            },
        )
        .await?,
    ))
}

pub(super) async fn update_benchmark_definition(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((tenant, id)): Path<(i64, uuid::Uuid)>,
    Json(request): Json<UpdateBenchmarkDefinitionRequest>,
) -> ApiResult<Json<SavedBenchmarkDefinition>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let tenant = tenant_id(tenant)?;
    ensure_tenant(&auth, tenant)?;
    let saved = benchmark_library::audited(
        state,
        Operation::UpdateBenchmarkDefinition {
            tenant_id: tenant.0,
        },
        auth.principal_id.0,
        async move {
            validate_definition(&request.definition)?;
            let previous = metadata
                .get_benchmark_definition(tenant, auth.principal_id, id)
                .await?;
            if previous.revision != request.expected_revision {
                return Err(
                    sift_metadata::MetadataError::BenchmarkDefinitionRevisionConflict.into(),
                );
            }
            let revision = previous.revision.checked_add(1).ok_or_else(|| {
                ApiError::BadRequest("benchmark definition revision exhausted".into())
            })?;
            metadata
                .update_benchmark_definition(
                    tenant,
                    auth.principal_id,
                    SavedBenchmarkDefinition {
                        id,
                        revision,
                        created_at: previous.created_at,
                        updated_at: chrono::Utc::now(),
                        name: request.definition.name.trim().to_owned(),
                        engine: request.definition.engine,
                        sql: request.definition.sql,
                        parameter_count: request.definition.parameter_count,
                        limits: request.definition.limits,
                    },
                    request.expected_revision,
                )
                .await
                .map_err(Into::into)
        },
    )
    .await?;
    Ok(Json(saved))
}

pub(super) async fn delete_benchmark_definition(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((tenant, id)): Path<(i64, uuid::Uuid)>,
) -> ApiResult<Json<serde_json::Value>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let tenant = tenant_id(tenant)?;
    ensure_tenant(&auth, tenant)?;
    benchmark_library::audited(
        state,
        Operation::DeleteBenchmarkDefinition {
            tenant_id: tenant.0,
        },
        auth.principal_id.0,
        async move {
            metadata
                .delete_benchmark_definition(tenant, auth.principal_id, id)
                .await
                .map_err(Into::into)
        },
    )
    .await?;
    Ok(Json(serde_json::json!({"deleted":true})))
}

pub(super) async fn run_benchmark_definition(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((session, connection, id)): Path<(
        sift_protocol::SessionId,
        sift_protocol::ConnectionId,
        uuid::Uuid,
    )>,
    Json(request): Json<RunBenchmarkDefinitionRequest>,
) -> ApiResult<Json<sift_protocol::BenchmarkReport>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let tenant = tenant_id(request.tenant_id)?;
    ensure_tenant(&auth, tenant)?;
    if state.sessions.session_owner(session)? != Some(auth.principal_id)
        || state
            .sessions
            .managed_tenant_for_session(session)
            .is_some_and(|bound| bound != tenant)
    {
        return Err(ApiError::Forbidden(
            "benchmark connection belongs to another principal or tenant".into(),
        ));
    }
    let result = benchmark_library::audited(
        state.clone(),
        Operation::RunBenchmarkDefinition {
            session,
            connection,
            definition_id: id,
        },
        auth.principal_id.0,
        async move {
            let saved = metadata
                .get_benchmark_definition(tenant, auth.principal_id, id)
                .await?;
            if saved.revision != request.expected_revision {
                return Err(
                    sift_metadata::MetadataError::BenchmarkDefinitionRevisionConflict.into(),
                );
            }
            if request.params.len() != saved.parameter_count as usize {
                return Err(ApiError::BadRequest(
                    "bind value count does not match definition; supply current values".into(),
                ));
            }
            let entry = state.sessions.conn_entry(session, connection)?;
            if entry.driver.semantic_engine() != Some(saved.engine) {
                return Err(ApiError::BadRequest(
                    "connection engine does not match definition".into(),
                ));
            }
            state
                .sessions
                .benchmark(
                    session,
                    connection,
                    sift_protocol::BenchmarkRequest {
                        run_id: request.run_id,
                        sql: saved.sql,
                        params: request.params,
                        warmups: saved.limits.warmups,
                        iterations: saved.limits.iterations,
                        query_timeout_ms: saved.limits.query_timeout_ms,
                        total_budget_ms: saved.limits.total_budget_ms,
                        delay_ms: saved.limits.delay_ms,
                        workload_confirmed: request.workload_confirmed,
                    },
                )
                .await
        },
    )
    .await?;
    Ok(Json(result))
}
