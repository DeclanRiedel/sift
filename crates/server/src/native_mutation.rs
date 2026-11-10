//! Cooperative admission for an already bounded structured-mutation owner.
//! Deadlines close future dispatch; they never cancel issued native work.
use super::*;

#[derive(Default)]
struct State {
    closed: bool,
    admitted: bool,
}

pub(crate) struct Admission {
    deadline: tokio::time::Instant,
    state: Mutex<State>,
    guard: Option<Arc<dyn Fn() -> ApiResult<()> + Send + Sync>>,
    cancellation: Option<tokio_util::sync::CancellationToken>,
}

tokio::task_local! { static CURRENT: Option<Arc<Admission>>; }

impl Admission {
    pub(crate) fn new(timeout: Duration) -> Arc<Self> {
        Self::with_guard(timeout, None, None)
    }
    pub(crate) fn with_guard(
        timeout: Duration,
        guard: Option<Arc<dyn Fn() -> ApiResult<()> + Send + Sync>>,
        cancellation: Option<tokio_util::sync::CancellationToken>,
    ) -> Arc<Self> {
        Arc::new(Self {
            guard,
            cancellation,
            deadline: tokio::time::Instant::now() + timeout,
            state: Mutex::new(State::default()),
        })
    }
    pub(crate) fn deadline(&self) -> tokio::time::Instant {
        self.deadline
    }
    pub(crate) fn close(&self) {
        self.state.lock().unwrap().closed = true;
        if let Some(token) = &self.cancellation {
            token.cancel();
        }
    }
    pub(crate) fn cancellation(&self) -> Option<tokio_util::sync::CancellationToken> {
        self.cancellation.clone()
    }
    pub(crate) fn was_admitted(&self) -> bool {
        self.state.lock().unwrap().admitted
    }
    pub(crate) fn check(&self) -> ApiResult<()> {
        self.check_locked(&self.state.lock().unwrap())
    }
    fn check_locked(&self, state: &State) -> ApiResult<()> {
        if state.closed
            || tokio::time::Instant::now() >= self.deadline
            || self
                .cancellation
                .as_ref()
                .is_some_and(|token| token.is_cancelled())
        {
            Err(ApiError::Driver(DriverError::new(
                Code::QueryCanceled,
                "structured mutation deadline closed native admission",
            )))
        } else if let Some(guard) = &self.guard {
            guard()
        } else {
            Ok(())
        }
    }
}

pub(crate) fn current() -> Option<Arc<Admission>> {
    CURRENT.try_with(Clone::clone).ok().flatten()
}

struct Close(Arc<Admission>);
impl Drop for Close {
    fn drop(&mut self) {
        self.0.close();
    }
}
pub(crate) async fn scope<F: std::future::Future>(admission: Arc<Admission>, work: F) -> F::Output {
    let _close = Close(admission.clone());
    CURRENT.scope(Some(admission), work).await
}

/// Capture before spawning; do not close the owner when an individual worker exits.
pub(super) struct Context(Option<Arc<Admission>>);
impl Context {
    pub(super) fn capture() -> Self {
        Self(current())
    }
    pub(super) async fn run<T>(self, work: futures::future::BoxFuture<'static, T>) -> T {
        CURRENT.scope(self.0, work).await
    }
}

/// Keep the deadline check atomic with native observation and host end claims.
/// The closure contains no await or driver I/O.
pub(super) fn admit<T>(prepare: impl FnOnce() -> ApiResult<T>) -> ApiResult<T> {
    let Some(admission) = current() else {
        return prepare();
    };
    let mut state = admission.state.lock().unwrap();
    admission.check_locked(&state)?;
    let value = prepare()?;
    state.admitted = true;
    Ok(value)
}
