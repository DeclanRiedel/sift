//! Server-owned AI chat indexes. Potentially sensitive bodies live in ai_content.

use chrono::Utc;
use rusqlite::{params, OptionalExtension};
use sift_protocol::{AiChat, AiVisibility};
use uuid::Uuid;

use super::{sqlite_blocking, MetadataError, MetadataStore, PrincipalId, Result, RoomId, TenantId};

const MAX_TITLE_BYTES: usize = 256;
const MAX_CHATS_PER_TENANT: i64 = 10_000;

impl MetadataStore {
    /// Trusted HTTP scope lookup only; this does not grant chat visibility.
    pub async fn ai_chat_tenant(&self, id: Uuid) -> Result<TenantId> {
        let store = self.clone();
        super::sqlite_blocking(move || {
            let conn = store.conn()?;
            conn.query_row(
                "SELECT tenant_id FROM ai_chat WHERE id=?1",
                [id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .map(TenantId)
            .ok_or(MetadataError::AiNotFound)
        })
        .await
    }

    /// Trusted HTTP scope lookup only; run leases/visibility are checked
    /// separately before any content is read or mutated.
    pub async fn ai_run_tenant(&self, id: Uuid) -> Result<TenantId> {
        let store = self.clone();
        super::sqlite_blocking(move || {
            let conn = store.conn()?;
            conn.query_row(
                "SELECT c.tenant_id FROM ai_run r JOIN ai_chat c ON c.id=r.chat_id WHERE r.id=?1",
                [id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .map(TenantId)
            .ok_or(MetadataError::AiNotFound)
        })
        .await
    }

    /// Administrative key rotation; generation pointers and bytes stay in
    /// SecretStore. The content store resumes incomplete prior rotations.
    pub async fn rotate_ai_content_key(
        &self,
        tenant: TenantId,
        actor: PrincipalId,
    ) -> Result<usize> {
        let store = self.clone();
        super::sqlite_blocking(move || {
            let conn = store.conn()?;
            super::ensure_tenant_admin_locked(&conn, tenant, actor)
        })
        .await?;
        self.ai_content.rotate(tenant.0).await
    }

    /// Trusted offline recovery coordinator only. Returns opaque key handles,
    /// after checking referenced encrypted content is complete and readable.
    pub async fn ai_content_recovery_key_handles(&self, tenant: TenantId) -> Result<Vec<String>> {
        let handles = self.ai_content_handles(Some(tenant)).await?;
        let handles = handles
            .into_iter()
            .map(|(_, handle)| handle)
            .collect::<Vec<_>>();
        Ok(self
            .ai_content
            .recovery_key_handles(tenant.0, &handles)
            .await?
            .into_iter()
            .collect())
    }

    /// Snapshot-owned blob inventory. No body or key bytes enter SQLite.
    pub async fn ai_content_handles(&self, tenant: Option<TenantId>) -> Result<Vec<(i64, String)>> {
        let store = self.clone();
        super::sqlite_blocking(move || store.ai_content_handles_offline(tenant)).await
    }

    /// Trusted offline snapshot inventory under the exclusive maintenance lock.
    pub fn ai_content_handles_offline(
        &self,
        tenant: Option<TenantId>,
    ) -> Result<Vec<(i64, String)>> {
        let conn = self.conn()?;
        let mut statement = conn.prepare(
                "SELECT tenant_id,title_handle FROM ai_chat WHERE (?1 IS NULL OR tenant_id=?1)
                 UNION SELECT c.tenant_id,r.prompt_handle FROM ai_run r JOIN ai_chat c ON c.id=r.chat_id WHERE (?1 IS NULL OR c.tenant_id=?1)
                 UNION SELECT c.tenant_id,r.context_handle FROM ai_run r JOIN ai_chat c ON c.id=r.chat_id WHERE (?1 IS NULL OR c.tenant_id=?1)
                 UNION SELECT c.tenant_id,r.lease_handle FROM ai_run r JOIN ai_chat c ON c.id=r.chat_id WHERE (?1 IS NULL OR c.tenant_id=?1)
                 UNION SELECT c.tenant_id,e.content_handle FROM ai_run_event e JOIN ai_run r ON r.id=e.run_id JOIN ai_chat c ON c.id=r.chat_id WHERE e.content_handle IS NOT NULL AND (?1 IS NULL OR c.tenant_id=?1)
                 UNION SELECT c.tenant_id,p.target_handle FROM ai_proposal p JOIN ai_chat c ON c.id=p.chat_id WHERE (?1 IS NULL OR c.tenant_id=?1)
                 UNION SELECT c.tenant_id,p.content_handle FROM ai_proposal p JOIN ai_chat c ON c.id=p.chat_id WHERE (?1 IS NULL OR c.tenant_id=?1)
                 UNION SELECT tenant_id,content_handle FROM ai_proposal_review WHERE (?1 IS NULL OR tenant_id=?1)
                 UNION SELECT tenant_id,receipt_handle FROM ai_proposal_apply WHERE receipt_handle IS NOT NULL AND (?1 IS NULL OR tenant_id=?1)
                 UNION SELECT tenant_id,config_handle FROM ai_external_source WHERE (?1 IS NULL OR tenant_id=?1)
                 UNION SELECT tenant_id,content_handle FROM ai_external_room_grant WHERE (?1 IS NULL OR tenant_id=?1)
                 ORDER BY 1,2")?;
        let result = statement
            .query_map([tenant.map(|tenant| tenant.0)], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(result)
    }

    /// Validates encrypted content and keys internally; returns only ciphertext.
    pub fn read_validated_ai_blob(
        root: &std::path::Path,
        tenant: i64,
        id: Uuid,
        keys: &crate::FileSecretStore,
    ) -> Result<Vec<u8>> {
        crate::ai_content::read_validated_blob(root, tenant, id, keys)
    }
    /// Imports an authenticated ciphertext without exposing plaintext to callers.
    pub fn install_validated_ai_blob(
        root: &std::path::Path,
        tenant: i64,
        id: Uuid,
        sealed: Vec<u8>,
        keys: &crate::FileSecretStore,
    ) -> Result<()> {
        crate::ai_content::install_validated_blob(root, tenant, id, sealed, keys)
    }

    pub async fn create_ai_chat(
        &self,
        tenant: TenantId,
        room: Option<RoomId>,
        owner: PrincipalId,
        visibility: AiVisibility,
        title: String,
    ) -> Result<AiChat> {
        if title.trim().is_empty() || title.len() > MAX_TITLE_BYTES {
            return Err(MetadataError::AiInvalid(
                "chat title is empty or too long".into(),
            ));
        }
        if visibility == AiVisibility::RoomPublic && room.is_none() {
            return Err(MetadataError::AiInvalid(
                "room-public chat requires a room".into(),
            ));
        }
        let store = self.clone();
        sqlite_blocking(move || store.validate_ai_chat_scope(tenant, room, owner)).await?;
        let handle = self.ai_content.put(tenant.0, title.as_bytes()).await?;
        let id = Uuid::new_v4();
        let now = Utc::now();
        let stored_handle = handle.clone();
        let store = self.clone();
        let inserted = sqlite_blocking(move || {
            let mut conn = store.conn()?;
            let tx = conn.transaction()?;
            require_scope(&tx, tenant, room, owner)?;
            let count: i64 = tx.query_row(
                "SELECT COUNT(*) FROM ai_chat WHERE tenant_id=?1",
                [tenant.0],
                |row| row.get(0),
            )?;
            if count >= MAX_CHATS_PER_TENANT {
                return Err(MetadataError::AiInvalid("tenant chat limit reached".into()));
            }
            tx.execute(
                "INSERT INTO ai_chat(id,tenant_id,room_id,owner_principal_id,visibility,title_handle,revision,created_at,updated_at)
                 VALUES(?1,?2,?3,?4,?5,?6,1,?7,?7)",
                params![id.to_string(),tenant.0,room.map(|value| value.0),owner.0,visibility_text(visibility),stored_handle,now.to_rfc3339()],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await;
        if let Err(error) = inserted {
            self.ai_content.delete(tenant.0, &handle).await?;
            return Err(error);
        }
        Ok(AiChat {
            id,
            tenant_id: tenant.0,
            room_id: room.map(|value| value.0),
            owner_principal_id: owner.0,
            visibility,
            title,
            revision: 1,
            created_at: now,
            updated_at: now,
        })
    }

    pub async fn get_ai_chat(&self, id: Uuid, viewer: PrincipalId) -> Result<AiChat> {
        let store = self.clone();
        let record = sqlite_blocking(move || store.ai_chat_row(id, viewer)).await?;
        self.chat_from_record(record).await
    }

    pub async fn list_ai_chats(
        &self,
        tenant: TenantId,
        viewer: PrincipalId,
        limit: u32,
    ) -> Result<Vec<AiChat>> {
        let store = self.clone();
        let records = sqlite_blocking(move || {
            let conn = store.conn()?;
            let mut statement = conn.prepare(
                "SELECT c.id,c.tenant_id,c.room_id,c.owner_principal_id,c.visibility,c.title_handle,c.revision,c.created_at,c.updated_at
                 FROM ai_chat c
                 WHERE c.tenant_id=?1
                   AND EXISTS(SELECT 1 FROM membership m WHERE m.tenant_id=c.tenant_id AND m.principal_id=?2)
                   AND (c.room_id IS NULL OR EXISTS(
                       SELECT 1 FROM room_member rm WHERE rm.room_id=c.room_id AND rm.principal_id=?2))
                   AND (c.owner_principal_id=?2 OR (c.visibility='room_public' AND EXISTS(
                       SELECT 1 FROM room_member rm WHERE rm.room_id=c.room_id AND rm.principal_id=?2)))
                 ORDER BY c.updated_at DESC,c.id DESC LIMIT ?3",
            )?;
            let rows = statement.query_map(params![tenant.0, viewer.0, limit.clamp(1, 200)], record_from_row)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(Into::into)
        })
        .await?;
        let mut chats = Vec::with_capacity(records.len());
        for record in records {
            chats.push(self.chat_from_record(record).await?);
        }
        Ok(chats)
    }

    pub async fn delete_ai_chat(&self, id: Uuid, actor: PrincipalId) -> Result<()> {
        let store = self.clone();
        sqlite_blocking(move || {
            let mut conn = store.conn()?;
            let tx = conn.transaction()?;
            let (tenant_id, owner): (i64, i64) = tx
                .query_row(
                    "SELECT tenant_id,owner_principal_id FROM ai_chat WHERE id=?1",
                    [id.to_string()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?
                .ok_or(MetadataError::AiNotFound)?;
            let role: Option<String> = tx
                .query_row(
                    "SELECT role FROM membership WHERE tenant_id=?1 AND principal_id=?2",
                    params![tenant_id, actor.0],
                    |row| row.get(0),
                )
                .optional()?;
            if role.is_none()
                || (actor.0 != owner && !matches!(role.as_deref(), Some("owner" | "admin")))
            {
                return Err(MetadataError::AiAccessDenied);
            }
            tx.execute("DELETE FROM ai_chat WHERE id=?1", [id.to_string()])?;
            tx.commit()?;
            Ok(())
        })
        .await?;
        // Deletion is durable even when filesystem cleanup fails. The queue
        // captures all opaque handles transactionally through delete triggers.
        if self.process_ai_content_cleanup(100).await.is_err() {
            tracing::warn!("AI chat content cleanup deferred to maintenance");
        }
        Ok(())
    }

    fn validate_ai_chat_scope(
        &self,
        tenant: TenantId,
        room: Option<RoomId>,
        owner: PrincipalId,
    ) -> Result<()> {
        let conn = self.conn()?;
        require_scope(&conn, tenant, room, owner)
    }

    fn ai_chat_row(&self, id: Uuid, viewer: PrincipalId) -> Result<AiChatRecord> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT c.id,c.tenant_id,c.room_id,c.owner_principal_id,c.visibility,c.title_handle,c.revision,c.created_at,c.updated_at
             FROM ai_chat c WHERE c.id=?1
             AND EXISTS(SELECT 1 FROM membership m WHERE m.tenant_id=c.tenant_id AND m.principal_id=?2)
             AND (c.room_id IS NULL OR EXISTS(
                 SELECT 1 FROM room_member rm WHERE rm.room_id=c.room_id AND rm.principal_id=?2))
             AND (c.owner_principal_id=?2 OR (c.visibility='room_public' AND EXISTS(
                 SELECT 1 FROM room_member rm WHERE rm.room_id=c.room_id AND rm.principal_id=?2)))",
            params![id.to_string(), viewer.0],
            record_from_row,
        )
        .optional()?
        .ok_or(MetadataError::AiNotFound)
    }

    async fn chat_from_record(&self, record: AiChatRecord) -> Result<AiChat> {
        let title = self
            .ai_content
            .get(record.tenant_id, &record.title_handle)
            .await?
            .ok_or_else(|| MetadataError::AiContent("AI chat title is unavailable".into()))?;
        let title = String::from_utf8(title)
            .map_err(|_| MetadataError::AiContent("AI chat title is not UTF-8".into()))?;
        Ok(AiChat {
            id: Uuid::parse_str(&record.id)
                .map_err(|_| MetadataError::AiContent("AI chat ID is invalid".into()))?,
            tenant_id: record.tenant_id,
            room_id: record.room_id,
            owner_principal_id: record.owner_principal_id,
            visibility: match record.visibility.as_str() {
                "private" => AiVisibility::Private,
                "room_public" => AiVisibility::RoomPublic,
                _ => return Err(MetadataError::AiContent("AI visibility is invalid".into())),
            },
            title,
            revision: record.revision,
            created_at: super::parse_time_sql(record.created_at)?,
            updated_at: super::parse_time_sql(record.updated_at)?,
        })
    }
}

struct AiChatRecord {
    id: String,
    tenant_id: i64,
    room_id: Option<i64>,
    owner_principal_id: i64,
    visibility: String,
    title_handle: String,
    revision: u64,
    created_at: String,
    updated_at: String,
}

fn record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AiChatRecord> {
    Ok(AiChatRecord {
        id: row.get(0)?,
        tenant_id: row.get(1)?,
        room_id: row.get(2)?,
        owner_principal_id: row.get(3)?,
        visibility: row.get(4)?,
        title_handle: row.get(5)?,
        revision: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn visibility_text(visibility: AiVisibility) -> &'static str {
    match visibility {
        AiVisibility::Private => "private",
        AiVisibility::RoomPublic => "room_public",
    }
}

pub(super) fn require_chat_access(
    conn: &rusqlite::Connection,
    id: Uuid,
    viewer: PrincipalId,
) -> Result<i64> {
    conn.query_row(
        "SELECT c.tenant_id FROM ai_chat c WHERE c.id=?1
         AND EXISTS(SELECT 1 FROM membership m WHERE m.tenant_id=c.tenant_id AND m.principal_id=?2)
         AND (c.room_id IS NULL OR EXISTS(SELECT 1 FROM room_member rm WHERE rm.room_id=c.room_id AND rm.principal_id=?2))
         AND (c.owner_principal_id=?2 OR (c.visibility='room_public' AND EXISTS(
             SELECT 1 FROM room_member rm WHERE rm.room_id=c.room_id AND rm.principal_id=?2)))",
        params![id.to_string(), viewer.0],
        |row| row.get(0),
    )
    .optional()?
    .ok_or(MetadataError::AiNotFound)
}

fn require_scope(
    conn: &rusqlite::Connection,
    tenant: TenantId,
    room: Option<RoomId>,
    actor: PrincipalId,
) -> Result<()> {
    let member: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM membership WHERE tenant_id=?1 AND principal_id=?2)",
        params![tenant.0, actor.0],
        |row| row.get(0),
    )?;
    if !member {
        return Err(MetadataError::AiAccessDenied);
    }
    if let Some(room) = room {
        let allowed: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM room r JOIN room_member rm ON rm.room_id=r.id
             WHERE r.id=?1 AND r.tenant_id=?2 AND rm.principal_id=?3)",
            params![room.0, tenant.0, actor.0],
            |row| row.get(0),
        )?;
        if !allowed {
            return Err(MetadataError::AiAccessDenied);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{
        MembershipRole, MemorySecretStore, NewOperationAudit, NewRoom, RoomKind, RoomRole,
    };

    fn audit(actor: PrincipalId, room: RoomId) -> NewOperationAudit {
        NewOperationAudit {
            actor_principal_id: Some(actor),
            action: "add_member".into(),
            target: "room".into(),
            target_id: Some(room.0),
            status: "succeeded".into(),
            result_code: None,
            row_count: None,
            error_message: None,
            correlation_id: None,
        }
    }

    #[tokio::test]
    async fn private_and_public_chats_enforce_room_access_and_preserve_visibility() {
        let store = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        store.bootstrap_local("owner").unwrap();
        let owner = PrincipalId(1);
        let tenant = TenantId(1);
        let peer = store.create_principal("ai-peer", "peer", None).unwrap().id;
        let outsider = store
            .create_principal("ai-outsider", "outsider", None)
            .unwrap()
            .id;
        for principal in [peer, outsider] {
            store
                .upsert_tenant_membership(tenant, principal, MembershipRole::Member)
                .unwrap();
        }
        let room = store
            .create_room(
                tenant,
                owner,
                NewRoom {
                    name: "AI room".into(),
                    kind: RoomKind::Shared,
                },
            )
            .unwrap()
            .id;
        store
            .add_room_member_authorized(room, owner, peer, RoomRole::Viewer, audit(owner, room))
            .unwrap();

        let private = store
            .create_ai_chat(
                tenant,
                Some(room),
                owner,
                AiVisibility::Private,
                "private".into(),
            )
            .await
            .unwrap();
        assert!(matches!(
            store.get_ai_chat(private.id, peer).await,
            Err(MetadataError::AiNotFound)
        ));
        let public = store
            .create_ai_chat(
                tenant,
                Some(room),
                owner,
                AiVisibility::RoomPublic,
                "shared".into(),
            )
            .await
            .unwrap();
        assert_eq!(
            store.get_ai_chat(public.id, peer).await.unwrap().visibility,
            AiVisibility::RoomPublic
        );
        assert!(matches!(
            store.get_ai_chat(public.id, outsider).await,
            Err(MetadataError::AiNotFound)
        ));
        assert_eq!(
            store.list_ai_chats(tenant, peer, 50).await.unwrap().len(),
            1
        );
        assert_eq!(
            store
                .get_ai_chat(private.id, owner)
                .await
                .unwrap()
                .visibility,
            AiVisibility::Private
        );
    }

    #[tokio::test]
    async fn chat_title_uses_an_opaque_handle_and_delete_removes_content() {
        let store = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        store.bootstrap_local("owner").unwrap();
        let tenant = TenantId(1);
        let owner = PrincipalId(1);
        let title = "Confidential customer query";
        let chat = store
            .create_ai_chat(tenant, None, owner, AiVisibility::Private, title.into())
            .await
            .unwrap();
        let handle: String = store
            .conn()
            .unwrap()
            .query_row(
                "SELECT title_handle FROM ai_chat WHERE id=?1",
                [chat.id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_ne!(handle, title);
        assert!(Uuid::parse_str(&handle).is_ok());
        assert_eq!(
            store
                .ai_content
                .get(tenant.0, &handle)
                .await
                .unwrap()
                .unwrap(),
            title.as_bytes()
        );
        store.delete_ai_chat(chat.id, owner).await.unwrap();
        assert!(store
            .ai_content
            .get(tenant.0, &handle)
            .await
            .unwrap()
            .is_none());
    }
}
