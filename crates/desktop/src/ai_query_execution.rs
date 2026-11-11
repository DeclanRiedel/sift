//! Captured-server ownership for human-approved local query text edits.
use crate::ai_harness::AiEventSender;
use sift_client_sdk::ScopedAiClient as Client;
use sift_protocol::*;
use sift_workspace_ui::{AiQueryEditorWork, AiQueryExecutionView, ExecutorEvent};
use uuid::Uuid;

pub(crate) struct Request {
    pub instance_id: String,
    pub chat_id: Uuid,
    pub id: Uuid,
    pub request_id: Uuid,
    pub desktop_id: Uuid,
    pub approval: Option<String>,
    pub deny: bool,
    pub reconcile: bool,
}
static OWNERS: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::OnceLock::new();
pub(crate) async fn run(client: Result<Client, String>, events: AiEventSender, request: Request) {
    let result=async {
        let _permit=OWNERS.get_or_init(||std::sync::Arc::new(tokio::sync::Semaphore::new(8))).clone().try_acquire_owned().map_err(|_|"Query edit companion is busy".to_owned())?;
        let client=client?;
        if let Some(fp)=&request.approval {
            let approval=ApproveAiQueryExecution{fingerprint:fp.clone(),desktop_id:request.desktop_id};
            if request.deny {client.deny_ai_query_execution(request.id,&approval).await.map_err(|e|e.to_string())?;}
            else if request.reconcile {client.reconcile_ai_query_execution(request.id,&approval).await.map_err(|e|e.to_string())?;}
            else {
                let reviewed=client.ai_query_execution(request.id).await.map_err(|e|e.to_string())?;
                if reviewed.summary.fingerprint!=*fp || reviewed.summary.chat_id!=request.chat_id {return Err("Query edit review changed".into());}
                if matches!(reviewed.target,AiQueryExecutionTarget::PrivateEditor{desktop_id,..} if desktop_id!=request.desktop_id) {return Err("Return to the original desktop companion for this edit".into());}
                let approved=client.approve_ai_query_execution(request.id,&approval).await.map_err(|e|e.to_string())?;
                if let Some(dispatch)=approved.dispatch {
                    if dispatch.desktop_id!=request.desktop_id {return Err("Query dispatch belongs to another desktop".into());}
                    let expires=reviewed.summary.expires_at;
                    let (work,wait)=AiQueryEditorWork::channel(request.instance_id.clone(),request.chat_id,request.request_id,request.desktop_id,reviewed);
                    let _=events.send(ExecutorEvent::AiQueryEditorWork(Box::new(work)));
                    let observation=wait.observe(expires).await;
                    // This client remains the original server even if the UI has
                    // switched instances or dropped the queued work event.
                    client.acknowledge_ai_query_editor_execution(request.id,&AcknowledgeAiQueryEditorExecution{fingerprint:fp.clone(),desktop_id:request.desktop_id,dispatch_token:dispatch.token,outcome:observation.outcome,before_sha256:observation.before_sha256,after_sha256:observation.after_sha256}).await.map_err(|_|"Original query edit acknowledgement could not be recorded; inspect its durable outcome".to_owned())?;
                }
            }
        }
        let detail=client.ai_query_execution(request.id).await.ok();
        let summary=if let Some(detail)=&detail {detail.summary.clone()}else{client.ai_query_execution_status(request.id).await.map_err(|e|e.to_string())?};
        let run=client.ai_run(request.chat_id,summary.run_id).await.ok();
        Ok(Box::new(AiQueryExecutionView{summary,detail,run}))
    }.await;
    let _ = events.send(ExecutorEvent::AiQueryExecution {
        instance_id: request.instance_id,
        chat_id: request.chat_id,
        id: request.id,
        request_id: request.request_id,
        result,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use sift_metadata::{
        CredentialMode, MemorySecretStore, MetadataStore, NewConnectionProfile, PrincipalId,
        TenantId,
    };
    use std::sync::Arc;
    #[tokio::test]
    async fn query_companion_dropped_ui_work_settles_on_the_captured_server_without_an_edit() {
        let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        metadata.bootstrap_local("query owner").unwrap();
        let actor = PrincipalId(1);
        let profile = metadata
            .upsert_connection_profile(
                TenantId(1),
                actor,
                NewConnectionProfile {
                    name: "query".into(),
                    provider_id: Engine::Postgres.provider_id(),
                    semantic_engine: Some(Engine::Postgres),
                    configuration: serde_json::json!({"database":"query"}),
                    credentials: None,
                    credential_mode: CredentialMode::Shared,
                    tags: vec![],
                },
            )
            .await
            .unwrap();
        let chat = metadata
            .create_ai_chat(
                TenantId(1),
                None,
                actor,
                AiVisibility::Private,
                "query".into(),
            )
            .await
            .unwrap();
        let digest = Sha256::digest(b"SELECT 1");
        let revision = u64::from_le_bytes(digest[..8].try_into().unwrap()) & i64::MAX as u64;
        let context:AiTurnContext=serde_json::from_value(serde_json::json!({"target":{"tenant_id":1,"profile_id":profile.id.0,"connection_id":"1:1"},"editor_item_id":7,"sql":{"text":"SELECT 1","document_revision":revision},"staged_change_count":0})).unwrap();
        let lease = metadata
            .start_ai_run(
                chat.id,
                actor,
                StartAiTurnRequest {
                    client_request_id: Uuid::new_v4(),
                    desktop_id: Uuid::new_v4(),
                    prompt: "edit".into(),
                    provider: AiProvider::Codex,
                    model: None,
                    mode: AiMode::Execute,
                    context: context.clone(),
                    attachment_previews: vec![],
                },
            )
            .await
            .unwrap();
        metadata
            .bind_ai_execute_run_source(
                lease.run.id,
                actor,
                AiExecutionSource {
                    tenant_id: 1,
                    profile_id: profile.id.0,
                    session: SessionId(1),
                    connection: ConnectionId(1),
                    generation: Uuid::new_v4(),
                    profile_digest: metadata
                        .ai_profile_source_digest(profile.id, actor)
                        .await
                        .unwrap(),
                    database_label: "query".into(),
                    production: false,
                    engine: Engine::Postgres,
                },
            )
            .await
            .unwrap();
        let proposal = metadata
            .stage_ai_query_proposal(
                lease.run.id,
                actor,
                StageAiQueryProposalRequest {
                    client_request_id: Uuid::new_v4(),
                    lease_token: lease.lease_token,
                    target: context.target.clone(),
                    base_revision: revision,
                    proposed_sql: "SELECT 2".into(),
                },
            )
            .await
            .unwrap();
        let action = metadata
            .create_ai_query_execution(
                lease.run.id,
                actor,
                RequestAiQueryExecution {
                    call_id: Uuid::new_v4(),
                    lease_token: lease.lease_token,
                    proposal_id: proposal.proposal.id,
                },
                chrono::Utc::now() + chrono::Duration::minutes(4),
                100,
            )
            .await
            .unwrap();
        let mut auth = sift_server::http::AuthState::default();
        auth.ai.enabled = true;
        auth.ai.execution_enabled = true;
        let router = sift_server::http::app(sift_server::http::AppState {
            sessions: sift_server::SessionStore::new(sift_server::DriverRegistry::new()),
            rooms: Default::default(),
            shutdown: Default::default(),
            auth,
            metadata: Some(metadata.clone()),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ordinary =
            sift_client_sdk::Client::new(format!("http://{}", listener.local_addr().unwrap()));
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let handshake = ordinary.connect().await.unwrap();
        let identity = ordinary.whoami().await.unwrap();
        let client = ordinary
            .capture_ai_caller(&sift_protocol::handshake::ExtensionCallerIdentity {
                instance_id: handshake.instance_id,
                daemon_generation: handshake.daemon_generation,
                principal_id: identity.principal.id,
                auth_session_id: identity.auth_session_id,
            })
            .await
            .unwrap();
        let (send, mut events) = tokio::sync::mpsc::unbounded_channel();
        let nonce = Uuid::new_v4();
        let owner = tokio::spawn(super::run(
            Ok(client.clone()),
            send.into(),
            Request {
                instance_id: "original-server".into(),
                chat_id: chat.id,
                id: action.id,
                request_id: nonce,
                desktop_id: lease.run.desktop_id,
                approval: Some(action.fingerprint.clone()),
                deny: false,
                reconcile: false,
            },
        ));
        let event = tokio::time::timeout(std::time::Duration::from_secs(5), events.recv())
            .await
            .unwrap()
            .unwrap();
        let ExecutorEvent::AiQueryEditorWork(work) = event else {
            panic!("expected one private companion dispatch")
        };
        assert_eq!(work.instance_id, "original-server");
        assert_eq!(work.request_id, nonce);
        drop(work); // A switched view drops its scoped event without admitting an edit.
        tokio::time::timeout(std::time::Duration::from_secs(5), owner)
            .await
            .unwrap()
            .unwrap();
        let summary = client.ai_query_execution_status(action.id).await.unwrap();
        let receipt = summary.receipt.unwrap();
        assert_eq!(summary.state, AiExecutionState::Completed);
        assert_eq!(receipt.outcome, AiQueryEditOutcome::Refused);
        assert_eq!(
            receipt.evidence,
            AiQueryEditEvidence::CompanionAcknowledgement
        );
        assert!(receipt.document_sequence.is_none());
        assert!(client
            .approve_ai_query_execution(
                action.id,
                &ApproveAiQueryExecution {
                    fingerprint: action.fingerprint,
                    desktop_id: lease.run.desktop_id
                }
            )
            .await
            .unwrap()
            .dispatch
            .is_none());
        let status = metadata
            .list_ai_query_proposals(chat.id, actor)
            .await
            .unwrap();
        assert_eq!(
            status[0].proposal.status,
            AiProposalStatus::Staged,
            "a dropped companion cannot attest an applied proposal"
        );
        server.abort();
    }
}
