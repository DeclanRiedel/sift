//! Schema introspection against `pg_catalog` (Shallow pass) and
//! `information_schema.columns` + `pg_constraint` + `pg_indexes` +
//! `pg_trigger` (Deep pass).

use std::collections::{BTreeMap, HashMap};

use deadpool_postgres::Object as PooledConn;
use sift_protocol::{
    CatalogColumnPair, CatalogCoverage, CatalogEdgeKind, CatalogGraphData, CatalogGraphOptions,
    CatalogNodeKind, CatalogTree, ColumnMetadata, ConstraintInfo, ConstraintKind, IndexInfo,
    IndexKind, ObjectInfo, ObjectKind, ObjectPath, SchemaDepth, SchemaScope, SchemaSnapshot,
    SchemaTree, TypeCategory, TypeRef,
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
        SchemaDepth::Graph { options } => {
            let tree = graph_tree(conn, &current_db, options).await?;
            let mut graph = sift_core::catalog::graph_from_trees(
                std::slice::from_ref(&tree),
                CatalogCoverage::complete(),
                &format!("postgres:{current_db}"),
            );
            enrich_graph_identity_and_foreign_keys(conn, &mut graph, &tree).await?;
            sift_core::catalog::project_graph(&mut graph, options);
            snapshot.graph = Some(graph);
            tree
        }
    };
    snapshot.trees.push(tree);
    Ok(snapshot)
}

async fn graph_tree(
    conn: &PooledConn,
    current_db: &str,
    options: &CatalogGraphOptions,
) -> Result<CatalogTree, DriverError> {
    let filter = SchemaFilter {
        catalogs: None,
        schemas: options.schemas.clone(),
        kinds: graph_object_filter(options),
        name_pattern: None,
    };
    let mut tree = shallow_tree(conn, current_db, Some(&filter)).await?;
    bulk_enrich_tree(conn, &mut tree, options.include_definitions).await?;
    Ok(tree)
}

fn graph_object_filter(options: &CatalogGraphOptions) -> Option<Vec<ObjectKind>> {
    let kinds = options.kinds.as_ref()?;
    let object_kinds = kinds
        .iter()
        .filter_map(|kind| object_kind(*kind))
        .collect::<Vec<_>>();
    (object_kinds.len() == kinds.len()).then_some(object_kinds)
}

async fn bulk_enrich_tree(
    conn: &PooledConn,
    tree: &mut CatalogTree,
    include_definitions: bool,
) -> Result<(), DriverError> {
    let positions = object_positions(tree);
    if positions.is_empty() {
        return Ok(());
    }
    let schemas = tree
        .schemas
        .iter()
        .map(|schema| schema.name.clone())
        .collect::<Vec<_>>();

    let column_rows = conn
        .query(
            "SELECT n.nspname, c.relname, a.attname, a.atttypid,
                    a.attnotnull, a.attidentity, a.attndims,
                    pg_get_expr(ad.adbin, ad.adrelid),
                    EXISTS (
                        SELECT 1 FROM pg_index pi
                        WHERE pi.indrelid = c.oid AND pi.indisprimary
                          AND a.attnum = ANY(pi.indkey)
                    ) AS primary_key
             FROM pg_attribute a
             JOIN pg_class c ON c.oid = a.attrelid
             JOIN pg_namespace n ON n.oid = c.relnamespace
             LEFT JOIN pg_attrdef ad
               ON ad.adrelid = a.attrelid AND ad.adnum = a.attnum
             WHERE n.nspname = ANY($1::text[])
               AND a.attnum > 0 AND NOT a.attisdropped
             ORDER BY n.nspname, c.relname, a.attnum",
            &[&schemas],
        )
        .await
        .map_err(pg_err)?;
    for row in column_rows {
        let schema: String = row.get(0);
        let object: String = row.get(1);
        let Some(info) = positioned_object_mut(tree, &positions, &schema, &object) else {
            continue;
        };
        let name: String = row.get(2);
        let type_oid: u32 = row.get(3);
        let not_null: bool = row.get(4);
        let identity: i8 = row.get(5);
        let array_dims: i16 = row.get(6);
        let default_expr: Option<String> = row.get(7);
        let primary_key: bool = row.get(8);
        let type_ref = tokio_postgres::types::Type::from_oid(type_oid)
            .map(|data_type| crate::decode::pg_type_to_type_ref(&data_type))
            .unwrap_or_else(|| TypeRef::Native {
                provider_id: sift_protocol::Engine::Postgres.provider_id(),
                name: format!("oid={type_oid}"),
                category: TypeCategory::Other,
            });
        let auto_increment =
            is_pg_identity(identity) || default_expr.as_deref().is_some_and(is_serial_default);
        info.columns.push(ColumnMetadata {
            name,
            type_ref,
            nullable: if not_null {
                sift_protocol::Nullability::NotNullable
            } else {
                sift_protocol::Nullability::Nullable
            },
            auto_increment,
            primary_key,
            facets: sift_protocol::EngineColumnFacets {
                postgres: Some(sift_protocol::PgColumnFacets {
                    oid: Some(type_oid),
                    array_dims: u8::try_from(array_dims).unwrap_or(u8::MAX),
                    is_identity: is_pg_identity(identity),
                    default_expr: include_definitions.then_some(default_expr).flatten(),
                    enum_values: None,
                }),
                sql_server: None,
            },
        });
    }

    let index_rows = conn
        .query(
            "SELECT n.nspname, c.relname, ci.relname, i.indisunique,
                    i.indisprimary, am.amname,
                    CASE WHEN $2::bool THEN pg_get_expr(i.indpred, i.indrelid) END,
                    array_agg(a.attname ORDER BY k.ord)
             FROM pg_index i
             JOIN pg_class ci ON ci.oid = i.indexrelid
             JOIN pg_class c ON c.oid = i.indrelid
             JOIN pg_namespace n ON n.oid = c.relnamespace
             JOIN pg_am am ON am.oid = ci.relam
             LEFT JOIN LATERAL unnest(i.indkey) WITH ORDINALITY AS k(attnum, ord) ON true
             LEFT JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum = k.attnum
             WHERE n.nspname = ANY($1::text[])
             GROUP BY n.nspname, c.relname, ci.relname, i.indisunique,
                      i.indisprimary, am.amname, i.indpred, i.indrelid
             ORDER BY n.nspname, c.relname, ci.relname",
            &[&schemas, &include_definitions],
        )
        .await
        .map_err(pg_err)?;
    for row in index_rows {
        let schema: String = row.get(0);
        let object: String = row.get(1);
        let Some(info) = positioned_object_mut(tree, &positions, &schema, &object) else {
            continue;
        };
        let access_method: String = row.get(5);
        let columns: Vec<Option<String>> = row.get(7);
        info.indexes.push(IndexInfo {
            name: row.get(2),
            unique: row.get(3),
            primary_key: row.get(4),
            kind: map_index_kind(&access_method),
            partial_predicate: row
                .get::<_, Option<String>>(6)
                .filter(|value| !value.is_empty()),
            columns: columns.into_iter().flatten().collect(),
        });
    }

    let constraint_rows = conn
        .query(
            "SELECT n.nspname, c.relname, con.conname, con.contype,
                    CASE WHEN $2::bool THEN pg_get_constraintdef(con.oid) END,
                    rn.nspname, rc.relname,
                    array_agg(a.attname ORDER BY u.ord)
                      FILTER (WHERE a.attname IS NOT NULL)
             FROM pg_constraint con
             JOIN pg_class c ON c.oid = con.conrelid
             JOIN pg_namespace n ON n.oid = c.relnamespace
             LEFT JOIN pg_class rc ON rc.oid = NULLIF(con.confrelid, 0)
             LEFT JOIN pg_namespace rn ON rn.oid = rc.relnamespace
             LEFT JOIN LATERAL unnest(con.conkey) WITH ORDINALITY AS u(attnum, ord) ON true
             LEFT JOIN pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = u.attnum
             WHERE n.nspname = ANY($1::text[])
             GROUP BY n.nspname, c.relname, con.conname, con.contype,
                      con.oid, rn.nspname, rc.relname
             ORDER BY n.nspname, c.relname, con.conname",
            &[&schemas, &include_definitions],
        )
        .await
        .map_err(pg_err)?;
    for row in constraint_rows {
        let schema: String = row.get(0);
        let object: String = row.get(1);
        let Some(info) = positioned_object_mut(tree, &positions, &schema, &object) else {
            continue;
        };
        let kind = match row.get::<_, i8>(3) as u8 {
            b'p' => ConstraintKind::PrimaryKey,
            b'f' => ConstraintKind::ForeignKey,
            b'u' => ConstraintKind::Unique,
            b'c' => ConstraintKind::Check,
            b'x' => ConstraintKind::Exclusion,
            _ => ConstraintKind::Other,
        };
        let target_schema: Option<String> = row.get(5);
        let target_object: Option<String> = row.get(6);
        let columns: Option<Vec<Option<String>>> = row.get(7);
        info.constraints.push(ConstraintInfo {
            name: row.get(2),
            kind,
            definition: row.get(4),
            references: target_schema
                .zip(target_object)
                .map(|(schema, object)| format!("{schema}.{object}")),
            columns: columns.unwrap_or_default().into_iter().flatten().collect(),
        });
    }

    let trigger_rows = conn
        .query(
            "SELECT n.nspname, c.relname, t.tgname, t.tgtype::integer,
                    CASE WHEN $2::bool THEN pg_get_triggerdef(t.oid) END
             FROM pg_trigger t
             JOIN pg_class c ON c.oid = t.tgrelid
             JOIN pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = ANY($1::text[]) AND NOT t.tgisinternal
             ORDER BY n.nspname, c.relname, t.tgname",
            &[&schemas, &include_definitions],
        )
        .await
        .map_err(pg_err)?;
    for row in trigger_rows {
        let schema: String = row.get(0);
        let object: String = row.get(1);
        let Some(info) = positioned_object_mut(tree, &positions, &schema, &object) else {
            continue;
        };
        let (timing, events) = decode_tgtype(row.get(3));
        info.triggers.push(sift_protocol::TriggerInfo {
            name: row.get(2),
            timing,
            events,
            columns: Vec::new(),
            definition: row.get(4),
        });
    }
    Ok(())
}

fn object_positions(tree: &CatalogTree) -> HashMap<(String, String), (usize, usize)> {
    tree.schemas
        .iter()
        .enumerate()
        .flat_map(|(schema_index, schema)| {
            schema
                .objects
                .iter()
                .enumerate()
                .filter(|(_, object)| is_introspectable(&object.kind))
                .map(move |(object_index, object)| {
                    (
                        (schema.name.clone(), object.name.clone()),
                        (schema_index, object_index),
                    )
                })
        })
        .collect()
}

fn positioned_object_mut<'a>(
    tree: &'a mut CatalogTree,
    positions: &HashMap<(String, String), (usize, usize)>,
    schema: &str,
    object: &str,
) -> Option<&'a mut ObjectInfo> {
    let (schema_index, object_index) = positions.get(&(schema.to_string(), object.to_string()))?;
    tree.schemas
        .get_mut(*schema_index)?
        .objects
        .get_mut(*object_index)
}

async fn enrich_graph_identity_and_foreign_keys(
    conn: &PooledConn,
    graph: &mut CatalogGraphData,
    tree: &CatalogTree,
) -> Result<(), DriverError> {
    let schema_names = graph
        .nodes
        .iter()
        .filter(|node| node.kind == CatalogNodeKind::Schema)
        .map(|node| (node.id.clone(), node.name.clone()))
        .collect::<HashMap<_, _>>();
    let object_nodes = graph
        .nodes
        .iter()
        .filter_map(|node| {
            let schema = schema_names.get(node.parent_id.as_ref()?)?;
            Some((
                (schema.clone(), graph_object_lookup_name(node)),
                node.id.clone(),
            ))
        })
        .collect::<HashMap<_, _>>();
    let relation_nodes = graph
        .nodes
        .iter()
        .filter(|node| {
            matches!(
                node.kind,
                CatalogNodeKind::Table
                    | CatalogNodeKind::View
                    | CatalogNodeKind::MaterializedView
                    | CatalogNodeKind::ForeignTable
                    | CatalogNodeKind::PartitionedTable
            )
        })
        .filter_map(|node| {
            let schema = schema_names.get(node.parent_id.as_ref()?)?;
            Some(((schema.clone(), node.name.clone()), node.id.clone()))
        })
        .collect::<HashMap<_, _>>();
    let object_identity = object_nodes
        .iter()
        .map(|((schema, object), id)| (id.clone(), (schema.clone(), object.clone())))
        .collect::<HashMap<_, _>>();
    let subordinate_nodes = graph
        .nodes
        .iter()
        .filter_map(|node| {
            let (schema, object) = object_identity.get(node.parent_id.as_ref()?)?;
            Some((
                (node.kind, schema.clone(), object.clone(), node.name.clone()),
                node.id.clone(),
            ))
        })
        .collect::<HashMap<_, _>>();
    let schemas = tree
        .schemas
        .iter()
        .map(|schema| schema.name.clone())
        .collect::<Vec<_>>();
    if schemas.is_empty() {
        return Ok(());
    }

    let identity_rows = conn
        .query(
            "SELECT 'object'::text, n.nspname, c.relname, NULL::text, c.oid::text
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = ANY($1::text[])
             UNION ALL
             SELECT 'column', n.nspname, c.relname, a.attname,
                    c.oid::text || ':att:' || a.attnum::text
             FROM pg_attribute a
             JOIN pg_class c ON c.oid = a.attrelid
             JOIN pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = ANY($1::text[]) AND a.attnum > 0 AND NOT a.attisdropped
             UNION ALL
             SELECT 'index', n.nspname, c.relname, ci.relname, ci.oid::text
             FROM pg_index i
             JOIN pg_class c ON c.oid = i.indrelid
             JOIN pg_class ci ON ci.oid = i.indexrelid
             JOIN pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = ANY($1::text[])
             UNION ALL
             SELECT 'constraint', n.nspname, c.relname, con.conname, con.oid::text
             FROM pg_constraint con
             JOIN pg_class c ON c.oid = con.conrelid
             JOIN pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = ANY($1::text[])
             UNION ALL
             SELECT 'trigger', n.nspname, c.relname, t.tgname, t.oid::text
             FROM pg_trigger t
             JOIN pg_class c ON c.oid = t.tgrelid
             JOIN pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = ANY($1::text[]) AND NOT t.tgisinternal
             UNION ALL
             SELECT 'routine', n.nspname,
                    p.proname || '(' || array_to_string(ARRAY(
                        SELECT format_type(arg_oid::oid, NULL)
                        FROM unnest(string_to_array(p.proargtypes::text, ' '))
                             WITH ORDINALITY AS args(arg_oid, ord)
                        WHERE arg_oid <> '' ORDER BY ord
                    ), ',') || ')',
                    NULL::text, p.oid::text
             FROM pg_proc p
             JOIN pg_namespace n ON n.oid = p.pronamespace
             WHERE n.nspname = ANY($1::text[]) AND p.prokind IN ('f', 'p')
             UNION ALL
             SELECT 'type', n.nspname, t.typname, NULL::text, t.oid::text
             FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace
             WHERE n.nspname = ANY($1::text[])
               AND t.typtype IN ('e', 'd', 'c', 'r')
               AND NOT EXISTS (SELECT 1 FROM pg_class c WHERE c.reltype = t.oid)
             UNION ALL
             SELECT 'extension', n.nspname, e.extname, NULL::text, e.oid::text
             FROM pg_extension e JOIN pg_namespace n ON n.oid = e.extnamespace
             WHERE n.nspname = ANY($1::text[])",
            &[&schemas],
        )
        .await
        .map_err(pg_err)?;
    let node_indexes = graph
        .nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id.clone(), index))
        .collect::<HashMap<_, _>>();
    for row in identity_rows {
        let tag: String = row.get(0);
        let schema: String = row.get(1);
        let object: String = row.get(2);
        let child: Option<String> = row.get(3);
        let native: String = row.get(4);
        let id = match tag.as_str() {
            "object" => object_nodes.get(&(schema, object)),
            "column" => child.as_ref().and_then(|name| {
                subordinate_nodes.get(&(CatalogNodeKind::Column, schema, object, name.clone()))
            }),
            "index" => child.as_ref().and_then(|name| {
                subordinate_nodes.get(&(CatalogNodeKind::Index, schema, object, name.clone()))
            }),
            "constraint" => child.as_ref().and_then(|name| {
                subordinate_nodes.get(&(CatalogNodeKind::Constraint, schema, object, name.clone()))
            }),
            "trigger" => child.as_ref().and_then(|name| {
                subordinate_nodes.get(&(CatalogNodeKind::Trigger, schema, object, name.clone()))
            }),
            "routine" => object_nodes.get(&(schema, object)),
            "type" | "extension" => object_nodes.get(&(schema, object)),
            _ => None,
        };
        if let Some(index) = id.and_then(|id| node_indexes.get(id)) {
            graph.nodes[*index].native_id = Some(format!("pg:{tag}:{native}"));
        }
    }

    let foreign_keys = conn
        .query(
            "SELECT sn.nspname, sc.relname, con.conname,
                    tn.nspname, tc.relname,
                    array_agg(sa.attname ORDER BY keys.ord),
                    array_agg(ta.attname ORDER BY keys.ord)
             FROM pg_constraint con
             JOIN pg_class sc ON sc.oid = con.conrelid
             JOIN pg_namespace sn ON sn.oid = sc.relnamespace
             JOIN pg_class tc ON tc.oid = con.confrelid
             JOIN pg_namespace tn ON tn.oid = tc.relnamespace
             JOIN LATERAL unnest(con.conkey, con.confkey)
                  WITH ORDINALITY AS keys(source_attnum, target_attnum, ord) ON true
             JOIN pg_attribute sa
               ON sa.attrelid = con.conrelid AND sa.attnum = keys.source_attnum
             JOIN pg_attribute ta
               ON ta.attrelid = con.confrelid AND ta.attnum = keys.target_attnum
             WHERE con.contype = 'f' AND sn.nspname = ANY($1::text[])
             GROUP BY sn.nspname, sc.relname, con.conname, tn.nspname, tc.relname
             ORDER BY sn.nspname, sc.relname, con.conname",
            &[&schemas],
        )
        .await
        .map_err(pg_err)?;
    for row in foreign_keys {
        let source_schema: String = row.get(0);
        let source_object: String = row.get(1);
        let constraint_name: String = row.get(2);
        let target_schema: String = row.get(3);
        let target_object: String = row.get(4);
        let source_columns: Vec<String> = row.get(5);
        let target_columns: Vec<String> = row.get(6);
        let Some(constraint_id) = subordinate_nodes.get(&(
            CatalogNodeKind::Constraint,
            source_schema.clone(),
            source_object.clone(),
            constraint_name,
        )) else {
            continue;
        };
        let Some(target_id) = relation_nodes.get(&(target_schema.clone(), target_object.clone()))
        else {
            continue;
        };
        let pairs = source_columns
            .into_iter()
            .zip(target_columns)
            .filter_map(|(source, target)| {
                Some(CatalogColumnPair {
                    from: subordinate_nodes
                        .get(&(
                            CatalogNodeKind::Column,
                            source_schema.clone(),
                            source_object.clone(),
                            source,
                        ))?
                        .clone(),
                    to: subordinate_nodes
                        .get(&(
                            CatalogNodeKind::Column,
                            target_schema.clone(),
                            target_object.clone(),
                            target,
                        ))?
                        .clone(),
                })
            })
            .collect::<Vec<_>>();
        if let Some(edge) = graph
            .edges
            .iter_mut()
            .find(|edge| edge.kind == CatalogEdgeKind::ForeignKey && edge.from == *constraint_id)
        {
            edge.to = Some(target_id.clone());
            edge.column_pairs = pairs;
            edge.certainty = sift_protocol::CatalogEdgeCertainty::CatalogProven;
            edge.referenced_path = None;
        }
    }

    let native_nodes = graph
        .nodes
        .iter()
        .filter_map(|node| {
            node.native_id
                .as_ref()
                .map(|native| (native.clone(), node.id.clone()))
        })
        .collect::<HashMap<_, _>>();
    let dependency_rows = conn
        .query(
            "SELECT 'reads_from', 'pg:object:' || source.oid::text,
                    'pg:object:' || dependency.refobjid::text
             FROM pg_rewrite rewrite
             JOIN pg_class source ON source.oid = rewrite.ev_class
             JOIN pg_namespace source_namespace ON source_namespace.oid = source.relnamespace
             JOIN pg_depend dependency
               ON dependency.classid = 'pg_rewrite'::regclass
              AND dependency.objid = rewrite.oid
              AND dependency.refclassid = 'pg_class'::regclass
             WHERE source_namespace.nspname = ANY($1::text[])
               AND dependency.refobjid <> source.oid
             UNION
             SELECT 'depends_on', 'pg:routine:' || routine.oid::text,
                    'pg:object:' || dependency.refobjid::text
             FROM pg_proc routine
             JOIN pg_namespace source_namespace ON source_namespace.oid = routine.pronamespace
             JOIN pg_depend dependency
               ON dependency.classid = 'pg_proc'::regclass
              AND dependency.objid = routine.oid
              AND dependency.refclassid = 'pg_class'::regclass
             WHERE source_namespace.nspname = ANY($1::text[])
             UNION
             SELECT 'calls', 'pg:routine:' || routine.oid::text,
                    'pg:routine:' || dependency.refobjid::text
             FROM pg_proc routine
             JOIN pg_namespace source_namespace ON source_namespace.oid = routine.pronamespace
             JOIN pg_depend dependency
               ON dependency.classid = 'pg_proc'::regclass
              AND dependency.objid = routine.oid
              AND dependency.refclassid = 'pg_proc'::regclass
             WHERE source_namespace.nspname = ANY($1::text[])
             UNION
             SELECT 'uses_type',
                    'pg:column:' || relation.oid::text || ':att:' || attribute.attnum::text,
                    'pg:type:' || attribute.atttypid::text
             FROM pg_attribute attribute
             JOIN pg_class relation ON relation.oid = attribute.attrelid
             JOIN pg_namespace source_namespace ON source_namespace.oid = relation.relnamespace
             WHERE source_namespace.nspname = ANY($1::text[])
               AND attribute.attnum > 0 AND NOT attribute.attisdropped
             UNION
             SELECT 'owns_sequence',
                    'pg:column:' || owned_relation.oid::text || ':att:' || dependency.refobjsubid::text,
                    'pg:object:' || sequence.oid::text
             FROM pg_class sequence
             JOIN pg_depend dependency
               ON dependency.classid = 'pg_class'::regclass
              AND dependency.objid = sequence.oid
              AND dependency.refclassid = 'pg_class'::regclass
              AND dependency.deptype IN ('a', 'i')
             JOIN pg_class owned_relation ON owned_relation.oid = dependency.refobjid
             JOIN pg_namespace source_namespace ON source_namespace.oid = owned_relation.relnamespace
             WHERE sequence.relkind = 'S' AND source_namespace.nspname = ANY($1::text[])",
            &[&schemas],
        )
        .await
        .map_err(pg_err)?;
    for row in dependency_rows {
        let kind = match row.get::<_, String>(0).as_str() {
            "reads_from" => CatalogEdgeKind::ReadsFrom,
            "depends_on" => CatalogEdgeKind::DependsOn,
            "calls" => CatalogEdgeKind::Calls,
            "uses_type" => CatalogEdgeKind::UsesType,
            "owns_sequence" => CatalogEdgeKind::OwnsSequence,
            _ => continue,
        };
        let from_native: String = row.get(1);
        let to_native: String = row.get(2);
        let Some((from, to)) = native_nodes
            .get(&from_native)
            .zip(native_nodes.get(&to_native))
        else {
            continue;
        };
        let edge = sift_protocol::CatalogEdge {
            from: from.clone(),
            to: Some(to.clone()),
            kind,
            certainty: sift_protocol::CatalogEdgeCertainty::CatalogProven,
            referenced_path: None,
            column_pairs: Vec::new(),
        };
        if !graph.edges.contains(&edge) {
            graph.edges.push(edge);
        }
    }
    Ok(())
}

fn graph_object_lookup_name(node: &sift_protocol::CatalogNode) -> String {
    match &node.details {
        sift_protocol::CatalogNodeDetails::Object {
            routine_args: Some(arguments),
        } => format!("{}({})", node.name, arguments.join(",")),
        _ => node.name.clone(),
    }
}

fn object_kind(kind: CatalogNodeKind) -> Option<ObjectKind> {
    Some(match kind {
        CatalogNodeKind::Table => ObjectKind::Table,
        CatalogNodeKind::View => ObjectKind::View,
        CatalogNodeKind::MaterializedView => ObjectKind::MaterializedView,
        CatalogNodeKind::ForeignTable => ObjectKind::ForeignTable,
        CatalogNodeKind::PartitionedTable => ObjectKind::PartitionedTable,
        CatalogNodeKind::TableValuedFunction => ObjectKind::TableValuedFunction,
        CatalogNodeKind::ScalarFunction => ObjectKind::ScalarFunction,
        CatalogNodeKind::Procedure => ObjectKind::Procedure,
        CatalogNodeKind::Synonym => ObjectKind::Synonym,
        CatalogNodeKind::Sequence => ObjectKind::Sequence,
        CatalogNodeKind::Trigger => ObjectKind::Trigger,
        CatalogNodeKind::Type => ObjectKind::Type,
        CatalogNodeKind::Extension => ObjectKind::Extension,
        CatalogNodeKind::Catalog
        | CatalogNodeKind::Schema
        | CatalogNodeKind::Column
        | CatalogNodeKind::Index
        | CatalogNodeKind::Constraint => return None,
    })
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
                    c.relkind AS relkind,
                    GREATEST(c.reltuples, 0)::bigint AS estimated_rows,
                    obj_description(c.oid, 'pg_class') AS comment
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
                    c.relkind AS relkind,
                    GREATEST(c.reltuples, 0)::bigint AS estimated_rows,
                    obj_description(c.oid, 'pg_class') AS comment
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

    let special_rows = if let Some(schemas) = schemas_filter.as_ref() {
        conn.query(
            "SELECT n.nspname, t.typname, 'type'::text
             FROM pg_type t
             JOIN pg_namespace n ON n.oid = t.typnamespace
             WHERE n.nspname NOT IN ('pg_catalog', 'information_schema')
               AND n.nspname NOT LIKE 'pg_toast%'
               AND t.typtype IN ('e', 'd', 'c', 'r')
               AND NOT EXISTS (SELECT 1 FROM pg_class c WHERE c.reltype = t.oid)
               AND t.typname LIKE $1 AND n.nspname = ANY($2::text[])
             UNION ALL
             SELECT n.nspname, e.extname, 'extension'
             FROM pg_extension e JOIN pg_namespace n ON n.oid = e.extnamespace
             WHERE e.extname LIKE $1 AND n.nspname = ANY($2::text[])
             ORDER BY 1, 2",
            &[&like, schemas],
        )
        .await
        .map_err(pg_err)?
    } else {
        conn.query(
            "SELECT n.nspname, t.typname, 'type'::text
             FROM pg_type t
             JOIN pg_namespace n ON n.oid = t.typnamespace
             WHERE n.nspname NOT IN ('pg_catalog', 'information_schema')
               AND n.nspname NOT LIKE 'pg_toast%'
               AND t.typtype IN ('e', 'd', 'c', 'r')
               AND NOT EXISTS (SELECT 1 FROM pg_class c WHERE c.reltype = t.oid)
               AND t.typname LIKE $1
             UNION ALL
             SELECT n.nspname, e.extname, 'extension'
             FROM pg_extension e JOIN pg_namespace n ON n.oid = e.extnamespace
             WHERE e.extname LIKE $1
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
        let mut info = ObjectInfo::new(object_name, kind);
        if matches!(
            kind,
            ObjectKind::Table | ObjectKind::ForeignTable | ObjectKind::PartitionedTable
        ) {
            info.estimated_rows = u64::try_from(row.get::<_, i64>(3)).ok();
        }
        info.comment = row.get(4);
        by_schema.entry(schema_name).or_default().push(info);
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
    for row in special_rows {
        let schema_name: String = row.get(0);
        let object_name: String = row.get(1);
        let kind = match row.get::<_, String>(2).as_str() {
            "type" => ObjectKind::Type,
            "extension" => ObjectKind::Extension,
            _ => continue,
        };
        if kinds_filter
            .as_ref()
            .is_some_and(|kinds| !kinds.contains(&kind))
        {
            continue;
        }
        by_schema
            .entry(schema_name)
            .or_default()
            .push(ObjectInfo::new(object_name, kind));
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
        estimated_rows: None,
        modified_at: None,
        comment: None,
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
                    a.attndims AS array_dims,
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
        // pg_attribute.attndims is an int2 catalog column.
        let array_dims: i16 = row.get(4);
        let default_expr: Option<String> = row.get(5);

        // Build TypeRef from the type OID via tokio_postgres::Type::from_oid.
        let type_ref = tokio_postgres::types::Type::from_oid(type_oid)
            .map(|t| crate::decode::pg_type_to_type_ref(&t))
            .unwrap_or_else(|| TypeRef::Native {
                provider_id: sift_protocol::Engine::Postgres.provider_id(),
                name: format!("oid={type_oid}"),
                category: TypeCategory::Other,
            });

        let auto_increment =
            is_pg_identity(identity) || default_expr.as_deref().is_some_and(is_serial_default);

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
            facets: sift_protocol::EngineColumnFacets {
                postgres: Some(sift_protocol::PgColumnFacets {
                    oid: Some(type_oid),
                    array_dims: u8::try_from(array_dims).unwrap_or(u8::MAX),
                    is_identity: is_pg_identity(identity),
                    default_expr,
                    enum_values: None,
                }),
                sql_server: None,
            },
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

fn is_pg_identity(identity: i8) -> bool {
    matches!(identity as u8, b'a' | b'd')
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

#[cfg(test)]
mod tests {
    use super::is_pg_identity;

    #[test]
    fn postgres_identity_marker_rejects_the_empty_internal_char() {
        assert!(!is_pg_identity(0));
        assert!(is_pg_identity(b'a' as i8));
        assert!(is_pg_identity(b'd' as i8));
    }
}
