//! Per-request timeout + spawn discipline (Phase B reliability step 1).
//!
//! These drive `SessionStore` against a `MockDriver` programmed to wedge, and
//! assert that a slow/wedged driver call surfaces `Code::QueryTimedOut` within
//! the configured deadline instead of freezing the caller — and that the
//! post-timeout cancel keeps SQL Server's discard-on-cancel rule intact.

use std::time::{Duration, Instant};

use sift_driver_api::mock::MockDriver;
use sift_protocol::{
    Code, ConnectionSpec, Engine, ExecuteRequestHttp, OpenSessionRequest, ServerInfo, SslMode,
};
use sift_server::error::ApiError;
use sift_server::registry::DriverRegistry;
use sift_server::session::SessionStore;

const TIMEOUT: Duration = Duration::from_millis(150);

fn store_with(driver: MockDriver) -> SessionStore {
    let registry = DriverRegistry::builder().register(driver).build();
    let store = SessionStore::new(registry);
    store.set_request_timeout(TIMEOUT);
    store
}

// The mock ignores the spec entirely; the engine is carried separately into
// `open_connection`.
fn mock_spec() -> ConnectionSpec {
    ConnectionSpec {
        host: "mock.invalid".into(),
        port: None,
        database: Some("mock".into()),
        user: "mock".into(),
        password: None,
        ssl_mode: Some(SslMode::Disable),
        engine_specific: None,
    }
}

fn execute_req(connection: sift_protocol::ConnectionId, sql: &str) -> ExecuteRequestHttp {
    ExecuteRequestHttp {
        connection,
        sql: sql.into(),
        params: Vec::new(),
        tx: None,
        room_id: None,
        connection_profile_id: None,
    }
}

fn assert_timed_out(result: Result<impl std::fmt::Debug, ApiError>) {
    match result {
        Err(ApiError::Driver(error)) => assert_eq!(
            error.code,
            Code::QueryTimedOut,
            "expected QueryTimedOut, got {error:?}"
        ),
        other => panic!("expected QueryTimedOut driver error, got {other:?}"),
    }
}

#[tokio::test]
async fn wedged_execute_times_out_within_deadline() {
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .execute_pending()
        .build();
    let store = store_with(driver);
    let session = store.open_session(OpenSessionRequest { tag: None });
    let conn = store
        .open_connection(session.id, Engine::Postgres, mock_spec())
        .await
        .expect("open connection");

    let started = Instant::now();
    let result = store
        .execute_http(session.id, execute_req(conn.id, "select 1"))
        .await;
    let elapsed = started.elapsed();

    assert_timed_out(result);
    assert!(
        elapsed < TIMEOUT * 8,
        "handler should return near the deadline, took {elapsed:?}"
    );
}

#[tokio::test]
async fn wedged_schema_times_out() {
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .schema_pending()
        .build();
    let store = store_with(driver);
    let session = store.open_session(OpenSessionRequest { tag: None });
    let conn = store
        .open_connection(session.id, Engine::Postgres, mock_spec())
        .await
        .expect("open connection");

    let result = store
        .schema(session.id, conn.id, sift_protocol::SchemaScope::shallow())
        .await;
    assert_timed_out(result);
}

#[tokio::test]
async fn sqlserver_execute_timeout_discards_connection_after_cancel() {
    // `execute_hang` returns a live cursor then never yields pages, so the
    // server learns the cursor id and cancels it on timeout. SQL Server's
    // rule is to drop the connection after an abort — assert it is gone.
    let driver = MockDriver::builder()
        .engine(Engine::SqlServer)
        .execute_hang()
        .build();
    let store = store_with(driver);
    let session = store.open_session(OpenSessionRequest { tag: None });
    let conn = store
        .open_connection(session.id, Engine::SqlServer, mock_spec())
        .await
        .expect("open connection");

    let result = store
        .execute_http(session.id, execute_req(conn.id, "select 1"))
        .await;
    assert_timed_out(result);

    let connections = store
        .list_connections(session.id)
        .expect("list connections");
    assert!(
        connections.is_empty(),
        "sql server connection must be discarded after cancel-on-timeout, found {connections:?}"
    );
}

#[tokio::test]
async fn wedged_execute_does_not_block_other_work() {
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .execute_hang()
        .ping_ok(ServerInfo {
            engine: Engine::Postgres,
            server_version: "MockDB 0.1".into(),
            current_database: "mock".into(),
            current_user: "mock".into(),
            pool_warm_slots: None,
        })
        .cancel_ok()
        .build();
    let store = store_with(driver);
    let session = store.open_session(OpenSessionRequest { tag: None });
    let conn = store
        .open_connection(session.id, Engine::Postgres, mock_spec())
        .await
        .expect("open connection");

    // Run a wedged execute concurrently with a healthy ping. The ping must
    // complete (proving the handler task is not blocked by the wedged one)
    // while the execute times out.
    let execute = store.execute_http(session.id, execute_req(conn.id, "select 1"));
    let ping = store.ping(session.id, conn.id);
    let (execute_result, ping_result) = tokio::join!(execute, ping);

    assert_timed_out(execute_result);
    assert!(
        ping_result.is_ok(),
        "ping must succeed while execute is wedged, got {ping_result:?}"
    );
}
