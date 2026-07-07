//! Graceful-shutdown drain state (ADR-018).
//!
//! A single `Shutdown` handle is shared via `AppState`. The HTTP layer reads
//! [`Shutdown::is_draining`] to reject new work, and wraps query execution in
//! a [`QueryGuard`] so the shutdown driver can [`Shutdown::await_drain`] until
//! in-flight queries finish or a deadline elapses.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Cheap-to-clone handle over the process drain flag and in-flight query
/// counter. Cloning shares the same underlying state (`Arc` inside).
#[derive(Clone, Default)]
pub struct Shutdown {
    inner: Arc<ShutdownInner>,
}

#[derive(Default)]
struct ShutdownInner {
    draining: AtomicBool,
    in_flight: AtomicUsize,
}

impl Shutdown {
    /// True once draining has begun. New work should be refused.
    pub fn is_draining(&self) -> bool {
        self.inner.draining.load(Ordering::Acquire)
    }

    /// Flip into the draining state. Idempotent; returns `true` only for the
    /// call that actually started the drain.
    pub fn begin_drain(&self) -> bool {
        !self.inner.draining.swap(true, Ordering::AcqRel)
    }

    /// Register an in-flight query. The returned guard decrements the count
    /// when dropped, so `await_drain` observes the query finishing.
    pub fn track_query(&self) -> QueryGuard {
        self.inner.in_flight.fetch_add(1, Ordering::AcqRel);
        QueryGuard {
            inner: self.inner.clone(),
        }
    }

    /// Number of queries currently in flight.
    pub fn in_flight(&self) -> usize {
        self.inner.in_flight.load(Ordering::Acquire)
    }

    /// Wait for in-flight queries to reach zero, bounded by `deadline`. A zero
    /// deadline waits indefinitely. Returns the number of queries still in
    /// flight when the wait ended (`0` = drained cleanly).
    pub async fn await_drain(&self, deadline: Duration) -> usize {
        let poll = async {
            while self.in_flight() != 0 {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        };
        if deadline.is_zero() {
            poll.await;
            return 0;
        }
        match tokio::time::timeout(deadline, poll).await {
            Ok(()) => 0,
            Err(_) => self.in_flight(),
        }
    }
}

/// Drop guard that decrements the in-flight query count. Held for the lifetime
/// of a query (execute + drain/stream).
pub struct QueryGuard {
    inner: Arc<ShutdownInner>,
}

impl Drop for QueryGuard {
    fn drop(&mut self) {
        self.inner.in_flight.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn begin_drain_is_idempotent() {
        let s = Shutdown::default();
        assert!(!s.is_draining());
        assert!(s.begin_drain());
        assert!(s.is_draining());
        assert!(!s.begin_drain());
    }

    #[test]
    fn guards_track_in_flight_count() {
        let s = Shutdown::default();
        assert_eq!(s.in_flight(), 0);
        let g1 = s.track_query();
        let g2 = s.track_query();
        assert_eq!(s.in_flight(), 2);
        drop(g1);
        assert_eq!(s.in_flight(), 1);
        drop(g2);
        assert_eq!(s.in_flight(), 0);
    }

    #[tokio::test]
    async fn await_drain_returns_immediately_when_idle() {
        let s = Shutdown::default();
        assert_eq!(s.await_drain(Duration::from_secs(1)).await, 0);
    }

    #[tokio::test]
    async fn await_drain_reports_stragglers_past_deadline() {
        let s = Shutdown::default();
        let _guard = s.track_query();
        // Deadline elapses while the query is still in flight.
        assert_eq!(s.await_drain(Duration::from_millis(50)).await, 1);
    }

    #[tokio::test]
    async fn await_drain_completes_when_query_finishes() {
        let s = Shutdown::default();
        let guard = s.track_query();
        let waiter = {
            let s = s.clone();
            tokio::spawn(async move { s.await_drain(Duration::from_secs(5)).await })
        };
        // Let the query finish shortly after the drain starts waiting.
        tokio::time::sleep(Duration::from_millis(40)).await;
        drop(guard);
        assert_eq!(waiter.await.unwrap(), 0);
    }
}
