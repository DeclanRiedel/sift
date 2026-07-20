//! HTTP integration tests for the Phase D search endpoints
//! (`/search/schema`, `/search/data`) over a `MockDriver`.

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use sift_driver_api::mock::MockDriver;
use sift_protocol::{
    CatalogTree, ColumnMetadata, DataSearchResponse, Engine, ObjectInfo, ObjectKind, Page,
    PrimitiveType, Row, SchemaScope, SchemaSearchResponse, SchemaSnapshot, SchemaTree,
    SearchTarget, ServerInfo, TypeRef, Value,
};
use sift_server::http::{app, AppState, AuthState};
use sift_server::registry::DriverRegistry;
use sift_server::room_runtime::RoomRuntime;
use sift_server::session::SessionStore;
use tower::ServiceExt;

fn shallow() -> SchemaSnapshot {
    SchemaSnapshot {
        trees: vec![CatalogTree {
            name: "mock".into(),
            schemas: vec![SchemaTree {
                name: "public".into(),
                objects: vec![
                    ObjectInfo::new("users", ObjectKind::Table),
                    ObjectInfo::new("orders", ObjectKind::Table),
                ],
            }],
        }],
        fetched_at: chrono::Utc::now(),
        scope: SchemaScope::shallow(),
        incomplete: false,
    }
}

fn text_col(name: &str) -> ColumnMetadata {
    ColumnMetadata::new(name, TypeRef::Primitive(PrimitiveType::Text))
}

fn row(vals: &[&str]) -> Row {
    Row::new(vals.iter().map(|s| Value::Text((*s).into())).collect())
}

/// Canned pages for the bulk column catalog query.
fn bulk_pages() -> Vec<Page> {
    vec![
        Page::NextResult {
            columns: vec![
                text_col("table_schema"),
                text_col("table_name"),
                text_col("column_name"),
                text_col("data_type"),
            ],
        },
        Page::Rows {
            rows: vec![
                row(&["public", "users", "email", "text"]),
                row(&["public", "users", "id", "integer"]),
                row(&["public", "orders", "note", "text"]),
            ],
        },
        Page::Done {
            affected_rows: None,
            warnings: vec![],
        },
    ]
}

fn base_builder() -> sift_driver_api::mock::MockDriverBuilder {
    MockDriver::builder()
        .engine(Engine::Postgres)
        .ping_ok(ServerInfo {
            engine: Engine::Postgres,
            server_version: "MockDB 0.1".into(),
            current_database: "mock".into(),
            current_user: "mock".into(),
            pool_warm_slots: None,
        })
}

fn state_with(driver: MockDriver) -> AppState {
    let registry = DriverRegistry::builder().register(driver).build();
    AppState {
        sessions: SessionStore::new(registry),
        rooms: RoomRuntime::default(),
        shutdown: sift_server::shutdown::Shutdown::default(),
        auth: AuthState::default(),
        metadata: None,
    }
}

fn post_json(uri: String, body: serde_json::Value) -> Request<Body> {
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

async fn setup(
    driver: MockDriver,
) -> (
    axum::Router,
    sift_protocol::SessionId,
    sift_protocol::ConnectionId,
) {
    let router = app(state_with(driver));
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
    let session: sift_protocol::SessionInfo = body_json(res.into_body()).await;
    let sid = session.id;
    let res = router
        .clone()
        .oneshot(post_json(
            format!("/v1/sessions/{sid}/connections"),
            serde_json::json!({
                "engine": "postgres", "host": "mock.invalid", "port": 5432,
                "database": "mock", "user": "mock", "ssl_mode": "disable",
            }),
        ))
        .await
        .unwrap();
    let conn: sift_protocol::ConnectionInfo = body_json(res.into_body()).await;
    (router, sid, conn.id)
}

#[tokio::test]
async fn schema_search_finds_object_and_column() {
    let driver = base_builder()
        .schema_ok(shallow())
        .execute_ok(bulk_pages())
        .build();
    let (router, sid, cid) = setup(driver).await;

    let res = router
        .oneshot(post_json(
            format!("/v1/sessions/{sid}/connections/{cid}/search/schema"),
            serde_json::json!({ "query": "email" }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let resp: SchemaSearchResponse = body_json(res.into_body()).await;
    assert!(
        resp.hits
            .iter()
            .any(|h| h.display == "public.users.email" && matches!(h.target, SearchTarget::Column)),
        "email column hit missing: {:?}",
        resp.hits.iter().map(|h| &h.display).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn schema_search_kinds_filter_excludes_nonmatching_objects() {
    let driver = base_builder()
        .schema_ok(shallow())
        .execute_ok(bulk_pages())
        .build();
    let (router, sid, cid) = setup(driver).await;

    let res = router
        .oneshot(post_json(
            format!("/v1/sessions/{sid}/connections/{cid}/search/schema"),
            serde_json::json!({ "query": "users", "kinds": ["view"] }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let resp: SchemaSearchResponse = body_json(res.into_body()).await;
    // No table objects should appear (filter = view only); the users table is excluded.
    assert!(!resp.hits.iter().any(|h| matches!(
        h.target,
        SearchTarget::Object {
            object_kind: ObjectKind::Table
        }
    )));
}

#[tokio::test]
async fn data_search_returns_matching_rows() {
    let driver = base_builder()
        .schema_ok(shallow())
        .execute_ok(bulk_pages())
        // The per-table data query result for `users` (only `email` is text-ish).
        .execute_ok(vec![
            Page::NextResult {
                columns: vec![text_col("email")],
            },
            Page::Rows {
                rows: vec![row(&["found@example.com"])],
            },
            Page::Done {
                affected_rows: None,
                warnings: vec![],
            },
        ])
        .build();
    let (router, sid, cid) = setup(driver).await;

    let res = router
        .oneshot(post_json(
            format!("/v1/sessions/{sid}/connections/{cid}/search/data"),
            serde_json::json!({
                "scope": {"scope": "table", "table": {"schema": "public", "name": "users"}},
                "query": "found",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let resp: DataSearchResponse = body_json(res.into_body()).await;
    assert_eq!(resp.tables_searched, 1);
    assert_eq!(resp.hits.len(), 1);
    assert_eq!(resp.hits[0].matched_columns, vec!["email".to_string()]);
}
