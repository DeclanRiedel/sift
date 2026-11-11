//! Fixed caller transport for setup and the shared HTTP AI operation contracts.
use super::*;
use sift_protocol::handshake::{ExtensionCallerIdentity, CAPABILITY_AI_CALLER_SCOPE};

/// No ordinary Client, credential mutation, redirect or transport-rebinding
/// escape is exposed. Unsupported scoped routes fail instead of falling back.
#[derive(Clone)]
pub struct ScopedAiClient {
    scoped: ScopedExtensionClient,
}

impl Client {
    pub async fn capture_ai_caller(
        &self,
        expected: &ExtensionCallerIdentity,
    ) -> Result<ScopedAiClient> {
        let scoped = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            self.capture_caller_inner(expected, CAPABILITY_AI_CALLER_SCOPE, true),
        )
        .await
        .map_err(|_| Error::Timeout("AI caller capture"))??;
        Ok(ScopedAiClient { scoped })
    }
}

impl ScopedAiClient {
    fn path(path: &str) -> Result<&str> {
        path.strip_prefix("/v1")
            .filter(|rest| rest.starts_with('/'))
            .ok_or_else(|| Error::Protocol("Captured AI request requires a versioned path".into()))
    }
    pub(super) async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.scoped.get(Self::path(path)?).await
    }
    pub(super) async fn post<T: serde::de::DeserializeOwned, B: serde::Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        self.scoped.post(Self::path(path)?, body).await
    }
    pub(super) async fn put<T: serde::de::DeserializeOwned, B: serde::Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        self.scoped.put(Self::path(path)?, body).await
    }
    pub(super) async fn delete(&self, path: &str) -> Result<()> {
        self.scoped.delete(Self::path(path)?).await
    }
    pub(super) async fn post_empty_body<B: serde::Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<()> {
        let _: serde_json::Value = self.post(path, body).await?;
        Ok(())
    }

    pub async fn vaults(&self, tenant: TenantId) -> Result<Vec<Vault>> {
        self.get(&format!("/v1/metadata/vaults?tenant={}", tenant.0))
            .await
    }
    pub async fn room_results(&self, room: RoomId) -> Result<Vec<RoomQueryResult>> {
        self.get(&format!("/v1/metadata/rooms/{}/results", room.0))
            .await
    }
    pub async fn managed_review_ai_database_proposal(
        &self,
        id: uuid::Uuid,
        request: &sift_protocol::ManagedAiDatabaseReviewRequest,
    ) -> Result<sift_protocol::AiDatabaseProposalReview> {
        self.post(
            &format!("/v1/ai/database-proposals/{id}/managed-review"),
            request,
        )
        .await
    }
    pub async fn commit_room_ai_query_proposal(
        &self,
        chat: uuid::Uuid,
        proposal: uuid::Uuid,
        request: &sift_protocol::CommitAiRoomQueryProposalRequest,
    ) -> Result<sift_protocol::AiRoomQueryProposalCommit> {
        self.post(
            &format!("/v1/ai/chats/{chat}/query-proposals/{proposal}/room-commit"),
            request,
        )
        .await
    }
    pub async fn room_ai_query_commit(
        &self,
        chat: uuid::Uuid,
        proposal: uuid::Uuid,
    ) -> Result<sift_protocol::AiRoomQueryProposalCommit> {
        self.get(&format!(
            "/v1/ai/chats/{chat}/query-proposals/{proposal}/room-commit"
        ))
        .await
    }
    pub async fn whoami(&self) -> Result<WhoAmIResponse> {
        self.get("/v1/auth/whoami").await
    }
    pub async fn open_session(&self, tag: Option<String>) -> Result<SessionInfo> {
        self.open_session_for_tenant(tag, None).await
    }
    pub async fn open_session_for_tenant(
        &self,
        tag: Option<String>,
        tenant_id: Option<i64>,
    ) -> Result<SessionInfo> {
        self.post("/v1/sessions", &OpenSessionRequest { tag, tenant_id })
            .await
    }
    pub async fn close_session(&self, session: SessionId) -> Result<()> {
        self.delete(&format!("/v1/sessions/{session}")).await
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
    pub async fn close_connection(
        &self,
        session: SessionId,
        connection: ConnectionId,
    ) -> Result<()> {
        self.delete(&format!("/v1/sessions/{session}/connections/{connection}"))
            .await
    }
    pub async fn ping_connection(
        &self,
        session: SessionId,
        connection: ConnectionId,
    ) -> Result<ServerInfo> {
        self.get(&format!(
            "/v1/sessions/{session}/connections/{connection}/ping"
        ))
        .await
    }
}

impl ScopedAiClient {
    pub async fn preview_ai_delegation_materials(
        &self,
        chat: uuid::Uuid,
        request: &sift_protocol::PreviewAiDelegationMaterialsRequest,
    ) -> Result<sift_protocol::AiDelegationMaterialsPreview> {
        self.post(
            &format!("/v1/ai/chats/{chat}/delegation-materials"),
            request,
        )
        .await
    }
    pub async fn create_ai_delegation_review(
        &self,
        chat: uuid::Uuid,
        request: &sift_protocol::CreateAiDelegationReview,
    ) -> Result<sift_protocol::AiDelegationReviewDetail> {
        self.post(&format!("/v1/ai/chats/{chat}/delegation-reviews"), request)
            .await
    }
    pub async fn ai_delegation_reviews(
        &self,
        chat: uuid::Uuid,
    ) -> Result<Vec<sift_protocol::AiDelegationReviewSummary>> {
        self.get(&format!("/v1/ai/chats/{chat}/delegation-reviews"))
            .await
    }
    pub async fn ai_delegation_review(
        &self,
        id: uuid::Uuid,
    ) -> Result<sift_protocol::AiDelegationReviewDetail> {
        self.get(&format!("/v1/ai/delegation-reviews/{id}")).await
    }
    pub async fn approve_ai_delegation_review(
        &self,
        id: uuid::Uuid,
        request: &sift_protocol::ApproveAiDelegationReview,
    ) -> Result<sift_protocol::AiDelegationReviewSummary> {
        self.post(&format!("/v1/ai/delegation-reviews/{id}/approve"), request)
            .await
    }
    pub async fn deny_ai_delegation_review(
        &self,
        id: uuid::Uuid,
        request: &sift_protocol::EndAiDelegationReview,
    ) -> Result<sift_protocol::AiDelegationReviewSummary> {
        self.post(&format!("/v1/ai/delegation-reviews/{id}/deny"), request)
            .await
    }
    pub async fn revoke_ai_delegation_review(
        &self,
        id: uuid::Uuid,
        request: &sift_protocol::EndAiDelegationReview,
    ) -> Result<sift_protocol::AiDelegationReviewSummary> {
        self.post(&format!("/v1/ai/delegation-reviews/{id}/revoke"), request)
            .await
    }
    pub async fn ai_delegation_tenant_policy(
        &self,
        tenant: TenantId,
    ) -> Result<sift_protocol::AiDelegationTenantPolicy> {
        self.get(&format!("/v1/ai/tenants/{}/delegation-policy", tenant.0))
            .await
    }
    pub async fn set_ai_delegation_tenant_policy(
        &self,
        tenant: TenantId,
        request: &sift_protocol::SetAiDelegationTenantPolicy,
    ) -> Result<sift_protocol::AiDelegationTenantPolicy> {
        self.put(
            &format!("/v1/ai/tenants/{}/delegation-policy", tenant.0),
            request,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn absent_ai_scope_capability_refuses_before_identity_or_setup() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut buffer = [0; 2048];
                let n = socket.read(&mut buffer).await.unwrap();
                assert!(n > 0 && bytes.len() + n <= 16384);
                bytes.extend_from_slice(&buffer[..n]);
                if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                    let length = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .map_or(0, |n| n.parse::<usize>().unwrap());
                    if bytes.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            assert!(bytes.starts_with(b"POST /v1/handshake "));
            let body=serde_json::json!({"server_version":"fixture","protocol":{"minimum":PROTOCOL_VERSION_NUMBER,"maximum":PROTOCOL_VERSION_NUMBER},"selected_protocol":PROTOCOL_VERSION_NUMBER,"instance_id":"instance","daemon_generation":"daemon","deployment":"personal","transport":"loopback","runtime_mode":"in_process","capabilities":[sift_protocol::handshake::CAPABILITY_EXTENSION_CALLER_SCOPE]}).to_string();
            socket.write_all(format!("HTTP/1.1 200 OK\r\nx-sift-protocol-version: {PROTOCOL_VERSION_NUMBER}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        });
        let result = Client::new(format!("http://{address}"))
            .capture_ai_caller(&ExtensionCallerIdentity {
                instance_id: "instance".into(),
                daemon_generation: "daemon".into(),
                principal_id: 1,
                auth_session_id: None,
            })
            .await;
        assert!(
            matches!(result,Err(Error::Protocol(message)) if message.contains("caller scoping"))
        );
        server.await.unwrap();
    }
}
