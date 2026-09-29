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

#[tokio::test]
async fn role_catalog_excludes_secrets_and_is_paged() {
    let driver = pg_driver()
        .execute_ok(rows(vec![
            Row::new(vec![
                Value::Text("analyst".into()),
                Value::Text("false".into()),
                Value::Text("false".into()),
                Value::Text("false".into()),
            ]),
            Row::new(vec![
                Value::Text("operator".into()),
                Value::Text("true".into()),
                Value::Text("true".into()),
                Value::Text("false".into()),
            ]),
        ]))
        .build();
    let (router, session, connection) = setup(driver).await;
    let response = router
        .oneshot(
            Request::get(format!(
                "/v1/sessions/{session}/connections/{connection}/postgres/roles?limit=1"
            ))
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let page: PostgresObjectPage<sift_protocol::PostgresRole> = json(response.into_body()).await;
    assert_eq!(page.items[0].name, "analyst");
    assert!(!page.items[0].can_login);
    assert_eq!(page.next_offset, Some(1));
    assert!(!serde_json::to_string(&page).unwrap().contains("password"));
}

#[tokio::test]
async fn role_creation_requires_preview_and_production_confirmation() {
    let state = Row::new(vec![
        Value::Text("false".into()),
        Value::Text("true".into()),
    ]);
    let driver = pg_driver()
        .execute_ok(rows(vec![state.clone()]))
        .execute_ok(rows(vec![state]))
        .execute_ok(rows(Vec::new()))
        .build();
    let (router, session, connection) = setup(driver).await;
    let base = format!("/v1/sessions/{session}/connections/{connection}/postgres/objects");
    let action = serde_json::json!({"kind":"create_role","name":"reader\"; DROP ROLE x; --"});
    let response = router
        .clone()
        .oneshot(post(format!("{base}/preview"), action.clone()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let preview: PostgresObjectPreview = json(response.into_body()).await;
    assert_eq!(
        preview.sql,
        "CREATE ROLE \"reader\"\"; DROP ROLE x; --\" NOLOGIN"
    );
    let denied = router
        .clone()
        .oneshot(post(
            format!("{base}/apply"),
            serde_json::json!({
                "action": action, "precondition": preview.precondition, "confirmed": true,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::BAD_REQUEST);
    let applied = router
        .oneshot(post(
            format!("{base}/apply"),
            serde_json::json!({
                "action": preview.action, "precondition": preview.precondition,
                "confirmed": true, "production_confirmed": true,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(applied.status(), StatusCode::OK);
}

#[tokio::test]
async fn schema_grant_preview_requires_owner_authority() {
    let driver = pg_driver()
        .execute_ok(rows(vec![Row::new(vec![
            Value::Text("17".into()),
            Value::Text("".into()),
            Value::Text("false".into()),
            Value::Text("true".into()),
        ])]))
        .build();
    let (router, session, connection) = setup(driver).await;
    let response = router.oneshot(post(format!("/v1/sessions/{session}/connections/{connection}/postgres/objects/preview"), serde_json::json!({
        "kind":"grant_schema_privilege", "schema":"public", "grantee":"analyst", "privilege":"usage",
    }))).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

fn policy_state(owner: bool, target_exists: bool, expression_hash: &str) -> Row {
    Row::new(vec![
        Value::Text("123".into()),
        Value::Text("456".into()),
        Value::Text("own_rows".into()),
        Value::Text(expression_hash.into()),
        Value::Text("check_hash".into()),
        Value::Text("{0}".into()),
        Value::Text("*".into()),
        Value::Text("true".into()),
        Value::Text("true".into()),
        Value::Text("false".into()),
        Value::Text("42".into()),
        Value::Text(owner.to_string()),
        Value::Text(target_exists.to_string()),
    ])
}

#[tokio::test]
async fn policy_rename_requires_owner_current_state_and_production_confirmation() {
    let driver = pg_driver()
        .execute_ok(rows(vec![policy_state(false, false, "hash")]))
        .execute_ok(rows(vec![policy_state(true, false, "hash")]))
        .execute_ok(rows(vec![policy_state(true, false, "changed")]))
        .build();
    let (router, session, connection) = setup(driver).await;
    let base = format!("/v1/sessions/{session}/connections/{connection}/postgres/objects");
    let action = serde_json::json!({"kind":"rename_policy", "schema":"public", "table":"items", "name":"own_rows", "new_name":"own_rows_v2"});
    let forbidden = router
        .clone()
        .oneshot(post(format!("{base}/preview"), action.clone()))
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    let preview_response = router
        .clone()
        .oneshot(post(format!("{base}/preview"), action.clone()))
        .await
        .unwrap();
    assert_eq!(preview_response.status(), StatusCode::OK);
    let preview: PostgresObjectPreview = json(preview_response.into_body()).await;
    assert_eq!(
        preview.sql,
        "ALTER POLICY \"own_rows\" ON \"public\".\"items\" RENAME TO \"own_rows_v2\""
    );
    let unconfirmed = router.clone().oneshot(post(format!("{base}/apply"), serde_json::json!({"action":action,"precondition":preview.precondition,"confirmed":true}))).await.unwrap();
    assert_eq!(unconfirmed.status(), StatusCode::BAD_REQUEST);
    let stale = router.oneshot(post(format!("{base}/apply"), serde_json::json!({"action":preview.action,"precondition":preview.precondition,"confirmed":true,"production_confirmed":true}))).await.unwrap();
    assert_eq!(stale.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn policy_list_is_bounded_and_expressions_are_display_only() {
    let driver = pg_driver()
        .execute_ok(rows(vec![Row::new(vec![
            Value::Text("public".into()),
            Value::Text("items".into()),
            Value::Text("own_rows".into()),
            Value::Text("*".into()),
            Value::Text("true".into()),
            Value::Text("PUBLIC".into()),
            Value::Text("false".into()),
            Value::Text("owner_id = current_user".into()),
            Value::Text("true".into()),
            Value::Null,
            Value::Text("false".into()),
            Value::Text("true".into()),
            Value::Text("false".into()),
        ])]))
        .build();
    let (router, session, connection) = setup(driver).await;
    let path = format!("/v1/sessions/{session}/connections/{connection}/postgres/policies");
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
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let page: PostgresObjectPage<sift_protocol::PostgresPolicy> = json(response.into_body()).await;
    assert_eq!(
        page.items[0].using_expression.as_deref(),
        Some("owner_id = current_user")
    );
    assert!(page.items[0].row_security_enabled);
    assert!(page.items[0].using_truncated);
}

#[tokio::test]
async fn policy_rename_applies_only_reviewed_quoted_identifiers() {
    let state = policy_state(true, false, "hash");
    let driver = pg_driver()
        .execute_ok(rows(vec![state.clone()]))
        .execute_ok(rows(vec![state]))
        .execute_ok(rows(Vec::new()))
        .build();
    let (router, session, connection) = setup(driver).await;
    let base = format!("/v1/sessions/{session}/connections/{connection}/postgres/objects");
    let action = serde_json::json!({"kind":"rename_policy", "schema":"public", "table":"items", "name":"own_rows", "new_name":"owner\"; DROP TABLE x; --"});
    let response = router
        .clone()
        .oneshot(post(format!("{base}/preview"), action.clone()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let preview: PostgresObjectPreview = json(response.into_body()).await;
    assert_eq!(preview.sql, "ALTER POLICY \"own_rows\" ON \"public\".\"items\" RENAME TO \"owner\"\"; DROP TABLE x; --\"");
    let applied = router
        .oneshot(post(
            format!("{base}/apply"),
            serde_json::json!({
                "action": action, "precondition": preview.precondition,
                "confirmed": true, "production_confirmed": true,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(applied.status(), StatusCode::OK);
}
