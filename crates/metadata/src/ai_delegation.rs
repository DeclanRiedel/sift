//! Encrypted human run reviews. No provider tool can create or approve these.
use super::{sqlite_blocking, MetadataError, MetadataStore, PrincipalId, Result, TenantId};
use chrono::Utc;
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};
use sift_core::ai_delegation::{canonical_digest, migration_contract_digest, validate_policy};
use sift_protocol::*;
use std::collections::BTreeSet;
use uuid::Uuid;

mod end_review;
pub use end_review::AiDelegationReviewChange;

mod claims;
pub use claims::{AiDelegationClaim, AiDelegationDatabaseMaterial};

/// Trusted server policy snapshot, deliberately not a wire request.
#[derive(Clone)]
pub struct AiDelegationAdmissionPolicy {
    pub enabled: bool,
    pub arbitrary_sql_enabled: bool,
    pub scope_sha256: String,
    pub max_calls: u32,
    pub max_duration_secs: u32,
}

/// Decryption is outside the dispatch transaction. Activation must recheck the
/// immutable fingerprint and current approval/policy inside that transaction.
#[derive(Clone)]
pub struct PreparedAiDelegationReview {
    detail: AiDelegationReviewDetail,
    instance: AiDelegationAdmissionPolicy,
}
impl PreparedAiDelegationReview {
    pub fn detail(&self) -> &AiDelegationReviewDetail {
        &self.detail
    }
}
impl std::fmt::Debug for PreparedAiDelegationReview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedAiDelegationReview")
            .field("summary", &self.detail.summary)
            .finish_non_exhaustive()
    }
}

pub struct AiDelegationPolicyChange {
    pub policy: AiDelegationTenantPolicy,
    /// The retained server owner must signal these original run cancellation tokens.
    pub closed_runs: Vec<Uuid>,
}
struct StoredReview {
    summary: AiDelegationReviewSummary,
    handle: String,
    scope: String,
    tenant_revision: u64,
}
fn invalid(message: &str) -> MetadataError {
    MetadataError::AiInvalid(message.into())
}
fn fingerprint(value: &impl serde::Serialize) -> Result<String> {
    canonical_digest(value).map_err(|_| invalid("Delegation content cannot be represented"))
}
fn uuid(value: String) -> Result<Uuid> {
    Uuid::parse_str(&value)
        .map_err(|_| MetadataError::AiContent("Invalid delegation identity".into()))
}
fn hash_is_valid(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn tenant_policy(
    conn: &rusqlite::Connection,
    tenant: TenantId,
) -> Result<AiDelegationTenantPolicy> {
    Ok(conn.query_row("SELECT revision,enabled,arbitrary_sql_enabled FROM ai_delegation_tenant_policy WHERE tenant_id=?1",[tenant.0],|r|Ok(AiDelegationTenantPolicy {tenant_id:tenant.0,revision:r.get(0)?,enabled:r.get(1)?,arbitrary_sql_enabled:r.get(2)?})).optional()?.unwrap_or(AiDelegationTenantPolicy {tenant_id:tenant.0,revision:0,enabled:false,arbitrary_sql_enabled:false}))
}
fn stored(conn: &rusqlite::Connection, id: Uuid, actor: PrincipalId) -> Result<StoredReview> {
    let result=conn.query_row("SELECT chat_id,tenant_id,initiator_id,approved_by,state,review_sha256,used_run_id,created_at,expires_at,content_handle,scope_sha256,tenant_policy_revision FROM ai_delegation_review WHERE id=?1",[id.to_string()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?,r.get::<_,Option<i64>>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,Option<String>>(6)?,r.get::<_,String>(7)?,r.get::<_,String>(8)?,r.get::<_,String>(9)?,r.get::<_,String>(10)?,r.get::<_,u64>(11)?))).optional()?.ok_or(MetadataError::AiNotFound)?;
    let chat = uuid(result.0)?;
    super::ai::require_chat_access(conn, chat, actor)?;
    if result.2 != actor.0 {
        return Err(MetadataError::AiAccessDenied);
    }
    Ok(StoredReview {
        summary: AiDelegationReviewSummary {
            id,
            chat_id: chat,
            tenant_id: result.1,
            initiator_id: result.2,
            approved_by: result.3,
            state: serde_json::from_value(serde_json::Value::String(result.4))?,
            fingerprint: result.5,
            used_run_id: result.6.map(uuid).transpose()?,
            created_at: super::parse_time_sql(result.7)?,
            expires_at: super::parse_time_sql(result.8)?,
        },
        handle: result.9,
        scope: result.10,
        tenant_revision: result.11,
    })
}
fn require_policy(
    content: &AiDelegationReviewContent,
    instance: &AiDelegationAdmissionPolicy,
    tenant: AiDelegationTenantPolicy,
) -> Result<()> {
    validate_policy(&content.policy).map_err(|_| invalid("Delegation policy is invalid"))?;
    if !instance.enabled
        || !tenant.enabled
        || tenant.revision != content.tenant_policy_revision
        || content.scope_sha256 != instance.scope_sha256
        || !hash_is_valid(&instance.scope_sha256)
        || !(1..=100).contains(&instance.max_calls)
        || !(1..=3600).contains(&instance.max_duration_secs)
        || content.policy.budget.max_tool_calls > instance.max_calls
        || content.policy.budget.max_duration_secs > instance.max_duration_secs
        || content.source.tenant_id != tenant.tenant_id
    {
        return Err(invalid("Delegation policy changed or is disabled"));
    }
    if content
        .policy
        .rules
        .iter()
        .any(|r| matches!(r.action, AiDelegationActionRule::ArbitrarySql { .. }))
        && !(instance.arbitrary_sql_enabled && tenant.arbitrary_sql_enabled)
    {
        return Err(invalid(
            "Arbitrary SQL requires current instance and tenant opt-in",
        ));
    }
    Ok(())
}
fn material_rule_id(material: &AiDelegationReviewMaterial) -> Uuid {
    match material {
        AiDelegationReviewMaterial::MigrationPlan { rule_id, .. }
        | AiDelegationReviewMaterial::ExtensionTool { rule_id, .. }
        | AiDelegationReviewMaterial::Query { rule_id, .. } => *rule_id,
    }
}
fn reference_rule_id(material: &AiDelegationMaterialReference) -> Uuid {
    match material {
        AiDelegationMaterialReference::MigrationPlan { rule_id, .. }
        | AiDelegationMaterialReference::ExtensionTool { rule_id, .. }
        | AiDelegationMaterialReference::RoomQuery { rule_id }
        | AiDelegationMaterialReference::PrivateQuery { rule_id, .. } => *rule_id,
    }
}
fn require_materials(content: &AiDelegationReviewContent) -> Result<()> {
    let mut seen = BTreeSet::new();
    for material in &content.materials {
        let id = material_rule_id(material);
        if !seen.insert(id) {
            return Err(invalid("Delegation material is duplicated"));
        }
        let rule = content
            .policy
            .rules
            .iter()
            .find(|r| r.id == id)
            .ok_or_else(|| invalid("Delegation material has no reviewed rule"))?;
        let valid = match (&rule.action, material) {
            (
                AiDelegationActionRule::ExactMigration {
                    contract_sha256,
                    change_count,
                },
                AiDelegationReviewMaterial::MigrationPlan { plan, changes, .. },
            ) => {
                changes.len() == *change_count as usize
                    && migration_contract_digest(plan, changes)
                        .map_err(|_| invalid("Invalid migration review material"))?
                        == *contract_sha256
            }
            (
                AiDelegationActionRule::Extension {
                    contract_sha256, ..
                },
                AiDelegationReviewMaterial::ExtensionTool {
                    tool,
                    context,
                    prepared_contract,
                    ..
                },
            ) => fingerprint(&(tool, prepared_contract, context))? == *contract_sha256,
            (
                AiDelegationActionRule::QueryEdit {
                    before_sha256,
                    base_revision,
                    target,
                    ..
                },
                AiDelegationReviewMaterial::Query {
                    original_sql,
                    base_revision: actual,
                    ..
                },
            ) => {
                format!("{:x}", Sha256::digest(original_sql.as_bytes())) == *before_sha256
                    && actual == base_revision
                    && match target {
                        AiQueryExecutionTarget::PrivateEditor { desktop_id, .. } => {
                            *desktop_id == content.desktop_id
                        }
                        AiQueryExecutionTarget::RoomDocument { .. } => true,
                    }
            }
            _ => false,
        };
        if !valid {
            return Err(invalid(
                "Delegation material differs from its reviewed rule",
            ));
        }
    }
    for rule in &content.policy.rules {
        if matches!(
            rule.action,
            AiDelegationActionRule::ExactMigration { .. }
                | AiDelegationActionRule::Extension { .. }
                | AiDelegationActionRule::QueryEdit { .. }
        ) && !seen.contains(&rule.id)
        {
            return Err(invalid(
                "Delegation requires the actual human-readable review material",
            ));
        }
    }
    Ok(())
}
impl MetadataStore {
    pub async fn ai_delegation_tenant_policy(
        &self,
        tenant: TenantId,
        actor: PrincipalId,
    ) -> Result<AiDelegationTenantPolicy> {
        let store = self.clone();
        sqlite_blocking(move || {
            let conn = store.conn()?;
            super::ensure_tenant_membership_locked(&conn, tenant, actor)?;
            tenant_policy(&conn, tenant)
        })
        .await
    }
    pub async fn set_ai_delegation_tenant_policy(
        &self,
        tenant: TenantId,
        actor: PrincipalId,
        request: SetAiDelegationTenantPolicy,
    ) -> Result<AiDelegationPolicyChange> {
        if request.arbitrary_sql_enabled && !request.enabled {
            return Err(invalid("Arbitrary SQL requires enabled delegation"));
        }
        let store = self.clone();
        sqlite_blocking(move || {
            let mut conn=store.conn()?;let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            super::ensure_tenant_admin_locked(&tx,tenant,actor)?;
            let current=tenant_policy(&tx,tenant)?;
            if current.revision!=request.expected_revision {return Err(invalid("Delegation tenant policy changed"));}
            let revision=current.revision.checked_add(1).filter(|r|*r<=i64::MAX as u64).ok_or_else(||invalid("Delegation policy revision exhausted"))?;
            let closed_runs=tx.prepare("SELECT g.run_id FROM ai_run_delegation g JOIN ai_delegation_review d ON d.id=g.review_id WHERE d.tenant_id=?1 AND g.state='active'")?.query_map([tenant.0],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?.into_iter().map(uuid).collect::<Result<Vec<_>>>()?;
            tx.execute("INSERT INTO ai_delegation_tenant_policy(tenant_id,revision,enabled,arbitrary_sql_enabled,updated_by,updated_at) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(tenant_id) DO UPDATE SET revision=excluded.revision,enabled=excluded.enabled,arbitrary_sql_enabled=excluded.arbitrary_sql_enabled,updated_by=excluded.updated_by,updated_at=excluded.updated_at",params![tenant.0,revision,request.enabled,request.arbitrary_sql_enabled,actor.0,super::now_text()])?;
            tx.execute("UPDATE ai_delegation_review SET state='revoked' WHERE tenant_id=?1 AND state IN ('awaiting_approval','approved')",[tenant.0])?;
            tx.execute("UPDATE ai_run_delegation SET state='revoked',closed_at=?2 WHERE state='active' AND review_id IN (SELECT id FROM ai_delegation_review WHERE tenant_id=?1)",params![tenant.0,super::now_text()])?;
            for run in &closed_runs {
                tx.execute("INSERT INTO ai_run_event(run_id,sequence,kind,at) SELECT id,next_sequence,'stopped',?2 FROM ai_run WHERE id=?1 AND status='running'",params![run.to_string(),super::now_text()])?;
                tx.execute("UPDATE ai_run SET status='canceled',ended_at=?2,next_sequence=next_sequence+1 WHERE id=?1 AND status='running'",params![run.to_string(),super::now_text()])?;
            }
            tx.commit()?;
            Ok(AiDelegationPolicyChange {policy:AiDelegationTenantPolicy {tenant_id:tenant.0,revision,enabled:request.enabled,arbitrary_sql_enabled:request.arbitrary_sql_enabled},closed_runs})
        }).await
    }
    /// The server resolves and authorizes materials and recaptures native source
    /// before entering here. Neither source nor instance policy is model input.
    pub async fn create_ai_delegation_review(
        &self,
        chat: Uuid,
        actor: PrincipalId,
        request: CreateAiDelegationReview,
        content: AiDelegationReviewContent,
        instance: AiDelegationAdmissionPolicy,
    ) -> Result<AiDelegationReviewDetail> {
        if request.client_request_id.is_nil()
            || request.desktop_id.is_nil()
            || content.desktop_id != request.desktop_id
            || content.target != request.target
            || request.expires_at <= Utc::now()
            || request.expires_at > Utc::now() + chrono::Duration::hours(1)
            || request.target.tenant_id != Some(content.source.tenant_id)
            || request.target.profile_id != Some(content.source.profile_id)
            || request
                .target
                .connection_id
                .as_deref()
                .and_then(|v| v.split_once(':'))
                .and_then(|(s, c)| Some((s.parse::<u64>().ok()?, c.parse::<u64>().ok()?)))
                != Some((content.source.session.0, content.source.connection.0))
            || content.source.generation.is_nil()
            || fingerprint(&request.policy)? != fingerprint(&content.policy)?
        {
            return Err(invalid(
                "Delegation review differs from its original request/source",
            ));
        }
        require_materials(&content)?;
        let refs: BTreeSet<_> = request
            .material_references
            .iter()
            .map(reference_rule_id)
            .collect();
        let material_ids: BTreeSet<_> = content.materials.iter().map(material_rule_id).collect();
        if refs.len() != request.material_references.len() || refs != material_ids {
            return Err(invalid(
                "Delegation references differ from resolved materials",
            ));
        }
        let expected_source = content.source.clone();
        let expected_scope = content.scope_sha256.clone();
        let expected_revision = content.tenant_policy_revision;
        for reference in &request.material_references {
            let material = content
                .materials
                .iter()
                .find(|m| material_rule_id(m) == reference_rule_id(reference))
                .ok_or_else(|| invalid("Delegation material is missing"))?;
            let rule = content
                .policy
                .rules
                .iter()
                .find(|r| r.id == reference_rule_id(reference))
                .ok_or_else(|| invalid("Delegation rule is missing"))?;
            let valid = match (reference, material, &rule.action) {
                (
                    AiDelegationMaterialReference::MigrationPlan { plan_id, .. },
                    AiDelegationReviewMaterial::MigrationPlan { plan, .. },
                    _,
                ) => *plan_id == plan.id,
                (
                    AiDelegationMaterialReference::ExtensionTool { tool_id, .. },
                    AiDelegationReviewMaterial::ExtensionTool { tool, .. },
                    _,
                ) => *tool_id == tool.id,
                (
                    AiDelegationMaterialReference::PrivateQuery {
                        original_sql,
                        base_revision,
                        ..
                    },
                    AiDelegationReviewMaterial::Query {
                        original_sql: actual,
                        base_revision: revision,
                        ..
                    },
                    AiDelegationActionRule::QueryEdit {
                        target: AiQueryExecutionTarget::PrivateEditor { .. },
                        ..
                    },
                ) => original_sql == actual && base_revision == revision,
                (
                    AiDelegationMaterialReference::RoomQuery { .. },
                    AiDelegationReviewMaterial::Query { .. },
                    AiDelegationActionRule::QueryEdit {
                        target:
                            AiQueryExecutionTarget::RoomDocument {
                                room_id,
                                document_id,
                            },
                        ..
                    },
                ) => {
                    request.target.room_id == Some(*room_id)
                        && request
                            .target
                            .document_id
                            .as_deref()
                            .and_then(|v| v.parse::<i64>().ok())
                            == Some(*document_id)
                }
                _ => false,
            };
            if !valid {
                return Err(invalid(
                    "Delegation references differ from native review material",
                ));
            }
        }
        let request_sha = fingerprint(&(chat, actor.0, &request))?;
        let store = self.clone();
        let request_id = request.client_request_id;
        let expected_request = request_sha.clone();
        let prior=sqlite_blocking(move || {
            let conn=store.conn()?;let tenant=super::ai::require_chat_access(&conn,chat,actor)?;
            if tenant!=content.source.tenant_id {return Err(MetadataError::AiAccessDenied);}
            let prior=conn.query_row("SELECT id,request_sha256 FROM ai_delegation_review WHERE chat_id=?1 AND initiator_id=?2 AND client_request_id=?3",params![chat.to_string(),actor.0,request_id.to_string()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional()?;
            if let Some((id,sha))=prior {if sha!=expected_request {return Err(invalid("Delegation request identity changed"));}return uuid(id).map(Some);}
            Ok(None)
        }).await?;
        if let Some(id) = prior {
            let detail = self.ai_delegation_review(id, actor).await?;
            if detail.content.source != content.source
                || detail.content.scope_sha256 != instance.scope_sha256
                || detail.content.tenant_policy_revision != expected_revision
            {
                return Err(invalid("Delegation source or policy changed"));
            }
            return Ok(detail);
        }
        let tenant = content.source.tenant_id;
        let tenant_now = self
            .ai_delegation_tenant_policy(TenantId(tenant), actor)
            .await?;
        require_policy(&content, &instance, tenant_now)?;
        let bytes = serde_json::to_vec(&content)?;
        if bytes.len() > super::ai_content::MAX_CONTENT_BYTES {
            return Err(invalid("Delegation review material exceeds its limit"));
        }
        let id = Uuid::new_v4();
        let now = Utc::now();
        let expires = request.expires_at;
        let review_sha = fingerprint(&(id, chat, actor.0, &content, expires))?;
        let handle = self.ai_content.put(tenant, &bytes).await?;
        let stored_handle = handle.clone();
        let scope = content.scope_sha256.clone();
        let revision = content.tenant_policy_revision;
        let store = self.clone();
        let result=sqlite_blocking(move || {
            let mut conn=store.conn()?;let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let record=super::ai::require_chat_access(&tx,chat,actor)?;
            let (owner,visibility):(i64,String)=tx.query_row("SELECT owner_principal_id,visibility FROM ai_chat WHERE id=?1",[chat.to_string()],|r|Ok((r.get(0)?,r.get(1)?)))?;
            if owner!=actor.0 || visibility!="private" || record!=tenant {return Err(MetadataError::AiAccessDenied);}
            require_policy(&content,&instance,tenant_policy(&tx,TenantId(tenant))?)?;
            if expires<=Utc::now() {return Err(invalid("Delegation review expired during preparation"));}
            let prior:Option<(String,String)>=tx.query_row("SELECT id,request_sha256 FROM ai_delegation_review WHERE chat_id=?1 AND initiator_id=?2 AND client_request_id=?3",params![chat.to_string(),actor.0,request_id.to_string()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if let Some((prior,sha))=prior {if sha!=request_sha {return Err(invalid("Delegation request identity changed"));}return Ok((uuid(prior)?,false));}
            tx.execute("INSERT INTO ai_delegation_review(id,chat_id,tenant_id,initiator_id,client_request_id,request_sha256,content_handle,review_sha256,scope_sha256,tenant_policy_revision,state,created_at,expires_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'awaiting_approval',?11,?12)",params![id.to_string(),chat.to_string(),tenant,actor.0,request_id.to_string(),request_sha,stored_handle,review_sha,scope,revision,now.to_rfc3339(),expires.to_rfc3339()])?;
            let floor:u32=tx.pragma_query_value(None,"user_version",|r|r.get(0))?;if floor<65 {tx.pragma_update(None,"user_version",65)?;}
            tx.commit()?;Ok((id,true))
        }).await;
        if !matches!(result, Ok((_, true))) {
            self.ai_content.delete(tenant, &handle).await?;
        }
        let detail = self.ai_delegation_review(result?.0, actor).await?;
        if detail.content.source != expected_source
            || detail.content.scope_sha256 != expected_scope
            || detail.content.tenant_policy_revision != expected_revision
        {
            return Err(invalid(
                "Delegation source or policy changed during preparation",
            ));
        }
        Ok(detail)
    }
    pub async fn ai_delegation_review(
        &self,
        id: Uuid,
        actor: PrincipalId,
    ) -> Result<AiDelegationReviewDetail> {
        let store = self.clone();
        let row = sqlite_blocking(move || {
            let conn = store.conn()?;
            stored(&conn, id, actor)
        })
        .await?;
        let bytes = self
            .ai_content
            .get(row.summary.tenant_id, &row.handle)
            .await?
            .ok_or_else(|| {
                MetadataError::AiContent("Delegation review content unavailable".into())
            })?;
        let content: AiDelegationReviewContent = serde_json::from_slice(&bytes)
            .map_err(|_| MetadataError::AiContent("Delegation review content is invalid".into()))?;
        if fingerprint(&(
            id,
            row.summary.chat_id,
            actor.0,
            &content,
            row.summary.expires_at,
        ))? != row.summary.fingerprint
            || content.scope_sha256 != row.scope
            || content.tenant_policy_revision != row.tenant_revision
            || content.source.tenant_id != row.summary.tenant_id
        {
            return Err(invalid(
                "Delegation review content differs from its immutable identity",
            ));
        }
        let store = self.clone();
        let current = sqlite_blocking(move || {
            let conn = store.conn()?;
            stored(&conn, id, actor)
        })
        .await?;
        if current.handle != row.handle || current.summary.fingerprint != row.summary.fingerprint {
            return Err(invalid("Delegation review changed during read"));
        }
        Ok(AiDelegationReviewDetail {
            summary: current.summary,
            content,
        })
    }
    /// Human-only approval; native source/caller must also be checked by its
    /// retained server owner. No automatic execution is performed by approval.
    pub async fn approve_ai_delegation_review(
        &self,
        id: Uuid,
        actor: PrincipalId,
        request: ApproveAiDelegationReview,
        instance: AiDelegationAdmissionPolicy,
    ) -> Result<AiDelegationReviewSummary> {
        let detail = self.ai_delegation_review(id, actor).await?;
        if detail.summary.fingerprint != request.fingerprint {
            return Err(invalid("Delegation review changed"));
        }
        if detail.content.source.production
            && request.production_confirmation.as_deref()
                != Some(detail.content.source.database_label.as_str())
        {
            return Err(invalid("Type the reviewed production database label"));
        }
        let store = self.clone();
        sqlite_blocking(move || {
            let mut conn=store.conn()?;let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate)?;let row=stored(&tx,id,actor)?;
            require_policy(&detail.content,&instance,tenant_policy(&tx,TenantId(row.summary.tenant_id))?)?;
            if row.summary.fingerprint!=request.fingerprint || row.summary.expires_at<=Utc::now() {return Err(invalid("Delegation review changed or expired"));}
            match row.summary.state {
                AiDelegationReviewState::AwaitingApproval => {tx.execute("UPDATE ai_delegation_review SET state='approved',approved_by=?2,approved_at=?3 WHERE id=?1",params![id.to_string(),actor.0,super::now_text()])?;},
                AiDelegationReviewState::Approved => {},
                _=>return Err(invalid("Delegation review cannot issue new approval")),
            }
            let summary=stored(&tx,id,actor)?.summary;tx.commit()?;Ok(summary)
        }).await
    }
    pub async fn prepare_ai_delegation_review(
        &self,
        id: Uuid,
        actor: PrincipalId,
        instance: AiDelegationAdmissionPolicy,
    ) -> Result<PreparedAiDelegationReview> {
        let detail = self.ai_delegation_review(id, actor).await?;
        require_policy(
            &detail.content,
            &instance,
            self.ai_delegation_tenant_policy(TenantId(detail.summary.tenant_id), actor)
                .await?,
        )?;
        if detail.summary.expires_at <= Utc::now() {
            return Err(invalid("Delegation review expired"));
        }
        let prepared = PreparedAiDelegationReview { detail, instance };
        match prepared.detail.summary.state {
            AiDelegationReviewState::Approved => {}
            AiDelegationReviewState::Consumed => {
                let run = prepared
                    .detail
                    .summary
                    .used_run_id
                    .ok_or_else(|| invalid("Consumed delegation has no original run"))?;
                self.require_ai_delegation_start_replay(run, actor, &prepared)
                    .await?;
            }
            _ => return Err(invalid("Delegation review is not an active human approval")),
        }
        Ok(prepared)
    }
}

mod calls;
pub use calls::{AiDelegationCallReservation, NewAiDelegationCall};
mod lifecycle;
pub(super) use lifecycle::{activate_run_locked, require_start_context};

#[cfg(test)]
mod tests;
