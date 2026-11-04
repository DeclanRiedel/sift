//! Owner-private retention. Index removal, cleanup enqueueing and audit are atomic.
use super::{sqlite_blocking, MetadataError, MetadataStore, PrincipalId, Result, TenantId};
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use sift_protocol::{
    BenchmarkRetentionPolicy, BenchmarkRetentionPreview, BenchmarkRetentionState,
    BenchmarkRetentionUpdate, PreviewBenchmarkRetentionRequest, UpdateBenchmarkRetentionRequest,
};
use uuid::Uuid;

const OWNER_SCAN_SQL:&str="SELECT tenant_id,owner_principal_id FROM benchmark_retention INDEXED BY benchmark_retention_maintenance WHERE max_age_days IS NOT NULL OR max_runs IS NOT NULL ORDER BY last_checked_at,tenant_id,owner_principal_id LIMIT ?1";

#[derive(Debug, Default)]
pub struct BenchmarkRetentionMaintenance {
    pub visited: u32,
    pub removed: u32,
}
#[derive(serde::Serialize)]
struct Row {
    id: Uuid,
    saved_at: DateTime<Utc>,
    handle: String,
    bytes: u64,
    revision: u64,
}
fn invalid(message: &str) -> MetadataError {
    MetadataError::InvalidBenchmarkRun(message.into())
}
fn time(value: String) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&value)
        .map(|at| at.with_timezone(&Utc))
        .map_err(|_| invalid("invalid retention timestamp"))
}
fn policy(
    conn: &Connection,
    tenant: TenantId,
    owner: PrincipalId,
) -> Result<BenchmarkRetentionState> {
    let row=conn.query_row("SELECT max_age_days,max_runs,revision,updated_at FROM benchmark_retention WHERE tenant_id=?1 AND owner_principal_id=?2",params![tenant.0,owner.0],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get::<_,String>(3)?))).optional()?;
    match row {
        Some((max_age_days, max_runs, revision, updated_at)) => Ok(BenchmarkRetentionState {
            policy: BenchmarkRetentionPolicy {
                max_age_days,
                max_runs,
            },
            revision,
            updated_at: Some(time(updated_at)?),
        }),
        None => Ok(BenchmarkRetentionState {
            policy: Default::default(),
            revision: 0,
            updated_at: None,
        }),
    }
}
fn rows(
    conn: &Connection,
    tenant: TenantId,
    owner: PrincipalId,
    maximum: usize,
) -> Result<Vec<Row>> {
    let mut statement=conn.prepare("SELECT id,saved_at,secret_handle,payload_bytes,annotation_revision FROM benchmark_run WHERE tenant_id=?1 AND owner_principal_id=?2 ORDER BY id LIMIT ?3")?;
    let raw = statement
        .query_map(params![tenant.0, owner.0, maximum + 1], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if raw.len() > maximum {
        return Err(invalid("retention requires at most 500 owned snapshots"));
    }
    let mut rows = raw
        .into_iter()
        .map(|(id, saved_at, handle, bytes, revision)| {
            Ok(Row {
                id: Uuid::parse_str(&id).map_err(|_| invalid("invalid saved snapshot identity"))?,
                saved_at: time(saved_at)?,
                handle,
                bytes,
                revision,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    rows.sort_by(|a, b| b.saved_at.cmp(&a.saved_at).then_with(|| b.id.cmp(&a.id)));
    Ok(rows)
}
fn selected<'a>(
    rows: &'a [Row],
    policy: &BenchmarkRetentionPolicy,
    as_of: DateTime<Utc>,
) -> Result<Vec<&'a Row>> {
    let cutoff = policy
        .max_age_days
        .map(|days| {
            as_of
                .checked_sub_signed(Duration::days(i64::from(days)))
                .ok_or_else(|| invalid("retention cutoff is out of range"))
        })
        .transpose()?;
    Ok(rows
        .iter()
        .enumerate()
        .filter(|(index, row)| {
            policy.max_runs.is_some_and(|max| *index >= max as usize)
                || cutoff.is_some_and(|cutoff| row.saved_at <= cutoff)
        })
        .map(|(_, row)| row)
        .collect())
}
fn digest(
    tenant: TenantId,
    owner: PrincipalId,
    revision: u64,
    policy: &BenchmarkRetentionPolicy,
    as_of: DateTime<Utc>,
    rows: &[Row],
) -> Result<String> {
    let bytes = serde_json::to_vec(&(tenant.0, owner.0, revision, policy, as_of, rows))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
fn remove(
    conn: &Connection,
    tenant: TenantId,
    owner: PrincipalId,
    revision: u64,
    rows: &[&Row],
    actor: Option<PrincipalId>,
) -> Result<(u32, u64)> {
    let mut bytes = 0;
    for row in rows {
        let changed=conn.execute("DELETE FROM benchmark_run WHERE id=?1 AND tenant_id=?2 AND owner_principal_id=?3 AND secret_handle=?4",params![row.id.to_string(),tenant.0,owner.0,row.handle])?;
        if changed != 1 {
            return Err(MetadataError::BenchmarkRetentionConflict);
        }
        bytes += row.bytes;
    }
    if !rows.is_empty() {
        let summary = sift_protocol::Operation::PruneBenchmarkRuns {
            tenant_id: tenant.0,
            owner_principal_id: owner.0,
            policy_revision: revision,
        }
        .audit_summary();
        super::insert_operation_audit_row(
            conn,
            &super::NewOperationAudit {
                actor_principal_id: actor,
                action: summary.action,
                target: summary.target,
                target_id: summary.target_id,
                status: "succeeded".into(),
                result_code: None,
                row_count: Some(rows.len() as i64),
                error_message: None,
                correlation_id: Some(format!("benchmark-retention/{}/{revision}", owner.0)),
            },
        )?;
    }
    Ok((rows.len() as u32, bytes))
}
/// Caller owns the save transaction and has already checked current membership.
pub(super) fn enforce_on_save(
    conn: &Connection,
    tenant: TenantId,
    owner: PrincipalId,
) -> Result<()> {
    let state = policy(conn, tenant, owner)?;
    if state.policy == BenchmarkRetentionPolicy::default() {
        return Ok(());
    }
    // A save may temporarily add the 501st record before transactional pruning.
    let rows = rows(conn, tenant, owner, 501)?;
    let selected = selected(&rows, &state.policy, Utc::now())?;
    remove(conn, tenant, owner, state.revision, &selected, Some(owner))?;
    Ok(())
}
impl MetadataStore {
    pub async fn benchmark_retention(
        &self,
        tenant: TenantId,
        owner: PrincipalId,
    ) -> Result<BenchmarkRetentionState> {
        let store = self.clone();
        sqlite_blocking(move || {
            let conn = store.conn()?;
            super::benchmark_run::require_membership(&conn, tenant, owner)?;
            policy(&conn, tenant, owner)
        })
        .await
    }
    pub async fn preview_benchmark_retention(
        &self,
        tenant: TenantId,
        owner: PrincipalId,
        request: PreviewBenchmarkRetentionRequest,
    ) -> Result<BenchmarkRetentionPreview> {
        request.policy.validate().map_err(invalid)?;
        let store = self.clone();
        sqlite_blocking(move || {
            let mut conn = store.conn()?;
            let tx = conn.transaction()?;
            super::benchmark_run::require_membership(&tx, tenant, owner)?;
            let state = policy(&tx, tenant, owner)?;
            if state.revision != request.expected_revision {
                return Err(MetadataError::BenchmarkRetentionConflict);
            }
            let as_of = Utc::now();
            let rows = rows(&tx, tenant, owner, 500)?;
            let selected = selected(&rows, &request.policy, as_of)?;
            let preview = BenchmarkRetentionPreview {
                revision: state.revision,
                policy: request.policy.clone(),
                as_of,
                expires_at: as_of + Duration::minutes(5),
                remove_count: selected.len() as u32,
                remove_bytes: selected.iter().map(|row| row.bytes).sum(),
                review_token: digest(tenant, owner, state.revision, &request.policy, as_of, &rows)?,
            };
            tx.commit()?;
            Ok(preview)
        })
        .await
    }
    pub async fn update_benchmark_retention(
        &self,
        tenant: TenantId,
        owner: PrincipalId,
        request: UpdateBenchmarkRetentionRequest,
    ) -> Result<BenchmarkRetentionUpdate> {
        request.policy.validate().map_err(invalid)?;
        if !request.confirmed {
            return Err(invalid(
                "confirm ongoing automatic removal of saved snapshots, notes and source context",
            ));
        }
        let store = self.clone();
        sqlite_blocking(move||{
            let mut conn=store.conn()?;let tx=conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            super::benchmark_run::require_membership(&tx,tenant,owner)?;
            let now=Utc::now();
            if request.as_of>now || now-request.as_of>Duration::minutes(5) {return Err(MetadataError::BenchmarkRetentionConflict);}
            let state=policy(&tx,tenant,owner)?;
            if state.revision!=request.expected_revision {return Err(MetadataError::BenchmarkRetentionConflict);}
            let rows=rows(&tx,tenant,owner,500)?;
            if request.review_token!=digest(tenant,owner,state.revision,&request.policy,request.as_of,&rows)? {return Err(MetadataError::BenchmarkRetentionConflict);}
            let revision=state.revision.checked_add(1).filter(|revision|*revision<=i64::MAX as u64).ok_or(MetadataError::BenchmarkRetentionConflict)?;
            tx.execute("INSERT INTO benchmark_retention(tenant_id,owner_principal_id,max_age_days,max_runs,revision,updated_at,last_checked_at) VALUES(?1,?2,?3,?4,?5,?6,?6) ON CONFLICT(tenant_id,owner_principal_id) DO UPDATE SET max_age_days=excluded.max_age_days,max_runs=excluded.max_runs,revision=excluded.revision,updated_at=excluded.updated_at,last_checked_at=excluded.last_checked_at",params![tenant.0,owner.0,request.policy.max_age_days,request.policy.max_runs,revision,now.to_rfc3339()])?;
            let chosen=selected(&rows,&request.policy,request.as_of)?;
            let (removed,removed_bytes)=remove(&tx,tenant,owner,revision,&chosen,Some(owner))?;
            tx.commit()?;
            Ok(BenchmarkRetentionUpdate {retention:BenchmarkRetentionState {policy:request.policy,revision,updated_at:Some(now)},removed,removed_bytes})
        }).await
    }
    /// Trusted bounded maintenance; all row removals carry a typed persisted audit.
    pub async fn maintain_benchmark_retention(
        &self,
        limit: u32,
    ) -> Result<BenchmarkRetentionMaintenance> {
        self.maintain_benchmark_retention_at(limit, Utc::now())
            .await
    }
    async fn maintain_benchmark_retention_at(
        &self,
        limit: u32,
        now: DateTime<Utc>,
    ) -> Result<BenchmarkRetentionMaintenance> {
        let store = self.clone();
        let limit = limit.clamp(1, 100);
        sqlite_blocking(move||{
            let mut conn=store.conn()?;let tx=conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let owners={let mut statement=tx.prepare(OWNER_SCAN_SQL)?;
                let rows=statement.query_map([limit],|row|Ok((TenantId(row.get(0)?),PrincipalId(row.get(1)?))))?.collect::<rusqlite::Result<Vec<_>>>()?;rows};
            let mut report=BenchmarkRetentionMaintenance::default();
            for (tenant,owner) in owners {
                super::benchmark_run::require_membership(&tx,tenant,owner)?;
                let state=policy(&tx,tenant,owner)?;let rows=rows(&tx,tenant,owner,500)?;
                let selected=selected(&rows,&state.policy,now)?;
                report.removed+=remove(&tx,tenant,owner,state.revision,&selected,None)?.0;
                report.visited+=1;
                tx.execute("UPDATE benchmark_retention SET last_checked_at=?1 WHERE tenant_id=?2 AND owner_principal_id=?3",params![now.to_rfc3339(),tenant.0,owner.0])?;
            }
            tx.commit()?;Ok(report)
        }).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FileSecretStore, MembershipRole, MemorySecretStore, SecretStore};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    struct DeleteGate {
        inner: Arc<dyn SecretStore>,
        fail: AtomicBool,
        hold_put: AtomicBool,
        started: tokio::sync::Notify,
        resume: tokio::sync::Notify,
    }
    #[async_trait::async_trait]
    impl SecretStore for DeleteGate {
        async fn put(&self, namespace: &str, handle: &str, bytes: &[u8]) -> Result<()> {
            self.inner.put(namespace, handle, bytes).await?;
            if self.hold_put.swap(false, Ordering::SeqCst) {
                self.started.notify_one();
                self.resume.notified().await;
            }
            Ok(())
        }
        async fn get(&self, namespace: &str, handle: &str) -> Result<Option<Vec<u8>>> {
            self.inner.get(namespace, handle).await
        }
        async fn delete(&self, namespace: &str, handle: &str) -> Result<()> {
            if self.fail.load(Ordering::SeqCst) {
                return Err(MetadataError::SecretStore(
                    "private-backend-error-sentinel".into(),
                ));
            }
            self.inner.delete(namespace, handle).await
        }
    }
    fn saved(id: u128, at: DateTime<Utc>) -> sift_protocol::SavedBenchmarkRun {
        serde_json::from_value(serde_json::json!({"id":Uuid::from_u128(id),"saved_at":at,"name":"private-retention-name","source_context":{"captured_at":at,"instance_id":"private-retention-instance","workspace_id":1,"workspace_revision":1,"path":"queries/private-retention-source.sql"},
            "report":{"version":2,"run_id":Uuid::new_v4(),"engine":"postgres","sql":"SELECT 'private-retention-literal'","captured_at":at,"warmups":0,"requested_iterations":1,"query_timeout_ms":100,"total_budget_ms":1000,"delay_ms":0,"parameter_count":0,"samples":[],"completed":false,"warnings":[]}})).unwrap()
    }
    fn apply(preview: BenchmarkRetentionPreview) -> UpdateBenchmarkRetentionRequest {
        UpdateBenchmarkRetentionRequest {
            expected_revision: preview.revision,
            policy: preview.policy,
            as_of: preview.as_of,
            review_token: preview.review_token,
            confirmed: true,
        }
    }
    fn handle(store: &MetadataStore, id: Uuid) -> String {
        store
            .conn()
            .unwrap()
            .query_row(
                "SELECT secret_handle FROM benchmark_run WHERE id=?1",
                [id.to_string()],
                |row| row.get(0),
            )
            .unwrap()
    }
    async fn preview(
        store: &MetadataStore,
        owner: PrincipalId,
        revision: u64,
        policy: BenchmarkRetentionPolicy,
    ) -> BenchmarkRetentionPreview {
        store
            .preview_benchmark_retention(
                TenantId(1),
                owner,
                PreviewBenchmarkRetentionRequest {
                    expected_revision: revision,
                    policy,
                },
            )
            .await
            .unwrap()
    }
    #[tokio::test(flavor = "current_thread")]
    async fn retention_cleanup_keeps_async_executor_responsive_under_sqlite_contention() {
        let store = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        store.apply_migrations(false).unwrap();
        let blocked = store.clone();
        let (held, acquired) = tokio::sync::oneshot::channel();
        let (release, released) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let _guard = blocked.conn().unwrap();
            held.send(()).unwrap();
            released.recv_timeout(std::time::Duration::from_secs(2))
        });
        acquired.await.unwrap();
        let (started, running) = tokio::sync::oneshot::channel();
        let cleanup = tokio::spawn(async move {
            started.send(()).unwrap();
            store.process_vault_secret_cleanup(1).await
        });
        running.await.unwrap();
        assert!(
            release.send(()).is_ok(),
            "cleanup blocked the async executor while waiting for SQLite"
        );
        assert_eq!(cleanup.await.unwrap().unwrap(), (0, 0));
        assert!(thread.join().unwrap().is_ok());
    }

    #[test]
    fn retention_age_and_count_use_exact_boundaries_and_stable_order() {
        let at = DateTime::parse_from_rfc3339("2026-01-03T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let rows = (0..3)
            .map(|index| Row {
                id: Uuid::from_u128(3 - index),
                saved_at: at - Duration::days(index as i64),
                handle: "opaque".into(),
                bytes: 1,
                revision: 0,
            })
            .collect::<Vec<_>>();
        let chosen = selected(
            &rows,
            &BenchmarkRetentionPolicy {
                max_age_days: Some(2),
                max_runs: None,
            },
            at,
        )
        .unwrap();
        assert_eq!(
            chosen.iter().map(|row| row.id).collect::<Vec<_>>(),
            vec![Uuid::from_u128(1)]
        );
        assert_eq!(
            selected(
                &rows,
                &BenchmarkRetentionPolicy {
                    max_age_days: Some(2),
                    max_runs: Some(1)
                },
                at
            )
            .unwrap()
            .len(),
            2
        );
        assert!(selected(&rows, &Default::default(), at).unwrap().is_empty());
        assert!(BenchmarkRetentionPolicy {
            max_age_days: Some(0),
            max_runs: None
        }
        .validate()
        .is_err());
    }
    #[tokio::test]
    async fn retention_review_expiry_owner_revocation_and_durable_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("metadata");
        let key = dir.path().join("key");
        let encrypted = dir.path().join("secrets");
        std::fs::write(&key, "12".repeat(32)).unwrap();
        let gate = Arc::new(DeleteGate {
            inner: Arc::new(FileSecretStore::open(&encrypted, &key).unwrap()),
            fail: AtomicBool::new(false),
            hold_put: AtomicBool::new(false),
            started: tokio::sync::Notify::new(),
            resume: tokio::sync::Notify::new(),
        });
        let store = MetadataStore::open(&db, gate.clone()).unwrap();
        store.apply_migrations(false).unwrap();
        store.bootstrap_local("owner").unwrap();
        {
            let conn = store.conn().unwrap();
            let check = |sql: &str, bindings: &[&dyn rusqlite::ToSql]| {
                let mut statement = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
                let plans = statement
                    .query_map(bindings, |row| row.get::<_, String>(3))
                    .unwrap()
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .unwrap();
                assert!(
                    plans.iter().any(|detail| detail.contains("INDEX")),
                    "{plans:?}"
                );
                assert!(
                    plans.iter().all(|detail| !detail.contains("TEMP B-TREE")),
                    "{plans:?}"
                );
            };
            check(OWNER_SCAN_SQL, &[&32]);
            check(
                super::super::vault::SECRET_CLEANUP_DUE_SQL,
                &[
                    &super::super::benchmark_run::BENCHMARK_SECRET_NAMESPACE,
                    &Utc::now().to_rfc3339(),
                    &32,
                ],
            );
        }
        let peer = store
            .create_principal("retention-peer", "Peer", None)
            .unwrap()
            .id;
        store
            .upsert_tenant_membership(TenantId(1), peer, MembershipRole::Member)
            .unwrap();
        let owner = PrincipalId(1);
        let now = Utc::now();
        let first = saved(1, now - Duration::hours(1));
        let second = saved(2, now);
        for record in [&first, &second] {
            store
                .save_benchmark_run(TenantId(1), owner, record.clone())
                .await
                .unwrap();
        }
        assert_eq!(
            store
                .benchmark_retention(TenantId(1), owner)
                .await
                .unwrap()
                .policy,
            Default::default()
        );
        assert_eq!(
            store
                .maintain_benchmark_retention(10)
                .await
                .unwrap()
                .visited,
            0
        );
        let policy = BenchmarkRetentionPolicy {
            max_age_days: None,
            max_runs: Some(1),
        };
        let initial = preview(&store, owner, 0, policy.clone()).await;
        assert_eq!(initial.remove_count, 1);
        assert!(initial.remove_bytes > 0);
        let mut expired = apply(initial.clone());
        expired.as_of -= Duration::minutes(6);
        assert!(matches!(
            store
                .update_benchmark_retention(TenantId(1), owner, expired)
                .await,
            Err(MetadataError::BenchmarkRetentionConflict)
        ));
        assert!(matches!(
            store
                .update_benchmark_retention(TenantId(1), peer, apply(initial.clone()))
                .await,
            Err(MetadataError::BenchmarkRetentionConflict)
        ));
        assert!(store.benchmark_retention(TenantId(2), owner).await.is_err());
        store
            .update_benchmark_run_annotations(
                TenantId(1),
                owner,
                first.id,
                sift_protocol::UpdateBenchmarkRunAnnotationsRequest {
                    expected_revision: 0,
                    notes: "private-retention-note".into(),
                    tags: vec![],
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            store
                .update_benchmark_retention(TenantId(1), owner, apply(initial))
                .await,
            Err(MetadataError::BenchmarkRetentionConflict)
        ));
        let fresh = preview(&store, owner, 0, policy).await;
        let (left, right) = tokio::join!(
            store.update_benchmark_retention(TenantId(1), owner, apply(fresh.clone())),
            store.update_benchmark_retention(TenantId(1), owner, apply(fresh))
        );
        assert_ne!(left.is_ok(), right.is_ok());
        let accepted = left.or(right).unwrap();
        assert_eq!(accepted.removed, 1);
        assert!(store
            .get_benchmark_run(TenantId(1), owner, first.id)
            .await
            .is_err());
        let kept = handle(&store, second.id);
        // A stale queued handle that still has a current index reference is never deleted.
        store.conn().unwrap().execute("INSERT OR IGNORE INTO vault_secret_cleanup_queue(namespace,secret_handle,reason,not_before) VALUES('benchmark-runs',?1,'test','2000-01-01T00:00:00Z')",[&kept]).unwrap();
        gate.fail.store(true, Ordering::SeqCst);
        let (_, failed) = store.process_vault_secret_cleanup(100).await.unwrap();
        assert!(failed > 0);
        assert!(gate
            .get(
                super::super::benchmark_run::BENCHMARK_SECRET_NAMESPACE,
                &kept
            )
            .await
            .unwrap()
            .is_some());
        let queue_text: String = store
            .conn()
            .unwrap()
            .query_row(
                "SELECT group_concat(COALESCE(last_error,'')) FROM vault_secret_cleanup_queue",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!queue_text.contains("private-backend-error-sentinel"));
        gate.fail.store(false, Ordering::SeqCst);
        store
            .conn()
            .unwrap()
            .execute(
                "UPDATE vault_secret_cleanup_queue SET not_before='2000-01-01T00:00:00Z'",
                [],
            )
            .unwrap();
        assert_eq!(store.process_vault_secret_cleanup(100).await.unwrap().1, 0);
        let age = BenchmarkRetentionPolicy {
            max_age_days: Some(1),
            max_runs: None,
        };
        let reviewed = preview(&store, owner, 1, age).await;
        store
            .update_benchmark_retention(TenantId(1), owner, apply(reviewed))
            .await
            .unwrap();
        let third = saved(3, now + Duration::days(1));
        store
            .save_benchmark_run(TenantId(1), owner, third.clone())
            .await
            .unwrap();
        gate.hold_put.store(true, Ordering::SeqCst);
        let annotation_store = store.clone();
        let second_id = second.id;
        let annotation = tokio::spawn(async move {
            annotation_store
                .update_benchmark_run_annotations(
                    TenantId(1),
                    owner,
                    second_id,
                    sift_protocol::UpdateBenchmarkRunAnnotationsRequest {
                        expected_revision: 0,
                        notes: "private-retention-losing-note".into(),
                        tags: vec![],
                    },
                )
                .await
        });
        gate.started.notified().await;
        let report = store
            .maintain_benchmark_retention_at(1, now + Duration::hours(36))
            .await
            .unwrap();
        assert_eq!((report.visited, report.removed), (1, 1));
        gate.fail.store(true, Ordering::SeqCst);
        gate.resume.notify_one();
        assert!(matches!(
            annotation.await.unwrap(),
            Err(MetadataError::BenchmarkRunRevisionConflict)
        ));
        let pending:i64=store.conn().unwrap().query_row("SELECT COUNT(*) FROM vault_secret_cleanup_queue WHERE reason='uncommitted benchmark annotation'",[],|row|row.get(0)).unwrap();
        assert_eq!(pending, 1);
        let abandoned:String=store.conn().unwrap().query_row("SELECT secret_handle FROM vault_secret_cleanup_queue WHERE reason='uncommitted benchmark annotation'",[],|row|row.get(0)).unwrap();
        gate.fail.store(false, Ordering::SeqCst);
        store
            .conn()
            .unwrap()
            .execute(
                "UPDATE vault_secret_cleanup_queue SET not_before='2000-01-01T00:00:00Z'",
                [],
            )
            .unwrap();
        assert_eq!(store.process_vault_secret_cleanup(100).await.unwrap().1, 0);
        assert!(gate
            .get(
                super::super::benchmark_run::BENCHMARK_SECRET_NAMESPACE,
                &abandoned
            )
            .await
            .unwrap()
            .is_none());
        assert!(gate
            .get(
                super::super::benchmark_run::BENCHMARK_SECRET_NAMESPACE,
                &kept
            )
            .await
            .unwrap()
            .is_none());
        assert!(store
            .get_benchmark_run(TenantId(1), owner, second.id)
            .await
            .is_err());
        assert!(store
            .get_benchmark_run(TenantId(1), owner, third.id)
            .await
            .is_ok());
        let other = store
            .create_tenant("other-retention", crate::TenantKind::Personal)
            .unwrap()
            .id;
        store
            .upsert_tenant_membership(other, owner, MembershipRole::Member)
            .unwrap();
        assert_eq!(
            store
                .benchmark_retention(other, owner)
                .await
                .unwrap()
                .revision,
            0
        );
        let other_record = saved(20, now - Duration::days(3));
        store
            .save_benchmark_run(other, owner, other_record.clone())
            .await
            .unwrap();
        let other_review = store
            .preview_benchmark_retention(
                other,
                owner,
                PreviewBenchmarkRetentionRequest {
                    expected_revision: 0,
                    policy: BenchmarkRetentionPolicy {
                        max_age_days: None,
                        max_runs: Some(1),
                    },
                },
            )
            .await
            .unwrap();
        store
            .update_benchmark_retention(other, owner, apply(other_review))
            .await
            .unwrap();
        let peer_record = saved(10, now);
        store
            .save_benchmark_run(TenantId(1), peer, peer_record.clone())
            .await
            .unwrap();
        let peer_preview = preview(
            &store,
            peer,
            0,
            BenchmarkRetentionPolicy {
                max_age_days: Some(1),
                max_runs: None,
            },
        )
        .await;
        store
            .update_benchmark_retention(TenantId(1), peer, apply(peer_preview))
            .await
            .unwrap();
        store
            .conn()
            .unwrap()
            .execute(
                "DELETE FROM membership WHERE tenant_id=1 AND principal_id=?1",
                [peer.0],
            )
            .unwrap();
        assert!(store.benchmark_retention(TenantId(1), peer).await.is_err());
        store
            .maintain_benchmark_retention_at(100, now + Duration::hours(36))
            .await
            .unwrap();
        store
            .upsert_tenant_membership(TenantId(1), peer, MembershipRole::Member)
            .unwrap();
        assert_eq!(
            store
                .benchmark_retention(TenantId(1), peer)
                .await
                .unwrap()
                .revision,
            0
        );
        assert!(store
            .get_benchmark_run(TenantId(1), peer, peer_record.id)
            .await
            .is_ok());
        assert!(store
            .get_benchmark_run(other, owner, other_record.id)
            .await
            .is_ok());
        assert!(store
            .get_benchmark_run(TenantId(1), owner, other_record.id)
            .await
            .is_err());
        let audit = store.list_operation_audit(100).unwrap();
        assert!(audit.iter().any(|row| row.action == "prune"
            && row.actor_principal_id.is_none()
            && row.row_count == Some(1)));
        let audit = serde_json::to_string(&audit).unwrap();
        assert!(!audit.contains("private-retention"));
        drop(store);
        drop(gate);
        for path in [&db, &encrypted] {
            let bytes = std::fs::read(path).unwrap();
            assert!(!bytes
                .windows(b"private-retention".len())
                .any(|bytes| bytes == b"private-retention"));
        }
        let backend = Arc::new(FileSecretStore::open(&encrypted, &key).unwrap());
        let reopened = MetadataStore::open(&db, backend).unwrap();
        assert_eq!(
            reopened
                .benchmark_retention(TenantId(1), owner)
                .await
                .unwrap()
                .revision,
            2
        );
        assert!(reopened
            .get_benchmark_run(TenantId(1), owner, third.id)
            .await
            .is_ok());
    }
    #[tokio::test]
    async fn retention_save_prunes_before_hard_quotas_and_rolls_back_failed_saves() {
        let gate = Arc::new(DeleteGate {
            inner: Arc::new(MemorySecretStore::new()),
            fail: AtomicBool::new(false),
            hold_put: AtomicBool::new(false),
            started: tokio::sync::Notify::new(),
            resume: tokio::sync::Notify::new(),
        });
        let store = MetadataStore::open_in_memory(gate.clone()).unwrap();
        store.apply_migrations(false).unwrap();
        store.bootstrap_local("owner").unwrap();
        let owner = PrincipalId(1);
        let now = Utc::now();
        let first = saved(1, now);
        store
            .save_benchmark_run(TenantId(1), owner, first)
            .await
            .unwrap();
        {
            let mut conn = store.conn().unwrap();
            let tx = conn.transaction().unwrap();
            for index in 1000..1499 {
                tx.execute("INSERT INTO benchmark_run(id,tenant_id,owner_principal_id,run_id,saved_at,secret_handle,payload_bytes) VALUES(?1,1,1,?2,'2020-01-01T00:00:00Z',?3,1)",params![Uuid::from_u128(index).to_string(),Uuid::new_v4().to_string(),format!("missing-{index}")]).unwrap();
            }
            tx.commit().unwrap();
        }
        let cap = preview(
            &store,
            owner,
            0,
            BenchmarkRetentionPolicy {
                max_age_days: None,
                max_runs: Some(500),
            },
        )
        .await;
        assert_eq!(cap.remove_count, 0);
        store
            .update_benchmark_retention(TenantId(1), owner, apply(cap))
            .await
            .unwrap();
        let second = saved(2, now + Duration::seconds(1));
        store
            .save_benchmark_run(TenantId(1), owner, second.clone())
            .await
            .unwrap();
        assert_eq!(
            rows(&store.conn().unwrap(), TenantId(1), owner, 500)
                .unwrap()
                .len(),
            500
        );
        assert!(!rows(&store.conn().unwrap(), TenantId(1), owner, 500)
            .unwrap()
            .iter()
            .any(|row| row.id == Uuid::from_u128(1000)));
        let other: i64 = store
            .conn()
            .unwrap()
            .query_row(
                "SELECT SUM(payload_bytes) FROM benchmark_run WHERE id!=?1",
                [second.id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        store
            .conn()
            .unwrap()
            .execute(
                "UPDATE benchmark_run SET payload_bytes=?1 WHERE id=?2",
                params![67108864 - other - 100, second.id.to_string()],
            )
            .unwrap();
        let before = preview(
            &store,
            owner,
            1,
            BenchmarkRetentionPolicy {
                max_age_days: None,
                max_runs: Some(500),
            },
        )
        .await;
        gate.fail.store(true, Ordering::SeqCst);
        assert!(store
            .save_benchmark_run(TenantId(1), owner, saved(3, now + Duration::seconds(2)))
            .await
            .is_err());
        let after = store
            .preview_benchmark_retention(
                TenantId(1),
                owner,
                PreviewBenchmarkRetentionRequest {
                    expected_revision: 1,
                    policy: before.policy.clone(),
                },
            )
            .await
            .unwrap();
        assert_eq!(after.remove_count, 0);
        assert_eq!(
            rows(&store.conn().unwrap(), TenantId(1), owner, 500)
                .unwrap()
                .len(),
            500
        );
        let abandoned:i64=store.conn().unwrap().query_row("SELECT COUNT(*) FROM vault_secret_cleanup_queue WHERE reason='uncommitted benchmark snapshot'",[],|row|row.get(0)).unwrap();
        assert_eq!(abandoned, 1);
        gate.fail.store(false, Ordering::SeqCst);
        assert_eq!(store.process_vault_secret_cleanup(100).await.unwrap().1, 0);
        let peer = store
            .create_principal("fair-peer", "Peer", None)
            .unwrap()
            .id;
        store
            .upsert_tenant_membership(TenantId(1), peer, MembershipRole::Member)
            .unwrap();
        let reviewed = preview(
            &store,
            peer,
            0,
            BenchmarkRetentionPolicy {
                max_age_days: None,
                max_runs: Some(1),
            },
        )
        .await;
        store
            .update_benchmark_retention(TenantId(1), peer, apply(reviewed))
            .await
            .unwrap();
        store
            .conn()
            .unwrap()
            .execute(
                "UPDATE benchmark_retention SET last_checked_at='2000-01-01T00:00:00Z'",
                [],
            )
            .unwrap();
        assert_eq!(
            store.maintain_benchmark_retention(1).await.unwrap().visited,
            1
        );
        let old:i64=store.conn().unwrap().query_row("SELECT COUNT(*) FROM benchmark_retention WHERE last_checked_at='2000-01-01T00:00:00Z'",[],|row|row.get(0)).unwrap();
        assert_eq!(old, 1);
        assert_eq!(
            store.maintain_benchmark_retention(1).await.unwrap().visited,
            1
        );
        let old:i64=store.conn().unwrap().query_row("SELECT COUNT(*) FROM benchmark_retention WHERE last_checked_at='2000-01-01T00:00:00Z'",[],|row|row.get(0)).unwrap();
        assert_eq!(old, 0);
    }
}
