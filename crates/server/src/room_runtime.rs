use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock, Weak};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use sift_protocol::{RoomPresence, RoomSelection, RoomServerMessage};
use tokio::sync::broadcast;

use crate::document_registry::DocumentRegistry;

#[derive(Clone, Default)]
pub struct RoomRuntime {
    inner: Arc<RoomRuntimeInner>,
}

#[derive(Default)]
struct RoomRuntimeInner {
    rooms: DashMap<i64, Arc<RoomRuntimeRoom>>,
    next_attachment_id: AtomicI64,
    documents: DocumentRegistry,
    results: crate::room_results::RoomResultRegistry,
    workspace_locks: DashMap<i64, Arc<tokio::sync::Mutex<()>>>,
    workspace_adapter: RwLock<Option<Arc<crate::workspace_adapter::RootedFilesystemAdapter>>>,
    git_adapter: RwLock<Option<Arc<crate::git_adapter::GitAdapter>>>,
    vcs_pending: DashMap<(i64, String), sift_protocol::VcsPendingOperation>,
    run_cancellations: DashMap<i64, tokio_util::sync::CancellationToken>,
    scheduler_started: AtomicBool,
}

struct RoomRuntimeRoom {
    presence: DashMap<i64, PresenceEntry>,
    /// Ephemeral lane: presence, attach refresh, query-result references,
    /// rate-limit notices. A lagged consumer heals with a presence snapshot.
    presence_events: broadcast::Sender<RoomServerMessage>,
    /// Durable lane: committed CRDT document ops. A lagged consumer must
    /// resynchronize (`ResyncRequired`), not silently drop the op.
    doc_events: broadcast::Sender<RoomServerMessage>,
    subscribers: AtomicUsize,
}

struct PresenceEntry {
    presence: RoomPresence,
    last_seen: Instant,
}

/// Ring capacity for the ephemeral presence lane. Newest-wins is fine — a
/// lagged consumer refreshes from the authoritative presence map.
const PRESENCE_CHANNEL_CAPACITY: usize = 256;
/// Ring capacity for the durable document lane. Deeper buffer buys resync-free
/// recovery for briefly slow consumers before `ResyncRequired` is forced.
const DOC_CHANNEL_CAPACITY: usize = 1024;
const PRESENCE_LEASE: Duration = Duration::from_secs(30);

pub struct RoomSubscription {
    room_id: i64,
    room: Arc<RoomRuntimeRoom>,
    runtime: Weak<RoomRuntimeInner>,
    presence_rx: broadcast::Receiver<RoomServerMessage>,
    doc_rx: broadcast::Receiver<RoomServerMessage>,
}

#[derive(Debug, thiserror::Error)]
pub enum IntegrationError {
    #[error(transparent)]
    Workspace(#[from] crate::workspace_adapter::WorkspaceAdapterError),
    #[error(transparent)]
    Git(#[from] crate::git_adapter::GitAdapterError),
}

#[must_use = "dropping the attachment detaches it from room presence"]
pub struct RoomAttachment {
    runtime: RoomRuntime,
    room_id: i64,
    attachment_id: i64,
    attached: bool,
}

impl RoomRuntime {
    pub fn start_scheduler_once(&self) -> bool {
        !self.inner.scheduler_started.swap(true, Ordering::AcqRel)
    }
    pub fn with_workspace_config(
        config: &crate::config::WorkspaceProjectionConfig,
    ) -> Result<Self, crate::workspace_adapter::WorkspaceAdapterError> {
        let runtime = Self::default();
        let adapter =
            crate::workspace_adapter::RootedFilesystemAdapter::from_config(config)?.map(Arc::new);
        *runtime
            .inner
            .workspace_adapter
            .write()
            .expect("workspace adapter lock poisoned") = adapter;
        Ok(runtime)
    }

    pub async fn with_integrations(
        workspaces: &crate::config::WorkspaceProjectionConfig,
        vcs: &crate::config::VcsConfig,
    ) -> Result<Self, IntegrationError> {
        let runtime = Self::with_workspace_config(workspaces)?;
        let git = crate::git_adapter::GitAdapter::from_config(vcs)
            .await?
            .map(Arc::new);
        *runtime
            .inner
            .git_adapter
            .write()
            .expect("Git adapter lock poisoned") = git;
        Ok(runtime)
    }

    pub fn workspace_adapter(
        &self,
    ) -> Option<Arc<crate::workspace_adapter::RootedFilesystemAdapter>> {
        self.inner
            .workspace_adapter
            .read()
            .expect("workspace adapter lock poisoned")
            .clone()
    }

    pub fn git_adapter(&self) -> Option<Arc<crate::git_adapter::GitAdapter>> {
        self.inner
            .git_adapter
            .read()
            .expect("Git adapter lock poisoned")
            .clone()
    }

    pub fn set_vcs_pending(
        &self,
        binding_id: i64,
        paths: &[sift_protocol::WorkspacePath],
        operation: sift_protocol::VcsPendingOperation,
    ) {
        for path in paths {
            self.inner
                .vcs_pending
                .insert((binding_id, path.0.clone()), operation);
        }
    }

    pub fn clear_vcs_pending(&self, binding_id: i64, paths: &[sift_protocol::WorkspacePath]) {
        for path in paths {
            self.inner.vcs_pending.remove(&(binding_id, path.0.clone()));
        }
    }

    pub fn vcs_pending(
        &self,
        binding_id: i64,
    ) -> std::collections::HashMap<String, sift_protocol::VcsPendingOperation> {
        self.inner
            .vcs_pending
            .iter()
            .filter(|entry| entry.key().0 == binding_id)
            .map(|entry| (entry.key().1.clone(), *entry.value()))
            .collect()
    }

    pub fn register_run(&self, run_id: i64) -> tokio_util::sync::CancellationToken {
        let token = tokio_util::sync::CancellationToken::new();
        self.inner.run_cancellations.insert(run_id, token.clone());
        token
    }

    pub fn cancel_run(&self, run_id: i64) -> bool {
        self.inner
            .run_cancellations
            .get(&run_id)
            .is_some_and(|token| {
                token.cancel();
                true
            })
    }

    pub fn finish_run(&self, run_id: i64) {
        self.inner.run_cancellations.remove(&run_id);
    }

    pub fn attach(
        &self,
        room_id: i64,
        principal_id: i64,
        client_id: String,
    ) -> (RoomAttachment, Vec<RoomPresence>) {
        let room = self.room(room_id);
        let attachment_id = self
            .inner
            .next_attachment_id
            .fetch_add(1, Ordering::Relaxed)
            + 1;
        room.presence.insert(
            attachment_id,
            PresenceEntry {
                presence: RoomPresence {
                    attachment_id,
                    principal_id,
                    client_id,
                    active_document_id: None,
                    selection: None,
                },
                last_seen: Instant::now(),
            },
        );
        let presence = Self::presence_for(&room);
        let _ = room.presence_events.send(RoomServerMessage::Presence {
            presence: presence.clone(),
        });
        (
            RoomAttachment {
                runtime: self.clone(),
                room_id,
                attachment_id,
                attached: true,
            },
            presence,
        )
    }

    pub fn detach(&self, room_id: i64, attachment_id: i64) -> Vec<RoomPresence> {
        let Some(room) = self.inner.rooms.get(&room_id).map(|entry| entry.clone()) else {
            return Vec::new();
        };
        room.presence.remove(&attachment_id);
        let presence = Self::presence_for(&room);
        let _ = room.presence_events.send(RoomServerMessage::Presence {
            presence: presence.clone(),
        });
        presence
    }

    pub fn subscribe(&self, room_id: i64) -> RoomSubscription {
        let room = self.room(room_id);
        room.subscribers.fetch_add(1, Ordering::AcqRel);
        RoomSubscription {
            room_id,
            presence_rx: room.presence_events.subscribe(),
            doc_rx: room.doc_events.subscribe(),
            room,
            runtime: Arc::downgrade(&self.inner),
        }
    }

    /// Publish on the ephemeral presence lane (presence, query-result
    /// references, rate-limit notices). Loss on lag is healed by a snapshot.
    pub fn publish_presence(&self, room_id: i64, message: RoomServerMessage) {
        if let Some(room) = self.inner.rooms.get(&room_id) {
            let _ = room.presence_events.send(message);
        }
    }

    /// Publish a committed CRDT op on the durable document lane and advance the
    /// runtime event sequence. Loss on lag forces the consumer to resync.
    pub fn publish_doc(&self, room_id: i64, message: RoomServerMessage) {
        if let Some(room) = self.inner.rooms.get(&room_id) {
            let _ = room.doc_events.send(message);
            self.inner.documents.next_event_seq();
        }
    }

    /// The process-wide document actor registry, leases, and runtime epoch.
    pub fn documents(&self) -> &DocumentRegistry {
        &self.inner.documents
    }

    /// Serialize cross-resource workspace mutations, especially checkpoint
    /// restore where Loro document updates and tree metadata must agree.
    pub fn workspace_lock(&self, workspace_id: i64) -> Arc<tokio::sync::Mutex<()>> {
        self.inner
            .workspace_locks
            .entry(workspace_id)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    pub fn results(&self) -> &crate::room_results::RoomResultRegistry {
        &self.inner.results
    }

    /// Whether the room still has live runtime state (subscribers or
    /// presence). A room is evicted once its last subscription drops.
    pub fn is_active(&self, room_id: i64) -> bool {
        self.inner.rooms.contains_key(&room_id)
    }

    pub fn presence(&self, room_id: i64) -> Vec<RoomPresence> {
        self.inner
            .rooms
            .get(&room_id)
            .map(|room| Self::presence_for(&room))
            .unwrap_or_default()
    }

    /// Renew an attachment's ephemeral lease. Returns false when the
    /// attachment no longer exists (for example after lease expiry).
    pub fn heartbeat(&self, room_id: i64, attachment_id: i64) -> bool {
        let Some(room) = self.inner.rooms.get(&room_id) else {
            return false;
        };
        let Some(mut entry) = room.presence.get_mut(&attachment_id) else {
            return false;
        };
        entry.last_seen = Instant::now();
        true
    }

    pub fn update_presence(
        &self,
        room_id: i64,
        attachment_id: i64,
        active_document_id: Option<i64>,
        selection: Option<RoomSelection>,
    ) -> bool {
        let Some(room) = self.inner.rooms.get(&room_id) else {
            return false;
        };
        let Some(mut entry) = room.presence.get_mut(&attachment_id) else {
            return false;
        };
        entry.presence.active_document_id = active_document_id;
        entry.presence.selection = selection;
        entry.last_seen = Instant::now();
        drop(entry);
        let presence = Self::presence_for(&room);
        let _ = room
            .presence_events
            .send(RoomServerMessage::Presence { presence });
        true
    }

    /// Remove attachments whose heartbeat lease elapsed and publish one
    /// authoritative snapshot if anything changed.
    pub fn expire_presence(&self, room_id: i64) -> Vec<i64> {
        let Some(room) = self.inner.rooms.get(&room_id).map(|entry| entry.clone()) else {
            return Vec::new();
        };
        let now = Instant::now();
        let expired: Vec<_> = room
            .presence
            .iter()
            .filter(|entry| now.duration_since(entry.last_seen) >= PRESENCE_LEASE)
            .map(|entry| *entry.key())
            .collect();
        for attachment_id in &expired {
            room.presence.remove(attachment_id);
        }
        if !expired.is_empty() {
            let presence = Self::presence_for(&room);
            let _ = room
                .presence_events
                .send(RoomServerMessage::Presence { presence });
        }
        expired
    }

    fn room(&self, room_id: i64) -> Arc<RoomRuntimeRoom> {
        self.inner
            .rooms
            .entry(room_id)
            .or_insert_with(|| {
                let (presence_events, _) = broadcast::channel(PRESENCE_CHANNEL_CAPACITY);
                let (doc_events, _) = broadcast::channel(DOC_CHANNEL_CAPACITY);
                Arc::new(RoomRuntimeRoom {
                    presence: DashMap::new(),
                    presence_events,
                    doc_events,
                    subscribers: AtomicUsize::new(0),
                })
            })
            .clone()
    }

    fn presence_for(room: &RoomRuntimeRoom) -> Vec<RoomPresence> {
        let mut presence: Vec<_> = room
            .presence
            .iter()
            .map(|entry| entry.value().presence.clone())
            .collect();
        presence.sort_by_key(|presence| presence.attachment_id);
        presence
    }

    #[cfg(test)]
    fn room_count(&self) -> usize {
        self.inner.rooms.len()
    }
}

impl RoomSubscription {
    /// Disjoint mutable handles to both lanes so a single `select!` can await
    /// them concurrently: `(presence, document)`. The presence lane heals a
    /// `Lagged` with a snapshot; the document lane must resync on `Lagged`.
    pub fn receivers(
        &mut self,
    ) -> (
        &mut broadcast::Receiver<RoomServerMessage>,
        &mut broadcast::Receiver<RoomServerMessage>,
    ) {
        (&mut self.presence_rx, &mut self.doc_rx)
    }
}

impl RoomAttachment {
    pub fn id(&self) -> i64 {
        self.attachment_id
    }

    pub fn detach(mut self) -> Vec<RoomPresence> {
        self.attached = false;
        self.runtime.detach(self.room_id, self.attachment_id)
    }
}

impl Drop for RoomAttachment {
    fn drop(&mut self) {
        if self.attached {
            self.runtime.detach(self.room_id, self.attachment_id);
            self.attached = false;
        }
    }
}

impl Drop for RoomSubscription {
    fn drop(&mut self) {
        if self.room.subscribers.fetch_sub(1, Ordering::AcqRel) != 1
            || !self.room.presence.is_empty()
        {
            return;
        }
        let Some(runtime) = self.runtime.upgrade() else {
            return;
        };
        runtime.rooms.remove_if(&self.room_id, |_, candidate| {
            Arc::ptr_eq(candidate, &self.room)
                && candidate.subscribers.load(Ordering::Acquire) == 0
                && candidate.presence.is_empty()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_access_does_not_create_room_state() {
        let runtime = RoomRuntime::default();
        assert!(runtime.presence(10).is_empty());
        runtime.publish_presence(
            10,
            RoomServerMessage::Presence {
                presence: Vec::new(),
            },
        );
        assert!(runtime.detach(10, 1).is_empty());
        assert_eq!(runtime.room_count(), 0);
    }

    #[test]
    fn final_subscription_drop_evicts_an_empty_room() {
        let runtime = RoomRuntime::default();
        let first = runtime.subscribe(10);
        let second = runtime.subscribe(10);
        let (attachment, _) = runtime.attach(10, 1, "client".into());
        assert_eq!(runtime.room_count(), 1);
        attachment.detach();
        drop(first);
        assert_eq!(runtime.room_count(), 1);
        drop(second);
        assert_eq!(runtime.room_count(), 0);
    }

    fn sample(message: &str) -> RoomServerMessage {
        RoomServerMessage::Error {
            message: message.into(),
        }
    }

    #[test]
    fn is_active_tracks_room_lifecycle() {
        let runtime = RoomRuntime::default();
        assert!(!runtime.is_active(5));
        let subscription = runtime.subscribe(5);
        assert!(runtime.is_active(5));
        drop(subscription);
        // Last subscription dropped -> room evicted -> teardown fires.
        assert!(!runtime.is_active(5));
    }

    #[test]
    fn presence_overflow_does_not_evict_durable_doc_ops() {
        let runtime = RoomRuntime::default();
        let mut sub = runtime.subscribe(7);
        // One durable op sits undrained on the document lane.
        runtime.publish_doc(7, sample("commit"));
        // Flood the presence lane far past its capacity.
        for i in 0..(PRESENCE_CHANNEL_CAPACITY * 2) {
            runtime.publish_presence(7, sample(&format!("p{i}")));
        }
        let (presence_rx, doc_rx) = sub.receivers();
        // The presence lane lagged...
        assert!(matches!(
            presence_rx.try_recv(),
            Err(broadcast::error::TryRecvError::Lagged(_))
        ));
        // ...but the durable op survived on its own lane.
        assert!(matches!(
            doc_rx.try_recv(),
            Ok(RoomServerMessage::Error { .. })
        ));
    }

    #[test]
    fn doc_lane_overflow_reports_lagged() {
        let runtime = RoomRuntime::default();
        let mut sub = runtime.subscribe(8);
        for i in 0..(DOC_CHANNEL_CAPACITY + 5) {
            runtime.publish_doc(8, sample(&format!("c{i}")));
        }
        let (_presence_rx, doc_rx) = sub.receivers();
        assert!(matches!(
            doc_rx.try_recv(),
            Err(broadcast::error::TryRecvError::Lagged(_))
        ));
    }

    #[test]
    fn publish_doc_advances_event_seq_but_presence_does_not() {
        let runtime = RoomRuntime::default();
        let _sub = runtime.subscribe(9);
        let before = runtime.documents().current_event_seq();
        runtime.publish_doc(9, sample("c"));
        assert_eq!(runtime.documents().current_event_seq(), before + 1);
        runtime.publish_presence(9, sample("p"));
        assert_eq!(runtime.documents().current_event_seq(), before + 1);
    }

    #[tokio::test]
    async fn workspace_mutation_locks_are_shared_per_workspace() {
        let runtime = RoomRuntime::default();
        let first = runtime.workspace_lock(7);
        let same_workspace = runtime.workspace_lock(7);
        let other_workspace = runtime.workspace_lock(8);

        let guard = first.lock().await;
        assert!(same_workspace.try_lock().is_err());
        assert!(other_workspace.try_lock().is_ok());
        drop(guard);
        assert!(same_workspace.try_lock().is_ok());
    }

    #[test]
    fn dropping_an_attachment_clears_presence() {
        let runtime = RoomRuntime::default();
        let subscription = runtime.subscribe(10);
        let (attachment, _) = runtime.attach(10, 1, "client".into());
        assert_eq!(runtime.presence(10).len(), 1);

        drop(attachment);

        assert!(runtime.presence(10).is_empty());
        drop(subscription);
        assert_eq!(runtime.room_count(), 0);
    }
}
