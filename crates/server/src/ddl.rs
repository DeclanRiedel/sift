//! DDL generation (Phase D).
//!
//! Composes CREATE statements for a database object by calling
//! existing `Driver` methods — no new trait method is required, so
//! the ADR-017 driver-trait lock is undisturbed.
//!
//! Strategy per object kind:
//!
//! - **Tables** (both engines): use `Driver::schema(Deep)` to fetch
//!   columns / indexes / constraints / triggers, then format a
//!   `CREATE TABLE` with inline PK/UNIQUE/CHECK/FK constraints,
//!   followed by standalone `CREATE INDEX` statements for
//!   non-constraint indexes.
//! - **Views / Materialized Views / Procedures / Functions**: use
//!   engine-native catalog functions via `Driver::execute`:
//!   - PG: `pg_get_viewdef(oid)`, `pg_get_functiondef(oid)`. The
//!     regnamespace / regprocedure casts resolve the identifier.
//!   - MSSQL: `OBJECT_DEFINITION(OBJECT_ID(...))`.
//!
//! Server-side composition means driver crates stay unchanged; the
//! DDL layer runs alongside HTTP handlers and depends only on
//! primitives that already exist.

use sift_driver_api::Driver;
use sift_protocol::{
    Code, ConstraintKind, DriverError, Engine, ExecuteRequest, ObjectDdl, ObjectInfo, ObjectKind,
    ObjectPath, Page, SchemaDepth, SchemaScope, TypeRef, Value,
};

/// Fetch and format DDL for `object` on `driver`. Dispatches by
/// engine + kind. Errors bubble up from the underlying driver calls;
/// unsupported combinations return `Code::UnsupportedForEngine`.
pub async fn generate_ddl(
    driver: &dyn Driver,
    handle: sift_driver_api::ConnHandle,
    object: ObjectPath,
) -> Result<ObjectDdl, DriverError> {
    let kind = object.kind.unwrap_or(ObjectKind::Table);
    let engine = driver.engine();
    let ddl = match kind {
        ObjectKind::Table | ObjectKind::PartitionedTable | ObjectKind::ForeignTable => {
            generate_table_ddl(driver, handle, &object, engine).await?
        }
        ObjectKind::View | ObjectKind::MaterializedView => {
            generate_view_ddl(driver, handle, &object, engine, kind).await?
        }
        ObjectKind::Procedure | ObjectKind::ScalarFunction | ObjectKind::TableValuedFunction => {
            generate_routine_ddl(driver, handle, &object, engine).await?
        }
        other => {
            return Err(DriverError::new(
                Code::UnsupportedForEngine,
                format!("DDL generation for object kind {other:?} is not supported"),
            )
            .with_engine(engine));
        }
    };
    Ok(ObjectDdl { path: object, ddl })
}

async fn generate_table_ddl(
    driver: &dyn Driver,
    handle: sift_driver_api::ConnHandle,
    object: &ObjectPath,
    engine: Engine,
) -> Result<String, DriverError> {
    // Reuse the deep schema pass to get columns/indexes/constraints/
    // triggers in one round-trip.
    let scope = SchemaScope {
        depth: SchemaDepth::Deep {
            object: object.clone(),
        },
        filter: None,
    };
    let snap = driver.schema(handle, scope).await?;
    let info = snap
        .trees
        .iter()
        .flat_map(|t| t.schemas.iter())
        .flat_map(|s| s.objects.iter())
        .find(|o| o.name == object.name)
        .cloned()
        .ok_or_else(|| {
            DriverError::new(
                Code::UndefinedObject,
                "object not found in deep schema snapshot",
            )
            .with_engine(engine)
        })?;
    Ok(format_table_ddl(object, &info, engine))
}

fn format_table_ddl(path: &ObjectPath, info: &ObjectInfo, engine: Engine) -> String {
    let mut out = String::new();
    let qname = qualified_name(path, engine);
    out.push_str("CREATE TABLE ");
    out.push_str(&qname);
    out.push_str(" (\n");

    let mut lines: Vec<String> = info
        .columns
        .iter()
        .map(|col| {
            let mut line = format!(
                "    {} {}",
                quote_ident(&col.name, engine),
                type_to_sql(&col.type_ref, engine)
            );
            if matches!(col.nullable, sift_protocol::Nullability::NotNullable) {
                line.push_str(" NOT NULL");
            }
            line
        })
        .collect();

    // Inline PK / UNIQUE / CHECK / FK constraints. We use each
    // constraint's `definition` when the driver provided it (PG's
    // `pg_get_constraintdef` is authoritative). SQL Server's driver
    // fills `definition` for CHECK/FK/DEFAULT but not for the PK
    // constraint on the base table; fall back to a synthesized form.
    for c in &info.constraints {
        let clause = if let Some(def) = &c.definition {
            let name = quote_ident(&c.name, engine);
            format!("    CONSTRAINT {name} {def}")
        } else {
            constraint_fallback(c, engine)
        };
        lines.push(clause);
    }

    out.push_str(&lines.join(",\n"));
    out.push_str("\n)");
    if matches!(engine, Engine::Postgres) {
        out.push(';');
    } else {
        out.push_str(";\nGO");
    }

    // Standalone CREATE INDEX for indexes that aren't already
    // enforcing a PK/UNIQUE constraint (those are inlined above).
    let constraint_index_names: std::collections::HashSet<&str> =
        info.constraints.iter().map(|c| c.name.as_str()).collect();
    for idx in &info.indexes {
        if idx.primary_key {
            continue;
        }
        if constraint_index_names.contains(idx.name.as_str()) {
            continue;
        }
        out.push('\n');
        out.push_str(&format_index_ddl(path, idx, engine));
    }
    out
}

fn constraint_fallback(c: &sift_protocol::ConstraintInfo, engine: Engine) -> String {
    let name = quote_ident(&c.name, engine);
    let cols: Vec<String> = c.columns.iter().map(|c| quote_ident(c, engine)).collect();
    let cols_joined = cols.join(", ");
    match c.kind {
        ConstraintKind::PrimaryKey => {
            format!("    CONSTRAINT {name} PRIMARY KEY ({cols_joined})")
        }
        ConstraintKind::Unique => {
            format!("    CONSTRAINT {name} UNIQUE ({cols_joined})")
        }
        _ => format!(
            "    -- constraint {name} ({:?}) definition unavailable",
            c.kind
        ),
    }
}

fn format_index_ddl(path: &ObjectPath, idx: &sift_protocol::IndexInfo, engine: Engine) -> String {
    let name = quote_ident(&idx.name, engine);
    let qname = qualified_name(path, engine);
    let cols: Vec<String> = idx.columns.iter().map(|c| quote_ident(c, engine)).collect();
    let cols_joined = cols.join(", ");
    let unique = if idx.unique { "UNIQUE " } else { "" };
    let mut out = format!("CREATE {unique}INDEX {name} ON {qname} ({cols_joined})");
    if let Some(pred) = &idx.partial_predicate {
        out.push_str(" WHERE ");
        out.push_str(pred);
    }
    out.push(';');
    if matches!(engine, Engine::SqlServer) {
        out.push_str("\nGO");
    }
    out
}

async fn generate_view_ddl(
    driver: &dyn Driver,
    handle: sift_driver_api::ConnHandle,
    object: &ObjectPath,
    engine: Engine,
    kind: ObjectKind,
) -> Result<String, DriverError> {
    let qname = qualified_name(object, engine);
    let is_materialized = matches!(kind, ObjectKind::MaterializedView);
    let (sql, prefix) = match (engine, is_materialized) {
        (Engine::Postgres, false) => (
            format!(
                "SELECT pg_get_viewdef('{}'::regclass, true)",
                qname.replace('\'', "''")
            ),
            format!("CREATE OR REPLACE VIEW {qname} AS\n"),
        ),
        (Engine::Postgres, true) => (
            // `pg_get_viewdef` works on materialized views too — it
            // returns the view body (SELECT ...). `CREATE OR REPLACE`
            // is not supported for materialized views; a caller who
            // wants to redeploy must DROP + CREATE.
            format!(
                "SELECT pg_get_viewdef('{}'::regclass, true)",
                qname.replace('\'', "''")
            ),
            format!("CREATE MATERIALIZED VIEW {qname} AS\n"),
        ),
        (Engine::SqlServer, false) => (
            format!(
                "SELECT OBJECT_DEFINITION(OBJECT_ID(N'{}'))",
                qname.replace('\'', "''")
            ),
            String::new(),
        ),
        (Engine::SqlServer, true) => {
            // SQL Server has no materialized views (indexed views are a
            // distinct concept and don't round-trip cleanly). Signal
            // the caller rather than emit misleading DDL.
            return Err(DriverError::new(
                Code::UnsupportedForEngine,
                "SQL Server does not have materialized views",
            )
            .with_engine(engine));
        }
    };
    let body = fetch_scalar_text(driver, handle, sql).await?;
    Ok(format!("{prefix}{body}"))
}

async fn generate_routine_ddl(
    driver: &dyn Driver,
    handle: sift_driver_api::ConnHandle,
    object: &ObjectPath,
    engine: Engine,
) -> Result<String, DriverError> {
    let qname = qualified_name(object, engine);
    let sql = match engine {
        Engine::Postgres => {
            let regprocedure = pg_regprocedure_name(object);
            format!(
                "SELECT pg_get_functiondef('{}'::regprocedure)",
                regprocedure.replace('\'', "''")
            )
        }
        Engine::SqlServer => format!(
            "SELECT OBJECT_DEFINITION(OBJECT_ID(N'{}'))",
            qname.replace('\'', "''")
        ),
    };
    fetch_scalar_text(driver, handle, sql).await
}

fn pg_regprocedure_name(object: &ObjectPath) -> String {
    let qname = qualified_name(object, Engine::Postgres);
    match &object.routine_args {
        Some(args) => format!("{qname}({})", args.join(", ")),
        None => qname,
    }
}

/// Drain an execute stream and return the first column of the first
/// row as a String. Used by the catalog-function DDL paths.
async fn fetch_scalar_text(
    driver: &dyn Driver,
    handle: sift_driver_api::ConnHandle,
    sql: String,
) -> Result<String, DriverError> {
    let stream = driver
        .execute(
            handle,
            ExecuteRequest {
                sql,
                params: Vec::new(),
            },
        )
        .await?;
    let mut rx = stream.rows;
    let mut result: Option<String> = None;
    while let Some(page) = rx.recv().await {
        match page {
            Page::Rows { rows } if result.is_none() => {
                if let Some(row) = rows.into_iter().next() {
                    if let Some(v) = row.values.into_iter().next() {
                        result = Some(value_to_text(v));
                    }
                }
            }
            Page::Error { error } => return Err(error),
            Page::Done { .. } => break,
            _ => {}
        }
    }
    result.ok_or_else(|| {
        DriverError::new(
            Code::UndefinedObject,
            "DDL query returned no rows — object may not exist",
        )
    })
}

fn value_to_text(v: Value) -> String {
    match v {
        Value::Text(s) => s,
        Value::Null => String::new(),
        other => format!("{other:?}"),
    }
}

fn qualified_name(path: &ObjectPath, engine: Engine) -> String {
    let schema = path.schema.as_deref();
    match (engine, schema) {
        (Engine::Postgres, Some(s)) => {
            format!(
                "{}.{}",
                quote_ident(s, engine),
                quote_ident(&path.name, engine)
            )
        }
        (Engine::Postgres, None) => quote_ident(&path.name, engine),
        (Engine::SqlServer, Some(s)) => {
            format!(
                "{}.{}",
                quote_ident(s, engine),
                quote_ident(&path.name, engine)
            )
        }
        (Engine::SqlServer, None) => quote_ident(&path.name, engine),
    }
}

fn quote_ident(name: &str, engine: Engine) -> String {
    match engine {
        Engine::Postgres => {
            let escaped = name.replace('"', "\"\"");
            format!("\"{escaped}\"")
        }
        Engine::SqlServer => {
            let escaped = name.replace(']', "]]");
            format!("[{escaped}]")
        }
    }
}

fn type_to_sql(t: &TypeRef, engine: Engine) -> String {
    // Prefer the engine-native name when the driver preserved it via
    // Value::Engine facets — that's how deep-schema type introspection
    // reports SQL Server's `nvarchar(64)` etc. Fall back to a
    // Primitive-to-generic-SQL mapping.
    match t {
        TypeRef::Engine { name, .. } => name.clone(),
        TypeRef::Primitive(p) => primitive_to_sql(*p, engine),
    }
}

fn primitive_to_sql(p: sift_protocol::PrimitiveType, engine: Engine) -> String {
    use sift_protocol::PrimitiveType as P;
    let (pg, ms) = match p {
        P::Bool => ("boolean", "bit"),
        P::Int16 => ("smallint", "smallint"),
        P::Int32 => ("integer", "int"),
        P::Int64 => ("bigint", "bigint"),
        P::Float32 => ("real", "real"),
        P::Float64 => ("double precision", "float"),
        P::Decimal => ("numeric", "decimal"),
        P::Text => ("text", "nvarchar(max)"),
        P::Blob => ("bytea", "varbinary(max)"),
        P::Date => ("date", "date"),
        P::Time => ("time", "time"),
        P::Timestamp => ("timestamp", "datetime2"),
        P::TimestampTz => ("timestamptz", "datetimeoffset"),
        P::Uuid => ("uuid", "uniqueidentifier"),
        P::Json => ("json", "nvarchar(max)"),
        P::Jsonb => ("jsonb", "nvarchar(max)"),
        P::Interval => ("interval", "nvarchar(64)"),
    };
    match engine {
        Engine::Postgres => pg.to_string(),
        Engine::SqlServer => ms.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pg_regprocedure_name_includes_argument_signature() {
        let path = ObjectPath {
            catalog: None,
            schema: Some("public".into()),
            name: "overloaded".into(),
            kind: Some(ObjectKind::ScalarFunction),
            routine_args: Some(vec!["integer".into(), "text".into()]),
        };
        assert_eq!(
            pg_regprocedure_name(&path),
            "\"public\".\"overloaded\"(integer, text)"
        );
    }

    #[test]
    fn pg_regprocedure_name_handles_nullary_signature() {
        let path = ObjectPath {
            catalog: None,
            schema: Some("public".into()),
            name: "answer".into(),
            kind: Some(ObjectKind::ScalarFunction),
            routine_args: Some(Vec::new()),
        };
        assert_eq!(pg_regprocedure_name(&path), "\"public\".\"answer\"()");
    }
}
