//! Human-approved query text edits. Scratch receipts identify companion evidence.
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestAiQueryExecution {
    pub call_id: Uuid,
    pub lease_token: Uuid,
    pub proposal_id: Uuid,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AiQueryExecutionTarget {
    RoomDocument { room_id: i64, document_id: i64 },
    PrivateEditor { desktop_id: Uuid, item_id: u64 },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AiQueryEditOutcome {
    Applied,
    Refused,
    OutcomeUnknown,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AiQueryEditEvidence {
    RoomCommit,
    CompanionAcknowledgement,
    NoDispatch,
    Interrupted,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AiQueryExecutionReceipt {
    pub outcome: AiQueryEditOutcome,
    pub evidence: AiQueryEditEvidence,
    pub before_sha256: String,
    pub after_sha256: Option<String>,
    pub document_sequence: Option<i64>,
    pub observed_at: DateTime<Utc>,
    /// A later companion observation does not rewrite the original unknown receipt.
    pub late_companion_outcome: Option<AiQueryEditOutcome>,
    pub late_observed_at: Option<DateTime<Utc>>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AiQueryExecutionSummary {
    pub id: Uuid,
    pub run_id: Uuid,
    pub chat_id: Uuid,
    pub proposal_id: Uuid,
    pub call_id: Uuid,
    pub initiator_id: i64,
    pub fingerprint: String,
    pub state: crate::AiExecutionState,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub receipt: Option<AiQueryExecutionReceipt>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AiQueryExecutionDetail {
    pub summary: AiQueryExecutionSummary,
    pub target: AiQueryExecutionTarget,
    pub original_sql: String,
    pub proposal: crate::AiQueryProposalDetail,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApproveAiQueryExecution {
    pub fingerprint: String,
    pub desktop_id: Uuid,
}
/// Returned only on first private-editor approval. Never expose this to a provider.
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct AiQueryEditorDispatch {
    pub token: Uuid,
    pub desktop_id: Uuid,
}
impl std::fmt::Debug for AiQueryEditorDispatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AiQueryEditorDispatch")
            .field("desktop_id", &self.desktop_id)
            .finish_non_exhaustive()
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AiQueryExecutionApproval {
    pub summary: AiQueryExecutionSummary,
    pub dispatch: Option<AiQueryEditorDispatch>,
}
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AcknowledgeAiQueryEditorExecution {
    pub fingerprint: String,
    pub desktop_id: Uuid,
    pub dispatch_token: Uuid,
    pub outcome: AiQueryEditOutcome,
    pub before_sha256: Option<String>,
    pub after_sha256: Option<String>,
}
impl std::fmt::Debug for AcknowledgeAiQueryEditorExecution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcknowledgeAiQueryEditorExecution")
            .field("desktop_id", &self.desktop_id)
            .field("outcome", &self.outcome)
            .finish_non_exhaustive()
    }
}
