//! Ad-hoc perf measurement against a live SQL Server. Gated behind
//! `live-mssql`.
//!
//! Run: `SIFT_MSSQL_PASSWORD=... cargo run -p sift-driver-sqlserver
//! --release --features live-mssql --example bench`.

#![cfg(feature = "live-mssql")]

use std::time::{Duration, Instant};

use sift_driver_api::Driver;
use sift_driver_sqlserver::MssqlDriver;
use sift_protocol::{
    ConnectionSpec, EngineConnectionSpec, ExecuteRequest, MssqlConnectionSpec, ObjectKind,
    ObjectPath, SchemaScope, SslMode,
};

const DEFAULT_MSSQL_HOST: &str = "127.0.0.1";
const DEFAULT_MSSQL_PORT: u16 = 1433;
const DEFAULT_MSSQL_USER: &str = "sa";
const DEFAULT_MSSQL_DB: &str = "master";

fn spec() -> ConnectionSpec {
    ConnectionSpec {
        host: std::env::var("SIFT_MSSQL_HOST").unwrap_or_else(|_| DEFAULT_MSSQL_HOST.to_string()),
        port: Some(
            std::env::var("SIFT_MSSQL_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(DEFAULT_MSSQL_PORT),
        ),
        database: Some(
            std::env::var("SIFT_MSSQL_DB").unwrap_or_else(|_| DEFAULT_MSSQL_DB.to_string()),
        ),
        user: std::env::var("SIFT_MSSQL_USER").unwrap_or_else(|_| DEFAULT_MSSQL_USER.to_string()),
        password: Some(std::env::var("SIFT_MSSQL_PASSWORD").expect("SIFT_MSSQL_PASSWORD required")),
        ssl_mode: Some(SslMode::Require),
        engine_specific: Some(EngineConnectionSpec::SqlServer(MssqlConnectionSpec {
            mars: false,
            encrypt: Some(true),
            trust_server_certificate: Some(true),
            connect_timeout_secs: Some(15),
            pool_min_size: None,
        })),
    }
}

async fn drain(stream: sift_driver_api::ResultSetStream) -> u64 {
    let mut rows = 0u64;
    let mut rx = stream.rows;
    while let Some(page) = rx.recv().await {
        if let sift_protocol::Page::Rows { rows: r } = page {
            rows += r.len() as u64;
        }
    }
    rows
}

async fn run(driver: &MssqlDriver, conn: sift_driver_api::ConnHandle, sql: impl Into<String>) {
    let stream = driver
        .execute(conn, ExecuteRequest::new(sql))
        .await
        .expect("execute setup query");
    drain(stream).await;
}

async fn time_n<F, Fut, T>(label: &str, n: usize, mut f: F) -> Vec<Duration>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = T>,
{
    let _ = f().await;
    let mut samples = Vec::with_capacity(n);
    for _ in 0..n {
        let t0 = Instant::now();
        let _ = f().await;
        samples.push(t0.elapsed());
    }
    samples.sort();
    let p50 = samples[samples.len() / 2];
    let p95 = samples[(samples.len() * 95) / 100];
    let p99 = samples[(samples.len() * 99) / 100];
    println!("  {label:42}  p50={p50:?}  p95={p95:?}  p99={p99:?}");
    samples
}

#[tokio::main]
async fn main() {
    println!("sift-driver-sqlserver perf bench");
    println!();
    let driver = MssqlDriver::new();

    let t0 = Instant::now();
    let conn = driver.open(&spec()).await.expect("open");
    println!(
        "  open:                                  {:?}",
        t0.elapsed()
    );

    run(
        &driver,
        conn.clone(),
        "IF OBJECT_ID(N'dbo.sift_bench', N'U') IS NULL \
         CREATE TABLE dbo.sift_bench (\
             id bigint IDENTITY(1,1) NOT NULL PRIMARY KEY, \
             payload nvarchar(64) NOT NULL, \
             created_at datetime2 NOT NULL DEFAULT SYSUTCDATETIME()\
         )",
    )
    .await;
    run(&driver, conn.clone(), "TRUNCATE TABLE dbo.sift_bench").await;
    run(
        &driver,
        conn.clone(),
        "INSERT INTO dbo.sift_bench (payload) \
         SELECT CONVERT(nvarchar(64), n) \
         FROM (\
             SELECT TOP (10000) ROW_NUMBER() OVER (ORDER BY (SELECT NULL)) AS n \
             FROM sys.all_objects a CROSS JOIN sys.all_objects b\
         ) AS src",
    )
    .await;

    println!();
    println!("per-call latency, 200 samples each:");
    let _ = time_n("ping", 200, || async { driver.ping(conn.clone()).await }).await;

    let _ = time_n("execute \"SELECT 1\"", 200, || async {
        let s = driver
            .execute(conn.clone(), ExecuteRequest::new("SELECT 1"))
            .await
            .unwrap();
        drain(s).await
    })
    .await;

    let _ = time_n("execute \"SELECT TOP (1) *\"", 200, || async {
        let s = driver
            .execute(
                conn.clone(),
                ExecuteRequest::new("SELECT TOP (1) * FROM dbo.sift_bench"),
            )
            .await
            .unwrap();
        drain(s).await
    })
    .await;

    let _ = time_n("execute \"SELECT TOP (100) *\"", 200, || async {
        let s = driver
            .execute(
                conn.clone(),
                ExecuteRequest::new("SELECT TOP (100) * FROM dbo.sift_bench"),
            )
            .await
            .unwrap();
        drain(s).await
    })
    .await;

    let _ = time_n("execute DML \"UPDATE sift_bench\"", 200, || async {
        let s = driver
            .execute(
                conn.clone(),
                ExecuteRequest::new("UPDATE dbo.sift_bench SET payload = payload"),
            )
            .await
            .unwrap();
        drain(s).await
    })
    .await;

    let _ = time_n("schema (Shallow)", 50, || async {
        driver.schema(conn.clone(), SchemaScope::shallow()).await
    })
    .await;

    let deep = SchemaScope::deep(ObjectPath {
        catalog: None,
        schema: Some("dbo".into()),
        name: "sift_bench".into(),
        kind: Some(ObjectKind::Table),
        routine_args: None,
    });
    let _ = time_n("schema (Deep)", 50, || async {
        driver.schema(conn.clone(), deep.clone()).await
    })
    .await;

    println!();
    println!("throughput: 10k-row full table scan");
    let t0 = Instant::now();
    let s = driver
        .execute(
            conn.clone(),
            ExecuteRequest::new("SELECT * FROM dbo.sift_bench"),
        )
        .await
        .unwrap();
    let rows = drain(s).await;
    let elapsed = t0.elapsed();
    println!(
        "  {rows} rows in {elapsed:?}  =  {:.0} rows/sec",
        rows as f64 / elapsed.as_secs_f64()
    );

    println!();
    println!("throughput: 100x ping burst (sequential)");
    let t0 = Instant::now();
    for _ in 0..100 {
        let _ = driver.ping(conn.clone()).await.unwrap();
    }
    let elapsed = t0.elapsed();
    println!(
        "  100 pings in {elapsed:?}  =  {:.0} pings/sec",
        100.0 / elapsed.as_secs_f64()
    );

    driver.close(conn).await.unwrap();
}
