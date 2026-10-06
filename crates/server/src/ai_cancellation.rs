//! Bounded in-flight run cancellation; durable run state remains authoritative.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const MAX_ACTIVE_RUNS: usize = 128;
const MAX_CALLS_PER_RUN: usize = 8;

#[derive(Clone, Default)]
pub(crate) struct AiWorkRegistry(Arc<Mutex<HashMap<Uuid, Entry>>>);

struct Entry {
    token: CancellationToken,
    calls: usize,
}

pub(crate) struct AiWorkGuard {
    registry: AiWorkRegistry,
    run_id: Uuid,
    pub(crate) token: CancellationToken,
}

impl AiWorkRegistry {
    /// Callers must check durable running state after registration. This closes
    /// the race where Stop settles a run before its first guard is registered.
    pub(crate) fn register(&self, run_id: Uuid) -> Result<AiWorkGuard, &'static str> {
        let mut entries = self.0.lock().unwrap();
        if !entries.contains_key(&run_id) && entries.len() >= MAX_ACTIVE_RUNS {
            return Err("AI run concurrency limit reached");
        }
        let entry = entries.entry(run_id).or_insert_with(|| Entry {
            token: CancellationToken::new(),
            calls: 0,
        });
        if entry.calls >= MAX_CALLS_PER_RUN {
            return Err("AI tool concurrency limit reached");
        }
        entry.calls += 1;
        Ok(AiWorkGuard {
            registry: self.clone(),
            run_id,
            token: entry.token.child_token(),
        })
    }

    pub(crate) fn cancel(&self, run_id: Uuid) {
        if let Some(entry) = self.0.lock().unwrap().get(&run_id) {
            entry.token.cancel();
        }
    }
}

impl Drop for AiWorkGuard {
    fn drop(&mut self) {
        let mut entries = self.registry.0.lock().unwrap();
        if let Some(entry) = entries.get_mut(&self.run_id) {
            entry.calls -= 1;
            if entry.calls == 0 {
                entries.remove(&self.run_id);
            }
        }
    }
}

tokio::task_local! {
    static CURRENT_WORK: CancellationToken;
}

pub(crate) fn current_token() -> Option<CancellationToken> {
    CURRENT_WORK.try_with(Clone::clone).ok()
}

pub(crate) async fn scope<F: std::future::Future>(
    token: CancellationToken,
    future: F,
) -> F::Output {
    let guard = token.clone().drop_guard();
    let result = CURRENT_WORK.scope(token, future).await;
    guard.disarm();
    result
}

/// SELECT's execute/drain task owns a native cursor after execute returns.
/// Dropping an AI handler must abort that task and still cancel the real cursor.
pub(crate) struct QueryTaskGuard {
    abort: Option<tokio::task::AbortHandle>,
    slot: Arc<Mutex<Option<sift_protocol::CursorId>>>,
    sessions: crate::session::SessionStore,
    session: sift_protocol::SessionId,
    connection: sift_protocol::ConnectionId,
}

impl QueryTaskGuard {
    pub(crate) fn new(
        abort: tokio::task::AbortHandle,
        slot: Arc<Mutex<Option<sift_protocol::CursorId>>>,
        sessions: crate::session::SessionStore,
        session: sift_protocol::SessionId,
        connection: sift_protocol::ConnectionId,
    ) -> Self {
        Self {
            abort: Some(abort),
            slot,
            sessions,
            session,
            connection,
        }
    }

    pub(crate) fn disarm(&mut self) {
        self.abort = None;
    }
}

impl Drop for QueryTaskGuard {
    fn drop(&mut self) {
        if let Some(abort) = self.abort.take() {
            abort.abort();
            let slot = self.slot.clone();
            let sessions = self.sessions.clone();
            let session = self.session;
            let connection = self.connection;
            tokio::spawn(async move {
                let cursor = *slot.lock().unwrap();
                if let Some(cursor) = cursor {
                    sessions
                        .cancel_after_timeout(session, connection, cursor)
                        .await;
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelling_governed_reads_aborts_the_spawned_driver_future() {
        struct NotifyOnDrop(Option<tokio::sync::oneshot::Sender<()>>);
        impl Drop for NotifyOnDrop {
            fn drop(&mut self) {
                if let Some(sender) = self.0.take() {
                    let _ = sender.send(());
                }
            }
        }
        let sessions = crate::session::SessionStore::new(crate::registry::DriverRegistry::new());
        let token = CancellationToken::new();
        let cancellation = token.clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
        let work = tokio::spawn(async move {
            scope(
                token,
                sessions.run_bounded("AI test read", async move {
                    let _guard = NotifyOnDrop(Some(dropped_tx));
                    let _ = started_tx.send(());
                    std::future::pending::<Result<(), sift_protocol::DriverError>>().await
                }),
            )
            .await
        });
        started_rx.await.unwrap();
        cancellation.cancel();
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), work)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        tokio::time::timeout(std::time::Duration::from_secs(1), dropped_rx)
            .await
            .unwrap()
            .unwrap();
    }

    #[test]
    fn stop_reaches_all_registered_calls_and_guards_release_capacity() {
        let registry = AiWorkRegistry::default();
        let id = Uuid::new_v4();
        let first = registry.register(id).unwrap();
        let second = registry.register(id).unwrap();
        registry.cancel(id);
        assert!(first.token.is_cancelled());
        assert!(second.token.is_cancelled());
        drop(first);
        assert!(registry.register(id).unwrap().token.is_cancelled());
        drop(second);
        assert!(registry.0.lock().unwrap().is_empty());
        // No persistent cancellation marker is needed: after registration,
        // the HTTP caller rechecks durable state before any new I/O.
        assert!(!registry.register(id).unwrap().token.is_cancelled());
    }

    #[test]
    fn concurrency_is_bounded_per_run_and_globally() {
        let registry = AiWorkRegistry::default();
        let id = Uuid::new_v4();
        let calls = (0..MAX_CALLS_PER_RUN)
            .map(|_| registry.register(id).unwrap())
            .collect::<Vec<_>>();
        assert!(registry.register(id).is_err());
        drop(calls);
        let runs = (0..MAX_ACTIVE_RUNS)
            .map(|_| registry.register(Uuid::new_v4()).unwrap())
            .collect::<Vec<_>>();
        assert!(registry.register(Uuid::new_v4()).is_err());
        drop(runs);
        assert!(registry.0.lock().unwrap().is_empty());
    }
}
