//! Rule debit and the existing native claim share one immediate transaction.
use super::calls::{call_fingerprint, load, require_live, usage, LoadedGrant};
use super::*;
use chrono::DateTime;
use sift_core::ai_delegation::{reserve_action, ArbitrarySqlPolicy, PreparedDelegationAction};

/// Admission evidence for the retained native owner. Its original deadline must
/// bound dispatch; observing a replay never authorizes another dispatch.
#[derive(Debug, Clone)]
pub struct AiDelegationClaim {
    pub claimed: bool,
    pub expires_at: DateTime<Utc>,
    pub approval: AiExecutionApprovalProvenance,
}

/// Native preparation supplied by the server, never deserialized from a model
/// request. Migration changes must be the submitted changes of the retained
/// rendered plan, after command-shape validation for bounded admission.
pub enum AiDelegationDatabaseMaterial {
    RowEdits,
    Migration { changes: Vec<SchemaChange> },
}

struct NativeClaim<'a> {
    call: Uuid,
    operation: &'a str,
    input: &'a serde_json::Value,
    family: &'a str,
    action_id: Uuid,
    fingerprint: &'a str,
    action: PreparedDelegationAction<'a>,
}

fn claim_locked(
    tx: &rusqlite::Transaction<'_>,
    grant: &LoadedGrant,
    native: NativeClaim<'_>,
    claim: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<bool>,
) -> Result<AiDelegationClaim> {
    let expires_at = require_live(tx, grant)?;
    let sha = call_fingerprint(native.operation, native.input)?;
    let reservation: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM ai_delegation_call WHERE run_id=?1 AND call_id=?2 AND operation=?3 AND request_sha256=?4)",params![grant.run.to_string(),native.call.to_string(),native.operation,sha],|r|r.get(0))?;
    if !reservation {
        return Err(invalid(
            "Native action has no matching immutable call reservation",
        ));
    }
    let prior: Option<(String,String,String,String)> = tx.query_row("SELECT family,action_id,action_sha256,rule_id FROM ai_delegation_action WHERE run_id=?1 AND call_id=?2",params![grant.run.to_string(),native.call.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    let (rule_id, claimed) = if let Some((family, id, fp, rule)) = prior {
        if family != native.family || id != native.action_id.to_string() || fp != native.fingerprint
        {
            return Err(invalid("Delegated native claim identity changed"));
        }
        (uuid(rule)?, false)
    } else {
        let policy = tenant_policy(tx, TenantId(grant.prepared.detail.summary.tenant_id))?;
        let debit = reserve_action(
            &grant.prepared.detail.content.policy,
            &usage(tx, grant.run)?,
            native.action,
            ArbitrarySqlPolicy {
                instance_enabled: grant.prepared.instance.arbitrary_sql_enabled,
                tenant_enabled: policy.arbitrary_sql_enabled,
            },
        )
        .map_err(|_| invalid("Native action is outside the reviewed rule or remaining budget"))?;
        // A prior human claim cannot be relabeled as delegated admission.
        if !claim(tx)? {
            return Err(invalid(
                "Native action was already claimed without this delegation",
            ));
        }
        let used = debit.after.rules[&debit.rule_id];
        tx.execute("INSERT INTO ai_delegation_rule_usage(run_id,rule_id,uses,submitted_rows,schema_changes) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(run_id,rule_id) DO UPDATE SET uses=excluded.uses,submitted_rows=excluded.submitted_rows,schema_changes=excluded.schema_changes",params![grant.run.to_string(),debit.rule_id.to_string(),used.uses,used.submitted_rows,used.schema_changes])?;
        tx.execute("INSERT INTO ai_delegation_action(run_id,call_id,rule_id,family,action_id,action_sha256,claimed_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![grant.run.to_string(),native.call.to_string(),debit.rule_id.to_string(),native.family,native.action_id.to_string(),native.fingerprint,super::super::now_text()])?;
        (debit.rule_id, true)
    };
    let (approved_by, approved_at): (i64, String) = tx.query_row(
        "SELECT approved_by,approved_at FROM ai_delegation_review WHERE id=?1",
        [grant.prepared.detail.summary.id.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok(AiDelegationClaim {
        claimed,
        expires_at,
        approval: AiExecutionApprovalProvenance::RunDelegation {
            approved_by,
            approved_at: super::super::parse_time_sql(approved_at)?,
            review_id: grant.prepared.detail.summary.id,
            run_id: grant.run,
            rule_id,
        },
    })
}

impl MetadataStore {
    /// The existing typed execution and proposal apply journals remain the
    /// dispatch owners. Their claim and delegation usage commit atomically.
    pub async fn claim_delegated_ai_database_execution(
        &self,
        id: Uuid,
        actor: PrincipalId,
        lease_token: Uuid,
        material: AiDelegationDatabaseMaterial,
        instance: AiDelegationAdmissionPolicy,
    ) -> Result<AiDelegationClaim> {
        let detail = self.ai_database_execution_detail(id, actor).await?;
        let grant = load(self, detail.summary.run_id, actor, instance).await?;
        if detail.source != grant.prepared.detail.content.source
            || detail.review.session != detail.source.session
            || detail.review.connection != detail.source.connection
            || detail.review.source_digest != detail.proposal.source_digest
        {
            return Err(invalid("Delegated database source or review changed"));
        }
        let acknowledgements = match &detail.review.preview {
            AiDatabasePreview::RowEditSet { .. } => vec![],
            AiDatabasePreview::MigrationDraft { plan } => plan.required_acknowledgements.clone(),
        };
        let request = ApplyAiDatabaseProposalRequest {
            client_request_id: id,
            review_id: detail.review.id,
            review_digest: detail.review.review_digest.clone(),
            production_confirmation: detail
                .source
                .production
                .then(|| detail.source.database_label.clone()),
            acknowledgements,
        };
        let digest = format!("{:x}", Sha256::digest(serde_json::to_vec(&request)?));
        let store = self.clone();
        sqlite_blocking(move || {
            let mut conn = store.conn()?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            super::super::ai_run::ai_run_authorized_conn(&tx, grant.run, actor, lease_token)?;
            let action = match (&detail.proposal.draft, &detail.review.preview, &material) {
                (
                    AiDatabaseDraft::RowEditSet { edit_set, .. },
                    AiDatabasePreview::RowEditSet { .. },
                    AiDelegationDatabaseMaterial::RowEdits,
                ) => PreparedDelegationAction::RowEdits(edit_set),
                (
                    AiDatabaseDraft::MigrationDraft { .. },
                    AiDatabasePreview::MigrationDraft { plan },
                    AiDelegationDatabaseMaterial::Migration { changes },
                ) => PreparedDelegationAction::Migration { plan, changes },
                _ => {
                    return Err(invalid(
                        "Prepared database material differs from its native review",
                    ))
                }
            };
            let input = serde_json::json!({"proposal_id":detail.summary.proposal_id});
            let execution = (id, detail.summary.fingerprint.clone());
            let proof = claim_locked(
                &tx,
                &grant,
                NativeClaim {
                    call: detail.summary.call_id,
                    operation: "sift_execute_database",
                    input: &input,
                    family: "database",
                    action_id: id,
                    fingerprint: &detail.summary.fingerprint,
                    action,
                },
                |tx| {
                    store
                        .claim_ai_database_apply_locked(
                            tx,
                            detail.summary.proposal_id,
                            actor,
                            &request,
                            Some(&execution),
                            &digest,
                        )
                        .map(|prior| prior.is_none())
                },
            )?;
            tx.commit()?;
            Ok(proof)
        })
        .await
    }

    /// Prepared RPC contract, approval consumption, native journal and delegation
    /// usage commit together. The retained owner must use the clamped deadline.
    pub async fn claim_delegated_ai_extension_execution(
        &self,
        id: Uuid,
        actor: PrincipalId,
        lease_token: Uuid,
        mut input: super::super::NewExtensionExecution,
        instance: AiDelegationAdmissionPolicy,
    ) -> Result<(
        AiDelegationClaim,
        Option<super::super::AiExtensionExecutionClaim>,
    )> {
        let detail = self.ai_extension_execution_detail(id, actor).await?;
        super::super::ai_extension_execution::validate_claim_input(id, actor, &detail, &input)?;
        let grant = load(self, detail.summary.run_id, actor, instance).await?;
        if detail.review.source != grant.prepared.detail.content.source
            || self.ai_execute_run_source(grant.run, actor).await? != detail.review.source
        {
            return Err(invalid("Delegated extension source changed"));
        }
        let store = self.clone();
        sqlite_blocking(move || {
            let mut conn=store.conn()?;
            let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            super::super::ai_run::ai_run_authorized_conn(&tx,grant.run,actor,lease_token)?;
            input.deadline_unix_ms=input.deadline_unix_ms.min(require_live(&tx,&grant)?.timestamp_millis());
            let normalized=serde_json::json!({"tool_id":detail.review.tool.id,"arguments":detail.review.arguments});
            let mut execution=None;
            let proof=claim_locked(&tx,&grant,NativeClaim {call:detail.summary.call_id,operation:"sift_execute_extension",input:&normalized,family:"extension",action_id:id,fingerprint:&detail.summary.fingerprint,action:PreparedDelegationAction::Extension(&detail.review)},|tx| {
                let native=store.claim_ai_extension_execution_locked(tx,id,actor,&detail.summary.fingerprint,&mut input)?;
                let claimed=native.execution.claimed;
                execution=Some(native);
                Ok(claimed)
            })?;
            tx.commit()?;
            Ok((proof,execution))
        }).await
    }

    /// The original document actor or live editor companion still supplies the
    /// actual edit evidence. A replay never receives another editor capability.
    pub async fn claim_delegated_ai_query_execution(
        &self,
        id: Uuid,
        actor: PrincipalId,
        lease_token: Uuid,
        instance: AiDelegationAdmissionPolicy,
    ) -> Result<(AiDelegationClaim, Option<AiQueryEditorDispatch>)> {
        let detail = self.ai_query_execution_detail(id, actor).await?;
        let grant = load(self, detail.summary.run_id, actor, instance).await?;
        if self.ai_execute_run_source(grant.run, actor).await?
            != grant.prepared.detail.content.source
        {
            return Err(invalid("Delegated query source changed"));
        }
        let base_revision = detail
            .proposal
            .proposal
            .base_revision
            .ok_or_else(|| invalid("Query proposal has no original revision"))?;
        let store = self.clone();
        sqlite_blocking(move || {
            let mut conn = store.conn()?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            super::super::ai_run::ai_run_authorized_conn(&tx, grant.run, actor, lease_token)?;
            let input = serde_json::json!({"proposal_id":detail.summary.proposal_id});
            let before = format!("{:x}", Sha256::digest(detail.original_sql.as_bytes()));
            let mut dispatch = None;
            let proof = claim_locked(
                &tx,
                &grant,
                NativeClaim {
                    call: detail.summary.call_id,
                    operation: "sift_execute_query",
                    input: &input,
                    family: "query",
                    action_id: id,
                    fingerprint: &detail.summary.fingerprint,
                    action: PreparedDelegationAction::QueryEdit {
                        target: &detail.target,
                        before_sha256: &before,
                        base_revision,
                        replacement: &detail.proposal.proposed_sql,
                    },
                },
                |tx| {
                    let (claimed, capability) = store.claim_ai_query_execution_locked(
                        tx,
                        id,
                        actor,
                        &ApproveAiQueryExecution {
                            fingerprint: detail.summary.fingerprint.clone(),
                            desktop_id: grant.prepared.detail.content.desktop_id,
                        },
                    )?;
                    dispatch = capability;
                    Ok(claimed)
                },
            )?;
            tx.commit()?;
            Ok((proof, dispatch))
        })
        .await
    }

    /// Server-only admission after native SQL preparation and live source checks.
    /// SQL/parameters come from the encrypted immutable request, never the model's
    /// claimed classification. This method does not execute SQL.
    pub async fn claim_delegated_ai_sql_execution(
        &self,
        id: Uuid,
        actor: PrincipalId,
        lease_token: Uuid,
        instance: AiDelegationAdmissionPolicy,
    ) -> Result<AiDelegationClaim> {
        let detail = self.ai_sql_execution(id, actor).await?;
        let grant = load(self, detail.summary.run_id, actor, instance).await?;
        if detail.input.source != grant.prepared.detail.content.source
            || self.ai_execute_run_source(grant.run, actor).await? != detail.input.source
        {
            return Err(invalid("Delegated SQL source changed"));
        }
        let store = self.clone();
        sqlite_blocking(move || {
            let mut conn = store.conn()?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            super::super::ai_run::ai_run_authorized_conn(&tx, grant.run, actor, lease_token)?;
            let input = serde_json::json!({"sql":detail.input.sql,"params":detail.input.params});
            let proof = claim_locked(
                &tx,
                &grant,
                NativeClaim {
                    call: detail.summary.call_id,
                    operation: "sift_execute_sql",
                    input: &input,
                    family: "sql",
                    action_id: id,
                    fingerprint: &detail.summary.fingerprint,
                    action: PreparedDelegationAction::Sql {
                        sql: &detail.input.sql,
                        params: &detail.input.params,
                    },
                },
                |tx| {
                    super::super::ai_execution::claim_ai_sql_execution_locked(
                        tx,
                        id,
                        actor,
                        &detail.summary.fingerprint,
                    )
                },
            )?;
            tx.commit()?;
            Ok(proof)
        })
        .await
    }
}
