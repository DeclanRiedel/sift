//! DDL generation.
//!
//! Composes CREATE statements for a database object by calling
//! existing `Driver` methods — no new trait method is required, so
//! the ADR-017 driver-trait lock is undisturbed.
//!
//! Strategy per object kind:
//!
//! - **Tables**: read native catalogs through `Driver::execute`, preserving
//!   column and index definitions rather than reconstructing from the lossy
//!   explorer schema projection. Unsupported table properties fail explicitly.
//! - **Views / Materialized Views / Procedures / Functions**: use
//!   engine-native catalog functions via `Driver::execute`:
//!   - PG: `pg_get_viewdef(oid)`, `pg_get_functiondef(oid)`. The
//!     regnamespace / regprocedure casts resolve the identifier.
//!   - MSSQL: `OBJECT_DEFINITION(OBJECT_ID(...))`.
//!
//! Server-side composition means driver crates stay unchanged; the
//! DDL layer runs alongside HTTP handlers and depends only on
//! primitives that already exist.

mod native;
mod sequence;

use sift_driver_api::Driver;
use sift_protocol::{
    Code, DriverError, Engine, ExecuteRequest, ObjectDdl, ObjectKind, ObjectPath, Page, TypeRef,
    Value,
};

/// Fetch and format DDL for `object` on `driver`. Dispatches by
/// engine + kind. Errors bubble up from the underlying driver calls;
/// unsupported combinations return `Code::UnsupportedForEngine`.
pub async fn generate_ddl(
    driver: &dyn Driver,
    handle: sift_driver_api::ConnHandle,
    object: ObjectPath,
) -> Result<ObjectDdl, DriverError> {
    if driver.engine() == Engine::Sqlite {
        let ddl = driver
            .as_sqlite()
            .ok_or_else(|| DriverError::new(Code::UnsupportedForEngine, "SQLite DDL unavailable"))?
            .object_ddl(handle, object.clone())
            .await?;
        return Ok(ObjectDdl { path: object, ddl });
    }
    let kind = object.kind.unwrap_or(ObjectKind::Table);
    let engine = driver.engine();
    let ddl = match kind {
        ObjectKind::Table | ObjectKind::PartitionedTable => {
            native::table(driver, handle, &object, engine).await?
        }
        ObjectKind::Trigger => native::trigger(driver, handle, &object, engine).await?,
        ObjectKind::Type => native::user_type(driver, handle, &object, engine).await?,
        ObjectKind::Sequence => {
            sequence::generate_sequence_ddl(driver, handle, &object, engine).await?
        }
        ObjectKind::ForeignTable => {
            return Err(DriverError::new(
                Code::UnsupportedForEngine,
                "foreign-table DDL requires server and options metadata and is not supported",
            )
            .with_engine(engine));
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
        (Engine::Sqlite, _) => {
            return Err(DriverError::new(
                Code::UnsupportedForEngine,
                "use native SQLite object DDL",
            ))
        }
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
        Engine::Sqlite => {
            return Err(DriverError::new(
                Code::UnsupportedForEngine,
                "SQLite routines are unsupported",
            ))
        }
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
                transform: None,
            },
        )
        .await?;
    let mut rx = stream.rows;
    let mut result: Option<String> = None;
    let mut completed = false;
    while let Some(page) = rx.recv().await {
        match page {
            Page::Rows { rows } if result.is_none() => {
                if let Some(row) = rows.into_iter().next() {
                    if let Some(v) = row.values.into_iter().next() {
                        result =
                            match v {
                                Value::Text(text) if !text.trim().is_empty() => Some(text),
                                Value::Null | Value::TypedNull { .. } => None,
                                _ => return Err(DriverError::new(
                                    Code::DriverInternal,
                                    "DDL catalog query did not return a non-empty text definition",
                                )),
                            };
                    }
                }
            }
            Page::Error { error } => return Err(error),
            Page::Done { .. } => {
                completed = true;
                break;
            }
            _ => {}
        }
    }
    if !completed {
        return Err(DriverError::new(
            Code::DriverInternal,
            "DDL catalog stream ended before completion",
        ));
    }
    result.ok_or_else(|| {
        DriverError::new(
            Code::UndefinedObject,
            "DDL definition is missing or inaccessible",
        )
    })
}

pub(crate) fn qualified_name(path: &ObjectPath, engine: Engine) -> String {
    let schema = path.schema.as_deref();
    match (engine, schema) {
        (Engine::Postgres | Engine::Sqlite, Some(s)) => {
            format!(
                "{}.{}",
                quote_ident(s, engine),
                quote_ident(&path.name, engine)
            )
        }
        (Engine::Postgres | Engine::Sqlite, None) => quote_ident(&path.name, engine),
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

pub(crate) fn quote_ident(name: &str, engine: Engine) -> String {
    match engine {
        Engine::Postgres | Engine::Sqlite => {
            let escaped = name.replace('"', "\"\"");
            format!("\"{escaped}\"")
        }
        Engine::SqlServer => {
            let escaped = name.replace(']', "]]");
            format!("[{escaped}]")
        }
    }
}

pub(crate) fn type_to_sql(t: &TypeRef, engine: Engine) -> String {
    // Prefer the engine-native name when the driver preserved it via
    // Value::Native facets — that's how deep-schema type introspection
    // reports SQL Server's `nvarchar(64)` etc. Fall back to a
    // Primitive-to-generic-SQL mapping.
    match t {
        TypeRef::Native { name, .. } => name.clone(),
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
        Engine::Sqlite => match p {
            P::Int16 | P::Int32 | P::Int64 | P::Bool => "INTEGER",
            P::Float32 | P::Float64 => "REAL",
            P::Blob => "BLOB",
            _ => "TEXT",
        }
        .to_string(),
        Engine::SqlServer => ms.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn scalar_ddl_rejects_missing_invalid_and_incomplete_definitions() {
        use sift_driver_api::{mock::MockDriver, ConnHandle};
        use sift_protocol::Row;
        for (value, terminal, expected) in [
            (Value::Null, true, Code::UndefinedObject),
            (Value::Int64(7), true, Code::DriverInternal),
            (
                Value::Text("CREATE VIEW v AS SELECT 1".into()),
                false,
                Code::DriverInternal,
            ),
        ] {
            let mut pages = vec![Page::Rows {
                rows: vec![Row::new(vec![value])],
            }];
            if terminal {
                pages.push(Page::Done {
                    affected_rows: None,
                    warnings: Vec::new(),
                });
            }
            let driver = MockDriver::builder().execute_ok(pages).build();
            let error = fetch_scalar_text(
                &driver,
                ConnHandle::new(1, Engine::Postgres),
                "catalog read".into(),
            )
            .await
            .unwrap_err();
            assert_eq!(error.code, expected);
        }
    }

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
