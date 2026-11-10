//! Encrypted AI reviews linked to immutable, redacted extension RPC facts.
use super::{sqlite_blocking, MetadataError, MetadataStore, PrincipalId, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};
use sift_protocol::*;
use uuid::Uuid;

#[derive(Debug)]
pub struct AiExtensionExecutionClaim {
    pub summary: AiExtensionExecutionSummary,
    pub execution: super::ExtensionExecutionClaim,
}

fn invalid(message: &str) -> MetadataError {
    MetadataError::AiInvalid(message.into())
}
fn uuid(raw: &str) -> Result<Uuid> {
    Uuid::parse_str(raw).map_err(|_| invalid("Invalid extension execution identity"))
}
fn hash<T: serde::Serialize>(value: &T) -> Result<String> {
    let mut canonical = serde_json::to_value(value)?;
    // Desktop dependency feature unification can enable preserve_order. Match
    // the governed server binding independently of JSON map insertion order.
    canonical.sort_all_objects();
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&canonical)?)
    ))
}
fn review_hash(
    run: Uuid,
    actor: PrincipalId,
    review: &AiExtensionExecutionReview,
    expires: DateTime<Utc>,
) -> Result<String> {
    hash(&(run, actor.0, review, expires))
}

struct Stored {
    id: Uuid,
    run: Uuid,
    chat: Uuid,
    tenant: i64,
    actor: i64,
    call: Uuid,
    tool: String,
    fingerprint: String,
    request_hash: String,
    input: String,
    result: Option<String>,
    availability: AiExtensionResultAvailability,
    review_state: String,
    execution: Option<Uuid>,
    approved_by: Option<i64>,
    created: DateTime<Utc>,
    expires: DateTime<Utc>,
    acknowledged: Option<DateTime<Utc>>,
}
fn stored(conn: &Connection, id: Uuid, actor: PrincipalId) -> Result<Stored> {
    let row = conn.query_row(
        "SELECT run_id,chat_id,tenant_id,initiator_id,call_id,tool_id,action_sha256,request_sha256,input_handle,result_handle,result_availability,review_state,execution_id,approved_by,created_at,expires_at,acknowledged_at FROM ai_extension_execution WHERE id=?1",
        [id.to_string()], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?,r.get::<_,String>(7)?,r.get::<_,String>(8)?,r.get::<_,Option<String>>(9)?,r.get::<_,String>(10)?,r.get::<_,String>(11)?,r.get::<_,Option<String>>(12)?,r.get::<_,Option<i64>>(13)?,r.get::<_,String>(14)?,r.get::<_,String>(15)?,r.get::<_,Option<String>>(16)?)))
        .optional()?.ok_or(MetadataError::AiNotFound)?;
    if row.3 != actor.0 {
        return Err(MetadataError::AiAccessDenied);
    }
    let chat = uuid(&row.1)?;
    if super::ai::require_chat_access(conn, chat, actor)? != row.2 {
        return Err(MetadataError::AiAccessDenied);
    }
    Ok(Stored {
        id,
        run: uuid(&row.0)?,
        chat,
        tenant: row.2,
        actor: row.3,
        call: uuid(&row.4)?,
        tool: row.5,
        fingerprint: row.6,
        request_hash: row.7,
        input: row.8,
        result: row.9,
        availability: serde_json::from_value(serde_json::Value::String(row.10))?,
        review_state: row.11,
        execution: row.12.as_deref().map(uuid).transpose()?,
        approved_by: row.13,
        created: super::parse_time_sql(row.14)?,
        expires: super::parse_time_sql(row.15)?,
        acknowledged: row.16.map(super::parse_time_sql).transpose()?,
    })
}
fn journal(conn: &Connection, row: &Stored) -> Result<Option<ExtensionExecutionReceipt>> {
    let Some(id) = row.execution else {
        return Ok(None);
    };
    let found = match super::extension_execution::record(conn, id) {
        Ok(record) => record.receipt,
        Err(MetadataError::ExtensionExecutionUnavailable) => return Ok(None),
        Err(error) => return Err(error),
    };
    if found.principal_id != row.actor
        || found.tenant_id != Some(row.tenant)
        || found.request_id != row.id
    {
        return Ok(None);
    }
    Ok(Some(found))
}
fn summary(conn: &Connection, row: &Stored) -> Result<AiExtensionExecutionSummary> {
    let receipt = journal(conn, row)?;
    let uncertain = if row.acknowledged.is_some() {
        AiExecutionState::Reconciled
    } else {
        AiExecutionState::OutcomeUnknown
    };
    let state = match row.review_state.as_str() {
        "denied" => AiExecutionState::Denied,
        "awaiting_approval" => {
            if row.expires <= Utc::now() {
                AiExecutionState::Denied
            } else {
                AiExecutionState::AwaitingApproval
            }
        }
        "issued" => match receipt.as_ref() {
            None => uncertain,
            Some(r) => match r.state {
                ExtensionExecutionState::Claimed | ExtensionExecutionState::Dispatching => {
                    AiExecutionState::Applying
                }
                ExtensionExecutionState::Refused => AiExecutionState::Denied,
                ExtensionExecutionState::OutcomeUnknown => uncertain,
                ExtensionExecutionState::Acknowledged
                    if matches!(
                        r.result_code,
                        Some(
                            ExtensionExecutionCode::RequestTimedOut
                                | ExtensionExecutionCode::OwnerInterrupted
                        )
                    ) =>
                {
                    uncertain
                }
                ExtensionExecutionState::Acknowledged => AiExecutionState::Completed,
            },
        },
        _ => return Err(invalid("Invalid extension review state")),
    };
    let available = state == AiExecutionState::Completed
        && receipt.as_ref().is_some_and(|r| {
            r.response_kind == Some(ExtensionResponseKind::Result)
                && r.result_code == Some(ExtensionExecutionCode::Acknowledged)
        });
    let availability = if row.availability == AiExtensionResultAvailability::Available && !available
    {
        AiExtensionResultAvailability::Withheld
    } else {
        row.availability
    };
    Ok(AiExtensionExecutionSummary {
        id: row.id,
        run_id: row.run,
        chat_id: row.chat,
        call_id: row.call,
        initiator_id: row.actor,
        approved_by: row.approved_by,
        tool_id: row.tool.clone(),
        fingerprint: row.fingerprint.clone(),
        state,
        created_at: row.created,
        expires_at: row.expires,
        execution_id: row.execution,
        receipt,
        result_availability: availability,
        acknowledged_at: row.acknowledged,
    })
}
fn floor(conn: &Connection) -> Result<()> {
    let current: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if current < 64 {
        conn.pragma_update(None, "user_version", 64)?;
    }
    Ok(())
}

/// Compare run/review authority inside the same transaction as dispatch intent.
pub(super) fn require_dispatch_live(
    conn: &Connection,
    execution: Uuid,
    actor: PrincipalId,
    review: Option<Uuid>,
) -> Result<()> {
    let actual: Option<String> = conn
        .query_row(
            "SELECT id FROM ai_extension_execution WHERE execution_id=?1",
            [execution.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    if actual.as_deref() != review.map(|id| id.to_string()).as_deref() {
        return Err(invalid("Extension dispatch review identity changed"));
    }
    if let Some(id) = review {
        let row = stored(conn, id, actor)?;
        let live:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM ai_run WHERE id=?1 AND initiator_principal_id=?2 AND execute_mode=1 AND status='running')",params![row.run.to_string(),actor.0],|r|r.get(0))?;
        if !live || row.review_state != "issued" || row.expires <= Utc::now() {
            return Err(invalid("Extension run or review ended before dispatch"));
        }
    }
    Ok(())
}

impl MetadataStore {
    async fn cleanup_extension_content(&self, tenant: i64, handle: &str) -> Result<()> {
        if self.ai_content.delete(tenant, handle).await.is_ok() {
            return Ok(());
        }
        let handle = handle.to_owned();
        let store = self.clone();
        sqlite_blocking(move|| {let conn=store.conn()?;conn.execute("INSERT OR IGNORE INTO ai_content_cleanup(tenant_id,content_handle,queued_at) VALUES(?1,?2,?3)",params![tenant,handle,super::now_text()])?;Ok(())}).await
    }

    pub async fn ai_extension_execution(
        &self,
        id: Uuid,
        actor: PrincipalId,
    ) -> Result<AiExtensionExecutionSummary> {
        let store = self.clone();
        sqlite_blocking(move || {
            let mut conn = store.conn()?;
            let tx = conn.transaction()?;
            let row = stored(&tx, id, actor)?;
            let result = summary(&tx, &row)?;
            tx.commit()?;
            Ok(result)
        })
        .await
    }
    pub async fn ai_extension_executions(
        &self,
        chat: Uuid,
        actor: PrincipalId,
    ) -> Result<Vec<AiExtensionExecutionSummary>> {
        let store = self.clone();
        sqlite_blocking(move|| {
            let mut conn=store.conn()?;let tx=conn.transaction()?;super::ai::require_chat_access(&tx,chat,actor)?;
            let ids=tx.prepare("SELECT id FROM ai_extension_execution WHERE chat_id=?1 AND initiator_id=?2 ORDER BY created_at DESC,id DESC LIMIT 100")?
                .query_map(params![chat.to_string(),actor.0],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let results=ids.into_iter().map(|id| {let row=stored(&tx,uuid(&id)?,actor)?;summary(&tx,&row)}).collect::<Result<Vec<_>>>()?;
            tx.commit()?;Ok(results)
        }).await
    }
    pub async fn ai_extension_execution_detail(
        &self,
        id: Uuid,
        actor: PrincipalId,
    ) -> Result<AiExtensionExecutionDetail> {
        let store = self.clone();
        let (tenant, input, result, initial) = sqlite_blocking(move || {
            let mut conn = store.conn()?;
            let tx = conn.transaction()?;
            let row = stored(&tx, id, actor)?;
            let result = (
                row.tenant,
                row.input.clone(),
                row.result.clone(),
                summary(&tx, &row)?,
            );
            tx.commit()?;
            Ok(result)
        })
        .await?;
        let bytes = self
            .ai_content
            .get(tenant, &input)
            .await?
            .ok_or_else(|| invalid("Extension review content unavailable"))?;
        let review: AiExtensionExecutionReview = serde_json::from_slice(&bytes)?;
        if review_hash(initial.run_id, actor, &review, initial.expires_at)? != initial.fingerprint {
            return Err(invalid("Extension review content changed"));
        }
        let body = if initial.result_availability == AiExtensionResultAvailability::Available {
            match &result {
                Some(handle) => self
                    .ai_content
                    .get(tenant, handle)
                    .await?
                    .map(|b| serde_json::from_slice(&b))
                    .transpose()?,
                None => None,
            }
        } else {
            None
        };
        // Secret backends may block: recheck current disclosure and exact handles.
        let store = self.clone();
        let mut latest = sqlite_blocking(move || {
            let mut conn = store.conn()?;
            let tx = conn.transaction()?;
            let row = stored(&tx, id, actor)?;
            if row.input != input || row.result != result || row.fingerprint != initial.fingerprint
            {
                return Err(invalid("Extension review changed while reading"));
            }
            let summary = summary(&tx, &row)?;
            tx.commit()?;
            Ok(summary)
        })
        .await?;
        let body = (latest.result_availability == AiExtensionResultAvailability::Available)
            .then_some(body)
            .flatten();
        if latest.result_availability == AiExtensionResultAvailability::Available && body.is_none()
        {
            latest.result_availability = AiExtensionResultAvailability::Unavailable;
        }
        Ok(AiExtensionExecutionDetail {
            summary: latest,
            review,
            result: body,
        })
    }
    pub async fn prior_ai_extension_execution(
        &self,
        run: Uuid,
        actor: PrincipalId,
        request: &RequestAiExtensionExecution,
    ) -> Result<Option<AiExtensionExecutionSummary>> {
        let fingerprint = hash(&(run, request.call_id, &request.tool_id, &request.arguments))?;
        let call = request.call_id;
        let store = self.clone();
        sqlite_blocking(move || {
            let mut conn = store.conn()?;
            let tx = conn.transaction()?;
            let prior: Option<String> = tx
                .query_row(
                    "SELECT id FROM ai_extension_execution WHERE run_id=?1 AND call_id=?2",
                    params![run.to_string(), call.to_string()],
                    |r| r.get(0),
                )
                .optional()?;
            let result = prior
                .map(|id| {
                    let row = stored(&tx, uuid(&id)?, actor)?;
                    if row.request_hash != fingerprint {
                        return Err(invalid("Extension call identity changed"));
                    }
                    summary(&tx, &row)
                })
                .transpose()?;
            tx.commit()?;
            Ok(result)
        })
        .await
    }
    pub async fn create_ai_extension_execution(
        &self,
        run: Uuid,
        actor: PrincipalId,
        request: RequestAiExtensionExecution,
        review: AiExtensionExecutionReview,
        expires: DateTime<Utc>,
        max_calls: u32,
    ) -> Result<AiExtensionExecutionSummary> {
        if request.call_id.is_nil()
            || request.tool_id.is_empty()
            || request.tool_id.len() > 512
            || serde_json::to_vec(&request.arguments)?.len() > 256 * 1024
            || expires <= Utc::now()
            || expires > Utc::now() + chrono::Duration::minutes(5)
            || review.tool.id != request.tool_id
            || review.arguments != request.arguments
        {
            return Err(invalid(
                "Extension request exceeds its bounds or changed its review",
            ));
        }
        if let Some(prior) = self
            .prior_ai_extension_execution(run, actor, &request)
            .await?
        {
            return Ok(prior);
        }
        let active = self.ai_tool_run(run, actor, request.lease_token).await?;
        if active.mode != AiMode::Execute || active.visibility != AiVisibility::Private {
            return Err(MetadataError::AiAccessDenied);
        }
        let original = self.ai_run_detail(active.chat_id, run, actor).await?;
        if original.context.target != review.context
            || self.ai_execute_run_source(run, actor).await? != review.source
            || review.context.tenant_id != Some(review.source.tenant_id)
            || review.context.profile_id != Some(review.source.profile_id)
        {
            return Err(invalid(
                "Extension review differs from its original Execute source",
            ));
        }
        let bytes = serde_json::to_vec(&review)?;
        if bytes.len() > super::ai_content::MAX_CONTENT_BYTES {
            return Err(invalid("Extension review exceeds private content bound"));
        }
        let request_hash = hash(&(run, request.call_id, &request.tool_id, &request.arguments))?;
        let action_hash = review_hash(run, actor, &review, expires)?;
        let tenant = review.source.tenant_id;
        let handle = self.ai_content.put(tenant, &bytes).await?;
        let saved_handle = handle.clone();
        let store = self.clone();
        let id = Uuid::new_v4();
        let created=sqlite_blocking(move|| {
            let mut conn=store.conn()?;let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (actual,status)=super::ai_run::ai_run_authorized_conn(&tx,run,actor,request.lease_token)?;
            let prior:Option<String>=tx.query_row("SELECT id FROM ai_extension_execution WHERE run_id=?1 AND call_id=?2",params![run.to_string(),request.call_id.to_string()],|r|r.get(0)).optional()?;
            if let Some(prior)=prior {let row=stored(&tx,uuid(&prior)?,actor)?;if row.request_hash!=request_hash {return Err(invalid("Extension call identity changed"));}return Ok(row.id);}
            let (chat,enabled,visibility):(String,bool,String)=tx.query_row("SELECT r.chat_id,r.execute_mode,c.visibility FROM ai_run r JOIN ai_chat c ON c.id=r.chat_id WHERE r.id=?1",[run.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
            if actual!=tenant || status!="running" || !enabled || visibility!="private" {return Err(MetadataError::AiAccessDenied);}
            super::ai_run::ensure_ai_tool_budget_conn(&tx,run,max_calls)?;
            let used:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM ai_run_event WHERE run_id=?1 AND tool_call_id=?2)",params![run.to_string(),request.call_id.to_string()],|r|r.get(0))?;
            if used {return Err(invalid("AI call identity was already used"));}
            let sequence:u64=tx.query_row("SELECT next_sequence FROM ai_run WHERE id=?1",[run.to_string()],|r|r.get(0))?;
            if sequence>500 {return Err(invalid("AI event limit reached"));}
            let now=Utc::now().to_rfc3339();
            tx.execute("INSERT INTO ai_extension_execution(id,run_id,chat_id,tenant_id,initiator_id,call_id,tool_id,request_sha256,action_sha256,input_handle,review_state,created_at,expires_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'awaiting_approval',?11,?12)",params![id.to_string(),run.to_string(),chat,tenant,actor.0,request.call_id.to_string(),request.tool_id,request_hash,action_hash,saved_handle,now,expires.to_rfc3339()])?;
            tx.execute("INSERT INTO ai_run_event(run_id,sequence,kind,at,tool_call_id) VALUES(?1,?2,'tool_requested',?3,?4)",params![run.to_string(),sequence,now,request.call_id.to_string()])?;
            tx.execute("UPDATE ai_run SET next_sequence=next_sequence+1 WHERE id=?1",[run.to_string()])?;floor(&tx)?;tx.commit()?;Ok(id)
        }).await;
        if !matches!(&created,Ok(saved) if *saved==id) {
            self.cleanup_extension_content(tenant, &handle).await?;
        }
        self.ai_extension_execution(created?, actor).await
    }

    /// Only a retained human-authorized owner calls this after live preparation.
    pub async fn claim_ai_extension_execution(
        &self,
        id: Uuid,
        actor: PrincipalId,
        fingerprint: String,
        mut input: super::NewExtensionExecution,
    ) -> Result<AiExtensionExecutionClaim> {
        let detail = self.ai_extension_execution_detail(id, actor).await?;
        let expected_context = hash(
            &serde_json::json!({"tool":detail.review.tool,"action":detail.review.prepared_contract}),
        )?;
        let expected_operation: ExtensionOperation = serde_json::from_value(
            detail
                .review
                .prepared_contract
                .get("operation")
                .cloned()
                .ok_or_else(|| invalid("Missing prepared operation"))?,
        )?;
        if input.request_id != id
            || input.binding.principal_id != actor
            || input.binding.operation_id != detail.review.tool.id
            || input.binding.input_fingerprint != hash(&detail.review.arguments)?
            || input.binding.context_fingerprint != expected_context
            || input.operation != expected_operation
            || input.context != detail.review.context
            || input.approval_id.is_some()
        {
            return Err(invalid("Extension claim differs from its frozen review"));
        }
        let store = self.clone();
        sqlite_blocking(move|| {
            let mut conn=store.conn()?;let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate)?;let row=stored(&tx,id,actor)?;
            if row.fingerprint!=fingerprint {return Err(invalid("Extension review changed"));}
            if row.review_state=="issued" {
                let receipt=journal(&tx,&row)?.ok_or_else(||invalid("Issued extension has missing journal evidence"))?;
                let result=AiExtensionExecutionClaim {summary:summary(&tx,&row)?,execution:super::ExtensionExecutionClaim {receipt,claimed:false}};
                tx.commit()?;return Ok(result);
            }
            if row.review_state!="awaiting_approval" || row.expires<=Utc::now() || input.deadline_unix_ms>row.expires.timestamp_millis() {return Err(invalid("Extension review ended or expired"));}
            let live:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM ai_run r JOIN ai_chat c ON c.id=r.chat_id WHERE r.id=?1 AND r.status='running' AND r.execute_mode=1 AND r.initiator_principal_id=?2 AND c.visibility='private')",params![row.run.to_string(),actor.0],|r|r.get(0))?;
            if !live {return Err(MetadataError::AiAccessDenied);}
            super::ai_mutation_admission::require_clear(&tx,&row.chat.to_string())?;
            input.approval_id=Some(super::approval::create_approved_operation_approval_locked(&tx,&input.binding,row.expires)?);
            let execution=super::extension_execution::claim_extension_execution_locked(&tx,&input)?;
            if !execution.claimed {return Err(invalid("Extension identity was already claimed outside this review"));}
            tx.execute("UPDATE ai_extension_execution SET review_state='issued',execution_id=?2,approved_by=?3,approved_at=?4 WHERE id=?1 AND review_state='awaiting_approval'",params![id.to_string(),execution.receipt.id.to_string(),actor.0,super::now_text()])?;
            floor(&tx)?;let result=AiExtensionExecutionClaim {summary:summary(&tx,&stored(&tx,id,actor)?)?,execution};tx.commit()?;Ok(result)
        }).await
    }
    pub async fn deny_ai_extension_execution(
        &self,
        id: Uuid,
        actor: PrincipalId,
        fingerprint: String,
    ) -> Result<AiExtensionExecutionSummary> {
        let store = self.clone();
        sqlite_blocking(move || {
            let mut conn = store.conn()?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let row = stored(&tx, id, actor)?;
            if row.fingerprint != fingerprint {
                return Err(invalid("Extension review changed"));
            }
            if row.review_state == "awaiting_approval" {
                tx.execute(
                    "UPDATE ai_extension_execution SET review_state='denied' WHERE id=?1",
                    [id.to_string()],
                )?;
            }
            let result = summary(&tx, &stored(&tx, id, actor)?)?;
            tx.commit()?;
            Ok(result)
        })
        .await
    }
    pub async fn acknowledge_ai_extension_execution(
        &self,
        id: Uuid,
        actor: PrincipalId,
        request: AcknowledgeAiExtensionExecution,
    ) -> Result<AiExtensionExecutionSummary> {
        let store = self.clone();
        sqlite_blocking(move|| {
            let mut conn=store.conn()?;let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate)?;let row=stored(&tx,id,actor)?;let before=summary(&tx,&row)?;
            if row.fingerprint!=request.fingerprint || row.execution!=Some(request.execution_id) {return Err(invalid("Extension acknowledgement identity changed"));}
            if before.state==AiExecutionState::OutcomeUnknown {tx.execute("UPDATE ai_extension_execution SET acknowledged_at=?2 WHERE id=?1 AND acknowledged_at IS NULL",params![id.to_string(),super::now_text()])?;}
            else if before.state!=AiExecutionState::Reconciled {return Err(invalid("Only an unknown extension outcome can be acknowledged"));}
            let result=summary(&tx,&stored(&tx,id,actor)?)?;tx.commit()?;Ok(result)
        }).await
    }
    /// Independent result retention cannot rewrite the journal or dispatch again.
    pub async fn retain_ai_extension_result(
        &self,
        id: Uuid,
        actor: PrincipalId,
        execution: Uuid,
        result: Option<serde_json::Value>,
    ) -> Result<()> {
        let store = self.clone();
        let tenant = sqlite_blocking(move || {
            let conn = store.conn()?;
            let row = stored(&conn, id, actor)?;
            if row.execution != Some(execution) {
                return Err(invalid("Extension result identity changed"));
            }
            Ok(row.tenant)
        })
        .await?;
        let handle = match result {
            Some(value) => Some(
                self.ai_content
                    .put(tenant, &serde_json::to_vec(&value)?)
                    .await?,
            ),
            None => None,
        };
        let saved = handle.clone();
        let store = self.clone();
        let outcome=sqlite_blocking(move|| {
            let mut conn=store.conn()?;let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate)?;let row=stored(&tx,id,actor)?;
            if row.execution!=Some(execution) || summary(&tx,&row)?.state!=AiExecutionState::Completed {return Err(invalid("Extension result has no timely settled RPC"));}
            let changed=tx.execute("UPDATE ai_extension_execution SET result_handle=?2,result_availability=?3 WHERE id=?1 AND result_availability='not_recorded'",params![id.to_string(),saved,if saved.is_some(){"available"}else{"withheld"}])?;
            tx.commit()?;Ok(changed==1)
        }).await;
        if !matches!(outcome, Ok(true)) {
            if let Some(handle) = handle {
                self.cleanup_extension_content(tenant, &handle).await?;
            }
        }
        outcome.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    struct Fixture {
        store: MetadataStore,
        actor: PrincipalId,
        lease: AiRunLease,
        review: AiExtensionExecutionReview,
        request: RequestAiExtensionExecution,
        summary: AiExtensionExecutionSummary,
    }
    async fn fixture() -> Fixture {
        let store =
            MetadataStore::open_in_memory(Arc::new(crate::MemorySecretStore::new())).unwrap();
        store.bootstrap_local("extension review owner").unwrap();
        let actor = PrincipalId(1);
        let chat = store
            .create_ai_chat(
                crate::TenantId(1),
                None,
                actor,
                AiVisibility::Private,
                "Reviewed extension".into(),
            )
            .await
            .unwrap();
        let context = ToolContext {
            tenant_id: Some(1),
            room_id: None,
            profile_id: Some(1),
            connection_id: Some("2:3".into()),
            document_id: None,
        };
        let turn:StartAiTurnRequest=serde_json::from_value(serde_json::json!({
            "client_request_id":Uuid::new_v4(),"desktop_id":Uuid::new_v4(),"prompt":"review extension",
            "provider":"codex","mode":"execute","context":{"target":context,"attachments":[],"staged_change_count":0}
        })).unwrap();
        let lease = store.start_ai_run(chat.id, actor, turn).await.unwrap();
        let source = AiExecutionSource {
            tenant_id: 1,
            profile_id: 1,
            session: SessionId(2),
            connection: ConnectionId(3),
            generation: Uuid::new_v4(),
            profile_digest: "profile proof".into(),
            database_label: "fixture".into(),
            production: false,
            engine: Engine::Postgres,
        };
        store
            .bind_ai_execute_run_source(lease.run.id, actor, source.clone())
            .await
            .unwrap();
        let operation = ExtensionOperation {
            extension_id: ExtensionId::new("acme/conformance").unwrap(),
            contribution_id: ContributionId::new("acme/conformance/governed_tool/inspect").unwrap(),
            action: SegmentId::new("run").unwrap(),
            classification: OperationClassification::Read,
            target_kind: SegmentId::new("connection").unwrap(),
            target_id: Some("2:3".into()),
            sanitized_arguments: Default::default(),
        };
        let tool = GovernedToolDescriptor {
            id: "fixture.inspect".into(),
            title: "Inspect".into(),
            description: "fixture".into(),
            operation: operation.clone(),
            input_schema: serde_json::json!({"type":"object"}),
            output_schema: serde_json::json!({"type":"object"}),
            required_context: vec![],
            mcp_exposable: true,
            schedulable: false,
            interactive: true,
        };
        let arguments = serde_json::json!({"payload":"private-extension-argument-sentinel"});
        let review = AiExtensionExecutionReview {
            tool,
            context: context.clone(),
            arguments: arguments.clone(),
            prepared_contract: serde_json::json!({"operation":operation,"context":context,"contract":{"manifest":"pinned","grant_revision":1},"source":"pinned-source"}),
            source,
        };
        let request = RequestAiExtensionExecution {
            call_id: Uuid::new_v4(),
            lease_token: lease.lease_token,
            tool_id: review.tool.id.clone(),
            arguments,
        };
        let summary = store
            .create_ai_extension_execution(
                lease.run.id,
                actor,
                request.clone(),
                review.clone(),
                Utc::now() + chrono::Duration::minutes(1),
                40,
            )
            .await
            .unwrap();
        Fixture {
            store,
            actor,
            lease,
            review,
            request,
            summary,
        }
    }
    fn claim_input(f: &Fixture) -> super::super::NewExtensionExecution {
        crate::NewExtensionExecution {
            request_id: f.summary.id,
            binding: crate::ApprovalBinding {
                principal_id: f.actor,
                operation_id: f.review.tool.id.clone(),
                context_fingerprint: hash(
                    &serde_json::json!({"tool":f.review.tool,"action":f.review.prepared_contract}),
                )
                .unwrap(),
                input_fingerprint: hash(&f.review.arguments).unwrap(),
            },
            approval_id: None,
            operation: f.review.tool.operation.clone(),
            context: f.review.context.clone(),
            deadline_unix_ms: (Utc::now() + chrono::Duration::seconds(30)).timestamp_millis(),
            correlation_id: "ai-extension-test".into(),
        }
    }
    fn counts(f: &Fixture) -> (u64, u64) {
        let c = f.store.conn().unwrap();
        (
            c.query_row("SELECT count(*) FROM operation_approval", [], |r| r.get(0))
                .unwrap(),
            c.query_row("SELECT count(*) FROM extension_execution", [], |r| r.get(0))
                .unwrap(),
        )
    }
    #[tokio::test]
    async fn extension_review_claim_is_atomic_private_and_shared_with_native_sql() {
        let f = fixture().await;
        assert_eq!(f.summary.state, AiExecutionState::AwaitingApproval);
        assert_eq!(
            f.store
                .prior_ai_extension_execution(f.lease.run.id, f.actor, &f.request)
                .await
                .unwrap()
                .unwrap()
                .id,
            f.summary.id
        );
        let changed = RequestAiExtensionExecution {
            arguments: serde_json::json!({"payload":"changed"}),
            ..f.request.clone()
        };
        assert!(f
            .store
            .prior_ai_extension_execution(f.lease.run.id, f.actor, &changed)
            .await
            .is_err());
        f.store.conn().unwrap().execute_batch("CREATE TRIGGER reject_ai_extension_journal BEFORE INSERT ON extension_execution BEGIN SELECT RAISE(ABORT,'injected journal failure'); END;").unwrap();
        assert!(f
            .store
            .claim_ai_extension_execution(
                f.summary.id,
                f.actor,
                f.summary.fingerprint.clone(),
                claim_input(&f)
            )
            .await
            .is_err());
        assert_eq!(counts(&f), (0, 0));
        assert_eq!(
            f.store
                .ai_extension_execution(f.summary.id, f.actor)
                .await
                .unwrap()
                .state,
            AiExecutionState::AwaitingApproval
        );
        f.store
            .conn()
            .unwrap()
            .execute_batch("DROP TRIGGER reject_ai_extension_journal")
            .unwrap();
        let issued = f
            .store
            .claim_ai_extension_execution(
                f.summary.id,
                f.actor,
                f.summary.fingerprint.clone(),
                claim_input(&f),
            )
            .await
            .unwrap();
        assert!(issued.execution.claimed);
        assert_eq!(issued.summary.state, AiExecutionState::Applying);
        assert_eq!(counts(&f), (1, 1));
        let replay = f
            .store
            .claim_ai_extension_execution(
                f.summary.id,
                f.actor,
                f.summary.fingerprint.clone(),
                claim_input(&f),
            )
            .await
            .unwrap();
        assert!(!replay.execution.claimed);
        assert_eq!(replay.execution.receipt.id, issued.execution.receipt.id);
        let sql_request = RequestAiSqlExecution {
            call_id: Uuid::new_v4(),
            lease_token: f.lease.lease_token,
            sql: "SELECT 1".into(),
            params: vec![],
        };
        let sql = f
            .store
            .create_ai_sql_execution(
                f.lease.run.id,
                f.actor,
                sql_request.clone(),
                AiSqlExecutionInput {
                    sql: sql_request.sql,
                    params: vec![],
                    source: f.review.source.clone(),
                },
                Utc::now() + chrono::Duration::minutes(1),
                40,
            )
            .await
            .unwrap();
        assert!(f
            .store
            .claim_ai_sql_execution(sql.id, f.actor, sql.fingerprint.clone())
            .await
            .is_err());
        f.store
            .mark_ai_extension_dispatching_guarded(
                issued.execution.receipt.id,
                f.actor,
                None,
                None,
                None,
                f.summary.id,
            )
            .unwrap();
        f.store
            .settle_extension_execution(
                issued.execution.receipt.id,
                f.actor,
                crate::ExtensionExecutionSettlement {
                    state: ExtensionExecutionState::Acknowledged,
                    frame_started: Some(true),
                    response_kind: Some(ExtensionResponseKind::Result),
                    late_response: false,
                    result_code: ExtensionExecutionCode::Acknowledged,
                },
            )
            .unwrap();
        f.store
            .retain_ai_extension_result(
                f.summary.id,
                f.actor,
                issued.execution.receipt.id,
                Some(serde_json::json!({"result":"private-extension-result-sentinel"})),
            )
            .await
            .unwrap();
        let detail = f
            .store
            .ai_extension_execution_detail(f.summary.id, f.actor)
            .await
            .unwrap();
        assert_eq!(detail.summary.state, AiExecutionState::Completed);
        assert_eq!(detail.review.arguments, f.request.arguments);
        assert_eq!(
            detail.result.unwrap()["result"],
            "private-extension-result-sentinel"
        );
        let stored:String=f.store.conn().unwrap().query_row("SELECT e.input_handle||e.result_handle||e.request_sha256||e.action_sha256||j.operation_json||j.context_json FROM ai_extension_execution e JOIN extension_execution j ON j.id=e.execution_id WHERE e.id=?1",[f.summary.id.to_string()],|r|r.get(0)).unwrap();
        assert!(
            !stored.contains("private-extension-argument-sentinel")
                && !stored.contains("private-extension-result-sentinel")
        );
        let missing_handle: String = f
            .store
            .conn()
            .unwrap()
            .query_row(
                "SELECT result_handle FROM ai_extension_execution WHERE id=?1",
                [f.summary.id.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        f.store.ai_content.delete(1, &missing_handle).await.unwrap();
        let lost = f
            .store
            .ai_extension_execution_detail(f.summary.id, f.actor)
            .await
            .unwrap();
        assert_eq!(lost.summary.state, AiExecutionState::Completed);
        assert_eq!(
            lost.summary.result_availability,
            AiExtensionResultAvailability::Unavailable
        );
        assert!(lost.result.is_none());
        assert_eq!(
            f.store
                .migration_status()
                .unwrap()
                .minimum_compatible_version,
            64
        );
        assert!(f
            .store
            .claim_ai_sql_execution(sql.id, f.actor, sql.fingerprint)
            .await
            .unwrap());
        let second = f
            .store
            .create_ai_extension_execution(
                f.lease.run.id,
                f.actor,
                RequestAiExtensionExecution {
                    call_id: Uuid::new_v4(),
                    ..f.request.clone()
                },
                f.review.clone(),
                Utc::now() + chrono::Duration::minutes(1),
                40,
            )
            .await
            .unwrap();
        let mut second_input = claim_input(&f);
        second_input.request_id = second.id;
        assert!(f
            .store
            .claim_ai_extension_execution(second.id, f.actor, second.fingerprint, second_input)
            .await
            .is_err());
        assert_eq!(counts(&f), (1, 1));
    }
    #[tokio::test]
    async fn extension_unknown_acknowledgement_never_reissues_and_cleanup_preserves_journal() {
        let f = fixture().await;
        let issued = f
            .store
            .claim_ai_extension_execution(
                f.summary.id,
                f.actor,
                f.summary.fingerprint.clone(),
                claim_input(&f),
            )
            .await
            .unwrap();
        let id = issued.execution.receipt.id;
        f.store
            .mark_ai_extension_dispatching_guarded(id, f.actor, None, None, None, f.summary.id)
            .unwrap();
        f.store
            .settle_extension_execution(
                id,
                f.actor,
                crate::ExtensionExecutionSettlement {
                    state: ExtensionExecutionState::Acknowledged,
                    frame_started: Some(true),
                    response_kind: Some(ExtensionResponseKind::Result),
                    late_response: true,
                    result_code: ExtensionExecutionCode::OwnerInterrupted,
                },
            )
            .unwrap();
        assert_eq!(
            f.store
                .ai_extension_execution(f.summary.id, f.actor)
                .await
                .unwrap()
                .state,
            AiExecutionState::OutcomeUnknown
        );
        assert!(super::super::ai_mutation_admission::require_clear(
            &f.store.conn().unwrap(),
            &f.summary.chat_id.to_string()
        )
        .is_err());
        assert!(f
            .store
            .acknowledge_ai_extension_execution(
                f.summary.id,
                f.actor,
                AcknowledgeAiExtensionExecution {
                    fingerprint: f.summary.fingerprint.clone(),
                    execution_id: Uuid::new_v4()
                }
            )
            .await
            .is_err());
        let ack = AcknowledgeAiExtensionExecution {
            fingerprint: f.summary.fingerprint.clone(),
            execution_id: id,
        };
        assert_eq!(
            f.store
                .acknowledge_ai_extension_execution(f.summary.id, f.actor, ack.clone())
                .await
                .unwrap()
                .state,
            AiExecutionState::Reconciled
        );
        assert_eq!(
            f.store
                .acknowledge_ai_extension_execution(f.summary.id, f.actor, ack)
                .await
                .unwrap()
                .state,
            AiExecutionState::Reconciled
        );
        assert!(super::super::ai_mutation_admission::require_clear(
            &f.store.conn().unwrap(),
            &f.summary.chat_id.to_string()
        )
        .is_ok());
        assert!(
            !f.store
                .claim_ai_extension_execution(
                    f.summary.id,
                    f.actor,
                    f.summary.fingerprint.clone(),
                    claim_input(&f)
                )
                .await
                .unwrap()
                .execution
                .claimed
        );
        assert_eq!(counts(&f), (1, 1));
        let before = f
            .store
            .ai_content_handles_offline(Some(crate::TenantId(1)))
            .unwrap();
        assert!(!before.is_empty());
        f.store
            .conn()
            .unwrap()
            .execute(
                "DELETE FROM ai_chat WHERE id=?1",
                [f.summary.chat_id.to_string()],
            )
            .unwrap();
        let cleaned = f.store.process_ai_content_cleanup(100).await.unwrap();
        assert!(cleaned.0 > 0 && cleaned.1 == 0);
        assert!(f
            .store
            .ai_content_handles_offline(Some(crate::TenantId(1)))
            .unwrap()
            .is_empty());
        assert_eq!(
            f.store
                .extension_execution(id, f.actor)
                .unwrap()
                .receipt
                .state,
            ExtensionExecutionState::Acknowledged
        );
    }
    #[tokio::test]
    async fn extension_missing_restored_journal_is_unknown_and_cannot_be_reclaimed() {
        let f = fixture().await;
        let claim = f
            .store
            .claim_ai_extension_execution(
                f.summary.id,
                f.actor,
                f.summary.fingerprint.clone(),
                claim_input(&f),
            )
            .await
            .unwrap();
        let id = claim.execution.receipt.id;
        f.store
            .settle_extension_execution(
                id,
                f.actor,
                crate::ExtensionExecutionSettlement {
                    state: ExtensionExecutionState::Refused,
                    frame_started: Some(false),
                    response_kind: None,
                    late_response: false,
                    result_code: ExtensionExecutionCode::AdmissionRefused,
                },
            )
            .unwrap();
        f.store
            .conn()
            .unwrap()
            .execute(
                "DELETE FROM extension_execution WHERE id=?1",
                [id.to_string()],
            )
            .unwrap();
        let detail = f
            .store
            .ai_extension_execution_detail(f.summary.id, f.actor)
            .await
            .unwrap();
        assert_eq!(detail.summary.state, AiExecutionState::OutcomeUnknown);
        assert!(detail.summary.receipt.is_none());
        assert!(f
            .store
            .claim_ai_extension_execution(
                f.summary.id,
                f.actor,
                f.summary.fingerprint.clone(),
                claim_input(&f)
            )
            .await
            .is_err());
        assert_eq!(counts(&f), (1, 0));
        assert_eq!(
            f.store
                .acknowledge_ai_extension_execution(
                    f.summary.id,
                    f.actor,
                    AcknowledgeAiExtensionExecution {
                        fingerprint: f.summary.fingerprint.clone(),
                        execution_id: id
                    }
                )
                .await
                .unwrap()
                .state,
            AiExecutionState::Reconciled
        );
    }
    #[tokio::test]
    async fn durable_stop_refuses_ai_dispatch_intent_even_before_token_delivery() {
        let f = fixture().await;
        let claim = f
            .store
            .claim_ai_extension_execution(
                f.summary.id,
                f.actor,
                f.summary.fingerprint.clone(),
                claim_input(&f),
            )
            .await
            .unwrap();
        let id = claim.execution.receipt.id;
        assert!(
            f.store.mark_extension_dispatching(id, f.actor).is_err(),
            "ordinary dispatch must not bypass an AI review"
        );
        f.store
            .finish_ai_run(
                f.lease.run.id,
                f.actor,
                f.lease.lease_token,
                AiRunStatus::Canceled,
            )
            .await
            .unwrap();
        assert!(f
            .store
            .mark_ai_extension_dispatching_guarded(id, f.actor, None, None, None, f.summary.id)
            .is_err());
        assert_eq!(
            f.store
                .extension_execution(id, f.actor)
                .unwrap()
                .receipt
                .state,
            ExtensionExecutionState::Claimed
        );
        f.store
            .settle_extension_execution(
                id,
                f.actor,
                crate::ExtensionExecutionSettlement {
                    state: ExtensionExecutionState::Refused,
                    frame_started: Some(false),
                    response_kind: None,
                    late_response: false,
                    result_code: ExtensionExecutionCode::OwnerInterrupted,
                },
            )
            .unwrap();
        assert_eq!(
            f.store
                .ai_extension_execution(f.summary.id, f.actor)
                .await
                .unwrap()
                .state,
            AiExecutionState::Denied
        );
        assert!(
            super::super::ai_mutation_admission::require_clear(
                &f.store.conn().unwrap(),
                &f.summary.chat_id.to_string()
            )
            .is_ok(),
            "a known no-frame Stop refusal must release the gate without unknown acknowledgement"
        );
        assert!(f
            .store
            .acknowledge_ai_extension_execution(
                f.summary.id,
                f.actor,
                AcknowledgeAiExtensionExecution {
                    fingerprint: f.summary.fingerprint.clone(),
                    execution_id: id
                }
            )
            .await
            .is_err());
    }
    #[tokio::test]
    async fn extension_snapshot_and_tenant_recovery_preserve_uncertainty_without_authority() {
        let source = fixture().await;
        let source_claim = source
            .store
            .claim_ai_extension_execution(
                source.summary.id,
                source.actor,
                source.summary.fingerprint.clone(),
                claim_input(&source),
            )
            .await
            .unwrap();
        let root = tempfile::tempdir().unwrap();
        let snapshot = root.path().join("historical.sqlite");
        source
            .store
            .conn()
            .unwrap()
            .backup(rusqlite::DatabaseName::Main, &snapshot, None)
            .unwrap();
        let mut restored = MetadataStore::open(&snapshot, source.store.secrets.clone()).unwrap();
        // The recovery coordinator has staged the exact encrypted bodies and keys.
        restored.ai_content = source.store.ai_content.clone();
        restored.sanitize_workspace_backup_snapshot().unwrap();
        restored.sanitize_restored_database().unwrap();
        let historical = restored
            .ai_extension_execution_detail(source.summary.id, source.actor)
            .await
            .unwrap();
        assert_eq!(historical.review.arguments, source.request.arguments);
        assert_eq!(
            historical.summary.state,
            AiExecutionState::OutcomeUnknown,
            "a historical unissued claim may have executed after snapshot capture"
        );
        assert_eq!(historical.summary.receipt.unwrap().frame_started, None);
        assert!(
            !restored
                .claim_ai_extension_execution(
                    source.summary.id,
                    source.actor,
                    source.summary.fingerprint.clone(),
                    claim_input(&source)
                )
                .await
                .unwrap()
                .execution
                .claimed
        );
        assert!(restored
            .conn()
            .unwrap()
            .query_row("SELECT count(*)=0 FROM operation_approval", [], |row| row
                .get::<_, bool>(
                0
            ))
            .unwrap());

        let target = fixture().await;
        let target_claim = target
            .store
            .claim_ai_extension_execution(
                target.summary.id,
                target.actor,
                target.summary.fingerprint.clone(),
                claim_input(&target),
            )
            .await
            .unwrap();
        target
            .store
            .mark_ai_extension_dispatching_guarded(
                target_claim.execution.receipt.id,
                target.actor,
                None,
                None,
                None,
                target.summary.id,
            )
            .unwrap();
        let incoming = root.path().join("source.sqlite");
        let destination = root.path().join("target.sqlite");
        source
            .store
            .conn()
            .unwrap()
            .backup(rusqlite::DatabaseName::Main, &incoming, None)
            .unwrap();
        target
            .store
            .conn()
            .unwrap()
            .backup(rusqlite::DatabaseName::Main, &destination, None)
            .unwrap();
        crate::tenant_restore::merge_tenant_snapshot_with_ai_content(
            &destination,
            &incoming,
            crate::TenantId(1),
        )
        .unwrap();
        let mut merged = MetadataStore::open(&destination, source.store.secrets.clone()).unwrap();
        merged.ai_content = source.store.ai_content.clone();
        let imported = merged
            .ai_extension_execution_detail(source.summary.id, source.actor)
            .await
            .unwrap();
        assert_eq!(imported.review.arguments, source.request.arguments);
        assert_eq!(imported.summary.state, AiExecutionState::OutcomeUnknown);
        assert!(
            imported.summary.receipt.is_none(),
            "source journal authority must not be imported"
        );
        assert!(merged
            .extension_execution(source_claim.execution.receipt.id, source.actor)
            .is_err());
        let retained = merged
            .extension_execution(target_claim.execution.receipt.id, target.actor)
            .unwrap()
            .receipt;
        assert_eq!(retained.state, ExtensionExecutionState::OutcomeUnknown);
        assert_eq!(
            retained.result_code,
            Some(ExtensionExecutionCode::OwnerInterrupted)
        );
        assert!(merged
            .claim_ai_extension_execution(
                source.summary.id,
                source.actor,
                source.summary.fingerprint.clone(),
                claim_input(&source)
            )
            .await
            .is_err());
        let ack = merged
            .acknowledge_ai_extension_execution(
                source.summary.id,
                source.actor,
                AcknowledgeAiExtensionExecution {
                    fingerprint: source.summary.fingerprint,
                    execution_id: source_claim.execution.receipt.id,
                },
            )
            .await
            .unwrap();
        assert_eq!(ack.state, AiExecutionState::Reconciled);
        assert!(
            merged
                .extension_execution(source_claim.execution.receipt.id, source.actor)
                .is_err(),
            "acknowledgement cannot recreate missing journal evidence"
        );
    }
    #[tokio::test]
    async fn mismatching_journal_acknowledgement_releases_only_its_original_ai_gate() {
        let f = fixture().await;
        let mut unrelated = claim_input(&f);
        unrelated.request_id = Uuid::new_v4();
        let other = f.store.claim_extension_execution(&unrelated).unwrap();
        f.store.conn().unwrap().execute("UPDATE ai_extension_execution SET review_state='issued',execution_id=?2,approved_by=?3,approved_at=?4 WHERE id=?1",params![f.summary.id.to_string(),other.receipt.id.to_string(),f.actor.0,super::super::now_text()]).unwrap();
        let unknown = f
            .store
            .ai_extension_execution(f.summary.id, f.actor)
            .await
            .unwrap();
        assert_eq!(unknown.state, AiExecutionState::OutcomeUnknown);
        assert!(unknown.receipt.is_none());
        assert!(super::super::ai_mutation_admission::require_clear(
            &f.store.conn().unwrap(),
            &f.summary.chat_id.to_string()
        )
        .is_err());
        let acknowledged = f
            .store
            .acknowledge_ai_extension_execution(
                f.summary.id,
                f.actor,
                AcknowledgeAiExtensionExecution {
                    fingerprint: f.summary.fingerprint,
                    execution_id: other.receipt.id,
                },
            )
            .await
            .unwrap();
        assert_eq!(acknowledged.state, AiExecutionState::Reconciled);
        assert!(super::super::ai_mutation_admission::require_clear(
            &f.store.conn().unwrap(),
            &f.summary.chat_id.to_string()
        )
        .is_ok());
        assert_eq!(
            f.store
                .extension_execution(other.receipt.id, f.actor)
                .unwrap()
                .receipt
                .state,
            ExtensionExecutionState::Claimed,
            "acknowledgement cannot change another journal owner"
        );
        f.store
            .conn()
            .unwrap()
            .execute(
                "DELETE FROM ai_chat WHERE id=?1",
                [f.summary.chat_id.to_string()],
            )
            .unwrap();
        assert_eq!(
            f.store
                .extension_execution(other.receipt.id, f.actor)
                .unwrap()
                .receipt
                .state,
            ExtensionExecutionState::Claimed
        );
    }
    #[test]
    fn extension_fingerprints_are_canonical_under_json_key_order_features() {
        let first: serde_json::Value =
            serde_json::from_str(r#"{"z":[{"b":2,"a":1}],"a":false}"#).unwrap();
        let reordered: serde_json::Value =
            serde_json::from_str(r#"{"a":false,"z":[{"a":1,"b":2}]}"#).unwrap();
        let expected = format!(
            "{:x}",
            Sha256::digest(br#"{"a":false,"z":[{"a":1,"b":2}]}"#)
        );
        assert_eq!(hash(&first).unwrap(),expected,"approval bindings must match the server's canonical JSON regardless of Cargo feature unification");
        assert_eq!(hash(&reordered).unwrap(), expected);
        let changed: serde_json::Value =
            serde_json::from_str(r#"{"z":[{"b":1,"a":2}],"a":false}"#).unwrap();
        assert_ne!(hash(&changed).unwrap(), expected);
    }
}
