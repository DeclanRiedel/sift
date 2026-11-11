//! Reviewed typed requests use the original Execute source and native apply owner.
use super::*;
use sift_protocol::*;
use uuid::Uuid;
fn operation(proposal: &AiDatabaseProposalDetail) -> OperationKind {
    match proposal.draft {
        AiDatabaseDraft::RowEditSet { .. } => OperationKind::ApplyEdits,
        _ => OperationKind::ApplyMigration,
    }
}
async fn delivery(
    state: &AppState,
    auth: &AuthContext,
    id: Uuid,
    full: bool,
) -> ApiResult<AiDatabaseExecutionDetail> {
    let actor = auth.principal_id;
    let metadata = metadata_store_cloned(state)?;
    let mut detail = metadata.ai_database_execution_detail(id, actor).await?;
    super::ai::check_ai_chat_scope(state, auth, detail.summary.chat_id).await?;
    let current = super::ai_execution::source_current_for(
        &state.sessions,
        &metadata,
        actor,
        &detail.source,
        operation(&detail.proposal),
        None,
        false,
    );
    if current.is_err() {
        if full {
            return Err(ApiError::Forbidden("Execution source changed; inspect the redacted claim and original database outcome".into()));
        }
        if let Some(receipt) = &mut detail.summary.receipt {
            receipt.row_result = None;
            receipt.migration_result = None;
        }
    }
    Ok(detail)
}
pub(super) async fn request(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
    Json(request): Json<RequestAiDatabaseExecution>,
) -> ApiResult<Json<AiDatabaseExecutionSummary>> {
    let auth = resolve_auth_context_blocking(state.clone(), headers.clone()).await?;
    let run = super::ai::ai_chat_id(&raw)?;
    super::execution::owned_audited_operation(
        state.clone(),
        Operation::Ai {
            action: "request_database_execution".into(),
            chat_id: None,
            run_id: Some(run),
        },
        Some(auth.principal_id.0),
        Box::pin(async move {
            super::ai_execution::enabled(&state)?;
            let metadata = metadata_store_cloned(&state)?;
            if let Some(prior) = metadata
                .prior_ai_database_execution(run, auth.principal_id, &request)
                .await?
            {
                return Ok(Json(
                    delivery(&state, &auth, prior.id, false).await?.summary,
                ));
            }
            super::ai::check_ai_run_scope(&state, &auth, run).await?;
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
            let proposal = metadata
                .ai_database_proposal(request.proposal_id, auth.principal_id)
                .await?;
            if proposal.proposal.run_id != run {
                return Err(ApiError::Forbidden(
                    "Typed execution requires a proposal from this run".into(),
                ));
            }
            super::ai_execution::source_current_for(
                &state.sessions,
                &metadata,
                auth.principal_id,
                &source,
                operation(&proposal),
                None,
                true,
            )?;
            let review = super::ai_database::review_ai_database_change(
                State(state.clone()),
                headers,
                Path(request.proposal_id.to_string()),
                Json(ReviewAiDatabaseProposalRequest {
                    client_request_id: request.call_id,
                    session: source.session,
                    connection: source.connection,
                }),
            )
            .await?
            .0;
            let expires = (authorized.started_at
                + chrono::Duration::seconds(i64::from(state.auth.ai.max_run_secs)))
            .min(review.expires_at);
            let result = metadata
                .create_ai_database_execution(
                    run,
                    auth.principal_id,
                    request,
                    review,
                    expires,
                    state.auth.ai.max_tool_calls_per_run,
                )
                .await?;
            Ok(Json(result))
        }),
    )
    .await
}
pub(super) async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
) -> ApiResult<Json<Vec<AiDatabaseExecutionSummary>>> {
    super::ai::ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let chat = super::ai::ai_chat_id(&raw)?;
    let result = async {
        super::ai::check_ai_chat_scope(&state, &auth, chat).await?;
        metadata_store_cloned(&state)?
            .ai_database_executions(chat, auth.principal_id)
            .await
            .map_err(Into::into)
    }
    .await;
    finish_operation_as(
        &state.sessions,
        Operation::Ai {
            action: "list_database_executions".into(),
            chat_id: Some(chat),
            run_id: None,
        },
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
) -> ApiResult<Json<AiDatabaseExecutionDetail>> {
    super::ai::ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let id = super::ai::ai_chat_id(&raw)?;
    let result = delivery(&state, &auth, id, true).await;
    finish_operation_as(
        &state.sessions,
        Operation::Ai {
            action: "inspect_database_execution".into(),
            chat_id: None,
            run_id: None,
        },
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
) -> ApiResult<Json<AiDatabaseExecutionSummary>> {
    super::ai::ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let id = super::ai::ai_chat_id(&raw)?;
    let result = delivery(&state, &auth, id, false)
        .await
        .map(|detail| detail.summary);
    finish_operation_as(
        &state.sessions,
        Operation::Ai {
            action: "inspect_database_execution_status".into(),
            chat_id: None,
            run_id: None,
        },
        result,
        Some(auth.principal_id.0),
        |_| None,
    )
    .map(Json)
}
pub(super) async fn approve(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
    Json(request): Json<ApproveAiDatabaseExecution>,
) -> ApiResult<Json<AiDatabaseExecutionSummary>> {
    let auth = resolve_auth_context_blocking(state.clone(), headers.clone()).await?;
    let id = super::ai::ai_chat_id(&raw)?;
    super::execution::owned_audited_operation(
        state.clone(),
        Operation::Ai {
            action: "approve_database_execution".into(),
            chat_id: None,
            run_id: None,
        },
        Some(auth.principal_id.0),
        Box::pin(async move {
            super::ai_execution::enabled(&state)?;
            let metadata = metadata_store_cloned(&state)?;
            let detail = metadata
                .ai_database_execution_detail(id, auth.principal_id)
                .await?;
            super::ai::check_ai_chat_scope(&state, &auth, detail.summary.chat_id).await?;
            if request.fingerprint != detail.summary.fingerprint {
                return Err(ApiError::BadRequest(
                    "Typed execution review changed".into(),
                ));
            }
            if detail.summary.state != AiExecutionState::AwaitingApproval {
                return Ok(Json(delivery(&state, &auth, id, false).await?.summary));
            }
            let apply = ApplyAiDatabaseProposalRequest {
                client_request_id: id,
                review_id: detail.review.id,
                review_digest: detail.review.review_digest,
                production_confirmation: request.production_confirmation,
                acknowledgements: request.acknowledgements,
            };
            let _ = super::ai_database::apply_ai_database_change_with_execution(
                State(state.clone()),
                headers,
                Path(detail.summary.proposal_id.to_string()),
                Json(apply),
                Some((id, request.fingerprint)),
            )
            .await?;
            Ok(Json(delivery(&state, &auth, id, false).await?.summary))
        }),
    )
    .await
}
pub(super) async fn deny(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
    Json(request): Json<ApproveAiDatabaseExecution>,
) -> ApiResult<Json<AiDatabaseExecutionSummary>> {
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let id = super::ai::ai_chat_id(&raw)?;
    super::execution::owned_audited_operation(
        state.clone(),
        Operation::Ai {
            action: "deny_database_execution".into(),
            chat_id: None,
            run_id: None,
        },
        Some(auth.principal_id.0),
        Box::pin(async move {
            super::ai::ai_enabled(&state)?;
            let metadata = metadata_store_cloned(&state)?;
            let summary = metadata
                .ai_database_execution(id, auth.principal_id)
                .await?;
            super::ai::check_ai_chat_scope(&state, &auth, summary.chat_id).await?;
            metadata
                .deny_ai_database_execution(id, auth.principal_id, request.fingerprint)
                .await?;
            Ok(Json(delivery(&state, &auth, id, false).await?.summary))
        }),
    )
    .await
}
pub(super) async fn reconcile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
    Json(request): Json<ApproveAiDatabaseExecution>,
) -> ApiResult<Json<AiDatabaseExecutionSummary>> {
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let id = super::ai::ai_chat_id(&raw)?;
    super::execution::owned_audited_operation(
        state.clone(),
        Operation::Ai {
            action: "reconcile_database_execution".into(),
            chat_id: None,
            run_id: None,
        },
        Some(auth.principal_id.0),
        Box::pin(async move {
            super::ai::ai_enabled(&state)?;
            let metadata = metadata_store_cloned(&state)?;
            let detail = metadata
                .ai_database_execution_detail(id, auth.principal_id)
                .await?;
            super::ai::check_ai_chat_scope(&state, &auth, detail.summary.chat_id).await?;
            if request.fingerprint != detail.summary.fingerprint {
                return Err(ApiError::BadRequest("Typed execution claim changed".into()));
            }
            metadata
                .reconcile_ai_database_apply(
                    detail.summary.proposal_id,
                    auth.principal_id,
                    ReconcileAiDatabaseApply {
                        review_id: detail.review.id,
                        client_request_id: id,
                        review_digest: detail.review.review_digest,
                    },
                )
                .await?;
            Ok(Json(delivery(&state, &auth, id, false).await?.summary))
        }),
    )
    .await
}
