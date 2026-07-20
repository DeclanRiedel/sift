//! HTTP integration tests for the Phase D execution-plan endpoint
//! (`/explain`) over a `MockDriver`.

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use sift_driver_api::mock::MockDriver;
use sift_protocol::{
    ColumnMetadata, Engine, ExplainResponse, Page, PrimitiveType, Row, ServerInfo, TypeRef, Value,
};
use sift_server::http::{app, AppState, AuthState};
use sift_server::registry::DriverRegistry;
use sift_server::room_runtime::RoomRuntime;
use sift_server::session::SessionStore;
use tower::ServiceExt;

fn base_builder(engine: Engine) -> sift_driver_api::mock::MockDriverBuilder {
    MockDriver::builder().engine(engine).ping_ok(ServerInfo {
        engine,
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
    engine: &str,
    port: u16,
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
                "engine": engine, "host": "mock.invalid", "port": port,
                "database": "mock", "user": "mock", "ssl_mode": "disable",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let conn: sift_protocol::ConnectionInfo = body_json(res.into_body()).await;
    (router, sid, conn.id)
}

fn text_col(name: &str) -> ColumnMetadata {
    ColumnMetadata::new(name, TypeRef::Primitive(PrimitiveType::Text))
}

/// One PG EXPLAIN (FORMAT JSON) row: a single json-typed cell.
fn pg_plan_pages() -> Vec<Page> {
    let plan = serde_json::json!([{
        "Plan": {
            "Node Type": "Seq Scan",
            "Relation Name": "users",
            "Plan Rows": 100,
            "Total Cost": 12.5,
            "Filter": "(id > 5)"
        }
    }]);
    vec![
        Page::NextResult {
            columns: vec![ColumnMetadata::new(
                "QUERY PLAN",
                TypeRef::Primitive(PrimitiveType::Json),
            )],
        },
        Page::Rows {
            rows: vec![Row::new(vec![Value::Json(plan)])],
        },
        Page::Done {
            affected_rows: None,
            warnings: vec![],
        },
    ]
}

#[tokio::test]
async fn pg_explain_estimate_returns_typed_plan() {
    let driver = base_builder(Engine::Postgres)
        .execute_ok(pg_plan_pages())
        .build();
    let (router, sid, cid) = setup(driver, "postgres", 5432).await;

    let res = router
        .oneshot(post_json(
            format!("/v1/sessions/{sid}/connections/{cid}/explain"),
            serde_json::json!({ "connection": cid, "sql": "SELECT * FROM users", "analyze": false }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let resp: ExplainResponse = body_json(res.into_body()).await;
    assert_eq!(resp.engine, Engine::Postgres);
    assert!(!resp.analyzed);
    assert_eq!(resp.root.op, "Seq Scan");
    assert_eq!(resp.root.relation.as_deref(), Some("users"));
    assert_eq!(resp.root.est_rows, Some(100.0));
    assert!(resp.root.extra.contains_key("Filter"));
}

#[tokio::test]
async fn pg_explain_analyze_write_is_wrapped_and_rolled_back() {
    // begin + execute (plan) + rollback are all default-permissive on the mock;
    // only the one plan-producing execute needs canned pages.
    let driver = base_builder(Engine::Postgres)
        .execute_ok(pg_plan_pages())
        .build();
    let (router, sid, cid) = setup(driver, "postgres", 5432).await;

    let res = router
        .oneshot(post_json(
            format!("/v1/sessions/{sid}/connections/{cid}/explain"),
            serde_json::json!({
                "connection": cid,
                "sql": "DELETE FROM users WHERE id = 1",
                "analyze": true
            }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let resp: ExplainResponse = body_json(res.into_body()).await;
    assert!(resp.analyzed);
    assert_eq!(resp.root.op, "Seq Scan");
}

#[tokio::test]
async fn mssql_explain_estimate_parses_showplan_xml() {
    let xml = r#"<ShowPlanXML xmlns="http://schemas.microsoft.com/sqlserver/2004/07/showplan">
<BatchSequence><Batch><Statements><StmtSimple><QueryPlan>
<RelOp PhysicalOp="Clustered Index Scan" EstimateRows="42" EstimatedTotalSubtreeCost="0.3">
<IndexScan><Object Table="[users]"/></IndexScan>
</RelOp>
</QueryPlan></StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>"#;
    let driver = base_builder(Engine::SqlServer)
        // SET SHOWPLAN_XML ON
        .execute_ok(vec![Page::Done {
            affected_rows: None,
            warnings: vec![],
        }])
        // the query, returning the plan XML
        .execute_ok(vec![
            Page::NextResult {
                columns: vec![text_col("Microsoft SQL Server 2005 XML Showplan")],
            },
            Page::Rows {
                rows: vec![Row::new(vec![Value::Text(xml.into())])],
            },
            Page::Done {
                affected_rows: None,
                warnings: vec![],
            },
        ])
        // SET SHOWPLAN_XML OFF
        .execute_ok(vec![Page::Done {
            affected_rows: None,
            warnings: vec![],
        }])
        .build();
    let (router, sid, cid) = setup(driver, "sql_server", 1433).await;

    let res = router
        .oneshot(post_json(
            format!("/v1/sessions/{sid}/connections/{cid}/explain"),
            serde_json::json!({ "connection": cid, "sql": "SELECT * FROM users", "analyze": false }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let resp: ExplainResponse = body_json(res.into_body()).await;
    assert_eq!(resp.engine, Engine::SqlServer);
    assert_eq!(resp.root.op, "Clustered Index Scan");
    assert_eq!(resp.root.relation.as_deref(), Some("users"));
    assert_eq!(resp.root.est_rows, Some(42.0));
}

#[tokio::test]
async fn mssql_explain_analyze_is_rejected() {
    let driver = base_builder(Engine::SqlServer).build();
    let (router, sid, cid) = setup(driver, "sql_server", 1433).await;

    let res = router
        .oneshot(post_json(
            format!("/v1/sessions/{sid}/connections/{cid}/explain"),
            serde_json::json!({ "connection": cid, "sql": "SELECT 1", "analyze": true }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
}
