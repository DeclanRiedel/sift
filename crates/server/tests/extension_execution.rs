//! Real process + HTTP/SDK ownership, revocation and one-use receipt acceptance.
use sift_metadata::{MembershipRole, MemorySecretStore, MetadataStore, PrincipalId, TenantId};
use sift_plugin_host::{ProcessSpec, RequestObservation, SupervisedProcess, SupervisorLimits};
use sift_protocol::*;
use sift_server::{
    automation::{GovernedToolRegistry, ToolApprovalPolicy},
    extension_dispatch::{
        ActionAdmission, ActionInvoker, ActionRegistration, ExtensionOperationDispatcher,
    },
    http::{AppState, AuthState},
    registry::DriverRegistry,
    SessionStore,
};
use std::{sync::Arc, time::Duration};
use tower::ServiceExt;

struct ReadinessGate {
    process: Arc<SupervisedProcess>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}
#[async_trait::async_trait]
impl ActionInvoker for ReadinessGate {
    async fn request(
        &self,
        _tenant: Option<i64>,
        request: sift_extension_protocol::Request,
    ) -> Result<sift_extension_protocol::Response, sift_plugin_host::SupervisorError> {
        self.process.request(request).await
    }
    async fn request_observed(
        &self,
        _tenant: Option<i64>,
        request: sift_extension_protocol::Request,
        admit: ActionAdmission,
    ) -> RequestObservation {
        self.entered.notify_one();
        self.release.notified().await;
        self.process.request_observed(request, admit).await
    }
}
struct Fixture {
    temp: tempfile::TempDir,
    metadata: MetadataStore,
    process: Arc<SupervisedProcess>,
    router: axum::Router,
    shutdown: sift_server::shutdown::Shutdown,
    client: sift_client_sdk::Client,
    server: tokio::task::JoinHandle<()>,
    request: InvokeExtensionRequest,
}
async fn fixture(
    classification: OperationClassification,
    timeout: Duration,
    gate: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
) -> Fixture {
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
                cancel_grace: Duration::from_millis(50),
                ..Default::default()
            },
        )
        .await
        .unwrap(),
    );
    let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::default())).unwrap();
    metadata.bootstrap_local("extension owner").unwrap();
    let dispatcher = ExtensionOperationDispatcher::new(Arc::new(metadata.clone()));
    let invoker: Arc<dyn ActionInvoker> = match gate {
        Some((entered, release)) => Arc::new(ReadinessGate {
            process: process.clone(),
            entered,
            release,
        }),
        None => process.clone(),
    };
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
        output_schema:serde_json::json!({"type":"object","required":["ok"],"properties":{"ok":{"const":true}},"additionalProperties":false}),
        timeout,max_result_bytes:1024,package_identity:None,invoker}]).unwrap();
    let sessions = SessionStore::new(DriverRegistry::builder().build());
    sessions.set_audit_store(metadata.clone());
    sessions.set_tool_registry(GovernedToolRegistry::new(
        dispatcher,
        Arc::new(metadata.clone()),
        ToolApprovalPolicy::default(),
    ));
    let shutdown = sift_server::shutdown::Shutdown::default();
    let router = sift_server::http::app(AppState {
        sessions,
        rooms: Default::default(),
        shutdown: shutdown.clone(),
        metadata: Some(metadata.clone()),
        auth: AuthState {
            bearer_token: Some("fixture-owner".into()),
            loopback_bypass: false,
            ..Default::default()
        },
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = sift_client_sdk::Client::new(format!("http://{}", listener.local_addr().unwrap()))
        .with_bearer_token("fixture-owner");
    let server = tokio::spawn({
        let router = router.clone();
        async move { axum::serve(listener, router).await.unwrap() }
    });
    let request = InvokeExtensionRequest {
        request_id: Some(uuid::Uuid::new_v4()),
        operation,
        arguments: serde_json::json!({}),
        context: Some(ToolContext {
            tenant_id: Some(1),
            room_id: None,
            profile_id: None,
            connection_id: None,
            document_id: None,
        }),
        approval_id: None,
    };
    Fixture {
        temp,
        metadata,
        process,
        router,
        shutdown,
        client,
        server,
        request,
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
async fn approve(f: &mut Fixture) {
    let InvokeExtensionOutcome::ApprovalRequired { approval } =
        f.client.invoke_extension(&f.request).await.unwrap()
    else {
        panic!("approval required")
    };
    f.client
        .approve_operation(
            &approval.id,
            &ExpectedRevision {
                expected_revision: approval.revision,
            },
        )
        .await
        .unwrap();
    f.request.approval_id = Some(approval.id);
}
async fn stop(f: Fixture) {
    f.shutdown.begin_drain();
    f.process
        .shutdown("extension acceptance complete")
        .await
        .unwrap();
    f.server.abort();
}

#[tokio::test]
#[ignore = "requires built conformance executable"]
async fn extension_caller_loss_retains_original_owner_audit_and_one_rpc() {
    let mut f = fixture(OperationClassification::Write, Duration::from_secs(5), None).await;
    approve(&mut f).await;
    let body = serde_json::to_vec(&f.request).unwrap();
    let handler = tokio::spawn(
        f.router.clone().oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/v1/extension-actions/invoke")
                .header("authorization", "Bearer fixture-owner")
                .header("content-type", "application/json")
                .header("x-correlation-id", "extension-caller-loss")
                .body(axum::body::Body::from(body))
                .unwrap(),
        ),
    );
    until(|| f.temp.path().join(".action.entered").exists()).await;
    assert_eq!(f.shutdown.in_flight(), 1);
    handler.abort();
    let _ = handler.await;
    assert_eq!(f.shutdown.in_flight(), 1);
    std::fs::write(f.temp.path().join(".action.release"), b"release").unwrap();
    until(|| {
        f.metadata
            .extension_executions(PrincipalId(1))
            .unwrap()
            .first()
            .is_some_and(|r| r.receipt.state == ExtensionExecutionState::Acknowledged)
    })
    .await;
    until(|| f.shutdown.in_flight() == 0).await;
    let InvokeExtensionOutcome::Observed { execution } =
        f.client.invoke_extension(&f.request).await.unwrap()
    else {
        panic!("replay must be a receipt")
    };
    assert_eq!(execution.state, ExtensionExecutionState::Acknowledged);
    assert_eq!(execution.frame_started, Some(true));
    assert_eq!(
        f.client.extension_execution(execution.id).await.unwrap(),
        execution
    );
    assert_eq!(
        f.client
            .extension_execution_for_request(f.request.request_id.unwrap())
            .await
            .unwrap(),
        Some(execution.clone())
    );
    assert_eq!(
        std::fs::read_to_string(f.temp.path().join(".action.calls"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    let audits: Vec<_> = f
        .metadata
        .list_operation_audit(100)
        .unwrap()
        .into_iter()
        .filter(|r| r.action == "invoke_extension" && r.target == "extension_execution")
        .collect();
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0].actor_principal_id, Some(PrincipalId(1)));
    assert_eq!(audits[0].id.0, execution.audit_id.unwrap());
    assert_eq!(
        audits[0].correlation_id.as_deref(),
        Some("extension-caller-loss")
    );
    stop(f).await;
}

#[tokio::test]
#[ignore = "requires built conformance executable"]
async fn extension_live_authority_refuses_before_frame_and_preserves_known_ack_after_revocation() {
    for before_frame in [true, false] {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let mut f = fixture(
            OperationClassification::Write,
            Duration::from_secs(5),
            before_frame.then(|| (entered.clone(), release.clone())),
        )
        .await;
        approve(&mut f).await;
        let observer = tokio::spawn({
            let client = f.client.clone();
            let request = f.request.clone();
            async move { client.invoke_extension(&request).await.unwrap() }
        });
        if before_frame {
            entered.notified().await;
        } else {
            until(|| f.temp.path().join(".action.entered").exists()).await;
        }
        f.metadata
            .upsert_tenant_membership(TenantId(1), PrincipalId(1), MembershipRole::Viewer)
            .unwrap();
        if before_frame {
            release.notify_one();
        } else {
            std::fs::write(f.temp.path().join(".action.release"), b"release").unwrap();
        }
        let InvokeExtensionOutcome::Observed { execution } = observer.await.unwrap() else {
            panic!("revocation must withhold output")
        };
        if before_frame {
            assert_eq!(execution.state, ExtensionExecutionState::Refused);
            assert_eq!(execution.frame_started, Some(false));
            assert!(!f.temp.path().join(".action.calls").exists());
        } else {
            assert_eq!(execution.state, ExtensionExecutionState::Acknowledged);
            assert_eq!(execution.response_kind, Some(ExtensionResponseKind::Result));
            assert_eq!(
                execution.result_code,
                Some(ExtensionExecutionCode::DeliveryDenied)
            );
        }
        until(|| f.shutdown.in_flight() == 0).await;
        stop(f).await;
    }
}

#[tokio::test]
#[ignore = "requires built conformance executable"]
async fn extension_read_timeout_replay_never_repeats_a_possibly_effectful_rpc() {
    let f = fixture(
        OperationClassification::Read,
        Duration::from_millis(150),
        None,
    )
    .await;
    let InvokeExtensionOutcome::Observed { execution } =
        f.client.invoke_extension(&f.request).await.unwrap()
    else {
        panic!("timeout must have a receipt")
    };
    assert_eq!(execution.state, ExtensionExecutionState::OutcomeUnknown);
    assert_eq!(execution.frame_started, Some(true));
    assert_eq!(
        execution.result_code,
        Some(ExtensionExecutionCode::RequestTimedOut)
    );
    let InvokeExtensionOutcome::Observed { execution: replay } =
        f.client.invoke_extension(&f.request).await.unwrap()
    else {
        panic!("replay must have a receipt")
    };
    assert_eq!(replay, execution);
    assert_eq!(
        std::fs::read_to_string(f.temp.path().join(".action.calls"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    until(|| f.shutdown.in_flight() == 0).await;
    stop(f).await;
}

#[tokio::test]
#[ignore = "requires built conformance executable"]
async fn extension_document_review_includes_its_associated_profile_identity() {
    let mut f = fixture(OperationClassification::Write, Duration::from_secs(5), None).await;
    let make_profile = |value: &str| sift_metadata::NewConnectionProfile {
        name: "document-source".into(),
        provider_id: ProviderId::new("acme/source").unwrap(),
        configuration: serde_json::json!({"source":value}),
        semantic_engine: None,
        credentials: None,
        credential_mode: sift_metadata::CredentialMode::PerUser,
        tags: vec![],
    };
    let profile = f
        .metadata
        .upsert_connection_profile(TenantId(1), PrincipalId(1), make_profile("original"))
        .await
        .unwrap();
    let room = f
        .metadata
        .create_room(
            TenantId(1),
            PrincipalId(1),
            sift_metadata::NewRoom {
                name: "document-room".into(),
                kind: sift_metadata::RoomKind::Shared,
            },
        )
        .unwrap();
    let document = f
        .metadata
        .create_document_for_principal(
            room.id,
            PrincipalId(1),
            sift_metadata::NewDocument {
                kind: "query".into(),
                title: "Reviewed target".into(),
                crdt_state: vec![],
                snapshot_version: vec![],
                position: 0,
                connection_profile_id: Some(profile.id),
            },
        )
        .unwrap();
    let target = f.request.context.as_mut().unwrap();
    target.room_id = Some(room.id.0);
    target.document_id = Some(document.id.0.to_string());
    // The client deliberately supplies no profile hint; it is derived on the server.
    assert!(target.profile_id.is_none());
    approve(&mut f).await;
    f.metadata
        .upsert_connection_profile(TenantId(1), PrincipalId(1), make_profile("replacement"))
        .await
        .unwrap();
    assert!(f.client.invoke_extension(&f.request).await.is_err());
    assert!(!f.temp.path().join(".action.calls").exists());
    assert!(f
        .metadata
        .list_operation_approvals(PrincipalId(1))
        .unwrap()
        .iter()
        .all(|a| a.consumed_at.is_none()));
    stop(f).await;
}
