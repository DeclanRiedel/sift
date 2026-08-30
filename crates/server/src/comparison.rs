//! Process-local comparison retention and safe table-filter rendering.

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use base64::Engine as _;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use dashmap::DashMap;
use sha2::{Digest, Sha256};
use sift_core::comparison::ComparisonDataset;
use sift_protocol::{
    ColumnMetadata, ComparePredicate, ComparePredicateOperator, ComparisonId, ComparisonPage,
    ComparisonPageRequest, ComparisonStatus, ComparisonSummary, ConnectionId, CursorId, Engine,
    RowDiff, SessionId, Value,
};

use crate::error::{ApiError, ApiResult};

const RESULT_TTL: Duration = Duration::from_secs(600);
const MAX_RETAINED_QUERY_RESULTS: usize = 1_024;
const MAX_COMPARISONS: usize = 1_024;
const MAX_PAGE_ROWS: usize = 500;
const MAX_COMPARISON_BYTES: usize = 64 * 1024 * 1024;
const MAX_RETAINED_COMPARISON_BYTES: usize = 256 * 1024 * 1024;
const COMPARISON_MEMORY_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone)]
pub struct RetainedQueryResult {
    pub session: SessionId,
    pub connection: ConnectionId,
    pub dataset: ComparisonDataset,
    pub schema_digest: String,
    created: Instant,
}

#[derive(Clone, Default)]
pub struct RetainedQueryRegistry {
    entries: Arc<DashMap<CursorId, RetainedQueryResult>>,
}

impl RetainedQueryRegistry {
    pub fn insert(
        &self,
        session: SessionId,
        connection: ConnectionId,
        cursor: CursorId,
        columns: Vec<ColumnMetadata>,
        rows: Vec<sift_protocol::Row>,
    ) {
        self.reap();
        if self.entries.len() >= MAX_RETAINED_QUERY_RESULTS {
            if let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|entry| entry.created)
                .map(|entry| *entry.key())
            {
                self.entries.remove(&oldest);
            }
        }
        let schema_digest = schema_digest(&columns);
        self.entries.insert(
            cursor,
            RetainedQueryResult {
                session,
                connection,
                dataset: ComparisonDataset {
                    columns,
                    rows,
                    immutable_order: true,
                },
                schema_digest,
                created: Instant::now(),
            },
        );
    }

    pub fn get(
        &self,
        session: SessionId,
        cursor: CursorId,
        result_set: u32,
        expected_schema_digest: &str,
    ) -> ApiResult<RetainedQueryResult> {
        self.reap();
        if result_set != 0 {
            return Err(ApiError::BadRequest(
                "retained HTTP query results contain only result_set=0".into(),
            ));
        }
        let result = self.entries.get(&cursor).ok_or_else(|| {
            ApiError::BadRequest(format!(
                "query result cursor {cursor} was not retained or has expired"
            ))
        })?;
        if result.session != session {
            return Err(ApiError::BadRequest(format!(
                "query result cursor {cursor} was not retained or has expired"
            )));
        }
        if result.schema_digest != expected_schema_digest {
            return Err(ApiError::BadRequest(format!(
                "stale query-result schema digest for cursor {cursor}"
            )));
        }
        Ok(result.clone())
    }

    fn reap(&self) {
        self.entries
            .retain(|_, result| result.created.elapsed() < RESULT_TTL);
    }
}

pub fn schema_digest(columns: &[ColumnMetadata]) -> String {
    let bytes = serde_json::to_vec(columns).unwrap_or_default();
    format!("schemafp:{:x}", Sha256::digest(bytes))
}

#[derive(Clone)]
pub struct ComparisonRegistry {
    entries: Arc<DashMap<ComparisonId, Arc<ComparisonEntry>>>,
    retained_bytes: Arc<AtomicUsize>,
    spill_key: [u8; 32],
}

impl Default for ComparisonRegistry {
    fn default() -> Self {
        let mut spill_key = [0u8; 32];
        getrandom::getrandom(&mut spill_key).expect("OS RNG is available");
        Self {
            entries: Arc::new(DashMap::new()),
            retained_bytes: Arc::new(AtomicUsize::new(0)),
            spill_key,
        }
    }
}

pub struct ComparisonEntry {
    session: SessionId,
    summary: Mutex<ComparisonSummary>,
    rows: RwLock<Arc<StoredRows>>,
    cancel: Arc<AtomicBool>,
    created: Instant,
    patch_context: Mutex<Option<PatchContext>>,
    retained_bytes: Arc<AtomicUsize>,
    spill_key: [u8; 32],
}

enum StoredRows {
    Empty,
    Memory {
        rows: Vec<RowDiff>,
        bytes: usize,
        retained_bytes: Arc<AtomicUsize>,
    },
    Spill {
        path: tempfile::TempPath,
        nonce: [u8; 12],
        len: usize,
        bytes: usize,
        retained_bytes: Arc<AtomicUsize>,
    },
}

impl Drop for StoredRows {
    fn drop(&mut self) {
        let (bytes, retained) = match self {
            Self::Empty => return,
            Self::Memory {
                bytes,
                retained_bytes,
                ..
            }
            | Self::Spill {
                bytes,
                retained_bytes,
                ..
            } => (*bytes, retained_bytes),
        };
        retained.fetch_sub(bytes, Ordering::AcqRel);
    }
}

#[derive(Debug, Clone)]
pub struct PatchContext {
    pub connection: ConnectionId,
    pub catalog_revision: sift_protocol::CatalogRevision,
    pub table: sift_protocol::ObjectPath,
    pub object: sift_protocol::ObjectInfo,
    pub target_is_left: bool,
    pub key: sift_protocol::ResolvedCompareKey,
}

impl ComparisonRegistry {
    pub fn create(&self, session: SessionId, summary: ComparisonSummary) -> Arc<ComparisonEntry> {
        self.reap();
        if self.entries.len() >= MAX_COMPARISONS {
            if let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|entry| entry.created)
                .map(|entry| *entry.key())
            {
                self.entries.remove(&oldest);
            }
        }
        let entry = Arc::new(ComparisonEntry {
            session,
            summary: Mutex::new(summary),
            rows: RwLock::new(Arc::new(StoredRows::Empty)),
            cancel: Arc::new(AtomicBool::new(false)),
            created: Instant::now(),
            patch_context: Mutex::new(None),
            retained_bytes: self.retained_bytes.clone(),
            spill_key: self.spill_key,
        });
        self.entries
            .insert(entry.summary().comparison_id, entry.clone());
        entry
    }

    pub fn get(&self, session: SessionId, id: ComparisonId) -> ApiResult<Arc<ComparisonEntry>> {
        self.reap();
        let entry = self.entries.get(&id).ok_or_else(|| {
            ApiError::BadRequest(format!("comparison {id} was not found or has expired"))
        })?;
        if entry.session != session {
            return Err(ApiError::BadRequest(format!(
                "comparison {id} was not found or has expired"
            )));
        }
        Ok(entry.clone())
    }

    pub fn page(
        &self,
        session: SessionId,
        id: ComparisonId,
        request: ComparisonPageRequest,
    ) -> ApiResult<ComparisonPage> {
        let entry = self.get(session, id)?;
        let start = request
            .after
            .as_deref()
            .map(|token| decode_page_token(&self.spill_key, id, token))
            .transpose()?
            .unwrap_or(0);
        let limit = request.limit.unwrap_or(100) as usize;
        if !(1..=MAX_PAGE_ROWS).contains(&limit) {
            return Err(ApiError::BadRequest(format!(
                "comparison page limit must be between 1 and {MAX_PAGE_ROWS}"
            )));
        }
        let rows = entry.read_rows(&self.spill_key)?;
        if start > rows.len() {
            return Err(ApiError::BadRequest(
                "comparison continuation is beyond the retained result".into(),
            ));
        }
        let end = start.saturating_add(limit).min(rows.len());
        let page = rows[start..end].to_vec();
        let next = (end < rows.len()).then(|| encode_page_token(&self.spill_key, id, end));
        Ok(ComparisonPage {
            comparison_id: id,
            status: entry.summary().status,
            rows: page,
            next,
        })
    }

    pub fn cancel(&self, session: SessionId, id: ComparisonId) -> ApiResult<ComparisonStatus> {
        let entry = self.get(session, id)?;
        entry.cancel.store(true, Ordering::Release);
        let mut summary = entry.summary.lock().unwrap();
        if summary.status == ComparisonStatus::Running {
            summary.status = ComparisonStatus::Canceled;
            summary.patch_eligible = false;
            summary.patch_refusal_reasons = vec!["comparison was canceled".into()];
        }
        Ok(summary.status)
    }

    fn reap(&self) {
        self.entries
            .retain(|_, entry| entry.created.elapsed() < RESULT_TTL);
    }
}

impl ComparisonEntry {
    pub fn summary(&self) -> ComparisonSummary {
        self.summary.lock().unwrap().clone()
    }

    pub fn canceled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }

    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    pub fn complete(&self, summary: ComparisonSummary, rows: Vec<RowDiff>) -> ApiResult<()> {
        if self.canceled() {
            return Ok(());
        }
        let encoded = serde_json::to_vec(&rows)
            .map_err(|error| ApiError::Internal(format!("encoding comparison rows: {error}")))?;
        if encoded.len() > MAX_COMPARISON_BYTES {
            return Err(ApiError::BadRequest(format!(
                "comparison diff exceeds the {MAX_COMPARISON_BYTES}-byte retention limit"
            )));
        }
        reserve_retained_bytes(&self.retained_bytes, encoded.len())?;
        let stored = if encoded.len() <= COMPARISON_MEMORY_BYTES {
            StoredRows::Memory {
                rows,
                bytes: encoded.len(),
                retained_bytes: self.retained_bytes.clone(),
            }
        } else {
            match self.spill_rows(&encoded, rows.len()) {
                Ok(stored) => stored,
                Err(error) => {
                    self.retained_bytes
                        .fetch_sub(encoded.len(), Ordering::AcqRel);
                    return Err(error);
                }
            }
        };
        *self.rows.write().unwrap() = Arc::new(stored);
        *self.summary.lock().unwrap() = summary;
        Ok(())
    }

    pub fn set_patch_context(&self, context: PatchContext) {
        *self.patch_context.lock().unwrap() = Some(context);
    }

    pub fn patch_context(&self) -> Option<PatchContext> {
        self.patch_context.lock().unwrap().clone()
    }

    pub fn fail(&self, code: impl Into<String>) {
        if self.canceled() {
            return;
        }
        let mut summary = self.summary.lock().unwrap();
        summary.status = ComparisonStatus::Failed;
        summary.failure_code = Some(code.into());
        summary.patch_eligible = false;
        summary.patch_refusal_reasons = vec!["comparison did not complete".into()];
    }

    fn spill_rows(&self, encoded: &[u8], len: usize) -> ApiResult<StoredRows> {
        let mut nonce = [0u8; 12];
        getrandom::getrandom(&mut nonce)
            .map_err(|error| ApiError::Internal(format!("comparison spill nonce: {error}")))?;
        let cipher = ChaCha20Poly1305::new((&self.spill_key).into());
        let ciphertext = cipher
            .encrypt(Nonce::from_slice(&nonce), encoded)
            .map_err(|_| ApiError::Internal("encrypting comparison spill failed".into()))?;
        let mut file = tempfile::NamedTempFile::new()
            .map_err(|error| ApiError::Internal(format!("creating comparison spill: {error}")))?;
        file.write_all(&ciphertext)
            .map_err(|error| ApiError::Internal(format!("writing comparison spill: {error}")))?;
        Ok(StoredRows::Spill {
            path: file.into_temp_path(),
            nonce,
            len,
            bytes: encoded.len(),
            retained_bytes: self.retained_bytes.clone(),
        })
    }

    fn read_rows(&self, spill_key: &[u8; 32]) -> ApiResult<Vec<RowDiff>> {
        let stored = self.rows.read().unwrap().clone();
        match &*stored {
            StoredRows::Empty => Ok(Vec::new()),
            StoredRows::Memory { rows, .. } => Ok(rows.clone()),
            StoredRows::Spill {
                path, nonce, len, ..
            } => {
                let ciphertext = std::fs::read(path).map_err(|error| {
                    ApiError::Internal(format!("reading comparison spill: {error}"))
                })?;
                let cipher = ChaCha20Poly1305::new(spill_key.into());
                let encoded = cipher
                    .decrypt(Nonce::from_slice(nonce), ciphertext.as_ref())
                    .map_err(|_| ApiError::Internal("decrypting comparison spill failed".into()))?;
                let rows: Vec<RowDiff> = serde_json::from_slice(&encoded).map_err(|error| {
                    ApiError::Internal(format!("decoding comparison spill: {error}"))
                })?;
                if rows.len() != *len {
                    return Err(ApiError::Internal(
                        "comparison spill row count is inconsistent".into(),
                    ));
                }
                Ok(rows)
            }
        }
    }
}

fn reserve_retained_bytes(retained: &AtomicUsize, bytes: usize) -> ApiResult<()> {
    retained
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            current
                .checked_add(bytes)
                .filter(|next| *next <= MAX_RETAINED_COMPARISON_BYTES)
        })
        .map(|_| ())
        .map_err(|_| {
            ApiError::BadRequest(
                "comparison retained-byte quota is exhausted; retry after older results expire"
                    .into(),
            )
        })
}

fn encode_page_token(secret: &[u8; 32], id: ComparisonId, offset: usize) -> String {
    let offset = u64::try_from(offset).unwrap_or(u64::MAX);
    let offset_bytes = offset.to_be_bytes();
    let proof = page_token_proof(secret, id, offset_bytes);
    let mut token = [0u8; 24];
    token[..8].copy_from_slice(&offset_bytes);
    token[8..].copy_from_slice(&proof);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(token)
}

fn page_token_proof(secret: &[u8; 32], id: ComparisonId, offset_bytes: [u8; 8]) -> [u8; 16] {
    let mut hasher = Sha256::new();
    hasher.update(secret);
    hasher.update(id.0.as_bytes());
    hasher.update(offset_bytes);
    hasher.update(b"sift-comparison-page-v2");
    let digest = hasher.finalize();
    let mut proof = [0u8; 16];
    proof.copy_from_slice(&digest[..16]);
    proof
}

fn decode_page_token(secret: &[u8; 32], id: ComparisonId, token: &str) -> ApiResult<usize> {
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| ApiError::BadRequest("invalid comparison continuation".into()))?;
    let bytes: [u8; 24] = decoded
        .try_into()
        .map_err(|_| ApiError::BadRequest("invalid comparison continuation".into()))?;
    let mut offset_bytes = [0u8; 8];
    offset_bytes.copy_from_slice(&bytes[..8]);
    let offset_u64 = u64::from_be_bytes(offset_bytes);
    let offset = usize::try_from(offset_u64)
        .map_err(|_| ApiError::BadRequest("invalid comparison continuation".into()))?;
    let expected = page_token_proof(secret, id, offset_bytes);
    let mismatch = expected
        .iter()
        .zip(&bytes[8..])
        .fold(0u8, |difference, (left, right)| difference | (left ^ right));
    if mismatch != 0 {
        return Err(ApiError::BadRequest(
            "invalid comparison continuation".into(),
        ));
    }
    Ok(offset)
}

pub struct RenderedFilter {
    pub sql: String,
    pub params: Vec<Value>,
}

pub fn render_filter(
    predicate: &ComparePredicate,
    columns: &std::collections::HashSet<String>,
    engine: Engine,
) -> ApiResult<RenderedFilter> {
    const MAX_NODES: usize = 64;
    const MAX_DEPTH: usize = 8;
    let mut params = Vec::new();
    let mut nodes = 0;
    let sql = render_predicate(
        predicate,
        columns,
        engine,
        &mut params,
        &mut nodes,
        0,
        MAX_NODES,
        MAX_DEPTH,
    )?;
    Ok(RenderedFilter { sql, params })
}

#[allow(clippy::too_many_arguments)]
fn render_predicate(
    predicate: &ComparePredicate,
    columns: &std::collections::HashSet<String>,
    engine: Engine,
    params: &mut Vec<Value>,
    nodes: &mut usize,
    depth: usize,
    max_nodes: usize,
    max_depth: usize,
) -> ApiResult<String> {
    *nodes += 1;
    if *nodes > max_nodes || depth > max_depth {
        return Err(ApiError::BadRequest(
            "comparison filter exceeds predicate limits".into(),
        ));
    }
    let column = |name: &str| -> ApiResult<String> {
        if !columns.contains(name) {
            return Err(ApiError::BadRequest(format!(
                "comparison filter references unknown column {name:?}"
            )));
        }
        Ok(crate::ddl::quote_ident(name, engine))
    };
    match predicate {
        ComparePredicate::Compare {
            column: name,
            operator,
            value,
        } => {
            if value.is_null() {
                return Err(ApiError::BadRequest(
                    "comparison filter uses is_null for NULL predicates".into(),
                ));
            }
            params.push(value.clone());
            let marker = match engine {
                Engine::Postgres => format!("${}", params.len()),
                Engine::SqlServer => format!("@P{}", params.len()),
            };
            let operator = match operator {
                ComparePredicateOperator::Eq => "=",
                ComparePredicateOperator::NotEq => "<>",
                ComparePredicateOperator::Less => "<",
                ComparePredicateOperator::LessOrEqual => "<=",
                ComparePredicateOperator::Greater => ">",
                ComparePredicateOperator::GreaterOrEqual => ">=",
            };
            Ok(format!("{} {operator} {marker}", column(name)?))
        }
        ComparePredicate::IsNull {
            column: name,
            negated,
        } => Ok(format!(
            "{} IS {}NULL",
            column(name)?,
            if *negated { "NOT " } else { "" }
        )),
        ComparePredicate::And { predicates } | ComparePredicate::Or { predicates } => {
            if predicates.is_empty() || predicates.len() > 32 {
                return Err(ApiError::BadRequest(
                    "comparison filter groups require between 1 and 32 predicates".into(),
                ));
            }
            let separator = if matches!(predicate, ComparePredicate::And { .. }) {
                " AND "
            } else {
                " OR "
            };
            predicates
                .iter()
                .map(|predicate| {
                    render_predicate(
                        predicate,
                        columns,
                        engine,
                        params,
                        nodes,
                        depth + 1,
                        max_nodes,
                        max_depth,
                    )
                    .map(|sql| format!("({sql})"))
                })
                .collect::<ApiResult<Vec<_>>>()
                .map(|parts| parts.join(separator))
        }
        ComparePredicate::Not { predicate } => render_predicate(
            predicate,
            columns,
            engine,
            params,
            nodes,
            depth + 1,
            max_nodes,
            max_depth,
        )
        .map(|sql| format!("NOT ({sql})")),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn filter_only_renders_catalog_columns_and_binds_values() {
        let columns = HashSet::from(["email".to_string()]);
        let rendered = render_filter(
            &ComparePredicate::Compare {
                column: "email".into(),
                operator: ComparePredicateOperator::Eq,
                value: Value::Text("private".into()),
            },
            &columns,
            Engine::Postgres,
        )
        .unwrap();
        assert_eq!(rendered.sql, "\"email\" = $1");
        assert_eq!(rendered.params, vec![Value::Text("private".into())]);
    }

    fn summary(id: ComparisonId) -> ComparisonSummary {
        ComparisonSummary {
            comparison_id: id,
            status: ComparisonStatus::Running,
            result_digest: String::new(),
            left_rows: 1,
            right_rows: 0,
            equal_rows: 0,
            changed_rows: 0,
            added_rows: 0,
            removed_rows: 1,
            incomparable_rows: 0,
            duplicate_key_groups: 0,
            retained_diff_rows: 1,
            columns: Vec::new(),
            key: sift_protocol::ResolvedCompareKey {
                columns: Vec::new(),
                inferred_constraint: None,
                row_ordinal: true,
            },
            tolerances: Vec::new(),
            patch_eligible: false,
            patch_refusal_reasons: vec!["fixture".into()],
            failure_code: None,
            expires_at: chrono::Utc::now() + chrono::Duration::minutes(10),
        }
    }

    #[test]
    fn large_comparison_pages_are_encrypted_at_rest_and_page_normally() {
        let registry = ComparisonRegistry::default();
        let id = ComparisonId(uuid::Uuid::new_v4());
        let session = SessionId(7);
        let entry = registry.create(session, summary(id));
        let secret = "private-comparison-value";
        let row = RowDiff {
            key: vec![Value::Text(format!(
                "{secret}{}",
                "x".repeat(COMPARISON_MEMORY_BYTES)
            ))],
            occurrence: 0,
            kind: sift_protocol::RowDiffKind::Removed,
            duplicate_key: false,
            cells: Vec::new(),
        };
        let mut completed = summary(id);
        completed.status = ComparisonStatus::Complete;
        entry.complete(completed, vec![row]).unwrap();

        let guard = entry.rows.read().unwrap();
        let StoredRows::Spill { path, .. } = &**guard else {
            panic!("large comparison should spill")
        };
        let ciphertext = std::fs::read(path).unwrap();
        assert!(!String::from_utf8_lossy(&ciphertext).contains(secret));
        drop(guard);

        let page = registry
            .page(
                session,
                id,
                ComparisonPageRequest {
                    after: None,
                    limit: Some(1),
                },
            )
            .unwrap();
        assert_eq!(page.rows.len(), 1);
        assert!(matches!(page.rows[0].key[0], Value::Text(_)));
    }

    #[test]
    fn continuation_tokens_are_scope_bound_and_tamper_evident() {
        let secret = [7u8; 32];
        let id = ComparisonId(uuid::Uuid::new_v4());
        let token = encode_page_token(&secret, id, 42);
        assert_eq!(decode_page_token(&secret, id, &token).unwrap(), 42);
        assert!(decode_page_token(&secret, ComparisonId(uuid::Uuid::new_v4()), &token).is_err());

        let mut bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&token)
            .unwrap();
        bytes[0] ^= 1;
        let tampered = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        assert!(decode_page_token(&secret, id, &tampered).is_err());
    }
}
