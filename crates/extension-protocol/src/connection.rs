use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{ConnectionDisposition, CredentialField, WireId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DatabaseEndpoint {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResolveCredentialsRequest {
    pub configuration: serde_json::Value,
    pub required_fields: Vec<String>,
    pub secret_handles: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResolveCredentialsResponse {
    pub credentials: Vec<CredentialField>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_unix_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenTunnelRequest {
    pub endpoint: DatabaseEndpoint,
    pub configuration: serde_json::Value,
    pub credentials: Vec<CredentialField>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenTunnelResponse {
    pub endpoint: DatabaseEndpoint,
    pub lease: WireId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_unix_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionHookStage {
    PreResolve,
    PostResolve,
    PreConnect,
    PostConnect,
    ConnectionFailed,
    PreClose,
    PostClose,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectionHookRequest {
    pub stage: ConnectionHookStage,
    pub configuration: serde_json::Value,
    pub logical_endpoint: DatabaseEndpoint,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<SanitizedServerInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SanitizedServerInfo {
    pub server_version: String,
    pub current_database: String,
    pub current_user: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectionHookResponse {
    #[serde(default)]
    pub configuration_patch: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub disposition: Option<ConnectionDisposition>,
}
