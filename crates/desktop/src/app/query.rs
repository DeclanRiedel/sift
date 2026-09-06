//! Streamed query ownership, backpressure, cancellation, and spill recovery.

use super::*;

pub(super) struct QueryRun {
    pub(super) client: Client,
    pub(super) session: SessionId,
    pub(super) connection: ConnectionId,
    pub(super) transaction: Option<sift_protocol::TransactionInfo>,
    pub(super) item_id: u64,
    pub(super) execution_id: u64,
    pub(super) sql: String,
    pub(super) params: Vec<sift_protocol::Value>,
    pub(super) transform: Option<sift_protocol::ResultTransform>,
    pub(super) source: Option<sift_protocol::VersionedExecutionContext>,
    pub(super) variable_context: Option<sift_protocol::SqlVariableHistoryContext>,
}

pub(super) async fn run_streamed_query(
    run: QueryRun,
    controls: tokio::sync::mpsc::UnboundedReceiver<QueryControl>,
    events: tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
) {
    let client = run.client.clone();
    let session = run.session;
    let transaction = run.transaction.clone();
    run_streamed_query_inner(run, controls, events.clone()).await;
    let Some(transaction) = transaction else {
        return;
    };
    let state = client
        .list_transactions(session)
        .await
        .map(|states| {
            states
                .into_iter()
                .find(|state| state.transaction.tx_id == transaction.tx_id)
        })
        .map_err(|error| format!("refreshing transaction state failed: {error}"));
    let _ = events.send(ExecutorEvent::TransactionStateRefreshed(state));
}

async fn run_streamed_query_inner(
    run: QueryRun,
    mut controls: tokio::sync::mpsc::UnboundedReceiver<QueryControl>,
    events: tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
) {
    let QueryRun {
        client,
        session,
        connection,
        transaction,
        item_id,
        execution_id,
        sql,
        params,
        transform,
        source,
        variable_context,
    } = run;
    let started = tokio::select! {
        stream = client.start_query_event_stream_versioned_with_variables(
            session,
            connection,
            sql,
            params,
            transaction.map(|transaction| sift_protocol::TxHandleRef {
                tx_id: transaction.tx_id,
                connection: transaction.connection,
                mode: transaction.mode,
            }),
            transform,
            source,
            variable_context,
        ) => stream,
        control = controls.recv() => {
            if matches!(control, Some(QueryControl::Cancel)) {
                let _ = events.send(ExecutorEvent::Execution {
                    item_id,
                    execution_id,
                    state: ResultState::Cancelled,
                });
            }
            return;
        }
    };
    let mut stream = match started {
        Ok(stream) => stream,
        Err(error) => {
            send_execution_error(item_id, execution_id, error, &events);
            return;
        }
    };
    let cursor_id = stream.cursor_id();
    if events
        .send(ExecutorEvent::ExecutionStarted {
            item_id,
            execution_id,
            cursor_id,
        })
        .is_err()
    {
        let _ = stream.cancel().await;
        return;
    }

    loop {
        let next = tokio::select! {
            page = stream.next_events() => page,
            control = controls.recv() => {
                if matches!(control, Some(QueryControl::Cancel)) {
                    let _ = stream.cancel().await;
                    let _ = events.send(ExecutorEvent::Execution {
                        item_id,
                        execution_id,
                        state: ResultState::Cancelled,
                    });
                }
                return;
            }
        };
        let (seq, execution_events) = match next {
            Ok(page) => page,
            Err(error) => {
                send_execution_error(item_id, execution_id, error, &events);
                return;
            }
        };
        let mut terminal = false;
        let mut batch_warnings = Vec::new();
        for event in execution_events {
            let page = match event {
                sift_protocol::ExecutionEventV2::ResultSetStarted { columns, .. } => {
                    Some(sift_protocol::Page::NextResult { columns })
                }
                sift_protocol::ExecutionEventV2::Rows { rows, .. } => {
                    Some(sift_protocol::Page::Rows { rows })
                }
                sift_protocol::ExecutionEventV2::ResultSetCompleted { summary, .. } => {
                    batch_warnings.extend(summary.warnings);
                    None
                }
                sift_protocol::ExecutionEventV2::CommandCompleted { summary, .. } => {
                    batch_warnings.extend(summary.warnings);
                    None
                }
                sift_protocol::ExecutionEventV2::Notice { message, .. } => {
                    batch_warnings.push(sift_protocol::DriverWarning::new(message));
                    None
                }
                sift_protocol::ExecutionEventV2::ExecutionCompleted { summary, .. } => {
                    terminal = true;
                    Some(sift_protocol::Page::Done {
                        affected_rows: summary.affected_rows,
                        warnings: std::mem::take(&mut batch_warnings),
                    })
                }
                sift_protocol::ExecutionEventV2::Error { error, .. } => {
                    if error.code == sift_protocol::Code::CursorEvicted
                        && error.resume_url.is_some()
                    {
                        resume_spilled_query(
                            &client,
                            cursor_id,
                            item_id,
                            execution_id,
                            &mut controls,
                            &events,
                        )
                        .await;
                        return;
                    }
                    terminal = true;
                    Some(sift_protocol::Page::Error { error })
                }
                sift_protocol::ExecutionEventV2::Progress { progress, .. } => {
                    let _ = events.send(ExecutorEvent::ExecutionProgress {
                        item_id,
                        execution_id,
                        progress,
                    });
                    None
                }
                sift_protocol::ExecutionEventV2::ExecutionStarted { .. }
                | sift_protocol::ExecutionEventV2::StatementStarted { .. } => None,
            };
            let Some(page) = page else { continue };
            let (acknowledge, consumed) = tokio::sync::oneshot::channel();
            if events
                .send(ExecutorEvent::ExecutionPage {
                    item_id,
                    execution_id,
                    cursor_id,
                    page: sift_workspace_ui::results::PreparedResultPage::new(page),
                    acknowledge,
                })
                .is_err()
            {
                let _ = stream.cancel().await;
                return;
            }
            if terminal {
                return;
            }
            let consumed = tokio::select! {
                consumed = consumed => consumed.is_ok(),
                control = controls.recv() => {
                    if matches!(control, Some(QueryControl::Cancel)) {
                        let _ = stream.cancel().await;
                        let _ = events.send(ExecutorEvent::Execution {
                            item_id,
                            execution_id,
                            state: ResultState::Cancelled,
                        });
                    }
                    return;
                }
            };
            if !consumed {
                let _ = stream.cancel().await;
                return;
            }
        }
        if terminal {
            return;
        }
        tokio::select! {
            acknowledged = stream.acknowledge(seq) => {
                if acknowledged.is_err() {
                    let _ = stream.cancel().await;
                    return;
                }
            }
            control = controls.recv() => {
                if matches!(control, Some(QueryControl::Cancel)) {
                    let _ = stream.cancel().await;
                    let _ = events.send(ExecutorEvent::Execution {
                        item_id,
                        execution_id,
                        state: ResultState::Cancelled,
                    });
                }
                return;
            }
        }
    }
}

async fn resume_spilled_query(
    client: &Client,
    cursor_id: sift_protocol::CursorId,
    item_id: u64,
    execution_id: u64,
    controls: &mut tokio::sync::mpsc::UnboundedReceiver<QueryControl>,
    events: &tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
) {
    loop {
        let batch = tokio::select! {
            batch = client.read_spilled_page_batch(cursor_id, None, Some(1)) => batch,
            control = controls.recv() => {
                if matches!(control, Some(QueryControl::Cancel)) {
                    let _ = client.delete_spilled_cursor(cursor_id).await;
                    let _ = events.send(ExecutorEvent::Execution {
                        item_id,
                        execution_id,
                        state: ResultState::Cancelled,
                    });
                }
                return;
            }
        };
        let batch = match batch {
            Ok(batch) => batch,
            Err(error) => {
                send_execution_error(item_id, execution_id, error, events);
                return;
            }
        };
        if batch.cursor_id != cursor_id || (batch.pages.is_empty() && !batch.done) {
            let _ = events.send(ExecutorEvent::Execution {
                item_id,
                execution_id,
                state: ResultState::Failed("invalid spilled cursor page sequence".into()),
            });
            return;
        }
        let mut saw_terminal = false;
        for page in batch.pages {
            saw_terminal |= matches!(
                &page,
                sift_protocol::Page::Done { .. } | sift_protocol::Page::Error { .. }
            );
            let (acknowledge, consumed) = tokio::sync::oneshot::channel();
            if events
                .send(ExecutorEvent::ExecutionPage {
                    item_id,
                    execution_id,
                    cursor_id,
                    page: sift_workspace_ui::results::PreparedResultPage::new(page),
                    acknowledge,
                })
                .is_err()
            {
                let _ = client.delete_spilled_cursor(cursor_id).await;
                return;
            }
            if saw_terminal {
                return;
            }
            tokio::select! {
                acknowledged = consumed => {
                    if acknowledged.is_err() {
                        return;
                    }
                }
                control = controls.recv() => {
                    if matches!(control, Some(QueryControl::Cancel)) {
                        let _ = client.delete_spilled_cursor(cursor_id).await;
                        let _ = events.send(ExecutorEvent::Execution {
                            item_id,
                            execution_id,
                            state: ResultState::Cancelled,
                        });
                    }
                    return;
                }
            }
        }
        if batch.done {
            if !saw_terminal {
                let (acknowledge, _) = tokio::sync::oneshot::channel();
                let _ = events.send(ExecutorEvent::ExecutionPage {
                    item_id,
                    execution_id,
                    cursor_id,
                    page: sift_workspace_ui::results::PreparedResultPage::new(
                        sift_protocol::Page::Done {
                            affected_rows: None,
                            warnings: vec![sift_protocol::DriverWarning::new(
                                "Cursor resumed after eviction; only retained spill pages are available.",
                            )],
                        },
                    ),
                    acknowledge,
                });
            }
            return;
        }
    }
}

fn send_execution_error(
    item_id: u64,
    execution_id: u64,
    error: ClientError,
    events: &tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
) {
    let transport = matches!(
        &error,
        ClientError::Transport(_) | ClientError::WebSocket(_) | ClientError::Timeout(_)
    );
    let message = match error {
        ClientError::Server { error, .. } => error.message,
        other => other.to_string(),
    };
    if transport {
        let _ = events.send(ExecutorEvent::Connection(ConnectionStatus::Disconnected));
    }
    let _ = events.send(ExecutorEvent::Execution {
        item_id,
        execution_id,
        state: ResultState::from_execution_error(transport, message),
    });
}
