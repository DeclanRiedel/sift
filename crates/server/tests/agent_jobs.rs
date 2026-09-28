use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use sift_driver_api::mock::MockDriver;
use sift_protocol::{
    AgentJobOutcome, AgentJobsReport, AgentJobsState, ColumnMetadata, Engine, Page, PrimitiveType,
    Row, ServerInfo, TypeRef, Value,
};
use sift_server::http::{app, AppState, AuthState};
use sift_server::{DriverRegistry, RoomRuntime, SessionStore, Shutdown};
use tower::ServiceExt;

async fn json<T: serde::de::DeserializeOwned>(body: Body) -> T {
    serde_json::from_slice(&to_bytes(body, 1024 * 1024).await.unwrap()).unwrap()
}

#[tokio::test]
async fn agent_jobs_route_caps_rows_and_omits_job_commands() {
    let rows = (0..101)
        .map(|index| {
            Row::new(vec![
                Value::Text(format!("00000000-0000-0000-0000-{index:012}")),
                Value::Text(format!("job-{index:03}")),
                Value::Bool(true),
                Value::Text("operator".into()),
                Value::Int32(1),
                Value::Int32(20260928),
                Value::Int32(93000),
                Value::Int32(5),
            ])
        })
        .collect();
    let driver = MockDriver::builder()
        .engine(Engine::SqlServer)
        .ping_ok(ServerInfo {
            provider: Engine::SqlServer.provider_ref("test"),
            server_version: "mock".into(),
            current_database: "app".into(),
            current_user: "operator".into(),
            pool_warm_slots: None,
        })
        .execute_ok(vec![
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
            Page::Rows { rows },
            Page::Done {
                affected_rows: None,
                warnings: vec![],
            },
        ])
        .build();
    let router = app(AppState {
        sessions: SessionStore::new(DriverRegistry::builder().register(driver).build()),
        rooms: RoomRuntime::default(),
        auth: AuthState::default(),
        metadata: None,
        shutdown: Shutdown::default(),
    });
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
                        r#"{"provider_id":"sift/sql-server","host":"mock","user":"operator"}"#,
                    ))
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
                "/v1/sessions/{}/connections/{}/agent/jobs",
                session.id, connection.id
            ))
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let payload = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let report: AgentJobsReport = serde_json::from_slice(&payload).unwrap();
    assert_eq!(report.state, AgentJobsState::Available);
    assert_eq!(report.jobs.len(), 100);
    assert!(report.truncated);
    assert_eq!(
        report.jobs[0].last_outcome,
        Some(AgentJobOutcome::Succeeded)
    );
    assert!(!String::from_utf8_lossy(&payload).contains("command"));
}
