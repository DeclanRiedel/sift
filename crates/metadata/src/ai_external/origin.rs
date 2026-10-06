//! Local proposals retain reviewed intent, without remote execution authority.
use super::publication::ExternalSourceAuthorization;
use super::*;
use sift_protocol::AiExternalProposalOrigin;

#[derive(Clone)]
pub struct AiExternalProposalIntent {
    pub source_id: Uuid,
    pub alias: String,
}
impl MetadataStore {
    pub(crate) async fn ai_external_proposal_origin(
        &self,
        run_id: Uuid,
        actor: PrincipalId,
        lease: Uuid,
        intent: Option<AiExternalProposalIntent>,
        policy: AiExternalToolPolicy,
    ) -> Result<Option<(AiExternalProposalOrigin, ExternalSourceAuthorization)>> {
        let Some(intent) = intent else {
            return Ok(None);
        };
        let (proof, source) = self
            .ai_external_run_source(run_id, actor, lease, intent.source_id)
            .await?;
        let tool = source
            .definition
            .tools
            .iter()
            .find(|tool| tool.alias == intent.alias && tool.policy == policy)
            .ok_or_else(|| {
                invalid("Reviewed source intent does not authorize this local draft kind")
            })?;
        if !matches!(
            policy,
            AiExternalToolPolicy::LocalQueryDraft
                | AiExternalToolPolicy::LocalRowDraft
                | AiExternalToolPolicy::LocalMigrationDraft
        ) {
            return Err(MetadataError::AiAccessDenied);
        }
        let origin = AiExternalProposalOrigin {
            source: proof.clone(),
            tool_alias: tool.alias.clone(),
            tool_name: tool.name.clone(),
            schema_sha256: tool.schema_sha256.clone(),
            policy,
        };
        let run = self.ai_tool_run(run_id, actor, lease).await?;
        let chat = self.get_ai_chat(run.chat_id, actor).await?;
        let mut context = run.context;
        context.external_sources = vec![proof];
        let guard = self
            .authorize_ai_external_turn_sources(&chat, actor, &context)
            .await?
            .pop()
            .ok_or(MetadataError::AiAccessDenied)?;
        Ok(Some((origin, guard)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CredentialMode, MemorySecretStore, NewConnectionProfile};
    use sift_protocol::{AiDatabaseDraft, AiMode, AiProvider, AiVisibility, StartAiTurnRequest};
    use std::sync::Arc;

    #[tokio::test]
    async fn local_row_intents_preserve_database_authority_quota_and_provenance() {
        let store = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        store.bootstrap_local("owner").unwrap();
        let actor = PrincipalId(1);
        let tenant = TenantId(1);
        let profile = store
            .upsert_connection_profile(
                tenant,
                actor,
                NewConnectionProfile {
                    name: "Local target".into(),
                    provider_id: sift_protocol::Engine::Postgres.provider_id(),
                    configuration: serde_json::json!({"database":"test"}),
                    semantic_engine: Some(sift_protocol::Engine::Postgres),
                    credentials: None,
                    credential_mode: CredentialMode::Shared,
                    tags: vec![],
                },
            )
            .await
            .unwrap();
        let source_digest = store
            .ai_profile_source_digest(profile.id, actor)
            .await
            .unwrap();
        let draft_source = store
            .create_ai_external_source(tenant, actor, None, super::super::tests::definition(), None)
            .await
            .unwrap();
        let mut review = super::super::tests::approval(&draft_source);
        review.tools[0].policy = AiExternalToolPolicy::LocalRowDraft;
        let source = store
            .activate_ai_external_source(draft_source.id, actor, review)
            .await
            .unwrap();
        let proof = AiExternalSourceProof {
            source_id: source.id,
            source_revision: source.revision,
            config_sha256: source.config_sha256.clone(),
            credential_identity: source.credential_identity,
            label: source.definition.label.clone(),
            room_grant_id: None,
        };
        let chat = store
            .create_ai_chat(
                tenant,
                None,
                actor,
                AiVisibility::Private,
                "Local intents".into(),
            )
            .await
            .unwrap();
        let context = serde_json::from_value(serde_json::json!({
            "target":{"tenant_id":1,"profile_id":profile.id.0},"staged_change_count":0,"external_sources":[proof]
        })).unwrap();
        let lease = store
            .start_ai_run(
                chat.id,
                actor,
                StartAiTurnRequest {
                    attachment_previews: vec![],
                    client_request_id: Uuid::new_v4(),
                    desktop_id: Uuid::new_v4(),
                    prompt: "Propose a row".into(),
                    provider: AiProvider::Codex,
                    model: None,
                    mode: AiMode::Propose,
                    context,
                },
            )
            .await
            .unwrap();
        let intent = AiExternalProposalIntent {
            source_id: source.id,
            alias: source.definition.tools[0].alias.clone(),
        };
        let request = sift_protocol::StageAiDatabaseProposalRequest {
            client_request_id: Uuid::new_v4(),
            lease_token: lease.lease_token,
            draft: AiDatabaseDraft::RowEditSet {
                edit_set: sift_protocol::EditSet {
                    table: sift_protocol::ObjectPath::new("items"),
                    edits: vec![sift_protocol::RowEdit::Insert {
                        values: vec![sift_protocol::CellEdit {
                            column: "name".into(),
                            value: sift_protocol::Value::Text("reviewed".into()),
                        }],
                    }],
                },
                expected_catalog_revision: sift_protocol::CatalogRevision(1),
            },
        };
        assert!(store
            .stage_ai_database_proposal_with_intent(
                lease.run.id,
                actor,
                request.clone(),
                "0".repeat(64),
                "dbid:test".into(),
                None,
                1,
                Some(intent.clone())
            )
            .await
            .is_err());
        assert!(store
            .ai_external_proposal_origin(
                lease.run.id,
                actor,
                lease.lease_token,
                Some(intent.clone()),
                AiExternalToolPolicy::LocalMigrationDraft
            )
            .await
            .is_err());
        let detail = store
            .stage_ai_database_proposal_with_intent(
                lease.run.id,
                actor,
                request.clone(),
                source_digest.clone(),
                "dbid:test".into(),
                None,
                1,
                Some(intent.clone()),
            )
            .await
            .unwrap();
        let origin = detail.external_origin.clone().unwrap();
        assert_eq!(origin.policy, AiExternalToolPolicy::LocalRowDraft);
        assert_eq!(origin.source.source_id, source.id);
        let retry = store
            .stage_ai_database_proposal_with_intent(
                lease.run.id,
                actor,
                request.clone(),
                source_digest.clone(),
                "dbid:test".into(),
                None,
                1,
                Some(intent.clone()),
            )
            .await
            .unwrap();
        assert_eq!(retry.proposal.id, detail.proposal.id);
        assert_eq!(retry.external_origin, Some(origin.clone()));
        assert!(store
            .stage_ai_database_proposal_with_limit(
                lease.run.id,
                actor,
                request.clone(),
                source_digest.clone(),
                "dbid:test".into(),
                None,
                1
            )
            .await
            .is_err());
        let mut next = request;
        next.client_request_id = Uuid::new_v4();
        assert!(store
            .stage_ai_database_proposal_with_intent(
                lease.run.id,
                actor,
                next.clone(),
                source_digest.clone(),
                "dbid:test".into(),
                None,
                1,
                Some(intent.clone())
            )
            .await
            .is_err());
        store
            .disable_ai_external_source(source.id, actor, source.revision)
            .await
            .unwrap();
        assert!(store
            .stage_ai_database_proposal_with_intent(
                lease.run.id,
                actor,
                next,
                source_digest,
                "dbid:test".into(),
                None,
                20,
                Some(intent)
            )
            .await
            .is_err());
        let accepted = store
            .ai_database_proposal(detail.proposal.id, actor)
            .await
            .unwrap();
        assert_eq!(accepted.external_origin, Some(origin));
        assert_eq!(
            accepted.proposal.status,
            sift_protocol::AiProposalStatus::Staged
        );
        assert!(accepted.apply_state.is_none());
    }
}
