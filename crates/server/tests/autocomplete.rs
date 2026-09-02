//! HTTP integration test for the autocomplete endpoint.
//!
//! Boots the axum server against a `MockDriver` that returns a canned
//! `SchemaSnapshot`, then exercises `POST /complete` end-to-end.

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use sift_driver_api::mock::MockDriver;
use sift_protocol::completion::{
    CompletionCandidate, CompletionContext, CompletionKind, CompletionRequest, CompletionResponse,
};
use sift_protocol::{
    CatalogTree, ColumnMetadata, Engine, Nullability, ObjectInfo, ObjectKind, PrimitiveType,
    SchemaScope, SchemaSnapshot, SchemaTree, ServerInfo, TypeRef,
};
use sift_server::http::{app, AppState, AuthState};
use sift_server::registry::DriverRegistry;
use sift_server::room_runtime::RoomRuntime;
use sift_server::session::SessionStore;
use tower::ServiceExt;

fn users() -> ObjectInfo {
    let mut o = ObjectInfo::new("users", ObjectKind::Table);
    o.columns = vec![
        ColumnMetadata {
            name: "id".into(),
            type_ref: TypeRef::Primitive(PrimitiveType::Int32),
            nullable: Nullability::NotNullable,
            auto_increment: false,
            primary_key: true,
            facets: Default::default(),
        },
        ColumnMetadata {
            name: "email".into(),
            type_ref: TypeRef::Primitive(PrimitiveType::Text),
            nullable: Nullability::NotNullable,
            auto_increment: false,
            primary_key: false,
            facets: Default::default(),
        },
    ];
    o
}

fn snapshot() -> SchemaSnapshot {
    let orders = ObjectInfo::new("orders", ObjectKind::Table);
    SchemaSnapshot {
        trees: vec![CatalogTree {
            name: "mock".into(),
            schemas: vec![SchemaTree {
                name: "public".into(),
                objects: vec![users(), orders],
            }],
        }],
        fetched_at: chrono::Utc::now(),
        scope: SchemaScope::shallow(),
        incomplete: false,
        graph: None,
    }
}

fn shallow_snapshot() -> SchemaSnapshot {
    let users = ObjectInfo::new("users", ObjectKind::Table);
    let orders = ObjectInfo::new("orders", ObjectKind::Table);
    SchemaSnapshot {
        trees: vec![CatalogTree {
            name: "mock".into(),
            schemas: vec![SchemaTree {
                name: "public".into(),
                objects: vec![users, orders],
            }],
        }],
        fetched_at: chrono::Utc::now(),
        scope: SchemaScope::shallow(),
        incomplete: false,
        graph: None,
    }
}

fn single_table_snapshot(catalog: &str, table: &str) -> SchemaSnapshot {
    SchemaSnapshot {
        trees: vec![CatalogTree {
            name: catalog.into(),
            schemas: vec![SchemaTree {
                name: "dbo".into(),
                objects: vec![ObjectInfo::new(table, ObjectKind::Table)],
            }],
        }],
        fetched_at: chrono::Utc::now(),
        scope: SchemaScope::shallow(),
        incomplete: false,
        graph: None,
    }
}

fn mock_driver() -> MockDriver {
    MockDriver::builder()
        .engine(Engine::Postgres)
        .ping_ok(ServerInfo {
            provider: Engine::Postgres.provider_ref("test"),
            server_version: "MockDB 0.1".into(),
            current_database: "mock".into(),
            current_user: "mock".into(),
            pool_warm_slots: None,
        })
        .schema_ok(snapshot())
        .build()
}

fn state() -> AppState {
    let registry = DriverRegistry::builder().register(mock_driver()).build();
    state_with_registry(registry)
}

fn state_with_registry(registry: DriverRegistry) -> AppState {
    AppState {
        sessions: SessionStore::new(registry),
        rooms: RoomRuntime::default(),
        shutdown: sift_server::shutdown::Shutdown::default(),
        auth: AuthState::default(),
        metadata: None,
    }
}

fn post_json(uri: String, body: impl serde::Serialize) -> Request<Body> {
    Request::post(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

async fn body_json<T: serde::de::DeserializeOwned>(body: Body) -> T {
    let bytes = to_bytes(body, 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|e| panic!("decode: {e}; {}", String::from_utf8_lossy(&bytes)))
}

async fn setup() -> (
    axum::Router,
    sift_protocol::SessionId,
    sift_protocol::ConnectionId,
) {
    setup_with_state(state()).await
}

async fn setup_with_state(
    state: AppState,
) -> (
    axum::Router,
    sift_protocol::SessionId,
    sift_protocol::ConnectionId,
) {
    let router = app(state);

    let res = router
        .clone()
        .oneshot(
            Request::post("/v1/sessions")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let session: sift_protocol::SessionInfo = body_json(res.into_body()).await;
    let sid = session.id;

    let open_req = serde_json::json!({
        "provider_id": "sift/postgres",
        "host": "mock.invalid",
        "port": 5432,
        "database": "mock",
        "user": "mock",
        "ssl_mode": "disable",
    });
    let res = router
        .clone()
        .oneshot(post_json(
            format!("/v1/sessions/{sid}/connections"),
            open_req,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let conn: sift_protocol::ConnectionInfo = body_json(res.into_body()).await;
    (router, sid, conn.id)
}

#[tokio::test]
async fn complete_after_from_returns_users() {
    let (router, sid, cid) = setup().await;
    let req = CompletionRequest {
        sql: "SELECT * FROM us".into(),
        cursor: 16,
        limit: Some(10),
    };
    let res = router
        .oneshot(post_json(
            format!("/v1/sessions/{sid}/connections/{cid}/complete"),
            &req,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let resp: CompletionResponse = body_json(res.into_body()).await;
    assert!(matches!(resp.context, CompletionContext::ExpectingTable));
    let first = resp.candidates.first().expect("has candidate");
    assert_eq!(first.label, "users");
    assert!(matches!(first.kind, CompletionKind::Table));
}

#[tokio::test]
async fn complete_dotted_returns_columns() {
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .ping_ok(ServerInfo {
            provider: Engine::Postgres.provider_ref("test"),
            server_version: "MockDB 0.1".into(),
            current_database: "mock".into(),
            current_user: "mock".into(),
            pool_warm_slots: None,
        })
        .schema_ok(shallow_snapshot())
        .schema_ok(snapshot())
        .build();
    let registry = DriverRegistry::builder().register(driver).build();
    let (router, sid, cid) = setup_with_state(state_with_registry(registry)).await;
    let req = CompletionRequest {
        sql: "SELECT users. FROM users".into(),
        cursor: 13,
        limit: Some(10),
    };
    let res = router
        .oneshot(post_json(
            format!("/v1/sessions/{sid}/connections/{cid}/complete"),
            &req,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let resp: CompletionResponse = body_json(res.into_body()).await;
    let labels: Vec<&str> = resp.candidates.iter().map(|c| c.label.as_ref()).collect();
    assert!(labels.contains(&"id"), "id absent in {labels:?}");
    assert!(labels.contains(&"email"), "email absent in {labels:?}");
    // Every column candidate carries a column kind.
    for c in resp
        .candidates
        .iter()
        .filter(|c: &&CompletionCandidate| c.label == "id" || c.label == "email")
    {
        assert!(matches!(c.kind, CompletionKind::Column));
    }
}

#[tokio::test]
async fn completion_hydrates_columns_through_an_alias() {
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .ping_ok(ServerInfo {
            provider: Engine::Postgres.provider_ref("test"),
            server_version: "MockDB 0.1".into(),
            current_database: "mock".into(),
            current_user: "mock".into(),
            pool_warm_slots: None,
        })
        .schema_ok(shallow_snapshot())
        .schema_ok(snapshot())
        .build();
    let registry = DriverRegistry::builder().register(driver).build();
    let (router, sid, cid) = setup_with_state(state_with_registry(registry)).await;
    let sql = "SELECT u. FROM public.users AS u";
    let response = router
        .oneshot(post_json(
            format!("/v1/sessions/{sid}/connections/{cid}/complete"),
            CompletionRequest {
                sql: sql.into(),
                cursor: 9,
                limit: Some(10),
            },
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response: CompletionResponse = body_json(response.into_body()).await;
    let labels: Vec<&str> = response
        .candidates
        .iter()
        .map(|candidate| candidate.label.as_ref())
        .collect();
    assert!(labels.contains(&"id"), "id absent in {labels:?}");
    assert!(labels.contains(&"email"), "email absent in {labels:?}");
}

#[tokio::test]
async fn stateful_and_legacy_completion_have_corpus_parity() {
    let (router, sid, cid) = setup().await;
    let sql = "SELECT * FROM us";
    let legacy: CompletionResponse = body_json(
        router
            .clone()
            .oneshot(post_json(
                format!("/v1/sessions/{sid}/connections/{cid}/complete"),
                CompletionRequest {
                    sql: sql.into(),
                    cursor: 16,
                    limit: Some(10),
                },
            ))
            .await
            .unwrap()
            .into_body(),
    )
    .await;

    let opened = router
        .clone()
        .oneshot(post_json(
            format!("/v1/sessions/{sid}/connections/{cid}/semantic-documents"),
            serde_json::json!({"text": sql, "source": {"kind": "scratch"}}),
        ))
        .await
        .unwrap();
    assert_eq!(opened.status(), StatusCode::CREATED);
    let document: sift_protocol::SemanticDocumentState = body_json(opened.into_body()).await;
    let stateful = router
        .clone()
        .oneshot(post_json(
            format!(
                "/v1/sessions/{sid}/connections/{cid}/semantic-documents/{}/complete",
                document.document_id
            ),
            serde_json::json!({"revision": 1, "cursor": 16, "limit": 10}),
        ))
        .await
        .unwrap();
    assert_eq!(stateful.status(), StatusCode::OK);
    let stateful: CompletionResponse = body_json(stateful.into_body()).await;
    assert_eq!(
        serde_json::to_value(&legacy).unwrap(),
        serde_json::to_value(&stateful).unwrap()
    );

    let stale = router
        .oneshot(post_json(
            format!(
                "/v1/sessions/{sid}/connections/{cid}/semantic-documents/{}/complete",
                document.document_id
            ),
            serde_json::json!({"revision": 2, "cursor": 16}),
        ))
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn complete_openapi_registers_completion_schemas() {
    let router = app(state());
    let res = router
        .oneshot(
            Request::get("/v1/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let doc: serde_json::Value = body_json(res.into_body()).await;
    assert!(doc["paths"]["/v1/sessions/{id}/connections/{conn_id}/complete"].is_object());
    assert!(doc["paths"]
        ["/v1/sessions/{id}/connections/{conn_id}/semantic-documents/{document}/complete"]
        .is_object());
    assert!(doc["paths"]
        ["/v1/sessions/{id}/connections/{conn_id}/semantic-documents/{document}/hover"]
        .is_object());
    assert!(doc["paths"]
        ["/v1/sessions/{id}/connections/{conn_id}/semantic-documents/{document}/star-expansions/prepare"]
        .is_object());
    assert!(doc["components"]["schemas"]["CompletionRequest"].is_object());
    assert!(doc["components"]["schemas"]["CompletionResponse"].is_object());
    assert!(doc["components"]["schemas"]["CompletionCandidate"].is_object());
    assert!(doc["components"]["schemas"]["SemanticCompletionRequest"].is_object());
    assert!(doc["components"]["schemas"]["SemanticHoverRequest"].is_object());
    assert!(doc["components"]["schemas"]["SemanticHoverResponse"].is_object());
    assert!(doc["components"]["schemas"]["PrepareStarExpansionRequest"].is_object());
    assert!(doc["components"]["schemas"]["StarExpansionPreview"].is_object());
}

#[tokio::test]
async fn completion_catalogs_do_not_leak_between_connections() {
    let server = |database: &str| ServerInfo {
        provider: Engine::Postgres.provider_ref("test"),
        server_version: "MockDB 0.1".into(),
        current_database: database.into(),
        current_user: "mock".into(),
        pool_warm_slots: None,
    };
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .ping_ok(server("alpha"))
        .ping_ok(server("beta"))
        .schema_ok(single_table_snapshot("alpha", "alpha_accounts"))
        .schema_ok(single_table_snapshot("beta", "beta_accounts"))
        .build();
    let router = app(state_with_registry(
        DriverRegistry::builder().register(driver).build(),
    ));
    let session: sift_protocol::SessionInfo = body_json(
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
    let mut connection_ids = Vec::new();
    for database in ["alpha", "beta"] {
        let response = router
            .clone()
            .oneshot(post_json(
                format!("/v1/sessions/{}/connections", session.id),
                serde_json::json!({
                    "provider_id": "sift/postgres",
                    "host": "mock.invalid",
                    "port": 5432,
                    "database": database,
                    "user": "mock",
                    "ssl_mode": "disable"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let connection: sift_protocol::ConnectionInfo = body_json(response.into_body()).await;
        connection_ids.push(connection.id);
    }
    for (connection, expected, forbidden) in [
        (connection_ids[0], "alpha_accounts", "beta_accounts"),
        (connection_ids[1], "beta_accounts", "alpha_accounts"),
    ] {
        let response: CompletionResponse = body_json(
            router
                .clone()
                .oneshot(post_json(
                    format!(
                        "/v1/sessions/{}/connections/{connection}/complete",
                        session.id
                    ),
                    CompletionRequest {
                        sql: "SELECT * FROM ".into(),
                        cursor: 14,
                        limit: Some(20),
                    },
                ))
                .await
                .unwrap()
                .into_body(),
        )
        .await;
        assert!(response
            .candidates
            .iter()
            .any(|candidate| candidate.label == expected));
        assert!(!response
            .candidates
            .iter()
            .any(|candidate| candidate.label == forbidden));
    }
}
