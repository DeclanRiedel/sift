//! Process-wide registry of loaded [`DocumentActor`]s, live-writer leases, and
//! the runtime epoch used for lag recovery.
//!
//! Actors are loaded lazily and shared behind a blocking [`Mutex`] so all Loro
//! CPU work for one document is serialized and, at the call site, driven from a
//! `spawn_blocking` task rather than a Tokio worker.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use dashmap::DashMap;
use sift_metadata::{DocumentId, MetadataStore};

use crate::document_actor::{ApplyError, CollaborationLimits, DocumentActor};

/// Shared handle to one document's serialized actor.
pub type SharedActor = Arc<Mutex<DocumentActor>>;

// Idle cache target, not an active-document quota. Borrowed actors cannot be
// evicted without splitting shared identity; a later miss trims them once idle.
const MAX_CACHED_ACTORS: usize = 64;

struct CachedActor {
    actor: SharedActor,
    last_used: AtomicU64,
}

pub struct DocumentRegistry {
    actors: DashMap<i64, CachedActor>,
    cache_clock: AtomicU64,
    /// document_id -> set of replica ids with a live writer.
    leases: DashMap<i64, HashSet<String>>,
    limits: CollaborationLimits,
    /// Per-process id. A change after restart signals clients to resynchronize.
    runtime_epoch: String,
    event_seq: AtomicU64,
}

impl Default for DocumentRegistry {
    fn default() -> Self {
        Self::new(CollaborationLimits::default())
    }
}

impl DocumentRegistry {
    pub fn new(limits: CollaborationLimits) -> Self {
        Self {
            actors: DashMap::new(),
            cache_clock: AtomicU64::new(0),
            leases: DashMap::new(),
            limits,
            runtime_epoch: uuid::Uuid::new_v4().to_string(),
            event_seq: AtomicU64::new(0),
        }
    }

    pub fn limits(&self) -> CollaborationLimits {
        self.limits
    }

    /// The current runtime epoch; stable for the life of the process.
    pub fn runtime_epoch(&self) -> &str {
        &self.runtime_epoch
    }

    /// Monotonic in-memory event sequence for broadcast lag detection.
    pub fn next_event_seq(&self) -> u64 {
        self.event_seq.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// The current event-sequence high-water mark, without advancing it.
    /// Reported in `ResyncRequired` as an opaque cursor for lagged consumers.
    pub fn current_event_seq(&self) -> u64 {
        self.event_seq.load(Ordering::Relaxed)
    }

    /// Load (or return the cached) actor for `document`. Blocking: call from a
    /// `spawn_blocking` task.
    pub fn get_or_load(
        &self,
        metadata: &MetadataStore,
        document: DocumentId,
    ) -> Result<SharedActor, ApplyError> {
        let used = self.cache_clock.fetch_add(1, Ordering::Relaxed);
        if let Some(cached) = self.actors.get(&document.0) {
            cached.last_used.store(used, Ordering::Relaxed);
            return Ok(cached.actor.clone());
        }
        self.trim_idle_cache();
        // Loading outside the entry lock could insert an old replica after a
        // concurrent load, durable update, and idle eviction of that document.
        let cached = match self.actors.entry(document.0) {
            dashmap::mapref::entry::Entry::Occupied(entry) => entry.into_ref(),
            dashmap::mapref::entry::Entry::Vacant(entry) => entry.insert(CachedActor {
                actor: Arc::new(Mutex::new(DocumentActor::load(
                    metadata,
                    document,
                    self.limits,
                )?)),
                last_used: AtomicU64::new(used),
            }),
        };
        cached.last_used.store(used, Ordering::Relaxed);
        Ok(cached.actor.clone())
    }

    fn trim_idle_cache(&self) {
        let remove = self.actors.len().saturating_sub(MAX_CACHED_ACTORS - 1);
        if remove == 0 {
            return;
        }
        let mut candidates: Vec<_> = self
            .actors
            .iter()
            .filter(|entry| Arc::strong_count(&entry.actor) == 1)
            .map(|entry| (*entry.key(), entry.last_used.load(Ordering::Relaxed)))
            .collect();
        candidates.sort_unstable_by_key(|(_, used)| *used);
        for (document, used) in candidates.into_iter().take(remove) {
            self.actors.remove_if(&document, |_, cached| {
                Arc::strong_count(&cached.actor) == 1
                    && cached.last_used.load(Ordering::Relaxed) == used
            });
        }
    }

    /// Drop a cached actor (idle eviction / shutdown). Its state is durable.
    pub fn evict(&self, document: DocumentId) {
        self.actors.remove(&document.0);
    }

    /// Try to claim a live-writer lease for `(document, replica)`. Returns
    /// `false` if another connection already holds it.
    pub fn try_acquire_lease(&self, document: i64, replica: &str) -> bool {
        self.leases
            .entry(document)
            .or_default()
            .insert(replica.to_string())
    }

    /// Release a previously held lease.
    pub fn release_lease(&self, document: i64, replica: &str) {
        self.leases.remove_if_mut(&document, |_, set| {
            set.remove(replica);
            set.is_empty()
        });
    }
}

/// RAII guard: releases every lease a connection acquired when the socket ends.
pub struct LeaseGuard {
    runtime: crate::room_runtime::RoomRuntime,
    held: HashSet<(i64, String)>,
}

impl LeaseGuard {
    pub fn new(runtime: crate::room_runtime::RoomRuntime) -> Self {
        Self {
            runtime,
            held: HashSet::new(),
        }
    }

    /// Ensure this connection holds the writer lease for `(document, replica)`.
    /// Returns `false` if another live connection owns it.
    pub fn ensure(&mut self, document: i64, replica: &str) -> bool {
        let key = (document, replica.to_string());
        if self.held.contains(&key) {
            return true;
        }
        if self
            .runtime
            .documents()
            .try_acquire_lease(document, replica)
        {
            self.held.insert(key);
            true
        } else {
            false
        }
    }
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        for (document, replica) in self.held.drain() {
            self.runtime.documents().release_lease(document, &replica);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_metadata::{MemorySecretStore, NewDocument, NewRoom, PrincipalId, RoomKind, TenantId};

    #[test]
    fn idle_actors_reload_without_splitting_live_owners_and_empty_leases_leave() {
        let store = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        store.bootstrap_local("test").unwrap();
        let room = store
            .create_room(
                TenantId(1),
                PrincipalId(1),
                NewRoom {
                    name: "cache".into(),
                    kind: RoomKind::Shared,
                },
            )
            .unwrap();
        let replica = sift_doc::TextReplica::new(sift_doc::random_peer_id()).unwrap();
        replica.insert(0, "select 1").unwrap();
        let registry = DocumentRegistry::default();
        let mut documents = Vec::new();
        for _ in 0..MAX_CACHED_ACTORS + 2 {
            let document = store
                .create_document(
                    room.id,
                    NewDocument {
                        kind: "sql".into(),
                        title: "query.sql".into(),
                        crdt_state: replica.export_snapshot().unwrap(),
                        snapshot_version: replica.version_vector(),
                        position: 0,
                        connection_profile_id: None,
                    },
                )
                .unwrap();
            documents.push(document.id);
        }
        let pinned = registry.get_or_load(&store, documents[0]).unwrap();
        for &document in &documents[1..] {
            registry.get_or_load(&store, document).unwrap();
        }
        assert_eq!(registry.actors.len(), MAX_CACHED_ACTORS);
        assert!(Arc::ptr_eq(
            &pinned,
            &registry.get_or_load(&store, documents[0]).unwrap()
        ));
        assert!(!registry.actors.contains_key(&documents[1].0));
        let reloaded = registry.get_or_load(&store, documents[1]).unwrap();
        assert_eq!(reloaded.lock().unwrap().text(), "select 1");
        assert!(registry.try_acquire_lease(documents[0].0, "peer"));
        assert!(!registry.try_acquire_lease(documents[0].0, "peer"));
        registry.release_lease(documents[0].0, "peer");
        assert!(registry.leases.is_empty());
    }
}
