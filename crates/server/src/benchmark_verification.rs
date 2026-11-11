//! Untimed pair checks retain only fixed observations, never native row data.
use super::*;
use sift_core::performance_result::{CheckError, Fingerprint, ResultCheck};
use sift_protocol::{
    BenchmarkResultCheck, BenchmarkResultVerification, BenchmarkVerificationOutcome as Outcome,
    BenchmarkVerificationSnapshot as Snapshot,
};

pub(super) fn stopped(
    config: BenchmarkResultCheck,
    engine: Engine,
    outcome: Outcome,
) -> BenchmarkResultVerification {
    BenchmarkResultVerification {
        version: 1,
        config,
        snapshot: match engine {
            Engine::Postgres => Snapshot::ReadOnlyRepeatableRead,
            Engine::Sqlite => Snapshot::ReadOnlySerializable,
            Engine::SqlServer => Snapshot::IndependentReads,
        },
        outcome,
        baseline: None,
        candidate: None,
    }
}
impl SessionStore {
    pub(super) async fn verify_benchmark_pair(
        &self,
        session: SessionId,
        source: ConnectionId,
        connection: ConnectionId,
        workloads: IterationWorkloads<'_>,
        token: &CancellationToken,
        engine: Engine,
    ) -> BenchmarkResultVerification {
        let config = workloads
            .mode
            .result_check()
            .expect("requested verification");
        let mut report = stopped(config, engine, Outcome::Failed);
        self.update_performance_progress(
            session,
            source,
            workloads.requests[0].run_id,
            PerformanceRunKind::Benchmark,
            PerformanceStage::Verifying,
            None,
        );
        // Sorted digest bags plus bounded JSON key references and schema scratch.
        let scratch = 2 * 1024 * 1024
            + if config.order == sift_protocol::BenchmarkResultOrder::Unordered {
                config.max_rows as usize * 64
            } else {
                0
            };
        let scratch_guard = Arc::new(
            match self.reserve_session_retained_bytes(session, scratch) {
                Ok(guard) => guard,
                Err(_) => {
                    report.outcome = Outcome::LimitExceeded;
                    return report;
                }
            },
        );
        let deadline = self
            .inner
            .benchmarks
            .get(&(session, source))
            .map(|run| run.deadline)
            .unwrap_or_else(tokio::time::Instant::now);
        let mut first: Option<Fingerprint> = None;
        for (index, request) in workloads.requests.iter().enumerate() {
            if token.is_cancelled() {
                report.outcome = Outcome::Cancelled;
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                report.outcome = Outcome::TimedOut;
                break;
            }
            if [
                workloads.mode.operation(),
                sift_protocol::OperationKind::ExecuteQuery,
            ]
            .into_iter()
            .any(|kind| {
                self.authorize_connection_operation(session, source, kind, Some(&request.sql), &[])
                    .is_err()
            }) {
                report.outcome = Outcome::AccessChanged;
                break;
            }
            let entry = match self.conn_entry(session, connection) {
                Ok(entry) => entry,
                Err(_) => {
                    report.outcome = Outcome::Failed;
                    break;
                }
            };
            let resources = match self.reserve_query_resources(&entry) {
                Ok(resources) => resources,
                Err(_) => {
                    report.outcome = Outcome::LimitExceeded;
                    break;
                }
            };
            let driver = entry.driver.clone();
            let handle = entry.handle.clone();
            let sql = request.sql.clone();
            let params = request.params.clone();
            let cursor = Arc::new(Mutex::new(None));
            let cursor_slot = cursor.clone();
            let scratch_guard = scratch_guard.clone();
            let mut task = tokio::spawn(async move {
                let _scratch = scratch_guard;
                let _resources = resources;
                let mut check = ResultCheck::new(config)?;
                let mut stream = driver
                    .execute(
                        handle,
                        ExecuteRequest {
                            sql,
                            params,
                            transform: None,
                        },
                    )
                    .await
                    .map_err(|_| CheckError::Failed)?;
                *cursor_slot.lock().unwrap() = Some(stream.cursor_id);
                while let Some(page) = stream.rows.recv().await {
                    check.page(&page)?;
                }
                check.finish()
            });
            let until = deadline
                .min(tokio::time::Instant::now() + Duration::from_millis(request.query_timeout_ms));
            let result = tokio::select! {biased;
                _=token.cancelled()=>Err(Outcome::Cancelled),
                _=tokio::time::sleep_until(until)=>Err(Outcome::TimedOut),
                result=&mut task=>match result {Ok(Ok(value))=>Ok(value),Ok(Err(CheckError::Limit))=>Err(Outcome::LimitExceeded),Ok(Err(CheckError::Unsupported))=>Err(Outcome::Unsupported),_=>Err(Outcome::Failed)}
            };
            match result {
                Ok(value) => {
                    if index == 0 {
                        report.baseline = Some(value.observation);
                        first = Some(value);
                    } else {
                        report.candidate = Some(value.observation);
                        report.outcome =
                            if first.as_ref().is_some_and(|first| first.matches(&value)) {
                                Outcome::Matched
                            } else {
                                Outcome::Different
                            };
                    }
                }
                Err(outcome) => {
                    task.abort();
                    let cursor_id = *cursor.lock().unwrap();
                    if let Some(cursor_id) = cursor_id {
                        let _ = tokio::time::timeout(
                            Duration::from_secs(5),
                            entry.driver.cancel(entry.handle.clone(), cursor_id),
                        )
                        .await;
                    }
                    report.outcome = outcome;
                    break;
                }
            }
        }
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_protocol::{BenchmarkPairOrder, ColumnMetadata, Page, PrimitiveType, TypeRef};
    fn shape() -> Page {
        Page::NextResult {
            columns: vec![ColumnMetadata::new(
                "x",
                TypeRef::Primitive(PrimitiveType::Int32),
            )],
        }
    }
    fn done() -> Page {
        Page::Done {
            affected_rows: None,
            warnings: vec![],
        }
    }
    async fn fixture(
        driver: sift_driver_api::mock::MockDriver,
    ) -> (
        SessionStore,
        SessionId,
        ConnectionId,
        ConnectionId,
        Arc<sift_driver_api::mock::MockDriver>,
    ) {
        let observer = Arc::new(driver);
        let registry = DriverRegistry::builder().build();
        registry
            .providers()
            .replace([Arc::new(crate::registry::BuiltinProviderAdapter::new(
                observer.clone(),
            )) as Arc<dyn crate::registry::DatabaseProvider>])
            .unwrap();
        let store = SessionStore::new(registry);
        let session = store
            .open_session(OpenSessionRequest {
                tag: None,
                tenant_id: None,
            })
            .id;
        let mut connections = Vec::new();
        for _ in 0..2 {
            connections.push(
                store
                    .open_connection(
                        session,
                        Engine::Postgres,
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
                    .id,
            );
        }
        let store = store
            .bind_source(
                session,
                connections[0],
                store.conn_entry(session, connections[0]).unwrap(),
            )
            .unwrap();
        (store, session, connections[0], connections[1], observer)
    }
    fn requests() -> [BenchmarkRequest; 2] {
        std::array::from_fn(|_| BenchmarkRequest {
            run_id: uuid::Uuid::new_v4(),
            sql: "SELECT 1 AS x".into(),
            params: vec![],
            warmups: 1,
            iterations: 2,
            query_timeout_ms: 100,
            total_budget_ms: 5000,
            delay_ms: 0,
            workload_confirmed: true,
        })
    }
    fn register(
        store: &SessionStore,
        session: SessionId,
        source: ConnectionId,
        requests: &[BenchmarkRequest; 2],
        token: &CancellationToken,
    ) {
        store.inner.benchmarks.insert(
            (session, source),
            ActiveBenchmark {
                run_id: requests[0].run_id,
                cancellation: token.clone(),
                deadline: tokio::time::Instant::now() + Duration::from_secs(5),
                progress: Arc::new(performance_progress::ProgressState::benchmark(&requests[0])),
            },
        );
    }
    fn workloads(requests: &[BenchmarkRequest; 2]) -> IterationWorkloads<'_> {
        IterationWorkloads {
            requests,
            mode: RunMode::Pair(
                BenchmarkPairOrder::Alternating,
                Some(BenchmarkResultCheck::default()),
            ),
        }
    }
    #[tokio::test]
    async fn verification_requires_raw_eof_and_refuses_late_error_or_second_result() {
        for tail in [
            vec![],
            vec![shape()],
            vec![Page::Error {
                error: DriverError::new(Code::DriverInternal, "PRIVATE-NATIVE-ERROR"),
            }],
        ] {
            let (sender, rows) = tokio::sync::mpsc::channel(8);
            sender.send(shape()).await.unwrap();
            sender.send(done()).await.unwrap();
            let driver = sift_driver_api::mock::MockDriver::builder()
                .execute_rows(rows)
                .execute_ok(vec![shape(), done()])
                .build();
            let (store, session, source, dedicated, observer) = fixture(driver).await;
            let requests = requests();
            let token = CancellationToken::new();
            register(&store, session, source, &requests, &token);
            let run_store = store.clone();
            let run_requests = requests.clone();
            let run_token = token.clone();
            let task = tokio::spawn(async move {
                run_store
                    .verify_benchmark_pair(
                        session,
                        source,
                        dedicated,
                        workloads(&run_requests),
                        &run_token,
                        Engine::Postgres,
                    )
                    .await
            });
            while !observer.invocations().contains(&"execute") {
                tokio::task::yield_now().await;
            }
            tokio::task::yield_now().await;
            assert!(!task.is_finished(), "Done must not replace raw EOF");
            let snapshot = store
                .performance_progress(
                    session,
                    source,
                    requests[0].run_id,
                    PerformanceRunKind::Benchmark,
                )
                .unwrap()
                .unwrap();
            assert_eq!(snapshot.stage, PerformanceStage::Verifying);
            for page in &tail {
                sender.send(page.clone()).await.unwrap();
            }
            drop(sender);
            let report = task.await.unwrap();
            assert_eq!(
                report.outcome,
                if tail.is_empty() {
                    Outcome::Matched
                } else if matches!(tail[0], Page::Error { .. }) {
                    Outcome::Failed
                } else {
                    Outcome::Unsupported
                }
            );
            assert_eq!(
                observer
                    .invocations()
                    .iter()
                    .filter(|method| **method == "execute")
                    .count(),
                if tail.is_empty() { 2 } else { 1 }
            );
            assert!(!serde_json::to_string(&report)
                .unwrap()
                .contains("PRIVATE-NATIVE-ERROR"));
        }
    }
    #[tokio::test(start_paused = true)]
    async fn verification_timeout_cancel_and_source_replacement_stop_before_candidate() {
        for action in 0..3 {
            let (sender, rows) = tokio::sync::mpsc::channel(8);
            sender.send(shape()).await.unwrap();
            let driver = sift_driver_api::mock::MockDriver::builder()
                .execute_rows(rows)
                .build();
            let (store, session, source, dedicated, observer) = fixture(driver).await;
            let requests = requests();
            let token = CancellationToken::new();
            register(&store, session, source, &requests, &token);
            let run_store = store.clone();
            let run_requests = requests.clone();
            let run_token = token.clone();
            let task = tokio::spawn(async move {
                run_store
                    .verify_benchmark_pair(
                        session,
                        source,
                        dedicated,
                        workloads(&run_requests),
                        &run_token,
                        Engine::Postgres,
                    )
                    .await
            });
            while !observer.invocations().contains(&"execute") {
                tokio::task::yield_now().await;
            }
            if action == 0 {
                tokio::time::advance(Duration::from_millis(101)).await;
            } else if action == 1 {
                token.cancel();
            } else {
                let replacement = store.conn_entry(session, dedicated).unwrap().execution;
                store
                    .with_session(&session, |s| {
                        s.connections.get_mut(&source).unwrap().execution = replacement;
                    })
                    .unwrap();
                sender.send(done()).await.unwrap();
            }
            drop(sender);
            let report = task.await.unwrap();
            assert_eq!(
                report.outcome,
                [
                    Outcome::TimedOut,
                    Outcome::Cancelled,
                    Outcome::AccessChanged
                ][action]
            );
            assert_eq!(
                observer
                    .invocations()
                    .iter()
                    .filter(|method| **method == "execute")
                    .count(),
                1
            );
        }
    }
}
