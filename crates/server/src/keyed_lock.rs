//! Lifetime-owned async mutexes for short-lived keyed work. Map entries exist
//! only while a caller owns a gate, including while waiting for its lock.

use std::{hash::Hash, sync::Arc};

use dashmap::DashMap;
use tokio::sync::{Mutex, MutexGuard, OwnedMutexGuard, TryLockError};

pub(crate) struct KeyedLocks<K: Eq + Hash>(Arc<DashMap<K, Arc<Mutex<()>>>>);

impl<K: Eq + Hash> Default for KeyedLocks<K> {
    fn default() -> Self {
        Self(Arc::new(DashMap::new()))
    }
}

impl<K: Eq + Hash + Clone> KeyedLocks<K> {
    pub(crate) fn gate(&self, key: K) -> KeyedGate<K> {
        let gate = self
            .0
            .entry(key.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        KeyedGate {
            key,
            gate: Some(gate),
            entries: self.0.clone(),
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[must_use = "keep the gate alive until the mutation and its lock guard finish"]
pub struct KeyedGate<K: Eq + Hash> {
    key: K,
    gate: Option<Arc<Mutex<()>>>,
    entries: Arc<DashMap<K, Arc<Mutex<()>>>>,
}

pub struct OwnedKeyedGuard<K: Eq + Hash> {
    // Field order matters: unlock and release the mutex Arc before the gate
    // checks whether this was the final owner of the map entry.
    _guard: OwnedMutexGuard<()>,
    _gate: KeyedGate<K>,
}

impl<K: Eq + Hash> KeyedGate<K> {
    pub async fn lock(&self) -> MutexGuard<'_, ()> {
        self.gate.as_ref().unwrap().lock().await
    }

    pub async fn lock_owned(self) -> OwnedKeyedGuard<K> {
        let guard = self.gate.as_ref().unwrap().clone().lock_owned().await;
        OwnedKeyedGuard {
            _guard: guard,
            _gate: self,
        }
    }

    pub fn try_lock(&self) -> Result<MutexGuard<'_, ()>, TryLockError> {
        self.gate.as_ref().unwrap().try_lock()
    }
}

impl<K: Eq + Hash> Drop for KeyedGate<K> {
    fn drop(&mut self) {
        let mut gate = self.gate.take();
        self.entries.remove_if(&self.key, |_, current| {
            let same_gate = Arc::ptr_eq(current, gate.as_ref().unwrap());
            // Release this caller under the map lock. Otherwise concurrent
            // drops can each see another owner and leave an idle entry behind.
            drop(gate.take());
            same_gate && Arc::strong_count(current) == 1
        });
    }
}
