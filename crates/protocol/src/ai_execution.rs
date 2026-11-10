//! Human-approved SQL execution. Bodies are encrypted by metadata, not stored here.
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestAiSqlExecution {
    pub call_id: Uuid,
    pub lease_token: Uuid,
    pub sql: String,
    #[serde(default)]
    pub params: Vec<crate::Value>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AiExecutionSource {
    pub tenant_id: i64,
    pub profile_id: i64,
    pub session: crate::SessionId,
    pub connection: crate::ConnectionId,
    pub generation: Uuid,
    pub profile_digest: String,
    pub database_label: String,
    pub production: bool,
    pub engine: crate::Engine,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AiSqlExecutionInput {
    pub sql: String,
    pub params: Vec<crate::Value>,
    pub source: AiExecutionSource,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AiExecutionState {
    AwaitingApproval,
    Applying,
    Completed,
    Denied,
    OutcomeUnknown,
    Reconciled,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AiExecutionReceipt {
    /// Native effects, independently observed through raw EOF. None for a read.
    pub native_outcome: Option<crate::ChangeLedgerOutcome>,
    pub affected_rows: Option<u64>,
    /// Bounded result observation. Absence never proves absence of native effects.
    pub result: Option<crate::ExecuteResponse>,
    pub result_code: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AiSqlExecutionSummary {
    pub id: Uuid,
    pub run_id: Uuid,
    pub chat_id: Uuid,
    pub call_id: Uuid,
    pub initiator_id: i64,
    pub approved_by: Option<i64>,
    pub fingerprint: String,
    pub state: AiExecutionState,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub receipt: Option<AiExecutionReceipt>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AiSqlExecutionDetail {
    pub summary: AiSqlExecutionSummary,
    pub input: AiSqlExecutionInput,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApproveAiSqlExecution {
    pub fingerprint: String,
    pub production_confirmation: Option<String>,
}

/// A provider requests review; it cannot supply approval or change the run source.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestAiDatabaseExecution {
    pub call_id: Uuid,
    pub lease_token: Uuid,
    pub proposal_id: Uuid,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApproveAiDatabaseExecution {
    pub fingerprint: String,
    pub production_confirmation: Option<String>,
    #[serde(default)]
    pub acknowledgements: Vec<crate::SchemaChangeRisk>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AiDatabaseExecutionSummary {
    pub id: Uuid,
    pub run_id: Uuid,
    pub chat_id: Uuid,
    pub proposal_id: Uuid,
    pub review_id: Uuid,
    pub call_id: Uuid,
    pub initiator_id: i64,
    pub fingerprint: String,
    pub state: AiExecutionState,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub receipt: Option<crate::AiDatabaseApplyReceipt>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AiDatabaseExecutionDetail {
    pub summary: AiDatabaseExecutionSummary,
    pub source: AiExecutionSource,
    pub proposal: crate::AiDatabaseProposalDetail,
    pub review: crate::AiDatabaseProposalReview,
}
