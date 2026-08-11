//! Deterministic, provider-neutral catalog comparison (ADR-033).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use sha2::{Digest, Sha256};
use sift_protocol::{
    CatalogCoverageState, CatalogEdgeKind, CatalogGraph, CatalogNode, CatalogNodeDetails,
    CatalogNodeKind, CatalogObjectId, CatalogSourceRef, RenameMapping, RenameSuggestion,
    SchemaChange, SchemaChangeId, SchemaChangeKind, SchemaChangeReversibility, SchemaChangeRisk,
    SchemaDiff, SchemaDiffCoverage, SchemaFieldChange,
};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DiffError {
    #[error("schema diff max_changes must be between 1 and 100000")]
    InvalidLimit,
    #[error("accepted rename mappings must be one-to-one, refer to unmatched objects, and preserve object kind and structure")]
    InvalidRenameMapping,
    #[error("schema diff exceeds the requested change limit")]
    LimitExceeded,
    #[error("catalog node could not be normalized")]
    InvalidCatalogNode,
}

pub fn diff_catalogs(
    from_ref: CatalogSourceRef,
    from: &CatalogGraph,
    to_ref: CatalogSourceRef,
    to: &CatalogGraph,
    accepted_renames: &[RenameMapping],
    max_changes: Option<u32>,
) -> Result<SchemaDiff, DiffError> {
    let max_changes = max_changes.unwrap_or(10_000) as usize;
    if max_changes == 0 || max_changes > 100_000 {
        return Err(DiffError::InvalidLimit);
    }
    let from_nodes = node_map(from);
    let to_nodes = node_map(to);
    let mut matched_from = HashSet::new();
    let mut matched_to = HashSet::new();
    let mut pairs = Vec::new();

    // Opaque ids are strongest within one database identity.
    for (id, before) in &from_nodes {
        if let Some(after) = to_nodes.get(id) {
            if before.kind == after.kind {
                match_pair(
                    before,
                    after,
                    false,
                    &mut matched_from,
                    &mut matched_to,
                    &mut pairs,
                );
            }
        }
    }
    // Qualified kind/path is the portable identity across snapshots and
    // database identities; duplicate paths are rejected by graph validation.
    let to_paths = to
        .data
        .nodes
        .iter()
        .filter(|node| !matched_to.contains(&node.id))
        .map(|node| ((node.kind, node.qualified_name.as_str()), node))
        .collect::<HashMap<_, _>>();
    for before in &from.data.nodes {
        if matched_from.contains(&before.id) {
            continue;
        }
        if let Some(after) = to_paths.get(&(before.kind, before.qualified_name.as_str())) {
            match_pair(
                before,
                after,
                false,
                &mut matched_from,
                &mut matched_to,
                &mut pairs,
            );
        }
    }

    let mut rename_from = HashSet::new();
    let mut rename_to = HashSet::new();
    for mapping in accepted_renames {
        let Some(before) = from_nodes.get(&mapping.from) else {
            return Err(DiffError::InvalidRenameMapping);
        };
        let Some(after) = to_nodes.get(&mapping.to) else {
            return Err(DiffError::InvalidRenameMapping);
        };
        if before.kind != after.kind
            || !rename_compatible(before, after)?
            || matched_from.contains(&mapping.from)
            || matched_to.contains(&mapping.to)
            || !rename_from.insert(mapping.from.clone())
            || !rename_to.insert(mapping.to.clone())
        {
            return Err(DiffError::InvalidRenameMapping);
        }
        match_pair(
            before,
            after,
            true,
            &mut matched_from,
            &mut matched_to,
            &mut pairs,
        );
    }

    // A qualified identity change on a container necessarily changes every
    // descendant's opaque id and qualified path. Once the caller has
    // explicitly accepted that container transition, correlate unchanged
    // descendants by their local identity and normalized structure. Emitting
    // drop/create changes for those children would be both noisy and unsafe:
    // the engine's rename/move statement already carries them with the parent.
    correlate_renamed_descendants(
        accepted_renames,
        &from_nodes,
        &to_nodes,
        &mut matched_from,
        &mut matched_to,
    )?;

    let mut rename_suggestions =
        rename_suggestions(from, to, &matched_from, &matched_to, &from_nodes, &to_nodes)?;
    let definitive_drops = from.data.coverage.state == CatalogCoverageState::Complete
        && to.data.coverage.state == CatalogCoverageState::Complete;
    let mut warnings = Vec::new();
    if !definitive_drops {
        warnings.push(
            "catalog coverage is partial; missing target objects are reported as unknown, not definitive drops"
                .into(),
        );
    }

    let mut changes = Vec::new();
    for (before, after, renamed) in pairs {
        let fields = field_changes(before, after)?;
        if renamed || !fields.is_empty() {
            let kind = if renamed {
                let before_parent = before
                    .parent_id
                    .as_ref()
                    .and_then(|id| from_nodes.get(id))
                    .map(|node| node.qualified_name.as_str());
                let after_parent = after
                    .parent_id
                    .as_ref()
                    .and_then(|id| to_nodes.get(id))
                    .map(|node| node.qualified_name.as_str());
                if before_parent == after_parent {
                    SchemaChangeKind::Rename
                } else {
                    SchemaChangeKind::Move
                }
            } else {
                SchemaChangeKind::Alter
            };
            changes.push(change(kind, Some(before), Some(after), fields));
        }
    }
    for before in from
        .data
        .nodes
        .iter()
        .filter(|node| !matched_from.contains(&node.id))
    {
        let kind = if definitive_drops {
            SchemaChangeKind::Drop
        } else {
            SchemaChangeKind::Unknown
        };
        changes.push(change(kind, Some(before), None, Vec::new()));
    }
    for after in to
        .data
        .nodes
        .iter()
        .filter(|node| !matched_to.contains(&node.id))
    {
        changes.push(change(
            SchemaChangeKind::Create,
            None,
            Some(after),
            Vec::new(),
        ));
    }
    if changes.len() > max_changes {
        return Err(DiffError::LimitExceeded);
    }

    attach_dependencies(&mut changes, from, to);
    stable_dependency_order(&mut changes, &mut warnings);
    rename_suggestions.sort_by(|left, right| {
        left.from
            .cmp(&right.from)
            .then_with(|| left.to.cmp(&right.to))
    });
    let partial = !definitive_drops
        || changes
            .iter()
            .any(|change| change.kind == SchemaChangeKind::Unknown);
    let digest = diff_digest(&changes, &rename_suggestions, partial)?;
    Ok(SchemaDiff {
        from: from_ref,
        to: to_ref,
        digest,
        coverage: SchemaDiffCoverage {
            from: from.data.coverage.clone(),
            to: to.data.coverage.clone(),
            definitive_drops,
        },
        changes,
        rename_suggestions,
        warnings,
        partial,
    })
}

fn correlate_renamed_descendants(
    accepted_renames: &[RenameMapping],
    from_nodes: &HashMap<CatalogObjectId, &CatalogNode>,
    to_nodes: &HashMap<CatalogObjectId, &CatalogNode>,
    matched_from: &mut HashSet<CatalogObjectId>,
    matched_to: &mut HashSet<CatalogObjectId>,
) -> Result<(), DiffError> {
    let mut containers = accepted_renames
        .iter()
        .map(|mapping| (mapping.from.clone(), mapping.to.clone()))
        .collect::<Vec<_>>();
    let mut cursor = 0;
    while let Some((from_parent, to_parent)) = containers.get(cursor).cloned() {
        cursor += 1;
        let to_children = to_nodes
            .values()
            .filter(|node| node.parent_id.as_ref() == Some(&to_parent))
            .map(|node| ((node.kind, node.name.as_str()), *node))
            .collect::<HashMap<_, _>>();
        let from_children = from_nodes
            .values()
            .filter(|node| {
                node.parent_id.as_ref() == Some(&from_parent) && !matched_from.contains(&node.id)
            })
            .copied()
            .collect::<Vec<_>>();
        for before in from_children {
            let Some(after) = to_children.get(&(before.kind, before.name.as_str())) else {
                continue;
            };
            if matched_to.contains(&after.id) || !rename_compatible(before, after)? {
                continue;
            }
            matched_from.insert(before.id.clone());
            matched_to.insert(after.id.clone());
            containers.push((before.id.clone(), after.id.clone()));
        }
    }
    Ok(())
}

fn rename_compatible(before: &CatalogNode, after: &CatalogNode) -> Result<bool, DiffError> {
    let before = serde_json::to_value((
        before.ordinal,
        before.definition_digest.as_ref(),
        before.completeness,
        &before.details,
        &before.extra,
    ))
    .map_err(|_| DiffError::InvalidCatalogNode)?;
    let after = serde_json::to_value((
        after.ordinal,
        after.definition_digest.as_ref(),
        after.completeness,
        &after.details,
        &after.extra,
    ))
    .map_err(|_| DiffError::InvalidCatalogNode)?;
    Ok(before == after)
}

fn node_map(graph: &CatalogGraph) -> HashMap<CatalogObjectId, &CatalogNode> {
    graph
        .data
        .nodes
        .iter()
        .map(|node| (node.id.clone(), node))
        .collect()
}

fn match_pair<'a>(
    before: &'a CatalogNode,
    after: &'a CatalogNode,
    renamed: bool,
    matched_from: &mut HashSet<CatalogObjectId>,
    matched_to: &mut HashSet<CatalogObjectId>,
    pairs: &mut Vec<(&'a CatalogNode, &'a CatalogNode, bool)>,
) {
    if matched_from.insert(before.id.clone()) && matched_to.insert(after.id.clone()) {
        pairs.push((before, after, renamed));
    }
}

fn field_changes(
    before: &CatalogNode,
    after: &CatalogNode,
) -> Result<Vec<SchemaFieldChange>, DiffError> {
    let mut fields = Vec::new();
    push_field(&mut fields, "name", &before.name, &after.name)?;
    push_field(
        &mut fields,
        "qualified_name",
        &before.qualified_name,
        &after.qualified_name,
    )?;
    push_field(&mut fields, "ordinal", &before.ordinal, &after.ordinal)?;
    push_field(
        &mut fields,
        "definition_digest",
        &before.definition_digest,
        &after.definition_digest,
    )?;
    push_field(
        &mut fields,
        "completeness",
        &before.completeness,
        &after.completeness,
    )?;
    push_field(&mut fields, "details", &before.details, &after.details)?;
    push_field(&mut fields, "extra", &before.extra, &after.extra)?;
    Ok(fields)
}

fn push_field<T: serde::Serialize>(
    fields: &mut Vec<SchemaFieldChange>,
    field: &str,
    before: &T,
    after: &T,
) -> Result<(), DiffError> {
    let before = serde_json::to_value(before).map_err(|_| DiffError::InvalidCatalogNode)?;
    let after = serde_json::to_value(after).map_err(|_| DiffError::InvalidCatalogNode)?;
    if before != after {
        fields.push(SchemaFieldChange {
            field: field.into(),
            before: Some(before),
            after: Some(after),
        });
    }
    Ok(())
}

fn change(
    kind: SchemaChangeKind,
    before: Option<&CatalogNode>,
    after: Option<&CatalogNode>,
    field_changes: Vec<SchemaFieldChange>,
) -> SchemaChange {
    let seed = format!(
        "{kind:?}:{}:{}",
        before.map_or("", |node| node.id.0.as_str()),
        after.map_or("", |node| node.id.0.as_str())
    );
    let node = after.or(before).expect("a schema change has an object");
    let risk = risk(kind, before, after, node);
    let reversibility = match kind {
        SchemaChangeKind::Create | SchemaChangeKind::Rename | SchemaChangeKind::Move => {
            SchemaChangeReversibility::Exact
        }
        SchemaChangeKind::Drop => SchemaChangeReversibility::Lossy,
        SchemaChangeKind::Alter | SchemaChangeKind::Unknown => {
            SchemaChangeReversibility::Unavailable
        }
    };
    SchemaChange {
        id: SchemaChangeId(format!("chg:{}", hex_digest(seed.as_bytes()))),
        kind,
        object_before: before.cloned(),
        object_after: after.cloned(),
        field_changes,
        prerequisites: Vec::new(),
        dependency_group: None,
        risk,
        reversibility,
    }
}

fn risk(
    kind: SchemaChangeKind,
    before: Option<&CatalogNode>,
    after: Option<&CatalogNode>,
    node: &CatalogNode,
) -> SchemaChangeRisk {
    match kind {
        SchemaChangeKind::Unknown => SchemaChangeRisk::Unknown,
        SchemaChangeKind::Drop => SchemaChangeRisk::DataLoss,
        SchemaChangeKind::Rename | SchemaChangeKind::Move => SchemaChangeRisk::Locking,
        SchemaChangeKind::Create => match (&node.kind, &node.details) {
            (CatalogNodeKind::Column, CatalogNodeDetails::Column { column })
                if column.nullable == sift_protocol::Nullability::NotNullable
                    && column
                        .facets
                        .postgres
                        .as_ref()
                        .and_then(|facets| facets.default_expr.as_ref())
                        .or_else(|| {
                            column
                                .facets
                                .sql_server
                                .as_ref()
                                .and_then(|facets| facets.default_expr.as_ref())
                        })
                        .is_none() =>
            {
                SchemaChangeRisk::DataRewrite
            }
            (CatalogNodeKind::Index | CatalogNodeKind::Constraint, _) => SchemaChangeRisk::Locking,
            _ => SchemaChangeRisk::Safe,
        },
        SchemaChangeKind::Alter => match before.zip(after) {
            Some((before, after)) if node.kind == CatalogNodeKind::Column => {
                column_alter_risk(before, after)
            }
            _ => SchemaChangeRisk::Locking,
        },
    }
}

fn column_alter_risk(before: &CatalogNode, after: &CatalogNode) -> SchemaChangeRisk {
    let (
        CatalogNodeDetails::Column {
            column: before_column,
        },
        CatalogNodeDetails::Column {
            column: after_column,
        },
    ) = (&before.details, &after.details)
    else {
        return SchemaChangeRisk::Unknown;
    };
    if before_column.type_ref != after_column.type_ref {
        return if is_widening_type_change(&before_column.type_ref, &after_column.type_ref) {
            SchemaChangeRisk::DataRewrite
        } else {
            SchemaChangeRisk::DataLoss
        };
    }
    if before_column.nullable == sift_protocol::Nullability::Nullable
        && after_column.nullable == sift_protocol::Nullability::NotNullable
    {
        return SchemaChangeRisk::DataRewrite;
    }
    let facets_changed = serde_json::to_value(&before_column.facets)
        .ok()
        .zip(serde_json::to_value(&after_column.facets).ok())
        .map_or(true, |(before, after)| before != after);
    if facets_changed
        || before_column.auto_increment != after_column.auto_increment
        || before_column.primary_key != after_column.primary_key
    {
        SchemaChangeRisk::DataRewrite
    } else {
        SchemaChangeRisk::Locking
    }
}

fn is_widening_type_change(
    before: &sift_protocol::TypeRef,
    after: &sift_protocol::TypeRef,
) -> bool {
    use sift_protocol::{PrimitiveType, TypeRef};
    matches!(
        (before, after),
        (
            TypeRef::Primitive(PrimitiveType::Int16),
            TypeRef::Primitive(PrimitiveType::Int32 | PrimitiveType::Int64)
        ) | (
            TypeRef::Primitive(PrimitiveType::Int32),
            TypeRef::Primitive(PrimitiveType::Int64)
        ) | (
            TypeRef::Primitive(PrimitiveType::Float32),
            TypeRef::Primitive(PrimitiveType::Float64)
        )
    )
}

fn rename_suggestions(
    from: &CatalogGraph,
    to: &CatalogGraph,
    matched_from: &HashSet<CatalogObjectId>,
    matched_to: &HashSet<CatalogObjectId>,
    _from_nodes: &HashMap<CatalogObjectId, &CatalogNode>,
    _to_nodes: &HashMap<CatalogObjectId, &CatalogNode>,
) -> Result<Vec<RenameSuggestion>, DiffError> {
    let mut suggestions = Vec::new();
    for before in from
        .data
        .nodes
        .iter()
        .filter(|node| !matched_from.contains(&node.id))
    {
        let candidates = to
            .data
            .nodes
            .iter()
            .filter(|after| !matched_to.contains(&after.id) && before.kind == after.kind)
            .filter(|after| {
                before.native_id.is_some()
                    && before.native_id == after.native_id
                    && from.database_identity == to.database_identity
            })
            .collect::<Vec<_>>();
        if let [after] = candidates.as_slice() {
            suggestions.push(RenameSuggestion {
                from: before.id.clone(),
                to: after.id.clone(),
                reason: "same native identity in the same database".into(),
            });
        }
    }
    Ok(suggestions)
}

fn attach_dependencies(changes: &mut [SchemaChange], from: &CatalogGraph, to: &CatalogGraph) {
    let mut creates = HashMap::new();
    let mut drops = HashMap::new();
    for change in changes.iter() {
        if let Some(node) = &change.object_after {
            if matches!(change.kind, SchemaChangeKind::Create) {
                creates.insert(node.id.clone(), change.id.clone());
            }
        }
        if let Some(node) = &change.object_before {
            if matches!(change.kind, SchemaChangeKind::Drop) {
                drops.insert(node.id.clone(), change.id.clone());
            }
        }
    }
    let mut prerequisites: HashMap<SchemaChangeId, BTreeSet<SchemaChangeId>> = HashMap::new();
    for node in &to.data.nodes {
        let Some(change) = creates.get(&node.id) else {
            continue;
        };
        if let Some(parent) = node.parent_id.as_ref().and_then(|id| creates.get(id)) {
            prerequisites
                .entry(change.clone())
                .or_default()
                .insert(parent.clone());
        }
    }
    for edge in &to.data.edges {
        if edge.kind == CatalogEdgeKind::Contains {
            continue;
        }
        if let (Some(change), Some(target)) = (
            creates.get(&edge.from),
            edge.to.as_ref().and_then(|id| creates.get(id)),
        ) {
            prerequisites
                .entry(change.clone())
                .or_default()
                .insert(target.clone());
        }
    }
    for node in &from.data.nodes {
        let Some(parent_drop) = node.parent_id.as_ref().and_then(|id| drops.get(id)) else {
            continue;
        };
        if let Some(child_drop) = drops.get(&node.id) {
            prerequisites
                .entry(parent_drop.clone())
                .or_default()
                .insert(child_drop.clone());
        }
    }
    for edge in &from.data.edges {
        if edge.kind == CatalogEdgeKind::Contains {
            continue;
        }
        if let (Some(target_drop), Some(dependent_drop)) = (
            edge.to.as_ref().and_then(|id| drops.get(id)),
            drops.get(&edge.from),
        ) {
            prerequisites
                .entry(target_drop.clone())
                .or_default()
                .insert(dependent_drop.clone());
        }
    }
    for change in changes.iter() {
        if !matches!(
            change.kind,
            SchemaChangeKind::Rename | SchemaChangeKind::Move
        ) {
            continue;
        }
        if let Some(target_parent_create) = change
            .object_after
            .as_ref()
            .and_then(|node| node.parent_id.as_ref())
            .and_then(|parent| creates.get(parent))
        {
            prerequisites
                .entry(change.id.clone())
                .or_default()
                .insert(target_parent_create.clone());
        }
        if let Some(source_parent_drop) = change
            .object_before
            .as_ref()
            .and_then(|node| node.parent_id.as_ref())
            .and_then(|parent| drops.get(parent))
        {
            prerequisites
                .entry(source_parent_drop.clone())
                .or_default()
                .insert(change.id.clone());
        }
    }
    for change in changes {
        change.prerequisites = prerequisites
            .remove(&change.id)
            .unwrap_or_default()
            .into_iter()
            .collect();
    }
}

fn stable_dependency_order(changes: &mut Vec<SchemaChange>, warnings: &mut Vec<String>) {
    let mut pending = changes
        .drain(..)
        .map(|change| (change.id.clone(), change))
        .collect::<BTreeMap<_, _>>();
    let all_ids = pending.keys().cloned().collect::<HashSet<_>>();
    let mut emitted = HashSet::new();
    let mut ordered = Vec::with_capacity(pending.len());
    loop {
        let ready = pending
            .iter()
            .filter(|(_, change)| {
                change
                    .prerequisites
                    .iter()
                    .filter(|id| all_ids.contains(*id))
                    .all(|id| emitted.contains(id))
            })
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        if ready.is_empty() {
            break;
        }
        for id in ready {
            let change = pending.remove(&id).expect("ready change remains pending");
            emitted.insert(id);
            ordered.push(change);
        }
    }
    if !pending.is_empty() {
        let group_seed = pending
            .keys()
            .map(|id| id.0.as_str())
            .collect::<Vec<_>>()
            .join(":");
        let group = format!("cycle:{}", hex_digest(group_seed.as_bytes()));
        warnings.push(format!(
            "dependency cycle `{group}` requires an engine-specific migration strategy"
        ));
        for (_, mut change) in pending {
            change.dependency_group = Some(group.clone());
            ordered.push(change);
        }
    }
    *changes = ordered;
}

fn diff_digest(
    changes: &[SchemaChange],
    suggestions: &[RenameSuggestion],
    partial: bool,
) -> Result<String, DiffError> {
    let bytes = serde_json::to_vec(&(changes, suggestions, partial))
        .map_err(|_| DiffError::InvalidCatalogNode)?;
    Ok(format!("difffp:{}", hex_digest(&bytes)))
}

fn hex_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use sift_protocol::{
        CatalogCoverage, CatalogEdge, CatalogEdgeCertainty, CatalogGraphOptions, CatalogRevision,
        CatalogTree, ColumnMetadata, ObjectInfo, ObjectKind, PrimitiveType, ProviderRef,
        SchemaTree, TypeRef,
    };

    use super::*;

    fn graph(objects: &[&str], seed: &str) -> CatalogGraph {
        let trees = vec![CatalogTree {
            name: "db".into(),
            schemas: vec![SchemaTree {
                name: "public".into(),
                objects: objects
                    .iter()
                    .map(|name| ObjectInfo::new(*name, ObjectKind::Table))
                    .collect(),
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
            database_identity: seed.into(),
            data: crate::catalog::graph_from_trees(&trees, CatalogCoverage::complete(), seed),
        }
    }

    fn source() -> CatalogSourceRef {
        CatalogSourceRef::Live {
            expected_revision: CatalogRevision(1),
            options: CatalogGraphOptions::default(),
        }
    }

    fn graph_with_column(
        table: &str,
        column: &str,
        primitive: PrimitiveType,
        seed: &str,
    ) -> CatalogGraph {
        let mut object = ObjectInfo::new(table, ObjectKind::Table);
        object
            .columns
            .push(ColumnMetadata::new(column, TypeRef::Primitive(primitive)));
        let mut graph = graph(&[], seed);
        graph.data = crate::catalog::graph_from_trees(
            &[CatalogTree {
                name: "db".into(),
                schemas: vec![SchemaTree {
                    name: "public".into(),
                    objects: vec![object],
                }],
            }],
            CatalogCoverage::complete(),
            seed,
        );
        graph
    }

    fn graph_in_schema(schema: &str) -> CatalogGraph {
        let trees = vec![CatalogTree {
            name: "db".into(),
            schemas: vec![SchemaTree {
                name: schema.into(),
                objects: vec![ObjectInfo::new("events", ObjectKind::Table)],
            }],
        }];
        CatalogGraph {
            data: crate::catalog::graph_from_trees(&trees, CatalogCoverage::complete(), "db"),
            ..graph(&[], "db")
        }
    }

    #[test]
    fn portable_identity_ignores_database_seed_and_orders_parent_before_create() {
        let from = graph(&["users"], "old");
        let to = graph(&["users", "posts"], "new");
        let diff = diff_catalogs(source(), &from, source(), &to, &[], None).unwrap();
        assert!(diff.changes.iter().all(|change| {
            change
                .object_after
                .as_ref()
                .map_or(true, |node| node.name != "users")
        }));
        assert!(diff.changes.iter().any(|change| {
            change.kind == SchemaChangeKind::Create
                && change
                    .object_after
                    .as_ref()
                    .is_some_and(|node| node.name == "posts")
        }));
    }

    #[test]
    fn partial_target_never_claims_a_drop() {
        let from = graph(&["users"], "db");
        let mut to = graph(&[], "db");
        to.data.coverage.state = CatalogCoverageState::Partial;
        let diff = diff_catalogs(source(), &from, source(), &to, &[], None).unwrap();
        assert!(!diff.coverage.definitive_drops);
        assert!(diff
            .changes
            .iter()
            .all(|change| change.kind != SchemaChangeKind::Drop));
        assert!(diff
            .changes
            .iter()
            .any(|change| change.kind == SchemaChangeKind::Unknown));
    }

    #[test]
    fn column_type_changes_are_conservatively_risk_classified() {
        let narrow = diff_catalogs(
            source(),
            &graph_with_column("events", "value", PrimitiveType::Int64, "db"),
            source(),
            &graph_with_column("events", "value", PrimitiveType::Int16, "db"),
            &[],
            None,
        )
        .unwrap();
        assert_eq!(narrow.changes.len(), 1);
        assert_eq!(narrow.changes[0].risk, SchemaChangeRisk::DataLoss);

        let widen = diff_catalogs(
            source(),
            &graph_with_column("events", "value", PrimitiveType::Int16, "db"),
            source(),
            &graph_with_column("events", "value", PrimitiveType::Int64, "db"),
            &[],
            None,
        )
        .unwrap();
        assert_eq!(widen.changes[0].risk, SchemaChangeRisk::DataRewrite);
    }

    #[test]
    fn rename_mapping_rejects_a_simultaneous_structural_change() {
        let from = graph_with_column("events", "old", PrimitiveType::Int64, "db");
        let to = graph_with_column("events", "new", PrimitiveType::Text, "db");
        let old = from
            .data
            .nodes
            .iter()
            .find(|node| node.kind == CatalogNodeKind::Column)
            .unwrap();
        let new = to
            .data
            .nodes
            .iter()
            .find(|node| node.kind == CatalogNodeKind::Column)
            .unwrap();
        assert!(matches!(
            diff_catalogs(
                source(),
                &from,
                source(),
                &to,
                &[RenameMapping {
                    from: old.id.clone(),
                    to: new.id.clone(),
                }],
                None,
            ),
            Err(DiffError::InvalidRenameMapping)
        ));
    }

    #[test]
    fn dependency_cycles_are_named_instead_of_arbitrarily_ordered() {
        let from = graph(&[], "db");
        let mut to = graph(&["a", "b"], "db");
        let tables = to
            .data
            .nodes
            .iter()
            .filter(|node| node.kind == CatalogNodeKind::Table)
            .map(|node| node.id.clone())
            .collect::<Vec<_>>();
        for (from, to_id) in [
            (tables[0].clone(), tables[1].clone()),
            (tables[1].clone(), tables[0].clone()),
        ] {
            to.data.edges.push(CatalogEdge {
                from,
                to: Some(to_id),
                kind: CatalogEdgeKind::DependsOn,
                certainty: CatalogEdgeCertainty::CatalogProven,
                referenced_path: None,
                column_pairs: Vec::new(),
            });
        }
        let diff = diff_catalogs(source(), &from, source(), &to, &[], None).unwrap();
        let groups = diff
            .changes
            .iter()
            .filter_map(|change| change.dependency_group.as_deref())
            .collect::<Vec<_>>();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0], groups[1]);
        assert!(diff
            .warnings
            .iter()
            .any(|warning| warning.contains(groups[0])));
    }

    #[test]
    fn move_is_ordered_between_target_schema_create_and_source_schema_drop() {
        let from = graph_in_schema("old");
        let to = graph_in_schema("new");
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
        let diff = diff_catalogs(
            source(),
            &from,
            source(),
            &to,
            &[RenameMapping {
                from: before.id.clone(),
                to: after.id.clone(),
            }],
            None,
        )
        .unwrap();
        let create = diff
            .changes
            .iter()
            .position(|change| {
                change.kind == SchemaChangeKind::Create
                    && change
                        .object_after
                        .as_ref()
                        .is_some_and(|node| node.kind == CatalogNodeKind::Schema)
            })
            .unwrap();
        let moved = diff
            .changes
            .iter()
            .position(|change| change.kind == SchemaChangeKind::Move)
            .unwrap();
        let drop = diff
            .changes
            .iter()
            .position(|change| {
                change.kind == SchemaChangeKind::Drop
                    && change
                        .object_before
                        .as_ref()
                        .is_some_and(|node| node.kind == CatalogNodeKind::Schema)
            })
            .unwrap();
        assert!(create < moved && moved < drop);
    }

    #[test]
    fn accepted_table_move_carries_unchanged_children_without_drop_create_noise() {
        let mut from = graph_in_schema("old");
        let mut to = graph_in_schema("new");
        for graph in [&mut from, &mut to] {
            let schema = graph
                .data
                .nodes
                .iter()
                .find(|node| node.kind == CatalogNodeKind::Schema)
                .unwrap()
                .name
                .clone();
            graph.data = crate::catalog::graph_from_trees(
                &[CatalogTree {
                    name: "db".into(),
                    schemas: vec![SchemaTree {
                        name: schema,
                        objects: vec![{
                            let mut table = ObjectInfo::new("events", ObjectKind::Table);
                            table.columns.push(ColumnMetadata::new(
                                "id",
                                TypeRef::Primitive(PrimitiveType::Int64),
                            ));
                            table
                        }],
                    }],
                }],
                CatalogCoverage::complete(),
                "db",
            );
        }
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
        let diff = diff_catalogs(
            source(),
            &from,
            source(),
            &to,
            &[RenameMapping {
                from: before.id.clone(),
                to: after.id.clone(),
            }],
            None,
        )
        .unwrap();
        assert_eq!(
            diff.changes
                .iter()
                .filter(|change| {
                    change
                        .object_before
                        .as_ref()
                        .or(change.object_after.as_ref())
                        .is_some_and(|node| node.kind == CatalogNodeKind::Column)
                })
                .count(),
            0
        );
        assert_eq!(
            diff.changes
                .iter()
                .filter(|change| change.kind == SchemaChangeKind::Move)
                .count(),
            1
        );
    }

    #[test]
    fn diff_digest_and_order_ignore_provider_row_order() {
        let from = graph(&["users", "legacy"], "db");
        let to = graph(&["users", "posts"], "db");
        let expected = diff_catalogs(source(), &from, source(), &to, &[], None).unwrap();

        let mut shuffled_from = from.clone();
        shuffled_from.data.nodes.reverse();
        shuffled_from.data.edges.reverse();
        let mut shuffled_to = to.clone();
        shuffled_to.data.nodes.rotate_left(1);
        shuffled_to.data.edges.reverse();
        let actual =
            diff_catalogs(source(), &shuffled_from, source(), &shuffled_to, &[], None).unwrap();

        assert_eq!(actual.digest, expected.digest);
        assert_eq!(
            serde_json::to_value(actual.changes).unwrap(),
            serde_json::to_value(expected.changes).unwrap()
        );
    }
}
