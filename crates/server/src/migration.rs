//! Conservative engine-aware migration rendering (ADR-033).

use std::collections::{BTreeSet, HashMap, HashSet};

use sha2::{Digest, Sha256};
use sift_protocol::{
    CatalogGraph, CatalogNode, CatalogNodeDetails, CatalogNodeKind, ConstraintKind, Engine,
    MigrationGroup, MigrationOptions, MigrationPlan, MigrationPlanId, MigrationStatement,
    Nullability, SchemaChange, SchemaChangeId, SchemaChangeKind, SchemaChangeRisk, SchemaDiff,
};

#[derive(Debug, thiserror::Error)]
pub enum MigrationRenderError {
    #[error("selected schema change does not exist in the diff")]
    UnknownChange,
    #[error("selected schema changes omit required prerequisite {0}")]
    MissingPrerequisite(SchemaChangeId),
    #[error("dependency group requires an engine-specific strategy: {0}")]
    UnsupportedDependencyCycle(String),
    #[error("migration rendering is not supported for {kind:?} change {change}")]
    UnsupportedChange {
        change: SchemaChangeId,
        kind: CatalogNodeKind,
    },
    #[error("catalog hierarchy is incomplete for migration change {0}")]
    IncompleteHierarchy(SchemaChangeId),
    #[error("schema change {0} does not contain the objects required by its kind")]
    InvalidChangeShape(SchemaChangeId),
    #[error("migration plan has no executable statements")]
    EmptyPlan,
    #[error("migration plan could not be serialized")]
    Serialization,
}

pub fn render_plan(
    engine: Engine,
    diff: &SchemaDiff,
    from: &CatalogGraph,
    to: &CatalogGraph,
    selected: &[SchemaChangeId],
    expected_live_revision: sift_protocol::CatalogRevision,
    options: &MigrationOptions,
) -> Result<MigrationPlan, MigrationRenderError> {
    let selected = if selected.is_empty() {
        diff.changes
            .iter()
            .map(|change| change.id.clone())
            .collect::<HashSet<_>>()
    } else {
        let unique = selected.iter().cloned().collect::<HashSet<_>>();
        if unique.len() != selected.len()
            || unique
                .iter()
                .any(|id| !diff.changes.iter().any(|change| &change.id == id))
        {
            return Err(MigrationRenderError::UnknownChange);
        }
        unique
    };
    let selected_changes = diff
        .changes
        .iter()
        .filter(|change| selected.contains(&change.id))
        .collect::<Vec<_>>();
    for change in &selected_changes {
        if let Some(missing) = change
            .prerequisites
            .iter()
            .find(|prerequisite| !selected.contains(*prerequisite))
        {
            return Err(MigrationRenderError::MissingPrerequisite(missing.clone()));
        }
    }
    if let Some(group) = selected_changes
        .iter()
        .find_map(|change| change.dependency_group.as_ref())
    {
        return Err(MigrationRenderError::UnsupportedDependencyCycle(
            group.clone(),
        ));
    }
    let created = selected_changes
        .iter()
        .filter(|change| change.kind == SchemaChangeKind::Create)
        .filter_map(|change| change.object_after.as_ref().map(|node| node.id.clone()))
        .collect::<HashSet<_>>();
    let dropped = selected_changes
        .iter()
        .filter(|change| change.kind == SchemaChangeKind::Drop)
        .filter_map(|change| change.object_before.as_ref().map(|node| node.id.clone()))
        .collect::<HashSet<_>>();
    let from_nodes = nodes(from);
    let to_nodes = nodes(to);
    let mut statements = Vec::new();
    let mut warnings = diff.warnings.clone();
    if options.online_indexes {
        warnings.push(
            "online index rendering is not enabled without server/edition capability proof".into(),
        );
    }
    for change in selected_changes {
        if implicitly_covered(change, &created, &dropped) {
            continue;
        }
        let sql = render_change(engine, change, &from_nodes, &to_nodes, to)?;
        let Some(sql) = sql else {
            continue;
        };
        statements.push(MigrationStatement {
            ordinal: u32::try_from(statements.len() + 1).unwrap_or(u32::MAX),
            fingerprint: crate::fingerprint::sql(&sql),
            sql,
            change_ids: vec![change.id.clone()],
            risk: change.risk,
        });
    }
    if statements.is_empty() {
        return Err(MigrationRenderError::EmptyPlan);
    }
    let required_acknowledgements = statements
        .iter()
        .filter_map(|statement| {
            matches!(
                statement.risk,
                SchemaChangeRisk::DataLoss
                    | SchemaChangeRisk::DataRewrite
                    | SchemaChangeRisk::Privilege
                    | SchemaChangeRisk::Unknown
            )
            .then_some(statement.risk)
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let groups = vec![MigrationGroup {
        ordinal: 1,
        transactional: options.prefer_transactional,
        statements,
    }];
    let id = MigrationPlanId(uuid::Uuid::new_v4());
    let run_id = sift_protocol::MigrationRunId(uuid::Uuid::new_v4());
    let expires_at = chrono::Utc::now() + chrono::Duration::minutes(10);
    let digest_bytes = serde_json::to_vec(&(
        id,
        run_id,
        &diff.digest,
        expected_live_revision,
        &groups,
        &required_acknowledgements,
        &warnings,
        expires_at,
    ))
    .map_err(|_| MigrationRenderError::Serialization)?;
    Ok(MigrationPlan {
        id,
        run_id,
        digest: format!("migfp:{}", hex_digest(&digest_bytes)),
        diff_digest: diff.digest.clone(),
        expected_live_revision,
        groups,
        required_acknowledgements,
        warnings,
        expires_at,
    })
}

fn nodes(graph: &CatalogGraph) -> HashMap<sift_protocol::CatalogObjectId, &CatalogNode> {
    graph
        .data
        .nodes
        .iter()
        .map(|node| (node.id.clone(), node))
        .collect()
}

fn implicitly_covered(
    change: &SchemaChange,
    created: &HashSet<sift_protocol::CatalogObjectId>,
    dropped: &HashSet<sift_protocol::CatalogObjectId>,
) -> bool {
    match change.kind {
        SchemaChangeKind::Create => change.object_after.as_ref().is_some_and(|node| {
            node.kind == CatalogNodeKind::Column
                && node
                    .parent_id
                    .as_ref()
                    .is_some_and(|parent| created.contains(parent))
        }),
        SchemaChangeKind::Drop => change
            .object_before
            .as_ref()
            .and_then(|node| node.parent_id.as_ref())
            .is_some_and(|parent| dropped.contains(parent)),
        _ => false,
    }
}

fn render_change(
    engine: Engine,
    change: &SchemaChange,
    from_nodes: &HashMap<sift_protocol::CatalogObjectId, &CatalogNode>,
    to_nodes: &HashMap<sift_protocol::CatalogObjectId, &CatalogNode>,
    to: &CatalogGraph,
) -> Result<Option<String>, MigrationRenderError> {
    let node = change
        .object_after
        .as_ref()
        .or(change.object_before.as_ref())
        .ok_or_else(|| MigrationRenderError::InvalidChangeShape(change.id.clone()))?;
    match change.kind {
        SchemaChangeKind::Unknown => unsupported(change, node),
        SchemaChangeKind::Alter => alter_sql(engine, change, from_nodes, to_nodes).map(Some),
        SchemaChangeKind::Create => create_sql(engine, change, node, to_nodes, to).map(Some),
        SchemaChangeKind::Drop => drop_sql(engine, change, node, from_nodes).map(Some),
        SchemaChangeKind::Rename | SchemaChangeKind::Move => {
            rename_or_move_sql(engine, change, from_nodes, to_nodes).map(Some)
        }
    }
}

fn create_sql(
    engine: Engine,
    change: &SchemaChange,
    node: &CatalogNode,
    nodes: &HashMap<sift_protocol::CatalogObjectId, &CatalogNode>,
    graph: &CatalogGraph,
) -> Result<String, MigrationRenderError> {
    match node.kind {
        CatalogNodeKind::Schema => Ok(format!(
            "CREATE SCHEMA {};",
            crate::ddl::quote_ident(&node.name, engine)
        )),
        CatalogNodeKind::Table | CatalogNodeKind::PartitionedTable => {
            let columns = graph
                .data
                .nodes
                .iter()
                .filter(|candidate| {
                    candidate.kind == CatalogNodeKind::Column
                        && candidate.parent_id.as_ref() == Some(&node.id)
                })
                .map(|column| render_column(engine, change, column))
                .collect::<Result<Vec<_>, _>>()?;
            if columns.is_empty() && engine == Engine::SqlServer {
                return unsupported(change, node);
            }
            Ok(format!(
                "CREATE TABLE {} (\n{}\n);",
                qualified_object(engine, change, node, nodes)?,
                columns
                    .iter()
                    .map(|column| format!("    {column}"))
                    .collect::<Vec<_>>()
                    .join(",\n")
            ))
        }
        CatalogNodeKind::Column => {
            let parent = parent(change, node, nodes)?;
            Ok(format!(
                "ALTER TABLE {} ADD {};",
                qualified_object(engine, change, parent, nodes)?,
                render_column(engine, change, node)?
            ))
        }
        CatalogNodeKind::Index => {
            let parent = parent(change, node, nodes)?;
            let CatalogNodeDetails::Index { index } = &node.details else {
                return unsupported(change, node);
            };
            let unique = if index.unique { "UNIQUE " } else { "" };
            let columns = index
                .columns
                .iter()
                .map(|column| crate::ddl::quote_ident(column, engine))
                .collect::<Vec<_>>()
                .join(", ");
            let mut sql = format!(
                "CREATE {unique}INDEX {} ON {} ({columns})",
                crate::ddl::quote_ident(&node.name, engine),
                qualified_object(engine, change, parent, nodes)?
            );
            if let Some(predicate) = &index.partial_predicate {
                sql.push_str(" WHERE ");
                sql.push_str(predicate);
            }
            sql.push(';');
            Ok(sql)
        }
        CatalogNodeKind::Constraint => {
            let parent = parent(change, node, nodes)?;
            let CatalogNodeDetails::Constraint { constraint } = &node.details else {
                return unsupported(change, node);
            };
            let clause = if let Some(definition) = &constraint.definition {
                definition.clone()
            } else if matches!(
                constraint.kind,
                ConstraintKind::PrimaryKey | ConstraintKind::Unique
            ) {
                let kind = if constraint.kind == ConstraintKind::PrimaryKey {
                    "PRIMARY KEY"
                } else {
                    "UNIQUE"
                };
                let columns = constraint
                    .columns
                    .iter()
                    .map(|column| crate::ddl::quote_ident(column, engine))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{kind} ({columns})")
            } else if constraint.kind == ConstraintKind::ForeignKey {
                render_foreign_key(engine, change, node, parent, nodes, graph)?
            } else {
                return unsupported(change, node);
            };
            Ok(format!(
                "ALTER TABLE {} ADD CONSTRAINT {} {clause};",
                qualified_object(engine, change, parent, nodes)?,
                crate::ddl::quote_ident(&node.name, engine)
            ))
        }
        _ => unsupported(change, node),
    }
}

fn render_foreign_key(
    engine: Engine,
    change: &SchemaChange,
    constraint: &CatalogNode,
    source_table: &CatalogNode,
    nodes: &HashMap<sift_protocol::CatalogObjectId, &CatalogNode>,
    graph: &CatalogGraph,
) -> Result<String, MigrationRenderError> {
    let edge = graph
        .data
        .edges
        .iter()
        .find(|edge| {
            edge.from == constraint.id
                && edge.kind == sift_protocol::CatalogEdgeKind::ForeignKey
                && edge.to.is_some()
                && !edge.column_pairs.is_empty()
        })
        .ok_or_else(|| MigrationRenderError::UnsupportedChange {
            change: change.id.clone(),
            kind: constraint.kind,
        })?;
    let target_table = edge
        .to
        .as_ref()
        .and_then(|id| nodes.get(id).copied())
        .ok_or_else(|| MigrationRenderError::IncompleteHierarchy(change.id.clone()))?;
    let mut source_columns = Vec::with_capacity(edge.column_pairs.len());
    let mut target_columns = Vec::with_capacity(edge.column_pairs.len());
    for pair in &edge.column_pairs {
        let source = nodes
            .get(&pair.from)
            .copied()
            .filter(|node| {
                node.kind == CatalogNodeKind::Column
                    && node.parent_id.as_ref() == Some(&source_table.id)
            })
            .ok_or_else(|| MigrationRenderError::IncompleteHierarchy(change.id.clone()))?;
        let target = nodes
            .get(&pair.to)
            .copied()
            .filter(|node| {
                node.kind == CatalogNodeKind::Column
                    && node.parent_id.as_ref() == Some(&target_table.id)
            })
            .ok_or_else(|| MigrationRenderError::IncompleteHierarchy(change.id.clone()))?;
        source_columns.push(crate::ddl::quote_ident(&source.name, engine));
        target_columns.push(crate::ddl::quote_ident(&target.name, engine));
    }
    Ok(format!(
        "FOREIGN KEY ({}) REFERENCES {} ({})",
        source_columns.join(", "),
        qualified_object(engine, change, target_table, nodes)?,
        target_columns.join(", ")
    ))
}

fn drop_sql(
    engine: Engine,
    change: &SchemaChange,
    node: &CatalogNode,
    nodes: &HashMap<sift_protocol::CatalogObjectId, &CatalogNode>,
) -> Result<String, MigrationRenderError> {
    let verb = match node.kind {
        CatalogNodeKind::Schema => "SCHEMA",
        CatalogNodeKind::Table | CatalogNodeKind::PartitionedTable => "TABLE",
        _ => "",
    };
    if !verb.is_empty() {
        return Ok(format!(
            "DROP {verb} {};",
            qualified_object(engine, change, node, nodes)?
        ));
    }
    let parent = parent(change, node, nodes)?;
    let table = qualified_object(engine, change, parent, nodes)?;
    match node.kind {
        CatalogNodeKind::Column => Ok(format!(
            "ALTER TABLE {table} DROP COLUMN {};",
            crate::ddl::quote_ident(&node.name, engine)
        )),
        CatalogNodeKind::Constraint => Ok(format!(
            "ALTER TABLE {table} DROP CONSTRAINT {};",
            crate::ddl::quote_ident(&node.name, engine)
        )),
        CatalogNodeKind::Index => match engine {
            Engine::Postgres => {
                let schema = schema_ancestor(change, parent, nodes)?;
                Ok(format!(
                    "DROP INDEX {}.{};",
                    crate::ddl::quote_ident(&schema.name, engine),
                    crate::ddl::quote_ident(&node.name, engine)
                ))
            }
            Engine::SqlServer => Ok(format!(
                "DROP INDEX {} ON {table};",
                crate::ddl::quote_ident(&node.name, engine)
            )),
        },
        _ => unsupported(change, node),
    }
}

fn rename_or_move_sql(
    engine: Engine,
    change: &SchemaChange,
    from_nodes: &HashMap<sift_protocol::CatalogObjectId, &CatalogNode>,
    to_nodes: &HashMap<sift_protocol::CatalogObjectId, &CatalogNode>,
) -> Result<String, MigrationRenderError> {
    let before = change
        .object_before
        .as_ref()
        .ok_or_else(|| MigrationRenderError::InvalidChangeShape(change.id.clone()))?;
    let after = change
        .object_after
        .as_ref()
        .ok_or_else(|| MigrationRenderError::InvalidChangeShape(change.id.clone()))?;
    if change.kind == SchemaChangeKind::Move {
        let new_schema = schema_ancestor(change, after, to_nodes)?;
        let new_schema = crate::ddl::quote_ident(&new_schema.name, engine);
        return match (engine, before.kind) {
            (
                Engine::Postgres,
                CatalogNodeKind::Table
                | CatalogNodeKind::PartitionedTable
                | CatalogNodeKind::View
                | CatalogNodeKind::MaterializedView
                | CatalogNodeKind::Sequence,
            ) => Ok(format!(
                "ALTER {} {} SET SCHEMA {new_schema};",
                postgres_object_verb(before.kind),
                qualified_object(engine, change, before, from_nodes)?
            )),
            (
                Engine::SqlServer,
                CatalogNodeKind::Table
                | CatalogNodeKind::PartitionedTable
                | CatalogNodeKind::View
                | CatalogNodeKind::TableValuedFunction
                | CatalogNodeKind::ScalarFunction
                | CatalogNodeKind::Procedure
                | CatalogNodeKind::Sequence,
            ) => Ok(format!(
                "ALTER SCHEMA {new_schema} TRANSFER {};",
                qualified_object(engine, change, before, from_nodes)?
            )),
            _ => unsupported(change, after),
        };
    }
    let old = qualified_object(engine, change, before, from_nodes)?;
    let new_name = crate::ddl::quote_ident(&after.name, engine);
    match (engine, before.kind) {
        (Engine::Postgres, CatalogNodeKind::Schema) => {
            Ok(format!("ALTER SCHEMA {old} RENAME TO {new_name};"))
        }
        (Engine::Postgres, CatalogNodeKind::Table | CatalogNodeKind::PartitionedTable) => {
            Ok(format!("ALTER TABLE {old} RENAME TO {new_name};"))
        }
        (Engine::Postgres, CatalogNodeKind::Column) => {
            let table = qualified_object(
                engine,
                change,
                parent(change, before, from_nodes)?,
                from_nodes,
            )?;
            Ok(format!(
                "ALTER TABLE {table} RENAME COLUMN {} TO {new_name};",
                crate::ddl::quote_ident(&before.name, engine)
            ))
        }
        (Engine::SqlServer, CatalogNodeKind::Table | CatalogNodeKind::PartitionedTable) => {
            Ok(format!(
                "EXEC sp_rename N'{}', N'{}';",
                old.replace('\'', "''"),
                after.name.replace('\'', "''")
            ))
        }
        (Engine::SqlServer, CatalogNodeKind::Column) => {
            let table = qualified_object(
                engine,
                change,
                parent(change, before, from_nodes)?,
                from_nodes,
            )?;
            Ok(format!(
                "EXEC sp_rename N'{}.{}', N'{}', N'COLUMN';",
                table.replace('\'', "''"),
                before.name.replace('\'', "''"),
                after.name.replace('\'', "''")
            ))
        }
        _ => unsupported(change, after),
    }
}

fn alter_sql(
    engine: Engine,
    change: &SchemaChange,
    from_nodes: &HashMap<sift_protocol::CatalogObjectId, &CatalogNode>,
    to_nodes: &HashMap<sift_protocol::CatalogObjectId, &CatalogNode>,
) -> Result<String, MigrationRenderError> {
    let before = change
        .object_before
        .as_ref()
        .ok_or_else(|| MigrationRenderError::InvalidChangeShape(change.id.clone()))?;
    let after = change
        .object_after
        .as_ref()
        .ok_or_else(|| MigrationRenderError::InvalidChangeShape(change.id.clone()))?;
    let (
        CatalogNodeDetails::Column {
            column: before_column,
        },
        CatalogNodeDetails::Column {
            column: after_column,
        },
    ) = (&before.details, &after.details)
    else {
        return unsupported(change, after);
    };
    let table = qualified_object(
        engine,
        change,
        parent(change, before, from_nodes)?,
        from_nodes,
    )?;
    let name = crate::ddl::quote_ident(&before.name, engine);
    match engine {
        Engine::Postgres => {
            let mut clauses = Vec::new();
            if before_column.type_ref != after_column.type_ref {
                clauses.push(format!(
                    "ALTER COLUMN {name} TYPE {}",
                    crate::ddl::type_to_sql(&after_column.type_ref, engine)
                ));
            }
            if before_column.nullable != after_column.nullable {
                clauses.push(format!(
                    "ALTER COLUMN {name} {} NOT NULL",
                    if after_column.nullable == Nullability::NotNullable {
                        "SET"
                    } else {
                        "DROP"
                    }
                ));
            }
            let before_default = column_default(before_column, engine);
            let after_default = column_default(after_column, engine);
            if before_default != after_default {
                clauses.push(match after_default {
                    Some(default) => format!("ALTER COLUMN {name} SET DEFAULT {default}"),
                    None => format!("ALTER COLUMN {name} DROP DEFAULT"),
                });
            }
            if clauses.is_empty() {
                return unsupported(change, after);
            }
            Ok(format!("ALTER TABLE {table} {};", clauses.join(", ")))
        }
        Engine::SqlServer => {
            if column_default(before_column, engine) != column_default(after_column, engine) {
                // SQL Server defaults are separately named constraints. The
                // graph must identify that constraint before it is safe to
                // replace; guessing a generated name is forbidden.
                return unsupported(change, after);
            }
            if before_column.type_ref == after_column.type_ref
                && before_column.nullable == after_column.nullable
            {
                return unsupported(change, after);
            }
            // Use the target hierarchy for validation too: a malformed diff
            // must not smuggle an unrelated after-node into executable SQL.
            let _ = parent(change, after, to_nodes)?;
            Ok(format!(
                "ALTER TABLE {table} ALTER COLUMN {name} {} {};",
                crate::ddl::type_to_sql(&after_column.type_ref, engine),
                if after_column.nullable == Nullability::NotNullable {
                    "NOT NULL"
                } else {
                    "NULL"
                }
            ))
        }
    }
}

fn column_default(column: &sift_protocol::ColumnMetadata, engine: Engine) -> Option<&str> {
    match engine {
        Engine::Postgres => column
            .facets
            .postgres
            .as_ref()
            .and_then(|facets| facets.default_expr.as_deref()),
        Engine::SqlServer => column
            .facets
            .sql_server
            .as_ref()
            .and_then(|facets| facets.default_expr.as_deref()),
    }
}

fn postgres_object_verb(kind: CatalogNodeKind) -> &'static str {
    match kind {
        CatalogNodeKind::View => "VIEW",
        CatalogNodeKind::MaterializedView => "MATERIALIZED VIEW",
        CatalogNodeKind::Sequence => "SEQUENCE",
        _ => "TABLE",
    }
}

fn render_column(
    engine: Engine,
    change: &SchemaChange,
    node: &CatalogNode,
) -> Result<String, MigrationRenderError> {
    let CatalogNodeDetails::Column { column } = &node.details else {
        return unsupported(change, node);
    };
    let mut sql = format!(
        "{} {}",
        crate::ddl::quote_ident(&node.name, engine),
        crate::ddl::type_to_sql(&column.type_ref, engine)
    );
    let default = match engine {
        Engine::Postgres => column
            .facets
            .postgres
            .as_ref()
            .and_then(|facets| facets.default_expr.as_deref()),
        Engine::SqlServer => column
            .facets
            .sql_server
            .as_ref()
            .and_then(|facets| facets.default_expr.as_deref()),
    };
    if let Some(default) = default {
        sql.push_str(" DEFAULT ");
        sql.push_str(default);
    }
    if column.nullable == Nullability::NotNullable {
        sql.push_str(" NOT NULL");
    }
    Ok(sql)
}

fn parent<'a>(
    change: &SchemaChange,
    node: &CatalogNode,
    nodes: &'a HashMap<sift_protocol::CatalogObjectId, &'a CatalogNode>,
) -> Result<&'a CatalogNode, MigrationRenderError> {
    node.parent_id
        .as_ref()
        .and_then(|id| nodes.get(id).copied())
        .ok_or_else(|| MigrationRenderError::IncompleteHierarchy(change.id.clone()))
}

fn schema_ancestor<'a>(
    change: &SchemaChange,
    node: &'a CatalogNode,
    nodes: &'a HashMap<sift_protocol::CatalogObjectId, &'a CatalogNode>,
) -> Result<&'a CatalogNode, MigrationRenderError> {
    let mut current = node;
    loop {
        if current.kind == CatalogNodeKind::Schema {
            return Ok(current);
        }
        current = parent(change, current, nodes)?;
    }
}

fn qualified_object(
    engine: Engine,
    change: &SchemaChange,
    node: &CatalogNode,
    nodes: &HashMap<sift_protocol::CatalogObjectId, &CatalogNode>,
) -> Result<String, MigrationRenderError> {
    if node.kind == CatalogNodeKind::Schema {
        return Ok(crate::ddl::quote_ident(&node.name, engine));
    }
    let schema = schema_ancestor(change, node, nodes)?;
    Ok(format!(
        "{}.{}",
        crate::ddl::quote_ident(&schema.name, engine),
        crate::ddl::quote_ident(&node.name, engine)
    ))
}

fn unsupported<T>(change: &SchemaChange, node: &CatalogNode) -> Result<T, MigrationRenderError> {
    Err(MigrationRenderError::UnsupportedChange {
        change: change.id.clone(),
        kind: node.kind,
    })
}

fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use sift_protocol::{
        CatalogCoverage, CatalogGraphOptions, CatalogRevision, CatalogSourceRef, CatalogTree,
        ColumnMetadata, ConstraintInfo, IndexInfo, IndexKind, Nullability, ObjectInfo, ObjectKind,
        PrimitiveType, ProviderRef, SchemaDiffRequest, SchemaTree, TypeRef,
    };

    use super::*;

    fn graph(with_table: bool) -> CatalogGraph {
        let objects = if with_table {
            let mut table = ObjectInfo::new("events", ObjectKind::Table);
            table.columns.push(ColumnMetadata {
                name: "id".into(),
                type_ref: TypeRef::Primitive(PrimitiveType::Int64),
                nullable: Nullability::NotNullable,
                auto_increment: false,
                primary_key: false,
                facets: Default::default(),
            });
            table.indexes.push(IndexInfo {
                name: "idx_events_id".into(),
                columns: vec!["id".into()],
                unique: false,
                primary_key: false,
                kind: IndexKind::Btree,
                partial_predicate: None,
            });
            table.constraints.push(ConstraintInfo {
                name: "pk_events".into(),
                kind: ConstraintKind::PrimaryKey,
                columns: vec!["id".into()],
                definition: None,
                references: None,
            });
            vec![table]
        } else {
            Vec::new()
        };
        let trees = vec![CatalogTree {
            name: "db".into(),
            schemas: vec![SchemaTree {
                name: "public".into(),
                objects,
            }],
        }];
        CatalogGraph {
            revision: CatalogRevision(1),
            content_digest: "catfp:test".into(),
            invalidation_epoch: 0,
            captured_at: chrono::Utc::now(),
            provider: ProviderRef {
                provider_id: sift_protocol::ProviderId::new("test/provider").unwrap(),
                dialect_id: sift_protocol::DialectId::new("test/dialect").unwrap(),
                provider_version: "1".into(),
            },
            database_identity: "db".into(),
            data: sift_core::catalog::graph_from_trees(&trees, CatalogCoverage::complete(), "db"),
        }
    }

    fn source() -> CatalogSourceRef {
        CatalogSourceRef::Live {
            expected_revision: CatalogRevision(1),
            options: CatalogGraphOptions::default(),
        }
    }

    fn graph_in_schema(schema: &str) -> CatalogGraph {
        let mut table = ObjectInfo::new("odd table", ObjectKind::Table);
        table.columns.push(ColumnMetadata::new(
            "id",
            TypeRef::Primitive(PrimitiveType::Int64),
        ));
        let trees = vec![CatalogTree {
            name: "db".into(),
            schemas: vec![SchemaTree {
                name: schema.into(),
                objects: vec![table],
            }],
        }];
        CatalogGraph {
            data: sift_core::catalog::graph_from_trees(&trees, CatalogCoverage::complete(), "db"),
            ..graph(false)
        }
    }

    #[test]
    fn table_creation_absorbs_child_columns_into_one_statement() {
        let from = graph(false);
        let to = graph(true);
        let request = SchemaDiffRequest {
            from: source(),
            to: source(),
            accepted_renames: Vec::new(),
            max_changes: None,
        };
        let diff = sift_core::schema_diff::diff_catalogs(
            request.from.clone(),
            &from,
            request.to.clone(),
            &to,
            &[],
            None,
        )
        .unwrap();
        let plan = render_plan(
            Engine::Postgres,
            &diff,
            &from,
            &to,
            &[],
            CatalogRevision(1),
            &MigrationOptions::default(),
        )
        .unwrap();
        assert_eq!(plan.groups[0].statements.len(), 3);
        assert_eq!(
            plan.groups[0].statements[0].sql,
            "CREATE TABLE \"public\".\"events\" (\n    \"id\" bigint NOT NULL\n);"
        );
        assert!(plan.digest.starts_with("migfp:"));
        assert!(plan.groups[0]
            .statements
            .iter()
            .any(|statement| statement.sql.starts_with("CREATE INDEX")));
        assert!(plan.groups[0]
            .statements
            .iter()
            .any(|statement| statement.sql.contains("ADD CONSTRAINT")));
    }

    #[test]
    fn selected_changes_must_include_changed_prerequisites() {
        let from = graph(false);
        let to = graph(true);
        let diff = sift_core::schema_diff::diff_catalogs(source(), &from, source(), &to, &[], None)
            .unwrap();
        let index = diff
            .changes
            .iter()
            .find(|change| {
                change
                    .object_after
                    .as_ref()
                    .is_some_and(|node| node.kind == CatalogNodeKind::Index)
            })
            .unwrap();
        assert!(matches!(
            render_plan(
                Engine::Postgres,
                &diff,
                &from,
                &to,
                std::slice::from_ref(&index.id),
                CatalogRevision(1),
                &MigrationOptions::default(),
            ),
            Err(MigrationRenderError::MissingPrerequisite(_))
        ));
    }

    #[test]
    fn postgres_column_alter_is_rendered_from_normalized_target_state() {
        let from = graph(true);
        let mut to = from.clone();
        to.content_digest = "catfp:changed".into();
        let column = to
            .data
            .nodes
            .iter_mut()
            .find(|node| node.kind == CatalogNodeKind::Column)
            .unwrap();
        let CatalogNodeDetails::Column { column } = &mut column.details else {
            unreachable!()
        };
        column.type_ref = TypeRef::Primitive(PrimitiveType::Text);
        column.nullable = Nullability::Nullable;
        let diff = sift_core::schema_diff::diff_catalogs(source(), &from, source(), &to, &[], None)
            .unwrap();
        let plan = render_plan(
            Engine::Postgres,
            &diff,
            &from,
            &to,
            &[],
            CatalogRevision(1),
            &MigrationOptions::default(),
        )
        .unwrap();
        assert_eq!(plan.groups[0].statements.len(), 1);
        assert_eq!(
            plan.groups[0].statements[0].sql,
            "ALTER TABLE \"public\".\"events\" ALTER COLUMN \"id\" TYPE text, ALTER COLUMN \"id\" DROP NOT NULL;"
        );
    }

    #[test]
    fn table_designer_add_column_renders_engine_aware_ddl() {
        let from = graph_in_schema("odd schema");
        let table = from
            .data
            .nodes
            .iter()
            .find(|node| node.kind == CatalogNodeKind::Table)
            .unwrap();
        let (to, _) = sift_core::catalog::apply_diagram_mutation(
            &from,
            &sift_protocol::CatalogDiagramMutation::AddColumn {
                table_id: table.id.clone(),
                name: "payload value".into(),
                type_ref: TypeRef::Primitive(PrimitiveType::Text),
                nullability: Nullability::NotNullable,
            },
        )
        .unwrap();
        let diff = sift_core::schema_diff::diff_catalogs(source(), &from, source(), &to, &[], None)
            .unwrap();
        for (engine, expected) in [
            (
                Engine::Postgres,
                "ALTER TABLE \"odd schema\".\"odd table\" ADD \"payload value\" text NOT NULL;",
            ),
            (
                Engine::SqlServer,
                "ALTER TABLE [odd schema].[odd table] ADD [payload value] nvarchar(max) NOT NULL;",
            ),
        ] {
            let plan = render_plan(
                engine,
                &diff,
                &from,
                &to,
                &[],
                CatalogRevision(1),
                &MigrationOptions::default(),
            )
            .unwrap();
            assert_eq!(plan.groups[0].statements[0].sql, expected);
        }
    }

    #[test]
    fn table_move_renders_after_target_schema_create_for_both_engines() {
        let from = graph_in_schema("old schema");
        let to = graph_in_schema("new schema");
        let before = from
            .data
            .nodes
            .iter()
            .find(|node| node.kind == CatalogNodeKind::Table)
            .unwrap();
        let after = to
            .data
            .nodes
            .iter()
            .find(|node| node.kind == CatalogNodeKind::Table)
            .unwrap();
        let diff = sift_core::schema_diff::diff_catalogs(
            source(),
            &from,
            source(),
            &to,
            &[sift_protocol::RenameMapping {
                from: before.id.clone(),
                to: after.id.clone(),
            }],
            None,
        )
        .unwrap();

        let postgres = render_plan(
            Engine::Postgres,
            &diff,
            &from,
            &to,
            &[],
            CatalogRevision(1),
            &MigrationOptions::default(),
        )
        .unwrap();
        assert_eq!(
            postgres.groups[0]
                .statements
                .iter()
                .map(|statement| statement.sql.as_str())
                .collect::<Vec<_>>(),
            vec![
                "CREATE SCHEMA \"new schema\";",
                "ALTER TABLE \"old schema\".\"odd table\" SET SCHEMA \"new schema\";",
                "DROP SCHEMA \"old schema\";",
            ]
        );

        let sql_server = render_plan(
            Engine::SqlServer,
            &diff,
            &from,
            &to,
            &[],
            CatalogRevision(1),
            &MigrationOptions::default(),
        )
        .unwrap();
        assert_eq!(
            sql_server.groups[0]
                .statements
                .iter()
                .map(|statement| statement.sql.as_str())
                .collect::<Vec<_>>(),
            vec![
                "CREATE SCHEMA [new schema];",
                "ALTER SCHEMA [new schema] TRANSFER [old schema].[odd table];",
                "DROP SCHEMA [old schema];",
            ]
        );
    }

    #[test]
    fn diagram_foreign_key_intent_uses_catalog_proven_pairs_for_safe_sql() {
        let mut parent = ObjectInfo::new("parent table", ObjectKind::Table);
        parent.columns.push(ColumnMetadata::new(
            "tenant id",
            TypeRef::Primitive(PrimitiveType::Int64),
        ));
        parent.columns.push(ColumnMetadata::new(
            "id",
            TypeRef::Primitive(PrimitiveType::Int64),
        ));
        let mut child = ObjectInfo::new("child table", ObjectKind::Table);
        child.columns.clone_from(&parent.columns);
        let trees = vec![CatalogTree {
            name: "db".into(),
            schemas: vec![SchemaTree {
                name: "odd schema".into(),
                objects: vec![parent, child],
            }],
        }];
        let mut from = graph(false);
        from.data = sift_core::catalog::graph_from_trees(&trees, CatalogCoverage::complete(), "db");
        let table = |name: &str| {
            from.data
                .nodes
                .iter()
                .find(|node| node.kind == CatalogNodeKind::Table && node.name == name)
                .unwrap()
        };
        let columns = |table: &CatalogNode| {
            from.data
                .nodes
                .iter()
                .filter(|node| {
                    node.kind == CatalogNodeKind::Column
                        && node.parent_id.as_ref() == Some(&table.id)
                })
                .map(|node| node.id.clone())
                .collect::<Vec<_>>()
        };
        let child = table("child table");
        let parent = table("parent table");
        let (to, _) = sift_core::catalog::apply_diagram_mutation(
            &from,
            &sift_protocol::CatalogDiagramMutation::AddForeignKey {
                table_id: child.id.clone(),
                name: "child parent fk".into(),
                columns: columns(child),
                referenced_table_id: parent.id.clone(),
                referenced_columns: columns(parent),
            },
        )
        .unwrap();
        let diff = sift_core::schema_diff::diff_catalogs(source(), &from, source(), &to, &[], None)
            .unwrap();

        let postgres = render_plan(
            Engine::Postgres,
            &diff,
            &from,
            &to,
            &[],
            CatalogRevision(1),
            &MigrationOptions::default(),
        )
        .unwrap();
        assert_eq!(
            postgres.groups[0].statements[0].sql,
            "ALTER TABLE \"odd schema\".\"child table\" ADD CONSTRAINT \"child parent fk\" FOREIGN KEY (\"id\", \"tenant id\") REFERENCES \"odd schema\".\"parent table\" (\"id\", \"tenant id\");"
        );
        let sql_server = render_plan(
            Engine::SqlServer,
            &diff,
            &from,
            &to,
            &[],
            CatalogRevision(1),
            &MigrationOptions::default(),
        )
        .unwrap();
        assert_eq!(
            sql_server.groups[0].statements[0].sql,
            "ALTER TABLE [odd schema].[child table] ADD CONSTRAINT [child parent fk] FOREIGN KEY ([id], [tenant id]) REFERENCES [odd schema].[parent table] ([id], [tenant id]);"
        );
    }
}
