use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};
use sift_protocol::{
    TransferDirection, TransferRecipe, TransferRecipeId, WorkspaceArtifact, WorkspaceArtifactId,
    WorkspaceId,
};

use crate::workspace::{ensure_room_access, workspace_by_id_locked};
use crate::{
    now_text, parse_time_sql, MetadataError, MetadataStore, NewTransferRecipe, PrincipalId, Result,
    WorkspaceArtifactRecord,
};

const MAX_ARTIFACT_BYTES: usize = 64 * 1024 * 1024;

impl MetadataStore {
    pub fn create_transfer_recipe(
        &self,
        workspace_id: WorkspaceId,
        actor: PrincipalId,
        input: NewTransferRecipe,
    ) -> Result<TransferRecipe> {
        validate_recipe(&input)?;
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let workspace = workspace_by_id_locked(&tx, workspace_id)?;
        ensure_room_access(&tx, workspace.room_id, actor, true)?;
        let now = now_text();
        tx.execute(
            "INSERT INTO transfer_recipe
             (workspace_id, name, direction, source_json, sink_json, format_id,
              format_version, options_json, revision, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1, ?9, ?9)",
            params![
                workspace_id.0,
                input.name,
                direction_text(input.direction),
                serde_json::to_string(&input.source)?,
                serde_json::to_string(&input.sink)?,
                input.format_id,
                input.format_version,
                serde_json::to_string(&input.options)?,
                now,
            ],
        )
        .map_err(map_constraint)?;
        let recipe = recipe_by_id_locked(&tx, TransferRecipeId(tx.last_insert_rowid()))?;
        tx.commit()?;
        Ok(recipe)
    }

    pub fn list_transfer_recipes_for_principal(
        &self,
        workspace_id: WorkspaceId,
        actor: PrincipalId,
    ) -> Result<Vec<TransferRecipe>> {
        let conn = self.conn()?;
        let workspace = workspace_by_id_locked(&conn, workspace_id)?;
        ensure_room_access(&conn, workspace.room_id, actor, false)?;
        let mut statement =
            conn.prepare("SELECT id FROM transfer_recipe WHERE workspace_id = ?1 ORDER BY id")?;
        let ids = statement
            .query_map(params![workspace_id.0], |row| row.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter()
            .map(|id| recipe_by_id_locked(&conn, TransferRecipeId(id)))
            .collect()
    }

    pub fn transfer_recipe_for_principal(
        &self,
        id: TransferRecipeId,
        actor: PrincipalId,
        writable: bool,
    ) -> Result<TransferRecipe> {
        let conn = self.conn()?;
        let recipe = recipe_by_id_locked(&conn, id)?;
        let workspace = workspace_by_id_locked(&conn, recipe.workspace_id)?;
        ensure_room_access(&conn, workspace.room_id, actor, writable)?;
        Ok(recipe)
    }

    pub fn update_transfer_recipe(
        &self,
        id: TransferRecipeId,
        actor: PrincipalId,
        expected_revision: u64,
        input: NewTransferRecipe,
    ) -> Result<TransferRecipe> {
        validate_recipe(&input)?;
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = recipe_by_id_locked(&tx, id)?;
        let workspace = workspace_by_id_locked(&tx, current.workspace_id)?;
        ensure_room_access(&tx, workspace.room_id, actor, true)?;
        if current.revision != expected_revision {
            return Err(MetadataError::TransferRecipeRevisionConflict {
                expected: expected_revision,
                current: current.revision,
            });
        }
        tx.execute(
            "UPDATE transfer_recipe SET name = ?1, direction = ?2, source_json = ?3,
             sink_json = ?4, format_id = ?5, format_version = ?6, options_json = ?7,
             revision = revision + 1, updated_at = ?8 WHERE id = ?9",
            params![
                input.name,
                direction_text(input.direction),
                serde_json::to_string(&input.source)?,
                serde_json::to_string(&input.sink)?,
                input.format_id,
                input.format_version,
                serde_json::to_string(&input.options)?,
                now_text(),
                id.0,
            ],
        )
        .map_err(map_constraint)?;
        let updated = recipe_by_id_locked(&tx, id)?;
        tx.commit()?;
        Ok(updated)
    }

    pub fn delete_transfer_recipe(
        &self,
        id: TransferRecipeId,
        actor: PrincipalId,
        expected_revision: u64,
    ) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = recipe_by_id_locked(&tx, id)?;
        let workspace = workspace_by_id_locked(&tx, current.workspace_id)?;
        ensure_room_access(&tx, workspace.room_id, actor, true)?;
        if current.revision != expected_revision {
            return Err(MetadataError::TransferRecipeRevisionConflict {
                expected: expected_revision,
                current: current.revision,
            });
        }
        tx.execute("DELETE FROM transfer_recipe WHERE id = ?1", params![id.0])?;
        tx.commit()?;
        Ok(())
    }

    pub fn create_workspace_artifact(
        &self,
        workspace_id: WorkspaceId,
        actor: PrincipalId,
        content_type: &str,
        content: Vec<u8>,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<WorkspaceArtifact> {
        if content.is_empty()
            || content.len() > MAX_ARTIFACT_BYTES
            || content_type.is_empty()
            || content_type.len() > 128
        {
            return Err(MetadataError::InvalidTransferRecipe);
        }
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let workspace = workspace_by_id_locked(&tx, workspace_id)?;
        ensure_room_access(&tx, workspace.room_id, actor, true)?;
        let digest = format!("{:x}", Sha256::digest(&content));
        let byte_len =
            i64::try_from(content.len()).map_err(|_| MetadataError::InvalidTransferRecipe)?;
        tx.execute(
            "INSERT INTO workspace_artifact
             (workspace_id, content_type, digest, byte_len, content, expires_at, pinned, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7)",
            params![
                workspace_id.0,
                content_type,
                digest,
                byte_len,
                content,
                expires_at.map(|value| value.to_rfc3339()),
                now_text()
            ],
        )?;
        let record = artifact_by_id_locked(&tx, WorkspaceArtifactId(tx.last_insert_rowid()))?;
        tx.commit()?;
        Ok(record.artifact)
    }

    pub fn workspace_artifact_for_principal(
        &self,
        id: WorkspaceArtifactId,
        actor: PrincipalId,
    ) -> Result<WorkspaceArtifactRecord> {
        let conn = self.conn()?;
        let record = artifact_by_id_locked(&conn, id)?;
        let workspace = workspace_by_id_locked(&conn, record.artifact.workspace_id)?;
        ensure_room_access(&conn, workspace.room_id, actor, false)?;
        if record
            .artifact
            .expires_at
            .is_some_and(|expiry| expiry <= Utc::now())
        {
            return Err(MetadataError::WorkspaceArtifactNotFound(id));
        }
        Ok(record)
    }
}

fn validate_recipe(input: &NewTransferRecipe) -> Result<()> {
    let bundled = matches!(
        input.format_id.as_str(),
        "csv" | "tsv" | "jsonl" | "json_array" | "html" | "markdown" | "xlsx" | "sql"
    );
    let extension = input.format_id.len() <= 255
        && input
            .format_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'_' | b'-' | b'.'))
        && match input.direction {
            TransferDirection::Import => input.format_id.contains("/import_format/"),
            TransferDirection::Export => input.format_id.contains("/export_format/"),
        };
    let endpoints_valid = match input.direction {
        TransferDirection::Import => {
            input.source == sift_protocol::TransferEndpoint::Upload
                && input.sink == sift_protocol::TransferEndpoint::Table
                && (matches!(input.format_id.as_str(), "csv" | "xlsx") || extension)
        }
        TransferDirection::Export => {
            input.source == sift_protocol::TransferEndpoint::Query
                && input.sink == sift_protocol::TransferEndpoint::Artifact
                && (bundled || extension)
        }
    };
    if input.name.is_empty()
        || input.name.len() > 128
        || input.name.trim() != input.name
        || !(bundled || extension)
        || !endpoints_valid
        || input.format_version.is_empty()
        || input.format_version.len() > 64
        || (bundled && input.format_version != "1")
        || !input.options.is_object()
    {
        Err(MetadataError::InvalidTransferRecipe)
    } else {
        Ok(())
    }
}

fn recipe_by_id_locked(conn: &Connection, id: TransferRecipeId) -> Result<TransferRecipe> {
    conn.query_row(
        "SELECT id, workspace_id, name, direction, source_json, sink_json, format_id,
         format_version, options_json, revision, created_at, updated_at
         FROM transfer_recipe WHERE id = ?1",
        params![id.0],
        |row| {
            let revision = row.get::<_, i64>(9)?;
            Ok(TransferRecipe {
                id: TransferRecipeId(row.get(0)?),
                workspace_id: WorkspaceId(row.get(1)?),
                name: row.get(2)?,
                direction: parse_direction(row.get::<_, String>(3)?)?,
                source: serde_json::from_str(&row.get::<_, String>(4)?).map_err(json_error)?,
                sink: serde_json::from_str(&row.get::<_, String>(5)?).map_err(json_error)?,
                format_id: row.get(6)?,
                format_version: row.get(7)?,
                options: serde_json::from_str(&row.get::<_, String>(8)?).map_err(json_error)?,
                revision: u64::try_from(revision)
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(9, revision))?,
                created_at: parse_time_sql(row.get(10)?)?,
                updated_at: parse_time_sql(row.get(11)?)?,
            })
        },
    )
    .optional()?
    .ok_or(MetadataError::TransferRecipeNotFound(id))
}

fn artifact_by_id_locked(
    conn: &Connection,
    id: WorkspaceArtifactId,
) -> Result<WorkspaceArtifactRecord> {
    conn.query_row(
        "SELECT id, workspace_id, content_type, digest, byte_len, content, expires_at, pinned, created_at
         FROM workspace_artifact WHERE id = ?1", params![id.0], |row| {
            let byte_len = row.get::<_, i64>(4)?;
            Ok(WorkspaceArtifactRecord { artifact: WorkspaceArtifact {
                id: WorkspaceArtifactId(row.get(0)?), workspace_id: WorkspaceId(row.get(1)?),
                content_type: row.get(2)?, digest: row.get(3)?,
                byte_len: u64::try_from(byte_len).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(4, byte_len))?,
                expires_at: row.get::<_, Option<String>>(6)?.map(parse_time_sql).transpose()?,
                pinned: row.get(7)?, created_at: parse_time_sql(row.get(8)?)?,
            }, content: row.get(5)? })
        }
    ).optional()?.ok_or(MetadataError::WorkspaceArtifactNotFound(id))
}

fn direction_text(value: TransferDirection) -> &'static str {
    match value {
        TransferDirection::Import => "import",
        TransferDirection::Export => "export",
    }
}
fn parse_direction(value: String) -> rusqlite::Result<TransferDirection> {
    match value.as_str() {
        "import" => Ok(TransferDirection::Import),
        "export" => Ok(TransferDirection::Export),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}
fn json_error(error: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}
fn map_constraint(error: rusqlite::Error) -> MetadataError {
    match error {
        rusqlite::Error::SqliteFailure(ref inner, _)
            if inner.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            MetadataError::InvalidTransferRecipe
        }
        other => MetadataError::Sqlite(other),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sift_protocol::{TransferEndpoint, WorkspaceArtifactId};

    use crate::{secrets::MemorySecretStore, NewRoom, RoomKind, TenantId};

    use super::*;

    #[tokio::test]
    async fn recipes_are_revisioned_and_artifacts_publish_atomically() {
        let store = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        store.bootstrap_local("transfer-test").unwrap();
        let actor = PrincipalId(1);
        let room = store
            .create_room(
                TenantId(1),
                actor,
                NewRoom {
                    name: "transfer-room".into(),
                    kind: RoomKind::Shared,
                },
            )
            .unwrap();
        let workspace = store
            .create_workspace(room.id, actor, "transfer-workspace")
            .unwrap();
        let input = NewTransferRecipe {
            name: "markdown".into(),
            direction: TransferDirection::Export,
            source: TransferEndpoint::Query,
            sink: TransferEndpoint::Artifact,
            format_id: "markdown".into(),
            format_version: "1".into(),
            options: serde_json::json!({}),
        };
        let recipe = store
            .create_transfer_recipe(workspace.id, actor, input.clone())
            .unwrap();
        assert_eq!(recipe.revision, 1);
        assert!(matches!(
            store.update_transfer_recipe(recipe.id, actor, 99, input.clone()),
            Err(MetadataError::TransferRecipeRevisionConflict { .. })
        ));
        let updated = store
            .update_transfer_recipe(recipe.id, actor, 1, input)
            .unwrap();
        assert_eq!(updated.revision, 2);

        assert!(store
            .create_workspace_artifact(workspace.id, actor, "text/plain", Vec::new(), None)
            .is_err());
        let artifact = store
            .create_workspace_artifact(
                workspace.id,
                actor,
                "text/plain",
                b"complete".to_vec(),
                None,
            )
            .unwrap();
        let record = store
            .workspace_artifact_for_principal(artifact.id, actor)
            .unwrap();
        assert_eq!(record.content, b"complete");
        assert_eq!(record.artifact.byte_len, 8);
        assert_eq!(record.artifact.digest.len(), 64);
        store.sanitize_phase_l_backup_snapshot().unwrap();
        assert!(matches!(
            store.workspace_artifact_for_principal(artifact.id, actor),
            Err(MetadataError::WorkspaceArtifactNotFound(_))
        ));
        assert!(matches!(
            store.workspace_artifact_for_principal(WorkspaceArtifactId(999), actor),
            Err(MetadataError::WorkspaceArtifactNotFound(_))
        ));
    }
}
