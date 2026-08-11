use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use sift_protocol::{
    ProjectionBinding, ProjectionBindingId, ProjectionHealth, ProjectionMode, WorkspaceId,
    WorkspacePath, WorkspaceRevision,
};

use crate::workspace::{
    ensure_room_access, ensure_room_owner, workspace_by_id_locked, workspace_path_key,
};
use crate::{
    now_text, parse_time_sql, rows, MetadataError, MetadataStore, NewProjectionBinding,
    PrincipalId, ProjectionBindingRecord, ProjectionFileState, Result,
};

const ROOT_HANDLE_MAX_BYTES: usize = 64;
const ADAPTER_GENERATION_MAX_BYTES: usize = 128;

impl MetadataStore {
    pub fn create_projection_binding(
        &self,
        workspace_id: WorkspaceId,
        actor: PrincipalId,
        input: NewProjectionBinding,
    ) -> Result<ProjectionBindingRecord> {
        validate_binding_input(&input)?;
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let workspace = workspace_by_id_locked(&tx, workspace_id)?;
        ensure_room_owner(&tx, workspace.room_id, actor)?;
        tx.execute(
            "INSERT INTO projection_binding (
                 workspace_id, adapter_id, root_handle, mode, adapter_generation,
                 health, revision, created_at, updated_at
             ) VALUES (?1, 'rooted_filesystem', ?2, ?3, ?4, ?5, 1, ?6, ?6)",
            params![
                workspace_id.0,
                input.root_handle,
                projection_mode_text(input.mode),
                input.adapter_generation,
                projection_health_text(input.health),
                now,
            ],
        )
        .map_err(|error| match error {
            rusqlite::Error::SqliteFailure(ref inner, _)
                if inner.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                MetadataError::InvalidProjectionBinding
            }
            other => MetadataError::Sqlite(other),
        })?;
        let id = ProjectionBindingId(tx.last_insert_rowid());
        let binding = projection_binding_by_id_locked(&tx, id)?;
        tx.commit()?;
        Ok(binding)
    }

    pub fn projection_binding_for_principal(
        &self,
        id: ProjectionBindingId,
        principal: PrincipalId,
        writable: bool,
    ) -> Result<ProjectionBindingRecord> {
        let conn = self.conn()?;
        let binding = projection_binding_by_id_locked(&conn, id)?;
        let workspace = workspace_by_id_locked(&conn, binding.binding.workspace_id)?;
        ensure_room_access(&conn, workspace.room_id, principal, writable)?;
        Ok(binding)
    }

    pub fn projection_binding_for_workspace(
        &self,
        workspace_id: WorkspaceId,
        principal: PrincipalId,
    ) -> Result<Option<ProjectionBindingRecord>> {
        let conn = self.conn()?;
        let workspace = workspace_by_id_locked(&conn, workspace_id)?;
        ensure_room_access(&conn, workspace.room_id, principal, false)?;
        let id = conn
            .query_row(
                "SELECT id FROM projection_binding WHERE workspace_id = ?1",
                params![workspace_id.0],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .map(ProjectionBindingId);
        id.map(|id| projection_binding_by_id_locked(&conn, id))
            .transpose()
    }

    pub fn delete_projection_binding(
        &self,
        id: ProjectionBindingId,
        actor: PrincipalId,
        expected_revision: u64,
    ) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let binding = projection_binding_by_id_locked(&tx, id)?;
        let workspace = workspace_by_id_locked(&tx, binding.binding.workspace_id)?;
        ensure_room_owner(&tx, workspace.room_id, actor)?;
        ensure_binding_revision(&binding.binding, expected_revision)?;
        tx.execute(
            "DELETE FROM projection_binding WHERE id = ?1",
            params![id.0],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn projection_file_state_for_principal(
        &self,
        id: ProjectionBindingId,
        principal: PrincipalId,
    ) -> Result<Vec<ProjectionFileState>> {
        let conn = self.conn()?;
        let binding = projection_binding_by_id_locked(&conn, id)?;
        let workspace = workspace_by_id_locked(&conn, binding.binding.workspace_id)?;
        ensure_room_access(&conn, workspace.room_id, principal, false)?;
        projection_file_state_locked(&conn, id)
    }

    pub fn commit_projection_observation(
        &self,
        id: ProjectionBindingId,
        actor: PrincipalId,
        expected_binding_revision: u64,
        expected_workspace_revision: WorkspaceRevision,
        health: ProjectionHealth,
        files: &[ProjectionFileState],
    ) -> Result<ProjectionBindingRecord> {
        validate_file_states(files)?;
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let binding = projection_binding_by_id_locked(&tx, id)?;
        let workspace = workspace_by_id_locked(&tx, binding.binding.workspace_id)?;
        ensure_room_access(&tx, workspace.room_id, actor, true)?;
        ensure_binding_revision(&binding.binding, expected_binding_revision)?;
        if workspace.revision != expected_workspace_revision {
            return Err(MetadataError::WorkspaceRevisionConflict {
                expected: expected_workspace_revision.0,
                current: workspace.revision.0,
            });
        }
        tx.execute(
            "DELETE FROM projection_file_state WHERE binding_id = ?1",
            params![id.0],
        )?;
        for file in files {
            tx.execute(
                "INSERT INTO projection_file_state (
                     binding_id, node_id, path, path_key, workspace_digest, projection_digest
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    id.0,
                    file.node_id.map(|node| node.0),
                    file.path.0,
                    workspace_path_key(&file.path)?,
                    file.workspace_digest,
                    file.projection_digest,
                ],
            )?;
        }
        tx.execute(
            "UPDATE projection_binding
             SET last_workspace_revision = ?1, health = ?2, revision = revision + 1,
                 updated_at = ?3
             WHERE id = ?4",
            params![
                expected_workspace_revision.0,
                projection_health_text(health),
                now,
                id.0,
            ],
        )?;
        let updated = projection_binding_by_id_locked(&tx, id)?;
        tx.commit()?;
        Ok(updated)
    }
}

fn validate_binding_input(input: &NewProjectionBinding) -> Result<()> {
    let valid_handle = !input.root_handle.is_empty()
        && input.root_handle.len() <= ROOT_HANDLE_MAX_BYTES
        && input
            .root_handle
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
    let valid_generation = !input.adapter_generation.is_empty()
        && input.adapter_generation.len() <= ADAPTER_GENERATION_MAX_BYTES
        && !input.adapter_generation.contains('\0');
    if valid_handle && valid_generation {
        Ok(())
    } else {
        Err(MetadataError::InvalidProjectionBinding)
    }
}

fn validate_file_states(files: &[ProjectionFileState]) -> Result<()> {
    if files.len() > 20_000 {
        return Err(MetadataError::InvalidProjectionBinding);
    }
    let mut keys = std::collections::HashSet::with_capacity(files.len());
    for file in files {
        let key = workspace_path_key(&file.path)?;
        if !keys.insert(key)
            || file
                .workspace_digest
                .iter()
                .chain(file.projection_digest.iter())
                .any(|digest| !is_sha256(digest))
        {
            return Err(MetadataError::InvalidProjectionBinding);
        }
    }
    Ok(())
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn ensure_binding_revision(binding: &ProjectionBinding, expected: u64) -> Result<()> {
    if binding.revision == expected {
        Ok(())
    } else {
        Err(MetadataError::ProjectionRevisionConflict {
            expected,
            current: binding.revision,
        })
    }
}

fn projection_binding_by_id_locked(
    conn: &Connection,
    id: ProjectionBindingId,
) -> Result<ProjectionBindingRecord> {
    conn.query_row(
        "SELECT id, workspace_id, adapter_id, root_handle, mode,
                last_workspace_revision, adapter_generation, health, revision,
                created_at, updated_at
         FROM projection_binding WHERE id = ?1",
        params![id.0],
        |row| {
            let revision = row.get::<_, i64>(8)?;
            let last_revision = row.get::<_, Option<i64>>(5)?;
            Ok(ProjectionBindingRecord {
                binding: ProjectionBinding {
                    id: ProjectionBindingId(row.get(0)?),
                    workspace_id: WorkspaceId(row.get(1)?),
                    adapter_id: row.get(2)?,
                    mode: projection_mode_from_text(row.get(4)?)?,
                    last_workspace_revision: last_revision
                        .map(|value| checked_u64(5, value).map(WorkspaceRevision))
                        .transpose()?,
                    adapter_generation: row.get(6)?,
                    health: projection_health_from_text(row.get(7)?)?,
                    revision: checked_u64(8, revision)?,
                },
                root_handle: row.get(3)?,
                created_at: parse_time_sql(row.get(9)?)?,
                updated_at: parse_time_sql(row.get(10)?)?,
            })
        },
    )
    .optional()?
    .ok_or(MetadataError::ProjectionBindingNotFound(id))
}

fn projection_file_state_locked(
    conn: &Connection,
    id: ProjectionBindingId,
) -> Result<Vec<ProjectionFileState>> {
    let mut stmt = conn.prepare(
        "SELECT node_id, path, workspace_digest, projection_digest
         FROM projection_file_state WHERE binding_id = ?1 ORDER BY path_key",
    )?;
    let files = rows(stmt.query_map(params![id.0], |row| {
        Ok(ProjectionFileState {
            node_id: row
                .get::<_, Option<i64>>(0)?
                .map(sift_protocol::WorkspaceNodeId),
            path: WorkspacePath(row.get(1)?),
            workspace_digest: row.get(2)?,
            projection_digest: row.get(3)?,
        })
    })?)?;
    Ok(files)
}

fn projection_mode_text(mode: ProjectionMode) -> &'static str {
    match mode {
        ProjectionMode::ReadOnly => "read_only",
        ProjectionMode::ReadWrite => "read_write",
    }
}

fn projection_mode_from_text(value: String) -> rusqlite::Result<ProjectionMode> {
    match value.as_str() {
        "read_only" => Ok(ProjectionMode::ReadOnly),
        "read_write" => Ok(ProjectionMode::ReadWrite),
        _ => Err(invalid_enum(4, value)),
    }
}

fn projection_health_text(health: ProjectionHealth) -> &'static str {
    match health {
        ProjectionHealth::Ready => "ready",
        ProjectionHealth::Disabled => "disabled",
        ProjectionHealth::Missing => "missing",
        ProjectionHealth::ReadOnly => "read_only",
        ProjectionHealth::Conflicted => "conflicted",
        ProjectionHealth::Unavailable => "unavailable",
    }
}

fn projection_health_from_text(value: String) -> rusqlite::Result<ProjectionHealth> {
    match value.as_str() {
        "ready" => Ok(ProjectionHealth::Ready),
        "disabled" => Ok(ProjectionHealth::Disabled),
        "missing" => Ok(ProjectionHealth::Missing),
        "read_only" => Ok(ProjectionHealth::ReadOnly),
        "conflicted" => Ok(ProjectionHealth::Conflicted),
        "unavailable" => Ok(ProjectionHealth::Unavailable),
        _ => Err(invalid_enum(7, value)),
    }
}

fn checked_u64(column: usize, value: i64) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(column, value))
}

fn invalid_enum(column: usize, value: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        column,
        rusqlite::types::Type::Text,
        format!("invalid projection enum: {value}").into(),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::{MemorySecretStore, NewRoom, RoomKind};

    use super::*;

    #[test]
    fn projection_baseline_is_revisioned_and_path_ordered() {
        let store = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        store.bootstrap_local("projection-test").unwrap();
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
        let binding = store
            .create_projection_binding(
                workspace.id,
                actor,
                NewProjectionBinding {
                    root_handle: "checkout".into(),
                    mode: ProjectionMode::ReadWrite,
                    adapter_generation: "filesystem-v1".into(),
                    health: ProjectionHealth::Ready,
                },
            )
            .unwrap();
        let digest = "a".repeat(64);
        let updated = store
            .commit_projection_observation(
                binding.binding.id,
                actor,
                1,
                workspace.revision,
                ProjectionHealth::Ready,
                &[
                    ProjectionFileState {
                        node_id: None,
                        path: WorkspacePath("z.sql".into()),
                        workspace_digest: None,
                        projection_digest: Some(digest.clone()),
                    },
                    ProjectionFileState {
                        node_id: None,
                        path: WorkspacePath("a.sql".into()),
                        workspace_digest: None,
                        projection_digest: Some(digest),
                    },
                ],
            )
            .unwrap();
        assert_eq!(updated.binding.revision, 2);
        let files = store
            .projection_file_state_for_principal(binding.binding.id, actor)
            .unwrap();
        assert_eq!(files[0].path.0, "a.sql");
        assert!(matches!(
            store.commit_projection_observation(
                binding.binding.id,
                actor,
                1,
                workspace.revision,
                ProjectionHealth::Ready,
                &[],
            ),
            Err(MetadataError::ProjectionRevisionConflict { current: 2, .. })
        ));
    }
}
