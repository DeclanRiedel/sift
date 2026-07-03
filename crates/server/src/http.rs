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
use schemars::{schema_for, JsonSchema};
use serde::Deserialize;
use serde_json::json;
use std::time::Instant;

use sift_protocol::{
    AuditEntry, BeginTransactionRequest, BulkInsertRequest, CancelRequest, EndTransactionRequest,
    ExecuteRequest, ExecuteRequestHttp, Health, ObjectPath, OpenConnectionRequest,
    OpenSessionRequest, Operation, OperationStatus, SchemaFilter, SchemaScope, WsClientMessage,
    WsServerMessage, PROTOCOL_VERSION,
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
        .route("/v1/audit", get(list_audit))
        .route("/v1/operations", get(list_operations))
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
            "/v1/sessions/:id/connections/:conn_id/bulk-insert",
            post(bulk_insert),
        )
        .route(
            "/v1/sessions/:id/connections/:conn_id/schema",
            get(get_schema),
        )
        .route("/v1/sessions/:id/queries", post(execute_query))
        .route("/v1/sessions/:id/transactions", post(begin_transaction))
        .route(
            "/v1/sessions/:id/transactions/:tx_id/commit",
            post(commit_transaction),
        )
        .route(
            "/v1/sessions/:id/transactions/:tx_id/rollback",
            post(rollback_transaction),
        )
        .route("/v1/sessions/:id/ws", get(ws_session))
        .route(
            "/v1/sessions/:id/queries/:cursor_id/cancel",
            post(cancel_query),
        )
        .layer(from_fn_with_state(state.auth.clone(), auth_middleware))
        .layer(from_fn_with_state(state.sessions.clone(), audit_middleware))
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

async fn audit_middleware(
    State(sessions): State<SessionStore>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let method = req.method().to_string();
    let path = req.uri().path().to_string();
    let start = Instant::now();
    let response = next.run(req).await;
    let status = response.status().as_u16();
    sessions.push_audit(AuditEntry {
        at: chrono::Utc::now(),
        method,
        path,
        status,
        duration_ms: start.elapsed().as_millis(),
    });
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

async fn list_audit(State(state): State<AppState>) -> Json<Vec<AuditEntry>> {
    Json(state.sessions.list_audit())
}

async fn list_operations(
    State(state): State<AppState>,
) -> Json<Vec<sift_protocol::OperationAuditEntry>> {
    Json(state.sessions.list_operations())
}

async fn openapi() -> Json<serde_json::Value> {
    Json(json!({
        "openapi": "3.1.0",
        "info": {
            "title": "sift API",
            "version": VERSION
        },
        "x-sift-protocol-version": PROTOCOL_VERSION,
        "security": [{ "bearerAuth": [] }],
        "paths": {
            "/v1/health": {
                "get": {
                    "operationId": "health",
                    "summary": "Health and registered engines",
                    "responses": { "200": { "description": "Health", "content": json_content("Health") } }
                }
            },
            "/v1/audit": {
                "get": {
                    "operationId": "listAudit",
                    "summary": "List in-memory operation audit rows",
                    "responses": { "200": { "description": "Audit rows", "content": json_array_content("AuditEntry") } }
                }
            },
            "/v1/operations": {
                "get": {
                    "operationId": "listOperations",
                    "summary": "List replayable operation audit rows",
                    "responses": { "200": { "description": "Operation rows", "content": json_array_content("OperationAuditEntry") } }
                }
            },
            "/v1/sessions": {
                "get": {
                    "operationId": "listSessions",
                    "summary": "List sessions",
                    "responses": { "200": { "description": "Sessions", "content": json_array_content("SessionInfo") } }
                },
                "post": {
                    "operationId": "createSession",
                    "summary": "Create session",
                    "requestBody": json_body("OpenSessionRequest"),
                    "responses": { "200": { "description": "Session", "content": json_content("SessionInfo") } }
                }
            },
            "/v1/sessions/{id}": {
                "get": {
                    "operationId": "getSession",
                    "summary": "Get session",
                    "responses": { "200": { "description": "Session", "content": json_content("SessionInfo") } }
                },
                "delete": {
                    "operationId": "closeSession",
                    "summary": "Close session",
                    "responses": { "200": { "description": "Ack", "content": json_object_content() } }
                }
            },
            "/v1/sessions/{id}/connections": {
                "get": {
                    "operationId": "listConnections",
                    "summary": "List connections",
                    "responses": { "200": { "description": "Connections", "content": json_array_content("ConnectionInfo") } }
                },
                "post": {
                    "operationId": "openConnection",
                    "summary": "Open connection",
                    "requestBody": json_body("OpenConnectionRequest"),
                    "responses": { "200": { "description": "Connection", "content": json_content("ConnectionInfo") } }
                }
            },
            "/v1/sessions/{id}/connections/{conn_id}": {
                "delete": {
                    "operationId": "closeConnection",
                    "summary": "Close connection",
                    "responses": { "200": { "description": "Ack", "content": json_object_content() } }
                }
            },
            "/v1/sessions/{id}/connections/{conn_id}/ping": {
                "post": {
                    "operationId": "pingConnection",
                    "summary": "Ping connection",
                    "responses": { "200": { "description": "Server info", "content": json_content("ServerInfo") } }
                }
            },
            "/v1/sessions/{id}/connections/{conn_id}/bulk-insert": {
                "post": {
                    "operationId": "bulkInsert",
                    "summary": "Bulk insert rows into a SQL Server table",
                    "requestBody": json_body("BulkInsertRequest"),
                    "responses": { "200": { "description": "Bulk insert result", "content": json_content("BulkInsertResponse") } }
                }
            },
            "/v1/sessions/{id}/connections/{conn_id}/schema": {
                "get": {
                    "operationId": "getSchema",
                    "summary": "Fetch schema",
                    "parameters": [
                        { "name": "depth", "in": "query", "schema": { "type": "string", "enum": ["shallow", "deep"] } },
                        { "name": "schema", "in": "query", "schema": { "type": "string" } },
                        { "name": "object", "in": "query", "schema": { "type": "string" } },
                        { "name": "name_pattern", "in": "query", "schema": { "type": "string" } }
                    ],
                    "responses": { "200": { "description": "Schema snapshot", "content": json_content("SchemaSnapshot") } }
                }
            },
            "/v1/sessions/{id}/queries": {
                "post": {
                    "operationId": "executeQuery",
                    "summary": "Execute query over synchronous HTTP",
                    "requestBody": json_body("ExecuteRequestHttp"),
                    "responses": { "200": { "description": "Query result", "content": json_content("ExecuteResponse") } }
                }
            },
            "/v1/sessions/{id}/transactions": {
                "post": {
                    "operationId": "beginTransaction",
                    "summary": "Begin transaction",
                    "requestBody": json_body("BeginTransactionRequest"),
                    "responses": { "200": { "description": "Transaction", "content": json_content("TransactionInfo") } }
                }
            },
            "/v1/sessions/{id}/transactions/{tx_id}/commit": {
                "post": {
                    "operationId": "commitTransaction",
                    "summary": "Commit transaction",
                    "requestBody": json_body("EndTransactionRequest"),
                    "responses": { "200": { "description": "Ack", "content": json_object_content() } }
                }
            },
            "/v1/sessions/{id}/transactions/{tx_id}/rollback": {
                "post": {
                    "operationId": "rollbackTransaction",
                    "summary": "Rollback transaction",
                    "requestBody": json_body("EndTransactionRequest"),
                    "responses": { "200": { "description": "Ack", "content": json_object_content() } }
                }
            },
            "/v1/sessions/{id}/queries/{cursor_id}/cancel": {
                "post": {
                    "operationId": "cancelQuery",
                    "summary": "Cancel query",
                    "requestBody": json_body("CancelRequest"),
                    "responses": { "200": { "description": "Ack", "content": json_object_content() } }
                }
            },
            "/v1/sessions/{id}/ws": {
                "get": {
                    "operationId": "sessionWebSocket",
                    "summary": "WebSocket query stream; protocol uses WsClientMessage/WsServerMessage",
                    "responses": { "101": { "description": "WebSocket upgrade" } }
                }
            },
            "/v1/openapi.json": {
                "get": {
                    "operationId": "openapi",
                    "summary": "OpenAPI document",
                    "responses": { "200": { "description": "OpenAPI document" } }
                }
            }
        },
        "components": {
            "securitySchemes": {
                "bearerAuth": { "type": "http", "scheme": "bearer" }
            },
            "schemas": protocol_schema_refs()
        }
    }))
}

fn json_body(schema: &'static str) -> serde_json::Value {
    json!({
        "required": true,
        "content": {
            "application/json": {
                "schema": { "$ref": format!("#/components/schemas/{schema}") }
            }
        }
    })
}

fn json_content(schema: &'static str) -> serde_json::Value {
    json!({
        "application/json": {
            "schema": { "$ref": format!("#/components/schemas/{schema}") }
        }
    })
}

fn json_array_content(schema: &'static str) -> serde_json::Value {
    json!({
        "application/json": {
            "schema": {
                "type": "array",
                "items": { "$ref": format!("#/components/schemas/{schema}") }
            }
        }
    })
}

fn json_object_content() -> serde_json::Value {
    json!({
        "application/json": {
            "schema": { "type": "object" }
        }
    })
}

fn protocol_schema_refs() -> serde_json::Value {
    let mut schemas = serde_json::Map::new();
    add_schema::<sift_protocol::AuditEntry>("AuditEntry", &mut schemas);
    add_schema::<sift_protocol::BeginTransactionRequest>("BeginTransactionRequest", &mut schemas);
    add_schema::<sift_protocol::BulkInsertRequest>("BulkInsertRequest", &mut schemas);
    add_schema::<sift_protocol::BulkInsertResponse>("BulkInsertResponse", &mut schemas);
    add_schema::<sift_protocol::CancelRequest>("CancelRequest", &mut schemas);
    add_schema::<sift_protocol::ConnectionInfo>("ConnectionInfo", &mut schemas);
    add_schema::<sift_protocol::EndTransactionRequest>("EndTransactionRequest", &mut schemas);
    add_schema::<sift_protocol::ExecuteRequestHttp>("ExecuteRequestHttp", &mut schemas);
    add_schema::<sift_protocol::ExecuteResponse>("ExecuteResponse", &mut schemas);
    add_schema::<sift_protocol::Health>("Health", &mut schemas);
    add_schema::<sift_protocol::OpenConnectionRequest>("OpenConnectionRequest", &mut schemas);
    add_schema::<sift_protocol::OpenSessionRequest>("OpenSessionRequest", &mut schemas);
    add_schema::<sift_protocol::OperationAuditEntry>("OperationAuditEntry", &mut schemas);
    add_schema::<sift_protocol::SchemaSnapshot>("SchemaSnapshot", &mut schemas);
    add_schema::<sift_protocol::ServerInfo>("ServerInfo", &mut schemas);
    add_schema::<sift_protocol::SessionInfo>("SessionInfo", &mut schemas);
    add_schema::<sift_protocol::TransactionInfo>("TransactionInfo", &mut schemas);
    add_schema::<sift_protocol::WsClientMessage>("WsClientMessage", &mut schemas);
    add_schema::<sift_protocol::WsServerMessage>("WsServerMessage", &mut schemas);
    serde_json::Value::Object(schemas)
}

fn add_schema<T: JsonSchema>(
    name: &'static str,
    schemas: &mut serde_json::Map<String, serde_json::Value>,
) {
    let root = schema_for!(T);
    for (def_name, schema) in root.definitions {
        schemas
            .entry(def_name)
            .or_insert_with(|| serde_json::to_value(schema).expect("schema serializes"));
    }
    schemas.insert(
        name.to_string(),
        serde_json::to_value(root.schema).expect("schema serializes"),
    );
}

async fn create_session(
    State(state): State<AppState>,
    body: Option<Json<OpenSessionRequest>>,
) -> ApiResult<Json<sift_protocol::SessionInfo>> {
    let req = match body {
        Some(Json(b)) => b,
        None => OpenSessionRequest { tag: None },
    };
    let info = state.sessions.open_session(req.clone());
    state.sessions.push_operation(
        Operation::OpenSession { request: req },
        OperationStatus::Succeeded,
    );
    Ok(Json(info))
}

async fn list_sessions(
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<sift_protocol::SessionInfo>>> {
    let sessions = state.sessions.list_sessions();
    state
        .sessions
        .push_operation(Operation::ListSessions, OperationStatus::Succeeded);
    Ok(Json(sessions))
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
    state.sessions.push_operation(
        Operation::CloseSession { session: id },
        OperationStatus::Succeeded,
    );
    Ok(Json(json!({"ok": true})))
}

async fn open_connection(
    State(state): State<AppState>,
    Path(id): Path<sift_protocol::SessionId>,
    Json(req): Json<OpenConnectionRequest>,
) -> ApiResult<Json<sift_protocol::ConnectionInfo>> {
    let engine = req.engine;
    let operation = Operation::OpenConnection {
        session: id,
        request: req.clone(),
    };
    let spec = req.spec;
    let info = state.sessions.open_connection(id, engine, spec).await?;
    state
        .sessions
        .push_operation(operation, OperationStatus::Succeeded);
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
    state.sessions.push_operation(
        Operation::CloseConnection {
            session: id,
            connection: conn_id,
        },
        OperationStatus::Succeeded,
    );
    Ok(Json(json!({"ok": true})))
}

async fn ping_connection(
    State(state): State<AppState>,
    Path((id, conn_id)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
) -> ApiResult<Json<sift_protocol::ServerInfo>> {
    Ok(Json(state.sessions.ping(id, conn_id).await?))
}

async fn bulk_insert(
    State(state): State<AppState>,
    Path((id, conn_id)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Json(req): Json<BulkInsertRequest>,
) -> ApiResult<Json<sift_protocol::BulkInsertResponse>> {
    let operation = Operation::BulkInsert {
        session: id,
        connection: conn_id,
        request: req.clone(),
    };
    let response = state.sessions.bulk_insert(id, conn_id, req).await?;
    state
        .sessions
        .push_operation(operation, OperationStatus::Succeeded);
    Ok(Json(response))
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
    let snap = state.sessions.schema(id, conn_id, scope.clone()).await?;
    state.sessions.push_operation(
        Operation::RefreshSchema {
            session: id,
            connection: conn_id,
            scope,
        },
        OperationStatus::Succeeded,
    );
    Ok(Json(snap))
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
    let operation = Operation::ExecuteQuery {
        session: id,
        request: req.clone(),
    };
    let resp = state.sessions.execute_http(id, req).await?;
    state
        .sessions
        .push_operation(operation, OperationStatus::Succeeded);
    Ok(Json(resp))
}

async fn begin_transaction(
    State(state): State<AppState>,
    Path(id): Path<sift_protocol::SessionId>,
    Json(req): Json<BeginTransactionRequest>,
) -> ApiResult<Json<sift_protocol::TransactionInfo>> {
    let operation = Operation::BeginTransaction {
        session: id,
        request: req.clone(),
    };
    let tx = state.sessions.begin_transaction(id, req).await?;
    state
        .sessions
        .push_operation(operation, OperationStatus::Succeeded);
    Ok(Json(tx))
}

async fn commit_transaction(
    State(state): State<AppState>,
    Path((id, tx_id)): Path<(sift_protocol::SessionId, sift_protocol::TxId)>,
    Json(req): Json<EndTransactionRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    if req.tx_id != tx_id {
        return Err(ApiError::BadRequest(
            "`tx_id` body value must match tx id in path".into(),
        ));
    }
    state.sessions.commit_transaction(id, req.clone()).await?;
    state.sessions.push_operation(
        Operation::CommitTransaction {
            session: id,
            request: req,
        },
        OperationStatus::Succeeded,
    );
    Ok(Json(json!({"ok": true})))
}

async fn rollback_transaction(
    State(state): State<AppState>,
    Path((id, tx_id)): Path<(sift_protocol::SessionId, sift_protocol::TxId)>,
    Json(req): Json<EndTransactionRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    if req.tx_id != tx_id {
        return Err(ApiError::BadRequest(
            "`tx_id` body value must match tx id in path".into(),
        ));
    }
    state.sessions.rollback_transaction(id, req.clone()).await?;
    state.sessions.push_operation(
        Operation::RollbackTransaction {
            session: id,
            request: req,
        },
        OperationStatus::Succeeded,
    );
    Ok(Json(json!({"ok": true})))
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
    state.sessions.push_operation(
        Operation::CancelQuery {
            session: id,
            request: req,
        },
        OperationStatus::Succeeded,
    );
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
                        let stream = match sessions
                            .execute_stream(session_id, connection, ExecuteRequest { sql, params })
                            .await
                        {
                            Ok(stream) => stream,
                            Err(error) => {
                                send_json(
                                    &mut sender,
                                    &WsServerMessage::Error {
                                        request_id: Some(request_id),
                                        message: error.to_string(),
                                    },
                                )
                                .await?;
                                continue;
                            }
                        };
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
                    WsClientMessage::Listen {
                        request_id,
                        connection,
                        channels,
                    } => {
                        let stream =
                            match sessions.listen_pg(session_id, connection, channels).await {
                                Ok(stream) => stream,
                                Err(error) => {
                                    send_json(
                                        &mut sender,
                                        &WsServerMessage::Error {
                                            request_id: Some(request_id),
                                            message: error.to_string(),
                                        },
                                    )
                                    .await?;
                                    continue;
                                }
                            };
                        stream_notifications(&mut sender, request_id, stream.notifications).await?;
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

async fn stream_notifications(
    sender: &mut futures::stream::SplitSink<WebSocket, Message>,
    request_id: String,
    mut notifications: tokio::sync::mpsc::Receiver<sift_driver_api::PgNotification>,
) -> ApiResult<()> {
    while let Some(notification) = notifications.recv().await {
        send_json(
            sender,
            &WsServerMessage::Notification {
                request_id: request_id.clone(),
                channel: notification.channel,
                payload: notification.payload,
            },
        )
        .await?;
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
        let terminal = matches!(
            &page,
            sift_protocol::Page::Done { .. } | sift_protocol::Page::Error { .. }
        );
        send_json(
            sender,
            &WsServerMessage::Page {
                cursor_id,
                seq,
                page,
            },
        )
        .await?;
        if terminal {
            break;
        }
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
                WsClientMessage::Listen { .. } => {
                    return Err(ApiError::BadRequest(
                        "listen during active stream is not supported".into(),
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
