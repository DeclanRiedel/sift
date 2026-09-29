//! Real-file SQLite root maintenance through the public HTTP SDK.
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
    let source = root.path().join("source.db");
    let database = rusqlite::Connection::open(&source).unwrap();
    database
        .execute_batch("CREATE TABLE sample(id INTEGER PRIMARY KEY, value TEXT); INSERT INTO sample VALUES(1,'kept');")
        .unwrap();
    drop(database);
    let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
    metadata.bootstrap_local("sqlite maintenance test").unwrap();
    let mut profiles = Vec::new();
    for mode in ["read_write", "read_only"] {
        let profile = metadata
            .upsert_connection_profile(
                TenantId(1),
                PrincipalId(1),
                NewConnectionProfile {
                    name: format!("SQLite {mode}"),
                    provider_id: Engine::Sqlite.provider_id(),
                    configuration: serde_json::json!({"root_id":"test","path":"source.db","mode":mode}),
                    semantic_engine: Some(Engine::Sqlite),
                    credentials: None,
                    credential_mode: CredentialMode::Shared,
                    tags: vec![],
                },
            )
            .await
            .unwrap();
        profiles.push(profile.id.0);
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
    let address = listener.local_addr().unwrap();
    let _server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = sift_client_sdk::Client::new(format!("http://{address}"));
    let session = client.open_session(None).await.unwrap().id;
    let mut connections = Vec::new();
    for profile_id in profiles {
        connections.push(
            client
                .open_connection_from_profile(
                    session,
                    OpenConnectionFromProfileRequest {
                        tenant_id: 1,
                        profile_id,
                    },
                )
                .await
                .unwrap()
                .id,
        );
    }
    (root, client, session, connections[0], connections[1])
}

fn request(action: SqliteMaintenanceAction) -> SqliteMaintenanceRequest {
    SqliteMaintenanceRequest {
        action,
        apply: false,
        confirm_write: false,
        preview_token: None,
    }
}

#[tokio::test]
async fn sqlite_create_and_online_backup_require_preview_and_preserve_source() {
    let (root, client, session, writable, readonly) = fixture().await;
    let create = request(SqliteMaintenanceAction::Create {
        path: "new.db".into(),
    });
    assert!(client
        .sqlite_maintenance(session, readonly, create.clone())
        .await
        .is_err());
    let preview = client
        .sqlite_maintenance(session, writable, create.clone())
        .await
        .unwrap();
    assert!(!preview.applied);
    assert_eq!(preview.root_id, "test");
    assert!(!root.path().join("new.db").exists());
    let mut apply = create.clone();
    apply.apply = true;
    apply.confirm_write = true;
    apply.preview_token = preview.preview_token;
    assert!(client
        .sqlite_maintenance(session, writable, {
            let mut missing = apply.clone();
            missing.preview_token = None;
            missing
        })
        .await
        .is_err());
    let created = client
        .sqlite_maintenance(session, writable, apply.clone())
        .await
        .unwrap();
    assert!(created.applied);
    assert!(client
        .sqlite_maintenance(session, writable, apply)
        .await
        .is_err());
    let opened = rusqlite::Connection::open(root.path().join("new.db")).unwrap();
    assert_eq!(
        opened
            .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert_eq!(
        opened
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE type='table'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );

    let backup = request(SqliteMaintenanceAction::Backup {
        path: "backup.db".into(),
    });
    let preview = client
        .sqlite_maintenance(session, writable, backup.clone())
        .await
        .unwrap();
    assert_eq!(preview.backup_file.as_deref(), Some("backup.db"));
    assert!(!root.path().join("backup.db").exists());
    let mut apply = backup;
    apply.apply = true;
    apply.confirm_write = true;
    apply.preview_token = preview.preview_token;
    let applied = client
        .sqlite_maintenance(session, writable, apply)
        .await
        .unwrap();
    assert!(applied.applied);
    let copied = rusqlite::Connection::open(root.path().join("backup.db")).unwrap();
    assert_eq!(
        copied
            .query_row("SELECT value FROM sample WHERE id=1", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "kept"
    );
    assert_eq!(
        copied
            .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    let operations = client.operations().await.unwrap();
    assert!(operations.iter().any(|entry| matches!(
        &entry.operation,
        Operation::SqliteMaintenance { request, .. }
            if request.preview_token.is_none() && matches!(&request.action, SqliteMaintenanceAction::Backup { .. })
    )));
}

#[tokio::test]
async fn sqlite_maintenance_rejects_path_escape_symlinks_existing_targets_and_stale_previews() {
    let (root, client, session, writable, _) = fixture().await;
    std::fs::create_dir(root.path().join("safe")).unwrap();
    std::os::unix::fs::symlink(root.path().join("safe"), root.path().join("linked")).unwrap();
    for path in [
        "../outside.db",
        "/tmp/outside.db",
        "linked/escape.db",
        "source.db",
        "source.db-wal",
    ] {
        assert!(
            client
                .sqlite_maintenance(
                    session,
                    writable,
                    request(SqliteMaintenanceAction::Create { path: path.into() })
                )
                .await
                .is_err(),
            "accepted {path}"
        );
    }
    let create = request(SqliteMaintenanceAction::Create {
        path: "safe/new.db".into(),
    });
    let preview = client
        .sqlite_maintenance(session, writable, create.clone())
        .await
        .unwrap();
    let mut changed = create.clone();
    changed.action = SqliteMaintenanceAction::Backup {
        path: "safe/new.db".into(),
    };
    changed.apply = true;
    changed.confirm_write = true;
    changed.preview_token = preview.preview_token;
    assert!(client
        .sqlite_maintenance(session, writable, changed)
        .await
        .is_err());
    assert!(!root.path().join("safe/new.db").exists());
    let preview = client
        .sqlite_maintenance(session, writable, create.clone())
        .await
        .unwrap();
    rusqlite::Connection::open(root.path().join("source.db"))
        .unwrap()
        .execute("INSERT INTO sample VALUES(2, zeroblob(100000))", [])
        .unwrap();
    let mut apply = create;
    apply.apply = true;
    apply.confirm_write = true;
    apply.preview_token = preview.preview_token;
    assert!(client
        .sqlite_maintenance(session, writable, apply)
        .await
        .is_err());
    assert!(!root.path().join("safe/new.db").exists());
}
