//! Retention and retryable encrypted-body cleanup; policy/audit stay in SQLite.
use super::{sqlite_blocking, MetadataError, MetadataStore, PrincipalId, Result, TenantId};
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};

#[derive(Debug, Default)]
pub struct AiMaintenanceReport {
    pub interrupted_runs: usize,
    pub interrupted_database_applies: usize,
    pub expired_chats: usize,
    pub deleted_blobs: usize,
    pub failed_blobs: usize,
}
fn validate_days(days: Option<u32>) -> Result<()> {
    if days.is_some_and(|days| !(1..=36500).contains(&days)) {
        return Err(MetadataError::AiInvalid(
            "AI retention must be between 1 and 36500 days".into(),
        ));
    }
    Ok(())
}
impl MetadataStore {
    pub async fn ai_retention_days(
        &self,
        tenant: TenantId,
        actor: PrincipalId,
    ) -> Result<Option<u32>> {
        let store = self.clone();
        sqlite_blocking(move || {
            let conn = store.conn()?;
            super::ensure_tenant_membership_locked(&conn, tenant, actor)?;
            Ok(conn
                .query_row(
                    "SELECT retention_days FROM ai_tenant_retention WHERE tenant_id=?1",
                    [tenant.0],
                    |row| row.get::<_, Option<u32>>(0),
                )
                .optional()?
                .flatten())
        })
        .await
    }
    pub async fn set_ai_retention_days(
        &self,
        tenant: TenantId,
        actor: PrincipalId,
        days: Option<u32>,
    ) -> Result<()> {
        validate_days(days)?;
        let store = self.clone();
        sqlite_blocking(move|| {
            let mut conn=store.conn()?;
            let tx=conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            super::ensure_tenant_admin_locked(&tx,tenant,actor)?;
            tx.execute("INSERT INTO ai_tenant_retention(tenant_id,retention_days,updated_by,updated_at) VALUES(?1,?2,?3,?4) ON CONFLICT(tenant_id) DO UPDATE SET retention_days=excluded.retention_days,updated_by=excluded.updated_by,updated_at=excluded.updated_at",params![tenant.0,days,actor.0,Utc::now().to_rfc3339()])?;
            tx.commit()?;
            Ok(())
        }).await
    }
    /// Trusted periodic maintenance. Bounded transactions, no owner credentials.
    pub async fn maintain_ai_chats(
        &self,
        max_run_secs: u32,
        max_retention_days: Option<u32>,
        batch_size: u32,
    ) -> Result<AiMaintenanceReport> {
        self.maintain_ai_chats_at(Utc::now(), max_run_secs, max_retention_days, batch_size)
            .await
    }
    async fn maintain_ai_chats_at(
        &self,
        now: DateTime<Utc>,
        max_run_secs: u32,
        max_retention_days: Option<u32>,
        batch_size: u32,
    ) -> Result<AiMaintenanceReport> {
        validate_days(max_retention_days)?;
        if !(1..=3600).contains(&max_run_secs) || !(1..=1000).contains(&batch_size) {
            return Err(MetadataError::AiInvalid(
                "AI maintenance limits are invalid".into(),
            ));
        }
        let store = self.clone();
        let (interrupted_runs,expired_chats,interrupted_database_applies)=sqlite_blocking(move|| {
            let mut conn=store.conn()?;
            let tx=conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let cutoff=(now-chrono::Duration::seconds(i64::from(max_run_secs))).to_rfc3339();
            let now=now.to_rfc3339();
            let interrupted={
                let mut statement=tx.prepare("SELECT id,chat_id,next_sequence FROM ai_run WHERE status='running' AND julianday(started_at)<julianday(?1) ORDER BY started_at,id LIMIT ?2")?;
                let rows = statement.query_map(params![cutoff,batch_size],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,u64>(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            };
            for (run,chat,sequence) in &interrupted {
                tx.execute("INSERT INTO ai_run_event(run_id,sequence,kind,at) VALUES(?1,?2,'stopped',?3)",params![run,sequence,now])?;
                tx.execute("UPDATE ai_run SET status='interrupted',ended_at=?2,next_sequence=next_sequence+1 WHERE id=?1",params![run,now])?;
                maintenance_audit(&tx,"interrupt_run",chat,Some(run))?;
            }
            let applies={
                let mut statement=tx.prepare("SELECT a.proposal_id,p.chat_id,p.run_id FROM ai_proposal_apply a JOIN ai_proposal p ON p.id=a.proposal_id WHERE a.state='applying' AND julianday(a.claimed_at)<julianday(?1) ORDER BY a.claimed_at,a.proposal_id LIMIT ?2")?;
                let rows=statement.query_map(params![cutoff,batch_size],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            };
            for (proposal,chat,run) in &applies {
                tx.execute("UPDATE ai_proposal_apply SET state='outcome_unknown',finished_at=?2 WHERE proposal_id=?1",params![proposal,now])?;
                maintenance_audit(&tx,"interrupt_database_apply",chat,Some(run))?;
            }
            let expired={
                let mut statement=tx.prepare("SELECT c.id FROM ai_chat c LEFT JOIN ai_tenant_retention p ON p.tenant_id=c.tenant_id
                    WHERE julianday(c.updated_at) <= julianday(?1) - CASE WHEN ?2 IS NULL THEN p.retention_days WHEN p.retention_days IS NULL THEN ?2 ELSE min(p.retention_days,?2) END
                    AND NOT EXISTS(SELECT 1 FROM ai_run r WHERE r.chat_id=c.id AND r.status='running')
                    AND NOT EXISTS(SELECT 1 FROM ai_proposal_apply a JOIN ai_proposal p ON p.id=a.proposal_id WHERE p.chat_id=c.id AND a.state='applying')
                    ORDER BY c.updated_at,c.id LIMIT ?3")?;
                let rows = statement.query_map(params![now,max_retention_days,batch_size],|row|row.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            };
            for chat in &expired {
                tx.execute("DELETE FROM ai_chat WHERE id=?1",[chat])?;
                maintenance_audit(&tx,"expire_chat",chat,None)?;
            }
            tx.commit()?;
            Ok((interrupted.len(),expired.len(),applies.len()))
        }).await?;
        let (deleted_blobs, failed_blobs) = self.process_ai_content_cleanup(batch_size).await?;
        Ok(AiMaintenanceReport {
            interrupted_runs,
            interrupted_database_applies,
            expired_chats,
            deleted_blobs,
            failed_blobs,
        })
    }
    /// Queue rows survive failures and parent cascades. Existing references are
    /// protected, including handles reintroduced by coordinated tenant recovery.
    pub async fn process_ai_content_cleanup(&self, batch_size: u32) -> Result<(usize, usize)> {
        if !(1..=1000).contains(&batch_size) {
            return Err(MetadataError::AiInvalid(
                "AI cleanup batch limit is invalid".into(),
            ));
        }
        let store = self.clone();
        let queued=sqlite_blocking(move|| {
            let queued={
                let conn=store.conn()?;
                let mut statement=conn.prepare("SELECT tenant_id,content_handle FROM ai_content_cleanup ORDER BY attempts,queued_at,tenant_id,content_handle LIMIT ?1")?;
                let rows = statement.query_map([batch_size],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            };
            if queued.is_empty() { return Ok(Vec::new()); }
            let conn=store.conn()?;
            let mut statement=conn.prepare("SELECT EXISTS(
                SELECT 1 FROM ai_chat WHERE tenant_id=?1 AND title_handle=?2
                UNION ALL SELECT 1 FROM ai_run r JOIN ai_chat c ON c.id=r.chat_id WHERE c.tenant_id=?1 AND r.prompt_handle=?2
                UNION ALL SELECT 1 FROM ai_run r JOIN ai_chat c ON c.id=r.chat_id WHERE c.tenant_id=?1 AND r.context_handle=?2
                UNION ALL SELECT 1 FROM ai_run r JOIN ai_chat c ON c.id=r.chat_id WHERE c.tenant_id=?1 AND r.lease_handle=?2
                UNION ALL SELECT 1 FROM ai_run_event e JOIN ai_run r ON r.id=e.run_id JOIN ai_chat c ON c.id=r.chat_id WHERE c.tenant_id=?1 AND e.content_handle=?2
                UNION ALL SELECT 1 FROM ai_proposal p JOIN ai_chat c ON c.id=p.chat_id WHERE c.tenant_id=?1 AND p.target_handle=?2
                UNION ALL SELECT 1 FROM ai_proposal p JOIN ai_chat c ON c.id=p.chat_id WHERE c.tenant_id=?1 AND p.content_handle=?2
                UNION ALL SELECT 1 FROM ai_proposal_review WHERE tenant_id=?1 AND content_handle=?2
                UNION ALL SELECT 1 FROM ai_proposal_apply WHERE tenant_id=?1 AND receipt_handle=?2
                UNION ALL SELECT 1 FROM ai_external_source WHERE tenant_id=?1 AND config_handle=?2
                UNION ALL SELECT 1 FROM ai_external_room_grant WHERE tenant_id=?1 AND content_handle=?2)")?;
            queued.into_iter().map(|(tenant,handle)| {
                let referenced:bool=statement.query_row(params![tenant,handle],|row|row.get(0))?;
                Ok((tenant,handle,referenced))
            }).collect::<Result<Vec<_>>>()
        }).await?;
        let mut deleted = 0;
        let mut failed = 0;
        for (tenant, handle, referenced) in queued {
            let succeeded = referenced || self.ai_content.delete(tenant, &handle).await.is_ok();
            let store = self.clone();
            sqlite_blocking(move|| {
                let conn=store.conn()?;
                if succeeded { conn.execute("DELETE FROM ai_content_cleanup WHERE tenant_id=?1 AND content_handle=?2",params![tenant,handle])?; }
                else { conn.execute("UPDATE ai_content_cleanup SET attempts=attempts+1 WHERE tenant_id=?1 AND content_handle=?2",params![tenant,handle])?; }
                Ok(())
            }).await?;
            if !referenced {
                if succeeded {
                    deleted += 1;
                } else {
                    failed += 1;
                }
            }
        }
        Ok((deleted, failed))
    }
}
fn maintenance_audit(
    conn: &rusqlite::Connection,
    action: &str,
    chat: &str,
    run: Option<&str>,
) -> Result<()> {
    let operation = sift_protocol::Operation::Ai {
        action: action.into(),
        chat_id: uuid::Uuid::parse_str(chat).ok(),
        run_id: run.and_then(|id| uuid::Uuid::parse_str(id).ok()),
    };
    let summary = operation.audit_summary();
    super::insert_operation_audit_row(
        conn,
        &super::NewOperationAudit {
            actor_principal_id: None,
            action: summary.action,
            target: summary.target,
            target_id: summary.target_id,
            status: "succeeded".into(),
            result_code: None,
            row_count: None,
            error_message: None,
            correlation_id: Some(run.unwrap_or(chat).into()),
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MembershipRole, MemorySecretStore, TenantKind};
    use sift_protocol::{AiRunStatus, AiVisibility};
    use std::sync::Arc;
    use uuid::Uuid;
    async fn chat(
        store: &MetadataStore,
        tenant: TenantId,
        title: &str,
        at: DateTime<Utc>,
    ) -> sift_protocol::AiChat {
        let chat = store
            .create_ai_chat(
                tenant,
                None,
                PrincipalId(1),
                AiVisibility::Private,
                title.into(),
            )
            .await
            .unwrap();
        store
            .conn()
            .unwrap()
            .execute(
                "UPDATE ai_chat SET updated_at=?2 WHERE id=?1",
                params![chat.id.to_string(), at.to_rfc3339()],
            )
            .unwrap();
        chat
    }
    #[tokio::test]
    async fn retention_defaults_ceiling_active_runs_and_admin_authority() {
        let store = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        store.bootstrap_local("owner").unwrap();
        let now = Utc::now();
        let old = now - chrono::Duration::days(30);
        let expired = chat(&store, TenantId(1), "Expire after opting in", old).await;
        assert_eq!(
            store
                .maintain_ai_chats_at(now, 600, None, 100)
                .await
                .unwrap()
                .expired_chats,
            0
        );
        let peer = store.create_principal("member", "member", None).unwrap().id;
        store
            .upsert_tenant_membership(TenantId(1), peer, MembershipRole::Member)
            .unwrap();
        assert!(store
            .set_ai_retention_days(TenantId(1), peer, Some(3))
            .await
            .is_err());
        assert!(store
            .set_ai_retention_days(TenantId(1), PrincipalId(1), Some(0))
            .await
            .is_err());
        store
            .set_ai_retention_days(TenantId(1), PrincipalId(1), Some(3))
            .await
            .unwrap();
        assert_eq!(
            store.ai_retention_days(TenantId(1), peer).await.unwrap(),
            Some(3)
        );
        let recent = chat(
            &store,
            TenantId(1),
            "Recent",
            now - chrono::Duration::days(2),
        )
        .await;
        let active = chat(&store, TenantId(1), "Active desktop", old).await;
        let request=serde_json::from_value(serde_json::json!({"client_request_id":Uuid::new_v4(),"desktop_id":Uuid::new_v4(),"prompt":"Continue", "provider":"codex","model":null,"mode":"read",
            "context":{"target":{"tenant_id":1},"database":null,"dialect":null,"environment_label":null,"sql":null,"current_error":null,"staged_change_count":0}})).unwrap();
        let lease = store
            .start_ai_run(active.id, PrincipalId(1), request)
            .await
            .unwrap();
        store
            .conn()
            .unwrap()
            .execute(
                "UPDATE ai_chat SET updated_at=?2 WHERE id=?1",
                params![active.id.to_string(), old.to_rfc3339()],
            )
            .unwrap();
        let other = store
            .create_tenant("Default retention", TenantKind::Team)
            .unwrap()
            .id;
        store
            .upsert_tenant_membership(other, PrincipalId(1), MembershipRole::Owner)
            .unwrap();
        let default = chat(&store, other, "Default until ceiling", old).await;
        let report = store
            .maintain_ai_chats_at(now, 600, None, 100)
            .await
            .unwrap();
        assert_eq!(report.expired_chats, 1);
        assert_eq!(report.deleted_blobs, 1);
        assert!(store.get_ai_chat(expired.id, PrincipalId(1)).await.is_err());
        assert!(store.get_ai_chat(default.id, PrincipalId(1)).await.is_ok());
        assert!(store.get_ai_chat(recent.id, PrincipalId(1)).await.is_ok());
        assert!(store.get_ai_chat(active.id, PrincipalId(1)).await.is_ok());
        assert_eq!(
            store
                .maintain_ai_chats_at(now, 600, Some(7), 100)
                .await
                .unwrap()
                .expired_chats,
            1
        );
        store
            .conn()
            .unwrap()
            .execute(
                "UPDATE ai_run SET started_at=?2 WHERE id=?1",
                params![
                    lease.run.id.to_string(),
                    (now - chrono::Duration::seconds(601)).to_rfc3339()
                ],
            )
            .unwrap();
        let report = store
            .maintain_ai_chats_at(now, 600, Some(7), 100)
            .await
            .unwrap();
        assert_eq!(report.interrupted_runs, 1);
        assert_eq!(report.expired_chats, 1);
        assert!(report.deleted_blobs >= 4);
        let count: i64 = store
            .conn()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM operation_audit WHERE action='expire_chat'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 3);
        // A stale but retained run gets a durable stopped event and never resumes.
        let retained = chat(&store, TenantId(1), "Retained interrupted", now).await;
        let request=serde_json::from_value(serde_json::json!({"client_request_id":Uuid::new_v4(),"desktop_id":Uuid::new_v4(),"prompt":"Continue", "provider":"codex","model":null,"mode":"read",
            "context":{"target":{"tenant_id":1},"database":null,"dialect":null,"environment_label":null,"sql":null,"current_error":null,"staged_change_count":0}})).unwrap();
        let lease = store
            .start_ai_run(retained.id, PrincipalId(1), request)
            .await
            .unwrap();
        store
            .conn()
            .unwrap()
            .execute(
                "UPDATE ai_run SET started_at=?2 WHERE id=?1",
                params![
                    lease.run.id.to_string(),
                    (now - chrono::Duration::seconds(601)).to_rfc3339()
                ],
            )
            .unwrap();
        store
            .maintain_ai_chats_at(now, 600, None, 100)
            .await
            .unwrap();
        assert_eq!(
            store
                .list_ai_runs(retained.id, PrincipalId(1))
                .await
                .unwrap()[0]
                .run
                .status,
            AiRunStatus::Interrupted
        );
    }

    #[tokio::test]
    async fn failed_blob_deletion_retries_and_live_handles_are_protected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("metadata.sqlite");
        let store = MetadataStore::open(&path, Arc::new(MemorySecretStore::new())).unwrap();
        store.apply_migrations(false).unwrap();
        store.bootstrap_local("owner").unwrap();
        let doomed = chat(&store, TenantId(1), "Delete me", Utc::now()).await;
        let handles = store.ai_content_handles_offline(None).unwrap();
        let handle = &handles[0].1;
        let blob = path.with_extension("ai-content").join("1").join(handle);
        let original = std::fs::read(&blob).unwrap();
        std::fs::remove_file(&blob).unwrap();
        std::fs::create_dir(&blob).unwrap();
        store
            .delete_ai_chat(doomed.id, PrincipalId(1))
            .await
            .unwrap();
        let queued: i64 = store
            .conn()
            .unwrap()
            .query_row("SELECT count(*) FROM ai_content_cleanup", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(queued, 1);
        assert_eq!(store.process_ai_content_cleanup(100).await.unwrap(), (0, 1));
        std::fs::remove_dir(&blob).unwrap();
        std::fs::write(&blob, original).unwrap();
        assert_eq!(store.process_ai_content_cleanup(100).await.unwrap(), (1, 0));
        assert!(!blob.exists());
        let live = chat(&store, TenantId(1), "Restored same handle", Utc::now()).await;
        let handle = store.ai_content_handles_offline(None).unwrap()[0].1.clone();
        store.conn().unwrap().execute("INSERT INTO ai_content_cleanup(tenant_id,content_handle,queued_at) VALUES(1,?1,?2)",params![handle,Utc::now().to_rfc3339()]).unwrap();
        assert_eq!(store.process_ai_content_cleanup(100).await.unwrap(), (0, 0));
        assert_eq!(
            store
                .get_ai_chat(live.id, PrincipalId(1))
                .await
                .unwrap()
                .title,
            live.title
        );
        store
            .conn()
            .unwrap()
            .execute("DELETE FROM tenant WHERE id=1", [])
            .unwrap();
        assert_eq!(store.process_ai_content_cleanup(100).await.unwrap(), (1, 0));
    }
}
