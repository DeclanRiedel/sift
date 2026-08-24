//! Opt-in end-to-end plan coverage against the demo Postgres catalog.
//!
//! ```text
//! SIFT_PG_HOST=/tmp/sift-pg-socket SIFT_PG_PORT=5433 \
//!   cargo test -p sift-server --features live-pg --test plan_capture_live
//! ```

#![cfg(feature = "live-pg")]

use sift_driver_postgres::PgDriver;
use sift_protocol::{ConnectionSpec, Engine, ExplainRequest, OpenSessionRequest, SslMode, Value};
use sift_server::{plan, DriverRegistry, SessionStore};

fn spec() -> ConnectionSpec {
    ConnectionSpec {
        host: std::env::var("SIFT_PG_HOST")
            .unwrap_or_else(|_| "/tmp/opencode/sift-pg-socket".into()),
        port: Some(
            std::env::var("SIFT_PG_PORT")
                .ok()
                .and_then(|port| port.parse().ok())
                .unwrap_or(5433),
        ),
        database: Some(std::env::var("SIFT_PG_DB").unwrap_or_else(|_| "sifttest".into())),
        user: std::env::var("SIFT_PG_USER").unwrap_or_else(|_| "sift".into()),
        password: std::env::var("SIFT_PG_PASSWORD").ok(),
        ssl_mode: Some(SslMode::Disable),
        engine_specific: None,
    }
}

#[tokio::test]
async fn demo_audit_events_parameterized_plan_is_normalized() {
    let store = SessionStore::new(DriverRegistry::builder().register(PgDriver::new()).build());
    let session = store.open_session(OpenSessionRequest {
        tag: Some("plan-capture-live".into()),
        tenant_id: None,
    });
    let connection = store
        .open_connection(session.id, Engine::Postgres, spec())
        .await
        .expect("demo Postgres opens");

    let response = plan::explain(
        &store,
        session.id,
        connection.id,
        &ExplainRequest {
            connection: connection.id,
            sql: "select * from audit.events limit $1".into(),
            params: vec![Value::Int64(5)],
            analyze: false,
        },
    )
    .await
    .expect("audit.events estimated plan succeeds");

    assert_eq!(response.engine, Engine::Postgres);
    assert!(!response.analyzed);
    assert!(!response.root.op.is_empty());
    assert!(response.raw.contains("Plan"));
}
