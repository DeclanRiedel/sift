//! Session + connection manager. The session store is the orchestrator
//! between HTTP handlers and drivers; it's the only thing that touches
//! `Arc<dyn Driver>` directly. A session is a logical workspace (ADR-002);
//! it holds zero or more open connections.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use sift_driver_api::{ConnHandle, Driver, ResultSetStream};
use sift_protocol::{
    ColumnMetadata, ConnectionId, ConnectionInfo, ConnectionSpec, CursorId, DriverError,
    DriverWarning, Engine, ExecuteRequest, ExecuteResponse, OpenSessionRequest, Page, Row,
    SchemaScope, SchemaSnapshot, ServerInfo, SessionId, SessionInfo,
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
    next_id: AtomicU64,
    registry: DriverRegistry,
}

impl SessionStore {
    pub fn new(registry: DriverRegistry) -> Self {
        Self {
            inner: Arc::new(SessionStoreInner {
                sessions: DashMap::new(),
                next_id: AtomicU64::new(1),
                registry,
            }),
        }
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
        let (_, entry) = self
            .with_session(&session_id, |s| s.connections.remove(&conn_id))?
            .ok_or(ApiError::ConnectionNotFound(conn_id))?;
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
    pub async fn execute(
        &self,
        session_id: SessionId,
        conn_id: ConnectionId,
        req: ExecuteRequest,
    ) -> ApiResult<ExecuteResponse> {
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
        Ok(())
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
            Page::Rows(r) => rows.extend(r),
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

// Trait import marker — kept for the (TBD) transactions endpoint that needs
// TxMode as part of its body shape.
#[allow(unused_imports)]
use sift_protocol::TxMode as _TxModeImportMarker;
