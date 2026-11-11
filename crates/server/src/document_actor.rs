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

/// Server-internal collaboration safety limits. The registry uses these
/// defaults; they are not currently exposed in the instance manifest.
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

pub struct AuthoredReplacement {
    pub outcome: ApplyOutcome,
    pub update_bytes: Vec<u8>,
    pub server_version: Vec<u8>,
    pub replica_id: u64,
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
        let document = self.document;
        self.apply_update_with_persistence(
            submitted_by,
            replica_id,
            update_id,
            update_bytes,
            |update| metadata.append_document_update(document, update),
        )
    }

    fn apply_update_with_persistence(
        &mut self,
        submitted_by: PrincipalId,
        replica_id: &str,
        update_id: &str,
        update_bytes: &[u8],
        persist: impl FnOnce(NewDocumentUpdate) -> Result<i64, MetadataError>,
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

        let projected =
            self.base_snapshot_bytes as u64 + self.bytes_since_snapshot + update_bytes.len() as u64;
        if projected > self.limits.max_document_history_bytes as u64 {
            return Err(ApplyError::DocumentTooLarge);
        }

        // Durably insert and sequence before touching committed state.
        let server_seq = persist(NewDocumentUpdate {
            update_id: update_id.to_string(),
            replica_id: replica_id.to_string(),
            submitted_by,
            update_bytes: update_bytes.to_vec(),
            decoded_len: update_bytes.len() as i64,
        })?;

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

    /// Author a whole-text replacement as native Loro operations and pass it
    /// through the same validation, durable sequencing, and limits as a client
    /// update. This is used by checkpoint restore; snapshots are never written
    /// over a live collaborative document.
    pub fn author_replacement(
        &mut self,
        metadata: &MetadataStore,
        submitted_by: PrincipalId,
        replica_id: &str,
        update_id: &str,
        replacement: &str,
    ) -> Result<Option<AuthoredReplacement>, ApplyError> {
        let document = self.document;
        self.author_replacement_with_persistence(
            submitted_by,
            replica_id,
            update_id,
            replacement,
            |update| metadata.append_document_update(document, update),
        )
    }

    /// The edit and its execution receipt commit atomically after validating text.
    pub fn author_ai_query_execution(
        &mut self,
        metadata: &MetadataStore,
        submitted_by: PrincipalId,
        id: uuid::Uuid,
        fingerprint: &str,
        original: &str,
        replacement: &str,
    ) -> Result<Option<AuthoredReplacement>, ApplyError> {
        use sha2::{Digest, Sha256};
        if self.committed.text() != original || original == replacement {
            return Err(ApplyError::InvalidUpdate(
                "query text changed since approval".into(),
            ));
        }
        let before = format!("{:x}", Sha256::digest(original.as_bytes()));
        let after = format!("{:x}", Sha256::digest(replacement.as_bytes()));
        let document = self.document;
        self.author_replacement_with_persistence(
            submitted_by,
            "sift-ai-query-execution",
            &id.to_string(),
            replacement,
            |update| {
                metadata.append_ai_query_execution_document_update(
                    id,
                    fingerprint,
                    document,
                    update,
                    &before,
                    &after,
                )
            },
        )
    }

    /// Human room proposal edits have a retained server owner, not a UI ACK.
    pub fn author_ai_room_query_proposal(
        &mut self,
        metadata: &MetadataStore,
        prepared: &sift_metadata::PreparedAiRoomQueryCommit,
    ) -> Result<AuthoredReplacement, ApplyError> {
        if self.document != prepared.document_id()
            || self.committed.text() != prepared.original_sql()
            || prepared.original_sql() == prepared.proposal().proposed_sql
        {
            return Err(ApplyError::InvalidUpdate(
                "Room query text changed or the proposal has no edit".into(),
            ));
        }
        self.author_replacement_with_persistence(
            prepared.actor(),
            "sift-ai-room-proposal",
            &prepared.proposal().proposal.id.to_string(),
            &prepared.proposal().proposed_sql,
            |update| metadata.append_ai_room_query_proposal_update(prepared, update),
        )?
        .ok_or_else(|| {
            ApplyError::InvalidUpdate("Room query proposal did not author an update".into())
        })
    }

    fn author_replacement_with_persistence(
        &mut self,
        submitted_by: PrincipalId,
        replica_id: &str,
        update_id: &str,
        replacement: &str,
        persist: impl FnOnce(NewDocumentUpdate) -> Result<i64, MetadataError>,
    ) -> Result<Option<AuthoredReplacement>, ApplyError> {
        if self.committed.text() == replacement {
            return Ok(None);
        }
        let since = self.committed.version_vector();
        let authored = self.committed.fork();
        let current_len = authored.text().chars().count();
        if current_len > 0 {
            authored.delete(0, current_len)?;
        }
        if !replacement.is_empty() {
            authored.insert(0, replacement)?;
        }
        let update_bytes = authored.export_updates_since(&since)?;
        let outcome = self.apply_update_with_persistence(
            submitted_by,
            replica_id,
            update_id,
            &update_bytes,
            persist,
        )?;
        Ok(Some(AuthoredReplacement {
            outcome,
            update_bytes,
            server_version: self.committed.version_vector(),
            replica_id: authored.peer_id(),
        }))
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

        let client =
            TextReplica::from_snapshot(sift_doc::random_peer_id(), &row.crdt_state).unwrap();
        let _first = client_edit(&client, |r| r.insert(0, "x").unwrap());
        let second = client_edit(&client, |r| r.insert(1, "y").unwrap());

        let err = actor
            .apply_update(&store, PrincipalId(1), "replica-a", "u2", &second)
            .unwrap_err();
        assert!(matches!(err, ApplyError::DependenciesMissing));
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

        let actor = DocumentActor::load(&store, legacy, CollaborationLimits::default()).unwrap();
        assert_eq!(actor.text(), "select * from t");
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
    #[tokio::test]
    async fn query_execution_commits_text_and_receipt_together_and_refuses_stale_authority() {
        use sha2::Digest;
        use sift_metadata::{
            CredentialMode, MembershipRole, NewConnectionProfile, NewOperationAudit, RoomRole,
            TenantId,
        };
        use sift_protocol::*;
        use uuid::Uuid;
        for mode in ["applied", "stale", "stopped", "policy", "revoked", "limit"] {
            let store = store();
            let actor = PrincipalId(1);
            let document = seed_document(&store, "SELECT 1");
            let room = store.get_document(document).unwrap().room_id;
            let profile_input = NewConnectionProfile {
                name: "query".into(),
                provider_id: Engine::Postgres.provider_id(),
                semantic_engine: Some(Engine::Postgres),
                configuration: serde_json::json!({"database":"query"}),
                credentials: None,
                credential_mode: CredentialMode::Shared,
                tags: vec![],
            };
            let profile = store
                .upsert_connection_profile(TenantId(1), actor, profile_input.clone())
                .await
                .unwrap();
            let chat = store
                .create_ai_chat(
                    TenantId(1),
                    None,
                    actor,
                    AiVisibility::Private,
                    "query".into(),
                )
                .await
                .unwrap();
            let digest = sha2::Sha256::digest(b"SELECT 1");
            let revision = u64::from_le_bytes(digest[..8].try_into().unwrap()) & i64::MAX as u64;
            let context:AiTurnContext=serde_json::from_value(serde_json::json!({"target":{"tenant_id":1,"profile_id":profile.id.0,"room_id":room.0,"document_id":document.0.to_string(),"connection_id":"1:1"},"sql":{"text":"SELECT 1","document_revision":revision,"room_document_id":document.0},"staged_change_count":0})).unwrap();
            let run = store
                .start_ai_run(
                    chat.id,
                    actor,
                    StartAiTurnRequest {
                        client_request_id: Uuid::new_v4(),
                        desktop_id: Uuid::new_v4(),
                        prompt: "edit".into(),
                        provider: AiProvider::Codex,
                        model: None,
                        mode: AiMode::Execute,
                        context: context.clone(),
                        attachment_previews: vec![],
                    },
                )
                .await
                .unwrap();
            store
                .bind_ai_execute_run_source(
                    run.run.id,
                    actor,
                    AiExecutionSource {
                        tenant_id: 1,
                        profile_id: profile.id.0,
                        session: SessionId(1),
                        connection: ConnectionId(1),
                        generation: Uuid::new_v4(),
                        profile_digest: store
                            .ai_profile_source_digest(profile.id, actor)
                            .await
                            .unwrap(),
                        database_label: "query".into(),
                        production: false,
                        engine: Engine::Postgres,
                    },
                )
                .await
                .unwrap();
            let proposal = store
                .stage_ai_query_proposal(
                    run.run.id,
                    actor,
                    StageAiQueryProposalRequest {
                        client_request_id: Uuid::new_v4(),
                        lease_token: run.lease_token,
                        target: context.target,
                        base_revision: revision,
                        proposed_sql: "SELECT 2".into(),
                    },
                )
                .await
                .unwrap();
            let request = store
                .create_ai_query_execution(
                    run.run.id,
                    actor,
                    RequestAiQueryExecution {
                        call_id: Uuid::new_v4(),
                        lease_token: run.lease_token,
                        proposal_id: proposal.proposal.id,
                    },
                    chrono::Utc::now() + chrono::Duration::minutes(4),
                    100,
                )
                .await
                .unwrap();
            let approval = ApproveAiQueryExecution {
                fingerprint: request.fingerprint.clone(),
                desktop_id: run.run.desktop_id,
            };
            let claim = store
                .claim_ai_query_execution(request.id, actor, approval.clone())
                .await
                .unwrap();
            assert!(claim.0 && claim.1.is_none());
            let limits = CollaborationLimits {
                max_document_update_bytes: if mode == "limit" { 0 } else { 1024 * 1024 },
                ..Default::default()
            };
            let mut native = DocumentActor::load(&store, document, limits).unwrap();
            if mode == "stale" {
                native
                    .author_replacement(&store, actor, "human", "human-edit", "SELECT 3")
                    .unwrap();
            }
            if mode == "stopped" {
                store
                    .finish_ai_run(run.run.id, actor, run.lease_token, AiRunStatus::Canceled)
                    .await
                    .unwrap();
            }
            if mode == "policy" {
                let mut changed = profile_input;
                changed.tags = vec!["changed".into()];
                store
                    .upsert_connection_profile(TenantId(1), actor, changed)
                    .await
                    .unwrap();
            }
            if mode == "revoked" {
                let peer = store
                    .create_principal("room owner", "Room Owner", None)
                    .unwrap()
                    .id;
                store
                    .upsert_tenant_membership(TenantId(1), peer, MembershipRole::Member)
                    .unwrap();
                let audit = NewOperationAudit {
                    actor_principal_id: Some(actor),
                    action: "test.room_membership".into(),
                    target: "room".into(),
                    target_id: Some(room.0),
                    status: "succeeded".into(),
                    result_code: None,
                    row_count: None,
                    error_message: None,
                    correlation_id: None,
                };
                store
                    .add_room_member_authorized(room, actor, peer, RoomRole::Owner, audit.clone())
                    .unwrap();
                store
                    .remove_room_member_authorized(room, peer, actor, audit)
                    .unwrap();
            }
            let authored = native.author_ai_query_execution(
                &store,
                actor,
                request.id,
                &request.fingerprint,
                "SELECT 1",
                "SELECT 2",
            );
            if mode == "applied" {
                let authored = authored.unwrap().unwrap();
                let seq = match authored.outcome {
                    ApplyOutcome::Applied { server_seq, .. } => server_seq,
                    _ => panic!("expected durable replacement"),
                };
                assert_eq!(native.text(), "SELECT 2");
                let receipt = store
                    .ai_query_execution(request.id, actor)
                    .await
                    .unwrap()
                    .receipt
                    .unwrap();
                assert_eq!(receipt.outcome, AiQueryEditOutcome::Applied);
                assert_eq!(receipt.evidence, AiQueryEditEvidence::RoomCommit);
                assert_eq!(receipt.document_sequence, Some(seq));
                assert_eq!(store.interrupt_ai_database_applies().await.unwrap(), 0);
            } else {
                assert!(authored.is_err(), "{mode}");
                assert_eq!(
                    store
                        .ai_query_execution(request.id, actor)
                        .await
                        .unwrap()
                        .state,
                    AiExecutionState::Applying,
                    "no receipt may attest a refused append"
                );
                store
                    .refuse_ai_query_document_execution(request.id, actor)
                    .await
                    .unwrap();
                let receipt = store
                    .ai_query_execution(request.id, actor)
                    .await
                    .unwrap()
                    .receipt
                    .unwrap();
                assert_eq!(receipt.outcome, AiQueryEditOutcome::Refused);
                assert_eq!(receipt.evidence, AiQueryEditEvidence::NoDispatch);
                assert_eq!(
                    native.text(),
                    if mode == "stale" {
                        "SELECT 3"
                    } else {
                        "SELECT 1"
                    }
                );
            }
            assert!(
                !store
                    .claim_ai_query_execution(request.id, actor, approval)
                    .await
                    .unwrap()
                    .0
            );
            let updates = store.list_document_updates_since(document, 0).unwrap();
            assert_eq!(
                updates
                    .iter()
                    .filter(|u| u.update_id == request.id.to_string())
                    .count(),
                usize::from(mode == "applied")
            );
            assert_eq!(
                DocumentActor::load(&store, document, CollaborationLimits::default())
                    .unwrap()
                    .text(),
                native.text()
            );
        }
    }
}
