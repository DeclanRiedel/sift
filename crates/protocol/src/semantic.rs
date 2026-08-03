//! Pure wire contracts for the shared SQL semantic service (ADR-032).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::DialectId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct SemanticDocumentId(pub Uuid);

impl std::fmt::Display for SemanticDocumentId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TextRange {
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SemanticSource {
    Scratch,
    RoomDocument {
        room_id: i64,
        document_id: i64,
        version_fingerprint: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateSemanticDocumentRequest {
    pub text: String,
    #[serde(default)]
    pub source: Option<SemanticSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateSemanticDocumentRequest {
    pub base_revision: u64,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SemanticParseStatus {
    Valid,
    Recovered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SemanticRelatedRange {
    pub message: String,
    pub range: TextRange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SemanticDiagnostic {
    pub id: String,
    pub severity: DiagnosticSeverity,
    pub code: String,
    pub message: String,
    pub range: TextRange,
    #[serde(default)]
    pub related_ranges: Vec<SemanticRelatedRange>,
    pub source: String,
    #[serde(default)]
    pub quick_fix_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SemanticDocumentState {
    pub document_id: SemanticDocumentId,
    pub revision: u64,
    pub source_digest: String,
    pub dialect_id: DialectId,
    pub pack_version: String,
    pub parse_status: SemanticParseStatus,
    pub syntax_diagnostics: Vec<SemanticDiagnostic>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SemanticRevisionRequest {
    pub revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SelectStatementRequest {
    pub revision: u64,
    pub cursor: u32,
    #[serde(default)]
    pub selection: Option<TextRange>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StatementKind {
    Query,
    Insert,
    Update,
    Delete,
    Merge,
    Ddl,
    Transaction,
    Procedure,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SemanticStatement {
    pub statement_id: String,
    pub ordinal: u32,
    pub full_range: TextRange,
    pub executable_range: TextRange,
    pub kind: StatementKind,
    pub recovered: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct StatementSelection {
    pub document_id: SemanticDocumentId,
    pub revision: u64,
    pub selection: Option<TextRange>,
    pub statements: Vec<SemanticStatement>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DiagnosticsResponse {
    pub document_id: SemanticDocumentId,
    pub revision: u64,
    pub diagnostics: Vec<SemanticDiagnostic>,
    pub incomplete: bool,
}
