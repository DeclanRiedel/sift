//! Native-definition fidelity against both real engines. Fixtures are isolated schemas.
#![cfg(any(feature = "live-pg", feature = "live-mssql"))]

use sift_driver_api::{ConnHandle, Driver};
use sift_protocol::{
    ConnectionSpec, Engine, ExecuteRequest, ObjectKind, ObjectPath, Page, SslMode,
};
use sift_server::ddl::generate_ddl;

async fn execute(driver: &dyn Driver, conn: &ConnHandle, sql: &str) {
    // SQL Server GO is a client delimiter, not SQL. Our fixtures use standalone GO only.
    let mut batch = String::new();
    for line in sql.lines().chain(std::iter::once("GO")) {
        if line.trim().eq_ignore_ascii_case("GO") {
            if !batch.trim().is_empty() {
                let mut stream = driver
                    .execute(conn.clone(), ExecuteRequest::new(&batch))
                    .await
                    .unwrap();
                let mut done = false;
                while let Some(page) = stream.rows.recv().await {
                    match page {
                        Page::Error { error } => panic!("{error}\n{batch}"),
                        Page::Done { .. } => done = true,
                        _ => {}
                    }
                }
                assert!(done, "stream ended without completion");
                batch.clear();
            }
        } else {
            batch.push_str(line);
            batch.push('\n');
        }
    }
}

fn spec(engine: Engine) -> ConnectionSpec {
    let pg = engine == Engine::Postgres;
    let prefix = if pg { "SIFT_PG" } else { "SIFT_MSSQL" };
    let get = |key: &str, fallback: &str| {
        std::env::var(format!("{prefix}_{key}")).unwrap_or_else(|_| fallback.into())
    };
    ConnectionSpec {
        host: get(
            "HOST",
            if pg {
                "/tmp/sift-demo-pg-socket"
            } else {
                "127.0.0.1"
            },
        ),
        port: Some(
            get("PORT", if pg { "5433" } else { "1433" })
                .parse()
                .unwrap(),
        ),
        database: Some(get("DB", if pg { "sifttest" } else { "master" })),
        user: get("USER", if pg { "sift" } else { "sa" }),
        password: std::env::var(format!("{prefix}_PASSWORD")).ok(),
        ssl_mode: Some(SslMode::Disable),
        engine_specific: if pg {
            None
        } else {
            Some(sift_protocol::EngineConnectionSpec::SqlServer(
                sift_protocol::MssqlConnectionSpec {
                    trust_server_certificate: Some(true),
                    ..Default::default()
                },
            ))
        },
    }
}

fn path(schema: &str, name: &str, kind: ObjectKind) -> ObjectPath {
    ObjectPath {
        catalog: None,
        schema: Some(schema.into()),
        name: name.into(),
        kind: Some(kind),
        routine_args: None,
    }
}

async fn round_trip(
    driver: &dyn Driver,
    conn: &ConnHandle,
    src: &str,
    dst: &str,
    name: &str,
    kind: ObjectKind,
) -> String {
    let ddl = generate_ddl(driver, conn.clone(), path(src, name, kind))
        .await
        .unwrap()
        .ddl;
    let target = ddl.replace(src, dst);
    execute(driver, conn, &target).await;
    let regenerated = generate_ddl(driver, conn.clone(), path(dst, name, kind))
        .await
        .unwrap()
        .ddl;
    // SQL Server stores the batch's trailing newlines in trigger definitions.
    // These fixtures contain no multiline string literals; ignore empty padding
    // lines while comparing every substantive catalog-rendered line exactly.
    let lines = |sql: &str| {
        sql.lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        lines(&target),
        lines(&regenerated),
        "native definition changed for {name}"
    );
    ddl
}

async fn assert_migration_fenced(driver: &dyn Driver, conn: &ConnHandle, schema: &str) {
    let snapshot = driver
        .schema(
            conn.clone(),
            sift_protocol::SchemaScope {
                depth: sift_protocol::SchemaDepth::Graph {
                    options: sift_protocol::CatalogGraphOptions {
                        schemas: Some(vec![schema.into()]),
                        include_definitions: true,
                        ..Default::default()
                    },
                },
                filter: None,
            },
        )
        .await
        .unwrap();
    let graph = snapshot.graph.unwrap();
    let table = graph
        .nodes
        .iter()
        .find(|node| node.kind == sift_protocol::CatalogNodeKind::Table && node.name == "items")
        .unwrap();
    assert_eq!(
        table.extra.get("migration_unsupported"),
        Some(&serde_json::Value::Bool(true))
    );
    assert!(table.extra.contains_key("native_column_shape"));
}

async fn assert_write_denied(driver: &dyn Driver, conn: &ConnHandle, sql: String) {
    match driver.execute(conn.clone(), ExecuteRequest::new(sql)).await {
        Err(_) => {}
        Ok(mut stream) => {
            let mut denied = false;
            while let Some(page) = stream.rows.recv().await {
                denied |= matches!(page, Page::Error { .. });
            }
            assert!(denied, "restricted user unexpectedly mutated the table");
        }
    }
}

#[cfg(feature = "live-pg")]
#[tokio::test]
async fn postgres_owned_sequence_round_trips_and_fences_generic_migration() {
    let driver = sift_driver_postgres::PgDriver::new();
    let conn = driver.open(&spec(Engine::Postgres)).await.unwrap();
    let (src, dst) = schemas();
    execute(
        &driver,
        &conn,
        &format!(
            "CREATE SCHEMA {src}; CREATE SCHEMA {dst}; \
         CREATE TABLE {src}.items(id integer); CREATE TABLE {dst}.items(id integer); \
         CREATE SEQUENCE {src}.counter AS integer START WITH 17 INCREMENT BY 3 CACHE 4; \
         ALTER SEQUENCE {src}.counter OWNED BY {src}.items.id; \
         CREATE INDEX items_id_idx ON {src}.items(id); \
         CLUSTER {src}.items USING items_id_idx;"
        ),
    )
    .await;
    let ddl = round_trip(&driver, &conn, &src, &dst, "counter", ObjectKind::Sequence).await;
    assert!(ddl.contains(&format!("OWNED BY {src}.items.id")), "{ddl}");

    let graph = driver
        .schema(
            conn.clone(),
            sift_protocol::SchemaScope {
                depth: sift_protocol::SchemaDepth::Graph {
                    options: sift_protocol::CatalogGraphOptions {
                        schemas: Some(vec![src.clone()]),
                        include_definitions: true,
                        ..Default::default()
                    },
                },
                filter: None,
            },
        )
        .await
        .unwrap()
        .graph
        .unwrap();
    let sequence = graph
        .nodes
        .iter()
        .find(|node| {
            node.kind == sift_protocol::CatalogNodeKind::Sequence && node.name == "counter"
        })
        .unwrap();
    let table = graph
        .nodes
        .iter()
        .find(|node| node.kind == sift_protocol::CatalogNodeKind::Table && node.name == "items")
        .unwrap();
    assert!(sequence.extra.contains_key("native_sequence_shape"));
    assert_eq!(
        sequence.extra.get("migration_unsupported"),
        Some(&serde_json::Value::Bool(true))
    );
    assert!(table.extra.contains_key("native_owned_sequence_shape"));
    assert!(table.extra.contains_key("native_unsupported_index_shape"));
    assert_eq!(
        table.extra.get("migration_unsupported"),
        Some(&serde_json::Value::Bool(true))
    );
    assert!(generate_ddl(
        &driver,
        conn.clone(),
        path(&src, "items", ObjectKind::Table)
    )
    .await
    .is_err());

    execute(
        &driver,
        &conn,
        &format!("CREATE TABLE {src}.identity_item(id bigint GENERATED ALWAYS AS IDENTITY);"),
    )
    .await;
    let error = generate_ddl(
        &driver,
        conn.clone(),
        path(&src, "identity_item_id_seq", ObjectKind::Sequence),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, sift_protocol::Code::UnsupportedForEngine);
    assert!(error.message.contains("identity sequence"));

    execute(
        &driver,
        &conn,
        &format!("DROP SCHEMA {dst} CASCADE; DROP SCHEMA {src} CASCADE;"),
    )
    .await;
    driver.close(conn).await.unwrap();
}

fn schemas() -> (String, String) {
    let id = uuid::Uuid::new_v4().simple().to_string();
    (format!("ns_{id}"), format!("nd_{id}"))
}

#[cfg(feature = "live-pg")]
#[tokio::test]
async fn postgres_standalone_index_round_trips_and_fences_generic_migration() {
    let driver = sift_driver_postgres::PgDriver::new();
    let conn = driver.open(&spec(Engine::Postgres)).await.unwrap();
    let (src, dst) = schemas();
    execute(
        &driver,
        &conn,
        &format!(
            "CREATE SCHEMA {src}; CREATE SCHEMA {dst}; \
             CREATE TABLE {src}.items(id integer PRIMARY KEY, name text); \
             CREATE TABLE {dst}.items(id integer PRIMARY KEY, name text); \
             CREATE INDEX expression_idx ON {src}.items USING btree \
                (lower(name) text_pattern_ops) INCLUDE (id) \
                WITH (fillfactor=80) WHERE name IS NOT NULL; \
             CREATE INDEX clustered_idx ON {src}.items(id); \
             ALTER TABLE {src}.items CLUSTER ON clustered_idx;"
        ),
    )
    .await;
    let ddl = round_trip(
        &driver,
        &conn,
        &src,
        &dst,
        "expression_idx",
        ObjectKind::Index,
    )
    .await;
    assert!(ddl.contains("text_pattern_ops"), "{ddl}");
    assert!(ddl.contains("INCLUDE (id)"), "{ddl}");
    assert!(ddl.contains("fillfactor='80'"), "{ddl}");
    assert!(ddl.contains("WHERE (name IS NOT NULL)"), "{ddl}");

    let graph = driver
        .schema(
            conn.clone(),
            sift_protocol::SchemaScope {
                depth: sift_protocol::SchemaDepth::Graph {
                    options: sift_protocol::CatalogGraphOptions {
                        schemas: Some(vec![src.clone()]),
                        include_definitions: true,
                        ..Default::default()
                    },
                },
                filter: None,
            },
        )
        .await
        .unwrap()
        .graph
        .unwrap();
    let index = graph
        .nodes
        .iter()
        .find(|node| {
            node.kind == sift_protocol::CatalogNodeKind::Index && node.name == "expression_idx"
        })
        .unwrap();
    assert!(index
        .native_id
        .as_deref()
        .is_some_and(|id| id.starts_with("pg:index:")));
    assert!(index.extra.contains_key("native_index_shape"));
    let table = graph
        .nodes
        .iter()
        .find(|node| node.kind == sift_protocol::CatalogNodeKind::Table && node.name == "items")
        .unwrap();
    assert!(table.extra.contains_key("native_index_set_shape"));
    assert_eq!(
        table.extra.get("migration_unsupported"),
        Some(&serde_json::Value::Bool(true))
    );

    for (name, reason) in [
        ("items_pkey", "constraint-backed"),
        ("clustered_idx", "clustered"),
    ] {
        let error = generate_ddl(&driver, conn.clone(), path(&src, name, ObjectKind::Index))
            .await
            .unwrap_err();
        assert_eq!(error.code, sift_protocol::Code::UnsupportedForEngine);
        assert!(error.message.contains(reason), "{error:?}");
    }
    execute(
        &driver,
        &conn,
        &format!("DROP SCHEMA {dst} CASCADE; DROP SCHEMA {src} CASCADE;"),
    )
    .await;
    driver.close(conn).await.unwrap();
}

#[cfg(feature = "live-pg")]
#[tokio::test]
async fn postgres_foreign_table_round_trip_and_restricted_metadata() {
    let driver = sift_driver_postgres::PgDriver::new();
    let conn = driver.open(&spec(Engine::Postgres)).await.unwrap();
    let (src, dst) = schemas();
    let id = uuid::Uuid::new_v4().simple().to_string();
    let wrapper = format!("fdw_{id}");
    let server = format!("srv_{id}");
    let reader = format!("reader_{id}");
    execute(
        &driver,
        &conn,
        &format!(
            r#"
CREATE FOREIGN DATA WRAPPER {wrapper} NO HANDLER NO VALIDATOR;
CREATE SERVER {server} FOREIGN DATA WRAPPER {wrapper};
CREATE SCHEMA {src}; CREATE SCHEMA {dst};
CREATE FUNCTION {src}.changed() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$;
CREATE FUNCTION {dst}.changed() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$;
CREATE FOREIGN TABLE {src}.remote_items (
    id integer OPTIONS (column_name 'remote_id') NOT NULL,
    note text COLLATE "C" DEFAULT 'it''s here',
    CONSTRAINT positive_id CHECK (id > 0)
) SERVER {server} OPTIONS (schema_name 'remote', table_name 'remote_items');
CREATE TRIGGER foreign_changed BEFORE INSERT ON {src}.remote_items
    FOR EACH ROW EXECUTE FUNCTION {src}.changed();
ALTER TABLE {src}.remote_items DISABLE TRIGGER foreign_changed;
CREATE TABLE {src}.partition_root (id integer) PARTITION BY RANGE (id);
CREATE FOREIGN TABLE {src}.partition_child PARTITION OF {src}.partition_root
    FOR VALUES FROM (0) TO (10) SERVER {server};
CREATE ROLE {reader};
GRANT USAGE ON SCHEMA {src} TO {reader};
GRANT SELECT ON {src}.remote_items TO {reader};
GRANT USAGE ON FOREIGN SERVER {server} TO {reader};
"#
        ),
    )
    .await;
    let ddl = round_trip(
        &driver,
        &conn,
        &src,
        &dst,
        "remote_items",
        ObjectKind::ForeignTable,
    )
    .await;
    for expected in [
        &format!("SERVER {server}"),
        "OPTIONS (column_name 'remote_id')",
        "OPTIONS (schema_name 'remote', table_name 'remote_items')",
        "CONSTRAINT positive_id CHECK",
        "COLLATE pg_catalog.\"C\"",
        "DISABLE TRIGGER foreign_changed",
    ] {
        assert!(ddl.contains(expected), "missing {expected}: {ddl}");
    }
    let unsupported = generate_ddl(
        &driver,
        conn.clone(),
        path(&src, "partition_child", ObjectKind::ForeignTable),
    )
    .await
    .unwrap_err();
    assert_eq!(unsupported.code, sift_protocol::Code::UnsupportedForEngine);
    execute(&driver, &conn, &format!("SET ROLE {reader};")).await;
    let denied = generate_ddl(
        &driver,
        conn.clone(),
        path(&src, "remote_items", ObjectKind::ForeignTable),
    )
    .await
    .unwrap_err();
    assert!(denied.message.contains("requires table ownership"));
    execute(&driver, &conn, "RESET ROLE;").await;
    execute(
        &driver,
        &conn,
        &format!(
            "DROP SCHEMA {dst} CASCADE; DROP SCHEMA {src} CASCADE; DROP SERVER {server}; DROP FOREIGN DATA WRAPPER {wrapper}; DROP ROLE {reader};"
        ),
    )
    .await;
    driver.close(conn).await.unwrap();
}

#[cfg(feature = "live-pg")]
#[tokio::test]
async fn postgres_native_ddl_preserves_advanced_columns_types_indexes_and_triggers() {
    let driver = sift_driver_postgres::PgDriver::new();
    let conn = driver.open(&spec(Engine::Postgres)).await.unwrap();
    let (src, dst) = schemas();
    execute(
        &driver,
        &conn,
        &format!(
            r#"
CREATE SCHEMA {src}; CREATE SCHEMA {dst};
CREATE TYPE {src}.mood AS ENUM ('it''s fine', 'sad');
CREATE TYPE {src}.pair AS (label varchar(19) COLLATE "C", amount numeric(17,4));
CREATE DOMAIN {src}.positive AS numeric(19,5) DEFAULT 1.25 NOT NULL CHECK (VALUE > 0);
CREATE FUNCTION {src}.changed() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$;
CREATE FUNCTION {dst}.changed() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$;
CREATE TABLE {src}.items (
 id bigint GENERATED BY DEFAULT AS IDENTITY (START WITH 17 INCREMENT BY 3 CACHE 4),
 label varchar(21) COLLATE "C" DEFAULT 'hello' NOT NULL,
 amount numeric(19,5) NOT NULL DEFAULT 1.23456,
 doubled numeric GENERATED ALWAYS AS (amount * 2) STORED,
 PRIMARY KEY (id), CHECK (amount > 0));
CREATE INDEX items_expr ON {src}.items ((lower(label)) DESC) INCLUDE (amount) WHERE amount > 2;
CREATE TRIGGER changed BEFORE UPDATE ON {src}.items FOR EACH ROW EXECUTE FUNCTION {src}.changed();
ALTER TABLE {src}.items DISABLE TRIGGER changed;
CREATE RULE ignore_zero AS ON INSERT TO {src}.items WHERE NEW.amount = 0 DO INSTEAD NOTHING;
ALTER TABLE {src}.items DISABLE RULE ignore_zero;
CREATE POLICY visible_items ON {src}.items AS RESTRICTIVE FOR SELECT TO PUBLIC USING (amount > 0);
ALTER TABLE {src}.items ENABLE ROW LEVEL SECURITY;
ALTER TABLE {src}.items FORCE ROW LEVEL SECURITY;
CREATE TABLE {src}.partitioned (id int NOT NULL) PARTITION BY RANGE (id);
CREATE TABLE {src}.child PARTITION OF {src}.partitioned FOR VALUES FROM (0) TO (10);
ALTER TABLE {src}.child ADD CONSTRAINT child_positive CHECK (id >= 0);
CREATE INDEX child_id_desc ON {src}.child (id DESC);
CREATE TRIGGER child_changed BEFORE UPDATE ON {src}.child FOR EACH ROW EXECUTE FUNCTION {src}.changed();
CREATE TABLE {src}.inherited_parent (base_id int NOT NULL);
CREATE TABLE {src}.inherited_child (local_note text) INHERITS ({src}.inherited_parent);
CREATE INDEX inherited_note_idx ON {src}.inherited_child (local_note);
CREATE TABLE {src}.inherited_parent_two (extra_id int NOT NULL);
CREATE TABLE {src}.multi_child (local_note text) INHERITS ({src}.inherited_parent, {src}.inherited_parent_two);
CREATE INDEX multi_note_idx ON {src}.multi_child (local_note);
CREATE TABLE {dst}.external_inherited_child (external_note text) INHERITS ({src}.inherited_parent);
CREATE TABLE {src}.policy_only (id integer);
CREATE POLICY positive_id ON {src}.policy_only FOR SELECT TO PUBLIC USING (id > 0);
ALTER TABLE {src}.policy_only ENABLE ROW LEVEL SECURITY;
"#
        ),
    )
    .await;
    for name in ["mood", "pair", "positive"] {
        round_trip(&driver, &conn, &src, &dst, name, ObjectKind::Type).await;
    }
    let ddl = round_trip(&driver, &conn, &src, &dst, "items", ObjectKind::Table).await;
    for expected in [
        "BY DEFAULT",
        "START WITH 17",
        "INCREMENT BY 3",
        "CACHE 4",
        "COLLATE",
        "numeric(19,5)",
        "STORED",
        "INCLUDE",
        "DISABLE TRIGGER",
        "DISABLE RULE",
        "CREATE POLICY visible_items",
        "FORCE ROW LEVEL SECURITY",
    ] {
        assert!(ddl.contains(expected), "missing {expected}: {ddl}");
    }
    round_trip(
        &driver,
        &conn,
        &src,
        &dst,
        "partitioned",
        ObjectKind::PartitionedTable,
    )
    .await;
    let child = round_trip(&driver, &conn, &src, &dst, "child", ObjectKind::Table).await;
    assert!(child.contains("PARTITION OF"));
    assert!(child.contains("FOR VALUES FROM (0) TO (10)"));
    assert!(child.contains("child_positive"));
    assert!(child.contains("child_id_desc"));
    assert!(child.contains("child_changed"));
    round_trip(
        &driver,
        &conn,
        &src,
        &dst,
        "inherited_parent",
        ObjectKind::Table,
    )
    .await;
    round_trip(
        &driver,
        &conn,
        &src,
        &dst,
        "inherited_parent_two",
        ObjectKind::Table,
    )
    .await;
    let inherited = round_trip(
        &driver,
        &conn,
        &src,
        &dst,
        "inherited_child",
        ObjectKind::Table,
    )
    .await;
    assert!(inherited.contains("INHERITS"));
    assert!(inherited.contains("local_note"));
    assert!(inherited.contains("inherited_note_idx"));
    let multi = round_trip(&driver, &conn, &src, &dst, "multi_child", ObjectKind::Table).await;
    assert!(multi.contains("multi_note_idx"));
    assert!(multi.contains(&format!(
        "INHERITS ({src}.inherited_parent, {src}.inherited_parent_two)"
    )));
    round_trip(&driver, &conn, &src, &dst, "policy_only", ObjectKind::Table).await;
    let security_shape = |graph: &sift_protocol::CatalogGraphData| {
        let table = graph
            .nodes
            .iter()
            .find(|node| node.name == "policy_only")
            .unwrap();
        assert_eq!(
            table.extra.get("migration_unsupported"),
            Some(&serde_json::Value::Bool(true))
        );
        table.extra.get("native_security_shape").cloned().unwrap()
    };
    let scope = sift_protocol::SchemaScope {
        depth: sift_protocol::SchemaDepth::Graph {
            options: sift_protocol::CatalogGraphOptions {
                schemas: Some(vec![src.clone()]),
                include_definitions: true,
                ..Default::default()
            },
        },
        filter: None,
    };
    let before_graph = driver
        .schema(conn.clone(), scope.clone())
        .await
        .unwrap()
        .graph
        .unwrap();
    let before = security_shape(&before_graph);
    let inherited_node = before_graph
        .nodes
        .iter()
        .find(|node| node.name == "inherited_child")
        .unwrap();
    assert_eq!(
        inherited_node.extra.get("migration_unsupported"),
        Some(&serde_json::Value::Bool(true))
    );
    assert!(inherited_node
        .extra
        .contains_key("native_inheritance_shape"));
    let inherited_parent = before_graph
        .nodes
        .iter()
        .find(|node| node.name == "inherited_parent")
        .unwrap();
    assert_eq!(
        inherited_parent.extra.get("migration_unsupported"),
        Some(&serde_json::Value::Bool(true))
    );
    let descendant_shape = inherited_parent
        .extra
        .get("native_descendant_shape")
        .cloned()
        .unwrap();
    assert!(before_graph
        .nodes
        .iter()
        .all(|node| node.name != "external_inherited_child"));
    execute(
        &driver,
        &conn,
        &format!("ALTER POLICY positive_id ON {src}.policy_only USING (id >= 0);"),
    )
    .await;
    let after = security_shape(
        &driver
            .schema(conn.clone(), scope.clone())
            .await
            .unwrap()
            .graph
            .unwrap(),
    );
    assert_ne!(before, after);
    let rule_shape = |graph: &sift_protocol::CatalogGraphData| {
        let table = graph
            .nodes
            .iter()
            .find(|node| node.name == "items")
            .unwrap();
        assert_eq!(
            table.extra.get("migration_unsupported"),
            Some(&serde_json::Value::Bool(true))
        );
        table.extra.get("native_rule_shape").cloned().unwrap()
    };
    let before_rule = rule_shape(
        &driver
            .schema(conn.clone(), scope.clone())
            .await
            .unwrap()
            .graph
            .unwrap(),
    );
    execute(
        &driver,
        &conn,
        &format!("ALTER TABLE {src}.items ENABLE RULE ignore_zero;"),
    )
    .await;
    let after_rule = rule_shape(
        &driver
            .schema(conn.clone(), scope.clone())
            .await
            .unwrap()
            .graph
            .unwrap(),
    );
    assert_ne!(before_rule, after_rule);
    let partition_shape = |graph: &sift_protocol::CatalogGraphData| {
        let child = graph
            .nodes
            .iter()
            .find(|node| node.name == "child")
            .unwrap();
        assert_eq!(
            child.extra.get("migration_unsupported"),
            Some(&serde_json::Value::Bool(true))
        );
        child.extra.get("native_partition_shape").cloned().unwrap()
    };
    let before_partition = partition_shape(
        &driver
            .schema(
                conn.clone(),
                sift_protocol::SchemaScope {
                    depth: sift_protocol::SchemaDepth::Graph {
                        options: sift_protocol::CatalogGraphOptions {
                            schemas: Some(vec![src.clone()]),
                            include_definitions: true,
                            ..Default::default()
                        },
                    },
                    filter: None,
                },
            )
            .await
            .unwrap()
            .graph
            .unwrap(),
    );
    execute(
        &driver,
        &conn,
        &format!("CREATE INDEX child_id_asc ON {src}.child (id ASC);"),
    )
    .await;
    let after_partition = partition_shape(
        &driver
            .schema(
                conn.clone(),
                sift_protocol::SchemaScope {
                    depth: sift_protocol::SchemaDepth::Graph {
                        options: sift_protocol::CatalogGraphOptions {
                            schemas: Some(vec![src.clone()]),
                            include_definitions: true,
                            ..Default::default()
                        },
                    },
                    filter: None,
                },
            )
            .await
            .unwrap()
            .graph
            .unwrap(),
    );
    assert_ne!(before_partition, after_partition);
    execute(
        &driver,
        &conn,
        &format!("ALTER TABLE {dst}.external_inherited_child RENAME TO renamed_external_child;"),
    )
    .await;
    let renamed_graph = driver
        .schema(conn.clone(), scope.clone())
        .await
        .unwrap()
        .graph
        .unwrap();
    let renamed_parent = renamed_graph
        .nodes
        .iter()
        .find(|node| node.name == "inherited_parent")
        .unwrap();
    assert_ne!(
        renamed_parent.extra.get("native_descendant_shape"),
        Some(&descendant_shape)
    );
    let trigger = generate_ddl(
        &driver,
        conn.clone(),
        path(&src, "changed", ObjectKind::Trigger),
    )
    .await
    .unwrap()
    .ddl;
    assert!(trigger.contains("DISABLE TRIGGER"));
    assert_migration_fenced(&driver, &conn, &src).await;
    execute(&driver, &conn, &format!("CREATE ROLE {src}; GRANT USAGE ON SCHEMA {src} TO {src}; GRANT SELECT ON {src}.items TO {src}; SET ROLE {src};")).await;
    execute(&driver, &conn, &format!("SELECT * FROM {src}.items")).await;
    assert_write_denied(
        &driver,
        &conn,
        format!("INSERT INTO {src}.items (label) VALUES ('denied')"),
    )
    .await;
    execute(&driver, &conn, &format!("RESET ROLE; REVOKE SELECT ON {src}.items FROM {src}; REVOKE USAGE ON SCHEMA {src} FROM {src}; DROP ROLE {src};")).await;
    execute(
        &driver,
        &conn,
        &format!("DROP SCHEMA {dst} CASCADE; DROP SCHEMA {src} CASCADE;"),
    )
    .await;
    driver.close(conn).await.unwrap();
}

#[cfg(feature = "live-mssql")]
#[tokio::test]
async fn sqlserver_native_ddl_preserves_disabled_and_untrusted_constraints() {
    let driver = sift_driver_sqlserver::MssqlDriver::new();
    let conn = driver.open(&spec(Engine::SqlServer)).await.unwrap();
    let (src, dst) = schemas();
    execute(
        &driver,
        &conn,
        &format!(
            r#"
CREATE SCHEMA {src};
GO
CREATE SCHEMA {dst};
GO
CREATE TABLE {src}.parent (id int NOT NULL CONSTRAINT parent_pk PRIMARY KEY);
CREATE TABLE {dst}.parent (id int NOT NULL CONSTRAINT parent_pk PRIMARY KEY);
CREATE TABLE {src}.child (
    id int NOT NULL,
    parent_id int NULL,
    amount int NOT NULL,
    CONSTRAINT child_check_untrusted CHECK (amount > 0),
    CONSTRAINT child_check_disabled CHECK (amount < 100)
);
ALTER TABLE {src}.child ADD CONSTRAINT child_fk_untrusted FOREIGN KEY (parent_id) REFERENCES {src}.parent(id);
ALTER TABLE {src}.child ADD CONSTRAINT child_fk_disabled FOREIGN KEY (parent_id) REFERENCES {src}.parent(id);
ALTER TABLE {src}.child NOCHECK CONSTRAINT child_check_untrusted;
ALTER TABLE {src}.child CHECK CONSTRAINT child_check_untrusted;
ALTER TABLE {src}.child NOCHECK CONSTRAINT child_fk_untrusted;
ALTER TABLE {src}.child CHECK CONSTRAINT child_fk_untrusted;
ALTER TABLE {src}.child NOCHECK CONSTRAINT child_check_disabled;
ALTER TABLE {src}.child NOCHECK CONSTRAINT child_fk_disabled;
"#
        ),
    )
    .await;

    let ddl = round_trip(&driver, &conn, &src, &dst, "child", ObjectKind::Table).await;
    for name in ["child_check_untrusted", "child_fk_untrusted"] {
        assert!(ddl.contains(&format!("NOCHECK CONSTRAINT [{name}]")));
        assert!(ddl.contains(&format!("CHECK CONSTRAINT [{name}]")));
    }
    for name in ["child_check_disabled", "child_fk_disabled"] {
        assert!(ddl.contains(&format!("NOCHECK CONSTRAINT [{name}]")));
        assert!(!ddl.contains(&format!(" CHECK CONSTRAINT [{name}]")));
    }

    let graph = driver
        .schema(
            conn.clone(),
            sift_protocol::SchemaScope {
                depth: sift_protocol::SchemaDepth::Graph {
                    options: sift_protocol::CatalogGraphOptions {
                        schemas: Some(vec![src.clone()]),
                        include_definitions: true,
                        ..Default::default()
                    },
                },
                filter: None,
            },
        )
        .await
        .unwrap()
        .graph
        .unwrap();
    let child = graph
        .nodes
        .iter()
        .find(|node| node.name == "child")
        .unwrap();
    assert_eq!(
        child.extra.get("migration_unsupported"),
        Some(&serde_json::Value::Bool(true))
    );
    let before = child.extra.get("native_column_shape").cloned().unwrap();
    execute(
        &driver,
        &conn,
        &format!("ALTER TABLE {src}.child WITH CHECK CHECK CONSTRAINT child_check_untrusted;"),
    )
    .await;
    let graph = driver
        .schema(
            conn.clone(),
            sift_protocol::SchemaScope {
                depth: sift_protocol::SchemaDepth::Graph {
                    options: sift_protocol::CatalogGraphOptions {
                        schemas: Some(vec![src.clone()]),
                        include_definitions: true,
                        ..Default::default()
                    },
                },
                filter: None,
            },
        )
        .await
        .unwrap()
        .graph
        .unwrap();
    let after = graph
        .nodes
        .iter()
        .find(|node| node.name == "child")
        .unwrap()
        .extra
        .get("native_column_shape")
        .cloned()
        .unwrap();
    assert_ne!(before, after);

    execute(
        &driver,
        &conn,
        &format!(
            "DROP TABLE {dst}.child; DROP TABLE {src}.child; DROP TABLE {dst}.parent; DROP TABLE {src}.parent; DROP SCHEMA {dst}; DROP SCHEMA {src};"
        ),
    )
    .await;
    driver.close(conn).await.unwrap();
}

#[cfg(feature = "live-mssql")]
#[tokio::test]
async fn sqlserver_native_ddl_preserves_advanced_columns_types_indexes_and_triggers() {
    let driver = sift_driver_sqlserver::MssqlDriver::new();
    let conn = driver.open(&spec(Engine::SqlServer)).await.unwrap();
    let (src, dst) = schemas();
    execute(&driver,&conn,&format!(r#"
SET ANSI_NULLS ON; SET QUOTED_IDENTIFIER ON;
GO
CREATE SCHEMA {src};
GO
CREATE SCHEMA {dst};
GO
CREATE TYPE {src}.label FROM nvarchar(21) NOT NULL;
CREATE SEQUENCE {src}.counter AS decimal(18,0) START WITH 17 INCREMENT BY 3 MINVALUE 2 MAXVALUE 9999 CYCLE CACHE 4;
CREATE TABLE {src}.items (
 id bigint IDENTITY(17,3) NOT NULL,
 label nvarchar(21) COLLATE Latin1_General_100_BIN2 NOT NULL CONSTRAINT items_label_default DEFAULT 'hello',
 amount numeric(19,5) NOT NULL CONSTRAINT items_amount_default DEFAULT 1.23456,
 doubled AS (amount * 2) PERSISTED,
 CONSTRAINT items_pk PRIMARY KEY NONCLUSTERED (id DESC), CONSTRAINT items_check CHECK (amount > 0));
CREATE INDEX items_label ON {src}.items (label DESC) INCLUDE (amount) WHERE amount > 2;
CREATE TABLE {src}.sparse_items (id int NOT NULL, optional_amount int SPARSE NULL);
CREATE TABLE {src}.compressed_items (id int NOT NULL, payload char(80));
ALTER TABLE {src}.compressed_items REBUILD PARTITION = ALL WITH (DATA_COMPRESSION = PAGE);
CREATE INDEX compressed_payload ON {src}.compressed_items (payload) WITH (DATA_COMPRESSION = ROW);
CREATE TABLE {src}.temporal_items (
 id int NOT NULL CONSTRAINT temporal_pk PRIMARY KEY,
 valid_from datetime2 GENERATED ALWAYS AS ROW START HIDDEN NOT NULL,
 valid_to datetime2 GENERATED ALWAYS AS ROW END NOT NULL,
 PERIOD FOR SYSTEM_TIME (valid_from, valid_to)
) WITH (SYSTEM_VERSIONING = ON (HISTORY_TABLE = {src}.temporal_items_history));
CREATE SYNONYM {src}.items_alias FOR {src}.items;
GO
CREATE TRIGGER {src}.changed ON {src}.items AFTER UPDATE AS BEGIN SET NOCOUNT ON; END;
GO
DISABLE TRIGGER {src}.changed ON {src}.items;
"#)).await;
    round_trip(&driver, &conn, &src, &dst, "label", ObjectKind::Type).await;
    round_trip(&driver, &conn, &src, &dst, "counter", ObjectKind::Sequence).await;
    let ddl = round_trip(&driver, &conn, &src, &dst, "items", ObjectKind::Table).await;
    let synonym = round_trip(
        &driver,
        &conn,
        &src,
        &dst,
        "items_alias",
        ObjectKind::Synonym,
    )
    .await;
    assert!(synonym.contains("CREATE SYNONYM"));
    assert!(synonym.contains("FOR"));
    let sparse = round_trip(
        &driver,
        &conn,
        &src,
        &dst,
        "sparse_items",
        ObjectKind::Table,
    )
    .await;
    assert!(sparse.contains("SPARSE NULL"));
    let compressed = round_trip(
        &driver,
        &conn,
        &src,
        &dst,
        "compressed_items",
        ObjectKind::Table,
    )
    .await;
    assert!(compressed.contains("DATA_COMPRESSION = PAGE"));
    assert!(compressed.contains("DATA_COMPRESSION = ROW"));
    let temporal = round_trip(
        &driver,
        &conn,
        &src,
        &dst,
        "temporal_items",
        ObjectKind::Table,
    )
    .await;
    assert!(temporal.contains("PERIOD FOR SYSTEM_TIME"));
    assert!(temporal.contains("SYSTEM_VERSIONING = ON"));
    assert!(temporal.contains("ROW START HIDDEN"));
    assert_eq!(
        generate_ddl(
            &driver,
            conn.clone(),
            path(&src, "temporal_items_history", ObjectKind::Table),
        )
        .await
        .unwrap_err()
        .code,
        sift_protocol::Code::UnsupportedForEngine,
    );
    for expected in [
        "IDENTITY(17,3)",
        "COLLATE",
        "[numeric](19,5)",
        "PERSISTED",
        "INCLUDE",
        "DISABLE TRIGGER",
    ] {
        assert!(ddl.contains(expected), "missing {expected}: {ddl}");
    }
    let trigger = generate_ddl(
        &driver,
        conn.clone(),
        path(&src, "changed", ObjectKind::Trigger),
    )
    .await
    .unwrap()
    .ddl;
    assert!(trigger.contains("DISABLE TRIGGER"));
    assert_migration_fenced(&driver, &conn, &src).await;
    let graph = driver
        .schema(
            conn.clone(),
            sift_protocol::SchemaScope {
                depth: sift_protocol::SchemaDepth::Graph {
                    options: sift_protocol::CatalogGraphOptions {
                        schemas: Some(vec![src.clone()]),
                        include_definitions: true,
                        ..Default::default()
                    },
                },
                filter: None,
            },
        )
        .await
        .unwrap()
        .graph
        .unwrap();
    for name in ["temporal_items", "sparse_items", "compressed_items"] {
        let table = graph.nodes.iter().find(|node| node.name == name).unwrap();
        assert_eq!(
            table.extra.get("migration_unsupported"),
            Some(&serde_json::Value::Bool(true))
        );
    }
    let synonym_node = graph
        .nodes
        .iter()
        .find(|node| node.name == "items_alias")
        .unwrap();
    assert!(graph.edges.iter().any(|edge| {
        edge.from == synonym_node.id
            && edge.kind == sift_protocol::CatalogEdgeKind::DependsOn
            && (edge.to.is_some()
                || edge
                    .referenced_path
                    .as_deref()
                    .is_some_and(|path| path.contains("items")))
    }));
    let compressed_shape = |graph: &sift_protocol::CatalogGraphData| {
        graph
            .nodes
            .iter()
            .find(|node| node.name == "compressed_items")
            .unwrap()
            .extra
            .get("native_column_shape")
            .cloned()
            .unwrap()
    };
    let before_compression = compressed_shape(&graph);
    execute(
        &driver,
        &conn,
        &format!("ALTER INDEX compressed_payload ON {src}.compressed_items REBUILD PARTITION = ALL WITH (DATA_COMPRESSION = PAGE);"),
    )
    .await;
    let after_compression = compressed_shape(
        &driver
            .schema(
                conn.clone(),
                sift_protocol::SchemaScope {
                    depth: sift_protocol::SchemaDepth::Graph {
                        options: sift_protocol::CatalogGraphOptions {
                            schemas: Some(vec![src.clone()]),
                            include_definitions: true,
                            ..Default::default()
                        },
                    },
                    filter: None,
                },
            )
            .await
            .unwrap()
            .graph
            .unwrap(),
    );
    assert_ne!(before_compression, after_compression);
    execute(&driver, &conn, &format!("CREATE USER {src} WITHOUT LOGIN; GRANT SELECT ON {src}.items TO {src}; GRANT SELECT ON {src}.items_alias TO {src}; EXECUTE AS USER = '{src}';")).await;
    execute(&driver, &conn, &format!("SELECT * FROM {src}.items")).await;
    assert_write_denied(
        &driver,
        &conn,
        format!("INSERT INTO {src}.items (label) VALUES ('denied')"),
    )
    .await;
    assert!(generate_ddl(
        &driver,
        conn.clone(),
        path(&src, "items", ObjectKind::Table)
    )
    .await
    .is_err());
    assert!(generate_ddl(
        &driver,
        conn.clone(),
        path(&src, "items_alias", ObjectKind::Synonym)
    )
    .await
    .is_err());
    execute(&driver, &conn, &format!("REVERT; DROP USER {src};")).await;
    execute(&driver,&conn,&format!("ALTER TABLE {dst}.temporal_items SET (SYSTEM_VERSIONING = OFF); ALTER TABLE {src}.temporal_items SET (SYSTEM_VERSIONING = OFF); DROP SYNONYM {dst}.items_alias; DROP SYNONYM {src}.items_alias; DROP TABLE {dst}.temporal_items; DROP TABLE {dst}.temporal_items_history; DROP TABLE {src}.temporal_items; DROP TABLE {src}.temporal_items_history; DROP TABLE {dst}.compressed_items; DROP TABLE {src}.compressed_items; DROP TABLE {dst}.sparse_items; DROP TABLE {src}.sparse_items; DROP TABLE {dst}.items; DROP TABLE {src}.items; DROP SEQUENCE {dst}.counter; DROP SEQUENCE {src}.counter; DROP TYPE {dst}.label; DROP TYPE {src}.label; DROP SCHEMA {dst}; DROP SCHEMA {src};")).await;
    driver.close(conn).await.unwrap();
}
