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
    // Parquet stays typed through the real SQLite driver and atomic importer.
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
    rusqlite::Connection::open(path)
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
    server.abort();
}
