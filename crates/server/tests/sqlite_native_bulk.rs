//! Real-file checks for preview-bound SQLite native parameter batches.
#![cfg(unix)]

use std::sync::Arc;

use sift_api_types::OpenConnectionFromProfileRequest;
use sift_driver_sqlite::{FilePolicy, SqliteDriver};
use sift_metadata::{
    CredentialMode, MemorySecretStore, MetadataStore, NewConnectionProfile, PrincipalId, TenantId,
};
use sift_protocol::*;
use sift_server::{
    http::{app, AppState, AuthState},
    room_runtime::RoomRuntime,
    DriverRegistry, SessionStore,
};

async fn fixture() -> (
    tempfile::TempDir,
    sift_client_sdk::Client,
    SessionId,
    ConnectionId,
    ConnectionId,
) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("native.db");
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(
            "CREATE TABLE records (id INTEGER PRIMARY KEY, amount TEXT NOT NULL, sample REAL, payload BLOB); \
             CREATE TABLE numeric_target (amount NUMERIC); \
             CREATE TABLE with_trigger (id INTEGER); \
             CREATE TRIGGER audit_insert AFTER INSERT ON with_trigger BEGIN SELECT 1; END;",
        )
        .unwrap();
    let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
    metadata.bootstrap_local("sqlite native bulk test").unwrap();
    let mut ids = Vec::new();
    for (name, mode) in [("writable", "read_write"), ("read only", "read_only")] {
        let profile = metadata
            .upsert_connection_profile(
                TenantId(1),
                PrincipalId(1),
                NewConnectionProfile {
                    name: name.into(),
                    provider_id: Engine::Sqlite.provider_id(),
                    configuration: serde_json::json!({"root_id":"test","path":"native.db","mode":mode}),
                    semantic_engine: Some(Engine::Sqlite),
                    credentials: None,
                    credential_mode: CredentialMode::Shared,
                    tags: vec![],
                },
            )
            .await
            .unwrap();
        ids.push(profile.id.0);
    }
    let driver = SqliteDriver::with_files(FilePolicy {
        config: SqliteDriverConfig {
            roots: std::collections::BTreeMap::from([(
                "test".into(),
                SqliteRootConfig {
                    path: root.path().to_str().unwrap().into(),
                    allowed_tenants: vec![1],
                    read_only: false,
                },
            )]),
            max_connections: 3,
        },
        protected: vec![],
    });
    let router = app(AppState {
        sessions: SessionStore::new(DriverRegistry::builder().register(driver).build()),
        rooms: RoomRuntime::default(),
        shutdown: Default::default(),
        auth: AuthState::default(),
        metadata: Some(metadata),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = sift_client_sdk::Client::new(format!("http://{addr}"));
    let session = client.open_session(None).await.unwrap().id;
    let writable = client
        .open_connection_from_profile(
            session,
            OpenConnectionFromProfileRequest {
                tenant_id: 1,
                profile_id: ids[0],
            },
        )
        .await
        .unwrap()
        .id;
    let readonly = client
        .open_connection_from_profile(
            session,
            OpenConnectionFromProfileRequest {
                tenant_id: 1,
                profile_id: ids[1],
            },
        )
        .await
        .unwrap()
        .id;
    let _ = server; // The listener lives for the duration of the runtime.
    (root, client, session, writable, readonly)
}

fn request(table: &str, columns: &[&str], rows: Vec<Vec<Value>>) -> BulkInsertRequest {
    BulkInsertRequest {
        table: table.into(),
        format: BulkInsertFormat::Native,
        native: Some(SqliteNativeBulkRows {
            columns: columns.iter().map(|column| (*column).into()).collect(),
            rows,
        }),
        preview: true,
        ..Default::default()
    }
}

#[tokio::test]
async fn sqlite_native_bulk_previews_then_stores_exact_text_decimal_and_audits_without_values() {
    let (root, client, session, writable, readonly) = fixture().await;
    let decimal = "12345678901234567890.123456789012345678";
    let mut insert = request(
        "records",
        &["id", "amount", "sample", "payload"],
        vec![vec![
            Value::Int64(1),
            Value::Decimal(decimal.into()),
            Value::Float64(1.25),
            Value::Blob(vec![0, 1, 255]),
        ]],
    );
    if let Ok(readonly_preview) = client.bulk_insert(session, readonly, insert.clone()).await {
        let mut readonly_apply = insert.clone();
        readonly_apply.preview = false;
        readonly_apply.confirm_write = true;
        readonly_apply.preview_token = readonly_preview.preview_token;
        assert!(client
            .bulk_insert(session, readonly, readonly_apply)
            .await
            .is_err());
    }
    assert!(client
        .bulk_insert(session, writable, {
            let mut missing_preview = insert.clone();
            missing_preview.preview = false;
            missing_preview.confirm_write = true;
            missing_preview
        })
        .await
        .is_err());
    let preview = client
        .bulk_insert(session, writable, insert.clone())
        .await
        .unwrap();
    assert_eq!(preview.rows_inserted, 0);
    assert_eq!(preview.rows_validated, 1);
    assert!(preview
        .target_affinities
        .iter()
        .any(|entry| entry == "amount:text"));
    let path = root.path().join("native.db");
    assert_eq!(
        rusqlite::Connection::open(&path)
            .unwrap()
            .query_row("SELECT count(*) FROM records", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    insert.preview = false;
    insert.confirm_write = true;
    insert.preview_token = preview.preview_token;
    let mut changed = insert.clone();
    changed.native.as_mut().unwrap().rows[0][0] = Value::Int64(2);
    assert!(client
        .bulk_insert(session, writable, changed)
        .await
        .is_err());
    let replacement = client
        .bulk_insert(session, writable, {
            let mut retry = insert.clone();
            retry.preview = true;
            retry.confirm_write = false;
            retry.preview_token = None;
            retry
        })
        .await
        .unwrap();
    insert.preview_token = replacement.preview_token;
    let applied = client
        .bulk_insert(session, writable, insert.clone())
        .await
        .unwrap();
    assert_eq!(applied.rows_inserted, 1);
    assert!(client.bulk_insert(session, writable, insert).await.is_err());
    let stored = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row(
            "SELECT amount, typeof(amount), typeof(sample), payload FROM records WHERE id=1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        stored,
        (
            decimal.into(),
            "text".into(),
            "real".into(),
            vec![0, 1, 255]
        )
    );
    let operations = client.operations().await.unwrap();
    assert!(operations.iter().any(|entry| matches!(
        &entry.operation,
        Operation::BulkInsert { request, .. }
            if request.native.is_none() && request.preview_token.is_none() && request.data.is_empty()
    )));
}

#[tokio::test]
async fn sqlite_native_bulk_refuses_lossy_shapes_and_rolls_back_all_batches() {
    let (root, client, session, connection, _) = fixture().await;
    assert!(client
        .bulk_insert(
            session,
            connection,
            request(
                "numeric_target",
                &["amount"],
                vec![vec![Value::Decimal("1.25".into())]]
            )
        )
        .await
        .is_err());
    assert!(client
        .bulk_insert(
            session,
            connection,
            request("with_trigger", &["id"], vec![vec![Value::Int64(1)]])
        )
        .await
        .is_err());
    assert!(client
        .bulk_insert(
            session,
            connection,
            request(
                "records",
                &["amount"],
                vec![vec![Value::Decimal("1.1234567890123456789".into())]]
            )
        )
        .await
        .is_err());
    assert!(client
        .bulk_insert(
            session,
            connection,
            request("records", &["amount"], vec![vec![Value::Int64(1)]])
        )
        .await
        .is_err());
    let rows = (1..=130)
        .map(|id| {
            vec![
                Value::Int64(if id == 130 { 1 } else { id }),
                Value::Text("ok".into()),
            ]
        })
        .collect();
    let mut insert = request("records", &["id", "amount"], rows);
    let preview = client
        .bulk_insert(session, connection, insert.clone())
        .await
        .unwrap();
    insert.preview = false;
    insert.confirm_write = true;
    insert.preview_token = preview.preview_token;
    assert!(client
        .bulk_insert(session, connection, insert)
        .await
        .is_err());
    let count = rusqlite::Connection::open(root.path().join("native.db"))
        .unwrap()
        .query_row("SELECT count(*) FROM records", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    assert_eq!(count, 0);
}
