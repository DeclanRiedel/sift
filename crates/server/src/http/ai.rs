//! Chat catalog routes. Provider turns and tools enter through separate run APIs.

use super::*;

fn audit_ai(
    state: &AppState,
    actor: PrincipalId,
    action: &str,
    chat_id: Option<uuid::Uuid>,
    run_id: Option<uuid::Uuid>,
) {
    state.sessions.push_operation_full(
        Operation::Ai {
            action: action.into(),
            chat_id,
            run_id,
        },
        OperationStatus::Succeeded,
        Some(actor.0),
        None,
        None,
        None,
    );
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct ListAiChatsQuery {
    tenant_id: i64,
    limit: Option<u32>,
}

fn ai_enabled(state: &AppState) -> ApiResult<()> {
    if state.auth.ai.enabled {
        Ok(())
    } else {
        Err(ApiError::Forbidden(
            "AI chat is disabled for this instance".into(),
        ))
    }
}

fn ai_chat_id(raw: &str) -> ApiResult<uuid::Uuid> {
    uuid::Uuid::parse_str(raw).map_err(|_| ApiError::BadRequest("invalid AI chat ID".into()))
}

pub(super) async fn create_ai_chat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<sift_protocol::CreateAiChatRequest>,
) -> ApiResult<Json<sift_protocol::AiChat>> {
    ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let metadata = metadata_store_cloned(&state)?;
    let visibility = match state.auth.ai.chat_visibility {
        sift_instance_config::AiChatVisibility::Private => sift_protocol::AiVisibility::Private,
        sift_instance_config::AiChatVisibility::RoomPublic => {
            sift_protocol::AiVisibility::RoomPublic
        }
    };
    let chat = metadata
        .create_ai_chat(
            TenantId(request.tenant_id),
            request.room_id.map(RoomId),
            auth.principal_id,
            visibility,
            request.title,
        )
        .await?;
    audit_ai(
        &state,
        auth.principal_id,
        "create_chat",
        Some(chat.id),
        None,
    );
    Ok(Json(chat))
}

pub(super) async fn list_ai_chats(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ListAiChatsQuery>,
) -> ApiResult<Json<Vec<sift_protocol::AiChat>>> {
    ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let metadata = metadata_store_cloned(&state)?;
    let chats = metadata
        .list_ai_chats(
            TenantId(query.tenant_id),
            auth.principal_id,
            query.limit.unwrap_or(50),
        )
        .await?;
    audit_ai(&state, auth.principal_id, "list_chats", None, None);
    Ok(Json(chats))
}

pub(super) async fn get_ai_chat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<sift_protocol::AiChat>> {
    ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let metadata = metadata_store_cloned(&state)?;
    let chat = metadata
        .get_ai_chat(ai_chat_id(&id)?, auth.principal_id)
        .await?;
    audit_ai(&state, auth.principal_id, "read_chat", Some(chat.id), None);
    Ok(Json(chat))
}

pub(super) async fn delete_ai_chat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let metadata = metadata_store_cloned(&state)?;
    let id = ai_chat_id(&id)?;
    metadata.delete_ai_chat(id, auth.principal_id).await?;
    audit_ai(&state, auth.principal_id, "delete_chat", Some(id), None);
    Ok(Json(json!({"deleted": true})))
}

pub(super) async fn start_ai_turn(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(mut request): Json<sift_protocol::StartAiTurnRequest>,
) -> ApiResult<Json<sift_protocol::AiRunLease>> {
    ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    if request.provider != sift_protocol::AiProvider::Codex {
        return Err(ApiError::BadRequest(
            "this AI provider adapter is not available".into(),
        ));
    }
    if request
        .context
        .sql
        .as_ref()
        .is_some_and(|sql| sql.text.len() as u64 > state.auth.ai.max_context_sql_bytes)
    {
        return Err(ApiError::BadRequest(
            "AI SQL context exceeds the instance limit".into(),
        ));
    }
    // Room-public SQL must come from a server-verified, publishable document.
    // No such verifier is exposed yet, so omit it before storage or provider delivery.
    let chat_id = ai_chat_id(&id)?;
    let metadata = metadata_store_cloned(&state)?;
    let chat = metadata.get_ai_chat(chat_id, auth.principal_id).await?;
    if chat.visibility == sift_protocol::AiVisibility::RoomPublic
        && request.mode == sift_protocol::AiMode::Propose
    {
        return Err(ApiError::BadRequest(
            "room-public proposal publication is not available yet".into(),
        ));
    }
    if chat.visibility == sift_protocol::AiVisibility::RoomPublic {
        if request.context.target.tenant_id != Some(chat.tenant_id)
            || request.context.target.room_id != chat.room_id
        {
            return Err(ApiError::Forbidden(
                "AI context is outside this chat".into(),
            ));
        }
        request.context.sql = None;
        request.context.target.profile_id = None;
        request.context.target.connection_id = None;
        request.context.target.document_id = None;
        request.context.database = None;
        request.context.dialect = None;
        request.context.environment_label = None;
        request.context.current_error = None;
        request.context.staged_change_count = 0;
    }
    let lease = metadata
        .start_ai_run(chat_id, auth.principal_id, request)
        .await?;
    audit_ai(
        &state,
        auth.principal_id,
        "start_turn",
        Some(chat_id),
        Some(lease.run.id),
    );
    Ok(Json(lease))
}

pub(super) async fn stage_ai_query_proposal(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<sift_protocol::StageAiQueryProposalRequest>,
) -> ApiResult<Json<sift_protocol::AiQueryProposalDetail>> {
    ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let run_id = ai_chat_id(&id)?;
    let detail = metadata_store_cloned(&state)?
        .stage_ai_query_proposal(run_id, auth.principal_id, request)
        .await?;
    audit_ai(
        &state,
        auth.principal_id,
        "stage_query_proposal",
        Some(detail.proposal.chat_id),
        Some(run_id),
    );
    Ok(Json(detail))
}

pub(super) async fn list_ai_query_proposals(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<sift_protocol::AiQueryProposalDetail>>> {
    ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let chat_id = ai_chat_id(&id)?;
    let proposals = metadata_store_cloned(&state)?
        .list_ai_query_proposals(chat_id, auth.principal_id)
        .await?;
    audit_ai(
        &state,
        auth.principal_id,
        "list_proposals",
        Some(chat_id),
        None,
    );
    Ok(Json(proposals))
}

pub(super) async fn discard_ai_query_proposal(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((chat_id, proposal_id)): Path<(String, String)>,
) -> ApiResult<Json<sift_protocol::AiQueryProposalDetail>> {
    ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let chat_id = ai_chat_id(&chat_id)?;
    let proposal_id = ai_chat_id(&proposal_id)?;
    let detail = metadata_store_cloned(&state)?
        .discard_ai_query_proposal(chat_id, proposal_id, auth.principal_id)
        .await?;
    audit_ai(
        &state,
        auth.principal_id,
        "discard_query_proposal",
        Some(chat_id),
        Some(detail.proposal.run_id),
    );
    Ok(Json(detail))
}

pub(super) async fn list_ai_runs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<sift_protocol::AiRunDetail>>> {
    ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let chat_id = ai_chat_id(&id)?;
    let runs = metadata_store_cloned(&state)?
        .list_ai_runs(chat_id, auth.principal_id)
        .await?;
    audit_ai(&state, auth.principal_id, "list_turns", Some(chat_id), None);
    Ok(Json(runs))
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct AiEventsQuery {
    after: Option<u64>,
}

pub(super) async fn list_ai_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<AiEventsQuery>,
) -> ApiResult<Json<Vec<sift_protocol::AiRunEvent>>> {
    ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let run_id = ai_chat_id(&id)?;
    let events = metadata_store_cloned(&state)?
        .list_ai_run_events(run_id, auth.principal_id, query.after.unwrap_or(0))
        .await?;
    audit_ai(&state, auth.principal_id, "list_events", None, Some(run_id));
    Ok(Json(events))
}

pub(super) async fn append_ai_event(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<sift_protocol::AppendAiEventRequest>,
) -> ApiResult<Json<sift_protocol::AiRunEvent>> {
    ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let run_id = ai_chat_id(&id)?;
    let event = metadata_store_cloned(&state)?
        .append_ai_provider_event(
            run_id,
            auth.principal_id,
            request.lease_token,
            request.client_event_id,
            request.kind,
            request.content,
        )
        .await?;
    audit_ai(
        &state,
        auth.principal_id,
        "append_event",
        None,
        Some(run_id),
    );
    Ok(Json(event))
}

pub(super) async fn finish_ai_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<sift_protocol::FinishAiRunRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let run_id = ai_chat_id(&id)?;
    metadata_store_cloned(&state)?
        .finish_ai_run(
            run_id,
            auth.principal_id,
            request.lease_token,
            request.status,
        )
        .await?;
    audit_ai(&state, auth.principal_id, "finish_turn", None, Some(run_id));
    Ok(Json(json!({"finished": true})))
}
