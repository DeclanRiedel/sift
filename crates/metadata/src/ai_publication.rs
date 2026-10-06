//! Explicit public database scope, separate from a principal's connection access.
use super::{
    sqlite_blocking, ConnectionProfile, CredentialMode, MetadataError, MetadataStore, PrincipalId,
    Result, Room, RoomId, RoomKind,
};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};
use sift_protocol::{AiRoomPublication, AiRoomPublicationPreview, CreateAiRoomPublicationRequest};
use uuid::Uuid;

type VaultIdentity = Option<(i64, i64, u64, u64, Option<String>)>;

fn active_room_member(conn: &Connection, room: RoomId, actor: PrincipalId) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM room r JOIN room_member m ON m.room_id=r.id
         JOIN membership t ON t.tenant_id=r.tenant_id AND t.principal_id=m.principal_id
         JOIN principal p ON p.id=m.principal_id
         WHERE r.id=?1 AND m.principal_id=?2 AND p.disabled_at IS NULL)",
        params![room.0, actor.0],
        |row| row.get(0),
    )?)
}
fn vault_identity(conn: &Connection, profile: i64) -> Result<VaultIdentity> {
    Ok(conn
        .query_row(
            "SELECT i.vault_id,i.id,i.head_version,i.revision,v.secret_handle
         FROM vault_connection_binding b JOIN vault_item i ON i.id=b.item_id
         JOIN vault_item_version v ON v.item_id=i.id AND v.version=i.head_version
         WHERE b.connection_profile_id=?1",
            [profile],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?)
}
fn require_vault_use(
    conn: &Connection,
    identity: &VaultIdentity,
    actor: PrincipalId,
) -> Result<()> {
    if let Some((vault, ..)) = identity {
        super::vault::require_capability(
            conn,
            sift_api_types::VaultId(*vault),
            sift_api_types::PrincipalId(actor.0),
            |capabilities| capabilities.use_secret,
        )?;
    }
    Ok(())
}
fn source_preview(
    conn: &Connection,
    room: &Room,
    profile: &ConnectionProfile,
) -> Result<AiRoomPublicationPreview> {
    if room.kind != RoomKind::Shared || profile.credential_mode != CredentialMode::Shared {
        return Err(MetadataError::AiInvalid(
            "AI publication requires a shared room and shared-credential profile".into(),
        ));
    }
    if profile.tenant_id != room.tenant_id {
        return Err(MetadataError::AiAccessDenied);
    }
    let binder = room
        .bound_connection_by
        .ok_or_else(|| MetadataError::AiInvalid("room has no active connection binder".into()))?;
    if !active_room_member(conn, room.id, binder)? {
        return Err(MetadataError::AiInvalid(
            "room connection binder is no longer active; rebind the connection".into(),
        ));
    }
    let engine = profile.semantic_engine.ok_or_else(|| {
        MetadataError::AiInvalid("AI publication requires a supported SQL provider".into())
    })?;
    if engine == sift_protocol::Engine::Sqlite
        && serde_json::from_value::<sift_protocol::SqliteFileConfiguration>(
            profile.configuration.clone(),
        )
        .is_err()
    {
        return Err(MetadataError::AiInvalid(
            "Room AI publication requires a persistent SQLite file profile".into(),
        ));
    }
    let vault = vault_identity(conn, profile.id.0)?;
    require_vault_use(conn, &vault, binder)?;
    // Never resolve credentials: only their opaque identity/version enters the hash.
    let bytes = serde_json::to_vec(&(
        room.id.0,
        room.tenant_id.0,
        binder.0,
        profile.id.0,
        &profile.name,
        &profile.provider_id,
        profile.semantic_engine,
        &profile.configuration,
        &profile.tags,
        &profile.policy,
        &profile.shared_secret_handle,
        &vault,
    ))?;
    let file_database = if engine == sift_protocol::Engine::Sqlite {
        serde_json::from_value::<sift_protocol::SqliteFileConfiguration>(
            profile.configuration.clone(),
        )
        .ok()
        .map(|file| format!("{}:{}", file.root_id, file.path))
    } else {
        None
    };
    let database = file_database.or_else(|| {
        ["database", "dbname", "catalog", "initial_catalog"]
            .into_iter()
            .find_map(|key| {
                profile
                    .configuration
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .filter(|value| !value.is_empty() && value.len() <= 256)
                    .map(str::to_owned)
            })
    });
    Ok(AiRoomPublicationPreview {
        tenant_id: room.tenant_id.0,
        room_id: room.id.0,
        profile_id: profile.id.0,
        profile_name: profile.name.clone(),
        provider_id: profile.provider_id.clone(),
        database,
        dialect: engine.as_str().into(),
        scope_digest: format!("{:x}", Sha256::digest(bytes)),
    })
}

impl MetadataStore {
    /// Opaque source identity for private typed proposals. Callers separately
    /// compare the live connection's configuration with the durable profile.
    pub async fn ai_profile_source_digest(
        &self,
        profile: super::ConnectionProfileId,
        actor: PrincipalId,
    ) -> Result<String> {
        let store = self.clone();
        sqlite_blocking(move || {
            let conn=store.conn()?;
            let profile=super::connection_profile_by_id_locked(&conn,profile)?;
            let active:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM membership m JOIN principal p ON p.id=m.principal_id WHERE m.tenant_id=?1 AND m.principal_id=?2 AND p.disabled_at IS NULL)",params![profile.tenant_id.0,actor.0],|row|row.get(0))?;
            if !active {return Err(MetadataError::AiAccessDenied);}
            let vault=vault_identity(&conn,profile.id.0)?;
            require_vault_use(&conn,&vault,actor)?;
            let personal:Option<String>=if profile.credential_mode==CredentialMode::PerUser {
                conn.query_row("SELECT secret_handle FROM connection_credential WHERE connection_profile_id=?1 AND principal_id=?2",params![profile.id.0,actor.0],|row|row.get(0)).optional()?
            } else {None};
            let bytes=serde_json::to_vec(&(profile.tenant_id.0,profile.id.0,&profile.provider_id,profile.semantic_engine,&profile.name,&profile.tags,&profile.credential_mode,&profile.configuration,&profile.policy,&profile.shared_secret_handle,&vault,personal))?;
            Ok(format!("{:x}",Sha256::digest(bytes)))
        }).await
    }
    fn ai_publication_source_locked(
        &self,
        conn: &Connection,
        room: RoomId,
    ) -> Result<(Room, ConnectionProfile, AiRoomPublicationPreview)> {
        let room = self.room_by_id_locked(conn, room)?;
        let profile = room.bound_connection_profile_id.ok_or_else(|| {
            MetadataError::AiInvalid(
                "bind a shared-credential connection before publishing AI database context".into(),
            )
        })?;
        let profile = super::connection_profile_by_id_locked(conn, profile)?;
        let preview = source_preview(conn, &room, &profile)?;
        Ok((room, profile, preview))
    }
    pub async fn preview_ai_room_publication(
        &self,
        room: RoomId,
        actor: PrincipalId,
    ) -> Result<AiRoomPublicationPreview> {
        let store = self.clone();
        sqlite_blocking(move || {
            let conn = store.conn()?;
            if !active_room_member(&conn, room, actor)? {
                return Err(MetadataError::AiAccessDenied);
            }
            super::ensure_room_owner_locked(&conn, room, actor)?;
            let (_, profile, preview) = store.ai_publication_source_locked(&conn, room)?;
            require_vault_use(&conn, &vault_identity(&conn, profile.id.0)?, actor)?;
            Ok(preview)
        })
        .await
    }
    pub async fn create_ai_room_publication(
        &self,
        room: RoomId,
        actor: PrincipalId,
        request: CreateAiRoomPublicationRequest,
    ) -> Result<AiRoomPublication> {
        if request.client_request_id.is_nil()
            || request.expected_profile_id <= 0
            || request.expected_scope_digest.len() != 64
        {
            return Err(MetadataError::AiInvalid(
                "AI publication review identity is invalid".into(),
            ));
        }
        let store = self.clone();
        sqlite_blocking(move || {
            let mut conn = store.conn()?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if !active_room_member(&tx,room,actor)? { return Err(MetadataError::AiAccessDenied); }
            super::ensure_room_owner_locked(&tx,room,actor)?;
            let (_,profile,source) = store.ai_publication_source_locked(&tx,room)?;
            require_vault_use(&tx,&vault_identity(&tx,profile.id.0)?,actor)?;
            if source.profile_id!=request.expected_profile_id || source.scope_digest!=request.expected_scope_digest {
                return Err(MetadataError::AiInvalid("room connection changed; refresh the publication preview".into()));
            }
            let prior: Option<(String,String,bool,i64,String,bool)> = tx.query_row(
                "SELECT id,scope_digest,allow_rows,created_by,created_at,revoked_at IS NULL FROM ai_room_publication WHERE room_id=?1 AND client_request_id=?2",
                params![room.0,request.client_request_id.to_string()],
                |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)),
            ).optional()?;
            if let Some((id,digest,rows,creator,at,active)) = prior {
                if !active || creator!=actor.0 || digest!=source.scope_digest || rows!=request.allow_rows {
                    return Err(MetadataError::AiInvalid("publication request ID was reused or its grant was revoked".into()));
                }
                return Ok(AiRoomPublication {id:parse_id(&id)?,source,allow_rows:rows,created_by:creator,created_at:parse_at(&at)?});
            }
            let now = Utc::now();
            tx.execute("UPDATE ai_room_publication SET revoked_at=?2 WHERE room_id=?1 AND revoked_at IS NULL",params![room.0,now.to_rfc3339()])?;
            let id = Uuid::new_v4();
            let binder = profile_source_binder(&tx,room)?;
            tx.execute("INSERT INTO ai_room_publication(id,client_request_id,tenant_id,room_id,profile_id,binder_id,scope_digest,allow_rows,created_by,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![id.to_string(),request.client_request_id.to_string(),source.tenant_id,room.0,source.profile_id,binder.0,source.scope_digest,request.allow_rows,actor.0,now.to_rfc3339()])?;
            tx.commit()?;
            Ok(AiRoomPublication {id,source,allow_rows:request.allow_rows,created_by:actor.0,created_at:now})
        }).await
    }
    /// Current published labels are room-readable. Database/vault execution
    /// permission is checked separately for each initiating tool caller.
    pub async fn ai_room_publication(
        &self,
        room: RoomId,
        actor: PrincipalId,
    ) -> Result<Option<AiRoomPublication>> {
        let store = self.clone();
        sqlite_blocking(move || {
            let conn = store.conn()?;
            if !active_room_member(&conn,room,actor)? { return Err(MetadataError::AiAccessDenied); }
            let record: Option<(String,String,bool,i64,String)> = conn.query_row(
                "SELECT id,scope_digest,allow_rows,created_by,created_at FROM ai_room_publication WHERE room_id=?1 AND revoked_at IS NULL",
                [room.0],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
            ).optional()?;
            let Some((id,digest,rows,creator,at)) = record else { return Ok(None); };
            let issuer = PrincipalId(creator);
            if !active_room_member(&conn,room,issuer)? || super::ensure_room_owner_locked(&conn,room,issuer).is_err() { return Ok(None); }
            let (_,profile,source) = match store.ai_publication_source_locked(&conn,room) {
                Ok(source)=>source,
                Err(MetadataError::AiInvalid(_) | MetadataError::VaultPermissionDenied | MetadataError::TenantMembershipRequired{..})=>return Ok(None),
                Err(error)=>return Err(error),
            };
            if digest!=source.scope_digest || require_vault_use(&conn,&vault_identity(&conn,profile.id.0)?,issuer).is_err() { return Ok(None); }
            Ok(Some(AiRoomPublication {id:parse_id(&id)?,source,allow_rows:rows,created_by:creator,created_at:parse_at(&at)?}))
        }).await
    }
    pub async fn revoke_ai_room_publication(&self, room: RoomId, actor: PrincipalId) -> Result<()> {
        let store = self.clone();
        sqlite_blocking(move || {
            let mut conn = store.conn()?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if !active_room_member(&tx,room,actor)? { return Err(MetadataError::AiAccessDenied); }
            super::ensure_room_owner_locked(&tx,room,actor)?;
            tx.execute("UPDATE ai_room_publication SET revoked_at=?2 WHERE room_id=?1 AND revoked_at IS NULL",params![room.0,Utc::now().to_rfc3339()])?;
            tx.commit()?;
            Ok(())
        }).await
    }
}
fn profile_source_binder(conn: &Connection, room: RoomId) -> Result<PrincipalId> {
    conn.query_row(
        "SELECT bound_connection_by FROM room WHERE id=?1",
        [room.0],
        |row| row.get::<_, i64>(0).map(PrincipalId),
    )
    .map_err(Into::into)
}
fn parse_id(id: &str) -> Result<Uuid> {
    Uuid::parse_str(id).map_err(|_| MetadataError::AiContent("AI publication ID is invalid".into()))
}
fn parse_at(at: &str) -> Result<chrono::DateTime<Utc>> {
    chrono::DateTime::parse_from_rfc3339(at)
        .map(|at| at.with_timezone(&Utc))
        .map_err(|_| MetadataError::AiContent("AI publication timestamp is invalid".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        MembershipRole, MemorySecretStore, NewConnectionProfile, NewOperationAudit, NewRoom,
        RoomRole, TenantId,
    };
    use std::sync::Arc;
    fn audit(actor: PrincipalId) -> NewOperationAudit {
        NewOperationAudit {
            actor_principal_id: Some(actor),
            action: "test".into(),
            target: "room".into(),
            target_id: None,
            status: "succeeded".into(),
            result_code: None,
            row_count: None,
            error_message: None,
            correlation_id: None,
        }
    }
    #[tokio::test]
    async fn publication_pins_reviewed_scope_and_requires_current_owner_authority() {
        let store = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        store.bootstrap_local("owner").unwrap();
        let owner = PrincipalId(1);
        let tenant = TenantId(1);
        let peer = store
            .create_principal("publication-peer", "peer", None)
            .unwrap()
            .id;
        store
            .upsert_tenant_membership(tenant, peer, MembershipRole::Member)
            .unwrap();
        let room = store
            .create_room(
                tenant,
                owner,
                NewRoom {
                    name: "public".into(),
                    kind: RoomKind::Shared,
                },
            )
            .unwrap();
        store
            .add_room_member_authorized(room.id, owner, peer, RoomRole::Viewer, audit(owner))
            .unwrap();
        assert!(store
            .preview_ai_room_publication(room.id, owner)
            .await
            .is_err());
        let input = |database: &str, mode| NewConnectionProfile {
            name: "shared".into(),
            provider_id: sift_protocol::Engine::Postgres.provider_id(),
            configuration: serde_json::json!({"database":database}),
            semantic_engine: Some(sift_protocol::Engine::Postgres),
            credentials: None,
            credential_mode: mode,
            tags: vec![],
        };
        let profile = store
            .upsert_connection_profile(tenant, owner, input("first", CredentialMode::Shared))
            .await
            .unwrap();
        store
            .bind_room_connection(room.id, owner, profile.id, audit(owner))
            .unwrap();
        assert!(store
            .preview_ai_room_publication(room.id, peer)
            .await
            .is_err());
        let preview = store
            .preview_ai_room_publication(room.id, owner)
            .await
            .unwrap();
        let request = CreateAiRoomPublicationRequest {
            client_request_id: Uuid::new_v4(),
            expected_profile_id: profile.id.0,
            expected_scope_digest: preview.scope_digest,
            allow_rows: false,
        };
        assert!(store
            .create_ai_room_publication(room.id, peer, request.clone())
            .await
            .is_err());
        let grant = store
            .create_ai_room_publication(room.id, owner, request.clone())
            .await
            .unwrap();
        assert!(!grant.allow_rows);
        assert_eq!(
            store
                .create_ai_room_publication(room.id, owner, request.clone())
                .await
                .unwrap()
                .id,
            grant.id
        );
        assert_eq!(
            store
                .ai_room_publication(room.id, peer)
                .await
                .unwrap()
                .unwrap()
                .id,
            grant.id
        );
        store
            .upsert_connection_profile(tenant, owner, input("changed", CredentialMode::Shared))
            .await
            .unwrap();
        assert!(store
            .ai_room_publication(room.id, owner)
            .await
            .unwrap()
            .is_none());
        assert!(store
            .create_ai_room_publication(room.id, owner, request)
            .await
            .is_err());
        let preview = store
            .preview_ai_room_publication(room.id, owner)
            .await
            .unwrap();
        let request = CreateAiRoomPublicationRequest {
            client_request_id: Uuid::new_v4(),
            expected_profile_id: profile.id.0,
            expected_scope_digest: preview.scope_digest,
            allow_rows: true,
        };
        let grant = store
            .create_ai_room_publication(room.id, owner, request.clone())
            .await
            .unwrap();
        assert!(grant.allow_rows);
        assert!(store
            .revoke_ai_room_publication(room.id, peer)
            .await
            .is_err());
        store
            .revoke_ai_room_publication(room.id, owner)
            .await
            .unwrap();
        assert!(store
            .ai_room_publication(room.id, peer)
            .await
            .unwrap()
            .is_none());
        assert!(store
            .create_ai_room_publication(room.id, owner, request.clone())
            .await
            .is_err());
        let mut request = request;
        request.client_request_id = Uuid::new_v4();
        store
            .create_ai_room_publication(room.id, owner, request)
            .await
            .unwrap();
        store
            .conn()
            .unwrap()
            .execute(
                "DELETE FROM membership WHERE tenant_id=1 AND principal_id=1",
                [],
            )
            .unwrap();
        assert!(store
            .ai_room_publication(room.id, peer)
            .await
            .unwrap()
            .is_none());
    }
    #[tokio::test]
    async fn vault_rotation_policy_change_and_recovery_invalidate_publication() {
        let store = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        store.bootstrap_local("owner").unwrap();
        let owner = PrincipalId(1);
        let tenant = TenantId(1);
        let api_owner = sift_api_types::PrincipalId(1);
        let api_tenant = sift_api_types::TenantId(1);
        let room = store
            .create_room(
                tenant,
                owner,
                NewRoom {
                    name: "vault publication".into(),
                    kind: RoomKind::Shared,
                },
            )
            .unwrap();
        let vault = store
            .create_team_vault(api_tenant, api_owner, "Database")
            .unwrap();
        let (profile, item_id) = store
            .upsert_vault_connection_profile(
                api_tenant,
                api_owner,
                Some(vault.id),
                NewConnectionProfile {
                    name: "vault database".into(),
                    provider_id: sift_protocol::Engine::Postgres.provider_id(),
                    configuration: serde_json::json!({"database":"shared"}),
                    semantic_engine: Some(sift_protocol::Engine::Postgres),
                    credentials: Some(serde_json::json!({"password":"test-only-first"})),
                    credential_mode: CredentialMode::Shared,
                    tags: vec![],
                },
                None,
            )
            .await
            .unwrap();
        store
            .bind_room_connection(room.id, owner, profile.id, audit(owner))
            .unwrap();
        async fn publish(
            store: &MetadataStore,
            room: RoomId,
            owner: PrincipalId,
        ) -> AiRoomPublication {
            let preview = store
                .preview_ai_room_publication(room, owner)
                .await
                .unwrap();
            store
                .create_ai_room_publication(
                    room,
                    owner,
                    CreateAiRoomPublicationRequest {
                        client_request_id: Uuid::new_v4(),
                        expected_profile_id: preview.profile_id,
                        expected_scope_digest: preview.scope_digest,
                        allow_rows: false,
                    },
                )
                .await
                .unwrap()
        }
        let first = publish(&store, room.id, owner).await;
        let item = store.get_vault_item(item_id, api_owner).unwrap();
        store
            .set_vault_secret(
                item_id,
                api_owner,
                item.revision,
                serde_json::json!({"password":"test-only-second"}),
            )
            .await
            .unwrap();
        assert!(store
            .ai_room_publication(room.id, owner)
            .await
            .unwrap()
            .is_none());
        let second = publish(&store, room.id, owner).await;
        assert_ne!(first.source.scope_digest, second.source.scope_digest);
        store
            .update_connection_policy(
                tenant,
                owner,
                profile.id,
                sift_protocol::UpdateConnectionPolicyRequest {
                    expected_revision: Some(0),
                    minimum_tenant_role: sift_protocol::TenantRole::Member,
                    read_only: true,
                    allowed_ops: None,
                    blocked_ops: vec![],
                    allowed_schemas: None,
                },
                audit(owner),
            )
            .unwrap();
        assert!(store
            .ai_room_publication(room.id, owner)
            .await
            .unwrap()
            .is_none());
        publish(&store, room.id, owner).await;
        store.sanitize_workspace_backup_snapshot().unwrap();
        assert!(store
            .ai_room_publication(room.id, owner)
            .await
            .unwrap()
            .is_none());
        let restored = publish(&store, room.id, owner).await;
        assert_ne!(second.id, restored.id);
    }
}
