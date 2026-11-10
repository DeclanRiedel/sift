//! A captured credential and server/account assertions for queued extension work.
use super::*;
use sift_protocol::handshake::{
    ExtensionCallerIdentity, CAPABILITY_EXTENSION_CALLER_SCOPE, EXPECTED_AUTH_SESSION_HEADER,
    EXPECTED_DAEMON_HEADER, EXPECTED_INSTANCE_HEADER, EXPECTED_PRINCIPAL_HEADER,
};

/// Only guarded extension routes are exposed. This cannot silently fall back to
/// an old daemon's legacy action route or a mutable interactive credential.
#[derive(Clone)]
pub struct ScopedExtensionClient {
    client: Client,
    identity: ExtensionCallerIdentity,
}

impl Client {
    pub async fn capture_extension_caller(
        &self,
        expected: &ExtensionCallerIdentity,
    ) -> Result<ScopedExtensionClient> {
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            self.capture_extension_caller_inner(expected),
        )
        .await
        .map_err(|_| Error::Timeout("extension caller capture"))?
    }

    async fn capture_extension_caller_inner(
        &self,
        expected: &ExtensionCallerIdentity,
    ) -> Result<ScopedExtensionClient> {
        let mut client = self.clone();
        client.http = Self::transport_builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let handshake = client.probe_handshake().await?;
        if handshake.instance_id != expected.instance_id
            || handshake.daemon_generation != expected.daemon_generation
            || !handshake
                .capabilities
                .iter()
                .any(|c| c == CAPABILITY_EXTENSION_CALLER_SCOPE)
        {
            return Err(Error::Protocol(
                "extension server identity changed or caller scoping is unavailable".into(),
            ));
        }
        client.token = client.current_bearer().await?;
        client.session_tokens = None;
        let identity = client.whoami().await?;
        if identity.principal.id != expected.principal_id
            || identity.auth_session_id != expected.auth_session_id
        {
            return Err(Error::Protocol(
                "extension account changed; review again".into(),
            ));
        }
        Ok(ScopedExtensionClient {
            client,
            identity: expected.clone(),
        })
    }
}

impl ScopedExtensionClient {
    async fn send<T: serde::de::DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<T> {
        self.client
            .send(
                request
                    .header(EXPECTED_INSTANCE_HEADER, &self.identity.instance_id)
                    .header(EXPECTED_DAEMON_HEADER, &self.identity.daemon_generation)
                    .header(
                        EXPECTED_AUTH_SESSION_HEADER,
                        self.identity.auth_session_id.as_deref().unwrap_or("-"),
                    )
                    .header(
                        EXPECTED_PRINCIPAL_HEADER,
                        self.identity.principal_id.to_string(),
                    ),
            )
            .await
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.send(
            self.client
                .http
                .get(self.client.url(&format!("/v1/scoped{path}"))),
        )
        .await
    }

    async fn post<T: serde::de::DeserializeOwned, B: serde::Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        self.send(
            self.client
                .http
                .post(self.client.url(&format!("/v1/scoped{path}")))
                .json(body),
        )
        .await
    }

    pub async fn extensions(&self) -> Result<Vec<ExtensionDescriptor>> {
        self.get("/extensions").await
    }

    pub async fn governed_tools(
        &self,
        context: &ToolContext,
        mcp_only: bool,
    ) -> Result<Vec<GovernedToolDescriptor>> {
        let request = self
            .client
            .http
            .get(self.client.url("/v1/scoped/tools"))
            .query(context)
            .query(&[("mcp_only", mcp_only)]);
        self.send(request).await
    }

    pub async fn invoke_extension(
        &self,
        request: &InvokeExtensionRequest,
    ) -> Result<InvokeExtensionOutcome> {
        let mut request = request.clone();
        request.request_id.get_or_insert_with(uuid::Uuid::new_v4);
        self.post("/extension-actions/invoke", &request).await
    }

    pub async fn invoke_tool(&self, request: &InvokeToolRequest) -> Result<InvokeToolResponse> {
        let mut request = request.clone();
        request.request_id.get_or_insert_with(uuid::Uuid::new_v4);
        self.post("/tools/invoke", &request).await
    }

    pub async fn create_operation_approval(
        &self,
        request: &sift_protocol::CreateOperationApprovalRequest,
    ) -> Result<OperationApproval> {
        self.post("/operation-approvals", request).await
    }

    pub async fn approve_operation(
        &self,
        id: &str,
        request: &ExpectedRevision,
    ) -> Result<OperationApproval> {
        let id = uuid::Uuid::parse_str(id)
            .map_err(|_| Error::Protocol("invalid operation approval identity".into()))?;
        self.post(&format!("/operation-approvals/{id}/approve"), request)
            .await
    }

    pub async fn extension_executions(
        &self,
    ) -> Result<Vec<sift_protocol::ExtensionExecutionReceipt>> {
        self.get("/extension-executions").await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn captured_extensions_keep_original_credentials_and_never_fall_back_after_rollover() {
        for response_status in [200, 404, 307, 308] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let mut paths = Vec::new();
                loop {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let mut bytes = Vec::new();
                    loop {
                        let mut chunk = [0_u8; 2048];
                        let n = stream.read(&mut chunk).await.unwrap();
                        assert!(n > 0 && bytes.len() + n <= 16384);
                        bytes.extend_from_slice(&chunk[..n]);
                        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                            let header =
                                String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                            let length = header
                                .lines()
                                .find_map(|line| line.strip_prefix("content-length: "))
                                .map_or(0, |n| n.parse::<usize>().unwrap());
                            if bytes.len() >= end + 4 + length {
                                break;
                            }
                        }
                    }
                    let header = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
                    let path = header
                        .lines()
                        .next()
                        .unwrap()
                        .split_whitespace()
                        .nth(1)
                        .unwrap()
                        .to_owned();
                    paths.push(path.clone());
                    let (status, body) = match path.as_str() {
                        "/v1/handshake" => (
                            "200 OK",
                            serde_json::json!({
                                "server_version":"test","protocol":{"minimum":PROTOCOL_VERSION_NUMBER,"maximum":PROTOCOL_VERSION_NUMBER},
                                "selected_protocol":PROTOCOL_VERSION_NUMBER,"instance_id":"original","daemon_generation":"generation",
                                "deployment":"personal","transport":"loopback","runtime_mode":"in_process",
                                "capabilities":[CAPABILITY_EXTENSION_CALLER_SCOPE],
                            }),
                        ),
                        "/v1/auth/whoami" => {
                            assert!(header.contains("authorization: bearer original-access\r\n"));
                            (
                                "200 OK",
                                serde_json::json!({"principal":{"id":1,"display_name":"owner","email":null,"avatar_url":null,"is_instance_admin":true},"memberships":[],"auth_session_id":null}),
                            )
                        }
                        "/v1/scoped/extension-executions" => {
                            assert!(header.contains("authorization: bearer original-access\r\n"));
                            assert!(header.contains("x-sift-expected-instance: original\r\n"));
                            assert!(header.contains("x-sift-expected-daemon: generation\r\n"));
                            assert!(header.contains("x-sift-expected-principal: 1\r\n"));
                            assert!(header.contains("x-sift-expected-auth-session: -\r\n"));
                            match response_status {
                                404 => (
                                    "404 Not Found",
                                    serde_json::json!({"kind":"not_found","message":"route unavailable"}),
                                ),
                                307 => (
                                    "307 Temporary Redirect",
                                    serde_json::json!({"kind":"redirect","message":"route moved"}),
                                ),
                                308 => (
                                    "308 Permanent Redirect",
                                    serde_json::json!({"kind":"redirect","message":"route moved"}),
                                ),
                                _ => ("200 OK", serde_json::json!([])),
                            }
                        }
                        _ => panic!("unexpected legacy or refresh request: {path}"),
                    };
                    let body = body.to_string();
                    stream.write_all(format!("HTTP/1.1 {status}\r\nlocation: /v1/extension-executions\r\nx-sift-protocol-version: {PROTOCOL_VERSION_NUMBER}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                    if path == "/v1/scoped/extension-executions" {
                        return paths;
                    }
                }
            });
            let tokens = AuthTokensResponse {
                access_token: "original-access".into(),
                access_expires_at: chrono::Utc::now() + chrono::Duration::minutes(10),
                refresh_token: "original-refresh".into(),
                refresh_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
            };
            let provider = SessionTokenProvider::new(tokens.clone());
            let client =
                Client::new(format!("http://{address}")).with_session_tokens(provider.clone());
            let captured = client
                .capture_extension_caller(&ExtensionCallerIdentity {
                    instance_id: "original".into(),
                    daemon_generation: "generation".into(),
                    principal_id: 1,
                    auth_session_id: None,
                })
                .await
                .unwrap();
            assert!(matches!(
                captured
                    .approve_operation(
                        "../../operation-approvals/00000000-0000-0000-0000-000000000001",
                        &ExpectedRevision {
                            expected_revision: 0
                        },
                    )
                    .await,
                Err(Error::Protocol(_))
            ));
            provider
                .replace(AuthTokensResponse {
                    access_token: "different-account".into(),
                    ..tokens
                })
                .await;
            let result = captured.extension_executions().await;
            if response_status != 200 {
                assert!(
                    matches!(result, Err(Error::Server { status, .. }) if status.as_u16() == response_status)
                );
            } else {
                assert!(result.unwrap().is_empty());
            }
            let paths = server.await.unwrap();
            assert_eq!(
                paths
                    .iter()
                    .filter(|p| p.starts_with("/v1/scoped/"))
                    .count(),
                1
            );
        }
    }
}
