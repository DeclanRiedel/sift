use super::{db_error, error, values};
use rusqlite::{Connection, OptionalExtension};
use sift_protocol::*;
use sqlparser::ast::{ObjectName, Query, Visit, Visitor};
use sqlparser::dialect::SQLiteDialect;
use sqlparser::parser::Parser;
use sqlparser::tokenizer::{Token, Tokenizer};
use std::collections::{HashMap, HashSet};
use std::ops::ControlFlow;
const MAX_OBJECTS: usize = 10_000;
const MAX_DEPENDENCY_EDGES: usize = 10_000;
const MAX_VIEW_REFERENCES: usize = 64;
const MAX_VIEW_SQL: usize = 1024 * 1024;
const MAX_DEPENDENCY_SQL_BYTES: usize = 16 * 1024 * 1024;

struct DependencySource {
    schema: String,
    name: String,
    kind: ObjectKind,
    view_sql: Option<String>,
    view_sql_limited: bool,
    trigger_sql: Option<String>,
    trigger_sql_limited: bool,
    virtual_sql: Option<String>,
    virtual_sql_limited: bool,
    virtual_table: bool,
    table_name: String,
}
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
    let mut dependency_sources = Vec::new();
    let mut dependency_sql_budget = MAX_DEPENDENCY_SQL_BYTES;
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
        let mut statement=conn.prepare(&format!("SELECT name,type,sql,tbl_name FROM {schema}.sqlite_schema WHERE type IN ('table','view','trigger') AND name NOT LIKE 'sqlite_%' AND (?1 IS NULL OR name=?1) AND name GLOB ?2 ORDER BY type,name LIMIT 10001")).map_err(db_error)?;
        let rows = statement
            .query_map(rusqlite::params![target, pattern], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, String>(3)?,
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
            let (name, kind, sql, table_name) = row.map_err(db_error)?;
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
            let mut object = ObjectInfo::new(name.clone(), kind);
            let virtual_table = kind == ObjectKind::Table
                && sql.as_deref().is_some_and(|sql| {
                    sql.trim_start()
                        .to_ascii_uppercase()
                        .starts_with("CREATE VIRTUAL TABLE")
                });
            if kind == ObjectKind::Table && !virtual_table {
                object.estimated_rows = table_rows(conn, schema, &object.name, &mut count_budget)?;
            }
            if virtual_table && target.is_some() {
                result.incomplete = true;
            }

            if (target.is_some() || navigation)
                && matches!(kind, ObjectKind::Table | ObjectKind::View)
                && !virtual_table
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
                let dependency_sql = match sql.as_ref() {
                    Some(sql)
                        if (kind == ObjectKind::View
                            || kind == ObjectKind::Trigger
                            || virtual_table)
                            && sql.len() <= MAX_VIEW_SQL
                            && sql.len() <= dependency_sql_budget =>
                    {
                        dependency_sql_budget -= sql.len();
                        Some(sql.clone())
                    }
                    _ => None,
                };
                let sql_limited = sql.is_some() && dependency_sql.is_none();
                dependency_sources.push(DependencySource {
                    schema: schema.into(),
                    name: name.clone(),
                    kind,
                    view_sql: (kind == ObjectKind::View)
                        .then(|| dependency_sql.clone())
                        .flatten(),
                    view_sql_limited: kind == ObjectKind::View && sql_limited,
                    trigger_sql: (kind == ObjectKind::Trigger)
                        .then(|| dependency_sql.clone())
                        .flatten(),
                    trigger_sql_limited: kind == ObjectKind::Trigger && sql_limited,
                    virtual_sql: virtual_table.then_some(dependency_sql).flatten(),
                    virtual_sql_limited: virtual_table && sql_limited,
                    virtual_table,
                    table_name,
                });
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
            code: "sqlite_expression_dependencies_unavailable".into(),
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
        enrich_dependencies(&mut graph, &dependency_sources);
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

/// SQLite has catalog-proven FK and trigger targets, but view reads come from
/// stored SQL. Keep those certainty levels distinct and leave missing or
/// filtered targets unresolved instead of inferring a cross-schema match.
fn enrich_dependencies(graph: &mut CatalogGraphData, sources: &[DependencySource]) {
    let schemas = graph
        .nodes
        .iter()
        .filter(|node| node.kind == CatalogNodeKind::Schema)
        .map(|node| (node.id.clone(), node.name.to_ascii_lowercase()))
        .collect::<HashMap<_, _>>();
    let parents = graph
        .nodes
        .iter()
        .map(|node| (node.id.clone(), node.parent_id.clone()))
        .collect::<HashMap<_, _>>();
    let references = graph
        .nodes
        .iter()
        .filter_map(|node| match &node.details {
            CatalogNodeDetails::Constraint { constraint }
                if constraint.kind == ConstraintKind::ForeignKey =>
            {
                constraint
                    .references
                    .as_ref()
                    .map(|reference| (node.id.clone(), reference.clone()))
            }
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    let mut objects = HashMap::new();
    let mut relations = HashMap::new();
    for node in &graph.nodes {
        let Some(schema) = node.parent_id.as_ref().and_then(|id| schemas.get(id)) else {
            continue;
        };
        let key = (schema.clone(), node.name.to_ascii_lowercase());
        if matches!(node.kind, CatalogNodeKind::Table | CatalogNodeKind::View) {
            relations.insert(key.clone(), node.id.clone());
        }
        objects.insert((key.0, key.1, node.kind), node.id.clone());
    }

    // The provider-neutral normalizer resolves unqualified FK names across
    // every schema. SQLite FKs target a table in their own schema only.
    for edge in &mut graph.edges {
        if edge.kind != CatalogEdgeKind::ForeignKey {
            continue;
        }
        let Some(table) = parents.get(&edge.from).and_then(Option::as_ref) else {
            continue;
        };
        let Some(schema) = parents
            .get(table)
            .and_then(Option::as_ref)
            .and_then(|id| schemas.get(id))
        else {
            continue;
        };
        let Some(reference) = references.get(&edge.from) else {
            continue;
        };
        let target = objects.get(&(
            schema.clone(),
            reference.to_ascii_lowercase(),
            CatalogNodeKind::Table,
        ));
        edge.to = target.cloned();
        edge.certainty = if target.is_some() {
            CatalogEdgeCertainty::CatalogProven
        } else {
            CatalogEdgeCertainty::Unresolved
        };
        edge.referenced_path = target.is_none().then(|| format!("{schema}.{reference}"));
        edge.column_pairs.clear();
    }

    let mut new_edges = 0usize;
    for source in sources {
        let schema = source.schema.to_ascii_lowercase();
        let Some(from) = objects.get(&(
            schema.clone(),
            source.name.to_ascii_lowercase(),
            CatalogNodeKind::from(source.kind),
        )) else {
            continue;
        };
        if new_edges >= MAX_DEPENDENCY_EDGES {
            coverage_failure(graph, &source.schema, "sqlite_dependency_edge_limit");
            mark_dependency_gap(graph, from, "dependency_edge_limit");
            break;
        }
        match source.kind {
            ObjectKind::Trigger => {
                if source.table_name.len() > 4096 {
                    coverage_failure(graph, &source.schema, "sqlite_dependency_path_limit");
                    mark_dependency_gap(graph, from, "dependency_path_limit");
                    continue;
                }
                let target = resolve_relation(&relations, &schema, None, &source.table_name);
                push_dependency(
                    graph,
                    from.clone(),
                    target,
                    CatalogEdgeKind::TriggerOn,
                    CatalogEdgeCertainty::CatalogProven,
                    &source.table_name,
                );
                new_edges += 1;
                let Some(sql) = source.trigger_sql.as_deref() else {
                    let code = if source.trigger_sql_limited {
                        "sqlite_trigger_sql_limit"
                    } else {
                        "sqlite_trigger_sql_unavailable"
                    };
                    coverage_failure(graph, &source.schema, code);
                    mark_dependency_gap(graph, from, code);
                    continue;
                };
                let Some(references) = trigger_body_references(sql) else {
                    coverage_failure(graph, &source.schema, "sqlite_trigger_body_unparsed");
                    mark_dependency_gap(graph, from, "trigger_body_unparsed");
                    continue;
                };
                let mut seen = HashSet::new();
                for reference in references {
                    let path = reference.to_string();
                    if path.len() > 4096 {
                        coverage_failure(graph, &source.schema, "sqlite_dependency_path_limit");
                        mark_dependency_gap(graph, from, "dependency_path_limit");
                        continue;
                    }
                    if !seen.insert(path.to_ascii_lowercase()) {
                        continue;
                    }
                    if new_edges >= MAX_DEPENDENCY_EDGES {
                        coverage_failure(graph, &source.schema, "sqlite_dependency_edge_limit");
                        mark_dependency_gap(graph, from, "dependency_edge_limit");
                        break;
                    }
                    let target = relation_target(&relations, &schema, &reference);
                    push_dependency(
                        graph,
                        from.clone(),
                        target,
                        CatalogEdgeKind::DependsOn,
                        CatalogEdgeCertainty::Parsed,
                        &path,
                    );
                    new_edges += 1;
                }
            }
            ObjectKind::View => {
                let Some(sql) = source.view_sql.as_deref() else {
                    let code = if source.view_sql_limited {
                        "sqlite_view_sql_limit"
                    } else {
                        "sqlite_view_sql_unavailable"
                    };
                    coverage_failure(graph, &source.schema, code);
                    mark_dependency_gap(graph, from, code);
                    continue;
                };
                let Some(references) = view_references(sql) else {
                    coverage_failure(graph, &source.schema, "sqlite_view_sql_unparsed");
                    mark_dependency_gap(graph, from, "view_sql_unparsed");
                    continue;
                };
                let mut seen = HashSet::new();
                for reference in references {
                    let path = reference.to_string();
                    if path.len() > 4096 {
                        coverage_failure(graph, &source.schema, "sqlite_dependency_path_limit");
                        mark_dependency_gap(graph, from, "dependency_path_limit");
                        continue;
                    }
                    if !seen.insert(path.to_ascii_lowercase()) {
                        continue;
                    }
                    if new_edges >= MAX_DEPENDENCY_EDGES {
                        coverage_failure(graph, &source.schema, "sqlite_dependency_edge_limit");
                        mark_dependency_gap(graph, from, "dependency_edge_limit");
                        break;
                    }
                    let (target_schema, name) = match reference.0.as_slice() {
                        [name] => (None, name.value.as_str()),
                        [schema, name] => (Some(schema.value.as_str()), name.value.as_str()),
                        _ => {
                            push_dependency(
                                graph,
                                from.clone(),
                                None,
                                CatalogEdgeKind::ReadsFrom,
                                CatalogEdgeCertainty::Parsed,
                                &path,
                            );
                            new_edges += 1;
                            continue;
                        }
                    };
                    let target = resolve_relation(&relations, &schema, target_schema, name);
                    push_dependency(
                        graph,
                        from.clone(),
                        target,
                        CatalogEdgeKind::ReadsFrom,
                        CatalogEdgeCertainty::Parsed,
                        &path,
                    );
                    new_edges += 1;
                }
            }
            ObjectKind::Table if source.virtual_table => {
                if graph.coverage.failures.len() < 4_096 {
                    graph.coverage.failures.push(CatalogCoverageFailure {
                        stage: "columns".into(),
                        schema: Some(source.schema.clone()),
                        code: "sqlite_virtual_table_columns_unavailable".into(),
                    });
                }
                if let Some(node) = graph.nodes.iter_mut().find(|node| &node.id == from) {
                    node.extra.insert(
                        "sqlite_metadata_gap".into(),
                        "virtual_table_columns_unavailable".into(),
                    );
                }
                let Some(sql) = source.virtual_sql.as_deref() else {
                    let code = if source.virtual_sql_limited {
                        "sqlite_virtual_table_sql_limit"
                    } else {
                        "sqlite_virtual_table_sql_unavailable"
                    };
                    coverage_failure(graph, &source.schema, code);
                    mark_dependency_gap(graph, from, code);
                    continue;
                };
                match fts5_external_content(sql) {
                    Some(Some(path)) if path.len() <= 4096 && new_edges < MAX_DEPENDENCY_EDGES => {
                        let target = resolve_relation(&relations, &schema, Some(&schema), &path);
                        push_dependency(
                            graph,
                            from.clone(),
                            target,
                            CatalogEdgeKind::DependsOn,
                            CatalogEdgeCertainty::Parsed,
                            &path,
                        );
                        new_edges += 1;
                    }
                    Some(None) => {}
                    _ => {
                        coverage_failure(
                            graph,
                            &source.schema,
                            "sqlite_virtual_table_dependencies_unavailable",
                        );
                        mark_dependency_gap(graph, from, "virtual_table_dependencies_unavailable");
                    }
                }
            }
            _ => {}
        }
    }
    sift_core::catalog::normalize_graph(graph);
}

fn resolve_relation(
    relations: &HashMap<(String, String), CatalogObjectId>,
    source_schema: &str,
    named_schema: Option<&str>,
    name: &str,
) -> Option<CatalogObjectId> {
    let name = name.to_ascii_lowercase();
    if let Some(schema) = named_schema {
        return matches!(schema.to_ascii_lowercase().as_str(), "main" | "temp")
            .then(|| relations.get(&(schema.to_ascii_lowercase(), name)))
            .flatten()
            .cloned();
    }
    let local = relations.get(&(source_schema.into(), name.clone()));
    if source_schema == "main" {
        return local.cloned();
    }
    let main = relations.get(&("main".into(), name));
    match (local, main) {
        (Some(_), Some(_)) => None,
        (Some(id), None) | (None, Some(id)) => Some(id.clone()),
        (None, None) => None,
    }
}

fn relation_target(
    relations: &HashMap<(String, String), CatalogObjectId>,
    schema: &str,
    reference: &ObjectName,
) -> Option<CatalogObjectId> {
    match reference.0.as_slice() {
        [name] => resolve_relation(relations, schema, None, &name.value),
        [named_schema, name] => {
            resolve_relation(relations, schema, Some(&named_schema.value), &name.value)
        }
        _ => None,
    }
}

fn push_dependency(
    graph: &mut CatalogGraphData,
    from: CatalogObjectId,
    target: Option<CatalogObjectId>,
    kind: CatalogEdgeKind,
    proven: CatalogEdgeCertainty,
    path: &str,
) {
    graph.edges.push(CatalogEdge {
        from,
        to: target.clone(),
        kind,
        certainty: if target.is_some() {
            proven
        } else {
            CatalogEdgeCertainty::Unresolved
        },
        referenced_path: target.is_none().then(|| path.into()),
        column_pairs: Vec::new(),
    });
}

fn coverage_failure(graph: &mut CatalogGraphData, schema: &str, code: &str) {
    if graph.coverage.failures.len() < 4_096 {
        graph.coverage.failures.push(CatalogCoverageFailure {
            stage: "dependencies".into(),
            schema: Some(schema.into()),
            code: code.into(),
        });
    }
}

fn mark_dependency_gap(graph: &mut CatalogGraphData, id: &CatalogObjectId, reason: &str) {
    if let Some(node) = graph.nodes.iter_mut().find(|node| &node.id == id) {
        node.extra
            .insert("sqlite_dependency_gap".into(), reason.into());
    }
}

#[derive(Default)]
struct ViewRelations {
    cte_scopes: Vec<HashSet<String>>,
    references: Vec<ObjectName>,
}

impl Visitor for ViewRelations {
    type Break = ();

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<Self::Break> {
        self.cte_scopes
            .push(query.with.as_ref().map_or_else(HashSet::new, |with| {
                with.cte_tables
                    .iter()
                    .map(|cte| cte.alias.name.value.to_ascii_lowercase())
                    .collect()
            }));
        ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _query: &Query) -> ControlFlow<Self::Break> {
        self.cte_scopes.pop();
        ControlFlow::Continue(())
    }

    fn pre_visit_relation(&mut self, relation: &ObjectName) -> ControlFlow<Self::Break> {
        if let [name] = relation.0.as_slice() {
            if self
                .cte_scopes
                .iter()
                .rev()
                .any(|scope| scope.contains(&name.value.to_ascii_lowercase()))
            {
                return ControlFlow::Continue(());
            }
        }
        if self.references.len() >= MAX_VIEW_REFERENCES {
            return ControlFlow::Break(());
        }
        self.references.push(relation.clone());
        ControlFlow::Continue(())
    }
}

fn view_references(sql: &str) -> Option<Vec<ObjectName>> {
    if sql.len() > MAX_VIEW_SQL {
        return None;
    }
    let mut statements = Parser::parse_sql(&SQLiteDialect {}, sql).ok()?;
    let [sqlparser::ast::Statement::CreateView { query, .. }] = statements.as_mut_slice() else {
        return None;
    };
    let mut collector = ViewRelations::default();
    if query.visit(&mut collector).is_break() {
        return None;
    }
    Some(collector.references)
}

// sqlparser's CREATE TRIGGER grammar is PostgreSQL-only. SQLite stores the
// complete statement, so isolate its BEGIN...END body using SQL tokens and
// parse the contained DML statements with the SQLite dialect. Token matching
// avoids mistaking quoted text or comments for the body delimiters.
fn trigger_body_references(sql: &str) -> Option<Vec<ObjectName>> {
    if sql.len() > MAX_VIEW_SQL {
        return None;
    }
    let tokens = Tokenizer::new(&SQLiteDialect {}, sql).tokenize().ok()?;
    let word_is = |token: &Token, value: &str| {
        matches!(token, Token::Word(word)
            if word.quote_style.is_none() && word.value.eq_ignore_ascii_case(value))
    };
    let begin = tokens.iter().position(|token| word_is(token, "BEGIN"))?;
    let end = tokens.iter().rposition(|token| word_is(token, "END"))?;
    if end <= begin + 1 {
        return None;
    }
    let body = tokens[begin + 1..end]
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" ");
    let statements = Parser::parse_sql(&SQLiteDialect {}, &body).ok()?;
    if statements.is_empty() {
        return None;
    }
    let mut collector = ViewRelations::default();
    for statement in &statements {
        if statement.visit(&mut collector).is_break() {
            return None;
        }
    }
    Some(collector.references)
}

// FTS5's external-content option names a table in the same SQLite schema.
// Its other modes have no catalog table dependency. Other virtual-table
// modules remain unknown because their argument semantics are module-defined.
fn fts5_external_content(sql: &str) -> Option<Option<String>> {
    let tokens = Tokenizer::new(&SQLiteDialect {}, sql)
        .tokenize()
        .ok()?
        .into_iter()
        .filter(|token| !matches!(token, Token::Whitespace(_)))
        .collect::<Vec<_>>();
    let using = tokens.iter().position(|token| {
        matches!(token, Token::Word(word)
        if word.quote_style.is_none() && word.value.eq_ignore_ascii_case("USING"))
    })?;
    if !matches!(tokens.get(using + 1), Some(Token::Word(word))
        if word.quote_style.is_none() && word.value.eq_ignore_ascii_case("fts5"))
    {
        return None;
    }
    for triple in tokens.windows(3) {
        if !matches!(&triple[0], Token::Word(word)
            if word.quote_style.is_none() && word.value.eq_ignore_ascii_case("content"))
            || !matches!(triple[1], Token::Eq)
        {
            continue;
        }
        return match &triple[2] {
            Token::SingleQuotedString(value) | Token::DoubleQuotedString(value) => {
                Some((!value.is_empty()).then(|| value.clone()))
            }
            _ => None,
        };
    }
    Some(None)
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
