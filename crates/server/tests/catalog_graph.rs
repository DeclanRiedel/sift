use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use sift_driver_api::mock::MockDriver;
use sift_metadata::{
    CredentialMode, MemorySecretStore, MetadataStore, NewConnectionProfile, PrincipalId, TenantId,
};
use sift_protocol::{
    CatalogCoverage, CatalogGraph, CatalogGraphData, CatalogGraphOptions, CatalogTree,
    ColumnMetadata, Engine, Nullability, ObjectInfo, ObjectKind, PrimitiveType, SchemaDepth,
    SchemaScope, SchemaSnapshot, SchemaTree, TypeRef,
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
    let mut users = ObjectInfo::new("users", ObjectKind::Table);
    users.comment = Some("Application users".into());
    users.columns = vec![ColumnMetadata {
        name: "id".into(),
        type_ref: TypeRef::Primitive(PrimitiveType::Int64),
        nullable: Nullability::NotNullable,
        auto_increment: false,
        primary_key: true,
        facets: Default::default(),
    }];
    graph_snapshot_with_objects(vec![users])
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
            serde_json::json!({"text": "select users.id from users; select * from ghosts"}),
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
    let hover = router
        .clone()
        .oneshot(post(
            format!("{}/{}/hover", semantic_base, semantic.document_id),
            serde_json::json!({
                "revision": semantic.revision,
                "position": 13,
                "catalog_revision": first.revision
            }),
        ))
        .await
        .unwrap();
    assert_eq!(hover.status(), StatusCode::OK);
    let hover: sift_protocol::SemanticHoverResponse = json(hover.into_body()).await;
    assert_eq!(hover.kind, sift_protocol::SemanticHoverKind::Column);
    assert_eq!(
        hover.type_ref,
        Some(TypeRef::Primitive(PrimitiveType::Int64))
    );
    assert_eq!(hover.nullability, Some(Nullability::NotNullable));
    assert_eq!(hover.comment.as_deref(), Some("Application users"));
    let star_document = router
        .clone()
        .oneshot(post(
            &semantic_base,
            serde_json::json!({"text": "select u.* from users u"}),
        ))
        .await
        .unwrap();
    let star_document: sift_protocol::SemanticDocumentState = json(star_document.into_body()).await;
    let star = router
        .clone()
        .oneshot(post(
            format!(
                "{}/{}/star-expansions/prepare",
                semantic_base, star_document.document_id
            ),
            serde_json::json!({
                "revision": star_document.revision,
                "position": 10,
                "catalog_revision": first.revision
            }),
        ))
        .await
        .unwrap();
    assert_eq!(star.status(), StatusCode::OK);
    let star: sift_protocol::StarExpansionPreview = json(star.into_body()).await;
    assert_eq!(star.range, sift_protocol::TextRange { start: 7, end: 10 });
    assert_eq!(star.replacement, "u.id");
    assert!(star.exact);
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
    assert_eq!(usages.usages.len(), 2);
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
                "refactor": {"kind": "qualify_name", "position": 22}
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
    let stale_hover = router
        .clone()
        .oneshot(post(
            format!("{}/{}/hover", semantic_base, semantic.document_id),
            serde_json::json!({
                "revision": semantic.revision,
                "position": 13,
                "catalog_revision": first.revision
            }),
        ))
        .await
        .unwrap();
    assert_eq!(stale_hover.status(), StatusCode::BAD_REQUEST);
    let stale_star = router
        .clone()
        .oneshot(post(
            format!(
                "{}/{}/star-expansions/prepare",
                semantic_base, star_document.document_id
            ),
            serde_json::json!({
                "revision": star_document.revision,
                "position": 10,
                "catalog_revision": first.revision
            }),
        ))
        .await
        .unwrap();
    assert_eq!(stale_star.status(), StatusCode::BAD_REQUEST);

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
    assert!(sessions.list_operations().iter().any(|entry| matches!(
        entry.operation,
        sift_protocol::Operation::HoverSemanticDocument { .. }
    )));
    assert!(sessions.list_operations().iter().any(|entry| matches!(
        entry.operation,
        sift_protocol::Operation::PrepareStarExpansion { .. }
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
    sessions.set_audit_store(metadata.clone());
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
    let mut desired_users = ObjectInfo::new("users", ObjectKind::Table);
    desired_users.comment = Some("Application users".into());
    desired_users.columns = vec![ColumnMetadata {
        name: "id".into(),
        type_ref: TypeRef::Primitive(PrimitiveType::Int64),
        nullable: Nullability::NotNullable,
        auto_increment: false,
        primary_key: true,
        facets: Default::default(),
    }];
    let mut desired_events = ObjectInfo::new("events", ObjectKind::Table);
    desired_events.columns = vec![ColumnMetadata {
        name: "id".into(),
        type_ref: TypeRef::Primitive(PrimitiveType::Int64),
        nullable: Nullability::NotNullable,
        auto_increment: false,
        primary_key: true,
        facets: Default::default(),
    }];
    desired_graph.data = sift_core::catalog::graph_from_trees(
        &[CatalogTree {
            name: "app".into(),
            schemas: vec![SchemaTree {
                name: "private_customer_data".into(),
                objects: vec![desired_users, desired_events],
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
                "plan_digest": plan.digest,
                "acknowledgements": plan.required_acknowledgements
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

    let native_path = format!(
        "/v1/metadata/tenants/1/migration-runs/{}/native-outcomes",
        run.id
    );
    let response = router
        .clone()
        .oneshot(get(native_path.clone()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let native: sift_protocol::MigrationNativeOutcomesPage = json(response.into_body()).await;
    assert_eq!(native.run_id, run.id);
    assert_eq!(native.plan_id, run.plan_id);
    assert_eq!(native.plan_digest, run.plan_digest);
    assert!(native.journal_present && native.closed);
    assert!(native
        .source
        .as_ref()
        .is_some_and(|source| source.session == session.id
            && source.connection == connection.id
            && !source.generation.is_nil()));
    assert_eq!(native.observations.len(), 1);
    let witness = &native.observations[0];
    assert_eq!(
        witness.group.outcome,
        Some(sift_protocol::ChangeLedgerOutcome::Committed)
    );
    assert!(witness.group.transaction_id.is_some());
    assert_eq!(
        witness.statement.as_ref().unwrap().native_admitted,
        Some(true)
    );
    assert_eq!(
        witness.statement.as_ref().unwrap().native_completed,
        Some(true)
    );
    let changes = metadata
        .change_ledger(&sift_protocol::ChangeLedgerFilter::default())
        .unwrap();
    let changes = changes
        .entries
        .iter()
        .filter(|entry| entry.source_workflow == "schema_migration")
        .collect::<Vec<_>>();
    assert_eq!(changes.len(), 1);
    assert_eq!(
        changes[0].outcome,
        sift_protocol::ChangeLedgerOutcome::Committed
    );
    assert_eq!(
        changes[0].transaction_id,
        witness.group.transaction_id.map(|tx| tx.0.to_string())
    );
    assert_ne!(
        changes[0].transaction_id.as_deref(),
        Some(run.id.to_string().as_str())
    );
    for (suffix, status) in [
        ("?limit=0", StatusCode::BAD_REQUEST),
        (
            "?expected_revision=18446744073709551615",
            StatusCode::CONFLICT,
        ),
    ] {
        let response = router
            .clone()
            .oneshot(get(format!("{native_path}{suffix}")))
            .await
            .unwrap();
        assert_eq!(response.status(), status);
    }
    let response = router
        .clone()
        .oneshot(get(format!(
            "/v1/metadata/tenants/2/migration-runs/{}/native-outcomes",
            run.id
        )))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let native_audits = sessions
        .list_operations()
        .into_iter()
        .filter(|entry| {
            matches!(
                entry.operation,
                sift_protocol::Operation::GetMigrationNativeOutcomes { .. }
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(native_audits.len(), 4);
    let durable_native_audits = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let rows = metadata
                .list_operation_audit(100)
                .unwrap()
                .into_iter()
                .filter(|row| row.target == "migration_native_journal")
                .collect::<Vec<_>>();
            if rows.len() == 4 {
                break rows;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    assert!(durable_native_audits
        .iter()
        .all(|entry| entry.actor_principal_id.map(|actor| actor.0) == Some(1)));
    assert_eq!(
        native_audits
            .iter()
            .filter(|entry| entry.status == sift_protocol::OperationStatus::Succeeded)
            .count(),
        1
    );

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

#[tokio::test]
async fn ai_migration_preview_requires_risk_ack_and_cannot_bypass_human_apply() {
    use sift_protocol::*;
    for mode in [
        "caller_loss",
        "timeout",
        "late_commit",
        "native_error",
        "delivery_revoked",
        "begin_revoke",
        "journal_closed_after_begin",
        "commit_witness_failure",
        "autocommit_timeout",
    ] {
        let journal_root = tempfile::tempdir().unwrap();
        let journal_path = journal_root.path().join("metadata.sqlite");
        let metadata = if mode == "commit_witness_failure" {
            let store =
                MetadataStore::open(&journal_path, Arc::new(MemorySecretStore::new())).unwrap();
            store.apply_migrations(false).unwrap();
            store
        } else {
            MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap()
        };
        metadata.bootstrap_local("AI migration").unwrap();
        let profile=metadata.upsert_connection_profile(TenantId(1),PrincipalId(1),NewConnectionProfile {
        name:"AI migration fixture".into(),provider_id:Engine::Postgres.provider_id(),semantic_engine:Some(Engine::Postgres),configuration:serde_json::json!({"host":"mock.invalid","port":5432,"database":"app","user":"mock","ssl_mode":"disable"}),credentials:None,credential_mode:CredentialMode::Shared,tags:vec![],
    }).await.unwrap();
        let owner = PrincipalId(1);
        let room = metadata
            .create_room(
                TenantId(1),
                owner,
                sift_metadata::NewRoom {
                    name: "Published AI migration".into(),
                    kind: sift_metadata::RoomKind::Shared,
                },
            )
            .unwrap();
        metadata
            .bind_room_connection(
                room.id,
                owner,
                profile.id,
                sift_metadata::NewOperationAudit {
                    actor_principal_id: Some(owner),
                    action: "bind".into(),
                    target: "room".into(),
                    target_id: Some(room.id.0),
                    status: "succeeded".into(),
                    result_code: None,
                    row_count: None,
                    error_message: None,
                    correlation_id: None,
                },
            )
            .unwrap();
        let membership_audit = |action: &str| sift_metadata::NewOperationAudit {
            actor_principal_id: None,
            action: action.into(),
            target: "room".into(),
            target_id: Some(room.id.0),
            status: "succeeded".into(),
            result_code: None,
            row_count: None,
            error_message: None,
            correlation_id: None,
        };
        let delivery_peer = if mode == "delivery_revoked" {
            let peer = metadata
                .create_principal("ai-delivery-owner", "Delivery owner", None)
                .unwrap()
                .id;
            metadata
                .upsert_tenant_membership(TenantId(1), peer, sift_metadata::MembershipRole::Member)
                .unwrap();
            metadata
                .add_room_member_authorized(
                    room.id,
                    owner,
                    peer,
                    sift_metadata::RoomRole::Owner,
                    membership_audit("add_member"),
                )
                .unwrap();
            Some(peer)
        } else {
            None
        };
        let chat = metadata
            .create_ai_chat(
                TenantId(1),
                Some(room.id),
                owner,
                AiVisibility::RoomPublic,
                "Review shared schema".into(),
            )
            .await
            .unwrap();
        let dispatch = Arc::new(tokio::sync::Notify::new());
        let commit = Arc::new(tokio::sync::Notify::new());
        let begin = Arc::new(tokio::sync::Notify::new());
        let (native, rows) = tokio::sync::mpsc::channel(2);
        let mut builder = MockDriver::builder()
            .engine(Engine::Postgres)
            .execute_wait(dispatch.clone())
            .execute_rows(rows);
        if matches!(
            mode,
            "delivery_revoked" | "late_commit" | "commit_witness_failure"
        ) {
            builder = builder.commit_wait(commit.clone());
        }
        if matches!(mode, "begin_revoke" | "journal_closed_after_begin") {
            builder = builder.begin_wait(begin.clone());
        }
        for _ in 0..10 {
            builder = builder.schema_ok(graph_snapshot());
        }
        let mock = Arc::new(builder.build());
        let registry = DriverRegistry::new();
        registry
            .providers()
            .replace(
                [Arc::new(sift_server::registry::BuiltinProviderAdapter::new(
                    mock.clone(),
                ))
                    as Arc<dyn sift_server::registry::DatabaseProvider>],
            )
            .unwrap();
        let sessions = SessionStore::new(registry);
        sessions.set_audit_store(metadata.clone());
        if matches!(mode, "timeout" | "late_commit" | "autocommit_timeout") {
            sessions.set_request_timeout(std::time::Duration::from_millis(100));
        }
        let shutdown = sift_server::shutdown::Shutdown::default();
        let mut auth = AuthState::default();
        auth.ai.enabled = true;
        if matches!(mode, "timeout" | "late_commit" | "autocommit_timeout") {
            auth.ai.max_run_secs = 1;
        }
        let router = app(AppState {
            sessions: sessions.clone(),
            rooms: RoomRuntime::default(),
            shutdown: shutdown.clone(),
            auth,
            metadata: Some(metadata.clone()),
        });
        let direct_router = router.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let client = sift_client_sdk::Client::new(format!("http://{addr}"));
        let session = client.open_session(None).await.unwrap().id;
        let connection = client
            .open_connection_from_profile(
                session,
                sift_api_types::OpenConnectionFromProfileRequest {
                    tenant_id: 1,
                    profile_id: profile.id.0,
                },
            )
            .await
            .unwrap()
            .id;
        let publication = client.preview_ai_room_publication(room.id.0).await.unwrap();
        client
            .create_ai_room_publication(
                room.id.0,
                &CreateAiRoomPublicationRequest {
                    client_request_id: uuid::Uuid::new_v4(),
                    expected_profile_id: profile.id.0,
                    expected_scope_digest: publication.scope_digest,
                    allow_rows: false,
                },
            )
            .await
            .unwrap();
        let lease = client
            .start_ai_turn(
                chat.id,
                &StartAiTurnRequest {
                    attachment_previews: Vec::new(),
                    client_request_id: uuid::Uuid::new_v4(),
                    desktop_id: uuid::Uuid::new_v4(),
                    prompt: "Drop the obsolete users table".into(),
                    provider: AiProvider::Codex,
                    model: None,
                    mode: AiMode::Propose,
                    context: AiTurnContext {
                        reasoning_effort: None,
                        external_sources: Vec::new(),
                        inclusion: Default::default(),
                        workspace: None,
                        attachments: Vec::new(),
                        target: ToolContext {
                            tenant_id: Some(1),
                            room_id: Some(room.id.0),
                            profile_id: Some(profile.id.0),
                            connection_id: Some(format!("{}:{}", session.0, connection.0)),
                            document_id: None,
                        },
                        editor_item_id: None,
                        database: None,
                        dialect: Some("postgres".into()),
                        environment_label: None,
                        sql: None,
                        current_error: None,
                        staged_change_count: 0,
                        publication_id: None,
                    },
                },
            )
            .await
            .unwrap();
        let result = client
            .invoke_ai_tool(
                lease.run.id,
                &InvokeAiToolRequest {
                    parameters: None,
                    call_id: uuid::Uuid::new_v4(),
                    lease_token: lease.lease_token,
                    tool: AiToolKind::Catalog,
                    sql: None,
                },
            )
            .await
            .unwrap();
        let graph: CatalogGraph = serde_json::from_value(result.result).unwrap();
        let mut desired = graph.clone();
        desired.data = graph_snapshot_with_objects(vec![]).graph.unwrap();
        let proposal = client
            .stage_ai_database_proposal(
                lease.run.id,
                &StageAiDatabaseProposalRequest {
                    client_request_id: uuid::Uuid::new_v4(),
                    lease_token: lease.lease_token,
                    draft: AiDatabaseDraft::MigrationDraft {
                        desired_catalog: desired,
                        expected_catalog_revision: graph.revision,
                        options: MigrationOptions {
                            prefer_transactional: mode != "autocommit_timeout",
                            ..Default::default()
                        },
                    },
                },
            )
            .await
            .unwrap();
        let review = client
            .review_ai_database_proposal(
                proposal.proposal.id,
                &ReviewAiDatabaseProposalRequest {
                    client_request_id: uuid::Uuid::new_v4(),
                    session,
                    connection,
                },
            )
            .await
            .unwrap();
        let AiDatabasePreview::MigrationDraft { plan } = &review.preview else {
            panic!("expected migration")
        };
        assert_eq!(
            plan.groups
                .iter()
                .map(|group| group.statements.len())
                .sum::<usize>(),
            1
        );
        assert!(!plan.required_acknowledgements.is_empty());
        let direct = client
            .apply_migration(
                session,
                connection,
                ApplyMigrationRequest {
                    plan_id: plan.id,
                    plan_digest: plan.digest.clone(),
                    acknowledgements: plan.required_acknowledgements.clone(),
                    source: None,
                },
            )
            .await;
        assert!(
            direct.is_err(),
            "AI plans require their proposal review/apply boundary"
        );
        let mut apply = ApplyAiDatabaseProposalRequest {
            client_request_id: uuid::Uuid::new_v4(),
            review_id: review.id,
            review_digest: review.review_digest.clone(),
            production_confirmation: None,
            acknowledgements: vec![],
        };
        assert!(client
            .apply_ai_database_proposal(proposal.proposal.id, &apply)
            .await
            .is_err());
        assert!(client.ai_database_proposals(chat.id).await.unwrap()[0]
            .apply_state
            .is_none());
        apply.acknowledgements = plan.required_acknowledgements.clone();
        let correlation = format!("ai-owned-apply-{mode}");
        let mut request = post(
            format!("/v1/ai/database-proposals/{}/apply", proposal.proposal.id),
            &apply,
        );
        request
            .headers_mut()
            .insert("x-correlation-id", correlation.parse().unwrap());
        let mut caller = Some(tokio::spawn(direct_router.oneshot(request)));
        if matches!(mode, "begin_revoke" | "journal_closed_after_begin") {
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while !mock.invocations().contains(&"begin") {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let before = metadata
                .migration_native_outcomes(
                    TenantId(1),
                    plan.run_id,
                    &MigrationNativePageRequest::default(),
                )
                .unwrap();
            assert!(!before.closed);
            assert_eq!(
                before.observations[0].group.phase,
                MigrationNativePhase::Beginning
            );
            assert!(before.observations[0].group.transaction_id.is_none());
            if mode == "begin_revoke" {
                client
                    .update_connection_policy(
                        sift_api_types::ConnectionProfileId(profile.id.0),
                        UpdateConnectionPolicyRequest {
                            expected_revision: Some(0),
                            minimum_tenant_role: TenantRole::Member,
                            read_only: false,
                            allowed_ops: None,
                            blocked_ops: vec![OperationKind::ApplyMigration],
                            allowed_schemas: None,
                        },
                    )
                    .await
                    .unwrap();
            } else {
                metadata
                    .close_migration_native_journal(TenantId(1), owner, plan.run_id)
                    .unwrap();
            }
            begin.notify_one();
            let response = caller.take().unwrap().await.unwrap().unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let receipt: AiDatabaseApplyReceipt = json(response.into_body()).await;
            let expected = if mode == "begin_revoke" {
                AiDatabaseApplyState::Failed
            } else {
                AiDatabaseApplyState::OutcomeUnknown
            };
            assert_eq!(receipt.state, expected, "{receipt:?}");
            let observed = metadata
                .migration_native_outcomes(
                    TenantId(1),
                    plan.run_id,
                    &MigrationNativePageRequest::default(),
                )
                .unwrap();
            assert!(observed.closed && observed.journal_present);
            assert!(!mock.invocations().contains(&"execute"));
            assert_eq!(
                mock.invocations()
                    .iter()
                    .filter(|&&call| call == "rollback")
                    .count(),
                1,
                "mandatory cleanup survives publication or journal refusal"
            );
            let group = &observed.observations[0].group;
            if mode == "begin_revoke" {
                assert!(group.transaction_id.is_some() && group.rollback_intent);
                assert_eq!(group.outcome, Some(ChangeLedgerOutcome::RolledBack));
            } else {
                assert_eq!(group.outcome, Some(ChangeLedgerOutcome::OutcomeUnknown));
                assert!(group.transaction_id.is_none());
            }
            assert!(
                metadata
                    .change_ledger(&ChangeLedgerFilter::default())
                    .unwrap()
                    .entries
                    .iter()
                    .all(|entry| entry.source_workflow != "ai_migration_proposal"),
                "a durable intent cannot invent a native statement attempt"
            );
            assert_eq!(
                client
                    .apply_ai_database_proposal(proposal.proposal.id, &apply)
                    .await
                    .unwrap()
                    .state,
                expected
            );
            server.abort();
            continue;
        }
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !mock.invocations().contains(&"execute") {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(shutdown.in_flight(), 1);
        if matches!(
            mode,
            "caller_loss" | "native_error" | "commit_witness_failure"
        ) {
            let caller = caller.take().unwrap();
            caller.abort();
            assert!(caller.await.unwrap_err().is_cancelled());
        }
        if matches!(mode, "timeout" | "autocommit_timeout") {
            let response = caller.take().unwrap().await.unwrap().unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let pending: AiDatabaseApplyReceipt = json(response.into_body()).await;
            assert_eq!(pending.state, AiDatabaseApplyState::Applying);
            let observed = metadata
                .migration_native_outcomes(
                    TenantId(1),
                    plan.run_id,
                    &MigrationNativePageRequest::default(),
                )
                .unwrap();
            assert!(!observed.closed);
            assert!(observed
                .observations
                .iter()
                .all(|item| item.group.outcome.is_none()));
            assert_eq!(
                client
                    .apply_ai_database_proposal(proposal.proposal.id, &apply)
                    .await
                    .unwrap()
                    .state,
                AiDatabaseApplyState::Applying,
                "exact replay observes the pending claim without another native write"
            );
            assert_eq!(
                mock.invocations()
                    .iter()
                    .filter(|&&call| call == "execute")
                    .count(),
                1
            );
            assert!(!metadata
                .list_operation_audit(100)
                .unwrap()
                .iter()
                .any(|row| row.action == "apply_database_proposal"
                    && row.correlation_id.as_deref() == Some(correlation.as_str())));
            assert!(
                !mock.invocations().contains(&"rollback"),
                "cleanup must wait for native EOF"
            );
            assert_eq!(shutdown.in_flight(), 1);
        }
        dispatch.notify_one();
        native
            .send(if mode == "native_error" {
                Page::Error {
                    error: DriverError::new(
                        Code::Other {
                            message: "PRIVATE-AI-NATIVE-ERROR".into(),
                        },
                        "PRIVATE-AI-NATIVE-ERROR",
                    ),
                }
            } else {
                Page::Done {
                    affected_rows: None,
                    warnings: vec![],
                }
            })
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert_eq!(shutdown.in_flight(), 1, "Done/error is not raw native EOF");
        let revoker = delivery_peer.map(|peer| {
            let commit = commit.clone();
            let metadata = metadata.clone();
            let mock = mock.clone();
            let room_id = room.id;
            let audit = membership_audit("remove_member");
            tokio::spawn(async move {
                tokio::time::timeout(std::time::Duration::from_secs(2), async {
                    while !mock.invocations().contains(&"commit") {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
                // Native commit is already admitted and awaits acknowledgement.
                // Remove only room access; the user retains tenant membership.
                metadata
                    .remove_room_member_authorized(room_id, peer, owner, audit)
                    .unwrap();
                commit.notify_one();
            })
        });
        drop(native);
        if mode == "commit_witness_failure" {
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while !mock.invocations().contains(&"commit") {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let before = metadata
                .migration_native_outcomes(
                    TenantId(1),
                    plan.run_id,
                    &MigrationNativePageRequest::default(),
                )
                .unwrap();
            assert_eq!(
                before.observations[0].group.phase,
                MigrationNativePhase::Committing
            );
            assert!(
                before.observations[0].group.commit_intent
                    && before.observations[0].group.outcome.is_none()
            );
            rusqlite::Connection::open(&journal_path)
                .unwrap()
                .execute_batch(
                    "CREATE TRIGGER refuse_commit_witness BEFORE UPDATE ON migration_native_group
                 WHEN NEW.phase='settled' AND NEW.outcome='committed'
                 BEGIN SELECT RAISE(ABORT, 'PRIVATE-JOURNAL-WITNESS-ERROR'); END;",
                )
                .unwrap();
            commit.notify_one();
        }
        if mode == "late_commit" {
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while !mock.invocations().contains(&"commit") {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let response = caller.take().unwrap().await.unwrap().unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let pending: AiDatabaseApplyReceipt = json(response.into_body()).await;
            assert_eq!(pending.state, AiDatabaseApplyState::Applying);
            let observed = metadata
                .migration_native_outcomes(
                    TenantId(1),
                    plan.run_id,
                    &MigrationNativePageRequest::default(),
                )
                .unwrap();
            assert!(!observed.closed);
            assert!(observed
                .observations
                .iter()
                .all(|item| item.group.outcome.is_none()));
            assert_eq!(
                client
                    .apply_ai_database_proposal(proposal.proposal.id, &apply)
                    .await
                    .unwrap()
                    .state,
                AiDatabaseApplyState::Applying,
                "exact replay observes the pending claim without another native write"
            );
            assert_eq!(
                mock.invocations()
                    .iter()
                    .filter(|&&call| call == "execute")
                    .count(),
                1
            );
            assert_eq!(shutdown.in_flight(), 1);
            assert!(!metadata
                .list_operation_audit(100)
                .unwrap()
                .iter()
                .any(|row| row.action == "apply_database_proposal"
                    && row.correlation_id.as_deref() == Some(correlation.as_str())));
            commit.notify_one();
        }
        if let Some(revoker) = revoker {
            revoker.await.unwrap();
        }
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let audited = metadata
                    .list_operation_audit(100)
                    .unwrap()
                    .iter()
                    .any(|row| {
                        row.action == "apply_database_proposal"
                            && row.correlation_id.as_deref() == Some(correlation.as_str())
                    });
                if shutdown.in_flight() == 0 && audited {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        let terminal = metadata
            .list_operation_audit(100)
            .unwrap()
            .into_iter()
            .filter(|row| {
                row.action == "apply_database_proposal"
                    && row.correlation_id.as_deref() == Some(correlation.as_str())
            })
            .collect::<Vec<_>>();
        assert_eq!(
            terminal.len(),
            1,
            "one owned terminal audit after caller loss"
        );
        assert_eq!(terminal[0].actor_principal_id, Some(PrincipalId(1)));
        assert_eq!(
            terminal[0].status,
            if matches!(
                mode,
                "caller_loss" | "delivery_revoked" | "late_commit" | "autocommit_timeout"
            ) {
                "succeeded"
            } else {
                "failed"
            }
        );
        if let Some(caller) = caller {
            let response = caller.await.unwrap().unwrap();
            assert_eq!(
                response.status(),
                StatusCode::NOT_FOUND,
                "tenant membership alone cannot deliver a private/shared chat receipt"
            );
            let peer = delivery_peer.unwrap();
            assert_eq!(
                metadata
                    .ai_database_proposal(proposal.proposal.id, peer)
                    .await
                    .unwrap()
                    .apply_state,
                Some(AiDatabaseApplyState::Applied)
            );
            assert!(metadata.get_ai_chat(chat.id, owner).await.is_err());
            assert!(client
                .apply_ai_database_proposal(proposal.proposal.id, &apply)
                .await
                .is_err());
            assert_eq!(
                mock.invocations()
                    .iter()
                    .filter(|&&call| call == "execute")
                    .count(),
                1
            );
            let ledger = metadata
                .change_ledger(&ChangeLedgerFilter::default())
                .unwrap();
            let changes = ledger
                .entries
                .iter()
                .filter(|entry| entry.source_workflow == "ai_migration_proposal")
                .collect::<Vec<_>>();
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].outcome, ChangeLedgerOutcome::Committed);
            assert_eq!(changes[0].executed_by, owner.0);
            server.abort();
            continue;
        }
        let receipt = client
            .apply_ai_database_proposal(proposal.proposal.id, &apply)
            .await
            .unwrap();
        if mode == "autocommit_timeout" {
            assert_eq!(receipt.state, AiDatabaseApplyState::Applied, "{receipt:?}");
            assert_eq!(receipt.migration_result.as_ref().unwrap().state, MigrationRunState::Failed,
                "workflow delivery timeout must not erase independently witnessed native autocommit");
            assert!(
                !mock.invocations().contains(&"begin") && !mock.invocations().contains(&"rollback")
            );
        } else if matches!(mode, "caller_loss" | "late_commit") {
            assert_eq!(receipt.state, AiDatabaseApplyState::Applied, "{receipt:?}");
            assert_eq!(
                receipt.migration_result.as_ref().unwrap().state,
                MigrationRunState::Applied
            );
        } else {
            assert_ne!(receipt.state, AiDatabaseApplyState::Applied, "{receipt:?}");
        }
        let receipt_json = serde_json::to_string(&receipt).unwrap();
        assert!(
            !receipt_json.contains("PRIVATE-AI-NATIVE-ERROR")
                && !receipt_json.contains("PRIVATE-JOURNAL-WITNESS-ERROR")
        );
        let settled_state = receipt.state;
        assert_eq!(
            client
                .apply_ai_database_proposal(proposal.proposal.id, &apply)
                .await
                .unwrap()
                .state,
            settled_state
        );
        assert_eq!(
            mock.invocations()
                .iter()
                .filter(|&&call| call == "execute")
                .count(),
            1,
            "replay never repeats native DDL"
        );
        let ledger = client
            .change_ledger(&ChangeLedgerFilter::default())
            .await
            .unwrap();
        let changes = ledger
            .entries
            .iter()
            .filter(|entry| entry.source_workflow == "ai_migration_proposal")
            .collect::<Vec<_>>();
        assert!(!changes.is_empty());
        let observed = metadata
            .migration_native_outcomes(
                TenantId(1),
                plan.run_id,
                &MigrationNativePageRequest::default(),
            )
            .unwrap();
        assert!(observed.closed && observed.journal_present);
        if mode == "commit_witness_failure" {
            assert_eq!(
                observed.observations[0].group.outcome,
                Some(ChangeLedgerOutcome::OutcomeUnknown)
            );
            assert!(
                !mock.invocations().contains(&"rollback"),
                "failed metadata cannot roll back an acknowledged COMMIT"
            );
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].outcome, ChangeLedgerOutcome::OutcomeUnknown);
        }
        let tx = observed.observations[0]
            .group
            .transaction_id
            .map(|tx| tx.0.to_string());
        assert!(changes.iter().all(|entry| entry.transaction_id == tx));
        if matches!(mode, "timeout" | "native_error") {
            assert!(changes
                .iter()
                .all(|entry| entry.outcome == ChangeLedgerOutcome::RolledBack));
        }
        if matches!(mode, "caller_loss" | "late_commit" | "autocommit_timeout") {
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].outcome, ChangeLedgerOutcome::Committed);
        } else {
            assert!(changes
                .iter()
                .all(|entry| entry.outcome != ChangeLedgerOutcome::Committed));
        }
        assert!(!serde_json::to_string(&ledger)
            .unwrap()
            .contains("PRIVATE-AI-NATIVE-ERROR"));
        assert!(!serde_json::to_string(&sessions.list_operations())
            .unwrap()
            .contains("PRIVATE-AI-NATIVE-ERROR"));
        server.abort();
    }
}
