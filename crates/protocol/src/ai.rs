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
    /// Stable content revision of the complete SQL text (first 64 bits of SHA-256).
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
    Diagnostics,
    Explain,
    Select,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InvokeAiToolRequest {
    pub call_id: Uuid,
    pub lease_token: Uuid,
    pub tool: AiToolKind,
    /// SQL is required except for schema. It is never accepted as a target selector.
    pub sql: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InvokeAiToolResponse {
    pub call_id: Uuid,
    pub tool: AiToolKind,
    pub result: serde_json::Value,
    pub sift_restricted: bool,
}
