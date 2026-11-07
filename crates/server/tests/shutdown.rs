//! Graceful-shutdown drain gate (ADR-018).
//!
//! Drives the axum surface via `tower::ServiceExt::oneshot` and asserts that
//! once the drain gate flips, new work (sessions, connections) is refused with
//! `503 service_draining`, while a session opened before draining is still
//! reachable (its in-flight work continues).

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use sift_driver_api::mock::MockDriver;
use sift_metadata::{MemorySecretStore, MetadataStore};
use sift_protocol::{Engine, Readiness, SchemaScope, SchemaSnapshot, ServerInfo, SessionInfo};
use sift_server::http::{app, AppState, AuthState};
use sift_server::registry::DriverRegistry;
use sift_server::room_runtime::RoomRuntime;
use sift_server::session::SessionStore;
use sift_server::shutdown::Shutdown;
use std::sync::Arc;
use tower::ServiceExt;

fn mock_driver() -> MockDriver {
    MockDriver::builder()
        .engine(Engine::Postgres)
        .ping_ok(ServerInfo {
            provider: Engine::Postgres.provider_ref("test"),
            server_version: "MockDB 0.1".into(),
            current_database: "mock".into(),
            current_user: "mock".into(),
            pool_warm_slots: None,
        })
        .schema_ok(SchemaSnapshot::empty(SchemaScope::shallow()))
        .build()
}

fn test_state() -> AppState {
    let registry = DriverRegistry::builder().register(mock_driver()).build();
    AppState {
        sessions: SessionStore::new(registry),
        rooms: RoomRuntime::default(),
        auth: AuthState::default(),
        metadata: None,
        shutdown: Shutdown::default(),
    }
}

fn post(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::post(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

async fn body_json<T: serde::de::DeserializeOwned>(body: Body) -> T {
    let bytes = to_bytes(body, 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn new_session_is_rejected_while_draining() {
    let state = test_state();
    let shutdown = state.shutdown.clone();
    let app = app(state);

    shutdown.begin_drain();

    let res = app
        .clone()
        .oneshot(post("/v1/sessions", serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: serde_json::Value = body_json(res.into_body()).await;
    assert_eq!(body["kind"], "service_draining");
}

#[tokio::test]
async fn new_connection_is_rejected_while_draining() {
    let state = test_state();
    let shutdown = state.shutdown.clone();
    let app = app(state);

    // Open a session before draining begins.
    let res = app
        .clone()
        .oneshot(post("/v1/sessions", serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let session: SessionInfo = body_json(res.into_body()).await;

    shutdown.begin_drain();

    // Opening a connection on the existing session is new work → refused.
    let res = app
        .clone()
        .oneshot(post(
            &format!("/v1/sessions/{}/connections", session.id),
            serde_json::json!({
                "provider_id": "sift/postgres",
                "host": "mock.invalid",
                "database": "mock",
                "user": "mock",
                "ssl_mode": "disable"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);

    // The session itself is still reachable while draining.
    let res = app
        .oneshot(
            Request::get(format!("/v1/sessions/{}", session.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn sessions_open_freely_before_draining() {
    let app = app(test_state());
    let res = app
        .oneshot(post("/v1/sessions", serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

async fn get_ready(app: axum::Router) -> (StatusCode, Readiness) {
    let res = app
        .oneshot(Request::get("/v1/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    (status, body_json(res.into_body()).await)
}

#[tokio::test]
async fn pinning_unsupported_protocol_version_is_rejected() {
    let app = app(test_state());
    let request = Request::get("/v1/health")
        .header("x-sift-protocol-version", "999")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(request).await.unwrap();
    assert_eq!(res.status(), StatusCode::UPGRADE_REQUIRED);
    let body: serde_json::Value = body_json(res.into_body()).await;
    assert_eq!(body["kind"], "unsupported_protocol_version");
}

#[tokio::test]
async fn pinning_supported_protocol_version_is_accepted() {
    let app = app(test_state());
    let request = Request::get("/v1/health")
        .header("x-sift-protocol-version", sift_protocol::PROTOCOL_VERSION)
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(request).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    // Server advertises its version on the response regardless.
    assert_eq!(
        res.headers()
            .get("x-sift-protocol-version")
            .and_then(|v| v.to_str().ok()),
        Some(sift_protocol::PROTOCOL_VERSION)
    );
}

#[tokio::test]
async fn error_body_echoes_correlation_id() {
    let app = app(test_state());
    let request = Request::get("/v1/sessions/999999")
        .header("x-correlation-id", "corr-err")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(request).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        res.headers()
            .get("x-correlation-id")
            .and_then(|v| v.to_str().ok()),
        Some("corr-err")
    );
    let body: serde_json::Value = body_json(res.into_body()).await;
    assert_eq!(body["correlation_id"], "corr-err");
}

#[tokio::test]
async fn ready_returns_200_when_healthy() {
    // No metadata store configured → metadata_ok is None (nothing to reach).
    let (status, body) = get_ready(app(test_state())).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.ready);
    assert!(!body.draining);
    assert!(body.drivers_registered);
    assert_eq!(body.metadata_ok, None);
}

#[tokio::test]
async fn ready_returns_503_while_draining() {
    let state = test_state();
    let shutdown = state.shutdown.clone();
    let app = app(state);
    shutdown.begin_drain();

    let (status, body) = get_ready(app).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(!body.ready);
    assert!(body.draining);
}

#[tokio::test]
async fn ready_probes_configured_metadata_store() {
    let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
    let registry = DriverRegistry::builder().register(mock_driver()).build();
    let state = AppState {
        sessions: SessionStore::new(registry),
        rooms: RoomRuntime::default(),
        auth: AuthState::default(),
        metadata: Some(metadata),
        shutdown: Shutdown::default(),
    };

    let (status, body) = get_ready(app(state)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.ready);
    assert_eq!(body.metadata_ok, Some(true));
}

#[tokio::test]
async fn ready_returns_503_when_no_driver_registered() {
    let state = AppState {
        sessions: SessionStore::new(DriverRegistry::builder().build()),
        rooms: RoomRuntime::default(),
        auth: AuthState::default(),
        metadata: None,
        shutdown: Shutdown::default(),
    };

    let (status, body) = get_ready(app(state)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(!body.ready);
    assert!(!body.drivers_registered);
}

#[tokio::test]
async fn http_query_owner_keeps_settlement_and_native_shutdown_after_caller_loss() {
    use sift_metadata::{CredentialMode, NewConnectionProfile, PrincipalId, QueryStatus, TenantId};
    use sift_protocol::{
        ChangeLedgerFilter, ChangeLedgerOutcome, Code, ConnectionSpec, Operation, OperationStatus,
        Page,
    };
    use std::time::Duration;
    for case in ["success", "native_error", "timeout", "response_limit"] {
        let gate = Arc::new(tokio::sync::Notify::new());
        let (native, rows) = tokio::sync::mpsc::channel(2);
        let mock = Arc::new(
            MockDriver::builder()
                .execute_wait(gate.clone())
                .execute_rows(rows)
                .build(),
        );
        let registry = DriverRegistry::new();
        registry
            .providers()
            .replace(
                [Arc::new(sift_server::registry::BuiltinProviderAdapter::new(
                    mock.clone(),
                ))
                    as Arc<dyn sift_server::registry::DatabaseProvider>],
            )
            .unwrap();
        let sessions = SessionStore::new(registry);
        let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        metadata.bootstrap_local("query owner fixture").unwrap();
        sessions.set_audit_store(metadata.clone());
        sessions.set_authorization_store(metadata.clone());
        sessions.set_store_sql(false);
        sessions.set_request_timeout(if case == "timeout" {
            Duration::from_millis(100)
        } else {
            Duration::from_secs(2)
        });
        if case == "response_limit" {
            sessions.set_resource_manager(sift_server::resources::ResourceManager::new(
                &sift_server::config::TenantLimitsConfig {
                    trusted_local_unlimited: false,
                    defaults: sift_protocol::TenantResourceLimits {
                        // Native Done fits; the complete JSON response does not.
                        retained_result_bytes: Some(128),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                None,
            ));
        }
        let spec = ConnectionSpec {
            host: "mock.invalid".into(),
            port: None,
            database: Some("fixture".into()),
            user: "mock".into(),
            password: None,
            ssl_mode: None,
            engine_specific: None,
        };
        let profile = metadata
            .upsert_connection_profile(
                TenantId(1),
                PrincipalId(1),
                NewConnectionProfile {
                    name: "query ownership".into(),
                    provider_id: Engine::Postgres.provider_id(),
                    configuration: serde_json::to_value(&spec).unwrap(),
                    semantic_engine: Some(Engine::Postgres),
                    credentials: None,
                    credential_mode: CredentialMode::Shared,
                    tags: vec![],
                },
            )
            .await
            .unwrap();
        let session = sessions
            .open_session_with_owner(
                sift_protocol::OpenSessionRequest {
                    tag: None,
                    tenant_id: Some(1),
                },
                Some(PrincipalId(1)),
                Some(TenantId(1)),
                true,
            )
            .unwrap()
            .id;
        let connection = sessions
            .open_managed_connection(
                session,
                profile.provider_id.clone(),
                profile.configuration.clone(),
                Default::default(),
                PrincipalId(1),
                TenantId(1),
                profile.id,
                profile.policy.revision,
                true,
            )
            .await
            .unwrap()
            .id;
        let state = AppState {
            sessions: sessions.clone(),
            rooms: Default::default(),
            auth: Default::default(),
            metadata: Some(metadata.clone()),
            shutdown: Default::default(),
        };
        let shutdown = state.shutdown.clone();
        let app = app(state);
        let correlation = format!("http-owner-{case}");
        let sql = "UPDATE notes SET value='private-http-owner-sql' WHERE id=1";
        let mut request = post(
            &format!("/v1/sessions/{session}/queries"),
            serde_json::json!({
                "connection": connection, "sql": sql, "connection_profile_id": profile.id.0,
            }),
        );
        request
            .headers_mut()
            .insert("x-correlation-id", correlation.parse().unwrap());
        let mut caller = Some(tokio::spawn(app.oneshot(request)));
        tokio::time::timeout(Duration::from_secs(2), async {
            while !mock.invocations().contains(&"execute") {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(shutdown.in_flight(), 1);
        if case != "response_limit" {
            let caller = caller.take().unwrap();
            caller.abort();
            assert!(caller.await.unwrap_err().is_cancelled());
        }
        shutdown.begin_drain();
        let completed = || async {
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let durable = metadata.list_operation_audit(100).unwrap();
                    let queries: Vec<_> = durable
                        .iter()
                        .filter(|row| row.action == "execute" && row.target == "query")
                        .collect();
                    let history = metadata
                        .list_query_history_for_principal(PrincipalId(1), 100)
                        .unwrap();
                    let ledger = metadata
                        .change_ledger(&ChangeLedgerFilter::default())
                        .unwrap();
                    if queries.len() == 1 && history.len() == 1 {
                        assert_eq!(queries[0].actor_principal_id, Some(PrincipalId(1)));
                        assert_eq!(
                            queries[0].correlation_id.as_deref(),
                            Some(correlation.as_str())
                        );
                        assert_eq!(history[0].principal_id, PrincipalId(1));
                        assert_eq!(history[0].connection_profile_id, Some(profile.id));
                        assert_eq!(
                            history[0].status,
                            if case == "native_error" || case == "timeout" {
                                QueryStatus::Error
                            } else {
                                QueryStatus::Ok
                            }
                        );
                        let recorded =
                            serde_json::to_string(&(&durable, &history, &ledger)).unwrap();
                        assert!(!recorded.contains("private-http-owner-sql"));
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap();
        };
        if case == "timeout" {
            completed().await;
        }
        gate.notify_one();
        native
            .send(if case == "native_error" {
                Page::Error {
                    error: sift_protocol::DriverError::new(
                        Code::SyntaxError,
                        "native echo: private-http-owner-sql",
                    ),
                }
            } else {
                Page::Done {
                    affected_rows: Some(1),
                    warnings: vec![],
                }
            })
            .await
            .unwrap();
        if case == "response_limit" {
            let response = caller.take().unwrap().await.unwrap().unwrap();
            assert_ne!(response.status(), StatusCode::OK);
            drop(response);
        }
        if case != "timeout" {
            completed().await;
        }
        assert!(
            metadata
                .change_ledger(&ChangeLedgerFilter::default())
                .unwrap()
                .entries
                .is_empty(),
            "{case}: result observation cannot settle a native receipt before EOF"
        );
        assert_eq!(shutdown.await_drain(Duration::from_millis(10)).await, 1);
        let operations = sessions.list_operations();
        let observed: Vec<_> = operations
            .iter()
            .filter(|row| matches!(row.operation, Operation::ExecuteQuery { .. }))
            .collect();
        assert_eq!(observed.len(), 1);
        assert_eq!(
            observed[0].status,
            if case == "native_error" || case == "timeout" {
                OperationStatus::Failed
            } else {
                OperationStatus::Succeeded
            }
        );
        assert!(!serde_json::to_string(&operations)
            .unwrap()
            .contains("private-http-owner-sql"));
        drop(native);
        assert_eq!(shutdown.await_drain(Duration::from_secs(1)).await, 0);
        let ledger = metadata
            .change_ledger(&ChangeLedgerFilter::default())
            .unwrap();
        assert_eq!(ledger.entries.len(), 1, "{case}");
        assert_eq!(
            ledger.entries[0].correlation_id.as_deref(),
            Some(correlation.as_str())
        );
        assert_eq!(
            ledger.entries[0].outcome,
            if case == "native_error" {
                ChangeLedgerOutcome::OutcomeUnknown
            } else {
                ChangeLedgerOutcome::Committed
            },
            "{case}"
        );
        assert_eq!(
            ledger.entries[0].row_count,
            if case == "native_error" {
                None
            } else {
                Some(1)
            },
            "{case}"
        );

        assert_eq!(
            sessions
                .list_operations()
                .iter()
                .filter(|row| matches!(row.operation, Operation::ExecuteQuery { .. }))
                .count(),
            1
        );
        sessions.close_session(session).unwrap();
    }
}

#[tokio::test]
async fn export_audit_tracks_body_observation_and_native_eof_after_caller_loss() {
    use futures::StreamExt;
    use sift_metadata::PrincipalId;
    use sift_protocol::{Code, ConnectionSpec, Operation, OperationStatus, Page, Row, Value};
    use std::time::Duration;

    for case in [
        "success",
        "native_error",
        "missing_done",
        "encoder_error",
        "unpolled_body",
        "body_drop",
        "startup_drop",
        "startup_error",
        "timeout",
        "drain",
    ] {
        let gate = Arc::new(tokio::sync::Notify::new());
        let (native, rows) = tokio::sync::mpsc::channel(2);
        let mock = Arc::new(
            MockDriver::builder()
                .execute_wait(gate.clone())
                .execute_rows(rows)
                .build(),
        );
        let registry = DriverRegistry::new();
        registry
            .providers()
            .replace(
                [Arc::new(sift_server::registry::BuiltinProviderAdapter::new(
                    mock.clone(),
                ))
                    as Arc<dyn sift_server::registry::DatabaseProvider>],
            )
            .unwrap();
        let sessions = SessionStore::new(registry);
        let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        metadata.bootstrap_local("export fixture").unwrap();
        sessions.set_audit_store(metadata.clone());
        sessions.set_request_timeout(if case == "timeout" {
            Duration::from_millis(100)
        } else {
            Duration::ZERO
        });
        let session = sessions
            .open_session_with_owner(
                sift_protocol::OpenSessionRequest {
                    tag: None,
                    tenant_id: None,
                },
                Some(PrincipalId(1)),
                None,
                true,
            )
            .unwrap()
            .id;
        let connection = sessions
            .open_connection(
                session,
                Engine::Postgres,
                ConnectionSpec {
                    host: "mock.invalid".into(),
                    port: None,
                    database: Some("fixture".into()),
                    user: "mock".into(),
                    password: None,
                    ssl_mode: None,
                    engine_specific: None,
                },
            )
            .await
            .unwrap()
            .id;
        let state = AppState {
            sessions: sessions.clone(),
            rooms: Default::default(),
            auth: Default::default(),
            metadata: Some(metadata.clone()),
            shutdown: Default::default(),
        };
        let shutdown = state.shutdown.clone();
        let correlation = format!("export-owner-{case}");
        let target = if case == "startup_error" {
            sift_protocol::ConnectionId(connection.0 + 1)
        } else {
            connection
        };
        let mut request = post(
            &format!("/v1/sessions/{session}/connections/{target}/export"),
            serde_json::json!({"sql":"SELECT 'private-export-sql', $1",
                "params":[Value::Text("private-export-bind".into())],
                "format":if case == "encoder_error" { "parquet" } else { "csv" }, "header":false }),
        );
        request
            .headers_mut()
            .insert("x-correlation-id", correlation.parse().unwrap());
        let caller = tokio::spawn(app(state).oneshot(request));
        if case != "startup_error" {
            tokio::time::timeout(Duration::from_secs(2), async {
                while !mock.invocations().contains(&"execute") {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(shutdown.in_flight(), 1, "{case}");
        }
        let mut native = Some(native);
        if case == "startup_drop" {
            caller.abort();
            assert!(caller.await.unwrap_err().is_cancelled());
            assert!(
                sessions
                    .list_operations()
                    .iter()
                    .all(|row| !matches!(row.operation, Operation::ExportQuery { .. })),
                "startup owner retains audit until native startup settles"
            );
            assert_eq!(shutdown.in_flight(), 1);
            gate.notify_one();
        } else {
            if case != "timeout" {
                gate.notify_one();
            }
            let response = tokio::time::timeout(Duration::from_secs(2), caller)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            if matches!(case, "startup_error" | "timeout") {
                assert!(!response.status().is_success(), "{case}");
            } else {
                assert_eq!(response.status(), StatusCode::OK, "{case}");
                assert!(
                    !sessions
                        .list_operations()
                        .iter()
                        .any(|entry| matches!(entry.operation, Operation::ExportQuery { .. })),
                    "{case}: cursor opening is not export success"
                );
                if case == "unpolled_body" {
                    drop(response);
                } else if case == "body_drop" {
                    native
                        .as_ref()
                        .unwrap()
                        .send(Page::Rows {
                            rows: vec![Row::new(vec![Value::Text("private-export-row".into())])],
                        })
                        .await
                        .unwrap();
                    let mut body = response.into_body().into_data_stream();
                    let bytes = tokio::time::timeout(Duration::from_secs(2), body.next())
                        .await
                        .unwrap()
                        .unwrap()
                        .unwrap();
                    assert!(bytes.starts_with(b"private-export-row"));
                    drop(body);
                } else if case == "drain" {
                    shutdown.begin_drain();
                    assert!(to_bytes(response.into_body(), 1024).await.is_err());
                } else {
                    if case == "missing_done" {
                        drop(native.take());
                    } else {
                        native
                            .as_ref()
                            .unwrap()
                            .send(if case == "native_error" {
                                Page::Error {
                                    error: sift_protocol::DriverError::new(
                                        Code::SyntaxError,
                                        "native echo: private-export-sql private-export-bind",
                                    ),
                                }
                            } else {
                                Page::Done {
                                    affected_rows: Some(0),
                                    warnings: vec![],
                                }
                            })
                            .await
                            .unwrap();
                    }
                    let result = to_bytes(response.into_body(), 1024).await;
                    assert_eq!(result.is_ok(), case == "success", "{case}: {result:?}");
                }
            }
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let records = metadata.list_operation_audit(100).unwrap();
                let exports: Vec<_> = records
                    .iter()
                    .filter(|row| row.action == "export" && row.target == "query")
                    .collect();
                if exports.len() == 1 {
                    assert_eq!(
                        exports[0].actor_principal_id,
                        Some(PrincipalId(1)),
                        "{case}"
                    );
                    assert_eq!(
                        exports[0].correlation_id.as_deref(),
                        Some(correlation.as_str()),
                        "{case}"
                    );
                    assert_eq!(
                        exports[0].status,
                        if case == "success" {
                            "succeeded"
                        } else {
                            "failed"
                        },
                        "{case}"
                    );
                    if case == "native_error" {
                        assert_eq!(
                            exports[0].result_code.as_ref(),
                            Some(&Code::SyntaxError.to_string()),
                            "{case}"
                        );
                    }
                    let encoded =
                        serde_json::to_string(&(records, sessions.list_operations())).unwrap();
                    assert!(
                        !encoded.contains("private-export"),
                        "{case}: audit must exclude input/row/native echoes"
                    );
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|error| panic!("{case}: audit did not settle: {error}"));
        if case == "startup_error" {
            assert!(
                !mock.invocations().contains(&"execute"),
                "invalid connection must fail before native dispatch"
            );
        }
        let entries = sessions.list_operations();
        let exports: Vec<_> = entries
            .iter()
            .filter(|entry| matches!(entry.operation, Operation::ExportQuery { .. }))
            .collect();
        assert_eq!(exports.len(), 1, "{case}");
        assert_eq!(
            exports[0].status,
            if case == "success" {
                OperationStatus::Succeeded
            } else {
                OperationStatus::Failed
            },
            "{case}"
        );
        if !matches!(case, "missing_done" | "startup_error") {
            assert_eq!(
                shutdown.await_drain(Duration::from_millis(10)).await,
                1,
                "{case}: native owner must retain shutdown until raw EOF"
            );
        }
        if matches!(case, "startup_drop" | "timeout") {
            gate.notify_one();
        }
        drop(native);
        assert_eq!(
            shutdown.await_drain(Duration::from_secs(1)).await,
            0,
            "{case}"
        );
        sessions.close_session(session).unwrap();
    }
}
