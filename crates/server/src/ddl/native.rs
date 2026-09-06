//! Catalog-owned DDL. No schema projection is treated as a lossless export.
use super::*;

pub(super) async fn table(
    driver: &dyn Driver,
    handle: sift_driver_api::ConnHandle,
    object: &ObjectPath,
    engine: Engine,
) -> Result<String, DriverError> {
    let template = match engine {
        Engine::Postgres => include_str!("sql/postgres-table.sql"),
        Engine::SqlServer => include_str!("sql/sqlserver-table.sql"),
    };
    let sql = template.replace(
        "__OBJECT__",
        &qualified_name(object, engine).replace('\'', "''"),
    );
    definition(driver, handle, sql, engine).await
}

pub(super) async fn trigger(
    driver: &dyn Driver,
    handle: sift_driver_api::ConnHandle,
    object: &ObjectPath,
    engine: Engine,
) -> Result<String, DriverError> {
    let name = object.name.replace('\'', "''");
    let schema = object
        .schema
        .as_deref()
        .ok_or_else(|| {
            DriverError::new(Code::InvalidParameterValue, "trigger DDL requires a schema")
        })?
        .replace('\'', "''");
    let sql = match engine {
        Engine::Postgres => format!(
            r#"
SELECT CASE WHEN count(*) > 1 THEN 'sift:unsupported:trigger name is ambiguous within schema'
ELSE min(pg_get_triggerdef(t.oid) || ';' || CASE t.tgenabled
WHEN 'D' THEN format(' ALTER TABLE %I.%I DISABLE TRIGGER %I;',n.nspname,c.relname,t.tgname)
WHEN 'R' THEN format(' ALTER TABLE %I.%I ENABLE REPLICA TRIGGER %I;',n.nspname,c.relname,t.tgname)
WHEN 'A' THEN format(' ALTER TABLE %I.%I ENABLE ALWAYS TRIGGER %I;',n.nspname,c.relname,t.tgname) ELSE '' END) END
FROM pg_catalog.pg_trigger t JOIN pg_catalog.pg_class c ON c.oid=t.tgrelid
JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace
WHERE NOT t.tgisinternal AND t.tgname='{name}' AND n.nspname='{schema}'"#
        ),
        Engine::SqlServer => {
            let qname = qualified_name(object, engine).replace('\'', "''");
            format!(
                r#"SELECT CASE WHEN m.uses_ansi_nulls=0 OR m.uses_quoted_identifier=0 OR UPPER(LTRIM(m.definition)) NOT LIKE N'CREATE%'
THEN N'sift:unsupported:trigger module settings or ALTER-only definition require separate export'
ELSE N'SET ANSI_NULLS ON; SET QUOTED_IDENTIFIER ON;'+CHAR(10)+N'GO'+CHAR(10)+OBJECT_DEFINITION(t.object_id) + CASE WHEN t.is_disabled=1 THEN
CHAR(10)+N'GO'+CHAR(10)+N'DISABLE TRIGGER '+QUOTENAME(OBJECT_SCHEMA_NAME(t.object_id))+N'.'+QUOTENAME(t.name)+N' ON '+QUOTENAME(OBJECT_SCHEMA_NAME(t.parent_id))+N'.'+QUOTENAME(OBJECT_NAME(t.parent_id))+N';' ELSE N'' END END
FROM sys.triggers t JOIN sys.sql_modules m ON m.object_id=t.object_id WHERE t.object_id=OBJECT_ID(N'{qname}') AND t.parent_class=1"#
            )
        }
    };
    definition(driver, handle, sql, engine).await
}

pub(super) async fn user_type(
    driver: &dyn Driver,
    handle: sift_driver_api::ConnHandle,
    object: &ObjectPath,
    engine: Engine,
) -> Result<String, DriverError> {
    let name = qualified_name(object, engine).replace('\'', "''");
    let sql = match engine {
        Engine::Postgres => format!(
            r#"
SELECT CASE t.typtype
WHEN 'e' THEN format('CREATE TYPE %I.%I AS ENUM (',n.nspname,t.typname) ||
    (SELECT string_agg(quote_literal(enumlabel), ', ' ORDER BY enumsortorder) FROM pg_catalog.pg_enum WHERE enumtypid=t.oid) || ');'
WHEN 'c' THEN CASE WHEN c.relkind <> 'c' THEN 'sift:unsupported:implicit table row types are exported with their table'
ELSE format('CREATE TYPE %I.%I AS (',n.nspname,t.typname) ||
    (SELECT string_agg(format('%I %s',a.attname,format_type(a.atttypid,a.atttypmod)) ||
    CASE WHEN a.attcollation<>0 AND a.attcollation<>ty.typcollation THEN format(' COLLATE %I.%I',cn.nspname,co.collname) ELSE '' END, ', ' ORDER BY a.attnum)
    FROM pg_catalog.pg_attribute a JOIN pg_catalog.pg_type ty ON ty.oid=a.atttypid
    LEFT JOIN pg_catalog.pg_collation co ON co.oid=a.attcollation LEFT JOIN pg_catalog.pg_namespace cn ON cn.oid=co.collnamespace
    WHERE a.attrelid=t.typrelid AND a.attnum>0 AND NOT a.attisdropped) || ');' END
WHEN 'd' THEN format('CREATE DOMAIN %I.%I AS %s',n.nspname,t.typname,format_type(t.typbasetype,t.typtypmod)) ||
    CASE WHEN t.typcollation<>0 AND t.typcollation<>(SELECT typcollation FROM pg_catalog.pg_type WHERE oid=t.typbasetype)
    THEN format(' COLLATE %I.%I',cn.nspname,co.collname) ELSE '' END ||
    CASE WHEN t.typdefaultbin IS NOT NULL THEN ' DEFAULT '||pg_get_expr(t.typdefaultbin,0) ELSE '' END ||
    CASE WHEN t.typnotnull AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint WHERE contypid=t.oid AND contype='n') THEN ' NOT NULL' ELSE '' END ||
    COALESCE((SELECT string_agg(format(' CONSTRAINT %I %s',conname,pg_get_constraintdef(oid)),'' ORDER BY oid) FROM pg_catalog.pg_constraint WHERE contypid=t.oid),'') || ';'
ELSE 'sift:unsupported:type family is outside enum/composite/domain export' END
FROM pg_catalog.pg_type t JOIN pg_catalog.pg_namespace n ON n.oid=t.typnamespace
LEFT JOIN pg_catalog.pg_class c ON c.oid=t.typrelid
LEFT JOIN pg_catalog.pg_collation co ON co.oid=t.typcollation LEFT JOIN pg_catalog.pg_namespace cn ON cn.oid=co.collnamespace
WHERE t.oid=to_regtype('{name}')"#
        ),
        Engine::SqlServer => format!(
            r#"
SELECT CASE WHEN t.is_assembly_type=1 OR t.is_table_type=1 OR t.default_object_id<>0 OR t.rule_object_id<>0
THEN N'sift:unsupported:CLR/table types and bound defaults/rules require separate export'
ELSE N'CREATE TYPE '+QUOTENAME(SCHEMA_NAME(t.schema_id))+N'.'+QUOTENAME(t.name)+N' FROM '+QUOTENAME(b.name)+
CASE WHEN b.name IN (N'varchar',N'char',N'varbinary',N'binary',N'nvarchar',N'nchar')
THEN N'('+CASE WHEN t.max_length=-1 THEN N'max' ELSE CONVERT(nvarchar(10),t.max_length/CASE WHEN b.name IN (N'nvarchar',N'nchar') THEN 2 ELSE 1 END) END+N')'
WHEN b.name IN (N'decimal',N'numeric') THEN N'('+CONVERT(nvarchar(10),t.precision)+N','+CONVERT(nvarchar(10),t.scale)+N')'
WHEN b.name IN (N'datetime2',N'datetimeoffset',N'time') THEN N'('+CONVERT(nvarchar(10),t.scale)+N')'
WHEN b.name=N'float' THEN N'('+CONVERT(nvarchar(10),t.precision)+N')' ELSE N'' END+
CASE WHEN t.is_nullable=1 THEN N' NULL;' ELSE N' NOT NULL;' END END
FROM sys.types t LEFT JOIN sys.types b ON b.user_type_id=t.system_type_id AND b.user_type_id=b.system_type_id
WHERE t.user_type_id=TYPE_ID(N'{name}') AND t.is_user_defined=1"#
        ),
    };
    definition(driver, handle, sql, engine).await
}

async fn definition(
    driver: &dyn Driver,
    handle: sift_driver_api::ConnHandle,
    sql: String,
    engine: Engine,
) -> Result<String, DriverError> {
    let ddl = fetch_scalar_text(driver, handle, sql).await?;
    match ddl.strip_prefix("sift:unsupported:") {
        Some(reason) => {
            Err(DriverError::new(Code::UnsupportedForEngine, reason).with_engine(engine))
        }
        None => Ok(ddl),
    }
}
