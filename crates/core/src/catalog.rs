//! Provider-neutral catalog normalization (ADR-033).

use std::collections::{BTreeMap, HashMap, HashSet};

use sha2::{Digest, Sha256};
use sift_protocol::{
    CatalogCompleteness, CatalogCoverage, CatalogDiagram, CatalogDiagramRequest, CatalogEdge,
    CatalogEdgeCertainty, CatalogEdgeKind, CatalogGraph, CatalogGraphData, CatalogGraphOptions,
    CatalogNode, CatalogNodeDetails, CatalogNodeKind, CatalogObjectId, CatalogTree, ConstraintKind,
};

/// Normalize fully populated progressive schema trees into a deterministic
/// structural graph. Engine adapters enrich the resulting graph with native
/// ids and catalog-proven dependency edges when available.
pub fn graph_from_trees(
    trees: &[CatalogTree],
    coverage: CatalogCoverage,
    identity_seed: &str,
) -> CatalogGraphData {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut object_ids = HashMap::new();

    for catalog in trees {
        let catalog_path = catalog.name.clone();
        let catalog_id = object_id(identity_seed, CatalogNodeKind::Catalog, &catalog_path);
        nodes.push(node(
            catalog_id.clone(),
            CatalogNodeKind::Catalog,
            &catalog.name,
            &catalog_path,
            None,
            CatalogNodeDetails::None,
        ));
        for schema in &catalog.schemas {
            let schema_path = format!("{}.{}", catalog.name, schema.name);
            let schema_id = object_id(identity_seed, CatalogNodeKind::Schema, &schema_path);
            nodes.push(node(
                schema_id.clone(),
                CatalogNodeKind::Schema,
                &schema.name,
                &schema_path,
                Some(catalog_id.clone()),
                CatalogNodeDetails::None,
            ));
            contain(&mut edges, &catalog_id, &schema_id);
            for object in &schema.objects {
                let signature = object
                    .routine_args
                    .as_ref()
                    .map(|arguments| format!("({})", arguments.join(",")))
                    .unwrap_or_default();
                let object_path = format!("{schema_path}.{}{signature}", object.name);
                let kind = CatalogNodeKind::from(object.kind);
                let object_node_id = object_id(identity_seed, kind, &object_path);
                if is_relation_kind(kind) {
                    object_ids.insert(
                        (
                            schema.name.to_ascii_lowercase(),
                            object.name.to_ascii_lowercase(),
                        ),
                        object_node_id.clone(),
                    );
                }
                let mut object_node = node(
                    object_node_id.clone(),
                    kind,
                    &object.name,
                    &object_path,
                    Some(schema_id.clone()),
                    CatalogNodeDetails::Object {
                        routine_args: object.routine_args.clone(),
                    },
                );
                if let Some(comment) = &object.comment {
                    object_node
                        .extra
                        .insert("comment".into(), serde_json::Value::String(comment.clone()));
                }
                if let Some(estimated_rows) = object.estimated_rows {
                    object_node.extra.insert(
                        "estimated_rows".into(),
                        serde_json::Value::Number(estimated_rows.into()),
                    );
                }
                if let Some(modified_at) = &object.modified_at {
                    object_node.extra.insert(
                        "modified_at".into(),
                        serde_json::Value::String(modified_at.clone()),
                    );
                }
                nodes.push(object_node);
                contain(&mut edges, &schema_id, &object_node_id);

                let mut column_ids = HashMap::new();
                for (ordinal, column) in object.columns.iter().enumerate() {
                    let path = format!("{object_path}.{}", column.name);
                    let id = object_id(identity_seed, CatalogNodeKind::Column, &path);
                    let mut column_node = node(
                        id.clone(),
                        CatalogNodeKind::Column,
                        &column.name,
                        &path,
                        Some(object_node_id.clone()),
                        CatalogNodeDetails::Column {
                            column: column.clone(),
                        },
                    );
                    column_node.ordinal = u32::try_from(ordinal + 1).ok();
                    nodes.push(column_node);
                    contain(&mut edges, &object_node_id, &id);
                    column_ids.insert(column.name.clone(), id);
                }
                for index in &object.indexes {
                    let path = format!("{object_path}.index.{}", index.name);
                    let id = object_id(identity_seed, CatalogNodeKind::Index, &path);
                    nodes.push(node(
                        id.clone(),
                        CatalogNodeKind::Index,
                        &index.name,
                        &path,
                        Some(object_node_id.clone()),
                        CatalogNodeDetails::Index {
                            index: index.clone(),
                        },
                    ));
                    contain(&mut edges, &object_node_id, &id);
                    edges.push(relation(&id, &object_node_id, CatalogEdgeKind::Indexes));
                    for column in &index.columns {
                        if let Some(column_id) = column_ids.get(column) {
                            edges.push(relation(&id, column_id, CatalogEdgeKind::DependsOn));
                        }
                    }
                }
                for constraint in &object.constraints {
                    let path = format!("{object_path}.constraint.{}", constraint.name);
                    let id = object_id(identity_seed, CatalogNodeKind::Constraint, &path);
                    let definition_digest = constraint.definition.as_deref().map(digest);
                    let mut constraint_node = node(
                        id.clone(),
                        CatalogNodeKind::Constraint,
                        &constraint.name,
                        &path,
                        Some(object_node_id.clone()),
                        CatalogNodeDetails::Constraint {
                            constraint: constraint.clone(),
                        },
                    );
                    constraint_node.definition_digest = definition_digest;
                    nodes.push(constraint_node);
                    contain(&mut edges, &object_node_id, &id);
                    edges.push(relation(&id, &object_node_id, CatalogEdgeKind::Constrains));
                    for column in &constraint.columns {
                        if let Some(column_id) = column_ids.get(column) {
                            edges.push(relation(&id, column_id, CatalogEdgeKind::DependsOn));
                        }
                    }
                    if constraint.kind == ConstraintKind::ForeignKey {
                        if let Some(reference) = constraint.references.as_deref() {
                            edges.push(CatalogEdge {
                                from: id,
                                to: None,
                                kind: CatalogEdgeKind::ForeignKey,
                                certainty: CatalogEdgeCertainty::Unresolved,
                                referenced_path: Some(reference.to_string()),
                                column_pairs: Vec::new(),
                            });
                        }
                    }
                }
                for trigger in &object.triggers {
                    let path = format!("{object_path}.trigger.{}", trigger.name);
                    let id = object_id(identity_seed, CatalogNodeKind::Trigger, &path);
                    let mut trigger_node = node(
                        id.clone(),
                        CatalogNodeKind::Trigger,
                        &trigger.name,
                        &path,
                        Some(object_node_id.clone()),
                        CatalogNodeDetails::Trigger {
                            trigger: trigger.clone(),
                        },
                    );
                    trigger_node.definition_digest = trigger.definition.as_deref().map(digest);
                    nodes.push(trigger_node);
                    contain(&mut edges, &object_node_id, &id);
                    edges.push(relation(&id, &object_node_id, CatalogEdgeKind::TriggerOn));
                }
            }
        }
    }

    for edge in &mut edges {
        if edge.kind != CatalogEdgeKind::ForeignKey || edge.to.is_some() {
            continue;
        }
        let Some(reference) = edge.referenced_path.as_deref() else {
            continue;
        };
        let mut parts = reference.trim_matches('"').split('.');
        let first = parts.next().unwrap_or_default().trim_matches('"');
        let second = parts.next().map(|part| part.trim_matches('"'));
        let target = match second {
            Some(name) => object_ids.get(&(first.to_ascii_lowercase(), name.to_ascii_lowercase())),
            None => {
                let wanted_name = first.to_ascii_lowercase();
                let mut matches = object_ids
                    .iter()
                    .filter(|((_, name), _)| name == &wanted_name)
                    .map(|(_, id)| id);
                let first_match = matches.next();
                if matches.next().is_some() {
                    None
                } else {
                    first_match
                }
            }
        };
        if let Some(target) = target {
            edge.to = Some(target.clone());
            edge.certainty = CatalogEdgeCertainty::CatalogProven;
            edge.referenced_path = None;
        }
    }

    let mut graph = CatalogGraphData {
        coverage,
        nodes,
        edges,
    };
    normalize_graph(&mut graph);
    graph
}

/// Canonicalize order-insensitive provider collections before validation,
/// digesting, caching, or projection. Ordered semantic collections inside a
/// node or edge (routine arguments, index columns, FK column pairs) are left
/// intact.
pub fn normalize_graph(graph: &mut CatalogGraphData) {
    graph.coverage.requested_kinds.sort_unstable();
    graph.coverage.requested_kinds.dedup();
    graph.coverage.covered_schemas.sort();
    graph.coverage.covered_schemas.dedup();
    graph.coverage.omitted_schemas.sort();
    graph.coverage.omitted_schemas.dedup();
    graph.coverage.failures.sort_by(|left, right| {
        left.stage
            .cmp(&right.stage)
            .then_with(|| left.schema.cmp(&right.schema))
            .then_with(|| left.code.cmp(&right.code))
    });
    graph.coverage.failures.dedup_by(|left, right| {
        left.stage == right.stage && left.schema == right.schema && left.code == right.code
    });
    graph.nodes.sort_by(|left, right| {
        left.qualified_name
            .cmp(&right.qualified_name)
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.id.cmp(&right.id))
            .then_with(|| canonical_json(left).cmp(&canonical_json(right)))
    });
    graph.edges.sort_by(|left, right| {
        left.from
            .cmp(&right.from)
            .then_with(|| left.to.cmp(&right.to))
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| canonical_json(left).cmp(&canonical_json(right)))
    });
}

fn canonical_json<T: serde::Serialize>(value: &T) -> Vec<u8> {
    // Protocol graph values are infallibly serializable. Keeping this helper
    // total makes normalization usable before hostile-provider validation.
    serde_json::to_vec(value).unwrap_or_default()
}

/// Apply request projection and deterministic truncation while retaining the
/// ancestors needed to interpret every selected node.
pub fn project_graph(graph: &mut CatalogGraphData, options: &CatalogGraphOptions) {
    graph.coverage.requested_kinds = options.kinds.clone().unwrap_or_default();

    let parents: HashMap<_, _> = graph
        .nodes
        .iter()
        .map(|node| (node.id.clone(), node.parent_id.clone()))
        .collect();
    let schema_allowed: HashSet<_> = graph
        .nodes
        .iter()
        .filter(|node| {
            node.kind == CatalogNodeKind::Schema
                && options.schemas.as_ref().map_or(true, |schemas| {
                    schemas.iter().any(|schema| schema == &node.name)
                })
        })
        .map(|node| node.id.clone())
        .collect();
    graph.coverage.covered_schemas = graph
        .nodes
        .iter()
        .filter(|node| schema_allowed.contains(&node.id))
        .map(|node| node.qualified_name.clone())
        .collect();

    let is_in_allowed_schema = |node: &CatalogNode| {
        if options.schemas.is_none() || node.kind == CatalogNodeKind::Catalog {
            return true;
        }
        let mut current = Some(&node.id);
        while let Some(id) = current {
            if schema_allowed.contains(id) {
                return true;
            }
            current = parents.get(id).and_then(Option::as_ref);
        }
        false
    };
    let mut retained: HashSet<CatalogObjectId> = graph
        .nodes
        .iter()
        .filter(|node| {
            is_in_allowed_schema(node)
                && options
                    .kinds
                    .as_ref()
                    .map_or(true, |kinds| kinds.contains(&node.kind))
        })
        .map(|node| node.id.clone())
        .collect();
    let selected = retained.clone();
    for id in selected {
        let mut parent = parents.get(&id).and_then(Option::as_ref);
        while let Some(id) = parent {
            retained.insert(id.clone());
            parent = parents.get(id).and_then(Option::as_ref);
        }
    }
    graph.nodes.retain(|node| retained.contains(&node.id));
    retain_valid_edges(&mut graph.edges, &retained);

    if let Some(max_nodes) = options.max_nodes.map(|value| value as usize) {
        if graph.nodes.len() > max_nodes {
            graph.nodes.truncate(max_nodes);
            let kept = graph
                .nodes
                .iter()
                .map(|node| node.id.clone())
                .collect::<HashSet<_>>();
            retain_valid_edges(&mut graph.edges, &kept);
            graph.coverage.state = sift_protocol::CatalogCoverageState::Partial;
            graph.coverage.truncated_at_nodes = u32::try_from(max_nodes).ok();
        }
    }
}

fn retain_valid_edges(edges: &mut Vec<CatalogEdge>, retained: &HashSet<CatalogObjectId>) {
    edges.retain(|edge| {
        retained.contains(&edge.from)
            && edge
                .to
                .as_ref()
                .map_or(true, |target| retained.contains(target))
            && edge
                .column_pairs
                .iter()
                .all(|pair| retained.contains(&pair.from) && retained.contains(&pair.to))
    });
}

#[derive(Debug, thiserror::Error)]
pub enum DiagramProjectionError {
    #[error("diagram selection contains an unknown or inaccessible object id")]
    UnknownObject,
    #[error("diagram request exceeds projection limits")]
    LimitExceeded,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DiagramMutationError {
    #[error("diagram mutation references an unknown or inaccessible catalog object")]
    UnknownObject,
    #[error("diagram mutation is not supported for the selected object")]
    UnsupportedObject,
    #[error("diagram mutation name is empty, oversized, or collides with a sibling")]
    InvalidName,
    #[error("foreign-key columns must be non-empty, paired, bounded, and belong to their selected tables")]
    InvalidForeignKeyColumns,
    #[error("column mutation must change type or nullability")]
    EmptyColumnChange,
    #[error("column name is empty, oversized, or collides with an existing column")]
    InvalidColumnName,
    #[error("mutated catalog graph failed structural validation")]
    InvalidResult,
}

/// Apply a bounded declarative diagram intent to a private graph clone. The
/// result is suitable only as an ephemeral diff/migration-preview target; the
/// canonical live graph is never mutated.
pub fn apply_diagram_mutation(
    catalog: &CatalogGraph,
    mutation: &sift_protocol::CatalogDiagramMutation,
) -> Result<(CatalogGraph, Vec<sift_protocol::RenameMapping>), DiagramMutationError> {
    let mut desired = catalog.clone();
    let renames = match mutation {
        sift_protocol::CatalogDiagramMutation::RenameObject {
            object_id,
            new_name,
        } => rename_catalog_object(&mut desired.data, object_id, new_name)?,
        sift_protocol::CatalogDiagramMutation::AddForeignKey {
            table_id,
            name,
            columns,
            referenced_table_id,
            referenced_columns,
        } => {
            add_catalog_foreign_key(
                &mut desired.data,
                table_id,
                name,
                columns,
                referenced_table_id,
                referenced_columns,
            )?;
            Vec::new()
        }
        sift_protocol::CatalogDiagramMutation::DropForeignKey { constraint_id } => {
            drop_catalog_foreign_key(&mut desired.data, constraint_id)?;
            Vec::new()
        }
        sift_protocol::CatalogDiagramMutation::ChangeColumn {
            column_id,
            type_ref,
            nullability,
        } => {
            change_catalog_column(
                &mut desired.data,
                column_id,
                type_ref.as_ref(),
                *nullability,
            )?;
            Vec::new()
        }
        sift_protocol::CatalogDiagramMutation::AddColumn {
            table_id,
            name,
            type_ref,
            nullability,
        } => {
            add_catalog_column(&mut desired.data, table_id, name, type_ref, *nullability)?;
            Vec::new()
        }
    };
    normalize_graph(&mut desired.data);
    validate_graph(&desired.data, 100_000, 500_000)
        .map_err(|_| DiagramMutationError::InvalidResult)?;
    let encoded =
        serde_json::to_vec(&desired.data).map_err(|_| DiagramMutationError::InvalidResult)?;
    desired.content_digest = format!("catfp:{}", digest_bytes(&encoded));
    Ok((desired, renames))
}

fn rename_catalog_object(
    graph: &mut CatalogGraphData,
    object_id: &CatalogObjectId,
    new_name: &str,
) -> Result<Vec<sift_protocol::RenameMapping>, DiagramMutationError> {
    if new_name.is_empty() || new_name.len() > 1_024 {
        return Err(DiagramMutationError::InvalidName);
    }
    let root = graph
        .nodes
        .iter()
        .find(|node| &node.id == object_id)
        .cloned()
        .ok_or(DiagramMutationError::UnknownObject)?;
    if !matches!(
        root.kind,
        CatalogNodeKind::Schema
            | CatalogNodeKind::Table
            | CatalogNodeKind::PartitionedTable
            | CatalogNodeKind::Column
    ) {
        return Err(DiagramMutationError::UnsupportedObject);
    }
    if root.name == new_name
        || graph.nodes.iter().any(|node| {
            node.id != root.id
                && node.parent_id == root.parent_id
                && node.kind == root.kind
                && node.name == new_name
        })
    {
        return Err(DiagramMutationError::InvalidName);
    }
    let parent_qualified = root
        .parent_id
        .as_ref()
        .and_then(|parent| graph.nodes.iter().find(|node| &node.id == parent))
        .map(|node| node.qualified_name.as_str())
        .ok_or(DiagramMutationError::InvalidResult)?;
    let new_root_qualified = format!("{parent_qualified}.{new_name}");
    let mut descendants = HashSet::from([root.id.clone()]);
    loop {
        let discovered = graph
            .nodes
            .iter()
            .filter(|node| {
                node.parent_id
                    .as_ref()
                    .is_some_and(|parent| descendants.contains(parent))
            })
            .map(|node| node.id.clone())
            .collect::<Vec<_>>();
        let before = descendants.len();
        descendants.extend(discovered);
        if descendants.len() == before {
            break;
        }
    }
    let mut remap = HashMap::new();
    for node in graph
        .nodes
        .iter()
        .filter(|node| descendants.contains(&node.id))
    {
        let qualified_name = if node.id == root.id {
            new_root_qualified.clone()
        } else {
            node.qualified_name
                .strip_prefix(&root.qualified_name)
                .map(|suffix| format!("{new_root_qualified}{suffix}"))
                .ok_or(DiagramMutationError::InvalidResult)?
        };
        remap.insert(
            node.id.clone(),
            CatalogObjectId(format!(
                "cat:{}",
                digest(&format!(
                    "diagram-mutation\0{}\0{qualified_name}",
                    node.id.0
                ))
            )),
        );
    }
    for node in &mut graph.nodes {
        let Some(new_id) = remap.get(&node.id).cloned() else {
            continue;
        };
        if node.id == root.id {
            node.name = new_name.into();
            node.qualified_name.clone_from(&new_root_qualified);
        } else {
            node.qualified_name = node
                .qualified_name
                .strip_prefix(&root.qualified_name)
                .map(|suffix| format!("{new_root_qualified}{suffix}"))
                .ok_or(DiagramMutationError::InvalidResult)?;
        }
        node.id = new_id;
        if let Some(parent) = node.parent_id.as_mut() {
            if let Some(new_parent) = remap.get(parent) {
                parent.clone_from(new_parent);
            }
        }
    }
    for edge in &mut graph.edges {
        if let Some(new_from) = remap.get(&edge.from) {
            edge.from.clone_from(new_from);
        }
        if let Some(target) = edge.to.as_mut() {
            if let Some(new_target) = remap.get(target) {
                target.clone_from(new_target);
            }
        }
        for pair in &mut edge.column_pairs {
            if let Some(new_from) = remap.get(&pair.from) {
                pair.from.clone_from(new_from);
            }
            if let Some(new_to) = remap.get(&pair.to) {
                pair.to.clone_from(new_to);
            }
        }
    }
    Ok(vec![sift_protocol::RenameMapping {
        from: root.id,
        to: remap
            .get(object_id)
            .cloned()
            .ok_or(DiagramMutationError::InvalidResult)?,
    }])
}

fn add_catalog_foreign_key(
    graph: &mut CatalogGraphData,
    table_id: &CatalogObjectId,
    name: &str,
    columns: &[CatalogObjectId],
    target_table_id: &CatalogObjectId,
    target_columns: &[CatalogObjectId],
) -> Result<(), DiagramMutationError> {
    if name.is_empty() || name.len() > 1_024 {
        return Err(DiagramMutationError::InvalidName);
    }
    let relation_kind = |kind| {
        matches!(
            kind,
            CatalogNodeKind::Table | CatalogNodeKind::PartitionedTable
        )
    };
    let table = graph
        .nodes
        .iter()
        .find(|node| &node.id == table_id && relation_kind(node.kind))
        .cloned()
        .ok_or(DiagramMutationError::UnknownObject)?;
    let target = graph
        .nodes
        .iter()
        .find(|node| &node.id == target_table_id && relation_kind(node.kind))
        .cloned()
        .ok_or(DiagramMutationError::UnknownObject)?;
    if graph.nodes.iter().any(|node| {
        node.parent_id.as_ref() == Some(table_id)
            && node.kind == CatalogNodeKind::Constraint
            && node.name == name
    }) {
        return Err(DiagramMutationError::InvalidName);
    }
    if columns.is_empty()
        || columns.len() > 16
        || columns.len() != target_columns.len()
        || columns.iter().collect::<HashSet<_>>().len() != columns.len()
        || target_columns.iter().collect::<HashSet<_>>().len() != target_columns.len()
    {
        return Err(DiagramMutationError::InvalidForeignKeyColumns);
    }
    let column_node = |id: &CatalogObjectId, parent: &CatalogObjectId| {
        graph.nodes.iter().find(|node| {
            &node.id == id
                && node.kind == CatalogNodeKind::Column
                && node.parent_id.as_ref() == Some(parent)
        })
    };
    let source_names = columns
        .iter()
        .map(|id| column_node(id, table_id).map(|node| node.name.clone()))
        .collect::<Option<Vec<_>>>()
        .ok_or(DiagramMutationError::InvalidForeignKeyColumns)?;
    if target_columns
        .iter()
        .any(|id| column_node(id, target_table_id).is_none())
    {
        return Err(DiagramMutationError::InvalidForeignKeyColumns);
    }
    let qualified_name = format!("{}.constraint.{name}", table.qualified_name);
    let id = CatalogObjectId(format!(
        "cat:{}",
        digest(&format!("diagram-foreign-key\0{}\0{name}", table.id.0))
    ));
    graph.nodes.push(CatalogNode {
        id: id.clone(),
        native_id: None,
        kind: CatalogNodeKind::Constraint,
        name: name.into(),
        qualified_name,
        parent_id: Some(table.id.clone()),
        ordinal: None,
        definition_digest: None,
        completeness: CatalogCompleteness::Complete,
        details: CatalogNodeDetails::Constraint {
            constraint: sift_protocol::ConstraintInfo {
                name: name.into(),
                kind: ConstraintKind::ForeignKey,
                columns: source_names,
                definition: None,
                references: Some(target.qualified_name.clone()),
            },
        },
        extra: BTreeMap::new(),
    });
    contain(&mut graph.edges, &table.id, &id);
    graph
        .edges
        .push(relation(&id, &table.id, CatalogEdgeKind::Constrains));
    for column in columns {
        graph
            .edges
            .push(relation(&id, column, CatalogEdgeKind::DependsOn));
    }
    graph.edges.push(CatalogEdge {
        from: id,
        to: Some(target.id),
        kind: CatalogEdgeKind::ForeignKey,
        certainty: CatalogEdgeCertainty::CatalogProven,
        referenced_path: None,
        column_pairs: columns
            .iter()
            .cloned()
            .zip(target_columns.iter().cloned())
            .map(|(from, to)| sift_protocol::CatalogColumnPair { from, to })
            .collect(),
    });
    Ok(())
}

fn drop_catalog_foreign_key(
    graph: &mut CatalogGraphData,
    constraint_id: &CatalogObjectId,
) -> Result<(), DiagramMutationError> {
    let constraint = graph
        .nodes
        .iter()
        .find(|node| &node.id == constraint_id)
        .ok_or(DiagramMutationError::UnknownObject)?;
    if !matches!(
        &constraint.details,
        CatalogNodeDetails::Constraint { constraint }
            if constraint.kind == ConstraintKind::ForeignKey
    ) {
        return Err(DiagramMutationError::UnsupportedObject);
    }
    graph.nodes.retain(|node| &node.id != constraint_id);
    graph.edges.retain(|edge| {
        &edge.from != constraint_id
            && edge.to.as_ref() != Some(constraint_id)
            && edge
                .column_pairs
                .iter()
                .all(|pair| &pair.from != constraint_id && &pair.to != constraint_id)
    });
    Ok(())
}

fn change_catalog_column(
    graph: &mut CatalogGraphData,
    column_id: &CatalogObjectId,
    type_ref: Option<&sift_protocol::TypeRef>,
    nullability: Option<sift_protocol::Nullability>,
) -> Result<(), DiagramMutationError> {
    if type_ref.is_none() && nullability.is_none() {
        return Err(DiagramMutationError::EmptyColumnChange);
    }
    let node = graph
        .nodes
        .iter_mut()
        .find(|node| &node.id == column_id)
        .ok_or(DiagramMutationError::UnknownObject)?;
    let CatalogNodeDetails::Column { column } = &mut node.details else {
        return Err(DiagramMutationError::UnsupportedObject);
    };
    if let Some(type_ref) = type_ref {
        column.type_ref.clone_from(type_ref);
    }
    if let Some(nullability) = nullability {
        column.nullable = nullability;
    }
    Ok(())
}

fn add_catalog_column(
    graph: &mut CatalogGraphData,
    table_id: &CatalogObjectId,
    name: &str,
    type_ref: &sift_protocol::TypeRef,
    nullability: sift_protocol::Nullability,
) -> Result<(), DiagramMutationError> {
    if name.is_empty() || name.len() > 1_024 {
        return Err(DiagramMutationError::InvalidColumnName);
    }
    let table = graph
        .nodes
        .iter()
        .find(|node| {
            &node.id == table_id
                && matches!(
                    node.kind,
                    CatalogNodeKind::Table | CatalogNodeKind::PartitionedTable
                )
        })
        .cloned()
        .ok_or(DiagramMutationError::UnknownObject)?;
    if graph.nodes.iter().any(|node| {
        node.parent_id.as_ref() == Some(table_id)
            && node.kind == CatalogNodeKind::Column
            && node.name == name
    }) {
        return Err(DiagramMutationError::InvalidColumnName);
    }
    let ordinal = graph
        .nodes
        .iter()
        .filter(|node| {
            node.parent_id.as_ref() == Some(table_id) && node.kind == CatalogNodeKind::Column
        })
        .filter_map(|node| node.ordinal)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    let qualified_name = format!("{}.{name}", table.qualified_name);
    let id = CatalogObjectId(format!(
        "cat:{}",
        digest(&format!("table-designer-column\0{}\0{name}", table.id.0))
    ));
    graph.nodes.push(CatalogNode {
        id: id.clone(),
        native_id: None,
        kind: CatalogNodeKind::Column,
        name: name.into(),
        qualified_name,
        parent_id: Some(table.id.clone()),
        ordinal: Some(ordinal),
        definition_digest: None,
        completeness: CatalogCompleteness::Complete,
        details: CatalogNodeDetails::Column {
            column: sift_protocol::ColumnMetadata {
                name: name.into(),
                type_ref: type_ref.clone(),
                nullable: nullability,
                auto_increment: false,
                primary_key: false,
                facets: sift_protocol::EngineColumnFacets::default(),
            },
        },
        extra: BTreeMap::new(),
    });
    contain(&mut graph.edges, &table.id, &id);
    Ok(())
}

fn digest_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Deterministically project a catalog revision for client-owned layout.
pub fn project_diagram(
    catalog: &CatalogGraph,
    request: &CatalogDiagramRequest,
    hard_max_nodes: usize,
) -> Result<CatalogDiagram, DiagramProjectionError> {
    let requested_limit = request.max_nodes.unwrap_or(10_000) as usize;
    if requested_limit == 0 || requested_limit > hard_max_nodes {
        return Err(DiagramProjectionError::LimitExceeded);
    }
    let nodes_by_id = catalog
        .data
        .nodes
        .iter()
        .map(|node| (node.id.clone(), node))
        .collect::<HashMap<_, _>>();
    if request
        .object_ids
        .iter()
        .any(|object| !nodes_by_id.contains_key(object))
    {
        return Err(DiagramProjectionError::UnknownObject);
    }
    let object_owner = |id: &CatalogObjectId| {
        let mut current = id;
        while let Some(node) = nodes_by_id.get(current) {
            if node
                .parent_id
                .as_ref()
                .and_then(|parent| nodes_by_id.get(parent))
                .is_some_and(|parent| parent.kind == CatalogNodeKind::Schema)
            {
                return Some(current.clone());
            }
            current = node.parent_id.as_ref()?;
        }
        None
    };
    let is_routine = |kind| {
        matches!(
            kind,
            CatalogNodeKind::TableValuedFunction
                | CatalogNodeKind::ScalarFunction
                | CatalogNodeKind::Procedure
        )
    };
    let selected_edge_kinds = if request.edge_kinds.is_empty() {
        [
            CatalogEdgeKind::ForeignKey,
            CatalogEdgeKind::DependsOn,
            CatalogEdgeKind::ReadsFrom,
            CatalogEdgeKind::WritesTo,
            CatalogEdgeKind::Calls,
            CatalogEdgeKind::UsesType,
            CatalogEdgeKind::TriggerOn,
        ]
        .into_iter()
        .collect::<HashSet<_>>()
    } else {
        request.edge_kinds.iter().copied().collect()
    };
    let requested_schemas = request.schemas.iter().collect::<HashSet<_>>();
    let mut selected_objects = if request.object_ids.is_empty() {
        catalog
            .data
            .nodes
            .iter()
            .filter(|node| {
                node.parent_id
                    .as_ref()
                    .and_then(|parent| nodes_by_id.get(parent))
                    .is_some_and(|parent| {
                        parent.kind == CatalogNodeKind::Schema
                            && (requested_schemas.is_empty()
                                || requested_schemas.contains(&parent.name))
                    })
                    && (request.include_routines || !is_routine(node.kind))
            })
            .map(|node| node.id.clone())
            .collect::<HashSet<_>>()
    } else {
        request
            .object_ids
            .iter()
            .filter_map(object_owner)
            .collect::<HashSet<_>>()
    };

    for _ in 0..request.neighborhood_depth {
        let mut discovered = Vec::new();
        for edge in &catalog.data.edges {
            if !selected_edge_kinds.contains(&edge.kind) {
                continue;
            }
            let Some(from) = object_owner(&edge.from) else {
                continue;
            };
            let to = edge.to.as_ref().and_then(object_owner);
            if selected_objects.contains(&from) {
                if let Some(to) = to.as_ref() {
                    discovered.push(to.clone());
                }
            } else if to.as_ref().is_some_and(|to| selected_objects.contains(to)) {
                discovered.push(from);
            }
        }
        if discovered.is_empty() {
            break;
        }
        selected_objects.extend(discovered);
    }

    let mut retained = selected_objects.clone();
    let mut selected_edges = Vec::new();
    for edge in &catalog.data.edges {
        if !selected_edge_kinds.contains(&edge.kind) {
            continue;
        }
        let Some(from_owner) = object_owner(&edge.from) else {
            continue;
        };
        let to_owner = edge.to.as_ref().and_then(object_owner);
        if selected_objects.contains(&from_owner)
            && to_owner
                .as_ref()
                .map_or(true, |owner| selected_objects.contains(owner))
        {
            retained.insert(edge.from.clone());
            if let Some(to) = edge.to.as_ref() {
                retained.insert(to.clone());
            }
            for pair in &edge.column_pairs {
                retained.insert(pair.from.clone());
                retained.insert(pair.to.clone());
            }
            selected_edges.push(edge.clone());
        }
    }
    if request.include_columns {
        retained.extend(
            catalog
                .data
                .nodes
                .iter()
                .filter(|node| {
                    node.kind == CatalogNodeKind::Column
                        && node
                            .parent_id
                            .as_ref()
                            .is_some_and(|parent| selected_objects.contains(parent))
                })
                .map(|node| node.id.clone()),
        );
    }
    let descendants = retained.clone();
    for id in descendants {
        let mut parent = nodes_by_id
            .get(&id)
            .and_then(|node| node.parent_id.as_ref());
        while let Some(id) = parent {
            retained.insert(id.clone());
            parent = nodes_by_id.get(id).and_then(|node| node.parent_id.as_ref());
        }
    }

    let total_nodes = retained.len();
    let mut nodes = catalog
        .data
        .nodes
        .iter()
        .filter(|node| retained.contains(&node.id))
        .cloned()
        .collect::<Vec<_>>();
    nodes.truncate(requested_limit);
    let kept = nodes
        .iter()
        .map(|node| node.id.clone())
        .collect::<HashSet<_>>();
    let total_edges = selected_edges.len();
    selected_edges.retain(|edge| {
        kept.contains(&edge.from)
            && edge
                .to
                .as_ref()
                .map_or(true, |target| kept.contains(target))
            && edge
                .column_pairs
                .iter()
                .all(|pair| kept.contains(&pair.from) && kept.contains(&pair.to))
    });
    let omitted_nodes = total_nodes.saturating_sub(nodes.len());
    let omitted_edges = total_edges.saturating_sub(selected_edges.len());
    let inaccessible_boundaries = selected_edges
        .iter()
        .filter(|edge| edge.certainty == CatalogEdgeCertainty::Inaccessible)
        .count();
    Ok(CatalogDiagram {
        catalog_revision: catalog.revision,
        catalog_digest: catalog.content_digest.clone(),
        nodes,
        edges: selected_edges,
        omitted_nodes: u32::try_from(omitted_nodes).unwrap_or(u32::MAX),
        omitted_edges: u32::try_from(omitted_edges).unwrap_or(u32::MAX),
        inaccessible_boundaries: u32::try_from(inaccessible_boundaries).unwrap_or(u32::MAX),
        partial: catalog.data.coverage.state != sift_protocol::CatalogCoverageState::Complete
            || omitted_nodes > 0
            || omitted_edges > 0,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum GraphValidationError {
    #[error("catalog graph exceeds node or edge limits")]
    LimitExceeded,
    #[error("catalog graph contains a duplicate object id")]
    DuplicateObjectId,
    #[error("catalog graph contains a dangling parent or edge")]
    DanglingReference,
    #[error("catalog graph contains an invalid bounded string")]
    InvalidString,
    #[error("catalog graph contains invalid node or edge structure")]
    InvalidStructure,
    #[error("catalog graph contains a containment cycle")]
    ContainmentCycle,
}

/// Validate untrusted provider graph output before it enters caches or public
/// responses.
pub fn validate_graph(
    graph: &CatalogGraphData,
    max_nodes: usize,
    max_edges: usize,
) -> Result<(), GraphValidationError> {
    const MAX_DETAIL_ITEMS: usize = 4_096;
    const MAX_DETAIL_STRING: usize = 4_096;
    const MAX_DEFINITION_BYTES: usize = 1024 * 1024;
    const MAX_TOTAL_DEFINITION_BYTES: usize = 16 * 1024 * 1024;
    if graph.nodes.len() > max_nodes || graph.edges.len() > max_edges {
        return Err(GraphValidationError::LimitExceeded);
    }
    if graph.coverage.requested_kinds.len() > 32
        || graph.coverage.covered_schemas.len() > max_nodes
        || graph.coverage.omitted_schemas.len() > max_nodes
        || graph.coverage.failures.len() > MAX_DETAIL_ITEMS
        || graph
            .coverage
            .covered_schemas
            .iter()
            .chain(&graph.coverage.omitted_schemas)
            .any(|value| value.is_empty() || value.len() > MAX_DETAIL_STRING)
        || graph.coverage.failures.iter().any(|failure| {
            failure.stage.is_empty()
                || failure.stage.len() > 128
                || failure.code.is_empty()
                || failure.code.len() > 128
                || failure
                    .schema
                    .as_ref()
                    .is_some_and(|schema| schema.is_empty() || schema.len() > MAX_DETAIL_STRING)
        })
    {
        return Err(GraphValidationError::LimitExceeded);
    }
    let mut ids = HashSet::with_capacity(graph.nodes.len());
    let mut nodes_by_id = HashMap::with_capacity(graph.nodes.len());
    let mut ordinals = HashSet::new();
    let mut definition_bytes = 0usize;
    for node in &graph.nodes {
        if node.id.0.is_empty()
            || node.id.0.len() > 128
            || node.name.is_empty()
            || node.name.len() > 1024
            || node.qualified_name.is_empty()
            || node.qualified_name.len() > 4096
            || node.native_id.as_ref().is_some_and(|id| id.len() > 256)
            || node.definition_digest.as_ref().is_some_and(|digest| {
                digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
            || node.extra.len() > 64
            || node.extra.iter().any(|(key, value)| {
                key.is_empty()
                    || key.len() > 128
                    || serde_json::to_vec(value).map_or(true, |encoded| encoded.len() > 16_384)
            })
        {
            return Err(GraphValidationError::InvalidString);
        }
        validate_node_details(
            &node.details,
            &mut definition_bytes,
            MAX_DETAIL_ITEMS,
            MAX_DETAIL_STRING,
            MAX_DEFINITION_BYTES,
            MAX_TOTAL_DEFINITION_BYTES,
        )?;
        if !ids.insert(node.id.clone()) {
            return Err(GraphValidationError::DuplicateObjectId);
        }
        if node.kind == CatalogNodeKind::Catalog {
            if node.parent_id.is_some() || node.ordinal.is_some() {
                return Err(GraphValidationError::InvalidStructure);
            }
        } else if node.parent_id.is_none() {
            return Err(GraphValidationError::InvalidStructure);
        }
        match (&node.kind, &node.details) {
            (CatalogNodeKind::Column, CatalogNodeDetails::Column { .. })
            | (CatalogNodeKind::Index, CatalogNodeDetails::Index { .. })
            | (CatalogNodeKind::Constraint, CatalogNodeDetails::Constraint { .. })
            | (CatalogNodeKind::Trigger, CatalogNodeDetails::Trigger { .. }) => {}
            (
                CatalogNodeKind::Catalog
                | CatalogNodeKind::Schema
                | CatalogNodeKind::Table
                | CatalogNodeKind::View
                | CatalogNodeKind::MaterializedView
                | CatalogNodeKind::ForeignTable
                | CatalogNodeKind::PartitionedTable
                | CatalogNodeKind::TableValuedFunction
                | CatalogNodeKind::ScalarFunction
                | CatalogNodeKind::Procedure
                | CatalogNodeKind::Synonym
                | CatalogNodeKind::Sequence
                | CatalogNodeKind::Type
                | CatalogNodeKind::Extension,
                CatalogNodeDetails::None
                | CatalogNodeDetails::Object { .. }
                | CatalogNodeDetails::Routine { .. }
                | CatalogNodeDetails::Type { .. },
            ) => {}
            _ => return Err(GraphValidationError::InvalidStructure),
        }
        if node.kind == CatalogNodeKind::Column {
            let Some(ordinal) = node.ordinal.filter(|ordinal| *ordinal > 0) else {
                return Err(GraphValidationError::InvalidStructure);
            };
            if !ordinals.insert((node.parent_id.clone(), ordinal)) {
                return Err(GraphValidationError::InvalidStructure);
            }
        } else if node.ordinal.is_some() {
            return Err(GraphValidationError::InvalidStructure);
        }
        nodes_by_id.insert(node.id.clone(), node);
    }
    for node in &graph.nodes {
        if node
            .parent_id
            .as_ref()
            .is_some_and(|parent| !ids.contains(parent))
        {
            return Err(GraphValidationError::DanglingReference);
        }
        let mut visited = HashSet::new();
        let mut current = node.parent_id.as_ref();
        while let Some(parent) = current {
            if !visited.insert(parent) {
                return Err(GraphValidationError::ContainmentCycle);
            }
            current = nodes_by_id
                .get(parent)
                .and_then(|node| node.parent_id.as_ref());
        }
    }
    let containment = graph
        .edges
        .iter()
        .filter_map(|edge| {
            (edge.kind == CatalogEdgeKind::Contains)
                .then(|| {
                    edge.to
                        .as_ref()
                        .map(|target| (edge.from.clone(), target.clone()))
                })
                .flatten()
        })
        .collect::<HashSet<_>>();
    for edge in &graph.edges {
        if !ids.contains(&edge.from)
            || edge.to.as_ref().is_some_and(|target| !ids.contains(target))
            || edge
                .column_pairs
                .iter()
                .any(|pair| !ids.contains(&pair.from) || !ids.contains(&pair.to))
        {
            return Err(GraphValidationError::DanglingReference);
        }
        match edge.certainty {
            CatalogEdgeCertainty::CatalogProven | CatalogEdgeCertainty::Parsed
                if edge.to.is_none() || edge.referenced_path.is_some() =>
            {
                return Err(GraphValidationError::InvalidStructure);
            }
            CatalogEdgeCertainty::Unresolved
                if edge.to.is_some()
                    || edge.referenced_path.is_none()
                    || !edge.column_pairs.is_empty() =>
            {
                return Err(GraphValidationError::InvalidStructure);
            }
            CatalogEdgeCertainty::Inaccessible
                if edge.to.is_some()
                    || edge.referenced_path.is_some()
                    || !edge.column_pairs.is_empty() =>
            {
                return Err(GraphValidationError::InvalidStructure);
            }
            _ => {}
        }
        if edge.kind == CatalogEdgeKind::Contains
            && (edge.certainty != CatalogEdgeCertainty::CatalogProven
                || edge.to.as_ref().is_some_and(|target| {
                    nodes_by_id
                        .get(target)
                        .and_then(|node| node.parent_id.as_ref())
                        != Some(&edge.from)
                }))
            && edge.certainty != CatalogEdgeCertainty::Inaccessible
        {
            return Err(GraphValidationError::InvalidStructure);
        }
        if !edge.column_pairs.is_empty()
            && (edge.column_pairs.len() > MAX_DETAIL_ITEMS
                || edge.kind != CatalogEdgeKind::ForeignKey
                || edge.column_pairs.iter().any(|pair| {
                    nodes_by_id.get(&pair.from).map(|node| node.kind)
                        != Some(CatalogNodeKind::Column)
                        || nodes_by_id.get(&pair.to).map(|node| node.kind)
                            != Some(CatalogNodeKind::Column)
                }))
        {
            return Err(GraphValidationError::InvalidStructure);
        }
        if edge.kind == CatalogEdgeKind::ForeignKey
            && edge.certainty != CatalogEdgeCertainty::Inaccessible
        {
            let source_relation = match nodes_by_id.get(&edge.from) {
                Some(CatalogNode {
                    parent_id: Some(parent),
                    details: CatalogNodeDetails::Constraint { constraint },
                    ..
                }) if constraint.kind == sift_protocol::ConstraintKind::ForeignKey => parent,
                _ => return Err(GraphValidationError::InvalidStructure),
            };
            if let Some(target_relation) = edge.to.as_ref() {
                if !nodes_by_id
                    .get(target_relation)
                    .is_some_and(|node| is_relation_kind(node.kind))
                    || edge.column_pairs.iter().any(|pair| {
                        nodes_by_id
                            .get(&pair.from)
                            .and_then(|node| node.parent_id.as_ref())
                            != Some(source_relation)
                            || nodes_by_id
                                .get(&pair.to)
                                .and_then(|node| node.parent_id.as_ref())
                                != Some(target_relation)
                    })
                {
                    return Err(GraphValidationError::InvalidStructure);
                }
            }
        }
        if edge
            .referenced_path
            .as_ref()
            .is_some_and(|path| path.is_empty() || path.len() > 4096)
        {
            return Err(GraphValidationError::InvalidString);
        }
    }
    for node in &graph.nodes {
        if let Some(parent) = node.parent_id.as_ref() {
            if !containment.contains(&(parent.clone(), node.id.clone())) {
                return Err(GraphValidationError::InvalidStructure);
            }
        }
    }
    Ok(())
}

fn validate_node_details(
    details: &CatalogNodeDetails,
    total_definition_bytes: &mut usize,
    max_items: usize,
    max_string: usize,
    max_definition: usize,
    max_total_definitions: usize,
) -> Result<(), GraphValidationError> {
    let strings_valid = |values: &[String]| {
        values.len() <= max_items
            && values
                .iter()
                .all(|value| !value.is_empty() && value.len() <= max_string)
    };
    let mut definition = |value: Option<&String>| {
        let Some(value) = value else {
            return true;
        };
        if value.len() > max_definition {
            return false;
        }
        let Some(next) = total_definition_bytes.checked_add(value.len()) else {
            return false;
        };
        if next > max_total_definitions {
            return false;
        }
        *total_definition_bytes = next;
        true
    };
    let type_valid = |value: &sift_protocol::TypeRef| match value {
        sift_protocol::TypeRef::Primitive(_) => true,
        sift_protocol::TypeRef::Native { name, .. } => !name.is_empty() && name.len() <= max_string,
    };

    let valid = match details {
        CatalogNodeDetails::None => true,
        CatalogNodeDetails::Object { routine_args } => routine_args
            .as_ref()
            .map_or(true, |args| strings_valid(args)),
        CatalogNodeDetails::Column { column } => {
            !column.name.is_empty()
                && column.name.len() <= max_string
                && type_valid(&column.type_ref)
                && match (&column.facets.postgres, &column.facets.sql_server) {
                    (Some(_), Some(_)) => false,
                    (postgres, sql_server) => {
                        postgres.as_ref().map_or(true, |facets| {
                            definition(facets.default_expr.as_ref())
                                && facets
                                    .enum_values
                                    .as_ref()
                                    .map_or(true, |values| strings_valid(values))
                        }) && sql_server.as_ref().map_or(true, |facets| {
                            facets
                                .tds_type
                                .as_ref()
                                .map_or(true, |value| value.len() <= max_string)
                                && facets
                                    .collation
                                    .as_ref()
                                    .map_or(true, |value| value.len() <= max_string)
                                && definition(facets.default_expr.as_ref())
                        })
                    }
                }
        }
        CatalogNodeDetails::Index { index } => {
            !index.name.is_empty()
                && index.name.len() <= max_string
                && strings_valid(&index.columns)
                && definition(index.partial_predicate.as_ref())
        }
        CatalogNodeDetails::Constraint { constraint } => {
            !constraint.name.is_empty()
                && constraint.name.len() <= max_string
                && strings_valid(&constraint.columns)
                && definition(constraint.definition.as_ref())
                && constraint
                    .references
                    .as_ref()
                    .map_or(true, |value| !value.is_empty() && value.len() <= max_string)
        }
        CatalogNodeDetails::Trigger { trigger } => {
            !trigger.name.is_empty()
                && trigger.name.len() <= max_string
                && trigger.events.len() <= 4
                && strings_valid(&trigger.columns)
                && definition(trigger.definition.as_ref())
        }
        CatalogNodeDetails::Routine {
            arguments,
            return_type,
        } => strings_valid(arguments) && return_type.as_ref().map_or(true, type_valid),
        CatalogNodeDetails::Type { base_type } => base_type.as_ref().map_or(true, type_valid),
    };
    if valid {
        Ok(())
    } else {
        Err(GraphValidationError::LimitExceeded)
    }
}

fn node(
    id: CatalogObjectId,
    kind: CatalogNodeKind,
    name: &str,
    qualified_name: &str,
    parent_id: Option<CatalogObjectId>,
    details: CatalogNodeDetails,
) -> CatalogNode {
    CatalogNode {
        id,
        native_id: None,
        kind,
        name: name.to_string(),
        qualified_name: qualified_name.to_string(),
        parent_id,
        ordinal: None,
        definition_digest: None,
        completeness: CatalogCompleteness::Complete,
        details,
        extra: BTreeMap::new(),
    }
}

fn contain(edges: &mut Vec<CatalogEdge>, parent: &CatalogObjectId, child: &CatalogObjectId) {
    edges.push(relation(parent, child, CatalogEdgeKind::Contains));
}

fn relation(from: &CatalogObjectId, to: &CatalogObjectId, kind: CatalogEdgeKind) -> CatalogEdge {
    CatalogEdge {
        from: from.clone(),
        to: Some(to.clone()),
        kind,
        certainty: CatalogEdgeCertainty::CatalogProven,
        referenced_path: None,
        column_pairs: Vec::new(),
    }
}

fn is_relation_kind(kind: CatalogNodeKind) -> bool {
    matches!(
        kind,
        CatalogNodeKind::Table
            | CatalogNodeKind::View
            | CatalogNodeKind::MaterializedView
            | CatalogNodeKind::ForeignTable
            | CatalogNodeKind::PartitionedTable
    )
}

fn object_id(identity_seed: &str, kind: CatalogNodeKind, path: &str) -> CatalogObjectId {
    CatalogObjectId(format!(
        "cat:{}",
        digest(&format!("{identity_seed}\0{kind:?}\0{path}"))
    ))
}

fn digest(value: &str) -> String {
    let bytes = Sha256::digest(value.as_bytes());
    let mut output = String::with_capacity(64);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

#[cfg(test)]
mod tests {
    use sift_protocol::{
        CatalogTree, ColumnMetadata, ConstraintInfo, ConstraintKind, ObjectInfo, ObjectKind,
        PrimitiveType, SchemaTree, TypeRef,
    };

    use super::*;

    #[test]
    fn graph_is_deterministic_under_tree_ordering() {
        let users = ObjectInfo::new("users", ObjectKind::Table);
        let orders = ObjectInfo::new("orders", ObjectKind::Table);
        let left = vec![CatalogTree {
            name: "db".into(),
            schemas: vec![SchemaTree {
                name: "public".into(),
                objects: vec![users.clone(), orders.clone()],
            }],
        }];
        let right = vec![CatalogTree {
            name: "db".into(),
            schemas: vec![SchemaTree {
                name: "public".into(),
                objects: vec![orders, users],
            }],
        }];
        let left = graph_from_trees(&left, CatalogCoverage::complete(), "postgres:db");
        let right = graph_from_trees(&right, CatalogCoverage::complete(), "postgres:db");
        assert_eq!(
            left.nodes.iter().map(|node| &node.id).collect::<Vec<_>>(),
            right.nodes.iter().map(|node| &node.id).collect::<Vec<_>>()
        );
        assert_eq!(left.edges, right.edges);
    }

    #[test]
    fn normalization_removes_provider_and_coverage_row_order() {
        let trees = vec![CatalogTree {
            name: "db".into(),
            schemas: vec![SchemaTree {
                name: "public".into(),
                objects: vec![
                    ObjectInfo::new("users", ObjectKind::Table),
                    ObjectInfo::new("orders", ObjectKind::Table),
                ],
            }],
        }];
        let mut expected = graph_from_trees(&trees, CatalogCoverage::complete(), "provider:db");
        expected.coverage.covered_schemas = vec!["public".into(), "audit".into()];
        normalize_graph(&mut expected);

        let mut actual = expected.clone();
        actual.nodes.reverse();
        actual.edges.reverse();
        actual.coverage.covered_schemas.reverse();
        normalize_graph(&mut actual);

        assert_eq!(
            serde_json::to_value(actual).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
    }

    #[test]
    fn projection_retains_requested_schema_and_ancestors() {
        let trees = vec![CatalogTree {
            name: "db".into(),
            schemas: vec![SchemaTree {
                name: "wanted".into(),
                objects: vec![ObjectInfo::new("users", ObjectKind::Table)],
            }],
        }];
        let mut graph = graph_from_trees(&trees, CatalogCoverage::complete(), "postgres:db");
        project_graph(
            &mut graph,
            &CatalogGraphOptions {
                schemas: Some(vec!["wanted".into()]),
                ..CatalogGraphOptions::default()
            },
        );
        assert_eq!(graph.nodes.len(), 3);
        assert!(graph.nodes.iter().any(|node| node.name == "users"));
    }

    #[test]
    fn routine_overload_name_cannot_capture_foreign_key_target_resolution() {
        let mut child = ObjectInfo::new("child", ObjectKind::Table);
        child.constraints.push(ConstraintInfo {
            name: "child_fk".into(),
            kind: ConstraintKind::ForeignKey,
            columns: Vec::new(),
            definition: None,
            references: Some("public.target".into()),
        });
        let mut routine = ObjectInfo::new("target", ObjectKind::ScalarFunction);
        routine.routine_args = Some(vec!["integer".into()]);
        let graph = graph_from_trees(
            &[CatalogTree {
                name: "db".into(),
                schemas: vec![SchemaTree {
                    name: "public".into(),
                    objects: vec![ObjectInfo::new("target", ObjectKind::Table), routine, child],
                }],
            }],
            CatalogCoverage::complete(),
            "postgres:db",
        );
        let target = graph
            .nodes
            .iter()
            .find(|node| node.kind == CatalogNodeKind::Table && node.name == "target")
            .unwrap();
        let edge = graph
            .edges
            .iter()
            .find(|edge| edge.kind == CatalogEdgeKind::ForeignKey)
            .unwrap();
        assert_eq!(edge.to.as_ref(), Some(&target.id));
    }

    #[test]
    fn hostile_nested_definition_is_rejected_before_publication() {
        let mut table = ObjectInfo::new("events", ObjectKind::Table);
        table.constraints.push(ConstraintInfo {
            name: "events_check".into(),
            kind: ConstraintKind::Check,
            columns: Vec::new(),
            definition: Some("x".repeat(1024 * 1024 + 1)),
            references: None,
        });
        let graph = graph_from_trees(
            &[CatalogTree {
                name: "db".into(),
                schemas: vec![SchemaTree {
                    name: "public".into(),
                    objects: vec![table],
                }],
            }],
            CatalogCoverage::complete(),
            "provider:db",
        );
        assert!(matches!(
            validate_graph(&graph, 100, 1_000),
            Err(GraphValidationError::LimitExceeded)
        ));
    }

    #[test]
    fn contradictory_edge_certainty_is_rejected_before_publication() {
        let mut graph = graph_from_trees(
            &[CatalogTree {
                name: "db".into(),
                schemas: vec![SchemaTree {
                    name: "public".into(),
                    objects: vec![ObjectInfo::new("events", ObjectKind::Table)],
                }],
            }],
            CatalogCoverage::complete(),
            "provider:db",
        );
        let proven = graph.edges.first_mut().expect("fixture has containment");
        proven.referenced_path = Some("public.hidden".into());
        assert!(matches!(
            validate_graph(&graph, 100, 1_000),
            Err(GraphValidationError::InvalidStructure)
        ));

        let unresolved = graph.edges.first_mut().unwrap();
        unresolved.certainty = CatalogEdgeCertainty::Unresolved;
        unresolved.to = None;
        unresolved.referenced_path = None;
        assert!(matches!(
            validate_graph(&graph, 100, 1_000),
            Err(GraphValidationError::InvalidStructure)
        ));
    }

    #[test]
    fn diagram_foreign_key_intent_preserves_ordered_column_pairs() {
        let mut parent = ObjectInfo::new("parents", ObjectKind::Table);
        parent.columns.push(ColumnMetadata::new(
            "tenant_id",
            TypeRef::Primitive(PrimitiveType::Int64),
        ));
        parent.columns.push(ColumnMetadata::new(
            "id",
            TypeRef::Primitive(PrimitiveType::Int64),
        ));
        let mut child = ObjectInfo::new("children", ObjectKind::Table);
        child.columns.clone_from(&parent.columns);
        let data = graph_from_trees(
            &[CatalogTree {
                name: "db".into(),
                schemas: vec![SchemaTree {
                    name: "public".into(),
                    objects: vec![parent, child],
                }],
            }],
            CatalogCoverage::complete(),
            "provider:db",
        );
        let graph = CatalogGraph {
            revision: sift_protocol::CatalogRevision(1),
            content_digest: "catfp:fixture".into(),
            invalidation_epoch: 1,
            captured_at: chrono::Utc::now(),
            provider: sift_protocol::ProviderRef {
                provider_id: sift_protocol::ProviderId::new("test/provider").unwrap(),
                dialect_id: sift_protocol::DialectId::new("test/dialect").unwrap(),
                provider_version: "1".into(),
            },
            database_identity: "db".into(),
            data,
        };
        let table = |name: &str| {
            graph
                .data
                .nodes
                .iter()
                .find(|node| node.kind == CatalogNodeKind::Table && node.name == name)
                .unwrap()
        };
        let columns = |table: &CatalogNode| {
            graph
                .data
                .nodes
                .iter()
                .filter(|node| {
                    node.kind == CatalogNodeKind::Column
                        && node.parent_id.as_ref() == Some(&table.id)
                })
                .map(|node| node.id.clone())
                .collect::<Vec<_>>()
        };
        let children = table("children");
        let parents = table("parents");
        let source_columns = columns(children);
        let target_columns = columns(parents);
        let (desired, renames) = apply_diagram_mutation(
            &graph,
            &sift_protocol::CatalogDiagramMutation::AddForeignKey {
                table_id: children.id.clone(),
                name: "children_parent_fk".into(),
                columns: source_columns.clone(),
                referenced_table_id: parents.id.clone(),
                referenced_columns: target_columns.clone(),
            },
        )
        .unwrap();
        assert!(renames.is_empty());
        let edge = desired
            .data
            .edges
            .iter()
            .find(|edge| edge.kind == CatalogEdgeKind::ForeignKey)
            .unwrap();
        assert_eq!(
            edge.column_pairs,
            source_columns
                .into_iter()
                .zip(target_columns)
                .map(|(from, to)| sift_protocol::CatalogColumnPair { from, to })
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn table_designer_adds_a_column_at_the_next_ordinal() {
        let mut table = ObjectInfo::new("events", ObjectKind::Table);
        table.columns.push(ColumnMetadata::new(
            "id",
            TypeRef::Primitive(PrimitiveType::Int64),
        ));
        let data = graph_from_trees(
            &[CatalogTree {
                name: "db".into(),
                schemas: vec![SchemaTree {
                    name: "public".into(),
                    objects: vec![table],
                }],
            }],
            CatalogCoverage::complete(),
            "provider:db",
        );
        let graph = CatalogGraph {
            revision: sift_protocol::CatalogRevision(1),
            content_digest: "catfp:fixture".into(),
            invalidation_epoch: 1,
            captured_at: chrono::Utc::now(),
            provider: sift_protocol::ProviderRef {
                provider_id: sift_protocol::ProviderId::new("test/provider").unwrap(),
                dialect_id: sift_protocol::DialectId::new("test/dialect").unwrap(),
                provider_version: "1".into(),
            },
            database_identity: "db".into(),
            data,
        };
        let table = graph
            .data
            .nodes
            .iter()
            .find(|node| node.kind == CatalogNodeKind::Table)
            .unwrap();
        let (desired, renames) = apply_diagram_mutation(
            &graph,
            &sift_protocol::CatalogDiagramMutation::AddColumn {
                table_id: table.id.clone(),
                name: "payload".into(),
                type_ref: TypeRef::Primitive(PrimitiveType::Jsonb),
                nullability: sift_protocol::Nullability::Nullable,
            },
        )
        .unwrap();
        assert!(renames.is_empty());
        let added = desired
            .data
            .nodes
            .iter()
            .find(|node| node.kind == CatalogNodeKind::Column && node.name == "payload")
            .unwrap();
        assert_eq!(added.parent_id.as_ref(), Some(&table.id));
        assert_eq!(added.ordinal, Some(2));
        assert!(desired.data.edges.iter().any(|edge| {
            edge.from == table.id
                && edge.to.as_ref() == Some(&added.id)
                && edge.kind == CatalogEdgeKind::Contains
        }));
        assert_eq!(
            apply_diagram_mutation(
                &desired,
                &sift_protocol::CatalogDiagramMutation::AddColumn {
                    table_id: table.id.clone(),
                    name: "payload".into(),
                    type_ref: TypeRef::Primitive(PrimitiveType::Text),
                    nullability: sift_protocol::Nullability::Nullable,
                },
            )
            .unwrap_err(),
            DiagramMutationError::InvalidColumnName
        );
    }
}
