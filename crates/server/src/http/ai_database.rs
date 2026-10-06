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
        if run.mode != sift_protocol::AiMode::Propose
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

pub(super) async fn review_ai_database_change(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(raw): Path<String>,
    Json(request): Json<sift_protocol::ReviewAiDatabaseProposalRequest>,
) -> ApiResult<Json<AiDatabaseProposalReview>> {
    ai_enabled(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
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
        if let Some(prior) = metadata
            .prior_ai_database_review(id, auth.principal_id, request.client_request_id)
            .await?
        {
            if prior.session != request.session
                || prior.connection != request.connection
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
                    let plan = crate::migration::render_plan(
                        engine,
                        &diff,
                        &live,
                        desired_catalog,
                        &[],
                        live.revision,
                        options,
                    )
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
    ai_enabled(&state)?;
    let verify_headers = headers.clone();
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let id = ai_chat_id(&raw)?;
    let mut chat = None;
    let result: ApiResult<_> = async {
        let chat_id = check_proposal_scope(&state, &auth, id).await?;
        chat = Some(chat_id);
        let metadata = metadata_store_cloned(&state)?;
        if let Some(prior) = metadata
            .prior_ai_database_apply(id, auth.principal_id, request.clone())
            .await?
        {
            return observed_claim(id, prior);
        }
        let proposal = metadata.ai_database_proposal(id, auth.principal_id).await?;
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
        let operation = if draft_rows(&proposal.draft) {
            sift_protocol::OperationKind::ApplyEdits
        } else {
            sift_protocol::OperationKind::ApplyMigration
        };
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
                if state.sessions.migration_plan_ai_proposal(plan.id) != Some(id)
                    || plan.expires_at <= Utc::now()
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
        match metadata
            .claim_ai_database_apply(id, auth.principal_id, request.clone())
            .await?
        {
            sift_metadata::AiDatabaseApplyClaim::Claimed => {}
            prior => return observed_claim(id, prior),
        }
        // Complete receipt/ledger persistence even if the caller disconnects.
        let worker_state = state.clone();
        let worker_auth = auth.clone();
        let receipt = tokio::spawn(async move {
            complete_claimed_apply(
                worker_state,
                worker_auth,
                proposal,
                review,
                request,
                profile,
            )
            .await
        })
        .await
        .map_err(|_| {
            ApiError::Internal(
                "AI application was interrupted; database outcome must be checked".into(),
            )
        })??;
        let current = resolve_auth_context_blocking(state.clone(), verify_headers).await?;
        if current.principal_id != auth.principal_id {
            return Err(ApiError::Unauthorized);
        }
        check_ai_chat_scope(&state, &current, chat_id).await?;
        metadata.get_ai_chat(chat_id, current.principal_id).await?;
        Ok(receipt)
    }
    .await;
    audit_change(
        &state,
        auth.principal_id,
        "apply_database_proposal",
        chat,
        None,
        result
            .as_ref()
            .is_ok_and(|receipt| receipt.state == sift_protocol::AiDatabaseApplyState::Applied),
    );
    Ok(Json(result?))
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
        message: "Authorization changed before dispatch; database work was not started".into(),
    };
    let mut result_code = None;
    if review_binding(
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
                        .apply_edits_guarded(
                            review.session,
                            sift_protocol::ApplyEditsRequest {
                                connection: review.connection,
                                edit_set: edit_set.clone(),
                                tx: None,
                            },
                            Some(dispatch_guard.clone()),
                            Some(plan.clone()),
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
        match tokio::time::timeout(
            std::time::Duration::from_secs(u64::from(state.auth.ai.max_run_secs)),
            apply,
        )
        .await
        {
            Ok(Ok((rows, migration))) => {
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
            Ok(Err(error)) => {
                result_code = match &error {
                    ApiError::Driver(error) => Some(error.code.to_string()),
                    _ => Some("ai_apply_failed".into()),
                };
                receipt.state = AiDatabaseApplyState::OutcomeUnknown;
                receipt.message="Database outcome is uncertain; check the database before creating a new proposal".into();
            }
            Err(_) => {
                receipt.state = AiDatabaseApplyState::OutcomeUnknown;
                receipt.message =
                    "Application timed out; check the database before creating a new proposal"
                        .into();
                result_code = Some("ai_apply_timeout".into());
            }
        }
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
        result_code,
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
    result_code: Option<String>,
) -> ApiResult<()> {
    use sift_protocol::{AiDatabaseApplyState, ChangeLedgerOperation, ChangeLedgerOutcome};
    let metadata = metadata_store_cloned(state)?;
    let operations = match &proposal.draft {
        AiDatabaseDraft::RowEditSet { edit_set, .. } => [
            (
                ChangeLedgerOperation::GridInsert,
                edit_set
                    .edits
                    .iter()
                    .any(|edit| matches!(edit, sift_protocol::RowEdit::Insert { .. })),
            ),
            (
                ChangeLedgerOperation::GridUpdate,
                edit_set
                    .edits
                    .iter()
                    .any(|edit| matches!(edit, sift_protocol::RowEdit::Update { .. })),
            ),
            (
                ChangeLedgerOperation::GridDelete,
                edit_set
                    .edits
                    .iter()
                    .any(|edit| matches!(edit, sift_protocol::RowEdit::Delete { .. })),
            ),
        ]
        .into_iter()
        .filter_map(|(operation, present)| present.then_some(operation))
        .collect::<Vec<_>>(),
        _ => {
            let mut operations = vec![ChangeLedgerOperation::MigrationApply];
            if receipt.migration_result.as_ref().is_some_and(|run| {
                run.outcomes.iter().any(|outcome| {
                    outcome.status == sift_protocol::MigrationStatementStatus::RolledBack
                })
            }) {
                operations.push(ChangeLedgerOperation::MigrationRollback);
            }
            operations
        }
    };
    for operation in operations {
        let row_count = receipt
            .row_result
            .as_ref()
            .map(|result| {
                result
                    .applied
                    .iter()
                    .filter(|entry| {
                        matches!(
                            (operation, entry.kind),
                            (
                                ChangeLedgerOperation::GridInsert,
                                sift_protocol::EditStatementKind::Insert
                            ) | (
                                ChangeLedgerOperation::GridUpdate,
                                sift_protocol::EditStatementKind::Update
                            ) | (
                                ChangeLedgerOperation::GridDelete,
                                sift_protocol::EditStatementKind::Delete
                            )
                        )
                    })
                    .map(|entry| entry.affected_rows)
                    .sum::<u64>()
            })
            .and_then(|count| i64::try_from(count).ok());
        let input =
            sift_metadata::NewChangeLedgerEntry {
                tenant_id: Some(profile.tenant_id.0),
                room_id: proposal.proposal.target.room_id,
                connection_profile_id: Some(profile.id.0),
                database_target: connection_database_target(profile),
                operation,
                affected_object: match &proposal.draft {
                    AiDatabaseDraft::RowEditSet { edit_set, .. } => {
                        Some(object_path_label(&edit_set.table))
                    }
                    _ => None,
                },
                row_count,
                sql_fingerprint: Some(review.review_digest.clone()),
                row_identity_fingerprint: None,
                transaction_id: receipt
                    .migration_result
                    .as_ref()
                    .map(|run| run.id.0.to_string()),
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
                outcome: if operation == ChangeLedgerOperation::MigrationRollback
                    || receipt.migration_result.as_ref().is_some_and(|run| {
                        run.state == sift_protocol::MigrationRunState::RolledBack
                    }) {
                    ChangeLedgerOutcome::RolledBack
                } else {
                    match receipt.state {
                        AiDatabaseApplyState::Applied => ChangeLedgerOutcome::Committed,
                        AiDatabaseApplyState::Failed => ChangeLedgerOutcome::Failed,
                        _ => ChangeLedgerOutcome::Partial,
                    }
                },
                result_code: result_code.clone(),
                identity_source: sift_protocol::ChangeIdentitySource::Sift,
                identity_confidence: sift_protocol::ChangeIdentityConfidence::Authenticated,
            };
        let store = metadata.clone();
        metadata_blocking(move || store.append_change_ledger(input).map_err(Into::into)).await?;
    }
    Ok(())
}
