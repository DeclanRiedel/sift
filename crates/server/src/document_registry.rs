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

pub struct DocumentRegistry {
    actors: DashMap<i64, SharedActor>,
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

    /// Load (or return the cached) actor for `document`. Blocking: call from a
    /// `spawn_blocking` task.
    pub fn get_or_load(
        &self,
        metadata: &MetadataStore,
        document: DocumentId,
    ) -> Result<SharedActor, ApplyError> {
        if let Some(actor) = self.actors.get(&document.0) {
            return Ok(actor.clone());
        }
        let actor = Arc::new(Mutex::new(DocumentActor::load(
            metadata,
            document,
            self.limits,
        )?));
        Ok(self.actors.entry(document.0).or_insert(actor).clone())
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
        if let Some(mut set) = self.leases.get_mut(&document) {
            set.remove(replica);
        }
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
