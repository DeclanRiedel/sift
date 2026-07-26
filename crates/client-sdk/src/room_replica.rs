//! In-memory reference `RoomReplica` state machine.
//!
//! Wraps a [`sift_doc::TextReplica`] and speaks the room WebSocket document
//! protocol: bootstrap or catch-up sync, chunk reassembly, local edits with
//! stable update ids held until durable ACK, idempotent application of peer
//! commits, and resync on runtime-epoch change. It does **not** persist replica
//! state to disk; a future durable client must store a Loro snapshot together
//! with its peer id before reusing that peer id.

use std::collections::HashMap;

use sift_doc::{DocError, TextReplica};
use sift_protocol::{CrdtUpdate, DocumentVersion, ReplicaId, RoomClientMessage, RoomServerMessage};

/// Client-side projection used by a UI that follows another room attachment.
/// The server remains authoritative; follow state can always be rebuilt from
/// a presence snapshot plus shared-result discovery after `NeedsRecovery`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FollowEvent {
    Unchanged,
    PresenceChanged {
        active_document_id: Option<i64>,
        selection: Option<sift_protocol::RoomSelection>,
    },
    ResultChanged(sift_protocol::RoomQueryResult),
    TargetLeft,
    NeedsRecovery,
}

#[derive(Debug, Clone)]
pub struct FollowMode {
    attachment_id: i64,
    principal_id: Option<i64>,
    active_document_id: Option<i64>,
    selection: Option<sift_protocol::RoomSelection>,
}

impl FollowMode {
    pub fn new(attachment_id: i64) -> Self {
        Self {
            attachment_id,
            principal_id: None,
            active_document_id: None,
            selection: None,
        }
    }

    pub fn attachment_id(&self) -> i64 {
        self.attachment_id
    }

    pub fn ingest(&mut self, message: &RoomServerMessage) -> FollowEvent {
        match message {
            RoomServerMessage::Attached { presence, .. }
            | RoomServerMessage::Presence { presence } => {
                let Some(target) = presence
                    .iter()
                    .find(|presence| presence.attachment_id == self.attachment_id)
                else {
                    return FollowEvent::TargetLeft;
                };
                if self.active_document_id == target.active_document_id
                    && self.selection == target.selection
                    && self.principal_id == Some(target.principal_id)
                {
                    return FollowEvent::Unchanged;
                }
                self.principal_id = Some(target.principal_id);
                self.active_document_id = target.active_document_id;
                self.selection = target.selection.clone();
                FollowEvent::PresenceChanged {
                    active_document_id: self.active_document_id,
                    selection: self.selection.clone(),
                }
            }
            RoomServerMessage::QueryResult { result }
                if self.principal_id == Some(result.actor_principal_id) =>
            {
                FollowEvent::ResultChanged(result.clone())
            }
            RoomServerMessage::ResyncRequired { .. } => FollowEvent::NeedsRecovery,
            _ => FollowEvent::Unchanged,
        }
    }
}

/// What ingesting one server message meant for the replica.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ingest {
    /// A chunk landed or a peer commit was applied; keep reading.
    Progress,
    /// A `DocumentSync` completed; the payload is the server's version vector.
    Synced(Vec<u8>),
    /// The server durably acknowledged the local update with this id.
    Acked(String),
    /// The runtime restarted or this receiver lagged; resynchronize.
    Resync,
    /// A structured document error for the given request id.
    Error {
        code: sift_protocol::DocumentErrorCode,
        message: String,
    },
    /// Nothing relevant to this replica.
    Ignored,
}

pub struct RoomReplica {
    document_id: i64,
    replica_id: u64,
    replica: TextReplica,
    /// Local updates awaiting a durable ACK: update_id -> bytes.
    pending: HashMap<String, Vec<u8>>,
    /// In-flight chunk transfers: transfer_id -> ordered chunk slots.
    transfers: HashMap<String, Vec<Option<Vec<u8>>>>,
    seq: u64,
}

impl RoomReplica {
    /// Construct from a caller-supplied persisted peer id and optional snapshot.
    pub fn new(
        document_id: i64,
        replica_id: u64,
        snapshot: Option<&[u8]>,
    ) -> Result<Self, DocError> {
        let replica = match snapshot {
            Some(bytes) => TextReplica::from_snapshot(replica_id, bytes)?,
            None => TextReplica::new(replica_id)?,
        };
        Ok(Self {
            document_id,
            replica_id,
            replica,
            pending: HashMap::new(),
            transfers: HashMap::new(),
            seq: 0,
        })
    }

    pub fn document_id(&self) -> i64 {
        self.document_id
    }

    pub fn text(&self) -> String {
        self.replica.text()
    }

    pub fn replica_id(&self) -> ReplicaId {
        ReplicaId(self.replica_id)
    }

    /// Export the peer id plus snapshot for storage by a future durable client.
    pub fn persist(&self) -> Result<(u64, Vec<u8>), DocError> {
        Ok((self.replica_id, self.replica.export_snapshot()?))
    }

    /// Number of local updates not yet durably acknowledged.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    fn known_version(&self) -> DocumentVersion {
        DocumentVersion::new(self.replica.version_vector())
    }

    fn next_id(&mut self, tag: &str) -> String {
        self.seq += 1;
        format!("{}-{}-{}", self.replica_id, tag, self.seq)
    }

    /// A `DocumentSync` carrying this replica's current version.
    pub fn sync_message(&mut self) -> (String, RoomClientMessage) {
        let request_id = self.next_id("sync");
        (
            request_id.clone(),
            RoomClientMessage::DocumentSync {
                request_id,
                document_id: self.document_id,
                replica_id: self.replica_id(),
                known_version: self.known_version(),
            },
        )
    }

    /// Apply a local insert and produce the update to submit. The update id is
    /// stable until the server ACKs it.
    pub fn local_insert(&mut self, pos: usize, text: &str) -> Result<RoomClientMessage, DocError> {
        let since = self.replica.version_vector();
        self.replica.insert(pos, text)?;
        self.update_since(&since, "ins")
    }

    /// Apply a local delete and produce the update to submit.
    pub fn local_delete(&mut self, pos: usize, len: usize) -> Result<RoomClientMessage, DocError> {
        let since = self.replica.version_vector();
        self.replica.delete(pos, len)?;
        self.update_since(&since, "del")
    }

    fn update_since(&mut self, since: &[u8], tag: &str) -> Result<RoomClientMessage, DocError> {
        let update = self.replica.export_updates_since(since)?;
        let update_id = self.next_id(tag);
        let request_id = self.next_id("req");
        self.pending.insert(update_id.clone(), update.clone());
        Ok(RoomClientMessage::DocumentUpdate {
            request_id,
            update_id,
            document_id: self.document_id,
            replica_id: self.replica_id(),
            update: CrdtUpdate::new(update),
        })
    }

    /// After a sync, submit everything this replica holds that the server (at
    /// `server_version`) is missing — the offline-divergence catch-up. Returns
    /// `None` when there is nothing to send.
    pub fn catch_up(
        &mut self,
        server_version: &[u8],
    ) -> Result<Option<RoomClientMessage>, DocError> {
        let Some(update) = self.replica.updates_since_if_any(server_version)? else {
            return Ok(None);
        };
        let update_id = self.next_id("catchup");
        let request_id = self.next_id("req");
        self.pending.insert(update_id.clone(), update.clone());
        Ok(Some(RoomClientMessage::DocumentUpdate {
            request_id,
            update_id,
            document_id: self.document_id,
            replica_id: self.replica_id(),
            update: CrdtUpdate::new(update),
        }))
    }

    /// Fold one server message into replica state.
    pub fn ingest(&mut self, message: &RoomServerMessage) -> Result<Ingest, DocError> {
        match message {
            RoomServerMessage::DocumentChunk {
                document_id,
                transfer_id,
                index,
                count,
                payload,
                ..
            } if *document_id == self.document_id => {
                let slots = self
                    .transfers
                    .entry(transfer_id.clone())
                    .or_insert_with(|| vec![None; *count as usize]);
                if let Some(slot) = slots.get_mut(*index as usize) {
                    *slot = Some(payload.as_bytes().to_vec());
                }
                if slots.iter().all(Option::is_some) {
                    let bytes: Vec<u8> = slots.iter().flatten().flatten().copied().collect();
                    self.transfers.remove(transfer_id);
                    if !bytes.is_empty() {
                        self.replica.import(&bytes)?;
                    }
                }
                Ok(Ingest::Progress)
            }
            RoomServerMessage::DocumentSynced {
                document_id,
                server_version,
                ..
            } if *document_id == self.document_id => {
                Ok(Ingest::Synced(server_version.as_bytes().to_vec()))
            }
            RoomServerMessage::DocumentUpdateAck {
                document_id,
                update_id,
                ..
            } if *document_id == self.document_id => {
                self.pending.remove(update_id);
                Ok(Ingest::Acked(update_id.clone()))
            }
            RoomServerMessage::DocumentUpdateCommitted {
                document_id,
                update,
                ..
            } if *document_id == self.document_id => {
                // Import is idempotent, so re-applying our own echo is harmless.
                if !update.as_bytes().is_empty() {
                    self.replica.import(update.as_bytes())?;
                }
                Ok(Ingest::Progress)
            }
            RoomServerMessage::ResyncRequired { .. } => Ok(Ingest::Resync),
            RoomServerMessage::DocumentError {
                document_id,
                code,
                message,
                ..
            } if *document_id == self.document_id => Ok(Ingest::Error {
                code: *code,
                message: message.clone(),
            }),
            _ => Ok(Ingest::Ignored),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_reassembly_imports_a_snapshot() {
        // Build a source replica and split its snapshot into 2 chunks.
        let src = TextReplica::new(0xA).unwrap();
        src.insert(0, "select 1").unwrap();
        let snapshot = src.export_snapshot().unwrap();
        let mid = snapshot.len() / 2;

        let mut replica = RoomReplica::new(1, 0xB, None).unwrap();
        for (index, part) in [&snapshot[..mid], &snapshot[mid..]].iter().enumerate() {
            let msg = RoomServerMessage::DocumentChunk {
                request_id: "r".into(),
                document_id: 1,
                transfer_id: "t".into(),
                index: index as u32,
                count: 2,
                payload_kind: sift_protocol::DocumentTransferKind::Snapshot,
                payload: CrdtUpdate::new(part.to_vec()),
                snapshot_seq: 0,
                server_version: DocumentVersion::new(src.version_vector()),
            };
            replica.ingest(&msg).unwrap();
        }
        assert_eq!(replica.text(), "select 1");
    }

    #[test]
    fn local_edit_is_pending_until_acked() {
        let mut replica = RoomReplica::new(1, 0xC, None).unwrap();
        let msg = replica.local_insert(0, "hi").unwrap();
        assert_eq!(replica.pending_count(), 1);
        let RoomClientMessage::DocumentUpdate { update_id, .. } = msg else {
            panic!("expected a document update");
        };
        let ack = RoomServerMessage::DocumentUpdateAck {
            request_id: "r".into(),
            update_id: update_id.clone(),
            document_id: 1,
            server_seq: 1,
            version_fingerprint: "abcd".into(),
        };
        assert_eq!(replica.ingest(&ack).unwrap(), Ingest::Acked(update_id));
        assert_eq!(replica.pending_count(), 0);
    }

    #[test]
    fn follow_mode_projects_presence_and_result_references() {
        let mut follow = FollowMode::new(12);
        let presence = RoomServerMessage::Presence {
            presence: vec![sift_protocol::RoomPresence {
                attachment_id: 12,
                principal_id: 7,
                client_id: "editor".into(),
                active_document_id: Some(3),
                selection: None,
            }],
        };
        assert_eq!(
            follow.ingest(&presence),
            FollowEvent::PresenceChanged {
                active_document_id: Some(3),
                selection: None,
            }
        );
        let result = sift_protocol::RoomQueryResult {
            result_id: sift_protocol::RoomResultId(uuid::Uuid::nil()),
            room_id: 1,
            actor_principal_id: 7,
            connection_profile_id: Some(9),
            row_count: Some(1),
            page_count: 2,
            status: sift_protocol::RoomQueryStatus::Ok,
            error_message: None,
            created_at: chrono::Utc::now(),
            finished_at: Some(chrono::Utc::now()),
        };
        assert_eq!(
            follow.ingest(&RoomServerMessage::QueryResult {
                result: result.clone()
            }),
            FollowEvent::ResultChanged(result)
        );
        assert_eq!(
            follow.ingest(&RoomServerMessage::ResyncRequired {
                runtime_epoch: "new".into(),
                event_seq: 3,
            }),
            FollowEvent::NeedsRecovery
        );
    }
}
