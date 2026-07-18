//! axum router + handlers. Routes versioned under `/v1`. The `AppState`
//! carries the `SessionStore` (which in turn carries the `DriverRegistry`).

use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{header, header::HeaderName, HeaderMap, HeaderValue, Request, StatusCode};
use axum::middleware::{from_fn, from_fn_with_state, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use futures::{SinkExt, StreamExt};
use schemars::{schema_for, JsonSchema};
use serde::Deserialize;
use serde_json::json;
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use tokio::sync::Semaphore;

use sift_doc::{CrdtKind, DocumentSnapshot, TextDocument, TextOperation};
use sift_metadata::{
    ApiTokenId, ConnectionProfileId, CrdtType, Document, DocumentId, MetadataStore,
    NewConnectionProfile, NewDocument, NewQueryHistory, NewRoom, NewSavedQuery, PrincipalId,
    QueryHistory, QueryStatus, Room, RoomId, RoomKind, RoomMember, RoomRole, SavedQuery,
    SavedQueryFilter, SavedQueryId, SavedQueryScope, TenantId, TenantMembership, UpdateSavedQuery,
};
use sift_protocol::{
    AuditEntry, BeginTransactionRequest, BulkInsertRequest, CancelRequest,
    DocumentOperationEnvelope, EndTransactionRequest, ExecuteRequest, ExecuteRequestHttp, Health,
    ObjectPath, OpenConnectionRequest, OpenSessionRequest, Operation, OperationStatus, Readiness,
    RoomClientMessage, RoomQueryResult, RoomQueryStatus, RoomServerMessage, SavepointRequest,
    SchemaFilter, SchemaScope, WsClientMessage, WsServerMessage, PROTOCOL_VERSION,
};

use crate::error::{ApiError, ApiResult};
use crate::room_runtime::RoomRuntime;
use crate::session::SessionStore;
use crate::VERSION;

const MAX_METADATA_BLOCKING_TASKS: usize = 16;
static METADATA_BLOCKING_PERMITS: OnceLock<Arc<Semaphore>> = OnceLock::new();

#[derive(Clone)]
pub struct AppState {
    pub sessions: SessionStore,
    pub rooms: RoomRuntime,
    pub auth: AuthState,
    pub metadata: Option<MetadataStore>,
    /// Graceful-shutdown drain state (ADR-018). New work is refused once this
    /// flips to draining; query execution is tracked so shutdown can wait.
    pub shutdown: crate::shutdown::Shutdown,
}

#[derive(Clone, Default)]
pub struct AuthState {
    pub bearer_token: Option<String>,
    pub loopback_bypass: bool,
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/ready", get(ready))
        .route("/v1/audit", get(list_audit))
        .route("/v1/operations", get(list_operations))
        .route("/v1/operations/audit", get(list_operation_audit_log))
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
        .route("/v1/metadata/rooms/:id/ws", get(ws_room))
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
            "/v1/metadata/saved-queries",
            get(list_metadata_saved_queries).post(create_metadata_saved_query),
        )
        .route(
            "/v1/metadata/saved-queries/:id",
            get(get_metadata_saved_query)
                .put(update_metadata_saved_query)
                .delete(delete_metadata_saved_query),
        )
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
        .route(
            "/v1/sessions/:id/connections/:conn_id/ddl",
            get(get_object_ddl),
        )
        .route(
            "/v1/sessions/:id/connections/:conn_id/complete",
            post(post_completion),
        )
        .route(
            "/v1/sessions/:id/connections/:conn_id/export",
            post(export_query),
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
        .route(
            "/v1/sessions/:id/transactions/:tx_id/savepoints",
            post(create_savepoint),
        )
        .route(
            "/v1/sessions/:id/transactions/:tx_id/savepoints/rollback",
            post(rollback_to_savepoint),
        )
        .route(
            "/v1/sessions/:id/transactions/:tx_id/savepoints/release",
            post(release_savepoint),
        )
        .route("/v1/sessions/:id/ws", get(ws_session))
        .route(
            "/v1/sessions/:id/queries/:cursor_id/cancel",
            post(cancel_query),
        )
        .route("/v1/cursors/:cursor_id/pages", get(read_spill_pages))
        .route("/v1/cursors/:cursor_id", delete(delete_spilled_cursor))
        .layer(from_fn_with_state(state.auth.clone(), auth_middleware))
        .layer(from_fn(inject_peer_addr))
        .layer(from_fn_with_state(state.sessions.clone(), audit_middleware))
        .layer(from_fn(protocol_version_middleware))
        .layer(from_fn(correlation_middleware))
        // gzip/br compression on HTTP responses when the client advertises
        // support via Accept-Encoding. WS frames are untouched (upgraded
        // connections bypass response compression layers).
        .layer(
            tower_http::compression::CompressionLayer::new()
                .gzip(true)
                .br(true),
        )
        .with_state(state)
}

/// Internal header carrying the trusted peer IP. Any client-supplied value
/// is stripped before we set this — handlers may treat it as authoritative.
const PEER_ADDR_HEADER: HeaderName = HeaderName::from_static("x-sift-peer-addr");

async fn inject_peer_addr(
    peer: Option<axum::extract::ConnectInfo<std::net::SocketAddr>>,
    mut req: Request<Body>,
    next: Next,
) -> Response {
    req.headers_mut().remove(&PEER_ADDR_HEADER);
    // Absent ConnectInfo => in-process caller (e.g. tower::oneshot in tests),
    // treated as loopback. Real network path always has ConnectInfo when the
    // server is started via `into_make_service_with_connect_info`; if a future
    // refactor drops that wiring, remote requests would be authenticated as
    // loopback under the default loopback_bypass=true. Emit a warn so the
    // regression is at least noticeable in logs.
    let ip = peer
        .map(|axum::extract::ConnectInfo(addr)| addr.ip())
        .unwrap_or_else(|| {
            tracing::warn!(
                "request lacks ConnectInfo; falling back to loopback — \
                 verify serve() uses into_make_service_with_connect_info"
            );
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
        });
    if let Ok(value) = HeaderValue::from_str(&ip.to_string()) {
        req.headers_mut().insert(PEER_ADDR_HEADER.clone(), value);
    }
    next.run(req).await
}

fn peer_is_loopback(headers: &HeaderMap) -> bool {
    headers
        .get(&PEER_ADDR_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<std::net::IpAddr>().ok())
        .is_some_and(|ip| ip.is_loopback())
}

const PROTOCOL_VERSION_HEADER: HeaderName = HeaderName::from_static("x-sift-protocol-version");

/// Protocol version negotiation (ADR-016). A request may pin a version via the
/// `x-sift-protocol-version` header; a mismatch is rejected before routing.
/// Absent header = unpinned = proceed. The server's version is always
/// advertised on the response.
async fn protocol_version_middleware(req: Request<Body>, next: Next) -> Response {
    if let Some(requested) = req
        .headers()
        .get(&PROTOCOL_VERSION_HEADER)
        .and_then(|value| value.to_str().ok())
    {
        if requested != PROTOCOL_VERSION {
            return ApiError::UnsupportedProtocolVersion {
                requested: requested.to_string(),
            }
            .into_response();
        }
    }
    let mut response = next.run(req).await;
    response.headers_mut().insert(
        PROTOCOL_VERSION_HEADER,
        HeaderValue::from_static(PROTOCOL_VERSION),
    );
    response
}

/// Accept-or-generate a request correlation ID (ADR step 5). The ID is put on
/// the request's tracing span, made available to handlers and audit writes via
/// a task-local, and echoed back in the response header.
async fn correlation_middleware(req: Request<Body>, next: Next) -> Response {
    use tracing::Instrument;

    let id = req
        .headers()
        .get(&crate::correlation::CORRELATION_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(crate::correlation::sanitize)
        .unwrap_or_else(crate::correlation::generate);
    let span = tracing::info_span!("request", correlation_id = %id);
    let mut response = crate::correlation::scope(id.clone(), next.run(req))
        .instrument(span)
        .await;
    if let Ok(value) = HeaderValue::from_str(&id) {
        response
            .headers_mut()
            .insert(crate::correlation::CORRELATION_HEADER, value);
    }
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
        .is_some_and(|actual| constant_time_eq(actual.as_bytes(), expected.as_bytes()));
    if valid {
        Ok(next.run(req).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

/// Constant-time equality for the static bearer token, so the auth check is
/// not a timing oracle for the token. Both sides are hashed to a fixed-width
/// digest first, so neither the length nor the content leaks through timing.
fn constant_time_eq(actual: &[u8], expected: &[u8]) -> bool {
    use sha2::{Digest, Sha256};
    let a = Sha256::digest(actual);
    let b = Sha256::digest(expected);
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[derive(Clone)]
struct AuthContext {
    principal_id: PrincipalId,
    tenants: Vec<TenantMembership>,
}

#[derive(Clone)]
struct ExecuteMetadataContext {
    metadata: MetadataStore,
    principal_id: PrincipalId,
    room_id: Option<RoomId>,
    connection_profile_id: Option<ConnectionProfileId>,
}

#[derive(Deserialize, JsonSchema)]
struct TenantQuery {
    tenant: i64,
}

#[derive(Deserialize, JsonSchema)]
struct RoomListQuery {
    tenant: i64,
}

#[derive(Deserialize, JsonSchema)]
struct DeleteConnectionQuery {
    tenant: i64,
}

#[derive(Deserialize, JsonSchema)]
struct HistoryQuery {
    room: Option<i64>,
    limit: Option<u32>,
}

use sift_metadata::http::{
    AddRoomMemberRequest, CreateDocumentRequest, CreateRoomRequest, CreateSavedQueryRequest,
    IssueTokenRequest, IssueTokenResponse, OpenConnectionFromProfileRequest, SetCredentialRequest,
    UpdateDocumentSnapshotRequest, UpdateSavedQueryRequest, UpsertConnectionProfileRequest,
};

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
    let permit = METADATA_BLOCKING_PERMITS
        .get_or_init(|| Arc::new(Semaphore::new(MAX_METADATA_BLOCKING_TASKS)))
        .clone()
        .acquire_owned()
        .await
        .map_err(|error| ApiError::Internal(format!("metadata runtime closed: {error}")))?;
    let result = tokio::task::spawn_blocking(f)
        .await
        .map_err(|error| ApiError::Internal(format!("metadata task failed: {error}")))?;
    drop(permit);
    result
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
            let mut tenants = metadata.list_principal_tenants(row.principal_id)?;
            if let Some(scope) = row.tenant_id {
                tenants.retain(|membership| membership.tenant.id == scope);
            }
            return Ok(AuthContext {
                principal_id: row.principal_id,
                tenants,
            });
        }
    }

    // Loopback bypass: only trust when the peer is actually on the loopback
    // interface. Without this check, `loopback_bypass = true` (the default)
    // authenticates any unauthenticated client — remote or local — as the
    // local principal.
    let bypass_allowed = state.auth.loopback_bypass && peer_is_loopback(headers);

    if bearer_from_headers(headers).is_some_and(|token| {
        state
            .auth
            .bearer_token
            .as_deref()
            .is_some_and(|expected| token == expected)
    }) && bypass_allowed
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

    if bypass_allowed {
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

async fn optional_auth_context_blocking(
    state: AppState,
    headers: HeaderMap,
) -> ApiResult<Option<AuthContext>> {
    if state.metadata.is_none() {
        return Ok(None);
    }
    match resolve_auth_context_blocking(state, headers).await {
        Ok(auth) => Ok(Some(auth)),
        Err(ApiError::Unauthorized) => Ok(None),
        Err(error) => Err(error),
    }
}

async fn session_auth_context_blocking(
    state: AppState,
    headers: HeaderMap,
) -> ApiResult<Option<AuthContext>> {
    if state.metadata.is_some() {
        resolve_auth_context_blocking(state, headers)
            .await
            .map(Some)
    } else {
        Ok(None)
    }
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

fn saved_query_id(id: i64) -> ApiResult<SavedQueryId> {
    if id > 0 {
        Ok(SavedQueryId(id))
    } else {
        Err(ApiError::BadRequest(
            "saved query id must be positive".into(),
        ))
    }
}

/// True if the caller has an elevated role (Owner or Admin) in
/// `tenant`. Used to gate tenant-shared saved-query edits.
fn is_tenant_admin(auth: &AuthContext, tenant: TenantId) -> bool {
    use sift_metadata::MembershipRole;
    auth.tenants.iter().any(|m| {
        m.tenant.id == tenant && matches!(m.role, MembershipRole::Owner | MembershipRole::Admin)
    })
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

#[derive(Clone, Copy)]
enum RoomPermission {
    Read,
    Write,
    Admin,
}

fn ensure_room_permission(
    metadata: &MetadataStore,
    auth: &AuthContext,
    room: RoomId,
    permission: RoomPermission,
) -> ApiResult<Room> {
    let room_row = ensure_room_access(metadata, auth, room)?;
    let Some(member) = metadata.get_room_member(room, auth.principal_id)? else {
        return Err(ApiError::Forbidden(format!(
            "principal {:?} is not a member of room {:?}",
            auth.principal_id, room
        )));
    };
    if room_role_allows(&member.role, permission) {
        Ok(room_row)
    } else {
        Err(ApiError::Forbidden(format!(
            "room role {:?} cannot perform this action in room {:?}",
            member.role, room
        )))
    }
}

fn room_role_allows(role: &RoomRole, permission: RoomPermission) -> bool {
    match permission {
        RoomPermission::Read => {
            matches!(role, RoomRole::Owner | RoomRole::Editor | RoomRole::Viewer)
        }
        RoomPermission::Write => matches!(role, RoomRole::Owner | RoomRole::Editor),
        RoomPermission::Admin => matches!(role, RoomRole::Owner),
    }
}

fn ensure_document_access(
    metadata: &MetadataStore,
    auth: &AuthContext,
    document: DocumentId,
    permission: RoomPermission,
) -> ApiResult<Document> {
    let document = metadata.get_document(document)?;
    ensure_room_permission(metadata, auth, document.room_id, permission)?;
    Ok(document)
}

fn push_metadata_operation(
    state: &AppState,
    actor: PrincipalId,
    action: &str,
    target: &str,
    id: Option<i64>,
) {
    state.sessions.push_operation_full(
        Operation::Metadata {
            action: action.to_string(),
            target: target.to_string(),
            id,
        },
        OperationStatus::Succeeded,
        Some(actor.0),
        None,
        None,
        None,
    );
}

async fn execute_metadata_context(
    state: &AppState,
    headers: HeaderMap,
    req: &ExecuteRequestHttp,
) -> ApiResult<Option<ExecuteMetadataContext>> {
    if req.room_id.is_none() && req.connection_profile_id.is_none() {
        return Ok(None);
    }

    let metadata = metadata_store_cloned(state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let room = req.room_id.map(room_id).transpose()?;
    let profile = req
        .connection_profile_id
        .map(connection_profile_id)
        .transpose()?;
    let metadata_for_check = metadata.clone();
    let auth_for_check = auth.clone();
    metadata_blocking(move || {
        if let Some(room) = room {
            ensure_room_permission(
                &metadata_for_check,
                &auth_for_check,
                room,
                RoomPermission::Write,
            )?;
        }
        if let Some(profile) = profile {
            let profile = metadata_for_check.get_connection_profile_for_any_tenant(profile)?;
            ensure_tenant(&auth_for_check, profile.tenant_id)?;
        }
        Ok(())
    })
    .await?;

    Ok(Some(ExecuteMetadataContext {
        metadata,
        principal_id: auth.principal_id,
        room_id: room,
        connection_profile_id: profile,
    }))
}

async fn record_execute_history(
    context: ExecuteMetadataContext,
    sql_text: String,
    duration_ms: i64,
    result: &ApiResult<sift_protocol::ExecuteResponse>,
) {
    let (status, row_count, error_code, error_message) = match result {
        Ok(response) => (
            QueryStatus::Ok,
            Some(response.rows.len() as i64),
            None,
            None,
        ),
        Err(ApiError::Driver(error)) => (
            QueryStatus::Error,
            None,
            Some(error.code.to_string()),
            Some(error.message.clone()),
        ),
        Err(error) => (QueryStatus::Error, None, None, Some(error.to_string())),
    };
    let record = NewQueryHistory {
        principal_id: context.principal_id,
        room_id: context.room_id,
        connection_profile_id: context.connection_profile_id,
        sql_text,
        duration_ms: Some(duration_ms),
        row_count,
        status,
        error_code,
        error_message,
    };
    if let Err(error) = metadata_blocking(move || {
        context
            .metadata
            .record_query_history(record)
            .map(|_| ())
            .map_err(Into::into)
    })
    .await
    {
        tracing::warn!(%error, "failed to record query history");
    }
}

fn room_query_result(
    context: &ExecuteMetadataContext,
    sql_text: String,
    result: &ApiResult<sift_protocol::ExecuteResponse>,
) -> Option<RoomQueryResult> {
    let room_id = context.room_id?;
    let (status, row_count, error_message) = match result {
        Ok(response) => (RoomQueryStatus::Ok, Some(response.rows.len() as i64), None),
        Err(error) => (RoomQueryStatus::Error, None, Some(error.to_string())),
    };
    Some(RoomQueryResult {
        room_id: room_id.0,
        actor_principal_id: context.principal_id.0,
        connection_profile_id: context.connection_profile_id.map(|id| id.0),
        sql_text,
        row_count,
        status,
        error_message,
    })
}

async fn health(State(state): State<AppState>) -> Json<Health> {
    Json(Health {
        status: "ok".to_string(),
        version: VERSION.to_string(),
        engines: state.sessions.registry().engines(),
    })
}

/// Readiness probe (ADR-018): `200` when the server should take traffic,
/// `503` otherwise. Not ready while draining, when no driver is registered,
/// or when the (enabled) metadata store is unreachable. The `Readiness` body
/// is returned in both cases so callers can see which check failed.
async fn ready(State(state): State<AppState>) -> Response {
    let draining = state.shutdown.is_draining();
    let engines = state.sessions.registry().engines();
    let drivers_registered = !engines.is_empty();
    let metadata_ok = match state.metadata.clone() {
        None => None,
        Some(store) => Some(
            metadata_blocking(move || store.health_check().map_err(Into::into))
                .await
                .is_ok(),
        ),
    };
    let ready = !draining && drivers_registered && metadata_ok != Some(false);
    let body = Readiness {
        ready,
        version: VERSION.to_string(),
        draining,
        drivers_registered,
        metadata_ok,
        engines,
    };
    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(body)).into_response()
}

async fn list_audit(State(state): State<AppState>) -> Json<Vec<AuditEntry>> {
    Json(state.sessions.list_audit())
}

async fn list_operations(
    State(state): State<AppState>,
) -> Json<Vec<sift_protocol::OperationAuditEntry>> {
    Json(state.sessions.list_operations())
}

async fn list_operation_audit_log(
    State(state): State<AppState>,
    Query(q): Query<HistoryQuery>,
) -> ApiResult<Json<Vec<sift_metadata::OperationAudit>>> {
    let metadata = metadata_store_cloned(&state)?;
    let limit = q.limit.unwrap_or(100).min(500);
    Ok(Json(
        metadata_blocking(move || metadata.list_operation_audit(limit).map_err(Into::into)).await?,
    ))
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
                    "summary": "Liveness and registered engines",
                    "responses": { "200": { "description": "Health", "content": json_content("Health") } }
                }
            },
            "/v1/ready": {
                "get": {
                    "operationId": "ready",
                    "summary": "Readiness: 200 when ready, 503 while draining/unhealthy",
                    "responses": {
                        "200": { "description": "Ready", "content": json_content("Readiness") },
                        "503": { "description": "Not ready", "content": json_content("Readiness") }
                    }
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
            "/v1/operations/audit": {
                "get": {
                    "operationId": "listOperationAudit",
                    "summary": "List durable operation audit rows (actor, target, result, rows)",
                    "responses": { "200": { "description": "Durable audit rows", "content": json_array_content("OperationAudit") } }
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
            "/v1/sessions/{id}/transactions/{tx_id}/savepoints": {
                "post": {
                    "operationId": "createSavepoint",
                    "summary": "Create transaction savepoint",
                    "requestBody": json_body("SavepointRequest"),
                    "responses": { "200": { "description": "Ack", "content": json_object_content() } }
                }
            },
            "/v1/sessions/{id}/transactions/{tx_id}/savepoints/rollback": {
                "post": {
                    "operationId": "rollbackToSavepoint",
                    "summary": "Rollback to transaction savepoint",
                    "requestBody": json_body("SavepointRequest"),
                    "responses": { "200": { "description": "Ack", "content": json_object_content() } }
                }
            },
            "/v1/sessions/{id}/transactions/{tx_id}/savepoints/release": {
                "post": {
                    "operationId": "releaseSavepoint",
                    "summary": "Release transaction savepoint",
                    "requestBody": json_body("SavepointRequest"),
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
            "/v1/sessions/{id}/connections/{conn_id}/ddl": {
                "get": {
                    "operationId": "getObjectDdl",
                    "summary": "Generate DDL (CREATE statement) for a database object. Query params: `name` (required), `schema`, `kind` (defaults to `table`).",
                    "responses": { "200": { "description": "ObjectDdl", "content": json_content("ObjectDdl") } }
                }
            },
            "/v1/sessions/{id}/connections/{conn_id}/complete": {
                "post": {
                    "operationId": "postCompletion",
                    "summary": "Compute ranked autocomplete candidates for a SQL text + cursor position on the connection's engine.",
                    "requestBody": json_body("CompletionRequest"),
                    "responses": { "200": { "description": "CompletionResponse", "content": json_content("CompletionResponse") } }
                }
            },
            "/v1/sessions/{id}/connections/{conn_id}/export": {
                "post": {
                    "operationId": "exportQuery",
                    "summary": "Stream a query result as CSV / TSV / JSON Lines / JSON Array. Response is chunked; Content-Type depends on the requested format.",
                    "requestBody": json_body("ExportRequest"),
                    "responses": { "200": { "description": "Streamed export body" } }
                }
            },
            "/v1/cursors/{cursor_id}/pages": {
                "get": {
                    "operationId": "readSpilledCursorPages",
                    "summary": "Read pages from a spilled (evicted) cursor. Query params: `from_seq` (optional, must equal current pages_read), `limit` (default 32, max 256).",
                    "responses": { "200": { "description": "Batch of pages + done flag" } }
                }
            },
            "/v1/cursors/{cursor_id}": {
                "delete": {
                    "operationId": "deleteSpilledCursor",
                    "summary": "Delete a spilled cursor's file explicitly (idempotent). Reaper handles this on TTL too.",
                    "responses": { "200": { "description": "Ok" } }
                }
            },
            "/v1/metadata/tenants": {
                "get": {
                    "operationId": "listMetadataTenants",
                    "summary": "List current principal tenant memberships",
                    "responses": { "200": { "description": "Tenant memberships", "content": json_array_content("TenantMembership") } }
                }
            },
            "/v1/metadata/rooms": {
                "get": {
                    "operationId": "listMetadataRooms",
                    "summary": "List rooms for current principal in a tenant",
                    "responses": { "200": { "description": "Rooms", "content": json_array_content("Room") } }
                },
                "post": {
                    "operationId": "createMetadataRoom",
                    "summary": "Create room",
                    "requestBody": json_body("CreateRoomRequest"),
                    "responses": { "200": { "description": "Room", "content": json_content("Room") } }
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
                    "responses": { "200": { "description": "Room members", "content": json_array_content("RoomMember") } }
                },
                "post": {
                    "operationId": "addMetadataRoomMember",
                    "summary": "Add or update room member",
                    "requestBody": json_body("AddRoomMemberRequest"),
                    "responses": { "200": { "description": "Room member", "content": json_content("RoomMember") } }
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
            "/v1/metadata/rooms/{id}/ws": {
                "get": {
                    "operationId": "roomWebSocket",
                    "summary": "WebSocket room presence and document operations; protocol uses RoomClientMessage/RoomServerMessage",
                    "responses": { "101": { "description": "WebSocket upgrade" } }
                }
            },
            "/v1/metadata/rooms/{id}/documents": {
                "get": {
                    "operationId": "listMetadataDocuments",
                    "summary": "List room documents",
                    "responses": { "200": { "description": "Documents", "content": json_array_content("Document") } }
                },
                "post": {
                    "operationId": "createMetadataDocument",
                    "summary": "Create room document",
                    "requestBody": json_body("CreateDocumentRequest"),
                    "responses": { "200": { "description": "Document", "content": json_content("Document") } }
                }
            },
            "/v1/metadata/documents/{id}": {
                "put": {
                    "operationId": "updateMetadataDocument",
                    "summary": "Update document CRDT snapshot",
                    "requestBody": json_body("UpdateDocumentSnapshotRequest"),
                    "responses": { "200": { "description": "Document", "content": json_content("Document") } }
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
                    "responses": { "200": { "description": "Connection profiles", "content": json_array_content("ConnectionProfile") } }
                },
                "post": {
                    "operationId": "upsertMetadataConnectionProfile",
                    "summary": "Create or replace connection profile",
                    "requestBody": json_body("UpsertConnectionProfileRequest"),
                    "responses": { "200": { "description": "Connection profile", "content": json_content("ConnectionProfile") } }
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
                    "requestBody": json_body("SetCredentialRequest"),
                    "responses": { "200": { "description": "Ack", "content": json_object_content() } }
                }
            },
            "/v1/metadata/history": {
                "get": {
                    "operationId": "listMetadataHistory",
                    "summary": "List query history by room or current principal",
                    "responses": { "200": { "description": "Query history", "content": json_array_content("QueryHistory") } }
                }
            },
            "/v1/auth/tokens": {
                "get": {
                    "operationId": "listAuthTokens",
                    "summary": "List current principal API tokens",
                    "responses": { "200": { "description": "API tokens", "content": json_array_content("ApiTokenRow") } }
                },
                "post": {
                    "operationId": "issueAuthToken",
                    "summary": "Issue API token; plaintext returned once",
                    "requestBody": json_body("IssueTokenRequest"),
                    "responses": { "200": { "description": "Issued token", "content": json_content("IssueTokenResponse") } }
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
                    "requestBody": json_body("OpenConnectionFromProfileRequest"),
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
    add_schema::<sift_protocol::Readiness>("Readiness", &mut schemas);
    add_schema::<sift_protocol::SavepointRequest>("SavepointRequest", &mut schemas);
    add_schema::<sift_protocol::SchemaSnapshot>("SchemaSnapshot", &mut schemas);
    add_schema::<sift_protocol::ObjectDdl>("ObjectDdl", &mut schemas);
    add_schema::<sift_protocol::completion::CompletionRequest>("CompletionRequest", &mut schemas);
    add_schema::<sift_protocol::completion::CompletionResponse>("CompletionResponse", &mut schemas);
    add_schema::<sift_protocol::completion::CompletionCandidate>(
        "CompletionCandidate",
        &mut schemas,
    );
    add_schema::<sift_protocol::completion::CompletionKind>("CompletionKind", &mut schemas);
    add_schema::<sift_protocol::completion::CompletionContext>("CompletionContext", &mut schemas);
    add_schema::<sift_protocol::ExportRequest>("ExportRequest", &mut schemas);
    add_schema::<sift_protocol::ServerInfo>("ServerInfo", &mut schemas);
    add_schema::<sift_protocol::SessionInfo>("SessionInfo", &mut schemas);
    add_schema::<sift_protocol::TransactionInfo>("TransactionInfo", &mut schemas);
    add_schema::<sift_protocol::WsClientMessage>("WsClientMessage", &mut schemas);
    add_schema::<sift_protocol::WsServerMessage>("WsServerMessage", &mut schemas);
    add_schema::<sift_protocol::RoomClientMessage>("RoomClientMessage", &mut schemas);
    add_schema::<sift_protocol::RoomServerMessage>("RoomServerMessage", &mut schemas);
    add_schema::<sift_metadata::ApiTokenRow>("ApiTokenRow", &mut schemas);
    add_schema::<sift_metadata::ConnectionProfile>("ConnectionProfile", &mut schemas);
    add_schema::<sift_metadata::Document>("Document", &mut schemas);
    add_schema::<sift_metadata::OperationAudit>("OperationAudit", &mut schemas);
    add_schema::<sift_metadata::QueryHistory>("QueryHistory", &mut schemas);
    add_schema::<sift_metadata::Room>("Room", &mut schemas);
    add_schema::<sift_metadata::RoomMember>("RoomMember", &mut schemas);
    add_schema::<sift_metadata::TenantMembership>("TenantMembership", &mut schemas);
    add_schema::<AddRoomMemberRequest>("AddRoomMemberRequest", &mut schemas);
    add_schema::<CreateDocumentRequest>("CreateDocumentRequest", &mut schemas);
    add_schema::<CreateRoomRequest>("CreateRoomRequest", &mut schemas);
    add_schema::<IssueTokenRequest>("IssueTokenRequest", &mut schemas);
    add_schema::<IssueTokenResponse>("IssueTokenResponse", &mut schemas);
    add_schema::<OpenConnectionFromProfileRequest>(
        "OpenConnectionFromProfileRequest",
        &mut schemas,
    );
    add_schema::<SetCredentialRequest>("SetCredentialRequest", &mut schemas);
    add_schema::<UpdateDocumentSnapshotRequest>("UpdateDocumentSnapshotRequest", &mut schemas);
    add_schema::<UpsertConnectionProfileRequest>("UpsertConnectionProfileRequest", &mut schemas);
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
    push_metadata_operation(&state, auth.principal_id, "create", "room", Some(room.id.0));
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
    let actor = auth.principal_id;
    metadata_blocking(move || {
        ensure_room_permission(&metadata, &auth, room, RoomPermission::Admin)?;
        metadata.delete_room(room)?;
        Ok(())
    })
    .await?;
    push_metadata_operation(&state, actor, "delete", "room", Some(room.0));
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
            ensure_room_permission(&metadata, &auth, room, RoomPermission::Read)?;
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
    let actor = auth.principal_id;
    let member = metadata_blocking(move || {
        ensure_room_permission(&metadata, &auth, room, RoomPermission::Admin)?;
        metadata
            .add_room_member(room, principal, req.role)
            .map_err(Into::into)
    })
    .await?;
    push_metadata_operation(&state, actor, "add_member", "room", Some(room.0));
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
    let actor = auth.principal_id;
    metadata_blocking(move || {
        ensure_room_permission(&metadata, &auth, room, RoomPermission::Admin)?;
        metadata.remove_room_member(room, principal)?;
        Ok(())
    })
    .await?;
    push_metadata_operation(&state, actor, "remove_member", "room", Some(room.0));
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
        if room_row.kind == RoomKind::Personal && room_row.created_by != principal {
            return Err(ApiError::Forbidden(
                "personal rooms cannot be joined by other principals".into(),
            ));
        }
        metadata
            .add_room_member(room, principal, RoomRole::Editor)
            .map_err(Into::into)
    })
    .await?;
    push_metadata_operation(&state, principal, "join", "room", Some(room.0));
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
        ensure_room_permission(&metadata, &auth, room, RoomPermission::Read)?;
        metadata.remove_room_member(room, principal)?;
        Ok(())
    })
    .await?;
    push_metadata_operation(&state, principal, "leave", "room", Some(room.0));
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
            ensure_room_permission(&metadata, &auth, room, RoomPermission::Read)?;
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
    let actor = auth.principal_id;
    let document = metadata_blocking(move || {
        let room_row = ensure_room_permission(&metadata, &auth, room, RoomPermission::Write)?;
        let connection_profile_id = req.connection_profile_id.map(ConnectionProfileId);
        if let Some(profile_id) = connection_profile_id {
            let profile = metadata.get_connection_profile_for_any_tenant(profile_id)?;
            if profile.tenant_id != room_row.tenant_id {
                return Err(ApiError::Forbidden(format!(
                    "connection profile {:?} is not in room tenant {:?}",
                    profile_id, room_row.tenant_id
                )));
            }
        }
        metadata
            .create_document(
                room,
                NewDocument {
                    kind: req.kind,
                    title: req.title,
                    crdt_type: req.crdt_type,
                    crdt_state: req.crdt_state,
                    position: req.position,
                    connection_profile_id,
                },
            )
            .map_err(Into::into)
    })
    .await?;
    push_metadata_operation(&state, actor, "create", "document", Some(document.id.0));
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
    let actor = auth.principal_id;
    let updated = metadata_blocking(move || {
        ensure_document_access(&metadata, &auth, document, RoomPermission::Write)?;
        metadata
            .update_document_snapshot(document, req.crdt_state)
            .map_err(Into::into)
    })
    .await?;
    push_metadata_operation(&state, actor, "update", "document", Some(document.0));
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
    let actor = auth.principal_id;
    metadata_blocking(move || {
        ensure_document_access(&metadata, &auth, document, RoomPermission::Write)?;
        metadata.delete_document(document)?;
        Ok(())
    })
    .await?;
    push_metadata_operation(&state, actor, "delete", "document", Some(document.0));
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
    push_metadata_operation(
        &state,
        auth.principal_id,
        "upsert",
        "connection_profile",
        Some(profile.id.0),
    );
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
    push_metadata_operation(
        &state,
        auth.principal_id,
        "delete",
        "connection_profile",
        Some(profile.0),
    );
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
        auth.principal_id,
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
                ensure_room_permission(&metadata, &auth, room, RoomPermission::Read)?;
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

#[derive(Deserialize, JsonSchema)]
struct SavedQueryListQuery {
    tenant: i64,
    #[serde(default)]
    q: Option<String>,
    /// Comma-separated tag list (axum's default query deserializer
    /// doesn't handle repeated keys). Empty entries are ignored.
    #[serde(default)]
    tags: Option<String>,
    #[serde(default)]
    scope: Option<SavedQueryScope>,
}

async fn list_metadata_saved_queries(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<SavedQueryListQuery>,
) -> ApiResult<Json<Vec<SavedQuery>>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state, headers).await?;
    let tenant = tenant_id(query.tenant)?;
    ensure_tenant(&auth, tenant)?;
    let tags: Vec<String> = query
        .tags
        .as_deref()
        .map(|s| {
            s.split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let filter = SavedQueryFilter {
        tenant_id: tenant,
        q: query.q,
        tags,
        scope: query.scope,
    };
    let principal = auth.principal_id;
    Ok(Json(
        metadata_blocking(move || {
            metadata
                .list_saved_queries(principal, filter)
                .map_err(Into::into)
        })
        .await?,
    ))
}

async fn get_metadata_saved_query(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<SavedQuery>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state, headers).await?;
    let sq_id = saved_query_id(id)?;
    let sq = metadata_blocking(move || metadata.get_saved_query(sq_id).map_err(Into::into)).await?;
    ensure_saved_query_visible(&auth, &sq)?;
    Ok(Json(sq))
}

async fn create_metadata_saved_query(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateSavedQueryRequest>,
) -> ApiResult<Json<SavedQuery>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let tenant = tenant_id(req.tenant_id)?;
    ensure_tenant(&auth, tenant)?;
    // Sharing rules on create:
    // - If owner_principal_id is None, the query is tenant-shared —
    //   creator must be a tenant admin (Owner/Admin role).
    // - If owner_principal_id is Some, it must equal the caller's
    //   principal_id. A caller cannot mint a personal query owned by
    //   someone else.
    let owner = match req.owner_principal_id {
        Some(p) => {
            let p = principal_id(p)?;
            if p != auth.principal_id {
                return Err(ApiError::Forbidden(
                    "cannot create a personal saved query owned by another principal".into(),
                ));
            }
            Some(p)
        }
        None => {
            if !is_tenant_admin(&auth, tenant) {
                return Err(ApiError::Forbidden(
                    "creating a tenant-shared saved query requires Owner or Admin role".into(),
                ));
            }
            None
        }
    };
    let new = NewSavedQuery {
        tenant_id: tenant,
        owner_principal_id: owner,
        name: req.name.clone(),
        sql_text: req.sql_text,
        connection_profile_id: req.connection_profile_id.map(ConnectionProfileId),
        tags: req.tags,
    };
    let saved =
        metadata_blocking(move || metadata.insert_saved_query(new).map_err(Into::into)).await?;
    state.sessions.push_operation(
        Operation::Metadata {
            action: "saved_query.create".into(),
            target: "saved_query".into(),
            id: Some(saved.id.0),
        },
        OperationStatus::Succeeded,
    );
    Ok(Json(saved))
}

async fn update_metadata_saved_query(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<UpdateSavedQueryRequest>,
) -> ApiResult<Json<SavedQuery>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let sq_id = saved_query_id(id)?;
    let existing = {
        let metadata = metadata.clone();
        metadata_blocking(move || metadata.get_saved_query(sq_id).map_err(Into::into)).await?
    };
    ensure_saved_query_editable(&auth, &existing)?;
    let update = UpdateSavedQuery {
        name: req.name,
        sql_text: req.sql_text,
        connection_profile_id: req
            .connection_profile_id
            .map(|opt| opt.map(ConnectionProfileId)),
        tags: req.tags,
    };
    let updated = metadata_blocking(move || {
        metadata
            .update_saved_query(sq_id, update)
            .map_err(Into::into)
    })
    .await?;
    state.sessions.push_operation(
        Operation::Metadata {
            action: "saved_query.update".into(),
            target: "saved_query".into(),
            id: Some(updated.id.0),
        },
        OperationStatus::Succeeded,
    );
    Ok(Json(updated))
}

async fn delete_metadata_saved_query(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let sq_id = saved_query_id(id)?;
    let existing = {
        let metadata = metadata.clone();
        metadata_blocking(move || metadata.get_saved_query(sq_id).map_err(Into::into)).await?
    };
    ensure_saved_query_editable(&auth, &existing)?;
    let deleted =
        metadata_blocking(move || metadata.delete_saved_query(sq_id).map_err(Into::into)).await?;
    state.sessions.push_operation(
        Operation::Metadata {
            action: "saved_query.delete".into(),
            target: "saved_query".into(),
            id: Some(sq_id.0),
        },
        OperationStatus::Succeeded,
    );
    Ok(Json(json!({ "ok": true, "deleted": deleted })))
}

fn ensure_saved_query_visible(auth: &AuthContext, sq: &SavedQuery) -> ApiResult<()> {
    ensure_tenant(auth, sq.tenant_id)?;
    match sq.owner_principal_id {
        Some(owner) if owner != auth.principal_id => Err(ApiError::Forbidden(
            "saved query is personal to another principal".into(),
        )),
        _ => Ok(()),
    }
}

fn ensure_saved_query_editable(auth: &AuthContext, sq: &SavedQuery) -> ApiResult<()> {
    ensure_tenant(auth, sq.tenant_id)?;
    match sq.owner_principal_id {
        Some(owner) => {
            if owner == auth.principal_id {
                Ok(())
            } else {
                Err(ApiError::Forbidden(
                    "personal saved queries can only be edited by their owner".into(),
                ))
            }
        }
        None => {
            if is_tenant_admin(auth, sq.tenant_id) {
                Ok(())
            } else {
                Err(ApiError::Forbidden(
                    "tenant-shared saved queries can only be edited by tenant Owner or Admin"
                        .into(),
                ))
            }
        }
    }
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
    push_metadata_operation(
        &state,
        auth.principal_id,
        "issue",
        "api_token",
        Some(token.id.0),
    );
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
    push_metadata_operation(
        &state,
        auth.principal_id,
        "revoke",
        "api_token",
        Some(token_id.0),
    );
    Ok(Json(json!({"ok": true})))
}

async fn open_connection_from_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<sift_protocol::SessionId>,
    Json(req): Json<OpenConnectionFromProfileRequest>,
) -> ApiResult<Json<sift_protocol::ConnectionInfo>> {
    if state.shutdown.is_draining() {
        return Err(ApiError::ServiceDraining);
    }
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
    push_metadata_operation(
        &state,
        auth.principal_id,
        "open",
        "connection_profile",
        Some(profile_id.0),
    );
    Ok(Json(info))
}

async fn create_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Option<Json<OpenSessionRequest>>,
) -> ApiResult<Json<sift_protocol::SessionInfo>> {
    if state.shutdown.is_draining() {
        return Err(ApiError::ServiceDraining);
    }
    let req = match body {
        Some(Json(b)) => b,
        None => OpenSessionRequest { tag: None },
    };
    let auth = session_auth_context_blocking(state.clone(), headers).await?;
    let actor = auth.as_ref().map(|auth| auth.principal_id.0);
    let info = state
        .sessions
        .open_session_with_owner(req.clone(), auth.map(|auth| auth.principal_id));
    state.sessions.push_operation_full(
        Operation::OpenSession { request: req },
        OperationStatus::Succeeded,
        actor,
        None,
        None,
        None,
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
    if state.shutdown.is_draining() {
        return Err(ApiError::ServiceDraining);
    }
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
    state.sessions.push_operation_full(
        operation,
        OperationStatus::Succeeded,
        None,
        None,
        Some(response.rows_inserted as i64),
        None,
    );
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

#[derive(Deserialize, JsonSchema)]
struct DdlQuery {
    /// Object schema. Optional — engines with a single schema per DB
    /// (rare) skip it.
    #[serde(default)]
    schema: Option<String>,
    /// Object name. Required.
    name: String,
    /// Object kind: `table`, `view`, `procedure`, `scalar_function`,
    /// etc. Defaults to `table` if omitted.
    #[serde(default)]
    kind: Option<sift_protocol::ObjectKind>,
    /// Routine input argument types. Repeat `routine_args=...` for each
    /// argument. Empty/omitted means not supplied; use no values for a nullary
    /// routine from typed clients via `ObjectPath.routine_args = Some(vec![])`.
    #[serde(default)]
    routine_args: Option<Vec<String>>,
}

async fn export_query(
    State(state): State<AppState>,
    Path((id, conn_id)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Json(req): Json<sift_protocol::ExportRequest>,
) -> ApiResult<Response> {
    use axum::body::Body;
    use axum::response::IntoResponse;
    let entry = state.sessions.conn_entry(id, conn_id)?;
    let driver = entry.driver.clone();
    let handle = entry.handle.clone();
    let format = req.format;
    let stream = crate::export::run_export(
        driver,
        handle,
        req.sql.clone(),
        req.params,
        req.format,
        req.header,
        req.null_display,
    )
    .await?;
    let content_type = crate::export::content_type(format);
    state.sessions.push_operation(
        Operation::Metadata {
            action: "export.stream".into(),
            target: format!("{format:?}"),
            id: None,
        },
        OperationStatus::Succeeded,
    );
    let body = Body::from_stream(stream);
    let mut resp = body.into_response();
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        content_type.parse().unwrap(),
    );
    Ok(resp)
}

async fn get_object_ddl(
    State(state): State<AppState>,
    Path((id, conn_id)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Query(q): Query<DdlQuery>,
) -> ApiResult<Json<sift_protocol::ObjectDdl>> {
    let path = ObjectPath {
        catalog: None,
        schema: q.schema,
        name: q.name,
        kind: q.kind,
        routine_args: q.routine_args,
    };
    let ddl = state.sessions.ddl_for(id, conn_id, path.clone()).await?;
    state.sessions.push_operation(
        Operation::Metadata {
            action: "ddl.generate".into(),
            target: format!(
                "{:?}",
                path.kind.unwrap_or(sift_protocol::ObjectKind::Table)
            ),
            id: None,
        },
        OperationStatus::Succeeded,
    );
    Ok(Json(ddl))
}

async fn post_completion(
    State(state): State<AppState>,
    Path((id, conn_id)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Json(req): Json<sift_protocol::completion::CompletionRequest>,
) -> ApiResult<Json<sift_protocol::completion::CompletionResponse>> {
    let resp = state.sessions.complete(id, conn_id, req.clone()).await?;
    state.sessions.push_operation(
        Operation::Complete {
            session: id,
            connection: conn_id,
            request: req,
        },
        OperationStatus::Succeeded,
    );
    Ok(Json(resp))
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
                routine_args: None,
            }))
        }
        other => Err(ApiError::BadRequest(format!(
            "unknown depth `{other}` (want `shallow` or `deep`)"
        ))),
    }
}

async fn execute_query(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<sift_protocol::SessionId>,
    Json(req): Json<ExecuteRequestHttp>,
) -> ApiResult<Json<sift_protocol::ExecuteResponse>> {
    let metadata_context = execute_metadata_context(&state, headers, &req).await?;
    let sql_text = req.sql.clone();
    let operation = Operation::ExecuteQuery {
        session: id,
        request: req.clone(),
    };
    // Count this query against the shutdown drain gate for its whole lifetime;
    // in-flight queries continue draining even after `begin_drain`.
    let _query_guard = state.shutdown.track_query();
    let actor = metadata_context.as_ref().map(|c| c.principal_id.0);
    let started = Instant::now();
    let result = state.sessions.execute_http(id, req).await;
    let duration_ms = started.elapsed().as_millis().min(i64::MAX as u128) as i64;
    if let Some(context) = metadata_context {
        if let Some(summary) = room_query_result(&context, sql_text.clone(), &result) {
            state.rooms.publish(
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
            Ok(Json(resp))
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

async fn create_savepoint(
    State(state): State<AppState>,
    Path((id, tx_id)): Path<(sift_protocol::SessionId, sift_protocol::TxId)>,
    Json(req): Json<SavepointRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    if req.tx_id != tx_id {
        return Err(ApiError::BadRequest(
            "`tx_id` body value must match tx id in path".into(),
        ));
    }
    state.sessions.create_savepoint(id, req.clone()).await?;
    state.sessions.push_operation(
        Operation::Savepoint {
            session: id,
            request: req,
        },
        OperationStatus::Succeeded,
    );
    Ok(Json(json!({"ok": true})))
}

async fn rollback_to_savepoint(
    State(state): State<AppState>,
    Path((id, tx_id)): Path<(sift_protocol::SessionId, sift_protocol::TxId)>,
    Json(req): Json<SavepointRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    if req.tx_id != tx_id {
        return Err(ApiError::BadRequest(
            "`tx_id` body value must match tx id in path".into(),
        ));
    }
    state
        .sessions
        .rollback_to_savepoint(id, req.clone())
        .await?;
    state.sessions.push_operation(
        Operation::RollbackToSavepoint {
            session: id,
            request: req,
        },
        OperationStatus::Succeeded,
    );
    Ok(Json(json!({"ok": true})))
}

async fn release_savepoint(
    State(state): State<AppState>,
    Path((id, tx_id)): Path<(sift_protocol::SessionId, sift_protocol::TxId)>,
    Json(req): Json<SavepointRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    if req.tx_id != tx_id {
        return Err(ApiError::BadRequest(
            "`tx_id` body value must match tx id in path".into(),
        ));
    }
    state.sessions.release_savepoint(id, req.clone()).await?;
    state.sessions.push_operation(
        Operation::ReleaseSavepoint {
            session: id,
            request: req,
        },
        OperationStatus::Succeeded,
    );
    Ok(Json(json!({"ok": true})))
}

async fn cancel_query(
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

#[derive(Deserialize, JsonSchema)]
struct ReadSpillPagesQuery {
    /// Optional starting page (0-indexed). If omitted, resumes from
    /// wherever the last call left off.
    from_seq: Option<usize>,
    /// Max pages to return in this response. Default 32; capped at 256
    /// to bound memory per request.
    #[serde(default)]
    limit: Option<usize>,
}

/// Resume from a spilled cursor. The client learns the URL from the
/// `resume_url` field on the `CursorEvicted` terminal.
async fn read_spill_pages(
    State(state): State<AppState>,
    Path(cursor_id): Path<sift_protocol::CursorId>,
    axum::extract::Query(q): axum::extract::Query<ReadSpillPagesQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let registry = state.sessions.cursor_registry();
    // If from_seq is set and it doesn't match the entry's current
    // read cursor, reject — we don't allow re-reading already-read
    // pages (spill files are append-only + read-forward).
    if let Some(seq) = q.from_seq {
        let info = registry.spill_info(cursor_id).ok_or_else(|| {
            ApiError::Driver(sift_protocol::DriverError::new(
                sift_protocol::Code::CursorNotFound,
                "no spill for cursor",
            ))
        })?;
        if seq != info.pages_read {
            return Err(ApiError::BadRequest(format!(
                "from_seq={seq} does not match pages_read={} for cursor",
                info.pages_read
            )));
        }
    }
    let limit = q.limit.unwrap_or(32).clamp(1, 256);
    let (pages, done) = registry
        .read_spill_pages(cursor_id, limit)
        .map_err(ApiError::Driver)?;
    Ok(Json(json!({
        "cursor_id": cursor_id.0,
        "pages": pages,
        "done": done,
    })))
}

/// Explicit cleanup of a spill file. Idempotent; returns ok whether or
/// not the entry existed.
async fn delete_spilled_cursor(
    State(state): State<AppState>,
    Path(cursor_id): Path<sift_protocol::CursorId>,
) -> ApiResult<Json<serde_json::Value>> {
    state.sessions.cursor_registry().drop_spill(cursor_id);
    Ok(Json(json!({"ok": true})))
}

async fn ws_session(
    State(state): State<AppState>,
    Path(session_id): Path<sift_protocol::SessionId>,
    ws: WebSocketUpgrade,
) -> Response {
    // Capture the correlation ID from the upgrade request so the (detached)
    // socket task's per-message operations are audited under the same ID.
    let correlation_id = crate::correlation::current().unwrap_or_else(crate::correlation::generate);
    ws.on_upgrade(move |socket| {
        crate::correlation::scope(correlation_id, async move {
            if let Err(error) = handle_ws(state.sessions, state.shutdown, session_id, socket).await
            {
                tracing::warn!(%session_id, error = %error, "websocket session ended with error");
            }
        })
    })
}

async fn ws_room(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let room = room_id(id)?;
    let room_row = metadata_blocking({
        let metadata = metadata.clone();
        let auth = auth.clone();
        move || ensure_room_permission(&metadata, &auth, room, RoomPermission::Read)
    })
    .await?;
    let correlation_id = crate::correlation::current().unwrap_or_else(crate::correlation::generate);
    Ok(ws.on_upgrade(move |socket| {
        crate::correlation::scope(correlation_id, async move {
            if let Err(error) = handle_room_ws(state, metadata, auth, room_row.id, socket).await {
                tracing::warn!(room_id = %room_row.id.0, error = %error, "room websocket ended with error");
            }
        })
    }))
}

async fn handle_room_ws(
    state: AppState,
    metadata: MetadataStore,
    auth: AuthContext,
    room: RoomId,
    socket: WebSocket,
) -> ApiResult<()> {
    let (mut sender, mut receiver) = socket.split();
    let mut events = state.rooms.subscribe(room.0);
    let mut attachment_id = None;

    loop {
        tokio::select! {
            Some(message) = receiver.next() => {
                let message = message.map_err(|error| ApiError::BadRequest(error.to_string()))?;
                let Message::Text(text) = message else {
                    if matches!(message, Message::Close(_)) {
                        break;
                    }
                    continue;
                };
                let message: RoomClientMessage =
                    serde_json::from_str(&text).map_err(|error| ApiError::BadRequest(error.to_string()))?;
                match message {
                    RoomClientMessage::Attach { client_id } => {
                        if attachment_id.is_some() {
                            send_json(&mut sender, &RoomServerMessage::Error {
                                message: "room websocket is already attached".into(),
                            }).await?;
                            continue;
                        }
                        let (id, presence, next_events) =
                            state.rooms.attach(room.0, auth.principal_id.0, client_id.clone());
                        events = next_events;
                        attachment_id = Some(id);
                        state.sessions.push_operation(
                            Operation::AttachRoom {
                                room_id: room.0,
                                attachment_id: id,
                                client_id,
                            },
                            OperationStatus::Succeeded,
                        );
                        send_json(&mut sender, &RoomServerMessage::Attached {
                            attachment_id: id,
                            presence,
                        }).await?;
                    }
                    RoomClientMessage::Detach => break,
                    RoomClientMessage::PresencePing => {
                        send_json(&mut sender, &RoomServerMessage::Presence {
                            presence: state.rooms.presence(room.0),
                        }).await?;
                    }
                    RoomClientMessage::DocumentOperation {
                        operation_id,
                        document_id: raw_document_id,
                        operation,
                    } => {
                        if attachment_id.is_none() {
                            send_json(&mut sender, &RoomServerMessage::Error {
                                message: "attach before sending document operations".into(),
                            }).await?;
                            continue;
                        }
                        let document = document_id(raw_document_id)?;
                        let applied = apply_room_document_operation(
                            metadata.clone(),
                            auth.clone(),
                            room,
                            document,
                            operation.clone(),
                        )
                        .await;
                        if let Err(error) = applied {
                            send_json(&mut sender, &RoomServerMessage::Error {
                                message: error.to_string(),
                            }).await?;
                            continue;
                        }
                        let envelope = DocumentOperationEnvelope {
                            operation_id: operation_id.clone(),
                            room_id: room.0,
                            document_id: document.0,
                            actor_principal_id: auth.principal_id.0,
                            operation: operation.clone(),
                        };
                        state.rooms.publish(
                            room.0,
                            RoomServerMessage::DocumentOperation {
                                operation: envelope,
                            },
                        );
                        state.sessions.push_operation(
                            Operation::ApplyDocumentOperation {
                                room_id: room.0,
                                document_id: document.0,
                                operation_id,
                                operation,
                            },
                            OperationStatus::Succeeded,
                        );
                    }
                }
            }
            event = events.recv() => {
                match event {
                    Ok(message) => send_json(&mut sender, &message).await?,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        send_json(&mut sender, &RoomServerMessage::Presence {
                            presence: state.rooms.presence(room.0),
                        }).await?;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }

    if let Some(id) = attachment_id {
        state.rooms.detach(room.0, id);
        state.sessions.push_operation(
            Operation::DetachRoom {
                room_id: room.0,
                attachment_id: id,
            },
            OperationStatus::Succeeded,
        );
    }
    Ok(())
}

async fn apply_room_document_operation(
    metadata: MetadataStore,
    auth: AuthContext,
    room: RoomId,
    document: DocumentId,
    operation: sift_protocol::TextDocumentOperation,
) -> ApiResult<()> {
    metadata_blocking(move || {
        let row = ensure_document_access(&metadata, &auth, document, RoomPermission::Write)?;
        if row.room_id != room {
            return Err(ApiError::Forbidden(format!(
                "document {:?} is not in room {:?}",
                document, room
            )));
        }
        let crdt = match row.crdt_type {
            CrdtType::Loro => CrdtKind::Loro,
            CrdtType::Automerge => CrdtKind::Automerge,
        };
        let mut doc = TextDocument::from_snapshot(DocumentSnapshot::new(crdt, row.crdt_state));
        let operation = match operation {
            sift_protocol::TextDocumentOperation::Replace { text } => {
                TextOperation::Replace { text }
            }
            sift_protocol::TextDocumentOperation::Insert { offset, text } => {
                TextOperation::Insert { offset, text }
            }
            sift_protocol::TextDocumentOperation::Delete { start, end } => {
                TextOperation::Delete { start, end }
            }
        };
        let snapshot = doc
            .apply(operation)
            .map_err(|error| ApiError::BadRequest(error.to_string()))?;
        metadata.update_document_snapshot(document, snapshot.bytes)?;
        Ok(())
    })
    .await
}

async fn handle_ws(
    sessions: SessionStore,
    shutdown: crate::shutdown::Shutdown,
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
                        tx,
                    } => {
                        // Track the streaming query against the drain gate for
                        // its whole lifetime (execute + paging).
                        let _query_guard = shutdown.track_query();
                        let stream = match sessions
                            .execute_stream(
                                session_id,
                                connection,
                                ExecuteRequest { sql, params },
                                tx.as_ref(),
                            )
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
                            &sessions,
                            session_id,
                            connection,
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

enum AckOutcome {
    Acked,
    Cancelled,
}

async fn stream_pages_with_ack(
    sender: &mut futures::stream::SplitSink<WebSocket, Message>,
    receiver: &mut futures::stream::SplitStream<WebSocket>,
    sessions: &SessionStore,
    session_id: sift_protocol::SessionId,
    connection: sift_protocol::ConnectionId,
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
            // Terminal page delivered: cursor is done. Drop the
            // registry entry so the per-session slot frees up.
            sessions.cursor_remove(cursor_id);
            break;
        }
        match wait_for_ack(receiver, sessions, session_id, connection, cursor_id, seq).await? {
            AckOutcome::Acked => {
                // Fresh ack — bump the cursor's last-ack so it is not
                // ranked as idle by the eviction policy.
                sessions.cursor_touch(cursor_id);
            }
            AckOutcome::Cancelled => {
                sessions.cursor_remove(cursor_id);
                break;
            }
        }
        seq += 1;
    }
    Ok(())
}

async fn wait_for_ack(
    receiver: &mut futures::stream::SplitStream<WebSocket>,
    sessions: &SessionStore,
    session_id: sift_protocol::SessionId,
    connection: sift_protocol::ConnectionId,
    cursor_id: sift_protocol::CursorId,
    seq: u64,
) -> ApiResult<AckOutcome> {
    loop {
        let Some(message) = receiver.next().await else {
            // Client dropped the socket mid-stream: cancel the driver-side
            // work so we honor the abort+discard invariant instead of
            // waiting for the mpsc drop to eventually reach the driver.
            let _ = sessions.cancel(session_id, connection, cursor_id).await;
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
                } if ack_cursor == cursor_id && ack_seq == seq => return Ok(AckOutcome::Acked),
                WsClientMessage::Ack { .. } => {
                    return Err(ApiError::BadRequest("ack cursor or seq mismatch".into()));
                }
                WsClientMessage::Cancel {
                    connection: cancel_conn,
                    cursor_id: cancel_cursor,
                } => {
                    if cancel_cursor != cursor_id || cancel_conn != connection {
                        return Err(ApiError::BadRequest(
                            "cancel cursor or connection mismatch".into(),
                        ));
                    }
                    sessions.cancel(session_id, connection, cursor_id).await?;
                    return Ok(AckOutcome::Cancelled);
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
            Message::Close(_) => {
                let _ = sessions.cancel(session_id, connection, cursor_id).await;
                return Err(ApiError::BadRequest("websocket closed".into()));
            }
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
