//! Connection recovery behavior (Phase B reliability step 6).
//!
//! Idempotent operations (ping, schema) transparently re-establish a broken
//! connection and retry once; the retry boundary stops there — a persistent
//! failure surfaces `Code::ConnectionFailed`, and mutating operations are
//! never auto-retried.

use sift_driver_api::mock::MockDriver;
use sift_protocol::{
    Code, ConnectionSpec, DriverError, Engine, OpenSessionRequest, SchemaScope, SchemaSnapshot,
    ServerInfo, SslMode,
};
use sift_server::registry::DriverRegistry;
use sift_server::session::SessionStore;

fn server_info() -> ServerInfo {
    ServerInfo {
        engine: Engine::Postgres,
        server_version: "MockDB 0.1".into(),
        current_database: "mock".into(),
        current_user: "mock".into(),
    }
}

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

async fn open(store: &SessionStore) -> (sift_protocol::SessionId, sift_protocol::ConnectionId) {
    let session = store.open_session(OpenSessionRequest { tag: None });
    let conn = store
        .open_connection(session.id, Engine::Postgres, mock_spec())
        .await
        .expect("open connection");
    (session.id, conn.id)
}

fn store(driver: MockDriver) -> SessionStore {
    SessionStore::new(DriverRegistry::builder().register(driver).build())
}

#[tokio::test]
async fn ping_recovers_from_a_broken_connection() {
    // First ping fails as if the backend dropped the socket; after a
    // transparent reconnect the retry succeeds.
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .ping_err(DriverError::new(Code::ConnectionFailed, "connection reset"))
        .ping_ok(server_info())
        .build();
    let store = store(driver);
    let (session, conn) = open(&store).await;

    let info = store.ping(session, conn).await.expect("ping recovers");
    assert_eq!(info.current_database, "mock");
}

#[tokio::test]
async fn schema_recovers_from_a_broken_connection() {
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .schema_err(DriverError::new(Code::ConnectionFailed, "connection reset"))
        .schema_ok(SchemaSnapshot::empty(SchemaScope::shallow()))
        .build();
    let store = store(driver);
    let (session, conn) = open(&store).await;

    store
        .schema(session, conn, SchemaScope::shallow())
        .await
        .expect("schema recovers");
}

#[tokio::test]
async fn persistent_failure_surfaces_after_one_retry() {
    // Both attempts fail: reconnect happens, retry still fails, and the error
    // is surfaced rather than retried forever.
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .ping_err(DriverError::new(Code::ConnectionFailed, "down"))
        .ping_err(DriverError::new(Code::ConnectionFailed, "still down"))
        .build();
    let store = store(driver);
    let (session, conn) = open(&store).await;

    let error = store.ping(session, conn).await.unwrap_err();
    match error {
        sift_server::ApiError::Driver(e) => assert_eq!(e.code, Code::ConnectionFailed),
        other => panic!("expected ConnectionFailed, got {other:?}"),
    }
}

#[tokio::test]
async fn non_reconnectable_error_is_not_retried() {
    // A syntax error is not a broken connection: no reconnect, no retry, and
    // the second (would-be-retry) ping result is never consumed.
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .ping_err(DriverError::new(Code::SyntaxError, "nope"))
        .build();
    let store = store(driver);
    let (session, conn) = open(&store).await;

    let error = store.ping(session, conn).await.unwrap_err();
    match error {
        sift_server::ApiError::Driver(e) => assert_eq!(e.code, Code::SyntaxError),
        other => panic!("expected SyntaxError, got {other:?}"),
    }
}
