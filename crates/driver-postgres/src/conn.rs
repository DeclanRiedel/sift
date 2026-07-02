//! PgDriver — fat struct holding pool + per-conn state, plus the inner
//! state that spawned query tasks share.

use std::collections::HashMap;
use std::sync::Arc;

use dashmap::DashMap;
use sift_driver_api::{ConnHandle, IdCounter};
use sift_protocol::{Code, ConnectionSpec, DriverError, SslMode, TxId};
use tokio::sync::Mutex;

use deadpool_postgres::Runtime;

pub(crate) type PooledConn = deadpool_postgres::Object;

/// Postgres driver. Cheap to clone (internally `Arc`). Wrap as
/// `Arc<dyn Driver>` for the server registry.
#[derive(Clone)]
pub struct PgDriver {
    pub(crate) inner: Arc<PgDriverInner>,
}

pub(crate) struct PgDriverInner {
    /// conn_id → state. Single mutex guards the map; ops take a conn out
    /// (state becomes `Taken`) for the duration of an async op, restoring
    /// it when the op finishes. Concurrent ops on the same conn return
    /// "conn busy" — sequential-per-conn is the assumed access pattern.
    pub(crate) conns: Mutex<HashMap<u64, ConnState>>,
    /// tx_id → conn_id index, for fast lookup when the caller has a
    /// `TxHandle` (which carries `tx_id`, not `conn_id`).
    pub(crate) tx_index: Mutex<HashMap<u64, u64>>,
    /// cursor_id → cancel token. Read by `cancel` from any task.
    pub(crate) cursors: DashMap<u64, tokio_postgres::CancelToken>,
    pub(crate) conn_id: IdCounter,
    pub(crate) tx_id: IdCounter,
    pub(crate) cursor_id: IdCounter,
}

#[derive(Debug)]
pub(crate) enum ConnState {
    Free(PooledConn),
    InTx { conn: PooledConn, tx_id: u64 },
    Taken,
}

/// Remembered when a conn is taken for an op, so the spawned task knows how
/// to restore the slot (Free vs InTx).
#[derive(Debug, Clone, Copy)]
pub(crate) enum SlotKind {
    Free,
    InTx(u64),
}

impl PgDriver {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(PgDriverInner::new()),
        }
    }
}

impl Default for PgDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl PgDriverInner {
    fn new() -> Self {
        Self {
            conns: Mutex::new(HashMap::new()),
            tx_index: Mutex::new(HashMap::new()),
            cursors: DashMap::new(),
            conn_id: IdCounter::new(),
            tx_id: IdCounter::new(),
            cursor_id: IdCounter::new(),
        }
    }

    /// Open a connection from a fresh ad-hoc pool built from `spec`. Phase 0
    /// simplification: each `open()` builds its own pool of size 8; future
    /// passes cache pools by spec hash (BACKEND.md Tier 0 #15 / Tier 1 #14).
    pub(crate) async fn open_conn(spec: &ConnectionSpec) -> Result<PooledConn, DriverError> {
        let mut cfg = deadpool_postgres::Config::new();
        cfg.host = Some(spec.host.clone());
        cfg.port = spec.port;
        cfg.dbname = spec.database.clone();
        cfg.user = Some(spec.user.clone());
        cfg.password = spec.password.clone();
        cfg.application_name = Some("sift".to_string());
        cfg.ssl_mode = Some(map_ssl_mode(spec.ssl_mode.unwrap_or(SslMode::Prefer)));

        if let Some(sift_protocol::EngineConnectionSpec::Postgres(p)) = &spec.engine_specific {
            if let Some(s) = &p.search_path {
                // `options` propagates as `-c search_path=...` on connect.
                cfg.options = Some(format!("-c search_path={}", s.join(",")));
            }
            if let Some(t) = p.connect_timeout_secs {
                cfg.connect_timeout = Some(std::time::Duration::from_secs(t as u64));
            }
        }
        cfg.pool = Some(deadpool_postgres::PoolConfig {
            max_size: 8,
            timeouts: deadpool_postgres::Timeouts {
                wait: Some(std::time::Duration::from_secs(15)),
                create: Some(std::time::Duration::from_secs(15)),
                recycle: Some(std::time::Duration::from_secs(5)),
            },
            ..Default::default()
        });

        let pool = cfg
            .create_pool(Some(Runtime::Tokio1), tokio_postgres::NoTls)
            .map_err(|e| DriverError::new(Code::ConnectionFailed, e.to_string()))?;

        pool.get().await.map_err(|e| match e {
            deadpool_postgres::PoolError::Backend(backend) => crate::pg_err(backend),
            other => DriverError::new(Code::PoolExhausted, other.to_string()),
        })
    }

    pub(crate) async fn put_free(&self, id: u64, conn: PooledConn) {
        self.conns.lock().await.insert(id, ConnState::Free(conn));
    }

    pub(crate) async fn put_in_tx(&self, conn_id: u64, tx_id: u64, conn: PooledConn) {
        self.conns
            .lock()
            .await
            .insert(conn_id, ConnState::InTx { conn, tx_id });
        self.tx_index.lock().await.insert(tx_id, conn_id);
    }

    /// Take a conn out for an op, marking the slot `Taken`. Returns the conn
    /// plus a `SlotKind` so the caller (or the spawned task) knows how to
    /// restore it. Caller is responsible for `restore_after_op` /
    /// `restore_after_query` / `put_free` / `put_in_tx`.
    pub(crate) async fn take_for_op(
        &self,
        c: &ConnHandle,
    ) -> Result<(PooledConn, SlotKind), DriverError> {
        let mut guard = self.conns.lock().await;
        let entry = guard
            .get_mut(&c.id())
            .ok_or_else(|| DriverError::new(Code::ConnectionFailed, "no conn for handle"))?;
        let slot = std::mem::replace(entry, ConnState::Taken);
        match slot {
            ConnState::Free(conn) => Ok((conn, SlotKind::Free)),
            ConnState::InTx { conn, tx_id } => Ok((conn, SlotKind::InTx(tx_id))),
            ConnState::Taken => Err(DriverError::new(
                Code::DriverInternal,
                "connection is busy with another op",
            )),
        }
    }

    /// Take a conn out of an InTx slot by `tx_id`. Used by savepoint /
    /// rollback_to / commit / rollback. Caller restores via `put_in_tx` or
    /// `put_free`.
    pub(crate) async fn take_in_tx(&self, tx_id: &TxId) -> Option<(u64, PooledConn)> {
        let conn_id = self.tx_index.lock().await.remove(&tx_id.0)?;
        let mut guard = self.conns.lock().await;
        let entry = guard.get_mut(&conn_id)?;
        let slot = std::mem::replace(entry, ConnState::Taken);
        match slot {
            ConnState::InTx { conn, tx_id: _ } => Some((conn_id, conn)),
            // Slot wasn't actually in tx — leave it Taken; caller can debug.
            other => {
                tracing::error!(conn_id, "expected InTx slot, got {:?}", other);
                *entry = other;
                None
            }
        }
    }

    /// Restore a conn to whatever state it was in before the op. Used by
    /// ping/schema/execute-on-free and execute-on-tx paths.
    pub(crate) async fn restore(&self, conn_id: u64, kind: SlotKind, conn: PooledConn) {
        match kind {
            SlotKind::Free => {
                self.conns
                    .lock()
                    .await
                    .insert(conn_id, ConnState::Free(conn));
            }
            SlotKind::InTx(tx_id) => {
                self.conns
                    .lock()
                    .await
                    .insert(conn_id, ConnState::InTx { conn, tx_id });
                self.tx_index.lock().await.insert(tx_id, conn_id);
            }
        }
    }

    pub(crate) async fn remove_conn(&self, c: &ConnHandle) {
        // Drop from whichever map holds it.
        let mut guard = self.conns.lock().await;
        if let Some(ConnState::InTx { tx_id, .. }) = guard.remove(&c.id()) {
            self.tx_index.lock().await.remove(&tx_id);
        }
    }
}

impl PgDriver {
    pub(crate) async fn open_internal(
        &self,
        spec: &ConnectionSpec,
    ) -> Result<PooledConn, DriverError> {
        PgDriverInner::open_conn(spec).await
    }

    pub(crate) async fn take_for_op(&self, c: &ConnHandle) -> Result<PooledConn, DriverError> {
        Ok(self.inner.take_for_op(c).await?.0)
    }

    pub(crate) async fn restore_after_op(&self, c: &ConnHandle, conn: PooledConn) {
        // We took it from some slot; we put it back as Free. If it was InTx
        // before, that's a logic error — the tx APIs use take_in_tx/put_in_tx
        // explicitly, not this method. For ping/schema/execute the slot was
        // always Free at entry, so restoring as Free is correct.
        self.inner.put_free(c.id(), conn).await;
    }
}

fn map_ssl_mode(m: SslMode) -> deadpool_postgres::SslMode {
    match m {
        SslMode::Disable => deadpool_postgres::SslMode::Disable,
        SslMode::Prefer => deadpool_postgres::SslMode::Prefer,
        // VerifyCa/VerifyFull need deadpool-postgres TLS features (rustls /
        // native-tls) enabled; for Phase 0 we fall back to Require without
        // certificate verification. Tier 1 hardens this (BACKEND.md #12).
        SslMode::Require | SslMode::VerifyCa | SslMode::VerifyFull => {
            deadpool_postgres::SslMode::Require
        }
    }
}
