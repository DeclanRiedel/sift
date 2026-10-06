//! Explicit rediscovery invalidates approvals; stored credentials stay endpoint-bound.
use super::*;
use sift_protocol::AiExternalCredentialUpdate;

impl MetadataStore {
    pub async fn ai_external_refresh_credential(
        &self,
        id: Uuid,
        actor: PrincipalId,
        expected_revision: u64,
        endpoint: &str,
        update: &AiExternalCredentialUpdate,
    ) -> Result<AiExternalCredential> {
        let source = self.ai_external_source(id, actor).await?;
        let store = self.clone();
        let entry = sqlite_blocking(move || {
            let conn = store.conn()?;
            let entry = record(&conn, id)?;
            editable(&conn, &entry, actor)?;
            if entry.revision != expected_revision {
                return Err(invalid("Source revision changed; refresh its review"));
            }
            Ok(entry)
        })
        .await?;
        if source.revision != entry.revision || source.config_sha256 != entry.config_sha256 {
            return Err(invalid("Source revision changed; refresh its review"));
        }
        let bearer_token = match update {
            AiExternalCredentialUpdate::Keep => {
                if endpoint != source.definition.endpoint {
                    return Err(invalid("Keeping credentials requires the exact existing endpoint; replace or clear them to change it"));
                }
                if let Some(handle) = &entry.credential_handle {
                    Some(
                        String::from_utf8(
                            self.secrets
                                .get(CREDENTIAL_NAMESPACE, handle)
                                .await?
                                .ok_or_else(|| invalid("Source credential is unavailable"))?,
                        )
                        .map_err(|_| invalid("Source credential is invalid"))?,
                    )
                } else {
                    None
                }
            }
            AiExternalCredentialUpdate::Replace { bearer_token } => Some(bearer_token.clone()),
            AiExternalCredentialUpdate::Clear => None,
        };
        let fresh = self.ai_external_source(id, actor).await?;
        if fresh.revision != source.revision
            || fresh.config_sha256 != source.config_sha256
            || fresh.credential_identity != source.credential_identity
        {
            return Err(invalid("Source changed during credential resolution"));
        }
        let store = self.clone();
        sqlite_blocking(move || {
            let conn = store.conn()?;
            let entry = record(&conn, id)?;
            editable(&conn, &entry, actor)?;
            if entry.revision != expected_revision {
                return Err(invalid("Source changed during credential resolution"));
            }
            Ok(())
        })
        .await?;
        Ok(AiExternalCredential {
            source: fresh,
            bearer_token,
        })
    }

    pub async fn refresh_ai_external_source(
        &self,
        id: Uuid,
        actor: PrincipalId,
        expected_revision: u64,
        definition: AiExternalSourceDefinition,
        update: AiExternalCredentialUpdate,
    ) -> Result<AiExternalSource> {
        if definition
            .tools
            .iter()
            .any(|tool| tool.policy != AiExternalToolPolicy::Unavailable)
        {
            return Err(invalid("Rediscovery cannot approve tools"));
        }
        let credential = self
            .ai_external_refresh_credential(
                id,
                actor,
                expected_revision,
                &definition.endpoint,
                &update,
            )
            .await?;
        let bytes = definition_bytes(&definition)?;
        if let Some(token) = &credential.bearer_token {
            validate_credential_metadata(token, &bytes)?;
        }
        let store = self.clone();
        let mut entry = sqlite_blocking(move || {
            let conn = store.conn()?;
            let entry = record(&conn, id)?;
            editable(&conn, &entry, actor)?;
            if entry.revision != expected_revision {
                return Err(invalid("Source changed during rediscovery"));
            }
            Ok(entry)
        })
        .await?;
        let replace = !matches!(update, AiExternalCredentialUpdate::Keep);
        if replace {
            entry.credential_identity = Uuid::new_v4();
            entry.credential_handle = credential
                .bearer_token
                .as_ref()
                .map(|_| Uuid::new_v4().to_string());
            if let (Some(handle), Some(token)) =
                (&entry.credential_handle, &credential.bearer_token)
            {
                self.secrets
                    .put(CREDENTIAL_NAMESPACE, handle, token.as_bytes())
                    .await?;
            }
        }
        entry.config_sha256 = config_digest(&entry, &definition)?;
        let handle = match self.ai_content.put(entry.tenant.0, &bytes).await {
            Ok(handle) => handle,
            Err(error) => {
                if replace {
                    if let Some(handle) = &entry.credential_handle {
                        self.queue_external_credential_cleanup(handle, "external_refresh_failed")?;
                    }
                }
                return Err(error);
            }
        };
        let written = handle.clone();
        let tenant = entry.tenant;
        let new_credential = entry.credential_handle.clone();
        let store = self.clone();
        let result=sqlite_blocking(move|| {
            let mut conn=store.conn()?;let tx=conn.transaction()?;let current=record(&tx,id)?;editable(&tx,&current,actor)?;
            if current.revision!=expected_revision {return Err(invalid("Source changed during rediscovery"));}
            tx.execute("UPDATE ai_external_source SET revision=revision+1,state='draft',config_handle=?2,config_sha256=?3,credential_identity=?4,credential_handle=?5,credential_scope_reviewed=0,updated_at=?6 WHERE id=?1",
                params![id.to_string(),written,entry.config_sha256,entry.credential_identity.to_string(),entry.credential_handle,Utc::now().to_rfc3339()])?;
            tx.commit()?;Ok(())
        }).await;
        if result.is_err() {
            self.queue_external_content_cleanup(tenant, &handle)?;
            if replace {
                if let Some(handle) = new_credential {
                    self.queue_external_credential_cleanup(&handle, "external_refresh_failed")?;
                }
            }
        }
        result?;
        self.ai_external_source(id, actor).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MemorySecretStore, SecretStore};
    use std::sync::Arc;

    #[tokio::test]
    async fn credential_refresh_is_endpoint_bound_resets_approvals_and_cleans_replaced_handles() {
        let secrets = Arc::new(MemorySecretStore::new());
        let store = MetadataStore::open_in_memory(secrets.clone()).unwrap();
        store.bootstrap_local("Refresh owner").unwrap();
        let (owner, tenant) = (PrincipalId(1), TenantId(1));
        let token = "refresh-fixture-credential-012345";
        let draft = store
            .create_ai_external_source(
                tenant,
                owner,
                None,
                super::super::tests::definition(),
                Some(token.into()),
            )
            .await
            .unwrap();
        let active = store
            .activate_ai_external_source(draft.id, owner, super::super::tests::approval(&draft))
            .await
            .unwrap();
        let old_handle = record(&store.conn().unwrap(), active.id)
            .unwrap()
            .credential_handle
            .unwrap();
        let keep = AiExternalCredentialUpdate::Keep;
        assert!(store
            .ai_external_refresh_credential(
                active.id,
                owner,
                active.revision,
                "https://different.invalid/mcp",
                &keep
            )
            .await
            .is_err());
        let retained = store
            .ai_external_refresh_credential(
                active.id,
                owner,
                active.revision,
                &active.definition.endpoint,
                &keep,
            )
            .await
            .unwrap();
        assert_eq!(retained.bearer_token.as_deref(), Some(token));
        let refreshed = store
            .refresh_ai_external_source(
                active.id,
                owner,
                active.revision,
                super::super::tests::definition(),
                keep,
            )
            .await
            .unwrap();
        assert_eq!(refreshed.state, AiExternalSourceState::Draft);
        assert!(!refreshed.credential_scope_reviewed);
        assert_eq!(refreshed.credential_identity, active.credential_identity);
        assert_eq!(refreshed.revision, active.revision + 1);
        assert!(refreshed
            .definition
            .tools
            .iter()
            .all(|tool| tool.policy == AiExternalToolPolicy::Unavailable));
        assert!(store
            .refresh_ai_external_source(
                active.id,
                owner,
                active.revision,
                super::super::tests::definition(),
                AiExternalCredentialUpdate::Clear
            )
            .await
            .is_err());
        let replacement = "replacement-fixture-credential-012345";
        let request = AiExternalCredentialUpdate::Replace {
            bearer_token: replacement.into(),
        };
        assert!(!format!("{request:?}").contains(replacement));
        let rotated = store
            .refresh_ai_external_source(
                active.id,
                owner,
                refreshed.revision,
                super::super::tests::definition(),
                request,
            )
            .await
            .unwrap();
        assert_ne!(rotated.credential_identity, active.credential_identity);
        let current = store
            .ai_external_refresh_credential(
                rotated.id,
                owner,
                rotated.revision,
                &rotated.definition.endpoint,
                &AiExternalCredentialUpdate::Keep,
            )
            .await
            .unwrap();
        assert_eq!(current.bearer_token.as_deref(), Some(replacement));
        assert_eq!(
            store.process_vault_secret_cleanup(100).await.unwrap(),
            (1, 0)
        );
        assert!(secrets
            .get(CREDENTIAL_NAMESPACE, &old_handle)
            .await
            .unwrap()
            .is_none());
        let mut echo = super::super::tests::definition();
        echo.label = replacement.into();
        assert!(store
            .refresh_ai_external_source(
                rotated.id,
                owner,
                rotated.revision,
                echo,
                AiExternalCredentialUpdate::Keep
            )
            .await
            .is_err());
        let mut anonymous = super::super::tests::definition();
        anonymous.endpoint = "https://new-explicit-fixture.invalid/mcp".into();
        let cleared = store
            .refresh_ai_external_source(
                rotated.id,
                owner,
                rotated.revision,
                anonymous,
                AiExternalCredentialUpdate::Clear,
            )
            .await
            .unwrap();
        assert!(!cleared.credential_configured);
        assert_ne!(cleared.credential_identity, rotated.credential_identity);
        assert_eq!(
            store.process_vault_secret_cleanup(100).await.unwrap(),
            (1, 0)
        );
    }
    #[tokio::test]
    async fn stale_room_grant_headers_support_explicit_republication_after_refresh() {
        use sift_protocol::{AiExternalSourceProof, PublishAiExternalSourceRequest};
        let store = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        store.bootstrap_local("Grant review owner").unwrap();
        let (owner, tenant) = (PrincipalId(1), TenantId(1));
        let draft = store
            .create_ai_external_source(tenant, owner, None, super::super::tests::definition(), None)
            .await
            .unwrap();
        let active = store
            .activate_ai_external_source(draft.id, owner, super::super::tests::approval(&draft))
            .await
            .unwrap();
        let room = store
            .create_room(
                tenant,
                owner,
                crate::NewRoom {
                    name: "Review room".into(),
                    kind: crate::RoomKind::Shared,
                },
            )
            .unwrap()
            .id;
        let proof = |source: &AiExternalSource| AiExternalSourceProof {
            source_id: source.id,
            source_revision: source.revision,
            config_sha256: source.config_sha256.clone(),
            credential_identity: source.credential_identity,
            label: source.definition.label.clone(),
            room_grant_id: None,
        };
        let grant = store
            .publish_ai_external_source(
                room,
                owner,
                PublishAiExternalSourceRequest {
                    expected_grant_id: None,
                    source: proof(&active),
                    tool_aliases: vec!["lookup".into()],
                    publish_future_results_to_room: true,
                },
            )
            .await
            .unwrap();
        let refreshed = store
            .refresh_ai_external_source(
                active.id,
                owner,
                active.revision,
                super::super::tests::definition(),
                AiExternalCredentialUpdate::Keep,
            )
            .await
            .unwrap();
        assert!(store
            .list_ai_external_room_sources(room, owner)
            .await
            .unwrap()
            .is_empty());
        let review = store
            .list_ai_external_room_grants_for_review(room, owner)
            .await
            .unwrap();
        assert_eq!(review.len(), 1);
        assert_eq!(review[0].id, grant.id);
        let reviewed = store
            .activate_ai_external_source(
                refreshed.id,
                owner,
                super::super::tests::approval(&refreshed),
            )
            .await
            .unwrap();
        let published = store
            .publish_ai_external_source(
                room,
                owner,
                PublishAiExternalSourceRequest {
                    expected_grant_id: Some(review[0].id),
                    source: proof(&reviewed),
                    tool_aliases: vec!["lookup".into()],
                    publish_future_results_to_room: true,
                },
            )
            .await
            .unwrap();
        assert_ne!(published.id, grant.id);
        assert!(store
            .ai_external_room_credential(tenant, room, owner, grant.source)
            .await
            .is_err());
        assert!(store
            .ai_external_room_credential(tenant, room, owner, published.source)
            .await
            .is_ok());
    }
}
