//! Human delegation review material; never exposed as provider approval tools.
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AiDelegationReviewState {
    AwaitingApproval,
    Approved,
    Consumed,
    Denied,
    Revoked,
}

/// This is a HUMAN endpoint. No provider tool exposes these fields.
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateAiDelegationReview {
    pub client_request_id: Uuid,
    pub desktop_id: Uuid,
    /// Server resolves and captures this target using the original caller.
    pub target: crate::ToolContext,
    pub policy: crate::AiDelegationPolicy,
    pub expires_at: DateTime<Utc>,
    #[serde(default)]
    pub material_references: Vec<AiDelegationMaterialReference>,
}
impl std::fmt::Debug for CreateAiDelegationReview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreateAiDelegationReview")
            .field("client_request_id", &self.client_request_id)
            .field("desktop_id", &self.desktop_id)
            .finish_non_exhaustive()
    }
}

/// This body, including the policy, is encrypted in the existing AI content store.
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AiDelegationReviewContent {
    pub policy: crate::AiDelegationPolicy,
    /// Original room/document/profile/connection context, not a later UI view.
    pub target: crate::ToolContext,
    pub source: crate::AiExecutionSource,
    pub scope_sha256: String,
    pub tenant_policy_revision: u64,
    pub desktop_id: Uuid,
    pub materials: Vec<AiDelegationReviewMaterial>,
}
impl std::fmt::Debug for AiDelegationReviewContent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AiDelegationReviewContent")
            .field("desktop_id", &self.desktop_id)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AiDelegationRunState {
    Active,
    Ended,
    Revoked,
    Interrupted,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AiDelegationReviewSummary {
    pub id: Uuid,
    pub chat_id: Uuid,
    pub tenant_id: i64,
    pub initiator_id: i64,
    pub approved_by: Option<i64>,
    pub state: AiDelegationReviewState,
    pub fingerprint: String,
    pub used_run_id: Option<Uuid>,
    /// Missing retained run evidence never makes a consumed review reusable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_state: Option<AiDelegationRunState>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct AiDelegationReviewDetail {
    pub summary: AiDelegationReviewSummary,
    pub content: AiDelegationReviewContent,
}
impl std::fmt::Debug for AiDelegationReviewDetail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AiDelegationReviewDetail")
            .field("summary", &self.summary)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApproveAiDelegationReview {
    pub fingerprint: String,
    pub production_confirmation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EndAiDelegationReview {
    pub fingerprint: String,
}

/// Historical provenance remains true after a delegation closes or is revoked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AiExecutionApprovalProvenance {
    /// Missing restored evidence never fabricates individual human approval.
    Unrecorded { approved_by: Option<i64> },
    HumanAction {
        approved_by: i64,
        approved_at: Option<DateTime<Utc>>,
    },
    RunDelegation {
        approved_by: i64,
        approved_at: DateTime<Utc>,
        review_id: Uuid,
        run_id: Uuid,
        rule_id: Uuid,
    },
}

/// Hosts choose prompts/tool descriptions from this server-authored strategy.
/// It is not an authority token and is not a provider-selected request field.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AiApprovalStrategy {
    #[default]
    HumanAction,
    RunDelegation,
}

/// Human-selected references; the server resolves native evidence itself.
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AiDelegationMaterialReference {
    MigrationPlan {
        rule_id: Uuid,
        plan_id: crate::MigrationPlanId,
    },
    ExtensionTool {
        rule_id: Uuid,
        tool_id: String,
    },
    RoomQuery {
        rule_id: Uuid,
    },
    /// Advisory local observation; actual dispatch requires the original editor.
    PrivateQuery {
        rule_id: Uuid,
        original_sql: String,
        base_revision: u64,
    },
}

/// Resolved private bodies are encrypted with their review, never stored in SQL.
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AiDelegationReviewMaterial {
    MigrationPlan {
        rule_id: Uuid,
        plan: crate::MigrationPlan,
        changes: Vec<crate::SchemaChange>,
    },
    ExtensionTool {
        rule_id: Uuid,
        tool: Box<crate::GovernedToolDescriptor>,
        context: crate::ToolContext,
        prepared_contract: serde_json::Value,
    },
    Query {
        rule_id: Uuid,
        original_sql: String,
        base_revision: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AiDelegationTenantPolicy {
    pub tenant_id: i64,
    pub revision: u64,
    pub enabled: bool,
    pub arbitrary_sql_enabled: bool,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetAiDelegationTenantPolicy {
    pub expected_revision: u64,
    pub enabled: bool,
    pub arbitrary_sql_enabled: bool,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PreviewAiDelegationMaterialsRequest {
    pub target: crate::ToolContext,
    pub material_references: Vec<AiDelegationMaterialReference>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AiDelegationMaterialFingerprint {
    MigrationPlan {
        rule_id: Uuid,
        contract_sha256: String,
        change_count: u32,
    },
    ExtensionTool {
        rule_id: Uuid,
        contract_sha256: String,
    },
    Query {
        rule_id: Uuid,
        before_sha256: String,
        base_revision: u64,
    },
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct AiDelegationMaterialsPreview {
    pub source: crate::AiExecutionSource,
    pub materials: Vec<AiDelegationReviewMaterial>,
    pub fingerprints: Vec<AiDelegationMaterialFingerprint>,
}
impl std::fmt::Debug for AiDelegationMaterialsPreview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AiDelegationMaterialsPreview")
            .field("fingerprints", &self.fingerprints)
            .finish_non_exhaustive()
    }
}
