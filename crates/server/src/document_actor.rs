//! Per-document Loro actor and durable update path.
//!
//! Each loaded document owns one [`DocumentActor`] holding an authoritative
//! *committed* replica. Incoming updates are first imported into a throwaway
//! *validation* fork so a corrupt, oversized, or dependency-missing update never
//! touches committed state. Only after the update commits to SQLite is it
//! applied to the committed replica, acknowledged, and (by the caller)
//! rebroadcast.
//!
//! The actor is transport-agnostic: it takes a [`MetadataStore`] and byte
//! payloads and returns an [`ApplyOutcome`]. The WebSocket layer (G3) owns
//! framing, ACK ordering, and mapping [`ApplyError`] onto protocol error codes.
//! All Loro CPU work here runs on a blocking thread, never a Tokio worker.

use sift_doc::{DocError, ImportOutcome, TextReplica};
use sift_metadata::{DocumentId, MetadataError, MetadataStore, NewDocumentUpdate, PrincipalId};

/// Tunables mirrored by the server's `CollaborationConfig`. Defaults match the
/// plan; the runtime overrides them from configuration.
#[derive(Debug, Clone, Copy)]
pub struct CollaborationLimits {
    pub max_document_text_bytes: usize,
    pub max_document_update_bytes: usize,
    pub max_document_history_bytes: usize,
    pub snapshot_update_threshold: u64,
    pub snapshot_log_bytes_threshold: u64,
}

impl Default for CollaborationLimits {
    fn default() -> Self {
        Self {
            max_document_text_bytes: 8 * 1024 * 1024,
            max_document_update_bytes: 1024 * 1024,
            max_document_history_bytes: 256 * 1024 * 1024,
            snapshot_update_threshold: 256,
            snapshot_log_bytes_threshold: 4 * 1024 * 1024,
        }
    }
}

/// Why a durable apply was refused. Maps to stable protocol codes at the
/// transport boundary in G3.
#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    #[error("invalid crdt update: {0}")]
    InvalidUpdate(String),
    #[error("update depends on operations the server has not seen")]
    DependenciesMissing,
    #[error("document history exceeds the configured cap")]
    DocumentTooLarge,
    #[error(transparent)]
    Doc(#[from] DocError),
    #[error(transparent)]
    Metadata(#[from] MetadataError),
}

/// Result of a durable apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// The update carried new operations; it committed at `server_seq` and the
    /// resulting version fingerprint is returned for audit.
    Applied {
        server_seq: i64,
        version_fingerprint: String,
    },
    /// The update carried nothing new. Acknowledge idempotently without
    /// inserting a row or rebroadcasting.
    Idempotent,
}

/// A loaded document's serialized CRDT state.
pub struct DocumentActor {
    document: DocumentId,
    committed: TextReplica,
    limits: CollaborationLimits,
    /// Highest durable `server_seq` applied to `committed`.
    last_seq: i64,
    updates_since_snapshot: u64,
    bytes_since_snapshot: u64,
    /// Encoded byte size of the base snapshot, for the history cap estimate.
    base_snapshot_bytes: usize,
}

impl DocumentActor {
    /// Reconstruct a document from its persisted snapshot plus every durable
    /// update after the snapshot sequence. Byte-identical to the last committed
    /// state, so an ACK-visible update always survives reconstruction.
    pub fn load(
        metadata: &MetadataStore,
        document: DocumentId,
        limits: CollaborationLimits,
    ) -> Result<Self, ApplyError> {
        let row = metadata.get_document(document)?;
        let committed = TextReplica::from_snapshot(sift_doc::random_peer_id(), &row.crdt_state)?;
        let mut last_seq = row.snapshot_seq;
        let mut bytes_since_snapshot = 0u64;
        let mut updates_since_snapshot = 0u64;
        for update in metadata.list_document_updates_since(document, row.snapshot_seq)? {
            committed.import(&update.update_bytes)?;
            last_seq = update.server_seq;
            bytes_since_snapshot += update.decoded_len.max(0) as u64;
            updates_since_snapshot += 1;
        }
        Ok(Self {
            document,
            committed,
            limits,
            last_seq,
            updates_since_snapshot,
            bytes_since_snapshot,
            base_snapshot_bytes: row.crdt_state.len(),
        })
    }

    /// Current materialized text.
    pub fn text(&self) -> String {
        self.committed.text()
    }

    /// Encoded version vector of the committed replica.
    pub fn version_vector(&self) -> Vec<u8> {
        self.committed.version_vector()
    }

    /// A fresh full snapshot of committed state, for bootstrapping a new replica.
    pub fn snapshot(&self) -> Result<Vec<u8>, ApplyError> {
        Ok(self.committed.export_snapshot()?)
    }

    /// The Loro updates a peer at `known_version` is missing.
    pub fn updates_since(&self, known_version: &[u8]) -> Result<Vec<u8>, ApplyError> {
        Ok(self.committed.export_updates_since(known_version)?)
    }

    /// Durably apply one client update.
    ///
    /// Steps (matching the plan): enforce the decoded-size limit, import into a
    /// validation fork, reject corrupt/dependency-missing/extra-container/mark
    /// updates, short-circuit no-ops, then insert-and-sequence in one SQLite
    /// transaction before applying to the committed replica. A persistence
    /// failure yields no ACK — the update is not applied to committed state.
    pub fn apply_update(
        &mut self,
        metadata: &MetadataStore,
        submitted_by: PrincipalId,
        replica_id: &str,
        update_id: &str,
        update_bytes: &[u8],
    ) -> Result<ApplyOutcome, ApplyError> {
        if update_bytes.len() > self.limits.max_document_update_bytes {
            return Err(ApplyError::InvalidUpdate(format!(
                "update is {} bytes, over the {}-byte limit",
                update_bytes.len(),
                self.limits.max_document_update_bytes
            )));
        }

        // Validate against a throwaway fork so committed state is never touched
        // by an update that turns out to be invalid.
        let validation = self.committed.fork();
        let outcome = validation
            .import(update_bytes)
            .map_err(|e| ApplyError::InvalidUpdate(e.to_string()))?;
        match outcome {
            ImportOutcome::Pending => return Err(ApplyError::DependenciesMissing),
            ImportOutcome::NoOp => return Ok(ApplyOutcome::Idempotent),
            ImportOutcome::Applied => {}
        }
        validation
            .validate(self.limits.max_document_text_bytes)
            .map_err(|e| match e {
                DocError::TextTooLarge { .. } => ApplyError::InvalidUpdate(e.to_string()),
                other => ApplyError::InvalidUpdate(other.to_string()),
            })?;

        // History cap: reject once the estimated encoded history would exceed
        // the hard per-document ceiling.
        let projected =
            self.base_snapshot_bytes as u64 + self.bytes_since_snapshot + update_bytes.len() as u64;
        if projected > self.limits.max_document_history_bytes as u64 {
            return Err(ApplyError::DocumentTooLarge);
        }

        // Durably insert and sequence before touching committed state.
        let server_seq = metadata.append_document_update(
            self.document,
            NewDocumentUpdate {
                update_id: update_id.to_string(),
                replica_id: replica_id.to_string(),
                submitted_by,
                update_bytes: update_bytes.to_vec(),
                decoded_len: update_bytes.len() as i64,
            },
        )?;

        // Import into the authoritative replica (idempotent).
        self.committed.import(update_bytes)?;
        self.last_seq = server_seq;
        self.updates_since_snapshot += 1;
        self.bytes_since_snapshot += update_bytes.len() as u64;

        let version_fingerprint = fingerprint_version(&self.committed.version_vector());
        Ok(ApplyOutcome::Applied {
            server_seq,
            version_fingerprint,
        })
    }

    /// Whether accumulated updates warrant a fresh snapshot.
    pub fn should_compact(&self) -> bool {
        self.updates_since_snapshot >= self.limits.snapshot_update_threshold
            || self.bytes_since_snapshot >= self.limits.snapshot_log_bytes_threshold
    }

    /// Persist a fresh full snapshot and delete the update rows it now covers,
    /// under the caller's document lock. Full Loro history stays inside the
    /// snapshot, so arbitrarily old replicas still synchronize.
    pub fn compact(&mut self, metadata: &MetadataStore) -> Result<(), ApplyError> {
        let snapshot = self.committed.export_snapshot()?;
        let version = self.committed.version_vector();
        self.base_snapshot_bytes = snapshot.len();
        metadata.replace_document_snapshot(self.document, snapshot, version, self.last_seq)?;
        self.updates_since_snapshot = 0;
        self.bytes_since_snapshot = 0;
        Ok(())
    }
}

/// Upgrade every legacy document (`crdt_format_version == 0`, whose `crdt_state`
/// is raw UTF-8) to a format-version-1 Loro snapshot.
///
/// All-or-nothing: every legacy row is validated and its snapshot built first,
/// so a single invalid-UTF-8 row aborts the whole pass — with the offending
/// document id — before any row is written. Returns the number upgraded.
pub fn upgrade_legacy_documents(metadata: &MetadataStore) -> Result<usize, ApplyError> {
    let legacy = metadata.list_legacy_documents()?;
    let mut built = Vec::with_capacity(legacy.len());
    for doc in &legacy {
        let text = std::str::from_utf8(&doc.crdt_state).map_err(|_| {
            ApplyError::InvalidUpdate(format!(
                "legacy document {} contains non-utf8 bytes; migration aborted",
                doc.id.0
            ))
        })?;
        let replica = TextReplica::new(sift_doc::random_peer_id())?;
        if !text.is_empty() {
            replica.insert(0, text)?;
        }
        built.push((doc.id, replica.export_snapshot()?, replica.version_vector()));
    }
    metadata.upgrade_documents_to_loro(&built)?;
    Ok(built.len())
}

/// Short hex fingerprint of an encoded version vector, for audit records. Never
/// carries update bytes, replica ids, or SQL text.
fn fingerprint_version(version: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(version);
    let mut out = String::with_capacity(16);
    for byte in &digest[..8] {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_metadata::{MemorySecretStore, MetadataStore, NewDocument, NewRoom, RoomKind};
    use std::sync::Arc;

    fn store() -> MetadataStore {
        let store = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        store.bootstrap_local("tester").unwrap();
        store
    }

    /// Create a room + Loro-seeded document, returning its id.
    fn seed_document(store: &MetadataStore, text: &str) -> DocumentId {
        let room = store
            .create_room(
                sift_metadata::TenantId(1),
                PrincipalId(1),
                NewRoom {
                    name: "r".into(),
                    kind: RoomKind::Shared,
                },
            )
            .unwrap();
        let replica = TextReplica::new(sift_doc::random_peer_id()).unwrap();
        if !text.is_empty() {
            replica.insert(0, text).unwrap();
        }
        let doc = store
            .create_document(
                room.id,
                NewDocument {
                    kind: "sql".into(),
                    title: "t.sql".into(),
                    crdt_state: replica.export_snapshot().unwrap(),
                    snapshot_version: replica.version_vector(),
                    position: 0,
                    connection_profile_id: None,
                },
            )
            .unwrap();
        doc.id
    }

    /// Build a client update: fork the actor's committed state, edit, export the
    /// delta since the actor's version.
    fn client_edit(base: &TextReplica, edit: impl FnOnce(&TextReplica)) -> Vec<u8> {
        let since = base.version_vector();
        edit(base);
        base.export_updates_since(&since).unwrap()
    }

    #[test]
    fn durable_apply_survives_reconstruction() {
        let store = store();
        let doc = seed_document(&store, "select 1");
        let mut actor = DocumentActor::load(&store, doc, CollaborationLimits::default()).unwrap();

        // A client replica bootstrapped from the same snapshot authors an edit.
        let row = store.get_document(doc).unwrap();
        let client =
            TextReplica::from_snapshot(sift_doc::random_peer_id(), &row.crdt_state).unwrap();
        let update = client_edit(&client, |r| r.insert(8, "0").unwrap());

        let outcome = actor
            .apply_update(&store, PrincipalId(1), "replica-a", "u1", &update)
            .unwrap();
        let ApplyOutcome::Applied { server_seq, .. } = outcome else {
            panic!("expected applied, got {outcome:?}");
        };
        assert_eq!(server_seq, 1);
        assert_eq!(actor.text(), "select 10");

        // Reconstruct from SQLite: snapshot + post-snapshot update rows.
        let reloaded = DocumentActor::load(&store, doc, CollaborationLimits::default()).unwrap();
        assert_eq!(reloaded.text(), "select 10");
    }

    #[test]
    fn duplicate_update_is_idempotent_without_new_sequence() {
        let store = store();
        let doc = seed_document(&store, "a");
        let mut actor = DocumentActor::load(&store, doc, CollaborationLimits::default()).unwrap();
        let row = store.get_document(doc).unwrap();
        let client =
            TextReplica::from_snapshot(sift_doc::random_peer_id(), &row.crdt_state).unwrap();
        let update = client_edit(&client, |r| r.insert(1, "b").unwrap());

        actor
            .apply_update(&store, PrincipalId(1), "replica-a", "u1", &update)
            .unwrap();
        // Re-applying the same bytes is a no-op: no row, no rebroadcast.
        let again = actor
            .apply_update(&store, PrincipalId(1), "replica-a", "u1", &update)
            .unwrap();
        assert_eq!(again, ApplyOutcome::Idempotent);
        assert_eq!(store.list_document_updates_since(doc, -1).unwrap().len(), 1);
    }

    #[test]
    fn missing_dependencies_are_rejected() {
        let store = store();
        let doc = seed_document(&store, "");
        let mut actor = DocumentActor::load(&store, doc, CollaborationLimits::default()).unwrap();
        let row = store.get_document(doc).unwrap();

        // Two sequential edits on a client; deliver only the second.
        let client =
            TextReplica::from_snapshot(sift_doc::random_peer_id(), &row.crdt_state).unwrap();
        let _first = client_edit(&client, |r| r.insert(0, "x").unwrap());
        let second = client_edit(&client, |r| r.insert(1, "y").unwrap());

        let err = actor
            .apply_update(&store, PrincipalId(1), "replica-a", "u2", &second)
            .unwrap_err();
        assert!(matches!(err, ApplyError::DependenciesMissing));
        // Nothing was persisted.
        assert!(store
            .list_document_updates_since(doc, -1)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn compaction_rebuilds_exactly_and_drops_covered_rows() {
        let store = store();
        let doc = seed_document(&store, "");
        let limits = CollaborationLimits {
            snapshot_update_threshold: 4,
            ..CollaborationLimits::default()
        };
        let mut actor = DocumentActor::load(&store, doc, limits).unwrap();
        let row = store.get_document(doc).unwrap();
        let client =
            TextReplica::from_snapshot(sift_doc::random_peer_id(), &row.crdt_state).unwrap();

        for i in 0..5u32 {
            let update = client_edit(&client, |r| {
                let len = r.text().chars().count();
                r.insert(len, &i.to_string()).unwrap();
            });
            actor
                .apply_update(
                    &store,
                    PrincipalId(1),
                    "replica-a",
                    &format!("u{i}"),
                    &update,
                )
                .unwrap();
        }
        assert!(actor.should_compact());
        let last_text = actor.text();
        assert_eq!(last_text, "01234");

        actor.compact(&store).unwrap();
        // All covered rows are gone; the snapshot advanced.
        assert!(store
            .list_document_updates_since(doc, -1)
            .unwrap()
            .is_empty());
        let reloaded = DocumentActor::load(&store, doc, limits).unwrap();
        assert_eq!(reloaded.text(), "01234");
    }

    #[test]
    fn legacy_rows_upgrade_to_loro_without_loss() {
        let store = store();
        let room = store
            .create_room(
                sift_metadata::TenantId(1),
                PrincipalId(1),
                NewRoom {
                    name: "r".into(),
                    kind: RoomKind::Shared,
                },
            )
            .unwrap();
        let legacy = store
            .insert_legacy_document(room.id, "old.sql", "select * from t".as_bytes())
            .unwrap();

        assert_eq!(upgrade_legacy_documents(&store).unwrap(), 1);

        // The row is now a real Loro snapshot whose text is the original.
        let actor = DocumentActor::load(&store, legacy, CollaborationLimits::default()).unwrap();
        assert_eq!(actor.text(), "select * from t");
        // Nothing left to upgrade.
        assert_eq!(upgrade_legacy_documents(&store).unwrap(), 0);
    }

    #[test]
    fn invalid_legacy_bytes_abort_without_partial_changes() {
        let store = store();
        let room = store
            .create_room(
                sift_metadata::TenantId(1),
                PrincipalId(1),
                NewRoom {
                    name: "r".into(),
                    kind: RoomKind::Shared,
                },
            )
            .unwrap();
        let good = store
            .insert_legacy_document(room.id, "good.sql", b"ok")
            .unwrap();
        store
            .insert_legacy_document(room.id, "bad.sql", &[0xff, 0xfe, 0x00])
            .unwrap();

        let err = upgrade_legacy_documents(&store).unwrap_err();
        assert!(matches!(err, ApplyError::InvalidUpdate(_)));
        // The good row was NOT upgraded: the pass is all-or-nothing.
        assert_eq!(store.get_document(good).unwrap().crdt_format_version, 0);
        assert_eq!(store.list_legacy_documents().unwrap().len(), 2);
    }

    #[test]
    fn oversize_update_is_rejected() {
        let store = store();
        let doc = seed_document(&store, "");
        let limits = CollaborationLimits {
            max_document_update_bytes: 4,
            ..CollaborationLimits::default()
        };
        let mut actor = DocumentActor::load(&store, doc, limits).unwrap();
        let err = actor
            .apply_update(&store, PrincipalId(1), "replica-a", "u1", &[0u8; 64])
            .unwrap_err();
        assert!(matches!(err, ApplyError::InvalidUpdate(_)));
    }
}
