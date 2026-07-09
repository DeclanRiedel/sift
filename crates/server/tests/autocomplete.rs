//! HTTP integration test for the Phase D autocomplete endpoint.
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
    }
}

fn mock_driver() -> MockDriver {
    MockDriver::builder()
        .engine(Engine::Postgres)
        .ping_ok(ServerInfo {
            engine: Engine::Postgres,
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
    let router = app(state());

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
        "engine": "postgres",
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
    let (router, sid, cid) = setup().await;
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
    let labels: Vec<&str> = resp.candidates.iter().map(|c| c.label.as_str()).collect();
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
    assert!(doc["components"]["schemas"]["CompletionRequest"].is_object());
    assert!(doc["components"]["schemas"]["CompletionResponse"].is_object());
    assert!(doc["components"]["schemas"]["CompletionCandidate"].is_object());
}
