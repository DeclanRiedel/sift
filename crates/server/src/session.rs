//! Session + connection manager. The session store is the orchestrator
//! between HTTP handlers and drivers; it's the only thing that touches
//! `Arc<dyn Driver>` directly. A session is a logical workspace (ADR-002);
//! it holds zero or more open connections.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::Arc;
use std::sync::{Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use sift_driver_api::{
    BulkOp, ConnHandle, Driver, MssqlSavepoint, NotificationStream, PgSavepoint, ResultSetStream,
    TxHandle,
};
use sift_protocol::{
    AuditEntry, BeginTransactionRequest, BulkInsertFormat, BulkInsertRequest, BulkInsertResponse,
    Code, ColumnMetadata, ConnectionId, ConnectionInfo, ConnectionSpec, CursorId, DriverError,
    DriverWarning, EndTransactionRequest, Engine, ExecuteRequest, ExecuteRequestHttp,
    ExecuteResponse, ExportRequest, OpenSessionRequest, Operation, OperationAuditEntry,
    OperationStatus, Page, Row, SavepointInfo, SavepointRequest, SavepointState, SchemaScope,
    SchemaSnapshot, ServerInfo, SessionId, SessionInfo, TransactionEndAction, TransactionInfo,
    TransactionPreview, TransactionPreviewRequest, TransactionState, TxHandleRef, TxId,
};

use sift_metadata::{MetadataStore, NewOperationAudit, PrincipalId};

use crate::cursors::CursorRegistry;
use crate::error::{ApiError, ApiResult};
use crate::registry::DriverRegistry;
use crate::schema_cache::{CachedSchema, SchemaCache};

/// Fallback per-request timeout used until the server wires
/// `config.timeouts.request_secs` in via [`SessionStore::set_request_timeout`].
const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 30_000;
const MAX_DRIVER_TASKS: usize = 256;

/// In-memory ring caps for the request-audit and operation-replay logs.
const MAX_AUDIT_ROWS: usize = 10_000;
const MAX_OPERATION_ROWS: usize = 10_000;

/// Whether [`SessionStore::push_operation_inner`] should enqueue the
/// durable SQLite audit row (P1-meta-4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DurableAudit {
    /// Hand the row to the async audit-writer thread (the default path).
    Enqueue,
    /// The row was already written transactionally with the mutation;
    /// enqueuing again would duplicate it.
    AlreadyWritten,
}

/// Default synchronous-execute result caps until the server wires
/// `config.limits` in via [`SessionStore::set_result_limits`].
const DEFAULT_MAX_RESULT_ROWS: usize = 5_000;
const DEFAULT_MAX_RESULT_BYTES: usize = 8 * 1024 * 1024;

/// Server-owned session state. Clonable because handlers share it via
/// `Arc<SessionStore>` from axum state.
#[derive(Clone)]
pub struct SessionStore {
    inner: Arc<SessionStoreInner>,
}

struct SessionStoreInner {
    sessions: DashMap<SessionId, Session>,
    /// Legacy request audit ring (`/v1/audit`).
    audit: RingLog<AuditEntry>,
    /// Replayable operation ring (`/v1/operations`). The durable JSONL sink is
    /// a separate immutable field so a `list_operations` snapshot never
    /// contends with the writer.
    operations: RingLog<OperationAuditEntry>,
    /// Append-only JSONL sink for the operation ring. `None` when no operation
    /// log path is configured. Immutable after construction, so it lives
    /// outside the ring's lock.
    operation_writer: Option<OperationLogWriter>,
    next_id: AtomicU64,
    registry: DriverRegistry,
    /// Per-request driver deadline in milliseconds. `0` disables the bound.
    /// Stored as an atomic so the server can set it from config after the
    /// store is constructed and shared behind an `Arc`.
    request_timeout_ms: AtomicU64,
    /// Sender to the background durable-audit writer thread. `None` when
    /// metadata is disabled. The channel is bounded and sends are
    /// non-blocking (`try_send`), so the request path never waits on the
    /// SQLite write and a stalled writer cannot grow the queue without bound;
    /// every recorded operation still lands synchronously in the
    /// in-memory/JSONL log below.
    audit_tx: Mutex<Option<SyncSender<NewOperationAudit>>>,
    /// Durable policy source used by the dispatcher for managed connections.
    authorization_store: RwLock<Option<MetadataStore>>,
    /// Reverse index for immediate hard-revocation cleanup.
    managed_connections: DashMap<
        (
            PrincipalId,
            sift_metadata::TenantId,
            sift_metadata::ConnectionProfileId,
            SessionId,
            ConnectionId,
        ),
        (),
    >,
    resource_manager: RwLock<crate::resources::ResourceManager>,
    cursor_resource_guards: DashMap<
        CursorId,
        (
            crate::resources::ResourceGuard,
            crate::resources::ResourceGuard,
        ),
    >,
    /// Count of durable-audit rows dropped because the bounded channel above
    /// was full. Surfaced in the overflow log so the drop is never silent.
    audit_dropped: AtomicU64,
    /// Persist raw SQL in query history. When false, only a fingerprint is
    /// stored. The audit/replay trail is always fingerprinted regardless.
    store_sql: AtomicBool,
    /// Synchronous HTTP execute result caps (`config.limits`). Exceeding
    /// either returns `Code::ResultTooLarge`.
    max_result_rows: AtomicUsize,
    max_result_bytes: AtomicUsize,
    driver_tasks: AtomicUsize,
    /// Server-side cursor registry (ADR-011). Tracks every open cursor
    /// across all sessions; enforces per-session caps; routes eviction
    /// through `driver.cancel`.
    cursors: CursorRegistry,
    /// Per-spec schema cache with TTL + engine-specific invalidators.
    schema_cache: SchemaCache,
    /// Per-connection schema-search index (object + column names), built lazily
    /// and cached with a TTL (Phase D schema search). Keyed by connection since
    /// search scope is the active connection.
    search_indexes: DashMap<ConnectionId, (Arc<crate::search::SearchIndex>, Instant)>,
}

/// TTL for a cached per-connection search index before it is rebuilt.
const SEARCH_INDEX_TTL: Duration = Duration::from_secs(60);

/// An append-mostly in-memory ring with cheap snapshot reads. Appends are
/// O(1) amortized (`VecDeque` push-back + a single pop-front at the cap).
/// Reads clone the backing `Arc` under the lock and materialize the `Vec`
/// *outside* it, so a `list` — up to 10k entries — never blocks appends for
/// the length of the copy (P1-lock-1). `Arc::make_mut` copies once on the
/// first append after a snapshot was handed out; since reads are rare
/// (admin/debug endpoints) that copy is paid at most once per read.
struct RingLog<T> {
    entries: Mutex<Arc<VecDeque<T>>>,
    cap: usize,
}

impl<T: Clone> RingLog<T> {
    fn new(cap: usize) -> Self {
        Self {
            entries: Mutex::new(Arc::new(VecDeque::new())),
            cap,
        }
    }

    fn from_iter(cap: usize, items: impl IntoIterator<Item = T>) -> Self {
        let mut ring: VecDeque<T> = items.into_iter().collect();
        while ring.len() > cap {
            ring.pop_front();
        }
        Self {
            entries: Mutex::new(Arc::new(ring)),
            cap,
        }
    }

    fn push(&self, entry: T) {
        let mut guard = self.entries.lock().unwrap();
        let ring = Arc::make_mut(&mut guard);
        ring.push_back(entry);
        while ring.len() > self.cap {
            ring.pop_front();
        }
    }

    /// O(1) snapshot: clone the `Arc` under the lock, materialize outside it.
    fn to_vec(&self) -> Vec<T> {
        let snapshot = Arc::clone(&self.entries.lock().unwrap());
        snapshot.iter().cloned().collect()
    }
}

struct OperationLogWriter {
    tx: SyncSender<OperationAuditEntry>,
    _task: JoinHandle<()>,
}

struct DriverTaskPermit(Arc<SessionStoreInner>);

impl Drop for DriverTaskPermit {
    fn drop(&mut self) {
        self.0.driver_tasks.fetch_sub(1, Ordering::Release);
    }
}

impl SessionStore {
    pub fn new(registry: DriverRegistry) -> Self {
        let store = Self {
            inner: Arc::new(SessionStoreInner {
                sessions: DashMap::new(),
                audit: RingLog::new(MAX_AUDIT_ROWS),
                operations: RingLog::new(MAX_OPERATION_ROWS),
                operation_writer: None,
                next_id: AtomicU64::new(1),
                registry,
                request_timeout_ms: AtomicU64::new(DEFAULT_REQUEST_TIMEOUT_MS),
                audit_tx: Mutex::new(None),
                authorization_store: RwLock::new(None),
                managed_connections: DashMap::new(),
                resource_manager: RwLock::new(crate::resources::ResourceManager::default()),
                cursor_resource_guards: DashMap::new(),
                audit_dropped: AtomicU64::new(0),
                store_sql: AtomicBool::new(true),
                max_result_rows: AtomicUsize::new(DEFAULT_MAX_RESULT_ROWS),
                max_result_bytes: AtomicUsize::new(DEFAULT_MAX_RESULT_BYTES),
                driver_tasks: AtomicUsize::new(0),
                cursors: CursorRegistry::default(),
                schema_cache: SchemaCache::default(),
                search_indexes: DashMap::new(),
            }),
        };
        store.install_eviction_callback();
        store
    }

    pub fn new_with_operation_log_path(
        registry: DriverRegistry,
        path: impl AsRef<Path>,
    ) -> std::io::Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let entries = read_operation_log(path)?;
        let writer = OpenOptions::new().create(true).append(true).open(path)?;
        let writer = spawn_operation_log_writer(writer);
        let store = Self {
            inner: Arc::new(SessionStoreInner {
                sessions: DashMap::new(),
                audit: RingLog::new(MAX_AUDIT_ROWS),
                operations: RingLog::from_iter(MAX_OPERATION_ROWS, entries),
                operation_writer: Some(writer),
                next_id: AtomicU64::new(1),
                registry,
                request_timeout_ms: AtomicU64::new(DEFAULT_REQUEST_TIMEOUT_MS),
                audit_tx: Mutex::new(None),
                authorization_store: RwLock::new(None),
                managed_connections: DashMap::new(),
                resource_manager: RwLock::new(crate::resources::ResourceManager::default()),
                cursor_resource_guards: DashMap::new(),
                audit_dropped: AtomicU64::new(0),
                store_sql: AtomicBool::new(true),
                max_result_rows: AtomicUsize::new(DEFAULT_MAX_RESULT_ROWS),
                max_result_bytes: AtomicUsize::new(DEFAULT_MAX_RESULT_BYTES),
                driver_tasks: AtomicUsize::new(0),
                cursors: CursorRegistry::default(),
                schema_cache: SchemaCache::default(),
                search_indexes: DashMap::new(),
            }),
        };
        store.install_eviction_callback();
        Ok(store)
    }

    /// Wire the cursor registry's eviction hook back into this store so
    /// evicted cursors take the same driver.cancel path as an
    /// explicit user cancel. Called once, at construction.
    fn install_eviction_callback(&self) {
        let inner = Arc::downgrade(&self.inner);
        self.inner
            .cursors
            .set_on_evict(Arc::new(move |session, cursor| {
                let Some(inner) = inner.upgrade() else {
                    return;
                };
                // Best-effort: cancel via the driver on a background task so
                // the caller (which is inside `open`) doesn't await here.
                // Look up the connection that owns this cursor. In the
                // current data model cursors are keyed only by id — we scan
                // the session's connections and let driver.cancel run
                // against each; the driver-side ownership check filters
                // out non-owners cheaply.
                let store = SessionStore {
                    inner: Arc::clone(&inner),
                };
                tokio::spawn(async move {
                    let conn_ids: Vec<ConnectionId> = match store.inner.sessions.get(&session) {
                        Some(s) => s.connections.iter().map(|e| e.id).collect(),
                        None => return,
                    };
                    for conn in conn_ids {
                        let Ok(entry) = store.get_conn_entry(session, conn) else {
                            continue;
                        };
                        // Best-effort — an error means the cursor wasn't
                        // owned by this handle (driver returns CursorNotFound).
                        let _ = entry.driver.cancel(entry.handle, cursor).await;
                    }
                });
            }));
    }

    /// Access the cursor registry (for tests and future wiring).
    pub fn cursor_registry(&self) -> &CursorRegistry {
        &self.inner.cursors
    }

    /// Access the schema cache (for tests, metrics, and config wiring).
    pub fn schema_cache(&self) -> &SchemaCache {
        &self.inner.schema_cache
    }

    pub fn registry(&self) -> &DriverRegistry {
        &self.inner.registry
    }

    /// Set the per-request driver deadline. A zero duration disables the
    /// bound (driver calls run to completion). Called by the server at
    /// startup with `config.timeouts.request_secs`.
    pub fn set_request_timeout(&self, timeout: Duration) {
        let ms = timeout.as_millis().min(u64::MAX as u128) as u64;
        self.inner.request_timeout_ms.store(ms, Ordering::Relaxed);
    }

    fn request_timeout(&self) -> Duration {
        Duration::from_millis(self.inner.request_timeout_ms.load(Ordering::Relaxed))
    }

    /// Install the durable operation-audit sink. Spawns a dedicated writer
    /// thread that owns the metadata store and drains audit rows off the
    /// request path, so a slow disk never stalls an async worker. Called by
    /// the server at startup when a metadata store is configured.
    pub fn set_audit_store(&self, store: MetadataStore) {
        const AUDIT_QUEUE: usize = 1024;
        // The writer's INSERT runs on its own pooled connection (file-backed
        // stores check one out per call), so it never holds the request-path
        // connection (P1-meta-5, P1-meta-1). In-memory stores share the single
        // connection, which is fine for their low volume.
        let (tx, rx) = std::sync::mpsc::sync_channel::<NewOperationAudit>(AUDIT_QUEUE);
        std::thread::Builder::new()
            .name("sift-audit-writer".to_string())
            .spawn(move || {
                // Exits when the sender is dropped (SessionStore torn down).
                while let Ok(record) = rx.recv() {
                    if let Err(error) = store.record_operation_audit(record) {
                        tracing::warn!(%error, "durable operation audit write failed");
                    }
                }
            })
            .expect("spawn audit writer thread");
        *self.inner.audit_tx.lock().unwrap() = Some(tx);
    }

    pub fn set_authorization_store(&self, store: MetadataStore) {
        *self.inner.authorization_store.write().unwrap() = Some(store);
    }

    pub fn set_resource_manager(&self, manager: crate::resources::ResourceManager) {
        *self.inner.resource_manager.write().unwrap() = manager;
    }

    pub fn resource_manager(&self) -> crate::resources::ResourceManager {
        self.inner.resource_manager.read().unwrap().clone()
    }

    pub fn authorize_connection_operation(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        operation: sift_protocol::OperationKind,
        sql: Option<&str>,
        objects: &[&sift_protocol::ObjectPath],
    ) -> ApiResult<ConnectionEntryClone> {
        let entry = self.get_conn_entry(session_id, conn_id)?;
        let ConnectionProvenance::Managed {
            principal_id,
            tenant_id,
            profile_id,
            policy_revision,
            ..
        } = entry.provenance.clone()
        else {
            return Ok(entry);
        };
        if self.session_owner(session_id)? != Some(principal_id) {
            return Err(ApiError::Forbidden(
                "managed connection principal no longer owns the session".into(),
            ));
        }
        let metadata = self
            .inner
            .authorization_store
            .read()
            .unwrap()
            .clone()
            .ok_or(ApiError::MetadataUnavailable)?;
        let profile = metadata
            .get_connection_profile(tenant_id, profile_id)
            .map_err(|error| match error {
                sift_metadata::MetadataError::ConnectionProfileNotFound(_) => {
                    ApiError::Forbidden("connection profile is no longer available".into())
                }
                other => ApiError::Metadata(other),
            })?;
        let membership = metadata
            .list_principal_tenants(principal_id)?
            .into_iter()
            .find(|membership| membership.tenant.id == tenant_id)
            .ok_or_else(|| ApiError::Forbidden("tenant membership required".into()))?;
        let scope = crate::authorization::AuthorizationScope {
            authenticated: true,
            trusted_local: false,
            instance_admin: false,
            tenant_role: Some(sift_protocol::TenantRole::from(&membership.role)),
            room_role: None,
            connection_policy: Some(profile.policy.clone()),
        };
        crate::authorization::authorize(&scope, operation)
            .map_err(|denial| ApiError::Forbidden(denial.public_reason().into()))?;
        crate::sql_policy::enforce(
            &profile.policy,
            entry.driver.engine(),
            operation,
            sql,
            objects,
        )?;
        if profile.policy.revision != policy_revision {
            self.with_session(&session_id, |session| {
                if let Some(mut live) = session.connections.get_mut(&conn_id) {
                    if let ConnectionProvenance::Managed {
                        policy_revision, ..
                    } = &mut live.provenance
                    {
                        *policy_revision = profile.policy.revision;
                    }
                }
            })?;
        }
        Ok(entry)
    }

    fn current_connection_policy(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
    ) -> ApiResult<Option<sift_protocol::ConnectionPolicy>> {
        let entry = self.get_conn_entry(session_id, conn_id)?;
        let ConnectionProvenance::Managed {
            tenant_id,
            profile_id,
            ..
        } = entry.provenance
        else {
            return Ok(None);
        };
        let metadata = self
            .inner
            .authorization_store
            .read()
            .unwrap()
            .clone()
            .ok_or(ApiError::MetadataUnavailable)?;
        Ok(Some(
            metadata
                .get_connection_profile(tenant_id, profile_id)?
                .policy,
        ))
    }

    fn reserve_query_resources(
        &self,
        entry: &ConnectionEntryClone,
    ) -> ApiResult<
        Option<(
            crate::resources::ResourceGuard,
            crate::resources::ResourceGuard,
        )>,
    > {
        let ConnectionProvenance::Managed {
            tenant_id,
            quota_exempt,
            ..
        } = &entry.provenance
        else {
            return Ok(None);
        };
        if *quota_exempt {
            return Ok(None);
        }
        let tenant_id = *tenant_id;
        let manager = self.resource_manager();
        let query = manager.reserve(
            tenant_id,
            sift_protocol::TenantResource::ConcurrentQueries,
            1,
        )?;
        let cursor = manager.reserve(tenant_id, sift_protocol::TenantResource::Cursors, 1)?;
        Ok(Some((query, cursor)))
    }

    fn reserve_retained_bytes(
        &self,
        entry: &ConnectionEntryClone,
        bytes: u64,
    ) -> ApiResult<Option<crate::resources::ResourceGuard>> {
        let ConnectionProvenance::Managed {
            tenant_id,
            quota_exempt,
            ..
        } = &entry.provenance
        else {
            return Ok(None);
        };
        if *quota_exempt {
            return Ok(None);
        }
        self.resource_manager()
            .reserve(
                *tenant_id,
                sift_protocol::TenantResource::RetainedResultBytes,
                bytes,
            )
            .map(Some)
    }

    /// Whether raw SQL is persisted in query history (`metadata.store_sql`).
    pub fn set_store_sql(&self, store_sql: bool) {
        self.inner.store_sql.store(store_sql, Ordering::Relaxed);
    }

    pub fn store_sql(&self) -> bool {
        self.inner.store_sql.load(Ordering::Relaxed)
    }

    /// Set the synchronous-execute result caps from `config.limits`.
    pub fn set_result_limits(&self, max_rows: usize, max_bytes: usize) {
        self.inner
            .max_result_rows
            .store(max_rows, Ordering::Relaxed);
        self.inner
            .max_result_bytes
            .store(max_bytes, Ordering::Relaxed);
    }

    fn result_limits(&self) -> (usize, usize) {
        (
            self.inner.max_result_rows.load(Ordering::Relaxed),
            self.inner.max_result_bytes.load(Ordering::Relaxed),
        )
    }

    /// Run a driver future on its own task, bounded by the request timeout.
    /// Driver work never runs inline on the handler task: a wedged driver
    /// cannot freeze the request path, and on timeout we surface
    /// [`Code::QueryTimedOut`] rather than hanging. The spawned task is
    /// detached on timeout (not aborted) so the driver reaches a safe point
    /// on its own rather than being dropped mid-call.
    pub(crate) async fn run_bounded<F, T>(&self, op: &'static str, fut: F) -> ApiResult<T>
    where
        F: std::future::Future<Output = Result<T, DriverError>> + Send + 'static,
        T: Send + 'static,
    {
        let dur = self.request_timeout();
        if self
            .inner
            .driver_tasks
            .fetch_update(Ordering::Acquire, Ordering::Relaxed, |current| {
                (current < MAX_DRIVER_TASKS).then_some(current + 1)
            })
            .is_err()
        {
            return Err(ApiError::Driver(DriverError::new(
                Code::PoolExhausted,
                "driver task limit reached",
            )));
        }
        let permit = DriverTaskPermit(Arc::clone(&self.inner));
        let task = tokio::spawn(async move {
            let _permit = permit;
            fut.await
        });
        if dur.is_zero() {
            return match task.await {
                Ok(res) => res.map_err(ApiError::Driver),
                Err(join) => Err(ApiError::Internal(format!("{op} task failed: {join}"))),
            };
        }
        match tokio::time::timeout(dur, task).await {
            Ok(Ok(res)) => res.map_err(ApiError::Driver),
            Ok(Err(join)) => Err(ApiError::Internal(format!("{op} task failed: {join}"))),
            Err(_) => Err(timeout_error(op)),
        }
    }

    pub fn open_session(&self, req: OpenSessionRequest) -> SessionInfo {
        self.open_session_with_owner(req, None)
    }

    pub fn open_session_with_owner(
        &self,
        req: OpenSessionRequest,
        owner_principal_id: Option<PrincipalId>,
    ) -> SessionInfo {
        let id = SessionId(self.inner.next_id.fetch_add(1, Ordering::Relaxed));
        let now = chrono::Utc::now();
        let session = Session {
            id,
            created_at: now,
            tag: req.tag.clone(),
            owner_principal_id,
            connections: DashMap::new(),
            transactions: DashMap::new(),
            next_conn_id: AtomicU64::new(1),
            tenant_id: Mutex::new(None),
            resource_guard: Mutex::new(None),
        };
        let info = session.info();
        self.inner.sessions.insert(id, session);
        tracing::info!(session_id = %id, tag = ?req.tag, "session opened");
        info
    }

    pub fn list_sessions(&self) -> Vec<SessionInfo> {
        self.inner.sessions.iter().map(|s| s.info()).collect()
    }

    /// List only sessions owned by `owner`. `None` retains the metadata-free
    /// personal development behavior and returns only legacy unowned sessions.
    pub fn list_sessions_for_owner(&self, owner: Option<PrincipalId>) -> Vec<SessionInfo> {
        self.inner
            .sessions
            .iter()
            .filter(|session| session.owner_principal_id == owner)
            .map(|session| session.info())
            .collect()
    }

    pub fn push_audit(&self, entry: AuditEntry) {
        self.inner.audit.push(entry);
    }

    pub fn list_audit(&self) -> Vec<AuditEntry> {
        self.inner.audit.to_vec()
    }

    /// Record an operation with only its status known (actor, row count, and
    /// failure details unavailable at the call site). Prefer
    /// [`SessionStore::push_operation_full`] where those are known.
    pub fn push_operation(&self, operation: Operation, status: OperationStatus) {
        self.push_operation_full(operation, status, None, None, None, None);
    }

    /// The single choke point for operation audit. Records the operation in
    /// the in-memory/JSONL replay log **and** — when a metadata store is
    /// configured — a sanitized durable audit row (actor, target, result
    /// code, row count, failure message; never SQL text or bind values).
    /// Success and failure paths both call this, so a new operation cannot be
    /// added without an audit trail.
    pub fn push_operation_full(
        &self,
        operation: Operation,
        status: OperationStatus,
        actor_principal_id: Option<i64>,
        result_code: Option<String>,
        row_count: Option<i64>,
        error_message: Option<String>,
    ) {
        self.push_operation_inner(
            operation,
            status,
            actor_principal_id,
            result_code,
            row_count,
            error_message,
            DurableAudit::Enqueue,
        );
    }

    /// Like [`SessionStore::push_operation_full`], but does **not** enqueue
    /// the durable SQLite audit row — the caller has already written it
    /// transactionally alongside the mutation (P1-meta-4), so enqueuing here
    /// would double-write it. Still records the in-memory ring and JSONL
    /// replay log. Use only when the metadata method wrote the audit row in
    /// the same tx as the mutation.
    pub fn push_operation_local(
        &self,
        operation: Operation,
        status: OperationStatus,
        actor_principal_id: Option<i64>,
        result_code: Option<String>,
        row_count: Option<i64>,
        error_message: Option<String>,
    ) {
        self.push_operation_inner(
            operation,
            status,
            actor_principal_id,
            result_code,
            row_count,
            error_message,
            DurableAudit::AlreadyWritten,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn push_operation_inner(
        &self,
        operation: Operation,
        status: OperationStatus,
        actor_principal_id: Option<i64>,
        result_code: Option<String>,
        row_count: Option<i64>,
        error_message: Option<String>,
        durable: DurableAudit,
    ) {
        // Sanitize before the operation is stored anywhere (in-memory ring,
        // JSONL log, or durable audit): SQL is reduced to a fingerprint and
        // secrets/bind values are stripped, so no audit surface carries them.
        let operation = sanitize_operation(operation);
        let summary = operation.audit_summary();
        let entry = OperationAuditEntry {
            at: chrono::Utc::now(),
            operation,
            status,
        };
        self.inner.operations.push(entry.clone());

        if let Some(writer) = &self.inner.operation_writer {
            match writer.tx.try_send(entry) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    tracing::error!("operation audit writer queue is full; dropping JSONL row");
                }
                Err(TrySendError::Disconnected(_)) => {
                    tracing::error!("operation audit writer is stopped; dropping JSONL row");
                }
            }
        }

        if durable == DurableAudit::AlreadyWritten {
            // The durable audit row was committed in the same tx as the
            // mutation; enqueuing it again would duplicate it.
            return;
        }

        if let Some(tx) = self.inner.audit_tx.lock().unwrap().as_ref() {
            let record = NewOperationAudit {
                actor_principal_id: actor_principal_id.map(PrincipalId),
                action: summary.action,
                target: summary.target,
                target_id: summary.target_id,
                status: match status {
                    OperationStatus::Succeeded => "succeeded".to_string(),
                    OperationStatus::Failed => "failed".to_string(),
                },
                result_code,
                row_count,
                error_message,
                correlation_id: crate::correlation::current(),
            };
            // Hand off to the writer thread without blocking the request path.
            // On a full queue we drop and count rather than wait on the SQLite
            // write; a disconnected writer was already logged at spawn.
            match tx.try_send(record) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    let dropped = self.inner.audit_dropped.fetch_add(1, Ordering::Relaxed) + 1;
                    tracing::error!(dropped, "durable audit queue full; dropping row");
                }
                Err(TrySendError::Disconnected(_)) => {
                    tracing::error!("durable audit writer is stopped; dropping row");
                }
            }
        }
    }

    pub fn list_operations(&self) -> Vec<OperationAuditEntry> {
        self.inner.operations.to_vec()
    }

    pub fn close_session(&self, id: SessionId) -> ApiResult<()> {
        let (_, session) = self
            .inner
            .sessions
            .remove(&id)
            .ok_or(ApiError::SessionNotFound(id))?;
        // Drop connections. We spawn closes concurrently to not block the
        // handler on N sequential round-trips.
        for entry in session.connections.iter() {
            let driver = entry.driver.clone();
            let handle = entry.handle.clone();
            let connection_id = entry.id;
            if let ConnectionProvenance::Managed {
                principal_id,
                tenant_id,
                profile_id,
                ..
            } = entry.provenance.clone()
            {
                self.inner.managed_connections.remove(&(
                    principal_id,
                    tenant_id,
                    profile_id,
                    id,
                    connection_id,
                ));
            }
            let cursors = self.inner.cursors.clone();
            let sessions = self.clone();
            tokio::spawn(async move {
                for cursor in cursors.connection_cursors(id, connection_id) {
                    let _ = driver.cancel(handle.clone(), cursor).await;
                    sessions.cursor_remove(cursor);
                }
                if let Err(e) = driver.close(handle).await {
                    tracing::warn!(error = %e, "error closing conn during session close");
                }
            });
        }
        tracing::info!(session_id = %id, "session closed");
        Ok(())
    }

    pub fn session_info(&self, id: SessionId) -> ApiResult<SessionInfo> {
        let session = self
            .inner
            .sessions
            .get(&id)
            .ok_or(ApiError::SessionNotFound(id))?;
        Ok(session.info())
    }

    pub fn session_owner(&self, id: SessionId) -> ApiResult<Option<PrincipalId>> {
        let session = self
            .inner
            .sessions
            .get(&id)
            .ok_or(ApiError::SessionNotFound(id))?;
        Ok(session.owner_principal_id)
    }

    pub fn managed_tenant_for_session(&self, id: SessionId) -> Option<sift_metadata::TenantId> {
        let session = self.inner.sessions.get(&id)?;
        let tenant = *session.tenant_id.lock().unwrap();
        tenant
    }

    pub async fn open_connection(
        &self,
        session_id: SessionId,
        engine: Engine,
        spec: ConnectionSpec,
    ) -> ApiResult<ConnectionInfo> {
        self.open_connection_with_provenance(
            session_id,
            engine,
            spec,
            ConnectionProvenance::TrustedLocal,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn open_managed_connection(
        &self,
        session_id: SessionId,
        engine: Engine,
        spec: ConnectionSpec,
        principal_id: PrincipalId,
        tenant_id: sift_metadata::TenantId,
        profile_id: sift_metadata::ConnectionProfileId,
        policy_revision: u64,
        trusted_local: bool,
    ) -> ApiResult<ConnectionInfo> {
        if self.session_owner(session_id)? != Some(principal_id) {
            return Err(ApiError::Forbidden(
                "managed connection principal must own the session".into(),
            ));
        }
        let manager = self.resource_manager();
        let enforce_limits = manager.enforces_for(trusted_local);
        self.bind_session_tenant(session_id, tenant_id, enforce_limits)?;
        let connection_guard = if enforce_limits {
            Some(manager.reserve(tenant_id, sift_protocol::TenantResource::Connections, 1)?)
        } else {
            None
        };
        self.open_connection_with_provenance(
            session_id,
            engine,
            spec,
            ConnectionProvenance::Managed {
                principal_id,
                tenant_id,
                profile_id,
                policy_revision,
                quota_exempt: !enforce_limits,
            },
            connection_guard,
        )
        .await
    }

    async fn open_connection_with_provenance(
        &self,
        session_id: SessionId,
        engine: Engine,
        spec: ConnectionSpec,
        provenance: ConnectionProvenance,
        resource_guard: Option<crate::resources::ResourceGuard>,
    ) -> ApiResult<ConnectionInfo> {
        if !self.inner.sessions.contains_key(&session_id) {
            return Err(ApiError::SessionNotFound(session_id));
        }

        let managed_identity = match &provenance {
            ConnectionProvenance::Managed {
                principal_id,
                tenant_id,
                profile_id,
                ..
            } => Some((*principal_id, *tenant_id, *profile_id)),
            ConnectionProvenance::TrustedLocal => None,
        };
        let driver = self.inner.registry.get(engine)?;
        let handle = driver.open(&spec).await?;
        let info = {
            let Some(session) = self.inner.sessions.get(&session_id) else {
                driver.close(handle).await?;
                return Err(ApiError::SessionNotFound(session_id));
            };
            let id = ConnectionId(session.next_conn_id.fetch_add(1, Ordering::Relaxed));
            let display_name = display_name_for(&spec);
            let info = ConnectionInfo {
                id,
                engine,
                display_name,
                created_at: chrono::Utc::now(),
            };
            session.connections.insert(
                id,
                ConnectionEntry {
                    id,
                    engine,
                    handle: handle.clone(),
                    driver: driver.clone(),
                    info: info.clone(),
                    spec,
                    provenance,
                    _resource_guard: resource_guard,
                },
            );
            info
        };
        if let Some((principal_id, tenant_id, profile_id)) = managed_identity {
            self.inner.managed_connections.insert(
                (principal_id, tenant_id, profile_id, session_id, info.id),
                (),
            );
        }
        tracing::info!(session_id = %session_id, conn_id = %info.id, %engine, "connection opened");
        Ok(info)
    }

    fn bind_session_tenant(
        &self,
        session_id: SessionId,
        tenant_id: sift_metadata::TenantId,
        enforce_limits: bool,
    ) -> ApiResult<()> {
        let session = self
            .inner
            .sessions
            .get(&session_id)
            .ok_or(ApiError::SessionNotFound(session_id))?;
        let mut bound = session.tenant_id.lock().unwrap();
        if let Some(current) = *bound {
            if current != tenant_id {
                return Err(ApiError::Forbidden(
                    "a managed session cannot span tenants".into(),
                ));
            }
            return Ok(());
        }
        if enforce_limits {
            let guard = self.resource_manager().reserve(
                tenant_id,
                sift_protocol::TenantResource::Sessions,
                1,
            )?;
            *session.resource_guard.lock().unwrap() = Some(guard);
        }
        *bound = Some(tenant_id);
        Ok(())
    }

    pub async fn close_connection(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
    ) -> ApiResult<()> {
        self.authorize_connection_operation(
            session_id,
            conn_id,
            sift_protocol::OperationKind::CloseConnection,
            None,
            &[],
        )?;
        self.close_connection_unchecked(session_id, conn_id).await
    }

    async fn close_connection_unchecked(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
    ) -> ApiResult<()> {
        let (txs, entry) = self
            .with_session(&session_id, |s| {
                let txs = drain_connection_transactions(s, conn_id);
                s.connections
                    .remove(&conn_id)
                    .map(|(_, entry)| (txs, entry))
            })?
            .ok_or(ApiError::ConnectionNotFound(conn_id))?;
        for cursor in self.inner.cursors.connection_cursors(session_id, conn_id) {
            if let Err(error) = entry.driver.cancel(entry.handle.clone(), cursor).await {
                tracing::debug!(%error, %cursor, "cursor cancel during connection close failed");
            }
            self.inner.cursors.remove(cursor);
            self.inner.cursor_resource_guards.remove(&cursor);
        }
        for tx in txs {
            if let Err(error) = entry.driver.rollback(tx.handle).await {
                tracing::warn!(session_id = %session_id, conn_id = %conn_id, error = %error, "rollback during connection close failed");
            }
        }
        let close_result = entry.driver.close(entry.handle).await;
        if let ConnectionProvenance::Managed {
            principal_id,
            tenant_id,
            profile_id,
            ..
        } = entry.provenance
        {
            self.inner.managed_connections.remove(&(
                principal_id,
                tenant_id,
                profile_id,
                session_id,
                conn_id,
            ));
        }
        close_result?;
        tracing::info!(session_id = %session_id, conn_id = %conn_id, "connection closed");
        Ok(())
    }

    pub async fn disconnect_managed_profile(
        &self,
        tenant_id: sift_metadata::TenantId,
        profile_id: sift_metadata::ConnectionProfileId,
    ) -> usize {
        let targets: Vec<_> = self
            .inner
            .managed_connections
            .iter()
            .filter_map(|entry| {
                let (_, tenant, profile, session, connection) = *entry.key();
                (tenant == tenant_id && profile == profile_id).then_some((session, connection))
            })
            .collect();
        let mut disconnected = 0;
        for (session, connection) in targets {
            match self.close_connection_unchecked(session, connection).await {
                Ok(()) => disconnected += 1,
                Err(ApiError::ConnectionNotFound(_)) | Err(ApiError::SessionNotFound(_)) => {}
                Err(error) => {
                    tracing::warn!(%error, %session, %connection, "hard revocation cleanup failed")
                }
            }
        }
        disconnected
    }

    pub async fn disconnect_managed_principal(&self, principal_id: PrincipalId) -> usize {
        let targets: Vec<_> = self
            .inner
            .managed_connections
            .iter()
            .filter_map(|entry| {
                let (principal, _, _, session, connection) = *entry.key();
                (principal == principal_id).then_some((session, connection))
            })
            .collect();
        let mut disconnected = 0;
        for (session, connection) in targets {
            if self
                .close_connection_unchecked(session, connection)
                .await
                .is_ok()
            {
                disconnected += 1;
            }
        }
        disconnected
    }

    pub async fn disconnect_managed_profile_principal(
        &self,
        profile_id: sift_metadata::ConnectionProfileId,
        principal_id: PrincipalId,
    ) -> usize {
        let targets: Vec<_> = self
            .inner
            .managed_connections
            .iter()
            .filter_map(|entry| {
                let (principal, _, profile, session, connection) = *entry.key();
                (principal == principal_id && profile == profile_id)
                    .then_some((session, connection))
            })
            .collect();
        let mut disconnected = 0;
        for (session, connection) in targets {
            if self
                .close_connection_unchecked(session, connection)
                .await
                .is_ok()
            {
                disconnected += 1;
            }
        }
        disconnected
    }

    pub fn list_connections(&self, session_id: SessionId) -> ApiResult<Vec<ConnectionInfo>> {
        let session = self
            .inner
            .sessions
            .get(&session_id)
            .ok_or(ApiError::SessionNotFound(session_id))?;
        Ok(session.connections.iter().map(|e| e.info.clone()).collect())
    }

    pub async fn ping(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
    ) -> ApiResult<ServerInfo> {
        let entry = self.authorize_connection_operation(
            session_id,
            conn_id,
            sift_protocol::OperationKind::PingConnection,
            None,
            &[],
        )?;
        let driver = entry.driver.clone();
        let handle = entry.handle.clone();
        let first = {
            let driver = driver.clone();
            self.run_bounded("ping", async move { driver.ping(handle).await })
                .await
        };
        match first {
            Err(ApiError::Driver(error)) if is_reconnectable(&error) => {
                // ping is idempotent: re-establish the connection and try once.
                let handle = self.reconnect(session_id, conn_id).await?;
                self.run_bounded("ping", async move { driver.ping(handle).await })
                    .await
            }
            other => other,
        }
    }

    pub async fn schema(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        scope: SchemaScope,
    ) -> ApiResult<SchemaSnapshot> {
        let objects: Vec<_> = match &scope.depth {
            sift_protocol::SchemaDepth::Shallow => Vec::new(),
            sift_protocol::SchemaDepth::Deep { object } => vec![object],
        };
        self.authorize_connection_operation(
            session_id,
            conn_id,
            sift_protocol::OperationKind::RefreshSchema,
            None,
            &objects,
        )?;
        let cached = self.schema_cached(session_id, conn_id, scope).await?;
        Ok((*cached.snapshot).clone())
    }

    pub async fn schema_cached(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        scope: SchemaScope,
    ) -> ApiResult<CachedSchema> {
        let cached = self
            .schema_cached_unfiltered(session_id, conn_id, scope)
            .await?;
        let Some(policy) = self.current_connection_policy(session_id, conn_id)? else {
            return Ok(cached);
        };
        if policy.allowed_schemas.is_none() {
            return Ok(cached);
        }
        let mut snapshot = (*cached.snapshot).clone();
        crate::sql_policy::filter_snapshot(&policy, &mut snapshot);
        Ok(CachedSchema::new_uncached(snapshot))
    }

    async fn schema_cached_unfiltered(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        scope: SchemaScope,
    ) -> ApiResult<CachedSchema> {
        let entry = self.get_conn_entry(session_id, conn_id)?;
        let spec = self.spec_for_conn(session_id, conn_id)?;
        // Cache lookup: return immediately if a fresh snapshot exists
        // for this (spec, scope).
        if let Some(cached) = self.inner.schema_cache.get_cached(&spec, &scope) {
            return Ok(cached);
        }
        let fetch_gate = self.inner.schema_cache.fetch_gate(&spec, &scope).ok();
        let _fetch_guard = match fetch_gate.as_ref() {
            Some(gate) => Some(gate.lock().await),
            None => None,
        };
        if let Some(cached) = self.inner.schema_cache.get_cached(&spec, &scope) {
            return Ok(cached);
        }
        let driver = entry.driver.clone();
        let handle = entry.handle.clone();
        let first = {
            let driver = driver.clone();
            let scope = scope.clone();
            self.run_bounded("schema", async move { driver.schema(handle, scope).await })
                .await
        };
        let driver_for_retry = driver.clone();
        let result = match first {
            Err(ApiError::Driver(error)) if is_reconnectable(&error) => {
                // Schema introspection is idempotent: reconnect and retry once.
                let handle = self.reconnect(session_id, conn_id).await?;
                let scope = scope.clone();
                self.run_bounded("schema", async move {
                    driver_for_retry.schema(handle, scope).await
                })
                .await
            }
            other => other,
        };
        if let Ok(snapshot) = &result {
            if let Some(cached) =
                self.inner
                    .schema_cache
                    .insert(&spec, &scope, snapshot.clone(), driver)
            {
                if let Some(gate) = &fetch_gate {
                    self.inner.schema_cache.clear_fetch_gate(gate);
                }
                return Ok(cached);
            }
        }
        if let Some(gate) = &fetch_gate {
            self.inner.schema_cache.clear_fetch_gate(gate);
        }
        result.map(CachedSchema::new_uncached)
    }

    fn spec_for_conn(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
    ) -> ApiResult<ConnectionSpec> {
        let session = self
            .inner
            .sessions
            .get(&session_id)
            .ok_or(ApiError::SessionNotFound(session_id))?;
        let entry = session
            .connections
            .get(&conn_id)
            .ok_or(ApiError::ConnectionNotFound(conn_id))?;
        Ok(entry.spec.clone())
    }

    /// Re-establish a broken connection in place: open a fresh backend session
    /// from the stored spec, swap it into the connection entry so later
    /// operations use it, and close the dead handle best-effort. Bounded by
    /// the request timeout. Only invoked for idempotent operations after a
    /// reconnectable failure (see [`is_reconnectable`]).
    async fn reconnect(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
    ) -> ApiResult<ConnHandle> {
        let (driver, spec, old_handle) = {
            let session = self
                .inner
                .sessions
                .get(&session_id)
                .ok_or(ApiError::SessionNotFound(session_id))?;
            let entry = session
                .connections
                .get(&conn_id)
                .ok_or(ApiError::ConnectionNotFound(conn_id))?;
            (
                entry.driver.clone(),
                entry.spec.clone(),
                entry.handle.clone(),
            )
        };
        let opener = driver.clone();
        let new_handle = self
            .run_bounded("reconnect", async move { opener.open(&spec).await })
            .await?;
        self.with_session(&session_id, |s| {
            if let Some(mut entry) = s.connections.get_mut(&conn_id) {
                entry.handle = new_handle.clone();
            }
        })?;
        // The old backend session is gone; close it best-effort off the
        // request path.
        tokio::spawn(async move {
            let _ = driver.close(old_handle).await;
        });
        tracing::info!(
            session_id = %session_id,
            conn_id = %conn_id,
            "re-established broken connection"
        );
        Ok(new_handle)
    }

    /// Synchronous execute: drains the entire page stream into the response.
    /// Suitable for small/medium results; the WS streaming surface handles
    /// large results.
    pub async fn execute_http(
        &self,
        session_id: SessionId,
        req: ExecuteRequestHttp,
    ) -> ApiResult<ExecuteResponse> {
        self.execute_http_as(session_id, req, sift_protocol::OperationKind::ExecuteQuery)
            .await
    }

    pub async fn execute_http_as(
        &self,
        session_id: SessionId,
        req: ExecuteRequestHttp,
        operation: sift_protocol::OperationKind,
    ) -> ApiResult<ExecuteResponse> {
        let conn_id = req.connection;
        self.validate_execute_tx(session_id, conn_id, req.tx.as_ref())?;
        let entry = self.authorize_connection_operation(
            session_id,
            conn_id,
            operation,
            Some(&req.sql),
            &[],
        )?;
        let resource_guards = self.reserve_query_resources(&entry)?;
        let retained_bytes = self.reserve_retained_bytes(&entry, self.result_limits().1 as u64)?;
        let exec = ExecuteRequest {
            sql: req.sql,
            params: req.params,
        };
        let driver = entry.driver.clone();
        let handle = entry.handle.clone();
        let dur = self.request_timeout();
        let (max_rows, max_bytes) = self.result_limits();

        // The driver's execute + full drain runs on its own task. The cursor
        // id is only known once `execute` returns, so we stash it in a shared
        // slot the moment it is available; on timeout that lets us cancel the
        // in-flight cursor (which also drives SQL Server's discard-on-cancel).
        let cursor_slot: Arc<Mutex<Option<CursorId>>> = Arc::new(Mutex::new(None));
        let slot = cursor_slot.clone();
        let cursors = self.inner.cursors.clone();
        let mut task = tokio::spawn(async move {
            let _resource_guards = resource_guards;
            let _retained_bytes = retained_bytes;
            let stream = driver.execute(handle, exec).await?;
            let cursor_id = stream.cursor_id;
            *slot.lock().unwrap() = Some(cursor_id);
            // Hand the driver stream to the registry pump. Eviction of
            // a co-tenant cursor happens via the on_evict callback.
            let wrapped = cursors.wrap_for_connection(session_id, conn_id, stream)?;
            let result = drain_stream(wrapped, max_rows, max_bytes).await;
            cursors.remove(cursor_id);
            result
        });

        if dur.is_zero() {
            return match (&mut task).await {
                Ok(res) => res.map_err(ApiError::Driver),
                Err(join) => Err(ApiError::Internal(format!("execute task failed: {join}"))),
            };
        }

        match tokio::time::timeout(dur, &mut task).await {
            Ok(Ok(res)) => res.map_err(ApiError::Driver),
            Ok(Err(join)) => Err(ApiError::Internal(format!("execute task failed: {join}"))),
            Err(_) => {
                let cursor = *cursor_slot.lock().unwrap();
                if let Some(cursor) = cursor {
                    // Cursor exists: driver returned the stream and the task
                    // is draining rows. Cancel through the cursor so the
                    // driver's abort+discard rules run.
                    self.cancel_after_timeout(session_id, conn_id, cursor).await;
                } else {
                    // Task is hung inside driver.execute before any cursor
                    // was produced. There is nothing to cancel through the
                    // driver; abort the task itself so it doesn't outlive
                    // the handler and hold the ConnHandle busy indefinitely
                    // (which would also block Shutdown::await_drain).
                    task.abort();
                    tracing::warn!(
                        session_id = %session_id,
                        conn_id = %conn_id,
                        "aborted execute task after pre-cursor timeout"
                    );
                }
                Err(timeout_error("execute"))
            }
        }
    }

    /// Best-effort cancel of a cursor whose HTTP execute exceeded the request
    /// timeout. Reuses [`SessionStore::cancel`] so SQL Server's
    /// discard-on-cancel rule (drop the connection after aborting) still
    /// holds. Bounded so a wedged cancel cannot itself hang the handler.
    async fn cancel_after_timeout(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        cursor: CursorId,
    ) {
        let dur = self.request_timeout();
        // Safety cleanup is not a user-requested operation and must remain
        // available even when the profile blocks explicit cancellation.
        let cancel = self.cancel_unchecked(session_id, conn_id, cursor);
        let result = if dur.is_zero() {
            cancel.await
        } else {
            match tokio::time::timeout(dur, cancel).await {
                Ok(res) => res,
                Err(_) => {
                    tracing::warn!(
                        session_id = %session_id,
                        conn_id = %conn_id,
                        "cancel after query timeout itself timed out"
                    );
                    return;
                }
            }
        };
        match result {
            Ok(()) => tracing::info!(
                session_id = %session_id,
                conn_id = %conn_id,
                cursor = %cursor,
                "canceled query after request timeout"
            ),
            Err(error) => tracing::warn!(
                session_id = %session_id,
                conn_id = %conn_id,
                error = %error,
                "cancel after query timeout failed"
            ),
        }
    }

    pub async fn execute_stream(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        req: ExecuteRequest,
        tx: Option<&TxHandleRef>,
    ) -> ApiResult<ResultSetStream> {
        self.execute_stream_as(
            session_id,
            conn_id,
            req,
            tx,
            sift_protocol::OperationKind::ExecuteQuery,
        )
        .await
    }

    pub async fn execute_stream_as(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        req: ExecuteRequest,
        tx: Option<&TxHandleRef>,
        operation: sift_protocol::OperationKind,
    ) -> ApiResult<ResultSetStream> {
        self.validate_execute_tx(session_id, conn_id, tx)?;
        let entry = self.authorize_connection_operation(
            session_id,
            conn_id,
            operation,
            Some(&req.sql),
            &[],
        )?;
        let resource_guards = self.reserve_query_resources(&entry)?;
        let stream = entry.driver.execute(entry.handle.clone(), req).await?;
        let cursor_id = stream.cursor_id;
        // Hand the driver's stream to the registry-owned pump. Wrapping
        // enforces the per-session cap (evicting the LRA cursor of the
        // same session via the installed on_evict callback), spawns
        // the pump task, and returns a rebound stream whose `rows`
        // channel is fed by the pump.
        match self
            .inner
            .cursors
            .wrap_for_connection(session_id, conn_id, stream)
        {
            Ok(wrapped) => {
                if let Some(resource_guards) = resource_guards {
                    self.inner
                        .cursor_resource_guards
                        .insert(cursor_id, resource_guards);
                }
                Ok(wrapped)
            }
            Err(error) => {
                // Wrap failed (cap misconfig or duplicate id). Drop the
                // raw driver cursor we can't rely on the registry to
                // clean up — Drop on the raw stream isn't enough for
                // server-side cursors.
                //
                // Note: on the happy failure path (cap==0), the driver
                // stream is consumed by wrap()'s destructuring before
                // returning Err, so there is nothing to cancel here.
                Err(ApiError::Driver(error))
            }
        }
    }

    /// Run an export query and return the encoded byte stream. Unlike the
    /// old path (which called `driver.execute` directly), this routes
    /// through [`SessionStore::execute_stream`], so the export honors the
    /// per-session cursor cap and runs under the registry pump — a client
    /// can no longer spam exports to bypass the cap and exhaust DB
    /// connections, and a client disconnect cancels the query through the
    /// pump. A drop-guard releases the cursor from the registry when the
    /// download completes or the consumer is dropped. The initial execute
    /// is bounded by the request timeout so a wedged `driver.execute`
    /// cannot hang the handler forever.
    pub async fn export_stream(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        req: ExportRequest,
    ) -> ApiResult<impl futures::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send + 'static>
    {
        let exec = ExecuteRequest {
            sql: req.sql,
            params: req.params,
        };
        let dur = self.request_timeout();
        let fut = self.execute_stream_as(
            session_id,
            conn_id,
            exec,
            None,
            sift_protocol::OperationKind::ExportQuery,
        );
        let wrapped = if dur.is_zero() {
            fut.await?
        } else {
            tokio::time::timeout(dur, fut)
                .await
                .map_err(|_| timeout_error("export"))??
        };
        let guard = CursorGuard {
            sessions: self.clone(),
            cursor_id: wrapped.cursor_id,
        };
        Ok(crate::export::encode_stream(
            wrapped.rows,
            req.format,
            req.header,
            req.null_display.unwrap_or_default(),
            guard,
        ))
    }

    /// Called by the WS ack loop after each ack to keep the cursor from
    /// looking idle to the eviction policy.
    pub fn cursor_touch(&self, cursor_id: CursorId) {
        self.inner.cursors.touch(cursor_id);
    }

    /// Called after a cursor terminates or is cancelled to drop its
    /// registry bookkeeping. Idempotent.
    pub fn cursor_remove(&self, cursor_id: CursorId) {
        self.inner.cursors.remove(cursor_id);
        self.inner.cursor_resource_guards.remove(&cursor_id);
    }

    pub async fn listen_pg(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        channels: Vec<String>,
    ) -> ApiResult<NotificationStream> {
        let entry = self.authorize_connection_operation(
            session_id,
            conn_id,
            sift_protocol::OperationKind::Listen,
            None,
            &[],
        )?;
        let pg = entry.driver.as_pg().ok_or_else(|| {
            ApiError::Driver(
                DriverError::new(
                    sift_protocol::Code::UnsupportedForEngine,
                    "LISTEN/NOTIFY is only supported by Postgres connections",
                )
                .with_engine(entry.driver.engine()),
            )
        })?;
        Ok(pg.listen(entry.handle.clone(), channels).await?)
    }

    pub async fn cancel(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        cursor: CursorId,
    ) -> ApiResult<()> {
        self.authorize_connection_operation(
            session_id,
            conn_id,
            sift_protocol::OperationKind::CancelQuery,
            None,
            &[],
        )?;
        self.cancel_unchecked(session_id, conn_id, cursor).await
    }

    async fn cancel_unchecked(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        cursor: CursorId,
    ) -> ApiResult<()> {
        let entry = self.get_conn_entry(session_id, conn_id)?;
        entry.driver.cancel(entry.handle.clone(), cursor).await?;
        // Drop the registry entry so the per-session cap slot frees
        // up. Terminal-page cleanup calls `cursor_remove` on the same
        // path; this is idempotent.
        self.inner.cursors.remove(cursor);
        self.inner.cursor_resource_guards.remove(&cursor);
        if entry.driver.engine() == Engine::SqlServer {
            self.with_session(&session_id, |s| s.connections.remove(&conn_id))?;
            // Also invoke driver.close so the driver-level socket/FD is
            // returned promptly instead of relying on ConnHandle::Drop.
            // Best-effort — the driver has already dropped its state, so
            // an error here is informational only.
            if let Err(error) = entry.driver.close(entry.handle.clone()).await {
                tracing::debug!(
                    session_id = %session_id,
                    conn_id = %conn_id,
                    %error,
                    "driver.close after mssql cancel returned error"
                );
            }
            tracing::info!(
                session_id = %session_id,
                conn_id = %conn_id,
                "removed sqlserver connection after cancel abort"
            );
        }
        Ok(())
    }

    pub async fn bulk_insert(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        req: BulkInsertRequest,
    ) -> ApiResult<BulkInsertResponse> {
        let table_path = object_path_from_qualified_name(&req.table)?;
        let entry = self.authorize_connection_operation(
            session_id,
            conn_id,
            sift_protocol::OperationKind::BulkInsert,
            None,
            &[&table_path],
        )?;
        if req.format == BulkInsertFormat::Native {
            return Err(ApiError::Driver(
                DriverError::new(
                    sift_protocol::Code::UnsupportedForEngine,
                    "SQL Server native bulk format needs typed rows and is not part of the locked Phase A driver trait",
                )
                .with_engine(Engine::SqlServer),
            ));
        }

        let driver = entry.driver.clone();
        let handle = entry.handle.clone();
        let table = req.table;
        let data = req.data;
        let result = self
            .run_bounded("bulk_insert", async move {
                let mssql = driver.as_mssql().ok_or_else(|| {
                    DriverError::new(
                        Code::UnsupportedForEngine,
                        "bulk insert is only supported by SQL Server connections",
                    )
                    .with_engine(driver.engine())
                })?;
                mssql
                    .bulk_insert(
                        handle,
                        BulkOp {
                            table,
                            data,
                            delimiter: b',',
                            header: true,
                            null_value: None,
                        },
                    )
                    .await
            })
            .await?;
        Ok(BulkInsertResponse {
            rows_inserted: result.rows_inserted,
        })
    }

    pub async fn begin_transaction(
        &self,
        session_id: SessionId,
        req: BeginTransactionRequest,
    ) -> ApiResult<TransactionInfo> {
        self.begin_transaction_as(
            session_id,
            req,
            sift_protocol::OperationKind::BeginTransaction,
        )
        .await
    }

    pub(crate) async fn begin_transaction_as(
        &self,
        session_id: SessionId,
        req: BeginTransactionRequest,
        operation: sift_protocol::OperationKind,
    ) -> ApiResult<TransactionInfo> {
        let entry =
            self.authorize_connection_operation(session_id, req.connection, operation, None, &[])?;
        self.reject_if_connection_has_tx(session_id, req.connection, None)?;
        let driver = entry.driver.clone();
        let conn_handle = entry.handle.clone();
        let mode = req.mode;
        let handle = self
            .run_bounded(
                "begin",
                async move { driver.begin(conn_handle, mode).await },
            )
            .await?;
        let info = TransactionInfo {
            tx_id: handle.tx_id,
            connection: req.connection,
            mode: handle.mode,
            opened_at: chrono::Utc::now(),
        };
        let tx = TransactionEntry {
            info: info.clone(),
            handle,
            savepoints: Mutex::new(Vec::new()),
            ending: AtomicBool::new(false),
        };
        let session = self
            .inner
            .sessions
            .get(&session_id)
            .ok_or(ApiError::SessionNotFound(session_id))?;
        session.transactions.insert(info.tx_id, tx);
        Ok(info)
    }

    pub async fn commit_transaction(
        &self,
        session_id: SessionId,
        req: EndTransactionRequest,
    ) -> ApiResult<()> {
        self.commit_transaction_as(
            session_id,
            req,
            sift_protocol::OperationKind::CommitTransaction,
        )
        .await
    }

    pub(crate) async fn commit_transaction_as(
        &self,
        session_id: SessionId,
        req: EndTransactionRequest,
        operation: sift_protocol::OperationKind,
    ) -> ApiResult<()> {
        self.authorize_connection_operation(session_id, req.connection, operation, None, &[])?;
        let tx = self.claim_tx_end(session_id, req.connection, req.tx_id)?;
        let entry = self.get_conn_entry(session_id, req.connection)?;
        let driver = entry.driver.clone();
        if let Err(error) = self
            .run_bounded("commit", async move { driver.commit(tx).await })
            .await
        {
            if transaction_end_is_retryable(&error) {
                self.release_tx_end(session_id, req.tx_id);
            }
            return Err(error);
        }
        self.remove_tx(session_id, req.connection, req.tx_id)?;
        Ok(())
    }

    pub async fn rollback_transaction(
        &self,
        session_id: SessionId,
        req: EndTransactionRequest,
    ) -> ApiResult<()> {
        self.rollback_transaction_as(
            session_id,
            req,
            sift_protocol::OperationKind::RollbackTransaction,
        )
        .await
    }

    pub(crate) async fn rollback_transaction_as(
        &self,
        session_id: SessionId,
        req: EndTransactionRequest,
        operation: sift_protocol::OperationKind,
    ) -> ApiResult<()> {
        self.authorize_connection_operation(session_id, req.connection, operation, None, &[])?;
        let tx = self.claim_tx_end(session_id, req.connection, req.tx_id)?;
        let entry = self.get_conn_entry(session_id, req.connection)?;
        let driver = entry.driver.clone();
        if let Err(error) = self
            .run_bounded("rollback", async move { driver.rollback(tx).await })
            .await
        {
            if transaction_end_is_retryable(&error) {
                self.release_tx_end(session_id, req.tx_id);
            }
            return Err(error);
        }
        self.remove_tx(session_id, req.connection, req.tx_id)?;
        Ok(())
    }

    pub fn list_transactions(&self, session_id: SessionId) -> ApiResult<Vec<TransactionState>> {
        let session = self
            .inner
            .sessions
            .get(&session_id)
            .ok_or(ApiError::SessionNotFound(session_id))?;
        let mut transactions: Vec<_> = session
            .transactions
            .iter()
            .map(|entry| TransactionState {
                transaction: entry.info.clone(),
                savepoints: entry.savepoints.lock().unwrap().clone(),
            })
            .collect();
        for transaction in &transactions {
            self.authorize_connection_operation(
                session_id,
                transaction.transaction.connection,
                sift_protocol::OperationKind::ListTransactions,
                None,
                &[],
            )?;
        }
        transactions.sort_by_key(|state| state.transaction.opened_at);
        Ok(transactions)
    }

    pub fn preview_transaction(
        &self,
        session_id: SessionId,
        req: &TransactionPreviewRequest,
    ) -> ApiResult<TransactionPreview> {
        self.authorize_connection_operation(
            session_id,
            req.connection,
            sift_protocol::OperationKind::PreviewTransaction,
            None,
            &[],
        )?;
        let session = self
            .inner
            .sessions
            .get(&session_id)
            .ok_or(ApiError::SessionNotFound(session_id))?;
        let entry = session.transactions.get(&req.tx_id).ok_or_else(|| {
            ApiError::Driver(DriverError::new(
                Code::TransactionNotFound,
                "transaction not active",
            ))
        })?;
        if entry.info.connection != req.connection {
            return Err(ApiError::BadRequest(
                "`connection` must match transaction connection".into(),
            ));
        }
        let savepoints = entry.savepoints.lock().unwrap();
        let active_savepoints = savepoints
            .iter()
            .filter(|savepoint| savepoint.state == SavepointState::Active)
            .count();
        let age_seconds = chrono::Utc::now()
            .signed_duration_since(entry.info.opened_at)
            .num_seconds()
            .max(0) as u64;
        Ok(TransactionPreview {
            transaction: entry.info.clone(),
            action: req.action,
            age_seconds,
            active_savepoints,
            closes_savepoints: active_savepoints,
            destructive: req.action == TransactionEndAction::Rollback,
        })
    }

    pub async fn create_savepoint(
        &self,
        session_id: SessionId,
        req: SavepointRequest,
    ) -> ApiResult<()> {
        self.authorize_connection_operation(
            session_id,
            req.connection,
            sift_protocol::OperationKind::Savepoint,
            None,
            &[],
        )?;
        let name = req.name.trim().to_string();
        if name.is_empty() {
            return Err(ApiError::BadRequest(
                "savepoint name must not be empty".into(),
            ));
        }
        self.ensure_savepoint_name_available(session_id, req.tx_id, &name)?;
        let tx_handle = self.tx_handle_for(session_id, req.connection, req.tx_id)?;
        let entry = self.get_conn_entry(session_id, req.connection)?;
        let driver = entry.driver.clone();
        let driver_name = name.clone();
        self.run_bounded("savepoint", async move {
            match driver.engine() {
                Engine::Postgres => {
                    let pg = driver
                        .as_pg()
                        .ok_or_else(|| missing_ext(Engine::Postgres, "PgExt"))?;
                    pg.savepoint(&tx_handle, &driver_name).await.map(|_| ())
                }
                Engine::SqlServer => {
                    let mssql = driver
                        .as_mssql()
                        .ok_or_else(|| missing_ext(Engine::SqlServer, "MssqlExt"))?;
                    mssql.savepoint(&tx_handle, &driver_name).await.map(|_| ())
                }
            }
        })
        .await?;
        self.update_savepoints(session_id, req.tx_id, |savepoints| {
            savepoints.push(SavepointInfo {
                name,
                created_at: chrono::Utc::now(),
                state: SavepointState::Active,
            });
        })?;
        Ok(())
    }

    pub async fn rollback_to_savepoint(
        &self,
        session_id: SessionId,
        req: SavepointRequest,
    ) -> ApiResult<()> {
        self.authorize_connection_operation(
            session_id,
            req.connection,
            sift_protocol::OperationKind::RollbackToSavepoint,
            None,
            &[],
        )?;
        let tx_handle = self.tx_handle_for(session_id, req.connection, req.tx_id)?;
        let entry = self.get_conn_entry(session_id, req.connection)?;
        let driver = entry.driver.clone();
        let name = req.name;
        self.ensure_active_savepoint(session_id, req.tx_id, &name)?;
        let state_name = name.clone();
        let tx_id = req.tx_id;
        self.run_bounded("rollback_to_savepoint", async move {
            match driver.engine() {
                Engine::Postgres => {
                    let pg = driver
                        .as_pg()
                        .ok_or_else(|| missing_ext(Engine::Postgres, "PgExt"))?;
                    pg.rollback_to(PgSavepoint {
                        tx: tx_id,
                        conn: tx_handle.conn.clone(),
                        name,
                    })
                    .await
                }
                Engine::SqlServer => {
                    let mssql = driver
                        .as_mssql()
                        .ok_or_else(|| missing_ext(Engine::SqlServer, "MssqlExt"))?;
                    mssql
                        .rollback_to(MssqlSavepoint {
                            tx: tx_id,
                            conn: tx_handle.conn.clone(),
                            name,
                        })
                        .await
                }
            }
        })
        .await?;
        self.update_savepoints(session_id, tx_id, |savepoints| {
            if let Some(index) = savepoints.iter().position(|savepoint| {
                savepoint.name == state_name && savepoint.state == SavepointState::Active
            }) {
                for savepoint in savepoints.iter_mut().skip(index + 1) {
                    if savepoint.state == SavepointState::Active {
                        savepoint.state = SavepointState::Invalidated;
                    }
                }
            }
        })?;
        Ok(())
    }

    pub async fn release_savepoint(
        &self,
        session_id: SessionId,
        req: SavepointRequest,
    ) -> ApiResult<()> {
        self.authorize_connection_operation(
            session_id,
            req.connection,
            sift_protocol::OperationKind::ReleaseSavepoint,
            None,
            &[],
        )?;
        // Validate tx is active on the connection before dispatching.
        let tx_handle = self.tx_handle_for(session_id, req.connection, req.tx_id)?;
        let entry = self.get_conn_entry(session_id, req.connection)?;
        let driver = entry.driver.clone();
        let name = req.name;
        self.ensure_active_savepoint(session_id, req.tx_id, &name)?;
        let state_name = name.clone();
        let tx_id = req.tx_id;
        self.run_bounded("release_savepoint", async move {
            match driver.engine() {
                Engine::Postgres => {
                    let pg = driver
                        .as_pg()
                        .ok_or_else(|| missing_ext(Engine::Postgres, "PgExt"))?;
                    pg.release_savepoint(PgSavepoint {
                        tx: tx_id,
                        conn: tx_handle.conn.clone(),
                        name,
                    })
                    .await
                }
                Engine::SqlServer => Err(DriverError::new(
                    Code::UnsupportedForEngine,
                    "RELEASE SAVEPOINT is not supported by SQL Server",
                )
                .with_engine(Engine::SqlServer)),
            }
        })
        .await?;
        self.update_savepoints(session_id, tx_id, |savepoints| {
            if let Some(savepoint) = savepoints.iter_mut().find(|savepoint| {
                savepoint.name == state_name && savepoint.state == SavepointState::Active
            }) {
                savepoint.state = SavepointState::Released;
            }
        })?;
        Ok(())
    }

    fn ensure_savepoint_name_available(
        &self,
        session_id: SessionId,
        tx_id: TxId,
        name: &str,
    ) -> ApiResult<()> {
        self.with_transaction(session_id, tx_id, |entry| {
            if entry.savepoints.lock().unwrap().iter().any(|savepoint| {
                savepoint.name == name && savepoint.state == SavepointState::Active
            }) {
                return Err(ApiError::BadRequest(
                    "savepoint name is already active".into(),
                ));
            }
            Ok(())
        })
    }

    fn ensure_active_savepoint(
        &self,
        session_id: SessionId,
        tx_id: TxId,
        name: &str,
    ) -> ApiResult<()> {
        self.with_transaction(session_id, tx_id, |entry| {
            if entry.savepoints.lock().unwrap().iter().any(|savepoint| {
                savepoint.name == name && savepoint.state == SavepointState::Active
            }) {
                Ok(())
            } else {
                Err(ApiError::BadRequest("savepoint is not active".into()))
            }
        })
    }

    fn update_savepoints(
        &self,
        session_id: SessionId,
        tx_id: TxId,
        update: impl FnOnce(&mut Vec<SavepointInfo>),
    ) -> ApiResult<()> {
        self.with_transaction(session_id, tx_id, |entry| {
            update(&mut entry.savepoints.lock().unwrap());
            Ok(())
        })
    }

    fn with_transaction<T>(
        &self,
        session_id: SessionId,
        tx_id: TxId,
        use_entry: impl FnOnce(&TransactionEntry) -> ApiResult<T>,
    ) -> ApiResult<T> {
        let session = self
            .inner
            .sessions
            .get(&session_id)
            .ok_or(ApiError::SessionNotFound(session_id))?;
        let entry = session.transactions.get(&tx_id).ok_or_else(|| {
            ApiError::Driver(DriverError::new(
                Code::TransactionNotFound,
                "transaction not active",
            ))
        })?;
        use_entry(&entry)
    }

    fn tx_handle_for(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        tx_id: TxId,
    ) -> ApiResult<TxHandle> {
        let session = self
            .inner
            .sessions
            .get(&session_id)
            .ok_or(ApiError::SessionNotFound(session_id))?;
        let entry = session.transactions.get(&tx_id).ok_or_else(|| {
            ApiError::Driver(DriverError::new(
                sift_protocol::Code::TransactionNotFound,
                "transaction not active",
            ))
        })?;
        if entry.info.connection != conn_id {
            return Err(ApiError::BadRequest(
                "`connection` must match transaction connection".into(),
            ));
        }
        if entry.ending.load(Ordering::Acquire) {
            return Err(ApiError::BadRequest("transaction is ending".into()));
        }
        Ok(entry.handle.clone())
    }

    fn claim_tx_end(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        tx_id: TxId,
    ) -> ApiResult<TxHandle> {
        let session = self
            .inner
            .sessions
            .get(&session_id)
            .ok_or(ApiError::SessionNotFound(session_id))?;
        let entry = session.transactions.get(&tx_id).ok_or_else(|| {
            ApiError::Driver(DriverError::new(
                Code::TransactionNotFound,
                "transaction not active",
            ))
        })?;
        if entry.info.connection != conn_id {
            return Err(ApiError::BadRequest(
                "`connection` must match transaction connection".into(),
            ));
        }
        entry
            .ending
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| ApiError::BadRequest("transaction is already ending".into()))?;
        Ok(entry.handle.clone())
    }

    fn release_tx_end(&self, session_id: SessionId, tx_id: TxId) {
        if let Some(session) = self.inner.sessions.get(&session_id) {
            if let Some(entry) = session.transactions.get(&tx_id) {
                entry.ending.store(false, Ordering::Release);
            }
        }
    }

    fn validate_execute_tx(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        tx: Option<&TxHandleRef>,
    ) -> ApiResult<()> {
        match tx {
            Some(tx) => {
                if tx.connection != conn_id {
                    return Err(ApiError::BadRequest(
                        "`tx.connection` must match request connection".into(),
                    ));
                }
                self.reject_if_connection_has_tx(session_id, conn_id, Some(tx.tx_id))?;
                Ok(())
            }
            None => self.reject_if_connection_has_tx(session_id, conn_id, None),
        }
    }

    fn reject_if_connection_has_tx(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        expected: Option<TxId>,
    ) -> ApiResult<()> {
        let session = self
            .inner
            .sessions
            .get(&session_id)
            .ok_or(ApiError::SessionNotFound(session_id))?;
        let active = session
            .transactions
            .iter()
            .find(|tx| tx.info.connection == conn_id)
            .map(|tx| tx.info.tx_id);
        match (active, expected) {
            (Some(active), Some(expected)) if active == expected => Ok(()),
            (Some(_), Some(_)) => Err(ApiError::BadRequest(
                "transaction id is not active on this connection".into(),
            )),
            (Some(_), None) => Err(ApiError::BadRequest(
                "connection has an active transaction; pass `tx` explicitly".into(),
            )),
            (None, Some(_)) => Err(ApiError::BadRequest("transaction is not active".into())),
            (None, None) => Ok(()),
        }
    }

    fn remove_tx(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        tx_id: TxId,
    ) -> ApiResult<TransactionEntry> {
        let session = self
            .inner
            .sessions
            .get(&session_id)
            .ok_or(ApiError::SessionNotFound(session_id))?;
        let (_, tx) = session.transactions.remove(&tx_id).ok_or_else(|| {
            ApiError::Driver(DriverError::new(
                sift_protocol::Code::TransactionNotFound,
                "transaction not active",
            ))
        })?;
        if tx.info.connection != conn_id {
            session.transactions.insert(tx_id, tx);
            return Err(ApiError::BadRequest(
                "`connection` must match transaction connection".into(),
            ));
        }
        Ok(tx)
    }

    /// Public accessor for the (driver, handle) tuple of a session's
    /// connection. Used by the export streaming path in `http.rs`
    /// which needs the driver to spawn its own execute stream.
    pub fn conn_entry(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
    ) -> ApiResult<ConnectionEntryClone> {
        self.get_conn_entry(session_id, conn_id)
    }

    /// Generate DDL for `object` on the connection identified by
    /// `(session_id, conn_id)`. Delegates to
    /// [`crate::ddl::generate_ddl`] which orchestrates existing
    /// driver calls (`schema` + `execute`) rather than adding a new
    /// method to the `Driver` trait.
    pub async fn ddl_for(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        object: sift_protocol::ObjectPath,
    ) -> ApiResult<sift_protocol::ObjectDdl> {
        let entry = self.authorize_connection_operation(
            session_id,
            conn_id,
            sift_protocol::OperationKind::GenerateDdl,
            None,
            &[&object],
        )?;
        let driver = entry.driver.clone();
        let handle = entry.handle.clone();
        let result = crate::ddl::generate_ddl(&*driver, handle, object).await?;
        Ok(result)
    }

    /// Generate the inline-edit DML plan without executing it. Fetches the
    /// target table's deep schema to resolve row identity + column metadata.
    pub async fn preview_edits(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        edit_set: sift_protocol::EditSet,
    ) -> ApiResult<sift_protocol::EditPlan> {
        let entry = self.authorize_connection_operation(
            session_id,
            conn_id,
            sift_protocol::OperationKind::PreviewEdits,
            None,
            &[&edit_set.table],
        )?;
        let driver = entry.driver.clone();
        let handle = entry.handle.clone();
        crate::edit::build_plan(&*driver, handle, &edit_set)
            .await
            .map_err(ApiError::Driver)
    }

    /// Apply an inline-edit set transactionally. Generates the plan, then runs
    /// every statement inside one transaction (its own, or the caller-supplied
    /// `tx`). Any driver error or a mismatched `affected_rows` on an
    /// update/delete rolls the whole set back and returns a conflict.
    pub async fn apply_edits(
        &self,
        session_id: SessionId,
        req: sift_protocol::ApplyEditsRequest,
    ) -> ApiResult<sift_protocol::ApplyEditsResult> {
        use sift_protocol::{EditStatementKind, ExecuteRequestHttp};

        let conn_id = req.connection;
        self.authorize_connection_operation(
            session_id,
            conn_id,
            sift_protocol::OperationKind::ApplyEdits,
            None,
            &[&req.edit_set.table],
        )?;
        let plan = {
            let entry = self.get_conn_entry(session_id, conn_id)?;
            let driver = entry.driver.clone();
            let handle = entry.handle.clone();
            crate::edit::build_plan(&*driver, handle, &req.edit_set)
                .await
                .map_err(ApiError::Driver)?
        };

        // Own a transaction unless the caller passed one to run under.
        let (tx_ref, owned) = match req.tx {
            Some(tx) => (tx, false),
            None => {
                let info = self
                    .begin_transaction_as(
                        session_id,
                        sift_protocol::BeginTransactionRequest {
                            connection: conn_id,
                            mode: sift_protocol::TxMode::default(),
                        },
                        sift_protocol::OperationKind::ApplyEdits,
                    )
                    .await?;
                (
                    sift_protocol::TxHandleRef {
                        tx_id: info.tx_id,
                        connection: info.connection,
                        mode: info.mode,
                    },
                    true,
                )
            }
        };

        let mut applied = Vec::with_capacity(plan.statements.len());
        for stmt in plan.statements {
            let is_write = matches!(
                stmt.kind,
                EditStatementKind::Update | EditStatementKind::Delete
            );
            let exec = ExecuteRequestHttp {
                connection: conn_id,
                sql: stmt.sql,
                params: stmt.params,
                tx: Some(tx_ref.clone()),
                room_id: None,
                connection_profile_id: None,
            };
            match self
                .execute_http_as(session_id, exec, sift_protocol::OperationKind::ApplyEdits)
                .await
            {
                Ok(resp) => {
                    let mut affected = resp.affected_rows.unwrap_or(0);
                    // An update/delete must hit exactly one row; otherwise the
                    // row changed or vanished under the user (optimistic
                    // conflict), or the identity wasn't unique.
                    if is_write && affected != 1 {
                        if owned {
                            self.rollback_edits_tx(session_id, conn_id, tx_ref.tx_id)
                                .await;
                        }
                        return Err(ApiError::Driver(DriverError::new(
                            Code::EditConflict,
                            format!(
                                "edit {} affected {affected} rows (expected 1); the row \
                                 changed or no longer matches",
                                stmt.edit_index
                            ),
                        )));
                    }
                    // Insert-with-RETURNING is row-producing, so the driver may
                    // report no `affected_rows`; treat returned keys as the count.
                    if !is_write && affected == 0 {
                        affected = resp.rows.len() as u64;
                    }
                    applied.push(sift_protocol::EditOutcome {
                        edit_index: stmt.edit_index,
                        kind: stmt.kind,
                        affected_rows: affected,
                        returned: resp.rows,
                    });
                }
                Err(e) => {
                    if owned {
                        self.rollback_edits_tx(session_id, conn_id, tx_ref.tx_id)
                            .await;
                    }
                    return Err(e);
                }
            }
        }

        let committed = if owned {
            self.commit_transaction_as(
                session_id,
                sift_protocol::EndTransactionRequest {
                    connection: conn_id,
                    tx_id: tx_ref.tx_id,
                },
                sift_protocol::OperationKind::ApplyEdits,
            )
            .await?;
            true
        } else {
            false
        };

        Ok(sift_protocol::ApplyEditsResult { applied, committed })
    }

    /// Best-effort rollback on the inline-edit apply failure path.
    async fn rollback_edits_tx(&self, session_id: SessionId, conn_id: ConnectionId, tx_id: TxId) {
        if let Err(e) = self
            .rollback_transaction_as(
                session_id,
                sift_protocol::EndTransactionRequest {
                    connection: conn_id,
                    tx_id,
                },
                sift_protocol::OperationKind::ApplyEdits,
            )
            .await
        {
            tracing::warn!(
                session_id = %session_id,
                conn_id = %conn_id,
                error = %e,
                "rollback after failed inline-edit apply failed"
            );
        }
    }

    /// Build or reuse the per-connection schema-search index. Built from a
    /// shallow schema snapshot (objects) plus one bulk catalog query
    /// (columns); cached with a TTL. Synchronous build means the index is
    /// always `Ready` in v1 (background pre-warm is a future enhancement).
    async fn search_index_for(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
    ) -> ApiResult<(Arc<crate::search::SearchIndex>, sift_protocol::IndexState)> {
        if let Some(entry) = self.inner.search_indexes.get(&conn_id) {
            if entry.1.elapsed() < SEARCH_INDEX_TTL {
                return Ok((entry.0.clone(), sift_protocol::IndexState::Ready));
            }
        }
        let snapshot = (*self
            .schema_cached(session_id, conn_id, sift_protocol::SchemaScope::shallow())
            .await?
            .snapshot)
            .clone();
        let engine = self.get_conn_entry(session_id, conn_id)?.driver.engine();
        let resp = self
            .execute_http_as(
                session_id,
                sift_protocol::ExecuteRequestHttp {
                    connection: conn_id,
                    sql: crate::search::bulk_columns_sql(engine).to_string(),
                    params: Vec::new(),
                    tx: None,
                    room_id: None,
                    connection_profile_id: None,
                },
                sift_protocol::OperationKind::SearchSchema,
            )
            .await?;
        let columns = crate::search::decode_catalog_columns(resp.rows);
        let index = Arc::new(crate::search::SearchIndex::build(&snapshot, columns));
        self.inner
            .search_indexes
            .insert(conn_id, (index.clone(), Instant::now()));
        Ok((index, sift_protocol::IndexState::Ready))
    }

    /// Fuzzy schema search (object + column names) over the in-memory index.
    pub async fn search_schema(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        req: sift_protocol::SchemaSearchRequest,
    ) -> ApiResult<sift_protocol::SchemaSearchResponse> {
        self.authorize_connection_operation(
            session_id,
            conn_id,
            sift_protocol::OperationKind::SearchSchema,
            None,
            &[],
        )?;
        let (index, index_state) = self.search_index_for(session_id, conn_id).await?;
        let limit = req.limit.unwrap_or(crate::search::DEFAULT_SCHEMA_HITS);
        let hits = crate::search::rank(&index, &req.query, req.kinds.as_deref(), limit);
        Ok(sift_protocol::SchemaSearchResponse { hits, index_state })
    }

    /// Bounded live data search: parameterized `LIKE` over text columns of the
    /// scoped tables, capped per-table and by table count, running through the
    /// normal execute path (timeout + cursor caps apply).
    pub async fn search_data(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        req: sift_protocol::DataSearchRequest,
    ) -> ApiResult<sift_protocol::DataSearchResponse> {
        self.authorize_connection_operation(
            session_id,
            conn_id,
            sift_protocol::OperationKind::SearchData,
            None,
            &[],
        )?;
        let (index, _) = self.search_index_for(session_id, conn_id).await?;
        let engine = self.get_conn_entry(session_id, conn_id)?.driver.engine();
        let per_table = req
            .per_table_limit
            .unwrap_or(crate::search::DEFAULT_PER_TABLE)
            .clamp(1, crate::search::MAX_PER_TABLE);
        let max_tables = req
            .max_tables
            .unwrap_or(crate::search::DEFAULT_MAX_TABLES)
            .clamp(1, crate::search::MAX_TABLES);

        let all_tables = crate::search::resolve_scope(&index, &req.scope);
        let mut truncated = all_tables.len() as u32 > max_tables;
        let pattern = sift_protocol::Value::Text(crate::search::like_pattern(&req.query));

        let mut hits = Vec::new();
        let mut tables_searched = 0u32;
        for table in all_tables.into_iter().take(max_tables as usize) {
            let text_cols = crate::search::text_columns_for(&index, &table, req.columns.as_deref());
            let Some(sql) = crate::search::data_search_sql(engine, &table, &text_cols, per_table)
            else {
                continue;
            };
            tables_searched += 1;
            let resp = self
                .execute_http_as(
                    session_id,
                    sift_protocol::ExecuteRequestHttp {
                        connection: conn_id,
                        sql,
                        params: vec![pattern.clone()],
                        tx: None,
                        room_id: None,
                        connection_profile_id: None,
                    },
                    sift_protocol::OperationKind::SearchData,
                )
                .await?;
            if resp.rows.len() as u32 >= per_table {
                truncated = true;
            }
            for row in resp.rows {
                hits.push(sift_protocol::DataSearchHit {
                    table: table.clone(),
                    columns: text_cols.clone(),
                    row,
                    matched_columns: text_cols.clone(),
                });
            }
        }
        Ok(sift_protocol::DataSearchResponse {
            hits,
            truncated,
            tables_searched,
        })
    }

    /// Compute completion candidates for `request.sql` at
    /// `request.cursor` on the connection identified by
    /// `(session_id, conn_id)`. Delegates to
    /// [`crate::autocomplete::generate_completion`], which composes
    /// schema snapshots and the `sift-completion` ranker.
    pub async fn complete(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        request: sift_protocol::completion::CompletionRequest,
    ) -> ApiResult<sift_protocol::completion::CompletionResponse> {
        let entry = self.authorize_connection_operation(
            session_id,
            conn_id,
            sift_protocol::OperationKind::Complete,
            None,
            &[],
        )?;
        let engine = entry.driver.engine();
        crate::autocomplete::generate_completion(self, session_id, conn_id, engine, request).await
    }

    fn get_conn_entry(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
    ) -> ApiResult<ConnectionEntryClone> {
        // We can't return a borrowed `ConnectionEntry` because DashMap shard
        // locks can't be held across `.await`. Clone the cheap bits (Arc,
        // ConnHandle is Arc-backed) and release the lock.
        let session = self
            .inner
            .sessions
            .get(&session_id)
            .ok_or(ApiError::SessionNotFound(session_id))?;
        let entry = session
            .connections
            .get(&conn_id)
            .ok_or(ApiError::ConnectionNotFound(conn_id))?;
        Ok(ConnectionEntryClone {
            driver: entry.driver.clone(),
            handle: entry.handle.clone(),
            provenance: entry.provenance.clone(),
        })
    }

    fn with_session<F, R>(&self, session_id: &SessionId, f: F) -> ApiResult<R>
    where
        F: FnOnce(&Session) -> R,
    {
        let session = self
            .inner
            .sessions
            .get(session_id)
            .ok_or(ApiError::SessionNotFound(*session_id))?;
        Ok(f(&session))
    }

    /// Reap sessions idle longer than `max_idle`. Phase 0: not wired into a
    /// background task yet; tests call it directly.
    pub fn reap_idle(&self, max_idle: Duration) -> usize {
        let now = chrono::Utc::now();
        let cutoff = now
            - chrono::Duration::from_std(max_idle)
                .unwrap_or_else(|_| chrono::Duration::milliseconds(i64::MAX));
        let mut reaped = 0;
        let to_close: Vec<SessionId> = self
            .inner
            .sessions
            .iter()
            .filter(|s| s.created_at < cutoff && s.connections.is_empty())
            .map(|s| s.id)
            .collect();
        for id in to_close {
            if self.inner.sessions.remove(&id).is_some() {
                reaped += 1;
                tracing::info!(session_id = %id, "reaped idle session");
            }
        }
        reaped
    }
}

/// Cheap clone of a connection entry (just Arc + ConnHandle Arc).
pub struct ConnectionEntryClone {
    pub driver: Arc<dyn Driver>,
    pub handle: ConnHandle,
    pub provenance: ConnectionProvenance,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionProvenance {
    TrustedLocal,
    Managed {
        principal_id: PrincipalId,
        tenant_id: sift_metadata::TenantId,
        profile_id: sift_metadata::ConnectionProfileId,
        policy_revision: u64,
        quota_exempt: bool,
    },
}

pub struct Session {
    pub id: SessionId,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub tag: Option<String>,
    pub owner_principal_id: Option<PrincipalId>,
    pub connections: DashMap<ConnectionId, ConnectionEntry>,
    pub transactions: DashMap<TxId, TransactionEntry>,
    pub next_conn_id: AtomicU64,
    tenant_id: Mutex<Option<sift_metadata::TenantId>>,
    resource_guard: Mutex<Option<crate::resources::ResourceGuard>>,
}

impl Session {
    fn info(&self) -> SessionInfo {
        SessionInfo {
            id: self.id,
            created_at: self.created_at,
            tag: self.tag.clone(),
            connections: self.connections.iter().map(|e| e.info.clone()).collect(),
        }
    }
}

/// One open connection within a session.
pub struct ConnectionEntry {
    pub id: ConnectionId,
    pub engine: Engine,
    pub handle: ConnHandle,
    pub driver: Arc<dyn Driver>,
    pub info: ConnectionInfo,
    /// Original spec, retained so a broken connection can be transparently
    /// re-established for idempotent operations (ping/schema).
    pub spec: ConnectionSpec,
    pub provenance: ConnectionProvenance,
    _resource_guard: Option<crate::resources::ResourceGuard>,
}

pub struct TransactionEntry {
    pub info: TransactionInfo,
    pub handle: TxHandle,
    pub savepoints: Mutex<Vec<SavepointInfo>>,
    ending: AtomicBool,
}

/// Strip secrets and bind values from an operation before it is recorded on
/// any audit surface (ADR-009): SQL text becomes a fingerprint, execute params
/// are cleared, connection passwords are redacted, and bulk payloads dropped.
/// The audit trail correlates *what happened* without persisting *what data*.
fn sanitize_operation(operation: Operation) -> Operation {
    match operation {
        Operation::ExecuteQuery { session, request } => {
            let request = ExecuteRequestHttp {
                sql: crate::fingerprint::sql(&request.sql),
                params: Vec::new(),
                ..request
            };
            Operation::ExecuteQuery { session, request }
        }
        Operation::OpenConnection {
            session,
            mut request,
        } => {
            request.spec.password = None;
            Operation::OpenConnection { session, request }
        }
        Operation::BulkInsert {
            session,
            connection,
            mut request,
        } => {
            request.data = Vec::new();
            Operation::BulkInsert {
                session,
                connection,
                request,
            }
        }
        other => other,
    }
}

/// Whether a driver failure signals a broken connection that is safe to
/// re-establish. The retry boundary is deliberately narrow: only
/// `ConnectionFailed`, and callers only retry idempotent operations
/// (ping/schema). Mutating work (execute, bulk insert, transactions) is never
/// auto-retried because a reconnect cannot know whether the first attempt's
/// side effects already landed.
fn is_reconnectable(error: &DriverError) -> bool {
    error.code == Code::ConnectionFailed
}

fn missing_ext(engine: Engine, trait_name: &str) -> DriverError {
    DriverError::new(
        Code::UnsupportedForEngine,
        format!("driver does not expose {trait_name}"),
    )
    .with_engine(engine)
}

/// Releases a cursor from the registry when dropped. Used by the export
/// path: the encoded byte stream owns one of these, so a completed
/// download or a dropped consumer (client disconnect) removes the cursor
/// — which also signals the registry pump to cancel — without an explicit
/// cleanup call in the handler.
struct CursorGuard {
    sessions: SessionStore,
    cursor_id: CursorId,
}

impl Drop for CursorGuard {
    fn drop(&mut self) {
        self.sessions.cursor_remove(self.cursor_id);
    }
}

/// Build the `QueryTimedOut` driver error returned when a synchronous driver
/// call exceeds the configured per-request deadline.
fn timeout_error(op: &str) -> ApiError {
    ApiError::Driver(DriverError::new(
        Code::QueryTimedOut,
        format!("`{op}` exceeded the configured request timeout"),
    ))
}

fn transaction_end_is_retryable(error: &ApiError) -> bool {
    !matches!(
        error,
        ApiError::Driver(DriverError {
            code: Code::QueryTimedOut,
            ..
        }) | ApiError::Internal(_)
    )
}

fn spawn_operation_log_writer(file: File) -> OperationLogWriter {
    const OPERATION_LOG_QUEUE: usize = 1024;
    let (tx, rx) = std::sync::mpsc::sync_channel::<OperationAuditEntry>(OPERATION_LOG_QUEUE);
    let task = std::thread::Builder::new()
        .name("sift-operation-log-writer".into())
        .spawn(move || {
            let mut writer = BufWriter::new(file);
            while let Ok(entry) = rx.recv() {
                if let Err(error) = write_operation_log_entry(&mut writer, &entry) {
                    tracing::error!(%error, "operation audit append failed");
                    continue;
                }
                while let Ok(entry) = rx.try_recv() {
                    if let Err(error) = write_operation_log_entry(&mut writer, &entry) {
                        tracing::error!(%error, "operation audit append failed");
                    }
                }
                if let Err(error) = writer.flush() {
                    tracing::error!(%error, "operation audit flush failed");
                }
            }
            if let Err(error) = writer.flush() {
                tracing::error!(%error, "operation audit final flush failed");
            }
        })
        .expect("operation log writer thread starts");
    OperationLogWriter { tx, _task: task }
}

fn write_operation_log_entry(
    writer: &mut BufWriter<File>,
    entry: &OperationAuditEntry,
) -> std::io::Result<()> {
    serde_json::to_writer(&mut *writer, entry).map_err(std::io::Error::other)?;
    writer.write_all(b"\n")
}

fn drain_connection_transactions(s: &Session, conn_id: ConnectionId) -> Vec<TransactionEntry> {
    let tx_ids: Vec<TxId> = s
        .transactions
        .iter()
        .filter_map(|tx| {
            if tx.info.connection == conn_id {
                Some(tx.info.tx_id)
            } else {
                None
            }
        })
        .collect();
    tx_ids
        .into_iter()
        .filter_map(|id| s.transactions.remove(&id).map(|(_, tx)| tx))
        .collect()
}

fn read_operation_log(path: &Path) -> std::io::Result<Vec<OperationAuditEntry>> {
    match File::open(path) {
        Ok(file) => {
            let mut entries = Vec::new();
            for line in BufReader::new(file).lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<OperationAuditEntry>(&line) {
                    Ok(entry) => entries.push(entry),
                    Err(error) => {
                        tracing::warn!(%error, path = %path.display(), "skipping corrupt operation audit row");
                    }
                }
            }
            Ok(entries)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

/// Result type returned by the HTTP execute handler. Public so the WS
/// streaming layer can re-use the drain logic.
pub async fn drain_stream(
    stream: ResultSetStream,
    max_rows: usize,
    max_bytes: usize,
) -> Result<ExecuteResponse, DriverError> {
    let cursor_id = stream.cursor_id;
    let rx = stream.rows;
    tokio::pin!(rx);

    let mut columns: Vec<ColumnMetadata> = Vec::new();
    let mut rows: Vec<Row> = Vec::new();
    let mut affected_rows: Option<u64> = None;
    let mut warnings: Vec<DriverWarning> = Vec::new();
    let mut saw_result_set = false;
    let mut total_bytes: usize = 0;

    while let Some(page) = rx.recv().await {
        match page {
            Page::NextResult { columns: cols } => {
                if saw_result_set {
                    return Err(DriverError::new(
                        Code::UnsupportedResultShape,
                        "HTTP execute supports one result set; use WebSocket streaming for multi-result batches",
                    ));
                }
                saw_result_set = true;
                columns = cols;
            }
            Page::Rows { rows: r } => {
                if rows.capacity() == 0 {
                    rows.reserve(max_rows.min(r.len().saturating_mul(2)));
                }
                if rows.len().saturating_add(r.len()) > max_rows {
                    return Err(DriverError::new(
                        Code::ResultTooLarge,
                        format!(
                            "HTTP execute row cap exceeded ({max_rows}); use WebSocket streaming"
                        ),
                    ));
                }
                total_bytes = total_bytes.saturating_add(r.iter().map(row_bytes).sum());
                if total_bytes > max_bytes {
                    return Err(DriverError::new(
                        Code::ResultTooLarge,
                        format!(
                            "HTTP execute byte cap exceeded ({max_bytes} bytes); use WebSocket streaming"
                        ),
                    ));
                }
                rows.extend(r);
            }
            Page::Error { error } => return Err(error),
            Page::Done {
                affected_rows: a,
                warnings: w,
            } => {
                affected_rows = a;
                warnings = w;
            }
        }
    }

    Ok(ExecuteResponse {
        cursor_id,
        columns,
        rows,
        affected_rows,
        warnings,
        has_more: false,
    })
}

/// Approximate in-memory size of a row, for the HTTP result byte cap. Only the
/// variable-length variants (text/blob/decimal) are measured precisely;
/// fixed-width scalars use a small constant. This is an OOM guard, not an exact
/// accounting, so an estimate is sufficient.
fn row_bytes(row: &Row) -> usize {
    row.values.iter().map(value_bytes).sum::<usize>() + 8
}

fn value_bytes(value: &sift_protocol::Value) -> usize {
    use sift_protocol::Value;
    match value {
        Value::Text(s) | Value::Decimal(s) => s.len(),
        Value::Blob(b) => b.len(),
        Value::Json(_) => 16,
        _ => 16,
    }
}

/// Human-readable label for a connection spec. Used in `ConnectionInfo`
/// `display_name`; the client may overwrite.
fn object_path_from_qualified_name(value: &str) -> ApiResult<sift_protocol::ObjectPath> {
    let parts: Vec<_> = value.split('.').collect();
    let (catalog, schema, name) = match parts.as_slice() {
        [name] if !name.trim().is_empty() => (None, None, *name),
        [schema, name] if !schema.trim().is_empty() && !name.trim().is_empty() => {
            (None, Some((*schema).to_string()), *name)
        }
        [catalog, schema, name]
            if !catalog.trim().is_empty()
                && !schema.trim().is_empty()
                && !name.trim().is_empty() =>
        {
            (
                Some((*catalog).to_string()),
                Some((*schema).to_string()),
                *name,
            )
        }
        _ => {
            return Err(ApiError::BadRequest(
                "table must be `table`, `schema.table`, or `database.schema.table`".into(),
            ))
        }
    };
    Ok(sift_protocol::ObjectPath {
        catalog,
        schema,
        name: name.to_string(),
        kind: Some(sift_protocol::ObjectKind::Table),
        routine_args: None,
    })
}

fn display_name_for(spec: &ConnectionSpec) -> String {
    let db = spec.database.as_deref().unwrap_or("?");
    let host = if spec.host.starts_with('/') {
        // Unix socket directory — show the path's basename + db.
        let basename = std::path::Path::new(&spec.host)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("socket");
        format!("{basename}/{db}")
    } else {
        let port = spec.port.map(|p| format!(":{p}")).unwrap_or_default();
        format!("{}{port}/{db}", spec.host)
    };
    format!("{}@{}", spec.user, host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_log_trims_to_cap_keeping_newest() {
        let ring = RingLog::new(3);
        for n in 0..5 {
            ring.push(n);
        }
        // Oldest (0, 1) dropped; newest three retained in order.
        assert_eq!(ring.to_vec(), vec![2, 3, 4]);
    }

    #[test]
    fn ring_log_from_iter_trims_overflow() {
        let ring = RingLog::from_iter(2, vec!['a', 'b', 'c', 'd']);
        assert_eq!(ring.to_vec(), vec!['c', 'd']);
    }

    #[test]
    fn ring_log_snapshot_is_independent_of_later_pushes() {
        let ring = RingLog::new(10);
        ring.push(1);
        let snapshot = ring.to_vec();
        ring.push(2);
        // The snapshot taken before the second push is unchanged (COW).
        assert_eq!(snapshot, vec![1]);
        assert_eq!(ring.to_vec(), vec![1, 2]);
    }

    #[test]
    fn recording_operations_while_listing_does_not_lose_memory_rows() {
        const WRITERS: usize = 8;
        const PER_WRITER: usize = 250;

        let path = std::env::temp_dir().join(format!(
            "sift-operation-log-stress-{}.jsonl",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = Arc::new(
            SessionStore::new_with_operation_log_path(DriverRegistry::new(), &path)
                .expect("operation log opens"),
        );
        let done = Arc::new(AtomicBool::new(false));

        let reader_store = Arc::clone(&store);
        let reader_done = Arc::clone(&done);
        let reader = std::thread::spawn(move || {
            while !reader_done.load(Ordering::Relaxed) {
                let _ = reader_store.list_operations();
            }
        });

        let mut writers = Vec::new();
        for writer_id in 0..WRITERS {
            let store = Arc::clone(&store);
            writers.push(std::thread::spawn(move || {
                for i in 0..PER_WRITER {
                    store.push_operation(
                        Operation::OpenSession {
                            request: OpenSessionRequest {
                                tag: Some(format!("writer-{writer_id}-{i}")),
                            },
                        },
                        OperationStatus::Succeeded,
                    );
                }
            }));
        }
        for writer in writers {
            writer.join().unwrap();
        }
        done.store(true, Ordering::Relaxed);
        reader.join().unwrap();

        assert_eq!(store.list_operations().len(), WRITERS * PER_WRITER);
        let _ = std::fs::remove_file(path);
    }
}
