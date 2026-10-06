//! Encrypted, reviewable AI SQL drafts. Staging never mutates a Sift document.

use chrono::Utc;
use rusqlite::{params, OptionalExtension};
use sha2::{Digest, Sha256};
use sift_protocol::{
    AiProposal, AiProposalKind, AiProposalStatus, AiQueryProposalDetail,
    StageAiQueryProposalRequest, ToolContext,
};
use uuid::Uuid;

use super::{sqlite_blocking, MetadataError, MetadataStore, PrincipalId, Result};

const MAX_SQL_BYTES: usize = 64 * 1024;

impl MetadataStore {
    /// Record a human application after the desktop has checked and edited
    /// its local buffer. The server cannot independently inspect scratch tabs.
    pub async fn mark_ai_query_proposal_applied(
        &self,
        chat_id: Uuid,
        proposal_id: Uuid,
        actor: PrincipalId,
        expected_revision: u64,
    ) -> Result<AiQueryProposalDetail> {
        let store = self.clone();
        sqlite_blocking(move || {
            let mut conn = store.conn()?;
            let tx = conn.transaction()?;
            super::ai::require_chat_access(&tx, chat_id, actor)?;
            let (run_id, base_revision, status, created_by): (String, u64, String, i64) = tx
                .query_row(
                    "SELECT run_id,base_revision,status,created_by FROM ai_proposal WHERE id=?1 AND chat_id=?2 AND kind='query_text_patch'",
                    params![proposal_id.to_string(),chat_id.to_string()],
                    |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
                ).optional()?.ok_or(MetadataError::AiNotFound)?;
            let shared_editor: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM ai_chat c JOIN room_member m ON m.room_id=c.room_id
                 WHERE c.id=?1 AND c.visibility='room_public' AND m.principal_id=?2 AND m.role IN ('owner','editor'))",
                params![chat_id.to_string(), actor.0], |row| row.get(0))?;
            if status != "staged" || (created_by != actor.0 && !shared_editor) || base_revision != expected_revision {
                return Err(MetadataError::AiInvalid("proposal is no longer applicable to this revision".into()));
            }
            let sequence: u64 = tx.query_row("SELECT next_sequence FROM ai_run WHERE id=?1",
                [&run_id], |row| row.get(0))?;
            if sequence > 1_024 { return Err(MetadataError::AiInvalid("AI run event limit reached".into())); }
            let now = Utc::now().to_rfc3339();
            tx.execute("UPDATE ai_proposal SET status='applied',applied_by=?2,updated_at=?3 WHERE id=?1",
                params![proposal_id.to_string(),actor.0,now])?;
            tx.execute("INSERT INTO ai_run_event(run_id,sequence,kind,at,proposal_id) VALUES(?1,?2,'proposal_applied',?3,?4)",
                params![run_id,sequence,now,proposal_id.to_string()])?;
            tx.execute("UPDATE ai_run SET next_sequence=next_sequence+1 WHERE id=?1", [&run_id])?;
            tx.commit()?;
            Ok(())
        }).await?;
        self.list_ai_query_proposals(chat_id, actor)
            .await?
            .into_iter()
            .find(|detail| detail.proposal.id == proposal_id)
            .ok_or(MetadataError::AiNotFound)
    }

    pub async fn stage_ai_query_proposal(
        &self,
        run_id: Uuid,
        actor: PrincipalId,
        request: StageAiQueryProposalRequest,
    ) -> Result<AiQueryProposalDetail> {
        self.stage_ai_query_proposal_with_limit(run_id, actor, request, 20)
            .await
    }

    pub async fn stage_ai_query_proposal_with_limit(
        &self,
        run_id: Uuid,
        actor: PrincipalId,
        request: StageAiQueryProposalRequest,
        max_calls: u32,
    ) -> Result<AiQueryProposalDetail> {
        if request.proposed_sql.trim().is_empty() || request.proposed_sql.len() > MAX_SQL_BYTES {
            return Err(MetadataError::AiInvalid(
                "proposed SQL is empty or too large".into(),
            ));
        }
        let target = serde_json::to_vec(&request.target)?;
        if target.len() > 4096 {
            return Err(MetadataError::AiInvalid(
                "proposal target is too large".into(),
            ));
        }
        let digest = format!("{:x}", Sha256::digest(request.proposed_sql.as_bytes()));
        let publication_target = request.target.clone();
        let store = self.clone();
        let (chat_id, tenant, context_handle) = sqlite_blocking(move || {
            let conn = store.conn()?;
            let (tenant, status) = super::ai_run::ai_run_authorized_conn(
                &conn, run_id, actor, request.lease_token,
            )?;
            if status != "running" {
                return Err(MetadataError::AiInvalid("AI run is no longer active".into()));
            }
            let (chat_id, mode, context_handle, visibility): (String,String,String,String) = conn.query_row(
                "SELECT r.chat_id,r.mode,r.context_handle,c.visibility FROM ai_run r JOIN ai_chat c ON c.id=r.chat_id WHERE r.id=?1",
                [run_id.to_string()], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
            )?;
            if mode != "propose" {
                return Err(MetadataError::AiInvalid("Read mode cannot stage changes".into()));
            }
            if visibility != "private" {
                let document = publication_target.document_id.as_deref().and_then(|id| id.parse::<i64>().ok())
                    .ok_or_else(|| MetadataError::AiInvalid("public proposals require a room document".into()))?;
                let published: bool = conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM document d JOIN ai_chat c ON c.room_id=d.room_id
                     WHERE c.id=?1 AND d.id=?2 AND c.room_id=?3)",
                    params![chat_id, document, publication_target.room_id], |row| row.get(0))?;
                if !published { return Err(MetadataError::AiAccessDenied); }
            }
            Ok((Uuid::parse_str(&chat_id).map_err(|_|MetadataError::AiContent("invalid AI chat ID".into()))?,tenant,context_handle))
        }).await?;
        let context_bytes = self
            .ai_content
            .get(tenant, &context_handle)
            .await?
            .ok_or_else(|| MetadataError::AiContent("AI run context is unavailable".into()))?;
        let context: sift_protocol::AiTurnContext = serde_json::from_slice(&context_bytes)?;
        if request.target != context.target {
            return Err(MetadataError::AiInvalid(
                "proposal target differs from the turn snapshot".into(),
            ));
        }
        if let Some(existing) = self
            .prior_ai_proposal(run_id, actor, request.client_request_id)
            .await?
        {
            if existing.proposal.content_sha256 == digest
                && existing.proposal.target == request.target
                && existing.proposal.base_revision == Some(request.base_revision)
            {
                return Ok(existing);
            }
            return Err(MetadataError::AiInvalid(
                "proposal request ID was reused with different content".into(),
            ));
        }
        let target_handle = self.ai_content.put(tenant, &target).await?;
        let content_handle = match self
            .ai_content
            .put(tenant, request.proposed_sql.as_bytes())
            .await
        {
            Ok(handle) => handle,
            Err(error) => {
                self.ai_content.delete(tenant, &target_handle).await?;
                return Err(error);
            }
        };
        let id = Uuid::new_v4();
        let now = Utc::now();
        let target_for_db = target_handle.clone();
        let content_for_db = content_handle.clone();
        let digest_for_db = digest.clone();
        let store = self.clone();
        let result = sqlite_blocking(move || {
            let mut conn = store.conn()?;
            let tx = conn.transaction()?;
            let (_,status) = super::ai_run::ai_run_authorized_conn(&tx,run_id,actor,request.lease_token)?;
            if status != "running" { return Err(MetadataError::AiInvalid("AI run is no longer active".into())); }
            super::ai_run::ensure_ai_tool_budget_conn(&tx, run_id, max_calls)?;
            let count:u64=tx.query_row("SELECT COUNT(*) FROM ai_proposal WHERE chat_id=?1",
                [chat_id.to_string()],|row|row.get(0))?;
            if count>=200 { return Err(MetadataError::AiInvalid("AI chat proposal limit reached".into())); }
            tx.execute(
                "INSERT INTO ai_proposal(id,client_request_id,chat_id,run_id,kind,status,target_handle,content_handle,base_revision,content_sha256,created_by,created_at,updated_at)
                 VALUES(?1,?2,?3,?4,'query_text_patch','staged',?5,?6,?7,?8,?9,?10,?10)",
                params![id.to_string(),request.client_request_id.to_string(),chat_id.to_string(),run_id.to_string(),target_for_db,content_for_db,request.base_revision,digest_for_db,actor.0,now.to_rfc3339()],
            )?;
            let sequence:u64=tx.query_row("SELECT next_sequence FROM ai_run WHERE id=?1",[run_id.to_string()],|row|row.get(0))?;
            if sequence>1_024 { return Err(MetadataError::AiInvalid("AI run event limit reached".into())); }
            tx.execute("INSERT INTO ai_run_event(run_id,sequence,kind,at,proposal_id) VALUES(?1,?2,'proposal_created',?3,?4)",
                params![run_id.to_string(),sequence,now.to_rfc3339(),id.to_string()])?;
            tx.execute("UPDATE ai_run SET next_sequence=next_sequence+1 WHERE id=?1",[run_id.to_string()])?;
            tx.commit()?;
            Ok(())
        }).await;
        if let Err(error) = result {
            self.ai_content.delete(tenant, &target_handle).await?;
            self.ai_content.delete(tenant, &content_handle).await?;
            if let Some(existing) = self
                .prior_ai_proposal(run_id, actor, request.client_request_id)
                .await?
            {
                if existing.proposal.content_sha256 == digest
                    && existing.proposal.target == request.target
                    && existing.proposal.base_revision == Some(request.base_revision)
                {
                    return Ok(existing);
                }
            }
            return Err(error);
        }
        Ok(AiQueryProposalDetail {
            proposal: AiProposal {
                id,
                chat_id,
                run_id,
                kind: AiProposalKind::QueryTextPatch,
                status: AiProposalStatus::Staged,
                target: request.target,
                base_revision: Some(request.base_revision),
                content_sha256: digest,
                created_by: actor.0,
                applied_by: None,
                created_at: now,
                updated_at: now,
            },
            proposed_sql: request.proposed_sql,
        })
    }

    pub async fn list_ai_query_proposals(
        &self,
        chat_id: Uuid,
        viewer: PrincipalId,
    ) -> Result<Vec<AiQueryProposalDetail>> {
        let store = self.clone();
        let (tenant,records)=sqlite_blocking(move || {
            let conn=store.conn()?;
            let tenant=super::ai::require_chat_access(&conn,chat_id,viewer)?;
            let mut statement=conn.prepare(
                "SELECT id,run_id,kind,status,target_handle,content_handle,base_revision,content_sha256,created_by,applied_by,created_at,updated_at
                 FROM ai_proposal WHERE chat_id=?1 ORDER BY created_at,id LIMIT 200")?;
            let records=statement.query_map([chat_id.to_string()],|row|Ok((
                row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?,
                row.get::<_,String>(4)?,row.get::<_,String>(5)?,row.get::<_,Option<u64>>(6)?,row.get::<_,String>(7)?,
                row.get::<_,i64>(8)?,row.get::<_,Option<i64>>(9)?,row.get::<_,String>(10)?,row.get::<_,String>(11)?
            )))?.collect::<std::result::Result<Vec<_>,_>>()?;
            Ok((tenant,records))
        }).await?;
        let mut proposals = Vec::with_capacity(records.len());
        for row in records {
            if row.2 != "query_text_patch" {
                continue;
            }
            let target =
                self.ai_content.get(tenant, &row.4).await?.ok_or_else(|| {
                    MetadataError::AiContent("proposal target unavailable".into())
                })?;
            let content =
                self.ai_content.get(tenant, &row.5).await?.ok_or_else(|| {
                    MetadataError::AiContent("proposal content unavailable".into())
                })?;
            let target: ToolContext = serde_json::from_slice(&target)?;
            let proposed_sql = String::from_utf8(content)
                .map_err(|_| MetadataError::AiContent("proposal SQL is invalid UTF-8".into()))?;
            proposals.push(AiQueryProposalDetail {
                proposal: AiProposal {
                    id: parse_id(&row.0)?,
                    chat_id,
                    run_id: parse_id(&row.1)?,
                    kind: AiProposalKind::QueryTextPatch,
                    status: parse_status(&row.3)?,
                    target,
                    base_revision: row.6,
                    content_sha256: row.7,
                    created_by: row.8,
                    applied_by: row.9,
                    created_at: super::parse_time_sql(row.10)?,
                    updated_at: super::parse_time_sql(row.11)?,
                },
                proposed_sql,
            });
        }
        Ok(proposals)
    }

    pub async fn discard_ai_query_proposal(
        &self,
        chat_id: Uuid,
        proposal_id: Uuid,
        actor: PrincipalId,
    ) -> Result<AiQueryProposalDetail> {
        let store = self.clone();
        sqlite_blocking(move || {
            let mut conn=store.conn()?;
            let tx=conn.transaction()?;
            super::ai::require_chat_access(&tx,chat_id,actor)?;
            let changed=tx.execute(
                "UPDATE ai_proposal SET status='discarded',updated_at=?4
                 WHERE id=?1 AND chat_id=?2 AND kind='query_text_patch' AND status='staged' AND (created_by=?3 OR EXISTS(
                    SELECT 1 FROM ai_chat c JOIN room_member m ON m.room_id=c.room_id
                    WHERE c.id=?2 AND c.visibility='room_public' AND m.principal_id=?3 AND m.role IN ('owner','editor')))",
                params![proposal_id.to_string(),chat_id.to_string(),actor.0,Utc::now().to_rfc3339()],
            )?;
            if changed!=1 { return Err(MetadataError::AiInvalid("proposal is unavailable or no longer staged".into())); }
            let run_id:String=tx.query_row("SELECT run_id FROM ai_proposal WHERE id=?1",
                [proposal_id.to_string()],|row|row.get(0))?;
            let sequence:u64=tx.query_row("SELECT next_sequence FROM ai_run WHERE id=?1",
                [&run_id],|row|row.get(0))?;
            if sequence>1_024 { return Err(MetadataError::AiInvalid("AI run event limit reached".into())); }
            tx.execute("INSERT INTO ai_run_event(run_id,sequence,kind,at,proposal_id) VALUES(?1,?2,'proposal_discarded',?3,?4)",
                params![run_id,sequence,Utc::now().to_rfc3339(),proposal_id.to_string()])?;
            tx.execute("UPDATE ai_run SET next_sequence=next_sequence+1 WHERE id=?1",[run_id])?;
            tx.commit()?;
            Ok(())
        }).await?;
        self.list_ai_query_proposals(chat_id, actor)
            .await?
            .into_iter()
            .find(|detail| detail.proposal.id == proposal_id)
            .ok_or(MetadataError::AiNotFound)
    }

    async fn prior_ai_proposal(
        &self,
        run_id: Uuid,
        actor: PrincipalId,
        client_request_id: Uuid,
    ) -> Result<Option<AiQueryProposalDetail>> {
        let store = self.clone();
        let existing = sqlite_blocking(move || {
            let conn = store.conn()?;
            let row: Option<(String, String)> = conn
                .query_row(
                    "SELECT p.chat_id,p.id FROM ai_proposal p JOIN ai_run r ON r.id=p.run_id
                 WHERE p.run_id=?1 AND p.client_request_id=?2 AND r.initiator_principal_id=?3",
                    params![run_id.to_string(), client_request_id.to_string(), actor.0],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            row.map(|(chat, id)| Ok((parse_id(&chat)?, parse_id(&id)?)))
                .transpose()
        })
        .await?;
        let Some((chat_id, id)) = existing else {
            return Ok(None);
        };
        Ok(self
            .list_ai_query_proposals(chat_id, actor)
            .await?
            .into_iter()
            .find(|detail| detail.proposal.id == id))
    }
}

fn parse_id(raw: &str) -> Result<Uuid> {
    Uuid::parse_str(raw).map_err(|_| MetadataError::AiContent("invalid proposal ID".into()))
}
fn parse_status(raw: &str) -> Result<AiProposalStatus> {
    match raw {
        "staged" => Ok(AiProposalStatus::Staged),
        "applied" => Ok(AiProposalStatus::Applied),
        "discarded" => Ok(AiProposalStatus::Discarded),
        "conflicted" => Ok(AiProposalStatus::Conflicted),
        _ => Err(MetadataError::AiContent("invalid proposal status".into())),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{MemorySecretStore, TenantId};
    use sift_protocol::{AiMode, AiProvider, AiTurnContext, StartAiTurnRequest};

    #[tokio::test]
    async fn proposal_stages_encrypted_sql_without_applying_it() {
        let store = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        store.bootstrap_local("owner").unwrap();
        let actor = PrincipalId(1);
        let chat = store
            .create_ai_chat(
                TenantId(1),
                None,
                actor,
                sift_protocol::AiVisibility::Private,
                "SQL review".into(),
            )
            .await
            .unwrap();
        let target = ToolContext {
            tenant_id: Some(1),
            room_id: None,
            profile_id: None,
            connection_id: None,
            document_id: Some("draft-tab".into()),
        };
        let lease = store
            .start_ai_run(
                chat.id,
                actor,
                StartAiTurnRequest {
                    attachment_previews: Vec::new(),
                    client_request_id: Uuid::new_v4(),
                    desktop_id: Uuid::new_v4(),
                    prompt: "Improve this SQL".into(),
                    provider: AiProvider::Codex,
                    model: None,
                    mode: AiMode::Propose,
                    context: AiTurnContext {
                        external_sources: Vec::new(),
                        inclusion: Default::default(),
                        workspace: None,
                        attachments: Vec::new(),
                        target: target.clone(),
                        editor_item_id: None,
                        database: None,
                        dialect: Some("postgres".into()),
                        environment_label: None,
                        sql: None,
                        current_error: None,
                        staged_change_count: 0,
                        publication_id: None,
                    },
                },
            )
            .await
            .unwrap();
        let request = StageAiQueryProposalRequest {
            client_request_id: Uuid::new_v4(),
            lease_token: lease.lease_token,
            target: target.clone(),
            base_revision: 7,
            proposed_sql: "SELECT id FROM customers".into(),
        };
        let first = store
            .stage_ai_query_proposal(lease.run.id, actor, request.clone())
            .await
            .unwrap();
        let retry = store
            .stage_ai_query_proposal(lease.run.id, actor, request)
            .await
            .unwrap();
        assert_eq!(first.proposal.id, retry.proposal.id);
        assert_eq!(first.proposal.status, AiProposalStatus::Staged);
        assert_eq!(
            store
                .list_ai_query_proposals(chat.id, actor)
                .await
                .unwrap()
                .len(),
            1
        );
        let handle: String = store
            .conn()
            .unwrap()
            .query_row(
                "SELECT content_handle FROM ai_proposal WHERE id=?1",
                [first.proposal.id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_ne!(handle, "SELECT id FROM customers");
        let events = store
            .list_ai_run_events(lease.run.id, actor, 0)
            .await
            .unwrap();
        assert_eq!(events[1].proposal_id, Some(first.proposal.id));
        let discarded = store
            .discard_ai_query_proposal(chat.id, first.proposal.id, actor)
            .await
            .unwrap();
        assert_eq!(discarded.proposal.status, AiProposalStatus::Discarded);
        assert!(matches!(
            store
                .discard_ai_query_proposal(chat.id, first.proposal.id, actor)
                .await,
            Err(MetadataError::AiInvalid(_))
        ));
    }

    #[tokio::test]
    async fn applied_proposal_requires_the_reviewed_revision() {
        let store = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        store.bootstrap_local("owner").unwrap();
        let actor = PrincipalId(1);
        let chat = store
            .create_ai_chat(
                TenantId(1),
                None,
                actor,
                sift_protocol::AiVisibility::Private,
                "Revision check".into(),
            )
            .await
            .unwrap();
        let target = ToolContext {
            tenant_id: Some(1),
            room_id: None,
            profile_id: None,
            connection_id: None,
            document_id: None,
        };
        let lease = store
            .start_ai_run(
                chat.id,
                actor,
                StartAiTurnRequest {
                    attachment_previews: Vec::new(),
                    client_request_id: Uuid::new_v4(),
                    desktop_id: Uuid::new_v4(),
                    prompt: "Draft SQL".into(),
                    provider: AiProvider::Codex,
                    model: None,
                    mode: AiMode::Propose,
                    context: AiTurnContext {
                        external_sources: Vec::new(),
                        inclusion: Default::default(),
                        workspace: None,
                        attachments: Vec::new(),
                        target: target.clone(),
                        editor_item_id: Some(9),
                        database: None,
                        dialect: None,
                        environment_label: None,
                        sql: None,
                        current_error: None,
                        staged_change_count: 0,
                        publication_id: None,
                    },
                },
            )
            .await
            .unwrap();
        let draft = store
            .stage_ai_query_proposal(
                lease.run.id,
                actor,
                StageAiQueryProposalRequest {
                    client_request_id: Uuid::new_v4(),
                    lease_token: lease.lease_token,
                    target,
                    base_revision: 7,
                    proposed_sql: "SELECT 2".into(),
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            store
                .mark_ai_query_proposal_applied(chat.id, draft.proposal.id, actor, 8)
                .await,
            Err(MetadataError::AiInvalid(_))
        ));
        let applied = store
            .mark_ai_query_proposal_applied(chat.id, draft.proposal.id, actor, 7)
            .await
            .unwrap();
        assert_eq!(applied.proposal.status, AiProposalStatus::Applied);
        assert_eq!(applied.proposal.applied_by, Some(actor.0));
        assert!(matches!(
            store
                .mark_ai_query_proposal_applied(chat.id, draft.proposal.id, actor, 7)
                .await,
            Err(MetadataError::AiInvalid(_))
        ));
    }
}
