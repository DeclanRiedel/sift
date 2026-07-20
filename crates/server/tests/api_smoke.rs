//! Server integration tests against `MockDriver`. No real DB required —
//! these exercise the axum surface end-to-end via tower::ServiceExt::oneshot.

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use sift_driver_api::{mock::MockDriver, BulkResult, PgNotification};
use sift_metadata::{
    CrdtType, CredentialMode, MembershipRole, MemorySecretStore, MetadataStore,
    NewConnectionProfile, NewDocument, NewOperationAudit, NewPasswordPrincipal, NewRoom,
    PrincipalId, RoomKind, RoomRole, TenantId, TenantKind,
};
use sift_protocol::{
    AuthTokensResponse, ColumnMetadata, ConnectionSpec, Engine, ExecuteRequestHttp, Health,
    Nullability, Page, PrimitiveType, RoomClientMessage, RoomServerMessage, Row, SchemaScope,
    SchemaSnapshot, ServerInfo, SslMode, TextDocumentOperation, TypeRef, Value, WebAuthResponse,
    WhoAmIResponse,
};
use sift_server::http::{app, AppState, AuthState};
use sift_server::registry::DriverRegistry;
use sift_server::room_runtime::RoomRuntime;
use sift_server::session::SessionStore;
use std::sync::Arc;
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
            pool_warm_slots: None,
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
        rooms: RoomRuntime::default(),
        shutdown: sift_server::shutdown::Shutdown::default(),
        auth: AuthState::default(),
        metadata: None,
    }
}

fn ws_json<T: serde::de::DeserializeOwned>(message: tokio_tungstenite::tungstenite::Message) -> T {
    match message {
        tokio_tungstenite::tungstenite::Message::Text(text) => serde_json::from_str(&text).unwrap(),
        tokio_tungstenite::tungstenite::Message::Binary(bytes) => {
            serde_json::from_slice(&bytes).unwrap()
        }
        other => panic!("unexpected websocket message: {other:?}"),
    }
}

fn test_state_with_driver(driver: MockDriver) -> AppState {
    let registry = DriverRegistry::builder().register(driver).build();
    AppState {
        sessions: SessionStore::new(registry),
        rooms: RoomRuntime::default(),
        shutdown: sift_server::shutdown::Shutdown::default(),
        auth: AuthState::default(),
        metadata: None,
    }
}

fn test_state_with_token(token: &str) -> AppState {
    let registry = DriverRegistry::builder()
        .register(mock_postgres_driver())
        .build();
    AppState {
        sessions: SessionStore::new(registry),
        rooms: RoomRuntime::default(),
        shutdown: sift_server::shutdown::Shutdown::default(),
        auth: AuthState {
            bearer_token: Some(token.to_string()),
            loopback_bypass: false,
            deployment: Default::default(),
            ..Default::default()
        },
        metadata: None,
    }
}

fn test_state_with_operation_log(path: &std::path::Path) -> AppState {
    let registry = DriverRegistry::builder()
        .register(mock_postgres_driver())
        .build();
    AppState {
        sessions: SessionStore::new_with_operation_log_path(registry, path)
            .expect("operation log opens"),
        rooms: RoomRuntime::default(),
        shutdown: sift_server::shutdown::Shutdown::default(),
        auth: AuthState::default(),
        metadata: None,
    }
}

fn test_state_with_metadata(loopback_bypass: bool) -> AppState {
    let registry = DriverRegistry::builder()
        .register(mock_postgres_driver())
        .build();
    let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
    metadata.bootstrap_local("local user").unwrap();
    AppState {
        sessions: SessionStore::new(registry),
        rooms: RoomRuntime::default(),
        shutdown: sift_server::shutdown::Shutdown::default(),
        auth: AuthState {
            bearer_token: None,
            loopback_bypass,
            deployment: Default::default(),
            ..Default::default()
        },
        metadata: Some(metadata),
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

fn mssql_spec() -> ConnectionSpec {
    ConnectionSpec {
        host: "mock.invalid".into(),
        port: Some(1433),
        database: Some("mock".into()),
        user: "mock".into(),
        password: Some("mock".into()),
        ssl_mode: Some(SslMode::Require),
        engine_specific: Some(sift_protocol::EngineConnectionSpec::SqlServer(
            sift_protocol::MssqlConnectionSpec {
                mars: false,
                encrypt: Some(true),
                trust_server_certificate: Some(true),
                connect_timeout_secs: Some(15),
                pool_min_size: None,
            },
        )),
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
    assert!(body["paths"]["/v1/sessions/{id}/transactions/{tx_id}/savepoints"].is_object());
    assert!(
        body["paths"]["/v1/sessions/{id}/transactions/{tx_id}/savepoints/rollback"].is_object()
    );
    assert!(body["paths"]["/v1/sessions/{id}/transactions/{tx_id}/savepoints/release"].is_object());
    assert!(body["paths"]["/v1/sessions/{id}/connections/{conn_id}/bulk-insert"].is_object());
    assert!(body["paths"]["/v1/ready"].is_object());
    assert_eq!(
        body["paths"]["/v1/ready"]["get"]["responses"]["503"]["content"]["application/json"]
            ["schema"]["$ref"],
        "#/components/schemas/Readiness"
    );
    assert!(body["paths"]["/v1/audit"].is_object());
    assert!(body["paths"]["/v1/operations"].is_object());
    assert!(body["paths"]["/v1/metadata/tenants"].is_object());
    assert!(body["paths"]["/v1/metadata/rooms/{id}/members"].is_object());
    assert!(body["paths"]["/v1/metadata/rooms/{id}/ws"].is_object());
    assert!(body["paths"]["/v1/metadata/history"].is_object());
    assert!(body["paths"]["/v1/auth/tokens"].is_object());
    assert!(body["paths"]["/v1/sessions/{id}/connections/from-profile"].is_object());
    assert_eq!(
        body["paths"]["/v1/metadata/rooms"]["post"]["requestBody"]["content"]["application/json"]
            ["schema"]["$ref"],
        "#/components/schemas/CreateRoomRequest"
    );
    assert_eq!(
        body["paths"]["/v1/sessions/{id}/transactions/{tx_id}/savepoints"]["post"]["requestBody"]
            ["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/SavepointRequest"
    );
    assert_eq!(
        body["paths"]["/v1/metadata/rooms"]["get"]["responses"]["200"]["content"]
            ["application/json"]["schema"]["items"]["$ref"],
        "#/components/schemas/Room"
    );
    assert_eq!(
        body["paths"]["/v1/auth/tokens"]["post"]["responses"]["200"]["content"]["application/json"]
            ["schema"]["$ref"],
        "#/components/schemas/IssueTokenResponse"
    );
    assert!(body["components"]["securitySchemes"]["bearerAuth"].is_object());
    assert!(body["components"]["schemas"]["ExecuteResponse"].is_object());
    assert!(body["components"]["schemas"]["BulkInsertRequest"].is_object());
    assert!(body["components"]["schemas"]["BulkInsertResponse"].is_object());
    assert!(body["components"]["schemas"]["CreateRoomRequest"].is_object());
    assert!(body["components"]["schemas"]["IssueTokenResponse"].is_object());
    assert!(body["components"]["schemas"]["Room"].is_object());
    assert!(body["components"]["schemas"]["RoomClientMessage"].is_object());
    assert!(body["components"]["schemas"]["RoomServerMessage"].is_object());
    for path in [
        "/v1/operations/available",
        "/v1/sessions/{id}/connections/{conn_id}/ddl",
        "/v1/sessions/{id}/connections/{conn_id}/complete",
        "/v1/sessions/{id}/connections/{conn_id}/export",
        "/v1/sessions/{id}/connections/{conn_id}/edits/preview",
        "/v1/sessions/{id}/connections/{conn_id}/edits/apply",
        "/v1/sessions/{id}/connections/{conn_id}/search/schema",
        "/v1/sessions/{id}/connections/{conn_id}/search/data",
        "/v1/sessions/{id}/connections/{conn_id}/explain",
        "/v1/sessions/{id}/connections/{conn_id}/processes",
        "/v1/sessions/{id}/connections/{conn_id}/processes/kill",
        "/v1/sessions/{id}/connections/{conn_id}/import/csv",
        "/v1/sessions/{id}/transactions/{tx_id}/preview",
    ] {
        assert!(
            body["paths"][path].is_object(),
            "missing Phase D path {path}"
        );
    }
    for schema in [
        "CompletionRequest",
        "CompletionResponse",
        "PreviewEditsRequest",
        "EditPlan",
        "ApplyEditsRequest",
        "ApplyEditsResult",
        "SchemaSearchRequest",
        "SchemaSearchResponse",
        "DataSearchRequest",
        "DataSearchResponse",
        "ExplainRequest",
        "ExplainResponse",
        "DatabaseProcess",
        "KillProcessRequest",
        "KillProcessResponse",
        "CsvImportRequest",
        "CsvImportResponse",
        "OperationCapability",
        "TransactionState",
        "TransactionPreviewRequest",
        "TransactionPreview",
    ] {
        assert!(
            body["components"]["schemas"][schema].is_object(),
            "missing Phase D schema {schema}"
        );
    }
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
async fn bulk_insert_is_public_http_api() {
    let driver = MockDriver::builder()
        .engine(Engine::SqlServer)
        .bulk_insert_ok(BulkResult { rows_inserted: 3 })
        .build();
    let app = app(test_state_with_driver(driver));

    let session: sift_protocol::SessionInfo = body_json(
        app.clone()
            .oneshot(post_json(
                "/v1/sessions",
                sift_protocol::OpenSessionRequest {
                    tag: Some("bulk".into()),
                },
            ))
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let conn: sift_protocol::ConnectionInfo = body_json(
        app.clone()
            .oneshot(post_json(
                format!("/v1/sessions/{}/connections", session.id),
                sift_protocol::OpenConnectionRequest {
                    engine: Engine::SqlServer,
                    spec: mssql_spec(),
                },
            ))
            .await
            .unwrap()
            .into_body(),
    )
    .await;

    let res = app
        .clone()
        .oneshot(post_json(
            format!(
                "/v1/sessions/{}/connections/{}/bulk-insert",
                session.id, conn.id
            ),
            sift_protocol::BulkInsertRequest {
                table: "dbo.people".into(),
                data: b"id,name\n1,Alice\n2,Bob\n3,Carol\n".to_vec(),
                format: sift_protocol::BulkInsertFormat::Csv,
            },
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: sift_protocol::BulkInsertResponse = body_json(res.into_body()).await;
    assert_eq!(body.rows_inserted, 3);

    let ops: Vec<sift_protocol::OperationAuditEntry> = body_json(
        app.oneshot(Request::get("/v1/operations").body(Body::empty()).unwrap())
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    assert!(ops.iter().any(|row| matches!(
        &row.operation,
        sift_protocol::Operation::BulkInsert { connection, request, .. }
            if *connection == conn.id && request.table == "dbo.people"
    )));
}

#[tokio::test]
async fn native_bulk_insert_is_explicitly_rejected() {
    let driver = MockDriver::builder().engine(Engine::SqlServer).build();
    let app = app(test_state_with_driver(driver));

    let session: sift_protocol::SessionInfo = body_json(
        app.clone()
            .oneshot(post_json(
                "/v1/sessions",
                sift_protocol::OpenSessionRequest { tag: None },
            ))
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let conn: sift_protocol::ConnectionInfo = body_json(
        app.clone()
            .oneshot(post_json(
                format!("/v1/sessions/{}/connections", session.id),
                sift_protocol::OpenConnectionRequest {
                    engine: Engine::SqlServer,
                    spec: mssql_spec(),
                },
            ))
            .await
            .unwrap()
            .into_body(),
    )
    .await;

    let res = app
        .oneshot(post_json(
            format!(
                "/v1/sessions/{}/connections/{}/bulk-insert",
                session.id, conn.id
            ),
            sift_protocol::BulkInsertRequest {
                table: "dbo.people".into(),
                data: Vec::new(),
                format: sift_protocol::BulkInsertFormat::Native,
            },
        ))
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
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

    let mut replayed = Vec::new();
    for _ in 0..200 {
        replayed = test_state_with_operation_log(&path)
            .sessions
            .list_operations();
        if replayed.iter().any(|row| {
            matches!(
                &row.operation,
                sift_protocol::Operation::OpenSession { request }
                    if request.tag.as_deref() == Some("durable")
            )
        }) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
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
async fn client_sdk_consumes_metadata_api() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app(test_state_with_metadata(true)).into_make_service(),
        )
        .await
        .unwrap();
    });

    let client = sift_client_sdk::Client::new(format!("http://{addr}"));
    let tenants = client.tenants().await.unwrap();
    assert_eq!(tenants[0].tenant.id, TenantId(1));

    let room = client
        .create_room(sift_client_sdk::CreateRoomRequest {
            tenant_id: 1,
            name: "sdk room".into(),
            kind: RoomKind::Shared,
        })
        .await
        .unwrap();
    let rooms = client.rooms(TenantId(1)).await.unwrap();
    assert!(rooms.iter().any(|listed| listed.id == room.id));

    let document = client
        .create_document(
            room.id,
            sift_client_sdk::CreateDocumentRequest {
                kind: "sql".into(),
                title: "sdk.sql".into(),
                crdt_type: CrdtType::Loro,
                crdt_state: b"select 1".to_vec(),
                position: 0,
                connection_profile_id: None,
            },
        )
        .await
        .unwrap();
    let updated = client
        .update_document_snapshot(
            document.id,
            sift_client_sdk::UpdateDocumentSnapshotRequest {
                crdt_state: b"select 2".to_vec(),
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.crdt_state, b"select 2");

    let issued = client
        .issue_token(sift_client_sdk::IssueTokenRequest {
            name: "sdk token".into(),
            tenant_id: Some(1),
            expires_at: None,
        })
        .await
        .unwrap();
    assert!(issued.plaintext.starts_with("sift_"));
    assert!(client
        .auth_tokens()
        .await
        .unwrap()
        .iter()
        .any(|token| token.id == issued.token.id));
    client.revoke_token(issued.token.id).await.unwrap();

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
async fn websocket_mid_stream_cancel_stops_paging() {
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    // Drive many pages so the server sits waiting on our ack between them.
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .execute_ok(vec![
            Page::NextResult {
                columns: vec![ColumnMetadata {
                    name: "n".into(),
                    type_ref: TypeRef::Primitive(PrimitiveType::Int32),
                    nullable: Nullability::NotNullable,
                    auto_increment: false,
                    primary_key: false,
                    facets: Default::default(),
                }],
            },
            Page::Rows {
                rows: vec![Row::new(vec![Value::Int32(1)])],
            },
            Page::Rows {
                rows: vec![Row::new(vec![Value::Int32(2)])],
            },
            Page::Done {
                affected_rows: None,
                warnings: Vec::new(),
            },
        ])
        .build();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app(test_state_with_driver(driver)).into_make_service(),
        )
        .await
        .unwrap();
    });

    let client = sift_client_sdk::Client::new(format!("http://{addr}"));
    let session = client
        .open_session(Some("sdk-ws-cancel".into()))
        .await
        .unwrap();
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

    let (mut ws, _) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/v1/sessions/{}/ws", session.id))
            .await
            .unwrap();
    ws.send(Message::Text(
        serde_json::to_string(&sift_protocol::WsClientMessage::Execute {
            request_id: "req".into(),
            connection: conn.id,
            sql: "SELECT * FROM big".into(),
            params: Vec::new(),
            tx: None,
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();

    // Started + first page (columns).
    let started: sift_protocol::WsServerMessage = ws_json(ws.next().await.unwrap().unwrap());
    let cursor_id = match started {
        sift_protocol::WsServerMessage::Started { cursor_id, .. } => cursor_id,
        other => panic!("expected Started, got {other:?}"),
    };
    let first: sift_protocol::WsServerMessage = ws_json(ws.next().await.unwrap().unwrap());
    assert!(matches!(first, sift_protocol::WsServerMessage::Page { .. }));

    // Send Cancel instead of Ack. Server must route to driver.cancel and
    // stop paging, not reject the message.
    ws.send(Message::Text(
        serde_json::to_string(&sift_protocol::WsClientMessage::Cancel {
            connection: conn.id,
            cursor_id,
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();

    // The server should either close the socket cleanly or send nothing
    // further. It MUST NOT push more Pages. Give it a short window to do
    // anything else; assert what did (or didn't) arrive.
    let after = tokio::time::timeout(std::time::Duration::from_millis(200), ws.next()).await;
    match after {
        Ok(Some(Ok(Message::Close(_)))) | Ok(None) => {}
        Ok(Some(Ok(Message::Text(text)))) => {
            let msg: sift_protocol::WsServerMessage = serde_json::from_str(&text).unwrap();
            assert!(
                !matches!(msg, sift_protocol::WsServerMessage::Page { .. }),
                "server sent another Page after Cancel: {msg:?}"
            );
        }
        Ok(Some(Ok(Message::Binary(bytes)))) => {
            let msg: sift_protocol::WsServerMessage = serde_json::from_slice(&bytes).unwrap();
            assert!(
                !matches!(msg, sift_protocol::WsServerMessage::Page { .. }),
                "server sent another Page after Cancel: {msg:?}"
            );
        }
        Ok(Some(Ok(_))) | Ok(Some(Err(_))) => {}
        Err(_) => {} // idle within the window is also acceptable
    }

    server.abort();
}

#[tokio::test]
async fn websocket_execute_requires_active_tx_ref() {
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app(test_state()).into_make_service())
            .await
            .unwrap();
    });

    let client = sift_client_sdk::Client::new(format!("http://{addr}"));
    let session = client.open_session(Some("sdk-ws-tx".into())).await.unwrap();
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
    let _tx = client
        .begin_transaction(session.id, conn.id, sift_protocol::TxMode::default())
        .await
        .unwrap();

    let (mut ws, _) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/v1/sessions/{}/ws", session.id))
            .await
            .unwrap();
    ws.send(Message::Text(
        serde_json::to_string(&sift_protocol::WsClientMessage::Execute {
            request_id: "no-tx".into(),
            connection: conn.id,
            sql: "SELECT id, name FROM users".into(),
            params: Vec::new(),
            tx: None,
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();

    let message: sift_protocol::WsServerMessage = ws_json(ws.next().await.unwrap().unwrap());
    assert!(matches!(
        message,
        sift_protocol::WsServerMessage::Error {
            request_id: Some(id),
            message
        } if id == "no-tx" && message.contains("active transaction")
    ));

    server.abort();
}

#[tokio::test]
async fn client_sdk_consumes_postgres_notifications() {
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .listen_ok(vec![PgNotification {
            channel: "events".into(),
            payload: "created".into(),
        }])
        .build();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app(test_state_with_driver(driver)).into_make_service(),
        )
        .await
        .unwrap();
    });

    let client = sift_client_sdk::Client::new(format!("http://{addr}"));
    let session = client
        .open_session(Some("sdk-listen".into()))
        .await
        .unwrap();
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

    let notifications = client
        .listen_notifications(session.id, conn.id, vec!["events".into()], 1)
        .await
        .unwrap();
    assert_eq!(notifications, vec![("events".into(), "created".into())]);

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
    assert_eq!(res.status(), StatusCode::OK);

    let res = app
        .clone()
        .oneshot(Request::get("/v1/audit").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let res = app
        .oneshot(
            Request::get("/v1/audit")
                .header("authorization", "Bearer secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn password_login_refresh_whoami_and_logout_are_end_to_end() {
    let mut state = test_state_with_metadata(false);
    let metadata = state.metadata.as_ref().unwrap();
    let password = "correct horse battery staple";
    let verifier = sift_server::identity::hash_password(password.as_bytes().to_vec())
        .await
        .unwrap();
    let principal = metadata
        .create_password_principal(
            NewPasswordPrincipal {
                username: "hosted-user",
                display_name: "Hosted User",
                email: Some("hosted@example.com"),
                is_instance_admin: false,
            },
            verifier.as_bytes(),
            NewOperationAudit {
                actor_principal_id: Some(PrincipalId(1)),
                action: "create".into(),
                target: "principal".into(),
                target_id: None,
                status: "succeeded".into(),
                result_code: None,
                row_count: None,
                error_message: None,
                correlation_id: None,
            },
        )
        .await
        .unwrap();
    state.auth.loopback_bypass = false;
    let app = app(state);

    let denied = app
        .clone()
        .oneshot(
            Request::post("/v1/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"username":"hosted-user","password":"incorrect password value"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

    let login = app
        .clone()
        .oneshot(
            Request::post("/v1/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "username": "HOSTED-USER",
                        "password": password,
                        "client_kind": "native",
                        "client_label": "integration test"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let first: AuthTokensResponse = body_json(login.into_body()).await;

    let whoami = app
        .clone()
        .oneshot(
            Request::get("/v1/auth/whoami")
                .header("authorization", format!("Bearer {}", first.access_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(whoami.status(), StatusCode::OK);
    let whoami: WhoAmIResponse = body_json(whoami.into_body()).await;
    assert_eq!(whoami.principal.id, principal.id.0);
    assert!(whoami.auth_session_id.is_some());

    let refresh = app
        .clone()
        .oneshot(
            Request::post("/v1/auth/refresh")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"refresh_token": first.refresh_token}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(refresh.status(), StatusCode::OK);
    let second: AuthTokensResponse = body_json(refresh.into_body()).await;

    let old_access = app
        .clone()
        .oneshot(
            Request::get("/v1/auth/whoami")
                .header("authorization", format!("Bearer {}", first.access_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(old_access.status(), StatusCode::UNAUTHORIZED);

    let logout = app
        .clone()
        .oneshot(
            Request::post("/v1/auth/logout")
                .header("authorization", format!("Bearer {}", second.access_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(logout.status(), StatusCode::OK);
    let after_logout = app
        .oneshot(
            Request::get("/v1/auth/whoami")
                .header("authorization", format!("Bearer {}", second.access_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(after_logout.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn websocket_lease_reauthenticates_after_rotation_and_closes_on_revocation() {
    let mut state = test_state_with_metadata(false);
    let password = "renewable websocket password";
    let verifier = sift_server::identity::hash_password(password.as_bytes().to_vec())
        .await
        .unwrap();
    state
        .metadata
        .as_ref()
        .unwrap()
        .create_password_principal(
            NewPasswordPrincipal {
                username: "websocket-user",
                display_name: "WebSocket User",
                email: None,
                is_instance_admin: false,
            },
            verifier.as_bytes(),
            NewOperationAudit {
                actor_principal_id: Some(PrincipalId(1)),
                action: "create".into(),
                target: "principal".into(),
                target_id: None,
                status: "succeeded".into(),
                result_code: None,
                row_count: None,
                error_message: None,
                correlation_id: None,
            },
        )
        .await
        .unwrap();
    state.auth.loopback_bypass = false;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app(state).into_make_service())
            .await
            .unwrap();
    });

    let unauthenticated = sift_client_sdk::Client::new(format!("http://{addr}"));
    let provider = unauthenticated
        .password_login(sift_protocol::PasswordLoginRequest {
            username: "websocket-user".into(),
            password: password.into(),
            client_kind: sift_protocol::AuthClientKind::Native,
            client_label: Some("lease test".into()),
        })
        .await
        .unwrap();
    let client = sift_client_sdk::Client::new(format!("http://{addr}"))
        .with_session_tokens(provider.clone());
    let session = client
        .open_session(Some("lease test".into()))
        .await
        .unwrap();
    let mut socket = client.connect_session_websocket(session.id).await.unwrap();

    client.refresh_session().await.unwrap();
    let renewed_expiry = provider
        .reauthenticate_session_websocket(&mut socket)
        .await
        .unwrap();
    assert!(renewed_expiry > chrono::Utc::now());

    client.logout_all().await.unwrap();
    let revoked = tokio::time::timeout(std::time::Duration::from_secs(3), socket.next())
        .await
        .expect("lease revocation is delivered")
        .unwrap();
    assert!(matches!(
        revoked,
        sift_protocol::WsServerMessage::Error { message, .. }
            if message.contains("revoked")
    ));
    server.abort();
}

#[tokio::test]
async fn room_websocket_lease_closes_when_membership_is_removed() {
    let mut state = test_state_with_metadata(false);
    let metadata = state.metadata.as_ref().unwrap().clone();
    let member = metadata
        .create_password_principal(
            NewPasswordPrincipal {
                username: "removed-room-member",
                display_name: "Removed Member",
                email: None,
                is_instance_admin: false,
            },
            b"test-verifier",
            NewOperationAudit {
                actor_principal_id: Some(PrincipalId(1)),
                action: "create".into(),
                target: "principal".into(),
                target_id: None,
                status: "succeeded".into(),
                result_code: None,
                row_count: None,
                error_message: None,
                correlation_id: None,
            },
        )
        .await
        .unwrap();
    let room = metadata
        .create_room(
            TenantId(1),
            PrincipalId(1),
            NewRoom {
                name: "membership lease".into(),
                kind: RoomKind::Shared,
            },
        )
        .unwrap();
    metadata
        .upsert_tenant_membership(TenantId(1), member.id, MembershipRole::Member)
        .unwrap();
    metadata
        .add_room_member(room.id, member.id, RoomRole::Editor)
        .unwrap();
    let issued = metadata
        .issue_auth_session(
            member.id,
            sift_metadata::AuthClientKind::Native,
            Some("room lease test"),
            NewOperationAudit {
                actor_principal_id: Some(member.id),
                action: "authenticate".into(),
                target: "auth_session".into(),
                target_id: None,
                status: "succeeded".into(),
                result_code: None,
                row_count: None,
                error_message: None,
                correlation_id: None,
            },
        )
        .await
        .unwrap();
    state.auth.loopback_bypass = false;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app(state).into_make_service())
            .await
            .unwrap();
    });
    let provider = sift_client_sdk::SessionTokenProvider::new(AuthTokensResponse {
        access_token: issued.access_token,
        access_expires_at: issued.access_expires_at,
        refresh_token: issued.refresh_token,
        refresh_expires_at: issued.refresh_expires_at,
    });
    let client =
        sift_client_sdk::Client::new(format!("http://{addr}")).with_session_tokens(provider);
    let mut socket = client.connect_room_websocket(room.id).await.unwrap();
    socket
        .send(RoomClientMessage::Attach {
            client_id: "membership-test".into(),
        })
        .await
        .unwrap();
    assert!(matches!(
        socket.next().await.unwrap(),
        RoomServerMessage::Attached { .. }
    ));

    metadata.remove_room_member(room.id, member.id).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if let RoomServerMessage::Error { message } = socket.next().await.unwrap() {
                assert!(message.contains("membership"));
                break;
            }
        }
    })
    .await
    .expect("membership revocation is delivered");
    server.abort();
}

#[tokio::test]
async fn web_password_auth_uses_http_only_cookies_and_requires_csrf() {
    let mut state = test_state_with_metadata(false);
    let metadata = state.metadata.as_ref().unwrap();
    let password = "browser password manager value";
    let verifier = sift_server::identity::hash_password(password.as_bytes().to_vec())
        .await
        .unwrap();
    metadata
        .create_password_principal(
            NewPasswordPrincipal {
                username: "browser-user",
                display_name: "Browser User",
                email: None,
                is_instance_admin: false,
            },
            verifier.as_bytes(),
            NewOperationAudit {
                actor_principal_id: Some(PrincipalId(1)),
                action: "create".into(),
                target: "principal".into(),
                target_id: None,
                status: "succeeded".into(),
                result_code: None,
                row_count: None,
                error_message: None,
                correlation_id: None,
            },
        )
        .await
        .unwrap();
    state.auth.loopback_bypass = false;
    let app = app(state);
    let login = app
        .clone()
        .oneshot(
            Request::post("/v1/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "username": "browser-user",
                        "password": password,
                        "client_kind": "web"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let set_cookies: Vec<String> = login
        .headers()
        .get_all("set-cookie")
        .iter()
        .map(|value| value.to_str().unwrap().to_string())
        .collect();
    assert_eq!(set_cookies.len(), 3);
    assert!(set_cookies
        .iter()
        .filter(|cookie| cookie.starts_with("sift_access=") || cookie.starts_with("sift_refresh="))
        .all(|cookie| cookie.contains("HttpOnly") && cookie.contains("Secure")));
    let cookie_header = set_cookies
        .iter()
        .map(|cookie| cookie.split(';').next().unwrap())
        .collect::<Vec<_>>()
        .join("; ");
    let web: WebAuthResponse = body_json(login.into_body()).await;

    let whoami = app
        .clone()
        .oneshot(
            Request::get("/v1/auth/whoami")
                .header("cookie", &cookie_header)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(whoami.status(), StatusCode::OK);

    let no_csrf = app
        .clone()
        .oneshot(
            Request::post("/v1/auth/logout")
                .header("cookie", &cookie_header)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(no_csrf.status(), StatusCode::FORBIDDEN);

    let refreshed = app
        .clone()
        .oneshot(
            Request::post("/v1/auth/refresh")
                .header("content-type", "application/json")
                .header("cookie", &cookie_header)
                .header("x-sift-csrf", &web.csrf_token)
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(refreshed.status(), StatusCode::OK);
    let refreshed_cookies: Vec<String> = refreshed
        .headers()
        .get_all("set-cookie")
        .iter()
        .map(|value| value.to_str().unwrap().to_string())
        .collect();
    let refreshed_cookie_header = refreshed_cookies
        .iter()
        .map(|cookie| cookie.split(';').next().unwrap())
        .collect::<Vec<_>>()
        .join("; ");
    let refreshed_web: WebAuthResponse = body_json(refreshed.into_body()).await;
    let logout = app
        .oneshot(
            Request::post("/v1/auth/logout")
                .header("cookie", refreshed_cookie_header)
                .header("x-sift-csrf", refreshed_web.csrf_token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(logout.status(), StatusCode::OK);
    assert_eq!(logout.headers().get_all("set-cookie").iter().count(), 3);
}

#[tokio::test]
async fn github_start_uses_fixed_callback_pkce_and_admin_owned_allowlist() {
    let mut state = test_state_with_metadata(false);
    let metadata = state.metadata.as_ref().unwrap();
    let admin = metadata
        .create_password_principal(
            NewPasswordPrincipal {
                username: "instance-admin",
                display_name: "Instance Admin",
                email: None,
                is_instance_admin: true,
            },
            b"test-verifier",
            NewOperationAudit {
                actor_principal_id: Some(PrincipalId(1)),
                action: "create".into(),
                target: "principal".into(),
                target_id: None,
                status: "succeeded".into(),
                result_code: None,
                row_count: None,
                error_message: None,
                correlation_id: None,
            },
        )
        .await
        .unwrap();
    let (_, admin_token) = metadata
        .issue_api_token(admin.id, None, "admin test", None)
        .unwrap();
    state.auth.github = Some(sift_server::identity::GithubOAuthConfig {
        client_id: "github-client-id".into(),
        client_secret: "github-client-secret".into(),
        public_base_url: "https://sift.example.test".into(),
        http: reqwest::Client::new(),
    });
    let operation_log = state.sessions.clone();
    let app = app(state);

    let start = app
        .clone()
        .oneshot(
            Request::get("/v1/auth/github/start")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(start.status(), StatusCode::TEMPORARY_REDIRECT);
    let location = start.headers()["location"].to_str().unwrap();
    let location = reqwest::Url::parse(location).unwrap();
    assert_eq!(location.host_str(), Some("github.com"));
    let query: std::collections::HashMap<_, _> = location.query_pairs().into_owned().collect();
    assert_eq!(
        query.get("redirect_uri").map(String::as_str),
        Some("https://sift.example.test/v1/auth/github/callback")
    );
    assert_eq!(query.get("code_challenge").unwrap().len(), 43);
    assert_eq!(query.get("code_challenge_method").unwrap(), "S256");
    assert_eq!(query.get("scope").unwrap(), "read:user");
    assert!(!location.as_str().contains("github-client-secret"));

    let created = app
        .clone()
        .oneshot(
            Request::post("/v1/admin/auth/github-allowlist")
                .header("authorization", format!("Bearer {admin_token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"login":"OctoCat","target_principal_id":admin.id.0})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let entry: sift_metadata::GithubAllowlistEntry = body_json(created.into_body()).await;
    assert_eq!(entry.normalized_login, "octocat");
    assert_eq!(entry.target_principal_id, Some(admin.id));

    let listed = app
        .oneshot(
            Request::get("/v1/admin/auth/github-allowlist")
                .header("authorization", format!("Bearer {admin_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    assert!(operation_log.list_operations().iter().any(|entry| matches!(
        entry.operation,
        sift_protocol::Operation::ManageGithubAllowlist {
            action: sift_protocol::IdentityAdminAction::Create,
            principal_id: Some(id),
        } if id == admin.id.0
    )));
}

#[tokio::test]
async fn ed25519_challenge_is_one_use_and_issues_standard_session_tokens() {
    use base64::Engine as _;
    use ed25519_dalek::Signer as _;

    let state = test_state_with_metadata(true);
    let app = app(state);
    let signing = ed25519_dalek::SigningKey::from_bytes(&[7_u8; 32]);
    let public_key =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signing.verifying_key().as_bytes());
    let registered = app
        .clone()
        .oneshot(
            Request::post("/v1/auth/keys")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"public_key":public_key,"label":"test key"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(registered.status(), StatusCode::OK);
    let key: sift_metadata::PrincipalKey = body_json(registered.into_body()).await;

    let challenge = app
        .clone()
        .oneshot(
            Request::post("/v1/auth/keys/challenge")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"fingerprint":key.fingerprint}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(challenge.status(), StatusCode::OK);
    let challenge: sift_protocol::KeyChallengeResponse = body_json(challenge.into_body()).await;
    assert!(challenge.message.contains("sift:local"));
    let signature = signing.sign(challenge.message.as_bytes());
    let auth_body = serde_json::json!({
        "nonce": challenge.nonce,
        "signature": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature.to_bytes())
    })
    .to_string();
    let authenticated = app
        .clone()
        .oneshot(
            Request::post("/v1/auth/keys/authenticate")
                .header("content-type", "application/json")
                .body(Body::from(auth_body.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(authenticated.status(), StatusCode::OK);
    let tokens: AuthTokensResponse = body_json(authenticated.into_body()).await;
    let whoami = app
        .clone()
        .oneshot(
            Request::get("/v1/auth/whoami")
                .header("authorization", format!("Bearer {}", tokens.access_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(whoami.status(), StatusCode::OK);

    let replay = app
        .oneshot(
            Request::post("/v1/auth/keys/authenticate")
                .header("content-type", "application/json")
                .body(Body::from(auth_body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn responses_are_gzipped_when_client_advertises_gzip() {
    let app = app(test_state());
    let res = app
        .oneshot(
            Request::get("/v1/openapi.json")
                .header("accept-encoding", "gzip")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let encoding = res
        .headers()
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        encoding.contains("gzip"),
        "expected gzip content-encoding, got {encoding:?}"
    );
}

#[tokio::test]
async fn responses_are_uncompressed_when_client_does_not_advertise() {
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
    assert!(
        res.headers().get("content-encoding").is_none(),
        "unexpected content-encoding on non-negotiated response"
    );
}

#[tokio::test]
async fn metadata_room_document_lifecycle_uses_local_principal() {
    let app = app(test_state_with_metadata(true));

    let res = app
        .clone()
        .oneshot(
            Request::get("/v1/metadata/tenants")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let tenants: serde_json::Value = body_json(res.into_body()).await;
    assert_eq!(tenants[0]["tenant"]["id"], 1);

    let room: serde_json::Value = body_json(
        app.clone()
            .oneshot(post_json(
                "/v1/metadata/rooms",
                serde_json::json!({
                    "tenant_id": 1,
                    "name": "planning",
                    "kind": "personal"
                }),
            ))
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let room_id = room["id"].as_i64().unwrap();

    let members: Vec<serde_json::Value> = body_json(
        app.clone()
            .oneshot(
                Request::get(format!("/v1/metadata/rooms/{room_id}/members"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    assert_eq!(members.len(), 1);

    let member: serde_json::Value = body_json(
        app.clone()
            .oneshot(post_json(
                format!("/v1/metadata/rooms/{room_id}/join"),
                serde_json::json!({}),
            ))
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    assert_eq!(member["principal_id"], 1);

    let rooms: Vec<serde_json::Value> = body_json(
        app.clone()
            .oneshot(
                Request::get("/v1/metadata/rooms?tenant=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    assert_eq!(rooms.len(), 1);

    let document: serde_json::Value = body_json(
        app.clone()
            .oneshot(post_json(
                format!("/v1/metadata/rooms/{room_id}/documents"),
                serde_json::json!({
                    "kind": "sql",
                    "title": "scratch",
                    "crdt_type": "loro",
                    "crdt_state": [1, 2, 3],
                    "position": 0
                }),
            ))
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let document_id = document["id"].as_i64().unwrap();

    let res = app
        .clone()
        .oneshot(
            Request::put(format!("/v1/metadata/documents/{document_id}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"crdt_state":[4,5]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let updated: serde_json::Value = body_json(res.into_body()).await;
    assert_eq!(updated["crdt_state"], serde_json::json!([4, 5]));

    let history: Vec<serde_json::Value> = body_json(
        app.clone()
            .oneshot(
                Request::get("/v1/metadata/history?limit=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    assert!(history.is_empty());

    let ops: Vec<sift_protocol::OperationAuditEntry> = body_json(
        app.oneshot(Request::get("/v1/operations").body(Body::empty()).unwrap())
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    assert!(ops.iter().any(|row| matches!(
        &row.operation,
        sift_protocol::Operation::Metadata { action, target, id }
            if action == "create" && target == "room" && *id == Some(room_id)
    )));
    assert!(ops.iter().any(|row| matches!(
        &row.operation,
        sift_protocol::Operation::Metadata { action, target, id }
            if action == "update" && target == "document" && *id == Some(document_id)
    )));
}

#[tokio::test]
async fn metadata_api_tokens_can_authenticate_and_be_revoked() {
    let state = test_state_with_metadata(true);
    let app_with_loopback = app(state.clone());
    let issued: serde_json::Value = body_json(
        app_with_loopback
            .oneshot(post_json(
                "/v1/auth/tokens",
                serde_json::json!({
                    "name": "test token",
                    "tenant_id": 1
                }),
            ))
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let plaintext = issued["plaintext"].as_str().unwrap().to_string();
    let token_id = issued["token"]["id"].as_i64().unwrap();

    let mut no_loopback = state;
    no_loopback.auth.loopback_bypass = false;
    let app_no_loopback = app(no_loopback);
    let res = app_no_loopback
        .clone()
        .oneshot(
            Request::get("/v1/metadata/tenants")
                .header("authorization", format!("Bearer {plaintext}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app_no_loopback
        .clone()
        .oneshot(
            Request::delete(format!("/v1/auth/tokens/{token_id}"))
                .header("authorization", format!("Bearer {plaintext}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app_no_loopback
        .oneshot(
            Request::get("/v1/metadata/tenants")
                .header("authorization", format!("Bearer {plaintext}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn metadata_auth_and_tenant_edges_are_rejected() {
    let state = test_state_with_metadata(true);
    let app_with_loopback = app(state.clone());
    let issued: serde_json::Value = body_json(
        app_with_loopback
            .oneshot(post_json(
                "/v1/auth/tokens",
                serde_json::json!({
                    "name": "expired",
                    "tenant_id": 1,
                    "expires_at": "2000-01-01T00:00:00Z"
                }),
            ))
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let expired = issued["plaintext"].as_str().unwrap().to_string();

    let mut no_loopback = state;
    no_loopback.auth.loopback_bypass = false;
    let app_no_loopback = app(no_loopback);
    let res = app_no_loopback
        .clone()
        .oneshot(
            Request::get("/v1/metadata/tenants")
                .header("authorization", format!("Bearer {expired}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let app_with_loopback = app(test_state_with_metadata(true));
    let res = app_with_loopback
        .clone()
        .oneshot(
            Request::get("/v1/metadata/rooms?tenant=2")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    let res = app_with_loopback
        .oneshot(post_json(
            "/v1/sessions/1/connections/from-profile",
            serde_json::json!({
                "tenant_id": 2,
                "profile_id": 1
            }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn metadata_room_roles_are_enforced() {
    let mut state = test_state_with_metadata(true);
    let metadata = state.metadata.as_ref().unwrap();
    let viewer = metadata
        .create_principal("test:viewer", "Viewer", None)
        .unwrap();
    let editor = metadata
        .create_principal("test:editor", "Editor", None)
        .unwrap();
    let outsider = metadata
        .create_principal("test:outsider", "Outsider", None)
        .unwrap();
    for principal in [viewer.id, editor.id, outsider.id] {
        metadata
            .upsert_tenant_membership(TenantId(1), principal, MembershipRole::Member)
            .unwrap();
    }
    let room = metadata
        .create_room(
            TenantId(1),
            PrincipalId(1),
            NewRoom {
                name: "roles".into(),
                kind: RoomKind::Shared,
            },
        )
        .unwrap();
    metadata
        .add_room_member(room.id, viewer.id, RoomRole::Viewer)
        .unwrap();
    metadata
        .add_room_member(room.id, editor.id, RoomRole::Editor)
        .unwrap();
    let document = metadata
        .create_document(
            room.id,
            NewDocument {
                kind: "sql".into(),
                title: "role-check.sql".into(),
                crdt_type: CrdtType::Loro,
                crdt_state: vec![1],
                position: 0,
                connection_profile_id: None,
            },
        )
        .unwrap();
    let (_, viewer_token) = metadata
        .issue_api_token(viewer.id, Some(TenantId(1)), "viewer", None)
        .unwrap();
    let (_, editor_token) = metadata
        .issue_api_token(editor.id, Some(TenantId(1)), "editor", None)
        .unwrap();
    let (_, outsider_token) = metadata
        .issue_api_token(outsider.id, Some(TenantId(1)), "outsider", None)
        .unwrap();

    state.auth.loopback_bypass = false;
    let app = app(state);

    let res = app
        .clone()
        .oneshot(
            Request::get(format!("/v1/metadata/rooms/{}/documents", room.id.0))
                .header("authorization", format!("Bearer {viewer_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app
        .clone()
        .oneshot(
            Request::put(format!("/v1/metadata/documents/{}", document.id.0))
                .header("authorization", format!("Bearer {viewer_token}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"crdt_state":[2]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    let res = app
        .clone()
        .oneshot(
            Request::put(format!("/v1/metadata/documents/{}", document.id.0))
                .header("authorization", format!("Bearer {editor_token}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"crdt_state":[3]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app
        .clone()
        .oneshot(
            Request::delete(format!("/v1/metadata/rooms/{}", room.id.0))
                .header("authorization", format!("Bearer {editor_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    let res = app
        .clone()
        .oneshot(
            Request::post(format!("/v1/metadata/rooms/{}/members", room.id.0))
                .header("authorization", format!("Bearer {viewer_token}"))
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"principal_id":{},"role":"viewer"}}"#,
                    outsider.id.0
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    let res = app
        .oneshot(
            Request::get(format!("/v1/metadata/rooms/{}/documents", room.id.0))
                .header("authorization", format!("Bearer {outsider_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn document_creation_rejects_cross_tenant_connection_profile() {
    let state = test_state_with_metadata(true);
    let metadata = state.metadata.as_ref().unwrap().clone();
    let other_tenant = metadata.create_tenant("other", TenantKind::Team).unwrap();
    metadata
        .upsert_tenant_membership(other_tenant.id, PrincipalId(1), MembershipRole::Owner)
        .unwrap();
    let room = metadata
        .create_room(
            TenantId(1),
            PrincipalId(1),
            NewRoom {
                name: "tenant-one-room".into(),
                kind: RoomKind::Shared,
            },
        )
        .unwrap();
    let profile = metadata
        .upsert_connection_profile(
            other_tenant.id,
            PrincipalId(1),
            NewConnectionProfile {
                name: "other-tenant-profile".into(),
                engine: Engine::Postgres,
                spec: pg_spec(),
                credential_mode: CredentialMode::Shared,
                tags: Vec::new(),
            },
        )
        .await
        .unwrap();
    let app = app(state);

    let res = app
        .oneshot(post_json(
            format!("/v1/metadata/rooms/{}/documents", room.id.0),
            serde_json::json!({
                "kind": "sql",
                "title": "bad.sql",
                "crdt_type": "loro",
                "crdt_state": [],
                "position": 0,
                "connection_profile_id": profile.id.0
            }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn http_execute_records_room_scoped_query_history() {
    let mut state = test_state_with_metadata(true);
    let metadata = state.metadata.as_ref().unwrap();
    let room = metadata
        .create_room(
            TenantId(1),
            PrincipalId(1),
            NewRoom {
                name: "history execute".into(),
                kind: RoomKind::Shared,
            },
        )
        .unwrap();
    let (_, token) = metadata
        .issue_api_token(PrincipalId(1), Some(TenantId(1)), "history", None)
        .unwrap();
    state.auth.loopback_bypass = false;
    let app = app(state);

    let session: sift_protocol::SessionInfo = body_json(
        app.clone()
            .oneshot(
                Request::post("/v1/sessions")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"tag":"history"}"#))
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let conn: sift_protocol::ConnectionInfo = body_json(
        app.clone()
            .oneshot(
                Request::post(format!("/v1/sessions/{}/connections", session.id))
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&sift_protocol::OpenConnectionRequest {
                            engine: Engine::Postgres,
                            spec: pg_spec(),
                        })
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body(),
    )
    .await;

    let res = app
        .clone()
        .oneshot(
            Request::post(format!("/v1/sessions/{}/queries", session.id))
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&ExecuteRequestHttp {
                        connection: conn.id,
                        sql: "SELECT id, name FROM users".into(),
                        params: Vec::new(),
                        tx: None,
                        room_id: Some(room.id.0),
                        connection_profile_id: None,
                    })
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let history: Vec<serde_json::Value> = body_json(
        app.oneshot(
            Request::get(format!("/v1/metadata/history?room={}&limit=10", room.id.0))
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .into_body(),
    )
    .await;
    assert_eq!(history.len(), 1);
    assert_eq!(history[0]["room_id"], room.id.0);
    assert_eq!(history[0]["sql_text"], "SELECT id, name FROM users");
    assert_eq!(history[0]["status"], "ok");
    assert_eq!(history[0]["row_count"], 2);
}

#[tokio::test]
async fn room_websocket_applies_and_broadcasts_document_operations() {
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let state = test_state_with_metadata(true);
    let metadata = state.metadata.as_ref().unwrap().clone();
    let room = metadata
        .create_room(
            TenantId(1),
            PrincipalId(1),
            NewRoom {
                name: "room ws".into(),
                kind: RoomKind::Shared,
            },
        )
        .unwrap();
    let document = metadata
        .create_document(
            room.id,
            NewDocument {
                kind: "sql".into(),
                title: "ws.sql".into(),
                crdt_type: CrdtType::Loro,
                crdt_state: b"select 1".to_vec(),
                position: 0,
                connection_profile_id: None,
            },
        )
        .unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app(state).into_make_service())
            .await
            .unwrap();
    });

    let (mut ws, _) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/v1/metadata/rooms/{}/ws", room.id.0))
            .await
            .unwrap();
    ws.send(Message::Text(
        serde_json::to_string(&RoomClientMessage::Attach {
            client_id: "test-client".into(),
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();
    let attached: RoomServerMessage = ws_json(ws.next().await.unwrap().unwrap());
    assert!(matches!(attached, RoomServerMessage::Attached { .. }));

    ws.send(Message::Text(
        serde_json::to_string(&RoomClientMessage::DocumentOperation {
            operation_id: "op-1".into(),
            document_id: document.id.0,
            operation: TextDocumentOperation::Replace {
                text: "select 2".into(),
            },
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();

    let mut saw_operation = false;
    for _ in 0..4 {
        let message: RoomServerMessage = ws_json(ws.next().await.unwrap().unwrap());
        if matches!(
            message,
            RoomServerMessage::DocumentOperation { operation }
                if operation.operation_id == "op-1"
        ) {
            saw_operation = true;
            break;
        }
    }
    assert!(saw_operation);
    assert_eq!(
        metadata.get_document(document.id).unwrap().crdt_state,
        b"select 2"
    );

    let client = sift_client_sdk::Client::new(format!("http://{addr}"));
    let envelope = client
        .apply_room_text_operation(
            room.id,
            document.id,
            "sdk-room-client",
            "op-2",
            TextDocumentOperation::Replace {
                text: "select 3".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(envelope.operation_id, "op-2");
    assert_eq!(
        metadata.get_document(document.id).unwrap().crdt_state,
        b"select 3"
    );

    server.abort();
}

#[tokio::test]
async fn metadata_connection_profile_opens_session_connection() {
    let app = app(test_state_with_metadata(true));
    let session: sift_protocol::SessionInfo = body_json(
        app.clone()
            .oneshot(post_json_str("/v1/sessions", r#"{"tag":"profile"}"#))
            .await
            .unwrap()
            .into_body(),
    )
    .await;

    let profile: serde_json::Value = body_json(
        app.clone()
            .oneshot(post_json(
                "/v1/metadata/connections",
                serde_json::json!({
                    "tenant_id": 1,
                    "name": "local pg",
                    "engine": "postgres",
                    "spec": {
                        "host": "mock.invalid",
                        "port": 5432,
                        "database": "mock",
                        "user": "mock",
                        "ssl_mode": "disable"
                    },
                    "credential_mode": "shared",
                    "tags": ["test"]
                }),
            ))
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let profile_id = profile["id"].as_i64().unwrap();

    let res = app
        .oneshot(post_json(
            format!("/v1/sessions/{}/connections/from-profile", session.id),
            serde_json::json!({
                "tenant_id": 1,
                "profile_id": profile_id
            }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let conn: sift_protocol::ConnectionInfo = body_json(res.into_body()).await;
    assert_eq!(conn.engine, Engine::Postgres);
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
        params: Vec::new(),
        tx: None,
        room_id: None,
        connection_profile_id: None,
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
async fn http_execute_rejects_results_over_row_cap() {
    let rows = (0..10_001)
        .map(|idx| Row::new(vec![Value::Int32(idx)]))
        .collect::<Vec<_>>();
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .execute_ok(vec![
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
            Page::Rows { rows },
            Page::Done {
                affected_rows: None,
                warnings: Vec::new(),
            },
        ])
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
    let conn: sift_protocol::ConnectionInfo = body_json(
        app.clone()
            .oneshot(post_json(
                format!("/v1/sessions/{}/connections", session.id),
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
            format!("/v1/sessions/{}/queries", session.id),
            ExecuteRequestHttp {
                connection: conn.id,
                sql: "SELECT too_many_rows".into(),
                params: Vec::new(),
                tx: None,
                room_id: None,
                connection_profile_id: None,
            },
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn http_execute_rejects_multi_result_batches() {
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .execute_ok(vec![
            Page::NextResult {
                columns: vec![ColumnMetadata {
                    name: "one".into(),
                    type_ref: TypeRef::Primitive(PrimitiveType::Int32),
                    nullable: Nullability::NotNullable,
                    auto_increment: false,
                    primary_key: false,
                    facets: Default::default(),
                }],
            },
            Page::Rows {
                rows: vec![Row::new(vec![Value::Int32(1)])],
            },
            Page::NextResult {
                columns: vec![ColumnMetadata {
                    name: "two".into(),
                    type_ref: TypeRef::Primitive(PrimitiveType::Text),
                    nullable: Nullability::NotNullable,
                    auto_increment: false,
                    primary_key: false,
                    facets: Default::default(),
                }],
            },
            Page::Rows {
                rows: vec![Row::new(vec![Value::Text("x".into())])],
            },
            Page::Done {
                affected_rows: None,
                warnings: Vec::new(),
            },
        ])
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
    let conn: sift_protocol::ConnectionInfo = body_json(
        app.clone()
            .oneshot(post_json(
                format!("/v1/sessions/{}/connections", session.id),
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
            format!("/v1/sessions/{}/queries", session.id),
            ExecuteRequestHttp {
                connection: conn.id,
                sql: "SELECT 1; SELECT 'x'".into(),
                params: Vec::new(),
                tx: None,
                room_id: None,
                connection_profile_id: None,
            },
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
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
                params: Vec::new(),
                tx: None,
                room_id: None,
                connection_profile_id: None,
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
                params: Vec::new(),
                tx: Some(sift_protocol::TxHandleRef {
                    tx_id: tx.tx_id,
                    connection: conn.id,
                    mode: tx.mode,
                }),
                room_id: None,
                connection_profile_id: None,
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
async fn cancel_rejects_different_session_owner() {
    let mut state = test_state_with_metadata(true);
    let metadata = state.metadata.as_ref().unwrap();
    let owner = metadata
        .create_principal("test:cancel-owner", "Cancel Owner", None)
        .unwrap();
    let other = metadata
        .create_principal("test:cancel-other", "Cancel Other", None)
        .unwrap();
    for principal in [owner.id, other.id] {
        metadata
            .upsert_tenant_membership(TenantId(1), principal, MembershipRole::Member)
            .unwrap();
    }
    let (_, owner_token) = metadata
        .issue_api_token(owner.id, Some(TenantId(1)), "owner", None)
        .unwrap();
    let (_, other_token) = metadata
        .issue_api_token(other.id, Some(TenantId(1)), "other", None)
        .unwrap();
    state.auth.loopback_bypass = false;
    let app = app(state);

    let session: sift_protocol::SessionInfo = body_json(
        app.clone()
            .oneshot(
                Request::post("/v1/sessions")
                    .header("authorization", format!("Bearer {owner_token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"tag":"cancel-owner"}"#))
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let conn: sift_protocol::ConnectionInfo = body_json(
        app.clone()
            .oneshot(
                Request::post(format!("/v1/sessions/{}/connections", session.id))
                    .header("authorization", format!("Bearer {owner_token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&sift_protocol::OpenConnectionRequest {
                            engine: Engine::Postgres,
                            spec: pg_spec(),
                        })
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body(),
    )
    .await;

    let res = app
        .clone()
        .oneshot(
            Request::get(format!("/v1/sessions/{}", session.id))
                .header("authorization", format!("Bearer {other_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    let other_sessions: Vec<sift_protocol::SessionInfo> = body_json(
        app.clone()
            .oneshot(
                Request::get("/v1/sessions")
                    .header("authorization", format!("Bearer {other_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    assert!(other_sessions.is_empty());

    let res = app
        .clone()
        .oneshot(
            Request::post(format!("/v1/sessions/{}/queries/1/cancel", session.id))
                .header("authorization", format!("Bearer {other_token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "connection": conn.id,
                        "cursor": 1,
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    let res = app
        .oneshot(
            Request::post(format!("/v1/sessions/{}/queries/1/cancel", session.id))
                .header("authorization", format!("Bearer {owner_token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "connection": conn.id,
                        "cursor": 1,
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
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
        .clone()
        .oneshot(post_json(
            format!("/v1/sessions/{sid}/queries"),
            ExecuteRequestHttp {
                connection: conn.id,
                sql: "BAD".into(),
                params: Vec::new(),
                tx: None,
                room_id: None,
                connection_profile_id: None,
            },
        ))
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let ops: Vec<sift_protocol::OperationAuditEntry> = body_json(
        app.oneshot(Request::get("/v1/operations").body(Body::empty()).unwrap())
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    // The operation trail records the failed execute, but SQL is fingerprinted
    // (never raw) per the audit-sanitization contract.
    assert!(ops.iter().any(|entry| matches!(
        &entry.operation,
        sift_protocol::Operation::ExecuteQuery { request, .. }
            if request.sql.starts_with("sqlfp:")
                && request.params.is_empty()
                && entry.status == sift_protocol::OperationStatus::Failed
    )));
}

#[tokio::test]
async fn savepoint_routes_dispatch_to_ext_traits() {
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
                    "user": "mock",
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

    let sp_body = sift_protocol::SavepointRequest {
        connection: conn.id,
        tx_id: tx.tx_id,
        name: "sp1".into(),
    };

    let res = app
        .clone()
        .oneshot(post_json(
            format!("/v1/sessions/{sid}/transactions/{}/savepoints", tx.tx_id),
            sp_body.clone(),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app
        .clone()
        .oneshot(post_json(
            format!(
                "/v1/sessions/{sid}/transactions/{}/savepoints/rollback",
                tx.tx_id
            ),
            sp_body.clone(),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = app
        .clone()
        .oneshot(post_json(
            format!(
                "/v1/sessions/{sid}/transactions/{}/savepoints/release",
                tx.tx_id
            ),
            sp_body,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // Path/body tx_id mismatch must be rejected.
    let res = app
        .oneshot(post_json(
            format!("/v1/sessions/{sid}/transactions/{}/savepoints", tx.tx_id),
            sift_protocol::SavepointRequest {
                connection: conn.id,
                tx_id: sift_protocol::TxId(999),
                name: "sp2".into(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn transaction_panel_lists_previews_and_tracks_savepoints() {
    let app = app(test_state());
    let session: sift_protocol::SessionInfo = body_json(
        app.clone()
            .oneshot(post_json_str("/v1/sessions", "{}"))
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let conn: sift_protocol::ConnectionInfo = body_json(
        app.clone()
            .oneshot(post_json(
                format!("/v1/sessions/{}/connections", session.id),
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
    let tx: sift_protocol::TransactionInfo = body_json(
        app.clone()
            .oneshot(post_json(
                format!("/v1/sessions/{}/transactions", session.id),
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
    for name in ["outer", "inner"] {
        let response = app
            .clone()
            .oneshot(post_json(
                format!(
                    "/v1/sessions/{}/transactions/{}/savepoints",
                    session.id, tx.tx_id
                ),
                sift_protocol::SavepointRequest {
                    connection: conn.id,
                    tx_id: tx.tx_id,
                    name: name.into(),
                },
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    let rollback = sift_protocol::SavepointRequest {
        connection: conn.id,
        tx_id: tx.tx_id,
        name: "outer".into(),
    };
    let response = app
        .clone()
        .oneshot(post_json(
            format!(
                "/v1/sessions/{}/transactions/{}/savepoints/rollback",
                session.id, tx.tx_id
            ),
            rollback,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let states: Vec<sift_protocol::TransactionState> = body_json(
        app.clone()
            .oneshot(
                Request::get(format!("/v1/sessions/{}/transactions", session.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].savepoints.len(), 2);
    assert_eq!(
        states[0].savepoints[0].state,
        sift_protocol::SavepointState::Active
    );
    assert_eq!(
        states[0].savepoints[1].state,
        sift_protocol::SavepointState::Invalidated
    );

    let preview: sift_protocol::TransactionPreview = body_json(
        app.oneshot(post_json(
            format!(
                "/v1/sessions/{}/transactions/{}/preview",
                session.id, tx.tx_id
            ),
            sift_protocol::TransactionPreviewRequest {
                connection: conn.id,
                tx_id: tx.tx_id,
                action: sift_protocol::TransactionEndAction::Rollback,
            },
        ))
        .await
        .unwrap()
        .into_body(),
    )
    .await;
    assert!(preview.destructive);
    assert_eq!(preview.active_savepoints, 1);
}

#[tokio::test]
async fn ws_streaming_bounded_memory_across_many_pages() {
    // Prove the cursor pump doesn't buffer everything ahead of the
    // consumer. Push 10k row pages through the driver; assert the WS
    // client observes them in-order and terminates cleanly. The
    // registry's prefetch buffer (default 2 pages) is the invariant
    // that keeps memory bounded regardless of driver throughput vs
    // consumer speed.
    let mut pages = Vec::with_capacity(10_002);
    pages.push(Page::NextResult {
        columns: vec![ColumnMetadata {
            name: "n".into(),
            type_ref: TypeRef::Primitive(PrimitiveType::Int32),
            nullable: Nullability::NotNullable,
            auto_increment: false,
            primary_key: false,
            facets: Default::default(),
        }],
    });
    for i in 0..10_000 {
        pages.push(Page::Rows {
            rows: vec![Row::new(vec![Value::Int32(i)])],
        });
    }
    pages.push(Page::Done {
        affected_rows: None,
        warnings: Vec::new(),
    });
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .execute_ok(pages)
        .build();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app(test_state_with_driver(driver)).into_make_service(),
        )
        .await
        .unwrap();
    });

    let client = sift_client_sdk::Client::new(format!("http://{addr}"));
    let session = client
        .open_session(Some("bounded-mem".into()))
        .await
        .unwrap();
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

    let pages_back = client
        .stream_query(session.id, conn.id, "SELECT n FROM big")
        .await
        .unwrap();
    // The SDK's stream_query drains until Done/Error. We should see
    // one NextResult, 10k Rows, and one Done.
    let rows_pages = pages_back
        .iter()
        .filter(|p| matches!(p, Page::Rows { .. }))
        .count();
    assert_eq!(rows_pages, 10_000);
    assert!(pages_back.iter().any(|p| matches!(p, Page::Done { .. })));

    server.abort();
}

#[cfg(feature = "stress-1m")]
#[tokio::test]
async fn ws_streaming_bounded_memory_across_one_million_pages() {
    // Same shape as the 10k test above but at 1M pages. Slow —
    // gated behind `stress-1m`. What we're proving: the cursor
    // pump's bounded consumer channel (prefetch_pages) keeps
    // steady-state memory bounded regardless of driver throughput
    // vs consumer speed. If the pump were unbounded, a 1M-page
    // buffer would OOM the test runner.
    const N: usize = 1_000_000;
    let mut pages = Vec::with_capacity(N + 2);
    pages.push(Page::NextResult {
        columns: vec![ColumnMetadata {
            name: "n".into(),
            type_ref: TypeRef::Primitive(PrimitiveType::Int32),
            nullable: Nullability::NotNullable,
            auto_increment: false,
            primary_key: false,
            facets: Default::default(),
        }],
    });
    for i in 0..(N as i32) {
        pages.push(Page::Rows {
            rows: vec![Row::new(vec![Value::Int32(i)])],
        });
    }
    pages.push(Page::Done {
        affected_rows: None,
        warnings: Vec::new(),
    });
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .execute_ok(pages)
        .build();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app(test_state_with_driver(driver)).into_make_service(),
        )
        .await
        .unwrap();
    });

    let client = sift_client_sdk::Client::new(format!("http://{addr}"));
    let session = client.open_session(Some("stress-1m".into())).await.unwrap();
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

    let pages_back = client
        .stream_query(session.id, conn.id, "SELECT n FROM huge")
        .await
        .unwrap();
    let rows_pages = pages_back
        .iter()
        .filter(|p| matches!(p, Page::Rows { .. }))
        .count();
    assert_eq!(rows_pages, N);
    assert!(pages_back.iter().any(|p| matches!(p, Page::Done { .. })));

    server.abort();
}

#[tokio::test]
async fn schema_cache_serves_second_call_without_touching_driver() {
    // MockDriver.schema queue has a single canned snapshot. Without a
    // cache, the second schema() call would return
    // "MockDriver: no canned result for `schema`" (DriverInternal). We
    // prove the cache is doing its job by asserting the second call
    // succeeds and returns the same snapshot.
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .schema_ok(SchemaSnapshot::empty(SchemaScope::shallow()))
        .build();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app(test_state_with_driver(driver)).into_make_service(),
        )
        .await
        .unwrap();
    });

    let client = sift_client_sdk::Client::new(format!("http://{addr}"));
    let session = client
        .open_session(Some("schema-cache".into()))
        .await
        .unwrap();
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

    let first = client.schema(session.id, conn.id).await.unwrap();
    // Second call: driver's schema queue is empty; must be served from
    // cache.
    let second = client.schema(session.id, conn.id).await.unwrap();
    assert_eq!(
        serde_json::to_string(&first).unwrap(),
        serde_json::to_string(&second).unwrap()
    );

    server.abort();
}

#[tokio::test]
async fn schema_cache_coalesces_concurrent_misses() {
    // The driver has exactly one schema result. Concurrent cache misses must
    // singleflight behind that one fetch; otherwise the extra callers hit the
    // empty mock queue and fail with DriverInternal.
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .schema_ok(SchemaSnapshot::empty(SchemaScope::shallow()))
        .build();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app(test_state_with_driver(driver)).into_make_service(),
        )
        .await
        .unwrap();
    });

    let base = format!("http://{addr}");
    let client = sift_client_sdk::Client::new(base.clone());
    let session = client
        .open_session(Some("schema-singleflight".into()))
        .await
        .unwrap();
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

    let mut tasks = Vec::new();
    for _ in 0..10 {
        let client = sift_client_sdk::Client::new(base.clone());
        tasks.push(tokio::spawn(async move {
            client.schema(session.id, conn.id).await
        }));
    }
    for task in tasks {
        task.await.unwrap().unwrap();
    }

    server.abort();
}

#[tokio::test]
async fn export_query_csv_and_json_streaming() {
    // Two rows with a mix of scalar types, plus a NULL and a text
    // value that needs CSV quoting. Prove all four formats.
    let cols = vec![
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
    let pages = vec![
        Page::NextResult { columns: cols },
        Page::Rows {
            rows: vec![
                Row::new(vec![Value::Int32(1), Value::Text("alice".into())]),
                Row::new(vec![Value::Int32(2), Value::Text("bob, jr.".into())]),
                Row::new(vec![Value::Int32(3), Value::Null]),
            ],
        },
        Page::Done {
            affected_rows: Some(3),
            warnings: Vec::new(),
        },
    ];

    // A helper to spin up a fresh server (MockDriver.execute_ok
    // consumes its canned pages once, so each format needs its own
    // server).
    async fn run(format: sift_protocol::ExportFormat, pages: Vec<Page>) -> Vec<u8> {
        let driver = MockDriver::builder()
            .engine(Engine::Postgres)
            .execute_ok(pages)
            .build();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                app(test_state_with_driver(driver)).into_make_service(),
            )
            .await
            .unwrap();
        });
        let client = sift_client_sdk::Client::new(format!("http://{addr}"));
        let session = client.open_session(Some("export".into())).await.unwrap();
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
        let bytes = client
            .export_query(
                session.id,
                conn.id,
                sift_protocol::ExportRequest {
                    sql: "SELECT id, name FROM users".into(),
                    params: Vec::new(),
                    format,
                    header: true,
                    null_display: None,
                },
            )
            .await
            .unwrap();
        server.abort();
        bytes
    }

    // CSV: header row, quoted "bob, jr.", empty NULL.
    let csv =
        String::from_utf8(run(sift_protocol::ExportFormat::Csv, pages.clone()).await).unwrap();
    assert!(csv.starts_with("id,name\n"), "csv header: {csv:?}");
    assert!(csv.contains("1,alice\n"));
    assert!(csv.contains("2,\"bob, jr.\"\n"));
    assert!(csv.contains("3,\n"));

    // TSV: tab delimiter, no quoting needed.
    let tsv =
        String::from_utf8(run(sift_protocol::ExportFormat::Tsv, pages.clone()).await).unwrap();
    assert!(tsv.starts_with("id\tname\n"));
    assert!(tsv.contains("2\tbob, jr.\n"));

    // JSON Lines: one object per line, NULL → null.
    let ndjson =
        String::from_utf8(run(sift_protocol::ExportFormat::JsonLines, pages.clone()).await)
            .unwrap();
    let lines: Vec<&str> = ndjson.trim().split('\n').collect();
    assert_eq!(lines.len(), 3);
    let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(first["id"], 1);
    assert_eq!(first["name"], "alice");
    let third: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
    assert_eq!(third["id"], 3);
    assert!(third["name"].is_null());

    // JSON Array: single valid JSON document.
    let json_array =
        String::from_utf8(run(sift_protocol::ExportFormat::JsonArray, pages).await).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&json_array).unwrap();
    assert!(doc.is_array());
    assert_eq!(doc.as_array().unwrap().len(), 3);
    assert_eq!(doc[1]["name"], "bob, jr.");
}

#[tokio::test]
async fn ddl_generation_view_uses_execute_pg_get_viewdef() {
    // View path calls driver.execute("SELECT pg_get_viewdef(...)")
    // and expects the first scalar of the first row back. Feed the
    // MockDriver that shape directly.
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .execute_ok(vec![
            Page::NextResult {
                columns: vec![ColumnMetadata {
                    name: "def".into(),
                    type_ref: TypeRef::Primitive(PrimitiveType::Text),
                    nullable: Nullability::Nullable,
                    auto_increment: false,
                    primary_key: false,
                    facets: Default::default(),
                }],
            },
            Page::Rows {
                rows: vec![Row::new(vec![Value::Text(
                    "SELECT id, name FROM users WHERE active;".into(),
                )])],
            },
            Page::Done {
                affected_rows: None,
                warnings: Vec::new(),
            },
        ])
        .build();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app(test_state_with_driver(driver)).into_make_service(),
        )
        .await
        .unwrap();
    });
    let client = sift_client_sdk::Client::new(format!("http://{addr}"));
    let session = client.open_session(Some("ddl".into())).await.unwrap();
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
    let ddl = client
        .object_ddl(
            session.id,
            conn.id,
            &sift_protocol::ObjectPath {
                catalog: None,
                schema: Some("public".into()),
                name: "active_users".into(),
                kind: Some(sift_protocol::ObjectKind::View),
                routine_args: None,
            },
        )
        .await
        .unwrap();
    assert!(ddl.ddl.contains("CREATE OR REPLACE VIEW"));
    assert!(ddl.ddl.contains("active_users"));
    assert!(ddl.ddl.contains("SELECT id, name FROM users"));
    server.abort();
}

#[tokio::test]
async fn ddl_generation_table_composes_from_deep_schema() {
    // Table path calls driver.schema(Deep) and formats a
    // CREATE TABLE from the returned ObjectInfo. Prime MockDriver
    // with a snapshot describing a users table with an id PK column
    // and a name column.
    use sift_protocol::{
        ConstraintInfo, ConstraintKind, ObjectInfo, ObjectKind, ObjectPath, SchemaScope,
        SchemaSnapshot, SchemaTree,
    };
    let mut snap = SchemaSnapshot::empty(SchemaScope::deep(ObjectPath {
        catalog: None,
        schema: Some("public".into()),
        name: "users".into(),
        kind: Some(ObjectKind::Table),
        routine_args: None,
    }));
    snap.trees.push(sift_protocol::CatalogTree {
        name: "sifttest".into(),
        schemas: vec![SchemaTree {
            name: "public".into(),
            objects: vec![ObjectInfo {
                name: "users".into(),
                kind: ObjectKind::Table,
                routine_args: None,
                columns: vec![
                    ColumnMetadata {
                        name: "id".into(),
                        type_ref: TypeRef::Primitive(PrimitiveType::Int64),
                        nullable: Nullability::NotNullable,
                        auto_increment: true,
                        primary_key: true,
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
                ],
                indexes: Vec::new(),
                constraints: vec![ConstraintInfo {
                    name: "users_pkey".into(),
                    kind: ConstraintKind::PrimaryKey,
                    columns: vec!["id".into()],
                    definition: Some("PRIMARY KEY (id)".into()),
                    references: None,
                }],
                triggers: Vec::new(),
            }],
        }],
    });
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .schema_ok(snap)
        .build();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app(test_state_with_driver(driver)).into_make_service(),
        )
        .await
        .unwrap();
    });
    let client = sift_client_sdk::Client::new(format!("http://{addr}"));
    let session = client.open_session(Some("ddl".into())).await.unwrap();
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
    let ddl = client
        .object_ddl(
            session.id,
            conn.id,
            &ObjectPath {
                catalog: None,
                schema: Some("public".into()),
                name: "users".into(),
                kind: Some(ObjectKind::Table),
                routine_args: None,
            },
        )
        .await
        .unwrap();
    assert!(ddl.ddl.contains("CREATE TABLE"));
    assert!(ddl.ddl.contains("\"public\".\"users\""));
    assert!(ddl.ddl.contains("\"id\" bigint NOT NULL"));
    assert!(ddl.ddl.contains("\"name\" text"));
    assert!(ddl
        .ddl
        .contains("CONSTRAINT \"users_pkey\" PRIMARY KEY (id)"));
    server.abort();
}

#[tokio::test]
async fn saved_queries_lifecycle_personal_and_shared() {
    // Local bootstrap creates a tenant + local principal with Owner
    // role, so this principal can both mint tenant-shared queries
    // AND own personal ones. Loopback bypass gives us the principal
    // without a bearer token.
    let app = app(test_state_with_metadata(true));

    // Create a personal query owned by the caller (principal_id=1).
    let create_personal = serde_json::json!({
        "tenant_id": 1,
        "owner_principal_id": 1,
        "name": "my daily count",
        "sql_text": "select count(*) from users",
        "tags": ["daily", "users"],
    });
    let res = app
        .clone()
        .oneshot(
            Request::post("/v1/metadata/saved-queries")
                .header("content-type", "application/json")
                .body(Body::from(create_personal.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let personal: serde_json::Value = body_json(res.into_body()).await;
    assert_eq!(personal["owner_principal_id"], 1);
    assert_eq!(personal["tags"], serde_json::json!(["daily", "users"]));
    let personal_id = personal["id"].as_i64().unwrap();

    // Create a tenant-shared query (owner_principal_id omitted).
    let create_shared = serde_json::json!({
        "tenant_id": 1,
        "name": "monthly revenue",
        "sql_text": "select sum(amount) from orders where paid = true",
        "tags": ["finance"],
    });
    let res = app
        .clone()
        .oneshot(
            Request::post("/v1/metadata/saved-queries")
                .header("content-type", "application/json")
                .body(Body::from(create_shared.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let shared: serde_json::Value = body_json(res.into_body()).await;
    assert!(shared["owner_principal_id"].is_null());
    let shared_id = shared["id"].as_i64().unwrap();

    // List with an FTS query — should hit only one, and prove FTS
    // works against sql_text (not just name).
    let res = app
        .clone()
        .oneshot(
            Request::get("/v1/metadata/saved-queries?tenant=1&q=revenue")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let hits: serde_json::Value = body_json(res.into_body()).await;
    assert_eq!(hits.as_array().unwrap().len(), 1);
    assert_eq!(hits[0]["id"], shared_id);

    // Punctuation-only searches should stay restrictive. They should
    // not collapse into an FTS match-all and leak every visible query.
    let res = app
        .clone()
        .oneshot(
            Request::get("/v1/metadata/saved-queries?tenant=1&q=***")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let hits: serde_json::Value = body_json(res.into_body()).await;
    assert_eq!(hits.as_array().unwrap().len(), 0);

    // Scope=personal returns only the personal one.
    let res = app
        .clone()
        .oneshot(
            Request::get("/v1/metadata/saved-queries?tenant=1&scope=personal")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let hits: serde_json::Value = body_json(res.into_body()).await;
    assert_eq!(hits.as_array().unwrap().len(), 1);
    assert_eq!(hits[0]["id"], personal_id);

    // Tag filter across both would match neither ("daily" only on
    // personal, "finance" only on shared).
    let res = app
        .clone()
        .oneshot(
            Request::get("/v1/metadata/saved-queries?tenant=1&tags=daily,finance")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let hits: serde_json::Value = body_json(res.into_body()).await;
    assert_eq!(hits.as_array().unwrap().len(), 0);

    // Update the personal query — rename it, keep tags.
    let update = serde_json::json!({ "name": "my daily user count" });
    let res = app
        .clone()
        .oneshot(
            Request::put(format!("/v1/metadata/saved-queries/{personal_id}"))
                .header("content-type", "application/json")
                .body(Body::from(update.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let updated: serde_json::Value = body_json(res.into_body()).await;
    assert_eq!(updated["name"], "my daily user count");
    assert_eq!(updated["tags"], serde_json::json!(["daily", "users"]));

    // Delete both.
    for id in [personal_id, shared_id] {
        let res = app
            .clone()
            .oneshot(
                Request::delete(format!("/v1/metadata/saved-queries/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    // Nothing left.
    let res = app
        .oneshot(
            Request::get("/v1/metadata/saved-queries?tenant=1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let hits: serde_json::Value = body_json(res.into_body()).await;
    assert_eq!(hits.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn loopback_bypass_rejects_non_loopback_peer() {
    use axum::extract::ConnectInfo;
    use std::net::SocketAddr;

    let app = app(test_state_with_metadata(true));

    // Sanity: a loopback peer with no bearer token is authorized under
    // the bypass path.
    let mut req = Request::get("/v1/metadata/tenants")
        .body(Body::empty())
        .unwrap();
    req.extensions_mut()
        .insert(ConnectInfo::<SocketAddr>("127.0.0.1:5555".parse().unwrap()));
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // A remote peer must NOT be authorized just because loopback_bypass
    // is on — that would be a remote-auth bypass hiding behind a default.
    let mut req = Request::get("/v1/metadata/tenants")
        .body(Body::empty())
        .unwrap();
    req.extensions_mut().insert(ConnectInfo::<SocketAddr>(
        "203.0.113.4:5555".parse().unwrap(),
    ));
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // A remote peer cannot spoof the internal peer header — the middleware
    // strips any client-supplied value before setting its own.
    let mut req = Request::get("/v1/metadata/tenants")
        .header("x-sift-peer-addr", "127.0.0.1")
        .body(Body::empty())
        .unwrap();
    req.extensions_mut().insert(ConnectInfo::<SocketAddr>(
        "203.0.113.4:5555".parse().unwrap(),
    ));
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}
