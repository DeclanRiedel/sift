use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Non-secret, backend-owned transport configuration stored in a profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TailnetSettings {
    pub mode: TailnetMode,
    #[serde(default)]
    pub ssh_user: String,
    #[serde(default = "ssh_port")]
    pub ssh_port: u16,
    /// Explicitly verified public SSH host key. Never a private key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_key: Option<String>,
}
fn ssh_port() -> u16 {
    22
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TailnetMode {
    Direct,
    Automatic,
    Tunnel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TailnetPeer {
    pub name: String,
    pub address: String,
    pub online: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TailnetStatus {
    pub backend_state: String,
    pub backend_name: String,
    pub peers: Vec<TailnetPeer>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TailnetProbeRequest {
    pub host: String,
    pub port: u16,
    pub settings: TailnetSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TailnetProbeReport {
    pub stage: String,
    pub reachable: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TailnetHostKey {
    pub key: String,
    pub fingerprint: String,
    pub host: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TailnetServeAction {
    Preview,
    Apply,
    Inspect,
    Remove,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TailnetServeRequest {
    pub connection: TailnetProbeRequest,
    pub action: TailnetServeAction,
    #[serde(default)]
    pub acknowledge_exposure: bool,
    pub expected_revision: Option<String>,
    #[serde(default)]
    pub expected_instance_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TailnetServeReport {
    #[serde(default)]
    pub instance_id: String,
    pub revision: String,
    pub owned: bool,
    pub configured: bool,
    pub message: String,
}
