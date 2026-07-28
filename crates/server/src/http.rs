//! axum router + handlers. Routes versioned under `/v1`. The `AppState`
//! carries the `SessionStore` (which in turn carries the `DriverRegistry`).

use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Extension, Path, Query, State};
use axum::http::{header, header::HeaderName, HeaderMap, HeaderValue, Request, StatusCode};
use axum::middleware::{from_fn, from_fn_with_state, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Json, Router};
use futures::{SinkExt, StreamExt};
use schemars::{schema_for, JsonSchema};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use std::time::Instant;

use aide::axum::routing::{delete_with, get_with, post_with, put_with};
use aide::axum::ApiRouter;
use aide::openapi::OpenApi;
use aide::transform::TransformOperation;

use sift_metadata::{
    ApiTokenId, AuthClientKind as MetadataAuthClientKind, AuthIdentityId, ConnectionProfileId,
    Document, DocumentId, GithubAllowlistId, GithubProfile, MetadataStore, NewConnectionProfile,
    NewDocument, NewOperationAudit, NewQueryHistory, NewRoom, NewSavedQuery, PrincipalId,
    PrincipalKeyId, QueryHistory, QueryStatus, RefreshAuthResult, Room, RoomId, RoomMember,
    RoomRole, SavedQuery, SavedQueryFilter, SavedQueryId, SavedQueryScope, TenantId,
    TenantInvitationId, TenantMembership, UpdateSavedQuery,
};
use sift_protocol::{
    AcceptTenantInvitationRequest, AdminCreatePasswordPrincipalRequest,
    AdminLinkPasswordIdentityRequest, AdminSetPrincipalDisabledRequest, AuditEntry, AuthClientKind,
    AuthIdentitySummary, AuthPrincipal, AuthSessionSummary, AuthTenantMembership,
    AuthTokensResponse, BeginTransactionRequest, BulkInsertRequest, CancelRequest,
    ChangePasswordRequest, CreateGithubAllowlistRequest, CreateTenantInvitationRequest,
    CsvImportRequest, EndTransactionRequest, ExecuteRequest, ExecuteRequestHttp,
    GithubNativeAuthExchangeRequest, GithubNativeAuthStartResponse, HandshakeDeployment,
    HandshakeRequest, HandshakeResponse, HandshakeRuntimeMode, HandshakeTransport, Health,
    InvitationRole, IssuedPasswordResetResponse, IssuedTenantInvitationResponse,
    KeyAuthenticateRequest, KeyChallengeRequest, KeyChallengeResponse, KillProcessRequest,
    ObjectPath, OpenConnectionRequest, OpenSessionRequest, Operation, OperationStatus,
    PasswordLoginRequest, PasswordResetRequest, ProtocolRange, Readiness, RefreshAuthRequest,
    RegisterPrincipalKeyRequest, RoomClientMessage, RoomQueryResult, RoomServerMessage,
    SavepointRequest, SchemaFilter, SchemaScope, SshProxyAccessGrant,
    SshProxyCapabilityExchangeRequest, TransactionPreviewRequest, UpdateConnectionPolicyRequest,
    UpdateTenantLimitsRequest, WebAuthResponse, WhoAmIResponse, WsClientMessage, WsServerMessage,
    PROTOCOL_VERSION, PROTOCOL_VERSION_NUMBER,
};

use crate::config::{DeploymentPolicy, RuntimeMode, Transport};
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
    pub runtime_mode: RuntimeMode,
    pub runtime: crate::identity::AuthRuntime,
    pub github: Option<crate::identity::GithubOAuthConfig>,
    pub instance_audience: String,
    pub instance_id: String,
    pub daemon_generation: String,
    /// Transitional test/embed escape hatch. The production binary always
    /// requires a negotiated version on product routes.
    pub allow_legacy_unversioned: bool,
    pub rate_limiter: crate::rate_limit::RateLimiter,
}

impl Default for AuthState {
    fn default() -> Self {
        Self {
            bearer_token: None,
            loopback_bypass: true,
            deployment: DeploymentPolicy::Personal,
            transport: Transport::Loopback,
            runtime_mode: RuntimeMode::InProcess,
            runtime: crate::identity::AuthRuntime::default(),
            github: None,
            instance_audience: "sift:local".into(),
            instance_id: uuid::Uuid::new_v4().to_string(),
            daemon_generation: uuid::Uuid::new_v4().to_string(),
            allow_legacy_unversioned: true,
            rate_limiter: crate::rate_limit::RateLimiter::default(),
        }
    }
}

pub fn app(state: AppState) -> Router {
    if let Some(metadata) = &state.metadata {
        state.sessions.set_authorization_store(metadata.clone());
    }
    let router = ApiRouter::new()
        .api_route(
            "/v1/handshake",
            post_with(
                handshake,
                doc("handshake", "Negotiate the application protocol version"),
            ),
        )
        .api_route(
            "/v1/health",
            get_with(health, doc("health", "Liveness and registered engines")),
        )
        .api_route(
            "/v1/ready",
            get_with(ready, |op| {
                doc(
                    "ready",
                    "Readiness: 200 when ready, 503 while draining/unhealthy",
                )(op)
                .response::<200, Json<sift_protocol::Readiness>>()
                .response::<503, Json<sift_protocol::Readiness>>()
            }),
        )
        .api_route(
            "/v1/audit",
            get_with(list_audit, doc("listAudit", "List in-memory operation audit rows")),
        )
        .api_route(
            "/v1/operations",
            get_with(list_operations, doc("listOperations", "List replayable operation audit rows")),
        )
        .api_route(
            "/v1/operations/available",
            get_with(list_available_operations, doc("listAvailableOperations", "List contextual operation capabilities")),
        )
        .api_route(
            "/v1/operations/audit",
            get_with(list_operation_audit_log, doc("listOperationAudit", "List durable operation audit rows (actor, target, result, rows)")),
        )
        .api_route(
            "/v1/providers",
            get_with(list_providers, doc("listProviders", "List provider-neutral database capabilities")),
        )
        .api_route(
            "/v1/extensions",
            get_with(list_extensions, doc("listExtensions", "List installed extension descriptors")),
        )
        .api_route(
            "/v1/extensions/:publisher/:name",
            get_with(get_extension, doc("getExtension", "Inspect one installed extension"))
                .delete_with(uninstall_extension, doc("uninstallExtension", "Uninstall an extension while retaining orphaned data")),
        )
        .api_route(
            "/v1/extensions/validate",
            post_with(validate_extension, doc("validateExtension", "Validate a bounded local extension archive")),
        )
        .api_route(
            "/v1/extensions/install",
            post_with(install_extension, doc("installExtension", "Validate and install a bounded local extension archive")),
        )
        .api_route(
            "/v1/extensions/:publisher/:name/selection",
            put_with(update_extension_selection, doc("updateExtensionSelection", "Enable or disable an extension with revision checking")),
        )
        .api_route(
            "/v1/extensions/:publisher/:name/grants",
            put_with(update_extension_grants, doc("updateExtensionGrants", "Replace extension grants with revision checking")),
        )
        .api_route(
            "/v1/extensions/:publisher/:name/tenants/:tenant_id",
            put_with(update_extension_tenant, doc("updateExtensionTenant", "Allow or deny an extension for one tenant")),
        )
        .api_route(
            "/v1/extensions/:publisher/:name/rollback",
            post_with(rollback_extension, doc("rollbackExtension", "Select the previous installed package")),
        )
        .api_route(
            "/v1/extensions/:publisher/:name/purge",
            post_with(purge_extension, doc("purgeExtension", "Purge orphaned extension data")),
        )
        .api_route(
            "/v1/extensions/:publisher/:name/diagnostics",
            get_with(extension_diagnostics, doc("extensionDiagnostics", "Inspect bounded extension lifecycle diagnostics")),
        )
        .api_route(
            "/v1/extension-actions/invoke",
            post_with(invoke_extension_action, doc("invokeExtensionAction", "Invoke a schema-validated governed extension action")),
        )
        .api_route(
            "/v1/operation-approvals",
            post_with(create_operation_approval, doc("createOperationApproval", "Create a narrowly bound one-use approval request")),
        )
        .api_route(
            "/v1/operation-approvals/:approval_id/approve",
            post_with(approve_operation, doc("approveOperation", "Approve a pending operation request")),
        )
        .api_route(
            "/v1/tools",
            get_with(list_governed_tools, doc("listGovernedTools", "List governed tools available in context")),
        )
        .api_route(
            "/v1/tools/invoke",
            post_with(invoke_governed_tool, doc("invokeGovernedTool", "Invoke a governed tool through policy and approval admission")),
        )
        .api_route(
            "/v1/openapi.json",
            get_with(openapi, doc("openapi", "OpenAPI document")),
        )
        .api_route(
            "/v1/auth/login",
            post_with(password_login, doc("passwordLogin", "Authenticate an instance-owned password identity")),
        )
        .api_route(
            "/v1/auth/refresh",
            post_with(refresh_auth, doc("refreshAuth", "Atomically rotate an interactive refresh credential")),
        )
        .api_route(
            "/v1/auth/logout",
            post_with(logout_auth, doc("logoutAuth", "Revoke the current interactive auth session")),
        )
        .api_route(
            "/v1/auth/logout-all",
            post_with(logout_all_auth, doc("logoutAllAuth", "Revoke every interactive auth session for the principal")),
        )
        .api_route(
            "/v1/auth/whoami",
            get_with(whoami, doc("whoAmI", "Return the authenticated principal and memberships")),
        )
        .api_route(
            "/v1/auth/password",
            put_with(change_password, doc("changePassword", "Replace the current principal password and revoke interactive sessions")),
        )
        .api_route(
            "/v1/auth/password/reset",
            post_with(reset_password, doc("resetPassword", "Consume an administrator-issued one-use password reset token")),
        )
        .api_route(
            "/v1/auth/github/start",
            get_with(github_start, doc("githubAuthStart", "Start the instance GitHub OAuth flow with state and S256 PKCE")),
        )
        .api_route(
            "/v1/auth/github/callback",
            get_with(github_callback, doc("githubAuthCallback", "Complete GitHub OAuth, enforce the allowlist, and set browser cookies")),
        )
        .api_route(
            "/v1/auth/github/exchange",
            post_with(github_native_exchange, doc("githubNativeAuthExchange", "Exchange a completed one-use native GitHub handoff for Sift tokens")),
        )
        .api_route(
            "/v1/admin/auth/github-allowlist",
            get_with(list_github_allowlist, doc("listGithubAllowlist", "List GitHub allowlist entries (instance admin)")).post_with(create_github_allowlist, doc("createGithubAllowlist", "Allow a GitHub login, optionally linked to an existing principal")),
        )
        .api_route(
            "/v1/admin/auth/github-allowlist/:id",
            delete_with(revoke_github_allowlist, doc("revokeGithubAllowlist", "Revoke a pending GitHub allowlist entry")),
        )
        .api_route(
            "/v1/admin/principals",
            post_with(admin_create_principal, doc("adminCreatePasswordPrincipal", "")),
        )
        .api_route(
            "/v1/admin/principals/:id/disabled",
            put_with(admin_set_principal_disabled, doc("adminSetPrincipalDisabled", "")),
        )
        .api_route(
            "/v1/admin/principals/:id/identities",
            get_with(admin_list_principal_identities, doc("adminListPrincipalIdentities", "")),
        )
        .api_route(
            "/v1/admin/principals/:id/identities/password",
            post_with(admin_link_password_identity, doc("adminLinkPasswordIdentity", "")),
        )
        .api_route(
            "/v1/admin/principals/:principal_id/identities/:identity_id",
            delete_with(admin_unlink_identity, doc("adminUnlinkIdentity", "")),
        )
        .api_route(
            "/v1/admin/principals/:id/auth-sessions",
            get_with(admin_list_auth_sessions, doc("adminListAuthSessions", "")),
        )
        .api_route(
            "/v1/admin/principals/:principal_id/auth-sessions/:session_id",
            delete_with(admin_revoke_auth_session, doc("adminRevokeAuthSession", "")),
        )
        .api_route(
            "/v1/admin/principals/:principal_id/identities/:identity_id/password-reset",
            post_with(admin_issue_password_reset, doc("adminIssuePasswordReset", "")),
        )
        .api_route(
            "/v1/metadata/tenants/:id/invitations",
            get_with(list_tenant_invitations, doc("listTenantInvitations", "")).post_with(create_tenant_invitation, doc("createTenantInvitation", "")),
        )
        .api_route(
            "/v1/metadata/tenants/:tenant_id/invitations/:id",
            delete_with(revoke_tenant_invitation, doc("revokeTenantInvitation", "Revoke an unconsumed tenant invitation")),
        )
        .api_route(
            "/v1/auth/invitations/accept",
            post_with(accept_tenant_invitation, doc("acceptTenantInvitation", "")),
        )
        .api_route(
            "/v1/auth/keys",
            get_with(list_principal_keys, doc("listPrincipalKeys", "")).post_with(register_principal_key, doc("registerPrincipalKey", "")),
        )
        .api_route(
            "/v1/auth/keys/:id",
            delete_with(revoke_principal_key, doc("revokePrincipalKey", "Revoke a registered principal key")),
        )
        .api_route(
            "/v1/auth/keys/challenge",
            post_with(issue_key_challenge, doc("issueKeyChallenge", "")),
        )
        .api_route(
            "/v1/auth/keys/authenticate",
            post_with(authenticate_key, doc("authenticateKey", "")),
        )
        .api_route(
            "/v1/auth/ssh-proxy/exchange",
            post_with(
                exchange_ssh_proxy_capability,
                doc(
                    "exchangeSshProxyCapability",
                    "Atomically exchange a one-use SSH bootstrap capability",
                ),
            ),
        )
        .api_route(
            "/v1/metadata/tenants",
            get_with(list_metadata_tenants, doc("listMetadataTenants", "List current principal tenant memberships")),
        )
        .api_route(
            "/v1/metadata/rooms",
            get_with(list_metadata_rooms, doc("listMetadataRooms", "List rooms for current principal in a tenant")).post_with(create_metadata_room, doc("createMetadataRoom", "Create room")),
        )
        .api_route(
            "/v1/metadata/rooms/:id",
            delete_with(delete_metadata_room, doc("deleteMetadataRoom", "Delete room")),
        )
        .api_route(
            "/v1/metadata/rooms/:id/members",
            get_with(list_metadata_room_members, doc("listMetadataRoomMembers", "List room members")).post_with(add_metadata_room_member, doc("addMetadataRoomMember", "Add or update room member")),
        )
        .api_route(
            "/v1/metadata/rooms/:id/members/:principal_id",
            delete_with(remove_metadata_room_member, doc("removeMetadataRoomMember", "Remove room member")),
        )
        .api_route(
            "/v1/metadata/rooms/:id/connection",
            put_with(bind_metadata_room_connection, doc("bindMetadataRoomConnection", "Bind a connection profile to a room"))
                .delete_with(unbind_metadata_room_connection, doc("unbindMetadataRoomConnection", "Unbind the room's connection")),
        )
        .api_route(
            "/v1/metadata/rooms/:id/join",
            post_with(join_metadata_room, doc("joinMetadataRoom", "Join room as current principal")),
        )
        .api_route(
            "/v1/metadata/rooms/:id/leave",
            post_with(leave_metadata_room, doc("leaveMetadataRoom", "Leave room as current principal")),
        )
        .api_route(
            "/v1/metadata/rooms/:id/ws",
            get_with(ws_room, doc("roomWebSocket", "WebSocket room presence and document operations; protocol uses RoomClientMessage/RoomServerMessage")),
        )
        .api_route(
            "/v1/metadata/rooms/:id/results",
            get_with(list_room_results, doc("listRoomResults", "List transient shared results visible to current room members")),
        )
        .api_route(
            "/v1/metadata/rooms/:id/results/:result_id",
            get_with(get_room_result, doc("getRoomResult", "Get one transient shared-result reference")),
        )
        .api_route(
            "/v1/metadata/rooms/:id/results/:result_id/pages",
            get_with(get_room_result_pages, doc("getRoomResultPages", "Independently page a transient shared result; query params: from_seq and limit")),
        )
        .api_route(
            "/v1/metadata/rooms/:id/documents",
            get_with(list_metadata_documents, doc("listMetadataDocuments", "List room documents")).post_with(create_metadata_document, doc("createMetadataDocument", "Create room document")),
        )
        .api_route(
            "/v1/metadata/documents/:id",
            put_with(update_metadata_document, doc("updateMetadataDocument", "Update document CRDT snapshot")).delete_with(delete_metadata_document, doc("deleteMetadataDocument", "Delete document")),
        )
        .api_route(
            "/v1/metadata/connections",
            get_with(list_metadata_connections, doc("listMetadataConnectionProfiles", "List connection profiles")).post_with(upsert_metadata_connection, doc("upsertMetadataConnectionProfile", "Create or replace connection profile")),
        )
        .api_route(
            "/v1/metadata/connections/:id",
            delete_with(delete_metadata_connection, doc("deleteMetadataConnectionProfile", "Delete connection profile")),
        )
        .api_route(
            "/v1/metadata/connections/:id/credential",
            post_with(set_metadata_connection_credential, doc("setMetadataConnectionCredential", "Set per-user credential for connection profile")),
        )
        .api_route(
            "/v1/metadata/connections/:id/policy",
            get_with(get_metadata_connection_policy, doc("getMetadataConnectionPolicy", "Read the effective connection profile policy")).put_with(update_metadata_connection_policy, doc("updateMetadataConnectionPolicy", "Replace a connection profile policy with optimistic revision checking")),
        )
        .api_route(
            "/v1/metadata/connections/:id/disconnect",
            post_with(disconnect_metadata_connection, doc("disconnectMetadataConnectionProfile", "Immediately close all active connections using a managed profile")),
        )
        .api_route(
            "/v1/metadata/tenants/:id/usage",
            get_with(get_tenant_usage, doc("getTenantUsage", "Read effective tenant limits and current resource usage")),
        )
        .api_route(
            "/v1/admin/tenants/:id/limits",
            put_with(set_tenant_limits, doc("setTenantLimits", "Set an operator-bounded tenant resource limit override")).delete_with(clear_tenant_limits, doc("clearTenantLimits", "Clear a tenant resource limit override")),
        )
        .api_route(
            "/v1/metadata/history",
            get_with(list_metadata_history, doc("listMetadataHistory", "List query history by room or current principal")),
        )
        .api_route(
            "/v1/metadata/saved-queries",
            get_with(list_metadata_saved_queries, doc("listMetadataSavedQueries", "List visible personal and tenant-shared saved queries")).post_with(create_metadata_saved_query, doc("createMetadataSavedQuery", "Create a personal or tenant-shared saved query")),
        )
        .api_route(
            "/v1/metadata/saved-queries/:id",
            get_with(get_metadata_saved_query, doc("getMetadataSavedQuery", "")).put_with(update_metadata_saved_query, doc("updateMetadataSavedQuery", "")).delete_with(delete_metadata_saved_query, doc("deleteMetadataSavedQuery", "")),
        )
        .api_route(
            "/v1/auth/tokens",
            get_with(list_auth_tokens, doc("listAuthTokens", "List current principal API tokens")).post_with(issue_auth_token, doc("issueAuthToken", "Issue API token; plaintext returned once")),
        )
        .api_route(
            "/v1/auth/tokens/:id",
            delete_with(revoke_auth_token, doc("revokeAuthToken", "Revoke API token")),
        )
        .api_route(
            "/v1/sessions",
            post_with(create_session, doc("createSession", "Create session")).get_with(list_sessions, doc("listSessions", "List sessions")),
        )
        .api_route(
            "/v1/sessions/:id",
            get_with(get_session, doc("getSession", "Get session")).delete_with(close_session, doc("closeSession", "Close session")),
        )
        .api_route(
            "/v1/sessions/:id/connections",
            post_with(open_connection, doc("openConnection", "Open connection")).get_with(list_connections, doc("listConnections", "List connections")),
        )
        .api_route(
            "/v1/sessions/:id/connections/from-profile",
            post_with(open_connection_from_profile, doc("openConnectionFromProfile", "Open session connection from metadata profile")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id",
            delete_with(close_connection, doc("closeConnection", "Close connection")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/ping",
            post_with(ping_connection, doc("pingConnection", "Ping connection")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/bulk-insert",
            post_with(bulk_insert, doc("bulkInsert", "Bulk insert rows into a SQL Server table")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/import/csv",
            post_with(import_csv, doc("importCsv", "Import CSV into a table")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/schema",
            get_with(get_schema, doc("getSchema", "Fetch schema")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/ddl",
            get_with(get_object_ddl, doc("getObjectDdl", "Generate DDL (CREATE statement) for a database object. Query params: `name` (required), `schema`, `kind` (defaults to `table`).")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/complete",
            post_with(post_completion, doc("postCompletion", "Compute ranked autocomplete candidates for a SQL text + cursor position on the connection's engine.")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/export",
            post_with(export_query, doc("exportQuery", "Stream a query result as CSV / TSV / JSON Lines / JSON Array. Response is chunked; Content-Type depends on the requested format.")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/edits/preview",
            post_with(post_edits_preview, doc("previewEdits", "Preview parameterized inline-edit DML")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/edits/apply",
            post_with(post_edits_apply, doc("applyEdits", "Apply inline edits transactionally")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/search/schema",
            post_with(post_search_schema, doc("searchSchema", "Search schema objects and columns")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/search/data",
            post_with(post_search_data, doc("searchData", "Search table data with bounded fan-out")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/explain",
            post_with(post_explain, doc("explainQuery", "Capture a typed execution plan")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/processes",
            get_with(list_processes, doc("listProcesses", "List database processes")),
        )
        .api_route(
            "/v1/sessions/:id/connections/:conn_id/processes/kill",
            post_with(kill_process, doc("killProcess", "Terminate a database process")),
        )
        .api_route(
            "/v1/sessions/:id/queries",
            post_with(execute_query, doc("executeQuery", "Execute query over synchronous HTTP")),
        )
        .api_route(
            "/v1/sessions/:id/transactions",
            get_with(list_transactions, doc("listTransactions", "List open transactions")).post_with(begin_transaction, doc("beginTransaction", "Begin transaction")),
        )
        .api_route(
            "/v1/sessions/:id/transactions/:tx_id/commit",
            post_with(commit_transaction, doc("commitTransaction", "Commit transaction")),
        )
        .api_route(
            "/v1/sessions/:id/transactions/:tx_id/rollback",
            post_with(rollback_transaction, doc("rollbackTransaction", "Rollback transaction")),
        )
        .api_route(
            "/v1/sessions/:id/transactions/:tx_id/preview",
            post_with(preview_transaction, doc("previewTransaction", "Preview commit or rollback consequences")),
        )
        .api_route(
            "/v1/sessions/:id/transactions/:tx_id/savepoints",
            post_with(create_savepoint, doc("createSavepoint", "Create transaction savepoint")),
        )
        .api_route(
            "/v1/sessions/:id/transactions/:tx_id/savepoints/rollback",
            post_with(rollback_to_savepoint, doc("rollbackToSavepoint", "Rollback to transaction savepoint")),
        )
        .api_route(
            "/v1/sessions/:id/transactions/:tx_id/savepoints/release",
            post_with(release_savepoint, doc("releaseSavepoint", "Release transaction savepoint")),
        )
        .api_route(
            "/v1/sessions/:id/ws",
            get_with(ws_session, doc("sessionWebSocket", "WebSocket query stream; protocol uses WsClientMessage/WsServerMessage")),
        )
        .api_route(
            "/v1/sessions/:id/queries/:cursor_id/cancel",
            post_with(cancel_query, doc("cancelQuery", "Cancel query")),
        )
        .api_route(
            "/v1/cursors/:cursor_id/pages",
            get_with(read_spill_pages, doc("readSpilledCursorPages", "Read pages from a spilled (evicted) cursor. Query params: `from_seq` (optional, must equal current pages_read), `limit` (default 32, max 256).")),
        )
        .api_route(
            "/v1/cursors/:cursor_id",
            delete_with(delete_spilled_cursor, doc("deleteSpilledCursor", "Delete a spilled cursor's file explicitly (idempotent). Reaper handles this on TTL too.")),
        )
        ;
    let mut api = OpenApi::default();
    let router = router.finish_api_with(&mut api, |t| t.title("sift API").version(VERSION));
    let openapi_doc = Arc::new(finalize_openapi(api));
    router
        .layer(Extension(openapi_doc))
        .layer(from_fn_with_state(state.clone(), rate_limit_middleware))
        .layer(from_fn_with_state(state.clone(), auth_middleware))
        .layer(from_fn(inject_peer_addr))
        .layer(from_fn_with_state(state.sessions.clone(), audit_middleware))
        .layer(from_fn_with_state(
            state.clone(),
            protocol_version_middleware,
        ))
        .layer(from_fn(correlation_middleware))
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
async fn protocol_version_middleware(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let path = req.uri().path();
    let presented = req.headers().get(&PROTOCOL_VERSION_HEADER);
    if let Some(value) = presented {
        let requested = match value.to_str() {
            Ok(value) => value,
            Err(_) => {
                return with_protocol_header(
                    ApiError::UnsupportedProtocolVersion {
                        requested: "<invalid header>".into(),
                    }
                    .into_response(),
                )
            }
        };
        if requested != PROTOCOL_VERSION {
            return with_protocol_header(
                ApiError::UnsupportedProtocolVersion {
                    requested: requested.to_string(),
                }
                .into_response(),
            );
        }
    } else if !state.auth.allow_legacy_unversioned
        && !matches!(path, "/v1/handshake" | "/v1/health" | "/v1/ready")
    {
        return with_protocol_header(ApiError::ProtocolHandshakeRequired.into_response());
    }

    with_protocol_header(next.run(req).await)
}

fn with_protocol_header(mut response: Response) -> Response {
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
    mut req: Request<Body>,
    next: Next,
) -> Response {
    let method = req.method().to_string();
    let path = req.uri().path().to_string();
    let actor = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0));
    req.extensions_mut()
        .insert(RequestAuditActor(actor.clone()));
    let start = Instant::now();
    let response = next.run(req).await;
    let status = response.status().as_u16();
    sessions.push_audit(AuditEntry {
        at: chrono::Utc::now(),
        method: method.clone(),
        path: path.clone(),
        status,
        duration_ms: start.elapsed().as_millis(),
    });
    sessions.push_operation_full(
        Operation::HttpRequest {
            method,
            path,
            status_code: status,
        },
        if status < 400 {
            OperationStatus::Succeeded
        } else {
            OperationStatus::Failed
        },
        match actor.load(std::sync::atomic::Ordering::Acquire) {
            0 => None,
            id => Some(id),
        },
        (status >= 400).then(|| status.to_string()),
        None,
        None,
    );
    response
}

#[derive(Clone)]
struct RequestAuditActor(std::sync::Arc<std::sync::atomic::AtomicI64>);

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
                if let Some(actor) = req.extensions().get::<RequestAuditActor>() {
                    actor
                        .0
                        .store(context.principal_id.0, std::sync::atomic::Ordering::Release);
                }
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

async fn rate_limit_middleware(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let Some(auth) = req.extensions().get::<AuthContext>().cloned() else {
        return next.run(req).await;
    };
    if body_tenant_rate_owned(req.method(), req.uri().path()) {
        return next.run(req).await;
    }
    let class = rate_limit_class(req.method(), req.uri().path());
    let tenant = request_tenant(&state, &auth, req.uri()).await;
    match state.auth.rate_limiter.admit(
        auth.principal_id.0,
        tenant.map(|tenant| tenant.0),
        class,
        auth.trusted_local,
    ) {
        Ok(()) => next.run(req).await,
        Err(retry_after_secs) => {
            record_rate_rejection(
                &state,
                &auth,
                class,
                req.uri().path(),
                tenant,
                retry_after_secs,
            );
            ApiError::RateLimited { retry_after_secs }.into_response()
        }
    }
}

fn body_tenant_rate_owned(method: &axum::http::Method, path: &str) -> bool {
    *method == axum::http::Method::POST
        && matches!(
            path,
            "/v1/sessions"
                | "/v1/metadata/rooms"
                | "/v1/metadata/connections"
                | "/v1/metadata/saved-queries"
                | "/v1/auth/tokens"
        )
}

fn admit_resolved_tenant(
    state: &AppState,
    auth: &AuthContext,
    tenant: Option<TenantId>,
    class: sift_protocol::RateLimitClass,
    route: &'static str,
) -> ApiResult<()> {
    state
        .auth
        .rate_limiter
        .admit(
            auth.principal_id.0,
            tenant.map(|tenant| tenant.0),
            class,
            auth.trusted_local,
        )
        .map_err(|retry_after_secs| {
            record_rate_rejection(state, auth, class, route, tenant, retry_after_secs);
            ApiError::RateLimited { retry_after_secs }
        })
}

fn record_rate_rejection(
    state: &AppState,
    auth: &AuthContext,
    class: sift_protocol::RateLimitClass,
    route: &str,
    tenant: Option<TenantId>,
    retry_after_secs: u64,
) {
    state.sessions.push_operation_full(
        Operation::RateLimitRejected {
            class,
            route: route.to_string(),
            tenant_id: tenant.map(|tenant| tenant.0),
        },
        OperationStatus::Failed,
        Some(auth.principal_id.0),
        Some("rate_limited".into()),
        None,
        Some(format!("retry after {retry_after_secs}s")),
    );
}

fn rate_limit_class(method: &axum::http::Method, path: &str) -> sift_protocol::RateLimitClass {
    use sift_protocol::RateLimitClass;
    if path.contains("/export") || path.contains("/import/") || path.ends_with("/bulk-insert") {
        return RateLimitClass::HeavyTransfer;
    }
    if path.ends_with("/queries")
        || path.ends_with("/explain")
        || path.ends_with("/search/data")
        || path.ends_with("/edits/apply")
        || path.ends_with("/processes/kill")
    {
        return RateLimitClass::Query;
    }
    if method == axum::http::Method::GET
        || path.ends_with("/ping")
        || path.ends_with("/complete")
        || path.ends_with("/search/schema")
        || path.ends_with("/edits/preview")
    {
        return RateLimitClass::Interactive;
    }
    RateLimitClass::Control
}

async fn request_tenant(
    state: &AppState,
    auth: &AuthContext,
    uri: &axum::http::Uri,
) -> Option<TenantId> {
    if let Some(tenant) = uri.query().and_then(|query| {
        query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            matches!(key, "tenant" | "tenant_id")
                .then(|| value.parse::<i64>().ok().map(TenantId))
                .flatten()
        })
    }) {
        return Some(tenant);
    }
    if let Some(session) = uri
        .path()
        .strip_prefix("/v1/sessions/")
        .and_then(|rest| rest.split('/').next())
        .and_then(|value| value.parse::<u64>().ok())
        .and_then(|id| {
            state
                .sessions
                .managed_tenant_for_session(sift_protocol::SessionId(id))
        })
    {
        return Some(session);
    }
    if let Some(tenant) = tenant_from_path(uri.path()) {
        return Some(tenant);
    }
    if let (Some(metadata), Some(resource)) =
        (state.metadata.clone(), metadata_tenant_resource(uri.path()))
    {
        let principal = auth.principal_id;
        let tenants: Vec<_> = auth
            .tenants
            .iter()
            .map(|membership| membership.tenant.id)
            .collect();
        if let Ok(tenant) = metadata_blocking(move || {
            let tenant = match resource {
                MetadataTenantResource::Room(id) => metadata
                    .get_room_member(RoomId(id), principal)?
                    .map(|_| metadata.get_room(RoomId(id)))
                    .transpose()?
                    .map(|room| room.tenant_id),
                MetadataTenantResource::Document(id) => metadata
                    .get_document_for_principal(DocumentId(id), principal, false)
                    .and_then(|document| metadata.get_room(document.room_id))
                    .map(|room| Some(room.tenant_id))
                    .or_else(|error| match error {
                        sift_metadata::MetadataError::DocumentNotFound(_) => Ok(None),
                        error => Err(error),
                    })?,
                MetadataTenantResource::Connection(id) => metadata
                    .get_connection_profile_for_principal(ConnectionProfileId(id), principal)
                    .map(|profile| Some(profile.tenant_id))
                    .or_else(|error| match error {
                        sift_metadata::MetadataError::ConnectionProfileNotFound(_) => Ok(None),
                        error => Err(error),
                    })?,
                MetadataTenantResource::SavedQuery(id) => {
                    let mut resolved = None;
                    for tenant in tenants {
                        match metadata.get_saved_query_visible(SavedQueryId(id), tenant, principal)
                        {
                            Ok(_) => {
                                resolved = Some(tenant);
                                break;
                            }
                            Err(sift_metadata::MetadataError::SavedQueryNotFound(_)) => {}
                            Err(error) => return Err(error.into()),
                        }
                    }
                    resolved
                }
            };
            Ok(tenant)
        })
        .await
        {
            if tenant.is_some() {
                return tenant;
            }
        }
    }
    (auth.tenants.len() == 1).then(|| auth.tenants[0].tenant.id)
}

fn tenant_from_path(path: &str) -> Option<TenantId> {
    ["/v1/metadata/tenants/", "/v1/admin/tenants/"]
        .iter()
        .find_map(|prefix| {
            path.strip_prefix(prefix)?
                .split('/')
                .next()?
                .parse::<i64>()
                .ok()
                .map(TenantId)
        })
}

#[derive(Clone, Copy)]
enum MetadataTenantResource {
    Room(i64),
    Document(i64),
    Connection(i64),
    SavedQuery(i64),
}

fn metadata_tenant_resource(path: &str) -> Option<MetadataTenantResource> {
    for (prefix, constructor) in [
        (
            "/v1/metadata/rooms/",
            MetadataTenantResource::Room as fn(i64) -> MetadataTenantResource,
        ),
        (
            "/v1/metadata/documents/",
            MetadataTenantResource::Document as fn(i64) -> MetadataTenantResource,
        ),
        (
            "/v1/metadata/connections/",
            MetadataTenantResource::Connection as fn(i64) -> MetadataTenantResource,
        ),
        (
            "/v1/metadata/saved-queries/",
            MetadataTenantResource::SavedQuery as fn(i64) -> MetadataTenantResource,
        ),
    ] {
        if let Some(id) = path
            .strip_prefix(prefix)
            .and_then(|rest| rest.split('/').next())
            .and_then(|value| value.parse::<i64>().ok())
        {
            return Some(constructor(id));
        }
    }
    None
}

fn is_public_path(path: &str) -> bool {
    matches!(
        path,
        "/v1/handshake"
            | "/v1/health"
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
            | "/v1/auth/ssh-proxy/exchange"
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
    trusted_local: bool,
}

#[derive(Clone)]
struct ExecuteMetadataContext {
    metadata: MetadataStore,
    principal_id: PrincipalId,
    room_id: Option<RoomId>,
    connection_profile_id: Option<ConnectionProfileId>,
    /// Present when the query targets a bound room: route it through the
    /// room's server-owned connection instead of the caller's (ADR-037).
    room_routing: Option<RoomRouting>,
}

/// Everything needed to route a room-scoped query onto the room's
/// server-owned connection, opened under the binder's provenance.
#[derive(Clone)]
struct RoomRouting {
    room_id: i64,
    binder: PrincipalId,
    tenant: TenantId,
    profile_id: ConnectionProfileId,
    provider_id: sift_protocol::ProviderId,
    engine: sift_protocol::Engine,
    policy_revision: u64,
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
struct RoomResultPagesQuery {
    #[serde(default)]
    from_seq: u64,
    #[serde(default = "default_room_result_page_limit")]
    limit: usize,
}

fn default_room_result_page_limit() -> usize {
    32
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
    AddRoomMemberRequest, BindRoomConnectionRequest, CreateDocumentRequest, CreateRoomRequest,
    CreateSavedQueryRequest, IssueTokenRequest, IssueTokenResponse,
    OpenConnectionFromProfileRequest, SetCredentialRequest, UpdateDocumentSnapshotRequest,
    UpdateSavedQueryRequest, UpsertConnectionProfileRequest,
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
    state
        .sessions
        .disconnect_managed_principal(auth.principal_id)
        .await;
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
    state
        .sessions
        .disconnect_managed_principal(auth.principal_id)
        .await;
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
    state
        .sessions
        .disconnect_managed_principal(auth.principal_id)
        .await;
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

#[derive(Deserialize, JsonSchema)]
struct GithubStartQuery {
    client_kind: Option<AuthClientKind>,
}

#[derive(Deserialize, JsonSchema)]
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
    if request.disabled {
        state
            .sessions
            .disconnect_managed_principal(PrincipalId(id))
            .await;
    }
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
    state
        .sessions
        .disconnect_managed_principal(PrincipalId(principal_id))
        .await;
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
    state
        .sessions
        .disconnect_managed_principal(PrincipalId(principal_id))
        .await;
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

async fn exchange_ssh_proxy_capability(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<SshProxyCapabilityExchangeRequest>,
) -> ApiResult<Json<SshProxyAccessGrant>> {
    if state.auth.transport != Transport::SshProxy {
        return Err(ApiError::Forbidden(
            "SSH proxy capability exchange is unavailable on this transport".into(),
        ));
    }
    let source = headers
        .get(&PEER_ADDR_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown");
    if state.auth.runtime.ssh_capability_is_limited(source) {
        return Err(ApiError::TooManyAuthAttempts);
    }
    let metadata = metadata_store(&state)?;
    let issued = match metadata
        .consume_ssh_proxy_capability(
            &request.capability,
            &state.auth.instance_audience,
            &state.auth.daemon_generation,
            NewOperationAudit {
                actor_principal_id: None,
                action: "authenticate.ssh_capability".into(),
                target: "auth_session".into(),
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
        Ok(issued) => issued,
        Err(error) => {
            tracing::warn!(%error, "SSH proxy capability exchange was denied");
            state.auth.runtime.record_ssh_capability_failure(source);
            record_auth_failure(metadata, "authenticate.ssh_capability", "denied")?;
            state.sessions.push_operation_full(
                Operation::Authenticate {
                    method: sift_protocol::AuthenticationMethod::SshCapability,
                },
                OperationStatus::Failed,
                None,
                Some("authentication_denied".into()),
                None,
                Some("authentication denied".into()),
            );
            return Err(ApiError::Unauthorized);
        }
    };
    state.auth.runtime.clear_ssh_capability_failures(source);
    state.sessions.push_operation_local(
        Operation::Authenticate {
            method: sift_protocol::AuthenticationMethod::SshCapability,
        },
        OperationStatus::Succeeded,
        Some(issued.principal_id.0),
        None,
        None,
        None,
    );
    Ok(Json(SshProxyAccessGrant {
        access_token: issued.access_token,
        expires_at: issued.access_expires_at,
        principal_id: issued.principal_id.0,
        daemon_generation: issued.daemon_generation,
    }))
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
                trusted_local: false,
            });
        }
        if state
            .auth
            .bearer_token
            .as_deref()
            .is_some_and(|expected| constant_time_eq(token.as_bytes(), expected.as_bytes()))
        {
            return local_auth_context(metadata, false);
        }
        // Explicit invalid credentials never fall through to loopback bypass.
        return Err(ApiError::Unauthorized);
    }

    // Team-mode validation forbids enabling this implicit path.
    if state.auth.loopback_bypass && peer_is_loopback(headers) {
        return local_auth_context(metadata, true);
    }

    Err(ApiError::Unauthorized)
}

fn local_auth_context(metadata: &MetadataStore, trusted_local: bool) -> ApiResult<AuthContext> {
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
        trusted_local,
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
                trusted_local: false,
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
    let sql = req.sql.clone();
    let (effective_profile, room_routing) = metadata_blocking(move || {
        let mut effective = profile;
        let mut routing: Option<RoomRouting> = None;
        if let Some(room) = room {
            ensure_room_permission(
                &metadata_for_check,
                &auth_for_check,
                room,
                RoomPermission::Write,
            )?;
            let room_row = metadata_for_check.get_room(room)?;
            // A bound room runs every query through its server-owned
            // connection (ADR-037); an unbound room cannot execute.
            let profile_id = room_row.bound_connection_profile_id.ok_or_else(|| {
                ApiError::BadRequest(
                    "room has no bound connection; bind one before running queries".into(),
                )
            })?;
            let binder = room_row.bound_connection_by.ok_or_else(|| {
                ApiError::Internal("bound room connection is missing its binder".into())
            })?;
            let bound_profile =
                metadata_for_check.get_connection_profile(room_row.tenant_id, profile_id)?;
            // Submitter-scoped intersection: the submitting member's room role
            // x the bound profile's policy, enforced BEFORE routing to the
            // shared connection (which itself authorizes only as the binder).
            let scope = room_submitter_scope(
                &metadata_for_check,
                auth_for_check.principal_id,
                room,
                room_row.tenant_id,
                &bound_profile,
            )?;
            crate::authorization::authorize(&scope, sift_protocol::OperationKind::ExecuteQuery)
                .map_err(|denial| ApiError::Forbidden(denial.public_reason().into()))?;
            crate::sql_policy::enforce(
                &bound_profile.policy,
                bound_profile.semantic_engine,
                sift_protocol::OperationKind::ExecuteQuery,
                Some(&sql),
                &[],
            )?;
            effective = Some(profile_id);
            routing = Some(RoomRouting {
                room_id: room.0,
                binder,
                tenant: room_row.tenant_id,
                profile_id,
                provider_id: bound_profile.provider_id,
                engine: bound_profile.semantic_engine,
                policy_revision: bound_profile.policy.revision,
            });
        } else if let Some(profile) = effective {
            metadata_for_check
                .get_connection_profile_for_principal(profile, auth_for_check.principal_id)?;
        }
        Ok((effective, routing))
    })
    .await?;

    Ok(Some(ExecuteMetadataContext {
        metadata,
        principal_id: auth.principal_id,
        room_id: room,
        connection_profile_id: effective_profile,
        room_routing,
    }))
}

/// Build the submitting member's authorization scope for a room-scoped
/// execute: their room role and tenant role intersected with the bound
/// profile's policy (ADR-036/037).
fn room_submitter_scope(
    metadata: &MetadataStore,
    principal: PrincipalId,
    room: RoomId,
    tenant: TenantId,
    profile: &sift_metadata::ConnectionProfile,
) -> ApiResult<crate::authorization::AuthorizationScope> {
    use crate::authorization::{AuthorizationRoomRole, AuthorizationScope};
    let member = metadata
        .get_room_member(room, principal)?
        .ok_or_else(|| ApiError::Forbidden("room membership required".into()))?;
    let room_role = Some(match member.role {
        RoomRole::Owner => AuthorizationRoomRole::Owner,
        RoomRole::Editor => AuthorizationRoomRole::Editor,
        RoomRole::Viewer => AuthorizationRoomRole::Viewer,
    });
    let tenant_role = metadata
        .list_principal_tenants(principal)?
        .into_iter()
        .find(|membership| membership.tenant.id == tenant)
        .map(|membership| sift_protocol::TenantRole::from(&membership.role));
    if tenant_role.is_none() {
        return Err(ApiError::Forbidden("tenant membership required".into()));
    }
    Ok(AuthorizationScope {
        authenticated: true,
        trusted_local: false,
        instance_admin: false,
        tenant_role,
        room_role,
        connection_policy: Some(profile.policy.clone()),
    })
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

async fn handshake(
    State(state): State<AppState>,
    Json(request): Json<HandshakeRequest>,
) -> ApiResult<Json<HandshakeResponse>> {
    if !request.protocol.is_valid() {
        return Err(ApiError::BadRequest(
            "protocol range minimum must not exceed maximum".into(),
        ));
    }
    let supported = ProtocolRange::exact(PROTOCOL_VERSION_NUMBER);
    let selected = request.protocol.highest_common(supported).ok_or_else(|| {
        ApiError::UnsupportedProtocolVersion {
            requested: format!("{}-{}", request.protocol.minimum, request.protocol.maximum),
        }
    })?;

    Ok(Json(HandshakeResponse {
        server_version: VERSION.to_string(),
        protocol: supported,
        selected_protocol: selected,
        instance_id: state.auth.instance_id.clone(),
        daemon_generation: state.auth.daemon_generation.clone(),
        deployment: match state.auth.deployment {
            DeploymentPolicy::Personal => HandshakeDeployment::Personal,
            DeploymentPolicy::Team => HandshakeDeployment::Team,
        },
        transport: match state.auth.transport {
            Transport::Loopback => HandshakeTransport::Loopback,
            Transport::Network => HandshakeTransport::Network,
            Transport::SshProxy => HandshakeTransport::SshProxy,
        },
        runtime_mode: match state.auth.runtime_mode {
            RuntimeMode::InProcess => HandshakeRuntimeMode::InProcess,
            RuntimeMode::Daemon => HandshakeRuntimeMode::Daemon,
            RuntimeMode::Container => HandshakeRuntimeMode::Container,
        },
        capabilities: vec!["protocol_handshake".into()],
    }))
}

async fn health(State(state): State<AppState>) -> Json<Health> {
    Json(Health {
        status: "ok".to_string(),
        version: VERSION.to_string(),
        providers: state
            .sessions
            .registry()
            .providers()
            .descriptors()
            .into_iter()
            .map(|descriptor| descriptor.provider.provider_id)
            .collect(),
    })
}

/// Readiness probe (ADR-018): `200` when the server should take traffic,
/// `503` otherwise. Not ready while draining, when no driver is registered,
/// or when the (enabled) metadata store is unreachable. The `Readiness` body
/// is returned in both cases so callers can see which check failed.
async fn ready(State(state): State<AppState>) -> Response {
    let draining = state.shutdown.is_draining();
    let providers: Vec<_> = state
        .sessions
        .registry()
        .providers()
        .descriptors()
        .into_iter()
        .map(|descriptor| descriptor.provider.provider_id)
        .collect();
    let drivers_registered = !providers.is_empty();
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
        providers,
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
        .map(|id| metadata.get_connection_profile_for_principal(id, auth.principal_id))
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

async fn list_providers(
    State(state): State<AppState>,
) -> Json<Vec<sift_protocol::ProviderDescriptor>> {
    Json(state.sessions.registry().providers().descriptors())
}

async fn list_extensions(
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<sift_protocol::ExtensionDescriptor>>> {
    let metadata = metadata_store_cloned(&state)?;
    let packages = metadata.selected_extension_packages()?;
    let mut descriptors = Vec::with_capacity(packages.len());
    for package in packages {
        descriptors.push(extension_descriptor(&state, &metadata, package)?);
    }
    Ok(Json(descriptors))
}

async fn get_extension(
    State(state): State<AppState>,
    Path((publisher, name)): Path<(String, String)>,
) -> ApiResult<Json<sift_protocol::ExtensionDescriptor>> {
    let id = extension_id(&publisher, &name)?;
    let metadata = metadata_store_cloned(&state)?;
    let package = metadata.selected_extension_package(id.as_str())?;
    Ok(Json(extension_descriptor(&state, &metadata, package)?))
}

async fn validate_extension(
    State(state): State<AppState>,
    auth: Option<Extension<AuthContext>>,
    body: Body,
) -> ApiResult<Json<sift_protocol::ValidatedExtensionPackage>> {
    require_instance_admin(&state, auth.as_ref().map(|Extension(auth)| auth))?;
    let registry = state
        .sessions
        .package_registry()
        .ok_or(ApiError::MetadataUnavailable)?;
    let archive = receive_extension_archive(body).await?;
    let path = archive.to_path_buf();
    let validated = tokio::task::spawn_blocking(move || {
        registry.validate(&path, sift_plugin_host::SignaturePolicy::AllowUnsigned)
    })
    .await
    .map_err(|error| ApiError::Internal(error.to_string()))?
    .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    Ok(Json(validated_package(&validated)))
}

async fn install_extension(
    State(state): State<AppState>,
    auth: Option<Extension<AuthContext>>,
    Query(request): Query<ExtensionInstallQuery>,
    body: Body,
) -> ApiResult<Json<sift_protocol::ValidatedExtensionPackage>> {
    let auth = require_instance_admin(&state, auth.as_ref().map(|Extension(auth)| auth))?;
    let registry = state
        .sessions
        .package_registry()
        .ok_or(ApiError::MetadataUnavailable)?;
    let archive = receive_extension_archive(body).await?;
    let path = archive.to_path_buf();
    let installed = tokio::task::spawn_blocking(move || {
        registry.install_authorized(&path, request.allow_unsigned_local)
    })
    .await
    .map_err(|error| ApiError::Internal(error.to_string()))?
    .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    state.sessions.push_operation_full(
        Operation::ManageExtension {
            action: sift_protocol::ExtensionAdminAction::Install,
            extension_id: installed.validated.manifest.id.clone(),
        },
        OperationStatus::Succeeded,
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    Ok(Json(validated_package(&installed.validated)))
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct ExtensionInstallQuery {
    #[serde(default)]
    allow_unsigned_local: bool,
}

async fn update_extension_selection(
    State(state): State<AppState>,
    auth: Option<Extension<AuthContext>>,
    Path((publisher, name)): Path<(String, String)>,
    Json(request): Json<sift_protocol::ExtensionSelectionRequest>,
) -> ApiResult<Json<sift_protocol::ExtensionDescriptor>> {
    let auth = require_instance_admin(&state, auth.as_ref().map(|Extension(auth)| auth))?;
    let id = extension_id(&publisher, &name)?;
    let metadata = metadata_store_cloned(&state)?;
    let current = metadata.extension_selection(id.as_str())?;
    let lifecycle = if request.enabled {
        sift_protocol::ExtensionLifecycleState::Ready
    } else {
        sift_protocol::ExtensionLifecycleState::Disabled
    };
    metadata.update_extension_selection(sift_metadata::UpdateExtensionSelection {
        extension_id: id.as_str(),
        selected_archive_sha256: None,
        enabled: request.enabled,
        lifecycle,
        isolation: current.isolation,
        quarantine_reason: None,
        expected_revision: request.expected_revision,
    })?;
    state.sessions.refresh_extension_runtimes()?;
    record_extension_admin(
        &state,
        auth.principal_id,
        if request.enabled {
            sift_protocol::ExtensionAdminAction::Enable
        } else {
            sift_protocol::ExtensionAdminAction::Disable
        },
        id.clone(),
    );
    let package = metadata.selected_extension_package(id.as_str())?;
    Ok(Json(extension_descriptor(&state, &metadata, package)?))
}

async fn update_extension_grants(
    State(state): State<AppState>,
    auth: Option<Extension<AuthContext>>,
    Path((publisher, name)): Path<(String, String)>,
    Json(request): Json<sift_protocol::ExtensionGrantRequest>,
) -> ApiResult<Json<sift_protocol::ExtensionDescriptor>> {
    let auth = require_instance_admin(&state, auth.as_ref().map(|Extension(auth)| auth))?;
    let id = extension_id(&publisher, &name)?;
    let metadata = metadata_store_cloned(&state)?;
    metadata.replace_extension_grants(&sift_metadata::ReplaceExtensionGrants {
        extension_id: id.to_string(),
        grants: request
            .granted
            .into_iter()
            .map(|capability| (capability, "{}".into()))
            .collect(),
        expected_revision: request.expected_revision,
    })?;
    state.sessions.refresh_extension_runtimes()?;
    record_extension_admin(
        &state,
        auth.principal_id,
        sift_protocol::ExtensionAdminAction::Grant,
        id.clone(),
    );
    let package = metadata.selected_extension_package(id.as_str())?;
    Ok(Json(extension_descriptor(&state, &metadata, package)?))
}

async fn update_extension_tenant(
    State(state): State<AppState>,
    auth: Option<Extension<AuthContext>>,
    Path((publisher, name, tenant_id)): Path<(String, String, i64)>,
    Json(request): Json<sift_protocol::ExtensionTenantSelectionRequest>,
) -> ApiResult<Json<sift_protocol::ExtensionDescriptor>> {
    let auth = require_instance_admin(&state, auth.as_ref().map(|Extension(auth)| auth))?;
    let id = extension_id(&publisher, &name)?;
    let metadata = metadata_store_cloned(&state)?;
    metadata.set_extension_tenant_allowed(
        id.as_str(),
        tenant_id,
        request.allowed,
        request.expected_revision,
    )?;
    record_extension_admin(
        &state,
        auth.principal_id,
        if request.allowed {
            sift_protocol::ExtensionAdminAction::AllowTenant
        } else {
            sift_protocol::ExtensionAdminAction::DenyTenant
        },
        id.clone(),
    );
    let package = metadata.selected_extension_package(id.as_str())?;
    Ok(Json(extension_descriptor(&state, &metadata, package)?))
}

async fn rollback_extension(
    State(state): State<AppState>,
    auth: Option<Extension<AuthContext>>,
    Path((publisher, name)): Path<(String, String)>,
    Json(request): Json<sift_protocol::ExpectedRevision>,
) -> ApiResult<Json<sift_protocol::ExtensionDescriptor>> {
    let auth = require_instance_admin(&state, auth.as_ref().map(|Extension(auth)| auth))?;
    let id = extension_id(&publisher, &name)?;
    let metadata = metadata_store_cloned(&state)?;
    metadata.rollback_extension_selection(id.as_str(), request.expected_revision)?;
    state.sessions.refresh_extension_runtimes()?;
    record_extension_admin(
        &state,
        auth.principal_id,
        sift_protocol::ExtensionAdminAction::Rollback,
        id.clone(),
    );
    let package = metadata.selected_extension_package(id.as_str())?;
    Ok(Json(extension_descriptor(&state, &metadata, package)?))
}

async fn uninstall_extension(
    State(state): State<AppState>,
    auth: Option<Extension<AuthContext>>,
    Path((publisher, name)): Path<(String, String)>,
    Query(request): Query<sift_protocol::ExpectedRevision>,
) -> ApiResult<Json<sift_protocol::ExtensionDescriptor>> {
    let auth = require_instance_admin(&state, auth.as_ref().map(|Extension(auth)| auth))?;
    let id = extension_id(&publisher, &name)?;
    let metadata = metadata_store_cloned(&state)?;
    metadata.uninstall_extension(id.as_str(), request.expected_revision)?;
    state.sessions.refresh_extension_runtimes()?;
    record_extension_admin(
        &state,
        auth.principal_id,
        sift_protocol::ExtensionAdminAction::Uninstall,
        id.clone(),
    );
    let package = metadata.selected_extension_package(id.as_str())?;
    Ok(Json(extension_descriptor(&state, &metadata, package)?))
}

async fn purge_extension(
    State(state): State<AppState>,
    auth: Option<Extension<AuthContext>>,
    Path((publisher, name)): Path<(String, String)>,
    Json(request): Json<sift_protocol::ExpectedRevision>,
) -> ApiResult<Json<sift_protocol::ExtensionPurgeResponse>> {
    let auth = require_instance_admin(&state, auth.as_ref().map(|Extension(auth)| auth))?;
    let id = extension_id(&publisher, &name)?;
    let metadata = metadata_store_cloned(&state)?;
    let selection = metadata.extension_selection(id.as_str())?;
    if selection.revision != request.expected_revision
        || selection.lifecycle != sift_protocol::ExtensionLifecycleState::Uninstalled
    {
        return Err(ApiError::BadRequest(
            "extension must be uninstalled at the expected revision before purge".into(),
        ));
    }
    let purged_namespaces = metadata.purge_extension_storage(id.as_str())?;
    record_extension_admin(
        &state,
        auth.principal_id,
        sift_protocol::ExtensionAdminAction::Purge,
        id,
    );
    Ok(Json(sift_protocol::ExtensionPurgeResponse {
        purged_namespaces,
    }))
}

async fn extension_diagnostics(
    State(state): State<AppState>,
    Path((publisher, name)): Path<(String, String)>,
) -> ApiResult<Json<sift_protocol::ExtensionDiagnostics>> {
    let id = extension_id(&publisher, &name)?;
    let selection = metadata_store_cloned(&state)?.extension_selection(id.as_str())?;
    Ok(Json(sift_protocol::ExtensionDiagnostics {
        extension_id: id,
        lifecycle: selection.lifecycle,
        quarantine_reason: selection.quarantine_reason,
        generation_health: None,
        messages: vec![],
    }))
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ToolListQuery {
    #[serde(default)]
    mcp_only: bool,
    tenant_id: Option<i64>,
    room_id: Option<i64>,
    profile_id: Option<i64>,
    connection_id: Option<String>,
    document_id: Option<String>,
}

async fn list_governed_tools(
    State(state): State<AppState>,
    auth: Option<Extension<AuthContext>>,
    Query(query): Query<ToolListQuery>,
) -> ApiResult<Json<Vec<sift_protocol::GovernedToolDescriptor>>> {
    let auth = auth
        .as_ref()
        .map(|Extension(auth)| auth)
        .ok_or(ApiError::Unauthorized)?;
    let context = tool_context(&query);
    let authorization = tool_authorization_scope(&state, auth, &context)?;
    let registry = state
        .sessions
        .tool_registry()
        .ok_or(ApiError::MetadataUnavailable)?;
    Ok(Json(registry.list(
        &authorization,
        &context,
        query.mcp_only,
    )))
}

async fn invoke_governed_tool(
    State(state): State<AppState>,
    auth: Option<Extension<AuthContext>>,
    Json(request): Json<sift_protocol::InvokeToolRequest>,
) -> ApiResult<Json<sift_protocol::InvokeToolResponse>> {
    let auth = auth
        .as_ref()
        .map(|Extension(auth)| auth)
        .ok_or(ApiError::Unauthorized)?;
    let authorization = tool_authorization_scope(&state, auth, &request.context)?;
    let registry = state
        .sessions
        .tool_registry()
        .ok_or(ApiError::MetadataUnavailable)?;
    let tenant_id = request.context.tenant_id;
    let room_id = request.context.room_id;
    let response = registry
        .invoke(
            request,
            crate::extension_dispatch::DispatchContext {
                authorization,
                principal_id: auth.principal_id,
                tenant_id,
                room_id,
                correlation_id: crate::correlation::current()
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            },
        )
        .await
        .map_err(tool_error)?;
    Ok(Json(response))
}

async fn invoke_extension_action(
    State(state): State<AppState>,
    auth: Option<Extension<AuthContext>>,
    Json(request): Json<sift_protocol::InvokeExtensionRequest>,
) -> ApiResult<Json<sift_protocol::InvokeExtensionOutcome>> {
    let auth = auth
        .as_ref()
        .map(|Extension(auth)| auth)
        .ok_or(ApiError::Unauthorized)?;
    let context = sift_protocol::ToolContext {
        tenant_id: None,
        room_id: None,
        profile_id: None,
        connection_id: None,
        document_id: None,
    };
    let authorization = tool_authorization_scope(&state, auth, &context)?;
    let metadata = metadata_store_cloned(&state)?;
    let operation_id = format!(
        "{}#{}",
        request.operation.contribution_id, request.operation.action
    );
    let binding = sift_metadata::ApprovalBinding {
        principal_id: auth.principal_id,
        operation_id,
        context_fingerprint: crate::automation::fingerprint(
            &serde_json::to_value(&request.operation)
                .map_err(|error| ApiError::BadRequest(error.to_string()))?,
        )
        .map_err(|error| ApiError::BadRequest(error.to_string()))?,
        input_fingerprint: crate::automation::fingerprint(&request.arguments)
            .map_err(|error| ApiError::BadRequest(error.to_string()))?,
    };
    if sift_protocol::classification_requires_approval(request.operation.classification) {
        if let Some(approval_id) = request.approval_id {
            metadata.consume_operation_approval(&approval_id, &binding)?;
        } else {
            let approval = metadata.create_operation_approval(&binding, None)?;
            return Ok(Json(
                sift_protocol::InvokeExtensionOutcome::ApprovalRequired { approval },
            ));
        }
    }
    let registry = state
        .sessions
        .tool_registry()
        .ok_or(ApiError::MetadataUnavailable)?;
    let (_, response) = registry
        .dispatcher()
        .dispatch(
            request.operation,
            request.arguments,
            crate::extension_dispatch::DispatchContext {
                authorization,
                principal_id: auth.principal_id,
                tenant_id: None,
                room_id: None,
                correlation_id: crate::correlation::current()
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            },
        )
        .await
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    Ok(Json(sift_protocol::InvokeExtensionOutcome::Completed {
        result: response.result,
    }))
}

async fn create_operation_approval(
    State(state): State<AppState>,
    auth: Option<Extension<AuthContext>>,
    Json(request): Json<sift_protocol::CreateOperationApprovalRequest>,
) -> ApiResult<Json<sift_protocol::OperationApproval>> {
    let auth = auth
        .as_ref()
        .map(|Extension(auth)| auth)
        .ok_or(ApiError::Unauthorized)?;
    let operation_id = format!(
        "{}#{}",
        request.operation.contribution_id, request.operation.action
    );
    let context_fingerprint = crate::automation::fingerprint(
        &serde_json::to_value(&request.operation)
            .map_err(|error| ApiError::BadRequest(error.to_string()))?,
    )
    .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    let binding = sift_metadata::ApprovalBinding {
        principal_id: auth.principal_id,
        operation_id,
        context_fingerprint,
        input_fingerprint: request.input_fingerprint,
    };
    Ok(Json(
        metadata_store_cloned(&state)?.create_operation_approval(&binding, None)?,
    ))
}

async fn approve_operation(
    State(state): State<AppState>,
    auth: Option<Extension<AuthContext>>,
    Path(approval_id): Path<String>,
    Json(request): Json<sift_protocol::ExpectedRevision>,
) -> ApiResult<Json<sift_protocol::OperationApproval>> {
    let auth = auth
        .as_ref()
        .map(|Extension(auth)| auth)
        .ok_or(ApiError::Unauthorized)?;
    Ok(Json(metadata_store_cloned(&state)?.approve_operation(
        &approval_id,
        auth.principal_id,
        request.expected_revision,
    )?))
}

fn extension_descriptor(
    state: &AppState,
    metadata: &MetadataStore,
    package: sift_metadata::SelectedExtensionPackage,
) -> ApiResult<sift_protocol::ExtensionDescriptor> {
    let manifest: sift_extension_protocol::ExtensionManifest =
        serde_json::from_str(&package.manifest_json)
            .map_err(|error| ApiError::Internal(error.to_string()))?;
    let stored = metadata.extension_contributions(&package.selection.selected_archive_sha256)?;
    let registry = state.sessions.package_registry();
    let required_capabilities: Vec<_> = manifest
        .capabilities
        .iter()
        .filter(|capability| capability.required)
        .map(|capability| capability.kind)
        .collect();
    let mut contributions = Vec::with_capacity(stored.len());
    for contribution in stored {
        let id = sift_protocol::ContributionId::new(contribution.contribution_id.clone())
            .map_err(|error| ApiError::Internal(error.to_string()))?;
        let mut operation = None;
        let mut client = None;
        let action = if matches!(contribution.kind.as_str(), "command" | "governed_tool") {
            Some(
                serde_json::from_str::<sift_extension_protocol::ActionContribution>(
                    &contribution.descriptor_json,
                )
                .map_err(|error| ApiError::Internal(error.to_string()))?,
            )
        } else {
            None
        };
        if let Some(action) = action {
            let input_schema = load_package_schema(
                registry.as_ref(),
                &package.selection.selected_archive_sha256,
                &action.input_schema,
            )?;
            let output_schema = load_package_schema(
                registry.as_ref(),
                &package.selection.selected_archive_sha256,
                &action.output_schema,
            )?;
            operation = Some(sift_protocol::ExtensionActionDescriptor {
                action: action.action.clone(),
                classification: action.classification,
                input_schema,
                output_schema,
                timeout_ms: action.timeout_ms,
                max_result_bytes: action.max_result_bytes,
            });
            if contribution.kind == "command" {
                client = Some(sift_protocol::ClientContributionDescriptor::Command {
                    title: contribution.local_id.clone(),
                    action: action.action,
                });
            }
        }
        let invocable_kind = matches!(
            contribution.kind.as_str(),
            "database_provider" | "command" | "governed_tool"
        );
        contributions.push(sift_protocol::ContributionDescriptor {
            id,
            kind: contribution.kind,
            display_name: contribution.local_id,
            active: package.selection.enabled,
            invocable: package.selection.enabled
                && package.selection.lifecycle == sift_protocol::ExtensionLifecycleState::Ready
                && invocable_kind,
            required_capabilities: required_capabilities.clone(),
            operation,
            client,
        });
    }
    Ok(sift_protocol::ExtensionDescriptor {
        id: manifest.id,
        name: manifest.name,
        version: package.version,
        archive_sha256: package.selection.selected_archive_sha256,
        manifest_sha256: package.manifest_sha256,
        provenance: package.provenance,
        lifecycle: package.selection.lifecycle,
        isolation: package.selection.isolation,
        enabled: package.selection.enabled,
        revision: package.selection.revision,
        contributions,
    })
}

fn load_package_schema(
    registry: Option<&Arc<sift_plugin_host::ExtensionPackageRegistry>>,
    digest: &str,
    path: &str,
) -> ApiResult<serde_json::Value> {
    let registry = registry.ok_or(ApiError::MetadataUnavailable)?;
    let bytes = registry
        .read_package_file(digest, path, 1024 * 1024)
        .map_err(|error| ApiError::Internal(error.to_string()))?;
    serde_json::from_slice(&bytes).map_err(|error| ApiError::Internal(error.to_string()))
}

fn validated_package(
    package: &sift_plugin_host::ValidatedPackage,
) -> sift_protocol::ValidatedExtensionPackage {
    let contributions = package.manifest.contributions.database_provider.len()
        + package.manifest.contributions.tunnel_provider.len()
        + package.manifest.contributions.credential_broker.len()
        + package.manifest.contributions.connection_hook.len()
        + package.manifest.contributions.import_format.len()
        + package.manifest.contributions.export_format.len()
        + package.manifest.contributions.dialect_pack.len()
        + package.manifest.contributions.command.len()
        + package.manifest.contributions.governed_tool.len()
        + package.manifest.contributions.agent_context.len()
        + package.manifest.contributions.client_panel.len();
    sift_protocol::ValidatedExtensionPackage {
        extension_id: package.manifest.id.clone(),
        name: package.manifest.name.clone(),
        version: package.manifest.version.clone(),
        archive_sha256: package.archive_sha256.clone(),
        manifest_sha256: package.manifest_sha256.clone(),
        signed: package.signed,
        contributions,
    }
}

async fn receive_extension_archive(body: Body) -> ApiResult<tempfile::TempPath> {
    use tokio::io::AsyncWriteExt;

    const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
    let temporary =
        tempfile::NamedTempFile::new().map_err(|error| ApiError::Internal(error.to_string()))?;
    let path = temporary.into_temp_path();
    let mut file = tokio::fs::File::create(&path)
        .await
        .map_err(|error| ApiError::Internal(error.to_string()))?;
    let mut received = 0_u64;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| ApiError::BadRequest(error.to_string()))?;
        received = received
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| ApiError::BadRequest("extension archive is too large".into()))?;
        if received > MAX_ARCHIVE_BYTES {
            return Err(ApiError::BadRequest(
                "extension archive exceeds the byte limit".into(),
            ));
        }
        file.write_all(&chunk)
            .await
            .map_err(|error| ApiError::Internal(error.to_string()))?;
    }
    file.flush()
        .await
        .map_err(|error| ApiError::Internal(error.to_string()))?;
    file.sync_all()
        .await
        .map_err(|error| ApiError::Internal(error.to_string()))?;
    drop(file);
    Ok(path)
}

fn extension_id(publisher: &str, name: &str) -> ApiResult<sift_protocol::ExtensionId> {
    sift_protocol::ExtensionId::new(format!("{publisher}/{name}"))
        .map_err(|error| ApiError::BadRequest(error.to_string()))
}

fn require_instance_admin<'a>(
    state: &AppState,
    auth: Option<&'a AuthContext>,
) -> ApiResult<&'a AuthContext> {
    let auth = auth.ok_or(ApiError::Unauthorized)?;
    if auth.trusted_local
        && state.auth.deployment == DeploymentPolicy::Personal
        && state.auth.transport == Transport::Loopback
    {
        return Ok(auth);
    }
    let metadata = metadata_store(state)?;
    let principal = metadata
        .principal_by_id(auth.principal_id)?
        .ok_or(ApiError::Unauthorized)?;
    if principal.is_instance_admin {
        Ok(auth)
    } else {
        Err(ApiError::Forbidden(
            "instance administrator context required".into(),
        ))
    }
}

fn record_extension_admin(
    state: &AppState,
    principal: PrincipalId,
    action: sift_protocol::ExtensionAdminAction,
    extension_id: sift_protocol::ExtensionId,
) {
    state.sessions.push_operation_full(
        Operation::ManageExtension {
            action,
            extension_id,
        },
        OperationStatus::Succeeded,
        Some(principal.0),
        None,
        None,
        None,
    );
}

fn tool_context(query: &ToolListQuery) -> sift_protocol::ToolContext {
    sift_protocol::ToolContext {
        tenant_id: query.tenant_id,
        room_id: query.room_id,
        profile_id: query.profile_id,
        connection_id: query.connection_id.clone(),
        document_id: query.document_id.clone(),
    }
}

fn tool_authorization_scope(
    state: &AppState,
    auth: &AuthContext,
    context: &sift_protocol::ToolContext,
) -> ApiResult<crate::authorization::AuthorizationScope> {
    let operation_context = sift_protocol::OperationCapabilityContext {
        tenant_id: context.tenant_id,
        room_id: context.room_id,
        connection_profile_id: context.profile_id,
        session: None,
        connection: None,
        transaction: None,
    };
    capability_authorization_scope(state, Some(auth), &operation_context)?
        .ok_or(ApiError::Unauthorized)
}

fn tool_error(error: crate::automation::ToolRegistryError) -> ApiError {
    match error {
        crate::automation::ToolRegistryError::NotFound => {
            ApiError::BadRequest("tool is not available".into())
        }
        crate::automation::ToolRegistryError::Denied => {
            ApiError::Forbidden("tool operation is not authorized".into())
        }
        crate::automation::ToolRegistryError::Approval(error) => ApiError::Metadata(error),
        other => ApiError::BadRequest(other.to_string()),
    }
}

/// Serve the immutable OpenAPI document generated once at startup and stored as
/// an `Extension`. See `finalize_openapi`.
async fn openapi(
    Extension(document): Extension<Arc<serde_json::Value>>,
) -> Json<serde_json::Value> {
    Json((*document).clone())
}

/// Per-operation OpenAPI metadata: a stable operation ID plus an optional human
/// summary. aide infers request/response/parameter schemas from each handler's
/// signature, so routes only declare identity here.
fn doc(
    id: &'static str,
    summary: &'static str,
) -> impl FnOnce(TransformOperation) -> TransformOperation {
    move |op| {
        let op = op.id(id);
        if summary.is_empty() {
            op
        } else {
            op.summary(summary)
        }
    }
}

/// Operations reachable without authentication; they opt out of the global
/// bearer security requirement.
const PUBLIC_OPERATIONS: &[&str] = &[
    "passwordLogin",
    "refreshAuth",
    "resetPassword",
    "githubAuthStart",
    "githubAuthCallback",
    "githubNativeAuthExchange",
    "issueKeyChallenge",
    "authenticateKey",
];

/// Finish the aide-generated document: pin the OpenAPI version, attach the
/// protocol-version extension and bearer security scheme, register the room and
/// session WebSocket message contracts as components (they are unreachable from
/// any HTTP body), and mark public operations security-optional.
fn finalize_openapi(api: OpenApi) -> serde_json::Value {
    let mut document = serde_json::to_value(api).expect("openapi serializes");
    let obj = document
        .as_object_mut()
        .expect("openapi document is an object");
    obj.insert("openapi".into(), json!("3.1.0"));
    obj.insert("x-sift-protocol-version".into(), json!(PROTOCOL_VERSION));
    obj.insert("security".into(), json!([{ "bearerAuth": [] }]));

    let components = obj
        .entry("components")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .expect("components is an object");
    components
        .entry("securitySchemes")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .expect("securitySchemes is an object")
        .insert(
            "bearerAuth".into(),
            json!({ "type": "http", "scheme": "bearer" }),
        );

    let schemas = components
        .entry("schemas")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .expect("schemas is an object");
    add_component_schema::<WsClientMessage>(schemas);
    add_component_schema::<WsServerMessage>(schemas);
    add_component_schema::<RoomClientMessage>(schemas);
    add_component_schema::<RoomServerMessage>(schemas);
    // The synchronous execute endpoint streams a raw body, so aide cannot infer
    // its response type from the handler signature; register it explicitly as
    // part of the public contract clients decode.
    add_component_schema::<sift_protocol::ExecuteResponse>(schemas);

    if let Some(paths) = obj.get_mut("paths").and_then(|p| p.as_object_mut()) {
        for path_item in paths.values_mut() {
            let Some(methods) = path_item.as_object_mut() else {
                continue;
            };
            for operation in methods.values_mut() {
                let Some(operation) = operation.as_object_mut() else {
                    continue;
                };
                let is_public = operation
                    .get("operationId")
                    .and_then(|id| id.as_str())
                    .map(|id| PUBLIC_OPERATIONS.contains(&id))
                    .unwrap_or(false);
                if is_public {
                    operation.insert("security".into(), json!([]));
                }
            }
        }
    }
    document
}

/// Merge a schemars-generated schema (and its nested definitions) into the
/// OpenAPI `components/schemas` map, rewriting `#/definitions/*` references to
/// the `#/components/schemas/*` form aide uses. Existing aide-generated entries
/// win, so shared nested types are not duplicated.
fn add_component_schema<T: JsonSchema>(schemas: &mut serde_json::Map<String, serde_json::Value>) {
    let root = schema_for!(T);
    let mut entries: Vec<(String, serde_json::Value)> = root
        .definitions
        .into_iter()
        .map(|(name, schema)| {
            (
                name,
                serde_json::to_value(schema).expect("schema serializes"),
            )
        })
        .collect();
    entries.push((
        <T as JsonSchema>::schema_name(),
        serde_json::to_value(root.schema).expect("schema serializes"),
    ));
    for (name, mut value) in entries {
        rewrite_definition_refs(&mut value);
        schemas.entry(name).or_insert(value);
    }
}

/// Recursively rewrite schemars `#/definitions/*` `$ref`s to the OpenAPI
/// `#/components/schemas/*` location.
fn rewrite_definition_refs(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(reference)) = map.get_mut("$ref") {
                if let Some(rest) = reference.strip_prefix("#/definitions/") {
                    *reference = format!("#/components/schemas/{rest}");
                }
            }
            for child in map.values_mut() {
                rewrite_definition_refs(child);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                rewrite_definition_refs(item);
            }
        }
        _ => {}
    }
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
    admit_resolved_tenant(
        &state,
        &auth,
        Some(tenant),
        sift_protocol::RateLimitClass::Control,
        "/v1/metadata/rooms",
    )?;
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
    state.sessions.close_room_connection(room.0).await;
    state.rooms.results().remove_room(room.0);
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
        metadata
            .add_room_member_authorized(
                room,
                actor,
                principal,
                req.role,
                metadata_audit_record(actor, "add_member", "room", Some(room.0)),
            )
            .map_err(Into::into)
    })
    .await?;
    push_metadata_operation_local(&state, actor, "add_member", "room", Some(room.0));
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
        metadata.remove_room_member_authorized(
            room,
            actor,
            principal,
            metadata_audit_record(actor, "remove_member", "room", Some(room.0)),
        )?;
        Ok(())
    })
    .await?;
    push_metadata_operation_local(&state, actor, "remove_member", "room", Some(room.0));
    Ok(Json(json!({"ok": true})))
}

async fn bind_metadata_room_connection(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<BindRoomConnectionRequest>,
) -> ApiResult<Json<sift_metadata::Room>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let room = room_id(id)?;
    let profile = connection_profile_id(req.connection_profile_id)?;
    let actor = auth.principal_id;
    let room_row = metadata_blocking(move || {
        metadata
            .bind_room_connection(
                room,
                actor,
                profile,
                metadata_audit_record(actor, "bind_connection", "room", Some(room.0)),
            )
            .map_err(Into::into)
    })
    .await?;
    // Drop any existing server-owned connection so the next room query
    // reopens under the newly bound profile (ADR-037).
    state.sessions.close_room_connection(room.0).await;
    push_metadata_operation_local(&state, actor, "bind_connection", "room", Some(room.0));
    Ok(Json(room_row))
}

async fn unbind_metadata_room_connection(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<sift_metadata::Room>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let room = room_id(id)?;
    let actor = auth.principal_id;
    let room_row = metadata_blocking(move || {
        metadata
            .unbind_room_connection(
                room,
                actor,
                metadata_audit_record(actor, "unbind_connection", "room", Some(room.0)),
            )
            .map_err(Into::into)
    })
    .await?;
    // Close the room's server-owned connection now that it is unbound.
    state.sessions.close_room_connection(room.0).await;
    push_metadata_operation_local(&state, actor, "unbind_connection", "room", Some(room.0));
    Ok(Json(room_row))
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
        metadata
            .get_room_member(room, principal)?
            .ok_or(ApiError::Forbidden(
                "room membership must be granted by a room owner".into(),
            ))
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
        metadata.leave_room_authorized(
            room,
            principal,
            metadata_audit_record(principal, "leave", "room", Some(room.0)),
        )?;
        Ok(())
    })
    .await?;
    push_metadata_operation_local(&state, principal, "leave", "room", Some(room.0));
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
    let principal = auth.principal_id;
    Ok(Json(
        metadata_blocking(move || {
            metadata
                .list_documents_for_principal(room, principal)
                .map_err(Into::into)
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
        let connection_profile_id = req.connection_profile_id.map(ConnectionProfileId);
        // The server owns the canonical Loro snapshot: build a fresh replica,
        // seed it with any initial text, and persist the snapshot plus its
        // encoded version. Clients no longer choose a backend or ship bytes.
        let replica = sift_doc::TextReplica::new(sift_doc::random_peer_id())
            .map_err(|e| ApiError::Internal(format!("failed to seed document replica: {e}")))?;
        if let Some(text) = req.initial_text.as_deref().filter(|t| !t.is_empty()) {
            replica
                .insert(0, text)
                .map_err(|e| ApiError::BadRequest(format!("invalid initial text: {e}")))?;
        }
        let crdt_state = replica
            .export_snapshot()
            .map_err(|e| ApiError::Internal(format!("failed to export document snapshot: {e}")))?;
        let snapshot_version = replica.version_vector();
        metadata
            .create_document_for_principal(
                room,
                actor,
                NewDocument {
                    kind: req.kind,
                    title: req.title,
                    crdt_state,
                    snapshot_version,
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
        metadata
            .update_document_snapshot_for_principal(document, actor, req.crdt_state)
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
        metadata.delete_document_for_principal(document, actor)?;
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
    admit_resolved_tenant(
        &state,
        &auth,
        Some(tenant),
        sift_protocol::RateLimitClass::Control,
        "/v1/metadata/connections",
    )?;
    let manager = state.sessions.resource_manager();
    let profile_limit = if manager.enforces_for(auth.trusted_local) {
        manager.effective_limits(tenant)?.connection_profiles
    } else {
        None
    };
    let registered = state.sessions.registry().get_provider(&req.provider_id)?;
    let descriptor = registered.provider.descriptor();
    let semantic_engine = registered.provider.legacy_engine().ok_or_else(|| {
        ApiError::BadRequest(format!(
            "provider `{}` uses a dialect not supported by the Phase I SQL policy",
            req.provider_id
        ))
    })?;
    let validator = jsonschema::draft202012::new(&descriptor.configuration_schema)
        .map_err(|error| ApiError::Internal(error.to_string()))?;
    if let Err(error) = validator.validate(&req.configuration) {
        return Err(ApiError::BadRequest(format!(
            "provider configuration is invalid: {error}"
        )));
    }
    let profile = metadata
        .upsert_connection_profile_with_limit(
            tenant,
            auth.principal_id,
            NewConnectionProfile {
                name: req.name,
                provider_id: req.provider_id,
                configuration: req.configuration,
                semantic_engine,
                credentials: req.credentials,
                credential_mode: req.credential_mode,
                tags: req.tags,
            },
            profile_limit,
            metadata_audit_record(auth.principal_id, "upsert", "connection_profile", None),
        )
        .await?;
    push_metadata_operation_local(
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
        .delete_connection_profile(tenant, auth.principal_id, profile, audit)
        .await?;
    state
        .sessions
        .disconnect_managed_profile(tenant, profile)
        .await;
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
            .get_connection_profile_for_principal(profile_id, auth.principal_id)
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
        .set_per_user_credential(profile_id, auth.principal_id, &req.credentials, audit)
        .await?;
    state
        .sessions
        .disconnect_managed_profile_principal(profile_id, auth.principal_id)
        .await;
    push_metadata_operation_local(
        &state,
        auth.principal_id,
        "set_credential",
        "connection_profile",
        Some(profile_id.0),
    );
    Ok(Json(json!({"ok": true})))
}

async fn get_metadata_connection_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<sift_protocol::ConnectionPolicy>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let profile_id = connection_profile_id(id)?;
    let principal = auth.principal_id;
    let profile = metadata_blocking(move || {
        metadata
            .get_connection_profile_for_principal(profile_id, principal)
            .map_err(Into::into)
    })
    .await?;
    state.sessions.push_operation_full(
        Operation::ManageConnectionPolicy {
            action: sift_protocol::PolicyAdminAction::Read,
            tenant_id: profile.tenant_id.0,
            profile_id: profile.id.0,
        },
        OperationStatus::Succeeded,
        Some(principal.0),
        None,
        None,
        None,
    );
    Ok(Json(profile.policy))
}

async fn update_metadata_connection_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(request): Json<UpdateConnectionPolicyRequest>,
) -> ApiResult<Json<sift_protocol::ConnectionPolicy>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let profile_id = connection_profile_id(id)?;
    let principal = auth.principal_id;
    let profile = {
        let lookup = metadata.clone();
        metadata_blocking(move || {
            lookup
                .get_connection_profile_for_principal(profile_id, principal)
                .map_err(Into::into)
        })
        .await?
    };
    let tenant = profile.tenant_id;
    let updated = metadata_blocking(move || {
        metadata
            .update_connection_policy(
                tenant,
                principal,
                profile_id,
                request,
                metadata_audit_record(principal, "update", "connection_policy", Some(profile_id.0)),
            )
            .map_err(Into::into)
    })
    .await?;
    state.sessions.push_operation_local(
        Operation::ManageConnectionPolicy {
            action: sift_protocol::PolicyAdminAction::Update,
            tenant_id: tenant.0,
            profile_id: profile_id.0,
        },
        OperationStatus::Succeeded,
        Some(principal.0),
        None,
        None,
        None,
    );
    Ok(Json(updated.policy))
}

async fn disconnect_metadata_connection(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<sift_protocol::DisconnectManagedConnectionsResponse>> {
    let metadata = metadata_store_cloned(&state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let profile_id = connection_profile_id(id)?;
    let principal = auth.principal_id;
    let profile = metadata_blocking(move || {
        metadata
            .get_connection_profile_for_principal(profile_id, principal)
            .map_err(Into::into)
    })
    .await?;
    if !is_tenant_admin(&auth, profile.tenant_id) {
        return Err(ApiError::Forbidden(
            "tenant administrator access required".into(),
        ));
    }
    let disconnected = state
        .sessions
        .disconnect_managed_profile(profile.tenant_id, profile_id)
        .await;
    state.sessions.push_operation_full(
        Operation::ManageConnectionPolicy {
            action: sift_protocol::PolicyAdminAction::Disconnect,
            tenant_id: profile.tenant_id.0,
            profile_id: profile_id.0,
        },
        OperationStatus::Succeeded,
        Some(principal.0),
        None,
        Some(i64::try_from(disconnected).unwrap_or(i64::MAX)),
        None,
    );
    Ok(Json(sift_protocol::DisconnectManagedConnectionsResponse {
        disconnected: disconnected as u64,
    }))
}

async fn get_tenant_usage(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<sift_protocol::TenantUsageSnapshot>> {
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let tenant = tenant_id(id)?;
    let membership = auth
        .tenants
        .iter()
        .find(|membership| membership.tenant.id == tenant)
        .ok_or_else(|| ApiError::Forbidden("tenant membership required".into()))?;
    if !matches!(
        membership.role,
        sift_metadata::MembershipRole::Owner | sift_metadata::MembershipRole::Admin
    ) {
        return Err(ApiError::Forbidden(
            "tenant administrator access required".into(),
        ));
    }
    Ok(Json(state.sessions.resource_manager().snapshot(tenant)?))
}

async fn set_tenant_limits(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<i64>,
    Json(request): Json<UpdateTenantLimitsRequest>,
) -> ApiResult<Json<sift_metadata::TenantLimitOverride>> {
    ensure_instance_admin(&state, &auth)?;
    let tenant = tenant_id(id)?;
    state
        .sessions
        .resource_manager()
        .validate_override(&request.limits)?;
    let row = metadata_store(&state)?.set_tenant_limit_override(
        auth.principal_id,
        tenant,
        request.limits,
        metadata_audit_record(auth.principal_id, "update", "tenant_limits", Some(tenant.0)),
    )?;
    state.sessions.push_operation_local(
        Operation::ManageTenantLimits {
            action: sift_protocol::PolicyAdminAction::Update,
            tenant_id: tenant.0,
        },
        OperationStatus::Succeeded,
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    Ok(Json(row))
}

async fn clear_tenant_limits(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    ensure_instance_admin(&state, &auth)?;
    let tenant = tenant_id(id)?;
    let cleared = metadata_store(&state)?.clear_tenant_limit_override(
        auth.principal_id,
        tenant,
        metadata_audit_record(auth.principal_id, "clear", "tenant_limits", Some(tenant.0)),
    )?;
    state.sessions.push_operation_local(
        Operation::ManageTenantLimits {
            action: sift_protocol::PolicyAdminAction::Clear,
            tenant_id: tenant.0,
        },
        OperationStatus::Succeeded,
        Some(auth.principal_id.0),
        None,
        None,
        None,
    );
    Ok(Json(json!({"cleared": cleared})))
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
    let principal = auth.principal_id;
    let tenants: Vec<_> = auth.tenants.iter().map(|row| row.tenant.id).collect();
    let sq = metadata_blocking(move || {
        for tenant in tenants {
            match metadata.get_saved_query_visible(sq_id, tenant, principal) {
                Ok(query) => return Ok(query),
                Err(sift_metadata::MetadataError::SavedQueryNotFound(_)) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(ApiError::Metadata(
            sift_metadata::MetadataError::SavedQueryNotFound(sq_id),
        ))
    })
    .await?;
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
    admit_resolved_tenant(
        &state,
        &auth,
        Some(tenant),
        sift_protocol::RateLimitClass::Control,
        "/v1/metadata/saved-queries",
    )?;
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
    let update = UpdateSavedQuery {
        name: req.name,
        sql_text: req.sql_text,
        connection_profile_id: req
            .connection_profile_id
            .map(|opt| opt.map(ConnectionProfileId)),
        tags: req.tags,
    };
    let principal = auth.principal_id;
    let tenants: Vec<_> = auth
        .tenants
        .iter()
        .map(|row| (row.tenant.id, is_tenant_admin(&auth, row.tenant.id)))
        .collect();
    let updated = metadata_blocking(move || {
        for (tenant, admin) in tenants {
            match metadata.update_saved_query_authorized(
                sq_id,
                tenant,
                principal,
                admin,
                update.clone(),
            ) {
                Ok(query) => return Ok(query),
                Err(sift_metadata::MetadataError::SavedQueryNotFound(_)) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(ApiError::Metadata(
            sift_metadata::MetadataError::SavedQueryNotFound(sq_id),
        ))
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
    let principal = auth.principal_id;
    let tenants: Vec<_> = auth
        .tenants
        .iter()
        .map(|row| (row.tenant.id, is_tenant_admin(&auth, row.tenant.id)))
        .collect();
    metadata_blocking(move || {
        for (tenant, admin) in tenants {
            match metadata.delete_saved_query_authorized(sq_id, tenant, principal, admin) {
                Ok(()) => return Ok(()),
                Err(sift_metadata::MetadataError::SavedQueryNotFound(_)) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(ApiError::Metadata(
            sift_metadata::MetadataError::SavedQueryNotFound(sq_id),
        ))
    })
    .await?;
    state.sessions.push_operation(
        Operation::Metadata {
            action: "saved_query.delete".into(),
            target: "saved_query".into(),
            id: Some(sq_id.0),
        },
        OperationStatus::Succeeded,
    );
    Ok(Json(json!({ "ok": true, "deleted": true })))
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
    admit_resolved_tenant(
        &state,
        &auth,
        tenant,
        sift_protocol::RateLimitClass::Control,
        "/v1/auth/tokens",
    )?;
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
    let (configuration, credentials) = metadata
        .resolve_provider_connection(tenant, auth.principal_id, profile_id)
        .await?;
    let info = state
        .sessions
        .open_managed_connection(
            session_id,
            profile.provider_id,
            configuration,
            credentials,
            auth.principal_id,
            tenant,
            profile_id,
            profile.policy.revision,
            auth.trusted_local,
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
        None => OpenSessionRequest {
            tag: None,
            tenant_id: None,
        },
    };
    let auth = session_auth_context_blocking(state.clone(), headers).await?;
    let actor = auth.as_ref().map(|auth| auth.principal_id.0);
    let (owner, tenant, enforce_limits) = match auth.as_ref() {
        Some(auth) => {
            let requested = req.tenant_id.map(tenant_id).transpose()?;
            let tenant = if let Some(tenant) = requested {
                ensure_tenant(auth, tenant)?;
                tenant
            } else if auth.tenants.len() == 1 {
                auth.tenants[0].tenant.id
            } else if auth.trusted_local {
                auth.tenants
                    .first()
                    .map(|membership| membership.tenant.id)
                    .ok_or_else(|| ApiError::Forbidden("local principal has no tenant".into()))?
            } else {
                return Err(ApiError::BadRequest(
                    "tenant_id is required when opening a multi-tenant hosted session".into(),
                ));
            };
            (
                Some(auth.principal_id),
                Some(tenant),
                state
                    .sessions
                    .resource_manager()
                    .enforces_for(auth.trusted_local),
            )
        }
        None => (None, None, false),
    };
    if let Some(auth) = auth.as_ref() {
        admit_resolved_tenant(
            &state,
            auth,
            tenant,
            sift_protocol::RateLimitClass::Control,
            "/v1/sessions",
        )?;
    }
    let info =
        state
            .sessions
            .open_session_with_owner(req.clone(), owner, tenant, enforce_limits)?;
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
    let operation = Operation::OpenConnection {
        session: id,
        request: req.clone(),
    };
    let spec = req.spec;
    let info = state
        .sessions
        .open_provider_connection(id, req.provider_id, spec)
        .await?;
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
    let operation = Operation::PingConnection {
        session: id,
        connection: conn_id,
    };
    let response = finish_operation(
        &state.sessions,
        operation,
        state.sessions.ping(id, conn_id).await,
        |_| None,
    )?;
    Ok(Json(response))
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

#[derive(Deserialize, JsonSchema)]
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
    auth: Option<Extension<AuthContext>>,
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
    let query_guard = state.shutdown.track_query();
    // Routes through the cursor registry (per-session cap + pump), unlike
    // the previous direct driver.execute call. See `export_stream`.
    let stream = finish_operation(
        &state.sessions,
        operation,
        state.sessions.export_stream(id, conn_id, req).await,
        |_| None,
    )?;
    let content_type = crate::export::content_type(format);
    let tenant_id = state
        .sessions
        .managed_tenant_for_session(id)
        .map(|tenant| tenant.0);
    let pacing = auth.map(|Extension(auth)| {
        (
            state.auth.rate_limiter.clone(),
            state.sessions.clone(),
            id,
            auth.principal_id.0,
            tenant_id,
            auth.trusted_local,
        )
    });
    let body = Body::from_stream(pace_http_export(
        stream,
        pacing,
        state.shutdown.clone(),
        query_guard,
    ));
    let mut resp = body.into_response();
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        content_type.parse().unwrap(),
    );
    Ok(resp)
}

fn pace_http_export<S>(
    stream: S,
    pacing: Option<(
        crate::rate_limit::RateLimiter,
        SessionStore,
        sift_protocol::SessionId,
        i64,
        Option<i64>,
        bool,
    )>,
    shutdown: crate::shutdown::Shutdown,
    query_guard: crate::shutdown::QueryGuard,
) -> impl futures::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send + 'static
where
    S: futures::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send + 'static,
{
    async_stream::stream! {
        let _query_guard = query_guard;
        futures::pin_mut!(stream);
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    let mut bytes = bytes;
                    if let Some((limiter, sessions, session, principal, tenant, trusted_local)) = &pacing {
                        let retained = match sessions
                            .reserve_session_retained_bytes(*session, bytes.len())
                        {
                            Ok(guard) => guard,
                            Err(error) => {
                                yield Err(std::io::Error::other(error.to_string()));
                                break;
                            }
                        };
                        bytes = bytes::Bytes::from_owner(RetainedStreamBytes {
                            bytes,
                            _guard: retained,
                        });
                        let paced = tokio::select! {
                            _ = shutdown.wait_for_drain_start() => None,
                            result = limiter.pace_bytes(
                                *principal,
                                *tenant,
                                bytes.len(),
                                *trusted_local,
                                std::time::Duration::from_secs(5),
                            ) => Some(result),
                        };
                        let Some(paced) = paced else {
                            break;
                        };
                        if let Err(retry_after_secs) = paced {
                            sessions.push_operation_full(
                                Operation::RateLimitRejected {
                                    class: sift_protocol::RateLimitClass::StreamBytes,
                                    route: "/v1/sessions/:id/connections/:id/export".into(),
                                    tenant_id: *tenant,
                                },
                                OperationStatus::Failed,
                                Some(*principal),
                                Some("rate_limited".into()),
                                None,
                                Some(format!("retry after {retry_after_secs}s")),
                            );
                            yield Err(std::io::Error::other(format!(
                                "stream byte rate limit exceeded; retry after {retry_after_secs}s"
                            )));
                            break;
                        }
                    }
                    yield Ok(bytes);
                }
                Err(error) => {
                    yield Err(error);
                    break;
                }
            }
        }
    }
}

struct RetainedStreamBytes {
    bytes: bytes::Bytes,
    _guard: Option<crate::resources::ResourceGuard>,
}

impl AsRef<[u8]> for RetainedStreamBytes {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
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
) -> ApiResult<Response> {
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

struct RetainedResponseBytes {
    bytes: Vec<u8>,
    _guard: Option<crate::resources::ResourceGuard>,
}

impl AsRef<[u8]> for RetainedResponseBytes {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

fn retained_json_response(
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

async fn list_room_results(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<RoomQueryResult>>> {
    authorize_room_result_read(&state, headers, id).await?;
    Ok(Json(state.rooms.results().list(id)))
}

async fn get_room_result(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, result_id)): Path<(i64, uuid::Uuid)>,
) -> ApiResult<Json<RoomQueryResult>> {
    authorize_room_result_read(&state, headers, id).await?;
    let result_id = sift_protocol::RoomResultId(result_id);
    let result = state.rooms.results().get(id, result_id);
    finish_operation(
        &state.sessions,
        Operation::ReadSharedResult {
            room_id: id,
            result_id,
        },
        result,
        |_| None,
    )
    .map(Json)
}

async fn get_room_result_pages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, result_id)): Path<(i64, uuid::Uuid)>,
    Query(query): Query<RoomResultPagesQuery>,
) -> ApiResult<Json<sift_protocol::RoomResultPages>> {
    authorize_room_result_read(&state, headers, id).await?;
    let result_id = sift_protocol::RoomResultId(result_id);
    let registry = state.rooms.results().clone();
    let result = tokio::task::spawn_blocking(move || {
        registry.pages(id, result_id, query.from_seq, query.limit)
    })
    .await
    .map_err(|error| ApiError::Internal(format!("room result read task failed: {error}")))?;
    finish_operation(
        &state.sessions,
        Operation::ReadSharedResult {
            room_id: id,
            result_id,
        },
        result,
        |pages| Some(pages.pages.len() as i64),
    )
    .map(Json)
}

async fn authorize_room_result_read(
    state: &AppState,
    headers: HeaderMap,
    id: i64,
) -> ApiResult<()> {
    let metadata = metadata_store_cloned(state)?;
    let auth = resolve_auth_context_blocking(state.clone(), headers).await?;
    let room = room_id(id)?;
    metadata_blocking({
        let metadata = metadata.clone();
        let auth = auth.clone();
        move || ensure_room_permission(&metadata, &auth, room, RoomPermission::Read).map(|_| ())
    })
    .await?;
    Ok(())
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
            let room = room_row.id;
            if let Err(error) = handle_room_ws(
                state.clone(),
                metadata,
                auth,
                room,
                room_row.tenant_id,
                socket,
            )
            .await
            {
                tracing::warn!(room_id = %room.0, error = %error, "room websocket ended with error");
            }
            // handle_room_ws has dropped its subscription and attachment; if
            // that emptied the room, close its server-owned connection so an
            // abandoned room does not hold a database connection open. A later
            // join lazily reopens it, so a race with a rejoin self-heals
            // (ADR-037 teardown).
            if !state.rooms.is_active(room.0) {
                state.sessions.close_room_connection(room.0).await;
            }
        })
    }))
}

async fn handle_room_ws(
    state: AppState,
    metadata: MetadataStore,
    mut auth: AuthContext,
    room: RoomId,
    tenant: TenantId,
    socket: WebSocket,
) -> ApiResult<()> {
    let (mut sender, mut receiver) = socket.split();
    let mut subscription = state.rooms.subscribe(room.0);
    let mut attachment_id = None;
    // Releases every live-writer lease this connection acquired when it ends.
    let mut leases = crate::document_registry::LeaseGuard::new(state.rooms.clone());
    let mut lease_tick = tokio::time::interval(std::time::Duration::from_secs(1));

    loop {
        let (presence_rx, doc_rx) = subscription.receivers();
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
                if let Err(error) = admit_resolved_tenant(
                    &state,
                    &auth,
                    Some(tenant),
                    sift_protocol::RateLimitClass::Control,
                    "/v1/metadata/rooms/:id/ws",
                ) {
                    let ApiError::RateLimited { retry_after_secs } = error else {
                        return Err(error);
                    };
                    send_json(
                        &mut sender,
                        &RoomServerMessage::RateLimited {
                            retry_after_ms: retry_after_secs.saturating_mul(1_000),
                        },
                    )
                    .await?;
                    continue;
                }
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
                        let (attachment, presence) =
                            state.rooms.attach(room.0, auth.principal_id.0, client_id.clone());
                        let id = attachment.id();
                        attachment_id = Some(AuditedRoomAttachment {
                            attachment: Some(attachment),
                            sessions: state.sessions.clone(),
                            room_id: room.0,
                        });
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
                    RoomClientMessage::PresenceHeartbeat | RoomClientMessage::PresencePing => {
                        if let Some(attachment) = attachment_id.as_ref() {
                            state.rooms.heartbeat(room.0, attachment.id());
                        }
                        send_json(&mut sender, &RoomServerMessage::Presence {
                            presence: state.rooms.presence(room.0),
                        }).await?;
                    }
                    RoomClientMessage::PresenceUpdate {
                        active_document_id,
                        selection,
                    } => {
                        let Some(attachment) = attachment_id.as_ref() else {
                            send_json(&mut sender, &RoomServerMessage::Error {
                                message: "attach before updating presence".into(),
                            }).await?;
                            continue;
                        };
                        if !state.rooms.update_presence(
                            room.0,
                            attachment.id(),
                            active_document_id,
                            selection,
                        ) {
                            send_json(&mut sender, &RoomServerMessage::Error {
                                message: "presence lease expired; attach again".into(),
                            }).await?;
                        }
                    }
                    RoomClientMessage::DocumentSync {
                        request_id,
                        document_id: raw_document_id,
                        replica_id: _,
                        known_version,
                    } => {
                        if attachment_id.is_none() {
                            send_json(&mut sender, &RoomServerMessage::Error {
                                message: "attach before synchronizing documents".into(),
                            }).await?;
                            continue;
                        }
                        handle_document_sync(
                            &state,
                            &metadata,
                            &mut sender,
                            room,
                            request_id,
                            raw_document_id,
                            known_version,
                        ).await?;
                    }
                    RoomClientMessage::DocumentUpdate {
                        request_id,
                        update_id,
                        document_id: raw_document_id,
                        replica_id,
                        update,
                    } => {
                        if attachment_id.is_none() {
                            send_json(&mut sender, &RoomServerMessage::Error {
                                message: "attach before submitting document updates".into(),
                            }).await?;
                            continue;
                        }
                        handle_document_update(
                            &state,
                            &metadata,
                            &mut sender,
                            &mut leases,
                            room,
                            &auth,
                            request_id,
                            update_id,
                            raw_document_id,
                            replica_id,
                            update,
                        ).await?;
                    }
                }
            }
            event = presence_rx.recv() => {
                match event {
                    Ok(message) => send_json(&mut sender, &message).await?,
                    // Ephemeral lane: a lagged consumer heals with a fresh
                    // presence snapshot; nothing durable is lost.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        send_json(&mut sender, &RoomServerMessage::Presence {
                            presence: state.rooms.presence(room.0),
                        }).await?;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            event = doc_rx.recv() => {
                match event {
                    Ok(message) => send_json(&mut sender, &message).await?,
                    // Durable lane: a dropped committed op cannot be replayed
                    // from the ring, so force a CRDT resync. The client
                    // re-issues DocumentSync from its version vector and Loro
                    // merges the delta idempotently.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        send_json(&mut sender, &RoomServerMessage::ResyncRequired {
                            runtime_epoch: state.rooms.documents().runtime_epoch().to_string(),
                            event_seq: state.rooms.documents().current_event_seq(),
                        }).await?;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            _ = lease_tick.tick() => {
                let expired = state.rooms.expire_presence(room.0);
                if attachment_id
                    .as_ref()
                    .is_some_and(|attachment| expired.contains(&attachment.id()))
                {
                    send_json(&mut sender, &RoomServerMessage::Error {
                        message: "presence lease expired; attach again".into(),
                    }).await?;
                    break;
                }
                if !ws_lease_is_valid(&state, &auth, Some(room))? {
                    send_json(&mut sender, &RoomServerMessage::Error {
                        message: "authentication lease or room membership was revoked".into(),
                    }).await?;
                    break;
                }
            }
        }
    }

    Ok(())
}

/// Decoded-byte size of each chunk in a document snapshot/update transfer.
const SYNC_CHUNK_BYTES: usize = 256 * 1024;

/// Map a durable-apply error onto a structured room error message.
fn document_error(
    request_id: Option<String>,
    document_id: i64,
    err: &crate::document_actor::ApplyError,
) -> RoomServerMessage {
    use crate::document_actor::ApplyError;
    use sift_protocol::DocumentErrorCode as Code;
    let code = match err {
        ApplyError::InvalidUpdate(_) => Code::InvalidCrdtUpdate,
        ApplyError::DependenciesMissing => Code::CrdtDependenciesMissing,
        ApplyError::DocumentTooLarge => Code::DocumentTooLarge,
        ApplyError::Doc(sift_doc::DocError::VersionNotFound) => Code::DocumentVersionNotFound,
        ApplyError::Metadata(sift_metadata::MetadataError::DocumentNotFound(_)) => Code::NotFound,
        _ => Code::Internal,
    };
    RoomServerMessage::DocumentError {
        request_id,
        document_id,
        code,
        message: err.to_string(),
    }
}

/// Split `bytes` into 256 KiB chunks and stream them as one transfer. An empty
/// payload still sends a single empty chunk so the client sees the transfer.
#[allow(clippy::too_many_arguments)]
async fn send_document_transfer(
    sender: &mut futures::stream::SplitSink<WebSocket, Message>,
    request_id: &str,
    document_id: i64,
    transfer_id: &str,
    payload_kind: sift_protocol::DocumentTransferKind,
    bytes: &[u8],
    snapshot_seq: i64,
    server_version: &[u8],
) -> ApiResult<()> {
    let chunks: Vec<&[u8]> = if bytes.is_empty() {
        vec![&[]]
    } else {
        bytes.chunks(SYNC_CHUNK_BYTES).collect()
    };
    let count = chunks.len() as u32;
    for (index, chunk) in chunks.into_iter().enumerate() {
        send_json(
            sender,
            &RoomServerMessage::DocumentChunk {
                request_id: request_id.to_string(),
                document_id,
                transfer_id: transfer_id.to_string(),
                index: index as u32,
                count,
                payload_kind,
                payload: sift_protocol::CrdtUpdate::new(chunk.to_vec()),
                snapshot_seq,
                server_version: sift_protocol::DocumentVersion::new(server_version.to_vec()),
            },
        )
        .await?;
    }
    Ok(())
}

struct SyncPlan {
    kind: sift_protocol::DocumentTransferKind,
    bytes: Vec<u8>,
    server_version: Vec<u8>,
    snapshot_seq: i64,
}

/// Answer a `DocumentSync`: send the snapshot (new replica) or the missing
/// update range (known replica), then a terminal `DocumentSynced`.
async fn handle_document_sync(
    state: &AppState,
    metadata: &MetadataStore,
    sender: &mut futures::stream::SplitSink<WebSocket, Message>,
    room: RoomId,
    request_id: String,
    raw_document_id: i64,
    known_version: sift_protocol::DocumentVersion,
) -> ApiResult<()> {
    let document = document_id(raw_document_id)?;
    let md = metadata.clone();
    let rooms = state.rooms.clone();
    let known = known_version.into_bytes();
    let room_id = room.0;
    let planned = tokio::task::spawn_blocking(
        move || -> Result<SyncPlan, crate::document_actor::ApplyError> {
            let row = md.get_document(document)?;
            if row.room_id.0 != room_id {
                return Err(crate::document_actor::ApplyError::Metadata(
                    sift_metadata::MetadataError::DocumentNotFound(document),
                ));
            }
            let actor = rooms.documents().get_or_load(&md, document)?;
            let guard = actor.lock().expect("document actor mutex poisoned");
            let server_version = guard.version_vector();
            let (kind, bytes) = if known.is_empty() {
                (
                    sift_protocol::DocumentTransferKind::Snapshot,
                    guard.snapshot()?,
                )
            } else {
                (
                    sift_protocol::DocumentTransferKind::Update,
                    guard.updates_since(&known)?,
                )
            };
            Ok(SyncPlan {
                kind,
                bytes,
                server_version,
                snapshot_seq: row.snapshot_seq,
            })
        },
    )
    .await
    .map_err(|e| ApiError::Internal(format!("document sync task failed: {e}")))?;

    let plan = match planned {
        Ok(plan) => plan,
        Err(err) => {
            send_json(
                sender,
                &document_error(Some(request_id), raw_document_id, &err),
            )
            .await?;
            return Ok(());
        }
    };
    let transfer_id = uuid::Uuid::new_v4().to_string();
    send_document_transfer(
        sender,
        &request_id,
        raw_document_id,
        &transfer_id,
        plan.kind,
        &plan.bytes,
        plan.snapshot_seq,
        &plan.server_version,
    )
    .await?;
    send_json(
        sender,
        &RoomServerMessage::DocumentSynced {
            request_id,
            document_id: raw_document_id,
            server_version: sift_protocol::DocumentVersion::new(plan.server_version),
        },
    )
    .await?;
    Ok(())
}

/// Durably apply a `DocumentUpdate`: enforce editor role and the writer lease,
/// commit through the document actor, then ACK the submitter and broadcast the
/// committed update to the room.
#[allow(clippy::too_many_arguments)]
async fn handle_document_update(
    state: &AppState,
    metadata: &MetadataStore,
    sender: &mut futures::stream::SplitSink<WebSocket, Message>,
    leases: &mut crate::document_registry::LeaseGuard,
    room: RoomId,
    auth: &AuthContext,
    request_id: String,
    update_id: String,
    raw_document_id: i64,
    replica_id: sift_protocol::ReplicaId,
    update: sift_protocol::CrdtUpdate,
) -> ApiResult<()> {
    let document = document_id(raw_document_id)?;
    let principal = auth.principal_id;

    // Editor/owner role and document-in-room, re-checked on every update.
    let md = metadata.clone();
    let room_id = room.0;
    let role = tokio::task::spawn_blocking(move || -> ApiResult<sift_metadata::RoomRole> {
        let row = md.get_document(document)?;
        if row.room_id.0 != room_id {
            return Err(ApiError::Forbidden("document is not in this room".into()));
        }
        let member = md
            .get_room_member(RoomId(room_id), principal)?
            .ok_or_else(|| ApiError::Forbidden("not a room member".into()))?;
        Ok(member.role)
    })
    .await
    .map_err(|e| ApiError::Internal(format!("room role task failed: {e}")))??;
    if matches!(role, sift_metadata::RoomRole::Viewer) {
        send_json(
            sender,
            &RoomServerMessage::DocumentError {
                request_id: Some(request_id),
                document_id: raw_document_id,
                code: sift_protocol::DocumentErrorCode::Forbidden,
                message: "viewers cannot submit document updates".into(),
            },
        )
        .await?;
        return Ok(());
    }

    // One live writer per (document, replica).
    let replica_str = replica_id.to_string();
    if !leases.ensure(raw_document_id, &replica_str) {
        send_json(
            sender,
            &RoomServerMessage::DocumentError {
                request_id: Some(request_id),
                document_id: raw_document_id,
                code: sift_protocol::DocumentErrorCode::ReplicaInUse,
                message: "another live connection is writing as this replica".into(),
            },
        )
        .await?;
        return Ok(());
    }

    let md = metadata.clone();
    let rooms = state.rooms.clone();
    let update_bytes = update.into_bytes();
    // Peers import the exact bytes the submitter sent; keep a copy for broadcast.
    let broadcast_bytes = update_bytes.clone();
    let apply_replica = replica_str.clone();
    let apply_update_id = update_id.clone();
    let applied = tokio::task::spawn_blocking(
        move || -> Result<(crate::document_actor::ApplyOutcome, Vec<u8>), crate::document_actor::ApplyError> {
            let actor = rooms.documents().get_or_load(&md, document)?;
            let mut guard = actor.lock().expect("document actor mutex poisoned");
            let outcome = guard.apply_update(
                &md,
                principal,
                &apply_replica,
                &apply_update_id,
                &update_bytes,
            )?;
            if guard.should_compact() {
                // Best-effort: a failed compaction leaves the log intact.
                let _ = guard.compact(&md);
            }
            let version = guard.version_vector();
            Ok((outcome, version))
        },
    )
    .await
    .map_err(|e| ApiError::Internal(format!("document apply task failed: {e}")))?;

    let (outcome, server_version) = match applied {
        Ok(result) => result,
        Err(err) => {
            send_json(
                sender,
                &document_error(Some(request_id), raw_document_id, &err),
            )
            .await?;
            return Ok(());
        }
    };

    match outcome {
        crate::document_actor::ApplyOutcome::Idempotent => {
            send_json(
                sender,
                &RoomServerMessage::DocumentUpdateAck {
                    request_id,
                    update_id,
                    document_id: raw_document_id,
                    server_seq: -1,
                    version_fingerprint: String::new(),
                },
            )
            .await?;
        }
        crate::document_actor::ApplyOutcome::Applied {
            server_seq,
            version_fingerprint,
        } => {
            // ACK the submitter, then broadcast the committed update.
            send_json(
                sender,
                &RoomServerMessage::DocumentUpdateAck {
                    request_id,
                    update_id: update_id.clone(),
                    document_id: raw_document_id,
                    server_seq,
                    version_fingerprint,
                },
            )
            .await?;
            state.rooms.publish_doc(
                room.0,
                RoomServerMessage::DocumentUpdateCommitted {
                    document_id: raw_document_id,
                    replica_id,
                    server_seq,
                    update: sift_protocol::CrdtUpdate::new(broadcast_bytes),
                    server_version: sift_protocol::DocumentVersion::new(server_version),
                },
            );
            state.sessions.push_operation(
                Operation::ApplyDocumentUpdate {
                    room_id: room.0,
                    document_id: raw_document_id,
                    update_id,
                    server_seq,
                },
                OperationStatus::Succeeded,
            );
        }
    }
    Ok(())
}

struct AuditedRoomAttachment {
    attachment: Option<crate::room_runtime::RoomAttachment>,
    sessions: SessionStore,
    room_id: i64,
}

impl AuditedRoomAttachment {
    fn id(&self) -> i64 {
        self.attachment
            .as_ref()
            .map(crate::room_runtime::RoomAttachment::id)
            .unwrap_or_default()
    }
}

impl Drop for AuditedRoomAttachment {
    fn drop(&mut self) {
        let Some(attachment) = self.attachment.take() else {
            return;
        };
        let attachment_id = attachment.id();
        attachment.detach();
        self.sessions.push_operation(
            Operation::DetachRoom {
                room_id: self.room_id,
                attachment_id,
            },
            OperationStatus::Succeeded,
        );
    }
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
        trusted_local: false,
    })
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
                        if let Err(retry_after_secs) = ws_rate_admit(
                            &state,
                            auth.as_ref(),
                            session_id,
                            sift_protocol::RateLimitClass::Query,
                        ) {
                            send_rate_limited(&mut sender, Some(request_id), retry_after_secs)
                                .await?;
                            continue;
                        }
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
                            stream.rows,
                            WsPageContext {
                                sessions: &state.sessions,
                                session_id,
                                connection,
                                cursor_id: stream.cursor_id,
                                rate_limiter: &state.auth.rate_limiter,
                                auth: auth.as_ref(),
                                shutdown: &state.shutdown,
                            },
                        )
                        .await?;
                    }
                    WsClientMessage::Listen {
                        request_id,
                        connection,
                        channels,
                    } => {
                        if let Err(retry_after_secs) = ws_rate_admit(
                            &state,
                            auth.as_ref(),
                            session_id,
                            sift_protocol::RateLimitClass::Interactive,
                        ) {
                            send_rate_limited(&mut sender, Some(request_id), retry_after_secs)
                                .await?;
                            continue;
                        }
                        let operation = Operation::Listen {
                            session: session_id,
                            connection,
                        };
                        let stream = match state
                            .sessions
                            .listen_pg(session_id, connection, channels)
                            .await
                        {
                            Ok(stream) => stream,
                            Err(error) => {
                                state.sessions.push_operation_full(
                                    operation,
                                    OperationStatus::Failed,
                                    None,
                                    None,
                                    None,
                                    Some(error.to_string()),
                                );
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
                        state
                            .sessions
                            .push_operation(operation, OperationStatus::Succeeded);
                        stream_notifications(&mut sender, request_id, stream.notifications).await?;
                    }
                    WsClientMessage::Cancel {
                        connection,
                        cursor_id,
                    } => {
                        if let Err(retry_after_secs) = ws_rate_admit(
                            &state,
                            auth.as_ref(),
                            session_id,
                            sift_protocol::RateLimitClass::Control,
                        ) {
                            send_rate_limited(&mut sender, None, retry_after_secs).await?;
                            continue;
                        }
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

fn ws_rate_admit(
    state: &AppState,
    auth: Option<&AuthContext>,
    session_id: sift_protocol::SessionId,
    class: sift_protocol::RateLimitClass,
) -> Result<(), u64> {
    let Some(auth) = auth else {
        return Ok(());
    };
    let tenant = state
        .sessions
        .managed_tenant_for_session(session_id)
        .or_else(|| (auth.tenants.len() == 1).then(|| auth.tenants[0].tenant.id));
    let result = state.auth.rate_limiter.admit(
        auth.principal_id.0,
        tenant.map(|tenant| tenant.0),
        class,
        auth.trusted_local,
    );
    if let Err(retry_after_secs) = result {
        record_rate_rejection(
            state,
            auth,
            class,
            "/v1/sessions/:id/ws",
            tenant,
            retry_after_secs,
        );
    }
    result
}

async fn send_rate_limited(
    sender: &mut futures::stream::SplitSink<WebSocket, Message>,
    request_id: Option<String>,
    retry_after_secs: u64,
) -> ApiResult<()> {
    send_json(
        sender,
        &WsServerMessage::Error {
            request_id,
            code: Some(sift_protocol::Code::RateLimited),
            retry_after_ms: Some(retry_after_secs.saturating_mul(1_000)),
            message: "rate limit exceeded".into(),
        },
    )
    .await
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
    mut rows: tokio::sync::mpsc::Receiver<sift_protocol::Page>,
    context: WsPageContext<'_>,
) -> ApiResult<()> {
    let WsPageContext {
        sessions,
        session_id,
        connection,
        cursor_id,
        rate_limiter,
        auth,
        shutdown,
    } = context;
    let mut seq = 0_u64;
    while let Some(page) = rows.recv().await {
        sessions.cursor_page_received(cursor_id);
        let page_bytes = serde_json::to_vec(&page)
            .map_err(|error| ApiError::Internal(error.to_string()))?
            .len();
        if let Some(auth) = auth {
            let tenant = sessions
                .managed_tenant_for_session(session_id)
                .or_else(|| (auth.tenants.len() == 1).then(|| auth.tenants[0].tenant.id));
            let paced = tokio::select! {
                _ = shutdown.wait_for_drain_start() => None,
                result = rate_limiter.pace_bytes(
                    auth.principal_id.0,
                    tenant.map(|tenant| tenant.0),
                    page_bytes,
                    auth.trusted_local,
                    std::time::Duration::from_secs(5),
                ) => Some(result),
            };
            let Some(paced) = paced else {
                let _ = sessions.cancel(session_id, connection, cursor_id).await;
                sessions.cursor_remove(cursor_id);
                break;
            };
            if let Err(retry_after_secs) = paced {
                sessions.push_operation_full(
                    Operation::RateLimitRejected {
                        class: sift_protocol::RateLimitClass::StreamBytes,
                        route: "/v1/sessions/:id/ws".into(),
                        tenant_id: tenant.map(|tenant| tenant.0),
                    },
                    OperationStatus::Failed,
                    Some(auth.principal_id.0),
                    Some("rate_limited".into()),
                    None,
                    Some(format!("retry after {retry_after_secs}s")),
                );
                send_rate_limited(sender, None, retry_after_secs).await?;
                let _ = sessions.cancel(session_id, connection, cursor_id).await;
                sessions.cursor_remove(cursor_id);
                break;
            }
        }
        let terminal = matches!(
            &page,
            sift_protocol::Page::Done { .. } | sift_protocol::Page::Error { .. }
        );
        let _response_guard = match sessions.reserve_session_retained_bytes(session_id, page_bytes)
        {
            Ok(guard) => guard,
            Err(error) => {
                let _ = sessions.cancel(session_id, connection, cursor_id).await;
                sessions.cursor_remove(cursor_id);
                return Err(error);
            }
        };
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
                sessions.cursor_page_processed(cursor_id);
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

struct WsPageContext<'a> {
    sessions: &'a SessionStore,
    session_id: sift_protocol::SessionId,
    connection: sift_protocol::ConnectionId,
    cursor_id: sift_protocol::CursorId,
    rate_limiter: &'a crate::rate_limit::RateLimiter,
    auth: Option<&'a AuthContext>,
    shutdown: &'a crate::shutdown::Shutdown,
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

    #[tokio::test]
    async fn http_export_stream_applies_byte_pacing() {
        let limiter =
            crate::rate_limit::RateLimiter::from_config(&crate::config::RateLimitsConfig {
                trusted_local_exempt: false,
                stream_bytes: Some(crate::config::RateBucketConfig {
                    refill_per_second: 1.0,
                    burst: 3.0,
                    cost: 1.0,
                }),
                ..Default::default()
            });
        let shutdown = crate::shutdown::Shutdown::default();
        let stream = futures::stream::iter([Ok(bytes::Bytes::from_static(b"four"))]);
        let sessions = SessionStore::new(crate::registry::DriverRegistry::new());
        let session = sessions.open_session(OpenSessionRequest {
            tag: None,
            tenant_id: None,
        });
        let paced = pace_http_export(
            stream,
            Some((limiter, sessions, session.id, 1, Some(1), false)),
            shutdown.clone(),
            shutdown.track_query(),
        );
        futures::pin_mut!(paced);

        assert!(paced.next().await.unwrap().is_err());
        assert!(paced.next().await.is_none());
        assert_eq!(shutdown.in_flight(), 0);
    }

    #[test]
    fn serialized_response_bytes_hold_quota_until_last_bytes_clone_drops() {
        let sessions = SessionStore::new(crate::registry::DriverRegistry::new());
        let tenant = TenantId(5);
        let limits = crate::config::TenantLimitsConfig {
            trusted_local_unlimited: false,
            defaults: sift_protocol::TenantResourceLimits {
                retained_result_bytes: Some(16),
                ..Default::default()
            },
            ceilings: sift_protocol::TenantResourceLimits {
                retained_result_bytes: Some(16),
                ..Default::default()
            },
        };
        let manager = crate::resources::ResourceManager::new(&limits, None);
        sessions.set_resource_manager(manager.clone());
        let session = sessions
            .open_session_with_owner(
                OpenSessionRequest {
                    tag: None,
                    tenant_id: Some(tenant.0),
                },
                Some(PrincipalId(1)),
                Some(tenant),
                true,
            )
            .unwrap();
        let guard = sessions
            .reserve_session_retained_bytes(session.id, 5)
            .unwrap();
        let bytes = bytes::Bytes::from_owner(RetainedResponseBytes {
            bytes: b"12345".to_vec(),
            _guard: guard,
        });
        let clone = bytes.clone();
        assert_eq!(
            manager
                .snapshot(tenant)
                .unwrap()
                .usage
                .retained_result_bytes,
            5
        );
        drop(bytes);
        assert_eq!(
            manager
                .snapshot(tenant)
                .unwrap()
                .usage
                .retained_result_bytes,
            5
        );
        drop(clone);
        assert_eq!(
            manager
                .snapshot(tenant)
                .unwrap()
                .usage
                .retained_result_bytes,
            0
        );
    }
}
