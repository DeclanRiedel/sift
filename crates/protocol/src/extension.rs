use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use sift_extension_protocol::{
    ContributionContext, ContributionId, ExtensionId, HostCapabilityKind, OperationClassification,
    SegmentId,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionProvenance {
    Bundled,
    Verified,
    Local,
    Development,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionLifecycleState {
    Installed,
    Disabled,
    Starting,
    Ready,
    Degraded,
    Quarantined,
    Uninstalled,
    Orphaned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionIsolation {
    HostEnforced,
    PlatformSandboxed,
    ProcessOnly,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExtensionDescriptor {
    pub id: ExtensionId,
    pub name: String,
    pub version: String,
    pub archive_sha256: String,
    pub manifest_sha256: String,
    pub provenance: ExtensionProvenance,
    pub lifecycle: ExtensionLifecycleState,
    pub isolation: ExtensionIsolation,
    pub enabled: bool,
    pub revision: u64,
    pub contributions: Vec<ContributionDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ContributionDescriptor {
    pub id: ContributionId,
    pub kind: String,
    pub display_name: String,
    pub active: bool,
    pub invocable: bool,
    #[serde(default)]
    pub required_capabilities: Vec<HostCapabilityKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<ExtensionActionDescriptor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<ClientContributionDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExtensionActionDescriptor {
    pub action: SegmentId,
    pub classification: OperationClassification,
    pub input_schema: serde_json::Value,
    pub output_schema: serde_json::Value,
    pub timeout_ms: u32,
    pub max_result_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClientContributionDescriptor {
    Command {
        title: String,
        action: SegmentId,
    },
    ContextAction {
        title: String,
        action: SegmentId,
        target_kind: SegmentId,
    },
    DetailPanel {
        title: String,
        fields: Vec<ClientFieldDescriptor>,
    },
    Form {
        title: String,
        action: SegmentId,
        schema: serde_json::Value,
    },
    Table {
        title: String,
        columns: Vec<ClientFieldDescriptor>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClientFieldDescriptor {
    pub key: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExtensionOperation {
    pub extension_id: ExtensionId,
    pub contribution_id: ContributionId,
    pub action: SegmentId,
    pub classification: OperationClassification,
    pub target_kind: SegmentId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
    #[serde(default)]
    pub sanitized_arguments: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InvokeExtensionRequest {
    pub operation: ExtensionOperation,
    pub arguments: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InvokeExtensionResponse {
    pub result: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum InvokeExtensionOutcome {
    Completed { result: serde_json::Value },
    ApprovalRequired { approval: OperationApproval },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CreateOperationApprovalRequest {
    pub operation: ExtensionOperation,
    pub input_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OperationApproval {
    pub id: String,
    pub principal_id: i64,
    pub operation_id: String,
    pub input_fingerprint: String,
    pub expires_at: String,
    pub approved_at: Option<String>,
    pub consumed_at: Option<String>,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExpectedRevision {
    pub expected_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExtensionSelectionRequest {
    pub enabled: bool,
    pub expected_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExtensionGrantRequest {
    pub granted: Vec<HostCapabilityKind>,
    pub expected_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExtensionTenantSelectionRequest {
    pub allowed: bool,
    pub expected_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ValidatedExtensionPackage {
    pub extension_id: ExtensionId,
    pub name: String,
    pub version: String,
    pub archive_sha256: String,
    pub manifest_sha256: String,
    pub signed: bool,
    pub contributions: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExtensionDiagnostics {
    pub extension_id: ExtensionId,
    pub lifecycle: ExtensionLifecycleState,
    pub quarantine_reason: Option<String>,
    pub generation_health: Option<String>,
    pub messages: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExtensionPurgeResponse {
    pub purged_namespaces: u64,
}
