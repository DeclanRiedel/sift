use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use sift_driver_api::mock::MockDriver;
use sift_protocol::{
    Engine, Page, Row, ServerInfo, SqlServerSecurityPreview, SqlServerSecurityReport,
    SqlServerSecurityState, Value,
};
use sift_server::http::{app, AppState, AuthState};
use sift_server::{DriverRegistry, RoomRuntime, SessionStore, Shutdown};
use tower::ServiceExt;

fn rows(rows: Vec<Row>) -> Vec<Page> {
    vec![
        Page::Rows { rows },
        Page::Done {
            affected_rows: None,
            warnings: vec![],
        },
    ]
}

fn post(path: String, body: serde_json::Value) -> Request<Body> {
    Request::post(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

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
            .oneshot(post("/v1/sessions".into(), serde_json::json!({})))
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let connection: sift_protocol::ConnectionInfo = json(router.clone().oneshot(post(format!("/v1/sessions/{}/connections", session.id),
        serde_json::json!({"provider_id":"sift/sql-server","host":"mock","user":"operator"}))).await.unwrap().into_body()).await;
    (router, session.id, connection.id)
}

fn driver() -> sift_driver_api::mock::MockDriverBuilder {
    MockDriver::builder()
        .engine(Engine::SqlServer)
        .ping_ok(ServerInfo {
            provider: Engine::SqlServer.provider_ref("test"),
            server_version: "mock".into(),
            current_database: "app".into(),
            current_user: "operator".into(),
            pool_warm_slots: None,
        })
}

#[tokio::test]
async fn report_caps_catalogs_and_omits_secret_fields() {
    let logins = (0..101)
        .map(|index| {
            Row::new(vec![
                Value::Text(format!("login-{index:03}")),
                Value::Text("SQL_LOGIN".into()),
            ])
        })
        .collect();
    let driver = driver()
        .execute_ok(rows(vec![Row::new(vec![Value::Text("app".into())])]))
        .execute_ok(rows(logins))
        .execute_ok(rows(Vec::new()))
        .execute_ok(rows(Vec::new()))
        .execute_ok(rows(Vec::new()))
        .execute_ok(rows(Vec::new()))
        .build();
    let (router, session, connection) = setup(driver).await;
    let response = router
        .oneshot(
            Request::get(format!(
                "/v1/sessions/{session}/connections/{connection}/sqlserver/security"
            ))
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let payload = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let report: SqlServerSecurityReport = serde_json::from_slice(&payload).unwrap();
    assert_eq!(report.database, "app");
    assert_eq!(report.logins.items.len(), 100);
    assert!(report.logins.truncated);
    assert_eq!(
        report.schema_permissions.state,
        SqlServerSecurityState::Available
    );
    assert!(!String::from_utf8_lossy(&payload).contains("password"));
}

#[tokio::test]
async fn role_creation_requires_authority_and_production_confirmation() {
    let state = Row::new(vec![Value::Bool(false), Value::Bool(true)]);
    let driver = driver()
        .execute_ok(rows(vec![state.clone()]))
        .execute_ok(rows(vec![state]))
        .execute_ok(rows(Vec::new()))
        .build();
    let (router, session, connection) = setup(driver).await;
    let base = format!("/v1/sessions/{session}/connections/{connection}/sqlserver/security");
    let action =
        serde_json::json!({"kind":"create_database_role","name":"reader]; DROP ROLE x; --"});
    let response = router
        .clone()
        .oneshot(post(format!("{base}/preview"), action.clone()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let preview: SqlServerSecurityPreview = json(response.into_body()).await;
    assert_eq!(preview.sql, "CREATE ROLE [reader]]; DROP ROLE x; --]");
    let denied = router.clone().oneshot(post(format!("{base}/apply"), serde_json::json!({
        "action": action, "precondition": preview.precondition, "production_confirmed": false
    }))).await.unwrap();
    assert_eq!(denied.status(), StatusCode::BAD_REQUEST);
    let applied = router.oneshot(post(format!("{base}/apply"), serde_json::json!({
        "action": preview.action, "precondition": preview.precondition, "production_confirmed": true
    }))).await.unwrap();
    assert_eq!(applied.status(), StatusCode::OK);
}

#[tokio::test]
async fn membership_preview_denies_missing_authority() {
    let driver = driver()
        .execute_ok(rows(vec![Row::new(vec![
            Value::Bool(true),
            Value::Bool(true),
            Value::Bool(false),
            Value::Bool(false),
        ])]))
        .build();
    let (router, session, connection) = setup(driver).await;
    let response = router
        .oneshot(post(
            format!("/v1/sessions/{session}/connections/{connection}/sqlserver/security/preview"),
            serde_json::json!({"kind":"add_role_member","role":"readers","member":"alice"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn permission_denial_marks_only_affected_catalog_section() {
    let driver = driver()
        .execute_ok(rows(vec![Row::new(vec![Value::Text("app".into())])]))
        .execute_err(
            sift_protocol::DriverError::new(sift_protocol::Code::AuthFailed, "permission denied")
                .with_native_code("229"),
        )
        .execute_ok(rows(vec![Row::new(vec![
            Value::Text("reader".into()),
            Value::Text("DATABASE_ROLE".into()),
            Value::Text("NONE".into()),
        ])]))
        .execute_ok(rows(Vec::new()))
        .execute_ok(rows(Vec::new()))
        .execute_ok(rows(Vec::new()))
        .build();
    let (router, session, connection) = setup(driver).await;
    let response = router
        .oneshot(
            Request::get(format!(
                "/v1/sessions/{session}/connections/{connection}/sqlserver/security"
            ))
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let report: SqlServerSecurityReport = json(response.into_body()).await;
    assert_eq!(
        report.logins.state,
        SqlServerSecurityState::PermissionRequired
    );
    assert_eq!(report.principals.state, SqlServerSecurityState::Available);
    assert_eq!(report.principals.items[0].name, "reader");
}
