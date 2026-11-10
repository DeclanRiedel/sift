//! One-use reviewed text edits; action rows contain IDs/hashes, never SQL or tokens.
use super::{
    sqlite_blocking, ConnectionProfileId, DocumentId, MetadataError, MetadataStore,
    NewDocumentUpdate, PrincipalId, Result,
};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};
use sift_protocol::*;
use uuid::Uuid;

fn invalid(message: &str) -> MetadataError {
    MetadataError::AiInvalid(message.into())
}
fn uuid(value: &str) -> Result<Uuid> {
    Uuid::parse_str(value).map_err(|_| invalid("Invalid query execution identity"))
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn tag<T: serde::Serialize>(value: T) -> Result<String> {
    serde_json::to_value(value)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| invalid("Invalid query edit outcome"))
}
fn parse<T: serde::de::DeserializeOwned>(value: String) -> Result<T> {
    Ok(serde_json::from_value(serde_json::Value::String(value))?)
}
fn revision(text: &str) -> u64 {
    let digest = Sha256::digest(text.as_bytes());
    u64::from_le_bytes(digest[..8].try_into().expect("SHA-256 prefix")) & i64::MAX as u64
}
fn owner(conn: &Connection, id: Uuid, actor: PrincipalId) -> Result<String> {
    let row: Option<(String, i64)> = conn
        .query_row(
            "SELECT chat_id,initiator_id FROM ai_query_execution WHERE id=?1",
            [id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (chat, initiator) = row.ok_or(MetadataError::AiNotFound)?;
    if initiator != actor.0 {
        return Err(MetadataError::AiAccessDenied);
    }
    Ok(chat)
}
fn access(conn: &Connection, id: Uuid, actor: PrincipalId) -> Result<()> {
    let chat = owner(conn, id, actor)?;
    super::ai::require_chat_access(conn, uuid(&chat)?, actor).map(|_| ())
}
fn target(run: &AiRunDetail) -> Result<AiQueryExecutionTarget> {
    let context = &run.context;
    match (&context.target.document_id, context.target.room_id) {
        (Some(document), Some(room)) => {
            let document: i64 = document
                .parse()
                .map_err(|_| invalid("Invalid query document target"))?;
            if document <= 0
                || room <= 0
                || context.sql.as_ref().and_then(|s| s.room_document_id) != Some(document)
            {
                return Err(invalid(
                    "Query document target differs from its original text",
                ));
            }
            Ok(AiQueryExecutionTarget::RoomDocument {
                room_id: room,
                document_id: document,
            })
        }
        (None, None) => {
            let item = context
                .editor_item_id
                .filter(|id| *id > 0 && *id <= i64::MAX as u64)
                .ok_or_else(|| invalid("The original private editor is required"))?;
            if run.run.desktop_id.is_nil()
                || context
                    .sql
                    .as_ref()
                    .map_or(true, |s| s.room_document_id.is_some())
            {
                return Err(invalid("The original private desktop is required"));
            }
            Ok(AiQueryExecutionTarget::PrivateEditor {
                desktop_id: run.run.desktop_id,
                item_id: item,
            })
        }
        _ => Err(invalid("Query target is incomplete")),
    }
}
fn fingerprint(
    run: &AiRunDetail,
    proposal: &AiQueryProposalDetail,
    source: &AiExecutionSource,
    expires: DateTime<Utc>,
) -> Result<String> {
    let original = run
        .context
        .sql
        .as_ref()
        .ok_or_else(|| invalid("Complete original SQL is required"))?;
    Ok(hash(&serde_json::to_vec(&(
        run.run.id,
        run.run.initiator_principal_id,
        proposal.proposal.id,
        &proposal.proposal.content_sha256,
        &proposal.proposal.target,
        proposal.proposal.base_revision,
        target(run)?,
        hash(original.text.as_bytes()),
        source,
        expires,
    ))?))
}
impl MetadataStore {
    fn query_execution_live_locked(
        &self,
        conn: &Connection,
        id: Uuid,
        actor: PrincipalId,
        fingerprint: &str,
        phase: &str,
    ) -> Result<()> {
        access(conn, id, actor)?;
        let row:Option<(Option<i64>,String,Option<i64>)>=conn.query_row("SELECT q.source_profile_id,q.source_digest,q.document_id FROM ai_query_execution q JOIN ai_run r ON r.id=q.run_id JOIN ai_chat c ON c.id=q.chat_id JOIN ai_proposal p ON p.id=q.proposal_id WHERE q.id=?1 AND q.fingerprint=?2 AND q.state=?3 AND julianday(q.expires_at)>julianday(?4) AND r.status='running' AND r.execute_mode=1 AND c.visibility='private' AND c.owner_principal_id=?5 AND p.status='staged'",params![id.to_string(),fingerprint,phase,Utc::now().to_rfc3339(),actor.0],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        let (profile, digest, document) =
            row.ok_or_else(|| invalid("Query execution run, review or claim is no longer active"))?;
        let profile = profile.ok_or_else(|| invalid("Query source was removed"))?;
        if self.ai_profile_source_digest_locked(conn, ConnectionProfileId(profile), actor)?
            != digest
        {
            return Err(invalid("Query source or policy changed"));
        }
        let room_target: bool = conn.query_row(
            "SELECT target_kind='room_document' FROM ai_query_execution WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )?;
        if room_target {
            let writable:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM ai_query_execution q JOIN document d ON d.id=q.document_id AND d.room_id=q.room_id JOIN room r ON r.id=d.room_id AND r.tenant_id=q.tenant_id JOIN room_member m ON m.room_id=d.room_id WHERE q.id=?1 AND d.id=?3 AND m.principal_id=?2 AND m.role IN ('owner','editor'))",params![id.to_string(),actor.0,document],|r|r.get(0))?;
            if !writable {
                return Err(MetadataError::AiAccessDenied);
            }
        }
        Ok(())
    }
    pub async fn prior_ai_query_execution(
        &self,
        run: Uuid,
        actor: PrincipalId,
        request: &RequestAiQueryExecution,
    ) -> Result<Option<AiQueryExecutionSummary>> {
        let store = self.clone();
        let request = request.clone();
        let id = sqlite_blocking(move || {
            let conn = store.conn()?;
            super::ai_run::ai_run_authorized_conn(&conn, run, actor, request.lease_token)?;
            let row: Option<(String, String)> = conn
                .query_row(
                    "SELECT id,proposal_id FROM ai_query_execution WHERE run_id=?1 AND call_id=?2",
                    params![run.to_string(), request.call_id.to_string()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            row.map(|(id, proposal)| {
                if proposal == request.proposal_id.to_string() {
                    uuid(&id)
                } else {
                    Err(invalid("Query call identity changed its proposal"))
                }
            })
            .transpose()
        })
        .await?;
        match id {
            Some(id) => self.ai_query_execution(id, actor).await.map(Some),
            None => Ok(None),
        }
    }
    pub async fn create_ai_query_execution(
        &self,
        run_id: Uuid,
        actor: PrincipalId,
        request: RequestAiQueryExecution,
        expires: DateTime<Utc>,
        max_calls: u32,
    ) -> Result<AiQueryExecutionSummary> {
        if request.call_id.is_nil() || expires <= Utc::now() {
            return Err(invalid("Query request identity or expiry is invalid"));
        }
        if let Some(prior) = self
            .prior_ai_query_execution(run_id, actor, &request)
            .await?
        {
            return Ok(prior);
        }
        let run = self.ai_tool_run(run_id, actor, request.lease_token).await?;
        if run.mode != AiMode::Execute || run.visibility != AiVisibility::Private {
            return Err(MetadataError::AiAccessDenied);
        }
        let run = self.ai_run_detail(run.chat_id, run_id, actor).await?;
        let proposal = self
            .list_ai_query_proposals(run.run.chat_id, actor)
            .await?
            .into_iter()
            .find(|p| p.proposal.id == request.proposal_id)
            .ok_or(MetadataError::AiNotFound)?;
        let original = run
            .context
            .sql
            .as_ref()
            .ok_or_else(|| invalid("Complete original SQL is required"))?;
        if proposal.proposal.run_id != run_id
            || proposal.proposal.status != AiProposalStatus::Staged
            || proposal.proposal.target != run.context.target
            || proposal.proposal.base_revision != Some(revision(&original.text))
            || original.document_revision != proposal.proposal.base_revision
            || original.text == proposal.proposed_sql
            || original.text.len() > 64 * 1024
        {
            return Err(invalid(
                "Query proposal does not match its complete original text and revision",
            ));
        }
        let target = target(&run)?;
        let source = self.ai_execute_run_source(run_id, actor).await?;
        if run.context.target.tenant_id != Some(source.tenant_id)
            || run.context.target.profile_id != Some(source.profile_id)
        {
            return Err(invalid(
                "Query target differs from its original Execute source",
            ));
        }
        let fp = fingerprint(&run, &proposal, &source, expires)?;
        let before = hash(original.text.as_bytes());
        let after = hash(proposal.proposed_sql.as_bytes());
        let document = match target {
            AiQueryExecutionTarget::RoomDocument { document_id, .. } => Some(document_id),
            _ => None,
        };
        let room = match target {
            AiQueryExecutionTarget::RoomDocument { room_id, .. } => Some(room_id),
            _ => None,
        };
        let kind = if document.is_some() {
            "room_document"
        } else {
            "private_editor"
        };
        let id = Uuid::new_v4();
        let store = self.clone();
        let id=sqlite_blocking(move|| {
            let mut conn=store.conn()?;let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (tenant,status)=super::ai_run::ai_run_authorized_conn(&tx,run_id,actor,request.lease_token)?;
            let prior:Option<(String,String)>=tx.query_row("SELECT id,proposal_id FROM ai_query_execution WHERE run_id=?1 AND call_id=?2",params![run_id.to_string(),request.call_id.to_string()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if let Some((id,proposal))=prior {if proposal!=request.proposal_id.to_string(){return Err(invalid("Query call identity changed"));}return uuid(&id);}
            if status!="running" || tenant!=source.tenant_id {return Err(MetadataError::AiAccessDenied);}
            super::ai_run::ensure_ai_tool_budget_conn(&tx,run_id,max_calls)?;
            let used:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM ai_run_event WHERE run_id=?1 AND tool_call_id=?2)",params![run_id.to_string(),request.call_id.to_string()],|r|r.get(0))?;
            if used {return Err(invalid("AI call identity was already used"));}
            let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM ai_proposal WHERE id=?1 AND run_id=?2 AND status='staged' AND content_sha256=?3)",params![request.proposal_id.to_string(),run_id.to_string(),after],|r|r.get(0))?;
            if !valid {return Err(invalid("Query proposal changed"));}
            let now=Utc::now().to_rfc3339();
            tx.execute("INSERT INTO ai_query_execution(id,run_id,chat_id,tenant_id,initiator_id,call_id,proposal_id,source_profile_id,source_digest,fingerprint,before_sha256,after_sha256,target_kind,document_id,desktop_id,state,created_at,expires_at,room_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,'awaiting_approval',?16,?17,?18)",params![id.to_string(),run_id.to_string(),run.run.chat_id.to_string(),tenant,actor.0,request.call_id.to_string(),request.proposal_id.to_string(),source.profile_id,source.profile_digest,fp,before,after,kind,document,run.run.desktop_id.to_string(),now,expires.to_rfc3339(),room])?;
            store.query_execution_live_locked(&tx,id,actor,&fp,"awaiting_approval")?;
            query_event(&tx,run_id,"tool_requested",Some(request.call_id),None)?;
            let floor:u32=tx.pragma_query_value(None,"user_version",|r|r.get(0))?;if floor<62 {tx.pragma_update(None,"user_version",62)?;}
            tx.commit()?;Ok(id)
        }).await?;
        self.ai_query_execution(id, actor).await
    }
    pub async fn ai_query_execution(
        &self,
        id: Uuid,
        actor: PrincipalId,
    ) -> Result<AiQueryExecutionSummary> {
        let store = self.clone();
        sqlite_blocking(move|| {
            let mut conn=store.conn()?;let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate)?;access(&tx,id,actor)?;
            tx.execute("UPDATE ai_query_execution SET state='outcome_unknown',outcome='outcome_unknown',evidence='interrupted',observed_at=?2 WHERE id=?1 AND target_kind='private_editor' AND state='applying' AND julianday(expires_at)<=julianday(?2)",params![id.to_string(),Utc::now().to_rfc3339()])?;
            let summary=read_summary(&tx,id,actor)?;tx.commit()?;Ok(summary)
        }).await
    }
    /// Settlement owners use redacted evidence after dispatch, even after revocation.
    /// Public delivery must separately require current chat access.
    pub async fn ai_query_execution_settlement(
        &self,
        id: Uuid,
        actor: PrincipalId,
    ) -> Result<AiQueryExecutionSummary> {
        let store = self.clone();
        sqlite_blocking(move || {
            let conn = store.conn()?;
            owner(&conn, id, actor)?;
            read_summary(&conn, id, actor)
        })
        .await
    }
    pub async fn ai_query_execution_detail(
        &self,
        id: Uuid,
        actor: PrincipalId,
    ) -> Result<AiQueryExecutionDetail> {
        let summary = self.ai_query_execution(id, actor).await?;
        let run = self
            .ai_run_detail(summary.chat_id, summary.run_id, actor)
            .await?;
        let proposal = self
            .list_ai_query_proposals(summary.chat_id, actor)
            .await?
            .into_iter()
            .find(|p| p.proposal.id == summary.proposal_id)
            .ok_or(MetadataError::AiNotFound)?;
        let source = self.ai_execute_run_source(summary.run_id, actor).await?;
        if fingerprint(&run, &proposal, &source, summary.expires_at)? != summary.fingerprint {
            return Err(invalid("Query execution fingerprint changed"));
        }
        let target = target(&run)?;
        let store = self.clone();
        sqlite_blocking(move || {
            let conn = store.conn()?;
            access(&conn, id, actor)
        })
        .await?;
        Ok(AiQueryExecutionDetail {
            summary,
            target,
            original_sql: run
                .context
                .sql
                .ok_or_else(|| invalid("Original SQL unavailable"))?
                .text,
            proposal,
        })
    }
    pub async fn ai_query_executions(
        &self,
        chat: Uuid,
        actor: PrincipalId,
    ) -> Result<Vec<AiQueryExecutionSummary>> {
        let store = self.clone();
        let ids=sqlite_blocking(move|| {
            let conn=store.conn()?;super::ai::require_chat_access(&conn,chat,actor)?;
            let mut stmt=conn.prepare("SELECT id FROM ai_query_execution WHERE chat_id=?1 AND initiator_id=?2 ORDER BY created_at DESC,id DESC LIMIT 200")?;
            let ids=stmt.query_map(params![chat.to_string(),actor.0],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            ids.iter().map(|s|uuid(s)).collect::<Result<Vec<_>>>()
        }).await?;
        let mut result = Vec::new();
        for id in ids {
            result.push(self.ai_query_execution(id, actor).await?);
        }
        Ok(result)
    }
    /// Returns a private-editor capability once. Duplicate approval never reissues it.
    pub async fn claim_ai_query_execution(
        &self,
        id: Uuid,
        actor: PrincipalId,
        request: ApproveAiQueryExecution,
    ) -> Result<(bool, Option<AiQueryEditorDispatch>)> {
        let store = self.clone();
        sqlite_blocking(move || {
            let mut conn = store.conn()?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let claimed = store.claim_ai_query_execution_locked(&tx, id, actor, &request)?;
            tx.commit()?;
            Ok(claimed)
        })
        .await
    }
    /// Borrowed native claim; a duplicate never reissues an editor capability.
    pub(super) fn claim_ai_query_execution_locked(
        &self,
        tx: &rusqlite::Transaction<'_>,
        id: Uuid,
        actor: PrincipalId,
        request: &ApproveAiQueryExecution,
    ) -> Result<(bool, Option<AiQueryEditorDispatch>)> {
        access(tx, id, actor)?;
        let (fp,phase,kind,desktop,chat):(String,String,String,String,String)=tx.query_row("SELECT fingerprint,state,target_kind,desktop_id,chat_id FROM ai_query_execution WHERE id=?1",[id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
        if fp != request.fingerprint {
            return Err(invalid("Query approval changed"));
        }
        if phase != "awaiting_approval" {
            return Ok((false, None));
        }
        if kind == "private_editor" && desktop != request.desktop_id.to_string() {
            return Err(invalid("Approval requires the original desktop companion"));
        }
        self.query_execution_live_locked(tx, id, actor, &fp, "awaiting_approval")?;
        super::ai_mutation_admission::require_clear(tx, &chat)?;
        let dispatch = (kind == "private_editor").then(|| AiQueryEditorDispatch {
            token: Uuid::new_v4(),
            desktop_id: request.desktop_id,
        });
        let digest = dispatch.as_ref().map(|d| hash(d.token.as_bytes()));
        tx.execute(
            "UPDATE ai_query_execution SET state='applying',dispatch_digest=?2 WHERE id=?1",
            params![id.to_string(), digest],
        )?;
        Ok((true, dispatch))
    }
    /// Called by the document actor after validating the actual before/after text.
    /// The CRDT append and receipt/proposal settlement share this transaction.
    pub fn append_ai_query_execution_document_update(
        &self,
        id: Uuid,
        fingerprint: &str,
        document: DocumentId,
        update: NewDocumentUpdate,
        before_sha256: &str,
        after_sha256: &str,
    ) -> Result<i64> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let actor = update.submitted_by;
        self.query_execution_live_locked(&tx, id, actor, fingerprint, "applying")?;
        let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM ai_query_execution WHERE id=?1 AND target_kind='room_document' AND document_id=?2 AND before_sha256=?3 AND after_sha256=?4)",params![id.to_string(),document.0,before_sha256,after_sha256],|r|r.get(0))?;
        if !valid
            || update.update_id != id.to_string()
            || update.replica_id != "sift-ai-query-execution"
        {
            return Err(invalid(
                "Authored query edit differs from its original claim",
            ));
        }
        let seq = super::append_document_update_locked(&tx, document, update)?;
        settle(
            &tx,
            id,
            AiQueryEditOutcome::Applied,
            AiQueryEditEvidence::RoomCommit,
            Some(seq),
        )?;
        tx.commit()?;
        Ok(seq)
    }
    /// Only a room owner that has not committed its atomic append can report no dispatch.
    pub async fn refuse_ai_query_document_execution(
        &self,
        id: Uuid,
        actor: PrincipalId,
    ) -> Result<()> {
        let store = self.clone();
        sqlite_blocking(move|| {
            let mut conn=store.conn()?;let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate)?;owner(&tx,id,actor)?;
            let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM ai_query_execution WHERE id=?1 AND target_kind='room_document' AND state='applying')",[id.to_string()],|r|r.get(0))?;
            if valid {settle(&tx,id,AiQueryEditOutcome::Refused,AiQueryEditEvidence::NoDispatch,None)?;}
            tx.commit()?;Ok(())
        }).await
    }
    pub async fn acknowledge_ai_query_editor_execution(
        &self,
        id: Uuid,
        actor: PrincipalId,
        request: AcknowledgeAiQueryEditorExecution,
    ) -> Result<()> {
        let store = self.clone();
        sqlite_blocking(move|| {
            let mut conn=store.conn()?;let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            // Settlement retains the original actor even after Stop or membership loss.
            owner(&tx,id,actor)?;
            tx.execute("UPDATE ai_query_execution SET state='outcome_unknown',outcome='outcome_unknown',evidence='interrupted',observed_at=?2 WHERE id=?1 AND target_kind='private_editor' AND state='applying' AND julianday(expires_at)<=julianday(?2)",params![id.to_string(),Utc::now().to_rfc3339()])?;
            let (fp,desktop,token,old_ack,before,after,phase):(String,String,Option<String>,Option<String>,String,String,String)=tx.query_row("SELECT fingerprint,desktop_id,dispatch_digest,ack_digest,before_sha256,after_sha256,state FROM ai_query_execution WHERE id=?1 AND target_kind='private_editor'",[id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).optional()?.ok_or(MetadataError::AiNotFound)?;
            if fp!=request.fingerprint || desktop!=request.desktop_id.to_string() || token.as_deref()!=Some(hash(request.dispatch_token.as_bytes()).as_str()) {return Err(MetadataError::AiAccessDenied);}
            let digest=hash(&serde_json::to_vec(&request)?);
            if let Some(prior)=old_ack {if prior==digest {return Ok(());}return Err(invalid("Query acknowledgement changed"));}
            if !matches!(phase.as_str(),"applying"|"outcome_unknown"|"reconciled") {return Err(invalid("Query dispatch is not awaiting acknowledgement"));}
            if request.outcome==AiQueryEditOutcome::Applied && (request.before_sha256.as_ref()!=Some(&before) || request.after_sha256.as_ref()!=Some(&after)) {return Err(invalid("Companion text does not match the reviewed before/after hashes"));}
            if request.outcome!=AiQueryEditOutcome::Applied && (request.before_sha256.is_some() || request.after_sha256.is_some()) {return Err(invalid("A refused or unknown edit cannot attest text hashes"));}
            tx.execute("UPDATE ai_query_execution SET ack_digest=?2 WHERE id=?1",params![id.to_string(),digest])?;
            if phase=="applying" {settle(&tx,id,request.outcome,AiQueryEditEvidence::CompanionAcknowledgement,None)?;}
            else {tx.execute("UPDATE ai_query_execution SET late_companion_outcome=?2,late_observed_at=?3 WHERE id=?1",params![id.to_string(),tag(request.outcome)?,Utc::now().to_rfc3339()])?;}
            tx.commit()?;Ok(())
        }).await
    }
    pub async fn deny_ai_query_execution(
        &self,
        id: Uuid,
        actor: PrincipalId,
        fp: String,
    ) -> Result<()> {
        let store = self.clone();
        sqlite_blocking(move|| {
            let mut conn=store.conn()?;let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate)?;access(&tx,id,actor)?;
            let changed=tx.execute("UPDATE ai_query_execution SET state='denied' WHERE id=?1 AND fingerprint=?2 AND state IN ('awaiting_approval','denied')",params![id.to_string(),fp])?;
            if changed!=1 {return Err(invalid("Only the exact pending query request can be denied"));}tx.commit()?;Ok(())
        }).await
    }
    pub async fn reconcile_ai_query_execution(
        &self,
        id: Uuid,
        actor: PrincipalId,
        fp: String,
    ) -> Result<()> {
        let store = self.clone();
        sqlite_blocking(move|| {
            let mut conn=store.conn()?;let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate)?;access(&tx,id,actor)?;
            let changed=tx.execute("UPDATE ai_query_execution SET state='reconciled' WHERE id=?1 AND fingerprint=?2 AND state IN ('outcome_unknown','reconciled')",params![id.to_string(),fp])?;
            if changed!=1 {return Err(invalid("Only the exact unknown query outcome can be acknowledged"));}tx.commit()?;Ok(())
        }).await
    }
}
fn query_event(
    conn: &Connection,
    run: Uuid,
    kind: &str,
    call: Option<Uuid>,
    proposal: Option<Uuid>,
) -> Result<()> {
    let sequence: u64 = conn.query_row(
        "SELECT next_sequence FROM ai_run WHERE id=?1",
        [run.to_string()],
        |r| r.get(0),
    )?;
    if sequence > 1024 {
        return Err(invalid("AI event limit reached"));
    }
    conn.execute("INSERT INTO ai_run_event(run_id,sequence,kind,at,tool_call_id,proposal_id) VALUES(?1,?2,?3,?4,?5,?6)",params![run.to_string(),sequence,kind,Utc::now().to_rfc3339(),call.map(|s|s.to_string()),proposal.map(|s|s.to_string())])?;
    conn.execute(
        "UPDATE ai_run SET next_sequence=next_sequence+1 WHERE id=?1",
        [run.to_string()],
    )?;
    Ok(())
}
fn settle(
    conn: &Connection,
    id: Uuid,
    outcome: AiQueryEditOutcome,
    evidence: AiQueryEditEvidence,
    sequence: Option<i64>,
) -> Result<()> {
    let phase = if outcome == AiQueryEditOutcome::OutcomeUnknown {
        "outcome_unknown"
    } else {
        "completed"
    };
    conn.execute("UPDATE ai_query_execution SET state=?2,outcome=?3,evidence=?4,document_sequence=?5,observed_at=?6 WHERE id=?1 AND state='applying'",params![id.to_string(),phase,tag(outcome)?,tag(evidence)?,sequence,Utc::now().to_rfc3339()])?;
    if outcome == AiQueryEditOutcome::Applied {
        let (proposal, run, actor): (String, String, i64) = conn.query_row(
            "SELECT proposal_id,run_id,initiator_id FROM ai_query_execution WHERE id=?1",
            [id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        conn.execute(
            "UPDATE ai_proposal SET status='applied',applied_by=?2,updated_at=?3 WHERE id=?1",
            params![proposal, actor, Utc::now().to_rfc3339()],
        )?;
        let sequence: u64 = conn.query_row(
            "SELECT next_sequence FROM ai_run WHERE id=?1",
            [&run],
            |r| r.get(0),
        )?;
        if sequence <= 1024 {
            query_event(
                conn,
                uuid(&run)?,
                "proposal_applied",
                None,
                Some(uuid(&proposal)?),
            )?;
        }
    }
    Ok(())
}
fn read_summary(
    conn: &Connection,
    id: Uuid,
    actor: PrincipalId,
) -> Result<AiQueryExecutionSummary> {
    let mut summary=conn.query_row("SELECT run_id,chat_id,proposal_id,call_id,fingerprint,state,created_at,expires_at FROM ai_query_execution WHERE id=?1",[id.to_string()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?,r.get::<_,String>(7)?))).map_err(MetadataError::from).and_then(|r|Ok(AiQueryExecutionSummary{id,run_id:uuid(&r.0)?,chat_id:uuid(&r.1)?,proposal_id:uuid(&r.2)?,call_id:uuid(&r.3)?,initiator_id:actor.0,fingerprint:r.4,state:parse(r.5)?,created_at:super::parse_time_sql(r.6)?,expires_at:super::parse_time_sql(r.7)?,receipt:None}))?;
    if summary.state == AiExecutionState::AwaitingApproval && summary.expires_at <= Utc::now() {
        summary.state = AiExecutionState::Denied;
    }
    let row=conn.query_row("SELECT outcome,evidence,before_sha256,after_sha256,document_sequence,observed_at,late_companion_outcome,late_observed_at FROM ai_query_execution WHERE id=?1",[id.to_string()],|r|Ok((r.get::<_,Option<String>>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,Option<i64>>(4)?,r.get::<_,Option<String>>(5)?,r.get::<_,Option<String>>(6)?,r.get::<_,Option<String>>(7)?)))?;
    if let (Some(outcome), Some(evidence), Some(at)) = (row.0, row.1, row.5) {
        let outcome = parse(outcome)?;
        summary.receipt = Some(AiQueryExecutionReceipt {
            outcome,
            evidence: parse(evidence)?,
            before_sha256: row.2,
            after_sha256: (outcome == AiQueryEditOutcome::Applied).then_some(row.3),
            document_sequence: row.4,
            observed_at: super::parse_time_sql(at)?,
            late_companion_outcome: row.6.map(parse).transpose()?,
            late_observed_at: row.7.map(super::parse_time_sql).transpose()?,
        });
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    async fn fixture_for_target(
        room_target: bool,
    ) -> (
        MetadataStore,
        AiRunLease,
        AiQueryExecutionSummary,
        RequestAiQueryExecution,
        AiExecutionSource,
    ) {
        let store = MetadataStore::open_in_memory(Arc::new(super::super::MemorySecretStore::new()))
            .unwrap();
        store.bootstrap_local("query owner").unwrap();
        let actor = PrincipalId(1);
        let profile = store
            .upsert_connection_profile(
                super::super::TenantId(1),
                actor,
                super::super::NewConnectionProfile {
                    name: "query".into(),
                    provider_id: Engine::Postgres.provider_id(),
                    semantic_engine: Some(Engine::Postgres),
                    configuration: serde_json::json!({"database":"query"}),
                    credentials: None,
                    credential_mode: super::super::CredentialMode::Shared,
                    tags: vec![],
                },
            )
            .await
            .unwrap();
        let chat = store
            .create_ai_chat(
                super::super::TenantId(1),
                None,
                actor,
                AiVisibility::Private,
                "query".into(),
            )
            .await
            .unwrap();
        let document = if room_target {
            let room = store
                .create_room(
                    super::super::TenantId(1),
                    actor,
                    super::super::NewRoom {
                        name: "query room".into(),
                        kind: super::super::RoomKind::Shared,
                    },
                )
                .unwrap();
            Some(
                store
                    .create_document(
                        room.id,
                        super::super::NewDocument {
                            kind: "sql".into(),
                            title: "query.sql".into(),
                            crdt_state: b"storage transaction fixture".to_vec(),
                            snapshot_version: vec![],
                            position: 0,
                            connection_profile_id: Some(profile.id),
                        },
                    )
                    .unwrap(),
            )
        } else {
            None
        };
        let context:AiTurnContext=serde_json::from_value(serde_json::json!({"target":{"tenant_id":1,"profile_id":profile.id.0,"connection_id":"1:1","room_id":document.as_ref().map(|d|d.room_id.0),"document_id":document.as_ref().map(|d|d.id.0.to_string())},"editor_item_id":if room_target {None}else{Some(7)},"sql":{"text":"SELECT 1","document_revision":revision("SELECT 1"),"room_document_id":document.as_ref().map(|d|d.id.0)},"staged_change_count":0})).unwrap();
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
        let source = AiExecutionSource {
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
        };
        store
            .bind_ai_execute_run_source(run.run.id, actor, source.clone())
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
                    base_revision: revision("SELECT 1"),
                    proposed_sql: "SELECT 2 /* PRIVATE-QUERY-EXECUTION */".into(),
                },
            )
            .await
            .unwrap();
        let request = RequestAiQueryExecution {
            call_id: Uuid::new_v4(),
            lease_token: run.lease_token,
            proposal_id: proposal.proposal.id,
        };
        let summary = store
            .create_ai_query_execution(
                run.run.id,
                actor,
                request.clone(),
                Utc::now() + chrono::Duration::minutes(4),
                100,
            )
            .await
            .unwrap();
        (store, run, summary, request, source)
    }
    #[tokio::test]
    async fn query_claims_are_one_use_and_unknown_companion_evidence_never_redispatches() {
        let (store, run, summary, request, source) = fixture_for_target(false).await;
        let actor = PrincipalId(1);
        assert_eq!(
            store
                .prior_ai_query_execution(run.run.id, actor, &request)
                .await
                .unwrap()
                .unwrap()
                .id,
            summary.id
        );
        assert!(store
            .prior_ai_query_execution(
                run.run.id,
                actor,
                &RequestAiQueryExecution {
                    proposal_id: Uuid::new_v4(),
                    ..request
                }
            )
            .await
            .is_err());
        assert!(
            store
                .mark_ai_query_proposal_applied(
                    summary.chat_id,
                    summary.proposal_id,
                    actor,
                    revision("SELECT 1")
                )
                .await
                .is_err(),
            "ordinary human receipt cannot bypass execution approval"
        );
        let approval = ApproveAiQueryExecution {
            fingerprint: summary.fingerprint.clone(),
            desktop_id: run.run.desktop_id,
        };
        assert!(store
            .claim_ai_query_execution(
                summary.id,
                actor,
                ApproveAiQueryExecution {
                    desktop_id: Uuid::new_v4(),
                    ..approval.clone()
                }
            )
            .await
            .is_err());
        let sql = store
            .create_ai_sql_execution(
                run.run.id,
                actor,
                RequestAiSqlExecution {
                    call_id: Uuid::new_v4(),
                    lease_token: run.lease_token,
                    sql: "SELECT 1".into(),
                    params: vec![],
                },
                AiSqlExecutionInput {
                    sql: "SELECT 1".into(),
                    params: vec![],
                    source,
                },
                Utc::now() + chrono::Duration::minutes(4),
                100,
            )
            .await
            .unwrap();
        let (a, b) = tokio::join!(
            store.claim_ai_query_execution(summary.id, actor, approval.clone()),
            store.claim_ai_query_execution(summary.id, actor, approval.clone())
        );
        let a = a.unwrap();
        let b = b.unwrap();
        assert_ne!(a.0, b.0);
        let dispatch = a.1.or(b.1).unwrap();
        assert!(
            store
                .claim_ai_sql_execution(sql.id, actor, sql.fingerprint.clone())
                .await
                .is_err(),
            "shared mutation gate includes the issued editor"
        );
        let blocked = store.conn().unwrap().execute(
            "UPDATE ai_sql_execution SET state='applying' WHERE id=?1",
            [sql.id.to_string()],
        );
        assert!(
            blocked.is_err(),
            "older/direct writers cannot bypass query admission"
        );
        let detail = store
            .ai_query_execution_detail(summary.id, actor)
            .await
            .unwrap();
        let ack = AcknowledgeAiQueryEditorExecution {
            fingerprint: summary.fingerprint.clone(),
            desktop_id: dispatch.desktop_id,
            dispatch_token: dispatch.token,
            outcome: AiQueryEditOutcome::Applied,
            before_sha256: Some(hash(detail.original_sql.as_bytes())),
            after_sha256: Some(hash(detail.proposal.proposed_sql.as_bytes())),
        };
        assert!(!format!("{dispatch:?} {ack:?}").contains(&dispatch.token.to_string()));
        assert!(store
            .acknowledge_ai_query_editor_execution(
                summary.id,
                actor,
                AcknowledgeAiQueryEditorExecution {
                    dispatch_token: Uuid::new_v4(),
                    ..ack.clone()
                }
            )
            .await
            .is_err());
        store.interrupt_ai_database_applies().await.unwrap();
        store
            .acknowledge_ai_query_editor_execution(summary.id, actor, ack.clone())
            .await
            .unwrap();
        let unknown = store.ai_query_execution(summary.id, actor).await.unwrap();
        assert_eq!(unknown.state, AiExecutionState::OutcomeUnknown);
        let receipt = unknown.receipt.unwrap();
        assert_eq!(receipt.outcome, AiQueryEditOutcome::OutcomeUnknown);
        assert_eq!(
            receipt.late_companion_outcome,
            Some(AiQueryEditOutcome::Applied)
        );
        store
            .acknowledge_ai_query_editor_execution(summary.id, actor, ack.clone())
            .await
            .unwrap();
        assert!(store
            .acknowledge_ai_query_editor_execution(
                summary.id,
                actor,
                AcknowledgeAiQueryEditorExecution {
                    outcome: AiQueryEditOutcome::Refused,
                    before_sha256: None,
                    after_sha256: None,
                    ..ack
                }
            )
            .await
            .is_err());
        assert!(
            !store
                .claim_ai_query_execution(summary.id, actor, approval.clone())
                .await
                .unwrap()
                .0
        );
        store
            .reconcile_ai_query_execution(summary.id, actor, summary.fingerprint.clone())
            .await
            .unwrap();
        assert_eq!(
            store
                .ai_query_execution(summary.id, actor)
                .await
                .unwrap()
                .state,
            AiExecutionState::Reconciled
        );
        store
            .claim_ai_sql_execution(sql.id, actor, sql.fingerprint.clone())
            .await
            .unwrap();
        let floor: u32 = store
            .conn()
            .unwrap()
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(floor, 62);
    }
    #[tokio::test]
    async fn query_companion_exact_ack_survives_stop_and_pending_work_cannot_be_approved() {
        let (store, run, summary, _, _) = fixture_for_target(false).await;
        let actor = PrincipalId(1);
        let approval = ApproveAiQueryExecution {
            fingerprint: summary.fingerprint.clone(),
            desktop_id: run.run.desktop_id,
        };
        let dispatch = store
            .claim_ai_query_execution(summary.id, actor, approval.clone())
            .await
            .unwrap()
            .1
            .unwrap();
        let detail = store
            .ai_query_execution_detail(summary.id, actor)
            .await
            .unwrap();
        store
            .finish_ai_run(run.run.id, actor, run.lease_token, AiRunStatus::Canceled)
            .await
            .unwrap();
        store
            .acknowledge_ai_query_editor_execution(
                summary.id,
                actor,
                AcknowledgeAiQueryEditorExecution {
                    fingerprint: summary.fingerprint.clone(),
                    desktop_id: dispatch.desktop_id,
                    dispatch_token: dispatch.token,
                    outcome: AiQueryEditOutcome::Applied,
                    before_sha256: Some(hash(detail.original_sql.as_bytes())),
                    after_sha256: Some(hash(detail.proposal.proposed_sql.as_bytes())),
                },
            )
            .await
            .unwrap();
        let completed = store.ai_query_execution(summary.id, actor).await.unwrap();
        assert_eq!(completed.state, AiExecutionState::Completed);
        assert_eq!(
            completed.receipt.unwrap().evidence,
            AiQueryEditEvidence::CompanionAcknowledgement
        );
        assert!(
            !store
                .claim_ai_query_execution(summary.id, actor, approval)
                .await
                .unwrap()
                .0
        );
        let (store, run, pending, _, _) = fixture_for_target(false).await;
        store
            .finish_ai_run(run.run.id, actor, run.lease_token, AiRunStatus::Canceled)
            .await
            .unwrap();
        assert_eq!(
            store
                .ai_query_execution(pending.id, actor)
                .await
                .unwrap()
                .state,
            AiExecutionState::Denied
        );
        assert!(
            !store
                .claim_ai_query_execution(
                    pending.id,
                    actor,
                    ApproveAiQueryExecution {
                        fingerprint: pending.fingerprint,
                        desktop_id: run.run.desktop_id
                    }
                )
                .await
                .unwrap()
                .0
        );
    }
    #[tokio::test]
    async fn room_update_and_receipt_rollback_together_and_restart_proves_no_unsettled_append() {
        let (store, run, action, _, _) = fixture_for_target(true).await;
        let actor = PrincipalId(1);
        let detail = store
            .ai_query_execution_detail(action.id, actor)
            .await
            .unwrap();
        let document = match detail.target {
            AiQueryExecutionTarget::RoomDocument { document_id, .. } => DocumentId(document_id),
            _ => panic!("room target"),
        };
        store
            .claim_ai_query_execution(
                action.id,
                actor,
                ApproveAiQueryExecution {
                    fingerprint: action.fingerprint.clone(),
                    desktop_id: run.run.desktop_id,
                },
            )
            .await
            .unwrap();
        let update = || NewDocumentUpdate {
            update_id: action.id.to_string(),
            replica_id: "sift-ai-query-execution".into(),
            submitted_by: actor,
            update_bytes: b"validated actor update fixture".to_vec(),
            decoded_len: b"validated actor update fixture".len() as i64,
        };
        store.conn().unwrap().execute_batch("CREATE TRIGGER reject_query_receipt BEFORE UPDATE OF state ON ai_query_execution WHEN NEW.state='completed' BEGIN SELECT RAISE(ABORT,'injected receipt persistence failure'); END;").unwrap();
        let append = || {
            store.append_ai_query_execution_document_update(
                action.id,
                &action.fingerprint,
                document,
                update(),
                &hash(detail.original_sql.as_bytes()),
                &hash(detail.proposal.proposed_sql.as_bytes()),
            )
        };
        assert!(append().is_err());
        assert!(store
            .list_document_updates_since(document, 0)
            .unwrap()
            .is_empty());
        assert_eq!(store.get_document(document).unwrap().next_update_seq, 1);
        assert_eq!(
            store
                .ai_query_execution(action.id, actor)
                .await
                .unwrap()
                .state,
            AiExecutionState::Applying
        );
        store
            .conn()
            .unwrap()
            .execute_batch("DROP TRIGGER reject_query_receipt")
            .unwrap();
        let seq = append().unwrap();
        assert_eq!(seq, 1);
        let receipt = store
            .ai_query_execution(action.id, actor)
            .await
            .unwrap()
            .receipt
            .unwrap();
        assert_eq!(receipt.evidence, AiQueryEditEvidence::RoomCommit);
        assert_eq!(receipt.document_sequence, Some(seq));
        assert!(append().is_err());
        assert_eq!(
            store
                .list_document_updates_since(document, 0)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(store.interrupt_ai_database_applies().await.unwrap(), 0);
        let (store, run, pending, _, _) = fixture_for_target(true).await;
        store
            .claim_ai_query_execution(
                pending.id,
                actor,
                ApproveAiQueryExecution {
                    fingerprint: pending.fingerprint.clone(),
                    desktop_id: run.run.desktop_id,
                },
            )
            .await
            .unwrap();
        assert_eq!(store.interrupt_ai_database_applies().await.unwrap(), 1);
        let recovered = store.ai_query_execution(pending.id, actor).await.unwrap();
        assert_eq!(recovered.state, AiExecutionState::Completed);
        assert_eq!(
            recovered.receipt.unwrap().outcome,
            AiQueryEditOutcome::Refused
        );
    }
}
