use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use sift_driver_api::mock::MockDriver;
use sift_metadata::{
    CredentialMode, MemorySecretStore, MetadataStore, NewConnectionProfile, PrincipalId, TenantId,
};
use sift_protocol::{
    CatalogCoverage, CatalogGraph, CatalogGraphData, CatalogGraphOptions, CatalogTree, Engine,
    ObjectInfo, ObjectKind, SchemaDepth, SchemaScope, SchemaSnapshot, SchemaTree,
};
use sift_server::http::{app, AppState, AuthState};
use sift_server::registry::DriverRegistry;
use sift_server::room_runtime::RoomRuntime;
use sift_server::session::SessionStore;
use std::sync::Arc;
use tower::ServiceExt;

async fn json<T: serde::de::DeserializeOwned>(body: Body) -> T {
    let bytes = to_bytes(body, 4 * 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        panic!(
            "decode response: {error}: {}",
            String::from_utf8_lossy(&bytes)
        )
    })
}

fn post(uri: impl AsRef<str>, body: impl serde::Serialize) -> Request<Body> {
    Request::post(uri.as_ref())
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn get(uri: impl AsRef<str>) -> Request<Body> {
    Request::get(uri.as_ref()).body(Body::empty()).unwrap()
}

fn graph_snapshot() -> SchemaSnapshot {
    graph_snapshot_with_objects(vec![ObjectInfo::new("users", ObjectKind::Table)])
}

fn graph_snapshot_with_objects(objects: Vec<ObjectInfo>) -> SchemaSnapshot {
    let trees = vec![CatalogTree {
        name: "app".into(),
        schemas: vec![SchemaTree {
            name: "private_customer_data".into(),
            objects,
        }],
    }];
    let graph =
        sift_core::catalog::graph_from_trees(&trees, CatalogCoverage::complete(), "mock:db");
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

#[tokio::test]
async fn catalog_graph_is_revisioned_public_and_audit_safe() {
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .schema_ok(graph_snapshot())
        .schema_ok(graph_snapshot_with_objects(vec![
            ObjectInfo::new("users", ObjectKind::Table),
            ObjectInfo::new("events", ObjectKind::Table),
        ]))
        .schema_err(sift_protocol::DriverError::new(
            sift_protocol::Code::InvalidParameterValue,
            "private provider failure detail",
        ))
        .schema_ok(graph_snapshot_with_objects(vec![
            ObjectInfo::new("users", ObjectKind::Table),
            ObjectInfo::new("raced_publication", ObjectKind::Table),
        ]))
        .schema_delay(std::time::Duration::from_millis(200))
        .build();
    let sessions = SessionStore::new(DriverRegistry::builder().register(driver).build());
    let router = app(AppState {
        sessions: sessions.clone(),
        rooms: RoomRuntime::default(),
        shutdown: sift_server::shutdown::Shutdown::default(),
        auth: AuthState::default(),
        metadata: None,
    });

    let opened = router
        .clone()
        .oneshot(post("/v1/sessions", serde_json::json!({})))
        .await
        .unwrap();
    let session: sift_protocol::SessionInfo = json(opened.into_body()).await;
    let connection = router
        .clone()
        .oneshot(post(
            format!("/v1/sessions/{}/connections", session.id),
            serde_json::json!({
                "provider_id": "sift/postgres",
                "host": "mock.invalid",
                "port": 5432,
                "database": "app",
                "user": "fixture",
                "ssl_mode": "disable"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(connection.status(), StatusCode::OK);
    let connection: sift_protocol::ConnectionInfo = json(connection.into_body()).await;
    let uri = format!(
        "/v1/sessions/{}/connections/{}/catalog/graph",
        session.id, connection.id
    );

    let first = router
        .clone()
        .oneshot(post(
            &uri,
            serde_json::json!({"options": {"include_definitions": false}}),
        ))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let first: CatalogGraph = json(first.into_body()).await;
    assert_eq!(first.revision.0, 1);
    assert!(first.content_digest.starts_with("catfp:"));
    assert!(first.database_identity.starts_with("dbfp:"));
    assert!(first
        .data
        .nodes
        .iter()
        .any(|node| node.qualified_name == "app.private_customer_data.users"));

    let semantic_base = format!(
        "/v1/sessions/{}/connections/{}/semantic-documents",
        session.id, connection.id
    );
    let semantic = router
        .clone()
        .oneshot(post(
            &semantic_base,
            serde_json::json!({"text": "select * from users; select * from ghosts"}),
        ))
        .await
        .unwrap();
    let semantic: sift_protocol::SemanticDocumentState = json(semantic.into_body()).await;
    let diagnostics = router
        .clone()
        .oneshot(post(
            format!("{}/{}/diagnostics", semantic_base, semantic.document_id),
            serde_json::json!({
                "revision": semantic.revision,
                "catalog_revision": first.revision
            }),
        ))
        .await
        .unwrap();
    assert_eq!(diagnostics.status(), StatusCode::OK);
    let diagnostics: sift_protocol::DiagnosticsResponse = json(diagnostics.into_body()).await;
    assert_eq!(diagnostics.catalog_revision, Some(first.revision));
    assert!(diagnostics
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "unqualified_object"));
    assert!(diagnostics
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "undefined_object"));
    let fix = diagnostics
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "unqualified_object")
        .and_then(|diagnostic| diagnostic.quick_fix_ids.first())
        .unwrap();
    let quick_fix = router
        .clone()
        .oneshot(post(
            format!(
                "{}/{}/quick-fixes/{}",
                semantic_base, semantic.document_id, fix
            ),
            serde_json::json!({
                "revision": semantic.revision,
                "catalog_revision": first.revision
            }),
        ))
        .await
        .unwrap();
    assert_eq!(quick_fix.status(), StatusCode::OK);
    let quick_fix: sift_protocol::WorkspaceEdit = json(quick_fix.into_body()).await;
    assert_eq!(
        quick_fix.documents[0].edits[0].new_text,
        "\"private_customer_data\".\"users\""
    );
    let usages = router
        .clone()
        .oneshot(post(
            format!("{}/{}/usages", semantic_base, semantic.document_id),
            serde_json::json!({
                "revision": semantic.revision,
                "catalog_revision": first.revision,
                "target": {"kind": "catalog_object", "object_id": table_id(&first, "users")},
                "limit": 10
            }),
        ))
        .await
        .unwrap();
    assert_eq!(usages.status(), StatusCode::OK);
    let usages: sift_protocol::SqlUsagePage = json(usages.into_body()).await;
    assert_eq!(usages.usages.len(), 1);
    let refactor = router
        .clone()
        .oneshot(post(
            format!(
                "{}/{}/refactors/prepare",
                semantic_base, semantic.document_id
            ),
            serde_json::json!({
                "revision": semantic.revision,
                "catalog_revision": first.revision,
                "refactor": {"kind": "qualify_name", "position": 15}
            }),
        ))
        .await
        .unwrap();
    assert_eq!(refactor.status(), StatusCode::OK);
    let refactor: sift_protocol::WorkspaceEdit = json(refactor.into_body()).await;
    assert_eq!(
        refactor.documents[0].edits[0].new_text,
        "\"private_customer_data\".\"users\""
    );

    let second = router
        .clone()
        .oneshot(post(
            &uri,
            serde_json::json!({"options": {"include_definitions": false}}),
        ))
        .await
        .unwrap();
    let second: CatalogGraph = json(second.into_body()).await;
    assert_eq!(second.revision, first.revision);
    assert_eq!(second.content_digest, first.content_digest);

    let table = first
        .data
        .nodes
        .iter()
        .find(|node| node.kind == sift_protocol::CatalogNodeKind::Table)
        .unwrap();
    let diagram = router
        .clone()
        .oneshot(post(
            format!(
                "/v1/sessions/{}/connections/{}/catalog/diagram",
                session.id, connection.id
            ),
            serde_json::json!({
                "expected_revision": first.revision,
                "object_ids": [table.id],
                "include_columns": true,
                "neighborhood_depth": 1
            }),
        ))
        .await
        .unwrap();
    assert_eq!(diagram.status(), StatusCode::OK);
    let diagram: sift_protocol::CatalogDiagram = json(diagram.into_body()).await;
    assert_eq!(diagram.catalog_revision, first.revision);
    assert!(diagram.nodes.iter().any(|node| node.name == "users"));

    let refreshed = router
        .clone()
        .oneshot(post(&uri, serde_json::json!({"refresh": true})))
        .await
        .unwrap();
    assert_eq!(refreshed.status(), StatusCode::OK);
    let refreshed: CatalogGraph = json(refreshed.into_body()).await;
    assert_eq!(refreshed.revision.0, 2);
    assert!(refreshed
        .data
        .nodes
        .iter()
        .any(|node| node.name == "events"));

    let stale = router
        .clone()
        .oneshot(post(&uri, serde_json::json!({"refresh": true})))
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::OK);
    let stale: CatalogGraph = json(stale.into_body()).await;
    assert_eq!(stale.revision.0, 3);
    assert_eq!(
        stale.data.coverage.state,
        sift_protocol::CatalogCoverageState::Stale
    );
    assert!(stale.data.nodes.iter().any(|node| node.name == "events"));
    assert!(stale
        .data
        .coverage
        .failures
        .iter()
        .any(|failure| failure.code == "provider_unavailable_using_stale_catalog"));
    assert!(!serde_json::to_string(&stale)
        .unwrap()
        .contains("private provider failure detail"));

    let race_router = router.clone();
    let race_uri = uri.clone();
    let racing = tokio::spawn(async move {
        race_router
            .oneshot(post(&race_uri, serde_json::json!({"refresh": true})))
            .await
            .unwrap()
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let invalidating_router = router.clone();
    let invalidating_uri = uri.clone();
    let invalidating = tokio::spawn(async move {
        invalidating_router
            .oneshot(post(
                &invalidating_uri,
                serde_json::json!({"refresh": true}),
            ))
            .await
            .unwrap()
    });
    let raced = racing.await.unwrap();
    let invalidating = invalidating.await.unwrap();
    assert_eq!(raced.status(), StatusCode::OK);
    assert_eq!(invalidating.status(), StatusCode::OK);
    let raced: CatalogGraph = json(raced.into_body()).await;
    assert_eq!(
        raced.data.coverage.state,
        sift_protocol::CatalogCoverageState::Stale
    );
    assert!(!raced
        .data
        .nodes
        .iter()
        .any(|node| node.name == "raced_publication"));

    let replay = serde_json::to_string(&sessions.list_operations()).unwrap();
    assert!(!replay.contains("private_customer_data"));
    assert!(replay.contains("catalog_graph"));
    assert!(replay.contains("catalog_diagram"));
    assert!(sessions.list_operations().iter().any(|entry| matches!(
        entry.operation,
        sift_protocol::Operation::ReadCatalogGraph { refresh: true, .. }
    )));
}

fn table_id(graph: &CatalogGraph, name: &str) -> sift_protocol::CatalogObjectId {
    graph
        .data
        .nodes
        .iter()
        .find(|node| node.kind == sift_protocol::CatalogNodeKind::Table && node.name == name)
        .unwrap()
        .id
        .clone()
}

#[tokio::test]
async fn durable_catalog_snapshots_are_managed_tenant_scoped_and_revision_guarded() {
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .schema_ok(graph_snapshot())
        .schema_ok(graph_snapshot())
        .execute_ok(vec![sift_protocol::Page::Done {
            affected_rows: None,
            warnings: Vec::new(),
        }])
        .build();
    let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
    metadata.bootstrap_local("local user").unwrap();
    let profile = metadata
        .upsert_connection_profile(
            TenantId(1),
            PrincipalId(1),
            NewConnectionProfile {
                name: "catalog fixture".into(),
                provider_id: Engine::Postgres.provider_id(),
                configuration: serde_json::json!({
                    "host": "mock.invalid",
                    "database": "app",
                    "user": "fixture"
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
        metadata: Some(metadata.clone()),
    });
    let response = router
        .clone()
        .oneshot(post("/v1/sessions", serde_json::json!({})))
        .await
        .unwrap();
    let session: sift_protocol::SessionInfo = json(response.into_body()).await;
    let response = router
        .clone()
        .oneshot(post(
            format!("/v1/sessions/{}/connections/from-profile", session.id),
            serde_json::json!({"tenant_id": 1, "profile_id": profile.id.0}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let connection: sift_protocol::ConnectionInfo = json(response.into_body()).await;
    let response = router
        .clone()
        .oneshot(post(
            format!(
                "/v1/sessions/{}/connections/{}/catalog/graph",
                session.id, connection.id
            ),
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let graph: CatalogGraph = json(response.into_body()).await;

    let response = router
        .clone()
        .oneshot(post(
            format!(
                "/v1/sessions/{}/connections/{}/catalog/diagram/mutations/preview",
                session.id, connection.id
            ),
            serde_json::json!({
                "expected_catalog_revision": graph.revision,
                "mutation": {
                    "kind": "rename_object",
                    "object_id": table_id(&graph, "users"),
                    "new_name": "application users"
                }
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let diagram_plan: sift_protocol::MigrationPlan = json(response.into_body()).await;
    assert_eq!(diagram_plan.groups[0].statements.len(), 1);
    assert_eq!(
        diagram_plan.groups[0].statements[0].sql,
        "ALTER TABLE \"private_customer_data\".\"users\" RENAME TO \"application users\";"
    );

    let response = router
        .clone()
        .oneshot(post(
            format!(
                "/v1/sessions/{}/connections/{}/catalog/snapshots",
                session.id, connection.id
            ),
            serde_json::json!({
                "expected_catalog_revision": graph.revision,
                "description": "release baseline"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let snapshot: sift_protocol::CatalogSnapshot = json(response.into_body()).await;
    assert_eq!(snapshot.tenant_id, 1);
    assert_eq!(snapshot.connection_profile_id, Some(profile.id.0));
    assert_eq!(snapshot.graph.content_digest, graph.content_digest);

    let response = router
        .clone()
        .oneshot(get("/v1/metadata/tenants/1/catalog-snapshots"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let summaries: Vec<sift_protocol::CatalogSnapshotSummary> = json(response.into_body()).await;
    assert_eq!(summaries.len(), 1);

    let response = router
        .clone()
        .oneshot(post(
            format!(
                "/v1/sessions/{}/connections/{}/catalog/diffs",
                session.id, connection.id
            ),
            serde_json::json!({
                "from": {"kind": "snapshot", "snapshot_id": snapshot.id},
                "to": {"kind": "live", "expected_revision": graph.revision}
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let diff: sift_protocol::SchemaDiff = json(response.into_body()).await;
    assert!(diff.changes.is_empty());
    assert!(diff.digest.starts_with("difffp:"));

    let mut desired_graph = graph.clone();
    desired_graph.revision = sift_protocol::CatalogRevision(2);
    desired_graph.content_digest = "catfp:desired".into();
    desired_graph.data = sift_core::catalog::graph_from_trees(
        &[CatalogTree {
            name: "app".into(),
            schemas: vec![SchemaTree {
                name: "private_customer_data".into(),
                objects: vec![
                    ObjectInfo::new("users", ObjectKind::Table),
                    ObjectInfo::new("events", ObjectKind::Table),
                ],
            }],
        }],
        CatalogCoverage::complete(),
        "mock:db",
    );
    let desired = metadata
        .create_catalog_snapshot(
            TenantId(1),
            Some(profile.id),
            PrincipalId(1),
            Some("desired".into()),
            &desired_graph,
        )
        .unwrap();
    let diff_request = serde_json::json!({
        "from": {"kind": "live", "expected_revision": graph.revision},
        "to": {"kind": "snapshot", "snapshot_id": desired.id}
    });
    let response = router
        .clone()
        .oneshot(post(
            format!(
                "/v1/sessions/{}/connections/{}/catalog/diffs",
                session.id, connection.id
            ),
            &diff_request,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let desired_diff: sift_protocol::SchemaDiff = json(response.into_body()).await;
    assert!(desired_diff.changes.iter().any(|change| {
        change.kind == sift_protocol::SchemaChangeKind::Create
            && change
                .object_after
                .as_ref()
                .is_some_and(|node| node.name == "events")
    }));
    let response = router
        .clone()
        .oneshot(post(
            format!(
                "/v1/sessions/{}/connections/{}/catalog/migrations/preview",
                session.id, connection.id
            ),
            serde_json::json!({
                "diff": diff_request,
                "expected_diff_digest": desired_diff.digest,
                "expected_live_revision": graph.revision
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let plan: sift_protocol::MigrationPlan = json(response.into_body()).await;
    assert_eq!(plan.groups[0].statements.len(), 1);
    let response = router
        .clone()
        .oneshot(post(
            format!(
                "/v1/sessions/{}/connections/{}/catalog/migrations/apply",
                session.id, connection.id
            ),
            serde_json::json!({
                "plan_id": plan.id,
                "plan_digest": plan.digest
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let run: sift_protocol::MigrationRun = json(response.into_body()).await;
    assert_eq!(run.state, sift_protocol::MigrationRunState::Applied);
    assert_eq!(run.outcomes.len(), 1);
    let response = router
        .clone()
        .oneshot(get(format!(
            "/v1/metadata/tenants/1/migration-runs/{}",
            run.id
        )))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let durable_run: sift_protocol::MigrationRun = json(response.into_body()).await;
    assert_eq!(durable_run.state, sift_protocol::MigrationRunState::Applied);

    let response = router
        .clone()
        .oneshot(post(
            format!(
                "/v1/sessions/{}/connections/{}/catalog/migrations/preview",
                session.id, connection.id
            ),
            serde_json::json!({
                "diff": diff_request,
                "expected_diff_digest": desired_diff.digest,
                "expected_live_revision": graph.revision
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let stale_policy_plan: sift_protocol::MigrationPlan = json(response.into_body()).await;
    let response = router
        .clone()
        .oneshot(
            Request::put(format!("/v1/metadata/connections/{}/policy", profile.id.0))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "expected_revision": 0,
                        "minimum_tenant_role": "member",
                        "read_only": false,
                        "blocked_ops": []
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = router
        .clone()
        .oneshot(post(
            format!(
                "/v1/sessions/{}/connections/{}/catalog/migrations/apply",
                session.id, connection.id
            ),
            serde_json::json!({
                "plan_id": stale_policy_plan.id,
                "plan_digest": stale_policy_plan.digest
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let mut empty_graph = graph.clone();
    empty_graph.content_digest = "catfp:empty".into();
    empty_graph.data = sift_core::catalog::graph_from_trees(
        &[CatalogTree {
            name: "app".into(),
            schemas: vec![SchemaTree {
                name: "private_customer_data".into(),
                objects: Vec::new(),
            }],
        }],
        CatalogCoverage::complete(),
        "mock:db",
    );
    let empty = metadata
        .create_catalog_snapshot(
            TenantId(1),
            Some(profile.id),
            PrincipalId(1),
            Some("empty target".into()),
            &empty_graph,
        )
        .unwrap();
    let destructive_diff_request = serde_json::json!({
        "from": {"kind": "live", "expected_revision": graph.revision},
        "to": {"kind": "snapshot", "snapshot_id": empty.id}
    });
    let response = router
        .clone()
        .oneshot(post(
            format!(
                "/v1/sessions/{}/connections/{}/catalog/diffs",
                session.id, connection.id
            ),
            &destructive_diff_request,
        ))
        .await
        .unwrap();
    let destructive_diff: sift_protocol::SchemaDiff = json(response.into_body()).await;
    assert!(destructive_diff
        .changes
        .iter()
        .any(|change| change.risk == sift_protocol::SchemaChangeRisk::DataLoss));
    let response = router
        .clone()
        .oneshot(post(
            format!(
                "/v1/sessions/{}/connections/{}/catalog/migrations/preview",
                session.id, connection.id
            ),
            serde_json::json!({
                "diff": destructive_diff_request,
                "expected_diff_digest": destructive_diff.digest,
                "expected_live_revision": graph.revision
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let destructive_plan: sift_protocol::MigrationPlan = json(response.into_body()).await;
    assert_eq!(
        destructive_plan.required_acknowledgements,
        vec![sift_protocol::SchemaChangeRisk::DataLoss]
    );
    let response = router
        .clone()
        .oneshot(post(
            format!(
                "/v1/sessions/{}/connections/{}/catalog/migrations/apply",
                session.id, connection.id
            ),
            serde_json::json!({
                "plan_id": destructive_plan.id,
                "plan_digest": destructive_plan.digest
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let response = router
        .clone()
        .oneshot(
            Request::delete(format!(
                "/v1/metadata/tenants/1/catalog-snapshots/{}?expected_revision=1",
                snapshot.id
            ))
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let response = router
        .oneshot(
            Request::delete(format!(
                "/v1/metadata/tenants/1/catalog-snapshots/{}?expected_revision=0",
                snapshot.id
            ))
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(sessions.list_operations().iter().any(|entry| matches!(
        entry.operation,
        sift_protocol::Operation::CreateCatalogSnapshot { .. }
    )));
}

#[test]
fn rejects_dangling_provider_graphs() {
    let graph = CatalogGraphData {
        coverage: CatalogCoverage::complete(),
        nodes: Vec::new(),
        edges: vec![sift_protocol::CatalogEdge {
            from: sift_protocol::CatalogObjectId("missing".into()),
            to: None,
            kind: sift_protocol::CatalogEdgeKind::DependsOn,
            certainty: sift_protocol::CatalogEdgeCertainty::Unresolved,
            referenced_path: Some("public.users".into()),
            column_pairs: Vec::new(),
        }],
    };
    assert!(matches!(
        sift_core::catalog::validate_graph(&graph, 10, 10),
        Err(sift_core::catalog::GraphValidationError::DanglingReference)
    ));
}
