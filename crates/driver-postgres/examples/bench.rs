//! Ad-hoc perf measurement against a live PG. Gated behind `live-pg`.
//!
//! Run: `SIFT_PG_HOST=/tmp/sift-pg-socket SIFT_PG_PORT=5433 cargo run
//! -p sift-driver-postgres --release --features live-pg --example bench`.

#![cfg(feature = "live-pg")]

use std::time::{Duration, Instant};

use sift_driver_api::Driver;
use sift_driver_postgres::PgDriver;
use sift_protocol::{ConnectionSpec, ExecuteRequest, SchemaScope, SslMode};

const DEFAULT_PG_HOST: &str = "/tmp/opencode/sift-pg-socket";
const DEFAULT_PG_PORT: u16 = 5433;
const DEFAULT_PG_USER: &str = "sift";
const DEFAULT_PG_DB: &str = "sifttest";

fn spec() -> ConnectionSpec {
    ConnectionSpec {
        host: std::env::var("SIFT_PG_HOST").unwrap_or_else(|_| DEFAULT_PG_HOST.to_string()),
        port: Some(
            std::env::var("SIFT_PG_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(DEFAULT_PG_PORT),
        ),
        database: Some(std::env::var("SIFT_PG_DB").unwrap_or_else(|_| DEFAULT_PG_DB.to_string())),
        user: std::env::var("SIFT_PG_USER").unwrap_or_else(|_| DEFAULT_PG_USER.to_string()),
        password: std::env::var("SIFT_PG_PASSWORD").ok(),
        ssl_mode: Some(SslMode::Disable),
        engine_specific: None,
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

async fn time_n<F, Fut, T>(label: &str, n: usize, mut f: F) -> Vec<Duration>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = T>,
{
    // Warm-up.
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
    println!("sift-driver-postgres perf bench");
    println!();
    let driver = PgDriver::new();

    // open() includes pool build (cold) on first call; subsequent opens of
    // the same spec hit the cache.
    let t0 = Instant::now();
    let conn = driver.open(&spec()).await.expect("open");
    println!(
        "  open (cold, builds pool):                {:?}",
        t0.elapsed()
    );

    let t0 = Instant::now();
    let conn2 = driver.open(&spec()).await.expect("open cached");
    println!(
        "  open (warm, cached pool):                {:?}",
        t0.elapsed()
    );
    driver.close(conn2).await.unwrap();

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

    let _ = time_n("execute \"SELECT * FROM bench LIMIT 1\"", 200, || async {
        let s = driver
            .execute(
                conn.clone(),
                ExecuteRequest::new("SELECT * FROM bench LIMIT 1"),
            )
            .await
            .unwrap();
        drain(s).await
    })
    .await;

    let _ = time_n("execute \"SELECT * FROM bench LIMIT 100\"", 200, || async {
        let s = driver
            .execute(
                conn.clone(),
                ExecuteRequest::new("SELECT * FROM bench LIMIT 100"),
            )
            .await
            .unwrap();
        drain(s).await
    })
    .await;

    let _ = time_n(
        "execute DML \"UPDATE bench SET payload=payload\"",
        200,
        || async {
            let s = driver
                .execute(
                    conn.clone(),
                    ExecuteRequest::new("UPDATE bench SET payload=payload"),
                )
                .await
                .unwrap();
            drain(s).await
        },
    )
    .await;

    let _ = time_n("schema (Shallow)", 50, || async {
        driver.schema(conn.clone(), SchemaScope::shallow()).await
    })
    .await;

    let deep = SchemaScope::deep(sift_protocol::ObjectPath {
        catalog: None,
        schema: Some("public".into()),
        name: "bench".into(),
        kind: Some(sift_protocol::ObjectKind::Table),
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
        .execute(conn.clone(), ExecuteRequest::new("SELECT * FROM bench"))
        .await
        .unwrap();
    let rows = drain(s).await;
    let elapsed = t0.elapsed();
    println!(
        "  {rows} rows in {elapsed:?}  =  {:.0} rows/sec",
        rows as f64 / elapsed.as_secs_f64()
    );

    println!();
    println!("throughput: 100× ping burst (sequential)");
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
