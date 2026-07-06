use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

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
}

impl RoomRuntime {
    pub fn attach(
        &self,
        room_id: i64,
        principal_id: i64,
        client_id: String,
    ) -> (
        i64,
        Vec<RoomPresence>,
        broadcast::Receiver<RoomServerMessage>,
    ) {
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
        let receiver = room.events.subscribe();
        let _ = room.events.send(RoomServerMessage::Presence {
            presence: presence.clone(),
        });
        (attachment_id, presence, receiver)
    }

    pub fn detach(&self, room_id: i64, attachment_id: i64) -> Vec<RoomPresence> {
        let room = self.room(room_id);
        room.presence.remove(&attachment_id);
        let presence = Self::presence_for(&room);
        let _ = room.events.send(RoomServerMessage::Presence {
            presence: presence.clone(),
        });
        presence
    }

    pub fn subscribe(&self, room_id: i64) -> broadcast::Receiver<RoomServerMessage> {
        self.room(room_id).events.subscribe()
    }

    pub fn publish(&self, room_id: i64, message: RoomServerMessage) {
        let _ = self.room(room_id).events.send(message);
    }

    pub fn presence(&self, room_id: i64) -> Vec<RoomPresence> {
        Self::presence_for(&self.room(room_id))
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
}
