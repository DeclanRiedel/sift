//! Encrypted typed proposals, human previews and durable single-dispatch claims.
use super::{sqlite_blocking, MetadataError, MetadataStore, PrincipalId, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sift_protocol::{
    AiDatabaseApplyState, AiDatabaseDraft, AiDatabaseProposalDetail, AiProposal, AiProposalKind,
    AiProposalStatus,
};
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
struct StoredDraft {
    draft: AiDatabaseDraft,
    source_digest: String,
    database_identity: String,
    publication_id: Option<Uuid>,
}

struct ProposalRecord {
    id: String,
    chat: String,
    run: String,
    kind: String,
    status: String,
    target: String,
    content: String,
    digest: String,
    author: i64,
    approver: Option<i64>,
    created: String,
    updated: String,
    apply: Option<String>,
}
fn record(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProposalRecord> {
    Ok(ProposalRecord {
        id: row.get(0)?,
        chat: row.get(1)?,
        run: row.get(2)?,
        kind: row.get(3)?,
        status: row.get(4)?,
        target: row.get(5)?,
        content: row.get(6)?,
        digest: row.get(7)?,
        author: row.get(8)?,
        approver: row.get(9)?,
        created: row.get(10)?,
        updated: row.get(11)?,
        apply: row.get(12)?,
    })
}
const SELECT_PROPOSAL: &str="SELECT p.id,p.chat_id,p.run_id,p.kind,p.status,p.target_handle,p.content_handle,p.content_sha256,p.created_by,p.applied_by,p.created_at,p.updated_at,a.state FROM ai_proposal p LEFT JOIN ai_proposal_apply a ON a.proposal_id=p.id";
fn uuid(id: &str) -> Result<Uuid> {
    Uuid::parse_str(id).map_err(|_| MetadataError::AiContent("invalid AI proposal identity".into()))
}
fn apply_state(value: &str) -> Result<AiDatabaseApplyState> {
    match value {
        "applying" => Ok(AiDatabaseApplyState::Applying),
        "applied" => Ok(AiDatabaseApplyState::Applied),
        "failed" => Ok(AiDatabaseApplyState::Failed),
        "outcome_unknown" => Ok(AiDatabaseApplyState::OutcomeUnknown),
        _ => Err(MetadataError::AiContent("invalid AI apply state".into())),
    }
}
fn reviewer(conn: &Connection, chat: Uuid, actor: PrincipalId) -> Result<i64> {
    let tenant = super::ai::require_chat_access(conn, chat, actor)?;
    let allowed:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM ai_chat c WHERE c.id=?1 AND (c.visibility='private' AND c.owner_principal_id=?2 OR c.visibility='room_public' AND EXISTS(SELECT 1 FROM room_member m WHERE m.room_id=c.room_id AND m.principal_id=?2 AND m.role IN ('owner','editor'))))",params![chat.to_string(),actor.0],|row|row.get(0))?;
    if !allowed {
        return Err(MetadataError::AiAccessDenied);
    }
    Ok(tenant)
}
impl MetadataStore {
    /// Scope lookup only; visibility is independently checked before reading bodies.
    pub async fn ai_database_proposal_chat(&self, id: Uuid) -> Result<Uuid> {
        let store = self.clone();
        sqlite_blocking(move|| {
            let conn=store.conn()?;
            let id:String=conn.query_row("SELECT chat_id FROM ai_proposal WHERE id=?1 AND kind IN ('row_edit_set','migration_draft')",[id.to_string()],|row|row.get(0)).optional()?.ok_or(MetadataError::AiNotFound)?;
            uuid(&id)
        }).await
    }
    pub async fn ai_database_proposal(
        &self,
        id: Uuid,
        actor: PrincipalId,
    ) -> Result<AiDatabaseProposalDetail> {
        let store = self.clone();
        let (tenant,record)=sqlite_blocking(move|| {
            let conn=store.conn()?;
            let row=conn.query_row(&format!("{SELECT_PROPOSAL} WHERE p.id=?1 AND p.kind IN ('row_edit_set','migration_draft')"),[id.to_string()],record).optional()?.ok_or(MetadataError::AiNotFound)?;
            let tenant=super::ai::require_chat_access(&conn,uuid(&row.chat)?,actor)?;
            Ok((tenant,row))
        }).await?;
        let target = self
            .ai_content
            .get(tenant, &record.target)
            .await?
            .ok_or_else(|| MetadataError::AiContent("AI proposal target unavailable".into()))?;
        let content = self
            .ai_content
            .get(tenant, &record.content)
            .await?
            .ok_or_else(|| MetadataError::AiContent("AI proposal body unavailable".into()))?;
        if format!("{:x}", Sha256::digest(&content)) != record.digest {
            return Err(MetadataError::AiContent(
                "AI proposal digest mismatch".into(),
            ));
        }
        let stored: StoredDraft = serde_json::from_slice(&content)?;
        let kind = match record.kind.as_str() {
            "row_edit_set" => AiProposalKind::RowEditSet,
            "migration_draft" => AiProposalKind::MigrationDraft,
            _ => return Err(MetadataError::AiNotFound),
        };
        let status = match record.status.as_str() {
            "staged" => AiProposalStatus::Staged,
            "applied" => AiProposalStatus::Applied,
            "discarded" => AiProposalStatus::Discarded,
            "conflicted" => AiProposalStatus::Conflicted,
            _ => {
                return Err(MetadataError::AiContent(
                    "AI proposal status is invalid".into(),
                ))
            }
        };
        Ok(AiDatabaseProposalDetail {
            proposal: AiProposal {
                id: uuid(&record.id)?,
                chat_id: uuid(&record.chat)?,
                run_id: uuid(&record.run)?,
                kind,
                status,
                target: serde_json::from_slice(&target)?,
                base_revision: None,
                content_sha256: record.digest,
                created_by: record.author,
                applied_by: record.approver,
                created_at: super::parse_time_sql(record.created)?,
                updated_at: super::parse_time_sql(record.updated)?,
            },
            draft: stored.draft,
            source_digest: stored.source_digest,
            database_identity: stored.database_identity,
            publication_id: stored.publication_id,
            apply_state: record.apply.as_deref().map(apply_state).transpose()?,
        })
    }
    pub async fn ai_database_proposals(
        &self,
        chat: Uuid,
        actor: PrincipalId,
    ) -> Result<Vec<AiDatabaseProposalDetail>> {
        let store = self.clone();
        let ids=sqlite_blocking(move|| {
            let conn=store.conn()?;super::ai::require_chat_access(&conn,chat,actor)?;
            let mut statement=conn.prepare("SELECT id FROM ai_proposal WHERE chat_id=?1 AND kind IN ('row_edit_set','migration_draft') ORDER BY created_at,id LIMIT 200")?;
            let ids=statement.query_map([chat.to_string()],|row|row.get::<_,String>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
            ids.into_iter().map(|id|uuid(&id)).collect::<Result<Vec<_>>>()
        }).await?;
        let mut result = Vec::with_capacity(ids.len());
        for id in ids {
            result.push(self.ai_database_proposal(id, actor).await?);
        }
        Ok(result)
    }
}

fn validate_draft(draft: &AiDatabaseDraft) -> Result<()> {
    let invalid = || {
        MetadataError::AiInvalid(
            "AI database draft requires bounded typed changes, stable row keys and original values"
                .into(),
        )
    };
    match draft {
        AiDatabaseDraft::RowEditSet {
            edit_set,
            expected_catalog_revision,
        } => {
            if expected_catalog_revision.0 == 0
                || edit_set.edits.is_empty()
                || edit_set.edits.len() > 100
                || edit_set.table.name.is_empty()
                || edit_set.table.name.len() > 256
            {
                return Err(invalid());
            }
            for edit in &edit_set.edits {
                let (values, key, expected) = match edit {
                    sift_protocol::RowEdit::Insert { values } => (values, None, None),
                    sift_protocol::RowEdit::Update {
                        key,
                        changes,
                        expected,
                    } => (changes, Some(key), Some(expected)),
                    sift_protocol::RowEdit::Delete { key, expected } => {
                        (expected, Some(key), Some(expected))
                    }
                };
                if values.is_empty()
                    || values.len() > 256
                    || values
                        .iter()
                        .any(|value| value.column.is_empty() || value.column.len() > 256)
                {
                    return Err(invalid());
                }
                if key.is_some_and(|key| key.columns.is_empty() || key.columns.len() > 64)
                    || expected.is_some_and(|expected| expected.is_empty() || expected.len() > 256)
                {
                    return Err(invalid());
                }
            }
        }
        AiDatabaseDraft::MigrationDraft {
            desired_catalog,
            expected_catalog_revision,
            ..
        } => {
            if expected_catalog_revision.0 == 0
                || desired_catalog.data.nodes.len() > 2000
                || desired_catalog.data.edges.len() > 10000
            {
                return Err(invalid());
            }
        }
    }
    Ok(())
}
impl MetadataStore {
    #[allow(clippy::too_many_arguments)]
    pub async fn stage_ai_database_proposal(
        &self,
        run: Uuid,
        actor: PrincipalId,
        request: sift_protocol::StageAiDatabaseProposalRequest,
        source_digest: String,
        database_identity: String,
        publication_id: Option<Uuid>,
    ) -> Result<AiDatabaseProposalDetail> {
        validate_draft(&request.draft)?;
        if request.client_request_id.is_nil()
            || source_digest.len() != 64
            || database_identity.is_empty()
            || database_identity.len() > 256
        {
            return Err(MetadataError::AiInvalid(
                "AI database source identity is invalid".into(),
            ));
        }
        let authorized = self.ai_tool_run(run, actor, request.lease_token).await?;
        if authorized.mode != sift_protocol::AiMode::Propose
            || authorized.context.target.profile_id.is_none()
        {
            return Err(MetadataError::AiInvalid(
                "Propose mode with a bound database is required".into(),
            ));
        }
        if authorized.visibility == sift_protocol::AiVisibility::RoomPublic {
            let room = authorized
                .context
                .target
                .room_id
                .ok_or(MetadataError::AiAccessDenied)?;
            let grant = self
                .ai_room_publication(super::RoomId(room), actor)
                .await?
                .ok_or(MetadataError::AiAccessDenied)?;
            if Some(grant.id) != publication_id
                || publication_id != authorized.context.publication_id
                || grant.source.scope_digest != source_digest
                || matches!(request.draft, AiDatabaseDraft::RowEditSet { .. }) && !grant.allow_rows
            {
                return Err(MetadataError::AiAccessDenied);
            }
        } else {
            if publication_id.is_some()
                || self
                    .ai_profile_source_digest(
                        super::ConnectionProfileId(
                            authorized.context.target.profile_id.expect("profile"),
                        ),
                        actor,
                    )
                    .await?
                    != source_digest
            {
                return Err(MetadataError::AiAccessDenied);
            }
        }
        let chat = authorized.chat_id;
        let kind = match request.draft {
            AiDatabaseDraft::RowEditSet { .. } => "row_edit_set",
            AiDatabaseDraft::MigrationDraft { .. } => "migration_draft",
        };
        let content = serde_json::to_vec(&StoredDraft {
            draft: request.draft,
            source_digest,
            database_identity,
            publication_id,
        })?;
        if content.len() > super::ai_content::MAX_CONTENT_BYTES {
            return Err(MetadataError::AiInvalid(
                "AI database draft exceeds the encrypted body limit".into(),
            ));
        }
        let digest = format!("{:x}", Sha256::digest(&content));
        let target = serde_json::to_vec(&authorized.context.target)?;
        let store = self.clone();
        let tenant = sqlite_blocking(move || {
            let conn = store.conn()?;
            reviewer(&conn, chat, actor)
        })
        .await?;
        let content_handle = self.ai_content.put(tenant, &content).await?;
        let target_handle = match self.ai_content.put(tenant, &target).await {
            Ok(handle) => handle,
            Err(error) => {
                self.queue_ai_database_cleanup(tenant, vec![content_handle])
                    .await?;
                return Err(error);
            }
        };
        let id = Uuid::new_v4();
        let now = Utc::now();
        let saved_content = content_handle.clone();
        let saved_target = target_handle.clone();
        let saved_digest = digest.clone();
        let store = self.clone();
        let result=sqlite_blocking(move|| {
            let mut conn=store.conn()?;let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            reviewer(&tx,chat,actor)?;
            let (_,status)=super::ai_run::ai_run_authorized_conn(&tx,run,actor,request.lease_token)?;
            if status!="running" {return Err(MetadataError::AiInvalid("AI run is no longer active".into()));}
            if let Some(grant)=publication_id {
                let active:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM ai_room_publication g JOIN ai_chat c ON c.room_id=g.room_id WHERE c.id=?1 AND g.id=?2 AND g.revoked_at IS NULL)",params![chat.to_string(),grant.to_string()],|row|row.get(0))?;
                if !active {return Err(MetadataError::AiAccessDenied);}
            }
            let prior:Option<(String,String,String)>=tx.query_row("SELECT id,content_sha256,kind FROM ai_proposal WHERE run_id=?1 AND client_request_id=?2",params![run.to_string(),request.client_request_id.to_string()],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?;
            if let Some((prior,digest,prior_kind))=prior {
                if digest!=saved_digest || prior_kind!=kind {return Err(MetadataError::AiInvalid("proposal request ID was reused with different content".into()));}
                return Ok((uuid(&prior)?,false));
            }
            let count:u64=tx.query_row("SELECT COUNT(*) FROM ai_proposal WHERE chat_id=?1",[chat.to_string()],|row|row.get(0))?;
            if count>=200 {return Err(MetadataError::AiInvalid("AI chat proposal limit reached".into()));}
            tx.execute("INSERT INTO ai_proposal(id,client_request_id,chat_id,run_id,kind,status,target_handle,content_handle,content_sha256,created_by,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,'staged',?6,?7,?8,?9,?10,?10)",params![id.to_string(),request.client_request_id.to_string(),chat.to_string(),run.to_string(),kind,saved_target,saved_content,saved_digest,actor.0,now.to_rfc3339()])?;
            let sequence:u64=tx.query_row("SELECT next_sequence FROM ai_run WHERE id=?1",[run.to_string()],|row|row.get(0))?;
            if sequence>1024 {return Err(MetadataError::AiInvalid("AI run event limit reached".into()));}
            tx.execute("INSERT INTO ai_run_event(run_id,sequence,kind,at,proposal_id) VALUES(?1,?2,'proposal_created',?3,?4)",params![run.to_string(),sequence,now.to_rfc3339(),id.to_string()])?;
            tx.execute("UPDATE ai_run SET next_sequence=next_sequence+1 WHERE id=?1",[run.to_string()])?;
            tx.execute("UPDATE ai_chat SET updated_at=?2,revision=revision+1 WHERE id=?1",params![chat.to_string(),Utc::now().to_rfc3339()])?;
            tx.commit()?;Ok((id,true))
        }).await;
        if !matches!(result, Ok((_, true))) {
            self.queue_ai_database_cleanup(tenant, vec![content_handle, target_handle])
                .await?;
        }
        self.ai_database_proposal(result?.0, actor).await
    }
    async fn queue_ai_database_cleanup(&self, tenant: i64, handles: Vec<String>) -> Result<()> {
        let store = self.clone();
        sqlite_blocking(move|| {
            let mut conn=store.conn()?;let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            for handle in handles {tx.execute("INSERT OR IGNORE INTO ai_content_cleanup(tenant_id,content_handle,queued_at) VALUES(?1,?2,?3)",params![tenant,handle,Utc::now().to_rfc3339()])?;}
            tx.commit()?;Ok(())
        }).await
    }
}

impl MetadataStore {
    pub async fn ai_database_review(
        &self,
        id: Uuid,
        actor: PrincipalId,
    ) -> Result<sift_protocol::AiDatabaseProposalReview> {
        let store = self.clone();
        let (tenant,handle,digest)=sqlite_blocking(move|| {
            let conn=store.conn()?;
            let (tenant,chat,reviewer_id,handle,digest):(i64,String,i64,String,String)=conn.query_row("SELECT r.tenant_id,p.chat_id,r.reviewer_id,r.content_handle,r.review_digest FROM ai_proposal_review r JOIN ai_proposal p ON p.id=r.proposal_id WHERE r.id=?1",[id.to_string()],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))).optional()?.ok_or(MetadataError::AiNotFound)?;
            reviewer(&conn,uuid(&chat)?,actor)?;
            if reviewer_id!=actor.0 {return Err(MetadataError::AiAccessDenied);}
            Ok((tenant,handle,digest))
        }).await?;
        let bytes = self
            .ai_content
            .get(tenant, &handle)
            .await?
            .ok_or_else(|| MetadataError::AiContent("AI review body unavailable".into()))?;
        let mut review: sift_protocol::AiDatabaseProposalReview = serde_json::from_slice(&bytes)?;
        let recorded = std::mem::take(&mut review.review_digest);
        if recorded != digest
            || format!("{:x}", Sha256::digest(serde_json::to_vec(&review)?)) != digest
        {
            return Err(MetadataError::AiContent("AI review digest mismatch".into()));
        }
        review.review_digest = recorded;
        Ok(review)
    }
    pub async fn prior_ai_database_review(
        &self,
        proposal: Uuid,
        actor: PrincipalId,
        client_request_id: Uuid,
    ) -> Result<Option<sift_protocol::AiDatabaseProposalReview>> {
        let store = self.clone();
        let id: Option<String> = sqlite_blocking(move || {
            let conn = store.conn()?;
            let chat: String = conn
                .query_row(
                    "SELECT chat_id FROM ai_proposal WHERE id=?1",
                    [proposal.to_string()],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or(MetadataError::AiNotFound)?;
            reviewer(&conn, uuid(&chat)?, actor)?;
            conn.query_row(
                "SELECT id FROM ai_proposal_review WHERE proposal_id=?1 AND client_request_id=?2",
                params![proposal.to_string(), client_request_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
        })
        .await?;
        match id {
            Some(id) => self.ai_database_review(uuid(&id)?, actor).await.map(Some),
            None => Ok(None),
        }
    }
    pub async fn save_ai_database_review(
        &self,
        actor: PrincipalId,
        client_request_id: Uuid,
        mut review: sift_protocol::AiDatabaseProposalReview,
    ) -> Result<sift_protocol::AiDatabaseProposalReview> {
        if client_request_id.is_nil()
            || review.id.is_nil()
            || review.reviewer_id != actor.0
            || review.source_digest.len() != 64
            || review.content_sha256.len() != 64
            || review.expires_at <= Utc::now()
            || review.expires_at > Utc::now() + chrono::Duration::minutes(10)
        {
            return Err(MetadataError::AiInvalid(
                "AI review identity or deadline is invalid".into(),
            ));
        }
        review.review_digest.clear();
        review.review_digest = format!("{:x}", Sha256::digest(serde_json::to_vec(&review)?));
        let proposal = self.ai_database_proposal(review.proposal_id, actor).await?;
        if proposal.proposal.status != AiProposalStatus::Staged
            || proposal.apply_state.is_some()
            || proposal.source_digest != review.source_digest
            || proposal.proposal.content_sha256 != review.content_sha256
        {
            return Err(MetadataError::AiInvalid(
                "AI proposal changed or was already consumed".into(),
            ));
        }
        let store = self.clone();
        let chat = proposal.proposal.chat_id;
        let tenant = sqlite_blocking(move || {
            let conn = store.conn()?;
            reviewer(&conn, chat, actor)
        })
        .await?;
        let content_handle = self
            .ai_content
            .put(tenant, &serde_json::to_vec(&review)?)
            .await?;
        let saved_handle = content_handle.clone();
        let saved = review.clone();
        let store = self.clone();
        let result=sqlite_blocking(move|| {
            let mut conn=store.conn()?;let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            reviewer(&tx,chat,actor)?;
            let staged:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM ai_proposal p WHERE p.id=?1 AND p.status='staged' AND p.content_sha256=?2 AND NOT EXISTS(SELECT 1 FROM ai_proposal_apply a WHERE a.proposal_id=p.id))",params![saved.proposal_id.to_string(),saved.content_sha256],|row|row.get(0))?;
            if !staged {return Err(MetadataError::AiInvalid("AI proposal changed or was already consumed".into()));}
            let prior:Option<String>=tx.query_row("SELECT id FROM ai_proposal_review WHERE proposal_id=?1 AND client_request_id=?2",params![saved.proposal_id.to_string(),client_request_id.to_string()],|row|row.get(0)).optional()?;
            if let Some(prior)=prior {return Ok((uuid(&prior)?,false));}
            let count:u64=tx.query_row("SELECT COUNT(*) FROM ai_proposal_review WHERE proposal_id=?1",[saved.proposal_id.to_string()],|row|row.get(0))?;
            if count>=20 {return Err(MetadataError::AiInvalid("AI proposal review limit reached".into()));}
            tx.execute("INSERT INTO ai_proposal_review(id,client_request_id,tenant_id,proposal_id,reviewer_id,content_handle,review_digest,created_at,expires_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![saved.id.to_string(),client_request_id.to_string(),tenant,saved.proposal_id.to_string(),actor.0,saved_handle,saved.review_digest,Utc::now().to_rfc3339(),saved.expires_at.to_rfc3339()])?;
            tx.execute("UPDATE ai_chat SET updated_at=?2,revision=revision+1 WHERE id=?1",params![chat.to_string(),Utc::now().to_rfc3339()])?;
            tx.commit()?;Ok((saved.id,true))
        }).await;
        if !matches!(result, Ok((_, true))) {
            self.queue_ai_database_cleanup(tenant, vec![content_handle])
                .await?;
        }
        let saved = self.ai_database_review(result?.0, actor).await?;
        if saved.session != review.session
            || saved.connection != review.connection
            || saved.source_digest != review.source_digest
            || saved.content_sha256 != review.content_sha256
        {
            return Err(MetadataError::AiInvalid(
                "review request ID was reused with another target".into(),
            ));
        }
        Ok(saved)
    }
}

/// Only a fresh durable claim authorizes dispatch. Other states are observations.
pub enum AiDatabaseApplyClaim {
    Claimed,
    Replay(Box<sift_protocol::AiDatabaseApplyReceipt>),
    Pending(AiDatabaseApplyState),
}
fn request_digest(request: &sift_protocol::ApplyAiDatabaseProposalRequest) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(request)?)
    ))
}
impl MetadataStore {
    pub async fn prior_ai_database_apply(
        &self,
        proposal: Uuid,
        actor: PrincipalId,
        request: sift_protocol::ApplyAiDatabaseProposalRequest,
    ) -> Result<Option<AiDatabaseApplyClaim>> {
        let store = self.clone();
        let claimed = sqlite_blocking(move || {
            let conn = store.conn()?;
            let chat: String = conn
                .query_row(
                    "SELECT chat_id FROM ai_proposal WHERE id=?1",
                    [proposal.to_string()],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or(MetadataError::AiNotFound)?;
            reviewer(&conn, uuid(&chat)?, actor)?;
            conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM ai_proposal_apply WHERE proposal_id=?1)",
                [proposal.to_string()],
                |row| row.get::<_, bool>(0),
            )
            .map_err(Into::into)
        })
        .await?;
        if claimed {
            self.claim_ai_database_apply(proposal, actor, request)
                .await
                .map(Some)
        } else {
            Ok(None)
        }
    }
    pub async fn claim_ai_database_apply(
        &self,
        proposal: Uuid,
        actor: PrincipalId,
        request: sift_protocol::ApplyAiDatabaseProposalRequest,
    ) -> Result<AiDatabaseApplyClaim> {
        if request.client_request_id.is_nil() {
            return Err(MetadataError::AiInvalid(
                "apply request identity is invalid".into(),
            ));
        }
        let digest = request_digest(&request)?;
        let store = self.clone();
        let record=sqlite_blocking(move|| {
            let mut conn=store.conn()?;let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let chat:String=tx.query_row("SELECT chat_id FROM ai_proposal WHERE id=?1 AND kind IN ('row_edit_set','migration_draft')",[proposal.to_string()],|row|row.get(0)).optional()?.ok_or(MetadataError::AiNotFound)?;
            let tenant=reviewer(&tx,uuid(&chat)?,actor)?;
            let prior:Option<(String,String,i64,String,Option<String>)>=tx.query_row("SELECT client_request_id,request_digest,approved_by,state,receipt_handle FROM ai_proposal_apply WHERE proposal_id=?1",[proposal.to_string()],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))).optional()?;
            if let Some((client,prior_digest,approver,state,handle))=prior {
                if client!=request.client_request_id.to_string() || prior_digest!=digest || approver!=actor.0 {return Err(MetadataError::AiInvalid("AI proposal was already claimed; database work will not be repeated".into()));}
                return Ok(Some((tenant,state,handle)));
            }
            let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM ai_proposal_review r JOIN ai_proposal p ON p.id=r.proposal_id WHERE p.id=?1 AND p.status='staged' AND r.id=?2 AND r.reviewer_id=?3 AND r.review_digest=?4 AND julianday(r.expires_at)>julianday(?5))",params![proposal.to_string(),request.review_id.to_string(),actor.0,request.review_digest,Utc::now().to_rfc3339()],|row|row.get(0))?;
            if !valid {return Err(MetadataError::AiInvalid("AI proposal review is unavailable, stale or no longer staged".into()));}
            tx.execute("INSERT INTO ai_proposal_apply(proposal_id,tenant_id,review_id,client_request_id,request_digest,approved_by,state,claimed_at) VALUES(?1,?2,?3,?4,?5,?6,'applying',?7)",params![proposal.to_string(),tenant,request.review_id.to_string(),request.client_request_id.to_string(),digest,actor.0,Utc::now().to_rfc3339()])?;
            tx.execute("UPDATE ai_chat SET updated_at=?2,revision=revision+1 WHERE id=?1",params![chat.to_string(),Utc::now().to_rfc3339()])?;
            tx.commit()?;Ok(None)
        }).await?;
        if let Some((tenant, state, handle)) = record {
            if let Some(handle) = handle {
                let bytes = self.ai_content.get(tenant, &handle).await?.ok_or_else(|| {
                    MetadataError::AiContent("AI apply receipt unavailable".into())
                })?;
                let receipt: sift_protocol::AiDatabaseApplyReceipt =
                    serde_json::from_slice(&bytes)?;
                if receipt.proposal_id != proposal || receipt.state != apply_state(&state)? {
                    return Err(MetadataError::AiContent(
                        "AI receipt identity is invalid".into(),
                    ));
                }
                Ok(AiDatabaseApplyClaim::Replay(Box::new(receipt)))
            } else {
                Ok(AiDatabaseApplyClaim::Pending(apply_state(&state)?))
            }
        } else {
            Ok(AiDatabaseApplyClaim::Claimed)
        }
    }
    pub async fn finish_ai_database_apply(
        &self,
        actor: PrincipalId,
        request: sift_protocol::ApplyAiDatabaseProposalRequest,
        receipt: sift_protocol::AiDatabaseApplyReceipt,
    ) -> Result<sift_protocol::AiDatabaseApplyReceipt> {
        if receipt.state == AiDatabaseApplyState::Applying {
            return Err(MetadataError::AiInvalid(
                "AI apply receipt must describe a finished outcome".into(),
            ));
        }
        // Trusted execution completion must record its outcome even when
        // permission was revoked while the external database was committing.
        let store = self.clone();
        let proposal_id = receipt.proposal_id;
        let proof = request_digest(&request)?;
        let client_id = request.client_request_id;
        let (tenant,chat)=sqlite_blocking(move|| {
            let conn=store.conn()?;
            let (tenant,chat):(i64,String)=conn.query_row("SELECT a.tenant_id,p.chat_id FROM ai_proposal_apply a JOIN ai_proposal p ON p.id=a.proposal_id WHERE a.proposal_id=?1 AND a.approved_by=?2 AND a.client_request_id=?3 AND a.request_digest=?4",params![proposal_id.to_string(),actor.0,client_id.to_string(),proof],|row|Ok((row.get(0)?,row.get(1)?))).optional()?.ok_or(MetadataError::AiAccessDenied)?;
            Ok((tenant,uuid(&chat)?))
        }).await?;
        let content_handle = self
            .ai_content
            .put(tenant, &serde_json::to_vec(&receipt)?)
            .await?;
        let saved_handle = content_handle.clone();
        let saved = receipt.clone();
        let digest = request_digest(&request)?;
        let store = self.clone();
        let result=sqlite_blocking(move|| {
            let mut conn=store.conn()?;let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let matching:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM ai_proposal_apply WHERE proposal_id=?1 AND review_id=?2 AND client_request_id=?3 AND request_digest=?4 AND approved_by=?5)",params![saved.proposal_id.to_string(),request.review_id.to_string(),request.client_request_id.to_string(),digest,actor.0],|row|row.get(0))?;
            if !matching {return Err(MetadataError::AiInvalid("AI application receipt does not match its durable claim".into()));}
            let state=match saved.state {AiDatabaseApplyState::Applied=>"applied",AiDatabaseApplyState::Failed=>"failed",AiDatabaseApplyState::OutcomeUnknown=>"outcome_unknown",AiDatabaseApplyState::Applying=>unreachable!()};
            let changed=tx.execute("UPDATE ai_proposal_apply SET state=?2,receipt_handle=?3,finished_at=?4 WHERE proposal_id=?1 AND receipt_handle IS NULL",params![saved.proposal_id.to_string(),state,saved_handle,Utc::now().to_rfc3339()])?;
            if changed==0 {return Ok(false);}
            let status=if saved.state==AiDatabaseApplyState::Applied {"applied"} else if saved.state==AiDatabaseApplyState::Failed {"conflicted"} else {"staged"};
            tx.execute("UPDATE ai_proposal SET status=?2,applied_by=?3,updated_at=?4 WHERE id=?1",params![saved.proposal_id.to_string(),status,actor.0,Utc::now().to_rfc3339()])?;
            if saved.state==AiDatabaseApplyState::Applied {
                let run:String=tx.query_row("SELECT run_id FROM ai_proposal WHERE id=?1",[saved.proposal_id.to_string()],|row|row.get(0))?;
                let sequence:u64=tx.query_row("SELECT next_sequence FROM ai_run WHERE id=?1",[&run],|row|row.get(0))?;
                // Receipt storage must succeed even when the provider consumed
                // the event budget before human review.
                if sequence<=1024 {
                    tx.execute("INSERT INTO ai_run_event(run_id,sequence,kind,at,proposal_id) VALUES(?1,?2,'proposal_applied',?3,?4)",params![run,sequence,Utc::now().to_rfc3339(),saved.proposal_id.to_string()])?;
                    tx.execute("UPDATE ai_run SET next_sequence=next_sequence+1 WHERE id=?1",[&run])?;
                }
            }
            tx.execute("UPDATE ai_chat SET updated_at=?2,revision=revision+1 WHERE id=?1",params![chat.to_string(),Utc::now().to_rfc3339()])?;
            tx.commit()?;Ok(true)
        }).await;
        if !matches!(result, Ok(true)) {
            self.queue_ai_database_cleanup(tenant, vec![content_handle])
                .await?;
        }
        if result? {
            return Ok(receipt);
        }
        match self
            .claim_ai_database_apply(receipt.proposal_id, actor, request)
            .await?
        {
            AiDatabaseApplyClaim::Replay(receipt) => Ok(*receipt),
            _ => Err(MetadataError::AiContent(
                "AI receipt was not durably recorded".into(),
            )),
        }
    }
    pub async fn interrupt_ai_database_applies(&self) -> Result<usize> {
        let store = self.clone();
        sqlite_blocking(move|| {
            let mut conn=store.conn()?;let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let changed=tx.execute("UPDATE ai_proposal_apply SET state='outcome_unknown',finished_at=?1 WHERE state='applying'",[Utc::now().to_rfc3339()])?;
            if changed>0 {super::insert_operation_audit_row(&tx,&super::NewOperationAudit {actor_principal_id:None,action:"ai.interrupt_database_applies".into(),target:"ai".into(),target_id:None,status:"succeeded".into(),result_code:None,row_count:i64::try_from(changed).ok(),error_message:None,correlation_id:None})?;}
            tx.commit()?;Ok(changed)
        }).await
    }
    pub async fn discard_ai_database_proposal(
        &self,
        id: Uuid,
        actor: PrincipalId,
    ) -> Result<AiDatabaseProposalDetail> {
        let proposal = self.ai_database_proposal(id, actor).await?;
        let chat = proposal.proposal.chat_id;
        let store = self.clone();
        sqlite_blocking(move|| {
            let mut conn=store.conn()?;let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            reviewer(&tx,chat,actor)?;
            let changed=tx.execute("UPDATE ai_proposal SET status='discarded',updated_at=?2 WHERE id=?1 AND status='staged' AND NOT EXISTS(SELECT 1 FROM ai_proposal_apply WHERE proposal_id=?1)",params![id.to_string(),Utc::now().to_rfc3339()])?;
            if changed!=1 {return Err(MetadataError::AiInvalid("AI database proposal is unavailable or was already claimed".into()));}
            let run:String=tx.query_row("SELECT run_id FROM ai_proposal WHERE id=?1",[id.to_string()],|row|row.get(0))?;
            let sequence:u64=tx.query_row("SELECT next_sequence FROM ai_run WHERE id=?1",[&run],|row|row.get(0))?;
            if sequence<=1024 {
                tx.execute("INSERT INTO ai_run_event(run_id,sequence,kind,at,proposal_id) VALUES(?1,?2,'proposal_discarded',?3,?4)",params![run,sequence,Utc::now().to_rfc3339(),id.to_string()])?;
                tx.execute("UPDATE ai_run SET next_sequence=next_sequence+1 WHERE id=?1",[&run])?;
            }
            tx.execute("UPDATE ai_chat SET updated_at=?2,revision=revision+1 WHERE id=?1",params![chat.to_string(),Utc::now().to_rfc3339()])?;
            tx.commit()?;Ok(())
        }).await?;
        self.ai_database_proposal(id, actor).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CredentialMode, MemorySecretStore, NewConnectionProfile, TenantId};
    use sift_protocol::{
        AiMode, AiProvider, AiTurnContext, AiVisibility, CellEdit, EditSet, ObjectPath, RowEdit,
        StartAiTurnRequest, Value,
    };
    use std::sync::Arc;
    #[tokio::test]
    async fn typed_reviews_claim_once_replay_receipts_and_interrupt_uncertain_work() {
        let store = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        store.bootstrap_local("owner").unwrap();
        let actor = PrincipalId(1);
        let tenant = TenantId(1);
        let profile = store
            .upsert_connection_profile(
                tenant,
                actor,
                NewConnectionProfile {
                    name: "draft database".into(),
                    provider_id: sift_protocol::Engine::Postgres.provider_id(),
                    configuration: serde_json::json!({"database":"test"}),
                    semantic_engine: Some(sift_protocol::Engine::Postgres),
                    credentials: None,
                    credential_mode: CredentialMode::Shared,
                    tags: vec![],
                },
            )
            .await
            .unwrap();
        let source = store
            .ai_profile_source_digest(profile.id, actor)
            .await
            .unwrap();
        let chat = store
            .create_ai_chat(tenant, None, actor, AiVisibility::Private, "rows".into())
            .await
            .unwrap();
        let context:AiTurnContext=serde_json::from_value(serde_json::json!({"target":{"tenant_id":1,"profile_id":profile.id.0},"staged_change_count":0})).unwrap();
        let lease = store
            .start_ai_run(
                chat.id,
                actor,
                StartAiTurnRequest {
                    attachment_previews: Vec::new(),
                    client_request_id: Uuid::new_v4(),
                    desktop_id: Uuid::new_v4(),
                    prompt: "Propose a row".into(),
                    provider: AiProvider::Codex,
                    model: None,
                    mode: AiMode::Propose,
                    context,
                },
            )
            .await
            .unwrap();
        let draft = AiDatabaseDraft::RowEditSet {
            edit_set: EditSet {
                table: ObjectPath::new("items"),
                edits: vec![RowEdit::Insert {
                    values: vec![CellEdit {
                        column: "name".into(),
                        value: Value::Text("reviewed value".into()),
                    }],
                }],
            },
            expected_catalog_revision: sift_protocol::CatalogRevision(1),
        };
        let request = sift_protocol::StageAiDatabaseProposalRequest {
            client_request_id: Uuid::new_v4(),
            lease_token: lease.lease_token,
            draft: draft.clone(),
        };
        assert!(store
            .stage_ai_database_proposal(
                lease.run.id,
                actor,
                request.clone(),
                "0".repeat(64),
                "dbid:test".into(),
                None
            )
            .await
            .is_err());
        let proposal = store
            .stage_ai_database_proposal(
                lease.run.id,
                actor,
                request.clone(),
                source.clone(),
                "dbid:test".into(),
                None,
            )
            .await
            .unwrap();
        let retry = store
            .stage_ai_database_proposal(
                lease.run.id,
                actor,
                request,
                source.clone(),
                "dbid:test".into(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(retry.proposal.id, proposal.proposal.id);
        assert!(store
            .list_ai_query_proposals(chat.id, actor)
            .await
            .unwrap()
            .is_empty());
        assert!(store
            .mark_ai_query_proposal_applied(chat.id, proposal.proposal.id, actor, 1)
            .await
            .is_err());
        async fn review(
            store: &MetadataStore,
            actor: PrincipalId,
            proposal: &AiDatabaseProposalDetail,
        ) -> sift_protocol::AiDatabaseProposalReview {
            store
                .save_ai_database_review(
                    actor,
                    Uuid::new_v4(),
                    sift_protocol::AiDatabaseProposalReview {
                        id: Uuid::new_v4(),
                        proposal_id: proposal.proposal.id,
                        reviewer_id: actor.0,
                        session: sift_protocol::SessionId(1),
                        connection: sift_protocol::ConnectionId(1),
                        source_digest: proposal.source_digest.clone(),
                        content_sha256: proposal.proposal.content_sha256.clone(),
                        review_digest: String::new(),
                        database_label: "test".into(),
                        production: false,
                        preview: sift_protocol::AiDatabasePreview::RowEditSet {
                            plan: sift_protocol::EditPlan {
                                table: ObjectPath::new("items"),
                                identity: sift_protocol::IdentitySource::PrimaryKey {
                                    columns: vec!["id".into()],
                                },
                                statements: vec![],
                            },
                        },
                        expires_at: Utc::now() + chrono::Duration::minutes(5),
                    },
                )
                .await
                .unwrap()
        }
        let preview = review(&store, actor, &proposal).await;
        let apply = sift_protocol::ApplyAiDatabaseProposalRequest {
            client_request_id: Uuid::new_v4(),
            review_id: preview.id,
            review_digest: preview.review_digest,
            production_confirmation: None,
            acknowledgements: vec![],
        };
        let (first, second) = tokio::join!(
            store.claim_ai_database_apply(proposal.proposal.id, actor, apply.clone()),
            store.claim_ai_database_apply(proposal.proposal.id, actor, apply.clone())
        );
        let claims = [first.unwrap(), second.unwrap()];
        assert_eq!(
            claims
                .iter()
                .filter(|claim| matches!(claim, AiDatabaseApplyClaim::Claimed))
                .count(),
            1
        );
        assert_eq!(
            claims
                .iter()
                .filter(|claim| matches!(
                    claim,
                    AiDatabaseApplyClaim::Pending(AiDatabaseApplyState::Applying)
                ))
                .count(),
            1
        );
        let mut altered = apply.clone();
        altered.client_request_id = Uuid::new_v4();
        assert!(store
            .claim_ai_database_apply(proposal.proposal.id, actor, altered)
            .await
            .is_err());
        assert!(store
            .discard_ai_database_proposal(proposal.proposal.id, actor)
            .await
            .is_err());
        let receipt = sift_protocol::AiDatabaseApplyReceipt {
            proposal_id: proposal.proposal.id,
            state: AiDatabaseApplyState::Applied,
            row_result: Some(sift_protocol::ApplyEditsResult {
                applied: vec![],
                committed: true,
            }),
            migration_result: None,
            message: "Applied reviewed rows".into(),
        };
        // Revocation after database dispatch cannot erase the durable outcome,
        // but it must still prevent the former reviewer from reading it.
        store
            .conn()
            .unwrap()
            .execute(
                "DELETE FROM membership WHERE tenant_id=?1 AND principal_id=?2",
                params![tenant.0, actor.0],
            )
            .unwrap();
        store
            .finish_ai_database_apply(actor, apply.clone(), receipt)
            .await
            .unwrap();
        assert!(store
            .ai_database_proposal(proposal.proposal.id, actor)
            .await
            .is_err());
        store
            .upsert_tenant_membership(tenant, actor, crate::MembershipRole::Owner)
            .unwrap();
        assert!(
            matches!(store.claim_ai_database_apply(proposal.proposal.id,actor,apply).await.unwrap(),AiDatabaseApplyClaim::Replay(receipt) if receipt.state==AiDatabaseApplyState::Applied)
        );
        assert_eq!(
            store
                .ai_database_proposal(proposal.proposal.id, actor)
                .await
                .unwrap()
                .proposal
                .status,
            AiProposalStatus::Applied
        );
        let second = store
            .stage_ai_database_proposal(
                lease.run.id,
                actor,
                sift_protocol::StageAiDatabaseProposalRequest {
                    client_request_id: Uuid::new_v4(),
                    lease_token: lease.lease_token,
                    draft,
                },
                source,
                "dbid:test".into(),
                None,
            )
            .await
            .unwrap();
        let preview = review(&store, actor, &second).await;
        let request = sift_protocol::ApplyAiDatabaseProposalRequest {
            client_request_id: Uuid::new_v4(),
            review_id: preview.id,
            review_digest: preview.review_digest,
            production_confirmation: None,
            acknowledgements: vec![],
        };
        assert!(matches!(
            store
                .claim_ai_database_apply(second.proposal.id, actor, request.clone())
                .await
                .unwrap(),
            AiDatabaseApplyClaim::Claimed
        ));
        assert_eq!(store.interrupt_ai_database_applies().await.unwrap(), 1);
        assert!(matches!(
            store
                .claim_ai_database_apply(second.proposal.id, actor, request)
                .await
                .unwrap(),
            AiDatabaseApplyClaim::Pending(AiDatabaseApplyState::OutcomeUnknown)
        ));
        let inventory = store.ai_content_handles_offline(Some(tenant)).unwrap();
        let review_bodies = {
            let conn = store.conn().unwrap();
            let mut statement = conn.prepare("SELECT content_handle FROM ai_proposal_review UNION SELECT receipt_handle FROM ai_proposal_apply WHERE receipt_handle IS NOT NULL").unwrap();
            statement
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        assert_eq!(review_bodies.len(), 3);
        assert!(review_bodies
            .iter()
            .all(|handle| inventory.iter().any(|(_, body)| body == handle)));
        store.delete_ai_chat(chat.id, actor).await.unwrap();
        assert!(store
            .ai_content_handles_offline(Some(tenant))
            .unwrap()
            .is_empty());
        assert_eq!(store.process_ai_content_cleanup(100).await.unwrap().1, 0);
        for handle in review_bodies {
            assert!(store
                .ai_content
                .get(tenant.0, &handle)
                .await
                .unwrap()
                .is_none());
        }
    }
}
