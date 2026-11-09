//! Dedicated notification sockets retain cleanup and one terminal audit.
use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) async fn listen(
    state: &AppState,
    auth: Option<&AuthContext>,
    session: sift_protocol::SessionId,
    connection: sift_protocol::ConnectionId,
    channels: Vec<String>,
    request_id: String,
    sender: &mut futures::stream::SplitSink<WebSocket, Message>,
    receiver: &mut futures::stream::SplitStream<WebSocket>,
) -> ApiResult<()> {
    let operation = Operation::Listen {
        session,
        connection,
    };
    let actor = auth.map(|auth| auth.principal_id.0).or_else(|| {
        state
            .sessions
            .session_owner(session)
            .ok()
            .flatten()
            .map(|id| id.0)
    });
    let guard = std::sync::Arc::new(state.shutdown.track_query());
    let mut audit = query_audit::QueryOperationAudit::new(state.sessions.clone(), operation, actor);
    let result = crate::shutdown::scope_query(Some(guard.clone()), async {
        let entry = state.sessions.conn_entry(session, connection)?;
        let bound = state.sessions.bind_source(session, connection, entry)?;
        let startup = bound.listen_pg(session, connection, channels);
        tokio::pin!(startup);
        let stream = with_ws_lease(state, auth, async {
            loop {
                tokio::select! {
                    result = &mut startup => return result,
                    message = receiver.next() => match message {
                        Some(Ok(Message::Ping(_) | Message::Pong(_))) => {},
                        None | Some(Ok(Message::Close(_))) | Some(Err(_)) => return Err(ApiError::Driver(
                            sift_protocol::DriverError::new(sift_protocol::Code::QueryCanceled,
                                "Notification startup observer closed"))),
                        _ => return Err(ApiError::BadRequest("Notification socket accepts only ping or close after Listen".into())),
                    }
                }
            }
        }).await?;
        let crate::registry::ProviderNotificationStream {mut notifications, settled} = stream;
        let delivery = async {
            loop {
                tokio::select! {
                    biased;
                    message = receiver.next() => match message {
                        None | Some(Ok(Message::Close(_))) => return Ok(false),
                        Some(Ok(Message::Ping(_) | Message::Pong(_))) => {},
                        Some(Ok(_)) => return Err(ApiError::BadRequest("Notification socket accepts only ping or close after Listen".into())),
                        Some(Err(_)) => return Ok(false),
                    },
                    event = notifications.recv() => {
                        match event {
                            Some(Ok(event)) => send_json(sender, &WsServerMessage::Notification {
                                request_id: request_id.clone(), channel: event.channel, payload: event.payload,
                            }).await?,
                            Some(Err(error)) => return Err(ApiError::Driver(error)),
                            None => return Ok(true),
                        }
                    }
                }
            }
        };
        let checked = async {
            tokio::pin!(delivery);
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = tick.tick() => {
                        bound.authorize_connection_operation(session, connection, sift_protocol::OperationKind::Listen, None, &[])?;
                        bound.conn_entry(session, connection)?;
                    }
                    result = &mut delivery => return result,
                }
            }
        };
        let result = with_ws_lease(state, auth, checked).await;
        notifications.close();
        drop(notifications);
        let cleanup = settled.await.map_err(|_| ApiError::Internal("Notification cleanup owner failed".into()))?;
        cleanup?;
        result
    }).await;
    match &result {
        Ok(_) => audit.record(OperationStatus::Succeeded, None, None, None),
        Err(error) => audit.failure(error),
    }
    drop(guard);
    match result {
        Ok(false) => Ok(()),
        Ok(true) => {
            send_json(
                sender,
                &WsServerMessage::Error {
                    request_id: Some(request_id),
                    code: Some(sift_protocol::Code::QueryCanceled),
                    retry_after_ms: None,
                    message: "Notification subscription stopped".into(),
                },
            )
            .await
        }
        Err(error) => {
            send_json(
                sender,
                &WsServerMessage::Error {
                    request_id: Some(request_id),
                    code: query_error_code(&error),
                    retry_after_ms: None,
                    message: error.to_string(),
                },
            )
            .await
        }
    }
}
