//! Native SQL Server tests require live-mssql and a disposable database.
//! Setup: repository docs/DEVELOPMENT.md; connection variables: .env.example.

#![cfg(feature = "live-mssql")]

use sift_client_sdk::Client;
use sift_driver_api::{Driver, MssqlExt};
use sift_driver_sqlserver::MssqlDriver;
use sift_protocol::{
    CatalogGraphOptions, CatalogNodeKind, ConnectionSpec, Engine, EngineConnectionSpec,
    ExecuteRequest, MssqlConnectionSpec, ObjectPath, Page, SchemaDepth, SchemaScope, SslMode,
    Value,
};
use sift_server::{
    http::{app, AppState, AuthState},
    DriverRegistry, SessionStore,
};

fn spec() -> ConnectionSpec {
    ConnectionSpec {
        host: std::env::var("SIFT_MSSQL_HOST").unwrap_or_else(|_| "127.0.0.1".into()),
        port: Some(
            std::env::var("SIFT_MSSQL_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(1433),
        ),
        database: Some(std::env::var("SIFT_MSSQL_DB").unwrap_or_else(|_| "master".into())),
        user: std::env::var("SIFT_MSSQL_USER").unwrap_or_else(|_| "sa".into()),
        password: Some(std::env::var("SIFT_MSSQL_PASSWORD").expect("SIFT_MSSQL_PASSWORD required")),
        ssl_mode: Some(SslMode::Require),
        engine_specific: Some(EngineConnectionSpec::SqlServer(MssqlConnectionSpec {
            mars: false,
            encrypt: Some(true),
            trust_server_certificate: Some(true),
            connect_timeout_secs: Some(15),
            pool_min_size: None,
            ..Default::default()
        })),
    }
}

async fn drain(mut stream: sift_driver_api::ResultSetStream) -> Vec<Page> {
    let mut pages = Vec::new();
    while let Some(page) = stream.rows.recv().await {
        pages.push(page);
    }
    pages
}

// Requires a whole disposable server: this fixture creates databases and a login.
#[tokio::test]
#[ignore = "requires SIFT_MSSQL_MULTIDB_DISPOSABLE=1 and database/login creation rights"]
async fn multi_database_metadata_preserves_catalog_and_session() {
    use futures::FutureExt;
    assert_eq!(
        std::env::var("SIFT_MSSQL_MULTIDB_DISPOSABLE").as_deref(),
        Ok("1")
    );
    let driver = MssqlDriver::new();
    let mut admin_spec = spec();
    admin_spec.database = Some("master".into());
    let admin = driver.open(&admin_spec).await.unwrap();
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let databases = [
        format!("sift browse {suffix} a"),
        format!("sift browse {suffix} ] b"),
        format!("sift browse {suffix} offline"),
        format!("sift browse {suffix} denied"),
    ];
    let login = format!("sift_browse_{suffix}");
    let password = format!("aA9!{}", uuid::Uuid::new_v4().simple());
    let quote = |value: &str| format!("[{}]", value.replace(']', "]]"));
    async fn fixture_sql(
        driver: &MssqlDriver,
        connection: &sift_driver_api::ConnHandle,
        sql: String,
    ) {
        // Setup SQL may contain generated credentials; never print SQL/error text.
        let stream = driver
            .execute(connection.clone(), ExecuteRequest::new(sql))
            .await
            .unwrap_or_else(|error| panic!("fixture failed: {:?}", error.native_code));
        let pages = drain(stream).await;
        for page in &pages {
            if let Page::Error { error } = page {
                panic!("fixture failed: {:?}", error.native_code);
            }
        }
        assert!(pages.iter().any(|page| matches!(page, Page::Done { .. })));
    }
    let sessions = SessionStore::new(
        DriverRegistry::builder()
            .register(MssqlDriver::new())
            .build(),
    );
    // Keep fixture cleanup independent of the background polling lifetime.
    let mut cache_config = sessions.schema_cache().config();
    cache_config.max_invalidators = 0;
    sessions.schema_cache().set_config(cache_config);
    let router = app(AppState {
        sessions,
        rooms: Default::default(),
        shutdown: Default::default(),
        auth: AuthState::default(),
        metadata: None,
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let client = Client::new(base_url.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let mut http_session = None;
    let outcome = std::panic::AssertUnwindSafe(async {
        for database in &databases {
            fixture_sql(&driver, &admin, format!("CREATE DATABASE {}", quote(database))).await;
        }
        fixture_sql(&driver, &admin, format!("CREATE LOGIN {} WITH PASSWORD=N'{password}', CHECK_POLICY=OFF, DEFAULT_DATABASE=master", quote(&login))).await;
        for (index, database) in databases[..2].iter().enumerate() {
            let mut target = admin_spec.clone();
            target.database = Some(database.clone());
            let setup = driver.open(&target).await.unwrap();
            fixture_sql(&driver, &setup, format!("CREATE TABLE dbo.shared (id int PRIMARY KEY, only_{} nvarchar(20)); INSERT dbo.shared VALUES ({}, N'value'); CREATE USER {} FOR LOGIN {}; GRANT SELECT, UPDATE, VIEW DEFINITION TO {}",
                if index == 0 { "a" } else { "b" }, index + 1, quote(&login), quote(&login), quote(&login))).await;
            fixture_sql(&driver, &setup, "CREATE TRIGGER dbo.shared_trigger ON dbo.shared AFTER INSERT AS BEGIN SET NOCOUNT ON; END".into()).await;
            driver.close(setup).await.unwrap();
        }
        fixture_sql(&driver, &admin, format!("ALTER DATABASE {} SET OFFLINE", quote(&databases[2]))).await;
        let mut reader = admin_spec.clone();
        reader.user = login.clone();
        reader.password = Some(password);
        let connection = driver.open(&reader).await.unwrap();
        let checks = tokio::time::timeout(std::time::Duration::from_secs(90), std::panic::AssertUnwindSafe(async {
            let session = client.open_session(Some("disposable multi-database browsing".into())).await.unwrap().id;
            http_session = Some(session);
            let http_connection = client.open_connection(session, sift_protocol::OpenConnectionRequest {
                provider_id: Engine::SqlServer.provider_id(), spec: reader.clone(),
            }).await.unwrap().id;
            let visible = client.schema(session, http_connection).await.unwrap();
            for database in &databases[..2] {
                assert!(visible.trees.iter().any(|tree| &tree.name == database));
            }
            let snapshot = driver.schema(connection.clone(), SchemaScope::shallow()).await.unwrap();
            assert_eq!(snapshot.trees[0].name, "master");
            for (index, database) in databases[..2].iter().enumerate() {
                let tree = snapshot.trees.iter().find(|tree| &tree.name == database)
                    .expect("accessible database must be discovered");
                assert!(tree.schemas.iter().any(|schema| schema.name == "dbo"
                    && schema.objects.iter().any(|object| object.name == "shared")));
                let deep = driver.schema(connection.clone(), SchemaScope::deep(ObjectPath {
                    catalog: Some(database.clone()), schema: Some("dbo".into()),
                    name: "shared".into(), kind: Some(sift_protocol::ObjectKind::Table), routine_args: None,
                })).await.unwrap();
                assert_eq!(&deep.trees[0].name, database);
                let object = &deep.trees[0].schemas[0].objects[0];
                let expected = if index == 0 { "only_a" } else { "only_b" };
                assert!(object.columns.iter().any(|column| column.name == expected));
                assert!(!object.indexes.is_empty());
                assert!(!object.constraints.is_empty());
                assert!(!object.triggers.is_empty());
                let pages = drain(driver.execute(connection.clone(), ExecuteRequest::new(format!("SELECT TOP (100) * FROM {}.[dbo].[shared]", quote(database)))).await.unwrap()).await;
                assert!(pages.iter().any(|page| matches!(page, Page::Rows { rows } if rows[0].values[0] == Value::Int32((index + 1) as i32))));
            }
            for (index, database) in databases[..2].iter().enumerate() {
                let path = ObjectPath { catalog: Some(database.clone()), schema: Some("dbo".into()), name: "shared".into(), kind: Some(sift_protocol::ObjectKind::Table), routine_args: None };
                let column = if index == 0 { "only_a" } else { "only_b" };
                let deep: sift_protocol::SchemaSnapshot = reqwest::Client::new()
                    .get(format!("{base_url}/v1/sessions/{session}/connections/{http_connection}/schema"))
                    .query(&[("depth", "deep"), ("catalog", database.as_str()), ("schema", "dbo"), ("object", "shared")])
                    .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
                assert_eq!(deep.trees[0].name, *database);
                assert!(deep.trees[0].schemas[0].objects[0].columns.iter().any(|entry| entry.name == column));
                let ddl = client.object_ddl(session, http_connection, &path).await.unwrap();
                assert!(ddl.ddl.contains(column));
                let edit_set = sift_protocol::EditSet { table: path, edits: vec![sift_protocol::RowEdit::Update {
                    key: sift_protocol::RowKey { columns: vec![sift_protocol::CellEdit { column: "id".into(), value: Value::Int32((index + 1) as i32) }] },
                    changes: vec![sift_protocol::CellEdit { column: column.into(), value: Value::Text(format!("edited {index}")) }], expected: vec![],
                }] };
                let plan = client.preview_edits(session, http_connection, sift_protocol::PreviewEditsRequest { connection: http_connection, edit_set: edit_set.clone() }).await.unwrap();
                assert!(plan.statements[0].sql.starts_with(&format!("UPDATE {}.[dbo].[shared]", quote(database))));
                let applied = client.apply_edits(session, http_connection, sift_protocol::ApplyEditsRequest { connection: http_connection, edit_set, tx: None }).await.unwrap();
                assert!(applied.committed);
                let preview = sift_snippets::table_preview_sql_in_catalog(&Engine::SqlServer.provider_id(), Some(database), "dbo", "shared").unwrap();
                let rows = client.execute(session, http_connection, preview).await.unwrap().rows;
                assert_eq!(rows[0].values[1], Value::Text(format!("edited {index}")));
            }
            assert_eq!(client.execute(session, http_connection, "SELECT DB_NAME()").await.unwrap().rows[0].values[0], Value::Text("master".into()));
            assert!(!snapshot.trees.iter().any(|tree| tree.name == databases[2] || tree.name == databases[3]));
            let filtered = driver.schema(connection.clone(), SchemaScope {
                depth: SchemaDepth::Shallow,
                filter: Some(sift_protocol::SchemaFilter { catalogs: Some(vec![databases[1].clone()]), ..Default::default() }),
            }).await.unwrap();
            assert_eq!(filtered.trees.len(), 1);
            assert_eq!(filtered.trees[0].name, databases[1]);
            assert!(driver.schema(connection.clone(), SchemaScope::deep(ObjectPath {
                catalog: Some(databases[3].clone()), schema: Some("dbo".into()), ..ObjectPath::new("shared")
            })).await.is_err());
            assert_eq!(driver.ping(connection.clone()).await.unwrap().current_database, "master");
            driver.use_database(connection.clone(), &databases[1]).await.unwrap();
            let selected = driver.schema(connection.clone(), SchemaScope::shallow()).await.unwrap();
            assert_eq!(selected.trees[0].name, databases[1]);
            let deep = driver.schema(connection.clone(), SchemaScope::deep(ObjectPath {
                schema: Some("dbo".into()), ..ObjectPath::new("shared")
            })).await.unwrap();
            assert_eq!(deep.trees[0].name, databases[1]);
            let graph = driver.schema(connection.clone(), SchemaScope {
                depth: SchemaDepth::Graph { options: CatalogGraphOptions::default() }, filter: None,
            }).await.unwrap();
            assert_eq!(graph.trees.len(), 1);
            assert_eq!(graph.trees[0].name, databases[1]);
            assert!(graph.graph.unwrap().nodes.iter().any(|node| node.kind == CatalogNodeKind::Column && node.name == "only_b"));
            assert_eq!(driver.ping(connection.clone()).await.unwrap().current_database, databases[1]);
        }).catch_unwind()).await;
        driver.close(connection).await.unwrap();
        match checks {
            Ok(Err(panic)) => std::panic::resume_unwind(panic),
            Err(_) => panic!("multi-database checks exceeded deadline"),
            Ok(Ok(())) => {},
        }
    }).catch_unwind().await;
    if let Some(session) = http_session {
        client.close_session(session).await.unwrap();
    }
    server.abort();
    let _ = server.await;
    // Only exact database/login names generated by this test are cleanup targets.
    for database in &databases {
        fixture_sql(
            &driver,
            &admin,
            format!("DROP DATABASE IF EXISTS {}", quote(database)),
        )
        .await;
    }
    fixture_sql(
        &driver,
        &admin,
        format!(
            "IF SUSER_ID(N'{login}') IS NOT NULL DROP LOGIN {}",
            quote(&login)
        ),
    )
    .await;
    driver.close(admin).await.unwrap();
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
