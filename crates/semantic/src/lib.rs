//! Bounded, process-local parsed SQL document state (ADR-032 K0).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};
use sift_protocol::{
    completion::CompletionContext, DiagnosticSeverity, DiagnosticsResponse, DocumentEdit,
    FindSqlUsagesRequest, FormatSqlRequest, KeywordCase, PrepareSqlRefactorRequest,
    SelectStatementRequest, SemanticDiagnostic, SemanticDocumentId, SemanticDocumentState,
    SemanticOutlineSymbol, SemanticOutlineSymbolKind, SemanticParseStatus, SemanticSource,
    SemanticStatement, SqlRefactor, SqlSymbolTarget, SqlUsage, SqlUsageKind, SqlUsagePage,
    StatementKind, StatementSelection, TextEdit, TextRange, WorkspaceEdit,
};
use sqlparser::dialect::{Dialect, MsSqlDialect, PostgreSqlDialect};
use sqlparser::parser::Parser;
use sqlparser::tokenizer::{Token, Tokenizer, Whitespace, Word};
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

/// Revision-bound completion input derived by the shared semantic service.
#[derive(Debug, Clone)]
pub struct CompletionAnalysis {
    pub context: CompletionContext,
    pub cursor: usize,
    pub prefix_start: usize,
    pub prefix: String,
    pub prefix_lower: String,
    pub relations: Vec<CompletionRelation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionRelation {
    pub name: String,
    pub target: Option<String>,
    pub is_alias: bool,
}

#[derive(Debug, Clone)]
pub struct StatementSource {
    pub source_digest: String,
    pub statement: SemanticStatement,
    pub sql: String,
}

#[derive(Debug, Clone)]
pub struct CatalogBindingView {
    pub revision: sift_protocol::CatalogRevision,
    pub complete: bool,
    pub objects: Vec<CatalogBindingObject>,
    by_name: HashMap<String, Vec<usize>>,
    by_qualified_name: HashMap<String, Vec<usize>>,
}

#[derive(Debug, Clone)]
pub struct CatalogBindingObject {
    pub id: sift_protocol::CatalogObjectId,
    pub catalog: String,
    pub schema: String,
    pub name: String,
    pub qualified_name: String,
    pub kind: sift_protocol::CatalogNodeKind,
    pub complete: bool,
    pub columns: Vec<CatalogBindingColumn>,
}

#[derive(Debug, Clone)]
pub struct CatalogBindingColumn {
    pub id: sift_protocol::CatalogObjectId,
    pub name: String,
    pub type_ref: sift_protocol::TypeRef,
    pub nullable: sift_protocol::Nullability,
    pub ordinal: Option<u32>,
}

impl CatalogBindingView {
    pub fn new(
        revision: sift_protocol::CatalogRevision,
        complete: bool,
        objects: Vec<CatalogBindingObject>,
    ) -> Self {
        let mut by_name = HashMap::<String, Vec<usize>>::new();
        let mut by_qualified_name = HashMap::<String, Vec<usize>>::new();
        for (index, object) in objects.iter().enumerate() {
            by_name
                .entry(object.name.to_ascii_lowercase())
                .or_default()
                .push(index);
            for key in [
                format!("{}.{}", object.schema, object.name),
                format!("{}.{}.{}", object.catalog, object.schema, object.name),
                object.qualified_name.clone(),
            ] {
                let matches = by_qualified_name
                    .entry(key.to_ascii_lowercase())
                    .or_default();
                if matches.last() != Some(&index) {
                    matches.push(index);
                }
            }
        }
        Self {
            revision,
            complete,
            objects,
            by_name,
            by_qualified_name,
        }
    }

    pub fn resolve(&self, reference: &str) -> Option<&CatalogBindingObject> {
        let key = reference.to_ascii_lowercase();
        if key.contains('.') {
            let matches = self.by_qualified_name.get(&key)?;
            return (matches.len() == 1).then(|| &self.objects[matches[0]]);
        }
        let matches = self.by_name.get(&key)?;
        (matches.len() == 1).then(|| &self.objects[matches[0]])
    }

    fn matching_objects(
        &self,
        schema: Option<(&str, bool)>,
        object: (&str, bool),
    ) -> Vec<&CatalogBindingObject> {
        if !object.1 && schema.is_none() {
            return self
                .by_name
                .get(&object.0.to_ascii_lowercase())
                .into_iter()
                .flatten()
                .map(|index| &self.objects[*index])
                .collect();
        }
        self.objects
            .iter()
            .filter(|candidate| {
                identifier_matches(&candidate.name, object.0, object.1)
                    && schema.map_or(true, |(schema, quoted)| {
                        identifier_matches(&candidate.schema, schema, quoted)
                    })
            })
            .collect()
    }
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
    #[error("invalid semantic feature request")]
    InvalidRequest,
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
            catalog_revision: None,
        })
    }

    pub fn diagnostics_with_catalog(
        &self,
        scope: DocumentScope,
        id: SemanticDocumentId,
        revision: u64,
        catalog: &CatalogBindingView,
    ) -> Result<DiagnosticsResponse, Error> {
        let (source, mut diagnostics) = {
            let registry = self.inner.lock().unwrap();
            let document = registry.documents.get(&id).ok_or(Error::NotFound)?;
            ensure_scope(document, scope)?;
            ensure_revision(document, revision)?;
            (Arc::clone(&document.source), document.diagnostics.clone())
        };
        let mut binder = bind_catalog_references(&source, revision, catalog);
        let incomplete = !catalog.complete;
        diagnostics.append(&mut binder);
        diagnostics.truncate(MAX_DIAGNOSTICS);
        Ok(DiagnosticsResponse {
            document_id: id,
            revision,
            diagnostics,
            incomplete,
            catalog_revision: Some(catalog.revision),
        })
    }

    pub fn format(
        &self,
        scope: DocumentScope,
        id: SemanticDocumentId,
        request: FormatSqlRequest,
        canceled: &AtomicBool,
    ) -> Result<WorkspaceEdit, Error> {
        let (source, source_digest, dialect_id, statements) = {
            let registry = self.inner.lock().unwrap();
            let document = registry.documents.get(&id).ok_or(Error::NotFound)?;
            ensure_scope(document, scope)?;
            ensure_revision(document, request.revision)?;
            (
                Arc::clone(&document.source),
                document.source_digest.clone(),
                document.dialect_id.clone(),
                document.statements.clone(),
            )
        };
        let requested = request.range.unwrap_or(TextRange {
            start: 0,
            end: source.len() as u32,
        });
        validate_range(&source, requested)?;
        let intersecting = statements
            .iter()
            .filter(|statement| ranges_intersect(statement.full_range, requested))
            .collect::<Vec<_>>();
        let actual_range = if request.range.is_some() && !intersecting.is_empty() {
            TextRange {
                start: intersecting
                    .iter()
                    .map(|statement| statement.full_range.start)
                    .min()
                    .unwrap_or(requested.start),
                end: intersecting
                    .iter()
                    .map(|statement| statement.full_range.end)
                    .max()
                    .unwrap_or(requested.end),
            }
        } else {
            requested
        };
        let protected = statements
            .iter()
            .filter(|statement| statement.recovered)
            .map(|statement| statement.full_range)
            .collect::<Vec<_>>();
        let flavor = dialect_flavor(&dialect_id)?;
        let edits = keyword_case_edits(
            &source,
            actual_range,
            &protected,
            flavor,
            request.options.keyword_case,
            canceled,
        )?;
        Ok(WorkspaceEdit {
            documents: vec![DocumentEdit {
                document_id: id,
                expected_revision: request.revision,
                source_digest,
                edits,
            }],
            warnings: (!protected.is_empty())
                .then(|| "recovered statements were preserved verbatim".to_string())
                .into_iter()
                .collect(),
            is_complete: protected.is_empty(),
            actual_range: Some(actual_range),
        })
    }

    pub fn prepare_quick_fix(
        &self,
        scope: DocumentScope,
        id: SemanticDocumentId,
        revision: u64,
        fix_id: &str,
        catalog: &CatalogBindingView,
    ) -> Result<WorkspaceEdit, Error> {
        let (source, source_digest, dialect_id) = {
            let registry = self.inner.lock().unwrap();
            let document = registry.documents.get(&id).ok_or(Error::NotFound)?;
            ensure_scope(document, scope)?;
            ensure_revision(document, revision)?;
            (
                Arc::clone(&document.source),
                document.source_digest.clone(),
                document.dialect_id.clone(),
            )
        };
        let mut parts = fix_id.splitn(4, ':');
        if parts.next() != Some("qualify") {
            return Err(Error::InvalidRequest);
        }
        let start = parts
            .next()
            .and_then(|value| value.parse::<u32>().ok())
            .ok_or(Error::InvalidRequest)?;
        let end = parts
            .next()
            .and_then(|value| value.parse::<u32>().ok())
            .ok_or(Error::InvalidRequest)?;
        let object_id = parts.next().ok_or(Error::InvalidRequest)?;
        let range = TextRange { start, end };
        validate_range(&source, range)?;
        let object = catalog
            .objects
            .iter()
            .find(|object| object.id.0 == object_id)
            .ok_or(Error::InvalidRequest)?;
        let existing = &source[start as usize..end as usize];
        if !existing.eq_ignore_ascii_case(&object.name) {
            return Err(Error::InvalidRequest);
        }
        let replacement = match dialect_flavor(&dialect_id)? {
            Flavor::Postgres => format!(
                "\"{}\".\"{}\"",
                object.schema.replace('"', "\"\""),
                object.name.replace('"', "\"\"")
            ),
            Flavor::Tsql => format!(
                "[{}].[{}]",
                object.schema.replace(']', "]]"),
                object.name.replace(']', "]]"),
            ),
        };
        Ok(WorkspaceEdit {
            documents: vec![DocumentEdit {
                document_id: id,
                expected_revision: revision,
                source_digest,
                edits: vec![TextEdit {
                    range,
                    new_text: replacement,
                }],
            }],
            warnings: Vec::new(),
            is_complete: true,
            actual_range: None,
        })
    }

    pub fn find_usages(
        &self,
        scope: DocumentScope,
        id: SemanticDocumentId,
        request: FindSqlUsagesRequest,
        catalog: Option<&CatalogBindingView>,
    ) -> Result<SqlUsagePage, Error> {
        let source = {
            let registry = self.inner.lock().unwrap();
            let document = registry.documents.get(&id).ok_or(Error::NotFound)?;
            ensure_scope(document, scope)?;
            ensure_revision(document, request.revision)?;
            Arc::clone(&document.source)
        };
        let tokens = binding_tokens(&source);
        let (target_name, target_quoted, catalog_object_id) = match &request.target {
            SqlSymbolTarget::AtPosition { position } => {
                validate_offset(&source, *position)?;
                let token = tokens
                    .iter()
                    .find(|token| {
                        token.is_word()
                            && token.range.start <= *position
                            && *position <= token.range.end
                    })
                    .ok_or(Error::InvalidRequest)?;
                if is_format_keyword(&token.text) {
                    return Err(Error::InvalidRequest);
                }
                let matches = catalog
                    .into_iter()
                    .flat_map(|catalog| catalog.objects.iter())
                    .filter(|object| identifier_matches(&object.name, &token.text, token.quoted))
                    .collect::<Vec<_>>();
                (
                    token.text.clone(),
                    token.quoted,
                    (matches.len() == 1).then(|| matches[0].id.clone()),
                )
            }
            SqlSymbolTarget::CatalogObject { object_id } => {
                let object = catalog
                    .and_then(|catalog| {
                        catalog
                            .objects
                            .iter()
                            .find(|object| object.id == *object_id)
                    })
                    .ok_or(Error::InvalidRequest)?;
                (object.name.clone(), false, Some(object.id.clone()))
            }
        };
        let all = tokens
            .iter()
            .filter(|token| {
                token.is_word() && identifier_matches(&target_name, &token.text, target_quoted)
            })
            .map(|token| SqlUsage {
                range: token.range,
                kind: classify_usage(&source, token.range),
                catalog_object_id: catalog_object_id.clone(),
            })
            .collect::<Vec<_>>();
        let offset = parse_usage_cursor(request.cursor.as_deref(), request.revision)?;
        if offset > all.len() {
            return Err(Error::InvalidRequest);
        }
        let limit = request.limit.unwrap_or(100);
        if limit == 0 || limit > 500 {
            return Err(Error::LimitExceeded);
        }
        let end = usize::min(offset.saturating_add(limit as usize), all.len());
        Ok(SqlUsagePage {
            document_id: id,
            revision: request.revision,
            catalog_revision: catalog.map(|catalog| catalog.revision),
            usages: all[offset..end].to_vec(),
            next_cursor: (end < all.len()).then(|| format!("usage:{}:{end}", request.revision)),
            is_complete: true,
            search_scope: "current_document_and_visible_catalog".into(),
        })
    }

    pub fn prepare_refactor(
        &self,
        scope: DocumentScope,
        id: SemanticDocumentId,
        request: PrepareSqlRefactorRequest,
        catalog: Option<&CatalogBindingView>,
    ) -> Result<WorkspaceEdit, Error> {
        let (source, source_digest, dialect_id) = {
            let registry = self.inner.lock().unwrap();
            let document = registry.documents.get(&id).ok_or(Error::NotFound)?;
            ensure_scope(document, scope)?;
            ensure_revision(document, request.revision)?;
            (
                Arc::clone(&document.source),
                document.source_digest.clone(),
                document.dialect_id.clone(),
            )
        };
        let tokens = binding_tokens(&source);
        let (edits, warnings, complete) = match request.refactor {
            SqlRefactor::RenameSymbol { position, new_name } => {
                validate_offset(&source, position)?;
                if new_name.is_empty()
                    || new_name.len() > 256
                    || new_name.chars().any(char::is_control)
                {
                    return Err(Error::InvalidRequest);
                }
                let target = tokens
                    .iter()
                    .find(|token| {
                        token.is_word()
                            && token.range.start <= position
                            && position <= token.range.end
                    })
                    .ok_or(Error::InvalidRequest)?;
                if is_format_keyword(&target.text) {
                    return Err(Error::InvalidRequest);
                }
                let replacement = render_identifier(&new_name, dialect_flavor(&dialect_id)?);
                let edits = tokens
                    .iter()
                    .filter(|token| {
                        token.is_word()
                            && identifier_matches(&target.text, &token.text, target.quoted)
                    })
                    .map(|token| TextEdit {
                        range: token.range,
                        new_text: replacement.clone(),
                    })
                    .collect();
                (
                    edits,
                    vec!["rename is limited to the current semantic document".into()],
                    false,
                )
            }
            SqlRefactor::QualifyName { position } => {
                validate_offset(&source, position)?;
                let target = tokens
                    .iter()
                    .find(|token| {
                        token.is_word()
                            && token.range.start <= position
                            && position <= token.range.end
                    })
                    .ok_or(Error::InvalidRequest)?;
                let matches = catalog
                    .into_iter()
                    .flat_map(|catalog| catalog.objects.iter())
                    .filter(|object| identifier_matches(&object.name, &target.text, target.quoted))
                    .collect::<Vec<_>>();
                if matches.len() != 1 {
                    return Err(Error::InvalidRequest);
                }
                let flavor = dialect_flavor(&dialect_id)?;
                let replacement = format!(
                    "{}.{}",
                    render_identifier(&matches[0].schema, flavor),
                    render_identifier(&matches[0].name, flavor)
                );
                (
                    vec![TextEdit {
                        range: target.range,
                        new_text: replacement,
                    }],
                    Vec::new(),
                    true,
                )
            }
        };
        Ok(WorkspaceEdit {
            documents: vec![DocumentEdit {
                document_id: id,
                expected_revision: request.revision,
                source_digest,
                edits,
            }],
            warnings,
            is_complete: complete,
            actual_range: None,
        })
    }

    pub fn completion_analysis(
        &self,
        scope: DocumentScope,
        id: SemanticDocumentId,
        revision: u64,
        cursor: u32,
    ) -> Result<CompletionAnalysis, Error> {
        let (source, dialect_id) = {
            let registry = self.inner.lock().unwrap();
            let document = registry.documents.get(&id).ok_or(Error::NotFound)?;
            ensure_scope(document, scope)?;
            ensure_revision(document, revision)?;
            validate_offset(&document.source, cursor)?;
            (Arc::clone(&document.source), document.dialect_id.clone())
        };
        detect_completion_context(&source, cursor as usize, &dialect_id)
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
                .collect::<Vec<_>>();
            let symbols = outline_symbols(&document.source, &statements);
            return Ok(StatementSelection {
                document_id: id,
                revision: request.revision,
                selection: Some(range),
                statements,
                symbols,
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
        let statements = chosen.into_iter().collect::<Vec<_>>();
        let symbols = outline_symbols(&document.source, &statements);
        Ok(StatementSelection {
            document_id: id,
            revision: request.revision,
            selection: statements
                .first()
                .map(|statement| statement.executable_range),
            statements,
            symbols,
        })
    }

    pub fn statement_source(
        &self,
        scope: DocumentScope,
        id: SemanticDocumentId,
        revision: u64,
        statement_id: &str,
    ) -> Result<StatementSource, Error> {
        let registry = self.inner.lock().unwrap();
        let document = registry.documents.get(&id).ok_or(Error::NotFound)?;
        ensure_scope(document, scope)?;
        ensure_revision(document, revision)?;
        let statement = document
            .statements
            .iter()
            .find(|statement| statement.statement_id == statement_id)
            .cloned()
            .ok_or(Error::InvalidRequest)?;
        let range = statement.executable_range;
        validate_range(&document.source, range)?;
        Ok(StatementSource {
            source_digest: document.source_digest.clone(),
            sql: document.source[range.start as usize..range.end as usize].to_string(),
            statement,
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

fn dialect_flavor(dialect_id: &sift_protocol::DialectId) -> Result<Flavor, Error> {
    match dialect_id.as_str() {
        "sift/postgresql" => Ok(Flavor::Postgres),
        "sift/tsql" => Ok(Flavor::Tsql),
        other => Err(Error::DialectUnavailable(other.to_string())),
    }
}

fn keyword_case_edits(
    source: &str,
    target: TextRange,
    protected: &[TextRange],
    flavor: Flavor,
    keyword_case: KeywordCase,
    canceled: &AtomicBool,
) -> Result<Vec<TextEdit>, Error> {
    if keyword_case == KeywordCase::Preserve {
        return Ok(Vec::new());
    }
    let bytes = source.as_bytes();
    let mut edits = Vec::new();
    let mut index = 0usize;
    let mut quote = None;
    let mut bracket = false;
    let mut line_comment = false;
    let mut block_depth = 0u32;
    let mut dollar_tag: Option<Vec<u8>> = None;
    while index < bytes.len() {
        if index % 4096 == 0 && canceled.load(Ordering::Relaxed) {
            return Err(Error::Canceled);
        }
        if line_comment {
            line_comment = bytes[index] != b'\n';
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
        } else if bytes[index].is_ascii_alphabetic() || bytes[index] == b'_' {
            let start = index;
            index += 1;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                index += 1;
            }
            let range = TextRange {
                start: start as u32,
                end: index as u32,
            };
            if range.start < target.start
                || range.end > target.end
                || protected
                    .iter()
                    .any(|protected| ranges_intersect(*protected, range))
            {
                continue;
            }
            let word = &source[start..index];
            if !is_format_keyword(word) {
                continue;
            }
            let replacement = match keyword_case {
                KeywordCase::Preserve => unreachable!(),
                KeywordCase::Upper => word.to_ascii_uppercase(),
                KeywordCase::Lower => word.to_ascii_lowercase(),
            };
            if replacement != word {
                edits.push(TextEdit {
                    range,
                    new_text: replacement,
                });
                if edits.len() > 10_000 {
                    return Err(Error::LimitExceeded);
                }
            }
        } else {
            index += 1;
        }
    }
    Ok(edits)
}

fn is_format_keyword(word: &str) -> bool {
    matches_ci(
        word,
        &[
            "ADD",
            "ALL",
            "ALTER",
            "AND",
            "AS",
            "ASC",
            "BEGIN",
            "BETWEEN",
            "BY",
            "CALL",
            "CASE",
            "CHECK",
            "COLUMN",
            "COMMIT",
            "CONSTRAINT",
            "CREATE",
            "CROSS",
            "DELETE",
            "DESC",
            "DISTINCT",
            "DO",
            "DROP",
            "ELSE",
            "END",
            "EXCEPT",
            "EXEC",
            "EXECUTE",
            "EXISTS",
            "FALSE",
            "FETCH",
            "FOREIGN",
            "FROM",
            "FULL",
            "FUNCTION",
            "GROUP",
            "HAVING",
            "IF",
            "IN",
            "INDEX",
            "INNER",
            "INSERT",
            "INTERSECT",
            "INTO",
            "IS",
            "JOIN",
            "KEY",
            "LEFT",
            "LIKE",
            "LIMIT",
            "MERGE",
            "NOT",
            "NULL",
            "OFFSET",
            "ON",
            "OR",
            "ORDER",
            "OUTER",
            "OUTPUT",
            "PRIMARY",
            "PROCEDURE",
            "REFERENCES",
            "RETURNING",
            "RIGHT",
            "ROLLBACK",
            "SAVEPOINT",
            "SELECT",
            "SET",
            "TABLE",
            "THEN",
            "TOP",
            "TRANSACTION",
            "TRIGGER",
            "TRUE",
            "TRUNCATE",
            "UNION",
            "UNIQUE",
            "UPDATE",
            "USING",
            "VALUES",
            "VIEW",
            "WHEN",
            "WHERE",
            "WITH",
        ],
    )
}

#[derive(Debug)]
struct BindingToken {
    text: String,
    range: TextRange,
    quoted: bool,
    kind: BindingTokenKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BindingTokenKind {
    Word,
    Dot,
    LeftParen,
    RightParen,
    Comma,
}

impl BindingToken {
    const fn is_word(&self) -> bool {
        matches!(self.kind, BindingTokenKind::Word)
    }

    const fn is_dot(&self) -> bool {
        matches!(self.kind, BindingTokenKind::Dot)
    }
}

fn bind_catalog_references(
    source: &str,
    revision: u64,
    catalog: &CatalogBindingView,
) -> Vec<SemanticDiagnostic> {
    let tokens = binding_tokens(source);
    let mut diagnostics = Vec::new();
    let mut index = 0usize;
    while index < tokens.len() {
        if !tokens[index].is_word()
            || !matches_ci(&tokens[index].text, &["FROM", "JOIN", "UPDATE", "INTO"])
        {
            index += 1;
            continue;
        }
        let Some(first) = tokens.get(index + 1).filter(|token| token.is_word()) else {
            index += 1;
            continue;
        };
        let (schema, object, range, qualified) =
            if tokens.get(index + 2).is_some_and(BindingToken::is_dot) {
                let Some(second) = tokens.get(index + 3).filter(|token| token.is_word()) else {
                    index += 1;
                    continue;
                };
                (
                    Some((first.text.as_str(), first.quoted)),
                    (second.text.as_str(), second.quoted),
                    TextRange {
                        start: first.range.start,
                        end: second.range.end,
                    },
                    true,
                )
            } else {
                (
                    None,
                    (first.text.as_str(), first.quoted),
                    first.range,
                    false,
                )
            };
        let matches = catalog.matching_objects(schema, object);
        let ordinal = diagnostics.len();
        if matches.is_empty() {
            if catalog.complete {
                diagnostics.push(SemanticDiagnostic {
                    id: format!("{revision}:binder:{ordinal}"),
                    severity: DiagnosticSeverity::Error,
                    code: "undefined_object".into(),
                    message: format!("catalog object `{}` was not found", object.0),
                    range,
                    related_ranges: Vec::new(),
                    source: "binder".into(),
                    quick_fix_ids: Vec::new(),
                });
            }
        } else if !qualified && matches.len() == 1 {
            let candidate = matches[0];
            diagnostics.push(SemanticDiagnostic {
                id: format!("{revision}:binder:{ordinal}"),
                severity: DiagnosticSeverity::Hint,
                code: "unqualified_object".into(),
                message: format!(
                    "object resolves uniquely to `{}.{}`",
                    candidate.schema, candidate.name
                ),
                range,
                related_ranges: Vec::new(),
                source: "binder".into(),
                quick_fix_ids: vec![format!(
                    "qualify:{}:{}:{}",
                    range.start, range.end, candidate.id.0
                )],
            });
        }
        index += if qualified { 4 } else { 2 };
    }
    diagnostics
}

fn identifier_matches(catalog: &str, source: &str, quoted: bool) -> bool {
    if quoted {
        catalog == source
    } else {
        catalog.eq_ignore_ascii_case(source)
    }
}

fn parse_usage_cursor(cursor: Option<&str>, revision: u64) -> Result<usize, Error> {
    let Some(cursor) = cursor else {
        return Ok(0);
    };
    let mut parts = cursor.split(':');
    if parts.next() != Some("usage")
        || parts.next().and_then(|value| value.parse::<u64>().ok()) != Some(revision)
    {
        return Err(Error::InvalidRequest);
    }
    let offset = parts
        .next()
        .and_then(|value| value.parse::<usize>().ok())
        .ok_or(Error::InvalidRequest)?;
    if parts.next().is_some() {
        return Err(Error::InvalidRequest);
    }
    Ok(offset)
}

fn classify_usage(source: &str, range: TextRange) -> SqlUsageKind {
    let prefix = &source[..range.start as usize];
    let statement = prefix.rsplit_once(';').map_or(prefix, |(_, tail)| tail);
    let first = statement.split_whitespace().next().unwrap_or_default();
    if matches_ci(first, &["CREATE", "ALTER", "DROP"]) {
        SqlUsageKind::Definition
    } else if matches_ci(first, &["INSERT", "UPDATE", "DELETE", "MERGE"]) {
        SqlUsageKind::Write
    } else if matches_ci(first, &["CALL", "EXEC", "EXECUTE"]) {
        SqlUsageKind::Call
    } else {
        SqlUsageKind::Read
    }
}

fn render_identifier(identifier: &str, flavor: Flavor) -> String {
    match flavor {
        Flavor::Postgres => format!("\"{}\"", identifier.replace('"', "\"\"")),
        Flavor::Tsql => format!("[{}]", identifier.replace(']', "]]")),
    }
}

fn binding_tokens(source: &str) -> Vec<BindingToken> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index..].starts_with(b"--") {
            index = source[index..]
                .find('\n')
                .map_or(bytes.len(), |offset| index + offset + 1);
        } else if bytes[index..].starts_with(b"/*") {
            index = source[index + 2..]
                .find("*/")
                .map_or(bytes.len(), |offset| index + offset + 4);
        } else if bytes[index] == b'$' {
            if let Some(tag) = dollar_quote_tag(&bytes[index..]) {
                index += tag.len();
                index = source.as_bytes()[index..]
                    .windows(tag.len())
                    .position(|window| window == tag)
                    .map_or(bytes.len(), |offset| index + offset + tag.len());
            } else {
                index += 1;
            }
        } else if bytes[index] == b'\'' {
            index += 1;
            while index < bytes.len() {
                if bytes[index] == b'\'' {
                    index += 1;
                    if bytes.get(index) == Some(&b'\'') {
                        index += 1;
                    } else {
                        break;
                    }
                } else {
                    index += 1;
                }
            }
        } else if bytes[index] == b'"' || bytes[index] == b'[' {
            let start = index;
            let (closing, escaped) = if bytes[index] == b'"' {
                (b'"', b'"')
            } else {
                (b']', b']')
            };
            index += 1;
            let content_start = index;
            let mut text = String::new();
            while index < bytes.len() {
                if bytes[index] == closing {
                    if bytes.get(index + 1) == Some(&escaped) {
                        text.push(closing as char);
                        index += 2;
                    } else {
                        index += 1;
                        break;
                    }
                } else {
                    let character = source[index..].chars().next().unwrap();
                    text.push(character);
                    index += character.len_utf8();
                }
            }
            if text.is_empty() && content_start < index.saturating_sub(1) {
                text = source[content_start..index - 1].to_string();
            }
            tokens.push(BindingToken {
                text,
                range: TextRange {
                    start: start as u32,
                    end: index as u32,
                },
                quoted: true,
                kind: BindingTokenKind::Word,
            });
        } else if bytes[index].is_ascii_alphabetic() || bytes[index] == b'_' {
            let start = index;
            index += 1;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric()
                    || bytes[index] == b'_'
                    || bytes[index] >= 0x80)
            {
                if bytes[index] >= 0x80 {
                    index += source[index..].chars().next().unwrap().len_utf8();
                } else {
                    index += 1;
                }
            }
            tokens.push(BindingToken {
                text: source[start..index].to_string(),
                range: TextRange {
                    start: start as u32,
                    end: index as u32,
                },
                quoted: false,
                kind: BindingTokenKind::Word,
            });
        } else if matches!(bytes[index], b'.' | b'(' | b')' | b',') {
            let kind = match bytes[index] {
                b'.' => BindingTokenKind::Dot,
                b'(' => BindingTokenKind::LeftParen,
                b')' => BindingTokenKind::RightParen,
                b',' => BindingTokenKind::Comma,
                _ => unreachable!(),
            };
            tokens.push(BindingToken {
                text: (bytes[index] as char).to_string(),
                range: TextRange {
                    start: index as u32,
                    end: index as u32 + 1,
                },
                quoted: false,
                kind,
            });
            index += 1;
        } else {
            index += 1;
        }
    }
    tokens
}

fn outline_symbols(source: &str, statements: &[SemanticStatement]) -> Vec<SemanticOutlineSymbol> {
    let tokens = binding_tokens(source);
    let mut symbols = Vec::new();
    for statement in statements {
        let statement_tokens = tokens
            .iter()
            .filter(|token| {
                statement.executable_range.start <= token.range.start
                    && token.range.end <= statement.executable_range.end
            })
            .collect::<Vec<_>>();
        let ctes = outline_cte_definitions(&statement_tokens);
        for (name, range) in &ctes {
            symbols.push(SemanticOutlineSymbol {
                symbol_id: format!("{}:cte:{}", statement.statement_id, range.start),
                statement_id: statement.statement_id.clone(),
                kind: SemanticOutlineSymbolKind::Cte,
                name: name.clone(),
                range: *range,
                definition_range: Some(*range),
                alias: None,
                target: None,
                usage_kind: SqlUsageKind::Definition,
            });
        }
        let cte_map = ctes
            .iter()
            .map(|(name, range)| (name.to_lowercase(), *range))
            .collect::<HashMap<_, _>>();
        let mut index = 0usize;
        while index < statement_tokens.len() {
            let token = statement_tokens[index];
            if !token.is_word() {
                index += 1;
                continue;
            }
            let usage_kind = if token.text.eq_ignore_ascii_case("FROM")
                && statement.kind == StatementKind::Delete
            {
                SqlUsageKind::Write
            } else if matches_ci(&token.text, &["FROM", "JOIN", "USING"]) {
                SqlUsageKind::Read
            } else if matches_ci(&token.text, &["UPDATE", "INTO"]) {
                SqlUsageKind::Write
            } else if matches_ci(&token.text, &["CALL", "EXEC", "EXECUTE"]) {
                SqlUsageKind::Call
            } else {
                index += 1;
                continue;
            };
            let Some((target, target_name, range, next)) =
                outline_relation_target(&statement_tokens, index + 1)
            else {
                index += 1;
                continue;
            };
            let (alias, next) = outline_relation_alias(&statement_tokens, next);
            let cte_definition = (!target.contains('.'))
                .then(|| cte_map.get(&target_name.to_lowercase()).copied())
                .flatten();
            let kind = if cte_definition.is_some() {
                SemanticOutlineSymbolKind::Cte
            } else {
                SemanticOutlineSymbolKind::Object
            };
            symbols.push(SemanticOutlineSymbol {
                symbol_id: format!(
                    "{}:{}:{}",
                    statement.statement_id,
                    if kind == SemanticOutlineSymbolKind::Cte {
                        "cte-ref"
                    } else {
                        "object"
                    },
                    range.start
                ),
                statement_id: statement.statement_id.clone(),
                kind,
                name: alias
                    .as_ref()
                    .map_or_else(|| target_name.clone(), |(alias, _)| alias.clone()),
                range: TextRange {
                    start: range.start,
                    end: alias.as_ref().map_or(range.end, |(_, range)| range.end),
                },
                definition_range: cte_definition,
                alias: alias.map(|(alias, _)| alias),
                target: Some(target),
                usage_kind,
            });
            index = next.max(index + 1);
        }
    }
    symbols
}

fn outline_cte_definitions(tokens: &[&BindingToken]) -> Vec<(String, TextRange)> {
    let Some(mut index) = tokens
        .iter()
        .position(|token| token.is_word() && token.text.eq_ignore_ascii_case("WITH"))
    else {
        return Vec::new();
    };
    index += 1;
    if tokens
        .get(index)
        .is_some_and(|token| token.is_word() && token.text.eq_ignore_ascii_case("RECURSIVE"))
    {
        index += 1;
    }
    let mut definitions = Vec::new();
    while let Some(name) = tokens.get(index).filter(|token| token.is_word()) {
        let definition = (name.text.clone(), name.range);
        index += 1;
        if tokens
            .get(index)
            .is_some_and(|token| token.kind == BindingTokenKind::LeftParen)
        {
            let Some(next) = outline_after_balanced_parens(tokens, index) else {
                break;
            };
            index = next;
        }
        if !tokens
            .get(index)
            .is_some_and(|token| token.is_word() && token.text.eq_ignore_ascii_case("AS"))
        {
            break;
        }
        index += 1;
        if !tokens
            .get(index)
            .is_some_and(|token| token.kind == BindingTokenKind::LeftParen)
        {
            break;
        }
        definitions.push(definition);
        let Some(next) = outline_after_balanced_parens(tokens, index) else {
            break;
        };
        index = next;
        if !tokens
            .get(index)
            .is_some_and(|token| token.kind == BindingTokenKind::Comma)
        {
            break;
        }
        index += 1;
    }
    definitions
}

fn outline_after_balanced_parens(tokens: &[&BindingToken], start: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (index, token) in tokens.iter().enumerate().skip(start) {
        match token.kind {
            BindingTokenKind::LeftParen => depth += 1,
            BindingTokenKind::RightParen => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index + 1);
                }
            }
            _ => {}
        }
    }
    None
}

fn outline_relation_target(
    tokens: &[&BindingToken],
    start: usize,
) -> Option<(String, String, TextRange, usize)> {
    let first = *tokens.get(start)?;
    if !first.is_word() {
        return None;
    }
    let mut parts = vec![first.text.clone()];
    let mut end = first.range.end;
    let mut index = start + 1;
    while parts.len() < 3
        && tokens.get(index).is_some_and(|token| token.is_dot())
        && tokens.get(index + 1).is_some_and(|token| token.is_word())
    {
        let word = tokens[index + 1];
        parts.push(word.text.clone());
        end = word.range.end;
        index += 2;
    }
    Some((
        parts.join("."),
        parts.last().cloned().unwrap_or_default(),
        TextRange {
            start: first.range.start,
            end,
        },
        index,
    ))
}

fn outline_relation_alias(
    tokens: &[&BindingToken],
    mut index: usize,
) -> (Option<(String, TextRange)>, usize) {
    if tokens
        .get(index)
        .is_some_and(|token| token.is_word() && token.text.eq_ignore_ascii_case("AS"))
    {
        index += 1;
    }
    let alias = tokens
        .get(index)
        .filter(|token| token.is_word() && !is_alias_stop(&token.text))
        .map(|token| (token.text.clone(), token.range));
    if alias.is_some() {
        index += 1;
    }
    (alias, index)
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

/// Detect a completion slot from tolerant tokenization of shared semantic
/// source. Stateful callers validate the exact revision before reaching here.
pub fn detect_completion_context(
    sql: &str,
    cursor: usize,
    dialect_id: &sift_protocol::DialectId,
) -> Result<CompletionAnalysis, Error> {
    if cursor > sql.len() || !sql.is_char_boundary(cursor) {
        return Err(Error::InvalidRange);
    }
    let flavor = match dialect_id.as_str() {
        "sift/postgresql" => Flavor::Postgres,
        "sift/tsql" => Flavor::Tsql,
        other => return Err(Error::DialectUnavailable(other.to_string())),
    };
    let (prefix_start, prefix) = extract_prefix(sql, cursor, flavor);
    let dialect: Box<dyn Dialect> = match flavor {
        Flavor::Postgres => Box::new(PostgreSqlDialect {}),
        Flavor::Tsql => Box::new(MsSqlDialect {}),
    };
    let tokens = semantic_tokens(&*dialect, &sql[..prefix_start]);
    // Bindings can be declared after the cursor (`SELECT u.| FROM users u`),
    // so relation discovery consumes the whole revision while slot
    // classification consumes only the text preceding the replacement range.
    let relations = semantic_tokens(&*dialect, sql);
    Ok(CompletionAnalysis {
        context: classify_completion(&tokens),
        cursor,
        prefix_start,
        prefix_lower: prefix.to_ascii_lowercase(),
        prefix,
        relations: completion_relations(&relations),
    })
}

fn semantic_tokens(dialect: &dyn Dialect, source: &str) -> Vec<Token> {
    match Tokenizer::new(dialect, source).tokenize() {
        Ok(tokens) => tokens
            .into_iter()
            .filter(|token| !is_ignorable(token))
            .collect(),
        Err(error) => {
            tracing::debug!(%error, "completion tokenization failed");
            Vec::new()
        }
    }
}

fn completion_relations(tokens: &[Token]) -> Vec<CompletionRelation> {
    let mut relations = Vec::new();
    let mut index = 0usize;
    while index < tokens.len() {
        let Some(word) = word_value(&tokens[index]) else {
            index += 1;
            continue;
        };
        if word.eq_ignore_ascii_case("WITH") {
            if let Some(name) = tokens.get(index + 1).and_then(word_value) {
                push_relation(&mut relations, name, None, false);
            }
        }
        if word.eq_ignore_ascii_case("CREATE") {
            let mut table = index + 1;
            if tokens.get(table).and_then(word_value).is_some_and(|value| {
                value.eq_ignore_ascii_case("TEMP") || value.eq_ignore_ascii_case("TEMPORARY")
            }) {
                table += 1;
            }
            if tokens
                .get(table)
                .and_then(word_value)
                .is_some_and(|value| value.eq_ignore_ascii_case("TABLE"))
            {
                if let Some(name) = tokens.get(table + 1).and_then(word_value) {
                    push_relation(&mut relations, name, None, false);
                }
            }
        }
        if is_from_or_join(word) {
            let Some(mut object_index) = next_word_index(tokens, index + 1) else {
                index += 1;
                continue;
            };
            let mut object_parts = vec![word_value(&tokens[object_index]).unwrap_or_default()];
            while matches!(tokens.get(object_index + 1), Some(Token::Period)) {
                let Some(next) = tokens.get(object_index + 2).and_then(word_value) else {
                    break;
                };
                object_parts.push(next);
                object_index += 2;
            }
            let object = object_parts.join(".");
            let relation_name = object_parts.last().copied().unwrap_or_default();
            push_relation(&mut relations, relation_name, Some(object.as_str()), false);
            let mut alias_index = next_word_index(tokens, object_index + 1);
            if alias_index.is_some_and(|candidate| {
                word_value(&tokens[candidate]).is_some_and(|value| value.eq_ignore_ascii_case("AS"))
            }) {
                alias_index =
                    alias_index.and_then(|candidate| next_word_index(tokens, candidate + 1));
            }
            if let Some(alias) = alias_index.and_then(|candidate| word_value(&tokens[candidate])) {
                if !is_alias_stop(alias) {
                    push_relation(&mut relations, alias, Some(&object), true);
                }
            }
        }
        index += 1;
    }
    relations
}

fn push_relation(
    relations: &mut Vec<CompletionRelation>,
    name: &str,
    target: Option<&str>,
    is_alias: bool,
) {
    if !relations
        .iter()
        .any(|relation| relation.name.eq_ignore_ascii_case(name))
    {
        relations.push(CompletionRelation {
            name: name.to_string(),
            target: target.map(str::to_string),
            is_alias,
        });
    }
}

fn next_word_index(tokens: &[Token], start: usize) -> Option<usize> {
    (start..tokens.len()).find(|index| word_value(&tokens[*index]).is_some())
}

fn is_from_or_join(word: &str) -> bool {
    matches_ci(word, &["FROM", "JOIN"])
}

fn is_alias_stop(word: &str) -> bool {
    matches_ci(
        word,
        &[
            "WHERE",
            "JOIN",
            "LEFT",
            "RIGHT",
            "FULL",
            "INNER",
            "OUTER",
            "CROSS",
            "ON",
            "GROUP",
            "ORDER",
            "HAVING",
            "LIMIT",
            "OFFSET",
            "UNION",
            "EXCEPT",
            "INTERSECT",
            "SELECT",
            "FROM",
            "SET",
            "RETURNING",
            "OUTPUT",
            "INSERT",
            "UPDATE",
            "DELETE",
            "CREATE",
            "ALTER",
            "DROP",
        ],
    )
}

fn classify_completion(tokens: &[Token]) -> CompletionContext {
    // Completion context never crosses a statement delimiter. In particular,
    // `SELECT 1; SEL|` is a statement-leading slot, not a continuation of the
    // preceding SELECT list.
    let tokens = tokens
        .iter()
        .rposition(|token| matches!(token, Token::SemiColon))
        .map_or(tokens, |delimiter| &tokens[delimiter + 1..]);
    if let Some(Token::Period) = tokens.last() {
        let qualifier = tokens
            .get(tokens.len().wrapping_sub(2))
            .and_then(word_value);
        if let Some(qualifier) = qualifier {
            let preceding = tokens
                .get(tokens.len().wrapping_sub(3))
                .and_then(word_value)
                .unwrap_or_default();
            if is_table_slot_lead(preceding) {
                return CompletionContext::ExpectingObjectInSchema {
                    schema: qualifier.to_string(),
                };
            }
            return CompletionContext::ExpectingColumn {
                qualifier: Some(qualifier.to_string()),
            };
        }
    }
    for token in tokens.iter().rev() {
        let Some(word) = word_value(token) else {
            continue;
        };
        if is_table_slot_lead(word) {
            return CompletionContext::ExpectingTable;
        }
        if matches_ci(
            word,
            &[
                "SELECT", "WHERE", "SET", "BY", "ON", "HAVING", "AND", "OR", "NOT", "IS", "IN",
                "LIKE", "BETWEEN",
            ],
        ) {
            return CompletionContext::ExpectingColumn { qualifier: None };
        }
    }
    if tokens.is_empty() {
        CompletionContext::Statement
    } else {
        CompletionContext::Unknown
    }
}

fn extract_prefix(sql: &str, cursor: usize, flavor: Flavor) -> (usize, String) {
    let bytes = sql.as_bytes();
    let mut start = cursor;
    while start > 0 {
        let byte = bytes[start - 1];
        if byte.is_ascii_alphanumeric() || byte == b'_' || byte >= 0x80 {
            start -= 1;
        } else {
            break;
        }
    }
    let quoted = start > 0 && bytes[start - 1] == b'"';
    let bracketed = flavor == Flavor::Tsql
        && start > 0
        && bytes[start - 1] == b'['
        && !sql[cursor..].starts_with(']');
    if quoted || bracketed {
        start -= 1;
    }
    (start, sql[start..cursor].to_string())
}

fn is_ignorable(token: &Token) -> bool {
    matches!(
        token,
        Token::Whitespace(Whitespace::Space)
            | Token::Whitespace(Whitespace::Tab)
            | Token::Whitespace(Whitespace::Newline)
            | Token::Whitespace(Whitespace::SingleLineComment { .. })
            | Token::Whitespace(Whitespace::MultiLineComment(_))
    )
}

fn word_value(token: &Token) -> Option<&str> {
    match token {
        Token::Word(Word { value, .. }) => Some(value),
        _ => None,
    }
}

fn is_table_slot_lead(word: &str) -> bool {
    matches_ci(word, &["FROM", "JOIN", "INTO", "UPDATE", "TABLE"])
}

fn matches_ci(word: &str, values: &[&str]) -> bool {
    values.iter().any(|value| word.eq_ignore_ascii_case(value))
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
    fn statement_selection_describes_ctes_objects_aliases_and_usage() {
        let registry = SemanticRegistry::default();
        let scope = DocumentScope {
            session: 1,
            connection: 1,
        };
        let source = "WITH recent AS (SELECT * FROM jobs j), older AS (SELECT * FROM archive.jobs a) SELECT * FROM recent r JOIN public.users u ON u.id = r.user_id; UPDATE jobs j SET done = true; CALL refresh_jobs()";
        let state = registry
            .create(
                scope,
                dialect("sift/postgresql"),
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
                    cursor: 0,
                    selection: Some(TextRange {
                        start: 0,
                        end: source.len() as u32,
                    }),
                },
            )
            .unwrap();

        let recent_definition = selected
            .symbols
            .iter()
            .find(|symbol| {
                symbol.kind == SemanticOutlineSymbolKind::Cte
                    && symbol.name == "recent"
                    && symbol.usage_kind == SqlUsageKind::Definition
            })
            .unwrap();
        assert!(selected.symbols.iter().any(|symbol| {
            symbol.kind == SemanticOutlineSymbolKind::Cte
                && symbol.target.as_deref() == Some("recent")
                && symbol.alias.as_deref() == Some("r")
                && symbol.definition_range == recent_definition.definition_range
                && symbol.usage_kind == SqlUsageKind::Read
        }));
        for (target, alias) in [("jobs", "j"), ("archive.jobs", "a"), ("public.users", "u")] {
            assert!(selected.symbols.iter().any(|symbol| {
                symbol.kind == SemanticOutlineSymbolKind::Object
                    && symbol.target.as_deref() == Some(target)
                    && symbol.alias.as_deref() == Some(alias)
                    && symbol.usage_kind == SqlUsageKind::Read
            }));
        }
        assert!(selected.symbols.iter().any(|symbol| {
            symbol.target.as_deref() == Some("jobs") && symbol.usage_kind == SqlUsageKind::Write
        }));
        assert!(selected.symbols.iter().any(|symbol| {
            symbol.target.as_deref() == Some("refresh_jobs")
                && symbol.usage_kind == SqlUsageKind::Call
        }));
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
    fn formatter_is_revision_bound_comment_safe_and_idempotent() {
        let registry = SemanticRegistry::default();
        let scope = DocumentScope {
            session: 1,
            connection: 1,
        };
        let source = "select 'from' as value -- where\nfrom users where id is not null;";
        let state = registry
            .create(
                scope,
                dialect("sift/postgresql"),
                source.into(),
                None,
                &AtomicBool::new(false),
            )
            .unwrap();
        let result = registry
            .format(
                scope,
                state.document_id,
                FormatSqlRequest {
                    revision: 1,
                    range: None,
                    options: sift_protocol::FormatOptions::default(),
                },
                &AtomicBool::new(false),
            )
            .unwrap();
        let edits = &result.documents[0].edits;
        assert!(edits.iter().any(|edit| edit.new_text == "SELECT"));
        assert!(!edits.iter().any(|edit| {
            &source[edit.range.start as usize..edit.range.end as usize] == "where"
                && edit.range.start < source.find('\n').unwrap() as u32
        }));
        let mut formatted = source.to_string();
        for edit in edits.iter().rev() {
            formatted.replace_range(
                edit.range.start as usize..edit.range.end as usize,
                &edit.new_text,
            );
        }
        let updated = registry
            .update(
                scope,
                state.document_id,
                1,
                formatted,
                &AtomicBool::new(false),
            )
            .unwrap();
        let second = registry
            .format(
                scope,
                state.document_id,
                FormatSqlRequest {
                    revision: updated.revision,
                    range: None,
                    options: sift_protocol::FormatOptions::default(),
                },
                &AtomicBool::new(false),
            )
            .unwrap();
        assert!(second.documents[0].edits.is_empty());
    }

    #[test]
    fn catalog_binding_is_conservative_and_offers_qualification() {
        let catalog = CatalogBindingView::new(
            sift_protocol::CatalogRevision(7),
            true,
            vec![CatalogBindingObject {
                id: sift_protocol::CatalogObjectId("users-id".into()),
                catalog: "mock".into(),
                schema: "public".into(),
                name: "users".into(),
                qualified_name: "mock.public.users".into(),
                kind: sift_protocol::CatalogNodeKind::Table,
                complete: true,
                columns: vec![CatalogBindingColumn {
                    id: sift_protocol::CatalogObjectId("users-id-column-id".into()),
                    name: "id".into(),
                    type_ref: sift_protocol::TypeRef::Primitive(
                        sift_protocol::PrimitiveType::Int64,
                    ),
                    nullable: sift_protocol::Nullability::NotNullable,
                    ordinal: Some(1),
                }],
            }],
        );
        let diagnostics = bind_catalog_references(
            "select * from users; select * from missing; -- from ignored\nselect $$from hidden$$",
            1,
            &catalog,
        );
        let users = catalog.resolve("mock.public.users").unwrap();
        assert_eq!(users.columns[0].name, "id");
        assert_eq!(
            users.columns[0].type_ref,
            sift_protocol::TypeRef::Primitive(sift_protocol::PrimitiveType::Int64)
        );
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "unqualified_object" && !diagnostic.quick_fix_ids.is_empty()
        }));
        assert_eq!(
            diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code == "undefined_object")
                .count(),
            1
        );
    }

    #[test]
    fn partial_catalog_does_not_claim_an_object_is_missing() {
        let diagnostics = bind_catalog_references(
            "select * from maybe_hidden",
            1,
            &CatalogBindingView::new(sift_protocol::CatalogRevision(1), false, Vec::new()),
        );
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn usages_page_and_refactor_only_touch_semantic_identifiers() {
        let registry = SemanticRegistry::default();
        let scope = DocumentScope {
            session: 1,
            connection: 1,
        };
        let source = "select users.id from users; -- users\nselect 'users'";
        let state = registry
            .create(
                scope,
                dialect("sift/postgresql"),
                source.into(),
                None,
                &AtomicBool::new(false),
            )
            .unwrap();
        let first = registry
            .find_usages(
                scope,
                state.document_id,
                FindSqlUsagesRequest {
                    revision: 1,
                    catalog_revision: None,
                    target: SqlSymbolTarget::AtPosition { position: 8 },
                    cursor: None,
                    limit: Some(1),
                },
                None,
            )
            .unwrap();
        assert_eq!(first.usages.len(), 1);
        assert!(first.next_cursor.is_some());
        let second = registry
            .find_usages(
                scope,
                state.document_id,
                FindSqlUsagesRequest {
                    revision: 1,
                    catalog_revision: None,
                    target: SqlSymbolTarget::AtPosition { position: 8 },
                    cursor: first.next_cursor,
                    limit: Some(10),
                },
                None,
            )
            .unwrap();
        assert_eq!(second.usages.len(), 1);
        let rename = registry
            .prepare_refactor(
                scope,
                state.document_id,
                PrepareSqlRefactorRequest {
                    revision: 1,
                    catalog_revision: None,
                    refactor: SqlRefactor::RenameSymbol {
                        position: 8,
                        new_name: "accounts".into(),
                    },
                },
                None,
            )
            .unwrap();
        assert_eq!(rename.documents[0].edits.len(), 2);
        assert!(rename.documents[0]
            .edits
            .iter()
            .all(|edit| edit.new_text == "\"accounts\""));
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

    #[test]
    fn completion_context_does_not_cross_statement_boundaries() {
        let source = "select * from users; SEL";
        let analysis =
            detect_completion_context(source, source.len(), &dialect("sift/postgresql")).unwrap();
        assert!(matches!(analysis.context, CompletionContext::Statement));
        assert_eq!(analysis.prefix, "SEL");
    }

    #[test]
    fn completion_resolves_table_aliases() {
        let source = "select u. from public.users as u";
        let cursor = source.find("u.").unwrap() + 2;
        let analysis =
            detect_completion_context(source, cursor, &dialect("sift/postgresql")).unwrap();
        assert!(matches!(
            analysis.context,
            CompletionContext::ExpectingColumn { ref qualifier }
                if qualifier.as_deref() == Some("u")
        ));
        assert!(analysis.relations.iter().any(|relation| {
            relation.name == "u"
                && relation.target.as_deref() == Some("public.users")
                && relation.is_alias
        }));
        assert!(analysis.relations.iter().any(|relation| {
            relation.name == "users"
                && relation.target.as_deref() == Some("public.users")
                && !relation.is_alias
        }));
    }
}
