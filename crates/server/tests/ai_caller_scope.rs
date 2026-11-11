//! Captured HTTP callers must be fenced before setup and native work.
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use sift_client_sdk::{Client, Error};
use sift_driver_api::mock::MockDriver;
use sift_metadata::{MemorySecretStore, MetadataStore, PrincipalId, TenantId};
use sift_protocol::handshake::*;
use sift_protocol::*;
use sift_server::{
    http::{AppState, AuthState},
    DriverRegistry, SessionStore,
};
use std::sync::Arc;
use tower::ServiceExt;

#[tokio::test]
async fn captured_ai_setup_and_turns_preserve_caller_and_runtime_ownership() {
    let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::default())).unwrap();
    metadata.bootstrap_local("caller owner").unwrap();
    let room = metadata
        .create_room(
            TenantId(1),
            PrincipalId(1),
            sift_metadata::NewRoom {
                name: "captured results".into(),
                kind: sift_metadata::RoomKind::Shared,
            },
        )
        .unwrap();
    let info = ServerInfo {
        provider: Engine::Postgres.provider_ref("fixture"),
        server_version: "fixture".into(),
        current_database: "app".into(),
        current_user: "reader".into(),
        pool_warm_slots: None,
    };
    let driver = Arc::new(
        MockDriver::builder()
            .engine(Engine::Postgres)
            .ping_ok(info.clone())
            .ping_ok(info)
            .build(),
    );
    let registry = DriverRegistry::new();
    registry
        .providers()
        .replace(
            [Arc::new(sift_server::registry::BuiltinProviderAdapter::new(
                driver.clone(),
            ))
                as Arc<dyn sift_server::registry::DatabaseProvider>],
        )
        .unwrap();
    let sessions = SessionStore::new(registry);
    sessions.set_audit_store(metadata.clone());
    let foreign = sessions
        .open_session_with_owner(
            OpenSessionRequest {
                tag: None,
                tenant_id: Some(1),
            },
            Some(PrincipalId(2)),
            Some(TenantId(1)),
            false,
        )
        .unwrap();
    let profile=metadata.upsert_connection_profile(TenantId(1),PrincipalId(1),sift_metadata::NewConnectionProfile {name:"captured source".into(),provider_id:Engine::Postgres.provider_id(),semantic_engine:Some(Engine::Postgres),configuration:serde_json::json!({"host":"fixture.invalid","database":"app","user":"reader","engine_specific":{}}),credentials:None,credential_mode:sift_metadata::CredentialMode::Shared,tags:vec![]}).await.unwrap();
    let mut auth = AuthState {
        bearer_token: Some("caller-fixture".into()),
        loopback_bypass: false,
        instance_id: "caller-instance".into(),
        daemon_generation: "caller-generation".into(),
        ..Default::default()
    };
    auth.ai.enabled = true;
    auth.ai.execution_enabled = true;
    let router = sift_server::http::app(AppState {
        sessions: sessions.clone(),
        rooms: Default::default(),
        metadata: Some(metadata.clone()),
        auth,
        shutdown: Default::default(),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = Client::new(format!("http://{}", listener.local_addr().unwrap()))
        .with_bearer_token("caller-fixture");
    let server = tokio::spawn({
        let router = router.clone();
        async move { axum::serve(listener, router).await.unwrap() }
    });
    let expected = ExtensionCallerIdentity {
        instance_id: "caller-instance".into(),
        daemon_generation: "caller-generation".into(),
        principal_id: 1,
        auth_session_id: None,
    };
    let mut wrong = expected.clone();
    wrong.daemon_generation = "replaced".into();
    assert!(client.capture_ai_caller(&wrong).await.is_err());
    wrong = expected.clone();
    wrong.principal_id = 2;
    assert!(client.capture_ai_caller(&wrong).await.is_err());
    for case in [
        "missing",
        "instance",
        "daemon",
        "principal",
        "session",
        "duplicate",
    ] {
        let mut request = Request::post("/v1/scoped/sessions")
            .header("authorization", "Bearer caller-fixture")
            .header("content-type", "application/json");
        if case != "missing" {
            request = request
                .header(
                    EXPECTED_INSTANCE_HEADER,
                    if case == "instance" {
                        "changed"
                    } else {
                        "caller-instance"
                    },
                )
                .header(
                    EXPECTED_DAEMON_HEADER,
                    if case == "daemon" {
                        "changed"
                    } else {
                        "caller-generation"
                    },
                )
                .header(
                    EXPECTED_PRINCIPAL_HEADER,
                    if case == "principal" { "2" } else { "1" },
                )
                .header(
                    EXPECTED_AUTH_SESSION_HEADER,
                    if case == "session" {
                        "other-session"
                    } else {
                        "-"
                    },
                );
            if case == "duplicate" {
                request = request.header(EXPECTED_PRINCIPAL_HEADER, "1");
            }
        }
        let response = router
            .clone()
            .oneshot(request.body(Body::from("{\"tenant_id\":1}")).unwrap())
            .await
            .unwrap();
        assert!(
            matches!(
                response.status(),
                StatusCode::BAD_REQUEST | StatusCode::FORBIDDEN
            ),
            "{case}"
        );
        assert_eq!(
            sessions.list_sessions().len(),
            1,
            "{case} must not create a session"
        );
        assert!(
            driver.invocations().is_empty(),
            "{case} must not touch the driver"
        );
    }
    let scoped = client.capture_ai_caller(&expected).await.unwrap();
    let vaults = scoped.vaults(sift_api_types::TenantId(1)).await.unwrap();
    let ordinary_vaults = client.vaults(sift_api_types::TenantId(1)).await.unwrap();
    assert_eq!(
        vaults.iter().map(|vault| vault.id).collect::<Vec<_>>(),
        ordinary_vaults
            .iter()
            .map(|vault| vault.id)
            .collect::<Vec<_>>()
    );
    assert!(scoped
        .room_results(sift_api_types::RoomId(room.id.0))
        .await
        .unwrap()
        .is_empty());
    for path in [
        "/v1/scoped/metadata/vaults?tenant=1".to_owned(),
        format!("/v1/scoped/metadata/rooms/{}/results", room.id.0),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::get(path)
                    .header("authorization", "Bearer caller-fixture")
                    .header(EXPECTED_INSTANCE_HEADER, "caller-instance")
                    .header(EXPECTED_DAEMON_HEADER, "caller-generation")
                    .header(EXPECTED_PRINCIPAL_HEADER, "2")
                    .header(EXPECTED_AUTH_SESSION_HEADER, "-")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    let denied = scoped
        .open_connection_from_profile(
            foreign.id,
            sift_api_types::OpenConnectionFromProfileRequest {
                tenant_id: 1,
                profile_id: profile.id.0,
            },
        )
        .await;
    assert!(matches!(
        denied,
        Err(Error::Server {
            status: StatusCode::FORBIDDEN,
            ..
        })
    ));
    assert!(driver.invocations().is_empty());
    let encoded = foreign
        .id
        .0
        .to_string()
        .bytes()
        .map(|b| format!("%{b:02X}"))
        .collect::<String>();
    for path in [
        format!("/v1/sessions/{}", foreign.id),
        format!("/v1/scoped/sessions/{}", foreign.id),
        format!("/v1/sessions/{encoded}"),
        format!("/v1/scoped/sessions/{encoded}"),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::delete(&path)
                    .header("authorization", "Bearer caller-fixture")
                    .header(EXPECTED_INSTANCE_HEADER, "caller-instance")
                    .header(EXPECTED_DAEMON_HEADER, "caller-generation")
                    .header(EXPECTED_PRINCIPAL_HEADER, "1")
                    .header(EXPECTED_AUTH_SESSION_HEADER, "-")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
        assert_eq!(
            sessions.session_owner(foreign.id).unwrap(),
            Some(PrincipalId(2))
        );
    }
    let session = scoped.open_session_for_tenant(None, Some(1)).await.unwrap();
    assert_eq!(
        sessions.session_owner(session.id).unwrap(),
        Some(PrincipalId(1))
    );
    let connection = scoped
        .open_connection_from_profile(
            session.id,
            sift_api_types::OpenConnectionFromProfileRequest {
                tenant_id: 1,
                profile_id: profile.id.0,
            },
        )
        .await
        .unwrap();
    scoped
        .ping_connection(session.id, connection.id)
        .await
        .unwrap();
    let chat = scoped
        .create_ai_chat(&CreateAiChatRequest {
            tenant_id: 1,
            room_id: None,
            title: "captured chat".into(),
        })
        .await
        .unwrap();
    let context=serde_json::from_value(serde_json::json!({"target":{"tenant_id":1,"profile_id":profile.id.0,"connection_id":format!("{}:{}",session.id,connection.id)},"staged_change_count":0})).unwrap();
    let run = scoped
        .start_ai_turn(
            chat.id,
            &StartAiTurnRequest {
                client_request_id: uuid::Uuid::new_v4(),
                desktop_id: uuid::Uuid::new_v4(),
                prompt: "captured turn".into(),
                provider: AiProvider::Codex,
                model: None,
                mode: AiMode::Execute,
                context,
                attachment_previews: vec![],
            },
        )
        .await
        .unwrap();
    assert_eq!(
        scoped.ai_run(chat.id, run.run.id).await.unwrap().run.id,
        run.run.id
    );
    scoped
        .finish_ai_run(
            run.run.id,
            &FinishAiRunRequest {
                lease_token: run.lease_token,
                status: AiRunStatus::Completed,
            },
        )
        .await
        .unwrap();
    scoped
        .close_connection(session.id, connection.id)
        .await
        .unwrap();
    scoped.close_session(session.id).await.unwrap();
    assert_eq!(sessions.list_sessions().len(), 1);
    server.abort();
}
