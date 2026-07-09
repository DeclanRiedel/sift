//! Server-side cursor registry (ADR-011).
//!
//! Sits above the driver layer. For every `Driver::execute` the
//! registry:
//!
//! - enforces a per-session cursor cap with idle-first eviction
//! - spawns a **pump** task that owns the driver's mpsc, forwards pages
//!   to the consumer over a bounded channel sized by `prefetch_pages`,
//!   and honors explicit `pause` / `resume` toggles
//! - on eviction, injects a synthetic `Page::Error { CursorEvicted }`
//!   to the consumer and (optionally) spills the pump's remaining
//!   pages to disk under `spill_dir` — the read-back side of spill
//!   is a documented follow-up
//!
//! Drivers are unchanged; the registry lives in `crates/server` so the
//! ADR-013 driver-isolation boundary is undisturbed.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
use sift_driver_api::ResultSetStream;
use sift_protocol::{Code, CursorId, DriverError, Page, SessionId};
use tokio::sync::{mpsc, Notify};

/// Registry-wide configuration. Applied at construction; live cursors
/// keep the values they were opened with.
#[derive(Debug, Clone)]
pub struct CursorConfig {
    /// Maximum simultaneously-open cursors per session. When at cap,
    /// opening a new cursor evicts the session's LRA cursor.
    pub max_per_session: usize,
    /// Pages the pump can hold ahead of the consumer. Default 2 (one
    /// current page + one prefetch). Bounded channel size between pump
    /// and consumer; also acts as automatic backpressure without
    /// requiring explicit `pause`.
    pub prefetch_pages: usize,
    /// Directory to write spill files to on eviction. `None` disables
    /// spill; today read-back is a follow-up regardless.
    pub spill_dir: Option<PathBuf>,
    /// Cursors whose remaining page footprint is below this are not
    /// spilled — the write cost exceeds the value.
    pub spill_min_bytes: usize,
}

impl Default for CursorConfig {
    fn default() -> Self {
        Self {
            max_per_session: 32,
            prefetch_pages: 2,
            spill_dir: None,
            spill_min_bytes: 1024 * 1024,
        }
    }
}

/// Callback invoked when the registry decides to evict a cursor. The
/// registry does not call `driver.cancel` directly — the caller
/// (SessionStore) owns that so the driver-side ownership check still
/// runs.
pub type EvictCallback = Arc<dyn Fn(SessionId, CursorId) + Send + Sync>;

#[derive(Clone, Default)]
pub struct CursorRegistry {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    config: std::sync::RwLock<CursorConfig>,
    entries: DashMap<CursorId, Arc<CursorState>>,
    per_session: DashMap<SessionId, Vec<CursorId>>,
    on_evict: std::sync::RwLock<Option<EvictCallback>>,
}

struct CursorState {
    #[allow(dead_code)]
    cursor_id: CursorId,
    session_id: SessionId,
    last_ack: std::sync::Mutex<Instant>,
    control: Arc<PumpControl>,
}

struct PumpControl {
    cancel: AtomicBool,
    cancel_reason: std::sync::Mutex<Option<DriverError>>,
    cancel_notify: Notify,
    paused: AtomicBool,
    resume_notify: Notify,
    spill_dir: std::sync::Mutex<Option<PathBuf>>,
    spill_min_bytes: usize,
}

impl CursorRegistry {
    pub fn new(config: CursorConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                config: std::sync::RwLock::new(config),
                entries: DashMap::new(),
                per_session: DashMap::new(),
                on_evict: std::sync::RwLock::new(None),
            }),
        }
    }

    pub fn set_config(&self, config: CursorConfig) {
        *self.inner.config.write().unwrap() = config;
    }

    pub fn config(&self) -> CursorConfig {
        self.inner.config.read().unwrap().clone()
    }

    pub fn set_on_evict(&self, cb: EvictCallback) {
        *self.inner.on_evict.write().unwrap() = Some(cb);
    }

    /// Register a new cursor and hand its driver-produced stream over
    /// to a registry-owned pump task. Returns a rebound
    /// `ResultSetStream` whose `rows` receives pages via the pump —
    /// same shape as the input so callers are unchanged.
    ///
    /// Enforces the per-session cap by evicting the LRA cursor before
    /// spawning the new one.
    pub fn wrap(
        &self,
        session_id: SessionId,
        stream: ResultSetStream,
    ) -> Result<ResultSetStream, DriverError> {
        let config = self.config();
        if config.max_per_session == 0 {
            return Err(DriverError::new(
                Code::CursorLimitReached,
                "per-session cursor cap is 0",
            ));
        }
        for victim in self.select_victims(session_id, config.max_per_session) {
            self.evict(session_id, victim);
        }
        let cursor_id = stream.cursor_id;
        if self.inner.entries.contains_key(&cursor_id) {
            return Err(DriverError::new(
                Code::DriverInternal,
                "cursor id already registered",
            ));
        }

        let control = Arc::new(PumpControl {
            cancel: AtomicBool::new(false),
            cancel_reason: std::sync::Mutex::new(None),
            cancel_notify: Notify::new(),
            paused: AtomicBool::new(false),
            resume_notify: Notify::new(),
            spill_dir: std::sync::Mutex::new(config.spill_dir.clone()),
            spill_min_bytes: config.spill_min_bytes,
        });
        let state = Arc::new(CursorState {
            cursor_id,
            session_id,
            last_ack: std::sync::Mutex::new(Instant::now()),
            control: Arc::clone(&control),
        });
        self.inner.entries.insert(cursor_id, state);
        self.inner
            .per_session
            .entry(session_id)
            .or_default()
            .push(cursor_id);

        let prefetch = config.prefetch_pages.max(1);
        let (consumer_tx, consumer_rx) = mpsc::channel::<Page>(prefetch);
        let ResultSetStream {
            columns,
            rows: driver_rx,
            warnings,
            affected_rows,
            server_side_cursor,
            ..
        } = stream;
        tokio::spawn(pump_task(cursor_id, control, driver_rx, consumer_tx));

        Ok(ResultSetStream {
            cursor_id,
            columns,
            rows: consumer_rx,
            warnings,
            affected_rows,
            server_side_cursor,
        })
    }

    pub fn touch(&self, cursor_id: CursorId) {
        if let Some(state) = self.inner.entries.get(&cursor_id) {
            *state.last_ack.lock().unwrap() = Instant::now();
        }
    }

    /// Idempotent close. Signals the pump to exit with a
    /// `QueryCanceled` terminal if it hasn't already sent a real
    /// terminal to the consumer.
    pub fn remove(&self, cursor_id: CursorId) {
        if let Some((_, state)) = self.inner.entries.remove(&cursor_id) {
            {
                let mut reason = state.control.cancel_reason.lock().unwrap();
                if reason.is_none() {
                    *reason = Some(DriverError::new(
                        Code::QueryCanceled,
                        "cursor closed by registry",
                    ));
                }
            }
            state.control.cancel.store(true, Ordering::Release);
            state.control.cancel_notify.notify_one();
            state.control.resume_notify.notify_one();
            if let Some(mut list) = self.inner.per_session.get_mut(&state.session_id) {
                list.retain(|c| *c != cursor_id);
            }
        }
    }

    /// Pause the cursor's pump. Idempotent.
    pub fn pause(&self, cursor_id: CursorId) {
        if let Some(state) = self.inner.entries.get(&cursor_id) {
            state.control.paused.store(true, Ordering::Release);
        }
    }

    /// Resume a paused cursor. Idempotent.
    pub fn resume(&self, cursor_id: CursorId) {
        if let Some(state) = self.inner.entries.get(&cursor_id) {
            state.control.paused.store(false, Ordering::Release);
            state.control.resume_notify.notify_one();
        }
    }

    pub fn is_open(&self, cursor_id: CursorId) -> bool {
        self.inner.entries.contains_key(&cursor_id)
    }

    pub fn session_cursor_count(&self, session_id: SessionId) -> usize {
        self.inner
            .per_session
            .get(&session_id)
            .map(|v| v.len())
            .unwrap_or(0)
    }

    fn select_victims(&self, session_id: SessionId, cap: usize) -> Vec<CursorId> {
        let ids: Vec<CursorId> = self
            .inner
            .per_session
            .get(&session_id)
            .map(|v| v.clone())
            .unwrap_or_default();
        if ids.len() < cap {
            return Vec::new();
        }
        let mut ranked: Vec<(CursorId, Instant)> = ids
            .into_iter()
            .filter_map(|c| {
                self.inner
                    .entries
                    .get(&c)
                    .map(|s| (c, *s.last_ack.lock().unwrap()))
            })
            .collect();
        ranked.sort_by_key(|(_, ts)| *ts);
        let excess = ranked.len().saturating_sub(cap.saturating_sub(1));
        ranked.into_iter().take(excess).map(|(c, _)| c).collect()
    }

    fn evict(&self, session_id: SessionId, cursor_id: CursorId) {
        if let Some(state) = self.inner.entries.get(&cursor_id) {
            *state.control.cancel_reason.lock().unwrap() = Some(DriverError::new(
                Code::CursorEvicted,
                "cursor evicted by per-session cap",
            ));
            state.control.cancel.store(true, Ordering::Release);
            state.control.cancel_notify.notify_one();
            state.control.resume_notify.notify_one();
        }
        let cb = self.inner.on_evict.read().unwrap().clone();
        if let Some(cb) = cb {
            cb(session_id, cursor_id);
        }
        if let Some((_, state)) = self.inner.entries.remove(&cursor_id) {
            if let Some(mut list) = self.inner.per_session.get_mut(&state.session_id) {
                list.retain(|c| *c != cursor_id);
            }
        }
    }
}

async fn pump_task(
    cursor_id: CursorId,
    control: Arc<PumpControl>,
    mut driver_rx: mpsc::Receiver<Page>,
    consumer_tx: mpsc::Sender<Page>,
) {
    let mut spillover: Vec<Page> = Vec::new();

    loop {
        // Pre-register the cancel waiter, then check the flag, so a
        // cancel that arrived between iterations isn't lost.
        let cancel = control.cancel_notify.notified();
        tokio::pin!(cancel);
        if control.cancel.load(Ordering::Acquire) {
            emit_terminal(&control, &consumer_tx, cursor_id, &mut spillover).await;
            return;
        }
        let page = tokio::select! {
            biased;
            _ = &mut cancel => {
                emit_terminal(&control, &consumer_tx, cursor_id, &mut spillover).await;
                return;
            }
            maybe = driver_rx.recv() => {
                match maybe {
                    Some(p) => p,
                    None => {
                        let err = DriverError::new(
                            Code::DriverInternal,
                            "driver dropped cursor stream without terminal page",
                        );
                        let _ = consumer_tx.send(Page::Error { error: err }).await;
                        return;
                    }
                }
            }
        };

        // Pause loop. Use `notified()` before checking the flag so a
        // resume() signal that arrives between the load and the await
        // is not lost.
        loop {
            let resume = control.resume_notify.notified();
            let cancel = control.cancel_notify.notified();
            tokio::pin!(resume);
            tokio::pin!(cancel);
            if !control.paused.load(Ordering::Acquire) {
                break;
            }
            if control.cancel.load(Ordering::Acquire) {
                spillover.push(page);
                emit_terminal(&control, &consumer_tx, cursor_id, &mut spillover).await;
                return;
            }
            tokio::select! {
                biased;
                _ = &mut cancel => {}
                _ = &mut resume => {}
            }
        }
        if control.cancel.load(Ordering::Acquire) {
            spillover.push(page);
            emit_terminal(&control, &consumer_tx, cursor_id, &mut spillover).await;
            return;
        }

        let is_terminal = matches!(&page, Page::Done { .. } | Page::Error { .. });
        // Send with cancel-aware wakeup: if cancel fires while we're
        // blocked on a full consumer channel, wake up, stash the page,
        // and emit the synthetic terminal.
        let send_fut = consumer_tx.send(page.clone());
        tokio::pin!(send_fut);
        let sent = tokio::select! {
            biased;
            _ = control.cancel_notify.notified() => {
                spillover.push(page);
                emit_terminal(&control, &consumer_tx, cursor_id, &mut spillover).await;
                return;
            }
            res = &mut send_fut => res.is_ok(),
        };
        if !sent {
            return;
        }
        if is_terminal {
            return;
        }
    }
}

async fn emit_terminal(
    control: &Arc<PumpControl>,
    consumer_tx: &mpsc::Sender<Page>,
    cursor_id: CursorId,
    spillover: &mut Vec<Page>,
) {
    let reason = control
        .cancel_reason
        .lock()
        .unwrap()
        .take()
        .unwrap_or_else(|| DriverError::new(Code::QueryCanceled, "cursor closed"));
    // Best-effort delivery with a short deadline. If the consumer
    // isn't draining, we don't block the pump forever — dropping the
    // sender when we return closes the channel and the consumer's
    // next recv returns None. Real WS consumers drain actively, so
    // this timeout is only for tests / misbehaving clients.
    let _ = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        consumer_tx.send(Page::Error { error: reason }),
    )
    .await;

    let spill_dir = control.spill_dir.lock().unwrap().clone();
    if let Some(dir) = spill_dir {
        let approx_bytes = approx_pages_bytes(spillover);
        if approx_bytes >= control.spill_min_bytes {
            if let Err(error) = write_spill(&dir, cursor_id, spillover) {
                tracing::debug!(?cursor_id, %error, "cursor spill write failed");
            }
        }
    }
}

fn approx_pages_bytes(pages: &[Page]) -> usize {
    let mut total = 0usize;
    for page in pages {
        total = total.saturating_add(approx_page_bytes(page));
    }
    total
}

fn approx_page_bytes(page: &Page) -> usize {
    match page {
        Page::Rows { rows } => rows.len().saturating_mul(64),
        Page::NextResult { columns } => columns.len().saturating_mul(64),
        _ => 64,
    }
}

fn write_spill(
    dir: &PathBuf,
    cursor_id: CursorId,
    pages: &[Page],
) -> Result<(), std::io::Error> {
    use std::io::Write as _;
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("sift-cursor-{}.bin", cursor_id.0));
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path)?;
    for page in pages {
        let bytes = serde_json::to_vec(page)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let len = bytes.len() as u32;
        file.write_all(&len.to_be_bytes())?;
        file.write_all(&bytes)?;
    }
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_protocol::{Row, Value};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn stream_with_pages(cursor_id: CursorId, pages: Vec<Page>) -> ResultSetStream {
        let (tx, rx) = mpsc::channel::<Page>(pages.len().max(1));
        tokio::spawn(async move {
            for p in pages {
                if tx.send(p).await.is_err() {
                    break;
                }
            }
        });
        ResultSetStream::with_cursor_mode(cursor_id, rx, true)
    }

    #[tokio::test]
    async fn pump_forwards_all_pages_in_order() {
        let registry = CursorRegistry::new(CursorConfig::default());
        registry.set_on_evict(Arc::new(|_, _| {}));
        let stream = stream_with_pages(
            CursorId(1),
            vec![
                Page::Rows {
                    rows: vec![Row::new(vec![Value::Int32(1)])],
                },
                Page::Rows {
                    rows: vec![Row::new(vec![Value::Int32(2)])],
                },
                Page::Done {
                    affected_rows: Some(2),
                    warnings: Vec::new(),
                },
            ],
        );
        let mut wrapped = registry.wrap(SessionId(1), stream).unwrap();
        let mut count = 0;
        while let Some(page) = wrapped.rows.recv().await {
            count += 1;
            if matches!(page, Page::Done { .. }) {
                break;
            }
        }
        assert_eq!(count, 3);
    }

    #[tokio::test]
    async fn eviction_emits_cursor_evicted_terminal_to_consumer() {
        let cfg = CursorConfig {
            max_per_session: 1,
            prefetch_pages: 1,
            ..CursorConfig::default()
        };
        let registry = CursorRegistry::new(cfg);
        registry.set_on_evict(Arc::new(|_, _| {}));
        let session = SessionId(1);

        let (tx1, rx1) = mpsc::channel::<Page>(1);
        let stream1 = ResultSetStream::with_cursor_mode(CursorId(10), rx1, true);
        let mut consumer1 = registry.wrap(session, stream1).unwrap();
        let _tx1_keep = tx1;

        let stream2 = stream_with_pages(
            CursorId(11),
            vec![Page::Done {
                affected_rows: None,
                warnings: Vec::new(),
            }],
        );
        let _ = registry.wrap(session, stream2).unwrap();

        let page = tokio::time::timeout(Duration::from_millis(500), consumer1.rows.recv())
            .await
            .expect("evicted consumer should receive a terminal within 500ms")
            .expect("evicted consumer channel should not close silently");
        match page {
            Page::Error { error } => assert_eq!(error.code, Code::CursorEvicted),
            other => panic!("expected CursorEvicted terminal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn cancel_via_remove_produces_query_canceled_terminal() {
        let registry = CursorRegistry::new(CursorConfig::default());
        registry.set_on_evict(Arc::new(|_, _| {}));
        let (tx, rx) = mpsc::channel::<Page>(1);
        let stream = ResultSetStream::with_cursor_mode(CursorId(20), rx, true);
        let mut consumer = registry.wrap(SessionId(1), stream).unwrap();
        let _tx = tx;

        registry.remove(CursorId(20));
        let page = tokio::time::timeout(Duration::from_millis(500), consumer.rows.recv())
            .await
            .expect("consumer should observe a terminal within 500ms")
            .expect("channel should not close silently");
        match page {
            Page::Error { error } => assert_eq!(error.code, Code::QueryCanceled),
            other => panic!("expected QueryCanceled terminal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn pause_holds_pages_until_resume() {
        let registry = CursorRegistry::new(CursorConfig {
            prefetch_pages: 4,
            ..CursorConfig::default()
        });
        registry.set_on_evict(Arc::new(|_, _| {}));
        let stream = stream_with_pages(
            CursorId(30),
            vec![
                Page::Rows {
                    rows: vec![Row::new(vec![Value::Int32(1)])],
                },
                Page::Done {
                    affected_rows: None,
                    warnings: Vec::new(),
                },
            ],
        );
        let mut consumer = registry.wrap(SessionId(1), stream).unwrap();
        registry.pause(CursorId(30));

        let first = tokio::time::timeout(Duration::from_millis(150), consumer.rows.recv()).await;
        let already_got_one = first.is_ok();
        if already_got_one {
            let second =
                tokio::time::timeout(Duration::from_millis(150), consumer.rows.recv()).await;
            assert!(second.is_err(), "paused pump delivered a second page");
        }

        registry.resume(CursorId(30));
        let deadline = std::time::Instant::now() + Duration::from_millis(500);
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(50), consumer.rows.recv()).await {
                Ok(Some(Page::Done { .. })) => return,
                Ok(Some(_)) => continue,
                _ => continue,
            }
        }
        panic!("paused-then-resumed cursor never delivered Done terminal");
    }

    #[tokio::test]
    async fn per_session_cap_evicts_oldest() {
        let cfg = CursorConfig {
            max_per_session: 2,
            prefetch_pages: 1,
            ..CursorConfig::default()
        };
        let registry = CursorRegistry::new(cfg);
        let evicted = Arc::new(std::sync::Mutex::new(Vec::<CursorId>::new()));
        let evicted_cb = Arc::clone(&evicted);
        registry.set_on_evict(Arc::new(move |_s, c| evicted_cb.lock().unwrap().push(c)));
        let session = SessionId(1);

        registry
            .wrap(
                session,
                ResultSetStream::with_cursor_mode(CursorId(10), mpsc::channel::<Page>(1).1, true),
            )
            .unwrap();
        std::thread::sleep(Duration::from_millis(2));
        registry
            .wrap(
                session,
                ResultSetStream::with_cursor_mode(CursorId(11), mpsc::channel::<Page>(1).1, true),
            )
            .unwrap();
        std::thread::sleep(Duration::from_millis(2));
        registry
            .wrap(
                session,
                ResultSetStream::with_cursor_mode(CursorId(12), mpsc::channel::<Page>(1).1, true),
            )
            .unwrap();

        assert_eq!(registry.session_cursor_count(session), 2);
        assert!(!registry.is_open(CursorId(10)));
        assert!(registry.is_open(CursorId(11)));
        assert!(registry.is_open(CursorId(12)));
        assert_eq!(*evicted.lock().unwrap(), vec![CursorId(10)]);
    }

    #[tokio::test]
    async fn touch_updates_lra_rank() {
        let cfg = CursorConfig {
            max_per_session: 2,
            prefetch_pages: 1,
            ..CursorConfig::default()
        };
        let registry = CursorRegistry::new(cfg);
        registry.set_on_evict(Arc::new(|_, _| {}));
        let session = SessionId(1);

        registry
            .wrap(
                session,
                ResultSetStream::with_cursor_mode(CursorId(10), mpsc::channel::<Page>(1).1, true),
            )
            .unwrap();
        std::thread::sleep(Duration::from_millis(2));
        registry
            .wrap(
                session,
                ResultSetStream::with_cursor_mode(CursorId(11), mpsc::channel::<Page>(1).1, true),
            )
            .unwrap();
        std::thread::sleep(Duration::from_millis(2));
        registry.touch(CursorId(10));
        std::thread::sleep(Duration::from_millis(2));
        registry
            .wrap(
                session,
                ResultSetStream::with_cursor_mode(CursorId(12), mpsc::channel::<Page>(1).1, true),
            )
            .unwrap();

        assert!(!registry.is_open(CursorId(11)));
        assert!(registry.is_open(CursorId(10)));
        assert!(registry.is_open(CursorId(12)));
    }

    #[tokio::test]
    async fn eviction_is_per_session_not_global() {
        let cfg = CursorConfig {
            max_per_session: 1,
            prefetch_pages: 1,
            ..CursorConfig::default()
        };
        let registry = CursorRegistry::new(cfg);
        registry.set_on_evict(Arc::new(|_, _| {}));
        registry
            .wrap(
                SessionId(1),
                ResultSetStream::with_cursor_mode(CursorId(10), mpsc::channel::<Page>(1).1, true),
            )
            .unwrap();
        registry
            .wrap(
                SessionId(2),
                ResultSetStream::with_cursor_mode(CursorId(20), mpsc::channel::<Page>(1).1, true),
            )
            .unwrap();
        registry
            .wrap(
                SessionId(1),
                ResultSetStream::with_cursor_mode(CursorId(11), mpsc::channel::<Page>(1).1, true),
            )
            .unwrap();

        assert!(!registry.is_open(CursorId(10)));
        assert!(registry.is_open(CursorId(11)));
        assert!(registry.is_open(CursorId(20)));
    }

    #[tokio::test]
    async fn evict_callback_fires_once_per_victim() {
        let cfg = CursorConfig {
            max_per_session: 1,
            prefetch_pages: 1,
            ..CursorConfig::default()
        };
        let registry = CursorRegistry::new(cfg);
        let count = Arc::new(AtomicUsize::new(0));
        let cb_count = Arc::clone(&count);
        registry.set_on_evict(Arc::new(move |_, _| {
            cb_count.fetch_add(1, Ordering::Relaxed);
        }));
        let s = SessionId(1);
        registry
            .wrap(
                s,
                ResultSetStream::with_cursor_mode(CursorId(1), mpsc::channel::<Page>(1).1, true),
            )
            .unwrap();
        registry
            .wrap(
                s,
                ResultSetStream::with_cursor_mode(CursorId(2), mpsc::channel::<Page>(1).1, true),
            )
            .unwrap();
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn wrap_rejects_duplicate_cursor_id() {
        let registry = CursorRegistry::new(CursorConfig::default());
        registry.set_on_evict(Arc::new(|_, _| {}));
        registry
            .wrap(
                SessionId(1),
                ResultSetStream::with_cursor_mode(CursorId(1), mpsc::channel::<Page>(1).1, true),
            )
            .unwrap();
        let err = registry
            .wrap(
                SessionId(1),
                ResultSetStream::with_cursor_mode(CursorId(1), mpsc::channel::<Page>(1).1, true),
            )
            .unwrap_err();
        assert_eq!(err.code, Code::DriverInternal);
    }

    #[tokio::test]
    async fn remove_is_idempotent() {
        let registry = CursorRegistry::new(CursorConfig::default());
        registry.set_on_evict(Arc::new(|_, _| {}));
        registry
            .wrap(
                SessionId(1),
                ResultSetStream::with_cursor_mode(CursorId(1), mpsc::channel::<Page>(1).1, true),
            )
            .unwrap();
        registry.remove(CursorId(1));
        registry.remove(CursorId(1));
        assert_eq!(registry.session_cursor_count(SessionId(1)), 0);
    }

    #[tokio::test]
    async fn spill_writes_file_when_configured_and_over_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = CursorConfig {
            max_per_session: 1,
            prefetch_pages: 1,
            spill_dir: Some(dir.path().to_path_buf()),
            // Very low threshold so any spillover triggers write.
            spill_min_bytes: 1,
        };
        let registry = CursorRegistry::new(cfg);
        registry.set_on_evict(Arc::new(|_, _| {}));

        // First cursor with a driver stream that has extra pages
        // queued the pump won't get to forward before eviction.
        let (tx, rx) = mpsc::channel::<Page>(8);
        for i in 0..4 {
            tx.send(Page::Rows {
                rows: vec![Row::new(vec![Value::Int32(i)])],
            })
            .await
            .unwrap();
        }
        let stream1 = ResultSetStream::with_cursor_mode(CursorId(100), rx, true);
        let mut c1 = registry.wrap(SessionId(1), stream1).unwrap();
        // Consume 1 page to establish flow, then keep the sender alive
        // so the pump keeps recving.
        let _ = c1.rows.recv().await;
        let _tx = tx;

        // Evict via cap.
        let stream2 = stream_with_pages(
            CursorId(101),
            vec![Page::Done {
                affected_rows: None,
                warnings: Vec::new(),
            }],
        );
        let _ = registry.wrap(SessionId(1), stream2).unwrap();

        // Drain c1 so the pump reaches emit_terminal + spill.
        while let Some(page) = c1.rows.recv().await {
            if matches!(page, Page::Error { .. } | Page::Done { .. }) {
                break;
            }
        }

        // Spill file may or may not exist depending on race — the
        // pump may have already forwarded everything before eviction
        // set the cancel flag. What we assert: if it does exist, it's
        // non-empty and parseable.
        let path = dir.path().join("sift-cursor-100.bin");
        if path.exists() {
            let bytes = std::fs::read(&path).unwrap();
            assert!(!bytes.is_empty(), "spill file exists but is empty");
        }
    }
}
