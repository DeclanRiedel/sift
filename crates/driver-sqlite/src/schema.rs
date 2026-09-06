use super::{db_error, error, values};
use rusqlite::{Connection, OptionalExtension};
use sift_protocol::*;
const MAX_OBJECTS: usize = 10_000;
fn schema_name(object: &ObjectPath) -> Result<&str, DriverError> {
    match object.schema.as_deref().unwrap_or("main") {
        name @ ("main" | "temp") => Ok(name),
        _ => Err(error(
            Code::UndefinedObject,
            "SQLite schema must be main or temp",
        )),
    }
}
pub fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
fn revisions(conn: &Connection) -> Result<(i64, i64), DriverError> {
    Ok((
        conn.query_row("PRAGMA main.schema_version", [], |r| r.get(0))
            .map_err(db_error)?,
        conn.query_row("PRAGMA temp.schema_version", [], |r| r.get(0))
            .map_err(db_error)?,
    ))
}
pub fn load(
    conn: &Connection,
    scope: SchemaScope,
    name: &str,
    identity: &str,
) -> Result<SchemaSnapshot, DriverError> {
    let mut scope = scope;
    if let SchemaDepth::Graph { options } = &scope.depth {
        scope.filter = Some(SchemaFilter {
            schemas: options.schemas.clone(),
            ..Default::default()
        });
    }
    for _ in 0..3 {
        let before = revisions(conn)?;
        let result = load_once(conn, scope.clone(), name, identity)?;
        if before == revisions(conn)? {
            return Ok(result);
        }
    }
    Err(error(
        Code::Other {
            message: "schema changed during introspection".into(),
        },
        "SQLite schema changed during introspection; refresh again",
    ))
}
fn load_once(
    conn: &Connection,
    scope: SchemaScope,
    name: &str,
    identity: &str,
) -> Result<SchemaSnapshot, DriverError> {
    let navigation = matches!(scope.depth, SchemaDepth::Graph { .. });
    let mut node_budget = 10_000usize;
    let mut result = SchemaSnapshot::empty(scope.clone());
    let mut catalog = CatalogTree {
        name: name.into(),
        schemas: vec![],
    };
    for schema in ["main", "temp"] {
        if scope
            .filter
            .as_ref()
            .and_then(|f| f.schemas.as_ref())
            .is_some_and(|names| !names.iter().any(|n| n == schema))
        {
            continue;
        }
        if scope
            .filter
            .as_ref()
            .and_then(|f| f.catalogs.as_ref())
            .is_some_and(|names| !names.iter().any(|n| n == name))
        {
            continue;
        }
        let target = match &scope.depth {
            SchemaDepth::Deep { object } => {
                if schema_name(object)? != schema {
                    continue;
                }
                Some(object.name.as_str())
            }
            _ => None,
        };
        let pattern = scope
            .filter
            .as_ref()
            .and_then(|f| f.name_pattern.as_deref())
            .unwrap_or("*");
        let mut statement=conn.prepare(&format!("SELECT name,type,sql FROM {schema}.sqlite_schema WHERE type IN ('table','view','trigger') AND name NOT LIKE 'sqlite_%' AND (?1 IS NULL OR name=?1) AND name GLOB ?2 ORDER BY type,name LIMIT 10001")).map_err(db_error)?;
        let rows = statement
            .query_map(rusqlite::params![target, pattern], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })
            .map_err(db_error)?;
        let mut tree = SchemaTree {
            name: schema.into(),
            objects: vec![],
        };
        for row in rows {
            if tree.objects.len() >= MAX_OBJECTS {
                result.incomplete = true;
                break;
            }
            let (name, kind, sql) = row.map_err(db_error)?;
            let kind = match kind.as_str() {
                "view" => ObjectKind::View,
                "trigger" => ObjectKind::Trigger,
                _ => ObjectKind::Table,
            };
            if scope
                .filter
                .as_ref()
                .and_then(|f| f.kinds.as_ref())
                .is_some_and(|kinds| !kinds.contains(&kind))
            {
                continue;
            }
            let mut object = ObjectInfo::new(name, kind);
            if (target.is_some() || navigation)
                && matches!(kind, ObjectKind::Table | ObjectKind::View)
            {
                deepen(conn, schema, &mut object, sql.as_deref().unwrap_or(""))?;
            }
            if navigation {
                let cost =
                    1 + object.columns.len() + object.indexes.len() + object.constraints.len();
                if cost > node_budget {
                    result.incomplete = true;
                    break;
                }
                node_budget -= cost;
            }
            tree.objects.push(object);
        }
        catalog.schemas.push(tree);
    }
    result.trees.push(catalog);
    if let SchemaDepth::Graph { options } = &scope.depth {
        let mut coverage = CatalogCoverage::complete();
        coverage.state = CatalogCoverageState::Partial;
        coverage.covered_schemas = result
            .trees
            .iter()
            .flat_map(|t| t.schemas.iter().map(|s| s.name.clone()))
            .collect();
        coverage.failures.push(CatalogCoverageFailure {
            stage: "dependencies".into(),
            schema: None,
            code: "sqlite_navigation_only".into(),
        });
        let mut graph = sift_core::catalog::graph_from_trees(&result.trees, coverage, identity);
        if let Some(kinds) = &options.kinds {
            graph.nodes.retain(|node| kinds.contains(&node.kind));
        }
        let limit = options.max_nodes.unwrap_or(10_000).min(10_000) as usize;
        if graph.nodes.len() > limit || result.incomplete {
            graph.nodes.truncate(limit);
            graph.coverage.truncated_at_nodes = Some(limit as u32);
        }
        {
            let ids = graph
                .nodes
                .iter()
                .map(|n| n.id.clone())
                .collect::<std::collections::HashSet<_>>();
            graph.edges.retain(|edge| {
                ids.contains(&edge.from)
                    && edge.to.as_ref().map_or(true, |target| ids.contains(target))
            });
        }
        result.graph = Some(graph);
    }
    Ok(result)
}
fn deepen(
    conn: &Connection,
    schema: &str,
    object: &mut ObjectInfo,
    sql: &str,
) -> Result<(), DriverError> {
    // SQLite keeps CHECK expressions in its native CREATE statement, not a
    // PRAGMA. Add parsed expressions when supported; native DDL remains the
    // authority for syntax outside the parser's coverage.
    if let Ok(statements) =
        sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::SQLiteDialect {}, sql)
    {
        for statement in statements {
            if let sqlparser::ast::Statement::CreateTable(table) = statement {
                for constraint in table.constraints {
                    if let sqlparser::ast::TableConstraint::Check { name, expr } = constraint {
                        object.constraints.push(ConstraintInfo {
                            name: name.map_or_else(
                                || format!("CHECK {}", object.constraints.len() + 1),
                                |n| n.value,
                            ),
                            kind: ConstraintKind::Check,
                            columns: vec![],
                            definition: Some(expr.to_string()),
                            references: None,
                        });
                    }
                }
                for column in table.columns {
                    for option in column.options {
                        if let sqlparser::ast::ColumnOption::Check(expr) = option.option {
                            object.constraints.push(ConstraintInfo {
                                name: option.name.map_or_else(
                                    || format!("CHECK {}", column.name.value),
                                    |n| n.value,
                                ),
                                kind: ConstraintKind::Check,
                                columns: vec![column.name.value.clone()],
                                definition: Some(expr.to_string()),
                                references: None,
                            });
                        }
                    }
                }
            }
        }
    }
    let mut stmt = conn
        .prepare("SELECT name,\"unique\",origin,partial FROM pragma_index_list(?1,?2) ORDER BY seq LIMIT 10001")
        .map_err(db_error)?;
    let indexes = stmt
        .query_map([&object.name, schema], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, bool>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, bool>(3)?,
            ))
        })
        .map_err(db_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_error)?;
    let has_pk_index = indexes.iter().any(|(_, _, origin, _)| origin == "pk");
    if indexes.len() > MAX_OBJECTS {
        return Err(error(
            Code::ResultTooLarge,
            "SQLite index metadata exceeds the object limit",
        ));
    }
    let mut stmt=conn.prepare("SELECT name,type,\"notnull\",dflt_value,pk,hidden FROM pragma_table_xinfo(?1,?2) ORDER BY cid").map_err(db_error)?;
    let rows = stmt
        .query_map([&object.name, schema], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, bool>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, u32>(4)?,
                r.get::<_, u8>(5)?,
            ))
        })
        .map_err(db_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_error)?;
    let strict_or_without: bool = conn
        .query_row(
            "SELECT (wr OR strict) FROM pragma_table_list WHERE schema=?1 AND name=?2",
            [schema, &object.name],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?
        .unwrap_or(false);
    let virtual_table = sql
        .trim_start()
        .to_ascii_uppercase()
        .starts_with("CREATE VIRTUAL TABLE");
    let pk_count = rows.iter().filter(|r| r.4 > 0).count();
    let mut pk = Vec::new();
    for (name, declared, notnull, default, ordinal, hidden) in rows {
        let rowid_alias = ordinal == 1
            && pk_count == 1
            && !has_pk_index
            && declared.eq_ignore_ascii_case("INTEGER")
            && !virtual_table
            && object.kind == ObjectKind::Table;
        let mut col = ColumnMetadata::new(
            &name,
            values::type_ref(if declared.is_empty() {
                "dynamic"
            } else {
                &declared
            }),
        );
        col.primary_key = ordinal > 0;
        col.nullable = if notnull || rowid_alias || (ordinal > 0 && strict_or_without) {
            Nullability::NotNullable
        } else {
            Nullability::Nullable
        };
        col.auto_increment = rowid_alias;
        col.facets.sqlite = Some(Box::new(SqliteColumnFacets {
            affinity: values::affinity(&declared).into(),
            declared_type: declared,
            default_expr: default,
            primary_key_ordinal: ordinal,
            virtual_table,
            hidden,
        }));
        if ordinal > 0 {
            pk.push((ordinal, name));
        }
        object.columns.push(col);
    }
    pk.sort_by_key(|(ordinal, _)| *ordinal);
    if !pk.is_empty() {
        object.constraints.push(ConstraintInfo {
            name: "PRIMARY KEY".into(),
            kind: ConstraintKind::PrimaryKey,
            columns: pk.into_iter().map(|(_, name)| name).collect(),
            definition: None,
            references: None,
        });
    }
    for (name, unique, origin, partial) in indexes {
        let mut stmt = conn
            .prepare("SELECT name FROM pragma_index_xinfo(?1,?2) WHERE key=1 ORDER BY seqno")
            .map_err(db_error)?;
        let names = stmt
            .query_map([&name, schema], |r| r.get::<_, Option<String>>(0))
            .map_err(db_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(db_error)?;
        let columns = names
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .unwrap_or_default();
        let predicate = if partial {
            let sql: String = conn
                .query_row(
                    &format!(
                        "SELECT sql FROM {schema}.sqlite_schema WHERE type='index' AND name=?1"
                    ),
                    [&name],
                    |r| r.get(0),
                )
                .map_err(db_error)?;
            let parsed =
                sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::SQLiteDialect {}, &sql)
                    .ok();
            Some(
                parsed
                    .and_then(|statements| {
                        statements.into_iter().find_map(|s| match s {
                            sqlparser::ast::Statement::CreateIndex(i) => {
                                i.predicate.map(|p| p.to_string())
                            }
                            _ => None,
                        })
                    })
                    .unwrap_or_else(|| format!("Native partial-index definition: {sql}")),
            )
        } else {
            None
        };
        if origin == "u" {
            object.constraints.push(ConstraintInfo {
                name: name.clone(),
                kind: ConstraintKind::Unique,
                columns: columns.clone(),
                definition: None,
                references: None,
            });
        }
        object.indexes.push(IndexInfo {
            name,
            columns,
            unique,
            primary_key: origin == "pk",
            kind: IndexKind::Btree,
            partial_predicate: predicate,
        });
    }
    let mut stmt=conn.prepare("SELECT id,seq,\"table\",\"from\",\"to\",on_update,on_delete FROM pragma_foreign_key_list(?1,?2) ORDER BY id,seq").map_err(db_error)?;
    let mut foreign = std::collections::BTreeMap::<i64, ConstraintInfo>::new();
    let rows = stmt
        .query_map([&object.name, schema], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
            ))
        })
        .map_err(db_error)?;
    for row in rows {
        let (id, table, column, target, update, delete) = row.map_err(db_error)?;
        let item = foreign.entry(id).or_insert_with(|| ConstraintInfo {
            name: format!("foreign_key_{id}"),
            kind: ConstraintKind::ForeignKey,
            columns: vec![],
            definition: Some(format!("ON UPDATE {update} ON DELETE {delete}")),
            references: Some(table),
        });
        item.columns.push(column);
        if let Some(target) = target {
            item.definition
                .as_mut()
                .unwrap()
                .push_str(&format!("; references {}", quote(&target)));
        }
    }
    object.constraints.extend(foreign.into_values());
    Ok(())
}
pub fn ddl(conn: &Connection, object: &ObjectPath) -> Result<String, DriverError> {
    let schema = schema_name(object)?;
    let record: Option<(String, String)> = conn
        .query_row(
            &format!(
                "SELECT type,sql FROM {schema}.sqlite_schema WHERE name=?1 AND sql IS NOT NULL"
            ),
            [&object.name],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(db_error)?;
    let (kind, sql) = record.ok_or_else(|| {
        error(
            Code::UndefinedObject,
            "SQLite object has no stored definition",
        )
    })?;
    if sql
        .trim_start()
        .to_ascii_uppercase()
        .starts_with("CREATE VIRTUAL TABLE")
    {
        return Err(error(
            Code::UnsupportedForEngine,
            "SQLite virtual-table DDL is unsupported",
        ));
    }
    if let Some(expected) = object.kind {
        let actual = match kind.as_str() {
            "table" => Some(ObjectKind::Table),
            "view" => Some(ObjectKind::View),
            "trigger" => Some(ObjectKind::Trigger),
            _ => None,
        };
        if actual != Some(expected) {
            return Err(error(
                Code::UndefinedObject,
                "SQLite object kind does not match",
            ));
        }
    }
    let mut out = format!("{};", sql.trim_end_matches(';'));
    if kind == "table" {
        let mut stmt=conn.prepare(&format!("SELECT sql FROM {schema}.sqlite_schema WHERE tbl_name=?1 AND type IN ('index','trigger') AND sql IS NOT NULL ORDER BY type,name")).map_err(db_error)?;
        for row in stmt
            .query_map([&object.name], |r| r.get::<_, String>(0))
            .map_err(db_error)?
        {
            let sql = row.map_err(db_error)?;
            if out.len().saturating_add(sql.len()).saturating_add(2) > 8 * 1024 * 1024 {
                return Err(error(
                    Code::ResultTooLarge,
                    "SQLite DDL exceeds the 8 MiB limit",
                ));
            }
            out.push('\n');
            out.push_str(sql.trim_end_matches(';'));
            out.push(';');
        }
    }
    Ok(out)
}
