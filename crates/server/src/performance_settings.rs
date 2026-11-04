//! Bounded setup observations; never execute on the editor connection.
use super::*;
use sift_protocol::{SessionSettingsObservation, Value};
use tokio_util::sync::CancellationToken;

fn projection(engine: Engine) -> (&'static str, &'static [&'static str]) {
    match engine {
        Engine::Postgres => (
            "SELECT pg_catalog.current_setting('transaction_isolation', true), pg_catalog.current_setting('transaction_read_only', true), pg_catalog.current_setting('statement_timeout', true), pg_catalog.current_setting('lock_timeout', true), pg_catalog.current_setting('work_mem', true), pg_catalog.current_setting('jit', true), pg_catalog.current_setting('plan_cache_mode', true), pg_catalog.current_setting('max_parallel_workers_per_gather', true), pg_catalog.current_setting('enable_seqscan', true)",
            &["transaction_isolation", "transaction_read_only", "statement_timeout", "lock_timeout", "work_mem", "jit", "plan_cache_mode", "max_parallel_workers_per_gather", "enable_seqscan"],
        ),
        Engine::SqlServer => (
            "SELECT transaction_isolation_level, ansi_nulls, ansi_warnings, ansi_padding, arithabort, concat_null_yields_null, quoted_identifier, date_first, lock_timeout FROM sys.dm_exec_sessions WHERE session_id = @@SPID",
            &["transaction_isolation_level", "ansi_nulls", "ansi_warnings", "ansi_padding", "arithabort", "concat_null_yields_null", "quoted_identifier", "date_first", "lock_timeout"],
        ),
        Engine::Sqlite => (
            "SELECT (SELECT query_only FROM pragma_query_only()), (SELECT read_uncommitted FROM pragma_read_uncommitted()), (SELECT timeout FROM pragma_busy_timeout()), (SELECT cache_size FROM pragma_cache_size()), (SELECT synchronous FROM pragma_synchronous()), (SELECT temp_store FROM pragma_temp_store()), (SELECT journal_mode FROM pragma_journal_mode()), (SELECT foreign_keys FROM pragma_foreign_keys()), (SELECT automatic_index FROM pragma_automatic_index())",
            &["query_only", "read_uncommitted", "busy_timeout", "cache_size", "synchronous", "temp_store", "journal_mode", "foreign_keys", "automatic_index"],
        ),
    }
}

fn decode(engine: Engine, cells: &[Value]) -> Option<SessionSettingsObservation> {
    let (_, names) = projection(engine);
    if cells.len() != names.len() {
        return None;
    }
    let values = names
        .iter()
        .zip(cells)
        .map(|(name, value)| {
            let text = match value {
                Value::Text(text) if text.len() <= 128 => text.clone(),
                Value::Bool(value) => value.to_string(),
                Value::Int16(value) => value.to_string(),
                Value::Int32(value) => value.to_string(),
                Value::Int64(value) => value.to_string(),
                _ => return None,
            };
            Some((name.to_string(), text))
        })
        .collect::<Option<std::collections::BTreeMap<_, _>>>()?;
    let observation = SessionSettingsObservation {
        phase: "dedicated_connection_before_workload".into(),
        values,
    };
    observation.is_bounded().then_some(observation)
}

impl SessionStore {
    pub(super) async fn performance_settings(
        &self,
        session: SessionId,
        connection: ConnectionId,
        engine: Engine,
        cancellation: &CancellationToken,
        deadline: tokio::time::Instant,
    ) -> ApiResult<Option<SessionSettingsObservation>> {
        let entry = self.conn_entry(session, connection)?;
        let resources = self.reserve_query_resources(&entry)?;
        let driver = entry.driver.clone();
        let handle = entry.handle.clone();
        let cursor = Arc::new(Mutex::new(None));
        let slot = cursor.clone();
        let mut task = tokio::spawn(async move {
            let _resources = resources;
            let mut stream = driver
                .execute(
                    handle,
                    ExecuteRequest {
                        sql: projection(engine).0.into(),
                        params: Vec::new(),
                        transform: None,
                    },
                )
                .await?;
            *slot.lock().unwrap() = Some(stream.cursor_id);
            let mut observation = None;
            let mut saw_row = false;
            let mut saw_header = false;
            while let Some(page) = stream.rows.recv().await {
                match page {
                    Page::Rows { rows } => {
                        if rows.len() > 1 || (saw_row && !rows.is_empty()) {
                            return Err(DriverError::new(
                                Code::ResultTooLarge,
                                "Session settings returned extra rows",
                            ));
                        }
                        if let Some(row) = rows.first() {
                            observation = decode(engine, &row.values);
                            saw_row = true;
                        }
                    }
                    Page::Done { .. } => return Ok(observation),
                    Page::NextResult { columns } => {
                        if saw_header || saw_row || columns.len() != projection(engine).1.len() {
                            return Err(DriverError::new(
                                Code::UnsupportedResultShape,
                                "Session settings returned an unexpected result layout",
                            ));
                        }
                        saw_header = true;
                    }
                    Page::Error { .. } => {
                        return Err(DriverError::new(
                            Code::DriverInternal,
                            "Session settings observation failed",
                        ))
                    }
                }
            }
            Err(DriverError::new(
                Code::DriverInternal,
                "Session settings observation did not complete",
            ))
        });
        let deadline = deadline.min(tokio::time::Instant::now() + Duration::from_secs(5));
        let outcome = tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(ApiError::Driver(DriverError::new(Code::QueryCanceled, "Session settings observation cancelled"))),
            _ = tokio::time::sleep_until(deadline) => Err(ApiError::Driver(DriverError::new(Code::QueryTimedOut, "Session settings observation timed out"))),
            result = &mut task => match result {
                Ok(result) => result.map_err(ApiError::Driver),
                Err(_) => Err(ApiError::Internal("session settings observation task failed".into())),
            }
        };
        if outcome.is_err() {
            task.abort();
            let cursor = *cursor.lock().unwrap();
            if let Some(cursor) = cursor {
                let _ = tokio::time::timeout(
                    Duration::from_secs(5),
                    entry.driver.cancel(entry.handle.clone(), cursor),
                )
                .await;
            }
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn extra_rows_refuse_workload_and_close_only_the_dedicated_connection() {
        let driver = sift_driver_api::mock::MockDriver::builder()
            .engine(Engine::Postgres)
            .execute_ok(vec![Page::Rows {
                rows: vec![
                    sift_protocol::Row::new(vec![]),
                    sift_protocol::Row::new(vec![]),
                ],
            }])
            .build();
        let store = SessionStore::new(DriverRegistry::builder().register(driver).build());
        let session = store
            .open_session(OpenSessionRequest {
                tag: None,
                tenant_id: None,
            })
            .id;
        let source = store
            .open_connection(
                session,
                Engine::Postgres,
                ConnectionSpec {
                    host: "mock.invalid".into(),
                    port: None,
                    database: None,
                    user: "fixture".into(),
                    password: None,
                    ssl_mode: None,
                    engine_specific: None,
                },
            )
            .await
            .unwrap()
            .id;
        let result = store
            .benchmark(
                session,
                source,
                sift_protocol::BenchmarkRequest {
                    run_id: uuid::Uuid::new_v4(),
                    sql: "SELECT 1".into(),
                    params: vec![],
                    warmups: 0,
                    iterations: 1,
                    query_timeout_ms: 100,
                    total_budget_ms: 1000,
                    delay_ms: 0,
                    workload_confirmed: true,
                },
            )
            .await;
        assert!(matches!(
            result,
            Err(ApiError::Driver(DriverError {
                code: Code::ResultTooLarge,
                ..
            }))
        ));
        let connections = store.list_connections(session).unwrap();
        assert_eq!(connections.len(), 1);
        assert_eq!(connections[0].id, source);
    }

    #[test]
    fn fixed_projection_rejects_missing_oversized_controls_and_native_values() {
        let mut values = vec![Value::Int64(1); projection(Engine::Sqlite).1.len()];
        values[6] = Value::Text("wal".into());
        let captured = decode(Engine::Sqlite, &values).unwrap();
        assert_eq!(captured.values["journal_mode"], "wal");
        assert_eq!(captured.phase, "dedicated_connection_before_workload");
        assert!(decode(Engine::Sqlite, &values[..8]).is_none());
        for invalid in [
            Value::Null,
            Value::Text("x".repeat(129)),
            Value::Text("bad\nvalue".into()),
            Value::Blob(vec![0; 2]),
        ] {
            values[0] = invalid;
            assert!(decode(Engine::Sqlite, &values).is_none());
        }
        let legacy: sift_protocol::BenchmarkEnvironment = serde_json::from_value(serde_json::json!({"preparation":"driver_default","connection_reuse":"dedicated","isolation":"unknown","session_settings":"uninspected","cache_state":"unknown"})).unwrap();
        assert!(legacy.observed_settings.is_none());
    }
}
