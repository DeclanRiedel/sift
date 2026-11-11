//! Bounded server-owned automatic review sources (ADR-207).
use super::*;
use sift_protocol::handshake::ExtensionCallerIdentity;
use std::collections::HashMap;
use std::sync::{OnceLock, Weak};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;
use uuid::Uuid;

const MAX_SOURCES: usize = 32;
const SOURCE_TTL: Duration = Duration::from_secs(300);

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ReviewRequest {
    pub caller: ExtensionCallerIdentity,
    pub proposal: Uuid,
    pub request: Uuid,
}

pub(crate) struct ReviewSources {
    entries: Mutex<HashMap<Uuid, Arc<ReviewSource>>>,
    slots: Arc<Semaphore>,
}
impl Default for ReviewSources {
    fn default() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            slots: Arc::new(Semaphore::new(MAX_SOURCES)),
        }
    }
}

pub(crate) struct ReviewSource {
    pub request: ReviewRequest,
    store: Weak<SessionStoreInner>,
    session: OnceLock<SessionId>,
    connection: OnceLock<ConnectionId>,
    execution: OnceLock<Arc<execution_control::ConnectionExecutionState>>,
    review: OnceLock<Uuid>,
    expires: Mutex<Instant>,
    pub preparation: tokio::sync::Mutex<()>,
    started: AtomicBool,
    slot: Option<OwnedSemaphorePermit>,
}

impl SessionStore {
    pub(crate) fn reserve_managed_review(
        &self,
        request: ReviewRequest,
    ) -> ApiResult<Arc<ReviewSource>> {
        let mut entries = self.inner.ai_review_sources.entries.lock().unwrap();
        if let Some(source) = entries.get(&request.request) {
            if source.request != request {
                return Err(ApiError::Forbidden(
                    "Review request identity changed".into(),
                ));
            }
            return Ok(source.clone());
        }
        let slot = self
            .inner
            .ai_review_sources
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                ApiError::BadRequest(
                    "Managed review sources are busy; let an existing review expire".into(),
                )
            })?;
        let source = Arc::new(ReviewSource {
            request,
            store: Arc::downgrade(&self.inner),
            session: OnceLock::new(),
            connection: OnceLock::new(),
            execution: OnceLock::new(),
            review: OnceLock::new(),
            expires: Mutex::new(Instant::now() + SOURCE_TTL),
            preparation: tokio::sync::Mutex::new(()),
            started: AtomicBool::new(false),
            slot: Some(slot),
        });
        entries.insert(source.request.request, source.clone());
        drop(entries);
        // Hold no strong source/store reference while sleeping. Expiry releases
        // only the registry's reference; native owners retain the original source.
        let weak = Arc::downgrade(&self.inner);
        let request_id = source.request.request;
        tokio::spawn(async move {
            let mut delay = SOURCE_TTL;
            loop {
                tokio::time::sleep(delay).await;
                let Some(inner) = weak.upgrade() else { return };
                let expired = {
                    let mut entries = inner.ai_review_sources.entries.lock().unwrap();
                    let Some(source) = entries.get(&request_id) else {
                        return;
                    };
                    delay = source
                        .expires
                        .lock()
                        .unwrap()
                        .saturating_duration_since(Instant::now());
                    if delay.is_zero() {
                        entries.remove(&request_id)
                    } else {
                        None
                    }
                };
                if expired.is_some() {
                    drop(expired);
                    return;
                }
            }
        });
        Ok(source)
    }

    /// A present but expired weak marker never becomes an ordinary session.
    pub(crate) fn with_managed_review_source(&self, source: Arc<ReviewSource>) -> Self {
        let mut store = self.clone();
        store.ai_review_owner = Some(source);
        store
    }
    pub(super) fn retain_managed_review_source(
        &self,
        session: SessionId,
    ) -> ApiResult<Option<Arc<ReviewSource>>> {
        let owner = self.managed_review_source(session)?;
        if owner.as_ref().is_some_and(|owner| {
            !self
                .ai_review_owner
                .as_ref()
                .is_some_and(|bound| Arc::ptr_eq(owner, bound))
        }) {
            return Err(ApiError::Forbidden(
                "Managed review source requires its captured AI owner".into(),
            ));
        }
        Ok(owner)
    }

    pub(crate) fn managed_review_source(
        &self,
        session: SessionId,
    ) -> ApiResult<Option<Arc<ReviewSource>>> {
        let session = self
            .inner
            .sessions
            .get(&session)
            .ok_or(ApiError::SessionNotFound(session))?;
        let marker = session.ai_review_source.lock().unwrap();
        marker
            .as_ref()
            .map(|weak| weak.upgrade().ok_or_else(unavailable))
            .transpose()
    }
}
impl ReviewSource {
    pub fn start(&self) -> ApiResult<()> {
        if self.started.swap(true, Ordering::AcqRel) {
            return Err(unavailable());
        }
        Ok(())
    }
    pub fn prepared(&self) -> Option<Uuid> {
        self.review.get().copied()
    }
    pub fn check_caller(&self, caller: &ExtensionCallerIdentity) -> ApiResult<()> {
        if &self.request.caller != caller {
            return Err(ApiError::Forbidden(
                "Review caller changed; request a fresh review".into(),
            ));
        }
        if Instant::now() >= *self.expires.lock().unwrap() {
            return Err(unavailable());
        }
        Ok(())
    }
    pub fn attach_session(
        self: &Arc<Self>,
        store: &SessionStore,
        session: SessionId,
    ) -> ApiResult<()> {
        self.session.set(session).map_err(|_| unavailable())?;
        let session = store
            .inner
            .sessions
            .get(&session)
            .ok_or(ApiError::SessionNotFound(session))?;
        *session.ai_review_source.lock().unwrap() = Some(Arc::downgrade(self));
        Ok(())
    }
    pub fn attach_connection(
        &self,
        store: &SessionStore,
        connection: ConnectionId,
    ) -> ApiResult<()> {
        let session = *self.session.get().ok_or_else(unavailable)?;
        let entry = store.conn_entry(session, connection)?;
        self.execution
            .set(entry.execution)
            .map_err(|_| unavailable())?;
        self.connection.set(connection).map_err(|_| unavailable())
    }
    pub fn finish(&self, review: &sift_protocol::AiDatabaseProposalReview) -> ApiResult<()> {
        self.review.set(review.id).map_err(|_| unavailable())?;
        let remaining = (review.expires_at - chrono::Utc::now())
            .to_std()
            .unwrap_or_default()
            .min(SOURCE_TTL);
        *self.expires.lock().unwrap() = Instant::now() + remaining;
        Ok(())
    }
}
impl Drop for ReviewSource {
    fn drop(&mut self) {
        let (Some(session), Some(inner)) = (self.session.get().copied(), self.store.upgrade())
        else {
            return;
        };
        let connection = self.connection.get().copied();
        let slot = self.slot.take();
        let execution = self.execution.get().cloned();
        let store = SessionStore {
            inner,
            csv_source: None,
            source_authority: None,
            ai_review_owner: None,
            csv_statement: None,
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                // No observer timeout here: retain admission through actual native
                // close. close_session alone only schedules detached cleanup.
                let _slot = slot;
                if let Some(connection) = connection {
                    let _ = store.close_connection_owned(session, connection).await;
                    // A revocation/close worker may already have removed the
                    // entry. Its physical close still owns this admission slot.
                    if let Some(execution) = execution {
                        execution.wait_closed().await;
                    }
                }
                let _ = store.close_session(session);
            });
        }
    }
}
fn unavailable() -> ApiError {
    ApiError::BadRequest(
        "Original automatic review source is unavailable; request a fresh review".into(),
    )
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
    fn fixture(
        driver: Arc<MockDriver>,
    ) -> (SessionStore, Arc<ReviewSource>, SessionStore, SessionId) {
        let registry = DriverRegistry::new();
        registry
            .providers()
            .replace([Arc::new(BuiltinProviderAdapter::new(driver)) as Arc<dyn DatabaseProvider>])
            .unwrap();
        let store = SessionStore::new(registry);
        let source = store
            .reserve_managed_review(ReviewRequest {
                caller: ExtensionCallerIdentity {
                    instance_id: "original".into(),
                    daemon_generation: "original".into(),
                    principal_id: 1,
                    auth_session_id: Some("original".into()),
                },
                proposal: Uuid::new_v4(),
                request: Uuid::new_v4(),
            })
            .unwrap();
        let session = store
            .open_session(OpenSessionRequest {
                tag: None,
                tenant_id: None,
            })
            .id;
        source.attach_session(&store, session).unwrap();
        let bound = store.with_managed_review_source(source.clone());
        (store, source, bound, session)
    }
    async fn open(store: &SessionStore, session: SessionId) -> ApiResult<ConnectionId> {
        store
            .open_connection(
                session,
                Engine::Postgres,
                ConnectionSpec {
                    host: "mock.invalid".into(),
                    port: None,
                    database: Some("mock".into()),
                    user: "mock".into(),
                    password: None,
                    ssl_mode: Some(sift_protocol::SslMode::Disable),
                    engine_specific: None,
                },
            )
            .await
            .map(|info| info.id)
    }
    fn expire(store: &SessionStore, source: &ReviewSource) {
        let expired = store
            .inner
            .ai_review_sources
            .entries
            .lock()
            .unwrap()
            .remove(&source.request.request);
        drop(expired);
    }

    #[tokio::test(start_paused = true)]
    async fn managed_source_expiry_closes_without_reclassifying_the_session() {
        let closed = Arc::new(Notify::new());
        let mock = Arc::new(
            MockDriver::builder()
                .engine(Engine::Postgres)
                .close_wait(closed.clone())
                .build(),
        );
        let (store, source, bound, session) = fixture(mock.clone());
        let connection = open(&bound, session).await.unwrap();
        source.attach_connection(&bound, connection).unwrap();
        drop(source);
        drop(bound);
        tokio::task::yield_now().await;
        tokio::time::advance(SOURCE_TTL).await;
        until(|| mock.invocations().contains(&"close")).await;
        assert!(
            store.managed_review_source(session).is_err(),
            "An expired weak owner cannot become an ordinary session"
        );
        assert_eq!(
            store.inner.ai_review_sources.slots.available_permits(),
            MAX_SOURCES - 1
        );
        closed.notify_one();
        until(|| store.inner.ai_review_sources.slots.available_permits() == MAX_SOURCES).await;
        assert!(store.session_info(session).is_err());
    }

    #[tokio::test]
    async fn managed_source_survives_observer_timeout_native_eof_and_close() {
        let completed = Arc::new(Notify::new());
        let closed = Arc::new(Notify::new());
        let mock = Arc::new(
            MockDriver::builder()
                .engine(Engine::Postgres)
                .close_wait(closed.clone())
                .build(),
        );
        let (store, source, bound, session) = fixture(mock.clone());
        let connection = open(&bound, session).await.unwrap();
        source.attach_connection(&bound, connection).unwrap();
        assert!(
            store.conn_entry(session, connection).is_err(),
            "An unbound request must never acquire a managed source"
        );
        let entry = bound.conn_entry(session, connection).unwrap();
        let captured = bound.bind_source(session, connection, entry).unwrap();
        drop(bound);
        let bound = captured;
        bound.set_request_timeout(Duration::from_millis(20));
        let native = completed.clone();
        let result = bound
            .run_bounded("retained review read", async move {
                native.notified().await;
                Ok(())
            })
            .await;
        assert!(
            matches!(result, Err(ApiError::Driver(error)) if error.code == Code::QueryTimedOut)
        );
        expire(&store, &source);
        drop(source);
        drop(bound);
        assert_eq!(
            store.inner.ai_review_sources.slots.available_permits(),
            MAX_SOURCES - 1
        );
        assert!(!mock.invocations().contains(&"close"));
        completed.notify_one();
        until(|| mock.invocations().contains(&"close")).await;
        assert_eq!(
            store.inner.ai_review_sources.slots.available_permits(),
            MAX_SOURCES - 1
        );
        closed.notify_one();
        until(|| store.inner.ai_review_sources.slots.available_permits() == MAX_SOURCES).await;
        assert!(store.session_info(session).is_err());
    }

    #[tokio::test]
    async fn managed_source_slot_waits_for_an_existing_revocation_close() {
        let closed = Arc::new(Notify::new());
        let mock = Arc::new(
            MockDriver::builder()
                .engine(Engine::Postgres)
                .close_wait(closed.clone())
                .build(),
        );
        let (store, source, bound, session) = fixture(mock.clone());
        let connection = open(&bound, session).await.unwrap();
        source.attach_connection(&bound, connection).unwrap();
        let revoker = store.clone();
        let revocation =
            tokio::spawn(async move { revoker.close_connection_owned(session, connection).await });
        until(|| mock.invocations().contains(&"close")).await;
        expire(&store, &source);
        drop(source);
        drop(bound);
        assert_eq!(
            store.inner.ai_review_sources.slots.available_permits(),
            MAX_SOURCES - 1
        );
        closed.notify_one();
        revocation.await.unwrap().unwrap();
        until(|| store.inner.ai_review_sources.slots.available_permits() == MAX_SOURCES).await;
        assert_eq!(
            mock.invocations()
                .iter()
                .filter(|&&call| call == "close")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn managed_source_retains_admission_through_late_open_cleanup() {
        let opened = Arc::new(Notify::new());
        let closed = Arc::new(Notify::new());
        let mock = Arc::new(
            MockDriver::builder()
                .engine(Engine::Postgres)
                .open_wait(opened.clone())
                .close_wait(closed.clone())
                .build(),
        );
        let (store, source, bound, session) = fixture(mock.clone());
        bound.set_request_timeout(Duration::from_millis(20));
        assert!(
            matches!(open(&bound, session).await, Err(ApiError::Driver(error)) if error.code == Code::QueryTimedOut)
        );
        expire(&store, &source);
        drop(source);
        drop(bound);
        assert_eq!(
            store.inner.ai_review_sources.slots.available_permits(),
            MAX_SOURCES - 1
        );
        opened.notify_one();
        until(|| mock.invocations().contains(&"close")).await;
        assert_eq!(
            store.inner.ai_review_sources.slots.available_permits(),
            MAX_SOURCES - 1
        );
        closed.notify_one();
        until(|| store.inner.ai_review_sources.slots.available_permits() == MAX_SOURCES).await;
        assert!(store.session_info(session).is_err());
    }
}
