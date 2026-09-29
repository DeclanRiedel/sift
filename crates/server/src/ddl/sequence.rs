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
WITH target AS (
    SELECT s.*, c.oid, c.relname, c.relowner, n.nspname
    FROM pg_catalog.pg_sequence s
    JOIN pg_catalog.pg_class c ON c.oid = s.seqrelid
    JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
    WHERE c.oid = to_regclass('{name}') AND c.relkind = 'S'
), ownership AS (
    SELECT d.objid, count(*) AS dependency_count,
        min(format('ALTER SEQUENCE %I.%I OWNED BY %I.%I.%I;',
            t.nspname, t.relname, owner_namespace.nspname,
            owner_table.relname, owner_column.attname)) AS clause
    FROM target t
    JOIN pg_catalog.pg_depend d ON d.classid = 'pg_catalog.pg_class'::regclass
        AND d.objid = t.oid AND d.refclassid = 'pg_catalog.pg_class'::regclass
        AND d.deptype = 'a' AND d.refobjsubid > 0
    JOIN pg_catalog.pg_class owner_table ON owner_table.oid = d.refobjid
    JOIN pg_catalog.pg_namespace owner_namespace ON owner_namespace.oid = owner_table.relnamespace
    JOIN pg_catalog.pg_attribute owner_column ON owner_column.attrelid = owner_table.oid
        AND owner_column.attnum = d.refobjsubid AND NOT owner_column.attisdropped
    GROUP BY d.objid
)
SELECT CASE
    WHEN NOT (pg_catalog.pg_has_role(current_user, t.relowner, 'USAGE')
        OR current_setting('is_superuser') = 'on')
        THEN 'sift:denied:sequence ownership is required for native DDL'
    WHEN EXISTS (SELECT 1 FROM pg_catalog.pg_depend d
        WHERE d.classid = 'pg_catalog.pg_class'::regclass AND d.objid = t.oid
          AND d.deptype = 'e')
        THEN 'sift:unsupported:extension member sequence is exported with its extension'
    WHEN EXISTS (SELECT 1 FROM pg_catalog.pg_depend d
        WHERE d.classid = 'pg_catalog.pg_class'::regclass AND d.objid = t.oid
          AND d.deptype = 'i')
        THEN 'sift:unsupported:identity sequence is exported with its table'
    WHEN EXISTS (SELECT 1 FROM pg_catalog.pg_depend d
        WHERE d.classid = 'pg_catalog.pg_class'::regclass AND d.objid = t.oid
          AND d.refclassid = 'pg_catalog.pg_class'::regclass AND d.deptype = 'a'
          AND (d.refobjsubid <= 0 OR NOT EXISTS
            (SELECT 1 FROM ownership o WHERE o.objid = t.oid)))
        OR coalesce((SELECT o.dependency_count FROM ownership o WHERE o.objid = t.oid), 0) > 1
        THEN 'sift:unsupported:sequence ownership dependency is ambiguous'
    ELSE format(
        'CREATE SEQUENCE %I.%I AS %s INCREMENT BY %s MINVALUE %s MAXVALUE %s START WITH %s CACHE %s %s;',
        t.nspname, t.relname, format_type(t.seqtypid, NULL), t.seqincrement,
        t.seqmin, t.seqmax, t.seqstart, t.seqcache,
        CASE WHEN t.seqcycle THEN 'CYCLE' ELSE 'NO CYCLE' END)
        || coalesce(E'\n' || (SELECT o.clause FROM ownership o WHERE o.objid = t.oid), '')
END
FROM target t
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
    if engine == Engine::Postgres {
        super::native::definition(driver, handle, sql, engine).await
    } else {
        fetch_scalar_text(driver, handle, sql).await
    }
}
