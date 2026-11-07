//! Source-bound CSV settlement after observer loss, with distinct data/DDL effects.
use super::*;

pub(super) async fn import_csv(
    State(state): State<AppState>,
    headers: HeaderMap,
    auth: Option<Extension<AuthContext>>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Json(request): Json<CsvImportRequest>,
) -> ApiResult<Json<sift_protocol::CsvImportResponse>> {
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
            Operation::ImportCsv {
                session,
                connection,
                table: request.table.clone(),
                create_table: request.create_table,
                conflict_policy: request.conflict_policy,
            },
            actor,
        )
    });
    let prepared = (|| {
        if state.shutdown.is_draining() {
            return Err(ApiError::ServiceDraining);
        }
        let permit = state.sessions.reserve_query_settlement()?;
        let source = state.sessions.authorize_connection_operation(
            session,
            connection,
            sift_protocol::OperationKind::ImportCsv,
            None,
            &[&crate::csv_import::table_path(&request.table)?],
        )?;
        Ok((
            permit,
            state
                .sessions
                .bind_csv_source(session, connection, source)?,
        ))
    })();
    let (permit, sessions) = match prepared {
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
            let result = apply_owned(&state, sessions, headers, session, connection, request).await;
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
    .map_err(|_| ApiError::Internal("CSV settlement owner failed".into()))?
}
async fn apply_owned(
    state: &AppState,
    sessions: SessionStore,
    headers: HeaderMap,
    session: sift_protocol::SessionId,
    connection: sift_protocol::ConnectionId,
    request: CsvImportRequest,
) -> ApiResult<sift_protocol::CsvImportResponse> {
    let captured = sessions
        .csv_execution()
        .ok_or_else(|| ApiError::Internal("CSV source binding unavailable".into()))?;
    let scope = if state.metadata.is_some() {
        let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
        ws_query::actor_scope(Some(&auth), &captured.source)?
    } else {
        None
    };
    let table = request.table.clone();
    let result = crate::csv_import::import(&sessions, session, connection, request).await;
    let observation = captured.observation();
    if let Some(scope) = scope {
        for (ddl, phase) in [(false, observation.data), (true, observation.ddl)] {
            if !phase.attempted {
                continue;
            }
            let outcome = if !ddl
                && result.is_err()
                && phase.outcome == sift_protocol::ChangeLedgerOutcome::Committed
            {
                sift_protocol::ChangeLedgerOutcome::Partial
            } else {
                phase.outcome
            };
            let result_code = if outcome == sift_protocol::ChangeLedgerOutcome::Committed {
                None
            } else {
                result
                    .as_ref()
                    .err()
                    .and_then(|error| query_audit::error_details(error).0)
            };
            if record_database_change(
                state,
                scope.actor,
                DatabaseChangeRecord {
                    tenant_id: scope.tenant_id,
                    room_id: None,
                    connection_profile_id: scope.connection_profile_id,
                    database_target: scope.database_target.clone(),
                    operation: if ddl {
                        sift_protocol::ChangeLedgerOperation::DdlApply
                    } else {
                        sift_protocol::ChangeLedgerOperation::CsvImport
                    },
                    affected_object: Some(table.clone()),
                    row_count: if ddl {
                        None
                    } else {
                        phase.rows.and_then(|count| i64::try_from(count).ok())
                    },
                    sql_fingerprint: None,
                    row_identity_fingerprint: None,
                    transaction_id: phase.transaction_id.map(|tx| tx.0.to_string()),
                    source_workflow: "csv_import",
                    approved_by: Some(scope.actor.0),
                    outcome,
                    result_code,
                    versioned_source: None,
                },
            )
            .await
            .is_err()
            {
                tracing::error!("failed to append CSV change ledger entry");
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn csv_settlement_admission_refusal_is_audited_before_preparation() {
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
        let request =
            serde_json::from_value(serde_json::json!({"table":"users","data":b"id\n1\n".to_vec()}))
                .unwrap();
        let result = import_csv(
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
            Operation::ImportCsv { .. }
        ));
        assert_eq!(operations[0].status, OperationStatus::Failed);
        assert_eq!(state.shutdown.in_flight(), 0);
    }
}
