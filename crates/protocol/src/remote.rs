use crate::{ProtocolRange, SshProxyCapabilityClaims};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RemoteDaemonDescriptor {
    pub instance_id: String,
    pub daemon_generation: String,
    pub pid: u32,
    pub endpoint: String,
    pub server_version: String,
    pub protocol: ProtocolRange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RemoteProbeResponse {
    pub schema_version: u8,
    pub server_version: String,
    pub protocol: ProtocolRange,
    pub target_os: String,
    pub target_arch: String,
    pub install_layout_version: u8,
    pub daemon: Option<RemoteDaemonDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RemoteKeyChallenge {
    pub nonce: String,
    pub message: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct RemoteCapabilityResponse {
    pub capability: String,
    pub claims: SshProxyCapabilityClaims,
    pub daemon: RemoteDaemonDescriptor,
}

impl fmt::Debug for RemoteCapabilityResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteCapabilityResponse")
            .field("capability", &"[REDACTED]")
            .field("claims", &self.claims)
            .field("daemon", &self.daemon)
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct RemoteReady {
    pub local_base_url: String,
    pub access_token: String,
    pub access_expires_at: DateTime<Utc>,
    pub principal_id: i64,
    pub instance_id: String,
    pub daemon_generation: String,
    pub server_version: String,
    pub selected_protocol: u32,
}

impl fmt::Debug for RemoteReady {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteReady")
            .field("local_base_url", &self.local_base_url)
            .field("access_token", &"[REDACTED]")
            .field("access_expires_at", &self.access_expires_at)
            .field("principal_id", &self.principal_id)
            .field("instance_id", &self.instance_id)
            .field("daemon_generation", &self.daemon_generation)
            .field("server_version", &self.server_version)
            .field("selected_protocol", &self.selected_protocol)
            .finish()
    }
}
