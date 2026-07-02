//! Session + connection manager. The session store is the orchestrator
//! between HTTP handlers and drivers; it's the only thing that touches
//! `Arc<dyn Driver>` directly. A session is a logical workspace (ADR-002);
//! it holds zero or more open connections.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use dashmap::DashMap;
use sift_driver_api::{ConnHandle, Driver, ResultSetStream, TxHandle};
use sift_protocol::{
    AuditEntry, BeginTransactionRequest, ColumnMetadata, ConnectionId, ConnectionInfo,
    ConnectionSpec, CursorId, DriverError, DriverWarning, EndTransactionRequest, Engine,
    ExecuteRequest, ExecuteRequestHttp, ExecuteResponse, OpenSessionRequest, Operation,
    OperationAuditEntry, OperationStatus, Page, Row, SchemaScope, SchemaSnapshot, ServerInfo,
    SessionId, SessionInfo, TransactionInfo, TxHandleRef, TxId,
};

use crate::error::{ApiError, ApiResult};
use crate::registry::DriverRegistry;

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
}

struct OperationLog {
    entries: Vec<OperationAuditEntry>,
    writer: Option<File>,
}

impl SessionStore {
    pub fn new(registry: DriverRegistry) -> Self {
        Self {
            inner: Arc::new(SessionStoreInner {
                sessions: DashMap::new(),
                audit: Mutex::new(Vec::new()),
                operations: Mutex::new(OperationLog {
                    entries: Vec::new(),
                    writer: None,
                }),
                next_id: AtomicU64::new(1),
                registry,
            }),
        }
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
        Ok(Self {
            inner: Arc::new(SessionStoreInner {
                sessions: DashMap::new(),
                audit: Mutex::new(Vec::new()),
                operations: Mutex::new(OperationLog {
                    entries,
                    writer: Some(writer),
                }),
                next_id: AtomicU64::new(1),
                registry,
            }),
        })
    }

    pub fn registry(&self) -> &DriverRegistry {
        &self.inner.registry
    }

    pub fn open_session(&self, req: OpenSessionRequest) -> SessionInfo {
        let id = SessionId(self.inner.next_id.fetch_add(1, Ordering::Relaxed));
        let now = chrono::Utc::now();
        let session = Session {
            id,
            created_at: now,
            tag: req.tag.clone(),
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

    pub fn push_operation(&self, operation: Operation, status: OperationStatus) {
        const MAX_OPERATION_ROWS: usize = 10_000;
        let mut log = self.inner.operations.lock().unwrap();
        let entry = OperationAuditEntry {
            at: chrono::Utc::now(),
            operation,
            status,
        };
        if let Some(writer) = &mut log.writer {
            match serde_json::to_writer(&mut *writer, &entry)
                .and_then(|_| writer.write_all(b"\n").map_err(serde_json::Error::io))
                .and_then(|_| writer.flush().map_err(serde_json::Error::io))
            {
                Ok(()) => {}
                Err(error) => tracing::error!(%error, "operation audit append failed"),
            }
        }
        log.entries.push(entry);
        if log.entries.len() > MAX_OPERATION_ROWS {
            let overflow = log.entries.len() - MAX_OPERATION_ROWS;
            log.entries.drain(0..overflow);
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
        let info = entry.driver.ping(entry.handle.clone()).await?;
        Ok(info)
    }

    pub async fn schema(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        scope: SchemaScope,
    ) -> ApiResult<SchemaSnapshot> {
        let entry = self.get_conn_entry(session_id, conn_id)?;
        let snap = entry.driver.schema(entry.handle.clone(), scope).await?;
        Ok(snap)
    }

    /// Synchronous execute: drains the entire page stream into the response.
    /// Suitable for small/medium results. The WS streaming surface (PHASE0
    /// step 10) replaces this for large results.
    pub async fn execute_http(
        &self,
        session_id: SessionId,
        req: ExecuteRequestHttp,
    ) -> ApiResult<ExecuteResponse> {
        let conn_id = req.connection;
        self.validate_execute_tx(session_id, conn_id, req.tx.as_ref())?;
        let req = ExecuteRequest {
            sql: req.sql,
            params: Vec::new(),
        };
        let entry = self.get_conn_entry(session_id, conn_id)?;
        let stream = entry.driver.execute(entry.handle.clone(), req).await?;
        drain_stream(stream).await.map_err(ApiError::Driver)
    }

    pub async fn execute_stream(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        req: ExecuteRequest,
    ) -> ApiResult<ResultSetStream> {
        let entry = self.get_conn_entry(session_id, conn_id)?;
        Ok(entry.driver.execute(entry.handle.clone(), req).await?)
    }

    pub async fn cancel(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        cursor: CursorId,
    ) -> ApiResult<()> {
        let entry = self.get_conn_entry(session_id, conn_id)?;
        entry.driver.cancel(entry.handle.clone(), cursor).await?;
        if entry.driver.engine() == Engine::SqlServer {
            self.with_session(&session_id, |s| s.connections.remove(&conn_id))?;
            tracing::info!(
                session_id = %session_id,
                conn_id = %conn_id,
                "removed sqlserver connection after cancel abort"
            );
        }
        Ok(())
    }

    pub async fn begin_transaction(
        &self,
        session_id: SessionId,
        req: BeginTransactionRequest,
    ) -> ApiResult<TransactionInfo> {
        let entry = self.get_conn_entry(session_id, req.connection)?;
        self.reject_if_connection_has_tx(session_id, req.connection, None)?;
        let handle = entry.driver.begin(entry.handle.clone(), req.mode).await?;
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
        entry.driver.commit(tx.handle).await?;
        Ok(())
    }

    pub async fn rollback_transaction(
        &self,
        session_id: SessionId,
        req: EndTransactionRequest,
    ) -> ApiResult<()> {
        let tx = self.remove_tx(session_id, req.connection, req.tx_id)?;
        let entry = self.get_conn_entry(session_id, req.connection)?;
        entry.driver.rollback(tx.handle).await?;
        Ok(())
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
}

pub struct TransactionEntry {
    pub info: TransactionInfo,
    pub handle: TxHandle,
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
/// streaming layer (PHASE0 step 10) can re-use the drain logic.
pub async fn drain_stream(stream: ResultSetStream) -> Result<ExecuteResponse, DriverError> {
    let cursor_id = stream.cursor_id;
    let rx = stream.rows;
    tokio::pin!(rx);

    let mut columns: Vec<ColumnMetadata> = Vec::new();
    let mut rows: Vec<Row> = Vec::new();
    let mut affected_rows: Option<u64> = None;
    let mut warnings: Vec<DriverWarning> = Vec::new();

    while let Some(page) = rx.recv().await {
        match page {
            Page::NextResult { columns: cols } => columns = cols,
            Page::Rows { rows: r } => rows.extend(r),
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
