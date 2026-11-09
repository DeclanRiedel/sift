//! Redacted native migration witnesses, independent of workflow success.
use super::{ConnectionProfileId, MetadataError, MetadataStore, PrincipalId, Result, TenantId};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};
use sift_protocol::*;
use std::collections::{BTreeMap, BTreeSet};

fn invalid(message: &str) -> MetadataError {
    MetadataError::MigrationNativeInvalid(message.into())
}
fn hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
fn phase(value: MigrationNativePhase) -> &'static str {
    match value {
        MigrationNativePhase::Prepared => "prepared",
        MigrationNativePhase::Beginning => "beginning",
        MigrationNativePhase::Executing => "executing",
        MigrationNativePhase::Committing => "committing",
        MigrationNativePhase::RollingBack => "rolling_back",
        MigrationNativePhase::Settled => "settled",
    }
}
fn outcome(value: Option<ChangeLedgerOutcome>) -> Result<Option<&'static str>> {
    value
        .map(|value| match value {
            ChangeLedgerOutcome::Committed => Ok("committed"),
            ChangeLedgerOutcome::RolledBack => Ok("rolled_back"),
            ChangeLedgerOutcome::OutcomeUnknown => Ok("outcome_unknown"),
            _ => Err(invalid("unsupported native group outcome")),
        })
        .transpose()
}
fn decode<T: serde::de::DeserializeOwned>(value: &str) -> Result<T> {
    serde_json::from_value(serde_json::Value::String(value.into()))
        .map_err(|_| invalid("invalid native journal enum"))
}
fn owner(
    conn: &Connection,
    tenant: TenantId,
    actor: PrincipalId,
    run: MigrationRunId,
) -> Result<()> {
    let owned: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM migration_run WHERE id=?1 AND tenant_id=?2 AND creator_principal_id=?3)",
        params![run.to_string(),tenant.0,actor.0], |r|r.get(0))?;
    if !owned {
        return Err(MetadataError::MigrationRunNotFound);
    }
    Ok(())
}
fn revision(conn: &Connection, run: MigrationRunId, open: bool) -> Result<u64> {
    let (revision, closed): (u64, bool) = conn
        .query_row(
            "SELECT revision,closed FROM migration_native_journal WHERE run_id=?1",
            [run.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| invalid("native journal is not open"))?;
    if open && closed {
        return Err(invalid("native journal admission is closed"));
    }
    Ok(revision)
}
fn advance(conn: &Connection, run: MigrationRunId) -> Result<u64> {
    if conn.execute("UPDATE migration_native_journal SET revision=revision+1 WHERE run_id=?1 AND revision<9223372036854775807",[run.to_string()])? != 1 {
        return Err(invalid("native journal revision exhausted"));
    }
    revision(conn, run, false)
}
fn group(conn: &Connection, run: MigrationRunId, ordinal: u32) -> Result<MigrationNativeGroup> {
    let (transactional,tx,stage,commit,rollback,end):(bool,Option<String>,String,bool,bool,Option<String>)=conn.query_row(
        "SELECT transactional,transaction_id,phase,commit_intent,rollback_intent,outcome FROM migration_native_group WHERE run_id=?1 AND ordinal=?2",
        params![run.to_string(),ordinal],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))
        .optional()?.ok_or_else(||invalid("group is not in the frozen migration plan"))?;
    Ok(MigrationNativeGroup {
        ordinal,
        transactional,
        transaction_id: tx
            .map(|tx| {
                tx.parse::<u64>()
                    .map(TxId)
                    .map_err(|_| invalid("invalid native transaction witness"))
            })
            .transpose()?,
        phase: decode(&stage)?,
        commit_intent: commit,
        rollback_intent: rollback,
        outcome: end.map(|end| decode(&end)).transpose()?,
    })
}
fn valid_group(value: &MigrationNativeGroup) -> Result<()> {
    use MigrationNativePhase::*;
    if value.ordinal == 0 {
        return Err(invalid("group ordinal must be positive"));
    }
    outcome(value.outcome)?;
    if !value.transactional {
        if value.transaction_id.is_some()
            || value.commit_intent
            || value.rollback_intent
            || value.outcome.is_some()
            || !matches!(value.phase, Prepared | Executing | Settled)
        {
            return Err(invalid(
                "nontransactional groups cannot claim an atomic native outcome",
            ));
        }
        return Ok(());
    }
    if value.phase != Settled && value.outcome.is_some() {
        return Err(invalid("unsettled group has a terminal outcome"));
    }
    match value.phase {
        Prepared | Beginning
            if value.transaction_id.is_some() || value.commit_intent || value.rollback_intent =>
        {
            return Err(invalid("transaction began before its witness"))
        }
        Executing | Committing | RollingBack if value.transaction_id.is_none() => {
            return Err(invalid(
                "transactional group needs its managed transaction witness",
            ))
        }
        _ => {}
    }
    if value.phase == Executing && (value.commit_intent || value.rollback_intent) {
        return Err(invalid("transaction end needs a separate durable intent"));
    }
    if value.phase == Committing && !value.commit_intent
        || value.phase == RollingBack && !value.rollback_intent
    {
        return Err(invalid("transaction end intent is missing"));
    }
    match value.outcome {
        Some(ChangeLedgerOutcome::Committed)
            if !value.commit_intent || value.transaction_id.is_none() =>
        {
            return Err(invalid("commit witness is missing"))
        }
        Some(ChangeLedgerOutcome::RolledBack)
            if !value.rollback_intent || value.transaction_id.is_none() =>
        {
            return Err(invalid("rollback witness is missing"))
        }
        None if value.phase == Settled
            && (value.transaction_id.is_some() || value.commit_intent || value.rollback_intent) =>
        {
            return Err(invalid("settled transaction has no outcome"))
        }
        _ => {}
    }
    Ok(())
}

impl MetadataStore {
    /// Freeze identities before any BEGIN/statement intent. No SQL is persisted.
    pub fn open_migration_native_journal(
        &self,
        tenant: TenantId,
        profile: ConnectionProfileId,
        actor: PrincipalId,
        plan: &MigrationPlan,
        source: &MigrationNativeSource,
    ) -> Result<u64> {
        if source.profile_id != profile.0
            || source.generation.is_nil()
            || !hash(&source.configuration_sha256)
            || !plan.digest.strip_prefix("migfp:").is_some_and(hash)
            || plan.groups.is_empty()
        {
            return Err(invalid("invalid frozen migration identity"));
        }
        let source_json = serde_json::to_string(source)?;
        if source_json.len() > 65536 {
            return Err(invalid("native source proof exceeds its bound"));
        }
        let mut groups = BTreeSet::new();
        let mut count = 0usize;
        for group in &plan.groups {
            if group.ordinal == 0 || !groups.insert(group.ordinal) {
                return Err(invalid("duplicate or invalid group ordinal"));
            }
            let mut statements = BTreeSet::new();
            for statement in &group.statements {
                if statement.ordinal == 0
                    || !statements.insert(statement.ordinal)
                    || !statement
                        .fingerprint
                        .strip_prefix("sqlfp:")
                        .is_some_and(hash)
                {
                    return Err(invalid("invalid frozen statement identity"));
                }
                count += 1;
            }
        }
        if count == 0 {
            return Err(invalid("migration has no statements"));
        }
        let manifest_sha256 = format!("{:x}", Sha256::digest(serde_json::to_vec(&plan.groups)?));
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        owner(&tx, tenant, actor, plan.run_id)?;
        let (id,digest,stored_profile,session,connection,state):(String,String,Option<i64>,u64,u64,String)=tx.query_row(
            "SELECT plan_id,plan_digest,connection_profile_id,session_id,connection_id,state FROM migration_run WHERE id=?1",[plan.run_id.to_string()],
            |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?;
        if id != plan.id.to_string()
            || digest != plan.digest
            || stored_profile != Some(profile.0)
            || source.session.0 != session
            || source.connection.0 != connection
        {
            return Err(invalid(
                "native journal does not match the captured workflow",
            ));
        }
        let prior: Option<(String, String)> = tx
            .query_row(
                "SELECT source_json,manifest_sha256 FROM migration_native_journal WHERE run_id=?1",
                [plan.run_id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((prior, manifest)) = prior {
            if prior != source_json || manifest != manifest_sha256 {
                return Err(invalid("frozen native source changed"));
            }
            let total: usize = tx.query_row(
                "SELECT COUNT(*) FROM migration_native_statement WHERE run_id=?1",
                [plan.run_id.to_string()],
                |r| r.get(0),
            )?;
            let group_count: usize = tx.query_row(
                "SELECT COUNT(*) FROM migration_native_group WHERE run_id=?1",
                [plan.run_id.to_string()],
                |r| r.get(0),
            )?;
            if total != count || group_count != plan.groups.len() {
                return Err(invalid("frozen native manifest changed"));
            }
            for planned in &plan.groups {
                if group(&tx, plan.run_id, planned.ordinal)?.transactional != planned.transactional
                {
                    return Err(invalid("frozen group changed"));
                }
                for statement in &planned.statements {
                    let fp:Option<String>=tx.query_row("SELECT fingerprint FROM migration_native_statement WHERE run_id=?1 AND group_ordinal=?2 AND ordinal=?3",params![plan.run_id.to_string(),planned.ordinal,statement.ordinal],|r|r.get(0)).optional()?;
                    if fp.as_deref() != Some(statement.fingerprint.as_str()) {
                        return Err(invalid("frozen statement changed"));
                    }
                }
            }
            return revision(&tx, plan.run_id, false);
        }
        if state != "running" {
            return Err(invalid(
                "cannot invent native witnesses for a terminal workflow",
            ));
        }
        let floor: u32 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if floor < 59 {
            tx.pragma_update(None, "user_version", 59)?;
        }
        tx.execute("INSERT INTO migration_native_journal(run_id,format_version,source_json,manifest_sha256,revision) VALUES(?1,1,?2,?3,1)",params![plan.run_id.to_string(),source_json,manifest_sha256])?;
        for group in &plan.groups {
            tx.execute("INSERT INTO migration_native_group(run_id,ordinal,transactional,phase) VALUES(?1,?2,?3,'prepared')",params![plan.run_id.to_string(),group.ordinal,group.transactional])?;
            for statement in &group.statements {
                tx.execute("INSERT INTO migration_native_statement(run_id,group_ordinal,ordinal,fingerprint) VALUES(?1,?2,?3,?4)",params![plan.run_id.to_string(),group.ordinal,statement.ordinal,statement.fingerprint])?;
            }
        }
        tx.commit()?;
        Ok(1)
    }

    /// One-use durable BEGIN/group admission intent. Replaying it never resumes work.
    pub fn begin_migration_native_group(
        &self,
        tenant: TenantId,
        actor: PrincipalId,
        run: MigrationRunId,
        ordinal: u32,
    ) -> Result<u64> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        owner(&tx, tenant, actor, run)?;
        revision(&tx, run, true)?;
        if tx.execute("UPDATE migration_native_group SET phase=CASE WHEN transactional=1 THEN 'beginning' ELSE 'executing' END WHERE run_id=?1 AND ordinal=?2 AND phase='prepared'",params![run.to_string(),ordinal])?!=1 {
            return Err(invalid("group intent already used or missing"));
        }
        let revision = advance(&tx, run)?;
        tx.commit()?;
        Ok(revision)
    }

    pub fn record_migration_native_group(
        &self,
        tenant: TenantId,
        actor: PrincipalId,
        run: MigrationRunId,
        value: &MigrationNativeGroup,
    ) -> Result<u64> {
        valid_group(value)?;
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        owner(&tx, tenant, actor, run)?;
        revision(&tx, run, true)?;
        let old = group(&tx, run, value.ordinal)?;
        use MigrationNativePhase::*;
        let allowed = matches!(
            (old.phase, value.phase),
            (Beginning, Executing | Settled)
                | (Executing, Committing | RollingBack | Settled)
                | (Committing, RollingBack | Settled)
                | (RollingBack, Settled)
        );
        if !allowed
            || old.transactional != value.transactional
            || old.transaction_id.is_some() && old.transaction_id != value.transaction_id
            || old.commit_intent && !value.commit_intent
            || old.rollback_intent && !value.rollback_intent
        {
            return Err(invalid(
                "native group transition refuses replay or changed identity",
            ));
        }
        if value.outcome == Some(ChangeLedgerOutcome::Committed) && old.phase != Committing
            || value.outcome == Some(ChangeLedgerOutcome::RolledBack) && old.phase != RollingBack
        {
            return Err(invalid("known native outcome needs its prior end intent"));
        }
        if value.phase == Committing {
            let pending:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM migration_native_statement WHERE run_id=?1 AND group_ordinal=?2 AND (intent_recorded=0 OR native_admitted IS NOT 1 OR native_completed IS NOT 1))",params![run.to_string(),value.ordinal],|r|r.get(0))?;
            if pending {
                return Err(invalid(
                    "cannot commit before every planned statement has native completion evidence",
                ));
            }
        }
        tx.execute("UPDATE migration_native_group SET transaction_id=?3,phase=?4,commit_intent=?5,rollback_intent=?6,outcome=?7 WHERE run_id=?1 AND ordinal=?2",
            params![run.to_string(),value.ordinal,value.transaction_id.map(|tx|tx.0.to_string()),phase(value.phase),value.commit_intent,value.rollback_intent,outcome(value.outcome)?])?;
        let revision = advance(&tx, run)?;
        tx.commit()?;
        Ok(revision)
    }

    pub fn prepare_migration_native_statement(
        &self,
        tenant: TenantId,
        actor: PrincipalId,
        run: MigrationRunId,
        group_ordinal: u32,
        ordinal: u32,
    ) -> Result<u64> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        owner(&tx, tenant, actor, run)?;
        revision(&tx, run, true)?;
        if group(&tx, run, group_ordinal)?.phase != MigrationNativePhase::Executing {
            return Err(invalid("group cannot admit a statement"));
        }
        if tx.execute("UPDATE migration_native_statement SET intent_recorded=1 WHERE run_id=?1 AND group_ordinal=?2 AND ordinal=?3 AND intent_recorded=0",params![run.to_string(),group_ordinal,ordinal])?!=1 {
            return Err(invalid("statement intent already used or missing"));
        }
        let revision = advance(&tx, run)?;
        tx.commit()?;
        Ok(revision)
    }

    pub fn record_migration_native_statement(
        &self,
        tenant: TenantId,
        actor: PrincipalId,
        run: MigrationRunId,
        group_ordinal: u32,
        value: &MigrationNativeStatement,
    ) -> Result<u64> {
        if !value.intent_recorded
            || value.native_admitted.is_none()
            || value.native_admitted != Some(true)
                && (value.native_completed.is_some()
                    || value.autocommit_known
                    || value.affected_rows.is_some())
            || value.native_completed != Some(true) && value.affected_rows.is_some()
            || value.result_code.as_ref().is_some_and(|code| {
                code.is_empty()
                    || code.len() > 64
                    || !code
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            })
        {
            return Err(invalid("invalid native statement observation"));
        }
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        owner(&tx, tenant, actor, run)?;
        revision(&tx, run, true)?;
        let native_group = group(&tx, run, group_ordinal)?;
        if native_group.phase != MigrationNativePhase::Executing {
            return Err(invalid("statement owner no longer owns the group"));
        }
        if native_group.transactional && value.autocommit_known {
            return Err(invalid(
                "transactional statements cannot claim autocommit evidence",
            ));
        }
        let (fp,intent,admitted,completed):(String,bool,Option<bool>,Option<bool>)=tx.query_row("SELECT fingerprint,intent_recorded,native_admitted,native_completed FROM migration_native_statement WHERE run_id=?1 AND group_ordinal=?2 AND ordinal=?3",params![run.to_string(),group_ordinal,value.ordinal],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))
            .optional()?.ok_or_else(||invalid("statement is not in the frozen plan"))?;
        if fp != value.fingerprint
            || !intent
            || admitted.is_some() && admitted != value.native_admitted
            || completed.is_some()
        {
            return Err(invalid(
                "statement observation is frozen or has changed identity",
            ));
        }
        tx.execute("UPDATE migration_native_statement SET native_admitted=?4,native_completed=?5,autocommit_known=?6,affected_rows=?7,result_code=?8 WHERE run_id=?1 AND group_ordinal=?2 AND ordinal=?3",
            params![run.to_string(),group_ordinal,value.ordinal,value.native_admitted,value.native_completed,value.autocommit_known,value.affected_rows.map(|n|n.to_string()),value.result_code])?;
        let revision = advance(&tx, run)?;
        tx.commit()?;
        Ok(revision)
    }

    /// Close admission and preserve known terminal groups. Pending intent stays uncertain.
    pub fn close_migration_native_journal(
        &self,
        tenant: TenantId,
        actor: PrincipalId,
        run: MigrationRunId,
    ) -> Result<u64> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        owner(&tx, tenant, actor, run)?;
        if !tx.query_row(
            "SELECT closed FROM migration_native_journal WHERE run_id=?1",
            [run.to_string()],
            |r| r.get::<_, bool>(0),
        )? {
            close(&tx, run)?;
        }
        let revision = revision(&tx, run, false)?;
        tx.commit()?;
        Ok(revision)
    }
}

fn close(conn: &Connection, run: MigrationRunId) -> Result<()> {
    conn.execute("UPDATE migration_native_group SET outcome=CASE WHEN transactional=1 AND phase<>'prepared' THEN 'outcome_unknown' ELSE outcome END,phase='settled' WHERE run_id=?1 AND phase<>'settled'",[run.to_string()])?;
    conn.execute(
        "UPDATE migration_native_journal SET closed=1 WHERE run_id=?1",
        [run.to_string()],
    )?;
    advance(conn, run)?;
    Ok(())
}

impl MetadataStore {
    pub fn migration_native_outcomes(
        &self,
        tenant: TenantId,
        run: MigrationRunId,
        request: &MigrationNativePageRequest,
    ) -> Result<MigrationNativeOutcomesPage> {
        if request.limit == 0
            || request.limit > 100
            || request.offset > 0 && request.expected_revision.is_none()
        {
            return Err(invalid(
                "native observation pages need a bound and stable revision",
            ));
        }
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let (plan_id, plan_digest): (String, String) = tx
            .query_row(
                "SELECT plan_id,plan_digest FROM migration_run WHERE id=?1 AND tenant_id=?2",
                params![run.to_string(), tenant.0],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or(MetadataError::MigrationRunNotFound)?;
        let header:Option<(u32,String,u64,bool)>=tx.query_row("SELECT format_version,source_json,revision,closed FROM migration_native_journal WHERE run_id=?1",[run.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        let mut page = MigrationNativeOutcomesPage {
            format_version: MIGRATION_NATIVE_FORMAT_VERSION,
            run_id: run,
            plan_id: MigrationPlanId(
                uuid::Uuid::parse_str(&plan_id)
                    .map_err(|_| invalid("invalid stored plan identity"))?,
            ),
            plan_digest,
            journal_present: header.is_some(),
            source: None,
            revision: header.as_ref().map_or(0, |h| h.2),
            closed: header.as_ref().is_some_and(|h| h.3),
            observations: vec![],
            next_offset: None,
        };
        if request
            .expected_revision
            .is_some_and(|expected| expected != page.revision)
        {
            return Err(MetadataError::MigrationNativeRevisionConflict);
        }
        let Some((version, source, _, _)) = header else {
            return Ok(page);
        };
        if version != MIGRATION_NATIVE_FORMAT_VERSION || source.len() > 65536 {
            return Err(invalid("unsupported native journal representation"));
        }
        page.source = Some(
            serde_json::from_str(&source).map_err(|_| invalid("invalid frozen source proof"))?,
        );
        let rows = {
            let mut stmt=tx.prepare("SELECT g.ordinal,s.ordinal,s.fingerprint,s.intent_recorded,s.native_admitted,s.native_completed,s.autocommit_known,s.affected_rows,s.result_code FROM migration_native_group g LEFT JOIN migration_native_statement s ON s.run_id=g.run_id AND s.group_ordinal=g.ordinal WHERE g.run_id=?1 ORDER BY g.ordinal,s.ordinal LIMIT ?2 OFFSET ?3")?;
            let rows = stmt
                .query_map(
                    params![
                        run.to_string(),
                        u32::from(request.limit) + 1,
                        request.offset
                    ],
                    |r| {
                        Ok((
                            r.get::<_, u32>(0)?,
                            r.get::<_, Option<u32>>(1)?,
                            r.get::<_, Option<String>>(2)?,
                            r.get::<_, Option<bool>>(3)?,
                            r.get::<_, Option<bool>>(4)?,
                            r.get::<_, Option<bool>>(5)?,
                            r.get::<_, Option<bool>>(6)?,
                            r.get::<_, Option<String>>(7)?,
                            r.get::<_, Option<String>>(8)?,
                        ))
                    },
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let more = rows.len() > usize::from(request.limit);
        let mut groups = BTreeMap::new();
        for (group_ordinal, ordinal, fp, intent, admitted, completed, autocommit, rows, code) in
            rows.into_iter().take(usize::from(request.limit))
        {
            let native_group = if let Some(group) = groups.get(&group_ordinal) {
                Clone::clone(group)
            } else {
                let value = group(&tx, run, group_ordinal)?;
                groups.insert(group_ordinal, value.clone());
                value
            };
            let statement = ordinal
                .map(|ordinal| {
                    Ok::<_, MetadataError>(MigrationNativeStatement {
                        ordinal,
                        fingerprint: fp.ok_or_else(|| invalid("statement fingerprint missing"))?,
                        intent_recorded: intent.unwrap_or(false),
                        native_admitted: admitted,
                        native_completed: completed,
                        autocommit_known: autocommit.unwrap_or(false),
                        affected_rows: rows
                            .map(|rows| {
                                rows.parse::<u64>()
                                    .map_err(|_| invalid("invalid native affected count"))
                            })
                            .transpose()?,
                        result_code: code,
                    })
                })
                .transpose()?;
            page.observations.push(MigrationNativeObservation {
                group: native_group,
                statement,
            });
        }
        if more {
            page.next_offset = Some(
                request
                    .offset
                    .checked_add(u32::from(request.limit))
                    .ok_or_else(|| invalid("native page offset overflow"))?,
            );
        }
        tx.commit()?;
        Ok(page)
    }

    /// Startup/offline use only: never interrupts a live server's active owner.
    pub fn interrupt_migration_native_journals(&self) -> Result<usize> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let count = interrupt(&tx, None, &super::now_text())?;
        tx.commit()?;
        Ok(count)
    }
}

pub(crate) fn interrupt(conn: &Connection, tenant: Option<TenantId>, now: &str) -> Result<usize> {
    let runs = {
        // Closing journal admission and persisting the workflow result are
        // separate writes. A crash between them must not leave a closed
        // journal's parent appearing to run forever.
        let mut stmt=conn.prepare("SELECT j.run_id,j.closed FROM migration_native_journal j JOIN migration_run m ON m.id=j.run_id WHERE (j.closed=0 OR m.state='running') AND (?1 IS NULL OR m.tenant_id=?1)")?;
        let rows = stmt
            .query_map([tenant.map(|t| t.0)], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    for (raw, closed) in &runs {
        let run = MigrationRunId(
            uuid::Uuid::parse_str(raw).map_err(|_| invalid("invalid journal run identity"))?,
        );
        if !closed {
            close(conn, run)?;
        }
        conn.execute("UPDATE migration_run SET state='partial',finished_at=COALESCE(finished_at,?2) WHERE id=?1 AND state='running'",params![raw,now])?;
    }
    Ok(runs.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn native_journal_freezes_intents_pages_and_survives_recovery_without_inventing_effects()
    {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("native.sqlite");
        let secrets = Arc::new(crate::MemorySecretStore::new());
        let store = MetadataStore::open(&path, secrets.clone()).unwrap();
        store.apply_migrations(false).unwrap();
        store.bootstrap_local("journal owner").unwrap();
        let tenant = TenantId(1);
        let actor = PrincipalId(1);
        let profile = store
            .upsert_connection_profile(
                tenant,
                actor,
                crate::NewConnectionProfile {
                    name: "native fixture".into(),
                    provider_id: Engine::Postgres.provider_id(),
                    semantic_engine: Some(Engine::Postgres),
                    configuration: serde_json::json!({"host":"mock.invalid","user":"fixture"}),
                    credentials: None,
                    credential_mode: crate::CredentialMode::Shared,
                    tags: vec![],
                },
            )
            .await
            .unwrap();
        let statement = |ordinal, ch: char| MigrationStatement {
            ordinal,
            sql: "private-native-sql-sentinel".into(),
            fingerprint: format!("sqlfp:{}", ch.to_string().repeat(64)),
            change_ids: vec![],
            risk: SchemaChangeRisk::Safe,
        };
        let plan = MigrationPlan {
            id: MigrationPlanId(uuid::Uuid::new_v4()),
            run_id: MigrationRunId(uuid::Uuid::new_v4()),
            digest: format!("migfp:{}", "a".repeat(64)),
            diff_digest: "b".repeat(64),
            expected_live_revision: CatalogRevision(1),
            groups: vec![
                MigrationGroup {
                    ordinal: 1,
                    transactional: true,
                    statements: vec![statement(1, 'c'), statement(2, 'd')],
                },
                MigrationGroup {
                    ordinal: 2,
                    transactional: true,
                    statements: vec![statement(1, 'e')],
                },
                MigrationGroup {
                    ordinal: 3,
                    transactional: false,
                    statements: vec![statement(1, 'f')],
                },
                MigrationGroup {
                    ordinal: 4,
                    transactional: true,
                    statements: vec![statement(1, 'a')],
                },
            ],
            rollback_groups: vec![],
            required_acknowledgements: vec![],
            warnings: vec![],
            expires_at: chrono::Utc::now() + chrono::Duration::minutes(5),
        };
        let run = MigrationRun {
            id: plan.run_id,
            plan_id: plan.id,
            session: SessionId(1),
            connection: ConnectionId(2),
            plan_digest: plan.digest.clone(),
            state: MigrationRunState::Running,
            started_at: chrono::Utc::now(),
            finished_at: None,
            outcomes: vec![],
            resulting_catalog_revision: None,
        };
        store
            .put_migration_run(tenant, profile.id, actor, &run)
            .unwrap();
        let read = |store: &MetadataStore| {
            store
                .migration_native_outcomes(tenant, run.id, &MigrationNativePageRequest::default())
                .unwrap()
        };
        let legacy = read(&store);
        assert!(
            !legacy.journal_present && legacy.source.is_none() && legacy.observations.is_empty()
        );
        assert!(store.migration_status().unwrap().minimum_compatible_version < 59);
        let source = MigrationNativeSource {
            profile_id: profile.id.0,
            provider_id: Engine::Postgres.provider_id(),
            session: run.session,
            connection: run.connection,
            generation: uuid::Uuid::new_v4(),
            configuration_sha256: "d".repeat(64),
            database_target: Some("fixture".into()),
        };
        assert_eq!(
            store
                .open_migration_native_journal(tenant, profile.id, actor, &plan, &source)
                .unwrap(),
            1
        );
        assert_eq!(
            store.migration_status().unwrap().minimum_compatible_version,
            59
        );

        assert_eq!(
            store
                .open_migration_native_journal(tenant, profile.id, actor, &plan, &source)
                .unwrap(),
            1
        );
        let mut changed = source.clone();
        changed.generation = uuid::Uuid::new_v4();
        assert!(store
            .open_migration_native_journal(tenant, profile.id, actor, &plan, &changed)
            .is_err());
        let mut changed = plan.clone();
        changed.groups[0].statements[0].fingerprint = format!("sqlfp:{}", "e".repeat(64));
        assert!(store
            .open_migration_native_journal(tenant, profile.id, actor, &changed, &source)
            .is_err());
        let mut changed = plan.clone();
        changed.groups[0].statements[0]
            .sql
            .push_str(" changed exact input");
        assert!(
            store
                .open_migration_native_journal(tenant, profile.id, actor, &changed, &source)
                .is_err(),
            "coarse SQL fingerprints cannot replace exact manifest identity"
        );
        assert!(matches!(
            store.migration_native_outcomes(
                TenantId(2),
                run.id,
                &MigrationNativePageRequest::default()
            ),
            Err(MetadataError::MigrationRunNotFound)
        ));
        assert!(matches!(
            store.begin_migration_native_group(tenant, PrincipalId(2), run.id, 1),
            Err(MetadataError::MigrationRunNotFound)
        ));
        let first = store
            .migration_native_outcomes(
                tenant,
                run.id,
                &MigrationNativePageRequest {
                    limit: 1,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(first.next_offset, Some(1));
        let second = store
            .migration_native_outcomes(
                tenant,
                run.id,
                &MigrationNativePageRequest {
                    expected_revision: Some(first.revision),
                    offset: 1,
                    limit: 1,
                },
            )
            .unwrap();
        assert_eq!(
            second.observations[0].statement.as_ref().unwrap().ordinal,
            2
        );
        assert!(store
            .migration_native_outcomes(
                tenant,
                run.id,
                &MigrationNativePageRequest {
                    offset: 1,
                    ..Default::default()
                }
            )
            .is_err());

        store
            .begin_migration_native_group(tenant, actor, run.id, 1)
            .unwrap();
        assert!(store
            .begin_migration_native_group(tenant, actor, run.id, 1)
            .is_err());
        let mut group = read(&store).observations[0].group.clone();
        group.phase = MigrationNativePhase::Executing;
        group.transaction_id = Some(TxId(7));
        store
            .record_migration_native_group(tenant, actor, run.id, &group)
            .unwrap();
        let mut premature = group.clone();
        premature.phase = MigrationNativePhase::Settled;
        premature.commit_intent = true;
        premature.outcome = Some(ChangeLedgerOutcome::Committed);
        assert!(store
            .record_migration_native_group(tenant, actor, run.id, &premature)
            .is_err());
        let mut premature = group.clone();
        premature.phase = MigrationNativePhase::Committing;
        premature.commit_intent = true;
        assert!(store
            .record_migration_native_group(tenant, actor, run.id, &premature)
            .is_err());
        for (ordinal, count) in [(1, 0), (2, u64::MAX)] {
            store
                .prepare_migration_native_statement(tenant, actor, run.id, 1, ordinal)
                .unwrap();
            assert!(store
                .prepare_migration_native_statement(tenant, actor, run.id, 1, ordinal)
                .is_err());
            let mut statement = read(&store)
                .observations
                .into_iter()
                .find(|o| {
                    o.group.ordinal == 1
                        && o.statement.as_ref().is_some_and(|s| s.ordinal == ordinal)
                })
                .unwrap()
                .statement
                .unwrap();
            statement.native_admitted = Some(true);
            statement.native_completed = Some(true);
            statement.affected_rows = Some(count);
            let mut contradictory = statement.clone();
            contradictory.autocommit_known = true;
            assert!(store
                .record_migration_native_statement(tenant, actor, run.id, 1, &contradictory)
                .is_err());
            store
                .record_migration_native_statement(tenant, actor, run.id, 1, &statement)
                .unwrap();
        }
        assert!(
            read(&store).observations[0].group.outcome.is_none(),
            "statement completion does not prove commit"
        );
        group.phase = MigrationNativePhase::Committing;
        group.commit_intent = true;
        store
            .record_migration_native_group(tenant, actor, run.id, &group)
            .unwrap();
        group.phase = MigrationNativePhase::Settled;
        group.outcome = Some(ChangeLedgerOutcome::Committed);
        store
            .record_migration_native_group(tenant, actor, run.id, &group)
            .unwrap();
        assert!(
            store
                .record_migration_native_group(tenant, actor, run.id, &group)
                .is_err(),
            "settled native groups cannot be rewritten"
        );

        for (ordinal, tx_id, admitted) in [(2, 8, true), (4, 9, false)] {
            store
                .begin_migration_native_group(tenant, actor, run.id, ordinal)
                .unwrap();
            let mut group = read(&store)
                .observations
                .into_iter()
                .find(|o| o.group.ordinal == ordinal)
                .unwrap()
                .group;
            group.phase = MigrationNativePhase::Executing;
            group.transaction_id = Some(TxId(tx_id));
            store
                .record_migration_native_group(tenant, actor, run.id, &group)
                .unwrap();
            store
                .prepare_migration_native_statement(tenant, actor, run.id, ordinal, 1)
                .unwrap();
            if admitted {
                let mut statement = read(&store)
                    .observations
                    .into_iter()
                    .find(|o| o.group.ordinal == ordinal)
                    .unwrap()
                    .statement
                    .unwrap();
                statement.native_admitted = Some(true);
                store
                    .record_migration_native_statement(tenant, actor, run.id, ordinal, &statement)
                    .unwrap();
            }
        }
        store
            .begin_migration_native_group(tenant, actor, run.id, 3)
            .unwrap();
        store
            .prepare_migration_native_statement(tenant, actor, run.id, 3, 1)
            .unwrap();
        let mut nontransactional = read(&store)
            .observations
            .into_iter()
            .find(|o| o.group.ordinal == 3)
            .unwrap();
        let mut refused = nontransactional.statement.take().unwrap();
        refused.native_admitted = Some(false);
        refused.result_code = Some("forbidden".into());
        store
            .record_migration_native_statement(tenant, actor, run.id, 3, &refused)
            .unwrap();
        nontransactional.group.phase = MigrationNativePhase::Settled;
        store
            .record_migration_native_group(tenant, actor, run.id, &nontransactional.group)
            .unwrap();
        assert!(matches!(
            store.migration_native_outcomes(
                tenant,
                run.id,
                &MigrationNativePageRequest {
                    expected_revision: Some(first.revision),
                    offset: 1,
                    limit: 1
                }
            ),
            Err(MetadataError::MigrationNativeRevisionConflict)
        ));

        // Exercise the actual offline recovery boundaries with an unresolved
        // native intent, rather than only calling startup recovery directly.
        let snapshot_path = root.path().join("snapshot.sqlite");
        store.backup_database_to(&snapshot_path).unwrap();
        let snapshot = MetadataStore::open(&snapshot_path, secrets.clone()).unwrap();
        snapshot.sanitize_workspace_backup_snapshot().unwrap();
        let sanitized = read(&snapshot);
        assert!(sanitized.closed);
        assert_eq!(
            sanitized.observations[0].group.outcome,
            Some(ChangeLedgerOutcome::Committed)
        );
        assert_eq!(
            sanitized
                .observations
                .iter()
                .find(|o| o.group.ordinal == 4)
                .unwrap()
                .statement
                .as_ref()
                .unwrap()
                .native_admitted,
            None
        );
        assert!(!read(&store).closed);

        let destination_path = root.path().join("restored.sqlite");
        store.backup_database_to(&destination_path).unwrap();
        {
            let destination = MetadataStore::open(&destination_path, secrets.clone()).unwrap();
            let conn = destination.conn().unwrap();
            conn.execute(
                "DELETE FROM migration_run WHERE id=?1",
                [run.id.to_string()],
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 0).unwrap();
        }
        crate::merge_tenant_snapshot(&destination_path, &path, tenant).unwrap();
        let restored = MetadataStore::open(&destination_path, secrets.clone()).unwrap();
        let restored_page = read(&restored);
        assert!(restored_page.closed);
        assert_eq!(restored_page.source, sanitized.source);
        assert_eq!(restored_page.observations, sanitized.observations);
        assert_eq!(
            restored.get_migration_run(tenant, run.id).unwrap().state,
            MigrationRunState::Partial
        );
        assert_eq!(
            restored
                .migration_status()
                .unwrap()
                .minimum_compatible_version,
            59
        );
        assert!(!read(&store).closed);
        drop(store);

        let store = MetadataStore::open(&path, secrets).unwrap();
        store.apply_migrations(false).unwrap();
        assert_eq!(store.interrupt_migration_native_journals().unwrap(), 1);
        assert_eq!(store.interrupt_migration_native_journals().unwrap(), 0);
        let recovered = read(&store);
        assert!(recovered.closed);
        assert_eq!(
            recovered.observations[0].group.outcome,
            Some(ChangeLedgerOutcome::Committed)
        );
        assert_eq!(
            recovered.observations[0].group.transaction_id,
            Some(TxId(7))
        );
        assert_eq!(
            recovered.observations[0]
                .statement
                .as_ref()
                .unwrap()
                .affected_rows,
            Some(0)
        );
        assert_eq!(
            recovered.observations[1]
                .statement
                .as_ref()
                .unwrap()
                .affected_rows,
            Some(u64::MAX)
        );
        for observation in &recovered.observations {
            match observation.group.ordinal {
                2 => {
                    assert_eq!(
                        observation.group.outcome,
                        Some(ChangeLedgerOutcome::OutcomeUnknown)
                    );
                    assert_eq!(
                        observation.statement.as_ref().unwrap().native_admitted,
                        Some(true)
                    );
                }
                3 => {
                    assert!(observation.group.outcome.is_none());
                    assert_eq!(
                        observation.statement.as_ref().unwrap().native_admitted,
                        Some(false)
                    );
                }
                4 => {
                    assert_eq!(
                        observation.group.outcome,
                        Some(ChangeLedgerOutcome::OutcomeUnknown)
                    );
                    assert!(observation
                        .statement
                        .as_ref()
                        .unwrap()
                        .native_admitted
                        .is_none());
                }
                _ => {}
            }
        }
        assert!(store
            .prepare_migration_native_statement(tenant, actor, run.id, 4, 1)
            .is_err());
        assert_eq!(
            store.get_migration_run(tenant, run.id).unwrap().state,
            MigrationRunState::Partial
        );
        assert_eq!(
            store
                .close_migration_native_journal(tenant, actor, run.id)
                .unwrap(),
            recovered.revision
        );
        assert!(!serde_json::to_string(&recovered)
            .unwrap()
            .contains("private-native-sql-sentinel"));
        assert_eq!(
            store.migration_status().unwrap().minimum_compatible_version,
            59
        );
        // Crash after journal closure but before the workflow's terminal write:
        // recover the parent without rewriting native evidence or its revision.
        store
            .conn()
            .unwrap()
            .execute(
                "UPDATE migration_run SET state='running',finished_at=NULL WHERE id=?1",
                [run.id.to_string()],
            )
            .unwrap();
        assert_eq!(store.interrupt_migration_native_journals().unwrap(), 1);
        assert_eq!(store.interrupt_migration_native_journals().unwrap(), 0);
        let after_parent_recovery = read(&store);
        assert_eq!(after_parent_recovery.revision, recovered.revision);
        assert_eq!(after_parent_recovery.observations, recovered.observations);
        assert_eq!(
            store.get_migration_run(tenant, run.id).unwrap().state,
            MigrationRunState::Partial
        );
    }
}
