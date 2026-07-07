//! `MockDriver` — programmable driver for server-substrate unit tests.
//! Behind the `mock` feature flag so it never ships in production builds.
//!
//! Each method has a FIFO queue of canned results. When a queue is empty,
//! the method returns its default (`Ok(default)` for value-returning
//! methods, `Ok(())` for unit-returning ones). Invocation history is kept
//! for assertions.

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::{
    BulkResult, ConnHandle, Driver, IdCounter, MssqlExt, MssqlSavepoint, NotificationStream, PgExt,
    PgNotification, PgSavepoint, ResultSetStream, TxHandle,
};
use sift_protocol::{
    Code, ConnectionSpec, CursorId, DriverError, Engine, ExecuteRequest, SchemaScope,
    SchemaSnapshot, ServerInfo, TxMode,
};
use tokio::sync::mpsc;

type Boxed<T> = Box<dyn FnOnce() -> T + Send + 'static>;

/// Canned-result queue. Each call to the matching method pops the front;
/// when empty, returns a `DriverInternal` error so test authors set up
/// every expectation explicitly.
#[derive(Default)]
struct Queues {
    open: VecDeque<Boxed<Result<ServerInfo, DriverError>>>,
    ping: VecDeque<Boxed<Result<ServerInfo, DriverError>>>,
    schema: VecDeque<Boxed<Result<SchemaSnapshot, DriverError>>>,
    begin: VecDeque<Boxed<Result<TxHandle, DriverError>>>,
    commit: VecDeque<Boxed<Result<(), DriverError>>>,
    rollback: VecDeque<Boxed<Result<(), DriverError>>>,
    execute: VecDeque<Boxed<Result<Vec<sift_protocol::Page>, DriverError>>>,
    listen: VecDeque<Boxed<Result<Vec<PgNotification>, DriverError>>>,
    bulk_insert: VecDeque<Boxed<Result<BulkResult, DriverError>>>,
    cancel: VecDeque<Boxed<Result<(), DriverError>>>,
    close: VecDeque<Boxed<Result<(), DriverError>>>,
    invocations: Vec<&'static str>,
}

/// Programmable mock driver. Build via [`MockDriver::builder`].
pub struct MockDriver {
    engine: Engine,
    state: Mutex<Queues>,
    conn_id: IdCounter,
    tx_id: IdCounter,
    cursor_id: IdCounter,
    /// `execute` never resolves — models a driver wedged before it can even
    /// return a cursor. Used to exercise the server's request timeout.
    execute_pending: bool,
    /// `execute` returns a cursor immediately, but its page stream never
    /// yields — models a query that hangs mid-drain. The returned cursor lets
    /// the server exercise cancel-on-timeout.
    execute_hang: bool,
    /// `schema` never resolves — models a wedged introspection call.
    schema_pending: bool,
}

impl MockDriver {
    pub fn builder() -> MockDriverBuilder {
        MockDriverBuilder {
            engine: Engine::Postgres,
            state: Queues::default(),
            execute_pending: false,
            execute_hang: false,
            schema_pending: false,
        }
    }

    /// Names of methods invoked since construction, in invocation order.
    pub fn invocations(&self) -> Vec<&'static str> {
        self.state.lock().unwrap().invocations.clone()
    }

    fn record(&self, name: &'static str) {
        self.state.lock().unwrap().invocations.push(name);
    }

    fn pop<T: Send + 'static>(
        queue: &mut VecDeque<Boxed<Result<T, DriverError>>>,
        method_name: &'static str,
    ) -> Result<T, DriverError> {
        match queue.pop_front() {
            Some(f) => f(),
            None => Err(DriverError::new(
                Code::DriverInternal,
                format!("MockDriver: no canned result for `{method_name}`"),
            )),
        }
    }

    /// Like `pop` but returns `Ok(())` when the queue is empty. Use for
    /// unit-returning methods (`commit`, `rollback`, `cancel`, `close`)
    /// where requiring an explicit canned Ok for every call would burden
    /// every test setup unnecessarily.
    fn pop_or_ok(queue: &mut VecDeque<Boxed<Result<(), DriverError>>>) -> Result<(), DriverError> {
        match queue.pop_front() {
            Some(f) => f(),
            None => Ok(()),
        }
    }

    fn pop_optional<T: Send + 'static>(
        queue: &mut VecDeque<Boxed<Result<T, DriverError>>>,
    ) -> Option<Result<T, DriverError>> {
        queue.pop_front().map(|f| f())
    }
}

/// Builder for [`MockDriver`].
pub struct MockDriverBuilder {
    engine: Engine,
    state: Queues,
    execute_pending: bool,
    execute_hang: bool,
    schema_pending: bool,
}

impl MockDriverBuilder {
    pub fn engine(mut self, e: Engine) -> Self {
        self.engine = e;
        self
    }

    /// Make `execute` hang forever without returning a cursor.
    pub fn execute_pending(mut self) -> Self {
        self.execute_pending = true;
        self
    }

    /// Make `execute` return a cursor immediately but hang forever draining
    /// pages. Exercises the server's cancel-on-timeout path.
    pub fn execute_hang(mut self) -> Self {
        self.execute_hang = true;
        self
    }

    /// Make `schema` hang forever.
    pub fn schema_pending(mut self) -> Self {
        self.schema_pending = true;
        self
    }

    pub fn open_ok(mut self, info: ServerInfo) -> Self {
        self.state.open.push_back(Box::new(move || Ok(info)));
        self
    }

    pub fn open_err(mut self, err: DriverError) -> Self {
        self.state.open.push_back(Box::new(move || Err(err)));
        self
    }

    pub fn ping_ok(mut self, info: ServerInfo) -> Self {
        self.state.ping.push_back(Box::new(move || Ok(info)));
        self
    }

    pub fn ping_err(mut self, err: DriverError) -> Self {
        self.state.ping.push_back(Box::new(move || Err(err)));
        self
    }

    pub fn schema_ok(mut self, snap: SchemaSnapshot) -> Self {
        self.state.schema.push_back(Box::new(move || Ok(snap)));
        self
    }

    pub fn schema_err(mut self, err: DriverError) -> Self {
        self.state.schema.push_back(Box::new(move || Err(err)));
        self
    }

    pub fn execute_ok(mut self, pages: Vec<sift_protocol::Page>) -> Self {
        self.state.execute.push_back(Box::new(move || Ok(pages)));
        self
    }

    pub fn execute_err(mut self, err: DriverError) -> Self {
        self.state.execute.push_back(Box::new(move || Err(err)));
        self
    }

    pub fn listen_ok(mut self, notifications: Vec<PgNotification>) -> Self {
        self.state
            .listen
            .push_back(Box::new(move || Ok(notifications)));
        self
    }

    pub fn cancel_ok(mut self) -> Self {
        self.state.cancel.push_back(Box::new(|| Ok(())));
        self
    }

    pub fn bulk_insert_ok(mut self, result: BulkResult) -> Self {
        self.state
            .bulk_insert
            .push_back(Box::new(move || Ok(result)));
        self
    }

    pub fn begin_err(mut self, err: DriverError) -> Self {
        self.state.begin.push_back(Box::new(move || Err(err)));
        self
    }

    pub fn build(self) -> MockDriver {
        MockDriver {
            engine: self.engine,
            state: Mutex::new(self.state),
            conn_id: IdCounter::new(),
            tx_id: IdCounter::new(),
            cursor_id: IdCounter::new(),
            execute_pending: self.execute_pending,
            execute_hang: self.execute_hang,
            schema_pending: self.schema_pending,
        }
    }
}

#[async_trait]
impl Driver for MockDriver {
    fn engine(&self) -> Engine {
        self.engine
    }

    async fn open(&self, _spec: &ConnectionSpec) -> Result<ConnHandle, DriverError> {
        self.record("open");
        if let Some(result) =
            MockDriver::pop_optional::<ServerInfo>(&mut self.state.lock().unwrap().open)
        {
            result?;
        }
        let id = self.conn_id.next();
        Ok(ConnHandle::new(id, self.engine))
    }

    async fn ping(&self, _c: ConnHandle) -> Result<ServerInfo, DriverError> {
        self.record("ping");
        MockDriver::pop(&mut self.state.lock().unwrap().ping, "ping")
    }

    async fn schema(
        &self,
        _c: ConnHandle,
        _scope: SchemaScope,
    ) -> Result<SchemaSnapshot, DriverError> {
        self.record("schema");
        if self.schema_pending {
            std::future::pending::<()>().await;
        }
        MockDriver::pop(&mut self.state.lock().unwrap().schema, "schema")
    }

    async fn begin(&self, c: ConnHandle, mode: TxMode) -> Result<TxHandle, DriverError> {
        self.record("begin");
        let mut guard = self.state.lock().unwrap();
        if let Some(result) = MockDriver::pop_optional::<TxHandle>(&mut guard.begin) {
            result?;
        }
        let tx_id = sift_protocol::TxId::new(self.tx_id.next());
        Ok(TxHandle::new(tx_id, c, mode))
    }

    async fn commit(&self, _t: TxHandle) -> Result<(), DriverError> {
        self.record("commit");
        MockDriver::pop_or_ok(&mut self.state.lock().unwrap().commit)
    }

    async fn rollback(&self, _t: TxHandle) -> Result<(), DriverError> {
        self.record("rollback");
        MockDriver::pop_or_ok(&mut self.state.lock().unwrap().rollback)
    }

    async fn execute(
        &self,
        c: ConnHandle,
        _req: ExecuteRequest,
    ) -> Result<ResultSetStream, DriverError> {
        self.record("execute");
        if self.execute_pending {
            std::future::pending::<()>().await;
        }
        let cursor_id = CursorId::new(self.cursor_id.next());
        if self.execute_hang {
            // Return a live cursor whose page stream never yields: hold the
            // sender open on a parked task so the receiver blocks forever.
            let (tx, rx) = mpsc::channel(1);
            tokio::spawn(async move {
                let _held = tx;
                std::future::pending::<()>().await;
            });
            let _ = c;
            return Ok(ResultSetStream::new(cursor_id, rx));
        }
        let result = MockDriver::pop::<Vec<sift_protocol::Page>>(
            &mut self.state.lock().unwrap().execute,
            "execute",
        );
        match result {
            Ok(pages) => {
                let (tx, rx) = mpsc::channel(pages.len().max(1));
                tokio::spawn(async move {
                    for page in pages {
                        if tx.send(page).await.is_err() {
                            break;
                        }
                    }
                });
                Ok(ResultSetStream::new(cursor_id, rx))
            }
            Err(e) => {
                let (tx, rx) = mpsc::channel(1);
                tokio::spawn(async move {
                    let _ = tx.send(sift_protocol::Page::Error { error: e }).await;
                });
                let _ = c;
                Ok(ResultSetStream::new(cursor_id, rx))
            }
        }
    }

    async fn cancel(&self, _c: ConnHandle, _cursor: CursorId) -> Result<(), DriverError> {
        self.record("cancel");
        MockDriver::pop_or_ok(&mut self.state.lock().unwrap().cancel)
    }

    async fn close(&self, _c: ConnHandle) -> Result<(), DriverError> {
        self.record("close");
        MockDriver::pop_or_ok(&mut self.state.lock().unwrap().close)
    }

    fn as_pg(&self) -> Option<&dyn PgExt> {
        (self.engine == Engine::Postgres).then_some(self)
    }

    fn as_mssql(&self) -> Option<&dyn MssqlExt> {
        (self.engine == Engine::SqlServer).then_some(self)
    }
}

#[async_trait]
impl PgExt for MockDriver {
    async fn listen(
        &self,
        _c: ConnHandle,
        _channels: Vec<String>,
    ) -> Result<NotificationStream, DriverError> {
        self.record("listen");
        let notifications = MockDriver::pop(&mut self.state.lock().unwrap().listen, "listen")?;
        let (tx, rx) = mpsc::channel(notifications.len().max(1));
        tokio::spawn(async move {
            for notification in notifications {
                if tx.send(notification).await.is_err() {
                    break;
                }
            }
        });
        Ok(NotificationStream { notifications: rx })
    }

    async fn unlisten(&self, _c: ConnHandle, _channels: Vec<String>) -> Result<(), DriverError> {
        Ok(())
    }

    async fn copy(
        &self,
        _c: ConnHandle,
        _op: crate::CopyOp,
    ) -> Result<crate::CopyResult, DriverError> {
        Err(DriverError::new(
            Code::UnsupportedForEngine,
            "COPY not wired in MockDriver",
        ))
    }

    async fn advisory_lock(
        &self,
        _c: ConnHandle,
        _key: crate::AdvisoryKey,
    ) -> Result<(), DriverError> {
        Ok(())
    }

    async fn advisory_unlock(
        &self,
        _c: ConnHandle,
        _key: crate::AdvisoryKey,
    ) -> Result<(), DriverError> {
        Ok(())
    }

    async fn savepoint(&self, _t: &TxHandle, name: &str) -> Result<PgSavepoint, DriverError> {
        Ok(PgSavepoint {
            tx: sift_protocol::TxId::new(0),
            name: name.to_string(),
        })
    }

    async fn rollback_to(&self, _sp: PgSavepoint) -> Result<(), DriverError> {
        Ok(())
    }

    async fn release_savepoint(&self, _sp: PgSavepoint) -> Result<(), DriverError> {
        Ok(())
    }
}

#[async_trait]
impl MssqlExt for MockDriver {
    async fn use_database(&self, _c: ConnHandle, _db: &str) -> Result<(), DriverError> {
        self.record("use_database");
        Ok(())
    }

    async fn bulk_insert(
        &self,
        _c: ConnHandle,
        _op: crate::BulkOp,
    ) -> Result<BulkResult, DriverError> {
        self.record("bulk_insert");
        MockDriver::pop(&mut self.state.lock().unwrap().bulk_insert, "bulk_insert")
    }

    async fn savepoint(&self, t: &TxHandle, name: &str) -> Result<MssqlSavepoint, DriverError> {
        self.record("mssql_savepoint");
        Ok(MssqlSavepoint {
            tx: t.tx_id,
            conn: t.conn.clone(),
            name: name.to_string(),
        })
    }

    async fn rollback_to(&self, _sp: MssqlSavepoint) -> Result<(), DriverError> {
        self.record("mssql_rollback_to");
        Ok(())
    }
}
