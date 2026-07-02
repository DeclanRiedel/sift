//! Integration tests against a real Postgres instance. Gated behind the
//! `live-pg` feature so CI runs without it; local invocation requires the
//! developer to bring up a PG first (see PHASE0.md step 14).
//!
//! Bring up a throwaway PG inside the nix shell:
//!
//! ```text
//! nix develop --command bash -c '
//!   export PGDATA=/tmp/sift-pg
//!   initdb -D "$PGDATA" -U sift --auth=trust --no-locale --encoding=UTF8
//!   printf "port = 5433\nunix_socket_directories = '\''/tmp/sift-pg-socket'\''\n" >> "$PGDATA/postgresql.conf"
//!   mkdir -p /tmp/sift-pg-socket
//!   pg_ctl -D "$PGDATA" -l /tmp/sift-pg.log -w start
//!   psql -h /tmp/sift-pg-socket -p 5433 -U sift -d postgres -c "CREATE DATABASE sifttest;"
//! '
//! SIFT_PG_HOST=/tmp/sift-pg-socket SIFT_PG_PORT=5433 \
//!   cargo test -p sift-driver-postgres --features live-pg --test live_pg -- --nocapture
//! ```

#![cfg(feature = "live-pg")]

use std::sync::atomic::{AtomicU64, Ordering};

use sift_driver_api::{AdvisoryKey, ConnHandle, Driver, PgExt};
use sift_driver_postgres::PgDriver;
use sift_protocol::{
    ConnectionSpec, Engine, IsolationLevel, ObjectKind, Page, PrimitiveType, SchemaScope, SslMode,
    TxAccessMode as AccessMode, TxMode, TypeRef, Value,
};

const DEFAULT_PG_HOST: &str = "/tmp/opencode/sift-pg-socket";
const DEFAULT_PG_PORT: u16 = 5433;
const DEFAULT_PG_USER: &str = "sift";
const DEFAULT_PG_DB: &str = "sifttest";

static SCHEMA_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Per-test unique schema name so tests run in parallel without setup
/// races on a shared schema.
fn unique_schema() -> String {
    let n = SCHEMA_COUNTER.fetch_add(1, Ordering::Relaxed);
    let caller = std::thread::current()
        .name()
        .unwrap_or("anon")
        .replace("::", "_");
    format!("live_pg_test_{caller}_{n}")
}

fn spec() -> ConnectionSpec {
    ConnectionSpec {
        host: std::env::var("SIFT_PG_HOST").unwrap_or_else(|_| DEFAULT_PG_HOST.to_string()),
        port: Some(
            std::env::var("SIFT_PG_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(DEFAULT_PG_PORT),
        ),
        database: Some(std::env::var("SIFT_PG_DB").unwrap_or_else(|_| DEFAULT_PG_DB.to_string())),
        user: std::env::var("SIFT_PG_USER").unwrap_or_else(|_| DEFAULT_PG_USER.to_string()),
        password: std::env::var("SIFT_PG_PASSWORD").ok(),
        ssl_mode: Some(SslMode::Disable),
        engine_specific: None,
    }
}

/// Set up the test fixture in a unique schema, returning the schema name.
async fn setup_schema(driver: &PgDriver, conn: &ConnHandle) -> String {
    let schema = unique_schema();
    let queries = [
        format!("CREATE SCHEMA {schema}"),
        format!(
            "CREATE TABLE {schema}.users (
            id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
            email TEXT NOT NULL UNIQUE,
            age INT CHECK (age >= 0),
            bio TEXT
        )"
        ),
        format!("CREATE INDEX idx_users_age ON {schema}.users (age) WHERE age IS NOT NULL"),
        format!(
            "INSERT INTO {schema}.users (email, age, bio) VALUES
            ('alice@x.io', 30, 'engineer'),
            ('bob@y.io', 25, NULL),
            ('carol@z.io', 41, 'manager')"
        ),
        format!(
            "CREATE MATERIALIZED VIEW {schema}.mv_adults AS
            SELECT email, age FROM {schema}.users WHERE age >= 18"
        ),
    ];
    for q in queries {
        let stream = driver
            .execute(conn.clone(), sift_protocol::ExecuteRequest::new(q))
            .await
            .expect("setup query runs");
        drain(stream).await;
    }
    schema
}

async fn drain(mut stream: sift_driver_api::ResultSetStream) -> Vec<Page> {
    let mut pages = Vec::new();
    while let Some(page) = stream.rows.recv().await {
        pages.push(page);
    }
    pages
}

#[tokio::test]
async fn open_and_ping() {
    let driver = PgDriver::new();
    let conn = driver.open(&spec()).await.expect("open succeeds");
    let info = driver.ping(conn.clone()).await.expect("ping succeeds");
    assert_eq!(info.engine, Engine::Postgres);
    assert!(info.current_user.contains("sift"));
    assert_eq!(
        info.current_database,
        std::env::var("SIFT_PG_DB").unwrap_or_else(|_| DEFAULT_PG_DB.to_string())
    );
    assert!(info.server_version.starts_with("PostgreSQL"));
    driver.close(conn).await.expect("close clean");
}

#[tokio::test]
async fn execute_select_decodes_types() {
    let driver = PgDriver::new();
    let conn = driver.open(&spec()).await.unwrap();
    let schema = setup_schema(&driver, &conn).await;

    let stream = driver
        .execute(
            conn.clone(),
            sift_protocol::ExecuteRequest::new(format!(
                "SELECT email, age, 12345.67::numeric AS amount, INTERVAL '2 days 1 second' AS elapsed FROM {schema}.users ORDER BY age"
            )),
        )
        .await
        .expect("execute select");
    let pages = drain(stream).await;

    // Expect: NextResult { 4 cols }, Rows x 3, Done.
    let columns = pages.iter().find_map(|p| match p {
        Page::NextResult { columns } => Some(columns),
        _ => None,
    });
    let cols = columns.expect("got NextResult").clone();
    assert_eq!(cols.len(), 4);
    assert_eq!(cols[0].name, "email");
    assert_eq!(cols[1].name, "age");
    assert!(
        matches!(cols[0].type_ref, TypeRef::Primitive(PrimitiveType::Text)),
        "{:?}",
        cols[0].type_ref
    );
    assert!(matches!(
        cols[1].type_ref,
        TypeRef::Primitive(PrimitiveType::Int32)
    ));
    assert!(matches!(
        cols[2].type_ref,
        TypeRef::Primitive(PrimitiveType::Decimal)
    ));
    assert!(matches!(
        cols[3].type_ref,
        TypeRef::Primitive(PrimitiveType::Interval)
    ));

    let rows: Vec<&[Value]> = pages
        .iter()
        .filter_map(|p| match p {
            Page::Rows { rows } => Some(rows),
            _ => None,
        })
        .flatten()
        .map(|r| r.values.as_slice())
        .collect();
    assert_eq!(rows.len(), 3, "three users seeded");
    assert!(matches!(&rows[0][0], Value::Text(s) if s.contains("bob")));
    assert!(matches!(&rows[0][1], Value::Int32(25)));
    assert!(matches!(&rows[0][2], Value::Decimal(v) if v == "12345.67"));
    assert!(
        matches!(&rows[0][3], Value::Interval(v) if *v == chrono::Duration::days(2) + chrono::Duration::seconds(1))
    );

    driver.close(conn).await.unwrap();
}

#[tokio::test]
async fn execute_dml_reports_affected_rows() {
    let driver = PgDriver::new();
    let conn = driver.open(&spec()).await.unwrap();
    let schema = setup_schema(&driver, &conn).await;

    // DML goes through simple_query path; CommandComplete(u64) carries the
    // count.
    let stream = driver
        .execute(
            conn.clone(),
            sift_protocol::ExecuteRequest::new(format!(
                "UPDATE {schema}.users SET bio = 'updated' WHERE age < 40"
            )),
        )
        .await
        .expect("execute update");
    let pages = drain(stream).await;

    let done: Option<u64> = pages.iter().find_map(|p| match p {
        Page::Done { affected_rows, .. } => *affected_rows,
        _ => None,
    });
    assert_eq!(done, Some(2), "alice(30) + bob(25) updated");

    driver.close(conn).await.unwrap();
}

#[tokio::test]
async fn schema_shallow_lists_test_objects() {
    let driver = PgDriver::new();
    let conn = driver.open(&spec()).await.unwrap();
    let schema_name = setup_schema(&driver, &conn).await;

    let snap = driver
        .schema(conn.clone(), SchemaScope::shallow())
        .await
        .expect("schema shallow");
    assert_eq!(snap.trees.len(), 1);
    assert_eq!(
        snap.trees[0].name,
        std::env::var("SIFT_PG_DB").unwrap_or_else(|_| DEFAULT_PG_DB.to_string())
    );

    let schema_tree = snap.trees[0]
        .schemas
        .iter()
        .find(|s| s.name == schema_name)
        .expect("test schema present");
    let kinds: std::collections::HashSet<_> = schema_tree.objects.iter().map(|o| o.kind).collect();
    assert!(kinds.contains(&ObjectKind::Table), "found users table");
    assert!(
        kinds.contains(&ObjectKind::MaterializedView),
        "found mv_adults"
    );

    let users = schema_tree
        .objects
        .iter()
        .find(|o| o.name == "users")
        .expect("users table present");
    assert_eq!(users.kind, ObjectKind::Table);

    driver.close(conn).await.unwrap();
}

#[tokio::test]
async fn schema_shallow_pushes_down_name_filter() {
    let driver = PgDriver::new();
    let conn = driver.open(&spec()).await.unwrap();
    let _schema = setup_schema(&driver, &conn).await;

    let mut scope = SchemaScope::shallow();
    scope.filter = Some(sift_protocol::SchemaFilter {
        catalogs: None,
        schemas: None,
        kinds: None,
        name_pattern: Some("users".to_string()),
    });
    let snap = driver.schema(conn.clone(), scope).await.expect("schema");
    let names: Vec<&str> = snap.trees[0]
        .schemas
        .iter()
        .flat_map(|s| s.objects.iter().map(|o| o.name.as_str()))
        .collect();
    assert!(names.contains(&"users"), "users present");
    assert!(
        !names.contains(&"mv_adults"),
        "mv_adults filtered out by name pattern"
    );

    driver.close(conn).await.unwrap();
}

#[tokio::test]
async fn schema_deep_lists_columns_pk_indexes_constraints() {
    let driver = PgDriver::new();
    let conn = driver.open(&spec()).await.unwrap();
    let schema = setup_schema(&driver, &conn).await;

    let scope = SchemaScope::deep(sift_protocol::ObjectPath {
        catalog: None,
        schema: Some(schema),
        name: "users".into(),
        kind: Some(ObjectKind::Table),
    });
    let snap = driver
        .schema(conn.clone(), scope)
        .await
        .expect("deep schema");
    let obj = &snap.trees[0].schemas[0].objects[0];

    // 4 columns: id, email, age, bio.
    assert_eq!(obj.columns.len(), 4);

    let id = obj.columns.iter().find(|c| c.name == "id").unwrap();
    assert!(id.primary_key, "id is PK");
    assert!(id.auto_increment, "id is identity");
    assert!(matches!(
        id.nullable,
        sift_protocol::Nullability::NotNullable
    ));

    let email = obj.columns.iter().find(|c| c.name == "email").unwrap();
    assert!(matches!(
        email.nullable,
        sift_protocol::Nullability::NotNullable
    ));

    // PK constraint + unique email + check age.
    assert!(
        obj.constraints
            .iter()
            .any(|c| c.kind == sift_protocol::ConstraintKind::PrimaryKey),
        "PK constraint present"
    );
    assert!(
        obj.constraints
            .iter()
            .any(|c| c.kind == sift_protocol::ConstraintKind::Unique),
        "UNIQUE constraint present"
    );
    assert!(
        obj.constraints
            .iter()
            .any(|c| c.kind == sift_protocol::ConstraintKind::Check),
        "CHECK constraint present"
    );

    // At least two indexes: PK + idx_users_age.
    assert!(obj.indexes.len() >= 2, "indexes: {:?}", obj.indexes);
    assert!(
        obj.indexes.iter().any(|i| i.primary_key),
        "PK index present"
    );
    assert!(
        obj.indexes
            .iter()
            .any(|i| i.name == "idx_users_age" && i.partial_predicate.is_some()),
        "partial index present with predicate"
    );

    driver.close(conn).await.unwrap();
}

#[tokio::test]
async fn transaction_commit_persists() {
    let driver = PgDriver::new();
    let conn = driver.open(&spec()).await.unwrap();
    let schema = setup_schema(&driver, &conn).await;

    let tx_mode = TxMode {
        isolation: IsolationLevel::ReadCommitted,
        access: AccessMode::ReadWrite,
    };
    let tx = driver.begin(conn.clone(), tx_mode).await.expect("begin");
    drain(
        driver
            .execute(
                conn.clone(),
                sift_protocol::ExecuteRequest::new(format!(
                    "INSERT INTO {schema}.users (email, age) VALUES ('tx@commit.io', 99)"
                )),
            )
            .await
            .unwrap(),
    )
    .await;
    driver.commit(tx).await.expect("commit");

    let stream = driver
        .execute(
            conn.clone(),
            sift_protocol::ExecuteRequest::new(format!(
                "SELECT count(*) FROM {schema}.users WHERE email = 'tx@commit.io'"
            )),
        )
        .await
        .unwrap();
    let pages = drain(stream).await;
    let last_row = pages
        .iter()
        .rev()
        .find_map(|p| match p {
            Page::Rows { rows: r } => Some(&r[r.len() - 1].values[0]),
            _ => None,
        })
        .unwrap();
    assert!(matches!(last_row, Value::Int64(1)), "row committed");

    driver.close(conn).await.unwrap();
}

#[tokio::test]
async fn transaction_rollback_discards() {
    let driver = PgDriver::new();
    let conn = driver.open(&spec()).await.unwrap();
    let schema = setup_schema(&driver, &conn).await;

    let tx = driver
        .begin(
            conn.clone(),
            TxMode {
                isolation: IsolationLevel::ReadCommitted,
                access: AccessMode::ReadWrite,
            },
        )
        .await
        .expect("begin");
    drain(
        driver
            .execute(
                conn.clone(),
                sift_protocol::ExecuteRequest::new(format!(
                    "INSERT INTO {schema}.users (email, age) VALUES ('tx@rollback.io', 1)"
                )),
            )
            .await
            .unwrap(),
    )
    .await;
    driver.rollback(tx).await.expect("rollback");

    let stream = driver
        .execute(
            conn.clone(),
            sift_protocol::ExecuteRequest::new(format!(
                "SELECT count(*) FROM {schema}.users WHERE email = 'tx@rollback.io'"
            )),
        )
        .await
        .unwrap();
    let pages = drain(stream).await;
    let last_row = pages
        .iter()
        .rev()
        .find_map(|p| match p {
            Page::Rows { rows: r } => Some(&r[r.len() - 1].values[0]),
            _ => None,
        })
        .unwrap();
    assert!(matches!(last_row, Value::Int64(0)), "row rolled back");

    driver.close(conn).await.unwrap();
}

#[tokio::test]
async fn advisory_lock_round_trip() {
    let driver = PgDriver::new();
    let conn = driver.open(&spec()).await.unwrap();
    driver
        .advisory_lock(conn.clone(), AdvisoryKey::Int64(42))
        .await
        .expect("lock");
    driver
        .advisory_unlock(conn.clone(), AdvisoryKey::Int64(42))
        .await
        .expect("unlock");
    driver.close(conn).await.unwrap();
}

#[tokio::test]
async fn cancel_aborts_long_query() {
    let driver = PgDriver::new();
    let conn = driver.open(&spec()).await.unwrap();

    let stream = driver
        .execute(
            conn.clone(),
            sift_protocol::ExecuteRequest::new("SELECT pg_sleep(30), 1"),
        )
        .await
        .expect("execute sleep");

    let cursor_id = stream.cursor_id;
    // Give PG a moment to start the query, then cancel.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    driver
        .cancel(conn.clone(), cursor_id)
        .await
        .expect("cancel call");

    // The query task should observe cancellation via the channel.
    let pages = drain(stream).await;
    let done = pages.iter().find_map(|p| match p {
        Page::Done { warnings, .. } => Some(warnings),
        _ => None,
    });
    // Either a Done with warnings (cancel surfaced cleanly) or stream end.
    // We accept either; what we reject is "the query completed normally in
    // < 30s without cancellation."
    if let Some(warnings) = done {
        assert!(
            !warnings.is_empty(),
            "cancel should produce at least one warning, got: {warnings:?}"
        );
    }

    driver.close(conn).await.unwrap();
}

#[tokio::test]
async fn close_mid_query_does_not_panic() {
    // Closing a conn with an in-flight query should not panic or hang.
    let driver = PgDriver::new();
    let conn = driver.open(&spec()).await.unwrap();
    let _stream = driver
        .execute(
            conn.clone(),
            sift_protocol::ExecuteRequest::new("SELECT pg_sleep(0.5), 1"),
        )
        .await
        .unwrap();
    // Don't drain — close immediately. Implicit assertion: no panic, no hang.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), driver.close(conn))
        .await
        .expect("close mid-query completes within 5s");
}
