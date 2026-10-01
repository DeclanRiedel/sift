//! Private, editable benchmark workloads. SQLite indexes opaque SecretStore handles.
use rusqlite::{params, OptionalExtension};
use sift_protocol::{SavedBenchmarkDefinition, SavedBenchmarkDefinitionSummary};
use uuid::Uuid;

use super::{sqlite_blocking, MetadataError, MetadataStore, PrincipalId, Result, TenantId};

pub(crate) const BENCHMARK_DEFINITION_SECRET_NAMESPACE: &str = "benchmark-definitions";
const MAX_BYTES: usize = 1024 * 1024 + 4096;
const OWNER_BYTES: i64 = 32 * 1024 * 1024;

impl MetadataStore {
    pub async fn save_benchmark_definition(
        &self,
        tenant: TenantId,
        owner: PrincipalId,
        definition: SavedBenchmarkDefinition,
    ) -> Result<SavedBenchmarkDefinition> {
        let bytes = definition_bytes(&definition)?;
        let store = self.clone();
        sqlite_blocking(move || store.benchmark_definition_membership(tenant, owner)).await?;
        let handle = Uuid::new_v4().to_string();
        self.secrets
            .put(BENCHMARK_DEFINITION_SECRET_NAMESPACE, &handle, &bytes)
            .await?;
        let store = self.clone();
        let record = definition.clone();
        let stored_handle = handle.clone();
        let inserted = sqlite_blocking(move || {
            let mut conn = store.conn()?;
            let tx = conn.transaction()?;
            require_membership(&tx, tenant, owner)?;
            let (count, used): (i64, i64) = tx.query_row(
                "SELECT COUNT(*),COALESCE(SUM(payload_bytes),0) FROM benchmark_definition WHERE tenant_id=?1 AND owner_principal_id=?2",
                params![tenant.0,owner.0],
                |row| Ok((row.get(0)?,row.get(1)?)),
            )?;
            if count >= 100 || used + bytes.len() as i64 > OWNER_BYTES {
                return Err(MetadataError::InvalidBenchmarkDefinition("definition limit reached (100 definitions / 32 MiB per owner)".into()));
            }
            tx.execute(
                "INSERT INTO benchmark_definition(id,tenant_id,owner_principal_id,revision,created_at,updated_at,secret_handle,payload_bytes) VALUES(?1,?2,?3,1,?4,?5,?6,?7)",
                params![record.id.to_string(),tenant.0,owner.0,record.created_at.to_rfc3339(),record.updated_at.to_rfc3339(),stored_handle,bytes.len() as i64],
            )?;
            tx.commit()?;
            Ok(())
        }).await;
        if let Err(error) = inserted {
            self.secrets
                .delete(BENCHMARK_DEFINITION_SECRET_NAMESPACE, &handle)
                .await?;
            return Err(error);
        }
        Ok(definition)
    }

    fn benchmark_definition_membership(&self, tenant: TenantId, owner: PrincipalId) -> Result<()> {
        let conn = self.conn()?;
        require_membership(&conn, tenant, owner)
    }

    async fn benchmark_definition_row(
        &self,
        tenant: TenantId,
        owner: PrincipalId,
        id: Uuid,
    ) -> Result<(u64, String)> {
        let store = self.clone();
        sqlite_blocking(move || {
            let conn = store.conn()?;
            require_membership(&conn, tenant, owner)?;
            conn.query_row(
                "SELECT revision,secret_handle FROM benchmark_definition WHERE id=?1 AND tenant_id=?2 AND owner_principal_id=?3",
                params![id.to_string(),tenant.0,owner.0],
                |row| Ok((row.get::<_,u64>(0)?,row.get::<_,String>(1)?)),
            ).optional()?.ok_or(MetadataError::BenchmarkDefinitionNotFound)
        }).await
    }

    pub async fn get_benchmark_definition(
        &self,
        tenant: TenantId,
        owner: PrincipalId,
        id: Uuid,
    ) -> Result<SavedBenchmarkDefinition> {
        let (revision, handle) = self.benchmark_definition_row(tenant, owner, id).await?;
        let bytes = self.secrets
            .get(BENCHMARK_DEFINITION_SECRET_NAMESPACE, &handle)
            .await?
            .ok_or_else(|| MetadataError::InvalidBenchmarkDefinition("definition payload unavailable; restore matching secret backup or delete entry".into()))?;
        if bytes.len() > MAX_BYTES {
            return Err(MetadataError::InvalidBenchmarkDefinition(
                "definition exceeds size limit".into(),
            ));
        }
        let saved: SavedBenchmarkDefinition = serde_json::from_slice(&bytes)?;
        if saved.id != id || saved.revision != revision {
            return Err(MetadataError::InvalidBenchmarkDefinition(
                "definition identity or revision mismatch".into(),
            ));
        }
        Ok(saved)
    }

    pub async fn list_benchmark_definitions(
        &self,
        tenant: TenantId,
        owner: PrincipalId,
        cursor: Option<Uuid>,
        limit: u32,
    ) -> Result<Vec<SavedBenchmarkDefinitionSummary>> {
        let store = self.clone();
        let rows = sqlite_blocking(move || {
            let conn = store.conn()?;
            require_membership(&conn, tenant, owner)?;
            let cursor_time: Option<String> = cursor.map(|id| conn.query_row(
                "SELECT updated_at FROM benchmark_definition WHERE id=?1 AND tenant_id=?2 AND owner_principal_id=?3",
                params![id.to_string(),tenant.0,owner.0], |row| row.get(0),
            ).optional()?.ok_or(MetadataError::BenchmarkDefinitionNotFound)).transpose()?;
            let mut stmt = conn.prepare(
                "SELECT id,revision,updated_at FROM benchmark_definition WHERE tenant_id=?1 AND owner_principal_id=?2 AND (?3 IS NULL OR updated_at < ?3 OR (updated_at = ?3 AND id < ?4)) ORDER BY updated_at DESC,id DESC LIMIT ?5"
            )?;
            let rows = stmt.query_map(
                params![tenant.0,owner.0,cursor_time,cursor.map(|id| id.to_string()),limit.clamp(1,51)],
                |row| Ok((row.get::<_,String>(0)?,row.get::<_,u64>(1)?,row.get::<_,String>(2)?)),
            )?.collect::<std::result::Result<Vec<_>,_>>()?;
            Ok(rows)
        }).await?;
        let mut summaries = Vec::with_capacity(rows.len());
        for (id, revision, updated_at) in rows {
            let id =
                Uuid::parse_str(&id).map_err(|_| MetadataError::BenchmarkDefinitionNotFound)?;
            match self.get_benchmark_definition(tenant, owner, id).await {
                Ok(saved) => summaries.push(SavedBenchmarkDefinitionSummary {
                    id,
                    revision: saved.revision,
                    updated_at: saved.updated_at,
                    name: saved.name,
                    engine: Some(saved.engine),
                    parameter_count: saved.parameter_count,
                    payload_available: true,
                }),
                Err(MetadataError::InvalidBenchmarkDefinition(_)) => {
                    summaries.push(SavedBenchmarkDefinitionSummary {
                        id,
                        revision,
                        updated_at: super::parse_time_sql(updated_at)?,
                        name: "Definition unavailable — restore secrets or delete entry".into(),
                        engine: None,
                        parameter_count: 0,
                        payload_available: false,
                    })
                }
                Err(error) => return Err(error),
            }
        }
        Ok(summaries)
    }

    pub async fn update_benchmark_definition(
        &self,
        tenant: TenantId,
        owner: PrincipalId,
        definition: SavedBenchmarkDefinition,
        expected_revision: u64,
    ) -> Result<SavedBenchmarkDefinition> {
        let (current_revision, old_handle) = self
            .benchmark_definition_row(tenant, owner, definition.id)
            .await?;
        if current_revision != expected_revision
            || Some(definition.revision) != expected_revision.checked_add(1)
        {
            return Err(MetadataError::BenchmarkDefinitionRevisionConflict);
        }
        let bytes = definition_bytes(&definition)?;
        let new_handle = Uuid::new_v4().to_string();
        self.secrets
            .put(BENCHMARK_DEFINITION_SECRET_NAMESPACE, &new_handle, &bytes)
            .await?;
        let store = self.clone();
        let record = definition.clone();
        let stored_handle = new_handle.clone();
        let prior_handle = old_handle.clone();
        let updated = sqlite_blocking(move || {
            let mut conn = store.conn()?;
            let tx = conn.transaction()?;
            require_membership(&tx, tenant, owner)?;
            let used: i64 = tx.query_row(
                "SELECT COALESCE(SUM(payload_bytes),0) FROM benchmark_definition WHERE tenant_id=?1 AND owner_principal_id=?2 AND id<>?3",
                params![tenant.0,owner.0,record.id.to_string()], |row| row.get(0),
            )?;
            if used + bytes.len() as i64 > OWNER_BYTES {
                return Err(MetadataError::InvalidBenchmarkDefinition("definition payload limit reached".into()));
            }
            let changed = tx.execute(
                "UPDATE benchmark_definition SET revision=?1,updated_at=?2,secret_handle=?3,payload_bytes=?4 WHERE id=?5 AND tenant_id=?6 AND owner_principal_id=?7 AND revision=?8 AND secret_handle=?9",
                params![record.revision,record.updated_at.to_rfc3339(),stored_handle,bytes.len() as i64,record.id.to_string(),tenant.0,owner.0,expected_revision,prior_handle],
            )?;
            if changed != 1 {
                return Err(MetadataError::BenchmarkDefinitionRevisionConflict);
            }
            tx.commit()?;
            Ok(())
        }).await;
        if let Err(error) = updated {
            self.secrets
                .delete(BENCHMARK_DEFINITION_SECRET_NAMESPACE, &new_handle)
                .await?;
            return Err(error);
        }
        // The new index is committed. A failed cleanup leaves an unreachable
        // old payload, but must not report a committed edit as a failed edit.
        if self
            .secrets
            .delete(BENCHMARK_DEFINITION_SECRET_NAMESPACE, &old_handle)
            .await
            .is_err()
        {
            tracing::warn!("old benchmark definition payload cleanup failed");
        }
        Ok(definition)
    }

    pub async fn delete_benchmark_definition(
        &self,
        tenant: TenantId,
        owner: PrincipalId,
        id: Uuid,
    ) -> Result<()> {
        let (_, handle) = self.benchmark_definition_row(tenant, owner, id).await?;
        self.secrets
            .delete(BENCHMARK_DEFINITION_SECRET_NAMESPACE, &handle)
            .await?;
        let store = self.clone();
        sqlite_blocking(move || {
            let conn = store.conn()?;
            let changed = conn.execute(
                "DELETE FROM benchmark_definition WHERE id=?1 AND tenant_id=?2 AND owner_principal_id=?3 AND secret_handle=?4",
                params![id.to_string(),tenant.0,owner.0,handle],
            )?;
            if changed != 1 {
                return Err(MetadataError::BenchmarkDefinitionRevisionConflict);
            }
            Ok(())
        }).await
    }
}

fn definition_bytes(definition: &SavedBenchmarkDefinition) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(definition)?;
    if bytes.len() > MAX_BYTES
        || definition.revision == 0
        || definition.name.trim().is_empty()
        || definition.name.len() > 200
        || definition.sql.is_empty()
        || definition.sql.len() > 1024 * 1024
        || definition.parameter_count > 256
        || definition.limits.validate().is_err()
    {
        return Err(MetadataError::InvalidBenchmarkDefinition(
            "invalid name, SQL, parameter count or limits".into(),
        ));
    }
    Ok(bytes)
}

fn require_membership(
    conn: &rusqlite::Connection,
    tenant: TenantId,
    owner: PrincipalId,
) -> Result<()> {
    let member: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM membership WHERE tenant_id=?1 AND principal_id=?2)",
        params![tenant.0, owner.0],
        |row| row.get(0),
    )?;
    if member {
        Ok(())
    } else {
        Err(MetadataError::BenchmarkDefinitionNotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FileSecretStore, MembershipRole, SecretStore};
    use std::sync::Arc;

    #[tokio::test]
    async fn private_definitions_reopen_update_by_revision_and_hide_sensitive_payloads() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("metadata.sqlite");
        let key = dir.path().join("key");
        let encrypted = dir.path().join("secrets");
        std::fs::write(&key, "01".repeat(32)).unwrap();
        let secrets = Arc::new(FileSecretStore::open(&encrypted, &key).unwrap());
        let store = MetadataStore::open(&db, secrets.clone()).unwrap();
        store.apply_migrations(false).unwrap();
        store.bootstrap_local("owner").unwrap();
        let peer = store
            .create_principal("benchmark-peer", "peer", None)
            .unwrap();
        store
            .upsert_tenant_membership(TenantId(1), peer.id, MembershipRole::Member)
            .unwrap();
        let now = chrono::Utc::now();
        let saved = SavedBenchmarkDefinition {
            id: Uuid::new_v4(),
            revision: 1,
            created_at: now,
            updated_at: now,
            name: "private definition name".into(),
            engine: sift_protocol::Engine::Postgres,
            sql: "SELECT 'private-definition-literal' WHERE id = $1".into(),
            parameter_count: 1,
            limits: sift_protocol::BenchmarkLimits::default(),
        };
        store
            .save_benchmark_definition(TenantId(1), PrincipalId(1), saved.clone())
            .await
            .unwrap();
        assert!(matches!(
            store
                .get_benchmark_definition(TenantId(1), peer.id, saved.id)
                .await,
            Err(MetadataError::BenchmarkDefinitionNotFound)
        ));
        assert!(store
            .list_benchmark_definitions(TenantId(1), peer.id, None, 50)
            .await
            .unwrap()
            .is_empty());
        assert!(store
            .list_benchmark_definitions(TenantId(1), PrincipalId(1), Some(Uuid::new_v4()), 50)
            .await
            .is_err());
        let page = store
            .list_benchmark_definitions(TenantId(1), PrincipalId(1), None, 50)
            .await
            .unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].parameter_count, 1);
        let mut updated = saved.clone();
        updated.revision = 2;
        updated.updated_at = chrono::Utc::now();
        updated.name = "updated private definition".into();
        assert!(matches!(
            store
                .update_benchmark_definition(TenantId(1), PrincipalId(1), updated.clone(), u64::MAX)
                .await,
            Err(MetadataError::BenchmarkDefinitionRevisionConflict)
        ));
        store
            .update_benchmark_definition(TenantId(1), PrincipalId(1), updated.clone(), 1)
            .await
            .unwrap();
        assert!(matches!(
            store
                .update_benchmark_definition(TenantId(1), PrincipalId(1), updated.clone(), 1)
                .await,
            Err(MetadataError::BenchmarkDefinitionRevisionConflict)
        ));
        let (_, handle) = store
            .benchmark_definition_row(TenantId(1), PrincipalId(1), saved.id)
            .await
            .unwrap();
        drop(store);
        drop(secrets);
        for path in [&db, &encrypted] {
            let bytes = std::fs::read(path).unwrap();
            for sensitive in ["private-definition-literal", "updated private definition"] {
                assert!(!bytes
                    .windows(sensitive.len())
                    .any(|window| window == sensitive.as_bytes()));
            }
        }
        let secrets = Arc::new(FileSecretStore::open(&encrypted, &key).unwrap());
        let store = MetadataStore::open(&db, secrets.clone()).unwrap();
        assert_eq!(
            store
                .get_benchmark_definition(TenantId(1), PrincipalId(1), saved.id)
                .await
                .unwrap()
                .revision,
            2
        );
        secrets
            .delete(BENCHMARK_DEFINITION_SECRET_NAMESPACE, &handle)
            .await
            .unwrap();
        let page = store
            .list_benchmark_definitions(TenantId(1), PrincipalId(1), None, 50)
            .await
            .unwrap();
        assert!(!page[0].payload_available);
        store
            .delete_benchmark_definition(TenantId(1), PrincipalId(1), saved.id)
            .await
            .unwrap();
        assert!(store
            .list_benchmark_definitions(TenantId(1), PrincipalId(1), None, 50)
            .await
            .unwrap()
            .is_empty());
    }
}
