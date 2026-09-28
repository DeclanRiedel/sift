use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use sift_driver_api::mock::MockDriver;
use sift_protocol::{
    ColumnMetadata, Engine, Page, PrimitiveType, Row, ServerInfo, SqlServerSettingsReport,
    SqlServerSettingsState, TypeRef, Value,
};
use sift_server::http::{app, AppState, AuthState};
use sift_server::{DriverRegistry, RoomRuntime, SessionStore, Shutdown};
use tower::ServiceExt;

async fn json<T: serde::de::DeserializeOwned>(body: Body) -> T {
    serde_json::from_slice(&to_bytes(body, 1024 * 1024).await.unwrap()).unwrap()
}

#[tokio::test]
async fn sqlserver_settings_route_caps_rows_and_exposes_effective_value() {
    let rows = (0..201)
        .map(|index| {
            Row::new(vec![
                Value::Text(format!("setting-{index:03}")),
                Value::Int64(4),
                Value::Int64(2),
                Value::Int64(0),
                Value::Int64(32767),
                Value::Bool(true),
                Value::Bool(false),
                Value::Text("A server setting".into()),
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
                "/v1/sessions/{}/connections/{}/settings/sqlserver",
                session.id, connection.id
            ))
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let payload = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let report: SqlServerSettingsReport = serde_json::from_slice(&payload).unwrap();
    assert_eq!(report.state, SqlServerSettingsState::Available);
    assert_eq!(report.settings.len(), 200);
    assert!(report.truncated);
    assert_eq!(report.settings[0].configured_value, 4);
    assert_eq!(report.settings[0].effective_value, 2);
}
