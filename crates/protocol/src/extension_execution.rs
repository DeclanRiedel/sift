//! Redacted RPC observations; these do not attest native database outcomes.
use crate::{ContributionId, ExtensionId, OperationClassification, SegmentId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionExecutionState {
    Claimed,
    Dispatching,
    Acknowledged,
    Refused,
    OutcomeUnknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionResponseKind {
    Result,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionExecutionCode {
    Acknowledged,
    ReportedError,
    AdmissionRefused,
    RequestTimedOut,
    InvalidResponse,
    TransportFailed,
    OwnerInterrupted,
    DeliveryDenied,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExtensionExecutionReceipt {
    pub id: uuid::Uuid,
    pub request_id: uuid::Uuid,
    pub principal_id: i64,
    pub tenant_id: Option<i64>,
    pub operation_id: String,
    pub extension_id: ExtensionId,
    pub contribution_id: ContributionId,
    pub action: SegmentId,
    pub classification: OperationClassification,
    pub state: ExtensionExecutionState,
    pub created_at: String,
    pub deadline_unix_ms: i64,
    pub observed_at: Option<String>,
    /// None for an interrupted dispatch intent; true proves only writer entry.
    pub frame_started: Option<bool>,
    pub response_kind: Option<ExtensionResponseKind>,
    pub late_response: bool,
    pub result_code: Option<ExtensionExecutionCode>,
    pub audit_id: Option<i64>,
}
