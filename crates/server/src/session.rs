//! Session + connection manager. The session store is the orchestrator
//! between HTTP handlers and drivers; it's the only thing that touches
//! `Arc<dyn Driver>` directly. A session is a logical workspace (ADR-002);
//! it holds zero or more open connections.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::Arc;
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::Duration;

use dashmap::DashMap;
use sift_driver_api::{
    BulkOp, ConnHandle, Driver, MssqlSavepoint, NotificationStream, PgSavepoint, ResultSetStream,
    TxHandle,
};
use sift_protocol::{
    AuditEntry, BeginTransactionRequest, BulkInsertFormat, BulkInsertRequest, BulkInsertResponse,
    Code, ColumnMetadata, ConnectionId, ConnectionInfo, ConnectionSpec, CursorId, DriverError,
    DriverWarning, EndTransactionRequest, Engine, ExecuteRequest, ExecuteRequestHttp,
    ExecuteResponse, OpenSessionRequest, Operation, OperationAuditEntry, OperationStatus, Page,
    Row, SavepointRequest, SchemaScope, SchemaSnapshot, ServerInfo, SessionId, SessionInfo,
    TransactionInfo, TxHandleRef, TxId,
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
    audit: Mutex<Vec<AuditEntry>>,
    operations: Mutex<OperationLog>,
    next_id: AtomicU64,
    registry: DriverRegistry,
    /// Per-request driver deadline in milliseconds. `0` disables the bound.
    /// Stored as an atomic so the server can set it from config after the
    /// store is constructed and shared behind an `Arc`.
    request_timeout_ms: AtomicU64,
    /// Sender to the background durable-audit writer thread. `None` when
    /// metadata is disabled. Sending is non-blocking, so the request path
    /// never waits on the SQLite write; every recorded operation still lands
    /// synchronously in the in-memory/JSONL log below.
    audit_tx: Mutex<Option<std::sync::mpsc::Sender<NewOperationAudit>>>,
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
}

struct OperationLog {
    entries: Vec<OperationAuditEntry>,
    writer: Option<OperationLogWriter>,
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
                audit: Mutex::new(Vec::new()),
                operations: Mutex::new(OperationLog {
                    entries: Vec::new(),
                    writer: None,
                }),
                next_id: AtomicU64::new(1),
                registry,
                request_timeout_ms: AtomicU64::new(DEFAULT_REQUEST_TIMEOUT_MS),
                audit_tx: Mutex::new(None),
                store_sql: AtomicBool::new(true),
                max_result_rows: AtomicUsize::new(DEFAULT_MAX_RESULT_ROWS),
                max_result_bytes: AtomicUsize::new(DEFAULT_MAX_RESULT_BYTES),
                driver_tasks: AtomicUsize::new(0),
                cursors: CursorRegistry::default(),
                schema_cache: SchemaCache::default(),
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
                audit: Mutex::new(Vec::new()),
                operations: Mutex::new(OperationLog {
                    entries,
                    writer: Some(writer),
                }),
                next_id: AtomicU64::new(1),
                registry,
                request_timeout_ms: AtomicU64::new(DEFAULT_REQUEST_TIMEOUT_MS),
                audit_tx: Mutex::new(None),
                store_sql: AtomicBool::new(true),
                max_result_rows: AtomicUsize::new(DEFAULT_MAX_RESULT_ROWS),
                max_result_bytes: AtomicUsize::new(DEFAULT_MAX_RESULT_BYTES),
                driver_tasks: AtomicUsize::new(0),
                cursors: CursorRegistry::default(),
                schema_cache: SchemaCache::default(),
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
        let (tx, rx) = std::sync::mpsc::channel::<NewOperationAudit>();
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
    async fn run_bounded<F, T>(&self, op: &'static str, fut: F) -> ApiResult<T>
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
        };
        let info = session.info();
        self.inner.sessions.insert(id, session);
        tracing::info!(session_id = %id, tag = ?req.tag, "session opened");
        info
    }

    pub fn list_sessions(&self) -> Vec<SessionInfo> {
        self.inner.sessions.iter().map(|s| s.info()).collect()
    }

    pub fn push_audit(&self, entry: AuditEntry) {
        const MAX_AUDIT_ROWS: usize = 10_000;
        let mut audit = self.inner.audit.lock().unwrap();
        audit.push(entry);
        if audit.len() > MAX_AUDIT_ROWS {
            let overflow = audit.len() - MAX_AUDIT_ROWS;
            audit.drain(0..overflow);
        }
    }

    pub fn list_audit(&self) -> Vec<AuditEntry> {
        self.inner.audit.lock().unwrap().clone()
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
        const MAX_OPERATION_ROWS: usize = 10_000;
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
        let writer = {
            let mut log = self.inner.operations.lock().unwrap();
            let writer = log.writer.as_ref().map(|writer| writer.tx.clone());
            log.entries.push(entry.clone());
            if log.entries.len() > MAX_OPERATION_ROWS {
                let overflow = log.entries.len() - MAX_OPERATION_ROWS;
                log.entries.drain(0..overflow);
            }
            writer
        };

        if let Some(writer) = writer {
            match writer.try_send(entry) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    tracing::error!("operation audit writer queue is full; dropping JSONL row");
                }
                Err(TrySendError::Disconnected(_)) => {
                    tracing::error!("operation audit writer is stopped; dropping JSONL row");
                }
            }
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
            // Hand off to the writer thread; the request path does not wait on
            // the SQLite write. Send only fails if the writer died, already
            // logged there.
            let _ = tx.send(record);
        }
    }

    pub fn list_operations(&self) -> Vec<OperationAuditEntry> {
        self.inner.operations.lock().unwrap().entries.clone()
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
            tokio::spawn(async move {
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

    pub async fn open_connection(
        &self,
        session_id: SessionId,
        engine: Engine,
        spec: ConnectionSpec,
    ) -> ApiResult<ConnectionInfo> {
        if !self.inner.sessions.contains_key(&session_id) {
            return Err(ApiError::SessionNotFound(session_id));
        }

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
                },
            );
            info
        };
        tracing::info!(session_id = %session_id, conn_id = %info.id, %engine, "connection opened");
        Ok(info)
    }

    pub async fn close_connection(
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
        for tx in txs {
            if let Err(error) = entry.driver.rollback(tx.handle).await {
                tracing::warn!(session_id = %session_id, conn_id = %conn_id, error = %error, "rollback during connection close failed");
            }
        }
        entry.driver.close(entry.handle).await?;
        tracing::info!(session_id = %session_id, conn_id = %conn_id, "connection closed");
        Ok(())
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
        let entry = self.get_conn_entry(session_id, conn_id)?;
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
        let cached = self.schema_cached(session_id, conn_id, scope).await?;
        Ok((*cached.snapshot).clone())
    }

    pub async fn schema_cached(
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
        let conn_id = req.connection;
        self.validate_execute_tx(session_id, conn_id, req.tx.as_ref())?;
        let exec = ExecuteRequest {
            sql: req.sql,
            params: req.params,
        };
        let entry = self.get_conn_entry(session_id, conn_id)?;
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
            let stream = driver.execute(handle, exec).await?;
            let cursor_id = stream.cursor_id;
            *slot.lock().unwrap() = Some(cursor_id);
            // Hand the driver stream to the registry pump. Eviction of
            // a co-tenant cursor happens via the on_evict callback.
            let wrapped = cursors.wrap(session_id, stream)?;
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
        let cancel = self.cancel(session_id, conn_id, cursor);
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
        self.validate_execute_tx(session_id, conn_id, tx)?;
        let entry = self.get_conn_entry(session_id, conn_id)?;
        let stream = entry.driver.execute(entry.handle.clone(), req).await?;
        // Hand the driver's stream to the registry-owned pump. Wrapping
        // enforces the per-session cap (evicting the LRA cursor of the
        // same session via the installed on_evict callback), spawns
        // the pump task, and returns a rebound stream whose `rows`
        // channel is fed by the pump.
        match self.inner.cursors.wrap(session_id, stream) {
            Ok(wrapped) => Ok(wrapped),
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

    /// Called by the WS ack loop after each ack to keep the cursor from
    /// looking idle to the eviction policy.
    pub fn cursor_touch(&self, cursor_id: CursorId) {
        self.inner.cursors.touch(cursor_id);
    }

    /// Called after a cursor terminates or is cancelled to drop its
    /// registry bookkeeping. Idempotent.
    pub fn cursor_remove(&self, cursor_id: CursorId) {
        self.inner.cursors.remove(cursor_id);
    }

    pub async fn listen_pg(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        channels: Vec<String>,
    ) -> ApiResult<NotificationStream> {
        let entry = self.get_conn_entry(session_id, conn_id)?;
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
        let entry = self.get_conn_entry(session_id, conn_id)?;
        entry.driver.cancel(entry.handle.clone(), cursor).await?;
        // Drop the registry entry so the per-session cap slot frees
        // up. Terminal-page cleanup calls `cursor_remove` on the same
        // path; this is idempotent.
        self.inner.cursors.remove(cursor);
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
        let entry = self.get_conn_entry(session_id, conn_id)?;
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
                mssql.bulk_insert(handle, BulkOp { table, data }).await
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
        let entry = self.get_conn_entry(session_id, req.connection)?;
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
        let tx = self.remove_tx(session_id, req.connection, req.tx_id)?;
        let entry = self.get_conn_entry(session_id, req.connection)?;
        let driver = entry.driver.clone();
        self.run_bounded("commit", async move { driver.commit(tx.handle).await })
            .await
    }

    pub async fn rollback_transaction(
        &self,
        session_id: SessionId,
        req: EndTransactionRequest,
    ) -> ApiResult<()> {
        let tx = self.remove_tx(session_id, req.connection, req.tx_id)?;
        let entry = self.get_conn_entry(session_id, req.connection)?;
        let driver = entry.driver.clone();
        self.run_bounded("rollback", async move { driver.rollback(tx.handle).await })
            .await
    }

    pub async fn create_savepoint(
        &self,
        session_id: SessionId,
        req: SavepointRequest,
    ) -> ApiResult<()> {
        let tx_handle = self.tx_handle_for(session_id, req.connection, req.tx_id)?;
        let entry = self.get_conn_entry(session_id, req.connection)?;
        let driver = entry.driver.clone();
        let name = req.name;
        self.run_bounded("savepoint", async move {
            match driver.engine() {
                Engine::Postgres => {
                    let pg = driver
                        .as_pg()
                        .ok_or_else(|| missing_ext(Engine::Postgres, "PgExt"))?;
                    pg.savepoint(&tx_handle, &name).await.map(|_| ())
                }
                Engine::SqlServer => {
                    let mssql = driver
                        .as_mssql()
                        .ok_or_else(|| missing_ext(Engine::SqlServer, "MssqlExt"))?;
                    mssql.savepoint(&tx_handle, &name).await.map(|_| ())
                }
            }
        })
        .await
    }

    pub async fn rollback_to_savepoint(
        &self,
        session_id: SessionId,
        req: SavepointRequest,
    ) -> ApiResult<()> {
        let tx_handle = self.tx_handle_for(session_id, req.connection, req.tx_id)?;
        let entry = self.get_conn_entry(session_id, req.connection)?;
        let driver = entry.driver.clone();
        let name = req.name;
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
        .await
    }

    pub async fn release_savepoint(
        &self,
        session_id: SessionId,
        req: SavepointRequest,
    ) -> ApiResult<()> {
        // Validate tx is active on the connection before dispatching.
        let tx_handle = self.tx_handle_for(session_id, req.connection, req.tx_id)?;
        let entry = self.get_conn_entry(session_id, req.connection)?;
        let driver = entry.driver.clone();
        let name = req.name;
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
        .await
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
        Ok(entry.handle.clone())
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
        let entry = self.get_conn_entry(session_id, conn_id)?;
        let driver = entry.driver.clone();
        let handle = entry.handle.clone();
        let result = crate::ddl::generate_ddl(&*driver, handle, object).await?;
        Ok(result)
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
        let entry = self.get_conn_entry(session_id, conn_id)?;
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
}

pub struct Session {
    pub id: SessionId,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub tag: Option<String>,
    pub owner_principal_id: Option<PrincipalId>,
    pub connections: DashMap<ConnectionId, ConnectionEntry>,
    pub transactions: DashMap<TxId, TransactionEntry>,
    pub next_conn_id: AtomicU64,
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
}

pub struct TransactionEntry {
    pub info: TransactionInfo,
    pub handle: TxHandle,
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

/// Build the `QueryTimedOut` driver error returned when a synchronous driver
/// call exceeds the configured per-request deadline.
fn timeout_error(op: &str) -> ApiError {
    ApiError::Driver(DriverError::new(
        Code::QueryTimedOut,
        format!("`{op}` exceeded the configured request timeout"),
    ))
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
