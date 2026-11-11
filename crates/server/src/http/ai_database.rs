//! Typed AI staging and the separate human preview/apply boundary (ADR-102).
use super::ai::{
    ai_catalog_options, ai_chat_id, ai_connection_ids, ai_enabled, authorized_publication,
    check_ai_chat_scope, check_ai_run_scope,
};
use super::*;
use chrono::Utc;
use sha2::{Digest, Sha256};
use sift_protocol::{
    AiDatabaseDraft, AiDatabasePreview, AiDatabaseProposalDetail, AiDatabaseProposalReview,
};
use uuid::Uuid;

struct AiApplyObservation {
    migration: Option<crate::session::migration_journal::LedgerProjection>,
    rows: Option<crate::session::EditsExecutionObservation>,
    result_code: Option<String>,
}

fn observed_row_count(
    result: &sift_protocol::ApplyEditsResult,
    operation: sift_protocol::ChangeLedgerOperation,
) -> Option<i64> {
    use sift_protocol::{ChangeLedgerOperation, EditStatementKind};
    let kind = match operation {
        ChangeLedgerOperation::GridInsert => EditStatementKind::Insert,
        ChangeLedgerOperation::GridUpdate => EditStatementKind::Update,
        ChangeLedgerOperation::GridDelete => EditStatementKind::Delete,
        _ => return None,
    };
    result
        .applied
        .iter()
        .filter(|entry| entry.kind == kind)
        .try_fold(0i64, |sum, entry| {
            sum.checked_add(i64::try_from(entry.affected_rows).ok()?)
        })
}

fn audit_change(
    state: &AppState,
    actor: PrincipalId,
    action: &str,
    chat: Option<Uuid>,
    run: Option<Uuid>,
    success: bool,
) {
    state.sessions.push_operation_full(
        Operation::Ai {
            action: action.into(),
            chat_id: chat,
            run_id: run,
        },
        if success {
            OperationStatus::Succeeded
        } else {
            OperationStatus::Failed
        },
        Some(actor.0),
        None,
        None,
        (!success).then(|| "AI database proposal action failed".into()),
    );
}
async fn check_proposal_scope(state: &AppState, auth: &AuthContext, id: Uuid) -> ApiResult<Uuid> {
    let chat = metadata_store_cloned(state)?
        .ai_database_proposal_chat(id)
        .await?;
    check_ai_chat_scope(state, auth, chat).await?;
    Ok(chat)
}
fn draft_revision(draft: &AiDatabaseDraft) -> sift_protocol::CatalogRevision {
    match draft {
        AiDatabaseDraft::RowEditSet {
            expected_catalog_revision,
            ..
        }
        | AiDatabaseDraft::MigrationDraft {
            expected_catalog_revision,
            ..
        } => *expected_catalog_revision,
    }
}
fn draft_rows(draft: &AiDatabaseDraft) -> bool {
    matches!(draft, AiDatabaseDraft::RowEditSet { .. })
}
fn canonicalize_draft(
    draft: &mut AiDatabaseDraft,
    live: &sift_protocol::CatalogGraph,
) -> ApiResult<()> {
    if draft_revision(draft) != live.revision {
        return Err(ApiError::BadRequest(
            "Database schema changed; read the catalog and stage a fresh proposal".into(),
        ));
    }
    if let AiDatabaseDraft::MigrationDraft {
        desired_catalog, ..
    } = draft
    {
        if desired_catalog.provider.provider_id != live.provider.provider_id
            || desired_catalog.database_identity != live.database_identity
        {
            return Err(ApiError::BadRequest(
                "Desired schema belongs to another database or provider".into(),
            ));
        }
        sift_core::catalog::normalize_graph(&mut desired_catalog.data);
        sift_core::catalog::validate_graph(&desired_catalog.data, 2000, 10000).map_err(|_| {
            ApiError::BadRequest("Desired catalog is invalid or exceeds AI limits".into())
        })?;
        desired_catalog.content_digest = format!(
            "catfp:{:x}",
            Sha256::digest(
                serde_json::to_vec(&desired_catalog.data)
                    .map_err(|_| ApiError::Internal("cannot serialize desired catalog".into()))?
            )
        );
    }
    Ok(())
}

pub(super) async fn stage_ai_database_change(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
    Json(request): Json<sift_protocol::StageAiDatabaseProposalRequest>,
) -> ApiResult<Json<AiDatabaseProposalDetail>> {
    stage_ai_database_change_with_intent(State(state), headers, Path(raw), Json(request), None)
        .await
}

pub(super) async fn stage_ai_database_change_with_intent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
    Json(mut request): Json<sift_protocol::StageAiDatabaseProposalRequest>,
    intent: Option<sift_metadata::AiExternalProposalIntent>,
) -> ApiResult<Json<AiDatabaseProposalDetail>> {
    ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers.clone()).await?;
    let id = ai_chat_id(&raw)?;
    let mut chat = None;
    let result: ApiResult<_> = async {
        check_ai_run_scope(&state, &auth, id).await?;
        let work = state.sessions.register_ai_work(id)?;
        let metadata = metadata_store_cloned(&state)?;
        let run = metadata
            .ai_tool_run(id, auth.principal_id, request.lease_token)
            .await?;
        chat = Some(run.chat_id);
        if !run.mode.can_propose()
            || (Utc::now() - run.started_at).num_seconds() >= i64::from(state.auth.ai.max_run_secs)
        {
            return Err(ApiError::Forbidden(
                "An active Propose turn is required".into(),
            ));
        }
        let remaining = (run.started_at + chrono::Duration::seconds(i64::from(state.auth.ai.max_run_secs)) - Utc::now())
            .to_std().map_err(|_| ApiError::Forbidden("AI run time limit reached".into()))?;
        let preflight = async {
        let (source, publication, live) =
            if run.visibility == sift_protocol::AiVisibility::RoomPublic {
                let tool = if draft_rows(&request.draft) {
                    sift_protocol::AiToolKind::Select
                } else {
                    sift_protocol::AiToolKind::Catalog
                };
                let (grant, provenance) =
                    authorized_publication(&state, &auth, &run.context, tool).await?;
                let live = state
                    .sessions
                    .with_ai_room_connection(provenance, grant.id, |session, connection| {
                        let state = &state;
                        let auth = &auth;
                        let context = &run.context;
                        async move {
                            authorized_publication(state, auth, context, tool).await?;
                            let live = state
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
                            authorized_publication(state, auth, context, tool).await?;
                            Ok(live)
                        }
                    })
                    .await?;
                (grant.source.scope_digest, Some(grant.id), live)
            } else {
                let target = authorized_tool_context(&state, &auth, run.context.target.clone())?;
                if target != run.context.target {
                    return Err(ApiError::Forbidden(
                        "AI target authorization changed".into(),
                    ));
                }
                let (session, connection) = ai_connection_ids(&run.context)?;
                let (owner, tenant, profile, _) = state.sessions.managed_catalog_scope(
                    session,
                    connection,
                    sift_protocol::OperationKind::ReadCatalogGraph,
                )?;
                if owner != auth.principal_id {
                    return Err(ApiError::Forbidden(
                        "AI database caller must own the managed connection".into(),
                    ));
                }
                ensure_tenant(&auth, tenant)?;
                let profile_record = metadata.get_connection_profile(tenant, profile)?;
                if state
                    .sessions
                    .conn_entry(session, connection)?
                    .configuration
                    != profile_record.configuration
                {
                    return Err(ApiError::BadRequest(
                    "Connection configuration changed; reconnect before proposing database changes"
                        .into(),
                ));
                }
                let source = metadata
                    .ai_profile_source_digest(profile, auth.principal_id)
                    .await?;
                let live = state
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
                (source, None, live)
            };
        Ok::<_, ApiError>((source, publication, live))
        };
        let cancellation = work.token.clone();
        let (source, publication, live) = crate::ai_cancellation::scope(cancellation.clone(), async {
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => Err(ApiError::Forbidden("AI run stopped".into())),
                result = tokio::time::timeout(remaining, preflight) => result
                    .map_err(|_| ApiError::Forbidden("AI run time limit reached".into()))?,
            }
        }).await?;
        if work.token.is_cancelled() {
            return Err(ApiError::Forbidden("AI run stopped".into()));
        }
        metadata.ai_tool_run(id, auth.principal_id, request.lease_token).await?;
        let fresh_auth = resolve_auth_context_blocking(state.clone(), headers).await?;
        check_ai_run_scope(&state, &fresh_auth, id).await?;
        if run.visibility == sift_protocol::AiVisibility::RoomPublic {
            let tool = if draft_rows(&request.draft) { sift_protocol::AiToolKind::Select } else { sift_protocol::AiToolKind::Catalog };
            let (grant, _) = authorized_publication(&state, &fresh_auth, &run.context, tool).await?;
            if Some(grant.id) != publication || grant.source.scope_digest != source {
                return Err(ApiError::Forbidden("AI publication changed during proposal preparation".into()));
            }
        } else {
            if authorized_tool_context(&state, &fresh_auth, run.context.target.clone())? != run.context.target {
                return Err(ApiError::Forbidden("AI target authorization changed".into()));
            }
            let profile = run.context.target.profile_id.ok_or_else(|| ApiError::Forbidden("AI profile is unavailable".into()))?;
            if metadata.ai_profile_source_digest(sift_metadata::ConnectionProfileId(profile), fresh_auth.principal_id).await? != source {
                return Err(ApiError::Forbidden("AI source changed during proposal preparation".into()));
            }
        }
        canonicalize_draft(&mut request.draft, &live)?;
        metadata
            .stage_ai_database_proposal_with_intent(
                id,
                auth.principal_id,
                request,
                source,
                live.database_identity,
                publication,
                state.auth.ai.max_tool_calls_per_run,
                intent,
            )
            .await
            .map_err(Into::into)
    }
    .await;
    audit_change(
        &state,
        auth.principal_id,
        "stage_database_proposal",
        chat,
        Some(id),
        result.is_ok(),
    );
    Ok(Json(result?))
}

pub(super) async fn list_ai_database_changes(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
) -> ApiResult<Json<Vec<AiDatabaseProposalDetail>>> {
    ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let chat = ai_chat_id(&raw)?;
    let result: ApiResult<_> = async {
        check_ai_chat_scope(&state, &auth, chat).await?;
        metadata_store_cloned(&state)?
            .ai_database_proposals(chat, auth.principal_id)
            .await
            .map_err(Into::into)
    }
    .await;
    audit_change(
        &state,
        auth.principal_id,
        "read_database_proposals",
        Some(chat),
        None,
        result.is_ok(),
    );
    Ok(Json(result?))
}
pub(super) async fn discard_ai_database_change(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
) -> ApiResult<Json<AiDatabaseProposalDetail>> {
    ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let id = ai_chat_id(&raw)?;
    let mut chat = None;
    let result: ApiResult<_> = async {
        chat = Some(check_proposal_scope(&state, &auth, id).await?);
        metadata_store_cloned(&state)?
            .discard_ai_database_proposal(id, auth.principal_id)
            .await
            .map_err(Into::into)
    }
    .await;
    audit_change(
        &state,
        auth.principal_id,
        "discard_database_proposal",
        chat,
        None,
        result.is_ok(),
    );
    Ok(Json(result?))
}

/// Source publication is independent of the reviewer's normal write permission.
async fn review_binding(
    state: &AppState,
    auth: &AuthContext,
    proposal: &AiDatabaseProposalDetail,
    session: sift_protocol::SessionId,
    connection: sift_protocol::ConnectionId,
    operation: sift_protocol::OperationKind,
) -> ApiResult<sift_metadata::ConnectionProfile> {
    review_binding_checked(state, auth, proposal, session, connection, operation, true).await
}

async fn review_binding_checked(
    state: &AppState,
    auth: &AuthContext,
    proposal: &AiDatabaseProposalDetail,
    session: sift_protocol::SessionId,
    connection: sift_protocol::ConnectionId,
    operation: sift_protocol::OperationKind,
    check_transaction: bool,
) -> ApiResult<sift_metadata::ConnectionProfile> {
    let (owner, tenant, profile, _) = state
        .sessions
        .managed_catalog_scope(session, connection, operation)?;
    if owner != auth.principal_id
        || proposal.proposal.target.tenant_id != Some(tenant.0)
        || proposal.proposal.target.profile_id != Some(profile.0)
    {
        return Err(ApiError::Forbidden(
            "Review requires your managed connection on the proposal's database profile".into(),
        ));
    }
    if check_transaction
        && matches!(
            operation,
            sift_protocol::OperationKind::ApplyEdits | sift_protocol::OperationKind::ApplyMigration
        )
        && state
            .sessions
            .connection_has_transaction(session, connection)?
    {
        return Err(ApiError::BadRequest(
            "Finish the active transaction before committing AI database changes".into(),
        ));
    }
    ensure_tenant(auth, tenant)?;
    let metadata = metadata_store_cloned(state)?;
    let profile_record = metadata.get_connection_profile(tenant, profile)?;
    let entry = state.sessions.conn_entry(session, connection)?;
    if entry.configuration != profile_record.configuration
        || entry.driver.provider().provider_id != profile_record.provider_id
    {
        return Err(ApiError::BadRequest(
            "Connection configuration changed; reconnect and review again".into(),
        ));
    }
    review_profile_binding(state, auth, proposal, profile_record, operation).await
}

async fn review_profile_binding(
    state: &AppState,
    auth: &AuthContext,
    proposal: &AiDatabaseProposalDetail,
    profile_record: sift_metadata::ConnectionProfile,
    operation: sift_protocol::OperationKind,
) -> ApiResult<sift_metadata::ConnectionProfile> {
    let tenant = profile_record.tenant_id;
    let profile = profile_record.id;
    ensure_tenant(auth, tenant)?;
    let metadata = metadata_store_cloned(state)?;
    let chat = metadata
        .get_ai_chat(proposal.proposal.chat_id, auth.principal_id)
        .await?;
    if chat.visibility == sift_protocol::AiVisibility::RoomPublic {
        let room = room_id(chat.room_id.ok_or(ApiError::Unauthorized)?)?;
        ensure_room_permission(&metadata, auth, room, RoomPermission::Write)?;
        let grant = metadata
            .ai_room_publication(room, auth.principal_id)
            .await?
            .filter(|grant| {
                Some(grant.id) == proposal.publication_id
                    && grant.source.scope_digest == proposal.source_digest
                    && (!draft_rows(&proposal.draft) || grant.allow_rows)
            })
            .ok_or_else(|| {
                ApiError::Forbidden(
                    "Room database publication changed; stage a fresh proposal".into(),
                )
            })?;
        if grant.source.profile_id != profile.0 {
            return Err(ApiError::Forbidden(
                "Published database profile changed".into(),
            ));
        }
        let scope =
            room_submitter_scope(&metadata, auth.principal_id, room, tenant, &profile_record)?;
        crate::authorization::authorize(&scope, operation)
            .map_err(|denial| ApiError::Forbidden(denial.public_reason().into()))?;
    } else if chat.owner_principal_id != auth.principal_id.0
        || metadata
            .ai_profile_source_digest(profile, auth.principal_id)
            .await?
            != proposal.source_digest
    {
        return Err(ApiError::Forbidden(
            "Private proposal source changed; stage a fresh proposal".into(),
        ));
    }
    if chat.visibility != sift_protocol::AiVisibility::RoomPublic {
        let role = auth
            .tenants
            .iter()
            .find(|membership| membership.tenant.id == tenant)
            .map(|membership| sift_protocol::TenantRole::from(&membership.role))
            .ok_or_else(|| ApiError::Forbidden("Tenant membership is required".into()))?;
        let scope = crate::authorization::AuthorizationScope {
            authenticated: true,
            trusted_local: auth.trusted_local,
            instance_admin: false,
            tenant_role: Some(role),
            room_role: None,
            connection_policy: Some(profile_record.policy.clone()),
        };
        crate::authorization::authorize(&scope, operation)
            .map_err(|denial| ApiError::Forbidden(denial.public_reason().into()))?;
    }
    Ok(profile_record)
}
fn production(profile: &sift_metadata::ConnectionProfile) -> bool {
    profile.tags.iter().any(|tag| {
        tag.eq_ignore_ascii_case("prod")
            || tag.eq_ignore_ascii_case("production")
            || tag.eq_ignore_ascii_case("environment:production")
            || tag.eq_ignore_ascii_case("environment:prod")
    })
}
fn database_label(profile: &sift_metadata::ConnectionProfile) -> String {
    format!(
        "{} / {}",
        profile.name,
        connection_database_target(profile).unwrap_or_else(|| "default database".into())
    )
}

pub(super) fn captured_review_caller(
    state: &AppState,
    auth: &AuthContext,
    headers: &HeaderMap,
) -> ApiResult<sift_protocol::handshake::ExtensionCallerIdentity> {
    let caller = super::extension_scope::receive(state, headers)?;
    if caller.principal_id != auth.principal_id.0 || caller.auth_session_id != auth.auth_session_id
    {
        return Err(ApiError::Forbidden(
            "Review account changed; review again".into(),
        ));
    }
    Ok(caller)
}

fn check_managed_review_caller(
    state: &mut AppState,
    auth: &AuthContext,
    headers: &HeaderMap,
    proposal: Uuid,
    session: sift_protocol::SessionId,
) -> ApiResult<Option<Arc<crate::session::managed_ai_reviews::ReviewSource>>> {
    if let Some(source) = state.sessions.managed_review_source(session)? {
        source.check_caller(&captured_review_caller(state, auth, headers)?)?;
        if source.request.proposal != proposal {
            return Err(ApiError::Forbidden(
                "Automatic review source belongs to another proposal".into(),
            ));
        }
        state.sessions = state.sessions.with_managed_review_source(source.clone());
        return Ok(Some(source));
    }
    Ok(None)
}

/// Losing an HTTP observer cannot cancel source setup or create a new attempt.
pub(super) async fn managed_review_ai_database_change(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
    Json(request): Json<sift_protocol::ManagedAiDatabaseReviewRequest>,
) -> ApiResult<Json<AiDatabaseProposalReview>> {
    ai_enabled(&state)?;
    if state.shutdown.is_draining() {
        return Err(ApiError::ServiceDraining);
    }
    let auth = resolve_auth_context_blocking(state.clone(), headers.clone()).await?;
    let caller = captured_review_caller(&state, &auth, &headers)?;
    let id = ai_chat_id(&raw)?;
    check_proposal_scope(&state, &auth, id).await?;
    let metadata = metadata_store_cloned(&state)?;
    let proposal = metadata.ai_database_proposal(id, auth.principal_id).await?;
    if request.client_request_id.is_nil()
        || proposal.proposal.status != sift_protocol::AiProposalStatus::Staged
        || proposal.apply_state.is_some()
    {
        return Err(ApiError::BadRequest(
            "Proposal is unavailable for a fresh review".into(),
        ));
    }
    let tenant = tenant_id(
        proposal
            .proposal
            .target
            .tenant_id
            .ok_or(ApiError::Unauthorized)?,
    )?;
    let profile = connection_profile_id(
        proposal
            .proposal
            .target
            .profile_id
            .ok_or(ApiError::Unauthorized)?,
    )?;
    ensure_tenant(&auth, tenant)?;
    let sync = metadata.clone();
    let profile = metadata_blocking(move || {
        sync.get_connection_profile(tenant, profile)
            .map_err(Into::into)
    })
    .await?;
    let operation = if draft_rows(&proposal.draft) {
        sift_protocol::OperationKind::PreviewEdits
    } else {
        sift_protocol::OperationKind::PreviewMigration
    };
    let profile = review_profile_binding(&state, &auth, &proposal, profile, operation).await?;
    let source = state.sessions.reserve_managed_review(
        crate::session::managed_ai_reviews::ReviewRequest {
            caller,
            proposal: id,
            request: request.client_request_id,
        },
    )?;
    let chat = proposal.proposal.chat_id;
    let actor = auth.principal_id;
    let permit = state.sessions.reserve_query_settlement()?;
    let mut audit = query_audit::QueryOperationAudit::new(
        state.sessions.clone(),
        Operation::Ai {
            action: "managed_review_database_proposal".into(),
            chat_id: Some(chat),
            run_id: Some(proposal.proposal.run_id),
        },
        Some(actor.0),
    );
    let guard = crate::shutdown::current_query_guard()
        .unwrap_or_else(|| Arc::new(state.shutdown.track_query()));
    let mut worker = tokio::spawn(crate::shutdown::scope_query(Some(guard), async move {
        let _permit = permit;
        let result = async {
            let mut state = state;
            let _preparation = source.preparation.lock().await;
            source.check_caller(&captured_review_caller(&state, &auth, &headers)?)?;
            if let Some(review) = source.prepared() {
                return metadata
                    .ai_database_review(review, actor)
                    .await
                    .map_err(Into::into);
            }
            source.start()?;
            if !metadata
                .claim_ai_managed_review_attempt(id, actor, request.client_request_id)
                .await?
            {
                return Err(ApiError::BadRequest(
                    "Original setup attempt has no retained source; request a fresh review".into(),
                ));
            }
            let session = create_session(
                State(state.clone()),
                headers.clone(),
                Some(Json(OpenSessionRequest {
                    tag: Some("Sift AI database review".into()),
                    tenant_id: Some(tenant.0),
                })),
            )
            .await?
            .0
            .id;
            source.attach_session(&state.sessions, session)?;
            state.sessions = state.sessions.with_managed_review_source(source.clone());
            // Repeat source/publication/policy preflight immediately before
            // native connect, after any queued setup or metadata contention.
            let current_proposal = metadata.ai_database_proposal(id, actor).await?;
            if current_proposal.proposal.status != sift_protocol::AiProposalStatus::Staged
                || current_proposal.apply_state.is_some()
            {
                return Err(ApiError::BadRequest(
                    "Proposal was consumed during setup".into(),
                ));
            }
            let sync = metadata.clone();
            let profile_id = profile.id;
            let current_profile = metadata_blocking(move || {
                sync.get_connection_profile(tenant, profile_id)
                    .map_err(Into::into)
            })
            .await?;
            review_profile_binding(&state, &auth, &current_proposal, current_profile, operation)
                .await?;
            let connection = open_connection_from_profile(
                State(state.clone()),
                headers.clone(),
                Path(session),
                Json(OpenConnectionFromProfileRequest {
                    tenant_id: tenant.0,
                    profile_id: profile.id.0,
                }),
            )
            .await?
            .0
            .id;
            source.attach_connection(&state.sessions, connection)?;
            let review = review_ai_database_change(
                State(state),
                headers,
                Path(id.to_string()),
                Json(sift_protocol::ReviewAiDatabaseProposalRequest {
                    client_request_id: request.client_request_id,
                    session,
                    connection,
                }),
            )
            .await?
            .0;
            source.finish(&review)?;
            Ok(review)
        }
        .await;
        match &result {
            Ok(_) => audit.record(OperationStatus::Succeeded, None, None, None),
            Err(error) => audit.failure(error),
        }
        result
    }));
    let result = match tokio::time::timeout(std::time::Duration::from_secs(30), &mut worker).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err(ApiError::Internal(
            "Automatic review owner failed; inspect before requesting a fresh review".into(),
        )),
        Err(_) => Err(ApiError::BadRequest(
            "Review preparation remains pending; observe the same request before retrying".into(),
        )),
    };
    Ok(Json(result?))
}

pub(super) async fn review_ai_database_change(
    State(mut state): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
    Json(request): Json<sift_protocol::ReviewAiDatabaseProposalRequest>,
) -> ApiResult<Json<AiDatabaseProposalReview>> {
    ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers.clone()).await?;
    let id = ai_chat_id(&raw)?;
    let mut chat = None;
    let result: ApiResult<_> = async {
        chat = Some(check_proposal_scope(&state, &auth, id).await?);
        let metadata = metadata_store_cloned(&state)?;
        let proposal = metadata.ai_database_proposal(id, auth.principal_id).await?;
        if proposal.proposal.status != sift_protocol::AiProposalStatus::Staged
            || proposal.apply_state.is_some()
        {
            return Err(ApiError::BadRequest(
                "Proposal was discarded or already consumed".into(),
            ));
        }
        if let Some(source) =
            check_managed_review_caller(&mut state, &auth, &headers, id, request.session)?
        {
            if source.request.request != request.client_request_id {
                return Err(ApiError::BadRequest(
                    "Automatic source requires its original review request".into(),
                ));
            }
        }
        let operation = if draft_rows(&proposal.draft) {
            sift_protocol::OperationKind::PreviewEdits
        } else {
            sift_protocol::OperationKind::PreviewMigration
        };
        let profile = review_binding(
            &state,
            &auth,
            &proposal,
            request.session,
            request.connection,
            operation,
        )
        .await?;
        let source = state.sessions.authorize_connection_operation(
            request.session,
            request.connection,
            operation,
            None,
            &[],
        )?;
        let source_generation = source.execution.review_id;
        state.sessions =
            state
                .sessions
                .bind_source(request.session, request.connection, source.clone())?;
        if let Some(prior) = metadata
            .prior_ai_database_review(id, auth.principal_id, request.client_request_id)
            .await?
        {
            if prior.session != request.session
                || prior.connection != request.connection
                || prior.source_generation != Some(source_generation)
                || prior.expires_at <= Utc::now()
            {
                return Err(ApiError::BadRequest(
                    "Review request changed or expired; request a fresh preview".into(),
                ));
            }
            return Ok(prior);
        }
        let live = state
            .sessions
            .catalog_graph(
                request.session,
                request.connection,
                sift_protocol::CatalogGraphRequest {
                    options: ai_catalog_options(),
                    refresh: true,
                },
            )
            .await?;
        if live.database_identity != proposal.database_identity
            || live.revision != draft_revision(&proposal.draft)
        {
            return Err(ApiError::BadRequest(
                "Database schema changed; stage a fresh proposal".into(),
            ));
        }
        let preview =
            match &proposal.draft {
                AiDatabaseDraft::RowEditSet { edit_set, .. } => AiDatabasePreview::RowEditSet {
                    plan: state
                        .sessions
                        .preview_edits(request.session, request.connection, edit_set.clone())
                        .await?,
                },
                AiDatabaseDraft::MigrationDraft {
                    desired_catalog,
                    options,
                    ..
                } => {
                    let from = sift_protocol::CatalogSourceRef::Live {
                        expected_revision: live.revision,
                        options: ai_catalog_options(),
                    };
                    let to = sift_protocol::CatalogSourceRef::AiProposal {
                        proposal_id: id,
                        content_sha256: proposal.proposal.content_sha256.clone(),
                    };
                    let diff = sift_core::schema_diff::diff_catalogs(
                        from,
                        &live,
                        to,
                        desired_catalog,
                        &[],
                        Some(1000),
                    )
                    .map_err(|_| {
                        ApiError::BadRequest("Desired schema cannot be compared safely".into())
                    })?;
                    let engine = state
                        .sessions
                        .conn_entry(request.session, request.connection)?
                        .driver
                        .engine();
                    let (plan, review_evidence) = crate::migration::render_reviewed_plan(
                        engine,
                        &diff,
                        &live,
                        desired_catalog,
                        &[],
                        live.revision,
                        options,
                    )
                    .map_err(|error| ApiError::BadRequest(error.to_string()))?;
                    crate::migration::validate_ai_fragments(engine, &diff.changes)
                        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
                    if plan
                        .groups
                        .iter()
                        .map(|group| group.statements.len())
                        .sum::<usize>()
                        > 100
                    {
                        return Err(ApiError::BadRequest(
                            "AI migration preview exceeds 100 statements".into(),
                        ));
                    }
                    let (_, tenant, profile, policy_revision) = state
                        .sessions
                        .managed_catalog_scope(request.session, request.connection, operation)?;
                    let plan = state.sessions.store_migration_plan(
                        plan,
                        crate::session::MigrationPlanScope {
                            review_evidence: Some(review_evidence),
                            ai_proposal_id: Some(id),
                            session: request.session,
                            connection: request.connection,
                            principal: auth.principal_id,
                            tenant,
                            profile,
                            policy_revision,
                            live_options: ai_catalog_options(),
                        },
                    )?;
                    AiDatabasePreview::MigrationDraft { plan }
                }
            };
        review_binding(
            &state,
            &auth,
            &proposal,
            request.session,
            request.connection,
            operation,
        )
        .await?;
        state
            .sessions
            .ensure_captured_connection(request.session, request.connection, &source)?;
        metadata
            .save_ai_database_review(
                auth.principal_id,
                request.client_request_id,
                AiDatabaseProposalReview {
                    id: Uuid::new_v4(),
                    proposal_id: id,
                    reviewer_id: auth.principal_id.0,
                    session: request.session,
                    connection: request.connection,
                    source_generation: Some(source_generation),
                    source_digest: proposal.source_digest,
                    content_sha256: proposal.proposal.content_sha256,
                    review_digest: String::new(),
                    database_label: database_label(&profile),
                    production: production(&profile),
                    preview,
                    expires_at: Utc::now() + chrono::Duration::minutes(5),
                },
            )
            .await
            .map_err(Into::into)
    }
    .await;
    audit_change(
        &state,
        auth.principal_id,
        "review_database_proposal",
        chat,
        None,
        result.is_ok(),
    );
    Ok(Json(result?))
}

fn pending_receipt(
    id: Uuid,
    state: sift_protocol::AiDatabaseApplyState,
) -> sift_protocol::AiDatabaseApplyReceipt {
    sift_protocol::AiDatabaseApplyReceipt {
        proposal_id: id,
        state,
        row_result: None,
        migration_result: None,
        message: if state == sift_protocol::AiDatabaseApplyState::Applying {
            "Application is in progress; database work will not be repeated"
        } else {
            "Application outcome is unknown; check the database before creating a new proposal"
        }
        .into(),
    }
}
fn observed_claim(
    id: Uuid,
    claim: sift_metadata::AiDatabaseApplyClaim,
) -> ApiResult<sift_protocol::AiDatabaseApplyReceipt> {
    match claim {
        sift_metadata::AiDatabaseApplyClaim::Replay(receipt) => Ok(*receipt),
        sift_metadata::AiDatabaseApplyClaim::Pending(state) => Ok(pending_receipt(id, state)),
        sift_metadata::AiDatabaseApplyClaim::Claimed => {
            Err(ApiError::Internal("unexpected new apply claim".into()))
        }
    }
}
fn exact_plan_digest(plan: &sift_protocol::EditPlan) -> ApiResult<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(plan)
                .map_err(|_| ApiError::Internal("cannot serialize row preview".into()))?
        )
    ))
}
pub(super) async fn apply_ai_database_change(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
    Json(request): Json<sift_protocol::ApplyAiDatabaseProposalRequest>,
) -> ApiResult<Json<sift_protocol::AiDatabaseApplyReceipt>> {
    apply_ai_database_change_with_execution(State(state), headers, Path(raw), Json(request), None)
        .await
}
pub(super) async fn apply_ai_database_change_with_execution(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
    Json(request): Json<sift_protocol::ApplyAiDatabaseProposalRequest>,
    execution: Option<(Uuid, String)>,
) -> ApiResult<Json<sift_protocol::AiDatabaseApplyReceipt>> {
    ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers.clone()).await?;
    let id = ai_chat_id(&raw)?;
    let correlation = crate::correlation::current().unwrap_or_else(crate::correlation::generate);
    let mut audit = crate::correlation::sync_scope(correlation.clone(), || {
        query_audit::QueryOperationAudit::new(
            state.sessions.clone(),
            Operation::Ai {
                action: if execution.is_some() {
                    "apply_database_execution"
                } else {
                    "apply_database_proposal"
                }
                .into(),
                chat_id: None,
                run_id: None,
            },
            Some(auth.principal_id.0),
        )
    });
    let prepared = (|| {
        if state.shutdown.is_draining() {
            return Err(ApiError::ServiceDraining);
        }
        state.sessions.reserve_query_settlement()
    })();
    let permit = match prepared {
        Ok(permit) => permit,
        Err(error) => {
            audit.failure(&error);
            return Err(error);
        }
    };
    let guard = crate::shutdown::current_query_guard()
        .unwrap_or_else(|| std::sync::Arc::new(state.shutdown.track_query()));
    let worker_state = state.clone();
    let worker_auth = auth.clone();
    let worker_headers = headers.clone();
    let mut timeout = std::time::Duration::from_secs(u64::from(state.auth.ai.max_run_secs));
    let (typed_guard, work) = if let Some((id, fp)) = execution.clone() {
        let metadata = metadata_store_cloned(&state)?;
        let actor = auth.principal_id;
        let (run, expires) = metadata.ai_database_execution_lifetime(id, actor)?;
        timeout = timeout.min((expires - Utc::now()).to_std().unwrap_or_default());
        let work = state.sessions.register_ai_work(run)?;
        (
            Some(Arc::new(move || {
                metadata
                    .check_ai_database_execution_admission(id, actor, &fp)
                    .map_err(Into::into)
            })
                as Arc<dyn Fn() -> ApiResult<()> + Send + Sync>),
            Some(work),
        )
    } else {
        (None, None)
    };
    let cancellation = work.as_ref().map(|work| work.token.clone());
    let admission = if typed_guard.is_some() {
        crate::session::native_mutation::Admission::with_guard(
            timeout,
            typed_guard,
            cancellation.clone(),
        )
    } else {
        crate::session::native_mutation::Admission::new(timeout)
    };
    let timer = cancellation.map(|token| {
        let deadline = admission.deadline();
        tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            tokio::time::sleep_until(deadline).await;
            token.cancel();
        }))
    });
    let worker_admission = admission.clone();
    let claimed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_claimed = claimed.clone();
    let mut task = tokio::spawn(crate::correlation::scope(
        correlation,
        crate::shutdown::scope_query(
            Some(guard),
            crate::session::native_mutation::scope(worker_admission, async move {
                let _permit = permit;
                let _work = work;
                let _timer = timer;
                let result = apply_owned(
                    worker_state,
                    worker_auth,
                    id,
                    request,
                    worker_headers,
                    &mut audit,
                    ApplyDispatchClaim {
                        claimed: worker_claimed,
                        execution,
                    },
                )
                .await;
                match &result {
                    Ok(receipt)
                        if receipt.state == sift_protocol::AiDatabaseApplyState::Applied =>
                    {
                        audit.record(OperationStatus::Succeeded, None, None, None);
                    }
                    Ok(receipt) => {
                        let code = match receipt.state {
                            sift_protocol::AiDatabaseApplyState::Applying => "ai_apply_in_progress",
                            sift_protocol::AiDatabaseApplyState::Failed => "ai_apply_failed",
                            _ => "ai_apply_outcome_unknown",
                        };
                        audit.record(OperationStatus::Failed, Some(code.into()), None,
                        Some("AI application did not produce a confirmed applied receipt; inspect its durable outcome".into()));
                    }
                    Err(error) => audit.failure(error),
                }
                result
            }),
        ),
    ));
    let receipt = match tokio::time::timeout_at(admission.deadline(), &mut task).await {
        Ok(result) => result.map_err(|_| {
            ApiError::Internal(
                "AI application settlement owner failed; inspect its durable outcome".into(),
            )
        })??,
        Err(_) => {
            admission.close();
            if claimed.load(std::sync::atomic::Ordering::Acquire) {
                pending_receipt(id, sift_protocol::AiDatabaseApplyState::Applying)
            } else {
                return Err(ApiError::Driver(sift_protocol::DriverError::new(
                    sift_protocol::Code::QueryTimedOut,
                    "AI application observer timed out; inspect its durable receipt before retrying",
                )));
            }
        }
    };

    // Delivery is separately authorized. Losing access after native settlement
    // must not rewrite the known outcome or emit a second terminal audit.
    let current = resolve_auth_context_blocking(state.clone(), headers).await?;
    if current.principal_id != auth.principal_id {
        return Err(ApiError::Unauthorized);
    }
    let chat = check_proposal_scope(&state, &current, id).await?;
    metadata_store_cloned(&state)?
        .get_ai_chat(chat, current.principal_id)
        .await?;
    Ok(Json(receipt))
}

struct ApplyDispatchClaim {
    claimed: Arc<std::sync::atomic::AtomicBool>,
    execution: Option<(Uuid, String)>,
}

async fn apply_owned(
    mut state: AppState,
    auth: AuthContext,
    id: Uuid,
    request: sift_protocol::ApplyAiDatabaseProposalRequest,
    replay_headers: HeaderMap,
    audit: &mut query_audit::QueryOperationAudit,
    claim: ApplyDispatchClaim,
) -> ApiResult<sift_protocol::AiDatabaseApplyReceipt> {
    let ApplyDispatchClaim { claimed, execution } = claim;
    let replay_request = request.clone();
    let mut chat = None;
    let mut result: ApiResult<_> = async {
        let chat_id = check_proposal_scope(&state, &auth, id).await?;
        chat = Some(chat_id);
        audit.bind_ai_context(chat_id, None);
        // Identical approvals must not run native catalog/edit previews on the
        // same handle concurrently. Serialize only preflight through claim;
        // the durable claim remains authoritative for replay and native dispatch.
        let preflight_gate = state.sessions.ai_apply_preflight_gate(id);
        let preflight = preflight_gate.lock().await;
        let metadata = metadata_store_cloned(&state)?;
        if let Some(prior) = metadata
            .prior_ai_database_apply(id, auth.principal_id, request.clone())
            .await?
        {
            return observed_claim(id, prior);
        }
        let proposal = metadata.ai_database_proposal(id, auth.principal_id).await?;
        audit.bind_ai_context(chat_id, Some(proposal.proposal.run_id));
        let review = metadata
            .ai_database_review(request.review_id, auth.principal_id)
            .await?;
        if proposal.proposal.status != sift_protocol::AiProposalStatus::Staged
            || review.proposal_id != id
            || review.review_digest != request.review_digest
            || review.content_sha256 != proposal.proposal.content_sha256
            || review.source_digest != proposal.source_digest
            || review.expires_at <= Utc::now()
        {
            return Err(ApiError::BadRequest(
                "AI preview changed or expired; request a fresh review".into(),
            ));
        }
        if let Some(source) =
            check_managed_review_caller(&mut state, &auth, &replay_headers, id, review.session)?
        {
            if source.prepared() != Some(review.id) {
                return Err(ApiError::BadRequest(
                    "Automatic source belongs to another prepared review".into(),
                ));
            }
        }
        let typed_source = if let Some((action, fp)) = &execution {
            let detail = metadata
                .ai_database_execution_detail(*action, auth.principal_id)
                .await?;
            if detail.summary.fingerprint != *fp
                || detail.summary.proposal_id != id
                || detail.review.id != review.id
            {
                return Err(ApiError::BadRequest(
                    "Typed execution approval changed".into(),
                ));
            }
            let operation = if draft_rows(&proposal.draft) {
                sift_protocol::OperationKind::ApplyEdits
            } else {
                sift_protocol::OperationKind::ApplyMigration
            };
            Some(super::ai_execution::source_current_for(
                &state.sessions,
                &metadata,
                auth.principal_id,
                &detail.source,
                operation,
                None,
                true,
            )?)
        } else {
            None
        };
        let operation = if draft_rows(&proposal.draft) {
            sift_protocol::OperationKind::ApplyEdits
        } else {
            sift_protocol::OperationKind::ApplyMigration
        };
        let source = if let Some(source) = typed_source {
            state.sessions.ensure_captured_connection(
                review.session,
                review.connection,
                &source,
            )?;
            source
        } else {
            state.sessions.authorize_connection_operation(
                review.session,
                review.connection,
                operation,
                None,
                &[],
            )?
        };
        if review.source_generation != Some(source.execution.review_id) {
            return Err(ApiError::BadRequest(
                "AI review source changed or is no longer current; request a fresh review".into(),
            ));
        }
        state.sessions = state
            .sessions
            .bind_source(review.session, review.connection, source)?;
        let profile = review_binding(
            &state,
            &auth,
            &proposal,
            review.session,
            review.connection,
            operation,
        )
        .await?;
        if database_label(&profile) != review.database_label
            || production(&profile) != review.production
        {
            return Err(ApiError::BadRequest(
                "Database label or production status changed; stage a fresh proposal".into(),
            ));
        }
        if review.production
            && request.production_confirmation.as_deref() != Some(review.database_label.as_str())
        {
            return Err(ApiError::BadRequest(
                "Type the reviewed database label to confirm production changes".into(),
            ));
        }
        let live = state
            .sessions
            .catalog_graph(
                review.session,
                review.connection,
                sift_protocol::CatalogGraphRequest {
                    options: ai_catalog_options(),
                    refresh: true,
                },
            )
            .await?;
        if live.revision != draft_revision(&proposal.draft)
            || live.database_identity != proposal.database_identity
        {
            return Err(ApiError::BadRequest(
                "Database schema changed; stage a fresh proposal".into(),
            ));
        }
        match (&proposal.draft, &review.preview) {
            (
                AiDatabaseDraft::RowEditSet { edit_set, .. },
                AiDatabasePreview::RowEditSet { plan },
            ) => {
                let current = state
                    .sessions
                    .preview_edits(review.session, review.connection, edit_set.clone())
                    .await?;
                if exact_plan_digest(&current)? != exact_plan_digest(plan)? {
                    return Err(ApiError::BadRequest(
                        "Row preview changed; request a fresh review".into(),
                    ));
                }
            }
            (
                AiDatabaseDraft::MigrationDraft { .. },
                AiDatabasePreview::MigrationDraft { plan },
            ) => {
                let retained = state.sessions.migration_review_material(
                    review.session,
                    review.connection,
                    auth.principal_id,
                    plan.id,
                    Some(id),
                )?;
                if retained.changes.is_empty()
                    || sift_core::ai_delegation::canonical_digest(&retained.plan).map_err(|_| {
                        ApiError::Internal("Invalid retained migration review".into())
                    })? != sift_core::ai_delegation::canonical_digest(plan)
                        .map_err(|_| ApiError::Internal("Invalid migration review".into()))?
                {
                    return Err(ApiError::BadRequest(
                        "Migration preview is no longer available; request a fresh review".into(),
                    ));
                }
                if plan
                    .required_acknowledgements
                    .iter()
                    .any(|risk| !request.acknowledgements.contains(risk))
                {
                    return Err(ApiError::BadRequest(
                        "Review and acknowledge each migration risk before applying".into(),
                    ));
                }
            }
            _ => {
                return Err(ApiError::BadRequest(
                    "AI preview kind does not match its proposal".into(),
                ))
            }
        }
        review_binding(
            &state,
            &auth,
            &proposal,
            review.session,
            review.connection,
            operation,
        )
        .await?;
        if let Some(admission) = crate::session::native_mutation::current() {
            admission.check()?;
        }
        match metadata
            .claim_ai_database_apply_with_execution(
                id,
                auth.principal_id,
                request.clone(),
                execution.clone(),
            )
            .await?
        {
            sift_metadata::AiDatabaseApplyClaim::Claimed => {
                claimed.store(true, std::sync::atomic::Ordering::Release);
            }
            prior => return observed_claim(id, prior),
        }
        drop(preflight);
        // The outer owner already retains audit, receipt and native shutdown
        // lifetime. Await directly so task-local source/correlation/guard scope
        // reaches all guarded native work and its independent EOF owners.
        complete_claimed_apply(
            state.clone(),
            auth.clone(),
            proposal,
            review,
            request,
            profile,
        )
        .await
    }
    .await;
    // An identical concurrent request may claim the proposal after the initial
    // lookup, changing transaction/schema state during this request's preflight.
    // Recover only its exact durable claim; never admit new database work here.
    if result.is_err() && chat.is_some() {
        let prior: ApiResult<Option<sift_metadata::AiDatabaseApplyClaim>> = async {
            let current = resolve_auth_context_blocking(state.clone(), replay_headers).await?;
            if current.principal_id != auth.principal_id {
                return Err(ApiError::Unauthorized);
            }
            check_proposal_scope(&state, &current, id).await?;
            metadata_store_cloned(&state)?
                .prior_ai_database_apply(id, current.principal_id, replay_request)
                .await
                .map_err(Into::into)
        }
        .await;
        if let Ok(Some(prior)) = prior {
            result = observed_claim(id, prior);
        }
    }
    result
}

async fn complete_claimed_apply(
    state: AppState,
    auth: AuthContext,
    proposal: AiDatabaseProposalDetail,
    review: AiDatabaseProposalReview,
    request: sift_protocol::ApplyAiDatabaseProposalRequest,
    profile: sift_metadata::ConnectionProfile,
) -> ApiResult<sift_protocol::AiDatabaseApplyReceipt> {
    use sift_protocol::{AiDatabaseApplyReceipt, AiDatabaseApplyState, MigrationRunState};
    let id = proposal.proposal.id;
    let operation = if draft_rows(&proposal.draft) {
        sift_protocol::OperationKind::ApplyEdits
    } else {
        sift_protocol::OperationKind::ApplyMigration
    };
    let mut receipt = AiDatabaseApplyReceipt {
        proposal_id: id,
        state: AiDatabaseApplyState::Failed,
        row_result: None,
        migration_result: None,
        message: "Database changes were not dispatched; review a fresh proposal before applying"
            .into(),
    };
    let mut result_code = None;
    let row_observation = Arc::new(std::sync::Mutex::new(
        crate::session::EditsExecutionObservation::default(),
    ));
    if crate::session::native_mutation::current()
        .map_or(true, |admission| admission.check().is_ok())
        && review_binding(
            &state,
            &auth,
            &proposal,
            review.session,
            review.connection,
            operation,
        )
        .await
        .is_ok()
    {
        let guard_state = state.clone();
        let guard_auth = auth.clone();
        let guard_proposal = std::sync::Arc::new(proposal.clone());
        let guard_session = review.session;
        let guard_connection = review.connection;
        let dispatch_guard: crate::session::DatabaseDispatchGuard =
            std::sync::Arc::new(move || {
                let state = guard_state.clone();
                let auth = guard_auth.clone();
                let proposal = guard_proposal.clone();
                Box::pin(async move {
                    if let Some(admission) = crate::session::native_mutation::current() {
                        admission.check()?;
                    }
                    review_binding_checked(
                        &state,
                        &auth,
                        &proposal,
                        guard_session,
                        guard_connection,
                        operation,
                        false,
                    )
                    .await
                    .map(|_| ())
                })
            });
        let apply = async {
            match (&proposal.draft, &review.preview) {
                (
                    AiDatabaseDraft::RowEditSet { edit_set, .. },
                    AiDatabasePreview::RowEditSet { plan },
                ) => {
                    let result = state
                        .sessions
                        .apply_edits_guarded_observed(
                            review.session,
                            sift_protocol::ApplyEditsRequest {
                                connection: review.connection,
                                edit_set: edit_set.clone(),
                                tx: None,
                            },
                            Some(dispatch_guard.clone()),
                            Some(plan.clone()),
                            row_observation.clone(),
                        )
                        .await?;
                    Ok::<_, ApiError>((Some(result), None))
                }
                (
                    AiDatabaseDraft::MigrationDraft { .. },
                    AiDatabasePreview::MigrationDraft { plan },
                ) => {
                    let result = state
                        .sessions
                        .apply_migration_guarded(
                            review.session,
                            review.connection,
                            auth.principal_id,
                            sift_protocol::ApplyMigrationRequest {
                                plan_id: plan.id,
                                plan_digest: plan.digest.clone(),
                                acknowledgements: request.acknowledgements.clone(),
                                source: None,
                            },
                            Some(dispatch_guard.clone()),
                        )
                        .await?;
                    Ok((None, Some(result)))
                }
                _ => Err(ApiError::BadRequest("AI draft kind changed".into())),
            }
        };
        match apply.await {
            Ok((rows, migration)) => {
                receipt.state = if rows.as_ref().is_some_and(|result| result.committed)
                    || migration
                        .as_ref()
                        .is_some_and(|run| run.state == MigrationRunState::Applied)
                {
                    AiDatabaseApplyState::Applied
                } else if migration.as_ref().is_some_and(|run| {
                    matches!(
                        run.state,
                        MigrationRunState::Failed | MigrationRunState::RolledBack
                    )
                }) {
                    AiDatabaseApplyState::Failed
                } else {
                    AiDatabaseApplyState::OutcomeUnknown
                };
                receipt.row_result = rows;
                receipt.migration_result = migration;
                receipt.message=match receipt.state {AiDatabaseApplyState::Applied=>"Applied the reviewed database changes",AiDatabaseApplyState::Failed=>"Database changes failed or rolled back; create a fresh proposal after checking the outcome",_=>"Database outcome is uncertain; check the database before creating a new proposal"}.into();
            }
            Err(error) => {
                result_code = match &error {
                    ApiError::Driver(error) => Some(crate::error::audit_code(&error.code)),
                    _ => Some("ai_apply_failed".into()),
                };
                receipt.state = AiDatabaseApplyState::OutcomeUnknown;
                receipt.message="Database outcome is uncertain; check the database before creating a new proposal".into();
            }
        }
    }
    if crate::session::native_mutation::current().is_some_and(|admission| !admission.was_admitted())
    {
        receipt.state = AiDatabaseApplyState::Failed;
        receipt.message =
            "Database changes were not dispatched; review a fresh proposal before applying".into();
    }
    let migration_observation = match &review.preview {
        AiDatabasePreview::MigrationDraft { plan } => {
            match crate::session::migration_journal::ledger(
                metadata_store_cloned(&state)?,
                profile.tenant_id,
                plan.run_id,
            )
            .await
            {
                Ok(native) => {
                    use crate::session::migration_journal::Settlement;
                    match native.settlement {
                        Settlement::Committed => {
                            let workflow_applied = receipt
                                .migration_result
                                .as_ref()
                                .is_some_and(|run| run.state == MigrationRunState::Applied);
                            receipt.state = AiDatabaseApplyState::Applied;
                            receipt.message = if workflow_applied {
                                "Applied the reviewed database changes"
                            } else {
                                "The reviewed changes committed. The workflow receipt reports a delivery or persistence error; inspect the migration before retrying"
                            }.into();
                        }
                        Settlement::RolledBack => {
                            receipt.state = AiDatabaseApplyState::Failed;
                            receipt.message = "The owned migration transaction was confirmed rolled back; review a fresh proposal before another apply".into();
                        }
                        Settlement::NotDispatched => {
                            receipt.state = AiDatabaseApplyState::Failed;
                            receipt.message = "Migration statements were not dispatched; inspect its transaction state before another apply".into();
                        }
                        Settlement::Unknown => {
                            receipt.state = AiDatabaseApplyState::OutcomeUnknown;
                            receipt.message = "Migration changes are partial or their outcome is uncertain; inspect the database before another apply".into();
                        }
                        Settlement::Unobserved => {}
                    }
                    Some(native)
                }
                Err(_) => {
                    tracing::error!(proposal_id=%id, "AI migration native observation read failed");
                    None
                }
            }
        }
        _ => None,
    };
    if draft_rows(&proposal.draft) && row_observation.lock().unwrap().rollback == Some(true) {
        receipt.state = AiDatabaseApplyState::Failed;
        receipt.message = "The owned row-edit transaction was confirmed rolled back; review a fresh proposal before another apply".into();
    }
    // Result values belong in a public receipt only while their publication
    // and caller access remain current. Outcome counts/fingerprints persist.
    if proposal.publication_id.is_some()
        && review_binding(
            &state,
            &auth,
            &proposal,
            review.session,
            review.connection,
            operation,
        )
        .await
        .is_err()
    {
        if let Some(rows) = &mut receipt.row_result {
            for outcome in &mut rows.applied {
                outcome.returned.clear();
            }
        }
    }
    if record_ai_change(
        &state,
        &auth,
        &proposal,
        &review,
        &profile,
        &receipt,
        AiApplyObservation {
            migration: migration_observation,
            rows: draft_rows(&proposal.draft).then(|| row_observation.lock().unwrap().clone()),
            result_code,
        },
    )
    .await
    .is_err()
    {
        tracing::error!(proposal_id=%id,"AI database change ledger persistence failed");
    }
    metadata_store_cloned(&state)?
        .finish_ai_database_apply(auth.principal_id, request, receipt)
        .await
        .map_err(Into::into)
}

async fn record_ai_change(
    state: &AppState,
    auth: &AuthContext,
    proposal: &AiDatabaseProposalDetail,
    review: &AiDatabaseProposalReview,
    profile: &sift_metadata::ConnectionProfile,
    receipt: &sift_protocol::AiDatabaseApplyReceipt,
    observation: AiApplyObservation,
) -> ApiResult<()> {
    use sift_protocol::{ChangeLedgerOperation, ChangeLedgerOutcome};
    let metadata = metadata_store_cloned(state)?;
    let native = observation.migration;
    let operations = match &native {
        Some(native) => native
            .entries
            .iter()
            .cloned()
            .map(|witness| (witness.operation, Some(witness)))
            .collect::<Vec<_>>(),
        None => observation
            .rows
            .as_ref()
            .map(|rows| rows.attempted())
            .unwrap_or_default()
            .into_iter()
            .map(|kind| {
                (
                    match kind {
                        sift_protocol::EditStatementKind::Insert => {
                            ChangeLedgerOperation::GridInsert
                        }
                        sift_protocol::EditStatementKind::Update => {
                            ChangeLedgerOperation::GridUpdate
                        }
                        sift_protocol::EditStatementKind::Delete => {
                            ChangeLedgerOperation::GridDelete
                        }
                    },
                    None,
                )
            })
            .collect(),
    };
    for (operation, witness) in operations {
        let row_count = receipt
            .row_result
            .as_ref()
            .and_then(|result| observed_row_count(result, operation));
        let input = sift_metadata::NewChangeLedgerEntry {
            tenant_id: Some(profile.tenant_id.0),
            room_id: proposal.proposal.target.room_id,
            connection_profile_id: Some(profile.id.0),
            database_target: match &native {
                Some(native) => native
                    .source
                    .as_ref()
                    .and_then(|source| source.database_target.clone()),
                None => connection_database_target(profile),
            },
            operation,
            affected_object: match &proposal.draft {
                AiDatabaseDraft::RowEditSet { edit_set, .. } => {
                    Some(object_path_label(&edit_set.table))
                }
                _ => None,
            },
            row_count: witness
                .as_ref()
                .map(|witness| witness.rows)
                .unwrap_or(row_count),
            sql_fingerprint: Some(
                witness
                    .as_ref()
                    .map(|witness| witness.fingerprint.clone())
                    .unwrap_or_else(|| review.review_digest.clone()),
            ),
            row_identity_fingerprint: None,
            transaction_id: witness
                .as_ref()
                .and_then(|witness| witness.transaction_id)
                .or_else(|| {
                    observation
                        .rows
                        .as_ref()
                        .and_then(|rows| rows.transaction_id)
                })
                .map(|tx| tx.0.to_string()),
            correlation_id: Some(format!(
                "ai:{}:{}",
                proposal.proposal.run_id, proposal.proposal.id
            )),
            workspace_id: None,
            workspace_revision: None,
            checkpoint_id: None,
            workspace_path: None,
            git_commit: None,
            source_workflow: if draft_rows(&proposal.draft) {
                "ai_row_proposal"
            } else {
                "ai_migration_proposal"
            }
            .into(),
            authored_by: Some(proposal.proposal.created_by),
            approved_by: Some(auth.principal_id.0),
            executed_by: auth.principal_id.0,
            database_actor: None,
            outcome: if let Some(witness) = &witness {
                witness.outcome
            } else if let Some(rows) = &observation.rows {
                if rows.rollback == Some(true) {
                    ChangeLedgerOutcome::RolledBack
                } else if receipt
                    .row_result
                    .as_ref()
                    .is_some_and(|result| result.committed)
                {
                    ChangeLedgerOutcome::Committed
                } else if receipt.row_result.is_some() {
                    ChangeLedgerOutcome::Partial
                } else {
                    ChangeLedgerOutcome::OutcomeUnknown
                }
            } else {
                ChangeLedgerOutcome::OutcomeUnknown
            },
            result_code: witness
                .as_ref()
                .and_then(|witness| witness.result_code.clone())
                .or_else(|| observation.result_code.clone()),
            identity_source: sift_protocol::ChangeIdentitySource::Sift,
            identity_confidence: sift_protocol::ChangeIdentityConfidence::Authenticated,
        };
        let store = metadata.clone();
        metadata_blocking(move || store.append_change_ledger(input).map_err(Into::into)).await?;
    }
    Ok(())
}

pub(super) async fn inspect_ai_database_apply(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
) -> ApiResult<Json<sift_protocol::AiDatabaseApplyStatus>> {
    ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let id = ai_chat_id(&raw)?;
    let scope = check_proposal_scope(&state, &auth, id).await;
    let chat = scope.as_ref().ok().copied();
    let result = match scope {
        Ok(_) => metadata_store_cloned(&state)?
            .ai_database_apply_status(id, auth.principal_id)
            .await
            .map_err(Into::into),
        Err(error) => Err(error),
    };
    finish_operation_as(
        &state.sessions,
        Operation::Ai {
            action: "inspect_database_apply".into(),
            chat_id: chat,
            run_id: None,
        },
        result,
        Some(auth.principal_id.0),
        |_| None,
    )
    .map(Json)
}

pub(super) async fn reconcile_ai_database_apply(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
    Json(request): Json<sift_protocol::ReconcileAiDatabaseApply>,
) -> ApiResult<Json<sift_protocol::AiDatabaseApplyStatus>> {
    ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let id = ai_chat_id(&raw)?;
    let worker = state.clone();
    super::execution::owned_audited_operation(
        state,
        Operation::Ai {
            action: "reconcile_database_apply".into(),
            chat_id: None,
            run_id: None,
        },
        Some(auth.principal_id.0),
        async move {
            check_proposal_scope(&worker, &auth, id).await?;
            metadata_store_cloned(&worker)?
                .reconcile_ai_database_apply(id, auth.principal_id, request)
                .await
                .map(Json)
                .map_err(Into::into)
        },
    )
    .await
}

#[cfg(test)]
mod observation_tests {
    use super::*;
    use sift_protocol::{
        ApplyEditsResult, ChangeLedgerOperation as Operation, EditOutcome,
        EditStatementKind as Kind,
    };

    #[test]
    fn ledger_counts_preserve_zero_and_refuse_per_kind_overflow() {
        let entry = |kind, affected_rows| EditOutcome {
            edit_index: 0,
            kind,
            affected_rows,
            returned: vec![],
        };
        let mut result = ApplyEditsResult {
            committed: true,
            applied: vec![
                entry(Kind::Insert, i64::MAX as u64),
                entry(Kind::Insert, 1),
                entry(Kind::Update, 0),
                entry(Kind::Delete, u64::MAX),
            ],
        };
        assert_eq!(observed_row_count(&result, Operation::GridInsert), None);
        assert_eq!(observed_row_count(&result, Operation::GridDelete), None);
        assert_eq!(observed_row_count(&result, Operation::GridUpdate), Some(0));
        result.applied.pop();
        result.applied.remove(1);
        assert_eq!(
            observed_row_count(&result, Operation::GridInsert),
            Some(i64::MAX)
        );
        assert_eq!(observed_row_count(&result, Operation::MigrationApply), None);
    }
}
