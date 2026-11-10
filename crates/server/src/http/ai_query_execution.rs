//! Reviewed query text actions; neither proposal status nor a transcript proves an edit.
use super::*;
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
) -> ApiResult<AiQueryExecutionSummary> {
    let result = metadata_store_cloned(state)?
        .ai_query_execution(id, auth.principal_id)
        .await?;
    super::ai::check_ai_chat_scope(state, auth, result.chat_id).await?;
    Ok(result)
}
async fn detail(
    state: &AppState,
    auth: &AuthContext,
    id: Uuid,
) -> ApiResult<AiQueryExecutionDetail> {
    summary(state, auth, id).await?;
    let metadata = metadata_store_cloned(state)?;
    let result = metadata
        .ai_query_execution_detail(id, auth.principal_id)
        .await?;
    if let AiQueryExecutionTarget::RoomDocument {
        room_id,
        document_id,
    } = result.target
    {
        let document = metadata.get_document_for_principal(
            DocumentId(document_id),
            auth.principal_id,
            false,
        )?;
        if document.room_id.0 != room_id {
            return Err(ApiError::Forbidden("Query document target changed".into()));
        }
    }
    Ok(result)
}
pub(super) async fn request(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
    Json(request): Json<RequestAiQueryExecution>,
) -> ApiResult<Json<AiQueryExecutionSummary>> {
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let run = super::ai::ai_chat_id(&raw)?;
    super::execution::owned_audited_operation(
        state.clone(),
        op("request_query_execution", None, Some(run)),
        Some(auth.principal_id.0),
        Box::pin(async move {
            super::ai_execution::enabled(&state)?;
            let metadata = metadata_store_cloned(&state)?;
            if let Some(prior) = metadata
                .prior_ai_query_execution(run, auth.principal_id, &request)
                .await?
            {
                return Ok(Json(summary(&state, &auth, prior.id).await?));
            }
            super::ai::check_ai_run_scope(&state, &auth, run).await?;
            let active = metadata
                .ai_tool_run(run, auth.principal_id, request.lease_token)
                .await?;
            let expires = (active.started_at
                + chrono::Duration::seconds(i64::from(state.auth.ai.max_run_secs)))
            .min(chrono::Utc::now() + chrono::Duration::minutes(5));
            let created = metadata
                .create_ai_query_execution(
                    run,
                    auth.principal_id,
                    request,
                    expires,
                    state.auth.ai.max_tool_calls_per_run,
                )
                .await?;
            Ok(Json(created))
        }),
    )
    .await
}
pub(super) async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
) -> ApiResult<Json<Vec<AiQueryExecutionSummary>>> {
    super::ai::ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let chat = super::ai::ai_chat_id(&raw)?;
    let result = async {
        super::ai::check_ai_chat_scope(&state, &auth, chat).await?;
        Ok(metadata_store_cloned(&state)?
            .ai_query_executions(chat, auth.principal_id)
            .await?)
    }
    .await;
    finish_operation_as(
        &state.sessions,
        op("list_query_executions", Some(chat), None),
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
) -> ApiResult<Json<AiQueryExecutionDetail>> {
    super::ai::ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let id = super::ai::ai_chat_id(&raw)?;
    let result = detail(&state, &auth, id).await;
    finish_operation_as(
        &state.sessions,
        op("inspect_query_execution", None, None),
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
) -> ApiResult<Json<AiQueryExecutionSummary>> {
    super::ai::ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let id = super::ai::ai_chat_id(&raw)?;
    let result = summary(&state, &auth, id).await;
    finish_operation_as(
        &state.sessions,
        op("inspect_query_execution_status", None, None),
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
    Json(request): Json<ApproveAiQueryExecution>,
) -> ApiResult<Response> {
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let id = super::ai::ai_chat_id(&raw)?;
    let result = super::execution::owned_audited_operation(
        state.clone(),
        op("approve_query_execution", None, None),
        Some(auth.principal_id.0),
        Box::pin(async move {
            super::ai_execution::enabled(&state)?;
            let current = summary(&state, &auth, id).await?;
            if current.fingerprint != request.fingerprint {
                return Err(ApiError::BadRequest("Query approval changed".into()));
            }
            if current.state != AiExecutionState::AwaitingApproval {
                return Ok(AiQueryExecutionApproval {
                    summary: current,
                    dispatch: None,
                });
            }
            let reviewed = detail(&state, &auth, id).await?;
            let metadata = metadata_store_cloned(&state)?;
            let (claimed, dispatch) = metadata
                .claim_ai_query_execution(id, auth.principal_id, request)
                .await?;
            if claimed {
                if let AiQueryExecutionTarget::RoomDocument {
                    room_id,
                    document_id,
                } = reviewed.target
                {
                    apply_room(
                        &state,
                        auth.principal_id,
                        reviewed,
                        room_id,
                        DocumentId(document_id),
                    )
                    .await?;
                }
            }
            Ok(AiQueryExecutionApproval {
                summary: summary(&state, &auth, id).await?,
                dispatch,
            })
        }),
    )
    .await?;
    let mut response = Json(result).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    Ok(response)
}
async fn apply_room(
    state: &AppState,
    actor: PrincipalId,
    reviewed: AiQueryExecutionDetail,
    room: i64,
    document: DocumentId,
) -> ApiResult<()> {
    let metadata = metadata_store_cloned(state)?;
    let rooms = state.rooms.clone();
    let id = reviewed.summary.id;
    let mut audit = query_audit::QueryOperationAudit::new(
        state.sessions.clone(),
        op(
            "apply_query_execution",
            Some(reviewed.summary.chat_id),
            Some(reviewed.summary.run_id),
        ),
        Some(actor.0),
    );
    let storage = metadata.clone();
    let applied: ApiResult<_> = metadata_blocking(move || {
        let shared = rooms
            .documents()
            .get_or_load(&storage, document)
            .map_err(|_| ApiError::BadRequest("Query document state is unavailable".into()))?;
        let mut native = shared
            .lock()
            .map_err(|_| ApiError::Internal("Query document actor is unavailable".into()))?;
        native
            .author_ai_query_execution(
                &storage,
                actor,
                id,
                &reviewed.summary.fingerprint,
                &reviewed.original_sql,
                &reviewed.proposal.proposed_sql,
            )
            .map_err(|_| {
                ApiError::BadRequest("Query text, authority or execution admission changed".into())
            })
    })
    .await;
    if let Ok(Some(authored)) = applied {
        if let crate::document_actor::ApplyOutcome::Applied { server_seq, .. } = authored.outcome {
            state.rooms.publish_doc(
                room,
                RoomServerMessage::DocumentUpdateCommitted {
                    document_id: document.0,
                    replica_id: ReplicaId(authored.replica_id),
                    server_seq,
                    update: CrdtUpdate::new(authored.update_bytes),
                    server_version: DocumentVersion::new(authored.server_version),
                },
            );
            state.sessions.push_operation_full(
                Operation::ApplyDocumentUpdate {
                    room_id: room,
                    document_id: document.0,
                    update_id: id.to_string(),
                    server_seq,
                },
                OperationStatus::Succeeded,
                Some(actor.0),
                None,
                None,
                None,
            );
        }
    } else {
        // This cannot overwrite a receipt committed with the document update.
        metadata
            .refuse_ai_query_document_execution(id, actor)
            .await?;
    }
    let result = metadata.ai_query_execution_settlement(id, actor).await?;
    if result
        .receipt
        .as_ref()
        .is_some_and(|r| r.outcome == AiQueryEditOutcome::Applied)
    {
        audit.record(OperationStatus::Succeeded, None, None, None);
    } else {
        audit.record(
            OperationStatus::Failed,
            Some("ai_query_edit_refused".into()),
            None,
            Some("Reviewed query edit was not committed".into()),
        );
    }
    Ok(())
}
pub(super) async fn acknowledge(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
    Json(request): Json<AcknowledgeAiQueryEditorExecution>,
) -> ApiResult<Json<AiQueryExecutionSummary>> {
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let id = super::ai::ai_chat_id(&raw)?;
    super::execution::owned_audited_operation(
        state.clone(),
        op("acknowledge_query_editor_execution", None, None),
        Some(auth.principal_id.0),
        Box::pin(async move {
            super::ai::ai_enabled(&state)?;
            metadata_store_cloned(&state)?
                .acknowledge_ai_query_editor_execution(id, auth.principal_id, request)
                .await?;
            Ok(Json(summary(&state, &auth, id).await?))
        }),
    )
    .await
}
pub(super) async fn deny(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
    Json(request): Json<ApproveAiQueryExecution>,
) -> ApiResult<Json<AiQueryExecutionSummary>> {
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let id = super::ai::ai_chat_id(&raw)?;
    super::execution::owned_audited_operation(
        state.clone(),
        op("deny_query_execution", None, None),
        Some(auth.principal_id.0),
        Box::pin(async move {
            super::ai::ai_enabled(&state)?;
            summary(&state, &auth, id).await?;
            metadata_store_cloned(&state)?
                .deny_ai_query_execution(id, auth.principal_id, request.fingerprint)
                .await?;
            Ok(Json(summary(&state, &auth, id).await?))
        }),
    )
    .await
}
pub(super) async fn reconcile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
    Json(request): Json<ApproveAiQueryExecution>,
) -> ApiResult<Json<AiQueryExecutionSummary>> {
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let id = super::ai::ai_chat_id(&raw)?;
    super::execution::owned_audited_operation(
        state.clone(),
        op("reconcile_query_execution", None, None),
        Some(auth.principal_id.0),
        Box::pin(async move {
            super::ai::ai_enabled(&state)?;
            summary(&state, &auth, id).await?;
            metadata_store_cloned(&state)?
                .reconcile_ai_query_execution(id, auth.principal_id, request.fingerprint)
                .await?;
            Ok(Json(summary(&state, &auth, id).await?))
        }),
    )
    .await
}
