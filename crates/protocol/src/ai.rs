//! Wire contract for desktop-run AI chats. No provider or storage code belongs here.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ToolContext;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AiVisibility {
    Private,
    RoomPublic,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AiChatPolicy {
    /// Visibility for newly created chats. Existing chats keep their label.
    pub new_chat_visibility: AiVisibility,
    pub max_context_sql_bytes: u64,
    pub max_tool_result_bytes: u64,
    pub max_tool_calls_per_run: u32,
    pub max_run_secs: u32,
    #[serde(default)]
    pub max_retention_days: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AiRetentionPolicy {
    pub tenant_id: i64,
    /// Tenant preference; None means retain until explicit deletion.
    pub retention_days: Option<u32>,
    /// Instance ceiling, applied even when the tenant has no preference.
    pub max_retention_days: Option<u32>,
    pub effective_retention_days: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SetAiRetentionRequest {
    pub retention_days: Option<u32>,
}

/// Human-reviewed source identity; contains no configuration or credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AiRoomPublicationPreview {
    pub tenant_id: i64,
    pub room_id: i64,
    pub profile_id: i64,
    pub profile_name: String,
    pub provider_id: crate::ProviderId,
    pub database: Option<String>,
    pub dialect: String,
    /// Pins binder/configuration/policy and opaque credential version identity.
    pub scope_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AiRoomPublication {
    pub id: Uuid,
    pub source: AiRoomPublicationPreview,
    /// Schema and estimated plans are published; rows require this extra grant.
    pub allow_rows: bool,
    pub created_by: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateAiRoomPublicationRequest {
    pub client_request_id: Uuid,
    pub expected_profile_id: i64,
    pub expected_scope_digest: String,
    #[serde(default)]
    pub allow_rows: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AiContentKeyRotation {
    pub rewritten_blobs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AiMode {
    Read,
    Propose,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AiProvider {
    Codex,
    ClaudeCode,
    OpenCode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AiRunStatus {
    Running,
    Completed,
    Failed,
    Canceled,
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AiEventKind {
    Started,
    MessageDelta,
    MessageCompleted,
    ProgressSummary,
    ToolRequested,
    ToolCompleted,
    ToolDenied,
    ProposalCreated,
    ProposalApplied,
    ProposalDiscarded,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AiProposalKind {
    QueryTextPatch,
    RowEditSet,
    MigrationDraft,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AiProposalStatus {
    Staged,
    Applied,
    Discarded,
    Conflicted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AiChat {
    pub id: Uuid,
    pub tenant_id: i64,
    pub room_id: Option<i64>,
    pub owner_principal_id: i64,
    pub visibility: AiVisibility,
    pub title: String,
    pub revision: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CreateAiChatRequest {
    pub tenant_id: i64,
    pub room_id: Option<i64>,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AiSqlContext {
    /// Exact editor or executed SQL, bounded by instance policy.
    pub text: String,
    /// A room document ID is required for automatic SQL in a public chat.
    pub room_document_id: Option<i64>,
    /// Stable content revision: first eight SHA-256 bytes, little-endian,
    /// with the sign bit cleared for SQLite's signed integer representation.
    pub document_revision: Option<u64>,
    pub selected_start: Option<u32>,
    pub selected_end: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AiTurnContext {
    pub target: ToolContext,
    /// Desktop-local SQL tab identity. Never used by the server as authority.
    #[serde(default)]
    pub editor_item_id: Option<u64>,
    pub database: Option<String>,
    pub dialect: Option<String>,
    pub environment_label: Option<String>,
    pub sql: Option<AiSqlContext>,
    pub current_error: Option<String>,
    pub staged_change_count: u32,
    /// Server-derived explicit public database publication for this turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publication_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<AiContextAttachment>,
}

/// References identify server-owned resources; inline client data is not accepted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AiAttachmentSource {
    QueryRows {
        result_id: Uuid,
        result_set: u32,
        schema_digest: String,
        row_ordinals: Vec<u64>,
        column_indices: Vec<u32>,
    },
    RoomRows {
        room_id: i64,
        result_id: crate::RoomResultId,
        result_set: u32,
        schema_digest: String,
        row_ordinals: Vec<u64>,
        column_indices: Vec<u32>,
    },
    QueryHistory {
        history_id: i64,
    },
    PlanCapture {
        capture_id: crate::PlanCaptureId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PreviewAiAttachmentRequest {
    pub target: ToolContext,
    pub source: AiAttachmentSource,
}

/// Immutable server-materialized content stored with the encrypted turn context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AiContextAttachment {
    pub source: AiAttachmentSource,
    pub label: String,
    pub content: serde_json::Value,
    pub sha256: String,
    pub truncated: bool,
    pub origin_visibility: AiVisibility,
    /// Server-derived attribution of an explicit private-to-room publication.
    pub published_by: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AiAttachmentPreview {
    pub id: Uuid,
    pub attachment: AiContextAttachment,
    pub visibility: AiVisibility,
    pub requires_publication_ack: bool,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AcceptAiAttachment {
    pub preview_id: Uuid,
    pub expected_sha256: String,
    #[serde(default)]
    pub publish_to_room: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StartAiTurnRequest {
    pub client_request_id: Uuid,
    pub desktop_id: Uuid,
    pub prompt: String,
    pub provider: AiProvider,
    pub model: Option<String>,
    pub mode: AiMode,
    pub context: AiTurnContext,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachment_previews: Vec<AcceptAiAttachment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AiRun {
    pub id: Uuid,
    pub chat_id: Uuid,
    pub desktop_id: Uuid,
    pub initiator_principal_id: i64,
    pub provider: AiProvider,
    pub model: Option<String>,
    pub mode: AiMode,
    pub status: AiRunStatus,
    pub next_sequence: u64,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AiRunEvent {
    pub run_id: Uuid,
    pub sequence: u64,
    pub kind: AiEventKind,
    pub at: DateTime<Utc>,
    /// Sensitive content is stored behind an opaque encrypted-content handle.
    pub content: Option<serde_json::Value>,
    pub tool_call_id: Option<Uuid>,
    pub proposal_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AiRunLease {
    pub run: AiRun,
    /// Ephemeral capability returned only to the initiating desktop.
    pub lease_token: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AiRunDetail {
    pub run: AiRun,
    pub prompt: String,
    pub context: AiTurnContext,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AppendAiEventRequest {
    pub client_event_id: Uuid,
    pub lease_token: Uuid,
    pub kind: AiEventKind,
    pub content: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FinishAiRunRequest {
    pub lease_token: Uuid,
    pub status: AiRunStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AiProposal {
    pub id: Uuid,
    pub chat_id: Uuid,
    pub run_id: Uuid,
    pub kind: AiProposalKind,
    pub status: AiProposalStatus,
    pub target: ToolContext,
    pub base_revision: Option<u64>,
    pub content_sha256: String,
    pub created_by: i64,
    pub applied_by: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StageAiQueryProposalRequest {
    pub client_request_id: Uuid,
    pub lease_token: Uuid,
    pub target: ToolContext,
    pub base_revision: u64,
    pub proposed_sql: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AiQueryProposalDetail {
    pub proposal: AiProposal,
    pub proposed_sql: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApplyAiQueryProposalRequest {
    /// Revision checked by the desktop immediately before the human edit.
    pub expected_revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AiToolKind {
    Schema,
    Catalog,
    Diagnostics,
    Explain,
    Select,
    QueryHistory,
    PlanCaptures,
    PlanCapture,
    ObjectDdl,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AiToolParameters {
    ObjectDdl {
        expected_catalog_revision: crate::CatalogRevision,
        object_id: crate::CatalogObjectId,
    },
    PlanCapture {
        capture_id: crate::PlanCaptureId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InvokeAiToolRequest {
    pub call_id: Uuid,
    pub lease_token: Uuid,
    pub tool: AiToolKind,
    /// SQL is required for diagnostics/explain/SELECT only. Never a target selector.
    pub sql: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<AiToolParameters>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InvokeAiToolResponse {
    pub call_id: Uuid,
    pub tool: AiToolKind,
    pub result: serde_json::Value,
    pub sift_restricted: bool,
}
