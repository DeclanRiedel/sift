use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{ContributionContext, ExtensionOperation, OperationApproval, OperationClassification};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GovernedToolDescriptor {
    pub id: String,
    pub title: String,
    pub description: String,
    pub operation: ExtensionOperation,
    pub input_schema: serde_json::Value,
    pub output_schema: serde_json::Value,
    pub required_context: Vec<ContributionContext>,
    pub mcp_exposable: bool,
    pub schedulable: bool,
    pub interactive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ToolContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InvokeToolRequest {
    pub tool_id: String,
    pub arguments: serde_json::Value,
    pub context: ToolContext,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum InvokeToolResponse {
    Completed { result: serde_json::Value },
    ApprovalRequired { approval: OperationApproval },
}

pub fn classification_requires_approval(classification: OperationClassification) -> bool {
    matches!(
        classification,
        OperationClassification::Write
            | OperationClassification::Destructive
            | OperationClassification::Administrative
    )
}
