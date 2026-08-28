use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use sift_protocol::{ProjectionBindingId, RepositoryBinding, RepositoryBindingId, WorkspaceId};
use uuid::Uuid;

use crate::workspace::{ensure_room_access, ensure_room_owner, workspace_by_id_locked};
use crate::{
    now_text, parse_time_sql, MetadataError, MetadataStore, NewRepositoryBinding,
    NewRepositoryCommit, PrincipalId, RepositoryBindingRecord, RepositoryObservation, Result,
};

const VCS_CREDENTIAL_NAMESPACE: &str = "vcs-credential";

impl MetadataStore {
    pub fn repository_commit_for_checkpoint(
        &self,
        id: RepositoryBindingId,
        actor: PrincipalId,
        checkpoint: sift_protocol::WorkspaceCheckpointId,
    ) -> Result<Option<String>> {
        let conn = self.conn()?;
        let binding = repository_binding_by_id_locked(&conn, id)?;
        let workspace = workspace_by_id_locked(&conn, binding.binding.workspace_id)?;
        ensure_room_access(&conn, workspace.room_id, actor, false)?;
        conn.query_row(
            "SELECT commit_oid FROM repository_commit WHERE binding_id = ?1 AND checkpoint_id = ?2",
            params![id.0, checkpoint.0],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn create_repository_binding(
        &self,
        workspace_id: WorkspaceId,
        actor: PrincipalId,
        input: NewRepositoryBinding,
    ) -> Result<RepositoryBindingRecord> {
        validate_new_binding(&input)?;
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let workspace = workspace_by_id_locked(&tx, workspace_id)?;
        ensure_room_owner(&tx, workspace.room_id, actor)?;
        let projection_workspace = tx
            .query_row(
                "SELECT workspace_id FROM projection_binding WHERE id = ?1",
                params![input.projection_id.0],
                |row| row.get::<_, i64>(0).map(WorkspaceId),
            )
            .optional()?
            .ok_or(MetadataError::ProjectionBindingNotFound(
                input.projection_id,
            ))?;
        if projection_workspace != workspace_id {
            return Err(MetadataError::InvalidRepositoryBinding);
        }
        tx.execute(
            "INSERT INTO repository_binding (
                 workspace_id, projection_id, adapter_id, repository_identity,
                 adapter_generation, executable_version, network_enabled, branch, head,
                 revision, created_at, updated_at
             ) VALUES (?1, ?2, 'sift/git', ?3, ?4, ?5, ?6, ?7, ?8, 1, ?9, ?9)",
            params![
                workspace_id.0,
                input.projection_id.0,
                input.repository_identity,
                input.adapter_generation,
                input.executable_version,
                input.network_enabled,
                input.branch,
                input.head,
                now,
            ],
        )
        .map_err(map_binding_constraint)?;
        let binding =
            repository_binding_by_id_locked(&tx, RepositoryBindingId(tx.last_insert_rowid()))?;
        tx.commit()?;
        Ok(binding)
    }

    pub fn repository_binding_for_principal(
        &self,
        id: RepositoryBindingId,
        principal: PrincipalId,
        writable: bool,
    ) -> Result<RepositoryBindingRecord> {
        let conn = self.conn()?;
        let binding = repository_binding_by_id_locked(&conn, id)?;
        let workspace = workspace_by_id_locked(&conn, binding.binding.workspace_id)?;
        ensure_room_access(&conn, workspace.room_id, principal, writable)?;
        Ok(binding)
    }

    pub fn repository_binding_for_workspace(
        &self,
        workspace_id: WorkspaceId,
        principal: PrincipalId,
    ) -> Result<Option<RepositoryBindingRecord>> {
        let conn = self.conn()?;
        let workspace = workspace_by_id_locked(&conn, workspace_id)?;
        ensure_room_access(&conn, workspace.room_id, principal, false)?;
        let id = conn
            .query_row(
                "SELECT id FROM repository_binding WHERE workspace_id = ?1",
                params![workspace_id.0],
                |row| row.get::<_, i64>(0).map(RepositoryBindingId),
            )
            .optional()?;
        id.map(|id| repository_binding_by_id_locked(&conn, id))
            .transpose()
    }

    pub fn observe_repository(
        &self,
        id: RepositoryBindingId,
        actor: PrincipalId,
        observation: RepositoryObservation,
    ) -> Result<RepositoryBindingRecord> {
        validate_ref(observation.branch.as_deref())?;
        validate_oid(observation.head.as_deref())?;
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = repository_binding_by_id_locked(&tx, id)?;
        let workspace = workspace_by_id_locked(&tx, current.binding.workspace_id)?;
        ensure_room_access(&tx, workspace.room_id, actor, true)?;
        ensure_revision(&current.binding, observation.expected_revision)?;
        tx.execute(
            "UPDATE repository_binding SET branch = ?1, head = ?2,
                 revision = revision + 1, updated_at = ?3 WHERE id = ?4",
            params![observation.branch, observation.head, now, id.0],
        )?;
        let updated = repository_binding_by_id_locked(&tx, id)?;
        tx.commit()?;
        Ok(updated)
    }

    pub fn record_repository_commit(
        &self,
        id: RepositoryBindingId,
        actor: PrincipalId,
        observation: RepositoryObservation,
        commit: NewRepositoryCommit,
    ) -> Result<RepositoryBindingRecord> {
        validate_oid(Some(&commit.commit_oid))?;
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = repository_binding_by_id_locked(&tx, id)?;
        let workspace = workspace_by_id_locked(&tx, current.binding.workspace_id)?;
        ensure_room_access(&tx, workspace.room_id, actor, true)?;
        ensure_revision(&current.binding, observation.expected_revision)?;
        let checkpoint_valid: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM workspace_checkpoint
             WHERE id = ?1 AND workspace_id = ?2 AND workspace_revision = ?3)",
            params![
                commit.checkpoint_id.0,
                workspace.id.0,
                commit.workspace_revision.0
            ],
            |row| row.get(0),
        )?;
        if !checkpoint_valid {
            return Err(MetadataError::InvalidRepositoryBinding);
        }
        tx.execute(
            "INSERT INTO repository_commit (
                 binding_id, commit_oid, checkpoint_id, workspace_revision, created_by, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id.0,
                commit.commit_oid,
                commit.checkpoint_id.0,
                commit.workspace_revision.0,
                actor.0,
                now,
            ],
        )?;
        tx.execute(
            "UPDATE repository_binding SET branch = ?1, head = ?2,
                 revision = revision + 1, updated_at = ?3 WHERE id = ?4",
            params![observation.branch, observation.head, now, id.0],
        )?;
        let updated = repository_binding_by_id_locked(&tx, id)?;
        tx.commit()?;
        Ok(updated)
    }

    pub async fn set_repository_credential(
        &self,
        id: RepositoryBindingId,
        actor: PrincipalId,
        expected_revision: u64,
        secret: &[u8],
    ) -> Result<RepositoryBindingRecord> {
        if secret.is_empty() || secret.len() > 16 * 1024 {
            return Err(MetadataError::InvalidRepositoryBinding);
        }
        let handle = Uuid::new_v4().to_string();
        self.secrets
            .put(VCS_CREDENTIAL_NAMESPACE, &handle, secret)
            .await?;
        let backend = self.backend.clone();
        let db_handle = handle.clone();
        let result = sqlite_blocking_repository(move || {
            let mut conn = backend.conn()?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let current = repository_binding_by_id_locked(&tx, id)?;
            let workspace = workspace_by_id_locked(&tx, current.binding.workspace_id)?;
            ensure_room_owner(&tx, workspace.room_id, actor)?;
            ensure_revision(&current.binding, expected_revision)?;
            tx.execute(
                "UPDATE repository_binding SET credential_handle = ?1,
                     revision = revision + 1, updated_at = ?2 WHERE id = ?3",
                params![db_handle, now_text(), id.0],
            )?;
            let updated = repository_binding_by_id_locked(&tx, id)?;
            tx.commit()?;
            Ok((updated, current.credential_handle))
        })
        .await;
        match result {
            Ok((updated, previous)) => {
                if let Some(previous) = previous {
                    self.secrets
                        .delete(VCS_CREDENTIAL_NAMESPACE, &previous)
                        .await?;
                }
                Ok(updated)
            }
            Err(error) => {
                let _ = self.secrets.delete(VCS_CREDENTIAL_NAMESPACE, &handle).await;
                Err(error)
            }
        }
    }

    pub async fn repository_credential(
        &self,
        id: RepositoryBindingId,
        principal: PrincipalId,
    ) -> Result<Option<Vec<u8>>> {
        let binding = self.repository_binding_for_principal(id, principal, true)?;
        let Some(handle) = binding.credential_handle else {
            return Ok(None);
        };
        self.secrets.get(VCS_CREDENTIAL_NAMESPACE, &handle).await
    }

    pub async fn delete_repository_binding(
        &self,
        id: RepositoryBindingId,
        actor: PrincipalId,
        expected_revision: u64,
    ) -> Result<()> {
        let backend = self.backend.clone();
        let credential = sqlite_blocking_repository(move || {
            let mut conn = backend.conn()?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let current = repository_binding_by_id_locked(&tx, id)?;
            let workspace = workspace_by_id_locked(&tx, current.binding.workspace_id)?;
            ensure_room_owner(&tx, workspace.room_id, actor)?;
            ensure_revision(&current.binding, expected_revision)?;
            tx.execute(
                "DELETE FROM repository_binding WHERE id = ?1",
                params![id.0],
            )?;
            tx.commit()?;
            Ok(current.credential_handle)
        })
        .await?;
        if let Some(handle) = credential {
            self.secrets
                .delete(VCS_CREDENTIAL_NAMESPACE, &handle)
                .await?;
        }
        Ok(())
    }
}

async fn sqlite_blocking_repository<T>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T>
where
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|error| MetadataError::BlockingTask(error.to_string()))?
}

fn repository_binding_by_id_locked(
    conn: &Connection,
    id: RepositoryBindingId,
) -> Result<RepositoryBindingRecord> {
    conn.query_row(
        "SELECT id, workspace_id, projection_id, adapter_id, repository_identity,
                adapter_generation, executable_version, network_enabled, branch, head,
                credential_handle, revision, created_at, updated_at
         FROM repository_binding WHERE id = ?1",
        params![id.0],
        |row| {
            let revision = row.get::<_, i64>(11)?;
            let credential_handle = row.get::<_, Option<String>>(10)?;
            Ok(RepositoryBindingRecord {
                binding: RepositoryBinding {
                    id: RepositoryBindingId(row.get(0)?),
                    workspace_id: WorkspaceId(row.get(1)?),
                    projection_id: ProjectionBindingId(row.get(2)?),
                    adapter_id: row.get(3)?,
                    repository_identity: row.get(4)?,
                    adapter_generation: row.get(5)?,
                    executable_version: row.get(6)?,
                    network_enabled: row.get(7)?,
                    branch: row.get(8)?,
                    head: row.get(9)?,
                    credential_handle_present: credential_handle.is_some(),
                    revision: u64::try_from(revision)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(11, revision))?,
                    created_at: parse_time_sql(row.get(12)?)?,
                    updated_at: parse_time_sql(row.get(13)?)?,
                },
                credential_handle,
            })
        },
    )
    .optional()?
    .ok_or(MetadataError::RepositoryBindingNotFound(id))
}

fn validate_new_binding(input: &NewRepositoryBinding) -> Result<()> {
    if input.repository_identity.len() != 64
        || !input
            .repository_identity
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || input.adapter_generation.is_empty()
        || input.adapter_generation.len() > 128
        || input.executable_version.is_empty()
        || input.executable_version.len() > 256
    {
        return Err(MetadataError::InvalidRepositoryBinding);
    }
    validate_ref(input.branch.as_deref())?;
    validate_oid(input.head.as_deref())
}

fn validate_ref(value: Option<&str>) -> Result<()> {
    if value.is_some_and(|value| {
        value.is_empty()
            || value.len() > 1024
            || value.contains('\0')
            || value.contains('\n')
            || value.starts_with('-')
    }) {
        Err(MetadataError::InvalidRepositoryBinding)
    } else {
        Ok(())
    }
}

fn validate_oid(value: Option<&str>) -> Result<()> {
    if value.is_some_and(|value| {
        !matches!(value.len(), 40 | 64) || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
    }) {
        Err(MetadataError::InvalidRepositoryBinding)
    } else {
        Ok(())
    }
}

fn ensure_revision(binding: &RepositoryBinding, expected: u64) -> Result<()> {
    if binding.revision == expected {
        Ok(())
    } else {
        Err(MetadataError::RepositoryRevisionConflict {
            expected,
            current: binding.revision,
        })
    }
}

fn map_binding_constraint(error: rusqlite::Error) -> MetadataError {
    match error {
        rusqlite::Error::SqliteFailure(ref inner, _)
            if inner.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            MetadataError::InvalidRepositoryBinding
        }
        other => MetadataError::Sqlite(other),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sift_protocol::{ProjectionHealth, ProjectionMode};

    use super::*;
    use crate::{MemorySecretStore, NewProjectionBinding, NewRoom, RoomKind, TenantId};

    #[test]
    fn repository_credential_request_debug_is_redacted() {
        let request = crate::http::SetVcsCredentialRequest {
            expected_revision: 1,
            username: sift_protocol::RedactedString("alice-secret".into()),
            password: sift_protocol::RedactedString("password-secret".into()),
        };
        let debug = format!("{request:?}");
        assert!(!debug.contains("alice-secret"));
        assert!(!debug.contains("password-secret"));
        assert!(debug.contains("[REDACTED]"));
    }

    #[tokio::test]
    async fn repository_credentials_are_opaque_to_sqlite_and_revisioned() {
        let secrets = Arc::new(MemorySecretStore::new());
        let store = MetadataStore::open_in_memory(secrets.clone()).unwrap();
        store.bootstrap_local("repository-test").unwrap();
        let actor = PrincipalId(1);
        let room = store
            .create_room(
                TenantId(1),
                actor,
                NewRoom {
                    name: "room".into(),
                    kind: RoomKind::Shared,
                },
            )
            .unwrap();
        let workspace = store.create_workspace(room.id, actor, "database").unwrap();
        let projection = store
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
        let binding = store
            .create_repository_binding(
                workspace.id,
                actor,
                NewRepositoryBinding {
                    projection_id: projection.binding.id,
                    repository_identity: "a".repeat(64),
                    adapter_generation: "git-v1".into(),
                    executable_version: "git version test".into(),
                    network_enabled: true,
                    branch: Some("main".into()),
                    head: None,
                },
            )
            .unwrap();
        let secret = br#"{"username":"alice","password":"not-in-sqlite"}"#;
        let updated = store
            .set_repository_credential(binding.binding.id, actor, 1, secret)
            .await
            .unwrap();
        assert_eq!(updated.binding.revision, 2);
        assert!(updated.binding.credential_handle_present);
        assert_eq!(
            store
                .repository_credential(binding.binding.id, actor)
                .await
                .unwrap()
                .unwrap(),
            secret
        );
        let conn = store.conn().unwrap();
        let stored: String = conn
            .query_row(
                "SELECT credential_handle FROM repository_binding WHERE id = ?1",
                params![binding.binding.id.0],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!stored.contains("alice"));
        assert!(!stored.contains("not-in-sqlite"));
        drop(conn);
        store
            .delete_repository_binding(binding.binding.id, actor, 2)
            .await
            .unwrap();
        assert!(secrets.is_empty());
    }
}
