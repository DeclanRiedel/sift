use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};

use dashmap::DashMap;
use sift_protocol::{RoomPresence, RoomServerMessage};
use tokio::sync::broadcast;

#[derive(Clone, Default)]
pub struct RoomRuntime {
    inner: Arc<RoomRuntimeInner>,
}

#[derive(Default)]
struct RoomRuntimeInner {
    rooms: DashMap<i64, Arc<RoomRuntimeRoom>>,
    next_attachment_id: AtomicI64,
}

struct RoomRuntimeRoom {
    presence: DashMap<i64, RoomPresence>,
    events: broadcast::Sender<RoomServerMessage>,
    subscribers: AtomicUsize,
}

pub struct RoomSubscription {
    room_id: i64,
    room: Arc<RoomRuntimeRoom>,
    runtime: Weak<RoomRuntimeInner>,
    receiver: broadcast::Receiver<RoomServerMessage>,
}

#[must_use = "dropping the attachment detaches it from room presence"]
pub struct RoomAttachment {
    runtime: RoomRuntime,
    room_id: i64,
    attachment_id: i64,
    attached: bool,
}

impl RoomRuntime {
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
            RoomPresence {
                attachment_id,
                principal_id,
                client_id,
            },
        );
        let presence = Self::presence_for(&room);
        let _ = room.events.send(RoomServerMessage::Presence {
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
        let _ = room.events.send(RoomServerMessage::Presence {
            presence: presence.clone(),
        });
        presence
    }

    pub fn subscribe(&self, room_id: i64) -> RoomSubscription {
        let room = self.room(room_id);
        room.subscribers.fetch_add(1, Ordering::AcqRel);
        RoomSubscription {
            room_id,
            receiver: room.events.subscribe(),
            room,
            runtime: Arc::downgrade(&self.inner),
        }
    }

    pub fn publish(&self, room_id: i64, message: RoomServerMessage) {
        if let Some(room) = self.inner.rooms.get(&room_id) {
            let _ = room.events.send(message);
        }
    }

    pub fn presence(&self, room_id: i64) -> Vec<RoomPresence> {
        self.inner
            .rooms
            .get(&room_id)
            .map(|room| Self::presence_for(&room))
            .unwrap_or_default()
    }

    fn room(&self, room_id: i64) -> Arc<RoomRuntimeRoom> {
        self.inner
            .rooms
            .entry(room_id)
            .or_insert_with(|| {
                let (events, _) = broadcast::channel(1024);
                Arc::new(RoomRuntimeRoom {
                    presence: DashMap::new(),
                    events,
                    subscribers: AtomicUsize::new(0),
                })
            })
            .clone()
    }

    fn presence_for(room: &RoomRuntimeRoom) -> Vec<RoomPresence> {
        let mut presence: Vec<_> = room
            .presence
            .iter()
            .map(|entry| entry.value().clone())
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
    pub async fn recv(&mut self) -> Result<RoomServerMessage, broadcast::error::RecvError> {
        self.receiver.recv().await
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
        runtime.publish(
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
