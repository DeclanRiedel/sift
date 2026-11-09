//! Listener ownership is independent of ordinary connection execution admission.
use super::*;
use crate::registry::ProviderNotificationStream;
use tokio::sync::{oneshot, OwnedRwLockReadGuard};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub(super) struct ListenerOwners {
    gate: Arc<tokio::sync::RwLock<()>>,
    cancellation: std::sync::Mutex<CancellationToken>,
    stop: tokio::sync::Mutex<()>,
}
impl ListenerOwners {
    async fn lease(&self) -> (OwnedRwLockReadGuard<()>, CancellationToken) {
        let guard = self.gate.clone().read_owned().await;
        let cancellation = self
            .cancellation
            .lock()
            .expect("listener cancellation poisoned")
            .clone();
        (guard, cancellation)
    }
    pub(super) async fn stop(&self) {
        let _stop = self.stop.lock().await;
        self.cancellation
            .lock()
            .expect("listener cancellation poisoned")
            .cancel();
        let _drained = self.gate.clone().write_owned().await;
        *self
            .cancellation
            .lock()
            .expect("listener cancellation poisoned") = CancellationToken::new();
    }
}

impl SessionStore {
    /// Historical name retained for Rust callers; declared external families are
    /// dialect-neutral and never build SQL in this host path.
    pub async fn listen_pg(
        &self,
        session: SessionId,
        connection: ConnectionId,
        channels: Vec<String>,
    ) -> ApiResult<ProviderNotificationStream> {
        let source = self.authorize_connection_operation(
            session,
            connection,
            sift_protocol::OperationKind::Listen,
            None,
            &[],
        )?;
        if !sift_extension_protocol::valid_notification_channels(&channels) {
            return Err(ApiError::BadRequest(
                "Listen requires 1..16 distinct channels of 1..63 UTF-8 bytes without controls"
                    .into(),
            ));
        }
        let store = self.bind_source(session, connection, source.clone())?;
        let resources = store.reserve_query_resources(&source)?;
        let retained = store
            .retained_byte_context(&source)
            .map(|(manager, tenant)| {
                manager.reserve(
                    tenant,
                    sift_protocol::TenantResource::RetainedResultBytes,
                    4 * 1024 * 1024,
                )
            })
            .transpose()?;
        let permit = store.reserve_driver_work()?;
        let timeout = self.request_timeout();
        let admission = if timeout.is_zero() {
            Duration::from_secs(5)
        } else {
            timeout.min(Duration::from_secs(5))
        };
        let deadline = tokio::time::Instant::now() + admission;
        let query_guard = crate::shutdown::current_query_guard();
        let (sender, notifications) = tokio::sync::mpsc::channel(8);
        let (done, settled) = oneshot::channel();
        let (ready, startup) = oneshot::channel();
        tokio::spawn(crate::shutdown::scope_query(query_guard, async move {
            let _resources = resources;
            let _retained = retained;
            let _permit = permit;
            let start = async {
                let gate =
                    tokio::time::timeout_at(deadline, source.execution.gate.clone().read_owned())
                        .await
                        .map_err(|_| Self::admission_expired())?;
                Self::check_admission_deadline(deadline)?;
                store.authorize_connection_operation(
                    session,
                    connection,
                    sift_protocol::OperationKind::Listen,
                    None,
                    &[],
                )?;
                store.conn_entry(session, connection)?;
                let (lease, cancellation) = source.execution.listeners.lease().await;
                store.authorize_connection_operation(
                    session,
                    connection,
                    sift_protocol::OperationKind::Listen,
                    None,
                    &[],
                )?;
                store.conn_entry(session, connection)?;
                if sender.is_closed() || cancellation.is_cancelled() {
                    return Err(ApiError::Driver(DriverError::new(
                        Code::QueryCanceled,
                        "Notification startup observer closed before dispatch",
                    )));
                }
                let stream = source
                    .driver
                    .listen_notifications(source.handle.clone(), channels)
                    .await?;
                drop(gate);
                Ok::<_, ApiError>((stream, lease, cancellation))
            }
            .await;
            let (mut stream, _lease, cancellation) = match start {
                Ok(start) => start,
                Err(error) => {
                    let _ = ready.send(Err(error));
                    let _ = done.send(Ok(()));
                    return;
                }
            };
            let _ = ready.send(Ok(()));
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut pending = None;
            let mut observed_error = None;
            loop {
                tokio::select! {
                    biased;
                    _ = sender.closed() => break,
                    _ = cancellation.cancelled() => break,
                    _ = tick.tick() => {
                        let valid = store.authorize_connection_operation(session, connection,
                            sift_protocol::OperationKind::Listen, None, &[])
                            .and_then(|_| store.conn_entry(session, connection));
                        if valid.is_err() {
                            observed_error = Some(DriverError::new(Code::ConnectionInvalidated,
                                "Notification source or authorization changed"));
                            break;
                        }
                    }
                    permit = sender.reserve(), if pending.is_some() => {
                        match permit {
                            Ok(permit) => permit.send(pending.take().expect("guarded pending notification")),
                            Err(_) => break,
                        }
                    }
                    event = stream.notifications.recv(), if pending.is_none() => {
                        match event {
                            Some(Ok(event)) => pending = Some(Ok(event)),
                            Some(Err(error)) => { observed_error = Some(error); break; }
                            None => break,
                        }
                    }
                }
            }
            drop(stream.notifications);
            let result = stream.settled.await.unwrap_or_else(|_| {
                Err(DriverError::new(
                    Code::ConnectionInvalidated,
                    "Notification cleanup owner failed",
                ))
            });
            let result = match (result, observed_error) {
                (Ok(()), Some(error)) => Err(error),
                (result, _) => result,
            };
            if let Err(error) = &result {
                let _ = sender.try_send(Err(error.clone()));
            }
            let _ = done.send(result);
        }));
        let ready = if timeout.is_zero() {
            startup.await
        } else {
            tokio::time::timeout(timeout, startup)
                .await
                .map_err(|_| timeout_error("listen startup"))?
        };
        ready.map_err(|_| ApiError::Internal("Listener startup owner failed".into()))??;
        Ok(ProviderNotificationStream {
            notifications,
            settled,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        registry::{DatabaseProvider, DriverRegistry},
        RpcProvider,
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
            while !path.join(".sift-listen-entered").exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    #[ignore = "requires built conformance executable"]
    async fn notification_startup_observer_loss_keeps_cleanup_owner_and_reconnect_drains_listeners()
    {
        for timeout in [false, true] {
            let f = fixture("v4_listen_hold", "sift/postgresql", 4).await;
            if timeout {
                f.store.set_request_timeout(Duration::from_millis(30));
            }
            let shutdown = crate::Shutdown::default();
            let guard = Arc::new(shutdown.track_query());
            let store = f.store.clone();
            let session = f.session;
            let connection = f.connection;
            let task = tokio::spawn(crate::shutdown::scope_query(Some(guard), async move {
                store
                    .listen_pg(session, connection, vec!["changes".into()])
                    .await
            }));
            entered(f._temp.path()).await;
            if timeout {
                assert!(task.await.unwrap().is_err());
            } else {
                task.abort();
                assert!(matches!(task.await, Err(error) if error.is_cancelled()));
            }
            assert_eq!(shutdown.in_flight(), 1);
            std::fs::write(f._temp.path().join(".sift-listen-release"), b"release").unwrap();
            assert_eq!(shutdown.await_drain(Duration::from_secs(8)).await, 0);
            assert!(f._temp.path().join(".sift-unlisten-stopped").exists());
            assert_eq!(
                f.process.health().await,
                sift_plugin_host::GenerationHealth::Ready
            );
            f.process.shutdown("done").await.unwrap();
        }
        let f = fixture("v4_quiet", "sift/postgresql", 4).await;
        let old = f.store.conn_entry(f.session, f.connection).unwrap();
        let mut stream = f
            .store
            .listen_pg(f.session, f.connection, vec!["changes".into()])
            .await
            .unwrap();
        stream.notifications.recv().await.unwrap().unwrap();
        assert!(old.execution.gate.try_write().is_ok());
        f.store.reconnect(f.session, f.connection).await.unwrap();
        assert!(f._temp.path().join(".sift-unlisten-stopped").exists());
        assert!(f._temp.path().join(".sift-native-close").exists());
        assert!(!Arc::ptr_eq(
            &old.execution,
            &f.store
                .conn_entry(f.session, f.connection)
                .unwrap()
                .execution
        ));
        drop(stream.notifications);
        let _ = stream.settled.await.unwrap();
        let plan = crate::plan::explain(
            &f.store,
            f.session,
            f.connection,
            &request(f.connection, "SELECT $1"),
        )
        .await
        .unwrap();
        assert!(!plan.analyzed);
        assert_eq!(
            f.process.health().await,
            sift_plugin_host::GenerationHealth::Ready
        );
        f.process.shutdown("done").await.unwrap();
    }
    #[tokio::test]
    #[ignore = "requires built conformance executable"]
    async fn notification_admission_rechecks_deadline_source_and_authority_before_dispatch() {
        for scenario in ["expired", "replaced", "revoked"] {
            let mut f = fixture("v4_listen_hold", "sift/postgresql", 4).await;
            let authorized = Arc::new(AtomicBool::new(true));
            let authority = authorized.clone();
            f.store = f.store.with_source_authority(Arc::new(move || {
                if authority.load(Ordering::Acquire) {
                    Ok(())
                } else {
                    Err(ApiError::Forbidden("Notification authority revoked".into()))
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
                store
                    .listen_pg(session, connection, vec!["changes".into()])
                    .await
            });
            tokio::time::timeout(Duration::from_secs(5), async {
                while f.store.inner.driver_tasks.load(Ordering::Acquire) == 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            match scenario {
                "expired" => tokio::time::sleep(Duration::from_millis(60)).await,
                "replaced" => {
                    f.store
                        .with_session(&f.session, |session| {
                            session
                                .connections
                                .get_mut(&f.connection)
                                .unwrap()
                                .execution = Default::default()
                        })
                        .unwrap();
                }
                _ => authorized.store(false, Ordering::Release),
            }
            drop(gate);
            assert!(task.await.unwrap().is_err());
            assert!(!f._temp.path().join(".sift-listen-entered").exists());
            f.process.shutdown("done").await.unwrap();
        }
    }
    #[tokio::test]
    #[ignore = "requires built conformance executable"]
    async fn notification_authority_revocation_settles_even_while_output_is_backpressured() {
        let mut f = fixture("v4_flood", "sift/postgresql", 4).await;
        let allowed = Arc::new(AtomicBool::new(true));
        let authority = allowed.clone();
        f.store = f.store.with_source_authority(Arc::new(move || {
            if authority.load(Ordering::Acquire) {
                Ok(())
            } else {
                Err(ApiError::Forbidden("Notification source revoked".into()))
            }
        }));
        let mut stream = f
            .store
            .listen_pg(f.session, f.connection, vec!["changes".into()])
            .await
            .unwrap();
        stream.notifications.recv().await.unwrap().unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        allowed.store(false, Ordering::Release);
        assert!(tokio::time::timeout(Duration::from_secs(8), stream.settled)
            .await
            .unwrap()
            .unwrap()
            .is_err());
        assert!(f._temp.path().join(".sift-unlisten-stopped").exists());
        assert_eq!(
            f.process.health().await,
            sift_plugin_host::GenerationHealth::Ready
        );
        f.process.shutdown("done").await.unwrap();
    }
}
