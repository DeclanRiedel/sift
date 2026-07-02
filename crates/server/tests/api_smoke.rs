//! Server integration tests against `MockDriver`. No real DB required —
//! these exercise the axum surface end-to-end via tower::ServiceExt::oneshot.

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use sift_driver_api::mock::MockDriver;
use sift_protocol::{
    ColumnMetadata, ConnectionSpec, Engine, ExecuteRequestHttp, Health, Nullability, Page,
    PrimitiveType, Row, SchemaScope, SchemaSnapshot, ServerInfo, SslMode, TypeRef, Value,
};
use sift_server::http::{app, AppState, AuthState};
use sift_server::registry::DriverRegistry;
use sift_server::session::SessionStore;
use tower::ServiceExt;

fn mock_postgres_driver() -> MockDriver {
    let columns = vec![
        ColumnMetadata {
            name: "id".into(),
            type_ref: TypeRef::Primitive(PrimitiveType::Int32),
            nullable: Nullability::NotNullable,
            auto_increment: false,
            primary_key: false,
            facets: Default::default(),
        },
        ColumnMetadata {
            name: "name".into(),
            type_ref: TypeRef::Primitive(PrimitiveType::Text),
            nullable: Nullability::Nullable,
            auto_increment: false,
            primary_key: false,
            facets: Default::default(),
        },
    ];
    let rows = vec![
        Row::new(vec![Value::Int32(1), Value::Text("alice".into())]),
        Row::new(vec![Value::Int32(2), Value::Text("bob".into())]),
    ];
    MockDriver::builder()
        .engine(Engine::Postgres)
        .ping_ok(ServerInfo {
            engine: Engine::Postgres,
            server_version: "MockDB 0.1".into(),
            current_database: "mock".into(),
            current_user: "mock".into(),
        })
        .schema_ok(SchemaSnapshot::empty(SchemaScope::shallow()))
        .execute_ok(vec![
            Page::NextResult { columns },
            Page::Rows { rows },
            Page::Done {
                affected_rows: Some(2),
                warnings: Vec::new(),
            },
        ])
        .build()
}

fn test_state() -> AppState {
    let registry = DriverRegistry::builder()
        .register(mock_postgres_driver())
        .build();
    AppState {
        sessions: SessionStore::new(registry),
        auth: AuthState::default(),
    }
}

fn test_state_with_driver(driver: MockDriver) -> AppState {
    let registry = DriverRegistry::builder().register(driver).build();
    AppState {
        sessions: SessionStore::new(registry),
        auth: AuthState::default(),
    }
}

fn test_state_with_token(token: &str) -> AppState {
    let registry = DriverRegistry::builder()
        .register(mock_postgres_driver())
        .build();
    AppState {
        sessions: SessionStore::new(registry),
        auth: AuthState {
            bearer_token: Some(token.to_string()),
        },
    }
}

fn test_state_with_operation_log(path: &std::path::Path) -> AppState {
    let registry = DriverRegistry::builder()
        .register(mock_postgres_driver())
        .build();
    AppState {
        sessions: SessionStore::new_with_operation_log_path(registry, path)
            .expect("operation log opens"),
        auth: AuthState::default(),
    }
}

fn pg_spec() -> ConnectionSpec {
    ConnectionSpec {
        host: "mock.invalid".into(),
        port: Some(5432),
        database: Some("mock".into()),
        user: "mock".into(),
        password: None,
        ssl_mode: Some(SslMode::Disable),
        engine_specific: None,
    }
}

async fn body_json<T: serde::de::DeserializeOwned>(body: Body) -> T {
    let bytes = to_bytes(body, 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|e| panic!("decode body: {e}; {}", String::from_utf8_lossy(&bytes)))
}

/// Build a POST request with a JSON body and the right content-type.
fn post_json(uri: impl Into<String>, body: impl serde::Serialize) -> Request<Body> {
    Request::post(uri.into())
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn post_json_str(uri: impl Into<String>, body: &str) -> Request<Body> {
    Request::post(uri.into())
        .header("content-type", "application/json")
        .body(Body::from(body.to_owned()))
        .unwrap()
}

#[tokio::test]
async fn health_lists_registered_engines() {
    let app = app(test_state());
    let res = app
        .oneshot(Request::get("/v1/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()
            .get("x-sift-protocol-version")
            .and_then(|h| h.to_str().ok()),
        Some(sift_protocol::PROTOCOL_VERSION)
    );
    let health: Health = body_json(res.into_body()).await;
    assert_eq!(health.status, "ok");
    assert!(health.engines.contains(&Engine::Postgres));
}

#[tokio::test]
async fn openapi_is_published() {
    let app = app(test_state());
    let res = app
        .oneshot(
            Request::get("/v1/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: serde_json::Value = body_json(res.into_body()).await;
    assert_eq!(body["openapi"], "3.1.0");
    assert_eq!(
        body["x-sift-protocol-version"],
        sift_protocol::PROTOCOL_VERSION
    );
    assert!(body["paths"]["/v1/sessions/{id}/ws"].is_object());
    assert!(body["paths"]["/v1/sessions/{id}/transactions"].is_object());
    assert!(body["paths"]["/v1/audit"].is_object());
    assert!(body["paths"]["/v1/operations"].is_object());
    assert!(body["components"]["securitySchemes"]["bearerAuth"].is_object());
    assert!(body["components"]["schemas"]["ExecuteResponse"].is_object());
    assert!(body["components"]["schemas"]["ExecuteResponse"]["properties"]["rows"].is_object());
    assert!(
        body["components"]["schemas"]["OpenConnectionRequest"]["properties"]["engine"].is_object()
    );
    assert!(body["components"]["schemas"]["Page"].is_object());
    assert!(
        body["components"]["schemas"]["OperationAuditEntry"]["properties"]["operation"].is_object()
    );
}

#[tokio::test]
async fn audit_records_http_operations() {
    let app = app(test_state());
    let res = app
        .clone()
        .oneshot(Request::get("/v1/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app
        .oneshot(Request::get("/v1/audit").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let rows: Vec<sift_protocol::AuditEntry> = body_json(res.into_body()).await;
    assert!(rows
        .iter()
        .any(|r| r.method == "GET" && r.path == "/v1/health"));
}

#[tokio::test]
async fn operation_log_records_replayable_operations() {
    let app = app(test_state());
    let res = app
        .clone()
        .oneshot(post_json_str("/v1/sessions", r#"{"tag":"ops"}"#))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app
        .oneshot(Request::get("/v1/operations").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let rows: Vec<sift_protocol::OperationAuditEntry> = body_json(res.into_body()).await;
    assert!(rows.iter().any(|row| matches!(
        &row.operation,
        sift_protocol::Operation::OpenSession { request }
            if request.tag.as_deref() == Some("ops")
    )));
}

#[tokio::test]
async fn operation_log_replays_from_disk() {
    let path = std::env::temp_dir().join(format!(
        "sift-operation-log-{}.jsonl",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));

    let app = app(test_state_with_operation_log(&path));
    let res = app
        .oneshot(post_json_str("/v1/sessions", r#"{"tag":"durable"}"#))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let replayed = test_state_with_operation_log(&path)
        .sessions
        .list_operations();
    assert!(replayed.iter().any(|row| matches!(
        &row.operation,
        sift_protocol::Operation::OpenSession { request }
            if request.tag.as_deref() == Some("durable")
    )));

    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn client_sdk_consumes_public_http_api() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app(test_state()).into_make_service())
            .await
            .unwrap();
    });

    let client = sift_client_sdk::Client::new(format!("http://{addr}"));
    let health = client.health().await.unwrap();
    assert!(health.engines.contains(&Engine::Postgres));

    let session = client.open_session(Some("sdk".into())).await.unwrap();
    let conn = client
        .open_connection(
            session.id,
            sift_protocol::OpenConnectionRequest {
                engine: Engine::Postgres,
                spec: pg_spec(),
            },
        )
        .await
        .unwrap();
    let result = client
        .execute(session.id, conn.id, "SELECT id, name FROM users")
        .await
        .unwrap();
    assert_eq!(result.rows.len(), 2);
    let audit = client.audit().await.unwrap();
    assert!(audit.iter().any(|row| row.path == "/v1/health"));

    server.abort();
}

#[tokio::test]
async fn client_sdk_consumes_public_websocket_api() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app(test_state()).into_make_service())
            .await
            .unwrap();
    });

    let client = sift_client_sdk::Client::new(format!("http://{addr}"));
    let session = client.open_session(Some("sdk-ws".into())).await.unwrap();
    let conn = client
        .open_connection(
            session.id,
            sift_protocol::OpenConnectionRequest {
                engine: Engine::Postgres,
                spec: pg_spec(),
            },
        )
        .await
        .unwrap();

    let pages = client
        .stream_query(session.id, conn.id, "SELECT id, name FROM users")
        .await
        .unwrap();
    assert!(pages
        .iter()
        .any(|page| matches!(page, Page::NextResult { columns } if columns.len() == 2)));
    assert!(pages
        .iter()
        .any(|page| matches!(page, Page::Rows { rows } if rows.len() == 2)));
    assert!(pages.iter().any(|page| matches!(page, Page::Done { .. })));

    server.abort();
}

#[tokio::test]
async fn bearer_token_auth_is_enforced_when_configured() {
    let app = app(test_state_with_token("secret"));
    let res = app
        .clone()
        .oneshot(Request::get("/v1/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let res = app
        .oneshot(
            Request::get("/v1/health")
                .header("authorization", "Bearer secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn session_lifecycle_create_list_close() {
    let app = app(test_state());

    // Create.
    let res = app
        .clone()
        .oneshot(post_json_str("/v1/sessions", r#"{"tag":"test"}"#))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let info: sift_protocol::SessionInfo = body_json(res.into_body()).await;
    assert_eq!(info.tag.as_deref(), Some("test"));
    let id = info.id;

    // List.
    let res = app
        .clone()
        .oneshot(Request::get("/v1/sessions").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let list: Vec<sift_protocol::SessionInfo> = body_json(res.into_body()).await;
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, id);

    // Get.
    let res = app
        .clone()
        .oneshot(
            Request::get(format!("/v1/sessions/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // 404 for unknown.
    let res = app
        .clone()
        .oneshot(
            Request::get("/v1/sessions/9999")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    // Close.
    let res = app
        .oneshot(
            Request::delete(format!("/v1/sessions/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn connection_open_ping_close() {
    let app = app(test_state());

    // Create session first.
    let res = app
        .clone()
        .oneshot(
            Request::post("/v1/sessions")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    let session: sift_protocol::SessionInfo = body_json(res.into_body()).await;
    let sid = session.id;

    // Open connection.
    let open_req = serde_json::json!({
        "engine": "postgres",
        "host": "mock.invalid",
        "port": 5432,
        "database": "mock",
        "user": "mock",
        "ssl_mode": "disable",
    });
    let res = app
        .clone()
        .oneshot(post_json(
            format!("/v1/sessions/{sid}/connections"),
            open_req,
        ))
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "open_connection should succeed"
    );
    let conn: sift_protocol::ConnectionInfo = body_json(res.into_body()).await;
    assert_eq!(conn.engine, Engine::Postgres);
    let cid = conn.id;

    // List.
    let res = app
        .clone()
        .oneshot(
            Request::get(format!("/v1/sessions/{sid}/connections"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list: Vec<sift_protocol::ConnectionInfo> = body_json(res.into_body()).await;
    assert_eq!(list.len(), 1);

    // Ping.
    let res = app
        .clone()
        .oneshot(
            Request::post(format!("/v1/sessions/{sid}/connections/{cid}/ping"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let info: ServerInfo = body_json(res.into_body()).await;
    assert_eq!(info.engine, Engine::Postgres);

    // Schema.
    let res = app
        .clone()
        .oneshot(
            Request::get(format!("/v1/sessions/{sid}/connections/{cid}/schema"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // Close connection.
    let res = app
        .clone()
        .oneshot(
            Request::delete(format!("/v1/sessions/{sid}/connections/{cid}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // 404 after close.
    let res = app
        .oneshot(
            Request::post(format!("/v1/sessions/{sid}/connections/{cid}/ping"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn execute_returns_drained_rows_and_affected_count() {
    let app = app(test_state());

    let session: sift_protocol::SessionInfo = body_json(
        app.clone()
            .oneshot(post_json_str("/v1/sessions", "{}"))
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let sid = session.id;

    let open_req = serde_json::json!({
        "engine": "postgres",
        "host": "mock.invalid",
        "port": 5432,
        "database": "mock",
        "user": "mock",
        "ssl_mode": "disable",
    });
    let conn: sift_protocol::ConnectionInfo = body_json(
        app.clone()
            .oneshot(post_json(
                format!("/v1/sessions/{sid}/connections"),
                open_req,
            ))
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let cid = conn.id;

    let exec_req = ExecuteRequestHttp {
        connection: cid,
        sql: "SELECT id, name FROM users".into(),
        tx: None,
    };
    let res = app
        .oneshot(post_json(format!("/v1/sessions/{sid}/queries"), exec_req))
        .await
        .unwrap();
    let status = res.status();
    let body_bytes = to_bytes(res.into_body(), 1024 * 1024).await.unwrap();
    assert_eq!(
        status,
        StatusCode::OK,
        "execute failed: {}",
        String::from_utf8_lossy(&body_bytes)
    );
    let resp: sift_protocol::ExecuteResponse =
        serde_json::from_slice(&body_bytes).expect("decode ExecuteResponse");
    assert_eq!(resp.columns.len(), 2);
    assert_eq!(resp.columns[0].name, "id");
    assert_eq!(resp.rows.len(), 2);
    assert!(matches!(&resp.rows[0].values[0], Value::Int32(1)));
    assert_eq!(resp.affected_rows, Some(2));
    assert!(!resp.has_more);
}

#[tokio::test]
async fn transaction_flow_requires_explicit_tx_ref() {
    let app = app(test_state());

    let session: sift_protocol::SessionInfo = body_json(
        app.clone()
            .oneshot(post_json_str("/v1/sessions", "{}"))
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let sid = session.id;

    let conn: sift_protocol::ConnectionInfo = body_json(
        app.clone()
            .oneshot(post_json(
                format!("/v1/sessions/{sid}/connections"),
                serde_json::json!({
                    "engine": "postgres",
                    "host": "mock.invalid",
                    "port": 5432,
                    "database": "mock",
                    "user": "mock",
                    "ssl_mode": "disable",
                }),
            ))
            .await
            .unwrap()
            .into_body(),
    )
    .await;

    let tx: sift_protocol::TransactionInfo = body_json(
        app.clone()
            .oneshot(post_json(
                format!("/v1/sessions/{sid}/transactions"),
                sift_protocol::BeginTransactionRequest {
                    connection: conn.id,
                    mode: sift_protocol::TxMode::default(),
                },
            ))
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    assert_eq!(tx.connection, conn.id);

    let res = app
        .clone()
        .oneshot(post_json(
            format!("/v1/sessions/{sid}/queries"),
            ExecuteRequestHttp {
                connection: conn.id,
                sql: "SELECT id, name FROM users".into(),
                tx: None,
            },
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let res = app
        .clone()
        .oneshot(post_json(
            format!("/v1/sessions/{sid}/queries"),
            ExecuteRequestHttp {
                connection: conn.id,
                sql: "SELECT id, name FROM users".into(),
                tx: Some(sift_protocol::TxHandleRef {
                    tx_id: tx.tx_id,
                    connection: conn.id,
                    mode: tx.mode,
                }),
            },
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app
        .oneshot(post_json(
            format!("/v1/sessions/{sid}/transactions/{}/commit", tx.tx_id),
            sift_protocol::EndTransactionRequest {
                connection: conn.id,
                tx_id: tx.tx_id,
            },
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn unregistered_engine_yields_422() {
    let app = app(test_state());

    let session: sift_protocol::SessionInfo = body_json(
        app.clone()
            .oneshot(post_json_str("/v1/sessions", "{}"))
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let sid = session.id;

    let res = app
        .oneshot(post_json(
            format!("/v1/sessions/{sid}/connections"),
            serde_json::json!({
                "engine": "sql_server",
                "host": "mock.invalid",
                "user": "mock",
            }),
        ))
        .await
        .unwrap();
    // This app registers only the mock Postgres driver; SQL Server is
    // therefore rejected at registry lookup and mapped to 422.
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn cancel_rejects_cursor_body_path_mismatch() {
    let app = app(test_state());

    let res = app
        .oneshot(post_json(
            "/v1/sessions/1/queries/10/cancel",
            serde_json::json!({
                "connection": 1,
                "cursor": 11,
            }),
        ))
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn execute_stream_error_maps_to_http_error() {
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .execute_err(sift_protocol::DriverError::new(
            sift_protocol::Code::SyntaxError,
            "bad sql",
        ))
        .build();
    let app = app(test_state_with_driver(driver));

    let session: sift_protocol::SessionInfo = body_json(
        app.clone()
            .oneshot(post_json_str("/v1/sessions", "{}"))
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let sid = session.id;

    let conn: sift_protocol::ConnectionInfo = body_json(
        app.clone()
            .oneshot(post_json(
                format!("/v1/sessions/{sid}/connections"),
                serde_json::json!({
                    "engine": "postgres",
                    "host": "mock.invalid",
                    "user": "mock",
                }),
            ))
            .await
            .unwrap()
            .into_body(),
    )
    .await;

    let res = app
        .oneshot(post_json(
            format!("/v1/sessions/{sid}/queries"),
            ExecuteRequestHttp {
                connection: conn.id,
                sql: "BAD".into(),
                tx: None,
            },
        ))
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

// Silence unused imports for the spec helper (used only when test wants a
// canonical ConnectionSpec; kept for future tests).
#[allow(dead_code)]
fn _pg_spec_marker() -> ConnectionSpec {
    pg_spec()
}
