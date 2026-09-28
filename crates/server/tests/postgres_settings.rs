use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use sift_driver_api::mock::MockDriver;
use sift_protocol::{
    ColumnMetadata, Engine, Page, PostgresSettingsPage, PrimitiveType, Row, ServerInfo, TypeRef,
    Value,
};
use sift_server::http::{app, AppState, AuthState};
use sift_server::{DriverRegistry, RoomRuntime, SessionStore, Shutdown};
use tower::ServiceExt;

fn setting(name: &str, value: &str) -> Row {
    Row::new(vec![
        Value::Text(name.into()),
        Value::Text(value.into()),
        Value::Null,
        Value::Text("Resource Usage".into()),
        Value::Text("description".into()),
        Value::Text("user".into()),
        Value::Text("default".into()),
        Value::Bool(false),
    ])
}

async fn json<T: serde::de::DeserializeOwned>(body: Body) -> T {
    serde_json::from_slice(&to_bytes(body, 1024 * 1024).await.unwrap()).unwrap()
}

#[tokio::test]
async fn settings_route_is_bounded_and_redacts_credential_values() {
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .ping_ok(ServerInfo {
            provider: Engine::Postgres.provider_ref("test"),
            server_version: "mock".into(),
            current_database: "app".into(),
            current_user: "alice".into(),
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
            Page::Rows {
                rows: vec![
                    setting("primary_conninfo", "password=private"),
                    setting("shared_buffers", "128MB"),
                ],
            },
            Page::Done {
                affected_rows: None,
                warnings: vec![],
            },
        ])
        .build();
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
    let path = format!(
        "/v1/sessions/{}/connections/{}/settings/postgres",
        session.id, connection.id
    );
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
    let page: PostgresSettingsPage = json(response.into_body()).await;
    assert_eq!(page.settings.len(), 1);
    assert_eq!(page.next_offset, Some(1));
    assert_eq!(page.settings[0].value, None);
    assert!(page.settings[0].redacted);
}
