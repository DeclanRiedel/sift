//! Redacted native statement observations, independent of result delivery.
use super::*;
use tokio::sync::Notify;

tokio::task_local! { static CURRENT: Arc<NativeExecution>; }

pub(crate) fn current() -> Option<Arc<NativeExecution>> {
    CURRENT.try_with(Arc::clone).ok()
}
pub(crate) async fn scope<F: std::future::Future>(
    observation: Arc<NativeExecution>,
    work: F,
) -> F::Output {
    CURRENT.scope(observation, work).await
}

/// Capture before spawning an application drain task; task locals do not inherit.
pub(crate) fn inherit<F: std::future::Future>(
    work: F,
) -> impl std::future::Future<Output = F::Output> {
    let observation = current();
    async move {
        match observation {
            Some(observation) => scope(observation, work).await,
            None => work.await,
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct NativeObservation {
    pub attempted: bool,
    pub finished: bool,
    pub completed: bool,
    pub rows: Option<u64>,
    pub code: Option<String>,
    pub transaction_unknown: bool,
    /// Positive observation at native admission; absence is not autocommit proof.
    pub autocommit_observed: Option<bool>,
    closed: bool,
    done: bool,
    failed: bool,
}
#[derive(Default)]
pub(crate) struct NativeExecution {
    state: Mutex<NativeObservation>,
    changed: Notify,
}
impl NativeExecution {
    /// Current evidence, including native admission while its owner is pending.
    pub(crate) fn snapshot(&self) -> NativeObservation {
        self.state.lock().unwrap().clone()
    }
    pub fn dispatch(self: &Arc<Self>) -> ApiResult<NativeGuard> {
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return Err(ApiError::Driver(DriverError::new(
                Code::QueryTimedOut,
                "statement dispatch admission ended before native execution",
            )));
        }
        state.attempted = true;
        state.rows = Some(0);
        Ok(NativeGuard(self.clone()))
    }
    pub fn close_dispatch(&self) {
        let mut state = self.state.lock().unwrap();
        state.closed = true;
        if !state.attempted {
            state.finished = true;
        }
        drop(state);
        self.changed.notify_waiters();
    }
    pub fn page(&self, page: &Page) {
        let mut state = self.state.lock().unwrap();
        match page {
            Page::Done { affected_rows, .. } => {
                state.done = true;
                state.rows = state
                    .rows
                    .zip(*affected_rows)
                    .and_then(|(sum, count)| sum.checked_add(count));
            }
            Page::Error { error } => {
                state.failed = true;
                state.code = Some(crate::error::audit_code(&error.code));
            }
            _ => {}
        }
    }
    pub fn autocommit(&self, known: bool) {
        let mut state = self.state.lock().unwrap();
        state.autocommit_observed = Some(known);
        if !known {
            state.transaction_unknown = true;
        }
    }
    pub fn error(&self, error: &DriverError) {
        let mut state = self.state.lock().unwrap();
        state.failed = true;
        state.code = Some(crate::error::audit_code(&error.code));
    }
    fn finish(&self, eof: bool) {
        let mut state = self.state.lock().unwrap();
        state.completed = eof && state.done && !state.failed;
        state.finished = true;
        if !state.completed {
            state.rows = None;
        }
        drop(state);
        self.changed.notify_waiters();
    }
    pub async fn terminal(&self) -> NativeObservation {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let state = self.state.lock().unwrap().clone();
            if state.finished {
                return state;
            }
            changed.await;
        }
    }
}
pub(crate) struct NativeGuard(Arc<NativeExecution>);
impl NativeGuard {
    pub fn eof(self) {
        self.0.finish(true);
        // Drop is idempotent after explicit EOF.
    }
}
impl Drop for NativeGuard {
    fn drop(&mut self) {
        let finished = self.0.state.lock().unwrap().finished;
        if !finished {
            self.0.finish(false);
        }
    }
}
pub(crate) struct StartupGuard(pub Arc<NativeExecution>);
impl Drop for StartupGuard {
    fn drop(&mut self) {
        self.0.close_dispatch();
    }
}
