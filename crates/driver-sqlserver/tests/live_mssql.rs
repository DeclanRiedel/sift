//! Integration tests against a real SQL Server instance. Gated behind the
//! `live-mssql` feature so CI runs without Docker/SQL Server by default.
//!
//! Required env:
//! - `SIFT_MSSQL_HOST` (default `127.0.0.1`)
//! - `SIFT_MSSQL_PORT` (default `1433`)
//! - `SIFT_MSSQL_USER` (default `sa`)
//! - `SIFT_MSSQL_PASSWORD` (required)
//! - `SIFT_MSSQL_DB` (default `master`)

#![cfg(feature = "live-mssql")]

use sift_driver_api::{BulkOp, Driver, MssqlExt};
use sift_driver_sqlserver::MssqlDriver;
use sift_protocol::{
    CatalogEdgeKind, CatalogGraphOptions, CatalogNodeKind, ConnectionSpec, Engine,
    EngineConnectionSpec, ExecuteRequest, MssqlConnectionSpec, ObjectPath, Page, PrimitiveType,
    SchemaDepth, SchemaScope, SslMode, TxMode, TypeRef, Value,
};

fn spec() -> ConnectionSpec {
    ConnectionSpec {
        host: std::env::var("SIFT_MSSQL_HOST").unwrap_or_else(|_| "127.0.0.1".into()),
        port: Some(
            std::env::var("SIFT_MSSQL_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(1433),
        ),
        database: Some(std::env::var("SIFT_MSSQL_DB").unwrap_or_else(|_| "master".into())),
        user: std::env::var("SIFT_MSSQL_USER").unwrap_or_else(|_| "sa".into()),
        password: Some(std::env::var("SIFT_MSSQL_PASSWORD").expect("SIFT_MSSQL_PASSWORD required")),
        ssl_mode: Some(SslMode::Require),
        engine_specific: Some(EngineConnectionSpec::SqlServer(MssqlConnectionSpec {
            mars: false,
            encrypt: Some(true),
            trust_server_certificate: Some(true),
            connect_timeout_secs: Some(15),
            pool_min_size: None,
        })),
    }
}

async fn drain(mut stream: sift_driver_api::ResultSetStream) -> Vec<Page> {
    let mut pages = Vec::new();
    while let Some(page) = stream.rows.recv().await {
        pages.push(page);
    }
    pages
}

#[tokio::test]
async fn open_ping_execute_close() {
    let driver = MssqlDriver::new();
    let conn = driver.open(&spec()).await.expect("open succeeds");
    let info = driver.ping(conn.clone()).await.expect("ping succeeds");
    assert_eq!(info.provider.provider_id, Engine::SqlServer.provider_id());

    let pages = drain(
        driver
            .execute(
                conn.clone(),
                ExecuteRequest {
                    sql: "SELECT CAST(@P1 AS int) AS id, CAST(@P2 AS nvarchar(20)) AS name".into(),
                    params: vec![Value::Int32(7), Value::Text("seven".into())],
                },
            )
            .await
            .expect("execute succeeds"),
    )
    .await;

    let cols = pages
        .iter()
        .find_map(|p| match p {
            Page::NextResult { columns } => Some(columns),
            _ => None,
        })
        .expect("columns sent");
    assert_eq!(cols.len(), 2);
    assert!(matches!(
        cols[0].type_ref,
        TypeRef::Primitive(PrimitiveType::Int32)
    ));

    let rows: Vec<_> = pages
        .iter()
        .filter_map(|p| match p {
            Page::Rows { rows } => Some(rows),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(rows.len(), 1);
    assert!(matches!(rows[0].values[0], Value::Int32(7)));
    assert!(matches!(&rows[0].values[1], Value::Text(v) if v == "seven"));

    driver.close(conn).await.expect("close succeeds");
}

#[tokio::test]
async fn typed_null_parameters_preserve_sql_server_native_types() {
    let driver = MssqlDriver::new();
    let conn = driver.open(&spec()).await.expect("open succeeds");
    let pages = drain(
        driver
            .execute(
                conn.clone(),
                ExecuteRequest {
                    sql: "SELECT CAST(@P1 AS varbinary(max)), CAST(@P2 AS int)".into(),
                    params: vec![
                        Value::TypedNull {
                            type_name: "varbinary(max)".into(),
                        },
                        Value::TypedNull {
                            type_name: "int".into(),
                        },
                    ],
                },
            )
            .await
            .expect("execute typed nulls"),
    )
    .await;
    let row = pages
        .iter()
        .find_map(|page| match page {
            Page::Rows { rows } => rows.first(),
            _ => None,
        })
        .expect("one null row");
    assert_eq!(row.values, vec![Value::Null, Value::Null]);
    driver.close(conn).await.expect("close succeeds");
}

#[tokio::test]
async fn bulk_insert_csv_round_trip() {
    let driver = MssqlDriver::new();
    let conn = driver.open(&spec()).await.expect("open succeeds");
    let table = format!(
        "sift_bulk_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    );

    drain(
        driver
            .execute(
                conn.clone(),
                ExecuteRequest::new(format!(
                    "CREATE TABLE dbo.[{table}] (id int NOT NULL, name nvarchar(64) NULL)"
                )),
            )
            .await
            .expect("create bulk table"),
    )
    .await;

    let result = driver
        .bulk_insert(
            conn.clone(),
            BulkOp {
                table: format!("dbo.{table}"),
                data: b"id,name\n1,Alice\n2,\"Bob, Jr\"\n3,\n".to_vec(),
                delimiter: b',',
                header: true,
                null_value: None,
            },
        )
        .await
        .expect("bulk insert");
    assert_eq!(result.rows_inserted, 3);

    // Verify each row landed with the correct value. Post-P1-#9,
    // mssql_literal("") emits N'' (empty string), not NULL — so the
    // third row's name is '', not NULL. Count matches for Bob (comma
    // quoting) OR the empty-string row.
    let pages = drain(
        driver
            .execute(
                conn.clone(),
                ExecuteRequest::new(format!(
                    "SELECT COUNT(*) AS ct FROM dbo.[{table}] WHERE name = N'' OR name LIKE N'Bob,%'"
                )),
            )
            .await
            .expect("select bulk rows"),
    )
    .await;
    let count = pages.iter().find_map(|p| match p {
        Page::Rows { rows } => rows.first().and_then(|row| row.values.first()),
        _ => None,
    });
    assert!(matches!(count, Some(Value::Int32(2))));

    drain(
        driver
            .execute(
                conn.clone(),
                ExecuteRequest::new(format!("DROP TABLE dbo.[{table}]")),
            )
            .await
            .expect("drop bulk table"),
    )
    .await;
    driver.close(conn).await.expect("close succeeds");
}

#[tokio::test]
async fn cancel_long_query() {
    let driver = MssqlDriver::new();
    let conn = driver.open(&spec()).await.expect("open succeeds");
    let stream = driver
        .execute(
            conn.clone(),
            ExecuteRequest {
                sql: "WAITFOR DELAY '00:00:05'; SELECT 1 AS done".into(),
                params: Vec::new(),
            },
        )
        .await
        .expect("execute starts");

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    driver
        .cancel(conn.clone(), stream.cursor_id)
        .await
        .expect("cancel succeeds");
    driver.close(conn).await.expect("close is idempotent");
}

#[tokio::test]
async fn close_mid_query_drops_cursor_and_connection() {
    let driver = MssqlDriver::new();
    let conn = driver.open(&spec()).await.expect("open succeeds");
    let _stream = driver
        .execute(
            conn.clone(),
            ExecuteRequest {
                sql: "WAITFOR DELAY '00:00:05'; SELECT 1 AS done".into(),
                params: Vec::new(),
            },
        )
        .await
        .expect("execute starts");

    driver.close(conn.clone()).await.expect("close succeeds");
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        driver.ping(conn).await.is_err(),
        "closed connection must not be resurrected by query task"
    );
}

#[tokio::test]
async fn schema_deep_and_transactions() {
    let driver = MssqlDriver::new();
    let conn = driver.open(&spec()).await.expect("open succeeds");
    let table = format!(
        "sift_phase0_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    );

    let setup =
        format!("CREATE TABLE dbo.[{table}] (id int NOT NULL PRIMARY KEY, name nvarchar(64) NULL)");
    drain(
        driver
            .execute(conn.clone(), ExecuteRequest::new(setup))
            .await
            .expect("create table"),
    )
    .await;

    let shallow = driver
        .schema(conn.clone(), SchemaScope::shallow())
        .await
        .expect("shallow schema");
    assert!(shallow
        .trees
        .iter()
        .flat_map(|tree| &tree.schemas)
        .flat_map(|schema| &schema.objects)
        .any(|object| object.name == table));

    let deep = driver
        .schema(
            conn.clone(),
            SchemaScope::deep(ObjectPath {
                catalog: None,
                schema: Some("dbo".into()),
                name: table.clone(),
                kind: None,
                routine_args: None,
            }),
        )
        .await
        .expect("deep schema");
    let object = &deep.trees[0].schemas[0].objects[0];
    assert!(object
        .columns
        .iter()
        .any(|c| c.name == "id" && c.primary_key));
    assert!(object.indexes.iter().any(|idx| idx.primary_key));
    assert!(object
        .constraints
        .iter()
        .any(|constraint| constraint.columns.iter().any(|c| c == "id")));

    let tx = driver
        .begin(conn.clone(), TxMode::default())
        .await
        .expect("begin");
    drain(
        driver
            .execute(
                conn.clone(),
                ExecuteRequest::new(format!(
                    "INSERT INTO dbo.[{table}] (id, name) VALUES (1, N'a')"
                )),
            )
            .await
            .expect("insert in tx"),
    )
    .await;
    driver.rollback(tx).await.expect("rollback");

    let pages = drain(
        driver
            .execute(
                conn.clone(),
                ExecuteRequest::new(format!("SELECT COUNT(*) AS ct FROM dbo.[{table}]")),
            )
            .await
            .expect("count after rollback"),
    )
    .await;
    let count = pages.iter().find_map(|p| match p {
        Page::Rows { rows } => rows.first().and_then(|row| row.values.first()),
        _ => None,
    });
    assert!(matches!(count, Some(Value::Int32(0))));

    drain(
        driver
            .execute(
                conn.clone(),
                ExecuteRequest::new(format!("DROP TABLE dbo.[{table}]")),
            )
            .await
            .expect("drop table"),
    )
    .await;
    driver.close(conn).await.expect("close succeeds");
}

#[tokio::test]
async fn schema_graph_preserves_native_composite_foreign_key_identity() {
    let driver = MssqlDriver::new();
    let conn = driver.open(&spec()).await.expect("open succeeds");
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let parent = format!("sift_graph_parent_{}", &suffix[..8]);
    let child = format!("sift_graph_child_{}", &suffix[..8]);
    let user_type = format!("sift_graph_type_{}", &suffix[..8]);
    let sequence = format!("sift_graph_sequence_{}", &suffix[..8]);
    let view = format!("sift_graph_view_{}", &suffix[..8]);
    let function = format!("sift_graph_function_{}", &suffix[..8]);
    let procedure = format!("sift_graph_procedure_{}", &suffix[..8]);
    let synonym = format!("sift_graph_synonym_{}", &suffix[..8]);
    let explicit_index = format!("idx_{child}_ab");
    let quoted_table = format!("sift graph ü {}", &suffix[..8]);
    let quoted_trigger = format!("trg ü {}", &suffix[..8]);
    for sql in [
        format!("CREATE TYPE dbo.[{user_type}] FROM int"),
        format!("CREATE SEQUENCE dbo.[{sequence}] AS bigint START WITH 1"),
        format!("CREATE TABLE dbo.[{parent}] (a int NOT NULL, b int NOT NULL, CONSTRAINT [pk_{parent}] PRIMARY KEY (a,b))"),
        format!("CREATE TABLE dbo.[{child}] (a int NOT NULL, b int NOT NULL, typed_value dbo.[{user_type}] NULL, sequence_value bigint DEFAULT NEXT VALUE FOR dbo.[{sequence}], CONSTRAINT [fk_{child}] FOREIGN KEY (a,b) REFERENCES dbo.[{parent}] (a,b))"),
        format!("CREATE INDEX [{explicit_index}] ON dbo.[{child}] (a,b)"),
        format!("CREATE TABLE dbo.[{quoted_table}] ([clé] int NOT NULL PRIMARY KEY)"),
        format!("EXEC(N'CREATE TRIGGER dbo.[{quoted_trigger}] ON dbo.[{quoted_table}] AFTER INSERT AS BEGIN SET NOCOUNT ON; END')"),
        format!("EXEC(N'CREATE FUNCTION dbo.[{function}](@value int) RETURNS int AS BEGIN RETURN @value END')"),
        format!("EXEC(N'CREATE PROCEDURE dbo.[{procedure}] AS SELECT dbo.[{function}](a) FROM dbo.[{parent}]')"),
        format!("CREATE SYNONYM dbo.[{synonym}] FOR dbo.[{parent}]"),
        format!("EXEC(N'CREATE VIEW dbo.[{view}] AS SELECT a,b FROM dbo.[{parent}]')"),
    ] {
        let pages = drain(
            driver
                .execute(conn.clone(), ExecuteRequest::new(sql))
                .await
                .expect("create graph fixture"),
        )
        .await;
        assert!(
            pages.iter().all(|page| !matches!(page, Page::Error { .. })),
            "graph fixture DDL failed: {pages:?}"
        );
    }

    let snapshot = driver
        .schema(
            conn.clone(),
            SchemaScope {
                depth: SchemaDepth::Graph {
                    options: CatalogGraphOptions {
                        schemas: Some(vec!["dbo".into()]),
                        ..Default::default()
                    },
                },
                filter: None,
            },
        )
        .await
        .expect("graph schema");
    let graph = snapshot.graph.expect("graph payload");
    let parent_node = graph
        .nodes
        .iter()
        .find(|node| node.kind == CatalogNodeKind::Table && node.name == parent)
        .expect("parent table node");
    let child_node = graph
        .nodes
        .iter()
        .find(|node| node.kind == CatalogNodeKind::Table && node.name == child)
        .expect("child table node");
    assert!(parent_node
        .native_id
        .as_deref()
        .is_some_and(|id| id.starts_with("mssql:object:")));
    let fk = graph
        .edges
        .iter()
        .find(|edge| {
            edge.kind == CatalogEdgeKind::ForeignKey
                && graph
                    .nodes
                    .iter()
                    .find(|node| node.id == edge.from)
                    .is_some_and(|node| node.name == format!("fk_{child}"))
        })
        .expect("foreign-key edge");
    assert_eq!(fk.to.as_ref(), Some(&parent_node.id));
    assert_eq!(fk.column_pairs.len(), 2);
    let type_node = graph
        .nodes
        .iter()
        .find(|node| node.kind == CatalogNodeKind::Type && node.name == user_type)
        .expect("user-defined type node");
    assert!(type_node
        .native_id
        .as_deref()
        .is_some_and(|id| id.starts_with("mssql:type:")));
    let view_node = graph
        .nodes
        .iter()
        .find(|node| node.kind == CatalogNodeKind::View && node.name == view)
        .expect("view node");
    assert!(graph.edges.iter().any(|edge| {
        edge.from == view_node.id
            && edge.to.as_ref() == Some(&parent_node.id)
            && edge.kind == CatalogEdgeKind::ReadsFrom
    }));
    let typed_column = graph
        .nodes
        .iter()
        .find(|node| {
            node.kind == CatalogNodeKind::Column
                && node.name == "typed_value"
                && node.parent_id.as_ref() == Some(&child_node.id)
        })
        .expect("typed column node");
    assert!(
        graph.edges.iter().any(|edge| {
            edge.from == typed_column.id
                && edge.to.as_ref() == Some(&type_node.id)
                && edge.kind == CatalogEdgeKind::UsesType
        }),
        "typed column id={:?} native={:?}, type id={:?} native={:?}, relevant edges={:?}, edge targets={:?}",
        typed_column.id,
        typed_column.native_id,
        type_node.id,
        type_node.native_id,
        graph
            .edges
            .iter()
            .filter(|edge| edge.from == typed_column.id)
            .collect::<Vec<_>>(),
        graph
            .edges
            .iter()
            .filter(|edge| edge.from == typed_column.id)
            .filter_map(|edge| edge.to.as_ref())
            .filter_map(|id| graph.nodes.iter().find(|node| &node.id == id))
            .map(|node| (&node.id, &node.name, &node.native_id))
            .collect::<Vec<_>>()
    );
    let sequence_node = graph
        .nodes
        .iter()
        .find(|node| node.kind == CatalogNodeKind::Sequence && node.name == sequence)
        .expect("sequence node");
    let sequence_column = graph
        .nodes
        .iter()
        .find(|node| {
            node.kind == CatalogNodeKind::Column
                && node.name == "sequence_value"
                && node.parent_id.as_ref() == Some(&child_node.id)
        })
        .expect("sequence-backed column node");
    assert!(graph.edges.iter().any(|edge| {
        edge.from == sequence_column.id
            && edge.to.as_ref() == Some(&sequence_node.id)
            && edge.kind == CatalogEdgeKind::OwnsSequence
    }));
    let explicit_index_node = graph
        .nodes
        .iter()
        .find(|node| node.kind == CatalogNodeKind::Index && node.name == explicit_index)
        .expect("explicit composite index node");
    assert!(graph.edges.iter().any(|edge| {
        edge.from == explicit_index_node.id
            && edge.to.as_ref() == Some(&child_node.id)
            && edge.kind == CatalogEdgeKind::Indexes
    }));
    let quoted_table_node = graph
        .nodes
        .iter()
        .find(|node| node.kind == CatalogNodeKind::Table && node.name == quoted_table)
        .expect("quoted UTF-8 table node");
    assert!(graph.nodes.iter().any(|node| {
        node.kind == CatalogNodeKind::Column
            && node.name == "clé"
            && node.parent_id.as_ref() == Some(&quoted_table_node.id)
    }));
    let trigger_node = graph
        .nodes
        .iter()
        .find(|node| node.kind == CatalogNodeKind::Trigger && node.name == quoted_trigger)
        .expect("quoted UTF-8 trigger node");
    assert!(graph.edges.iter().any(|edge| {
        edge.from == trigger_node.id
            && edge.to.as_ref() == Some(&quoted_table_node.id)
            && edge.kind == CatalogEdgeKind::TriggerOn
    }));
    let function_node = graph
        .nodes
        .iter()
        .find(|node| node.kind == CatalogNodeKind::ScalarFunction && node.name == function)
        .expect("scalar function node");
    assert!(function_node
        .native_id
        .as_deref()
        .is_some_and(|id| id.starts_with("mssql:object:")));
    let procedure_node = graph
        .nodes
        .iter()
        .find(|node| node.kind == CatalogNodeKind::Procedure && node.name == procedure)
        .expect("procedure node");
    assert!(graph.edges.iter().any(|edge| {
        edge.from == procedure_node.id
            && edge.to.as_ref() == Some(&function_node.id)
            && edge.kind == CatalogEdgeKind::Calls
    }));
    assert!(graph.edges.iter().any(|edge| {
        edge.from == procedure_node.id
            && edge.to.as_ref() == Some(&parent_node.id)
            && edge.kind == CatalogEdgeKind::DependsOn
    }));
    assert!(graph
        .nodes
        .iter()
        .any(|node| node.kind == CatalogNodeKind::Synonym && node.name == synonym));

    drain(
        driver
            .execute(
                conn.clone(),
                ExecuteRequest::new(format!(
                    "DROP VIEW dbo.[{view}]; DROP SYNONYM dbo.[{synonym}]; DROP PROCEDURE dbo.[{procedure}]; DROP FUNCTION dbo.[{function}]; DROP TABLE dbo.[{quoted_table}]; DROP TABLE dbo.[{child}]; DROP TABLE dbo.[{parent}]; DROP SEQUENCE dbo.[{sequence}]; DROP TYPE dbo.[{user_type}];"
                )),
            )
            .await
            .expect("drop graph fixture"),
    )
    .await;
    driver.close(conn).await.expect("close succeeds");
}
