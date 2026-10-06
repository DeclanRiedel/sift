//! Explicit external source management; credentials are input-only.
use super::*;
use sift_protocol::{
    ActivateAiExternalSourceRequest, AiExternalRoomGrant, AiExternalRoomSource, AiExternalSource,
    DiscoverAiExternalSourceRequest, PublishAiExternalSourceRequest,
};
use uuid::Uuid;

impl Client {
    pub async fn review_ai_external_room_grants(
        &self,
        room: i64,
    ) -> Result<Vec<sift_protocol::AiExternalRoomGrantHeader>> {
        self.get(&format!("/v1/ai/rooms/{room}/external-sources/review"))
            .await
    }

    pub async fn refresh_ai_external_source(
        &self,
        id: Uuid,
        request: &sift_protocol::RefreshAiExternalSourceRequest,
    ) -> Result<AiExternalSource> {
        self.post(&format!("/v1/ai/external-sources/{id}/refresh"), request)
            .await
    }

    pub async fn invoke_ai_external_inventory(
        &self,
        run: Uuid,
        request: &sift_protocol::InvokeAiExternalInventoryRequest,
    ) -> Result<sift_protocol::InvokeAiExternalInventoryResponse> {
        self.post(&format!("/v1/ai/runs/{run}/external-tools"), request)
            .await
    }

    pub async fn invoke_ai_external_read(
        &self,
        run: Uuid,
        request: &sift_protocol::InvokeAiExternalReadRequest,
    ) -> Result<sift_protocol::InvokeAiExternalReadResponse> {
        self.post(&format!("/v1/ai/runs/{run}/external-read"), request)
            .await
    }

    pub async fn ai_external_sources(&self, tenant: i64) -> Result<Vec<AiExternalSource>> {
        self.get(&format!("/v1/ai/external-sources?tenant_id={tenant}"))
            .await
    }
    pub async fn ai_external_source(&self, id: Uuid) -> Result<AiExternalSource> {
        self.get(&format!("/v1/ai/external-sources/{id}")).await
    }
    pub async fn discover_ai_external_source(
        &self,
        request: &DiscoverAiExternalSourceRequest,
    ) -> Result<AiExternalSource> {
        self.post("/v1/ai/external-sources", request).await
    }
    pub async fn activate_ai_external_source(
        &self,
        id: Uuid,
        request: &ActivateAiExternalSourceRequest,
    ) -> Result<AiExternalSource> {
        self.post(&format!("/v1/ai/external-sources/{id}/activate"), request)
            .await
    }
    pub async fn disable_ai_external_source(&self, id: Uuid, expected_revision: u64) -> Result<()> {
        self.post_empty_body(
            &format!("/v1/ai/external-sources/{id}/disable"),
            &sift_protocol::ExpectedRevision { expected_revision },
        )
        .await
    }
    pub async fn delete_ai_external_source(&self, id: Uuid, expected_revision: u64) -> Result<()> {
        self.delete(&format!(
            "/v1/ai/external-sources/{id}?expected_revision={expected_revision}"
        ))
        .await
    }
    pub async fn ai_external_room_sources(&self, room: i64) -> Result<Vec<AiExternalRoomSource>> {
        self.get(&format!("/v1/ai/rooms/{room}/external-sources"))
            .await
    }
    pub async fn publish_ai_external_source(
        &self,
        room: i64,
        request: &PublishAiExternalSourceRequest,
    ) -> Result<AiExternalRoomGrant> {
        self.post(&format!("/v1/ai/rooms/{room}/external-sources"), request)
            .await
    }
    pub async fn revoke_ai_external_source(&self, room: i64, grant: Uuid) -> Result<()> {
        self.delete(&format!("/v1/ai/rooms/{room}/external-sources/{grant}"))
            .await
    }
}
