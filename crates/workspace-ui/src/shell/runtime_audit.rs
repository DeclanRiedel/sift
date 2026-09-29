//! Bounded, read-only Runtime administration audit rings.

use sift_protocol::{AuditEntry, OperationAuditEntry};

pub(super) const MAX_VISIBLE_RING_ROWS: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RingKind {
    Operations,
    Requests,
}

#[derive(Default)]
pub(super) struct RuntimeAuditState {
    generation: u64,
    pub pending: Option<(u64, RingKind)>,
    pub operations: Vec<OperationAuditEntry>,
    pub requests: Vec<AuditEntry>,
    pub error: Option<String>,
}

impl RuntimeAuditState {
    pub fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.pending = None;
        self.error = None;
    }

    pub fn begin(&mut self, kind: RingKind) -> u64 {
        self.invalidate();
        self.pending = Some((self.generation, kind));
        self.generation
    }

    pub fn accepts(&self, generation: u64, kind: RingKind) -> bool {
        self.pending == Some((generation, kind))
    }

    pub fn finish_operations(
        &mut self,
        generation: u64,
        result: Result<Vec<OperationAuditEntry>, String>,
    ) -> bool {
        if !self.accepts(generation, RingKind::Operations) {
            return false;
        }
        self.pending = None;
        match result {
            Ok(rows) => self.operations = newest(rows),
            Err(error) => self.error = Some(error),
        }
        true
    }

    pub fn finish_requests(
        &mut self,
        generation: u64,
        result: Result<Vec<AuditEntry>, String>,
    ) -> bool {
        if !self.accepts(generation, RingKind::Requests) {
            return false;
        }
        self.pending = None;
        match result {
            Ok(rows) => self.requests = newest(rows),
            Err(error) => self.error = Some(error),
        }
        true
    }
}

fn newest<T>(rows: Vec<T>) -> Vec<T> {
    rows.into_iter().rev().take(MAX_VISIBLE_RING_ROWS).collect()
}

pub(super) fn path_without_query(path: &str) -> &str {
    path.split_once('?').map_or(path, |(route, _)| route)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_view_keeps_newest_bounded_rows_and_rejects_stale_generation() {
        let rows: Vec<_> = (0..150).collect();
        let visible = newest(rows);
        assert_eq!(visible.len(), MAX_VISIBLE_RING_ROWS);
        assert_eq!(visible.first(), Some(&149));
        assert_eq!(visible.last(), Some(&50));

        let mut state = RuntimeAuditState::default();
        let old = state.begin(RingKind::Operations);
        let current = state.begin(RingKind::Requests);
        assert!(!state.accepts(old, RingKind::Operations));
        assert!(state.accepts(current, RingKind::Requests));
        state.invalidate();
        assert!(!state.accepts(current, RingKind::Requests));
        assert_eq!(path_without_query("/v1/search?q=secret"), "/v1/search");
    }
}
