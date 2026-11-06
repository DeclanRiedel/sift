//! Session-owned serial benchmarks. Rows are consumed and dropped, never added
//! to the result cache. One run per source connection; each run owns its connection.
use super::*;
use sift_protocol::{
    BenchmarkEnvironment, BenchmarkOutcome, BenchmarkReport, BenchmarkRequest, BenchmarkSample,
    PerformanceRunKind, PerformanceStage,
};
use tokio_util::sync::CancellationToken;

pub(super) struct ActiveBenchmark {
    pub(super) run_id: uuid::Uuid,
    pub(super) cancellation: CancellationToken,
    pub(super) deadline: tokio::time::Instant,
    pub(super) progress: Arc<performance_progress::ProgressState>,
}

#[derive(Clone, Copy)]
enum RunMode {
    Single,
    Pair(sift_protocol::BenchmarkPairOrder),
    Suite(sift_protocol::BenchmarkSuiteOrder),
}
impl RunMode {
    fn operation(self) -> sift_protocol::OperationKind {
        match self {
            Self::Single => sift_protocol::OperationKind::BenchmarkQuery,
            Self::Pair(_) => sift_protocol::OperationKind::BenchmarkPair,
            Self::Suite(_) => sift_protocol::OperationKind::BenchmarkSuite,
        }
    }
    fn pair_order(self) -> Option<sift_protocol::BenchmarkPairOrder> {
        match self {
            Self::Pair(order) => Some(order),
            _ => None,
        }
    }
}

struct IterationWorkloads<'a> {
    requests: &'a [BenchmarkRequest],
    mode: RunMode,
}

struct RunRegistration {
    store: SessionStore,
    session: SessionId,
    source: ConnectionId,
}
impl Drop for RunRegistration {
    fn drop(&mut self) {
        self.store
            .inner
            .benchmarks
            .remove(&(self.session, self.source));
    }
}

struct OwnedConnection {
    store: SessionStore,
    source: ConnectionId,
    run_id: uuid::Uuid,
    session: SessionId,
    connection: Option<ConnectionId>,
}
impl OwnedConnection {
    fn start_close(&mut self) -> Option<tokio::task::JoinHandle<ApiResult<()>>> {
        let connection = self.connection.take()?;
        let runtime = tokio::runtime::Handle::try_current().ok()?;
        let store = self.store.clone();
        let session = self.session;
        Some(
            runtime
                .spawn(async move { store.close_connection_unchecked(session, connection).await }),
        )
    }
    async fn close(&mut self) -> bool {
        let Some(mut task) = self.start_close() else {
            return false;
        };
        !matches!(
            tokio::time::timeout(Duration::from_secs(5), &mut task).await,
            Ok(Ok(Ok(())))
        )
    }
}
impl Drop for OwnedConnection {
    fn drop(&mut self) {
        self.store.update_performance_progress(
            self.session,
            self.source,
            self.run_id,
            PerformanceRunKind::Benchmark,
            PerformanceStage::CleaningUp,
            None,
        );
        drop(self.start_close());
    }
}

impl SessionStore {
    pub async fn benchmark(
        &self,
        session: SessionId,
        source: ConnectionId,
        request: BenchmarkRequest,
    ) -> ApiResult<BenchmarkReport> {
        validate(&request)?;
        let entry = self.authorize_connection_operation(
            session,
            source,
            sift_protocol::OperationKind::BenchmarkQuery,
            Some(&request.sql),
            &[],
        )?;
        // Benchmark permission does not imply ordinary execution permission.
        self.authorize_connection_operation(
            session,
            source,
            sift_protocol::OperationKind::ExecuteQuery,
            Some(&request.sql),
            &[],
        )?;
        let engine = entry.driver.semantic_engine().ok_or_else(|| {
            ApiError::BadRequest("benchmark requires a supported SQL dialect".into())
        })?;
        validate_read(engine, &request.sql)?;
        let token = CancellationToken::new();
        match self.inner.benchmarks.entry((session, source)) {
            dashmap::mapref::entry::Entry::Occupied(_) => {
                return Err(ApiError::BadRequest(
                    "a benchmark is already running on this connection".into(),
                ))
            }
            dashmap::mapref::entry::Entry::Vacant(slot) => {
                slot.insert(ActiveBenchmark {
                    run_id: request.run_id,
                    progress: Arc::new(performance_progress::ProgressState::benchmark(&request)),
                    cancellation: token.clone(),
                    deadline: tokio::time::Instant::now()
                        + Duration::from_millis(request.total_budget_ms),
                });
            }
        }
        let store = self.clone();
        // The supervisor owns cleanup even if the HTTP consumer disconnects.
        tokio::spawn(async move {
            let _registration = RunRegistration {
                store: store.clone(),
                session,
                source,
            };
            let result = store
                .benchmark_owned(
                    session,
                    source,
                    vec![request],
                    token,
                    engine,
                    RunMode::Single,
                )
                .await;
            result.map(|mut reports| reports.remove(0))
        })
        .await
        .map_err(|_| ApiError::Internal("benchmark task failed".into()))?
    }

    pub async fn benchmark_pair(
        &self,
        session: SessionId,
        source: ConnectionId,
        request: sift_protocol::BenchmarkPairRequest,
    ) -> ApiResult<sift_protocol::BenchmarkPairReport> {
        self.benchmark_pair_with_cancellation(session, source, request, CancellationToken::new())
            .await
    }

    pub(crate) async fn benchmark_pair_with_cancellation(
        &self,
        session: SessionId,
        source: ConnectionId,
        request: sift_protocol::BenchmarkPairRequest,
        token: CancellationToken,
    ) -> ApiResult<sift_protocol::BenchmarkPairReport> {
        request
            .limits
            .validate()
            .map_err(|message| ApiError::BadRequest(message.into()))?;
        if request.run_id.is_nil() || !request.workload_confirmed {
            return Err(ApiError::BadRequest(
                "An exact run identity and explicit confirmation of both workloads are required"
                    .into(),
            ));
        }
        let make = |run_id, workload: sift_protocol::BenchmarkWorkload| BenchmarkRequest {
            run_id,
            sql: workload.sql,
            params: workload.params,
            warmups: request.limits.warmups,
            iterations: request.limits.iterations,
            query_timeout_ms: request.limits.query_timeout_ms,
            total_budget_ms: request.limits.total_budget_ms,
            delay_ms: request.limits.delay_ms,
            workload_confirmed: request.workload_confirmed,
        };
        let requests = vec![
            make(request.run_id, request.baseline.clone()),
            make(uuid::Uuid::new_v4(), request.candidate.clone()),
        ];
        let entry = self.conn_entry(session, source)?;
        let engine = entry.driver.semantic_engine().ok_or_else(|| {
            ApiError::BadRequest("Paired benchmarks require a supported SQL dialect".into())
        })?;
        for request in &requests {
            validate(request)?;
            validate_read(engine, &request.sql)?;
            for kind in [
                sift_protocol::OperationKind::BenchmarkPair,
                sift_protocol::OperationKind::ExecuteQuery,
            ] {
                self.authorize_connection_operation(
                    session,
                    source,
                    kind,
                    Some(&request.sql),
                    &[],
                )?;
            }
        }
        match self.inner.benchmarks.entry((session, source)) {
            dashmap::mapref::entry::Entry::Occupied(_) => {
                return Err(ApiError::BadRequest(
                    "A benchmark is already running on this connection".into(),
                ))
            }
            dashmap::mapref::entry::Entry::Vacant(slot) => {
                slot.insert(ActiveBenchmark {
                    run_id: request.run_id,
                    cancellation: token.clone(),
                    deadline: tokio::time::Instant::now()
                        + Duration::from_millis(request.limits.total_budget_ms),
                    progress: Arc::new(performance_progress::ProgressState::paired(&request)),
                });
            }
        }
        let _cancel = token.clone().drop_guard();
        let store = self.clone();
        tokio::spawn(async move {
            let _registration = RunRegistration {
                store: store.clone(),
                session,
                source,
            };
            let mut reports = store
                .benchmark_owned(
                    session,
                    source,
                    requests,
                    token,
                    engine,
                    RunMode::Pair(request.order),
                )
                .await?;
            let candidate = reports
                .pop()
                .ok_or_else(|| ApiError::Internal("Missing candidate report".into()))?;
            let baseline = reports
                .pop()
                .ok_or_else(|| ApiError::Internal("Missing baseline report".into()))?;
            Ok(sift_protocol::BenchmarkPairReport {
                baseline,
                candidate,
            })
        })
        .await
        .map_err(|_| ApiError::Internal("Paired benchmark supervisor failed".into()))?
    }

    pub async fn benchmark_suite(
        &self,
        session: SessionId,
        source: ConnectionId,
        request: sift_protocol::BenchmarkSuiteRequest,
    ) -> ApiResult<sift_protocol::BenchmarkSuiteReport> {
        self.benchmark_suite_with_cancellation(session, source, request, CancellationToken::new())
            .await
    }

    pub(crate) async fn benchmark_suite_with_cancellation(
        &self,
        session: SessionId,
        source: ConnectionId,
        request: sift_protocol::BenchmarkSuiteRequest,
        token: CancellationToken,
    ) -> ApiResult<sift_protocol::BenchmarkSuiteReport> {
        sift_core::performance_suite::validate(&request)
            .map_err(|message| ApiError::BadRequest(message.into()))?;
        let entry = self.conn_entry(session, source)?;
        let engine = entry
            .driver
            .semantic_engine()
            .ok_or_else(|| ApiError::BadRequest("Suites require a supported SQL dialect".into()))?;
        let mut requests = Vec::new();
        let mut identities = Vec::new();
        for query in &request.queries {
            validate_read(engine, &query.sql)?;
            for kind in [
                sift_protocol::OperationKind::BenchmarkSuite,
                sift_protocol::OperationKind::ExecuteQuery,
            ] {
                self.authorize_connection_operation(session, source, kind, Some(&query.sql), &[])?;
            }
            for case in &query.cases {
                let limits = case.limits.unwrap_or(request.limits);
                identities.push((query.id.clone(), case.id.clone()));
                requests.push(BenchmarkRequest {
                    run_id: if requests.is_empty() {
                        request.run_id
                    } else {
                        uuid::Uuid::new_v4()
                    },
                    sql: query.sql.clone(),
                    params: case.params.clone(),
                    warmups: limits.warmups,
                    iterations: limits.iterations,
                    query_timeout_ms: limits.query_timeout_ms,
                    total_budget_ms: limits.total_budget_ms,
                    delay_ms: limits.delay_ms,
                    workload_confirmed: true,
                });
            }
        }
        match self.inner.benchmarks.entry((session, source)) {
            dashmap::mapref::entry::Entry::Occupied(_) => {
                return Err(ApiError::BadRequest(
                    "A benchmark is already running on this connection".into(),
                ))
            }
            dashmap::mapref::entry::Entry::Vacant(slot) => {
                slot.insert(ActiveBenchmark {
                    run_id: request.run_id,
                    cancellation: token.clone(),
                    deadline: tokio::time::Instant::now()
                        + Duration::from_millis(request.limits.total_budget_ms),
                    progress: Arc::new(performance_progress::ProgressState::suite(&request)),
                });
            }
        }
        let _cancel = token.clone().drop_guard();
        let store = self.clone();
        tokio::spawn(async move {
            let _registration = RunRegistration { store: store.clone(), session, source };
            let mut reports = store.benchmark_owned(session, source, requests, token, engine, RunMode::Suite(request.order)).await?;
            let completed = reports.iter().all(|report| report.completed);
            let count = reports.len() as u16;
            let case_limits = sift_core::performance_suite::effective_limits(&request);
            let mut actual_order = sift_core::performance_suite::schedule_ordered(&case_limits, request.order)
                .map_err(|message| ApiError::BadRequest(message.into()))?;
            actual_order.truncate(reports.iter().map(|report| report.samples.len()).sum());
            for (index, (report, (query_id, case_id))) in reports.iter_mut().zip(identities).enumerate() {
                report.suite_context = Some(Box::new(sift_protocol::BenchmarkSuiteContext {
 order: Some(request.order),
 case_limits: case_limits.clone(), shared_total_budget_ms: Some(request.limits.total_budget_ms),
                    version: 3, run_id: request.run_id, query_id, case_id, case_index: index as u16,
                    case_count: count, actual_order: actual_order.clone(), suite_completed: completed,
                }));
                report.warnings.push("Suite cases share one dedicated session in the explicitly recorded dispatch order; native read-only transactions are used where supported. Earlier cases affect shared cache/session state; data/load equivalence is unproven. Latencies are never pooled. Incomplete suites withhold comparisons.".into());
            }
            Ok(sift_protocol::BenchmarkSuiteReport { run_id: request.run_id, cases: reports, completed })
        }).await.map_err(|_| ApiError::Internal("Suite benchmark supervisor failed".into()))?
    }

    pub fn cancel_benchmark(
        &self,
        session: SessionId,
        connection: ConnectionId,
        run: uuid::Uuid,
    ) -> ApiResult<()> {
        self.authorize_connection_operation(
            session,
            connection,
            sift_protocol::OperationKind::CancelBenchmark,
            None,
            &[],
        )?;
        if let Some(active) = self.inner.benchmarks.get(&(session, connection)) {
            if active.run_id == run {
                active.cancellation.cancel();
                return Ok(());
            }
        }
        Err(ApiError::BadRequest(
            "Benchmark is not active (it may not have started or may already be complete)".into(),
        ))
    }

    async fn benchmark_owned(
        &self,
        session: SessionId,
        source: ConnectionId,
        requests: Vec<BenchmarkRequest>,
        token: CancellationToken,
        engine: Engine,
        mode: RunMode,
    ) -> ApiResult<Vec<BenchmarkReport>> {
        let request = &requests[0];
        let (configuration, credentials, provenance, driver) = self
            .with_session(&session, |s| {
                s.connections.get(&source).map(|entry| {
                    (
                        entry.configuration.clone(),
                        entry.credentials.clone(),
                        entry.provenance.clone(),
                        entry.driver.clone(),
                    )
                })
            })?
            .ok_or(ApiError::ConnectionNotFound(source))?;
        let resource = match &provenance {
            ConnectionProvenance::Managed {
                tenant_id,
                quota_exempt: false,
                ..
            } => Some(self.resource_manager().reserve(
                *tenant_id,
                sift_protocol::TenantResource::Connections,
                1,
            )?),
            _ => None,
        };
        let dedicated = self
            .open_connection_with_provenance(
                session,
                Some(engine),
                configuration,
                credentials,
                provenance,
                resource,
                driver,
            )
            .await?;
        let mut owned = OwnedConnection {
            store: self.clone(),
            source,
            run_id: request.run_id,
            session,
            connection: Some(dedicated.id),
        };
        let environment = benchmark_environment(engine);
        let version_entry = self.conn_entry(session, dedicated.id)?;
        let version_driver = version_entry.driver.clone();
        let version_handle = version_entry.handle.clone();
        let server_version = self
            .run_bounded("benchmark server version", async move {
                version_driver.ping(version_handle).await
            })
            .await
            .ok()
            .and_then(|info| (info.server_version.len() <= 256).then_some(info.server_version));
        let environment = BenchmarkEnvironment {
            server_version,
            ..environment
        };
        let deadline = self
            .inner
            .benchmarks
            .get(&(session, source))
            .map(|run| run.deadline)
            .unwrap_or_else(tokio::time::Instant::now);
        let settings = self
            .performance_settings(session, dedicated.id, engine, &token, deadline)
            .await;
        let result = match settings {
            Ok(observed_settings) => {
                let environment = BenchmarkEnvironment {
                    session_settings: if observed_settings.is_some() {
                        "allowlist_observed_before_workload".into()
                    } else {
                        environment.session_settings.clone()
                    },
                    observed_settings,
                    ..environment
                };
                self.benchmark_iterations(
                    session,
                    source,
                    dedicated.id,
                    IterationWorkloads {
                        requests: &requests,
                        mode,
                    },
                    token,
                    environment,
                )
                .await
            }
            Err(_) if token.is_cancelled() || tokio::time::Instant::now() >= deadline => {
                let mut reports = requests
                    .iter()
                    .map(|request| {
                        report(
                            request,
                            engine,
                            Vec::new(),
                            vec![
                                "Stopped during session settings setup; workload was not started"
                                    .into(),
                            ],
                            environment.clone(),
                        )
                    })
                    .collect::<Vec<_>>();
                set_pair_context(&mut reports, mode.pair_order(), Vec::new());
                Ok(reports)
            }
            Err(error) => Err(error),
        };
        // Never alter the editor's connection or transaction. Closing the owned
        // connection also discards any open read transaction after cancellation.
        self.update_performance_progress(
            session,
            source,
            request.run_id,
            PerformanceRunKind::Benchmark,
            PerformanceStage::CleaningUp,
            None,
        );
        let cleanup_pending = owned.close().await;
        let mut reports = result?;
        if cleanup_pending {
            for report in &mut reports {
                report.warnings.push("Dedicated connection cleanup remains pending or failed; native cleanup retains ownership beyond observation".into());
            }
        }
        Ok(reports)
    }

    async fn benchmark_iterations(
        &self,
        session: SessionId,
        source: ConnectionId,
        connection: ConnectionId,
        workloads: IterationWorkloads<'_>,
        token: CancellationToken,
        environment: BenchmarkEnvironment,
    ) -> ApiResult<Vec<BenchmarkReport>> {
        let IterationWorkloads { requests, mode } = workloads;
        let root = &requests[0];
        let entry = self.conn_entry(session, connection)?;
        let engine = entry.driver.semantic_engine().ok_or_else(|| {
            ApiError::BadRequest("benchmark requires a supported SQL dialect".into())
        })?;
        let mut warnings = vec![
            "Client elapsed, first row and full consumption are server-observed; native database execution time is unavailable; desktop network and rendering are excluded".into(),
            "Dedicated reused connection; driver-default preparation; cache state and concurrent database load are unknown".into(),
            "Uses connection-profile defaults, not the editor's session overrides or temporary tables; setup and cleanup can outlast the sampling budget".into(),
            "Read-only checks are not a sandbox for external function side effects".into(),
        ];
        let transaction = if engine != Engine::SqlServer {
            let driver = entry.driver.clone();
            let handle = entry.handle.clone();
            Some(
                self.run_bounded("benchmark read transaction", async move {
                    driver
                        .begin(
                            handle,
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
                })
                .await?,
            )
        } else {
            warnings.push("SQL Server has no read-only transaction mode: use a read-only database account; SQL is restricted to a single read query".into());
            None
        };
        let deadline = self
            .inner
            .benchmarks
            .get(&(session, source))
            .map(|run| run.value().deadline)
            .unwrap_or_else(tokio::time::Instant::now);
        let schedule: Vec<u16> = match mode {
            RunMode::Pair(order) => sift_core::performance_pair::schedule(
                &sift_protocol::BenchmarkLimits {
                    warmups: root.warmups,
                    iterations: root.iterations,
                    query_timeout_ms: root.query_timeout_ms,
                    total_budget_ms: root.total_budget_ms,
                    delay_ms: root.delay_ms,
                },
                order,
            )
            .map_err(|message| ApiError::BadRequest(message.into()))?
            .into_iter()
            .map(|p| u16::from(p == sift_protocol::BenchmarkPopulation::Candidate))
            .collect(),
            RunMode::Suite(order) => sift_core::performance_suite::schedule_ordered(
                &requests
                    .iter()
                    .map(|request| sift_protocol::BenchmarkLimits {
                        warmups: request.warmups,
                        iterations: request.iterations,
                        query_timeout_ms: request.query_timeout_ms,
                        total_budget_ms: request.total_budget_ms,
                        delay_ms: request.delay_ms,
                    })
                    .collect::<Vec<_>>(),
                order,
            )
            .map_err(|message| ApiError::BadRequest(message.into()))?,
            RunMode::Single => vec![0; (root.warmups + root.iterations) as usize],
        };
        let total = schedule.len();
        let mut actual_order = Vec::new();
        let mut populations = vec![Vec::new(); requests.len()];
        let mut case_deadlines = vec![None; requests.len()];
        for (position, population) in schedule.iter().copied().enumerate() {
            let index = usize::from(population);
            let request = &requests[index];
            let ordinal = populations[index].len() as u32;
            let case_deadline = if matches!(mode, RunMode::Suite(_)) {
                *case_deadlines[index].get_or_insert_with(|| {
                    deadline.min(
                        tokio::time::Instant::now()
                            + Duration::from_millis(request.total_budget_ms),
                    )
                })
            } else {
                deadline
            };
            if token.is_cancelled() || tokio::time::Instant::now() >= case_deadline {
                warnings.push(if matches!(mode, RunMode::Suite(_)) {
                    "Stopped: cancellation, shared deadline or current case budget exhausted; remaining scheduled attempts are not executed"
                } else { "Stopped: cancellation or run deadline reached" }.into());
                break;
            }
            // Re-check policy and source existence between every iteration.
            if self
                .authorize_connection_operation(
                    session,
                    source,
                    mode.operation(),
                    Some(&request.sql),
                    &[],
                )
                .is_err()
                || self
                    .authorize_connection_operation(
                        session,
                        source,
                        sift_protocol::OperationKind::ExecuteQuery,
                        Some(&request.sql),
                        &[],
                    )
                    .is_err()
            {
                warnings.push("Stopped: connection access changed".into());
                break;
            }
            let resources = match self.reserve_query_resources(&entry) {
                Ok(resources) => resources,
                Err(_) => {
                    warnings.push("Stopped: query resource limit reached".into());
                    break;
                }
            };
            let driver = entry.driver.clone();
            let handle = entry.handle.clone();
            let sql = request.sql.clone();
            let params = request.params.clone();
            let stage = if ordinal < request.warmups {
                PerformanceStage::WarmingUp
            } else {
                PerformanceStage::Measuring
            };
            self.update_performance_progress(
                session,
                source,
                root.run_id,
                PerformanceRunKind::Benchmark,
                stage,
                None,
            );
            actual_order.push(population);
            let started = std::time::Instant::now();
            let cursor = Arc::new(Mutex::new(None));
            let cursor_slot = cursor.clone();
            let mut task = tokio::spawn(async move {
                let _resources = resources;
                let mut stream = driver
                    .execute(
                        handle,
                        ExecuteRequest {
                            sql,
                            params,
                            transform: None,
                        },
                    )
                    .await?;
                *cursor_slot.lock().unwrap() = Some(stream.cursor_id);
                let mut count = 0_u64;
                let mut first = None;
                while let Some(page) = stream.rows.recv().await {
                    match page {
                        sift_protocol::Page::Rows { rows } => {
                            if !rows.is_empty() && first.is_none() {
                                first = Some(nanos(started.elapsed()));
                            }
                            count = count.saturating_add(rows.len() as u64);
                        }
                        sift_protocol::Page::Error { error } => return Err(error),
                        sift_protocol::Page::Done { .. } => {
                            return Ok((count, first, nanos(started.elapsed())))
                        }
                        sift_protocol::Page::NextResult { .. } => {}
                    }
                }
                Err(DriverError::new(
                    Code::DriverInternal,
                    "benchmark stream ended without completion",
                ))
            });
            let iteration_deadline = case_deadline
                .min(tokio::time::Instant::now() + Duration::from_millis(request.query_timeout_ms));
            let (outcome, rows, first_row_ns, full_consumption_ns) = tokio::select! {
                biased;
                _ = token.cancelled() => (BenchmarkOutcome::Cancelled, 0, None, None),
                _ = tokio::time::sleep_until(iteration_deadline) => (BenchmarkOutcome::TimedOut, 0, None, None),
                result = &mut task => match result {
                    Ok(Ok((rows, first, consumed))) => (BenchmarkOutcome::Success, rows, first, Some(consumed)),
                    _ => (BenchmarkOutcome::Failed, 0, None, None),
                }
            };
            let client_elapsed_ns = nanos(started.elapsed());
            populations[index].push(BenchmarkSample {
                ordinal,
                warmup: ordinal < request.warmups,
                outcome,
                elapsed_ns: client_elapsed_ns,
                database_execution_ns: None,
                client_elapsed_ns: Some(client_elapsed_ns),
                first_row_ns,
                full_consumption_ns,
                rows: (outcome == BenchmarkOutcome::Success).then_some(rows),
            });
            self.update_performance_progress(
                session,
                source,
                root.run_id,
                PerformanceRunKind::Benchmark,
                stage,
                Some((ordinal < request.warmups, outcome)),
            );
            if outcome != BenchmarkOutcome::Success {
                task.abort();
                let cursor_id = *cursor.lock().unwrap();
                if let Some(cursor_id) = cursor_id {
                    let _ = tokio::time::timeout(
                        Duration::from_secs(5),
                        entry.driver.cancel(entry.handle.clone(), cursor_id),
                    )
                    .await;
                }
                warnings.push("Stopped after an unsuccessful iteration; partial timing is not included in latency statistics".into());
                break;
            }
            if request.delay_ms > 0 && position + 1 < total {
                self.update_performance_progress(
                    session,
                    source,
                    root.run_id,
                    PerformanceRunKind::Benchmark,
                    PerformanceStage::Delay,
                    None,
                );
                tokio::select! {
                    _ = token.cancelled() => break,
                    _ = tokio::time::sleep_until(
                        (if schedule[position + 1] == population { case_deadline } else { deadline })
                            .min(tokio::time::Instant::now() + Duration::from_millis(request.delay_ms))) => {}
                }
            }
        }
        self.update_performance_progress(
            session,
            source,
            root.run_id,
            PerformanceRunKind::Benchmark,
            PerformanceStage::CleaningUp,
            None,
        );
        if let Some(transaction) = transaction {
            let driver = entry.driver.clone();
            // Cleanup is bounded separately and its failure must not erase samples.
            if !matches!(
                tokio::time::timeout(Duration::from_secs(5), driver.rollback(transaction)).await,
                Ok(Ok(()))
            ) {
                warnings.push("Read transaction rollback did not complete cleanly; connection will be discarded".into());
            }
        }
        let mut reports = requests
            .iter()
            .enumerate()
            .map(|(index, request)| {
                report(
                    request,
                    engine,
                    std::mem::take(&mut populations[index]),
                    warnings.clone(),
                    environment.clone(),
                )
            })
            .collect::<Vec<_>>();
        set_pair_context(
            &mut reports,
            mode.pair_order(),
            actual_order
                .into_iter()
                .map(|index| {
                    if index == 0 {
                        sift_protocol::BenchmarkPopulation::Baseline
                    } else {
                        sift_protocol::BenchmarkPopulation::Candidate
                    }
                })
                .collect(),
        );
        Ok(reports)
    }
}

fn set_pair_context(
    reports: &mut [BenchmarkReport],
    order: Option<sift_protocol::BenchmarkPairOrder>,
    actual_order: Vec<sift_protocol::BenchmarkPopulation>,
) {
    let Some(order) = order else {
        return;
    };
    let run_id = reports[0].run_id;
    let pair_completed = reports.len() == 2 && reports.iter().all(|report| report.completed);
    for (index, report) in reports.iter_mut().enumerate() {
        report.pair_context = Some(Box::new(sift_protocol::BenchmarkPairContext {
            version: 1,
            run_id,
            population: if index == 0 {
                sift_protocol::BenchmarkPopulation::Baseline
            } else {
                sift_protocol::BenchmarkPopulation::Candidate
            },
            order,
            actual_order: actual_order.clone(),
            pair_completed,
        }));
        report.warnings.push("Controlled A/B populations share a dedicated session and transaction. Ordering, shared cache/session state and concurrent data/load changes can affect observations; each latency population remains separate and Profile clocks are excluded.".into());
    }
}

fn nanos(duration: Duration) -> u64 {
    duration.as_nanos().min(u64::MAX as u128) as u64
}

fn validate(request: &BenchmarkRequest) -> ApiResult<()> {
    sift_core::performance::BenchmarkLimits {
        warmups: request.warmups,
        iterations: request.iterations,
        query_timeout_ms: request.query_timeout_ms,
        total_budget_ms: request.total_budget_ms,
        delay_ms: request.delay_ms,
    }
    .validate()
    .map_err(|error| ApiError::BadRequest(error.into()))?;
    if !request.workload_confirmed {
        return Err(ApiError::BadRequest(
            "Confirm the complete repeated workload before benchmarking".into(),
        ));
    }
    if request.sql.len() > 1024 * 1024 {
        return Err(ApiError::BadRequest("Benchmark SQL exceeds 1 MiB".into()));
    }
    Ok(())
}

pub(crate) fn validate_read(engine: Engine, sql: &str) -> ApiResult<()> {
    let normalized;
    let sql = if engine == Engine::Sqlite {
        normalized = sift_core::sqlite_parameters::layout(sql, 32766)
            .map_err(|message| ApiError::BadRequest(message.into()))?
            .normalized_sql;
        normalized.as_str()
    } else {
        sql
    };
    let dialect: Box<dyn sqlparser::dialect::Dialect> = match engine {
        Engine::Postgres => Box::new(sqlparser::dialect::PostgreSqlDialect {}),
        Engine::SqlServer => Box::new(sqlparser::dialect::MsSqlDialect {}),
        Engine::Sqlite => Box::new(sqlparser::dialect::SQLiteDialect {}),
    };
    let statements = sqlparser::parser::Parser::parse_sql(dialect.as_ref(), sql).map_err(|_| {
        ApiError::BadRequest("Benchmark requires a classifiable single read query".into())
    })?;
    if statements.len() != 1 || !matches!(statements[0], sqlparser::ast::Statement::Query(_)) {
        return Err(ApiError::BadRequest(
            "Benchmark requires exactly one read query".into(),
        ));
    }
    crate::sql_policy::enforce(
        &sift_protocol::ConnectionPolicy {
            read_only: true,
            ..Default::default()
        },
        Some(engine),
        sift_protocol::OperationKind::BenchmarkQuery,
        Some(sql),
        &[],
    )
}

fn report(
    request: &BenchmarkRequest,
    engine: Engine,
    samples: Vec<BenchmarkSample>,
    warnings: Vec<String>,
    environment: BenchmarkEnvironment,
) -> BenchmarkReport {
    use sift_core::performance::{summarize, SampleOutcome, SamplePhase, TimingSample};
    let timings: Vec<_> = samples
        .iter()
        .map(|s| TimingSample {
            phase: if s.warmup {
                SamplePhase::Warmup
            } else {
                SamplePhase::Measured
            },
            outcome: match s.outcome {
                BenchmarkOutcome::Success => SampleOutcome::Success,
                BenchmarkOutcome::Failed => SampleOutcome::Failed,
                BenchmarkOutcome::TimedOut => SampleOutcome::TimedOut,
                BenchmarkOutcome::Cancelled => SampleOutcome::Cancelled,
            },
            elapsed_ns: Some(s.elapsed_ns),
        })
        .collect();
    let distribution = summarize(&timings).distribution;
    BenchmarkReport {
        version: 2,
        run_id: request.run_id,
        engine,
        sql: request.sql.clone(),
        captured_at: chrono::Utc::now(),
        warmups: request.warmups,
        requested_iterations: request.iterations,
        query_timeout_ms: request.query_timeout_ms,
        total_budget_ms: request.total_budget_ms,
        delay_ms: request.delay_ms,
        parameter_count: request.params.len(),
        pair_context: None,
        suite_context: None,
        environment: Some(environment),
        completed: samples.len() == (request.warmups + request.iterations) as usize
            && samples
                .iter()
                .all(|s| s.outcome == BenchmarkOutcome::Success),
        samples,
        warnings,
        median_ns: distribution.as_ref().map(|d| d.median_ns),
        mean_ns: distribution.as_ref().map(|d| d.mean_ns),
        min_ns: distribution.as_ref().map(|d| d.min_ns),
        max_ns: distribution.as_ref().map(|d| d.max_ns),
        standard_deviation_ns: distribution.as_ref().and_then(|d| d.standard_deviation_ns),
        p95_ns: distribution.as_ref().and_then(|d| d.p95_ns),
        p99_ns: distribution.as_ref().and_then(|d| d.p99_ns),
    }
}

fn benchmark_environment(engine: Engine) -> BenchmarkEnvironment {
    BenchmarkEnvironment {
        server_version: None,
        preparation: "driver_default".into(),
        connection_reuse: "single_dedicated_connection".into(),
        isolation: match engine {
            Engine::Postgres => "read_only_read_committed",
            Engine::Sqlite => "read_only_serializable",
            Engine::SqlServer => "driver_default_no_read_only_transaction",
        }
        .into(),
        session_settings: "connection_profile_defaults_uninspected".into(),
        observed_settings: None,
        cache_state: "unknown".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelling_a_hung_run_releases_registration_and_preserves_editor_connection() {
        for mode in 0..3 {
            let paired = mode == 1;
            let suite = mode == 2;
            let driver = sift_driver_api::mock::MockDriver::builder()
                .engine(Engine::Postgres)
                .execute_hang()
                .build();
            let observer = Arc::new(driver);
            let registry = DriverRegistry::builder().build();
            registry
                .providers()
                .replace([Arc::new(crate::registry::BuiltinProviderAdapter::new(
                    observer.clone(),
                ))
                    as Arc<dyn crate::registry::DatabaseProvider>])
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
            let run_id = uuid::Uuid::new_v4();
            let run_store = store.clone();
            let task = tokio::spawn(async move {
                if suite {
                    let report = run_store
                        .benchmark_suite(
                            session,
                            connection,
                            sift_protocol::BenchmarkSuiteRequest {
                                order: sift_protocol::BenchmarkSuiteOrder::RandomizedRoundsV1 {
                                    seed: 0,
                                },
                                run_id,
                                queries: vec![sift_protocol::BenchmarkSuiteQuery {
                                    id: "query".into(),
                                    sql: "SELECT 1".into(),
                                    cases: vec![
                                        sift_protocol::BenchmarkParameterCase {
                                            limits: None,
                                            id: "one".into(),
                                            params: vec![],
                                        },
                                        sift_protocol::BenchmarkParameterCase {
                                            limits: Some(sift_protocol::BenchmarkLimits {
                                                warmups: 2,
                                                iterations: 3,
                                                query_timeout_ms: 500,
                                                total_budget_ms: 1000,
                                                delay_ms: 0,
                                            }),
                                            id: "two".into(),
                                            params: vec![],
                                        },
                                    ],
                                }],
                                limits: sift_protocol::BenchmarkLimits {
                                    warmups: 0,
                                    iterations: 10,
                                    query_timeout_ms: 30_000,
                                    total_budget_ms: 60_000,
                                    delay_ms: 0,
                                },
                                workload_confirmed: true,
                            },
                        )
                        .await?;
                    assert!(!report.completed);
                    for case in &report.cases {
                        sift_core::performance_suite::validate_context(case).unwrap();
                    }
                    return Ok(report.cases.into_iter().next().unwrap());
                }
                if paired {
                    let report = run_store
                        .benchmark_pair(
                            session,
                            connection,
                            sift_protocol::BenchmarkPairRequest {
                                run_id,
                                baseline: sift_protocol::BenchmarkWorkload {
                                    sql: "SELECT 1".into(),
                                    params: vec![],
                                },
                                candidate: sift_protocol::BenchmarkWorkload {
                                    sql: "SELECT 2".into(),
                                    params: vec![],
                                },
                                limits: sift_protocol::BenchmarkLimits {
                                    warmups: 0,
                                    iterations: 10,
                                    query_timeout_ms: 30_000,
                                    total_budget_ms: 60_000,
                                    delay_ms: 0,
                                },
                                order: sift_protocol::BenchmarkPairOrder::Alternating,
                                workload_confirmed: true,
                            },
                        )
                        .await?;
                    assert!(!report.candidate.completed);
                    assert!(
                        !report
                            .baseline
                            .pair_context
                            .as_ref()
                            .unwrap()
                            .pair_completed
                    );
                    return Ok(report.baseline);
                }
                run_store
                    .benchmark(
                        session,
                        connection,
                        BenchmarkRequest {
                            run_id,
                            sql: "SELECT 1".into(),
                            params: vec![],
                            warmups: 0,
                            iterations: 10,
                            query_timeout_ms: 30_000,
                            total_budget_ms: 60_000,
                            delay_ms: 0,
                            workload_confirmed: true,
                        },
                    )
                    .await
            });
            tokio::time::timeout(Duration::from_secs(2), async {
                while !store.inner.benchmarks.contains_key(&(session, connection)) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let before = observer.invocations();
            let snapshot = store
                .performance_progress(session, connection, run_id, PerformanceRunKind::Benchmark)
                .unwrap()
                .unwrap();
            assert_eq!(snapshot.run_id, run_id);
            assert_eq!(
                snapshot.benchmark.as_ref().unwrap().requested_iterations,
                if suite {
                    13
                } else if paired {
                    20
                } else {
                    10
                }
            );
            assert_eq!(
                snapshot.benchmark.as_ref().unwrap().requested_warmups,
                if suite { 2 } else { 0 }
            );
            assert!(store
                .cancel_benchmark(session, connection, uuid::Uuid::new_v4())
                .is_err());
            assert_eq!(
                observer.invocations(),
                before,
                "progress must not call the driver"
            );
            store.cancel_benchmark(session, connection, run_id).unwrap();
            let snapshot = store
                .performance_progress(session, connection, run_id, PerformanceRunKind::Benchmark)
                .unwrap()
                .unwrap();
            assert!(snapshot.cancellation_requested);

            let report = tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(!report.completed);
            assert!(report.median_ns.is_none());
            let environment = report.environment.unwrap();
            assert_eq!(environment.isolation, "read_only_read_committed");
            assert_eq!(environment.cache_state, "unknown");
            assert!(environment.server_version.is_none());
            assert!(!store.inner.benchmarks.contains_key(&(session, connection)));
            assert!(store
                .performance_progress(session, connection, run_id, PerformanceRunKind::Benchmark)
                .unwrap()
                .is_none());
            let remaining = store.list_connections(session).unwrap();
            assert_eq!(remaining.len(), 1);
            assert_eq!(remaining[0].id, connection);
        }
    }
    #[tokio::test(start_paused = true)]
    async fn cleanup_observer_retains_quota_and_closes_once_after_timeout() {
        let closed = Arc::new(tokio::sync::Notify::new());
        let mock = Arc::new(
            sift_driver_api::mock::MockDriver::builder()
                .engine(Engine::Postgres)
                .close_wait(closed.clone())
                .build(),
        );
        let registry = DriverRegistry::builder().build();
        registry
            .providers()
            .replace([
                Arc::new(crate::registry::BuiltinProviderAdapter::new(mock.clone()))
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
        let resources = store.resource_manager();
        let guard = resources
            .reserve(
                sift_metadata::TenantId(1),
                sift_protocol::TenantResource::Connections,
                1,
            )
            .unwrap();
        store
            .with_session(&session, |state| {
                state
                    .connections
                    .get_mut(&connection)
                    .unwrap()
                    ._resource_guard = Some(Arc::new(guard))
            })
            .unwrap();
        let mut owned = OwnedConnection {
            store: store.clone(),
            source: connection,
            run_id: uuid::Uuid::new_v4(),
            session,
            connection: Some(connection),
        };
        assert!(
            owned.close().await,
            "observer reports pending native cleanup"
        );
        assert_eq!(
            resources
                .snapshot(sift_metadata::TenantId(1))
                .unwrap()
                .usage
                .connections,
            1,
            "native cleanup retains the connection reservation"
        );
        drop(owned);
        assert_eq!(
            mock.invocations()
                .iter()
                .filter(|&&name| name == "close")
                .count(),
            1
        );
        closed.notify_one();
        tokio::time::timeout(Duration::from_secs(2), async {
            while resources
                .snapshot(sift_metadata::TenantId(1))
                .unwrap()
                .usage
                .connections
                != 0
            {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
    }

    #[test]
    fn benchmarks_reject_batches_and_hidden_writes() {
        for engine in [Engine::Postgres, Engine::SqlServer, Engine::Sqlite] {
            assert!(validate_read(engine, "SELECT 1").is_ok());
            for sql in [
                "SELECT 1; SELECT 2",
                "DELETE FROM t",
                "SELECT * INTO copied FROM t",
                "COMMIT",
            ] {
                assert!(validate_read(engine, sql).is_err(), "{engine:?}: {sql}");
            }
        }
        assert!(validate_read(
            Engine::Postgres,
            "WITH deleted AS (DELETE FROM t RETURNING *) SELECT * FROM deleted"
        )
        .is_err());
    }
}
