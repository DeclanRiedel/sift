//! Private load connections. Only the worker owns the native handle; it is never
//! published in the session connection list. Observation loss cannot abort it.
use super::*;
use std::collections::HashMap;
use tokio::sync::{oneshot, watch};

/// Captured once before supervision. Credentials stay in memory, never reports.
pub(crate) struct WorkerSource {
    pub store: SessionStore,
    pub session: SessionId,
    pub connection: ConnectionId,
    pub entry: ConnectionEntryClone,
    configuration: serde_json::Value,
    credentials: HashMap<String, Vec<u8>>,
    sql: Vec<String>,
}

impl WorkerSource {
    pub fn capture(
        store: &SessionStore,
        session: SessionId,
        connection: ConnectionId,
        requests: &[BenchmarkRequest],
    ) -> ApiResult<Arc<Self>> {
        if !(1..=sift_core::performance_suite::MAX_CASES).contains(&requests.len()) {
            return Err(ApiError::BadRequest(
                "Load requires 1–32 frozen cases".into(),
            ));
        }
        let entry = store.conn_entry(session, connection)?;
        let engine = entry
            .driver
            .semantic_engine()
            .ok_or_else(|| ApiError::BadRequest("Load requires a supported SQL dialect".into()))?;
        for request in requests {
            validate_read(engine, &request.sql)?;
        }
        let store = store.bind_source(session, connection, entry.clone())?;
        let (configuration, credentials) = store
            .with_session(&session, |session| {
                session.connections.get(&connection).map(|current| {
                    if !Arc::ptr_eq(&current.execution, &entry.execution) {
                        return Err(source_changed());
                    }
                    Ok((current.configuration.clone(), current.credentials.clone()))
                })
            })?
            .ok_or(ApiError::ConnectionNotFound(connection))??;
        let source = Arc::new(Self {
            store,
            session,
            connection,
            entry,
            configuration,
            credentials,
            sql: requests.iter().map(|request| request.sql.clone()).collect(),
        });
        source.check_all()?;
        Ok(source)
    }

    pub fn check(&self, sql: &str) -> ApiResult<()> {
        for operation in [
            sift_protocol::OperationKind::BenchmarkSuite,
            sift_protocol::OperationKind::ExecuteQuery,
        ] {
            let current = self.store.authorize_connection_operation(
                self.session,
                self.connection,
                operation,
                Some(sql),
                &[],
            )?;
            if !Arc::ptr_eq(&current.execution, &self.entry.execution)
                || current.execution.is_fenced()
            {
                return Err(source_changed());
            }
        }
        Ok(())
    }

    pub fn check_all(&self) -> ApiResult<()> {
        for sql in &self.sql {
            self.check(sql)?;
        }
        Ok(())
    }

    /// Reserve the whole connection budget before the caller starts any open.
    pub fn reserve_connections(
        &self,
        config: sift_protocol::BenchmarkLoadConfig,
    ) -> ApiResult<Vec<Option<crate::resources::ResourceGuard>>> {
        sift_core::performance::load::validate_config(config)
            .map_err(|message| ApiError::BadRequest(message.into()))?;
        (0..config.workers)
            .map(|_| match &self.entry.provenance {
                ConnectionProvenance::Managed {
                    tenant_id,
                    quota_exempt: false,
                    ..
                } => self
                    .store
                    .resource_manager()
                    .reserve(*tenant_id, sift_protocol::TenantResource::Connections, 1)
                    .map(Some),
                _ => Ok(None),
            })
            .collect()
    }

    pub fn reserve_bytes(
        &self,
        bytes: usize,
    ) -> ApiResult<Option<crate::resources::ResourceGuard>> {
        self.store
            .retained_byte_context(&self.entry)
            .map(|(manager, tenant)| {
                manager.reserve(
                    tenant,
                    sift_protocol::TenantResource::RetainedResultBytes,
                    bytes as u64,
                )
            })
            .transpose()
    }
}

fn source_changed() -> ApiError {
    ApiError::Driver(DriverError::new(
        Code::ConnectionInvalidated,
        "Load benchmark source changed",
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Cleanup {
    Pending,
    Closed,
    Failed,
}

#[derive(Clone)]
pub(super) struct Control {
    pub cancellation: CancellationToken,
    pub deadline: tokio::time::Instant,
}

impl Control {
    pub fn check(&self) -> ApiResult<()> {
        if self.cancellation.is_cancelled() {
            Err(ApiError::Driver(DriverError::new(
                Code::QueryCanceled,
                "Load benchmark cancelled",
            )))
        } else if tokio::time::Instant::now() >= self.deadline {
            Err(ApiError::Driver(DriverError::new(
                Code::QueryTimedOut,
                "Load benchmark budget exhausted",
            )))
        } else {
            Ok(())
        }
    }
}

pub(super) struct Opening {
    pub result: oneshot::Receiver<ApiResult<Arc<PrivateWorker>>>,
    pub cleanup: watch::Receiver<Cleanup>,
}

type QueryQuota = (
    crate::resources::ResourceGuard,
    crate::resources::ResourceGuard,
);

pub(super) struct QueryInput {
    pub request: ExecuteRequest,
    pub control: Control,
    pub origin: tokio::time::Instant,
    pub start_ns: u64,
    pub quota: Option<QueryQuota>,
    pub bytes: Option<crate::resources::ResourceGuard>,
    pub limits: (usize, usize),
    pub settings: Option<Engine>,
}

pub(super) struct QueryObservation {
    pub rows: u64,
    pub first_row_ns: Option<u64>,
    pub end_ns: u64,
    pub settings: Option<sift_protocol::SessionSettingsObservation>,
}

pub(super) struct Ready {
    pub worker: Arc<PrivateWorker>,
    pub environment: BenchmarkEnvironment,
}

struct Lifetime {
    _tunnel: Option<crate::tailnet::TunnelLease>,
    _quota: Option<crate::resources::ResourceGuard>,
    _permit: DriverTaskPermit,
    _query_guard: Option<Arc<crate::shutdown::QueryGuard>>,
    cleanup: watch::Sender<Cleanup>,
}

pub(super) struct PrivateWorker {
    source: Arc<WorkerSource>,
    driver: RuntimeDriver,
    handle: RuntimeConnectionHandle,
    transaction: Mutex<Option<RuntimeTransactionHandle>>,
    setup_complete: AtomicBool,
    busy: AtomicBool,
    retired: AtomicBool,
    cancel_started: AtomicBool,
    retired_quota: Mutex<Option<QueryQuota>>,
    retired_bytes: Mutex<Option<crate::resources::ResourceGuard>>,
    lifetime: Option<Lifetime>,
}

impl PrivateWorker {
    /// The returned receiver is only an observer. Never expose an abort handle
    /// for the native owner, including while driver.open is still pending.
    pub fn start_open(
        source: Arc<WorkerSource>,
        quota: Option<crate::resources::ResourceGuard>,
        control: Control,
    ) -> ApiResult<Opening> {
        control.check()?;
        source.check_all()?;
        if matches!(
            &source.entry.provenance,
            ConnectionProvenance::Managed {
                quota_exempt: false,
                ..
            }
        ) && quota.is_none()
        {
            return Err(ApiError::Internal(
                "Load worker is missing its connection reservation".into(),
            ));
        }
        let permit = source.store.reserve_driver_work()?;
        let guard = crate::shutdown::current_query_guard();
        let (send, result) = oneshot::channel();
        let (cleanup_send, cleanup) = watch::channel(Cleanup::Pending);
        tokio::spawn(crate::shutdown::scope_query(guard.clone(), async move {
            let opened =
                Self::open_native(source, quota, control, permit, guard, cleanup_send, &send).await;
            // An abandoned successful result drops its worker into actual close.
            let _ = send.send(opened);
        }));
        Ok(Opening { result, cleanup })
    }

    async fn open_native(
        source: Arc<WorkerSource>,
        quota: Option<crate::resources::ResourceGuard>,
        control: Control,
        permit: DriverTaskPermit,
        guard: Option<Arc<crate::shutdown::QueryGuard>>,
        cleanup: watch::Sender<Cleanup>,
        observer: &oneshot::Sender<ApiResult<Arc<Self>>>,
    ) -> ApiResult<Arc<Self>> {
        control.check()?;
        if observer.is_closed() {
            return Err(ApiError::BadRequest(
                "Load benchmark open observer left".into(),
            ));
        }
        source.check_all()?;
        let driver = source.entry.driver.clone();
        let opening = opening_connection::OpeningConnection::new(
            driver.clone(),
            quota,
            source.store.inner.opening_connections.clone(),
        )?;
        let (configuration, tunnel) = crate::tailnet::prepare(&source.configuration)
            .await
            .map_err(|_| {
                ApiError::Driver(DriverError::new(
                    Code::ConnectionFailed,
                    "Load benchmark tunnel setup failed",
                ))
            })?;
        control.check()?;
        if observer.is_closed() {
            return Err(ApiError::BadRequest(
                "Load benchmark open observer left".into(),
            ));
        }
        source.check_all()?;
        let tenant = match &source.entry.provenance {
            ConnectionProvenance::Managed { tenant_id, .. } => Some(tenant_id.0),
            _ => None,
        };
        let opened = match driver
            .open(&configuration, &source.credentials, tenant)
            .await
        {
            Ok(handle) => opening.completed(handle, tunnel),
            Err(error) => {
                // No handle crossed the driver's boundary. Never record native
                // diagnostics or credentials in the portable observations.
                cleanup.send_replace(Cleanup::Closed);
                return Err(ApiError::Driver(DriverError::new(
                    error.code,
                    "Load benchmark connection open failed",
                )));
            }
        };
        // No await occurs between adoption and construction of the close owner.
        let (handle, tunnel, quota) = opened.adopt();
        let worker = Arc::new(Self {
            source,
            driver,
            handle,
            transaction: Mutex::new(None),
            setup_complete: AtomicBool::new(false),
            busy: AtomicBool::new(false),
            retired: AtomicBool::new(false),
            cancel_started: AtomicBool::new(false),
            retired_quota: Mutex::new(None),
            retired_bytes: Mutex::new(None),
            lifetime: Some(Lifetime {
                _tunnel: tunnel,
                _quota: quota,
                _permit: permit,
                _query_guard: guard,
                cleanup,
            }),
        });
        control.check()?;
        if observer.is_closed() {
            return Err(ApiError::BadRequest(
                "Load benchmark open observer left".into(),
            ));
        }
        worker.source.check_all()?;
        Ok(worker)
    }

    fn acquire(&self) -> ApiResult<()> {
        if self.retired.load(Ordering::Acquire)
            || self
                .busy
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return Err(ApiError::BadRequest(
                "Load worker is busy or retired".into(),
            ));
        }
        Ok(())
    }

    pub fn start_setup(
        self: Arc<Self>,
        engine: Engine,
        control: Control,
    ) -> ApiResult<oneshot::Receiver<ApiResult<Ready>>> {
        if self.setup_complete.load(Ordering::Acquire) {
            return Err(ApiError::BadRequest(
                "Load worker setup already completed".into(),
            ));
        }
        self.acquire()?;
        let (mut send, receive) = oneshot::channel();
        tokio::spawn(async move {
            let result = self.setup_native(engine, &control, &mut send).await;
            if result.is_err() || send.is_closed() {
                self.retired.store(true, Ordering::Release);
            } else {
                self.setup_complete.store(true, Ordering::Release);
            }
            self.busy.store(false, Ordering::Release);
            let _ = send.send(result.map(|environment| Ready {
                worker: self,
                environment,
            }));
        });
        Ok(receive)
    }

    async fn setup_native(
        self: &Arc<Self>,
        engine: Engine,
        control: &Control,
        observer: &mut oneshot::Sender<ApiResult<Ready>>,
    ) -> ApiResult<BenchmarkEnvironment> {
        let check = |observer: &oneshot::Sender<_>| -> ApiResult<()> {
            control.check()?;
            if observer.is_closed() {
                return Err(ApiError::BadRequest("Load setup observer left".into()));
            }
            self.source.check_all()
        };
        check(observer)?;
        let server_version = self
            .driver
            .ping(self.handle.clone())
            .await
            .ok()
            .and_then(|version| {
                (version.server_version.len() <= 256).then_some(version.server_version)
            });
        check(observer)?;
        let origin = tokio::time::Instant::now();
        let settings_control = Control {
            cancellation: control.cancellation.clone(),
            deadline: control
                .deadline
                .min(tokio::time::Instant::now() + Duration::from_secs(5)),
        };
        let input = QueryInput {
            request: ExecuteRequest {
                sql: performance_settings::projection(engine).0.into(),
                params: Vec::new(),
                transform: None,
            },
            control: settings_control.clone(),
            origin,
            start_ns: 0,
            quota: self
                .source
                .store
                .reserve_query_resources(&self.source.entry)?,
            bytes: self.source.reserve_bytes(128 * 1024)?,
            limits: (1, 16 * 1024),
            settings: Some(engine),
        };
        let mut settings = self.clone().spawn_query(input, false);
        let observation = tokio::select! {
            biased;
            result = &mut settings => result.map_err(|_| ApiError::Internal("Load setup query owner failed".into()))??,
            _ = observer.closed() => return Err(ApiError::BadRequest("Load setup observer left".into())),
            _ = control.cancellation.cancelled() => return Err(ApiError::Driver(DriverError::new(Code::QueryCanceled, "Load setup cancelled"))),
            _ = tokio::time::sleep_until(settings_control.deadline) => return Err(ApiError::Driver(DriverError::new(Code::QueryTimedOut, "Load setup timed out"))),
        };
        check(observer)?;
        if engine != Engine::SqlServer {
            let transaction = self
                .driver
                .begin(
                    self.handle.clone(),
                    sift_protocol::TxMode {
                        access: sift_protocol::TxAccessMode::ReadOnly,
                        isolation: if engine == Engine::Sqlite {
                            sift_protocol::IsolationLevel::Serializable
                        } else {
                            sift_protocol::IsolationLevel::ReadCommitted
                        },
                    },
                )
                .await
                .map_err(|_| {
                    ApiError::Driver(DriverError::new(
                        Code::DriverInternal,
                        "Load read transaction setup failed",
                    ))
                })?;
            *self.transaction.lock().unwrap() = Some(transaction);
        }
        check(observer)?;
        Ok(BenchmarkEnvironment {
            server_version,
            connection_reuse: "one_private_reused_connection_per_load_worker".into(),
            session_settings: if observation.settings.is_some() {
                "allowlist_observed_before_workload"
            } else {
                "connection_profile_defaults_uninspected"
            }
            .into(),
            observed_settings: observation.settings,
            ..benchmark_environment(engine)
        })
    }

    pub fn start_attempt(
        self: Arc<Self>,
        input: QueryInput,
    ) -> ApiResult<oneshot::Receiver<ApiResult<QueryObservation>>> {
        if !self.setup_complete.load(Ordering::Acquire) {
            return Err(ApiError::BadRequest(
                "Load worker setup is incomplete".into(),
            ));
        }
        self.acquire()?;
        Ok(self.spawn_query(input, true))
    }

    fn spawn_query(
        self: Arc<Self>,
        mut input: QueryInput,
        release_busy: bool,
    ) -> oneshot::Receiver<ApiResult<QueryObservation>> {
        let (send, receive) = oneshot::channel();
        tokio::spawn(async move {
            let mut observer = Some(send);
            let result = self.consume(&input, &mut observer).await;
            if result.is_err() || observer.as_ref().map_or(true, |send| send.is_closed()) {
                self.retired.store(true, Ordering::Release);
            }
            if self.retired.load(Ordering::Acquire) {
                *self.retired_quota.lock().unwrap() = input.quota.take();
                *self.retired_bytes.lock().unwrap() = input.bytes.take();
            } else {
                drop(input.quota.take());
                drop(input.bytes.take());
            }
            if release_busy {
                self.busy.store(false, Ordering::Release);
            }
            if let Some(send) = observer.take() {
                if send.send(result).is_err() {
                    self.retired.store(true, Ordering::Release);
                }
            }
        });
        receive
    }

    fn cancel(self: &Arc<Self>, cursor: CursorId) {
        self.retired.store(true, Ordering::Release);
        if self.cancel_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let worker = self.clone();
        tokio::spawn(async move {
            // A single cancel control per private worker can outlast observation.
            // Its Arc keeps the same lifetime/permit until the native call ends.
            let _ = worker.driver.cancel(worker.handle.clone(), cursor).await;
        });
    }

    async fn consume(
        self: &Arc<Self>,
        input: &QueryInput,
        observer: &mut Option<oneshot::Sender<ApiResult<QueryObservation>>>,
    ) -> ApiResult<QueryObservation> {
        input.control.check()?;
        if observer.as_ref().map_or(true, |send| send.is_closed()) {
            return Err(ApiError::BadRequest("Load attempt observer left".into()));
        }
        if input.settings.is_none() {
            self.source.check(&input.request.sql)?;
        } else {
            self.source.check_all()?;
        }
        let mut execute = Box::pin(
            self.driver
                .execute(self.handle.clone(), input.request.clone()),
        );
        let mut stream = tokio::select! {
            biased;
            result = &mut execute => result.map_err(|_| load_query_error())?,
            _ = observer.as_mut().unwrap().closed() => {
                self.retired.store(true, Ordering::Release);
                execute.await.map_err(|_| load_query_error())?
            },
            _ = input.control.cancellation.cancelled() => {
                fail_observer(observer, Code::QueryCanceled);
                self.retired.store(true, Ordering::Release);
                execute.await.map_err(|_| load_query_error())?
            },
            _ = tokio::time::sleep_until(input.control.deadline) => {
                fail_observer(observer, Code::QueryTimedOut);
                self.retired.store(true, Ordering::Release);
                execute.await.map_err(|_| load_query_error())?
            },
        };
        let mut state = StreamObservation::new(input, &stream.columns);
        if state.invalid {
            fail_observer(observer, Code::DriverInternal);
            self.cancel(stream.cursor_id);
        }
        if self.retired.load(Ordering::Acquire)
            || input.control.check().is_err()
            || observer.as_ref().map_or(true, |send| send.is_closed())
        {
            self.cancel(stream.cursor_id);
        }
        loop {
            let page = if self.retired.load(Ordering::Acquire) {
                stream.rows.recv().await
            } else {
                tokio::select! {
                    biased;
                    page = stream.rows.recv() => page,
                    _ = observer.as_mut().unwrap().closed() => { self.cancel(stream.cursor_id); continue; },
                    _ = input.control.cancellation.cancelled() => {
                        fail_observer(observer, Code::QueryCanceled); self.cancel(stream.cursor_id); continue;
                    },
                    _ = tokio::time::sleep_until(input.control.deadline) => {
                        fail_observer(observer, Code::QueryTimedOut); self.cancel(stream.cursor_id); continue;
                    },
                }
            };
            let Some(page) = page else {
                break;
            };
            if !self.retired.load(Ordering::Acquire) && state.page(input, &page).is_err() {
                fail_observer(observer, Code::DriverInternal);
                self.cancel(stream.cursor_id);
            }
        }
        // Done is not EOF. All late pages/errors above remain observable.
        if self.retired.load(Ordering::Acquire) || !state.done {
            return Err(load_query_error());
        }
        input.control.check()?;
        Ok(QueryObservation {
            rows: state.rows,
            first_row_ns: state.first,
            end_ns: nanos(input.origin.elapsed()),
            settings: state.settings,
        })
    }
}

fn load_query_error() -> ApiError {
    ApiError::Driver(DriverError::new(
        Code::DriverInternal,
        "Load query did not complete cleanly",
    ))
}

fn fail_observer(observer: &mut Option<oneshot::Sender<ApiResult<QueryObservation>>>, code: Code) {
    if let Some(send) = observer.take() {
        let _ = send.send(Err(ApiError::Driver(DriverError::new(
            code,
            "Load query observation stopped",
        ))));
    }
}

struct StreamObservation {
    columns: Option<usize>,
    header: bool,
    rows: u64,
    bytes: usize,
    first: Option<u64>,
    done: bool,
    invalid: bool,
    settings: Option<sift_protocol::SessionSettingsObservation>,
}

impl StreamObservation {
    fn new(input: &QueryInput, columns: &[ColumnMetadata]) -> Self {
        let bytes = count_json(columns, input.limits.1);
        Self {
            columns: (!columns.is_empty()).then_some(columns.len()),
            header: false,
            rows: 0,
            invalid: columns.len() > 2000 || bytes.is_err(),
            bytes: bytes.unwrap_or(0),
            first: None,
            done: false,
            settings: None,
        }
    }

    fn page(&mut self, input: &QueryInput, page: &Page) -> ApiResult<()> {
        if self.done {
            return Err(load_query_error());
        }
        let remaining = input
            .limits
            .1
            .checked_sub(self.bytes)
            .ok_or_else(load_query_error)?;
        match page {
            Page::NextResult { columns } => {
                if self.header
                    || self.rows != 0
                    || columns.len() > 2000
                    || self.columns.is_some_and(|count| count != columns.len())
                {
                    return Err(load_query_error());
                }
                self.header = true;
                self.columns = Some(columns.len());
                self.bytes += count_json(columns, remaining)?;
            }
            Page::Rows { rows } => {
                if self.columns.is_none()
                    || self.rows.saturating_add(rows.len() as u64) > input.limits.0 as u64
                {
                    return Err(load_query_error());
                }
                for row in rows {
                    if row.values.len() != self.columns.unwrap()
                        || !row.values.iter().all(bounded_json)
                    {
                        return Err(load_query_error());
                    }
                }
                self.bytes += count_json(rows, remaining)?;
                if !rows.is_empty() && self.first.is_none() {
                    self.first = Some(nanos(input.origin.elapsed()).saturating_sub(input.start_ns));
                }
                if let (Some(engine), Some(row)) = (input.settings, rows.first()) {
                    self.settings = performance_settings::decode(engine, &row.values);
                }
                self.rows += rows.len() as u64;
            }
            Page::Done { .. } => self.done = true,
            Page::Error { .. } => return Err(load_query_error()),
        }
        Ok(())
    }
}

fn count_json<T: serde::Serialize + ?Sized>(value: &T, limit: usize) -> ApiResult<usize> {
    struct Counter {
        bytes: usize,
        limit: usize,
    }
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes) {
                return Err(std::io::Error::other("load observation byte limit"));
            }
            self.bytes += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter { bytes: 0, limit };
    serde_json::to_writer(&mut counter, value).map_err(|_| load_query_error())?;
    Ok(counter.bytes)
}

fn bounded_json(value: &sift_protocol::Value) -> bool {
    let sift_protocol::Value::Json(value) = value else {
        return true;
    };
    let mut pending = vec![(value, 0usize)];
    let mut visited = 0usize;
    while let Some((value, depth)) = pending.pop() {
        visited += 1;
        if depth > 64 || visited > 4096 {
            return false;
        }
        match value {
            serde_json::Value::Array(values) => {
                if visited + pending.len() + values.len() > 4096 {
                    return false;
                }
                pending.extend(values.iter().map(|value| (value, depth + 1)));
            }
            serde_json::Value::Object(values) => {
                if visited + pending.len() + values.len() > 4096 {
                    return false;
                }
                pending.extend(values.values().map(|value| (value, depth + 1)));
            }
            _ => {}
        }
    }
    true
}

impl Drop for PrivateWorker {
    fn drop(&mut self) {
        let Some(lifetime) = self.lifetime.take() else {
            return;
        };
        let transaction = self.transaction.get_mut().unwrap().take();
        let retired = (
            self.retired_quota.get_mut().unwrap().take(),
            self.retired_bytes.get_mut().unwrap().take(),
        );
        let driver = self.driver.clone();
        let handle = self.handle.clone();
        let source = self.source.clone();
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            lifetime.cleanup.send_replace(Cleanup::Failed);
            tracing::warn!("runtime unavailable for private load connection cleanup");
            return;
        };
        let guard = lifetime._query_guard.clone();
        runtime.spawn(crate::shutdown::scope_query(guard, async move {
            let owned_lifetime = lifetime;
            let _source = source;
            let _retired = retired;
            let mut failed = false;
            if let Some(transaction) = transaction {
                failed |= driver.rollback(transaction).await.is_err();
            }
            failed |= driver.close(handle).await.is_err();
            owned_lifetime.cleanup.send_replace(if failed {
                Cleanup::Failed
            } else {
                Cleanup::Closed
            });
            // Lifetime owns quota, tunnel, driver admission and the shutdown
            // guard until BOTH native rollback and close have actually returned.
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{BuiltinProviderAdapter, DatabaseProvider};

    async fn until(mut ready: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while !ready() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
    }

    fn request() -> BenchmarkRequest {
        BenchmarkRequest {
            run_id: uuid::Uuid::new_v4(),
            sql: "SELECT 1".into(),
            params: vec![],
            warmups: 0,
            iterations: 1,
            query_timeout_ms: 1000,
            total_budget_ms: 30000,
            delay_ms: 0,
            workload_confirmed: true,
        }
    }

    async fn fixture(
        mock: Arc<sift_driver_api::mock::MockDriver>,
    ) -> (SessionStore, SessionId, ConnectionId, Arc<WorkerSource>) {
        let registry = DriverRegistry::builder().build();
        registry
            .providers()
            .replace([Arc::new(BuiltinProviderAdapter::new(mock)) as Arc<dyn DatabaseProvider>])
            .unwrap();
        let store = SessionStore::new(registry);
        let session = store
            .open_session(OpenSessionRequest {
                tag: None,
                tenant_id: None,
            })
            .id;
        let original = store
            .open_connection(
                session,
                Engine::Postgres,
                ConnectionSpec {
                    host: "mock.invalid".into(),
                    port: None,
                    database: Some("mock".into()),
                    user: "mock".into(),
                    password: None,
                    ssl_mode: Some(sift_protocol::SslMode::Disable),
                    engine_specific: None,
                },
            )
            .await
            .unwrap()
            .id;
        let source = WorkerSource::capture(&store, session, original, &[request()]).unwrap();
        (store, session, original, source)
    }

    fn control() -> Control {
        Control {
            cancellation: CancellationToken::new(),
            deadline: tokio::time::Instant::now() + Duration::from_secs(30),
        }
    }

    fn pages() -> Vec<Page> {
        vec![
            Page::NextResult {
                columns: vec![ColumnMetadata::new(
                    "x",
                    sift_protocol::TypeRef::Primitive(sift_protocol::PrimitiveType::Int32),
                )],
            },
            Page::Rows {
                rows: vec![Row {
                    values: vec![sift_protocol::Value::Int32(1)],
                }],
            },
            Page::Done {
                affected_rows: None,
                warnings: vec![],
            },
        ]
    }

    #[tokio::test(start_paused = true)]
    async fn retire_worker_preserves_healthy_fixed_work_while_stop_run_closes_peer_admission() {
        use sift_protocol::{
            BenchmarkLimits, BenchmarkLoadFailurePolicy as Policy, BenchmarkParameterCase,
            BenchmarkSuiteOrder, BenchmarkSuiteQuery, BenchmarkSuiteRequest,
        };
        for (policy, successful, failed, unattempted) in
            [(Policy::StopRun, 0, 1, 3), (Policy::RetireWorker, 2, 1, 1)]
        {
            let mock = Arc::new(
                sift_driver_api::mock::MockDriver::builder()
                    .engine(Engine::Postgres)
                    .execute_ok(pages())
                    .execute_ok(pages())
                    .execute_err(DriverError::new(
                        Code::DriverInternal,
                        "scripted workload failure",
                    ))
                    .execute_ok(pages())
                    .execute_ok(pages())
                    .build(),
            );
            let (store, session, original, _source) = fixture(mock.clone()).await;
            let report = store
                .benchmark_suite(
                    session,
                    original,
                    BenchmarkSuiteRequest {
                        run_id: uuid::Uuid::new_v4(),
                        workload_confirmed: true,
                        order: BenchmarkSuiteOrder::ConcurrentV1 {
                            workers: 2,
                            max_starts_per_second: 10,
                            failure_policy: policy,
                        },
                        limits: BenchmarkLimits {
                            warmups: 0,
                            iterations: 2,
                            query_timeout_ms: 1000,
                            total_budget_ms: 5000,
                            delay_ms: 0,
                        },
                        queries: vec![BenchmarkSuiteQuery {
                            id: "query".into(),
                            sql: "SELECT 1".into(),
                            cases: vec![BenchmarkParameterCase {
                                id: "case".into(),
                                params: vec![],
                                limits: None,
                            }],
                        }],
                    },
                )
                .await
                .unwrap();
            assert!(!report.completed);
            let summary = report.load_summary.unwrap();
            assert_eq!(
                (
                    summary.measured.successful,
                    summary.measured.failed,
                    summary.measured.unattempted
                ),
                (successful, failed, unattempted)
            );
            assert_eq!(
                mock.invocations()
                    .iter()
                    .filter(|&&call| call == "execute")
                    .count() as u32,
                2 + successful + failed
            );
            assert_eq!(
                mock.invocations()
                    .iter()
                    .filter(|&&call| call == "open")
                    .count(),
                3,
                "no replacement worker may open"
            );
            assert_eq!(store.inner.driver_tasks.load(Ordering::Acquire), 0);
            assert_eq!(store.list_connections(session).unwrap()[0].id, original);
            store.close_connection(session, original).await.unwrap();
        }
    }

    #[tokio::test(start_paused = true)]
    async fn expired_case_budget_cancels_a_peer_already_running_another_case() {
        use sift_protocol::{
            BenchmarkLimits, BenchmarkLoadFailurePolicy, BenchmarkParameterCase,
            BenchmarkSuiteOrder, BenchmarkSuiteQuery, BenchmarkSuiteRequest,
        };
        let mock = Arc::new(
            sift_driver_api::mock::MockDriver::builder()
                .engine(Engine::Postgres)
                .execute_delay(Duration::ZERO)
                .execute_delay(Duration::ZERO)
                .execute_delay(Duration::from_millis(200))
                .execute_delay(Duration::ZERO)
                .execute_delay(Duration::from_millis(500))
                .execute_ok(pages())
                .execute_ok(pages())
                .execute_ok(pages())
                .execute_ok(pages())
                .execute_ok(pages())
                .build(),
        );
        let (store, session, original, _source) = fixture(mock.clone()).await;
        let defaults = BenchmarkLimits {
            warmups: 0,
            iterations: 1,
            query_timeout_ms: 1000,
            total_budget_ms: 5000,
            delay_ms: 0,
        };
        let report = store
            .benchmark_suite(
                session,
                original,
                BenchmarkSuiteRequest {
                    run_id: uuid::Uuid::new_v4(),
                    workload_confirmed: true,
                    order: BenchmarkSuiteOrder::ConcurrentV1 {
                        workers: 2,
                        max_starts_per_second: 1000,
                        failure_policy: BenchmarkLoadFailurePolicy::RetireWorker,
                    },
                    limits: defaults,
                    queries: vec![BenchmarkSuiteQuery {
                        id: "query".into(),
                        sql: "SELECT 1".into(),
                        cases: vec![
                            BenchmarkParameterCase {
                                id: "short".into(),
                                params: vec![],
                                limits: Some(BenchmarkLimits {
                                    query_timeout_ms: 100,
                                    total_budget_ms: 100,
                                    ..defaults
                                }),
                            },
                            BenchmarkParameterCase {
                                id: "peer".into(),
                                params: vec![],
                                limits: None,
                            },
                        ],
                    }],
                },
            )
            .await
            .unwrap();
        assert!(!report.completed);
        let summary = report.load_summary.unwrap();
        assert_eq!(
            (
                summary.measured.successful,
                summary.measured.timed_out,
                summary.measured.cancelled,
                summary.measured.unattempted
            ),
            (1, 1, 1, 1)
        );
        assert_eq!(report.cases[1].samples.len(), 1);
        assert_eq!(
            report.cases[1].samples[0].outcome,
            BenchmarkOutcome::Cancelled
        );
        assert_eq!(
            report.cases[0].suite_context.as_ref().unwrap().actual_order,
            vec![0, 0, 1]
        );
        assert_eq!(store.inner.driver_tasks.load(Ordering::Acquire), 0);
        assert_eq!(store.list_connections(session).unwrap()[0].id, original);
        store.close_connection(session, original).await.unwrap();
    }

    #[tokio::test]
    async fn whole_connection_budget_refusal_releases_slots_before_any_native_worker_open() {
        let mock = Arc::new(
            sift_driver_api::mock::MockDriver::builder()
                .engine(Engine::Postgres)
                .build(),
        );
        let (store, session, original, mut source) = fixture(mock.clone()).await;
        let mut limits = crate::config::TenantLimitsConfig::default();
        limits.defaults.connections = Some(2);
        limits.ceilings.connections = Some(2);
        store.set_resource_manager(crate::resources::ResourceManager::new(&limits, None));
        // Isolate connection-provenance admission from authentication: this
        // preflight must fail before any native worker or setup can start.
        Arc::get_mut(&mut source).unwrap().entry.provenance = ConnectionProvenance::Managed {
            principal_id: sift_metadata::PrincipalId(1),
            tenant_id: sift_metadata::TenantId(1),
            profile_id: sift_metadata::ConnectionProfileId(1),
            policy_revision: 0,
            quota_exempt: false,
        };
        let mut cleanup = vec![];
        let mut environments = vec![None; 3];
        let result = super::super::prepare(
            source,
            sift_protocol::BenchmarkLoadConfig {
                workers: 3,
                max_starts_per_second: 10,
                failure_policy: sift_protocol::BenchmarkLoadFailurePolicy::StopRun,
            },
            Engine::Postgres,
            &control(),
            &mut cleanup,
            &mut environments,
        )
        .await;
        assert!(matches!(
            result,
            Err(ApiError::TenantResourceExhausted {
                resource: sift_protocol::TenantResource::Connections,
                ..
            })
        ));
        assert!(cleanup.is_empty());
        assert_eq!(
            store
                .resource_manager()
                .snapshot(sift_metadata::TenantId(1))
                .unwrap()
                .usage
                .connections,
            0
        );
        assert_eq!(mock.invocations(), vec!["open"]);
        store.close_connection(session, original).await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn generation_and_authority_changes_during_native_open_refuse_the_worker_and_still_close()
    {
        for generation in [false, true] {
            let opened = Arc::new(tokio::sync::Notify::new());
            let closed = Arc::new(tokio::sync::Notify::new());
            let mock = Arc::new(
                sift_driver_api::mock::MockDriver::builder()
                    .engine(Engine::Postgres)
                    .open_wait(opened.clone())
                    .open_wait(opened.clone())
                    .close_wait(closed.clone())
                    .build(),
            );
            opened.notify_one();
            let (store, session, original, mut source) = fixture(mock.clone()).await;
            let allowed = Arc::new(AtomicBool::new(true));
            let authority = allowed.clone();
            let captured = Arc::get_mut(&mut source).unwrap();
            captured.store = captured.store.with_source_authority(Arc::new(move || {
                if authority.load(Ordering::Acquire) {
                    Ok(())
                } else {
                    Err(ApiError::Forbidden("Source authority revoked".into()))
                }
            }));
            let shutdown = crate::shutdown::Shutdown::default();
            let opening =
                crate::shutdown::scope_query(Some(Arc::new(shutdown.track_query())), async {
                    PrivateWorker::start_open(source, None, control())
                })
                .await
                .unwrap();
            until(|| {
                mock.invocations()
                    .iter()
                    .filter(|&&call| call == "open")
                    .count()
                    == 2
            })
            .await;
            if generation {
                store
                    .with_session(&session, |session| {
                        session.connections.get_mut(&original).unwrap().execution =
                            Arc::new(ConnectionExecutionState::default());
                    })
                    .unwrap();
            } else {
                allowed.store(false, Ordering::Release);
            }
            opened.notify_one();
            let result = opening.result.await.unwrap();
            assert!(if generation {
                matches!(result, Err(ApiError::Driver(error)) if error.code == Code::ConnectionInvalidated)
            } else {
                matches!(result, Err(ApiError::Forbidden(_)))
            });
            until(|| mock.invocations().contains(&"close")).await;
            assert_eq!(shutdown.in_flight(), 1);
            assert_eq!(store.inner.driver_tasks.load(Ordering::Acquire), 1);
            assert!(!mock.invocations().contains(&"execute"));
            closed.notify_one();
            until(|| {
                shutdown.in_flight() == 0 && store.inner.driver_tasks.load(Ordering::Acquire) == 0
            })
            .await;
            assert_eq!(*opening.cleanup.borrow(), Cleanup::Closed);
            store.close_connection(session, original).await.unwrap();
        }
    }

    #[tokio::test(start_paused = true)]
    async fn queued_pacing_wait_releases_bookkeeping_and_rechecks_current_authority() {
        let mock = Arc::new(
            sift_driver_api::mock::MockDriver::builder()
                .engine(Engine::Postgres)
                .build(),
        );
        let (store, session, original, mut source) = fixture(mock.clone()).await;
        let allowed = Arc::new(AtomicBool::new(true));
        let authority = allowed.clone();
        let captured = Arc::get_mut(&mut source).unwrap();
        captured.store = captured.store.with_source_authority(Arc::new(move || {
            if authority.load(Ordering::Acquire) {
                Ok(())
            } else {
                Err(ApiError::Forbidden("revoked".into()))
            }
        }));
        let shared = Arc::new(super::super::Shared {
            source,
            requests: Arc::new(vec![request()]),
            control: control(),
            config: sift_protocol::BenchmarkLoadConfig {
                workers: 1,
                max_starts_per_second: 1,
                failure_policy: sift_protocol::BenchmarkLoadFailurePolicy::StopRun,
            },
            origin: tokio::time::Instant::now(),
            stopped: Mutex::new(None),
            ledger: tokio::sync::Mutex::new(super::super::Ledger {
                next_start: tokio::time::Instant::now() + Duration::from_secs(1),
                case_deadlines: vec![None],
                counts: vec![0],
                records: vec![],
            }),
        });
        let waiting = shared.clone();
        let task = tokio::spawn(async move { waiting.admit(0, 0, false).await });
        tokio::time::sleep(Duration::from_millis(1)).await;
        assert!(!task.is_finished());
        assert!(
            shared.ledger.try_lock().is_ok(),
            "pacing must not block failure settlement"
        );
        allowed.store(false, Ordering::Release);
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(task.await.unwrap().is_none());
        assert!(shared.ledger.lock().await.records.is_empty());
        assert_eq!(mock.invocations(), vec!["open"]);
        store.close_connection(session, original).await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn late_error_after_done_fails_and_retains_cursor_bytes_and_quota_through_raw_eof_and_close(
    ) {
        let (setup_send, setup_rows) = tokio::sync::mpsc::channel(4);
        for page in pages() {
            setup_send.send(page).await.unwrap();
        }
        drop(setup_send);
        let (query_send, query_rows) = tokio::sync::mpsc::channel(8);
        let closed = Arc::new(tokio::sync::Notify::new());
        let mock = Arc::new(
            sift_driver_api::mock::MockDriver::builder()
                .engine(Engine::Postgres)
                .execute_rows(setup_rows)
                .execute_rows(query_rows)
                .close_wait(closed.clone())
                .build(),
        );
        let (store, session, original, source) = fixture(mock.clone()).await;
        let tenant = sift_metadata::TenantId(1);
        let manager = store.resource_manager();
        let quota = manager
            .reserve(tenant, sift_protocol::TenantResource::Connections, 1)
            .unwrap();
        let shutdown = crate::shutdown::Shutdown::default();
        let opening = crate::shutdown::scope_query(Some(Arc::new(shutdown.track_query())), async {
            PrivateWorker::start_open(source, Some(quota), control())
        })
        .await
        .unwrap();
        let worker = opening.result.await.unwrap().unwrap();
        let worker = worker
            .start_setup(Engine::Postgres, control())
            .unwrap()
            .await
            .unwrap()
            .unwrap()
            .worker;
        let mut result = worker
            .clone()
            .start_attempt(QueryInput {
                request: ExecuteRequest::new("SELECT 1"),
                control: control(),
                origin: tokio::time::Instant::now(),
                start_ns: 0,
                quota: Some((
                    manager
                        .reserve(tenant, sift_protocol::TenantResource::ConcurrentQueries, 1)
                        .unwrap(),
                    manager
                        .reserve(tenant, sift_protocol::TenantResource::Cursors, 1)
                        .unwrap(),
                )),
                bytes: Some(
                    manager
                        .reserve(
                            tenant,
                            sift_protocol::TenantResource::RetainedResultBytes,
                            4096,
                        )
                        .unwrap(),
                ),
                limits: (100, 4096),
                settings: None,
            })
            .unwrap();
        until(|| {
            mock.invocations()
                .iter()
                .filter(|&&call| call == "execute")
                .count()
                == 2
        })
        .await;
        for page in pages() {
            query_send.send(page).await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
        assert!(matches!(
            result.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        query_send
            .send(Page::Error {
                error: DriverError::new(Code::DriverInternal, "PRIVATE-LOAD-ERROR"),
            })
            .await
            .unwrap();
        let Err(error) = result.await.unwrap() else {
            panic!("Done must not hide a late native error");
        };
        assert!(!format!("{error:?}").contains("PRIVATE-LOAD-ERROR"));
        drop(worker);
        let usage = manager.snapshot(tenant).unwrap().usage;
        assert_eq!(
            (
                usage.connections,
                usage.concurrent_queries,
                usage.cursors,
                usage.retained_result_bytes
            ),
            (1, 1, 1, 4096)
        );
        assert!(
            !mock.invocations().contains(&"close"),
            "raw EOF has not arrived"
        );
        drop(query_send);
        until(|| mock.invocations().contains(&"close")).await;
        let usage = manager.snapshot(tenant).unwrap().usage;
        assert_eq!(
            (
                usage.connections,
                usage.concurrent_queries,
                usage.cursors,
                usage.retained_result_bytes
            ),
            (1, 1, 1, 4096)
        );
        assert_eq!(shutdown.in_flight(), 1);
        closed.notify_one();
        until(|| {
            shutdown.in_flight() == 0 && manager.snapshot(tenant).unwrap().usage.connections == 0
        })
        .await;
        let usage = manager.snapshot(tenant).unwrap().usage;
        assert_eq!(
            (
                usage.concurrent_queries,
                usage.cursors,
                usage.retained_result_bytes
            ),
            (0, 0, 0)
        );
        assert_eq!(*opening.cleanup.borrow(), Cleanup::Closed);
        assert_eq!(store.list_connections(session).unwrap()[0].id, original);
        store.close_connection(session, original).await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn stopped_observation_before_cursor_retains_ownership_through_cancel_eof_and_close() {
        for stop in ["observer_loss", "cancel", "timeout"] {
            let (setup_send, setup_rows) = tokio::sync::mpsc::channel(4);
            for page in pages() {
                setup_send.send(page).await.unwrap();
            }
            drop(setup_send);
            let (query_send, query_rows) = tokio::sync::mpsc::channel(4);
            let execute = Arc::new(tokio::sync::Notify::new());
            let cancel = Arc::new(tokio::sync::Notify::new());
            let close = Arc::new(tokio::sync::Notify::new());
            let mock = Arc::new(
                sift_driver_api::mock::MockDriver::builder()
                    .engine(Engine::Postgres)
                    .execute_rows(setup_rows)
                    .execute_rows(query_rows)
                    .execute_wait(execute.clone())
                    .execute_wait(execute.clone())
                    .cancel_wait(cancel.clone())
                    .close_wait(close.clone())
                    .build(),
            );
            execute.notify_one();
            let (store, session, original, source) = fixture(mock.clone()).await;
            let tenant = sift_metadata::TenantId(1);
            let manager = store.resource_manager();
            let quota = manager
                .reserve(tenant, sift_protocol::TenantResource::Connections, 1)
                .unwrap();
            let shutdown = crate::shutdown::Shutdown::default();
            let opening =
                crate::shutdown::scope_query(Some(Arc::new(shutdown.track_query())), async {
                    PrivateWorker::start_open(source, Some(quota), control())
                })
                .await
                .unwrap();
            let worker = opening.result.await.unwrap().unwrap();
            let worker = worker
                .start_setup(Engine::Postgres, control())
                .unwrap()
                .await
                .unwrap()
                .unwrap()
                .worker;
            let attempt_control = control();
            let result = worker
                .clone()
                .start_attempt(QueryInput {
                    request: ExecuteRequest::new("SELECT 1"),
                    control: attempt_control.clone(),
                    origin: tokio::time::Instant::now(),
                    start_ns: 0,
                    quota: Some((
                        manager
                            .reserve(tenant, sift_protocol::TenantResource::ConcurrentQueries, 1)
                            .unwrap(),
                        manager
                            .reserve(tenant, sift_protocol::TenantResource::Cursors, 1)
                            .unwrap(),
                    )),
                    bytes: Some(
                        manager
                            .reserve(
                                tenant,
                                sift_protocol::TenantResource::RetainedResultBytes,
                                4096,
                            )
                            .unwrap(),
                    ),
                    limits: (100, 4096),
                    settings: None,
                })
                .unwrap();
            until(|| {
                mock.invocations()
                    .iter()
                    .filter(|&&call| call == "execute")
                    .count()
                    == 2
            })
            .await;
            match stop {
                "observer_loss" => drop(result),
                "cancel" => {
                    attempt_control.cancellation.cancel();
                    assert!(
                        matches!(result.await.unwrap(), Err(ApiError::Driver(error)) if error.code == Code::QueryCanceled)
                    );
                }
                _ => {
                    tokio::time::advance(Duration::from_secs(30)).await;
                    assert!(
                        matches!(result.await.unwrap(), Err(ApiError::Driver(error)) if error.code == Code::QueryTimedOut)
                    );
                }
            }
            drop(worker);
            tokio::task::yield_now().await;
            assert!(!mock.invocations().contains(&"close"));
            assert!(
                !mock.invocations().contains(&"cancel"),
                "no cursor exists yet"
            );
            assert_eq!(shutdown.in_flight(), 1);
            assert_eq!(manager.snapshot(tenant).unwrap().usage.cursors, 1);
            execute.notify_one();
            until(|| mock.invocations().contains(&"cancel")).await;
            drop(query_send);
            tokio::time::sleep(Duration::from_millis(1)).await;
            assert!(
                !mock.invocations().contains(&"close"),
                "native cancel still owns the handle after EOF"
            );
            let usage = manager.snapshot(tenant).unwrap().usage;
            assert_eq!(
                (
                    usage.connections,
                    usage.concurrent_queries,
                    usage.cursors,
                    usage.retained_result_bytes
                ),
                (1, 1, 1, 4096)
            );
            cancel.notify_one();
            until(|| mock.invocations().contains(&"close")).await;
            assert_eq!(shutdown.in_flight(), 1);
            assert_eq!(manager.snapshot(tenant).unwrap().usage.cursors, 1);
            close.notify_one();
            until(|| {
                shutdown.in_flight() == 0
                    && manager.snapshot(tenant).unwrap().usage.connections == 0
            })
            .await;
            let usage = manager.snapshot(tenant).unwrap().usage;
            assert_eq!(
                (
                    usage.concurrent_queries,
                    usage.cursors,
                    usage.retained_result_bytes
                ),
                (0, 0, 0)
            );
            assert_eq!(*opening.cleanup.borrow(), Cleanup::Closed);
            assert_eq!(
                mock.invocations()
                    .iter()
                    .filter(|&&call| call == "cancel")
                    .count(),
                1
            );
            assert_eq!(store.list_connections(session).unwrap()[0].id, original);
            store.close_connection(session, original).await.unwrap();
        }
    }

    #[tokio::test(start_paused = true)]
    async fn abandoned_private_open_never_publishes_and_keeps_native_cleanup_owned() {
        let opened = Arc::new(tokio::sync::Notify::new());
        let closed = Arc::new(tokio::sync::Notify::new());
        let mock = Arc::new(
            sift_driver_api::mock::MockDriver::builder()
                .engine(Engine::Postgres)
                .open_wait(opened.clone())
                .close_wait(closed.clone())
                .build(),
        );
        opened.notify_one();
        let (store, session, original, source) = fixture(mock.clone()).await;
        let shutdown = crate::shutdown::Shutdown::default();
        let opening = crate::shutdown::scope_query(Some(Arc::new(shutdown.track_query())), async {
            PrivateWorker::start_open(
                source,
                None,
                Control {
                    cancellation: CancellationToken::new(),
                    deadline: tokio::time::Instant::now() + Duration::from_secs(30),
                },
            )
        })
        .await
        .unwrap();
        until(|| {
            mock.invocations()
                .iter()
                .filter(|&&call| call == "open")
                .count()
                == 2
        })
        .await;
        drop(opening.result);
        assert_eq!(shutdown.in_flight(), 1);
        assert_eq!(store.list_connections(session).unwrap().len(), 1);
        opened.notify_one();
        until(|| mock.invocations().contains(&"close")).await;
        tokio::time::advance(Duration::from_secs(6)).await;
        assert_eq!(shutdown.in_flight(), 1);
        assert_eq!(store.inner.driver_tasks.load(Ordering::Acquire), 1);
        assert_eq!(*opening.cleanup.borrow(), Cleanup::Pending);
        assert_eq!(store.list_connections(session).unwrap()[0].id, original);
        closed.notify_one();
        until(|| {
            shutdown.in_flight() == 0 && store.inner.driver_tasks.load(Ordering::Acquire) == 0
        })
        .await;
        assert_eq!(*opening.cleanup.borrow(), Cleanup::Closed);
        assert_eq!(
            mock.invocations()
                .iter()
                .filter(|&&call| call == "close")
                .count(),
            1
        );
        closed.notify_one();
        store.close_connection(session, original).await.unwrap();
    }
}
