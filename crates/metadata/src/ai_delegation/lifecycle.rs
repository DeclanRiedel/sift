//! One-use run activation and same-run observations. No provider startup occurs here.
use super::*;
use chrono::DateTime;

pub(crate) fn require_start_context(
    prepared: &PreparedAiDelegationReview,
    chat: Uuid,
    actor: PrincipalId,
    request: &StartAiTurnRequest,
    source: Option<&AiExecutionSource>,
) -> Result<()> {
    let detail = &prepared.detail;
    let content = &detail.content;
    if request.mode != AiMode::Execute
        || detail.summary.chat_id != chat
        || detail.summary.initiator_id != actor.0
        || source != Some(&content.source)
        || request.desktop_id != content.desktop_id
        || request.context.target != content.target
    {
        return Err(invalid(
            "Delegated startup differs from its reviewed caller or source",
        ));
    }
    for rule in &content.policy.rules {
        if let AiDelegationActionRule::QueryEdit {
            target,
            before_sha256,
            base_revision,
            ..
        } = &rule.action
        {
            let sql = request.context.sql.as_ref().ok_or_else(|| {
                invalid("Delegated query editing requires the original included query")
            })?;
            let original = format!("{:x}", Sha256::digest(sql.text.as_bytes()));
            let target_matches = match target {
                AiQueryExecutionTarget::PrivateEditor {
                    desktop_id,
                    item_id,
                } => {
                    request.desktop_id == *desktop_id
                        && request.context.editor_item_id == Some(*item_id)
                        && sql.room_document_id.is_none()
                }
                AiQueryExecutionTarget::RoomDocument {
                    room_id,
                    document_id,
                } => {
                    request.context.target.room_id == Some(*room_id)
                        && request
                            .context
                            .target
                            .document_id
                            .as_deref()
                            .and_then(|s| s.parse::<i64>().ok())
                            == Some(*document_id)
                        && sql.room_document_id == Some(*document_id)
                }
            };
            if !target_matches
                || original != *before_sha256
                || sql.document_revision != Some(*base_revision)
            {
                return Err(invalid("Delegated query target, text or revision changed"));
            }
        }
    }
    Ok(())
}

pub(crate) fn activate_run_locked(
    tx: &rusqlite::Transaction<'_>,
    run: Uuid,
    actor: PrincipalId,
    prepared: &PreparedAiDelegationReview,
    started: DateTime<Utc>,
) -> Result<()> {
    let detail = &prepared.detail;
    let row = stored(tx, detail.summary.id, actor)?;
    require_policy(
        &detail.content,
        &prepared.instance,
        tenant_policy(tx, TenantId(row.summary.tenant_id))?,
    )?;
    if row.summary.state != AiDelegationReviewState::Approved
        || row.summary.fingerprint != detail.summary.fingerprint
        || row.scope != prepared.instance.scope_sha256
        || row.summary.expires_at <= Utc::now()
        || row.summary.approved_by != Some(actor.0)
    {
        return Err(invalid(
            "Delegation approval changed, expired or was already consumed",
        ));
    }
    let source_bound:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM ai_execute_run_source s JOIN ai_run r ON r.id=s.run_id WHERE r.id=?1 AND r.initiator_principal_id=?2 AND r.chat_id=?3 AND s.tenant_id=?4 AND r.desktop_id=?5 AND r.status='running' AND r.execute_mode=1)",params![run.to_string(),actor.0,row.summary.chat_id.to_string(),row.summary.tenant_id,detail.content.desktop_id.to_string()],|r|r.get(0))?;
    if !source_bound {
        return Err(invalid(
            "Delegation requires its original atomically bound Execute source",
        ));
    }
    let budget = detail.content.policy.budget;
    let expires = row
        .summary
        .expires_at
        .min(started + chrono::Duration::seconds(i64::from(budget.max_duration_secs)));
    if expires <= Utc::now() {
        return Err(invalid("Delegated run deadline elapsed before admission"));
    }
    tx.execute("INSERT INTO ai_run_delegation(run_id,review_id,state,created_at,expires_at,max_calls,max_input_bytes,max_output_bytes) VALUES(?1,?2,'active',?3,?4,?5,?6,?7)",params![run.to_string(),row.summary.id.to_string(),started.to_rfc3339(),expires.to_rfc3339(),budget.max_tool_calls,budget.max_input_bytes,budget.max_output_bytes])?;
    tx.execute("UPDATE ai_delegation_review SET state='consumed',used_run_id=?2 WHERE id=?1 AND state='approved'",params![row.summary.id.to_string(),run.to_string()])?;
    Ok(())
}

impl MetadataStore {
    pub(crate) async fn require_ai_delegation_start_replay(
        &self,
        run: Uuid,
        actor: PrincipalId,
        prepared: &PreparedAiDelegationReview,
    ) -> Result<()> {
        let prepared = prepared.clone();
        let store = self.clone();
        sqlite_blocking(move || {
            let conn=store.conn()?;let row=stored(&conn,prepared.detail.summary.id,actor)?;
            require_policy(&prepared.detail.content,&prepared.instance,tenant_policy(&conn,TenantId(row.summary.tenant_id))?)?;
            let matches:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM ai_run_delegation g JOIN ai_run r ON r.id=g.run_id WHERE g.run_id=?1 AND g.review_id=?2 AND g.state='active' AND julianday(g.expires_at)>julianday(?3) AND r.initiator_principal_id=?4 AND r.status='running')",params![run.to_string(),row.summary.id.to_string(),super::super::now_text(),actor.0],|r|r.get(0))?;
            if !matches || row.summary.state!=AiDelegationReviewState::Consumed || row.summary.used_run_id!=Some(run) || row.summary.fingerprint!=prepared.detail.summary.fingerprint {return Err(invalid("Delegated startup replay cannot renew or replace its original run"));}
            Ok(())
        }).await
    }
}
