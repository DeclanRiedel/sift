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
    i64,
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
        protected: vec![root.path().join("protected.db")],
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
    for profile_id in profiles.iter().copied() {
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
    (
        root,
        client,
        session,
        connections[0],
        connections[1],
        profiles[0],
    )
}

fn request(action: SqliteMaintenanceAction) -> SqliteMaintenanceRequest {
    SqliteMaintenanceRequest {
        action,
        apply: false,
        confirm_write: false,
        backup_verified: false,
        preview_token: None,
    }
}

#[tokio::test]
async fn sqlite_analyze_and_reindex_use_reviewed_main_targets_and_atomic_writes() {
    let (root, client, session, writable, readonly, _) = fixture().await;
    let external = rusqlite::Connection::open(root.path().join("source.db")).unwrap();
    external.execute_batch("CREATE TABLE \"odd \"\" table\"(id INTEGER PRIMARY KEY,v TEXT UNIQUE); INSERT INTO \"odd \"\" table\" VALUES(1,'a'),(2,'b'),(3,'c'); CREATE INDEX \"odd \"\" index\" ON \"odd \"\" table\"(v) WHERE id>0; CREATE TABLE untouched(v TEXT); CREATE INDEX untouched_idx ON untouched(v); CREATE VIEW sample_view AS SELECT * FROM sample; CREATE VIRTUAL TABLE search USING fts5(body);").unwrap();
    let constraint_index=external.query_row("SELECT name FROM sqlite_schema WHERE tbl_name='odd \" table' AND type='index' AND sql IS NULL",[],|row|row.get::<_,String>(0)).unwrap();
    for action in [
        SqliteMaintenanceAction::Analyze {
            table: "odd \" table".into(),
        },
        SqliteMaintenanceAction::Reindex {
            index: "odd \" index".into(),
        },
        SqliteMaintenanceAction::Reindex {
            index: constraint_index,
        },
    ] {
        let review = request(action.clone());
        assert!(client
            .sqlite_maintenance(session, readonly, review.clone())
            .await
            .is_err());
        let preview = client
            .sqlite_maintenance(session, writable, review.clone())
            .await
            .unwrap();
        assert_eq!(
            preview.sql,
            sift_core::sqlite_maintenance::sql(&action).unwrap()
        );
        assert!(preview.sql.as_deref().unwrap().contains("\"main\"."));
        let version: i64 = external
            .query_row("PRAGMA data_version", [], |row| row.get(0))
            .unwrap();
        let mut apply = review;
        apply.apply = true;
        apply.confirm_write = true;
        apply.preview_token = preview.preview_token;
        assert!(
            client
                .sqlite_maintenance(session, writable, apply.clone())
                .await
                .is_err(),
            "backup acknowledgment is required"
        );
        apply.backup_verified = true;
        let tx = client
            .begin_transaction(
                session,
                writable,
                TxMode {
                    isolation: IsolationLevel::Serializable,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(client
            .sqlite_maintenance(session, writable, apply.clone())
            .await
            .is_err());
        client
            .rollback_transaction(session, writable, tx.tx_id)
            .await
            .unwrap();
        let result = client
            .sqlite_maintenance(session, writable, apply.clone())
            .await
            .unwrap();
        assert!(result.applied);
        assert_eq!(result.sql, preview.sql);
        assert_ne!(
            external
                .query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            version,
            "native maintenance must actually write"
        );
        assert!(
            client
                .sqlite_maintenance(session, writable, apply)
                .await
                .is_err(),
            "preview is one use"
        );
    }
    assert_eq!(
        external
            .query_row(
                "SELECT stat FROM sqlite_stat1 WHERE idx='odd \" index'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "3 1"
    );
    assert_eq!(
        external
            .query_row(
                "SELECT count(*) FROM sqlite_stat1 WHERE tbl='untouched'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(
        external
            .query_row(
                "SELECT group_concat(v,'') FROM \"odd \"\" table\" ORDER BY id",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "abc"
    );
    assert_eq!(
        external
            .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    for action in [
        SqliteMaintenanceAction::Analyze {
            table: "sample_view".into(),
        },
        SqliteMaintenanceAction::Analyze {
            table: "search".into(),
        },
        SqliteMaintenanceAction::Analyze {
            table: "search_data".into(),
        },
        SqliteMaintenanceAction::Analyze {
            table: "temp.sample".into(),
        },
        SqliteMaintenanceAction::Reindex {
            index: "missing\"; DELETE FROM sample;--".into(),
        },
    ] {
        assert!(client
            .sqlite_maintenance(session, writable, request(action))
            .await
            .is_err());
    }
    let review = request(SqliteMaintenanceAction::Analyze {
        table: "sample".into(),
    });
    let preview = client
        .sqlite_maintenance(session, writable, review.clone())
        .await
        .unwrap();
    external
        .execute_batch("CREATE INDEX changed_after_review ON sample(value)")
        .unwrap();
    let mut apply = review.clone();
    apply.apply = true;
    apply.confirm_write = true;
    apply.backup_verified = true;
    apply.preview_token = preview.preview_token;
    assert!(
        client
            .sqlite_maintenance(session, writable, apply)
            .await
            .is_err(),
        "changed native schema requires review"
    );
    let preview = client
        .sqlite_maintenance(session, writable, review.clone())
        .await
        .unwrap();
    let mut apply = review;
    apply.apply = true;
    apply.confirm_write = true;
    apply.backup_verified = true;
    apply.preview_token = preview.preview_token;
    external.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert!(
        client
            .sqlite_maintenance(session, writable, apply)
            .await
            .is_err(),
        "a competing writer prevents apply"
    );
    external.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        external
            .query_row(
                "SELECT count(*) FROM sqlite_stat1 WHERE tbl='sample'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0,
        "failed maintenance must not leave statistics"
    );
    assert_eq!(
        client
            .execute(session, writable, "SELECT value FROM sample")
            .await
            .unwrap()
            .rows[0]
            .values,
        [Value::Text("kept".into())]
    );
    let operations = client.operations().await.unwrap();
    for analyze in [true, false] {
        assert!(operations.iter().any(|entry| matches!(&entry.operation,Operation::SqliteMaintenance {request,..} if request.apply && matches!(&request.action,SqliteMaintenanceAction::Analyze {..})==analyze) && entry.status==OperationStatus::Succeeded));
    }
    client.close_session(session).await.unwrap();
}

#[tokio::test]
async fn sqlite_create_and_online_backup_require_preview_and_preserve_source() {
    let (root, client, session, writable, readonly, _) = fixture().await;
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
    let (root, client, session, writable, _, _) = fixture().await;
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

#[tokio::test]
async fn sqlite_vacuum_requires_preview_backup_ack_and_no_transaction() {
    let (root, client, session, writable, readonly, _) = fixture().await;
    let source = root.path().join("source.db");
    let external = rusqlite::Connection::open(&source).unwrap();
    external
        .execute("INSERT INTO sample VALUES(2, zeroblob(4000000))", [])
        .unwrap();
    external
        .execute("DELETE FROM sample WHERE id=2", [])
        .unwrap();
    drop(external);
    let before = std::fs::metadata(&source).unwrap().len();
    let vacuum = request(SqliteMaintenanceAction::Vacuum);
    assert!(client
        .sqlite_maintenance(session, readonly, vacuum.clone())
        .await
        .is_err());
    let preview = client
        .sqlite_maintenance(session, writable, vacuum.clone())
        .await
        .unwrap();
    assert!(!preview.applied);
    assert_eq!(preview.destination_file, None);
    assert!(preview.estimated_extra_bytes.unwrap() >= 32 * 1024 * 1024);
    assert_eq!(std::fs::metadata(&source).unwrap().len(), before);

    let mut apply = vacuum;
    apply.apply = true;
    apply.confirm_write = true;
    apply.backup_verified = true;
    assert!(client
        .sqlite_maintenance(session, writable, apply.clone())
        .await
        .is_err());
    apply.backup_verified = false;
    apply.preview_token = preview.preview_token;
    assert!(client
        .sqlite_maintenance(session, writable, apply.clone())
        .await
        .is_err());
    apply.backup_verified = true;
    let tx = client
        .begin_transaction(
            session,
            writable,
            TxMode {
                isolation: IsolationLevel::Serializable,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(client
        .sqlite_maintenance(session, writable, apply.clone())
        .await
        .is_err());
    client
        .rollback_transaction(session, writable, tx.tx_id)
        .await
        .unwrap();
    let report = client
        .sqlite_maintenance(session, writable, apply.clone())
        .await
        .unwrap();
    assert!(report.applied);
    assert!(client
        .sqlite_maintenance(session, writable, apply)
        .await
        .is_err());
    assert!(std::fs::metadata(&source).unwrap().len() < before);
    let db = rusqlite::Connection::open(&source).unwrap();
    assert_eq!(
        db.query_row("SELECT value FROM sample WHERE id=1", [], |row| row
            .get::<_, String>(0))
            .unwrap(),
        "kept"
    );
    assert_eq!(
        db.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    let operations = client.operations().await.unwrap();
    assert!(operations.iter().any(|entry| matches!(
        &entry.operation,
        Operation::SqliteMaintenance { request, .. }
            if request.apply
                && request.backup_verified
                && matches!(&request.action, SqliteMaintenanceAction::Vacuum)
    )));
}

#[tokio::test]
async fn sqlite_restore_reviews_wal_snapshot_replaces_main_and_preserves_backup() {
    let (root, client, session, writable, readonly, profile) = fixture().await;
    let archive_path = root.path().join("archive.db");
    let archive = rusqlite::Connection::open(&archive_path).unwrap();
    archive.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE restored(id INTEGER PRIMARY KEY, value); INSERT INTO restored VALUES(7,'from WAL'),(8,x'00ff'),(9,NULL); CREATE INDEX restored_idx ON restored(value); CREATE VIEW restored_view AS SELECT id FROM restored; CREATE TABLE child(parent INTEGER REFERENCES restored(id)); INSERT INTO child VALUES(7); CREATE TRIGGER child_insert AFTER INSERT ON child BEGIN UPDATE restored SET value=value WHERE id=NEW.parent; END;").unwrap();
    let restore = request(SqliteMaintenanceAction::Restore {
        path: "archive.db".into(),
    });
    assert!(client
        .sqlite_maintenance(session, readonly, restore.clone())
        .await
        .is_err());
    let apply = |mut request: SqliteMaintenanceRequest, token: Option<String>| {
        request.apply = true;
        request.confirm_write = true;
        request.backup_verified = true;
        request.preview_token = token;
        request
    };
    let preview = client
        .sqlite_maintenance(session, writable, restore.clone())
        .await
        .unwrap();
    assert!(!preview.applied);
    assert_eq!(preview.backup_file.as_deref(), Some("archive.db"));
    assert!(preview.destination_file.is_none());
    assert_eq!(
        client
            .execute(session, writable, "SELECT value FROM sample")
            .await
            .unwrap()
            .rows[0]
            .values,
        vec![Value::Text("kept".into())]
    );
    let mut confirmed = apply(restore.clone(), preview.preview_token);
    confirmed.backup_verified = false;
    assert!(client
        .sqlite_maintenance(session, writable, confirmed.clone())
        .await
        .is_err());
    confirmed.backup_verified = true;
    confirmed.confirm_write = false;
    assert!(client
        .sqlite_maintenance(session, writable, confirmed.clone())
        .await
        .is_err());
    // Preview is identity-bound to the backup, including committed WAL frames.
    archive
        .execute("INSERT INTO restored VALUES(10,42)", [])
        .unwrap();
    confirmed.confirm_write = true;
    assert!(client
        .sqlite_maintenance(session, writable, confirmed)
        .await
        .is_err());
    let preview = client
        .sqlite_maintenance(session, writable, restore.clone())
        .await
        .unwrap();
    client
        .execute(session, writable, "INSERT INTO sample VALUES(2,'changed')")
        .await
        .unwrap();
    assert!(client
        .sqlite_maintenance(
            session,
            writable,
            apply(restore.clone(), preview.preview_token)
        )
        .await
        .is_err());
    let tx = client
        .begin_transaction(
            session,
            writable,
            TxMode {
                isolation: IsolationLevel::Serializable,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(client
        .sqlite_maintenance(session, writable, restore.clone())
        .await
        .is_err());
    client
        .rollback_transaction(session, writable, tx.tx_id)
        .await
        .unwrap();
    let before = client
        .catalog_graph(session, writable, CatalogGraphRequest::default())
        .await
        .unwrap();
    let _readonly_before = client
        .catalog_graph(session, readonly, CatalogGraphRequest::default())
        .await
        .unwrap();
    let preview = client
        .sqlite_maintenance(session, writable, restore.clone())
        .await
        .unwrap();
    let confirmed = apply(restore.clone(), preview.preview_token);
    let report = client
        .sqlite_maintenance(session, writable, confirmed.clone())
        .await
        .unwrap();
    assert!(report.applied);
    assert!(report.preview_token.is_none());
    assert!(client
        .sqlite_maintenance(session, writable, confirmed)
        .await
        .is_err());
    let expected = vec![
        vec![
            Value::Int64(7),
            Value::Text("text".into()),
            Value::Int64(7),
            Value::Text("from WAL".into()),
        ],
        vec![
            Value::Int64(8),
            Value::Text("blob".into()),
            Value::Int64(8),
            Value::Blob(vec![0, 255]),
        ],
        vec![
            Value::Int64(9),
            Value::Text("null".into()),
            Value::Int64(9),
            Value::Null,
        ],
        vec![
            Value::Int64(10),
            Value::Text("integer".into()),
            Value::Int64(10),
            Value::Int64(42),
        ],
    ];
    for connection in [writable, readonly] {
        let rows = client
            .execute(
                session,
                connection,
                "SELECT rowid,typeof(value),id,value FROM restored ORDER BY rowid",
            )
            .await
            .unwrap()
            .rows
            .into_iter()
            .map(|row| row.values)
            .collect::<Vec<_>>();
        assert_eq!(rows, expected);
        assert!(client
            .execute(session, connection, "SELECT * FROM sample")
            .await
            .is_err());
        let graph = client
            .catalog_graph(session, connection, CatalogGraphRequest::default())
            .await
            .unwrap();
        assert!(graph
            .data
            .nodes
            .iter()
            .any(|node| node.kind == CatalogNodeKind::Table && node.name == "restored"));
        assert!(!graph
            .data
            .nodes
            .iter()
            .any(|node| node.kind == CatalogNodeKind::Table && node.name == "sample"));
        if connection == writable {
            assert_ne!(before.revision, graph.revision);
        }
    }
    assert!(
        client
            .execute(session, writable, "DELETE FROM restored WHERE id=7")
            .await
            .is_err(),
        "restored foreign keys remain enforced"
    );
    client
        .execute(session, writable, "INSERT INTO child VALUES(8)")
        .await
        .unwrap();
    let schema = client.execute(session,writable,"SELECT type FROM sqlite_schema WHERE name IN ('restored_idx','restored_view','child_insert') ORDER BY type").await.unwrap();
    assert_eq!(
        schema
            .rows
            .into_iter()
            .map(|row| row.values)
            .collect::<Vec<_>>(),
        vec![
            vec![Value::Text("index".into())],
            vec![Value::Text("trigger".into())],
            vec![Value::Text("view".into())]
        ]
    );
    assert_eq!(
        archive
            .query_row("SELECT count(*) FROM child", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        archive
            .query_row("SELECT count(*) FROM restored", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        4
    );
    let main = rusqlite::Connection::open(root.path().join("source.db")).unwrap();
    assert_eq!(
        main.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert!(client.operations().await.unwrap().iter().any(|entry| matches!(&entry.operation,Operation::SqliteMaintenance{request,..} if request.apply && request.backup_verified && request.preview_token.is_none() && matches!(request.action,SqliteMaintenanceAction::Restore{..}))));
    // File-wide maintenance cannot be scoped down to selected schemas.
    client
        .update_connection_policy(
            sift_api_types::ConnectionProfileId(profile),
            UpdateConnectionPolicyRequest {
                expected_revision: None,
                minimum_tenant_role: TenantRole::Member,
                read_only: false,
                allowed_ops: None,
                blocked_ops: vec![],
                allowed_schemas: Some(vec![SchemaSelector {
                    catalog: None,
                    schema: "main".into(),
                }]),
            },
        )
        .await
        .unwrap();
    assert!(client
        .sqlite_maintenance(session, writable, restore)
        .await
        .is_err());
}

#[tokio::test]
async fn sqlite_restore_rejects_untrusted_file_sources_without_target_changes() {
    let (root, client, session, writable, _, _) = fixture().await;
    std::fs::write(root.path().join("invalid.db"), b"not a database").unwrap();
    std::fs::write(root.path().join("empty.db"), []).unwrap();
    let valid = rusqlite::Connection::open(root.path().join("protected.db")).unwrap();
    valid
        .execute_batch("CREATE TABLE private(value TEXT)")
        .unwrap();
    std::os::unix::fs::symlink(
        root.path().join("protected.db"),
        root.path().join("linked.db"),
    )
    .unwrap();
    let hard = rusqlite::Connection::open(root.path().join("hard.db")).unwrap();
    hard.execute_batch("CREATE TABLE data(value TEXT)").unwrap();
    drop(hard);
    std::fs::hard_link(root.path().join("hard.db"), root.path().join("alias.db")).unwrap();
    let corrupt = rusqlite::Connection::open(root.path().join("corrupt.db")).unwrap();
    corrupt.execute_batch("PRAGMA page_size=4096; CREATE TABLE broken(value TEXT); INSERT INTO broken VALUES('data');").unwrap();
    drop(corrupt);
    {
        use std::io::{Seek, SeekFrom, Write};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(root.path().join("corrupt.db"))
            .unwrap();
        file.seek(SeekFrom::Start(4096)).unwrap();
        file.write_all(&[255; 64]).unwrap();
    }
    for path in [
        "../escape.db",
        "/tmp/escape.db",
        "source.db",
        "source.db-wal",
        "missing.db",
        "invalid.db",
        "corrupt.db",
        "empty.db",
        "protected.db",
        "linked.db",
        "hard.db",
        "alias.db",
    ] {
        assert!(
            client
                .sqlite_maintenance(
                    session,
                    writable,
                    request(SqliteMaintenanceAction::Restore { path: path.into() })
                )
                .await
                .is_err(),
            "accepted {path}"
        );
    }
    let valid = rusqlite::Connection::open(root.path().join("valid.db")).unwrap();
    valid
        .execute_batch("CREATE TABLE replacement(value TEXT)")
        .unwrap();
    drop(valid);
    std::fs::rename(root.path().join("source.db"), root.path().join("opened.db")).unwrap();
    std::fs::copy(root.path().join("valid.db"), root.path().join("source.db")).unwrap();
    assert!(
        client
            .sqlite_maintenance(
                session,
                writable,
                request(SqliteMaintenanceAction::Restore {
                    path: "valid.db".into()
                })
            )
            .await
            .is_err(),
        "reopen a replaced connected file before restore"
    );
    std::fs::remove_file(root.path().join("source.db")).unwrap();
    std::fs::rename(root.path().join("opened.db"), root.path().join("source.db")).unwrap();
    assert_eq!(
        client
            .execute(session, writable, "SELECT value FROM sample")
            .await
            .unwrap()
            .rows[0]
            .values,
        vec![Value::Text("kept".into())]
    );
}
