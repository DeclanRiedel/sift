//! Explicit external AI source registration. Credentials are input-only.
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum AiMcpRevision {
    #[serde(rename = "2026-07-28")]
    Modern20260728,
    #[serde(rename = "2025-11-25")]
    Legacy20251125,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AiExternalToolPolicy {
    Read,
    LocalQueryDraft,
    LocalRowDraft,
    LocalMigrationDraft,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AiExternalSourceState {
    Draft,
    Active,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AiExternalToolDefinition {
    pub name: String,
    pub alias: String,
    pub title: Option<String>,
    pub description: String,
    pub input_schema: Value,
    pub output_schema: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Value>,
    pub schema_sha256: String,
    pub policy: AiExternalToolPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AiExternalSourceDefinition {
    pub label: String,
    pub endpoint: String,
    pub protocol: AiMcpRevision,
    pub tools: Vec<AiExternalToolDefinition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AiExternalSource {
    pub id: Uuid,
    pub tenant_id: i64,
    pub owner_principal_id: i64,
    pub vault_id: Option<i64>,
    pub revision: u64,
    pub state: AiExternalSourceState,
    pub definition: AiExternalSourceDefinition,
    pub config_sha256: String,
    pub credential_identity: Uuid,
    pub credential_configured: bool,
    pub credential_scope_reviewed: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DiscoverAiExternalSourceRequest {
    pub tenant_id: i64,
    pub vault_id: Option<i64>,
    pub label: String,
    pub endpoint: String,
    pub protocol: AiMcpRevision,
    pub bearer_token: Option<String>,
}

impl std::fmt::Debug for DiscoverAiExternalSourceRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DiscoverAiExternalSourceRequest")
            .field("tenant_id", &self.tenant_id)
            .field("vault_id", &self.vault_id)
            .field("credential_configured", &self.bearer_token.is_some())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AiExternalToolApproval {
    pub alias: String,
    pub expected_schema_sha256: String,
    pub policy: AiExternalToolPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ActivateAiExternalSourceRequest {
    pub expected_revision: u64,
    pub expected_config_sha256: String,
    pub credential_scope_reviewed: bool,
    pub tools: Vec<AiExternalToolApproval>,
}

/// Only opaque source pins and its reviewed label enter an AI turn. No endpoint,
/// credential bytes or SecretStore handles accompany this proof.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AiExternalSourceProof {
    pub source_id: Uuid,
    pub source_revision: u64,
    pub config_sha256: String,
    pub credential_identity: Uuid,
    pub label: String,
    pub room_grant_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AiExternalRoomGrant {
    pub id: Uuid,
    pub room_id: i64,
    pub source: AiExternalSourceProof,
    pub tool_aliases: Vec<String>,
    pub published_by: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PublishAiExternalSourceRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_grant_id: Option<Uuid>,
    pub source: AiExternalSourceProof,
    pub tool_aliases: Vec<String>,
    pub publish_future_results_to_room: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InvokeAiExternalReadRequest {
    pub call_id: Uuid,
    pub lease_token: Uuid,
    pub source_id: Uuid,
    pub tool_alias: String,
    pub arguments: Value,
}

/// Room readers receive only reviewed aliases and schemas, never private endpoints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AiExternalRoomSource {
    pub grant: AiExternalRoomGrant,
    pub tools: Vec<AiExternalToolDefinition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InvokeAiExternalReadResponse {
    pub call_id: Uuid,
    pub source: AiExternalSourceProof,
    pub tool_alias: String,
    pub result: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InvokeAiExternalInventoryRequest {
    pub call_id: Uuid,
    pub lease_token: Uuid,
    pub source_id: Uuid,
    pub tool_alias: Option<String>,
    #[serde(default)]
    pub offset: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InvokeAiExternalInventoryResponse {
    pub call_id: Uuid,
    pub source: AiExternalSourceProof,
    pub result: Value,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum AiExternalCredentialUpdate {
    Keep,
    Replace { bearer_token: String },
    Clear,
}
impl std::fmt::Debug for AiExternalCredentialUpdate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Keep => "Keep",
            Self::Replace { .. } => "Replace { [redacted] }",
            Self::Clear => "Clear",
        })
    }
}
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RefreshAiExternalSourceRequest {
    pub expected_revision: u64,
    pub label: String,
    pub endpoint: String,
    pub protocol: AiMcpRevision,
    pub credentials: AiExternalCredentialUpdate,
}
impl std::fmt::Debug for RefreshAiExternalSourceRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RefreshAiExternalSourceRequest")
            .field("expected_revision", &self.expected_revision)
            .field("credentials", &self.credentials)
            .finish_non_exhaustive()
    }
}

/// Operator review uses safe grant headers even when old pins were invalidated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AiExternalRoomGrantHeader {
    pub id: Uuid,
    pub room_id: i64,
    pub source_id: Uuid,
    pub published_by: i64,
}
