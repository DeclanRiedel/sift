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
    assert_eq!(
        snapshot.trees[0].schemas[0].objects[0].estimated_rows,
        Some(0)
    );
    assert_eq!(snapshot.trees[0].schemas[0].objects[0].modified_at, None);
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
async fn catalog_graph_links_generated_columns_and_expression_indexes() {
    let f = Fixture::new(2);
    rusqlite::Connection::open(f.root.path().join("data.db"))
        .unwrap()
        .execute_batch(
            "CREATE TABLE calc(a INTEGER, b INTEGER, c INTEGER GENERATED ALWAYS AS (a + abs(b)) STORED);
             CREATE INDEX calc_expression ON calc(lower(a), b + 1);",
        )
        .unwrap();
    rusqlite::Connection::open(f.root.path().join("data.db"))
        .unwrap()
        .execute_batch(&format!(
            "CREATE INDEX calc_oversized ON calc(abs(a /*{}*/))",
            "x".repeat(1_048_600)
        ))
        .unwrap();
    let c = f.open(SqliteOpenMode::ReadOnly).await;
    let graph = f
        .driver
        .schema(
            c.clone(),
            SchemaScope {
                depth: SchemaDepth::Graph {
                    options: CatalogGraphOptions::default(),
                },
                filter: None,
            },
        )
        .await
        .unwrap()
        .graph
        .unwrap();
    sift_core::catalog::validate_graph(&graph, 10_000, 100_000).unwrap();
    let table = graph
        .nodes
        .iter()
        .find(|node| node.kind == CatalogNodeKind::Table && node.name == "calc")
        .unwrap();
    let column = |name: &str| {
        graph
            .nodes
            .iter()
            .find(|node| {
                node.kind == CatalogNodeKind::Column
                    && node.name == name
                    && node.parent_id.as_ref() == Some(&table.id)
            })
            .unwrap()
    };
    let a = column("a");
    let b = column("b");
    let c_node = column("c");
    let index = graph
        .nodes
        .iter()
        .find(|node| {
            node.kind == CatalogNodeKind::Index
                && node.name == "calc_expression"
                && node.parent_id.as_ref() == Some(&table.id)
        })
        .unwrap();
    let oversized = graph
        .nodes
        .iter()
        .find(|node| {
            node.kind == CatalogNodeKind::Index
                && node.name == "calc_oversized"
                && node.parent_id.as_ref() == Some(&table.id)
        })
        .unwrap();
    for target in [&a.id, &b.id] {
        assert!(graph.edges.iter().any(|edge| edge.from == c_node.id
            && edge.to.as_ref() == Some(target)
            && edge.kind == CatalogEdgeKind::DependsOn
            && edge.certainty == CatalogEdgeCertainty::Parsed));
        assert!(graph.edges.iter().any(|edge| edge.from == index.id
            && edge.to.as_ref() == Some(target)
            && edge.kind == CatalogEdgeKind::DependsOn
            && edge.certainty == CatalogEdgeCertainty::Parsed));
    }
    assert!(!graph.coverage.failures.iter().any(|failure| failure.code
        == "sqlite_generated_expression_unparsed"
        || failure.code == "sqlite_index_expression_unparsed"));
    assert_eq!(
        oversized.extra.get("sqlite_dependency_gap"),
        Some(&serde_json::json!("sqlite_index_sql_limit"))
    );
    f.driver.close(c).await.unwrap();
}

#[tokio::test]
async fn catalog_graph_links_check_and_partial_index_columns_without_inventing_table_functions() {
    let f = Fixture::new(2);
    rusqlite::Connection::open(f.root.path().join("data.db"))
        .unwrap()
        .execute_batch(
            "CREATE TABLE measured(a INTEGER, b INTEGER, CHECK(a > b AND abs(b) > 0));
             CREATE INDEX measured_partial ON measured(a) WHERE b > 0;
             CREATE VIRTUAL TABLE geo USING rtree(id, min_x, max_x, min_y, max_y);
             CREATE VIEW function_source AS SELECT value FROM json_each('[1]');",
        )
        .unwrap();
    let c = f.open(SqliteOpenMode::ReadOnly).await;
    let graph = f
        .driver
        .schema(
            c.clone(),
            SchemaScope {
                depth: SchemaDepth::Graph {
                    options: CatalogGraphOptions::default(),
                },
                filter: None,
            },
        )
        .await
        .unwrap()
        .graph
        .unwrap();
    sift_core::catalog::validate_graph(&graph, 10_000, 100_000).unwrap();
    let table = graph
        .nodes
        .iter()
        .find(|node| node.kind == CatalogNodeKind::Table && node.name == "measured")
        .unwrap();
    let a = graph
        .nodes
        .iter()
        .find(|node| {
            node.kind == CatalogNodeKind::Column
                && node.name == "a"
                && node.parent_id.as_ref() == Some(&table.id)
        })
        .unwrap();
    let b = graph
        .nodes
        .iter()
        .find(|node| {
            node.kind == CatalogNodeKind::Column
                && node.name == "b"
                && node.parent_id.as_ref() == Some(&table.id)
        })
        .unwrap();
    let check = graph.nodes.iter().find(|node| node.kind == CatalogNodeKind::Constraint && node.parent_id.as_ref() == Some(&table.id) && matches!(&node.details, CatalogNodeDetails::Constraint { constraint } if constraint.kind == ConstraintKind::Check)).unwrap();
    let index = graph
        .nodes
        .iter()
        .find(|node| node.kind == CatalogNodeKind::Index && node.name == "measured_partial")
        .unwrap();
    for target in [&a.id, &b.id] {
        assert!(graph.edges.iter().any(|edge| edge.from == check.id
            && edge.to.as_ref() == Some(target)
            && edge.kind == CatalogEdgeKind::DependsOn
            && edge.certainty == CatalogEdgeCertainty::Parsed));
    }
    assert!(graph.edges.iter().any(|edge| edge.from == index.id
        && edge.to.as_ref() == Some(&b.id)
        && edge.kind == CatalogEdgeKind::DependsOn
        && edge.certainty == CatalogEdgeCertainty::Parsed));
    let view = graph
        .nodes
        .iter()
        .find(|node| node.kind == CatalogNodeKind::View && node.name == "function_source")
        .unwrap();
    assert_eq!(
        view.extra.get("sqlite_dependency_gap"),
        Some(&serde_json::json!("view_table_function_unavailable"))
    );
    assert!(!graph
        .edges
        .iter()
        .any(|edge| edge.from == view.id && edge.kind == CatalogEdgeKind::ReadsFrom));
    let geo = graph
        .nodes
        .iter()
        .find(|node| node.kind == CatalogNodeKind::Table && node.name == "geo")
        .unwrap();
    assert_eq!(
        geo.extra.get("sqlite_dependency_gap"),
        Some(&serde_json::json!("virtual_table_dependencies_unavailable"))
    );
    assert_eq!(
        geo.extra.get("sqlite_virtual_module"),
        Some(&serde_json::json!("rtree"))
    );
    f.driver.close(c).await.unwrap();
}

#[tokio::test]
async fn catalog_graph_keeps_sqlite_dependencies_schema_correct_and_partial() {
    let f = Fixture::new(2);
    rusqlite::Connection::open(f.root.path().join("data.db"))
        .unwrap()
        .execute_batch(
            "CREATE TABLE parent(id INTEGER PRIMARY KEY);
             CREATE TABLE child(id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES parent(id));
             CREATE TABLE orphan(id INTEGER, missing_id INTEGER REFERENCES temp_only(id));
             CREATE TABLE external_content(body TEXT);
             CREATE VIRTUAL TABLE docs USING fts5(body, content='external_content');
             CREATE VIRTUAL TABLE orphan_docs USING fts5(body, content='missing_content');
             CREATE VIEW joined AS SELECT c.parent_id FROM child AS c JOIN parent AS p ON p.id=c.parent_id;
             CREATE VIEW with_cte AS WITH x AS (SELECT id FROM child) SELECT id FROM x;
             CREATE TRIGGER child_ai AFTER INSERT ON child BEGIN UPDATE parent SET id=NEW.parent_id WHERE id=NEW.parent_id; END;",
        )
        .unwrap();
    let c = f.open(SqliteOpenMode::ReadWrite).await;
    execute(
        &f.driver,
        &c,
        "CREATE TEMP TABLE temp_only(id INTEGER PRIMARY KEY)",
        vec![],
    )
    .await
    .unwrap();
    let snapshot = f
        .driver
        .schema(
            c.clone(),
            SchemaScope {
                depth: SchemaDepth::Graph {
                    options: CatalogGraphOptions::default(),
                },
                filter: None,
            },
        )
        .await
        .unwrap();
    let graph = snapshot.graph.unwrap();
    sift_core::catalog::validate_graph(&graph, 10_000, 100_000).unwrap();

    let node = |schema: &str, name: &str, kind: CatalogNodeKind| {
        let schema_node = graph
            .nodes
            .iter()
            .find(|node| node.kind == CatalogNodeKind::Schema && node.name == schema)
            .unwrap();
        graph
            .nodes
            .iter()
            .find(|node| {
                node.kind == kind
                    && node.name == name
                    && node.parent_id.as_ref() == Some(&schema_node.id)
            })
            .unwrap()
            .id
            .clone()
    };
    let parent = node("main", "parent", CatalogNodeKind::Table);
    let external_content = node("main", "external_content", CatalogNodeKind::Table);
    let docs = node("main", "docs", CatalogNodeKind::Table);
    let orphan_docs = node("main", "orphan_docs", CatalogNodeKind::Table);
    assert_eq!(
        graph
            .nodes
            .iter()
            .find(|node| node.id == docs)
            .unwrap()
            .extra
            .get("sqlite_metadata_gap"),
        Some(&serde_json::json!("virtual_table_columns_unavailable"))
    );
    let child = node("main", "child", CatalogNodeKind::Table);
    let joined = node("main", "joined", CatalogNodeKind::View);
    let with_cte = node("main", "with_cte", CatalogNodeKind::View);
    let trigger = node("main", "child_ai", CatalogNodeKind::Trigger);
    let orphan = node("main", "orphan", CatalogNodeKind::Table);
    let fk = graph
        .nodes
        .iter()
        .find(|node| {
            node.kind == CatalogNodeKind::Constraint
                && node.name == "foreign_key_0"
                && node.parent_id.as_ref() == Some(&child)
        })
        .unwrap();
    let orphan_fk = graph
        .nodes
        .iter()
        .find(|node| {
            node.kind == CatalogNodeKind::Constraint
                && node.name == "foreign_key_0"
                && node.parent_id.as_ref() == Some(&orphan)
        })
        .unwrap();
    assert!(graph.edges.iter().any(|edge| edge.from == fk.id
        && edge.kind == CatalogEdgeKind::ForeignKey
        && edge.to.as_ref() == Some(&parent)
        && edge.certainty == CatalogEdgeCertainty::CatalogProven));
    assert!(graph.edges.iter().any(|edge| edge.from == orphan_fk.id
        && edge.kind == CatalogEdgeKind::ForeignKey
        && edge.to.is_none()
        && edge.certainty == CatalogEdgeCertainty::Unresolved
        && edge.referenced_path.as_deref() == Some("main.temp_only")));
    assert!(graph.edges.iter().any(|edge| edge.from == trigger
        && edge.kind == CatalogEdgeKind::TriggerOn
        && edge.to.as_ref() == Some(&child)
        && edge.certainty == CatalogEdgeCertainty::CatalogProven));
    for target in [&parent, &child] {
        assert!(graph.edges.iter().any(|edge| edge.from == joined
            && edge.kind == CatalogEdgeKind::ReadsFrom
            && edge.to.as_ref() == Some(target)
            && edge.certainty == CatalogEdgeCertainty::Parsed));
    }
    assert_eq!(
        graph
            .edges
            .iter()
            .filter(|edge| edge.from == with_cte && edge.kind == CatalogEdgeKind::ReadsFrom)
            .count(),
        1
    );
    assert_eq!(graph.coverage.state, CatalogCoverageState::Partial);
    assert!(graph.edges.iter().any(|edge| edge.from == trigger
        && edge.kind == CatalogEdgeKind::DependsOn
        && edge.to.as_ref() == Some(&parent)
        && edge.certainty == CatalogEdgeCertainty::Parsed));
    assert!(graph.edges.iter().any(|edge| edge.from == docs
        && edge.kind == CatalogEdgeKind::DependsOn
        && edge.to.as_ref() == Some(&external_content)
        && edge.certainty == CatalogEdgeCertainty::Parsed));
    assert!(graph.edges.iter().any(|edge| edge.from == orphan_docs
        && edge.kind == CatalogEdgeKind::DependsOn
        && edge.certainty == CatalogEdgeCertainty::Unresolved
        && edge.referenced_path.as_deref() == Some("missing_content")));
    execute(
        &f.driver,
        &c,
        "CREATE TEMP VIEW temp_parent AS SELECT id FROM parent",
        vec![],
    )
    .await
    .unwrap();
    let filtered = f
        .driver
        .schema(
            c.clone(),
            SchemaScope {
                depth: SchemaDepth::Graph {
                    options: CatalogGraphOptions {
                        schemas: Some(vec!["temp".into()]),
                        ..CatalogGraphOptions::default()
                    },
                },
                filter: None,
            },
        )
        .await
        .unwrap();
    let filtered = filtered.graph.unwrap();
    sift_core::catalog::validate_graph(&filtered, 10_000, 100_000).unwrap();
    let temp_view = filtered
        .nodes
        .iter()
        .find(|node| node.kind == CatalogNodeKind::View && node.name == "temp_parent")
        .unwrap();
    assert!(filtered.edges.iter().any(|edge| edge.from == temp_view.id
        && edge.kind == CatalogEdgeKind::ReadsFrom
        && edge.certainty == CatalogEdgeCertainty::Unresolved
        && edge.referenced_path.as_deref() == Some("parent")));
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
    assert!(table
        .constraints
        .iter()
        .any(|c| { c.kind == ConstraintKind::Check && c.definition.as_deref() == Some("b>0") }));
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
async fn native_check_metadata_keeps_names_scopes_and_expression_text() {
    let f = Fixture::new(1);
    let c = f.open(SqliteOpenMode::ReadWrite).await;
    let ddl = r#"CREATE TABLE checks(
        "CHECK" TEXT CHECK (length("CHECK") > 0),
        amount INTEGER CONSTRAINT "amount positive" CHECK (amount > 0) CHECK (amount < 10),
        note TEXT CHECK (note <> 'CHECK (fake, value)') /* CHECK (ignored) */,
        CONSTRAINT [table check] CHECK (length(note) > 1 AND amount IN (1,2,3))
    ) STRICT"#;
    execute(&f.driver, &c, ddl, vec![]).await.unwrap();
    let path = ObjectPath::new("checks");
    let snapshot = f
        .driver
        .schema(c.clone(), SchemaScope::deep(path.clone()))
        .await
        .unwrap();
    let checks = &snapshot.trees[0].schemas[0].objects[0].constraints;
    assert_eq!(checks.len(), 5);
    assert_eq!(checks[0].name, "CHECK CHECK");
    assert_eq!(checks[0].columns, ["CHECK"]);
    assert_eq!(
        checks[0].definition.as_deref(),
        Some("length(\"CHECK\") > 0")
    );
    assert_eq!(checks[1].name, "amount positive");
    assert_eq!(checks[1].columns, ["amount"]);
    assert_eq!(checks[1].definition.as_deref(), Some("amount > 0"));
    assert_eq!(checks[2].name, "CHECK amount");
    assert_eq!(checks[2].columns, ["amount"]);
    assert_eq!(checks[2].definition.as_deref(), Some("amount < 10"));
    assert_eq!(checks[3].name, "CHECK note");
    assert_eq!(checks[3].columns, ["note"]);
    assert_eq!(
        checks[3].definition.as_deref(),
        Some("note <> 'CHECK (fake, value)'")
    );
    assert_eq!(checks[4].name, "table check");
    assert!(checks[4].columns.is_empty());
    assert_eq!(
        checks[4].definition.as_deref(),
        Some("length(note) > 1 AND amount IN (1,2,3)")
    );
    assert!(execute(
        &f.driver,
        &c,
        "INSERT INTO checks(amount,note) VALUES (0,'valid')",
        vec![]
    )
    .await
    .is_err());
    assert!(f
        .driver
        .object_ddl(c.clone(), path)
        .await
        .unwrap()
        .contains(ddl));
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

#[tokio::test]
async fn large_catalog_graph_and_mixed_storage_classes_remain_bounded() {
    let f = Fixture::new(1);
    let db = rusqlite::Connection::open(f.root.path().join("data.db")).unwrap();
    let mut ddl = String::from("BEGIN;");
    for index in 0_usize..192 {
        let table = format!("wide_{index:03}");
        let parent = format!("wide_{:03}", index.saturating_sub(1));
        ddl.push_str(&format!(
            "CREATE TABLE {table}(id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES {parent}(id), payload); CREATE INDEX {table}_parent ON {table}(parent_id);"
        ));
    }
    ddl.push_str("INSERT INTO wide_000(payload) VALUES (7),('text'),(x'00FF'),(NULL); COMMIT;");
    db.execute_batch(&ddl).unwrap();
    drop(db);

    let c = f.open(SqliteOpenMode::ReadOnly).await;
    let started = std::time::Instant::now();
    let graph = f
        .driver
        .schema(
            c.clone(),
            SchemaScope {
                depth: SchemaDepth::Graph {
                    options: CatalogGraphOptions::default(),
                },
                filter: None,
            },
        )
        .await
        .unwrap()
        .graph
        .unwrap();
    let graph_elapsed = started.elapsed();
    assert_eq!(graph.coverage.truncated_at_nodes, None);
    assert_eq!(
        graph
            .nodes
            .iter()
            .filter(|node| node.kind == CatalogNodeKind::Table)
            .count(),
        193
    );
    sift_core::catalog::validate_graph(&graph, 10_000, 100_000).unwrap();

    let started = std::time::Instant::now();
    let (rows, _, _) = execute(
        &f.driver,
        &c,
        "SELECT payload FROM wide_000 ORDER BY id",
        vec![],
    )
    .await
    .unwrap();
    let query_elapsed = started.elapsed();
    assert_eq!(
        rows.iter().map(|row| &row.values[0]).collect::<Vec<_>>(),
        vec![
            &Value::Int64(7),
            &Value::Text("text".into()),
            &Value::Blob(vec![0, 255]),
            &Value::Null,
        ]
    );
    eprintln!(
        "SQLite {}: 193-table graph in {:?}, 4 mixed-class rows in {:?}; {} nodes, {} edges",
        rusqlite::version(),
        graph_elapsed,
        query_elapsed,
        graph.nodes.len(),
        graph.edges.len()
    );
    f.driver.close(c).await.unwrap();
}

#[tokio::test]
async fn concurrent_file_creation_claims_destination_once() {
    let f = Fixture::new(2);
    let first = f.open(SqliteOpenMode::ReadWrite).await;
    let second = f.open(SqliteOpenMode::ReadWrite).await;
    let action = SqliteMaintenanceAction::Create {
        path: "created.db".into(),
    };
    let first_preview = f
        .driver
        .inspect_file_maintenance(first.clone(), 1, action.clone())
        .await
        .unwrap();
    let second_preview = f
        .driver
        .inspect_file_maintenance(second.clone(), 1, action.clone())
        .await
        .unwrap();
    assert_eq!(first_preview.identity, second_preview.identity);
    let (first_result, second_result) = tokio::join!(
        f.driver.apply_file_maintenance(
            first.clone(),
            1,
            action.clone(),
            first_preview.identity,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        ),
        f.driver.apply_file_maintenance(
            second.clone(),
            1,
            action,
            second_preview.identity,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
    );
    assert!(first_result.is_ok() ^ second_result.is_ok());
    let created = rusqlite::Connection::open_with_flags(
        f.root.path().join("created.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(
        created
            .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    f.driver.close(first).await.unwrap();
    f.driver.close(second).await.unwrap();
}

#[tokio::test]
async fn concurrent_catalog_work_and_first_query_share_worker() {
    let f = Fixture::new(1);
    let c = f.open(SqliteOpenMode::ReadOnly).await;
    for _ in 0..20 {
        let (ping, query) = tokio::join!(
            f.driver.ping(c.clone()),
            execute(&f.driver, &c, "SELECT 42", vec![])
        );
        ping.unwrap();
        assert_eq!(query.unwrap().0.len(), 1);
    }
    f.driver.close(c).await.unwrap();
}

#[tokio::test]
async fn seeded_trigger_ddl_is_readable_without_executing_it() {
    let f = Fixture::new(1);
    rusqlite::Connection::open(f.root.path().join("data.db"))
        .unwrap()
        .execute_batch(include_str!(
            "../../../examples/reproducible-instance/sql/sqlite-demo.sql"
        ))
        .unwrap();
    let c = f.open(SqliteOpenMode::ReadOnly).await;
    let object = ObjectPath {
        catalog: None,
        schema: Some("main".into()),
        name: "record_order_status".into(),
        kind: Some(ObjectKind::Trigger),
        routine_args: None,
    };
    let ddl = f.driver.object_ddl(c.clone(), object).await.unwrap();
    assert!(ddl.contains("CREATE TRIGGER record_order_status"));
    assert!(ddl.contains("INSERT INTO order_changes"));
    // Opening a stored CREATE statement is inspection, not another execution.

    let (rows, _, _) = execute(&f.driver, &c, "SELECT count(*) FROM order_changes", vec![])
        .await
        .unwrap();
    assert_eq!(rows[0].values, vec![Value::Int64(0)]);
    f.driver.close(c).await.unwrap();
}
