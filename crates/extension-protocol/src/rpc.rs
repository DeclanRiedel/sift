use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{ContributionId, ExtensionId, VersionRange, WireId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MethodFamilyRange {
    pub family: String,
    pub versions: VersionRange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    pub extension_rpc: VersionRange,
    pub method_families: Vec<MethodFamilyRange>,
    pub extension_id: ExtensionId,
    pub extension_version: String,
    pub manifest_sha256: String,
    pub process_nonce: WireId,
    pub contributions: Vec<ContributionId>,
    pub max_concurrent_requests: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Welcome {
    pub extension_rpc_version: u32,
    pub method_family_versions: BTreeMap<String, u32>,
    pub process_generation: WireId,
    pub granted_capabilities: Vec<String>,
    pub limits: RpcLimits,
    pub heartbeat_interval_ms: u32,
    pub max_concurrent_requests: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RpcLimits {
    pub max_frame_bytes: u32,
    pub max_row_bytes: u32,
    pub max_page_rows: u32,
    pub initial_stream_credit_bytes: u64,
    pub control_credit_bytes: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub id: WireId,
    pub contribution_id: ContributionId,
    pub method: String,
    pub payload: serde_json::Value,
    pub correlation_id: WireId,
    pub deadline_unix_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<RequestContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_id: Option<WireId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ResponseResult {
    Ok {
        payload: serde_json::Value,
    },
    Error {
        error: RpcError,
    },
    Stream {
        stream_id: WireId,
        payload: serde_json::Value,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub id: WireId,
    pub result: ResponseResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RpcError {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StreamFrame {
    pub stream_id: WireId,
    pub sequence: u64,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Credit {
    pub stream_id: WireId,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Cancel {
    pub request_id: WireId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Heartbeat {
    pub sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LogRecord {
    pub level: LogLevel,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Shutdown {
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", content = "body", rename_all = "snake_case")]
pub enum Message {
    Hello(Hello),
    Welcome(Welcome),
    Request(Request),
    Response(Response),
    Stream(StreamFrame),
    Credit(Credit),
    Cancel(Cancel),
    Heartbeat(Heartbeat),
    Log(LogRecord),
    Shutdown(Shutdown),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_golden_shape_is_stable() {
        let message = Message::Heartbeat(Heartbeat { sequence: 7 });
        assert_eq!(
            serde_json::to_string(&message).unwrap(),
            r#"{"kind":"heartbeat","body":{"sequence":7}}"#
        );
    }

    #[test]
    fn unknown_message_kind_is_rejected() {
        assert!(serde_json::from_str::<Message>(r#"{"kind":"future_kind","body":{}}"#).is_err());
    }
}
