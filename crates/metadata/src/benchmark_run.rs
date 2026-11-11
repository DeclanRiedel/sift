//! Private immutable snapshots. Sensitive payloads are only in SecretStore.
use rusqlite::{params, OptionalExtension};
use sift_protocol::{
    BenchmarkRunAnnotations, SavedBenchmarkRun, SavedBenchmarkRunSummary,
    UpdateBenchmarkRunAnnotationsRequest,
};
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
        if let Some(profile) = &saved.profile {
            profile
                .validate_for(&saved.report)
                .map_err(|message| MetadataError::InvalidBenchmarkRun(message.into()))?;
        }
        if let Some(context) = &saved.source_context {
            context
                .validate()
                .map_err(|message| MetadataError::InvalidBenchmarkRun(message.into()))?;
        }
        if saved.annotations != BenchmarkRunAnnotations::default() {
            return Err(MetadataError::InvalidBenchmarkRun(
                "new snapshots must start without annotations".into(),
            ));
        }
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
            let duplicate: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM benchmark_run WHERE tenant_id=?1 AND owner_principal_id=?2 AND run_id=?3)", params![tenant.0, owner.0, record.report.run_id.to_string()], |row| row.get(0))?;
            if duplicate {
                return Err(MetadataError::InvalidBenchmarkRun("this run is already saved; saved snapshots cannot be overwritten".into()));
            }
            tx.execute("INSERT INTO benchmark_run(id,tenant_id,owner_principal_id,run_id,saved_at,secret_handle,payload_bytes) VALUES(?1,?2,?3,?4,?5,?6,?7)", params![record.id.to_string(),tenant.0,owner.0,record.report.run_id.to_string(),record.saved_at.to_rfc3339(),stored_handle,bytes.len() as i64])?;
            super::benchmark_retention::enforce_on_save(&tx,tenant,owner)?;
            let (count,used):(i64,i64)=tx.query_row("SELECT COUNT(*),COALESCE(SUM(payload_bytes),0) FROM benchmark_run WHERE tenant_id=?1 AND owner_principal_id=?2",params![tenant.0,owner.0],|row|Ok((row.get(0)?,row.get(1)?)))?;
            if count>500 || used>OWNER_BYTES {return Err(MetadataError::InvalidBenchmarkRun("saved-run limit reached (500 runs / 64 MiB per owner); delete unwanted runs or review retention".into()));}
            let retained:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM benchmark_run WHERE id=?1)",[record.id.to_string()],|row|row.get(0))?;
            if !retained {return Err(MetadataError::InvalidBenchmarkRun("retention would remove this snapshot; review its save timestamp and policy".into()));}
            tx.commit()?;
            Ok(())
        }).await;
        if let Err(error) = inserted {
            self.discard_benchmark_payload(&handle, "uncommitted benchmark snapshot")
                .await;
            return Err(error);
        }
        Ok(saved)
    }

    async fn discard_benchmark_payload(&self, handle: &str, reason: &'static str) {
        let cleanup = self.clone();
        let abandoned = handle.to_owned();
        if sqlite_blocking(move||{
            cleanup.conn()?.execute("INSERT OR IGNORE INTO vault_secret_cleanup_queue(namespace,secret_handle,reason,not_before) VALUES(?1,?2,?3,?4)",params![BENCHMARK_SECRET_NAMESPACE,abandoned,reason,chrono::Utc::now().to_rfc3339()])?;Ok(())
        }).await.is_err() {tracing::warn!("could not queue uncommitted benchmark cleanup");}
        if self
            .secrets
            .delete(BENCHMARK_SECRET_NAMESPACE, handle)
            .await
            .is_err()
        {
            tracing::warn!("uncommitted benchmark payload cleanup deferred");
        }
    }

    fn benchmark_membership(&self, tenant: TenantId, owner: PrincipalId) -> Result<()> {
        let conn = self.conn()?;
        require_membership(&conn, tenant, owner)
    }

    async fn benchmark_row(
        &self,
        tenant: TenantId,
        owner: PrincipalId,
        id: Uuid,
    ) -> Result<(String, u64)> {
        let store = self.clone();
        sqlite_blocking(move || {
            let conn = store.conn()?;
            require_membership(&conn, tenant, owner)?;
            conn.query_row("SELECT secret_handle,annotation_revision FROM benchmark_run WHERE id=?1 AND tenant_id=?2 AND owner_principal_id=?3", params![id.to_string(),tenant.0,owner.0], |row| Ok((row.get(0)?, row.get(1)?)))
                .optional()?.ok_or(MetadataError::BenchmarkRunNotFound)
        }).await
    }

    pub async fn get_benchmark_run(
        &self,
        tenant: TenantId,
        owner: PrincipalId,
        id: Uuid,
    ) -> Result<SavedBenchmarkRun> {
        self.benchmark_snapshot(tenant, owner, id)
            .await
            .map(|(saved, _, _)| saved)
    }

    async fn benchmark_snapshot(
        &self,
        tenant: TenantId,
        owner: PrincipalId,
        id: Uuid,
    ) -> Result<(SavedBenchmarkRun, String, Vec<u8>)> {
        let (handle, revision) = self.benchmark_row(tenant, owner, id).await?;
        let bytes = match self
            .secrets
            .get(BENCHMARK_SECRET_NAMESPACE, &handle)
            .await?
        {
            Some(bytes) => bytes,
            None => {
                if self.benchmark_row(tenant, owner, id).await? != (handle.clone(), revision) {
                    return Err(MetadataError::BenchmarkRunRevisionConflict);
                }
                return Err(MetadataError::InvalidBenchmarkRun("snapshot payload unavailable; restore the matching secret backup or delete the entry".into()));
            }
        };
        if bytes.len() > MAX_BYTES {
            return Err(MetadataError::InvalidBenchmarkRun(
                "snapshot exceeds size limit".into(),
            ));
        }
        let saved: SavedBenchmarkRun = serde_json::from_slice(&bytes)?;
        if let Some(context) = &saved.source_context {
            context
                .validate()
                .map_err(|message| MetadataError::InvalidBenchmarkRun(message.into()))?;
        }
        if saved.id != id || saved.annotations.revision != revision {
            return Err(MetadataError::InvalidBenchmarkRun(
                "snapshot identity or annotation revision mismatch".into(),
            ));
        }
        Ok((saved, handle, bytes))
    }

    pub async fn update_benchmark_run_annotations(
        &self,
        tenant: TenantId,
        owner: PrincipalId,
        id: Uuid,
        mut request: UpdateBenchmarkRunAnnotationsRequest,
    ) -> Result<SavedBenchmarkRun> {
        request
            .normalize()
            .map_err(|message| MetadataError::InvalidBenchmarkRun(message.into()))?;
        let (mut saved, old_handle, original_bytes) =
            self.benchmark_snapshot(tenant, owner, id).await?;
        if saved.annotations.revision != request.expected_revision {
            return Err(MetadataError::BenchmarkRunRevisionConflict);
        }
        let revision = request
            .expected_revision
            .checked_add(1)
            .filter(|next| *next <= i64::MAX as u64)
            .ok_or(MetadataError::BenchmarkRunRevisionConflict)?;
        saved.annotations = BenchmarkRunAnnotations {
            revision,
            notes: request.notes,
            tags: request.tags,
        };
        // Replace only the annotation object. Preserve unknown measurement
        // fields from newer payloads instead of round-tripping them through
        // this version's typed report and silently dropping observations.
        let mut document: serde_json::Value = serde_json::from_slice(&original_bytes)?;
        document
            .as_object_mut()
            .ok_or_else(|| {
                MetadataError::InvalidBenchmarkRun("snapshot payload must be an object".into())
            })?
            .insert(
                "annotations".into(),
                serde_json::to_value(&saved.annotations)?,
            );
        let bytes = serde_json::to_vec(&document)?;
        if bytes.len() > MAX_BYTES {
            return Err(MetadataError::InvalidBenchmarkRun(
                "snapshot with annotations exceeds 4 MiB".into(),
            ));
        }
        let handle = Uuid::new_v4().to_string();
        self.secrets
            .put(BENCHMARK_SECRET_NAMESPACE, &handle, &bytes)
            .await?;
        let store = self.clone();
        let new_handle = handle.clone();
        let prior_handle = old_handle.clone();
        let updated = sqlite_blocking(move || {
            let mut conn = store.conn()?; let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            require_membership(&tx, tenant, owner)?;
            let used: i64 = tx.query_row("SELECT COALESCE(SUM(payload_bytes),0) FROM benchmark_run WHERE tenant_id=?1 AND owner_principal_id=?2 AND id<>?3", params![tenant.0,owner.0,id.to_string()],|row|row.get(0))?;
            if used + bytes.len() as i64 > OWNER_BYTES { return Err(MetadataError::InvalidBenchmarkRun("snapshot payload limit reached".into())); }
            let changed = tx.execute("UPDATE benchmark_run SET secret_handle=?1,payload_bytes=?2,annotation_revision=?3 WHERE id=?4 AND tenant_id=?5 AND owner_principal_id=?6 AND secret_handle=?7 AND annotation_revision=?8",params![new_handle,bytes.len() as i64,revision,id.to_string(),tenant.0,owner.0,prior_handle,request.expected_revision])?;
            if changed != 1 { return Err(MetadataError::BenchmarkRunRevisionConflict); }
            tx.commit()?; Ok(())
        }).await;
        if let Err(error) = updated {
            self.discard_benchmark_payload(&handle, "uncommitted benchmark annotation")
                .await;
            return Err(error);
        }
        if self
            .secrets
            .delete(BENCHMARK_SECRET_NAMESPACE, &old_handle)
            .await
            .is_err()
        {
            tracing::warn!("old benchmark snapshot payload cleanup failed");
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
                        tags: Vec::new(),
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
                tags: saved.annotations.tags,
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
        let (handle, _) = self.benchmark_row(tenant, owner, id).await?;
        // Keep the index on secret-backend failure so deletion can be retried.
        self.secrets
            .delete(BENCHMARK_SECRET_NAMESPACE, &handle)
            .await?;
        let store = self.clone();
        sqlite_blocking(move || {
            let conn = store.conn()?;
            let changed = conn.execute(
                "DELETE FROM benchmark_run WHERE id=?1 AND tenant_id=?2 AND owner_principal_id=?3 AND secret_handle=?4",
                params![id.to_string(), tenant.0, owner.0, handle],
            )?;
            if changed != 1 { return Err(MetadataError::BenchmarkRunRevisionConflict); }
            Ok(())
        })
        .await
    }
}

pub(super) fn require_membership(
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
            profile: None,
            source_context: Some(sift_protocol::BenchmarkSourceContext {
                captured_at: chrono::Utc::now(),
                instance_id: "private-context-instance".into(),
                workspace_id: sift_protocol::WorkspaceId(17),
                workspace_revision: sift_protocol::WorkspaceRevision(2),
                path: sift_protocol::WorkspacePath("queries/private-context-sentinel.sql".into()),
                git: Some(sift_protocol::BenchmarkSourceGit {
                    observed_at: chrono::Utc::now(),
                    workspace_revision: sift_protocol::WorkspaceRevision(1),
                    head_oid: Some("a".repeat(40)),
                    branch: Some("private-context-branch-sentinel".into()),
                    dirty: None,
                    truncated: true,
                }),
            }),
            id: Uuid::from_u128(id),
            saved_at: chrono::Utc::now(),
            name: "private fixture name".into(),
            annotations: Default::default(),
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
                pair_context: None,
                suite_context: None,
                environment: None,
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
        let mut first = fixture(1);
        first.profile = Some(serde_json::from_value(serde_json::json!({
            "captured_at":chrono::Utc::now(),"sql":first.report.sql,"parameter_count":0,
            "response":{"run_id":Uuid::new_v4(),"plan":{"engine":"postgres","analyzed":true,"raw":"private-profile-plan-sentinel","root":{"op":"Result","actual_rows":1.0}},"server_elapsed_ns":100,"execution_ms":0.01}
        })).unwrap());
        let mut legacy_profile = serde_json::to_value(&first).unwrap();
        legacy_profile.as_object_mut().unwrap().remove("profile");
        assert!(serde_json::from_value::<SavedBenchmarkRun>(legacy_profile)
            .unwrap()
            .profile
            .is_none());
        let mut legacy_context = serde_json::to_value(&first).unwrap();
        legacy_context
            .as_object_mut()
            .unwrap()
            .remove("source_context");
        assert!(serde_json::from_value::<SavedBenchmarkRun>(legacy_context)
            .unwrap()
            .source_context
            .is_none());
        let mut invalid = fixture(99);
        invalid.source_context.as_mut().unwrap().path.0 = "/absolute/private.sql".into();
        assert!(matches!(
            store
                .save_benchmark_run(TenantId(1), PrincipalId(1), invalid)
                .await,
            Err(MetadataError::InvalidBenchmarkRun(_))
        ));
        let mut second = fixture(2);
        second.report.sql = "x".repeat(MAX_BYTES - 4096);
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
        let oversized = UpdateBenchmarkRunAnnotationsRequest {
            expected_revision: 0,
            notes: "n".repeat(8192),
            tags: vec![],
        };
        assert!(matches!(
            store
                .update_benchmark_run_annotations(TenantId(1), PrincipalId(1), second.id, oversized)
                .await,
            Err(MetadataError::InvalidBenchmarkRun(_))
        ));
        assert_eq!(
            store
                .get_benchmark_run(TenantId(1), PrincipalId(1), second.id)
                .await
                .unwrap()
                .annotations
                .revision,
            0
        );
        let update = UpdateBenchmarkRunAnnotationsRequest {
            expected_revision: 0,
            notes: "private annotation sentinel\nsecond line".into(),
            tags: vec!["private-tag-sentinel".into(), " z ".into(), "z".into()],
        };
        assert!(matches!(
            store
                .update_benchmark_run_annotations(TenantId(1), peer.id, first.id, update.clone())
                .await,
            Err(MetadataError::BenchmarkRunNotFound)
        ));
        assert!(store
            .update_benchmark_run_annotations(TenantId(2), PrincipalId(1), first.id, update.clone())
            .await
            .is_err());
        let (legacy_handle, _) = store
            .benchmark_row(TenantId(1), PrincipalId(1), first.id)
            .await
            .unwrap();
        let mut legacy = serde_json::to_value(&first).unwrap();
        legacy.as_object_mut().unwrap().remove("annotations");
        legacy["report"]["future_measurement"] =
            serde_json::json!({"preserve":"unrecognized observation"});
        secrets
            .put(
                BENCHMARK_SECRET_NAMESPACE,
                &legacy_handle,
                &serde_json::to_vec(&legacy).unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .get_benchmark_run(TenantId(1), PrincipalId(1), first.id)
                .await
                .unwrap()
                .annotations,
            BenchmarkRunAnnotations::default()
        );
        let edited = store
            .update_benchmark_run_annotations(TenantId(1), PrincipalId(1), first.id, update.clone())
            .await
            .unwrap();
        assert_eq!(edited.annotations.revision, 1);
        assert_eq!(edited.annotations.tags, vec!["private-tag-sentinel", "z"]);
        assert_eq!(
            serde_json::to_value(&edited.report).unwrap(),
            serde_json::to_value(&first.report).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&edited.profile).unwrap(),
            serde_json::to_value(&first.profile).unwrap()
        );
        assert_eq!(edited.source_context, first.source_context);
        assert_eq!(edited.saved_at, first.saved_at);
        assert_eq!(edited.name, first.name);
        assert!(matches!(
            store
                .update_benchmark_run_annotations(
                    TenantId(1),
                    PrincipalId(1),
                    first.id,
                    update.clone()
                )
                .await,
            Err(MetadataError::BenchmarkRunRevisionConflict)
        ));
        let (current_handle, _) = store
            .benchmark_row(TenantId(1), PrincipalId(1), first.id)
            .await
            .unwrap();
        let stored_json: serde_json::Value = serde_json::from_slice(
            &secrets
                .get(BENCHMARK_SECRET_NAMESPACE, &current_handle)
                .await
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            stored_json["report"], legacy["report"],
            "all measurement fields, including unknown observations, stay immutable"
        );
        assert!(secrets
            .get(BENCHMARK_SECRET_NAMESPACE, &legacy_handle)
            .await
            .unwrap()
            .is_none());
        let concurrent = UpdateBenchmarkRunAnnotationsRequest {
            expected_revision: 1,
            ..update.clone()
        };
        let (left, right) = tokio::join!(
            store.update_benchmark_run_annotations(
                TenantId(1),
                PrincipalId(1),
                first.id,
                concurrent.clone()
            ),
            store.update_benchmark_run_annotations(
                TenantId(1),
                PrincipalId(1),
                first.id,
                concurrent
            )
        );
        assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
        assert!(
            matches!(left, Err(MetadataError::BenchmarkRunRevisionConflict))
                || matches!(right, Err(MetadataError::BenchmarkRunRevisionConflict)),
            "left error: {:?}; right error: {:?}",
            left.as_ref().err(),
            right.as_ref().err()
        );
        assert_eq!(
            store
                .get_benchmark_run(TenantId(1), PrincipalId(1), first.id)
                .await
                .unwrap()
                .annotations
                .revision,
            2
        );
        let (handle, _) = store
            .benchmark_row(TenantId(1), PrincipalId(1), first.id)
            .await
            .unwrap();
        drop(store);
        drop(secrets);
        for path in [&db, &encrypted] {
            let bytes = std::fs::read(path).unwrap();
            for secret in [
                "private-profile-plan-sentinel",
                "private-fixture-literal",
                "private fixture name",
                "private-context-instance",
                "private-context-sentinel",
                "private-context-branch-sentinel",
                "private annotation sentinel",
                "private-tag-sentinel",
            ] {
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
        let restored = store
            .get_benchmark_run(TenantId(1), PrincipalId(1), first.id)
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(&restored.profile).unwrap(),
            serde_json::to_value(&first.profile).unwrap()
        );
        assert_eq!(restored.source_context, first.source_context);
        assert_eq!(restored.annotations.revision, 2);
        assert_eq!(restored.annotations.notes, update.notes);
        // Revocation is checked again for an owner with an existing payload.
        let peer_owned = fixture(3);
        store
            .save_benchmark_run(TenantId(1), peer.id, peer_owned.clone())
            .await
            .unwrap();
        store
            .conn()
            .unwrap()
            .execute(
                "DELETE FROM membership WHERE tenant_id=1 AND principal_id=?1",
                params![peer.id.0],
            )
            .unwrap();
        assert!(matches!(
            store
                .update_benchmark_run_annotations(
                    TenantId(1),
                    peer.id,
                    peer_owned.id,
                    update.clone()
                )
                .await,
            Err(MetadataError::BenchmarkRunNotFound)
        ));
        store
            .upsert_tenant_membership(TenantId(1), peer.id, MembershipRole::Member)
            .unwrap();
        store
            .delete_benchmark_run(TenantId(1), peer.id, peer_owned.id)
            .await
            .unwrap();
        // Missing secret backups remain browsable/deletable, not a poisoned library.
        secrets
            .delete(BENCHMARK_SECRET_NAMESPACE, &handle)
            .await
            .unwrap();
        assert!(matches!(
            store
                .update_benchmark_run_annotations(
                    TenantId(1),
                    PrincipalId(1),
                    first.id,
                    UpdateBenchmarkRunAnnotationsRequest {
                        expected_revision: 2,
                        ..update
                    }
                )
                .await,
            Err(MetadataError::InvalidBenchmarkRun(_))
        ));
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
    #[tokio::test]
    async fn concurrent_annotation_update_cannot_be_erased_by_a_stale_delete() {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct PauseDelete {
            inner: crate::MemorySecretStore,
            armed: AtomicBool,
            entered: tokio::sync::Notify,
            release: tokio::sync::Notify,
        }
        #[async_trait::async_trait]
        impl SecretStore for PauseDelete {
            async fn put(&self, n: &str, h: &str, b: &[u8]) -> Result<()> {
                self.inner.put(n, h, b).await
            }
            async fn get(&self, n: &str, h: &str) -> Result<Option<Vec<u8>>> {
                self.inner.get(n, h).await
            }
            async fn delete(&self, n: &str, h: &str) -> Result<()> {
                if self.armed.swap(false, Ordering::SeqCst) {
                    self.entered.notify_one();
                    self.release.notified().await;
                }
                self.inner.delete(n, h).await
            }
        }
        let secrets = Arc::new(PauseDelete {
            inner: crate::MemorySecretStore::new(),
            armed: AtomicBool::new(false),
            entered: Default::default(),
            release: Default::default(),
        });
        let store = MetadataStore::open_in_memory(secrets.clone()).unwrap();
        store.bootstrap_local("owner").unwrap();
        let saved = fixture(1);
        store
            .save_benchmark_run(TenantId(1), PrincipalId(1), saved.clone())
            .await
            .unwrap();
        secrets.armed.store(true, Ordering::SeqCst);
        let task_store = store.clone();
        let id = saved.id;
        let deletion = tokio::spawn(async move {
            task_store
                .delete_benchmark_run(TenantId(1), PrincipalId(1), id)
                .await
        });
        secrets.entered.notified().await;
        let updated = store
            .update_benchmark_run_annotations(
                TenantId(1),
                PrincipalId(1),
                id,
                UpdateBenchmarkRunAnnotationsRequest {
                    expected_revision: 0,
                    notes: "retained concurrent edit".into(),
                    tags: vec![],
                },
            )
            .await
            .unwrap();
        secrets.release.notify_one();
        assert!(matches!(
            deletion.await.unwrap(),
            Err(MetadataError::BenchmarkRunRevisionConflict)
        ));
        assert_eq!(
            store
                .get_benchmark_run(TenantId(1), PrincipalId(1), id)
                .await
                .unwrap()
                .annotations,
            updated.annotations
        );
        store
            .delete_benchmark_run(TenantId(1), PrincipalId(1), id)
            .await
            .unwrap();
        assert!(
            secrets.inner.is_empty(),
            "both payload generations must be cleaned"
        );
    }
}
