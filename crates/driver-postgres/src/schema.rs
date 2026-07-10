//! Schema introspection against `pg_catalog` (Shallow pass) and
//! `information_schema.columns` + `pg_constraint` + `pg_indexes` +
//! `pg_trigger` (Deep pass).

use std::collections::BTreeMap;

use deadpool_postgres::Object as PooledConn;
use sift_protocol::{
    CatalogTree, ColumnMetadata, ConstraintInfo, ConstraintKind, IndexInfo, IndexKind, ObjectInfo,
    ObjectKind, ObjectPath, SchemaDepth, SchemaScope, SchemaSnapshot, SchemaTree, TypeCategory,
    TypeRef,
};
use sift_protocol::{DriverError, SchemaFilter};

use crate::pg_err;

/// Build a [`SchemaSnapshot`] for the requested scope.
///
/// Postgres has a single catalog per connection (the database name we
/// connected to); the snapshot contains exactly one `CatalogTree` named
/// after it. `Shallow` lists schema + object names + kinds; `Deep` lists
/// columns, indexes, constraints, triggers for the requested object.
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
        SchemaDepth::Shallow => shallow_tree(conn, &current_db, scope.filter.as_ref()).await?,
        SchemaDepth::Deep { object } => deep_tree(conn, &current_db, object).await?,
    };
    snapshot.trees.push(tree);
    Ok(snapshot)
}

async fn shallow_tree(
    conn: &PooledConn,
    current_db: &str,
    filter: Option<&SchemaFilter>,
) -> Result<CatalogTree, DriverError> {
    // Single round-trip: all schemas + objects (excluding system schemas).
    // `name_pattern` pushes down to a LIKE, `schemas` pushes down to
    // n.nspname = ANY($2::text[]) when supplied; `kinds` filters after
    // fetching because it maps to relkind chars we already read.
    let like = filter
        .and_then(|f| f.name_pattern.as_deref())
        .map(to_pg_like)
        .unwrap_or_else(|| "%".to_string());
    let schemas_filter: Option<Vec<String>> = filter.and_then(|f| f.schemas.clone());
    let kinds_filter: Option<Vec<ObjectKind>> = filter.and_then(|f| f.kinds.clone());

    let rel_rows = if let Some(schemas) = schemas_filter.as_ref() {
        conn.query(
            "SELECT n.nspname AS schema_name,
                    c.relname AS object_name,
                    c.relkind AS relkind
             FROM pg_class c
             JOIN pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname NOT IN ('pg_catalog', 'information_schema')
               AND n.nspname NOT LIKE 'pg_toast%'
               AND c.relkind IN ('r', 'v', 'm', 'S', 'f', 'p')
               AND c.relname LIKE $1
               AND n.nspname = ANY($2::text[])
             ORDER BY 1, 2",
            &[&like, schemas],
        )
        .await
        .map_err(pg_err)?
    } else {
        conn.query(
            "SELECT n.nspname AS schema_name,
                    c.relname AS object_name,
                    c.relkind AS relkind
             FROM pg_class c
             JOIN pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname NOT IN ('pg_catalog', 'information_schema')
               AND n.nspname NOT LIKE 'pg_toast%'
               AND c.relkind IN ('r', 'v', 'm', 'S', 'f', 'p')
               AND c.relname LIKE $1
             ORDER BY 1, 2",
            &[&like],
        )
        .await
        .map_err(pg_err)?
    };

    let proc_rows = if let Some(schemas) = schemas_filter.as_ref() {
        conn.query(
            "SELECT n.nspname AS schema_name,
                    p.proname AS object_name,
                    p.prokind,
                    p.proretset,
                    ARRAY(
                        SELECT format_type(arg_oid::oid, NULL)
                        FROM unnest(string_to_array(p.proargtypes::text, ' '))
                             WITH ORDINALITY AS args(arg_oid, ord)
                        WHERE arg_oid <> ''
                        ORDER BY ord
                    ) AS arg_types
             FROM pg_proc p
             JOIN pg_namespace n ON n.oid = p.pronamespace
             WHERE n.nspname NOT IN ('pg_catalog', 'information_schema')
               AND n.nspname NOT LIKE 'pg_toast%'
               AND p.prokind IN ('f', 'p')
               AND p.proname LIKE $1
               AND n.nspname = ANY($2::text[])
             ORDER BY 1, 2",
            &[&like, schemas],
        )
        .await
        .map_err(pg_err)?
    } else {
        conn.query(
            "SELECT n.nspname AS schema_name,
                    p.proname AS object_name,
                    p.prokind,
                    p.proretset,
                    ARRAY(
                        SELECT format_type(arg_oid::oid, NULL)
                        FROM unnest(string_to_array(p.proargtypes::text, ' '))
                             WITH ORDINALITY AS args(arg_oid, ord)
                        WHERE arg_oid <> ''
                        ORDER BY ord
                    ) AS arg_types
             FROM pg_proc p
             JOIN pg_namespace n ON n.oid = p.pronamespace
             WHERE n.nspname NOT IN ('pg_catalog', 'information_schema')
               AND n.nspname NOT LIKE 'pg_toast%'
               AND p.prokind IN ('f', 'p')
               AND p.proname LIKE $1
             ORDER BY 1, 2",
            &[&like],
        )
        .await
        .map_err(pg_err)?
    };

    let mut by_schema: BTreeMap<String, Vec<ObjectInfo>> = BTreeMap::new();
    for row in rel_rows {
        let schema_name: String = row.get(0);
        let object_name: String = row.get(1);
        let relkind: i8 = row.get(2);
        let Some(kind) = relkind_to_kind(relkind as u8) else {
            continue;
        };
        if let Some(kinds) = kinds_filter.as_ref() {
            if !kinds.contains(&kind) {
                continue;
            }
        }
        by_schema
            .entry(schema_name)
            .or_default()
            .push(ObjectInfo::new(object_name, kind));
    }
    for row in proc_rows {
        let schema_name: String = row.get(0);
        let object_name: String = row.get(1);
        let prokind: i8 = row.get(2);
        let proretset: bool = row.get(3);
        let routine_args: Vec<String> = row.get(4);
        let Some(kind) = prokind_to_kind(prokind as u8, proretset) else {
            continue;
        };
        if let Some(kinds) = kinds_filter.as_ref() {
            if !kinds.contains(&kind) {
                continue;
            }
        }
        let mut info = ObjectInfo::new(object_name, kind);
        info.routine_args = Some(routine_args);
        by_schema.entry(schema_name).or_default().push(info);
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

    let columns = query_columns(conn, schema_name, object_name).await?;
    let kind = object.kind.unwrap_or(ObjectKind::Table);
    let (indexes, constraints, triggers) = if is_introspectable(&kind) {
        let oid = resolve_oid(conn, schema_name, object_name).await?;
        if let Some(oid) = oid {
            let indexes = query_indexes(conn, oid).await?;
            let constraints = query_constraints(conn, oid).await?;
            let triggers = query_triggers(conn, oid).await?;
            (indexes, constraints, triggers)
        } else {
            Default::default()
        }
    } else {
        Default::default()
    };

    let object_info = ObjectInfo {
        name: object_name.clone(),
        kind,
        routine_args: object.routine_args.clone(),
        columns,
        indexes,
        constraints,
        triggers,
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

fn is_introspectable(k: &ObjectKind) -> bool {
    matches!(
        k,
        ObjectKind::Table
            | ObjectKind::View
            | ObjectKind::MaterializedView
            | ObjectKind::ForeignTable
            | ObjectKind::PartitionedTable
    )
}

/// Resolve a relation OID for the (schema, name) pair. Returns None if the
/// object doesn't exist or isn't a relation.
async fn resolve_oid(
    conn: &PooledConn,
    schema: &str,
    name: &str,
) -> Result<Option<u32>, DriverError> {
    let row = conn
        .query_opt(
            "SELECT c.oid
             FROM pg_class c
             JOIN pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = $1 AND c.relname = $2",
            &[&schema, &name],
        )
        .await
        .map_err(pg_err)?;
    Ok(row.map(|r| r.get(0)))
}

async fn query_columns(
    conn: &PooledConn,
    schema: &str,
    name: &str,
) -> Result<Vec<ColumnMetadata>, DriverError> {
    // We use pg_attribute (not information_schema.columns) so we get the OID
    // for type_ref mapping, plus attnum/identity/NOT NULL.
    let rows = conn
        .query(
            "SELECT a.attname AS column_name,
                    a.atttypid AS type_oid,
                    a.attnotnull AS not_null,
                    a.attidentity AS identity,
                    pg_get_expr(ad.adbin, ad.adrelid) AS default_expr
             FROM pg_attribute a
             JOIN pg_class c ON c.oid = a.attrelid
             JOIN pg_namespace n ON n.oid = c.relnamespace
             LEFT JOIN pg_attrdef ad ON ad.adrelid = a.attrelid AND ad.adnum = a.attnum
             WHERE n.nspname = $1 AND c.relname = $2 AND a.attnum > 0 AND NOT a.attisdropped
             ORDER BY a.attnum",
            &[&schema, &name],
        )
        .await
        .map_err(pg_err)?;

    // PK column set for primary_key flag.
    let pk_columns = pk_column_set(conn, schema, name).await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let col_name: String = row.get(0);
        let type_oid: u32 = row.get(1);
        let not_null: bool = row.get(2);
        let identity: i8 = row.get(3);
        let default_expr: Option<String> = row.get(4);

        // Build TypeRef from the type OID via tokio_postgres::Type::from_oid.
        let type_ref = tokio_postgres::types::Type::from_oid(type_oid)
            .map(|t| crate::decode::pg_type_to_type_ref(&t))
            .unwrap_or_else(|| TypeRef::Engine {
                engine: sift_protocol::Engine::Postgres,
                name: format!("oid={type_oid}"),
                category: TypeCategory::Other,
            });

        let auto_increment =
            identity as u8 != b' ' || default_expr.as_deref().is_some_and(is_serial_default);

        out.push(ColumnMetadata {
            name: col_name.clone(),
            type_ref,
            nullable: if not_null {
                sift_protocol::Nullability::NotNullable
            } else {
                sift_protocol::Nullability::Nullable
            },
            auto_increment,
            primary_key: pk_columns.contains(&col_name),
            facets: Default::default(),
        });
    }
    Ok(out)
}

/// Set of column names that participate in the primary key of (schema, name).
async fn pk_column_set(
    conn: &PooledConn,
    schema: &str,
    name: &str,
) -> Result<std::collections::HashSet<String>, DriverError> {
    let rows = conn
        .query(
            "SELECT a.attname
             FROM pg_index i
             JOIN pg_class c ON c.oid = i.indrelid
             JOIN pg_namespace n ON n.oid = c.relnamespace
             JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum = ANY(i.indkey)
             WHERE n.nspname = $1 AND c.relname = $2 AND i.indisprimary
             ORDER BY a.attnum",
            &[&schema, &name],
        )
        .await
        .map_err(pg_err)?;
    Ok(rows.into_iter().map(|r| r.get::<_, String>(0)).collect())
}

async fn query_indexes(conn: &PooledConn, oid: u32) -> Result<Vec<IndexInfo>, DriverError> {
    // pg_index gives us indkey (column attnums) and indisunique/indisprimary.
    // We resolve attnums to names via generate_subscripts + pg_attribute.
    let rows = conn
        .query(
            "SELECT ci.relname AS index_name,
                    i.indisunique,
                    i.indisprimary,
                    am.amname,
                    pg_get_expr(i.indpred, i.indrelid) AS pred,
                    array_agg(a.attname ORDER BY k.ord) AS cols
             FROM pg_index i
             JOIN pg_class ci ON ci.oid = i.indexrelid
             JOIN pg_class c ON c.oid = i.indrelid
             JOIN pg_am am ON am.oid = ci.relam
             LEFT JOIN LATERAL unnest(i.indkey) WITH ORDINALITY AS k(attnum, ord) ON true
             LEFT JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum = k.attnum
             WHERE i.indrelid = $1
             GROUP BY ci.relname, i.indisunique, i.indisprimary, am.amname, pred
             ORDER BY ci.relname",
            &[&oid],
        )
        .await
        .map_err(pg_err)?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let name: String = row.get(0);
        let unique: bool = row.get(1);
        let is_pk: bool = row.get(2);
        let am: String = row.get(3);
        let pred: Option<String> = row.get(4);
        let cols: Vec<Option<String>> = row.get(5);

        out.push(IndexInfo {
            name,
            columns: cols.into_iter().flatten().collect(),
            unique,
            primary_key: is_pk,
            kind: map_index_kind(&am),
            partial_predicate: pred.filter(|p| !p.is_empty()),
        });
    }
    Ok(out)
}

async fn query_constraints(
    conn: &PooledConn,
    oid: u32,
) -> Result<Vec<ConstraintInfo>, DriverError> {
    let rows = conn
        .query(
            "SELECT con.conname,
                    con.contype,
                    pg_get_constraintdef(con.oid),
                    con.confrelid,
                    array_agg(a.attname ORDER BY u.ord) FILTER (WHERE a.attname IS NOT NULL) AS cols
             FROM pg_constraint con
             LEFT JOIN LATERAL unnest(con.conkey) WITH ORDINALITY AS u(attnum, ord) ON true
             LEFT JOIN pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = u.attnum
             WHERE con.conrelid = $1
             GROUP BY con.conname, con.contype, pg_get_constraintdef(con.oid), con.confrelid
             ORDER BY con.conname",
            &[&oid],
        )
        .await
        .map_err(pg_err)?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let name: String = row.get(0);
        let contype: i8 = row.get(1);
        let definition: Option<String> = row.get(2);
        let confrelid: Option<u32> = row.get(3);
        let cols: Option<Vec<Option<String>>> = row.get(4);

        let kind = match contype as u8 {
            b'p' => ConstraintKind::PrimaryKey,
            b'f' => ConstraintKind::ForeignKey,
            b'u' => ConstraintKind::Unique,
            b'c' => ConstraintKind::Check,
            b'x' => ConstraintKind::Exclusion,
            _ => ConstraintKind::Other,
        };

        // Resolve FK target table name if applicable.
        let references = if let Some(ref_oid) = confrelid.filter(|o| *o != 0) {
            fk_target(conn, ref_oid).await.ok().flatten()
        } else {
            None
        };

        out.push(ConstraintInfo {
            name,
            kind,
            columns: cols.unwrap_or_default().into_iter().flatten().collect(),
            definition,
            references,
        });
    }
    Ok(out)
}

async fn fk_target(conn: &PooledConn, ref_oid: u32) -> Result<Option<String>, DriverError> {
    let row = conn
        .query_opt(
            "SELECT n.nspname || '.' || c.relname
             FROM pg_class c
             JOIN pg_namespace n ON n.oid = c.relnamespace
             WHERE c.oid = $1",
            &[&ref_oid],
        )
        .await
        .map_err(pg_err)?;
    Ok(row.map(|r| r.get(0)))
}

async fn query_triggers(
    conn: &PooledConn,
    oid: u32,
) -> Result<Vec<sift_protocol::TriggerInfo>, DriverError> {
    let rows = conn
        .query(
            "SELECT t.tgname,
                    t.tgtype,
                    pg_get_triggerdef(t.oid)
             FROM pg_trigger t
             WHERE t.tgrelid = $1 AND NOT t.tgisinternal
             ORDER BY t.tgname",
            &[&oid],
        )
        .await
        .map_err(pg_err)?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let name: String = row.get(0);
        let tgtype: i32 = row.get(1);
        let definition: Option<String> = row.get(2);

        let (timing, events) = decode_tgtype(tgtype);
        out.push(sift_protocol::TriggerInfo {
            name,
            timing,
            events,
            columns: Vec::new(),
            definition,
        });
    }
    Ok(out)
}

/// Decode PG's `tgtype` bitmask into (timing, events). Bit layout (from
/// pg_trigger.h): ROW_LEVEL=1, BEFORE=2, INSERT=4, DELETE=8, UPDATE=16,
/// TRUNCATE=32, INSTEAD=64. AFTER is the absence of BEFORE and INSTEAD.
fn decode_tgtype(
    tgtype: i32,
) -> (
    sift_protocol::TriggerTiming,
    Vec<sift_protocol::TriggerEvent>,
) {
    use sift_protocol::TriggerEvent as E;
    use sift_protocol::TriggerTiming as T;
    let bits = tgtype;
    let timing = if bits & 2 != 0 {
        T::Before
    } else if bits & 64 != 0 {
        T::InsteadOf
    } else {
        T::After
    };
    let mut events = Vec::new();
    if bits & 4 != 0 {
        events.push(E::Insert);
    }
    if bits & 16 != 0 {
        events.push(E::Update);
    }
    if bits & 8 != 0 {
        events.push(E::Delete);
    }
    if bits & 32 != 0 {
        events.push(E::Truncate);
    }
    (timing, events)
}

fn map_index_kind(am: &str) -> IndexKind {
    match am {
        "btree" => IndexKind::Btree,
        "hash" => IndexKind::Hash,
        "gist" => IndexKind::Gist,
        "gin" => IndexKind::Gin,
        "brin" => IndexKind::Brin,
        "spgist" => IndexKind::Spgist,
        _ => IndexKind::Other,
    }
}

fn relkind_to_kind(byte: u8) -> Option<ObjectKind> {
    match byte {
        b'r' => Some(ObjectKind::Table),
        b'p' => Some(ObjectKind::PartitionedTable),
        b'v' => Some(ObjectKind::View),
        b'm' => Some(ObjectKind::MaterializedView),
        b'S' => Some(ObjectKind::Sequence),
        b'f' => Some(ObjectKind::ForeignTable),
        _ => None,
    }
}

fn prokind_to_kind(byte: u8, returns_set: bool) -> Option<ObjectKind> {
    match byte {
        b'p' => Some(ObjectKind::Procedure),
        b'f' if returns_set => Some(ObjectKind::TableValuedFunction),
        b'f' => Some(ObjectKind::ScalarFunction),
        _ => None,
    }
}

fn is_serial_default(default: &str) -> bool {
    // `nextval('..._seq'::regclass)` is how SERIAL/BIGSERIAL surface in
    // modern PG (the underlying identity). Identity columns surface via
    // `attidentity` instead, handled separately.
    default.starts_with("nextval(")
}

/// Translate a glob-style filter pattern (`*` → `%`, `?` → `_`) to PG LIKE
/// syntax. Backslashes are preserved as escapes for literal `%` / `_`.
fn to_pg_like(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len() + 8);
    for c in pattern.chars() {
        match c {
            '*' => out.push('%'),
            '?' => out.push('_'),
            '%' | '_' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}
