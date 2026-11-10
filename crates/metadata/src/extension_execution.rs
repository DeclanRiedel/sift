//! One-use extension dispatch identities and terminal RPC observations, never payloads.
use super::{
    ApprovalBinding, MetadataError, MetadataStore, NewOperationAudit, PrincipalId, Result,
};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use sift_protocol::{
    ExtensionExecutionCode as Code, ExtensionExecutionReceipt, ExtensionExecutionState as State,
    ExtensionOperation, ExtensionResponseKind, ToolContext,
};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct NewExtensionExecution {
    pub request_id: Uuid,
    pub binding: ApprovalBinding,
    pub approval_id: Option<String>,
    pub operation: ExtensionOperation,
    pub context: ToolContext,
    pub deadline_unix_ms: i64,
    pub correlation_id: String,
}

#[derive(Debug, Clone)]
pub struct ExtensionExecutionRecord {
    pub receipt: ExtensionExecutionReceipt,
    pub context: ToolContext,
    pub context_fingerprint: String,
    pub input_fingerprint: String,
}

#[derive(Debug)]
pub struct ExtensionExecutionClaim {
    pub receipt: ExtensionExecutionReceipt,
    pub claimed: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct ExtensionExecutionSettlement {
    pub state: State,
    pub frame_started: Option<bool>,
    pub response_kind: Option<ExtensionResponseKind>,
    pub late_response: bool,
    pub result_code: Code,
}

fn invalid() -> MetadataError {
    MetadataError::InvalidExtensionExecution
}
fn tag<T: serde::Serialize>(value: T) -> Result<String> {
    serde_json::to_value(value)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(invalid)
}
fn parse<T: serde::de::DeserializeOwned>(value: String) -> Result<T> {
    Ok(serde_json::from_value(serde_json::Value::String(value))?)
}
fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn access(conn: &Connection, actor: PrincipalId, tenant: Option<i64>) -> Result<()> {
    let allowed: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM principal p WHERE p.id=?1 AND p.disabled_at IS NULL
         AND (?2 IS NULL OR EXISTS(SELECT 1 FROM tenant t WHERE t.id=?2 AND
          (p.is_instance_admin=1 OR EXISTS(SELECT 1 FROM membership m WHERE m.tenant_id=t.id AND m.principal_id=p.id)))))",
        params![actor.0,tenant], |r|r.get(0))?;
    if allowed {
        Ok(())
    } else {
        Err(MetadataError::ExtensionExecutionUnavailable)
    }
}

#[derive(Debug, Clone)]
pub enum ExtensionCredentialProof {
    ApiToken(i64, String),
    AccessToken(i64, String),
}

fn credential_live(
    conn: &Connection,
    actor: PrincipalId,
    context: &ToolContext,
    proof: &ExtensionCredentialProof,
) -> Result<()> {
    let now = super::now_text();
    let valid:bool=match proof {
        ExtensionCredentialProof::ApiToken(id,created)=>conn.query_row("SELECT EXISTS(SELECT 1 FROM api_token WHERE id=?1 AND principal_id=?2 AND created_at=?5
            AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at>?3) AND (tenant_id IS NULL OR tenant_id=?4))",
            params![id,actor.0,now,context.tenant_id,created],|r|r.get(0))?,
        ExtensionCredentialProof::AccessToken(id,created)=>conn.query_row("SELECT EXISTS(SELECT 1 FROM auth_access_token a JOIN auth_session s ON s.id=a.auth_session_id
            WHERE a.id=?1 AND s.principal_id=?2 AND a.created_at=?4 AND a.revoked_at IS NULL AND a.expires_at>?3 AND s.revoked_at IS NULL AND s.expires_at>?3)",
            params![id,actor.0,now,created],|r|r.get(0))?,
    };
    if valid {
        Ok(())
    } else {
        Err(MetadataError::ExtensionExecutionUnavailable)
    }
}

fn source_fingerprint_locked(
    conn: &Connection,
    actor: PrincipalId,
    context: &ToolContext,
) -> Result<String> {
    use sha2::{Digest, Sha256};
    let principal:Option<String>=conn.query_row("SELECT json_array(id,external_id,disabled_at,is_instance_admin,created_at) FROM principal WHERE id=?1",
        [actor.0],|r|r.get(0)).optional()?;
    let tenant:Option<String>=conn.query_row("SELECT json_array(t.id,t.name,t.kind,t.created_at,t.updated_at,m.role,m.created_at,m.updated_at)
        FROM tenant t LEFT JOIN membership m ON m.tenant_id=t.id AND m.principal_id=?1 WHERE t.id=?2",
        params![actor.0,context.tenant_id],|r|r.get(0)).optional()?;
    let profile:Option<String>=conn.query_row("SELECT json_array(id,tenant_id,name,provider_id,configuration_json,credential_mode,shared_secret_handle,policy_json,policy_revision,created_by,created_at,updated_at)
        FROM connection_profile WHERE id=?1",[context.profile_id],|r|r.get(0)).optional()?;
    let credential:Option<String>=conn.query_row("SELECT json_array(secret_handle,created_at,updated_at) FROM connection_credential WHERE connection_profile_id=?1 AND principal_id=?2",
        params![context.profile_id,actor.0],|r|r.get(0)).optional()?;
    let room:Option<String>=conn.query_row("SELECT json_array(r.id,r.tenant_id,r.kind,r.created_at,r.updated_at,r.bound_connection_profile_id,r.bound_connection_by,m.role,m.joined_at)
        FROM room r LEFT JOIN room_member m ON m.room_id=r.id AND m.principal_id=?1 WHERE r.id=?2",
        params![actor.0,context.room_id],|r|r.get(0)).optional()?;
    let document:Option<String>=conn.query_row("SELECT json_array(id,room_id,kind,connection_profile_id,created_at) FROM document WHERE id=?1",
        [context.document_id.as_deref()],|r|r.get(0)).optional()?;
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(
            context, principal, tenant, profile, credential, room, document
        ))?)
    ))
}

impl MetadataStore {
    /// Call only after cryptographic credential verification. Returns an opaque
    /// row reference so revocation can be checked again in the admission transaction.
    pub fn extension_verified_credential_reference(
        &self,
        actor: PrincipalId,
        presented: &str,
    ) -> Result<ExtensionCredentialProof> {
        let conn = self.conn()?;
        if let Some(lookup) = super::auth_token_lookup(presented, super::ACCESS_TOKEN_PREFIX) {
            let id:(i64,String)=conn.query_row("SELECT a.id,a.created_at FROM auth_access_token a JOIN auth_session s ON s.id=a.auth_session_id WHERE a.token_lookup=?1 AND s.principal_id=?2",
                params![lookup,actor.0],|r|Ok((r.get(0)?,r.get(1)?))).optional()?.ok_or(MetadataError::ExtensionExecutionUnavailable)?;
            Ok(ExtensionCredentialProof::AccessToken(id.0, id.1))
        } else {
            let lookup = super::token_lookup_from_presented(presented)
                .ok_or(MetadataError::ExtensionExecutionUnavailable)?;
            let id: (i64, String) = conn.query_row(
                "SELECT id,created_at FROM api_token WHERE token_lookup=?1 AND principal_id=?2",
                params![lookup, actor.0],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            Ok(ExtensionCredentialProof::ApiToken(id.0, id.1))
        }
    }

    pub fn extension_source_fingerprint(
        &self,
        actor: PrincipalId,
        context: &ToolContext,
    ) -> Result<String> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        access(&tx, actor, context.tenant_id)?;
        let fingerprint = source_fingerprint_locked(&tx, actor, context)?;
        tx.commit()?;
        Ok(fingerprint)
    }

    /// Caller validates current action/policy and bounded input before claiming.
    /// Approval consumption, one-use identity and compatibility floor are one commit.
    pub fn claim_extension_execution(
        &self,
        input: &NewExtensionExecution,
    ) -> Result<ExtensionExecutionClaim> {
        let mut operation = input.operation.clone();
        operation.sanitized_arguments.clear();
        let operation_json = serde_json::to_string(&operation)?;
        let context_json = serde_json::to_string(&input.context)?;
        if !valid_hash(&input.binding.context_fingerprint)
            || !valid_hash(&input.binding.input_fingerprint)
            || input.binding.operation_id.is_empty()
            || input.binding.operation_id.len() > 512
            || input.correlation_id.is_empty()
            || input.correlation_id.len() > 256
            || operation_json.len() > 8192
            || context_json.len() > 4096
        {
            return Err(invalid());
        }
        if sift_protocol::classification_requires_approval(operation.classification)
            && input.approval_id.is_none()
        {
            return Err(MetadataError::InvalidOperationApproval);
        }
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        access(&tx, input.binding.principal_id, input.context.tenant_id)?;
        {
            let prior=tx.prepare("SELECT id FROM extension_execution WHERE approval_id=?1 OR (actor_principal_id=?2 AND request_id=?3)")?
                .query_map(params![input.approval_id,input.binding.principal_id.0,input.request_id.to_string()],|r|r.get::<_,String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if prior.len() > 1 {
                return Err(invalid());
            }
            let prior = prior.into_iter().next();
            if let Some(id) = prior {
                let id = Uuid::parse_str(&id).map_err(|_| invalid())?;
                let prior = record(&tx, id)?;
                let (same_operation, same_approval): (String, Option<String>) = tx.query_row(
                    "SELECT operation_json,approval_id FROM extension_execution WHERE id=?1",
                    [id.to_string()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                if prior.receipt.principal_id != input.binding.principal_id.0
                    || prior.receipt.operation_id != input.binding.operation_id
                    || prior.context_fingerprint != input.binding.context_fingerprint
                    || prior.input_fingerprint != input.binding.input_fingerprint
                    || prior.context != input.context
                    || same_operation != operation_json
                    || input
                        .approval_id
                        .as_ref()
                        .is_some_and(|id| Some(id) != same_approval.as_ref())
                {
                    return Err(invalid());
                }
                tx.commit()?;
                return Ok(ExtensionExecutionClaim {
                    receipt: prior.receipt,
                    claimed: false,
                });
            }
        }
        let now = Utc::now().timestamp_millis();
        if input.deadline_unix_ms <= now || input.deadline_unix_ms > now.saturating_add(300_000) {
            return Err(invalid());
        }
        if let Some(approval) = input.approval_id.as_deref() {
            super::approval::consume_operation_approval_locked(&tx, approval, &input.binding)?;
        }
        let id = Uuid::new_v4();
        tx.execute("INSERT INTO extension_execution(id,actor_principal_id,tenant_id,approval_id,operation_id,
            operation_json,context_json,context_fingerprint,input_fingerprint,correlation_id,state,created_at,deadline_unix_ms,request_id)
            VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'claimed',?11,?12,?13)",
            params![id.to_string(),input.binding.principal_id.0,input.context.tenant_id,input.approval_id,
                input.binding.operation_id,operation_json,context_json,input.binding.context_fingerprint,
                input.binding.input_fingerprint,input.correlation_id,super::now_text(),input.deadline_unix_ms,input.request_id.to_string()])?;
        let floor: u32 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if floor < 63 {
            tx.pragma_update(None, "user_version", 63)?;
        }
        let receipt = record(&tx, id)?.receipt;
        tx.commit()?;
        Ok(ExtensionExecutionClaim {
            receipt,
            claimed: true,
        })
    }

    /// Durable intent must commit before the caller may enter a frame writer.
    pub fn mark_extension_dispatching(&self, id: Uuid, actor: PrincipalId) -> Result<()> {
        self.mark_extension_dispatching_guarded(id, actor, None, None, None)
    }

    /// Compare metadata authority and source/package identity atomically with intent.
    pub fn mark_extension_dispatching_guarded(
        &self,
        id: Uuid,
        actor: PrincipalId,
        expected_source: Option<&str>,
        expected_package: Option<&serde_json::Value>,
        credential: Option<&ExtensionCredentialProof>,
    ) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row = record(&tx, id)?;
        if row.receipt.principal_id != actor.0 {
            return Err(MetadataError::ExtensionExecutionUnavailable);
        }
        access(&tx, actor, row.context.tenant_id)?;
        if let Some(proof) = credential {
            credential_live(&tx, actor, &row.context, proof)?;
        }
        if let Some(expected) = expected_source {
            if source_fingerprint_locked(&tx, actor, &row.context)? != expected {
                return Err(invalid());
            }
        }
        if let Some(expected) = expected_package {
            let selected:Option<(String,String,u64)>=tx.query_row("SELECT s.selected_archive_sha256,p.manifest_sha256,s.revision
                FROM extension_selection s JOIN extension_package p ON p.archive_sha256=s.selected_archive_sha256
                WHERE s.extension_id=?1 AND s.enabled=1 AND s.quarantine_reason IS NULL
                AND (?2 IS NULL OR EXISTS(SELECT 1 FROM extension_tenant_allowlist a WHERE a.extension_id=s.extension_id AND a.tenant_id=?2 AND a.allowed=1))",
                params![row.receipt.extension_id.as_str(),row.context.tenant_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            let Some((archive, manifest, revision)) = selected else {
                return Err(invalid());
            };
            if *expected
                != serde_json::json!({"archive_sha256":archive,"manifest_sha256":manifest,"selection_revision":revision})
            {
                return Err(invalid());
            }
        }
        let changed=tx.execute("UPDATE extension_execution SET state='dispatching' WHERE id=?1 AND state='claimed' AND deadline_unix_ms>?2",
            params![id.to_string(),Utc::now().timestamp_millis()])?;
        if changed != 1 {
            return Err(invalid());
        }
        tx.commit()?;
        Ok(())
    }

    /// Settlement is allowed after source/actor revocation; only delivery is gated.
    pub fn settle_extension_execution(
        &self,
        id: Uuid,
        actor: PrincipalId,
        observation: ExtensionExecutionSettlement,
    ) -> Result<ExtensionExecutionReceipt> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row = record(&tx, id)?;
        if row.receipt.principal_id != actor.0 {
            return Err(MetadataError::ExtensionExecutionUnavailable);
        }
        let receipt = settle(&tx, id, observation)?;
        tx.commit()?;
        Ok(receipt)
    }

    pub fn extension_execution(
        &self,
        id: Uuid,
        actor: PrincipalId,
    ) -> Result<ExtensionExecutionRecord> {
        let conn = self.conn()?;
        let row = record(&conn, id)?;
        if row.receipt.principal_id != actor.0 {
            return Err(MetadataError::ExtensionExecutionUnavailable);
        }
        access(&conn, actor, row.context.tenant_id)?;
        Ok(row)
    }

    pub fn extension_execution_replay(
        &self,
        request: Uuid,
        binding: &ApprovalBinding,
        context: &ToolContext,
        approval: Option<&str>,
    ) -> Result<Option<ExtensionExecutionRecord>> {
        let row = self.extension_execution_for_request(request, binding.principal_id)?;
        if let Some(row) = &row {
            if let Some(approval) = approval {
                let original: Option<String> = self.conn()?.query_row(
                    "SELECT approval_id FROM extension_execution WHERE id=?1",
                    [row.receipt.id.to_string()],
                    |r| r.get(0),
                )?;
                if original.as_deref() != Some(approval) {
                    return Err(invalid());
                }
            }
            if row.receipt.operation_id != binding.operation_id
                || row.context_fingerprint != binding.context_fingerprint
                || row.input_fingerprint != binding.input_fingerprint
                || row.context != *context
            {
                return Err(invalid());
            }
        }
        Ok(row)
    }

    pub fn extension_execution_for_request(
        &self,
        request: Uuid,
        actor: PrincipalId,
    ) -> Result<Option<ExtensionExecutionRecord>> {
        let conn = self.conn()?;
        let id: Option<String> = conn
            .query_row(
                "SELECT id FROM extension_execution WHERE request_id=?1 AND actor_principal_id=?2",
                params![request.to_string(), actor.0],
                |r| r.get(0),
            )
            .optional()?;
        let Some(id) = id else { return Ok(None) };
        let row = record(&conn, Uuid::parse_str(&id).map_err(|_| invalid())?)?;
        access(&conn, actor, row.context.tenant_id)?;
        Ok(Some(row))
    }

    pub fn extension_execution_for_approval(
        &self,
        approval: &str,
        actor: PrincipalId,
    ) -> Result<Option<ExtensionExecutionRecord>> {
        let conn = self.conn()?;
        let id: Option<String> = conn
            .query_row(
                "SELECT id FROM extension_execution WHERE approval_id=?1 AND actor_principal_id=?2",
                params![approval, actor.0],
                |r| r.get(0),
            )
            .optional()?;
        let Some(id) = id else {
            return Ok(None);
        };
        let row = record(&conn, Uuid::parse_str(&id).map_err(|_| invalid())?)?;
        access(&conn, actor, row.context.tenant_id)?;
        Ok(Some(row))
    }

    pub fn extension_executions(
        &self,
        actor: PrincipalId,
    ) -> Result<Vec<ExtensionExecutionRecord>> {
        let conn = self.conn()?;
        access(&conn, actor, None)?;
        let mut statement=conn.prepare("SELECT e.id FROM extension_execution e JOIN principal p ON p.id=e.actor_principal_id
            WHERE e.actor_principal_id=?1 AND (e.tenant_id IS NULL OR EXISTS(SELECT 1 FROM tenant t WHERE t.id=e.tenant_id
            AND (p.is_instance_admin=1 OR EXISTS(SELECT 1 FROM membership m WHERE m.tenant_id=t.id AND m.principal_id=p.id))))
            ORDER BY e.created_at DESC,e.id DESC LIMIT 200")?;
        let ids = statement
            .query_map([actor.0], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter()
            .map(|id| record(&conn, Uuid::parse_str(&id).map_err(|_| invalid())?))
            .collect()
    }

    /// Startup only, under the serving/migration lease; never on ordinary store open.
    pub fn interrupt_extension_executions(&self) -> Result<usize> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = recover_extension_executions_locked(&tx, None, false)?;
        tx.commit()?;
        Ok(changed)
    }
}

pub(crate) fn recover_extension_executions_locked(
    conn: &Connection,
    tenant: Option<i64>,
    historical_snapshot: bool,
) -> Result<usize> {
    let mut statement = conn.prepare(
        "SELECT id FROM extension_execution WHERE state IN ('claimed','dispatching')
        AND (?1 IS NULL OR tenant_id=?1) ORDER BY created_at,id",
    )?;
    let ids = statement
        .query_map([tenant], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for id in &ids {
        let id = Uuid::parse_str(id).map_err(|_| invalid())?;
        let row = record(conn, id)?;
        let unissued = row.receipt.state == State::Claimed && !historical_snapshot;
        settle(
            conn,
            id,
            ExtensionExecutionSettlement {
                state: if unissued {
                    State::Refused
                } else {
                    State::OutcomeUnknown
                },
                frame_started: if unissued { Some(false) } else { None },
                response_kind: None,
                late_response: false,
                result_code: Code::OwnerInterrupted,
            },
        )?;
    }
    Ok(ids.len())
}

fn settle(
    conn: &Connection,
    id: Uuid,
    observation: ExtensionExecutionSettlement,
) -> Result<ExtensionExecutionReceipt> {
    let row = record(conn, id)?;
    if !matches!(row.receipt.state, State::Claimed | State::Dispatching) {
        return Ok(row.receipt);
    }
    let valid = match observation.state {
        State::Acknowledged => {
            observation.frame_started == Some(true)
                && observation.response_kind.is_some()
                && row.receipt.state == State::Dispatching
        }
        State::Refused => {
            observation.frame_started == Some(false)
                && observation.response_kind.is_none()
                && !observation.late_response
        }
        State::OutcomeUnknown => {
            observation.frame_started != Some(false)
                && observation.response_kind.is_none()
                && (row.receipt.state == State::Dispatching
                    || (observation.result_code == Code::OwnerInterrupted
                        && observation.frame_started.is_none()))
        }
        _ => false,
    };
    if !valid {
        return Err(invalid());
    }
    let correlation: String = conn.query_row(
        "SELECT correlation_id FROM extension_execution WHERE id=?1",
        [id.to_string()],
        |r| r.get(0),
    )?;
    let succeeded = observation.state == State::Acknowledged
        && observation.response_kind == Some(ExtensionResponseKind::Result)
        && observation.result_code == Code::Acknowledged
        && !observation.late_response;
    let code = tag(observation.result_code)?;
    let audit = super::insert_operation_audit_row(
        conn,
        &NewOperationAudit {
            actor_principal_id: Some(PrincipalId(row.receipt.principal_id)),
            action: "invoke_extension".into(),
            target: "extension_execution".into(),
            target_id: None,
            status: if succeeded { "succeeded" } else { "failed" }.into(),
            result_code: Some(code.clone()),
            row_count: None,
            error_message: None,
            correlation_id: Some(correlation),
        },
    )?;
    conn.execute("UPDATE extension_execution SET state=?2,observed_at=?3,frame_started=?4,response_kind=?5,late_response=?6,result_code=?7,audit_id=?8 WHERE id=?1",
        params![id.to_string(),tag(observation.state)?,super::now_text(),observation.frame_started,
            observation.response_kind.map(tag).transpose()?,observation.late_response,code,audit])?;
    Ok(record(conn, id)?.receipt)
}

fn record(conn: &Connection, id: Uuid) -> Result<ExtensionExecutionRecord> {
    let row=conn.query_row("SELECT actor_principal_id,tenant_id,operation_id,operation_json,context_json,
        context_fingerprint,input_fingerprint,state,created_at,deadline_unix_ms,observed_at,frame_started,
        response_kind,late_response,result_code,audit_id,request_id FROM extension_execution WHERE id=?1",[id.to_string()],|r| {
        Ok((r.get::<_,i64>(0)?,r.get::<_,Option<i64>>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,
            r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?,r.get::<_,String>(7)?,
            r.get::<_,String>(8)?,r.get::<_,i64>(9)?,r.get::<_,Option<String>>(10)?,r.get::<_,Option<bool>>(11)?,
            r.get::<_,Option<String>>(12)?,r.get::<_,bool>(13)?,r.get::<_,Option<String>>(14)?,r.get::<_,Option<i64>>(15)?,r.get::<_,String>(16)?))
    }).optional()?.ok_or(MetadataError::ExtensionExecutionUnavailable)?;
    let operation: ExtensionOperation = serde_json::from_str(&row.3)?;
    Ok(ExtensionExecutionRecord {
        receipt: ExtensionExecutionReceipt {
            id,
            request_id: Uuid::parse_str(&row.16).map_err(|_| invalid())?,
            principal_id: row.0,
            tenant_id: row.1,
            operation_id: row.2,
            extension_id: operation.extension_id,
            contribution_id: operation.contribution_id,
            action: operation.action,
            classification: operation.classification,
            state: parse(row.7)?,
            created_at: row.8,
            deadline_unix_ms: row.9,
            observed_at: row.10,
            frame_started: row.11,
            response_kind: row.12.map(parse).transpose()?,
            late_response: row.13,
            result_code: row.14.map(parse).transpose()?,
            audit_id: row.15,
        },
        context: serde_json::from_str(&row.4)?,
        context_fingerprint: row.5,
        input_fingerprint: row.6,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use sift_protocol::{ContributionId, ExtensionId, OperationClassification, SegmentId};
    use std::sync::Arc;

    fn store() -> MetadataStore {
        let store =
            MetadataStore::open_in_memory(Arc::new(super::super::MemorySecretStore::default()))
                .unwrap();
        store.bootstrap_local("extension-journal").unwrap();
        store
    }
    pub(crate) fn input(store: &MetadataStore, approval: bool) -> NewExtensionExecution {
        let binding = ApprovalBinding {
            principal_id: PrincipalId(1),
            operation_id: "acme/fixture/command/action#run".into(),
            context_fingerprint: "a".repeat(64),
            input_fingerprint: "b".repeat(64),
        };
        let approval_id = approval.then(|| {
            let row = store.create_operation_approval(&binding, None).unwrap();
            store
                .approve_operation(&row.id, PrincipalId(1), row.revision)
                .unwrap()
                .id
        });
        NewExtensionExecution {
            request_id: Uuid::new_v4(),
            binding,
            approval_id,
            operation: ExtensionOperation {
                extension_id: ExtensionId::new("acme/fixture").unwrap(),
                contribution_id: ContributionId::new("acme/fixture/command/action").unwrap(),
                action: SegmentId::new("run").unwrap(),
                classification: if approval {
                    OperationClassification::Write
                } else {
                    OperationClassification::Read
                },
                target_kind: SegmentId::new("tenant").unwrap(),
                target_id: Some("1".into()),
                sanitized_arguments: std::collections::BTreeMap::from([(
                    "unsafe".into(),
                    serde_json::json!("never-persist-this-value"),
                )]),
            },
            context: ToolContext {
                tenant_id: Some(1),
                room_id: None,
                profile_id: None,
                connection_id: None,
                document_id: None,
            },
            deadline_unix_ms: Utc::now().timestamp_millis() + 60_000,
            correlation_id: "original-extension-correlation".into(),
        }
    }
    fn acknowledged() -> ExtensionExecutionSettlement {
        ExtensionExecutionSettlement {
            state: State::Acknowledged,
            frame_started: Some(true),
            response_kind: Some(ExtensionResponseKind::Result),
            late_response: false,
            result_code: Code::Acknowledged,
        }
    }
    fn dispatch(store: &MetadataStore, input: &NewExtensionExecution) -> Uuid {
        let row = store.claim_extension_execution(input).unwrap();
        assert!(row.claimed);
        store
            .mark_extension_dispatching(row.receipt.id, PrincipalId(1))
            .unwrap();
        row.receipt.id
    }

    #[test]
    fn extension_claim_and_approval_are_atomic_one_use_and_redacted() {
        let store = store();
        let input = input(&store, true);
        let floor = store.migration_status().unwrap().minimum_compatible_version;
        store.conn().unwrap().execute_batch("CREATE TRIGGER reject_extension_claim BEFORE INSERT ON extension_execution BEGIN SELECT RAISE(ABORT,'injected claim failure'); END;").unwrap();
        assert!(store.claim_extension_execution(&input).is_err());
        assert!(store.list_operation_approvals(PrincipalId(1)).unwrap()[0]
            .consumed_at
            .is_none());
        assert_eq!(
            store.migration_status().unwrap().minimum_compatible_version,
            floor
        );
        store
            .conn()
            .unwrap()
            .execute_batch("DROP TRIGGER reject_extension_claim")
            .unwrap();
        let gate = Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let store = store.clone();
                let input = input.clone();
                let gate = gate.clone();
                std::thread::spawn(move || {
                    gate.wait();
                    store.claim_extension_execution(&input).unwrap()
                })
            })
            .collect();
        let claims: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(claims[0].receipt.id, claims[1].receipt.id);
        assert_ne!(claims[0].claimed, claims[1].claimed);
        assert_eq!(
            store.migration_status().unwrap().minimum_compatible_version,
            63
        );
        assert!(store.list_operation_approvals(PrincipalId(1)).unwrap()[0]
            .consumed_at
            .is_some());
        let stored: String = store
            .conn()
            .unwrap()
            .query_row("SELECT operation_json FROM extension_execution", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(!stored.contains("never-persist-this-value"));
        let mut different_approval = input.clone();
        different_approval.approval_id = Some("another-approval".into());
        assert!(store
            .claim_extension_execution(&different_approval)
            .is_err());
        assert!(store
            .extension_execution_replay(
                input.request_id,
                &input.binding,
                &input.context,
                Some("another-approval")
            )
            .is_err());
        assert!(store
            .extension_execution_replay(input.request_id, &input.binding, &input.context, None)
            .unwrap()
            .is_some());
        assert!(store
            .extension_execution_replay(
                input.request_id,
                &input.binding,
                &input.context,
                input.approval_id.as_deref()
            )
            .unwrap()
            .is_some());
        let mut altered = input.clone();
        altered.binding.input_fingerprint = "c".repeat(64);
        assert!(store.claim_extension_execution(&altered).is_err());
        altered = input.clone();
        altered.binding.context_fingerprint = "c".repeat(64);
        assert!(store.claim_extension_execution(&altered).is_err());
        altered = input.clone();
        altered.context.room_id = Some(2);
        assert!(store.claim_extension_execution(&altered).is_err());
        let mut replay = input.clone();
        replay.deadline_unix_ms = Utc::now().timestamp_millis() + 120_000;
        let replay = store.claim_extension_execution(&replay).unwrap();
        assert!(!replay.claimed);
        assert_eq!(replay.receipt.deadline_unix_ms, input.deadline_unix_ms);
        assert!(store
            .extension_execution(replay.receipt.id, PrincipalId(2))
            .is_err());
        assert!(store
            .extension_execution_for_approval(input.approval_id.as_deref().unwrap(), PrincipalId(2))
            .unwrap()
            .is_none());
    }

    #[test]
    fn extension_admission_transaction_fences_intervening_permissions_and_credentials() {
        let store = store();
        let first = input(&store, false);
        let expected = store
            .extension_source_fingerprint(PrincipalId(1), &first.context)
            .unwrap();
        let id = store.claim_extension_execution(&first).unwrap().receipt.id;
        store
            .upsert_tenant_membership(
                super::super::TenantId(1),
                PrincipalId(1),
                super::super::MembershipRole::Viewer,
            )
            .unwrap();
        assert!(store
            .mark_extension_dispatching_guarded(id, PrincipalId(1), Some(&expected), None, None)
            .is_err());
        assert_eq!(
            store
                .extension_execution(id, PrincipalId(1))
                .unwrap()
                .receipt
                .state,
            State::Claimed
        );
        store
            .upsert_tenant_membership(
                super::super::TenantId(1),
                PrincipalId(1),
                super::super::MembershipRole::Owner,
            )
            .unwrap();
        let fresh = input(&store, false);
        let expected = store
            .extension_source_fingerprint(PrincipalId(1), &fresh.context)
            .unwrap();
        let id = store.claim_extension_execution(&fresh).unwrap().receipt.id;
        let (token_row, token) = store
            .issue_api_token(
                PrincipalId(1),
                Some(super::super::TenantId(1)),
                "admission-test",
                None,
            )
            .unwrap();
        assert!(store.verify_api_token(&token).unwrap().is_some());
        let proof = store
            .extension_verified_credential_reference(PrincipalId(1), &token)
            .unwrap();
        store
            .conn()
            .unwrap()
            .execute(
                "UPDATE api_token SET revoked_at=?2 WHERE id=?1",
                params![token_row.id.0, super::super::now_text()],
            )
            .unwrap();
        assert!(store
            .mark_extension_dispatching_guarded(
                id,
                PrincipalId(1),
                Some(&expected),
                None,
                Some(&proof)
            )
            .is_err());
        assert_eq!(
            store
                .extension_execution(id, PrincipalId(1))
                .unwrap()
                .receipt
                .state,
            State::Claimed
        );
        // A new owner's claim can use a freshly verified credential without
        // changing the reviewed source; routine last-used writes are excluded.
        let fresh = input(&store, false);
        let id = store.claim_extension_execution(&fresh).unwrap().receipt.id;
        let (_, token) = store
            .issue_api_token(
                PrincipalId(1),
                Some(super::super::TenantId(1)),
                "new-admission-test",
                None,
            )
            .unwrap();
        assert!(store.verify_api_token(&token).unwrap().is_some());
        let proof = store
            .extension_verified_credential_reference(PrincipalId(1), &token)
            .unwrap();
        store
            .mark_extension_dispatching_guarded(
                id,
                PrincipalId(1),
                Some(&expected),
                None,
                Some(&proof),
            )
            .unwrap();
        assert_eq!(
            store
                .extension_execution(id, PrincipalId(1))
                .unwrap()
                .receipt
                .state,
            State::Dispatching
        );
    }

    #[test]
    fn extension_admission_transaction_binds_package_revision_and_tenant_allowance() {
        use crate::{NewExtensionPackage, ReplaceExtensionGrants, UpdateExtensionSelection};
        use sift_protocol::{ExtensionIsolation, ExtensionLifecycleState, ExtensionProvenance};
        let store = store();
        store
            .record_extension_package(
                &NewExtensionPackage {
                    archive_sha256: "a".repeat(64),
                    extension_id: "acme/fixture".into(),
                    version: "1.0.0".into(),
                    manifest_sha256: "b".repeat(64),
                    manifest_json: "{}".into(),
                    provenance: ExtensionProvenance::Local,
                },
                &[],
            )
            .unwrap();
        store
            .update_extension_selection(UpdateExtensionSelection {
                extension_id: "acme/fixture",
                selected_archive_sha256: None,
                enabled: true,
                lifecycle: ExtensionLifecycleState::Ready,
                isolation: ExtensionIsolation::ProcessOnly,
                quarantine_reason: None,
                expected_revision: 0,
            })
            .unwrap();
        let revision = store
            .set_extension_tenant_allowed("acme/fixture", 1, true, 1)
            .unwrap();
        let expected = serde_json::json!({"archive_sha256":"a".repeat(64),"manifest_sha256":"b".repeat(64),"selection_revision":revision});
        let claim = store
            .claim_extension_execution(&input(&store, false))
            .unwrap();
        store
            .mark_extension_dispatching_guarded(
                claim.receipt.id,
                PrincipalId(1),
                None,
                Some(&expected),
                None,
            )
            .unwrap();
        let stale = store
            .claim_extension_execution(&input(&store, false))
            .unwrap();
        let revision = store
            .replace_extension_grants(&ReplaceExtensionGrants {
                extension_id: "acme/fixture".into(),
                grants: vec![],
                expected_revision: revision,
            })
            .unwrap();
        assert!(store
            .mark_extension_dispatching_guarded(
                stale.receipt.id,
                PrincipalId(1),
                None,
                Some(&expected),
                None
            )
            .is_err());
        let revision = store
            .set_extension_tenant_allowed("acme/fixture", 1, false, revision)
            .unwrap();
        let expected = serde_json::json!({"archive_sha256":"a".repeat(64),"manifest_sha256":"b".repeat(64),"selection_revision":revision});
        assert!(store
            .mark_extension_dispatching_guarded(
                stale.receipt.id,
                PrincipalId(1),
                None,
                Some(&expected),
                None
            )
            .is_err());
        assert_eq!(
            store
                .extension_execution(stale.receipt.id, PrincipalId(1))
                .unwrap()
                .receipt
                .state,
            State::Claimed
        );
    }

    #[test]
    fn extension_settlement_and_audit_commit_together_and_keep_late_facts() {
        let store = store();
        let input = input(&store, true);
        let id = dispatch(&store, &input);
        store.conn().unwrap().execute_batch("CREATE TRIGGER reject_extension_audit BEFORE INSERT ON operation_audit WHEN NEW.target='extension_execution' BEGIN SELECT RAISE(ABORT,'injected audit failure'); END;").unwrap();
        assert!(store
            .settle_extension_execution(id, PrincipalId(1), acknowledged())
            .is_err());
        assert_eq!(
            store
                .extension_execution(id, PrincipalId(1))
                .unwrap()
                .receipt
                .state,
            State::Dispatching
        );
        assert!(!store
            .list_operation_audit(50)
            .unwrap()
            .iter()
            .any(|r| r.target == "extension_execution"));
        store
            .conn()
            .unwrap()
            .execute_batch("DROP TRIGGER reject_extension_audit")
            .unwrap();
        // Revocation closes delivery but must not erase the retained owner's response.
        store
            .conn()
            .unwrap()
            .execute(
                "UPDATE principal SET disabled_at=?1 WHERE id=1",
                [super::super::now_text()],
            )
            .unwrap();
        assert!(store.extension_execution(id, PrincipalId(1)).is_err());
        let mut observed = acknowledged();
        observed.late_response = true;
        observed.result_code = Code::RequestTimedOut;
        let receipt = store
            .settle_extension_execution(id, PrincipalId(1), observed)
            .unwrap();
        assert_eq!(receipt.state, State::Acknowledged);
        assert!(receipt.late_response);
        assert_eq!(receipt.result_code, Some(Code::RequestTimedOut));
        assert_eq!(
            store
                .settle_extension_execution(id, PrincipalId(1), acknowledged())
                .unwrap(),
            receipt
        );
        let audits: Vec<_> = store
            .list_operation_audit(50)
            .unwrap()
            .into_iter()
            .filter(|r| r.target == "extension_execution")
            .collect();
        assert_eq!(audits.len(), 1);
        assert_eq!(audits[0].id.0, receipt.audit_id.unwrap());
        assert_eq!(audits[0].actor_principal_id, Some(PrincipalId(1)));
        assert_eq!(
            audits[0].correlation_id.as_deref(),
            Some("original-extension-correlation")
        );
        assert_eq!(audits[0].status, "failed");
        assert!(store
            .conn()
            .unwrap()
            .execute(
                "UPDATE extension_execution SET state='claimed' WHERE id=?1",
                [id.to_string()]
            )
            .is_err());
    }

    #[test]
    fn extension_startup_recovers_intents_without_erasing_known_outcomes() {
        let store = store();
        let known_input = input(&store, true);
        let known = dispatch(&store, &known_input);
        let known = store
            .settle_extension_execution(known, PrincipalId(1), acknowledged())
            .unwrap();
        let unissued_input = input(&store, true);
        let unissued = store
            .claim_extension_execution(&unissued_input)
            .unwrap()
            .receipt
            .id;
        let issued_input = input(&store, true);
        let issued = dispatch(&store, &issued_input);
        assert_eq!(
            store
                .extension_execution(unissued, PrincipalId(1))
                .unwrap()
                .receipt
                .state,
            State::Claimed
        );
        assert!(store
            .conn()
            .unwrap()
            .execute(
                "DELETE FROM extension_execution WHERE id=?1",
                [issued.to_string()]
            )
            .is_err());
        assert_eq!(store.interrupt_extension_executions().unwrap(), 2);
        assert_eq!(store.interrupt_extension_executions().unwrap(), 0);
        let refused = store
            .extension_execution(unissued, PrincipalId(1))
            .unwrap()
            .receipt;
        assert_eq!(refused.state, State::Refused);
        assert_eq!(refused.frame_started, Some(false));
        let unknown = store
            .extension_execution(issued, PrincipalId(1))
            .unwrap()
            .receipt;
        assert_eq!(unknown.state, State::OutcomeUnknown);
        assert_eq!(unknown.frame_started, None);
        assert_eq!(
            store
                .extension_execution(known.id, PrincipalId(1))
                .unwrap()
                .receipt,
            known
        );
        assert!(
            !store
                .claim_extension_execution(&unissued_input)
                .unwrap()
                .claimed
        );
        assert!(
            !store
                .claim_extension_execution(&issued_input)
                .unwrap()
                .claimed
        );
        assert_eq!(
            store
                .list_operation_audit(50)
                .unwrap()
                .iter()
                .filter(|r| r.target == "extension_execution")
                .count(),
            3
        );
    }

    #[tokio::test]
    async fn extension_full_restore_never_proves_unissued_from_a_historical_claim() {
        let store = store();
        let original = input(&store, true);
        let pending = store
            .claim_extension_execution(&original)
            .unwrap()
            .receipt
            .id;
        let issued = dispatch(&store, &input(&store, true));
        let known = dispatch(&store, &input(&store, true));
        let known = store
            .settle_extension_execution(known, PrincipalId(1), acknowledged())
            .unwrap();
        let untouched_approval = input(&store, true).approval_id.unwrap();
        store.sanitize_after_restore().await.unwrap();
        assert!(store
            .list_operation_approvals(PrincipalId(1))
            .unwrap()
            .is_empty());
        for id in [pending, issued] {
            let receipt = store
                .extension_execution(id, PrincipalId(1))
                .unwrap()
                .receipt;
            assert_eq!(receipt.state, State::OutcomeUnknown);
            assert_eq!(receipt.frame_started, None);
        }
        assert_eq!(
            store
                .extension_execution(known.id, PrincipalId(1))
                .unwrap()
                .receipt,
            known
        );
        assert!(store
            .consume_operation_approval(&untouched_approval, &original.binding)
            .is_err());
        assert!(!store.claim_extension_execution(&original).unwrap().claimed);
    }
}
