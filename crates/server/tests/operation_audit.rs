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
fn audited_state(driver: MockDriver) -> AppState {
    let registry = DriverRegistry::builder().register(driver).build();
    let sessions = SessionStore::new(registry);
    let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
    metadata.bootstrap_local("local user").unwrap();
    sessions.set_audit_store(metadata.clone());
    AppState {
        sessions,
        rooms: RoomRuntime::default(),
        auth: AuthState::default(),
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

    let rows = audit_rows(&app).await;
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

    let rows = audit_rows(&app).await;
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
