//! Server-resolved exact-body attachment review and delivery (ADR-104).
use super::*;
use chrono::Utc;
use sha2::{Digest, Sha256};
use sift_protocol::{
    AiAttachmentPreview, AiAttachmentSource, AiContextAttachment, AiVisibility,
    PreviewAiAttachmentRequest, ToolContext,
};

const PREVIEW_BYTES: usize = 64 * 1024;

pub(super) async fn preview(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<PreviewAiAttachmentRequest>,
) -> ApiResult<Json<AiAttachmentPreview>> {
    super::ai::ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers.clone()).await?;
    let chat_id = super::ai::ai_chat_id(&id)?;
    let result: ApiResult<AiAttachmentPreview> = async {
        super::ai::check_ai_chat_scope(&state, &auth, chat_id).await?;
        let metadata = metadata_store_cloned(&state)?;
        let chat = metadata.get_ai_chat(chat_id, auth.principal_id).await?;
        let source_target = authorized_tool_context(&state, &auth, request.target)?;
        let (target, publication) =
            preview_target(&state, &auth, &chat, source_target.clone()).await?;
        let attachment = materialize(&state, &auth, &chat, &source_target, &request.source).await?;
        let fresh = resolve_auth_context_blocking(state.clone(), headers).await?;
        super::ai::check_ai_chat_scope(&state, &fresh, chat_id).await?;
        check_source_authority(&state, &fresh, &chat, &source_target, &attachment).await?;
        let (current, current_publication) =
            preview_target(&state, &fresh, &chat, source_target.clone()).await?;
        if current != target || current_publication != publication {
            return Err(ApiError::Forbidden(
                "Attachment scope changed during preview; review again".into(),
            ));
        }
        let requires_publication_ack = chat.visibility == AiVisibility::RoomPublic
            && attachment.origin_visibility == AiVisibility::Private;
        let expires_at = attachment
            .content
            .get("retained_until")
            .and_then(|value| serde_json::from_value::<chrono::DateTime<Utc>>(value.clone()).ok())
            .unwrap_or(Utc::now() + chrono::Duration::seconds(600))
            .min(Utc::now() + chrono::Duration::seconds(600));
        let preview = AiAttachmentPreview {
            id: uuid::Uuid::new_v4(),
            attachment,
            visibility: chat.visibility,
            requires_publication_ack,
            expires_at,
        };
        state.sessions.ai_attachment_previews().insert(
            auth.principal_id,
            chat_id,
            target,
            source_target,
            publication,
            preview.clone(),
        )?;
        Ok(preview)
    }
    .await;
    state.sessions.push_operation_full(
        Operation::Ai {
            action: "preview_attachment".into(),
            chat_id: Some(chat_id),
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
            .map(|_| "AI attachment unavailable or authorization changed".into()),
    );
    Ok(Json(result?))
}

async fn preview_target(
    state: &AppState,
    auth: &AuthContext,
    chat: &sift_protocol::AiChat,
    mut target: ToolContext,
) -> ApiResult<(ToolContext, Option<uuid::Uuid>)> {
    if target.tenant_id != Some(chat.tenant_id)
        || (chat.room_id.is_some() && target.room_id.is_some() && target.room_id != chat.room_id)
    {
        return Err(ApiError::Forbidden(
            "Attachment target is outside this chat".into(),
        ));
    }
    target.document_id = None;
    if chat.visibility == AiVisibility::RoomPublic {
        let room = RoomId(
            chat.room_id
                .ok_or_else(|| ApiError::Forbidden("Public attachment requires a room".into()))?,
        );
        target.room_id = Some(room.0);
        target.connection_id = None;
        target.profile_id = None;
        let grant = metadata_store_cloned(state)?
            .ai_room_publication(room, auth.principal_id)
            .await?;
        if let Some(grant) = grant {
            target.profile_id = Some(grant.source.profile_id);
            return Ok((target, Some(grant.id)));
        }
    }
    Ok((target, None))
}

pub(super) async fn accepted(
    state: &AppState,
    auth: &AuthContext,
    chat: &sift_protocol::AiChat,
    context: &sift_protocol::AiTurnContext,
    accepted: &[sift_protocol::AcceptAiAttachment],
) -> ApiResult<Vec<AiContextAttachment>> {
    if accepted.len() > 4
        || accepted
            .iter()
            .map(|item| item.preview_id)
            .collect::<std::collections::HashSet<_>>()
            .len()
            != accepted.len()
    {
        return Err(ApiError::BadRequest(
            "Attach at most four distinct reviewed resources".into(),
        ));
    }
    let mut target = context.target.clone();
    target.document_id = None;
    let mut attachments = Vec::new();
    for choice in accepted {
        let entry = state.sessions.ai_attachment_previews().get(
            auth.principal_id,
            chat.id,
            choice.preview_id,
        )?;
        if entry.target != target || entry.publication_id != context.publication_id {
            return Err(ApiError::BadRequest(
                "Connection or publication changed; review the attachment again".into(),
            ));
        }
        if entry.preview.attachment.sha256 != choice.expected_sha256
            || (entry.preview.requires_publication_ack && !choice.publish_to_room)
            || (chat.visibility == AiVisibility::Private && choice.publish_to_room)
        {
            return Err(ApiError::BadRequest(
                "Confirm the exact attachment preview and its sharing label before sending".into(),
            ));
        }
        let current = materialize(
            state,
            auth,
            chat,
            &entry.source_target,
            &entry.preview.attachment.source,
        )
        .await?;
        if current.sha256 != entry.preview.attachment.sha256 {
            return Err(ApiError::BadRequest(
                "Attachment source changed; review it again".into(),
            ));
        }
        check_source_authority(state, auth, chat, &entry.source_target, &current).await?;
        let mut attachment = entry.preview.attachment;
        if entry.preview.requires_publication_ack {
            attachment.published_by = Some(auth.principal_id.0);
        }
        attachments.push(attachment);
    }
    Ok(attachments)
}

pub(super) async fn reauthorize_accepted(
    state: &AppState,
    auth: &AuthContext,
    chat: &sift_protocol::AiChat,
    context: &sift_protocol::AiTurnContext,
    choices: &[sift_protocol::AcceptAiAttachment],
) -> ApiResult<()> {
    let mut target = context.target.clone();
    target.document_id = None;
    for choice in choices {
        let entry = state.sessions.ai_attachment_previews().get(
            auth.principal_id,
            chat.id,
            choice.preview_id,
        )?;
        let (current, publication) =
            preview_target(state, auth, chat, entry.source_target.clone()).await?;
        if current != target || publication != context.publication_id {
            return Err(ApiError::Forbidden(
                "Attachment publication changed before send; review again".into(),
            ));
        }
        check_source_authority(
            state,
            auth,
            chat,
            &entry.source_target,
            &entry.preview.attachment,
        )
        .await?;
    }
    Ok(())
}

async fn check_source_authority(
    state: &AppState,
    auth: &AuthContext,
    chat: &sift_protocol::AiChat,
    target: &ToolContext,
    attachment: &AiContextAttachment,
) -> ApiResult<()> {
    let current = authorized_tool_context(state, auth, target.clone())?;
    if &current != target {
        return Err(ApiError::Forbidden(
            "Attachment source authorization changed".into(),
        ));
    }
    if let AiAttachmentSource::RoomRows { room_id, .. } = attachment.source {
        ensure_room_permission(
            &metadata_store_cloned(state)?,
            auth,
            RoomId(room_id),
            RoomPermission::Read,
        )?;
        return Ok(());
    }
    if let AiAttachmentSource::QueryHistory { history_id } = attachment.source {
        if attachment.origin_visibility == AiVisibility::RoomPublic {
            let room = RoomId(chat.room_id.ok_or_else(|| {
                ApiError::Forbidden("Shared history requires its source room".into())
            })?);
            let metadata = metadata_store_cloned(state)?;
            ensure_room_permission(&metadata, auth, room, RoomPermission::Read)?;
            if metadata
                .ai_room_query_history_entry(
                    TenantId(chat.tenant_id),
                    room,
                    auth.principal_id,
                    sift_metadata::QueryHistoryId(history_id),
                )?
                .is_none()
            {
                return Err(ApiError::Forbidden("Shared history is unavailable".into()));
            }
            return Ok(());
        }
    }
    let profile = target.profile_id.ok_or_else(|| {
        ApiError::BadRequest(
            "Select the source connection profile before attaching this resource".into(),
        )
    })?;
    let tenant = TenantId(chat.tenant_id);
    let metadata = metadata_store_cloned(state)?;
    metadata.authorize_vault_connection_use(
        tenant,
        auth.principal_id,
        ConnectionProfileId(profile),
    )?;
    let scope = tool_authorization_scope(state, auth, target)?;
    crate::authorization::authorize(&scope, sift_protocol::OperationKind::ReadCatalogGraph)
        .map_err(|denial| ApiError::Forbidden(denial.public_reason().into()))?;
    if chat.visibility == AiVisibility::RoomPublic
        && attachment.origin_visibility == AiVisibility::Private
    {
        if matches!(attachment.source, AiAttachmentSource::QueryRows { .. }) {
            return Err(ApiError::Forbidden(
                "Private query rows require a canonical shared room result".into(),
            ));
        }
        let grant=metadata.ai_room_publication(RoomId(chat.room_id.expect("public room")),auth.principal_id).await?
            .filter(|grant|grant.source.profile_id==profile && grant.allow_rows)
            .ok_or_else(||ApiError::Forbidden("Publishing private history or plans requires the owner's reviewed row-data publication for this profile".into()))?;
        let probe = sift_protocol::AiTurnContext {
            external_sources: Vec::new(),
            inclusion: Default::default(),
            workspace: None,
            target: ToolContext {
                tenant_id: Some(chat.tenant_id),
                room_id: chat.room_id,
                profile_id: Some(profile),
                connection_id: None,
                document_id: None,
            },
            attachments: Vec::new(),
            editor_item_id: None,
            database: None,
            dialect: None,
            environment_label: None,
            sql: None,
            current_error: None,
            staged_change_count: 0,
            publication_id: Some(grant.id),
        };
        super::ai::authorized_publication(state, auth, &probe, sift_protocol::AiToolKind::Select)
            .await?;
    }
    if let AiAttachmentSource::QueryRows {
        result_id,
        result_set,
        ref schema_digest,
        ..
    } = attachment.source
    {
        let (provenance, _, _) = state.sessions.ai_result_excerpt(
            auth.principal_id,
            result_id,
            result_set,
            schema_digest,
        )?;
        if provenance.sql_truncated {
            return Err(ApiError::BadRequest("Original SQL exceeds the retained authorization limit; obtain a smaller result source".into()));
        }
        let (session, connection) = super::ai::ai_connection_ids(&sift_protocol::AiTurnContext {
            external_sources: Vec::new(),
            inclusion: Default::default(),
            workspace: None,
            target: target.clone(),
            attachments: Vec::new(),
            editor_item_id: None,
            database: None,
            dialect: None,
            environment_label: None,
            sql: None,
            current_error: None,
            staged_change_count: 0,
            publication_id: None,
        })?;
        state.sessions.authorize_connection_operation(
            session,
            connection,
            sift_protocol::OperationKind::ExecuteQuery,
            Some(&provenance.sql),
            &[],
        )?;
    }
    Ok(())
}

async fn materialize(
    state: &AppState,
    auth: &AuthContext,
    chat: &sift_protocol::AiChat,
    target: &ToolContext,
    source: &AiAttachmentSource,
) -> ApiResult<AiContextAttachment> {
    let _slot = state.sessions.ai_attachment_materialization_slot()?;
    let limit = PREVIEW_BYTES.min(state.auth.ai.max_tool_result_bytes as usize);
    let metadata = metadata_store_cloned(state)?;
    let tenant = TenantId(chat.tenant_id);
    let actor = auth.principal_id;
    let (label, mut content, mut truncated, origin_visibility) = match source {
        AiAttachmentSource::QueryRows {
            result_id,
            result_set,
            schema_digest,
            row_ordinals,
            column_indices,
        } => {
            if chat.visibility == AiVisibility::RoomPublic {
                return Err(ApiError::Forbidden("Private query rows cannot enter public context; select a canonical shared room result".into()));
            }
            crate::ai_attachment_rows::validate_selection(row_ordinals, column_indices)?;
            let (provenance, set, interrupted) =
                state
                    .sessions
                    .ai_result_excerpt(actor, *result_id, *result_set, schema_digest)?;
            if target.tenant_id != Some(provenance.tenant.0)
                || target.profile_id != Some(provenance.profile.0)
                || target.connection_id.as_deref()
                    != Some(
                        format!("{}:{}", provenance.session.0, provenance.connection.0).as_str(),
                    )
            {
                return Err(ApiError::BadRequest("The selected result belongs to a different execution connection; return to its source before attaching".into()));
            }
            let columns = crate::ai_attachment_rows::select_columns(&set.columns, column_indices)?;
            let rows =
                crate::ai_attachment_rows::select_rows(&set.rows, row_ordinals, column_indices)?;
            let mut sql = provenance.sql;
            let excerpt = shorten(&mut sql, 8192) || provenance.sql_truncated;
            (
                "Selected private query rows · historical execution".into(),
                json!({"columns":columns,"rows":rows,"row_ordinals":row_ordinals,"column_indices":column_indices,
                "schema_digest":schema_digest,"result_set":result_set,"source_profile_id":provenance.profile.0,"source_connection":target.connection_id,"retained_until":provenance.retained_until,
                "executed_sql":sql,"sql_excerpt":excerpt,"retained_rows":set.rows.len(),"rows_seen":set.rows_seen,"source_truncated":set.truncated,"interrupted":interrupted}),
                set.truncated || excerpt || interrupted,
                AiVisibility::Private,
            )
        }
        AiAttachmentSource::RoomRows {
            room_id,
            result_id,
            result_set,
            schema_digest,
            row_ordinals,
            column_indices,
        } => {
            if chat.room_id != Some(*room_id) {
                return Err(ApiError::Forbidden(
                    "Shared result belongs to another room".into(),
                ));
            }
            ensure_room_permission(&metadata, auth, RoomId(*room_id), RoomPermission::Read)?;
            let room = metadata.get_room(RoomId(*room_id))?;
            if room.tenant_id != tenant {
                return Err(ApiError::Forbidden(
                    "Shared result belongs to another tenant".into(),
                ));
            }
            let registry = state.rooms.results().clone();
            let room = *room_id;
            let result = *result_id;
            let set = *result_set;
            let digest = schema_digest.clone();
            let ordinals = row_ordinals.clone();
            let indices = column_indices.clone();
            let (reference, columns, rows) = metadata_blocking(move || {
                registry.ai_selected_rows(room, result, set, &digest, &ordinals, &indices)
            })
            .await?;
            (
                "Selected shared room rows · historical result".into(),
                json!({"columns":columns,"rows":rows,"row_ordinals":row_ordinals,"column_indices":column_indices,"schema_digest":schema_digest,
                "result_set":result_set,"source_profile_id":reference.connection_profile_id,"created_at":reference.created_at,"source_status":reference.status}),
                reference.status != sift_protocol::RoomQueryStatus::Ok,
                AiVisibility::RoomPublic,
            )
        }
        AiAttachmentSource::QueryHistory { history_id } => {
            let profile = target.profile_id.map(ConnectionProfileId);
            let room = chat.room_id.map(RoomId);
            let id = sift_metadata::QueryHistoryId(*history_id);
            let mut entry = metadata_blocking(move || {
                if let Some(room) = room {
                    if let Some(entry) =
                        metadata.ai_room_query_history_entry(tenant, room, actor, id)?
                    {
                        return Ok(entry);
                    }
                }
                let profile = profile.ok_or_else(|| {
                    ApiError::BadRequest(
                        "Select the history entry's profile before attaching".into(),
                    )
                })?;
                metadata
                    .ai_query_history_entry(tenant, profile, actor, id, room)
                    .map_err(Into::into)
            })
            .await?;
            let shared = chat.room_id.is_some() && entry.room_id.map(|room| room.0) == chat.room_id;
            if chat.visibility == AiVisibility::RoomPublic && entry.room_id.is_some() && !shared {
                return Err(ApiError::Forbidden(
                    "History from another room cannot be republished here".into(),
                ));
            }
            let mut trimmed = shorten(&mut entry.sql_text, 8192);
            if let Some(error) = &mut entry.error_message {
                trimmed |= shorten(error, 2048);
            }
            let redacted = entry.sql_text.starts_with("sqlfp:");
            (
                "Query history/error · historical statement".into(),
                json!({"history":entry,"sql_excerpt":trimmed,"sql_not_stored":redacted,"notice":"Historical query record; bind values are excluded. No SQL was executed."}),
                trimmed,
                if shared {
                    AiVisibility::RoomPublic
                } else {
                    AiVisibility::Private
                },
            )
        }
        AiAttachmentSource::PlanCapture { capture_id } => {
            let profile = ConnectionProfileId(target.profile_id.ok_or_else(|| {
                ApiError::BadRequest("Select the saved plan's profile before attaching".into())
            })?);
            let id = *capture_id;
            let capture = metadata_blocking(move || {
                metadata
                    .ai_plan_capture(tenant, profile, actor, id)
                    .map_err(Into::into)
            })
            .await?;
            let content = super::ai_context_tools::plan_snapshot(capture, limit);
            let truncated = content["truncated"].as_bool().unwrap_or(false);
            (
                "Saved execution plan · historical capture".into(),
                content,
                truncated,
                AiVisibility::Private,
            )
        }
    };
    // Trim complete selected rows only. The preview discloses exactly which
    // requested ordinals remain, and never substitutes or re-executes a row.
    loop {
        let bytes = serde_json::to_vec(&content)
            .map_err(|_| ApiError::Internal("Cannot encode AI attachment".into()))?;
        if bytes.len() <= limit {
            break;
        }
        let rows = content
            .get_mut("rows")
            .and_then(serde_json::Value::as_array_mut)
            .filter(|rows| !rows.is_empty())
            .ok_or_else(|| {
                ApiError::BadRequest(
                    "Attachment exceeds the byte limit; choose fewer columns or a smaller source"
                        .into(),
                )
            })?;
        rows.pop();
        if let Some(ordinals) = content
            .get_mut("row_ordinals")
            .and_then(serde_json::Value::as_array_mut)
        {
            ordinals.pop();
        }
        truncated = true;
    }
    if content
        .get("rows")
        .and_then(serde_json::Value::as_array)
        .is_some_and(Vec::is_empty)
    {
        return Err(ApiError::BadRequest(
            "Selected cells exceed the attachment limit; choose a smaller projection".into(),
        ));
    }
    let sha256 = attachment_digest(source, &content)?;
    let attachment = AiContextAttachment {
        source: source.clone(),
        label,
        content,
        sha256,
        truncated,
        origin_visibility,
        published_by: None,
    };
    check_source_authority(state, auth, chat, target, &attachment).await?;
    Ok(attachment)
}
fn shorten(text: &mut String, limit: usize) -> bool {
    if text.len() <= limit {
        return false;
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    true
}
fn attachment_digest(
    source: &AiAttachmentSource,
    content: &serde_json::Value,
) -> ApiResult<String> {
    let bytes = serde_json::to_vec(&json!({"source":source,"content":content}))
        .map_err(|_| ApiError::Internal("Cannot bind attachment body".into()))?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}
