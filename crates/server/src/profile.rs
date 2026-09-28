//! PostgreSQL instrumented read plans. The source connection remains untouched;
//! a supervised job owns a dedicated read-only connection and its cleanup.
use super::*;
use sift_protocol::{ProfileRequest, ProfileResponse, TxAccessMode, TxMode};
use tokio_util::sync::CancellationToken;

const MAX_PROFILE_TIMEOUT_MS: u64 = 120_000;

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
        if entry.driver.semantic_engine() != Some(Engine::Postgres) {
            return Err(ApiError::Driver(DriverError::new(
                Code::UnsupportedForEngine,
                "Profile currently supports PostgreSQL only",
            )));
        }
        benchmark::validate_read(Engine::Postgres, &request.sql)?;
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
                .profile_owned(session, source, request, cancellation)
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
                Some(Engine::Postgres),
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
            planning_ms,
            execution_ms,
            warnings: vec!["Instrumentation adds overhead; this run used a dedicated read-only connection. Cache state and external function side effects are unknown".into()],
        })
    }
}
