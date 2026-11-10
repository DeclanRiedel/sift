//! One captured native source and one bounded owner through ledger/audit settlement.
use super::*;

pub(super) async fn apply_migration(
    State(state): State<AppState>,
    headers: HeaderMap,
    auth: Option<Extension<AuthContext>>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Json(request): Json<sift_protocol::ApplyMigrationRequest>,
) -> ApiResult<Json<sift_protocol::MigrationRun>> {
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
            Operation::ApplyMigration {
                session,
                connection,
                plan_id: request.plan_id,
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
            sift_protocol::OperationKind::ApplyMigration,
            None,
            &[],
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
            let result = apply_owned(&state, headers, session, connection, request, source).await;
            match &result {
                Ok(run) if run.state == sift_protocol::MigrationRunState::Applied => audit.record(
                    OperationStatus::Succeeded,
                    None,
                    i64::try_from(run.outcomes.len()).ok(),
                    None,
                ),
                Ok(run) => {
                    let code = match run.state {
                        sift_protocol::MigrationRunState::Canceled => "migration_canceled",
                        sift_protocol::MigrationRunState::RolledBack => "migration_rolled_back",
                        sift_protocol::MigrationRunState::Partial => "migration_partial",
                        sift_protocol::MigrationRunState::Failed => "migration_failed",
                        _ => "migration_incomplete",
                    };
                    audit.record(
                        OperationStatus::Failed,
                        Some(code.into()),
                        None,
                        Some(
                            "migration did not complete a committed plan; inspect its run receipt"
                                .into(),
                        ),
                    );
                }
                Err(error) => audit.failure(error),
            }
            result.map(Json)
        }),
    ))
    .await
    .map_err(|_| ApiError::Internal("migration settlement owner failed".into()))?
}

async fn apply_owned(
    state: &AppState,
    headers: HeaderMap,
    session: sift_protocol::SessionId,
    connection: sift_protocol::ConnectionId,
    request: sift_protocol::ApplyMigrationRequest,
    source: crate::session::ConnectionEntryClone,
) -> ApiResult<sift_protocol::MigrationRun> {
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    if state
        .sessions
        .migration_plan_ai_proposal(request.plan_id)
        .is_some()
    {
        return Err(ApiError::BadRequest(
            "Apply AI migration plans through their human review and durable claim".into(),
        ));
    }
    let (principal, tenant, profile) = match &source.provenance {
        crate::session::ConnectionProvenance::Managed {
            principal_id,
            tenant_id,
            profile_id,
            ..
        } => (*principal_id, *tenant_id, *profile_id),
        _ => {
            return Err(ApiError::BadRequest(
                "migration apply requires a managed connection".into(),
            ))
        }
    };
    if principal != auth.principal_id {
        return Err(ApiError::Forbidden(
            "migration caller must own the managed session".into(),
        ));
    }
    ensure_tenant(&auth, tenant)?;
    let validated_source = match request.source.clone() {
        Some(source) => {
            let metadata = metadata_store_cloned(state)?;
            Some(
                metadata_blocking(move || {
                    let validated = metadata.validate_versioned_execution(principal, &source)?;
                    if metadata.get_room(RoomId(validated.room_id))?.tenant_id != tenant {
                        return Err(ApiError::BadRequest(
                            "versioned execution source belongs to another tenant".into(),
                        ));
                    }
                    Ok(validated)
                })
                .await?,
            )
        }
        None => None,
    };
    let attempt = state
        .sessions
        .apply_migration_captured(session, connection, principal, request, source)
        .await;
    let result = attempt.result;
    // Recovery of a late observation is permitted only to the invocation that
    // won the atomic plan claim; a concurrent replay cannot settle its run.
    let observation = attempt.run;
    if observation.is_none() {
        return result;
    }
    let run = observation.as_ref().unwrap();
    let native = match crate::session::migration_journal::ledger(
        metadata_store_cloned(state)?,
        tenant,
        run.id,
    )
    .await
    {
        Ok(native) => native,
        Err(_) => {
            tracing::error!("failed to read native migration ledger witnesses");
            return result;
        }
    };
    for witness in native.entries {
        if let Err(error) = record_database_change(
            state,
            principal,
            DatabaseChangeRecord {
                workspace_id: None,
                tenant_id: Some(tenant),
                room_id: None,
                connection_profile_id: Some(profile),
                database_target: native
                    .source
                    .as_ref()
                    .and_then(|source| source.database_target.clone()),
                operation: witness.operation,
                affected_object: None,
                row_count: witness.rows,
                sql_fingerprint: Some(witness.fingerprint),
                row_identity_fingerprint: None,
                transaction_id: witness.transaction_id.map(|tx| tx.0.to_string()),
                source_workflow: "schema_migration",
                approved_by: Some(principal.0),
                outcome: witness.outcome,
                result_code: witness.result_code,
                versioned_source: validated_source.clone(),
            },
        )
        .await
        {
            let _ = error;
            tracing::error!("failed to append migration change ledger entry");
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn migration_settlement_admission_refusal_is_audited_before_preparation() {
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
        let result = apply_migration(
            State(state.clone()),
            HeaderMap::new(),
            None,
            Path((sift_protocol::SessionId(1), sift_protocol::ConnectionId(2))),
            Json(sift_protocol::ApplyMigrationRequest {
                plan_id: sift_protocol::MigrationPlanId(uuid::Uuid::new_v4()),
                plan_digest: "unused".into(),
                acknowledgements: vec![],
                source: None,
            }),
        )
        .await;
        assert!(result.is_err());
        let operations = state.sessions.list_operations();
        assert_eq!(operations.len(), 1);
        assert!(matches!(
            operations[0].operation,
            Operation::ApplyMigration { .. }
        ));
        assert_eq!(operations[0].status, OperationStatus::Failed);
        assert_eq!(state.shutdown.in_flight(), 0);
    }
}
