//! Offline same-ID tenant recovery into a private destination snapshot.
use crate::{MetadataError, Result, TenantId};
use rusqlite::{params, Connection, OptionalExtension};
use std::{collections::BTreeMap, path::Path};

#[derive(Debug, serde::Serialize)]
pub struct TenantMergeReport {
    pub tenant_id: i64,
    pub removed_rows: BTreeMap<String, usize>,
    pub restored_rows: BTreeMap<String, usize>,
    pub copied_secrets: usize,
}

// Deliberately not serializable: reports expose counts, never secret handles.
pub struct TenantSecretCopy {
    pub namespace: String,
    pub source: String,
    pub destination: String,
}
pub struct TenantMerge {
    pub report: TenantMergeReport,
    pub secrets: Vec<TenantSecretCopy>,
}

const OWNED: &[&str] = &[
    "tenant",
    "membership",
    "connection_profile",
    "connection_credential",
    "room",
    "room_member",
    "document",
    "document_update",
    "room_attachment",
    "saved_query",
    "query_history",
    "catalog_snapshot",
    "migration_run",
    "migration_native_journal",
    "migration_native_group",
    "migration_native_statement",
    "plan_capture",
    "benchmark_run",
    "benchmark_retention",
    "benchmark_definition",
    "workspace",
    "workspace_node",
    "workspace_checkpoint",
    "workspace_checkpoint_node",
    "projection_binding",
    "projection_file_state",
    "ddl_source",
    "ddl_source_root",
    "ddl_source_mapping",
    "repository_binding",
    "repository_commit",
    "repository_principal_credential",
    "repository_hosting_credential",
    "run_configuration",
    "run_execution",
    "run_step_result",
    "run_log",
    "run_schedule",
    "schedule_occurrence",
    "transfer_recipe",
    "workspace_artifact",
    "vault",
    "vault_grant",
    "vault_item",
    "vault_item_version",
    "vault_connection_binding",
    "sql_snippet",
    "ai_chat",
    "ai_delegation_tenant_policy",
    "ai_delegation_review",
    "ai_pending_turn_cancel",
    "ai_run",
    "ai_run_delegation",
    "ai_delegation_call",
    "ai_delegation_rule_usage",
    "ai_delegation_action",
    "ai_run_event",
    "ai_sql_execution",
    "ai_execute_run_source",
    "ai_proposal",
    "ai_tenant_retention",
    "ai_room_publication",
    "ai_proposal_review",
    "ai_managed_review_attempt",
    "ai_database_execution",
    "ai_query_execution",
    "ai_extension_execution",
    "ai_proposal_apply",
    "ai_external_source",
    "ai_external_room_grant",
    "ai_content_cleanup",
];
const DISCARD: &[&str] = &[
    "ai_content_cleanup",
    "projection_file_state",
    "repository_principal_credential",
    "repository_hosting_credential",
    "workspace_artifact",
    "ai_chat",
    "ai_delegation_review",
    "ai_run_delegation",
    "ai_delegation_call",
    "ai_delegation_rule_usage",
    "ai_delegation_action",
    "ai_pending_turn_cancel",
    "ai_run",
    "ai_run_event",
    "ai_sql_execution",
    "ai_execute_run_source",
    "ai_proposal",
    "ai_managed_review_attempt",
];
const PRESERVED: &[&str] = &[
    "principal",
    "api_token",
    "principal_key",
    "keypair_challenge",
    "auth_identity",
    "github_allowlist",
    "auth_session",
    "auth_access_token",
    "auth_refresh_token",
    "oauth_login_attempt",
    "tenant_invitation",
    "password_reset_token",
    "tenant_limit_override",
    "ssh_proxy_capability",
    "operation_approval",
    "operation_audit",
    "refinery_schema_history",
    "document_id_allocator",
    "workspace_content_blob",
    "extension_package",
    "extension_selection",
    "extension_contribution",
    "extension_grant",
    "extension_tenant_allowlist",
    "extension_publisher_key",
    "extension_storage_namespace",
    "extension_storage_blob",
    "extension_storage_entry",
    "instance_manifest_state",
    "instance_managed_resource",
    "instance_credential_slot",
    "instance_credential_consumer",
    "extension_execution",
    "database_change_ledger",
    "database_change_ledger_policy",
    "vault_secret_cleanup_queue",
    "saved_query_fts",
    "saved_query_fts_data",
    "saved_query_fts_idx",
    "saved_query_fts_docsize",
    "saved_query_fts_config",
];
fn invalid(message: impl Into<String>) -> MetadataError {
    MetadataError::InvalidTenantRestore(message.into())
}
fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn scope(table: &str, db: &str, tenant: i64) -> String {
    let child = |column: &str, parent: &str, key: &str| {
        format!(
            "{} IN (SELECT {} FROM {db}.{} WHERE {})",
            quote(column),
            quote(key),
            quote(parent),
            scope(parent, db, tenant)
        )
    };
    match table {
        "migration_native_journal" | "migration_native_group" | "migration_native_statement" => {
            child("run_id", "migration_run", "id")
        }
        "tenant" => format!("id={tenant}"),
        "membership"
        | "connection_profile"
        | "room"
        | "saved_query"
        | "catalog_snapshot"
        | "migration_run"
        | "plan_capture"
        | "benchmark_run"
        | "benchmark_retention"
        | "benchmark_definition"
        | "vault"
        | "sql_snippet"
        | "ai_chat"
        | "ai_room_publication"
        | "ai_proposal_review"
        | "ai_proposal_apply"
        | "ai_tenant_retention"
        | "ai_delegation_tenant_policy"
        | "ai_delegation_review"
        | "ai_external_source"
        | "ai_external_room_grant"
        | "ai_content_cleanup"
        | "ai_sql_execution"
        | "ai_database_execution"
        | "ai_query_execution"
        | "ai_extension_execution"
        | "ai_execute_run_source" => {
            format!("tenant_id={tenant}")
        }
        "connection_credential" => child("connection_profile_id", "connection_profile", "id"),
        "document_update" => child("document_id", "document", "id"),
        "room_member" | "document" | "room_attachment" | "workspace" => {
            child("room_id", "room", "id")
        }
        "query_history" => format!(
            "({}) OR ({})",
            child("room_id", "room", "id"),
            child("connection_profile_id", "connection_profile", "id")
        ),
        "workspace_node"
        | "workspace_checkpoint"
        | "projection_binding"
        | "ddl_source"
        | "repository_binding"
        | "run_configuration"
        | "transfer_recipe"
        | "workspace_artifact" => child("workspace_id", "workspace", "id"),
        "workspace_checkpoint_node" => child("checkpoint_id", "workspace_checkpoint", "id"),
        "projection_file_state" => child("binding_id", "projection_binding", "id"),
        "ddl_source_root" | "ddl_source_mapping" => child("source_id", "ddl_source", "id"),
        "repository_commit"
        | "repository_principal_credential"
        | "repository_hosting_credential" => child("binding_id", "repository_binding", "id"),
        "run_execution" | "run_schedule" => child("configuration_id", "run_configuration", "id"),
        "run_step_result" | "run_log" => child("run_id", "run_execution", "id"),
        "schedule_occurrence" => child("schedule_id", "run_schedule", "id"),
        "vault_grant" | "vault_item" => child("vault_id", "vault", "id"),
        "vault_item_version" | "vault_connection_binding" => child("item_id", "vault_item", "id"),
        "ai_managed_review_attempt" => child("proposal_id", "ai_proposal", "id"),
        "ai_pending_turn_cancel" | "ai_run" | "ai_proposal" => child("chat_id", "ai_chat", "id"),
        "ai_run_event" | "ai_run_delegation" => child("run_id", "ai_run", "id"),
        "ai_delegation_call" | "ai_delegation_rule_usage" | "ai_delegation_action" => {
            child("run_id", "ai_run_delegation", "run_id")
        }
        _ => unreachable!("all ownership rules are explicit"),
    }
}

fn selected(db: &str, table: &str) -> String {
    quote(&format!("selected_{db}_{table}"))
}
type SchemaRow = (String, String, String, Option<String>);
fn schema(conn: &Connection, db: &str) -> Result<Vec<SchemaRow>> {
    Ok(conn.prepare(&format!("SELECT type,name,tbl_name,sql FROM {db}.sqlite_schema WHERE name NOT GLOB 'sqlite_*' ORDER BY type,name"))?.query_map([],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)))?.collect::<rusqlite::Result<_>>()?)
}

/// The caller owns staging, exclusive maintenance, portable secrets and install.
/// This function must never be used against the serving metadata database.
pub fn merge_tenant_snapshot(
    destination: &Path,
    source: &Path,
    tenant: TenantId,
) -> Result<TenantMerge> {
    merge_tenant_snapshot_inner(destination, source, tenant, false)
}

/// Trusted recovery coordinator only: callers must validate and stage referenced
/// AI bodies and portable content keys before atomically installing the result.
pub fn merge_tenant_snapshot_with_ai_content(
    destination: &Path,
    source: &Path,
    tenant: TenantId,
) -> Result<TenantMerge> {
    merge_tenant_snapshot_inner(destination, source, tenant, true)
}

fn merge_tenant_snapshot_inner(
    destination: &Path,
    source: &Path,
    tenant: TenantId,
    coordinated_ai: bool,
) -> Result<TenantMerge> {
    if tenant.0 <= 0 {
        return Err(invalid("tenant ID must be positive"));
    }
    let mut conn =
        Connection::open_with_flags(destination, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.execute(
        "ATTACH DATABASE ?1 AS source",
        params![source.to_string_lossy()],
    )?;
    let source_floor: u32 = conn.pragma_query_value(
        Some(rusqlite::DatabaseName::Attached("source")),
        "user_version",
        |row| row.get(0),
    )?;
    let target_floor: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let reader_floor = source_floor.max(target_floor);
    let latest_reader = crate::migrations::migrations::runner()
        .get_migrations()
        .iter()
        .map(refinery::Migration::version)
        .max()
        .unwrap_or(0);
    if reader_floor > latest_reader {
        return Err(invalid("snapshot requires a newer metadata reader"));
    }
    let source_schema = schema(&conn, "source")?;
    if schema(&conn, "main")? != source_schema {
        return Err(invalid("source and destination schemas differ"));
    }
    for (kind, name, _, _) in &source_schema {
        if kind == "table" && !OWNED.contains(&name.as_str()) && !PRESERVED.contains(&name.as_str())
        {
            return Err(invalid(format!("unclassified table: {name}")));
        }
    }
    let tenant_row = |db: &str| -> Result<Option<(String, String)>> {
        Ok(conn
            .query_row(
                &format!("SELECT name,kind FROM {db}.tenant WHERE id=?1"),
                params![tenant.0],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?)
    };
    let source_tenant =
        tenant_row("source")?.ok_or_else(|| invalid("source tenant does not exist"))?;
    if tenant_row("main")?.is_some_and(|row| row != source_tenant) {
        return Err(invalid(
            "target tenant ID belongs to a different name or kind",
        ));
    }
    for db in ["main", "source"] {
        let contains_ai_chat: bool = conn.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM {db}.ai_chat WHERE tenant_id=?1 UNION ALL SELECT 1 FROM {db}.ai_external_source WHERE tenant_id=?1 UNION ALL SELECT 1 FROM {db}.ai_external_room_grant WHERE tenant_id=?1)"),
            [tenant.0],
            |row| row.get(0),
        )?;
        if contains_ai_chat && !coordinated_ai {
            return Err(invalid(
                "AI chat content requires a coordinated metadata, blob, and key restore",
            ));
        }
    }
    for db in ["main", "source"] {
        if conn
            .prepare(&format!("PRAGMA {db}.foreign_key_check"))?
            .exists([])?
        {
            return Err(invalid("input snapshot violates foreign keys"));
        }
        if conn.query_row(&format!("SELECT EXISTS(SELECT 1 FROM {db}.extension_storage_namespace WHERE tenant_scope=?1)"),params![tenant.0],|row|row.get::<_,bool>(0))? {return Err(invalid("tenant extension storage requires its own migration contract"));}
        for table in OWNED {
            conn.execute(
                &format!(
                    "CREATE TEMP TABLE {} AS SELECT rowid AS rid FROM {db}.{} WHERE {}",
                    selected(db, table),
                    quote(table),
                    scope(table, db, tenant.0)
                ),
                [],
            )?;
        }
    }
    let approvals:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM operation_approval WHERE consumed_at IS NULL AND expires_at>?1 AND principal_id IN (SELECT principal_id FROM membership WHERE tenant_id=?2 UNION SELECT principal_id FROM source.membership WHERE tenant_id=?2))",params![crate::now_text(),tenant.0],|row|row.get(0))?;
    if approvals {
        return Err(invalid(
            "pending approvals for tenant members must expire before recovery",
        ));
    }
    validate_boundaries(&conn)?;
    conn.pragma_update(None, "foreign_keys", false)?;
    let tx = conn.transaction()?;
    // Selected rows can carry a representation unknown to an older reader.
    // Never restore their content under a weaker destination compatibility gate.
    tx.pragma_update(None, "user_version", reader_floor)?;
    // Offline target recovery settles retained owners before deleting linked AI
    // rows. Historical source authority is still never imported into V063.
    super::extension_execution::recover_extension_executions_locked(&tx, Some(tenant.0), false)?;
    let mut removed_rows = BTreeMap::new();
    let mut restored_rows = BTreeMap::new();
    for table in OWNED.iter().rev() {
        let count: usize = tx.query_row(
            &format!("SELECT count(*) FROM {}", selected("main", table)),
            [],
            |row| row.get(0),
        )?;
        tx.execute(
            &format!(
                "DELETE FROM main.{} WHERE rowid IN (SELECT rid FROM {})",
                quote(table),
                selected("main", table)
            ),
            [],
        )?;
        removed_rows.insert((*table).into(), count);
    }
    // Content-addressed shared blobs may be reused, never silently replaced.
    let blob_scope=format!("digest IN (SELECT content_digest FROM source.workspace_checkpoint_node WHERE rowid IN (SELECT rid FROM {}))",selected("source","workspace_checkpoint_node"));
    let collision:bool=tx.query_row(&format!("SELECT EXISTS(SELECT 1 FROM source.workspace_content_blob s JOIN main.workspace_content_blob d USING(digest) WHERE s.{blob_scope} AND (s.snapshot_bytes<>d.snapshot_bytes OR s.snapshot_version<>d.snapshot_version OR s.retained_bytes<>d.retained_bytes))"),[],|row|row.get(0))?;
    if collision {
        return Err(invalid("checkpoint content digest collision"));
    }
    tx.execute(&format!("INSERT OR IGNORE INTO main.workspace_content_blob SELECT * FROM source.workspace_content_blob WHERE {blob_scope}"),[])?;
    for table in OWNED.iter().filter(|table| {
        !DISCARD.contains(table)
            || (coordinated_ai && table.starts_with("ai_") && **table != "ai_content_cleanup")
    }) {
        let columns = tx
            .prepare(&format!("PRAGMA main.table_info({})", quote(table)))?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let projections=columns.iter().map(|column| match (*table,column.as_str()) {
            ("ai_delegation_review","state") => "CASE WHEN state IN ('awaiting_approval','approved') THEN 'revoked' ELSE state END".to_string(),
            ("ai_run_delegation","state") => "CASE WHEN state='active' THEN 'interrupted' ELSE state END".to_string(),
            ("ai_run_delegation","closed_at") => "CASE WHEN state='active' THEN strftime('%Y-%m-%dT%H:%M:%fZ','now') ELSE closed_at END".to_string(),
            _ => quote(column),
        }).collect::<Vec<_>>().join(",");
        let columns = columns
            .iter()
            .map(|column| quote(column))
            .collect::<Vec<_>>()
            .join(",");
        let count=tx.execute(&format!("INSERT INTO main.{} ({columns}) SELECT {projections} FROM source.{} WHERE rowid IN (SELECT rid FROM {})",quote(table),quote(table),selected("source",table)),[]).map_err(|_|invalid(format!("row/ID conflict while restoring {table}")))?;
        restored_rows.insert((*table).into(), count);
    }
    // Target execution facts survive tenant rollback. Source dispatch authority
    // and historical source receipts are deliberately not imported.

    tx.execute("INSERT INTO ai_run_event(run_id,sequence,kind,at) SELECT id,next_sequence,'stopped',?1 FROM ai_run WHERE status='running' AND chat_id IN (SELECT id FROM ai_chat WHERE tenant_id=?2)",params![crate::now_text(),tenant.0])?;
    tx.execute("UPDATE ai_sql_execution SET state='outcome_unknown',finished_at=COALESCE(finished_at,?1) WHERE tenant_id=?2 AND state='applying'",params![crate::now_text(),tenant.0])?;
    tx.execute("UPDATE ai_query_execution SET state=CASE WHEN target_kind='room_document' THEN 'completed' ELSE 'outcome_unknown' END,outcome=CASE WHEN target_kind='room_document' THEN 'refused' ELSE 'outcome_unknown' END,evidence=CASE WHEN target_kind='room_document' THEN 'no_dispatch' ELSE 'interrupted' END,observed_at=COALESCE(observed_at,?1) WHERE tenant_id=?2 AND state='applying'",params![crate::now_text(),tenant.0])?;
    tx.execute("UPDATE ai_run SET status='interrupted',ended_at=?1,next_sequence=next_sequence+1 WHERE status='running' AND chat_id IN (SELECT id FROM ai_chat WHERE tenant_id=?2)",params![crate::now_text(),tenant.0])?;
    for (table, update) in [
        (
            "projection_binding",
            "health='disabled', last_workspace_revision=NULL",
        ),
        (
            "repository_binding",
            "network_enabled=0, credential_handle=NULL",
        ),
        ("run_schedule", "enabled=0, next_fire_at=NULL"),
        (
            "ai_external_source",
            "state='draft',credential_scope_reviewed=0,revision=revision+1",
        ),
    ] {
        tx.execute(
            &format!(
                "UPDATE {} SET {update} WHERE {}",
                quote(table),
                scope(table, "main", tenant.0)
            ),
            [],
        )?;
    }
    // Archive creation already sanitizes run state; repeat only for this tenant.
    super::migration_native::interrupt(&tx, Some(tenant), &crate::now_text())?;
    tx.execute(&format!("UPDATE run_execution SET state='outcome_unknown', cancellation_requested=0, finished_at=COALESCE(finished_at,?1) WHERE ({}) AND state IN ('queued','admitted','preparing','running')",scope("run_execution","main",tenant.0)),params![crate::now_text()])?;
    tx.execute(&format!("UPDATE run_step_result SET state='cancelled',finished_at=COALESCE(finished_at,?1) WHERE ({}) AND state IN ('pending','running')",scope("run_step_result","main",tenant.0)),params![crate::now_text()])?;
    tx.execute(&format!("UPDATE schedule_occurrence SET state=CASE WHEN state IN ('queued','leased','running') THEN 'outcome_unknown' ELSE state END,lease_owner=NULL,lease_expires_at=NULL WHERE {}",scope("schedule_occurrence","main",tenant.0)),[])?;
    tx.execute("UPDATE migration_run SET state='failed',finished_at=COALESCE(finished_at,?1) WHERE tenant_id=?2 AND state='running'",params![crate::now_text(),tenant.0])?;
    tx.execute(
        "UPDATE tenant_invitation SET revoked_at=COALESCE(revoked_at,?1) WHERE tenant_id=?2",
        params![crate::now_text(), tenant.0],
    )?;
    tx.execute(
        "UPDATE api_token SET revoked_at=COALESCE(revoked_at,?1) WHERE tenant_id=?2",
        params![crate::now_text(), tenant.0],
    )?;
    let mut secret_map = BTreeMap::new();
    for (table, column, namespace) in [
        (
            "benchmark_definition",
            "secret_handle",
            crate::benchmark_definition::BENCHMARK_DEFINITION_SECRET_NAMESPACE,
        ),
        (
            "benchmark_run",
            "secret_handle",
            crate::benchmark_run::BENCHMARK_SECRET_NAMESPACE,
        ),
        (
            "connection_profile",
            "shared_secret_handle",
            crate::SECRET_NAMESPACE,
        ),
        (
            "connection_credential",
            "secret_handle",
            crate::SECRET_NAMESPACE,
        ),
        (
            "vault_item_version",
            "secret_handle",
            crate::vault::VAULT_SECRET_NAMESPACE,
        ),
        (
            "ai_external_source",
            "credential_handle",
            crate::ai_external::CREDENTIAL_NAMESPACE,
        ),
    ] {
        let predicate = scope(table, "main", tenant.0);
        let oversized: bool = tx.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE ({predicate}) AND length(CAST({column} AS BLOB))>1024)"),
            [], |row| row.get(0),
        )?;
        if oversized {
            return Err(invalid("selected secret handle exceeds recovery limit"));
        }
        let handles=tx.prepare(&format!("SELECT DISTINCT {column} FROM {table} WHERE ({predicate}) AND {column} IS NOT NULL LIMIT 10001"))?.query_map([],|row|row.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        for handle in handles {
            if secret_map.len() >= 10000
                && !secret_map.contains_key(&(namespace.to_string(), handle.clone()))
            {
                return Err(invalid(
                    "selected tenant exceeds 10000 secret recovery limit",
                ));
            }
            let replacement = secret_map
                .entry((namespace.to_string(), handle.clone()))
                .or_insert_with(|| uuid::Uuid::new_v4().to_string());
            tx.execute(
                &format!("UPDATE {table} SET {column}=?1 WHERE ({predicate}) AND {column}=?2"),
                params![replacement.as_str(), handle],
            )?;
        }
    }
    // Existing node sequences advance naturally; documents use a separate allocator.
    tx.execute("INSERT OR IGNORE INTO document_id_allocator(id) SELECT MAX(id) FROM document HAVING MAX(id) IS NOT NULL",[])?;
    tx.execute("DELETE FROM document_id_allocator", [])?;
    // Archived checkpoints can retain IDs above the maximum live row. Never
    // reuse those IDs after recovery, or lower a destination high-water mark.
    for table in [
        "document_id_allocator",
        "workspace_node",
        "vault",
        "vault_item",
    ] {
        let sequence: Option<i64> = tx
            .query_row(
                "SELECT seq FROM source.sqlite_sequence WHERE name=?1",
                params![table],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(sequence) = sequence {
            let updated = tx.execute(
                "UPDATE main.sqlite_sequence SET seq=MAX(seq,?2) WHERE name=?1",
                params![table, sequence],
            )?;
            if updated == 0 {
                tx.execute(
                    "INSERT INTO main.sqlite_sequence(name,seq) VALUES(?1,?2)",
                    params![table, sequence],
                )?;
            }
        }
    }
    if tx.prepare("PRAGMA main.foreign_key_check")?.exists([])? {
        return Err(invalid("restored snapshot violates foreign keys"));
    }
    let secrets = secret_map
        .into_iter()
        .map(|((namespace, source), destination)| TenantSecretCopy {
            namespace,
            source,
            destination,
        })
        .collect::<Vec<_>>();
    tx.commit()?;
    Ok(TenantMerge {
        report: TenantMergeReport {
            tenant_id: tenant.0,
            removed_rows,
            restored_rows,
            copied_secrets: secrets.len(),
        },
        secrets,
    })
}

fn validate_boundaries(conn: &Connection) -> Result<()> {
    for table in OWNED {
        let mut foreign = BTreeMap::<i64, (String, Vec<(String, String)>)>::new();
        let rows = conn
            .prepare(&format!("PRAGMA main.foreign_key_list({})", quote(table)))?
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (id, parent, from, to) in rows {
            foreign
                .entry(id)
                .or_insert_with(|| (parent, vec![]))
                .1
                .push((from, to));
        }
        for (_, (parent, columns)) in foreign {
            let join = columns
                .iter()
                .map(|(from, to)| format!("c.{}=p.{}", quote(from), quote(to)))
                .collect::<Vec<_>>()
                .join(" AND ");
            if OWNED.contains(&parent.as_str()) {
                for db in ["source", "main"] {
                    let query=format!("SELECT EXISTS(SELECT 1 FROM {db}.{} c JOIN {db}.{} p ON {join} WHERE (c.rowid IN (SELECT rid FROM {}))<>(p.rowid IN (SELECT rid FROM {})))",quote(table),quote(&parent),selected(db,table),selected(db,&parent));
                    if conn.query_row(&query, [], |row| row.get::<_, bool>(0))? {
                        return Err(invalid(format!(
                            "cross-tenant ownership reference: {table} to {parent}"
                        )));
                    }
                }
            } else if parent == "principal" && !DISCARD.contains(table) {
                let query=format!("SELECT EXISTS(SELECT 1 FROM source.{} c JOIN source.principal p ON {join} LEFT JOIN main.principal d ON d.id=p.id AND d.external_id=p.external_id WHERE c.rowid IN (SELECT rid FROM {}) AND d.id IS NULL)",quote(table),selected("source",table));
                if conn.query_row(&query, [], |row| row.get::<_, bool>(0))? {
                    return Err(invalid("referenced source principal is missing or has a different destination identity"));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MemorySecretStore, MetadataStore};
    use std::sync::Arc;
    #[test]
    fn tenant_restore_preserves_target_extension_facts_without_importing_dispatch_authority() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("extension-source.sqlite");
        let target = root.path().join("extension-target.sqlite");
        fixture(&source);
        fixture(&target);
        let source_store =
            MetadataStore::open(&source, Arc::new(MemorySecretStore::new())).unwrap();
        let target_store =
            MetadataStore::open(&target, Arc::new(MemorySecretStore::new())).unwrap();
        let source_claim = source_store
            .claim_extension_execution(&crate::extension_execution::tests::input(
                &source_store,
                false,
            ))
            .unwrap();
        let target_claim = target_store
            .claim_extension_execution(&crate::extension_execution::tests::input(
                &target_store,
                false,
            ))
            .unwrap();
        target_store
            .mark_extension_dispatching(target_claim.receipt.id, crate::PrincipalId(1))
            .unwrap();
        drop(source_store);
        drop(target_store);
        merge_tenant_snapshot(&target, &source, TenantId(1)).unwrap();
        let target_store =
            MetadataStore::open(&target, Arc::new(MemorySecretStore::new())).unwrap();
        let receipt = target_store
            .extension_execution(target_claim.receipt.id, crate::PrincipalId(1))
            .unwrap()
            .receipt;
        assert_eq!(
            receipt.state,
            sift_protocol::ExtensionExecutionState::OutcomeUnknown
        );
        assert_eq!(receipt.frame_started, None);
        assert!(target_store
            .extension_execution(source_claim.receipt.id, crate::PrincipalId(1))
            .is_err());
        assert_eq!(
            target_store
                .migration_status()
                .unwrap()
                .minimum_compatible_version,
            63
        );
    }

    fn fixture(path: &Path) {
        let store = MetadataStore::open(path, Arc::new(MemorySecretStore::new())).unwrap();
        store.apply_migrations(false).unwrap();
        store.bootstrap_local("shared principal").unwrap();
        store.conn().unwrap().execute_batch("INSERT INTO tenant(id,name,kind,created_at,updated_at) VALUES(2,'other','team','2026-01-01','2026-01-01'); INSERT INTO membership VALUES(2,1,'owner','2026-01-01','2026-01-01'); INSERT INTO saved_query(id,tenant_id,principal_id,name,sql_text,tags_json,created_at,updated_at) VALUES(1,1,1,'selected','SELECT 1','[]','2026-01-01','2026-01-01'),(2,2,1,'other','SELECT 2','[]','2026-01-01','2026-01-01');").unwrap();
    }

    #[test]
    fn restore_rejects_ai_chat_without_coordinated_blob_and_key_copy() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.sqlite");
        let destination = root.path().join("destination.sqlite");
        fixture(&source);
        fixture(&destination);
        Connection::open(&source)
            .unwrap()
            .execute(
                "INSERT INTO ai_chat(id,tenant_id,owner_principal_id,visibility,title_handle,revision,created_at,updated_at)
                 VALUES('test-chat',1,1,'private','opaque-handle',1,'2026-01-01','2026-01-01')",
                [],
            )
            .unwrap();
        assert!(matches!(
            merge_tenant_snapshot(&destination, &source, TenantId(1)),
            Err(MetadataError::InvalidTenantRestore(message)) if message.contains("AI chat content")
        ));
    }
    #[test]
    fn restore_refuses_unknown_reader_before_changing_destination() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.sqlite");
        let destination = root.path().join("destination.sqlite");
        fixture(&source);
        fixture(&destination);
        let before = Connection::open(&destination)
            .unwrap()
            .pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap();
        let future = crate::migrations::migrations::runner()
            .get_migrations()
            .iter()
            .map(refinery::Migration::version)
            .max()
            .unwrap()
            + 1;
        Connection::open(&source)
            .unwrap()
            .pragma_update(None, "user_version", future)
            .unwrap();
        assert!(
            matches!(merge_tenant_snapshot(&destination,&source,TenantId(1)),Err(MetadataError::InvalidTenantRestore(message)) if message.contains("newer metadata reader"))
        );
        assert_eq!(
            Connection::open(&destination)
                .unwrap()
                .pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
                .unwrap(),
            before
        );
    }
    #[test]
    fn coordinated_restore_interrupts_delegation_and_keeps_consumption_identity() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.sqlite");
        let destination = root.path().join("destination.sqlite");
        fixture(&source);
        fixture(&destination);
        let conn = Connection::open(&source).unwrap();
        let now = crate::now_text();
        let expires = (chrono::Utc::now() + chrono::Duration::minutes(5)).to_rfc3339();
        let body = uuid::Uuid::new_v4().to_string();
        conn.execute("INSERT INTO ai_chat(id,tenant_id,owner_principal_id,visibility,title_handle,created_at,updated_at) VALUES('delegation-chat',1,1,'private',?1,?2,?2)",params![uuid::Uuid::new_v4().to_string(),now]).unwrap();
        conn.execute("INSERT INTO ai_run(id,chat_id,client_request_id,request_sha256,initiator_principal_id,desktop_id,lease_digest,provider,mode,status,prompt_handle,context_handle,lease_handle,started_at,execute_mode) VALUES('delegated-run','delegation-chat','start',?1,1,'desktop','lease','codex','propose','running','prompt','context','lease-body',?2,1)",params!["a".repeat(64),now]).unwrap();
        conn.execute(
            "INSERT INTO ai_delegation_tenant_policy VALUES(1,1,1,0,1,?1)",
            [&now],
        )
        .unwrap();
        conn.execute("INSERT INTO ai_delegation_review(id,chat_id,tenant_id,initiator_id,client_request_id,request_sha256,content_handle,review_sha256,scope_sha256,tenant_policy_revision,state,approved_by,approved_at,created_at,expires_at) VALUES('consumed','delegation-chat',1,1,'review',?1,?2,?1,?1,1,'approved',1,?3,?3,?4)",params!["b".repeat(64),body,now,expires]).unwrap();
        conn.execute("INSERT INTO ai_run_delegation(run_id,review_id,state,created_at,expires_at,max_calls,max_input_bytes,max_output_bytes) VALUES('delegated-run','consumed','active',?1,?2,3,1024,2048)",params![now,expires]).unwrap();
        conn.execute("UPDATE ai_delegation_review SET state='consumed',used_run_id='delegated-run' WHERE id='consumed'",[]).unwrap();
        conn.execute("INSERT INTO ai_delegation_review(id,chat_id,tenant_id,initiator_id,client_request_id,request_sha256,content_handle,review_sha256,scope_sha256,tenant_policy_revision,state,approved_by,approved_at,created_at,expires_at) VALUES('pending','delegation-chat',1,1,'pending',?1,?2,?1,?1,1,'approved',1,?3,?3,?4)",params!["c".repeat(64),uuid::Uuid::new_v4().to_string(),now,expires]).unwrap();
        conn.pragma_update(None, "user_version", 65).unwrap();
        drop(conn);
        merge_tenant_snapshot_with_ai_content(&destination, &source, TenantId(1)).unwrap();
        let conn = Connection::open(destination).unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT state FROM ai_run_delegation WHERE run_id='delegated-run'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "interrupted"
        );
        assert_eq!(
            conn.query_row(
                "SELECT status FROM ai_run WHERE id='delegated-run'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "interrupted"
        );
        assert_eq!(
            conn.query_row(
                "SELECT state,used_run_id FROM ai_delegation_review WHERE id='consumed'",
                [],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            )
            .unwrap(),
            ("consumed".into(), "delegated-run".into())
        );
        assert_eq!(
            conn.query_row(
                "SELECT state FROM ai_delegation_review WHERE id='pending'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "revoked"
        );
        assert!(conn
            .query_row("SELECT closed_at FROM ai_run_delegation", [], |r| r
                .get::<_, Option<String>>(0))
            .unwrap()
            .is_some());
        assert_eq!(
            conn.pragma_query_value::<u32, _>(None, "user_version", |r| r.get(0))
                .unwrap(),
            65
        );
        assert!(conn
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query([])
            .unwrap()
            .next()
            .unwrap()
            .is_none());
    }

    #[test]
    fn coordinated_restore_preserves_pending_cancellation_for_only_selected_tenant() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.sqlite");
        let destination = root.path().join("destination.sqlite");
        fixture(&source);
        fixture(&destination);
        for (path, tenant, chat) in [(&source, 1, "source-chat"), (&destination, 2, "other-chat")] {
            let conn = Connection::open(path).unwrap();
            conn.execute("INSERT INTO ai_chat(id,tenant_id,owner_principal_id,visibility,title_handle,revision,created_at,updated_at) VALUES(?1,?2,1,'private',?3,1,'2026-01-01','2026-01-01')", params![chat, tenant, uuid::Uuid::new_v4().to_string()]).unwrap();
            conn.execute("INSERT INTO ai_pending_turn_cancel(chat_id,client_request_id,desktop_id,initiator_principal_id,created_at) VALUES(?1,'request','desktop',1,'2026-01-01')", [chat]).unwrap();
        }
        Connection::open(&source)
            .unwrap()
            .pragma_update(None, "user_version", 58)
            .unwrap();
        let merge =
            merge_tenant_snapshot_with_ai_content(&destination, &source, TenantId(1)).unwrap();
        assert_eq!(merge.report.restored_rows["ai_pending_turn_cancel"], 1);
        let conn = Connection::open(destination).unwrap();
        assert_eq!(
            conn.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
                .unwrap(),
            58,
            "tenant recovery preserves the source reader gate"
        );
        assert_eq!(
            conn.query_row("SELECT count(*) FROM ai_pending_turn_cancel", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
    }

    #[test]
    fn external_sources_require_coordinated_restore_and_fresh_review() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.sqlite");
        let destination = root.path().join("destination.sqlite");
        fixture(&source);
        fixture(&destination);
        let id = uuid::Uuid::new_v4().to_string();
        let identity = uuid::Uuid::new_v4().to_string();
        Connection::open(&source).unwrap().execute(
            "INSERT INTO ai_external_source(id,tenant_id,owner_principal_id,revision,state,config_handle,config_sha256,credential_identity,credential_handle,credential_scope_reviewed,created_at,updated_at) VALUES(?1,1,1,4,'active','opaque-config',?2,?3,'opaque-credential',1,'2026-01-01','2026-01-01')",
            params![id,"a".repeat(64),identity],
        ).unwrap();
        assert!(merge_tenant_snapshot(&destination, &source, TenantId(1)).is_err());
        let merge =
            merge_tenant_snapshot_with_ai_content(&destination, &source, TenantId(1)).unwrap();
        assert_eq!(merge.report.restored_rows["ai_external_source"], 1);
        let conn = Connection::open(&destination).unwrap();
        let restored: (String,u64,bool,String) = conn.query_row(
            "SELECT state,revision,credential_scope_reviewed,credential_handle FROM ai_external_source WHERE id=?1", [&id],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
        ).unwrap();
        assert_eq!(restored.0, "draft");
        assert_eq!(restored.1, 5);
        assert!(!restored.2);
        assert_ne!(restored.3, "opaque-credential");
        assert!(merge.secrets.iter().any(|copy| copy.namespace
            == crate::ai_external::CREDENTIAL_NAMESPACE
            && copy.source == "opaque-credential"
            && copy.destination == restored.3));
    }

    #[test]
    fn restores_one_tenant_without_importing_shared_principal_state() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.sqlite");
        let destination = root.path().join("destination.sqlite");
        fixture(&source);
        fixture(&destination);
        let annotated_id = uuid::Uuid::new_v4().to_string();
        Connection::open(&source).unwrap().execute("INSERT INTO benchmark_retention(tenant_id,owner_principal_id,max_age_days,max_runs,revision,updated_at) VALUES(1,1,30,10,3,'2026-01-01T00:00:00Z')",[]).unwrap();
        Connection::open(&source).unwrap().execute("INSERT INTO benchmark_run(id,tenant_id,owner_principal_id,run_id,saved_at,secret_handle,payload_bytes,annotation_revision) VALUES(?1,1,1,?2,'2026-01-01T00:00:00Z','opaque-annotated-snapshot',512,7)",params![annotated_id,uuid::Uuid::new_v4().to_string()]).unwrap();
        let conn = Connection::open(&destination).unwrap();
        conn.execute("UPDATE saved_query SET sql_text='SELECT 99'", [])
            .unwrap();
        conn.execute(
            "UPDATE principal SET display_name='destination identity'",
            [],
        )
        .unwrap();
        drop(conn);
        let merge = merge_tenant_snapshot(&destination, &source, TenantId(1)).unwrap();
        assert_eq!(merge.report.restored_rows["saved_query"], 1);
        assert_eq!(merge.report.restored_rows["benchmark_retention"], 1);
        let restored:(u32,u32,u64)=Connection::open(&destination).unwrap().query_row("SELECT max_age_days,max_runs,revision FROM benchmark_retention WHERE tenant_id=1 AND owner_principal_id=1",[],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
        assert_eq!(restored, (30, 10, 3));
        let conn = Connection::open(&destination).unwrap();
        assert_eq!(
            conn.query_row("SELECT sql_text FROM saved_query WHERE id=1", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            "SELECT 1"
        );
        assert_eq!(
            conn.query_row("SELECT sql_text FROM saved_query WHERE id=2", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            "SELECT 99"
        );
        assert_eq!(
            conn.query_row("SELECT display_name FROM principal WHERE id=1", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            "destination identity"
        );
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM membership WHERE tenant_id=2",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        let (revision, handle): (u64, String) = conn
            .query_row(
                "SELECT annotation_revision,secret_handle FROM benchmark_run WHERE id=?1",
                [annotated_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(revision, 7);
        assert_ne!(handle, "opaque-annotated-snapshot");
        assert_eq!(merge.report.restored_rows["benchmark_run"], 1);
        assert!(merge.secrets.iter().any(|copy| copy.namespace
            == crate::benchmark_run::BENCHMARK_SECRET_NAMESPACE
            && copy.source == "opaque-annotated-snapshot"
            && copy.destination == handle));
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM saved_query_fts WHERE saved_query_fts MATCH '99'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }
    #[test]
    fn checkpoint_blobs_and_historical_ids_survive_and_cross_tenant_edges_refuse() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.sqlite");
        let destination = root.path().join("destination.sqlite");
        fixture(&source);
        fixture(&destination);
        let conn = Connection::open(&source).unwrap();
        conn.execute_batch("
            INSERT INTO room(id,tenant_id,name,kind,created_by,created_at,updated_at)
                VALUES(1,1,'selected','shared',1,'now','now'),(2,2,'other','shared',1,'now','now');
            INSERT INTO workspace(id,room_id,name,created_at,updated_at) VALUES(1,1,'work','now','now');
            INSERT INTO workspace_content_blob VALUES('fixture',x'01',x'02',2,'now');
            INSERT INTO workspace_checkpoint VALUES(1,1,1,'named','before',1,'now');
            INSERT INTO workspace_checkpoint_node VALUES(1,900,NULL,'old.sql','sql_document','fixture');
            INSERT INTO document_id_allocator VALUES(800);
            DELETE FROM document_id_allocator;
            INSERT INTO sqlite_sequence(name,seq) VALUES('workspace_node',900);
        ").unwrap();
        merge_tenant_snapshot(&destination, &source, TenantId(1)).unwrap();
        let target = Connection::open(&destination).unwrap();
        assert_eq!(
            target
                .query_row(
                    "SELECT snapshot_bytes FROM workspace_content_blob WHERE digest='fixture'",
                    [],
                    |row| row.get::<_, Vec<u8>>(0)
                )
                .unwrap(),
            vec![1]
        );
        assert_eq!(
            target
                .query_row(
                    "SELECT seq FROM sqlite_sequence WHERE name='workspace_node'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            900
        );
        target
            .execute("INSERT INTO document_id_allocator DEFAULT VALUES", [])
            .unwrap();
        assert_eq!(target.last_insert_rowid(), 801);
        target
            .execute("UPDATE workspace_content_blob SET snapshot_bytes=x'03'", [])
            .unwrap();
        assert!(merge_tenant_snapshot(&destination, &source, TenantId(1))
            .err()
            .unwrap()
            .to_string()
            .contains("digest collision"));
        target
            .execute("UPDATE workspace_content_blob SET snapshot_bytes=x'01'", [])
            .unwrap();
        conn.execute_batch("
            INSERT INTO document(id,room_id,kind,title,crdt_type,crdt_state,position,created_at,updated_at)
                VALUES(1,2,'sql','other','loro',x'',0,'now','now');
            INSERT INTO workspace_node(workspace_id,path,path_key,kind,document_id,created_at,updated_at)
                VALUES(1,'cross.sql','cross.sql','sql_document',1,'now','now');
        ").unwrap();
        assert!(merge_tenant_snapshot(&destination, &source, TenantId(1))
            .err()
            .unwrap()
            .to_string()
            .contains("cross-tenant ownership reference"));
    }

    #[test]
    fn refuses_identity_and_primary_key_conflicts_without_partial_writes() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.sqlite");
        let destination = root.path().join("destination.sqlite");
        fixture(&source);
        fixture(&destination);
        let conn = Connection::open(&destination).unwrap();
        conn.execute(
            "UPDATE principal SET external_id='different' WHERE id=1",
            [],
        )
        .unwrap();
        assert!(merge_tenant_snapshot(&destination, &source, TenantId(1))
            .err()
            .unwrap()
            .to_string()
            .contains("different destination identity"));
        conn.execute("UPDATE principal SET external_id='local:1' WHERE id=1", [])
            .unwrap();
        conn.execute("UPDATE saved_query SET tenant_id=2 WHERE id=1", [])
            .unwrap();
        assert!(merge_tenant_snapshot(&destination, &source, TenantId(1))
            .err()
            .unwrap()
            .to_string()
            .contains("row/ID conflict"));
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM saved_query WHERE tenant_id=2",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            2
        );
        conn.execute_batch("CREATE TABLE unexpected(id INTEGER)")
            .unwrap();
        assert!(merge_tenant_snapshot(&destination, &source, TenantId(1))
            .err()
            .unwrap()
            .to_string()
            .contains("schemas differ"));
        Connection::open(&source)
            .unwrap()
            .execute_batch("CREATE TABLE unexpected(id INTEGER)")
            .unwrap();
        assert!(merge_tenant_snapshot(&destination, &source, TenantId(1))
            .err()
            .unwrap()
            .to_string()
            .contains("unclassified table"));
    }
}
