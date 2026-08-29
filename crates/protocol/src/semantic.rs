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
    #[serde(default)]
    pub catalog_revision: Option<crate::CatalogRevision>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SemanticCompletionRequest {
    pub revision: u64,
    pub cursor: u32,
    #[serde(default)]
    pub limit: Option<u32>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SemanticOutlineSymbolKind {
    Cte,
    Object,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SemanticOutlineSymbol {
    pub symbol_id: String,
    pub statement_id: String,
    pub kind: SemanticOutlineSymbolKind,
    pub name: String,
    pub range: TextRange,
    #[serde(default)]
    pub definition_range: Option<TextRange>,
    #[serde(default)]
    pub alias: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
    pub usage_kind: SqlUsageKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct StatementSelection {
    pub document_id: SemanticDocumentId,
    pub revision: u64,
    pub selection: Option<TextRange>,
    pub statements: Vec<SemanticStatement>,
    #[serde(default)]
    pub symbols: Vec<SemanticOutlineSymbol>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DiagnosticsResponse {
    pub document_id: SemanticDocumentId,
    pub revision: u64,
    pub diagnostics: Vec<SemanticDiagnostic>,
    pub incomplete: bool,
    #[serde(default)]
    pub catalog_revision: Option<crate::CatalogRevision>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum KeywordCase {
    Preserve,
    Upper,
    Lower,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FormatOptions {
    #[serde(default = "default_keyword_case")]
    pub keyword_case: KeywordCase,
}

fn default_keyword_case() -> KeywordCase {
    KeywordCase::Upper
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            keyword_case: default_keyword_case(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FormatSqlRequest {
    pub revision: u64,
    #[serde(default)]
    pub range: Option<TextRange>,
    #[serde(default)]
    pub options: FormatOptions,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TextEdit {
    pub range: TextRange,
    pub new_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DocumentEdit {
    pub document_id: SemanticDocumentId,
    pub expected_revision: u64,
    pub source_digest: String,
    pub edits: Vec<TextEdit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceEdit {
    pub documents: Vec<DocumentEdit>,
    #[serde(default)]
    pub warnings: Vec<String>,
    pub is_complete: bool,
    #[serde(default)]
    pub actual_range: Option<TextRange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SqlQuickFixRequest {
    pub revision: u64,
    pub catalog_revision: crate::CatalogRevision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SqlSymbolTarget {
    AtPosition { position: u32 },
    CatalogObject { object_id: crate::CatalogObjectId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FindSqlUsagesRequest {
    pub revision: u64,
    #[serde(default)]
    pub catalog_revision: Option<crate::CatalogRevision>,
    pub target: SqlSymbolTarget,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SqlUsageKind {
    Definition,
    Read,
    Write,
    Call,
    TypeReference,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SqlUsage {
    pub range: TextRange,
    pub kind: SqlUsageKind,
    #[serde(default)]
    pub catalog_object_id: Option<crate::CatalogObjectId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SqlUsagePage {
    pub document_id: SemanticDocumentId,
    pub revision: u64,
    #[serde(default)]
    pub catalog_revision: Option<crate::CatalogRevision>,
    pub usages: Vec<SqlUsage>,
    #[serde(default)]
    pub next_cursor: Option<String>,
    pub is_complete: bool,
    pub search_scope: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SqlRefactor {
    RenameSymbol { position: u32, new_name: String },
    QualifyName { position: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PrepareSqlRefactorRequest {
    pub revision: u64,
    #[serde(default)]
    pub catalog_revision: Option<crate::CatalogRevision>,
    pub refactor: SqlRefactor,
}
