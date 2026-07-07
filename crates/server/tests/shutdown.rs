//! Graceful-shutdown drain gate (ADR-018, Phase B reliability step 2).
//!
//! Drives the axum surface via `tower::ServiceExt::oneshot` and asserts that
//! once the drain gate flips, new work (sessions, connections) is refused with
//! `503 service_draining`, while a session opened before draining is still
//! reachable (its in-flight work continues).

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use sift_driver_api::mock::MockDriver;
use sift_protocol::{Engine, SchemaScope, SchemaSnapshot, ServerInfo, SessionInfo};
use sift_server::http::{app, AppState, AuthState};
use sift_server::registry::DriverRegistry;
use sift_server::room_runtime::RoomRuntime;
use sift_server::session::SessionStore;
use sift_server::shutdown::Shutdown;
use tower::ServiceExt;

fn mock_driver() -> MockDriver {
    MockDriver::builder()
        .engine(Engine::Postgres)
        .ping_ok(ServerInfo {
            engine: Engine::Postgres,
            server_version: "MockDB 0.1".into(),
            current_database: "mock".into(),
            current_user: "mock".into(),
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
                "engine": "postgres",
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
