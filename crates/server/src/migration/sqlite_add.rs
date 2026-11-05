//! Add only a proven prefix of appended nullable columns to an unchanged table.
use super::sqlite_rename::{children, shape, ColumnRenames, Step};
use super::*;

pub(super) fn render(
    diff: &SchemaDiff,
    from: &CatalogGraph,
    to: &CatalogGraph,
    from_nodes: &HashMap<sift_protocol::CatalogObjectId, &CatalogNode>,
    to_nodes: &HashMap<sift_protocol::CatalogObjectId, &CatalogNode>,
    selected: &HashSet<SchemaChangeId>,
) -> Result<ColumnRenames, MigrationRenderError> {
    let mut groups: HashMap<SchemaChangeId, Vec<&SchemaChange>> = HashMap::new();
    for change in diff.changes.iter().filter(|c| {
        selected.contains(&c.id)
            && c.kind == SchemaChangeKind::Create
            && c.object_after
                .as_ref()
                .is_some_and(|n| n.kind == CatalogNodeKind::Column)
    }) {
        let after = change.object_after.as_ref().unwrap();
        if diff.changes.iter().any(|c| {
            c.kind == SchemaChangeKind::Create
                && c.object_after.as_ref().is_some_and(|n| {
                    n.kind == CatalogNodeKind::Table && Some(&n.id) == after.parent_id.as_ref()
                })
        }) {
            continue;
        }
        let parent = diff
            .changes
            .iter()
            .find(|c| {
                c.kind == SchemaChangeKind::Alter
                    && c.object_after.as_ref().is_some_and(|n| {
                        n.kind == CatalogNodeKind::Table && Some(&n.id) == after.parent_id.as_ref()
                    })
            })
            .ok_or_else(|| MigrationRenderError::IncompleteHierarchy(change.id.clone()))?;
        groups.entry(parent.id.clone()).or_default().push(change);
    }
    if let Some(parent_id) = groups.keys().next() {
        let parent = diff
            .changes
            .iter()
            .find(|change| &change.id == parent_id)
            .unwrap();
        validate_sqlite_rename_catalog(parent, from_nodes, to_nodes, from, to)?;
    }
    let mut result = ColumnRenames::default();
    for (parent_id, additions) in groups {
        let anchor = additions[0];
        let parent = diff.changes.iter().find(|c| c.id == parent_id).unwrap();
        let (Some(before), Some(after)) =
            (parent.object_before.as_ref(), parent.object_after.as_ref())
        else {
            return unsupported(anchor, anchor.object_after.as_ref().unwrap());
        };
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
        let old = children(from, &before.id);
        let new = children(to, &after.id);
        if new.len() <= old.len() {
            return unsupported(anchor, before);
        }
        for (old, new) in old.iter().zip(&new) {
            if old.name != new.name
                || old.qualified_name != new.qualified_name
                || shape(old)? != shape(new)?
            {
                return unsupported(anchor, old);
            }
        }
        let tail = &new[old.len()..];
        let mut steps = Vec::new();
        let mut missing = None;
        for node in tail {
            let CatalogNodeDetails::Column { column } = &node.details else {
                return unsupported(anchor, node);
            };
            if column.nullable != Nullability::Nullable || node.name.chars().any(char::is_control) {
                return unsupported(anchor, node);
            }
            let change = diff
                .changes
                .iter()
                .find(|c| {
                    c.kind == SchemaChangeKind::Create
                        && c.object_after.as_ref().is_some_and(|n| n.id == node.id)
                })
                .ok_or_else(|| MigrationRenderError::InvalidChangeShape(anchor.id.clone()))?;
            if !selected.contains(&change.id) {
                missing.get_or_insert(change.id.clone());
                if selected.contains(&parent_id) {
                    return Err(MigrationRenderError::MissingPrerequisite(change.id.clone()));
                }
                continue;
            }
            if let Some(missing) = &missing {
                return Err(MigrationRenderError::MissingPrerequisite(missing.clone()));
            }
            let declared = &column.facets.sqlite.as_ref().unwrap().declared_type;
            steps.push(Step {
                change: change.id.clone(),
                forward: format!(
                    "ALTER TABLE \"main\".{} ADD COLUMN {} {};",
                    crate::ddl::quote_ident(&before.name, Engine::Sqlite),
                    crate::ddl::quote_ident(&node.name, Engine::Sqlite),
                    declared.to_ascii_uppercase()
                ),
                rollback: None,
            });
        }
        if steps.len() != additions.len() {
            return unsupported(anchor, before);
        }
        if selected.contains(&parent_id) {
            result.tables.insert(parent_id);
        }
        for change in additions.iter().skip(1) {
            result.covered.insert(change.id.clone());
        }
        result.groups.insert(anchor.id.clone(), steps);
    }
    Ok(result)
}
