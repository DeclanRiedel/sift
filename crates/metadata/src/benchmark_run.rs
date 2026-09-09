//! Private immutable snapshots. Sensitive payloads are only in SecretStore.
use rusqlite::{params, OptionalExtension};
use sift_protocol::{SavedBenchmarkRun, SavedBenchmarkRunSummary};
use uuid::Uuid;

use super::{sqlite_blocking, MetadataError, MetadataStore, PrincipalId, Result, TenantId};

pub(crate) const BENCHMARK_SECRET_NAMESPACE: &str = "benchmark-runs";
const MAX_BYTES: usize = 4 * 1024 * 1024;
const OWNER_BYTES: i64 = 64 * 1024 * 1024;

impl MetadataStore {
    pub async fn save_benchmark_run(
        &self,
        tenant: TenantId,
        owner: PrincipalId,
        saved: SavedBenchmarkRun,
    ) -> Result<SavedBenchmarkRun> {
        let bytes = serde_json::to_vec(&saved)?;
        if bytes.len() > MAX_BYTES || saved.name.trim().is_empty() || saved.name.len() > 200 {
            return Err(MetadataError::InvalidBenchmarkRun(
                "name must be 1–200 bytes and snapshot at most 4 MiB".into(),
            ));
        }
        // Check membership before writing secret bytes. Recheck under the insert transaction.
        let store = self.clone();
        sqlite_blocking(move || store.benchmark_membership(tenant, owner)).await?;
        let handle = Uuid::new_v4().to_string();
        self.secrets
            .put(BENCHMARK_SECRET_NAMESPACE, &handle, &bytes)
            .await?;
        let store = self.clone();
        let record = saved.clone();
        let stored_handle = handle.clone();
        let inserted = sqlite_blocking(move || {
            let mut conn = store.conn()?;
            let tx = conn.transaction()?;
            require_membership(&tx, tenant, owner)?;
            let (count, used): (i64, i64) = tx.query_row(
                "SELECT COUNT(*), COALESCE(SUM(payload_bytes),0) FROM benchmark_run WHERE tenant_id=?1 AND owner_principal_id=?2",
                params![tenant.0, owner.0], |row| Ok((row.get(0)?,row.get(1)?)))?;
            if count >= 500 || used + bytes.len() as i64 > OWNER_BYTES {
                return Err(MetadataError::InvalidBenchmarkRun("saved-run limit reached (500 runs / 64 MiB per owner); delete unwanted runs".into()));
            }
            let duplicate: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM benchmark_run WHERE tenant_id=?1 AND owner_principal_id=?2 AND run_id=?3)", params![tenant.0, owner.0, record.report.run_id.to_string()], |row| row.get(0))?;
            if duplicate {
                return Err(MetadataError::InvalidBenchmarkRun("this run is already saved; saved snapshots cannot be overwritten".into()));
            }
            tx.execute("INSERT INTO benchmark_run(id,tenant_id,owner_principal_id,run_id,saved_at,secret_handle,payload_bytes) VALUES(?1,?2,?3,?4,?5,?6,?7)", params![record.id.to_string(),tenant.0,owner.0,record.report.run_id.to_string(),record.saved_at.to_rfc3339(),stored_handle,bytes.len() as i64])?;
            tx.commit()?;
            Ok(())
        }).await;
        if let Err(error) = inserted {
            self.secrets
                .delete(BENCHMARK_SECRET_NAMESPACE, &handle)
                .await?;
            return Err(error);
        }
        Ok(saved)
    }

    fn benchmark_membership(&self, tenant: TenantId, owner: PrincipalId) -> Result<()> {
        let conn = self.conn()?;
        require_membership(&conn, tenant, owner)
    }

    async fn benchmark_handle(
        &self,
        tenant: TenantId,
        owner: PrincipalId,
        id: Uuid,
    ) -> Result<String> {
        let store = self.clone();
        sqlite_blocking(move || {
            let conn = store.conn()?;
            require_membership(&conn, tenant, owner)?;
            conn.query_row("SELECT secret_handle FROM benchmark_run WHERE id=?1 AND tenant_id=?2 AND owner_principal_id=?3", params![id.to_string(),tenant.0,owner.0], |row| row.get(0))
                .optional()?.ok_or(MetadataError::BenchmarkRunNotFound)
        }).await
    }

    pub async fn get_benchmark_run(
        &self,
        tenant: TenantId,
        owner: PrincipalId,
        id: Uuid,
    ) -> Result<SavedBenchmarkRun> {
        let handle = self.benchmark_handle(tenant, owner, id).await?;
        let bytes = self.secrets.get(BENCHMARK_SECRET_NAMESPACE, &handle).await?
            .ok_or_else(|| MetadataError::InvalidBenchmarkRun("snapshot payload unavailable; restore the matching secret backup or delete the entry".into()))?;
        if bytes.len() > MAX_BYTES {
            return Err(MetadataError::InvalidBenchmarkRun(
                "snapshot exceeds size limit".into(),
            ));
        }
        let saved: SavedBenchmarkRun = serde_json::from_slice(&bytes)?;
        if saved.id != id {
            return Err(MetadataError::InvalidBenchmarkRun(
                "snapshot identity mismatch".into(),
            ));
        }
        Ok(saved)
    }

    pub async fn list_benchmark_runs(
        &self,
        tenant: TenantId,
        owner: PrincipalId,
        cursor: Option<Uuid>,
        limit: u32,
    ) -> Result<Vec<SavedBenchmarkRunSummary>> {
        let store = self.clone();
        let ids = sqlite_blocking(move || {
            let conn = store.conn()?;
            require_membership(&conn, tenant, owner)?;
            let cursor_time: Option<String> = cursor.map(|id| conn.query_row("SELECT saved_at FROM benchmark_run WHERE id=?1 AND tenant_id=?2 AND owner_principal_id=?3", params![id.to_string(),tenant.0,owner.0], |row| row.get(0)).optional()?.ok_or(MetadataError::BenchmarkRunNotFound)).transpose()?;
            let mut stmt = conn.prepare("SELECT id,saved_at FROM benchmark_run WHERE tenant_id=?1 AND owner_principal_id=?2 AND (?3 IS NULL OR saved_at < ?3 OR (saved_at = ?3 AND id < ?4)) ORDER BY saved_at DESC,id DESC LIMIT ?5")?;
            let ids = stmt.query_map(params![tenant.0,owner.0,cursor_time,cursor.map(|id| id.to_string()),limit.clamp(1,51)], |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
            Ok(ids)
        }).await?;
        let mut summaries = Vec::with_capacity(ids.len());
        for (id, saved_at) in ids {
            let id = Uuid::parse_str(&id).map_err(|_| MetadataError::BenchmarkRunNotFound)?;
            let saved = match self.get_benchmark_run(tenant, owner, id).await {
                Ok(saved) => saved,
                Err(MetadataError::InvalidBenchmarkRun(_)) => {
                    summaries.push(SavedBenchmarkRunSummary {
                        id,
                        saved_at: super::parse_time_sql(saved_at)?,
                        name: "Snapshot unavailable — restore secrets or delete entry".into(),
                        engine: None,
                        payload_available: false,
                        completed: false,
                        median_ns: None,
                    });
                    continue;
                }
                Err(error) => return Err(error),
            };
            summaries.push(SavedBenchmarkRunSummary {
                id,
                saved_at: saved.saved_at,
                name: saved.name,
                engine: Some(saved.report.engine),
                payload_available: true,
                completed: saved.report.completed,
                median_ns: saved.report.median_ns,
            });
        }
        Ok(summaries)
    }

    pub async fn delete_benchmark_run(
        &self,
        tenant: TenantId,
        owner: PrincipalId,
        id: Uuid,
    ) -> Result<()> {
        let handle = self.benchmark_handle(tenant, owner, id).await?;
        // Keep the index on secret-backend failure so deletion can be retried.
        self.secrets
            .delete(BENCHMARK_SECRET_NAMESPACE, &handle)
            .await?;
        let store = self.clone();
        sqlite_blocking(move || {
            let conn = store.conn()?;
            conn.execute(
                "DELETE FROM benchmark_run WHERE id=?1 AND tenant_id=?2 AND owner_principal_id=?3",
                params![id.to_string(), tenant.0, owner.0],
            )?;
            Ok(())
        })
        .await
    }
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
        Err(MetadataError::BenchmarkRunNotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FileSecretStore, MembershipRole, SecretStore};
    use std::sync::Arc;

    fn fixture(id: u128) -> SavedBenchmarkRun {
        SavedBenchmarkRun {
            id: Uuid::from_u128(id),
            saved_at: chrono::Utc::now(),
            name: "private fixture name".into(),
            report: sift_protocol::BenchmarkReport {
                version: 1,
                run_id: Uuid::new_v4(),
                engine: sift_protocol::Engine::Postgres,
                sql: "SELECT 'private-fixture-literal'".into(),
                captured_at: chrono::Utc::now(),
                warmups: 0,
                requested_iterations: 1,
                query_timeout_ms: 100,
                total_budget_ms: 1000,
                delay_ms: 0,
                parameter_count: 0,
                samples: vec![],
                completed: false,
                warnings: vec![],
                median_ns: None,
                mean_ns: None,
                min_ns: None,
                max_ns: None,
                standard_deviation_ns: None,
                p95_ns: None,
                p99_ns: None,
            },
        }
    }

    #[tokio::test]
    async fn benchmark_runs_survive_reopen_and_enforce_private_pagination_and_deletion() {
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
        let first = fixture(1);
        let mut second = fixture(2);
        second.saved_at = first.saved_at;
        store
            .save_benchmark_run(TenantId(1), PrincipalId(1), first.clone())
            .await
            .unwrap();
        store
            .save_benchmark_run(TenantId(1), PrincipalId(1), second.clone())
            .await
            .unwrap();
        assert!(store
            .save_benchmark_run(TenantId(1), PrincipalId(1), first.clone())
            .await
            .is_err());
        assert!(matches!(
            store
                .get_benchmark_run(TenantId(1), peer.id, first.id)
                .await,
            Err(MetadataError::BenchmarkRunNotFound)
        ));
        assert!(store
            .list_benchmark_runs(TenantId(1), peer.id, None, 50)
            .await
            .unwrap()
            .is_empty());
        assert!(store
            .list_benchmark_runs(TenantId(1), peer.id, Some(first.id), 50)
            .await
            .is_err());
        assert!(store
            .delete_benchmark_run(TenantId(1), peer.id, first.id)
            .await
            .is_err());
        assert!(store
            .get_benchmark_run(TenantId(2), PrincipalId(1), first.id)
            .await
            .is_err());
        let page = store
            .list_benchmark_runs(TenantId(1), PrincipalId(1), None, 1)
            .await
            .unwrap();
        assert_eq!(page[0].id, second.id);
        let next = store
            .list_benchmark_runs(TenantId(1), PrincipalId(1), Some(second.id), 1)
            .await
            .unwrap();
        assert_eq!(next[0].id, first.id);
        let handle = store
            .benchmark_handle(TenantId(1), PrincipalId(1), first.id)
            .await
            .unwrap();
        drop(store);
        drop(secrets);
        for path in [&db, &encrypted] {
            let bytes = std::fs::read(path).unwrap();
            for secret in ["private-fixture-literal", "private fixture name"] {
                assert!(!bytes
                    .windows(secret.len())
                    .any(|window| window == secret.as_bytes()));
            }
        }
        let secrets = Arc::new(FileSecretStore::open(&encrypted, &key).unwrap());
        let store = MetadataStore::open(&db, secrets.clone()).unwrap();
        assert_eq!(
            store
                .get_benchmark_run(TenantId(1), PrincipalId(1), first.id)
                .await
                .unwrap()
                .report
                .sql,
            first.report.sql
        );
        // Missing secret backups remain browsable/deletable, not a poisoned library.
        secrets
            .delete(BENCHMARK_SECRET_NAMESPACE, &handle)
            .await
            .unwrap();
        let page = store
            .list_benchmark_runs(TenantId(1), PrincipalId(1), None, 50)
            .await
            .unwrap();
        assert!(
            !page
                .iter()
                .find(|item| item.id == first.id)
                .unwrap()
                .payload_available
        );
        store
            .delete_benchmark_run(TenantId(1), PrincipalId(1), first.id)
            .await
            .unwrap();
        store
            .delete_benchmark_run(TenantId(1), PrincipalId(1), second.id)
            .await
            .unwrap();
        assert!(store
            .list_benchmark_runs(TenantId(1), PrincipalId(1), None, 50)
            .await
            .unwrap()
            .is_empty());
        assert!(store
            .get_benchmark_run(TenantId(1), PrincipalId(1), second.id)
            .await
            .is_err());
    }
}
