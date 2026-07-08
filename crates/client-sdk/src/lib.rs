//! `sift-client-sdk` — thin reference consumer proving the HTTP API is
//! buildable-against from outside the server crate.

// Request/response DTOs shared with the server. Re-export so downstream
// consumers can build requests without depending on sift_metadata::http
// directly.
pub use sift_metadata::http::{
    AddRoomMemberRequest, CreateDocumentRequest, CreateRoomRequest, IssueTokenRequest,
    IssueTokenResponse, OpenConnectionFromProfileRequest, SetCredentialRequest,
    UpdateDocumentSnapshotRequest, UpsertConnectionProfileRequest,
};
use sift_metadata::{
    ApiTokenId, ConnectionProfile, ConnectionProfileId, Document, DocumentId, QueryHistory, Room,
    RoomId, RoomMember, TenantId, TenantMembership,
};
use sift_protocol::{
    BeginTransactionRequest, BulkInsertRequest, BulkInsertResponse, CancelRequest, ConnectionId,
    ConnectionInfo, CursorId, EndTransactionRequest, ExecuteRequestHttp, ExecuteResponse, Health,
    OpenConnectionRequest, OpenSessionRequest, Page, Readiness, SchemaSnapshot, ServerInfo,
    SessionId, SessionInfo, TextDocumentOperation, TransactionInfo, TxHandleRef, TxId, TxMode,
    Value, WsClientMessage, WsServerMessage,
};

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
    http: reqwest::Client,
}

impl Client {
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            token: None,
            http: reqwest::Client::new(),
        }
    }

    pub fn with_bearer_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    pub async fn health(&self) -> Result<Health> {
        self.get("/v1/health").await
    }

    /// Readiness probe. Returns the parsed [`Readiness`] on both `200` (ready)
    /// and `503` (not ready) — inspect [`Readiness::ready`] for the verdict.
    /// Other statuses (e.g. auth failure) surface as [`Error::Server`].
    pub async fn ready(&self) -> Result<Readiness> {
        let mut request = self.http.get(self.url("/v1/ready"));
        if let Some(token) = &self.token {
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

    pub async fn openapi(&self) -> Result<serde_json::Value> {
        self.get("/v1/openapi.json").await
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
        if let Some(token) = &self.token {
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
        if let Some(token) = &self.token {
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
        if let Some(token) = &self.token {
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
        if let Some(token) = &self.token {
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
            Message::Close(_) => return Err(Error::Protocol("websocket closed".into())),
            Message::Ping(_) | Message::Pong(_) | Message::Binary(_) | Message::Frame(_) => {}
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
            Message::Close(_) => return Err(Error::Protocol("websocket closed".into())),
            Message::Ping(_) | Message::Pong(_) | Message::Binary(_) | Message::Frame(_) => {}
        }
    }
}
