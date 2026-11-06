//! Derive execution attribution from captured server-owned sources.
use super::*;

pub(super) async fn execute_metadata_context(
    state: &AppState,
    headers: HeaderMap,
    req: &ExecuteRequestHttp,
    source: Option<&crate::session::ConnectionEntryClone>,
) -> ApiResult<Option<ExecuteMetadataContext>> {
    if state.metadata.is_none() {
        if req.room_id.is_some() || req.connection_profile_id.is_some() || req.source.is_some() {
            return Err(ApiError::MetadataUnavailable);
        }
        return Ok(None);
    }
    if req.room_id.is_some() && req.tx.is_some() {
        return Err(ApiError::BadRequest(
            "room execution cannot use a caller-session transaction".into(),
        ));
    }

    let metadata = metadata_store_cloned(state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let room = req.room_id.map(room_id).transpose()?;
    let profile = req
        .connection_profile_id
        .map(connection_profile_id)
        .transpose()?;
    let metadata_for_check = metadata.clone();
    let auth_for_check = auth.clone();
    let sql = req.sql.clone();
    let requested_source = req.source.clone();
    let native_source = source.cloned();
    let (effective_profile, room_routing, tenant_id, database_target, versioned_source) =
        metadata_blocking(move || {
            let effective;
            let mut routing: Option<RoomRouting> = None;
            let mut tenant_id = None;
            let database_target;
            if let Some(room) = room {
                ensure_room_permission(
                    &metadata_for_check,
                    &auth_for_check,
                    room,
                    RoomPermission::Write,
                )?;
                let room_row = metadata_for_check.get_room(room)?;
                // A bound room runs every query through its server-owned
                // connection (ADR-037); an unbound room cannot execute.
                let profile_id = room_row.bound_connection_profile_id.ok_or_else(|| {
                    ApiError::BadRequest(
                        "room has no bound connection; bind one before running queries".into(),
                    )
                })?;
                if profile.is_some_and(|claimed| claimed != profile_id) {
                    return Err(ApiError::BadRequest("connection profile assertion does not match the room binding".into()));
                }
                let binder = room_row.bound_connection_by.ok_or_else(|| {
                    ApiError::Internal("bound room connection is missing its binder".into())
                })?;
                let bound_profile =
                    metadata_for_check.get_connection_profile(room_row.tenant_id, profile_id)?;
                metadata_for_check
                    .authorize_vault_connection_use(
                        room_row.tenant_id,
                        auth_for_check.principal_id,
                        profile_id,
                    )
                    .map_err(|error| match error {
                        sift_metadata::MetadataError::VaultPermissionDenied
                        | sift_metadata::MetadataError::TenantMembershipRequired { .. } => {
                            ApiError::Forbidden("vault connection access required".into())
                        }
                        other => ApiError::Metadata(other),
                    })?;
                tenant_id = Some(room_row.tenant_id);
                database_target = connection_database_target(&bound_profile);
                // Submitter-scoped intersection: the submitting member's room role
                // x the bound profile's policy, enforced BEFORE routing to the
                // shared connection (which itself authorizes only as the binder).
                let scope = room_submitter_scope(
                    &metadata_for_check,
                    auth_for_check.principal_id,
                    room,
                    room_row.tenant_id,
                    &bound_profile,
                )?;
                crate::authorization::authorize(&scope, sift_protocol::OperationKind::ExecuteQuery)
                    .map_err(|denial| ApiError::Forbidden(denial.public_reason().into()))?;
                crate::sql_policy::enforce(
                    &bound_profile.policy,
                    bound_profile.semantic_engine,
                    sift_protocol::OperationKind::ExecuteQuery,
                    Some(&sql),
                    &[],
                )?;
                effective = Some(profile_id);
                routing = Some(RoomRouting {
                    room_id: room.0,
                    binder,
                    tenant: room_row.tenant_id,
                    profile_id,
                    provider_id: bound_profile.provider_id,
                    engine: bound_profile.semantic_engine,
                    policy_revision: bound_profile.policy.revision,
                    configuration: bound_profile.configuration,
                });
            } else {
                let source = native_source.as_ref().ok_or_else(||
                    ApiError::Internal("direct query source was not captured".into()))?;
                match &source.provenance {
                    crate::session::ConnectionProvenance::Managed {
                        principal_id, tenant_id: tenant, profile_id, ..
                    } => {
                        if *principal_id != auth_for_check.principal_id {
                            return Err(ApiError::Forbidden("query caller must own the managed session".into()));
                        }
                        if profile.is_some_and(|claimed| claimed != *profile_id) {
                            return Err(ApiError::BadRequest("connection profile assertion does not match the captured connection".into()));
                        }
                        effective = Some(*profile_id);
                        tenant_id = Some(*tenant);
                    }
                    crate::session::ConnectionProvenance::TrustedLocal => {
                        if profile.is_some() {
                            return Err(ApiError::BadRequest("ad-hoc connections cannot claim a managed profile".into()));
                        }
                        effective = None;
                    }
                }
                database_target = configured_database_target(
                    &source.driver.provider().provider_id, &source.configuration);
            }
            if requested_source.is_some() && tenant_id.is_none() {
                return Err(ApiError::BadRequest("versioned execution requires a proven managed tenant".into()));
            }
            let versioned_source = requested_source
                .as_ref()
                .map(|source| {
                    metadata_for_check
                        .validate_versioned_execution(auth_for_check.principal_id, source)
                })
                .transpose()?;
            if let (Some(source), Some(tenant)) = (&versioned_source, tenant_id) {
                if room.is_some_and(|room| room.0 != source.room_id) {
                    return Err(ApiError::BadRequest("versioned execution source belongs to another room".into()));
                }
                let workspace_room = metadata_for_check.get_room(RoomId(source.room_id))?;
                if workspace_room.tenant_id != tenant {
                    return Err(ApiError::BadRequest(
                        "versioned execution source belongs to another tenant".into(),
                    ));
                }
            }
            Ok((
                effective,
                routing,
                tenant_id,
                database_target,
                versioned_source,
            ))
        })
        .await?;

    Ok(Some(ExecuteMetadataContext {
        metadata,
        principal_id: auth.principal_id,
        room_id: room,
        connection_profile_id: effective_profile,
        tenant_id,
        database_target,
        versioned_source,
        room_routing,
    }))
}
