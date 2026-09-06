//! Vault HTTP handlers; shared admission and audit stay at the router boundary.

use super::*;

pub(super) async fn list_metadata_vaults(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<VaultListQuery>,
) -> ApiResult<Json<Vec<sift_api_types::Vault>>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state, headers).await?;
    let tenant = tenant_id(query.tenant)?;
    ensure_tenant(&auth, tenant)?;
    let actor = api_principal(auth.principal_id);
    Ok(Json(
        metadata_blocking(move || {
            metadata
                .list_vaults(sift_api_types::TenantId(tenant.0), actor)
                .map_err(Into::into)
        })
        .await?,
    ))
}

pub(super) async fn remove_metadata_tenant_member(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((tenant_id_value, principal_id_value)): Path<(i64, i64)>,
) -> ApiResult<Json<serde_json::Value>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let tenant = tenant_id(tenant_id_value)?;
    let principal = principal_id(principal_id_value)?;
    ensure_tenant(&auth, tenant)?;
    let actor = auth.principal_id;
    let rooms = metadata_blocking(move || {
        metadata
            .remove_tenant_membership(
                tenant,
                actor,
                principal,
                metadata_audit_record(actor, "remove_member", "tenant", Some(tenant.0)),
            )
            .map_err(Into::into)
    })
    .await?;
    state.sessions.disconnect_managed_principal(principal).await;
    for room in rooms {
        state.sessions.close_room_connection(room.0).await;
    }
    push_metadata_operation_local(&state, actor, "remove_member", "tenant", Some(tenant.0));
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn create_metadata_vault(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<sift_api_types::CreateVaultRequest>,
) -> ApiResult<Json<sift_api_types::Vault>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let tenant = tenant_id(request.tenant_id)?;
    ensure_tenant(&auth, tenant)?;
    admit_resolved_tenant(
        &state,
        &auth,
        Some(tenant),
        sift_protocol::RateLimitClass::Control,
        "/v1/metadata/vaults",
    )?;
    let actor = api_principal(auth.principal_id);
    let scope = request.scope;
    let vault = metadata_blocking(move || match scope {
        sift_protocol::VaultScope::Personal => metadata
            .ensure_personal_vault(sift_api_types::TenantId(tenant.0), actor)
            .map_err(Into::into),
        sift_protocol::VaultScope::Team => metadata
            .create_team_vault(sift_api_types::TenantId(tenant.0), actor, &request.name)
            .map_err(Into::into),
    })
    .await?;
    state.sessions.push_operation(
        Operation::Vault {
            action: sift_protocol::VaultAction::Create,
            vault_id: Some(vault.id.0),
            item_id: None,
        },
        OperationStatus::Succeeded,
    );
    Ok(Json(vault))
}

pub(super) async fn get_metadata_vault(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<sift_api_types::Vault>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state, headers).await?;
    Ok(Json(
        metadata_blocking(move || {
            metadata
                .get_vault(
                    sift_api_types::VaultId(id),
                    api_principal(auth.principal_id),
                )
                .map_err(Into::into)
        })
        .await?,
    ))
}

pub(super) async fn update_metadata_vault(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(request): Json<sift_api_types::UpdateVaultRequest>,
) -> ApiResult<Json<sift_api_types::Vault>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let actor = api_principal(auth.principal_id);
    let vault = metadata_blocking(move || {
        metadata
            .update_vault(
                sift_api_types::VaultId(id),
                actor,
                request.expected_revision,
                &request.name,
            )
            .map_err(Into::into)
    })
    .await?;
    state.sessions.push_operation(
        Operation::Vault {
            action: sift_protocol::VaultAction::Update,
            vault_id: Some(id),
            item_id: None,
        },
        OperationStatus::Succeeded,
    );
    Ok(Json(vault))
}

pub(super) async fn delete_metadata_vault(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(query): Query<VaultExpectedRevisionQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let metadata = metadata_store(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let vault = metadata.get_vault(
        sift_api_types::VaultId(id),
        api_principal(auth.principal_id),
    )?;
    let profiles = metadata
        .delete_vault(
            sift_api_types::VaultId(id),
            api_principal(auth.principal_id),
            query.expected_revision,
        )
        .await?;
    for profile in profiles {
        state
            .sessions
            .disconnect_managed_profile(sift_metadata::TenantId(vault.tenant_id.0), profile)
            .await;
    }
    state.sessions.push_operation(
        Operation::Vault {
            action: sift_protocol::VaultAction::Delete,
            vault_id: Some(id),
            item_id: None,
        },
        OperationStatus::Succeeded,
    );
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn list_metadata_vault_items(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<sift_api_types::VaultItem>>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state, headers).await?;
    let actor = api_principal(auth.principal_id);
    Ok(Json(
        metadata_blocking(move || {
            metadata
                .list_vault_items(sift_api_types::VaultId(id), actor)
                .map_err(Into::into)
        })
        .await?,
    ))
}

pub(super) async fn create_metadata_vault_item(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(request): Json<sift_api_types::CreateVaultItemRequest>,
) -> ApiResult<Json<sift_api_types::VaultItem>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let item = metadata
        .create_vault_item(
            sift_api_types::VaultId(id),
            api_principal(auth.principal_id),
            request.label,
            request.metadata,
            request.secret,
        )
        .await?;
    state.sessions.push_operation(
        Operation::Vault {
            action: sift_protocol::VaultAction::Create,
            vault_id: Some(id),
            item_id: Some(item.id.0),
        },
        OperationStatus::Succeeded,
    );
    Ok(Json(item))
}

pub(super) async fn list_metadata_vault_grants(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<sift_api_types::VaultGrant>>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state, headers).await?;
    let actor = api_principal(auth.principal_id);
    Ok(Json(
        metadata_blocking(move || {
            metadata
                .list_vault_grants(sift_api_types::VaultId(id), actor)
                .map_err(Into::into)
        })
        .await?,
    ))
}

pub(super) async fn set_metadata_vault_grant(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, principal)): Path<(i64, i64)>,
    Json(request): Json<sift_api_types::SetVaultGrantRequest>,
) -> ApiResult<Json<sift_api_types::VaultGrant>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let actor = api_principal(auth.principal_id);
    let grant = metadata_blocking(move || {
        metadata
            .set_vault_grant(
                sift_api_types::VaultId(id),
                actor,
                sift_api_types::PrincipalId(principal),
                request.expected_revision,
                request.capabilities,
            )
            .map_err(Into::into)
    })
    .await?;
    if !grant.capabilities.use_secret {
        let lookup = metadata_store_cloned(&state)?;
        let profiles = metadata_blocking(move || {
            lookup
                .vault_connection_profiles(sift_api_types::VaultId(id))
                .map_err(Into::into)
        })
        .await?;
        for profile in profiles {
            state
                .sessions
                .disconnect_managed_profile_principal(
                    profile,
                    sift_metadata::PrincipalId(principal),
                )
                .await;
        }
    }
    state.sessions.push_operation(
        Operation::Vault {
            action: sift_protocol::VaultAction::Grant,
            vault_id: Some(id),
            item_id: None,
        },
        OperationStatus::Succeeded,
    );
    Ok(Json(grant))
}

pub(super) async fn delete_metadata_vault_grant(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, principal)): Path<(i64, i64)>,
    Query(query): Query<VaultExpectedRevisionQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let actor = api_principal(auth.principal_id);
    let profiles = metadata.vault_connection_profiles(sift_api_types::VaultId(id))?;
    metadata_blocking(move || {
        metadata
            .delete_vault_grant(
                sift_api_types::VaultId(id),
                actor,
                sift_api_types::PrincipalId(principal),
                query.expected_revision,
            )
            .map_err(Into::into)
    })
    .await?;
    for profile in profiles {
        state
            .sessions
            .disconnect_managed_profile_principal(profile, sift_metadata::PrincipalId(principal))
            .await;
    }
    state.sessions.push_operation(
        Operation::Vault {
            action: sift_protocol::VaultAction::Revoke,
            vault_id: Some(id),
            item_id: None,
        },
        OperationStatus::Succeeded,
    );
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn get_metadata_vault_item(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<sift_api_types::VaultItem>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state, headers).await?;
    Ok(Json(
        metadata_blocking(move || {
            metadata
                .get_vault_item(
                    sift_api_types::VaultItemId(id),
                    api_principal(auth.principal_id),
                )
                .map_err(Into::into)
        })
        .await?,
    ))
}

pub(super) async fn disconnect_vault_profile(
    state: &AppState,
    metadata: &MetadataStore,
    item: &sift_api_types::VaultItem,
    profile: Option<sift_metadata::ConnectionProfileId>,
    actor: sift_api_types::PrincipalId,
) -> ApiResult<()> {
    if let Some(profile) = profile {
        let vault = metadata.get_vault(item.vault_id, actor)?;
        state
            .sessions
            .disconnect_managed_profile(sift_metadata::TenantId(vault.tenant_id.0), profile)
            .await;
    }
    Ok(())
}

pub(super) async fn update_metadata_vault_item(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(request): Json<sift_api_types::UpdateVaultItemRequest>,
) -> ApiResult<Json<sift_api_types::VaultItem>> {
    let metadata = metadata_store(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let (item, profile) = metadata
        .update_vault_item(
            sift_api_types::VaultItemId(id),
            api_principal(auth.principal_id),
            request.expected_revision,
            request.label,
            request.metadata,
            request.secret,
        )
        .await?;
    disconnect_vault_profile(
        &state,
        metadata,
        &item,
        profile,
        api_principal(auth.principal_id),
    )
    .await?;
    state.sessions.push_operation(
        Operation::Vault {
            action: sift_protocol::VaultAction::Update,
            vault_id: Some(item.vault_id.0),
            item_id: Some(id),
        },
        OperationStatus::Succeeded,
    );
    Ok(Json(item))
}

pub(super) async fn set_metadata_vault_item_secret(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(request): Json<sift_api_types::SetVaultSecretRequest>,
) -> ApiResult<Json<sift_api_types::VaultItem>> {
    let metadata = metadata_store(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let (item, profile) = metadata
        .set_vault_secret(
            sift_api_types::VaultItemId(id),
            api_principal(auth.principal_id),
            request.expected_revision,
            request.secret,
        )
        .await?;
    disconnect_vault_profile(
        &state,
        metadata,
        &item,
        profile,
        api_principal(auth.principal_id),
    )
    .await?;
    state.sessions.push_operation(
        Operation::Vault {
            action: sift_protocol::VaultAction::SetSecret,
            vault_id: Some(item.vault_id.0),
            item_id: Some(id),
        },
        OperationStatus::Succeeded,
    );
    Ok(Json(item))
}

pub(super) async fn clear_metadata_vault_item_secret(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(query): Query<VaultExpectedRevisionQuery>,
) -> ApiResult<Json<sift_api_types::VaultItem>> {
    let metadata = metadata_store(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let (item, profile) = metadata.clear_vault_secret(
        sift_api_types::VaultItemId(id),
        api_principal(auth.principal_id),
        query.expected_revision,
    )?;
    disconnect_vault_profile(
        &state,
        metadata,
        &item,
        profile,
        api_principal(auth.principal_id),
    )
    .await?;
    state.sessions.push_operation(
        Operation::Vault {
            action: sift_protocol::VaultAction::SetSecret,
            vault_id: Some(item.vault_id.0),
            item_id: Some(id),
        },
        OperationStatus::Succeeded,
    );
    Ok(Json(item))
}

pub(super) async fn delete_metadata_vault_item(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(query): Query<VaultExpectedRevisionQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let metadata = metadata_store(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let item = metadata.get_vault_item(
        sift_api_types::VaultItemId(id),
        api_principal(auth.principal_id),
    )?;
    let vault = metadata.get_vault(item.vault_id, api_principal(auth.principal_id))?;
    let (_, profile) = metadata
        .delete_vault_item(
            item.id,
            api_principal(auth.principal_id),
            query.expected_revision,
        )
        .await?;
    if let Some(profile) = profile {
        state
            .sessions
            .disconnect_managed_profile(sift_metadata::TenantId(vault.tenant_id.0), profile)
            .await;
    }
    state.sessions.push_operation(
        Operation::Vault {
            action: sift_protocol::VaultAction::Delete,
            vault_id: Some(item.vault_id.0),
            item_id: Some(id),
        },
        OperationStatus::Succeeded,
    );
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn list_metadata_vault_item_versions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<sift_api_types::VaultItemVersion>>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state, headers).await?;
    let actor = api_principal(auth.principal_id);
    Ok(Json(
        metadata_blocking(move || {
            metadata
                .list_vault_item_versions(sift_api_types::VaultItemId(id), actor)
                .map_err(Into::into)
        })
        .await?,
    ))
}

pub(super) async fn get_metadata_vault_item_version(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, version)): Path<(i64, u64)>,
) -> ApiResult<Json<sift_api_types::VaultItemVersion>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state, headers).await?;
    Ok(Json(
        metadata_blocking(move || {
            metadata
                .get_vault_item_version(
                    sift_api_types::VaultItemId(id),
                    api_principal(auth.principal_id),
                    version,
                )
                .map_err(Into::into)
        })
        .await?,
    ))
}

pub(super) async fn diff_metadata_vault_item_versions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(query): Query<VaultVersionDiffQuery>,
) -> ApiResult<Json<sift_api_types::VaultItemVersionDiff>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state, headers).await?;
    Ok(Json(
        metadata_blocking(move || {
            metadata
                .diff_vault_item_versions(
                    sift_api_types::VaultItemId(id),
                    api_principal(auth.principal_id),
                    query.from,
                    query.to,
                )
                .map_err(Into::into)
        })
        .await?,
    ))
}

pub(super) async fn restore_metadata_vault_item(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(request): Json<sift_api_types::RestoreVaultItemRequest>,
) -> ApiResult<Json<sift_api_types::VaultItem>> {
    let metadata = metadata_store(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let actor = api_principal(auth.principal_id);
    let (item, profile) = metadata
        .restore_vault_item(
            sift_api_types::VaultItemId(id),
            actor,
            request.expected_revision,
            request.version,
        )
        .await?;
    disconnect_vault_profile(&state, metadata, &item, profile, actor).await?;
    state.sessions.push_operation(
        Operation::Vault {
            action: sift_protocol::VaultAction::Restore,
            vault_id: Some(item.vault_id.0),
            item_id: Some(id),
        },
        OperationStatus::Succeeded,
    );
    Ok(Json(item))
}

pub(super) async fn test_metadata_vault_item(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    let metadata = metadata_store(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let item_id = sift_api_types::VaultItemId(id);
    let item = metadata.get_vault_item(item_id, api_principal(auth.principal_id))?;
    if item.kind != sift_protocol::VaultItemKind::Connection {
        return Err(ApiError::BadRequest(
            "only connection vault items can be tested".into(),
        ));
    }
    let (_, profile_id) = metadata
        .vault_connection_binding_by_item(item_id)?
        .ok_or_else(|| ApiError::BadRequest("connection item is not bound to a profile".into()))?;
    let vault = metadata.get_vault(item.vault_id, api_principal(auth.principal_id))?;
    let tenant = sift_metadata::TenantId(vault.tenant_id.0);
    let profile = metadata.get_connection_profile(tenant, profile_id)?;
    let (configuration, credentials) = metadata
        .resolve_provider_connection(tenant, auth.principal_id, profile_id)
        .await?;
    let session = state.sessions.open_session_with_owner(
        OpenSessionRequest {
            tag: Some(format!("vault-test:{id}")),
            tenant_id: Some(tenant.0),
        },
        Some(auth.principal_id),
        Some(tenant),
        false,
    )?;
    let tested = state
        .sessions
        .open_managed_connection(
            session.id,
            profile.provider_id,
            configuration,
            credentials,
            auth.principal_id,
            tenant,
            profile_id,
            profile.policy.revision,
            auth.trusted_local,
        )
        .await;
    let _ = state.sessions.close_session(session.id);
    tested?;
    state.sessions.push_operation(
        Operation::Vault {
            action: sift_protocol::VaultAction::Test,
            vault_id: Some(item.vault_id.0),
            item_id: Some(id),
        },
        OperationStatus::Succeeded,
    );
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn reveal_metadata_vault_item(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    body: Option<Json<sift_api_types::VaultRevealRequest>>,
) -> ApiResult<Response> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    if !auth.trusted_local {
        let session_id = auth.auth_session_id.as_deref().ok_or_else(|| {
            ApiError::Forbidden("vault reveal requires an interactive session".into())
        })?;
        let lease = body
            .as_ref()
            .and_then(|Json(request)| request.lease.as_deref())
            .ok_or_else(|| {
                ApiError::Forbidden("vault reveal requires a recent step-up authentication".into())
            })?;
        if !state
            .auth
            .runtime
            .consume_reveal_lease(lease, auth.principal_id, session_id, id)
        {
            return Err(ApiError::Forbidden(
                "vault reveal step-up is invalid or expired".into(),
            ));
        }
    }
    let value = metadata
        .reveal_vault_secret(
            sift_api_types::VaultItemId(id),
            api_principal(auth.principal_id),
        )
        .await?;
    state.sessions.push_operation(
        Operation::Vault {
            action: sift_protocol::VaultAction::Reveal,
            vault_id: None,
            item_id: Some(id),
        },
        OperationStatus::Succeeded,
    );
    let mut response = Json(sift_api_types::RevealVaultSecretResponse {
        item_id: id,
        value,
        expires_in_seconds: 30,
    })
    .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    Ok(response)
}

pub(super) async fn step_up_metadata_vault_reveal(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(request): Json<sift_api_types::VaultRevealStepUpRequest>,
) -> ApiResult<Response> {
    let metadata = metadata_store(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers.clone()).await?;
    let session_id = auth.auth_session_id.as_deref().ok_or_else(|| {
        ApiError::Forbidden("vault reveal requires an interactive session".into())
    })?;
    let item = sift_api_types::VaultItemId(id);
    let actor = api_principal(auth.principal_id);
    metadata.list_vault_item_versions(item, actor)?;
    let identity = metadata
        .password_identity_for_principal(auth.principal_id)?
        .ok_or_else(|| ApiError::Forbidden("password step-up is unavailable".into()))?;
    let source = headers
        .get(&PEER_ADDR_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown");
    let outcome = state
        .auth
        .runtime
        .authenticate_password(
            metadata,
            source,
            &identity.identity.subject,
            request.password.into_bytes(),
        )
        .await
        .map_err(|error| ApiError::Internal(error.to_string()))?;
    match outcome {
        crate::identity::PasswordAuthOutcome::Authenticated(identity)
            if identity.principal.id == auth.principal_id => {}
        crate::identity::PasswordAuthOutcome::Throttled => {
            return Err(ApiError::TooManyAuthAttempts);
        }
        _ => return Err(ApiError::Unauthorized),
    }
    let lease = state
        .auth
        .runtime
        .issue_reveal_lease(auth.principal_id, session_id, id)
        .ok_or(ApiError::TooManyAuthAttempts)?;
    state.sessions.push_operation(
        Operation::Vault {
            action: sift_protocol::VaultAction::StepUp,
            vault_id: None,
            item_id: Some(id),
        },
        OperationStatus::Succeeded,
    );
    let mut response = Json(sift_api_types::VaultRevealStepUpResponse {
        lease,
        expires_in_seconds: 60,
    })
    .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    Ok(response)
}
