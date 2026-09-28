use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use sift_driver_api::mock::{MockDriver, MockDriverBuilder};
use sift_protocol::{
    Engine, Page, PostgresReplicationReport, PostgresStatisticsReport, Row, ServerInfo, Value,
};
use sift_server::http::{app, AppState, AuthState};
use sift_server::{DriverRegistry, RoomRuntime, SessionStore, Shutdown};
use tower::ServiceExt;

async fn json<T: serde::de::DeserializeOwned>(body: Body) -> T {
    serde_json::from_slice(&to_bytes(body, 1024 * 1024).await.unwrap()).unwrap()
}

async fn setup(
    driver: MockDriver,
) -> (
    axum::Router,
    sift_protocol::SessionId,
    sift_protocol::ConnectionId,
) {
    let state = AppState {
        sessions: SessionStore::new(DriverRegistry::builder().register(driver).build()),
        rooms: RoomRuntime::default(),
        auth: AuthState::default(),
        metadata: None,
        shutdown: Shutdown::default(),
    };
    let router = app(state);
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
                        r#"{"provider_id":"sift/postgres","host":"mock","user":"alice"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    (router, session.id, connection.id)
}

fn pg_driver() -> MockDriverBuilder {
    MockDriver::builder()
        .engine(Engine::Postgres)
        .ping_ok(ServerInfo {
            provider: Engine::Postgres.provider_ref("test"),
            server_version: "mock".into(),
            current_database: "app".into(),
            current_user: "alice".into(),
            pool_warm_slots: None,
        })
}

fn rows(values: Vec<Row>) -> Vec<Page> {
    vec![
        Page::Rows { rows: values },
        Page::Done {
            affected_rows: None,
            warnings: vec![],
        },
    ]
}

#[tokio::test]
async fn replication_requires_explicit_database_privilege() {
    let driver = pg_driver()
        .execute_ok(rows(vec![Row::new(vec![Value::Bool(false)])]))
        .build();
    let (router, session, connection) = setup(driver).await;
    let response = router
        .oneshot(
            Request::get(format!(
                "/v1/sessions/{session}/connections/{connection}/postgres/replication"
            ))
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn replication_exposes_bounded_safe_fields() {
    let driver = pg_driver()
        .execute_ok(rows(vec![Row::new(vec![Value::Bool(true)])]))
        .execute_ok(rows(vec![Row::new(vec![
            Value::Int64(42),
            Value::Text("standby".into()),
            Value::Text("streaming".into()),
            Value::Text("async".into()),
            Value::Null,
            Value::Int64(12),
            Value::Null,
        ])]))
        .execute_ok(rows(vec![Row::new(vec![
            Value::Text("streaming".into()),
            Value::Text("0/100".into()),
            Value::Text("0/200".into()),
        ])]))
        .execute_ok(rows(vec![Row::new(vec![
            Value::Text("slot_a".into()),
            Value::Text("physical".into()),
            Value::Null,
            Value::Bool(true),
            Value::Text("0/100".into()),
            Value::Null,
        ])]))
        .build();
    let (router, session, connection) = setup(driver).await;
    let response = router
        .oneshot(
            Request::get(format!(
                "/v1/sessions/{session}/connections/{connection}/postgres/replication"
            ))
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let report: PostgresReplicationReport = json(response.into_body()).await;
    assert_eq!(report.senders[0].pid, 42);
    assert_eq!(report.senders[0].write_lag_ms, None);
    assert_eq!(
        report.receiver.unwrap().received_lsn.as_deref(),
        Some("0/100")
    );
    assert_eq!(report.slots[0].database, None);
}

#[tokio::test]
async fn statistics_pages_visible_tables_and_preserves_reset_time() {
    let database = Row::new(vec![
        Value::Text("app".into()),
        Value::Int64(2),
        Value::Int64(10),
        Value::Int64(1),
        Value::Int64(20),
        Value::Int64(30),
        Value::Int64(40),
        Value::Int64(50),
        Value::Int64(60),
        Value::Int64(70),
        Value::Int64(80),
        Value::Text("2026-09-29 00:00:00".into()),
    ]);
    let table = |name: &str| {
        Row::new(vec![
            Value::Text("public".into()),
            Value::Text(name.into()),
            Value::Int64(5),
            Value::Null,
            Value::Int64(100),
            Value::Int64(2),
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
        ])
    };
    let driver = pg_driver()
        .execute_ok(rows(vec![database]))
        .execute_ok(rows(vec![table("alpha"), table("beta")]))
        .build();
    let (router, session, connection) = setup(driver).await;
    let path = format!("/v1/sessions/{session}/connections/{connection}/postgres/statistics");
    let invalid = router
        .clone()
        .oneshot(
            Request::get(format!("{path}?limit=201"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    let response = router
        .oneshot(
            Request::get(format!("{path}?limit=1"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let report: PostgresStatisticsReport = json(response.into_body()).await;
    assert_eq!(report.database.database, "app");
    assert!(report.database.stats_reset.is_some());
    assert_eq!(report.tables.len(), 1);
    assert_eq!(report.tables[0].table, "alpha");
    assert_eq!(report.tables[0].index_scans, None);
    assert_eq!(report.next_offset, Some(1));
}
