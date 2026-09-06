//! Execution HTTP handlers; shared admission and audit stay at the router boundary.

use super::*;

pub(super) async fn execute_query(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<sift_protocol::SessionId>,
    Json(req): Json<ExecuteRequestHttp>,
) -> ApiResult<Response> {
    let metadata_context = execute_metadata_context(&state, headers, &req).await?;
    let ledger_context = metadata_context.clone();
    let sql_text = req.sql.clone();
    let change_kind = sql_change_kind(&sql_text);
    let transaction_id = req.tx.as_ref().map(|tx| tx.tx_id.0.to_string());
    let operation = Operation::ExecuteQuery {
        session: id,
        request: req.clone(),
    };
    // Count this query against the shutdown drain gate for its whole lifetime;
    // in-flight queries continue draining even after `begin_drain`.
    let _query_guard = state.shutdown.track_query();
    let actor = metadata_context.as_ref().map(|c| c.principal_id.0);
    let inactive_room_connection = metadata_context
        .as_ref()
        .and_then(|context| context.room_id)
        .filter(|room| !state.rooms.is_active(room.0));
    let started = Instant::now();
    // A bound room routes execution through its server-owned connection
    // (ADR-037); everything else runs on the caller's session connection.
    let (result, shared_pages, shared_retention_guards) = match metadata_context
        .as_ref()
        .and_then(|c| c.room_routing.clone())
    {
        Some(routing) => {
            let provenance = crate::session::RoomConnProvenance {
                room_id: routing.room_id,
                binder: routing.binder,
                tenant: routing.tenant,
                profile_id: routing.profile_id,
                provider_id: routing.provider_id,
                engine: routing.engine,
                policy_revision: routing.policy_revision,
            };
            match state.sessions.execute_room_query(provenance, req).await {
                Ok(execution) => (
                    Ok(execution.response),
                    Some(execution.pages),
                    execution.retention_guards,
                ),
                Err(error) => (Err(error), Some(Vec::new()), Vec::new()),
            }
        }
        None => (state.sessions.execute_http(id, req).await, None, Vec::new()),
    };
    let duration_ms = started.elapsed().as_millis().min(i64::MAX as u128) as i64;
    if let (Some(context), Some(change_kind)) = (ledger_context, change_kind) {
        let (row_count, outcome, result_code) = match &result {
            Ok(response) => (
                response
                    .affected_rows
                    .and_then(|rows| i64::try_from(rows).ok())
                    .or_else(|| i64::try_from(response.rows.len()).ok()),
                if transaction_id.is_some() {
                    sift_protocol::ChangeLedgerOutcome::Partial
                } else {
                    sift_protocol::ChangeLedgerOutcome::Committed
                },
                None,
            ),
            Err(ApiError::Driver(error)) => (
                None,
                sift_protocol::ChangeLedgerOutcome::Failed,
                Some(error.code.to_string()),
            ),
            Err(_) => (None, sift_protocol::ChangeLedgerOutcome::Failed, None),
        };
        if let Err(error) = record_database_change(
            &state,
            context.principal_id,
            DatabaseChangeRecord {
                tenant_id: context.tenant_id,
                room_id: context.room_id,
                connection_profile_id: context.connection_profile_id,
                database_target: context.database_target,
                operation: change_kind,
                affected_object: None,
                row_count,
                sql_fingerprint: Some(crate::fingerprint::sql(&sql_text)),
                row_identity_fingerprint: None,
                transaction_id,
                source_workflow: "direct_sql",
                approved_by: None,
                outcome,
                result_code,
                versioned_source: context.versioned_source,
            },
        )
        .await
        {
            tracing::error!(%error, "failed to append database change ledger entry");
        }
    }
    if let Some(context) = metadata_context {
        if let Some(room_id) = context.room_id {
            let row_count = shared_pages.as_ref().map(|pages| {
                pages
                    .iter()
                    .map(|page| match page {
                        sift_protocol::Page::Rows { rows } => rows.len() as i64,
                        _ => 0,
                    })
                    .sum()
            });
            let registry = state.rooms.results().clone();
            let actor_principal_id = context.principal_id.0;
            let connection_profile_id = context.connection_profile_id.map(|id| id.0);
            let error_message = result.as_ref().err().map(ToString::to_string);
            let summary = tokio::task::spawn_blocking(move || {
                registry.insert(crate::room_results::NewRoomResult {
                    room_id: room_id.0,
                    actor_principal_id,
                    connection_profile_id,
                    pages: shared_pages.unwrap_or_default(),
                    row_count,
                    error_message,
                    retention_guards: shared_retention_guards,
                })
            })
            .await
            .map_err(|error| {
                ApiError::Internal(format!("room result retention task failed: {error}"))
            })?;
            state.rooms.publish_presence(
                summary.room_id,
                RoomServerMessage::QueryResult { result: summary },
            );
        }
        // Query history keeps raw SQL by default; when store_sql is off it
        // stores only the fingerprint (audit trail is always fingerprinted).
        let history_sql = if state.sessions.store_sql() {
            sql_text
        } else {
            crate::fingerprint::sql(&sql_text)
        };
        record_execute_history(context, history_sql, duration_ms, &result).await;
    }
    if let Some(room) = inactive_room_connection.filter(|room| !state.rooms.is_active(room.0)) {
        state.sessions.close_room_connection(room.0).await;
    }
    match result {
        Ok(resp) => {
            let row_count = Some(resp.rows.len() as i64);
            state.sessions.push_operation_full(
                operation,
                OperationStatus::Succeeded,
                actor,
                None,
                row_count,
                None,
            );
            let bytes =
                serde_json::to_vec(&resp).map_err(|error| ApiError::Internal(error.to_string()))?;
            retained_json_response(&state.sessions, id, bytes)
        }
        Err(error) => {
            let (result_code, message) = match &error {
                ApiError::Driver(driver) => {
                    (Some(driver.code.to_string()), Some(driver.message.clone()))
                }
                other => (None, Some(other.to_string())),
            };
            state.sessions.push_operation_full(
                operation,
                OperationStatus::Failed,
                actor,
                result_code,
                None,
                message,
            );
            Err(error)
        }
    }
}

pub(super) fn retained_json_response(
    sessions: &SessionStore,
    session: sift_protocol::SessionId,
    bytes: Vec<u8>,
) -> ApiResult<Response> {
    let guard = sessions.reserve_session_retained_bytes(session, bytes.len())?;
    let bytes = bytes::Bytes::from_owner(RetainedResponseBytes {
        bytes,
        _guard: guard,
    });
    let mut response = Body::from(bytes).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    Ok(response)
}

pub(super) async fn begin_transaction(
    State(state): State<AppState>,
    Path(id): Path<sift_protocol::SessionId>,
    Json(req): Json<BeginTransactionRequest>,
) -> ApiResult<Json<sift_protocol::TransactionInfo>> {
    let operation = Operation::BeginTransaction {
        session: id,
        request: req.clone(),
    };
    let tx = finish_operation(
        &state.sessions,
        operation,
        state.sessions.begin_transaction(id, req).await,
        |_| None,
    )?;
    Ok(Json(tx))
}

pub(super) async fn list_processes(
    State(state): State<AppState>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
) -> ApiResult<Json<Vec<sift_protocol::DatabaseProcess>>> {
    let processes = finish_operation(
        &state.sessions,
        Operation::ListProcesses {
            session,
            connection,
        },
        crate::process::list(&state.sessions, session, connection).await,
        |processes| Some(processes.len() as i64),
    )?;
    Ok(Json(processes))
}

pub(super) async fn kill_process(
    State(state): State<AppState>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Json(req): Json<KillProcessRequest>,
) -> ApiResult<Json<sift_protocol::KillProcessResponse>> {
    let response = finish_operation(
        &state.sessions,
        Operation::KillProcess {
            session,
            connection,
            request: req.clone(),
        },
        crate::process::kill(&state.sessions, session, connection, req.process_id).await,
        |_| None,
    )?;
    Ok(Json(response))
}

pub(super) async fn list_transactions(
    State(state): State<AppState>,
    Path(id): Path<sift_protocol::SessionId>,
) -> ApiResult<Json<Vec<sift_protocol::TransactionState>>> {
    let result = finish_operation(
        &state.sessions,
        Operation::ListTransactions { session: id },
        state.sessions.list_transactions(id),
        |transactions| Some(transactions.len() as i64),
    )?;
    Ok(Json(result))
}

pub(super) async fn preview_transaction(
    State(state): State<AppState>,
    Path((id, tx_id)): Path<(sift_protocol::SessionId, sift_protocol::TxId)>,
    Json(req): Json<TransactionPreviewRequest>,
) -> ApiResult<Json<sift_protocol::TransactionPreview>> {
    let result = if req.tx_id != tx_id {
        Err(ApiError::BadRequest(
            "`tx_id` body value must match tx id in path".into(),
        ))
    } else {
        state.sessions.preview_transaction(id, &req)
    };
    let result = finish_operation(
        &state.sessions,
        Operation::PreviewTransaction {
            session: id,
            request: req.clone(),
        },
        result,
        |_| None,
    )?;
    Ok(Json(result))
}

pub(super) async fn commit_transaction(
    State(state): State<AppState>,
    Path((id, tx_id)): Path<(sift_protocol::SessionId, sift_protocol::TxId)>,
    Json(req): Json<EndTransactionRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let result = if req.tx_id != tx_id {
        Err(ApiError::BadRequest(
            "`tx_id` body value must match tx id in path".into(),
        ))
    } else {
        state.sessions.commit_transaction(id, req.clone()).await
    };
    finish_operation(
        &state.sessions,
        Operation::CommitTransaction {
            session: id,
            request: req,
        },
        result,
        |_| None,
    )?;
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn rollback_transaction(
    State(state): State<AppState>,
    Path((id, tx_id)): Path<(sift_protocol::SessionId, sift_protocol::TxId)>,
    Json(req): Json<EndTransactionRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let result = if req.tx_id != tx_id {
        Err(ApiError::BadRequest(
            "`tx_id` body value must match tx id in path".into(),
        ))
    } else {
        state.sessions.rollback_transaction(id, req.clone()).await
    };
    finish_operation(
        &state.sessions,
        Operation::RollbackTransaction {
            session: id,
            request: req,
        },
        result,
        |_| None,
    )?;
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn create_savepoint(
    State(state): State<AppState>,
    Path((id, tx_id)): Path<(sift_protocol::SessionId, sift_protocol::TxId)>,
    Json(req): Json<SavepointRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let result = if req.tx_id != tx_id {
        Err(ApiError::BadRequest(
            "`tx_id` body value must match tx id in path".into(),
        ))
    } else {
        state.sessions.create_savepoint(id, req.clone()).await
    };
    finish_operation(
        &state.sessions,
        Operation::Savepoint {
            session: id,
            request: req,
        },
        result,
        |_| None,
    )?;
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn rollback_to_savepoint(
    State(state): State<AppState>,
    Path((id, tx_id)): Path<(sift_protocol::SessionId, sift_protocol::TxId)>,
    Json(req): Json<SavepointRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let result = if req.tx_id != tx_id {
        Err(ApiError::BadRequest(
            "`tx_id` body value must match tx id in path".into(),
        ))
    } else {
        state.sessions.rollback_to_savepoint(id, req.clone()).await
    };
    finish_operation(
        &state.sessions,
        Operation::RollbackToSavepoint {
            session: id,
            request: req,
        },
        result,
        |_| None,
    )?;
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn release_savepoint(
    State(state): State<AppState>,
    Path((id, tx_id)): Path<(sift_protocol::SessionId, sift_protocol::TxId)>,
    Json(req): Json<SavepointRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let result = if req.tx_id != tx_id {
        Err(ApiError::BadRequest(
            "`tx_id` body value must match tx id in path".into(),
        ))
    } else {
        state.sessions.release_savepoint(id, req.clone()).await
    };
    finish_operation(
        &state.sessions,
        Operation::ReleaseSavepoint {
            session: id,
            request: req,
        },
        result,
        |_| None,
    )?;
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn cancel_query(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, cursor_id)): Path<(sift_protocol::SessionId, sift_protocol::CursorId)>,
    Json(req): Json<CancelRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    if req.cursor != cursor_id {
        return Err(ApiError::BadRequest(
            "`cursor` body value must match cursor id in path".into(),
        ));
    }
    let auth = optional_auth_context_blocking(state.clone(), headers).await?;
    let actor = auth.as_ref().map(|auth| auth.principal_id.0);
    if let Some(owner) = state.sessions.session_owner(id)? {
        let Some(auth) = auth.as_ref() else {
            return Err(ApiError::Unauthorized);
        };
        if auth.principal_id != owner {
            return Err(ApiError::Forbidden(
                "cannot cancel a cursor owned by another principal".into(),
            ));
        }
    }
    state.sessions.cancel(id, req.connection, cursor_id).await?;
    state.sessions.push_operation_full(
        Operation::CancelQuery {
            session: id,
            request: req,
        },
        OperationStatus::Succeeded,
        actor,
        None,
        None,
        None,
    );
    Ok(Json(json!({"ok": true})))
}

/// Resume from a spilled cursor. The client learns the URL from the
/// `resume_url` field on the `CursorEvicted` terminal.
pub(super) async fn read_spill_pages(
    State(state): State<AppState>,
    Path(cursor_id): Path<sift_protocol::CursorId>,
    axum::extract::Query(q): axum::extract::Query<ReadSpillPagesQuery>,
) -> ApiResult<Response> {
    let registry = state.sessions.cursor_registry().clone();
    let info = registry.spill_info(cursor_id).ok_or_else(|| {
        ApiError::Driver(sift_protocol::DriverError::new(
            sift_protocol::Code::CursorNotFound,
            "no spill for cursor",
        ))
    })?;
    // If from_seq is set and it doesn't match the entry's current
    // read cursor, reject — we don't allow re-reading already-read
    // pages (spill files are append-only + read-forward).
    if let Some(seq) = q.from_seq {
        if seq != info.pages_read {
            return Err(ApiError::BadRequest(format!(
                "from_seq={seq} does not match pages_read={} for cursor",
                info.pages_read
            )));
        }
    }
    let limit = q.limit.unwrap_or(32).clamp(1, 256);
    let (pages, done) =
        tokio::task::spawn_blocking(move || registry.read_spill_pages(cursor_id, limit))
            .await
            .map_err(|e| ApiError::Internal(format!("spill read task failed: {e}")))?
            .map_err(ApiError::Driver)?;
    let bytes = serde_json::to_vec(&json!({
        "cursor_id": cursor_id.0,
        "pages": pages,
        "done": done,
    }))
    .map_err(|error| ApiError::Internal(error.to_string()))?;
    retained_json_response(&state.sessions, info.session_id, bytes)
}

/// Explicit cleanup of a spill file. Idempotent; returns ok whether or
/// not the entry existed.
pub(super) async fn delete_spilled_cursor(
    State(state): State<AppState>,
    Path(cursor_id): Path<sift_protocol::CursorId>,
) -> ApiResult<Json<serde_json::Value>> {
    state.sessions.cursor_registry().drop_spill(cursor_id);
    Ok(Json(json!({"ok": true})))
}
