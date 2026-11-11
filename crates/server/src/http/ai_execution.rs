//! Interactive human-approved SQL; native outcomes outlive result observation.
use super::*;
use crate::session::native_execution::{self, NativeExecution, StartupGuard};
use chrono::Utc;
use sift_protocol::*;
use uuid::Uuid;

pub(super) fn enabled(state: &AppState) -> ApiResult<()> {
    super::ai::ai_enabled(state)?;
    if !state.auth.ai.execution_enabled {
        return Err(ApiError::Forbidden(
            "AI Execute is disabled by instance policy".into(),
        ));
    }
    Ok(())
}
fn source_startup_supported(engine: Engine, configuration: &serde_json::Value) -> bool {
    if engine == Engine::SqlServer {
        return crate::session::ai_execution_startup_supported(configuration);
    }
    configuration
        .pointer("/engine_specific/startup_sql")
        .map_or(true, |sql| {
            sql.as_array().is_some_and(|statements| {
                statements
                    .iter()
                    .all(|sql| sql.as_str().is_some_and(|sql| sql.trim().is_empty()))
            })
        })
}
pub(super) async fn capture_source(
    state: &AppState,
    auth: &AuthContext,
    context: &AiTurnContext,
) -> ApiResult<AiExecutionSource> {
    capture_target(state, auth, &context.target).await
}

pub(super) async fn capture_target(
    state: &AppState,
    auth: &AuthContext,
    original: &ToolContext,
) -> ApiResult<AiExecutionSource> {
    let target = authorized_tool_context(state, auth, original.clone())?;
    // Execute startup already requires a private chat. A private conversation
    // can edit a shared document without borrowing a room database connection.
    // The managed source below must still belong to the initiating actor.
    if target != *original || target.room_id.is_some() && target.document_id.is_none() {
        return Err(ApiError::Forbidden(
            "Execute requires the original target and a document for room context".into(),
        ));
    }
    let (session, connection) = super::ai::ai_target_connection_ids(original)?;
    let (owner, tenant, profile, _) =
        state
            .sessions
            .managed_catalog_scope(session, connection, OperationKind::ExecuteQuery)?;
    if owner != auth.principal_id || target.profile_id != Some(profile.0) {
        return Err(ApiError::Forbidden(
            "Execute requires your managed source connection".into(),
        ));
    }
    let metadata = metadata_store_cloned(state)?;
    let profile_record = metadata.get_connection_profile(tenant, profile)?;
    let entry = state.sessions.conn_entry(session, connection)?;
    if !source_startup_supported(
        entry.driver.semantic_engine().ok_or_else(|| {
            ApiError::BadRequest("Execute requires a supported SQL dialect".into())
        })?,
        &entry.configuration,
    ) {
        return Err(ApiError::BadRequest("Execute refuses arbitrary startup SQL; use typed session variables or supported SQL Server session-only SET options and reconnect".into()));
    }
    if entry.configuration != profile_record.configuration {
        return Err(ApiError::Forbidden(
            "Reconnect the changed profile before starting Execute".into(),
        ));
    }
    Ok(AiExecutionSource {
        tenant_id: tenant.0,
        profile_id: profile.0,
        session,
        connection,
        generation: entry.execution.review_id,
        profile_digest: metadata.ai_profile_source_digest(profile, owner).await?,
        database_label: format!(
            "{} / {}",
            profile_record.name,
            connection_database_target(&profile_record)
                .unwrap_or_else(|| "default database".into())
        ),
        production: profile_record.tags.iter().any(|tag| {
            matches!(
                tag.to_ascii_lowercase().as_str(),
                "prod" | "production" | "environment:prod" | "environment:production"
            )
        }),
        engine: entry.driver.semantic_engine().ok_or_else(|| {
            ApiError::BadRequest("Execute requires a supported SQL dialect".into())
        })?,
    })
}
fn source_current(
    sessions: &SessionStore,
    metadata: &MetadataStore,
    actor: PrincipalId,
    source: &AiExecutionSource,
    sql: &str,
) -> ApiResult<crate::session::ConnectionEntryClone> {
    source_current_for(
        sessions,
        metadata,
        actor,
        source,
        OperationKind::ExecuteQuery,
        Some(sql),
        true,
    )
}
pub(super) fn source_current_for(
    sessions: &SessionStore,
    metadata: &MetadataStore,
    actor: PrincipalId,
    source: &AiExecutionSource,
    operation: OperationKind,
    sql: Option<&str>,
    check_transaction: bool,
) -> ApiResult<crate::session::ConnectionEntryClone> {
    let entry = sessions.authorize_connection_operation(
        source.session,
        source.connection,
        operation,
        sql,
        &[],
    )?;
    if !source_startup_supported(source.engine, &entry.configuration)
        || entry.execution.review_id != source.generation
        || !entry.execution.autocommit_known()
        || check_transaction
            && sessions.connection_has_transaction(source.session, source.connection)?
        || metadata.ai_profile_source_digest_now(ConnectionProfileId(source.profile_id), actor)?
            != source.profile_digest
    {
        return Err(ApiError::Forbidden("Execution source changed or has an active/uncertain transaction; start a fresh Execute run".into()));
    }
    if !matches!(entry.provenance,crate::session::ConnectionProvenance::Managed {principal_id,tenant_id,profile_id,..} if principal_id==actor && tenant_id.0==source.tenant_id && profile_id.0==source.profile_id)
    {
        return Err(ApiError::Forbidden(
            "Execution source identity changed".into(),
        ));
    }
    Ok(entry)
}
fn statement(engine: Engine, sql: &str) -> ApiResult<Option<(ChangeLedgerOperation, bool)>> {
    let effect = crate::sql_policy::statement_effect(Some(engine), sql);
    if effect.is_some_and(|(_, ordinary)| !ordinary) {
        return Err(ApiError::Forbidden("This execution surface requires a supported single read/DML/DDL statement; transaction/session control and opaque SQL need a separate execution family".into()));
    }
    if effect.is_none() {
        crate::sql_policy::enforce_ai_select(&ConnectionPolicy::default(), engine, sql)?;
    }
    Ok(effect)
}
pub(super) async fn request_sql(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
    Json(request): Json<RequestAiSqlExecution>,
) -> ApiResult<Json<AiSqlExecutionSummary>> {
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let run = super::ai::ai_chat_id(&raw)?;
    let metadata = metadata_store_cloned(&state)?;
    let operation = Operation::Ai {
        action: "request_sql_execution".into(),
        chat_id: None,
        run_id: Some(run),
    };
    super::execution::owned_audited_operation(
        state.clone(),
        operation,
        Some(auth.principal_id.0),
        async move {
            enabled(&state)?;
            if let Some(prior) = metadata
                .prior_ai_sql_execution(run, auth.principal_id, &request)
                .await?
            {
                return Ok(Json(
                    delivery(&state, auth.principal_id, prior.id).await?.summary,
                ));
            }
            let authorized = metadata
                .ai_tool_run(run, auth.principal_id, request.lease_token)
                .await?;
            if authorized.mode != AiMode::Execute || authorized.visibility != AiVisibility::Private
            {
                return Err(ApiError::Forbidden(
                    "An active private Execute run is required".into(),
                ));
            }
            let source = metadata
                .ai_execute_run_source(run, auth.principal_id)
                .await?;
            source_current(
                &state.sessions,
                &metadata,
                auth.principal_id,
                &source,
                &request.sql,
            )?;
            statement(source.engine, &request.sql)?;
            let expires = (authorized.started_at
                + chrono::Duration::seconds(i64::from(state.auth.ai.max_run_secs)))
            .min(Utc::now() + chrono::Duration::minutes(5));
            let input = AiSqlExecutionInput {
                sql: request.sql.clone(),
                params: request.params.clone(),
                source,
            };
            let action = metadata
                .create_ai_sql_execution(
                    run,
                    auth.principal_id,
                    request,
                    input,
                    expires,
                    state.auth.ai.max_tool_calls_per_run,
                )
                .await?;
            Ok(Json(
                delivery(&state, auth.principal_id, action.id)
                    .await?
                    .summary,
            ))
        },
    )
    .await
}
async fn delivery(
    state: &AppState,
    actor: PrincipalId,
    id: Uuid,
) -> ApiResult<AiSqlExecutionDetail> {
    let metadata = metadata_store_cloned(state)?;
    let mut detail = metadata.ai_sql_execution(id, actor).await?;
    if source_current(
        &state.sessions,
        &metadata,
        actor,
        &detail.input.source,
        &detail.input.sql,
    )
    .is_err()
    {
        if let Some(receipt) = &mut detail.summary.receipt {
            receipt.result = None;
        }
    }
    Ok(detail)
}
pub(super) async fn get_sql(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
) -> ApiResult<Json<AiSqlExecutionDetail>> {
    super::ai::ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let id = super::ai::ai_chat_id(&raw)?;
    let result = delivery(&state, auth.principal_id, id).await;
    finish_operation_as(
        &state.sessions,
        Operation::Ai {
            action: "inspect_sql_execution".into(),
            chat_id: result.as_ref().ok().map(|d| d.summary.chat_id),
            run_id: result.as_ref().ok().map(|d| d.summary.run_id),
        },
        result,
        Some(auth.principal_id.0),
        |_| None,
    )
    .map(Json)
}
pub(super) async fn status_sql(
    state: State<AppState>,
    headers: HeaderMap,
    path: Path<String>,
) -> ApiResult<Json<AiSqlExecutionSummary>> {
    get_sql(state, headers, path)
        .await
        .map(|Json(d)| Json(d.summary))
}
pub(super) async fn list_sql(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
) -> ApiResult<Json<Vec<AiSqlExecutionSummary>>> {
    super::ai::ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let chat = super::ai::ai_chat_id(&raw)?;
    super::ai::check_ai_chat_scope(&state, &auth, chat).await?;
    let result = metadata_store_cloned(&state)?
        .list_ai_sql_executions(chat, auth.principal_id)
        .await
        .map_err(Into::into);
    finish_operation_as(
        &state.sessions,
        Operation::Ai {
            action: "list_sql_executions".into(),
            chat_id: Some(chat),
            run_id: None,
        },
        result,
        Some(auth.principal_id.0),
        |_| None,
    )
    .map(Json)
}
pub(super) async fn deny_sql(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
    Json(request): Json<ApproveAiSqlExecution>,
) -> ApiResult<Json<AiSqlExecutionSummary>> {
    super::ai::ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let id = super::ai::ai_chat_id(&raw)?;
    let worker = state.clone();
    super::execution::owned_audited_operation(
        state,
        Operation::Ai {
            action: "deny_sql_execution".into(),
            chat_id: None,
            run_id: None,
        },
        Some(auth.principal_id.0),
        async move {
            let metadata = metadata_store_cloned(&worker)?;
            metadata
                .deny_ai_sql_execution(id, auth.principal_id, request.fingerprint)
                .await?;
            Ok(Json(
                delivery(&worker, auth.principal_id, id).await?.summary,
            ))
        },
    )
    .await
}
pub(super) async fn reconcile_sql(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
    Json(request): Json<ApproveAiSqlExecution>,
) -> ApiResult<Json<AiSqlExecutionSummary>> {
    super::ai::ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let id = super::ai::ai_chat_id(&raw)?;
    let worker = state.clone();
    super::execution::owned_audited_operation(
        state,
        Operation::Ai {
            action: "reconcile_sql_execution".into(),
            chat_id: None,
            run_id: None,
        },
        Some(auth.principal_id.0),
        async move {
            let metadata = metadata_store_cloned(&worker)?;
            metadata
                .reconcile_ai_sql_execution(id, auth.principal_id, request.fingerprint)
                .await?;
            Ok(Json(
                delivery(&worker, auth.principal_id, id).await?.summary,
            ))
        },
    )
    .await
}
pub(super) async fn approve_sql(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
    Json(request): Json<ApproveAiSqlExecution>,
) -> ApiResult<Json<AiSqlExecutionSummary>> {
    let auth = resolve_auth_context_blocking(state.clone(), headers.clone()).await?;
    let id = super::ai::ai_chat_id(&raw)?;
    let correlation = crate::correlation::current().unwrap_or_else(crate::correlation::generate);
    let mut audit = crate::correlation::sync_scope(correlation.clone(), || {
        query_audit::QueryOperationAudit::new(
            state.sessions.clone(),
            Operation::Ai {
                action: "execute_sql".into(),
                chat_id: None,
                run_id: None,
            },
            Some(auth.principal_id.0),
        )
    });
    let prepared = (|| {
        enabled(&state)?;
        if state.shutdown.is_draining() {
            return Err(ApiError::ServiceDraining);
        }
        state.sessions.reserve_query_settlement()
    })();
    let permit = match prepared {
        Ok(p) => p,
        Err(e) => {
            audit.failure(&e);
            return Err(e);
        }
    };
    let guard = Arc::new(state.shutdown.track_query());
    let worker = state.clone();
    let actor = auth.principal_id;
    let (send, receive) = tokio::sync::oneshot::channel();
    tokio::spawn(crate::correlation::scope(
        correlation,
        crate::shutdown::scope_query(Some(guard), async move {
            let _permit = permit;
            let mut response = Some(send);
            let result: ApiResult<()> = async {
                let fresh = resolve_auth_context_blocking(worker.clone(), headers).await?;
                if fresh.principal_id != actor {
                    return Err(ApiError::Unauthorized);
                }
                let metadata = metadata_store_cloned(&worker)?;
                let mut detail = metadata.ai_sql_execution(id, actor).await?;
                audit.bind_ai_context(detail.summary.chat_id, Some(detail.summary.run_id));
                if detail.summary.fingerprint != request.fingerprint {
                    return Err(ApiError::Conflict("Execution review changed".into()));
                }
                if detail.summary.state != AiExecutionState::AwaitingApproval {
                    let summary = delivery(&worker, actor, id).await?.summary;
                    let _ = response.take().unwrap().send(Ok(summary));
                    return Ok(());
                }
                if detail.input.source.production
                    && request.production_confirmation.as_deref()
                        != Some(detail.input.source.database_label.as_str())
                {
                    return Err(ApiError::BadRequest(
                        "Type the exact reviewed database label to approve production SQL".into(),
                    ));
                }
                let entry = source_current(
                    &worker.sessions,
                    &metadata,
                    actor,
                    &detail.input.source,
                    &detail.input.sql,
                )?;
                let effect = statement(detail.input.source.engine, &detail.input.sql)?;
                metadata.require_ai_execution_live(detail.summary.run_id, actor)?;
                let work = worker.sessions.register_ai_work(detail.summary.run_id)?;
                if !metadata
                    .claim_ai_sql_execution(id, actor, request.fingerprint)
                    .await?
                {
                    let _ = response
                        .take()
                        .unwrap()
                        .send(Ok(delivery(&worker, actor, id).await?.summary));
                    return Ok(());
                }
                detail.summary.state = AiExecutionState::Applying;
                detail.summary.approved_by = Some(actor.0);
                let _ = response.take().unwrap().send(Ok(detail.summary.clone()));
                execute_owned(&worker, &metadata, actor, detail, entry, work, effect).await
            }
            .await;
            match &result {
                Ok(()) => audit.record(OperationStatus::Succeeded, None, None, None),
                Err(e) => audit.failure(e),
            }
            if let Some(send) = response {
                let _ = send.send(result.and_then(|_| {
                    Err(ApiError::Internal(
                        "Execution did not return its state".into(),
                    ))
                }));
            }
        }),
    ));
    receive
        .await
        .map_err(|_| {
            ApiError::Internal(
                "Execution owner failed; inspect the durable action before retrying".into(),
            )
        })?
        .map(Json)
}
async fn execute_owned(
    state: &AppState,
    metadata: &MetadataStore,
    actor: PrincipalId,
    detail: AiSqlExecutionDetail,
    entry: crate::session::ConnectionEntryClone,
    work: crate::ai_cancellation::AiWorkGuard,
    effect: Option<(ChangeLedgerOperation, bool)>,
) -> ApiResult<()> {
    let source = detail.input.source.clone();
    let run = detail.summary.run_id;
    let deadline = detail.summary.expires_at;
    let token = work.token.clone();
    let expiry = token.clone();
    let duration = (deadline - Utc::now()).to_std().unwrap_or_default();
    let _timer = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        tokio::time::sleep(duration).await;
        expiry.cancel();
    }));
    let database_target =
        configured_database_target(&entry.driver.provider().provider_id, &entry.configuration);
    let bound = state
        .sessions
        .bind_source(source.session, source.connection, entry.clone());
    let verifier = state.sessions.clone();
    let meta = metadata.clone();
    let pinned = source.clone();
    let sql = detail.input.sql.clone();
    let cancellation = token.clone();
    let authority = Arc::new(move || {
        if cancellation.is_cancelled() || Utc::now() >= deadline {
            return Err(ApiError::Forbidden(
                "AI execution stopped or expired".into(),
            ));
        }
        meta.require_ai_execution_live(run, actor)?;
        source_current(&verifier, &meta, actor, &pinned, &sql).map(|_| ())
    });
    let request = ExecuteRequestHttp {
        connection: source.connection,
        sql: detail.input.sql.clone(),
        params: detail.input.params.clone(),
        tx: None,
        room_id: None,
        connection_profile_id: Some(source.profile_id),
        transform: None,
        source: None,
    };
    let mut audit = query_audit::QueryOperationAudit::new(
        state.sessions.clone(),
        Operation::ExecuteQuery {
            session: source.session,
            request: request.clone(),
        },
        Some(actor.0),
    );
    let native = Arc::new(NativeExecution::default());
    let startup = StartupGuard(native.clone());
    let result=native_execution::scope(native.clone(),crate::ai_cancellation::scope(token.clone(),async {
        tokio::select! { biased;
            result=async {bound?.with_source_authority(authority).execute_http_bounded(source.session,request,OperationKind::ExecuteQuery,Some((100,state.auth.ai.max_tool_result_bytes as usize)),false,Some(entry)).await}=>result,
            _=token.cancelled()=>Err(ApiError::Driver(DriverError::new(Code::QueryCanceled,"AI execution observation stopped"))),
        }
    })).await;
    drop(startup);
    match &result {
        Ok(r) => audit.record(
            OperationStatus::Succeeded,
            None,
            Some(r.rows.len() as i64),
            None,
        ),
        Err(e) => audit.failure(e),
    }
    let terminal = native.terminal().await;
    let outcome = effect.filter(|_| terminal.attempted).map(|_| {
        if terminal.completed && !terminal.transaction_unknown {
            ChangeLedgerOutcome::Committed
        } else {
            ChangeLedgerOutcome::OutcomeUnknown
        }
    });
    let phase = if !terminal.attempted {
        AiExecutionState::Denied
    } else if terminal.completed && !terminal.transaction_unknown {
        AiExecutionState::Completed
    } else {
        AiExecutionState::OutcomeUnknown
    };
    if terminal.attempted {
        if let Some((operation, _)) = effect {
            let input = sift_metadata::NewChangeLedgerEntry {
                tenant_id: Some(source.tenant_id),
                room_id: None,
                connection_profile_id: Some(source.profile_id),
                database_target,
                operation,
                affected_object: None,
                row_count: if terminal.completed {
                    terminal.rows.and_then(|n| i64::try_from(n).ok())
                } else {
                    None
                },
                sql_fingerprint: Some(crate::fingerprint::sql(&detail.input.sql)),
                row_identity_fingerprint: None,
                transaction_id: None,
                correlation_id: crate::correlation::current(),
                workspace_id: None,
                workspace_revision: None,
                checkpoint_id: None,
                workspace_path: None,
                git_commit: None,
                source_workflow: "ai_execute".into(),
                authored_by: Some(actor.0),
                approved_by: Some(actor.0),
                executed_by: actor.0,
                database_actor: None,
                outcome: outcome.unwrap(),
                result_code: terminal.code.clone(),
                identity_source: ChangeIdentitySource::Sift,
                identity_confidence: ChangeIdentityConfidence::Authenticated,
            };
            let meta = metadata.clone();
            if metadata_blocking(move || meta.append_change_ledger(input).map_err(Into::into))
                .await
                .is_err()
            {
                tracing::error!("AI native execution ledger persistence failed");
            }
        }
    }
    let mut result_code = result.as_ref().err().map(|e| match e {
        ApiError::Driver(e) => crate::error::audit_code(&e.code),
        _ => "ai_execution_refused".into(),
    });
    let result = result.ok().and_then(|r| {
        if r.rows.len() <= 100
            && serde_json::to_vec(&r)
                .is_ok_and(|v| v.len() <= state.auth.ai.max_tool_result_bytes as usize)
        {
            Some(r)
        } else {
            result_code = Some("result_too_large".into());
            None
        }
    });
    let result =
        if source_current(&state.sessions, metadata, actor, &source, &detail.input.sql).is_ok() {
            result
        } else {
            result_code = Some("execution_source_changed".into());
            None
        };
    metadata
        .finish_ai_sql_execution(
            detail.summary.id,
            actor,
            detail.summary.fingerprint,
            phase,
            AiExecutionReceipt {
                native_outcome: outcome,
                affected_rows: terminal.rows,
                result,
                result_code,
            },
        )
        .await?;
    if phase == AiExecutionState::Completed {
        Ok(())
    } else {
        Err(ApiError::Conflict(
            "SQL execution did not establish a complete known native outcome; inspect its journal"
                .into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn startup_transactions_and_batches_cannot_claim_autocommit_execution() {
        for engine in [Engine::Postgres, Engine::SqlServer, Engine::Sqlite] {
            assert!(source_startup_supported(engine, &serde_json::json!({})));
            assert!(source_startup_supported(
                engine,
                &serde_json::json!({"engine_specific":{"session_variables":{"context":"reviewed"},"startup_sql":[" "]}})
            ));
            for sql in [
                "BEGIN",
                "SET IMPLICIT_TRANSACTIONS ON",
                "USE another_database; SELECT 1",
            ] {
                assert!(!source_startup_supported(
                    engine,
                    &serde_json::json!({"engine_specific":{"startup_sql":[sql]}})
                ));
            }
        }
        assert!(source_startup_supported(
            Engine::SqlServer,
            &serde_json::json!({"engine_specific":{"startup_sql":["SET LOCK_TIMEOUT 1000"]}})
        ));
    }
}
