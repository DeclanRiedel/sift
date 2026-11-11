//! Typed requests reuse the proposal's one-use native apply claim and receipt.
use super::{sqlite_blocking, MetadataError, MetadataStore, PrincipalId, Result};
use chrono::Utc;
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};
use sift_protocol::*;
use uuid::Uuid;

fn invalid(message: &str) -> MetadataError {
    MetadataError::AiInvalid(message.into())
}
fn uuid(text: &str) -> Result<Uuid> {
    Uuid::parse_str(text).map_err(|_| invalid("Invalid typed execution identity"))
}
fn access(conn: &rusqlite::Connection, id: Uuid, actor: PrincipalId) -> Result<()> {
    let (chat, owner): (String, i64) = conn
        .query_row(
            "SELECT chat_id,initiator_id FROM ai_database_execution WHERE id=?1",
            [id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or(MetadataError::AiNotFound)?;
    super::ai::require_chat_access(conn, uuid(&chat)?, actor)?;
    if owner != actor.0 {
        return Err(MetadataError::AiAccessDenied);
    }
    Ok(())
}
fn fingerprint(
    run: Uuid,
    actor: PrincipalId,
    proposal: &AiDatabaseProposalDetail,
    review: &AiDatabaseProposalReview,
    source: &AiExecutionSource,
    expires: chrono::DateTime<Utc>,
) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(
            run,
            actor.0,
            proposal.proposal.id,
            &proposal.proposal.content_sha256,
            review.id,
            &review.review_digest,
            source,
            expires
        ))?)
    ))
}
/// Runs inside the same transaction as the existing durable native claim.
pub(super) fn consume(
    conn: &rusqlite::Connection,
    proposal: Uuid,
    actor: PrincipalId,
    request: &ApplyAiDatabaseProposalRequest,
    proof: Option<&(Uuid, String)>,
) -> Result<()> {
    let linked: Option<String> = conn
        .query_row(
            "SELECT id FROM ai_database_execution WHERE proposal_id=?1",
            [proposal.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    match (linked, proof) {
        (None, None) => Ok(()),
        (Some(linked), Some((id, fp))) if linked == id.to_string() => {
            check(conn, *id, actor, fp)?;
            let matches:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM ai_database_execution WHERE id=?1 AND state='awaiting_approval' AND review_id=?2)",params![id.to_string(),request.review_id.to_string()],|r|r.get(0))?;
            if !matches || request.client_request_id != *id {
                return Err(invalid("Typed execution approval is consumed or changed"));
            }
            conn.execute(
                "UPDATE ai_database_execution SET state='claimed' WHERE id=?1",
                [id.to_string()],
            )?;
            Ok(())
        }
        _ => Err(invalid(
            "Typed proposals require their exact execution approval",
        )),
    }
}
fn check(conn: &rusqlite::Connection, id: Uuid, actor: PrincipalId, fp: &str) -> Result<()> {
    access(conn, id, actor)?;
    let allowed:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM ai_database_execution e JOIN ai_run r ON r.id=e.run_id JOIN ai_chat c ON c.id=e.chat_id WHERE e.id=?1 AND e.fingerprint=?2 AND e.state IN ('awaiting_approval','claimed') AND julianday(e.expires_at)>julianday(?3) AND e.source_profile_id IS NOT NULL AND r.status='running' AND r.execute_mode=1 AND c.visibility='private' AND c.owner_principal_id=?4)",params![id.to_string(),fp,Utc::now().to_rfc3339(),actor.0],|r|r.get(0))?;
    if !allowed {
        return Err(invalid(
            "Typed execution run, source or approval is no longer active",
        ));
    }
    Ok(())
}
impl MetadataStore {
    pub fn ai_database_execution_lifetime(
        &self,
        id: Uuid,
        actor: PrincipalId,
    ) -> Result<(Uuid, chrono::DateTime<Utc>)> {
        let conn = self.conn()?;
        access(&conn, id, actor)?;
        let (run, expires): (String, String) = conn.query_row(
            "SELECT run_id,expires_at FROM ai_database_execution WHERE id=?1",
            [id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok((uuid(&run)?, super::parse_time_sql(expires)?))
    }
    /// Synchronous native-admission predicate; does not inspect encrypted bodies.
    pub fn check_ai_database_execution_admission(
        &self,
        id: Uuid,
        actor: PrincipalId,
        fp: &str,
    ) -> Result<()> {
        let (profile, digest) = {
            let conn = self.conn()?;
            check(&conn, id, actor, fp)?;
            conn.query_row(
                "SELECT source_profile_id,source_digest FROM ai_database_execution WHERE id=?1",
                [id.to_string()],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
            )?
        };
        if self.ai_profile_source_digest_now(super::ConnectionProfileId(profile), actor)? != digest
        {
            return Err(invalid("Typed execution profile or policy changed"));
        }
        Ok(())
    }
    pub async fn prior_ai_database_execution(
        &self,
        run: Uuid,
        actor: PrincipalId,
        request: &RequestAiDatabaseExecution,
    ) -> Result<Option<AiDatabaseExecutionSummary>> {
        let store = self.clone();
        let request = request.clone();
        let id=sqlite_blocking(move || {let conn=store.conn()?;super::ai_run::ai_run_authorized_conn(&conn,run,actor,request.lease_token)?;
            let prior:Option<(String,String)>=conn.query_row("SELECT id,proposal_id FROM ai_database_execution WHERE run_id=?1 AND call_id=?2",params![run.to_string(),request.call_id.to_string()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            prior.map(|(id,proposal)| {if proposal!=request.proposal_id.to_string() {Err(invalid("Typed call identity changed its proposal"))}else {uuid(&id)}}).transpose()
        }).await?;
        match id {
            Some(id) => self.ai_database_execution(id, actor).await.map(Some),
            None => Ok(None),
        }
    }
    pub async fn create_ai_database_execution(
        &self,
        run: Uuid,
        actor: PrincipalId,
        request: RequestAiDatabaseExecution,
        review: AiDatabaseProposalReview,
        expires: chrono::DateTime<Utc>,
        max_calls: u32,
    ) -> Result<AiDatabaseExecutionSummary> {
        if request.call_id.is_nil() || expires <= Utc::now() || expires > review.expires_at {
            return Err(invalid("Typed request identity or expiry is invalid"));
        }
        if let Some(prior) = self
            .prior_ai_database_execution(run, actor, &request)
            .await?
        {
            return Ok(prior);
        }
        let source = self.ai_execute_run_source(run, actor).await?;
        let proposal = self
            .ai_database_proposal(request.proposal_id, actor)
            .await?;
        let saved = self.ai_database_review(review.id, actor).await?;
        if proposal.proposal.run_id != run
            || proposal.apply_state.is_some()
            || proposal.proposal.status != AiProposalStatus::Staged
            || saved.proposal_id != request.proposal_id
            || saved.review_digest != review.review_digest
            || saved.session != source.session
            || saved.connection != source.connection
            || saved.source_digest != source.profile_digest
            || saved.production != source.production
            || saved.database_label != source.database_label
        {
            return Err(invalid(
                "Typed review must match the original Execute source and proposal",
            ));
        }
        let fp = fingerprint(run, actor, &proposal, &saved, &source, expires)?;
        let id = Uuid::new_v4();
        let store = self.clone();
        let id=sqlite_blocking(move|| {
            let mut conn=store.conn()?;let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (tenant,status)=super::ai_run::ai_run_authorized_conn(&tx,run,actor,request.lease_token)?;
            let prior:Option<(String,String)>=tx.query_row("SELECT id,proposal_id FROM ai_database_execution WHERE run_id=?1 AND call_id=?2",params![run.to_string(),request.call_id.to_string()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if let Some((prior,proposal))=prior {if proposal!=request.proposal_id.to_string(){return Err(invalid("Typed call identity changed"));}return uuid(&prior);}
            let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM ai_run r JOIN ai_chat c ON c.id=r.chat_id JOIN ai_proposal p ON p.run_id=r.id JOIN ai_proposal_review v ON v.proposal_id=p.id WHERE r.id=?1 AND r.execute_mode=1 AND c.visibility='private' AND c.owner_principal_id=?2 AND p.id=?3 AND p.status='staged' AND v.id=?4 AND NOT EXISTS(SELECT 1 FROM ai_proposal_apply a WHERE a.proposal_id=p.id))",params![run.to_string(),actor.0,request.proposal_id.to_string(),review.id.to_string()],|r|r.get(0))?;
            if status!="running" || tenant!=source.tenant_id || !valid {return Err(MetadataError::AiAccessDenied);}
            super::ai_run::ensure_ai_tool_budget_conn(&tx,run,max_calls)?;
            let used:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM ai_run_event WHERE run_id=?1 AND tool_call_id=?2)",params![run.to_string(),request.call_id.to_string()],|r|r.get(0))?;
            if used {return Err(invalid("AI call identity already used"));}
            let sequence:u64=tx.query_row("SELECT next_sequence FROM ai_run WHERE id=?1",[run.to_string()],|r|r.get(0))?;
            if sequence>1024 {return Err(invalid("AI event limit reached"));}
            let floor:u32=tx.pragma_query_value(None,"user_version",|r|r.get(0))?;if floor<61 {tx.pragma_update(None,"user_version",61)?;}
            tx.execute("INSERT INTO ai_database_execution(id,run_id,chat_id,tenant_id,initiator_id,call_id,proposal_id,review_id,source_profile_id,source_digest,fingerprint,state,created_at,expires_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'awaiting_approval',?12,?13)",params![id.to_string(),run.to_string(),proposal.proposal.chat_id.to_string(),tenant,actor.0,request.call_id.to_string(),request.proposal_id.to_string(),review.id.to_string(),source.profile_id,source.profile_digest,fp,Utc::now().to_rfc3339(),expires.to_rfc3339()])?;
            tx.execute("INSERT INTO ai_run_event(run_id,sequence,kind,at,tool_call_id) VALUES(?1,?2,'tool_requested',?3,?4)",params![run.to_string(),sequence,Utc::now().to_rfc3339(),request.call_id.to_string()])?;
            tx.execute("UPDATE ai_run SET next_sequence=next_sequence+1 WHERE id=?1",[run.to_string()])?;tx.commit()?;Ok(id)
        }).await?;
        self.ai_database_execution(id, actor).await
    }
    pub async fn ai_database_execution(
        &self,
        id: Uuid,
        actor: PrincipalId,
    ) -> Result<AiDatabaseExecutionSummary> {
        self.database_execution_summary(id, actor, true).await
    }
    async fn database_execution_summary(
        &self,
        id: Uuid,
        actor: PrincipalId,
        read_receipt: bool,
    ) -> Result<AiDatabaseExecutionSummary> {
        let store = self.clone();
        let mut summary=sqlite_blocking(move|| {
            let conn=store.conn()?;access(&conn,id,actor)?;
            let row=conn.query_row("SELECT e.run_id,e.chat_id,e.proposal_id,e.review_id,e.call_id,e.fingerprint,e.state,e.created_at,e.expires_at,a.state,a.reconciled_at FROM ai_database_execution e LEFT JOIN ai_proposal_apply a ON a.proposal_id=e.proposal_id AND a.client_request_id=e.id WHERE e.id=?1",[id.to_string()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?,r.get::<_,String>(7)?,r.get::<_,String>(8)?,r.get::<_,Option<String>>(9)?,r.get::<_,Option<String>>(10)?)))?;
            let expires=super::parse_time_sql(row.8)?;
            let state=match row.9.as_deref() {Some("applying")=>AiExecutionState::Applying,Some("outcome_unknown") if row.10.is_some()=>AiExecutionState::Reconciled,Some("outcome_unknown")=>AiExecutionState::OutcomeUnknown,Some("applied"|"failed")=>AiExecutionState::Completed,_ if row.6=="denied" || expires<=Utc::now()=>AiExecutionState::Denied,_=>AiExecutionState::AwaitingApproval};
            Ok(AiDatabaseExecutionSummary {id,run_id:uuid(&row.0)?,chat_id:uuid(&row.1)?,proposal_id:uuid(&row.2)?,review_id:uuid(&row.3)?,call_id:uuid(&row.4)?,initiator_id:actor.0,fingerprint:row.5,state,created_at:super::parse_time_sql(row.7)?,expires_at:expires,receipt:None})
        }).await?;
        if read_receipt
            && matches!(
                summary.state,
                AiExecutionState::Completed
                    | AiExecutionState::OutcomeUnknown
                    | AiExecutionState::Reconciled
                    | AiExecutionState::Applying
            )
        {
            summary.receipt = self.ai_database_execution_receipt(id, actor).await?;
        }
        Ok(summary)
    }
    async fn ai_database_execution_receipt(
        &self,
        id: Uuid,
        actor: PrincipalId,
    ) -> Result<Option<AiDatabaseApplyReceipt>> {
        let store = self.clone();
        let record=sqlite_blocking(move|| {let conn=store.conn()?;access(&conn,id,actor)?;
            conn.query_row("SELECT a.tenant_id,a.receipt_handle,a.proposal_id,a.state FROM ai_proposal_apply a JOIN ai_database_execution e ON e.proposal_id=a.proposal_id AND e.id=a.client_request_id WHERE e.id=?1",[id.to_string()],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?))).optional().map_err(Into::into)
        }).await?;
        let receipt = if let Some((tenant, Some(handle), proposal, state)) = record {
            let receipt: AiDatabaseApplyReceipt = serde_json::from_slice(
                &self
                    .ai_content
                    .get(tenant, &handle)
                    .await?
                    .ok_or_else(|| invalid("Typed receipt unavailable"))?,
            )?;
            let expected: AiDatabaseApplyState =
                serde_json::from_value(serde_json::Value::String(state))?;
            if receipt.proposal_id != uuid(&proposal)? || receipt.state != expected {
                return Err(invalid("Typed receipt identity mismatch"));
            }
            Some(receipt)
        } else {
            None
        };
        let store = self.clone();
        sqlite_blocking(move || {
            let conn = store.conn()?;
            access(&conn, id, actor)
        })
        .await?;
        Ok(receipt)
    }
    pub async fn ai_database_execution_detail(
        &self,
        id: Uuid,
        actor: PrincipalId,
    ) -> Result<AiDatabaseExecutionDetail> {
        let summary = self.ai_database_execution(id, actor).await?;
        let source = self.ai_execute_run_source(summary.run_id, actor).await?;
        let proposal = self
            .ai_database_proposal(summary.proposal_id, actor)
            .await?;
        let review = self.ai_database_review(summary.review_id, actor).await?;
        if fingerprint(
            summary.run_id,
            actor,
            &proposal,
            &review,
            &source,
            summary.expires_at,
        )? != summary.fingerprint
        {
            return Err(invalid("Typed execution fingerprint changed"));
        }
        let store = self.clone();
        sqlite_blocking(move || {
            let conn = store.conn()?;
            access(&conn, id, actor)
        })
        .await?;
        Ok(AiDatabaseExecutionDetail {
            summary,
            source,
            proposal,
            review,
        })
    }
    pub async fn ai_database_executions(
        &self,
        chat: Uuid,
        actor: PrincipalId,
    ) -> Result<Vec<AiDatabaseExecutionSummary>> {
        let store = self.clone();
        let ids=sqlite_blocking(move|| {let conn=store.conn()?;super::ai::require_chat_access(&conn,chat,actor)?;
            let mut stmt=conn.prepare("SELECT id FROM ai_database_execution WHERE chat_id=?1 AND initiator_id=?2 ORDER BY created_at DESC,id DESC LIMIT 200")?;
            let rows=stmt.query_map(params![chat.to_string(),actor.0],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;rows.iter().map(|s|uuid(s)).collect::<Result<Vec<_>>>()
        }).await?;
        let mut result = Vec::new();
        for id in ids {
            let mut summary = self.database_execution_summary(id, actor, false).await?;
            summary.receipt = None;
            result.push(summary);
        }
        Ok(result)
    }
    pub async fn deny_ai_database_execution(
        &self,
        id: Uuid,
        actor: PrincipalId,
        fp: String,
    ) -> Result<()> {
        let store = self.clone();
        sqlite_blocking(move || {
            let mut conn = store.conn()?;
            let conn = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            access(&conn, id, actor)?;
            let state: (String, String) = conn.query_row(
                "SELECT state,fingerprint FROM ai_database_execution WHERE id=?1",
                [id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            if state.1 != fp || state.0 == "claimed" {
                return Err(invalid(
                    "Only the exact pending typed request can be denied",
                ));
            }
            conn.execute(
                "UPDATE ai_database_execution SET state='denied' WHERE id=?1",
                [id.to_string()],
            )?;
            conn.commit()?;
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    async fn proposal(
        store: &MetadataStore,
        run: &AiRunLease,
        source: &AiExecutionSource,
    ) -> (AiDatabaseProposalDetail, AiDatabaseProposalReview) {
        let detail = store
            .stage_ai_database_proposal(
                run.run.id,
                PrincipalId(1),
                StageAiDatabaseProposalRequest {
                    client_request_id: Uuid::new_v4(),
                    lease_token: run.lease_token,
                    draft: AiDatabaseDraft::RowEditSet {
                        edit_set: EditSet {
                            table: ObjectPath::new("items"),
                            edits: vec![RowEdit::Insert {
                                values: vec![CellEdit {
                                    column: "label".into(),
                                    value: Value::Text("PRIVATE-TYPED-INPUT".into()),
                                }],
                            }],
                        },
                        expected_catalog_revision: CatalogRevision(1),
                    },
                },
                source.profile_digest.clone(),
                "db:typed".into(),
                None,
            )
            .await
            .unwrap();
        let review = store
            .save_ai_database_review(
                PrincipalId(1),
                Uuid::new_v4(),
                AiDatabaseProposalReview {
                    id: Uuid::new_v4(),
                    proposal_id: detail.proposal.id,
                    reviewer_id: 1,
                    session: source.session,
                    connection: source.connection,
                    source_generation: Some(source.generation),
                    source_digest: source.profile_digest.clone(),
                    content_sha256: detail.proposal.content_sha256.clone(),
                    review_digest: String::new(),
                    database_label: source.database_label.clone(),
                    production: false,
                    preview: AiDatabasePreview::RowEditSet {
                        plan: EditPlan {
                            table: ObjectPath::new("items"),
                            identity: IdentitySource::PrimaryKey {
                                columns: vec!["id".into()],
                            },
                            statements: vec![],
                        },
                    },
                    expires_at: Utc::now() + chrono::Duration::minutes(5),
                },
            )
            .await
            .unwrap();
        (detail, review)
    }
    #[tokio::test]
    async fn typed_approval_consumes_one_native_claim_and_stop_preserves_unknown_replay() {
        let store = MetadataStore::open_in_memory(Arc::new(super::super::MemorySecretStore::new()))
            .unwrap();
        store.bootstrap_local("typed owner").unwrap();
        let actor = PrincipalId(1);
        let profile = store
            .upsert_connection_profile(
                super::super::TenantId(1),
                actor,
                super::super::NewConnectionProfile {
                    name: "typed".into(),
                    provider_id: Engine::Postgres.provider_id(),
                    semantic_engine: Some(Engine::Postgres),
                    configuration: serde_json::json!({"database":"typed"}),
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
                "typed".into(),
            )
            .await
            .unwrap();
        let run=store.start_ai_run(chat.id,actor,StartAiTurnRequest {client_request_id:Uuid::new_v4(),desktop_id:Uuid::new_v4(),prompt:"typed".into(),provider:AiProvider::Codex,model:None,mode:AiMode::Execute,
            context:serde_json::from_value(serde_json::json!({"target":{"tenant_id":1,"profile_id":profile.id.0,"connection_id":"1:1"},"staged_change_count":0})).unwrap(),attachment_previews:vec![]}).await.unwrap();
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
            database_label: "typed".into(),
            production: false,
            engine: Engine::Postgres,
        };
        store
            .bind_ai_execute_run_source(run.run.id, actor, source.clone())
            .await
            .unwrap();
        let (proposal, review) = proposal(&store, &run, &source).await;
        let request = RequestAiDatabaseExecution {
            call_id: Uuid::new_v4(),
            lease_token: run.lease_token,
            proposal_id: proposal.proposal.id,
        };
        let summary = store
            .create_ai_database_execution(
                run.run.id,
                actor,
                request.clone(),
                review.clone(),
                Utc::now() + chrono::Duration::minutes(4),
                100,
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .prior_ai_database_execution(run.run.id, actor, &request)
                .await
                .unwrap()
                .unwrap()
                .id,
            summary.id
        );
        assert!(store
            .prior_ai_database_execution(
                run.run.id,
                actor,
                &RequestAiDatabaseExecution {
                    proposal_id: Uuid::new_v4(),
                    ..request.clone()
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
                    source: source.clone(),
                },
                Utc::now() + chrono::Duration::minutes(4),
                100,
            )
            .await
            .unwrap();
        assert!(store
            .claim_ai_sql_execution(sql.id, actor, sql.fingerprint.clone())
            .await
            .unwrap());
        let apply = ApplyAiDatabaseProposalRequest {
            client_request_id: summary.id,
            review_id: review.id,
            review_digest: review.review_digest.clone(),
            production_confirmation: None,
            acknowledgements: vec![],
        };
        let proof = Some((summary.id, summary.fingerprint.clone()));
        assert!(store
            .claim_ai_database_apply_with_execution(
                proposal.proposal.id,
                actor,
                apply.clone(),
                proof.clone()
            )
            .await
            .is_err());
        assert_eq!(
            store
                .ai_database_execution(summary.id, actor)
                .await
                .unwrap()
                .state,
            AiExecutionState::AwaitingApproval,
            "a busy chat must not consume typed approval"
        );
        store
            .finish_ai_sql_execution(
                sql.id,
                actor,
                sql.fingerprint,
                AiExecutionState::Denied,
                AiExecutionReceipt {
                    native_outcome: None,
                    affected_rows: None,
                    result: None,
                    result_code: Some("not_dispatched".into()),
                },
            )
            .await
            .unwrap();
        assert!(
            store
                .claim_ai_database_apply(proposal.proposal.id, actor, apply.clone())
                .await
                .is_err(),
            "ordinary human apply cannot bypass typed approval"
        );
        assert!(store
            .claim_ai_database_apply_with_execution(
                proposal.proposal.id,
                actor,
                apply.clone(),
                Some((summary.id, "changed".into()))
            )
            .await
            .is_err());
        let (other, other_review) = self::proposal(&store, &run, &source).await;
        let pending = store
            .create_ai_database_execution(
                run.run.id,
                actor,
                RequestAiDatabaseExecution {
                    call_id: Uuid::new_v4(),
                    lease_token: run.lease_token,
                    proposal_id: other.proposal.id,
                },
                other_review,
                Utc::now() + chrono::Duration::minutes(4),
                100,
            )
            .await
            .unwrap();
        let (a, b) = tokio::join!(
            store.claim_ai_database_apply_with_execution(
                proposal.proposal.id,
                actor,
                apply.clone(),
                proof.clone()
            ),
            store.claim_ai_database_apply_with_execution(
                proposal.proposal.id,
                actor,
                apply.clone(),
                proof
            )
        );
        let claims = [a.unwrap(), b.unwrap()];
        assert_eq!(
            claims
                .iter()
                .filter(|c| matches!(c, super::super::AiDatabaseApplyClaim::Claimed))
                .count(),
            1
        );
        assert_eq!(
            store
                .ai_database_execution(summary.id, actor)
                .await
                .unwrap()
                .state,
            AiExecutionState::Applying
        );
        assert!(store
            .deny_ai_database_execution(summary.id, actor, summary.fingerprint.clone())
            .await
            .is_err());
        assert!(store
            .check_ai_database_execution_admission(summary.id, actor, &summary.fingerprint)
            .is_ok());
        store
            .finish_ai_run(run.run.id, actor, run.lease_token, AiRunStatus::Canceled)
            .await
            .unwrap();
        assert!(store
            .check_ai_database_execution_admission(summary.id, actor, &summary.fingerprint)
            .is_err());
        assert_eq!(
            store
                .ai_database_execution(pending.id, actor)
                .await
                .unwrap()
                .state,
            AiExecutionState::Denied
        );
        store
            .finish_ai_database_apply(
                actor,
                apply.clone(),
                AiDatabaseApplyReceipt {
                    proposal_id: proposal.proposal.id,
                    state: AiDatabaseApplyState::OutcomeUnknown,
                    row_result: None,
                    migration_result: None,
                    message: "Unknown native outcome".into(),
                },
            )
            .await
            .unwrap();
        let unknown = store
            .prior_ai_database_execution(run.run.id, actor, &request)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(unknown.state, AiExecutionState::OutcomeUnknown);
        assert_eq!(
            unknown.receipt.unwrap().state,
            AiDatabaseApplyState::OutcomeUnknown
        );
        store
            .reconcile_ai_database_apply(
                proposal.proposal.id,
                actor,
                ReconcileAiDatabaseApply {
                    review_id: review.id,
                    client_request_id: summary.id,
                    review_digest: review.review_digest,
                },
            )
            .await
            .unwrap();
        let reconciled = store
            .ai_database_execution(summary.id, actor)
            .await
            .unwrap();
        assert_eq!(reconciled.state, AiExecutionState::Reconciled);
        assert_eq!(
            reconciled.receipt.unwrap().state,
            AiDatabaseApplyState::OutcomeUnknown
        );
        let mut older = store.migration_status().unwrap();
        assert_eq!(older.minimum_compatible_version, 61);
        older.latest_version = 60;
        assert!(!older.is_current());
        store
            .delete_connection_profile(
                super::super::TenantId(1),
                actor,
                profile.id,
                super::super::NewOperationAudit {
                    actor_principal_id: Some(actor),
                    action: "delete_profile".into(),
                    target: "connection_profile".into(),
                    target_id: Some(profile.id.0),
                    status: "succeeded".into(),
                    result_code: None,
                    row_count: None,
                    error_message: None,
                    correlation_id: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .ai_database_execution(summary.id, actor)
                .await
                .unwrap()
                .receipt
                .unwrap()
                .state,
            AiDatabaseApplyState::OutcomeUnknown,
            "source deletion must retain the original native outcome"
        );
    }
}
