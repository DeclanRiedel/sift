use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use sift_driver_api::mock::MockDriver;
use sift_protocol::{
    Engine, Page, PostgresObjectPage, PostgresObjectPreview, Row, ServerInfo, Value,
};
use sift_server::http::{app, AppState, AuthState};
use sift_server::{DriverRegistry, RoomRuntime, SessionStore, Shutdown};
use tower::ServiceExt;

async fn json<T: serde::de::DeserializeOwned>(body: Body) -> T {
    serde_json::from_slice(&to_bytes(body, 1024 * 1024).await.unwrap()).unwrap()
}

fn post(path: String, body: serde_json::Value) -> Request<Body> {
    Request::post(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
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
            .oneshot(post("/v1/sessions".into(), serde_json::json!({})))
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let connection: sift_protocol::ConnectionInfo = json(
        router
            .clone()
            .oneshot(post(
                format!("/v1/sessions/{}/connections", session.id),
                serde_json::json!({"provider_id":"sift/postgres","host":"mock","user":"alice"}),
            ))
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    (router, session.id, connection.id)
}

fn pg_driver() -> sift_driver_api::mock::MockDriverBuilder {
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
async fn lists_extensions_with_bounded_paging() {
    let driver = pg_driver()
        .execute_ok(rows(vec![
            Row::new(vec![
                Value::Text("hstore".into()),
                Value::Null,
                Value::Text("1.8".into()),
                Value::Null,
            ]),
            Row::new(vec![
                Value::Text("pg_trgm".into()),
                Value::Text("1.6".into()),
                Value::Text("1.6".into()),
                Value::Text("public".into()),
            ]),
        ]))
        .build();
    let (router, session, connection) = setup(driver).await;
    let path = format!("/v1/sessions/{session}/connections/{connection}/postgres/extensions");
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
    let page: PostgresObjectPage<sift_protocol::PostgresExtension> =
        json(response.into_body()).await;
    assert_eq!(page.items[0].name, "hstore");
    assert_eq!(page.items[0].installed_version, None);
    assert_eq!(page.next_offset, Some(1));
}

#[tokio::test]
async fn preview_requires_current_state_and_confirmation_for_apply() {
    let state = Row::new(vec![
        Value::Text("hstore".into()),
        Value::Null,
        Value::Text("1.8".into()),
    ]);
    let driver = pg_driver()
        .execute_ok(rows(vec![state.clone()]))
        .execute_ok(rows(vec![state]))
        .execute_ok(rows(Vec::new()))
        .build();
    let (router, session, connection) = setup(driver).await;
    let base = format!("/v1/sessions/{session}/connections/{connection}/postgres/objects");
    let action = serde_json::json!({"kind":"install_extension","name":"hstore"});
    let response = router
        .clone()
        .oneshot(post(format!("{base}/preview"), action.clone()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let preview: PostgresObjectPreview = json(response.into_body()).await;
    assert_eq!(preview.sql, "CREATE EXTENSION \"hstore\"");
    let rejected = router
        .clone()
        .oneshot(post(
            format!("{base}/apply"),
            serde_json::json!({
                "action": action, "precondition": preview.precondition, "confirmed": false,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
    let applied = router
        .oneshot(post(
            format!("{base}/apply"),
            serde_json::json!({
                "action": preview.action, "precondition": preview.precondition, "confirmed": true,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(applied.status(), StatusCode::OK);
}

#[tokio::test]
async fn apply_rejects_stale_extension_state() {
    let driver = pg_driver()
        .execute_ok(rows(vec![Row::new(vec![
            Value::Text("hstore".into()),
            Value::Null,
            Value::Text("1.8".into()),
        ])]))
        .execute_ok(rows(vec![Row::new(vec![
            Value::Text("hstore".into()),
            Value::Text("1.8".into()),
            Value::Text("1.8".into()),
        ])]))
        .build();
    let (router, session, connection) = setup(driver).await;
    let base = format!("/v1/sessions/{session}/connections/{connection}/postgres/objects");
    let action = serde_json::json!({"kind":"install_extension","name":"hstore"});
    let preview: PostgresObjectPreview = json(
        router
            .clone()
            .oneshot(post(format!("{base}/preview"), action.clone()))
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let rejected = router
        .oneshot(post(
            format!("{base}/apply"),
            serde_json::json!({
                "action": action, "precondition": preview.precondition, "confirmed": true,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
}
