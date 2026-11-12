//! Execution HTTP handlers; shared admission and audit stay at the router boundary.

use super::*;

pub(super) async fn execute_query(
    State(state): State<AppState>,
    headers: HeaderMap,
    auth: Option<Extension<AuthContext>>,
    Path(id): Path<sift_protocol::SessionId>,
    Json(req): Json<ExecuteRequestHttp>,
) -> ApiResult<Response> {
    let actor = auth
        .as_ref()
        .map(|Extension(auth)| auth.principal_id.0)
        .or_else(|| {
            state
                .sessions
                .session_owner(id)
                .ok()
                .flatten()
                .map(|owner| owner.0)
        });
    let mut audit = query_audit::QueryOperationAudit::new(
        state.sessions.clone(),
        Operation::ExecuteQuery {
            session: id,
            request: req.clone(),
        },
        actor,
    );
    let prepared = async {
        let permit = state.sessions.reserve_query_settlement()?;
        let source = if req.room_id.is_none() {
            Some(state.sessions.conn_entry(id, req.connection)?)
        } else {
            None
        };
        let context = execute_metadata_context(&state, headers, &req, source.as_ref()).await?;
        Ok::<_, ApiError>((permit, context, source))
    }
    .await;
    let (permit, metadata_context, source) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            audit.failure(&error);
            return Err(error);
        }
    };
    let guard = std::sync::Arc::new(state.shutdown.track_query());
    let correlation = crate::correlation::current();
    tokio::spawn(async move {
        let permit = Arc::new(permit);
        let work = crate::shutdown::scope_query(
            Some(guard),
            execute_query_owned(
                state,
                id,
                req,
                metadata_context,
                source,
                audit,
                permit.clone(),
            ),
        );
        match correlation {
            Some(id) => crate::correlation::scope(id, work).await,
            None => work.await,
        }
    })
    .await
    .map_err(|_| ApiError::Internal("HTTP query settlement owner failed".into()))?
}

async fn execute_query_owned(
    state: AppState,
    id: sift_protocol::SessionId,
    req: ExecuteRequestHttp,
    metadata_context: Option<ExecuteMetadataContext>,
    source: Option<crate::session::ConnectionEntryClone>,
    mut audit: query_audit::QueryOperationAudit,
    permit: Arc<tokio::sync::OwnedSemaphorePermit>,
) -> ApiResult<Response> {
    let sql_text = req.sql.clone();
    let transaction_id = req.tx.as_ref().map(|tx| tx.tx_id.0.to_string());
    let inactive_room_connection = metadata_context
        .as_ref()
        .and_then(|context| context.room_id)
        .filter(|room| !state.rooms.is_active(room.0));
    let started = Instant::now();
    // A bound room routes execution through its server-owned connection
    // (ADR-037); everything else runs on the caller's session connection.
    use crate::session::native_execution::{self, NativeExecution, StartupGuard};
    let engine = metadata_context
        .as_ref()
        .and_then(|context| context.room_routing.as_ref())
        .and_then(|routing| routing.engine)
        .or_else(|| {
            source
                .as_ref()
                .and_then(|entry| entry.driver.semantic_engine())
        });
    let observation = Arc::new(NativeExecution::default());
    let startup = StartupGuard(observation.clone());
    native_settlement::NativeLedger {
        scope: metadata_context.as_ref().map(|context| DatabaseActorScope {
            actor: context.principal_id,
            tenant_id: context.tenant_id,
            connection_profile_id: context.connection_profile_id,
            database_target: context.database_target.clone(),
        }),
        workspace: None,
        room: metadata_context
            .as_ref()
            .and_then(|context| context.room_id),
        transaction: transaction_id,
        source: metadata_context
            .as_ref()
            .and_then(|context| context.versioned_source.clone()),
        effect: crate::sql_policy::statement_effect(engine, &sql_text),
        fingerprint: crate::fingerprint::sql(&sql_text),
        workflow: "direct_sql",
    }
    .spawn(
        state.clone(),
        observation.clone(),
        permit,
        crate::shutdown::current_query_guard(),
    );
    let (result, shared_pages, shared_retention_guards) =
        native_execution::scope(observation, async {
            match metadata_context
                .as_ref()
                .and_then(|c| c.room_routing.clone())
            {
                Some(routing) => {
                    let provenance = crate::session::RoomConnProvenance {
                        room_id: routing.room_id,
                        binder: routing.binder,
                        tenant: routing.tenant,
                        profile_id: routing.profile_id,
                        provider_id: routing.provider_id,
                        engine: routing.engine,
                        policy_revision: routing.policy_revision,
                    };
                    match state
                        .sessions
                        .execute_room_query(provenance, req, routing.configuration)
                        .await
                    {
                        Ok(execution) => (
                            Ok(execution.response),
                            Some(execution.pages),
                            execution.retention_guards,
                        ),
                        Err(error) => (Err(error), Some(Vec::new()), Vec::new()),
                    }
                }
                None => match source {
                    Some(source) => (
                        state.sessions.execute_http_captured(id, req, source).await,
                        None,
                        Vec::new(),
                    ),
                    None => (
                        Err(ApiError::Internal(
                            "direct query source was not captured".into(),
                        )),
                        None,
                        Vec::new(),
                    ),
                },
            }
        })
        .await;
    drop(startup);
    let duration_ms = started.elapsed().as_millis().min(i64::MAX as u128) as i64;
    // Record the observed execution once before auxiliary retention/history or
    // delivery can fail. Those failures cannot change the native observation.
    match &result {
        Ok(response) => audit.record(
            OperationStatus::Succeeded,
            None,
            Some(response.rows.len() as i64),
            None,
        ),
        Err(error) => audit.failure(error),
    }

    if let Some(context) = metadata_context.as_ref() {
        // Query history keeps raw SQL by default; when store_sql is off it
        // stores only the fingerprint (audit trail is always fingerprinted).
        let history_sql = if state.sessions.store_sql() {
            sql_text
        } else {
            crate::fingerprint::sql(&sql_text)
        };
        record_execute_history(context.clone(), history_sql, duration_ms, &result).await;
    }
    let room_retention: ApiResult<()> = async {
        if let Some(context) = metadata_context {
            if let Some(room_id) = context.room_id {
                let row_count = shared_pages.as_ref().map(|pages| {
                    pages
                        .iter()
                        .map(|page| match page {
                            sift_protocol::Page::Rows { rows } => rows.len() as i64,
                            _ => 0,
                        })
                        .sum()
                });
                let registry = state.rooms.results().clone();
                let actor_principal_id = context.principal_id.0;
                let connection_profile_id = context.connection_profile_id.map(|id| id.0);
                let error_message = result.as_ref().err().map(ToString::to_string);
                let summary = tokio::task::spawn_blocking(move || {
                    registry.insert(crate::room_results::NewRoomResult {
                        room_id: room_id.0,
                        actor_principal_id,
                        connection_profile_id,
                        pages: shared_pages.unwrap_or_default(),
                        row_count,
                        error_message,
                        retention_guards: shared_retention_guards,
                    })
                })
                .await
                .map_err(|error| {
                    ApiError::Internal(format!("room result retention task failed: {error}"))
                })?;
                state.rooms.publish_presence(
                    summary.room_id,
                    RoomServerMessage::QueryResult { result: summary },
                );
            }
        }
        Ok(())
    }
    .await;
    if let Some(room) = inactive_room_connection.filter(|room| !state.rooms.is_active(room.0)) {
        state.sessions.close_room_connection(room.0).await;
    }
    room_retention?;
    match result {
        Ok(response) => {
            let bytes = serde_json::to_vec(&response)
                .map_err(|error| ApiError::Internal(error.to_string()))?;
            retained_json_response(&state.sessions, id, bytes)
        }
        Err(error) => Err(error),
    }
}

pub(super) fn retained_json_response(
    sessions: &SessionStore,
    session: sift_protocol::SessionId,
    bytes: Vec<u8>,
) -> ApiResult<Response> {
    let guard = sessions.reserve_session_retained_bytes(session, bytes.len())?;
    let bytes = bytes::Bytes::from_owner(RetainedResponseBytes {
        bytes,
        _guard: guard,
    });
    let mut response = Body::from(bytes).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    Ok(response)
}

// The response observer has a deadline; the existing bounded audit owner waits
// for the actual worker. This retains late acknowledgements and shutdown scope.
async fn owned_audited_transaction<T, F>(
    state: AppState,
    operation: Operation,
    actor: Option<i64>,
    action: crate::session::native_transaction::Action,
    work: F,
    settle: fn(ApiResult<T>, crate::session::native_transaction::Evidence) -> ApiResult<T>,
) -> ApiResult<T>
where
    T: Send + 'static,
    F: std::future::Future<Output = ApiResult<T>> + Send + 'static,
{
    use crate::session::native_transaction::{self, Observation};
    let observation = Observation::new(action);
    let retained = observation.clone();
    let timeout = state.sessions.request_timeout();
    let shutdown = state.shutdown.clone();
    let owner = owned_audited_operation(state, operation, actor, async move {
        if shutdown.is_draining() {
            return Err(ApiError::ServiceDraining);
        }
        let result = native_transaction::scope(retained.clone(), work).await;
        let evidence = retained.terminal().await;
        settle(result, evidence)
    });
    if timeout.is_zero() {
        return owner.await;
    }
    match tokio::time::timeout(timeout, owner).await {
        Ok(result) => result,
        Err(_) => {
            observation.close_dispatch();
            Err(ApiError::Driver(sift_protocol::DriverError::new(
                sift_protocol::Code::QueryTimedOut,
                "Transaction observation timed out; inspect transaction state before retrying",
            )))
        }
    }
}

pub(super) async fn begin_transaction(
    State(state): State<AppState>,
    auth: Option<Extension<AuthContext>>,
    Path(id): Path<sift_protocol::SessionId>,
    Json(req): Json<BeginTransactionRequest>,
) -> ApiResult<Json<sift_protocol::TransactionInfo>> {
    let actor = auth.map(|Extension(auth)| auth.principal_id.0).or_else(|| {
        state
            .sessions
            .session_owner(id)
            .ok()
            .flatten()
            .map(|owner| owner.0)
    });
    let operation = Operation::BeginTransaction {
        session: id,
        request: req.clone(),
    };
    let sessions = state.sessions.clone();
    owned_audited_transaction(
        state,
        operation,
        actor,
        crate::session::native_transaction::Action::Begin,
        async move { sessions.begin_transaction(id, req).await },
        |result, evidence| evidence.published.map(Ok).unwrap_or(result),
    )
    .await
    .map(Json)
}

pub(super) async fn list_processes(
    State(state): State<AppState>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
) -> ApiResult<Json<Vec<sift_protocol::DatabaseProcess>>> {
    let processes = finish_operation(
        &state.sessions,
        Operation::ListProcesses {
            session,
            connection,
        },
        crate::process::list(&state.sessions, session, connection).await,
        |processes| Some(processes.len() as i64),
    )?;
    Ok(Json(processes))
}

pub(super) async fn read_server_dashboard(
    State(state): State<AppState>,
    auth: Option<Extension<AuthContext>>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
) -> ApiResult<Json<sift_protocol::ServerDashboard>> {
    use sift_protocol::{
        DashboardProcesses, DashboardSectionState, OperationCapabilityContext, OperationKind,
    };
    let operation = Operation::ReadServerDashboard {
        session,
        connection,
    };
    let result: ApiResult<sift_protocol::ServerDashboard> = async {
        let context = OperationCapabilityContext {
            session: Some(session),
            connection: Some(connection),
            ..Default::default()
        };
        let authorization = capability_authorization_scope(
            &state,
            auth.as_ref().map(|Extension(auth)| auth),
            &context,
        )?;
        let all = crate::capability::evaluate(&state.sessions, &context, authorization.as_ref())?;
        let dashboard_access = all
            .iter()
            .find(|item| item.operation == OperationKind::ReadServerDashboard)
            .expect("dashboard capability belongs to the operation inventory");
        if !dashboard_access.available {
            return Err(ApiError::Forbidden(
                dashboard_access
                    .reason
                    .clone()
                    .unwrap_or_else(|| "dashboard access denied".into()),
            ));
        }
        let entry = state.sessions.conn_entry(session, connection)?;
        let engine = entry.driver.engine();
        let relevant = [
            OperationKind::ListProcesses,
            OperationKind::ListDeadlocks,
            OperationKind::ListPostgresSettings,
            OperationKind::ReadQueryStore,
            OperationKind::ReadAgentJobs,
            OperationKind::ReadSqlServerSettings,
            OperationKind::ReadSqlServerSecurity,
        ];
        let capabilities = all
            .into_iter()
            .filter(|item| relevant.contains(&item.operation))
            .collect::<Vec<_>>();
        let process_access = capabilities
            .iter()
            .find(|item| item.operation == OperationKind::ListProcesses)
            .expect("process capability belongs to the operation inventory");
        let processes = if !process_access.available {
            DashboardProcesses {
                state: if !entry
                    .driver
                    .supports_operation(OperationKind::ListProcesses)
                {
                    DashboardSectionState::Unsupported
                } else {
                    DashboardSectionState::PermissionDenied
                },
                summary: None,
                reason: process_access.reason.clone(),
            }
        } else {
            match crate::process::list(&state.sessions, session, connection).await {
                Ok(processes) => DashboardProcesses {
                    state: DashboardSectionState::Ready,
                    summary: Some(crate::process::dashboard_summary(&processes)),
                    reason: None,
                },
                Err(ApiError::Forbidden(_))
                | Err(ApiError::Unauthorized)
                | Err(ApiError::Driver(sift_protocol::DriverError {
                    code: sift_protocol::Code::AuthFailed,
                    ..
                })) => DashboardProcesses {
                    state: DashboardSectionState::PermissionDenied,
                    summary: None,
                    reason: Some("database monitoring permission denied".into()),
                },
                Err(_) => DashboardProcesses {
                    state: DashboardSectionState::Unavailable,
                    summary: None,
                    reason: Some("database monitoring sample unavailable".into()),
                },
            }
        };
        Ok(sift_protocol::ServerDashboard {
            sampled_at: chrono::Utc::now(),
            engine,
            processes,
            capabilities,
        })
    }
    .await;
    let dashboard = finish_operation(&state.sessions, operation, result, |_| None)?;
    Ok(Json(dashboard))
}

pub(super) async fn list_deadlocks(
    State(state): State<AppState>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
) -> ApiResult<Json<Vec<sift_protocol::DatabaseDeadlockEvent>>> {
    let events = finish_operation(
        &state.sessions,
        Operation::ListDeadlocks {
            session,
            connection,
        },
        crate::process::list_deadlocks(&state.sessions, session, connection).await,
        |events| Some(events.len() as i64),
    )?;
    Ok(Json(events))
}

pub(super) async fn list_postgres_settings(
    State(state): State<AppState>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Query(query): Query<sift_protocol::PostgresSettingsQuery>,
) -> ApiResult<Json<sift_protocol::PostgresSettingsPage>> {
    let operation = Operation::ListPostgresSettings {
        session,
        connection,
        offset: query.offset,
        limit: query.limit.unwrap_or(100),
    };
    let page = finish_operation(
        &state.sessions,
        operation,
        crate::postgres_settings::list(&state.sessions, session, connection, query).await,
        |page| Some(page.settings.len() as i64),
    )?;
    Ok(Json(page))
}

pub(super) async fn list_postgres_extensions(
    State(state): State<AppState>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Query(query): Query<sift_protocol::PostgresObjectPageQuery>,
) -> ApiResult<Json<sift_protocol::PostgresObjectPage<sift_protocol::PostgresExtension>>> {
    let operation = Operation::ListPostgresObjects {
        session,
        connection,
        object: "extension".into(),
        offset: query.offset,
        limit: query.limit.unwrap_or(100),
    };
    let page = finish_operation(
        &state.sessions,
        operation,
        crate::postgres_workbench::extensions(&state.sessions, session, connection, query).await,
        |page| Some(page.items.len() as i64),
    )?;
    Ok(Json(page))
}

pub(super) async fn list_postgres_partitions(
    State(state): State<AppState>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Query(query): Query<sift_protocol::PostgresObjectPageQuery>,
) -> ApiResult<Json<sift_protocol::PostgresObjectPage<sift_protocol::PostgresPartition>>> {
    let operation = Operation::ListPostgresObjects {
        session,
        connection,
        object: "partition".into(),
        offset: query.offset,
        limit: query.limit.unwrap_or(100),
    };
    let page = finish_operation(
        &state.sessions,
        operation,
        crate::postgres_workbench::partitions(&state.sessions, session, connection, query).await,
        |page| Some(page.items.len() as i64),
    )?;
    Ok(Json(page))
}

pub(super) async fn list_postgres_policies(
    State(state): State<AppState>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Query(query): Query<sift_protocol::PostgresObjectPageQuery>,
) -> ApiResult<Json<sift_protocol::PostgresObjectPage<sift_protocol::PostgresPolicy>>> {
    let operation = Operation::ListPostgresObjects {
        session,
        connection,
        object: "policy".into(),
        offset: query.offset,
        limit: query.limit.unwrap_or(100),
    };
    let page = finish_operation(
        &state.sessions,
        operation,
        crate::postgres_workbench::policies(&state.sessions, session, connection, query).await,
        |page| Some(page.items.len() as i64),
    )?;
    Ok(Json(page))
}

pub(super) async fn read_postgres_replication(
    State(state): State<AppState>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
) -> ApiResult<Json<sift_protocol::PostgresReplicationReport>> {
    let report = finish_operation(
        &state.sessions,
        Operation::ReadPostgresReplication {
            session,
            connection,
        },
        crate::postgres_diagnostics::replication(&state.sessions, session, connection).await,
        |report| {
            Some(
                (report.senders.len() + report.slots.len() + usize::from(report.receiver.is_some()))
                    as i64,
            )
        },
    )?;
    Ok(Json(report))
}

pub(super) async fn read_postgres_statistics(
    State(state): State<AppState>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Query(query): Query<sift_protocol::PostgresStatisticsQuery>,
) -> ApiResult<Json<sift_protocol::PostgresStatisticsReport>> {
    let operation = Operation::ReadPostgresStatistics {
        session,
        connection,
        offset: query.offset,
        limit: query.limit.unwrap_or(100),
    };
    let report = finish_operation(
        &state.sessions,
        operation,
        crate::postgres_diagnostics::statistics(&state.sessions, session, connection, query).await,
        |report| Some(report.tables.len() as i64),
    )?;
    Ok(Json(report))
}

pub(super) async fn sqlserver_privileges(
    State(state): State<AppState>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Query(query): Query<sift_protocol::SqlServerPrivilegeQuery>,
) -> ApiResult<Json<sift_protocol::SqlServerPrivilegeReport>> {
    let guard = state.shutdown.track_query();
    let report = tokio::spawn(async move {
        let _guard = guard;
        let actor = state.sessions.session_owner(session)?.map(|id| id.0);
        let operation = Operation::ReadSqlServerPrivileges {
            session,
            connection,
            query: query.clone(),
        };
        let result =
            crate::sqlserver_privileges::read(&state.sessions, session, connection, query).await;
        finish_operation_as(&state.sessions, operation, result, actor, |report| {
            Some(report.objects.len() as i64)
        })
    })
    .await
    .map_err(|_| ApiError::Internal("SQL Server privilege audit supervisor failed".into()))??;
    Ok(Json(report))
}

pub(super) async fn postgres_privileges(
    State(state): State<AppState>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Query(query): Query<sift_protocol::PostgresPrivilegeQuery>,
) -> ApiResult<Json<sift_protocol::PostgresPrivilegeReport>> {
    let guard = state.shutdown.track_query();
    let report = tokio::spawn(async move {
        let _guard = guard;
        let actor = state.sessions.session_owner(session)?.map(|id| id.0);
        let operation = Operation::ReadPostgresPrivileges {
            session,
            connection,
            query: query.clone(),
        };
        let result =
            crate::postgres_privileges::read(&state.sessions, session, connection, query).await;
        finish_operation_as(&state.sessions, operation, result, actor, |report| {
            Some(report.objects.len() as i64)
        })
    })
    .await
    .map_err(|_| ApiError::Internal("PostgreSQL privilege audit supervisor failed".into()))??;
    Ok(Json(report))
}

pub(super) async fn list_postgres_roles(
    State(state): State<AppState>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Query(query): Query<sift_protocol::PostgresObjectPageQuery>,
) -> ApiResult<Json<sift_protocol::PostgresObjectPage<sift_protocol::PostgresRole>>> {
    let operation = Operation::ListPostgresObjects {
        session,
        connection,
        object: "role".into(),
        offset: query.offset,
        limit: query.limit.unwrap_or(100),
    };
    let page = finish_operation(
        &state.sessions,
        operation,
        crate::postgres_workbench::roles(&state.sessions, session, connection, query).await,
        |page| Some(page.items.len() as i64),
    )?;
    Ok(Json(page))
}

pub(super) async fn list_postgres_role_memberships(
    State(state): State<AppState>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Query(query): Query<sift_protocol::PostgresObjectPageQuery>,
) -> ApiResult<Json<sift_protocol::PostgresObjectPage<sift_protocol::PostgresRoleMembership>>> {
    let operation = Operation::ListPostgresObjects {
        session,
        connection,
        object: "role_membership".into(),
        offset: query.offset,
        limit: query.limit.unwrap_or(100),
    };
    let page = finish_operation(
        &state.sessions,
        operation,
        crate::postgres_workbench::memberships(&state.sessions, session, connection, query).await,
        |page| Some(page.items.len() as i64),
    )?;
    Ok(Json(page))
}

pub(super) async fn list_postgres_owners(
    State(state): State<AppState>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Query(query): Query<sift_protocol::PostgresObjectPageQuery>,
) -> ApiResult<Json<sift_protocol::PostgresObjectPage<sift_protocol::PostgresOwnedObject>>> {
    let operation = Operation::ListPostgresObjects {
        session,
        connection,
        object: "owner".into(),
        offset: query.offset,
        limit: query.limit.unwrap_or(100),
    };
    let page = finish_operation(
        &state.sessions,
        operation,
        crate::postgres_workbench::owners(&state.sessions, session, connection, query).await,
        |page| Some(page.items.len() as i64),
    )?;
    Ok(Json(page))
}

pub(super) async fn list_postgres_schema_grants(
    State(state): State<AppState>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Query(query): Query<sift_protocol::PostgresObjectPageQuery>,
) -> ApiResult<Json<sift_protocol::PostgresObjectPage<sift_protocol::PostgresSchemaGrant>>> {
    let operation = Operation::ListPostgresObjects {
        session,
        connection,
        object: "schema_grant".into(),
        offset: query.offset,
        limit: query.limit.unwrap_or(100),
    };
    let page = finish_operation(
        &state.sessions,
        operation,
        crate::postgres_workbench::schema_grants(&state.sessions, session, connection, query).await,
        |page| Some(page.items.len() as i64),
    )?;
    Ok(Json(page))
}

pub(super) async fn preview_postgres_object(
    State(state): State<AppState>,
    auth: Option<Extension<AuthContext>>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Json(action): Json<sift_protocol::PostgresObjectAction>,
) -> ApiResult<Json<sift_protocol::PostgresObjectPreview>> {
    let operation = Operation::PreviewPostgresObject {
        session,
        connection,
        action: action.clone(),
    };
    if matches!(
        action,
        sift_protocol::PostgresObjectAction::ChangeObjectPrivilege { .. }
            | sift_protocol::PostgresObjectAction::ChangeDefaultPrivilege { .. }
    ) {
        let sessions = if matches!(
            operation,
            Operation::PreviewPostgresObject {
                action: sift_protocol::PostgresObjectAction::ChangeDefaultPrivilege { .. },
                ..
            } | Operation::ApplyPostgresObject {
                action: sift_protocol::PostgresObjectAction::ChangeDefaultPrivilege { .. },
                ..
            }
        ) {
            state
                .sessions
                .conn_entry(session, connection)
                .and_then(|entry| state.sessions.bind_source(session, connection, entry))
        } else {
            Ok(state.sessions.clone())
        };
        return owned_audited_work(state, session, operation, auth, async move {
            crate::postgres_workbench::preview(&sessions?, session, connection, action).await
        })
        .await
        .map(Json);
    }
    let preview = finish_operation(
        &state.sessions,
        operation,
        crate::postgres_workbench::preview(&state.sessions, session, connection, action).await,
        |_| None,
    )?;
    Ok(Json(preview))
}

pub(super) async fn apply_postgres_object(
    State(state): State<AppState>,
    auth: Option<Extension<AuthContext>>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Json(request): Json<sift_protocol::ApplyPostgresObjectRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let operation = Operation::ApplyPostgresObject {
        session,
        connection,
        action: request.action.clone(),
    };
    if matches!(
        request.action,
        sift_protocol::PostgresObjectAction::ChangeObjectPrivilege { .. }
            | sift_protocol::PostgresObjectAction::ChangeDefaultPrivilege { .. }
    ) {
        let sessions = if matches!(
            operation,
            Operation::PreviewPostgresObject {
                action: sift_protocol::PostgresObjectAction::ChangeDefaultPrivilege { .. },
                ..
            } | Operation::ApplyPostgresObject {
                action: sift_protocol::PostgresObjectAction::ChangeDefaultPrivilege { .. },
                ..
            }
        ) {
            state
                .sessions
                .conn_entry(session, connection)
                .and_then(|entry| state.sessions.bind_source(session, connection, entry))
        } else {
            Ok(state.sessions.clone())
        };
        owned_audited_work(state, session, operation, auth, async move {
            crate::postgres_workbench::apply(&sessions?, session, connection, request).await
        })
        .await?;
        return Ok(Json(serde_json::json!({"applied":true})));
    }
    finish_operation(
        &state.sessions,
        operation,
        crate::postgres_workbench::apply(&state.sessions, session, connection, request).await,
        |_| None,
    )?;
    Ok(Json(serde_json::json!({"applied": true})))
}

pub(super) async fn owned_audited_work<T, F>(
    state: AppState,
    session: sift_protocol::SessionId,
    operation: Operation,
    auth: Option<Extension<AuthContext>>,
    work: F,
) -> ApiResult<T>
where
    T: Send + 'static,
    F: std::future::Future<Output = ApiResult<T>> + Send + 'static,
{
    let actor = auth.map(|Extension(auth)| auth.principal_id.0).or_else(|| {
        state
            .sessions
            .session_owner(session)
            .ok()
            .flatten()
            .map(|owner| owner.0)
    });
    owned_audited_operation(state, operation, actor, work).await
}

pub(super) async fn owned_audited_operation<T, F>(
    state: AppState,
    operation: Operation,
    actor: Option<i64>,
    work: F,
) -> ApiResult<T>
where
    T: Send + 'static,
    F: std::future::Future<Output = ApiResult<T>> + Send + 'static,
{
    let mut audit = query_audit::QueryOperationAudit::new(state.sessions.clone(), operation, actor);
    let permit = match state.sessions.reserve_query_settlement() {
        Ok(permit) => permit,
        Err(error) => {
            audit.failure(&error);
            return Err(error);
        }
    };
    let guard = std::sync::Arc::new(state.shutdown.track_query());
    let correlation = crate::correlation::current();
    tokio::spawn(async move {
        let _permit = permit;
        let work = crate::shutdown::scope_query(Some(guard), async move {
            let result = work.await;
            match &result {
                Ok(_) => audit.record(OperationStatus::Succeeded, None, None, None),
                Err(error) => audit.failure(error),
            }
            result
        });
        match correlation {
            Some(id) => crate::correlation::scope(id, work).await,
            None => work.await,
        }
    })
    .await
    .map_err(|_| ApiError::Internal("Audited operation owner failed".into()))?
}

pub(super) async fn read_query_store(
    State(state): State<AppState>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
) -> ApiResult<Response> {
    let _guard = state.shutdown.track_query();
    let actor = state.sessions.session_owner(session)?.map(|id| id.0);
    let report = finish_operation_as(
        &state.sessions,
        Operation::ReadQueryStore {
            session,
            connection,
        },
        crate::query_store::read(&state.sessions, session, connection).await,
        actor,
        |report| Some(report.plans.len() as i64),
    )?;
    let mut response = Json(report).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

pub(super) async fn read_agent_jobs(
    State(state): State<AppState>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
) -> ApiResult<Response> {
    let _guard = state.shutdown.track_query();
    let actor = state.sessions.session_owner(session)?.map(|id| id.0);
    let report = finish_operation_as(
        &state.sessions,
        Operation::ReadAgentJobs {
            session,
            connection,
        },
        crate::agent_jobs::read(&state.sessions, session, connection).await,
        actor,
        |report| Some(report.jobs.len() as i64),
    )?;
    let mut response = Json(report).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

pub(super) async fn read_sqlserver_settings(
    State(state): State<AppState>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
) -> ApiResult<Response> {
    let _guard = state.shutdown.track_query();
    let actor = state.sessions.session_owner(session)?.map(|id| id.0);
    let report = finish_operation_as(
        &state.sessions,
        Operation::ReadSqlServerSettings {
            session,
            connection,
        },
        crate::server_settings::read(&state.sessions, session, connection).await,
        actor,
        |report| Some(report.settings.len() as i64),
    )?;
    let mut response = Json(report).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

pub(super) async fn read_sqlserver_security(
    State(state): State<AppState>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
) -> ApiResult<Response> {
    let _guard = state.shutdown.track_query();
    let actor = state.sessions.session_owner(session)?.map(|id| id.0);
    let report = finish_operation_as(
        &state.sessions,
        Operation::ReadSqlServerSecurity {
            session,
            connection,
        },
        crate::sql_server_security::read(&state.sessions, session, connection).await,
        actor,
        |report| {
            Some(
                (report.logins.items.len()
                    + report.principals.items.len()
                    + report.memberships.items.len()
                    + report.schemas.items.len()
                    + report.schema_permissions.items.len()) as i64,
            )
        },
    )?;
    let mut response = Json(report).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

pub(super) async fn preview_sqlserver_security(
    State(state): State<AppState>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Json(action): Json<sift_protocol::SqlServerSecurityAction>,
) -> ApiResult<Json<sift_protocol::SqlServerSecurityPreview>> {
    let operation = Operation::PreviewSqlServerSecurity {
        session,
        connection,
        action: action.clone(),
    };
    let preview = finish_operation(
        &state.sessions,
        operation,
        crate::sql_server_security::preview(&state.sessions, session, connection, action).await,
        |_| None,
    )?;
    Ok(Json(preview))
}

pub(super) async fn apply_sqlserver_security(
    State(state): State<AppState>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Json(request): Json<sift_protocol::ApplySqlServerSecurityRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let operation = Operation::ApplySqlServerSecurity {
        session,
        connection,
        action: request.action.clone(),
    };
    finish_operation(
        &state.sessions,
        operation,
        crate::sql_server_security::apply(&state.sessions, session, connection, request).await,
        |_| None,
    )?;
    Ok(Json(serde_json::json!({"applied": true})))
}

pub(super) async fn postgres_maintenance(
    State(state): State<AppState>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Json(request): Json<sift_protocol::PostgresMaintenanceRequest>,
) -> ApiResult<Json<sift_protocol::PostgresMaintenanceReport>> {
    let _guard = state.shutdown.track_query();
    let actor = state.sessions.session_owner(session)?.map(|id| id.0);
    let report = finish_operation_as(
        &state.sessions,
        Operation::PostgresMaintenance {
            session,
            connection,
            request: request.clone(),
        },
        crate::maintenance::run(&state.sessions, session, connection, request).await,
        actor,
        |_| None,
    )?;
    Ok(Json(report))
}

pub(super) async fn check_integrity(
    State(state): State<AppState>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Json(request): Json<sift_protocol::IntegrityCheckRequest>,
) -> ApiResult<Response> {
    let _guard = state.shutdown.track_query();
    let actor = state.sessions.session_owner(session)?.map(|id| id.0);
    let report = finish_operation_as(
        &state.sessions,
        Operation::CheckIntegrity {
            session,
            connection,
            request: request.clone(),
        },
        crate::integrity::run(&state.sessions, session, connection, request).await,
        actor,
        |report| Some(report.findings.len() as i64),
    )?;
    let bytes =
        serde_json::to_vec(&report).map_err(|error| ApiError::Internal(error.to_string()))?;
    retained_json_response(&state.sessions, session, bytes)
}

pub(super) async fn sqlite_maintenance(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Json(request): Json<sift_protocol::SqliteMaintenanceRequest>,
) -> ApiResult<Json<sift_protocol::SqliteMaintenanceReport>> {
    let guard = state.shutdown.track_query();
    let actor = state.sessions.session_owner(session)?.map(|id| id.0);
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _caller = crate::sqlite_maintenance::CancelOnDrop(cancel.clone());
    let report = tokio::spawn(async move {
        let _guard = guard;
        let result = async {
            let (owner, _, _, _) = state.sessions.managed_catalog_scope(
                session,
                connection,
                sift_protocol::OperationKind::ManageSqliteDatabase,
            )?;
            if optional_auth_context_blocking(state.clone(), headers)
                .await?
                .is_some_and(|auth| auth.principal_id != owner)
            {
                return Err(ApiError::Forbidden(
                    "SQLite maintenance caller must own the managed session".into(),
                ));
            }
            crate::sqlite_maintenance::run_with_cancellation(
                &state.sessions,
                session,
                connection,
                request.clone(),
                cancel,
            )
            .await
        }
        .await;
        finish_operation_as(
            &state.sessions,
            Operation::SqliteMaintenance {
                session,
                connection,
                request,
            },
            result,
            actor,
            |_| None,
        )
    })
    .await
    .map_err(|_| ApiError::Internal("SQLite maintenance audit supervisor failed".into()))??;
    Ok(Json(report))
}

pub(super) async fn sqlserver_index_maintenance(
    State(state): State<AppState>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Json(request): Json<sift_protocol::SqlServerIndexRequest>,
) -> ApiResult<Json<sift_protocol::SqlServerIndexReport>> {
    let cancellation = tokio_util::sync::CancellationToken::new();
    let _caller = cancellation.clone().drop_guard();
    let guard = state.shutdown.track_query();
    let report = tokio::spawn(async move {
        let _guard = guard;
        let actor = state.sessions.session_owner(session)?.map(|id| id.0);
        let result = crate::sqlserver_index_maintenance::run(
            &state.sessions,
            session,
            connection,
            request.clone(),
            cancellation,
        )
        .await;
        finish_operation_as(
            &state.sessions,
            Operation::MaintainSqlServerIndex {
                session,
                connection,
                request,
            },
            result,
            actor,
            |_| None,
        )
    })
    .await
    .map_err(|_| ApiError::Internal("SQL Server maintenance audit supervisor failed".into()))??;
    Ok(Json(report))
}

pub(super) async fn sql_server_recovery(
    State(state): State<AppState>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Json(request): Json<sift_protocol::SqlServerRecoveryRequest>,
) -> ApiResult<Json<sift_protocol::SqlServerRecoveryReport>> {
    let _guard = state.shutdown.track_query();
    let actor = state.sessions.session_owner(session)?.map(|id| id.0);
    let report = finish_operation_as(
        &state.sessions,
        Operation::SqlServerRecovery {
            session,
            connection,
            request: request.clone(),
        },
        crate::sql_server_recovery::run(&state.sessions, session, connection, request).await,
        actor,
        |_| None,
    )?;
    Ok(Json(report))
}

pub(super) async fn kill_process(
    State(state): State<AppState>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Json(req): Json<KillProcessRequest>,
) -> ApiResult<Json<sift_protocol::KillProcessResponse>> {
    let response = finish_operation(
        &state.sessions,
        Operation::KillProcess {
            session,
            connection,
            request: req.clone(),
        },
        crate::process::kill(&state.sessions, session, connection, req.process_id).await,
        |_| None,
    )?;
    Ok(Json(response))
}

pub(super) async fn watch_process_alerts(
    State(state): State<AppState>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Query(options): Query<crate::process_alerts::AlertOptions>,
) -> ApiResult<Response> {
    use axum::{body::Body, response::IntoResponse};
    options.validate()?;
    if state.shutdown.is_draining() {
        return Err(ApiError::ServiceDraining);
    }
    // Streaming is a projection of the existing ListProcesses operation. Every
    // sample repeats its normal authorization, supervision and audit path.
    let first = finish_operation(
        &state.sessions,
        Operation::ListProcesses {
            session,
            connection,
        },
        crate::process::list(&state.sessions, session, connection).await,
        |rows| Some(rows.len() as i64),
    )?;
    let guard = state.shutdown.track_query();
    let deadline =
        tokio::time::Instant::now() + std::time::Duration::from_secs(options.duration_seconds);
    let stream = async_stream::stream! {
        let _guard = guard;
        let mut tracker = crate::process_alerts::AlertTracker::default();
        let mut processes = first;
        loop {
            let sample = tracker.sample(&processes, &options, chrono::Utc::now());
            match serde_json::to_vec(&sample) {
                Ok(mut bytes) => {
                    bytes.push(b'\n');
                    yield Ok::<_, std::io::Error>(bytes::Bytes::from(bytes));
                }
                Err(_) => {
                    yield Err(std::io::Error::other("alert encoding failed"));
                    break;
                }
            }
            tokio::select! {
                _ = state.shutdown.wait_for_drain_start() => break,
                _ = tokio::time::sleep_until(deadline) => break,
                _ = tokio::time::sleep(std::time::Duration::from_secs(options.poll_seconds)) => {}
            }
            match finish_operation(&state.sessions, Operation::ListProcesses { session, connection },
                crate::process::list(&state.sessions, session, connection).await, |rows| Some(rows.len() as i64)) {
                Ok(rows) => processes = rows,
                Err(_) => {
                    yield Err(std::io::Error::other("process alert sampling stopped"));
                    break;
                }
            }
        }
    };
    let mut response = Body::from_stream(stream).into_response();
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        "application/x-ndjson".parse().unwrap(),
    );
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        "no-store".parse().unwrap(),
    );
    Ok(response)
}

pub(super) async fn list_transactions(
    State(state): State<AppState>,
    Path(id): Path<sift_protocol::SessionId>,
) -> ApiResult<Json<Vec<sift_protocol::TransactionState>>> {
    let result = finish_operation(
        &state.sessions,
        Operation::ListTransactions { session: id },
        state.sessions.list_transactions(id),
        |transactions| Some(transactions.len() as i64),
    )?;
    Ok(Json(result))
}

pub(super) async fn preview_transaction(
    State(state): State<AppState>,
    Path((id, tx_id)): Path<(sift_protocol::SessionId, sift_protocol::TxId)>,
    Json(req): Json<TransactionPreviewRequest>,
) -> ApiResult<Json<sift_protocol::TransactionPreview>> {
    let result = if req.tx_id != tx_id {
        Err(ApiError::BadRequest(
            "`tx_id` body value must match tx id in path".into(),
        ))
    } else {
        state.sessions.preview_transaction(id, &req)
    };
    let result = finish_operation(
        &state.sessions,
        Operation::PreviewTransaction {
            session: id,
            request: req.clone(),
        },
        result,
        |_| None,
    )?;
    Ok(Json(result))
}

pub(super) async fn commit_transaction(
    State(state): State<AppState>,
    auth: Option<Extension<AuthContext>>,
    Path((id, tx_id)): Path<(sift_protocol::SessionId, sift_protocol::TxId)>,
    Json(req): Json<EndTransactionRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    end_transaction(state, auth, id, tx_id, req, true).await
}

pub(super) async fn rollback_transaction(
    State(state): State<AppState>,
    auth: Option<Extension<AuthContext>>,
    Path((id, tx_id)): Path<(sift_protocol::SessionId, sift_protocol::TxId)>,
    Json(req): Json<EndTransactionRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    end_transaction(state, auth, id, tx_id, req, false).await
}

async fn end_transaction(
    state: AppState,
    auth: Option<Extension<AuthContext>>,
    id: sift_protocol::SessionId,
    tx_id: sift_protocol::TxId,
    req: EndTransactionRequest,
    commit: bool,
) -> ApiResult<Json<serde_json::Value>> {
    use crate::session::native_transaction::Action;
    let actor = auth.map(|Extension(auth)| auth.principal_id.0).or_else(|| {
        state
            .sessions
            .session_owner(id)
            .ok()
            .flatten()
            .map(|owner| owner.0)
    });
    let operation = if commit {
        Operation::CommitTransaction {
            session: id,
            request: req.clone(),
        }
    } else {
        Operation::RollbackTransaction {
            session: id,
            request: req.clone(),
        }
    };
    let sessions = state.sessions.clone();
    owned_audited_transaction(
        state,
        operation,
        actor,
        if commit {
            Action::Commit
        } else {
            Action::Rollback
        },
        async move {
            if req.tx_id != tx_id {
                return Err(ApiError::BadRequest(
                    "`tx_id` body value must match tx id in path".into(),
                ));
            }
            if commit {
                sessions.commit_transaction(id, req).await
            } else {
                sessions.rollback_transaction(id, req).await
            }
        },
        |result, evidence| {
            if evidence.acknowledged == Some(true) {
                Ok(())
            } else {
                result
            }
        },
    )
    .await?;
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn create_savepoint(
    State(state): State<AppState>,
    Path((id, tx_id)): Path<(sift_protocol::SessionId, sift_protocol::TxId)>,
    Json(req): Json<SavepointRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let guard = state.shutdown.track_query();
    let actor = state.sessions.session_owner(id)?.map(|owner| owner.0);
    tokio::spawn(async move {
        let _guard = guard;
        let result = if req.tx_id != tx_id {
            Err(ApiError::BadRequest(
                "`tx_id` body value must match tx id in path".into(),
            ))
        } else {
            state.sessions.create_savepoint(id, req.clone()).await
        };
        finish_operation_as(
            &state.sessions,
            Operation::Savepoint {
                session: id,
                request: req,
            },
            result,
            actor,
            |_| None,
        )?;
        Ok(Json(json!({"ok": true})))
    })
    .await
    .map_err(|_| ApiError::Internal("transaction control audit supervisor failed".into()))?
}

pub(super) async fn rollback_to_savepoint(
    State(state): State<AppState>,
    Path((id, tx_id)): Path<(sift_protocol::SessionId, sift_protocol::TxId)>,
    Json(req): Json<SavepointRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let guard = state.shutdown.track_query();
    let actor = state.sessions.session_owner(id)?.map(|owner| owner.0);
    tokio::spawn(async move {
        let _guard = guard;
        let result = if req.tx_id != tx_id {
            Err(ApiError::BadRequest(
                "`tx_id` body value must match tx id in path".into(),
            ))
        } else {
            state.sessions.rollback_to_savepoint(id, req.clone()).await
        };
        finish_operation_as(
            &state.sessions,
            Operation::RollbackToSavepoint {
                session: id,
                request: req,
            },
            result,
            actor,
            |_| None,
        )?;
        Ok(Json(json!({"ok": true})))
    })
    .await
    .map_err(|_| ApiError::Internal("transaction control audit supervisor failed".into()))?
}

pub(super) async fn release_savepoint(
    State(state): State<AppState>,
    Path((id, tx_id)): Path<(sift_protocol::SessionId, sift_protocol::TxId)>,
    Json(req): Json<SavepointRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let guard = state.shutdown.track_query();
    let actor = state.sessions.session_owner(id)?.map(|owner| owner.0);
    tokio::spawn(async move {
        let _guard = guard;
        let result = if req.tx_id != tx_id {
            Err(ApiError::BadRequest(
                "`tx_id` body value must match tx id in path".into(),
            ))
        } else {
            state.sessions.release_savepoint(id, req.clone()).await
        };
        finish_operation_as(
            &state.sessions,
            Operation::ReleaseSavepoint {
                session: id,
                request: req,
            },
            result,
            actor,
            |_| None,
        )?;
        Ok(Json(json!({"ok": true})))
    })
    .await
    .map_err(|_| ApiError::Internal("transaction control audit supervisor failed".into()))?
}

pub(super) async fn cancel_query(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, cursor_id)): Path<(sift_protocol::SessionId, sift_protocol::CursorId)>,
    Json(req): Json<CancelRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    if req.cursor != cursor_id {
        return Err(ApiError::BadRequest(
            "`cursor` body value must match cursor id in path".into(),
        ));
    }
    let auth = optional_auth_context_blocking(state.clone(), headers).await?;
    let actor = auth.as_ref().map(|auth| auth.principal_id.0);
    if let Some(owner) = state.sessions.session_owner(id)? {
        let Some(auth) = auth.as_ref() else {
            return Err(ApiError::Unauthorized);
        };
        if auth.principal_id != owner {
            return Err(ApiError::Forbidden(
                "cannot cancel a cursor owned by another principal".into(),
            ));
        }
    }
    state.sessions.cancel(id, req.connection, cursor_id).await?;
    state.sessions.push_operation_full(
        Operation::CancelQuery {
            session: id,
            request: req,
        },
        OperationStatus::Succeeded,
        actor,
        None,
        None,
        None,
    );
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn read_spill_pages(
    State(state): State<AppState>,
    Path(cursor_id): Path<sift_protocol::CursorId>,
    axum::extract::Query(q): axum::extract::Query<ReadSpillPagesQuery>,
) -> ApiResult<Response> {
    let registry = state.sessions.cursor_registry().clone();
    let info = registry.spill_info(cursor_id).ok_or_else(|| {
        ApiError::Driver(sift_protocol::DriverError::new(
            sift_protocol::Code::CursorNotFound,
            "no spill for cursor",
        ))
    })?;
    // If from_seq is set and it doesn't match the entry's current
    // read cursor, reject — we don't allow re-reading already-read
    // pages (spill files are append-only + read-forward).
    if let Some(seq) = q.from_seq {
        if seq != info.pages_read {
            return Err(ApiError::BadRequest(format!(
                "from_seq={seq} does not match pages_read={} for cursor",
                info.pages_read
            )));
        }
    }
    let limit = q.limit.unwrap_or(32).clamp(1, 256);
    let (pages, done) =
        tokio::task::spawn_blocking(move || registry.read_spill_pages(cursor_id, limit))
            .await
            .map_err(|e| ApiError::Internal(format!("spill read task failed: {e}")))?
            .map_err(ApiError::Driver)?;
    let bytes = serde_json::to_vec(&json!({
        "cursor_id": cursor_id.0,
        "pages": pages,
        "done": done,
    }))
    .map_err(|error| ApiError::Internal(error.to_string()))?;
    retained_json_response(&state.sessions, info.session_id, bytes)
}

pub(super) async fn delete_spilled_cursor(
    State(state): State<AppState>,
    Path(cursor_id): Path<sift_protocol::CursorId>,
) -> ApiResult<Json<serde_json::Value>> {
    state.sessions.cursor_registry().drop_spill(cursor_id);
    Ok(Json(json!({"ok": true})))
}

#[cfg(all(test, unix))]
mod sqlite_maintenance_tests {
    use super::*;
    #[tokio::test]
    async fn abandoned_native_maintenance_rolls_back_and_finishes_its_audit() {
        use sift_metadata::{
            CredentialMode, MemorySecretStore, MetadataStore, NewConnectionProfile,
        };
        use sift_protocol::{Engine, SqliteMaintenanceAction as Action, SqliteMaintenanceRequest};
        let root = tempfile::tempdir().unwrap();
        let database = rusqlite::Connection::open(root.path().join("source.db")).unwrap();
        database.execute_batch("CREATE TABLE items(id INTEGER PRIMARY KEY,v INTEGER); WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<200000) INSERT INTO items SELECT i,i%1000 FROM n; CREATE INDEX items_v ON items(v)").unwrap();
        database.busy_timeout(std::time::Duration::ZERO).unwrap();
        let metadata =
            MetadataStore::open_in_memory(std::sync::Arc::new(MemorySecretStore::new())).unwrap();
        metadata
            .bootstrap_local("maintenance audit acceptance")
            .unwrap();
        let configuration = json!({"root_id":"test","path":"source.db","mode":"read_write"});
        let profile = metadata
            .upsert_connection_profile(
                sift_metadata::TenantId(1),
                sift_metadata::PrincipalId(1),
                NewConnectionProfile {
                    name: "maintenance audit".into(),
                    provider_id: Engine::Sqlite.provider_id(),
                    semantic_engine: Some(Engine::Sqlite),
                    configuration: configuration.clone(),
                    credentials: None,
                    credential_mode: CredentialMode::Shared,
                    tags: vec![],
                },
            )
            .await
            .unwrap();
        let driver = sift_driver_sqlite::SqliteDriver::with_files(sift_driver_sqlite::FilePolicy {
            config: sift_protocol::SqliteDriverConfig {
                roots: std::collections::BTreeMap::from([(
                    "test".into(),
                    sift_protocol::SqliteRootConfig {
                        path: root.path().to_string_lossy().into_owned(),
                        allowed_tenants: vec![1],
                        read_only: false,
                    },
                )]),
                max_connections: 1,
            },
            protected: vec![],
        });
        let sessions = SessionStore::new(crate::DriverRegistry::builder().register(driver).build());
        sessions.set_audit_store(metadata.clone());
        sessions.set_authorization_store(metadata.clone());
        let session = sessions
            .open_session_with_owner(
                OpenSessionRequest {
                    tag: None,
                    tenant_id: Some(1),
                },
                Some(sift_metadata::PrincipalId(1)),
                Some(sift_metadata::TenantId(1)),
                false,
            )
            .unwrap()
            .id;
        let connection = sessions
            .open_managed_connection(
                session,
                Engine::Sqlite.provider_id(),
                configuration,
                Default::default(),
                sift_metadata::PrincipalId(1),
                sift_metadata::TenantId(1),
                profile.id,
                profile.policy.revision,
                true,
            )
            .await
            .unwrap()
            .id;
        let state = AppState {
            sessions: sessions.clone(),
            rooms: Default::default(),
            auth: AuthState::default(),
            metadata: Some(metadata),
            shutdown: Default::default(),
        };
        let request = SqliteMaintenanceRequest {
            action: Action::Analyze {
                table: "items".into(),
            },
            apply: false,
            confirm_write: false,
            backup_verified: false,
            preview_token: None,
        };
        let Json(preview) = sqlite_maintenance(
            State(state.clone()),
            HeaderMap::new(),
            Path((session, connection)),
            Json(request.clone()),
        )
        .await
        .unwrap();
        let mut request = request;
        request.apply = true;
        request.confirm_write = true;
        request.backup_verified = true;
        request.preview_token = preview.preview_token;
        let handler = tokio::spawn(sqlite_maintenance(
            State(state),
            HeaderMap::new(),
            Path((session, connection)),
            Json(request),
        ));
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                match database.execute_batch("BEGIN IMMEDIATE") {
                    Ok(()) => database.execute_batch("ROLLBACK").unwrap(),
                    Err(failure)
                        if failure.sqlite_error_code()
                            == Some(rusqlite::ErrorCode::DatabaseBusy) =>
                    {
                        break
                    }
                    Err(failure) => panic!("unexpected lock observation: {failure}"),
                }
                assert!(
                    !handler.is_finished(),
                    "observe native maintenance before completion"
                );
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        handler.abort();
        assert!(handler.await.unwrap_err().is_cancelled());
        tokio::time::timeout(std::time::Duration::from_secs(2),async {
            while !sessions.list_operations().iter().any(|entry| matches!(&entry.operation,Operation::SqliteMaintenance {request,..} if request.apply) && entry.status==OperationStatus::Failed) {tokio::task::yield_now().await;}
        }).await.unwrap();
        assert_eq!(
            database
                .query_row(
                    "SELECT count(*) FROM sqlite_schema WHERE name='sqlite_stat1'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert_eq!(
            database
                .query_row("SELECT count(*) FROM items", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            200000
        );
        sessions.close_session(session).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn abandoned_privilege_observer_retains_work_and_one_terminal_audit() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        let metadata =
            MetadataStore::open_in_memory(Arc::new(sift_metadata::MemorySecretStore::new()))
                .unwrap();
        metadata.bootstrap_local("privilege audit owner").unwrap();
        let sessions = SessionStore::new(crate::DriverRegistry::new());
        sessions.set_audit_store(metadata.clone());
        let state = AppState {
            sessions: sessions.clone(),
            rooms: Default::default(),
            shutdown: Default::default(),
            auth: AuthState::default(),
            metadata: None,
        };
        let shutdown = state.shutdown.clone();
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let completed = Arc::new(AtomicBool::new(false));
        let operation = Operation::ApplyPostgresObject {
            session: sift_protocol::SessionId(7),
            connection: sift_protocol::ConnectionId(8),
            action: sift_protocol::PostgresObjectAction::ChangeObjectPrivilege {
                target: sift_protocol::PostgresPrivilegeTarget {
                    kind: sift_protocol::PostgresPrivilegeObjectKind::Table,
                    object_oid: 10,
                    column_number: None,
                },
                grantee: sift_protocol::PostgresPrivilegeGrantee::Public {},
                privilege: sift_protocol::PostgresObjectPrivilege::Select,
                change: sift_protocol::PostgresPrivilegeChange::Grant {
                    grant_option: false,
                },
            },
        };
        let auth = Some(Extension(AuthContext {
            principal_id: PrincipalId(1),
            tenants: vec![],
            auth_session_id: None,
            cookie_authenticated: false,
            access_expires_at: None,
            trusted_local: false,
            database_credential: false,
            token_tenant_scope: None,
        }));
        let (started, resume, done) = (entered.clone(), release.clone(), completed.clone());
        let observer = tokio::spawn(crate::correlation::scope(
            "privilege-original".into(),
            owned_audited_work(
                state,
                sift_protocol::SessionId(7),
                operation,
                auth,
                async move {
                    started.notify_one();
                    resume.notified().await;
                    done.store(true, Ordering::SeqCst);
                    Ok(())
                },
            ),
        ));
        entered.notified().await;
        assert_eq!(shutdown.in_flight(), 1);
        observer.abort();
        let _ = observer.await;
        assert!(sessions.list_operations().is_empty());
        release.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while sessions.list_operations().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(completed.load(Ordering::SeqCst));
        let operations = sessions.list_operations();
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].status, OperationStatus::Succeeded);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let rows = metadata.list_operation_audit(10).unwrap();
                if let Some(row) = rows.first() {
                    assert_eq!(row.actor_principal_id, Some(PrincipalId(1)));
                    assert_eq!(row.correlation_id.as_deref(), Some("privilege-original"));
                    assert_eq!(row.status, "succeeded");
                    assert_eq!(rows.len(), 1);
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(matches!(
            operations[0].operation,
            Operation::ApplyPostgresObject { .. }
        ));
    }

    #[tokio::test]
    async fn http_query_owner_admission_refuses_before_dispatch_and_recovers_capacity() {
        let sessions = SessionStore::new(crate::DriverRegistry::new());
        let mut held = (0..256)
            .map(|_| sessions.reserve_query_settlement().unwrap())
            .collect::<Vec<_>>();
        let state = AppState {
            sessions: sessions.clone(),
            rooms: Default::default(),
            auth: Default::default(),
            metadata: None,
            shutdown: Default::default(),
        };
        let request = ExecuteRequestHttp {
            connection: sift_protocol::ConnectionId(1),
            sql: "SELECT 1".into(),
            params: vec![],
            tx: None,
            room_id: None,
            connection_profile_id: None,
            transform: None,
            source: None,
        };
        let refused = execute_query(
            State(state.clone()),
            HeaderMap::new(),
            None,
            Path(sift_protocol::SessionId(999)),
            Json(request.clone()),
        )
        .await;
        assert!(matches!(
            refused,
            Err(ApiError::Driver(sift_protocol::DriverError {
                code: sift_protocol::Code::PoolExhausted,
                ..
            }))
        ));
        let refused_audit = sessions.list_operations();
        assert_eq!(refused_audit.len(), 1);
        assert_eq!(refused_audit[0].status, OperationStatus::Failed);
        assert_eq!(state.shutdown.in_flight(), 0);
        held.pop();
        let admitted = execute_query(
            State(state.clone()),
            HeaderMap::new(),
            None,
            Path(sift_protocol::SessionId(999)),
            Json(request),
        )
        .await;
        assert!(matches!(admitted, Err(ApiError::SessionNotFound(_))));
        let audits = sessions.list_operations();
        assert_eq!(audits.len(), 2);
        assert!(audits
            .iter()
            .all(|entry| entry.status == OperationStatus::Failed));
        assert_eq!(state.shutdown.in_flight(), 0);
    }
}
