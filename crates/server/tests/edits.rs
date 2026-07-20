//! HTTP integration tests for the Phase D inline-edit endpoints
//! (`/edits/preview`, `/edits/apply`). Boots axum over a `MockDriver` with a
//! canned deep `SchemaSnapshot` and canned execute pages.

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use sift_driver_api::mock::MockDriver;
use sift_protocol::{
    ApplyEditsResult, CatalogTree, ColumnMetadata, EditPlan, Engine, Nullability, ObjectInfo,
    ObjectKind, Page, PrimitiveType, SchemaScope, SchemaSnapshot, SchemaTree, ServerInfo, TypeRef,
};
use sift_server::http::{app, AppState, AuthState};
use sift_server::registry::DriverRegistry;
use sift_server::room_runtime::RoomRuntime;
use sift_server::session::SessionStore;
use tower::ServiceExt;

fn column(name: &str, ty: PrimitiveType, pk: bool, nullable: Nullability) -> ColumnMetadata {
    ColumnMetadata {
        name: name.into(),
        type_ref: TypeRef::Primitive(ty),
        nullable,
        auto_increment: false,
        primary_key: pk,
        facets: Default::default(),
    }
}

fn users(with_pk: bool) -> ObjectInfo {
    let mut o = ObjectInfo::new("users", ObjectKind::Table);
    o.columns = vec![
        column(
            "id",
            PrimitiveType::Int32,
            with_pk,
            Nullability::NotNullable,
        ),
        column("email", PrimitiveType::Text, false, Nullability::Nullable),
    ];
    o
}

fn deep_snapshot(with_pk: bool) -> SchemaSnapshot {
    SchemaSnapshot {
        trees: vec![CatalogTree {
            name: "mock".into(),
            schemas: vec![SchemaTree {
                name: "public".into(),
                objects: vec![users(with_pk)],
            }],
        }],
        fetched_at: chrono::Utc::now(),
        scope: SchemaScope::shallow(),
        incomplete: false,
    }
}

fn mock_builder() -> sift_driver_api::mock::MockDriverBuilder {
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
    assert_eq!(res.status(), StatusCode::OK);
    let session: sift_protocol::SessionInfo = body_json(res.into_body()).await;
    let sid = session.id;

    let res = router
        .clone()
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
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let conn: sift_protocol::ConnectionInfo = body_json(res.into_body()).await;
    (router, sid, conn.id)
}

fn update_edit_set(cid: sift_protocol::ConnectionId) -> serde_json::Value {
    serde_json::json!({
        "connection": cid,
        "edit_set": {
            "table": {"schema": "public", "name": "users", "kind": "table"},
            "edits": [{
                "kind": "update",
                "key": {"columns": [{"column": "id", "value": {"kind": "int32", "value": 1}}]},
                "changes": [{"column": "email", "value": {"kind": "text", "value": "new@x"}}],
                "expected": [{"column": "email", "value": {"kind": "text", "value": "old@x"}}]
            }]
        }
    })
}

#[tokio::test]
async fn preview_generates_parameterized_dml_without_executing() {
    // Only a schema fetch is canned — no execute. Preview must not touch it.
    let driver = mock_builder().schema_ok(deep_snapshot(true)).build();
    let (router, sid, cid) = setup(driver).await;

    let res = router
        .oneshot(post_json(
            format!("/v1/sessions/{sid}/connections/{cid}/edits/preview"),
            update_edit_set(cid),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let plan: EditPlan = body_json(res.into_body()).await;
    assert_eq!(plan.statements.len(), 1);
    assert_eq!(
        plan.statements[0].sql,
        r#"UPDATE "public"."users" SET "email" = $1 WHERE "id" = $2 AND "email" = $3"#
    );
}

#[tokio::test]
async fn apply_update_commits_and_reports_affected_rows() {
    let driver = mock_builder()
        .schema_ok(deep_snapshot(true))
        .execute_ok(vec![Page::Done {
            affected_rows: Some(1),
            warnings: vec![],
        }])
        .build();
    let (router, sid, cid) = setup(driver).await;

    let res = router
        .oneshot(post_json(
            format!("/v1/sessions/{sid}/connections/{cid}/edits/apply"),
            update_edit_set(cid),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let result: ApplyEditsResult = body_json(res.into_body()).await;
    assert!(result.committed);
    assert_eq!(result.applied.len(), 1);
    assert_eq!(result.applied[0].affected_rows, 1);
}

#[tokio::test]
async fn apply_update_conflict_when_zero_rows_affected() {
    // The row changed under the user: the optimistic WHERE matches nothing.
    let driver = mock_builder()
        .schema_ok(deep_snapshot(true))
        .execute_ok(vec![Page::Done {
            affected_rows: Some(0),
            warnings: vec![],
        }])
        .build();
    let (router, sid, cid) = setup(driver).await;

    let res = router
        .oneshot(post_json(
            format!("/v1/sessions/{sid}/connections/{cid}/edits/apply"),
            update_edit_set(cid),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn preview_rejects_table_without_row_identity() {
    let driver = mock_builder().schema_ok(deep_snapshot(false)).build();
    let (router, sid, cid) = setup(driver).await;

    let res = router
        .oneshot(post_json(
            format!("/v1/sessions/{sid}/connections/{cid}/edits/preview"),
            update_edit_set(cid),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
}
