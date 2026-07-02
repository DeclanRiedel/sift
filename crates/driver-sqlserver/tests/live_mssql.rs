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
    ConnectionSpec, Engine, EngineConnectionSpec, ExecuteRequest, MssqlConnectionSpec, Page,
    PrimitiveType, SslMode, TypeRef, Value,
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
            Page::Rows(rows) => Some(rows),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(rows.len(), 1);
    assert!(matches!(rows[0].values[0], Value::Int32(7)));
    assert!(matches!(&rows[0].values[1], Value::Text(v) if v == "seven"));

    driver.close(conn).await.expect("close succeeds");
}
