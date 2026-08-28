use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use sift_driver_api::mock::MockDriver;
use sift_protocol::{
    CatalogCoverage, CatalogGraphOptions, CatalogTree, ColumnMetadata, CompareColumnPair,
    CompareKey, CompareSource, ComparisonStatus, ConstraintInfo, ConstraintKind, Engine,
    Nullability, ObjectInfo, ObjectKind, Page, PrimitiveType, Row, SchemaDepth, SchemaScope,
    SchemaSnapshot, SchemaTree, TypeRef, Value,
};
use sift_server::http::{app, AppState, AuthState};
use sift_server::registry::DriverRegistry;
use sift_server::room_runtime::RoomRuntime;
use sift_server::session::SessionStore;
use tower::ServiceExt;

fn post(uri: impl AsRef<str>, body: impl serde::Serialize) -> Request<Body> {
    Request::post(uri.as_ref())
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn graph_snapshot() -> SchemaSnapshot {
    let mut id = ColumnMetadata::new("id", TypeRef::Primitive(PrimitiveType::Int64));
    id.nullable = Nullability::NotNullable;
    id.primary_key = true;
    let mut name = ColumnMetadata::new("name", TypeRef::Primitive(PrimitiveType::Text));
    name.nullable = Nullability::NotNullable;
    let mut users = ObjectInfo::new("users", ObjectKind::Table);
    users.columns = vec![id, name];
    users.constraints = vec![ConstraintInfo {
        name: "users_pkey".into(),
        kind: ConstraintKind::PrimaryKey,
        columns: vec!["id".into()],
        definition: None,
        references: None,
    }];
    let trees = vec![CatalogTree {
        name: "app".into(),
        schemas: vec![SchemaTree {
            name: "public".into(),
            objects: vec![users],
        }],
    }];
    let graph =
        sift_core::catalog::graph_from_trees(&trees, CatalogCoverage::complete(), "mock:app");
    SchemaSnapshot {
        trees,
        fetched_at: chrono::Utc::now(),
        scope: SchemaScope {
            depth: SchemaDepth::Graph {
                options: CatalogGraphOptions::default(),
            },
            filter: None,
        },
        incomplete: false,
        graph: Some(graph),
    }
}

fn get(uri: impl AsRef<str>) -> Request<Body> {
    Request::get(uri.as_ref()).body(Body::empty()).unwrap()
}

async fn json<T: serde::de::DeserializeOwned>(body: Body) -> T {
    let bytes = to_bytes(body, 4 * 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        panic!(
            "decode response: {error}: {}",
            String::from_utf8_lossy(&bytes)
        )
    })
}

fn pages(rows: Vec<Row>) -> Vec<Page> {
    vec![
        Page::NextResult {
            columns: vec![
                ColumnMetadata::new("id", TypeRef::Primitive(PrimitiveType::Int64)),
                ColumnMetadata::new("name", TypeRef::Primitive(PrimitiveType::Text)),
            ],
        },
        Page::Rows { rows },
        Page::Done {
            affected_rows: None,
            warnings: Vec::new(),
        },
    ]
}

#[tokio::test]
async fn retained_query_comparison_is_revision_bound_paged_and_audited() {
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .execute_ok(pages(vec![
            Row::new(vec![Value::Int64(1), Value::Text("alice".into())]),
            Row::new(vec![Value::Int64(2), Value::Text("bob".into())]),
        ]))
        .execute_ok(pages(vec![
            Row::new(vec![Value::Int64(1), Value::Text("alicia".into())]),
            Row::new(vec![Value::Int64(3), Value::Text("carol".into())]),
        ]))
        .build();
    let state = AppState {
        sessions: SessionStore::new(DriverRegistry::builder().register(driver).build()),
        rooms: RoomRuntime::default(),
        auth: AuthState::default(),
        metadata: None,
        shutdown: sift_server::shutdown::Shutdown::default(),
    };
    let router = app(state);

    let session: sift_protocol::SessionInfo = json(
        router
            .clone()
            .oneshot(post("/v1/sessions", serde_json::json!({})))
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let connection: sift_protocol::ConnectionInfo = json(
        router
            .clone()
            .oneshot(post(
                format!("/v1/sessions/{}/connections", session.id),
                serde_json::json!({
                    "provider_id": "sift/postgres",
                    "host": "mock.invalid",
                    "user": "fixture"
                }),
            ))
            .await
            .unwrap()
            .into_body(),
    )
    .await;

    let execute = |sql: &'static str| {
        post(
            format!("/v1/sessions/{}/queries", session.id),
            sift_protocol::ExecuteRequestHttp {
                connection: connection.id,
                sql: sql.into(),
                params: Vec::new(),
                tx: None,
                room_id: None,
                connection_profile_id: None,
                transform: None,
                source: None,
            },
        )
    };
    let left: sift_protocol::ExecuteResponse = json(
        router
            .clone()
            .oneshot(execute("SELECT left_rows"))
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let right: sift_protocol::ExecuteResponse = json(
        router
            .clone()
            .oneshot(execute("SELECT right_rows"))
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    assert!(left.schema_digest.starts_with("schemafp:"));

    let started = router
        .clone()
        .oneshot(post(
            format!("/v1/sessions/{}/comparisons", session.id),
            sift_protocol::StartComparisonRequest {
                left: CompareSource::QueryResult {
                    cursor_id: left.cursor_id,
                    result_set: 0,
                    schema_digest: left.schema_digest,
                },
                right: CompareSource::QueryResult {
                    cursor_id: right.cursor_id,
                    result_set: 0,
                    schema_digest: right.schema_digest,
                },
                column_mappings: Vec::new(),
                key: CompareKey::Explicit {
                    columns: vec![CompareColumnPair {
                        left: "id".into(),
                        right: "id".into(),
                    }],
                },
                tolerances: Vec::new(),
                max_source_rows: None,
                max_diff_rows: None,
                timeout_ms: None,
            },
        ))
        .await
        .unwrap();
    assert_eq!(started.status(), StatusCode::OK);
    let started: sift_protocol::ComparisonSummary = json(started.into_body()).await;
    assert_eq!(started.status, ComparisonStatus::Running);

    let summary_uri = format!(
        "/v1/sessions/{}/comparisons/{}",
        session.id, started.comparison_id
    );
    let summary = loop {
        let summary: sift_protocol::ComparisonSummary = json(
            router
                .clone()
                .oneshot(get(&summary_uri))
                .await
                .unwrap()
                .into_body(),
        )
        .await;
        if summary.status != ComparisonStatus::Running {
            break summary;
        }
        tokio::task::yield_now().await;
    };
    assert_eq!(summary.status, ComparisonStatus::Complete);
    assert_eq!(summary.changed_rows, 1);
    assert_eq!(summary.added_rows, 1);
    assert_eq!(summary.removed_rows, 1);
    assert_eq!(summary.retained_diff_rows, 3);
    assert!(!summary.patch_eligible);

    let page: sift_protocol::ComparisonPage = json(
        router
            .clone()
            .oneshot(post(
                format!("{summary_uri}/pages"),
                sift_protocol::ComparisonPageRequest {
                    after: None,
                    limit: Some(2),
                },
            ))
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    assert_eq!(page.rows.len(), 2);
    assert!(page.next.is_some());
    let second: sift_protocol::ComparisonPage = json(
        router
            .clone()
            .oneshot(post(
                format!("{summary_uri}/pages"),
                sift_protocol::ComparisonPageRequest {
                    after: page.next,
                    limit: Some(2),
                },
            ))
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    assert_eq!(second.rows.len(), 1);
    assert!(second.next.is_none());

    let operations: Vec<sift_protocol::OperationAuditEntry> = json(
        router
            .oneshot(get("/v1/operations"))
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let comparison_operations = operations
        .iter()
        .filter(|entry| {
            matches!(
                entry.operation,
                sift_protocol::Operation::StartComparison { .. }
                    | sift_protocol::Operation::PageComparison { .. }
            )
        })
        .collect::<Vec<_>>();
    assert!(comparison_operations.len() >= 3);
    let replay = serde_json::to_string(&comparison_operations).unwrap();
    assert!(!replay.contains("alice"));
    assert!(!replay.contains("alicia"));
}

#[tokio::test]
async fn live_table_comparison_prepares_parameterized_optimistic_patch() {
    let desired = vec![
        Row::new(vec![Value::Int64(1), Value::Text("alicia".into())]),
        Row::new(vec![Value::Int64(3), Value::Text("carol".into())]),
    ];
    let current = vec![
        Row::new(vec![Value::Int64(1), Value::Text("alice".into())]),
        Row::new(vec![Value::Int64(2), Value::Text("bob".into())]),
    ];
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .schema_ok(graph_snapshot())
        .schema_ok(graph_snapshot())
        .execute_ok(pages(desired))
        .execute_ok(pages(current))
        .execute_ok(vec![Page::Done {
            affected_rows: Some(0),
            warnings: Vec::new(),
        }])
        .build();
    let router = app(AppState {
        sessions: SessionStore::new(DriverRegistry::builder().register(driver).build()),
        rooms: RoomRuntime::default(),
        auth: AuthState::default(),
        metadata: None,
        shutdown: sift_server::shutdown::Shutdown::default(),
    });
    let session: sift_protocol::SessionInfo = json(
        router
            .clone()
            .oneshot(post("/v1/sessions", serde_json::json!({})))
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let connection: sift_protocol::ConnectionInfo = json(
        router
            .clone()
            .oneshot(post(
                format!("/v1/sessions/{}/connections", session.id),
                serde_json::json!({
                    "provider_id": "sift/postgres",
                    "host": "mock.invalid",
                    "user": "fixture"
                }),
            ))
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let graph: sift_protocol::CatalogGraph = json(
        router
            .clone()
            .oneshot(post(
                format!(
                    "/v1/sessions/{}/connections/{}/catalog/graph",
                    session.id, connection.id
                ),
                serde_json::json!({}),
            ))
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let table_id = graph
        .data
        .nodes
        .iter()
        .find(|node| node.kind == sift_protocol::CatalogNodeKind::Table)
        .unwrap()
        .id
        .clone();
    let desired: sift_protocol::ExecuteResponse = json(
        router
            .clone()
            .oneshot(post(
                format!("/v1/sessions/{}/queries", session.id),
                sift_protocol::ExecuteRequestHttp {
                    connection: connection.id,
                    sql: "SELECT desired_rows".into(),
                    params: Vec::new(),
                    tx: None,
                    room_id: None,
                    connection_profile_id: None,
                    transform: None,
                    source: None,
                },
            ))
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let started: sift_protocol::ComparisonSummary = json(
        router
            .clone()
            .oneshot(post(
                format!("/v1/sessions/{}/comparisons", session.id),
                sift_protocol::StartComparisonRequest {
                    left: CompareSource::Table {
                        connection: connection.id,
                        catalog_revision: graph.revision,
                        object_id: table_id,
                        filter: None,
                    },
                    right: CompareSource::QueryResult {
                        cursor_id: desired.cursor_id,
                        result_set: 0,
                        schema_digest: desired.schema_digest,
                    },
                    column_mappings: Vec::new(),
                    key: CompareKey::Infer,
                    tolerances: Vec::new(),
                    max_source_rows: None,
                    max_diff_rows: None,
                    timeout_ms: None,
                },
            ))
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let summary_uri = format!(
        "/v1/sessions/{}/comparisons/{}",
        session.id, started.comparison_id
    );
    let summary = loop {
        let summary: sift_protocol::ComparisonSummary = json(
            router
                .clone()
                .oneshot(get(&summary_uri))
                .await
                .unwrap()
                .into_body(),
        )
        .await;
        if summary.status != ComparisonStatus::Running {
            break summary;
        }
        tokio::task::yield_now().await;
    };
    assert_eq!(summary.status, ComparisonStatus::Complete);
    assert!(
        summary.patch_eligible,
        "{:?}",
        summary.patch_refusal_reasons
    );
    assert!(summary.key.inferred_constraint.is_some());

    let prepared = router
        .clone()
        .oneshot(post(
            format!("{summary_uri}/patch"),
            sift_protocol::PrepareComparisonPatchRequest {
                expected_catalog_revision: graph.revision,
                max_statements: Some(3),
            },
        ))
        .await
        .unwrap();
    assert_eq!(prepared.status(), StatusCode::OK);
    let prepared: sift_protocol::ComparisonPatchPreparation = json(prepared.into_body()).await;
    assert!(prepared.eligible, "{:?}", prepared.refusal_reasons);
    let plan = prepared.edit_plan.unwrap();
    assert_eq!(plan.statements.len(), 3);
    assert_eq!(
        plan.statements[0].kind,
        sift_protocol::EditStatementKind::Delete
    );
    assert_eq!(
        plan.statements[1].kind,
        sift_protocol::EditStatementKind::Update
    );
    assert_eq!(
        plan.statements[2].kind,
        sift_protocol::EditStatementKind::Insert
    );
    assert!(plan.statements.iter().all(|statement| {
        !statement.sql.contains("alice")
            && !statement.sql.contains("alicia")
            && !statement.params.is_empty()
    }));
    let edit_set = prepared.edit_set.unwrap();
    let conflict = router
        .oneshot(post(
            format!(
                "/v1/sessions/{}/connections/{}/edits/apply",
                session.id, connection.id
            ),
            sift_protocol::ApplyEditsRequest {
                connection: connection.id,
                edit_set,
                tx: None,
            },
        ))
        .await
        .unwrap();
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn live_comparison_cancels_while_source_query_is_in_flight() {
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .schema_ok(graph_snapshot())
        .execute_delay(std::time::Duration::from_millis(200))
        .execute_ok(pages(vec![Row::new(vec![
            Value::Int64(1),
            Value::Text("private row".into()),
        ])]))
        .build();
    let sessions = SessionStore::new(DriverRegistry::builder().register(driver).build());
    let router = app(AppState {
        sessions: sessions.clone(),
        rooms: RoomRuntime::default(),
        auth: AuthState::default(),
        metadata: None,
        shutdown: sift_server::shutdown::Shutdown::default(),
    });
    let session: sift_protocol::SessionInfo = json(
        router
            .clone()
            .oneshot(post("/v1/sessions", serde_json::json!({})))
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let connection: sift_protocol::ConnectionInfo = json(
        router
            .clone()
            .oneshot(post(
                format!("/v1/sessions/{}/connections", session.id),
                serde_json::json!({
                    "provider_id": "sift/postgres",
                    "host": "mock.invalid",
                    "user": "fixture"
                }),
            ))
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let graph: sift_protocol::CatalogGraph = json(
        router
            .clone()
            .oneshot(post(
                format!(
                    "/v1/sessions/{}/connections/{}/catalog/graph",
                    session.id, connection.id
                ),
                serde_json::json!({}),
            ))
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    let table = graph
        .data
        .nodes
        .iter()
        .find(|node| node.kind == sift_protocol::CatalogNodeKind::Table)
        .unwrap()
        .id
        .clone();
    let source = CompareSource::Table {
        connection: connection.id,
        catalog_revision: graph.revision,
        object_id: table,
        filter: None,
    };
    let started: sift_protocol::ComparisonSummary = json(
        router
            .clone()
            .oneshot(post(
                format!("/v1/sessions/{}/comparisons", session.id),
                sift_protocol::StartComparisonRequest {
                    left: source.clone(),
                    right: source,
                    column_mappings: Vec::new(),
                    key: CompareKey::Infer,
                    tolerances: Vec::new(),
                    max_source_rows: None,
                    max_diff_rows: None,
                    timeout_ms: None,
                },
            ))
            .await
            .unwrap()
            .into_body(),
    )
    .await;
    assert_eq!(started.status, ComparisonStatus::Running);
    let base = format!(
        "/v1/sessions/{}/comparisons/{}",
        session.id, started.comparison_id
    );
    let canceled = router
        .clone()
        .oneshot(post(format!("{base}/cancel"), serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(canceled.status(), StatusCode::OK);
    let canceled: sift_protocol::CancelComparisonResponse = json(canceled.into_body()).await;
    assert_eq!(canceled.status, ComparisonStatus::Canceled);
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    let summary: sift_protocol::ComparisonSummary =
        json(router.oneshot(get(&base)).await.unwrap().into_body()).await;
    assert_eq!(summary.status, ComparisonStatus::Canceled);
    let replay = serde_json::to_string(&sessions.list_operations()).unwrap();
    assert!(replay.contains("cancel_comparison"));
    assert!(!replay.contains("private row"));
}
