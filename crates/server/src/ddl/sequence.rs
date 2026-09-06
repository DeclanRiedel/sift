//! Sequence definitions preserve configuration, never the live counter.

use super::*;

pub(super) async fn generate_sequence_ddl(
    driver: &dyn Driver,
    handle: sift_driver_api::ConnHandle,
    object: &ObjectPath,
    engine: Engine,
) -> Result<String, DriverError> {
    let name = qualified_name(object, engine).replace('\'', "''");
    // Catalog contracts: PostgreSQL pg_sequence and SQL Server sys.sequences.
    // https://www.postgresql.org/docs/current/catalog-pg-sequence.html
    // https://learn.microsoft.com/en-us/sql/relational-databases/system-catalog-views/sys-sequences-transact-sql
    let sql = match engine {
        Engine::Sqlite => {
            return Err(DriverError::new(
                Code::UnsupportedForEngine,
                "use native SQLite object DDL",
            ))
        }
        Engine::Postgres => format!(
            r#"
SELECT format(
    'CREATE SEQUENCE %I.%I AS %s INCREMENT BY %s MINVALUE %s MAXVALUE %s START WITH %s CACHE %s %s;',
    n.nspname, c.relname, format_type(s.seqtypid, NULL), s.seqincrement,
    s.seqmin, s.seqmax, s.seqstart, s.seqcache,
    CASE WHEN s.seqcycle THEN 'CYCLE' ELSE 'NO CYCLE' END)
FROM pg_catalog.pg_sequence s
JOIN pg_catalog.pg_class c ON c.oid = s.seqrelid
JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
WHERE c.oid = '{name}'::regclass
"#
        ),
        Engine::SqlServer => format!(
            r#"
SELECT N'CREATE SEQUENCE ' + QUOTENAME(SCHEMA_NAME(s.schema_id)) + N'.' + QUOTENAME(s.name)
    + N' AS ' + CASE WHEN t.is_user_defined = 1
        THEN QUOTENAME(SCHEMA_NAME(t.schema_id)) + N'.' + QUOTENAME(t.name)
        WHEN t.name IN (N'decimal', N'numeric')
        THEN QUOTENAME(t.name) + N'(' + CONVERT(nvarchar(10), s.precision) + N',0)'
        ELSE QUOTENAME(t.name) END
    + N' START WITH ' + CONVERT(nvarchar(100), s.start_value)
    + N' INCREMENT BY ' + CONVERT(nvarchar(100), s.increment)
    + N' MINVALUE ' + CONVERT(nvarchar(100), s.minimum_value)
    + N' MAXVALUE ' + CONVERT(nvarchar(100), s.maximum_value)
    + CASE WHEN s.is_cycling = 1 THEN N' CYCLE' ELSE N' NO CYCLE' END
    + CASE WHEN s.is_cached = 0 THEN N' NO CACHE'
        WHEN s.cache_size IS NULL THEN N' CACHE'
        ELSE N' CACHE ' + CONVERT(nvarchar(20), s.cache_size) END + N';'
FROM sys.sequences s
JOIN sys.types t ON t.user_type_id = s.user_type_id
WHERE s.object_id = OBJECT_ID(N'{name}', N'SO')
"#
        ),
    };
    fetch_scalar_text(driver, handle, sql).await
}
