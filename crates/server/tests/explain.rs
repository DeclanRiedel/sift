//! HTTP integration tests for the Phase D execution-plan endpoint
//! (`/explain`) over a `MockDriver`.

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use sift_driver_api::mock::MockDriver;
use sift_metadata::{
    CredentialMode, MemorySecretStore, MetadataStore, NewConnectionProfile, PrincipalId, TenantId,
};
use sift_protocol::{
    CatalogCoverage, CatalogGraph, CatalogGraphOptions, CatalogTree, ColumnMetadata, Engine,
    ExplainResponse, ObjectInfo, ObjectKind, Page, PrimitiveType, Row, SchemaDepth, SchemaScope,
    SchemaSnapshot, SchemaTree, ServerInfo, TypeRef, Value,
};
use sift_server::http::{app, AppState, AuthState};
use sift_server::registry::DriverRegistry;
use sift_server::room_runtime::RoomRuntime;
use sift_server::session::SessionStore;
use std::sync::Arc;
use tower::ServiceExt;

fn base_builder(engine: Engine) -> sift_driver_api::mock::MockDriverBuilder {
    MockDriver::builder().engine(engine).ping_ok(ServerInfo {
        provider: engine.provider_ref("test"),
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
    provider_id: &str,
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
                "provider_id": provider_id, "host": "mock.invalid", "port": port,
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

fn graph_snapshot() -> SchemaSnapshot {
    let trees = vec![CatalogTree {
        name: "mock".into(),
        schemas: vec![SchemaTree {
            name: "public".into(),
            objects: vec![ObjectInfo::new("users", ObjectKind::Table)],
        }],
    }];
    SchemaSnapshot {
        graph: Some(sift_core::catalog::graph_from_trees(
            &trees,
            CatalogCoverage::complete(),
            "mock:db",
        )),
        trees,
        fetched_at: chrono::Utc::now(),
        scope: SchemaScope {
            depth: SchemaDepth::Graph {
                options: CatalogGraphOptions::default(),
            },
            filter: None,
        },
        incomplete: false,
    }
}

#[tokio::test]
async fn pg_explain_estimate_returns_typed_plan() {
    let driver = base_builder(Engine::Postgres)
        .execute_ok(pg_plan_pages())
        .build();
    let (router, sid, cid) = setup(driver, "sift/postgres", 5432).await;

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
    let (router, sid, cid) = setup(driver, "sift/postgres", 5432).await;

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
    let (router, sid, cid) = setup(driver, "sift/sql-server", 1433).await;

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
    let (router, sid, cid) = setup(driver, "sift/sql-server", 1433).await;

    let res = router
        .oneshot(post_json(
            format!("/v1/sessions/{sid}/connections/{cid}/explain"),
            serde_json::json!({ "connection": cid, "sql": "SELECT 1", "analyze": true }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn semantic_plan_capture_binds_revision_and_never_persists_raw_plan() {
    let server_info = ServerInfo {
        provider: Engine::Postgres.provider_ref("test"),
        server_version: "MockDB 0.1".into(),
        current_database: "mock".into(),
        current_user: "mock".into(),
        pool_warm_slots: None,
    };
    let driver = base_builder(Engine::Postgres)
        .ping_ok(server_info.clone())
        .ping_ok(server_info)
        .schema_ok(graph_snapshot())
        .execute_ok(pg_plan_pages())
        .execute_ok(pg_plan_pages())
        .build();
    let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
    metadata.bootstrap_local("local user").unwrap();
    let profile = metadata
        .upsert_connection_profile(
            TenantId(1),
            PrincipalId(1),
            NewConnectionProfile {
                name: "plan fixture".into(),
                provider_id: Engine::Postgres.provider_id(),
                configuration: serde_json::json!({
                    "host": "mock.invalid",
                    "database": "mock",
                    "user": "mock"
                }),
                semantic_engine: Some(Engine::Postgres),
                credentials: None,
                credential_mode: CredentialMode::Shared,
                tags: Vec::new(),
            },
        )
        .await
        .unwrap();
    let sessions = SessionStore::new(DriverRegistry::builder().register(driver).build());
    let router = app(AppState {
        sessions: sessions.clone(),
        rooms: RoomRuntime::default(),
        shutdown: sift_server::shutdown::Shutdown::default(),
        auth: AuthState::default(),
        metadata: Some(metadata),
    });
    let response = router
        .clone()
        .oneshot(post_json("/v1/sessions".into(), serde_json::json!({})))
        .await
        .unwrap();
    let session: sift_protocol::SessionInfo = body_json(response.into_body()).await;
    let response = router
        .clone()
        .oneshot(post_json(
            format!("/v1/sessions/{}/connections/from-profile", session.id),
            serde_json::json!({"tenant_id": 1, "profile_id": profile.id.0}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let connection: sift_protocol::ConnectionInfo = body_json(response.into_body()).await;
    let response = router
        .clone()
        .oneshot(post_json(
            format!(
                "/v1/sessions/{}/connections/{}/catalog/graph",
                session.id, connection.id
            ),
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let graph: CatalogGraph = body_json(response.into_body()).await;
    let semantic_base = format!(
        "/v1/sessions/{}/connections/{}/semantic-documents",
        session.id, connection.id
    );
    let response = router
        .clone()
        .oneshot(post_json(
            semantic_base.clone(),
            serde_json::json!({"text": "SELECT * FROM users"}),
        ))
        .await
        .unwrap();
    let document: sift_protocol::SemanticDocumentState = body_json(response.into_body()).await;
    let response = router
        .clone()
        .oneshot(post_json(
            format!(
                "{}/{}/statements/select",
                semantic_base, document.document_id
            ),
            serde_json::json!({"revision": document.revision, "cursor": 1}),
        ))
        .await
        .unwrap();
    let selection: sift_protocol::StatementSelection = body_json(response.into_body()).await;
    let statement_id = selection.statements[0].statement_id.clone();
    let capture_uri = format!(
        "/v1/sessions/{}/connections/{}/plan-captures",
        session.id, connection.id
    );
    let request = serde_json::json!({
        "document_id": document.document_id,
        "revision": document.revision,
        "statement_id": statement_id,
        "catalog_revision": graph.revision,
        "include_raw_response": true
    });
    let response = router
        .clone()
        .oneshot(post_json(capture_uri.clone(), request.clone()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let first: sift_protocol::PlanCapture = body_json(response.into_body()).await;
    assert_eq!(first.source_digest, document.source_digest);
    assert_eq!(first.document_revision, document.revision);
    assert_eq!(first.catalog_revision, graph.revision);
    assert!(first
        .raw_response
        .as_deref()
        .is_some_and(|raw| raw.contains("Seq Scan")));
    assert!(!first.root.extra.contains_key("Filter"));

    let response = router
        .clone()
        .oneshot(
            Request::get(format!("/v1/metadata/tenants/1/plan-captures/{}", first.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let persisted: sift_protocol::PlanCapture = body_json(response.into_body()).await;
    assert!(persisted.raw_response.is_none());
    assert!(!serde_json::to_string(&persisted)
        .unwrap()
        .contains("id > 5"));

    let response = router
        .clone()
        .oneshot(post_json(capture_uri.clone(), request))
        .await
        .unwrap();
    let second: sift_protocol::PlanCapture = body_json(response.into_body()).await;
    let response = router
        .clone()
        .oneshot(post_json(
            "/v1/metadata/tenants/1/plan-captures/compare".into(),
            serde_json::json!({"left": first.id, "right": second.id}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let comparison: sift_protocol::PlanCaptureComparison = body_json(response.into_body()).await;
    assert_eq!(comparison.operator_changes, 0);

    let stale = router
        .clone()
        .oneshot(post_json(
            capture_uri,
            serde_json::json!({
                "document_id": document.document_id,
                "revision": document.revision + 1,
                "statement_id": selection.statements[0].statement_id,
                "catalog_revision": graph.revision
            }),
        ))
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::CONFLICT);
    let audit = serde_json::to_string(&sessions.list_operations()).unwrap();
    assert!(!audit.contains("SELECT * FROM users"));
    assert!(!audit.contains("id > 5"));
}
