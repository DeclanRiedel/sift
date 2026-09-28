use serde::{Deserialize, Serialize};

/// Search and page over settings visible to the connected PostgreSQL role.
#[derive(Debug, Clone, Default, Deserialize, Serialize, schemars::JsonSchema)]
pub struct PostgresSettingsQuery {
    #[serde(default)]
    pub filter: String,
    #[serde(default)]
    pub offset: u32,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct PostgresSetting {
    pub name: String,
    pub value: Option<String>,
    pub unit: Option<String>,
    pub category: String,
    pub description: Option<String>,
    pub context: String,
    pub source: String,
    pub pending_restart: bool,
    /// The server suppresses values for settings likely to contain credentials.
    pub redacted: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct PostgresSettingsPage {
    pub settings: Vec<PostgresSetting>,
    pub next_offset: Option<u32>,
}
