//! Derive private rename/add/drop targets from a verified native declaration.
use super::*;

fn declaration(graph: &CatalogGraph, table: &CatalogNode) -> Option<String> {
    let columns = graph
        .data
        .nodes
        .iter()
        .filter(|node| node.parent_id.as_ref() == Some(&table.id))
        .collect();
    declaration_from_columns(table, columns)
}
fn declaration_from_columns(table: &CatalogNode, mut columns: Vec<&CatalogNode>) -> Option<String> {
    columns.sort_by_key(|node| node.ordinal);
    if columns.is_empty() {
        return None;
    }
    let mut parts = Vec::with_capacity(columns.len());
    for (index, node) in columns.into_iter().enumerate() {
        if node.kind != CatalogNodeKind::Column || node.ordinal != u32::try_from(index + 1).ok() {
            return None;
        }
        let CatalogNodeDetails::Column { column } = &node.details else {
            return None;
        };
        let facets = column.facets.sqlite.as_ref()?;
        let declared = facets.declared_type.to_ascii_uppercase();
        if column.name != node.name
            || column.auto_increment
            || facets.hidden != 0
            || facets.virtual_table
            || facets.primary_key_ordinal != 0
            || facets.default_expr.is_some()
            || !["INTEGER", "REAL", "TEXT", "BLOB", "NUMERIC"].contains(&declared.as_str())
        {
            return None;
        }
        parts.push(format!(
            "{} {}{}",
            quote(&node.name),
            declared,
            if column.nullable == sift_protocol::Nullability::NotNullable {
                " NOT NULL"
            } else {
                ""
            }
        ));
    }
    Some(format!(
        "CREATE TABLE {} ({});",
        quote(&table.name),
        parts.join(", ")
    ))
}
fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

pub(super) fn derive(
    live: &CatalogGraph,
    desired: &mut CatalogGraph,
    mutation: &sift_protocol::CatalogDiagramMutation,
) -> Result<(), DiagramMutationError> {
    if let sift_protocol::CatalogDiagramMutation::DropColumn { column_id } = mutation {
        return drop_column(live, desired, column_id);
    }
    if let sift_protocol::CatalogDiagramMutation::AddColumn {
        table_id,
        name,
        type_ref,
        nullability,
    } = mutation
    {
        return add(live, desired, table_id, name, type_ref, *nullability);
    }
    let sift_protocol::CatalogDiagramMutation::RenameObject {
        object_id,
        new_name,
    } = mutation
    else {
        return Err(DiagramMutationError::UnsupportedObject);
    };
    let old = live
        .data
        .nodes
        .iter()
        .find(|node| &node.id == object_id)
        .ok_or(DiagramMutationError::UnknownObject)?;
    if !matches!(old.kind, CatalogNodeKind::Table | CatalogNodeKind::Column) {
        return Err(DiagramMutationError::UnsupportedObject);
    }
    // SQLite identifiers are ASCII case-insensitive. Table/view names share a
    // namespace, and a case-only rename may retain its own identity.
    if live.data.nodes.iter().any(|node| {
        node.id != old.id
            && node.parent_id == old.parent_id
            && node.name.eq_ignore_ascii_case(new_name)
            && (old.kind == CatalogNodeKind::Table || node.kind == old.kind)
    }) {
        return Err(DiagramMutationError::InvalidName);
    }
    let table = if old.kind == CatalogNodeKind::Table {
        old
    } else {
        live.data
            .nodes
            .iter()
            .find(|node| Some(&node.id) == old.parent_id.as_ref())
            .ok_or(DiagramMutationError::InvalidResult)?
    };
    if table.kind != CatalogNodeKind::Table
        || !live.data.nodes.iter().any(|node| {
            Some(&node.id) == table.parent_id.as_ref()
                && node.kind == CatalogNodeKind::Schema
                && node.name == "main"
        })
    {
        return Err(DiagramMutationError::UnsupportedObject);
    }
    let canonical = declaration(live, table).ok_or(DiagramMutationError::UnsupportedObject)?;
    if table
        .extra
        .get("sqlite_safe_create_sql")
        .and_then(serde_json::Value::as_str)
        != Some(canonical.as_str())
    {
        return Err(DiagramMutationError::UnsupportedObject);
    }
    // RenameObject remaps a table's id; identify its replacement by its same
    // parent and the explicit new name. Column renames keep their table id.
    let index = desired
        .data
        .nodes
        .iter()
        .position(|node| {
            if old.kind == CatalogNodeKind::Table {
                node.kind == CatalogNodeKind::Table
                    && node.parent_id == table.parent_id
                    && node.name == *new_name
            } else {
                node.id == table.id
            }
        })
        .ok_or(DiagramMutationError::InvalidResult)?;
    let canonical = declaration(desired, &desired.data.nodes[index])
        .ok_or(DiagramMutationError::UnsupportedObject)?;
    desired.data.nodes[index]
        .extra
        .insert("sqlite_safe_create_sql".into(), canonical.into());
    Ok(())
}

fn add(
    live: &CatalogGraph,
    desired: &mut CatalogGraph,
    table_id: &CatalogObjectId,
    name: &str,
    type_ref: &sift_protocol::TypeRef,
    nullability: sift_protocol::Nullability,
) -> Result<(), DiagramMutationError> {
    if nullability != sift_protocol::Nullability::Nullable || name.chars().any(char::is_control) {
        return Err(DiagramMutationError::UnsupportedObject);
    }
    let sift_protocol::TypeRef::Native {
        provider_id,
        name: declared,
        category: sift_protocol::TypeCategory::Other,
    } = type_ref
    else {
        return Err(DiagramMutationError::UnsupportedObject);
    };
    let declared = declared.to_ascii_uppercase();
    if provider_id != &sift_protocol::Engine::Sqlite.provider_id()
        || !["INTEGER", "REAL", "TEXT", "BLOB", "NUMERIC"].contains(&declared.as_str())
    {
        return Err(DiagramMutationError::UnsupportedObject);
    }
    let table = live
        .data
        .nodes
        .iter()
        .find(|node| &node.id == table_id)
        .ok_or(DiagramMutationError::UnknownObject)?;
    if table.kind != CatalogNodeKind::Table
        || !live.data.nodes.iter().any(|node| {
            Some(&node.id) == table.parent_id.as_ref()
                && node.kind == CatalogNodeKind::Schema
                && node.name == "main"
        })
    {
        return Err(DiagramMutationError::UnsupportedObject);
    }
    let original = declaration(live, table).ok_or(DiagramMutationError::UnsupportedObject)?;
    if table
        .extra
        .get("sqlite_safe_create_sql")
        .and_then(serde_json::Value::as_str)
        != Some(original.as_str())
    {
        return Err(DiagramMutationError::UnsupportedObject);
    }
    if live.data.nodes.iter().any(|node| {
        node.parent_id.as_ref() == Some(table_id) && node.name.eq_ignore_ascii_case(name)
    }) {
        return Err(DiagramMutationError::InvalidColumnName);
    }
    let new = desired
        .data
        .nodes
        .iter_mut()
        .find(|node| node.parent_id.as_ref() == Some(table_id) && node.name == name)
        .ok_or(DiagramMutationError::InvalidResult)?;
    let CatalogNodeDetails::Column { column } = &mut new.details else {
        return Err(DiagramMutationError::InvalidResult);
    };
    column.type_ref = sift_protocol::TypeRef::Native {
        provider_id: provider_id.clone(),
        name: declared.clone(),
        category: sift_protocol::TypeCategory::Other,
    };
    column.facets.sqlite = Some(Box::new(sift_protocol::SqliteColumnFacets {
        affinity: declared.to_ascii_lowercase(),
        declared_type: declared,
        default_expr: None,
        primary_key_ordinal: 0,
        virtual_table: false,
        hidden: 0,
    }));
    let index = desired
        .data
        .nodes
        .iter()
        .position(|node| &node.id == table_id)
        .ok_or(DiagramMutationError::InvalidResult)?;
    let canonical = declaration(desired, &desired.data.nodes[index])
        .ok_or(DiagramMutationError::UnsupportedObject)?;
    desired.data.nodes[index]
        .extra
        .insert("sqlite_safe_create_sql".into(), canonical.into());
    Ok(())
}

fn drop_column(
    live: &CatalogGraph,
    desired: &mut CatalogGraph,
    column_id: &CatalogObjectId,
) -> Result<(), DiagramMutationError> {
    let column = live
        .data
        .nodes
        .iter()
        .find(|node| &node.id == column_id)
        .ok_or(DiagramMutationError::UnknownObject)?;
    let table = column
        .parent_id
        .as_ref()
        .and_then(|id| live.data.nodes.iter().find(|node| &node.id == id))
        .ok_or(DiagramMutationError::UnsupportedObject)?;
    if column.kind != CatalogNodeKind::Column
        || table.kind != CatalogNodeKind::Table
        || !live.data.nodes.iter().any(|node| {
            Some(&node.id) == table.parent_id.as_ref()
                && node.kind == CatalogNodeKind::Schema
                && node.name == "main"
        })
        || live
            .data
            .nodes
            .iter()
            .filter(|node| node.parent_id.as_ref() == Some(&table.id))
            .count()
            <= 1
        || live.data.edges.iter().any(|edge| {
            (edge.from == *column_id
                || edge.to.as_ref() == Some(column_id)
                || edge
                    .column_pairs
                    .iter()
                    .any(|pair| pair.from == *column_id || pair.to == *column_id))
                && !(edge.kind == CatalogEdgeKind::Contains
                    && edge.from == table.id
                    && edge.to.as_ref() == Some(column_id))
        })
    {
        return Err(DiagramMutationError::UnsupportedObject);
    }
    let original = declaration(live, table).ok_or(DiagramMutationError::UnsupportedObject)?;
    if table
        .extra
        .get("sqlite_safe_create_sql")
        .and_then(serde_json::Value::as_str)
        != Some(original.as_str())
    {
        return Err(DiagramMutationError::UnsupportedObject);
    }
    let ordinal = column
        .ordinal
        .ok_or(DiagramMutationError::UnsupportedObject)?;
    desired.data.nodes.retain(|node| &node.id != column_id);
    desired
        .data
        .edges
        .retain(|edge| edge.from != *column_id && edge.to.as_ref() != Some(column_id));
    for node in &mut desired.data.nodes {
        if node.parent_id.as_ref() == Some(&table.id) && node.ordinal.is_some_and(|n| n > ordinal) {
            node.ordinal = node.ordinal.map(|n| n - 1);
        }
    }
    let index = desired
        .data
        .nodes
        .iter()
        .position(|node| node.id == table.id)
        .ok_or(DiagramMutationError::InvalidResult)?;
    let canonical = declaration(desired, &desired.data.nodes[index])
        .ok_or(DiagramMutationError::UnsupportedObject)?;
    desired.data.nodes[index]
        .extra
        .insert("sqlite_safe_create_sql".into(), canonical.into());
    Ok(())
}

/// Absence is definitive only within this completely observed simple shape.
pub(super) fn simple_catalog_proven(graph: &CatalogGraph) -> bool {
    let coverage = &graph.data.coverage;
    if graph.provider.provider_id != sift_protocol::Engine::Sqlite.provider_id()
        || coverage.state == sift_protocol::CatalogCoverageState::Stale
        || !coverage.requested_kinds.is_empty()
        || !coverage.omitted_schemas.is_empty()
        || coverage.truncated_at_nodes.is_some()
        || coverage.covered_schemas.len() != 2
        || !["main", "temp"]
            .iter()
            .all(|name| coverage.covered_schemas.iter().any(|schema| schema == name))
        || coverage.failures.iter().any(|failure| {
            failure.stage != "dependencies"
                || failure.schema.is_some()
                || failure.code != "sqlite_expression_parser_coverage_partial"
        })
        || graph
            .data
            .edges
            .iter()
            .any(|edge| edge.kind != CatalogEdgeKind::Contains || edge.to.is_none())
    {
        return false;
    }
    let nodes = graph
        .data
        .nodes
        .iter()
        .map(|node| (node.id.clone(), node))
        .collect::<std::collections::HashMap<_, _>>();
    let mut children: std::collections::HashMap<CatalogObjectId, Vec<&CatalogNode>> =
        std::collections::HashMap::new();
    for node in &graph.data.nodes {
        if let Some(parent) = &node.parent_id {
            children.entry(parent.clone()).or_default().push(node);
        }
    }
    graph.data.nodes.iter().all(|node| match node.kind {
        CatalogNodeKind::Catalog | CatalogNodeKind::Schema => true,
        CatalogNodeKind::Column => node
            .parent_id
            .as_ref()
            .and_then(|parent| nodes.get(parent))
            .is_some_and(|parent| parent.kind == CatalogNodeKind::Table),
        CatalogNodeKind::Table => {
            node.extra.get("migration_unsupported") != Some(&serde_json::Value::Bool(true))
                && node
                    .parent_id
                    .as_ref()
                    .and_then(|parent| nodes.get(parent))
                    .is_some_and(|parent| {
                        parent.kind == CatalogNodeKind::Schema && parent.name == "main"
                    })
                && declaration_from_columns(node, children.remove(&node.id).unwrap_or_default())
                    .is_some_and(|canonical| {
                        node.extra
                            .get("sqlite_safe_create_sql")
                            .and_then(serde_json::Value::as_str)
                            == Some(canonical.as_str())
                    })
        }
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_protocol::*;
    #[test]
    fn sqlite_removal_proves_absence_and_preserves_surviving_identity() {
        let mut table = ObjectInfo::new("sample", ObjectKind::Table);
        for name in ["first", "middle", "last"] {
            let mut column = ColumnMetadata::new(name, TypeRef::Primitive(PrimitiveType::Text));
            column.nullable = Nullability::Nullable;
            column.facets.sqlite = Some(Box::new(SqliteColumnFacets {
                declared_type: "TEXT".into(),
                affinity: "text".into(),
                default_expr: None,
                primary_key_ordinal: 0,
                virtual_table: false,
                hidden: 0,
            }));
            table.columns.push(column);
        }
        let mut data = graph_from_trees(
            &[CatalogTree {
                name: "db".into(),
                schemas: vec![
                    SchemaTree {
                        name: "main".into(),
                        objects: vec![table],
                    },
                    SchemaTree {
                        name: "temp".into(),
                        objects: vec![],
                    },
                ],
            }],
            CatalogCoverage {
                state: CatalogCoverageState::Partial,
                requested_kinds: vec![],
                covered_schemas: vec!["main".into(), "temp".into()],
                omitted_schemas: vec![],
                truncated_at_nodes: None,
                failures: vec![CatalogCoverageFailure {
                    stage: "dependencies".into(),
                    schema: None,
                    code: "sqlite_expression_parser_coverage_partial".into(),
                }],
            },
            "db",
        );
        data.nodes
            .iter_mut()
            .find(|node| node.kind == CatalogNodeKind::Table)
            .unwrap()
            .extra
            .insert(
                "sqlite_safe_create_sql".into(),
                serde_json::json!(
                    "CREATE TABLE \"sample\" (\"first\" TEXT, \"middle\" TEXT, \"last\" TEXT);"
                ),
            );
        let graph = CatalogGraph {
            revision: CatalogRevision(1),
            content_digest: "original".into(),
            invalidation_epoch: 1,
            captured_at: chrono::Utc::now(),
            provider: Engine::Sqlite.provider_ref("fixture"),
            database_identity: "db".into(),
            data,
        };
        assert!(simple_catalog_proven(&graph));
        let column = graph
            .data
            .nodes
            .iter()
            .find(|node| node.kind == CatalogNodeKind::Column && node.name == "middle")
            .unwrap();
        let mutation = CatalogDiagramMutation::DropColumn {
            column_id: column.id.clone(),
        };
        let (desired, renames) = apply_diagram_mutation(&graph, &mutation).unwrap();
        assert!(renames.is_empty());
        assert!(simple_catalog_proven(&desired));
        assert!(!desired.data.nodes.iter().any(|node| node.id == column.id));
        let last = graph
            .data
            .nodes
            .iter()
            .find(|node| node.kind == CatalogNodeKind::Column && node.name == "last")
            .unwrap();
        assert!(desired
            .data
            .nodes
            .iter()
            .any(|node| node.id == last.id && node.ordinal == Some(2)));
        assert_eq!(
            graph
                .data
                .nodes
                .iter()
                .find(|node| node.id == last.id)
                .unwrap()
                .ordinal,
            Some(3)
        );
        let source = CatalogSourceRef::Live {
            expected_revision: graph.revision,
            options: Default::default(),
        };
        let diff = crate::schema_diff::diff_catalogs(
            source.clone(),
            &graph,
            source.clone(),
            &desired,
            &[],
            None,
        )
        .unwrap();
        assert!(diff
            .changes
            .iter()
            .any(|change| change.kind == SchemaChangeKind::Drop
                && change.risk == SchemaChangeRisk::DataLoss
                && change.reversibility == SchemaChangeReversibility::Lossy));
        for mode in 0..5 {
            let mut unproven = graph.clone();
            match mode {
                0 => unproven.data.coverage.requested_kinds = vec![CatalogNodeKind::Table],
                1 => unproven.data.coverage.truncated_at_nodes = Some(1),
                2 => unproven
                    .data
                    .coverage
                    .failures
                    .push(CatalogCoverageFailure {
                        stage: "dependencies".into(),
                        schema: Some("main".into()),
                        code: "unparsed".into(),
                    }),
                3 => {
                    unproven
                        .data
                        .nodes
                        .iter_mut()
                        .find(|node| node.kind == CatalogNodeKind::Table)
                        .unwrap()
                        .extra
                        .remove("sqlite_safe_create_sql");
                }
                _ => unproven.data.coverage.state = CatalogCoverageState::Stale,
            }
            assert!(!simple_catalog_proven(&unproven));
            let diff = crate::schema_diff::diff_catalogs(
                source.clone(),
                &unproven,
                source.clone(),
                &desired,
                &[],
                None,
            )
            .unwrap();
            assert!(diff
                .changes
                .iter()
                .any(|change| change.kind == SchemaChangeKind::Unknown
                    && change
                        .object_before
                        .as_ref()
                        .is_some_and(|node| node.id == column.id)));
        }
        let mut wrong_provider = graph.clone();
        wrong_provider.provider = Engine::Postgres.provider_ref("fixture");
        assert_eq!(
            apply_diagram_mutation(&wrong_provider, &mutation).unwrap_err(),
            DiagramMutationError::UnsupportedObject
        );
        let mut bad = graph.clone();
        bad.data.edges.push(CatalogEdge {
            kind: CatalogEdgeKind::DependsOn,
            ..bad
                .data
                .edges
                .iter()
                .find(|edge| edge.to.as_ref() == Some(&column.id))
                .unwrap()
                .clone()
        });
        assert_eq!(
            apply_diagram_mutation(&bad, &mutation).unwrap_err(),
            DiagramMutationError::UnsupportedObject
        );
    }
}
