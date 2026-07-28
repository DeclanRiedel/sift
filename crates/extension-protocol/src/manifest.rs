use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{DialectId, ExtensionId, ProviderId, SegmentId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VersionRange {
    pub minimum: u32,
    pub maximum: u32,
}

impl VersionRange {
    pub const fn contains(self, version: u32) -> bool {
        self.minimum <= version && version <= self.maximum
    }

    pub const fn is_valid(self) -> bool {
        self.minimum > 0 && self.minimum <= self.maximum
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Compatibility {
    pub public_protocol: VersionRange,
    pub extension_rpc: VersionRange,
    pub driver_rpc: VersionRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleMode {
    Lazy,
    Eager,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Lifecycle {
    #[serde(default = "default_lifecycle_mode")]
    pub mode: LifecycleMode,
    #[serde(default = "default_readiness_ms")]
    pub readiness_deadline_ms: u32,
    #[serde(default = "default_idle_ms")]
    pub idle_timeout_ms: u32,
}

const fn default_lifecycle_mode() -> LifecycleMode {
    LifecycleMode::Lazy
}

const fn default_readiness_ms() -> u32 {
    10_000
}

const fn default_idle_ms() -> u32 {
    300_000
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self {
            mode: default_lifecycle_mode(),
            readiness_deadline_ms: default_readiness_ms(),
            idle_timeout_ms: default_idle_ms(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum HostCapabilityKind {
    #[serde(rename = "database.connect")]
    DatabaseConnect,
    #[serde(rename = "secret.receive")]
    SecretReceive,
    #[serde(rename = "network.connect")]
    NetworkConnect,
    #[serde(rename = "network.listen.loopback")]
    NetworkListenLoopback,
    #[serde(rename = "filesystem.data")]
    FilesystemData,
    #[serde(rename = "filesystem.read")]
    FilesystemRead,
    #[serde(rename = "filesystem.write")]
    FilesystemWrite,
    #[serde(rename = "process.spawn")]
    ProcessSpawn,
    #[serde(rename = "http.fetch")]
    HttpFetch,
    #[serde(rename = "storage.kv")]
    StorageKv,
    #[serde(rename = "operation.invoke")]
    OperationInvoke,
    #[serde(rename = "event.publish")]
    EventPublish,
    #[serde(rename = "tool.register")]
    ToolRegister,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CapabilityRequest {
    pub kind: HostCapabilityKind,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub constraints: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TargetArtifact {
    pub target: String,
    pub path: String,
    pub sha256: String,
    pub byte_length: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DataFile {
    pub path: String,
    pub sha256: String,
    pub byte_length: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DatabaseProviderContribution {
    pub id: SegmentId,
    pub provider_id: ProviderId,
    pub dialect_id: DialectId,
    pub config_schema: String,
    pub credential_schema: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationClassification {
    Read,
    ExecuteRead,
    Write,
    Destructive,
    Administrative,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContributionContext {
    Instance,
    Tenant,
    Room,
    Profile,
    Connection,
    Document,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ActionContribution {
    pub id: SegmentId,
    pub action: SegmentId,
    pub input_schema: String,
    pub output_schema: String,
    pub classification: OperationClassification,
    #[serde(default)]
    pub required_context: Vec<ContributionContext>,
    #[serde(default)]
    pub mcp_exposable: bool,
    #[serde(default)]
    pub schedulable: bool,
    #[serde(default)]
    pub interactive: bool,
    pub timeout_ms: u32,
    pub max_result_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GenericContribution {
    pub id: SegmentId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_schema: Option<String>,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Contributions {
    pub database_provider: Vec<DatabaseProviderContribution>,
    pub tunnel_provider: Vec<GenericContribution>,
    pub credential_broker: Vec<GenericContribution>,
    pub connection_hook: Vec<GenericContribution>,
    pub import_format: Vec<GenericContribution>,
    pub export_format: Vec<GenericContribution>,
    pub dialect_pack: Vec<GenericContribution>,
    pub command: Vec<ActionContribution>,
    pub governed_tool: Vec<ActionContribution>,
    pub agent_context: Vec<GenericContribution>,
    pub client_panel: Vec<GenericContribution>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExtensionManifest {
    pub schema_version: u32,
    pub id: ExtensionId,
    pub name: String,
    pub version: String,
    pub authors: Vec<String>,
    pub description: String,
    pub license: String,
    pub repository: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub support: Option<String>,
    pub minimum_sift_version: String,
    pub compatibility: Compatibility,
    #[serde(default)]
    pub lifecycle: Lifecycle,
    #[serde(default)]
    pub capabilities: Vec<CapabilityRequest>,
    #[serde(default)]
    pub artifacts: Vec<TargetArtifact>,
    #[serde(default)]
    pub data: Vec<DataFile>,
    #[serde(default)]
    pub contributions: Contributions,
    #[serde(default)]
    pub storage_schema_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PackageLock {
    pub manifest_sha256: String,
    pub files: Vec<LockedFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LockedFile {
    pub path: String,
    pub sha256: String,
    pub byte_length: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"
schema_version = 1
id = "acme/example"
name = "Example"
version = "1.2.3"
authors = ["Acme"]
description = "Example provider"
license = "Apache-2.0"
repository = "https://example.invalid/acme/example"
minimum_sift_version = "0.2.0"

[compatibility]
public_protocol = { minimum = 1, maximum = 1 }
extension_rpc = { minimum = 1, maximum = 1 }
driver_rpc = { minimum = 1, maximum = 1 }

[[capabilities]]
kind = "database.connect"
required = true

[[artifacts]]
target = "linux-x86_64"
path = "bin/example"
sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
byte_length = 123

[[contributions.database_provider]]
id = "example-db"
provider_id = "acme/example-db"
dialect_id = "acme/example-sql"
config_schema = "schemas/config.json"
credential_schema = "schemas/credentials.json"
capabilities = ["driver.core@1"]
"#;

    #[test]
    fn strict_manifest_parses() {
        let manifest: ExtensionManifest = toml::from_str(MANIFEST).unwrap();
        assert_eq!(manifest.id.as_str(), "acme/example");
        assert_eq!(manifest.contributions.database_provider.len(), 1);
    }

    #[test]
    fn unknown_manifest_fields_fail_closed() {
        let invalid = format!("{MANIFEST}\nunknown = true\n");
        assert!(toml::from_str::<ExtensionManifest>(&invalid).is_err());
    }
}
