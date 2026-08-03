//! Bounded, process-local parsed SQL document state (ADR-032 K0).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};
use sift_protocol::{
    DiagnosticSeverity, DiagnosticsResponse, SelectStatementRequest, SemanticDiagnostic,
    SemanticDocumentId, SemanticDocumentState, SemanticParseStatus, SemanticSource,
    SemanticStatement, StatementKind, StatementSelection, TextRange,
};
use sqlparser::dialect::{Dialect, MsSqlDialect, PostgreSqlDialect};
use sqlparser::parser::Parser;
use sqlparser::tokenizer::Tokenizer;
use uuid::Uuid;

pub const MAX_SOURCE_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_DOCUMENTS_PER_SCOPE: usize = 64;
pub const MAX_RETAINED_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_DIAGNOSTICS: usize = 500;
pub const MAX_TOKENS: usize = 250_000;
pub const PACK_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DocumentScope {
    pub session: u64,
    pub connection: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("semantic document not found")]
    NotFound,
    #[error("semantic revision conflict; current revision is {current}")]
    RevisionConflict { current: u64 },
    #[error("invalid UTF-8 byte range")]
    InvalidRange,
    #[error("dialect `{0}` has no bundled semantic pack")]
    DialectUnavailable(String),
    #[error("semantic source or registry limit exceeded")]
    LimitExceeded,
    #[error("semantic work canceled")]
    Canceled,
}

#[derive(Clone, Default)]
pub struct SemanticRegistry {
    inner: Arc<Mutex<RegistryState>>,
}

#[derive(Default)]
struct RegistryState {
    documents: HashMap<SemanticDocumentId, Document>,
    retained_bytes: usize,
}

struct Document {
    scope: DocumentScope,
    revision: u64,
    dialect_id: sift_protocol::DialectId,
    source: Arc<str>,
    source_digest: String,
    _source_link: Option<SemanticSource>,
    statements: Vec<SemanticStatement>,
    diagnostics: Vec<SemanticDiagnostic>,
}

struct Parsed {
    source: Arc<str>,
    source_digest: String,
    statements: Vec<SemanticStatement>,
    diagnostics: Vec<SemanticDiagnostic>,
}

impl SemanticRegistry {
    pub fn create(
        &self,
        scope: DocumentScope,
        dialect_id: sift_protocol::DialectId,
        text: String,
        source_link: Option<SemanticSource>,
        canceled: &AtomicBool,
    ) -> Result<SemanticDocumentState, Error> {
        self.check_create_limits(scope, text.len())?;
        let id = SemanticDocumentId(Uuid::new_v4());
        let parsed = parse(&dialect_id, 1, &text, canceled)?;
        let state = document_state(id, 1, &dialect_id, &parsed);
        let mut registry = self.inner.lock().unwrap();
        if registry
            .documents
            .values()
            .filter(|document| document.scope == scope)
            .count()
            >= MAX_DOCUMENTS_PER_SCOPE
            || registry.retained_bytes.saturating_add(text.len()) > MAX_RETAINED_BYTES
        {
            return Err(Error::LimitExceeded);
        }
        registry.retained_bytes += text.len();
        registry.documents.insert(
            id,
            Document {
                scope,
                revision: 1,
                dialect_id,
                source: parsed.source,
                source_digest: parsed.source_digest,
                _source_link: source_link,
                statements: parsed.statements,
                diagnostics: parsed.diagnostics,
            },
        );
        Ok(state)
    }

    pub fn update(
        &self,
        scope: DocumentScope,
        id: SemanticDocumentId,
        base_revision: u64,
        text: String,
        canceled: &AtomicBool,
    ) -> Result<SemanticDocumentState, Error> {
        if text.len() > MAX_SOURCE_BYTES {
            return Err(Error::LimitExceeded);
        }
        let requested_digest = source_digest(&text);
        let (dialect_id, next_revision, old_len) = {
            let registry = self.inner.lock().unwrap();
            let document = registry.documents.get(&id).ok_or(Error::NotFound)?;
            ensure_scope(document, scope)?;
            if document.revision != base_revision {
                if document.revision == base_revision.saturating_add(1)
                    && document.source_digest == requested_digest
                {
                    return Ok(state_from_document(id, document));
                }
                return Err(Error::RevisionConflict {
                    current: document.revision,
                });
            }
            (
                document.dialect_id.clone(),
                document
                    .revision
                    .checked_add(1)
                    .ok_or(Error::LimitExceeded)?,
                document.source.len(),
            )
        };
        let parsed = parse(&dialect_id, next_revision, &text, canceled)?;
        let state = document_state(id, next_revision, &dialect_id, &parsed);
        let mut registry = self.inner.lock().unwrap();
        let projected = registry.retained_bytes - old_len + text.len();
        if projected > MAX_RETAINED_BYTES {
            return Err(Error::LimitExceeded);
        }
        let document = registry.documents.get_mut(&id).ok_or(Error::NotFound)?;
        ensure_scope(document, scope)?;
        if document.revision != base_revision {
            if document.revision == base_revision.saturating_add(1)
                && document.source_digest == requested_digest
            {
                return Ok(state_from_document(id, document));
            }
            return Err(Error::RevisionConflict {
                current: document.revision,
            });
        }
        document.revision = next_revision;
        document.source = parsed.source;
        document.source_digest = parsed.source_digest;
        document.statements = parsed.statements;
        document.diagnostics = parsed.diagnostics;
        registry.retained_bytes = projected;
        Ok(state)
    }

    pub fn close(&self, scope: DocumentScope, id: SemanticDocumentId) -> Result<(), Error> {
        let mut registry = self.inner.lock().unwrap();
        let document = registry.documents.get(&id).ok_or(Error::NotFound)?;
        ensure_scope(document, scope)?;
        let document = registry.documents.remove(&id).expect("checked above");
        registry.retained_bytes -= document.source.len();
        Ok(())
    }

    pub fn close_scope(&self, scope: DocumentScope) {
        let mut registry = self.inner.lock().unwrap();
        let ids: Vec<_> = registry
            .documents
            .iter()
            .filter_map(|(id, document)| (document.scope == scope).then_some(*id))
            .collect();
        for id in ids {
            if let Some(document) = registry.documents.remove(&id) {
                registry.retained_bytes -= document.source.len();
            }
        }
    }

    pub fn close_session(&self, session: u64) {
        let mut registry = self.inner.lock().unwrap();
        let ids: Vec<_> = registry
            .documents
            .iter()
            .filter_map(|(id, document)| (document.scope.session == session).then_some(*id))
            .collect();
        for id in ids {
            if let Some(document) = registry.documents.remove(&id) {
                registry.retained_bytes -= document.source.len();
            }
        }
    }

    pub fn diagnostics(
        &self,
        scope: DocumentScope,
        id: SemanticDocumentId,
        revision: u64,
    ) -> Result<DiagnosticsResponse, Error> {
        let registry = self.inner.lock().unwrap();
        let document = registry.documents.get(&id).ok_or(Error::NotFound)?;
        ensure_scope(document, scope)?;
        ensure_revision(document, revision)?;
        Ok(DiagnosticsResponse {
            document_id: id,
            revision,
            diagnostics: document.diagnostics.clone(),
            incomplete: false,
        })
    }

    pub fn select_statement(
        &self,
        scope: DocumentScope,
        id: SemanticDocumentId,
        request: SelectStatementRequest,
    ) -> Result<StatementSelection, Error> {
        let registry = self.inner.lock().unwrap();
        let document = registry.documents.get(&id).ok_or(Error::NotFound)?;
        ensure_scope(document, scope)?;
        ensure_revision(document, request.revision)?;
        validate_offset(&document.source, request.cursor)?;
        if let Some(range) = request.selection {
            validate_range(&document.source, range)?;
            if range.start == range.end {
                return Err(Error::InvalidRange);
            }
            let statements = document
                .statements
                .iter()
                .filter(|statement| ranges_intersect(statement.full_range, range))
                .cloned()
                .collect();
            return Ok(StatementSelection {
                document_id: id,
                revision: request.revision,
                selection: Some(range),
                statements,
            });
        }
        let cursor = request.cursor;
        let chosen = document
            .statements
            .iter()
            .find(|statement| {
                statement.executable_range.start <= cursor
                    && cursor < statement.executable_range.end
            })
            .or_else(|| {
                document
                    .statements
                    .iter()
                    .find(|statement| statement.executable_range.start >= cursor)
            })
            .or_else(|| document.statements.last())
            .cloned();
        Ok(StatementSelection {
            document_id: id,
            revision: request.revision,
            selection: chosen.as_ref().map(|statement| statement.executable_range),
            statements: chosen.into_iter().collect(),
        })
    }

    fn check_create_limits(&self, scope: DocumentScope, bytes: usize) -> Result<(), Error> {
        if bytes > MAX_SOURCE_BYTES {
            return Err(Error::LimitExceeded);
        }
        let registry = self.inner.lock().unwrap();
        if registry
            .documents
            .values()
            .filter(|document| document.scope == scope)
            .count()
            >= MAX_DOCUMENTS_PER_SCOPE
            || registry.retained_bytes.saturating_add(bytes) > MAX_RETAINED_BYTES
        {
            return Err(Error::LimitExceeded);
        }
        Ok(())
    }
}

fn ensure_scope(document: &Document, scope: DocumentScope) -> Result<(), Error> {
    (document.scope == scope)
        .then_some(())
        .ok_or(Error::NotFound)
}

fn ensure_revision(document: &Document, revision: u64) -> Result<(), Error> {
    (document.revision == revision)
        .then_some(())
        .ok_or(Error::RevisionConflict {
            current: document.revision,
        })
}

fn document_state(
    id: SemanticDocumentId,
    revision: u64,
    dialect_id: &sift_protocol::DialectId,
    parsed: &Parsed,
) -> SemanticDocumentState {
    SemanticDocumentState {
        document_id: id,
        revision,
        source_digest: parsed.source_digest.clone(),
        dialect_id: dialect_id.clone(),
        pack_version: PACK_VERSION.to_string(),
        parse_status: if parsed.diagnostics.is_empty() {
            SemanticParseStatus::Valid
        } else {
            SemanticParseStatus::Recovered
        },
        syntax_diagnostics: parsed.diagnostics.clone(),
    }
}

fn state_from_document(id: SemanticDocumentId, document: &Document) -> SemanticDocumentState {
    SemanticDocumentState {
        document_id: id,
        revision: document.revision,
        source_digest: document.source_digest.clone(),
        dialect_id: document.dialect_id.clone(),
        pack_version: PACK_VERSION.to_string(),
        parse_status: if document.diagnostics.is_empty() {
            SemanticParseStatus::Valid
        } else {
            SemanticParseStatus::Recovered
        },
        syntax_diagnostics: document.diagnostics.clone(),
    }
}

fn parse(
    dialect_id: &sift_protocol::DialectId,
    revision: u64,
    text: &str,
    canceled: &AtomicBool,
) -> Result<Parsed, Error> {
    if text.len() > MAX_SOURCE_BYTES {
        return Err(Error::LimitExceeded);
    }
    let flavor = match dialect_id.as_str() {
        "sift/postgresql" => Flavor::Postgres,
        "sift/tsql" => Flavor::Tsql,
        other => return Err(Error::DialectUnavailable(other.to_string())),
    };
    let source: Arc<str> = Arc::from(text);
    let mut diagnostics = Vec::new();
    let spans = split_statements(&source, flavor, revision, canceled, &mut diagnostics)?;
    let dialect: Box<dyn Dialect> = match flavor {
        Flavor::Postgres => Box::new(PostgreSqlDialect {}),
        Flavor::Tsql => Box::new(MsSqlDialect {}),
    };
    let mut statements = Vec::new();
    let mut token_count = 0usize;
    for (full, executable) in spans {
        if canceled.load(Ordering::Relaxed) {
            return Err(Error::Canceled);
        }
        let sql = &source[executable.start as usize..executable.end as usize];
        if flavor == Flavor::Tsql && sql.eq_ignore_ascii_case("go") {
            continue;
        }
        if let Ok(tokens) = Tokenizer::new(&*dialect, sql).tokenize() {
            token_count = token_count.saturating_add(tokens.len());
            if token_count > MAX_TOKENS {
                return Err(Error::LimitExceeded);
            }
        }
        let ordinal = statements.len();
        let before = diagnostics.len();
        match Parser::parse_sql(&*dialect, sql) {
            Ok(parsed) if parsed.is_empty() => continue,
            Ok(_) => {}
            Err(error) if diagnostics.len() < MAX_DIAGNOSTICS => {
                diagnostics.push(SemanticDiagnostic {
                    id: format!("{revision}:parser:{ordinal}"),
                    severity: DiagnosticSeverity::Error,
                    code: "syntax_error".into(),
                    message: error.to_string(),
                    range: executable,
                    related_ranges: Vec::new(),
                    source: "parser".into(),
                    quick_fix_ids: Vec::new(),
                });
            }
            Err(_) => {}
        }
        let recovered = diagnostics[before..]
            .iter()
            .any(|diagnostic| ranges_intersect(diagnostic.range, executable))
            || diagnostics[..before]
                .iter()
                .any(|diagnostic| ranges_intersect(diagnostic.range, executable));
        statements.push(SemanticStatement {
            statement_id: format!("{revision}:{ordinal}"),
            ordinal: ordinal as u32,
            full_range: full,
            executable_range: executable,
            kind: statement_kind(sql),
            recovered,
        });
    }
    let source_digest = source_digest(&source);
    Ok(Parsed {
        source,
        source_digest,
        statements,
        diagnostics,
    })
}

fn source_digest(source: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(source.as_bytes()))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Flavor {
    Postgres,
    Tsql,
}

fn split_statements(
    source: &str,
    flavor: Flavor,
    revision: u64,
    canceled: &AtomicBool,
    diagnostics: &mut Vec<SemanticDiagnostic>,
) -> Result<Vec<(TextRange, TextRange)>, Error> {
    let bytes = source.as_bytes();
    let mut boundaries = vec![0usize];
    let mut index = 0usize;
    let mut quote = None;
    let mut bracket = false;
    let mut line_comment = false;
    let mut block_depth = 0u32;
    let mut dollar_tag: Option<Vec<u8>> = None;
    let mut paren_depth = 0i32;
    while index < bytes.len() {
        if index % 4096 == 0 && canceled.load(Ordering::Relaxed) {
            return Err(Error::Canceled);
        }
        if flavor == Flavor::Tsql
            && paren_depth == 0
            && is_line_start(bytes, index)
            && tsql_go_line_end(source, index).is_some()
        {
            let line_end = tsql_go_line_end(source, index).expect("checked above");
            if *boundaries.last().unwrap() != index {
                boundaries.push(index);
            }
            boundaries.push(line_end);
            index = line_end;
        } else if line_comment {
            if bytes[index] == b'\n' {
                line_comment = false;
            }
            index += 1;
            continue;
        }
        if block_depth > 0 {
            if bytes[index..].starts_with(b"/*") {
                block_depth += 1;
                index += 2;
            } else if bytes[index..].starts_with(b"*/") {
                block_depth -= 1;
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        if let Some(tag) = &dollar_tag {
            if bytes[index..].starts_with(tag) {
                index += tag.len();
                dollar_tag = None;
            } else {
                index += 1;
            }
            continue;
        }
        if bracket {
            if bytes[index] == b']' {
                if bytes.get(index + 1) == Some(&b']') {
                    index += 2;
                } else {
                    bracket = false;
                    index += 1;
                }
            } else {
                index += 1;
            }
            continue;
        }
        if let Some(active) = quote {
            if bytes[index] == active {
                if bytes.get(index + 1) == Some(&active) {
                    index += 2;
                } else {
                    quote = None;
                    index += 1;
                }
            } else {
                index += 1;
            }
            continue;
        }
        if bytes[index..].starts_with(b"--") {
            line_comment = true;
            index += 2;
        } else if bytes[index..].starts_with(b"/*") {
            block_depth = 1;
            index += 2;
        } else if matches!(bytes[index], b'\'' | b'"') {
            quote = Some(bytes[index]);
            index += 1;
        } else if flavor == Flavor::Tsql && bytes[index] == b'[' {
            bracket = true;
            index += 1;
        } else if flavor == Flavor::Postgres && bytes[index] == b'$' {
            if let Some(tag) = dollar_quote_tag(&bytes[index..]) {
                index += tag.len();
                dollar_tag = Some(tag);
            } else {
                index += 1;
            }
        } else if bytes[index] == b'(' {
            paren_depth += 1;
            index += 1;
        } else if bytes[index] == b')' {
            paren_depth -= 1;
            index += 1;
        } else if bytes[index] == b';' && paren_depth <= 0 {
            boundaries.push(index + 1);
            index += 1;
        } else {
            index += 1;
        }
    }
    if *boundaries.last().unwrap() != bytes.len() {
        boundaries.push(bytes.len());
    }
    if quote.is_some() || bracket || block_depth > 0 || dollar_tag.is_some() || paren_depth != 0 {
        diagnostics.push(SemanticDiagnostic {
            id: format!("{revision}:scanner:unclosed"),
            severity: DiagnosticSeverity::Error,
            code: "unclosed_construct".into(),
            message: "unclosed SQL construct".into(),
            range: TextRange {
                start: boundaries[boundaries.len().saturating_sub(2)] as u32,
                end: bytes.len() as u32,
            },
            related_ranges: Vec::new(),
            source: "parser".into(),
            quick_fix_ids: Vec::new(),
        });
    }
    let mut result = Vec::new();
    for pair in boundaries.windows(2) {
        let start = pair[0];
        let end = pair[1];
        let executable = trim_statement(source, start, end);
        if executable.start < executable.end {
            result.push((
                TextRange {
                    start: start as u32,
                    end: end as u32,
                },
                executable,
            ));
        }
    }
    Ok(result)
}

fn is_line_start(bytes: &[u8], index: usize) -> bool {
    index == 0 || bytes.get(index.wrapping_sub(1)) == Some(&b'\n')
}

fn tsql_go_line_end(source: &str, index: usize) -> Option<usize> {
    let relative_end = source[index..]
        .find('\n')
        .map(|value| value + 1)
        .unwrap_or(source.len() - index);
    let end = index + relative_end;
    let line = source[index..end].trim();
    line.eq_ignore_ascii_case("go").then_some(end)
}

fn dollar_quote_tag(input: &[u8]) -> Option<Vec<u8>> {
    let end = input.get(1..)?.iter().position(|byte| *byte == b'$')? + 1;
    input[1..end]
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        .then(|| input[..=end].to_vec())
}

fn trim_statement(source: &str, start: usize, mut end: usize) -> TextRange {
    if end > start && source.as_bytes()[end - 1] == b';' {
        end -= 1;
    }
    let slice = &source[start..end];
    let leading = slice.len() - slice.trim_start().len();
    let trimmed_start = start + leading;
    let trimmed_end = trimmed_start + slice.trim().len();
    TextRange {
        start: trimmed_start as u32,
        end: trimmed_end as u32,
    }
}

fn statement_kind(sql: &str) -> StatementKind {
    match sql
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_uppercase()
        .as_str()
    {
        "SELECT" | "WITH" | "VALUES" => StatementKind::Query,
        "INSERT" => StatementKind::Insert,
        "UPDATE" => StatementKind::Update,
        "DELETE" => StatementKind::Delete,
        "MERGE" => StatementKind::Merge,
        "CREATE" | "ALTER" | "DROP" | "TRUNCATE" | "COMMENT" => StatementKind::Ddl,
        "BEGIN" | "COMMIT" | "ROLLBACK" | "SAVEPOINT" => StatementKind::Transaction,
        "EXEC" | "EXECUTE" | "CALL" | "DO" => StatementKind::Procedure,
        _ => StatementKind::Unknown,
    }
}

fn validate_offset(source: &str, offset: u32) -> Result<(), Error> {
    let offset = offset as usize;
    (offset <= source.len() && source.is_char_boundary(offset))
        .then_some(())
        .ok_or(Error::InvalidRange)
}

fn validate_range(source: &str, range: TextRange) -> Result<(), Error> {
    if range.start > range.end {
        return Err(Error::InvalidRange);
    }
    validate_offset(source, range.start)?;
    validate_offset(source, range.end)
}

fn ranges_intersect(left: TextRange, right: TextRange) -> bool {
    left.start < right.end && right.start < left.end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dialect(value: &str) -> sift_protocol::DialectId {
        sift_protocol::DialectId::new(value).unwrap()
    }

    #[test]
    fn malformed_middle_statement_does_not_hide_later_statement() {
        let registry = SemanticRegistry::default();
        let scope = DocumentScope {
            session: 1,
            connection: 1,
        };
        let state = registry
            .create(
                scope,
                dialect("sift/postgresql"),
                "select 1; select from; select 3".into(),
                None,
                &AtomicBool::new(false),
            )
            .unwrap();
        assert_eq!(state.parse_status, SemanticParseStatus::Recovered);
        let selected = registry
            .select_statement(
                scope,
                state.document_id,
                SelectStatementRequest {
                    revision: 1,
                    cursor: 25,
                    selection: None,
                },
            )
            .unwrap();
        assert_eq!(selected.statements[0].ordinal, 2);
    }

    #[test]
    fn semicolons_in_postgres_dollar_quotes_do_not_split() {
        let mut diagnostics = Vec::new();
        let spans = split_statements(
            "do $$ begin; end $$; select 1;",
            Flavor::Postgres,
            1,
            &AtomicBool::new(false),
            &mut diagnostics,
        )
        .unwrap();
        assert_eq!(spans.len(), 2);
    }

    #[test]
    fn tsql_go_lines_split_batches_but_are_not_statements() {
        let registry = SemanticRegistry::default();
        let scope = DocumentScope {
            session: 1,
            connection: 1,
        };
        let source = "select [semi;colon]\r\nGO\r\nselect 2";
        let state = registry
            .create(
                scope,
                dialect("sift/tsql"),
                source.into(),
                None,
                &AtomicBool::new(false),
            )
            .unwrap();
        let selected = registry
            .select_statement(
                scope,
                state.document_id,
                SelectStatementRequest {
                    revision: 1,
                    cursor: source.len() as u32,
                    selection: None,
                },
            )
            .unwrap();
        assert_eq!(selected.statements[0].ordinal, 1);
        assert_eq!(selected.statements[0].kind, StatementKind::Query);
    }

    #[test]
    fn cursor_on_delimiter_prefers_following_statement() {
        let registry = SemanticRegistry::default();
        let scope = DocumentScope {
            session: 1,
            connection: 1,
        };
        let state = registry
            .create(
                scope,
                dialect("sift/postgresql"),
                "select 1; select 2".into(),
                None,
                &AtomicBool::new(false),
            )
            .unwrap();
        let selected = registry
            .select_statement(
                scope,
                state.document_id,
                SelectStatementRequest {
                    revision: 1,
                    cursor: 8,
                    selection: None,
                },
            )
            .unwrap();
        assert_eq!(selected.statements[0].ordinal, 1);
    }

    #[test]
    fn pre_canceled_parse_does_not_publish_document() {
        let registry = SemanticRegistry::default();
        let scope = DocumentScope {
            session: 1,
            connection: 1,
        };
        assert!(matches!(
            registry.create(
                scope,
                dialect("sift/postgresql"),
                "select 1".into(),
                None,
                &AtomicBool::new(true),
            ),
            Err(Error::Canceled)
        ));
    }

    #[test]
    fn stale_update_is_rejected_without_changing_revision() {
        let registry = SemanticRegistry::default();
        let scope = DocumentScope {
            session: 1,
            connection: 1,
        };
        let state = registry
            .create(
                scope,
                dialect("sift/tsql"),
                "select 1".into(),
                None,
                &AtomicBool::new(false),
            )
            .unwrap();
        registry
            .update(
                scope,
                state.document_id,
                1,
                "select 2".into(),
                &AtomicBool::new(false),
            )
            .unwrap();
        assert!(matches!(
            registry.update(
                scope,
                state.document_id,
                1,
                "select 3".into(),
                &AtomicBool::new(false)
            ),
            Err(Error::RevisionConflict { current: 2 })
        ));
    }

    #[test]
    fn accepted_update_retry_is_idempotent() {
        let registry = SemanticRegistry::default();
        let scope = DocumentScope {
            session: 1,
            connection: 1,
        };
        let state = registry
            .create(
                scope,
                dialect("sift/postgresql"),
                "select 1".into(),
                None,
                &AtomicBool::new(false),
            )
            .unwrap();
        let first = registry
            .update(
                scope,
                state.document_id,
                1,
                "select 2".into(),
                &AtomicBool::new(false),
            )
            .unwrap();
        let retry = registry
            .update(
                scope,
                state.document_id,
                1,
                "select 2".into(),
                &AtomicBool::new(false),
            )
            .unwrap();
        assert_eq!(first.revision, 2);
        assert_eq!(retry.revision, 2);
        assert_eq!(first.source_digest, retry.source_digest);
    }

    #[test]
    fn rejects_mid_codepoint_offsets() {
        let registry = SemanticRegistry::default();
        let scope = DocumentScope {
            session: 1,
            connection: 1,
        };
        let state = registry
            .create(
                scope,
                dialect("sift/postgresql"),
                "select '😀'".into(),
                None,
                &AtomicBool::new(false),
            )
            .unwrap();
        assert!(matches!(
            registry.select_statement(
                scope,
                state.document_id,
                SelectStatementRequest {
                    revision: 1,
                    cursor: 9,
                    selection: None
                }
            ),
            Err(Error::InvalidRange)
        ));
    }
}
