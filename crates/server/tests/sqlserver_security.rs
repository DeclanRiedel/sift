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
        .execute_ok(rows(vec![Row::new(vec![
            Value::Text("app".into()),
            Value::Text("owner".into()),
        ])]))
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
    assert_eq!(report.database_owner.as_deref(), Some("owner"));
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
    let state = Row::new(vec![
        Value::Bool(false),
        Value::Bool(true),
        Value::Text("a".repeat(64)),
    ]);
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
            Value::Text("a".repeat(64)),
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
        .execute_ok(rows(vec![Row::new(vec![
            Value::Text("app".into()),
            Value::Null,
        ])]))
        .execute_err(
            sift_protocol::DriverError::new(sift_protocol::Code::AuthFailed, "permission denied")
                .with_native_code("229"),
        )
        .execute_ok(rows(vec![Row::new(vec![
            Value::Text("reader".into()),
            Value::Text("DATABASE_ROLE".into()),
            Value::Text("NONE".into()),
            Value::Null,
            Value::Null,
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
    assert!(report.database_owner.is_none());
    assert_eq!(
        report.logins.state,
        SqlServerSecurityState::PermissionRequired
    );
    assert_eq!(report.principals.state, SqlServerSecurityState::Available);
    assert_eq!(report.principals.items[0].name, "reader");
}

#[tokio::test]
async fn sql_user_names_reject_windows_shape_before_driver_work() {
    let (router, session, connection) = setup(driver().build()).await;
    let response=router.oneshot(post(format!("/v1/sessions/{session}/connections/{connection}/sqlserver/security/preview"),
        serde_json::json!({"kind":"create_database_user","name":format!("DOMAIN{}user",char::from(92)),"login":"existing_sql_login"})))
        .await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("backslash"));
}

#[tokio::test]
async fn effective_matrix_pages_native_nulls_and_rejects_invalid_bounds_and_shapes() {
    let object = serde_json::json!({"kind":"table","object_id":1,"column_number":null,"schema":"dbo","relation":null,"name":"items","checks":[{"privilege":"SELECT","allowed":null}]});
    let native = serde_json::json!({"database":"app","actor":"operator","principal":"subject","principal_id":5,"database_user_token":true,"objects":[object.clone(),object]});
    let (router, session, connection) = setup(
        driver()
            .execute_ok(rows(vec![Row::new(vec![Value::Text(native.to_string())])]))
            .build(),
    )
    .await;
    let path = format!("/v1/sessions/{session}/connections/{connection}/sqlserver/privileges");
    for query in [
        "limit=0",
        "limit=101",
        "offset=10001",
        "principal=",
        "principal=a&unknown=1",
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::get(format!("{path}?{query}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{query}");
    }
    let response = router
        .oneshot(
            Request::get(format!("{path}?principal=subject&limit=1"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let report: sift_protocol::SqlServerPrivilegeReport = json(response.into_body()).await;
    assert_eq!(report.objects.len(), 1);
    assert_eq!(report.next_offset, Some(1));
    assert_eq!(report.objects[0].checks[0].allowed, None);
    assert!(report.warning.contains("NULL means unavailable"));
    let (router, session, connection) = setup(
        driver()
            .execute_ok(rows(vec![Row::new(vec![Value::Text("{}".into())])]))
            .build(),
    )
    .await;
    let response = router
        .oneshot(
            Request::get(format!(
                "/v1/sessions/{session}/connections/{connection}/sqlserver/privileges"
            ))
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn effective_matrix_refuses_publication_when_source_closes_during_owned_cleanup() {
    use std::sync::Arc;
    let close = Arc::new(tokio::sync::Notify::new());
    let native = serde_json::json!({"database":"app","actor":"operator","principal":"operator","principal_id":5,"database_user_token":false,"objects":[]});
    let mock = Arc::new(
        driver()
            .execute_ok(rows(vec![Row::new(vec![Value::Text(native.to_string())])]))
            .close_wait(close.clone())
            .build(),
    );
    let registry = DriverRegistry::builder().build();
    registry
        .providers()
        .replace(
            [Arc::new(sift_server::registry::BuiltinProviderAdapter::new(
                mock.clone(),
            ))
                as Arc<dyn sift_server::registry::DatabaseProvider>],
        )
        .unwrap();
    let store = SessionStore::new(registry);
    let session = store
        .open_session(sift_protocol::OpenSessionRequest {
            tag: None,
            tenant_id: None,
        })
        .id;
    let source = store
        .open_connection(
            session,
            Engine::SqlServer,
            sift_protocol::ConnectionSpec {
                host: "mock".into(),
                port: None,
                database: None,
                user: "operator".into(),
                password: None,
                ssl_mode: None,
                engine_specific: None,
            },
        )
        .await
        .unwrap()
        .id;
    let read = tokio::spawn({
        let store = store.clone();
        async move {
            sift_server::sqlserver_privileges::read(&store, session, source, Default::default())
                .await
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !mock.invocations().contains(&"close") {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        !read.is_finished(),
        "native cleanup is still owned and blocked"
    );
    store.close_connection(session, source).await.unwrap();
    close.notify_one();
    assert!(
        matches!(read.await.unwrap(),Err(sift_server::error::ApiError::ConnectionNotFound(id)) if id==source)
    );
}
