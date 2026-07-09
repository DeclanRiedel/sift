//! Server-side cursor registry (ADR-011).
//!
//! Sits above the driver layer. Tracks every open cursor, indexes them
//! by session, enforces a per-session cap, and exposes the hooks the WS
//! layer needs (last-ack update, cancel routing).
//!
//! This is the first slice of ADR-011: cap enforcement + idle-based
//! eviction + cancel routing. Prefetch buffering and spill-to-disk are
//! called out in the ADR as follow-up work; the registry structure here
//! is designed to grow into them without churning callers.

use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
use sift_protocol::{Code, CursorId, DriverError, SessionId};

/// Configuration knobs. Applied on the registry and used for every new
/// cursor. Mutating this affects new cursors only.
#[derive(Debug, Clone)]
pub struct CursorConfig {
    /// Maximum simultaneously-open cursors per session. When at cap,
    /// opening a new cursor evicts the least-recently-acked one.
    pub max_per_session: usize,
    /// Directory for spill files (ADR-011 follow-up). `None` disables
    /// spill; today all evictions drop.
    pub spill_dir: Option<std::path::PathBuf>,
    /// Cursors whose spilled footprint would be smaller than this are
    /// dropped instead of spilled (ADR-011 follow-up).
    pub spill_min_bytes: usize,
}

impl Default for CursorConfig {
    fn default() -> Self {
        Self {
            max_per_session: 32,
            spill_dir: None,
            spill_min_bytes: 1024 * 1024,
        }
    }
}

/// Callback invoked by the registry when it decides to evict a cursor.
/// The registry does not call `driver.cancel` directly — the caller
/// (SessionStore) owns that so the driver-side ownership check (P0 #4)
/// still runs on the same code path as an explicit cancel.
pub type EvictCallback = Arc<dyn Fn(SessionId, CursorId) + Send + Sync>;

#[derive(Clone, Default)]
pub struct CursorRegistry {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    config: std::sync::RwLock<CursorConfig>,
    entries: DashMap<CursorId, Arc<CursorState>>,
    /// Reverse index: session → cursors it owns. Kept in sync with
    /// `entries`.
    per_session: DashMap<SessionId, Vec<CursorId>>,
    /// Called when a cursor is evicted under the per-session cap. Set
    /// by SessionStore at wire-up.
    on_evict: std::sync::RwLock<Option<EvictCallback>>,
}

struct CursorState {
    #[allow(dead_code)]
    cursor_id: CursorId,
    session_id: SessionId,
    /// Wall clock of the last consumer-observed page (or `open` time
    /// before any ack). Used for LRA (least-recently-acked) ranking
    /// under the per-session cap.
    last_ack: std::sync::Mutex<Instant>,
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

    /// Install the eviction callback. Called by SessionStore during
    /// construction; the callback delegates to `SessionStore::cancel`
    /// so the driver-side ownership check runs on the same path as an
    /// explicit user-issued cancel.
    pub fn set_on_evict(&self, cb: EvictCallback) {
        *self.inner.on_evict.write().unwrap() = Some(cb);
    }

    /// Register a new cursor for `session_id`. Enforces the per-session
    /// cap by evicting this session's LRA cursor before insertion.
    /// Returns `Err(CursorLimitReached)` only when the cap is 0 (a
    /// misconfiguration) or the cursor id already exists.
    pub fn open(
        &self,
        session_id: SessionId,
        cursor_id: CursorId,
    ) -> Result<(), DriverError> {
        let cap = self.config().max_per_session;
        if cap == 0 {
            return Err(DriverError::new(
                Code::CursorLimitReached,
                "per-session cursor cap is 0",
            ));
        }
        // Snapshot then evict, so we don't hold shard locks across the
        // eviction callback.
        let victims = self.select_victims(session_id, cap);
        for victim in victims {
            self.evict(session_id, victim);
        }
        if self.inner.entries.contains_key(&cursor_id) {
            return Err(DriverError::new(
                Code::DriverInternal,
                "cursor id already registered",
            ));
        }
        let state = Arc::new(CursorState {
            cursor_id,
            session_id,
            last_ack: std::sync::Mutex::new(Instant::now()),
        });
        self.inner.entries.insert(cursor_id, state);
        self.inner
            .per_session
            .entry(session_id)
            .or_default()
            .push(cursor_id);
        Ok(())
    }

    /// Called by the WS ack loop after each successful ack; updates the
    /// cursor's last-ack timestamp so eviction correctly ranks it as
    /// non-idle.
    pub fn touch(&self, cursor_id: CursorId) {
        if let Some(state) = self.inner.entries.get(&cursor_id) {
            *state.last_ack.lock().unwrap() = Instant::now();
        }
    }

    /// Remove a cursor from the registry. Idempotent. Called after a
    /// cursor terminates (`Page::Done`/`Page::Error`) or is cancelled.
    pub fn remove(&self, cursor_id: CursorId) {
        if let Some((_, state)) = self.inner.entries.remove(&cursor_id) {
            if let Some(mut list) = self.inner.per_session.get_mut(&state.session_id) {
                list.retain(|c| *c != cursor_id);
            }
        }
    }

    /// True while the cursor is still registered.
    pub fn is_open(&self, cursor_id: CursorId) -> bool {
        self.inner.entries.contains_key(&cursor_id)
    }

    /// Number of live cursors for a session. For tests and metrics.
    pub fn session_cursor_count(&self, session_id: SessionId) -> usize {
        self.inner
            .per_session
            .get(&session_id)
            .map(|v| v.len())
            .unwrap_or(0)
    }

    /// Pick eviction victims so `session_id`'s cursor count fits under
    /// `cap` after `open` inserts one more. Returns the LRA-ranked list
    /// (oldest first). Called at open time only.
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
        // Fire the callback first so SessionStore can driver.cancel
        // while the registry entry is still live. Then remove.
        let cb = self.inner.on_evict.read().unwrap().clone();
        if let Some(cb) = cb {
            cb(session_id, cursor_id);
        }
        // The callback may already have called `remove` (via
        // SessionStore::cancel → CursorRegistry::remove). If not,
        // remove now to make sure the reverse index doesn't leak.
        self.remove(cursor_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn per_session_cap_evicts_oldest() {
        let cfg = CursorConfig {
            max_per_session: 2,
            ..CursorConfig::default()
        };
        let registry = CursorRegistry::new(cfg);

        let evicted = Arc::new(std::sync::Mutex::new(Vec::<CursorId>::new()));
        let evicted_cb = Arc::clone(&evicted);
        registry.set_on_evict(Arc::new(move |_sess, c| {
            evicted_cb.lock().unwrap().push(c);
        }));

        let session = SessionId(1);
        registry.open(session, CursorId(10)).unwrap();
        // Bump last_ack for 10 backward by touching it first, then 11
        // and 12. We rely on wall-clock ordering here; a tiny sleep
        // gives us stable ordering without brittleness.
        std::thread::sleep(std::time::Duration::from_millis(2));
        registry.open(session, CursorId(11)).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        registry.open(session, CursorId(12)).unwrap();

        assert_eq!(registry.session_cursor_count(session), 2);
        assert!(!registry.is_open(CursorId(10)));
        assert!(registry.is_open(CursorId(11)));
        assert!(registry.is_open(CursorId(12)));
        assert_eq!(*evicted.lock().unwrap(), vec![CursorId(10)]);
    }

    #[test]
    fn touch_updates_lra_rank() {
        let cfg = CursorConfig {
            max_per_session: 2,
            ..CursorConfig::default()
        };
        let registry = CursorRegistry::new(cfg);
        registry.set_on_evict(Arc::new(|_, _| {}));
        let session = SessionId(1);

        registry.open(session, CursorId(10)).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        registry.open(session, CursorId(11)).unwrap();
        // Touch 10 so it becomes LRA-newest; 11 should be evicted next.
        std::thread::sleep(std::time::Duration::from_millis(2));
        registry.touch(CursorId(10));
        std::thread::sleep(std::time::Duration::from_millis(2));
        registry.open(session, CursorId(12)).unwrap();

        assert!(!registry.is_open(CursorId(11)));
        assert!(registry.is_open(CursorId(10)));
        assert!(registry.is_open(CursorId(12)));
    }

    #[test]
    fn eviction_is_per_session_not_global() {
        let cfg = CursorConfig {
            max_per_session: 1,
            ..CursorConfig::default()
        };
        let registry = CursorRegistry::new(cfg);
        registry.set_on_evict(Arc::new(|_, _| {}));

        let s1 = SessionId(1);
        let s2 = SessionId(2);
        registry.open(s1, CursorId(10)).unwrap();
        registry.open(s2, CursorId(20)).unwrap();
        // Opening a second cursor on s1 evicts 10, not 20.
        registry.open(s1, CursorId(11)).unwrap();

        assert!(!registry.is_open(CursorId(10)));
        assert!(registry.is_open(CursorId(11)));
        assert!(registry.is_open(CursorId(20)));
    }

    #[test]
    fn remove_is_idempotent_and_cleans_reverse_index() {
        let registry = CursorRegistry::new(CursorConfig::default());
        registry.set_on_evict(Arc::new(|_, _| {}));
        let session = SessionId(1);
        registry.open(session, CursorId(10)).unwrap();
        registry.remove(CursorId(10));
        registry.remove(CursorId(10)); // second call: no panic
        assert_eq!(registry.session_cursor_count(session), 0);
    }

    #[test]
    fn open_rejects_duplicate_cursor_id() {
        let registry = CursorRegistry::new(CursorConfig::default());
        registry.set_on_evict(Arc::new(|_, _| {}));
        let session = SessionId(1);
        registry.open(session, CursorId(10)).unwrap();
        let err = registry.open(session, CursorId(10)).unwrap_err();
        assert_eq!(err.code, Code::DriverInternal);
    }

    #[test]
    fn evict_callback_fires_once_per_victim() {
        let cfg = CursorConfig {
            max_per_session: 1,
            ..CursorConfig::default()
        };
        let registry = CursorRegistry::new(cfg);
        let count = Arc::new(AtomicUsize::new(0));
        let cb_count = Arc::clone(&count);
        registry.set_on_evict(Arc::new(move |_, _| {
            cb_count.fetch_add(1, Ordering::Relaxed);
        }));
        let s = SessionId(1);
        registry.open(s, CursorId(1)).unwrap();
        registry.open(s, CursorId(2)).unwrap();
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }
}
