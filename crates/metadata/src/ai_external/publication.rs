//! Independent room grants. Room membership never implies private vault use.
use super::*;
use crate::RoomId;
use sift_protocol::{AiExternalRoomGrant, PublishAiExternalSourceRequest};

struct GrantRecord {
    tenant: TenantId,
    room: RoomId,
    source: Uuid,
    publisher: PrincipalId,
    handle: String,
}

fn grant_record(conn: &rusqlite::Connection, id: Uuid) -> Result<GrantRecord> {
    conn.query_row("SELECT tenant_id,room_id,source_id,published_by,content_handle FROM ai_external_room_grant WHERE id=?1",[id.to_string()], |row| {
        let source: String = row.get(2)?;
        Ok(GrantRecord { tenant: TenantId(row.get(0)?), room: RoomId(row.get(1)?),
            source: Uuid::parse_str(&source).map_err(|_| rusqlite::Error::InvalidQuery)?,
            publisher: PrincipalId(row.get(3)?), handle: row.get(4)? })
    }).optional()?.ok_or(MetadataError::AiNotFound)
}

fn room_access(
    conn: &rusqlite::Connection,
    tenant: TenantId,
    room: RoomId,
    actor: PrincipalId,
) -> Result<()> {
    let active: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM room r JOIN room_member m ON m.room_id=r.id JOIN membership t ON t.tenant_id=r.tenant_id AND t.principal_id=m.principal_id JOIN principal p ON p.id=m.principal_id WHERE r.id=?1 AND r.tenant_id=?2 AND r.kind='shared' AND m.principal_id=?3 AND p.disabled_at IS NULL)",
        params![room.0,tenant.0,actor.0], |row|row.get(0),
    )?;
    if active {
        Ok(())
    } else {
        Err(MetadataError::AiAccessDenied)
    }
}

fn grant_authority(
    conn: &rusqlite::Connection,
    id: Uuid,
    actor: PrincipalId,
) -> Result<GrantRecord> {
    let grant = grant_record(conn, id)?;
    let entry = record(conn, grant.source)?;
    if entry.tenant != grant.tenant || grant.publisher != entry.owner {
        return Err(MetadataError::AiAccessDenied);
    }
    vault_scope(conn, entry.tenant, entry.owner, entry.vault, false)?;
    room_access(conn, grant.tenant, grant.room, grant.publisher)?;
    room_access(conn, grant.tenant, grant.room, actor)?;
    Ok(grant)
}

pub(crate) struct ExternalSourceAuthorization {
    proof: AiExternalSourceProof,
    room: Option<RoomId>,
    grant_handle: Option<String>,
}

impl ExternalSourceAuthorization {
    pub(crate) fn require(
        &self,
        conn: &rusqlite::Connection,
        tenant: TenantId,
        actor: PrincipalId,
    ) -> Result<()> {
        let entry = require_source_pin(conn, tenant, &self.proof)?;
        match (
            self.room,
            self.proof.room_grant_id,
            self.grant_handle.as_ref(),
        ) {
            (None, None, None) => readable(conn, &entry, actor),
            (Some(room), Some(id), Some(handle)) => {
                let grant = grant_authority(conn, id, actor)?;
                if grant.tenant != tenant
                    || grant.room != room
                    || grant.source != entry.id
                    || grant.handle != *handle
                {
                    return Err(invalid(
                        "Room source grant changed; review its publication again",
                    ));
                }
                Ok(())
            }
            _ => Err(invalid(
                "External source visibility does not match this turn",
            )),
        }
    }
}

impl MetadataStore {
    pub async fn revoke_ai_external_room_grant(&self, id: Uuid, actor: PrincipalId) -> Result<()> {
        let store = self.clone();
        sqlite_blocking(move || {
            let mut conn = store.conn()?; let tx = conn.transaction()?;
            let grant = grant_record(&tx,id)?;
            let source = record(&tx,grant.source)?;
            crate::ensure_principal_tenant_member_locked(&tx,grant.tenant,actor)?;
            let administrator: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM membership WHERE tenant_id=?1 AND principal_id=?2 AND role IN ('owner','admin'))",params![grant.tenant.0,actor.0],|row|row.get(0))?;
            let room_owner: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM room_member WHERE room_id=?1 AND principal_id=?2 AND role='owner')",params![grant.room.0,actor.0],|row|row.get(0))?;
            if source.tenant != grant.tenant || (source.owner != actor && !administrator && !room_owner) {
                return Err(MetadataError::AiAccessDenied);
            }
            tx.execute("DELETE FROM ai_external_room_grant WHERE id=?1",[id.to_string()])?;
            tx.commit()?; Ok(())
        }).await
    }

    pub(crate) async fn authorize_ai_external_turn_sources(
        &self,
        chat: &sift_protocol::AiChat,
        actor: PrincipalId,
        context: &sift_protocol::AiTurnContext,
    ) -> Result<Vec<ExternalSourceAuthorization>> {
        let tenant = TenantId(chat.tenant_id);
        let mut authorizations = Vec::new();
        for proof in &context.external_sources {
            let (room, handle) = if chat.visibility == sift_protocol::AiVisibility::RoomPublic {
                let room = RoomId(
                    chat.room_id
                        .ok_or_else(|| invalid("Public external sources require a room"))?,
                );
                self.authorize_ai_external_room_source(tenant, room, actor, proof)
                    .await?;
                let store = self.clone();
                let grant_id = proof
                    .room_grant_id
                    .ok_or_else(|| invalid("External source has no room grant"))?;
                let handle = sqlite_blocking(move || {
                    let conn = store.conn()?;
                    Ok(grant_authority(&conn, grant_id, actor)?.handle)
                })
                .await?;
                (Some(room), Some(handle))
            } else {
                self.authorize_ai_external_private_source(tenant, actor, proof)
                    .await?;
                (None, None)
            };
            authorizations.push(ExternalSourceAuthorization {
                proof: proof.clone(),
                room,
                grant_handle: handle,
            });
        }
        Ok(authorizations)
    }

    pub async fn publish_ai_external_source(
        &self,
        room: RoomId,
        actor: PrincipalId,
        request: PublishAiExternalSourceRequest,
    ) -> Result<AiExternalRoomGrant> {
        if !request.publish_future_results_to_room
            || request.source.room_grant_id.is_some()
            || request.tool_aliases.is_empty()
        {
            return Err(invalid(
                "Review the source and acknowledge future room results before publishing it",
            ));
        }
        let source = self
            .ai_external_source(request.source.source_id, actor)
            .await?;
        let tenant = TenantId(source.tenant_id);
        if source.owner_principal_id != actor.0
            || !source_matches_proof(&source, tenant, &request.source)
        {
            return Err(MetadataError::AiAccessDenied);
        }
        let mut aliases = std::collections::HashSet::new();
        if request.tool_aliases.len() > 32
            || request.tool_aliases.iter().any(|alias| {
                !aliases.insert(alias.clone())
                    || !source.definition.tools.iter().any(|tool| {
                        tool.alias == *alias && tool.policy != AiExternalToolPolicy::Unavailable
                    })
            })
        {
            return Err(invalid("Room grant aliases must be unique reviewed tools"));
        }
        let store = self.clone();
        let proof = request.source.clone();
        sqlite_blocking(move || {
            let conn = store.conn()?;
            let entry = require_source_pin(&conn, tenant, &proof)?;
            editable(&conn, &entry, actor)?;
            room_access(&conn, tenant, room, actor)?;
            crate::ensure_room_owner_locked(&conn, room, actor)?;
            Ok(())
        })
        .await?;
        let id = Uuid::new_v4();
        let mut proof = request.source;
        proof.room_grant_id = Some(id);
        let grant = AiExternalRoomGrant {
            id,
            room_id: room.0,
            source: proof,
            tool_aliases: request.tool_aliases,
            published_by: actor.0,
            created_at: Utc::now(),
        };
        let handle = self
            .ai_content
            .put(tenant.0, &serde_json::to_vec(&grant)?)
            .await?;
        let stored_handle = handle.clone();
        let saved = grant.clone();
        let store = self.clone();
        let result = sqlite_blocking(move || {
            let mut conn = store.conn()?; let tx = conn.transaction()?;
            let entry = require_source_pin(&tx,tenant,&saved.source)?;
            editable(&tx,&entry,actor)?;
            room_access(&tx,tenant,room,actor)?;
            crate::ensure_room_owner_locked(&tx,room,actor)?;
            let current: Option<String> = tx.query_row("SELECT id FROM ai_external_room_grant WHERE room_id=?1 AND source_id=?2",params![room.0,saved.source.source_id.to_string()], |row|row.get(0)).optional()?;
            if current != request.expected_grant_id.map(|id| id.to_string()) {
                return Err(invalid("Room source grant changed; review its current publication again"));
            }
            tx.execute("DELETE FROM ai_external_room_grant WHERE room_id=?1 AND source_id=?2",params![room.0,saved.source.source_id.to_string()])?;
            tx.execute("INSERT INTO ai_external_room_grant(id,tenant_id,room_id,source_id,published_by,content_handle,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![id.to_string(),tenant.0,room.0,saved.source.source_id.to_string(),actor.0,stored_handle,saved.created_at.to_rfc3339()])?;
            tx.commit()?; Ok(())
        }).await;
        if let Err(error) = result {
            self.queue_external_content_cleanup(tenant, &handle)?;
            return Err(error);
        }
        self.ai_external_room_grant(id, actor).await
    }

    pub async fn ai_external_room_grant(
        &self,
        id: Uuid,
        actor: PrincipalId,
    ) -> Result<AiExternalRoomGrant> {
        let store = self.clone();
        let entry = sqlite_blocking(move || {
            let conn = store.conn()?;
            grant_authority(&conn, id, actor)
        })
        .await?;
        let bytes = self
            .ai_content
            .get(entry.tenant.0, &entry.handle)
            .await?
            .ok_or_else(|| invalid("Room source grant content is unavailable"))?;
        let grant: AiExternalRoomGrant = serde_json::from_slice(&bytes)?;
        if grant.id != id
            || grant.room_id != entry.room.0
            || grant.source.source_id != entry.source
            || grant.source.room_grant_id != Some(id)
            || grant.published_by != entry.publisher.0
        {
            return Err(invalid("Room source grant identity does not match"));
        }
        let source = self
            .ai_external_source(entry.source, entry.publisher)
            .await?;
        if !source_matches_proof(&source, entry.tenant, &grant.source) {
            return Err(invalid(
                "Published external source changed; review and publish it again",
            ));
        }
        let store = self.clone();
        let proof = grant.source.clone();
        sqlite_blocking(move || {
            let conn = store.conn()?;
            let fresh = grant_authority(&conn, id, actor)?;
            require_source_pin(&conn, entry.tenant, &proof)?;
            if fresh.handle != entry.handle {
                return Err(invalid("Room source grant changed during the read"));
            }
            Ok(())
        })
        .await?;
        Ok(grant)
    }

    pub async fn authorize_ai_external_room_source(
        &self,
        tenant: TenantId,
        room: RoomId,
        actor: PrincipalId,
        proof: &AiExternalSourceProof,
    ) -> Result<(AiExternalSource, Vec<String>)> {
        let grant = self
            .ai_external_room_grant(
                proof.room_grant_id.ok_or_else(|| {
                    invalid("Public external access requires an independent room source grant")
                })?,
                actor,
            )
            .await?;
        if grant.room_id != room.0 || &grant.source != proof {
            return Err(invalid(
                "Selected external source does not match this room grant",
            ));
        }
        let source = self
            .ai_external_source(proof.source_id, PrincipalId(grant.published_by))
            .await?;
        if !source_matches_proof(&source, tenant, proof) {
            return Err(invalid(
                "Published external source changed; review it again",
            ));
        }
        let store = self.clone();
        let pin = proof.clone();
        let id = grant.id;
        sqlite_blocking(move || {
            let conn = store.conn()?;
            let current = grant_authority(&conn, id, actor)?;
            require_source_pin(&conn, tenant, &pin)?;
            if current.room != room || current.tenant != tenant || current.source != pin.source_id {
                return Err(MetadataError::AiAccessDenied);
            }
            Ok(())
        })
        .await?;
        Ok((source, grant.tool_aliases))
    }

    pub async fn ai_external_room_credential(
        &self,
        tenant: TenantId,
        room: RoomId,
        actor: PrincipalId,
        proof: AiExternalSourceProof,
    ) -> Result<(AiExternalCredential, Vec<String>)> {
        let (source, aliases) = self
            .authorize_ai_external_room_source(tenant, room, actor, &proof)
            .await?;
        let mut private = proof.clone();
        private.room_grant_id = None;
        let credential = self
            .ai_external_private_credential(tenant, PrincipalId(source.owner_principal_id), private)
            .await?;
        self.authorize_ai_external_room_source(tenant, room, actor, &proof)
            .await?;
        Ok((credential, aliases))
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{approval, definition};
    use super::*;
    use crate::{
        MembershipRole, MemorySecretStore, NewOperationAudit, NewRoom, RoomKind, RoomRole,
    };
    use std::sync::Arc;

    #[tokio::test]
    async fn room_revocation_during_credential_resolution_withholds_all_source_bytes() {
        let secrets = Arc::new(super::super::tests::PausedSecrets::default());
        let store = MetadataStore::open_in_memory(secrets.clone()).unwrap();
        store.bootstrap_local("owner").unwrap();
        let owner = PrincipalId(1);
        let tenant = TenantId(1);
        let peer = store
            .create_principal("revoked-room-peer", "peer", None)
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
                    name: "Revocable context".into(),
                    kind: RoomKind::Shared,
                },
            )
            .unwrap()
            .id;
        store
            .add_room_member_authorized(
                room,
                owner,
                peer,
                RoomRole::Viewer,
                NewOperationAudit {
                    actor_principal_id: Some(owner),
                    action: "add_member".into(),
                    target: "room".into(),
                    target_id: Some(room.0),
                    status: "succeeded".into(),
                    result_code: None,
                    row_count: None,
                    error_message: None,
                    correlation_id: None,
                },
            )
            .unwrap();
        let source = store
            .create_ai_external_source(
                tenant,
                owner,
                None,
                definition(),
                Some("fixture-credential-0123456789".into()),
            )
            .await
            .unwrap();
        let active = store
            .activate_ai_external_source(source.id, owner, approval(&source))
            .await
            .unwrap();
        let proof = AiExternalSourceProof {
            source_id: active.id,
            source_revision: active.revision,
            config_sha256: active.config_sha256,
            credential_identity: active.credential_identity,
            label: active.definition.label,
            room_grant_id: None,
        };
        let grant = store
            .publish_ai_external_source(
                room,
                owner,
                PublishAiExternalSourceRequest {
                    expected_grant_id: None,
                    source: proof,
                    tool_aliases: vec!["lookup".into()],
                    publish_future_results_to_room: true,
                },
            )
            .await
            .unwrap();
        assert!(store
            .revoke_ai_external_room_grant(grant.id, peer)
            .await
            .is_err());
        let worker_store = store.clone();
        let reader = tokio::spawn(async move {
            worker_store
                .ai_external_room_credential(tenant, room, peer, grant.source)
                .await
        });
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            secrets.reached.notified(),
        )
        .await
        .unwrap();
        store
            .conn()
            .unwrap()
            .execute(
                "DELETE FROM room_member WHERE room_id=?1 AND principal_id=?2",
                params![room.0, peer.0],
            )
            .unwrap();
        secrets.resume.notify_one();
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), reader)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
    }

    #[tokio::test]
    async fn independent_room_grants_allow_no_private_vault_access_and_revoke_future_turns() {
        let store = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        store.bootstrap_local("owner").unwrap();
        let owner = PrincipalId(1);
        let tenant = TenantId(1);
        let peer = store
            .create_principal("room-peer", "peer", None)
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
                    name: "External context".into(),
                    kind: RoomKind::Shared,
                },
            )
            .unwrap()
            .id;
        store
            .add_room_member_authorized(
                room,
                owner,
                peer,
                RoomRole::Viewer,
                NewOperationAudit {
                    actor_principal_id: Some(owner),
                    action: "add_member".into(),
                    target: "room".into(),
                    target_id: Some(room.0),
                    status: "succeeded".into(),
                    result_code: None,
                    row_count: None,
                    error_message: None,
                    correlation_id: None,
                },
            )
            .unwrap();
        let source = store
            .create_ai_external_source(
                tenant,
                owner,
                None,
                definition(),
                Some("fixture-credential-0123456789".into()),
            )
            .await
            .unwrap();
        let active = store
            .activate_ai_external_source(source.id, owner, approval(&source))
            .await
            .unwrap();
        let private = AiExternalSourceProof {
            source_id: active.id,
            source_revision: active.revision,
            config_sha256: active.config_sha256,
            credential_identity: active.credential_identity,
            label: active.definition.label,
            room_grant_id: None,
        };
        assert!(store
            .authorize_ai_external_private_source(tenant, peer, &private)
            .await
            .is_err());
        let mut request = PublishAiExternalSourceRequest {
            expected_grant_id: None,
            source: private.clone(),
            tool_aliases: vec!["lookup".into()],
            publish_future_results_to_room: false,
        };
        assert!(store
            .publish_ai_external_source(room, owner, request.clone())
            .await
            .is_err());
        request.publish_future_results_to_room = true;
        assert!(store
            .publish_ai_external_source(room, peer, request.clone())
            .await
            .is_err());
        let grant = store
            .publish_ai_external_source(room, owner, request.clone())
            .await
            .unwrap();
        assert!(store
            .publish_ai_external_source(room, owner, request)
            .await
            .is_err());
        let (credential, aliases) = store
            .ai_external_room_credential(tenant, room, peer, grant.source.clone())
            .await
            .unwrap();
        assert_eq!(aliases, vec!["lookup"]);
        assert_eq!(
            credential.bearer_token.as_deref(),
            Some("fixture-credential-0123456789")
        );
        assert!(store
            .ai_external_room_credential(TenantId(2), room, peer, grant.source.clone())
            .await
            .is_err());
        let chat = store
            .create_ai_chat(
                tenant,
                Some(room),
                owner,
                sift_protocol::AiVisibility::RoomPublic,
                "Shared source".into(),
            )
            .await
            .unwrap();
        let request: sift_protocol::StartAiTurnRequest = serde_json::from_value(serde_json::json!({
            "client_request_id":Uuid::new_v4(),"desktop_id":Uuid::new_v4(),"prompt":"Read the selected source","provider":"codex","model":null,"mode":"read",
            "context":{"target":{"tenant_id":1,"room_id":room.0},"database":null,"dialect":null,"environment_label":null,"sql":null,"current_error":null,"staged_change_count":0,"external_sources":[grant.source]}
        })).unwrap();
        let lease = store
            .start_ai_run(chat.id, peer, request.clone())
            .await
            .unwrap();
        store
            .revoke_ai_external_room_grant(grant.id, owner)
            .await
            .unwrap();
        assert!(store
            .ai_external_room_credential(tenant, room, peer, grant.source)
            .await
            .is_err());
        assert!(store.start_ai_run(chat.id, peer, request).await.is_err());
        assert_eq!(
            store
                .ai_run_detail(chat.id, lease.run.id, peer)
                .await
                .unwrap()
                .context
                .external_sources
                .len(),
            1
        );
    }
}
