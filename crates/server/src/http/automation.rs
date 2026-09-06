//! Automation HTTP handlers; shared admission and audit stay at the router boundary.

use super::*;

pub(super) async fn latest_successful_run_for_commit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(query): Query<LatestSuccessfulRunQuery>,
) -> ApiResult<Json<Option<sift_protocol::Run>>> {
    if !matches!(query.git_commit.len(), 40 | 64)
        || !query
            .git_commit
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(ApiError::BadRequest("invalid Git commit id".into()));
    }
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let workspace_id = workspace_id(id)?;
    authorize_run_configuration_operation(
        &state,
        &auth,
        workspace_id,
        None,
        RunConfigurationAction::Read,
    )?;
    let actor = auth.principal_id;
    let run = metadata_blocking(move || {
        metadata
            .latest_successful_run_for_commit(workspace_id, actor, &query.git_commit)
            .map_err(Into::into)
    })
    .await?;
    push_run_configuration_operation(
        &state,
        actor,
        workspace_id,
        None,
        RunConfigurationAction::Read,
    );
    Ok(Json(run))
}

pub(super) async fn list_run_configurations(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<RunConfiguration>>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let workspace_id = workspace_id(id)?;
    authorize_run_configuration_operation(
        &state,
        &auth,
        workspace_id,
        None,
        RunConfigurationAction::Read,
    )?;
    let actor = auth.principal_id;
    let configurations = metadata_blocking(move || {
        metadata
            .list_run_configurations_for_principal(workspace_id, actor)
            .map_err(Into::into)
    })
    .await?;
    push_run_configuration_operation(
        &state,
        actor,
        workspace_id,
        None,
        RunConfigurationAction::Read,
    );
    Ok(Json(configurations))
}

pub(super) async fn create_run_configuration(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(request): Json<CreateRunConfigurationRequest>,
) -> ApiResult<Json<RunConfiguration>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let workspace_id = workspace_id(id)?;
    authorize_run_configuration_operation(
        &state,
        &auth,
        workspace_id,
        None,
        RunConfigurationAction::Create,
    )?;
    let actor = auth.principal_id;
    let configuration = metadata_blocking(move || {
        metadata
            .create_run_configuration(workspace_id, actor, new_run_configuration(request))
            .map_err(Into::into)
    })
    .await?;
    push_run_configuration_operation(
        &state,
        actor,
        workspace_id,
        Some(configuration.id),
        RunConfigurationAction::Create,
    );
    Ok(Json(configuration))
}

pub(super) async fn get_run_configuration(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<RunConfiguration>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let configuration_id = run_configuration_id(id)?;
    let actor = auth.principal_id;
    let configuration = metadata_blocking(move || {
        metadata
            .run_configuration_for_principal(configuration_id, actor, false)
            .map_err(Into::into)
    })
    .await?;
    authorize_run_configuration_operation(
        &state,
        &auth,
        configuration.workspace_id,
        Some(configuration_id),
        RunConfigurationAction::Read,
    )?;
    push_run_configuration_operation(
        &state,
        actor,
        configuration.workspace_id,
        Some(configuration_id),
        RunConfigurationAction::Read,
    );
    Ok(Json(configuration))
}

pub(super) async fn update_run_configuration(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(request): Json<UpdateRunConfigurationRequest>,
) -> ApiResult<Json<RunConfiguration>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let configuration_id = run_configuration_id(id)?;
    let actor = auth.principal_id;
    let current = metadata_blocking({
        let metadata = metadata.clone();
        move || {
            metadata
                .run_configuration_for_principal(configuration_id, actor, true)
                .map_err(Into::into)
        }
    })
    .await?;
    authorize_run_configuration_operation(
        &state,
        &auth,
        current.workspace_id,
        Some(configuration_id),
        RunConfigurationAction::Update,
    )?;
    let workspace_id = current.workspace_id;
    let configuration = metadata_blocking(move || {
        metadata
            .update_run_configuration(
                configuration_id,
                actor,
                request.expected_revision,
                new_run_configuration(request.configuration),
            )
            .map_err(Into::into)
    })
    .await?;
    push_run_configuration_operation(
        &state,
        actor,
        workspace_id,
        Some(configuration_id),
        RunConfigurationAction::Update,
    );
    Ok(Json(configuration))
}

pub(super) async fn delete_run_configuration(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(request): Json<ExpectedRunConfigurationRevisionRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let configuration_id = run_configuration_id(id)?;
    let actor = auth.principal_id;
    let configuration = metadata_blocking({
        let metadata = metadata.clone();
        move || {
            metadata
                .run_configuration_for_principal(configuration_id, actor, true)
                .map_err(Into::into)
        }
    })
    .await?;
    authorize_run_configuration_operation(
        &state,
        &auth,
        configuration.workspace_id,
        Some(configuration_id),
        RunConfigurationAction::Delete,
    )?;
    metadata_blocking(move || {
        metadata
            .delete_run_configuration(configuration_id, actor, request.expected_revision)
            .map_err(Into::into)
    })
    .await?;
    push_run_configuration_operation(
        &state,
        actor,
        configuration.workspace_id,
        Some(configuration_id),
        RunConfigurationAction::Delete,
    );
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn validate_run_configuration(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<RunManifest>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let configuration_id = run_configuration_id(id)?;
    let actor = auth.principal_id;
    let rooms = state.rooms.clone();
    let (configuration, manifest) = metadata_blocking(move || {
        let configuration =
            metadata.run_configuration_for_principal(configuration_id, actor, false)?;
        let (manifest, _, _, _) = capture_run_payload(&metadata, &rooms, actor, &configuration)?;
        Ok::<_, ApiError>((configuration, manifest))
    })
    .await?;
    authorize_run_configuration_operation(
        &state,
        &auth,
        configuration.workspace_id,
        Some(configuration_id),
        RunConfigurationAction::Validate,
    )?;
    push_run_configuration_operation(
        &state,
        actor,
        configuration.workspace_id,
        Some(configuration_id),
        RunConfigurationAction::Validate,
    );
    Ok(Json(manifest))
}

pub(super) async fn start_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(request): Json<StartRunRequest>,
) -> ApiResult<Json<Run>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let configuration_id = run_configuration_id(id)?;
    let actor = auth.principal_id;
    let timeout = crate::run_executor::validate_timeout(request.timeout_secs)?;
    let configuration = metadata_blocking({
        let metadata = metadata.clone();
        move || {
            metadata
                .run_configuration_for_principal(configuration_id, actor, true)
                .map_err(Into::into)
        }
    })
    .await?;
    if configuration.revision != request.expected_configuration_revision {
        return Err(
            sift_metadata::MetadataError::RunConfigurationRevisionConflict {
                expected: request.expected_configuration_revision,
                current: configuration.revision,
            }
            .into(),
        );
    }
    authorize_run_operation(
        &state,
        &auth,
        configuration.workspace_id,
        None,
        RunAction::Start,
    )?;
    let workspace_lock = state.rooms.workspace_lock(configuration.workspace_id.0);
    let _workspace_guard = workspace_lock.lock().await;
    let rooms = state.rooms.clone();
    let expected_revision = request.expected_configuration_revision;
    let (configuration, record, room_id, tenant_id) = metadata_blocking({
        let metadata = metadata.clone();
        move || {
            let configuration =
                metadata.run_configuration_for_principal(configuration_id, actor, true)?;
            if configuration.revision != expected_revision {
                return Err(
                    sift_metadata::MetadataError::RunConfigurationRevisionConflict {
                        expected: expected_revision,
                        current: configuration.revision,
                    }
                    .into(),
                );
            }
            let (manifest, payload, room_id, tenant_id) =
                capture_run_payload(&metadata, &rooms, actor, &configuration)?;
            let record = metadata.create_run_execution(
                actor,
                NewRunExecution {
                    configuration_id,
                    trigger: RunTrigger::Interactive,
                    manifest,
                    resolved_scripts_json: serde_json::to_string(&payload).map_err(|_| {
                        ApiError::Internal("run manifest serialization failed".into())
                    })?,
                    previous_run_id: None,
                },
            )?;
            Ok::<_, ApiError>((configuration, record, room_id, tenant_id))
        }
    })
    .await?;
    drop(_workspace_guard);
    crate::run_executor::spawn_run(
        state.clone(),
        metadata,
        crate::run_executor::RunInvocation {
            actor,
            room_id,
            tenant_id,
            configuration: configuration.clone(),
            run_id: record.run.id,
            variables: request.variables,
            timeout,
        },
    );
    push_run_operation(
        &state,
        actor,
        configuration.workspace_id,
        Some(record.run.id),
        RunAction::Start,
    );
    Ok(Json(record.run))
}

pub(super) async fn get_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<Run>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let run_id = run_id(id)?;
    let actor = auth.principal_id;
    let (run, workspace_id) = metadata_blocking(move || {
        let record = metadata.run_execution_for_principal(run_id, actor, false)?;
        let configuration =
            metadata.run_configuration_for_principal(record.run.configuration_id, actor, false)?;
        Ok::<_, ApiError>((record.run, configuration.workspace_id))
    })
    .await?;
    authorize_run_operation(&state, &auth, workspace_id, Some(run_id), RunAction::Read)?;
    push_run_operation(&state, actor, workspace_id, Some(run_id), RunAction::Read);
    Ok(Json(run))
}

pub(super) async fn get_run_steps(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<RunStepResult>>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let run_id = run_id(id)?;
    let actor = auth.principal_id;
    let (steps, workspace_id) = metadata_blocking(move || {
        let record = metadata.run_execution_for_principal(run_id, actor, false)?;
        let configuration =
            metadata.run_configuration_for_principal(record.run.configuration_id, actor, false)?;
        Ok::<_, ApiError>((
            metadata.run_steps_for_principal(run_id, actor)?,
            configuration.workspace_id,
        ))
    })
    .await?;
    authorize_run_operation(&state, &auth, workspace_id, Some(run_id), RunAction::Read)?;
    Ok(Json(steps))
}

pub(super) async fn get_run_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(query): Query<RunLogQuery>,
) -> ApiResult<Json<Vec<RunLogEntry>>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let run_id = run_id(id)?;
    let actor = auth.principal_id;
    let (logs, workspace_id) = metadata_blocking(move || {
        let record = metadata.run_execution_for_principal(run_id, actor, false)?;
        let configuration =
            metadata.run_configuration_for_principal(record.run.configuration_id, actor, false)?;
        Ok::<_, ApiError>((
            metadata.run_logs_for_principal(run_id, actor, query.after, query.limit)?,
            configuration.workspace_id,
        ))
    })
    .await?;
    authorize_run_operation(&state, &auth, workspace_id, Some(run_id), RunAction::Read)?;
    Ok(Json(logs))
}

pub(super) async fn cancel_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<Run>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let run_id = run_id(id)?;
    let actor = auth.principal_id;
    let (run, workspace_id) = metadata_blocking({
        let metadata = metadata.clone();
        move || {
            let record = metadata.run_execution_for_principal(run_id, actor, true)?;
            let configuration = metadata.run_configuration_for_principal(
                record.run.configuration_id,
                actor,
                true,
            )?;
            Ok::<_, ApiError>((record.run, configuration.workspace_id))
        }
    })
    .await?;
    authorize_run_operation(&state, &auth, workspace_id, Some(run_id), RunAction::Cancel)?;
    let requested = metadata_blocking(move || {
        metadata
            .request_run_cancellation(run_id, actor)
            .map_err(Into::into)
    })
    .await?;
    if !state.rooms.cancel_run(run_id.0) && run.state != RunState::Queued {
        return Err(ApiError::BadRequest(
            "run is not active in this server generation".into(),
        ));
    }
    push_run_operation(&state, actor, workspace_id, Some(run_id), RunAction::Cancel);
    Ok(Json(requested))
}

pub(super) async fn rerun(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(request): Json<StartRunRequest>,
) -> ApiResult<Json<Run>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let previous_id = run_id(id)?;
    let actor = auth.principal_id;
    let timeout = crate::run_executor::validate_timeout(request.timeout_secs)?;
    let configuration = metadata_blocking({
        let metadata = metadata.clone();
        move || {
            let previous = metadata.run_execution_for_principal(previous_id, actor, true)?;
            let payload: crate::run_executor::ResolvedRunPayload =
                serde_json::from_str(&previous.resolved_scripts_json)
                    .map_err(|_| ApiError::Internal("stored run manifest is invalid".into()))?;
            Ok::<_, ApiError>(payload.configuration)
        }
    })
    .await?;
    if configuration.revision != request.expected_configuration_revision {
        return Err(
            sift_metadata::MetadataError::RunConfigurationRevisionConflict {
                expected: request.expected_configuration_revision,
                current: configuration.revision,
            }
            .into(),
        );
    }
    authorize_run_operation(
        &state,
        &auth,
        configuration.workspace_id,
        Some(previous_id),
        RunAction::Rerun,
    )?;
    let expected_revision = request.expected_configuration_revision;
    let (configuration, record, room_id, tenant_id) = metadata_blocking({
        let metadata = metadata.clone();
        move || {
            let previous = metadata.run_execution_for_principal(previous_id, actor, true)?;
            let payload: crate::run_executor::ResolvedRunPayload =
                serde_json::from_str(&previous.resolved_scripts_json)
                    .map_err(|_| ApiError::Internal("stored run manifest is invalid".into()))?;
            if payload.configuration.revision != expected_revision {
                return Err(
                    sift_metadata::MetadataError::RunConfigurationRevisionConflict {
                        expected: expected_revision,
                        current: payload.configuration.revision,
                    }
                    .into(),
                );
            }
            let workspace = metadata.get_workspace_for_principal(
                payload.configuration.workspace_id,
                actor,
                true,
            )?;
            let room = metadata.get_room(workspace.room_id)?;
            let record = metadata.create_run_execution(
                actor,
                NewRunExecution {
                    configuration_id: previous.run.configuration_id,
                    trigger: RunTrigger::Rerun,
                    manifest: previous.run.manifest,
                    resolved_scripts_json: previous.resolved_scripts_json,
                    previous_run_id: Some(previous_id),
                },
            )?;
            Ok::<_, ApiError>((payload.configuration, record, room.id, room.tenant_id))
        }
    })
    .await?;
    crate::run_executor::spawn_run(
        state.clone(),
        metadata,
        crate::run_executor::RunInvocation {
            actor,
            room_id,
            tenant_id,
            configuration: configuration.clone(),
            run_id: record.run.id,
            variables: request.variables,
            timeout,
        },
    );
    push_run_operation(
        &state,
        actor,
        configuration.workspace_id,
        Some(record.run.id),
        RunAction::Rerun,
    );
    Ok(Json(record.run))
}

pub(super) fn new_run_schedule(
    request: CreateRunScheduleRequest,
) -> ApiResult<sift_metadata::NewRunSchedule> {
    let next_fire_at = request
        .enabled
        .then(|| {
            crate::scheduler::next_cron_fire(&request.cron, &request.timezone, chrono::Utc::now())
        })
        .transpose()?;
    if !request.enabled {
        crate::scheduler::next_cron_fire(&request.cron, &request.timezone, chrono::Utc::now())?;
    }
    Ok(sift_metadata::NewRunSchedule {
        cron: request.cron,
        timezone: request.timezone,
        misfire_policy: request.misfire_policy,
        concurrency_policy: request.concurrency_policy,
        enabled: request.enabled,
        next_fire_at,
    })
}

pub(super) async fn list_run_schedules(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<RunSchedule>>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let configuration_id = run_configuration_id(id)?;
    let actor = auth.principal_id;
    let configuration = metadata_blocking({
        let metadata = metadata.clone();
        move || {
            metadata
                .run_configuration_for_principal(configuration_id, actor, false)
                .map_err(Into::into)
        }
    })
    .await?;
    authorize_schedule_operation(
        &state,
        &auth,
        configuration.workspace_id,
        None,
        ScheduleAction::Read,
    )?;
    let schedules = metadata_blocking(move || {
        metadata
            .list_run_schedules_for_principal(configuration_id, actor)
            .map_err(Into::into)
    })
    .await?;
    push_schedule_operation(
        &state,
        actor,
        configuration.workspace_id,
        None,
        ScheduleAction::Read,
    );
    Ok(Json(schedules))
}

pub(super) async fn create_run_schedule(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(request): Json<CreateRunScheduleRequest>,
) -> ApiResult<Json<RunSchedule>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let configuration_id = run_configuration_id(id)?;
    let actor = auth.principal_id;
    let configuration = metadata_blocking({
        let metadata = metadata.clone();
        move || {
            metadata
                .run_configuration_for_principal(configuration_id, actor, true)
                .map_err(Into::into)
        }
    })
    .await?;
    authorize_schedule_operation(
        &state,
        &auth,
        configuration.workspace_id,
        None,
        ScheduleAction::Create,
    )?;
    if configuration
        .variables
        .iter()
        .any(|variable| variable.required)
    {
        return Err(ApiError::BadRequest(
            "scheduled runs require stored variable bindings".into(),
        ));
    }
    let input = new_run_schedule(request)?;
    let schedule = metadata_blocking(move || {
        metadata
            .create_run_schedule(configuration_id, actor, input)
            .map_err(Into::into)
    })
    .await?;
    push_schedule_operation(
        &state,
        actor,
        configuration.workspace_id,
        Some(schedule.id),
        ScheduleAction::Create,
    );
    Ok(Json(schedule))
}

pub(super) async fn schedule_and_workspace(
    metadata: MetadataStore,
    schedule_id: ScheduleId,
    actor: PrincipalId,
    writable: bool,
) -> ApiResult<(RunSchedule, WorkspaceId)> {
    metadata_blocking(move || {
        let schedule = metadata.run_schedule_for_principal(schedule_id, actor, writable)?;
        let configuration =
            metadata.run_configuration_for_principal(schedule.configuration_id, actor, writable)?;
        Ok::<_, ApiError>((schedule, configuration.workspace_id))
    })
    .await
}

pub(super) async fn get_run_schedule(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<RunSchedule>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let id = schedule_id(id)?;
    let (schedule, workspace_id) =
        schedule_and_workspace(metadata, id, auth.principal_id, false).await?;
    authorize_schedule_operation(&state, &auth, workspace_id, Some(id), ScheduleAction::Read)?;
    push_schedule_operation(
        &state,
        auth.principal_id,
        workspace_id,
        Some(id),
        ScheduleAction::Read,
    );
    Ok(Json(schedule))
}

pub(super) async fn update_run_schedule(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(request): Json<UpdateRunScheduleRequest>,
) -> ApiResult<Json<RunSchedule>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let id = schedule_id(id)?;
    let (_, workspace_id) =
        schedule_and_workspace(metadata.clone(), id, auth.principal_id, true).await?;
    authorize_schedule_operation(
        &state,
        &auth,
        workspace_id,
        Some(id),
        ScheduleAction::Update,
    )?;
    let input = new_run_schedule(request.schedule)?;
    let actor = auth.principal_id;
    let schedule = metadata_blocking(move || {
        metadata
            .update_run_schedule(id, actor, request.expected_revision, input)
            .map_err(Into::into)
    })
    .await?;
    push_schedule_operation(
        &state,
        actor,
        workspace_id,
        Some(id),
        ScheduleAction::Update,
    );
    Ok(Json(schedule))
}

pub(super) async fn set_run_schedule_enabled(
    state: AppState,
    headers: HeaderMap,
    id: i64,
    request: ExpectedRunConfigurationRevisionRequest,
    enabled: bool,
) -> ApiResult<Json<RunSchedule>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let id = schedule_id(id)?;
    let (current, workspace_id) =
        schedule_and_workspace(metadata.clone(), id, auth.principal_id, true).await?;
    let action = if enabled {
        ScheduleAction::Enable
    } else {
        ScheduleAction::Disable
    };
    authorize_schedule_operation(&state, &auth, workspace_id, Some(id), action)?;
    let next_fire_at = enabled
        .then(|| {
            crate::scheduler::next_cron_fire(&current.cron, &current.timezone, chrono::Utc::now())
        })
        .transpose()?;
    let actor = auth.principal_id;
    let updated = metadata_blocking(move || {
        metadata
            .update_run_schedule(
                id,
                actor,
                request.expected_revision,
                sift_metadata::NewRunSchedule {
                    cron: current.cron,
                    timezone: current.timezone,
                    misfire_policy: current.misfire_policy,
                    concurrency_policy: current.concurrency_policy,
                    enabled,
                    next_fire_at,
                },
            )
            .map_err(Into::into)
    })
    .await?;
    push_schedule_operation(&state, actor, workspace_id, Some(id), action);
    Ok(Json(updated))
}

pub(super) async fn enable_run_schedule(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(request): Json<ExpectedRunConfigurationRevisionRequest>,
) -> ApiResult<Json<RunSchedule>> {
    set_run_schedule_enabled(state, headers, id, request, true).await
}

pub(super) async fn disable_run_schedule(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(request): Json<ExpectedRunConfigurationRevisionRequest>,
) -> ApiResult<Json<RunSchedule>> {
    set_run_schedule_enabled(state, headers, id, request, false).await
}

pub(super) async fn delete_run_schedule(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(request): Json<ExpectedRunConfigurationRevisionRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let id = schedule_id(id)?;
    let (_, workspace_id) =
        schedule_and_workspace(metadata.clone(), id, auth.principal_id, true).await?;
    authorize_schedule_operation(
        &state,
        &auth,
        workspace_id,
        Some(id),
        ScheduleAction::Delete,
    )?;
    let actor = auth.principal_id;
    metadata_blocking(move || {
        metadata
            .delete_run_schedule(id, actor, request.expected_revision)
            .map_err(Into::into)
    })
    .await?;
    push_schedule_operation(
        &state,
        actor,
        workspace_id,
        Some(id),
        ScheduleAction::Delete,
    );
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn list_schedule_occurrences(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(query): Query<ScheduleOccurrenceQuery>,
) -> ApiResult<Json<Vec<ScheduleOccurrence>>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let id = schedule_id(id)?;
    let (_, workspace_id) =
        schedule_and_workspace(metadata.clone(), id, auth.principal_id, false).await?;
    authorize_schedule_operation(&state, &auth, workspace_id, Some(id), ScheduleAction::Read)?;
    let actor = auth.principal_id;
    let occurrences = metadata_blocking(move || {
        metadata
            .list_schedule_occurrences_for_principal(id, actor, query.limit)
            .map_err(Into::into)
    })
    .await?;
    push_schedule_operation(&state, actor, workspace_id, Some(id), ScheduleAction::Read);
    Ok(Json(occurrences))
}

pub(super) async fn resume_schedule_occurrence(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<ScheduleOccurrence>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let occurrence_id = schedule_occurrence_id(id)?;
    let actor = auth.principal_id;
    let (occurrence, schedule, configuration) = metadata_blocking({
        let metadata = metadata.clone();
        move || {
            let occurrence =
                metadata.schedule_occurrence_for_principal(occurrence_id, actor, true)?;
            let schedule =
                metadata.run_schedule_for_principal(occurrence.schedule_id, actor, true)?;
            let configuration =
                metadata.run_configuration_for_principal(schedule.configuration_id, actor, true)?;
            Ok::<_, ApiError>((occurrence, schedule, configuration))
        }
    })
    .await?;
    authorize_schedule_operation(
        &state,
        &auth,
        configuration.workspace_id,
        Some(schedule.id),
        ScheduleAction::Resume,
    )?;
    if occurrence.run_id.is_some() {
        return Err(ApiError::BadRequest(
            "an occurrence with a run must use audited rerun".into(),
        ));
    }
    let resumed = metadata_blocking(move || {
        metadata
            .resume_schedule_occurrence(occurrence_id, actor)
            .map_err(Into::into)
    })
    .await?;
    push_schedule_operation(
        &state,
        actor,
        configuration.workspace_id,
        Some(schedule.id),
        ScheduleAction::Resume,
    );
    Ok(Json(resumed))
}

pub(super) fn new_transfer_recipe(
    request: CreateTransferRecipeRequest,
) -> sift_metadata::NewTransferRecipe {
    sift_metadata::NewTransferRecipe {
        name: request.name,
        direction: request.direction,
        source: request.source,
        sink: request.sink,
        format_id: request.format_id,
        format_version: request.format_version,
        options: request.options,
    }
}

pub(super) fn validate_transfer_format(
    state: &AppState,
    format_id: &str,
    format_version: &str,
    options: &serde_json::Value,
) -> ApiResult<()> {
    if matches!(
        format_id,
        "csv" | "tsv" | "jsonl" | "json_array" | "html" | "markdown" | "xlsx" | "sql" | "parquet"
    ) || state
        .sessions
        .formatter_registry()
        .validates(format_id, format_version, options)
    {
        Ok(())
    } else {
        Err(ApiError::BadRequest(
            "transfer format is not installed or its options are invalid".into(),
        ))
    }
}

pub(super) async fn list_transfer_recipes(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<TransferRecipe>>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let workspace_id = workspace_id(id)?;
    authorize_transfer_operation(
        &state,
        &auth,
        workspace_id,
        None,
        TransferRecipeAction::Read,
    )?;
    let actor = auth.principal_id;
    let recipes = metadata_blocking(move || {
        metadata
            .list_transfer_recipes_for_principal(workspace_id, actor)
            .map_err(Into::into)
    })
    .await?;
    push_transfer_operation(
        &state,
        actor,
        workspace_id,
        None,
        TransferRecipeAction::Read,
    );
    Ok(Json(recipes))
}

pub(super) async fn create_transfer_recipe(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(request): Json<CreateTransferRecipeRequest>,
) -> ApiResult<Json<TransferRecipe>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let workspace_id = workspace_id(id)?;
    authorize_transfer_operation(
        &state,
        &auth,
        workspace_id,
        None,
        TransferRecipeAction::Create,
    )?;
    validate_transfer_format(
        &state,
        &request.format_id,
        &request.format_version,
        &request.options,
    )?;
    let actor = auth.principal_id;
    let recipe = metadata_blocking(move || {
        metadata
            .create_transfer_recipe(workspace_id, actor, new_transfer_recipe(request))
            .map_err(Into::into)
    })
    .await?;
    push_transfer_operation(
        &state,
        actor,
        workspace_id,
        Some(recipe.id),
        TransferRecipeAction::Create,
    );
    Ok(Json(recipe))
}

pub(super) async fn get_transfer_recipe(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<TransferRecipe>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let id = transfer_recipe_id(id)?;
    let actor = auth.principal_id;
    let recipe = metadata_blocking(move || {
        metadata
            .transfer_recipe_for_principal(id, actor, false)
            .map_err(Into::into)
    })
    .await?;
    authorize_transfer_operation(
        &state,
        &auth,
        recipe.workspace_id,
        Some(id),
        TransferRecipeAction::Read,
    )?;
    push_transfer_operation(
        &state,
        actor,
        recipe.workspace_id,
        Some(id),
        TransferRecipeAction::Read,
    );
    Ok(Json(recipe))
}

pub(super) async fn update_transfer_recipe(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(request): Json<UpdateTransferRecipeRequest>,
) -> ApiResult<Json<TransferRecipe>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let id = transfer_recipe_id(id)?;
    let actor = auth.principal_id;
    let current = metadata_blocking({
        let metadata = metadata.clone();
        move || {
            metadata
                .transfer_recipe_for_principal(id, actor, true)
                .map_err(Into::into)
        }
    })
    .await?;
    authorize_transfer_operation(
        &state,
        &auth,
        current.workspace_id,
        Some(id),
        TransferRecipeAction::Update,
    )?;
    validate_transfer_format(
        &state,
        &request.recipe.format_id,
        &request.recipe.format_version,
        &request.recipe.options,
    )?;
    let workspace_id = current.workspace_id;
    let recipe = metadata_blocking(move || {
        metadata
            .update_transfer_recipe(
                id,
                actor,
                request.expected_revision,
                new_transfer_recipe(request.recipe),
            )
            .map_err(Into::into)
    })
    .await?;
    push_transfer_operation(
        &state,
        actor,
        workspace_id,
        Some(id),
        TransferRecipeAction::Update,
    );
    Ok(Json(recipe))
}

pub(super) async fn delete_transfer_recipe(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(request): Json<ExpectedTransferRecipeRevisionRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let id = transfer_recipe_id(id)?;
    let actor = auth.principal_id;
    let current = metadata_blocking({
        let metadata = metadata.clone();
        move || {
            metadata
                .transfer_recipe_for_principal(id, actor, true)
                .map_err(Into::into)
        }
    })
    .await?;
    authorize_transfer_operation(
        &state,
        &auth,
        current.workspace_id,
        Some(id),
        TransferRecipeAction::Delete,
    )?;
    metadata_blocking(move || {
        metadata
            .delete_transfer_recipe(id, actor, request.expected_revision)
            .map_err(Into::into)
    })
    .await?;
    push_transfer_operation(
        &state,
        actor,
        current.workspace_id,
        Some(id),
        TransferRecipeAction::Delete,
    );
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn validate_transfer_recipe(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<TransferRecipe>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let id = transfer_recipe_id(id)?;
    let actor = auth.principal_id;
    let recipe = metadata_blocking(move || {
        metadata
            .transfer_recipe_for_principal(id, actor, false)
            .map_err(Into::into)
    })
    .await?;
    authorize_transfer_operation(
        &state,
        &auth,
        recipe.workspace_id,
        Some(id),
        TransferRecipeAction::Validate,
    )?;
    validate_transfer_format(
        &state,
        &recipe.format_id,
        &recipe.format_version,
        &recipe.options,
    )?;
    push_transfer_operation(
        &state,
        actor,
        recipe.workspace_id,
        Some(id),
        TransferRecipeAction::Validate,
    );
    Ok(Json(recipe))
}

pub(super) async fn execute_transfer_recipe(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(request): Json<ExecuteTransferRecipeRequest>,
) -> ApiResult<Json<sift_protocol::TransferExecutionResult>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let id = transfer_recipe_id(id)?;
    let actor = auth.principal_id;
    let recipe = metadata_blocking({
        let metadata = metadata.clone();
        move || {
            metadata
                .transfer_recipe_for_principal(id, actor, false)
                .map_err(Into::into)
        }
    })
    .await?;
    authorize_transfer_operation(
        &state,
        &auth,
        recipe.workspace_id,
        Some(id),
        TransferRecipeAction::Execute,
    )?;
    validate_transfer_format(
        &state,
        &recipe.format_id,
        &recipe.format_version,
        &recipe.options,
    )?;
    let result =
        crate::transfer::execute_recipe(&state.sessions, &metadata, actor, &recipe, request)
            .await?;
    push_transfer_operation(
        &state,
        actor,
        recipe.workspace_id,
        Some(id),
        TransferRecipeAction::Execute,
    );
    Ok(Json(result))
}

pub(super) async fn get_workspace_artifact(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Response> {
    use axum::body::Body;
    use axum::response::IntoResponse;
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    if id <= 0 {
        return Err(ApiError::BadRequest("artifact id must be positive".into()));
    }
    let actor = auth.principal_id;
    let record = metadata_blocking(move || {
        metadata
            .workspace_artifact_for_principal(sift_protocol::WorkspaceArtifactId(id), actor)
            .map_err(Into::into)
    })
    .await?;
    authorize_transfer_operation(
        &state,
        &auth,
        record.artifact.workspace_id,
        None,
        TransferRecipeAction::Read,
    )?;
    let mut response = Body::from(record.content).into_response();
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        record
            .artifact
            .content_type
            .parse()
            .map_err(|_| ApiError::Internal("invalid artifact content type".into()))?,
    );
    response.headers_mut().insert(
        "x-content-sha256",
        record
            .artifact
            .digest
            .parse()
            .map_err(|_| ApiError::Internal("invalid artifact digest".into()))?,
    );
    Ok(response)
}
