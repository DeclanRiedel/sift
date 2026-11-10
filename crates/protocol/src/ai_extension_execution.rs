//! Human-reviewed extension requests; RPC observations do not attest native effects.
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A provider can request one tool from the original run context, never approve it.
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestAiExtensionExecution {
    pub call_id: Uuid,
    pub lease_token: Uuid,
    pub tool_id: String,
    pub arguments: serde_json::Value,
}
impl std::fmt::Debug for RequestAiExtensionExecution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestAiExtensionExecution")
            .field("call_id", &self.call_id)
            .field("tool_id", &self.tool_id)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AiExtensionResultAvailability {
    NotRecorded,
    Available,
    Unavailable,
    Withheld,
}

/// Redacted workflow state is derived from the linked immutable RPC journal.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AiExtensionExecutionSummary {
    pub id: Uuid,
    pub run_id: Uuid,
    pub chat_id: Uuid,
    pub call_id: Uuid,
    pub initiator_id: i64,
    pub approved_by: Option<i64>,
    pub tool_id: String,
    pub fingerprint: String,
    pub state: crate::AiExecutionState,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub execution_id: Option<Uuid>,
    /// None also covers missing restored journal evidence; state stays unknown.
    pub receipt: Option<crate::ExtensionExecutionReceipt>,
    pub result_availability: AiExtensionResultAvailability,
    /// Acknowledgement never changes original RPC evidence or permits redispatch.
    pub acknowledged_at: Option<DateTime<Utc>>,
}

/// Private review material is encrypted by metadata; no arguments enter V063.
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct AiExtensionExecutionReview {
    pub tool: crate::GovernedToolDescriptor,
    pub context: crate::ToolContext,
    pub arguments: serde_json::Value,
    /// Exact prepared registration/package/schema/limit contract, not authority.
    pub prepared_contract: serde_json::Value,
    pub source: crate::AiExecutionSource,
}
impl std::fmt::Debug for AiExtensionExecutionReview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AiExtensionExecutionReview")
            .field("tool_id", &self.tool.id)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct AiExtensionExecutionDetail {
    pub summary: AiExtensionExecutionSummary,
    pub review: AiExtensionExecutionReview,
    /// Authorized private content only; absence makes no claim about native effects.
    pub result: Option<serde_json::Value>,
}
impl std::fmt::Debug for AiExtensionExecutionDetail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AiExtensionExecutionDetail")
            .field("summary", &self.summary)
            .finish_non_exhaustive()
    }
}

/// This human route is never exposed as a provider tool.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApproveAiExtensionExecution {
    pub fingerprint: String,
    pub acknowledged_classification: crate::OperationClassification,
    pub production_confirmation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AcknowledgeAiExtensionExecution {
    pub fingerprint: String,
    pub execution_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DenyAiExtensionExecution {
    pub fingerprint: String,
}
