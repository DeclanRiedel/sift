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
    "plan_capture",
    "benchmark_run",
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
];
const DISCARD: &[&str] = &[
    "projection_file_state",
    "repository_principal_credential",
    "repository_hosting_credential",
    "workspace_artifact",
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
        "tenant" => format!("id={tenant}"),
        "membership" | "connection_profile" | "room" | "saved_query" | "catalog_snapshot"
        | "migration_run" | "plan_capture" | "benchmark_run" | "vault" | "sql_snippet" => {
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
    for table in OWNED.iter().filter(|table| !DISCARD.contains(table)) {
        let columns = tx
            .prepare(&format!("PRAGMA main.table_info({})", quote(table)))?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let columns = columns
            .iter()
            .map(|column| quote(column))
            .collect::<Vec<_>>()
            .join(",");
        let count=tx.execute(&format!("INSERT INTO main.{} ({columns}) SELECT {columns} FROM source.{} WHERE rowid IN (SELECT rid FROM {})",quote(table),quote(table),selected("source",table)),[]).map_err(|_|invalid(format!("row/ID conflict while restoring {table}")))?;
        restored_rows.insert((*table).into(), count);
    }
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
    fn fixture(path: &Path) {
        let store = MetadataStore::open(path, Arc::new(MemorySecretStore::new())).unwrap();
        store.apply_migrations(false).unwrap();
        store.bootstrap_local("shared principal").unwrap();
        store.conn().unwrap().execute_batch("INSERT INTO tenant(id,name,kind,created_at,updated_at) VALUES(2,'other','team','2026-01-01','2026-01-01'); INSERT INTO membership VALUES(2,1,'owner','2026-01-01','2026-01-01'); INSERT INTO saved_query(id,tenant_id,principal_id,name,sql_text,tags_json,created_at,updated_at) VALUES(1,1,1,'selected','SELECT 1','[]','2026-01-01','2026-01-01'),(2,2,1,'other','SELECT 2','[]','2026-01-01','2026-01-01');").unwrap();
    }
    #[test]
    fn restores_one_tenant_without_importing_shared_principal_state() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.sqlite");
        let destination = root.path().join("destination.sqlite");
        fixture(&source);
        fixture(&destination);
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
        assert!(merge.secrets.is_empty());
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
