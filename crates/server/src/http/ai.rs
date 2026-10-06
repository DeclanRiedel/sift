//! Chat catalog routes. Provider turns and tools enter through separate run APIs.

use super::*;
use chrono::Utc;

pub(super) fn audit_ai(
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

pub(super) fn ai_enabled(state: &AppState) -> ApiResult<()> {
    if state.auth.ai.enabled {
        Ok(())
    } else {
        Err(ApiError::Forbidden(
            "AI chat is disabled for this instance".into(),
        ))
    }
}

pub(super) async fn get_ai_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<sift_protocol::AiChatPolicy>> {
    ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    audit_ai(&state, auth.principal_id, "read_policy", None, None);
    let policy = &state.auth.ai;
    Ok(Json(sift_protocol::AiChatPolicy {
        new_chat_visibility: match policy.chat_visibility {
            sift_instance_config::AiChatVisibility::Private => sift_protocol::AiVisibility::Private,
            sift_instance_config::AiChatVisibility::RoomPublic => {
                sift_protocol::AiVisibility::RoomPublic
            }
        },
        max_context_sql_bytes: policy.max_context_sql_bytes,
        max_tool_result_bytes: policy.max_tool_result_bytes,
        max_tool_calls_per_run: policy.max_tool_calls_per_run,
        max_run_secs: policy.max_run_secs,
        max_retention_days: policy.max_retention_days,
    }))
}

fn retention_policy(
    tenant: i64,
    days: Option<u32>,
    ceiling: Option<u32>,
) -> sift_protocol::AiRetentionPolicy {
    sift_protocol::AiRetentionPolicy {
        tenant_id: tenant,
        retention_days: days,
        max_retention_days: ceiling,
        effective_retention_days: match (days, ceiling) {
            (Some(days), Some(ceiling)) => Some(days.min(ceiling)),
            (days, None) => days,
            (None, ceiling) => ceiling,
        },
    }
}

pub(super) async fn get_ai_retention(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(tenant): Path<i64>,
) -> ApiResult<Json<sift_protocol::AiRetentionPolicy>> {
    ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let tenant_id = tenant_id(tenant)?;
    ensure_tenant(&auth, tenant_id)?;
    let days = metadata_store_cloned(&state)?
        .ai_retention_days(tenant_id, auth.principal_id)
        .await?;
    audit_ai(
        &state,
        auth.principal_id,
        &format!("read_retention:{tenant}"),
        None,
        None,
    );
    Ok(Json(retention_policy(
        tenant,
        days,
        state.auth.ai.max_retention_days,
    )))
}

pub(super) async fn set_ai_retention(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(tenant): Path<i64>,
    Json(request): Json<sift_protocol::SetAiRetentionRequest>,
) -> ApiResult<Json<sift_protocol::AiRetentionPolicy>> {
    ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let tenant_id = tenant_id(tenant)?;
    let result: ApiResult<()> = async {
        require_tenant_admin(&auth, tenant_id)?;
        if let (Some(days), Some(ceiling)) =
            (request.retention_days, state.auth.ai.max_retention_days)
        {
            if days > ceiling {
                return Err(ApiError::BadRequest(
                    "AI retention exceeds the instance maximum".into(),
                ));
            }
        }
        metadata_store_cloned(&state)?
            .set_ai_retention_days(tenant_id, auth.principal_id, request.retention_days)
            .await?;
        Ok(())
    }
    .await;
    state.sessions.push_operation_full(
        Operation::Ai {
            action: format!("set_retention:{tenant}"),
            chat_id: None,
            run_id: None,
        },
        if result.is_ok() {
            OperationStatus::Succeeded
        } else {
            OperationStatus::Failed
        },
        Some(auth.principal_id.0),
        None,
        None,
        result
            .as_ref()
            .err()
            .map(|_| "AI retention policy update failed".into()),
    );
    result?;

    Ok(Json(retention_policy(
        tenant,
        request.retention_days,
        state.auth.ai.max_retention_days,
    )))
}

pub(super) async fn rotate_ai_content_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(tenant): Path<i64>,
) -> ApiResult<Json<sift_protocol::AiContentKeyRotation>> {
    ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let tenant_id = tenant_id(tenant)?;
    let result = match require_tenant_admin(&auth, tenant_id) {
        Ok(()) => metadata_store_cloned(&state)?
            .rotate_ai_content_key(tenant_id, auth.principal_id)
            .await
            .map_err(ApiError::from),
        Err(error) => Err(error),
    };
    state.sessions.push_operation_full(
        Operation::Ai {
            action: format!("rotate_content_key:{tenant}"),
            chat_id: None,
            run_id: None,
        },
        if result.is_ok() {
            OperationStatus::Succeeded
        } else {
            OperationStatus::Failed
        },
        Some(auth.principal_id.0),
        None,
        None,
        result
            .as_ref()
            .err()
            .map(|_| "AI content key rotation failed".into()),
    );
    Ok(Json(sift_protocol::AiContentKeyRotation {
        rewritten_blobs: result? as u64,
    }))
}

pub(super) fn ai_chat_id(raw: &str) -> ApiResult<uuid::Uuid> {
    uuid::Uuid::parse_str(raw).map_err(|_| ApiError::BadRequest("invalid AI chat ID".into()))
}

pub(super) async fn check_ai_chat_scope(
    state: &AppState,
    auth: &AuthContext,
    id: uuid::Uuid,
) -> ApiResult<()> {
    let tenant = metadata_store_cloned(state)?.ai_chat_tenant(id).await?;
    ensure_tenant(auth, tenant)
}

pub(super) async fn check_ai_run_scope(
    state: &AppState,
    auth: &AuthContext,
    id: uuid::Uuid,
) -> ApiResult<()> {
    let tenant = metadata_store_cloned(state)?.ai_run_tenant(id).await?;
    ensure_tenant(auth, tenant)
}

/// Materialize durable room SQL under its serialized actor. Never trust the
/// bytes accompanying a client-supplied document ID as publication evidence.
async fn committed_ai_document(
    state: &AppState,
    actor: PrincipalId,
    room: i64,
    document: i64,
    writable: bool,
) -> ApiResult<String> {
    let metadata = metadata_store_cloned(state)?;
    let rooms = state.rooms.clone();
    metadata_blocking(move || {
        let document =
            metadata.get_document_for_principal(DocumentId(document), actor, writable)?;
        if document.room_id.0 != room {
            return Err(ApiError::Forbidden(
                "AI document belongs to another room".into(),
            ));
        }
        let shared = rooms
            .documents()
            .get_or_load(&metadata, document.id)
            .map_err(|_| ApiError::BadRequest("AI document state is unavailable".into()))?;
        let guard = shared
            .lock()
            .map_err(|_| ApiError::Internal("document lock poisoned".into()))?;
        Ok(guard.text())
    })
    .await
}

fn ai_content_revision(text: &str) -> u64 {
    let digest = Sha256::digest(text.as_bytes());
    // Match the desktop's content revision; full text is compared as well.
    u64::from_le_bytes(digest[..8].try_into().expect("SHA-256 prefix")) & i64::MAX as u64
}

pub(super) async fn create_ai_chat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<sift_protocol::CreateAiChatRequest>,
) -> ApiResult<Json<sift_protocol::AiChat>> {
    ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    ensure_tenant(&auth, tenant_id(request.tenant_id)?)?;
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
    ensure_tenant(&auth, tenant_id(query.tenant_id)?)?;
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
    check_ai_chat_scope(&state, &auth, ai_chat_id(&id)?).await?;
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
    check_ai_chat_scope(&state, &auth, id).await?;
    metadata.delete_ai_chat(id, auth.principal_id).await?;
    audit_ai(&state, auth.principal_id, "delete_chat", Some(id), None);
    Ok(Json(json!({"deleted": true})))
}

pub(super) async fn preview_ai_publication(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<sift_protocol::AiRoomPublicationPreview>> {
    ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let metadata = metadata_store_cloned(&state)?;
    let room = room_id(id)?;
    ensure_room_permission(&metadata, &auth, room, RoomPermission::Admin)?;
    let result = metadata
        .preview_ai_room_publication(room, auth.principal_id)
        .await?;
    audit_ai(
        &state,
        auth.principal_id,
        &format!("preview_publication:{id}"),
        None,
        None,
    );
    Ok(Json(result))
}
pub(super) async fn get_ai_publication(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<Option<sift_protocol::AiRoomPublication>>> {
    ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let metadata = metadata_store_cloned(&state)?;
    let room = room_id(id)?;
    ensure_room_permission(&metadata, &auth, room, RoomPermission::Read)?;
    let result = metadata
        .ai_room_publication(room, auth.principal_id)
        .await?;
    audit_ai(
        &state,
        auth.principal_id,
        &format!("read_publication:{id}"),
        None,
        None,
    );
    Ok(Json(result))
}
pub(super) async fn create_ai_publication(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(request): Json<sift_protocol::CreateAiRoomPublicationRequest>,
) -> ApiResult<Json<sift_protocol::AiRoomPublication>> {
    ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let metadata = metadata_store_cloned(&state)?;
    let room = room_id(id)?;
    let result: ApiResult<_> = async {
        ensure_room_permission(&metadata, &auth, room, RoomPermission::Admin)?;
        let publication = metadata
            .create_ai_room_publication(room, auth.principal_id, request)
            .await?;
        state.sessions.close_room_connection(id).await;
        Ok(publication)
    }
    .await;
    audit_publication_change(&state, auth.principal_id, id, "publish", result.is_ok());
    Ok(Json(result?))
}
pub(super) async fn revoke_ai_publication(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let metadata = metadata_store_cloned(&state)?;
    let room = room_id(id)?;
    let result: ApiResult<()> = async {
        ensure_room_permission(&metadata, &auth, room, RoomPermission::Admin)?;
        metadata
            .revoke_ai_room_publication(room, auth.principal_id)
            .await?;
        state.sessions.close_room_connection(id).await;
        Ok(())
    }
    .await;
    audit_publication_change(&state, auth.principal_id, id, "revoke", result.is_ok());
    result?;
    Ok(StatusCode::NO_CONTENT)
}
fn audit_publication_change(
    state: &AppState,
    actor: PrincipalId,
    room: i64,
    action: &str,
    success: bool,
) {
    state.sessions.push_operation_full(
        Operation::Ai {
            action: format!("{action}_publication:{room}"),
            chat_id: None,
            run_id: None,
        },
        if success {
            OperationStatus::Succeeded
        } else {
            OperationStatus::Failed
        },
        Some(actor.0),
        None,
        None,
        (!success).then(|| "AI database publication update failed".into()),
    );
}

/// A publication never elevates the initiating member's normal connection rights.
pub(super) async fn authorized_publication(
    state: &AppState,
    auth: &AuthContext,
    context: &sift_protocol::AiTurnContext,
    tool: sift_protocol::AiToolKind,
) -> ApiResult<(
    sift_protocol::AiRoomPublication,
    crate::session::RoomConnProvenance,
)> {
    let metadata = metadata_store_cloned(state)?;
    let room = room_id(
        context
            .target
            .room_id
            .ok_or_else(|| ApiError::Forbidden("public AI context requires a room".into()))?,
    )?;
    let publication = metadata
        .ai_room_publication(room, auth.principal_id)
        .await?
        .filter(|grant| Some(grant.id) == context.publication_id)
        .ok_or_else(|| {
            ApiError::Forbidden("Room database publication changed; start a fresh turn".into())
        })?;
    if tool == sift_protocol::AiToolKind::Select && !publication.allow_rows {
        return Err(ApiError::Forbidden(
            "The room owner has not published row reads to AI chats".into(),
        ));
    }
    let check_metadata = metadata.clone();
    let check_auth = auth.clone();
    let profile_id = publication.source.profile_id;
    let provenance = metadata_blocking(move || {
        ensure_room_permission(&check_metadata, &check_auth, room, RoomPermission::Write)?;
        let room_row = check_metadata.get_room(room)?;
        let profile = check_metadata
            .get_connection_profile(room_row.tenant_id, connection_profile_id(profile_id)?)?;
        check_metadata.authorize_vault_connection_use(
            room_row.tenant_id,
            check_auth.principal_id,
            profile.id,
        )?;
        let scope = room_submitter_scope(
            &check_metadata,
            check_auth.principal_id,
            room,
            room_row.tenant_id,
            &profile,
        )?;
        let kind = match tool {
            sift_protocol::AiToolKind::Schema => sift_protocol::OperationKind::RefreshSchema,
            sift_protocol::AiToolKind::Catalog => sift_protocol::OperationKind::ReadCatalogGraph,
            sift_protocol::AiToolKind::Explain => sift_protocol::OperationKind::Explain,
            _ => sift_protocol::OperationKind::ExecuteQuery,
        };
        crate::authorization::authorize(&scope, kind)
            .map_err(|denial| ApiError::Forbidden(denial.public_reason().into()))?;
        Ok(crate::session::RoomConnProvenance {
            room_id: room.0,
            binder: room_row
                .bound_connection_by
                .ok_or_else(|| ApiError::Forbidden("room binding changed".into()))?,
            tenant: room_row.tenant_id,
            profile_id: profile.id,
            provider_id: profile.provider_id,
            engine: profile.semantic_engine,
            policy_revision: profile.policy.revision,
        })
    })
    .await?;
    Ok((publication, provenance))
}

pub(super) async fn start_ai_turn(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(mut request): Json<sift_protocol::StartAiTurnRequest>,
) -> ApiResult<Json<sift_protocol::AiRunLease>> {
    ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
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
    request.context.publication_id = None;
    // Publication is distinct from the initiating user's read permission.
    let chat_id = ai_chat_id(&id)?;
    check_ai_chat_scope(&state, &auth, chat_id).await?;
    let metadata = metadata_store_cloned(&state)?;
    let chat = metadata.get_ai_chat(chat_id, auth.principal_id).await?;
    request.context.target = authorized_tool_context(&state, &auth, request.context.target)?;
    if request.context.target.tenant_id != Some(chat.tenant_id)
        || (chat.room_id.is_some() && request.context.target.room_id != chat.room_id)
    {
        return Err(ApiError::Forbidden(
            "AI context is outside this chat".into(),
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
        if let Some(sql) = request.context.sql.as_mut() {
            if let Some(document_id) = sql.room_document_id {
                if request.context.target.document_id.as_deref()
                    != Some(document_id.to_string().as_str())
                {
                    return Err(ApiError::BadRequest(
                        "AI SQL document target does not match".into(),
                    ));
                }
                let text = committed_ai_document(
                    &state,
                    auth.principal_id,
                    chat.room_id.expect("public room"),
                    document_id,
                    false,
                )
                .await?;
                if sql.text != text || sql.document_revision != Some(ai_content_revision(&text)) {
                    return Err(ApiError::BadRequest(
                        "Shared SQL changed or has unsaved edits; sync before sending".into(),
                    ));
                }
                sql.text = text;
            } else {
                request.context.sql = None;
                request.context.target.document_id = None;
            }
        } else {
            request.context.target.document_id = None;
        }
        request.context.target.profile_id = None;
        request.context.target.connection_id = None;
        request.context.database = None;
        request.context.dialect = None;
        request.context.environment_label = None;
        request.context.current_error = None;
        request.context.staged_change_count = 0;
        if let Some(publication) = metadata
            .ai_room_publication(
                room_id(chat.room_id.expect("public room"))?,
                auth.principal_id,
            )
            .await?
        {
            request.context.publication_id = Some(publication.id);
            request.context.target.profile_id = Some(publication.source.profile_id);
            request.context.database = publication.source.database;
            request.context.dialect = Some(publication.source.dialect);
        }
    }
    metadata
        .expire_ai_runs_for_chat(chat_id, auth.principal_id, state.auth.ai.max_run_secs)
        .await?;
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

pub(super) async fn invoke_ai_tool(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<sift_protocol::InvokeAiToolRequest>,
) -> ApiResult<Json<sift_protocol::InvokeAiToolResponse>> {
    ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let run_id = ai_chat_id(&id)?;
    let mut chat_id = None;
    let result: ApiResult<sift_protocol::InvokeAiToolResponse> = async {
        check_ai_run_scope(&state, &auth, run_id).await?;
        let metadata = metadata_store_cloned(&state)?;
        let run = metadata
            .ai_tool_run(run_id, auth.principal_id, request.lease_token)
            .await?;
        chat_id = Some(run.chat_id);
        if (Utc::now() - run.started_at).num_seconds() >= i64::from(state.auth.ai.max_run_secs) {
            return Err(ApiError::Forbidden("AI run time limit reached".into()));
        }
        let public = run.visibility == sift_protocol::AiVisibility::RoomPublic;
        let routing = if public {
            Some(authorized_publication(&state, &auth, &run.context, request.tool).await?)
        } else {
            None
        };
        let private_connection = if public {
            None
        } else {
            let target = authorized_tool_context(&state, &auth, run.context.target.clone())?;
            if target != run.context.target {
                return Err(ApiError::Forbidden(
                    "AI context authorization changed".into(),
                ));
            }
            let connection = target.connection_id.as_deref().ok_or_else(|| {
                ApiError::BadRequest("AI tool requires an active connection".into())
            })?;
            let (session, connection) = connection
                .split_once(':')
                .ok_or_else(|| ApiError::BadRequest("invalid AI connection".into()))?;
            Some((
                sift_protocol::SessionId(
                    session
                        .parse()
                        .map_err(|_| ApiError::BadRequest("invalid AI session".into()))?,
                ),
                sift_protocol::ConnectionId(
                    connection
                        .parse()
                        .map_err(|_| ApiError::BadRequest("invalid AI connection".into()))?,
                ),
            ))
        };
        let sql = request.sql.as_deref().unwrap_or("");
        if !matches!(
            request.tool,
            sift_protocol::AiToolKind::Schema | sift_protocol::AiToolKind::Catalog
        ) && (sql.trim().is_empty() || sql.len() as u64 > state.auth.ai.max_context_sql_bytes)
        {
            return Err(ApiError::BadRequest(
                "AI tool SQL is empty or too large".into(),
            ));
        }
        metadata
            .reserve_ai_tool_call(
                run_id,
                auth.principal_id,
                request.lease_token,
                request.call_id,
                state.auth.ai.max_tool_calls_per_run,
                request.tool,
            )
            .await?;
        let result = if let Some((publication, provenance)) = routing {
            let shared_state = &state;
            let shared_auth = &auth;
            let shared_context = &run.context;
            state
                .sessions
                .with_ai_room_connection(provenance, publication.id, |session, conn| async move {
                    // The connection may have queued behind another room query.
                    authorized_publication(shared_state, shared_auth, shared_context, request.tool)
                        .await?;
                    let value =
                        dispatch_ai_tool(shared_state, session, conn, request.tool, sql).await?;
                    authorized_publication(shared_state, shared_auth, shared_context, request.tool)
                        .await?;
                    Ok(value)
                })
                .await
        } else {
            let (session, conn) = private_connection.expect("private connection");
            dispatch_ai_tool(&state, session, conn, request.tool, sql).await
        };
        let result = result.and_then(|value| {
            let bytes = serde_json::to_vec(&value)
                .map_err(|error| ApiError::Internal(error.to_string()))?;
            if bytes.len() as u64 > state.auth.ai.max_tool_result_bytes {
                return Err(ApiError::BadRequest(
                    "AI tool result exceeds the instance limit".into(),
                ));
            }
            Ok(value)
        });
        metadata
            .finish_ai_tool_call(
                run_id,
                auth.principal_id,
                request.lease_token,
                request.call_id,
                result.is_ok(),
            )
            .await?;
        Ok(sift_protocol::InvokeAiToolResponse {
            call_id: request.call_id,
            tool: request.tool,
            result: result?,
            sift_restricted: request.tool == sift_protocol::AiToolKind::Select,
        })
    }
    .await;
    state.sessions.push_operation_full(
        Operation::Ai {
            action: format!("tool:{:?}", request.tool).to_ascii_lowercase(),
            chat_id,
            run_id: Some(run_id),
        },
        if result.is_ok() {
            OperationStatus::Succeeded
        } else {
            OperationStatus::Failed
        },
        Some(auth.principal_id.0),
        None,
        None,
        result
            .as_ref()
            .err()
            .map(|_| "AI tool failed or authorization changed".into()),
    );
    Ok(Json(result?))
}

async fn dispatch_ai_tool(
    state: &AppState,
    session: sift_protocol::SessionId,
    connection: sift_protocol::ConnectionId,
    tool: sift_protocol::AiToolKind,
    sql: &str,
) -> ApiResult<serde_json::Value> {
    let entry = state.sessions.conn_entry(session, connection)?;
    let engine = entry
        .driver
        .semantic_engine()
        .ok_or_else(|| ApiError::Forbidden("AI tool requires a supported SQL dialect".into()))?;
    if matches!(
        tool,
        sift_protocol::AiToolKind::Explain | sift_protocol::AiToolKind::Select
    ) {
        // Sift's agent subprofile admits only one SELECT. This is enforced
        // even when the human's connection and DB login can write.
        let policy = state
            .sessions
            .current_connection_policy(session, connection)?
            .unwrap_or_default();
        crate::sql_policy::enforce_ai_select(&policy, engine, sql)?;
    }
    let value = match tool {
        sift_protocol::AiToolKind::Catalog => {
            let graph = state
                .sessions
                .catalog_graph(
                    session,
                    connection,
                    sift_protocol::CatalogGraphRequest {
                        options: ai_catalog_options(),
                        refresh: false,
                    },
                )
                .await?;
            serde_json::to_value(graph)
        }
        sift_protocol::AiToolKind::Schema => {
            let snapshot = state
                .sessions
                .schema(session, connection, sift_protocol::SchemaScope::shallow())
                .await?;
            serde_json::to_value(snapshot)
        }
        sift_protocol::AiToolKind::Diagnostics => {
            use sqlparser::dialect::{MsSqlDialect, PostgreSqlDialect, SQLiteDialect};
            let dialect: Box<dyn sqlparser::dialect::Dialect> = match engine {
                sift_protocol::Engine::Postgres => Box::new(PostgreSqlDialect {}),
                sift_protocol::Engine::SqlServer => Box::new(MsSqlDialect {}),
                sift_protocol::Engine::Sqlite => Box::new(SQLiteDialect {}),
            };
            let parsed = sqlparser::parser::Parser::parse_sql(dialect.as_ref(), sql);
            serde_json::to_value(
                json!({"valid": parsed.is_ok(), "diagnostic": parsed.err().map(|error| error.to_string())}),
            )
        }
        sift_protocol::AiToolKind::Explain => {
            let plan = crate::plan::explain(
                &state.sessions,
                session,
                connection,
                &sift_protocol::ExplainRequest {
                    connection,
                    sql: sql.into(),
                    params: Vec::new(),
                    analyze: false,
                },
            )
            .await?;
            serde_json::to_value(plan)
        }
        sift_protocol::AiToolKind::Select => {
            let response = state
                .sessions
                .execute_ai_read(
                    session,
                    sift_protocol::ExecuteRequestHttp {
                        connection,
                        sql: sql.into(),
                        params: Vec::new(),
                        tx: None,
                        room_id: None,
                        connection_profile_id: None,
                        transform: None,
                        source: None,
                    },
                    state.auth.ai.max_tool_result_bytes.min(usize::MAX as u64) as usize,
                )
                .await?;
            serde_json::to_value(response)
        }
    };
    value.map_err(|error| ApiError::Internal(error.to_string()))
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
    check_ai_run_scope(&state, &auth, run_id).await?;
    let metadata = metadata_store_cloned(&state)?;
    let run = metadata
        .ai_tool_run(run_id, auth.principal_id, request.lease_token)
        .await?;
    let current = authorized_tool_context(&state, &auth, request.target.clone())?;
    if current != run.context.target || current != request.target {
        return Err(ApiError::Forbidden(
            "AI proposal context authorization changed".into(),
        ));
    }
    if run.visibility == sift_protocol::AiVisibility::RoomPublic {
        let room = run
            .context
            .target
            .room_id
            .ok_or_else(|| ApiError::Forbidden("public proposal requires a room".into()))?;
        let document = run
            .context
            .target
            .document_id
            .as_deref()
            .and_then(|id| id.parse::<i64>().ok())
            .ok_or_else(|| {
                ApiError::BadRequest(
                    "Move SQL into a room document before proposing changes".into(),
                )
            })?;
        let text = committed_ai_document(&state, auth.principal_id, room, document, false).await?;
        if ai_content_revision(&text) != request.base_revision {
            return Err(ApiError::BadRequest(
                "Shared SQL changed; start a fresh turn".into(),
            ));
        }
    }
    let detail = metadata
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
    check_ai_chat_scope(&state, &auth, chat_id).await?;
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
    check_ai_chat_scope(&state, &auth, chat_id).await?;
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

pub(super) async fn apply_ai_query_proposal(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((chat_id, proposal_id)): Path<(String, String)>,
    Json(request): Json<sift_protocol::ApplyAiQueryProposalRequest>,
) -> ApiResult<Json<sift_protocol::AiQueryProposalDetail>> {
    ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let chat_id = ai_chat_id(&chat_id)?;
    check_ai_chat_scope(&state, &auth, chat_id).await?;
    let proposal_id = ai_chat_id(&proposal_id)?;
    let metadata = metadata_store_cloned(&state)?;
    let current = metadata
        .list_ai_query_proposals(chat_id, auth.principal_id)
        .await?
        .into_iter()
        .find(|proposal| proposal.proposal.id == proposal_id)
        .ok_or_else(|| ApiError::BadRequest("AI proposal is unavailable".into()))?;
    if let Some(document_id) = current.proposal.target.document_id.as_deref() {
        let document_id: i64 = document_id
            .parse()
            .map_err(|_| ApiError::BadRequest("invalid document target".into()))?;
        let document = metadata.get_document_for_principal(
            DocumentId(document_id),
            auth.principal_id,
            true,
        )?;
        let text = committed_ai_document(
            &state,
            auth.principal_id,
            document.room_id.0,
            document_id,
            true,
        )
        .await?;
        if text != current.proposed_sql {
            return Err(ApiError::BadRequest(
                "Reviewed SQL has not been committed to the room document".into(),
            ));
        }
    }
    let detail = metadata
        .mark_ai_query_proposal_applied(
            chat_id,
            proposal_id,
            auth.principal_id,
            request.expected_revision,
        )
        .await?;
    audit_ai(
        &state,
        auth.principal_id,
        "apply_query_proposal",
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
    check_ai_chat_scope(&state, &auth, chat_id).await?;
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
    check_ai_run_scope(&state, &auth, run_id).await?;
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
    check_ai_run_scope(&state, &auth, run_id).await?;
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
    check_ai_run_scope(&state, &auth, run_id).await?;
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

pub(super) fn ai_catalog_options() -> sift_protocol::CatalogGraphOptions {
    sift_protocol::CatalogGraphOptions {
        max_nodes: Some(2000),
        ..Default::default()
    }
}
pub(super) fn ai_connection_ids(
    context: &sift_protocol::AiTurnContext,
) -> ApiResult<(sift_protocol::SessionId, sift_protocol::ConnectionId)> {
    let value = context
        .target
        .connection_id
        .as_deref()
        .ok_or_else(|| ApiError::BadRequest("AI tool requires a managed connection".into()))?;
    let (session, connection) = value
        .split_once(':')
        .ok_or_else(|| ApiError::BadRequest("invalid AI connection".into()))?;
    Ok((
        sift_protocol::SessionId(
            session
                .parse()
                .map_err(|_| ApiError::BadRequest("invalid AI session".into()))?,
        ),
        sift_protocol::ConnectionId(
            connection
                .parse()
                .map_err(|_| ApiError::BadRequest("invalid AI connection".into()))?,
        ),
    ))
}
