//! `sift-driver-api` — the core [`Driver`] trait, the runtime handle types
//! ([`ConnHandle`], [`TxHandle`], [`ResultSetStream`]), and the engine-
//! specific extension trait declarations ([`PgExt`], [`MssqlExt`]).
//!
//! The trait is object-safe: every method takes `&self`, returns a boxed
//! future via `async_trait`, and returns a concrete protocol-crate type.
//! That lets the server hold a `HashMap<Engine, Arc<dyn Driver>>` registry.
//!
//! Extension trait **declarations** live here (so the `as_pg` / `as_mssql`
//! default downcasts on `Driver` can name them), but their **impls** live
//! in the corresponding driver crate (`driver-postgres`, `driver-sqlserver`).
//! Wrong engine returns `None`; the server translates that into
//! `Code::UnsupportedForEngine`, never a silent no-op.

use std::sync::Arc;

use sift_protocol::{
    ColumnMetadata, ConnectionSpec, CursorId, DriverError, DriverWarning, Engine, ExecuteRequest,
    Page, SchemaScope, SchemaSnapshot, ServerInfo, TxId, TxMode,
};
use tokio::sync::mpsc;

// Re-export so callers can use `sift_driver_api::TxMode` etc. without
// reaching into the protocol crate by hand.
pub use sift_protocol::{
    IsolationLevel, ObjectKind, ObjectPath, SchemaDepth, SchemaFilter, TxAccessMode,
};

/// Opaque connection handle. Concrete newtype over `Arc<ConnHandleInner>`
/// so the trait stays object-safe (no associated type). The driver maps
/// `id` to its own typed connection in its own internal map.
///
/// This does not carry a `Weak<dyn Driver>` backref. The server's registry
/// is in scope wherever a ConnHandle is used; that suffices. The backref is
/// a documented ADR-017 candidate for an explicit future pass if cross-task
/// cancel/close without the registry in scope turns out to be a real need.
#[derive(Clone)]
pub struct ConnHandle(Arc<ConnHandleInner>);

struct ConnHandleInner {
    id: u64,
    engine: Engine,
}

impl ConnHandle {
    pub fn new(id: u64, engine: Engine) -> Self {
        Self(Arc::new(ConnHandleInner { id, engine }))
    }

    pub fn id(&self) -> u64 {
        self.0.id
    }

    pub fn engine(&self) -> Engine {
        self.0.engine
    }
}

impl std::fmt::Debug for ConnHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnHandle")
            .field("id", &self.0.id)
            .field("engine", &self.0.engine)
            .finish()
    }
}

/// Handle to an open transaction. Holds the connection it was opened on;
/// `commit` / `rollback` consume it and return the connection to the pool.
#[derive(Clone, Debug)]
pub struct TxHandle {
    pub tx_id: TxId,
    pub conn: ConnHandle,
    pub mode: TxMode,
}

impl TxHandle {
    pub fn new(tx_id: TxId, conn: ConnHandle, mode: TxMode) -> Self {
        Self { tx_id, conn, mode }
    }
}

/// Result of [`Driver::execute`]. Owns the page receiver; the consumer
/// reads pages until `Page::Done` (or stream close). The server is the
/// sole consumer of `rows`; multi-subscriber fan-out is a Tier 3 server
/// concern, not a driver concern.
pub struct ResultSetStream {
    pub cursor_id: CursorId,
    /// Columns of the current result set. Empty until the first
    /// `Page::NextResult` arrives; populated by the consumer.
    pub columns: Vec<ColumnMetadata>,
    pub rows: mpsc::Receiver<Page>,
    pub warnings: Vec<DriverWarning>,
    pub affected_rows: Option<u64>,
    /// `false` = small result already buffered; `true` = driver holds a
    /// server-side cursor and pages on demand.
    pub server_side_cursor: bool,
}

impl ResultSetStream {
    pub fn new(cursor_id: CursorId, rows: mpsc::Receiver<Page>) -> Self {
        Self::with_cursor_mode(cursor_id, rows, true)
    }

    pub fn with_cursor_mode(
        cursor_id: CursorId,
        rows: mpsc::Receiver<Page>,
        server_side_cursor: bool,
    ) -> Self {
        Self {
            cursor_id,
            columns: Vec::new(),
            rows,
            warnings: Vec::new(),
            affected_rows: None,
            server_side_cursor,
        }
    }
}

impl std::fmt::Debug for ResultSetStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResultSetStream")
            .field("cursor_id", &self.cursor_id)
            .field("columns_len", &self.columns.len())
            .field("server_side_cursor", &self.server_side_cursor)
            .finish()
    }
}

/// The core driver trait. Eight verbs every supported engine implements.
///
/// Object-safe: `&self` everywhere, futures via `async_trait`, all return
/// types concrete. → `Box<dyn Driver>` and `Arc<dyn Driver>` both work.
#[async_trait::async_trait]
pub trait Driver: Send + Sync {
    /// Which engine this driver serves. Used by the server's registry and
    /// by clients to pick the right ext-trait downcast.
    fn engine(&self) -> Engine;

    /// Open a new logical connection. Drivers may satisfy this from a pool
    /// or by dialing a fresh backend session, but the returned handle maps
    /// to one typed connection owned by that driver.
    async fn open(&self, spec: &ConnectionSpec) -> Result<ConnHandle, DriverError>;

    /// Cheap liveness check + server-reported metadata.
    async fn ping(&self, c: ConnHandle) -> Result<ServerInfo, DriverError>;

    /// Introspect the catalog tree at the requested depth.
    async fn schema(
        &self,
        c: ConnHandle,
        scope: SchemaScope,
    ) -> Result<SchemaSnapshot, DriverError>;

    /// Begin a transaction. The returned handle holds its connection;
    /// `commit` / `rollback` consume it.
    async fn begin(&self, c: ConnHandle, mode: TxMode) -> Result<TxHandle, DriverError>;

    async fn commit(&self, t: TxHandle) -> Result<(), DriverError>;

    async fn rollback(&self, t: TxHandle) -> Result<(), DriverError>;

    /// Execute SQL. Returns a stream handle, not rows. Pages arrive via
    /// `ResultSetStream::rows`; the first `Page::NextResult` declares the
    /// column layout.
    async fn execute(
        &self,
        c: ConnHandle,
        req: ExecuteRequest,
    ) -> Result<ResultSetStream, DriverError>;

    /// Cancel an in-flight query. Must be safe to call from a different
    /// task than `execute` ran on.
    async fn cancel(&self, c: ConnHandle, cursor: CursorId) -> Result<(), DriverError>;

    /// Close the underlying DB connection cleanly.
    async fn close(&self, c: ConnHandle) -> Result<(), DriverError>;

    /// Downcast to the Postgres extension trait. `None` if this driver is
    /// not Postgres.
    fn as_pg(&self) -> Option<&dyn PgExt> {
        None
    }

    /// Downcast to the SQL Server extension trait. `None` if this driver is
    /// not SQL Server.
    fn as_mssql(&self) -> Option<&dyn MssqlExt> {
        None
    }
}

#[cfg(feature = "mock")]
pub mod mock;

#[cfg(feature = "mock")]
pub use mock::{MockDriver, MockDriverBuilder};

// ----------------------------------------------------------------------------
// Extension traits. Declared here so `as_pg` / `as_mssql` can name them.
// Implementations live in the driver crates.
// ----------------------------------------------------------------------------

/// Postgres-specific operations. Impl lives in `sift-driver-postgres`.
#[async_trait::async_trait]
pub trait PgExt: Send + Sync {
    async fn listen(
        &self,
        c: ConnHandle,
        channels: Vec<String>,
    ) -> Result<NotificationStream, DriverError>;

    async fn unlisten(&self, c: ConnHandle, channels: Vec<String>) -> Result<(), DriverError>;

    async fn copy(&self, c: ConnHandle, op: CopyOp) -> Result<CopyResult, DriverError>;

    async fn advisory_lock(&self, c: ConnHandle, key: AdvisoryKey) -> Result<(), DriverError>;

    async fn advisory_unlock(&self, c: ConnHandle, key: AdvisoryKey) -> Result<(), DriverError>;

    /// Create a savepoint within an open transaction. PG naming: anonymous
    /// savepoint, identified by `name` for rollback.
    async fn savepoint(&self, t: &TxHandle, name: &str) -> Result<PgSavepoint, DriverError>;

    async fn rollback_to(&self, sp: PgSavepoint) -> Result<(), DriverError>;

    async fn release_savepoint(&self, sp: PgSavepoint) -> Result<(), DriverError>;
}

/// SQL Server-specific operations. Impl lives in `sift-driver-sqlserver`.
#[async_trait::async_trait]
pub trait MssqlExt: Send + Sync {
    /// `USE <db>` — switch database without reconnecting. SQL Server only;
    /// Postgres's analogue is opening a new connection.
    async fn use_database(&self, c: ConnHandle, db: &str) -> Result<(), DriverError>;

    async fn bulk_insert(&self, c: ConnHandle, op: BulkOp) -> Result<BulkResult, DriverError>;

    /// SQL Server savepoint: `SAVE TRANSACTION <name>` / `ROLLBACK
    /// TRANSACTION <name>`.
    async fn savepoint(&self, t: &TxHandle, name: &str) -> Result<MssqlSavepoint, DriverError>;

    async fn rollback_to(&self, sp: MssqlSavepoint) -> Result<(), DriverError>;
}

// ----------------------------------------------------------------------------
// Supporting types referenced by ext traits. Minimal shapes for now; grow
// as features land.
// ----------------------------------------------------------------------------

/// Stream of `LISTEN` notifications. The server consumes the receiver and
/// fans out to subscribers; closing the receiver releases the listen conn.
pub struct NotificationStream {
    pub notifications: tokio::sync::mpsc::Receiver<PgNotification>,
}

#[derive(Debug, Clone)]
pub struct PgNotification {
    pub channel: String,
    pub payload: String,
}

/// `COPY ... TO STDOUT` / `COPY ... FROM STDIN` request. Shape TBD with
/// FEATURES.md Tier 1 #12 (result export).
#[derive(Debug, Clone)]
pub enum CopyOp {
    /// Export: server streams data out via COPY TO STDOUT.
    Export { sql: String },
    /// Import: client streams data in via COPY FROM STDIN.
    Import { table: String, data: Vec<u8> },
}

#[derive(Debug, Clone)]
pub struct CopyResult {
    pub bytes: u64,
    pub rows: Option<u64>,
}

/// Advisory lock key. PG supports both 32+32-bit and 64-bit forms.
#[derive(Debug, Clone, Copy)]
pub enum AdvisoryKey {
    Int32(i32, i32),
    Int64(i64),
}

#[derive(Debug, Clone)]
pub struct PgSavepoint {
    pub tx: TxId,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct MssqlSavepoint {
    pub tx: TxId,
    pub conn: ConnHandle,
    pub name: String,
}

/// BULK INSERT request shape for the currently supported CSV import path.
/// Native TDS bulk-load needs typed rows and column metadata, not raw bytes,
/// so it will require a separate request type if it graduates later.
#[derive(Debug, Clone)]
pub struct BulkOp {
    pub table: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct BulkResult {
    pub rows_inserted: u64,
}

/// Counter for minting ConnIds / CursorIds / TxIds. Each driver owns one.
/// Simple atomic; ids need only be unique within a driver instance for the
/// lifetime of a connection.
pub struct IdCounter(std::sync::atomic::AtomicU64);

impl IdCounter {
    pub const fn new() -> Self {
        Self(std::sync::atomic::AtomicU64::new(1))
    }

    pub fn next(&self) -> u64 {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }
}

impl Default for IdCounter {
    fn default() -> Self {
        Self::new()
    }
}
