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
use sha2::{Digest, Sha256};
use sift_driver_api::{BulkOp, MssqlSavepoint, NotificationStream, PgSavepoint, ResultSetStream};
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
use crate::registry::{
    DriverRegistry, RuntimeConnectionHandle, RuntimeDriver, RuntimeTransactionHandle,
};
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
    tool_registry: RwLock<Option<crate::automation::GovernedToolRegistry>>,
    formatter_registry: crate::formatter_extension::FormatterRegistry,
    package_registry: RwLock<Option<Arc<sift_plugin_host::ExtensionPackageRegistry>>>,
    extension_generation_limiter: Arc<sift_plugin_host::GenerationLimiter>,
    extension_runtime_monitor: RwLock<crate::extension_runtime::ExtensionRuntimeMonitor>,
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
    /// Server-owned room connections (ADR-037). Each room lazily opens one
    /// managed connection under its binder's provenance; the async mutex both
    /// guards lazy open and serializes member queries onto the single
    /// connection.
    room_connections: DashMap<i64, Arc<tokio::sync::Mutex<Option<RoomConn>>>>,
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
    /// and cached with a TTL. Keyed by connection since
    /// search scope is the active connection.
    search_indexes: DashMap<ConnectionId, (Arc<crate::search::SearchIndex>, Instant)>,
    /// Process-local parsed SQL document state (ADR-032).
    semantic: sift_semantic::SemanticRegistry,
    /// Revision state for normalized catalog graphs. The graph payload itself
    /// lives in `SchemaCache`; this map makes equal content retain a stable
    /// revision and advances revisions only on normalized change (ADR-033).
    catalog_revisions: DashMap<String, CatalogRevisionState>,
    migration_plans: DashMap<sift_protocol::MigrationPlanId, StoredMigrationPlan>,
    migration_runs: DashMap<sift_protocol::MigrationRunId, sift_protocol::MigrationRun>,
    migration_cancellations:
        DashMap<sift_protocol::MigrationRunId, Arc<std::sync::atomic::AtomicBool>>,
    migration_locks: DashMap<(SessionId, ConnectionId), Arc<tokio::sync::Mutex<()>>>,
    retained_query_results: crate::comparison::RetainedQueryRegistry,
    comparisons: crate::comparison::ComparisonRegistry,
}

struct CatalogRevisionState {
    digest: String,
    revision: u64,
    invalidation_epoch: u64,
}

#[derive(Clone)]
struct StoredMigrationPlan {
    plan: sift_protocol::MigrationPlan,
    session: SessionId,
    connection: ConnectionId,
    principal: PrincipalId,
    tenant: sift_metadata::TenantId,
    profile: sift_metadata::ConnectionProfileId,
    policy_revision: u64,
    live_options: sift_protocol::CatalogGraphOptions,
}

pub(crate) struct MigrationPlanScope {
    pub session: SessionId,
    pub connection: ConnectionId,
    pub principal: PrincipalId,
    pub tenant: sift_metadata::TenantId,
    pub profile: sift_metadata::ConnectionProfileId,
    pub policy_revision: u64,
    pub live_options: sift_protocol::CatalogGraphOptions,
}

#[derive(Clone)]
struct LoadedComparisonSource {
    dataset: sift_core::comparison::ComparisonDataset,
    table: Option<LoadedComparisonTable>,
    connection: Option<ConnectionId>,
    truncated: bool,
}

#[derive(Clone)]
struct LoadedComparisonTable {
    connection: ConnectionId,
    revision: sift_protocol::CatalogRevision,
    path: sift_protocol::ObjectPath,
    object: sift_protocol::ObjectInfo,
    identity: Option<(Vec<String>, sift_protocol::CatalogObjectId)>,
}

/// A live server-owned room connection: a hidden session owned by the binder
/// holding one managed connection opened from the room's bound profile.
#[derive(Clone)]
struct RoomConn {
    session_id: SessionId,
    conn_id: ConnectionId,
}

/// Identity a room connection is opened under (ADR-037): the binder's
/// provenance plus the bound profile's engine and policy revision.
#[derive(Clone)]
pub struct RoomConnProvenance {
    pub room_id: i64,
    pub binder: PrincipalId,
    pub tenant: sift_metadata::TenantId,
    pub profile_id: sift_metadata::ConnectionProfileId,
    pub provider_id: sift_protocol::ProviderId,
    pub engine: Option<Engine>,
    pub policy_revision: u64,
}

pub struct RoomQueryExecution {
    pub response: ExecuteResponse,
    pub pages: Vec<Page>,
    pub retention_guards: Vec<crate::resources::ResourceGuard>,
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
                tool_registry: RwLock::new(None),
                formatter_registry: Default::default(),
                package_registry: RwLock::new(None),
                extension_generation_limiter: Arc::new(sift_plugin_host::GenerationLimiter::new(
                    Default::default(),
                )),
                extension_runtime_monitor: RwLock::new(Default::default()),
                managed_connections: DashMap::new(),
                room_connections: DashMap::new(),
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
                semantic: sift_semantic::SemanticRegistry::default(),
                catalog_revisions: DashMap::new(),
                migration_plans: DashMap::new(),
                migration_runs: DashMap::new(),
                migration_cancellations: DashMap::new(),
                migration_locks: DashMap::new(),
                retained_query_results: Default::default(),
                comparisons: Default::default(),
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
                tool_registry: RwLock::new(None),
                formatter_registry: Default::default(),
                package_registry: RwLock::new(None),
                extension_generation_limiter: Arc::new(sift_plugin_host::GenerationLimiter::new(
                    Default::default(),
                )),
                extension_runtime_monitor: RwLock::new(Default::default()),
                managed_connections: DashMap::new(),
                room_connections: DashMap::new(),
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
                semantic: sift_semantic::SemanticRegistry::default(),
                catalog_revisions: DashMap::new(),
                migration_plans: DashMap::new(),
                migration_runs: DashMap::new(),
                migration_cancellations: DashMap::new(),
                migration_locks: DashMap::new(),
                retained_query_results: Default::default(),
                comparisons: Default::default(),
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

    pub fn set_tool_registry(&self, registry: crate::automation::GovernedToolRegistry) {
        *self
            .inner
            .tool_registry
            .write()
            .expect("tool registry lock poisoned") = Some(registry);
    }

    pub fn tool_registry(&self) -> Option<crate::automation::GovernedToolRegistry> {
        self.inner
            .tool_registry
            .read()
            .expect("tool registry lock poisoned")
            .clone()
    }

    pub fn formatter_registry(&self) -> crate::formatter_extension::FormatterRegistry {
        self.inner.formatter_registry.clone()
    }

    pub fn set_package_registry(&self, registry: Arc<sift_plugin_host::ExtensionPackageRegistry>) {
        *self
            .inner
            .package_registry
            .write()
            .expect("package registry lock poisoned") = Some(registry);
    }

    pub fn package_registry(&self) -> Option<Arc<sift_plugin_host::ExtensionPackageRegistry>> {
        self.inner
            .package_registry
            .read()
            .expect("package registry lock poisoned")
            .clone()
    }

    pub async fn refresh_extension_runtimes(&self) -> ApiResult<()> {
        let packages = self
            .package_registry()
            .ok_or(ApiError::MetadataUnavailable)?;
        let metadata = self
            .inner
            .authorization_store
            .read()
            .unwrap()
            .clone()
            .ok_or(ApiError::MetadataUnavailable)?;
        let mut runtimes = crate::extension_runtime::installed_extension_runtimes(
            &packages,
            &metadata,
            self.inner.extension_generation_limiter.clone(),
        )?;
        let eager_failures = runtimes.start_eager().await;
        if !eager_failures.is_empty() {
            let mut seen = std::collections::HashSet::new();
            for (extension_id, error) in eager_failures {
                if !seen.insert(extension_id.clone()) {
                    continue;
                }
                let current = metadata.extension_selection(extension_id.as_str())?;
                metadata.update_extension_selection(sift_metadata::UpdateExtensionSelection {
                    extension_id: extension_id.as_str(),
                    selected_archive_sha256: Some(&current.selected_archive_sha256),
                    enabled: true,
                    lifecycle: sift_protocol::ExtensionLifecycleState::Quarantined,
                    isolation: current.isolation,
                    quarantine_reason: Some(&error),
                    expected_revision: current.revision,
                })?;
            }
            runtimes = crate::extension_runtime::installed_extension_runtimes(
                &packages,
                &metadata,
                self.inner.extension_generation_limiter.clone(),
            )?;
        }
        let monitor = runtimes.monitor.clone();
        self.inner
            .formatter_registry
            .replace(runtimes.formatters)
            .map_err(|error| ApiError::Internal(error.to_string()))?;
        if let Some(tools) = self.tool_registry() {
            tools
                .dispatcher()
                .replace(runtimes.actions)
                .map_err(|error| ApiError::Internal(error.to_string()))?;
            tools
                .replace(runtimes.tools)
                .map_err(|error| ApiError::Internal(error.to_string()))?;
        }
        self.inner
            .registry
            .providers()
            .replace_extensions(runtimes.providers)?;
        let previous = std::mem::replace(
            &mut *self
                .inner
                .extension_runtime_monitor
                .write()
                .expect("extension runtime monitor lock poisoned"),
            monitor,
        );
        let deadline = match self.request_timeout() {
            duration if duration.is_zero() => Duration::from_secs(30),
            duration => duration,
        };
        tokio::spawn(async move {
            previous.drain_and_shutdown(deadline).await;
        });
        Ok(())
    }

    pub async fn extension_runtime_diagnostics(
        &self,
        extension_id: &sift_extension_protocol::ExtensionId,
    ) -> (Option<String>, Vec<String>) {
        let monitor = self
            .inner
            .extension_runtime_monitor
            .read()
            .expect("extension runtime monitor lock poisoned")
            .clone();
        monitor.diagnostics(extension_id).await
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
        entry.driver.require_operation(operation)?;
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
        metadata
            .authorize_vault_connection_use(tenant_id, principal_id, profile_id)
            .map_err(|error| match error {
                sift_metadata::MetadataError::VaultPermissionDenied
                | sift_metadata::MetadataError::TenantMembershipRequired { .. } => {
                    ApiError::Forbidden("vault connection access required".into())
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
            entry.driver.semantic_engine(),
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
            return self.get_conn_entry(session_id, conn_id);
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

    fn retained_byte_context(
        &self,
        entry: &ConnectionEntryClone,
    ) -> Option<(crate::resources::ResourceManager, sift_metadata::TenantId)> {
        let ConnectionProvenance::Managed {
            tenant_id,
            quota_exempt,
            ..
        } = &entry.provenance
        else {
            return None;
        };
        if *quota_exempt {
            return None;
        }
        Some((self.resource_manager(), *tenant_id))
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
        self.open_session_with_owner(req, None, None, false)
            .expect("unowned local sessions do not reserve tenant resources")
    }

    pub fn open_session_with_owner(
        &self,
        req: OpenSessionRequest,
        owner_principal_id: Option<PrincipalId>,
        tenant_id: Option<sift_metadata::TenantId>,
        enforce_limits: bool,
    ) -> ApiResult<SessionInfo> {
        let resource_guard = match tenant_id {
            Some(tenant) if enforce_limits => Some(self.resource_manager().reserve(
                tenant,
                sift_protocol::TenantResource::Sessions,
                1,
            )?),
            _ => None,
        };
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
            tenant_id: Mutex::new(tenant_id),
            quota_exempt: AtomicBool::new(!enforce_limits),
            resource_guard: Mutex::new(resource_guard),
        };
        let info = session.info();
        self.inner.sessions.insert(id, session);
        tracing::info!(session_id = %id, tag = ?req.tag, "session opened");
        Ok(info)
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
        self.inner.semantic.close_session(id.0);
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

    pub fn reserve_session_retained_bytes(
        &self,
        id: SessionId,
        bytes: usize,
    ) -> ApiResult<Option<crate::resources::ResourceGuard>> {
        let session = self
            .inner
            .sessions
            .get(&id)
            .ok_or(ApiError::SessionNotFound(id))?;
        if session.quota_exempt.load(Ordering::Acquire) {
            return Ok(None);
        }
        let tenant = *session.tenant_id.lock().unwrap();
        tenant
            .map(|tenant| {
                self.resource_manager().reserve(
                    tenant,
                    sift_protocol::TenantResource::RetainedResultBytes,
                    bytes as u64,
                )
            })
            .transpose()
    }

    pub async fn open_connection(
        &self,
        session_id: SessionId,
        engine: Engine,
        spec: ConnectionSpec,
    ) -> ApiResult<ConnectionInfo> {
        self.open_provider_connection(session_id, engine.provider_id(), spec)
            .await
    }

    pub async fn open_provider_connection(
        &self,
        session_id: SessionId,
        provider_id: sift_protocol::ProviderId,
        mut spec: ConnectionSpec,
    ) -> ApiResult<ConnectionInfo> {
        let mut credentials = std::collections::HashMap::new();
        if let Some(password) = spec.password.take() {
            credentials.insert("password".to_string(), password.into_bytes());
        }
        let configuration =
            serde_json::to_value(spec).map_err(|error| ApiError::BadRequest(error.to_string()))?;
        self.open_provider_configuration(
            session_id,
            provider_id,
            configuration,
            credentials,
            ConnectionProvenance::TrustedLocal,
            None,
        )
        .await
    }

    async fn open_provider_configuration(
        &self,
        session_id: SessionId,
        provider_id: sift_protocol::ProviderId,
        configuration: serde_json::Value,
        credentials: std::collections::HashMap<String, Vec<u8>>,
        provenance: ConnectionProvenance,
        resource_guard: Option<crate::resources::ResourceGuard>,
    ) -> ApiResult<ConnectionInfo> {
        let registered = self.inner.registry.get_provider(&provider_id)?;
        let runtime = RuntimeDriver::from_registered(registered);
        runtime.require_capability("driver.core@1")?;
        let engine = runtime.semantic_engine();
        self.open_connection_with_provenance(
            session_id,
            engine,
            configuration,
            credentials,
            provenance,
            resource_guard,
            runtime,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn open_managed_connection(
        &self,
        session_id: SessionId,
        provider_id: sift_protocol::ProviderId,
        configuration: serde_json::Value,
        credentials: std::collections::HashMap<String, Vec<u8>>,
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
        self.open_provider_configuration(
            session_id,
            provider_id,
            configuration,
            credentials,
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

    #[allow(clippy::too_many_arguments)]
    async fn open_connection_with_provenance(
        &self,
        session_id: SessionId,
        engine: Option<Engine>,
        configuration: serde_json::Value,
        credentials: std::collections::HashMap<String, Vec<u8>>,
        provenance: ConnectionProvenance,
        resource_guard: Option<crate::resources::ResourceGuard>,
        driver: RuntimeDriver,
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
        let tenant_id = match &provenance {
            ConnectionProvenance::Managed { tenant_id, .. } => Some(tenant_id.0),
            ConnectionProvenance::TrustedLocal => None,
        };
        let handle = driver.open(&configuration, &credentials, tenant_id).await?;
        let info = {
            let Some(session) = self.inner.sessions.get(&session_id) else {
                driver.close(handle).await?;
                return Err(ApiError::SessionNotFound(session_id));
            };
            let id = ConnectionId(session.next_conn_id.fetch_add(1, Ordering::Relaxed));
            let display_name = display_name_for_configuration(&configuration, driver.provider());
            let info = ConnectionInfo {
                id,
                provider_id: driver.provider().provider_id.clone(),
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
                    configuration,
                    credentials,
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
        tracing::info!(
            session_id = %session_id,
            conn_id = %info.id,
            semantic_engine = ?engine,
            provider_id = %info.provider_id,
            "connection opened"
        );
        Ok(info)
    }

    /// Run a room-scoped query through the room's server-owned connection
    /// (ADR-037). Holding the per-room async mutex across the whole execute
    /// both guards the lazy open and serializes concurrent member queries onto
    /// the single connection. The submitter must already be authorized (the
    /// HTTP layer runs the submitter-scoped intersection before routing here).
    pub async fn execute_room_query(
        &self,
        provenance: RoomConnProvenance,
        req: ExecuteRequestHttp,
    ) -> ApiResult<RoomQueryExecution> {
        let slot = self
            .inner
            .room_connections
            .entry(provenance.room_id)
            .or_default()
            .clone();
        let mut guard = slot.lock().await;
        let room_conn = match guard
            .as_ref()
            .filter(|existing| self.room_connection_is_live(existing))
        {
            Some(existing) => existing.clone(),
            None => {
                if let Some(stale) = guard.take() {
                    let _ = self.close_session(stale.session_id);
                }
                let opened = self.open_room_connection(&provenance).await?;
                *guard = Some(opened.clone());
                opened
            }
        };
        let stream = match self
            .execute_stream(
                room_conn.session_id,
                room_conn.conn_id,
                ExecuteRequest {
                    sql: req.sql,
                    params: req.params,
                    transform: None,
                },
                None,
            )
            .await
        {
            Ok(stream) => stream,
            Err(error) => {
                if matches!(
                    &error,
                    ApiError::Driver(DriverError {
                        code: Code::ConnectionFailed,
                        ..
                    })
                ) {
                    guard.take();
                    let _ = self.close_session(room_conn.session_id);
                }
                return Err(error);
            }
        };
        let (max_rows, max_bytes) = self.result_limits();
        let cursor_id = stream.cursor_id;
        let duration = self.request_timeout();
        let drain = drain_room_stream(
            self,
            stream,
            max_rows,
            max_bytes,
            (self.resource_manager(), provenance.tenant),
        );
        let result = if duration.is_zero() {
            drain.await
        } else {
            match tokio::time::timeout(duration, drain).await {
                Ok(result) => result,
                Err(_) => {
                    self.cancel_after_timeout(room_conn.session_id, room_conn.conn_id, cursor_id)
                        .await;
                    Err(timeout_error("room execute"))
                }
            }
        };
        if matches!(
            &result,
            Err(ApiError::Driver(DriverError {
                code: Code::ConnectionFailed,
                ..
            }))
        ) {
            guard.take();
            let _ = self.close_session(room_conn.session_id);
        }
        result
    }

    fn room_connection_is_live(&self, room_conn: &RoomConn) -> bool {
        self.inner
            .sessions
            .get(&room_conn.session_id)
            .is_some_and(|session| session.connections.contains_key(&room_conn.conn_id))
    }

    /// Open the room's hidden session (owned by the binder) and its managed
    /// connection from the bound profile.
    async fn open_room_connection(&self, p: &RoomConnProvenance) -> ApiResult<RoomConn> {
        let metadata = self
            .inner
            .authorization_store
            .read()
            .unwrap()
            .clone()
            .ok_or(ApiError::MetadataUnavailable)?;
        let (configuration, credentials) = metadata
            .resolve_provider_connection(p.tenant, p.binder, p.profile_id)
            .await?;
        let session = self.open_session_with_owner(
            OpenSessionRequest {
                tag: Some(format!("room:{}", p.room_id)),
                tenant_id: Some(p.tenant.0),
            },
            Some(p.binder),
            Some(p.tenant),
            false,
        )?;
        let info = match self
            .open_managed_connection(
                session.id,
                p.provider_id.clone(),
                configuration,
                credentials,
                p.binder,
                p.tenant,
                p.profile_id,
                p.policy_revision,
                false,
            )
            .await
        {
            Ok(info) => info,
            Err(error) => {
                let _ = self.close_session(session.id);
                return Err(error);
            }
        };
        Ok(RoomConn {
            session_id: session.id,
            conn_id: info.id,
        })
    }

    /// Tear down a room's server-owned connection, if any (on unbind, room
    /// emptiness, or revocation). Closing the session closes its connection.
    pub async fn close_room_connection(&self, room_id: i64) {
        let Some((_, slot)) = self.inner.room_connections.remove(&room_id) else {
            return;
        };
        let mut guard = slot.lock().await;
        if let Some(room_conn) = guard.take() {
            if let Err(error) = self.close_session(room_conn.session_id) {
                tracing::warn!(room_id, %error, "closing room connection session failed");
            }
        }
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
            session.quota_exempt.store(false, Ordering::Release);
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
        self.inner
            .semantic
            .close_scope(sift_semantic::DocumentScope {
                session: session_id.0,
                connection: conn_id.0,
            });
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
            sift_protocol::SchemaDepth::Graph { .. } => Vec::new(),
        };
        let entry = self.authorize_connection_operation(
            session_id,
            conn_id,
            sift_protocol::OperationKind::RefreshSchema,
            None,
            &objects,
        )?;
        let capability = match scope.depth {
            sift_protocol::SchemaDepth::Shallow => "driver.schema.shallow@1",
            sift_protocol::SchemaDepth::Deep { .. } => "driver.schema.deep@1",
            sift_protocol::SchemaDepth::Graph { .. } => "driver.schema.graph@1",
        };
        entry.driver.require_capability(capability)?;
        let cached = self.schema_cached(session_id, conn_id, scope).await?;
        Ok((*cached.snapshot).clone())
    }

    pub async fn catalog_graph(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        request: sift_protocol::CatalogGraphRequest,
    ) -> ApiResult<sift_protocol::CatalogGraph> {
        self.catalog_graph_for_operation(
            session_id,
            conn_id,
            request,
            sift_protocol::OperationKind::ReadCatalogGraph,
        )
        .await
    }

    async fn catalog_graph_for_operation(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        request: sift_protocol::CatalogGraphRequest,
        operation: sift_protocol::OperationKind,
    ) -> ApiResult<sift_protocol::CatalogGraph> {
        const MAX_GRAPH_NODES: usize = 100_000;
        const MAX_GRAPH_EDGES: usize = 500_000;

        if request.options.max_nodes == Some(0)
            || request
                .options
                .max_nodes
                .is_some_and(|limit| limit as usize > MAX_GRAPH_NODES)
        {
            return Err(ApiError::BadRequest(format!(
                "catalog max_nodes must be between 1 and {MAX_GRAPH_NODES}"
            )));
        }
        if request.options.schemas.as_ref().is_some_and(|schemas| {
            schemas.is_empty()
                || schemas.len() > 256
                || schemas
                    .iter()
                    .any(|schema| schema.is_empty() || schema.len() > 256)
        }) {
            return Err(ApiError::BadRequest(
                "catalog schema filters must contain between 1 and 256 bounded names".into(),
            ));
        }
        if request
            .options
            .kinds
            .as_ref()
            .is_some_and(|kinds| kinds.is_empty() || kinds.len() > 32)
        {
            return Err(ApiError::BadRequest(
                "catalog kinds must contain between 1 and 32 entries".into(),
            ));
        }

        let authorized =
            self.authorize_connection_operation(session_id, conn_id, operation, None, &[])?;
        authorized
            .driver
            .require_capability("driver.schema.graph@1")?;
        if request.refresh {
            if let Some(spec) = self.spec_for_conn(session_id, conn_id)? {
                self.inner.schema_cache.invalidate_spec(&spec);
            }
        }
        let mut options = request.options;
        if let Some(kinds) = &mut options.kinds {
            kinds.sort_unstable();
            kinds.dedup();
        }
        if let Some(schemas) = &mut options.schemas {
            schemas.sort();
            schemas.dedup();
        }
        let scope = SchemaScope {
            depth: sift_protocol::SchemaDepth::Graph { options },
            filter: None,
        };
        let cached = self
            .schema_cached_unfiltered(session_id, conn_id, scope.clone())
            .await?;
        let mut canonical_snapshot = (*cached.snapshot).clone();
        let canonical_data = canonical_snapshot.graph.as_mut().ok_or_else(|| {
            ApiError::Driver(DriverError::new(
                Code::DriverInternal,
                "graph-capable provider returned no catalog graph",
            ))
        })?;
        sift_core::catalog::normalize_graph(canonical_data);
        sift_core::catalog::validate_graph(canonical_data, MAX_GRAPH_NODES, MAX_GRAPH_EDGES)
            .map_err(|error| {
                ApiError::Driver(DriverError::new(
                    Code::DriverInternal,
                    format!("invalid provider catalog graph: {error}"),
                ))
            })?;
        let serialized = serde_json::to_vec(canonical_data)
            .map_err(|error| ApiError::Internal(format!("serialize catalog graph: {error}")))?;
        let canonical_digest = digest_bytes("catfp:", &serialized);
        let entry = self.get_conn_entry(session_id, conn_id)?;
        let provider = entry.driver.provider().clone();
        let database_identity = digest_bytes(
            "dbfp:",
            &serde_json::to_vec(&(provider.clone(), &entry.configuration)).map_err(|error| {
                ApiError::Internal(format!("serialize database identity: {error}"))
            })?,
        );
        let revision_key = digest_bytes(
            "catrev:",
            &serde_json::to_vec(&(&database_identity, &scope)).map_err(|error| {
                ApiError::Internal(format!("serialize catalog revision identity: {error}"))
            })?,
        );
        let observed_epoch = self
            .spec_for_conn(session_id, conn_id)?
            .as_ref()
            .map(|spec| self.inner.schema_cache.invalidation_epoch(spec))
            .unwrap_or(1)
            .max(1);
        let (revision, invalidation_epoch) = match self.inner.catalog_revisions.entry(revision_key)
        {
            dashmap::mapref::entry::Entry::Occupied(mut occupied) => {
                let state = occupied.get_mut();
                if state.digest != canonical_digest {
                    state.digest.clone_from(&canonical_digest);
                    state.revision = state.revision.checked_add(1).unwrap_or(1);
                }
                state.invalidation_epoch = state.invalidation_epoch.max(observed_epoch);
                (state.revision, state.invalidation_epoch)
            }
            dashmap::mapref::entry::Entry::Vacant(vacant) => {
                vacant.insert(CatalogRevisionState {
                    digest: canonical_digest,
                    revision: 1,
                    invalidation_epoch: observed_epoch,
                });
                (1, observed_epoch)
            }
        };
        let mut visible_snapshot = canonical_snapshot;
        if let Some(policy) = self.current_connection_policy(session_id, conn_id)? {
            if policy.allowed_schemas.is_some() {
                crate::sql_policy::filter_snapshot(&policy, &mut visible_snapshot);
            }
        }
        let data = visible_snapshot.graph.ok_or_else(|| {
            ApiError::Internal("catalog graph disappeared during policy projection".into())
        })?;
        sift_core::catalog::validate_graph(&data, MAX_GRAPH_NODES, MAX_GRAPH_EDGES).map_err(
            |error| ApiError::Internal(format!("invalid policy-filtered catalog graph: {error}")),
        )?;
        let content_digest = digest_bytes(
            "catfp:",
            &serde_json::to_vec(&data).map_err(|error| {
                ApiError::Internal(format!("serialize visible catalog graph: {error}"))
            })?,
        );
        Ok(sift_protocol::CatalogGraph {
            revision: sift_protocol::CatalogRevision(revision),
            content_digest,
            invalidation_epoch,
            captured_at: visible_snapshot.fetched_at,
            provider,
            database_identity,
            data,
        })
    }

    pub async fn catalog_diagram(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        request: sift_protocol::CatalogDiagramRequest,
    ) -> ApiResult<sift_protocol::CatalogDiagram> {
        const MAX_DIAGRAM_NODES: usize = 20_000;
        if request.schemas.len() > 256
            || request
                .schemas
                .iter()
                .any(|schema| schema.is_empty() || schema.len() > 256)
            || request.object_ids.len() > 10_000
            || request.edge_kinds.len() > 32
            || request.neighborhood_depth > 8
        {
            return Err(ApiError::BadRequest(
                "catalog diagram selection exceeds request limits".into(),
            ));
        }
        let catalog = self
            .catalog_graph_for_operation(
                session_id,
                conn_id,
                sift_protocol::CatalogGraphRequest {
                    options: sift_protocol::CatalogGraphOptions {
                        schemas: (!request.schemas.is_empty()).then(|| request.schemas.clone()),
                        ..sift_protocol::CatalogGraphOptions::default()
                    },
                    refresh: false,
                },
                sift_protocol::OperationKind::ProjectCatalogDiagram,
            )
            .await?;
        if catalog.revision != request.expected_revision {
            return Err(ApiError::BadRequest(format!(
                "stale catalog revision: expected {}, current {}",
                request.expected_revision.0, catalog.revision.0
            )));
        }
        sift_core::catalog::project_diagram(&catalog, &request, MAX_DIAGRAM_NODES).map_err(
            |error| ApiError::BadRequest(format!("invalid catalog diagram request: {error}")),
        )
    }

    pub async fn catalog_snapshot_source(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        request: &sift_protocol::CreateCatalogSnapshotRequest,
    ) -> ApiResult<(
        sift_protocol::CatalogGraph,
        PrincipalId,
        sift_metadata::TenantId,
        sift_metadata::ConnectionProfileId,
    )> {
        let catalog = self
            .catalog_graph_for_operation(
                session_id,
                conn_id,
                sift_protocol::CatalogGraphRequest {
                    options: request.options.clone(),
                    refresh: false,
                },
                sift_protocol::OperationKind::CreateCatalogSnapshot,
            )
            .await?;
        if catalog.revision != request.expected_catalog_revision {
            return Err(ApiError::BadRequest(format!(
                "stale catalog revision: expected {}, current {}",
                request.expected_catalog_revision.0, catalog.revision.0
            )));
        }
        if !request.accept_partial
            && catalog.data.coverage.state != sift_protocol::CatalogCoverageState::Complete
        {
            return Err(ApiError::BadRequest(
                "catalog snapshot requires complete coverage unless accept_partial is true".into(),
            ));
        }
        let provenance = self.get_conn_entry(session_id, conn_id)?.provenance;
        let ConnectionProvenance::Managed {
            principal_id,
            tenant_id,
            profile_id,
            ..
        } = provenance
        else {
            return Err(ApiError::Forbidden(
                "durable catalog snapshots require a managed connection profile".into(),
            ));
        };
        Ok((catalog, principal_id, tenant_id, profile_id))
    }

    pub fn managed_catalog_scope(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        operation: sift_protocol::OperationKind,
    ) -> ApiResult<(
        PrincipalId,
        sift_metadata::TenantId,
        sift_metadata::ConnectionProfileId,
        u64,
    )> {
        let entry =
            self.authorize_connection_operation(session_id, conn_id, operation, None, &[])?;
        let ConnectionProvenance::Managed {
            principal_id,
            tenant_id,
            profile_id,
            policy_revision,
            ..
        } = entry.provenance
        else {
            return Err(ApiError::Forbidden(
                "this catalog operation requires a managed connection profile".into(),
            ));
        };
        Ok((principal_id, tenant_id, profile_id, policy_revision))
    }

    pub async fn catalog_graph_for_schema_diff(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        expected_revision: sift_protocol::CatalogRevision,
        options: sift_protocol::CatalogGraphOptions,
    ) -> ApiResult<sift_protocol::CatalogGraph> {
        self.catalog_graph_at_revision(
            session_id,
            conn_id,
            expected_revision,
            options,
            sift_protocol::OperationKind::CompareCatalogSchemas,
        )
        .await
    }

    async fn catalog_graph_at_revision(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        expected_revision: sift_protocol::CatalogRevision,
        options: sift_protocol::CatalogGraphOptions,
        operation: sift_protocol::OperationKind,
    ) -> ApiResult<sift_protocol::CatalogGraph> {
        let graph = self
            .catalog_graph_for_operation(
                session_id,
                conn_id,
                sift_protocol::CatalogGraphRequest {
                    options,
                    refresh: false,
                },
                operation,
            )
            .await?;
        if graph.revision != expected_revision {
            return Err(ApiError::BadRequest(format!(
                "stale catalog revision: expected {}, current {}",
                expected_revision.0, graph.revision.0
            )));
        }
        Ok(graph)
    }

    pub(crate) fn store_migration_plan(
        &self,
        plan: sift_protocol::MigrationPlan,
        scope: MigrationPlanScope,
    ) -> ApiResult<sift_protocol::MigrationPlan> {
        let now = chrono::Utc::now();
        self.inner
            .migration_plans
            .retain(|_, stored| stored.plan.expires_at > now);
        if self.inner.migration_plans.len() >= 1_024 {
            return Err(ApiError::BadRequest(
                "migration plan retention limit reached".into(),
            ));
        }
        self.inner.migration_plans.insert(
            plan.id,
            StoredMigrationPlan {
                plan: plan.clone(),
                session: scope.session,
                connection: scope.connection,
                principal: scope.principal,
                tenant: scope.tenant,
                profile: scope.profile,
                policy_revision: scope.policy_revision,
                live_options: scope.live_options,
            },
        );
        Ok(plan)
    }

    pub async fn validate_migration(
        &self,
        session: SessionId,
        connection: ConnectionId,
        principal: PrincipalId,
        request: sift_protocol::ValidateMigrationRequest,
    ) -> ApiResult<sift_protocol::MigrationValidation> {
        use sift_protocol::{MigrationStatementOutcome, MigrationStatementStatus};

        if !request.confirm_test_database {
            return Err(ApiError::BadRequest(
                "explicitly confirm the selected connection is a test database".into(),
            ));
        }
        let (current_principal, tenant, profile, policy_revision) = self.managed_catalog_scope(
            session,
            connection,
            sift_protocol::OperationKind::PreviewMigration,
        )?;
        if current_principal != principal {
            return Err(ApiError::Forbidden(
                "migration caller must own the managed session".into(),
            ));
        }
        let stored = self
            .inner
            .migration_plans
            .get(&request.plan_id)
            .map(|entry| entry.clone())
            .ok_or_else(|| ApiError::BadRequest("migration plan not found or expired".into()))?;
        if stored.session != session
            || stored.connection != connection
            || stored.principal != principal
            || stored.tenant != tenant
            || stored.profile != profile
            || stored.policy_revision != policy_revision
            || stored.plan.digest != request.plan_digest
            || stored.plan.expires_at <= chrono::Utc::now()
        {
            return Err(ApiError::BadRequest(
                "migration plan is expired, tampered, or bound to another scope".into(),
            ));
        }
        if stored.plan.groups.iter().any(|group| !group.transactional) {
            return Err(ApiError::BadRequest(
                "this plan contains non-transactional DDL and cannot be safely test-rolled-back"
                    .into(),
            ));
        }
        let lock = self
            .inner
            .migration_locks
            .entry((session, connection))
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let _guard = lock.lock().await;
        self.catalog_graph_at_revision(
            session,
            connection,
            stored.plan.expected_live_revision,
            stored.live_options.clone(),
            sift_protocol::OperationKind::PreviewMigration,
        )
        .await?;
        let transaction = self
            .begin_transaction_as(
                session,
                sift_protocol::BeginTransactionRequest {
                    connection,
                    mode: sift_protocol::TxMode::default(),
                },
                sift_protocol::OperationKind::PreviewMigration,
            )
            .await?;
        let tx = sift_protocol::TxHandleRef {
            tx_id: transaction.tx_id,
            connection: transaction.connection,
            mode: transaction.mode,
        };
        let mut outcomes = Vec::new();
        let mut valid = true;
        'groups: for group in &stored.plan.groups {
            for statement in &group.statements {
                match self
                    .execute_http_as(
                        session,
                        ExecuteRequestHttp {
                            connection,
                            sql: statement.sql.clone(),
                            params: Vec::new(),
                            tx: Some(tx.clone()),
                            room_id: None,
                            connection_profile_id: Some(profile.0),
                            transform: None,
                            source: None,
                        },
                        sift_protocol::OperationKind::PreviewMigration,
                    )
                    .await
                {
                    Ok(response) => outcomes.push(MigrationStatementOutcome {
                        group_ordinal: group.ordinal,
                        statement_ordinal: statement.ordinal,
                        fingerprint: statement.fingerprint.clone(),
                        status: MigrationStatementStatus::Applied,
                        affected_rows: response.affected_rows,
                        result_code: None,
                    }),
                    Err(error) => {
                        valid = false;
                        outcomes.push(MigrationStatementOutcome {
                            group_ordinal: group.ordinal,
                            statement_ordinal: statement.ordinal,
                            fingerprint: statement.fingerprint.clone(),
                            status: MigrationStatementStatus::Failed,
                            affected_rows: None,
                            result_code: migration_result_code(&error),
                        });
                        break 'groups;
                    }
                }
            }
        }
        let rolled_back = self
            .rollback_migration_tx(session, connection, tx.tx_id)
            .await;
        if !rolled_back {
            return Err(ApiError::Internal(
                "test migration validation rollback failed; database outcome is unknown".into(),
            ));
        }
        for outcome in &mut outcomes {
            if outcome.status == MigrationStatementStatus::Applied {
                outcome.status = MigrationStatementStatus::RolledBack;
            }
        }
        Ok(sift_protocol::MigrationValidation {
            plan_id: stored.plan.id,
            valid,
            rolled_back,
            outcomes,
        })
    }

    pub async fn apply_migration(
        &self,
        session: SessionId,
        connection: ConnectionId,
        principal: PrincipalId,
        request: sift_protocol::ApplyMigrationRequest,
    ) -> ApiResult<sift_protocol::MigrationRun> {
        use sift_protocol::{
            MigrationRun, MigrationRunState, MigrationStatementOutcome, MigrationStatementStatus,
        };

        let (current_principal, tenant, profile, policy_revision) = self.managed_catalog_scope(
            session,
            connection,
            sift_protocol::OperationKind::ApplyMigration,
        )?;
        if current_principal != principal {
            return Err(ApiError::Forbidden(
                "migration caller must own the managed session".into(),
            ));
        }
        let stored = self
            .inner
            .migration_plans
            .get(&request.plan_id)
            .map(|entry| entry.clone())
            .ok_or_else(|| {
                ApiError::BadRequest("migration plan not found or already used".into())
            })?;
        if stored.session != session
            || stored.connection != connection
            || stored.principal != principal
            || stored.tenant != tenant
            || stored.profile != profile
            || stored.policy_revision != policy_revision
            || stored.plan.digest != request.plan_digest
            || stored.plan.expires_at <= chrono::Utc::now()
        {
            return Err(ApiError::BadRequest(
                "migration plan is expired, tampered, or bound to another scope".into(),
            ));
        }
        let acknowledgements = request
            .acknowledgements
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        if stored
            .plan
            .required_acknowledgements
            .iter()
            .any(|risk| !acknowledgements.contains(risk))
        {
            return Err(ApiError::BadRequest(
                "migration requires acknowledgement of every destructive or unknown risk".into(),
            ));
        }
        // Consume only after every caller-controlled precondition succeeds.
        // Removal remains the atomic one-use gate when two applies race.
        let (_, stored) = self
            .inner
            .migration_plans
            .remove(&request.plan_id)
            .ok_or_else(|| {
                ApiError::BadRequest("migration plan not found or already used".into())
            })?;

        let cancel = Arc::new(AtomicBool::new(false));
        self.inner
            .migration_cancellations
            .insert(stored.plan.run_id, cancel.clone());
        let started_at = chrono::Utc::now();
        let mut run = MigrationRun {
            id: stored.plan.run_id,
            plan_id: stored.plan.id,
            session,
            connection,
            plan_digest: stored.plan.digest.clone(),
            state: MigrationRunState::Running,
            started_at,
            finished_at: None,
            outcomes: Vec::new(),
            resulting_catalog_revision: None,
        };
        self.inner.migration_runs.insert(run.id, run.clone());
        let lock = self
            .inner
            .migration_locks
            .entry((session, connection))
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let _guard = lock.lock().await;

        // The revision check happens under the per-connection migration lock,
        // immediately before the first statement.
        if let Err(error) = self
            .catalog_graph_at_revision(
                session,
                connection,
                stored.plan.expected_live_revision,
                stored.live_options.clone(),
                sift_protocol::OperationKind::ApplyMigration,
            )
            .await
        {
            self.inner.migration_cancellations.remove(&run.id);
            self.inner.migration_runs.remove(&run.id);
            return Err(error);
        }
        if let Err(error) = self
            .persist_migration_run(tenant, profile, principal, &run)
            .await
        {
            self.inner.migration_cancellations.remove(&run.id);
            self.inner.migration_runs.remove(&run.id);
            return Err(error);
        }

        let mut attempted_ddl = false;
        let mut committed_work = false;
        'groups: for group in &stored.plan.groups {
            if cancel.load(Ordering::Acquire) {
                run.state = if committed_work {
                    MigrationRunState::Partial
                } else {
                    MigrationRunState::Canceled
                };
                break;
            }
            let tx = if group.transactional {
                match self
                    .begin_transaction_as(
                        session,
                        sift_protocol::BeginTransactionRequest {
                            connection,
                            mode: sift_protocol::TxMode::default(),
                        },
                        sift_protocol::OperationKind::ApplyMigration,
                    )
                    .await
                {
                    Ok(info) => Some(sift_protocol::TxHandleRef {
                        tx_id: info.tx_id,
                        connection: info.connection,
                        mode: info.mode,
                    }),
                    Err(_) => {
                        run.state = if committed_work {
                            MigrationRunState::Partial
                        } else {
                            MigrationRunState::Failed
                        };
                        break;
                    }
                }
            } else {
                None
            };
            let group_outcome_start = run.outcomes.len();
            for statement in &group.statements {
                if cancel.load(Ordering::Acquire) {
                    if let Some(tx) = &tx {
                        if self
                            .rollback_migration_tx(session, connection, tx.tx_id)
                            .await
                        {
                            mark_rolled_back(&mut run.outcomes[group_outcome_start..]);
                        } else {
                            run.state = MigrationRunState::Partial;
                        }
                    }
                    if run.state != MigrationRunState::Partial {
                        run.state = if committed_work {
                            MigrationRunState::Partial
                        } else {
                            MigrationRunState::Canceled
                        };
                    }
                    break 'groups;
                }
                attempted_ddl = true;
                let response = self
                    .execute_http_as(
                        session,
                        ExecuteRequestHttp {
                            connection,
                            sql: statement.sql.clone(),
                            params: Vec::new(),
                            tx: tx.clone(),
                            room_id: None,
                            connection_profile_id: Some(profile.0),
                            transform: None,
                            source: None,
                        },
                        sift_protocol::OperationKind::ApplyMigration,
                    )
                    .await;
                match response {
                    Ok(response) => {
                        run.outcomes.push(MigrationStatementOutcome {
                            group_ordinal: group.ordinal,
                            statement_ordinal: statement.ordinal,
                            fingerprint: statement.fingerprint.clone(),
                            status: MigrationStatementStatus::Applied,
                            affected_rows: response.affected_rows,
                            result_code: None,
                        });
                        if tx.is_none() {
                            committed_work = true;
                        }
                    }
                    Err(error) => {
                        run.outcomes.push(MigrationStatementOutcome {
                            group_ordinal: group.ordinal,
                            statement_ordinal: statement.ordinal,
                            fingerprint: statement.fingerprint.clone(),
                            status: MigrationStatementStatus::Failed,
                            affected_rows: None,
                            result_code: migration_result_code(&error),
                        });
                        if let Some(tx) = &tx {
                            if self
                                .rollback_migration_tx(session, connection, tx.tx_id)
                                .await
                            {
                                mark_rolled_back(&mut run.outcomes[group_outcome_start..]);
                                run.state = if committed_work {
                                    MigrationRunState::Partial
                                } else {
                                    MigrationRunState::RolledBack
                                };
                            } else {
                                run.state = MigrationRunState::Partial;
                            }
                        } else {
                            run.state = if committed_work {
                                MigrationRunState::Partial
                            } else {
                                MigrationRunState::Failed
                            };
                        }
                        break 'groups;
                    }
                }
            }
            if let Some(tx) = tx {
                if let Err(error) = self
                    .commit_transaction_as(
                        session,
                        sift_protocol::EndTransactionRequest {
                            connection,
                            tx_id: tx.tx_id,
                        },
                        sift_protocol::OperationKind::ApplyMigration,
                    )
                    .await
                {
                    if let Some(last) = run.outcomes.last_mut() {
                        last.result_code = migration_result_code(&error);
                    }
                    if self
                        .rollback_migration_tx(session, connection, tx.tx_id)
                        .await
                    {
                        mark_rolled_back(&mut run.outcomes[group_outcome_start..]);
                        run.state = if committed_work {
                            MigrationRunState::Partial
                        } else {
                            MigrationRunState::RolledBack
                        };
                    } else {
                        run.state = MigrationRunState::Partial;
                    }
                    break;
                }
                if run.outcomes[group_outcome_start..]
                    .iter()
                    .any(|outcome| outcome.status == MigrationStatementStatus::Applied)
                {
                    committed_work = true;
                }
            }
        }
        if run.state == MigrationRunState::Running {
            run.state = MigrationRunState::Applied;
        }
        let recorded = run
            .outcomes
            .iter()
            .map(|outcome| (outcome.group_ordinal, outcome.statement_ordinal))
            .collect::<std::collections::HashSet<_>>();
        for group in &stored.plan.groups {
            for statement in &group.statements {
                if !recorded.contains(&(group.ordinal, statement.ordinal)) {
                    run.outcomes.push(MigrationStatementOutcome {
                        group_ordinal: group.ordinal,
                        statement_ordinal: statement.ordinal,
                        fingerprint: statement.fingerprint.clone(),
                        status: MigrationStatementStatus::Skipped,
                        affected_rows: None,
                        result_code: None,
                    });
                }
            }
        }
        run.outcomes
            .sort_by_key(|outcome| (outcome.group_ordinal, outcome.statement_ordinal));
        if attempted_ddl {
            if let Some(spec) = self.spec_for_conn(session, connection)? {
                self.inner.schema_cache.invalidate_spec(&spec);
            }
            if let Ok(graph) = self
                .catalog_graph_for_operation(
                    session,
                    connection,
                    sift_protocol::CatalogGraphRequest {
                        options: stored.live_options,
                        refresh: false,
                    },
                    sift_protocol::OperationKind::ApplyMigration,
                )
                .await
            {
                run.resulting_catalog_revision = Some(graph.revision);
            }
        }
        run.finished_at = Some(chrono::Utc::now());
        self.inner.migration_runs.insert(run.id, run.clone());
        self.persist_migration_run(tenant, profile, principal, &run)
            .await?;
        self.inner.migration_cancellations.remove(&run.id);
        Ok(run)
    }

    async fn persist_migration_run(
        &self,
        tenant: sift_metadata::TenantId,
        profile: sift_metadata::ConnectionProfileId,
        principal: PrincipalId,
        run: &sift_protocol::MigrationRun,
    ) -> ApiResult<()> {
        let metadata = self
            .inner
            .authorization_store
            .read()
            .unwrap()
            .clone()
            .ok_or(ApiError::MetadataUnavailable)?;
        let run = run.clone();
        tokio::task::spawn_blocking(move || {
            metadata.put_migration_run(tenant, profile, principal, &run)
        })
        .await
        .map_err(|error| {
            ApiError::Internal(format!("migration run persistence task: {error}"))
        })??;
        Ok(())
    }

    async fn rollback_migration_tx(
        &self,
        session: SessionId,
        connection: ConnectionId,
        tx_id: TxId,
    ) -> bool {
        match self
            .rollback_transaction_as(
                session,
                sift_protocol::EndTransactionRequest { connection, tx_id },
                sift_protocol::OperationKind::ApplyMigration,
            )
            .await
        {
            Ok(()) => true,
            Err(error) => {
                tracing::error!(%session, %connection, %error, "migration rollback failed");
                false
            }
        }
    }

    pub fn migration_run(
        &self,
        session: SessionId,
        connection: ConnectionId,
        principal: PrincipalId,
        run_id: sift_protocol::MigrationRunId,
    ) -> ApiResult<sift_protocol::MigrationRun> {
        let (owner, _, _, _) = self.managed_catalog_scope(
            session,
            connection,
            sift_protocol::OperationKind::GetMigrationRun,
        )?;
        if owner != principal {
            return Err(ApiError::Forbidden(
                "migration run caller must own the managed session".into(),
            ));
        }
        self.inner
            .migration_runs
            .get(&run_id)
            .filter(|run| run.session == session && run.connection == connection)
            .map(|run| run.clone())
            .ok_or_else(|| ApiError::BadRequest("migration run not found".into()))
    }

    pub fn cancel_migration(
        &self,
        session: SessionId,
        connection: ConnectionId,
        principal: PrincipalId,
        run_id: sift_protocol::MigrationRunId,
    ) -> ApiResult<()> {
        let (owner, _, _, _) = self.managed_catalog_scope(
            session,
            connection,
            sift_protocol::OperationKind::CancelMigration,
        )?;
        if owner != principal {
            return Err(ApiError::Forbidden(
                "migration caller must own the managed session".into(),
            ));
        }
        let cancellation = self
            .inner
            .migration_cancellations
            .get(&run_id)
            .filter(|_| {
                self.inner
                    .migration_runs
                    .get(&run_id)
                    .is_some_and(|run| run.session == session && run.connection == connection)
            })
            .ok_or_else(|| ApiError::BadRequest("migration run is not active".into()))?;
        cancellation.store(true, Ordering::Release);
        Ok(())
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
        let cache_spec = self.spec_for_conn(session_id, conn_id)?;
        // Cache lookup: return immediately if a fresh snapshot exists
        // for this (spec, scope).
        if let Some(spec) = cache_spec.as_ref() {
            if let Some(cached) = self.inner.schema_cache.get_cached(spec, &scope) {
                return Ok(cached);
            }
        }
        let fetch_gate = cache_spec
            .as_ref()
            .and_then(|spec| self.inner.schema_cache.fetch_gate(spec, &scope).ok());
        let _fetch_guard = match fetch_gate.as_ref() {
            Some(gate) => Some(gate.lock().await),
            None => None,
        };
        if let Some(spec) = cache_spec.as_ref() {
            if let Some(cached) = self.inner.schema_cache.get_cached(spec, &scope) {
                return Ok(cached);
            }
        }
        let stale = cache_spec
            .as_ref()
            .and_then(|spec| self.inner.schema_cache.get_stale_cached(spec, &scope));
        let starting_epoch = cache_spec
            .as_ref()
            .map(|spec| self.inner.schema_cache.invalidation_epoch(spec));
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
        let invalidated_during_fetch =
            cache_spec
                .as_ref()
                .zip(starting_epoch)
                .is_some_and(|(spec, starting)| {
                    self.inner.schema_cache.invalidation_epoch(spec) != starting
                });
        let result = if invalidated_during_fetch {
            Err(ApiError::BadRequest(
                "schema invalidated while provider build was in flight; retry".into(),
            ))
        } else {
            result
        };
        if let (Ok(snapshot), Some(legacy), Some(spec)) =
            (&result, driver.legacy_driver(), cache_spec.as_ref())
        {
            if let Some(cached) =
                self.inner
                    .schema_cache
                    .insert(spec, &scope, snapshot.clone(), legacy.clone())
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
        match (result, stale) {
            (Ok(snapshot), _) => Ok(CachedSchema::new_uncached(snapshot)),
            (Err(_), Some(stale)) => Ok(stale_schema(stale.snapshot.as_ref())),
            (Err(error), None) => Err(error),
        }
    }

    fn spec_for_conn(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
    ) -> ApiResult<Option<ConnectionSpec>> {
        let session = self
            .inner
            .sessions
            .get(&session_id)
            .ok_or(ApiError::SessionNotFound(session_id))?;
        let entry = session
            .connections
            .get(&conn_id)
            .ok_or(ApiError::ConnectionNotFound(conn_id))?;
        if entry.driver.legacy_driver().is_none() {
            return Ok(None);
        }
        serde_json::from_value(entry.configuration.clone())
            .map(Some)
            .map_err(|error| ApiError::Internal(error.to_string()))
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
    ) -> ApiResult<RuntimeConnectionHandle> {
        let (driver, configuration, credentials, tenant_id, old_handle) = {
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
                entry.configuration.clone(),
                entry.credentials.clone(),
                match &entry.provenance {
                    ConnectionProvenance::Managed { tenant_id, .. } => Some(tenant_id.0),
                    ConnectionProvenance::TrustedLocal => None,
                },
                entry.handle.clone(),
            )
        };
        let opener = driver.clone();
        let new_handle = self
            .run_bounded("reconnect", async move {
                opener.open(&configuration, &credentials, tenant_id).await
            })
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
        let tx_id = req.tx.as_ref().map(|tx| tx.tx_id);
        self.validate_execute_tx(session_id, conn_id, req.tx.as_ref())?;
        let entry = self.authorize_connection_operation(
            session_id,
            conn_id,
            operation,
            Some(&req.sql),
            &[],
        )?;
        let resource_guards = self.reserve_query_resources(&entry)?;
        let retained_context = self.retained_byte_context(&entry);
        let mut exec = ExecuteRequest {
            sql: req.sql,
            params: req.params,
            transform: req.transform,
        };
        if let Some(transform) = exec.transform.take() {
            exec.sql = crate::result_transform::apply(entry.driver.engine(), &exec.sql, &transform)
                .map_err(ApiError::BadRequest)?;
        }
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
            let stream = driver.execute(handle, exec).await?;
            let cursor_id = stream.cursor_id;
            *slot.lock().unwrap() = Some(cursor_id);
            // Hand the driver stream to the registry pump. Eviction of
            // a co-tenant cursor happens via the on_evict callback.
            let wrapped = cursors.wrap_for_connection_accounted(
                session_id,
                conn_id,
                stream,
                retained_context.clone(),
            )?;
            let result = drain_stream_accounted(wrapped, max_rows, max_bytes, &cursors).await;
            cursors.remove(cursor_id);
            result
        });

        let result = if dur.is_zero() {
            match (&mut task).await {
                Ok(res) => res.map_err(ApiError::Driver),
                Err(join) => Err(ApiError::Internal(format!("execute task failed: {join}"))),
            }
        } else {
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
        };
        if let Ok(response) = &result {
            self.inner.retained_query_results.insert(
                session_id,
                conn_id,
                response.cursor_id,
                response.columns.clone(),
                response.rows.clone(),
            );
        } else if let Some(tx_id) = tx_id {
            self.mark_transaction_failed(session_id, tx_id);
        }
        result
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
        mut req: ExecuteRequest,
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
        if let Some(transform) = req.transform.take() {
            let engine = entry.driver.engine();
            req.sql = crate::result_transform::apply(engine, &req.sql, &transform)
                .map_err(ApiError::BadRequest)?;
        }
        let resource_guards = self.reserve_query_resources(&entry)?;
        let retained_context = self.retained_byte_context(&entry);
        let driver = entry.driver.clone();
        let handle = entry.handle.clone();
        let mut task = tokio::spawn(async move { driver.execute(handle, req).await });
        let duration = self.request_timeout();
        let stream = if duration.is_zero() {
            task.await
                .map_err(|error| ApiError::Internal(format!("execute task failed: {error}")))??
        } else {
            match tokio::time::timeout(duration, &mut task).await {
                Ok(joined) => joined.map_err(|error| {
                    ApiError::Internal(format!("execute task failed: {error}"))
                })??,
                Err(_) => {
                    task.abort();
                    return Err(timeout_error("execute"));
                }
            }
        };
        let cursor_id = stream.cursor_id;
        // Hand the driver's stream to the registry-owned pump. Wrapping
        // enforces the per-session cap (evicting the LRA cursor of the
        // same session via the installed on_evict callback), spawns
        // the pump task, and returns a rebound stream whose `rows`
        // channel is fed by the pump.
        match self.inner.cursors.wrap_for_connection_accounted(
            session_id,
            conn_id,
            stream,
            retained_context,
        ) {
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
            transform: None,
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

    pub fn cursor_page_received(&self, cursor_id: CursorId) {
        self.inner.cursors.page_received(cursor_id);
    }

    pub fn cursor_page_processed(&self, cursor_id: CursorId) {
        self.inner.cursors.page_processed(cursor_id);
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
        let handle = entry
            .handle
            .builtin()
            .cloned()
            .ok_or_else(native_provider_only)?;
        Ok(pg.listen(handle, channels).await?)
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
        if entry.driver.semantic_engine() == Some(Engine::SqlServer) {
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
                    "SQL Server native bulk format needs typed rows and is not part of the locked driver trait",
                )
                .with_engine(Engine::SqlServer),
            ));
        }

        let driver = entry.driver.clone();
        let handle = entry
            .handle
            .builtin()
            .cloned()
            .ok_or_else(native_provider_only)?;
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
            tx_id: handle.tx_id(),
            connection: req.connection,
            mode: handle.mode(),
            opened_at: chrono::Utc::now(),
        };
        let tx = TransactionEntry {
            info: info.clone(),
            handle,
            savepoints: Mutex::new(Vec::new()),
            ending: AtomicBool::new(false),
            failed: AtomicBool::new(false),
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
        if self.transaction_failed(session_id, req.connection, req.tx_id)? {
            return Err(ApiError::Conflict(
                "failed transaction must be rolled back before it can be closed".into(),
            ));
        }
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
                condition: if entry.failed.load(Ordering::Acquire) {
                    sift_protocol::TransactionCondition::Failed
                } else {
                    sift_protocol::TransactionCondition::Active
                },
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

    pub(crate) fn mark_transaction_failed(&self, session_id: SessionId, tx_id: TxId) {
        if let Some(session) = self.inner.sessions.get(&session_id) {
            if let Some(transaction) = session.transactions.get(&tx_id) {
                transaction.failed.store(true, Ordering::Release);
            }
        }
    }

    fn transaction_failed(
        &self,
        session_id: SessionId,
        connection: ConnectionId,
        tx_id: TxId,
    ) -> ApiResult<bool> {
        let session = self
            .inner
            .sessions
            .get(&session_id)
            .ok_or(ApiError::SessionNotFound(session_id))?;
        let transaction = session.transactions.get(&tx_id).ok_or_else(|| {
            ApiError::Driver(DriverError::new(
                Code::TransactionNotFound,
                "transaction not active",
            ))
        })?;
        if transaction.info.connection != connection {
            return Err(ApiError::BadRequest(
                "transaction belongs to another connection".into(),
            ));
        }
        Ok(transaction.failed.load(Ordering::Acquire))
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
        let tx_handle = tx_handle
            .builtin()
            .cloned()
            .ok_or_else(native_provider_only)?;
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
        let tx_handle = tx_handle
            .builtin()
            .cloned()
            .ok_or_else(native_provider_only)?;
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
        let tx_handle = tx_handle
            .builtin()
            .cloned()
            .ok_or_else(native_provider_only)?;
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
    ) -> ApiResult<RuntimeTransactionHandle> {
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
    ) -> ApiResult<RuntimeTransactionHandle> {
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
        let driver = entry
            .driver
            .legacy_driver()
            .cloned()
            .ok_or_else(native_provider_only)?;
        let handle = entry
            .handle
            .builtin()
            .cloned()
            .ok_or_else(native_provider_only)?;
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
        let driver = entry
            .driver
            .legacy_driver()
            .cloned()
            .ok_or_else(native_provider_only)?;
        let handle = entry
            .handle
            .builtin()
            .cloned()
            .ok_or_else(native_provider_only)?;
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
            let driver = entry
                .driver
                .legacy_driver()
                .cloned()
                .ok_or_else(native_provider_only)?;
            let handle = entry
                .handle
                .builtin()
                .cloned()
                .ok_or_else(native_provider_only)?;
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
                transform: None,
                source: None,
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
                        return Err(ApiError::EditConflict {
                            edit_index: stmt.edit_index,
                            affected_rows: affected,
                            expected_rows: 1,
                        });
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
                    transform: None,
                    source: None,
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
                        transform: None,
                        source: None,
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
        let cursor = floor_char_boundary(
            &request.sql,
            usize::min(request.cursor as usize, request.sql.len()),
        ) as u32;
        let limit = request.limit;
        let state = self
            .open_semantic_document(
                session_id,
                conn_id,
                sift_protocol::CreateSemanticDocumentRequest {
                    text: request.sql,
                    source: Some(sift_protocol::SemanticSource::Scratch),
                },
            )
            .await?;
        let result = self
            .complete_semantic_document(
                session_id,
                conn_id,
                state.document_id,
                sift_protocol::SemanticCompletionRequest {
                    revision: state.revision,
                    cursor,
                    limit,
                },
            )
            .await;
        let _ = self
            .inner
            .semantic
            .close(semantic_scope(session_id, conn_id), state.document_id);
        result
    }

    pub async fn complete_semantic_document(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        document: sift_protocol::SemanticDocumentId,
        request: sift_protocol::SemanticCompletionRequest,
    ) -> ApiResult<sift_protocol::completion::CompletionResponse> {
        let entry = self.authorize_connection_operation(
            session_id,
            conn_id,
            sift_protocol::OperationKind::Complete,
            None,
            &[],
        )?;
        let engine = entry.driver.engine();
        let registry = self.inner.semantic.clone();
        let scope = semantic_scope(session_id, conn_id);
        let revision = request.revision;
        let cursor = request.cursor;
        let analysis = self
            .run_semantic(move |_| registry.completion_analysis(scope, document, revision, cursor))
            .await?;
        crate::autocomplete::generate_completion_from_semantic(
            self,
            session_id,
            conn_id,
            engine,
            request.limit,
            analysis,
        )
        .await
    }

    pub async fn open_semantic_document(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        request: sift_protocol::CreateSemanticDocumentRequest,
    ) -> ApiResult<sift_protocol::SemanticDocumentState> {
        let entry = self.authorize_connection_operation(
            session_id,
            conn_id,
            sift_protocol::OperationKind::OpenSemanticDocument,
            None,
            &[],
        )?;
        let dialect_id = entry.driver.descriptor().provider.dialect_id.clone();
        let registry = self.inner.semantic.clone();
        let scope = semantic_scope(session_id, conn_id);
        self.run_semantic(move |canceled| {
            registry.create(scope, dialect_id, request.text, request.source, canceled)
        })
        .await
    }

    pub async fn update_semantic_document(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        document: sift_protocol::SemanticDocumentId,
        request: sift_protocol::UpdateSemanticDocumentRequest,
    ) -> ApiResult<sift_protocol::SemanticDocumentState> {
        self.authorize_connection_operation(
            session_id,
            conn_id,
            sift_protocol::OperationKind::UpdateSemanticDocument,
            None,
            &[],
        )?;
        let registry = self.inner.semantic.clone();
        let scope = semantic_scope(session_id, conn_id);
        self.run_semantic(move |canceled| {
            registry.update(
                scope,
                document,
                request.base_revision,
                request.text,
                canceled,
            )
        })
        .await
    }

    pub fn close_semantic_document(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        document: sift_protocol::SemanticDocumentId,
    ) -> ApiResult<()> {
        self.authorize_connection_operation(
            session_id,
            conn_id,
            sift_protocol::OperationKind::CloseSemanticDocument,
            None,
            &[],
        )?;
        self.inner
            .semantic
            .close(semantic_scope(session_id, conn_id), document)
            .map_err(semantic_error)
    }

    pub async fn semantic_diagnostics(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        document: sift_protocol::SemanticDocumentId,
        request: sift_protocol::SemanticRevisionRequest,
    ) -> ApiResult<sift_protocol::DiagnosticsResponse> {
        self.authorize_connection_operation(
            session_id,
            conn_id,
            sift_protocol::OperationKind::DiagnoseSql,
            None,
            &[],
        )?;
        let registry = self.inner.semantic.clone();
        let scope = semantic_scope(session_id, conn_id);
        let revision = request.revision;
        let Some(expected_catalog_revision) = request.catalog_revision else {
            return self
                .run_semantic(move |_| registry.diagnostics(scope, document, revision))
                .await;
        };
        let graph = self
            .catalog_graph_for_operation(
                session_id,
                conn_id,
                sift_protocol::CatalogGraphRequest::default(),
                sift_protocol::OperationKind::DiagnoseSql,
            )
            .await?;
        if graph.revision != expected_catalog_revision {
            return Err(ApiError::BadRequest(format!(
                "stale catalog revision: expected {}, current {}",
                expected_catalog_revision.0, graph.revision.0
            )));
        }
        let catalog = catalog_binding_view(&graph);
        self.run_semantic(move |_| {
            registry.diagnostics_with_catalog(scope, document, revision, &catalog)
        })
        .await
    }

    pub async fn format_semantic_document(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        document: sift_protocol::SemanticDocumentId,
        request: sift_protocol::FormatSqlRequest,
    ) -> ApiResult<sift_protocol::WorkspaceEdit> {
        self.authorize_connection_operation(
            session_id,
            conn_id,
            sift_protocol::OperationKind::FormatSql,
            None,
            &[],
        )?;
        let registry = self.inner.semantic.clone();
        let scope = semantic_scope(session_id, conn_id);
        self.run_semantic(move |canceled| registry.format(scope, document, request, canceled))
            .await
    }

    pub async fn prepare_semantic_quick_fix(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        document: sift_protocol::SemanticDocumentId,
        fix_id: String,
        request: sift_protocol::SqlQuickFixRequest,
    ) -> ApiResult<sift_protocol::WorkspaceEdit> {
        self.authorize_connection_operation(
            session_id,
            conn_id,
            sift_protocol::OperationKind::SqlQuickFix,
            None,
            &[],
        )?;
        let graph = self
            .catalog_graph_for_operation(
                session_id,
                conn_id,
                sift_protocol::CatalogGraphRequest::default(),
                sift_protocol::OperationKind::SqlQuickFix,
            )
            .await?;
        if graph.revision != request.catalog_revision {
            return Err(ApiError::BadRequest(format!(
                "stale catalog revision: expected {}, current {}",
                request.catalog_revision.0, graph.revision.0
            )));
        }
        let catalog = catalog_binding_view(&graph);
        let registry = self.inner.semantic.clone();
        let scope = semantic_scope(session_id, conn_id);
        self.run_semantic(move |_| {
            registry.prepare_quick_fix(scope, document, request.revision, &fix_id, &catalog)
        })
        .await
    }

    pub async fn find_semantic_usages(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        document: sift_protocol::SemanticDocumentId,
        request: sift_protocol::FindSqlUsagesRequest,
    ) -> ApiResult<sift_protocol::SqlUsagePage> {
        self.authorize_connection_operation(
            session_id,
            conn_id,
            sift_protocol::OperationKind::FindSqlUsages,
            None,
            &[],
        )?;
        let catalog = if let Some(expected) = request.catalog_revision {
            let graph = self
                .catalog_graph_for_operation(
                    session_id,
                    conn_id,
                    sift_protocol::CatalogGraphRequest::default(),
                    sift_protocol::OperationKind::FindSqlUsages,
                )
                .await?;
            if graph.revision != expected {
                return Err(ApiError::BadRequest(format!(
                    "stale catalog revision: expected {}, current {}",
                    expected.0, graph.revision.0
                )));
            }
            Some(catalog_binding_view(&graph))
        } else {
            None
        };
        let registry = self.inner.semantic.clone();
        let scope = semantic_scope(session_id, conn_id);
        self.run_semantic(move |_| registry.find_usages(scope, document, request, catalog.as_ref()))
            .await
    }

    pub async fn prepare_semantic_refactor(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        document: sift_protocol::SemanticDocumentId,
        request: sift_protocol::PrepareSqlRefactorRequest,
    ) -> ApiResult<sift_protocol::WorkspaceEdit> {
        self.authorize_connection_operation(
            session_id,
            conn_id,
            sift_protocol::OperationKind::PrepareSqlRefactor,
            None,
            &[],
        )?;
        let catalog = if let Some(expected) = request.catalog_revision {
            let graph = self
                .catalog_graph_for_operation(
                    session_id,
                    conn_id,
                    sift_protocol::CatalogGraphRequest::default(),
                    sift_protocol::OperationKind::PrepareSqlRefactor,
                )
                .await?;
            if graph.revision != expected {
                return Err(ApiError::BadRequest(format!(
                    "stale catalog revision: expected {}, current {}",
                    expected.0, graph.revision.0
                )));
            }
            Some(catalog_binding_view(&graph))
        } else {
            None
        };
        let registry = self.inner.semantic.clone();
        let scope = semantic_scope(session_id, conn_id);
        self.run_semantic(move |_| {
            registry.prepare_refactor(scope, document, request, catalog.as_ref())
        })
        .await
    }

    pub fn select_semantic_statement(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        document: sift_protocol::SemanticDocumentId,
        request: sift_protocol::SelectStatementRequest,
    ) -> ApiResult<sift_protocol::StatementSelection> {
        self.authorize_connection_operation(
            session_id,
            conn_id,
            sift_protocol::OperationKind::SelectStatement,
            None,
            &[],
        )?;
        self.inner
            .semantic
            .select_statement(semantic_scope(session_id, conn_id), document, request)
            .map_err(semantic_error)
    }

    pub fn start_comparison(
        &self,
        session_id: SessionId,
        request: sift_protocol::StartComparisonRequest,
        room_results: crate::room_results::RoomResultRegistry,
    ) -> ApiResult<sift_protocol::ComparisonSummary> {
        const MAX_SOURCE_ROWS: u32 = 50_000;
        const MAX_DIFF_ROWS: u32 = 20_000;
        const MAX_COLUMNS: usize = 512;
        const MAX_KEY_COLUMNS: usize = 16;
        const MAX_TIMEOUT_MS: u64 = 120_000;

        self.with_session(&session_id, |_| ())?;
        let max_source_rows = request.max_source_rows.unwrap_or(4_999);
        let max_diff_rows = request.max_diff_rows.unwrap_or(10_000);
        let timeout_ms = request.timeout_ms.unwrap_or(30_000);
        let key_columns = match &request.key {
            sift_protocol::CompareKey::Explicit { columns } => columns.len(),
            sift_protocol::CompareKey::Infer | sift_protocol::CompareKey::RowOrdinal => 0,
        };
        if max_source_rows == 0
            || max_source_rows > MAX_SOURCE_ROWS
            || max_diff_rows == 0
            || max_diff_rows > MAX_DIFF_ROWS
            || timeout_ms == 0
            || timeout_ms > MAX_TIMEOUT_MS
            || request.column_mappings.len() > MAX_COLUMNS
            || request.tolerances.len() > MAX_COLUMNS
            || key_columns > MAX_KEY_COLUMNS
        {
            return Err(ApiError::BadRequest(
                "comparison request exceeds source, diff, column, key, or timeout limits".into(),
            ));
        }
        validate_compare_source(&request.left)?;
        validate_compare_source(&request.right)?;

        let id = sift_protocol::ComparisonId(uuid::Uuid::new_v4());
        let expires_at = chrono::Utc::now() + chrono::Duration::seconds(600);
        let initial_key = match &request.key {
            sift_protocol::CompareKey::Explicit { columns } => sift_protocol::ResolvedCompareKey {
                columns: columns.clone(),
                inferred_constraint: None,
                row_ordinal: false,
            },
            sift_protocol::CompareKey::RowOrdinal => sift_protocol::ResolvedCompareKey {
                columns: Vec::new(),
                inferred_constraint: None,
                row_ordinal: true,
            },
            sift_protocol::CompareKey::Infer => sift_protocol::ResolvedCompareKey {
                columns: Vec::new(),
                inferred_constraint: None,
                row_ordinal: false,
            },
        };
        let summary = sift_protocol::ComparisonSummary {
            comparison_id: id,
            status: sift_protocol::ComparisonStatus::Running,
            result_digest: String::new(),
            left_rows: 0,
            right_rows: 0,
            equal_rows: 0,
            changed_rows: 0,
            added_rows: 0,
            removed_rows: 0,
            incomparable_rows: 0,
            duplicate_key_groups: 0,
            retained_diff_rows: 0,
            columns: Vec::new(),
            key: initial_key,
            tolerances: request.tolerances.clone(),
            patch_eligible: false,
            patch_refusal_reasons: vec!["comparison is still running".into()],
            failure_code: None,
            expires_at,
        };
        let entry = self.inner.comparisons.create(session_id, summary.clone());
        let store = self.clone();
        tokio::spawn(async move {
            let run = store.run_comparison(
                session_id,
                request,
                room_results,
                max_source_rows as usize,
                max_diff_rows as usize,
                entry.clone(),
            );
            match tokio::time::timeout(Duration::from_millis(timeout_ms), run).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => entry.fail(comparison_failure_code(&error)),
                Err(_) => entry.fail("comparison_timeout"),
            }
        });
        Ok(summary)
    }

    async fn run_comparison(
        &self,
        session_id: SessionId,
        request: sift_protocol::StartComparisonRequest,
        room_results: crate::room_results::RoomResultRegistry,
        max_source_rows: usize,
        max_diff_rows: usize,
        entry: Arc<crate::comparison::ComparisonEntry>,
    ) -> ApiResult<()> {
        let left = self
            .load_comparison_source(session_id, &request.left, &room_results, max_source_rows)
            .await?;
        ensure_comparison_source_bytes("left", &left.dataset)?;
        if entry.canceled() {
            return Ok(());
        }
        let right = self
            .load_comparison_source(session_id, &request.right, &room_results, max_source_rows)
            .await?;
        ensure_comparison_source_bytes("right", &right.dataset)?;
        if entry.canceled() {
            return Ok(());
        }
        if left.dataset.columns.len() > 512 || right.dataset.columns.len() > 512 {
            return Err(ApiError::BadRequest(
                "comparison source exceeds the 512-column limit".into(),
            ));
        }
        let key = resolve_comparison_key(&request, &left, &right)?;
        let left_rows = left.dataset.rows.len() as u64;
        let right_rows = right.dataset.rows.len() as u64;
        let LoadedComparisonSource {
            dataset: left_dataset,
            table: left_table,
            connection: left_connection,
            truncated: left_truncated,
        } = left;
        let LoadedComparisonSource {
            dataset: right_dataset,
            table: right_table,
            connection: right_connection,
            truncated: right_truncated,
        } = right;
        let core_request = sift_core::comparison::ComparisonInput {
            left: left_dataset,
            right: right_dataset,
            mappings: request.column_mappings.clone(),
            key: key.clone(),
            tolerances: request.tolerances.clone(),
            max_diff_rows,
            max_duplicate_group: 1_024,
            cancel: Some(entry.cancel_flag()),
        };
        let output =
            tokio::task::spawn_blocking(move || sift_core::comparison::compare(core_request))
                .await
                .map_err(|error| ApiError::Internal(format!("comparison task failed: {error}")))?
                .map_err(|error| ApiError::BadRequest(error.to_string()))?;
        if entry.canceled() {
            return Ok(());
        }

        let source_truncated = left_truncated || right_truncated;
        let incompatible_columns = output
            .columns
            .iter()
            .any(|column| column.status != sift_protocol::CompareColumnStatus::Mapped);
        let exactly_one_table = left_table.is_some() ^ right_table.is_some();
        let table = left_table.as_ref().or(right_table.as_ref());
        let other_connection = if left_table.is_some() {
            right_connection
        } else {
            left_connection
        };
        let same_connection = table
            .zip(other_connection)
            .is_some_and(|(table, other)| table.connection == other);
        let mut refusal = Vec::new();
        if !exactly_one_table {
            refusal.push("patches require exactly one live-table source".into());
        }
        if !same_connection {
            refusal.push("patches require a retained result from the target connection".into());
        }
        if key.inferred_constraint.is_none() {
            refusal.push("patches require a proven primary or non-null unique key".into());
        }
        if source_truncated || output.truncated {
            refusal.push("truncated comparisons cannot prepare patches".into());
        }
        if output.duplicate_key_groups > 0 {
            refusal.push("duplicate-key comparisons cannot prepare patches".into());
        }
        if !request.tolerances.is_empty() {
            refusal.push("tolerant comparisons cannot prepare patches".into());
        }
        if incompatible_columns || output.incomparable_rows > 0 {
            refusal.push("incomplete or incomparable columns cannot prepare patches".into());
        }
        let patch_eligible = refusal.is_empty();
        let status = if source_truncated || output.truncated {
            sift_protocol::ComparisonStatus::Truncated
        } else {
            sift_protocol::ComparisonStatus::Complete
        };
        let summary = sift_protocol::ComparisonSummary {
            comparison_id: entry.summary().comparison_id,
            status,
            result_digest: output.digest,
            left_rows,
            right_rows,
            equal_rows: output.equal_rows,
            changed_rows: output.changed_rows,
            added_rows: output.added_rows,
            removed_rows: output.removed_rows,
            incomparable_rows: output.incomparable_rows,
            duplicate_key_groups: output.duplicate_key_groups,
            retained_diff_rows: output.rows.len() as u32,
            columns: output.columns,
            key: key.clone(),
            tolerances: request.tolerances,
            patch_eligible,
            patch_refusal_reasons: refusal,
            failure_code: None,
            expires_at: entry.summary().expires_at,
        };
        if patch_eligible {
            let table = table.expect("eligible comparison has one table");
            entry.set_patch_context(crate::comparison::PatchContext {
                connection: table.connection,
                catalog_revision: table.revision,
                table: table.path.clone(),
                object: table.object.clone(),
                target_is_left: left_table.is_some(),
                key,
            });
        }
        entry.complete(summary, output.rows)?;
        Ok(())
    }

    async fn load_comparison_source(
        &self,
        session_id: SessionId,
        source: &sift_protocol::CompareSource,
        room_results: &crate::room_results::RoomResultRegistry,
        max_rows: usize,
    ) -> ApiResult<LoadedComparisonSource> {
        match source {
            sift_protocol::CompareSource::QueryResult {
                cursor_id,
                result_set,
                schema_digest,
            } => {
                let retained = self.inner.retained_query_results.get(
                    session_id,
                    *cursor_id,
                    *result_set,
                    schema_digest,
                )?;
                // Re-authorize the source on every use so a changed connection
                // policy cannot be bypassed through retained pages.
                self.authorize_connection_operation(
                    session_id,
                    retained.connection,
                    sift_protocol::OperationKind::StartComparison,
                    None,
                    &[],
                )?;
                let (dataset, truncated) = truncate_dataset(retained.dataset, max_rows);
                Ok(LoadedComparisonSource {
                    dataset,
                    table: None,
                    connection: Some(retained.connection),
                    truncated,
                })
            }
            sift_protocol::CompareSource::RoomResult {
                room_id,
                result_id,
                result_set,
                schema_digest,
            } => {
                let registry = room_results.clone();
                let room_id = *room_id;
                let result_id = *result_id;
                let result_set = *result_set;
                let schema_digest = schema_digest.clone();
                let dataset = tokio::task::spawn_blocking(move || {
                    registry.comparison_dataset(room_id, result_id, result_set, &schema_digest)
                })
                .await
                .map_err(|error| {
                    ApiError::Internal(format!("room comparison source task failed: {error}"))
                })??;
                let (dataset, truncated) = truncate_dataset(dataset, max_rows);
                Ok(LoadedComparisonSource {
                    dataset,
                    table: None,
                    connection: None,
                    truncated,
                })
            }
            sift_protocol::CompareSource::Table {
                connection,
                catalog_revision,
                object_id,
                filter,
            } => {
                let graph = self
                    .catalog_graph_at_revision(
                        session_id,
                        *connection,
                        *catalog_revision,
                        sift_protocol::CatalogGraphOptions::default(),
                        sift_protocol::OperationKind::StartComparison,
                    )
                    .await?;
                let entry = self.get_conn_entry(session_id, *connection)?;
                let engine = entry.driver.semantic_engine().ok_or_else(|| {
                    ApiError::BadRequest("table comparison requires a SQL provider".into())
                })?;
                let table = comparison_table_from_graph(&graph, *connection, object_id)?;
                let configured_max = self
                    .inner
                    .max_result_rows
                    .load(Ordering::Relaxed)
                    .saturating_sub(1);
                if configured_max == 0 {
                    return Err(ApiError::BadRequest(
                        "configured result-row limit is too small for table comparison".into(),
                    ));
                }
                let row_cap = max_rows.min(configured_max);
                let select_columns = table
                    .object
                    .columns
                    .iter()
                    .map(|column| crate::ddl::quote_ident(&column.name, engine))
                    .collect::<Vec<_>>()
                    .join(", ");
                let table_sql = crate::ddl::qualified_name(&table.path, engine);
                let column_names = table
                    .object
                    .columns
                    .iter()
                    .map(|column| column.name.clone())
                    .collect::<std::collections::HashSet<_>>();
                let rendered_filter = filter
                    .as_ref()
                    .map(|filter| crate::comparison::render_filter(filter, &column_names, engine))
                    .transpose()?;
                let where_sql = rendered_filter
                    .as_ref()
                    .map(|filter| format!(" WHERE {}", filter.sql))
                    .unwrap_or_default();
                let fetch = row_cap + 1;
                let sql = match engine {
                    Engine::Postgres => {
                        format!("SELECT {select_columns} FROM {table_sql}{where_sql} LIMIT {fetch}")
                    }
                    Engine::SqlServer => {
                        format!("SELECT TOP ({fetch}) {select_columns} FROM {table_sql}{where_sql}")
                    }
                };
                let response = self
                    .execute_http_as(
                        session_id,
                        ExecuteRequestHttp {
                            connection: *connection,
                            sql,
                            params: rendered_filter
                                .map(|filter| filter.params)
                                .unwrap_or_default(),
                            tx: None,
                            room_id: None,
                            connection_profile_id: None,
                            transform: None,
                            source: None,
                        },
                        sift_protocol::OperationKind::StartComparison,
                    )
                    .await?;
                let mut dataset = sift_core::comparison::ComparisonDataset {
                    columns: response.columns,
                    rows: response.rows,
                    immutable_order: false,
                };
                let truncated = dataset.rows.len() > row_cap;
                dataset.rows.truncate(row_cap);
                Ok(LoadedComparisonSource {
                    dataset,
                    table: Some(table),
                    connection: Some(*connection),
                    truncated,
                })
            }
        }
    }

    pub fn comparison_summary(
        &self,
        session_id: SessionId,
        id: sift_protocol::ComparisonId,
    ) -> ApiResult<sift_protocol::ComparisonSummary> {
        Ok(self.inner.comparisons.get(session_id, id)?.summary())
    }

    pub fn comparison_page(
        &self,
        session_id: SessionId,
        id: sift_protocol::ComparisonId,
        request: sift_protocol::ComparisonPageRequest,
    ) -> ApiResult<sift_protocol::ComparisonPage> {
        self.inner.comparisons.page(session_id, id, request)
    }

    pub fn cancel_comparison(
        &self,
        session_id: SessionId,
        id: sift_protocol::ComparisonId,
    ) -> ApiResult<sift_protocol::CancelComparisonResponse> {
        Ok(sift_protocol::CancelComparisonResponse {
            comparison_id: id,
            status: self.inner.comparisons.cancel(session_id, id)?,
        })
    }

    pub async fn prepare_comparison_patch(
        &self,
        session_id: SessionId,
        id: sift_protocol::ComparisonId,
        request: sift_protocol::PrepareComparisonPatchRequest,
    ) -> ApiResult<sift_protocol::ComparisonPatchPreparation> {
        let entry = self.inner.comparisons.get(session_id, id)?;
        let summary = entry.summary();
        if !summary.patch_eligible {
            return Ok(sift_protocol::ComparisonPatchPreparation {
                comparison_id: id,
                eligible: false,
                refusal_reasons: summary.patch_refusal_reasons,
                edit_plan: None,
                edit_set: None,
            });
        }
        let context = entry
            .patch_context()
            .ok_or_else(|| ApiError::Internal("eligible comparison has no patch context".into()))?;
        if request.expected_catalog_revision != context.catalog_revision {
            return Err(ApiError::BadRequest(
                "comparison patch catalog revision does not match its target".into(),
            ));
        }
        self.catalog_graph_at_revision(
            session_id,
            context.connection,
            request.expected_catalog_revision,
            sift_protocol::CatalogGraphOptions::default(),
            sift_protocol::OperationKind::PrepareComparisonPatch,
        )
        .await?;
        let rows = self.inner.comparisons.page(
            session_id,
            id,
            sift_protocol::ComparisonPageRequest {
                after: None,
                limit: Some(summary.retained_diff_rows.clamp(1, 500)),
            },
        )?;
        if summary.retained_diff_rows > 500 {
            return Ok(sift_protocol::ComparisonPatchPreparation {
                comparison_id: id,
                eligible: false,
                refusal_reasons: vec!["patch preparation is limited to 500 row changes".into()],
                edit_plan: None,
                edit_set: None,
            });
        }
        let max_statements = request.max_statements.unwrap_or(500);
        if max_statements == 0 || max_statements > 500 || rows.rows.len() > max_statements as usize
        {
            return Err(ApiError::BadRequest(
                "comparison patch statement limit must cover the retained diff and be at most 500"
                    .into(),
            ));
        }
        let edit_set = comparison_edit_set(&context, &rows.rows)?;
        let plan = crate::edit::plan_from_object(
            self.get_conn_entry(session_id, context.connection)?
                .driver
                .semantic_engine()
                .ok_or_else(|| ApiError::BadRequest("patch target is not a SQL provider".into()))?,
            &context.object,
            &edit_set,
        )
        .map_err(ApiError::Driver)?;
        Ok(sift_protocol::ComparisonPatchPreparation {
            comparison_id: id,
            eligible: true,
            refusal_reasons: Vec::new(),
            edit_plan: Some(plan),
            edit_set: Some(edit_set),
        })
    }

    pub async fn capture_semantic_plan(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        request: sift_protocol::CaptureSemanticPlanRequest,
    ) -> ApiResult<sift_protocol::PlanCapture> {
        let (principal, tenant, profile, _) = self.managed_catalog_scope(
            session_id,
            conn_id,
            sift_protocol::OperationKind::CaptureSemanticPlan,
        )?;
        let graph = self
            .catalog_graph_at_revision(
                session_id,
                conn_id,
                request.catalog_revision,
                sift_protocol::CatalogGraphOptions::default(),
                sift_protocol::OperationKind::CaptureSemanticPlan,
            )
            .await?;
        let statement = self
            .inner
            .semantic
            .statement_source(
                semantic_scope(session_id, conn_id),
                request.document_id,
                request.revision,
                &request.statement_id,
            )
            .map_err(semantic_error)?;
        if request.analyze && statement.statement.kind != sift_protocol::StatementKind::Query {
            self.authorize_connection_operation(
                session_id,
                conn_id,
                sift_protocol::OperationKind::ExecuteQuery,
                Some(&statement.sql),
                &[],
            )?;
        }
        let started = std::time::Instant::now();
        let explain = crate::plan::explain_as(
            self,
            session_id,
            conn_id,
            &sift_protocol::ExplainRequest {
                connection: conn_id,
                sql: statement.sql.clone(),
                params: request.params,
                analyze: request.analyze,
            },
            sift_protocol::OperationKind::CaptureSemanticPlan,
        )
        .await?;
        let entry = self.get_conn_entry(session_id, conn_id)?;
        let server = self
            .run_bounded("plan capture server identity", {
                let driver = entry.driver.clone();
                let handle = entry.handle.clone();
                async move { driver.ping(handle).await }
            })
            .await?;
        let raw_response = request.include_raw_response.then_some(explain.raw);
        let mut root = explain.root;
        sanitize_plan_node(&mut root)?;
        let warnings = explain
            .warnings
            .into_iter()
            .map(|warning| sift_protocol::DriverWarning {
                message: "plan warning details redacted".into(),
                code: warning.code,
            })
            .collect();
        let mut capture = sift_protocol::PlanCapture {
            id: sift_protocol::PlanCaptureId(uuid::Uuid::new_v4()),
            tenant_id: tenant.0,
            connection_profile_id: profile.0,
            creator_principal_id: principal.0,
            provider: graph.provider,
            server_version: server.server_version,
            engine: explain.engine,
            source_digest: statement.source_digest,
            document_revision: request.revision,
            statement_id: statement.statement.statement_id,
            statement_fingerprint: crate::fingerprint::sql(&statement.sql),
            catalog_revision: request.catalog_revision,
            analyzed: explain.analyzed,
            captured_at: chrono::Utc::now(),
            duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            root,
            warnings,
            complete: true,
            revision: 0,
            raw_response: None,
            source: request.source,
        };
        let metadata = self
            .inner
            .authorization_store
            .read()
            .unwrap()
            .clone()
            .ok_or(ApiError::MetadataUnavailable)?;
        let durable = capture.clone();
        tokio::task::spawn_blocking(move || metadata.create_plan_capture(&durable))
            .await
            .map_err(|error| {
                ApiError::Internal(format!("plan capture persistence task: {error}"))
            })??;
        capture.raw_response = raw_response;
        Ok(capture)
    }

    async fn run_semantic<T, F>(&self, work: F) -> ApiResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&AtomicBool) -> Result<T, sift_semantic::Error> + Send + 'static,
    {
        let canceled = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&canceled);
        let task = tokio::task::spawn_blocking(move || work(&worker_cancel));
        match tokio::time::timeout(Duration::from_millis(500), task).await {
            Ok(Ok(result)) => result.map_err(semantic_error),
            Ok(Err(join)) => Err(ApiError::Internal(format!(
                "semantic worker failed: {join}"
            ))),
            Err(_) => {
                canceled.store(true, Ordering::Release);
                Err(ApiError::Driver(DriverError::new(
                    Code::SemanticTimedOut,
                    "semantic operation exceeded its 500ms deadline",
                )))
            }
        }
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
            configuration: entry.configuration.clone(),
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

    /// Reap sessions idle longer than `max_idle`. Not wired into a background
    /// task yet; tests call it directly.
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

fn digest_bytes(prefix: &str, bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(prefix.len() + digest.len() * 2);
    output.push_str(prefix);
    for byte in digest {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn semantic_scope(session: SessionId, connection: ConnectionId) -> sift_semantic::DocumentScope {
    sift_semantic::DocumentScope {
        session: session.0,
        connection: connection.0,
    }
}

fn floor_char_boundary(value: &str, mut index: usize) -> usize {
    while index > 0 && !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ensure_comparison_source_bytes(
    side: &str,
    dataset: &sift_core::comparison::ComparisonDataset,
) -> ApiResult<()> {
    const MAX_SOURCE_BYTES: usize = 64 * 1024 * 1024;
    let bytes = serde_json::to_vec(&(&dataset.columns, &dataset.rows))
        .map_err(|error| ApiError::Internal(format!("measuring comparison source: {error}")))?
        .len();
    if bytes > MAX_SOURCE_BYTES {
        return Err(ApiError::BadRequest(format!(
            "{side} comparison source exceeds the {MAX_SOURCE_BYTES}-byte limit"
        )));
    }
    Ok(())
}

fn validate_compare_source(source: &sift_protocol::CompareSource) -> ApiResult<()> {
    let digest = match source {
        sift_protocol::CompareSource::QueryResult {
            result_set,
            schema_digest,
            ..
        }
        | sift_protocol::CompareSource::RoomResult {
            result_set,
            schema_digest,
            ..
        } => {
            if *result_set > 1_024 {
                return Err(ApiError::BadRequest(
                    "comparison result_set must be at most 1024".into(),
                ));
            }
            Some(schema_digest)
        }
        sift_protocol::CompareSource::Table { .. } => None,
    };
    if digest.is_some_and(|digest| {
        digest.len() != 73
            || !digest.starts_with("schemafp:")
            || !digest[9..].bytes().all(|byte| byte.is_ascii_hexdigit())
    }) {
        return Err(ApiError::BadRequest(
            "comparison schema digest must be a schemafp SHA-256 digest".into(),
        ));
    }
    if let sift_protocol::CompareSource::RoomResult { room_id, .. } = source {
        if *room_id <= 0 {
            return Err(ApiError::BadRequest(
                "comparison room_id must be positive".into(),
            ));
        }
    }
    Ok(())
}

fn truncate_dataset(
    mut dataset: sift_core::comparison::ComparisonDataset,
    max_rows: usize,
) -> (sift_core::comparison::ComparisonDataset, bool) {
    let truncated = dataset.rows.len() > max_rows;
    dataset.rows.truncate(max_rows);
    (dataset, truncated)
}

fn comparison_failure_code(error: &ApiError) -> &'static str {
    match error {
        ApiError::BadRequest(_) => "invalid_comparison",
        ApiError::Forbidden(_) => "comparison_forbidden",
        ApiError::Driver(driver) if driver.code == Code::ResultTooLarge => {
            "comparison_source_too_large"
        }
        ApiError::Driver(_) => "comparison_source_failed",
        _ => "comparison_internal",
    }
}

fn comparison_table_from_graph(
    graph: &sift_protocol::CatalogGraph,
    connection: ConnectionId,
    object_id: &sift_protocol::CatalogObjectId,
) -> ApiResult<LoadedComparisonTable> {
    use sift_protocol::{CatalogNodeDetails, CatalogNodeKind, ConstraintKind, Nullability};

    let node = graph
        .data
        .nodes
        .iter()
        .find(|node| &node.id == object_id)
        .ok_or_else(|| {
            ApiError::BadRequest(format!(
                "catalog object {:?} is absent or inaccessible",
                object_id.0
            ))
        })?;
    let kind = match node.kind {
        CatalogNodeKind::Table => sift_protocol::ObjectKind::Table,
        CatalogNodeKind::ForeignTable => sift_protocol::ObjectKind::ForeignTable,
        CatalogNodeKind::PartitionedTable => sift_protocol::ObjectKind::PartitionedTable,
        _ => {
            return Err(ApiError::BadRequest(
                "table comparison source must name a table, foreign table, or partitioned table"
                    .into(),
            ));
        }
    };
    if node.completeness != sift_protocol::CatalogCompleteness::Complete {
        return Err(ApiError::BadRequest(
            "table comparison requires complete metadata for the selected object".into(),
        ));
    }
    let schema = node.parent_id.as_ref().and_then(|parent| {
        graph
            .data
            .nodes
            .iter()
            .find(|candidate| &candidate.id == parent && candidate.kind == CatalogNodeKind::Schema)
            .map(|schema| schema.name.clone())
    });
    let path = sift_protocol::ObjectPath {
        catalog: None,
        schema,
        name: node.name.clone(),
        kind: Some(kind),
        routine_args: None,
    };
    let mut child_nodes: Vec<_> = graph
        .data
        .nodes
        .iter()
        .filter(|candidate| candidate.parent_id.as_ref() == Some(object_id))
        .collect();
    child_nodes.sort_by_key(|child| (child.ordinal.unwrap_or(u32::MAX), child.name.clone()));
    let columns = child_nodes
        .iter()
        .filter_map(|child| match &child.details {
            CatalogNodeDetails::Column { column } => Some(column.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if columns.is_empty() || columns.len() > 512 {
        return Err(ApiError::BadRequest(
            "table comparison requires between 1 and 512 visible columns".into(),
        ));
    }
    let indexes = child_nodes
        .iter()
        .filter_map(|child| match &child.details {
            CatalogNodeDetails::Index { index } => Some(index.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let constraints_with_ids = child_nodes
        .iter()
        .filter_map(|child| match &child.details {
            CatalogNodeDetails::Constraint { constraint } => {
                Some((child.id.clone(), constraint.clone()))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let constraints = constraints_with_ids
        .iter()
        .map(|(_, constraint)| constraint.clone())
        .collect::<Vec<_>>();
    let triggers = child_nodes
        .iter()
        .filter_map(|child| match &child.details {
            CatalogNodeDetails::Trigger { trigger } => Some(trigger.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let non_null = columns
        .iter()
        .filter(|column| column.nullable == Nullability::NotNullable)
        .map(|column| column.name.as_str())
        .collect::<std::collections::HashSet<_>>();
    let identity = constraints_with_ids
        .iter()
        .find(|(_, constraint)| {
            constraint.kind == ConstraintKind::PrimaryKey && !constraint.columns.is_empty()
        })
        .or_else(|| {
            constraints_with_ids.iter().find(|(_, constraint)| {
                constraint.kind == ConstraintKind::Unique
                    && !constraint.columns.is_empty()
                    && constraint
                        .columns
                        .iter()
                        .all(|column| non_null.contains(column.as_str()))
            })
        })
        .map(|(id, constraint)| (constraint.columns.clone(), id.clone()));
    Ok(LoadedComparisonTable {
        connection,
        revision: graph.revision,
        path,
        object: sift_protocol::ObjectInfo {
            name: node.name.clone(),
            kind,
            estimated_rows: None,
            modified_at: None,
            comment: None,
            routine_args: None,
            columns,
            indexes,
            constraints,
            triggers,
        },
        identity,
    })
}

fn resolve_comparison_key(
    request: &sift_protocol::StartComparisonRequest,
    left: &LoadedComparisonSource,
    right: &LoadedComparisonSource,
) -> ApiResult<sift_protocol::ResolvedCompareKey> {
    use sift_protocol::{CompareColumnPair, CompareKey, ResolvedCompareKey};

    match &request.key {
        CompareKey::Explicit { columns } => {
            if columns.is_empty() {
                return Err(ApiError::BadRequest(
                    "explicit comparison key must contain at least one column".into(),
                ));
            }
            Ok(ResolvedCompareKey {
                columns: columns.clone(),
                inferred_constraint: None,
                row_ordinal: false,
            })
        }
        CompareKey::RowOrdinal => {
            if left.table.is_some() || right.table.is_some() {
                return Err(ApiError::BadRequest(
                    "row-ordinal comparison keys are forbidden for live tables".into(),
                ));
            }
            Ok(ResolvedCompareKey {
                columns: Vec::new(),
                inferred_constraint: None,
                row_ordinal: true,
            })
        }
        CompareKey::Infer => {
            let explicit_right = |left_name: &str| {
                request
                    .column_mappings
                    .iter()
                    .find(|mapping| mapping.left == left_name)
                    .map(|mapping| mapping.right.clone())
                    .unwrap_or_else(|| left_name.to_owned())
            };
            let explicit_left = |right_name: &str| {
                request
                    .column_mappings
                    .iter()
                    .find(|mapping| mapping.right == right_name)
                    .map(|mapping| mapping.left.clone())
                    .unwrap_or_else(|| right_name.to_owned())
            };
            if let Some(left_table) = &left.table {
                let (left_columns, constraint) = left_table.identity.clone().ok_or_else(|| {
                    ApiError::BadRequest(
                        "left table has no primary or non-null unique comparison key".into(),
                    )
                })?;
                let columns = left_columns
                    .iter()
                    .map(|left| CompareColumnPair {
                        left: left.clone(),
                        right: explicit_right(left),
                    })
                    .collect::<Vec<_>>();
                if let Some(right_table) = &right.table {
                    let (right_columns, _) = right_table.identity.as_ref().ok_or_else(|| {
                        ApiError::BadRequest(
                            "right table has no primary or non-null unique comparison key".into(),
                        )
                    })?;
                    if columns
                        .iter()
                        .map(|column| &column.right)
                        .ne(right_columns.iter())
                    {
                        return Err(ApiError::BadRequest(
                            "table comparison keys do not map to the same proven identity".into(),
                        ));
                    }
                }
                Ok(ResolvedCompareKey {
                    columns,
                    inferred_constraint: Some(constraint),
                    row_ordinal: false,
                })
            } else if let Some(right_table) = &right.table {
                let (right_columns, constraint) =
                    right_table.identity.clone().ok_or_else(|| {
                        ApiError::BadRequest(
                            "right table has no primary or non-null unique comparison key".into(),
                        )
                    })?;
                Ok(ResolvedCompareKey {
                    columns: right_columns
                        .iter()
                        .map(|right| CompareColumnPair {
                            left: explicit_left(right),
                            right: right.clone(),
                        })
                        .collect(),
                    inferred_constraint: Some(constraint),
                    row_ordinal: false,
                })
            } else {
                Err(ApiError::BadRequest(
                    "key inference requires at least one live-table source; retained results must provide an explicit or ordinal key"
                        .into(),
                ))
            }
        }
    }
}

fn comparison_edit_set(
    context: &crate::comparison::PatchContext,
    rows: &[sift_protocol::RowDiff],
) -> ApiResult<sift_protocol::EditSet> {
    use sift_protocol::{CellComparisonStatus, CellEdit, RowDiffKind, RowEdit, RowKey};

    let key_names = context
        .key
        .columns
        .iter()
        .map(|pair| {
            if context.target_is_left {
                pair.left.clone()
            } else {
                pair.right.clone()
            }
        })
        .collect::<Vec<_>>();
    let mut edits = Vec::with_capacity(rows.len());
    for row in rows {
        let target_values = |desired: bool| -> ApiResult<Vec<CellEdit>> {
            row.cells
                .iter()
                .map(|cell| {
                    let target_left = context.target_is_left;
                    let use_left = if desired { !target_left } else { target_left };
                    let value = if use_left { &cell.left } else { &cell.right };
                    let value = value.clone().ok_or_else(|| {
                        ApiError::Internal("patchable comparison omitted a row value".into())
                    })?;
                    Ok(CellEdit {
                        column: if target_left {
                            cell.column.left.clone()
                        } else {
                            cell.column.right.clone()
                        },
                        value,
                    })
                })
                .collect()
        };
        let target_has_row = match row.kind {
            RowDiffKind::Added => !context.target_is_left,
            RowDiffKind::Removed => context.target_is_left,
            RowDiffKind::Changed => true,
            RowDiffKind::Incomparable => {
                return Err(ApiError::Internal(
                    "incomparable row reached patch preparation".into(),
                ));
            }
        };
        let desired_has_row = match row.kind {
            RowDiffKind::Added => context.target_is_left,
            RowDiffKind::Removed => !context.target_is_left,
            RowDiffKind::Changed => true,
            RowDiffKind::Incomparable => false,
        };
        let edit = match (target_has_row, desired_has_row) {
            (false, true) => RowEdit::Insert {
                values: target_values(true)?,
            },
            (true, false) | (true, true) => {
                let current = target_values(false)?;
                let key = RowKey {
                    columns: key_names
                        .iter()
                        .map(|name| {
                            current
                                .iter()
                                .find(|cell| &cell.column == name)
                                .cloned()
                                .ok_or_else(|| {
                                    ApiError::Internal(format!(
                                        "patchable comparison omitted key column {name:?}"
                                    ))
                                })
                        })
                        .collect::<ApiResult<Vec<_>>>()?,
                };
                let expected = current
                    .iter()
                    .filter(|cell| !key_names.contains(&cell.column))
                    .cloned()
                    .collect::<Vec<_>>();
                if desired_has_row {
                    let desired = target_values(true)?;
                    RowEdit::Update {
                        key,
                        changes: row
                            .cells
                            .iter()
                            .zip(desired)
                            .filter(|(cell, _)| cell.status == CellComparisonStatus::Unequal)
                            .map(|(_, value)| value)
                            .collect(),
                        expected,
                    }
                } else {
                    RowEdit::Delete { key, expected }
                }
            }
            (false, false) => {
                return Err(ApiError::Internal(
                    "comparison patch row has no target or desired row".into(),
                ));
            }
        };
        edits.push(edit);
    }
    Ok(sift_protocol::EditSet {
        table: context.table.clone(),
        edits,
    })
}

fn catalog_binding_view(graph: &sift_protocol::CatalogGraph) -> sift_semantic::CatalogBindingView {
    let schemas = graph
        .data
        .nodes
        .iter()
        .filter(|node| node.kind == sift_protocol::CatalogNodeKind::Schema)
        .map(|node| (node.id.clone(), node.name.clone()))
        .collect::<std::collections::HashMap<_, _>>();
    let columns = graph
        .data
        .nodes
        .iter()
        .filter(|node| node.kind == sift_protocol::CatalogNodeKind::Column)
        .filter_map(|node| Some((node.parent_id.clone()?, node.name.clone())))
        .fold(
            std::collections::HashMap::<_, Vec<_>>::new(),
            |mut columns, (parent, name)| {
                columns.entry(parent).or_default().push(name);
                columns
            },
        );
    sift_semantic::CatalogBindingView {
        revision: graph.revision,
        complete: graph.data.coverage.state == sift_protocol::CatalogCoverageState::Complete,
        objects: graph
            .data
            .nodes
            .iter()
            .filter_map(|node| {
                let schema = schemas.get(node.parent_id.as_ref()?)?;
                Some(sift_semantic::CatalogBindingObject {
                    id: node.id.clone(),
                    schema: schema.clone(),
                    name: node.name.clone(),
                    columns: columns.get(&node.id).cloned().unwrap_or_default(),
                })
            })
            .collect(),
    }
}

fn semantic_error(error: sift_semantic::Error) -> ApiError {
    let code = match error {
        sift_semantic::Error::NotFound => Code::SemanticDocumentNotFound,
        sift_semantic::Error::RevisionConflict { .. } => Code::SemanticRevisionConflict,
        sift_semantic::Error::InvalidRange => Code::InvalidTextRange,
        sift_semantic::Error::DialectUnavailable(_) => Code::DialectUnavailable,
        sift_semantic::Error::LimitExceeded => Code::SemanticLimitExceeded,
        sift_semantic::Error::Canceled => Code::QueryCanceled,
        sift_semantic::Error::InvalidRequest => Code::InvalidParameterValue,
    };
    ApiError::Driver(DriverError::new(code, error.to_string()))
}

fn stale_schema(snapshot: &sift_protocol::SchemaSnapshot) -> CachedSchema {
    let mut snapshot = snapshot.clone();
    snapshot.incomplete = true;
    if let Some(graph) = snapshot.graph.as_mut() {
        graph.coverage.state = sift_protocol::CatalogCoverageState::Stale;
        graph
            .coverage
            .failures
            .push(sift_protocol::CatalogCoverageFailure {
                stage: "refresh".into(),
                schema: None,
                code: "provider_unavailable_using_stale_catalog".into(),
            });
        sift_core::catalog::normalize_graph(graph);
    }
    CachedSchema::new_uncached(snapshot)
}

fn mark_rolled_back(outcomes: &mut [sift_protocol::MigrationStatementOutcome]) {
    for outcome in outcomes {
        if outcome.status == sift_protocol::MigrationStatementStatus::Applied {
            outcome.status = sift_protocol::MigrationStatementStatus::RolledBack;
        }
    }
}

fn migration_result_code(error: &ApiError) -> Option<String> {
    match error {
        ApiError::Driver(error) => Some(error.code.to_string()),
        _ => Some("migration_failed".into()),
    }
}

fn sanitize_plan_node(root: &mut sift_protocol::PlanNode) -> ApiResult<()> {
    fn visit(node: &mut sift_protocol::PlanNode, depth: usize, count: &mut usize) -> ApiResult<()> {
        *count += 1;
        if depth > 128
            || *count > 100_000
            || node.op.is_empty()
            || node.op.len() > 1_024
            || node
                .relation
                .as_ref()
                .is_some_and(|relation| relation.len() > 1_024)
        {
            return Err(ApiError::BadRequest(
                "normalized plan exceeds durable capture limits".into(),
            ));
        }
        if node
            .relation
            .as_ref()
            .is_some_and(|relation| relation.starts_with('#') || relation.starts_with("pg_temp"))
        {
            node.relation = None;
        }
        // Raw engine extras include predicates and literal-bearing fragments.
        // The durable form keeps only the typed, engine-neutral fields.
        node.extra.clear();
        for child in &mut node.children {
            visit(child, depth + 1, count)?;
        }
        Ok(())
    }
    visit(root, 0, &mut 0)
}

/// Cheap clone of a connection entry (just Arc + ConnHandle Arc).
pub struct ConnectionEntryClone {
    pub driver: RuntimeDriver,
    pub handle: RuntimeConnectionHandle,
    pub provenance: ConnectionProvenance,
    pub configuration: serde_json::Value,
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
    quota_exempt: AtomicBool,
    resource_guard: Mutex<Option<crate::resources::ResourceGuard>>,
}

impl Session {
    fn info(&self) -> SessionInfo {
        SessionInfo {
            id: self.id,
            created_at: self.created_at,
            tag: self.tag.clone(),
            tenant_id: self.tenant_id.lock().unwrap().map(|tenant| tenant.0),
            connections: self.connections.iter().map(|e| e.info.clone()).collect(),
        }
    }
}

/// One open connection within a session.
pub struct ConnectionEntry {
    pub id: ConnectionId,
    pub engine: Option<Engine>,
    pub handle: RuntimeConnectionHandle,
    pub driver: RuntimeDriver,
    pub info: ConnectionInfo,
    /// Original provider input, retained so a broken connection can be transparently
    /// re-established for idempotent operations (ping/schema).
    pub configuration: serde_json::Value,
    pub credentials: std::collections::HashMap<String, Vec<u8>>,
    pub provenance: ConnectionProvenance,
    _resource_guard: Option<crate::resources::ResourceGuard>,
}

pub struct TransactionEntry {
    pub info: TransactionInfo,
    pub handle: RuntimeTransactionHandle,
    pub savepoints: Mutex<Vec<SavepointInfo>>,
    ending: AtomicBool,
    failed: AtomicBool,
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
        Operation::Complete {
            session,
            connection,
            request,
        } => Operation::Complete {
            session,
            connection,
            request: sift_protocol::completion::CompletionRequest {
                sql: crate::fingerprint::sql(&request.sql),
                cursor: request.cursor,
                limit: request.limit,
            },
        },
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

fn native_provider_only() -> ApiError {
    ApiError::Driver(DriverError::new(
        Code::UnsupportedForEngine,
        "operation requires a bundled provider native extension",
    ))
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

impl crate::export::PageRetention for CursorGuard {
    fn page_received(&self) {
        self.sessions.cursor_page_received(self.cursor_id);
    }

    fn page_processed(&self) {
        self.sessions.cursor_page_processed(self.cursor_id);
    }
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
    drain_stream_inner(stream, max_rows, max_bytes, None).await
}

async fn drain_room_stream(
    sessions: &SessionStore,
    stream: ResultSetStream,
    max_response_rows: usize,
    max_response_bytes: usize,
    retention: (crate::resources::ResourceManager, sift_metadata::TenantId),
) -> ApiResult<RoomQueryExecution> {
    let cursor_id = stream.cursor_id;
    let mut rx = stream.rows;
    let mut pages = Vec::new();
    let mut columns = Vec::new();
    let mut rows = Vec::new();
    let mut response_bytes = 0usize;
    let mut affected_rows = None;
    let mut warnings = Vec::new();
    let mut has_more = false;
    let mut retention_guards = Vec::new();

    while let Some(page) = rx.recv().await {
        sessions.cursor_page_received(cursor_id);
        match &page {
            Page::NextResult { columns: next } if columns.is_empty() => {
                columns = next.clone();
            }
            Page::Rows { rows: next } => {
                for row in next {
                    let bytes = row_bytes(row);
                    if rows.len() < max_response_rows
                        && response_bytes.saturating_add(bytes) <= max_response_bytes
                    {
                        response_bytes = response_bytes.saturating_add(bytes);
                        rows.push(row.clone());
                    } else {
                        has_more = true;
                    }
                }
            }
            Page::Error { error } => {
                sessions.cursor_page_processed(cursor_id);
                sessions.cursor_remove(cursor_id);
                return Err(ApiError::Driver(error.clone()));
            }
            Page::Done {
                affected_rows: next_affected,
                warnings: next_warnings,
            } => {
                affected_rows = *next_affected;
                warnings = next_warnings.clone();
            }
            Page::NextResult { .. } => {}
        }
        let retained_bytes = match serde_json::to_vec(&page) {
            Ok(encoded) => encoded.len() as u64,
            Err(error) => {
                sessions.cursor_page_processed(cursor_id);
                sessions.cursor_remove(cursor_id);
                return Err(ApiError::Internal(error.to_string()));
            }
        };
        match retention.0.reserve(
            retention.1,
            sift_protocol::TenantResource::RetainedResultBytes,
            retained_bytes,
        ) {
            Ok(guard) => retention_guards.push(guard),
            Err(error) => {
                sessions.cursor_page_processed(cursor_id);
                sessions.cursor_remove(cursor_id);
                return Err(error);
            }
        }
        pages.push(page);
        sessions.cursor_page_processed(cursor_id);
    }
    sessions.cursor_remove(cursor_id);
    Ok(RoomQueryExecution {
        response: ExecuteResponse {
            cursor_id,
            schema_digest: crate::comparison::schema_digest(&columns),
            columns,
            rows,
            affected_rows,
            warnings,
            has_more,
        },
        pages,
        retention_guards,
    })
}

async fn drain_stream_accounted(
    stream: ResultSetStream,
    max_rows: usize,
    max_bytes: usize,
    cursors: &CursorRegistry,
) -> Result<ExecuteResponse, DriverError> {
    drain_stream_inner(stream, max_rows, max_bytes, Some(cursors)).await
}

async fn drain_stream_inner(
    stream: ResultSetStream,
    max_rows: usize,
    max_bytes: usize,
    cursors: Option<&CursorRegistry>,
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
    let mut _retained_guards = Vec::new();

    while let Some(page) = rx.recv().await {
        if let Some(cursors) = cursors {
            cursors.page_received(cursor_id);
        }
        if matches!(&page, Page::NextResult { .. } | Page::Rows { .. }) {
            if let Some(guard) = cursors.and_then(|cursors| cursors.take_page_retention(cursor_id))
            {
                _retained_guards.push(guard);
            }
        }
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
        if let Some(cursors) = cursors {
            cursors.page_processed(cursor_id);
        }
    }

    Ok(ExecuteResponse {
        cursor_id,
        schema_digest: crate::comparison::schema_digest(&columns),
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

fn display_name_for_configuration(
    configuration: &serde_json::Value,
    provider: &sift_protocol::ProviderRef,
) -> String {
    let Ok(spec) = serde_json::from_value::<ConnectionSpec>(configuration.clone()) else {
        return provider.provider_id.to_string();
    };
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
                                tenant_id: None,
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

    #[test]
    fn hosted_session_quota_is_acquired_at_creation_and_released_on_close() {
        let store = SessionStore::new(DriverRegistry::new());
        let config = crate::config::TenantLimitsConfig {
            trusted_local_unlimited: false,
            defaults: sift_protocol::TenantResourceLimits {
                sessions: Some(1),
                ..Default::default()
            },
            ceilings: sift_protocol::TenantResourceLimits {
                sessions: Some(1),
                ..Default::default()
            },
        };
        store.set_resource_manager(crate::resources::ResourceManager::new(&config, None));
        let tenant = sift_metadata::TenantId(7);
        let request = || OpenSessionRequest {
            tag: None,
            tenant_id: Some(tenant.0),
        };

        let first = store
            .open_session_with_owner(request(), Some(PrincipalId(9)), Some(tenant), true)
            .unwrap();
        assert_eq!(first.tenant_id, Some(tenant.0));
        assert!(matches!(
            store.open_session_with_owner(request(), Some(PrincipalId(9)), Some(tenant), true),
            Err(ApiError::TenantResourceExhausted {
                resource: sift_protocol::TenantResource::Sessions,
                ..
            })
        ));

        store.close_session(first.id).unwrap();
        assert!(store
            .open_session_with_owner(request(), Some(PrincipalId(9)), Some(tenant), true)
            .is_ok());
    }
}
