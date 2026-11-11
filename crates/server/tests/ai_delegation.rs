//! Captured human delegation lifecycle over HTTP; approval starts no provider.
#![cfg(unix)]
use axum::{body::Body, http::Request};
use chrono::Utc;
use sift_client_sdk::{Client, ScopedAiClient};
use sift_driver_sqlite::{FilePolicy, SqliteDriver};
use sift_metadata::{
    CredentialMode, MemorySecretStore, MetadataStore, NewConnectionProfile, PrincipalId, TenantId,
};
use sift_protocol::{handshake::*, *};
use sift_server::{
    http::{app, AppState, AuthState},
    DriverRegistry, SessionStore,
};
use std::{collections::BTreeMap, sync::Arc};
use tower::ServiceExt;
use uuid::Uuid;

struct Fixture {
    _root: tempfile::TempDir,
    server: tokio::task::JoinHandle<()>,
    state: AppState,
    client: Client,
    scoped: ScopedAiClient,
    target: ToolContext,
    chat: Uuid,
    desktop: Uuid,
    session: SessionId,
    connection: ConnectionId,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        rusqlite::Connection::open(root.path().join("review.db")).unwrap();
        let driver = Arc::new(SqliteDriver::with_files(FilePolicy {
            config: SqliteDriverConfig {
                roots: BTreeMap::from([(
                    "test".into(),
                    SqliteRootConfig {
                        path: root.path().to_string_lossy().into(),
                        allowed_tenants: vec![1],
                        read_only: false,
                    },
                )]),
                max_connections: 8,
            },
            protected: vec![],
        }));
        let registry = DriverRegistry::new();
        registry
            .providers()
            .replace([Arc::new(sift_server::BuiltinProviderAdapter::new(driver))
                as Arc<dyn sift_server::DatabaseProvider>])
            .unwrap();
        let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        metadata.bootstrap_local("human reviewer").unwrap();
        let profile=metadata.upsert_connection_profile(TenantId(1),PrincipalId(1),NewConnectionProfile {name:"review source".into(),provider_id:Engine::Sqlite.provider_id(),semantic_engine:Some(Engine::Sqlite),configuration:serde_json::json!({"root_id":"test","path":"review.db","mode":"read_write"}),credentials:None,credential_mode:CredentialMode::Shared,tags:vec!["production".into()]}).await.unwrap();
        let mut auth = AuthState {
            bearer_token: Some("delegation-fixture".into()),
            loopback_bypass: false,
            instance_id: "review-instance".into(),
            daemon_generation: "review-daemon".into(),
            ..Default::default()
        };
        auth.ai.enabled = true;
        auth.ai.execution_enabled = true;
        auth.ai.delegation_enabled = true;
        let state = AppState {
            sessions: SessionStore::new(registry),
            rooms: Default::default(),
            metadata: Some(metadata),
            auth,
            shutdown: Default::default(),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = Client::new(format!("http://{}", listener.local_addr().unwrap()))
            .with_bearer_token("delegation-fixture");
        let router = app(state.clone());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let scoped = client
            .capture_ai_caller(&ExtensionCallerIdentity {
                instance_id: "review-instance".into(),
                daemon_generation: "review-daemon".into(),
                principal_id: 1,
                auth_session_id: None,
            })
            .await
            .unwrap();
        let session = scoped
            .open_session_for_tenant(None, Some(1))
            .await
            .unwrap()
            .id;
        let connection = scoped
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
        let chat = scoped
            .create_ai_chat(&CreateAiChatRequest {
                tenant_id: 1,
                room_id: None,
                title: "human review".into(),
            })
            .await
            .unwrap()
            .id;
        Self {
            _root: root,
            server,
            state,
            client,
            scoped,
            target: ToolContext {
                tenant_id: Some(1),
                profile_id: Some(profile.id.0),
                connection_id: Some(format!("{session}:{connection}")),
                room_id: None,
                document_id: None,
            },
            chat,
            desktop: Uuid::new_v4(),
            session,
            connection,
        }
    }
    fn review(&self) -> CreateAiDelegationReview {
        CreateAiDelegationReview {
            client_request_id: Uuid::new_v4(),
            desktop_id: self.desktop,
            target: self.target.clone(),
            policy: AiDelegationPolicy {
                budget: AiDelegationBudget {
                    max_tool_calls: 3,
                    max_input_bytes: 4096,
                    max_output_bytes: 8192,
                    max_duration_secs: 60,
                },
                rules: vec![AiDelegationRule {
                    id: Uuid::new_v4(),
                    max_uses: 1,
                    action: AiDelegationActionRule::ExactSql {
                        sql: "DELETE FROM private_review_sentinel WHERE id=1".into(),
                        params: vec![],
                    },
                }],
            },
            expires_at: Utc::now() + chrono::Duration::minutes(5),
            material_references: vec![],
        }
    }
    async fn enable(&self) {
        let policy = self
            .scoped
            .ai_delegation_tenant_policy(sift_api_types::TenantId(1))
            .await
            .unwrap();
        assert!(!policy.enabled);
        self.scoped
            .set_ai_delegation_tenant_policy(
                sift_api_types::TenantId(1),
                &SetAiDelegationTenantPolicy {
                    expected_revision: 0,
                    enabled: true,
                    arbitrary_sql_enabled: false,
                },
            )
            .await
            .unwrap();
    }
    async fn post(
        &self,
        state: AppState,
        path: &str,
        body: impl serde::Serialize,
        changed: Option<&str>,
    ) -> axum::http::Response<Body> {
        let mut request = Request::post(path)
            .header("authorization", "Bearer delegation-fixture")
            .header("content-type", "application/json");
        if changed != Some("missing") {
            request = request
                .header(EXPECTED_INSTANCE_HEADER, "review-instance")
                .header(
                    EXPECTED_DAEMON_HEADER,
                    if changed == Some("daemon") {
                        "replaced"
                    } else {
                        "review-daemon"
                    },
                )
                .header(
                    EXPECTED_PRINCIPAL_HEADER,
                    if changed == Some("principal") {
                        "2"
                    } else {
                        "1"
                    },
                )
                .header(
                    EXPECTED_AUTH_SESSION_HEADER,
                    if changed == Some("session") {
                        "other-session"
                    } else {
                        "-"
                    },
                );
        }
        app(state)
            .oneshot(
                request
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn captured_human_reviews_are_explicit_one_use_and_withdrawable_after_execution_is_disabled()
{
    let f = Fixture::new().await;
    let request = f.review();
    assert!(
        f.scoped
            .create_ai_delegation_review(f.chat, &request)
            .await
            .is_err(),
        "tenant opt-in defaults off"
    );
    let path = format!("/v1/scoped/ai/chats/{}/delegation-reviews", f.chat);
    for changed in ["missing", "daemon", "principal", "session"] {
        assert!(!f
            .post(f.state.clone(), &path, &request, Some(changed))
            .await
            .status()
            .is_success());
    }
    assert!(f
        .scoped
        .ai_delegation_reviews(f.chat)
        .await
        .unwrap()
        .is_empty());
    let json = serde_json::to_string(&request).unwrap();
    let duplicated = format!("{{\"desktop_id\":\"{}\",{}", f.desktop, &json[1..]);
    let response = app(f.state.clone())
        .oneshot(
            Request::post(&path)
                .header("authorization", "Bearer delegation-fixture")
                .header("content-type", "application/json")
                .header(EXPECTED_INSTANCE_HEADER, "review-instance")
                .header(EXPECTED_DAEMON_HEADER, "review-daemon")
                .header(EXPECTED_PRINCIPAL_HEADER, "1")
                .header(EXPECTED_AUTH_SESSION_HEADER, "-")
                .body(Body::from(duplicated))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        axum::http::StatusCode::UNPROCESSABLE_ENTITY,
        "duplicate fields must still be rejected before review creation"
    );
    f.enable().await;
    let mut disabled = f.state.clone();
    disabled.auth.ai.delegation_enabled = false;
    assert!(
        !f.post(disabled, &path, &request, None)
            .await
            .status()
            .is_success(),
        "instance opt-in is independent"
    );
    assert!(
        !f.post(
            f.state.clone(),
            &path.replace("/v1/scoped/", "/v1/"),
            &request,
            None
        )
        .await
        .status()
        .is_success(),
        "no ordinary alias"
    );
    let review = f
        .scoped
        .create_ai_delegation_review(f.chat, &request)
        .await
        .unwrap();
    let replay = f
        .scoped
        .create_ai_delegation_review(f.chat, &request)
        .await
        .unwrap();
    assert_eq!(review.summary.id, replay.summary.id);
    assert_eq!(
        review.summary.state,
        AiDelegationReviewState::AwaitingApproval
    );
    let listed = f.scoped.ai_delegation_reviews(f.chat).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert!(!serde_json::to_string(&listed)
        .unwrap()
        .contains("private_review_sentinel"));
    assert!(
        f.scoped.ai_runs(f.chat).await.unwrap().is_empty(),
        "review starts no model turn"
    );
    let approve = ApproveAiDelegationReview {
        fingerprint: review.summary.fingerprint.clone(),
        production_confirmation: Some(review.content.source.database_label.clone()),
    };
    let mut wrong = approve.clone();
    wrong.fingerprint = "0".repeat(64);
    assert!(f
        .scoped
        .approve_ai_delegation_review(review.summary.id, &wrong)
        .await
        .is_err());
    wrong = approve.clone();
    wrong.production_confirmation = None;
    assert!(f
        .scoped
        .approve_ai_delegation_review(review.summary.id, &wrong)
        .await
        .is_err());
    assert_eq!(
        f.scoped
            .approve_ai_delegation_review(review.summary.id, &approve)
            .await
            .unwrap()
            .state,
        AiDelegationReviewState::Approved
    );
    assert_eq!(
        f.scoped
            .approve_ai_delegation_review(review.summary.id, &approve)
            .await
            .unwrap()
            .state,
        AiDelegationReviewState::Approved
    );
    assert!(
        f.scoped.ai_runs(f.chat).await.unwrap().is_empty(),
        "approval alone starts no model turn"
    );
    // Privileged fixture activates the internal grant; no public startup field
    // or provider tool is introduced by this human review API.
    let metadata = f.state.metadata.as_ref().unwrap();
    let instance = sift_metadata::AiDelegationAdmissionPolicy {
        enabled: true,
        arbitrary_sql_enabled: false,
        scope_sha256: review.content.scope_sha256.clone(),
        max_calls: 20,
        max_duration_secs: 600,
    };
    let prepared = metadata
        .prepare_ai_delegation_review(review.summary.id, PrincipalId(1), instance)
        .await
        .unwrap();
    let start:StartAiTurnRequest=serde_json::from_value(serde_json::json!({"client_request_id":Uuid::new_v4(),"desktop_id":f.desktop,"prompt":"reviewed run","provider":"codex","mode":"execute","context":{"target":f.target,"staged_change_count":0}})).unwrap();
    let lease = metadata
        .start_ai_run_with_delegation(
            f.chat,
            PrincipalId(1),
            start,
            review.content.source.clone(),
            prepared,
        )
        .await
        .unwrap();
    assert_eq!(
        f.scoped
            .ai_delegation_review(review.summary.id)
            .await
            .unwrap()
            .summary
            .run_state,
        Some(AiDelegationRunState::Active)
    );
    let mut disabled = f.state.clone();
    disabled.auth.ai.enabled = false;
    disabled.auth.ai.execution_enabled = false;
    disabled.auth.ai.delegation_enabled = false;
    for _ in 0..2 {
        let response = f
            .post(
                disabled.clone(),
                &format!(
                    "/v1/scoped/ai/delegation-reviews/{}/revoke",
                    review.summary.id
                ),
                EndAiDelegationReview {
                    fingerprint: review.summary.fingerprint.clone(),
                },
                None,
            )
            .await;
        assert!(response.status().is_success());
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let summary: AiDelegationReviewSummary = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(summary.state, AiDelegationReviewState::Consumed);
        assert_eq!(summary.used_run_id, Some(lease.run.id));
        assert_eq!(summary.run_state, Some(AiDelegationRunState::Revoked));
    }
    assert_eq!(
        f.scoped
            .ai_run(f.chat, lease.run.id)
            .await
            .unwrap()
            .run
            .status,
        AiRunStatus::Canceled
    );
    let other = f
        .scoped
        .create_ai_delegation_review(f.chat, &f.review())
        .await
        .unwrap();
    assert_eq!(
        f.scoped
            .deny_ai_delegation_review(
                other.summary.id,
                &EndAiDelegationReview {
                    fingerprint: other.summary.fingerprint.clone()
                }
            )
            .await
            .unwrap()
            .state,
        AiDelegationReviewState::Denied
    );
    assert!(f
        .scoped
        .approve_ai_delegation_review(
            other.summary.id,
            &ApproveAiDelegationReview {
                fingerprint: other.summary.fingerprint,
                production_confirmation: Some(other.content.source.database_label)
            }
        )
        .await
        .is_err());
    let actions = f.state.sessions.list_operations();
    for expected in [
        "create_delegation_review",
        "approve_delegation_review",
        "revoke_delegation_review",
        "deny_delegation_review",
        "set_delegation_tenant_policy",
    ] {
        assert!(actions
            .iter()
            .any(|a| matches!(&a.operation,Operation::Ai {action,..} if action==expected)));
    }
    assert!(!serde_json::to_string(&actions)
        .unwrap()
        .contains("private_review_sentinel"));

    let expected = ExtensionCallerIdentity {
        instance_id: "review-instance".into(),
        daemon_generation: "review-daemon".into(),
        principal_id: 1,
        auth_session_id: None,
    };
    let (_, first_token) = metadata
        .issue_api_token(
            PrincipalId(1),
            Some(TenantId(1)),
            "original-review-credential",
            None,
        )
        .unwrap();
    let (_, second_token) = metadata
        .issue_api_token(
            PrincipalId(1),
            Some(TenantId(1)),
            "replacement-review-credential",
            None,
        )
        .unwrap();
    let original = f
        .client
        .clone()
        .with_bearer_token(first_token)
        .capture_ai_caller(&expected)
        .await
        .unwrap();
    let replacement = f
        .client
        .clone()
        .with_bearer_token(second_token)
        .capture_ai_caller(&expected)
        .await
        .unwrap();
    let original_review = original
        .create_ai_delegation_review(f.chat, &f.review())
        .await
        .unwrap();
    let approve = ApproveAiDelegationReview {
        fingerprint: original_review.summary.fingerprint.clone(),
        production_confirmation: Some(original_review.content.source.database_label),
    };
    assert!(
        replacement
            .approve_ai_delegation_review(original_review.summary.id, &approve)
            .await
            .is_err(),
        "a different credential for the same principal cannot inherit review approval"
    );
    assert_eq!(
        original
            .approve_ai_delegation_review(original_review.summary.id, &approve)
            .await
            .unwrap()
            .state,
        AiDelegationReviewState::Approved
    );
    let mut arbitrary = f.review();
    arbitrary.policy.rules[0].action = AiDelegationActionRule::ArbitrarySql {
        acknowledge_unbounded_effects: true,
    };
    assert!(f
        .scoped
        .create_ai_delegation_review(f.chat, &arbitrary)
        .await
        .is_err());
    f.scoped
        .set_ai_delegation_tenant_policy(
            sift_api_types::TenantId(1),
            &SetAiDelegationTenantPolicy {
                expected_revision: 1,
                enabled: true,
                arbitrary_sql_enabled: true,
            },
        )
        .await
        .unwrap();
    assert!(
        f.scoped
            .create_ai_delegation_review(f.chat, &arbitrary)
            .await
            .is_err(),
        "tenant arbitrary-SQL opt-in alone is insufficient"
    );
    let mut enabled = f.state.clone();
    enabled.auth.ai.arbitrary_sql_delegation_enabled = true;
    assert!(f
        .post(enabled, &path, &arbitrary, None)
        .await
        .status()
        .is_success());
}

#[tokio::test]
async fn human_migration_reviews_resolve_native_commands_and_refuse_lost_source_material() {
    let f = Fixture::new().await;
    f.enable().await;
    f.client
        .execute(
            f.session,
            f.connection,
            "CREATE TABLE reviewed_native (id INTEGER NOT NULL)",
        )
        .await
        .unwrap();
    let graph = f
        .client
        .catalog_graph(
            f.session,
            f.connection,
            CatalogGraphRequest {
                refresh: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let target = f
        .client
        .create_catalog_snapshot(
            f.session,
            f.connection,
            CreateCatalogSnapshotRequest {
                expected_catalog_revision: graph.revision,
                options: Default::default(),
                description: None,
                accept_partial: true,
            },
        )
        .await
        .unwrap();
    f.client
        .execute(f.session, f.connection, "DROP TABLE reviewed_native")
        .await
        .unwrap();
    let live = f
        .client
        .catalog_graph(
            f.session,
            f.connection,
            CatalogGraphRequest {
                refresh: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let diff_request = SchemaDiffRequest {
        from: CatalogSourceRef::Live {
            expected_revision: live.revision,
            options: Default::default(),
        },
        to: CatalogSourceRef::Snapshot {
            snapshot_id: target.id,
        },
        accepted_renames: vec![],
        max_changes: Some(1000),
    };
    let diff = f
        .client
        .compare_catalog_schemas(f.session, f.connection, diff_request.clone())
        .await
        .unwrap();
    let plan = f
        .client
        .preview_migration(
            f.session,
            f.connection,
            PreviewMigrationRequest {
                diff: diff_request,
                expected_diff_digest: diff.digest,
                selected_changes: vec![],
                expected_live_revision: live.revision,
                options: Default::default(),
            },
        )
        .await
        .unwrap();
    let rule = Uuid::new_v4();
    let refs = vec![AiDelegationMaterialReference::MigrationPlan {
        rule_id: rule,
        plan_id: plan.id,
    }];
    let preview = f
        .scoped
        .preview_ai_delegation_materials(
            f.chat,
            &PreviewAiDelegationMaterialsRequest {
                target: f.target.clone(),
                material_references: refs.clone(),
            },
        )
        .await
        .unwrap();
    let AiDelegationMaterialFingerprint::MigrationPlan {
        contract_sha256,
        change_count,
        ..
    } = &preview.fingerprints[0]
    else {
        panic!("native migration fingerprint")
    };
    assert!(serde_json::to_string(&preview.materials)
        .unwrap()
        .contains("reviewed_native"));
    assert!(!format!("{preview:?}").contains("reviewed_native"));
    let mut request = f.review();
    request.material_references = refs;
    request.policy.rules[0] = AiDelegationRule {
        id: rule,
        max_uses: 1,
        action: AiDelegationActionRule::ExactMigration {
            contract_sha256: contract_sha256.clone(),
            change_count: *change_count,
        },
    };
    let review = f
        .scoped
        .create_ai_delegation_review(f.chat, &request)
        .await
        .unwrap();
    let approve = ApproveAiDelegationReview {
        fingerprint: review.summary.fingerprint.clone(),
        production_confirmation: Some(review.content.source.database_label),
    };
    assert_eq!(
        f.scoped
            .approve_ai_delegation_review(review.summary.id, &approve)
            .await
            .unwrap()
            .state,
        AiDelegationReviewState::Approved
    );
    f.scoped
        .close_connection(f.session, f.connection)
        .await
        .unwrap();
    assert!(
        f.scoped
            .approve_ai_delegation_review(review.summary.id, &approve)
            .await
            .is_err(),
        "approval cannot rebind lost native material"
    );
    assert_eq!(
        f.scoped
            .revoke_ai_delegation_review(
                review.summary.id,
                &EndAiDelegationReview {
                    fingerprint: review.summary.fingerprint
                }
            )
            .await
            .unwrap()
            .state,
        AiDelegationReviewState::Revoked
    );
}

struct NeverInvoke;
#[async_trait::async_trait]
impl sift_server::extension_dispatch::ActionInvoker for NeverInvoke {
    async fn request(
        &self,
        _tenant_id: Option<i64>,
        _request: sift_extension_protocol::Request,
    ) -> Result<sift_extension_protocol::Response, sift_plugin_host::SupervisorError> {
        panic!("human review must never invoke an extension")
    }
}

#[tokio::test]
async fn human_reviews_freeze_governed_tool_contracts_and_actual_room_query_text() {
    use sift_server::{
        automation::{GovernedToolRegistry, ToolApprovalPolicy},
        extension_dispatch::{ActionRegistration, ExtensionOperationDispatcher},
    };
    let f = Fixture::new().await;
    f.enable().await;
    let metadata = f.state.metadata.as_ref().unwrap();
    let dispatcher = ExtensionOperationDispatcher::new(Arc::new(metadata.clone()));
    let extension_id = ExtensionId::new("fixture/review").unwrap();
    let contribution_id = ContributionId::new("fixture/review/database_provider/action").unwrap();
    let operation = ExtensionOperation {
        extension_id: extension_id.clone(),
        contribution_id: contribution_id.clone(),
        action: SegmentId::new("run").unwrap(),
        classification: OperationClassification::Write,
        target_kind: SegmentId::new("tenant").unwrap(),
        target_id: Some("1".into()),
        sanitized_arguments: Default::default(),
    };
    let registration = |max_result_bytes| ActionRegistration {
        extension_id: extension_id.clone(),
        contribution_id: contribution_id.clone(),
        action: operation.action.clone(),
        classification: OperationClassification::Write,
        required_context: vec![ContributionContext::Tenant],
        input_schema: serde_json::json!({"type":"object"}),
        output_schema: serde_json::json!({"type":"object"}),
        timeout: std::time::Duration::from_secs(5),
        max_result_bytes,
        package_identity: None,
        invoker: Arc::new(NeverInvoke),
    };
    dispatcher.replace([registration(1024)]).unwrap();
    let tools = GovernedToolRegistry::new(
        dispatcher.clone(),
        Arc::new(metadata.clone()),
        ToolApprovalPolicy::default(),
    );
    tools
        .replace([GovernedToolDescriptor {
            id: "review-tool".into(),
            title: "Reviewed tool".into(),
            description: "Explicit human review".into(),
            operation: operation.clone(),
            input_schema: serde_json::json!({"type":"object"}),
            output_schema: serde_json::json!({"type":"object"}),
            required_context: vec![ContributionContext::Tenant],
            mcp_exposable: true,
            schedulable: false,
            interactive: false,
        }])
        .unwrap();
    f.state.sessions.set_tool_registry(tools);
    let rule = Uuid::new_v4();
    let refs = vec![AiDelegationMaterialReference::ExtensionTool {
        rule_id: rule,
        tool_id: "review-tool".into(),
    }];
    let preview = f
        .scoped
        .preview_ai_delegation_materials(
            f.chat,
            &PreviewAiDelegationMaterialsRequest {
                target: f.target.clone(),
                material_references: refs.clone(),
            },
        )
        .await
        .unwrap();
    let AiDelegationMaterialFingerprint::ExtensionTool {
        contract_sha256, ..
    } = &preview.fingerprints[0]
    else {
        panic!("extension contract")
    };
    let mut request = f.review();
    request.material_references = refs;
    request.policy.rules[0] = AiDelegationRule {
        id: rule,
        max_uses: 1,
        action: AiDelegationActionRule::Extension {
            contract_sha256: contract_sha256.clone(),
            arguments: AiDelegationArgumentRule::Object {
                fields: Default::default(),
                required: vec![],
            },
            max_argument_bytes: 1024,
        },
    };
    let review = f
        .scoped
        .create_ai_delegation_review(f.chat, &request)
        .await
        .unwrap();
    dispatcher.replace([registration(2048)]).unwrap();
    assert!(
        f.scoped
            .approve_ai_delegation_review(
                review.summary.id,
                &ApproveAiDelegationReview {
                    fingerprint: review.summary.fingerprint,
                    production_confirmation: Some(review.content.source.database_label)
                }
            )
            .await
            .is_err(),
        "native contract replacement cannot inherit human approval"
    );
    let room = metadata
        .create_room(
            TenantId(1),
            PrincipalId(1),
            sift_metadata::NewRoom {
                name: "review room".into(),
                kind: sift_metadata::RoomKind::Shared,
            },
        )
        .unwrap();
    let replica = sift_doc::TextReplica::new(sift_doc::random_peer_id()).unwrap();
    replica.insert(0, "SELECT private_original_query").unwrap();
    let create_doc = |kind: &str| {
        metadata
            .create_document(
                room.id,
                sift_metadata::NewDocument {
                    kind: kind.into(),
                    title: "query".into(),
                    crdt_state: replica.export_snapshot().unwrap(),
                    snapshot_version: replica.version_vector(),
                    position: 0,
                    connection_profile_id: None,
                },
            )
            .unwrap()
    };
    let doc = create_doc("sql");
    let markdown = create_doc("markdown");
    let mut target = f.target.clone();
    target.room_id = Some(room.id.0);
    target.document_id = Some(markdown.id.0.to_string());
    let rule = Uuid::new_v4();
    let refs = vec![AiDelegationMaterialReference::RoomQuery { rule_id: rule }];
    assert!(f
        .scoped
        .preview_ai_delegation_materials(
            f.chat,
            &PreviewAiDelegationMaterialsRequest {
                target: target.clone(),
                material_references: refs.clone()
            }
        )
        .await
        .is_err());
    target.document_id = Some(doc.id.0.to_string());
    let preview = f
        .scoped
        .preview_ai_delegation_materials(
            f.chat,
            &PreviewAiDelegationMaterialsRequest {
                target: target.clone(),
                material_references: refs.clone(),
            },
        )
        .await
        .unwrap();
    let AiDelegationMaterialFingerprint::Query {
        before_sha256,
        base_revision,
        ..
    } = &preview.fingerprints[0]
    else {
        panic!("committed query fingerprint")
    };
    let mut request = f.review();
    request.target = target;
    request.material_references = refs;
    request.policy.rules[0] = AiDelegationRule {
        id: rule,
        max_uses: 1,
        action: AiDelegationActionRule::QueryEdit {
            target: AiQueryExecutionTarget::RoomDocument {
                room_id: room.id.0,
                document_id: doc.id.0,
            },
            before_sha256: before_sha256.clone(),
            base_revision: *base_revision,
            replacement: AiDelegationTextRule::Exact {
                text: "SELECT 2".into(),
            },
        },
    };
    let review = f
        .scoped
        .create_ai_delegation_review(f.chat, &request)
        .await
        .unwrap();
    let actor = f
        .state
        .rooms
        .documents()
        .get_or_load(metadata, doc.id)
        .unwrap();
    actor
        .lock()
        .unwrap()
        .author_replacement(
            metadata,
            PrincipalId(1),
            "changed-room-text",
            &Uuid::new_v4().to_string(),
            "SELECT changed_query",
        )
        .unwrap();
    assert!(
        f.scoped
            .approve_ai_delegation_review(
                review.summary.id,
                &ApproveAiDelegationReview {
                    fingerprint: review.summary.fingerprint,
                    production_confirmation: Some(review.content.source.database_label)
                }
            )
            .await
            .is_err(),
        "room review uses current committed text, not client-submitted bytes"
    );
}
