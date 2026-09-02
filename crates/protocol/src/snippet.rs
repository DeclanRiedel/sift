use serde::{Deserialize, Serialize};

use crate::{DialectId, ObjectPath};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SnippetId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SnippetScope {
    BuiltIn,
    Personal,
    Workspace,
    Tenant,
    Catalog,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SqlSnippet {
    pub id: Option<SnippetId>,
    pub tenant_id: Option<i64>,
    pub workspace_id: Option<i64>,
    pub owner_principal_id: Option<i64>,
    pub trigger: String,
    pub title: String,
    pub description: String,
    pub body: String,
    pub dialects: Vec<DialectId>,
    pub scope: SnippetScope,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CreateSqlSnippetRequest {
    pub tenant_id: i64,
    #[serde(default)]
    pub workspace_id: Option<i64>,
    pub scope: SnippetScope,
    pub trigger: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub body: String,
    pub dialects: Vec<DialectId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UpdateSqlSnippetRequest {
    pub tenant_id: i64,
    #[serde(default)]
    pub workspace_id: Option<i64>,
    pub expected_revision: u64,
    pub trigger: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub body: String,
    pub dialects: Vec<DialectId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PrepareCatalogSnippetRequest {
    pub catalog_revision: u64,
    pub object: ObjectPath,
    pub kind: CatalogSnippetKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CatalogSnippetKind {
    Select,
    Insert,
    Update,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PreparedCatalogSnippet {
    pub snippet: SqlSnippet,
    pub catalog_revision: u64,
}
