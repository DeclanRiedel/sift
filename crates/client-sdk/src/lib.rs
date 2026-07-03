//! `sift-client-sdk` — thin reference consumer proving the HTTP API is
//! buildable-against from outside the server crate.

use sift_protocol::{
    BeginTransactionRequest, BulkInsertRequest, BulkInsertResponse, CancelRequest, ConnectionId,
    ConnectionInfo, CursorId, EndTransactionRequest, Engine, ExecuteRequestHttp, ExecuteResponse,
    Health, OpenConnectionRequest, OpenSessionRequest, Page, SchemaSnapshot, ServerInfo, SessionId,
    SessionInfo, TransactionInfo, TxHandleRef, TxId, TxMode, WsClientMessage, WsServerMessage,
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
                tx: None,
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
                tx: Some(TxHandleRef {
                    tx_id: tx.tx_id,
                    connection: tx.connection,
                    mode: tx.mode,
                }),
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

    pub async fn audit(&self) -> Result<Vec<sift_protocol::AuditEntry>> {
        self.get("/v1/audit").await
    }

    pub async fn operations(&self) -> Result<Vec<sift_protocol::OperationAuditEntry>> {
        self.get("/v1/operations").await
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

#[allow(dead_code)]
fn _engine_marker(engine: Engine) -> Engine {
    engine
}
