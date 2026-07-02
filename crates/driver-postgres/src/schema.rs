//! Schema introspection against `pg_catalog` (Shallow pass) and
//! `information_schema.columns` (Deep pass).

use deadpool_postgres::Object as PooledConn;
use sift_protocol::{
    CatalogTree, ColumnMetadata, ObjectInfo, ObjectKind, ObjectPath, PrimitiveType, SchemaDepth,
    SchemaSnapshot, TypeCategory, TypeRef,
};
use sift_protocol::{DriverError, SchemaScope, SchemaTree};
use tokio_postgres::Row;

use crate::pg_err;

/// Build a [`SchemaSnapshot`] for the requested scope.
///
/// Postgres has a single catalog per connection (the database name we
/// connected to); the snapshot contains exactly one `CatalogTree` named
/// after it. `Shallow` lists schema + object names + kinds; `Deep` lists
/// columns for the requested object.
pub(crate) async fn introspect(
    conn: &PooledConn,
    scope: &SchemaScope,
) -> Result<SchemaSnapshot, DriverError> {
    let current_db: String = conn
        .query_one("SELECT current_database()", &[])
        .await
        .map_err(pg_err)?
        .get(0);

    let mut snapshot = SchemaSnapshot::empty(scope.clone());
    let tree = match &scope.depth {
        SchemaDepth::Shallow => shallow_tree(conn, &current_db).await?,
        SchemaDepth::Deep { object } => deep_tree(conn, &current_db, object).await?,
    };
    snapshot.trees.push(tree);
    Ok(snapshot)
}

async fn shallow_tree(conn: &PooledConn, current_db: &str) -> Result<CatalogTree, DriverError> {
    // Single round-trip: all schemas + objects (excluding system schemas).
    let rows = conn
        .query(
            "SELECT n.nspname AS schema_name,
                    c.relname AS object_name,
                    c.relkind AS relkind
             FROM pg_class c
             JOIN pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname NOT IN ('pg_catalog', 'information_schema')
               AND n.nspname NOT LIKE 'pg_toast%'
               AND c.relkind IN ('r', 'v', 'm', 'S', 'f', 'p')
             ORDER BY 1, 2",
            &[],
        )
        .await
        .map_err(pg_err)?;

    let mut by_schema: std::collections::BTreeMap<String, Vec<ObjectInfo>> =
        std::collections::BTreeMap::new();
    for row in rows {
        let schema_name: String = row.get(0);
        let object_name: String = row.get(1);
        let relkind: i8 = row.get(2);
        let relkind_byte = relkind as u8;
        let kind = match relkind_byte {
            b'r' | b'p' => ObjectKind::Table,
            b'v' => ObjectKind::View,
            b'm' => ObjectKind::MaterializedView,
            b'S' => ObjectKind::Sequence,
            b'f' => ObjectKind::Table, // foreign table — closest match
            _ => continue,
        };
        by_schema.entry(schema_name).or_default().push(ObjectInfo {
            name: object_name,
            kind,
            columns: Vec::new(),
        });
    }

    let schemas = by_schema
        .into_iter()
        .map(|(name, objects)| SchemaTree { name, objects })
        .collect();

    Ok(CatalogTree {
        name: current_db.to_string(),
        schemas,
    })
}

async fn deep_tree(
    conn: &PooledConn,
    current_db: &str,
    object: &ObjectPath,
) -> Result<CatalogTree, DriverError> {
    let schema_name = object.schema.as_deref().unwrap_or("public");
    let object_name = &object.name;

    let rows = conn
        .query(
            "SELECT column_name,
                    data_type,
                    is_nullable,
                    column_default,
                    ordinal_position
             FROM information_schema.columns
             WHERE table_schema = $1 AND table_name = $2
             ORDER BY ordinal_position",
            &[&schema_name, object_name],
        )
        .await
        .map_err(pg_err)?;

    let columns: Vec<ColumnMetadata> = rows.iter().map(col_from_info_schema_row).collect();

    let kind = object.kind.unwrap_or(ObjectKind::Table);
    let object_info = ObjectInfo {
        name: object_name.clone(),
        kind,
        columns,
    };

    // Deep pass returns a single-object tree scoped to the object's schema.
    let schema_tree = SchemaTree {
        name: schema_name.to_string(),
        objects: vec![object_info],
    };

    Ok(CatalogTree {
        name: current_db.to_string(),
        schemas: vec![schema_tree],
    })
}

fn col_from_info_schema_row(row: &Row) -> ColumnMetadata {
    let name: String = row.get(0);
    let data_type: String = row.get(1);
    let is_nullable: String = row.get(2);
    let column_default: Option<String> = row.get(3);
    let _ordinal: i32 = row.get(4);

    let type_ref = pg_data_type_to_type_ref(&data_type);
    let auto_increment = column_default
        .as_deref()
        .map(|d| d.starts_with("nextval("))
        .unwrap_or(false);
    let nullable = if is_nullable == "YES" {
        sift_protocol::Nullability::Nullable
    } else {
        sift_protocol::Nullability::NotNullable
    };

    ColumnMetadata {
        name,
        type_ref,
        nullable,
        auto_increment,
        primary_key: false, // requires pg_constraint join; deferred
        facets: Default::default(),
    }
}

/// Map `information_schema.columns.data_type` strings to [`TypeRef`]. Less
/// precise than the OID-based path used for live rows (which sees the actual
/// type, not the SQL standard name) but good enough for the Deep pass.
fn pg_data_type_to_type_ref(data_type: &str) -> TypeRef {
    let prim = match data_type {
        "boolean" => Some(PrimitiveType::Bool),
        "smallint" => Some(PrimitiveType::Int16),
        "integer" => Some(PrimitiveType::Int32),
        "bigint" => Some(PrimitiveType::Int64),
        "real" => Some(PrimitiveType::Float32),
        "double precision" => Some(PrimitiveType::Float64),
        "numeric" | "decimal" => Some(PrimitiveType::Decimal),
        "text" | "character varying" | "character" => Some(PrimitiveType::Text),
        "bytea" => Some(PrimitiveType::Blob),
        "date" => Some(PrimitiveType::Date),
        "time without time zone" => Some(PrimitiveType::Time),
        "timestamp without time zone" => Some(PrimitiveType::Timestamp),
        "timestamp with time zone" => Some(PrimitiveType::TimestampTz),
        "uuid" => Some(PrimitiveType::Uuid),
        "json" => Some(PrimitiveType::Json),
        "jsonb" => Some(PrimitiveType::Jsonb),
        _ => None,
    };
    prim.map(TypeRef::Primitive)
        .unwrap_or_else(|| TypeRef::Engine {
            engine: sift_protocol::Engine::Postgres,
            name: data_type.to_string(),
            category: TypeCategory::Other,
        })
}
