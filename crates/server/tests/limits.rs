//! HTTP execute result caps (Phase B). The synchronous execute path bounds
//! both row count and total bytes so a large result cannot OOM the server;
//! exceeding either returns `Code::ResultTooLarge`.

use sift_driver_api::mock::MockDriver;
use sift_protocol::{
    Code, ColumnMetadata, ConnectionSpec, Engine, ExecuteRequestHttp, Nullability,
    OpenSessionRequest, Page, PrimitiveType, Row, SslMode, TypeRef, Value,
};
use sift_server::error::ApiError;
use sift_server::registry::DriverRegistry;
use sift_server::session::SessionStore;

fn columns() -> Vec<ColumnMetadata> {
    vec![ColumnMetadata {
        name: "v".into(),
        type_ref: TypeRef::Primitive(PrimitiveType::Text),
        nullable: Nullability::Nullable,
        auto_increment: false,
        primary_key: false,
        facets: Default::default(),
    }]
}

fn pages(rows: Vec<Row>) -> Vec<Page> {
    vec![
        Page::NextResult { columns: columns() },
        Page::Rows { rows },
        Page::Done {
            affected_rows: None,
            warnings: Vec::new(),
        },
    ]
}

fn mock_spec() -> ConnectionSpec {
    ConnectionSpec {
        host: "mock".into(),
        port: None,
        database: Some("mock".into()),
        user: "mock".into(),
        password: None,
        ssl_mode: Some(SslMode::Disable),
        engine_specific: None,
    }
}

async fn store_and_conn(
    pages: Vec<Page>,
) -> (
    SessionStore,
    sift_protocol::SessionId,
    sift_protocol::ConnectionId,
) {
    let driver = MockDriver::builder()
        .engine(Engine::Postgres)
        .execute_ok(pages)
        .build();
    let store = SessionStore::new(DriverRegistry::builder().register(driver).build());
    let session = store.open_session(OpenSessionRequest { tag: None });
    let conn = store
        .open_connection(session.id, Engine::Postgres, mock_spec())
        .await
        .unwrap();
    (store, session.id, conn.id)
}

fn req(conn: sift_protocol::ConnectionId) -> ExecuteRequestHttp {
    ExecuteRequestHttp {
        connection: conn,
        sql: "select v from t".into(),
        params: Vec::new(),
        tx: None,
        room_id: None,
        connection_profile_id: None,
    }
}

fn assert_too_large(result: Result<impl std::fmt::Debug, ApiError>) {
    match result {
        Err(ApiError::Driver(e)) => assert_eq!(e.code, Code::ResultTooLarge, "got {e:?}"),
        other => panic!("expected ResultTooLarge, got {other:?}"),
    }
}

#[tokio::test]
async fn byte_cap_trips_on_wide_rows() {
    // One ~4 KiB text row, byte cap of 1 KiB.
    let big = Value::Text("x".repeat(4096));
    let (store, sid, cid) = store_and_conn(pages(vec![Row::new(vec![big])])).await;
    store.set_result_limits(10_000, 1024);

    assert_too_large(store.execute_http(sid, req(cid)).await);
}

#[tokio::test]
async fn row_cap_trips_before_byte_cap() {
    let rows = vec![
        Row::new(vec![Value::Text("a".into())]),
        Row::new(vec![Value::Text("b".into())]),
        Row::new(vec![Value::Text("c".into())]),
    ];
    let (store, sid, cid) = store_and_conn(pages(rows)).await;
    store.set_result_limits(2, 16 * 1024 * 1024);

    assert_too_large(store.execute_http(sid, req(cid)).await);
}

#[tokio::test]
async fn within_caps_succeeds() {
    let (store, sid, cid) =
        store_and_conn(pages(vec![Row::new(vec![Value::Text("small".into())])])).await;
    store.set_result_limits(10_000, 16 * 1024 * 1024);

    let resp = store
        .execute_http(sid, req(cid))
        .await
        .expect("within caps");
    assert_eq!(resp.rows.len(), 1);
}
