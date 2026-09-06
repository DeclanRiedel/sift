#![cfg(unix)]
use sift_driver_api::{ConnHandle, Driver, SqliteExt};
use sift_driver_sqlite::{FilePolicy, SqliteDriver};
use sift_protocol::*;

struct Fixture {
    root: tempfile::TempDir,
    driver: SqliteDriver,
}
impl Fixture {
    fn new(cap: usize) -> Self {
        let root = tempfile::tempdir().unwrap();
        rusqlite::Connection::open(root.path().join("data.db"))
            .unwrap()
            .execute_batch("CREATE TABLE seed(id INTEGER PRIMARY KEY, value TEXT);")
            .unwrap();
        let driver = SqliteDriver::with_files(FilePolicy {
            config: SqliteDriverConfig {
                max_connections: cap,
                roots: std::collections::BTreeMap::from([(
                    "test".into(),
                    SqliteRootConfig {
                        path: root.path().to_str().unwrap().into(),
                        allowed_tenants: vec![1],
                        read_only: false,
                    },
                )]),
            },
            protected: vec![],
        });
        Self { root, driver }
    }
    fn spec(&self, mode: SqliteOpenMode) -> SqliteFileConfiguration {
        SqliteFileConfiguration {
            root_id: "test".into(),
            path: "data.db".into(),
            mode,
            busy_timeout_ms: 40,
        }
    }
    async fn open(&self, mode: SqliteOpenMode) -> ConnHandle {
        self.driver
            .open_file(self.spec(mode), Some(1))
            .await
            .unwrap()
    }
}
async fn execute(
    driver: &SqliteDriver,
    c: &ConnHandle,
    sql: &str,
    params: Vec<Value>,
) -> Result<(Vec<Row>, Option<u64>, usize), DriverError> {
    let mut stream = driver
        .execute(
            c.clone(),
            ExecuteRequest {
                sql: sql.into(),
                params,
                transform: None,
            },
        )
        .await?;
    let mut rows = vec![];
    let mut affected = None;
    let mut sets = 0;
    let mut done = false;
    while let Some(page) = stream.rows.recv().await {
        match page {
            Page::NextResult { .. } => sets += 1,
            Page::Rows { rows: page } => {
                assert!(page.len() <= 128);
                rows.extend(page)
            }
            Page::Done { affected_rows, .. } => {
                done = true;
                affected = affected_rows
            }
            Page::Error { error } => return Err(error),
        }
    }
    assert!(done, "stream closed without terminal outcome");
    Ok((rows, affected, sets))
}
#[tokio::test]
async fn runtime_values_parameters_and_native_batches() {
    let f = Fixture::new(2);
    let c = f.open(SqliteOpenMode::ReadWrite).await;
    let decimal = "12345678901234567890.123456789";
    let (rows, _, _) = execute(
        &f.driver,
        &c,
        "SELECT ?1, ?2, ?3, ?4, ?5",
        vec![
            Value::Decimal(decimal.into()),
            Value::Blob(vec![0, 255]),
            Value::Int64(i64::MIN),
            Value::Bool(true),
            Value::Null,
        ],
    )
    .await
    .unwrap();
    assert_eq!(
        rows[0].values,
        vec![
            Value::Text(decimal.into()),
            Value::Blob(vec![0, 255]),
            Value::Int64(i64::MIN),
            Value::Int64(1),
            Value::Null
        ]
    );
    let (rows,affected,sets)=execute(&f.driver,&c,"CREATE TRIGGER seed_log AFTER INSERT ON seed BEGIN UPDATE seed SET value='a;b' WHERE id=NEW.id; END; INSERT INTO seed VALUES(1,'old') RETURNING id; SELECT value FROM seed; SELECT 1 WHERE 0;",vec![]).await.unwrap();
    assert_eq!(sets, 3);
    assert_eq!(affected, Some(1));
    assert_eq!(rows[1].values, vec![Value::Text("a;b".into())]);
    for sql in [
        "EXPLAIN UPDATE seed SET value='unchanged'",
        "EXPLAIN QUERY PLAN DELETE FROM seed",
        "ANALYZE seed",
    ] {
        assert_eq!(
            execute(&f.driver, &c, sql, vec![]).await.unwrap().1,
            None,
            "{sql} must not report stale DML counts"
        );
    }
    assert_eq!(
        execute(&f.driver, &c, "SELECT value FROM seed", vec![])
            .await
            .unwrap()
            .0[0]
            .values,
        vec![Value::Text("a;b".into())]
    );
    let (rows, _, _) = execute(
        &f.driver,
        &c,
        "SELECT 7 AS value UNION ALL SELECT 'text' UNION ALL SELECT x'00' UNION ALL SELECT NULL",
        vec![],
    )
    .await
    .unwrap();
    assert!(matches!(rows[0].values[0], Value::Int64(7)));
    assert!(matches!(rows[1].values[0], Value::Text(_)));
    assert!(matches!(rows[2].values[0], Value::Blob(_)));
    assert!(matches!(rows[3].values[0], Value::Null));
    for (sql, params) in [
        (
            "SELECT ?1; INSERT INTO seed VALUES(2,'bad')",
            vec![Value::Int64(1)],
        ),
        ("SELECT :named", vec![Value::Int64(1)]),
        ("SELECT ?2", vec![Value::Int64(1), Value::Int64(2)]),
        ("SELECT ?, ?1", vec![Value::Int64(1)]),
        ("SELECT ?1", vec![]),
        ("SELECT ?1", vec![Value::Float64(f64::NAN)]),
    ] {
        assert!(execute(&f.driver, &c, sql, params).await.is_err());
    }
    let failed=execute(&f.driver,&c,"INSERT INTO seed VALUES(3,'kept'); SELECT missing FROM seed; INSERT INTO seed VALUES(4,'never');",vec![]).await;
    assert!(failed.is_err());
    let (rows, _, _) = execute(&f.driver, &c, "SELECT id FROM seed ORDER BY id", vec![])
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    f.driver.close(c).await.unwrap();
}
#[tokio::test]
async fn sql_authority_and_readonly_are_enforced_by_sqlite() {
    let f = Fixture::new(2);
    let c = f.open(SqliteOpenMode::ReadOnly).await;
    for sql in [
        "INSERT INTO seed VALUES(1,'denied')",
        "CREATE TEMP TABLE bypass(id)",
        "PRAGMA query_only=OFF",
        "PRAGMA writable_schema=ON",
        "PRAGMA trusted_schema=ON",
        "ATTACH ':memory:' AS other",
        "VACUUM INTO '/tmp/sift-should-not-exist.db'",
        "SELECT load_extension('anything')",
        "BEGIN",
    ] {
        assert!(
            execute(&f.driver, &c, sql, vec![]).await.is_err(),
            "allowed {sql}"
        );
    }
    assert_eq!(
        f.driver.ping(c.clone()).await.unwrap().provider.provider_id,
        Engine::Sqlite.provider_id()
    );
    let snapshot = f
        .driver
        .schema(c.clone(), SchemaScope::deep(ObjectPath::new("seed")))
        .await
        .unwrap();
    assert_eq!(snapshot.trees[0].schemas[0].objects[0].columns.len(), 2);
    f.driver.close(c).await.unwrap();
    let c = f.open(SqliteOpenMode::ReadWrite).await;
    for sql in [
        "ATTACH ':memory:' AS other",
        "PRAGMA foreign_keys=OFF",
        "PRAGMA journal_mode=WAL",
        "BEGIN",
        "SAVEPOINT bypass",
    ] {
        assert!(
            execute(&f.driver, &c, sql, vec![]).await.is_err(),
            "allowed {sql}"
        );
    }
    let tx = f
        .driver
        .begin(
            c.clone(),
            TxMode {
                isolation: IsolationLevel::Serializable,
                access: TxAccessMode::ReadOnly,
            },
        )
        .await
        .unwrap();
    assert!(
        execute(&f.driver, &c, "INSERT INTO seed VALUES(1,'denied')", vec![])
            .await
            .is_err()
    );
    f.driver.rollback(tx).await.unwrap();
    execute(
        &f.driver,
        &c,
        "INSERT INTO seed VALUES(1,'allowed')",
        vec![],
    )
    .await
    .unwrap();
    f.driver.close(c).await.unwrap();
}
#[tokio::test]
async fn file_admission_never_creates_or_escapes_roots() {
    let f = Fixture::new(1);
    assert!(f
        .driver
        .open_file(f.spec(SqliteOpenMode::ReadOnly), Some(2))
        .await
        .is_err());
    assert!(f
        .driver
        .open_file(f.spec(SqliteOpenMode::ReadOnly), None)
        .await
        .is_err());
    for path in [
        "absent.db",
        "../data.db",
        "/tmp/data.db",
        "file:data.db",
        ":memory:",
        "sub/../../data.db",
        "C:\\data.db",
    ] {
        let mut spec = f.spec(SqliteOpenMode::ReadWrite);
        spec.path = path.into();
        assert!(
            f.driver.open_file(spec, Some(1)).await.is_err(),
            "allowed {path}"
        );
    }
    assert!(!f.root.path().join("absent.db").exists());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            f.root.path().join("data.db"),
            f.root.path().join("alias.db"),
        )
        .unwrap();
        let mut spec = f.spec(SqliteOpenMode::ReadOnly);
        spec.path = "alias.db".into();
        assert!(f.driver.open_file(spec, Some(1)).await.is_err());
        std::fs::hard_link(f.root.path().join("data.db"), f.root.path().join("hard.db")).unwrap();
        let mut spec = f.spec(SqliteOpenMode::ReadOnly);
        spec.path = "hard.db".into();
        assert!(f.driver.open_file(spec, Some(1)).await.is_err());
    }
}
#[tokio::test]
async fn transactions_lock_contention_and_commit_busy_preserve_state() {
    let f = Fixture::new(2);
    let a = f.open(SqliteOpenMode::ReadWrite).await;
    let b = f.open(SqliteOpenMode::ReadWrite).await;
    assert!(f.driver.begin(a.clone(), TxMode::default()).await.is_err());
    let mode = TxMode {
        isolation: IsolationLevel::Serializable,
        access: TxAccessMode::ReadWrite,
    };
    let tx = f.driver.begin(a.clone(), mode).await.unwrap();
    execute(
        &f.driver,
        &a,
        "INSERT INTO seed VALUES(1,'pending')",
        vec![],
    )
    .await
    .unwrap();
    assert!(f.driver.begin(b.clone(), mode).await.is_err());
    let read = f
        .driver
        .begin(
            b.clone(),
            TxMode {
                access: TxAccessMode::ReadOnly,
                ..mode
            },
        )
        .await
        .unwrap();
    assert!(execute(&f.driver, &b, "SELECT * FROM seed", vec![])
        .await
        .unwrap()
        .0
        .is_empty());
    assert!(f.driver.commit(tx.clone()).await.is_err());
    f.driver.rollback(read).await.unwrap();
    f.driver.commit(tx).await.unwrap();
    assert_eq!(
        execute(&f.driver, &b, "SELECT * FROM seed", vec![])
            .await
            .unwrap()
            .0
            .len(),
        1
    );
    f.driver.close(a).await.unwrap();
    f.driver.close(b).await.unwrap();
}
#[tokio::test]
async fn cancel_stalled_and_running_queries_discards_without_leaking_capacity() {
    let f = Fixture::new(1);
    let c = f.open(SqliteOpenMode::ReadWrite).await;
    assert!(f
        .driver
        .open_file(f.spec(SqliteOpenMode::ReadOnly), Some(1))
        .await
        .is_err());
    let stream=f.driver.execute(c.clone(),ExecuteRequest::new("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10000000) SELECT x FROM n")).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert!(f
        .driver
        .execute(c.clone(), ExecuteRequest::new("SELECT 1"))
        .await
        .is_err());
    f.driver.cancel(c.clone(), stream.cursor_id).await.unwrap();
    assert!(f.driver.ping(c).await.is_err());
    drop(stream);
    let start = std::time::Instant::now();
    let reopened = loop {
        match f
            .driver
            .open_file(f.spec(SqliteOpenMode::ReadOnly), Some(1))
            .await
        {
            Ok(c) => break c,
            Err(e) => {
                assert_eq!(e.code, Code::PoolExhausted);
                assert!(start.elapsed() < std::time::Duration::from_secs(1));
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        }
    };
    let mut stream=f.driver.execute(reopened.clone(),ExecuteRequest::new("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<100000000) SELECT sum(x) FROM n")).await.unwrap();
    let cursor = stream.cursor_id;
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    f.driver.cancel(reopened, cursor).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while stream.rows.recv().await.is_some() {}
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn native_schema_ddl_and_external_refresh_preserve_file_semantics() {
    let f = Fixture::new(2);
    let c = f.open(SqliteOpenMode::ReadWrite).await;
    execute(&f.driver,&c,"CREATE TABLE rich(a TEXT NOT NULL,b INTEGER NOT NULL,c TEXT GENERATED ALWAYS AS (a||b) STORED,PRIMARY KEY(a,b),CHECK(b>0)) WITHOUT ROWID,STRICT; CREATE INDEX rich_idx ON rich(lower(a) DESC) WHERE b>1; CREATE TABLE nullable_pk(a TEXT,b TEXT,PRIMARY KEY(a,b));",vec![]).await.unwrap();
    let path = ObjectPath {
        name: "rich".into(),
        schema: Some("main".into()),
        catalog: None,
        kind: Some(ObjectKind::Table),
        routine_args: None,
    };
    let snapshot = f
        .driver
        .schema(c.clone(), SchemaScope::deep(path.clone()))
        .await
        .unwrap();
    let table = &snapshot.trees[0].schemas[0].objects[0];
    assert_eq!(table.columns[2].facets.sqlite.as_ref().unwrap().hidden, 3);
    assert!(table.indexes.iter().any(|i| i.partial_predicate.is_some()));
    let sql = f.driver.object_ddl(c.clone(), path).await.unwrap();
    assert!(sql.contains("WITHOUT ROWID,STRICT"));
    assert!(sql.contains("lower(a) DESC"));
    let other = rusqlite::Connection::open(f.root.path().join("roundtrip.db")).unwrap();
    other.execute_batch(&sql).unwrap();
    let actual: String = other
        .query_row("SELECT sql FROM sqlite_schema WHERE name='rich'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(sql.starts_with(&actual));
    let shallow = f
        .driver
        .schema(c.clone(), SchemaScope::shallow())
        .await
        .unwrap();
    let count = shallow.trees[0].schemas[0].objects.len();
    rusqlite::Connection::open(f.root.path().join("data.db"))
        .unwrap()
        .execute_batch("CREATE TABLE external(id);")
        .unwrap();
    assert_eq!(
        f.driver
            .schema(c.clone(), SchemaScope::shallow())
            .await
            .unwrap()
            .trees[0]
            .schemas[0]
            .objects
            .len(),
        count + 1
    );
    let snapshot = f
        .driver
        .schema(c.clone(), SchemaScope::deep(ObjectPath::new("nullable_pk")))
        .await
        .unwrap();
    assert_eq!(
        snapshot.trees[0].schemas[0].objects[0].columns[0].nullable,
        Nullability::Nullable
    );
    f.driver.close(c).await.unwrap();
}

#[tokio::test]
async fn bounded_large_stream_late_cancel_and_auto_rollback() {
    let f = Fixture::new(1);
    let c = f.open(SqliteOpenMode::ReadWrite).await;
    let started = std::time::Instant::now();
    let mut stream = f.driver.execute(c.clone(), ExecuteRequest::new("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<100000) SELECT x,printf('row-%d',x) FROM n")).await.unwrap();
    let cursor = stream.cursor_id;
    let mut count = 0;
    let mut done = false;
    while let Some(page) = stream.rows.recv().await {
        match page {
            Page::Rows { rows } => {
                assert!(rows.len() <= 128);
                count += rows.len();
            }
            Page::Done { .. } => done = true,
            Page::Error { error } => panic!("{error}"),
            _ => {}
        }
    }
    assert!(done);
    assert_eq!(count, 100000);
    eprintln!(
        "SQLite {}: 100000 rows in {:?}",
        rusqlite::version(),
        started.elapsed()
    );
    assert_eq!(
        f.driver.cancel(c.clone(), cursor).await.unwrap_err().code,
        Code::CursorNotFound
    );
    execute(&f.driver, &c, "INSERT INTO seed VALUES(1,'kept')", vec![])
        .await
        .unwrap();
    let tx = f
        .driver
        .begin(
            c.clone(),
            TxMode {
                isolation: IsolationLevel::Serializable,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    execute(
        &f.driver,
        &c,
        "INSERT INTO seed VALUES(2,'rollback')",
        vec![],
    )
    .await
    .unwrap();
    assert!(execute(
        &f.driver,
        &c,
        "INSERT OR ROLLBACK INTO seed VALUES(1,'duplicate')",
        vec![]
    )
    .await
    .is_err());
    assert!(f.driver.commit(tx).await.is_err());
    f.driver.close(c).await.unwrap();
    let reopened = f.open(SqliteOpenMode::ReadOnly).await;
    assert_eq!(
        execute(&f.driver, &reopened, "SELECT * FROM seed", vec![])
            .await
            .unwrap()
            .0
            .len(),
        1
    );
    assert!(
        execute(&f.driver, &reopened, "SELECT zeroblob(9000000)", vec![])
            .await
            .is_err()
    );
    assert_eq!(
        execute(
            &f.driver,
            &reopened,
            "SELECT zeroblob(5000000),zeroblob(5000000)",
            vec![]
        )
        .await
        .unwrap_err()
        .code,
        Code::ResultTooLarge
    );
    f.driver.close(reopened).await.unwrap();
}
