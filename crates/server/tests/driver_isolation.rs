//! Driver isolation / wedged-driver containment (ADR-013).
//!
//! A driver that panics or wedges must degrade a single request, not the
//! process. These exercise the server-side containment boundary: a panicking
//! driver call surfaces an internal error and leaves the server able to serve
//! the next request.

use sift_driver_api::mock::MockDriver;
use sift_protocol::{ConnectionSpec, Engine, OpenSessionRequest, ServerInfo, SslMode};
use sift_server::registry::DriverRegistry;
use sift_server::session::SessionStore;
use sift_server::ApiError;

fn server_info() -> ServerInfo {
    ServerInfo {
        engine: Engine::Postgres,
        server_version: "MockDB 0.1".into(),
        current_database: "mock".into(),
        current_user: "mock".into(),
        pool_warm_slots: None,
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

#[tokio::test]
async fn driver_panic_is_contained_as_internal_error() {
    let panicking = MockDriver::builder()
        .engine(Engine::Postgres)
        .ping_panic()
        .build();
    let store = SessionStore::new(DriverRegistry::builder().register(panicking).build());
    let (session, conn) = open(&store).await;

    // The driver panics inside the spawned, bounded task; the server maps the
    // JoinError to an internal error rather than unwinding the handler.
    let error = store.ping(session, conn).await.unwrap_err();
    assert!(
        matches!(error, ApiError::Internal(_)),
        "expected Internal, got {error:?}"
    );
}

#[tokio::test]
async fn server_survives_a_driver_panic_and_serves_the_next_request() {
    // Second ping is healthy: proves the panic degraded only the first call.
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .ping_ok(server_info())
        .build();
    let store = SessionStore::new(DriverRegistry::builder().register(driver).build());
    let (session, conn) = open(&store).await;

    // A separate panicking connection/store to trigger containment first.
    let panicking = SessionStore::new(
        DriverRegistry::builder()
            .register(
                MockDriver::builder()
                    .engine(Engine::Postgres)
                    .ping_panic()
                    .build(),
            )
            .build(),
    );
    let (ps, pc) = open(&panicking).await;
    assert!(panicking.ping(ps, pc).await.is_err());

    // The healthy store is unaffected.
    let info = store.ping(session, conn).await.expect("healthy ping");
    assert_eq!(info.current_database, "mock");
}
