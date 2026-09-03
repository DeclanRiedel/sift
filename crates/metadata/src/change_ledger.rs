use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sift_protocol::{
    ChangeIdentityConfidence, ChangeIdentitySource, ChangeLedgerEntry, ChangeLedgerFilter,
    ChangeLedgerOperation, ChangeLedgerOutcome, ChangeLedgerPage, ChangeLedgerPolicy,
    VersionedExecutionContext, WorkspaceCheckpointId, WorkspaceId, WorkspacePath,
    WorkspaceRevision,
};

use crate::{now_text, parse_time_sql, MetadataError, MetadataStore, Result};

const GENESIS_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Debug, Clone, Serialize)]
pub struct NewChangeLedgerEntry {
    pub tenant_id: Option<i64>,
    pub room_id: Option<i64>,
    pub connection_profile_id: Option<i64>,
    pub database_target: Option<String>,
    pub operation: ChangeLedgerOperation,
    pub affected_object: Option<String>,
    pub row_count: Option<i64>,
    pub sql_fingerprint: Option<String>,
    pub row_identity_fingerprint: Option<String>,
    pub transaction_id: Option<String>,
    pub correlation_id: Option<String>,
    pub workspace_id: Option<WorkspaceId>,
    pub workspace_revision: Option<WorkspaceRevision>,
    pub checkpoint_id: Option<WorkspaceCheckpointId>,
    pub workspace_path: Option<WorkspacePath>,
    pub git_commit: Option<String>,
    pub source_workflow: String,
    pub authored_by: Option<i64>,
    pub approved_by: Option<i64>,
    pub executed_by: i64,
    pub database_actor: Option<String>,
    pub outcome: ChangeLedgerOutcome,
    pub result_code: Option<String>,
    pub identity_source: ChangeIdentitySource,
    pub identity_confidence: ChangeIdentityConfidence,
}

#[derive(Debug, Clone)]
pub struct ValidatedVersionedExecution {
    pub room_id: i64,
    pub workspace_id: WorkspaceId,
    pub workspace_revision: WorkspaceRevision,
    pub checkpoint_id: Option<WorkspaceCheckpointId>,
    pub workspace_path: Option<WorkspacePath>,
    pub git_commit: Option<String>,
    pub authored_by: Option<i64>,
    pub source_workflow: String,
}

impl ValidatedVersionedExecution {
    pub fn context(&self) -> VersionedExecutionContext {
        VersionedExecutionContext {
            workspace_id: self.workspace_id,
            workspace_revision: self.workspace_revision,
            checkpoint_id: self.checkpoint_id,
            path: self.workspace_path.clone(),
            git_commit: self.git_commit.clone(),
            source_workflow: Some(self.source_workflow.clone()),
        }
    }
}

impl MetadataStore {
    pub fn validate_versioned_execution(
        &self,
        actor: crate::PrincipalId,
        source: &VersionedExecutionContext,
    ) -> Result<ValidatedVersionedExecution> {
        let workspace = self.get_workspace_for_principal(source.workspace_id, actor, false)?;
        if workspace.revision != source.workspace_revision {
            return Err(MetadataError::WorkspaceRevisionConflict {
                expected: source.workspace_revision.0,
                current: workspace.revision.0,
            });
        }
        let conn = self.conn()?;
        if let Some(path) = &source.path {
            let exists: bool = conn.query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM workspace_node
                   WHERE workspace_id = ?1 AND path = ?2 AND kind = 'sql_document'
                 )",
                params![source.workspace_id.0, path.0],
                |row| row.get(0),
            )?;
            if !exists {
                return Err(MetadataError::InvalidWorkspacePath);
            }
        }
        let checkpoint = source
            .checkpoint_id
            .map(|checkpoint| {
                conn.query_row(
                    "SELECT workspace_id, workspace_revision, created_by
                     FROM workspace_checkpoint WHERE id = ?1",
                    [checkpoint.0],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    },
                )
                .optional()?
                .ok_or(MetadataError::WorkspaceCheckpointNotFound(checkpoint))
            })
            .transpose()?;
        if let Some((workspace_id, revision, _)) = checkpoint {
            if workspace_id != source.workspace_id.0
                || u64::try_from(revision).ok() != Some(source.workspace_revision.0)
            {
                return Err(MetadataError::InvalidWorkspaceCheckpoint);
            }
        }
        let linked = conn
            .query_row(
                "SELECT rc.commit_oid, wc.created_by, rc.checkpoint_id
                 FROM repository_commit rc
                 JOIN repository_binding rb ON rb.id = rc.binding_id
                 JOIN workspace_checkpoint wc ON wc.id = rc.checkpoint_id
                 WHERE rb.workspace_id = ?1
                   AND (?2 IS NULL OR rc.checkpoint_id = ?2)
                   AND (?3 IS NULL OR rc.commit_oid = ?3)
                 ORDER BY rc.id DESC LIMIT 1",
                params![
                    source.workspace_id.0,
                    source.checkpoint_id.map(|value| value.0),
                    source.git_commit,
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?;
        if source.checkpoint_id.is_some() && linked.is_none() && source.git_commit.is_some() {
            return Err(MetadataError::InvalidRepositoryBinding);
        }
        if source.git_commit.is_some() && linked.is_none() {
            let head_matches: bool = conn.query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM repository_binding
                   WHERE workspace_id = ?1 AND head = ?2
                 )",
                params![source.workspace_id.0, source.git_commit],
                |row| row.get(0),
            )?;
            if !head_matches {
                return Err(MetadataError::InvalidRepositoryBinding);
            }
        }
        Ok(ValidatedVersionedExecution {
            room_id: workspace.room_id.0,
            workspace_id: source.workspace_id,
            workspace_revision: source.workspace_revision,
            checkpoint_id: source
                .checkpoint_id
                .or_else(|| linked.as_ref().map(|(_, _, id)| WorkspaceCheckpointId(*id))),
            workspace_path: source.path.clone(),
            git_commit: source
                .git_commit
                .clone()
                .or_else(|| linked.as_ref().map(|(commit, _, _)| commit.clone())),
            authored_by: checkpoint
                .map(|(_, _, actor)| actor)
                .or_else(|| linked.as_ref().map(|(_, actor, _)| *actor)),
            source_workflow: source
                .source_workflow
                .clone()
                .unwrap_or_else(|| "workspace_editor".into()),
        })
    }

    pub fn append_change_ledger(&self, input: NewChangeLedgerEntry) -> Result<ChangeLedgerEntry> {
        validate_input(&input)?;
        let at = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous_hash = tx
            .query_row(
                "SELECT entry_hash FROM database_change_ledger ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .unwrap_or_else(|| GENESIS_HASH.into());
        let entry_hash = entry_hash(&at, &previous_hash, &input)?;
        tx.execute(
            "INSERT INTO database_change_ledger
             (at, tenant_id, room_id, connection_profile_id, database_target,
              operation_kind, affected_object, row_count, sql_fingerprint,
              row_identity_fingerprint, transaction_id, correlation_id, workspace_id,
              workspace_revision, checkpoint_id, workspace_path, git_commit,
              source_workflow, authored_by, approved_by, executed_by, database_actor, outcome,
              result_code, identity_source, identity_confidence, previous_hash, entry_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                     ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24,
                     ?25, ?26, ?27, ?28)",
            params![
                at,
                input.tenant_id,
                input.room_id,
                input.connection_profile_id,
                input.database_target,
                enum_name(input.operation)?,
                input.affected_object,
                input.row_count,
                input.sql_fingerprint,
                input.row_identity_fingerprint,
                input.transaction_id,
                input.correlation_id,
                input.workspace_id.map(|value| value.0),
                input
                    .workspace_revision
                    .map(|value| i64::try_from(value.0))
                    .transpose()
                    .map_err(|_| MetadataError::InvalidWorkspaceNode)?,
                input.checkpoint_id.map(|value| value.0),
                input.workspace_path.map(|value| value.0),
                input.git_commit,
                input.source_workflow,
                input.authored_by,
                input.approved_by,
                input.executed_by,
                input.database_actor,
                enum_name(input.outcome)?,
                input.result_code,
                enum_name(input.identity_source)?,
                enum_name(input.identity_confidence)?,
                previous_hash,
                entry_hash,
            ],
        )?;
        let id = tx.last_insert_rowid();
        let entry = tx.query_row(
            &format!("{} WHERE id = ?1", select_change_ledger()),
            [id],
            change_ledger_from_row,
        )?;
        tx.commit()?;
        Ok(entry)
    }

    pub fn change_ledger(&self, filter: &ChangeLedgerFilter) -> Result<ChangeLedgerPage> {
        let limit = filter.limit.unwrap_or(100).clamp(1, 500);
        let operation = filter.operation.map(enum_name).transpose()?;
        let from = filter.from.map(|value| value.to_rfc3339());
        let to = filter.to.map(|value| value.to_rfc3339());
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&format!(
            "{} WHERE (?1 IS NULL OR tenant_id = ?1)
               AND (?2 IS NULL OR connection_profile_id = ?2)
               AND (?3 IS NULL OR database_target = ?3)
               AND (?4 IS NULL OR affected_object = ?4)
               AND (?5 IS NULL OR executed_by = ?5)
               AND (?6 IS NULL OR operation_kind = ?6)
               AND (?7 IS NULL OR at >= ?7)
               AND (?8 IS NULL OR at <= ?8)
               AND (?9 IS NULL OR git_commit = ?9)
               AND (?10 IS NULL OR id < ?10)
             ORDER BY id DESC LIMIT ?11",
            select_change_ledger()
        ))?;
        let rows = stmt
            .query_map(
                params![
                    filter.tenant_id,
                    filter.connection_profile_id,
                    filter.database_target,
                    filter.affected_object,
                    filter.executed_by,
                    operation,
                    from,
                    to,
                    filter.git_commit,
                    filter.before_id,
                    limit + 1,
                ],
                change_ledger_from_row,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut entries = rows;
        let has_more = entries.len() > limit as usize;
        entries.truncate(limit as usize);
        let next_before_id = has_more.then(|| entries.last().expect("non-empty page").id);
        let chain_verified = verify_chain_locked(&conn)?;
        Ok(ChangeLedgerPage {
            entries,
            next_before_id,
            chain_verified,
        })
    }

    pub fn set_change_ledger_policy(
        &self,
        tenant_id: i64,
        retention_days: u32,
        external_sink: Option<&str>,
        actor: i64,
    ) -> Result<ChangeLedgerPolicy> {
        if retention_days < 30 || external_sink.is_some_and(|value| value != "pull:csv") {
            return Err(MetadataError::InvalidWorkspaceNode);
        }
        let now = now_text();
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO database_change_ledger_policy
             (tenant_id, retention_days, external_sink, updated_by, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(tenant_id) DO UPDATE SET
               retention_days = excluded.retention_days,
               external_sink = excluded.external_sink,
               updated_by = excluded.updated_by,
               updated_at = excluded.updated_at",
            params![tenant_id, retention_days, external_sink, actor, now],
        )?;
        change_ledger_policy_locked(&conn, tenant_id)
    }

    pub fn change_ledger_policy(&self, tenant_id: i64) -> Result<ChangeLedgerPolicy> {
        let conn = self.conn()?;
        change_ledger_policy_locked(&conn, tenant_id)
    }
}

fn validate_input(input: &NewChangeLedgerEntry) -> Result<()> {
    if input.executed_by <= 0
        || input.source_workflow.is_empty()
        || input.source_workflow.len() > 100
        || input.row_count.is_some_and(|count| count < 0)
        || input
            .database_actor
            .as_ref()
            .is_some_and(|value| value.is_empty() || value.len() > 256)
        || input
            .sql_fingerprint
            .as_ref()
            .is_some_and(|value| value.len() > 128)
        || input
            .row_identity_fingerprint
            .as_ref()
            .is_some_and(|value| value.len() > 128)
    {
        return Err(MetadataError::InvalidWorkspaceNode);
    }
    Ok(())
}

fn enum_name<T: Serialize>(value: T) -> Result<String> {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or(MetadataError::InvalidWorkspaceNode)
}

fn enum_value<T: serde::de::DeserializeOwned>(
    field: &'static str,
    value: String,
) -> rusqlite::Result<T> {
    serde_json::from_value(serde_json::Value::String(value.clone()))
        .map_err(|_| crate::sql_conversion_error(MetadataError::InvalidEnum { field, value }))
}

fn entry_hash(at: &str, previous_hash: &str, input: &NewChangeLedgerEntry) -> Result<String> {
    let payload = serde_json::to_vec(input)
        .map_err(|error| MetadataError::InvalidMigrationHistory(error.to_string()))?;
    let mut hasher = Sha256::new();
    hasher.update(at.as_bytes());
    hasher.update([0]);
    hasher.update(previous_hash.as_bytes());
    hasher.update([0]);
    hasher.update(payload);
    Ok(format!("{:x}", hasher.finalize()))
}

fn select_change_ledger() -> &'static str {
    "SELECT id, at, tenant_id, room_id, connection_profile_id, database_target,
            operation_kind, affected_object, row_count, sql_fingerprint,
            row_identity_fingerprint, transaction_id, correlation_id, workspace_id,
            workspace_revision, checkpoint_id, workspace_path, git_commit,
            source_workflow, authored_by, approved_by, executed_by, database_actor, outcome,
            result_code, identity_source, identity_confidence, previous_hash, entry_hash
     FROM database_change_ledger"
}

fn change_ledger_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChangeLedgerEntry> {
    let revision = row.get::<_, Option<i64>>(14)?;
    Ok(ChangeLedgerEntry {
        id: row.get(0)?,
        at: parse_time_sql(row.get(1)?)?,
        tenant_id: row.get(2)?,
        room_id: row.get(3)?,
        connection_profile_id: row.get(4)?,
        database_target: row.get(5)?,
        operation: enum_value("change_ledger.operation_kind", row.get(6)?)?,
        affected_object: row.get(7)?,
        row_count: row.get(8)?,
        sql_fingerprint: row.get(9)?,
        row_identity_fingerprint: row.get(10)?,
        transaction_id: row.get(11)?,
        correlation_id: row.get(12)?,
        workspace_id: row.get::<_, Option<i64>>(13)?.map(WorkspaceId),
        workspace_revision: revision
            .map(|value| {
                u64::try_from(value)
                    .map(WorkspaceRevision)
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(14, value))
            })
            .transpose()?,
        checkpoint_id: row.get::<_, Option<i64>>(15)?.map(WorkspaceCheckpointId),
        workspace_path: row.get::<_, Option<String>>(16)?.map(WorkspacePath),
        git_commit: row.get(17)?,
        source_workflow: row.get(18)?,
        authored_by: row.get(19)?,
        approved_by: row.get(20)?,
        executed_by: row.get(21)?,
        database_actor: row.get(22)?,
        outcome: enum_value("change_ledger.outcome", row.get(23)?)?,
        result_code: row.get(24)?,
        identity_source: enum_value("change_ledger.identity_source", row.get(25)?)?,
        identity_confidence: enum_value("change_ledger.identity_confidence", row.get(26)?)?,
        previous_hash: row.get(27)?,
        entry_hash: row.get(28)?,
    })
}

fn verify_chain_locked(conn: &rusqlite::Connection) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("{} ORDER BY id ASC", select_change_ledger()))?;
    let entries = stmt
        .query_map([], change_ledger_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut previous = GENESIS_HASH.to_string();
    for entry in entries {
        let expected = entry_hash(
            &entry.at.to_rfc3339(),
            &previous,
            &NewChangeLedgerEntry {
                tenant_id: entry.tenant_id,
                room_id: entry.room_id,
                connection_profile_id: entry.connection_profile_id,
                database_target: entry.database_target.clone(),
                operation: entry.operation,
                affected_object: entry.affected_object.clone(),
                row_count: entry.row_count,
                sql_fingerprint: entry.sql_fingerprint.clone(),
                row_identity_fingerprint: entry.row_identity_fingerprint.clone(),
                transaction_id: entry.transaction_id.clone(),
                correlation_id: entry.correlation_id.clone(),
                workspace_id: entry.workspace_id,
                workspace_revision: entry.workspace_revision,
                checkpoint_id: entry.checkpoint_id,
                workspace_path: entry.workspace_path.clone(),
                git_commit: entry.git_commit.clone(),
                source_workflow: entry.source_workflow.clone(),
                authored_by: entry.authored_by,
                approved_by: entry.approved_by,
                executed_by: entry.executed_by,
                database_actor: entry.database_actor.clone(),
                outcome: entry.outcome,
                result_code: entry.result_code.clone(),
                identity_source: entry.identity_source,
                identity_confidence: entry.identity_confidence,
            },
        )?;
        if entry.previous_hash != previous || entry.entry_hash != expected {
            return Ok(false);
        }
        previous = entry.entry_hash;
    }
    Ok(true)
}

fn change_ledger_policy_locked(
    conn: &rusqlite::Connection,
    tenant_id: i64,
) -> Result<ChangeLedgerPolicy> {
    conn.query_row(
        "SELECT tenant_id, retention_days, external_sink, updated_by, updated_at
         FROM database_change_ledger_policy WHERE tenant_id = ?1",
        [tenant_id],
        |row| {
            let days = row.get::<_, i64>(1)?;
            Ok(ChangeLedgerPolicy {
                tenant_id: row.get(0)?,
                retention_days: u32::try_from(days)
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(1, days))?,
                external_sink: row.get(2)?,
                updated_by: row.get(3)?,
                updated_at: parse_time_sql(row.get(4)?)?,
            })
        },
    )
    .optional()?
    .map_or_else(
        || {
            Ok(ChangeLedgerPolicy {
                tenant_id,
                retention_days: 2555,
                external_sink: None,
                updated_by: 0,
                updated_at: DateTime::<Utc>::UNIX_EPOCH,
            })
        },
        Ok,
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::MemorySecretStore;

    fn entry(actor: i64, operation: ChangeLedgerOperation) -> NewChangeLedgerEntry {
        NewChangeLedgerEntry {
            tenant_id: Some(1),
            room_id: Some(2),
            connection_profile_id: Some(3),
            database_target: Some("app".into()),
            operation,
            affected_object: Some("public.accounts".into()),
            row_count: Some(1),
            sql_fingerprint: Some("sha256:sql".into()),
            row_identity_fingerprint: Some("sha256:key".into()),
            transaction_id: None,
            correlation_id: Some("request-1".into()),
            workspace_id: None,
            workspace_revision: None,
            checkpoint_id: None,
            workspace_path: None,
            git_commit: None,
            source_workflow: "grid".into(),
            authored_by: None,
            approved_by: None,
            executed_by: actor,
            database_actor: None,
            outcome: ChangeLedgerOutcome::Committed,
            result_code: None,
            identity_source: ChangeIdentitySource::Sift,
            identity_confidence: ChangeIdentityConfidence::Authenticated,
        }
    }

    #[test]
    fn ledger_is_hash_chained_filterable_and_contains_no_values() {
        let store = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        store.bootstrap_local("local").unwrap();
        store
            .append_change_ledger(entry(1, ChangeLedgerOperation::GridUpdate))
            .unwrap();
        store
            .append_change_ledger(entry(1, ChangeLedgerOperation::GridDelete))
            .unwrap();
        let page = store
            .change_ledger(&ChangeLedgerFilter {
                operation: Some(ChangeLedgerOperation::GridUpdate),
                ..Default::default()
            })
            .unwrap();
        assert!(page.chain_verified);
        assert_eq!(page.entries.len(), 1);
        let encoded = serde_json::to_string(&page).unwrap();
        assert!(!encoded.contains("before_value"));
        assert!(!encoded.contains("after_value"));
    }

    #[test]
    fn sqlite_rejects_ledger_update_and_delete() {
        let store = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        store.bootstrap_local("local").unwrap();
        let appended = store
            .append_change_ledger(entry(1, ChangeLedgerOperation::DirectDml))
            .unwrap();
        let conn = store.conn().unwrap();
        assert!(conn
            .execute(
                "UPDATE database_change_ledger SET outcome = 'failed' WHERE id = ?1",
                [appended.id]
            )
            .is_err());
        assert!(conn
            .execute(
                "DELETE FROM database_change_ledger WHERE id = ?1",
                [appended.id]
            )
            .is_err());
    }

    #[test]
    fn ledger_policy_only_accepts_explicit_pull_export_sink() {
        let store = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        store.bootstrap_local("local").unwrap();

        assert!(store.set_change_ledger_policy(1, 29, None, 1).is_err());
        assert!(store
            .set_change_ledger_policy(1, 90, Some("https://audit.example/upload"), 1,)
            .is_err());

        let policy = store
            .set_change_ledger_policy(1, 90, Some("pull:csv"), 1)
            .unwrap();
        assert_eq!(policy.retention_days, 90);
        assert_eq!(policy.external_sink.as_deref(), Some("pull:csv"));
    }
}
