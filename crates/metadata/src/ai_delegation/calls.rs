//! Immutable call reservations. Request/response limits are reserved before work.
use super::*;
use chrono::DateTime;
use sift_core::ai_delegation::{reserve_tool_call, DelegationRuleUsage, DelegationUsage};

/// Trusted normalized broker request, never a model-selected grant or byte count.
pub struct NewAiDelegationCall {
    pub run_id: Uuid,
    pub actor: PrincipalId,
    pub lease_token: Uuid,
    pub call_id: Uuid,
    pub operation: String,
    pub input: serde_json::Value,
    pub reserved_output_bytes: u64,
}
#[derive(Debug, Clone)]
pub struct AiDelegationCallReservation {
    pub replayed: bool,
    pub expires_at: DateTime<Utc>,
    pub output_bytes: u64,
}

pub(super) struct LoadedGrant {
    pub run: Uuid,
    pub actor: PrincipalId,
    pub prepared: PreparedAiDelegationReview,
}
pub(super) fn call_fingerprint(operation: &str, input: &serde_json::Value) -> Result<String> {
    fingerprint(&(operation, input))
}
pub(super) async fn load(
    store: &MetadataStore,
    run: Uuid,
    actor: PrincipalId,
    instance: AiDelegationAdmissionPolicy,
) -> Result<LoadedGrant> {
    let cloned = store.clone();
    let id=sqlite_blocking(move || {
        let conn=cloned.conn()?;
        let id:String=conn.query_row("SELECT g.review_id FROM ai_run_delegation g JOIN ai_run r ON r.id=g.run_id WHERE g.run_id=?1 AND r.initiator_principal_id=?2",params![run.to_string(),actor.0],|r|r.get(0)).optional()?.ok_or_else(||invalid("Run has no reviewed delegation"))?;
        uuid(id)
    }).await?;
    let prepared = store
        .prepare_ai_delegation_review(id, actor, instance)
        .await?;
    if prepared.detail.summary.used_run_id != Some(run) {
        return Err(invalid("Delegation belongs to another run"));
    }
    Ok(LoadedGrant {
        run,
        actor,
        prepared,
    })
}

pub(super) fn require_live(
    conn: &rusqlite::Connection,
    grant: &LoadedGrant,
) -> Result<DateTime<Utc>> {
    let detail = &grant.prepared.detail;
    let row = stored(conn, detail.summary.id, grant.actor)?;
    require_policy(
        &detail.content,
        &grant.prepared.instance,
        tenant_policy(conn, TenantId(row.summary.tenant_id))?,
    )?;
    let expires:Option<String>=conn.query_row("SELECT g.expires_at FROM ai_run_delegation g JOIN ai_run r ON r.id=g.run_id JOIN ai_chat c ON c.id=r.chat_id WHERE g.run_id=?1 AND g.review_id=?2 AND g.state='active' AND r.status='running' AND r.execute_mode=1 AND r.initiator_principal_id=?3 AND c.visibility='private'",params![grant.run.to_string(),row.summary.id.to_string(),grant.actor.0],|r|r.get(0)).optional()?;
    let expires = super::super::parse_time_sql(
        expires.ok_or_else(|| invalid("Delegated run is no longer active"))?,
    )?;
    if expires <= Utc::now()
        || row.summary.state != AiDelegationReviewState::Consumed
        || row.summary.used_run_id != Some(grant.run)
        || row.summary.fingerprint != detail.summary.fingerprint
    {
        return Err(invalid("Delegation expired or changed"));
    }
    Ok(expires)
}
pub(super) fn usage(conn: &rusqlite::Connection, run: Uuid) -> Result<DelegationUsage> {
    let mut usage = conn.query_row(
        "SELECT calls,input_bytes,output_bytes FROM ai_run_delegation WHERE run_id=?1",
        [run.to_string()],
        |r| {
            Ok(DelegationUsage {
                tool_calls: r.get(0)?,
                input_bytes: r.get(1)?,
                output_bytes: r.get(2)?,
                ..Default::default()
            })
        },
    )?;
    let rows=conn.prepare("SELECT rule_id,uses,submitted_rows,schema_changes FROM ai_delegation_rule_usage WHERE run_id=?1")?.query_map([run.to_string()],|r|Ok((r.get::<_,String>(0)?,DelegationRuleUsage {uses:r.get(1)?,submitted_rows:r.get(2)?,schema_changes:r.get(3)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, counters) in rows {
        usage.rules.insert(uuid(id)?, counters);
    }
    Ok(usage)
}
fn prior(
    conn: &rusqlite::Connection,
    request: &NewAiDelegationCall,
    hash: &str,
    input_bytes: u64,
) -> Result<Option<AiDelegationCallReservation>> {
    let row=conn.query_row("SELECT c.operation,c.request_sha256,c.input_bytes,c.reserved_output_bytes,g.expires_at FROM ai_delegation_call c JOIN ai_run_delegation g ON g.run_id=c.run_id WHERE c.run_id=?1 AND c.call_id=?2",params![request.run_id.to_string(),request.call_id.to_string()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,u64>(2)?,r.get::<_,u64>(3)?,r.get::<_,String>(4)?))).optional()?;
    row.map(|(operation, sha, bytes, output, expires)| {
        if operation != request.operation
            || sha != hash
            || bytes != input_bytes
            || output != request.reserved_output_bytes
        {
            return Err(invalid(
                "Delegated call identity changed its immutable request or limits",
            ));
        }
        Ok(AiDelegationCallReservation {
            replayed: true,
            expires_at: super::super::parse_time_sql(expires)?,
            output_bytes: output,
        })
    })
    .transpose()
}
impl MetadataStore {
    pub async fn reserve_ai_delegation_call(
        &self,
        request: NewAiDelegationCall,
        instance: AiDelegationAdmissionPolicy,
    ) -> Result<AiDelegationCallReservation> {
        if request.call_id.is_nil()
            || request.operation.is_empty()
            || request.operation.len() > 512
            || !(1..=1048576).contains(&request.reserved_output_bytes)
        {
            return Err(invalid("Delegated call is invalid or exceeds its bound"));
        }
        let bytes = serde_json::to_vec(&(&request.operation, &request.input))?;
        if bytes.len() > 1048576 {
            return Err(invalid("Delegated call input exceeds its bound"));
        }
        let input_bytes = bytes.len() as u64;
        let sha = call_fingerprint(&request.operation, &request.input)?;
        let store = self.clone();
        let replay_request = NewAiDelegationCall {
            run_id: request.run_id,
            actor: request.actor,
            lease_token: request.lease_token,
            call_id: request.call_id,
            operation: request.operation.clone(),
            input: serde_json::Value::Null,
            reserved_output_bytes: request.reserved_output_bytes,
        };
        let replay_sha = sha.clone();
        let replay = sqlite_blocking(move || {
            let conn = store.conn()?;
            super::super::ai_run::ai_run_authorized_conn(
                &conn,
                replay_request.run_id,
                replay_request.actor,
                replay_request.lease_token,
            )?;
            prior(&conn, &replay_request, &replay_sha, input_bytes)
        })
        .await?;
        if let Some(replay) = replay {
            return Ok(replay);
        }
        let grant = load(self, request.run_id, request.actor, instance).await?;
        let store = self.clone();
        sqlite_blocking(move || {
            let mut conn=store.conn()?;let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (_,status)=super::super::ai_run::ai_run_authorized_conn(&tx,request.run_id,request.actor,request.lease_token)?;
            if let Some(replay)=prior(&tx,&request,&sha,input_bytes)? {return Ok(replay);}
            if status!="running" {return Err(invalid("Delegated call cannot admit work after Stop"));}
            let expires=require_live(&tx,&grant)?;
            let after=reserve_tool_call(&grant.prepared.detail.content.policy,&usage(&tx,request.run_id)?,input_bytes,request.reserved_output_bytes).map_err(|_|invalid("Delegation call or byte budget exhausted"))?;
            tx.execute("INSERT INTO ai_delegation_call(run_id,call_id,operation,request_sha256,input_bytes,reserved_output_bytes,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![request.run_id.to_string(),request.call_id.to_string(),request.operation,sha,input_bytes,request.reserved_output_bytes,super::super::now_text()])?;
            tx.execute("UPDATE ai_run_delegation SET calls=?2,input_bytes=?3,output_bytes=?4 WHERE run_id=?1",params![request.run_id.to_string(),after.tool_calls,after.input_bytes,after.output_bytes])?;
            tx.commit()?;
            Ok(AiDelegationCallReservation {replayed:false,expires_at:expires,output_bytes:request.reserved_output_bytes})
        }).await
    }
}
