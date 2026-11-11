//! Captured human review lifecycle. No provider tool or run activation enters here.
use super::delegation_json::DelegationJson as Json;
use super::*;
use sift_protocol::*;
use uuid::Uuid;

fn instance(
    state: &AppState,
    auth: &AuthContext,
    headers: &HeaderMap,
) -> ApiResult<sift_metadata::AiDelegationAdmissionPolicy> {
    // Freeze an opaque verified credential record, never presented token bytes.
    let credential = if auth.database_credential {
        let presented = bearer_from_headers(headers)
            .or_else(|| cookie_value(headers, "sift_access"))
            .ok_or(ApiError::Unauthorized)?;
        Some(
            match metadata_store_cloned(state)?
                .extension_verified_credential_reference(auth.principal_id, presented)?
            {
                sift_metadata::ExtensionCredentialProof::ApiToken(id, created) => {
                    ("api_token", id, created)
                }
                sift_metadata::ExtensionCredentialProof::AccessToken(id, created) => {
                    ("access_token", id, created)
                }
            },
        )
    } else {
        None
    };
    let ai = &state.auth.ai;
    Ok(sift_metadata::AiDelegationAdmissionPolicy {
        enabled: ai.enabled && ai.execution_enabled && ai.delegation_enabled,
        arbitrary_sql_enabled: ai.arbitrary_sql_delegation_enabled,
        scope_sha256: sift_core::ai_delegation::canonical_digest(&(
            "sift.ai.human-delegation-scope.v1",
            &state.auth.instance_id,
            &state.auth.daemon_generation,
            auth.principal_id.0,
            &auth.auth_session_id,
            auth.token_tenant_scope,
            credential,
            ai,
        ))
        .map_err(|_| ApiError::Internal("Invalid delegation policy scope".into()))?,
        max_calls: ai.max_tool_calls_per_run,
        max_duration_secs: ai.max_run_secs,
    })
}

async fn caller(state: &AppState, headers: &HeaderMap) -> ApiResult<AuthContext> {
    let auth = resolve_auth_context_blocking(state.clone(), headers.clone()).await?;
    super::ai_database::captured_review_caller(state, &auth, headers)?;
    Ok(auth)
}

// Erase work futures at the retained owner boundary. Keeping every large
// resolver future in the transport's generic type needlessly expands compiler IR.
type HumanWork<T> = Box<
    dyn FnOnce(
            AppState,
            AuthContext,
            HeaderMap,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = ApiResult<(TenantId, T)>> + Send>,
        > + Send,
>;

async fn human<T: Send + 'static>(
    state: AppState,
    headers: HeaderMap,
    action: &'static str,
    chat: Option<Uuid>,
    work: HumanWork<T>,
) -> ApiResult<Json<T>> {
    let auth = caller(&state, &headers).await?;
    let owned = state.clone();
    let owned_headers = headers.clone();
    let (tenant, result) = super::execution::owned_audited_operation(
        state.clone(),
        Operation::Ai {
            action: action.into(),
            chat_id: chat,
            run_id: None,
        },
        Some(auth.principal_id.0),
        Box::pin(async move {
            if owned.shutdown.is_draining() {
                return Err(ApiError::ServiceDraining);
            }
            let fresh = caller(&owned, &owned_headers).await?;
            work(owned, fresh, owned_headers).await
        }),
    )
    .await?;
    let fresh = caller(&state, &headers).await?;
    ensure_tenant(&fresh, tenant)?;
    Ok(Json(result))
}

async fn summary(
    state: &AppState,
    auth: &AuthContext,
    id: Uuid,
) -> ApiResult<AiDelegationReviewSummary> {
    let review = metadata_store_cloned(state)?
        .ai_delegation_review_summary(id, auth.principal_id)
        .await?;
    ensure_tenant(auth, TenantId(review.tenant_id))?;
    Ok(review)
}

async fn materials(
    state: &AppState,
    auth: &AuthContext,
    headers: &HeaderMap,
    target: &ToolContext,
    refs: &[AiDelegationMaterialReference],
) -> ApiResult<Vec<AiDelegationReviewMaterial>> {
    if refs.len() > 64 {
        return Err(ApiError::BadRequest(
            "Delegation review exceeds 64 materials".into(),
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for reference in refs {
        let id = match reference {
            AiDelegationMaterialReference::MigrationPlan { rule_id, .. }
            | AiDelegationMaterialReference::ExtensionTool { rule_id, .. }
            | AiDelegationMaterialReference::RoomQuery { rule_id }
            | AiDelegationMaterialReference::PrivateQuery { rule_id, .. } => *rule_id,
        };
        if id.is_nil() || !seen.insert(id) {
            return Err(ApiError::BadRequest(
                "Delegation material rule identity is missing or duplicated".into(),
            ));
        }
    }
    let mut result = Vec::with_capacity(refs.len());
    let (session, connection) = super::ai::ai_target_connection_ids(target)?;
    for reference in refs {
        result.push(match reference {
            AiDelegationMaterialReference::MigrationPlan { rule_id, plan_id } => {
                let proposal = state.sessions.migration_plan_ai_proposal(*plan_id);
                let native = state.sessions.migration_review_material(
                    session,
                    connection,
                    auth.principal_id,
                    *plan_id,
                    proposal,
                )?;
                AiDelegationReviewMaterial::MigrationPlan {
                    rule_id: *rule_id,
                    plan: native.plan,
                    changes: native.changes,
                }
            }
            AiDelegationMaterialReference::ExtensionTool { rule_id, tool_id } => {
                let dispatch = super::extension_actions::context(
                    state,
                    auth,
                    headers.clone(),
                    target.clone(),
                )?;
                let registry = state
                    .sessions
                    .tool_registry()
                    .ok_or_else(|| ApiError::BadRequest("Governed tools are unavailable".into()))?;
                let (tool, prepared) = registry
                    .prepare_contract_for_ai(tool_id, &dispatch)
                    .map_err(|_| {
                        ApiError::BadRequest("Governed tool review is unavailable".into())
                    })?;
                AiDelegationReviewMaterial::ExtensionTool {
                    rule_id: *rule_id,
                    tool: Box::new(tool),
                    context: target.clone(),
                    prepared_contract: prepared.review_context(),
                }
            }
            AiDelegationMaterialReference::RoomQuery { rule_id } => {
                let room = target.room_id.ok_or_else(|| {
                    ApiError::BadRequest("Room query requires its original room".into())
                })?;
                let document = target
                    .document_id
                    .as_deref()
                    .and_then(|id| id.parse().ok())
                    .ok_or_else(|| {
                        ApiError::BadRequest("Room query requires its original document".into())
                    })?;
                let metadata = metadata_store_cloned(state)?;
                let actor = auth.principal_id;
                metadata_blocking(move || {
                    let row =
                        metadata.get_document_for_principal(DocumentId(document), actor, true)?;
                    if row.kind != "sql" {
                        return Err(ApiError::BadRequest(
                            "Delegation query material requires a SQL document".into(),
                        ));
                    }
                    Ok(())
                })
                .await?;
                let original_sql = super::ai::committed_ai_document(
                    state,
                    auth.principal_id,
                    room,
                    document,
                    true,
                )
                .await?;
                AiDelegationReviewMaterial::Query {
                    rule_id: *rule_id,
                    base_revision: super::ai::ai_content_revision(&original_sql),
                    original_sql,
                }
            }
            AiDelegationMaterialReference::PrivateQuery {
                rule_id,
                original_sql,
                base_revision,
            } => AiDelegationReviewMaterial::Query {
                rule_id: *rule_id,
                original_sql: original_sql.clone(),
                base_revision: *base_revision,
            },
        });
    }
    Ok(result)
}

fn references(
    content: &AiDelegationReviewContent,
) -> ApiResult<Vec<AiDelegationMaterialReference>> {
    content
        .materials
        .iter()
        .map(|m| {
            Ok(match m {
                AiDelegationReviewMaterial::MigrationPlan { rule_id, plan, .. } => {
                    AiDelegationMaterialReference::MigrationPlan {
                        rule_id: *rule_id,
                        plan_id: plan.id,
                    }
                }
                AiDelegationReviewMaterial::ExtensionTool { rule_id, tool, .. } => {
                    AiDelegationMaterialReference::ExtensionTool {
                        rule_id: *rule_id,
                        tool_id: tool.id.clone(),
                    }
                }
                AiDelegationReviewMaterial::Query {
                    rule_id,
                    original_sql,
                    base_revision,
                } => {
                    let rule = content
                        .policy
                        .rules
                        .iter()
                        .find(|r| r.id == *rule_id)
                        .ok_or_else(|| {
                            ApiError::BadRequest("Reviewed query rule is unavailable".into())
                        })?;
                    match &rule.action {
                        AiDelegationActionRule::QueryEdit {
                            target: AiQueryExecutionTarget::RoomDocument { .. },
                            ..
                        } => AiDelegationMaterialReference::RoomQuery { rule_id: *rule_id },
                        AiDelegationActionRule::QueryEdit {
                            target: AiQueryExecutionTarget::PrivateEditor { .. },
                            ..
                        } => AiDelegationMaterialReference::PrivateQuery {
                            rule_id: *rule_id,
                            original_sql: original_sql.clone(),
                            base_revision: *base_revision,
                        },
                        _ => {
                            return Err(ApiError::BadRequest("Reviewed query rule changed".into()))
                        }
                    }
                }
            })
        })
        .collect()
}

async fn current_source(
    state: &AppState,
    auth: &AuthContext,
    target: &ToolContext,
    source: &AiExecutionSource,
) -> ApiResult<()> {
    if super::ai_execution::capture_target(state, auth, target).await? != *source {
        return Err(ApiError::BadRequest(
            "Delegation source changed; review again".into(),
        ));
    }
    super::ai_execution::source_current_for(
        &state.sessions,
        &metadata_store_cloned(state)?,
        auth.principal_id,
        source,
        OperationKind::ExecuteQuery,
        None,
        true,
    )?;
    Ok(())
}

pub(super) async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(chat): Path<Uuid>,
    Json(request): Json<CreateAiDelegationReview>,
) -> ApiResult<Json<AiDelegationReviewDetail>> {
    human(
        state,
        headers,
        "create_delegation_review",
        Some(chat),
        Box::new(move |mut state, auth, headers| {
            Box::pin(async move {
                super::ai_execution::enabled(&state)?;
                let policy = instance(&state, &auth, &headers)?;
                if !policy.enabled {
                    return Err(ApiError::Forbidden(
                        "Run delegation reviews are disabled by instance policy".into(),
                    ));
                }
                super::ai::check_ai_chat_scope(&state, &auth, chat).await?;
                let metadata = metadata_store_cloned(&state)?;
                metadata
                    .list_ai_delegation_reviews(chat, auth.principal_id)
                    .await?;
                sift_core::ai_delegation::validate_policy(&request.policy)
                    .map_err(|_| ApiError::BadRequest("Delegation policy is invalid".into()))?;
                let source =
                    super::ai_execution::capture_target(&state, &auth, &request.target).await?;
                ensure_tenant(&auth, TenantId(source.tenant_id))?;
                let captured = super::ai_execution::source_current_for(
                    &state.sessions,
                    &metadata,
                    auth.principal_id,
                    &source,
                    OperationKind::ExecuteQuery,
                    None,
                    true,
                )?;
                state.sessions =
                    state
                        .sessions
                        .bind_source(source.session, source.connection, captured)?;
                let tenant = TenantId(source.tenant_id);
                let tenant_policy = metadata
                    .ai_delegation_tenant_policy(tenant, auth.principal_id)
                    .await?;
                let content = AiDelegationReviewContent {
                    policy: request.policy.clone(),
                    target: request.target.clone(),
                    source,
                    scope_sha256: policy.scope_sha256.clone(),
                    tenant_policy_revision: tenant_policy.revision,
                    desktop_id: request.desktop_id,
                    materials: materials(
                        &state,
                        &auth,
                        &headers,
                        &request.target,
                        &request.material_references,
                    )
                    .await?,
                };
                let fresh = caller(&state, &headers).await?;
                current_source(&state, &fresh, &content.target, &content.source).await?;
                let result = metadata
                    .create_ai_delegation_review(
                        chat,
                        fresh.principal_id,
                        request,
                        content,
                        instance(&state, &fresh, &headers)?,
                    )
                    .await?;
                Ok((tenant, result))
            })
        }),
    )
    .await
}

pub(super) async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(chat): Path<Uuid>,
) -> ApiResult<Json<Vec<AiDelegationReviewSummary>>> {
    human(
        state,
        headers,
        "list_delegation_reviews",
        Some(chat),
        Box::new(move |state, auth, _| {
            Box::pin(async move {
                super::ai::check_ai_chat_scope(&state, &auth, chat).await?;
                let metadata = metadata_store_cloned(&state)?;
                let tenant = metadata.ai_chat_tenant(chat).await?;
                Ok((
                    tenant,
                    metadata
                        .list_ai_delegation_reviews(chat, auth.principal_id)
                        .await?,
                ))
            })
        }),
    )
    .await
}

pub(super) async fn inspect(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<AiDelegationReviewDetail>> {
    human(
        state,
        headers,
        "read_delegation_review",
        None,
        Box::new(move |state, auth, _| {
            Box::pin(async move {
                let review = summary(&state, &auth, id).await?;
                Ok((
                    TenantId(review.tenant_id),
                    metadata_store_cloned(&state)?
                        .ai_delegation_review(id, auth.principal_id)
                        .await?,
                ))
            })
        }),
    )
    .await
}

pub(super) async fn approve(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<ApproveAiDelegationReview>,
) -> ApiResult<Json<AiDelegationReviewSummary>> {
    human(
        state,
        headers,
        "approve_delegation_review",
        None,
        Box::new(move |mut state, auth, headers| {
            Box::pin(async move {
                super::ai_execution::enabled(&state)?;
                let row = summary(&state, &auth, id).await?;
                let metadata = metadata_store_cloned(&state)?;
                let detail = metadata.ai_delegation_review(id, auth.principal_id).await?;
                current_source(
                    &state,
                    &auth,
                    &detail.content.target,
                    &detail.content.source,
                )
                .await?;
                let source = &detail.content.source;
                let captured = state
                    .sessions
                    .conn_entry(source.session, source.connection)?;
                state.sessions =
                    state
                        .sessions
                        .bind_source(source.session, source.connection, captured)?;
                let live = materials(
                    &state,
                    &auth,
                    &headers,
                    &detail.content.target,
                    &references(&detail.content)?,
                )
                .await?;
                if sift_core::ai_delegation::canonical_digest(&live)
                    .map_err(|_| ApiError::Internal("Invalid delegation materials".into()))?
                    != sift_core::ai_delegation::canonical_digest(&detail.content.materials)
                        .map_err(|_| ApiError::Internal("Invalid delegation materials".into()))?
                {
                    return Err(ApiError::BadRequest(
                        "Delegation review material changed; review again".into(),
                    ));
                }
                let fresh = caller(&state, &headers).await?;
                current_source(
                    &state,
                    &fresh,
                    &detail.content.target,
                    &detail.content.source,
                )
                .await?;
                Ok((
                    TenantId(row.tenant_id),
                    metadata
                        .approve_ai_delegation_review(
                            id,
                            fresh.principal_id,
                            request,
                            instance(&state, &fresh, &headers)?,
                        )
                        .await?,
                ))
            })
        }),
    )
    .await
}

async fn end(
    state: AppState,
    headers: HeaderMap,
    id: Uuid,
    request: EndAiDelegationReview,
    revoke: bool,
) -> ApiResult<Json<AiDelegationReviewSummary>> {
    human(
        state,
        headers,
        if revoke {
            "revoke_delegation_review"
        } else {
            "deny_delegation_review"
        },
        None,
        Box::new(move |state, auth, _| {
            Box::pin(async move {
                let row = summary(&state, &auth, id).await?;
                let metadata = metadata_store_cloned(&state)?;
                let changed = if revoke {
                    metadata
                        .revoke_ai_delegation_review(id, auth.principal_id, request)
                        .await?
                } else {
                    metadata
                        .deny_ai_delegation_review(id, auth.principal_id, request)
                        .await?
                };
                for run in changed.closed_runs {
                    state.sessions.cancel_ai_work(run);
                }
                Ok((TenantId(row.tenant_id), changed.review))
            })
        }),
    )
    .await
}
pub(super) async fn deny(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<EndAiDelegationReview>,
) -> ApiResult<Json<AiDelegationReviewSummary>> {
    end(state, headers, id, request, false).await
}
pub(super) async fn revoke(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<EndAiDelegationReview>,
) -> ApiResult<Json<AiDelegationReviewSummary>> {
    end(state, headers, id, request, true).await
}

pub(super) async fn tenant_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(tenant): Path<i64>,
) -> ApiResult<Json<AiDelegationTenantPolicy>> {
    human(
        state,
        headers,
        "read_delegation_tenant_policy",
        None,
        Box::new(move |state, auth, _| {
            Box::pin(async move {
                let tenant = tenant_id(tenant)?;
                ensure_tenant(&auth, tenant)?;
                Ok((
                    tenant,
                    metadata_store_cloned(&state)?
                        .ai_delegation_tenant_policy(tenant, auth.principal_id)
                        .await?,
                ))
            })
        }),
    )
    .await
}
pub(super) async fn set_tenant_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(tenant): Path<i64>,
    Json(request): Json<SetAiDelegationTenantPolicy>,
) -> ApiResult<Json<AiDelegationTenantPolicy>> {
    human(
        state,
        headers,
        "set_delegation_tenant_policy",
        None,
        Box::new(move |state, auth, _| {
            Box::pin(async move {
                let tenant = tenant_id(tenant)?;
                ensure_tenant(&auth, tenant)?;
                let changed = metadata_store_cloned(&state)?
                    .set_ai_delegation_tenant_policy(tenant, auth.principal_id, request)
                    .await?;
                for run in changed.closed_runs {
                    state.sessions.cancel_ai_work(run);
                }
                Ok((tenant, changed.policy))
            })
        }),
    )
    .await
}

pub(super) async fn preview(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(chat): Path<Uuid>,
    Json(request): Json<PreviewAiDelegationMaterialsRequest>,
) -> ApiResult<Json<AiDelegationMaterialsPreview>> {
    human(
        state,
        headers,
        "preview_delegation_materials",
        Some(chat),
        Box::new(move |mut state, auth, headers| {
            Box::pin(async move {
                super::ai_execution::enabled(&state)?;
                if !instance(&state, &auth, &headers)?.enabled {
                    return Err(ApiError::Forbidden(
                        "Run delegation reviews are disabled by instance policy".into(),
                    ));
                }
                super::ai::check_ai_chat_scope(&state, &auth, chat).await?;
                let metadata = metadata_store_cloned(&state)?;
                // Check chat ownership before resolving private native material.
                metadata
                    .list_ai_delegation_reviews(chat, auth.principal_id)
                    .await?;
                let source =
                    super::ai_execution::capture_target(&state, &auth, &request.target).await?;
                let tenant = TenantId(source.tenant_id);
                ensure_tenant(&auth, tenant)?;
                if metadata.ai_chat_tenant(chat).await? != tenant {
                    return Err(ApiError::Forbidden(
                        "Delegation chat and source tenant differ".into(),
                    ));
                }
                if !metadata
                    .ai_delegation_tenant_policy(tenant, auth.principal_id)
                    .await?
                    .enabled
                {
                    return Err(ApiError::Forbidden(
                        "Run delegation reviews are disabled by tenant policy".into(),
                    ));
                }
                let captured = super::ai_execution::source_current_for(
                    &state.sessions,
                    &metadata,
                    auth.principal_id,
                    &source,
                    OperationKind::ExecuteQuery,
                    None,
                    true,
                )?;
                state.sessions =
                    state
                        .sessions
                        .bind_source(source.session, source.connection, captured)?;
                let materials = materials(
                    &state,
                    &auth,
                    &headers,
                    &request.target,
                    &request.material_references,
                )
                .await?;
                if serde_json::to_vec(&materials)
                    .map_err(|_| ApiError::Internal("Invalid delegation material".into()))?
                    .len()
                    > 1024 * 1024
                {
                    return Err(ApiError::BadRequest(
                        "Delegation review material exceeds 1 MiB".into(),
                    ));
                }
                let fingerprints = materials
                    .iter()
                    .map(|material| {
                        let invalid = || {
                            ApiError::BadRequest(
                                "Delegation material cannot be fingerprinted".into(),
                            )
                        };
                        Ok(match material {
                            AiDelegationReviewMaterial::MigrationPlan {
                                rule_id,
                                plan,
                                changes,
                            } => AiDelegationMaterialFingerprint::MigrationPlan {
                                rule_id: *rule_id,
                                contract_sha256:
                                    sift_core::ai_delegation::migration_contract_digest(
                                        plan, changes,
                                    )
                                    .map_err(|_| invalid())?,
                                change_count: changes.len().try_into().map_err(|_| invalid())?,
                            },
                            AiDelegationReviewMaterial::ExtensionTool {
                                rule_id,
                                tool,
                                context,
                                prepared_contract,
                            } => AiDelegationMaterialFingerprint::ExtensionTool {
                                rule_id: *rule_id,
                                contract_sha256: sift_core::ai_delegation::canonical_digest(&(
                                    tool,
                                    prepared_contract,
                                    context,
                                ))
                                .map_err(|_| invalid())?,
                            },
                            AiDelegationReviewMaterial::Query {
                                rule_id,
                                original_sql,
                                base_revision,
                            } => AiDelegationMaterialFingerprint::Query {
                                rule_id: *rule_id,
                                before_sha256: format!(
                                    "{:x}",
                                    Sha256::digest(original_sql.as_bytes())
                                ),
                                base_revision: *base_revision,
                            },
                        })
                    })
                    .collect::<ApiResult<Vec<_>>>()?;
                let fresh = caller(&state, &headers).await?;
                current_source(&state, &fresh, &request.target, &source).await?;
                Ok((
                    tenant,
                    AiDelegationMaterialsPreview {
                        source,
                        materials,
                        fingerprints,
                    },
                ))
            })
        }),
    )
    .await
}
