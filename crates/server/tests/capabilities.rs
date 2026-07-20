use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use sift_driver_api::mock::MockDriver;
use sift_protocol::{Engine, OperationCapability, OperationKind, ServerInfo};
use sift_server::http::{app, AppState, AuthState};
use sift_server::{DriverRegistry, RoomRuntime, SessionStore, Shutdown};
use tower::ServiceExt;

fn state() -> AppState {
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .ping_ok(ServerInfo {
            engine: Engine::Postgres,
            server_version: "mock".into(),
            current_database: "app".into(),
            current_user: "alice".into(),
            pool_warm_slots: None,
        })
        .build();
    AppState {
        sessions: SessionStore::new(DriverRegistry::builder().register(driver).build()),
        rooms: RoomRuntime::default(),
        auth: AuthState::default(),
        metadata: None,
        shutdown: Shutdown::default(),
    }
}

async fn json<T: serde::de::DeserializeOwned>(body: Body) -> T {
    serde_json::from_slice(&to_bytes(body, 1024 * 1024).await.unwrap()).unwrap()
}

#[tokio::test]
async fn capabilities_follow_live_connection_and_transaction_context() {
    let router = app(state());
    let response = router
        .clone()
        .oneshot(
            Request::get("/v1/operations/available")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let capabilities: Vec<OperationCapability> = json(response.into_body()).await;
    assert_eq!(capabilities.len(), OperationKind::ALL.len());
    assert!(
        capabilities
            .iter()
            .find(|capability| capability.operation == OperationKind::OpenSession)
            .unwrap()
            .available
    );
    assert!(
        !capabilities
            .iter()
            .find(|capability| capability.operation == OperationKind::ExecuteQuery)
            .unwrap()
            .available
    );

    let session: sift_protocol::SessionInfo = json(
        router
            .clone()
            .oneshot(
                Request::post("/v1/sessions")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let connection: sift_protocol::ConnectionInfo = json(
        router
            .clone()
            .oneshot(
                Request::post(format!("/v1/sessions/{}/connections", session.id))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"engine":"postgres","host":"mock","user":"alice"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let transaction: sift_protocol::TransactionInfo = json(
        router
            .clone()
            .oneshot(
                Request::post(format!("/v1/sessions/{}/transactions", session.id))
                    .header("content-type", "application/json")
                    .body(Body::from(format!(
                        r#"{{"connection":{}}}"#,
                        connection.id.0
                    )))
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body(),
    )
    .await;

    let response = router
        .oneshot(
            Request::get(format!(
                "/v1/operations/available?session={}&connection={}&transaction={}",
                session.id, connection.id, transaction.tx_id
            ))
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let capabilities: Vec<OperationCapability> = json(response.into_body()).await;
    assert!(
        capabilities
            .iter()
            .find(|capability| capability.operation == OperationKind::CommitTransaction)
            .unwrap()
            .available
    );
    assert!(
        !capabilities
            .iter()
            .find(|capability| capability.operation == OperationKind::BeginTransaction)
            .unwrap()
            .available
    );
    assert!(
        !capabilities
            .iter()
            .find(|capability| capability.operation == OperationKind::ReleaseSavepoint)
            .unwrap()
            .destructive
    );
}
