use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::SegmentId;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InvokeActionRequest {
    pub action: SegmentId,
    pub target_kind: SegmentId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InvokeActionResponse {
    pub result: serde_json::Value,
}
