//! `sift-client-sdk` — thin reference consumer proving the HTTP API is
//! buildable-against from outside the server crate.

// Request/response DTOs shared with the server. Re-export so downstream
// consumers can build requests without depending on sift_metadata::http
// directly.
pub use sift_metadata::http::{
    AddRoomMemberRequest, CreateDocumentRequest, CreateRoomRequest, CreateSavedQueryRequest,
    IssueTokenRequest, IssueTokenResponse, OpenConnectionFromProfileRequest, SetCredentialRequest,
    UpdateDocumentSnapshotRequest, UpdateSavedQueryRequest, UpsertConnectionProfileRequest,
};
use sift_metadata::{
    ApiTokenId, ConnectionProfile, ConnectionProfileId, Document, DocumentId, GithubAllowlistEntry,
    PrincipalKey, QueryHistory, Room, RoomId, RoomMember, SavedQuery, SavedQueryId,
    SavedQueryScope, TenantId, TenantInvitation, TenantMembership,
};
use sift_protocol::{
    AcceptTenantInvitationRequest, AdminCreatePasswordPrincipalRequest,
    AdminLinkPasswordIdentityRequest, AdminSetPrincipalDisabledRequest, ApplyEditsRequest,
    ApplyEditsResult, AuthIdentitySummary, AuthPrincipal, AuthSessionSummary, AuthTokensResponse,
    BeginTransactionRequest, BulkInsertRequest, BulkInsertResponse, CancelRequest,
    ChangePasswordRequest, ConnectionId, ConnectionInfo, CreateGithubAllowlistRequest,
    CreateTenantInvitationRequest, CsvImportRequest, CsvImportResponse, CursorId,
    DataSearchRequest, DataSearchResponse, DatabaseProcess, EditPlan, EndTransactionRequest,
    ExecuteRequestHttp, ExecuteResponse, ExplainRequest, ExplainResponse, Health,
    IssuedPasswordResetResponse, IssuedTenantInvitationResponse, KeyAuthenticateRequest,
    KeyChallengeRequest, KeyChallengeResponse, KillProcessRequest, KillProcessResponse,
    OpenConnectionRequest, OpenSessionRequest, OperationCapability, OperationCapabilityContext,
    Page, PasswordLoginRequest, PasswordResetRequest, PreviewEditsRequest, Readiness,
    RefreshAuthRequest, RegisterPrincipalKeyRequest, SavepointRequest, SchemaSearchRequest,
    SchemaSearchResponse, SchemaSnapshot, ServerInfo, SessionId, SessionInfo,
    TextDocumentOperation, TransactionEndAction, TransactionInfo, TransactionPreview,
    TransactionPreviewRequest, TransactionState, TxHandleRef, TxId, TxMode, Value, WebAuthResponse,
    WhoAmIResponse, WsClientMessage, WsServerMessage,
};

#[derive(Clone)]
pub struct SessionTokenProvider {
    tokens: std::sync::Arc<tokio::sync::RwLock<AuthTokensResponse>>,
}

impl std::fmt::Debug for SessionTokenProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionTokenProvider")
            .field("tokens", &"[REDACTED]")
            .finish()
    }
}

impl SessionTokenProvider {
    pub fn new(tokens: AuthTokensResponse) -> Self {
        Self {
            tokens: std::sync::Arc::new(tokio::sync::RwLock::new(tokens)),
        }
    }

    async fn access_token(&self) -> String {
        self.tokens.read().await.access_token.clone()
    }

    async fn refresh_token(&self) -> String {
        self.tokens.read().await.refresh_token.clone()
    }

    async fn replace(&self, tokens: AuthTokensResponse) {
        *self.tokens.write().await = tokens;
    }

    pub async fn reauthenticate_session_websocket(
        &self,
        socket: &mut SessionWebSocket,
    ) -> Result<chrono::DateTime<chrono::Utc>> {
        socket.reauthenticate(self.access_token().await).await
    }

    pub async fn reauthenticate_room_websocket(
        &self,
        socket: &mut RoomWebSocket,
    ) -> Result<chrono::DateTime<chrono::Utc>> {
        socket.reauthenticate(self.access_token().await).await
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("http transport error: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("server error {status}: {body}")]
    Server {
        status: reqwest::StatusCode,
        body: String,
    },
    #[error("websocket error: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone)]
pub struct Client {
    base: String,
    token: Option<String>,
    session_tokens: Option<SessionTokenProvider>,
    http: reqwest::Client,
}

type TransportWebSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

pub struct SessionWebSocket {
    socket: TransportWebSocket,
}

impl SessionWebSocket {
    pub async fn send(&mut self, message: WsClientMessage) -> Result<()> {
        use futures::SinkExt;
        use tokio_tungstenite::tungstenite::Message;

        self.socket
            .send(Message::Text(serde_json::to_string(&message)?.into()))
            .await?;
        Ok(())
    }

    pub async fn next(&mut self) -> Result<WsServerMessage> {
        next_ws(&mut self.socket).await
    }

    pub async fn reauthenticate(
        &mut self,
        access_token: impl Into<String>,
    ) -> Result<chrono::DateTime<chrono::Utc>> {
        self.send(WsClientMessage::Reauthenticate {
            access_token: sift_protocol::RedactedString(access_token.into()),
        })
        .await?;
        match self.next().await? {
            WsServerMessage::Authenticated { expires_at } => Ok(expires_at),
            WsServerMessage::Error { message, .. } => Err(Error::Protocol(message)),
            other => Err(Error::Protocol(format!(
                "expected WebSocket authentication acknowledgement, got {other:?}"
            ))),
        }
    }
}

pub struct RoomWebSocket {
    socket: TransportWebSocket,
}

impl RoomWebSocket {
    pub async fn send(&mut self, message: sift_protocol::RoomClientMessage) -> Result<()> {
        use futures::SinkExt;
        use tokio_tungstenite::tungstenite::Message;

        self.socket
            .send(Message::Text(serde_json::to_string(&message)?.into()))
            .await?;
        Ok(())
    }

    pub async fn next(&mut self) -> Result<sift_protocol::RoomServerMessage> {
        next_room_ws(&mut self.socket).await
    }

    pub async fn reauthenticate(
        &mut self,
        access_token: impl Into<String>,
    ) -> Result<chrono::DateTime<chrono::Utc>> {
        self.send(sift_protocol::RoomClientMessage::Reauthenticate {
            access_token: sift_protocol::RedactedString(access_token.into()),
        })
        .await?;
        match self.next().await? {
            sift_protocol::RoomServerMessage::Authenticated { expires_at } => Ok(expires_at),
            sift_protocol::RoomServerMessage::Error { message } => Err(Error::Protocol(message)),
            other => Err(Error::Protocol(format!(
                "expected room WebSocket authentication acknowledgement, got {other:?}"
            ))),
        }
    }
}

impl Client {
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            token: None,
            session_tokens: None,
            http: reqwest::Client::new(),
        }
    }

    pub fn with_bearer_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self.session_tokens = None;
        self
    }

    pub fn with_session_tokens(mut self, provider: SessionTokenProvider) -> Self {
        self.token = None;
        self.session_tokens = Some(provider);
        self
    }

    pub async fn password_login(
        &self,
        request: PasswordLoginRequest,
    ) -> Result<SessionTokenProvider> {
        let response = self
            .http
            .post(self.url("/v1/auth/login"))
            .json(&request)
            .send()
            .await?;
        let tokens = response_json(response).await?;
        Ok(SessionTokenProvider::new(tokens))
    }

    pub async fn refresh_session(&self) -> Result<()> {
        let provider = self.session_tokens.as_ref().ok_or_else(|| {
            Error::Protocol("client has no interactive session token provider".into())
        })?;
        let response = self
            .http
            .post(self.url("/v1/auth/refresh"))
            .json(&RefreshAuthRequest {
                refresh_token: Some(provider.refresh_token().await),
            })
            .send()
            .await?;
        let tokens = response_json(response).await?;
        provider.replace(tokens).await;
        Ok(())
    }

    pub async fn whoami(&self) -> Result<WhoAmIResponse> {
        self.get("/v1/auth/whoami").await
    }

    pub async fn logout(&self) -> Result<()> {
        let _: serde_json::Value = self.post_empty("/v1/auth/logout").await?;
        Ok(())
    }

    pub async fn logout_all(&self) -> Result<()> {
        let _: serde_json::Value = self.post_empty("/v1/auth/logout-all").await?;
        Ok(())
    }

    pub async fn change_password(&self, request: ChangePasswordRequest) -> Result<()> {
        let _: serde_json::Value = self.put("/v1/auth/password", &request).await?;
        Ok(())
    }

    pub async fn reset_password(&self, request: PasswordResetRequest) -> Result<()> {
        let _: serde_json::Value = self.post("/v1/auth/password/reset", &request).await?;
        Ok(())
    }

    pub async fn github_authorization_url(&self) -> Result<String> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let response = client
            .get(self.url("/v1/auth/github/start?client_kind=web"))
            .send()
            .await?;
        if !response.status().is_redirection() {
            return Err(server_error(response).await);
        }
        response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
            .ok_or_else(|| Error::Protocol("GitHub authorization redirect omitted Location".into()))
    }

    pub async fn github_callback(&self, code: &str, state: &str) -> Result<WebAuthResponse> {
        self.get(&format!(
            "/v1/auth/github/callback?code={}&state={}",
            urlencoding_replace(code),
            urlencoding_replace(state)
        ))
        .await
    }

    pub async fn github_allowlist(&self) -> Result<Vec<GithubAllowlistEntry>> {
        self.get("/v1/admin/auth/github-allowlist").await
    }

    pub async fn create_github_allowlist_entry(
        &self,
        request: CreateGithubAllowlistRequest,
    ) -> Result<GithubAllowlistEntry> {
        self.post("/v1/admin/auth/github-allowlist", &request).await
    }

    pub async fn revoke_github_allowlist_entry(&self, id: i64) -> Result<()> {
        self.delete(&format!("/v1/admin/auth/github-allowlist/{id}"))
            .await
    }

    pub async fn admin_create_principal(
        &self,
        request: AdminCreatePasswordPrincipalRequest,
    ) -> Result<AuthPrincipal> {
        self.post("/v1/admin/principals", &request).await
    }

    pub async fn admin_set_principal_disabled(
        &self,
        principal_id: i64,
        disabled: bool,
    ) -> Result<()> {
        let _: serde_json::Value = self
            .put(
                &format!("/v1/admin/principals/{principal_id}/disabled"),
                &AdminSetPrincipalDisabledRequest { disabled },
            )
            .await?;
        Ok(())
    }

    pub async fn admin_principal_identities(
        &self,
        principal_id: i64,
    ) -> Result<Vec<AuthIdentitySummary>> {
        self.get(&format!("/v1/admin/principals/{principal_id}/identities"))
            .await
    }

    pub async fn admin_link_password_identity(
        &self,
        principal_id: i64,
        request: AdminLinkPasswordIdentityRequest,
    ) -> Result<AuthIdentitySummary> {
        self.post(
            &format!("/v1/admin/principals/{principal_id}/identities/password"),
            &request,
        )
        .await
    }

    pub async fn admin_unlink_identity(&self, principal_id: i64, identity_id: i64) -> Result<()> {
        self.delete(&format!(
            "/v1/admin/principals/{principal_id}/identities/{identity_id}"
        ))
        .await
    }

    pub async fn admin_auth_sessions(&self, principal_id: i64) -> Result<Vec<AuthSessionSummary>> {
        self.get(&format!(
            "/v1/admin/principals/{principal_id}/auth-sessions"
        ))
        .await
    }

    pub async fn admin_revoke_auth_session(
        &self,
        principal_id: i64,
        session_id: &str,
    ) -> Result<()> {
        self.delete(&format!(
            "/v1/admin/principals/{principal_id}/auth-sessions/{session_id}"
        ))
        .await
    }

    pub async fn admin_issue_password_reset(
        &self,
        principal_id: i64,
        identity_id: i64,
    ) -> Result<IssuedPasswordResetResponse> {
        self.post_empty(&format!(
            "/v1/admin/principals/{principal_id}/identities/{identity_id}/password-reset"
        ))
        .await
    }

    pub async fn create_tenant_invitation(
        &self,
        tenant: TenantId,
        request: CreateTenantInvitationRequest,
    ) -> Result<IssuedTenantInvitationResponse> {
        self.post(
            &format!("/v1/metadata/tenants/{}/invitations", tenant.0),
            &request,
        )
        .await
    }

    pub async fn tenant_invitations(&self, tenant: TenantId) -> Result<Vec<TenantInvitation>> {
        self.get(&format!("/v1/metadata/tenants/{}/invitations", tenant.0))
            .await
    }

    pub async fn revoke_tenant_invitation(
        &self,
        tenant: TenantId,
        invitation_id: i64,
    ) -> Result<()> {
        self.delete(&format!(
            "/v1/metadata/tenants/{}/invitations/{invitation_id}",
            tenant.0
        ))
        .await
    }

    pub async fn accept_tenant_invitation(
        &self,
        request: AcceptTenantInvitationRequest,
    ) -> Result<TenantMembership> {
        self.post("/v1/auth/invitations/accept", &request).await
    }

    pub async fn principal_keys(&self) -> Result<Vec<PrincipalKey>> {
        self.get("/v1/auth/keys").await
    }

    pub async fn register_principal_key(
        &self,
        request: RegisterPrincipalKeyRequest,
    ) -> Result<PrincipalKey> {
        self.post("/v1/auth/keys", &request).await
    }

    pub async fn revoke_principal_key(&self, key_id: i64) -> Result<()> {
        self.delete(&format!("/v1/auth/keys/{key_id}")).await
    }

    pub async fn issue_key_challenge(
        &self,
        request: KeyChallengeRequest,
    ) -> Result<KeyChallengeResponse> {
        self.post("/v1/auth/keys/challenge", &request).await
    }

    pub async fn authenticate_key(
        &self,
        request: KeyAuthenticateRequest,
    ) -> Result<SessionTokenProvider> {
        let tokens: AuthTokensResponse = self.post("/v1/auth/keys/authenticate", &request).await?;
        Ok(SessionTokenProvider::new(tokens))
    }

    pub async fn health(&self) -> Result<Health> {
        self.get("/v1/health").await
    }

    /// Readiness probe. Returns the parsed [`Readiness`] on both `200` (ready)
    /// and `503` (not ready) — inspect [`Readiness::ready`] for the verdict.
    /// Other statuses (e.g. auth failure) surface as [`Error::Server`].
    pub async fn ready(&self) -> Result<Readiness> {
        let mut request = self.http.get(self.url("/v1/ready"));
        if let Some(token) = self.current_bearer().await {
            request = request.bearer_auth(token);
        }
        let response = request.send().await?;
        let status = response.status();
        if status == reqwest::StatusCode::OK || status == reqwest::StatusCode::SERVICE_UNAVAILABLE {
            Ok(response.json().await?)
        } else {
            let body = response.text().await.unwrap_or_default();
            Err(Error::Server { status, body })
        }
    }

    pub async fn open_session(&self, tag: Option<String>) -> Result<SessionInfo> {
        self.post("/v1/sessions", &OpenSessionRequest { tag }).await
    }

    pub async fn list_sessions(&self) -> Result<Vec<SessionInfo>> {
        self.get("/v1/sessions").await
    }

    pub async fn close_session(&self, session: SessionId) -> Result<()> {
        self.delete(&format!("/v1/sessions/{session}")).await
    }

    pub async fn open_connection(
        &self,
        session: SessionId,
        request: OpenConnectionRequest,
    ) -> Result<ConnectionInfo> {
        self.post(&format!("/v1/sessions/{session}/connections"), &request)
            .await
    }

    pub async fn ping_connection(
        &self,
        session: SessionId,
        connection: ConnectionId,
    ) -> Result<ServerInfo> {
        self.post_empty(&format!(
            "/v1/sessions/{session}/connections/{connection}/ping"
        ))
        .await
    }

    pub async fn list_processes(
        &self,
        session: SessionId,
        connection: ConnectionId,
    ) -> Result<Vec<DatabaseProcess>> {
        self.get(&format!(
            "/v1/sessions/{session}/connections/{connection}/processes"
        ))
        .await
    }

    pub async fn kill_process(
        &self,
        session: SessionId,
        connection: ConnectionId,
        process_id: i64,
    ) -> Result<KillProcessResponse> {
        self.post(
            &format!("/v1/sessions/{session}/connections/{connection}/processes/kill"),
            &KillProcessRequest { process_id },
        )
        .await
    }

    pub async fn schema(
        &self,
        session: SessionId,
        connection: ConnectionId,
    ) -> Result<SchemaSnapshot> {
        self.get(&format!(
            "/v1/sessions/{session}/connections/{connection}/schema"
        ))
        .await
    }

    /// Export a query result as CSV / TSV / JSON Lines / JSON Array.
    /// Returns the full response body as bytes; caller writes to file
    /// or parses. For very large results, prefer calling the endpoint
    /// directly with reqwest and streaming the body — this convenience
    /// method buffers the whole payload.
    pub async fn export_query(
        &self,
        session: SessionId,
        connection: ConnectionId,
        request: sift_protocol::ExportRequest,
    ) -> Result<Vec<u8>> {
        let mut req = self
            .http
            .post(self.url(&format!(
                "/v1/sessions/{session}/connections/{connection}/export"
            )))
            .json(&request);
        if let Some(token) = self.current_bearer().await {
            req = req.bearer_auth(token);
        }
        let resp = req.send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Server { status, body });
        }
        Ok(resp.bytes().await?.to_vec())
    }

    /// Generate DDL for a database object. `path.name` is required;
    /// `path.schema` and `path.kind` are optional (kind defaults to
    /// table server-side). The response includes the object's
    /// canonical `path` and a `ddl` string containing the CREATE
    /// statement(s); for tables, standalone CREATE INDEX statements
    /// for non-constraint indexes follow the CREATE TABLE.
    pub async fn object_ddl(
        &self,
        session: SessionId,
        connection: ConnectionId,
        path: &sift_protocol::ObjectPath,
    ) -> Result<sift_protocol::ObjectDdl> {
        let mut query = vec![format!("name={}", urlencoding_replace(&path.name))];
        if let Some(schema) = &path.schema {
            query.push(format!("schema={}", urlencoding_replace(schema)));
        }
        if let Some(kind) = &path.kind {
            let kind_str = serde_json::to_string(kind).map_err(Error::Json)?;
            // Strip the surrounding quotes serde_json emits for enums.
            let cleaned = kind_str.trim_matches('"');
            query.push(format!("kind={cleaned}"));
        }
        if let Some(args) = &path.routine_args {
            for arg in args {
                query.push(format!("routine_args={}", urlencoding_replace(arg)));
            }
        }
        self.get(&format!(
            "/v1/sessions/{session}/connections/{connection}/ddl?{}",
            query.join("&")
        ))
        .await
    }

    pub async fn complete(
        &self,
        session: SessionId,
        connection: ConnectionId,
        request: sift_protocol::completion::CompletionRequest,
    ) -> Result<sift_protocol::completion::CompletionResponse> {
        self.post(
            &format!("/v1/sessions/{session}/connections/{connection}/complete"),
            &request,
        )
        .await
    }

    pub async fn preview_edits(
        &self,
        session: SessionId,
        connection: ConnectionId,
        request: PreviewEditsRequest,
    ) -> Result<EditPlan> {
        self.post(
            &format!("/v1/sessions/{session}/connections/{connection}/edits/preview"),
            &request,
        )
        .await
    }

    pub async fn apply_edits(
        &self,
        session: SessionId,
        connection: ConnectionId,
        request: ApplyEditsRequest,
    ) -> Result<ApplyEditsResult> {
        self.post(
            &format!("/v1/sessions/{session}/connections/{connection}/edits/apply"),
            &request,
        )
        .await
    }

    pub async fn search_schema(
        &self,
        session: SessionId,
        connection: ConnectionId,
        request: SchemaSearchRequest,
    ) -> Result<SchemaSearchResponse> {
        self.post(
            &format!("/v1/sessions/{session}/connections/{connection}/search/schema"),
            &request,
        )
        .await
    }

    pub async fn search_data(
        &self,
        session: SessionId,
        connection: ConnectionId,
        request: DataSearchRequest,
    ) -> Result<DataSearchResponse> {
        self.post(
            &format!("/v1/sessions/{session}/connections/{connection}/search/data"),
            &request,
        )
        .await
    }

    pub async fn explain(
        &self,
        session: SessionId,
        connection: ConnectionId,
        request: ExplainRequest,
    ) -> Result<ExplainResponse> {
        self.post(
            &format!("/v1/sessions/{session}/connections/{connection}/explain"),
            &request,
        )
        .await
    }

    pub async fn bulk_insert(
        &self,
        session: SessionId,
        connection: ConnectionId,
        request: BulkInsertRequest,
    ) -> Result<BulkInsertResponse> {
        self.post(
            &format!("/v1/sessions/{session}/connections/{connection}/bulk-insert"),
            &request,
        )
        .await
    }

    pub async fn import_csv(
        &self,
        session: SessionId,
        connection: ConnectionId,
        request: CsvImportRequest,
    ) -> Result<CsvImportResponse> {
        self.post(
            &format!("/v1/sessions/{session}/connections/{connection}/import/csv"),
            &request,
        )
        .await
    }

    pub async fn execute(
        &self,
        session: SessionId,
        connection: ConnectionId,
        sql: impl Into<String>,
    ) -> Result<ExecuteResponse> {
        self.post(
            &format!("/v1/sessions/{session}/queries"),
            &ExecuteRequestHttp {
                connection,
                sql: sql.into(),
                params: Vec::new(),
                tx: None,
                room_id: None,
                connection_profile_id: None,
            },
        )
        .await
    }

    pub async fn execute_with_params(
        &self,
        session: SessionId,
        connection: ConnectionId,
        sql: impl Into<String>,
        params: Vec<Value>,
    ) -> Result<ExecuteResponse> {
        self.post(
            &format!("/v1/sessions/{session}/queries"),
            &ExecuteRequestHttp {
                connection,
                sql: sql.into(),
                params,
                tx: None,
                room_id: None,
                connection_profile_id: None,
            },
        )
        .await
    }

    pub async fn execute_in_tx(
        &self,
        session: SessionId,
        tx: &TransactionInfo,
        sql: impl Into<String>,
    ) -> Result<ExecuteResponse> {
        self.post(
            &format!("/v1/sessions/{session}/queries"),
            &ExecuteRequestHttp {
                connection: tx.connection,
                sql: sql.into(),
                params: Vec::new(),
                tx: Some(TxHandleRef {
                    tx_id: tx.tx_id,
                    connection: tx.connection,
                    mode: tx.mode,
                }),
                room_id: None,
                connection_profile_id: None,
            },
        )
        .await
    }

    pub async fn begin_transaction(
        &self,
        session: SessionId,
        connection: ConnectionId,
        mode: TxMode,
    ) -> Result<TransactionInfo> {
        self.post(
            &format!("/v1/sessions/{session}/transactions"),
            &BeginTransactionRequest { connection, mode },
        )
        .await
    }

    pub async fn list_transactions(&self, session: SessionId) -> Result<Vec<TransactionState>> {
        self.get(&format!("/v1/sessions/{session}/transactions"))
            .await
    }

    pub async fn preview_transaction(
        &self,
        session: SessionId,
        connection: ConnectionId,
        tx_id: TxId,
        action: TransactionEndAction,
    ) -> Result<TransactionPreview> {
        self.post(
            &format!("/v1/sessions/{session}/transactions/{tx_id}/preview"),
            &TransactionPreviewRequest {
                connection,
                tx_id,
                action,
            },
        )
        .await
    }

    pub async fn commit_transaction(
        &self,
        session: SessionId,
        connection: ConnectionId,
        tx_id: TxId,
    ) -> Result<()> {
        self.post_empty_body(
            &format!("/v1/sessions/{session}/transactions/{tx_id}/commit"),
            &EndTransactionRequest { connection, tx_id },
        )
        .await
    }

    pub async fn rollback_transaction(
        &self,
        session: SessionId,
        connection: ConnectionId,
        tx_id: TxId,
    ) -> Result<()> {
        self.post_empty_body(
            &format!("/v1/sessions/{session}/transactions/{tx_id}/rollback"),
            &EndTransactionRequest { connection, tx_id },
        )
        .await
    }

    pub async fn cancel(
        &self,
        session: SessionId,
        connection: ConnectionId,
        cursor: CursorId,
    ) -> Result<()> {
        self.post_empty_body(
            &format!("/v1/sessions/{session}/queries/{cursor}/cancel"),
            &CancelRequest { connection, cursor },
        )
        .await
    }

    pub async fn close_connection(
        &self,
        session: SessionId,
        connection: ConnectionId,
    ) -> Result<()> {
        self.delete(&format!("/v1/sessions/{session}/connections/{connection}"))
            .await
    }

    pub async fn create_savepoint(
        &self,
        session: SessionId,
        connection: ConnectionId,
        tx_id: TxId,
        name: impl Into<String>,
    ) -> Result<()> {
        self.post_empty_body(
            &format!("/v1/sessions/{session}/transactions/{tx_id}/savepoints"),
            &SavepointRequest {
                connection,
                tx_id,
                name: name.into(),
            },
        )
        .await
    }

    pub async fn rollback_to_savepoint(
        &self,
        session: SessionId,
        connection: ConnectionId,
        tx_id: TxId,
        name: impl Into<String>,
    ) -> Result<()> {
        self.post_empty_body(
            &format!("/v1/sessions/{session}/transactions/{tx_id}/savepoints/rollback"),
            &SavepointRequest {
                connection,
                tx_id,
                name: name.into(),
            },
        )
        .await
    }

    pub async fn release_savepoint(
        &self,
        session: SessionId,
        connection: ConnectionId,
        tx_id: TxId,
        name: impl Into<String>,
    ) -> Result<()> {
        self.post_empty_body(
            &format!("/v1/sessions/{session}/transactions/{tx_id}/savepoints/release"),
            &SavepointRequest {
                connection,
                tx_id,
                name: name.into(),
            },
        )
        .await
    }

    pub async fn openapi(&self) -> Result<serde_json::Value> {
        self.get("/v1/openapi.json").await
    }

    /// Read the next batch of pages from an evicted cursor's spill
    /// file (ADR-011). The `resume_url` returned on
    /// `Page::Error { code: CursorEvicted }` points at this endpoint.
    /// The optional `from_seq` must equal the entry's current
    /// pages_read — the spill file is append-only forward-read.
    pub async fn read_spilled_pages(
        &self,
        cursor: CursorId,
        from_seq: Option<usize>,
        limit: Option<usize>,
    ) -> Result<serde_json::Value> {
        let mut query = Vec::new();
        if let Some(seq) = from_seq {
            query.push(format!("from_seq={seq}"));
        }
        if let Some(limit) = limit {
            query.push(format!("limit={limit}"));
        }
        let suffix = if query.is_empty() {
            String::new()
        } else {
            format!("?{}", query.join("&"))
        };
        self.get(&format!("/v1/cursors/{}/pages{suffix}", cursor.0))
            .await
    }

    /// Delete a spilled cursor's file explicitly. Idempotent — returns
    /// ok even if the entry has already been reaped by TTL or fully
    /// drained.
    pub async fn delete_spilled_cursor(&self, cursor: CursorId) -> Result<()> {
        self.delete(&format!("/v1/cursors/{}", cursor.0)).await
    }

    pub async fn tenants(&self) -> Result<Vec<TenantMembership>> {
        self.get("/v1/metadata/tenants").await
    }

    pub async fn rooms(&self, tenant: TenantId) -> Result<Vec<Room>> {
        self.get(&format!("/v1/metadata/rooms?tenant={}", tenant.0))
            .await
    }

    pub async fn create_room(&self, request: CreateRoomRequest) -> Result<Room> {
        self.post("/v1/metadata/rooms", &request).await
    }

    pub async fn delete_room(&self, room: RoomId) -> Result<()> {
        self.delete(&format!("/v1/metadata/rooms/{}", room.0)).await
    }

    pub async fn room_members(&self, room: RoomId) -> Result<Vec<RoomMember>> {
        self.get(&format!("/v1/metadata/rooms/{}/members", room.0))
            .await
    }

    pub async fn add_room_member(
        &self,
        room: RoomId,
        request: AddRoomMemberRequest,
    ) -> Result<RoomMember> {
        self.post(&format!("/v1/metadata/rooms/{}/members", room.0), &request)
            .await
    }

    pub async fn remove_room_member(&self, room: RoomId, principal_id: i64) -> Result<()> {
        self.delete(&format!(
            "/v1/metadata/rooms/{}/members/{principal_id}",
            room.0
        ))
        .await
    }

    pub async fn join_room(&self, room: RoomId) -> Result<RoomMember> {
        self.post_empty(&format!("/v1/metadata/rooms/{}/join", room.0))
            .await
    }

    pub async fn leave_room(&self, room: RoomId) -> Result<()> {
        self.post_empty_body(
            &format!("/v1/metadata/rooms/{}/leave", room.0),
            &serde_json::json!({}),
        )
        .await
    }

    pub async fn documents(&self, room: RoomId) -> Result<Vec<Document>> {
        self.get(&format!("/v1/metadata/rooms/{}/documents", room.0))
            .await
    }

    pub async fn create_document(
        &self,
        room: RoomId,
        request: CreateDocumentRequest,
    ) -> Result<Document> {
        self.post(
            &format!("/v1/metadata/rooms/{}/documents", room.0),
            &request,
        )
        .await
    }

    pub async fn update_document_snapshot(
        &self,
        document: DocumentId,
        request: UpdateDocumentSnapshotRequest,
    ) -> Result<Document> {
        self.put(&format!("/v1/metadata/documents/{}", document.0), &request)
            .await
    }

    pub async fn delete_document(&self, document: DocumentId) -> Result<()> {
        self.delete(&format!("/v1/metadata/documents/{}", document.0))
            .await
    }

    pub async fn connection_profiles(&self, tenant: TenantId) -> Result<Vec<ConnectionProfile>> {
        self.get(&format!("/v1/metadata/connections?tenant={}", tenant.0))
            .await
    }

    pub async fn upsert_connection_profile(
        &self,
        request: UpsertConnectionProfileRequest,
    ) -> Result<ConnectionProfile> {
        self.post("/v1/metadata/connections", &request).await
    }

    pub async fn delete_connection_profile(
        &self,
        tenant: TenantId,
        profile: ConnectionProfileId,
    ) -> Result<()> {
        self.delete(&format!(
            "/v1/metadata/connections/{}?tenant={}",
            profile.0, tenant.0
        ))
        .await
    }

    pub async fn set_connection_credential(
        &self,
        profile: ConnectionProfileId,
        request: SetCredentialRequest,
    ) -> Result<()> {
        self.post_empty_body(
            &format!("/v1/metadata/connections/{}/credential", profile.0),
            &request,
        )
        .await
    }

    pub async fn open_connection_from_profile(
        &self,
        session: SessionId,
        request: OpenConnectionFromProfileRequest,
    ) -> Result<ConnectionInfo> {
        self.post(
            &format!("/v1/sessions/{session}/connections/from-profile"),
            &request,
        )
        .await
    }

    pub async fn history(
        &self,
        room: Option<RoomId>,
        limit: Option<u32>,
    ) -> Result<Vec<QueryHistory>> {
        let mut query = Vec::new();
        if let Some(room) = room {
            query.push(format!("room={}", room.0));
        }
        if let Some(limit) = limit {
            query.push(format!("limit={limit}"));
        }
        let suffix = if query.is_empty() {
            String::new()
        } else {
            format!("?{}", query.join("&"))
        };
        self.get(&format!("/v1/metadata/history{suffix}")).await
    }

    /// List saved queries visible to the caller. `q` is a full-text
    /// search over name + sql_text; `tags` restrict to entries
    /// containing all listed tags; `scope` narrows to personal-only
    /// or shared-only.
    pub async fn saved_queries(
        &self,
        tenant: TenantId,
        q: Option<&str>,
        tags: &[String],
        scope: Option<SavedQueryScope>,
    ) -> Result<Vec<SavedQuery>> {
        let mut query = vec![format!("tenant={}", tenant.0)];
        if let Some(s) = q {
            query.push(format!("q={}", urlencoding_replace(s)));
        }
        if !tags.is_empty() {
            let joined: Vec<String> = tags.iter().map(|t| urlencoding_replace(t)).collect();
            query.push(format!("tags={}", joined.join(",")));
        }
        if let Some(scope) = scope {
            let s = match scope {
                SavedQueryScope::Personal => "personal",
                SavedQueryScope::Shared => "shared",
                SavedQueryScope::All => "all",
            };
            query.push(format!("scope={s}"));
        }
        self.get(&format!("/v1/metadata/saved-queries?{}", query.join("&")))
            .await
    }

    pub async fn saved_query(&self, id: SavedQueryId) -> Result<SavedQuery> {
        self.get(&format!("/v1/metadata/saved-queries/{}", id.0))
            .await
    }

    pub async fn create_saved_query(&self, request: CreateSavedQueryRequest) -> Result<SavedQuery> {
        self.post("/v1/metadata/saved-queries", &request).await
    }

    pub async fn update_saved_query(
        &self,
        id: SavedQueryId,
        request: UpdateSavedQueryRequest,
    ) -> Result<SavedQuery> {
        self.put(&format!("/v1/metadata/saved-queries/{}", id.0), &request)
            .await
    }

    pub async fn delete_saved_query(&self, id: SavedQueryId) -> Result<()> {
        self.delete(&format!("/v1/metadata/saved-queries/{}", id.0))
            .await
    }

    pub async fn auth_tokens(&self) -> Result<Vec<sift_metadata::ApiTokenRow>> {
        self.get("/v1/auth/tokens").await
    }

    pub async fn issue_token(&self, request: IssueTokenRequest) -> Result<IssueTokenResponse> {
        self.post("/v1/auth/tokens", &request).await
    }

    pub async fn revoke_token(&self, token: ApiTokenId) -> Result<()> {
        self.delete(&format!("/v1/auth/tokens/{}", token.0)).await
    }

    pub async fn apply_room_text_operation(
        &self,
        room: RoomId,
        document: DocumentId,
        client_id: impl Into<String>,
        operation_id: impl Into<String>,
        operation: TextDocumentOperation,
    ) -> Result<sift_protocol::DocumentOperationEnvelope> {
        use futures::SinkExt;
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        use tokio_tungstenite::tungstenite::Message;

        let mut request = self.room_ws_url(room).into_client_request()?;
        if let Some(token) = self.current_bearer().await {
            request.headers_mut().insert(
                "authorization",
                format!("Bearer {token}")
                    .parse()
                    .map_err(|e| Error::Protocol(format!("invalid bearer token header: {e}")))?,
            );
        }
        let (mut ws, _) = tokio_tungstenite::connect_async(request).await?;
        ws.send(Message::Text(
            serde_json::to_string(&sift_protocol::RoomClientMessage::Attach {
                client_id: client_id.into(),
            })?
            .into(),
        ))
        .await?;
        loop {
            match next_room_ws(&mut ws).await? {
                sift_protocol::RoomServerMessage::Attached { .. } => break,
                sift_protocol::RoomServerMessage::Error { message } => {
                    return Err(Error::Protocol(message));
                }
                _ => {}
            }
        }

        let operation_id = operation_id.into();
        ws.send(Message::Text(
            serde_json::to_string(&sift_protocol::RoomClientMessage::DocumentOperation {
                operation_id: operation_id.clone(),
                document_id: document.0,
                operation,
            })?
            .into(),
        ))
        .await?;
        loop {
            match next_room_ws(&mut ws).await? {
                sift_protocol::RoomServerMessage::DocumentOperation { operation }
                    if operation.operation_id == operation_id =>
                {
                    return Ok(operation);
                }
                sift_protocol::RoomServerMessage::Error { message } => {
                    return Err(Error::Protocol(message));
                }
                _ => {}
            }
        }
    }

    pub async fn audit(&self) -> Result<Vec<sift_protocol::AuditEntry>> {
        self.get("/v1/audit").await
    }

    pub async fn operations(&self) -> Result<Vec<sift_protocol::OperationAuditEntry>> {
        self.get("/v1/operations").await
    }

    pub async fn available_operations(
        &self,
        context: &OperationCapabilityContext,
    ) -> Result<Vec<OperationCapability>> {
        let mut query = Vec::new();
        if let Some(session) = context.session {
            query.push(format!("session={session}"));
        }
        if let Some(connection) = context.connection {
            query.push(format!("connection={connection}"));
        }
        if let Some(transaction) = context.transaction {
            query.push(format!("transaction={transaction}"));
        }
        let suffix = if query.is_empty() {
            String::new()
        } else {
            format!("?{}", query.join("&"))
        };
        self.get(&format!("/v1/operations/available{suffix}")).await
    }

    /// Durable operation-audit rows (actor, target, result code, row count,
    /// sanitized failure message). Requires a configured metadata store.
    pub async fn operation_audit(&self) -> Result<Vec<sift_metadata::OperationAudit>> {
        self.get("/v1/operations/audit").await
    }

    pub async fn stream_query(
        &self,
        session: SessionId,
        connection: ConnectionId,
        sql: impl Into<String>,
    ) -> Result<Vec<Page>> {
        use futures::SinkExt;
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        use tokio_tungstenite::tungstenite::Message;

        let mut request = self.ws_url(session).into_client_request()?;
        if let Some(token) = self.current_bearer().await {
            request.headers_mut().insert(
                "authorization",
                format!("Bearer {token}")
                    .parse()
                    .map_err(|e| Error::Protocol(format!("invalid bearer token header: {e}")))?,
            );
        }
        let (mut ws, _) = tokio_tungstenite::connect_async(request).await?;
        let request_id = "sdk-stream-query".to_string();
        ws.send(Message::Text(
            serde_json::to_string(&WsClientMessage::Execute {
                request_id: request_id.clone(),
                connection,
                sql: sql.into(),
                params: Vec::new(),
                tx: None,
            })?
            .into(),
        ))
        .await?;

        let first = next_ws(&mut ws).await?;
        let cursor_id = match first {
            WsServerMessage::Started {
                request_id: got,
                cursor_id,
            } if got == request_id => cursor_id,
            other => {
                return Err(Error::Protocol(format!(
                    "expected started message, got {other:?}"
                )));
            }
        };

        let mut pages = Vec::new();
        loop {
            let msg = next_ws(&mut ws).await?;
            match msg {
                WsServerMessage::Page {
                    cursor_id: got,
                    seq,
                    page,
                } if got == cursor_id => {
                    let done = matches!(page, Page::Done { .. } | Page::Error { .. });
                    pages.push(page);
                    if done {
                        return Ok(pages);
                    }
                    ws.send(Message::Text(
                        serde_json::to_string(&WsClientMessage::Ack { cursor_id, seq })?.into(),
                    ))
                    .await?;
                }
                WsServerMessage::Error { message, .. } => return Err(Error::Protocol(message)),
                other => {
                    return Err(Error::Protocol(format!(
                        "unexpected websocket message: {other:?}"
                    )));
                }
            }
        }
    }

    pub async fn connect_session_websocket(&self, session: SessionId) -> Result<SessionWebSocket> {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;

        let mut request = self.ws_url(session).into_client_request()?;
        if let Some(token) = self.current_bearer().await {
            request.headers_mut().insert(
                "authorization",
                format!("Bearer {token}").parse().map_err(|error| {
                    Error::Protocol(format!("invalid bearer token header: {error}"))
                })?,
            );
        }
        let (socket, _) = tokio_tungstenite::connect_async(request).await?;
        Ok(SessionWebSocket { socket })
    }

    pub async fn connect_room_websocket(&self, room: RoomId) -> Result<RoomWebSocket> {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;

        let mut request = self.room_ws_url(room).into_client_request()?;
        if let Some(token) = self.current_bearer().await {
            request.headers_mut().insert(
                "authorization",
                format!("Bearer {token}").parse().map_err(|error| {
                    Error::Protocol(format!("invalid bearer token header: {error}"))
                })?,
            );
        }
        let (socket, _) = tokio_tungstenite::connect_async(request).await?;
        Ok(RoomWebSocket { socket })
    }

    pub async fn listen_notifications(
        &self,
        session: SessionId,
        connection: ConnectionId,
        channels: Vec<String>,
        max_notifications: usize,
    ) -> Result<Vec<(String, String)>> {
        use futures::SinkExt;
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        use tokio_tungstenite::tungstenite::Message;

        let mut request = self.ws_url(session).into_client_request()?;
        if let Some(token) = self.current_bearer().await {
            request.headers_mut().insert(
                "authorization",
                format!("Bearer {token}")
                    .parse()
                    .map_err(|e| Error::Protocol(format!("invalid bearer token header: {e}")))?,
            );
        }
        let (mut ws, _) = tokio_tungstenite::connect_async(request).await?;
        let request_id = "sdk-listen".to_string();
        ws.send(Message::Text(
            serde_json::to_string(&WsClientMessage::Listen {
                request_id: request_id.clone(),
                connection,
                channels,
            })?
            .into(),
        ))
        .await?;

        let mut notifications = Vec::with_capacity(max_notifications);
        while notifications.len() < max_notifications {
            match next_ws(&mut ws).await? {
                WsServerMessage::Notification {
                    request_id: got,
                    channel,
                    payload,
                } if got == request_id => notifications.push((channel, payload)),
                WsServerMessage::Error { message, .. } => return Err(Error::Protocol(message)),
                other => {
                    return Err(Error::Protocol(format!(
                        "unexpected websocket message: {other:?}"
                    )));
                }
            }
        }
        Ok(notifications)
    }

    pub fn ws_url(&self, session: SessionId) -> String {
        let base = self
            .base
            .strip_prefix("https://")
            .map(|s| format!("wss://{s}"))
            .or_else(|| {
                self.base
                    .strip_prefix("http://")
                    .map(|s| format!("ws://{s}"))
            })
            .unwrap_or_else(|| self.base.clone());
        format!("{base}/v1/sessions/{session}/ws")
    }

    pub fn room_ws_url(&self, room: RoomId) -> String {
        let base = self
            .base
            .strip_prefix("https://")
            .map(|s| format!("wss://{s}"))
            .or_else(|| {
                self.base
                    .strip_prefix("http://")
                    .map(|s| format!("ws://{s}"))
            })
            .unwrap_or_else(|| self.base.clone());
        format!("{base}/v1/metadata/rooms/{}/ws", room.0)
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.send(self.http.get(self.url(path))).await
    }

    async fn post<T: serde::de::DeserializeOwned, B: serde::Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        self.send(self.http.post(self.url(path)).json(body)).await
    }

    async fn put<T: serde::de::DeserializeOwned, B: serde::Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        self.send(self.http.put(self.url(path)).json(body)).await
    }

    async fn post_empty<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.send(self.http.post(self.url(path))).await
    }

    async fn post_empty_body<B: serde::Serialize>(&self, path: &str, body: &B) -> Result<()> {
        let _: serde_json::Value = self.post(path, body).await?;
        Ok(())
    }

    async fn delete(&self, path: &str) -> Result<()> {
        let _: serde_json::Value = self.send(self.http.delete(self.url(path))).await?;
        Ok(())
    }

    async fn send<T: serde::de::DeserializeOwned>(
        &self,
        mut request: reqwest::RequestBuilder,
    ) -> Result<T> {
        if let Some(token) = self.current_bearer().await {
            request = request.bearer_auth(token);
        }
        let response = request.send().await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(Error::Server { status, body });
        }
        Ok(response.json().await?)
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    async fn current_bearer(&self) -> Option<String> {
        if let Some(provider) = &self.session_tokens {
            return Some(provider.access_token().await);
        }
        self.token.clone()
    }
}

async fn response_json<T: serde::de::DeserializeOwned>(response: reqwest::Response) -> Result<T> {
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(Error::Server { status, body });
    }
    Ok(response.json().await?)
}

async fn server_error(response: reqwest::Response) -> Error {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    Error::Server { status, body }
}

async fn next_ws<S>(ws: &mut S) -> Result<WsServerMessage>
where
    S: futures::Stream<
            Item = std::result::Result<
                tokio_tungstenite::tungstenite::Message,
                tokio_tungstenite::tungstenite::Error,
            >,
        > + Unpin,
{
    use futures::StreamExt;
    use tokio_tungstenite::tungstenite::Message;

    loop {
        let Some(message) = ws.next().await else {
            return Err(Error::Protocol("websocket closed".into()));
        };
        match message? {
            Message::Text(text) => return Ok(serde_json::from_str(&text)?),
            Message::Binary(bytes) => return Ok(serde_json::from_slice(&bytes)?),
            Message::Close(_) => return Err(Error::Protocol("websocket closed".into())),
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
        }
    }
}

async fn next_room_ws<S>(ws: &mut S) -> Result<sift_protocol::RoomServerMessage>
where
    S: futures::Stream<
            Item = std::result::Result<
                tokio_tungstenite::tungstenite::Message,
                tokio_tungstenite::tungstenite::Error,
            >,
        > + Unpin,
{
    use futures::StreamExt;
    use tokio_tungstenite::tungstenite::Message;

    loop {
        let Some(message) = ws.next().await else {
            return Err(Error::Protocol("websocket closed".into()));
        };
        match message? {
            Message::Text(text) => return Ok(serde_json::from_str(&text)?),
            Message::Binary(bytes) => return Ok(serde_json::from_slice(&bytes)?),
            Message::Close(_) => return Err(Error::Protocol("websocket closed".into())),
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
        }
    }
}

/// Minimal percent-encoding for query-string values. Only encodes
/// characters that would actually break parsing (`&`, `=`, `#`, `+`,
/// `%`, whitespace). Sufficient for typed SDK callers, which don't
/// produce hostile input.
fn urlencoding_replace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("%26"),
            '=' => out.push_str("%3D"),
            '#' => out.push_str("%23"),
            '+' => out.push_str("%2B"),
            '%' => out.push_str("%25"),
            ' ' => out.push_str("%20"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(access: &str, refresh: &str) -> AuthTokensResponse {
        AuthTokensResponse {
            access_token: access.into(),
            access_expires_at: chrono::Utc::now(),
            refresh_token: refresh.into(),
            refresh_expires_at: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn session_token_provider_rotates_and_redacts() {
        let provider = SessionTokenProvider::new(tokens("access-one", "refresh-one"));
        assert_eq!(provider.access_token().await, "access-one");
        assert!(!format!("{provider:?}").contains("access-one"));
        provider.replace(tokens("access-two", "refresh-two")).await;
        assert_eq!(provider.access_token().await, "access-two");
        assert_eq!(provider.refresh_token().await, "refresh-two");
    }
}
