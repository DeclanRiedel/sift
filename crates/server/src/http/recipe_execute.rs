//! Recipe source capture, current authority and terminal ownership.
use super::*;

pub(super) async fn execute_transfer_recipe(
    State(state): State<AppState>,
    headers: HeaderMap,
    auth: Option<Extension<AuthContext>>,
    Path(id): Path<i64>,
    Json(request): Json<ExecuteTransferRecipeRequest>,
) -> ApiResult<Json<sift_protocol::TransferExecutionResult>> {
    let actor = auth
        .as_ref()
        .map(|Extension(auth)| auth.principal_id.0)
        .or_else(|| {
            state
                .sessions
                .session_owner(request.session_id)
                .ok()
                .flatten()
                .map(|actor| actor.0)
        });
    let correlation = crate::correlation::current().unwrap_or_else(crate::correlation::generate);
    let mut audit = crate::correlation::sync_scope(correlation.clone(), || {
        query_audit::QueryOperationAudit::new(
            state.sessions.clone(),
            Operation::ExecuteTransferRecipe {
                recipe_id: TransferRecipeId(id),
            },
            actor,
        )
    });
    let prepared = (|| {
        if state.shutdown.is_draining() {
            return Err(ApiError::ServiceDraining);
        }
        let id = transfer_recipe_id(id)?;
        let permit = state.sessions.reserve_query_settlement()?;
        let source = state
            .sessions
            .conn_entry(request.session_id, request.connection_id)?;
        let sessions =
            state
                .sessions
                .bind_source(request.session_id, request.connection_id, source)?;
        Ok((id, permit, sessions))
    })();
    let (id, permit, sessions) = match prepared {
        Ok(value) => value,
        Err(error) => {
            audit.failure(&error);
            return Err(error);
        }
    };
    let guard = Arc::new(state.shutdown.track_query());
    tokio::spawn(crate::correlation::scope(
        correlation,
        crate::shutdown::scope_query(Some(guard), async move {
            let permit = Arc::new(permit);
            let result =
                execute_owned(&state, sessions, headers, id, request, permit.clone()).await;
            match &result {
                Ok(result) => audit.record(
                    OperationStatus::Succeeded,
                    None,
                    match result {
                        sift_protocol::TransferExecutionResult::Import { result, .. } => {
                            i64::try_from(result.rows_inserted).ok()
                        }
                        _ => None,
                    },
                    None,
                ),
                Err(error) => audit.failure(error),
            }
            result.map(Json)
        }),
    ))
    .await
    .map_err(|_| ApiError::Internal("recipe settlement owner failed".into()))?
}

async fn execute_owned(
    state: &AppState,
    sessions: SessionStore,
    headers: HeaderMap,
    id: TransferRecipeId,
    request: ExecuteTransferRecipeRequest,
    permit: Arc<tokio::sync::OwnedSemaphorePermit>,
) -> ApiResult<sift_protocol::TransferExecutionResult> {
    let metadata = metadata_store_cloned(state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let actor = auth.principal_id;
    let captured = sessions
        .csv_execution()
        .ok_or_else(|| ApiError::Internal("recipe source binding unavailable".into()))?;
    let scope = ws_query::actor_scope(Some(&auth), &captured.source)?;
    let (recipe, tenant) = metadata_blocking({
        let metadata = metadata.clone();
        move || {
            let recipe = metadata.transfer_recipe_for_principal(id, actor, false)?;
            let workspace =
                metadata.get_workspace_for_principal(recipe.workspace_id, actor, false)?;
            let tenant = metadata.get_room(workspace.room_id)?.tenant_id;
            Ok((recipe, tenant))
        }
    })
    .await?;
    if scope
        .as_ref()
        .and_then(|scope| scope.tenant_id)
        .is_some_and(|source| source != tenant)
    {
        return Err(ApiError::BadRequest(
            "recipe workspace belongs to another native source tenant".into(),
        ));
    }
    authorize_transfer_operation(
        state,
        &auth,
        recipe.workspace_id,
        Some(id),
        TransferRecipeAction::Execute,
    )?;
    validate_transfer_format(
        state,
        recipe.direction,
        &recipe.format_id,
        &recipe.format_version,
        &recipe.options,
    )?;
    let workspace = recipe.workspace_id;
    let revision = recipe.revision;
    let authority: crate::session::SourceAuthority = Arc::new({
        let state = state.clone();
        let metadata = metadata.clone();
        move || {
            metadata.check_transfer_recipe_revision(id, actor, revision)?;
            authorize_transfer_operation(
                &state,
                &auth,
                workspace,
                Some(id),
                TransferRecipeAction::Execute,
            )
        }
    });
    let sessions = sessions.with_source_authority(authority);
    let import = recipe.direction == sift_protocol::TransferDirection::Import;
    let table = request
        .table
        .as_ref()
        .map(object_path_label)
        .unwrap_or_default();
    let kind = if import {
        if recipe.format_id == "parquet" {
            sift_protocol::OperationKind::ExecuteTransferRecipe
        } else {
            sift_protocol::OperationKind::ImportCsv
        }
    } else {
        sift_protocol::OperationKind::ExportQuery
    };
    sessions.authorize_connection_operation(
        request.session_id,
        request.connection_id,
        kind,
        if import { None } else { request.sql.as_deref() },
        &request
            .table
            .as_ref()
            .filter(|_| import)
            .into_iter()
            .collect::<Vec<_>>(),
    )?;
    let result = if import {
        crate::transfer::execute_recipe(&sessions, &metadata, actor, &recipe, request).await
    } else {
        use crate::session::export_execution::{self, ExportExecution, StartupGuard};
        let observation = Arc::new(ExportExecution::default());
        let startup = StartupGuard(observation.clone());
        let sql = request.sql.as_deref().unwrap_or_default();
        export_settlement::ExportLedger {
            scope: scope.clone(),
            workspace: Some(workspace),
            effect: crate::sql_policy::export_effect(captured.source.driver.semantic_engine(), sql),
            fingerprint: crate::fingerprint::sql(sql),
            workflow: "transfer_recipe_export",
        }
        .spawn(state.clone(), observation.clone(), permit);
        let result = export_execution::scope(
            observation,
            crate::transfer::execute_recipe(&sessions, &metadata, actor, &recipe, request),
        )
        .await;
        drop(startup);
        result
    };
    if import {
        let observation = captured.observation();
        let complete = observation.import_completed;
        csv_apply::settle_import_changes(
            state,
            csv_apply::ImportLedgerContext {
                scope,
                table,
                workspace_id: Some(workspace),
                workflow: "transfer_recipe_import",
                native_complete: complete,
            },
            observation,
            result.as_ref().err(),
        )
        .await;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn recipe_settlement_admission_refusal_audits_the_requested_recipe() {
        let state = AppState {
            sessions: SessionStore::new(crate::DriverRegistry::new()),
            rooms: Default::default(),
            shutdown: Default::default(),
            auth: Default::default(),
            metadata: None,
        };
        let mut permits = Vec::new();
        while let Ok(permit) = state.sessions.reserve_query_settlement() {
            permits.push(permit);
        }
        let request = serde_json::from_value(
            serde_json::json!({"session_id":1,"connection_id":2,"dry_run":true}),
        )
        .unwrap();
        let result = execute_transfer_recipe(
            State(state.clone()),
            HeaderMap::new(),
            None,
            Path(42),
            Json(request),
        )
        .await;
        assert!(result.is_err());
        let operations = state.sessions.list_operations();
        assert_eq!(operations.len(), 1);
        assert!(
            matches!(operations[0].operation,Operation::ExecuteTransferRecipe {recipe_id} if recipe_id.0==42)
        );
        assert_eq!(operations[0].status, OperationStatus::Failed);
        assert_eq!(state.shutdown.in_flight(), 0);
    }
}
