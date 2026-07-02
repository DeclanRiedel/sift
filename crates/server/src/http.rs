//! axum router + handlers. Routes versioned under `/v1`. The `AppState`
//! carries the `SessionStore` (which in turn carries the `DriverRegistry`).

use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{header, header::HeaderName, HeaderValue, Request, StatusCode};
use axum::middleware::{from_fn, from_fn_with_state, Next};
use axum::response::Response;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;

use sift_protocol::{
    CancelRequest, ExecuteRequest, ExecuteRequestHttp, Health, ObjectPath, OpenConnectionRequest,
    OpenSessionRequest, SchemaFilter, SchemaScope, WsClientMessage, WsServerMessage,
    PROTOCOL_VERSION,
};

use crate::error::{ApiError, ApiResult};
use crate::session::SessionStore;
use crate::VERSION;

#[derive(Clone)]
pub struct AppState {
    pub sessions: SessionStore,
    pub auth: AuthState,
}

#[derive(Clone, Default)]
pub struct AuthState {
    pub bearer_token: Option<String>,
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/openapi.json", get(openapi))
        .route("/v1/sessions", post(create_session).get(list_sessions))
        .route("/v1/sessions/:id", get(get_session).delete(close_session))
        .route(
            "/v1/sessions/:id/connections",
            post(open_connection).get(list_connections),
        )
        .route(
            "/v1/sessions/:id/connections/:conn_id",
            delete(close_connection),
        )
        .route(
            "/v1/sessions/:id/connections/:conn_id/ping",
            post(ping_connection),
        )
        .route(
            "/v1/sessions/:id/connections/:conn_id/schema",
            get(get_schema),
        )
        .route("/v1/sessions/:id/queries", post(execute_query))
        .route("/v1/sessions/:id/ws", get(ws_session))
        .route(
            "/v1/sessions/:id/queries/:cursor_id/cancel",
            post(cancel_query),
        )
        .layer(from_fn_with_state(state.auth.clone(), auth_middleware))
        .layer(from_fn(protocol_version_header))
        .with_state(state)
}

async fn protocol_version_header(req: Request<Body>, next: Next) -> Response {
    let mut response = next.run(req).await;
    response.headers_mut().insert(
        HeaderName::from_static("x-sift-protocol-version"),
        HeaderValue::from_static(PROTOCOL_VERSION),
    );
    response
}

async fn auth_middleware(
    State(auth): State<AuthState>,
    req: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let Some(expected) = auth.bearer_token.as_deref() else {
        return Ok(next.run(req).await);
    };
    let valid = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .is_some_and(|actual| actual == expected);
    if valid {
        Ok(next.run(req).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

async fn health(State(state): State<AppState>) -> Json<Health> {
    Json(Health {
        status: "ok".to_string(),
        version: VERSION.to_string(),
        engines: state.sessions.registry().engines(),
    })
}

async fn openapi() -> Json<serde_json::Value> {
    Json(json!({
        "openapi": "3.1.0",
        "info": {
            "title": "sift API",
            "version": VERSION
        },
        "paths": {
            "/v1/health": { "get": { "summary": "Health and registered engines" } },
            "/v1/sessions": {
                "get": { "summary": "List sessions" },
                "post": { "summary": "Create session" }
            },
            "/v1/sessions/{id}": {
                "get": { "summary": "Get session" },
                "delete": { "summary": "Close session" }
            },
            "/v1/sessions/{id}/connections": {
                "get": { "summary": "List connections" },
                "post": { "summary": "Open connection" }
            },
            "/v1/sessions/{id}/connections/{conn_id}": {
                "delete": { "summary": "Close connection" }
            },
            "/v1/sessions/{id}/connections/{conn_id}/ping": {
                "post": { "summary": "Ping connection" }
            },
            "/v1/sessions/{id}/connections/{conn_id}/schema": {
                "get": { "summary": "Fetch schema" }
            },
            "/v1/sessions/{id}/queries": {
                "post": { "summary": "Execute query over synchronous HTTP" }
            },
            "/v1/sessions/{id}/queries/{cursor_id}/cancel": {
                "post": { "summary": "Cancel query" }
            },
            "/v1/sessions/{id}/ws": {
                "get": { "summary": "WebSocket query stream; protocol uses WsClientMessage/WsServerMessage" }
            },
            "/v1/openapi.json": { "get": { "summary": "OpenAPI document" } }
        }
    }))
}

async fn create_session(
    State(state): State<AppState>,
    body: Option<Json<OpenSessionRequest>>,
) -> ApiResult<Json<sift_protocol::SessionInfo>> {
    let req = match body {
        Some(Json(b)) => b,
        None => OpenSessionRequest { tag: None },
    };
    Ok(Json(state.sessions.open_session(req)))
}

async fn list_sessions(
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<sift_protocol::SessionInfo>>> {
    Ok(Json(state.sessions.list_sessions()))
}

async fn get_session(
    State(state): State<AppState>,
    Path(id): Path<sift_protocol::SessionId>,
) -> ApiResult<Json<sift_protocol::SessionInfo>> {
    Ok(Json(state.sessions.session_info(id)?))
}

async fn close_session(
    State(state): State<AppState>,
    Path(id): Path<sift_protocol::SessionId>,
) -> ApiResult<Json<serde_json::Value>> {
    state.sessions.close_session(id)?;
    Ok(Json(json!({"ok": true})))
}

async fn open_connection(
    State(state): State<AppState>,
    Path(id): Path<sift_protocol::SessionId>,
    Json(req): Json<OpenConnectionRequest>,
) -> ApiResult<Json<sift_protocol::ConnectionInfo>> {
    let engine = req.engine;
    let spec = req.spec;
    let info = state.sessions.open_connection(id, engine, spec).await?;
    Ok(Json(info))
}

async fn list_connections(
    State(state): State<AppState>,
    Path(id): Path<sift_protocol::SessionId>,
) -> ApiResult<Json<Vec<sift_protocol::ConnectionInfo>>> {
    Ok(Json(state.sessions.list_connections(id)?))
}

async fn close_connection(
    State(state): State<AppState>,
    Path((id, conn_id)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
) -> ApiResult<Json<serde_json::Value>> {
    state.sessions.close_connection(id, conn_id).await?;
    Ok(Json(json!({"ok": true})))
}

async fn ping_connection(
    State(state): State<AppState>,
    Path((id, conn_id)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
) -> ApiResult<Json<sift_protocol::ServerInfo>> {
    Ok(Json(state.sessions.ping(id, conn_id).await?))
}

#[derive(Deserialize)]
struct SchemaQuery {
    /// `shallow` (default) or `deep`. Deep requires `schema` and `object`.
    #[serde(default)]
    depth: Option<String>,
    schema: Option<String>,
    object: Option<String>,
    name_pattern: Option<String>,
}

async fn get_schema(
    State(state): State<AppState>,
    Path((id, conn_id)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Query(q): Query<SchemaQuery>,
) -> ApiResult<Json<sift_protocol::SchemaSnapshot>> {
    let scope = build_scope(q)?;
    Ok(Json(state.sessions.schema(id, conn_id, scope).await?))
}

fn build_scope(q: SchemaQuery) -> ApiResult<SchemaScope> {
    match q.depth.as_deref().unwrap_or("shallow") {
        "shallow" => {
            let mut scope = SchemaScope::shallow();
            if q.name_pattern.is_some() {
                scope.filter = Some(SchemaFilter {
                    catalogs: None,
                    schemas: None,
                    kinds: None,
                    name_pattern: q.name_pattern,
                });
            }
            Ok(scope)
        }
        "deep" => {
            let schema = q.schema.ok_or_else(|| {
                ApiError::BadRequest("`depth=deep` requires `schema` query param".into())
            })?;
            let object = q.object.ok_or_else(|| {
                ApiError::BadRequest("`depth=deep` requires `object` query param".into())
            })?;
            Ok(SchemaScope::deep(ObjectPath {
                catalog: None,
                schema: Some(schema),
                name: object,
                kind: None,
            }))
        }
        other => Err(ApiError::BadRequest(format!(
            "unknown depth `{other}` (want `shallow` or `deep`)"
        ))),
    }
}

async fn execute_query(
    State(state): State<AppState>,
    Path(id): Path<sift_protocol::SessionId>,
    Json(req): Json<ExecuteRequestHttp>,
) -> ApiResult<Json<sift_protocol::ExecuteResponse>> {
    let inner = ExecuteRequest::new(req.sql);
    // tx handle is part of the protocol surface but the transactions
    // endpoint is TBD; for now the sync execute path is autocommit only.
    let _ = req.tx;
    let resp = state.sessions.execute(id, req.connection, inner).await?;
    Ok(Json(resp))
}

async fn cancel_query(
    State(state): State<AppState>,
    Path((id, cursor_id)): Path<(sift_protocol::SessionId, sift_protocol::CursorId)>,
    Json(req): Json<CancelRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    if req.cursor != cursor_id {
        return Err(ApiError::BadRequest(
            "`cursor` body value must match cursor id in path".into(),
        ));
    }
    state.sessions.cancel(id, req.connection, cursor_id).await?;
    Ok(Json(json!({"ok": true})))
}

async fn ws_session(
    State(state): State<AppState>,
    Path(session_id): Path<sift_protocol::SessionId>,
    ws: WebSocketUpgrade,
) -> Response {
    ws.on_upgrade(move |socket| async move {
        if let Err(error) = handle_ws(state.sessions, session_id, socket).await {
            tracing::warn!(%session_id, error = %error, "websocket session ended with error");
        }
    })
}

async fn handle_ws(
    sessions: SessionStore,
    session_id: sift_protocol::SessionId,
    socket: WebSocket,
) -> ApiResult<()> {
    let (mut sender, mut receiver) = socket.split();
    while let Some(message) = receiver.next().await {
        let message = message.map_err(|e| ApiError::BadRequest(e.to_string()))?;
        match message {
            Message::Text(text) => {
                let msg: WsClientMessage =
                    serde_json::from_str(&text).map_err(|e| ApiError::BadRequest(e.to_string()))?;
                match msg {
                    WsClientMessage::Execute {
                        request_id,
                        connection,
                        sql,
                        params,
                    } => {
                        let stream = sessions
                            .execute_stream(session_id, connection, ExecuteRequest { sql, params })
                            .await?;
                        send_json(
                            &mut sender,
                            &WsServerMessage::Started {
                                request_id: request_id.clone(),
                                cursor_id: stream.cursor_id,
                            },
                        )
                        .await?;
                        stream_pages_with_ack(
                            &mut sender,
                            &mut receiver,
                            stream.cursor_id,
                            stream.rows,
                        )
                        .await?;
                    }
                    WsClientMessage::Cancel {
                        connection,
                        cursor_id,
                    } => sessions.cancel(session_id, connection, cursor_id).await?,
                    WsClientMessage::Ack { .. } => {
                        send_json(
                            &mut sender,
                            &WsServerMessage::Error {
                                request_id: None,
                                message: "unexpected ack without active stream".to_string(),
                            },
                        )
                        .await?;
                    }
                }
            }
            Message::Close(_) => break,
            Message::Ping(bytes) => sender
                .send(Message::Pong(bytes))
                .await
                .map_err(|e| ApiError::BadRequest(e.to_string()))?,
            Message::Pong(_) | Message::Binary(_) => {}
        }
    }
    Ok(())
}

async fn stream_pages_with_ack(
    sender: &mut futures::stream::SplitSink<WebSocket, Message>,
    receiver: &mut futures::stream::SplitStream<WebSocket>,
    cursor_id: sift_protocol::CursorId,
    mut rows: tokio::sync::mpsc::Receiver<sift_protocol::Page>,
) -> ApiResult<()> {
    let mut seq = 0_u64;
    while let Some(page) = rows.recv().await {
        send_json(
            sender,
            &WsServerMessage::Page {
                cursor_id,
                seq,
                page,
            },
        )
        .await?;
        wait_for_ack(receiver, cursor_id, seq).await?;
        seq += 1;
    }
    Ok(())
}

async fn wait_for_ack(
    receiver: &mut futures::stream::SplitStream<WebSocket>,
    cursor_id: sift_protocol::CursorId,
    seq: u64,
) -> ApiResult<()> {
    loop {
        let Some(message) = receiver.next().await else {
            return Err(ApiError::BadRequest("websocket closed before ack".into()));
        };
        let message = message.map_err(|e| ApiError::BadRequest(e.to_string()))?;
        match message {
            Message::Text(text) => match serde_json::from_str::<WsClientMessage>(&text)
                .map_err(|e| ApiError::BadRequest(e.to_string()))?
            {
                WsClientMessage::Ack {
                    cursor_id: ack_cursor,
                    seq: ack_seq,
                } if ack_cursor == cursor_id && ack_seq == seq => return Ok(()),
                WsClientMessage::Ack { .. } => {
                    return Err(ApiError::BadRequest("ack cursor or seq mismatch".into()));
                }
                WsClientMessage::Cancel {
                    connection: _,
                    cursor_id: _,
                } => {
                    return Err(ApiError::BadRequest(
                        "cancel during active stream must use HTTP cancel endpoint".into(),
                    ));
                }
                WsClientMessage::Execute { .. } => {
                    return Err(ApiError::BadRequest(
                        "concurrent execute on one websocket is not supported".into(),
                    ));
                }
            },
            Message::Close(_) => return Err(ApiError::BadRequest("websocket closed".into())),
            Message::Ping(_) | Message::Pong(_) | Message::Binary(_) => {}
        }
    }
}

async fn send_json<T: serde::Serialize>(
    sender: &mut futures::stream::SplitSink<WebSocket, Message>,
    value: &T,
) -> ApiResult<()> {
    let text = serde_json::to_string(value).map_err(|e| ApiError::Internal(e.to_string()))?;
    sender
        .send(Message::Text(text))
        .await
        .map_err(|e| ApiError::BadRequest(e.to_string()))
}
