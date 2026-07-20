//! axum router + handlers. Routes versioned under `/v1`. The `AppState`
//! carries the `SessionStore` (which in turn carries the `DriverRegistry`).

use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Extension, Path, Query, State};
use axum::http::{header, header::HeaderName, HeaderMap, HeaderValue, Request, StatusCode};
use axum::middleware::{from_fn, from_fn_with_state, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use futures::{SinkExt, StreamExt};
use schemars::{schema_for, JsonSchema};
use serde::Deserialize;
use serde_json::json;
use std::time::Instant;

use sift_doc::{CrdtKind, DocumentSnapshot, TextDocument, TextOperation};
use sift_metadata::{
    ApiTokenId, AuthClientKind as MetadataAuthClientKind, AuthIdentityId, ConnectionProfileId,
    CrdtType, Document, DocumentId, GithubAllowlistId, GithubProfile, MetadataStore,
    NewConnectionProfile, NewDocument, NewOperationAudit, NewQueryHistory, NewRoom, NewSavedQuery,
    PrincipalId, PrincipalKeyId, QueryHistory, QueryStatus, RefreshAuthResult, Room, RoomId,
    RoomKind, RoomMember, RoomRole, SavedQuery, SavedQueryFilter, SavedQueryId, SavedQueryScope,
    TenantId, TenantInvitationId, TenantMembership, UpdateSavedQuery,
};
use sift_protocol::{
    AcceptTenantInvitationRequest, AdminCreatePasswordPrincipalRequest,
    AdminLinkPasswordIdentityRequest, AdminSetPrincipalDisabledRequest, AuditEntry, AuthClientKind,
    AuthIdentitySummary, AuthPrincipal, AuthSessionSummary, AuthTenantMembership,
    AuthTokensResponse, BeginTransactionRequest, BulkInsertRequest, CancelRequest,
    ChangePasswordRequest, CreateGithubAllowlistRequest, CreateTenantInvitationRequest,
    CsvImportRequest, DocumentOperationEnvelope, EndTransactionRequest, ExecuteRequest,
    ExecuteRequestHttp, GithubNativeAuthExchangeRequest, GithubNativeAuthStartResponse, Health,
    InvitationRole, IssuedPasswordResetResponse, IssuedTenantInvitationResponse,
    KeyAuthenticateRequest, KeyChallengeRequest, KeyChallengeResponse, KillProcessRequest,
    ObjectPath, OpenConnectionRequest, OpenSessionRequest, Operation, OperationStatus,
    PasswordLoginRequest, PasswordResetRequest, Readiness, RefreshAuthRequest,
    RegisterPrincipalKeyRequest, RoomClientMessage, RoomQueryResult, RoomQueryStatus,
    RoomServerMessage, SavepointRequest, SchemaFilter, SchemaScope, TransactionPreviewRequest,
    WebAuthResponse, WhoAmIResponse, WsClientMessage, WsServerMessage, PROTOCOL_VERSION,
};

use crate::config::{DeploymentPolicy, Transport};
use crate::error::{ApiError, ApiResult};
use crate::room_runtime::RoomRuntime;
use crate::session::SessionStore;
use crate::VERSION;

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

#[derive(Clone)]
pub struct AuthState {
    pub bearer_token: Option<String>,
    pub loopback_bypass: bool,
    pub deployment: DeploymentPolicy,
    pub transport: Transport,
    pub runtime: crate::identity::AuthRuntime,
    pub github: Option<crate::identity::GithubOAuthConfig>,
    pub instance_audience: String,
}

impl Default for AuthState {
    fn default() -> Self {
        Self {
            bearer_token: None,
            loopback_bypass: true,
            deployment: DeploymentPolicy::Personal,
            transport: Transport::Loopback,
            runtime: crate::identity::AuthRuntime::default(),
            github: None,
            instance_audience: "sift:local".into(),
        }
    }
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/ready", get(ready))
        .route("/v1/audit", get(list_audit))
        .route("/v1/operations", get(list_operations))
        .route("/v1/operations/available", get(list_available_operations))
        .route("/v1/operations/audit", get(list_operation_audit_log))
        .route("/v1/openapi.json", get(openapi))
        .route("/v1/auth/login", post(password_login))
        .route("/v1/auth/refresh", post(refresh_auth))
        .route("/v1/auth/logout", post(logout_auth))
        .route("/v1/auth/logout-all", post(logout_all_auth))
        .route("/v1/auth/whoami", get(whoami))
        .route("/v1/auth/password", put(change_password))
        .route("/v1/auth/password/reset", post(reset_password))
        .route("/v1/auth/github/start", get(github_start))
        .route("/v1/auth/github/callback", get(github_callback))
        .route("/v1/auth/github/exchange", post(github_native_exchange))
        .route(
            "/v1/admin/auth/github-allowlist",
            get(list_github_allowlist).post(create_github_allowlist),
        )
        .route(
            "/v1/admin/auth/github-allowlist/:id",
            delete(revoke_github_allowlist),
        )
        .route("/v1/admin/principals", post(admin_create_principal))
        .route(
            "/v1/admin/principals/:id/disabled",
            put(admin_set_principal_disabled),
        )
        .route(
            "/v1/admin/principals/:id/identities",
            get(admin_list_principal_identities),
        )
        .route(
            "/v1/admin/principals/:id/identities/password",
            post(admin_link_password_identity),
        )
        .route(
            "/v1/admin/principals/:principal_id/identities/:identity_id",
            delete(admin_unlink_identity),
        )
        .route(
            "/v1/admin/principals/:id/auth-sessions",
            get(admin_list_auth_sessions),
        )
        .route(
            "/v1/admin/principals/:principal_id/auth-sessions/:session_id",
            delete(admin_revoke_auth_session),
        )
        .route(
            "/v1/admin/principals/:principal_id/identities/:identity_id/password-reset",
            post(admin_issue_password_reset),
        )
        .route(
            "/v1/metadata/tenants/:id/invitations",
            get(list_tenant_invitations).post(create_tenant_invitation),
        )
        .route(
            "/v1/metadata/tenants/:tenant_id/invitations/:id",
            delete(revoke_tenant_invitation),
        )
        .route(
            "/v1/auth/invitations/accept",
            post(accept_tenant_invitation),
        )
        .route(
            "/v1/auth/keys",
            get(list_principal_keys).post(register_principal_key),
        )
        .route("/v1/auth/keys/:id", delete(revoke_principal_key))
        .route("/v1/auth/keys/challenge", post(issue_key_challenge))
        .route("/v1/auth/keys/authenticate", post(authenticate_key))
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
            "/v1/sessions/:id/connections/:conn_id/import/csv",
            post(import_csv),
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
        .route(
            "/v1/sessions/:id/connections/:conn_id/edits/preview",
            post(post_edits_preview),
        )
        .route(
            "/v1/sessions/:id/connections/:conn_id/edits/apply",
            post(post_edits_apply),
        )
        .route(
            "/v1/sessions/:id/connections/:conn_id/search/schema",
            post(post_search_schema),
        )
        .route(
            "/v1/sessions/:id/connections/:conn_id/search/data",
            post(post_search_data),
        )
        .route(
            "/v1/sessions/:id/connections/:conn_id/explain",
            post(post_explain),
        )
        .route(
            "/v1/sessions/:id/connections/:conn_id/processes",
            get(list_processes),
        )
        .route(
            "/v1/sessions/:id/connections/:conn_id/processes/kill",
            post(kill_process),
        )
        .route("/v1/sessions/:id/queries", post(execute_query))
        .route(
            "/v1/sessions/:id/transactions",
            get(list_transactions).post(begin_transaction),
        )
        .route(
            "/v1/sessions/:id/transactions/:tx_id/commit",
            post(commit_transaction),
        )
        .route(
            "/v1/sessions/:id/transactions/:tx_id/rollback",
            post(rollback_transaction),
        )
        .route(
            "/v1/sessions/:id/transactions/:tx_id/preview",
            post(preview_transaction),
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
        .layer(from_fn_with_state(state.clone(), auth_middleware))
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

fn finish_operation<T>(
    sessions: &SessionStore,
    operation: Operation,
    result: ApiResult<T>,
    row_count: impl FnOnce(&T) -> Option<i64>,
) -> ApiResult<T> {
    match result {
        Ok(value) => {
            sessions.push_operation_full(
                operation,
                OperationStatus::Succeeded,
                None,
                None,
                row_count(&value),
                None,
            );
            Ok(value)
        }
        Err(error) => {
            let (result_code, message) = match &error {
                ApiError::Driver(driver) => {
                    (Some(driver.code.to_string()), Some(driver.message.clone()))
                }
                other => (None, Some(other.to_string())),
            };
            sessions.push_operation_full(
                operation,
                OperationStatus::Failed,
                None,
                result_code,
                None,
                message,
            );
            Err(error)
        }
    }
}

async fn auth_middleware(
    State(state): State<AppState>,
    mut req: Request<Body>,
    next: Next,
) -> Response {
    let path = req.uri().path();
    if is_public_path(path) {
        return next.run(req).await;
    }

    if state.metadata.is_some() {
        return match resolve_auth_context_blocking(state.clone(), req.headers().clone()).await {
            Ok(context) => {
                if let Err(error) = authorize_route(&state, &context, path) {
                    return error.into_response();
                }
                if context.cookie_authenticated
                    && is_state_changing(req.method())
                    && !valid_csrf(req.headers())
                {
                    return ApiError::Forbidden("invalid CSRF token".into()).into_response();
                }
                req.extensions_mut().insert(context);
                next.run(req).await
            }
            Err(error) => error.into_response(),
        };
    }

    // Metadata-free personal mode is retained for the headless development
    // harness. It never applies to team deployments and still requires either
    // an explicit static bearer or a verified loopback peer.
    if state.auth.deployment == DeploymentPolicy::Team {
        return ApiError::MetadataUnavailable.into_response();
    }
    let presented = bearer_from_headers(req.headers());
    let bearer_valid = match (presented, state.auth.bearer_token.as_deref()) {
        (Some(actual), Some(expected)) => constant_time_eq(actual.as_bytes(), expected.as_bytes()),
        (Some(_), None) | (None, _) => false,
    };
    let bypass_allowed =
        presented.is_none() && state.auth.loopback_bypass && peer_is_loopback(req.headers());
    if !bearer_valid && !bypass_allowed {
        return ApiError::Unauthorized.into_response();
    }
    next.run(req).await
}

fn is_public_path(path: &str) -> bool {
    matches!(
        path,
        "/v1/health"
            | "/v1/ready"
            | "/v1/openapi.json"
            | "/v1/auth/login"
            | "/v1/auth/password/reset"
            | "/v1/auth/refresh"
            | "/v1/auth/github/start"
            | "/v1/auth/github/callback"
            | "/v1/auth/github/exchange"
            | "/v1/auth/keys/challenge"
            | "/v1/auth/keys/authenticate"
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RouteAccess {
    Public,
    Authenticated,
    Session(sift_protocol::SessionId),
    Cursor(sift_protocol::CursorId),
}

/// Classify every current route family at the authentication boundary.
/// Tenant/room/admin detail is evaluated by the typed handler after this
/// authenticated floor; session-derived resources are enforced here because
/// every operation below them inherits the session owner.
fn route_access(path: &str) -> RouteAccess {
    if is_public_path(path) {
        return RouteAccess::Public;
    }
    if let Some(rest) = path.strip_prefix("/v1/sessions/") {
        if let Some(id) = rest.split('/').next().and_then(|part| part.parse().ok()) {
            return RouteAccess::Session(sift_protocol::SessionId(id));
        }
    }
    if let Some(rest) = path.strip_prefix("/v1/cursors/") {
        if let Some(id) = rest.split('/').next().and_then(|part| part.parse().ok()) {
            return RouteAccess::Cursor(sift_protocol::CursorId(id));
        }
    }
    RouteAccess::Authenticated
}

fn authorize_route(state: &AppState, auth: &AuthContext, path: &str) -> ApiResult<()> {
    let owner = match route_access(path) {
        RouteAccess::Public | RouteAccess::Authenticated => return Ok(()),
        RouteAccess::Session(session) => state.sessions.session_owner(session)?,
        RouteAccess::Cursor(cursor) => {
            let spill = state
                .sessions
                .cursor_registry()
                .spill_info(cursor)
                .ok_or_else(|| {
                    ApiError::Driver(sift_protocol::DriverError::new(
                        sift_protocol::Code::CursorNotFound,
                        "cursor not found",
                    ))
                })?;
            state.sessions.session_owner(spill.session_id)?
        }
    };
    if owner.is_some_and(|owner| owner != auth.principal_id) {
        return Err(ApiError::Forbidden(
            "resource belongs to another principal".into(),
        ));
    }
    if owner.is_none() && state.auth.deployment == DeploymentPolicy::Team {
        return Err(ApiError::Forbidden(
            "team deployments reject unowned runtime resources".into(),
        ));
    }
    Ok(())
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
    auth_session_id: Option<String>,
    cookie_authenticated: bool,
    access_expires_at: Option<chrono::DateTime<chrono::Utc>>,
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

async fn password_login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<PasswordLoginRequest>,
) -> ApiResult<Response> {
    let client_kind = request.client_kind;
    let source = headers
        .get(&PEER_ADDR_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown");
    let metadata = metadata_store(&state)?;
    let outcome = state
        .auth
        .runtime
        .authenticate_password(
            metadata,
            source,
            &request.username,
            request.password.into_bytes(),
        )
        .await
        .map_err(|error| ApiError::Internal(error.to_string()))?;
    let identity = match outcome {
        crate::identity::PasswordAuthOutcome::Authenticated(identity) => identity,
        crate::identity::PasswordAuthOutcome::Denied => {
            record_auth_failure(metadata, "authenticate.password", "denied")?;
            state.sessions.push_operation_full(
                Operation::Authenticate {
                    method: sift_protocol::AuthenticationMethod::Password,
                },
                OperationStatus::Failed,
                None,
                Some("authentication_denied".into()),
                None,
                Some("authentication denied".into()),
            );
            return Err(ApiError::Unauthorized);
        }
        crate::identity::PasswordAuthOutcome::Throttled => {
            record_auth_failure(metadata, "authenticate.password", "throttled")?;
            return Err(ApiError::TooManyAuthAttempts);
        }
    };
    let tokens = metadata
        .issue_auth_session(
            identity.principal.id,
            match client_kind {
                AuthClientKind::Native => MetadataAuthClientKind::Native,
                AuthClientKind::Web => MetadataAuthClientKind::Web,
            },
            request.client_label.as_deref(),
            NewOperationAudit {
                actor_principal_id: Some(identity.principal.id),
                action: "authenticate.password".into(),
                target: "auth_session".into(),
                target_id: None,
                status: "succeeded".into(),
                result_code: None,
                row_count: None,
                error_message: None,
                correlation_id: crate::correlation::current(),
            },
        )
        .await?;
    state.sessions.push_operation_local(
        Operation::Authenticate {
            method: sift_protocol::AuthenticationMethod::Password,
        },
        OperationStatus::Succeeded,
        Some(identity.principal.id.0),
        None,
        None,
        None,
    );
    auth_login_response(tokens, client_kind == AuthClientKind::Web)
}

async fn refresh_auth(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<RefreshAuthRequest>,
) -> ApiResult<Response> {
    let metadata = metadata_store(&state)?;
    let cookie_refresh = cookie_value(&headers, "sift_refresh");
    if cookie_refresh.is_some() && !valid_csrf(&headers) {
        return Err(ApiError::Forbidden("invalid CSRF token".into()));
    }
    let presented = request
        .refresh_token
        .as_deref()
        .or(cookie_refresh)
        .ok_or(ApiError::Unauthorized)?;
    let audit = NewOperationAudit {
        actor_principal_id: None,
        action: "refresh_auth_session".into(),
        target: "auth_session".into(),
        target_id: None,
        status: "succeeded".into(),
        result_code: None,
        row_count: None,
        error_message: None,
        correlation_id: crate::correlation::current(),
    };
    match metadata.rotate_auth_refresh_token(presented, audit).await? {
        RefreshAuthResult::Issued(tokens) => {
            state
                .auth
                .runtime
                .invalidate_auth_session(&tokens.session_id);
            state.sessions.push_operation_local(
                Operation::RefreshAuthSession,
                OperationStatus::Succeeded,
                None,
                None,
                None,
                None,
            );
            auth_login_response(tokens, cookie_refresh.is_some())
        }
        RefreshAuthResult::ReplayDetected => {
            state.auth.runtime.invalidate_all_access_tokens();
            Err(ApiError::Unauthorized)
        }
        RefreshAuthResult::Invalid => Err(ApiError::Unauthorized),
    }
}

async fn logout_auth(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
) -> ApiResult<Response> {
    let session_id = auth.auth_session_id.as_deref().ok_or_else(|| {
        ApiError::BadRequest("the current credential is not an interactive session".into())
    })?;
    metadata_store(&state)?.revoke_auth_session(
        session_id,
        "logout",
        metadata_audit_record(auth.principal_id, "logout", "auth_session", None),
    )?;
    state.auth.runtime.invalidate_auth_session(session_id);
    state.sessions.push_operation_local(
        Operation::Logout {
            all_sessions: false,
        },
        OperationStatus::Succeeded,
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    Ok(logout_response(auth.cookie_authenticated))
}

async fn logout_all_auth(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
) -> ApiResult<Response> {
    metadata_store(&state)?.revoke_all_auth_sessions(
        auth.principal_id,
        "logout_all",
        metadata_audit_record(auth.principal_id, "logout_all", "auth_session", None),
    )?;
    state.auth.runtime.invalidate_principal(auth.principal_id);
    state.sessions.push_operation_local(
        Operation::Logout { all_sessions: true },
        OperationStatus::Succeeded,
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    Ok(logout_response(auth.cookie_authenticated))
}

async fn change_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Extension(auth): Extension<AuthContext>,
    Json(request): Json<ChangePasswordRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let metadata = metadata_store(&state)?;
    let identity = metadata
        .list_auth_identities(auth.principal_id)?
        .into_iter()
        .find(|identity| {
            identity.method == sift_metadata::AuthIdentityMethod::Password
                && identity.disabled_at.is_none()
        })
        .ok_or_else(|| ApiError::BadRequest("principal has no password identity".into()))?;
    let source = headers
        .get(&PEER_ADDR_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown");
    let verified = state
        .auth
        .runtime
        .authenticate_password(
            metadata,
            source,
            &identity.subject,
            request.current_password.into_bytes(),
        )
        .await
        .map_err(|error| ApiError::Internal(error.to_string()))?;
    match verified {
        crate::identity::PasswordAuthOutcome::Authenticated(password)
            if password.principal.id == auth.principal_id => {}
        crate::identity::PasswordAuthOutcome::Throttled => {
            return Err(ApiError::TooManyAuthAttempts)
        }
        crate::identity::PasswordAuthOutcome::Authenticated(_)
        | crate::identity::PasswordAuthOutcome::Denied => return Err(ApiError::Unauthorized),
    }
    let verifier = crate::identity::hash_password(request.new_password.into_bytes())
        .await
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    metadata
        .replace_password_verifier(
            identity.id,
            verifier.as_bytes(),
            metadata_audit_record(
                auth.principal_id,
                "change_password",
                "auth_identity",
                Some(identity.id.0),
            ),
        )
        .await?;
    state.auth.runtime.invalidate_principal(auth.principal_id);
    state.sessions.push_operation_local(
        Operation::ChangePassword,
        OperationStatus::Succeeded,
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    Ok(Json(json!({"ok": true})))
}

async fn reset_password(
    State(state): State<AppState>,
    Json(request): Json<PasswordResetRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let verifier = state
        .auth
        .runtime
        .hash_password_bounded(request.new_password.into_bytes())
        .await
        .map_err(|error| ApiError::BadRequest(error.to_string()))?
        .ok_or(ApiError::TooManyAuthAttempts)?;
    let principal = match metadata_store(&state)?
        .consume_password_reset(
            &request.token,
            verifier.as_bytes(),
            NewOperationAudit {
                actor_principal_id: None,
                action: "manage_principal.reset_password".into(),
                target: "auth_identity".into(),
                target_id: None,
                status: "succeeded".into(),
                result_code: None,
                row_count: None,
                error_message: None,
                correlation_id: crate::correlation::current(),
            },
        )
        .await
    {
        Ok(principal) => principal,
        Err(sift_metadata::MetadataError::InvalidPasswordReset) => {
            return Err(ApiError::Unauthorized)
        }
        Err(error) => return Err(error.into()),
    };
    state.auth.runtime.invalidate_principal(principal);
    state.sessions.push_operation_local(
        Operation::ManagePrincipal {
            action: sift_protocol::IdentityAdminAction::Reset,
            principal_id: Some(principal.0),
        },
        OperationStatus::Succeeded,
        Some(principal.0),
        None,
        None,
        None,
    );
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct GithubStartQuery {
    client_kind: Option<AuthClientKind>,
}

#[derive(Deserialize)]
struct GithubCallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct GithubTokenResponse {
    access_token: String,
}

#[derive(Deserialize)]
struct GithubUserResponse {
    id: u64,
    login: String,
    name: Option<String>,
    email: Option<String>,
    avatar_url: Option<String>,
}

async fn github_start(
    State(state): State<AppState>,
    Query(query): Query<GithubStartQuery>,
) -> ApiResult<Response> {
    use base64::Engine as _;
    use sha2::Digest as _;

    let config =
        state.auth.github.as_ref().ok_or_else(|| {
            ApiError::BadRequest("GitHub authentication is not configured".into())
        })?;
    let client_kind = query.client_kind.unwrap_or(AuthClientKind::Web);
    let attempt = metadata_store(&state)?
        .create_github_oauth_attempt(match client_kind {
            AuthClientKind::Native => MetadataAuthClientKind::Native,
            AuthClientKind::Web => MetadataAuthClientKind::Web,
        })
        .await?;
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(sha2::Sha256::digest(attempt.code_verifier.as_bytes()));
    let callback = format!(
        "{}/v1/auth/github/callback",
        config.public_base_url.trim_end_matches('/')
    );
    let mut authorize = reqwest::Url::parse("https://github.com/login/oauth/authorize")
        .map_err(|error| ApiError::Internal(error.to_string()))?;
    authorize.query_pairs_mut().extend_pairs([
        ("client_id", config.client_id.as_str()),
        ("redirect_uri", callback.as_str()),
        ("scope", "read:user"),
        ("state", attempt.state.as_str()),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("allow_signup", "false"),
    ]);
    if client_kind == AuthClientKind::Native {
        Ok(Json(GithubNativeAuthStartResponse {
            authorization_url: authorize.to_string(),
            handoff_token: attempt
                .handoff_token
                .ok_or_else(|| ApiError::Internal("native OAuth handoff missing".into()))?,
        })
        .into_response())
    } else {
        Ok(Redirect::temporary(authorize.as_str()).into_response())
    }
}

async fn github_callback(
    State(state): State<AppState>,
    Query(query): Query<GithubCallbackQuery>,
) -> ApiResult<Response> {
    if query.error.is_some() {
        return Err(ApiError::Unauthorized);
    }
    let code = query.code.as_deref().ok_or(ApiError::Unauthorized)?;
    let oauth_state = query.state.as_deref().ok_or(ApiError::Unauthorized)?;
    let config =
        state.auth.github.as_ref().ok_or_else(|| {
            ApiError::BadRequest("GitHub authentication is not configured".into())
        })?;
    let attempt = metadata_store(&state)?
        .consume_github_oauth_attempt(oauth_state)
        .await
        .map_err(|_| ApiError::Unauthorized)?;
    let callback = format!(
        "{}/v1/auth/github/callback",
        config.public_base_url.trim_end_matches('/')
    );
    let token_response = config
        .http
        .post("https://github.com/login/oauth/access_token")
        .header(header::ACCEPT, "application/json")
        .form(&[
            ("client_id", config.client_id.as_str()),
            ("client_secret", config.client_secret.as_str()),
            ("code", code),
            ("redirect_uri", callback.as_str()),
            ("code_verifier", attempt.code_verifier.as_str()),
        ])
        .send()
        .await
        .map_err(|_| ApiError::Unauthorized)?;
    if !token_response.status().is_success() {
        return Err(ApiError::Unauthorized);
    }
    let token: GithubTokenResponse = token_response
        .json()
        .await
        .map_err(|_| ApiError::Unauthorized)?;
    let user_response = config
        .http
        .get("https://api.github.com/user")
        .bearer_auth(&token.access_token)
        .header(header::ACCEPT, "application/vnd.github+json")
        .header(header::USER_AGENT, "sift")
        .send()
        .await
        .map_err(|_| ApiError::Unauthorized)?;
    if !user_response.status().is_success() {
        return Err(ApiError::Unauthorized);
    }
    let user: GithubUserResponse = user_response
        .json()
        .await
        .map_err(|_| ApiError::Unauthorized)?;
    // `token` is dropped immediately after this profile fetch and is never
    // persisted or included in operation/audit values.
    drop(token);
    let metadata = metadata_store(&state)?;
    let principal = metadata
        .complete_github_identity(
            GithubProfile {
                id: user.id,
                login: user.login,
                display_name: user.name,
                email: user.email,
                avatar_url: user.avatar_url,
            },
            NewOperationAudit {
                actor_principal_id: None,
                action: "authenticate.github".into(),
                target: "auth_identity".into(),
                target_id: None,
                status: "succeeded".into(),
                result_code: None,
                row_count: None,
                error_message: None,
                correlation_id: crate::correlation::current(),
            },
        )?
        .ok_or(ApiError::Unauthorized)?;
    if attempt.client_kind == MetadataAuthClientKind::Native {
        metadata.complete_native_oauth_attempt(&attempt.attempt_id, principal.id)?;
        return Ok(Json(json!({
            "ok": true,
            "message": "GitHub authentication complete; return to Sift"
        }))
        .into_response());
    }
    let tokens = metadata
        .issue_auth_session(
            principal.id,
            MetadataAuthClientKind::Web,
            Some("GitHub OAuth"),
            metadata_audit_record(
                principal.id,
                "authenticate.github.session",
                "auth_session",
                None,
            ),
        )
        .await?;
    state.sessions.push_operation_local(
        Operation::Authenticate {
            method: sift_protocol::AuthenticationMethod::Github,
        },
        OperationStatus::Succeeded,
        Some(principal.id.0),
        None,
        None,
        None,
    );
    auth_login_response(tokens, true)
}

async fn github_native_exchange(
    State(state): State<AppState>,
    Json(request): Json<GithubNativeAuthExchangeRequest>,
) -> ApiResult<Json<AuthTokensResponse>> {
    let metadata = metadata_store(&state)?;
    let principal = metadata
        .consume_native_oauth_handoff(&request.handoff_token)
        .await
        .map_err(|_| ApiError::Unauthorized)?;
    let tokens = metadata
        .issue_auth_session(
            principal,
            MetadataAuthClientKind::Native,
            Some("GitHub OAuth native handoff"),
            metadata_audit_record(
                principal,
                "authenticate.github.session",
                "auth_session",
                None,
            ),
        )
        .await?;
    state.sessions.push_operation_local(
        Operation::Authenticate {
            method: sift_protocol::AuthenticationMethod::Github,
        },
        OperationStatus::Succeeded,
        Some(principal.0),
        None,
        None,
        None,
    );
    Ok(Json(AuthTokensResponse {
        access_token: tokens.access_token,
        access_expires_at: tokens.access_expires_at,
        refresh_token: tokens.refresh_token,
        refresh_expires_at: tokens.refresh_expires_at,
    }))
}

async fn create_github_allowlist(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Json(request): Json<CreateGithubAllowlistRequest>,
) -> ApiResult<Json<sift_metadata::GithubAllowlistEntry>> {
    ensure_instance_admin(&state, &auth)?;
    let login = crate::identity::normalize_github_login(&request.login)
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    let target = request.target_principal_id.map(PrincipalId);
    if let Some(target) = target {
        metadata_store(&state)?
            .principal_by_id(target)?
            .ok_or(ApiError::Metadata(
                sift_metadata::MetadataError::PrincipalNotFound(target),
            ))?;
    }
    let entry = metadata_store(&state)?.create_github_allowlist_entry(
        &login,
        target,
        auth.principal_id,
        metadata_audit_record(
            auth.principal_id,
            "github_allowlist.create",
            "github_allowlist",
            None,
        ),
    )?;
    state.sessions.push_operation_local(
        Operation::ManageGithubAllowlist {
            action: sift_protocol::IdentityAdminAction::Create,
            principal_id: request.target_principal_id,
        },
        OperationStatus::Succeeded,
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    Ok(Json(entry))
}

async fn list_github_allowlist(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
) -> ApiResult<Json<Vec<sift_metadata::GithubAllowlistEntry>>> {
    ensure_instance_admin(&state, &auth)?;
    Ok(Json(
        metadata_store(&state)?.list_github_allowlist_entries()?,
    ))
}

async fn revoke_github_allowlist(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    ensure_instance_admin(&state, &auth)?;
    metadata_store(&state)?.revoke_github_allowlist_entry(
        GithubAllowlistId(id),
        metadata_audit_record(
            auth.principal_id,
            "github_allowlist.revoke",
            "github_allowlist",
            Some(id),
        ),
    )?;
    state.sessions.push_operation_local(
        Operation::ManageGithubAllowlist {
            action: sift_protocol::IdentityAdminAction::Revoke,
            principal_id: None,
        },
        OperationStatus::Succeeded,
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    Ok(Json(json!({"ok": true})))
}

async fn admin_create_principal(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Json(request): Json<AdminCreatePasswordPrincipalRequest>,
) -> ApiResult<Json<AuthPrincipal>> {
    ensure_instance_admin(&state, &auth)?;
    let username = crate::identity::normalize_username(&request.username)
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    if request.display_name.trim().is_empty() || request.display_name.len() > 200 {
        return Err(ApiError::BadRequest(
            "display name must be between 1 and 200 characters".into(),
        ));
    }
    let verifier = crate::identity::hash_password(request.password.into_bytes())
        .await
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    let principal = metadata_store(&state)?
        .create_password_principal(
            sift_metadata::NewPasswordPrincipal {
                username: &username,
                display_name: request.display_name.trim(),
                email: request.email.as_deref(),
                is_instance_admin: request.is_instance_admin,
            },
            verifier.as_bytes(),
            metadata_audit_record(
                auth.principal_id,
                "manage_principal.create",
                "principal",
                None,
            ),
        )
        .await?;
    state.sessions.push_operation_local(
        Operation::ManagePrincipal {
            action: sift_protocol::IdentityAdminAction::Create,
            principal_id: Some(principal.id.0),
        },
        OperationStatus::Succeeded,
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    Ok(Json(AuthPrincipal {
        id: principal.id.0,
        display_name: principal.display_name,
        email: principal.email,
        avatar_url: principal.avatar_url,
        is_instance_admin: principal.is_instance_admin,
    }))
}

async fn admin_set_principal_disabled(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<i64>,
    Json(request): Json<AdminSetPrincipalDisabledRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    ensure_instance_admin(&state, &auth)?;
    metadata_store(&state)?.set_principal_disabled(
        PrincipalId(id),
        request.disabled,
        metadata_audit_record(
            auth.principal_id,
            if request.disabled {
                "manage_principal.disable"
            } else {
                "manage_principal.enable"
            },
            "principal",
            Some(id),
        ),
    )?;
    state.auth.runtime.invalidate_principal(PrincipalId(id));
    state.sessions.push_operation_local(
        Operation::ManagePrincipal {
            action: if request.disabled {
                sift_protocol::IdentityAdminAction::Disable
            } else {
                sift_protocol::IdentityAdminAction::Enable
            },
            principal_id: Some(id),
        },
        OperationStatus::Succeeded,
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    Ok(Json(json!({"ok": true})))
}

async fn admin_list_principal_identities(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<AuthIdentitySummary>>> {
    ensure_instance_admin(&state, &auth)?;
    if metadata_store(&state)?
        .principal_by_id(PrincipalId(id))?
        .is_none()
    {
        return Err(ApiError::Metadata(
            sift_metadata::MetadataError::PrincipalNotFound(PrincipalId(id)),
        ));
    }
    let identities = metadata_store(&state)?
        .list_auth_identities(PrincipalId(id))?
        .into_iter()
        .map(|identity| AuthIdentitySummary {
            id: identity.id.0,
            method: format!("{:?}", identity.method).to_lowercase(),
            issuer: identity.issuer,
            subject: identity.subject,
            provider_login: identity.provider_login,
            disabled: identity.disabled_at.is_some(),
        })
        .collect();
    Ok(Json(identities))
}

async fn admin_link_password_identity(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<i64>,
    Json(request): Json<AdminLinkPasswordIdentityRequest>,
) -> ApiResult<Json<AuthIdentitySummary>> {
    ensure_instance_admin(&state, &auth)?;
    let username = crate::identity::normalize_username(&request.username)
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    let verifier = crate::identity::hash_password(request.password.into_bytes())
        .await
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    let identity = metadata_store(&state)?
        .link_password_identity(
            PrincipalId(id),
            &username,
            verifier.as_bytes(),
            metadata_audit_record(
                auth.principal_id,
                "manage_principal.link",
                "auth_identity",
                None,
            ),
        )
        .await?;
    state.sessions.push_operation_local(
        Operation::ManagePrincipal {
            action: sift_protocol::IdentityAdminAction::Link,
            principal_id: Some(id),
        },
        OperationStatus::Succeeded,
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    Ok(Json(AuthIdentitySummary {
        id: identity.id.0,
        method: "password".into(),
        issuer: identity.issuer,
        subject: identity.subject,
        provider_login: identity.provider_login,
        disabled: false,
    }))
}

async fn admin_unlink_identity(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((principal_id, identity_id)): Path<(i64, i64)>,
) -> ApiResult<Json<serde_json::Value>> {
    ensure_instance_admin(&state, &auth)?;
    metadata_store(&state)?
        .unlink_auth_identity(
            PrincipalId(principal_id),
            AuthIdentityId(identity_id),
            metadata_audit_record(
                auth.principal_id,
                "manage_principal.unlink",
                "auth_identity",
                Some(identity_id),
            ),
        )
        .await?;
    state
        .auth
        .runtime
        .invalidate_principal(PrincipalId(principal_id));
    state.sessions.push_operation_local(
        Operation::ManagePrincipal {
            action: sift_protocol::IdentityAdminAction::Unlink,
            principal_id: Some(principal_id),
        },
        OperationStatus::Succeeded,
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    Ok(Json(json!({"ok": true})))
}

async fn admin_list_auth_sessions(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<AuthSessionSummary>>> {
    ensure_instance_admin(&state, &auth)?;
    if metadata_store(&state)?
        .principal_by_id(PrincipalId(id))?
        .is_none()
    {
        return Err(ApiError::Metadata(
            sift_metadata::MetadataError::PrincipalNotFound(PrincipalId(id)),
        ));
    }
    Ok(Json(
        metadata_store(&state)?.list_principal_auth_sessions(PrincipalId(id))?,
    ))
}

async fn admin_revoke_auth_session(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((principal_id, session_id)): Path<(i64, String)>,
) -> ApiResult<Json<serde_json::Value>> {
    ensure_instance_admin(&state, &auth)?;
    metadata_store(&state)?.revoke_principal_auth_session(
        PrincipalId(principal_id),
        &session_id,
        metadata_audit_record(
            auth.principal_id,
            "manage_principal.revoke_session",
            "auth_session",
            None,
        ),
    )?;
    state.auth.runtime.invalidate_auth_session(&session_id);
    state.sessions.push_operation_local(
        Operation::ManagePrincipal {
            action: sift_protocol::IdentityAdminAction::Revoke,
            principal_id: Some(principal_id),
        },
        OperationStatus::Succeeded,
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    Ok(Json(json!({"ok": true})))
}

async fn admin_issue_password_reset(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((principal_id, identity_id)): Path<(i64, i64)>,
) -> ApiResult<Json<IssuedPasswordResetResponse>> {
    ensure_instance_admin(&state, &auth)?;
    let issued = metadata_store(&state)?
        .issue_password_reset(
            PrincipalId(principal_id),
            AuthIdentityId(identity_id),
            auth.principal_id,
            metadata_audit_record(
                auth.principal_id,
                "manage_principal.issue_password_reset",
                "auth_identity",
                Some(identity_id),
            ),
        )
        .await?;
    state.sessions.push_operation_local(
        Operation::ManagePrincipal {
            action: sift_protocol::IdentityAdminAction::Reset,
            principal_id: Some(principal_id),
        },
        OperationStatus::Succeeded,
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    Ok(Json(IssuedPasswordResetResponse {
        token: issued.token,
        expires_at: issued.expires_at,
    }))
}

fn ensure_instance_admin(state: &AppState, auth: &AuthContext) -> ApiResult<()> {
    let principal = metadata_store(state)?
        .principal_by_id(auth.principal_id)?
        .ok_or(ApiError::Unauthorized)?;
    if principal.is_instance_admin && principal.disabled_at.is_none() {
        Ok(())
    } else {
        Err(ApiError::Forbidden(
            "instance administrator access required".into(),
        ))
    }
}

async fn create_tenant_invitation(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(tenant): Path<i64>,
    Json(request): Json<CreateTenantInvitationRequest>,
) -> ApiResult<Json<IssuedTenantInvitationResponse>> {
    let tenant = TenantId(tenant);
    if !is_tenant_admin(&auth, tenant) {
        return Err(ApiError::Forbidden(
            "tenant administrator access required".into(),
        ));
    }
    let now = chrono::Utc::now();
    if request.expires_at <= now || request.expires_at > now + chrono::Duration::days(30) {
        return Err(ApiError::BadRequest(
            "invitation expiry must be within the next 30 days".into(),
        ));
    }
    let role = match request.role {
        InvitationRole::Admin => sift_metadata::MembershipRole::Admin,
        InvitationRole::Member => sift_metadata::MembershipRole::Member,
        InvitationRole::Viewer => sift_metadata::MembershipRole::Viewer,
    };
    let issued = metadata_store(&state)?
        .issue_tenant_invitation(
            tenant,
            role,
            auth.principal_id,
            request.target_principal_id.map(PrincipalId),
            request.expires_at,
            metadata_audit_record(
                auth.principal_id,
                "tenant_invitation.create",
                "tenant_invitation",
                None,
            ),
        )
        .await?;
    state.sessions.push_operation_local(
        Operation::ManageTenantInvitation {
            action: sift_protocol::IdentityAdminAction::Create,
            tenant_id: tenant.0,
        },
        OperationStatus::Succeeded,
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    Ok(Json(IssuedTenantInvitationResponse {
        invitation_id: issued.invitation.id.0,
        token: issued.token,
        expires_at: issued.invitation.expires_at,
    }))
}

async fn list_tenant_invitations(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(tenant): Path<i64>,
) -> ApiResult<Json<Vec<sift_metadata::TenantInvitation>>> {
    let tenant = TenantId(tenant);
    if !is_tenant_admin(&auth, tenant) {
        return Err(ApiError::Forbidden(
            "tenant administrator access required".into(),
        ));
    }
    Ok(Json(
        metadata_store(&state)?.list_tenant_invitations(tenant)?,
    ))
}

async fn revoke_tenant_invitation(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((tenant, id)): Path<(i64, i64)>,
) -> ApiResult<Json<serde_json::Value>> {
    let tenant = TenantId(tenant);
    if !is_tenant_admin(&auth, tenant) {
        return Err(ApiError::Forbidden(
            "tenant administrator access required".into(),
        ));
    }
    metadata_store(&state)?.revoke_tenant_invitation(
        TenantInvitationId(id),
        metadata_audit_record(
            auth.principal_id,
            "tenant_invitation.revoke",
            "tenant_invitation",
            Some(id),
        ),
    )?;
    state.sessions.push_operation_local(
        Operation::ManageTenantInvitation {
            action: sift_protocol::IdentityAdminAction::Revoke,
            tenant_id: tenant.0,
        },
        OperationStatus::Succeeded,
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    Ok(Json(json!({"ok": true})))
}

async fn accept_tenant_invitation(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Json(request): Json<AcceptTenantInvitationRequest>,
) -> ApiResult<Json<TenantMembership>> {
    let membership = metadata_store(&state)?
        .accept_tenant_invitation(
            &request.token,
            auth.principal_id,
            metadata_audit_record(
                auth.principal_id,
                "tenant_invitation.accept",
                "tenant_invitation",
                None,
            ),
        )
        .await?;
    state.sessions.push_operation_local(
        Operation::ManageTenantInvitation {
            action: sift_protocol::IdentityAdminAction::Link,
            tenant_id: membership.tenant.id.0,
        },
        OperationStatus::Succeeded,
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    Ok(Json(membership))
}

async fn register_principal_key(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Json(request): Json<RegisterPrincipalKeyRequest>,
) -> ApiResult<Json<sift_metadata::PrincipalKey>> {
    use base64::Engine as _;
    use sha2::Digest as _;

    if request.label.trim().is_empty() || request.label.len() > 100 {
        return Err(ApiError::BadRequest(
            "key label must be between 1 and 100 characters".into(),
        ));
    }
    let public_key = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(request.public_key)
        .map_err(|_| ApiError::BadRequest("invalid Ed25519 public key encoding".into()))?;
    if public_key.len() != 32 {
        return Err(ApiError::BadRequest(
            "Ed25519 public key must be exactly 32 bytes".into(),
        ));
    }
    ed25519_dalek::VerifyingKey::from_bytes(
        public_key
            .as_slice()
            .try_into()
            .map_err(|_| ApiError::BadRequest("invalid Ed25519 public key".into()))?,
    )
    .map_err(|_| ApiError::BadRequest("invalid Ed25519 public key".into()))?;
    let fingerprint = format!(
        "SHA256:{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(&public_key))
    );
    let key = metadata_store(&state)?.register_principal_key(
        auth.principal_id,
        &public_key,
        &fingerprint,
        request.label.trim(),
        metadata_audit_record(
            auth.principal_id,
            "principal_key.register",
            "principal_key",
            None,
        ),
    )?;
    state.sessions.push_operation_local(
        Operation::ManagePrincipalKey {
            action: sift_protocol::IdentityAdminAction::Create,
            key_id: Some(key.id.0),
        },
        OperationStatus::Succeeded,
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    Ok(Json(key))
}

async fn list_principal_keys(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
) -> ApiResult<Json<Vec<sift_metadata::PrincipalKey>>> {
    Ok(Json(
        metadata_store(&state)?.list_principal_keys(auth.principal_id)?,
    ))
}

async fn revoke_principal_key(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    metadata_store(&state)?.revoke_principal_key(
        PrincipalKeyId(id),
        auth.principal_id,
        metadata_audit_record(
            auth.principal_id,
            "principal_key.revoke",
            "principal_key",
            Some(id),
        ),
    )?;
    state.sessions.push_operation_local(
        Operation::ManagePrincipalKey {
            action: sift_protocol::IdentityAdminAction::Revoke,
            key_id: Some(id),
        },
        OperationStatus::Succeeded,
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    Ok(Json(json!({"ok": true})))
}

async fn issue_key_challenge(
    State(state): State<AppState>,
    Json(request): Json<KeyChallengeRequest>,
) -> ApiResult<Json<KeyChallengeResponse>> {
    use base64::Engine as _;

    let challenge = metadata_store(&state)?
        .issue_key_challenge(&request.fingerprint)
        .map_err(|_| ApiError::Unauthorized)?;
    let nonce = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&challenge.nonce);
    Ok(Json(KeyChallengeResponse {
        message: key_challenge_message(&state.auth.instance_audience, &nonce),
        nonce,
        expires_at: challenge.expires_at,
    }))
}

async fn authenticate_key(
    State(state): State<AppState>,
    Json(request): Json<KeyAuthenticateRequest>,
) -> ApiResult<Json<AuthTokensResponse>> {
    use base64::Engine as _;
    use ed25519_dalek::Verifier as _;

    let nonce = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&request.nonce)
        .map_err(|_| ApiError::Unauthorized)?;
    let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&request.signature)
        .map_err(|_| ApiError::Unauthorized)?;
    let consumed = metadata_store(&state)?
        .consume_key_challenge(&nonce)
        .map_err(|_| ApiError::Unauthorized)?;
    let public_key: [u8; 32] = consumed
        .principal_key
        .public_key
        .as_slice()
        .try_into()
        .map_err(|_| ApiError::Unauthorized)?;
    let signature =
        ed25519_dalek::Signature::from_slice(&signature).map_err(|_| ApiError::Unauthorized)?;
    let verifying_key =
        ed25519_dalek::VerifyingKey::from_bytes(&public_key).map_err(|_| ApiError::Unauthorized)?;
    let nonce_text = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&nonce);
    verifying_key
        .verify(
            key_challenge_message(&state.auth.instance_audience, &nonce_text).as_bytes(),
            &signature,
        )
        .map_err(|_| ApiError::Unauthorized)?;
    let tokens = metadata_store(&state)?
        .issue_auth_session(
            consumed.principal_key.principal_id,
            MetadataAuthClientKind::Keypair,
            Some(&consumed.principal_key.label),
            metadata_audit_record(
                consumed.principal_key.principal_id,
                "authenticate.keypair",
                "auth_session",
                None,
            ),
        )
        .await?;
    Ok(Json(auth_tokens_response(tokens)))
}

fn key_challenge_message(audience: &str, nonce: &str) -> String {
    format!("sift-key-auth-v1\n{audience}\n{nonce}")
}

async fn whoami(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
) -> ApiResult<Json<WhoAmIResponse>> {
    let principal = metadata_store(&state)?
        .principal_by_id(auth.principal_id)?
        .ok_or(ApiError::Unauthorized)?;
    let memberships = auth
        .tenants
        .iter()
        .map(|membership| AuthTenantMembership {
            tenant_id: membership.tenant.id.0,
            tenant_name: membership.tenant.name.clone(),
            role: match membership.role {
                sift_metadata::MembershipRole::Owner => "owner",
                sift_metadata::MembershipRole::Admin => "admin",
                sift_metadata::MembershipRole::Member => "member",
                sift_metadata::MembershipRole::Viewer => "viewer",
            }
            .into(),
        })
        .collect();
    Ok(Json(WhoAmIResponse {
        principal: AuthPrincipal {
            id: principal.id.0,
            display_name: principal.display_name,
            email: principal.email,
            avatar_url: principal.avatar_url,
            is_instance_admin: principal.is_instance_admin,
        },
        memberships,
        auth_session_id: auth.auth_session_id,
    }))
}

fn auth_tokens_response(tokens: sift_metadata::IssuedAuthTokens) -> AuthTokensResponse {
    AuthTokensResponse {
        access_token: tokens.access_token,
        access_expires_at: tokens.access_expires_at,
        refresh_token: tokens.refresh_token,
        refresh_expires_at: tokens.refresh_expires_at,
    }
}

fn auth_login_response(tokens: sift_metadata::IssuedAuthTokens, web: bool) -> ApiResult<Response> {
    if !web {
        return Ok(Json(auth_tokens_response(tokens)).into_response());
    }
    let csrf = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let access_cookie = format!(
        "sift_access={}; Path=/; Max-Age=900; Secure; HttpOnly; SameSite=Lax",
        tokens.access_token
    );
    let refresh_cookie = format!(
        "sift_refresh={}; Path=/v1/auth/refresh; Max-Age=2592000; Secure; HttpOnly; SameSite=Strict",
        tokens.refresh_token
    );
    let csrf_cookie = format!("sift_csrf={csrf}; Path=/; Max-Age=2592000; Secure; SameSite=Strict");
    let mut response = Json(WebAuthResponse {
        access_expires_at: tokens.access_expires_at,
        refresh_expires_at: tokens.refresh_expires_at,
        csrf_token: csrf,
    })
    .into_response();
    for cookie in [access_cookie, refresh_cookie, csrf_cookie] {
        response.headers_mut().append(
            header::SET_COOKIE,
            HeaderValue::from_str(&cookie)
                .map_err(|error| ApiError::Internal(format!("invalid auth cookie: {error}")))?,
        );
    }
    Ok(response)
}

fn logout_response(clear_cookies: bool) -> Response {
    let mut response = Json(json!({"ok": true})).into_response();
    if clear_cookies {
        for cookie in [
            "sift_access=; Path=/; Max-Age=0; Secure; HttpOnly; SameSite=Lax",
            "sift_refresh=; Path=/v1/auth/refresh; Max-Age=0; Secure; HttpOnly; SameSite=Strict",
            "sift_csrf=; Path=/; Max-Age=0; Secure; SameSite=Strict",
        ] {
            response
                .headers_mut()
                .append(header::SET_COOKIE, HeaderValue::from_static(cookie));
        }
    }
    response
}

fn record_auth_failure(metadata: &MetadataStore, action: &str, code: &str) -> ApiResult<()> {
    metadata.record_operation_audit(NewOperationAudit {
        actor_principal_id: None,
        action: action.into(),
        target: "auth_session".into(),
        target_id: None,
        status: "failed".into(),
        result_code: Some(code.into()),
        row_count: None,
        error_message: Some("authentication denied".into()),
        correlation_id: crate::correlation::current(),
    })?;
    Ok(())
}

async fn metadata_blocking<T>(f: impl FnOnce() -> ApiResult<T> + Send + 'static) -> ApiResult<T>
where
    T: Send + 'static,
{
    let result = tokio::task::spawn_blocking(f)
        .await
        .map_err(|error| ApiError::Internal(format!("metadata task failed: {error}")))?;
    result
}

fn bearer_from_headers(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
}

fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())?
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(key, value)| (key == name).then_some(value))
}

fn is_state_changing(method: &axum::http::Method) -> bool {
    !matches!(
        *method,
        axum::http::Method::GET | axum::http::Method::HEAD | axum::http::Method::OPTIONS
    )
}

fn valid_csrf(headers: &HeaderMap) -> bool {
    let header = headers
        .get("x-sift-csrf")
        .and_then(|value| value.to_str().ok());
    let cookie = cookie_value(headers, "sift_csrf");
    matches!((header, cookie), (Some(header), Some(cookie)) if constant_time_eq(header.as_bytes(), cookie.as_bytes()))
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
                auth_session_id: None,
                cookie_authenticated: false,
                access_expires_at: None,
            });
        }
        if state
            .auth
            .bearer_token
            .as_deref()
            .is_some_and(|expected| constant_time_eq(token.as_bytes(), expected.as_bytes()))
        {
            return local_auth_context(metadata);
        }
        // Explicit invalid credentials never fall through to loopback bypass.
        return Err(ApiError::Unauthorized);
    }

    // Team-mode validation forbids enabling this implicit path.
    if state.auth.loopback_bypass && peer_is_loopback(headers) {
        return local_auth_context(metadata);
    }

    Err(ApiError::Unauthorized)
}

fn local_auth_context(metadata: &MetadataStore) -> ApiResult<AuthContext> {
    let principal = metadata
        .resolve_principal_by_external_id("local:1")?
        .ok_or(ApiError::Unauthorized)?;
    let tenants = metadata.list_principal_tenants(principal.id)?;
    Ok(AuthContext {
        principal_id: principal.id,
        tenants,
        auth_session_id: None,
        cookie_authenticated: false,
        access_expires_at: None,
    })
}

async fn resolve_auth_context_blocking(
    state: AppState,
    headers: HeaderMap,
) -> ApiResult<AuthContext> {
    let bearer = bearer_from_headers(&headers);
    let cookie_token = bearer
        .is_none()
        .then(|| cookie_value(&headers, "sift_access"))
        .flatten();
    if let Some(token) = bearer.or(cookie_token) {
        if token.starts_with("sift_at_") {
            let metadata = metadata_store(&state)?;
            let session = state
                .auth
                .runtime
                .resolve_access_token(metadata, token)
                .await?
                .ok_or(ApiError::Unauthorized)?;
            let tenants = metadata.list_principal_tenants(session.principal.id)?;
            return Ok(AuthContext {
                principal_id: session.principal.id,
                tenants,
                auth_session_id: Some(session.session_id),
                cookie_authenticated: cookie_token.is_some(),
                access_expires_at: Some(session.expires_at),
            });
        }
        if cookie_token.is_some() {
            return Err(ApiError::Unauthorized);
        }
    }
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

/// Build the durable audit record for a successful security-critical
/// metadata mutation whose audit row is written transactionally with the
/// mutation itself (P1-meta-4). Mirrors the fields the async audit path
/// would derive from `Operation::Metadata`, so the persisted row is
/// identical regardless of which path wrote it. `correlation_id` is
/// captured here in the request task — it would not survive the hop to a
/// `spawn_blocking` thread.
fn metadata_audit_record(
    actor: PrincipalId,
    action: &str,
    target: &str,
    id: Option<i64>,
) -> NewOperationAudit {
    NewOperationAudit {
        actor_principal_id: Some(actor),
        action: action.to_string(),
        target: target.to_string(),
        target_id: id,
        status: "succeeded".to_string(),
        result_code: None,
        row_count: None,
        error_message: None,
        correlation_id: crate::correlation::current(),
    }
}

/// Record the in-memory ring + JSONL replay entry for a metadata mutation
/// whose durable audit row was already written transactionally
/// (P1-meta-4). Skips the async durable enqueue to avoid double-writing.
fn push_metadata_operation_local(
    state: &AppState,
    actor: PrincipalId,
    action: &str,
    target: &str,
    id: Option<i64>,
) {
    state.sessions.push_operation_local(
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

async fn list_available_operations(
    State(state): State<AppState>,
    auth: Option<Extension<AuthContext>>,
    Query(context): Query<sift_protocol::OperationCapabilityContext>,
) -> ApiResult<Json<Vec<sift_protocol::OperationCapability>>> {
    let operation = Operation::ListAvailableOperations {
        context: context.clone(),
    };
    let authorization = capability_authorization_scope(
        &state,
        auth.as_ref().map(|Extension(auth)| auth),
        &context,
    )?;
    let capabilities = finish_operation(
        &state.sessions,
        operation,
        crate::capability::evaluate(&state.sessions, &context, authorization.as_ref()),
        |_| None,
    )?;
    Ok(Json(capabilities))
}

fn capability_authorization_scope(
    state: &AppState,
    auth: Option<&AuthContext>,
    context: &sift_protocol::OperationCapabilityContext,
) -> ApiResult<Option<crate::authorization::AuthorizationScope>> {
    use crate::authorization::{AuthorizationRoomRole, AuthorizationScope};

    let Some(metadata) = state.metadata.as_ref() else {
        return Ok(Some(AuthorizationScope::trusted_local()));
    };
    let auth = auth.ok_or(ApiError::Unauthorized)?;
    let principal = metadata
        .principal_by_id(auth.principal_id)?
        .ok_or(ApiError::Unauthorized)?;
    let mut tenant = context.tenant_id.map(tenant_id).transpose()?;
    let mut profile_id = context
        .connection_profile_id
        .map(connection_profile_id)
        .transpose()?;
    let mut runtime_trusted_local = false;
    if let (Some(session), Some(connection)) = (context.session, context.connection) {
        match state.sessions.conn_entry(session, connection)?.provenance {
            crate::session::ConnectionProvenance::TrustedLocal => runtime_trusted_local = true,
            crate::session::ConnectionProvenance::Managed {
                principal_id,
                tenant_id,
                profile_id: managed_profile,
                ..
            } => {
                if principal_id != auth.principal_id {
                    return Err(ApiError::Forbidden(
                        "managed connection belongs to another principal".into(),
                    ));
                }
                merge_capability_tenant(&mut tenant, tenant_id)?;
                if profile_id.is_some_and(|explicit| explicit != managed_profile) {
                    return Err(ApiError::BadRequest(
                        "capability profile does not match runtime connection".into(),
                    ));
                }
                profile_id = Some(managed_profile);
            }
        }
    }
    let profile = profile_id
        .map(|id| metadata.get_connection_profile_for_any_tenant(id))
        .transpose()?;
    if let Some(profile) = &profile {
        merge_capability_tenant(&mut tenant, profile.tenant_id)?;
    }
    let room = context
        .room_id
        .map(room_id)
        .transpose()?
        .map(|id| metadata.get_room(id))
        .transpose()?;
    if let Some(room) = &room {
        merge_capability_tenant(&mut tenant, room.tenant_id)?;
    }
    let tenant_role = tenant.and_then(|tenant| {
        auth.tenants
            .iter()
            .find(|membership| membership.tenant.id == tenant)
            .map(|membership| sift_protocol::TenantRole::from(&membership.role))
    });
    if tenant.is_some() && tenant_role.is_none() {
        return Err(ApiError::Forbidden("tenant membership required".into()));
    }
    let room_role = match room {
        Some(room) => {
            let member = metadata
                .get_room_member(room.id, auth.principal_id)?
                .ok_or_else(|| ApiError::Forbidden("room membership required".into()))?;
            Some(match member.role {
                RoomRole::Owner => AuthorizationRoomRole::Owner,
                RoomRole::Editor => AuthorizationRoomRole::Editor,
                RoomRole::Viewer => AuthorizationRoomRole::Viewer,
            })
        }
        None => None,
    };
    Ok(Some(AuthorizationScope {
        authenticated: true,
        trusted_local: runtime_trusted_local
            || (state.auth.deployment == DeploymentPolicy::Personal
                && state.auth.transport == Transport::Loopback),
        instance_admin: principal.is_instance_admin,
        tenant_role,
        room_role,
        connection_policy: profile.map(|profile| profile.policy),
    }))
}

fn merge_capability_tenant(current: &mut Option<TenantId>, candidate: TenantId) -> ApiResult<()> {
    if current.is_some_and(|tenant| tenant != candidate) {
        return Err(ApiError::BadRequest(
            "capability context spans multiple tenants".into(),
        ));
    }
    *current = Some(candidate);
    Ok(())
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
            "/v1/auth/login": {
                "post": {
                    "operationId": "passwordLogin",
                    "summary": "Authenticate an instance-owned password identity",
                    "security": [],
                    "requestBody": json_body("PasswordLoginRequest"),
                    "responses": {
                        "200": { "description": "Native opaque credentials or browser cookie metadata", "content": json_one_of_content(&["AuthTokensResponse", "WebAuthResponse"]) },
                        "401": { "description": "Authentication denied" },
                        "429": { "description": "Authentication throttled" }
                    }
                }
            },
            "/v1/auth/refresh": {
                "post": {
                    "operationId": "refreshAuth",
                    "summary": "Atomically rotate an interactive refresh credential",
                    "security": [],
                    "requestBody": json_body("RefreshAuthRequest"),
                    "responses": {
                        "200": { "description": "Rotated native credentials or browser cookie metadata", "content": json_one_of_content(&["AuthTokensResponse", "WebAuthResponse"]) },
                        "401": { "description": "Invalid, expired, or replayed refresh credential" }
                    }
                }
            },
            "/v1/auth/logout": {
                "post": {
                    "operationId": "logoutAuth",
                    "summary": "Revoke the current interactive auth session",
                    "responses": { "200": { "description": "Ack", "content": json_object_content() } }
                }
            },
            "/v1/auth/logout-all": {
                "post": {
                    "operationId": "logoutAllAuth",
                    "summary": "Revoke every interactive auth session for the principal",
                    "responses": { "200": { "description": "Ack", "content": json_object_content() } }
                }
            },
            "/v1/auth/whoami": {
                "get": {
                    "operationId": "whoAmI",
                    "summary": "Return the authenticated principal and memberships",
                    "responses": { "200": { "description": "Authentication context", "content": json_content("WhoAmIResponse") } }
                }
            },
            "/v1/auth/password": {
                "put": {
                    "operationId": "changePassword",
                    "summary": "Replace the current principal password and revoke interactive sessions",
                    "requestBody": json_body("ChangePasswordRequest"),
                    "responses": {
                        "200": { "description": "Ack", "content": json_object_content() },
                        "401": { "description": "Current password denied" }
                    }
                }
            },
            "/v1/auth/password/reset": {
                "post": {
                    "operationId": "resetPassword",
                    "summary": "Consume an administrator-issued one-use password reset token",
                    "security": [],
                    "requestBody": json_body("PasswordResetRequest"),
                    "responses": {
                        "200": { "description": "Ack", "content": json_object_content() },
                        "401": { "description": "Invalid, expired, or consumed reset token" }
                    }
                }
            },
            "/v1/auth/github/start": {
                "get": {
                    "operationId": "githubAuthStart",
                    "summary": "Start the instance GitHub OAuth flow with state and S256 PKCE",
                    "security": [],
                    "responses": {
                        "200": { "description": "Native IDE authorization URL and one-use handoff", "content": json_content("GithubNativeAuthStartResponse") },
                        "307": { "description": "Browser GitHub authorization redirect" }
                    }
                }
            },
            "/v1/auth/github/callback": {
                "get": {
                    "operationId": "githubAuthCallback",
                    "summary": "Complete GitHub OAuth, enforce the allowlist, and set browser cookies",
                    "security": [],
                    "responses": {
                        "200": { "description": "Browser authentication metadata", "content": json_content("WebAuthResponse") },
                        "401": { "description": "OAuth or allowlist denial" }
                    }
                }
            },
            "/v1/auth/github/exchange": {
                "post": {
                    "operationId": "githubNativeAuthExchange",
                    "summary": "Exchange a completed one-use native GitHub handoff for Sift tokens",
                    "security": [],
                    "requestBody": json_body("GithubNativeAuthExchangeRequest"),
                    "responses": {
                        "200": { "description": "Native session credentials", "content": json_content("AuthTokensResponse") },
                        "401": { "description": "Pending, invalid, expired, or consumed handoff" }
                    }
                }
            },
            "/v1/admin/auth/github-allowlist": {
                "get": {
                    "operationId": "listGithubAllowlist",
                    "summary": "List GitHub allowlist entries (instance admin)",
                    "responses": { "200": { "description": "Allowlist entries", "content": json_array_content("GithubAllowlistEntry") } }
                },
                "post": {
                    "operationId": "createGithubAllowlist",
                    "summary": "Allow a GitHub login, optionally linked to an existing principal",
                    "requestBody": json_body("CreateGithubAllowlistRequest"),
                    "responses": { "200": { "description": "Allowlist entry", "content": json_content("GithubAllowlistEntry") } }
                }
            },
            "/v1/admin/auth/github-allowlist/{id}": {
                "delete": {
                    "operationId": "revokeGithubAllowlist",
                    "summary": "Revoke a pending GitHub allowlist entry",
                    "responses": { "200": { "description": "Ack", "content": json_object_content() } }
                }
            },
            "/v1/admin/principals": {
                "post": {
                    "operationId": "adminCreatePasswordPrincipal",
                    "requestBody": json_body("AdminCreatePasswordPrincipalRequest"),
                    "responses": { "200": { "description": "Principal", "content": json_content("AuthPrincipal") } }
                }
            },
            "/v1/admin/principals/{id}/disabled": {
                "put": {
                    "operationId": "adminSetPrincipalDisabled",
                    "requestBody": json_body("AdminSetPrincipalDisabledRequest"),
                    "responses": { "200": { "description": "Ack", "content": json_object_content() } }
                }
            },
            "/v1/admin/principals/{id}/identities": {
                "get": {
                    "operationId": "adminListPrincipalIdentities",
                    "responses": { "200": { "description": "Authentication identities", "content": json_array_content("AuthIdentitySummary") } }
                }
            },
            "/v1/admin/principals/{id}/identities/password": {
                "post": {
                    "operationId": "adminLinkPasswordIdentity",
                    "requestBody": json_body("AdminLinkPasswordIdentityRequest"),
                    "responses": { "200": { "description": "Linked identity", "content": json_content("AuthIdentitySummary") } }
                }
            },
            "/v1/admin/principals/{principal_id}/identities/{identity_id}": {
                "delete": {
                    "operationId": "adminUnlinkIdentity",
                    "responses": { "200": { "description": "Ack", "content": json_object_content() } }
                }
            },
            "/v1/admin/principals/{id}/auth-sessions": {
                "get": {
                    "operationId": "adminListAuthSessions",
                    "responses": { "200": { "description": "Authentication sessions", "content": json_array_content("AuthSessionSummary") } }
                }
            },
            "/v1/admin/principals/{principal_id}/auth-sessions/{session_id}": {
                "delete": {
                    "operationId": "adminRevokeAuthSession",
                    "responses": { "200": { "description": "Ack", "content": json_object_content() } }
                }
            },
            "/v1/admin/principals/{principal_id}/identities/{identity_id}/password-reset": {
                "post": {
                    "operationId": "adminIssuePasswordReset",
                    "responses": { "200": { "description": "One-use reset token", "content": json_content("IssuedPasswordResetResponse") } }
                }
            },
            "/v1/metadata/tenants/{id}/invitations": {
                "get": {
                    "operationId": "listTenantInvitations",
                    "responses": { "200": { "description": "Invitations", "content": json_array_content("TenantInvitation") } }
                },
                "post": {
                    "operationId": "createTenantInvitation",
                    "requestBody": json_body("CreateTenantInvitationRequest"),
                    "responses": { "200": { "description": "One-use invitation token", "content": json_content("IssuedTenantInvitationResponse") } }
                }
            },
            "/v1/auth/invitations/accept": {
                "post": {
                    "operationId": "acceptTenantInvitation",
                    "requestBody": json_body("AcceptTenantInvitationRequest"),
                    "responses": { "200": { "description": "Membership", "content": json_content("TenantMembership") } }
                }
            },
            "/v1/auth/keys": {
                "get": { "operationId": "listPrincipalKeys", "responses": { "200": { "description": "Keys", "content": json_array_content("PrincipalKey") } } },
                "post": { "operationId": "registerPrincipalKey", "requestBody": json_body("RegisterPrincipalKeyRequest"), "responses": { "200": { "description": "Key", "content": json_content("PrincipalKey") } } }
            },
            "/v1/auth/keys/challenge": {
                "post": { "operationId": "issueKeyChallenge", "security": [], "requestBody": json_body("KeyChallengeRequest"), "responses": { "200": { "description": "Challenge", "content": json_content("KeyChallengeResponse") } } }
            },
            "/v1/auth/keys/authenticate": {
                "post": { "operationId": "authenticateKey", "security": [], "requestBody": json_body("KeyAuthenticateRequest"), "responses": { "200": { "description": "Session credentials", "content": json_content("AuthTokensResponse") } } }
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
            "/v1/operations/available": {
                "get": {
                    "operationId": "listAvailableOperations",
                    "summary": "List contextual operation capabilities",
                    "responses": { "200": { "description": "Capabilities", "content": json_array_content("OperationCapability") } }
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
            "/v1/sessions/{id}/connections/{conn_id}/import/csv": {
                "post": {
                    "operationId": "importCsv",
                    "summary": "Import CSV into a table",
                    "requestBody": json_body("CsvImportRequest"),
                    "responses": { "200": { "description": "Import result", "content": json_content("CsvImportResponse") } }
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
            "/v1/sessions/{id}/connections/{conn_id}/processes": {
                "get": {
                    "operationId": "listProcesses",
                    "summary": "List database processes",
                    "responses": { "200": { "description": "Processes", "content": json_array_content("DatabaseProcess") } }
                }
            },
            "/v1/sessions/{id}/connections/{conn_id}/processes/kill": {
                "post": {
                    "operationId": "killProcess",
                    "summary": "Terminate a database process",
                    "requestBody": json_body("KillProcessRequest"),
                    "responses": { "200": { "description": "Termination result", "content": json_content("KillProcessResponse") } }
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
                "get": {
                    "operationId": "listTransactions",
                    "summary": "List open transactions",
                    "responses": { "200": { "description": "Transactions", "content": json_array_content("TransactionState") } }
                },
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
            "/v1/sessions/{id}/transactions/{tx_id}/preview": {
                "post": {
                    "operationId": "previewTransaction",
                    "summary": "Preview commit or rollback consequences",
                    "requestBody": json_body("TransactionPreviewRequest"),
                    "responses": { "200": { "description": "Preview", "content": json_content("TransactionPreview") } }
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
            "/v1/sessions/{id}/connections/{conn_id}/edits/preview": {
                "post": {
                    "operationId": "previewEdits",
                    "summary": "Preview parameterized inline-edit DML",
                    "requestBody": json_body("PreviewEditsRequest"),
                    "responses": { "200": { "description": "Edit plan", "content": json_content("EditPlan") } }
                }
            },
            "/v1/sessions/{id}/connections/{conn_id}/edits/apply": {
                "post": {
                    "operationId": "applyEdits",
                    "summary": "Apply inline edits transactionally",
                    "requestBody": json_body("ApplyEditsRequest"),
                    "responses": { "200": { "description": "Apply result", "content": json_content("ApplyEditsResult") } }
                }
            },
            "/v1/sessions/{id}/connections/{conn_id}/search/schema": {
                "post": {
                    "operationId": "searchSchema",
                    "summary": "Search schema objects and columns",
                    "requestBody": json_body("SchemaSearchRequest"),
                    "responses": { "200": { "description": "Schema matches", "content": json_content("SchemaSearchResponse") } }
                }
            },
            "/v1/sessions/{id}/connections/{conn_id}/search/data": {
                "post": {
                    "operationId": "searchData",
                    "summary": "Search table data with bounded fan-out",
                    "requestBody": json_body("DataSearchRequest"),
                    "responses": { "200": { "description": "Data matches", "content": json_content("DataSearchResponse") } }
                }
            },
            "/v1/sessions/{id}/connections/{conn_id}/explain": {
                "post": {
                    "operationId": "explainQuery",
                    "summary": "Capture a typed execution plan",
                    "requestBody": json_body("ExplainRequest"),
                    "responses": { "200": { "description": "Execution plan", "content": json_content("ExplainResponse") } }
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

fn json_one_of_content(schemas: &[&str]) -> serde_json::Value {
    json!({
        "application/json": {
            "schema": {
                "oneOf": schemas
                    .iter()
                    .map(|schema| json!({ "$ref": format!("#/components/schemas/{schema}") }))
                    .collect::<Vec<_>>()
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
    add_schema::<sift_protocol::PasswordLoginRequest>("PasswordLoginRequest", &mut schemas);
    add_schema::<sift_protocol::ChangePasswordRequest>("ChangePasswordRequest", &mut schemas);
    add_schema::<sift_protocol::PasswordResetRequest>("PasswordResetRequest", &mut schemas);
    add_schema::<sift_protocol::IssuedPasswordResetResponse>(
        "IssuedPasswordResetResponse",
        &mut schemas,
    );
    add_schema::<sift_protocol::SshProxyCapabilityClaims>("SshProxyCapabilityClaims", &mut schemas);
    add_schema::<sift_protocol::CreateGithubAllowlistRequest>(
        "CreateGithubAllowlistRequest",
        &mut schemas,
    );
    add_schema::<sift_protocol::GithubNativeAuthStartResponse>(
        "GithubNativeAuthStartResponse",
        &mut schemas,
    );
    add_schema::<sift_protocol::GithubNativeAuthExchangeRequest>(
        "GithubNativeAuthExchangeRequest",
        &mut schemas,
    );
    add_schema::<sift_protocol::AdminCreatePasswordPrincipalRequest>(
        "AdminCreatePasswordPrincipalRequest",
        &mut schemas,
    );
    add_schema::<sift_protocol::AdminSetPrincipalDisabledRequest>(
        "AdminSetPrincipalDisabledRequest",
        &mut schemas,
    );
    add_schema::<sift_protocol::AdminLinkPasswordIdentityRequest>(
        "AdminLinkPasswordIdentityRequest",
        &mut schemas,
    );
    add_schema::<sift_protocol::AuthIdentitySummary>("AuthIdentitySummary", &mut schemas);
    add_schema::<sift_protocol::AuthSessionSummary>("AuthSessionSummary", &mut schemas);
    add_schema::<sift_protocol::AuthPrincipal>("AuthPrincipal", &mut schemas);
    add_schema::<sift_protocol::CreateTenantInvitationRequest>(
        "CreateTenantInvitationRequest",
        &mut schemas,
    );
    add_schema::<sift_protocol::AcceptTenantInvitationRequest>(
        "AcceptTenantInvitationRequest",
        &mut schemas,
    );
    add_schema::<sift_protocol::IssuedTenantInvitationResponse>(
        "IssuedTenantInvitationResponse",
        &mut schemas,
    );
    add_schema::<sift_protocol::RegisterPrincipalKeyRequest>(
        "RegisterPrincipalKeyRequest",
        &mut schemas,
    );
    add_schema::<sift_protocol::KeyChallengeRequest>("KeyChallengeRequest", &mut schemas);
    add_schema::<sift_protocol::KeyChallengeResponse>("KeyChallengeResponse", &mut schemas);
    add_schema::<sift_protocol::KeyAuthenticateRequest>("KeyAuthenticateRequest", &mut schemas);
    add_schema::<sift_protocol::RefreshAuthRequest>("RefreshAuthRequest", &mut schemas);
    add_schema::<sift_protocol::AuthTokensResponse>("AuthTokensResponse", &mut schemas);
    add_schema::<sift_protocol::WebAuthResponse>("WebAuthResponse", &mut schemas);
    add_schema::<sift_protocol::WhoAmIResponse>("WhoAmIResponse", &mut schemas);
    add_schema::<sift_protocol::BeginTransactionRequest>("BeginTransactionRequest", &mut schemas);
    add_schema::<sift_protocol::BulkInsertRequest>("BulkInsertRequest", &mut schemas);
    add_schema::<sift_protocol::BulkInsertResponse>("BulkInsertResponse", &mut schemas);
    add_schema::<sift_protocol::CancelRequest>("CancelRequest", &mut schemas);
    add_schema::<sift_protocol::ConnectionInfo>("ConnectionInfo", &mut schemas);
    add_schema::<sift_protocol::CsvImportRequest>("CsvImportRequest", &mut schemas);
    add_schema::<sift_protocol::CsvImportResponse>("CsvImportResponse", &mut schemas);
    add_schema::<sift_protocol::PreviewEditsRequest>("PreviewEditsRequest", &mut schemas);
    add_schema::<sift_protocol::EditPlan>("EditPlan", &mut schemas);
    add_schema::<sift_protocol::ApplyEditsRequest>("ApplyEditsRequest", &mut schemas);
    add_schema::<sift_protocol::ApplyEditsResult>("ApplyEditsResult", &mut schemas);
    add_schema::<sift_protocol::SchemaSearchRequest>("SchemaSearchRequest", &mut schemas);
    add_schema::<sift_protocol::SchemaSearchResponse>("SchemaSearchResponse", &mut schemas);
    add_schema::<sift_protocol::DataSearchRequest>("DataSearchRequest", &mut schemas);
    add_schema::<sift_protocol::DataSearchResponse>("DataSearchResponse", &mut schemas);
    add_schema::<sift_protocol::ExplainRequest>("ExplainRequest", &mut schemas);
    add_schema::<sift_protocol::ExplainResponse>("ExplainResponse", &mut schemas);
    add_schema::<sift_protocol::DatabaseProcess>("DatabaseProcess", &mut schemas);
    add_schema::<sift_protocol::EndTransactionRequest>("EndTransactionRequest", &mut schemas);
    add_schema::<sift_protocol::ExecuteRequestHttp>("ExecuteRequestHttp", &mut schemas);
    add_schema::<sift_protocol::ExecuteResponse>("ExecuteResponse", &mut schemas);
    add_schema::<sift_protocol::Health>("Health", &mut schemas);
    add_schema::<sift_protocol::KillProcessRequest>("KillProcessRequest", &mut schemas);
    add_schema::<sift_protocol::KillProcessResponse>("KillProcessResponse", &mut schemas);
    add_schema::<sift_protocol::OpenConnectionRequest>("OpenConnectionRequest", &mut schemas);
    add_schema::<sift_protocol::OpenSessionRequest>("OpenSessionRequest", &mut schemas);
    add_schema::<sift_protocol::OperationAuditEntry>("OperationAuditEntry", &mut schemas);
    add_schema::<sift_protocol::OperationCapability>("OperationCapability", &mut schemas);
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
    add_schema::<sift_protocol::TransactionState>("TransactionState", &mut schemas);
    add_schema::<sift_protocol::TransactionPreviewRequest>(
        "TransactionPreviewRequest",
        &mut schemas,
    );
    add_schema::<sift_protocol::TransactionPreview>("TransactionPreview", &mut schemas);
    add_schema::<sift_protocol::WsClientMessage>("WsClientMessage", &mut schemas);
    add_schema::<sift_protocol::WsServerMessage>("WsServerMessage", &mut schemas);
    add_schema::<sift_protocol::RoomClientMessage>("RoomClientMessage", &mut schemas);
    add_schema::<sift_protocol::RoomServerMessage>("RoomServerMessage", &mut schemas);
    add_schema::<sift_metadata::ApiTokenRow>("ApiTokenRow", &mut schemas);
    add_schema::<sift_metadata::GithubAllowlistEntry>("GithubAllowlistEntry", &mut schemas);
    add_schema::<sift_metadata::ConnectionProfile>("ConnectionProfile", &mut schemas);
    add_schema::<sift_metadata::Document>("Document", &mut schemas);
    add_schema::<sift_metadata::OperationAudit>("OperationAudit", &mut schemas);
    add_schema::<sift_metadata::QueryHistory>("QueryHistory", &mut schemas);
    add_schema::<sift_metadata::Room>("Room", &mut schemas);
    add_schema::<sift_metadata::RoomMember>("RoomMember", &mut schemas);
    add_schema::<sift_metadata::TenantMembership>("TenantMembership", &mut schemas);
    add_schema::<sift_metadata::TenantInvitation>("TenantInvitation", &mut schemas);
    add_schema::<sift_metadata::PrincipalKey>("PrincipalKey", &mut schemas);
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
    let audit = metadata_audit_record(
        auth.principal_id,
        "delete",
        "connection_profile",
        Some(profile.0),
    );
    metadata
        .delete_connection_profile(tenant, profile, audit)
        .await?;
    push_metadata_operation_local(
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
    let audit = metadata_audit_record(
        auth.principal_id,
        "set_credential",
        "connection_profile",
        Some(profile_id.0),
    );
    metadata
        .set_per_user_credential(profile_id, auth.principal_id, req.secret.as_bytes(), audit)
        .await?;
    push_metadata_operation_local(
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
    let audit = metadata_audit_record(auth.principal_id, "revoke", "api_token", Some(token_id.0));
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
        metadata.revoke_api_token(token_id, audit)?;
        Ok(())
    })
    .await?;
    push_metadata_operation_local(
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
    let tenant_role = auth
        .tenants
        .iter()
        .find(|membership| membership.tenant.id == tenant)
        .map(|membership| sift_protocol::TenantRole::from(&membership.role))
        .ok_or_else(|| ApiError::Forbidden("tenant membership required".into()))?;
    let authorization = crate::authorization::AuthorizationScope {
        authenticated: true,
        trusted_local: state.auth.deployment == DeploymentPolicy::Personal
            && state.auth.transport == Transport::Loopback,
        instance_admin: false,
        tenant_role: Some(tenant_role),
        room_role: None,
        connection_policy: Some(profile.policy.clone()),
    };
    crate::authorization::authorize(&authorization, sift_protocol::OperationKind::OpenConnection)
        .map_err(|denial| ApiError::Forbidden(denial.public_reason().into()))?;
    let spec = metadata
        .resolve_connection_spec(tenant, auth.principal_id, profile_id)
        .await?;
    let info = state
        .sessions
        .open_managed_connection(
            session_id,
            profile.engine,
            spec,
            auth.principal_id,
            tenant,
            profile_id,
            profile.policy.revision,
        )
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
    auth: Option<Extension<AuthContext>>,
) -> ApiResult<Json<Vec<sift_protocol::SessionInfo>>> {
    let sessions = state
        .sessions
        .list_sessions_for_owner(auth.map(|Extension(auth)| auth.principal_id));
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
    if state.auth.deployment != DeploymentPolicy::Personal
        || state.auth.transport != Transport::Loopback
    {
        return Err(ApiError::Forbidden(
            "raw connection specifications are available only in personal-loopback mode".into(),
        ));
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
    let response = finish_operation(
        &state.sessions,
        operation,
        state.sessions.bulk_insert(id, conn_id, req).await,
        |response| Some(response.rows_inserted as i64),
    )?;
    Ok(Json(response))
}

async fn import_csv(
    State(state): State<AppState>,
    Path((session, connection)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Json(req): Json<CsvImportRequest>,
) -> ApiResult<Json<sift_protocol::CsvImportResponse>> {
    let operation = Operation::ImportCsv {
        session,
        connection,
        table: req.table.clone(),
        create_table: req.create_table,
        conflict_policy: req.conflict_policy,
    };
    let response = finish_operation(
        &state.sessions,
        operation,
        crate::csv_import::import(&state.sessions, session, connection, req).await,
        |response| Some(response.rows_inserted as i64),
    )?;
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
    let format = req.format;
    let operation = Operation::ExportQuery {
        session: id,
        connection: conn_id,
    };
    // Routes through the cursor registry (per-session cap + pump), unlike
    // the previous direct driver.execute call. See `export_stream`.
    let stream = finish_operation(
        &state.sessions,
        operation,
        state.sessions.export_stream(id, conn_id, req).await,
        |_| None,
    )?;
    let content_type = crate::export::content_type(format);
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
    let ddl = finish_operation(
        &state.sessions,
        Operation::GenerateDdl {
            session: id,
            connection: conn_id,
        },
        state.sessions.ddl_for(id, conn_id, path).await,
        |_| None,
    )?;
    Ok(Json(ddl))
}

async fn post_completion(
    State(state): State<AppState>,
    Path((id, conn_id)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Json(req): Json<sift_protocol::completion::CompletionRequest>,
) -> ApiResult<Json<sift_protocol::completion::CompletionResponse>> {
    let resp = finish_operation(
        &state.sessions,
        Operation::Complete {
            session: id,
            connection: conn_id,
            request: req.clone(),
        },
        state.sessions.complete(id, conn_id, req).await,
        |_| None,
    )?;
    Ok(Json(resp))
}

async fn post_edits_preview(
    State(state): State<AppState>,
    Path((id, conn_id)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Json(req): Json<sift_protocol::PreviewEditsRequest>,
) -> ApiResult<Json<sift_protocol::EditPlan>> {
    let result = async {
        if req.connection != conn_id {
            return Err(ApiError::BadRequest(
                "`connection` in body must match the path connection".into(),
            ));
        }
        state
            .sessions
            .preview_edits(id, conn_id, req.edit_set)
            .await
    }
    .await;
    let plan = finish_operation(
        &state.sessions,
        Operation::PreviewEdits {
            session: id,
            connection: conn_id,
        },
        result,
        |_| None,
    )?;
    Ok(Json(plan))
}

async fn post_edits_apply(
    State(state): State<AppState>,
    Path((id, conn_id)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Json(mut req): Json<sift_protocol::ApplyEditsRequest>,
) -> ApiResult<Json<sift_protocol::ApplyEditsResult>> {
    let apply_result = async {
        if req.connection != conn_id {
            return Err(ApiError::BadRequest(
                "`connection` in body must match the path connection".into(),
            ));
        }
        req.connection = conn_id;
        state.sessions.apply_edits(id, req).await
    }
    .await;
    let result = finish_operation(
        &state.sessions,
        Operation::ApplyEdits {
            session: id,
            connection: conn_id,
        },
        apply_result,
        |result| Some(result.applied.len() as i64),
    )?;
    Ok(Json(result))
}

async fn post_search_schema(
    State(state): State<AppState>,
    Path((id, conn_id)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Json(req): Json<sift_protocol::SchemaSearchRequest>,
) -> ApiResult<Json<sift_protocol::SchemaSearchResponse>> {
    let resp = finish_operation(
        &state.sessions,
        Operation::SearchSchema {
            session: id,
            connection: conn_id,
        },
        state.sessions.search_schema(id, conn_id, req).await,
        |response| Some(response.hits.len() as i64),
    )?;
    Ok(Json(resp))
}

async fn post_search_data(
    State(state): State<AppState>,
    Path((id, conn_id)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Json(req): Json<sift_protocol::DataSearchRequest>,
) -> ApiResult<Json<sift_protocol::DataSearchResponse>> {
    let resp = finish_operation(
        &state.sessions,
        Operation::SearchData {
            session: id,
            connection: conn_id,
        },
        state.sessions.search_data(id, conn_id, req).await,
        |response| Some(response.hits.len() as i64),
    )?;
    Ok(Json(resp))
}

async fn post_explain(
    State(state): State<AppState>,
    Path((id, conn_id)): Path<(sift_protocol::SessionId, sift_protocol::ConnectionId)>,
    Json(req): Json<sift_protocol::ExplainRequest>,
) -> ApiResult<Json<sift_protocol::ExplainResponse>> {
    let result = async {
        if req.connection != conn_id {
            return Err(ApiError::BadRequest(
                "`connection` in body must match the path connection".into(),
            ));
        }
        crate::plan::explain(&state.sessions, id, conn_id, &req).await
    }
    .await;
    let resp = finish_operation(
        &state.sessions,
        Operation::Explain {
            session: id,
            connection: conn_id,
        },
        result,
        |_| None,
    )?;
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
    let tx = finish_operation(
        &state.sessions,
        operation,
        state.sessions.begin_transaction(id, req).await,
        |_| None,
    )?;
    Ok(Json(tx))
}

async fn list_processes(
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

async fn kill_process(
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

async fn list_transactions(
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

async fn preview_transaction(
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

async fn commit_transaction(
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

async fn rollback_transaction(
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

async fn create_savepoint(
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

async fn rollback_to_savepoint(
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

async fn release_savepoint(
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
    let registry = state.sessions.cursor_registry().clone();
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
    let (pages, done) =
        tokio::task::spawn_blocking(move || registry.read_spill_pages(cursor_id, limit))
            .await
            .map_err(|e| ApiError::Internal(format!("spill read task failed: {e}")))?
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
    auth: Option<Extension<AuthContext>>,
    Path(session_id): Path<sift_protocol::SessionId>,
    ws: WebSocketUpgrade,
) -> Response {
    // Capture the correlation ID from the upgrade request so the (detached)
    // socket task's per-message operations are audited under the same ID.
    let correlation_id = crate::correlation::current().unwrap_or_else(crate::correlation::generate);
    ws.on_upgrade(move |socket| {
        crate::correlation::scope(correlation_id, async move {
            if let Err(error) =
                handle_ws(state, auth.map(|Extension(auth)| auth), session_id, socket).await
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
    mut auth: AuthContext,
    room: RoomId,
    socket: WebSocket,
) -> ApiResult<()> {
    let (mut sender, mut receiver) = socket.split();
    let mut events = state.rooms.subscribe(room.0);
    let mut attachment_id = None;
    let mut lease_tick = tokio::time::interval(std::time::Duration::from_secs(1));

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
                    RoomClientMessage::Reauthenticate { access_token } => {
                        let replacement = reauthenticate_ws(&state, &access_token.0, auth.principal_id).await?;
                        let expires_at = replacement.access_expires_at.ok_or(ApiError::Unauthorized)?;
                        auth = replacement;
                        send_json(&mut sender, &RoomServerMessage::Authenticated { expires_at }).await?;
                    }
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
            _ = lease_tick.tick() => {
                if !ws_lease_is_valid(&state, &auth, Some(room))? {
                    send_json(&mut sender, &RoomServerMessage::Error {
                        message: "authentication lease or room membership was revoked".into(),
                    }).await?;
                    break;
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

fn ws_lease_is_valid(
    state: &AppState,
    auth: &AuthContext,
    room: Option<RoomId>,
) -> ApiResult<bool> {
    if auth
        .access_expires_at
        .is_some_and(|expires| expires <= chrono::Utc::now())
    {
        return Ok(false);
    }
    let Some(metadata) = state.metadata.as_ref() else {
        return Ok(true);
    };
    if let Some(session_id) = auth.auth_session_id.as_deref() {
        if !metadata.auth_session_is_active(session_id)? {
            return Ok(false);
        }
    }
    if let Some(room) = room {
        return Ok(metadata.get_room_member(room, auth.principal_id)?.is_some());
    }
    Ok(true)
}

async fn reauthenticate_ws(
    state: &AppState,
    token: &str,
    expected_principal: PrincipalId,
) -> ApiResult<AuthContext> {
    let metadata = metadata_store(state)?;
    let session = state
        .auth
        .runtime
        .resolve_access_token(metadata, token)
        .await?
        .ok_or(ApiError::Unauthorized)?;
    if session.principal.id != expected_principal {
        return Err(ApiError::Forbidden(
            "WebSocket reauthentication cannot change principal".into(),
        ));
    }
    Ok(AuthContext {
        principal_id: session.principal.id,
        tenants: metadata.list_principal_tenants(session.principal.id)?,
        auth_session_id: Some(session.session_id),
        cookie_authenticated: false,
        access_expires_at: Some(session.expires_at),
    })
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
    state: AppState,
    mut auth: Option<AuthContext>,
    session_id: sift_protocol::SessionId,
    socket: WebSocket,
) -> ApiResult<()> {
    let (mut sender, mut receiver) = socket.split();
    let mut lease_tick = tokio::time::interval(std::time::Duration::from_secs(1));
    loop {
        let message = tokio::select! {
            message = receiver.next() => match message {
                Some(message) => message,
                None => break,
            },
            _ = lease_tick.tick(), if auth.is_some() => {
                if !ws_lease_is_valid(&state, auth.as_ref().expect("guarded"), None)? {
                    send_json(&mut sender, &WsServerMessage::Error {
                        request_id: None,
                        code: None,
                        retry_after_ms: None,
                        message: "authentication lease expired or was revoked".into(),
                    }).await?;
                    break;
                }
                continue;
            }
        };
        let message = message.map_err(|e| ApiError::BadRequest(e.to_string()))?;
        match message {
            Message::Text(text) => {
                let msg: WsClientMessage =
                    serde_json::from_str(&text).map_err(|e| ApiError::BadRequest(e.to_string()))?;
                match msg {
                    WsClientMessage::Reauthenticate { access_token } => {
                        let current = auth.as_ref().ok_or(ApiError::Unauthorized)?;
                        let replacement =
                            reauthenticate_ws(&state, &access_token.0, current.principal_id)
                                .await?;
                        let expires_at = replacement
                            .access_expires_at
                            .ok_or(ApiError::Unauthorized)?;
                        auth = Some(replacement);
                        send_json(&mut sender, &WsServerMessage::Authenticated { expires_at })
                            .await?;
                    }
                    WsClientMessage::Execute {
                        request_id,
                        connection,
                        sql,
                        params,
                        tx,
                    } => {
                        // Track the streaming query against the drain gate for
                        // its whole lifetime (execute + paging).
                        let _query_guard = state.shutdown.track_query();
                        let stream = match state
                            .sessions
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
                                        code: None,
                                        retry_after_ms: None,
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
                            &state.sessions,
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
                        let stream = match state
                            .sessions
                            .listen_pg(session_id, connection, channels)
                            .await
                        {
                            Ok(stream) => stream,
                            Err(error) => {
                                send_json(
                                    &mut sender,
                                    &WsServerMessage::Error {
                                        request_id: Some(request_id),
                                        code: None,
                                        retry_after_ms: None,
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
                    } => {
                        state
                            .sessions
                            .cancel(session_id, connection, cursor_id)
                            .await?
                    }
                    WsClientMessage::Ack { .. } => {
                        send_json(
                            &mut sender,
                            &WsServerMessage::Error {
                                request_id: None,
                                code: None,
                                retry_after_ms: None,
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
                WsClientMessage::Reauthenticate { .. } => {
                    return Err(ApiError::BadRequest(
                        "reauthenticate before starting a result stream".into(),
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
    let bytes = serde_json::to_vec(value).map_err(|e| ApiError::Internal(e.to_string()))?;
    sender
        .send(Message::Binary(bytes))
        .await
        .map_err(|e| ApiError::BadRequest(e.to_string()))
}

#[cfg(test)]
mod route_access_tests {
    use super::*;

    #[test]
    fn classifies_public_authenticated_and_owned_route_families() {
        assert_eq!(route_access("/v1/health"), RouteAccess::Public);
        assert_eq!(
            route_access("/v1/metadata/rooms/1"),
            RouteAccess::Authenticated
        );
        assert_eq!(
            route_access("/v1/sessions/42/connections/7/schema"),
            RouteAccess::Session(sift_protocol::SessionId(42))
        );
        assert_eq!(
            route_access("/v1/cursors/9/pages"),
            RouteAccess::Cursor(sift_protocol::CursorId(9))
        );
        assert_eq!(
            route_access("/v1/sessions/not-a-number"),
            RouteAccess::Authenticated
        );
    }
}
