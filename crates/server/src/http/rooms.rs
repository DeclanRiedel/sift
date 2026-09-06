//! Room membership and document resource handlers.

use super::*;

pub(super) async fn list_metadata_rooms(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<RoomListQuery>,
) -> ApiResult<Json<Vec<Room>>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state, headers).await?;
    let tenant = tenant_id(q.tenant)?;
    ensure_tenant(&auth, tenant)?;
    Ok(Json(
        metadata_blocking(move || {
            metadata
                .list_rooms_for_principal(tenant, auth.principal_id)
                .map_err(Into::into)
        })
        .await?,
    ))
}

pub(super) async fn create_metadata_room(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateRoomRequest>,
) -> ApiResult<Json<Room>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let tenant = tenant_id(req.tenant_id)?;
    ensure_tenant(&auth, tenant)?;
    admit_resolved_tenant(
        &state,
        &auth,
        Some(tenant),
        sift_protocol::RateLimitClass::Control,
        "/v1/metadata/rooms",
    )?;
    let room = metadata_blocking(move || {
        metadata
            .create_room(
                tenant,
                auth.principal_id,
                NewRoom {
                    name: req.name,
                    kind: metadata_room_kind(req.kind),
                },
            )
            .map_err(Into::into)
    })
    .await?;
    push_metadata_operation(&state, auth.principal_id, "create", "room", Some(room.id.0));
    Ok(Json(room))
}

pub(super) async fn delete_metadata_room(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let room = room_id(id)?;
    let actor = auth.principal_id;
    metadata_blocking(move || {
        ensure_room_permission(&metadata, &auth, room, RoomPermission::Admin)?;
        metadata.delete_room(room)?;
        Ok(())
    })
    .await?;
    state.sessions.close_room_connection(room.0).await;
    state.rooms.results().remove_room(room.0);
    push_metadata_operation(&state, actor, "delete", "room", Some(room.0));
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn list_metadata_room_members(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<RoomMember>>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state, headers).await?;
    let room = room_id(id)?;
    Ok(Json(
        metadata_blocking(move || {
            ensure_room_permission(&metadata, &auth, room, RoomPermission::Read)?;
            metadata.list_room_members(room).map_err(Into::into)
        })
        .await?,
    ))
}

pub(super) async fn add_metadata_room_member(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<AddRoomMemberRequest>,
) -> ApiResult<Json<RoomMember>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let room = room_id(id)?;
    let principal = principal_id(req.principal_id)?;
    let actor = auth.principal_id;
    let member = metadata_blocking(move || {
        metadata
            .add_room_member_authorized(
                room,
                actor,
                principal,
                metadata_room_role(req.role),
                metadata_audit_record(actor, "add_member", "room", Some(room.0)),
            )
            .map_err(Into::into)
    })
    .await?;
    push_metadata_operation_local(&state, actor, "add_member", "room", Some(room.0));
    Ok(Json(member))
}

pub(super) async fn remove_metadata_room_member(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, principal)): Path<(i64, i64)>,
) -> ApiResult<Json<serde_json::Value>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let room = room_id(id)?;
    let principal = principal_id(principal)?;
    let actor = auth.principal_id;
    metadata_blocking(move || {
        metadata.remove_room_member_authorized(
            room,
            actor,
            principal,
            metadata_audit_record(actor, "remove_member", "room", Some(room.0)),
        )?;
        Ok(())
    })
    .await?;
    push_metadata_operation_local(&state, actor, "remove_member", "room", Some(room.0));
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn bind_metadata_room_connection(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<BindRoomConnectionRequest>,
) -> ApiResult<Json<sift_metadata::Room>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let room = room_id(id)?;
    let profile = connection_profile_id(req.connection_profile_id)?;
    let actor = auth.principal_id;
    metadata.authorize_vault_connection_use(metadata.get_room(room)?.tenant_id, actor, profile)?;
    let room_row = metadata_blocking(move || {
        metadata
            .bind_room_connection(
                room,
                actor,
                profile,
                metadata_audit_record(actor, "bind_connection", "room", Some(room.0)),
            )
            .map_err(Into::into)
    })
    .await?;
    // Drop any existing server-owned connection so the next room query
    // reopens under the newly bound profile (ADR-037).
    state.sessions.close_room_connection(room.0).await;
    push_metadata_operation_local(&state, actor, "bind_connection", "room", Some(room.0));
    Ok(Json(room_row))
}

pub(super) async fn unbind_metadata_room_connection(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<sift_metadata::Room>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let room = room_id(id)?;
    let actor = auth.principal_id;
    let room_row = metadata_blocking(move || {
        metadata
            .unbind_room_connection(
                room,
                actor,
                metadata_audit_record(actor, "unbind_connection", "room", Some(room.0)),
            )
            .map_err(Into::into)
    })
    .await?;
    // Close the room's server-owned connection now that it is unbound.
    state.sessions.close_room_connection(room.0).await;
    push_metadata_operation_local(&state, actor, "unbind_connection", "room", Some(room.0));
    Ok(Json(room_row))
}

pub(super) async fn join_metadata_room(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<RoomMember>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let room = room_id(id)?;
    let principal = auth.principal_id;
    let member = metadata_blocking(move || {
        ensure_room_permission(&metadata, &auth, room, RoomPermission::Read)?;
        metadata
            .get_room_member(room, principal)?
            .ok_or(ApiError::Forbidden(
                "room membership must be granted by a room owner".into(),
            ))
    })
    .await?;
    push_metadata_operation(&state, principal, "join", "room", Some(room.0));
    Ok(Json(member))
}

pub(super) async fn leave_metadata_room(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let room = room_id(id)?;
    let principal = auth.principal_id;
    metadata_blocking(move || {
        ensure_room_permission(&metadata, &auth, room, RoomPermission::Read)?;
        metadata.leave_room_authorized(
            room,
            principal,
            metadata_audit_record(principal, "leave", "room", Some(room.0)),
        )?;
        Ok(())
    })
    .await?;
    push_metadata_operation_local(&state, principal, "leave", "room", Some(room.0));
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn list_metadata_documents(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<Document>>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state, headers).await?;
    let room = room_id(id)?;
    let principal = auth.principal_id;
    Ok(Json(
        metadata_blocking(move || {
            metadata
                .list_documents_for_principal(room, principal)
                .map_err(Into::into)
        })
        .await?,
    ))
}

pub(super) async fn create_metadata_document(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<CreateDocumentRequest>,
) -> ApiResult<Json<Document>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let room = room_id(id)?;
    let actor = auth.principal_id;
    let document = metadata_blocking(move || {
        let connection_profile_id = req.connection_profile_id.map(ConnectionProfileId);
        // The server owns the canonical Loro snapshot: build a fresh replica,
        // seed it with any initial text, and persist the snapshot plus its
        // encoded version. Clients no longer choose a backend or ship bytes.
        let replica = sift_doc::TextReplica::new(sift_doc::random_peer_id())
            .map_err(|e| ApiError::Internal(format!("failed to seed document replica: {e}")))?;
        if let Some(text) = req.initial_text.as_deref().filter(|t| !t.is_empty()) {
            replica
                .insert(0, text)
                .map_err(|e| ApiError::BadRequest(format!("invalid initial text: {e}")))?;
        }
        let crdt_state = replica
            .export_snapshot()
            .map_err(|e| ApiError::Internal(format!("failed to export document snapshot: {e}")))?;
        let snapshot_version = replica.version_vector();
        metadata
            .create_document_for_principal(
                room,
                actor,
                NewDocument {
                    kind: req.kind,
                    title: req.title,
                    crdt_state,
                    snapshot_version,
                    position: req.position,
                    connection_profile_id,
                },
            )
            .map_err(Into::into)
    })
    .await?;
    push_metadata_operation(&state, actor, "create", "document", Some(document.id.0));
    Ok(Json(document))
}

pub(super) async fn update_metadata_document(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<UpdateDocumentSnapshotRequest>,
) -> ApiResult<Json<Document>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let document = document_id(id)?;
    let actor = auth.principal_id;
    let updated = metadata_blocking(move || {
        metadata
            .update_document_snapshot_for_principal(document, actor, req.crdt_state)
            .map_err(Into::into)
    })
    .await?;
    push_metadata_operation(&state, actor, "update", "document", Some(document.0));
    Ok(Json(updated))
}

pub(super) async fn delete_metadata_document(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let document = document_id(id)?;
    let actor = auth.principal_id;
    metadata_blocking(move || {
        metadata.delete_document_for_principal(document, actor)?;
        Ok(())
    })
    .await?;
    push_metadata_operation(&state, actor, "delete", "document", Some(document.0));
    Ok(Json(json!({"ok": true})))
}
