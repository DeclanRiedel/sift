//! Human-reviewed governed actions; receipts describe RPC facts, not database effects.
use super::*;
use crate::extension_dispatch::{ActionAuthorization, AiExtensionDispatch};
use sift_protocol::*;
use uuid::Uuid;

fn op(action: &str, chat: Option<Uuid>, run: Option<Uuid>) -> Operation {
    Operation::Ai {
        action: action.into(),
        chat_id: chat,
        run_id: run,
    }
}
async fn summary(
    state: &AppState,
    auth: &AuthContext,
    id: Uuid,
) -> ApiResult<AiExtensionExecutionSummary> {
    let found = metadata_store_cloned(state)?
        .ai_extension_execution(id, auth.principal_id)
        .await?;
    super::ai::check_ai_chat_scope(state, auth, found.chat_id).await?;
    Ok(found)
}
fn source_current(
    state: &AppState,
    metadata: &MetadataStore,
    actor: PrincipalId,
    source: &AiExecutionSource,
    dispatch: bool,
) -> ApiResult<()> {
    super::ai_execution::source_current_for(
        &state.sessions,
        metadata,
        actor,
        source,
        OperationKind::InvokeExtension,
        None,
        dispatch,
    )
    .map(|_| ())
}
async fn detail(
    state: &AppState,
    auth: &AuthContext,
    headers: &HeaderMap,
    id: Uuid,
) -> ApiResult<AiExtensionExecutionDetail> {
    summary(state, auth, id).await?;
    let metadata = metadata_store_cloned(state)?;
    let mut result = metadata
        .ai_extension_execution_detail(id, auth.principal_id)
        .await?;
    if authorized_tool_context(state, auth, result.review.context.clone())? != result.review.context
    {
        return Err(ApiError::Forbidden(
            "Extension review target is no longer available".into(),
        ));
    }
    source_current(
        state,
        &metadata,
        auth.principal_id,
        &result.review.source,
        false,
    )?;
    if result.result.is_some() {
        let context = super::extension_actions::context(
            state,
            auth,
            headers.clone(),
            result.review.context.clone(),
        )?;
        let allowed = state.sessions.tool_registry().is_some_and(|registry| {
            registry
                .prepare_for_ai(&result.review.tool.id, &result.review.arguments, &context)
                .is_ok_and(|(tool, prepared, _)| {
                    tool == result.review.tool
                        && prepared.review_context() == result.review.prepared_contract
                        && registry
                            .dispatcher()
                            .delivery_authorized(&prepared, &context)
                })
        });
        if !allowed {
            result.result = None;
            result.summary.result_availability = AiExtensionResultAvailability::Withheld;
        }
    }
    Ok(result)
}
pub(super) async fn request(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
    Json(request): Json<RequestAiExtensionExecution>,
) -> ApiResult<Json<AiExtensionExecutionSummary>> {
    let auth = resolve_auth_context_blocking(state.clone(), headers.clone()).await?;
    let run = super::ai::ai_chat_id(&raw)?;
    super::execution::owned_audited_operation(
        state.clone(),
        op("request_extension_execution", None, Some(run)),
        Some(auth.principal_id.0),
        Box::pin(async move {
            super::ai_execution::enabled(&state)?;
            let metadata = metadata_store_cloned(&state)?;
            if let Some(prior) = metadata
                .prior_ai_extension_execution(run, auth.principal_id, &request)
                .await?
            {
                return Ok(Json(summary(&state, &auth, prior.id).await?));
            }
            super::ai::check_ai_run_scope(&state, &auth, run).await?;
            let active = metadata
                .ai_tool_run(run, auth.principal_id, request.lease_token)
                .await?;
            let source = metadata
                .ai_execute_run_source(run, auth.principal_id)
                .await?;
            source_current(&state, &metadata, auth.principal_id, &source, true)?;
            let target = authorized_tool_context(&state, &auth, active.context.target.clone())?;
            if target != active.context.target {
                return Err(ApiError::Forbidden(
                    "Original extension target changed".into(),
                ));
            }
            let context =
                super::extension_actions::context(&state, &auth, headers, target.clone())?;
            let registry = state
                .sessions
                .tool_registry()
                .ok_or(ApiError::MetadataUnavailable)?;
            let (tool, prepared, _) = registry
                .prepare_for_ai(&request.tool_id, &request.arguments, &context)
                .map_err(tool_error)?;
            let review = AiExtensionExecutionReview {
                tool,
                context: target,
                arguments: request.arguments.clone(),
                prepared_contract: prepared.review_context(),
                source,
            };
            let expires = (active.started_at
                + chrono::Duration::seconds(i64::from(state.auth.ai.max_run_secs)))
            .min(chrono::Utc::now() + chrono::Duration::minutes(5));
            let result = metadata
                .create_ai_extension_execution(
                    run,
                    auth.principal_id,
                    request,
                    review,
                    expires,
                    state.auth.ai.max_tool_calls_per_run,
                )
                .await?;
            Ok(Json(summary(&state, &auth, result.id).await?))
        }),
    )
    .await
}
pub(super) async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
) -> ApiResult<Json<Vec<AiExtensionExecutionSummary>>> {
    super::ai::ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let chat = super::ai::ai_chat_id(&raw)?;
    let result = async {
        super::ai::check_ai_chat_scope(&state, &auth, chat).await?;
        Ok(metadata_store_cloned(&state)?
            .ai_extension_executions(chat, auth.principal_id)
            .await?)
    }
    .await;
    finish_operation_as(
        &state.sessions,
        op("list_extension_executions", Some(chat), None),
        result,
        Some(auth.principal_id.0),
        |_| None,
    )
    .map(Json)
}
pub(super) async fn get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
) -> ApiResult<Json<AiExtensionExecutionDetail>> {
    super::ai::ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers.clone()).await?;
    let id = super::ai::ai_chat_id(&raw)?;
    let result = detail(&state, &auth, &headers, id).await;
    finish_operation_as(
        &state.sessions,
        op("inspect_extension_execution", None, None),
        result,
        Some(auth.principal_id.0),
        |_| None,
    )
    .map(Json)
}
pub(super) async fn status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
) -> ApiResult<Json<AiExtensionExecutionSummary>> {
    super::ai::ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let id = super::ai::ai_chat_id(&raw)?;
    let result = summary(&state, &auth, id).await;
    finish_operation_as(
        &state.sessions,
        op("inspect_extension_execution_status", None, None),
        result,
        Some(auth.principal_id.0),
        |_| None,
    )
    .map(Json)
}
pub(super) async fn deny(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
    Json(request): Json<DenyAiExtensionExecution>,
) -> ApiResult<Json<AiExtensionExecutionSummary>> {
    super::ai::ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let id = super::ai::ai_chat_id(&raw)?;
    super::execution::owned_audited_operation(
        state.clone(),
        op("deny_extension_execution", None, None),
        Some(auth.principal_id.0),
        Box::pin(async move {
            summary(&state, &auth, id).await?;
            metadata_store_cloned(&state)?
                .deny_ai_extension_execution(id, auth.principal_id, request.fingerprint)
                .await?;
            Ok(Json(summary(&state, &auth, id).await?))
        }),
    )
    .await
}
pub(super) async fn acknowledge(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
    Json(request): Json<AcknowledgeAiExtensionExecution>,
) -> ApiResult<Json<AiExtensionExecutionSummary>> {
    super::ai::ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let id = super::ai::ai_chat_id(&raw)?;
    super::execution::owned_audited_operation(
        state.clone(),
        op("acknowledge_extension_execution", None, None),
        Some(auth.principal_id.0),
        Box::pin(async move {
            summary(&state, &auth, id).await?;
            metadata_store_cloned(&state)?
                .acknowledge_ai_extension_execution(id, auth.principal_id, request)
                .await?;
            Ok(Json(summary(&state, &auth, id).await?))
        }),
    )
    .await
}
pub(super) async fn approve(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
    Json(request): Json<ApproveAiExtensionExecution>,
) -> ApiResult<Json<AiExtensionExecutionSummary>> {
    let auth = resolve_auth_context_blocking(state.clone(), headers.clone()).await?;
    let actor = auth.principal_id;
    let id = super::ai::ai_chat_id(&raw)?;
    let correlation = crate::correlation::current().unwrap_or_else(crate::correlation::generate);
    let mut audit = crate::correlation::sync_scope(correlation.clone(), || {
        query_audit::QueryOperationAudit::new(
            state.sessions.clone(),
            op("execute_extension", None, None),
            Some(actor.0),
        )
    });
    let permit = match (|| {
        super::ai_execution::enabled(&state)?;
        if state.shutdown.is_draining() {
            return Err(ApiError::ServiceDraining);
        }
        state.sessions.reserve_query_settlement()
    })() {
        Ok(permit) => permit,
        Err(error) => {
            audit.failure(&error);
            return Err(error);
        }
    };
    let guard = Arc::new(state.shutdown.track_query());
    let worker = state.clone();
    let (send, receive) = tokio::sync::oneshot::channel();
    let response = Arc::new(std::sync::Mutex::new(Some(send)));
    tokio::spawn(crate::correlation::scope(
        correlation,
        crate::shutdown::scope_query(Some(guard), async move {
            let _permit = permit;
            let result:ApiResult<Option<ExtensionExecutionReceipt>>=async {
            let fresh=resolve_auth_context_blocking(worker.clone(),headers.clone()).await?;
            if fresh.principal_id!=actor {return Err(ApiError::Unauthorized);}
            let current=summary(&worker,&fresh,id).await?;audit.bind_ai_context(current.chat_id,Some(current.run_id));
            if current.fingerprint!=request.fingerprint {return Err(ApiError::Conflict("Extension review changed".into()));}
            if current.state!=AiExecutionState::AwaitingApproval {
                if let Some(send)=response.lock().unwrap().take(){let _=send.send(Ok(current));}
                return Ok(None);
            }
            let reviewed=detail(&worker,&fresh,&headers,id).await?;let metadata=metadata_store_cloned(&worker)?;
            if reviewed.review.tool.operation.classification!=request.acknowledged_classification {return Err(ApiError::BadRequest("Acknowledge the exact reviewed extension classification".into()));}
            if reviewed.review.source.production && request.production_confirmation.as_deref()!=Some(reviewed.review.source.database_label.as_str()) {
                return Err(ApiError::BadRequest("Type the exact reviewed database label for this production extension action".into()));
            }
            source_current(&worker,&metadata,actor,&reviewed.review.source,true)?;
            metadata.require_ai_execution_live(reviewed.summary.run_id,actor)?;
            let mut dispatch=super::extension_actions::context(&worker,&fresh,headers,reviewed.review.context.clone())?;
            let registry=worker.sessions.tool_registry().ok_or(ApiError::MetadataUnavailable)?;
            let (tool,prepared,binding)=registry.prepare_for_ai(&reviewed.review.tool.id,&reviewed.review.arguments,&dispatch).map_err(tool_error)?;
            if tool!=reviewed.review.tool || prepared.review_context()!=reviewed.review.prepared_contract {return Err(ApiError::Conflict("Registered extension contract changed; request a new review".into()));}
            let work=worker.sessions.register_ai_work(reviewed.summary.run_id)?;
            let token=work.token.clone();let run=reviewed.summary.run_id;let expires=reviewed.summary.expires_at;
            let prior=dispatch.reauthorize.take();let verification=worker.clone();let original=reviewed.review.source.clone();
            let live_registry=registry.clone();let frozen=Arc::new(reviewed.review.clone());let live_context=dispatch.clone();
            let authority:ActionAuthorization=Arc::new(move |classification| {
                let prior=prior.clone();let verification=verification.clone();let original=original.clone();let token=token.clone();
                let live_registry=live_registry.clone();let frozen=frozen.clone();let live_context=live_context.clone();
                Box::pin(async move {
                    if token.is_cancelled() || chrono::Utc::now()>=expires {return Err(sift_plugin_host::SupervisorError::AdmissionRefused);}
                    if let Some(prior)=prior {prior(classification).await?;}
                    metadata_blocking(move || {
                        let metadata=metadata_store_cloned(&verification)?;metadata.require_ai_execution_live(run,actor)?;
                        source_current(&verification,&metadata,actor,&original,true)?;
                        let (tool,prepared,_)=live_registry.prepare_for_ai(&frozen.tool.id,&frozen.arguments,&live_context).map_err(tool_error)?;
                        if tool!=frozen.tool || prepared.review_context()!=frozen.prepared_contract {return Err(ApiError::Forbidden("Reviewed extension registration changed".into()));}
                        Ok(())
                    }).await.map_err(|_|sift_plugin_host::SupervisorError::AdmissionRefused)
                })
            });
            dispatch.reauthorize=Some(authority);
            let delivery=response.clone();let ai=AiExtensionDispatch {id,fingerprint:request.fingerprint,deadline_unix_ms:expires.timestamp_millis(),work,on_claim:Some(Box::new(move |summary| {
                if let Some(send)=delivery.lock().unwrap().take(){let _=send.send(Ok(summary));}
            }))};
            let observed=registry.dispatcher().dispatch_ai_reviewed(prepared,reviewed.review.arguments,dispatch,binding,ai).await
                .map_err(|error|tool_error(error.into()))?;
            if observed.replayed {return Ok(None);}
            if observed.receipt.state==ExtensionExecutionState::Acknowledged && !observed.receipt.late_response {
                // Failure to retain a private body never erases journal settlement.
                let _=metadata.retain_ai_extension_result(id,actor,observed.receipt.id,observed.result).await;
            }
            Ok(Some(observed.receipt))
        }.await;
            match &result {
                Ok(Some(receipt)) => {
                    let success = receipt.state == ExtensionExecutionState::Acknowledged
                        && receipt.response_kind == Some(ExtensionResponseKind::Result)
                        && receipt.result_code == Some(ExtensionExecutionCode::Acknowledged)
                        && !receipt.late_response;
                    let code = receipt
                        .result_code
                        .and_then(|code| serde_json::to_value(code).ok())
                        .and_then(|v| v.as_str().map(str::to_owned));
                    audit.record(
                        if success {
                            OperationStatus::Succeeded
                        } else {
                            OperationStatus::Failed
                        },
                        code,
                        None,
                        None,
                    );
                }
                Ok(None) => audit.record(
                    OperationStatus::Succeeded,
                    Some("replayed".into()),
                    None,
                    None,
                ),
                Err(error) => audit.failure(error),
            }
            if let Some(send) = response.lock().unwrap().take() {
                let _ = send.send(result.and_then(|_| {
                    Err(ApiError::Internal(
                        "Extension owner did not publish its claim; inspect status before retrying"
                            .into(),
                    ))
                }));
            }
        }),
    ));
    receive
        .await
        .map_err(|_| {
            ApiError::Internal(
                "Extension owner failed; inspect its durable status before retrying".into(),
            )
        })?
        .map(Json)
}
