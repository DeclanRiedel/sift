//! Integration tests against a real SQL Server instance. Gated behind the
//! `live-mssql` feature so CI runs without Docker/SQL Server by default.
//!
//! Required env:
//! - `SIFT_MSSQL_HOST` (default `127.0.0.1`)
//! - `SIFT_MSSQL_PORT` (default `1433`)
//! - `SIFT_MSSQL_USER` (default `sa`)
//! - `SIFT_MSSQL_PASSWORD` (required)
//! - `SIFT_MSSQL_DB` (default `master`)

#![cfg(feature = "live-mssql")]

use sift_driver_api::Driver;
use sift_driver_sqlserver::MssqlDriver;
use sift_protocol::{
    ConnectionSpec, Engine, EngineConnectionSpec, ExecuteRequest, MssqlConnectionSpec, ObjectPath,
    Page, PrimitiveType, SchemaScope, SslMode, TxMode, TypeRef, Value,
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

#[tokio::test]
async fn open_ping_execute_close() {
    let driver = MssqlDriver::new();
    let conn = driver.open(&spec()).await.expect("open succeeds");
    let info = driver.ping(conn.clone()).await.expect("ping succeeds");
    assert_eq!(info.engine, Engine::SqlServer);

    let pages = drain(
        driver
            .execute(
                conn.clone(),
                ExecuteRequest {
                    sql: "SELECT CAST(@P1 AS int) AS id, CAST(@P2 AS nvarchar(20)) AS name".into(),
                    params: vec![Value::Int32(7), Value::Text("seven".into())],
                },
            )
            .await
            .expect("execute succeeds"),
    )
    .await;

    let cols = pages
        .iter()
        .find_map(|p| match p {
            Page::NextResult { columns } => Some(columns),
            _ => None,
        })
        .expect("columns sent");
    assert_eq!(cols.len(), 2);
    assert!(matches!(
        cols[0].type_ref,
        TypeRef::Primitive(PrimitiveType::Int32)
    ));

    let rows: Vec<_> = pages
        .iter()
        .filter_map(|p| match p {
            Page::Rows { rows } => Some(rows),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(rows.len(), 1);
    assert!(matches!(rows[0].values[0], Value::Int32(7)));
    assert!(matches!(&rows[0].values[1], Value::Text(v) if v == "seven"));

    driver.close(conn).await.expect("close succeeds");
}

#[tokio::test]
async fn cancel_long_query() {
    let driver = MssqlDriver::new();
    let conn = driver.open(&spec()).await.expect("open succeeds");
    let stream = driver
        .execute(
            conn.clone(),
            ExecuteRequest {
                sql: "WAITFOR DELAY '00:00:05'; SELECT 1 AS done".into(),
                params: Vec::new(),
            },
        )
        .await
        .expect("execute starts");

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    driver
        .cancel(conn.clone(), stream.cursor_id)
        .await
        .expect("cancel succeeds");
    driver.close(conn).await.expect("close is idempotent");
}

#[tokio::test]
async fn close_mid_query_drops_cursor_and_connection() {
    let driver = MssqlDriver::new();
    let conn = driver.open(&spec()).await.expect("open succeeds");
    let _stream = driver
        .execute(
            conn.clone(),
            ExecuteRequest {
                sql: "WAITFOR DELAY '00:00:05'; SELECT 1 AS done".into(),
                params: Vec::new(),
            },
        )
        .await
        .expect("execute starts");

    driver.close(conn.clone()).await.expect("close succeeds");
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        driver.ping(conn).await.is_err(),
        "closed connection must not be resurrected by query task"
    );
}

#[tokio::test]
async fn schema_deep_and_transactions() {
    let driver = MssqlDriver::new();
    let conn = driver.open(&spec()).await.expect("open succeeds");
    let table = format!(
        "sift_phase0_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    );

    let setup =
        format!("CREATE TABLE dbo.[{table}] (id int NOT NULL PRIMARY KEY, name nvarchar(64) NULL)");
    drain(
        driver
            .execute(conn.clone(), ExecuteRequest::new(setup))
            .await
            .expect("create table"),
    )
    .await;

    let shallow = driver
        .schema(conn.clone(), SchemaScope::shallow())
        .await
        .expect("shallow schema");
    assert!(shallow
        .trees
        .iter()
        .flat_map(|tree| &tree.schemas)
        .flat_map(|schema| &schema.objects)
        .any(|object| object.name == table));

    let deep = driver
        .schema(
            conn.clone(),
            SchemaScope::deep(ObjectPath {
                catalog: None,
                schema: Some("dbo".into()),
                name: table.clone(),
                kind: None,
            }),
        )
        .await
        .expect("deep schema");
    let object = &deep.trees[0].schemas[0].objects[0];
    assert!(object
        .columns
        .iter()
        .any(|c| c.name == "id" && c.primary_key));
    assert!(object.indexes.iter().any(|idx| idx.primary_key));
    assert!(object
        .constraints
        .iter()
        .any(|constraint| constraint.columns.iter().any(|c| c == "id")));

    let tx = driver
        .begin(conn.clone(), TxMode::default())
        .await
        .expect("begin");
    drain(
        driver
            .execute(
                conn.clone(),
                ExecuteRequest::new(format!(
                    "INSERT INTO dbo.[{table}] (id, name) VALUES (1, N'a')"
                )),
            )
            .await
            .expect("insert in tx"),
    )
    .await;
    driver.rollback(tx).await.expect("rollback");

    let pages = drain(
        driver
            .execute(
                conn.clone(),
                ExecuteRequest::new(format!("SELECT COUNT(*) AS ct FROM dbo.[{table}]")),
            )
            .await
            .expect("count after rollback"),
    )
    .await;
    let count = pages.iter().find_map(|p| match p {
        Page::Rows { rows } => rows.first().and_then(|row| row.values.first()),
        _ => None,
    });
    assert!(matches!(count, Some(Value::Int32(0))));

    drain(
        driver
            .execute(
                conn.clone(),
                ExecuteRequest::new(format!("DROP TABLE dbo.[{table}]")),
            )
            .await
            .expect("drop table"),
    )
    .await;
    driver.close(conn).await.expect("close succeeds");
}
