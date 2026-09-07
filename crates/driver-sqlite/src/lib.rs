//! Native SQLite connections owned by admitted dedicated worker threads.
mod files;
mod schema;
pub use files::FilePolicy;
mod security;
mod values;

use async_trait::async_trait;
use rusqlite::{Connection, OpenFlags};
use sift_driver_api::{ConnHandle, Driver, ResultSetStream, TxHandle};
use sift_protocol::*;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::sync::{mpsc, oneshot, watch, Semaphore};

const MAX_VALUE_BYTES: i32 = 8 * 1024 * 1024;
const ROWS_PER_PAGE: usize = 128;
type Job = Box<dyn FnOnce(&mut Worker) + Send>;

struct State {
    busy: bool,
    cursor: Option<CursorId>,
}
struct Entry {
    jobs: mpsc::Sender<Job>,
    state: Mutex<State>,
    admission: Arc<Semaphore>,
    closing: Arc<AtomicBool>,
    interrupt: rusqlite::InterruptHandle,
    exited: watch::Receiver<bool>,
}
struct Worker {
    conn: Connection,
    entry: Arc<Entry>,
    internal: Arc<AtomicBool>,
    read_only: bool,
    transaction: Option<TxId>,
    readonly_tx: Arc<AtomicBool>,
    name: String,
    effects: Arc<AtomicU8>,
}

pub struct SqliteDriver {
    entries: Mutex<HashMap<u64, Arc<Entry>>>,
    permits: Arc<Semaphore>,
    next: AtomicU64,
    files: Arc<FilePolicy>,
}
impl Default for SqliteDriver {
    fn default() -> Self {
        Self::new(8)
    }
}
impl SqliteDriver {
    pub fn new(max_connections: usize) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            permits: Arc::new(Semaphore::new(max_connections.min(128))),
            next: AtomicU64::new(1),
            files: Arc::new(FilePolicy::default()),
        }
    }
    pub fn with_files(files: FilePolicy) -> Self {
        let mut driver = Self::new(files.config.max_connections);
        driver.files = Arc::new(files);
        driver
    }
    async fn savepoint_control(
        &self,
        tx: &TxHandle,
        name: &str,
        verb: &'static str,
    ) -> Result<(), DriverError> {
        if name.is_empty() || name.len() > 128 || name.contains('\0') {
            return Err(error(
                Code::InvalidParameterValue,
                "invalid SQLite savepoint name",
            ));
        }
        let name = name.to_owned();
        let id = tx.tx_id;
        self.run(tx.conn.clone(), move |w| {
            w.check_tx(id)?;
            w.control(&format!("{verb} {}", schema::quote(&name)))
        })
        .await
    }
    fn id(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }
    fn entry(&self, c: &ConnHandle) -> Result<Arc<Entry>, DriverError> {
        if c.engine() != Engine::Sqlite {
            return Err(error(
                Code::ConnectionInvalidated,
                "wrong connection engine",
            ));
        }
        let entry = self
            .entries
            .lock()
            .unwrap()
            .get(&c.id())
            .cloned()
            .ok_or_else(|| {
                error(
                    Code::ConnectionInvalidated,
                    "SQLite connection must be reopened",
                )
            })?;
        if entry.closing.load(Ordering::Acquire) {
            return Err(error(
                Code::ConnectionInvalidated,
                "SQLite connection must be reopened",
            ));
        }
        Ok(entry)
    }
    // Catalog, semantic and query work share the same connection. Queue them
    // asynchronously, retaining admission until the worker (or stream) finishes.
    // A dropped caller must not let the next job overlap its still-running work.
    async fn wait_for_worker(
        entry: &Entry,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, DriverError> {
        tokio::time::timeout(
            Duration::from_secs(5),
            entry.admission.clone().acquire_owned(),
        )
        .await
        .map_err(|_| {
            error(
                Code::Other {
                    message: "SQLite operation timed out".into(),
                },
                "timed out waiting for the SQLite connection",
            )
        })?
        .map_err(|_| error(Code::ConnectionInvalidated, "SQLite worker unavailable"))
    }
    fn admit(entry: &Entry, cursor: Option<CursorId>) -> Result<(), DriverError> {
        let mut state = entry.state.lock().unwrap();
        if entry.closing.load(Ordering::Acquire) {
            return Err(error(
                Code::ConnectionInvalidated,
                "SQLite connection must be reopened",
            ));
        }
        if state.busy {
            return Err(error(
                Code::Other {
                    message: "SQLite operation failed".into(),
                },
                "SQLite connection is busy",
            ));
        }
        state.busy = true;
        state.cursor = cursor;
        Ok(())
    }
    async fn run<T: Send + 'static>(
        &self,
        c: ConnHandle,
        work: impl FnOnce(&mut Worker) -> Result<T, DriverError> + Send + 'static,
    ) -> Result<T, DriverError> {
        let entry = self.entry(&c)?;
        let permit = Self::wait_for_worker(&entry).await?;
        Self::admit(&entry, None)?;
        let (tx, rx) = oneshot::channel();
        let job: Job = Box::new(move |worker| {
            let _permit = permit;
            let result = work(worker);
            worker.finish();
            let _ = tx.send(result);
        });
        if entry.jobs.try_send(job).is_err() {
            entry.closing.store(true, Ordering::Release);
            return Err(error(
                Code::ConnectionInvalidated,
                "SQLite worker unavailable",
            ));
        }
        rx.await
            .map_err(|_| error(Code::ConnectionInvalidated, "SQLite worker exited"))?
    }
    pub async fn object_ddl(
        &self,
        c: ConnHandle,
        object: ObjectPath,
    ) -> Result<String, DriverError> {
        self.run(c, move |worker| schema::ddl(&worker.conn, &object))
            .await
    }
}
impl Worker {
    fn finish(&self) {
        let mut state = self.entry.state.lock().unwrap();
        state.busy = false;
        state.cursor = None;
    }
    fn control(&mut self, sql: &str) -> Result<(), DriverError> {
        self.internal.store(true, Ordering::Release);
        let result = self.conn.execute_batch(sql).map_err(db_error);
        self.internal.store(false, Ordering::Release);
        result
    }
    fn check_tx(&self, id: TxId) -> Result<(), DriverError> {
        if self.transaction == Some(id) {
            Ok(())
        } else {
            Err(error(
                Code::TransactionNotFound,
                "SQLite transaction is not active",
            ))
        }
    }
    fn check(&self) -> Result<(), DriverError> {
        if self.entry.closing.load(Ordering::Acquire) {
            Err(error(
                Code::QueryCanceled,
                "SQLite execution canceled; reopen the connection",
            ))
        } else {
            Ok(())
        }
    }
    fn page(&self, tx: &mpsc::Sender<Page>, mut page: Page) -> Result<(), DriverError> {
        loop {
            self.check()?;
            match tx.try_send(page) {
                Ok(()) => return Ok(()),
                Err(mpsc::error::TrySendError::Full(p)) => {
                    page = p;
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    self.entry.closing.store(true, Ordering::Release);
                    return self.check();
                }
            }
        }
    }
    fn execute(
        &mut self,
        request: ExecuteRequest,
        tx: &mpsc::Sender<Page>,
    ) -> Result<(), DriverError> {
        self.check()?;
        let params = request
            .params
            .iter()
            .map(values::bind)
            .collect::<Result<Vec<_>, _>>()?;
        if !params.is_empty() {
            let parsed = sqlparser::parser::Parser::parse_sql(
                &sqlparser::dialect::SQLiteDialect {},
                &request.sql,
            )
            .map_err(|_| {
                error(
                    Code::InvalidParameterValue,
                    "parameterized SQLite SQL must be one supported statement",
                )
            })?;
            if parsed.len() != 1 {
                return Err(error(
                    Code::InvalidParameterValue,
                    "parameters require exactly one statement",
                ));
            }
        }
        validate_slots(&request.sql, params.len())?;
        let mut batch = rusqlite::Batch::new(&self.conn, &request.sql);
        let mut affected = None::<u64>;
        loop {
            self.effects.store(0, Ordering::Release);
            let Some(mut statement) = batch.next().map_err(db_error)? else {
                break;
            };
            self.check()?;
            if statement.parameter_count() != params.len() {
                return Err(error(
                    Code::InvalidParameterValue,
                    "SQLite parameter cardinality mismatch",
                ));
            }
            for (index, value) in params.iter().enumerate() {
                if let Some(name) = statement.parameter_name(index + 1) {
                    if name != format!("?{}", index + 1) {
                        return Err(error(
                            Code::InvalidParameterValue,
                            "use contiguous SQLite ?1..?N parameters",
                        ));
                    }
                }
                statement
                    .raw_bind_parameter(index + 1, value)
                    .map_err(db_error)?;
            }
            let columns: Vec<_> = statement
                .columns()
                .iter()
                .map(|col| {
                    ColumnMetadata::new(
                        col.name(),
                        values::type_ref(col.decl_type().unwrap_or("dynamic")),
                    )
                })
                .collect();
            let produces_rows = !columns.is_empty();
            if produces_rows {
                self.page(tx, Page::NextResult { columns })?;
            }

            let mut rows = statement.raw_query();
            let mut page = Vec::with_capacity(ROWS_PER_PAGE);
            let mut page_bytes = 0;
            while let Some(row) = rows.next().map_err(db_error)? {
                self.check()?;
                let mut cells = Vec::with_capacity(row.as_ref().column_count());
                let mut row_bytes = 0usize;
                for i in 0..row.as_ref().column_count() {
                    let raw = row.get_ref(i).map_err(db_error)?;
                    row_bytes = row_bytes.saturating_add(match raw {
                        rusqlite::types::ValueRef::Text(v) | rusqlite::types::ValueRef::Blob(v) => {
                            v.len()
                        }
                        _ => 16,
                    });
                    if row_bytes > MAX_VALUE_BYTES as usize {
                        return Err(error(
                            Code::ResultTooLarge,
                            "SQLite result row exceeds the 8 MiB limit",
                        ));
                    }
                    let value = values::decode(raw)?;
                    cells.push(value);
                }
                page_bytes += row_bytes;
                page.push(Row::new(cells));
                if page.len() >= ROWS_PER_PAGE || page_bytes >= 1024 * 1024 {
                    self.page(
                        tx,
                        Page::Rows {
                            rows: std::mem::take(&mut page),
                        },
                    )?;
                    page_bytes = 0;
                }
            }
            if !page.is_empty() {
                self.page(tx, Page::Rows { rows: page })?;
            }
            drop(rows);
            if statement.is_explain() == 0 && self.effects.load(Ordering::Acquire) == 1 {
                affected = Some(affected.unwrap_or(0).saturating_add(self.conn.changes()));
            }
        }
        self.page(
            tx,
            Page::Done {
                affected_rows: affected,
                warnings: vec![],
            },
        )
    }
}

#[async_trait]
impl Driver for SqliteDriver {
    fn engine(&self) -> Engine {
        Engine::Sqlite
    }
    fn as_sqlite(&self) -> Option<&dyn sift_driver_api::SqliteExt> {
        Some(self)
    }
    async fn open(&self, spec: &ConnectionSpec) -> Result<ConnHandle, DriverError> {
        let logical_name = spec.database.clone();
        let Some(EngineConnectionSpec::Sqlite(spec)) = &spec.engine_specific else {
            return Err(error(
                Code::InvalidParameterValue,
                "approved SQLite file configuration required",
            ));
        };
        let name = logical_name.unwrap_or_else(|| "SQLite database".into());
        let mut spec = spec.clone();
        let files = self.files.clone();
        let permit = self
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| error(Code::PoolExhausted, "SQLite connection limit reached"))?;
        let id = self.id();
        let (ready, opened) = oneshot::channel();
        let (jobs, mut rx) = mpsc::channel::<Job>(1);
        let (exited_tx, exited) = watch::channel(false);
        let closing = Arc::new(AtomicBool::new(false));
        let mut opening = Opening {
            closing: closing.clone(),
            jobs: jobs.clone(),
            armed: true,
        };
        std::thread::Builder::new()
            .name(format!("sift-sqlite-{id}"))
            .spawn(move || {
                let _permit = permit;
                let init = || -> Result<Worker, DriverError> {
                    files.validate(&mut spec)?;
                    let mode = if spec.read_only {
                        OpenFlags::SQLITE_OPEN_READ_ONLY
                    } else {
                        OpenFlags::SQLITE_OPEN_READ_WRITE
                    };
                    let conn = Connection::open_with_flags(
                        &spec.file_path,
                        mode | OpenFlags::SQLITE_OPEN_NO_MUTEX
                            | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE,
                    )
                    .map_err(db_error)?;
                    conn.busy_timeout(Duration::from_millis(u64::from(
                        spec.busy_timeout_ms.min(5000),
                    )))
                    .map_err(db_error)?;
                    conn.set_limit(
                        rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH,
                        MAX_VALUE_BYTES,
                    );
                    conn.set_limit(
                        rusqlite::limits::Limit::SQLITE_LIMIT_SQL_LENGTH,
                        2 * 1024 * 1024,
                    );
                    conn.set_db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)
                        .map_err(db_error)?;
                    conn.set_db_config(
                        rusqlite::config::DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA,
                        false,
                    )
                    .map_err(db_error)?;
                    conn.execute_batch("PRAGMA foreign_keys=ON;")
                        .map_err(db_error)?;
                    conn.query_row("SELECT count(*) FROM sqlite_schema", [], |_| Ok(()))
                        .map_err(db_error)?;
                    let internal = Arc::new(AtomicBool::new(false));
                    let readonly_tx = Arc::new(AtomicBool::new(spec.read_only));
                    let effects = Arc::new(AtomicU8::new(0));
                    security::install(
                        &conn,
                        internal.clone(),
                        readonly_tx.clone(),
                        closing.clone(),
                        spec.busy_timeout_ms,
                        effects.clone(),
                    )
                    .map_err(db_error)?;
                    let entry = Arc::new(Entry {
                        jobs,
                        admission: Arc::new(Semaphore::new(1)),
                        state: Mutex::new(State {
                            busy: false,
                            cursor: None,
                        }),
                        closing,
                        interrupt: conn.get_interrupt_handle(),
                        exited,
                    });
                    Ok(Worker {
                        conn,
                        entry,
                        internal,
                        readonly_tx,
                        read_only: spec.read_only,
                        transaction: None,
                        effects,
                        name,
                    })
                };
                match init() {
                    Err(error) => {
                        let _ = ready.send(Err(error));
                    }
                    Ok(mut worker) => {
                        if ready.send(Ok(worker.entry.clone())).is_ok() {
                            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                while let Some(job) = rx.blocking_recv() {
                                    if worker.entry.closing.load(Ordering::Acquire) {
                                        break;
                                    }
                                    job(&mut worker);
                                    if worker.entry.closing.load(Ordering::Acquire) {
                                        break;
                                    }
                                }
                            }));
                        }
                        worker.entry.closing.store(true, Ordering::Release);
                        if !worker.conn.is_autocommit() {
                            let _ = worker.control("ROLLBACK");
                        }
                    }
                }
                let _ = exited_tx.send(true);
            })
            .map_err(|_| error(Code::ConnectionFailed, "could not start SQLite worker"))?;
        let entry = opened
            .await
            .map_err(|_| error(Code::ConnectionFailed, "SQLite worker exited during open"))??;
        self.entries.lock().unwrap().insert(id, entry);
        opening.armed = false;
        Ok(ConnHandle::new(id, Engine::Sqlite))
    }
    async fn ping(&self, c: ConnHandle) -> Result<ServerInfo, DriverError> {
        self.run(c, |w| {
            w.conn
                .query_row("SELECT 1", [], |_| Ok(()))
                .map_err(db_error)?;
            Ok(ServerInfo {
                provider: Engine::Sqlite.provider_ref(env!("CARGO_PKG_VERSION")),
                server_version: rusqlite::version().into(),
                current_database: w.name.clone(),
                current_user: String::new(),
                pool_warm_slots: None,
            })
        })
        .await
    }
    async fn schema(
        &self,
        c: ConnHandle,
        scope: SchemaScope,
    ) -> Result<SchemaSnapshot, DriverError> {
        let identity = format!("sqlite:{}", c.id());
        self.run(c, move |w| schema::load(&w.conn, scope, &w.name, &identity))
            .await
    }
    async fn execute(
        &self,
        c: ConnHandle,
        req: ExecuteRequest,
    ) -> Result<ResultSetStream, DriverError> {
        let entry = self.entry(&c)?;
        let cursor = CursorId::new(self.id());
        let permit = Self::wait_for_worker(&entry).await?;
        Self::admit(&entry, Some(cursor))?;
        let (tx, rx) = mpsc::channel(1);
        let job: Job = Box::new(move |w| {
            let _permit = permit;
            let closing = w.entry.closing.clone();
            let output = tx.clone();
            w.conn.progress_handler(
                1000,
                Some(move || {
                    if output.is_closed() {
                        closing.store(true, Ordering::Release);
                    }
                    closing.load(Ordering::Acquire)
                }),
            );
            let result = w.execute(req, &tx);
            let closing = w.entry.closing.clone();
            w.conn
                .progress_handler(1000, Some(move || closing.load(Ordering::Acquire)));
            if let Err(error) = result {
                let _ = w.page(&tx, Page::Error { error });
            }
            if w.transaction.is_some() && w.conn.is_autocommit() {
                w.entry.closing.store(true, Ordering::Release);
            }
            w.finish();
        });
        if entry.jobs.try_send(job).is_err() {
            entry.closing.store(true, Ordering::Release);
            return Err(error(
                Code::ConnectionInvalidated,
                "SQLite worker unavailable",
            ));
        }
        Ok(ResultSetStream::with_cursor_mode(cursor, rx, false))
    }
    async fn cancel(&self, c: ConnHandle, cursor: CursorId) -> Result<(), DriverError> {
        let entry = self.entry(&c)?;
        {
            let state = entry.state.lock().unwrap();
            if state.cursor != Some(cursor) {
                return Err(error(
                    Code::CursorNotFound,
                    "SQLite cursor is not active on this connection",
                ));
            }
            entry.closing.store(true, Ordering::Release);
            entry.interrupt.interrupt();
        }
        self.entries.lock().unwrap().remove(&c.id());
        Ok(())
    }
    async fn close(&self, c: ConnHandle) -> Result<(), DriverError> {
        let entry = self.entries.lock().unwrap().remove(&c.id());
        if let Some(entry) = entry {
            {
                let _state = entry.state.lock().unwrap();
                entry.closing.store(true, Ordering::Release);
                entry.interrupt.interrupt();
            }
            let _ = entry.jobs.try_send(Box::new(|_| {}));
            let mut exited = entry.exited.clone();
            if !*exited.borrow() {
                let _ = tokio::time::timeout(Duration::from_secs(5), exited.changed()).await;
            }
        }
        Ok(())
    }
    async fn begin(&self, c: ConnHandle, mode: TxMode) -> Result<TxHandle, DriverError> {
        if mode.isolation != IsolationLevel::Serializable {
            return Err(error(
                Code::UnsupportedForEngine,
                "SQLite supports Serializable isolation",
            ));
        }
        let id = TxId::new(self.id());
        let conn = c.clone();
        self.run(c, move |w| {
            if w.transaction.is_some() {
                return Err(error(
                    Code::InvalidParameterValue,
                    "SQLite transaction already active",
                ));
            }
            let mode = if w.read_only {
                TxMode {
                    access: TxAccessMode::ReadOnly,
                    ..mode
                }
            } else {
                mode
            };
            w.readonly_tx.store(
                w.read_only || mode.access == TxAccessMode::ReadOnly,
                Ordering::Release,
            );
            let result = w.control(if mode.access == TxAccessMode::ReadOnly {
                "BEGIN DEFERRED"
            } else {
                "BEGIN IMMEDIATE"
            });
            if result.is_err() {
                w.readonly_tx.store(w.read_only, Ordering::Release);
            }
            result?;
            w.transaction = Some(id);
            Ok(TxHandle::new(id, conn, mode))
        })
        .await
    }
    async fn commit(&self, t: TxHandle) -> Result<(), DriverError> {
        self.run(t.conn, move |w| {
            w.check_tx(t.tx_id)?;
            w.control("COMMIT")?;
            w.transaction = None;
            w.readonly_tx.store(w.read_only, Ordering::Release);
            Ok(())
        })
        .await
    }
    async fn rollback(&self, t: TxHandle) -> Result<(), DriverError> {
        self.run(t.conn, move |w| {
            w.check_tx(t.tx_id)?;
            w.control("ROLLBACK")?;
            w.transaction = None;
            w.readonly_tx.store(w.read_only, Ordering::Release);
            Ok(())
        })
        .await
    }
}
impl Drop for SqliteDriver {
    fn drop(&mut self) {
        for entry in self.entries.get_mut().unwrap().values() {
            entry.closing.store(true, Ordering::Release);
            entry.interrupt.interrupt();
            let _ = entry.jobs.try_send(Box::new(|_| {}));
        }
    }
}
fn error(code: Code, message: impl Into<String>) -> DriverError {
    DriverError::new(code, message).with_engine(Engine::Sqlite)
}
fn db_error(e: rusqlite::Error) -> DriverError {
    if let rusqlite::Error::SqliteFailure(_, Some(message)) = &e {
        if message.ends_with("already exists") {
            return error(
                Code::DuplicateObject,
                "SQLite object already exists; open its definition instead of recreating it",
            );
        }
    }
    let code = match e.sqlite_error_code() {
        Some(rusqlite::ErrorCode::OperationInterrupted) => Code::QueryCanceled,
        Some(
            rusqlite::ErrorCode::ReadOnly
            | rusqlite::ErrorCode::AuthorizationForStatementDenied
            | rusqlite::ErrorCode::PermissionDenied,
        ) => Code::UnsupportedForEngine,
        Some(rusqlite::ErrorCode::TooBig) => Code::ResultTooLarge,
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => {
            Code::Other {
                message: "SQLite database is busy; retry after the other transaction finishes"
                    .into(),
            }
        }
        Some(rusqlite::ErrorCode::ConstraintViolation) => Code::Other {
            message: "SQLite constraint violation".into(),
        },
        Some(rusqlite::ErrorCode::TypeMismatch | rusqlite::ErrorCode::ParameterOutOfRange) => {
            Code::InvalidParameterValue
        }
        Some(
            rusqlite::ErrorCode::CannotOpen
            | rusqlite::ErrorCode::NotADatabase
            | rusqlite::ErrorCode::DatabaseCorrupt,
        ) => Code::ConnectionFailed,
        _ => Code::Other {
            message: "SQLite operation failed".into(),
        },
    };
    // SQLite messages may include file paths or SQL literals. Keep those out of public errors.
    let message = code.to_string();
    let mut result = error(code, &message);
    if let Some(native) = e.sqlite_error() {
        result = result.with_native_code(native.extended_code.to_string());
    }
    result
}

#[async_trait]
impl sift_driver_api::SqliteExt for SqliteDriver {
    async fn savepoint(&self, tx: &TxHandle, name: &str) -> Result<(), DriverError> {
        self.savepoint_control(tx, name, "SAVEPOINT").await
    }
    async fn rollback_to(&self, tx: &TxHandle, name: &str) -> Result<(), DriverError> {
        self.savepoint_control(tx, name, "ROLLBACK TO").await
    }
    async fn release_savepoint(&self, tx: &TxHandle, name: &str) -> Result<(), DriverError> {
        self.savepoint_control(tx, name, "RELEASE").await
    }

    async fn open_file(
        &self,
        configuration: SqliteFileConfiguration,
        tenant_id: Option<i64>,
    ) -> Result<ConnHandle, DriverError> {
        let spec = self.files.resolve(configuration, tenant_id)?;
        self.open(&spec).await
    }
    async fn object_ddl(&self, c: ConnHandle, object: ObjectPath) -> Result<String, DriverError> {
        SqliteDriver::object_ddl(self, c, object).await
    }
}

struct Opening {
    closing: Arc<AtomicBool>,
    jobs: mpsc::Sender<Job>,
    armed: bool,
}
impl Drop for Opening {
    fn drop(&mut self) {
        if self.armed {
            self.closing.store(true, Ordering::Release);
            let _ = self.jobs.try_send(Box::new(|_| {}));
        }
    }
}
fn validate_slots(sql: &str, count: usize) -> Result<(), DriverError> {
    use sqlparser::tokenizer::{Token, Tokenizer};
    let tokens = Tokenizer::new(&sqlparser::dialect::SQLiteDialect {}, sql)
        .tokenize()
        .map_err(|_| error(Code::SyntaxError, "invalid SQLite SQL"))?;
    let mut numbered = std::collections::BTreeSet::new();
    let mut anonymous = 0;
    for token in tokens {
        if let Token::Placeholder(name) = token {
            if name == "?" {
                anonymous += 1;
            } else if let Some(number) =
                name.strip_prefix('?').and_then(|s| s.parse::<usize>().ok())
            {
                numbered.insert(number);
            } else {
                return Err(error(
                    Code::InvalidParameterValue,
                    "use SQLite ? or ?1..?N parameters",
                ));
            }
        }
    }
    if (anonymous > 0 && !numbered.is_empty())
        || (!numbered.is_empty() && numbered.iter().copied().ne(1..=count))
        || (anonymous > 0 && anonymous != count)
    {
        return Err(error(
            Code::InvalidParameterValue,
            "SQLite parameters must be contiguous; do not mix anonymous and numbered slots",
        ));
    }
    Ok(())
}
