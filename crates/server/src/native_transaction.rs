//! Native control evidence retained by the existing transaction worker.
use super::*;
use tokio::sync::Notify;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Begin,
    Commit,
    Rollback,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Evidence {
    pub admitted: bool,
    pub transaction_id: Option<TxId>,
    pub source_generation: Option<uuid::Uuid>,
    /// Explicit observation of the host's raw/opaque transaction-state fence.
    /// A positive driver acknowledgement alone cannot repair an unknown scope.
    pub transaction_state_known: Option<bool>,
    /// The driver's acknowledgement, before host publication/removal.
    pub acknowledged: Option<bool>,
    pub result_code: Option<String>,
    pub published: Option<TransactionInfo>,
    /// Compensating cleanup when a successful BEGIN cannot be published.
    pub cleanup_rollback: Option<bool>,
    pub finished: bool,
}

struct State {
    evidence: Evidence,
    closed: bool,
    workers: usize,
}

pub(crate) struct Observation {
    action: Action,
    state: Mutex<State>,
    changed: Notify,
}

tokio::task_local! { static CURRENT: Arc<Observation>; }

impl Observation {
    pub(crate) fn new(action: Action) -> Arc<Self> {
        Arc::new(Self {
            action,
            state: Mutex::new(State {
                evidence: Evidence::default(),
                closed: false,
                workers: 0,
            }),
            changed: Notify::new(),
        })
    }

    pub(crate) fn snapshot(&self) -> Evidence {
        self.state.lock().unwrap().evidence.clone()
    }

    pub(crate) fn close_dispatch(&self) {
        let mut state = self.state.lock().unwrap();
        state.closed = true;
        state.evidence.finished = state.workers == 0;
        drop(state);
        self.changed.notify_waiters();
    }

    pub(crate) async fn terminal(&self) -> Evidence {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let evidence = self.snapshot();
            if evidence.finished {
                return evidence;
            }
            changed.await;
        }
    }
}

struct Admission(Arc<Observation>);
impl Drop for Admission {
    fn drop(&mut self) {
        self.0.close_dispatch();
    }
}

pub(crate) async fn scope<F: std::future::Future>(
    observation: Arc<Observation>,
    work: F,
) -> F::Output {
    let _admission = Admission(observation.clone());
    CURRENT.scope(observation, work).await
}

/// Create before spawning: dropping an unpolled worker must also settle evidence.
pub(super) struct Worker(Option<Arc<Observation>>, Option<native_mutation::Context>);
impl Worker {
    pub(super) fn capture() -> Self {
        let observation = CURRENT.try_with(Arc::clone).ok();
        if let Some(observation) = &observation {
            let mut state = observation.state.lock().unwrap();
            state.workers += 1;
            state.evidence.finished = false;
        }
        Self(observation, Some(native_mutation::Context::capture()))
    }

    pub(super) async fn run<T: Send + 'static>(
        mut self,
        work: futures::future::BoxFuture<'static, ApiResult<T>>,
    ) -> ApiResult<T> {
        let observation = self.0.clone();
        self.1
            .take()
            .unwrap()
            .run(Box::pin(async move {
                match observation {
                    Some(observation) => CURRENT.scope(observation, work).await,
                    None => work.await,
                }
            }))
            .await
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(observation) = &self.0 {
            let mut state = observation.state.lock().unwrap();
            state.workers -= 1;
            state.evidence.finished = state.closed && state.workers == 0;
            drop(state);
            observation.changed.notify_waiters();
        }
    }
}

/// Atomically admit the native call with its fallible host claim. No await or
/// driver call belongs in `prepare`; refusal must not leave an ending claim.
pub(super) fn admit(
    action: Action,
    transaction: Option<TxId>,
    source: &super::execution_control::ConnectionExecutionState,
    prepare: impl FnOnce() -> ApiResult<()>,
) -> ApiResult<()> {
    let prepare_observed = || {
        if action != Action::Rollback
            && native_mutation::current().is_some()
            && !source.autocommit_known()
        {
            return Err(ApiError::Driver(DriverError::new(
                Code::ConnectionInvalidated,
                "structured transaction scope is unknown; reconnect and inspect the database",
            )));
        }
        let observation = CURRENT.try_with(Arc::clone).ok();
        let Some(observation) = observation else {
            return prepare();
        };
        let mut state = observation.state.lock().unwrap();
        if observation.action != action || state.closed || state.evidence.admitted {
            return Err(ApiError::Driver(DriverError::new(
                Code::QueryCanceled,
                "native transaction admission is closed or already used",
            )));
        }
        prepare()?;
        state.evidence.admitted = true;
        state.evidence.transaction_id = transaction;
        state.evidence.source_generation = Some(source.review_id);
        state.evidence.transaction_state_known = Some(source.autocommit_known());
        Ok(())
    };
    // Cleanup may still dispatch after the owner's mutation deadline.
    if action == Action::Rollback {
        prepare_observed()
    } else {
        native_mutation::admit(prepare_observed)
    }
}

/// Structured owners await the actual worker, not its method observer deadline.
pub(super) async fn owned_begin(
    work: futures::future::BoxFuture<'_, ApiResult<TransactionInfo>>,
) -> ApiResult<TransactionInfo> {
    if native_mutation::current().is_none() || CURRENT.try_with(|_| ()).is_ok() {
        return work.await;
    }
    let observation = Observation::new(Action::Begin);
    let result = scope(observation.clone(), work).await;
    let evidence = observation.terminal().await;
    evidence.published.map(Ok).unwrap_or(result)
}

pub(super) async fn owned_end(
    action: Option<Action>,
    work: futures::future::BoxFuture<'_, ApiResult<()>>,
) -> ApiResult<()> {
    let Some(action) = action else {
        return work.await;
    };
    if native_mutation::current().is_none() || CURRENT.try_with(|_| ()).is_ok() {
        return work.await;
    }
    let observation = Observation::new(action);
    let result = scope(observation.clone(), work).await;
    let evidence = observation.terminal().await;
    if evidence.acknowledged == Some(true) && evidence.transaction_state_known == Some(true) {
        Ok(())
    } else if evidence.acknowledged == Some(true) {
        Err(ApiError::Driver(DriverError::new(
            Code::ConnectionInvalidated,
            "native rollback acknowledged under unknown transaction scope; inspect the database",
        )))
    } else {
        result
    }
}

pub(super) fn acknowledge(transaction: Option<TxId>, result: Result<(), &DriverError>) {
    let _ = CURRENT.try_with(|observation| {
        let mut state = observation.state.lock().unwrap();
        if state.evidence.admitted && state.evidence.acknowledged.is_none() {
            state.evidence.transaction_id = transaction;
            state.evidence.acknowledged = Some(result.is_ok());
            state.evidence.result_code = result
                .err()
                .map(|error| crate::error::audit_code(&error.code));
        }
    });
}

pub(super) fn published(info: &TransactionInfo) {
    let _ = CURRENT.try_with(|observation| {
        observation.state.lock().unwrap().evidence.published = Some(info.clone());
    });
}

pub(super) fn cleanup_rollback(succeeded: bool) {
    let _ = CURRENT.try_with(|observation| {
        observation.state.lock().unwrap().evidence.cleanup_rollback = Some(succeeded);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_driver_api::mock::MockDriver;
    use sift_protocol::TxMode;

    async fn connection(driver: Arc<MockDriver>) -> (SessionStore, SessionId, ConnectionId) {
        let registry = crate::DriverRegistry::new();
        registry
            .providers()
            .replace([
                Arc::new(crate::registry::BuiltinProviderAdapter::new(driver))
                    as Arc<dyn crate::registry::DatabaseProvider>,
            ])
            .unwrap();
        let store = SessionStore::new(registry);
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
                    database: Some("fixture".into()),
                    user: "fixture".into(),
                    password: None,
                    ssl_mode: None,
                    engine_specific: None,
                },
            )
            .await
            .unwrap()
            .id;
        (store, session, connection)
    }

    async fn entered(driver: &MockDriver, method: &str) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while !driver.invocations().contains(&method) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    async fn terminal(observation: &Observation) -> Evidence {
        tokio::time::timeout(Duration::from_secs(2), observation.terminal())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn late_begin_retains_actual_handle_and_publication_after_observer_timeout() {
        let gate = Arc::new(Notify::new());
        let driver = Arc::new(MockDriver::builder().begin_wait(gate.clone()).build());
        let (store, session, connection) = connection(driver.clone()).await;
        store.set_request_timeout(Duration::from_millis(20));
        let observation = Observation::new(Action::Begin);
        let result = scope(
            observation.clone(),
            store.begin_transaction(
                session,
                BeginTransactionRequest {
                    connection,
                    mode: TxMode::default(),
                },
            ),
        )
        .await;
        assert!(matches!(
            result,
            Err(ApiError::Driver(DriverError {
                code: Code::QueryTimedOut,
                ..
            }))
        ));
        let pending = observation.snapshot();
        assert!(pending.admitted && !pending.finished);
        assert!(pending.transaction_id.is_none() && pending.acknowledged.is_none());
        gate.notify_one();
        let actual = terminal(&observation).await;
        let published = actual.published.unwrap();
        assert_eq!(actual.acknowledged, Some(true));
        assert_eq!(actual.transaction_state_known, Some(true));
        assert_eq!(actual.transaction_id, Some(published.tx_id));
        assert_eq!(
            store.list_transactions(session).unwrap()[0]
                .transaction
                .tx_id,
            published.tx_id
        );
        assert_eq!(
            driver
                .invocations()
                .iter()
                .filter(|m| **m == "begin")
                .count(),
            1
        );
        store
            .rollback_transaction(
                session,
                EndTransactionRequest {
                    connection,
                    tx_id: published.tx_id,
                },
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn late_commit_ack_survives_timeout_and_host_session_removal() {
        for (remove_session, unknown_scope) in [(false, false), (true, false), (false, true)] {
            let gate = Arc::new(Notify::new());
            let driver = Arc::new(
                MockDriver::builder()
                    .commit_wait(gate.clone())
                    .execute_ok(vec![sift_protocol::Page::Done {
                        affected_rows: Some(0),
                        warnings: vec![],
                    }])
                    .build(),
            );
            let (store, session, connection) = connection(driver.clone()).await;
            let info = store
                .begin_transaction(
                    session,
                    BeginTransactionRequest {
                        connection,
                        mode: TxMode::default(),
                    },
                )
                .await
                .unwrap();
            let source = store.conn_entry(session, connection).unwrap();
            if unknown_scope {
                store
                    .execute_http(
                        session,
                        sift_protocol::ExecuteRequestHttp {
                            connection,
                            sql: "ROLLBACK".into(),
                            params: vec![],
                            tx: Some(sift_protocol::TxHandleRef {
                                tx_id: info.tx_id,
                                connection,
                                mode: info.mode,
                            }),
                            room_id: None,
                            connection_profile_id: None,
                            transform: None,
                            source: None,
                        },
                    )
                    .await
                    .unwrap();
                assert!(!source.execution.autocommit_known());
            }
            store.set_request_timeout(Duration::from_millis(20));
            let observation = Observation::new(Action::Commit);
            assert!(scope(
                observation.clone(),
                store.commit_transaction(
                    session,
                    EndTransactionRequest {
                        connection,
                        tx_id: info.tx_id
                    }
                )
            )
            .await
            .is_err());
            assert!(observation.snapshot().admitted);
            assert!(!observation.snapshot().finished);
            if remove_session {
                store.close_session(session).unwrap();
            }
            gate.notify_one();
            let actual = terminal(&observation).await;
            assert_eq!(actual.transaction_id, Some(info.tx_id));
            assert_eq!(actual.acknowledged, Some(true));
            assert_eq!(actual.source_generation, Some(source.execution.review_id));
            assert_eq!(actual.transaction_state_known, Some(!unknown_scope));
            assert!(actual.result_code.is_none());
            assert_eq!(
                driver
                    .invocations()
                    .iter()
                    .filter(|m| **m == "commit")
                    .count(),
                1
            );
            if remove_session {
                assert!(source.execution.is_fenced());
            } else {
                assert!(store.list_transactions(session).unwrap().is_empty());
            }
        }
    }

    #[tokio::test]
    async fn begin_cleanup_is_separate_from_native_begin_ack_and_publication() {
        for cleanup_fails in [false, true] {
            let gate = Arc::new(Notify::new());
            let mut builder = MockDriver::builder().begin_wait(gate.clone());
            if cleanup_fails {
                builder = builder.rollback_err(DriverError::new(
                    Code::Other {
                        message: "private-cleanup-diagnostic".into(),
                    },
                    "private-cleanup-diagnostic",
                ));
            }
            let driver = Arc::new(builder.build());
            let (store, session, connection) = connection(driver.clone()).await;
            let source = store.conn_entry(session, connection).unwrap();
            let authority = Arc::new(AtomicBool::new(true));
            let retained = authority.clone();
            let store = store.with_source_authority(Arc::new(move || {
                if retained.load(Ordering::Acquire) {
                    Ok(())
                } else {
                    Err(ApiError::Forbidden("source authority revoked".into()))
                }
            }));
            let observation = Observation::new(Action::Begin);
            let owned = observation.clone();
            let task = tokio::spawn(async move {
                scope(
                    owned,
                    store.begin_transaction(
                        session,
                        BeginTransactionRequest {
                            connection,
                            mode: TxMode::default(),
                        },
                    ),
                )
                .await
            });
            entered(&driver, "begin").await;
            authority.store(false, Ordering::Release);
            gate.notify_one();
            assert!(matches!(task.await.unwrap(), Err(ApiError::Forbidden(_))));
            let actual = terminal(&observation).await;
            assert_eq!(actual.acknowledged, Some(true));
            assert!(actual.transaction_id.is_some() && actual.published.is_none());
            assert_eq!(actual.cleanup_rollback, Some(!cleanup_fails));
            assert_eq!(source.execution.is_fenced(), cleanup_fails);
            assert!(!format!("{actual:?}").contains("private-cleanup-diagnostic"));
        }
    }

    #[tokio::test]
    async fn closed_admission_does_not_dispatch_or_leave_transaction_ending() {
        let driver = Arc::new(MockDriver::builder().build());
        let (store, session, connection) = connection(driver.clone()).await;
        let tx = store
            .begin_transaction(
                session,
                BeginTransactionRequest {
                    connection,
                    mode: TxMode::default(),
                },
            )
            .await
            .unwrap();
        let source = store.conn_entry(session, connection).unwrap();
        let gate = source.execution.gate.clone().read_owned().await;
        let observation = Observation::new(Action::Commit);
        let owned = observation.clone();
        let worker_store = store.clone();
        let task = tokio::spawn(async move {
            scope(
                owned,
                worker_store.commit_transaction(
                    session,
                    EndTransactionRequest {
                        connection,
                        tx_id: tx.tx_id,
                    },
                ),
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while observation.state.lock().unwrap().workers == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        observation.close_dispatch();
        drop(gate);
        assert!(task.await.unwrap().is_err());
        let actual = terminal(&observation).await;
        assert!(!actual.admitted && actual.acknowledged.is_none());
        assert!(actual.source_generation.is_none() && actual.transaction_state_known.is_none());
        assert!(!driver.invocations().contains(&"commit"));
        assert!(!source.execution.is_fenced());
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
    async fn structured_owner_waits_for_late_begin_then_cleans_up_after_deadline() {
        let gate = Arc::new(Notify::new());
        let driver = Arc::new(MockDriver::builder().begin_wait(gate.clone()).build());
        let (store, session, connection) = connection(driver.clone()).await;
        store.set_request_timeout(Duration::from_millis(20));
        let admission = native_mutation::Admission::new(Duration::from_millis(40));
        let worker_admission = admission.clone();
        let task = tokio::spawn(native_mutation::scope(worker_admission, async move {
            let tx = store
                .begin_transaction(
                    session,
                    BeginTransactionRequest {
                        connection,
                        mode: TxMode::default(),
                    },
                )
                .await
                .unwrap();
            assert!(store
                .commit_transaction(
                    session,
                    EndTransactionRequest {
                        connection,
                        tx_id: tx.tx_id,
                    }
                )
                .await
                .is_err());
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
            assert!(store.list_transactions(session).unwrap().is_empty());
        }));
        entered(&driver, "begin").await;
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(
            !task.is_finished(),
            "method timeout must retain native ownership"
        );
        gate.notify_one();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        assert!(admission.was_admitted());
        assert!(!driver.invocations().contains(&"commit"));
        assert_eq!(
            driver
                .invocations()
                .iter()
                .filter(|&&m| m == "rollback")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn structured_owner_retains_late_issued_commit_acknowledgement() {
        let gate = Arc::new(Notify::new());
        let driver = Arc::new(MockDriver::builder().commit_wait(gate.clone()).build());
        let (store, session, connection) = connection(driver.clone()).await;
        let tx = store
            .begin_transaction(
                session,
                BeginTransactionRequest {
                    connection,
                    mode: TxMode::default(),
                },
            )
            .await
            .unwrap();
        store.set_request_timeout(Duration::from_millis(20));
        let admission = native_mutation::Admission::new(Duration::from_secs(2));
        let task = tokio::spawn(native_mutation::scope(admission.clone(), async move {
            store
                .commit_transaction(
                    session,
                    EndTransactionRequest {
                        connection,
                        tx_id: tx.tx_id,
                    },
                )
                .await
                .unwrap();
            assert!(store.list_transactions(session).unwrap().is_empty());
        }));
        entered(&driver, "commit").await;
        admission.close();
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(!task.is_finished());
        gate.notify_one();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            driver
                .invocations()
                .iter()
                .filter(|&&m| m == "commit")
                .count(),
            1
        );
        assert!(!driver.invocations().contains(&"rollback"));
    }

    #[tokio::test]
    async fn closed_mutation_owner_refuses_statement_and_commit_but_allows_cleanup() {
        let driver = Arc::new(MockDriver::builder().build());
        let (store, session, connection) = connection(driver.clone()).await;
        let tx = store
            .begin_transaction(
                session,
                BeginTransactionRequest {
                    connection,
                    mode: TxMode::default(),
                },
            )
            .await
            .unwrap();
        let admission = native_mutation::Admission::new(Duration::from_secs(2));
        admission.close();
        native_mutation::scope(admission.clone(), async {
            assert!(store
                .execute_http(
                    session,
                    ExecuteRequestHttp {
                        connection,
                        sql: "UPDATE fixture SET value = 1".into(),
                        params: vec![],
                        tx: Some(TxHandleRef {
                            tx_id: tx.tx_id,
                            connection,
                            mode: tx.mode
                        }),
                        room_id: None,
                        connection_profile_id: None,
                        transform: None,
                        source: None,
                    }
                )
                .await
                .is_err());
            assert!(store
                .commit_transaction(
                    session,
                    EndTransactionRequest {
                        connection,
                        tx_id: tx.tx_id,
                    }
                )
                .await
                .is_err());
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
        })
        .await;
        assert!(!admission.was_admitted());
        assert!(!driver.invocations().contains(&"execute"));
        assert!(!driver.invocations().contains(&"commit"));
        assert_eq!(
            driver
                .invocations()
                .iter()
                .filter(|&&m| m == "rollback")
                .count(),
            1
        );
    }
    #[tokio::test]
    async fn structured_owner_never_promotes_unknown_raw_scope_to_owned_end_proof() {
        let driver = Arc::new(
            MockDriver::builder()
                .execute_ok(vec![Page::Done {
                    affected_rows: Some(0),
                    warnings: vec![],
                }])
                .build(),
        );
        let (store, session, connection) = connection(driver.clone()).await;
        let tx = store
            .begin_transaction(
                session,
                BeginTransactionRequest {
                    connection,
                    mode: TxMode::default(),
                },
            )
            .await
            .unwrap();
        let source = store.conn_entry(session, connection).unwrap();
        store
            .execute_http(
                session,
                ExecuteRequestHttp {
                    connection,
                    sql: "ROLLBACK".into(),
                    params: vec![],
                    tx: Some(TxHandleRef {
                        tx_id: tx.tx_id,
                        connection,
                        mode: tx.mode,
                    }),
                    room_id: None,
                    connection_profile_id: None,
                    transform: None,
                    source: None,
                },
            )
            .await
            .unwrap();
        assert!(!source.execution.autocommit_known());
        let admission = native_mutation::Admission::new(Duration::from_secs(2));
        native_mutation::scope(admission.clone(), async {
            assert!(store
                .commit_transaction(
                    session,
                    EndTransactionRequest {
                        connection,
                        tx_id: tx.tx_id,
                    }
                )
                .await
                .is_err());
            assert!(
                store
                    .rollback_transaction(
                        session,
                        EndTransactionRequest {
                            connection,
                            tx_id: tx.tx_id,
                        }
                    )
                    .await
                    .is_err(),
                "a cleanup acknowledgement does not prove owned rollback"
            );
        })
        .await;
        assert!(!admission.was_admitted());
        assert!(!driver.invocations().contains(&"commit"));
        assert_eq!(
            driver
                .invocations()
                .iter()
                .filter(|&&m| m == "rollback")
                .count(),
            1
        );
        assert!(source.execution.is_fenced());
    }
}
