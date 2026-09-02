use serde::{Deserialize, Serialize};

use crate::Value;

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SqlVariableKind {
    Value,
    Identifier,
    List,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SqlVariableScope {
    TenantDefault,
    ConnectionProfile,
    Workspace,
    QueryTab,
    RunConfiguration,
    RunPrompt,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum SqlVariableValue {
    Scalar(Value),
    Identifier(String),
    List(Vec<Value>),
    /// Opaque reference only. Secret bytes are resolved by the server-side
    /// secret store and never represented by this protocol type.
    SecretHandle(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SqlVariableBinding {
    pub name: String,
    pub value: SqlVariableValue,
    pub scope: SqlVariableScope,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SqlVariableReference {
    pub name: String,
    pub kind: SqlVariableKind,
    pub template_start: u32,
    pub template_end: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SqlVariableSourceMapEntry {
    pub name: String,
    pub template_start: u32,
    pub template_end: u32,
    pub compiled_start: u32,
    pub compiled_end: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RedactedSqlVariableDescriptor {
    pub name: String,
    pub kind: SqlVariableKind,
    pub scope: SqlVariableScope,
    pub secret: bool,
    pub bind_count: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CompiledSqlVariables {
    pub sql: String,
    pub params: Vec<Value>,
    pub source_map: Vec<SqlVariableSourceMapEntry>,
    pub descriptors: Vec<RedactedSqlVariableDescriptor>,
}

/// Safe execution provenance for history and diagnostics. It contains the
/// template and descriptors only; resolved values are structurally absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SqlVariableHistoryContext {
    pub template_sql: String,
    pub descriptors: Vec<RedactedSqlVariableDescriptor>,
}
