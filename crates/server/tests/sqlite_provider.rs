//! Real SQLite through the HTTP SDK, managed profiles and audited operations.
#![cfg(unix)]
use sift_api_types::OpenConnectionFromProfileRequest;
use sift_driver_sqlite::{FilePolicy, SqliteDriver};
use sift_metadata::{
    CredentialMode, MemorySecretStore, MetadataStore, NewConnectionProfile, PrincipalId, TenantId,
};
use sift_protocol::*;
use sift_server::{
    http::{app, AppState, AuthState},
    room_runtime::RoomRuntime,
    DriverRegistry, SessionStore,
};
use std::sync::Arc;

#[tokio::test]
async fn sqlite_snapshot_migration_creates_only_a_proven_simple_table() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("migration.db");
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(
            "CREATE TABLE future (id INTEGER NOT NULL, label TEXT);\
             CREATE TABLE lossy (id INTEGER PRIMARY KEY, label TEXT DEFAULT 'preset');",
        )
        .unwrap();
    let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
    metadata.bootstrap_local("sqlite migration test").unwrap();
    let profile = metadata
        .upsert_connection_profile(
            TenantId(1),
            PrincipalId(1),
            NewConnectionProfile {
                name: "SQLite migration fixture".into(),
                provider_id: Engine::Sqlite.provider_id(),
                configuration: serde_json::json!({"root_id":"test","path":"migration.db","mode":"read_write"}),
                semantic_engine: Some(Engine::Sqlite),
                credentials: None,
                credential_mode: CredentialMode::Shared,
                tags: vec![],
            },
        )
        .await
        .unwrap();
    let read_only_profile = metadata
        .upsert_connection_profile(
            TenantId(1),
            PrincipalId(1),
            NewConnectionProfile {
                name: "SQLite migration read-only fixture".into(),
                provider_id: Engine::Sqlite.provider_id(),
                configuration: serde_json::json!({"root_id":"test","path":"migration.db","mode":"read_only"}),
                semantic_engine: Some(Engine::Sqlite),
                credentials: None,
                credential_mode: CredentialMode::Shared,
                tags: vec![],
            },
        )
        .await
        .unwrap();
    let driver = SqliteDriver::with_files(FilePolicy {
        config: SqliteDriverConfig {
            roots: std::collections::BTreeMap::from([(
                "test".into(),
                SqliteRootConfig {
                    path: root.path().to_str().unwrap().into(),
                    allowed_tenants: vec![1],
                    read_only: false,
                },
            )]),
            max_connections: 3,
        },
        protected: vec![],
    });
    let router = app(AppState {
        sessions: SessionStore::new(DriverRegistry::builder().register(driver).build()),
        rooms: RoomRuntime::default(),
        shutdown: Default::default(),
        auth: AuthState::default(),
        metadata: Some(metadata),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = sift_client_sdk::Client::new(format!("http://{addr}"));
    let session = client.open_session(None).await.unwrap().id;
    let connection = client
        .open_connection_from_profile(
            session,
            OpenConnectionFromProfileRequest {
                tenant_id: 1,
                profile_id: profile.id.0,
            },
        )
        .await
        .unwrap()
        .id;
    let graph = client
        .catalog_graph(session, connection, CatalogGraphRequest::default())
        .await
        .unwrap();
    let snapshot = client
        .create_catalog_snapshot(
            session,
            connection,
            CreateCatalogSnapshotRequest {
                expected_catalog_revision: graph.revision,
                options: CatalogGraphOptions::default(),
                description: None,
                accept_partial: true,
            },
        )
        .await
        .unwrap();
    assert!(snapshot.graph.data.nodes.iter().any(|node| {
        node.name == "future" && node.extra.contains_key("sqlite_safe_create_sql")
    }));
    assert!(snapshot.graph.data.nodes.iter().all(|node| {
        node.name != "lossy" || !node.extra.contains_key("sqlite_safe_create_sql")
    }));
    client
        .execute(session, connection, "DROP TABLE future")
        .await
        .unwrap();
    client
        .execute(session, connection, "DROP TABLE lossy")
        .await
        .unwrap();
    let live = client
        .catalog_graph(session, connection, CatalogGraphRequest::default())
        .await
        .unwrap();
    let diff_request = SchemaDiffRequest {
        from: CatalogSourceRef::Live {
            expected_revision: live.revision,
            options: CatalogGraphOptions::default(),
        },
        to: CatalogSourceRef::Snapshot {
            snapshot_id: snapshot.id,
        },
        accepted_renames: vec![],
        max_changes: None,
    };
    let diff = client
        .compare_catalog_schemas(session, connection, diff_request.clone())
        .await
        .unwrap();
    let table_change = |name: &str| {
        diff.changes
            .iter()
            .find(|change| {
                change.kind == SchemaChangeKind::Create
                    && change.object_after.as_ref().is_some_and(|node| {
                        node.kind == CatalogNodeKind::Table && node.name == name
                    })
            })
            .unwrap()
            .id
            .clone()
    };
    let lossy_change = table_change("lossy");
    let future_change = table_change("future");
    let lossy = client
        .preview_migration(
            session,
            connection,
            PreviewMigrationRequest {
                diff: diff_request.clone(),
                expected_diff_digest: diff.digest.clone(),
                selected_changes: vec![lossy_change],
                expected_live_revision: live.revision,
                options: MigrationOptions::default(),
            },
        )
        .await;
    assert!(lossy.is_err());
    let plan = client
        .preview_migration(
            session,
            connection,
            PreviewMigrationRequest {
                diff: diff_request,
                expected_diff_digest: diff.digest,
                selected_changes: vec![future_change],
                expected_live_revision: live.revision,
                options: MigrationOptions {
                    prefer_transactional: false,
                    online_indexes: false,
                },
            },
        )
        .await
        .unwrap();
    assert!(plan.groups[0].transactional);
    let validation = client
        .validate_migration(
            session,
            connection,
            ValidateMigrationRequest {
                plan_id: plan.id,
                plan_digest: plan.digest.clone(),
                confirm_test_database: true,
            },
        )
        .await
        .unwrap();
    assert!(validation.valid && validation.rolled_back);
    assert_eq!(
        rusqlite::Connection::open(&path)
            .unwrap()
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name='future'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    let read_only_connection = client
        .open_connection_from_profile(
            session,
            OpenConnectionFromProfileRequest {
                tenant_id: 1,
                profile_id: read_only_profile.id.0,
            },
        )
        .await
        .unwrap()
        .id;
    let read_only_live = client
        .catalog_graph(
            session,
            read_only_connection,
            CatalogGraphRequest::default(),
        )
        .await
        .unwrap();
    let read_only_diff_request = SchemaDiffRequest {
        from: CatalogSourceRef::Live {
            expected_revision: read_only_live.revision,
            options: CatalogGraphOptions::default(),
        },
        to: CatalogSourceRef::Snapshot {
            snapshot_id: snapshot.id,
        },
        accepted_renames: vec![],
        max_changes: None,
    };
    let read_only_diff = client
        .compare_catalog_schemas(
            session,
            read_only_connection,
            read_only_diff_request.clone(),
        )
        .await
        .unwrap();
    let read_only_plan = client
        .preview_migration(
            session,
            read_only_connection,
            PreviewMigrationRequest {
                diff: read_only_diff_request,
                expected_diff_digest: read_only_diff.digest,
                selected_changes: vec![read_only_diff
                    .changes
                    .iter()
                    .find(|change| {
                        change.kind == SchemaChangeKind::Create
                            && change
                                .object_after
                                .as_ref()
                                .is_some_and(|node| node.name == "future")
                    })
                    .unwrap()
                    .id
                    .clone()],
                expected_live_revision: read_only_live.revision,
                options: MigrationOptions::default(),
            },
        )
        .await
        .unwrap();
    let read_only_apply = client
        .apply_migration(
            session,
            read_only_connection,
            ApplyMigrationRequest {
                plan_id: read_only_plan.id,
                plan_digest: read_only_plan.digest,
                acknowledgements: read_only_plan.required_acknowledgements,
                source: None,
            },
        )
        .await;
    assert!(!matches!(read_only_apply, Ok(run) if run.state == MigrationRunState::Applied));
    assert_eq!(
        rusqlite::Connection::open(&path)
            .unwrap()
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name='future'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    let used_plan_id = plan.id;
    let used_plan_digest = plan.digest.clone();
    let run = client
        .apply_migration(
            session,
            connection,
            ApplyMigrationRequest {
                plan_id: plan.id,
                plan_digest: plan.digest,
                acknowledgements: plan.required_acknowledgements,
                source: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(run.state, MigrationRunState::Applied);
    assert!(client
        .apply_migration(
            session,
            connection,
            ApplyMigrationRequest {
                plan_id: used_plan_id,
                plan_digest: used_plan_digest,
                acknowledgements: vec![],
                source: None,
            },
        )
        .await
        .is_err());
    assert_eq!(
        rusqlite::Connection::open(&path)
            .unwrap()
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name='future'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    server.abort();
}

#[tokio::test]
async fn sqlite_managed_profile_transactions_catalog_plans_and_atomic_import() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("data.db");
    rusqlite::Connection::open(&path).unwrap().execute_batch("CREATE TABLE items(id INTEGER PRIMARY KEY, label TEXT NOT NULL); CREATE TABLE nullable(a TEXT,b TEXT,PRIMARY KEY(a,b));").unwrap();
    let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
    metadata.bootstrap_local("sqlite test").unwrap();
    let profile=metadata.upsert_connection_profile(TenantId(1),PrincipalId(1),NewConnectionProfile{name:"SQLite fixture".into(),provider_id:Engine::Sqlite.provider_id(),configuration:serde_json::json!({"root_id":"test","path":"data.db","mode":"read_write"}),semantic_engine:Some(Engine::Sqlite),credentials:None,credential_mode:CredentialMode::Shared,tags:vec![]}).await.unwrap();
    let driver = SqliteDriver::with_files(FilePolicy {
        config: SqliteDriverConfig {
            roots: std::collections::BTreeMap::from([(
                "test".into(),
                SqliteRootConfig {
                    path: root.path().to_str().unwrap().into(),
                    allowed_tenants: vec![1],
                    read_only: false,
                },
            )]),
            max_connections: 3,
        },
        protected: vec![],
    });
    let store = SessionStore::new(DriverRegistry::builder().register(driver).build());
    let router = app(AppState {
        sessions: store.clone(),
        rooms: RoomRuntime::default(),
        shutdown: Default::default(),
        auth: AuthState::default(),
        metadata: Some(metadata.clone()),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = sift_client_sdk::Client::new(format!("http://{addr}"));
    let session = client.open_session(None).await.unwrap().id;
    let connection = client
        .open_connection_from_profile(
            session,
            OpenConnectionFromProfileRequest {
                tenant_id: 1,
                profile_id: profile.id.0,
            },
        )
        .await
        .unwrap()
        .id;
    let metrics_response = reqwest::Client::new()
        .get(format!("http://{addr}/v1/metrics"))
        .send()
        .await
        .unwrap();
    assert!(metrics_response.status().is_success());
    assert_eq!(
        metrics_response.headers()[reqwest::header::CONTENT_TYPE],
        "text/plain; version=0.0.4; charset=utf-8"
    );
    assert!(metrics_response
        .text()
        .await
        .unwrap()
        .contains("sift_http_requests_total"));
    assert!(client
        .metrics()
        .await
        .unwrap()
        .contains("sift_http_requests_total"));
    client
        .execute(session, connection, "INSERT INTO items VALUES(1,'kept')")
        .await
        .unwrap();
    let measured = client
        .profile(
            session,
            connection,
            ProfileRequest {
                connection,
                run_id: uuid::Uuid::new_v4(),
                sql: "SELECT label FROM items WHERE id = ?".into(),
                params: vec![Value::Int64(1)],
                timeout_ms: 10_000,
                workload_confirmed: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(measured.plan.engine, Engine::Sqlite);
    assert!(!measured.plan.analyzed);
    assert_eq!(measured.rows_returned, Some(1));
    assert!(measured.server_elapsed_ns > 0);
    assert_eq!(measured.planning_ms, None);
    assert_eq!(measured.execution_ms, None);
    let environment = measured.environment.as_ref().unwrap();
    assert_eq!(
        environment.instrumentation,
        "estimated_plan_plus_measured_read"
    );
    assert_eq!(environment.isolation, "read_only_serializable");
    fn no_invented_metrics(node: &PlanNode) -> bool {
        node.est_cost.is_none()
            && node.actual_rows.is_none()
            && node.actual_ms.is_none()
            && node.children.iter().all(no_invented_metrics)
    }
    assert!(no_invented_metrics(&measured.plan.root));
    let benchmark = client
        .benchmark(
            session,
            connection,
            BenchmarkRequest {
                run_id: uuid::Uuid::new_v4(),
                sql: "SELECT label FROM items WHERE id = ?".into(),
                params: vec![Value::Int64(1)],
                warmups: 0,
                iterations: 1,
                query_timeout_ms: 10_000,
                total_budget_ms: 10_000,
                delay_ms: 0,
                workload_confirmed: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(benchmark.samples.len(), 1);
    assert_eq!(benchmark.samples[0].outcome, BenchmarkOutcome::Success);
    assert_eq!(benchmark.samples[0].rows, Some(1));
    let refused_write = client
        .profile(
            session,
            connection,
            ProfileRequest {
                connection,
                run_id: uuid::Uuid::new_v4(),
                sql: "UPDATE items SET label = 'changed' WHERE id = 1".into(),
                params: Vec::new(),
                timeout_ms: 10_000,
                workload_confirmed: true,
            },
        )
        .await
        .unwrap_err();
    assert!(refused_write.to_string().contains("read query"));
    let refused_result = client
        .profile(
            session,
            connection,
            ProfileRequest {
                connection,
                run_id: uuid::Uuid::new_v4(),
                sql: "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x < 10001) SELECT x FROM c".into(),
                params: Vec::new(),
                timeout_ms: 10_000,
                workload_confirmed: true,
            },
        )
        .await
        .unwrap_err();
    assert!(refused_result
        .to_string()
        .contains("SQLite profile result exceeds row or byte limit"));
    assert_eq!(
        rusqlite::Connection::open(&path)
            .unwrap()
            .query_row("SELECT label FROM items WHERE id=1", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "kept"
    );
    // Parquet stays typed through the real SQLite driver and atomic importer.
    for quick in [false, true] {
        let report = client
            .check_integrity(session, connection, IntegrityCheckRequest::Sqlite { quick })
            .await
            .unwrap();
        assert_eq!(report.outcome, IntegrityOutcome::NoIssuesReported);
        assert!(report.findings.is_empty());
    }
    // Introduce a recoverable constraint violation, not file corruption.
    for sql in [
        "PRAGMA main.integrity_check(0)",
        "PRAGMA main.quick_check(1001)",
        "PRAGMA user_version=123",
    ] {
        assert!(client.execute(session, connection, sql).await.is_err());
    }
    let local = rusqlite::Connection::open(&path).unwrap();
    local.execute_batch("PRAGMA ignore_check_constraints=ON; CREATE TABLE integrity_bad(v INTEGER CHECK(v>0)); INSERT INTO integrity_bad VALUES(-1);").unwrap();
    let report = client
        .check_integrity(
            session,
            connection,
            IntegrityCheckRequest::Sqlite { quick: false },
        )
        .await
        .unwrap();
    assert_eq!(report.outcome, IntegrityOutcome::IssuesReported);
    assert!(report
        .findings
        .iter()
        .any(|finding| finding.contains("integrity_bad")));
    assert_eq!(
        local
            .query_row("SELECT v FROM integrity_bad", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        -1
    );
    local.execute_batch("DROP TABLE integrity_bad").unwrap();
    drop(local);
    assert!(client
        .check_integrity(
            session,
            connection,
            IntegrityCheckRequest::SqlServer {
                physical_only: true
            }
        )
        .await
        .is_err());
    {
        use futures::StreamExt;
        let stream = store
            .export_stream(
                session,
                connection,
                ExportRequest {
                    sql: "SELECT id,label FROM items".into(),
                    params: vec![],
                    format: ExportFormat::Parquet,
                    header: true,
                    null_display: None,
                },
            )
            .await
            .unwrap();
        futures::pin_mut!(stream);
        let mut data = Vec::new();
        while let Some(chunk) = stream.next().await {
            data.extend_from_slice(&chunk.unwrap());
        }
        let recipe = TransferRecipe {
            id: TransferRecipeId(1),
            workspace_id: WorkspaceId(1),
            name: "parquet".into(),
            direction: TransferDirection::Import,
            source: TransferEndpoint::Upload,
            sink: TransferEndpoint::Table,
            format_id: "parquet".into(),
            format_version: "1".into(),
            options: serde_json::json!({}),
            revision: 1,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let request = sift_metadata::http::ExecuteTransferRecipeRequest {
            session_id: session,
            connection_id: connection,
            sql: None,
            params: vec![],
            data: Some(data),
            table: Some(ObjectPath {
                catalog: None,
                schema: None,
                name: "parquet_copy".into(),
                kind: Some(ObjectKind::Table),
                routine_args: None,
            }),
            sheet: None,
            create_table: true,
            conflict_policy: None,
            dry_run: true,
            resume_from_row: 0,
            type_mappings: Default::default(),
        };
        sift_server::transfer::execute_recipe(
            &store,
            &metadata,
            PrincipalId(1),
            &recipe,
            request.clone(),
        )
        .await
        .unwrap();
        let db = rusqlite::Connection::open(&path).unwrap();
        assert!(db.prepare("SELECT * FROM parquet_copy").is_err());
        let mut apply = request;
        apply.dry_run = false;
        sift_server::transfer::execute_recipe(
            &store,
            &metadata,
            PrincipalId(1),
            &recipe,
            apply.clone(),
        )
        .await
        .unwrap();
        let row = db
            .query_row("SELECT id,label FROM parquet_copy", [], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })
            .unwrap();
        assert_eq!(row, (1, "kept".into()));
        db.execute_batch("CREATE UNIQUE INDEX copy_id ON parquet_copy(id)")
            .unwrap();
        apply.create_table = false;
        assert!(sift_server::transfer::execute_recipe(
            &store,
            &metadata,
            PrincipalId(1),
            &recipe,
            apply
        )
        .await
        .is_err());
        assert_eq!(
            db.query_row("SELECT count(*) FROM parquet_copy", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    let tx = client
        .begin_transaction(
            session,
            connection,
            TxMode {
                isolation: IsolationLevel::Serializable,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    client
        .create_savepoint(session, connection, tx.tx_id, "outer")
        .await
        .unwrap();
    client
        .execute_in_tx(session, &tx, "INSERT INTO items VALUES(2,'rolled back')")
        .await
        .unwrap();
    client
        .create_savepoint(session, connection, tx.tx_id, "inner")
        .await
        .unwrap();
    client
        .rollback_to_savepoint(session, connection, tx.tx_id, "outer")
        .await
        .unwrap();
    client
        .release_savepoint(session, connection, tx.tx_id, "outer")
        .await
        .unwrap();
    client
        .commit_transaction(session, connection, tx.tx_id)
        .await
        .unwrap();
    let preview_sql =
        sift_snippets::table_preview_sql(&Engine::Sqlite.provider_id(), "main", "items")
            .expect("SQLite table previews are supported");
    assert_eq!(
        client
            .execute(session, connection, &preview_sql)
            .await
            .unwrap()
            .rows
            .len(),
        1
    );
    client
        .execute(
            session,
            connection,
            "CREATE TEMP TABLE local_temp(value TEXT)",
        )
        .await
        .unwrap();
    let graph = client
        .catalog_graph(session, connection, CatalogGraphRequest::default())
        .await
        .unwrap();
    assert_eq!(graph.data.coverage.state, CatalogCoverageState::Partial);
    assert!(graph.data.nodes.iter().any(|n| n.name == "local_temp"));
    let sql = "SELECT i.la FROM items i";
    let completion = client
        .complete(
            session,
            connection,
            completion::CompletionRequest {
                sql: sql.into(),
                cursor: 11,
                limit: Some(20),
            },
        )
        .await
        .unwrap();
    assert!(completion.candidates.iter().any(|i| i.label == "label"));
    let plan = client
        .explain(
            session,
            connection,
            ExplainRequest {
                connection,
                sql: "SELECT * FROM items WHERE id=?1".into(),
                params: vec![Value::Int64(1)],
                analyze: false,
            },
        )
        .await
        .unwrap();
    assert!(!plan.root.children.is_empty());
    assert!(plan.root.est_cost.is_none());
    assert!(plan.raw.contains("SEARCH"));
    let request = |data: &str| CsvImportRequest {
        table: "main.items".into(),
        data: data.as_bytes().to_vec(),
        header: true,
        delimiter: ',',
        null_value: Some("NULL".into()),
        create_table: false,
        conflict_policy: CsvConflictPolicy::Abort,
        dry_run: false,
        resume_from_row: 0,
        type_mappings: Default::default(),
    };
    assert!(client
        .import_csv(
            session,
            connection,
            request("id,label\n2,new\n1,conflict\n")
        )
        .await
        .is_err());
    assert_eq!(
        client
            .execute(session, connection, "SELECT * FROM items")
            .await
            .unwrap()
            .rows
            .len(),
        1
    );
    let imported = client
        .import_csv(session, connection, request("id,label\n2,new\n3,more\n"))
        .await
        .unwrap();
    assert_eq!(imported.rows_inserted, 2);
    let mut skip = request("id,label\n2,duplicate\n4,new\n");
    skip.conflict_policy = CsvConflictPolicy::Skip;
    let imported = client.import_csv(session, connection, skip).await.unwrap();
    assert_eq!((imported.rows_inserted, imported.rows_skipped), (1, 1));
    let mut quarantine = request("id,label\n2,NULL\n4,duplicate\n");
    quarantine.conflict_policy = CsvConflictPolicy::Quarantine;
    let rejected = client
        .import_csv(session, connection, quarantine)
        .await
        .unwrap();
    assert_eq!((rejected.rows_inserted, rejected.rows_skipped), (0, 2));
    assert_eq!(rejected.quarantined_rows[0].row_number, 0);
    assert_eq!(
        rejected.quarantined_rows[0].values,
        vec![Some("2".into()), None]
    );
    assert_eq!(
        client
            .execute(
                session,
                connection,
                "UPDATE items SET label='none' WHERE id=999"
            )
            .await
            .unwrap()
            .affected_rows,
        Some(0)
    );

    let edits = EditSet {
        table: ObjectPath::new("items"),
        edits: vec![RowEdit::Update {
            key: RowKey {
                columns: vec![CellEdit {
                    column: "id".into(),
                    value: Value::Int64(1),
                }],
            },
            changes: vec![CellEdit {
                column: "label".into(),
                value: Value::Text("edited".into()),
            }],
            expected: vec![CellEdit {
                column: "label".into(),
                value: Value::Text("kept".into()),
            }],
        }],
    };
    let preview = client
        .preview_edits(
            session,
            connection,
            PreviewEditsRequest {
                connection,
                edit_set: edits.clone(),
            },
        )
        .await
        .unwrap();
    assert!(preview.statements[0].sql.contains("?1"));
    let applied = client
        .apply_edits(
            session,
            connection,
            ApplyEditsRequest {
                connection,
                edit_set: edits.clone(),
                tx: None,
            },
        )
        .await
        .unwrap();
    assert!(applied.committed);
    assert_eq!(applied.applied[0].affected_rows, 1);
    assert!(client
        .apply_edits(
            session,
            connection,
            ApplyEditsRequest {
                connection,
                edit_set: edits,
                tx: None
            }
        )
        .await
        .is_err());
    assert!(client
        .preview_edits(
            session,
            connection,
            PreviewEditsRequest {
                connection,
                edit_set: EditSet {
                    table: ObjectPath::new("nullable"),
                    edits: vec![]
                }
            }
        )
        .await
        .is_err());
    let refreshed = client
        .catalog_graph(session, connection, CatalogGraphRequest::default())
        .await
        .unwrap();
    assert_eq!(refreshed.revision, graph.revision);
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch("ALTER TABLE items ADD COLUMN external TEXT")
        .unwrap();
    assert_ne!(
        client
            .catalog_graph(session, connection, CatalogGraphRequest::default())
            .await
            .unwrap()
            .revision,
        graph.revision
    );
    let tx = client
        .begin_transaction(
            session,
            connection,
            TxMode {
                isolation: IsolationLevel::Serializable,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let stream = client.start_query_stream_with(session,connection,"WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<100000000) SELECT sum(x) FROM n",vec![],Some(TxHandleRef {tx_id:tx.tx_id,connection:tx.connection,mode:tx.mode})).await.unwrap();
    client
        .cancel(session, connection, stream.cursor_id())
        .await
        .unwrap();
    assert!(client.list_transactions(session).await.unwrap().is_empty());
    assert!(client
        .execute(session, connection, "SELECT 1")
        .await
        .is_err());
    drop(stream);
    client.close_session(session).await.unwrap();
    // Resume must be safe even without a unique constraint on imported rows.
    let session = client.open_session(None).await.unwrap().id;
    let connection = client
        .open_connection_from_profile(
            session,
            OpenConnectionFromProfileRequest {
                tenant_id: 1,
                profile_id: profile.id.0,
            },
        )
        .await
        .unwrap()
        .id;
    client.execute(session, connection, "CREATE TABLE resume_rows(value TEXT); CREATE TRIGGER pause_resume BEFORE INSERT ON resume_rows WHEN NEW.value = '100' BEGIN SELECT RAISE(ABORT, 'pause'); END;").await.unwrap();
    let source = format!(
        "value\n{}",
        (0..150).map(|n| format!("{n}\n")).collect::<String>()
    );
    let mut resumable = request(&source);
    resumable.table = "main.resume_rows".into();
    let run_id = "8433f24a-2465-4a26-a6f9-cb24012aee08";
    let ledger = "main.resume_checkpoint";
    let authority = "workspace:1:recipe:1:actor:1";
    let mut preview = resumable.clone();
    preview.dry_run = true;
    assert!(
        sift_server::csv_import::import_with_checkpoint(
            &store, session, connection, preview, ledger, run_id, authority
        )
        .await
        .unwrap()
        .dry_run
    );
    assert!(sift_server::csv_import::import_with_checkpoint(
        &store,
        session,
        connection,
        resumable.clone(),
        ledger,
        run_id,
        authority
    )
    .await
    .is_err());
    let persisted = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        persisted
            .query_row("SELECT count(*) FROM resume_rows", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        100
    );
    assert_eq!(
        persisted
            .query_row("SELECT next_row FROM resume_checkpoint", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        100
    );
    drop(persisted);
    client
        .execute(session, connection, "DROP TRIGGER pause_resume")
        .await
        .unwrap();
    let other_connection = client
        .open_connection_from_profile(
            session,
            OpenConnectionFromProfileRequest {
                tenant_id: 1,
                profile_id: profile.id.0,
            },
        )
        .await
        .unwrap()
        .id;
    let resumed = sift_server::csv_import::import_with_checkpoint(
        &store,
        session,
        other_connection,
        resumable.clone(),
        ledger,
        run_id,
        authority,
    )
    .await
    .unwrap();
    assert_eq!((resumed.rows_inserted, resumed.resume_from_row), (50, 150));
    let replayed = sift_server::csv_import::import_with_checkpoint(
        &store,
        session,
        other_connection,
        resumable.clone(),
        ledger,
        run_id,
        authority,
    )
    .await
    .unwrap();
    assert_eq!(replayed.rows_inserted, 0);
    let mut changed = resumable.clone();
    changed.data.extend_from_slice(b"151\n");
    assert!(sift_server::csv_import::import_with_checkpoint(
        &store,
        session,
        other_connection,
        changed,
        ledger,
        run_id,
        authority
    )
    .await
    .is_err());
    assert!(sift_server::csv_import::import_with_checkpoint(
        &store,
        session,
        other_connection,
        resumable,
        ledger,
        run_id,
        "different actor"
    )
    .await
    .is_err());
    let persisted = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        persisted
            .query_row("SELECT count(*) FROM resume_rows", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        150
    );
    drop(persisted);
    client
        .close_connection(session, other_connection)
        .await
        .unwrap();
    client.close_session(session).await.unwrap();
    server.abort();
}

#[tokio::test]
async fn ai_row_proposals_require_review_confirm_production_and_replay_once() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("ai-review.db");
    let database = rusqlite::Connection::open(&path).unwrap();
    database.execute_batch("CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT NOT NULL); INSERT INTO items VALUES (1,'original'),(2,'remove');").unwrap();
    let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
    metadata.bootstrap_local("AI database review").unwrap();
    let profile = metadata.upsert_connection_profile(TenantId(1),PrincipalId(1),NewConnectionProfile {
        name:"Production fixture".into(),provider_id:Engine::Sqlite.provider_id(),semantic_engine:Some(Engine::Sqlite),
        configuration:serde_json::json!({"root_id":"test","path":"ai-review.db","mode":"read_write"}),
        credentials:None,credential_mode:CredentialMode::Shared,tags:vec!["production".into()],
    }).await.unwrap();
    let driver = SqliteDriver::with_files(FilePolicy {
        config: SqliteDriverConfig {
            roots: std::collections::BTreeMap::from([(
                "test".into(),
                SqliteRootConfig {
                    path: root.path().to_str().unwrap().into(),
                    allowed_tenants: vec![1],
                    read_only: false,
                },
            )]),
            max_connections: 3,
        },
        protected: vec![],
    });
    let mut auth = AuthState::default();
    auth.ai.enabled = true;
    let router = app(AppState {
        sessions: SessionStore::new(DriverRegistry::builder().register(driver).build()),
        rooms: RoomRuntime::default(),
        shutdown: Default::default(),
        auth,
        metadata: Some(metadata.clone()),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = sift_client_sdk::Client::new(format!("http://{addr}"));
    let session = client.open_session(None).await.unwrap().id;
    let connection = client
        .open_connection_from_profile(
            session,
            OpenConnectionFromProfileRequest {
                tenant_id: 1,
                profile_id: profile.id.0,
            },
        )
        .await
        .unwrap()
        .id;
    let chat = client
        .create_ai_chat(&CreateAiChatRequest {
            tenant_id: 1,
            room_id: None,
            title: "Review rows".into(),
        })
        .await
        .unwrap();
    let lease = client
        .start_ai_turn(
            chat.id,
            &StartAiTurnRequest {
                attachment_previews: Vec::new(),
                client_request_id: uuid::Uuid::new_v4(),
                desktop_id: uuid::Uuid::new_v4(),
                prompt: "Propose changes".into(),
                provider: AiProvider::Codex,
                model: None,
                mode: AiMode::Propose,
                context: AiTurnContext {
                    attachments: Vec::new(),
                    target: ToolContext {
                        tenant_id: Some(1),
                        room_id: None,
                        profile_id: Some(profile.id.0),
                        connection_id: Some(format!("{}:{}", session.0, connection.0)),
                        document_id: None,
                    },
                    editor_item_id: None,
                    database: None,
                    dialect: Some("sqlite".into()),
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
    let catalog = client
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
    let graph: CatalogGraph = serde_json::from_value(catalog.result).unwrap();
    let invoke = |tool, parameters| {
        let client = client.clone();
        let run_id = lease.run.id;
        let lease_token = lease.lease_token;
        async move {
            client
                .invoke_ai_tool(
                    run_id,
                    &InvokeAiToolRequest {
                        call_id: uuid::Uuid::new_v4(),
                        lease_token,
                        tool,
                        sql: None,
                        parameters,
                    },
                )
                .await
        }
    };
    let object = graph
        .data
        .nodes
        .iter()
        .find(|node| node.kind == CatalogNodeKind::Table && node.name == "items")
        .unwrap();
    let ddl = invoke(
        AiToolKind::ObjectDdl,
        Some(AiToolParameters::ObjectDdl {
            expected_catalog_revision: graph.revision,
            object_id: object.id.clone(),
        }),
    )
    .await
    .unwrap();
    assert!(ddl.result["object"]["ddl"]
        .as_str()
        .unwrap()
        .contains("CREATE TABLE"));
    assert!(invoke(
        AiToolKind::ObjectDdl,
        Some(AiToolParameters::ObjectDdl {
            expected_catalog_revision: CatalogRevision(graph.revision.0 + 1),
            object_id: object.id.clone()
        })
    )
    .await
    .is_err());
    let peer = metadata
        .create_principal("history-peer", "peer", None)
        .unwrap()
        .id;
    metadata
        .upsert_tenant_membership(TenantId(1), peer, sift_metadata::MembershipRole::Member)
        .unwrap();
    let other_profile=metadata.upsert_connection_profile(TenantId(1),PrincipalId(1),NewConnectionProfile {
        name:"Other history source".into(),provider_id:Engine::Sqlite.provider_id(),semantic_engine:Some(Engine::Sqlite),
        configuration:serde_json::json!({"root_id":"test","path":"ai-review.db","mode":"read_write"}),
        credentials:None,credential_mode:CredentialMode::Shared,tags:vec![],
    }).await.unwrap();
    for (actor, profile_id, sql) in [
        (peer, profile.id, "SELECT 'peer-only'"),
        (PrincipalId(1), other_profile.id, "SELECT 'other-profile'"),
    ] {
        metadata
            .record_query_history(sift_metadata::NewQueryHistory {
                principal_id: actor,
                room_id: None,
                connection_profile_id: Some(profile_id),
                sql_text: sql.into(),
                duration_ms: Some(1),
                row_count: None,
                status: sift_metadata::QueryStatus::Error,
                error_code: Some("fixture_error".into()),
                error_message: Some("Source-specific runtime error".into()),
                variable_descriptors: vec![],
            })
            .unwrap();
    }
    let scoped_history = invoke(AiToolKind::QueryHistory, None).await.unwrap().result;
    assert!(!scoped_history.to_string().contains("peer-only"));
    assert!(!scoped_history.to_string().contains("other-profile"));
    for index in 0..24 {
        metadata
            .record_query_history(sift_metadata::NewQueryHistory {
                principal_id: PrincipalId(1),
                room_id: None,
                connection_profile_id: Some(profile.id),
                sql_text: format!("SELECT {index}"),
                duration_ms: Some(1),
                row_count: None,
                status: sift_metadata::QueryStatus::Error,
                error_code: Some("fixture_error".into()),
                error_message: Some("Owned runtime error".into()),
                variable_descriptors: vec![],
            })
            .unwrap();
    }
    let history = invoke(AiToolKind::QueryHistory, None).await.unwrap().result;
    assert_eq!(history["items"].as_array().unwrap().len(), 20);
    assert_eq!(history["truncated"], true);
    let text = history.to_string();
    assert!(!text.contains("peer-only"));
    assert!(!text.contains("other-profile"));
    assert!(text.contains("Owned runtime error"));
    let mut capture = PlanCapture {
        id: PlanCaptureId(uuid::Uuid::new_v4()),
        tenant_id: 1,
        connection_profile_id: profile.id.0,
        creator_principal_id: 1,
        provider: graph.provider.clone(),
        server_version: "fixture".into(),
        engine: Engine::Sqlite,
        source_digest: format!("sha256:{}", "a".repeat(64)),
        document_revision: 1,
        statement_id: "fixture-statement".into(),
        statement_fingerprint: format!("sha256:{}", "b".repeat(64)),
        catalog_revision: graph.revision,
        analyzed: true,
        captured_at: chrono::Utc::now(),
        duration_ms: 1,
        root: PlanNode::new("SCAN items"),
        warnings: vec![],
        complete: true,
        revision: 1,
        raw_response: None,
        source: None,
    };
    capture.root.actual_rows = Some(2.0);
    capture.root.children = (0..200)
        .map(|_| {
            let mut child = PlanNode::new("owned child".repeat(30));
            child.relation = Some("long relation".repeat(80));
            child.extra.insert(
                "bounded_native_detail".into(),
                serde_json::json!("x".repeat(900)),
            );
            child
        })
        .collect();
    metadata.create_plan_capture(&capture).unwrap();
    let owned_id = capture.id;
    capture.id = PlanCaptureId(uuid::Uuid::new_v4());
    capture.creator_principal_id = peer.0;
    metadata.create_plan_capture(&capture).unwrap();
    let summaries = invoke(AiToolKind::PlanCaptures, None).await.unwrap().result;
    assert_eq!(summaries["items"].as_array().unwrap().len(), 1);
    assert_eq!(summaries["items"][0]["id"], serde_json::json!(owned_id));
    let saved = invoke(
        AiToolKind::PlanCapture,
        Some(AiToolParameters::PlanCapture {
            capture_id: owned_id,
        }),
    )
    .await
    .unwrap()
    .result;
    assert_eq!(saved["capture"]["analyzed"], true);
    assert_eq!(saved["truncated"], true);
    assert!(
        saved["capture"]["root"]["children"]
            .as_array()
            .unwrap()
            .len()
            < 200
    );
    assert!(saved["capture"]["raw_response"].is_null());
    assert!(invoke(
        AiToolKind::PlanCapture,
        Some(AiToolParameters::PlanCapture {
            capture_id: capture.id
        })
    )
    .await
    .is_err());

    let cell = |column: &str, value: Value| CellEdit {
        column: column.into(),
        value,
    };
    let key = |id| RowKey {
        columns: vec![cell("id", Value::Int64(id))],
    };
    let draft = AiDatabaseDraft::RowEditSet {
        expected_catalog_revision: graph.revision,
        edit_set: EditSet {
            table: ObjectPath {
                catalog: None,
                schema: Some("main".into()),
                name: "items".into(),
                kind: Some(ObjectKind::Table),
                routine_args: None,
            },
            edits: vec![
                RowEdit::Insert {
                    values: vec![cell("label", Value::Text("created".into()))],
                },
                RowEdit::Update {
                    key: key(1),
                    changes: vec![cell("label", Value::Text("updated".into()))],
                    expected: vec![cell("label", Value::Text("original".into()))],
                },
                RowEdit::Delete {
                    key: key(2),
                    expected: vec![cell("label", Value::Text("remove".into()))],
                },
            ],
        },
    };
    let request = StageAiDatabaseProposalRequest {
        client_request_id: uuid::Uuid::new_v4(),
        lease_token: lease.lease_token,
        draft,
    };
    let proposal = client
        .stage_ai_database_proposal(lease.run.id, &request)
        .await
        .unwrap();
    assert_eq!(
        client
            .stage_ai_database_proposal(lease.run.id, &request)
            .await
            .unwrap()
            .proposal
            .id,
        proposal.proposal.id
    );
    let review_connection = client
        .open_connection_from_profile(
            session,
            OpenConnectionFromProfileRequest {
                tenant_id: 1,
                profile_id: profile.id.0,
            },
        )
        .await
        .unwrap()
        .id;
    assert_eq!(
        client
            .catalog_graph(session, review_connection, CatalogGraphRequest::default())
            .await
            .unwrap()
            .database_identity,
        graph.database_identity
    );
    let review = client
        .review_ai_database_proposal(
            proposal.proposal.id,
            &ReviewAiDatabaseProposalRequest {
                client_request_id: uuid::Uuid::new_v4(),
                session,
                connection: review_connection,
            },
        )
        .await
        .unwrap();
    assert!(review.production);
    let AiDatabasePreview::RowEditSet { plan } = &review.preview else {
        panic!("expected row preview")
    };
    assert_eq!(plan.statements.len(), 3);
    assert_eq!(
        database
            .query_row("SELECT label FROM items WHERE id=1", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "original"
    );
    assert_eq!(
        database
            .query_row("SELECT COUNT(*) FROM items", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        2
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
    apply.production_confirmation = Some(review.database_label.clone());
    let (first, second) = tokio::join!(
        client.apply_ai_database_proposal(proposal.proposal.id, &apply),
        client.apply_ai_database_proposal(proposal.proposal.id, &apply)
    );
    let first = first.unwrap();
    let second = second.unwrap();
    assert!(
        first.state == AiDatabaseApplyState::Applied
            || second.state == AiDatabaseApplyState::Applied
    );
    let replay = client
        .apply_ai_database_proposal(proposal.proposal.id, &apply)
        .await
        .unwrap();
    assert_eq!(replay.state, AiDatabaseApplyState::Applied);
    assert_eq!(
        database
            .query_row(
                "SELECT COUNT(*) FROM items WHERE label='created'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        database
            .query_row("SELECT label FROM items WHERE id=1", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "updated"
    );
    assert_eq!(
        database
            .query_row(
                "SELECT COUNT(*) FROM items WHERE label='remove'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    let ledger = client
        .change_ledger(&ChangeLedgerFilter::default())
        .await
        .unwrap();
    assert_eq!(
        ledger
            .entries
            .iter()
            .filter(|entry| entry.source_workflow == "ai_row_proposal")
            .count(),
        3
    );
    assert!(ledger
        .entries
        .iter()
        .all(|entry| entry.authored_by == Some(1)
            && entry.approved_by == Some(1)
            && entry.executed_by == 1));
    apply.client_request_id = uuid::Uuid::new_v4();
    assert!(client
        .apply_ai_database_proposal(proposal.proposal.id, &apply)
        .await
        .is_err());
    assert!(client
        .discard_ai_database_proposal(proposal.proposal.id)
        .await
        .is_err());
    // A schema change after staging must invalidate human review before any write.
    let stale = client
        .stage_ai_database_proposal(
            lease.run.id,
            &StageAiDatabaseProposalRequest {
                client_request_id: uuid::Uuid::new_v4(),
                lease_token: lease.lease_token,
                draft: AiDatabaseDraft::RowEditSet {
                    expected_catalog_revision: graph.revision,
                    edit_set: EditSet {
                        table: ObjectPath {
                            schema: Some("main".into()),
                            kind: Some(ObjectKind::Table),
                            ..ObjectPath::new("items")
                        },
                        edits: vec![RowEdit::Insert {
                            values: vec![cell("label", Value::Text("must not execute".into()))],
                        }],
                    },
                },
            },
        )
        .await
        .unwrap();
    database
        .execute_batch("ALTER TABLE items ADD COLUMN changed TEXT")
        .unwrap();
    assert!(client
        .review_ai_database_proposal(
            stale.proposal.id,
            &ReviewAiDatabaseProposalRequest {
                client_request_id: uuid::Uuid::new_v4(),
                session,
                connection
            }
        )
        .await
        .is_err());
    assert_eq!(
        database
            .query_row(
                "SELECT COUNT(*) FROM items WHERE label='must not execute'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    server.abort();
}

#[tokio::test]
async fn ai_attachments_bind_exact_executions_and_reject_forged_or_stale_previews() {
    let root = tempfile::tempdir().unwrap();
    rusqlite::Connection::open(root.path().join("attachments.db")).unwrap()
        .execute_batch("CREATE TABLE samples(id INTEGER PRIMARY KEY,label TEXT); INSERT INTO samples VALUES(1,'first'),(2,'second');").unwrap();
    let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
    metadata.bootstrap_local("AI attachments").unwrap();
    let profile = metadata.upsert_connection_profile(TenantId(1),PrincipalId(1),NewConnectionProfile {
        name:"Attachment source".into(),provider_id:Engine::Sqlite.provider_id(),semantic_engine:Some(Engine::Sqlite),
        configuration:serde_json::json!({"root_id":"test","path":"attachments.db","mode":"read_write"}),
        credentials:None,credential_mode:CredentialMode::Shared,tags:vec![],
    }).await.unwrap();
    let driver = SqliteDriver::with_files(FilePolicy {
        config: SqliteDriverConfig {
            roots: std::collections::BTreeMap::from([(
                "test".into(),
                SqliteRootConfig {
                    path: root.path().to_str().unwrap().into(),
                    allowed_tenants: vec![1],
                    read_only: false,
                },
            )]),
            max_connections: 3,
        },
        protected: vec![],
    });
    let mut auth = AuthState::default();
    auth.ai.enabled = true;
    let sessions = SessionStore::new(DriverRegistry::builder().register(driver).build());
    let router = app(AppState {
        sessions: sessions.clone(),
        rooms: RoomRuntime::default(),
        shutdown: Default::default(),
        auth,
        metadata: Some(metadata.clone()),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = sift_client_sdk::Client::new(format!("http://{addr}"));
    let session = client.open_session(None).await.unwrap().id;
    let connection = client
        .open_connection_from_profile(
            session,
            OpenConnectionFromProfileRequest {
                tenant_id: 1,
                profile_id: profile.id.0,
            },
        )
        .await
        .unwrap()
        .id;
    let chat = client
        .create_ai_chat(&CreateAiChatRequest {
            tenant_id: 1,
            room_id: None,
            title: "Exact sources".into(),
        })
        .await
        .unwrap();
    let target = ToolContext {
        tenant_id: Some(1),
        room_id: None,
        profile_id: Some(profile.id.0),
        connection_id: Some(format!("{}:{}", session.0, connection.0)),
        document_id: None,
    };
    let mut stream = client
        .start_query_stream_with(
            session,
            connection,
            "SELECT label AS repeated, id AS repeated FROM samples WHERE id >= ? ORDER BY id",
            vec![Value::Int64(1)],
            None,
        )
        .await
        .unwrap();
    let result_id = stream
        .ai_result_id()
        .expect("managed websocket execution is retained");
    let mut columns = Vec::new();
    loop {
        let (seq, page) = stream.next_page().await.unwrap();
        if let Page::NextResult {
            columns: source, ..
        } = &page
        {
            columns = source.clone();
        }
        let done = matches!(page, Page::Done { .. });
        stream.acknowledge(seq).await.unwrap();
        if done {
            break;
        }
    }
    let digest = sift_server::comparison::schema_digest(&columns);
    let source = AiAttachmentSource::QueryRows {
        result_id,
        result_set: 0,
        schema_digest: digest.clone(),
        row_ordinals: vec![1, 0],
        column_indices: vec![1, 0],
    };
    let request = PreviewAiAttachmentRequest {
        target: target.clone(),
        source: source.clone(),
    };
    let preview = client
        .preview_ai_attachment(chat.id, &request)
        .await
        .unwrap();
    assert_eq!(
        preview.attachment.content["rows"][0],
        serde_json::json!(Row::new(vec![
            Value::Int64(2),
            Value::Text("second".into())
        ]))
    );
    assert_eq!(preview.attachment.content["columns"][0]["name"], "repeated");
    assert_eq!(preview.attachment.content["columns"][1]["name"], "repeated");
    assert_eq!(
        preview.attachment.content["executed_sql"],
        "SELECT label AS repeated, id AS repeated FROM samples WHERE id >= ? ORDER BY id"
    );
    assert!(preview.attachment.content.get("params").is_none());
    assert!(!preview.requires_publication_ack);
    // A later execution cannot change the immutable snapshot reviewed above.
    let later = client
        .execute(
            session,
            connection,
            "SELECT 'later' AS repeated, 99 AS repeated",
        )
        .await
        .unwrap();
    assert_ne!(later.ai_result_id, Some(result_id));
    assert_eq!(
        client
            .preview_ai_attachment(chat.id, &request)
            .await
            .unwrap()
            .attachment
            .sha256,
        preview.attachment.sha256
    );
    let mut turn = StartAiTurnRequest {
        client_request_id: uuid::Uuid::new_v4(),
        desktop_id: uuid::Uuid::new_v4(),
        prompt: "Explain my selection".into(),
        provider: AiProvider::Codex,
        model: None,
        mode: AiMode::Read,
        context: AiTurnContext {
            target: target.clone(),
            attachments: vec![AiContextAttachment {
                source: source.clone(),
                label: "forged".into(),
                content: serde_json::json!({"credential":"forged-inline-body"}),
                sha256: "forged".into(),
                truncated: false,
                origin_visibility: AiVisibility::Private,
                published_by: None,
            }],
            editor_item_id: None,
            database: None,
            dialect: Some("sqlite".into()),
            environment_label: None,
            sql: None,
            current_error: None,
            staged_change_count: 0,
            publication_id: None,
        },
        attachment_previews: vec![],
    };
    let lease = client.start_ai_turn(chat.id, &turn).await.unwrap();
    assert!(client.ai_runs(chat.id).await.unwrap()[0]
        .context
        .attachments
        .is_empty());
    client
        .finish_ai_run(
            lease.run.id,
            &FinishAiRunRequest {
                lease_token: lease.lease_token,
                status: AiRunStatus::Completed,
            },
        )
        .await
        .unwrap();
    turn.client_request_id = uuid::Uuid::new_v4();
    turn.attachment_previews = vec![AcceptAiAttachment {
        preview_id: preview.id,
        expected_sha256: "wrong".into(),
        publish_to_room: false,
    }];
    assert!(client.start_ai_turn(chat.id, &turn).await.is_err());
    turn.attachment_previews[0].expected_sha256 = preview.attachment.sha256.clone();
    let lease = client.start_ai_turn(chat.id, &turn).await.unwrap();
    let persisted = client
        .ai_runs(chat.id)
        .await
        .unwrap()
        .into_iter()
        .find(|detail| detail.run.id == lease.run.id)
        .unwrap();
    assert_eq!(
        persisted.context.attachments,
        vec![preview.attachment.clone()]
    );
    assert!(!serde_json::to_string(&persisted)
        .unwrap()
        .contains("forged-inline-body"));
    client
        .finish_ai_run(
            lease.run.id,
            &FinishAiRunRequest {
                lease_token: lease.lease_token,
                status: AiRunStatus::Completed,
            },
        )
        .await
        .unwrap();
    let mut wrong = request.clone();
    if let AiAttachmentSource::QueryRows { schema_digest, .. } = &mut wrong.source {
        *schema_digest = "wrong".into();
    }
    assert!(client.preview_ai_attachment(chat.id, &wrong).await.is_err());
    if let AiAttachmentSource::QueryRows {
        schema_digest,
        row_ordinals,
        ..
    } = &mut wrong.source
    {
        *schema_digest = digest;
        *row_ordinals = vec![256];
    }
    assert!(client.preview_ai_attachment(chat.id, &wrong).await.is_err());
    let second_connection = client
        .open_connection_from_profile(
            session,
            OpenConnectionFromProfileRequest {
                tenant_id: 1,
                profile_id: profile.id.0,
            },
        )
        .await
        .unwrap()
        .id;
    wrong = request.clone();
    wrong.target.connection_id = Some(format!("{}:{}", session.0, second_connection.0));
    assert!(client.preview_ai_attachment(chat.id, &wrong).await.is_err());
    let blocked = reqwest::Client::new().put(format!("http://{addr}/v1/metadata/connections/{}/policy", profile.id.0))
        .json(&serde_json::json!({"expected_revision":0,"minimum_tenant_role":"member","read_only":false,"blocked_ops":["execute_query"]}))
        .send().await.unwrap();
    assert_eq!(blocked.status(), reqwest::StatusCode::OK);
    turn.client_request_id = uuid::Uuid::new_v4();
    assert!(
        client.start_ai_turn(chat.id, &turn).await.is_err(),
        "review does not bypass newly revoked reads"
    );
    assert!(client
        .preview_ai_attachment(chat.id, &request)
        .await
        .is_err());
    client.close_connection(session, connection).await.unwrap();
    turn.client_request_id = uuid::Uuid::new_v4();
    assert!(client.start_ai_turn(chat.id, &turn).await.is_err());
    assert!(client
        .preview_ai_attachment(chat.id, &request)
        .await
        .is_err());
    client.close_session(session).await.unwrap();
    server.abort();
}
