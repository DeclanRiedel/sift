//! Immutable SQL requests and human-approved single-dispatch claims.
use super::{sqlite_blocking, MetadataError, MetadataStore, PrincipalId, Result};
use chrono::Utc;
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};
use sift_protocol::*;
use uuid::Uuid;

fn digest(value: &impl serde::Serialize) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}
fn state(value: &str) -> Result<AiExecutionState> {
    serde_json::from_value(serde_json::Value::String(value.into())).map_err(Into::into)
}
fn text(value: AiExecutionState) -> &'static str {
    match value {
        AiExecutionState::AwaitingApproval => "awaiting_approval",
        AiExecutionState::Applying => "applying",
        AiExecutionState::Completed => "completed",
        AiExecutionState::Denied => "denied",
        AiExecutionState::OutcomeUnknown => "outcome_unknown",
        AiExecutionState::Reconciled => "reconciled",
    }
}
fn execution_access(
    conn: &rusqlite::Connection,
    id: Uuid,
    actor: PrincipalId,
) -> Result<(i64, Uuid)> {
    let (tenant, chat, owner): (i64, String, i64) = conn
        .query_row(
            "SELECT tenant_id,chat_id,initiator_id FROM ai_sql_execution WHERE id=?1",
            [id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
        .ok_or(MetadataError::AiNotFound)?;
    let chat = Uuid::parse_str(&chat)
        .map_err(|_| MetadataError::AiContent("Invalid execution chat".into()))?;
    super::ai::require_chat_access(conn, chat, actor)?;
    if owner != actor.0 {
        return Err(MetadataError::AiAccessDenied);
    }
    Ok((tenant, chat))
}
impl MetadataStore {
    pub async fn prior_ai_sql_execution(
        &self,
        run: Uuid,
        actor: PrincipalId,
        request: &RequestAiSqlExecution,
    ) -> Result<Option<AiSqlExecutionSummary>> {
        let request_fp = digest(&(run, request.call_id, &request.sql, &request.params))?;
        let call = request.call_id;
        let token = request.lease_token;
        let store = self.clone();
        let id = sqlite_blocking(move || {
            let conn = store.conn()?;
            super::ai_run::ai_run_authorized_conn(&conn, run, actor, token)?;
            let prior: Option<(String, String)> = conn
                .query_row(
                    "SELECT id,request_sha256 FROM ai_sql_execution WHERE run_id=?1 AND call_id=?2",
                    params![run.to_string(), call.to_string()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            prior
                .map(|(id, fp)| {
                    if fp != request_fp {
                        return Err(MetadataError::AiInvalid(
                            "Execution call ID changed its immutable request".into(),
                        ));
                    }
                    Uuid::parse_str(&id)
                        .map_err(|_| MetadataError::AiContent("Invalid execution ID".into()))
                })
                .transpose()
        })
        .await?;
        match id {
            Some(id) => self
                .ai_sql_execution(id, actor)
                .await
                .map(|d| Some(d.summary)),
            None => Ok(None),
        }
    }
    pub async fn create_ai_sql_execution(
        &self,
        run: Uuid,
        actor: PrincipalId,
        request: RequestAiSqlExecution,
        input: AiSqlExecutionInput,
        expires: chrono::DateTime<Utc>,
        max_calls: u32,
    ) -> Result<AiSqlExecutionSummary> {
        if request.call_id.is_nil()
            || request.sql.trim().is_empty()
            || request.sql.len() > 64 * 1024
            || request.params.len() > 256
            || serde_json::to_vec(&request.params)?.len() > 256 * 1024
        {
            return Err(MetadataError::AiInvalid(
                "SQL execution request exceeds its bounds".into(),
            ));
        }
        if request.sql != input.sql || request.params != input.params || expires <= Utc::now() {
            return Err(MetadataError::AiInvalid(
                "Execution input or deadline changed".into(),
            ));
        }
        if let Some(prior) = self.prior_ai_sql_execution(run, actor, &request).await? {
            return Ok(prior);
        }
        if self.ai_execute_run_source(run, actor).await? != input.source {
            return Err(MetadataError::AiInvalid(
                "Execution run source changed".into(),
            ));
        }
        let request_fp = digest(&(run, request.call_id, &request.sql, &request.params))?;
        let action_fp = digest(&(run, actor.0, &input))?;
        let handle = self
            .ai_content
            .put(input.source.tenant_id, &serde_json::to_vec(&input)?)
            .await?;
        let stored = handle.clone();
        let store = self.clone();
        let tenant = input.source.tenant_id;
        let id = Uuid::new_v4();
        let call = request.call_id;
        let result=sqlite_blocking(move || {
            let mut conn=store.conn()?;let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (actual_tenant,status)=super::ai_run::ai_run_authorized_conn(&tx,run,actor,request.lease_token)?;
            let (chat,enabled,visibility):(String,bool,String)=tx.query_row("SELECT r.chat_id,r.execute_mode,c.visibility FROM ai_run r JOIN ai_chat c ON c.id=r.chat_id WHERE r.id=?1",[run.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
            if status!="running" || !enabled || actual_tenant!=tenant || visibility!="private" {return Err(MetadataError::AiAccessDenied);}
            let prior:Option<(String,String)>=tx.query_row("SELECT id,request_sha256 FROM ai_sql_execution WHERE run_id=?1 AND call_id=?2",params![run.to_string(),call.to_string()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if let Some((prior,fp))=prior {if fp!=request_fp {return Err(MetadataError::AiInvalid("Execution call ID changed".into()));}return Uuid::parse_str(&prior).map_err(|_|MetadataError::AiContent("Invalid execution ID".into()));}
            super::ai_run::ensure_ai_tool_budget_conn(&tx,run,max_calls)?;
            let used:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM ai_run_event WHERE run_id=?1 AND tool_call_id=?2)",params![run.to_string(),call.to_string()],|r|r.get(0))?;
            if used {return Err(MetadataError::AiInvalid("AI call ID already used".into()));}
            let sequence:u64=tx.query_row("SELECT next_sequence FROM ai_run WHERE id=?1",[run.to_string()],|r|r.get(0))?;
            if sequence>1024 {return Err(MetadataError::AiInvalid("AI event limit reached".into()));}
            tx.execute("INSERT INTO ai_sql_execution(id,run_id,chat_id,tenant_id,initiator_id,call_id,request_sha256,action_sha256,input_handle,state,created_at,expires_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'awaiting_approval',?10,?11)",params![id.to_string(),run.to_string(),chat,tenant,actor.0,call.to_string(),request_fp,action_fp,stored,Utc::now().to_rfc3339(),expires.to_rfc3339()])?;
            tx.execute("INSERT INTO ai_run_event(run_id,sequence,kind,at,tool_call_id) VALUES(?1,?2,'tool_requested',?3,?4)",params![run.to_string(),sequence,Utc::now().to_rfc3339(),call.to_string()])?;
            tx.execute("UPDATE ai_run SET next_sequence=next_sequence+1 WHERE id=?1",[run.to_string()])?;tx.commit()?;Ok(id)
        }).await;
        if !matches!(&result,Ok(saved) if *saved==id) {
            self.ai_content.delete(tenant, &handle).await?;
        }
        self.ai_sql_execution(result?, actor)
            .await
            .map(|d| d.summary)
    }
    pub async fn ai_sql_execution(
        &self,
        id: Uuid,
        actor: PrincipalId,
    ) -> Result<AiSqlExecutionDetail> {
        let store = self.clone();
        let (tenant,summary,input_handle,receipt_handle)=sqlite_blocking(move || {
            let conn=store.conn()?;let (tenant,chat)=execution_access(&conn,id,actor)?;
            let (run,call,approved,fp,phase,created,expires,input,receipt):(String,String,Option<i64>,String,String,String,String,String,Option<String>)=conn.query_row("SELECT run_id,call_id,approved_by,action_sha256,state,created_at,expires_at,input_handle,receipt_handle FROM ai_sql_execution WHERE id=?1",[id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?)))?;
            Ok((tenant,AiSqlExecutionSummary {id,run_id:Uuid::parse_str(&run).map_err(|_|MetadataError::AiContent("Invalid execution run".into()))?,chat_id:chat,call_id:Uuid::parse_str(&call).map_err(|_|MetadataError::AiContent("Invalid execution call".into()))?,initiator_id:actor.0,approved_by:approved,fingerprint:fp,state:state(&phase)?,created_at:super::parse_time_sql(created)?,expires_at:super::parse_time_sql(expires)?,receipt:None},input,receipt))
        }).await?;
        let bytes = self
            .ai_content
            .get(tenant, &input_handle)
            .await?
            .ok_or_else(|| MetadataError::AiContent("Execution input unavailable".into()))?;
        let input: AiSqlExecutionInput = serde_json::from_slice(&bytes)?;
        if digest(&(summary.run_id, actor.0, &input))? != summary.fingerprint {
            return Err(MetadataError::AiContent(
                "Execution input fingerprint mismatch".into(),
            ));
        }
        let mut summary = summary;
        if let Some(handle) = receipt_handle {
            summary.receipt = Some(serde_json::from_slice(
                &self.ai_content.get(tenant, &handle).await?.ok_or_else(|| {
                    MetadataError::AiContent("Execution receipt unavailable".into())
                })?,
            )?);
        }
        // Decryption can await a remote/blocked secret backend. Recheck disclosure
        // authority after those waits, independently of trusted native settlement.
        let store = self.clone();
        sqlite_blocking(move || {
            let conn = store.conn()?;
            execution_access(&conn, id, actor).map(|_| ())
        })
        .await?;
        Ok(AiSqlExecutionDetail { summary, input })
    }
    pub async fn list_ai_sql_executions(
        &self,
        chat: Uuid,
        actor: PrincipalId,
    ) -> Result<Vec<AiSqlExecutionSummary>> {
        let store = self.clone();
        sqlite_blocking(move || {
            let conn=store.conn()?;super::ai::require_chat_access(&conn,chat,actor)?;
            let mut stmt=conn.prepare("SELECT id,run_id,call_id,approved_by,action_sha256,state,created_at,expires_at FROM ai_sql_execution WHERE chat_id=?1 AND initiator_id=?2 ORDER BY created_at DESC,id DESC LIMIT 20")?;
            let rows=stmt.query_map(params![chat.to_string(),actor.0],|r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,Option<i64>>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?,r.get::<_,String>(7)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let id=|s:&str|Uuid::parse_str(s).map_err(|_|MetadataError::AiContent("Invalid execution ID".into()));
            rows.into_iter().map(|(action,run,call,approved,fp,phase,created,expires)|Ok(AiSqlExecutionSummary {
                id:id(&action)?,run_id:id(&run)?,chat_id:chat,call_id:id(&call)?,initiator_id:actor.0,approved_by:approved,fingerprint:fp,state:state(&phase)?,created_at:super::parse_time_sql(created)?,expires_at:super::parse_time_sql(expires)?,receipt:None,
            })).collect()
        }).await
    }

    /// Called only after live source/policy and human production confirmation.
    pub async fn claim_ai_sql_execution(
        &self,
        id: Uuid,
        actor: PrincipalId,
        fingerprint: String,
    ) -> Result<bool> {
        let store = self.clone();
        sqlite_blocking(move || {
            let mut conn=store.conn()?;let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate)?;execution_access(&tx,id,actor)?;
            let (run,fp,phase,expires,enabled,status):(String,String,String,String,bool,String)=tx.query_row("SELECT a.run_id,a.action_sha256,a.state,a.expires_at,r.execute_mode,r.status FROM ai_sql_execution a JOIN ai_run r ON r.id=a.run_id WHERE a.id=?1",[id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?;
            if fp!=fingerprint {return Err(MetadataError::AiInvalid("Execution review changed".into()));}
            if phase!="awaiting_approval" {return Ok(false);}
            if !enabled || status!="running" || super::parse_time_sql(expires)?<=Utc::now() {return Err(MetadataError::AiInvalid("Execution run or review expired".into()));}
            let chat:String=tx.query_row("SELECT chat_id FROM ai_run WHERE id=?1",[&run],|r|r.get(0))?;
            super::ai_mutation_admission::require_clear(&tx, &chat)?;
            tx.execute("UPDATE ai_sql_execution SET state='applying',approved_by=?2 WHERE id=?1",params![id.to_string(),actor.0])?;tx.commit()?;Ok(true)
        }).await
    }
    /// Trusted completion retains the claimed actor even after revocation/Stop.
    pub async fn finish_ai_sql_execution(
        &self,
        id: Uuid,
        actor: PrincipalId,
        fingerprint: String,
        phase: AiExecutionState,
        receipt: AiExecutionReceipt,
    ) -> Result<()> {
        if !matches!(
            phase,
            AiExecutionState::Completed
                | AiExecutionState::Denied
                | AiExecutionState::OutcomeUnknown
        ) {
            return Err(MetadataError::AiInvalid(
                "Invalid execution terminal state".into(),
            ));
        }
        let store = self.clone();
        let tenant=sqlite_blocking(move || {let conn=store.conn()?;conn.query_row("SELECT tenant_id FROM ai_sql_execution WHERE id=?1 AND initiator_id=?2 AND approved_by=?2 AND action_sha256=?3 AND state='applying'",params![id.to_string(),actor.0,fingerprint],|r|r.get::<_,i64>(0)).map_err(Into::into)}).await?;
        let handle = self
            .ai_content
            .put(tenant, &serde_json::to_vec(&receipt)?)
            .await?;
        let stored = handle.clone();
        let store = self.clone();
        let saved=sqlite_blocking(move || {
            let mut conn=store.conn()?;let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (run,call):(String,String)=tx.query_row("SELECT run_id,call_id FROM ai_sql_execution WHERE id=?1",[id.to_string()],|r|Ok((r.get(0)?,r.get(1)?)))?;
            let changed=tx.execute("UPDATE ai_sql_execution SET state=?2,receipt_handle=?3,finished_at=?4 WHERE id=?1 AND state='applying' AND receipt_handle IS NULL",params![id.to_string(),text(phase),stored,Utc::now().to_rfc3339()])?;
            if changed==1 {
                let sequence:u64=tx.query_row("SELECT next_sequence FROM ai_run WHERE id=?1",[&run],|r|r.get(0))?;
                tx.execute("INSERT INTO ai_run_event(run_id,sequence,kind,at,tool_call_id) VALUES(?1,?2,?3,?4,?5)",params![run,sequence,if phase==AiExecutionState::Completed {"tool_completed"}else{"tool_denied"},Utc::now().to_rfc3339(),call])?;
                tx.execute("UPDATE ai_run SET next_sequence=next_sequence+1 WHERE id=?1",[&run])?;
            }
            tx.commit()?;Ok(changed==1)
        }).await;
        if !matches!(saved, Ok(true)) {
            self.ai_content.delete(tenant, &handle).await?;
        }
        if saved? {
            Ok(())
        } else {
            Err(MetadataError::AiInvalid(
                "Execution claim no longer owns settlement".into(),
            ))
        }
    }
    pub async fn deny_ai_sql_execution(
        &self,
        id: Uuid,
        actor: PrincipalId,
        fingerprint: String,
    ) -> Result<()> {
        let store = self.clone();
        sqlite_blocking(move || {
            let mut conn=store.conn()?;let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            execution_access(&tx,id,actor)?;
            let (run,call,fp,phase):(String,String,String,String)=tx.query_row("SELECT run_id,call_id,action_sha256,state FROM ai_sql_execution WHERE id=?1",[id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
            if fp!=fingerprint {return Err(MetadataError::AiInvalid("Execution review changed".into()));}
            if phase=="denied" {return Ok(());}
            if phase!="awaiting_approval" {return Err(MetadataError::AiInvalid("Only a pending SQL request can be denied; use Stop for active work".into()));}
            tx.execute("UPDATE ai_sql_execution SET state='denied',finished_at=?2 WHERE id=?1",params![id.to_string(),Utc::now().to_rfc3339()])?;
            let sequence:u64=tx.query_row("SELECT next_sequence FROM ai_run WHERE id=?1",[&run],|r|r.get(0))?;
            tx.execute("INSERT INTO ai_run_event(run_id,sequence,kind,at,tool_call_id) VALUES(?1,?2,'tool_denied',?3,?4)",params![run,sequence,Utc::now().to_rfc3339(),call])?;
            tx.execute("UPDATE ai_run SET next_sequence=next_sequence+1 WHERE id=?1",[&run])?;
            tx.commit()?;Ok(())
        }).await
    }
    pub async fn reconcile_ai_sql_execution(
        &self,
        id: Uuid,
        actor: PrincipalId,
        fingerprint: String,
    ) -> Result<()> {
        let store = self.clone();
        sqlite_blocking(move || {let conn=store.conn()?;execution_access(&conn,id,actor)?;
            let changed=conn.execute("UPDATE ai_sql_execution SET state='reconciled' WHERE id=?1 AND action_sha256=?2 AND state='outcome_unknown'",params![id.to_string(),fingerprint])?;
            if changed!=1 {return Err(MetadataError::AiInvalid("Only the exact unknown execution can be acknowledged; its outcome remains unknown".into()));}Ok(())}).await
    }
}
impl MetadataStore {
    pub async fn bind_ai_execute_run_source(
        &self,
        run: Uuid,
        actor: PrincipalId,
        source: AiExecutionSource,
    ) -> Result<()> {
        let expected = source.clone();
        let tenant = source.tenant_id;
        let handle = self
            .ai_content
            .put(tenant, &serde_json::to_vec(&source)?)
            .await?;
        let stored = handle.clone();
        let store = self.clone();
        let saved=sqlite_blocking(move || {let conn=store.conn()?;
            let (chat,owner,enabled,status):(String,i64,bool,String)=conn.query_row("SELECT chat_id,initiator_principal_id,execute_mode,status FROM ai_run WHERE id=?1",[run.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
            if owner!=actor.0 || !enabled {return Err(MetadataError::AiAccessDenied);}
            if status!="running" {return Err(MetadataError::AiInvalid("Execute startup ended before source binding".into()));}
            let chat_id=Uuid::parse_str(&chat).map_err(|_|MetadataError::AiContent("Invalid chat".into()))?;
            super::ai::require_chat_access(&conn,chat_id,actor)?;
            let actual_tenant:i64=conn.query_row("SELECT tenant_id FROM ai_chat WHERE id=?1",[chat],|r|r.get(0))?;
            if actual_tenant!=tenant {return Err(MetadataError::AiAccessDenied);}
            conn.execute("INSERT OR IGNORE INTO ai_execute_run_source(run_id,tenant_id,source_handle) VALUES(?1,?2,?3)",params![run.to_string(),tenant,stored]).map_err(Into::into)
        }).await;
        if !matches!(saved, Ok(1)) {
            self.ai_content.delete(tenant, &handle).await?;
        }
        if saved? == 0 && self.ai_execute_run_source(run, actor).await? != expected {
            return Err(MetadataError::AiInvalid(
                "Execution startup source changed".into(),
            ));
        }
        Ok(())
    }
    pub async fn ai_execute_run_source(
        &self,
        run: Uuid,
        actor: PrincipalId,
    ) -> Result<AiExecutionSource> {
        let store = self.clone();
        let (tenant, handle) = sqlite_blocking(move || {
            let conn = store.conn()?;
            let (chat, owner): (String, i64) = conn.query_row(
                "SELECT chat_id,initiator_principal_id FROM ai_run WHERE id=?1",
                [run.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            if owner != actor.0 {
                return Err(MetadataError::AiAccessDenied);
            }
            super::ai::require_chat_access(
                &conn,
                Uuid::parse_str(&chat)
                    .map_err(|_| MetadataError::AiContent("Invalid chat".into()))?,
                actor,
            )?;
            conn.query_row(
                "SELECT tenant_id,source_handle FROM ai_execute_run_source WHERE run_id=?1",
                [run.to_string()],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
            )
            .map_err(Into::into)
        })
        .await?;
        let source =
            serde_json::from_slice(
                &self.ai_content.get(tenant, &handle).await?.ok_or_else(|| {
                    MetadataError::AiContent("Execution source unavailable".into())
                })?,
            )
            .map_err(MetadataError::from)?;
        let store = self.clone();
        sqlite_blocking(move || {
            let conn = store.conn()?;
            let chat: String = conn.query_row(
                "SELECT chat_id FROM ai_run WHERE id=?1 AND initiator_principal_id=?2",
                params![run.to_string(), actor.0],
                |r| r.get(0),
            )?;
            super::ai::require_chat_access(
                &conn,
                Uuid::parse_str(&chat)
                    .map_err(|_| MetadataError::AiContent("Invalid chat".into()))?,
                actor,
            )
            .map(|_| ())
        })
        .await?;
        Ok(source)
    }
    pub fn require_ai_execution_live(&self, run: Uuid, actor: PrincipalId) -> Result<()> {
        let conn = self.conn()?;
        let (chat, owner, enabled, status): (String, i64, bool, String) = conn.query_row(
            "SELECT chat_id,initiator_principal_id,execute_mode,status FROM ai_run WHERE id=?1",
            [run.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        if owner != actor.0 || !enabled || status != "running" {
            return Err(MetadataError::AiAccessDenied);
        }
        super::ai::require_chat_access(
            &conn,
            Uuid::parse_str(&chat).map_err(|_| MetadataError::AiContent("Invalid chat".into()))?,
            actor,
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SecretStore;
    use std::sync::Arc;
    struct PausedSecrets {
        inner: crate::MemorySecretStore,
        pause: std::sync::atomic::AtomicBool,
        reached: tokio::sync::Notify,
        resume: tokio::sync::Notify,
    }
    #[async_trait::async_trait]
    impl SecretStore for PausedSecrets {
        async fn put(&self, namespace: &str, handle: &str, secret: &[u8]) -> Result<()> {
            self.inner.put(namespace, handle, secret).await
        }
        async fn get(&self, namespace: &str, handle: &str) -> Result<Option<Vec<u8>>> {
            if self.pause.swap(false, std::sync::atomic::Ordering::SeqCst) {
                self.reached.notify_one();
                self.resume.notified().await;
            }
            self.inner.get(namespace, handle).await
        }
        async fn delete(&self, namespace: &str, handle: &str) -> Result<()> {
            self.inner.delete(namespace, handle).await
        }
    }

    #[tokio::test]
    async fn sql_actions_pin_inputs_share_quota_and_recover_without_repeating_writes() {
        let secrets = Arc::new(PausedSecrets {
            inner: crate::MemorySecretStore::new(),
            pause: false.into(),
            reached: Default::default(),
            resume: Default::default(),
        });
        let store = MetadataStore::open_in_memory(secrets.clone()).unwrap();
        store.bootstrap_local("owner").unwrap();
        let actor = PrincipalId(1);
        let chat = store
            .create_ai_chat(
                crate::TenantId(1),
                None,
                actor,
                AiVisibility::Private,
                "Execute".into(),
            )
            .await
            .unwrap();
        let turn:StartAiTurnRequest=serde_json::from_value(serde_json::json!({
            "client_request_id":Uuid::new_v4(),"desktop_id":Uuid::new_v4(),"prompt":"Review SQL","provider":"codex","mode":"execute",
            "context":{"target":{"tenant_id":1},"attachments":[],"staged_change_count":0}
        })).unwrap();
        let mut older_reader = store.migration_status().unwrap();
        older_reader.latest_version = 57;
        assert!(
            older_reader.is_current(),
            "unused additive schema remains rollback-compatible"
        );
        let lease = store.start_ai_run(chat.id, actor, turn).await.unwrap();
        let mut older_reader = store.migration_status().unwrap();
        assert_eq!(older_reader.minimum_compatible_version, 58);
        older_reader.latest_version = 57;
        assert!(
            !older_reader.is_current(),
            "old content inventories must not rotate execution keys"
        );
        let source = AiExecutionSource {
            tenant_id: 1,
            profile_id: 1,
            session: SessionId(2),
            connection: ConnectionId(3),
            generation: Uuid::new_v4(),
            profile_digest: "profile proof".into(),
            database_label: "private source".into(),
            production: false,
            engine: Engine::Sqlite,
        };
        store
            .bind_ai_execute_run_source(lease.run.id, actor, source.clone())
            .await
            .unwrap();
        store
            .bind_ai_execute_run_source(lease.run.id, actor, source.clone())
            .await
            .unwrap();
        let mut changed = source.clone();
        changed.generation = Uuid::new_v4();
        assert!(store
            .bind_ai_execute_run_source(lease.run.id, actor, changed)
            .await
            .is_err());
        let request = RequestAiSqlExecution {
            call_id: Uuid::new_v4(),
            lease_token: lease.lease_token,
            sql: "UPDATE private_execution_sentinel SET label=?".into(),
            params: vec![Value::Text("private-bind-sentinel".into())],
        };
        let input = AiSqlExecutionInput {
            sql: request.sql.clone(),
            params: request.params.clone(),
            source,
        };
        let deadline = Utc::now() + chrono::Duration::minutes(1);
        let first = store
            .create_ai_sql_execution(
                lease.run.id,
                actor,
                request.clone(),
                input.clone(),
                deadline,
                2,
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .create_ai_sql_execution(
                    lease.run.id,
                    actor,
                    request.clone(),
                    input.clone(),
                    deadline,
                    2
                )
                .await
                .unwrap()
                .id,
            first.id
        );
        let mut changed = request.clone();
        changed.sql.push_str(" changed");
        assert!(store
            .prior_ai_sql_execution(lease.run.id, actor, &changed)
            .await
            .is_err());
        let second = store
            .create_ai_sql_execution(
                lease.run.id,
                actor,
                RequestAiSqlExecution {
                    call_id: Uuid::new_v4(),
                    ..request.clone()
                },
                input.clone(),
                deadline,
                2,
            )
            .await
            .unwrap();
        assert!(store
            .create_ai_sql_execution(
                lease.run.id,
                actor,
                RequestAiSqlExecution {
                    call_id: Uuid::new_v4(),
                    ..request
                },
                input,
                deadline,
                2
            )
            .await
            .is_err());
        let (a, b) = tokio::join!(
            store.claim_ai_sql_execution(first.id, actor, first.fingerprint.clone()),
            store.claim_ai_sql_execution(first.id, actor, first.fingerprint.clone())
        );
        assert_ne!(a.unwrap(), b.unwrap(), "exactly one approval owns dispatch");
        assert!(store
            .claim_ai_sql_execution(second.id, actor, second.fingerprint.clone())
            .await
            .is_err());
        assert!(
            store.delete_ai_chat(chat.id, actor).await.is_err(),
            "a pending native owner protects its journal"
        );
        let before = store
            .ai_content_handles_offline(Some(crate::TenantId(1)))
            .unwrap();
        store
            .finish_ai_sql_execution(
                first.id,
                actor,
                first.fingerprint.clone(),
                AiExecutionState::OutcomeUnknown,
                AiExecutionReceipt {
                    native_outcome: Some(ChangeLedgerOutcome::OutcomeUnknown),
                    affected_rows: None,
                    result: None,
                    result_code: Some("query_timed_out".into()),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .ai_content_handles_offline(Some(crate::TenantId(1)))
                .unwrap()
                .len(),
            before.len() + 1,
            "recovery/rotation inventory includes the receipt"
        );
        assert!(store
            .claim_ai_sql_execution(second.id, actor, second.fingerprint.clone())
            .await
            .is_err());
        store
            .reconcile_ai_sql_execution(first.id, actor, first.fingerprint)
            .await
            .unwrap();
        let reconciled = store.ai_sql_execution(first.id, actor).await.unwrap();
        assert_eq!(reconciled.summary.state, AiExecutionState::Reconciled);
        assert_eq!(
            reconciled.summary.receipt.unwrap().native_outcome,
            Some(ChangeLedgerOutcome::OutcomeUnknown)
        );
        assert!(store
            .claim_ai_sql_execution(second.id, actor, second.fingerprint)
            .await
            .unwrap());
        assert_eq!(store.interrupt_ai_database_applies().await.unwrap(), 1);
        assert_eq!(
            store
                .ai_sql_execution(second.id, actor)
                .await
                .unwrap()
                .summary
                .state,
            AiExecutionState::OutcomeUnknown
        );
        assert!(store
            .list_ai_sql_executions(chat.id, actor)
            .await
            .unwrap()
            .iter()
            .all(|s| s.receipt.is_none()));
        let input = store.ai_sql_execution(first.id, actor).await.unwrap().input;
        let pending = store
            .create_ai_sql_execution(
                lease.run.id,
                actor,
                RequestAiSqlExecution {
                    call_id: Uuid::new_v4(),
                    lease_token: lease.lease_token,
                    sql: input.sql.clone(),
                    params: input.params.clone(),
                },
                input,
                deadline,
                3,
            )
            .await
            .unwrap();
        store
            .finish_ai_run(
                lease.run.id,
                actor,
                lease.lease_token,
                AiRunStatus::Canceled,
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .ai_sql_execution(pending.id, actor)
                .await
                .unwrap()
                .summary
                .state,
            AiExecutionState::Denied,
            "Stop revokes only unclaimed approvals"
        );
        assert_eq!(
            store
                .ai_sql_execution(second.id, actor)
                .await
                .unwrap()
                .summary
                .state,
            AiExecutionState::OutcomeUnknown,
            "Stop cannot rewrite a native outcome"
        );
        let pinned = store
            .ai_sql_execution(first.id, actor)
            .await
            .unwrap()
            .input
            .source;
        assert!(
            store
                .bind_ai_execute_run_source(lease.run.id, actor, pinned)
                .await
                .is_err(),
            "a stopped startup cannot bind a late source and return a live lease"
        );
        secrets
            .pause
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let reader = store.clone();
        let read = tokio::spawn(async move { reader.ai_sql_execution(first.id, actor).await });
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            secrets.reached.notified(),
        )
        .await
        .unwrap();
        store
            .conn()
            .unwrap()
            .execute(
                "DELETE FROM membership WHERE tenant_id=1 AND principal_id=1",
                [],
            )
            .unwrap();
        secrets.resume.notify_one();
        assert!(
            matches!(
                tokio::time::timeout(std::time::Duration::from_secs(2), read)
                    .await
                    .unwrap()
                    .unwrap(),
                Err(MetadataError::AiNotFound)
            ),
            "revocation during decryption must prevent input/receipt delivery"
        );
        store
            .upsert_tenant_membership(crate::TenantId(1), actor, crate::MembershipRole::Owner)
            .unwrap();
        store.delete_ai_chat(chat.id, actor).await.unwrap();
        assert!(store
            .ai_content_handles_offline(Some(crate::TenantId(1)))
            .unwrap()
            .is_empty());
    }
}
