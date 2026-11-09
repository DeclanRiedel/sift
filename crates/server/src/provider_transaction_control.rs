//! Owned, serialized controls for built-in and external transactions.
use super::*;

pub(super) enum ProviderTransactionControl {
    Create(String),
    RollbackTo(String),
    Release(String),
    Commit,
    Rollback,
}

impl SessionStore {
    pub(super) async fn provider_transaction_control(
        &self,
        session: SessionId,
        connection: ConnectionId,
        tx_id: TxId,
        action: ProviderTransactionControl,
        operation: sift_protocol::OperationKind,
    ) -> ApiResult<()> {
        // Bound queued admission as well as remote work. The owned task keeps
        // its lock through acknowledgment and metadata settlement, even if its
        // observer goes away or run_bounded's observation deadline expires.
        let control = self.with_transaction(session, tx_id, |tx| Ok(tx.control.clone()))?;
        let retained = self.with_session(&session, |session| {
            session
                .connections
                .get(&connection)
                .map(|entry| (entry._resource_guard.clone(), entry._tunnel.clone()))
                .ok_or(ApiError::ConnectionNotFound(connection))
        })??;
        let timeout = self.request_timeout();
        let admission = if timeout.is_zero() {
            Duration::from_secs(5)
        } else {
            timeout.min(Duration::from_secs(5))
        };
        let deadline = tokio::time::Instant::now() + admission;
        let store = self.clone();
        self.transaction_work("transaction control", async move {
            let _retained = retained;
            store
                .provider_transaction_control_owned(
                    session, connection, tx_id, action, operation, control, deadline,
                )
                .await
        })
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn provider_transaction_control_owned(
        &self,
        session: SessionId,
        connection: ConnectionId,
        tx_id: TxId,
        mut action: ProviderTransactionControl,
        operation: sift_protocol::OperationKind,
        control: Arc<tokio::sync::RwLock<()>>,
        deadline: tokio::time::Instant,
    ) -> ApiResult<()> {
        let _control = tokio::time::timeout_at(deadline, control.clone().write_owned())
            .await
            .map_err(|_| {
                ApiError::Driver(DriverError::new(
                    Code::PoolExhausted,
                    "transaction control admission deadline expired; no request dispatched",
                ))
            })?;
        // timeout_at may poll an immediately-ready lock before its expired
        // timer. Never dispatch queued work whose admission already expired.
        if tokio::time::Instant::now() >= deadline {
            return Err(ApiError::Driver(DriverError::new(
                Code::PoolExhausted,
                "transaction control admission deadline expired; no request dispatched",
            )));
        }
        // Recheck mutable policy and source ownership after waiting in line.
        self.authorize_connection_operation(session, connection, operation, None, &[])?;
        let entry = self.get_conn_entry(session, connection)?;
        if !Arc::ptr_eq(&control, &entry.execution.gate) {
            return Err(ApiError::Driver(DriverError::new(
                Code::ConnectionInvalidated,
                "transaction source connection was replaced",
            )));
        }
        let handle = self.tx_handle_for(session, connection, tx_id)?;
        let external = handle.builtin().is_none();
        if !external {
            if let ProviderTransactionControl::Create(name) = &mut action {
                *name = name.trim().to_string();
            }
            if matches!(action, ProviderTransactionControl::Release(_))
                && entry.driver.engine() == Engine::SqlServer
            {
                return Err(ApiError::Driver(
                    DriverError::new(
                        Code::UnsupportedForEngine,
                        "RELEASE SAVEPOINT is not supported by SQL Server",
                    )
                    .with_engine(Engine::SqlServer),
                ));
            }
        }
        if matches!(
            action,
            ProviderTransactionControl::Create(_)
                | ProviderTransactionControl::Release(_)
                | ProviderTransactionControl::Commit
        ) && self.transaction_failed(session, connection, tx_id)?
        {
            return Err(ApiError::Conflict(
                "failed transaction must be rolled back before this operation".into(),
            ));
        }
        let name = match &action {
            ProviderTransactionControl::Create(name)
            | ProviderTransactionControl::RollbackTo(name)
            | ProviderTransactionControl::Release(name) => Some(name.as_str()),
            _ => None,
        };
        if let Some(name) = name {
            if name.is_empty() || (external && !sift_extension_protocol::valid_savepoint_name(name))
            {
                return Err(ApiError::BadRequest(
                    "savepoint name must contain 1..128 UTF-8 bytes without control characters"
                        .into(),
                ));
            }
            match &action {
                ProviderTransactionControl::Create(_) => {
                    self.ensure_savepoint_name_available(session, tx_id, name)?
                }
                _ => self.ensure_active_savepoint(session, tx_id, name)?,
            }
        }
        let failure_sequence =
            self.with_transaction(session, tx_id, |tx| Ok(tx.failure.lock().unwrap().sequence))?;
        let native_action = match action {
            ProviderTransactionControl::Commit => Some(native_transaction::Action::Commit),
            ProviderTransactionControl::Rollback => Some(native_transaction::Action::Rollback),
            _ => None,
        };
        if let Some(action) = native_action {
            native_transaction::admit(action, Some(tx_id), &entry.execution, || {
                self.claim_tx_end(session, connection, tx_id).map(|_| ())
            })?;
        }
        let mut mutation = super::execution_control::MutationLease::new(entry.execution.clone());
        let result = dispatch_control(handle, &action).await;
        if native_action.is_some() {
            native_transaction::acknowledge(Some(tx_id), result.as_ref().map(|_| ()));
        }
        mutation.settle();
        if let Err(error) = result {
            self.mark_transaction_failed(session, tx_id);
            if name.is_none() {
                // A terminal command may have applied even when its response
                // was lost. Keep ending state and fence every reuse of this source.
                entry.execution.fence();
                return Err(ApiError::Driver(DriverError::new(
                    Code::ConnectionInvalidated,
                    "transaction completion outcome is uncertain; reconnect required",
                )));
            }
            return Err(ApiError::Driver(error));
        }
        match action {
            ProviderTransactionControl::Create(name) => {
                self.update_savepoints(session, tx_id, |points| {
                    points.push(SavepointInfo {
                        name,
                        created_at: chrono::Utc::now(),
                        state: SavepointState::Active,
                    });
                })
            }
            ProviderTransactionControl::RollbackTo(name) => {
                self.with_transaction(session, tx_id, |tx| {
                    let mut failure = tx.failure.lock().unwrap();
                    // A query failure reported after recovery admission stays
                    // failed, even if the provider's older ack arrives later.
                    if failure.sequence == failure_sequence && failure_sequence != u64::MAX {
                        failure.failed = false;
                    }
                    Ok(())
                })?;
                self.update_savepoints(session, tx_id, |points| {
                    if let Some(index) = points.iter().position(|point| {
                        point.name == name && point.state == SavepointState::Active
                    }) {
                        for point in points
                            .iter_mut()
                            .skip(index + 1)
                            .filter(|point| point.state == SavepointState::Active)
                        {
                            point.state = SavepointState::Invalidated;
                        }
                    }
                })
            }
            ProviderTransactionControl::Release(name) => {
                self.update_savepoints(session, tx_id, |points| {
                    if let Some(index) = points.iter().position(|point| {
                        point.name == name && point.state == SavepointState::Active
                    }) {
                        for point in points
                            .iter_mut()
                            .skip(index)
                            .filter(|point| point.state == SavepointState::Active)
                        {
                            point.state = SavepointState::Released;
                        }
                    }
                })
            }
            ProviderTransactionControl::Commit | ProviderTransactionControl::Rollback => {
                if let Err(error) = self.remove_tx(session, connection, tx_id) {
                    entry.execution.fence();
                    return Err(error);
                }
                Ok(())
            }
        }
    }
}

async fn dispatch_control(
    transaction: RuntimeTransactionHandle,
    action: &ProviderTransactionControl,
) -> Result<(), DriverError> {
    use ProviderTransactionControl::{Commit, Create, Release, Rollback, RollbackTo};
    if matches!(action, Commit) {
        return transaction.commit().await;
    }
    if matches!(action, Rollback) {
        return transaction.rollback().await;
    }
    match transaction {
        RuntimeTransactionHandle::External {
            provider, handle, ..
        } => match action {
            Create(name) => provider.provider.savepoint(&handle, name).await,
            RollbackTo(name) => provider.provider.rollback_to_savepoint(&handle, name).await,
            Release(name) => provider.provider.release_savepoint(&handle, name).await,
            _ => unreachable!("terminal controls dispatched above"),
        },
        RuntimeTransactionHandle::Builtin { driver, handle, .. } => match (action, driver.engine())
        {
            (Create(name), Engine::Sqlite) => {
                driver
                    .as_sqlite()
                    .ok_or_else(|| missing_ext(Engine::Sqlite, "SqliteExt"))?
                    .savepoint(&handle, name)
                    .await
            }
            (RollbackTo(name), Engine::Sqlite) => {
                driver
                    .as_sqlite()
                    .ok_or_else(|| missing_ext(Engine::Sqlite, "SqliteExt"))?
                    .rollback_to(&handle, name)
                    .await
            }
            (Release(name), Engine::Sqlite) => {
                driver
                    .as_sqlite()
                    .ok_or_else(|| missing_ext(Engine::Sqlite, "SqliteExt"))?
                    .release_savepoint(&handle, name)
                    .await
            }
            (Create(name), Engine::Postgres) => driver
                .as_pg()
                .ok_or_else(|| missing_ext(Engine::Postgres, "PgExt"))?
                .savepoint(&handle, name)
                .await
                .map(|_| ()),
            (RollbackTo(name), Engine::Postgres) => {
                driver
                    .as_pg()
                    .ok_or_else(|| missing_ext(Engine::Postgres, "PgExt"))?
                    .rollback_to(PgSavepoint {
                        tx: handle.tx_id,
                        conn: handle.conn.clone(),
                        name: name.clone(),
                    })
                    .await
            }
            (Release(name), Engine::Postgres) => {
                driver
                    .as_pg()
                    .ok_or_else(|| missing_ext(Engine::Postgres, "PgExt"))?
                    .release_savepoint(PgSavepoint {
                        tx: handle.tx_id,
                        conn: handle.conn.clone(),
                        name: name.clone(),
                    })
                    .await
            }
            (Create(name), Engine::SqlServer) => driver
                .as_mssql()
                .ok_or_else(|| missing_ext(Engine::SqlServer, "MssqlExt"))?
                .savepoint(&handle, name)
                .await
                .map(|_| ()),
            (RollbackTo(name), Engine::SqlServer) => {
                driver
                    .as_mssql()
                    .ok_or_else(|| missing_ext(Engine::SqlServer, "MssqlExt"))?
                    .rollback_to(MssqlSavepoint {
                        tx: handle.tx_id,
                        conn: handle.conn.clone(),
                        name: name.clone(),
                    })
                    .await
            }
            (Release(_), Engine::SqlServer) => Err(DriverError::new(
                Code::UnsupportedForEngine,
                "RELEASE SAVEPOINT is not supported by SQL Server",
            )),
            _ => unreachable!("terminal controls dispatched above"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        registry::{DatabaseProvider, DriverRegistry, ProviderOpenRequest},
        rpc_provider::RpcProvider,
        ApiError, SessionStore,
    };
    use sift_extension_protocol::{ContributionId, ExtensionId, VersionRange, WireId};
    use sift_plugin_host::{ProcessSpec, SupervisedProcess, SupervisorLimits};
    use sift_protocol::{
        BeginTransactionRequest, ConnectionSpec, EndTransactionRequest, OpenSessionRequest,
        ProviderCapability, ProviderDescriptor, ProviderId, ProviderRef, SavepointRequest,
        SavepointState, TransactionCondition, TxMode,
    };

    async fn fixture(
        mode: &str,
        generation: u128,
    ) -> (tempfile::TempDir, Arc<RpcProvider>, Arc<SupervisedProcess>) {
        // This explicit acceptance case reuses the host's executable corpus;
        // standalone server tests do not build another package's binaries.
        let executable = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join(format!(
                "sift-conformance-provider{}",
                std::env::consts::EXE_SUFFIX
            ));
        assert!(
            executable.is_file(),
            "build sift-plugin-host --bin sift-conformance-provider first"
        );
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join(".sift-conformance-mode"), mode).unwrap();
        let contribution =
            ContributionId::new("acme/conformance/database_provider/fixture").unwrap();
        let process = Arc::new(
            SupervisedProcess::start(
                ProcessSpec {
                    executable,
                    working_directory: temp.path().into(),
                    extension_id: ExtensionId::new("acme/conformance").unwrap(),
                    extension_version: "1.0.0".into(),
                    manifest_sha256: "a".repeat(64),
                    expected_contributions: vec![contribution.clone()],
                    driver_rpc: Some(VersionRange {
                        minimum: 1,
                        maximum: 2,
                    }),
                    generation: WireId::from_u128(generation),
                    granted_capabilities: vec![],
                },
                SupervisorLimits {
                    heartbeat_interval: Duration::from_secs(60),
                    ..SupervisorLimits::default()
                },
            )
            .await
            .unwrap(),
        );
        let descriptor = ProviderDescriptor {
            provider: ProviderRef {
                provider_id: ProviderId::new("acme/conformance").unwrap(),
                dialect_id: sift_protocol::DialectId::new("sift/postgresql").unwrap(),
                provider_version: "1.0.0".into(),
            },
            display_name: "Wire fixture".into(),
            configuration_schema: serde_json::json!({"type":"object"}),
            credential_schema: serde_json::json!({"type":"object"}),
            configuration_schema_version: 1,
            capabilities: [
                "driver.core@1",
                "driver.transactions@1",
                "driver.savepoints@1",
            ]
            .into_iter()
            .map(|id| ProviderCapability {
                id: id.into(),
                limits: Default::default(),
            })
            .collect(),
            quality: None,
            available: true,
        };
        let provider =
            Arc::new(RpcProvider::new(descriptor, contribution, process.clone()).unwrap());
        (temp, provider, process)
    }

    async fn session(
        provider: Arc<RpcProvider>,
    ) -> (
        SessionStore,
        sift_protocol::SessionId,
        sift_protocol::ConnectionId,
        sift_protocol::TxId,
    ) {
        let registry = DriverRegistry::new();
        registry
            .providers()
            .replace([provider as Arc<dyn DatabaseProvider>])
            .unwrap();
        let store = SessionStore::new(registry);
        let session = store
            .open_session(OpenSessionRequest {
                tag: None,
                tenant_id: None,
            })
            .id;
        let connection = store
            .open_provider_connection(
                session,
                ProviderId::new("acme/conformance").unwrap(),
                ConnectionSpec {
                    host: "fixture".into(),
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
        let tx = store
            .begin_transaction(
                session,
                BeginTransactionRequest {
                    connection,
                    mode: TxMode::default(),
                },
            )
            .await
            .unwrap()
            .tx_id;
        (store, session, connection, tx)
    }

    async fn entered(directory: &std::path::Path) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !directory.join(".sift-savepoint-entered").exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    #[ignore = "requires cargo build -p sift-plugin-host --bin sift-conformance-provider"]
    async fn executable_savepoint_lifecycle_fences_uncertainty_and_retains_abandoned_work() {
        for (index, method) in ["begin", "commit", "rollback"].into_iter().enumerate() {
            let (_directory, provider, process) =
                fixture(&format!("v2_{method}_uncertain"), 100 + index as u128).await;
            let connection = provider
                .open(ProviderOpenRequest {
                    configuration: serde_json::json!({}),
                    credentials: Default::default(),
                    tenant_id: None,
                })
                .await
                .unwrap();
            let result = if method == "begin" {
                provider
                    .begin(&connection, TxMode::default())
                    .await
                    .map(|_| ())
            } else {
                let transaction = provider
                    .begin(&connection, TxMode::default())
                    .await
                    .unwrap();
                if method == "commit" {
                    provider.commit(transaction).await
                } else {
                    provider.rollback(transaction).await
                }
            };
            assert_eq!(result.unwrap_err().code, Code::ConnectionInvalidated);
            assert_eq!(
                process.health().await,
                sift_plugin_host::GenerationHealth::Stopped
            );
            assert!(provider.ping(&connection).await.is_err());
        }
        let (_directory, provider, process) = fixture("v2", 11).await;
        let (store, id, connection, tx_id) = session(provider.clone()).await;
        let request = |name: &str| SavepointRequest {
            connection,
            tx_id,
            name: name.into(),
        };
        for invalid in ["", "line\nbreak", &"a".repeat(129)] {
            assert!(store.create_savepoint(id, request(invalid)).await.is_err());
        }
        assert!(store
            .create_savepoint(
                id,
                SavepointRequest {
                    connection: sift_protocol::ConnectionId(connection.0 + 1),
                    ..request("wrong")
                }
            )
            .await
            .is_err());
        assert!(store
            .create_savepoint(
                id,
                SavepointRequest {
                    tx_id: sift_protocol::TxId::new(tx_id.0 + 1),
                    ..request("wrong")
                }
            )
            .await
            .is_err());
        let control = store
            .with_transaction(id, tx_id, |tx| Ok(tx.control.clone()))
            .unwrap();
        let expired = store
            .provider_transaction_control_owned(
                id,
                connection,
                tx_id,
                ProviderTransactionControl::Create("expired_ready".into()),
                sift_protocol::OperationKind::Savepoint,
                control,
                tokio::time::Instant::now() - Duration::from_secs(1),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            expired,
            ApiError::Driver(DriverError {
                code: Code::PoolExhausted,
                ..
            })
        ));
        assert!(store.list_transactions(id).unwrap()[0]
            .savepoints
            .is_empty());
        store.create_savepoint(id, request("outer")).await.unwrap();
        store.create_savepoint(id, request("inner")).await.unwrap();
        assert!(store.create_savepoint(id, request("outer")).await.is_err());
        store.mark_transaction_failed(id, tx_id);
        assert!(store
            .commit_transaction(id, EndTransactionRequest { connection, tx_id })
            .await
            .is_err());
        store
            .rollback_to_savepoint(id, request("outer"))
            .await
            .unwrap();
        let state = store.list_transactions(id).unwrap().remove(0);
        assert_eq!(state.condition, TransactionCondition::Active);
        assert_eq!(state.savepoints[1].state, SavepointState::Invalidated);
        assert!(store.release_savepoint(id, request("inner")).await.is_err());
        store.create_savepoint(id, request("inner")).await.unwrap();
        store.release_savepoint(id, request("outer")).await.unwrap();
        assert!(store.list_transactions(id).unwrap()[0]
            .savepoints
            .iter()
            .all(|point| point.state != SavepointState::Active));
        store
            .commit_transaction(id, EndTransactionRequest { connection, tx_id })
            .await
            .unwrap();
        assert!(store.list_transactions(id).unwrap().is_empty());
        store.close_connection(id, connection).await.unwrap();
        process.shutdown("complete").await.unwrap();

        // Invalid acks, provider errors, disconnect and a real RPC deadline
        // cannot fabricate host state. Every uncertain generation is fenced.
        for mode in [
            "v2_bad_ack",
            "v2_bad_name",
            "v2_malformed_ack",
            "v2_error",
            "v2_exit",
            "v2_timeout",
            "v2_fail_recovery",
        ] {
            let (_directory, provider, process) = fixture(mode, 12).await;
            let (store, id, connection, tx_id) = session(provider).await;
            let request = SavepointRequest {
                connection,
                tx_id,
                name: "uncertain".into(),
            };
            let error = if mode == "v2_fail_recovery" {
                store.create_savepoint(id, request.clone()).await.unwrap();
                store.mark_transaction_failed(id, tx_id);
                store.rollback_to_savepoint(id, request).await.unwrap_err()
            } else {
                // Let the provider RPC's own deadline settle before the
                // ordinary observer timeout for this particular case.
                store.set_request_timeout(Duration::from_secs(40));
                store.create_savepoint(id, request).await.unwrap_err()
            };
            assert!(
                matches!(
                    error,
                    ApiError::Driver(DriverError {
                        code: Code::ConnectionInvalidated,
                        ..
                    })
                ),
                "{mode}: {error}"
            );
            let state = store.list_transactions(id).unwrap().remove(0);
            if mode == "v2_fail_recovery" {
                assert_eq!(state.savepoints[0].state, SavepointState::Active);
            } else {
                assert!(state.savepoints.is_empty());
            }
            assert_eq!(state.condition, TransactionCondition::Failed);
            assert!(store.ping(id, connection).await.is_err(), "{mode}");
            assert_eq!(process.active_work().await, 0);
        }

        // Reject stale generations and v1 before a mutating request is sent.
        let (_directory, v1, process) = fixture("normal", 13).await;
        let remote = v1
            .open(ProviderOpenRequest {
                configuration: serde_json::json!({}),
                credentials: Default::default(),
                tenant_id: None,
            })
            .await
            .unwrap();
        let transaction = v1.begin(&remote, TxMode::default()).await.unwrap();
        assert_eq!(
            v1.savepoint(&transaction, "v1").await.unwrap_err().code,
            Code::UnsupportedForEngine
        );
        v1.ping(&remote).await.unwrap();
        process.shutdown("complete").await.unwrap();
        let (_directory, next, process) = fixture("v2", 14).await;
        assert_eq!(
            next.savepoint(&transaction, "stale")
                .await
                .unwrap_err()
                .code,
            Code::TransactionNotFound
        );
        process.shutdown("complete").await.unwrap();

        // Observation timeout keeps the host owner through its eventual ack.
        let (directory, provider, process) = fixture("v2_delay", 15).await;
        let (store, id, connection, tx_id) = session(provider).await;
        store.set_request_timeout(Duration::from_millis(50));
        let error = store
            .create_savepoint(
                id,
                SavepointRequest {
                    connection,
                    tx_id,
                    name: "late".into(),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ApiError::Driver(DriverError {
                code: Code::QueryTimedOut,
                ..
            })
        ));
        entered(directory.path()).await;
        assert!(store
            .create_savepoint(
                id,
                SavepointRequest {
                    connection,
                    tx_id,
                    name: "expired_queue".into()
                }
            )
            .await
            .is_err());
        tokio::time::timeout(Duration::from_secs(5), async {
            while store.list_transactions(id).unwrap()[0]
                .savepoints
                .is_empty()
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(store.list_transactions(id).unwrap()[0].savepoints.len(), 1);
        store.set_request_timeout(Duration::from_secs(5));
        store.mark_transaction_failed(id, tx_id);
        std::fs::remove_file(directory.path().join(".sift-savepoint-entered")).unwrap();
        let recovery_store = store.clone();
        let recovery = tokio::spawn(async move {
            recovery_store
                .rollback_to_savepoint(
                    id,
                    SavepointRequest {
                        connection,
                        tx_id,
                        name: "late".into(),
                    },
                )
                .await
        });
        entered(directory.path()).await;
        store.mark_transaction_failed(id, tx_id);
        recovery.await.unwrap().unwrap();
        assert_eq!(
            store.list_transactions(id).unwrap()[0].condition,
            TransactionCondition::Failed
        );
        store
            .rollback_transaction(id, EndTransactionRequest { connection, tx_id })
            .await
            .unwrap();
        store.close_connection(id, connection).await.unwrap();
        process.shutdown("complete").await.unwrap();

        // Revoke durable policy while a second control waits behind admitted
        // work. Authorization must use the current policy after admission.
        let (directory, provider, process) = fixture("v2_hold", 18).await;
        let metadata =
            MetadataStore::open_in_memory(Arc::new(sift_metadata::MemorySecretStore::default()))
                .unwrap();
        metadata
            .bootstrap_local("savepoint policy fixture")
            .unwrap();
        let actor = PrincipalId(1);
        let tenant = sift_metadata::TenantId(1);
        let profile = metadata
            .upsert_connection_profile(
                tenant,
                actor,
                sift_metadata::NewConnectionProfile {
                    name: "external policy fixture".into(),
                    provider_id: provider.descriptor().provider.provider_id.clone(),
                    configuration: serde_json::json!({"host":"fixture","user":"fixture"}),
                    semantic_engine: Some(Engine::Postgres),
                    credentials: None,
                    credential_mode: sift_metadata::CredentialMode::Shared,
                    tags: vec![],
                },
            )
            .await
            .unwrap();
        let registry = DriverRegistry::new();
        registry
            .providers()
            .replace([provider as Arc<dyn DatabaseProvider>])
            .unwrap();
        let store = SessionStore::new(registry);
        store.set_authorization_store(metadata.clone());
        let id = store
            .open_session_with_owner(
                OpenSessionRequest {
                    tag: None,
                    tenant_id: None,
                },
                Some(actor),
                Some(tenant),
                false,
            )
            .unwrap()
            .id;
        let connection = store
            .open_managed_connection(
                id,
                profile.provider_id.clone(),
                profile.configuration,
                Default::default(),
                actor,
                tenant,
                profile.id,
                profile.policy.revision,
                true,
            )
            .await
            .unwrap()
            .id;
        let tx_id = store
            .begin_transaction(
                id,
                BeginTransactionRequest {
                    connection,
                    mode: TxMode::default(),
                },
            )
            .await
            .unwrap()
            .tx_id;
        let operation_store = store.clone();
        let operation = tokio::spawn(async move {
            operation_store
                .create_savepoint(
                    id,
                    SavepointRequest {
                        connection,
                        tx_id,
                        name: "admitted".into(),
                    },
                )
                .await
        });
        entered(directory.path()).await;
        store
            .authorize_connection_operation(
                id,
                connection,
                sift_protocol::OperationKind::Savepoint,
                None,
                &[],
            )
            .unwrap();
        let pending = store.create_savepoint(
            id,
            SavepointRequest {
                connection,
                tx_id,
                name: "denied".into(),
            },
        );
        tokio::pin!(pending);
        assert!(futures::poll!(pending.as_mut()).is_pending());
        metadata
            .update_connection_policy(
                tenant,
                actor,
                profile.id,
                sift_protocol::UpdateConnectionPolicyRequest {
                    expected_revision: Some(0),
                    minimum_tenant_role: sift_protocol::TenantRole::Member,
                    read_only: false,
                    allowed_ops: None,
                    blocked_ops: vec![sift_protocol::OperationKind::Savepoint],
                    allowed_schemas: None,
                },
                NewOperationAudit {
                    actor_principal_id: Some(actor),
                    action: "update_policy".into(),
                    target: "connection_profile".into(),
                    target_id: Some(profile.id.0),
                    status: "succeeded".into(),
                    result_code: None,
                    row_count: None,
                    error_message: None,
                    correlation_id: None,
                },
            )
            .unwrap();
        std::fs::write(directory.path().join(".sift-savepoint-release"), b"release").unwrap();
        operation.await.unwrap().unwrap();
        assert!(matches!(pending.await.unwrap_err(), ApiError::Forbidden(_)));
        assert_eq!(store.list_transactions(id).unwrap()[0].savepoints.len(), 1);
        store
            .rollback_transaction(id, EndTransactionRequest { connection, tx_id })
            .await
            .unwrap();
        store.close_connection(id, connection).await.unwrap();
        process.shutdown("complete").await.unwrap();

        // Close keeps quota and cleanup ownership even when its observer dies.
        let (directory, provider, process) = fixture("v2_hold", 17).await;
        let (store, id, connection, tx_id) = session(provider).await;
        let resources = crate::resources::ResourceManager::default();
        let tenant = sift_metadata::TenantId(42);
        let guard = resources
            .reserve(tenant, sift_protocol::TenantResource::Connections, 1)
            .unwrap();
        store
            .inner
            .sessions
            .get(&id)
            .unwrap()
            .connections
            .get_mut(&connection)
            .unwrap()
            ._resource_guard = Some(Arc::new(guard));
        let operation_store = store.clone();
        let operation = tokio::spawn(async move {
            operation_store
                .create_savepoint(
                    id,
                    SavepointRequest {
                        connection,
                        tx_id,
                        name: "closing".into(),
                    },
                )
                .await
        });
        entered(directory.path()).await;
        let closing_store = store.clone();
        let closing =
            tokio::spawn(async move { closing_store.close_connection(id, connection).await });
        tokio::time::timeout(Duration::from_secs(5), async {
            while store.get_conn_entry(id, connection).is_ok() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        closing.abort();
        assert_eq!(resources.snapshot(tenant).unwrap().usage.connections, 1);
        assert!(!directory.path().join(".sift-connection-closed").exists());
        std::fs::write(directory.path().join(".sift-savepoint-release"), b"release").unwrap();
        // The source has been closed, so its late ack cannot recreate host state.
        assert!(operation.await.unwrap().is_err());
        tokio::time::timeout(Duration::from_secs(5), async {
            while !directory.path().join(".sift-connection-closed").exists()
                || resources.snapshot(tenant).unwrap().usage.connections != 0
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert!(store.list_transactions(id).unwrap().is_empty());
        assert_eq!(process.active_work().await, 0);
        process.shutdown("complete").await.unwrap();
    }
}
