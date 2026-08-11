use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use sift_protocol::{
    DdlSource, DdlSourceCoverage, DdlSourceId, DdlSourceMapping, WorkspaceId, WorkspaceNodeId,
    WorkspaceRevision,
};

use crate::workspace::{ensure_room_access, workspace_by_id_locked};
use crate::{
    now_text, parse_time_sql, rows, DdlSourceModelUpdate, DdlSourceRecord, MetadataError,
    MetadataStore, NewDdlSource, PrincipalId, Result,
};

const MAX_SOURCES_PER_WORKSPACE: i64 = 64;
const MAX_SOURCE_NAME_BYTES: usize = 128;
const MAX_ROOTS: usize = 256;
const MAX_MAPPINGS: usize = 64;

impl MetadataStore {
    pub fn create_ddl_source(
        &self,
        workspace_id: WorkspaceId,
        actor: PrincipalId,
        input: NewDdlSource,
    ) -> Result<DdlSourceRecord> {
        validate_source_input(&input)?;
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let workspace = workspace_by_id_locked(&tx, workspace_id)?;
        ensure_room_access(&tx, workspace.room_id, actor, true)?;
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM ddl_source WHERE workspace_id = ?1",
            params![workspace_id.0],
            |row| row.get(0),
        )?;
        if count >= MAX_SOURCES_PER_WORKSPACE {
            return Err(MetadataError::InvalidDdlSource);
        }
        validate_roots(&tx, workspace_id, &input.roots)?;
        tx.execute(
            "INSERT INTO ddl_source (
                 workspace_id, name, dialect_id, workspace_revision, model_revision,
                 coverage, diagnostic_count, diagnostics_json, revision, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, 1, 'stale', 0, '[]', 1, ?5, ?5)",
            params![
                workspace_id.0,
                input.name,
                input.dialect_id,
                workspace.revision.0,
                now,
            ],
        )
        .map_err(map_source_constraint)?;
        let id = DdlSourceId(tx.last_insert_rowid());
        replace_roots(&tx, id, &input.roots)?;
        let source = ddl_source_by_id_locked(&tx, id)?;
        tx.commit()?;
        Ok(source)
    }

    pub fn list_ddl_sources_for_principal(
        &self,
        workspace_id: WorkspaceId,
        principal: PrincipalId,
    ) -> Result<Vec<DdlSource>> {
        let conn = self.conn()?;
        let workspace = workspace_by_id_locked(&conn, workspace_id)?;
        ensure_room_access(&conn, workspace.room_id, principal, false)?;
        let mut stmt =
            conn.prepare("SELECT id FROM ddl_source WHERE workspace_id = ?1 ORDER BY name, id")?;
        let ids = rows(stmt.query_map(params![workspace_id.0], |row| {
            row.get::<_, i64>(0).map(DdlSourceId)
        })?)?;
        ids.into_iter()
            .map(|id| ddl_source_by_id_locked(&conn, id).map(|record| record.source))
            .collect()
    }

    pub fn ddl_source_for_principal(
        &self,
        id: DdlSourceId,
        principal: PrincipalId,
        writable: bool,
    ) -> Result<DdlSourceRecord> {
        let conn = self.conn()?;
        let source = ddl_source_by_id_locked(&conn, id)?;
        let workspace = workspace_by_id_locked(&conn, source.source.workspace_id)?;
        ensure_room_access(&conn, workspace.room_id, principal, writable)?;
        Ok(source)
    }

    pub fn update_ddl_source(
        &self,
        id: DdlSourceId,
        actor: PrincipalId,
        expected_revision: u64,
        input: NewDdlSource,
        mappings: &[DdlSourceMapping],
    ) -> Result<DdlSourceRecord> {
        validate_source_input(&input)?;
        if mappings.len() > MAX_MAPPINGS {
            return Err(MetadataError::InvalidDdlSource);
        }
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = ddl_source_by_id_locked(&tx, id)?;
        let workspace = workspace_by_id_locked(&tx, current.source.workspace_id)?;
        ensure_room_access(&tx, workspace.room_id, actor, true)?;
        ensure_source_revision(&current.source, expected_revision)?;
        validate_roots(&tx, workspace.id, &input.roots)?;
        validate_mappings(&tx, workspace.room_id, mappings)?;
        tx.execute(
            "UPDATE ddl_source SET name = ?1, dialect_id = ?2, coverage = 'stale',
                 workspace_revision = ?3, model_json = NULL, diagnostics_json = '[]',
                 diagnostic_count = 0, revision = revision + 1, updated_at = ?4
             WHERE id = ?5",
            params![
                input.name,
                input.dialect_id,
                workspace.revision.0,
                now,
                id.0
            ],
        )
        .map_err(map_source_constraint)?;
        replace_roots(&tx, id, &input.roots)?;
        replace_mappings(&tx, id, mappings)?;
        let source = ddl_source_by_id_locked(&tx, id)?;
        tx.commit()?;
        Ok(source)
    }

    pub fn store_ddl_source_model(
        &self,
        id: DdlSourceId,
        actor: PrincipalId,
        input: DdlSourceModelUpdate,
    ) -> Result<DdlSourceRecord> {
        let now = now_text();
        let diagnostics_json = serde_json::to_string(&input.diagnostics)?;
        let diagnostic_count =
            i64::try_from(input.diagnostics.len()).map_err(|_| MetadataError::InvalidDdlSource)?;
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = ddl_source_by_id_locked(&tx, id)?;
        let workspace = workspace_by_id_locked(&tx, current.source.workspace_id)?;
        ensure_room_access(&tx, workspace.room_id, actor, true)?;
        ensure_source_revision(&current.source, input.expected_revision)?;
        if workspace.revision != input.expected_workspace_revision {
            return Err(MetadataError::WorkspaceRevisionConflict {
                expected: input.expected_workspace_revision.0,
                current: workspace.revision.0,
            });
        }
        tx.execute(
            "UPDATE ddl_source SET workspace_revision = ?1, model_revision = model_revision + 1,
                 coverage = ?2, diagnostic_count = ?3, model_json = ?4,
                 diagnostics_json = ?5, revision = revision + 1, updated_at = ?6
             WHERE id = ?7",
            params![
                workspace.revision.0,
                coverage_text(input.coverage),
                diagnostic_count,
                input.model_json,
                diagnostics_json,
                now,
                id.0,
            ],
        )?;
        let updated = ddl_source_by_id_locked(&tx, id)?;
        tx.commit()?;
        Ok(updated)
    }

    pub fn delete_ddl_source(
        &self,
        id: DdlSourceId,
        actor: PrincipalId,
        expected_revision: u64,
    ) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = ddl_source_by_id_locked(&tx, id)?;
        let workspace = workspace_by_id_locked(&tx, current.source.workspace_id)?;
        ensure_room_access(&tx, workspace.room_id, actor, true)?;
        ensure_source_revision(&current.source, expected_revision)?;
        tx.execute("DELETE FROM ddl_source WHERE id = ?1", params![id.0])?;
        tx.commit()?;
        Ok(())
    }
}

fn ddl_source_by_id_locked(conn: &Connection, id: DdlSourceId) -> Result<DdlSourceRecord> {
    let raw = conn
        .query_row(
            "SELECT id, workspace_id, name, dialect_id, workspace_revision, model_revision,
                    coverage, diagnostic_count, model_json, diagnostics_json, revision,
                    created_at, updated_at
             FROM ddl_source WHERE id = ?1",
            params![id.0],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, String>(12)?,
                ))
            },
        )
        .optional()?
        .ok_or(MetadataError::DdlSourceNotFound(id))?;
    let roots = source_roots(conn, id)?;
    let mappings = source_mappings(conn, id)?;
    let diagnostics = serde_json::from_str(&raw.9)?;
    Ok(DdlSourceRecord {
        source: DdlSource {
            id: DdlSourceId(raw.0),
            workspace_id: WorkspaceId(raw.1),
            name: raw.2,
            dialect_id: raw.3,
            roots,
            model_revision: checked_u64(5, raw.5)?,
            coverage: coverage_from_text(&raw.6)?,
            diagnostic_count: u32::try_from(raw.7).map_err(|_| MetadataError::InvalidDdlSource)?,
            revision: checked_u64(10, raw.10)?,
            created_at: parse_time_sql(raw.11)?,
            updated_at: parse_time_sql(raw.12)?,
        },
        workspace_revision: WorkspaceRevision(checked_u64(4, raw.4)?),
        model_json: raw.8,
        diagnostics,
        mappings,
    })
}

fn source_roots(conn: &Connection, id: DdlSourceId) -> Result<Vec<WorkspaceNodeId>> {
    let mut stmt =
        conn.prepare("SELECT node_id FROM ddl_source_root WHERE source_id = ?1 ORDER BY position")?;
    let roots = rows(stmt.query_map(params![id.0], |row| {
        row.get::<_, i64>(0).map(WorkspaceNodeId)
    })?)?;
    Ok(roots)
}

fn source_mappings(conn: &Connection, id: DdlSourceId) -> Result<Vec<DdlSourceMapping>> {
    let mut stmt = conn.prepare(
        "SELECT connection_profile_id, catalog, schema_name FROM ddl_source_mapping
         WHERE source_id = ?1 ORDER BY connection_profile_id, catalog, schema_name",
    )?;
    let mappings = rows(stmt.query_map(params![id.0], |row| {
        Ok(DdlSourceMapping {
            connection_profile_id: row.get(0)?,
            catalog: row.get(1)?,
            schema: row.get(2)?,
        })
    })?)?;
    Ok(mappings)
}

fn replace_roots(conn: &Connection, id: DdlSourceId, roots: &[WorkspaceNodeId]) -> Result<()> {
    conn.execute(
        "DELETE FROM ddl_source_root WHERE source_id = ?1",
        params![id.0],
    )?;
    for (position, node) in roots.iter().enumerate() {
        conn.execute(
            "INSERT INTO ddl_source_root (source_id, node_id, position) VALUES (?1, ?2, ?3)",
            params![id.0, node.0, i64::try_from(position).unwrap_or(i64::MAX)],
        )?;
    }
    Ok(())
}

fn replace_mappings(
    conn: &Connection,
    id: DdlSourceId,
    mappings: &[DdlSourceMapping],
) -> Result<()> {
    conn.execute(
        "DELETE FROM ddl_source_mapping WHERE source_id = ?1",
        params![id.0],
    )?;
    for mapping in mappings {
        conn.execute(
            "INSERT INTO ddl_source_mapping (
                 source_id, connection_profile_id, catalog, schema_name
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                id.0,
                mapping.connection_profile_id,
                mapping.catalog,
                mapping.schema
            ],
        )?;
    }
    Ok(())
}

fn validate_source_input(input: &NewDdlSource) -> Result<()> {
    if input.name.is_empty()
        || input.name.len() > MAX_SOURCE_NAME_BYTES
        || input.name.trim() != input.name
        || input.name.contains('\0')
        || !matches!(
            input.dialect_id.as_str(),
            "sift/postgres" | "sift/sql-server"
        )
        || input.roots.is_empty()
        || input.roots.len() > MAX_ROOTS
    {
        return Err(MetadataError::InvalidDdlSource);
    }
    let unique = input.roots.iter().collect::<std::collections::HashSet<_>>();
    if unique.len() != input.roots.len() {
        return Err(MetadataError::InvalidDdlSource);
    }
    Ok(())
}

fn validate_roots(
    conn: &Connection,
    workspace: WorkspaceId,
    roots: &[WorkspaceNodeId],
) -> Result<()> {
    for root in roots {
        let valid: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM workspace_node
             WHERE id = ?1 AND workspace_id = ?2 AND kind IN ('folder', 'sql_document'))",
            params![root.0, workspace.0],
            |row| row.get(0),
        )?;
        if !valid {
            return Err(MetadataError::InvalidDdlSource);
        }
    }
    Ok(())
}

fn validate_mappings(
    conn: &Connection,
    room: crate::RoomId,
    mappings: &[DdlSourceMapping],
) -> Result<()> {
    for mapping in mappings {
        let valid: bool = conn.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM connection_profile cp
                 JOIN room r ON r.tenant_id = cp.tenant_id
                 WHERE cp.id = ?1 AND r.id = ?2
             )",
            params![mapping.connection_profile_id, room.0],
            |row| row.get(0),
        )?;
        if !valid
            || mapping
                .catalog
                .as_ref()
                .is_some_and(|value| value.len() > 256 || value.contains('\0'))
            || mapping
                .schema
                .as_ref()
                .is_some_and(|value| value.len() > 256 || value.contains('\0'))
        {
            return Err(MetadataError::InvalidDdlSource);
        }
    }
    Ok(())
}

fn ensure_source_revision(source: &DdlSource, expected: u64) -> Result<()> {
    if source.revision == expected {
        Ok(())
    } else {
        Err(MetadataError::DdlSourceRevisionConflict {
            expected,
            current: source.revision,
        })
    }
}

fn coverage_text(coverage: DdlSourceCoverage) -> &'static str {
    match coverage {
        DdlSourceCoverage::Complete => "complete",
        DdlSourceCoverage::Partial => "partial",
        DdlSourceCoverage::Stale => "stale",
        DdlSourceCoverage::Invalid => "invalid",
    }
}

fn coverage_from_text(value: &str) -> Result<DdlSourceCoverage> {
    match value {
        "complete" => Ok(DdlSourceCoverage::Complete),
        "partial" => Ok(DdlSourceCoverage::Partial),
        "stale" => Ok(DdlSourceCoverage::Stale),
        "invalid" => Ok(DdlSourceCoverage::Invalid),
        _ => Err(MetadataError::InvalidEnum {
            field: "ddl_source.coverage",
            value: value.into(),
        }),
    }
}

fn checked_u64(column: usize, value: i64) -> Result<u64> {
    u64::try_from(value)
        .map_err(|_| MetadataError::Sqlite(rusqlite::Error::IntegralValueOutOfRange(column, value)))
}

fn map_source_constraint(error: rusqlite::Error) -> MetadataError {
    match error {
        rusqlite::Error::SqliteFailure(ref inner, _)
            if inner.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            MetadataError::InvalidDdlSource
        }
        other => MetadataError::Sqlite(other),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::{MemorySecretStore, NewRoom, NewWorkspaceNode, RoomKind};
    use sift_protocol::{WorkspaceNodeKind, WorkspacePath};

    use super::*;

    #[test]
    fn ddl_sources_become_stored_revisioned_models() {
        let store = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        store.bootstrap_local("ddl-source-test").unwrap();
        let actor = PrincipalId(1);
        let room = store
            .create_room(
                crate::TenantId(1),
                actor,
                NewRoom {
                    name: "room".into(),
                    kind: RoomKind::Shared,
                },
            )
            .unwrap();
        let workspace = store.create_workspace(room.id, actor, "database").unwrap();
        let (workspace, node) = store
            .create_workspace_node(
                workspace.id,
                actor,
                workspace.revision,
                NewWorkspaceNode {
                    parent_id: None,
                    path: WorkspacePath("schema.sql".into()),
                    kind: WorkspaceNodeKind::SqlDocument,
                    initial_snapshot: Some(vec![1]),
                    initial_snapshot_version: Some(vec![1]),
                },
            )
            .unwrap();
        let source = store
            .create_ddl_source(
                workspace.id,
                actor,
                NewDdlSource {
                    name: "desired".into(),
                    dialect_id: "sift/postgres".into(),
                    roots: vec![node.id],
                },
            )
            .unwrap();
        let updated = store
            .store_ddl_source_model(
                source.source.id,
                actor,
                DdlSourceModelUpdate {
                    expected_revision: 1,
                    expected_workspace_revision: workspace.revision,
                    coverage: DdlSourceCoverage::Complete,
                    model_json: Some("{}".into()),
                    diagnostics: Vec::new(),
                },
            )
            .unwrap();
        assert_eq!(updated.source.model_revision, 2);
        assert_eq!(updated.source.revision, 2);
        assert_eq!(updated.model_json.as_deref(), Some("{}"));
    }
}
