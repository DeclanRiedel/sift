//! Explicit source discovery/review and independent room grants.
use super::*;
use sift_protocol::{
    ActivateAiExternalSourceRequest, AiExternalRoomGrant, AiExternalSource,
    DiscoverAiExternalSourceRequest, PublishAiExternalSourceRequest,
};

struct DiscoveryAudit {
    state: AppState,
    actor: PrincipalId,
    succeeded: bool,
}
impl Drop for DiscoveryAudit {
    fn drop(&mut self) {
        self.state.sessions.push_operation_full(
            Operation::Ai {
                action: "discover_external_inventory".into(),
                chat_id: None,
                run_id: None,
            },
            if self.succeeded {
                OperationStatus::Succeeded
            } else {
                OperationStatus::Failed
            },
            Some(self.actor.0),
            None,
            None,
            (!self.succeeded).then(|| "External discovery failed or was canceled".into()),
        );
    }
}

#[derive(Deserialize, JsonSchema)]
pub(super) struct SourcesQuery {
    tenant_id: i64,
}

async fn source_scope(
    state: &AppState,
    headers: HeaderMap,
    id: uuid::Uuid,
) -> ApiResult<(AuthContext, MetadataStore)> {
    let auth = resolve_auth_context_blocking(state.clone(), headers.clone()).await?;
    let metadata = metadata_store_cloned(state)?;
    ensure_tenant(&auth, metadata.ai_external_source_tenant(id).await?)?;
    Ok((auth, metadata))
}

async fn fresh_scope(
    state: &AppState,
    headers: HeaderMap,
    actor: PrincipalId,
    tenant: TenantId,
) -> ApiResult<()> {
    let fresh = resolve_auth_context_blocking(state.clone(), headers).await?;
    if fresh.principal_id != actor {
        return Err(ApiError::Unauthorized);
    }
    ensure_tenant(&fresh, tenant)
}

pub(super) async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<SourcesQuery>,
) -> ApiResult<Json<Vec<AiExternalSource>>> {
    ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers.clone()).await?;
    let tenant = tenant_id(query.tenant_id)?;
    ensure_tenant(&auth, tenant)?;
    let sources = metadata_store_cloned(&state)?
        .list_ai_external_sources(tenant, auth.principal_id)
        .await?;
    audit_ai(
        &state,
        auth.principal_id,
        "list_external_sources",
        None,
        None,
    );
    Ok(Json(sources))
}

pub(super) async fn get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<uuid::Uuid>,
) -> ApiResult<Json<AiExternalSource>> {
    ai_enabled(&state)?;
    let (auth, metadata) = source_scope(&state, headers.clone(), id).await?;
    let source = metadata.ai_external_source(id, auth.principal_id).await?;
    fresh_scope(
        &state,
        headers,
        auth.principal_id,
        TenantId(source.tenant_id),
    )
    .await?;
    audit_ai(
        &state,
        auth.principal_id,
        &format!("read_external_source:{id}"),
        None,
        None,
    );
    Ok(Json(source))
}

pub(super) async fn discover(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<DiscoverAiExternalSourceRequest>,
) -> ApiResult<Json<AiExternalSource>> {
    ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers.clone()).await?;
    let tenant = tenant_id(request.tenant_id)?;
    require_tenant_admin(&auth, tenant)?;
    let metadata = metadata_store_cloned(&state)?;
    let vault = request.vault_id.map(sift_api_types::VaultId);
    metadata
        .authorize_ai_external_registration(tenant, auth.principal_id, vault)
        .await?;
    let permit = ai_source_setup::admit(&state)?;
    let mut audit = DiscoveryAudit {
        state: state.clone(),
        actor: auth.principal_id,
        succeeded: false,
    };
    let definition = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        crate::ai_external_gateway::discover(
            request.label,
            request.endpoint,
            request.protocol,
            request.bearer_token.clone(),
        ),
    )
    .await
    .map_err(|_| ApiError::BadRequest("External discovery timed out".into()))?
    .map_err(ApiError::BadRequest)?;
    audit.succeeded = true;
    drop(audit);
    let result = ai_source_setup::persist(
        state,
        headers,
        auth.principal_id,
        tenant,
        "create_external_source".into(),
        permit,
        move |fresh| async move {
            require_tenant_admin(&fresh, tenant)?;
            Ok(metadata
                .create_ai_external_source(
                    tenant,
                    fresh.principal_id,
                    vault,
                    definition,
                    request.bearer_token,
                )
                .await?)
        },
    )
    .await?;
    Ok(Json(result))
}

pub(super) async fn activate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<uuid::Uuid>,
    Json(request): Json<ActivateAiExternalSourceRequest>,
) -> ApiResult<Json<AiExternalSource>> {
    ai_enabled(&state)?;
    let (auth, metadata) = source_scope(&state, headers.clone(), id).await?;
    let tenant = metadata.ai_external_source_tenant(id).await?;
    require_tenant_admin(&auth, tenant)?;
    let permit = ai_source_setup::admit(&state)?;
    let source = ai_source_setup::persist(
        state,
        headers,
        auth.principal_id,
        tenant,
        format!("activate_external_source:{id}"),
        permit,
        move |fresh| async move {
            require_tenant_admin(&fresh, tenant)?;
            Ok(metadata
                .activate_ai_external_source(id, fresh.principal_id, request)
                .await?)
        },
    )
    .await?;
    Ok(Json(source))
}

pub(super) async fn disable(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<uuid::Uuid>,
    Json(request): Json<ExpectedRevision>,
) -> ApiResult<StatusCode> {
    close(state, headers, id, request.expected_revision, false).await
}
pub(super) async fn delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<uuid::Uuid>,
    Query(request): Query<ExpectedRevision>,
) -> ApiResult<StatusCode> {
    close(state, headers, id, request.expected_revision, true).await
}
async fn close(
    state: AppState,
    headers: HeaderMap,
    id: uuid::Uuid,
    revision: u64,
    delete: bool,
) -> ApiResult<StatusCode> {
    // Closing future authority remains available while AI is disabled.
    let (auth, metadata) = source_scope(&state, headers.clone(), id).await?;
    let tenant = metadata.ai_external_source_tenant(id).await?;
    require_tenant_admin(&auth, tenant)?;
    let permit = ai_source_setup::admit(&state)?;
    ai_source_setup::persist(
        state,
        headers,
        auth.principal_id,
        tenant,
        format!(
            "{}_external_source:{id}",
            if delete { "delete" } else { "disable" }
        ),
        permit,
        move |fresh| async move {
            require_tenant_admin(&fresh, tenant)?;
            if delete {
                metadata
                    .delete_ai_external_source(id, fresh.principal_id, revision)
                    .await?;
            } else {
                metadata
                    .disable_ai_external_source(id, fresh.principal_id, revision)
                    .await?;
            }
            Ok(())
        },
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn publish(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(request): Json<PublishAiExternalSourceRequest>,
) -> ApiResult<Json<AiExternalRoomGrant>> {
    ai_enabled(&state)?;
    let room = room_id(id)?;
    let (auth, metadata) = source_scope(&state, headers.clone(), request.source.source_id).await?;
    let tenant = metadata.get_room(room)?.tenant_id;
    ensure_tenant(&auth, tenant)?;
    let permit = ai_source_setup::admit(&state)?;
    let grant = ai_source_setup::persist(
        state,
        headers,
        auth.principal_id,
        tenant,
        format!("publish_external_source:{id}"),
        permit,
        move |fresh| async move {
            ensure_tenant(&fresh, tenant)?;
            Ok(metadata
                .publish_ai_external_source(room, fresh.principal_id, request)
                .await?)
        },
    )
    .await?;
    Ok(Json(grant))
}

pub(super) async fn room_sources(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<sift_protocol::AiExternalRoomSource>>> {
    ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers.clone()).await?;
    let metadata = metadata_store_cloned(&state)?;
    let room = room_id(id)?;
    let tenant = metadata.get_room(room)?.tenant_id;
    ensure_tenant(&auth, tenant)?;
    let sources = metadata
        .list_ai_external_room_sources(room, auth.principal_id)
        .await?;
    fresh_scope(&state, headers, auth.principal_id, tenant).await?;
    audit_ai(
        &state,
        auth.principal_id,
        &format!("list_room_external_sources:{id}"),
        None,
        None,
    );
    Ok(Json(sources))
}

pub(super) async fn revoke(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((room, id)): Path<(i64, uuid::Uuid)>,
) -> ApiResult<StatusCode> {
    let auth = resolve_auth_context_blocking(state.clone(), headers.clone()).await?;
    let metadata = metadata_store_cloned(&state)?;
    let room = room_id(room)?;
    let tenant = metadata.get_room(room)?.tenant_id;
    ensure_tenant(&auth, tenant)?;
    metadata.require_ai_external_grant_room(id, room).await?;
    let permit = ai_source_setup::admit(&state)?;
    ai_source_setup::persist(
        state,
        headers,
        auth.principal_id,
        tenant,
        format!("revoke_external_grant:{id}"),
        permit,
        move |fresh| async move {
            ensure_tenant(&fresh, tenant)?;
            metadata.require_ai_external_grant_room(id, room).await?;
            metadata
                .revoke_ai_external_room_grant(id, fresh.principal_id)
                .await?;
            Ok(())
        },
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn refresh(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<uuid::Uuid>,
    Json(request): Json<sift_protocol::RefreshAiExternalSourceRequest>,
) -> ApiResult<Json<AiExternalSource>> {
    ai_enabled(&state)?;
    let (auth, metadata) = source_scope(&state, headers.clone(), id).await?;
    let tenant = metadata.ai_external_source_tenant(id).await?;
    require_tenant_admin(&auth, tenant)?;
    let permit = ai_source_setup::admit(&state)?;
    let credential = metadata
        .ai_external_refresh_credential(
            id,
            auth.principal_id,
            request.expected_revision,
            &request.endpoint,
            &request.credentials,
        )
        .await?;
    let mut audit = DiscoveryAudit {
        state: state.clone(),
        actor: auth.principal_id,
        succeeded: false,
    };
    let definition = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        crate::ai_external_gateway::discover(
            request.label,
            request.endpoint,
            request.protocol,
            credential.bearer_token,
        ),
    )
    .await
    .map_err(|_| ApiError::BadRequest("Source rediscovery timed out".into()))?
    .map_err(ApiError::BadRequest)?;
    audit.succeeded = true;
    drop(audit);
    let source = ai_source_setup::persist(
        state,
        headers,
        auth.principal_id,
        tenant,
        format!("refresh_external_source:{id}"),
        permit,
        move |fresh| async move {
            require_tenant_admin(&fresh, tenant)?;
            Ok(metadata
                .refresh_ai_external_source(
                    id,
                    fresh.principal_id,
                    request.expected_revision,
                    definition,
                    request.credentials,
                )
                .await?)
        },
    )
    .await?;
    Ok(Json(source))
}

pub(super) async fn grant_review(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<sift_protocol::AiExternalRoomGrantHeader>>> {
    let auth = resolve_auth_context_blocking(state.clone(), headers.clone()).await?;
    let metadata = metadata_store_cloned(&state)?;
    let room = room_id(id)?;
    let tenant = metadata.get_room(room)?.tenant_id;
    require_tenant_admin(&auth, tenant)?;
    let grants = metadata
        .list_ai_external_room_grants_for_review(room, auth.principal_id)
        .await?;
    fresh_scope(&state, headers, auth.principal_id, tenant).await?;
    audit_ai(
        &state,
        auth.principal_id,
        &format!("review_external_grants:{id}"),
        None,
        None,
    );
    Ok(Json(grants))
}
