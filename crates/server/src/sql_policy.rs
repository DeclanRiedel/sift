//! Engine-aware SQL and object policy checks for restricted profiles.

use std::collections::HashSet;
use std::ops::ControlFlow;

use sift_protocol::{
    ConnectionPolicy, Engine, ObjectPath, OperationKind, SchemaSelector, SchemaSnapshot,
};
use sqlparser::ast::{Expr, ObjectName, Query, SetExpr, Statement, Visit, Visitor};
use sqlparser::dialect::{MsSqlDialect, PostgreSqlDialect};
use sqlparser::parser::Parser;

use crate::error::{ApiError, ApiResult};

pub fn enforce(
    policy: &ConnectionPolicy,
    engine: Engine,
    operation: OperationKind,
    sql: Option<&str>,
    objects: &[&ObjectPath],
) -> ApiResult<()> {
    if policy.read_only && is_structured_write(operation) {
        return Err(ApiError::Forbidden(
            "connection profile is read-only".into(),
        ));
    }
    if let Some(sql) = sql {
        enforce_sql(policy, engine, operation, sql)?;
    }
    if let Some(selectors) = &policy.allowed_schemas {
        for object in objects {
            enforce_object(selectors, engine, object)?;
        }
    }
    Ok(())
}

pub fn filter_snapshot(policy: &ConnectionPolicy, snapshot: &mut SchemaSnapshot) {
    let Some(selectors) = &policy.allowed_schemas else {
        return;
    };
    snapshot.trees.retain_mut(|catalog| {
        catalog.schemas.retain(|schema| {
            selectors.iter().any(|selector| {
                selector.schema == schema.name
                    && selector
                        .catalog
                        .as_ref()
                        .map_or(true, |expected| expected == &catalog.name)
            })
        });
        !catalog.schemas.is_empty()
    });
}

fn enforce_sql(
    policy: &ConnectionPolicy,
    engine: Engine,
    operation: OperationKind,
    sql: &str,
) -> ApiResult<()> {
    if !policy.read_only && policy.allowed_schemas.is_none() {
        return Ok(());
    }
    let statements = match engine {
        Engine::Postgres => Parser::parse_sql(&PostgreSqlDialect {}, sql),
        Engine::SqlServer => Parser::parse_sql(&MsSqlDialect {}, sql),
    }
    .map_err(|_| ApiError::Forbidden("restricted connection requires classifiable SQL".into()))?;
    if statements.is_empty() {
        return Err(ApiError::Forbidden(
            "restricted connection rejects an empty SQL request".into(),
        ));
    }
    let internal_showplan = operation == OperationKind::Explain
        && matches!(
            sql.trim().to_ascii_uppercase().as_str(),
            "SET SHOWPLAN_XML ON" | "SET SHOWPLAN_XML OFF"
        );
    if policy.read_only
        && !internal_showplan
        && statements.iter().any(|statement| !is_read_only(statement))
    {
        return Err(ApiError::Forbidden(
            "connection profile is read-only".into(),
        ));
    }
    if let Some(selectors) = &policy.allowed_schemas {
        let mut visitor = RelationVisitor::new(engine, selectors);
        for statement in &statements {
            if statement.visit(&mut visitor).is_break() {
                return Err(ApiError::Forbidden(
                    "SQL references a schema outside the connection policy".into(),
                ));
            }
        }
    }
    Ok(())
}

fn is_structured_write(operation: OperationKind) -> bool {
    matches!(
        operation,
        OperationKind::ApplyEdits
            | OperationKind::KillProcess
            | OperationKind::ImportCsv
            | OperationKind::BulkInsert
    )
}

fn is_read_only(statement: &Statement) -> bool {
    match statement {
        Statement::Query(query) => query_is_read_only(query),
        Statement::Explain {
            analyze, statement, ..
        } => !analyze && is_read_only(statement),
        Statement::ExplainTable { .. } | Statement::ShowVariable { .. } => true,
        _ => false,
    }
}

fn query_is_read_only(query: &Query) -> bool {
    query.with.as_ref().map_or(true, |with| {
        with.cte_tables
            .iter()
            .all(|cte| query_is_read_only(&cte.query))
    }) && set_expr_is_read_only(&query.body)
}

fn set_expr_is_read_only(expression: &SetExpr) -> bool {
    match expression {
        SetExpr::Select(select) => select.into.is_none(),
        SetExpr::Query(query) => query_is_read_only(query),
        SetExpr::SetOperation { left, right, .. } => {
            set_expr_is_read_only(left) && set_expr_is_read_only(right)
        }
        SetExpr::Values(_) | SetExpr::Table(_) => true,
        SetExpr::Insert(_) | SetExpr::Update(_) => false,
    }
}

fn enforce_object(
    selectors: &[SchemaSelector],
    engine: Engine,
    object: &ObjectPath,
) -> ApiResult<()> {
    let schema = object.schema.as_deref().ok_or_else(|| {
        ApiError::Forbidden("schema-restricted operations require a qualified object".into())
    })?;
    if selector_allows(selectors, engine, object.catalog.as_deref(), schema) {
        Ok(())
    } else {
        Err(ApiError::Forbidden(
            "object schema is outside the connection policy".into(),
        ))
    }
}

struct RelationVisitor<'a> {
    engine: Engine,
    selectors: &'a [SchemaSelector],
    cte_scopes: Vec<HashSet<String>>,
}

impl<'a> RelationVisitor<'a> {
    fn new(engine: Engine, selectors: &'a [SchemaSelector]) -> Self {
        Self {
            engine,
            selectors,
            cte_scopes: Vec::new(),
        }
    }

    fn relation_allowed(&self, relation: &ObjectName) -> bool {
        let parts = &relation.0;
        match parts.as_slice() {
            [name] => self
                .cte_scopes
                .iter()
                .rev()
                .any(|scope| scope.contains(&normalize_ident(self.engine, name))),
            [schema, _] => selector_allows(
                self.selectors,
                self.engine,
                None,
                &normalize_ident(self.engine, schema),
            ),
            [catalog, schema, _] => selector_allows(
                self.selectors,
                self.engine,
                Some(&normalize_ident(self.engine, catalog)),
                &normalize_ident(self.engine, schema),
            ),
            _ => false,
        }
    }
}

impl Visitor for RelationVisitor<'_> {
    type Break = ();

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<Self::Break> {
        let scope = if let Some(with) = &query.with {
            with.cte_tables
                .iter()
                .map(|cte| normalize_ident(self.engine, &cte.alias.name))
                .collect()
        } else {
            HashSet::new()
        };
        self.cte_scopes.push(scope);
        ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _query: &Query) -> ControlFlow<Self::Break> {
        self.cte_scopes.pop();
        ControlFlow::Continue(())
    }

    fn pre_visit_relation(&mut self, relation: &ObjectName) -> ControlFlow<Self::Break> {
        if self.relation_allowed(relation) {
            ControlFlow::Continue(())
        } else {
            ControlFlow::Break(())
        }
    }

    fn pre_visit_expr(&mut self, expression: &Expr) -> ControlFlow<Self::Break> {
        if let Expr::Function(function) = expression {
            let parts = &function.name.0;
            if let [schema, _] = parts.as_slice() {
                if !selector_allows(
                    self.selectors,
                    self.engine,
                    None,
                    &normalize_ident(self.engine, schema),
                ) {
                    return ControlFlow::Break(());
                }
            } else if let [catalog, schema, _] = parts.as_slice() {
                if !selector_allows(
                    self.selectors,
                    self.engine,
                    Some(&normalize_ident(self.engine, catalog)),
                    &normalize_ident(self.engine, schema),
                ) {
                    return ControlFlow::Break(());
                }
            } else if parts.len() > 3 {
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    }
}

fn selector_allows(
    selectors: &[SchemaSelector],
    engine: Engine,
    catalog: Option<&str>,
    schema: &str,
) -> bool {
    selectors.iter().any(|selector| {
        normalize_selector(engine, &selector.schema) == normalize_selector(engine, schema)
            && match (&selector.catalog, catalog) {
                (None, _) => true,
                (Some(expected), Some(actual)) => {
                    normalize_selector(engine, expected) == normalize_selector(engine, actual)
                }
                _ => false,
            }
    })
}

fn normalize_ident(engine: Engine, ident: &sqlparser::ast::Ident) -> String {
    if engine == Engine::Postgres && ident.quote_style.is_some() {
        ident.value.clone()
    } else {
        normalize(engine, &ident.value)
    }
}

fn normalize_selector(engine: Engine, value: &str) -> String {
    match engine {
        Engine::Postgres => value.to_string(),
        Engine::SqlServer => value.to_lowercase(),
    }
}

fn normalize(engine: Engine, value: &str) -> String {
    match engine {
        Engine::Postgres => value.to_lowercase(),
        Engine::SqlServer => value.to_lowercase(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn restricted() -> ConnectionPolicy {
        ConnectionPolicy {
            read_only: true,
            allowed_schemas: Some(vec![SchemaSelector {
                catalog: None,
                schema: "public".into(),
            }]),
            ..ConnectionPolicy::default()
        }
    }

    #[test]
    fn read_only_rejects_writes_and_select_into() {
        let policy = restricted();
        assert!(enforce_sql(
            &policy,
            Engine::Postgres,
            OperationKind::ExecuteQuery,
            "SELECT * FROM public.users"
        )
        .is_ok());
        assert!(enforce_sql(
            &policy,
            Engine::Postgres,
            OperationKind::ExecuteQuery,
            "UPDATE public.users SET name = 'x'"
        )
        .is_err());
        assert!(enforce_sql(
            &policy,
            Engine::SqlServer,
            OperationKind::ExecuteQuery,
            "SELECT * INTO public.copy FROM public.users"
        )
        .is_err());
    }

    #[test]
    fn both_dialect_corpora_fail_closed_under_restrictions() {
        let policy = restricted();
        let corpora = [
            (
                Engine::Postgres,
                vec![
                    ("SELECT id FROM public.users", true),
                    ("SELECT id FROM secret.users", false),
                    ("SELECT id FROM users", false),
                    ("INSERT INTO public.users(id) VALUES (1)", false),
                    ("DO $$ BEGIN DELETE FROM public.users; END $$", false),
                ],
            ),
            (
                Engine::SqlServer,
                vec![
                    ("SELECT id FROM public.users", true),
                    ("SELECT id FROM secret.users", false),
                    ("SELECT id FROM users", false),
                    ("UPDATE public.users SET id = 2", false),
                    ("EXEC public.rebuild_users", false),
                ],
            ),
        ];
        for (engine, cases) in corpora {
            for (sql, allowed) in cases {
                assert_eq!(
                    enforce_sql(&policy, engine, OperationKind::ExecuteQuery, sql).is_ok(),
                    allowed,
                    "{engine:?}: {sql}"
                );
            }
        }
    }

    #[test]
    fn schema_policy_requires_qualification_and_allows_ctes() {
        let policy = restricted();
        assert!(enforce_sql(
            &policy,
            Engine::Postgres,
            OperationKind::ExecuteQuery,
            "SELECT * FROM users"
        )
        .is_err());
        assert!(enforce_sql(
            &policy,
            Engine::Postgres,
            OperationKind::ExecuteQuery,
            "WITH u AS (SELECT * FROM public.users) SELECT * FROM u"
        )
        .is_ok());
        assert!(enforce_sql(
            &policy,
            Engine::Postgres,
            OperationKind::ExecuteQuery,
            "SELECT * FROM secret.users"
        )
        .is_err());
        assert!(enforce_sql(
            &policy,
            Engine::Postgres,
            OperationKind::ExecuteQuery,
            "WITH u AS (SELECT * FROM public.users) SELECT * FROM u; SELECT * FROM u"
        )
        .is_err());
    }

    #[test]
    fn schema_snapshots_are_filtered_before_reaching_consumers() {
        let policy = restricted();
        let mut snapshot = SchemaSnapshot {
            trees: vec![sift_protocol::CatalogTree {
                name: "app".into(),
                schemas: vec![
                    sift_protocol::SchemaTree {
                        name: "public".into(),
                        objects: vec![],
                    },
                    sift_protocol::SchemaTree {
                        name: "secret".into(),
                        objects: vec![],
                    },
                ],
            }],
            fetched_at: chrono::Utc::now(),
            scope: sift_protocol::SchemaScope::shallow(),
            incomplete: false,
        };
        filter_snapshot(&policy, &mut snapshot);
        assert_eq!(snapshot.trees.len(), 1);
        assert_eq!(snapshot.trees[0].schemas.len(), 1);
        assert_eq!(snapshot.trees[0].schemas[0].name, "public");
    }
}
