//! Instrumented read plans. The source connection remains untouched; a
//! supervised job owns a dedicated connection and its cleanup.
use super::*;
use sift_protocol::{ProfileEnvironment, ProfileRequest, ProfileResponse, TxAccessMode, TxMode};
use tokio_util::sync::CancellationToken;

const MAX_PROFILE_TIMEOUT_MS: u64 = 120_000;
const MAX_PROFILE_RESULT_ROWS: usize = 10_000;

pub(super) struct ActiveProfile {
    pub(super) run_id: uuid::Uuid,
    pub(super) cancellation: CancellationToken,
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
            .profiles
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
    pub async fn profile(
        &self,
        session: SessionId,
        source: ConnectionId,
        request: ProfileRequest,
    ) -> ApiResult<ProfileResponse> {
        if request.connection != source {
            return Err(ApiError::BadRequest(
                "profile connection does not match path".into(),
            ));
        }
        if request.sql.len() > 1024 * 1024 || request.sql.trim().is_empty() {
            return Err(ApiError::BadRequest(
                "Profile SQL must be nonempty and at most 1 MiB".into(),
            ));
        }
        if !(1..=MAX_PROFILE_TIMEOUT_MS).contains(&request.timeout_ms) {
            return Err(ApiError::BadRequest(
                "Profile timeout must be 1–120000 ms".into(),
            ));
        }
        if !request.workload_confirmed {
            return Err(ApiError::BadRequest(
                "Confirm the complete instrumented workload before profiling".into(),
            ));
        }
        let entry = self.authorize_connection_operation(
            session,
            source,
            sift_protocol::OperationKind::ProfileQuery,
            Some(&request.sql),
            &[],
        )?;
        self.authorize_connection_operation(
            session,
            source,
            sift_protocol::OperationKind::ExecuteQuery,
            Some(&request.sql),
            &[],
        )?;
        let engine = entry.driver.semantic_engine().ok_or_else(|| {
            ApiError::BadRequest("Profile requires a supported SQL dialect".into())
        })?;
        if !matches!(
            engine,
            Engine::Postgres | Engine::SqlServer | Engine::Sqlite
        ) {
            return Err(ApiError::Driver(DriverError::new(
                Code::UnsupportedForEngine,
                "Profile requires a supported SQL engine",
            )));
        }
        benchmark::validate_read(engine, &request.sql)?;
        let cancellation = CancellationToken::new();
        match self.inner.profiles.entry((session, source)) {
            dashmap::mapref::entry::Entry::Occupied(_) => {
                return Err(ApiError::BadRequest(
                    "a profile is already running on this connection".into(),
                ))
            }
            dashmap::mapref::entry::Entry::Vacant(slot) => {
                slot.insert(ActiveProfile {
                    run_id: request.run_id,
                    cancellation: cancellation.clone(),
                });
            }
        }
        let store = self.clone();
        tokio::spawn(async move {
            let _registration = RunRegistration {
                store: store.clone(),
                session,
                source,
            };
            store
                .profile_owned(session, source, request, cancellation, engine)
                .await
        })
        .await
        .map_err(|_| ApiError::Internal("profile supervisor failed".into()))?
    }

    pub fn cancel_profile(
        &self,
        session: SessionId,
        source: ConnectionId,
        run_id: uuid::Uuid,
    ) -> ApiResult<()> {
        self.authorize_connection_operation(
            session,
            source,
            sift_protocol::OperationKind::CancelProfile,
            None,
            &[],
        )?;
        if let Some(run) = self.inner.profiles.get(&(session, source)) {
            if run.run_id == run_id {
                run.cancellation.cancel();
                return Ok(());
            }
        }
        Err(ApiError::BadRequest("Profile is not active".into()))
    }

    async fn profile_owned(
        &self,
        session: SessionId,
        source: ConnectionId,
        request: ProfileRequest,
        cancellation: CancellationToken,
        engine: Engine,
    ) -> ApiResult<ProfileResponse> {
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
        let entry = self.conn_entry(session, dedicated.id)?;
        let version_driver = entry.driver.clone();
        let version_handle = entry.handle.clone();
        let server_version = self
            .run_bounded("profile server version", async move {
                version_driver.ping(version_handle).await
            })
            .await
            .ok()
            .and_then(|info| (info.server_version.len() <= 256).then_some(info.server_version));
        let environment = profile_environment(engine, server_version);
        if engine == Engine::SqlServer {
            let mut response = self
                .profile_owned_mssql(
                    session,
                    source,
                    dedicated.id,
                    &entry,
                    &request,
                    &cancellation,
                )
                .await?;
            response.environment = Some(environment);
            return Ok(response);
        }
        if engine == Engine::Sqlite {
            let mut response = self
                .profile_owned_sqlite(
                    session,
                    source,
                    dedicated.id,
                    &entry,
                    &request,
                    &cancellation,
                )
                .await?;
            response.environment = Some(environment);
            return Ok(response);
        }
        let transaction = {
            let driver = entry.driver.clone();
            let handle = entry.handle.clone();
            self.run_bounded("profile read transaction", async move {
                driver
                    .begin(
                        handle,
                        TxMode {
                            access: TxAccessMode::ReadOnly,
                            ..Default::default()
                        },
                    )
                    .await
            })
            .await?
        };
        let timeout_setup = {
            let driver = entry.driver.clone();
            let handle = entry.handle.clone();
            let timeout_ms = request.timeout_ms;
            self.run_bounded("profile statement timeout", async move {
                let mut stream = driver
                    .execute(
                        handle,
                        ExecuteRequest {
                            sql: format!("SET LOCAL statement_timeout = '{timeout_ms}ms'"),
                            params: Vec::new(),
                            transform: None,
                        },
                    )
                    .await?;
                while let Some(page) = stream.rows.recv().await {
                    match page {
                        Page::Done { .. } => return Ok(()),
                        Page::Error { error } => return Err(error),
                        _ => {}
                    }
                }
                Err(DriverError::new(
                    Code::DriverInternal,
                    "Profile timeout setup did not complete",
                ))
            })
            .await
        };
        if let Err(error) = timeout_setup {
            let _ = tokio::time::timeout(
                Duration::from_secs(5),
                self.close_connection_unchecked(session, dedicated.id),
            )
            .await;
            return Err(error);
        }
        let result = self
            .profile_plan(session, source, &entry, &request, &cancellation)
            .await;
        let rollback =
            tokio::time::timeout(Duration::from_secs(5), entry.driver.rollback(transaction)).await;
        let cleanup = tokio::time::timeout(
            Duration::from_secs(5),
            self.close_connection_unchecked(session, dedicated.id),
        )
        .await;
        let mut response = result?;
        response.environment = Some(environment);
        if !matches!(rollback, Ok(Ok(()))) {
            response.warnings.push(
                "Read transaction rollback did not complete cleanly; connection was discarded"
                    .into(),
            );
        }
        if !matches!(cleanup, Ok(Ok(()))) {
            response
                .warnings
                .push("Dedicated connection cleanup did not complete cleanly".into());
        }
        Ok(response)
    }

    async fn profile_owned_sqlite(
        &self,
        session: SessionId,
        source: ConnectionId,
        dedicated: ConnectionId,
        entry: &ConnectionEntryClone,
        request: &ProfileRequest,
        cancellation: &CancellationToken,
    ) -> ApiResult<ProfileResponse> {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(request.timeout_ms);
        let explain_request = sift_protocol::ExplainRequest {
            connection: dedicated,
            sql: request.sql.clone(),
            params: request.params.clone(),
            analyze: false,
        };
        let estimated = crate::plan::explain_as(
            self,
            session,
            dedicated,
            &explain_request,
            sift_protocol::OperationKind::ProfileQuery,
        );
        let plan = tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(ApiError::Driver(DriverError::new(Code::QueryCanceled, "Profile cancelled"))),
            _ = tokio::time::sleep_until(deadline) => Err(ApiError::Driver(DriverError::new(Code::QueryTimedOut, "Profile timed out"))),
            result = estimated => result,
        }?;
        // The SQLite plan is an estimate. Execute separately on this owned
        // connection so actual rows/timing cannot be attributed to plan nodes.
        let driver = entry.driver.clone();
        let handle = entry.handle.clone();
        let transaction = tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(ApiError::Driver(DriverError::new(Code::QueryCanceled, "Profile cancelled"))),
            _ = tokio::time::sleep_until(deadline) => Err(ApiError::Driver(DriverError::new(Code::QueryTimedOut, "Profile timed out"))),
            result = self.run_bounded("SQLite profile read transaction", async move {
                driver.begin(handle, TxMode {
                    access: TxAccessMode::ReadOnly,
                    isolation: sift_protocol::IsolationLevel::Serializable,
                }).await
            }) => result,
        }?;
        let outcome = self
            .profile_sqlite_read(session, source, entry, request, cancellation, deadline)
            .await;
        let rollback =
            tokio::time::timeout(Duration::from_secs(5), entry.driver.rollback(transaction)).await;
        let cleanup = tokio::time::timeout(
            Duration::from_secs(5),
            self.close_connection_unchecked(session, dedicated),
        )
        .await;
        let (rows_returned, server_elapsed_ns) = outcome?;
        let mut warnings = vec![
            "SQLite plan is estimated and captured before the measured read; concurrent schema changes can alter the executed plan. Sift-observed elapsed includes driver dispatch and full result consumption, not native execution time. No per-node actual rows or runtime counters are available".into(),
            "Dedicated read-only transaction; cache state and external function side effects are unknown".into(),
        ];
        if !matches!(rollback, Ok(Ok(()))) {
            warnings.push(
                "Read transaction rollback did not complete cleanly; connection was discarded"
                    .into(),
            );
        }
        if !matches!(cleanup, Ok(Ok(()))) {
            warnings.push("Dedicated connection cleanup did not complete cleanly".into());
        }
        Ok(ProfileResponse {
            run_id: request.run_id,
            plan,
            server_elapsed_ns,
            rows_returned: Some(rows_returned),
            planning_ms: None,
            execution_ms: None,
            environment: None,
            warnings,
        })
    }

    async fn profile_sqlite_read(
        &self,
        session: SessionId,
        source: ConnectionId,
        entry: &ConnectionEntryClone,
        request: &ProfileRequest,
        cancellation: &CancellationToken,
        deadline: tokio::time::Instant,
    ) -> ApiResult<(u64, u64)> {
        self.authorize_connection_operation(
            session,
            source,
            sift_protocol::OperationKind::ProfileQuery,
            Some(&request.sql),
            &[],
        )?;
        self.authorize_connection_operation(
            session,
            source,
            sift_protocol::OperationKind::ExecuteQuery,
            Some(&request.sql),
            &[],
        )?;
        let resources = self.reserve_query_resources(entry)?;
        let driver = entry.driver.clone();
        let handle = entry.handle.clone();
        let sql = request.sql.clone();
        let params = request.params.clone();
        let (configured_rows, configured_bytes) = self.result_limits();
        let max_rows = configured_rows.min(MAX_PROFILE_RESULT_ROWS);
        let max_bytes = configured_bytes.min(16 * 1024 * 1024);
        let cursor = Arc::new(Mutex::new(None));
        let cursor_slot = cursor.clone();
        let started = Instant::now();
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
            let mut rows = 0usize;
            let mut bytes = 0usize;
            let mut result_set = false;
            while let Some(page) = stream.rows.recv().await {
                match page {
                    Page::Rows { rows: page_rows } => {
                        for row in page_rows {
                            rows = rows.saturating_add(1);
                            bytes = bytes.saturating_add(
                                serde_json::to_vec(&row)
                                    .map_err(|_| {
                                        DriverError::new(
                                            Code::DriverInternal,
                                            "Profile row cannot be sized",
                                        )
                                    })?
                                    .len(),
                            );
                            if rows > max_rows || bytes > max_bytes {
                                return Err(DriverError::new(
                                    Code::ResultTooLarge,
                                    "SQLite profile result exceeds row or byte limit",
                                ));
                            }
                        }
                    }
                    Page::Error { error } => return Err(error),
                    Page::Done { .. } if result_set => {
                        return Ok((
                            rows as u64,
                            started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
                        ))
                    }
                    Page::Done { .. } => {
                        return Err(DriverError::new(
                            Code::UnsupportedResultShape,
                            "SQLite profile returned no query result set",
                        ))
                    }
                    Page::NextResult { .. } if !result_set => result_set = true,
                    Page::NextResult { .. } => {
                        return Err(DriverError::new(
                            Code::UnsupportedResultShape,
                            "SQLite profile returned multiple result sets",
                        ))
                    }
                }
            }
            Err(DriverError::new(
                Code::DriverInternal,
                "SQLite profile stream ended without completion",
            ))
        });
        let outcome = tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(ApiError::Driver(DriverError::new(Code::QueryCanceled, "Profile cancelled"))),
            _ = tokio::time::sleep_until(deadline) => Err(ApiError::Driver(DriverError::new(Code::QueryTimedOut, "Profile timed out"))),
            result = &mut task => match result {
                Ok(Ok(measurement)) => Ok(measurement),
                Ok(Err(error)) => Err(ApiError::Driver(error)),
                Err(_) => Err(ApiError::Internal("SQLite profile driver task failed".into())),
            },
        };
        if outcome.is_err() {
            task.abort();
            let cursor_id = *cursor.lock().unwrap();
            if let Some(cursor_id) = cursor_id {
                let _ = tokio::time::timeout(
                    Duration::from_secs(5),
                    entry.driver.cancel(entry.handle.clone(), cursor_id),
                )
                .await;
            }
        }
        outcome
    }

    async fn profile_plan(
        &self,
        session: SessionId,
        source: ConnectionId,
        entry: &ConnectionEntryClone,
        request: &ProfileRequest,
        cancellation: &CancellationToken,
    ) -> ApiResult<ProfileResponse> {
        self.authorize_connection_operation(
            session,
            source,
            sift_protocol::OperationKind::ProfileQuery,
            Some(&request.sql),
            &[],
        )?;
        self.authorize_connection_operation(
            session,
            source,
            sift_protocol::OperationKind::ExecuteQuery,
            Some(&request.sql),
            &[],
        )?;
        let resources = self.reserve_query_resources(entry)?;
        let driver = entry.driver.clone();
        let handle = entry.handle.clone();
        let sql = format!("EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) {}", request.sql);
        let params = request.params.clone();
        let max_bytes = self.result_limits().1;
        let cursor = Arc::new(Mutex::new(None));
        let cursor_slot = cursor.clone();
        let started = Instant::now();
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
            let mut plan = None;
            while let Some(page) = stream.rows.recv().await {
                match page {
                    Page::Rows { rows } => {
                        for row in rows {
                            let Some(value) = row.values.into_iter().next() else {
                                continue;
                            };
                            if plan.is_some() {
                                return Err(DriverError::new(
                                    Code::DriverInternal,
                                    "Profile returned multiple plan rows",
                                ));
                            }
                            let json = match value {
                                sift_protocol::Value::Json(value) => value,
                                sift_protocol::Value::Text(text) => {
                                    if text.len() > max_bytes {
                                        return Err(DriverError::new(
                                            Code::ResultTooLarge,
                                            "Profile plan exceeds result byte limit",
                                        ));
                                    }
                                    serde_json::from_str(&text).map_err(|_| {
                                        DriverError::new(
                                            Code::DriverInternal,
                                            "Profile returned invalid JSON",
                                        )
                                    })?
                                }
                                _ => {
                                    return Err(DriverError::new(
                                        Code::DriverInternal,
                                        "Profile returned no JSON plan",
                                    ))
                                }
                            };
                            if serde_json::to_vec(&json)
                                .map_err(|_| {
                                    DriverError::new(
                                        Code::DriverInternal,
                                        "Profile JSON cannot be encoded",
                                    )
                                })?
                                .len()
                                > max_bytes
                            {
                                return Err(DriverError::new(
                                    Code::ResultTooLarge,
                                    "Profile plan exceeds result byte limit",
                                ));
                            }
                            plan = Some(json);
                        }
                    }
                    Page::Error { error } => return Err(error),
                    Page::Done { .. } => {
                        return plan.ok_or_else(|| {
                            DriverError::new(Code::DriverInternal, "Profile returned no plan row")
                        })
                    }
                    Page::NextResult { .. } => {}
                }
            }
            Err(DriverError::new(
                Code::DriverInternal,
                "Profile stream ended without completion",
            ))
        });
        let outcome = tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(ApiError::Driver(DriverError::new(Code::QueryCanceled, "Profile cancelled"))),
            _ = tokio::time::sleep(Duration::from_millis(request.timeout_ms)) => Err(ApiError::Driver(DriverError::new(Code::QueryTimedOut, "Profile timed out"))),
            result = &mut task => match result {
                Ok(Ok(json)) => Ok(json),
                Ok(Err(error)) => Err(ApiError::Driver(error)),
                Err(_) => Err(ApiError::Internal("profile driver task failed".into())),
            }
        };
        if outcome.is_err() {
            task.abort();
            let cursor_id = *cursor.lock().unwrap();
            if let Some(cursor_id) = cursor_id {
                let _ = tokio::time::timeout(
                    Duration::from_secs(5),
                    entry.driver.cancel(entry.handle.clone(), cursor_id),
                )
                .await;
            }
        }
        let json = outcome?;
        let root = crate::plan::parse_pg_plan(&json).map_err(ApiError::Driver)?;
        let top = json.as_array().and_then(|values| values.first());
        let planning_ms = top
            .and_then(|value| value.get("Planning Time"))
            .and_then(|value| value.as_f64());
        let execution_ms = top
            .and_then(|value| value.get("Execution Time"))
            .and_then(|value| value.as_f64());
        let raw = serde_json::to_string_pretty(&json)
            .map_err(|_| ApiError::Internal("Profile JSON cannot be encoded".into()))?;
        Ok(ProfileResponse {
            run_id: request.run_id,
            plan: sift_protocol::ExplainResponse { engine: Engine::Postgres, analyzed: true, root, raw, warnings: Vec::new() },
            server_elapsed_ns: started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
            rows_returned: None,
            planning_ms,
            execution_ms,
            environment: None,
            warnings: vec!["Instrumentation adds overhead; this run used a dedicated read-only connection. Cache state and external function side effects are unknown".into()],
        })
    }

    async fn profile_owned_mssql(
        &self,
        session: SessionId,
        source: ConnectionId,
        dedicated: ConnectionId,
        entry: &ConnectionEntryClone,
        request: &ProfileRequest,
        cancellation: &CancellationToken,
    ) -> ApiResult<ProfileResponse> {
        let setup = self
            .profile_mssql_setting(entry, "SET STATISTICS XML ON")
            .await;
        if let Err(error) = setup {
            let _ = tokio::time::timeout(
                Duration::from_secs(5),
                self.close_connection_unchecked(session, dedicated),
            )
            .await;
            return Err(error);
        }
        let result = self
            .profile_mssql_plan(session, source, entry, request, cancellation)
            .await;
        let reset = if result.is_ok() {
            self.profile_mssql_setting(entry, "SET STATISTICS XML OFF")
                .await
        } else {
            Ok(())
        };
        let cleanup = tokio::time::timeout(
            Duration::from_secs(5),
            self.close_connection_unchecked(session, dedicated),
        )
        .await;
        let mut response = result?;
        if reset.is_err() {
            response
                .warnings
                .push("STATISTICS XML reset failed; dedicated connection was discarded".into());
        }
        if !matches!(cleanup, Ok(Ok(()))) {
            response
                .warnings
                .push("Dedicated connection cleanup did not complete cleanly".into());
        }
        Ok(response)
    }

    async fn profile_mssql_setting(
        &self,
        entry: &ConnectionEntryClone,
        sql: &'static str,
    ) -> ApiResult<()> {
        let driver = entry.driver.clone();
        let handle = entry.handle.clone();
        self.run_bounded("SQL Server profile setting", async move {
            let mut stream = driver
                .execute(
                    handle,
                    ExecuteRequest {
                        sql: sql.into(),
                        params: Vec::new(),
                        transform: None,
                    },
                )
                .await?;
            while let Some(page) = stream.rows.recv().await {
                match page {
                    Page::Done { .. } => return Ok(()),
                    Page::Error { error } => return Err(error),
                    _ => {}
                }
            }
            Err(DriverError::new(
                Code::DriverInternal,
                "SQL Server profile setting did not complete",
            ))
        })
        .await
    }

    async fn profile_mssql_plan(
        &self,
        session: SessionId,
        source: ConnectionId,
        entry: &ConnectionEntryClone,
        request: &ProfileRequest,
        cancellation: &CancellationToken,
    ) -> ApiResult<ProfileResponse> {
        self.authorize_connection_operation(
            session,
            source,
            sift_protocol::OperationKind::ProfileQuery,
            Some(&request.sql),
            &[],
        )?;
        self.authorize_connection_operation(
            session,
            source,
            sift_protocol::OperationKind::ExecuteQuery,
            Some(&request.sql),
            &[],
        )?;
        let resources = self.reserve_query_resources(entry)?;
        let driver = entry.driver.clone();
        let handle = entry.handle.clone();
        let sql = request.sql.clone();
        let params = request.params.clone();
        let (configured_rows, configured_bytes) = self.result_limits();
        let max_rows = configured_rows.min(MAX_PROFILE_RESULT_ROWS);
        let max_bytes = configured_bytes.min(16 * 1024 * 1024);
        let cursor = Arc::new(Mutex::new(None));
        let cursor_slot = cursor.clone();
        let started = Instant::now();
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
            let mut xml = None;
            let mut ordinary_rows = 0usize;
            let mut ordinary_bytes = 0usize;
            let mut showplan_result = false;
            while let Some(page) = stream.rows.recv().await {
                match page {
                    Page::Rows { rows } => {
                        for row in rows {
                            let showplan = showplan_result
                                && row.values.len() == 1
                                && row.values.first().and_then(showplan_text).is_some();
                            if showplan {
                                let text = showplan_text(&row.values[0]).unwrap();
                                if text.len() > max_bytes {
                                    return Err(DriverError::new(
                                        Code::ResultTooLarge,
                                        "SQL Server actual plan exceeds result byte limit",
                                    ));
                                }
                                if xml.replace(text.to_owned()).is_some() {
                                    return Err(DriverError::new(
                                        Code::UnsupportedResultShape,
                                        "Profile returned multiple SQL Server plans",
                                    ));
                                }
                            } else {
                                ordinary_rows += 1;
                                ordinary_bytes = ordinary_bytes.saturating_add(
                                    serde_json::to_vec(&row)
                                        .map_err(|_| {
                                            DriverError::new(
                                                Code::DriverInternal,
                                                "Profile row cannot be sized",
                                            )
                                        })?
                                        .len(),
                                );
                                if ordinary_rows > max_rows || ordinary_bytes > max_bytes {
                                    return Err(DriverError::new(
                                        Code::ResultTooLarge,
                                        "Profile result exceeds row or byte limit",
                                    ));
                                }
                            }
                        }
                    }
                    Page::Error { error } => return Err(error),
                    Page::Done { .. } => {
                        return xml.ok_or_else(|| {
                            DriverError::new(
                                Code::UnsupportedResultShape,
                                "SQL Server returned no actual plan",
                            )
                        })
                    }
                    Page::NextResult { columns } => {
                        showplan_result = columns.len() == 1
                            && columns[0].name.to_ascii_lowercase().contains("showplan");
                    }
                }
            }
            Err(DriverError::new(
                Code::DriverInternal,
                "Profile stream ended without completion",
            ))
        });
        let outcome = tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(ApiError::Driver(DriverError::new(Code::QueryCanceled, "Profile cancelled"))),
            _ = tokio::time::sleep(Duration::from_millis(request.timeout_ms)) => Err(ApiError::Driver(DriverError::new(Code::QueryTimedOut, "Profile timed out"))),
            result = &mut task => match result {
                Ok(Ok(xml)) => Ok(xml),
                Ok(Err(error)) => Err(ApiError::Driver(error)),
                Err(_) => Err(ApiError::Internal("profile driver task failed".into())),
            }
        };
        if outcome.is_err() {
            task.abort();
            let cursor_id = *cursor.lock().unwrap();
            if let Some(cursor_id) = cursor_id {
                let _ = tokio::time::timeout(
                    Duration::from_secs(5),
                    entry.driver.cancel(entry.handle.clone(), cursor_id),
                )
                .await;
            }
        }
        let xml = outcome?;
        let root = crate::plan::parse_mssql_plan(&xml).map_err(ApiError::Driver)?;
        let execution_ms = crate::plan::mssql_query_elapsed_ms(&xml);
        Ok(ProfileResponse {
            run_id: request.run_id,
            plan: sift_protocol::ExplainResponse {
                engine: Engine::SqlServer,
                analyzed: true,
                root,
                raw: xml,
                warnings: Vec::new(),
            },
            server_elapsed_ns: started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
            rows_returned: None,
            planning_ms: None,
            execution_ms,
            environment: None,
            warnings: vec![
                "Instrumentation adds overhead; this run used a dedicated connection. SQL Server has no read-only transaction mode: use a read-only database account. Cache state and external function side effects are unknown".into(),
                "STATISTICS IO/TIME messages are unavailable through this driver; per-node counters are shown only where Showplan XML provides them".into(),
            ],
        })
    }
}

fn profile_environment(engine: Engine, server_version: Option<String>) -> ProfileEnvironment {
    let (instrumentation, isolation) = match engine {
        Engine::Postgres => ("explain_analyze_buffers_json", "read_only_read_committed"),
        Engine::SqlServer => ("statistics_xml", "driver_default_no_read_only_transaction"),
        Engine::Sqlite => (
            "estimated_plan_plus_measured_read",
            "read_only_serializable",
        ),
    };
    ProfileEnvironment {
        server_version,
        instrumentation: instrumentation.into(),
        isolation: isolation.into(),
        session_settings: "connection_profile_defaults_uninspected".into(),
        cache_state: "unknown".into(),
    }
}

fn showplan_text(value: &sift_protocol::Value) -> Option<&str> {
    let text = match value {
        sift_protocol::Value::Text(text) => text.as_str(),
        sift_protocol::Value::Native { display_text, .. } => display_text.as_str(),
        _ => return None,
    };
    text.contains("<ShowPlanXML").then_some(text)
}
