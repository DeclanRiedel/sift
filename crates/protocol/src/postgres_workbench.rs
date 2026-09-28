use serde::{Deserialize, Serialize};

/// Role-visible PostgreSQL catalog entries. Both lists are bounded by the
/// server; absent extension versions mean the extension is not installed.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PostgresExtension {
    pub name: String,
    pub installed_version: Option<String>,
    pub default_version: Option<String>,
    pub schema: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PostgresPartition {
    pub parent_schema: String,
    pub parent: String,
    pub child_schema: String,
    pub child: String,
    pub bound: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PostgresObjectPageQuery {
    #[serde(default)]
    pub offset: u32,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PostgresObjectPage<T> {
    pub items: Vec<T>,
    pub next_offset: Option<u32>,
}

/// PostgreSQL database actions supported by the initial workbench. Extension
/// names and qualified relation names are identifiers, never SQL fragments.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PostgresObjectAction {
    InstallExtension {
        name: String,
    },
    DropExtension {
        name: String,
    },
    DetachPartition {
        parent_schema: String,
        parent: String,
        child_schema: String,
        child: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PostgresObjectPreview {
    pub action: PostgresObjectAction,
    pub sql: String,
    pub precondition: String,
    pub warning: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ApplyPostgresObjectRequest {
    pub action: PostgresObjectAction,
    pub precondition: String,
    pub confirmed: bool,
}
