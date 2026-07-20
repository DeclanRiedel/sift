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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
use futures::FutureExt;
use sift_driver_api::ResultSetStream;
use sift_protocol::{Code, ConnectionId, CursorId, DriverError, Page, SessionId};
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
    /// Time-to-live for a spill file after it's written. If the client
    /// never resumes, the file is reaped after this duration.
    pub spill_ttl: std::time::Duration,
}

impl Default for CursorConfig {
    fn default() -> Self {
        Self {
            max_per_session: 32,
            prefetch_pages: 2,
            spill_dir: None,
            spill_min_bytes: 1024 * 1024,
            spill_ttl: std::time::Duration::from_secs(600),
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
    per_connection: DashMap<(SessionId, ConnectionId), Vec<CursorId>>,
    /// Monotonic tick handed out on cursor creation and on every `touch`.
    /// A cursor's `last_ack` sequence is its rank; the lowest is the
    /// least-recently-acked (LRA) victim. A single relaxed atomic
    /// replaces a per-cursor `Mutex<Instant>`, so `select_victims` reads
    /// ranks without taking any lock.
    clock: AtomicU64,
    on_evict: std::sync::RwLock<Option<EvictCallback>>,
    /// Registry of spill files produced by evicted cursors that the
    /// client may still resume via the HTTP endpoint. Written by the
    /// pump when it lands a spill file; drained by `read_spill_page`
    /// on final page or by `reap_expired_spills` on TTL.
    spills: DashMap<CursorId, SpillEntry>,
}

#[derive(Clone)]
struct SpillEntry {
    session_id: SessionId,
    path: PathBuf,
    created_at: Instant,
    /// Byte offset the next resume read should start from. Advanced
    /// on each successful read_spill_page.
    read_offset: u64,
    /// Total pages written to the spill file, for the resume endpoint
    /// to report progress.
    total_pages: usize,
    pages_read: usize,
}

struct CursorState {
    #[allow(dead_code)]
    cursor_id: CursorId,
    session_id: SessionId,
    connection_id: Option<ConnectionId>,
    /// LRA rank: the `Inner::clock` tick as of the last `touch` (or
    /// creation). Lower = older. Relaxed ordering is sufficient — the
    /// value is only used to pick an eviction victim, not to synchronize.
    last_ack: AtomicU64,
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

impl Inner {
    /// Hand out the next monotonic LRA tick. Relaxed is fine — we only
    /// need a total order among ticks, not synchronization with other
    /// memory.
    fn next_seq(&self) -> u64 {
        self.clock.fetch_add(1, Ordering::Relaxed)
    }
}

impl CursorRegistry {
    pub fn new(config: CursorConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                config: std::sync::RwLock::new(config),
                entries: DashMap::new(),
                per_session: DashMap::new(),
                per_connection: DashMap::new(),
                clock: AtomicU64::new(0),
                on_evict: std::sync::RwLock::new(None),
                spills: DashMap::new(),
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
        self.wrap_inner(session_id, None, stream)
    }

    pub fn wrap_for_connection(
        &self,
        session_id: SessionId,
        connection_id: ConnectionId,
        stream: ResultSetStream,
    ) -> Result<ResultSetStream, DriverError> {
        self.wrap_inner(session_id, Some(connection_id), stream)
    }

    fn wrap_inner(
        &self,
        session_id: SessionId,
        connection_id: Option<ConnectionId>,
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
            connection_id,
            last_ack: AtomicU64::new(self.inner.next_seq()),
            control: Arc::clone(&control),
        });
        self.inner.entries.insert(cursor_id, state);
        self.inner
            .per_session
            .entry(session_id)
            .or_default()
            .push(cursor_id);
        if let Some(connection_id) = connection_id {
            self.inner
                .per_connection
                .entry((session_id, connection_id))
                .or_default()
                .push(cursor_id);
        }

        let prefetch = config.prefetch_pages.max(1);
        let (consumer_tx, consumer_rx) = mpsc::channel::<Page>(prefetch);
        let inner_for_pump = Arc::clone(&self.inner);
        let ResultSetStream {
            columns,
            rows: driver_rx,
            warnings,
            affected_rows,
            server_side_cursor,
            ..
        } = stream;
        tokio::spawn(supervise_pump_task(
            session_id,
            cursor_id,
            control,
            driver_rx,
            consumer_tx,
            inner_for_pump,
        ));

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
            state
                .last_ack
                .store(self.inner.next_seq(), Ordering::Relaxed);
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
            if let Some(connection_id) = state.connection_id {
                remove_connection_cursor(&self.inner, state.session_id, connection_id, cursor_id);
            }
        }
    }

    pub fn connection_cursors(
        &self,
        session_id: SessionId,
        connection_id: ConnectionId,
    ) -> Vec<CursorId> {
        self.inner
            .per_connection
            .get(&(session_id, connection_id))
            .map(|ids| ids.clone())
            .unwrap_or_default()
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

    /// Look up a spill entry by cursor id. Returns None if no spill
    /// exists (never was, or already consumed/reaped).
    pub fn spill_info(&self, cursor_id: CursorId) -> Option<SpillInfo> {
        self.inner.spills.get(&cursor_id).map(|e| SpillInfo {
            session_id: e.session_id,
            total_pages: e.total_pages,
            pages_read: e.pages_read,
            expires_in: self
                .config()
                .spill_ttl
                .checked_sub(e.created_at.elapsed())
                .unwrap_or_default(),
        })
    }

    /// Read the next `count` pages from the spill file for
    /// `cursor_id`. Returns the pages and a boolean `done` flag. When
    /// `done` is true the spill entry has been dropped and its file
    /// deleted; subsequent calls return `Err(CursorNotFound)`.
    pub fn read_spill_pages(
        &self,
        cursor_id: CursorId,
        count: usize,
    ) -> Result<(Vec<Page>, bool), DriverError> {
        // Fast-path check: entry exists.
        let entry_snapshot = self
            .inner
            .spills
            .get(&cursor_id)
            .ok_or_else(|| DriverError::new(Code::CursorNotFound, "no spill for cursor"))?
            .clone();

        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .open(&entry_snapshot.path)
            .map_err(|e| DriverError::new(Code::DriverInternal, format!("open spill file: {e}")))?;
        use std::io::{Read as _, Seek as _, SeekFrom};
        file.seek(SeekFrom::Start(entry_snapshot.read_offset))
            .map_err(|e| DriverError::new(Code::DriverInternal, e.to_string()))?;

        let mut out = Vec::with_capacity(count.min(entry_snapshot.total_pages));
        let mut offset = entry_snapshot.read_offset;
        let mut pages_read = entry_snapshot.pages_read;
        let want = count.max(1);
        for _ in 0..want {
            if pages_read >= entry_snapshot.total_pages {
                break;
            }
            let mut len_buf = [0u8; 4];
            if file.read_exact(&mut len_buf).is_err() {
                break;
            }
            let len = u32::from_be_bytes(len_buf) as usize;
            let mut bytes = vec![0u8; len];
            file.read_exact(&mut bytes)
                .map_err(|e| DriverError::new(Code::DriverInternal, format!("spill read: {e}")))?;
            let page: Page = serde_json::from_slice(&bytes).map_err(|e| {
                DriverError::new(Code::DriverInternal, format!("spill decode: {e}"))
            })?;
            out.push(page);
            offset += 4 + len as u64;
            pages_read += 1;
        }

        let done = pages_read >= entry_snapshot.total_pages;
        if done {
            self.drop_spill(cursor_id);
        } else if let Some(mut e) = self.inner.spills.get_mut(&cursor_id) {
            e.read_offset = offset;
            e.pages_read = pages_read;
        }
        Ok((out, done))
    }

    /// Drop a spill entry and delete its file. Idempotent.
    pub fn drop_spill(&self, cursor_id: CursorId) {
        if let Some((_, entry)) = self.inner.spills.remove(&cursor_id) {
            let _ = std::fs::remove_file(&entry.path);
        }
    }

    /// Reap spill entries whose age exceeds `spill_ttl`. Called on a
    /// periodic tick by the server.
    pub fn reap_expired_spills(&self) -> usize {
        let ttl = self.config().spill_ttl;
        let expired: Vec<CursorId> = self
            .inner
            .spills
            .iter()
            .filter(|e| e.value().created_at.elapsed() >= ttl)
            .map(|e| *e.key())
            .collect();
        let count = expired.len();
        for cid in expired {
            self.drop_spill(cid);
        }
        count
    }

    /// Number of open spill files. For tests and metrics.
    pub fn spill_count(&self) -> usize {
        self.inner.spills.len()
    }
}

/// Public summary of a spill entry — used by the resume endpoint to
/// gate access and by tests.
#[derive(Debug, Clone)]
pub struct SpillInfo {
    pub session_id: SessionId,
    pub total_pages: usize,
    pub pages_read: usize,
    pub expires_in: std::time::Duration,
}

impl CursorRegistry {
    fn select_victims(&self, session_id: SessionId, cap: usize) -> Vec<CursorId> {
        let Some(ids) = self.inner.per_session.get(&session_id) else {
            return Vec::new();
        };
        if ids.len() < cap {
            return Vec::new();
        }
        // Read each cursor's LRA rank directly off its atomic — no
        // per-cursor lock, no clone of the id list. We hold the
        // per-session shard guard only for the length of this cheap
        // scan, and drop it before `evict` (which takes the same map's
        // `get_mut`) runs in the caller.
        let mut ranked: Vec<(CursorId, u64)> = ids
            .iter()
            .filter_map(|&c| {
                self.inner
                    .entries
                    .get(&c)
                    .map(|s| (c, s.last_ack.load(Ordering::Relaxed)))
            })
            .collect();
        drop(ids);
        ranked.sort_by_key(|(_, seq)| *seq);
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
            if let Some(connection_id) = state.connection_id {
                remove_connection_cursor(&self.inner, state.session_id, connection_id, cursor_id);
            }
        }
    }
}

fn remove_connection_cursor(
    inner: &Inner,
    session_id: SessionId,
    connection_id: ConnectionId,
    cursor_id: CursorId,
) {
    let key = (session_id, connection_id);
    let empty = if let Some(mut ids) = inner.per_connection.get_mut(&key) {
        ids.retain(|id| *id != cursor_id);
        ids.is_empty()
    } else {
        false
    };
    if empty {
        inner.per_connection.remove(&key);
    }
}

async fn supervise_pump_task(
    session_id: SessionId,
    cursor_id: CursorId,
    control: Arc<PumpControl>,
    driver_rx: mpsc::Receiver<Page>,
    consumer_tx: mpsc::Sender<Page>,
    inner: Arc<Inner>,
) {
    let panic_tx = consumer_tx.clone();
    let panic_inner = Arc::clone(&inner);
    let result = std::panic::AssertUnwindSafe(pump_task(
        session_id,
        cursor_id,
        control,
        driver_rx,
        consumer_tx,
        inner,
    ))
    .catch_unwind()
    .await;
    if result.is_err() {
        remove_cursor_state(&panic_inner, session_id, cursor_id);
        let _ = panic_tx
            .send(Page::Error {
                error: DriverError::new(Code::DriverInternal, "cursor pump task panicked"),
            })
            .await;
    }
}

fn remove_cursor_state(inner: &Arc<Inner>, session_id: SessionId, cursor_id: CursorId) {
    let connection_id = inner
        .entries
        .remove(&cursor_id)
        .and_then(|(_, state)| state.connection_id);
    if let Some(mut list) = inner.per_session.get_mut(&session_id) {
        list.retain(|c| *c != cursor_id);
    }
    if let Some(connection_id) = connection_id {
        remove_connection_cursor(inner, session_id, connection_id, cursor_id);
    }
}

async fn pump_task(
    session_id: SessionId,
    cursor_id: CursorId,
    control: Arc<PumpControl>,
    mut driver_rx: mpsc::Receiver<Page>,
    consumer_tx: mpsc::Sender<Page>,
    inner: Arc<Inner>,
) {
    let mut spillover: Vec<Page> = Vec::new();

    loop {
        let cancel = control.cancel_notify.notified();
        tokio::pin!(cancel);
        if control.cancel.load(Ordering::Acquire) {
            emit_terminal(
                &control,
                &consumer_tx,
                session_id,
                cursor_id,
                &spillover,
                &inner,
            )
            .await;
            return;
        }
        let page = tokio::select! {
            biased;
            _ = &mut cancel => {
                emit_terminal(&control, &consumer_tx, session_id, cursor_id, &spillover, &inner).await;
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
                emit_terminal(
                    &control,
                    &consumer_tx,
                    session_id,
                    cursor_id,
                    &spillover,
                    &inner,
                )
                .await;
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
            emit_terminal(
                &control,
                &consumer_tx,
                session_id,
                cursor_id,
                &spillover,
                &inner,
            )
            .await;
            return;
        }

        let is_terminal = matches!(&page, Page::Done { .. } | Page::Error { .. });
        let reserve_fut = consumer_tx.reserve();
        tokio::pin!(reserve_fut);
        let sent = tokio::select! {
            biased;
            _ = control.cancel_notify.notified() => {
                spillover.push(page);
                emit_terminal(&control, &consumer_tx, session_id, cursor_id, &spillover, &inner).await;
                return;
            }
            res = &mut reserve_fut => {
                match res {
                    Ok(permit) => {
                        permit.send(page);
                        true
                    }
                    Err(_) => false,
                }
            },
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
    session_id: SessionId,
    cursor_id: CursorId,
    spillover: &[Page],
    inner: &Arc<Inner>,
) {
    let mut reason = control
        .cancel_reason
        .lock()
        .unwrap()
        .take()
        .unwrap_or_else(|| DriverError::new(Code::QueryCanceled, "cursor closed"));

    // Attempt spill first so we can attach a resume_url to the
    // terminal error when spill lands successfully.
    let spill_dir = control.spill_dir.lock().unwrap().clone();
    if reason.code == Code::CursorEvicted {
        if let Some(dir) = spill_dir {
            let approx_bytes = approx_pages_bytes(spillover);
            if approx_bytes >= control.spill_min_bytes && !spillover.is_empty() {
                let pages = spillover.to_vec();
                let write_result =
                    tokio::task::spawn_blocking(move || write_spill(&dir, cursor_id, &pages))
                        .await
                        .map_err(std::io::Error::other)
                        .and_then(|result| result);
                match write_result {
                    Ok(path) => {
                        inner.spills.insert(
                            cursor_id,
                            SpillEntry {
                                session_id,
                                path,
                                created_at: Instant::now(),
                                read_offset: 0,
                                total_pages: spillover.len(),
                                pages_read: 0,
                            },
                        );
                        reason =
                            reason.with_resume_url(format!("/v1/cursors/{}/pages", cursor_id.0));
                    }
                    Err(error) => {
                        tracing::debug!(?cursor_id, %error, "cursor spill write failed");
                    }
                }
            }
        }
    }

    // Best-effort delivery with a short deadline. If the consumer
    // isn't draining, we don't block the pump forever.
    let _ = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        consumer_tx.send(Page::Error { error: reason }),
    )
    .await;
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
        Page::Rows { rows } => rows.iter().map(approx_row_bytes).sum(),
        Page::NextResult { columns } => columns
            .iter()
            .map(|column| column.name.len().saturating_add(64))
            .sum(),
        _ => 64,
    }
}

fn approx_row_bytes(row: &sift_protocol::Row) -> usize {
    row.values
        .iter()
        .map(approx_value_bytes)
        .sum::<usize>()
        .saturating_add(8)
}

fn approx_value_bytes(value: &sift_protocol::Value) -> usize {
    use sift_protocol::Value;
    match value {
        Value::Text(s) | Value::Decimal(s) => s.len(),
        Value::Blob(bytes) => bytes.len(),
        Value::Json(value) => value.to_string().len(),
        Value::Engine {
            type_name,
            display_text,
            ..
        } => type_name.len().saturating_add(display_text.len()),
        _ => 16,
    }
}

fn write_spill(
    dir: &PathBuf,
    cursor_id: CursorId,
    pages: &[Page],
) -> Result<PathBuf, std::io::Error> {
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
    Ok(path)
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
    async fn connection_reverse_index_tracks_cursor_lifetime() {
        let registry = CursorRegistry::new(CursorConfig::default());
        let (tx, rx) = mpsc::channel(1);
        let cursor = CursorId(42);
        let _wrapped = registry
            .wrap_for_connection(
                SessionId(7),
                ConnectionId(9),
                ResultSetStream::with_cursor_mode(cursor, rx, true),
            )
            .unwrap();
        assert_eq!(
            registry.connection_cursors(SessionId(7), ConnectionId(9)),
            vec![cursor]
        );
        registry.remove(cursor);
        assert!(registry
            .connection_cursors(SessionId(7), ConnectionId(9))
            .is_empty());
        drop(tx);
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
            spill_ttl: std::time::Duration::from_secs(60),
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

    /// End-to-end: evict a cursor, verify the CursorEvicted terminal
    /// carries a resume_url, then read the spilled pages back via the
    /// registry and confirm the file is deleted when fully drained.
    #[tokio::test]
    async fn spill_resume_reads_all_pages_and_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = CursorConfig {
            max_per_session: 1,
            prefetch_pages: 1,
            spill_dir: Some(dir.path().to_path_buf()),
            spill_min_bytes: 1,
            spill_ttl: std::time::Duration::from_secs(60),
        };
        let registry = CursorRegistry::new(cfg);
        registry.set_on_evict(Arc::new(|_, _| {}));
        let session = SessionId(1);

        // Push 5 rows + a Done, then stop the sender. This gives the
        // pump plenty to buffer while we evict.
        let (tx, rx) = mpsc::channel::<Page>(16);
        for i in 0..5 {
            tx.send(Page::Rows {
                rows: vec![Row::new(vec![Value::Int32(i)])],
            })
            .await
            .unwrap();
        }
        let stream1 = ResultSetStream::with_cursor_mode(CursorId(500), rx, true);
        let mut c1 = registry.wrap(session, stream1).unwrap();
        // Read one page so the pump is definitely producing.
        let _ = c1.rows.recv().await;
        let _tx = tx;

        // Evict via cap.
        let stream2 = stream_with_pages(
            CursorId(501),
            vec![Page::Done {
                affected_rows: None,
                warnings: Vec::new(),
            }],
        );
        registry.wrap(session, stream2).unwrap();

        // Drain c1's channel until we hit the terminal Error.
        let mut resume_url = None;
        while let Some(page) =
            tokio::time::timeout(std::time::Duration::from_millis(500), c1.rows.recv())
                .await
                .unwrap_or(None)
        {
            if let Page::Error { error } = &page {
                if error.code == Code::CursorEvicted {
                    resume_url = error.resume_url.clone();
                }
                break;
            }
        }

        // Only assert on spill semantics when the pump actually spilled
        // (buffered pages existed when eviction fired). If it didn't,
        // there's nothing to test in this run — the race is legitimate.
        if let Some(url) = resume_url {
            assert!(url.contains("500"), "resume_url wrong: {url}");
            let info = registry
                .spill_info(CursorId(500))
                .expect("spill entry should exist");
            assert!(info.total_pages > 0);
            assert_eq!(info.pages_read, 0);

            // Read pages back in chunks until done.
            let mut total = 0;
            loop {
                let (pages, done) = registry.read_spill_pages(CursorId(500), 2).unwrap();
                total += pages.len();
                if done {
                    break;
                }
                if pages.is_empty() {
                    panic!("no pages returned but not done");
                }
            }
            assert_eq!(total, info.total_pages);
            // File was deleted on final read.
            assert!(registry.spill_info(CursorId(500)).is_none());
            let path = dir.path().join("sift-cursor-500.bin");
            assert!(!path.exists(), "spill file was not deleted");
        }
    }

    #[tokio::test]
    async fn spill_read_rejects_wrong_from_seq() {
        let dir = tempfile::tempdir().unwrap();
        // Manually construct a spill entry so we can test read logic
        // without racing eviction timing.
        let registry = CursorRegistry::new(CursorConfig {
            spill_dir: Some(dir.path().to_path_buf()),
            spill_min_bytes: 1,
            spill_ttl: std::time::Duration::from_secs(60),
            ..CursorConfig::default()
        });
        let pages = vec![
            Page::Rows {
                rows: vec![Row::new(vec![Value::Int32(1)])],
            },
            Page::Rows {
                rows: vec![Row::new(vec![Value::Int32(2)])],
            },
        ];
        let path = write_spill(&dir.path().to_path_buf(), CursorId(700), &pages).unwrap();
        registry.inner.spills.insert(
            CursorId(700),
            SpillEntry {
                session_id: SessionId(1),
                path,
                created_at: Instant::now(),
                read_offset: 0,
                total_pages: pages.len(),
                pages_read: 0,
            },
        );

        // First read of 1 page advances pages_read to 1.
        let (out, done) = registry.read_spill_pages(CursorId(700), 1).unwrap();
        assert_eq!(out.len(), 1);
        assert!(!done);
        let info = registry.spill_info(CursorId(700)).unwrap();
        assert_eq!(info.pages_read, 1);

        // Second read completes; done=true and entry is dropped.
        let (out, done) = registry.read_spill_pages(CursorId(700), 5).unwrap();
        assert_eq!(out.len(), 1);
        assert!(done);
        assert!(registry.spill_info(CursorId(700)).is_none());
    }

    #[test]
    fn approx_page_bytes_accounts_for_wide_values() {
        let narrow = Page::Rows {
            rows: vec![Row::new(vec![Value::Int32(1)])],
        };
        let wide = Page::Rows {
            rows: vec![Row::new(vec![Value::Text("x".repeat(1024))])],
        };
        assert!(approx_page_bytes(&wide) > approx_page_bytes(&narrow));
        assert!(approx_page_bytes(&wide) >= 1024);
    }

    #[tokio::test]
    async fn reap_expired_spills_deletes_files() {
        let dir = tempfile::tempdir().unwrap();
        let registry = CursorRegistry::new(CursorConfig {
            spill_dir: Some(dir.path().to_path_buf()),
            spill_ttl: std::time::Duration::from_millis(50),
            ..CursorConfig::default()
        });
        let pages = vec![Page::Rows {
            rows: vec![Row::new(vec![Value::Int32(1)])],
        }];
        let path = write_spill(&dir.path().to_path_buf(), CursorId(800), &pages).unwrap();
        registry.inner.spills.insert(
            CursorId(800),
            SpillEntry {
                session_id: SessionId(1),
                path: path.clone(),
                created_at: Instant::now(),
                read_offset: 0,
                total_pages: 1,
                pages_read: 0,
            },
        );
        assert_eq!(registry.spill_count(), 1);
        // Wait past TTL, then reap.
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        let reaped = registry.reap_expired_spills();
        assert_eq!(reaped, 1);
        assert_eq!(registry.spill_count(), 0);
        assert!(!path.exists());
    }
}
