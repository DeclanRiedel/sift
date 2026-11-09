//! Owned exclusive admission for stateful external estimated-plan captures.
use super::*;
impl SessionStore {
    pub(crate) async fn external_estimated_plan(
        &self,
        session: SessionId,
        connection: ConnectionId,
        request: sift_protocol::ExplainRequest,
        operation: sift_protocol::OperationKind,
    ) -> ApiResult<sift_protocol::ExplainResponse> {
        let source = self.conn_entry(session, connection)?;
        let store = self.bind_source(session, connection, source.clone())?;
        let timeout = self.request_timeout();
        let admission = if timeout.is_zero() {
            Duration::from_secs(5)
        } else {
            timeout.min(Duration::from_secs(5))
        };
        let deadline = tokio::time::Instant::now() + admission;
        let max_bytes = store.result_limits().1.min(1024 * 1024) as u32;
        let resources = store.reserve_query_resources(&source)?;
        self.transaction_work("external estimated plan", async move {
            let _resources = resources;
            let _gate =
                tokio::time::timeout_at(deadline, source.execution.gate.clone().write_owned())
                    .await
                    .map_err(|_| {
                        ApiError::Driver(DriverError::new(
                            Code::PoolExhausted,
                            "Estimated-plan admission expired without dispatch",
                        ))
                    })?;
            if tokio::time::Instant::now() >= deadline {
                return Err(ApiError::Driver(DriverError::new(
                    Code::PoolExhausted,
                    "Estimated-plan admission expired without dispatch",
                )));
            }
            store.authorize_connection_operation(
                session,
                connection,
                operation,
                Some(&request.sql),
                &[],
            )?;
            let entry = store.conn_entry(session, connection)?;
            if !Arc::ptr_eq(&entry.execution, &source.execution) {
                return Err(ApiError::Driver(DriverError::new(
                    Code::ConnectionInvalidated,
                    "Estimated-plan source was replaced",
                )));
            }
            if store.connection_has_transaction(session, connection)? {
                return Err(ApiError::BadRequest(
                    "External estimated capture requires no active managed transaction".into(),
                ));
            }
            entry
                .driver
                .estimated_plan(
                    entry.handle,
                    ExecuteRequest {
                        sql: request.sql,
                        params: request.params,
                        transform: None,
                    },
                    max_bytes,
                )
                .await
                .map_err(ApiError::from)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        registry::{DatabaseProvider, DriverRegistry},
        rpc_provider::RpcProvider,
    };
    use sift_extension_protocol::{ContributionId, ExtensionId, VersionRange, WireId};
    use sift_plugin_host::{ProcessSpec, SupervisedProcess, SupervisorLimits};
    use sift_protocol::{
        DialectId, ExplainRequest, ProviderCapability, ProviderDescriptor, ProviderId, ProviderRef,
    };
    mod provider_fixture {
        include!("../tests/support/estimated_provider.rs");
    }
    use provider_fixture::*;
    async fn entered(path: &std::path::Path) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !path.join(".sift-plan-entered").exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    #[ignore = "requires built conformance executable"]
    async fn external_plan_timeout_and_caller_loss_retain_gate_shutdown_and_single_audit() {
        use tower::ServiceExt;
        for timeout in [false, true] {
            let f = fixture("v3_hold", "sift/postgresql", 3).await;
            if timeout {
                f.store.set_request_timeout(Duration::from_millis(30));
            }
            let shutdown = crate::Shutdown::default();
            let router = crate::http::app(crate::http::AppState {
                sessions: f.store.clone(),
                rooms: Default::default(),
                shutdown: shutdown.clone(),
                auth: Default::default(),
                metadata: None,
            });
            let req = axum::http::Request::post(format!(
                "/v1/sessions/{}/connections/{}/explain",
                f.session, f.connection
            ))
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                serde_json::to_vec(&request(f.connection, "SELECT $1")).unwrap(),
            ))
            .unwrap();
            let task = tokio::spawn(async move { router.oneshot(req).await.unwrap() });
            entered(f._temp.path()).await;
            if timeout {
                assert_eq!(
                    task.await.unwrap().status(),
                    axum::http::StatusCode::GATEWAY_TIMEOUT
                );
            } else {
                task.abort();
                assert!(task.await.unwrap_err().is_cancelled());
            }
            let entry = f.store.conn_entry(f.session, f.connection).unwrap();
            assert!(entry.execution.gate.try_read().is_err());
            assert_eq!(shutdown.in_flight(), 1);
            std::fs::write(f._temp.path().join(".sift-plan-release"), b"release").unwrap();
            assert_eq!(shutdown.await_drain(Duration::from_secs(5)).await, 0);
            let operations = f
                .store
                .list_operations()
                .into_iter()
                .filter(|record| {
                    matches!(record.operation, sift_protocol::Operation::Explain { .. })
                })
                .collect::<Vec<_>>();
            assert_eq!(operations.len(), 1);
            assert_eq!(
                operations[0].status,
                if timeout {
                    sift_protocol::OperationStatus::Failed
                } else {
                    sift_protocol::OperationStatus::Succeeded
                }
            );
            assert!(entry.execution.gate.try_read().is_ok());
            f.process.shutdown("done").await.unwrap();
        }
    }
    #[tokio::test]
    #[ignore = "requires built conformance executable"]
    async fn external_plan_admission_expiry_replaced_source_and_revoked_authority_do_not_dispatch()
    {
        for scenario in ["expired", "replaced", "revoked"] {
            let mut f = fixture("v3_hold", "sift/postgresql", 3).await;
            let authorized = Arc::new(std::sync::atomic::AtomicBool::new(true));
            let authority = authorized.clone();
            f.store = f.store.with_source_authority(Arc::new(move || {
                if authority.load(Ordering::Acquire) {
                    Ok(())
                } else {
                    Err(ApiError::Forbidden("Plan source authority revoked".into()))
                }
            }));
            let entry = f.store.conn_entry(f.session, f.connection).unwrap();
            let gate = entry.execution.gate.clone().write_owned().await;
            if scenario == "expired" {
                f.store.set_request_timeout(Duration::from_millis(30));
            }
            let store = f.store.clone();
            let session = f.session;
            let connection = f.connection;
            let task = tokio::spawn(async move {
                crate::plan::explain(
                    &store,
                    session,
                    connection,
                    &request(connection, "SELECT $1"),
                )
                .await
            });
            while f.store.inner.driver_tasks.load(Ordering::Acquire) == 0 {
                tokio::task::yield_now().await;
            }
            if scenario == "replaced" {
                f.store
                    .with_session(&f.session, |session| {
                        session
                            .connections
                            .get_mut(&f.connection)
                            .unwrap()
                            .execution = Default::default()
                    })
                    .unwrap();
                drop(gate);
            } else if scenario == "revoked" {
                authorized.store(false, Ordering::Release);
                drop(gate);
            } else {
                tokio::time::sleep(Duration::from_millis(60)).await;
                drop(gate);
            }
            assert!(task.await.unwrap().is_err());
            assert!(!f._temp.path().join(".sift-plan-entered").exists());
            f.process.shutdown("done").await.unwrap();
        }
    }
}
