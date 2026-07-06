//! axum router + handlers. Routes versioned under `/v1`. The `AppState`
//! carries the `SessionStore` (which in turn carries the `DriverRegistry`).

use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{header, header::HeaderName, HeaderMap, HeaderValue, Request, StatusCode};
use axum::middleware::{from_fn, from_fn_with_state, Next};
use axum::response::Response;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use futures::{SinkExt, StreamExt};
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Instant;

use sift_metadata::{
    ApiTokenId, ConnectionProfileId, CrdtType, CredentialMode, Document, DocumentId, MetadataStore,
    NewConnectionProfile, NewDocument, NewRoom, PrincipalId, QueryHistory, Room, RoomId, RoomKind,
    RoomMember, RoomRole, TenantId, TenantMembership,
};
use sift_protocol::{
    AuditEntry, BeginTransactionRequest, BulkInsertRequest, CancelRequest, EndTransactionRequest,
    Engine, ExecuteRequest, ExecuteRequestHttp, Health, ObjectPath, OpenConnectionRequest,
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
    pub metadata: Option<MetadataStore>,
}

#[derive(Clone, Default)]
pub struct AuthState {
    pub bearer_token: Option<String>,
    pub loopback_bypass: bool,
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/audit", get(list_audit))
        .route("/v1/operations", get(list_operations))
        .route("/v1/openapi.json", get(openapi))
        .route("/v1/metadata/tenants", get(list_metadata_tenants))
        .route(
            "/v1/metadata/rooms",
            get(list_metadata_rooms).post(create_metadata_room),
        )
        .route("/v1/metadata/rooms/:id", delete(delete_metadata_room))
        .route(
            "/v1/metadata/rooms/:id/members",
            get(list_metadata_room_members).post(add_metadata_room_member),
        )
        .route(
            "/v1/metadata/rooms/:id/members/:principal_id",
            delete(remove_metadata_room_member),
        )
        .route("/v1/metadata/rooms/:id/join", post(join_metadata_room))
        .route("/v1/metadata/rooms/:id/leave", post(leave_metadata_room))
        .route(
            "/v1/metadata/rooms/:id/documents",
            get(list_metadata_documents).post(create_metadata_document),
        )
        .route(
            "/v1/metadata/documents/:id",
            put(update_metadata_document).delete(delete_metadata_document),
        )
        .route(
            "/v1/metadata/connections",
            get(list_metadata_connections).post(upsert_metadata_connection),
        )
        .route(
            "/v1/metadata/connections/:id",
            delete(delete_metadata_connection),
        )
        .route(
            "/v1/metadata/connections/:id/credential",
            post(set_metadata_connection_credential),
        )
        .route("/v1/metadata/history", get(list_metadata_history))
        .route(
            "/v1/auth/tokens",
            get(list_auth_tokens).post(issue_auth_token),
        )
        .route("/v1/auth/tokens/:id", delete(revoke_auth_token))
        .route("/v1/sessions", post(create_session).get(list_sessions))
        .route("/v1/sessions/:id", get(get_session).delete(close_session))
        .route(
            "/v1/sessions/:id/connections",
            post(open_connection).get(list_connections),
        )
        .route(
            "/v1/sessions/:id/connections/from-profile",
            post(open_connection_from_profile),
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
    let path = req.uri().path();
    if path.starts_with("/v1/metadata/") || path.starts_with("/v1/auth/") {
        return Ok(next.run(req).await);
    }
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

#[derive(Clone)]
struct AuthContext {
    principal_id: PrincipalId,
    tenants: Vec<TenantMembership>,
}

#[derive(Deserialize)]
struct TenantQuery {
    tenant: i64,
}

#[derive(Deserialize)]
struct RoomListQuery {
    tenant: i64,
}

#[derive(Deserialize)]
struct DeleteConnectionQuery {
    tenant: i64,
}

#[derive(Deserialize)]
struct HistoryQuery {
    room: Option<i64>,
    limit: Option<u32>,
}

#[derive(Deserialize)]
struct CreateRoomRequest {
    tenant_id: i64,
    name: String,
    kind: RoomKind,
}

#[derive(Deserialize)]
struct AddRoomMemberRequest {
    principal_id: i64,
    role: RoomRole,
}

#[derive(Deserialize)]
struct CreateDocumentRequest {
    kind: String,
    title: String,
    crdt_type: CrdtType,
    crdt_state: Vec<u8>,
    position: i64,
    connection_profile_id: Option<i64>,
}

#[derive(Deserialize)]
struct UpdateDocumentSnapshotRequest {
    crdt_state: Vec<u8>,
}

#[derive(Deserialize)]
struct UpsertConnectionProfileRequest {
    tenant_id: i64,
    name: String,
    engine: Engine,
    spec: sift_protocol::ConnectionSpec,
    credential_mode: CredentialMode,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Deserialize)]
struct SetCredentialRequest {
    secret: String,
}

#[derive(Deserialize)]
struct OpenConnectionFromProfileRequest {
    tenant_id: i64,
    profile_id: i64,
}

#[derive(Deserialize)]
struct IssueTokenRequest {
    name: String,
    tenant_id: Option<i64>,
    expires_at: Option<DateTime<Utc>>,
}

#[derive(Serialize)]
struct IssueTokenResponse {
    token: sift_metadata::ApiTokenRow,
    plaintext: String,
}

fn metadata_store(state: &AppState) -> ApiResult<&MetadataStore> {
    state.metadata.as_ref().ok_or(ApiError::MetadataUnavailable)
}

fn metadata_store_cloned(state: &AppState) -> ApiResult<MetadataStore> {
    state.metadata.clone().ok_or(ApiError::MetadataUnavailable)
}

async fn metadata_blocking<T>(f: impl FnOnce() -> ApiResult<T> + Send + 'static) -> ApiResult<T>
where
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|error| ApiError::Internal(format!("metadata task failed: {error}")))?
}

fn bearer_from_headers(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
}

fn resolve_auth_context(state: &AppState, headers: &HeaderMap) -> ApiResult<AuthContext> {
    let metadata = metadata_store(state)?;
    if let Some(token) = bearer_from_headers(headers) {
        if let Some(row) = metadata.verify_api_token(token)? {
            let tenants = metadata.list_principal_tenants(row.principal_id)?;
            return Ok(AuthContext {
                principal_id: row.principal_id,
                tenants,
            });
        }
    }

    if bearer_from_headers(headers).is_some_and(|token| {
        state
            .auth
            .bearer_token
            .as_deref()
            .is_some_and(|expected| token == expected)
    }) && state.auth.loopback_bypass
    {
        let principal = metadata
            .resolve_principal_by_external_id("local:1")?
            .ok_or(ApiError::Unauthorized)?;
        let tenants = metadata.list_principal_tenants(principal.id)?;
        return Ok(AuthContext {
            principal_id: principal.id,
            tenants,
        });
    }

    if state.auth.loopback_bypass {
        let principal = metadata
            .resolve_principal_by_external_id("local:1")?
            .ok_or(ApiError::Unauthorized)?;
        let tenants = metadata.list_principal_tenants(principal.id)?;
        return Ok(AuthContext {
            principal_id: principal.id,
            tenants,
        });
    }

    Err(ApiError::Unauthorized)
}

async fn resolve_auth_context_blocking(
    state: AppState,
    headers: HeaderMap,
) -> ApiResult<AuthContext> {
    metadata_blocking(move || resolve_auth_context(&state, &headers)).await
}

fn ensure_tenant(auth: &AuthContext, tenant: TenantId) -> ApiResult<()> {
    if auth
        .tenants
        .iter()
        .any(|membership| membership.tenant.id == tenant)
    {
        Ok(())
    } else {
        Err(ApiError::Forbidden(format!(
            "principal {:?} is not a member of tenant {:?}",
            auth.principal_id, tenant
        )))
    }
}

fn tenant_id(id: i64) -> ApiResult<TenantId> {
    if id > 0 {
        Ok(TenantId(id))
    } else {
        Err(ApiError::BadRequest("tenant id must be positive".into()))
    }
}

fn room_id(id: i64) -> ApiResult<RoomId> {
    if id > 0 {
        Ok(RoomId(id))
    } else {
        Err(ApiError::BadRequest("room id must be positive".into()))
    }
}

fn document_id(id: i64) -> ApiResult<DocumentId> {
    if id > 0 {
        Ok(DocumentId(id))
    } else {
        Err(ApiError::BadRequest("document id must be positive".into()))
    }
}

fn connection_profile_id(id: i64) -> ApiResult<ConnectionProfileId> {
    if id > 0 {
        Ok(ConnectionProfileId(id))
    } else {
        Err(ApiError::BadRequest(
            "connection profile id must be positive".into(),
        ))
    }
}

fn api_token_id(id: i64) -> ApiResult<ApiTokenId> {
    if id > 0 {
        Ok(ApiTokenId(id))
    } else {
        Err(ApiError::BadRequest("token id must be positive".into()))
    }
}

fn principal_id(id: i64) -> ApiResult<PrincipalId> {
    if id > 0 {
        Ok(PrincipalId(id))
    } else {
        Err(ApiError::BadRequest("principal id must be positive".into()))
    }
}

fn ensure_room_access(
    metadata: &MetadataStore,
    auth: &AuthContext,
    room: RoomId,
) -> ApiResult<Room> {
    let room = metadata.get_room(room)?;
    ensure_tenant(auth, room.tenant_id)?;
    Ok(room)
}

fn ensure_document_access(
    metadata: &MetadataStore,
    auth: &AuthContext,
    document: DocumentId,
) -> ApiResult<Document> {
    let document = metadata.get_document(document)?;
    ensure_room_access(metadata, auth, document.room_id)?;
    Ok(document)
}

fn push_metadata_operation(state: &AppState, action: &str, target: &str, id: Option<i64>) {
    state.sessions.push_operation(
        Operation::Metadata {
            action: action.to_string(),
            target: target.to_string(),
            id,
        },
        OperationStatus::Succeeded,
    );
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
            },
            "/v1/metadata/tenants": {
                "get": {
                    "operationId": "listMetadataTenants",
                    "summary": "List current principal tenant memberships",
                    "responses": { "200": { "description": "Tenant memberships", "content": json_array_object_content() } }
                }
            },
            "/v1/metadata/rooms": {
                "get": {
                    "operationId": "listMetadataRooms",
                    "summary": "List rooms for current principal in a tenant",
                    "responses": { "200": { "description": "Rooms", "content": json_array_object_content() } }
                },
                "post": {
                    "operationId": "createMetadataRoom",
                    "summary": "Create room",
                    "responses": { "200": { "description": "Room", "content": json_object_content() } }
                }
            },
            "/v1/metadata/rooms/{id}": {
                "delete": {
                    "operationId": "deleteMetadataRoom",
                    "summary": "Delete room",
                    "responses": { "200": { "description": "Ack", "content": json_object_content() } }
                }
            },
            "/v1/metadata/rooms/{id}/members": {
                "get": {
                    "operationId": "listMetadataRoomMembers",
                    "summary": "List room members",
                    "responses": { "200": { "description": "Room members", "content": json_array_object_content() } }
                },
                "post": {
                    "operationId": "addMetadataRoomMember",
                    "summary": "Add or update room member",
                    "responses": { "200": { "description": "Room member", "content": json_object_content() } }
                }
            },
            "/v1/metadata/rooms/{id}/members/{principal_id}": {
                "delete": {
                    "operationId": "removeMetadataRoomMember",
                    "summary": "Remove room member",
                    "responses": { "200": { "description": "Ack", "content": json_object_content() } }
                }
            },
            "/v1/metadata/rooms/{id}/join": {
                "post": {
                    "operationId": "joinMetadataRoom",
                    "summary": "Join room as current principal",
                    "responses": { "200": { "description": "Room member", "content": json_object_content() } }
                }
            },
            "/v1/metadata/rooms/{id}/leave": {
                "post": {
                    "operationId": "leaveMetadataRoom",
                    "summary": "Leave room as current principal",
                    "responses": { "200": { "description": "Ack", "content": json_object_content() } }
                }
            },
            "/v1/metadata/rooms/{id}/documents": {
                "get": {
                    "operationId": "listMetadataDocuments",
                    "summary": "List room documents",
                    "responses": { "200": { "description": "Documents", "content": json_array_object_content() } }
                },
                "post": {
                    "operationId": "createMetadataDocument",
                    "summary": "Create room document",
                    "responses": { "200": { "description": "Document", "content": json_object_content() } }
                }
            },
            "/v1/metadata/documents/{id}": {
                "put": {
                    "operationId": "updateMetadataDocument",
                    "summary": "Update document CRDT snapshot",
                    "responses": { "200": { "description": "Document", "content": json_object_content() } }
                },
                "delete": {
                    "operationId": "deleteMetadataDocument",
                    "summary": "Delete document",
                    "responses": { "200": { "description": "Ack", "content": json_object_content() } }
                }
            },
            "/v1/metadata/connections": {
                "get": {
                    "operationId": "listMetadataConnectionProfiles",
                    "summary": "List connection profiles",
                    "responses": { "200": { "description": "Connection profiles", "content": json_array_object_content() } }
                },
                "post": {
                    "operationId": "upsertMetadataConnectionProfile",
                    "summary": "Create or replace connection profile",
                    "responses": { "200": { "description": "Connection profile", "content": json_object_content() } }
                }
            },
            "/v1/metadata/connections/{id}": {
                "delete": {
                    "operationId": "deleteMetadataConnectionProfile",
                    "summary": "Delete connection profile",
                    "responses": { "200": { "description": "Ack", "content": json_object_content() } }
                }
            },
            "/v1/metadata/connections/{id}/credential": {
                "post": {
                    "operationId": "setMetadataConnectionCredential",
                    "summary": "Set per-user credential for connection profile",
                    "responses": { "200": { "description": "Ack", "content": json_object_content() } }
                }
            },
            "/v1/metadata/history": {
                "get": {
                    "operationId": "listMetadataHistory",
                    "summary": "List query history by room or current principal",
                    "responses": { "200": { "description": "Query history", "content": json_array_object_content() } }
                }
            },
            "/v1/auth/tokens": {
                "get": {
                    "operationId": "listAuthTokens",
                    "summary": "List current principal API tokens",
                    "responses": { "200": { "description": "API tokens", "content": json_array_object_content() } }
                },
                "post": {
                    "operationId": "issueAuthToken",
                    "summary": "Issue API token; plaintext returned once",
                    "responses": { "200": { "description": "Issued token", "content": json_object_content() } }
                }
            },
            "/v1/auth/tokens/{id}": {
                "delete": {
                    "operationId": "revokeAuthToken",
                    "summary": "Revoke API token",
                    "responses": { "200": { "description": "Ack", "content": json_object_content() } }
                }
            },
            "/v1/sessions/{id}/connections/from-profile": {
                "post": {
                    "operationId": "openConnectionFromProfile",
                    "summary": "Open session connection from metadata profile",
                    "responses": { "200": { "description": "Connection", "content": json_content("ConnectionInfo") } }
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

fn json_array_object_content() -> serde_json::Value {
    json!({
        "application/json": {
            "schema": {
                "type": "array",
                "items": { "type": "object" }
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

async fn list_metadata_tenants(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<TenantMembership>>> {
    let auth = resolve_auth_context_blocking(state, headers).await?;
    Ok(Json(auth.tenants))
}

async fn list_metadata_rooms(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<RoomListQuery>,
) -> ApiResult<Json<Vec<Room>>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state, headers).await?;
    let tenant = tenant_id(q.tenant)?;
    ensure_tenant(&auth, tenant)?;
    Ok(Json(
        metadata_blocking(move || {
            metadata
                .list_rooms_for_principal(tenant, auth.principal_id)
                .map_err(Into::into)
        })
        .await?,
    ))
}

async fn create_metadata_room(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateRoomRequest>,
) -> ApiResult<Json<Room>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let tenant = tenant_id(req.tenant_id)?;
    ensure_tenant(&auth, tenant)?;
    let room = metadata_blocking(move || {
        metadata
            .create_room(
                tenant,
                auth.principal_id,
                NewRoom {
                    name: req.name,
                    kind: req.kind,
                },
            )
            .map_err(Into::into)
    })
    .await?;
    push_metadata_operation(&state, "create", "room", Some(room.id.0));
    Ok(Json(room))
}

async fn delete_metadata_room(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let room = room_id(id)?;
    metadata_blocking(move || {
        ensure_room_access(&metadata, &auth, room)?;
        metadata.delete_room(room)?;
        Ok(())
    })
    .await?;
    push_metadata_operation(&state, "delete", "room", Some(room.0));
    Ok(Json(json!({"ok": true})))
}

async fn list_metadata_room_members(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<RoomMember>>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state, headers).await?;
    let room = room_id(id)?;
    Ok(Json(
        metadata_blocking(move || {
            ensure_room_access(&metadata, &auth, room)?;
            metadata.list_room_members(room).map_err(Into::into)
        })
        .await?,
    ))
}

async fn add_metadata_room_member(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<AddRoomMemberRequest>,
) -> ApiResult<Json<RoomMember>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let room = room_id(id)?;
    let principal = principal_id(req.principal_id)?;
    let member = metadata_blocking(move || {
        ensure_room_access(&metadata, &auth, room)?;
        metadata
            .add_room_member(room, principal, req.role)
            .map_err(Into::into)
    })
    .await?;
    push_metadata_operation(&state, "add_member", "room", Some(room.0));
    Ok(Json(member))
}

async fn remove_metadata_room_member(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, principal)): Path<(i64, i64)>,
) -> ApiResult<Json<serde_json::Value>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let room = room_id(id)?;
    let principal = principal_id(principal)?;
    metadata_blocking(move || {
        ensure_room_access(&metadata, &auth, room)?;
        metadata.remove_room_member(room, principal)?;
        Ok(())
    })
    .await?;
    push_metadata_operation(&state, "remove_member", "room", Some(room.0));
    Ok(Json(json!({"ok": true})))
}

async fn join_metadata_room(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<RoomMember>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let room = room_id(id)?;
    let principal = auth.principal_id;
    let member = metadata_blocking(move || {
        let room_row = metadata.get_room(room)?;
        ensure_tenant(&auth, room_row.tenant_id)?;
        metadata
            .add_room_member(room, principal, RoomRole::Editor)
            .map_err(Into::into)
    })
    .await?;
    push_metadata_operation(&state, "join", "room", Some(room.0));
    Ok(Json(member))
}

async fn leave_metadata_room(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let room = room_id(id)?;
    let principal = auth.principal_id;
    metadata_blocking(move || {
        ensure_room_access(&metadata, &auth, room)?;
        metadata.remove_room_member(room, principal)?;
        Ok(())
    })
    .await?;
    push_metadata_operation(&state, "leave", "room", Some(room.0));
    Ok(Json(json!({"ok": true})))
}

async fn list_metadata_documents(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<Document>>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state, headers).await?;
    let room = room_id(id)?;
    Ok(Json(
        metadata_blocking(move || {
            ensure_room_access(&metadata, &auth, room)?;
            metadata.list_documents(room).map_err(Into::into)
        })
        .await?,
    ))
}

async fn create_metadata_document(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<CreateDocumentRequest>,
) -> ApiResult<Json<Document>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let room = room_id(id)?;
    let document = metadata_blocking(move || {
        ensure_room_access(&metadata, &auth, room)?;
        metadata
            .create_document(
                room,
                NewDocument {
                    kind: req.kind,
                    title: req.title,
                    crdt_type: req.crdt_type,
                    crdt_state: req.crdt_state,
                    position: req.position,
                    connection_profile_id: req.connection_profile_id.map(ConnectionProfileId),
                },
            )
            .map_err(Into::into)
    })
    .await?;
    push_metadata_operation(&state, "create", "document", Some(document.id.0));
    Ok(Json(document))
}

async fn update_metadata_document(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<UpdateDocumentSnapshotRequest>,
) -> ApiResult<Json<Document>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let document = document_id(id)?;
    let updated = metadata_blocking(move || {
        ensure_document_access(&metadata, &auth, document)?;
        metadata
            .update_document_snapshot(document, req.crdt_state)
            .map_err(Into::into)
    })
    .await?;
    push_metadata_operation(&state, "update", "document", Some(document.0));
    Ok(Json(updated))
}

async fn delete_metadata_document(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let document = document_id(id)?;
    metadata_blocking(move || {
        ensure_document_access(&metadata, &auth, document)?;
        metadata.delete_document(document)?;
        Ok(())
    })
    .await?;
    push_metadata_operation(&state, "delete", "document", Some(document.0));
    Ok(Json(json!({"ok": true})))
}

async fn list_metadata_connections(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TenantQuery>,
) -> ApiResult<Json<Vec<sift_metadata::ConnectionProfile>>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state, headers).await?;
    let tenant = tenant_id(q.tenant)?;
    ensure_tenant(&auth, tenant)?;
    Ok(Json(
        metadata_blocking(move || {
            metadata
                .list_connection_profiles(tenant)
                .map_err(Into::into)
        })
        .await?,
    ))
}

async fn upsert_metadata_connection(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<UpsertConnectionProfileRequest>,
) -> ApiResult<Json<sift_metadata::ConnectionProfile>> {
    let metadata = metadata_store(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let tenant = tenant_id(req.tenant_id)?;
    ensure_tenant(&auth, tenant)?;
    let profile = metadata
        .upsert_connection_profile(
            tenant,
            auth.principal_id,
            NewConnectionProfile {
                name: req.name,
                engine: req.engine,
                spec: req.spec,
                credential_mode: req.credential_mode,
                tags: req.tags,
            },
        )
        .await?;
    push_metadata_operation(&state, "upsert", "connection_profile", Some(profile.id.0));
    Ok(Json(profile))
}

async fn delete_metadata_connection(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(q): Query<DeleteConnectionQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let metadata = metadata_store(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let tenant = tenant_id(q.tenant)?;
    ensure_tenant(&auth, tenant)?;
    let profile = connection_profile_id(id)?;
    metadata.delete_connection_profile(tenant, profile).await?;
    push_metadata_operation(&state, "delete", "connection_profile", Some(profile.0));
    Ok(Json(json!({"ok": true})))
}

async fn set_metadata_connection_credential(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<SetCredentialRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let metadata = metadata_store(&state)?;
    let metadata_sync = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let profile_id = connection_profile_id(id)?;
    let profile = metadata_blocking(move || {
        metadata_sync
            .get_connection_profile_for_any_tenant(profile_id)
            .map_err(Into::into)
    })
    .await?;
    ensure_tenant(&auth, profile.tenant_id)?;
    metadata
        .set_per_user_credential(profile_id, auth.principal_id, req.secret.as_bytes())
        .await?;
    push_metadata_operation(
        &state,
        "set_credential",
        "connection_profile",
        Some(profile_id.0),
    );
    Ok(Json(json!({"ok": true})))
}

async fn list_metadata_history(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<HistoryQuery>,
) -> ApiResult<Json<Vec<QueryHistory>>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state, headers).await?;
    let limit = q.limit.unwrap_or(100).min(500);
    Ok(Json(
        metadata_blocking(move || {
            if let Some(room) = q.room {
                let room = room_id(room)?;
                ensure_room_access(&metadata, &auth, room)?;
                metadata
                    .list_query_history_for_room(room, limit)
                    .map_err(Into::into)
            } else {
                metadata
                    .list_query_history_for_principal(auth.principal_id, limit)
                    .map_err(Into::into)
            }
        })
        .await?,
    ))
}

async fn list_auth_tokens(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<sift_metadata::ApiTokenRow>>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state, headers).await?;
    Ok(Json(
        metadata_blocking(move || {
            metadata
                .list_api_tokens(auth.principal_id)
                .map_err(Into::into)
        })
        .await?,
    ))
}

async fn issue_auth_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<IssueTokenRequest>,
) -> ApiResult<Json<IssueTokenResponse>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let tenant = req.tenant_id.map(tenant_id).transpose()?;
    if let Some(tenant) = tenant {
        ensure_tenant(&auth, tenant)?;
    }
    let (token, plaintext) = metadata_blocking(move || {
        metadata
            .issue_api_token(auth.principal_id, tenant, &req.name, req.expires_at)
            .map_err(Into::into)
    })
    .await?;
    push_metadata_operation(&state, "issue", "api_token", Some(token.id.0));
    Ok(Json(IssueTokenResponse { token, plaintext }))
}

async fn revoke_auth_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let token_id = api_token_id(id)?;
    metadata_blocking(move || {
        if !metadata
            .list_api_tokens(auth.principal_id)?
            .iter()
            .any(|token| token.id == token_id)
        {
            return Err(ApiError::Forbidden(
                "cannot revoke another principal's token".into(),
            ));
        }
        metadata.revoke_api_token(token_id)?;
        Ok(())
    })
    .await?;
    push_metadata_operation(&state, "revoke", "api_token", Some(token_id.0));
    Ok(Json(json!({"ok": true})))
}

async fn open_connection_from_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<sift_protocol::SessionId>,
    Json(req): Json<OpenConnectionFromProfileRequest>,
) -> ApiResult<Json<sift_protocol::ConnectionInfo>> {
    let metadata = metadata_store(&state)?;
    let metadata_sync = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let tenant = tenant_id(req.tenant_id)?;
    ensure_tenant(&auth, tenant)?;
    let profile_id = connection_profile_id(req.profile_id)?;
    let profile = metadata_blocking(move || {
        metadata_sync
            .get_connection_profile(tenant, profile_id)
            .map_err(Into::into)
    })
    .await?;
    let spec = metadata
        .resolve_connection_spec(tenant, auth.principal_id, profile_id)
        .await?;
    let info = state
        .sessions
        .open_connection(session_id, profile.engine, spec)
        .await?;
    push_metadata_operation(&state, "open", "connection_profile", Some(profile_id.0));
    Ok(Json(info))
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
