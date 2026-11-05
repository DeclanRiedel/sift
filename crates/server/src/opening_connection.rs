//! Ownership crosses the driver task boundary before a connection is registered.
use super::*;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

type Adopted = (
    RuntimeConnectionHandle,
    Option<crate::tailnet::TunnelLease>,
    Option<crate::resources::ResourceGuard>,
);
struct Pending {
    handle: Option<RuntimeConnectionHandle>,
    tunnel: Option<crate::tailnet::TunnelLease>,
    resource: Option<crate::resources::ResourceGuard>,
    _admission: OwnedSemaphorePermit,
}
pub(super) struct OpeningConnection {
    driver: RuntimeDriver,
    pending: Option<Pending>,
}
impl OpeningConnection {
    pub(super) fn new(
        driver: RuntimeDriver,
        resource: Option<crate::resources::ResourceGuard>,
        admission: Arc<Semaphore>,
    ) -> ApiResult<Self> {
        let permit = admission.try_acquire_owned().map_err(|_| {
            ApiError::Driver(DriverError::new(
                Code::PoolExhausted,
                "connection opening or late cleanup limit reached",
            ))
        })?;
        Ok(Self {
            driver,
            pending: Some(Pending {
                handle: None,
                tunnel: None,
                resource,
                _admission: permit,
            }),
        })
    }
    pub(super) fn completed(
        mut self,
        handle: RuntimeConnectionHandle,
        tunnel: Option<crate::tailnet::TunnelLease>,
    ) -> Self {
        let pending = self.pending.as_mut().expect("opening owns admission");
        pending.handle = Some(handle);
        pending.tunnel = tunnel;
        self
    }
    pub(super) fn adopt(mut self) -> Adopted {
        let mut pending = self.pending.take().expect("opening is adopted once");
        (
            pending.handle.take().expect("opening completed"),
            pending.tunnel.take(),
            pending.resource.take(),
        )
    }
}
impl Drop for OpeningConnection {
    fn drop(&mut self) {
        let Some(mut pending) = self.pending.take() else {
            return;
        };
        let Some(handle) = pending.handle.take() else {
            return;
        };
        let driver = self.driver.clone();
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::warn!("runtime unavailable for unregistered connection cleanup");
            return;
        };
        runtime.spawn(async move {
            let mut cleanup = tokio::spawn(async move {
                // Retain tunnel, tenant quota and opening admission until the
                // driver finishes close, including after the observer times out.
                let _pending = pending;
                driver.close(handle).await
            });
            match tokio::time::timeout(Duration::from_secs(5), &mut cleanup).await {
                Ok(Ok(Ok(()))) => {}
                Ok(_) => tracing::warn!("unregistered connection cleanup failed"),
                Err(_) => tracing::warn!(
                    "unregistered connection cleanup remains pending; admission retained"
                ),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{BuiltinProviderAdapter, DatabaseProvider};
    use sift_driver_api::mock::MockDriver;
    use tokio::sync::Notify;

    async fn until(mut ready: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while !ready() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
    }
    #[derive(Clone, Copy)]
    enum Abandon {
        Timeout,
        CallerDropped,
        SessionClosed,
    }
    #[tokio::test]
    async fn late_open_closes_once_and_retains_admission_until_native_cleanup() {
        for abandon in [
            Abandon::Timeout,
            Abandon::CallerDropped,
            Abandon::SessionClosed,
        ] {
            let opened = Arc::new(Notify::new());
            let closed = Arc::new(Notify::new());
            let mock = Arc::new(
                MockDriver::builder()
                    .engine(Engine::Postgres)
                    .open_wait(opened.clone())
                    .close_wait(closed.clone())
                    .build(),
            );
            let registry = DriverRegistry::builder().build();
            registry
                .providers()
                .replace([Arc::new(BuiltinProviderAdapter::new(mock.clone()))
                    as Arc<dyn DatabaseProvider>])
                .unwrap();
            let store = SessionStore::new(registry);
            store.set_request_timeout(if matches!(abandon, Abandon::Timeout) {
                Duration::from_millis(20)
            } else {
                Duration::from_secs(2)
            });
            let session = store
                .open_session(OpenSessionRequest {
                    tag: None,
                    tenant_id: None,
                })
                .id;
            let reserved = store
                .inner
                .opening_connections
                .clone()
                .try_acquire_many_owned((MAX_DRIVER_TASKS - 1) as u32)
                .unwrap();
            let worker = store.clone();
            let spec = || ConnectionSpec {
                host: "mock.invalid".into(),
                port: None,
                database: Some("mock".into()),
                user: "mock".into(),
                password: None,
                ssl_mode: Some(sift_protocol::SslMode::Disable),
                engine_specific: None,
            };
            let first_spec = spec();
            let task = tokio::spawn(async move {
                worker
                    .open_connection(session, Engine::Postgres, first_spec)
                    .await
            });
            until(|| mock.invocations().contains(&"open")).await;
            match abandon {
                Abandon::Timeout => assert!(
                    matches!(task.await.unwrap(),Err(ApiError::Driver(error)) if error.code==Code::QueryTimedOut)
                ),
                Abandon::CallerDropped => {
                    task.abort();
                    assert!(task.await.unwrap_err().is_cancelled());
                }
                Abandon::SessionClosed => {
                    store.close_session(session).unwrap();
                    opened.notify_one();
                    assert!(matches!(
                        task.await.unwrap(),
                        Err(ApiError::SessionNotFound(_))
                    ));
                }
            }
            assert_eq!(store.inner.opening_connections.available_permits(), 0);
            let live_session = if matches!(abandon, Abandon::SessionClosed) {
                store
                    .open_session(OpenSessionRequest {
                        tag: None,
                        tenant_id: None,
                    })
                    .id
            } else {
                session
            };
            assert!(
                matches!(store.open_connection(live_session,Engine::Postgres,spec()).await,Err(ApiError::Driver(error)) if error.code==Code::PoolExhausted)
            );
            opened.notify_one();
            until(|| mock.invocations().contains(&"close")).await;
            assert_eq!(
                store.inner.opening_connections.available_permits(),
                0,
                "cleanup must retain admission while close is pending"
            );
            assert!(store.list_connections(live_session).unwrap().is_empty());
            closed.notify_one();
            until(|| store.inner.opening_connections.available_permits() == 1).await;
            assert_eq!(
                mock.invocations()
                    .iter()
                    .filter(|&&method| method == "close")
                    .count(),
                1
            );
            drop(reserved);
            let adopted = store
                .open_connection(live_session, Engine::Postgres, spec())
                .await
                .unwrap();
            assert_eq!(
                store.inner.opening_connections.available_permits(),
                MAX_DRIVER_TASKS
            );
            assert_eq!(
                mock.invocations()
                    .iter()
                    .filter(|&&method| method == "close")
                    .count(),
                1,
                "registered handles must not be cleaned up early"
            );
            store
                .close_connection(live_session, adopted.id)
                .await
                .unwrap();
            assert_eq!(
                mock.invocations()
                    .iter()
                    .filter(|&&method| method == "close")
                    .count(),
                2
            );
        }
    }
}
