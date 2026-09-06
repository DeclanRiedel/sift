//! Repository HTTP handlers; shared admission and audit stay at the router boundary.

use super::*;

pub(super) async fn get_workspace_repository(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<Option<RepositoryBinding>>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let workspace_id = workspace_id(id)?;
    authorize_workspace_operation(
        &state,
        &auth,
        None,
        Some(workspace_id),
        WorkspaceAction::Read,
    )?;
    let actor = auth.principal_id;
    let binding = metadata_blocking(move || {
        metadata
            .repository_binding_for_workspace(workspace_id, actor)
            .map(|record| record.map(|record| record.binding))
            .map_err(Into::into)
    })
    .await?;
    if let Some(binding) = &binding {
        push_vcs_operation(&state, actor, VcsAction::Status, workspace_id, binding.id);
    } else {
        push_workspace_operation(
            &state,
            actor,
            WorkspaceAction::Read,
            Some(workspace_id),
            None,
        );
    }
    Ok(Json(binding))
}

pub(super) async fn bind_workspace_repository(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<BindRepositoryRequest>,
) -> ApiResult<Json<RepositoryBinding>> {
    use crate::git_adapter::VcsRepository as _;
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let workspace_id = workspace_id(id)?;
    authorize_workspace_operation(
        &state,
        &auth,
        None,
        Some(workspace_id),
        WorkspaceAction::BindProjection,
    )?;
    let _guard = state
        .rooms
        .workspace_lock(workspace_id.0)
        .lock_owned()
        .await;
    let actor = auth.principal_id;
    let projection = metadata_blocking({
        let metadata = metadata.clone();
        move || {
            let projection =
                metadata.projection_binding_for_principal(req.projection_id, actor, true)?;
            if projection.binding.workspace_id != workspace_id {
                return Err(ApiError::BadRequest(
                    "repository projection belongs to another workspace".into(),
                ));
            }
            Ok::<_, ApiError>(projection)
        }
    })
    .await?;
    let filesystem = state.rooms.workspace_adapter().ok_or_else(|| {
        ApiError::BadRequest("workspace filesystem projections are disabled".into())
    })?;
    let worktree = filesystem
        .canonical_root_path(&projection.root_handle)
        .map_err(workspace_adapter_error)?;
    let adapter = state
        .rooms
        .git_adapter()
        .ok_or_else(|| ApiError::BadRequest("Git integration is disabled".into()))?;
    let observation = if req.initialize {
        adapter.initialize(&worktree).await
    } else {
        adapter.discover(&worktree).await
    }
    .map_err(git_adapter_error)?;
    let input = NewRepositoryBinding {
        projection_id: req.projection_id,
        repository_identity: observation.identity,
        adapter_generation: adapter.generation().into(),
        executable_version: adapter.executable_version().into(),
        network_enabled: adapter.network_enabled(),
        branch: observation.branch,
        head: observation.head,
    };
    let binding = metadata_blocking({
        let metadata = metadata.clone();
        move || {
            metadata
                .create_repository_binding(workspace_id, actor, input)
                .map(|record| record.binding)
                .map_err(Into::into)
        }
    })
    .await?;
    push_vcs_operation(&state, actor, VcsAction::Bind, workspace_id, binding.id);
    let workspace = metadata_blocking(move || {
        metadata
            .get_workspace_for_principal(workspace_id, actor, false)
            .map_err(Into::into)
    })
    .await?;
    publish_repository_changed(&state, &workspace, binding.id, binding.revision);
    Ok(Json(binding))
}

pub(super) async fn clone_workspace_repository(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<CloneWorkspaceRepositoryRequest>,
) -> ApiResult<Json<RepositoryBinding>> {
    use crate::git_adapter::VcsRepository as _;
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let workspace_id = workspace_id(id)?;
    authorize_workspace_operation(
        &state,
        &auth,
        None,
        Some(workspace_id),
        WorkspaceAction::BindProjection,
    )?;
    let _guard = state
        .rooms
        .workspace_lock(workspace_id.0)
        .lock_owned()
        .await;
    let actor = auth.principal_id;
    let existing = metadata_blocking({
        let metadata = metadata.clone();
        move || {
            Ok::<_, ApiError>((
                metadata.projection_binding_for_workspace(workspace_id, actor)?,
                metadata.repository_binding_for_workspace(workspace_id, actor)?,
            ))
        }
    })
    .await?;
    if existing.0.is_some() || existing.1.is_some() {
        return Err(ApiError::Conflict(
            "workspace already has a projection or repository binding".into(),
        ));
    }
    let filesystem = state.rooms.workspace_adapter().ok_or_else(|| {
        ApiError::BadRequest("workspace filesystem projections are disabled".into())
    })?;
    filesystem
        .validate_binding(&req.root_handle, true)
        .map_err(workspace_adapter_error)?;
    let worktree = filesystem
        .canonical_root_path(&req.root_handle)
        .map_err(workspace_adapter_error)?;
    let adapter = state
        .rooms
        .git_adapter()
        .ok_or_else(|| ApiError::BadRequest("Git integration is disabled".into()))?;
    let username = req.username.0;
    let password = req.password.0;
    let credential_present = !username.is_empty() || !password.is_empty();
    let observation = adapter
        .clone_repository_into(
            &worktree,
            &req.url,
            crate::git_adapter::GitCredential {
                username: username.clone(),
                password: password.clone(),
            },
        )
        .await
        .map_err(git_adapter_error)?;
    let generation = crate::workspace_adapter::WorkspaceAdapter::generation(filesystem.as_ref());
    let root_handle = req.root_handle;
    let projection = metadata_blocking({
        let metadata = metadata.clone();
        move || {
            metadata
                .create_projection_binding(
                    workspace_id,
                    actor,
                    NewProjectionBinding {
                        root_handle,
                        mode: ProjectionMode::ReadWrite,
                        adapter_generation: generation.into(),
                        health: ProjectionHealth::Ready,
                    },
                )
                .map_err(Into::into)
        }
    })
    .await?;
    let input = NewRepositoryBinding {
        projection_id: projection.binding.id,
        repository_identity: observation.identity,
        adapter_generation: adapter.generation().into(),
        executable_version: adapter.executable_version().into(),
        network_enabled: adapter.network_enabled(),
        branch: observation.branch,
        head: observation.head,
    };
    let binding = metadata_blocking({
        let metadata = metadata.clone();
        move || {
            metadata
                .create_repository_binding(workspace_id, actor, input)
                .map_err(Into::into)
        }
    })
    .await?;
    let result = if credential_present {
        let mut stored_secret = serde_json::to_vec(&StoredGitCredential { username, password })
            .map_err(|_| ApiError::BadRequest("invalid repository credential".into()))?;
        let result = metadata
            .set_repository_credential(
                binding.binding.id,
                actor,
                binding.binding.revision,
                &stored_secret,
            )
            .await;
        stored_secret.fill(0);
        result
    } else {
        Ok(binding)
    };
    let binding = result?.binding;
    push_workspace_operation(
        &state,
        actor,
        WorkspaceAction::BindProjection,
        Some(workspace_id),
        None,
    );
    push_vcs_operation(&state, actor, VcsAction::Bind, workspace_id, binding.id);
    let workspace = metadata_blocking(move || {
        metadata
            .get_workspace_for_principal(workspace_id, actor, false)
            .map_err(Into::into)
    })
    .await?;
    publish_repository_changed(&state, &workspace, binding.id, binding.revision);
    Ok(Json(binding))
}

pub(super) async fn delete_workspace_repository(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<ExpectedRepositoryRevisionRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let (binding, workspace) = metadata_blocking({
        let metadata = metadata.clone();
        move || {
            let binding = metadata
                .repository_binding_for_principal(binding_id, actor, false)?
                .binding;
            let workspace =
                metadata.get_workspace_for_principal(binding.workspace_id, actor, true)?;
            Ok::<_, ApiError>((binding, workspace))
        }
    })
    .await?;
    authorize_vcs_operation(
        &state,
        &auth,
        binding.workspace_id,
        binding_id,
        VcsAction::Unbind,
    )?;
    let lease =
        RepositoryMutationLease::acquire(&state, &workspace, binding_id, actor, VcsAction::Unbind)
            .await;
    metadata
        .delete_repository_binding(binding_id, actor, req.expected_revision)
        .await?;
    push_vcs_operation(
        &state,
        actor,
        VcsAction::Unbind,
        binding.workspace_id,
        binding_id,
    );
    lease.succeed();
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn get_repository_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<VcsStatus>> {
    use crate::git_adapter::VcsRepository as _;
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, false).await?;
    authorize_vcs_operation(
        &state,
        &auth,
        context.record.binding.workspace_id,
        binding_id,
        VcsAction::Status,
    )?;
    let mut status = context
        .adapter
        .status(
            &context.worktree,
            binding_id,
            context.record.binding.revision,
            context.workspace.revision,
        )
        .await
        .map_err(git_adapter_error)?;
    let pending = state.rooms.vcs_pending(binding_id.0);
    for entry in &mut status.entries {
        entry.pending = pending.get(&entry.path.0).copied();
    }
    let filesystem = state.rooms.workspace_adapter().ok_or_else(|| {
        ApiError::BadRequest("workspace filesystem projections are disabled".into())
    })?;
    let inputs = metadata_blocking({
        let metadata = metadata.clone();
        let rooms = state.rooms.clone();
        let filesystem = filesystem.clone();
        let projection_id = context.record.binding.projection_id;
        move || load_projection_inputs(&metadata, &rooms, &filesystem, projection_id, actor, false)
    })
    .await?;
    let validation = crate::vcs_validation::validate(&status, &inputs.files);
    for entry in &mut status.entries {
        if let Some(artifact) = validation
            .artifacts
            .iter()
            .find(|artifact| artifact.path == entry.path)
        {
            entry.affected_objects = artifact.affected_objects.clone();
        }
        entry.validation_errors = validation
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.error && diagnostic.path == entry.path)
            .count() as u32;
    }
    status.validation = Some(validation);
    push_vcs_operation(
        &state,
        actor,
        VcsAction::Status,
        context.record.binding.workspace_id,
        binding_id,
    );
    push_vcs_operation(
        &state,
        actor,
        VcsAction::Validate,
        context.record.binding.workspace_id,
        binding_id,
    );
    Ok(Json(status))
}

pub(super) async fn get_repository_diff(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(query): Query<VcsDiffQuery>,
) -> ApiResult<Json<VcsDiff>> {
    use crate::git_adapter::VcsRepository as _;
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let context = load_repository_context(&state, metadata, actor, binding_id, false).await?;
    authorize_vcs_operation(
        &state,
        &auth,
        context.record.binding.workspace_id,
        binding_id,
        VcsAction::Diff,
    )?;
    let diff = context
        .adapter
        .diff(
            &context.worktree,
            binding_id,
            query.side,
            query.path.as_ref(),
        )
        .await
        .map_err(git_adapter_error)?;
    push_vcs_operation(
        &state,
        actor,
        VcsAction::Diff,
        context.record.binding.workspace_id,
        binding_id,
    );
    Ok(Json(diff))
}

pub(super) async fn list_repository_branches(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<VcsBranch>>> {
    use crate::git_adapter::VcsRepository as _;
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let context = load_repository_context(&state, metadata, actor, binding_id, false).await?;
    authorize_vcs_operation(
        &state,
        &auth,
        context.record.binding.workspace_id,
        binding_id,
        VcsAction::Branches,
    )?;
    let branches = context
        .adapter
        .branches(&context.worktree)
        .await
        .map_err(git_adapter_error)?;
    push_vcs_operation(
        &state,
        actor,
        VcsAction::Branches,
        context.record.binding.workspace_id,
        binding_id,
    );
    Ok(Json(branches))
}

pub(super) async fn get_repository_history(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(query): Query<VcsHistoryQuery>,
) -> ApiResult<Json<VcsHistoryPage>> {
    use crate::git_adapter::VcsRepository as _;
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let context = load_repository_context(&state, metadata, actor, binding_id, false).await?;
    authorize_vcs_operation(
        &state,
        &auth,
        context.workspace.id,
        binding_id,
        VcsAction::History,
    )?;
    let page = context
        .adapter
        .history(
            &context.worktree,
            query.cursor.as_deref(),
            query.limit,
            query.query.as_deref(),
        )
        .await
        .map_err(git_adapter_error)?;
    push_vcs_operation(
        &state,
        actor,
        VcsAction::History,
        context.workspace.id,
        binding_id,
    );
    Ok(Json(page))
}

pub(super) async fn compare_repository_commits(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(query): Query<VcsCompareQuery>,
) -> ApiResult<Json<VcsDiff>> {
    use crate::git_adapter::VcsRepository as _;
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let context = load_repository_context(&state, metadata, actor, binding_id, false).await?;
    authorize_vcs_operation(
        &state,
        &auth,
        context.workspace.id,
        binding_id,
        VcsAction::History,
    )?;
    let diff = context
        .adapter
        .revision_diff(&context.worktree, binding_id, &query.base, &query.target)
        .await
        .map_err(git_adapter_error)?;
    push_vcs_operation(
        &state,
        actor,
        VcsAction::History,
        context.workspace.id,
        binding_id,
    );
    Ok(Json(diff))
}

pub(super) async fn get_repository_commit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, oid)): Path<(i64, String)>,
) -> ApiResult<Json<VcsCommitDetail>> {
    use crate::git_adapter::VcsRepository as _;
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, false).await?;
    authorize_vcs_operation(
        &state,
        &auth,
        context.workspace.id,
        binding_id,
        VcsAction::History,
    )?;
    let mut detail = context
        .adapter
        .commit_detail(&context.worktree, &oid)
        .await
        .map_err(git_adapter_error)?;
    let provenance = metadata_blocking({
        let metadata = metadata.clone();
        let oid = oid.clone();
        move || {
            metadata
                .repository_commit_provenance(binding_id, actor, &oid)
                .map_err(Into::into)
        }
    })
    .await?;
    if let Some((checkpoint_id, workspace_revision)) = provenance {
        detail.checkpoint_id = Some(checkpoint_id);
        detail.workspace_revision = Some(workspace_revision);
    }
    push_vcs_operation(
        &state,
        actor,
        VcsAction::History,
        context.workspace.id,
        binding_id,
    );
    Ok(Json(detail))
}

pub(super) async fn get_repository_historical_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, oid)): Path<(i64, String)>,
    Query(query): Query<VcsHistoricalFileQuery>,
) -> ApiResult<Json<VcsHistoricalFile>> {
    use crate::git_adapter::VcsRepository as _;
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let context = load_repository_context(&state, metadata, actor, binding_id, false).await?;
    authorize_vcs_operation(
        &state,
        &auth,
        context.workspace.id,
        binding_id,
        VcsAction::History,
    )?;
    let file = context
        .adapter
        .historical_file(&context.worktree, &oid, &query.path)
        .await
        .map_err(git_adapter_error)?;
    push_vcs_operation(
        &state,
        actor,
        VcsAction::History,
        context.workspace.id,
        binding_id,
    );
    Ok(Json(file))
}

pub(super) async fn observe_repository_after_mutation(
    metadata: MetadataStore,
    binding_id: RepositoryBindingId,
    actor: PrincipalId,
    expected_revision: u64,
    observation: crate::git_adapter::GitRepositoryObservation,
) -> ApiResult<sift_metadata::RepositoryBindingRecord> {
    metadata_blocking(move || {
        metadata
            .observe_repository(
                binding_id,
                actor,
                sift_metadata::RepositoryObservation {
                    expected_revision,
                    branch: observation.branch,
                    head: observation.head,
                },
            )
            .map_err(Into::into)
    })
    .await
}

pub(super) async fn create_repository_branch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<VcsCreateBranchRequest>,
) -> ApiResult<Json<RepositoryBinding>> {
    use crate::git_adapter::VcsRepository as _;
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    authorize_vcs_operation(
        &state,
        &auth,
        context.workspace.id,
        binding_id,
        VcsAction::CreateBranch,
    )?;
    let lease = RepositoryMutationLease::acquire(
        &state,
        &context.workspace,
        binding_id,
        actor,
        VcsAction::CreateBranch,
    )
    .await;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    if context.record.binding.revision != req.expected_revision {
        return Err(ApiError::Conflict(
            "repository binding changed; refresh and retry".into(),
        ));
    }
    if req.start.is_some() && req.checkpoint_id.is_some() {
        return Err(ApiError::BadRequest(
            "choose either a commit start or a checkpoint, not both".into(),
        ));
    }
    let start = if let Some(checkpoint_id) = req.checkpoint_id {
        let lookup = metadata.clone();
        Some(
            metadata_blocking(move || {
                lookup
                    .repository_commit_for_checkpoint(binding_id, actor, checkpoint_id)
                    .map_err(Into::into)
            })
            .await?
            .ok_or_else(|| {
                ApiError::BadRequest("checkpoint is not linked to a repository commit".into())
            })?,
        )
    } else {
        req.start.clone()
    };
    context
        .adapter
        .create_branch(&context.worktree, &req.name, start.as_deref())
        .await
        .map_err(git_adapter_error)?;
    let observation = context
        .adapter
        .discover(&context.worktree)
        .await
        .map_err(git_adapter_error)?;
    let updated = observe_repository_after_mutation(
        metadata,
        binding_id,
        actor,
        req.expected_revision,
        observation,
    )
    .await?;
    push_vcs_operation(
        &state,
        actor,
        VcsAction::CreateBranch,
        context.workspace.id,
        binding_id,
    );
    publish_repository_changed(
        &state,
        &context.workspace,
        binding_id,
        updated.binding.revision,
    );
    lease.succeed();
    Ok(Json(updated.binding))
}

pub(super) async fn switch_repository_branch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<VcsSwitchBranchRequest>,
) -> ApiResult<Json<RepositoryBinding>> {
    use crate::git_adapter::VcsRepository as _;
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    authorize_vcs_operation(
        &state,
        &auth,
        context.workspace.id,
        binding_id,
        VcsAction::SwitchBranch,
    )?;
    let lease = RepositoryMutationLease::acquire(
        &state,
        &context.workspace,
        binding_id,
        actor,
        VcsAction::SwitchBranch,
    )
    .await;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    if context.record.binding.revision != req.expected_revision {
        return Err(ApiError::Conflict(
            "repository binding changed; refresh and retry".into(),
        ));
    }
    let status = context
        .adapter
        .status(
            &context.worktree,
            binding_id,
            req.expected_revision,
            context.workspace.revision,
        )
        .await
        .map_err(git_adapter_error)?;
    if !status.entries.is_empty() && !req.checkpoint_changes {
        return Err(ApiError::Conflict(
            "workspace contains changes; request checkpointed reconciliation before switching"
                .into(),
        ));
    }
    let old_head = context.record.binding.head.clone();
    let mut reconcile_paths = status
        .entries
        .iter()
        .flat_map(|entry| [Some(entry.path.clone()), entry.previous_path.clone()])
        .flatten()
        .collect::<Vec<_>>();
    if !status.entries.is_empty() {
        let lookup = metadata.clone();
        let workspace_id = context.workspace.id;
        let recoverable_paths = metadata_blocking(move || {
            lookup
                .list_workspace_nodes_for_principal(workspace_id, actor)
                .map(|nodes| {
                    nodes
                        .into_iter()
                        .filter(|node| node.kind == WorkspaceNodeKind::SqlDocument)
                        .map(|node| node.path)
                        .collect::<std::collections::HashSet<_>>()
                })
                .map_err(Into::into)
        })
        .await?;
        if status.entries.iter().any(|entry| {
            !recoverable_paths.contains(&entry.path)
                && entry
                    .previous_path
                    .as_ref()
                    .map_or(true, |path| !recoverable_paths.contains(path))
        }) {
            return Err(ApiError::Conflict(
                "checkpointed switch cannot recover projection-only Git changes; reconcile them into the workspace or discard them first".into(),
            ));
        }
        capture_before_vcs_checkpoint(
            &state,
            metadata.clone(),
            actor,
            context.workspace.id,
            context.workspace.revision,
        )
        .await?;
        context
            .adapter
            .clean_for_switch(&context.worktree, &reconcile_paths)
            .await
            .map_err(git_adapter_error)?;
    }
    let observation = context
        .adapter
        .switch_branch(&context.worktree, &req.target, req.detached)
        .await
        .map_err(git_adapter_error)?;
    reconcile_paths.extend(
        context
            .adapter
            .changed_paths_between(
                &context.worktree,
                old_head.as_deref(),
                observation.head.as_deref(),
            )
            .await
            .map_err(git_adapter_error)?,
    );
    reconcile_paths.sort_by(|left, right| left.0.cmp(&right.0));
    reconcile_paths.dedup();
    let mut workspace = context.workspace.clone();
    for path in &reconcile_paths {
        workspace = reconcile_vcs_worktree_path(
            &state,
            metadata.clone(),
            actor,
            context.record.binding.projection_id,
            path,
        )
        .await?;
    }
    let updated = observe_repository_after_mutation(
        metadata,
        binding_id,
        actor,
        req.expected_revision,
        observation,
    )
    .await?;
    push_vcs_operation(
        &state,
        actor,
        VcsAction::SwitchBranch,
        workspace.id,
        binding_id,
    );
    publish_repository_changed(&state, &workspace, binding_id, updated.binding.revision);
    lease.succeed();
    Ok(Json(updated.binding))
}

pub(super) async fn rename_repository_branch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<VcsRenameBranchRequest>,
) -> ApiResult<Json<RepositoryBinding>> {
    use crate::git_adapter::VcsRepository as _;
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    authorize_vcs_operation(
        &state,
        &auth,
        context.workspace.id,
        binding_id,
        VcsAction::RenameBranch,
    )?;
    let lease = RepositoryMutationLease::acquire(
        &state,
        &context.workspace,
        binding_id,
        actor,
        VcsAction::RenameBranch,
    )
    .await;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    if context.record.binding.revision != req.expected_revision {
        return Err(ApiError::Conflict(
            "repository binding changed; refresh and retry".into(),
        ));
    }
    context
        .adapter
        .rename_branch(&context.worktree, &req.old, &req.new)
        .await
        .map_err(git_adapter_error)?;
    let observation = context
        .adapter
        .discover(&context.worktree)
        .await
        .map_err(git_adapter_error)?;
    let updated = observe_repository_after_mutation(
        metadata,
        binding_id,
        actor,
        req.expected_revision,
        observation,
    )
    .await?;
    push_vcs_operation(
        &state,
        actor,
        VcsAction::RenameBranch,
        context.workspace.id,
        binding_id,
    );
    publish_repository_changed(
        &state,
        &context.workspace,
        binding_id,
        updated.binding.revision,
    );
    lease.succeed();
    Ok(Json(updated.binding))
}

pub(super) async fn delete_repository_branch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<VcsDeleteBranchRequest>,
) -> ApiResult<Json<RepositoryBinding>> {
    use crate::git_adapter::VcsRepository as _;
    if req.force && !req.confirm_unmerged {
        return Err(ApiError::BadRequest(
            "force deletion requires explicit unmerged-branch confirmation".into(),
        ));
    }
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    authorize_vcs_operation(
        &state,
        &auth,
        context.workspace.id,
        binding_id,
        VcsAction::DeleteBranch,
    )?;
    let lease = RepositoryMutationLease::acquire(
        &state,
        &context.workspace,
        binding_id,
        actor,
        VcsAction::DeleteBranch,
    )
    .await;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    if context.record.binding.revision != req.expected_revision {
        return Err(ApiError::Conflict(
            "repository binding changed; refresh and retry".into(),
        ));
    }
    context
        .adapter
        .delete_branch(&context.worktree, &req.name, req.force)
        .await
        .map_err(git_adapter_error)?;
    let observation = context
        .adapter
        .discover(&context.worktree)
        .await
        .map_err(git_adapter_error)?;
    let updated = observe_repository_after_mutation(
        metadata,
        binding_id,
        actor,
        req.expected_revision,
        observation,
    )
    .await?;
    push_vcs_operation(
        &state,
        actor,
        VcsAction::DeleteBranch,
        context.workspace.id,
        binding_id,
    );
    publish_repository_changed(
        &state,
        &context.workspace,
        binding_id,
        updated.binding.revision,
    );
    lease.succeed();
    Ok(Json(updated.binding))
}

pub(super) async fn set_repository_upstream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<VcsSetUpstreamRequest>,
) -> ApiResult<Json<RepositoryBinding>> {
    use crate::git_adapter::VcsRepository as _;
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    authorize_vcs_operation(
        &state,
        &auth,
        context.workspace.id,
        binding_id,
        VcsAction::SetUpstream,
    )?;
    let lease = RepositoryMutationLease::acquire(
        &state,
        &context.workspace,
        binding_id,
        actor,
        VcsAction::SetUpstream,
    )
    .await;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    if context.record.binding.revision != req.expected_revision {
        return Err(ApiError::Conflict(
            "repository binding changed; refresh and retry".into(),
        ));
    }
    context
        .adapter
        .set_upstream(&context.worktree, &req.branch, req.upstream.as_deref())
        .await
        .map_err(git_adapter_error)?;
    let observation = context
        .adapter
        .discover(&context.worktree)
        .await
        .map_err(git_adapter_error)?;
    let updated = observe_repository_after_mutation(
        metadata,
        binding_id,
        actor,
        req.expected_revision,
        observation,
    )
    .await?;
    push_vcs_operation(
        &state,
        actor,
        VcsAction::SetUpstream,
        context.workspace.id,
        binding_id,
    );
    publish_repository_changed(
        &state,
        &context.workspace,
        binding_id,
        updated.binding.revision,
    );
    lease.succeed();
    Ok(Json(updated.binding))
}

pub(super) async fn restore_repository_historical_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<VcsRestoreHistoricalFileRequest>,
) -> ApiResult<Json<VcsWorktreeMutationResult>> {
    use crate::git_adapter::VcsRepository as _;
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    authorize_vcs_operation(
        &state,
        &auth,
        context.workspace.id,
        binding_id,
        VcsAction::Revert,
    )?;
    let lease = RepositoryMutationLease::acquire(
        &state,
        &context.workspace,
        binding_id,
        actor,
        VcsAction::Revert,
    )
    .await;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    if context.record.binding.revision != req.expected_revision {
        return Err(ApiError::Conflict(
            "repository binding changed; refresh and retry".into(),
        ));
    }
    let checkpoint = capture_before_vcs_checkpoint(
        &state,
        metadata.clone(),
        actor,
        context.workspace.id,
        context.workspace.revision,
    )
    .await?;
    context
        .adapter
        .restore_historical_file(&context.worktree, &req.commit, &req.path)
        .await
        .map_err(git_adapter_error)?;
    let workspace = reconcile_vcs_worktree_path(
        &state,
        metadata.clone(),
        actor,
        context.record.binding.projection_id,
        &req.path,
    )
    .await?;
    let observation = context
        .adapter
        .discover(&context.worktree)
        .await
        .map_err(git_adapter_error)?;
    let updated = observe_repository_after_mutation(
        metadata,
        binding_id,
        actor,
        req.expected_revision,
        observation,
    )
    .await?;
    push_vcs_operation(&state, actor, VcsAction::Revert, workspace.id, binding_id);
    publish_repository_changed(&state, &workspace, binding_id, updated.binding.revision);
    lease.succeed();
    Ok(Json(VcsWorktreeMutationResult {
        binding_id,
        checkpoint_id: checkpoint.id,
        workspace_revision: workspace.revision,
        path: req.path,
    }))
}

pub(super) async fn revert_repository_commit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<VcsRevertCommitRequest>,
) -> ApiResult<Json<VcsHeadMutationResult>> {
    use crate::git_adapter::VcsRepository as _;
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    authorize_vcs_operation(
        &state,
        &auth,
        context.workspace.id,
        binding_id,
        VcsAction::Revert,
    )?;
    let lease = RepositoryMutationLease::acquire(
        &state,
        &context.workspace,
        binding_id,
        actor,
        VcsAction::Revert,
    )
    .await;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    if context.record.binding.revision != req.expected_revision {
        return Err(ApiError::Conflict(
            "repository binding changed; refresh and retry".into(),
        ));
    }
    let before = context
        .record
        .binding
        .head
        .clone()
        .ok_or_else(|| ApiError::Conflict("cannot revert from an unborn branch".into()))?;
    let status = context
        .adapter
        .status(
            &context.worktree,
            binding_id,
            req.expected_revision,
            context.workspace.revision,
        )
        .await
        .map_err(git_adapter_error)?;
    if !status.entries.is_empty() {
        return Err(ApiError::Conflict(
            "commit revert requires a clean worktree".into(),
        ));
    }
    let checkpoint = capture_before_vcs_checkpoint(
        &state,
        metadata.clone(),
        actor,
        context.workspace.id,
        context.workspace.revision,
    )
    .await?;
    context
        .adapter
        .revert_commit(&context.worktree, &req.commit)
        .await
        .map_err(git_adapter_error)?;
    let changed = context
        .adapter
        .status(
            &context.worktree,
            binding_id,
            req.expected_revision,
            checkpoint.workspace_revision,
        )
        .await
        .map_err(git_adapter_error)?;
    let mut workspace = context.workspace.clone();
    for path in changed.entries.iter().map(|entry| &entry.path) {
        workspace = reconcile_vcs_worktree_path(
            &state,
            metadata.clone(),
            actor,
            context.record.binding.projection_id,
            path,
        )
        .await?;
    }
    let observation = context
        .adapter
        .discover(&context.worktree)
        .await
        .map_err(git_adapter_error)?;
    let branch = observation.branch.clone();
    let head = observation.head.clone();
    let updated = observe_repository_after_mutation(
        metadata,
        binding_id,
        actor,
        req.expected_revision,
        observation,
    )
    .await?;
    push_vcs_operation(&state, actor, VcsAction::Revert, workspace.id, binding_id);
    publish_repository_changed(&state, &workspace, binding_id, updated.binding.revision);
    lease.succeed();
    Ok(Json(VcsHeadMutationResult {
        binding_id,
        checkpoint_id: checkpoint.id,
        workspace_revision: workspace.revision,
        previous_head: before,
        head,
        branch,
    }))
}

pub(super) async fn get_repository_conflict(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(query): Query<VcsConflictQuery>,
) -> ApiResult<Json<VcsConflictFile>> {
    use crate::git_adapter::VcsRepository as _;
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let context = load_repository_context(&state, metadata, actor, binding_id, false).await?;
    authorize_vcs_operation(
        &state,
        &auth,
        context.workspace.id,
        binding_id,
        VcsAction::Conflicts,
    )?;
    let conflict = context
        .adapter
        .conflict_file(&context.worktree, &query.path)
        .await
        .map_err(git_adapter_error)?;
    push_vcs_operation(
        &state,
        actor,
        VcsAction::Conflicts,
        context.workspace.id,
        binding_id,
    );
    Ok(Json(conflict))
}

pub(super) async fn begin_repository_conflict_resolution(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<VcsBeginConflictResolutionRequest>,
) -> ApiResult<Json<WorkspaceCheckpoint>> {
    use crate::git_adapter::VcsRepository as _;
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    authorize_vcs_operation(
        &state,
        &auth,
        context.workspace.id,
        binding_id,
        VcsAction::ResolveConflict,
    )?;
    let lease = RepositoryMutationLease::acquire(
        &state,
        &context.workspace,
        binding_id,
        actor,
        VcsAction::ResolveConflict,
    )
    .await;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    if context.record.binding.revision != req.expected_revision {
        return Err(ApiError::Conflict(
            "repository binding changed; refresh and retry".into(),
        ));
    }
    context
        .adapter
        .conflict_file(&context.worktree, &req.path)
        .await
        .map_err(git_adapter_error)?;
    let checkpoint = capture_before_vcs_checkpoint(
        &state,
        metadata,
        actor,
        context.workspace.id,
        context.workspace.revision,
    )
    .await?;
    push_vcs_operation(
        &state,
        actor,
        VcsAction::ResolveConflict,
        context.workspace.id,
        binding_id,
    );
    lease.succeed();
    Ok(Json(checkpoint))
}

pub(super) async fn resolve_repository_conflict(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<VcsResolveConflictRequest>,
) -> ApiResult<Json<VcsWorktreeMutationResult>> {
    use crate::git_adapter::VcsRepository as _;
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    authorize_vcs_operation(
        &state,
        &auth,
        context.workspace.id,
        binding_id,
        VcsAction::ResolveConflict,
    )?;
    let lease = RepositoryMutationLease::acquire(
        &state,
        &context.workspace,
        binding_id,
        actor,
        VcsAction::ResolveConflict,
    )
    .await;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    if context.record.binding.revision != req.expected_revision {
        return Err(ApiError::Conflict(
            "repository binding changed; refresh and retry".into(),
        ));
    }
    let conflict = context
        .adapter
        .conflict_file(&context.worktree, &req.path)
        .await
        .map_err(git_adapter_error)?;
    if conflict.binary {
        if !req.region_id.is_empty() {
            return Err(ApiError::Conflict(
                "binary conflict has no textual region".into(),
            ));
        }
    } else if conflict.regions.len() != 1 || conflict.regions[0].id != req.region_id {
        return Err(ApiError::Conflict(
            "conflict changed; refresh and retry".into(),
        ));
    }
    let checkpoint = capture_before_vcs_checkpoint(
        &state,
        metadata.clone(),
        actor,
        context.workspace.id,
        context.workspace.revision,
    )
    .await?;
    context
        .adapter
        .resolve_conflict(&context.worktree, &conflict, req.resolution)
        .await
        .map_err(git_adapter_error)?;
    let workspace = reconcile_vcs_worktree_path(
        &state,
        metadata.clone(),
        actor,
        context.record.binding.projection_id,
        &req.path,
    )
    .await?;
    let observation = context
        .adapter
        .discover(&context.worktree)
        .await
        .map_err(git_adapter_error)?;
    let updated = observe_repository_after_mutation(
        metadata,
        binding_id,
        actor,
        req.expected_revision,
        observation,
    )
    .await?;
    push_vcs_operation(
        &state,
        actor,
        VcsAction::ResolveConflict,
        workspace.id,
        binding_id,
    );
    publish_repository_changed(&state, &workspace, binding_id, updated.binding.revision);
    lease.succeed();
    Ok(Json(VcsWorktreeMutationResult {
        binding_id,
        checkpoint_id: checkpoint.id,
        workspace_revision: workspace.revision,
        path: req.path,
    }))
}

pub(super) async fn mark_repository_conflict_resolved(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<VcsMarkConflictResolvedRequest>,
) -> ApiResult<Json<RepositoryBinding>> {
    use crate::git_adapter::VcsRepository as _;
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    authorize_vcs_operation(
        &state,
        &auth,
        context.workspace.id,
        binding_id,
        VcsAction::ResolveConflict,
    )?;
    let lease = RepositoryMutationLease::acquire(
        &state,
        &context.workspace,
        binding_id,
        actor,
        VcsAction::ResolveConflict,
    )
    .await;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    if context.record.binding.revision != req.expected_revision {
        return Err(ApiError::Conflict(
            "repository binding changed; refresh and retry".into(),
        ));
    }
    context
        .adapter
        .conflict_file(&context.worktree, &req.path)
        .await
        .map_err(git_adapter_error)?;
    context
        .adapter
        .mark_conflict_resolved(&context.worktree, &req.path)
        .await
        .map_err(git_adapter_error)?;
    let observation = context
        .adapter
        .discover(&context.worktree)
        .await
        .map_err(git_adapter_error)?;
    let updated = observe_repository_after_mutation(
        metadata,
        binding_id,
        actor,
        req.expected_revision,
        observation,
    )
    .await?;
    push_vcs_operation(
        &state,
        actor,
        VcsAction::ResolveConflict,
        context.workspace.id,
        binding_id,
    );
    publish_repository_changed(
        &state,
        &context.workspace,
        binding_id,
        updated.binding.revision,
    );
    lease.succeed();
    Ok(Json(updated.binding))
}

pub(super) async fn mutate_repository_operation(
    state: AppState,
    headers: HeaderMap,
    id: i64,
    req: VcsRepositoryOperationRequest,
    abort: bool,
) -> ApiResult<Json<RepositoryBinding>> {
    use crate::git_adapter::VcsRepository as _;
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    let action = if abort {
        VcsAction::AbortOperation
    } else {
        VcsAction::ContinueOperation
    };
    authorize_vcs_operation(&state, &auth, context.workspace.id, binding_id, action)?;
    let lease =
        RepositoryMutationLease::acquire(&state, &context.workspace, binding_id, actor, action)
            .await;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    if context.record.binding.revision != req.expected_revision {
        return Err(ApiError::Conflict(
            "repository binding changed; refresh and retry".into(),
        ));
    }
    let status = context
        .adapter
        .status(
            &context.worktree,
            binding_id,
            req.expected_revision,
            context.workspace.revision,
        )
        .await
        .map_err(git_adapter_error)?;
    if status.operation.as_ref().map(|operation| operation.kind) != Some(req.kind) {
        return Err(ApiError::Conflict(
            "repository operation changed; refresh and retry".into(),
        ));
    }
    if !abort
        && status
            .entries
            .iter()
            .any(|entry| entry.stage == sift_protocol::VcsStageState::Conflict)
    {
        return Err(ApiError::Conflict(
            "resolve every conflict before continuing".into(),
        ));
    }
    let paths = status
        .entries
        .iter()
        .map(|entry| entry.path.clone())
        .collect::<Vec<_>>();
    if abort {
        capture_before_vcs_checkpoint(
            &state,
            metadata.clone(),
            actor,
            context.workspace.id,
            context.workspace.revision,
        )
        .await?;
    }
    let observation = if abort {
        context
            .adapter
            .abort_operation(&context.worktree, req.kind)
            .await
    } else {
        context
            .adapter
            .continue_operation(&context.worktree, req.kind)
            .await
    }
    .map_err(git_adapter_error)?;
    let mut workspace = context.workspace.clone();
    for path in &paths {
        workspace = reconcile_vcs_worktree_path(
            &state,
            metadata.clone(),
            actor,
            context.record.binding.projection_id,
            path,
        )
        .await?;
    }
    let updated = observe_repository_after_mutation(
        metadata,
        binding_id,
        actor,
        req.expected_revision,
        observation,
    )
    .await?;
    push_vcs_operation(&state, actor, action, workspace.id, binding_id);
    publish_repository_changed(&state, &workspace, binding_id, updated.binding.revision);
    lease.succeed();
    Ok(Json(updated.binding))
}

pub(super) async fn continue_repository_operation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<VcsRepositoryOperationRequest>,
) -> ApiResult<Json<RepositoryBinding>> {
    mutate_repository_operation(state, headers, id, req, false).await
}

pub(super) async fn abort_repository_operation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<VcsRepositoryOperationRequest>,
) -> ApiResult<Json<RepositoryBinding>> {
    mutate_repository_operation(state, headers, id, req, true).await
}

pub(super) async fn repair_repository_binding(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<ExpectedRepositoryRevisionRequest>,
) -> ApiResult<Json<RepositoryBinding>> {
    use crate::git_adapter::VcsRepository as _;
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let context =
        load_repository_context_for_repair(&state, metadata.clone(), actor, binding_id).await?;
    authorize_vcs_operation(
        &state,
        &auth,
        context.workspace.id,
        binding_id,
        VcsAction::RepairBinding,
    )?;
    let lease = RepositoryMutationLease::acquire(
        &state,
        &context.workspace,
        binding_id,
        actor,
        VcsAction::RepairBinding,
    )
    .await;
    let context =
        load_repository_context_for_repair(&state, metadata.clone(), actor, binding_id).await?;
    if context.record.binding.revision != req.expected_revision {
        return Err(ApiError::Conflict(
            "repository binding changed; refresh and retry".into(),
        ));
    }
    let observation = context
        .adapter
        .discover(&context.worktree)
        .await
        .map_err(git_adapter_error)?;
    let adapter = context.adapter.clone();
    let projection_id = context.record.binding.projection_id;
    let updated = metadata_blocking(move || {
        metadata
            .repair_repository_binding(
                binding_id,
                actor,
                req.expected_revision,
                NewRepositoryBinding {
                    projection_id,
                    repository_identity: observation.identity,
                    adapter_generation: adapter.generation().into(),
                    executable_version: adapter.executable_version().into(),
                    network_enabled: adapter.network_enabled(),
                    branch: observation.branch,
                    head: observation.head,
                },
            )
            .map_err(Into::into)
    })
    .await?;
    push_vcs_operation(
        &state,
        actor,
        VcsAction::RepairBinding,
        context.workspace.id,
        binding_id,
    );
    publish_repository_changed(
        &state,
        &context.workspace,
        binding_id,
        updated.binding.revision,
    );
    lease.succeed();
    Ok(Json(updated.binding))
}

pub(super) async fn stage_repository_paths(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<VcsPathsRequest>,
) -> ApiResult<Json<RepositoryBinding>> {
    mutate_repository_paths(state, headers, id, req, true).await
}

pub(super) async fn unstage_repository_paths(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<VcsPathsRequest>,
) -> ApiResult<Json<RepositoryBinding>> {
    mutate_repository_paths(state, headers, id, req, false).await
}

pub(super) async fn stage_repository_hunk(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<VcsHunkRequest>,
) -> ApiResult<Json<RepositoryBinding>> {
    mutate_repository_hunk(state, headers, id, req, true).await
}

pub(super) async fn unstage_repository_hunk(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<VcsHunkRequest>,
) -> ApiResult<Json<RepositoryBinding>> {
    mutate_repository_hunk(state, headers, id, req, false).await
}

pub(super) async fn discard_repository_path(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<VcsDiscardRequest>,
) -> ApiResult<Json<VcsWorktreeMutationResult>> {
    use crate::git_adapter::VcsRepository as _;
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let initial =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    authorize_vcs_operation(
        &state,
        &auth,
        initial.record.binding.workspace_id,
        binding_id,
        VcsAction::Discard,
    )?;
    let workspace_id = initial.record.binding.workspace_id;
    let lease = RepositoryMutationLease::acquire(
        &state,
        &initial.workspace,
        binding_id,
        actor,
        VcsAction::Discard,
    )
    .await;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    if context.record.binding.revision != req.expected_revision {
        return Err(sift_metadata::MetadataError::RepositoryRevisionConflict {
            expected: req.expected_revision,
            current: context.record.binding.revision,
        }
        .into());
    }
    let status = context
        .adapter
        .status(
            &context.worktree,
            binding_id,
            context.record.binding.revision,
            context.workspace.revision,
        )
        .await
        .map_err(git_adapter_error)?;
    let entry = status
        .entries
        .iter()
        .find(|entry| entry.path == req.path)
        .ok_or_else(|| ApiError::Conflict("changed file is stale".into()))?;
    if !matches!(
        entry.state,
        sift_protocol::VcsFileState::Modified | sift_protocol::VcsFileState::Deleted
    ) || matches!(
        entry.stage,
        sift_protocol::VcsStageState::Staged | sift_protocol::VcsStageState::Conflict
    ) {
        return Err(ApiError::BadRequest(
            "discard is limited to tracked, non-conflicted worktree modifications or deletions"
                .into(),
        ));
    }
    let checkpoint = capture_before_vcs_checkpoint(
        &state,
        metadata.clone(),
        actor,
        workspace_id,
        context.workspace.revision,
    )
    .await?;
    state.rooms.set_vcs_pending(
        binding_id.0,
        std::slice::from_ref(&req.path),
        VcsPendingOperation::Discard,
    );
    let operation = context
        .adapter
        .discard_worktree_path(&context.worktree, &req.path)
        .await;
    state
        .rooms
        .clear_vcs_pending(binding_id.0, std::slice::from_ref(&req.path));
    operation.map_err(git_adapter_error)?;
    let workspace = reconcile_vcs_worktree_path(
        &state,
        metadata.clone(),
        actor,
        context.record.binding.projection_id,
        &req.path,
    )
    .await?;
    let observation = context
        .adapter
        .discover(&context.worktree)
        .await
        .map_err(git_adapter_error)?;
    let updated = metadata_blocking(move || {
        metadata
            .observe_repository(
                binding_id,
                actor,
                sift_metadata::RepositoryObservation {
                    expected_revision: req.expected_revision,
                    branch: observation.branch,
                    head: observation.head,
                },
            )
            .map(|record| record.binding)
            .map_err(Into::into)
    })
    .await?;
    push_vcs_operation(&state, actor, VcsAction::Discard, workspace_id, binding_id);
    publish_workspace_changed(
        &state,
        &public_workspace_record(workspace.clone(), workspace_runtime_capabilities(&state)),
        true,
    );
    publish_repository_changed(&state, &workspace, binding_id, updated.revision);
    lease.succeed();
    Ok(Json(VcsWorktreeMutationResult {
        binding_id,
        checkpoint_id: checkpoint.id,
        workspace_revision: workspace.revision,
        path: req.path,
    }))
}

pub(super) async fn revert_repository_hunk(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<VcsRevertHunkRequest>,
) -> ApiResult<Json<VcsWorktreeMutationResult>> {
    use crate::git_adapter::VcsRepository as _;
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let initial =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    authorize_vcs_operation(
        &state,
        &auth,
        initial.record.binding.workspace_id,
        binding_id,
        VcsAction::Revert,
    )?;
    if req.side != sift_protocol::VcsDiffSide::IndexToWorktree || req.hunk_id.len() != 64 {
        return Err(ApiError::BadRequest(
            "hunk revert requires an index-to-worktree hunk".into(),
        ));
    }
    let workspace_id = initial.record.binding.workspace_id;
    let lease = RepositoryMutationLease::acquire(
        &state,
        &initial.workspace,
        binding_id,
        actor,
        VcsAction::Revert,
    )
    .await;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    if context.record.binding.revision != req.expected_revision {
        return Err(sift_metadata::MetadataError::RepositoryRevisionConflict {
            expected: req.expected_revision,
            current: context.record.binding.revision,
        }
        .into());
    }
    let diff = context
        .adapter
        .diff(&context.worktree, binding_id, req.side, Some(&req.path))
        .await
        .map_err(git_adapter_error)?;
    let file = diff
        .files
        .iter()
        .find(|file| file.path == req.path)
        .ok_or_else(|| ApiError::Conflict("changed file is stale".into()))?;
    let hunk = file
        .hunks
        .iter()
        .find(|hunk| hunk.id == req.hunk_id)
        .ok_or_else(|| ApiError::Conflict("diff hunk is stale".into()))?;
    let checkpoint = capture_before_vcs_checkpoint(
        &state,
        metadata.clone(),
        actor,
        workspace_id,
        context.workspace.revision,
    )
    .await?;
    state.rooms.set_vcs_pending(
        binding_id.0,
        std::slice::from_ref(&req.path),
        VcsPendingOperation::Revert,
    );
    let operation = context
        .adapter
        .revert_worktree_hunk(&context.worktree, file, hunk)
        .await;
    state
        .rooms
        .clear_vcs_pending(binding_id.0, std::slice::from_ref(&req.path));
    operation.map_err(git_adapter_error)?;
    let workspace = reconcile_vcs_worktree_path(
        &state,
        metadata.clone(),
        actor,
        context.record.binding.projection_id,
        &req.path,
    )
    .await?;
    let observation = context
        .adapter
        .discover(&context.worktree)
        .await
        .map_err(git_adapter_error)?;
    let updated = metadata_blocking(move || {
        metadata
            .observe_repository(
                binding_id,
                actor,
                sift_metadata::RepositoryObservation {
                    expected_revision: req.expected_revision,
                    branch: observation.branch,
                    head: observation.head,
                },
            )
            .map(|record| record.binding)
            .map_err(Into::into)
    })
    .await?;
    push_vcs_operation(&state, actor, VcsAction::Revert, workspace_id, binding_id);
    publish_workspace_changed(
        &state,
        &public_workspace_record(workspace.clone(), workspace_runtime_capabilities(&state)),
        true,
    );
    publish_repository_changed(&state, &workspace, binding_id, updated.revision);
    lease.succeed();
    Ok(Json(VcsWorktreeMutationResult {
        binding_id,
        checkpoint_id: checkpoint.id,
        workspace_revision: workspace.revision,
        path: req.path,
    }))
}

pub(super) async fn mutate_repository_hunk(
    state: AppState,
    headers: HeaderMap,
    id: i64,
    req: VcsHunkRequest,
    stage: bool,
) -> ApiResult<Json<RepositoryBinding>> {
    use crate::git_adapter::VcsRepository as _;
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    let action = if stage {
        VcsAction::Stage
    } else {
        VcsAction::Unstage
    };
    authorize_vcs_operation(
        &state,
        &auth,
        context.record.binding.workspace_id,
        binding_id,
        action,
    )?;
    let lease =
        RepositoryMutationLease::acquire(&state, &context.workspace, binding_id, actor, action)
            .await;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    if context.record.binding.revision != req.expected_revision {
        return Err(sift_metadata::MetadataError::RepositoryRevisionConflict {
            expected: req.expected_revision,
            current: context.record.binding.revision,
        }
        .into());
    }
    let expected_side = if stage {
        sift_protocol::VcsDiffSide::IndexToWorktree
    } else {
        sift_protocol::VcsDiffSide::HeadToIndex
    };
    if req.side != expected_side
        || req.hunk_id.len() != 64
        || req
            .line_indices
            .as_ref()
            .is_some_and(|indices| indices.is_empty() || indices.len() > 4_096)
    {
        return Err(ApiError::BadRequest(
            "hunk operation has an invalid side or line selection".into(),
        ));
    }
    let diff = context
        .adapter
        .diff(&context.worktree, binding_id, req.side, Some(&req.path))
        .await
        .map_err(git_adapter_error)?;
    let file = diff
        .files
        .iter()
        .find(|file| file.path == req.path)
        .ok_or_else(|| ApiError::Conflict("changed file is stale".into()))?;
    let hunk = file
        .hunks
        .iter()
        .find(|hunk| hunk.id == req.hunk_id)
        .ok_or_else(|| ApiError::Conflict("diff hunk is stale".into()))?;
    let paths = [req.path.clone()];
    state.rooms.set_vcs_pending(
        binding_id.0,
        &paths,
        if stage {
            VcsPendingOperation::Stage
        } else {
            VcsPendingOperation::Unstage
        },
    );
    let operation = if let Some(line_indices) = req.line_indices.as_deref() {
        context
            .adapter
            .apply_lines(&context.worktree, file, hunk, line_indices, stage)
            .await
    } else {
        context
            .adapter
            .apply_hunk(&context.worktree, file, hunk, !stage)
            .await
    };
    state.rooms.clear_vcs_pending(binding_id.0, &paths);
    operation.map_err(git_adapter_error)?;
    let observation = context
        .adapter
        .discover(&context.worktree)
        .await
        .map_err(git_adapter_error)?;
    let updated = metadata_blocking(move || {
        metadata
            .observe_repository(
                binding_id,
                actor,
                sift_metadata::RepositoryObservation {
                    expected_revision: req.expected_revision,
                    branch: observation.branch,
                    head: observation.head,
                },
            )
            .map(|record| record.binding)
            .map_err(Into::into)
    })
    .await?;
    push_vcs_operation(&state, actor, action, updated.workspace_id, binding_id);
    publish_repository_changed(&state, &context.workspace, binding_id, updated.revision);
    lease.succeed();
    Ok(Json(updated))
}

pub(super) async fn mutate_repository_paths(
    state: AppState,
    headers: HeaderMap,
    id: i64,
    req: VcsPathsRequest,
    stage: bool,
) -> ApiResult<Json<RepositoryBinding>> {
    use crate::git_adapter::VcsRepository as _;
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    let action = if stage {
        VcsAction::Stage
    } else {
        VcsAction::Unstage
    };
    authorize_vcs_operation(
        &state,
        &auth,
        context.record.binding.workspace_id,
        binding_id,
        action,
    )?;
    let lease =
        RepositoryMutationLease::acquire(&state, &context.workspace, binding_id, actor, action)
            .await;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    if context.record.binding.revision != req.expected_revision {
        return Err(sift_metadata::MetadataError::RepositoryRevisionConflict {
            expected: req.expected_revision,
            current: context.record.binding.revision,
        }
        .into());
    }
    let pending = if stage {
        VcsPendingOperation::Stage
    } else {
        VcsPendingOperation::Unstage
    };
    state
        .rooms
        .set_vcs_pending(binding_id.0, &req.paths, pending);
    let operation = if stage {
        context.adapter.stage(&context.worktree, &req.paths).await
    } else {
        context.adapter.unstage(&context.worktree, &req.paths).await
    };
    state.rooms.clear_vcs_pending(binding_id.0, &req.paths);
    operation.map_err(git_adapter_error)?;
    let observation = context
        .adapter
        .discover(&context.worktree)
        .await
        .map_err(git_adapter_error)?;
    let updated = metadata_blocking(move || {
        metadata
            .observe_repository(
                binding_id,
                actor,
                sift_metadata::RepositoryObservation {
                    expected_revision: req.expected_revision,
                    branch: observation.branch,
                    head: observation.head,
                },
            )
            .map(|record| record.binding)
            .map_err(Into::into)
    })
    .await?;
    push_vcs_operation(&state, actor, action, updated.workspace_id, binding_id);
    publish_repository_changed(&state, &context.workspace, binding_id, updated.revision);
    lease.succeed();
    Ok(Json(updated))
}

pub(super) async fn commit_repository(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<VcsCommitRequest>,
) -> ApiResult<Json<VcsCommitResult>> {
    mutate_repository_commit(state, headers, id, req, false).await
}

pub(super) async fn amend_repository(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<VcsCommitRequest>,
) -> ApiResult<Json<VcsCommitResult>> {
    mutate_repository_commit(state, headers, id, req, true).await
}

pub(super) async fn mutate_repository_commit(
    state: AppState,
    headers: HeaderMap,
    id: i64,
    req: VcsCommitRequest,
    amend: bool,
) -> ApiResult<Json<VcsCommitResult>> {
    use crate::git_adapter::VcsRepository as _;
    use crate::workspace_adapter::WorkspaceAdapter as _;
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    let action = if amend {
        VcsAction::Amend
    } else {
        VcsAction::Commit
    };
    authorize_vcs_operation(
        &state,
        &auth,
        context.record.binding.workspace_id,
        binding_id,
        action,
    )?;
    let lease =
        RepositoryMutationLease::acquire(&state, &context.workspace, binding_id, actor, action)
            .await;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    if context.record.binding.revision != req.expected_revision {
        return Err(sift_metadata::MetadataError::RepositoryRevisionConflict {
            expected: req.expected_revision,
            current: context.record.binding.revision,
        }
        .into());
    }
    if amend
        && (req.expected_head.as_deref().is_none()
            || req.expected_head.as_deref() != context.record.binding.head.as_deref())
    {
        return Err(ApiError::Conflict(
            "repository HEAD changed before amend".into(),
        ));
    }
    let workspace_id = context.record.binding.workspace_id;
    let filesystem = state.rooms.workspace_adapter().ok_or_else(|| {
        ApiError::BadRequest("workspace filesystem projections are disabled".into())
    })?;
    let rooms = state.rooms.clone();
    let projection_id = context.record.binding.projection_id;
    let inputs = metadata_blocking({
        let metadata = metadata.clone();
        let filesystem = filesystem.clone();
        move || load_projection_inputs(&metadata, &rooms, &filesystem, projection_id, actor, true)
    })
    .await?;
    let plan = crate::workspace_projection::reconcile_plan(
        &inputs.binding.binding,
        inputs.workspace.revision,
        &inputs.baseline,
        &inputs.files,
        &inputs.projection,
    );
    if plan
        .entries
        .iter()
        .any(|entry| entry.state != sift_protocol::ReconcileState::Unchanged)
    {
        return Err(ApiError::BadRequest(
            "workspace projection must be fully reconciled before commit".into(),
        ));
    }
    let allowed = inputs
        .files
        .iter()
        .map(|file| file.path.0.clone())
        .chain(inputs.baseline.iter().map(|file| file.path.0.clone()))
        .collect::<std::collections::BTreeSet<_>>();
    if allowed.is_empty() {
        return Err(ApiError::BadRequest(
            "workspace has no SQL paths to commit".into(),
        ));
    }
    let status = context
        .adapter
        .status(
            &context.worktree,
            binding_id,
            req.expected_revision,
            inputs.workspace.revision,
        )
        .await
        .map_err(git_adapter_error)?;
    if status.entries.iter().any(|entry| {
        entry.stage != sift_protocol::VcsStageState::Unstaged && !allowed.contains(&entry.path.0)
    }) {
        return Err(ApiError::BadRequest(
            "Git index contains staged paths outside the workspace SQL tree".into(),
        ));
    }
    if !status.entries.iter().any(|entry| {
        matches!(
            entry.stage,
            sift_protocol::VcsStageState::Staged | sift_protocol::VcsStageState::PartiallyStaged
        )
    }) {
        return Err(ApiError::BadRequest(
            "Git index has no staged SQL changes to commit".into(),
        ));
    }
    let validation = crate::vcs_validation::validate(&status, &inputs.files);
    if !validation.valid {
        let summary = validation
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.error)
            .take(5)
            .map(|diagnostic| format!("{}: {}", diagnostic.path.0, diagnostic.message))
            .collect::<Vec<_>>()
            .join("; ");
        push_vcs_operation(&state, actor, VcsAction::Validate, workspace_id, binding_id);
        return Err(ApiError::BadRequest(format!(
            "pre-commit SQL validation failed: {summary}"
        )));
    }
    push_vcs_operation(&state, actor, VcsAction::Validate, workspace_id, binding_id);
    filesystem
        .materialize(
            &inputs.binding.root_handle,
            &inputs
                .files
                .iter()
                .map(|file| crate::workspace_adapter::MaterializeFile {
                    path: file.path.clone(),
                    bytes: file.bytes.clone(),
                })
                .collect::<Vec<_>>(),
        )
        .map_err(workspace_adapter_error)?;
    let checkpoint = metadata_blocking({
        let metadata = metadata.clone();
        let revision = inputs.workspace.revision;
        let captures = inputs.captures;
        move || {
            metadata
                .create_workspace_checkpoint(
                    workspace_id,
                    actor,
                    NewWorkspaceCheckpoint {
                        expected_revision: revision,
                        reason: sift_protocol::WorkspaceCheckpointReason::BeforeVcs,
                        name: None,
                        captures,
                    },
                )
                .map_err(Into::into)
        }
    })
    .await?;
    let observation = if amend {
        context
            .adapter
            .amend(
                &context.worktree,
                &req.message,
                &req.author_name,
                &req.author_email,
            )
            .await
    } else {
        context
            .adapter
            .commit(
                &context.worktree,
                &req.message,
                &req.author_name,
                &req.author_email,
            )
            .await
    }
    .map_err(git_adapter_error)?;
    let commit = observation
        .head
        .clone()
        .ok_or_else(|| ApiError::Internal("Git commit did not produce a head".into()))?;
    let branch = observation.branch.clone();
    let commit_for_metadata = commit.clone();
    metadata_blocking(move || {
        metadata
            .record_repository_commit(
                binding_id,
                actor,
                sift_metadata::RepositoryObservation {
                    expected_revision: req.expected_revision,
                    branch: observation.branch,
                    head: observation.head,
                },
                sift_metadata::NewRepositoryCommit {
                    commit_oid: commit_for_metadata,
                    checkpoint_id: checkpoint.id,
                    workspace_revision: checkpoint.workspace_revision,
                },
            )
            .map_err(Into::into)
    })
    .await?;
    push_vcs_operation(&state, actor, action, workspace_id, binding_id);
    publish_repository_changed(
        &state,
        &context.workspace,
        binding_id,
        req.expected_revision.saturating_add(1),
    );
    lease.succeed();
    Ok(Json(VcsCommitResult {
        binding_id,
        checkpoint_id: checkpoint.id,
        workspace_revision: checkpoint.workspace_revision,
        commit,
        branch,
    }))
}

pub(super) async fn uncommit_repository(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<VcsUncommitRequest>,
) -> ApiResult<Json<VcsHeadMutationResult>> {
    use crate::git_adapter::VcsRepository as _;
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    authorize_vcs_operation(
        &state,
        &auth,
        context.record.binding.workspace_id,
        binding_id,
        VcsAction::Uncommit,
    )?;
    let lease = RepositoryMutationLease::acquire(
        &state,
        &context.workspace,
        binding_id,
        actor,
        VcsAction::Uncommit,
    )
    .await;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    if context.record.binding.revision != req.expected_revision {
        return Err(sift_metadata::MetadataError::RepositoryRevisionConflict {
            expected: req.expected_revision,
            current: context.record.binding.revision,
        }
        .into());
    }
    if context.record.binding.head.as_deref() != Some(req.expected_head.as_str()) {
        return Err(ApiError::Conflict(
            "repository HEAD changed before uncommit".into(),
        ));
    }
    let workspace_id = context.record.binding.workspace_id;
    let checkpoint = capture_before_vcs_checkpoint(
        &state,
        metadata.clone(),
        actor,
        workspace_id,
        context.workspace.revision,
    )
    .await?;
    state.rooms.publish_presence(
        context.workspace.room_id.0,
        RoomServerMessage::WorkspaceChanged {
            workspace_id: context.workspace.id.0,
            revision: context.workspace.revision.0,
            checkpoints_changed: true,
        },
    );
    let observation = context
        .adapter
        .soft_reset_parent(&context.worktree)
        .await
        .map_err(git_adapter_error)?;
    let head = observation.head.clone();
    let branch = observation.branch.clone();
    let updated = metadata_blocking(move || {
        metadata
            .observe_repository(
                binding_id,
                actor,
                sift_metadata::RepositoryObservation {
                    expected_revision: req.expected_revision,
                    branch: observation.branch,
                    head: observation.head,
                },
            )
            .map(|record| record.binding)
            .map_err(Into::into)
    })
    .await?;
    push_vcs_operation(&state, actor, VcsAction::Uncommit, workspace_id, binding_id);
    publish_repository_changed(&state, &context.workspace, binding_id, updated.revision);
    lease.succeed();
    Ok(Json(VcsHeadMutationResult {
        binding_id,
        checkpoint_id: checkpoint.id,
        workspace_revision: checkpoint.workspace_revision,
        previous_head: req.expected_head,
        head,
        branch,
    }))
}

pub(super) async fn capture_before_vcs_checkpoint(
    state: &AppState,
    metadata: MetadataStore,
    actor: PrincipalId,
    workspace_id: WorkspaceId,
    expected_revision: sift_protocol::WorkspaceRevision,
) -> ApiResult<WorkspaceCheckpoint> {
    let rooms = state.rooms.clone();
    metadata_blocking(move || {
        let workspace = metadata.get_workspace_for_principal(workspace_id, actor, true)?;
        if workspace.revision != expected_revision {
            return Err(sift_metadata::MetadataError::WorkspaceRevisionConflict {
                expected: expected_revision.0,
                current: workspace.revision.0,
            }
            .into());
        }
        let nodes = metadata.list_workspace_nodes_for_principal(workspace_id, actor)?;
        let mut captures = Vec::new();
        for node in nodes
            .iter()
            .filter(|node| node.kind == WorkspaceNodeKind::SqlDocument)
        {
            let document = DocumentId(
                node.document_id
                    .ok_or(sift_metadata::MetadataError::InvalidWorkspaceNode)?,
            );
            let document_actor = rooms
                .documents()
                .get_or_load(&metadata, document)
                .map_err(workspace_actor_error)?;
            let guard = document_actor
                .lock()
                .map_err(|_| ApiError::Internal("document actor mutex poisoned".into()))?;
            captures.push(WorkspaceCheckpointCapture {
                node_id: node.id,
                snapshot_bytes: guard.snapshot().map_err(workspace_actor_error)?,
                snapshot_version: guard.version_vector(),
            });
        }
        metadata
            .create_workspace_checkpoint(
                workspace_id,
                actor,
                NewWorkspaceCheckpoint {
                    expected_revision,
                    reason: sift_protocol::WorkspaceCheckpointReason::BeforeVcs,
                    name: None,
                    captures,
                },
            )
            .map_err(Into::into)
    })
    .await
}

pub(super) async fn set_repository_credential(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<SetVcsCredentialRequest>,
) -> ApiResult<Json<RepositoryBinding>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let (binding, workspace) = metadata_blocking({
        let metadata = metadata.clone();
        move || {
            let binding = metadata
                .repository_binding_for_principal(binding_id, actor, true)?
                .binding;
            let workspace =
                metadata.get_workspace_for_principal(binding.workspace_id, actor, true)?;
            Ok::<_, ApiError>((binding, workspace))
        }
    })
    .await?;
    authorize_vcs_operation(
        &state,
        &auth,
        binding.workspace_id,
        binding_id,
        VcsAction::SetCredential,
    )?;
    let lease = RepositoryMutationLease::acquire(
        &state,
        &workspace,
        binding_id,
        actor,
        VcsAction::SetCredential,
    )
    .await;
    let mut secret = serde_json::to_vec(&StoredGitCredential {
        username: req.username.0,
        password: req.password.0,
    })
    .map_err(|_| ApiError::BadRequest("invalid repository credential".into()))?;
    let result = metadata
        .set_repository_credential(binding_id, actor, req.expected_revision, &secret)
        .await;
    secret.fill(0);
    let updated = result?.binding;
    push_vcs_operation(
        &state,
        actor,
        VcsAction::SetCredential,
        updated.workspace_id,
        binding_id,
    );
    publish_repository_changed(&state, &workspace, binding_id, updated.revision);
    lease.succeed();
    Ok(Json(updated))
}

pub(super) async fn delete_repository_credential(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<ExpectedRepositoryRevisionRequest>,
) -> ApiResult<Json<RepositoryBinding>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let (binding, workspace) = metadata_blocking({
        let metadata = metadata.clone();
        move || {
            let binding = metadata
                .repository_binding_for_principal(binding_id, actor, true)?
                .binding;
            let workspace =
                metadata.get_workspace_for_principal(binding.workspace_id, actor, true)?;
            Ok::<_, ApiError>((binding, workspace))
        }
    })
    .await?;
    authorize_vcs_operation(
        &state,
        &auth,
        binding.workspace_id,
        binding_id,
        VcsAction::RemoveCredential,
    )?;
    let lease = RepositoryMutationLease::acquire(
        &state,
        &workspace,
        binding_id,
        actor,
        VcsAction::RemoveCredential,
    )
    .await;
    let updated = metadata
        .delete_repository_credential(binding_id, actor, req.expected_revision)
        .await?
        .binding;
    push_vcs_operation(
        &state,
        actor,
        VcsAction::RemoveCredential,
        updated.workspace_id,
        binding_id,
    );
    publish_repository_changed(&state, &workspace, binding_id, updated.revision);
    lease.succeed();
    Ok(Json(updated))
}

pub(super) async fn test_repository_credential(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<VcsCredentialTestRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, false).await?;
    authorize_vcs_operation(
        &state,
        &auth,
        context.workspace.id,
        binding_id,
        VcsAction::TestCredential,
    )?;
    if context.record.binding.revision != req.expected_revision {
        return Err(ApiError::Conflict(
            "repository binding changed; refresh and retry".into(),
        ));
    }
    let credential = load_git_credential(&metadata, binding_id, actor).await?;
    context
        .adapter
        .test_remote_credential(&context.worktree, &req.remote, credential)
        .await
        .map_err(git_adapter_error)?;
    push_vcs_operation(
        &state,
        actor,
        VcsAction::TestCredential,
        context.workspace.id,
        binding_id,
    );
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn list_repository_remotes(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<VcsRemote>>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let context = load_repository_context(&state, metadata, actor, binding_id, false).await?;
    authorize_vcs_operation(
        &state,
        &auth,
        context.workspace.id,
        binding_id,
        VcsAction::Remotes,
    )?;
    let remotes = context
        .adapter
        .remotes(&context.worktree)
        .await
        .map_err(git_adapter_error)?;
    push_vcs_operation(
        &state,
        actor,
        VcsAction::Remotes,
        context.workspace.id,
        binding_id,
    );
    Ok(Json(remotes))
}

pub(super) async fn mutate_repository_remote(
    state: AppState,
    headers: HeaderMap,
    id: i64,
    expected_revision: u64,
    mutation: RepositoryRemoteMutation,
) -> ApiResult<Json<RepositoryBinding>> {
    use crate::git_adapter::VcsRepository as _;
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    let action = match &mutation {
        RepositoryRemoteMutation::Add { .. } => VcsAction::AddRemote,
        RepositoryRemoteMutation::Update { .. } => VcsAction::EditRemote,
        RepositoryRemoteMutation::Rename { .. } => VcsAction::EditRemote,
        RepositoryRemoteMutation::Remove { .. } => VcsAction::RemoveRemote,
    };
    authorize_vcs_operation(&state, &auth, context.workspace.id, binding_id, action)?;
    let lease =
        RepositoryMutationLease::acquire(&state, &context.workspace, binding_id, actor, action)
            .await;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    if context.record.binding.revision != expected_revision {
        return Err(ApiError::Conflict(
            "repository binding changed; refresh and retry".into(),
        ));
    }
    match mutation {
        RepositoryRemoteMutation::Add { name, url } => {
            context
                .adapter
                .add_remote(&context.worktree, &name, &url)
                .await
        }
        RepositoryRemoteMutation::Update { name, url } => {
            context
                .adapter
                .set_remote_url(&context.worktree, &name, &url)
                .await
        }
        RepositoryRemoteMutation::Rename { old, new } => {
            context
                .adapter
                .rename_remote(&context.worktree, &old, &new)
                .await
        }
        RepositoryRemoteMutation::Remove { name } => {
            context
                .adapter
                .remove_remote(&context.worktree, &name)
                .await
        }
    }
    .map_err(git_adapter_error)?;
    let observation = context
        .adapter
        .discover(&context.worktree)
        .await
        .map_err(git_adapter_error)?;
    let updated = observe_repository_after_mutation(
        metadata,
        binding_id,
        actor,
        expected_revision,
        observation,
    )
    .await?;
    push_vcs_operation(&state, actor, action, context.workspace.id, binding_id);
    publish_repository_changed(
        &state,
        &context.workspace,
        binding_id,
        updated.binding.revision,
    );
    lease.succeed();
    Ok(Json(updated.binding))
}

pub(super) async fn add_repository_remote(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<VcsRemoteMutationRequest>,
) -> ApiResult<Json<RepositoryBinding>> {
    mutate_repository_remote(
        state,
        headers,
        id,
        req.expected_revision,
        RepositoryRemoteMutation::Add {
            name: req.name,
            url: req.url,
        },
    )
    .await
}

pub(super) async fn update_repository_remote(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<VcsRemoteMutationRequest>,
) -> ApiResult<Json<RepositoryBinding>> {
    mutate_repository_remote(
        state,
        headers,
        id,
        req.expected_revision,
        RepositoryRemoteMutation::Update {
            name: req.name,
            url: req.url,
        },
    )
    .await
}

pub(super) async fn rename_repository_remote(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<VcsRemoteRenameRequest>,
) -> ApiResult<Json<RepositoryBinding>> {
    mutate_repository_remote(
        state,
        headers,
        id,
        req.expected_revision,
        RepositoryRemoteMutation::Rename {
            old: req.old_name,
            new: req.new_name,
        },
    )
    .await
}

pub(super) async fn remove_repository_remote(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<VcsRemoteDeleteRequest>,
) -> ApiResult<Json<RepositoryBinding>> {
    mutate_repository_remote(
        state,
        headers,
        id,
        req.expected_revision,
        RepositoryRemoteMutation::Remove { name: req.name },
    )
    .await
}

pub(super) async fn load_git_credential(
    metadata: &MetadataStore,
    binding_id: RepositoryBindingId,
    actor: PrincipalId,
) -> ApiResult<crate::git_adapter::GitCredential> {
    let Some(mut secret) = metadata.repository_credential(binding_id, actor).await? else {
        return Ok(crate::git_adapter::GitCredential {
            username: String::new(),
            password: String::new(),
        });
    };
    let stored: StoredGitCredential = serde_json::from_slice(&secret)
        .map_err(|_| ApiError::Internal("stored repository credential is invalid".into()))?;
    secret.fill(0);
    Ok(crate::git_adapter::GitCredential {
        username: stored.username,
        password: stored.password,
    })
}

pub(super) async fn fetch_repository(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<VcsRemoteRequest>,
) -> ApiResult<Json<VcsRemoteResult>> {
    remote_repository_operation(state, headers, id, req, false).await
}

pub(super) async fn push_repository(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<VcsRemoteRequest>,
) -> ApiResult<Json<VcsRemoteResult>> {
    remote_repository_operation(state, headers, id, req, true).await
}

pub(super) async fn remote_repository_operation(
    state: AppState,
    headers: HeaderMap,
    id: i64,
    req: VcsRemoteRequest,
    push: bool,
) -> ApiResult<Json<VcsRemoteResult>> {
    use crate::git_adapter::VcsRepository as _;
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    let action = if push {
        VcsAction::Push
    } else {
        VcsAction::Fetch
    };
    authorize_vcs_operation(
        &state,
        &auth,
        context.record.binding.workspace_id,
        binding_id,
        action,
    )?;
    let lease =
        RepositoryMutationLease::acquire(&state, &context.workspace, binding_id, actor, action)
            .await;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    if context.record.binding.revision != req.expected_revision {
        return Err(sift_metadata::MetadataError::RepositoryRevisionConflict {
            expected: req.expected_revision,
            current: context.record.binding.revision,
        }
        .into());
    }
    let before_refs = context
        .adapter
        .branches(&context.worktree)
        .await
        .map_err(git_adapter_error)?;
    let credential = load_git_credential(&metadata, binding_id, actor).await?;
    let observation = if push {
        context
            .adapter
            .push(
                &context.worktree,
                &req.remote,
                req.branch.as_deref(),
                credential,
            )
            .await
    } else {
        context
            .adapter
            .fetch(&context.worktree, &req.remote, credential)
            .await
    }
    .map_err(git_adapter_error)?;
    let head = observation.head.clone();
    let after_refs = context
        .adapter
        .branches(&context.worktree)
        .await
        .map_err(git_adapter_error)?;
    let before_heads = before_refs
        .into_iter()
        .filter_map(|branch| branch.head.map(|head| (branch.name, head)))
        .collect::<std::collections::BTreeMap<_, _>>();
    let after_heads = after_refs
        .into_iter()
        .filter_map(|branch| branch.head.map(|head| (branch.name, head)))
        .collect::<std::collections::BTreeMap<_, _>>();
    let ref_names = before_heads
        .keys()
        .chain(after_heads.keys())
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let ref_changes = ref_names
        .into_iter()
        .filter_map(|name| {
            let before = before_heads.get(&name).cloned();
            let after = after_heads.get(&name).cloned();
            (before != after).then_some(sift_protocol::VcsRefChange {
                name,
                before,
                after,
            })
        })
        .collect::<Vec<_>>();
    let updated_refs = ref_changes
        .iter()
        .map(|change| change.name.clone())
        .collect();
    metadata_blocking(move || {
        metadata
            .observe_repository(
                binding_id,
                actor,
                sift_metadata::RepositoryObservation {
                    expected_revision: req.expected_revision,
                    branch: observation.branch,
                    head: observation.head,
                },
            )
            .map_err(Into::into)
    })
    .await?;
    push_vcs_operation(
        &state,
        actor,
        action,
        context.record.binding.workspace_id,
        binding_id,
    );
    publish_repository_changed(
        &state,
        &context.workspace,
        binding_id,
        req.expected_revision.saturating_add(1),
    );
    lease.succeed();
    Ok(Json(VcsRemoteResult {
        binding_id,
        operation: if push { "push" } else { "fetch" }.into(),
        head,
        updated_refs,
        ref_changes,
    }))
}

pub(super) async fn hosting_identity(
    context: &RepositoryContext,
    remote_name: Option<&str>,
) -> ApiResult<sift_protocol::HostingRepositoryIdentity> {
    let remotes = context
        .adapter
        .remotes(&context.worktree)
        .await
        .map_err(git_adapter_error)?;
    let selected = remote_name
        .and_then(|name| remotes.iter().find(|remote| remote.name == name))
        .or_else(|| remotes.iter().find(|remote| remote.name == "origin"))
        .or_else(|| remotes.first())
        .ok_or_else(|| ApiError::BadRequest("repository has no hosting remote".into()))?;
    crate::hosting::detect_repository(&selected.fetch_url).map_err(hosting_error)
}

pub(super) fn hosting_error(error: crate::hosting::HostingError) -> ApiError {
    use crate::hosting::HostingError;
    match error {
        HostingError::CredentialRequired
        | HostingError::UnsupportedRemote
        | HostingError::UnsupportedOperation => ApiError::BadRequest(error.to_string()),
        HostingError::Rejected(401 | 403) => {
            ApiError::Forbidden("hosting provider rejected credential or permissions".into())
        }
        HostingError::Rejected(404) => {
            ApiError::BadRequest("hosting repository not found or not visible".into())
        }
        HostingError::Rejected(_)
        | HostingError::InvalidResponse
        | HostingError::ResponseTooLarge => ApiError::BadRequest(error.to_string()),
    }
}

pub(super) async fn get_repository_hosting(
    State(state): State<AppState>,
    Extension(hosting): Extension<crate::hosting::HostingHttp>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(query): Query<HostingQuery>,
) -> ApiResult<Json<HostingRepositorySummary>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, false).await?;
    authorize_vcs_operation(
        &state,
        &auth,
        context.workspace.id,
        binding_id,
        VcsAction::HostingRead,
    )?;
    let identity = hosting_identity(&context, query.remote.as_deref()).await?;
    let credential = metadata
        .repository_hosting_credential(binding_id, actor)
        .await?
        .map(zeroize::Zeroizing::new);
    let credential_present = credential.is_some();
    let links = crate::hosting::browser_links(
        &identity,
        context.record.binding.branch.as_deref(),
        context.record.binding.head.as_deref(),
        query.path.as_deref(),
    );
    let provider = crate::hosting::provider(identity.provider);
    let client = hosting.client().map_err(hosting_error)?;
    let credential_bytes = credential.as_ref().map(|secret| secret.as_slice());
    let pull_requests = async {
        Ok::<_, ApiError>(if context.record.binding.network_enabled {
            match context.record.binding.branch.as_deref() {
                Some(branch) => provider
                    .pull_requests(client, credential_bytes, &identity, branch)
                    .await
                    .map_err(hosting_error)?,
                None => Vec::new(),
            }
        } else {
            Vec::new()
        })
    };
    let checks = async {
        Ok::<_, ApiError>(if context.record.binding.network_enabled {
            match context.record.binding.head.as_deref() {
                Some(head) => provider
                    .checks(client, credential_bytes, &identity, head)
                    .await
                    .map_err(hosting_error)?,
                None => Vec::new(),
            }
        } else {
            Vec::new()
        })
    };
    let (pull_requests, checks) = tokio::try_join!(pull_requests, checks)?;
    push_vcs_operation(
        &state,
        actor,
        VcsAction::HostingRead,
        context.workspace.id,
        binding_id,
    );
    Ok(Json(HostingRepositorySummary {
        identity,
        credential_present,
        links,
        pull_requests,
        checks,
    }))
}

pub(super) async fn list_hosting_repositories(
    State(state): State<AppState>,
    Extension(hosting): Extension<crate::hosting::HostingHttp>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(query): Query<HostingQuery>,
) -> ApiResult<Json<Vec<HostingRepositoryCandidate>>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, false).await?;
    authorize_vcs_operation(
        &state,
        &auth,
        context.workspace.id,
        binding_id,
        VcsAction::HostingRead,
    )?;
    if !context.record.binding.network_enabled {
        return Err(ApiError::Forbidden(
            "network Git and hosting operations are disabled".into(),
        ));
    }
    let identity = hosting_identity(&context, query.remote.as_deref()).await?;
    let credential = zeroize::Zeroizing::new(
        metadata
            .repository_hosting_credential(binding_id, actor)
            .await?
            .ok_or_else(|| ApiError::BadRequest("hosting credential is required".into()))?,
    );
    let result = crate::hosting::provider(identity.provider)
        .repositories(hosting.client().map_err(hosting_error)?, &credential)
        .await
        .map_err(hosting_error);
    let repositories = result?;
    push_vcs_operation(
        &state,
        actor,
        VcsAction::HostingRead,
        context.workspace.id,
        binding_id,
    );
    Ok(Json(repositories))
}

pub(super) async fn set_hosting_credential(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(request): Json<sift_protocol::SetHostingCredentialRequest>,
) -> ApiResult<Json<RepositoryBinding>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    authorize_vcs_operation(
        &state,
        &auth,
        context.workspace.id,
        binding_id,
        VcsAction::SetHostingCredential,
    )?;
    let lease = RepositoryMutationLease::acquire(
        &state,
        &context.workspace,
        binding_id,
        actor,
        VcsAction::SetHostingCredential,
    )
    .await;
    let mut token = request.token.0.into_bytes();
    let result = metadata
        .set_repository_hosting_credential(binding_id, actor, request.expected_revision, &token)
        .await;
    token.fill(0);
    let binding = result?.binding;
    push_vcs_operation(
        &state,
        actor,
        VcsAction::SetHostingCredential,
        binding.workspace_id,
        binding_id,
    );
    publish_repository_changed(&state, &context.workspace, binding_id, binding.revision);
    lease.succeed();
    Ok(Json(binding))
}

pub(super) async fn delete_hosting_credential(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(request): Json<ExpectedRepositoryRevisionRequest>,
) -> ApiResult<Json<RepositoryBinding>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    authorize_vcs_operation(
        &state,
        &auth,
        context.workspace.id,
        binding_id,
        VcsAction::RemoveHostingCredential,
    )?;
    let lease = RepositoryMutationLease::acquire(
        &state,
        &context.workspace,
        binding_id,
        actor,
        VcsAction::RemoveHostingCredential,
    )
    .await;
    let binding = metadata
        .delete_repository_hosting_credential(binding_id, actor, request.expected_revision)
        .await?
        .binding;
    push_vcs_operation(
        &state,
        actor,
        VcsAction::RemoveHostingCredential,
        binding.workspace_id,
        binding_id,
    );
    publish_repository_changed(&state, &context.workspace, binding_id, binding.revision);
    lease.succeed();
    Ok(Json(binding))
}

pub(super) async fn create_hosting_pull_request(
    State(state): State<AppState>,
    Extension(hosting): Extension<crate::hosting::HostingHttp>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(request): Json<sift_protocol::CreateHostingPullRequestRequest>,
) -> ApiResult<Json<sift_protocol::HostingPullRequest>> {
    if request.title.trim().is_empty()
        || request.title.len() > 256
        || request
            .body
            .as_ref()
            .is_some_and(|body| body.len() > 64 * 1024)
        || !crate::hosting::validate_ref(&request.head_branch)
        || !crate::hosting::validate_ref(&request.base_branch)
    {
        return Err(ApiError::BadRequest("invalid pull request fields".into()));
    }
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let binding_id = repository_binding_id(id)?;
    let actor = auth.principal_id;
    let context =
        load_repository_context(&state, metadata.clone(), actor, binding_id, true).await?;
    authorize_vcs_operation(
        &state,
        &auth,
        context.workspace.id,
        binding_id,
        VcsAction::CreatePullRequest,
    )?;
    if !context.record.binding.network_enabled {
        return Err(ApiError::Forbidden(
            "network Git and hosting operations are disabled".into(),
        ));
    }
    if context.record.binding.revision != request.expected_revision
        || context.record.binding.branch.as_deref() != Some(request.head_branch.as_str())
    {
        return Err(ApiError::Conflict(
            "repository branch or revision changed; refresh and retry".into(),
        ));
    }
    let lease = RepositoryMutationLease::acquire(
        &state,
        &context.workspace,
        binding_id,
        actor,
        VcsAction::CreatePullRequest,
    )
    .await;
    let identity = hosting_identity(&context, None).await?;
    let credential = zeroize::Zeroizing::new(
        metadata
            .repository_hosting_credential(binding_id, actor)
            .await?
            .ok_or_else(|| ApiError::BadRequest("hosting credential is required".into()))?,
    );
    let result = crate::hosting::provider(identity.provider)
        .create_pull_request(
            hosting.client().map_err(hosting_error)?,
            &credential,
            &identity,
            crate::hosting::PullRequestDraft {
                title: request.title.trim(),
                body: request.body.as_deref(),
                head: &request.head_branch,
                base: &request.base_branch,
            },
        )
        .await
        .map_err(hosting_error);
    let pull = result?;
    push_vcs_operation(
        &state,
        actor,
        VcsAction::CreatePullRequest,
        context.workspace.id,
        binding_id,
    );
    lease.succeed();
    Ok(Json(pull))
}

pub(super) fn load_projection_inputs(
    metadata: &MetadataStore,
    rooms: &RoomRuntime,
    adapter: &crate::workspace_adapter::RootedFilesystemAdapter,
    binding_id: sift_protocol::ProjectionBindingId,
    actor: PrincipalId,
    writable: bool,
) -> ApiResult<ProjectionInputs> {
    use crate::workspace_adapter::WorkspaceAdapter as _;
    let binding = metadata.projection_binding_for_principal(binding_id, actor, writable)?;
    if binding.binding.adapter_generation != adapter.generation() {
        return Err(ApiError::BadRequest(
            "workspace projection adapter generation changed; rebind it".into(),
        ));
    }
    let workspace =
        metadata.get_workspace_for_principal(binding.binding.workspace_id, actor, writable)?;
    let nodes = metadata.list_workspace_nodes_for_principal(workspace.id, actor)?;
    let mut files = Vec::new();
    let mut captures = Vec::new();
    for node in nodes
        .into_iter()
        .filter(|node| node.kind == WorkspaceNodeKind::SqlDocument)
    {
        let document = DocumentId(
            node.document_id
                .ok_or(sift_metadata::MetadataError::InvalidWorkspaceNode)?,
        );
        let document_actor = rooms
            .documents()
            .get_or_load(metadata, document)
            .map_err(workspace_actor_error)?;
        let guard = document_actor
            .lock()
            .map_err(|_| ApiError::Internal("document actor mutex poisoned".into()))?;
        let bytes = guard.text().into_bytes();
        captures.push(WorkspaceCheckpointCapture {
            node_id: node.id,
            snapshot_bytes: guard.snapshot().map_err(workspace_actor_error)?,
            snapshot_version: guard.version_vector(),
        });
        files.push(crate::workspace_projection::WorkspaceProjectionFile {
            node_id: node.id,
            path: node.path,
            digest: format!("{:x}", Sha256::digest(&bytes)),
            bytes,
        });
    }
    files.sort_by(|left, right| left.path.0.cmp(&right.path.0));
    let projection = adapter
        .scan(&binding.root_handle)
        .map_err(workspace_adapter_error)?;
    let baseline = metadata.projection_file_state_for_principal(binding_id, actor)?;
    Ok(ProjectionInputs {
        binding,
        workspace,
        files,
        projection,
        baseline,
        captures,
    })
}

pub(super) async fn reconcile_vcs_worktree_path(
    state: &AppState,
    metadata: MetadataStore,
    actor: PrincipalId,
    binding_id: sift_protocol::ProjectionBindingId,
    path: &sift_protocol::WorkspacePath,
) -> ApiResult<sift_metadata::WorkspaceRecord> {
    let adapter = state.rooms.workspace_adapter().ok_or_else(|| {
        ApiError::BadRequest("workspace filesystem projections are disabled".into())
    })?;
    let rooms = state.rooms.clone();
    let path = path.clone();
    let (workspace, broadcast) = metadata_blocking(move || {
        let current = load_projection_inputs(&metadata, &rooms, &adapter, binding_id, actor, true)?;
        let plan = crate::workspace_projection::reconcile_plan(
            &current.binding.binding,
            current.workspace.revision,
            &current.baseline,
            &current.files,
            &current.projection,
        );
        let entry = plan
            .entries
            .iter()
            .find(|entry| entry.path == path)
            .cloned();
        let Some(entry) = entry else {
            return Ok::<_, ApiError>((current.workspace, None));
        };
        let projected_path = entry.previous_path.as_ref().unwrap_or(&entry.path);
        let projected = current
            .projection
            .files
            .iter()
            .find(|projected| projected.path == *projected_path);
        let broadcast = import_projection_resolution(
            &metadata,
            &rooms,
            actor,
            current.workspace.id,
            &entry,
            projected,
            false,
        )?;
        let observed =
            load_projection_inputs(&metadata, &rooms, &adapter, binding_id, actor, true)?;
        let mut baseline = observed
            .baseline
            .iter()
            .filter(|file| file.path != path)
            .cloned()
            .collect::<Vec<_>>();
        let workspace_file = observed.files.iter().find(|file| file.path == path);
        let projection_file = observed
            .projection
            .files
            .iter()
            .find(|file| file.path == path);
        if workspace_file.is_some() || projection_file.is_some() {
            baseline.push(ProjectionFileState {
                node_id: workspace_file.map(|file| file.node_id),
                path: path.clone(),
                workspace_digest: workspace_file.map(|file| file.digest.clone()),
                projection_digest: projection_file.map(|file| file.digest.clone()),
            });
        }
        baseline.sort_by(|left, right| left.path.0.cmp(&right.path.0));
        metadata.commit_projection_observation(
            binding_id,
            actor,
            observed.binding.binding.revision,
            observed.workspace.revision,
            match observed.binding.binding.mode {
                ProjectionMode::ReadOnly => ProjectionHealth::ReadOnly,
                ProjectionMode::ReadWrite => ProjectionHealth::Ready,
            },
            &baseline,
        )?;
        Ok::<_, ApiError>((observed.workspace, broadcast))
    })
    .await?;
    if let Some((document, replica_id, update_id, server_seq, update, version)) = broadcast {
        state.rooms.publish_doc(
            workspace.room_id.0,
            RoomServerMessage::DocumentUpdateCommitted {
                document_id: document.0,
                replica_id: sift_protocol::ReplicaId(replica_id),
                server_seq,
                update: sift_protocol::CrdtUpdate::new(update),
                server_version: sift_protocol::DocumentVersion::new(version),
            },
        );
        state.sessions.push_operation_full(
            Operation::ApplyDocumentUpdate {
                room_id: workspace.room_id.0,
                document_id: document.0,
                update_id,
                server_seq,
            },
            OperationStatus::Succeeded,
            Some(actor.0),
            None,
            None,
            None,
        );
    }
    Ok(workspace)
}
