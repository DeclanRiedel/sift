//! Bulk mutation settlement belongs to a bounded owner, independent of HTTP loss.
use super::*;
use crate::session::bulk_execution::BulkExecution;

pub(super) async fn bulk_insert(
    State(state): State<AppState>,
    headers: HeaderMap,
    auth: Option<Extension<AuthContext>>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Json(request): Json<BulkInsertRequest>,
) -> ApiResult<Json<sift_protocol::BulkInsertResponse>> {
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
            Operation::BulkInsert {
                session,
                connection,
                request: request.clone(),
            },
            actor,
        )
    });
    let prepared = (|| {
        if state.shutdown.is_draining() {
            return Err(ApiError::ServiceDraining);
        }
        let permit = state.sessions.reserve_query_settlement()?;
        let path = crate::csv_import::table_path(&request.table)?;
        let source = state.sessions.authorize_connection_operation(
            session,
            connection,
            sift_protocol::OperationKind::BulkInsert,
            None,
            &[&path],
        )?;
        Ok((permit, BulkExecution::new(source)))
    })();
    let (permit, context) = match prepared {
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
            let result = apply_owned(&state, headers, session, connection, request, context).await;
            match &result {
                Ok(response) => audit.record(
                    OperationStatus::Succeeded,
                    None,
                    i64::try_from(response.rows_inserted).ok(),
                    None,
                ),
                Err(error) => audit.failure(error),
            }
            result.map(Json)
        }),
    ))
    .await
    .map_err(|_| ApiError::Internal("bulk settlement owner failed".into()))?
}

async fn apply_owned(
    state: &AppState,
    headers: HeaderMap,
    session: sift_protocol::SessionId,
    connection: sift_protocol::ConnectionId,
    request: BulkInsertRequest,
    context: BulkExecution,
) -> ApiResult<sift_protocol::BulkInsertResponse> {
    let scope = if state.metadata.is_some() {
        let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
        ws_query::actor_scope(Some(&auth), &context.source)?
    } else {
        None
    };
    let table = request.table.clone();
    let result = state
        .sessions
        .bulk_insert_captured(session, connection, request, context.clone())
        .await;
    let observation = context.observation();
    if let Some(scope) = scope.filter(|_| observation.attempted) {
        let (row_count, outcome, result_code) = match &result {
            Ok(response) => (
                i64::try_from(response.rows_inserted).ok(),
                sift_protocol::ChangeLedgerOutcome::Committed,
                None,
            ),
            Err(error) => (
                None,
                if observation.rollback == Some(true) {
                    sift_protocol::ChangeLedgerOutcome::RolledBack
                } else if context.is_fenced() {
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
                database_target: scope.database_target,
                operation: sift_protocol::ChangeLedgerOperation::BulkMutation,
                affected_object: Some(table),
                row_count,
                sql_fingerprint: None,
                row_identity_fingerprint: None,
                transaction_id: observation.transaction_id.map(|tx| tx.0.to_string()),
                source_workflow: "bulk_insert",
                approved_by: Some(scope.actor.0),
                outcome,
                result_code,
                versioned_source: None,
            },
        )
        .await
        .is_err()
        {
            tracing::error!("failed to append bulk change ledger entry");
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn bulk_settlement_admission_refusal_is_audited_before_preparation() {
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
        let result = bulk_insert(
            State(state.clone()),
            HeaderMap::new(),
            None,
            Path((sift_protocol::SessionId(1), sift_protocol::ConnectionId(2))),
            Json(BulkInsertRequest {
                table: "users".into(),
                ..Default::default()
            }),
        )
        .await;
        assert!(result.is_err());
        let operations = state.sessions.list_operations();
        assert_eq!(operations.len(), 1);
        assert!(matches!(
            operations[0].operation,
            Operation::BulkInsert { .. }
        ));
        assert_eq!(operations[0].status, OperationStatus::Failed);
        assert_eq!(state.shutdown.in_flight(), 0);
    }
}
