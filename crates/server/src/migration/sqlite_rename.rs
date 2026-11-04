//! Native column renames carry only the proven table-definition name changes.
use super::*;
use sift_protocol::CatalogObjectId;

#[derive(Default)]
pub(super) struct ColumnRenames {
    pub tables: HashSet<SchemaChangeId>,
    pub covered: HashSet<SchemaChangeId>,
    pub groups: HashMap<SchemaChangeId, Vec<Step>>,
}

pub(super) struct Step {
    pub change: SchemaChangeId,
    pub forward: String,
    pub rollback: String,
}
struct Pending {
    change: SchemaChangeId,
    column: CatalogObjectId,
    old: String,
    new: String,
    ordinal: Option<u32>,
}

fn shape(node: &CatalogNode) -> Result<serde_json::Value, MigrationRenderError> {
    let mut details = node.details.clone();
    if let CatalogNodeDetails::Column { column } = &mut details {
        column.name.clear();
    }
    let mut extra = node.extra.clone();
    if node.kind == CatalogNodeKind::Table {
        extra.remove("sqlite_safe_create_sql");
    }
    serde_json::to_value((
        node.kind,
        node.ordinal,
        &node.definition_digest,
        node.completeness,
        details,
        extra,
    ))
    .map_err(|_| MigrationRenderError::Serialization)
}

fn children<'a>(graph: &'a CatalogGraph, parent: &CatalogObjectId) -> Vec<&'a CatalogNode> {
    let mut columns = graph
        .data
        .nodes
        .iter()
        .filter(|node| node.parent_id.as_ref() == Some(parent))
        .collect::<Vec<_>>();
    columns.sort_by_key(|node| node.ordinal);
    columns
}

pub(super) fn render(
    diff: &SchemaDiff,
    from: &CatalogGraph,
    to: &CatalogGraph,
    from_nodes: &HashMap<CatalogObjectId, &CatalogNode>,
    to_nodes: &HashMap<CatalogObjectId, &CatalogNode>,
    selected: &HashSet<SchemaChangeId>,
) -> Result<ColumnRenames, MigrationRenderError> {
    let mut groups: HashMap<(CatalogObjectId, CatalogObjectId), Vec<&SchemaChange>> =
        HashMap::new();
    for change in diff.changes.iter().filter(|change| {
        selected.contains(&change.id)
            && change.kind == SchemaChangeKind::Rename
            && change
                .object_before
                .as_ref()
                .is_some_and(|node| node.kind == CatalogNodeKind::Column)
    }) {
        let before = change
            .object_before
            .as_ref()
            .and_then(|node| from_nodes.get(&node.id))
            .copied()
            .ok_or_else(|| MigrationRenderError::InvalidChangeShape(change.id.clone()))?;
        let after = change
            .object_after
            .as_ref()
            .and_then(|node| to_nodes.get(&node.id))
            .copied()
            .ok_or_else(|| MigrationRenderError::InvalidChangeShape(change.id.clone()))?;
        let parents = before
            .parent_id
            .as_ref()
            .zip(after.parent_id.as_ref())
            .ok_or_else(|| MigrationRenderError::IncompleteHierarchy(change.id.clone()))?;
        groups
            .entry((parents.0.clone(), parents.1.clone()))
            .or_default()
            .push(change);
    }
    // The capture proof is independent of individual rename groups. Check it
    // once, rather than rescanning every table for every selected table.
    if let Some(anchor) = groups.values().find_map(|renames| renames.first()).copied() {
        validate_sqlite_rename_catalog(anchor, from_nodes, to_nodes, from, to)?;
    }
    let mut result = ColumnRenames::default();
    for ((old_parent, new_parent), renames) in groups {
        let anchor = renames[0];
        let before = from_nodes
            .get(&old_parent)
            .copied()
            .ok_or_else(|| MigrationRenderError::IncompleteHierarchy(anchor.id.clone()))?;
        let after = to_nodes
            .get(&new_parent)
            .copied()
            .ok_or_else(|| MigrationRenderError::IncompleteHierarchy(anchor.id.clone()))?;
        if before.kind != CatalogNodeKind::Table
            || after.kind != CatalogNodeKind::Table
            || before.name != after.name
            || before.qualified_name != after.qualified_name
            || shape(before)? != shape(after)?
        {
            return unsupported(anchor, before);
        }
        let mut create = anchor.clone();
        create.kind = SchemaChangeKind::Create;
        sqlite_create_sql(&create, before, from_nodes, from)?;
        sqlite_create_sql(&create, after, to_nodes, to)?;
        let table_change = diff
            .changes
            .iter()
            .find(|change| {
                change.kind == SchemaChangeKind::Alter
                    && change
                        .object_before
                        .as_ref()
                        .is_some_and(|node| node.id == before.id)
                    && change
                        .object_after
                        .as_ref()
                        .is_some_and(|node| node.id == after.id)
            })
            .ok_or_else(|| MigrationRenderError::InvalidChangeShape(anchor.id.clone()))?;
        if selected.contains(&table_change.id) {
            let old_columns = children(from, &before.id);
            let new_columns = children(to, &after.id);
            if old_columns.len() != new_columns.len() {
                return unsupported(anchor, before);
            }
            for (old, new) in old_columns.into_iter().zip(new_columns) {
                if shape(old)? != shape(new)? {
                    return unsupported(anchor, old);
                }
                if old.name != new.name
                    && !renames.iter().any(|change| {
                        change
                            .object_before
                            .as_ref()
                            .is_some_and(|node| node.id == old.id)
                            && change
                                .object_after
                                .as_ref()
                                .is_some_and(|node| node.id == new.id)
                    })
                {
                    return unsupported(anchor, old);
                }
            }
            result.tables.insert(table_change.id.clone());
        }
        let mut pending = Vec::new();
        for change in &renames {
            let old = change
                .object_before
                .as_ref()
                .and_then(|node| from_nodes.get(&node.id))
                .copied()
                .ok_or_else(|| MigrationRenderError::InvalidChangeShape(change.id.clone()))?;
            let new = change
                .object_after
                .as_ref()
                .and_then(|node| to_nodes.get(&node.id))
                .copied()
                .ok_or_else(|| MigrationRenderError::InvalidChangeShape(change.id.clone()))?;
            if old.name == new.name || shape(old)? != shape(new)? {
                return unsupported(change, old);
            }
            pending.push(Pending {
                change: change.id.clone(),
                column: old.id.clone(),
                old: old.name.clone(),
                new: new.name.clone(),
                ordinal: old.ordinal,
            });
        }
        pending.sort_by_key(|item| item.ordinal);
        let mut occupied = children(from, &before.id)
            .into_iter()
            .map(|node| (node.name.to_ascii_lowercase(), node.id.clone()))
            .collect::<HashMap<_, _>>();
        let mut reserved = children(to, &after.id)
            .into_iter()
            .map(|node| node.name.to_ascii_lowercase())
            .collect::<HashSet<_>>();
        let sql = |old: &str, new: &str| {
            format!(
                "ALTER TABLE \"main\".{} RENAME COLUMN {} TO {};",
                crate::ddl::quote_ident(&before.name, Engine::Sqlite),
                crate::ddl::quote_ident(old, Engine::Sqlite),
                crate::ddl::quote_ident(new, Engine::Sqlite)
            )
        };
        let mut steps = Vec::new();
        let seed = crate::fingerprint::sql(&format!("{}:{}", diff.digest, before.id.0));
        let mut counter = 0usize;
        while !pending.is_empty() {
            if let Some(index) = pending.iter().position(|item| {
                occupied
                    .get(&item.new.to_ascii_lowercase())
                    .map_or(true, |owner| *owner == item.column)
            }) {
                let item = pending.remove(index);
                occupied.remove(&item.old.to_ascii_lowercase());
                occupied.insert(item.new.to_ascii_lowercase(), item.column);
                steps.push(Step {
                    change: item.change,
                    forward: sql(&item.old, &item.new),
                    rollback: sql(&item.new, &item.old),
                });
                continue;
            }
            let pending_columns = pending
                .iter()
                .map(|item| item.column.clone())
                .collect::<HashSet<_>>();
            if let Some(blocked) = pending
                .iter()
                .filter_map(|item| occupied.get(&item.new.to_ascii_lowercase()))
                .find(|owner| !pending_columns.contains(*owner))
            {
                if let Some(required) = diff.changes.iter().find(|change| {
                    change.kind == SchemaChangeKind::Rename
                        && change
                            .object_before
                            .as_ref()
                            .is_some_and(|node| node.id == *blocked)
                }) {
                    return Err(MigrationRenderError::MissingPrerequisite(
                        required.id.clone(),
                    ));
                }
                return unsupported(anchor, before);
            }
            // Every destination is occupied by a selected rename: break a
            // cycle without touching any unselected column or its data.
            let temp = loop {
                let candidate = format!("__sift_rename_{}_{counter}", &seed[..12]);
                counter += 1;
                let key = candidate.to_ascii_lowercase();
                if !occupied.contains_key(&key) && reserved.insert(key) {
                    break candidate;
                }
            };
            let item = &mut pending[0];
            steps.push(Step {
                change: item.change.clone(),
                forward: sql(&item.old, &temp),
                rollback: sql(&temp, &item.old),
            });
            occupied.remove(&item.old.to_ascii_lowercase());
            occupied.insert(temp.to_ascii_lowercase(), item.column.clone());
            item.old = temp;
        }
        result.covered.extend(
            renames
                .iter()
                .filter(|change| change.id != anchor.id)
                .map(|change| change.id.clone()),
        );
        result.groups.insert(anchor.id.clone(), steps);
    }
    Ok(result)
}
