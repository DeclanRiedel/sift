//! Connection admission and native execution ownership, independent of observers.
use super::*;
use tokio::sync::{oneshot, OwnedRwLockReadGuard, OwnedRwLockWriteGuard};

pub(crate) struct ConnectionExecutionState {
    pub(crate) review_id: uuid::Uuid,
    native_closed: AtomicBool,
    native_close_done: tokio::sync::Notify,
    pub(super) listeners: notifications::ListenerOwners,
    pub(super) gate: Arc<tokio::sync::RwLock<()>>,
    fenced: AtomicBool,
    transaction_state_unknown: AtomicBool,
}

impl Default for ConnectionExecutionState {
    fn default() -> Self {
        Self {
            review_id: uuid::Uuid::new_v4(),
            native_closed: AtomicBool::new(false),
            native_close_done: tokio::sync::Notify::new(),
            listeners: Default::default(),
            gate: Default::default(),
            fenced: Default::default(),
            transaction_state_unknown: Default::default(),
        }
    }
}

impl ConnectionExecutionState {
    pub(super) fn did_close(&self) {
        self.native_closed.store(true, Ordering::Release);
        self.native_close_done.notify_waiters();
    }
    pub(super) async fn wait_closed(&self) {
        loop {
            let notified = self.native_close_done.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.native_closed.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }

    pub(super) fn is_fenced(&self) -> bool {
        self.fenced.load(Ordering::Acquire)
    }
    pub(super) fn fence(&self) {
        self.fenced.store(true, Ordering::Release);
    }
    pub(crate) fn autocommit_known(&self) -> bool {
        !self.transaction_state_unknown.load(Ordering::Acquire)
    }
}

pub(super) struct MutationLease {
    state: Arc<ConnectionExecutionState>,
    settled: bool,
}
impl MutationLease {
    pub(super) fn new(state: Arc<ConnectionExecutionState>) -> Self {
        Self {
            state,
            settled: false,
        }
    }
    pub(super) fn settle(&mut self) {
        self.settled = true;
    }
}
impl Drop for MutationLease {
    fn drop(&mut self) {
        if !self.settled {
            self.state.fence();
        }
    }
}

type QueryResources = Option<
    Arc<(
        crate::resources::ResourceGuard,
        crate::resources::ResourceGuard,
    )>,
>;

pub(super) struct ExecutionRequest {
    pub session: SessionId,
    pub connection: ConnectionId,
    pub entry: ConnectionEntryClone,
    pub request: ExecuteRequest,
    pub tx: Option<TxHandleRef>,
    pub operation: sift_protocol::OperationKind,
    pub resources: QueryResources,
}

enum ExecutionLock {
    Read { _guard: OwnedRwLockReadGuard<()> },
    Write { _guard: OwnedRwLockWriteGuard<()> },
}

struct ExecutionLease {
    store: SessionStore,
    session: SessionId,
    tx: Option<TxId>,
    state: Arc<ConnectionExecutionState>,
    _lock: ExecutionLock,
    _entry: ConnectionEntryClone,
    _resources: QueryResources,
    _permit: DriverTaskPermit,
    failed: bool,
    settled: bool,
}

impl ExecutionLease {
    fn fail(&mut self) {
        if !self.failed {
            if let Some(tx) = self.tx {
                self.store.mark_transaction_failed(self.session, tx);
            }
            self.failed = true;
        }
    }
}

impl Drop for ExecutionLease {
    fn drop(&mut self) {
        if !self.settled {
            self.state.fence();
            self.fail();
        }
    }
}

impl SessionStore {
    pub(super) async fn transaction_work<F, T>(
        &self,
        operation: &'static str,
        work: F,
    ) -> ApiResult<T>
    where
        F: std::future::Future<Output = ApiResult<T>> + Send + 'static,
        T: Send + 'static,
    {
        // Transaction mutations must settle even if an AI read observer or an
        // HTTP caller leaves. Only the observer has a deadline; never abort work.
        let observation = native_transaction::Worker::capture();
        let permit = self.reserve_driver_work()?;
        // Keep transaction ownership scopes independent of each caller's full
        // state machine, including when adding native observation contexts.
        let work: futures::future::BoxFuture<'static, ApiResult<T>> = Box::pin(work);
        let query_guard = crate::shutdown::current_query_guard();
        let mut task = tokio::spawn(crate::shutdown::scope_query(query_guard, async move {
            let _permit = permit;
            observation.run(work).await
        }));
        let timeout = self.request_timeout();
        let outcome = if timeout.is_zero() {
            task.await
        } else {
            tokio::time::timeout(timeout, &mut task)
                .await
                .map_err(|_| timeout_error(operation))?
        };
        outcome.map_err(|_| ApiError::Internal("transaction work owner failed".into()))?
    }

    pub(super) async fn reconnect_owned(
        &self,
        session: SessionId,
        connection: ConnectionId,
        entry: ConnectionEntryClone,
        deadline: tokio::time::Instant,
    ) -> ApiResult<RuntimeConnectionHandle> {
        let _guard = tokio::time::timeout_at(deadline, entry.execution.gate.clone().write_owned())
            .await
            .map_err(|_| Self::admission_expired())?;
        Self::check_admission_deadline(deadline)?;
        self.authorize_connection_operation(
            session,
            connection,
            sift_protocol::OperationKind::OpenConnection,
            None,
            &[],
        )?;
        self.reject_if_connection_has_tx(session, connection, None)?;
        let (configuration, credentials, tenant) = {
            let session = self
                .inner
                .sessions
                .get(&session)
                .ok_or(ApiError::SessionNotFound(session))?;
            let current = session
                .connections
                .get(&connection)
                .ok_or(ApiError::ConnectionNotFound(connection))?;
            if !Arc::ptr_eq(&current.execution, &entry.execution) {
                return Err(ApiError::Driver(DriverError::new(
                    Code::ConnectionInvalidated,
                    "reconnect source was replaced",
                )));
            }
            (
                current.configuration.clone(),
                current.credentials.clone(),
                match &current.provenance {
                    ConnectionProvenance::Managed { tenant_id, .. } => Some(tenant_id.0),
                    ConnectionProvenance::TrustedLocal => None,
                },
            )
        };
        entry.execution.listeners.stop().await;
        let mut mutation = MutationLease::new(entry.execution.clone());
        let (effective, tunnel) = crate::tailnet::prepare(&configuration)
            .await
            .map_err(|error| DriverError::new(Code::ConnectionFailed, error.to_string()))?;
        let tunnel = tunnel.map(Arc::new);
        let handle = entry.driver.open(&effective, &credentials, tenant).await?;
        let adopted = (|| {
            self.authorize_connection_operation(
                session,
                connection,
                sift_protocol::OperationKind::OpenConnection,
                None,
                &[],
            )?;
            let session = self
                .inner
                .sessions
                .get(&session)
                .ok_or(ApiError::SessionNotFound(session))?;
            let mut current = session
                .connections
                .get_mut(&connection)
                .ok_or(ApiError::ConnectionNotFound(connection))?;
            if !Arc::ptr_eq(&current.execution, &entry.execution) {
                return Err(ApiError::Driver(DriverError::new(
                    Code::ConnectionInvalidated,
                    "reconnect source was replaced",
                )));
            }
            current.handle = handle.clone();
            current._tunnel = tunnel.clone();
            current.execution = Arc::new(ConnectionExecutionState::default());
            Ok(())
        })();
        if let Err(error) = adopted {
            let _ = entry.driver.close(handle).await;
            return Err(error);
        }
        mutation.settle();
        let _ = entry.driver.close(entry.handle.clone()).await;
        Ok(handle)
    }

    pub(super) async fn begin_transaction_owned(
        &self,
        session_id: SessionId,
        req: BeginTransactionRequest,
        operation: sift_protocol::OperationKind,
        entry: ConnectionEntryClone,
        deadline: tokio::time::Instant,
    ) -> ApiResult<TransactionInfo> {
        let _guard = tokio::time::timeout_at(deadline, entry.execution.gate.clone().write_owned())
            .await
            .map_err(|_| Self::admission_expired())?;
        Self::check_admission_deadline(deadline)?;
        let current =
            self.authorize_connection_operation(session_id, req.connection, operation, None, &[])?;
        if !Arc::ptr_eq(&entry.execution, &current.execution) {
            return Err(ApiError::Driver(DriverError::new(
                Code::ConnectionInvalidated,
                "begin source connection was replaced",
            )));
        }
        self.reject_if_connection_has_tx(session_id, req.connection, None)?;
        native_transaction::admit(
            native_transaction::Action::Begin,
            None,
            &entry.execution,
            || Ok(()),
        )?;
        let mut mutation = MutationLease::new(entry.execution.clone());
        let begun = current.driver.begin(current.handle.clone(), req.mode).await;
        native_transaction::acknowledge(
            begun.as_ref().ok().map(RuntimeTransactionHandle::tx_id),
            begun.as_ref().map(|_| ()),
        );
        let handle = match begun {
            Ok(handle) => handle,
            Err(error) => {
                if matches!(
                    error.code,
                    Code::UnsupportedForEngine | Code::InvalidParameterValue | Code::PoolExhausted
                ) {
                    mutation.settle();
                }
                return Err(ApiError::Driver(error));
            }
        };
        let info = TransactionInfo {
            tx_id: handle.tx_id(),
            connection: req.connection,
            mode: handle.mode(),
            opened_at: chrono::Utc::now(),
        };
        let publish = (|| {
            self.authorize_connection_operation(session_id, req.connection, operation, None, &[])?;
            let session = self
                .inner
                .sessions
                .get(&session_id)
                .ok_or(ApiError::SessionNotFound(session_id))?;
            // Keep the connection entry borrowed through publication. Close
            // removes it before draining transactions, so it cannot miss this row.
            let live = session
                .connections
                .get(&req.connection)
                .ok_or(ApiError::ConnectionNotFound(req.connection))?;
            if !Arc::ptr_eq(&live.execution, &entry.execution) {
                return Err(ApiError::Driver(DriverError::new(
                    Code::ConnectionInvalidated,
                    "begin source connection was replaced",
                )));
            }
            session.transactions.insert(
                info.tx_id,
                TransactionEntry {
                    info: info.clone(),
                    handle: handle.clone(),
                    savepoints: Mutex::new(Vec::new()),
                    control: entry.execution.gate.clone(),
                    ending: AtomicBool::new(false),
                    failure: Mutex::new(TransactionFailure::default()),
                },
            );
            Ok(())
        })();
        if let Err(error) = publish {
            native_transaction::before_cleanup(info.tx_id).await;
            let rolled_back = current.driver.rollback(handle).await.is_ok();
            native_transaction::cleanup_rollback(rolled_back);
            if rolled_back {
                mutation.settle();
            }
            return Err(error);
        }
        native_transaction::published(&info);
        mutation.settle();
        Ok(info)
    }

    pub(super) fn reserve_driver_work(&self) -> ApiResult<DriverTaskPermit> {
        self.inner
            .driver_tasks
            .fetch_update(Ordering::Acquire, Ordering::Relaxed, |current| {
                (current < MAX_DRIVER_TASKS).then_some(current + 1)
            })
            .map_err(|_| {
                ApiError::Driver(DriverError::new(
                    Code::PoolExhausted,
                    "driver task limit reached",
                ))
            })?;
        Ok(DriverTaskPermit(self.inner.clone()))
    }

    pub(super) fn admission_deadline(&self) -> tokio::time::Instant {
        let timeout = self.request_timeout();
        tokio::time::Instant::now()
            + if timeout.is_zero() {
                Duration::from_secs(5)
            } else {
                timeout.min(Duration::from_secs(5))
            }
    }

    pub(super) fn check_admission_deadline(deadline: tokio::time::Instant) -> ApiResult<()> {
        if tokio::time::Instant::now() >= deadline {
            return Err(Self::admission_expired());
        }
        Ok(())
    }

    pub(super) fn admission_expired() -> ApiError {
        ApiError::Driver(DriverError::new(
            Code::PoolExhausted,
            "connection admission deadline expired; no request dispatched",
        ))
    }

    pub(super) async fn execute_owned(&self, work: ExecutionRequest) -> ApiResult<ResultSetStream> {
        let permit = self.reserve_driver_work()?;
        let admission = self.admission_deadline();
        let timeout = self.request_timeout();
        let deadline = (!timeout.is_zero()).then(|| tokio::time::Instant::now() + timeout);
        let (reply, receive) = oneshot::channel();
        let store = self.clone();
        let observation = native_execution::current();
        let mutation = native_mutation::Context::capture();
        let query_guard = crate::shutdown::current_query_guard();
        tokio::spawn(async move {
            let _query_guard = query_guard;
            mutation
                .run(Box::pin(async move {
                    let work = store.execution_worker(work, permit, admission, deadline, reply);
                    match observation {
                        Some(observation) => native_execution::scope(observation, work).await,
                        None => work.await,
                    }
                }))
                .await
        });
        let result = match deadline {
            Some(deadline) => tokio::time::timeout_at(deadline, receive)
                .await
                .map_err(|_| timeout_error("execute"))?,
            None => receive.await,
        };
        result.map_err(|_| ApiError::Internal("execution owner failed".into()))?
    }

    async fn execution_worker(
        &self,
        work: ExecutionRequest,
        permit: DriverTaskPermit,
        admission: tokio::time::Instant,
        deadline: Option<tokio::time::Instant>,
        mut reply: oneshot::Sender<ApiResult<ResultSetStream>>,
    ) {
        let observation = native_execution::current();
        let ExecutionRequest {
            session,
            connection,
            entry,
            request,
            tx,
            operation,
            resources,
        } = work;
        let state = entry.execution.clone();
        let acquire = async {
            if tx.is_some() {
                ExecutionLock::Write {
                    _guard: state.gate.clone().write_owned().await,
                }
            } else {
                ExecutionLock::Read {
                    _guard: state.gate.clone().read_owned().await,
                }
            }
        };
        let lock = tokio::select! {
            _ = reply.closed() => return,
            result = tokio::time::timeout_at(admission, acquire) => match result {
                Ok(lock) => lock,
                Err(_) => { let _ = reply.send(Err(Self::admission_expired())); return; }
            }
        };
        let checked = (|| {
            Self::check_admission_deadline(admission)?;
            if reply.is_closed() {
                return Err(ApiError::BadRequest(
                    "query observer left before admission".into(),
                ));
            }
            let current = self.authorize_connection_operation(
                session,
                connection,
                operation,
                Some(&request.sql),
                &[],
            )?;
            if !Arc::ptr_eq(&state, &current.execution) {
                return Err(ApiError::Driver(DriverError::new(
                    Code::ConnectionInvalidated,
                    "query source connection was replaced",
                )));
            }
            self.validate_execute_tx(session, connection, tx.as_ref())?;
            if let Some(tx) = &tx {
                if self.transaction_failed(session, connection, tx.tx_id)? {
                    return Err(ApiError::Conflict(
                        "failed transaction must be rolled back before executing another query"
                            .into(),
                    ));
                }
            }
            Ok(current)
        })();
        let current = match checked {
            Ok(entry) => entry,
            Err(error) => {
                let _ = reply.send(Err(error));
                return;
            }
        };
        if let Some(dispatch) = &self.csv_statement {
            if let Err(error) = dispatch.dispatch() {
                let _ = reply.send(Err(error));
                return;
            }
        }
        let native_observation = match native_mutation::admit(|| {
            observation
                .as_ref()
                .map(|observation| observation.dispatch())
                .transpose()
        }) {
            Ok(guard) => guard,
            Err(error) => {
                let _ = reply.send(Err(error));
                return;
            }
        };
        if matches!(
            operation,
            sift_protocol::OperationKind::ExecuteQuery | sift_protocol::OperationKind::ExportQuery
        ) && crate::sql_policy::statement_effect(current.driver.semantic_engine(), &request.sql)
            .is_some_and(|(_, ordinary)| !ordinary)
        {
            state
                .transaction_state_unknown
                .store(true, Ordering::Release);
        }
        if let Some(observation) = &observation {
            observation.autocommit(state.autocommit_known());
        }
        let mut lease = ExecutionLease {
            store: self.clone(),
            session,
            tx: tx.map(|tx| tx.tx_id),
            state,
            _lock: lock,
            _entry: entry,
            _resources: resources,
            _permit: permit,
            failed: false,
            settled: false,
        };
        let driver = current.driver;
        let handle = current.handle;
        let execute = driver.execute(handle.clone(), request);
        tokio::pin!(execute);
        let wait_deadline = async {
            match deadline {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => futures::future::pending::<()>().await,
            }
        };
        let result = tokio::select! {
            result = &mut execute => result,
            _ = reply.closed() => { lease.fail(); execute.await },
            _ = wait_deadline => { lease.fail(); execute.await },
        };
        let mut stream = match result {
            Ok(stream) => stream,
            Err(error) => {
                if let Some(observation) = &observation {
                    observation.error(&error);
                }
                lease.fail();
                lease.settled = true;
                let _ = reply.send(Err(ApiError::Driver(error)));
                return;
            }
        };
        let cursor = stream.cursor_id;
        let (pages, receiver) = tokio::sync::mpsc::channel(1);
        let mut native = std::mem::replace(&mut stream.rows, receiver);
        let _ = reply.send(Ok(stream));
        let cancellation =
            native_mutation::current().and_then(|admission| admission.cancellation());
        let cancelled = async {
            if let Some(token) = &cancellation {
                token.cancelled().await;
            } else {
                futures::future::pending::<()>().await;
            }
        };
        tokio::pin!(cancelled);
        let mut cancellation_requested = false;
        let mut terminal = false;
        let mut abandoned = false;
        loop {
            let page = tokio::select! {
                _ = &mut cancelled, if !cancellation_requested => {
                    cancellation_requested=true;
                    lease.fail();
                    if !terminal {Self::cancel_native(&driver,&handle,cursor,&lease.state).await;}
                    continue;
                },
                _ = pages.closed(), if !abandoned => {
                    abandoned = true;
                    if !terminal { lease.fail(); if !cancellation_requested {cancellation_requested=true;Self::cancel_native(&driver, &handle, cursor, &lease.state).await;} }
                    continue;
                },
                page = native.recv() => page,
            };
            let Some(page) = page else {
                break;
            };
            if let Some(observation) = &observation {
                observation.page(&page);
            }
            if matches!(page, Page::Error { .. }) {
                lease.fail();
            }
            terminal |= matches!(page, Page::Done { .. } | Page::Error { .. });
            if !abandoned && pages.send(page).await.is_err() {
                abandoned = true;
                if !terminal {
                    lease.fail();
                    Self::cancel_native(&driver, &handle, cursor, &lease.state).await;
                }
            }
        }
        if !terminal {
            lease.fail();
            lease.state.fence();
            if !abandoned {
                let _ = pages
                    .send(Page::Error {
                        error: DriverError::new(
                            Code::DriverInternal,
                            "native query stream ended without a terminal page; connection fenced",
                        ),
                    })
                    .await;
            }
        }
        lease.settled = true;
        if let Some(observation) = &observation {
            observation.autocommit(lease.state.autocommit_known());
        }
        if let Some(observation) = native_observation {
            observation.eof();
        }
    }

    async fn cancel_native(
        driver: &RuntimeDriver,
        handle: &RuntimeConnectionHandle,
        cursor: CursorId,
        state: &ConnectionExecutionState,
    ) {
        if matches!(
            driver.semantic_engine(),
            Some(Engine::Sqlite | Engine::SqlServer)
        ) {
            state.fence();
        }
        // Cleanup uses the captured native source even if its public entry has
        // already disappeared or current policy refuses an explicit cancel.
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            driver.cancel(handle.clone(), cursor),
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_driver_api::mock::MockDriver;
    use sift_protocol::TxMode;
    use tokio::sync::{mpsc, Notify};

    async fn fixture(driver: MockDriver) -> (SessionStore, SessionId, ConnectionId) {
        let store = SessionStore::new(DriverRegistry::builder().register(driver).build());
        let session = store
            .open_session(OpenSessionRequest {
                tag: None,
                tenant_id: None,
            })
            .id;
        let connection = store
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
        (store, session, connection)
    }

    #[tokio::test]
    async fn export_closed_admission_and_stale_sources_never_dispatch() {
        for stale in [false, true] {
            let (store, session, connection) = fixture(MockDriver::builder().build()).await;
            let source = store.conn_entry(session, connection).unwrap();
            let scoped = store.bind_source(session, connection, source).unwrap();
            let observation = Arc::new(native_execution::NativeExecution::default());
            if stale {
                store
                    .inner
                    .sessions
                    .get(&session)
                    .unwrap()
                    .connections
                    .get_mut(&connection)
                    .unwrap()
                    .execution = Arc::new(ConnectionExecutionState::default());
            } else {
                observation.close_dispatch();
            }
            let result = native_execution::scope(
                observation.clone(),
                scoped.export_stream(
                    session,
                    connection,
                    ExportRequest {
                        sql: "INSERT INTO items VALUES (1) RETURNING id".into(),
                        params: vec![],
                        format: sift_protocol::ExportFormat::Csv,
                        header: false,
                        null_display: None,
                    },
                ),
            )
            .await;
            assert!(result.is_err());
            observation.close_dispatch();
            assert!(!observation.terminal().await.attempted);
        }
    }

    #[tokio::test]
    async fn export_rechecks_authority_after_native_admission_wait() {
        let (store, session, connection) = fixture(MockDriver::builder().build()).await;
        let source = store.conn_entry(session, connection).unwrap();
        let lock = source.execution.gate.clone().write_owned().await;
        let allowed = Arc::new(AtomicBool::new(true));
        let checked = Arc::new(AtomicBool::new(false));
        let scoped = store
            .bind_source(session, connection, source)
            .unwrap()
            .with_source_authority(Arc::new({
                let allowed = allowed.clone();
                let checked = checked.clone();
                move || {
                    checked.store(true, Ordering::Release);
                    if allowed.load(Ordering::Acquire) {
                        Ok(())
                    } else {
                        Err(ApiError::Forbidden("export authority revoked".into()))
                    }
                }
            }));
        let observation = Arc::new(native_execution::NativeExecution::default());
        let task = tokio::spawn(native_execution::scope(observation.clone(), async move {
            scoped
                .export_stream(
                    session,
                    connection,
                    ExportRequest {
                        sql: "INSERT INTO items VALUES (1) RETURNING id".into(),
                        params: vec![],
                        format: sift_protocol::ExportFormat::Csv,
                        header: false,
                        null_display: None,
                    },
                )
                .await
        }));
        tokio::time::timeout(Duration::from_secs(2), async {
            while !checked.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        allowed.store(false, Ordering::Release);
        drop(lock);
        assert!(task.await.unwrap().is_err());
        observation.close_dispatch();
        assert!(!observation.terminal().await.attempted);
    }

    async fn begin(
        store: &SessionStore,
        session: SessionId,
        connection: ConnectionId,
    ) -> TransactionInfo {
        store
            .begin_transaction(
                session,
                BeginTransactionRequest {
                    connection,
                    mode: TxMode::default(),
                },
            )
            .await
            .unwrap()
    }

    async fn query(
        store: &SessionStore,
        session: SessionId,
        connection: ConnectionId,
        tx: Option<&TransactionInfo>,
    ) -> ApiResult<ResultSetStream> {
        store
            .execute_owned(ExecutionRequest {
                session,
                connection,
                entry: store.get_conn_entry(session, connection)?,
                request: ExecuteRequest::new("SELECT 1"),
                tx: tx.map(|tx| TxHandleRef {
                    tx_id: tx.tx_id,
                    connection,
                    mode: tx.mode,
                }),
                operation: sift_protocol::OperationKind::ExecuteQuery,
                resources: None,
            })
            .await
    }

    fn done() -> Page {
        Page::Done {
            affected_rows: None,
            warnings: vec![],
        }
    }

    async fn wait_for_lease(state: &ConnectionExecutionState) {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if state.gate.try_write().is_err() {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn transaction_end_waits_for_native_eof_and_refuses_queued_query() {
        let (native, rows) = mpsc::channel(2);
        let (store, session, connection) =
            fixture(MockDriver::builder().execute_rows(rows).build()).await;
        let tx = begin(&store, session, connection).await;
        let mut stream = query(&store, session, connection, Some(&tx)).await.unwrap();
        native.send(done()).await.unwrap();
        assert!(matches!(stream.rows.recv().await, Some(Page::Done { .. })));
        let end = store.commit_transaction(
            session,
            EndTransactionRequest {
                connection,
                tx_id: tx.tx_id,
            },
        );
        tokio::pin!(end);
        assert!(futures::poll!(&mut end).is_pending());
        // Wait until the independently-owned control has queued its write lease.
        tokio::task::yield_now().await;
        let next = query(&store, session, connection, Some(&tx));
        tokio::pin!(next);
        assert!(futures::poll!(&mut next).is_pending());
        assert!(!store.list_transactions(session).unwrap().is_empty());
        drop(native); // Done alone did not allow commit to pass.
        tokio::time::timeout(Duration::from_secs(1), &mut end)
            .await
            .unwrap()
            .unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(1), &mut next)
            .await
            .unwrap()
            .is_err());
        assert!(store.list_transactions(session).unwrap().is_empty());
    }

    #[tokio::test]
    async fn ordinary_queries_share_leases_but_begin_waits_for_every_native_stream() {
        let (first, first_rows) = mpsc::channel(1);
        let (second, second_rows) = mpsc::channel(1);
        let (store, session, connection) = fixture(
            MockDriver::builder()
                .execute_rows(first_rows)
                .execute_rows(second_rows)
                .build(),
        )
        .await;
        let mut one = query(&store, session, connection, None).await.unwrap();
        let mut two = query(&store, session, connection, None).await.unwrap();
        let begin = store.begin_transaction(
            session,
            BeginTransactionRequest {
                connection,
                mode: TxMode::default(),
            },
        );
        tokio::pin!(begin);
        assert!(futures::poll!(&mut begin).is_pending());
        first.send(done()).await.unwrap();
        one.rows.recv().await.unwrap();
        drop(first);
        tokio::task::yield_now().await;
        assert!(store.list_transactions(session).unwrap().is_empty());
        second.send(done()).await.unwrap();
        two.rows.recv().await.unwrap();
        drop(second);
        let tx = tokio::time::timeout(Duration::from_secs(1), &mut begin)
            .await
            .unwrap()
            .unwrap();
        store
            .rollback_transaction(
                session,
                EndTransactionRequest {
                    connection,
                    tx_id: tx.tx_id,
                },
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn abandoned_pre_cursor_observer_retains_native_lease_and_cleanup_owner() {
        let gate = Arc::new(Notify::new());
        let (native, rows) = mpsc::channel(1);
        let (store, session, connection) = fixture(
            MockDriver::builder()
                .execute_wait(gate.clone())
                .execute_rows(rows)
                .build(),
        )
        .await;
        let resources = crate::resources::ResourceManager::default();
        let tenant = sift_metadata::TenantId(42);
        let guard = resources
            .reserve(tenant, sift_protocol::TenantResource::Connections, 1)
            .unwrap();
        store
            .inner
            .sessions
            .get(&session)
            .unwrap()
            .connections
            .get_mut(&connection)
            .unwrap()
            ._resource_guard = Some(Arc::new(guard));
        let tx = begin(&store, session, connection).await;
        let state = store.get_conn_entry(session, connection).unwrap().execution;
        let observer = {
            let store = store.clone();
            let tx = tx.clone();
            tokio::spawn(async move { query(&store, session, connection, Some(&tx)).await })
        };
        wait_for_lease(&state).await;
        observer.abort();
        let _ = observer.await;
        assert!(state.gate.try_write().is_err());
        gate.notify_one();
        // Even after cursor creation, loss of the receiver must not close raw rows.
        tokio::task::yield_now().await;
        assert!(!native.is_closed());
        let close = store.close_connection(session, connection);
        tokio::pin!(close);
        assert!(futures::poll!(&mut close).is_pending());
        tokio::time::timeout(Duration::from_secs(1), async {
            while store.get_conn_entry(session, connection).is_ok() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(resources.snapshot(tenant).unwrap().usage.connections, 1);
        native.send(done()).await.unwrap();
        assert!(state.gate.try_write().is_err());
        drop(native);
        tokio::time::timeout(Duration::from_secs(1), &mut close)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(resources.snapshot(tenant).unwrap().usage.connections, 0);
        assert_eq!(store.inner.driver_tasks.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn native_failure_is_recorded_before_recovery_can_admit_and_missing_terminal_fences() {
        let (native, rows) = mpsc::channel(1);
        let (store, session, connection) =
            fixture(MockDriver::builder().execute_rows(rows).build()).await;
        let tx = begin(&store, session, connection).await;
        let mut stream = query(&store, session, connection, Some(&tx)).await.unwrap();
        native
            .send(Page::Error {
                error: DriverError::new(Code::SyntaxError, "invalid"),
            })
            .await
            .unwrap();
        stream.rows.recv().await.unwrap();
        assert!(store
            .transaction_failed(session, connection, tx.tx_id)
            .unwrap());
        drop(native);
        store
            .rollback_transaction(
                session,
                EndTransactionRequest {
                    connection,
                    tx_id: tx.tx_id,
                },
            )
            .await
            .unwrap();

        let (native, rows) = mpsc::channel(1);
        let (store, session, connection) =
            fixture(MockDriver::builder().execute_rows(rows).build()).await;
        let mut stream = query(&store, session, connection, None).await.unwrap();
        drop(native);
        assert!(matches!(stream.rows.recv().await, Some(Page::Error { .. })));
        assert!(query(&store, session, connection, None).await.is_err());
    }

    #[tokio::test]
    async fn native_drivers_with_independent_allocators_keep_distinct_host_transactions() {
        let store = SessionStore::new(
            DriverRegistry::builder()
                .register(MockDriver::builder().engine(Engine::Postgres).build())
                .register(MockDriver::builder().engine(Engine::SqlServer).build())
                .build(),
        );
        let session = store
            .open_session(OpenSessionRequest {
                tag: None,
                tenant_id: None,
            })
            .id;
        let mut ids = Vec::new();
        for engine in [Engine::Postgres, Engine::SqlServer] {
            let connection = store
                .open_connection(
                    session,
                    engine,
                    ConnectionSpec {
                        host: "mock.invalid".into(),
                        port: None,
                        database: None,
                        user: "mock".into(),
                        password: None,
                        ssl_mode: None,
                        engine_specific: None,
                    },
                )
                .await
                .unwrap()
                .id;
            let tx = begin(&store, session, connection).await;
            let raw = store.tx_handle_for(session, connection, tx.tx_id).unwrap();
            assert_eq!(raw.builtin().unwrap().tx_id.0, 1);
            ids.push((connection, tx.tx_id));
        }
        assert_ne!(ids[0].1, ids[1].1);
        assert_eq!(store.list_transactions(session).unwrap().len(), 2);
        for (connection, tx_id) in ids {
            store
                .rollback_transaction(session, EndTransactionRequest { connection, tx_id })
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn late_begin_after_source_close_rolls_back_instead_of_publishing() {
        let gate = Arc::new(Notify::new());
        let (store, session, connection) =
            fixture(MockDriver::builder().begin_wait(gate.clone()).build()).await;
        let state = store.get_conn_entry(session, connection).unwrap().execution;
        let begin = store.begin_transaction(
            session,
            BeginTransactionRequest {
                connection,
                mode: TxMode::default(),
            },
        );
        tokio::pin!(begin);
        assert!(futures::poll!(&mut begin).is_pending());
        wait_for_lease(&state).await;
        let close = store.close_connection(session, connection);
        tokio::pin!(close);
        assert!(futures::poll!(&mut close).is_pending());
        tokio::time::timeout(Duration::from_secs(1), async {
            while store.get_conn_entry(session, connection).is_ok() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        gate.notify_one();
        assert!(tokio::time::timeout(Duration::from_secs(1), &mut begin)
            .await
            .unwrap()
            .is_err());
        tokio::time::timeout(Duration::from_secs(1), &mut close)
            .await
            .unwrap()
            .unwrap();
        assert!(store.list_transactions(session).unwrap().is_empty());
        assert!(!state.is_fenced()); // orphan rollback completed, not merely hidden.
    }
}
