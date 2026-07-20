//! Durable operation audit (Phase B reliability step 4).
//!
//! Drives the axum surface against a mock driver with a metadata store wired
//! as the audit sink, and asserts that operations land in the durable
//! `/v1/operations/audit` log with actor/target/result/row-count — and that a
//! failed query records the failure without leaking SQL text or bind values.

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use sift_driver_api::mock::MockDriver;
use sift_metadata::{MemorySecretStore, MetadataStore, OperationAudit};
use sift_protocol::{
    Code, ColumnMetadata, DriverError, Engine, Nullability, Page, PrimitiveType, Row, SessionInfo,
    TypeRef, Value,
};
use sift_server::http::{app, AppState, AuthState};
use sift_server::registry::DriverRegistry;
use sift_server::room_runtime::RoomRuntime;
use sift_server::session::SessionStore;
use sift_server::shutdown::Shutdown;
use std::sync::Arc;
use tower::ServiceExt;

fn success_pages() -> Vec<Page> {
    vec![
        Page::NextResult {
            columns: vec![ColumnMetadata {
                name: "id".into(),
                type_ref: TypeRef::Primitive(PrimitiveType::Int32),
                nullable: Nullability::NotNullable,
                auto_increment: false,
                primary_key: false,
                facets: Default::default(),
            }],
        },
        Page::Rows {
            rows: vec![
                Row::new(vec![Value::Int32(1)]),
                Row::new(vec![Value::Int32(2)]),
            ],
        },
        Page::Done {
            affected_rows: Some(2),
            warnings: Vec::new(),
        },
    ]
}

/// State with a metadata store wired as the durable audit sink, like `main`.
///
/// Loopback auth bypass is on (the production default). In-process `oneshot`
/// requests are treated as loopback, so session/connection/metadata calls
/// resolve to the bootstrapped local principal (id 1) — which routes now
/// require when a metadata store is configured.
fn audited_state(driver: MockDriver) -> AppState {
    let registry = DriverRegistry::builder().register(driver).build();
    let sessions = SessionStore::new(registry);
    let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
    metadata.bootstrap_local("local user").unwrap();
    sessions.set_audit_store(metadata.clone());
    AppState {
        sessions,
        rooms: RoomRuntime::default(),
        auth: AuthState {
            bearer_token: None,
            loopback_bypass: true,
            deployment: Default::default(),
        },
        metadata: Some(metadata),
        shutdown: Shutdown::default(),
    }
}

fn post(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::post(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

async fn body_json<T: serde::de::DeserializeOwned>(body: Body) -> T {
    let bytes = to_bytes(body, 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn open_session_and_connection(app: &axum::Router) -> (i64, i64) {
    let res = app
        .clone()
        .oneshot(post("/v1/sessions", serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let session: SessionInfo = body_json(res.into_body()).await;

    let res = app
        .clone()
        .oneshot(post(
            &format!("/v1/sessions/{}/connections", session.id),
            serde_json::json!({ "engine": "postgres", "host": "mock", "user": "mock" }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let conn: sift_protocol::ConnectionInfo = body_json(res.into_body()).await;
    (session.id.0 as i64, conn.id.0 as i64)
}

async fn audit_rows(app: &axum::Router) -> Vec<OperationAudit> {
    let res = app
        .clone()
        .oneshot(
            Request::get("/v1/operations/audit")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    body_json(res.into_body()).await
}

/// Durable audit is written on a background thread, so poll until the row we
/// expect has been flushed (FIFO ordering means earlier rows are present too),
/// then return the full set.
async fn audit_rows_where(
    app: &axum::Router,
    predicate: impl Fn(&OperationAudit) -> bool,
) -> Vec<OperationAudit> {
    for _ in 0..200 {
        let rows = audit_rows(app).await;
        if rows.iter().any(&predicate) {
            return rows;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for expected audit row");
}

#[tokio::test]
async fn successful_query_is_audited_with_row_count() {
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .execute_ok(success_pages())
        .build();
    let app = app(audited_state(driver));
    let (session_id, conn_id) = open_session_and_connection(&app).await;

    let res = app
        .clone()
        .oneshot(post(
            &format!("/v1/sessions/{session_id}/queries"),
            serde_json::json!({ "connection": conn_id, "sql": "select id from t" }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let rows = audit_rows_where(&app, |r| r.action == "execute" && r.target == "query").await;
    // Session open, connection open, and the query all recorded.
    assert!(rows
        .iter()
        .any(|r| r.action == "open" && r.target == "session"));
    assert!(rows
        .iter()
        .any(|r| r.action == "open" && r.target == "connection"));
    let query = rows
        .iter()
        .find(|r| r.action == "execute" && r.target == "query")
        .expect("query op audited");
    assert_eq!(query.status, "succeeded");
    assert_eq!(query.row_count, Some(2));
    assert_eq!(query.error_message, None);
}

#[tokio::test]
async fn query_audit_carries_client_correlation_id() {
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .execute_ok(success_pages())
        .build();
    let app = app(audited_state(driver));
    let (session_id, conn_id) = open_session_and_connection(&app).await;

    let request = Request::post(format!("/v1/sessions/{session_id}/queries"))
        .header("content-type", "application/json")
        .header("x-correlation-id", "corr-abc-123")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "connection": conn_id,
                "sql": "select id from t"
            }))
            .unwrap(),
        ))
        .unwrap();
    let res = app.clone().oneshot(request).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    // The correlation ID is echoed back to the client.
    assert_eq!(
        res.headers()
            .get("x-correlation-id")
            .and_then(|v| v.to_str().ok()),
        Some("corr-abc-123")
    );

    let rows = audit_rows_where(&app, |r| r.action == "execute" && r.target == "query").await;
    let query = rows
        .iter()
        .find(|r| r.action == "execute" && r.target == "query")
        .expect("query op audited");
    assert_eq!(query.correlation_id.as_deref(), Some("corr-abc-123"));
}

#[tokio::test]
async fn response_generates_correlation_id_when_absent() {
    let app = app(audited_state(
        MockDriver::builder().engine(Engine::Postgres).build(),
    ));
    let res = app
        .oneshot(post("/v1/sessions", serde_json::json!({})))
        .await
        .unwrap();
    let id = res
        .headers()
        .get("x-correlation-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .expect("correlation id present");
    assert!(!id.is_empty());
}

#[tokio::test]
async fn operation_trail_is_fingerprinted_and_secret_free() {
    let secret = "hunter2";
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .execute_ok(success_pages())
        .build();
    let app = app(audited_state(driver));

    // Open a session and a connection *with a password*.
    let res = app
        .clone()
        .oneshot(post("/v1/sessions", serde_json::json!({})))
        .await
        .unwrap();
    let session: SessionInfo = body_json(res.into_body()).await;
    let res = app
        .clone()
        .oneshot(post(
            &format!("/v1/sessions/{}/connections", session.id),
            serde_json::json!({
                "engine": "postgres", "host": "mock", "user": "mock", "password": secret
            }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let conn: sift_protocol::ConnectionInfo = body_json(res.into_body()).await;

    // Execute a query whose SQL embeds the secret.
    let res = app
        .clone()
        .oneshot(post(
            &format!("/v1/sessions/{}/queries", session.id),
            serde_json::json!({
                "connection": conn.id,
                "sql": format!("select * from t where token = '{secret}'")
            }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // The replayable operation trail (/v1/operations) is synchronous.
    let res = app
        .oneshot(Request::get("/v1/operations").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let ops: Vec<serde_json::Value> = body_json(res.into_body()).await;

    let exec = ops
        .iter()
        .find(|e| e["operation"]["op"] == "execute_query")
        .expect("execute_query recorded");
    let sql = exec["operation"]["request"]["sql"].as_str().unwrap();
    assert!(
        sql.starts_with("sqlfp:"),
        "SQL should be fingerprinted, got {sql}"
    );
    assert_eq!(
        exec["operation"]["request"]["params"],
        serde_json::json!([])
    );

    let open = ops
        .iter()
        .find(|e| e["operation"]["op"] == "open_connection")
        .expect("open_connection recorded");
    assert!(
        open["operation"]["request"]["password"].is_null(),
        "connection password must be redacted"
    );

    // Belt and suspenders: the secret appears nowhere in the trail.
    let whole = serde_json::to_string(&ops).unwrap();
    assert!(!whole.contains(secret), "operation trail leaked a secret");
}

#[tokio::test]
async fn metadata_operation_records_actor() {
    let app = app(audited_state(
        MockDriver::builder().engine(Engine::Postgres).build(),
    ));
    let res = app
        .clone()
        .oneshot(post(
            "/v1/metadata/rooms",
            serde_json::json!({ "tenant_id": 1, "name": "room-a", "kind": "personal" }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let rows = audit_rows_where(&app, |r| r.action == "create" && r.target == "room").await;
    let created = rows
        .iter()
        .find(|r| r.action == "create" && r.target == "room")
        .expect("room create audited");
    assert_eq!(
        created.actor_principal_id,
        Some(sift_metadata::PrincipalId(1))
    );
}

#[tokio::test]
async fn failed_query_records_failure_without_leaking_sql() {
    let secret_literal = "hunter2";
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .execute_err(DriverError::new(
            Code::SyntaxError,
            "syntax error near WHERE",
        ))
        .build();
    let app = app(audited_state(driver));
    let (session_id, conn_id) = open_session_and_connection(&app).await;

    let res = app
        .clone()
        .oneshot(post(
            &format!("/v1/sessions/{session_id}/queries"),
            // SQL carries a literal secret; it must never reach the audit row.
            serde_json::json!({
                "connection": conn_id,
                "sql": format!("select * from t where token = '{secret_literal}'")
            }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let rows = audit_rows_where(&app, |r| r.action == "execute" && r.target == "query").await;
    let query = rows
        .iter()
        .find(|r| r.action == "execute" && r.target == "query")
        .expect("failed query op audited");
    assert_eq!(query.status, "failed");
    assert_eq!(query.result_code.as_deref(), Some("syntax error"));
    assert!(query.error_message.is_some());
    // No column carries the SQL text or its embedded secret.
    for row in &rows {
        assert!(!row.action.contains(secret_literal));
        assert!(!row.target.contains(secret_literal));
        assert!(!row
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains(secret_literal));
    }
}
