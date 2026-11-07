//! Captured row-edit execution and settlement independent of its HTTP observer.
use super::*;

pub(super) async fn post_edits_apply(
    State(state): State<AppState>,
    headers: HeaderMap,
    auth: Option<Extension<AuthContext>>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Json(request): Json<sift_protocol::ApplyEditsRequest>,
) -> ApiResult<Json<sift_protocol::ApplyEditsResult>> {
    let actor = auth
        .as_ref()
        .map(|Extension(auth)| auth.principal_id.0)
        .or_else(|| {
            state
                .sessions
                .session_owner(session)
                .ok()
                .flatten()
                .map(|actor| actor.0)
        });
    let correlation = crate::correlation::current().unwrap_or_else(crate::correlation::generate);
    let mut audit = crate::correlation::sync_scope(correlation.clone(), || {
        query_audit::QueryOperationAudit::new(
            state.sessions.clone(),
            Operation::ApplyEdits {
                session,
                connection,
            },
            actor,
        )
    });
    let prepared = (|| {
        if state.shutdown.is_draining() {
            return Err(ApiError::ServiceDraining);
        }
        if request.connection != connection {
            return Err(ApiError::BadRequest(
                "`connection` in body must match the path connection".into(),
            ));
        }
        let permit = state.sessions.reserve_query_settlement()?;
        let source = state.sessions.authorize_connection_operation(
            session,
            connection,
            sift_protocol::OperationKind::ApplyEdits,
            None,
            &[&request.edit_set.table],
        )?;
        Ok((permit, source))
    })();
    let (permit, source) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            audit.failure(&error);
            return Err(error);
        }
    };
    let guard = Arc::new(state.shutdown.track_query());
    tokio::spawn(crate::correlation::scope(
        correlation,
        crate::shutdown::scope_query(Some(guard), async move {
            let _permit = permit;
            let result = apply_owned(&state, headers, session, request, source).await;
            match &result {
                Ok(response) => audit.record(
                    OperationStatus::Succeeded,
                    None,
                    i64::try_from(response.applied.len()).ok(),
                    None,
                ),
                Err(error) => audit.failure(error),
            }
            result.map(Json)
        }),
    ))
    .await
    .map_err(|_| ApiError::Internal("grid settlement owner failed".into()))?
}

async fn apply_owned(
    state: &AppState,
    headers: HeaderMap,
    session: sift_protocol::SessionId,
    request: sift_protocol::ApplyEditsRequest,
    source: crate::session::ConnectionEntryClone,
) -> ApiResult<sift_protocol::ApplyEditsResult> {
    let scope = if state.metadata.is_some() {
        let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
        ws_query::actor_scope(Some(&auth), &source)?
    } else {
        None
    };
    let edit_set = request.edit_set.clone();
    let attempt = state
        .sessions
        .apply_edits_captured(session, request, source)
        .await;
    let transaction_id = attempt.transaction_id.map(|tx| tx.0.to_string());
    if let Some(scope) = scope {
        for kind in attempt.attempted {
            let operation = match kind {
                sift_protocol::EditStatementKind::Insert => {
                    sift_protocol::ChangeLedgerOperation::GridInsert
                }
                sift_protocol::EditStatementKind::Update => {
                    sift_protocol::ChangeLedgerOperation::GridUpdate
                }
                sift_protocol::EditStatementKind::Delete => {
                    sift_protocol::ChangeLedgerOperation::GridDelete
                }
            };
            let (row_count, outcome, result_code) = match &attempt.result {
                Ok(result) => (
                    result
                        .applied
                        .iter()
                        .filter(|entry| entry.kind == kind)
                        .try_fold(0i64, |sum, entry| {
                            sum.checked_add(i64::try_from(entry.affected_rows).ok()?)
                        }),
                    if result.committed {
                        sift_protocol::ChangeLedgerOutcome::Committed
                    } else {
                        sift_protocol::ChangeLedgerOutcome::Partial
                    },
                    None,
                ),
                Err(ApiError::EditConflict { .. }) => (
                    None,
                    sift_protocol::ChangeLedgerOutcome::Conflicted,
                    Some("edit_conflict".into()),
                ),
                Err(ApiError::Driver(error)) if error.code == sift_protocol::Code::EditConflict => {
                    (
                        None,
                        sift_protocol::ChangeLedgerOutcome::Conflicted,
                        Some(error.code.to_string()),
                    )
                }
                Err(error) => (
                    None,
                    if attempt.rollback == Some(true) {
                        sift_protocol::ChangeLedgerOutcome::RolledBack
                    } else if matches!(
                        error,
                        ApiError::SessionNotFound(_)
                            | ApiError::ConnectionNotFound(_)
                            | ApiError::ServiceDraining
                            | ApiError::TenantResourceExhausted { .. }
                    ) {
                        sift_protocol::ChangeLedgerOutcome::OutcomeUnknown
                    } else {
                        query_audit::write_outcome(error)
                    },
                    query_audit::error_details(error).0,
                ),
            };
            if record_database_change(
                state,
                scope.actor,
                DatabaseChangeRecord {
                    workspace_id: None,
                    tenant_id: scope.tenant_id,
                    room_id: None,
                    connection_profile_id: scope.connection_profile_id,
                    database_target: scope.database_target.clone(),
                    operation,
                    affected_object: Some(object_path_label(&edit_set.table)),
                    row_count,
                    sql_fingerprint: None,
                    row_identity_fingerprint: Some(redacted_fingerprint(&edit_set.edits)),
                    transaction_id: transaction_id.clone(),
                    source_workflow: "data_grid",
                    approved_by: Some(scope.actor.0),
                    outcome,
                    result_code,
                    versioned_source: None,
                },
            )
            .await
            .is_err()
            {
                tracing::error!("failed to append grid change ledger entry");
            }
        }
    }
    attempt.result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn grid_settlement_admission_refusal_is_audited_before_preparation() {
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
        let request=serde_json::from_value(serde_json::json!({"connection":2,"edit_set":{"table":{"schema":"public","name":"users","kind":"table"},"edits":[]}})).unwrap();
        let result = post_edits_apply(
            State(state.clone()),
            HeaderMap::new(),
            None,
            Path((sift_protocol::SessionId(1), sift_protocol::ConnectionId(2))),
            Json(request),
        )
        .await;
        assert!(result.is_err());
        let operations = state.sessions.list_operations();
        assert_eq!(operations.len(), 1);
        assert!(matches!(
            operations[0].operation,
            Operation::ApplyEdits { .. }
        ));
        assert_eq!(operations[0].status, OperationStatus::Failed);
        assert_eq!(state.shutdown.in_flight(), 0);
    }
}
