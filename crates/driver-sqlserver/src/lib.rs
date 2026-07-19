//! `sift-driver-sqlserver` — SQL Server via tiberius (ADR-003).
//!
//! Intentionally conservative: one in-flight operation per connection,
//! streamed result pages, native metadata preserved through the protocol
//! escape hatches.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dashmap::DashMap;
use futures::StreamExt;
use sift_driver_api::{
    BulkOp, BulkResult, ConnHandle, Driver, IdCounter, MssqlExt, ResultSetStream, TxHandle,
};
use sift_protocol::{
    Code, ColumnMetadata, ConnectionSpec, ConstraintInfo, ConstraintKind, CursorId, DriverError,
    Engine, ExecuteRequest, IndexInfo, IndexKind, Nullability, ObjectInfo, ObjectKind,
    PrimitiveType, Row, SchemaDepth, SchemaFilter, SchemaScope, SchemaSnapshot, SchemaTree,
    ServerInfo, TxId, TxMode, TypeCategory, TypeRef, Value,
};
use tiberius::{AuthMethod, Client, ColumnType, Config, EncryptionLevel, QueryItem, ToSql};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::sync::Mutex;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

const ROW_BATCH_SIZE: usize = 128;
const BULK_INSERT_BATCH_ROWS: usize = 256;
const BULK_INSERT_MAX_SQL_BYTES: usize = 512 * 1024;
const MAX_POOLS: usize = 64;

type MssqlConn = Client<Compat<TcpStream>>;

pub struct MssqlDriver {
    inner: Arc<MssqlInner>,
}

struct MssqlInner {
    conns: Mutex<HashMap<u64, MssqlConn>>,
    conn_id: IdCounter,
    tx_id: IdCounter,
    cursor_id: IdCounter,
    cursors: DashMap<u64, (u64, tokio::task::JoinHandle<()>)>,
    /// Per-spec warm-idle pool. `open` first tries to pop from
    /// the pool before opening a fresh TDS session. Populated
    /// lazily by a background top-up task after each pop.
    pools: DashMap<String, Arc<Mutex<MssqlPool>>>,
    /// conn_id → pool key that owns this conn's spec. Used to
    /// report `pool_warm_slots` on `ping()`.
    conn_pool_key: DashMap<u64, String>,
}

struct MssqlPool {
    idle: std::collections::VecDeque<MssqlConn>,
    /// Target warm size. When idle drops below this, a background
    /// top-up task refills.
    min_size: usize,
    /// True while a top-up task is running so we don't spawn many
    /// concurrent refills.
    refilling: bool,
}

impl MssqlDriver {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(MssqlInner {
                conns: Mutex::new(HashMap::new()),
                conn_id: IdCounter::new(),
                tx_id: IdCounter::new(),
                cursor_id: IdCounter::new(),
                cursors: DashMap::new(),
                pools: DashMap::new(),
                conn_pool_key: DashMap::new(),
            }),
        }
    }

    /// Pop a warm-idle connection from the per-spec pool if any.
    async fn pop_warm(&self, pool_key: &str) -> Option<MssqlConn> {
        let pool = self.inner.pools.get(pool_key)?.clone();
        loop {
            let mut conn = {
                let mut guard = pool.lock().await;
                guard.idle.pop_front()?
            };
            if validate_warm_conn(&mut conn).await.is_ok() {
                return Some(conn);
            }
            tracing::debug!(pool_key, "discarding stale MSSQL warm connection");
        }
    }

    /// Spawn a background top-up task for the given spec. Idempotent:
    /// the `refilling` flag prevents multiple concurrent tasks from
    /// piling into the same pool.
    fn ensure_warm(&self, spec: ConnectionSpec, pool_key: String, min_size: usize) {
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            enforce_pool_bound(&inner);
            let pool = inner
                .pools
                .entry(pool_key.clone())
                .or_insert_with(|| {
                    Arc::new(Mutex::new(MssqlPool {
                        idle: std::collections::VecDeque::new(),
                        min_size,
                        refilling: false,
                    }))
                })
                .clone();
            {
                let mut guard = pool.lock().await;
                if guard.refilling || guard.idle.len() >= min_size {
                    guard.min_size = min_size;
                    return;
                }
                guard.refilling = true;
                guard.min_size = min_size;
            }
            // Connect outside the lock; retry until we hit min_size or an
            // error occurs. Wrap the refill in catch_unwind so a panic in
            // the loop can't leave `refilling = true` forever — which would
            // wedge the pool permanently cold (every later ensure_warm
            // bails on the flag). The reset below runs on both the normal
            // and the unwound path.
            let refill = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(async {
                loop {
                    let need = {
                        let g = pool.lock().await;
                        min_size.saturating_sub(g.idle.len())
                    };
                    if need == 0 {
                        break;
                    }
                    match connect_fresh(&spec).await {
                        Ok(conn) => {
                            let mut g = pool.lock().await;
                            g.idle.push_back(conn);
                        }
                        Err(error) => {
                            tracing::debug!(%error, "mssql pool refill failed");
                            break;
                        }
                    }
                }
            }))
            .await;
            {
                let mut g = pool.lock().await;
                g.refilling = false;
            }
            if refill.is_err() {
                tracing::error!("mssql pool refill task panicked; refilling flag reset");
            }
        });
    }

    /// Number of warm-idle conns sitting in the pool for the spec that
    /// opened `conn_id`. Returns `None` if we've lost the mapping
    /// (post-close) or no pool exists yet for that spec.
    async fn pool_warm_slots_for(&self, conn_id: u64) -> Option<u32> {
        let key = self.inner.conn_pool_key.get(&conn_id).map(|s| s.clone())?;
        let pool = self.inner.pools.get(&key)?.clone();
        let guard = pool.lock().await;
        Some(guard.idle.len().min(u32::MAX as usize) as u32)
    }

    async fn take_conn(&self, c: &ConnHandle) -> Result<MssqlConn, DriverError> {
        self.inner
            .conns
            .lock()
            .await
            .remove(&c.id())
            .ok_or_else(|| DriverError::new(Code::ConnectionFailed, "no conn for handle"))
    }

    async fn put_conn(&self, c: &ConnHandle, conn: MssqlConn) {
        self.inner.conns.lock().await.insert(c.id(), conn);
    }
}

impl Default for MssqlDriver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Driver for MssqlDriver {
    fn engine(&self) -> Engine {
        Engine::SqlServer
    }

    #[tracing::instrument(skip_all, fields(engine = "sql_server", host = %spec.host))]
    async fn open(&self, spec: &ConnectionSpec) -> Result<ConnHandle, DriverError> {
        // Try the warm-idle pool first. Fall back to a fresh TDS
        // session on miss.
        let (pool_key, min_size) = pool_config(spec);
        let conn = if let Some(warm) = self.pop_warm(&pool_key).await {
            warm
        } else {
            connect_fresh(spec).await?
        };
        let id = self.inner.conn_id.next();
        self.inner.conns.lock().await.insert(id, conn);
        self.inner.conn_pool_key.insert(id, pool_key.clone());
        // Kick off a background top-up so subsequent opens for this
        // spec find a warm entry.
        if min_size > 0 {
            self.ensure_warm(spec.clone(), pool_key, min_size);
        }
        Ok(ConnHandle::new(id, Engine::SqlServer))
    }

    #[tracing::instrument(skip_all, fields(engine = "sql_server", conn = c.id()))]
    async fn ping(&self, c: ConnHandle) -> Result<ServerInfo, DriverError> {
        let mut conn = self.take_conn(&c).await?;
        let warm_slots = self.pool_warm_slots_for(c.id()).await;
        let result = async {
            let row = conn
                .query(
                    "SELECT @@VERSION AS version, DB_NAME() AS database_name, SUSER_SNAME() AS user_name",
                    &[],
                )
                .await
                .map_err(ms_err)?
                .into_row()
                .await
                .map_err(ms_err)?
                .ok_or_else(|| DriverError::new(Code::DriverInternal, "ping returned no row"))?;
            Ok::<_, DriverError>(ServerInfo {
                engine: Engine::SqlServer,
                server_version: row.try_get::<&str, _>(0).map_err(ms_err)?.unwrap_or_default().to_string(),
                current_database: row.try_get::<&str, _>(1).map_err(ms_err)?.unwrap_or_default().to_string(),
                current_user: row.try_get::<&str, _>(2).map_err(ms_err)?.unwrap_or_default().to_string(),
                pool_warm_slots: warm_slots,
            })
        }
        .await;
        self.put_conn(&c, conn).await;
        result
    }

    #[tracing::instrument(skip_all, fields(engine = "sql_server", conn = c.id(), depth = ?scope.depth))]
    async fn schema(
        &self,
        c: ConnHandle,
        scope: SchemaScope,
    ) -> Result<SchemaSnapshot, DriverError> {
        let mut conn = self.take_conn(&c).await?;
        let result = mssql_schema(&mut conn, scope).await;
        self.put_conn(&c, conn).await;
        result
    }

    #[tracing::instrument(skip_all, fields(engine = "sql_server", conn = c.id()))]
    async fn begin(&self, c: ConnHandle, mode: TxMode) -> Result<TxHandle, DriverError> {
        let mut conn = self.take_conn(&c).await?;
        conn.simple_query("BEGIN TRANSACTION")
            .await
            .map_err(ms_err)?;
        let tx_id = TxId::new(self.inner.tx_id.next());
        self.put_conn(&c, conn).await;
        Ok(TxHandle::new(tx_id, c, mode))
    }

    #[tracing::instrument(skip_all, fields(engine = "sql_server", tx = t.tx_id.0))]
    async fn commit(&self, t: TxHandle) -> Result<(), DriverError> {
        let mut conn = self.take_conn(&t.conn).await?;
        let result = async {
            conn.simple_query("COMMIT TRANSACTION")
                .await
                .map_err(ms_err)?
                .into_results()
                .await
                .map_err(ms_err)?;
            Ok::<_, DriverError>(())
        }
        .await;
        self.put_conn(&t.conn, conn).await;
        result
    }

    #[tracing::instrument(skip_all, fields(engine = "sql_server", tx = t.tx_id.0))]
    async fn rollback(&self, t: TxHandle) -> Result<(), DriverError> {
        let mut conn = self.take_conn(&t.conn).await?;
        let result = async {
            conn.simple_query("ROLLBACK TRANSACTION")
                .await
                .map_err(ms_err)?
                .into_results()
                .await
                .map_err(ms_err)?;
            Ok::<_, DriverError>(())
        }
        .await;
        self.put_conn(&t.conn, conn).await;
        result
    }

    #[tracing::instrument(skip_all, fields(engine = "sql_server", conn = c.id()))]
    async fn execute(
        &self,
        c: ConnHandle,
        req: ExecuteRequest,
    ) -> Result<ResultSetStream, DriverError> {
        let conn = self.take_conn(&c).await?;
        let cursor_id = CursorId::new(self.inner.cursor_id.next());
        let (tx, rx) = mpsc::channel(1);
        let inner = Arc::clone(&self.inner);
        let cursor_key = cursor_id.0;
        let conn_id = c.id();
        let task = tokio::spawn(async move {
            // Isolate panics so a wedged decode path produces a `Page::Error`
            // instead of silently dropping the channel. Parity with PG's
            // `run_query` wrapper.
            let cursor_key = cursor_id.0;
            let cleanup_inner = Arc::clone(&inner);
            let error_tx = tx.clone();
            let fut =
                std::panic::AssertUnwindSafe(run_query(inner, conn_id, conn, cursor_id, req, tx));
            if let Err(panic) = futures::FutureExt::catch_unwind(fut).await {
                let msg = panic_message(panic);
                tracing::error!(cursor_key, "sqlserver query task panicked: {msg}");
                let _ = error_tx
                    .send(sift_protocol::Page::Error {
                        error: DriverError::new(
                            Code::DriverInternal,
                            format!("query task panicked: {msg}"),
                        )
                        .with_engine(Engine::SqlServer),
                    })
                    .await;
                // Connection was consumed by the panicking future; do not
                // restore it. Just drop the cursor entry so a subsequent
                // `close()` doesn't try to abort a dead task.
                cleanup_inner.cursors.remove(&cursor_key);
            }
        });
        self.inner.cursors.insert(cursor_key, (conn_id, task));
        Ok(ResultSetStream::with_cursor_mode(cursor_id, rx, false))
    }

    #[tracing::instrument(skip_all, fields(engine = "sql_server", cursor = cursor.0))]
    async fn cancel(&self, c: ConnHandle, cursor: CursorId) -> Result<(), DriverError> {
        // Ownership check first: peek at the entry, refuse cancels for a
        // cursor that does not belong to this ConnHandle. Cursor ids are
        // monotonic across all conns, so without this an authenticated
        // caller with any ConnHandle could cancel another user's query.
        {
            let entry = self.inner.cursors.get(&cursor.0).ok_or_else(|| {
                DriverError::new(Code::CursorNotFound, "cursor not active")
                    .with_engine(Engine::SqlServer)
            })?;
            if entry.value().0 != c.id() {
                return Err(DriverError::new(
                    Code::CursorNotFound,
                    "cursor does not belong to this connection",
                )
                .with_engine(Engine::SqlServer));
            }
        }
        let Some((_, (conn_id, task))) = self.inner.cursors.remove(&cursor.0) else {
            return Err(DriverError::new(Code::CursorNotFound, "cursor not active")
                .with_engine(Engine::SqlServer));
        };
        task.abort();
        // Aborting the task drops the MssqlConn owned by its future, so
        // nothing will ever reinsert into `inner.conns` for this conn_id.
        // Evict any residue so the driver's internal invariant matches the
        // "abort+discard" contract (SqlServer cancel = connection dies):
        // remove the conns map entry (defensive; `take_conn` in execute()
        // already removed it), and drop any other cursors that were
        // registered against the same conn_id.
        self.inner.conns.lock().await.remove(&conn_id);
        let stray: Vec<u64> = self
            .inner
            .cursors
            .iter()
            .filter_map(|entry| {
                if entry.value().0 == conn_id {
                    Some(*entry.key())
                } else {
                    None
                }
            })
            .collect();
        for cid in stray {
            if let Some((_, (_, t))) = self.inner.cursors.remove(&cid) {
                t.abort();
            }
        }
        Ok(())
    }

    #[tracing::instrument(skip_all, fields(engine = "sql_server", conn = c.id()))]
    async fn close(&self, c: ConnHandle) -> Result<(), DriverError> {
        // Abort in-flight cursor tasks BEFORE we clear the conns map so
        // an aborted task's final `conns.insert` (which would run inside
        // run_query on the happy path) cannot race with our removal.
        let cursors: Vec<u64> = self
            .inner
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
        for cursor_id in cursors {
            if let Some((_, (_, task))) = self.inner.cursors.remove(&cursor_id) {
                task.abort();
            }
        }
        // Yield once so any task that raced past the abort has a chance
        // to complete its final `conns.insert` before we clear.
        tokio::task::yield_now().await;
        self.inner.conns.lock().await.remove(&c.id());
        self.inner.conn_pool_key.remove(&c.id());
        Ok(())
    }

    fn as_mssql(&self) -> Option<&dyn MssqlExt> {
        Some(self)
    }
}

#[async_trait]
impl MssqlExt for MssqlDriver {
    #[tracing::instrument(skip_all, fields(engine = "sql_server", conn = c.id(), db = %db))]
    async fn use_database(&self, c: ConnHandle, db: &str) -> Result<(), DriverError> {
        validate_ident(db)?;
        let mut conn = self.take_conn(&c).await?;
        let sql = format!("USE [{db}]");
        let result = conn.execute(sql, &[]).await.map_err(ms_err);
        self.put_conn(&c, conn).await;
        result.map(|_| ())
    }

    #[tracing::instrument(skip_all, fields(engine = "sql_server", conn = c.id(), table = %op.table, bytes = op.data.len()))]
    async fn bulk_insert(&self, c: ConnHandle, op: BulkOp) -> Result<BulkResult, DriverError> {
        let mut conn = self.take_conn(&c).await?;
        let result = bulk_insert_csv(&mut conn, op).await;
        self.put_conn(&c, conn).await;
        result
    }

    #[tracing::instrument(skip_all, fields(engine = "sql_server", tx = t.tx_id.0, name = %name))]
    async fn savepoint(
        &self,
        t: &TxHandle,
        name: &str,
    ) -> Result<sift_driver_api::MssqlSavepoint, DriverError> {
        validate_ident(name)?;
        let mut conn = self.take_conn(&t.conn).await?;
        let sql = format!("SAVE TRANSACTION [{name}]");
        let result = async {
            conn.simple_query(sql)
                .await
                .map_err(ms_err)?
                .into_results()
                .await
                .map_err(ms_err)?;
            Ok::<_, DriverError>(())
        }
        .await;
        self.put_conn(&t.conn, conn).await;
        result?;
        Ok(sift_driver_api::MssqlSavepoint {
            tx: t.tx_id,
            conn: t.conn.clone(),
            name: name.to_string(),
        })
    }

    #[tracing::instrument(skip_all, fields(engine = "sql_server", tx = sp.tx.0, name = %sp.name))]
    async fn rollback_to(&self, sp: sift_driver_api::MssqlSavepoint) -> Result<(), DriverError> {
        validate_ident(&sp.name)?;
        let mut conn = self.take_conn(&sp.conn).await?;
        let sql = format!("ROLLBACK TRANSACTION [{}]", sp.name);
        let result = async {
            conn.simple_query(sql)
                .await
                .map_err(ms_err)?
                .into_results()
                .await
                .map_err(ms_err)?;
            Ok::<_, DriverError>(())
        }
        .await;
        self.put_conn(&sp.conn, conn).await;
        result
    }
}

async fn run_query(
    inner: Arc<MssqlInner>,
    conn_id: u64,
    mut conn: MssqlConn,
    cursor_id: CursorId,
    req: ExecuteRequest,
    tx: mpsc::Sender<sift_protocol::Page>,
) {
    let result = async {
        let param_boxes = params_to_mssql(req.params)?;
        let param_refs: Vec<&dyn ToSql> = param_boxes
            .iter()
            .map(|p| p.as_ref() as &dyn ToSql)
            .collect();
        // Route pure DML (no OUTPUT clause) through `execute()` so we can
        // report `affected_rows` from `ExecuteResult`. Row-producing SQL
        // (SELECT/WITH/VALUES/EXEC/…) and DML with OUTPUT stay on the
        // streaming `query()` path to preserve returned rows.
        if is_pure_dml(&req.sql) {
            let exec = conn.execute(req.sql, &param_refs).await.map_err(ms_err)?;
            let _ = tx
                .send(sift_protocol::Page::Done {
                    affected_rows: Some(exec.total()),
                    warnings: Vec::new(),
                })
                .await;
            return Ok::<_, DriverError>(());
        }

        let mut stream = conn.query(req.sql, &param_refs).await.map_err(ms_err)?;
        let mut batch = Vec::with_capacity(ROW_BATCH_SIZE);
        while let Some(item) = stream.next().await {
            match item.map_err(ms_err)? {
                QueryItem::Metadata(meta) => {
                    if !batch.is_empty() {
                        let rows = std::mem::take(&mut batch);
                        if tx.send(sift_protocol::Page::Rows { rows }).await.is_err() {
                            return Ok::<_, DriverError>(());
                        }
                    }
                    let columns = meta.columns().iter().map(ms_col).collect();
                    if tx
                        .send(sift_protocol::Page::NextResult { columns })
                        .await
                        .is_err()
                    {
                        return Ok(());
                    }
                }
                QueryItem::Row(row) => {
                    batch.push(ms_row(&row));
                    if batch.len() >= ROW_BATCH_SIZE {
                        let rows = std::mem::take(&mut batch);
                        if tx.send(sift_protocol::Page::Rows { rows }).await.is_err() {
                            return Ok(());
                        }
                    }
                }
            }
        }
        if !batch.is_empty() {
            let _ = tx.send(sift_protocol::Page::Rows { rows: batch }).await;
        }
        let _ = tx
            .send(sift_protocol::Page::Done {
                affected_rows: None,
                warnings: Vec::new(),
            })
            .await;
        Ok(())
    }
    .await;

    if let Err(error) = result {
        let _ = tx.send(sift_protocol::Page::Error { error }).await;
    }
    inner.cursors.remove(&cursor_id.0);
    inner.conns.lock().await.insert(conn_id, conn);
    tracing::debug!(%cursor_id, conn_id, "sqlserver query finished");
}

/// True when `sql` is a pure DML statement (INSERT/UPDATE/DELETE/MERGE)
/// with no OUTPUT clause — the case where `execute()`'s row-count is
/// useful and we don't lose returned rows by skipping the streaming path.
fn is_pure_dml(sql: &str) -> bool {
    let up = sql.trim_start().to_ascii_uppercase();
    let mut tokens = up.split_whitespace();
    let first = tokens.next().unwrap_or("");
    if !matches!(first, "INSERT" | "UPDATE" | "DELETE" | "MERGE") {
        return false;
    }
    // An OUTPUT clause streams rows back; keep those statements on the
    // query() path so we don't discard the returned rows via execute().
    // Match the keyword on any whitespace boundary — the old ` OUTPUT `
    // substring check missed tab/newline-delimited OUTPUT (e.g.
    // "INSERT\tOUTPUT\t..."), routing it through execute() and losing the
    // rows entirely.
    !tokens.any(|token| token == "OUTPUT")
}

/// Extract a usable message from a panic payload.
fn panic_message(p: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&'static str>() {
        return (*s).to_string();
    }
    if let Some(s) = p.downcast_ref::<String>() {
        return s.clone();
    }
    "<non-string panic payload>".to_string()
}

async fn mssql_schema(
    conn: &mut MssqlConn,
    scope: SchemaScope,
) -> Result<SchemaSnapshot, DriverError> {
    let mut snapshot = SchemaSnapshot::empty(scope.clone());

    match &scope.depth {
        SchemaDepth::Shallow => {
            // `sys.objects` covers tables, views, procs, funcs, synonyms,
            // sequences in one shot — INFORMATION_SCHEMA.TABLES only
            // reports tables and views.
            let rows = conn
                .query(
                    r#"
SELECT s.name AS schema_name, o.name AS object_name, o.type AS object_type
FROM sys.objects o
JOIN sys.schemas s ON s.schema_id = o.schema_id
WHERE o.type IN ('U','V','P','IF','FN','TF','SN','SO')
ORDER BY s.name, o.name
"#,
                    &[],
                )
                .await
                .map_err(ms_err)?
                .into_first_result()
                .await
                .map_err(ms_err)?;

            let mut by_schema: std::collections::BTreeMap<String, Vec<ObjectInfo>> =
                Default::default();
            for row in rows {
                let schema = row
                    .try_get::<&str, _>(0)
                    .map_err(ms_err)?
                    .unwrap_or("dbo")
                    .to_string();
                let name = row
                    .try_get::<&str, _>(1)
                    .map_err(ms_err)?
                    .unwrap_or_default()
                    .to_string();
                let object_type = row
                    .try_get::<&str, _>(2)
                    .map_err(ms_err)?
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                let kind = mssql_object_kind_from_sys(&object_type);
                if !schema_filter_matches(scope.filter.as_ref(), &schema, &name, kind) {
                    continue;
                }
                by_schema
                    .entry(schema)
                    .or_default()
                    .push(ObjectInfo::new(name, kind));
            }
            snapshot.trees.push(sift_protocol::CatalogTree {
                name: "default".to_string(),
                schemas: by_schema
                    .into_iter()
                    .map(|(name, objects)| SchemaTree { name, objects })
                    .collect(),
            });
        }
        SchemaDepth::Deep { object } => {
            let schema = object.schema.as_deref().unwrap_or("dbo");
            let mut obj = ObjectInfo::new(
                object.name.clone(),
                object.kind.unwrap_or(ObjectKind::Table),
            );
            obj.columns = mssql_columns(conn, schema, &object.name).await?;
            obj.indexes = mssql_indexes(conn, schema, &object.name).await?;
            obj.constraints = mssql_constraints(conn, schema, &object.name).await?;
            obj.triggers = mssql_triggers(conn, schema, &object.name).await?;
            snapshot.trees.push(sift_protocol::CatalogTree {
                name: object
                    .catalog
                    .clone()
                    .unwrap_or_else(|| "default".to_string()),
                schemas: vec![SchemaTree {
                    name: schema.to_string(),
                    objects: vec![obj],
                }],
            });
        }
    }
    Ok(snapshot)
}

async fn mssql_columns(
    conn: &mut MssqlConn,
    schema: &str,
    object: &str,
) -> Result<Vec<ColumnMetadata>, DriverError> {
    let rows = conn
        .query(
            r#"
SELECT
  c.COLUMN_NAME,
  c.DATA_TYPE,
  c.IS_NULLABLE,
  COLUMNPROPERTY(OBJECT_ID(QUOTENAME(c.TABLE_SCHEMA) + '.' + QUOTENAME(c.TABLE_NAME)), c.COLUMN_NAME, 'IsIdentity') AS IS_IDENTITY,
  CASE WHEN pk.COLUMN_NAME IS NULL THEN CAST(0 AS bit) ELSE CAST(1 AS bit) END AS IS_PK,
  c.CHARACTER_MAXIMUM_LENGTH,
  c.COLLATION_NAME
FROM INFORMATION_SCHEMA.COLUMNS c
LEFT JOIN (
  SELECT ku.TABLE_SCHEMA, ku.TABLE_NAME, ku.COLUMN_NAME
  FROM INFORMATION_SCHEMA.TABLE_CONSTRAINTS tc
  JOIN INFORMATION_SCHEMA.KEY_COLUMN_USAGE ku
    ON ku.CONSTRAINT_SCHEMA = tc.CONSTRAINT_SCHEMA
   AND ku.CONSTRAINT_NAME = tc.CONSTRAINT_NAME
  WHERE tc.CONSTRAINT_TYPE = 'PRIMARY KEY'
) pk
  ON pk.TABLE_SCHEMA = c.TABLE_SCHEMA
 AND pk.TABLE_NAME = c.TABLE_NAME
 AND pk.COLUMN_NAME = c.COLUMN_NAME
WHERE c.TABLE_SCHEMA = @P1 AND c.TABLE_NAME = @P2
ORDER BY c.ORDINAL_POSITION
"#,
            &[&schema, &object],
        )
        .await
        .map_err(ms_err)?
        .into_first_result()
        .await
        .map_err(ms_err)?;

    rows.into_iter()
        .map(|row| {
            let name = row
                .try_get::<&str, _>(0)
                .map_err(ms_err)?
                .unwrap_or_default()
                .to_string();
            let type_name = row
                .try_get::<&str, _>(1)
                .map_err(ms_err)?
                .unwrap_or_default();
            let nullable = match row.try_get::<&str, _>(2).map_err(ms_err)?.unwrap_or("YES") {
                "NO" => Nullability::NotNullable,
                "YES" => Nullability::Nullable,
                _ => Nullability::Unknown,
            };
            let auto_increment = row.try_get::<i32, _>(3).map_err(ms_err)?.unwrap_or(0) == 1;
            let primary_key = row.try_get::<bool, _>(4).map_err(ms_err)?.unwrap_or(false);
            let max_length = row
                .try_get::<i32, _>(5)
                .map_err(ms_err)?
                .and_then(|v| u32::try_from(v).ok());
            let collation = row
                .try_get::<&str, _>(6)
                .map_err(ms_err)?
                .map(str::to_string);
            Ok(ColumnMetadata {
                name,
                type_ref: mssql_type_name_ref(type_name),
                nullable,
                auto_increment,
                primary_key,
                facets: sift_protocol::EngineColumnFacets {
                    postgres: None,
                    sql_server: Some(sift_protocol::MssqlColumnFacets {
                        tds_type: Some(type_name.to_string()),
                        collation,
                        max_length,
                    }),
                },
            })
        })
        .collect()
}

async fn mssql_indexes(
    conn: &mut MssqlConn,
    schema: &str,
    object: &str,
) -> Result<Vec<IndexInfo>, DriverError> {
    let rows = conn
        .query(
            r#"
SELECT i.name, i.is_unique, i.is_primary_key, c.name, CAST(i.type AS int)
FROM sys.indexes i
JOIN sys.objects o ON o.object_id = i.object_id
JOIN sys.schemas s ON s.schema_id = o.schema_id
JOIN sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id
JOIN sys.columns c ON c.object_id = i.object_id AND c.column_id = ic.column_id
WHERE s.name = @P1 AND o.name = @P2 AND i.name IS NOT NULL AND i.is_hypothetical = 0
ORDER BY i.index_id, ic.key_ordinal
"#,
            &[&schema, &object],
        )
        .await
        .map_err(ms_err)?
        .into_first_result()
        .await
        .map_err(ms_err)?;

    let mut map: std::collections::BTreeMap<String, IndexInfo> = Default::default();
    for row in rows {
        let name = row
            .try_get::<&str, _>(0)
            .map_err(ms_err)?
            .unwrap_or_default()
            .to_string();
        let unique = row.try_get::<bool, _>(1).map_err(ms_err)?.unwrap_or(false);
        let primary_key = row.try_get::<bool, _>(2).map_err(ms_err)?.unwrap_or(false);
        let column = row
            .try_get::<&str, _>(3)
            .map_err(ms_err)?
            .unwrap_or_default()
            .to_string();
        let sys_type = row.try_get::<i32, _>(4).map_err(ms_err)?.unwrap_or(0);
        map.entry(name.clone())
            .and_modify(|idx| idx.columns.push(column.clone()))
            .or_insert_with(|| IndexInfo {
                name,
                columns: vec![column],
                unique,
                primary_key,
                kind: mssql_index_kind_from_sys(sys_type),
                partial_predicate: None,
            });
    }
    Ok(map.into_values().collect())
}

async fn mssql_constraints(
    conn: &mut MssqlConn,
    schema: &str,
    object: &str,
) -> Result<Vec<ConstraintInfo>, DriverError> {
    let rows = conn
        .query(
            r#"
SELECT tc.CONSTRAINT_NAME, tc.CONSTRAINT_TYPE, ku.COLUMN_NAME
FROM INFORMATION_SCHEMA.TABLE_CONSTRAINTS tc
LEFT JOIN INFORMATION_SCHEMA.KEY_COLUMN_USAGE ku
  ON ku.CONSTRAINT_SCHEMA = tc.CONSTRAINT_SCHEMA
 AND ku.CONSTRAINT_NAME = tc.CONSTRAINT_NAME
WHERE tc.TABLE_SCHEMA = @P1 AND tc.TABLE_NAME = @P2
ORDER BY tc.CONSTRAINT_NAME, ku.ORDINAL_POSITION
"#,
            &[&schema, &object],
        )
        .await
        .map_err(ms_err)?
        .into_first_result()
        .await
        .map_err(ms_err)?;

    let mut map: std::collections::BTreeMap<String, ConstraintInfo> = Default::default();
    for row in rows {
        let name = row
            .try_get::<&str, _>(0)
            .map_err(ms_err)?
            .unwrap_or_default()
            .to_string();
        let kind = match row
            .try_get::<&str, _>(1)
            .map_err(ms_err)?
            .unwrap_or_default()
        {
            "PRIMARY KEY" => ConstraintKind::PrimaryKey,
            "FOREIGN KEY" => ConstraintKind::ForeignKey,
            "UNIQUE" => ConstraintKind::Unique,
            "CHECK" => ConstraintKind::Check,
            _ => ConstraintKind::Other,
        };
        let column = row
            .try_get::<&str, _>(2)
            .map_err(ms_err)?
            .map(str::to_string);
        map.entry(name.clone())
            .and_modify(|constraint| {
                if let Some(column) = &column {
                    constraint.columns.push(column.clone());
                }
            })
            .or_insert_with(|| ConstraintInfo {
                name,
                kind,
                columns: column.into_iter().collect(),
                definition: None,
                references: None,
            });
    }
    Ok(map.into_values().collect())
}

async fn mssql_triggers(
    conn: &mut MssqlConn,
    schema: &str,
    object: &str,
) -> Result<Vec<sift_protocol::TriggerInfo>, DriverError> {
    // `sys.trigger_events.type_desc` reports 'INSERT'/'UPDATE'/'DELETE'.
    // SQL Server has AFTER and INSTEAD OF timings — no BEFORE analogue.
    let rows = conn
        .query(
            r#"
SELECT
  t.name,
  t.is_instead_of_trigger,
  te.type_desc,
  OBJECT_DEFINITION(t.object_id) AS definition
FROM sys.triggers t
JOIN sys.trigger_events te ON te.object_id = t.object_id
JOIN sys.objects o ON o.object_id = t.parent_id
JOIN sys.schemas s ON s.schema_id = o.schema_id
WHERE t.parent_class = 1
  AND s.name = @P1
  AND o.name = @P2
ORDER BY t.name, te.type
"#,
            &[&schema, &object],
        )
        .await
        .map_err(ms_err)?
        .into_first_result()
        .await
        .map_err(ms_err)?;

    use sift_protocol::{TriggerEvent, TriggerInfo, TriggerTiming};
    let mut map: std::collections::BTreeMap<String, TriggerInfo> = Default::default();
    for row in rows {
        let name = row
            .try_get::<&str, _>(0)
            .map_err(ms_err)?
            .unwrap_or_default()
            .to_string();
        let is_instead_of = row.try_get::<bool, _>(1).map_err(ms_err)?.unwrap_or(false);
        let timing = if is_instead_of {
            TriggerTiming::InsteadOf
        } else {
            TriggerTiming::After
        };
        let event = match row.try_get::<&str, _>(2).map_err(ms_err)?.unwrap_or("") {
            "INSERT" => Some(TriggerEvent::Insert),
            "UPDATE" => Some(TriggerEvent::Update),
            "DELETE" => Some(TriggerEvent::Delete),
            _ => None,
        };
        let definition = row
            .try_get::<&str, _>(3)
            .map_err(ms_err)?
            .map(str::to_string);

        let entry = map.entry(name.clone()).or_insert_with(|| TriggerInfo {
            name,
            timing,
            events: Vec::new(),
            columns: Vec::new(),
            definition,
        });
        if let Some(event) = event {
            if !entry.events.contains(&event) {
                entry.events.push(event);
            }
        }
    }
    Ok(map.into_values().collect())
}

fn ms_col(col: &tiberius::Column) -> ColumnMetadata {
    ColumnMetadata {
        name: col.name().to_string(),
        type_ref: ms_type_ref(col.column_type()),
        nullable: sift_protocol::Nullability::Unknown,
        auto_increment: false,
        primary_key: false,
        facets: sift_protocol::EngineColumnFacets {
            postgres: None,
            sql_server: Some(sift_protocol::MssqlColumnFacets {
                tds_type: Some(format!("{:?}", col.column_type())),
                collation: None,
                max_length: None,
            }),
        },
    }
}

fn ms_row(row: &tiberius::Row) -> Row {
    let mut values = Vec::with_capacity(row.len());
    for idx in 0..row.len() {
        values.push(ms_value(row, idx));
    }
    Row::new(values)
}

fn ms_value(row: &tiberius::Row, idx: usize) -> Value {
    let ty = row.columns()[idx].column_type();
    match ty {
        ColumnType::Bit | ColumnType::Bitn => ms_decode::<bool>(row, idx, ty, Value::Bool),
        ColumnType::Int1 => ms_decode::<u8>(row, idx, ty, |v| Value::Int16(v as i16)),
        ColumnType::Int2 => ms_decode::<i16>(row, idx, ty, Value::Int16),
        ColumnType::Int4 | ColumnType::Intn => ms_decode::<i32>(row, idx, ty, Value::Int32),
        ColumnType::Int8 => ms_decode::<i64>(row, idx, ty, Value::Int64),
        ColumnType::Float4 | ColumnType::Floatn => ms_decode::<f32>(row, idx, ty, Value::Float32),
        ColumnType::Float8 => ms_decode::<f64>(row, idx, ty, Value::Float64),
        ColumnType::Money => ms_decode::<f64>(row, idx, ty, |v| Value::Decimal(format!("{v:.4}"))),
        ColumnType::Money4 => ms_decode::<f32>(row, idx, ty, |v| Value::Decimal(format!("{v:.4}"))),
        ColumnType::BigVarBin | ColumnType::BigBinary | ColumnType::Image => {
            ms_decode::<&[u8]>(row, idx, ty, |v| Value::Blob(v.to_vec()))
        }
        ColumnType::Guid => ms_decode::<uuid::Uuid>(row, idx, ty, Value::Uuid),
        ColumnType::Daten => ms_decode::<chrono::NaiveDate>(row, idx, ty, Value::Date),
        ColumnType::Timen => ms_decode::<chrono::NaiveTime>(row, idx, ty, Value::Time),
        ColumnType::Datetime
        | ColumnType::Datetime2
        | ColumnType::Datetime4
        | ColumnType::Datetimen => {
            ms_decode::<chrono::NaiveDateTime>(row, idx, ty, Value::Timestamp)
        }
        ColumnType::DatetimeOffsetn => {
            ms_decode::<chrono::DateTime<chrono::FixedOffset>>(row, idx, ty, |v| {
                Value::TimestampTz(v.into())
            })
        }
        ColumnType::SSVariant | ColumnType::Udt => Value::Engine {
            engine: Engine::SqlServer,
            type_name: format!("{ty:?}"),
            display_text: format!("<undecoded {ty:?}>"),
        },
        _ => ms_decode::<&str>(row, idx, ty, |v| Value::Text(v.to_string())),
    }
}

/// Decode one cell as `T` and map it to a [`Value`]. A decode *error* is
/// surfaced as a `Value::Engine { "<decode error: …>" }` placeholder plus
/// a warning log — never silently swallowed to `Null`. This matches the
/// Postgres driver's contract (`decode_cell`); the previous
/// `.ok().flatten()` arms turned every decode failure into an
/// indistinguishable `NULL` (e.g. an `nvarchar(MAX)` that fails UTF-8
/// conversion decoded as `NULL` with no diagnostic). A genuine SQL NULL
/// still maps to `Value::Null`.
fn ms_decode<'a, T>(
    row: &'a tiberius::Row,
    idx: usize,
    ty: ColumnType,
    to_value: impl FnOnce(T) -> Value,
) -> Value
where
    T: tiberius::FromSql<'a>,
{
    match row.try_get::<T, _>(idx) {
        Ok(Some(v)) => to_value(v),
        Ok(None) => Value::Null,
        Err(error) => {
            tracing::warn!(idx, ?ty, %error, "sqlserver cell decode error");
            ms_decode_error_value(ty, error)
        }
    }
}

fn ms_decode_error_value(ty: ColumnType, error: tiberius::error::Error) -> Value {
    Value::Engine {
        engine: Engine::SqlServer,
        type_name: format!("{ty:?}"),
        display_text: format!("<decode error: {error}>"),
    }
}

fn params_to_mssql(params: Vec<Value>) -> Result<Vec<Box<dyn ToSql>>, DriverError> {
    let mut out: Vec<Box<dyn ToSql>> = Vec::with_capacity(params.len());
    for value in params {
        let param: Box<dyn ToSql> = match value {
            Value::Null => Box::new(None::<String>),
            Value::Bool(v) => Box::new(v),
            Value::Int16(v) => Box::new(v),
            Value::Int32(v) => Box::new(v),
            Value::Int64(v) => Box::new(v),
            Value::Float32(v) => Box::new(v),
            Value::Float64(v) => Box::new(v),
            Value::Decimal(v) => Box::new(v),
            Value::Text(v) => Box::new(v),
            Value::Blob(v) => Box::new(v),
            Value::Date(v) => Box::new(v),
            Value::Time(v) => Box::new(v),
            Value::Timestamp(v) => Box::new(v),
            Value::TimestampTz(v) => Box::new(v),
            Value::Uuid(v) => Box::new(v),
            Value::Json(v) => Box::new(v.to_string()),
            Value::Interval(_) | Value::Engine { .. } => {
                return Err(DriverError::new(
                    Code::UnsupportedForEngine,
                    "parameter type is not supported by SQL Server driver yet",
                )
                .with_engine(Engine::SqlServer));
            }
        };
        out.push(param);
    }
    Ok(out)
}

async fn bulk_insert_csv(conn: &mut MssqlConn, op: BulkOp) -> Result<BulkResult, DriverError> {
    let table = quote_qualified_ident(&op.table)?;
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_reader(op.data.as_slice());
    let headers = reader
        .headers()
        .map_err(csv_err)?
        .iter()
        .map(quote_ident)
        .collect::<Result<Vec<_>, _>>()?;
    if headers.is_empty() {
        return Err(DriverError::new(
            Code::InvalidParameterValue,
            "CSV bulk insert requires at least one header column",
        )
        .with_engine(Engine::SqlServer));
    }

    let insert_prefix = format!("INSERT INTO {table} ({}) VALUES ", headers.join(", "));

    // Wrap every batch in one transaction so a mid-run failure leaves no
    // partially-committed rows. Previously batch 3 of 5 failing left
    // batches 1-2 committed with no rollback, and the returned
    // `rows_inserted` reflected rows *attempted*, not committed.
    execute_sql_batch(conn, "BEGIN TRANSACTION").await?;
    match bulk_insert_batches(conn, &insert_prefix, headers.len(), &mut reader).await {
        Ok(rows_inserted) => {
            execute_sql_batch(conn, "COMMIT TRANSACTION").await?;
            Ok(BulkResult { rows_inserted })
        }
        Err(error) => {
            // Best-effort rollback; surface the original failure. If the
            // rollback itself fails (e.g. the connection is gone), the
            // server will reclaim the aborted transaction on disconnect.
            if let Err(rollback_error) = execute_sql_batch(conn, "ROLLBACK TRANSACTION").await {
                tracing::warn!(%rollback_error, "sqlserver bulk-insert rollback failed");
            }
            Err(error)
        }
    }
}

/// Stream CSV records into batched multi-row INSERTs. Runs inside the
/// transaction opened by [`bulk_insert_csv`]; on any error the caller
/// rolls the whole run back.
async fn bulk_insert_batches(
    conn: &mut MssqlConn,
    insert_prefix: &str,
    header_len: usize,
    reader: &mut csv::Reader<&[u8]>,
) -> Result<u64, DriverError> {
    let mut sql = String::with_capacity(insert_prefix.len() + 8192);
    let mut rows_in_batch = 0usize;
    let mut rows_inserted = 0u64;

    for record in reader.records() {
        let record = record.map_err(csv_err)?;
        if record.len() != header_len {
            return Err(DriverError::new(
                Code::InvalidParameterValue,
                "CSV record width does not match header width",
            )
            .with_engine(Engine::SqlServer));
        }
        let row_sql = csv_record_values(&record);
        // Reject a single row that exceeds the byte cap even before the
        // first flush; otherwise a >BULK_INSERT_MAX_SQL_BYTES row would
        // bypass the cap and could OOM the driver.
        if insert_prefix.len() + row_sql.len() > BULK_INSERT_MAX_SQL_BYTES {
            return Err(DriverError::new(
                Code::InvalidParameterValue,
                format!(
                    "single CSV row exceeds bulk insert byte cap ({} > {})",
                    insert_prefix.len() + row_sql.len(),
                    BULK_INSERT_MAX_SQL_BYTES
                ),
            )
            .with_engine(Engine::SqlServer));
        }
        if rows_in_batch > 0
            && (rows_in_batch >= BULK_INSERT_BATCH_ROWS
                || sql.len() + row_sql.len() + 2 > BULK_INSERT_MAX_SQL_BYTES)
        {
            execute_sql_batch(conn, &sql).await?;
            sql.clear();
            rows_in_batch = 0;
        }
        if rows_in_batch == 0 {
            sql.push_str(insert_prefix);
        } else {
            sql.push_str(", ");
        }
        sql.push_str(&row_sql);
        rows_in_batch += 1;
        rows_inserted += 1;
    }

    if rows_in_batch > 0 {
        execute_sql_batch(conn, &sql).await?;
    }
    Ok(rows_inserted)
}

async fn execute_sql_batch(conn: &mut MssqlConn, sql: &str) -> Result<(), DriverError> {
    conn.simple_query(sql)
        .await
        .map_err(ms_err)?
        .into_results()
        .await
        .map_err(ms_err)?;
    Ok(())
}

fn csv_record_values(record: &csv::StringRecord) -> String {
    let mut out = String::with_capacity(record.len() * 8 + 2);
    out.push('(');
    for (idx, field) in record.iter().enumerate() {
        if idx > 0 {
            out.push_str(", ");
        }
        out.push_str(&mssql_literal(field));
    }
    out.push(')');
    out
}

fn mssql_literal(value: &str) -> String {
    // Do NOT collapse "" to NULL — an empty string is a legitimate value
    // and CSV can't distinguish it from NULL anyway. Emit N'' so the
    // column receives an empty string; callers that need NULL support
    // should use a format that can express it.
    format!("N'{}'", value.replace('\'', "''"))
}

fn quote_qualified_ident(name: &str) -> Result<String, DriverError> {
    let parts = name.split('.').collect::<Vec<_>>();
    if parts.is_empty() || parts.len() > 3 {
        return Err(DriverError::new(
            Code::InvalidParameterValue,
            "identifier must be `table`, `schema.table`, or `database.schema.table`",
        )
        .with_engine(Engine::SqlServer));
    }
    parts
        .into_iter()
        .map(quote_ident)
        .collect::<Result<Vec<_>, _>>()
        .map(|parts| parts.join("."))
}

fn quote_ident(name: &str) -> Result<String, DriverError> {
    validate_ident(name)?;
    Ok(format!("[{name}]"))
}

fn csv_err(error: csv::Error) -> DriverError {
    DriverError::new(Code::InvalidParameterValue, error.to_string()).with_engine(Engine::SqlServer)
}

fn ms_type_ref(ty: ColumnType) -> TypeRef {
    let primitive = match ty {
        ColumnType::Bit | ColumnType::Bitn => Some(PrimitiveType::Bool),
        ColumnType::Int1 | ColumnType::Int2 => Some(PrimitiveType::Int16),
        ColumnType::Int4 | ColumnType::Intn => Some(PrimitiveType::Int32),
        ColumnType::Int8 => Some(PrimitiveType::Int64),
        ColumnType::Float4 | ColumnType::Floatn => Some(PrimitiveType::Float32),
        ColumnType::Float8 => Some(PrimitiveType::Float64),
        ColumnType::Decimaln | ColumnType::Numericn | ColumnType::Money | ColumnType::Money4 => {
            Some(PrimitiveType::Decimal)
        }
        ColumnType::BigVarBin | ColumnType::BigBinary | ColumnType::Image => {
            Some(PrimitiveType::Blob)
        }
        ColumnType::Daten => Some(PrimitiveType::Date),
        ColumnType::Timen => Some(PrimitiveType::Time),
        ColumnType::Datetime
        | ColumnType::Datetime2
        | ColumnType::Datetime4
        | ColumnType::Datetimen => Some(PrimitiveType::Timestamp),
        ColumnType::DatetimeOffsetn => Some(PrimitiveType::TimestampTz),
        ColumnType::Guid => Some(PrimitiveType::Uuid),
        ColumnType::Xml => Some(PrimitiveType::Text),
        ColumnType::BigVarChar
        | ColumnType::BigChar
        | ColumnType::NVarchar
        | ColumnType::NChar
        | ColumnType::Text
        | ColumnType::NText => Some(PrimitiveType::Text),
        _ => None,
    };
    primitive
        .map(TypeRef::Primitive)
        .unwrap_or_else(|| TypeRef::Engine {
            engine: Engine::SqlServer,
            name: format!("{ty:?}"),
            category: TypeCategory::Other,
        })
}

fn mssql_type_name_ref(type_name: &str) -> TypeRef {
    let primitive = match type_name.to_ascii_lowercase().as_str() {
        "bit" => Some(PrimitiveType::Bool),
        "tinyint" | "smallint" => Some(PrimitiveType::Int16),
        "int" => Some(PrimitiveType::Int32),
        "bigint" => Some(PrimitiveType::Int64),
        "real" => Some(PrimitiveType::Float32),
        "float" => Some(PrimitiveType::Float64),
        "decimal" | "numeric" | "money" | "smallmoney" => Some(PrimitiveType::Decimal),
        "binary" | "varbinary" | "image" => Some(PrimitiveType::Blob),
        "date" => Some(PrimitiveType::Date),
        "time" => Some(PrimitiveType::Time),
        "datetime" | "datetime2" | "smalldatetime" => Some(PrimitiveType::Timestamp),
        "datetimeoffset" => Some(PrimitiveType::TimestampTz),
        "uniqueidentifier" => Some(PrimitiveType::Uuid),
        "char" | "varchar" | "text" | "nchar" | "nvarchar" | "ntext" | "xml" => {
            Some(PrimitiveType::Text)
        }
        _ => None,
    };
    primitive
        .map(TypeRef::Primitive)
        .unwrap_or_else(|| TypeRef::Engine {
            engine: Engine::SqlServer,
            name: type_name.to_string(),
            category: TypeCategory::Other,
        })
}

/// Map `sys.objects.type` codes onto the engine-neutral `ObjectKind`.
/// Unknown codes fall back to `ObjectKind::Table` — the historical default.
fn mssql_object_kind_from_sys(sys_type: &str) -> ObjectKind {
    match sys_type {
        "U" => ObjectKind::Table,
        "V" => ObjectKind::View,
        "P" => ObjectKind::Procedure,
        "IF" | "FN" => ObjectKind::ScalarFunction,
        "TF" => ObjectKind::TableValuedFunction,
        "SN" => ObjectKind::Synonym,
        "SO" => ObjectKind::Sequence,
        _ => ObjectKind::Table,
    }
}

/// Map `sys.indexes.type` codes onto the engine-neutral `IndexKind`.
/// CLUSTERED/NONCLUSTERED are both rowstore B-trees; hash is memory-
/// optimized hash; columnstore/xml/spatial fall through to `Other`.
fn mssql_index_kind_from_sys(sys_type: i32) -> IndexKind {
    match sys_type {
        1 | 2 => IndexKind::Btree, // CLUSTERED, NONCLUSTERED
        7 => IndexKind::Hash,      // NONCLUSTERED HASH (in-memory OLTP)
        _ => IndexKind::Other,
    }
}

fn schema_filter_matches(
    filter: Option<&SchemaFilter>,
    schema: &str,
    name: &str,
    kind: ObjectKind,
) -> bool {
    let Some(filter) = filter else {
        return true;
    };
    if let Some(schemas) = &filter.schemas {
        if !schemas.iter().any(|s| s == schema) {
            return false;
        }
    }
    if let Some(kinds) = &filter.kinds {
        if !kinds.contains(&kind) {
            return false;
        }
    }
    if let Some(pattern) = &filter.name_pattern {
        if !glob_match(pattern, name) {
            return false;
        }
    }
    true
}

fn glob_match(pattern: &str, value: &str) -> bool {
    fn inner(pattern: &[u8], value: &[u8]) -> bool {
        match pattern.split_first() {
            None => value.is_empty(),
            Some((&b'*', rest)) => {
                inner(rest, value) || (!value.is_empty() && inner(pattern, &value[1..]))
            }
            Some((&b'?', rest)) => !value.is_empty() && inner(rest, &value[1..]),
            Some((&p, rest)) => value.first().is_some_and(|v| *v == p) && inner(rest, &value[1..]),
        }
    }
    inner(pattern.as_bytes(), value.as_bytes())
}

fn validate_ident(name: &str) -> Result<(), DriverError> {
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if valid {
        Ok(())
    } else {
        Err(DriverError::new(
            Code::InvalidParameterValue,
            "invalid identifier",
        ))
    }
}

/// Open a fresh MSSQL connection against `spec`. Extracted from
/// `open` so the pool refill path can reuse it. No handle bookkeeping.
async fn connect_fresh(spec: &ConnectionSpec) -> Result<MssqlConn, DriverError> {
    let mut config = Config::new();
    config.host(&spec.host);
    if let Some(port) = spec.port {
        config.port(port);
    }
    if let Some(database) = &spec.database {
        config.database(database);
    }
    config.application_name("sift");
    config.authentication(AuthMethod::sql_server(
        spec.user.clone(),
        spec.password.clone().unwrap_or_default(),
    ));

    let connect_timeout =
        if let Some(sift_protocol::EngineConnectionSpec::SqlServer(ms)) = &spec.engine_specific {
            if ms.mars {
                return Err(DriverError::new(
                    Code::UnsupportedForEngine,
                    "SQL Server MARS is not supported by the current driver backend",
                )
                .with_engine(Engine::SqlServer));
            }
            if let Some(encrypt) = ms.encrypt {
                config.encryption(if encrypt {
                    EncryptionLevel::Required
                } else {
                    EncryptionLevel::Off
                });
            }
            if ms.trust_server_certificate.unwrap_or(false) {
                config.trust_cert();
            }
            ms.connect_timeout_secs
                .map(|secs| Duration::from_secs(secs as u64))
        } else {
            None
        };

    let tcp = timeout_io(connect_timeout, TcpStream::connect(config.get_addr()))
        .await
        .map_err(io_err)?;
    tcp.set_nodelay(true).map_err(io_err)?;
    timeout_tds(connect_timeout, Client::connect(config, tcp.compat_write())).await
}

/// Canonicalize a ConnectionSpec into a stable pool key and pull out
/// the per-spec warm-idle target. SHA-256 of the JSON so the password
/// isn't held in the DashMap key indefinitely.
fn pool_config(spec: &ConnectionSpec) -> (String, usize) {
    use sha2::{Digest, Sha256};
    let json = serde_json::to_string(spec).unwrap_or_default();
    let hash = Sha256::digest(json.as_bytes());
    let mut key = String::with_capacity(64);
    for b in hash {
        use std::fmt::Write as _;
        let _ = write!(key, "{b:02x}");
    }
    let min = match &spec.engine_specific {
        Some(sift_protocol::EngineConnectionSpec::SqlServer(ms)) => {
            ms.pool_min_size.unwrap_or(0) as usize
        }
        _ => 0,
    };
    (key, min)
}

fn enforce_pool_bound(inner: &Arc<MssqlInner>) {
    if inner.pools.len() < MAX_POOLS {
        return;
    }
    let victims: Vec<String> = inner
        .pools
        .iter()
        .filter_map(|entry| {
            if Arc::strong_count(entry.value()) == 1 {
                Some(entry.key().clone())
            } else {
                None
            }
        })
        .take(inner.pools.len().saturating_sub(MAX_POOLS) + 1)
        .collect();
    for key in victims {
        inner.pools.remove(&key);
    }
}

async fn validate_warm_conn(conn: &mut MssqlConn) -> Result<(), DriverError> {
    conn.query("SELECT 1", &[])
        .await
        .map_err(ms_err)?
        .into_row()
        .await
        .map_err(ms_err)?;
    Ok(())
}

async fn timeout_io<T>(
    timeout: Option<Duration>,
    future: impl Future<Output = std::io::Result<T>>,
) -> std::io::Result<T> {
    match timeout {
        Some(timeout) => tokio::time::timeout(timeout, future).await.map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::TimedOut, "SQL Server connect timed out")
        })?,
        None => future.await,
    }
}

async fn timeout_tds<T>(
    timeout: Option<Duration>,
    future: impl Future<Output = tiberius::Result<T>>,
) -> Result<T, DriverError> {
    match timeout {
        Some(timeout) => tokio::time::timeout(timeout, future)
            .await
            .map_err(|_| {
                DriverError::new(Code::ConnectionFailed, "SQL Server login timed out")
                    .with_engine(Engine::SqlServer)
            })?
            .map_err(ms_err),
        None => future.await.map_err(ms_err),
    }
}

fn io_err(e: std::io::Error) -> DriverError {
    DriverError::new(Code::ConnectionFailed, e.to_string()).with_engine(Engine::SqlServer)
}

/// Map a tiberius error to a `DriverError`, mirroring the granularity of the
/// Postgres driver's `pg_err`. Server-side failures carry a SQL Server error
/// number (`Error::code()`) which we classify via [`mssql_error_code`];
/// transport/parse failures are classified by tiberius `Error` variant.
fn ms_err(e: tiberius::error::Error) -> DriverError {
    use tiberius::error::Error as T;
    let native_code = e.code();
    let code = match native_code {
        // Server responded with a numbered error; classify by number.
        Some(number) => mssql_error_code(number),
        // No server error number: classify by transport/parse variant.
        None => match &e {
            T::Io { .. } | T::Tls(_) | T::Routing { .. } => Code::ConnectionFailed,
            T::Conversion(_) | T::BulkInput(_) => Code::InvalidParameterValue,
            _ => Code::DriverInternal,
        },
    };
    let err = DriverError::new(code, e.to_string()).with_engine(Engine::SqlServer);
    match native_code {
        Some(number) => err.with_sqlstate(number.to_string()),
        None => err,
    }
}

/// Classify a SQL Server error number into a driver `Code`. Numbers are the
/// engine's documented message IDs; grouped by the closest protocol category.
fn mssql_error_code(number: u32) -> Code {
    match number {
        // Login/permission failures.
        18456 | 18452 | 4064 | 916 => Code::AuthFailed,
        // Database/connection unavailable.
        4060 | 40613 | 10054 | 10060 | 233 => Code::ConnectionFailed,
        // Timeouts (lock request / query wait).
        1222 => Code::QueryTimedOut,
        // Deadlock victim — the batch was aborted by the server.
        1205 => Code::QueryCanceled,
        // Syntax errors.
        102 | 105 | 156 | 170 => Code::SyntaxError,
        // Missing object / column / procedure.
        207 | 208 | 2812 | 4701 | 3701 => Code::UndefinedObject,
        // Object/constraint already exists (incl. unique-key violations).
        2627 | 2601 | 2714 | 1913 | 1779 => Code::DuplicateObject,
        // Value/type problems: constraints, conversion, overflow, truncation.
        220 | 232 | 245 | 547 | 8114 | 8115 | 8152 => Code::InvalidParameterValue,
        _ => Code::DriverInternal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_qualified_ident_accepts_common_shapes() {
        assert_eq!(quote_qualified_ident("users").unwrap(), "[users]");
        assert_eq!(quote_qualified_ident("dbo.users").unwrap(), "[dbo].[users]");
        assert_eq!(
            quote_qualified_ident("db.dbo.users").unwrap(),
            "[db].[dbo].[users]"
        );
    }

    #[test]
    fn quote_qualified_ident_rejects_injection() {
        assert!(quote_qualified_ident("dbo.users;drop").is_err());
        assert!(quote_qualified_ident("dbo.[users]").is_err());
        assert!(quote_qualified_ident("a.b.c.d").is_err());
    }

    #[test]
    fn mssql_literal_escapes_text_and_preserves_empty() {
        assert_eq!(mssql_literal(""), "N''");
        assert_eq!(mssql_literal("O'Reilly"), "N'O''Reilly'");
    }

    #[test]
    fn is_pure_dml_routes_dml_and_keeps_output_and_row_producers_streaming() {
        // Plain DML with no OUTPUT → execute() path (affected_rows).
        assert!(is_pure_dml("INSERT INTO t (a) VALUES (1)"));
        assert!(is_pure_dml("update t set a = 1 where id = 2"));
        assert!(is_pure_dml("DELETE FROM t WHERE id = 3"));
        assert!(is_pure_dml("MERGE t USING s ON t.id = s.id WHEN MATCHED THEN UPDATE SET a = 1"));

        // OUTPUT clause streams rows — must stay on the query() path
        // regardless of the whitespace delimiting the keyword. The old
        // ` OUTPUT ` substring check missed tab/newline forms.
        assert!(!is_pure_dml("INSERT INTO t (a) OUTPUT inserted.a VALUES (1)"));
        assert!(!is_pure_dml("INSERT INTO t (a)\tOUTPUT\tinserted.a VALUES (1)"));
        assert!(!is_pure_dml("DELETE FROM t\nOUTPUT deleted.id\nWHERE id = 3"));

        // Row-producing statements are never "pure DML".
        assert!(!is_pure_dml("SELECT * FROM t"));
        assert!(!is_pure_dml("WITH c AS (SELECT 1) SELECT * FROM c"));
    }

    #[test]
    fn mssql_error_code_classifies_known_numbers() {
        assert_eq!(mssql_error_code(18456), Code::AuthFailed);
        assert_eq!(mssql_error_code(4060), Code::ConnectionFailed);
        assert_eq!(mssql_error_code(1222), Code::QueryTimedOut);
        assert_eq!(mssql_error_code(1205), Code::QueryCanceled);
        assert_eq!(mssql_error_code(102), Code::SyntaxError);
        assert_eq!(mssql_error_code(208), Code::UndefinedObject);
        assert_eq!(mssql_error_code(2627), Code::DuplicateObject);
        assert_eq!(mssql_error_code(547), Code::InvalidParameterValue);
        assert_eq!(mssql_error_code(8114), Code::InvalidParameterValue);
    }

    #[test]
    fn mssql_error_code_falls_back_to_internal() {
        assert_eq!(mssql_error_code(999999), Code::DriverInternal);
    }

    #[test]
    fn ms_err_classifies_transport_variants() {
        let io = tiberius::error::Error::Io {
            kind: std::io::ErrorKind::ConnectionReset,
            message: "reset".into(),
        };
        assert_eq!(ms_err(io).code, Code::ConnectionFailed);

        let conv = tiberius::error::Error::Conversion("bad cast".into());
        assert_eq!(ms_err(conv).code, Code::InvalidParameterValue);

        let proto = tiberius::error::Error::Protocol("garbled".into());
        assert_eq!(ms_err(proto).code, Code::DriverInternal);
    }

    #[test]
    fn ms_err_tags_engine() {
        let io = tiberius::error::Error::Io {
            kind: std::io::ErrorKind::ConnectionReset,
            message: "reset".into(),
        };
        assert_eq!(ms_err(io).engine, Some(Engine::SqlServer));
    }

    #[test]
    fn mssql_object_kind_covers_sys_types() {
        assert_eq!(mssql_object_kind_from_sys("U"), ObjectKind::Table);
        assert_eq!(mssql_object_kind_from_sys("V"), ObjectKind::View);
        assert_eq!(mssql_object_kind_from_sys("P"), ObjectKind::Procedure);
        assert_eq!(mssql_object_kind_from_sys("IF"), ObjectKind::ScalarFunction);
        assert_eq!(mssql_object_kind_from_sys("FN"), ObjectKind::ScalarFunction);
        assert_eq!(
            mssql_object_kind_from_sys("TF"),
            ObjectKind::TableValuedFunction
        );
        assert_eq!(mssql_object_kind_from_sys("SN"), ObjectKind::Synonym);
        assert_eq!(mssql_object_kind_from_sys("SO"), ObjectKind::Sequence);
        // Unknown codes are safe (default to Table rather than panic).
        assert_eq!(mssql_object_kind_from_sys("XX"), ObjectKind::Table);
    }

    #[test]
    fn mssql_index_kind_maps_clustered_and_hash() {
        assert_eq!(mssql_index_kind_from_sys(1), IndexKind::Btree); // CLUSTERED
        assert_eq!(mssql_index_kind_from_sys(2), IndexKind::Btree); // NONCLUSTERED
        assert_eq!(mssql_index_kind_from_sys(7), IndexKind::Hash); // hash
        assert_eq!(mssql_index_kind_from_sys(5), IndexKind::Other); // columnstore
        assert_eq!(mssql_index_kind_from_sys(0), IndexKind::Other); // heap
    }

    #[test]
    fn is_pure_dml_recognizes_dml_and_keeps_output_on_query_path() {
        assert!(is_pure_dml("INSERT INTO t VALUES (1)"));
        assert!(is_pure_dml("  update t set x = 1 where id = 2"));
        assert!(is_pure_dml("DELETE FROM t"));
        assert!(is_pure_dml(
            "MERGE t USING s ON s.id = t.id WHEN MATCHED THEN UPDATE SET x = s.x;"
        ));

        // Row-producing statements stay on the streaming path.
        assert!(!is_pure_dml("SELECT * FROM t"));
        assert!(!is_pure_dml("WITH c AS (SELECT 1) SELECT * FROM c"));
        assert!(!is_pure_dml("VALUES (1),(2)"));
        assert!(!is_pure_dml("EXEC sp_who"));

        // OUTPUT clauses stream returned rows — must not route to execute().
        assert!(!is_pure_dml("INSERT INTO t OUTPUT INSERTED.id VALUES (1)"));
        assert!(!is_pure_dml("DELETE FROM t OUTPUT DELETED.id WHERE id = 1"));
    }
}
