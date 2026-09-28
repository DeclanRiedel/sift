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
    let mut count_budget = 100_000u64;
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
            if kind == ObjectKind::Table
                && !sql
                    .as_deref()
                    .unwrap_or("")
                    .trim_start()
                    .to_ascii_uppercase()
                    .starts_with("CREATE VIRTUAL TABLE")
            {
                object.estimated_rows = table_rows(conn, schema, &object.name, &mut count_budget)?;
            }

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
        // Counts describe current data, not schema identity. Keep them in the
        // navigation trees without invalidating catalog revisions after DML.
        let mut schema_trees = result.trees.clone();
        for object in schema_trees
            .iter_mut()
            .flat_map(|catalog| &mut catalog.schemas)
            .flat_map(|schema| &mut schema.objects)
        {
            object.estimated_rows = None;
        }
        let mut graph = sift_core::catalog::graph_from_trees(&schema_trees, coverage, identity);
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
// Prefer persistent statistics. Count small, unanalyzed tables within a shared
// catalog budget so inspection snapshots have useful counts without unbounded scans.
fn table_rows(
    conn: &Connection,
    schema: &str,
    name: &str,
    budget: &mut u64,
) -> Result<Option<u64>, DriverError> {
    let has_stats: bool = conn
        .query_row(
            &format!(
                "SELECT EXISTS(SELECT 1 FROM {schema}.sqlite_schema WHERE name='sqlite_stat1')"
            ),
            [],
            |row| row.get(0),
        )
        .map_err(db_error)?;
    if has_stats {
        let estimate: Option<u64> = conn
            .query_row(
                &format!(
                    "SELECT max(CAST(stat AS INTEGER)) FROM {schema}.sqlite_stat1 WHERE tbl=?1"
                ),
                [name],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        if estimate.is_some() {
            return Ok(estimate);
        }
    }
    if *budget == 0 {
        return Ok(None);
    }
    let limit = (*budget).min(10_000);
    let count: u64 = conn
        .query_row(
            &format!(
                "SELECT count(*) FROM (SELECT 1 FROM {schema}.{} LIMIT {})",
                quote(name),
                limit + 1
            ),
            [],
            |row| row.get(0),
        )
        .map_err(db_error)?;
    *budget = budget.saturating_sub(count);
    Ok((count <= limit).then_some(count))
}

struct DdlToken {
    value: String,
    start: usize,
    end: usize,
    depth: usize,
    quoted: bool,
}

fn ddl_tokens(sql: &str) -> Vec<DdlToken> {
    let bytes = sql.as_bytes();
    let mut tokens = Vec::new();
    let (mut i, mut depth) = (0, 0usize);
    while i < bytes.len() {
        let start = i;
        match bytes[i] {
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                i += 2;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < bytes.len() && &bytes[i..i + 2] != b"*/" {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
            }
            b'\'' | b'"' | b'`' | b'[' => {
                let open = bytes[i];
                let close = if open == b'[' { b']' } else { open };
                i += 1;
                let content = i;
                while i < bytes.len() {
                    if bytes[i] == close {
                        if bytes.get(i + 1) == Some(&close) {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                let value = sql[content..i].replace(
                    &format!("{}{}", close as char, close as char),
                    &(close as char).to_string(),
                );
                i = (i + 1).min(bytes.len());
                if open != b'\'' {
                    tokens.push(DdlToken {
                        value,
                        start,
                        end: i,
                        depth,
                        quoted: true,
                    });
                }
            }
            b'(' => {
                tokens.push(DdlToken {
                    value: "(".into(),
                    start,
                    end: i + 1,
                    depth,
                    quoted: false,
                });
                depth += 1;
                i += 1;
            }
            b')' => {
                depth = depth.saturating_sub(1);
                tokens.push(DdlToken {
                    value: ")".into(),
                    start,
                    end: i + 1,
                    depth,
                    quoted: false,
                });
                i += 1;
            }
            b',' => {
                tokens.push(DdlToken {
                    value: ",".into(),
                    start,
                    end: i + 1,
                    depth,
                    quoted: false,
                });
                i += 1;
            }
            b if b.is_ascii_alphabetic() || b == b'_' || b >= 0x80 => {
                i += 1;
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] >= 0x80)
                {
                    i += 1;
                }
                tokens.push(DdlToken {
                    value: sql[start..i].into(),
                    start,
                    end: i,
                    depth,
                    quoted: false,
                });
            }
            _ => i += 1,
        }
    }
    tokens
}

fn native_checks(sql: &str) -> Vec<ConstraintInfo> {
    const MAX_CHECK_SQL: usize = 1024 * 1024;
    if sql.len() > MAX_CHECK_SQL {
        return Vec::new();
    }
    let tokens = ddl_tokens(sql);
    let Some(body) = tokens.iter().position(|t| t.value == "(" && t.depth == 0) else {
        return Vec::new();
    };
    let prefix = &tokens[..body];
    if !prefix
        .first()
        .is_some_and(|t| t.value.eq_ignore_ascii_case("CREATE"))
        || !prefix
            .iter()
            .any(|t| !t.quoted && t.value.eq_ignore_ascii_case("TABLE"))
        || prefix
            .iter()
            .any(|t| !t.quoted && t.value.eq_ignore_ascii_case("VIRTUAL"))
    {
        return Vec::new();
    }
    let mut checks = Vec::new();
    let mut segment = body + 1;
    for end in body + 1..tokens.len() {
        if !(tokens[end].value == "," && tokens[end].depth == 1
            || tokens[end].value == ")" && tokens[end].depth == 0)
        {
            continue;
        }
        let top = tokens[segment..end]
            .iter()
            .filter(|t| t.depth == 1)
            .collect::<Vec<_>>();
        let table_constraint = top.first().is_some_and(|t| {
            !t.quoted
                && ["CONSTRAINT", "CHECK", "PRIMARY", "UNIQUE", "FOREIGN"]
                    .iter()
                    .any(|keyword| t.value.eq_ignore_ascii_case(keyword))
        });
        let column = (!table_constraint)
            .then(|| top.first().map(|t| t.value.clone()))
            .flatten();
        for (index, token) in top.iter().enumerate() {
            if checks.len() >= MAX_OBJECTS {
                return checks;
            }
            if token.quoted || !token.value.eq_ignore_ascii_case("CHECK") {
                continue;
            }
            let Some(open) = top.get(index + 1).filter(|t| t.value == "(") else {
                continue;
            };
            let next = tokens.partition_point(|t| t.start <= open.start);
            let Some(close) = tokens[next..]
                .iter()
                .find(|t| t.start > open.start && t.depth == 1 && t.value == ")")
            else {
                continue;
            };
            let clause_start = top[..index]
                .iter()
                .rposition(|prior| !prior.quoted && prior.value.eq_ignore_ascii_case("CHECK"))
                .map_or(0, |prior| prior + 1);
            let name = top[clause_start..index]
                .windows(2)
                .rev()
                .find(|pair| !pair[0].quoted && pair[0].value.eq_ignore_ascii_case("CONSTRAINT"))
                .map(|pair| pair[1].value.clone())
                .unwrap_or_else(|| {
                    column.as_ref().map_or_else(
                        || format!("CHECK {}", checks.len() + 1),
                        |name| format!("CHECK {name}"),
                    )
                });
            checks.push(ConstraintInfo {
                name,
                kind: ConstraintKind::Check,
                columns: column.iter().cloned().collect(),
                definition: Some(sql[open.end..close.start].trim().into()),
                references: None,
            });
        }
        if tokens[end].value == ")" {
            break;
        }
        segment = end + 1;
    }
    checks
}

fn deepen(
    conn: &Connection,
    schema: &str,
    object: &mut ObjectInfo,
    sql: &str,
) -> Result<(), DriverError> {
    // SQLite exposes CHECK only inside sqlite_schema.sql. The editor parser
    // rejects valid native table forms, so scan just the table body and retain
    // the exact expression text. Stored native DDL remains authoritative.
    object.constraints.extend(native_checks(sql));
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

#[cfg(test)]
mod check_tests {
    use super::native_checks;

    #[test]
    fn unsupported_shapes_do_not_invent_check_metadata() {
        assert!(
            native_checks("CREATE VIRTUAL TABLE docs USING fts5(body, CHECK(value))").is_empty()
        );
        let oversized = format!(
            "CREATE TABLE t(a CHECK(a > 0)) /*{}*/",
            "x".repeat(1024 * 1024)
        );
        assert!(native_checks(&oversized).is_empty());
        assert!(native_checks("CREATE TABLE copy AS SELECT 1 CHECK (fake)").is_empty());
        let unicode = native_checks("CREATE TABLE t(名 INTEGER CHECK(名 > 0))");
        assert_eq!(unicode[0].columns, ["名"]);
    }
}
