use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Inclusive application-protocol compatibility window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProtocolRange {
    pub minimum: u32,
    pub maximum: u32,
}

impl ProtocolRange {
    pub const fn exact(version: u32) -> Self {
        Self {
            minimum: version,
            maximum: version,
        }
    }

    pub const fn is_valid(self) -> bool {
        self.minimum <= self.maximum
    }

    pub const fn highest_common(self, other: Self) -> Option<u32> {
        let minimum = if self.minimum > other.minimum {
            self.minimum
        } else {
            other.minimum
        };
        let maximum = if self.maximum < other.maximum {
            self.maximum
        } else {
            other.maximum
        };
        if minimum <= maximum {
            Some(maximum)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HandshakeClientKind {
    Sdk,
    Native,
    Web,
    Automation,
    RemoteHelper,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HandshakeRequest {
    pub client_version: String,
    pub client_kind: HandshakeClientKind,
    pub protocol: ProtocolRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HandshakeDeployment {
    Personal,
    Team,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HandshakeTransport {
    Loopback,
    Network,
    SshProxy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HandshakeResponse {
    pub server_version: String,
    pub protocol: ProtocolRange,
    pub selected_protocol: u32,
    /// Stable opaque identity for one installed Sift instance.
    pub instance_id: String,
    /// Changes whenever the serving daemon process generation changes.
    pub daemon_generation: String,
    pub deployment: HandshakeDeployment,
    pub transport: HandshakeTransport,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_highest_common_version() {
        assert_eq!(
            ProtocolRange {
                minimum: 1,
                maximum: 3,
            }
            .highest_common(ProtocolRange {
                minimum: 2,
                maximum: 4,
            }),
            Some(3)
        );
    }

    #[test]
    fn disjoint_or_invalid_ranges_do_not_negotiate() {
        assert_eq!(
            ProtocolRange::exact(1).highest_common(ProtocolRange::exact(2)),
            None
        );
        assert!(!ProtocolRange {
            minimum: 3,
            maximum: 2,
        }
        .is_valid());
    }
}
