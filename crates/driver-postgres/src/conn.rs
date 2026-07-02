//! PgDriver — fat struct holding cached pools + per-conn state, plus the
//! inner state that spawned query tasks share.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Once;

use dashmap::DashMap;
use sift_driver_api::{ConnHandle, IdCounter};
use sift_protocol::{Code, ConnectionSpec, DriverError, SslMode, TxId};
use tokio::sync::Mutex;

use deadpool_postgres::{Pool, Runtime};

pub(crate) type PooledConn = deadpool_postgres::Object;

/// Postgres driver. Cheap to clone (internally `Arc`). Wrap as
/// `Arc<dyn Driver>` for the server registry.
#[derive(Clone)]
pub struct PgDriver {
    pub(crate) inner: Arc<PgDriverInner>,
}

pub(crate) struct PgDriverInner {
    /// conn_id → state. **Single** mutex; no side index. The previous design
    /// had a separate `tx_index` map producing a two-lock window between
    /// `tx_index.remove` and `conns.lock`. Now `ConnState::InTx` carries the
    /// `tx_id` inline, and `find_conn_in_tx` iterates the map. Acceptable at
    /// Phase 0 connection counts; revisit only if profiling shows contention.
    pub(crate) conns: Mutex<HashMap<u64, ConnState>>,
    /// cursor_id → (owning conn_id, cancel token). `conn_id` is carried so
    /// `close` can drain live cursors belonging to the conn.
    pub(crate) cursors: DashMap<u64, (u64, tokio_postgres::CancelToken)>,
    /// Cached pools by canonical connection-spec key. `open()` of an
    /// already-seen spec reuses the pool; identical connections share warm
    /// capacity. String key avoids silent hash-collision pool reuse.
    pub(crate) pools: DashMap<String, Arc<Pool>>,
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
            cursors: DashMap::new(),
            pools: DashMap::new(),
            conn_id: IdCounter::new(),
            tx_id: IdCounter::new(),
            cursor_id: IdCounter::new(),
        }
    }

    /// Borrow or build a pool for the spec. Key is the canonical serde-JSON
    /// form of `ConnectionSpec` so two opens of equivalent specs hit the same
    /// pool without lossy hashing.
    pub(crate) async fn pool_for(
        &self,
        spec: &ConnectionSpec,
    ) -> Result<(String, Arc<Pool>), DriverError> {
        let key = spec_key(spec)?;
        if let Some(pool) = self.pools.get(&key) {
            return Ok((key, Arc::clone(&pool)));
        }
        let mut cfg = deadpool_postgres::Config::new();
        cfg.host = Some(spec.host.clone());
        cfg.port = spec.port;
        cfg.dbname = spec.database.clone();
        cfg.user = Some(spec.user.clone());
        cfg.password = spec.password.clone();
        cfg.application_name = Some("sift".to_string());
        let ssl_mode = spec.ssl_mode.unwrap_or(SslMode::Prefer);
        cfg.ssl_mode = Some(map_ssl_mode(ssl_mode));

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

        let pool = if matches!(ssl_mode, SslMode::VerifyCa | SslMode::VerifyFull) {
            let tls = native_tls_connector()?;
            cfg.create_pool(Some(Runtime::Tokio1), tls)
        } else {
            cfg.create_pool(Some(Runtime::Tokio1), tokio_postgres::NoTls)
        }
        .map_err(|e| DriverError::new(Code::ConnectionFailed, e.to_string()))?;
        let arc = Arc::new(pool);
        // Another opener may have raced us; keep whichever landed first.
        self.pools
            .entry(key.clone())
            .or_insert_with(|| Arc::clone(&arc));
        Ok((key, arc))
    }

    pub(crate) async fn put_free(&self, id: u64, conn: PooledConn) {
        self.conns.lock().await.insert(id, ConnState::Free(conn));
    }

    pub(crate) async fn put_in_tx(&self, conn_id: u64, tx_id: u64, conn: PooledConn) {
        self.conns
            .lock()
            .await
            .insert(conn_id, ConnState::InTx { conn, tx_id });
    }

    /// Take a conn out for an op, marking the slot `Taken`. Returns the conn
    /// plus a `SlotKind` so the caller (or the spawned task) knows how to
    /// restore it. Caller restores via `restore`.
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

    /// Find and take the conn bound to a transaction. Single-lock iteration
    /// of the map (was a two-map dance before). Caller restores via
    /// `put_in_tx` or `put_free`.
    pub(crate) async fn take_in_tx(&self, tx_id: &TxId) -> Option<(u64, PooledConn)> {
        let mut guard = self.conns.lock().await;
        let conn_id = guard.iter().find_map(|(id, state)| match state {
            ConnState::InTx { tx_id: t, .. } if *t == tx_id.0 => Some(*id),
            _ => None,
        })?;
        let entry = guard.get_mut(&conn_id)?;
        let slot = std::mem::replace(entry, ConnState::Taken);
        match slot {
            ConnState::InTx { conn, .. } => Some((conn_id, conn)),
            // Slot wasn't actually InTx — put it back how we found it.
            other => {
                tracing::error!(conn_id, "expected InTx slot, got {:?}", other);
                *entry = other;
                None
            }
        }
    }

    /// Restore a conn to whatever state it was in before the op.
    pub(crate) async fn restore(&self, conn_id: u64, kind: SlotKind, conn: PooledConn) {
        let state = match kind {
            SlotKind::Free => ConnState::Free(conn),
            SlotKind::InTx(tx_id) => ConnState::InTx { conn, tx_id },
        };
        self.conns.lock().await.insert(conn_id, state);
    }

    pub(crate) async fn remove_conn(&self, c: &ConnHandle) {
        // Drop the slot.
        if let Some(ConnState::InTx { .. }) = self.conns.lock().await.remove(&c.id()) {
            // The tx is implicitly aborted by connection close; surface as
            // tracing only — caller decides whether that's an error.
            tracing::warn!(conn_id = c.id(), "closing conn with open transaction");
        }
        // Drain live cursors belonging to this conn. The spawned query tasks
        // will observe socket close and finish their Page::Done themselves.
        let to_remove: Vec<u64> = self
            .cursors
            .iter()
            .filter_map(|entry| {
                if entry.value().0 == c.id() {
                    Some(*entry.key())
                } else {
                    None
                }
            })
            .collect();
        for cursor_id in to_remove {
            self.cursors.remove(&cursor_id);
        }
    }
}

impl PgDriver {
    pub(crate) async fn open_internal(
        &self,
        spec: &ConnectionSpec,
    ) -> Result<PooledConn, DriverError> {
        let (_hash, pool) = self.inner.pool_for(spec).await?;
        pool.get().await.map_err(|e| match e {
            deadpool_postgres::PoolError::Backend(backend) => crate::pg_err(backend),
            other => DriverError::new(Code::PoolExhausted, other.to_string()),
        })
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
        SslMode::Require => deadpool_postgres::SslMode::Require,
        SslMode::VerifyCa | SslMode::VerifyFull => deadpool_postgres::SslMode::Require,
    }
}

fn native_tls_connector() -> Result<tokio_postgres_rustls::MakeRustlsConnect, DriverError> {
    static INSTALL_PROVIDER: Once = Once::new();
    INSTALL_PROVIDER.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
    tokio_postgres_rustls::MakeRustlsConnect::with_native_certs()
        .map(|(tls, errors)| {
            for error in errors {
                tracing::warn!(%error, "error loading a native certificate");
            }
            tls
        })
        .map_err(|errors| {
            DriverError::new(
                Code::ConnectionFailed,
                format!("failed to load native TLS roots: {errors:?}"),
            )
            .with_engine(sift_protocol::Engine::Postgres)
        })
}

/// Canonical pool key. Serialization failure is an internal error because
/// `ConnectionSpec` is a protocol type with derived Serialize.
fn spec_key(spec: &ConnectionSpec) -> Result<String, DriverError> {
    serde_json::to_string(spec).map_err(|e| DriverError::new(Code::DriverInternal, e.to_string()))
}
