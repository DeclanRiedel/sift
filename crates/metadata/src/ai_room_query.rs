//! Immutable human room-edit preparation and atomic CRDT/receipt settlement.
use super::{
    sqlite_blocking, DocumentId, MetadataError, MetadataStore, NewDocumentUpdate, PrincipalId,
    Result,
};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};
use sift_protocol::{
    AiProposalStatus, AiQueryProposalDetail, AiRoomQueryCommitReceipt,
    CommitAiRoomQueryProposalRequest,
};
use uuid::Uuid;

/// Server-internal capability: public wire inputs cannot choose text or actor.
#[derive(Clone)]
pub struct PreparedAiRoomQueryCommit {
    detail: AiQueryProposalDetail,
    original: String,
    actor: PrincipalId,
    auth_session: Option<String>,
    room: i64,
    document: DocumentId,
    tenant: i64,
    revision: u64,
    before: String,
    deadline: std::time::Instant,
}
impl PreparedAiRoomQueryCommit {
    /// The transport closes future admission on its original deadline; it never
    /// cancels an already admitted SQLite commit or switches to a new caller.
    pub fn with_deadline(mut self, deadline: std::time::Instant) -> Self {
        self.deadline = deadline;
        self
    }

    pub fn proposal(&self) -> &AiQueryProposalDetail {
        &self.detail
    }
    pub fn original_sql(&self) -> &str {
        &self.original
    }
    pub fn actor(&self) -> PrincipalId {
        self.actor
    }
    pub fn room_id(&self) -> i64 {
        self.room
    }
    pub fn document_id(&self) -> DocumentId {
        self.document
    }
    pub fn tenant_id(&self) -> i64 {
        self.tenant
    }
    pub fn committed_proposal(&self, receipt: &AiRoomQueryCommitReceipt) -> AiQueryProposalDetail {
        let mut detail = self.detail.clone();
        detail.proposal.status = AiProposalStatus::Applied;
        detail.proposal.applied_by = Some(receipt.applied_by);
        detail.proposal.updated_at = receipt.applied_at;
        detail
    }
}
fn invalid(message: &str) -> MetadataError {
    MetadataError::AiInvalid(message.into())
}
fn hash(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}
fn revision(text: &str) -> u64 {
    let digest = Sha256::digest(text.as_bytes());
    u64::from_le_bytes(digest[..8].try_into().expect("SHA-256 prefix")) & i64::MAX as u64
}
fn receipt(
    conn: &Connection,
    prepared: &PreparedAiRoomQueryCommit,
) -> Result<Option<AiRoomQueryCommitReceipt>> {
    super::ai::require_chat_access(conn, prepared.detail.proposal.chat_id, prepared.actor)?;
    let row = conn.query_row(
        "SELECT document_id,document_sequence,before_sha256,after_sha256,applied_by,applied_at FROM ai_room_query_commit WHERE proposal_id=?1",
        [prepared.detail.proposal.id.to_string()], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,i64>(4)?,r.get::<_,String>(5)?)),
    ).optional()?;
    row.map(|(document, seq, before, after, actor, at)| {
        if document != prepared.document.0
            || before != prepared.before
            || after != prepared.detail.proposal.content_sha256
            || actor != prepared.actor.0
        {
            return Err(invalid("Room commit identity changed"));
        }
        Ok(AiRoomQueryCommitReceipt {
            proposal_id: prepared.detail.proposal.id,
            document_id: document,
            document_sequence: seq,
            before_sha256: before,
            after_sha256: after,
            applied_by: actor,
            applied_at: chrono::DateTime::parse_from_rfc3339(&at)
                .map_err(|_| invalid("Room commit timestamp is invalid"))?
                .with_timezone(&Utc),
        })
    })
    .transpose()
}
impl MetadataStore {
    pub async fn prepare_ai_room_query_commit(
        &self,
        chat: Uuid,
        proposal: Uuid,
        actor: PrincipalId,
        auth_session: Option<String>,
        request: CommitAiRoomQueryProposalRequest,
    ) -> Result<PreparedAiRoomQueryCommit> {
        let detail = self
            .list_ai_query_proposals(chat, actor)
            .await?
            .into_iter()
            .find(|detail| detail.proposal.id == proposal)
            .ok_or(MetadataError::AiNotFound)?;
        if !matches!(
            detail.proposal.status,
            AiProposalStatus::Staged | AiProposalStatus::Applied
        ) || detail.proposal.base_revision != Some(request.expected_revision)
            || detail.proposal.content_sha256 != request.expected_content_sha256
            || hash(&detail.proposed_sql) != request.expected_content_sha256
        {
            return Err(invalid("Exact room proposal approval changed"));
        }
        let run = self
            .ai_run_detail(chat, detail.proposal.run_id, actor)
            .await?;
        let context = run.context;
        let sql = context
            .sql
            .ok_or_else(|| invalid("Original room query text is unavailable"))?;
        let target = &detail.proposal.target;
        let document = target
            .document_id
            .as_deref()
            .and_then(|id| id.parse::<i64>().ok())
            .filter(|id| *id > 0)
            .ok_or_else(|| invalid("A room query document is required"))?;
        let room = target
            .room_id
            .filter(|id| *id > 0)
            .ok_or_else(|| invalid("A room query document is required"))?;
        let tenant = target
            .tenant_id
            .filter(|id| *id > 0)
            .ok_or_else(|| invalid("A tenant is required"))?;
        if context.target != *target
            || sql.room_document_id != Some(document)
            || revision(&sql.text) != request.expected_revision
        {
            return Err(invalid("Original room query binding changed"));
        }
        Ok(PreparedAiRoomQueryCommit {
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(300),
            before: hash(&sql.text),
            original: sql.text,
            detail,
            actor,
            auth_session,
            room,
            document: DocumentId(document),
            tenant,
            revision: request.expected_revision,
        })
    }

    pub fn ai_room_query_commit_receipt_now(
        &self,
        prepared: &PreparedAiRoomQueryCommit,
    ) -> Result<Option<AiRoomQueryCommitReceipt>> {
        let conn = self.conn()?;
        receipt(&conn, prepared)
    }

    pub async fn ai_room_query_commit_receipt(
        &self,
        prepared: PreparedAiRoomQueryCommit,
    ) -> Result<Option<AiRoomQueryCommitReceipt>> {
        let store = self.clone();
        sqlite_blocking(move || store.ai_room_query_commit_receipt_now(&prepared)).await
    }

    /// Called only by the native document owner after verifying full before/after
    /// text. The update, proposal/event and durable sequence proof commit once.
    pub fn append_ai_room_query_proposal_update(
        &self,
        prepared: &PreparedAiRoomQueryCommit,
        update: NewDocumentUpdate,
    ) -> Result<i64> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if std::time::Instant::now() >= prepared.deadline {
            return Err(invalid(
                "Room query approval deadline closed; inspect its receipt before retrying",
            ));
        }
        if receipt(&tx, prepared)?.is_some() {
            return Err(invalid("Room query proposal was already committed"));
        }
        let valid: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM document d JOIN room r ON r.id=d.room_id JOIN room_member m ON m.room_id=r.id JOIN principal p ON p.id=m.principal_id JOIN ai_proposal a ON a.id=?1 JOIN ai_chat c ON c.id=a.chat_id WHERE d.kind='sql' AND d.id=?2 AND r.id=?3 AND r.tenant_id=?4 AND c.tenant_id=r.tenant_id AND (c.room_id IS NULL OR c.room_id=r.id) AND m.principal_id=?5 AND m.role IN ('owner','editor') AND p.disabled_at IS NULL AND a.status='staged' AND a.base_revision=?6 AND a.content_sha256=?7)",
            params![prepared.detail.proposal.id.to_string(),prepared.document.0,prepared.room,prepared.tenant,prepared.actor.0,prepared.revision,prepared.detail.proposal.content_sha256], |r| r.get(0),
        )?;
        if !valid
            || update.submitted_by != prepared.actor
            || update.replica_id != "sift-ai-room-proposal"
            || update.update_id != prepared.detail.proposal.id.to_string()
        {
            return Err(MetadataError::AiAccessDenied);
        }
        if let Some(session) = &prepared.auth_session {
            let live: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM auth_session WHERE id=?1 AND principal_id=?2 AND revoked_at IS NULL AND julianday(expires_at)>julianday('now'))", params![session,prepared.actor.0], |r| r.get(0))?;
            if !live {
                return Err(MetadataError::AiAccessDenied);
            }
        }
        super::ai_mutation_admission::require_clear(
            &tx,
            &prepared.detail.proposal.chat_id.to_string(),
        )?;
        let seq = super::append_document_update_locked(&tx, prepared.document, update)?;
        let now = Utc::now().to_rfc3339();
        super::ai_proposal::mark_applied_locked(
            &tx,
            prepared.detail.proposal.chat_id,
            prepared.detail.proposal.id,
            prepared.actor,
            prepared.revision,
            &now,
        )?;
        tx.execute(
            "INSERT INTO ai_room_query_commit VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                prepared.detail.proposal.id.to_string(),
                prepared.document.0,
                seq,
                prepared.before,
                prepared.detail.proposal.content_sha256,
                prepared.actor.0,
                now
            ],
        )?;
        tx.commit()?;
        Ok(seq)
    }
}
