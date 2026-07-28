use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use sift_extension_protocol::{DialectId, ProviderId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderRef {
    pub provider_id: ProviderId,
    pub dialect_id: DialectId,
    pub provider_version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderCapability {
    pub id: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub limits: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderQuality {
    Compatible,
    QueryCapable,
    Transactional,
    IdeCapable,
    SiftCertified,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderDescriptor {
    pub provider: ProviderRef,
    pub display_name: String,
    pub configuration_schema: serde_json::Value,
    pub credential_schema: serde_json::Value,
    pub configuration_schema_version: u32,
    pub capabilities: Vec<ProviderCapability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<ProviderQuality>,
    pub available: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderConnectionSpec {
    pub provider_id: ProviderId,
    pub configuration: serde_json::Value,
    #[serde(default)]
    pub credential_handles: BTreeMap<String, String>,
}
