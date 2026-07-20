use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use sift_driver_api::mock::MockDriver;
use sift_protocol::{
    ColumnMetadata, DatabaseProcess, Engine, Page, PrimitiveType, Row, ServerInfo, TypeRef, Value,
};
use sift_server::http::{app, AppState, AuthState};
use sift_server::{DriverRegistry, RoomRuntime, SessionStore, Shutdown};
use tower::ServiceExt;

fn process_pages() -> Vec<Page> {
    vec![
        Page::NextResult {
            columns: (0..8)
                .map(|index| {
                    ColumnMetadata::new(
                        format!("c{index}"),
                        TypeRef::Primitive(PrimitiveType::Text),
                    )
                })
                .collect(),
        },
        Page::Rows {
            rows: vec![Row::new(vec![
                Value::Int64(73),
                Value::Text("alice".into()),
                Value::Text("app".into()),
                Value::Text("active".into()),
                Value::Text("select * from jobs".into()),
                Value::Null,
                Value::Text("Lock:relation".into()),
                Value::Text("41,42".into()),
            ])],
        },
        Page::Done {
            affected_rows: None,
            warnings: vec![],
        },
    ]
}

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
        .execute_ok(process_pages())
        .execute_ok(vec![
            Page::NextResult {
                columns: vec![ColumnMetadata::new(
                    "pg_terminate_backend",
                    TypeRef::Primitive(PrimitiveType::Bool),
                )],
            },
            Page::Rows {
                rows: vec![Row::new(vec![Value::Bool(true)])],
            },
            Page::Done {
                affected_rows: None,
                warnings: vec![],
            },
        ])
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
async fn process_routes_list_and_kill() {
    let router = app(state());
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

    let response = router
        .clone()
        .oneshot(
            Request::get(format!(
                "/v1/sessions/{}/connections/{}/processes",
                session.id, connection.id
            ))
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let processes: Vec<DatabaseProcess> = json(response.into_body()).await;
    assert_eq!(processes[0].process_id, 73);
    assert_eq!(processes[0].blocked_by, vec![41, 42]);

    let response = router
        .oneshot(
            Request::post(format!(
                "/v1/sessions/{}/connections/{}/processes/kill",
                session.id, connection.id
            ))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"process_id":73}"#))
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let result: sift_protocol::KillProcessResponse = json(response.into_body()).await;
    assert!(result.terminated);
}
