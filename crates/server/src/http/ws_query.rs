//! Terminal settlement owned by the upgraded connection task, not its socket.
use super::*;

pub(super) struct PreparedSource {
    pub entry: crate::session::ConnectionEntryClone,
    pub scope: Option<DatabaseActorScope>,
    pub versioned_source: Option<sift_metadata::ValidatedVersionedExecution>,
}

pub(super) async fn prepare_source(
    state: &AppState,
    auth: Option<&AuthContext>,
    session: sift_protocol::SessionId,
    connection: sift_protocol::ConnectionId,
    sql: &str,
    source: Option<Box<sift_protocol::VersionedExecutionContext>>,
    variables: Option<&sift_protocol::SqlVariableHistoryContext>,
) -> ApiResult<PreparedSource> {
    let entry = state.sessions.authorize_connection_operation(
        session,
        connection,
        sift_protocol::OperationKind::ExecuteQuery,
        Some(sql),
        &[],
    )?;
    if let Some(context) = variables {
        if context.template_sql.len() > 1_048_576 {
            return Err(ApiError::BadRequest(
                "SQL variable template exceeds 1 MiB".into(),
            ));
        }
        let references = match entry.driver.semantic_engine() {
            Some(engine) => {
                sift_sql_variables::references_for_engine(engine, &context.template_sql)
            }
            None => sift_sql_variables::references(&context.template_sql),
        };
        let expected = references
            .map_err(|error| ApiError::BadRequest(error.to_string()))?
            .into_iter()
            .map(|reference| (reference.name, reference.kind))
            .collect::<std::collections::BTreeSet<_>>();
        let described = context
            .descriptors
            .iter()
            .map(|descriptor| (descriptor.name.clone(), descriptor.kind))
            .collect::<std::collections::BTreeSet<_>>();
        if expected != described || described.len() != context.descriptors.len() {
            return Err(ApiError::BadRequest(
                "SQL variable history descriptors do not match the template".into(),
            ));
        }
    }
    let scope = if state.metadata.is_some() {
        actor_scope(auth, &entry)?
    } else {
        None
    };
    let versioned_source = match source {
        Some(source) => {
            let scope = scope.as_ref().ok_or(ApiError::MetadataUnavailable)?;
            let tenant = scope.tenant_id.ok_or_else(|| {
                ApiError::BadRequest("versioned execution requires a proven managed tenant".into())
            })?;
            let metadata = metadata_store_cloned(state)?;
            let actor = scope.actor;
            Some(
                metadata_blocking(move || {
                    let validated = metadata.validate_versioned_execution(actor, &source)?;
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
    Ok(PreparedSource {
        entry,
        scope,
        versioned_source,
    })
}

pub(super) fn actor_scope(
    auth: Option<&AuthContext>,
    entry: &crate::session::ConnectionEntryClone,
) -> ApiResult<Option<DatabaseActorScope>> {
    let database_target =
        configured_database_target(&entry.driver.provider().provider_id, &entry.configuration);
    match &entry.provenance {
        crate::session::ConnectionProvenance::Managed {
            principal_id,
            tenant_id,
            profile_id,
            ..
        } => {
            if auth.is_some_and(|auth| auth.principal_id != *principal_id) {
                return Err(ApiError::Forbidden(
                    "query caller must own the managed session".into(),
                ));
            }
            Ok(Some(DatabaseActorScope {
                actor: *principal_id,
                tenant_id: Some(*tenant_id),
                connection_profile_id: Some(*profile_id),
                database_target,
            }))
        }
        crate::session::ConnectionProvenance::TrustedLocal => {
            Ok(auth.map(|auth| DatabaseActorScope {
                actor: auth.principal_id,
                tenant_id: None,
                connection_profile_id: None,
                database_target,
            }))
        }
    }
}

pub(super) struct Settlement {
    pub scope: Option<DatabaseActorScope>,
    pub history_sql: String,
    pub descriptors: Vec<sift_protocol::RedactedSqlVariableDescriptor>,
    pub change_kind: Option<sift_protocol::ChangeLedgerOperation>,
    pub sql_fingerprint: Option<String>,
    pub transaction_id: Option<String>,
    pub source: Option<sift_metadata::ValidatedVersionedExecution>,
    pub started: Instant,
}

impl Settlement {
    pub(super) async fn finish(
        self,
        state: &AppState,
        audit: &mut query_audit::QueryOperationAudit,
        terminal: &StreamTerminal,
    ) {
        let (status, history_status, code, message, outcome) = match &terminal.outcome {
            StreamOutcome::Completed => (
                OperationStatus::Succeeded,
                QueryStatus::Ok,
                None,
                None,
                if self.transaction_id.is_some() {
                    sift_protocol::ChangeLedgerOutcome::Partial
                } else {
                    sift_protocol::ChangeLedgerOutcome::Committed
                },
            ),
            StreamOutcome::Cancelled => (
                OperationStatus::Failed,
                QueryStatus::Canceled,
                Some(sift_protocol::Code::QueryCanceled.to_string()),
                None,
                sift_protocol::ChangeLedgerOutcome::OutcomeUnknown,
            ),
            StreamOutcome::Failed {
                code,
                message,
                write_outcome,
            } => (
                OperationStatus::Failed,
                QueryStatus::Error,
                code.clone(),
                Some(message.clone()),
                *write_outcome,
            ),
            StreamOutcome::Pending => (
                OperationStatus::Failed,
                QueryStatus::Error,
                Some("query_observer_lost".into()),
                Some(
                    "query delivery ended before a terminal observation; native outcome is unknown"
                        .into(),
                ),
                sift_protocol::ChangeLedgerOutcome::OutcomeUnknown,
            ),
        };
        let rows = terminal.observed_row_count();
        audit.record(status, code.clone(), rows, message.clone());
        let Some(scope) = self.scope else {
            return;
        };
        let metadata = match metadata_store_cloned(state) {
            Ok(metadata) => metadata,
            Err(error) => {
                tracing::warn!(%error, "stream query metadata unavailable");
                return;
            }
        };
        let sql_text = if state.sessions.store_sql() {
            self.history_sql
        } else {
            crate::fingerprint::sql(&self.history_sql)
        };
        let record = NewQueryHistory {
            principal_id: scope.actor,
            room_id: None,
            connection_profile_id: scope.connection_profile_id,
            sql_text,
            duration_ms: Some(self.started.elapsed().as_millis().min(i64::MAX as u128) as i64),
            row_count: rows,
            status: history_status,
            error_code: code.clone(),
            error_message: message,
            variable_descriptors: self.descriptors,
        };
        if let Err(error) = metadata_blocking(move || {
            metadata
                .record_query_history(record)
                .map(|_| ())
                .map_err(Into::into)
        })
        .await
        {
            tracing::warn!(%error, "failed to record streamed query history");
        }
        if let Some(operation) = self.change_kind {
            if let Err(error) = record_database_change(
                state,
                scope.actor,
                DatabaseChangeRecord {
                    workspace_id: None,
                    tenant_id: scope.tenant_id,
                    room_id: None,
                    connection_profile_id: scope.connection_profile_id,
                    database_target: scope.database_target,
                    operation,
                    affected_object: None,
                    row_count: rows,
                    sql_fingerprint: self.sql_fingerprint,
                    row_identity_fingerprint: None,
                    transaction_id: self.transaction_id,
                    source_workflow: "direct_sql",
                    approved_by: None,
                    outcome,
                    result_code: code,
                    versioned_source: self.source,
                },
            )
            .await
            {
                tracing::error!(%error, "failed to append streamed change ledger entry");
            }
        }
    }
}

pub(super) async fn cancel_user_query(
    sessions: &SessionStore,
    session: sift_protocol::SessionId,
    connection: sift_protocol::ConnectionId,
    cursor: sift_protocol::CursorId,
    expected: Option<(sift_protocol::ConnectionId, sift_protocol::CursorId)>,
    actor: Option<i64>,
) -> ApiResult<()> {
    let mut audit = query_audit::QueryOperationAudit::new(
        sessions.clone(),
        Operation::CancelQuery {
            session,
            request: sift_protocol::CancelRequest { connection, cursor },
        },
        actor,
    );
    let result = if expected.is_some_and(|expected| expected != (connection, cursor)) {
        Err(ApiError::BadRequest(
            "cancel cursor or connection mismatch".into(),
        ))
    } else {
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            sessions.cancel(session, connection, cursor),
        )
        .await
        .unwrap_or_else(|_| {
            Err(ApiError::Driver(sift_protocol::DriverError::new(
                sift_protocol::Code::QueryTimedOut,
                "query cancellation observation timed out; native outcome is uncertain",
            )))
        })
    };
    match &result {
        Ok(()) => audit.record(OperationStatus::Succeeded, None, None, None),
        Err(error) => audit.failure(error),
    }
    result
}

pub(super) async fn pending_control(
    message: Option<Result<Message, axum::Error>>,
    sessions: &SessionStore,
    session: sift_protocol::SessionId,
    connection: sift_protocol::ConnectionId,
    cursor: sift_protocol::CursorId,
    actor: Option<i64>,
) -> ApiResult<bool> {
    let message = message
        .ok_or_else(|| ApiError::BadRequest("websocket closed while awaiting query output".into()))?
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    match message {
        Message::Text(text) => match serde_json::from_str::<WsClientMessage>(&text)
            .map_err(|error| ApiError::BadRequest(error.to_string()))?
        {
            WsClientMessage::Cancel {
                connection: target,
                cursor_id,
            } => {
                cancel_user_query(
                    sessions,
                    session,
                    target,
                    cursor_id,
                    Some((connection, cursor)),
                    actor,
                )
                .await?;
                Ok(true)
            }
            _ => Err(ApiError::BadRequest(
                "query output is pending; only cancellation is accepted".into(),
            )),
        },
        Message::Close(_) => Err(ApiError::BadRequest(
            "websocket closed while awaiting query output".into(),
        )),
        Message::Ping(_) | Message::Pong(_) | Message::Binary(_) => Ok(false),
    }
}

/// Observe connection loss without canceling the independently owned native
/// execute future. No cursor is available yet, so explicit controls are refused.
pub(super) async fn await_start<F>(
    work: F,
    receiver: &mut futures::stream::SplitStream<WebSocket>,
    sessions: &SessionStore,
    session: sift_protocol::SessionId,
    actor: Option<i64>,
) -> ApiResult<ApiResult<sift_driver_api::ResultSetStream>>
where
    F: std::future::Future<Output = ApiResult<sift_driver_api::ResultSetStream>>,
{
    tokio::pin!(work);
    loop {
        tokio::select! {
            result = &mut work => return Ok(result),
            message = receiver.next() => {
                let message = message.ok_or_else(|| ApiError::BadRequest("websocket closed before query cursor publication".into()))?
                    .map_err(|error| ApiError::BadRequest(error.to_string()))?;
                match message {
                    Message::Close(_) => return Err(ApiError::BadRequest("websocket closed before query cursor publication".into())),
                    Message::Ping(_) | Message::Pong(_) | Message::Binary(_) => {},
                    Message::Text(text) => {
                        if let Ok(WsClientMessage::Cancel { connection, cursor_id }) = serde_json::from_str::<WsClientMessage>(&text) {
                            let mut audit = query_audit::QueryOperationAudit::new(sessions.clone(), Operation::CancelQuery {
                                session, request: sift_protocol::CancelRequest { connection, cursor: cursor_id },
                            }, actor);
                            audit.failure(&ApiError::BadRequest("query cursor has not been published; cancellation was not dispatched".into()));
                        }
                        return Err(ApiError::BadRequest("wait for query Started before sending controls".into()));
                    }
                }
            }
        }
    }
}
