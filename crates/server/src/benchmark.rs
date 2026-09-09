//! Session-owned serial benchmarks. Rows are consumed and dropped, never added
//! to the result cache. One run per source connection; each run owns its connection.
use super::*;
use sift_protocol::{BenchmarkOutcome, BenchmarkReport, BenchmarkRequest, BenchmarkSample};
use tokio_util::sync::CancellationToken;

pub(super) struct ActiveBenchmark {
    pub(super) run_id: uuid::Uuid,
    pub(super) cancellation: CancellationToken,
    pub(super) deadline: tokio::time::Instant,
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
    session: SessionId,
    connection: ConnectionId,
}
impl Drop for OwnedConnection {
    fn drop(&mut self) {
        let store = self.store.clone();
        let session = self.session;
        let connection = self.connection;
        // Also clean up if the supervisor panics. Normal completion closes first.
        tokio::spawn(async move {
            let _ = tokio::time::timeout(
                Duration::from_secs(5),
                store.close_connection_unchecked(session, connection),
            )
            .await;
        });
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
                .benchmark_owned(session, source, request, token, engine)
                .await;
            result
        })
        .await
        .map_err(|_| ApiError::Internal("benchmark task failed".into()))?
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
        request: BenchmarkRequest,
        token: CancellationToken,
        engine: Engine,
    ) -> ApiResult<BenchmarkReport> {
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
        let _owned = OwnedConnection {
            store: self.clone(),
            session,
            connection: dedicated.id,
        };
        let result = self
            .benchmark_iterations(session, source, dedicated.id, &request, token, engine)
            .await;
        // Never alter the editor's connection or transaction. Closing the owned
        // connection also discards any open read transaction after cancellation.
        let cleanup = tokio::time::timeout(
            Duration::from_secs(5),
            self.close_connection_unchecked(session, dedicated.id),
        )
        .await;
        let mut report = result?;
        if !matches!(cleanup, Ok(Ok(()))) {
            report
                .warnings
                .push("Dedicated connection cleanup did not complete cleanly".into());
        }
        Ok(report)
    }

    async fn benchmark_iterations(
        &self,
        session: SessionId,
        source: ConnectionId,
        connection: ConnectionId,
        request: &BenchmarkRequest,
        token: CancellationToken,
        engine: Engine,
    ) -> ApiResult<BenchmarkReport> {
        let entry = self.conn_entry(session, connection)?;
        let mut warnings = vec![
            "Timing is server-observed execution plus full result consumption; excludes desktop network and rendering".into(),
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
                                ..Default::default()
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
        let mut samples = Vec::new();
        for ordinal in 0..request.warmups + request.iterations {
            if token.is_cancelled() || tokio::time::Instant::now() >= deadline {
                break;
            }
            // Re-check policy and source existence between every iteration.
            if self
                .authorize_connection_operation(
                    session,
                    source,
                    sift_protocol::OperationKind::BenchmarkQuery,
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
                        sift_protocol::Page::Done { .. } => return Ok((count, first)),
                        sift_protocol::Page::NextResult { .. } => {}
                    }
                }
                Err(DriverError::new(
                    Code::DriverInternal,
                    "benchmark stream ended without completion",
                ))
            });
            let iteration_deadline = deadline
                .min(tokio::time::Instant::now() + Duration::from_millis(request.query_timeout_ms));
            let (outcome, rows, first_row_ns) = tokio::select! {
                biased;
                _ = token.cancelled() => (BenchmarkOutcome::Cancelled, 0, None),
                _ = tokio::time::sleep_until(iteration_deadline) => (BenchmarkOutcome::TimedOut, 0, None),
                result = &mut task => match result {
                    Ok(Ok((rows, first))) => (BenchmarkOutcome::Success, rows, first),
                    _ => (BenchmarkOutcome::Failed, 0, None),
                }
            };
            samples.push(BenchmarkSample {
                ordinal,
                warmup: ordinal < request.warmups,
                outcome,
                elapsed_ns: nanos(started.elapsed()),
                first_row_ns,
                rows: (outcome == BenchmarkOutcome::Success).then_some(rows),
            });
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
            if request.delay_ms > 0 && ordinal + 1 < request.warmups + request.iterations {
                tokio::select! {
                    _ = token.cancelled() => break,
                    _ = tokio::time::sleep_until(deadline.min(tokio::time::Instant::now() + Duration::from_millis(request.delay_ms))) => {}
                }
            }
        }
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
        Ok(report(request, engine, samples, warnings))
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

fn validate_read(engine: Engine, sql: &str) -> ApiResult<()> {
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
        version: 1,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelling_a_hung_run_releases_registration_and_preserves_editor_connection() {
        let driver = sift_driver_api::mock::MockDriver::builder()
            .engine(Engine::Postgres)
            .execute_hang()
            .build();
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
        store.cancel_benchmark(session, connection, run_id).unwrap();
        let report = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(!report.completed);
        assert!(report.median_ns.is_none());
        assert!(!store.inner.benchmarks.contains_key(&(session, connection)));
        let remaining = store.list_connections(session).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, connection);
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
