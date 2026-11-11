//! Real process + HTTP/SDK ownership, revocation and one-use receipt acceptance.
use sift_metadata::{MemorySecretStore, MetadataStore, PrincipalId, TenantId};
use sift_plugin_host::{ProcessSpec, SupervisedProcess, SupervisorLimits};
use sift_protocol::*;
use sift_server::{
    automation::{GovernedToolRegistry, ToolApprovalPolicy},
    extension_dispatch::{ActionInvoker, ActionRegistration, ExtensionOperationDispatcher},
    http::{AppState, AuthState},
    registry::DriverRegistry,
    SessionStore,
};
use std::{sync::Arc, time::Duration};

struct Fixture {
    temp: tempfile::TempDir,
    metadata: MetadataStore,
    process: Arc<SupervisedProcess>,
    shutdown: sift_server::shutdown::Shutdown,
    client: sift_client_sdk::Client,
    server: tokio::task::JoinHandle<()>,
    lease: AiRunLease,
    chat: uuid::Uuid,
    tools: GovernedToolRegistry,
}
async fn fixture() -> Fixture {
    fixture_with_output(true).await
}
async fn fixture_with_output(valid_output: bool) -> Fixture {
    let classification = OperationClassification::Read;
    let timeout = Duration::from_secs(5);
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join(".sift-conformance-mode"), "action_hold").unwrap();
    let executable = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(format!(
            "sift-conformance-provider{}",
            std::env::consts::EXE_SUFFIX
        ));
    assert!(
        executable.is_file(),
        "build sift-plugin-host --bin sift-conformance-provider first"
    );
    let extension_id = ExtensionId::new("acme/conformance").unwrap();
    let contribution_id =
        ContributionId::new("acme/conformance/database_provider/fixture").unwrap();
    let process = Arc::new(
        SupervisedProcess::start(
            ProcessSpec {
                executable,
                working_directory: temp.path().into(),
                extension_id: extension_id.clone(),
                extension_version: "1.0.0".into(),
                manifest_sha256: "a".repeat(64),
                expected_contributions: vec![contribution_id.clone()],
                driver_rpc: Some(sift_extension_protocol::VersionRange {
                    minimum: 1,
                    maximum: 1,
                }),
                generation: sift_extension_protocol::WireId::from_u128(
                    uuid::Uuid::new_v4().as_u128(),
                ),
                granted_capabilities: vec![],
            },
            SupervisorLimits {
                heartbeat_interval: Duration::from_secs(30),
                cancel_grace: Duration::from_millis(500),
                ..Default::default()
            },
        )
        .await
        .unwrap(),
    );
    let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::default())).unwrap();
    metadata.bootstrap_local("extension owner").unwrap();
    let dispatcher = ExtensionOperationDispatcher::new(Arc::new(metadata.clone()));
    let invoker: Arc<dyn ActionInvoker> = process.clone();
    let operation = ExtensionOperation {
        extension_id: extension_id.clone(),
        contribution_id: contribution_id.clone(),
        action: SegmentId::new("run").unwrap(),
        classification,
        target_kind: SegmentId::new("tenant").unwrap(),
        target_id: Some("1".into()),
        sanitized_arguments: Default::default(),
    };
    dispatcher.replace([ActionRegistration {extension_id,contribution_id,action:operation.action.clone(),classification,
        required_context:vec![ContributionContext::Tenant],input_schema:serde_json::json!({"type":"object"}),
        output_schema:serde_json::json!({"type":"object","required":["ok"],"properties":{"ok":{"const":valid_output}},"additionalProperties":false}),
        timeout,max_result_bytes:1024,package_identity:None,invoker}]).unwrap();
    let registry = DriverRegistry::new();
    let info = ServerInfo {
        provider: Engine::Postgres.provider_ref("test"),
        server_version: "fixture".into(),
        current_database: "fixture".into(),
        current_user: "reader".into(),
        pool_warm_slots: None,
    };
    let driver = Arc::new(
        sift_driver_api::mock::MockDriver::builder()
            .engine(Engine::Postgres)
            .ping_ok(info)
            .build(),
    );
    registry
        .providers()
        .replace([
            Arc::new(sift_server::registry::BuiltinProviderAdapter::new(driver))
                as Arc<dyn sift_server::registry::DatabaseProvider>,
        ])
        .unwrap();
    let profile = metadata.upsert_connection_profile(TenantId(1), PrincipalId(1), sift_metadata::NewConnectionProfile {
        name:"AI extension source".into(), provider_id:Engine::Postgres.provider_id(),semantic_engine:Some(Engine::Postgres),
        configuration:serde_json::json!({"host":"fixture.invalid","database":"fixture","user":"reader","engine_specific":{}}),credentials:None,
        credential_mode:sift_metadata::CredentialMode::Shared,tags:vec![]}).await.unwrap();
    let sessions = SessionStore::new(registry);
    sessions.set_audit_store(metadata.clone());
    let tools = GovernedToolRegistry::new(
        dispatcher,
        Arc::new(metadata.clone()),
        ToolApprovalPolicy::default(),
    );
    tools.replace([GovernedToolDescriptor {id:"fixture".into(),title:"Fixture".into(),description:"Reviewed fixture action".into(),operation,
        input_schema:serde_json::json!({"type":"object"}),output_schema:serde_json::json!({"type":"object","required":["ok"],"properties":{"ok":{"const":valid_output}},"additionalProperties":false}),
        required_context:vec![ContributionContext::Tenant],mcp_exposable:true,schedulable:false,interactive:false}]).unwrap();
    sessions.set_tool_registry(tools.clone());
    let mut auth = AuthState {
        bearer_token: Some("fixture-owner".into()),
        loopback_bypass: false,
        ..Default::default()
    };
    auth.ai.enabled = true;
    auth.ai.execution_enabled = true;
    let shutdown = sift_server::shutdown::Shutdown::default();
    let router = sift_server::http::app(AppState {
        sessions,
        rooms: Default::default(),
        shutdown: shutdown.clone(),
        metadata: Some(metadata.clone()),
        auth,
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = sift_client_sdk::Client::new(format!("http://{}", listener.local_addr().unwrap()))
        .with_bearer_token("fixture-owner");
    let server = tokio::spawn({
        let router = router.clone();
        async move { axum::serve(listener, router).await.unwrap() }
    });
    let session = client.open_session(None).await.unwrap().id;
    let connection = client
        .open_connection_from_profile(
            session,
            sift_api_types::OpenConnectionFromProfileRequest {
                tenant_id: 1,
                profile_id: profile.id.0,
            },
        )
        .await
        .unwrap()
        .id;
    let chat = client
        .create_ai_chat(&CreateAiChatRequest {
            tenant_id: 1,
            room_id: None,
            title: "Extension review".into(),
        })
        .await
        .unwrap();
    let context=serde_json::from_value(serde_json::json!({"target":{"tenant_id":1,"profile_id":profile.id.0,"connection_id":format!("{}:{}",session.0,connection.0)},"staged_change_count":0,"attachments":[]})).unwrap();
    let lease = client
        .start_ai_turn(
            chat.id,
            &StartAiTurnRequest {
                client_request_id: uuid::Uuid::new_v4(),
                desktop_id: uuid::Uuid::new_v4(),
                prompt: "Review action".into(),
                provider: AiProvider::Codex,
                model: None,
                mode: AiMode::Execute,
                context,
                attachment_previews: vec![],
            },
        )
        .await
        .unwrap();

    Fixture {
        temp,
        metadata,
        process,
        shutdown,
        client,
        server,
        lease,
        chat: chat.id,
        tools,
    }
}
async fn until(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
async fn stop(f: Fixture) {
    f.shutdown.begin_drain();
    f.process
        .shutdown("extension acceptance complete")
        .await
        .unwrap();
    f.server.abort();
}

async fn review(f: &Fixture) -> AiExtensionExecutionSummary {
    f.client
        .request_ai_extension_execution(
            f.lease.run.id,
            &RequestAiExtensionExecution {
                call_id: uuid::Uuid::new_v4(),
                lease_token: f.lease.lease_token,
                tool_id: "fixture".into(),
                arguments: serde_json::json!({"private":"review input"}),
            },
        )
        .await
        .unwrap()
}
fn approval(review: &AiExtensionExecutionSummary) -> ApproveAiExtensionExecution {
    ApproveAiExtensionExecution {
        fingerprint: review.fingerprint.clone(),
        acknowledged_classification: OperationClassification::Read,
        production_confirmation: None,
    }
}
#[tokio::test]
#[ignore = "requires built conformance executable"]
async fn human_review_and_replay_preserve_the_single_owner_result() {
    let f = fixture().await;
    let reviewed = review(&f).await;
    assert_eq!(reviewed.state, AiExecutionState::AwaitingApproval);
    assert!(!f.temp.path().join(".action.entered").exists());
    assert!(f
        .metadata
        .extension_executions(PrincipalId(1))
        .unwrap()
        .is_empty());
    let mut wrong = approval(&reviewed);
    wrong.acknowledged_classification = OperationClassification::Write;
    assert!(f
        .client
        .approve_ai_extension_execution(reviewed.id, &wrong)
        .await
        .is_err());
    let handshake = f.client.connect().await.unwrap();
    let scoped = f
        .client
        .capture_extension_caller(&sift_protocol::handshake::ExtensionCallerIdentity {
            instance_id: handshake.instance_id,
            daemon_generation: handshake.daemon_generation,
            principal_id: 1,
            auth_session_id: None,
        })
        .await
        .unwrap();
    let request = approval(&reviewed);
    let (a, b) = tokio::join!(
        scoped.approve_ai_extension_execution(reviewed.id, &request),
        scoped.approve_ai_extension_execution(reviewed.id, &request)
    );
    assert_eq!(a.unwrap().execution_id, b.unwrap().execution_id);
    until(|| f.temp.path().join(".action.entered").exists()).await;
    assert_eq!(
        std::fs::read_to_string(f.temp.path().join(".action.calls")).unwrap(),
        "invoke\n"
    );
    std::fs::write(f.temp.path().join(".action.release"), "").unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = f
                .client
                .ai_extension_execution_status(reviewed.id)
                .await
                .unwrap();
            if status.result_availability == AiExtensionResultAvailability::Available {
                assert_eq!(status.state, AiExecutionState::Completed);
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    for _ in 0..3 {
        scoped
            .approve_ai_extension_execution(reviewed.id, &request)
            .await
            .unwrap();
    }
    let detail = f.client.ai_extension_execution(reviewed.id).await.unwrap();
    assert_eq!(detail.result, Some(serde_json::json!({"ok":true})));
    f.tools.replace([]).unwrap();
    let revoked = f.client.ai_extension_execution(reviewed.id).await.unwrap();
    assert_eq!(revoked.summary.state, AiExecutionState::Completed);
    assert_eq!(
        revoked.summary.result_availability,
        AiExtensionResultAvailability::Withheld
    );
    assert!(revoked.result.is_none());

    assert_eq!(
        f.client
            .ai_extension_executions(f.chat)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        f.metadata
            .extension_executions(PrincipalId(1))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        std::fs::read_to_string(f.temp.path().join(".action.calls")).unwrap(),
        "invoke\n"
    );
    stop(f).await;
}
#[tokio::test]
#[ignore = "requires built conformance executable"]
async fn stop_retains_late_rpc_evidence_and_requires_exact_acknowledgement() {
    let f = fixture().await;
    let reviewed = review(&f).await;
    f.client
        .approve_ai_extension_execution(reviewed.id, &approval(&reviewed))
        .await
        .unwrap();
    until(|| f.temp.path().join(".action.entered").exists()).await;
    f.client
        .finish_ai_run(
            f.lease.run.id,
            &FinishAiRunRequest {
                lease_token: f.lease.lease_token,
                status: AiRunStatus::Canceled,
            },
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    std::fs::write(f.temp.path().join(".action.release"), "").unwrap();
    let settled = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = f
                .client
                .ai_extension_execution_status(reviewed.id)
                .await
                .unwrap();
            if status.state == AiExecutionState::OutcomeUnknown {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let receipt = settled.receipt.unwrap();
    assert_eq!(
        receipt.result_code,
        Some(ExtensionExecutionCode::OwnerInterrupted)
    );
    assert_eq!(receipt.state, ExtensionExecutionState::Acknowledged);
    assert!(receipt.late_response);
    assert!(f
        .client
        .ai_extension_execution(reviewed.id)
        .await
        .unwrap()
        .result
        .is_none());
    let mut ack = AcknowledgeAiExtensionExecution {
        fingerprint: reviewed.fingerprint,
        execution_id: uuid::Uuid::new_v4(),
    };
    assert!(f
        .client
        .acknowledge_ai_extension_execution(reviewed.id, &ack)
        .await
        .is_err());
    ack.execution_id = receipt.id;
    assert_eq!(
        f.client
            .acknowledge_ai_extension_execution(reviewed.id, &ack)
            .await
            .unwrap()
            .state,
        AiExecutionState::Reconciled
    );
    assert_eq!(
        std::fs::read_to_string(f.temp.path().join(".action.calls")).unwrap(),
        "invoke\n"
    );
    stop(f).await;
}

#[tokio::test]
#[ignore = "requires built conformance executable"]
async fn changed_registration_refuses_frozen_review_before_rpc_claim() {
    let f = fixture().await;
    let reviewed = review(&f).await;
    let detail = f.client.ai_extension_execution(reviewed.id).await.unwrap();
    let mut changed = detail.review.tool;
    changed.description = "Registration replaced after review".into();
    f.tools.replace([changed]).unwrap();
    assert!(f
        .client
        .approve_ai_extension_execution(reviewed.id, &approval(&reviewed))
        .await
        .is_err());
    assert!(f
        .metadata
        .extension_executions(PrincipalId(1))
        .unwrap()
        .is_empty());
    assert!(!f.temp.path().join(".action.entered").exists());
    assert_eq!(
        f.client
            .ai_extension_execution_status(reviewed.id)
            .await
            .unwrap()
            .state,
        AiExecutionState::AwaitingApproval
    );
    let denied = f
        .client
        .deny_ai_extension_execution(
            reviewed.id,
            &DenyAiExtensionExecution {
                fingerprint: reviewed.fingerprint,
            },
        )
        .await
        .unwrap();
    assert_eq!(denied.state, AiExecutionState::Denied);
    stop(f).await;
}

#[tokio::test]
#[ignore = "requires built conformance executable"]
async fn changed_source_withholds_output_without_erasing_rpc_acknowledgement() {
    let f = fixture().await;
    let reviewed = review(&f).await;
    f.client
        .approve_ai_extension_execution(reviewed.id, &approval(&reviewed))
        .await
        .unwrap();
    until(|| f.temp.path().join(".action.entered").exists()).await;
    f.metadata.upsert_connection_profile(TenantId(1), PrincipalId(1), sift_metadata::NewConnectionProfile {
        name: "AI extension source".into(), provider_id: Engine::Postgres.provider_id(), semantic_engine: Some(Engine::Postgres),
        configuration: serde_json::json!({"host":"replacement.invalid","database":"fixture","user":"reader","engine_specific":{}}),
        credentials: None, credential_mode: sift_metadata::CredentialMode::Shared, tags: vec![],
    }).await.unwrap();
    std::fs::write(f.temp.path().join(".action.release"), "").unwrap();
    let summary = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = f
                .client
                .ai_extension_execution_status(reviewed.id)
                .await
                .unwrap();
            if status.result_availability == AiExtensionResultAvailability::Withheld {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(summary.state, AiExecutionState::Completed);
    let receipt = summary.receipt.unwrap();
    assert_eq!(receipt.state, ExtensionExecutionState::Acknowledged);
    assert_eq!(
        receipt.result_code,
        Some(ExtensionExecutionCode::DeliveryDenied)
    );
    assert!(f.client.ai_extension_execution(reviewed.id).await.is_err());
    assert_eq!(
        std::fs::read_to_string(f.temp.path().join(".action.calls")).unwrap(),
        "invoke\n"
    );
    stop(f).await;
}

#[tokio::test]
#[ignore = "requires built conformance executable"]
async fn stop_remains_the_primary_cause_when_a_late_body_is_invalid() {
    let f = fixture_with_output(false).await;
    let reviewed = review(&f).await;
    f.client
        .approve_ai_extension_execution(reviewed.id, &approval(&reviewed))
        .await
        .unwrap();
    until(|| f.temp.path().join(".action.entered").exists()).await;
    f.client
        .finish_ai_run(
            f.lease.run.id,
            &FinishAiRunRequest {
                lease_token: f.lease.lease_token,
                status: AiRunStatus::Canceled,
            },
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    std::fs::write(f.temp.path().join(".action.release"), "").unwrap();
    let summary = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let summary = f
                .client
                .ai_extension_execution_status(reviewed.id)
                .await
                .unwrap();
            if summary.state == AiExecutionState::OutcomeUnknown {
                break summary;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let receipt = summary.receipt.unwrap();
    assert_eq!(receipt.state, ExtensionExecutionState::OutcomeUnknown);
    assert_eq!(
        receipt.result_code,
        Some(ExtensionExecutionCode::OwnerInterrupted)
    );
    assert!(receipt.late_response);
    assert!(receipt.response_kind.is_none());
    assert!(f
        .client
        .ai_extension_execution(reviewed.id)
        .await
        .unwrap()
        .result
        .is_none());
    assert_eq!(
        std::fs::read_to_string(f.temp.path().join(".action.calls")).unwrap(),
        "invoke\n"
    );
    stop(f).await;
}

#[tokio::test]
#[ignore = "requires built conformance executable"]
async fn removing_governed_tool_after_dispatch_withholds_body_and_keeps_rpc_facts() {
    let f = fixture().await;
    let reviewed = review(&f).await;
    f.client
        .approve_ai_extension_execution(reviewed.id, &approval(&reviewed))
        .await
        .unwrap();
    until(|| f.temp.path().join(".action.entered").exists()).await;
    f.tools.replace([]).unwrap();
    std::fs::write(f.temp.path().join(".action.release"), "").unwrap();
    let summary = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let summary = f
                .client
                .ai_extension_execution_status(reviewed.id)
                .await
                .unwrap();
            if summary.result_availability == AiExtensionResultAvailability::Withheld {
                break summary;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(summary.state, AiExecutionState::Completed);
    let receipt = summary.receipt.unwrap();
    assert_eq!(receipt.state, ExtensionExecutionState::Acknowledged);
    assert_eq!(
        receipt.result_code,
        Some(ExtensionExecutionCode::DeliveryDenied)
    );
    assert!(f
        .client
        .ai_extension_execution(reviewed.id)
        .await
        .unwrap()
        .result
        .is_none());
    assert_eq!(
        std::fs::read_to_string(f.temp.path().join(".action.calls")).unwrap(),
        "invoke\n"
    );
    stop(f).await;
}
