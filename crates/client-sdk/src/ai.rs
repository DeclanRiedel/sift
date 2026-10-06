use super::*;
use sift_protocol::{
    AiChat, AiQueryProposalDetail, AiRunDetail, AiRunEvent, AiRunLease, AppendAiEventRequest,
    ApplyAiQueryProposalRequest, CreateAiChatRequest, FinishAiRunRequest, InvokeAiToolRequest,
    InvokeAiToolResponse, StageAiQueryProposalRequest, StartAiTurnRequest,
};
use uuid::Uuid;

impl Client {
    pub async fn stage_ai_database_proposal(
        &self,
        run: Uuid,
        request: &sift_protocol::StageAiDatabaseProposalRequest,
    ) -> Result<sift_protocol::AiDatabaseProposalDetail> {
        self.post(&format!("/v1/ai/runs/{run}/database-proposals"), request)
            .await
    }
    pub async fn ai_database_proposals(
        &self,
        chat: Uuid,
    ) -> Result<Vec<sift_protocol::AiDatabaseProposalDetail>> {
        self.get(&format!("/v1/ai/chats/{chat}/database-proposals"))
            .await
    }
    pub async fn review_ai_database_proposal(
        &self,
        id: Uuid,
        request: &sift_protocol::ReviewAiDatabaseProposalRequest,
    ) -> Result<sift_protocol::AiDatabaseProposalReview> {
        self.post(&format!("/v1/ai/database-proposals/{id}/review"), request)
            .await
    }
    pub async fn apply_ai_database_proposal(
        &self,
        id: Uuid,
        request: &sift_protocol::ApplyAiDatabaseProposalRequest,
    ) -> Result<sift_protocol::AiDatabaseApplyReceipt> {
        self.post(&format!("/v1/ai/database-proposals/{id}/apply"), request)
            .await
    }
    pub async fn discard_ai_database_proposal(
        &self,
        id: Uuid,
    ) -> Result<sift_protocol::AiDatabaseProposalDetail> {
        self.post(
            &format!("/v1/ai/database-proposals/{id}/discard"),
            &serde_json::json!({}),
        )
        .await
    }

    pub async fn preview_ai_room_publication(
        &self,
        room: i64,
    ) -> Result<sift_protocol::AiRoomPublicationPreview> {
        self.get(&format!("/v1/ai/rooms/{room}/publication/preview"))
            .await
    }
    pub async fn ai_room_publication(
        &self,
        room: i64,
    ) -> Result<Option<sift_protocol::AiRoomPublication>> {
        self.get(&format!("/v1/ai/rooms/{room}/publication")).await
    }
    pub async fn create_ai_room_publication(
        &self,
        room: i64,
        request: &sift_protocol::CreateAiRoomPublicationRequest,
    ) -> Result<sift_protocol::AiRoomPublication> {
        self.post(&format!("/v1/ai/rooms/{room}/publication"), request)
            .await
    }
    pub async fn revoke_ai_room_publication(&self, room: i64) -> Result<()> {
        self.delete(&format!("/v1/ai/rooms/{room}/publication"))
            .await
    }

    pub async fn ai_policy(&self) -> Result<sift_protocol::AiChatPolicy> {
        self.get("/v1/ai/policy").await
    }

    pub async fn ai_retention(&self, tenant_id: i64) -> Result<sift_protocol::AiRetentionPolicy> {
        self.get(&format!("/v1/ai/tenants/{tenant_id}/retention"))
            .await
    }
    pub async fn set_ai_retention(
        &self,
        tenant_id: i64,
        request: &sift_protocol::SetAiRetentionRequest,
    ) -> Result<sift_protocol::AiRetentionPolicy> {
        self.put(&format!("/v1/ai/tenants/{tenant_id}/retention"), request)
            .await
    }

    pub async fn rotate_ai_content_key(
        &self,
        tenant_id: i64,
    ) -> Result<sift_protocol::AiContentKeyRotation> {
        self.post(
            &format!("/v1/ai/tenants/{tenant_id}/content-key/rotate"),
            &serde_json::json!({}),
        )
        .await
    }

    pub async fn create_ai_chat(&self, request: &CreateAiChatRequest) -> Result<AiChat> {
        self.post("/v1/ai/chats", request).await
    }

    pub async fn ai_chats(&self, tenant_id: i64) -> Result<Vec<AiChat>> {
        self.get(&format!("/v1/ai/chats?tenant_id={tenant_id}"))
            .await
    }

    pub async fn ai_chat(&self, id: Uuid) -> Result<AiChat> {
        self.get(&format!("/v1/ai/chats/{id}")).await
    }

    pub async fn delete_ai_chat(&self, id: Uuid) -> Result<()> {
        self.delete(&format!("/v1/ai/chats/{id}")).await
    }

    pub async fn start_ai_turn(
        &self,
        chat_id: Uuid,
        request: &StartAiTurnRequest,
    ) -> Result<AiRunLease> {
        self.post(&format!("/v1/ai/chats/{chat_id}/runs"), request)
            .await
    }

    pub async fn ai_runs(&self, chat_id: Uuid) -> Result<Vec<AiRunDetail>> {
        self.get(&format!("/v1/ai/chats/{chat_id}/runs")).await
    }

    pub async fn ai_events(&self, run_id: Uuid, after: u64) -> Result<Vec<AiRunEvent>> {
        self.get(&format!("/v1/ai/runs/{run_id}/events?after={after}"))
            .await
    }

    pub async fn append_ai_event(
        &self,
        run_id: Uuid,
        request: &AppendAiEventRequest,
    ) -> Result<AiRunEvent> {
        self.post(&format!("/v1/ai/runs/{run_id}/events"), request)
            .await
    }

    pub async fn finish_ai_run(&self, run_id: Uuid, request: &FinishAiRunRequest) -> Result<()> {
        let _: serde_json::Value = self
            .post(&format!("/v1/ai/runs/{run_id}/finish"), request)
            .await?;
        Ok(())
    }

    pub async fn invoke_ai_tool(
        &self,
        run_id: Uuid,
        request: &InvokeAiToolRequest,
    ) -> Result<InvokeAiToolResponse> {
        self.post(&format!("/v1/ai/runs/{run_id}/tools"), request)
            .await
    }

    pub async fn stage_ai_query_proposal(
        &self,
        run_id: Uuid,
        request: &StageAiQueryProposalRequest,
    ) -> Result<AiQueryProposalDetail> {
        self.post(&format!("/v1/ai/runs/{run_id}/query-proposals"), request)
            .await
    }

    pub async fn ai_query_proposals(&self, chat_id: Uuid) -> Result<Vec<AiQueryProposalDetail>> {
        self.get(&format!("/v1/ai/chats/{chat_id}/query-proposals"))
            .await
    }

    pub async fn discard_ai_query_proposal(
        &self,
        chat_id: Uuid,
        proposal_id: Uuid,
    ) -> Result<AiQueryProposalDetail> {
        self.post(
            &format!("/v1/ai/chats/{chat_id}/query-proposals/{proposal_id}/discard"),
            &serde_json::json!({}),
        )
        .await
    }

    pub async fn apply_ai_query_proposal(
        &self,
        chat_id: Uuid,
        proposal_id: Uuid,
        expected_revision: u64,
    ) -> Result<AiQueryProposalDetail> {
        self.post(
            &format!("/v1/ai/chats/{chat_id}/query-proposals/{proposal_id}/apply"),
            &ApplyAiQueryProposalRequest { expected_revision },
        )
        .await
    }
}
