//! Prove removal plus its table witness and induced ordinal compression.
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
    let mut groups: HashMap<sift_protocol::CatalogObjectId, Vec<&SchemaChange>> = HashMap::new();
    for change in diff
        .changes
        .iter()
        .filter(|change| selected.contains(&change.id) && change.kind == SchemaChangeKind::Drop)
    {
        let Some(node) = change
            .object_before
            .as_ref()
            .filter(|node| node.kind == CatalogNodeKind::Column)
        else {
            continue;
        };
        let parent = node
            .parent_id
            .as_ref()
            .ok_or_else(|| MigrationRenderError::IncompleteHierarchy(change.id.clone()))?;
        groups.entry(parent.clone()).or_default().push(change);
    }
    if let Some(anchor) = groups.values().find_map(|drops| drops.first()).copied() {
        validate_sqlite_rename_catalog(anchor, from_nodes, to_nodes, from, to)?;
    }
    let mut result = ColumnRenames::default();
    for (parent_id, mut drops) in groups {
        drops.sort_by_key(|change| change.object_before.as_ref().unwrap().ordinal);
        let anchor = drops[0];
        let before = from_nodes
            .get(&parent_id)
            .copied()
            .ok_or_else(|| MigrationRenderError::IncompleteHierarchy(anchor.id.clone()))?;
        let after = to_nodes
            .get(&parent_id)
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
        let old = children(from, &parent_id);
        let new = children(to, &parent_id);
        if new.is_empty() || new.len() >= old.len() {
            return unsupported(anchor, before);
        }
        let parent = diff
            .changes
            .iter()
            .find(|change| {
                change.kind == SchemaChangeKind::Alter
                    && change
                        .object_before
                        .as_ref()
                        .is_some_and(|node| node.id == parent_id)
                    && change
                        .object_after
                        .as_ref()
                        .is_some_and(|node| node.id == parent_id)
            })
            .ok_or_else(|| MigrationRenderError::IncompleteHierarchy(anchor.id.clone()))?;
        let mut retained = new.iter();
        let mut next = retained.next();
        let mut all_drops = Vec::new();
        let mut companions = vec![parent.id.clone()];
        for old in old {
            if next.is_some_and(|node| node.id == old.id) {
                let new = next.unwrap();
                let mut normalized = (*new).clone();
                normalized.ordinal = old.ordinal;
                if old.name != new.name
                    || old.qualified_name != new.qualified_name
                    || shape(old)? != shape(&normalized)?
                {
                    return unsupported(anchor, old);
                }
                if old.ordinal != new.ordinal {
                    let change = diff
                        .changes
                        .iter()
                        .find(|change| {
                            change.kind == SchemaChangeKind::Alter
                                && change
                                    .object_before
                                    .as_ref()
                                    .is_some_and(|node| node.id == old.id)
                                && change
                                    .object_after
                                    .as_ref()
                                    .is_some_and(|node| node.id == new.id)
                        })
                        .ok_or_else(|| {
                            MigrationRenderError::InvalidChangeShape(anchor.id.clone())
                        })?;
                    companions.push(change.id.clone());
                }
                next = retained.next();
            } else {
                let change = diff
                    .changes
                    .iter()
                    .find(|change| {
                        change.kind == SchemaChangeKind::Drop
                            && change
                                .object_before
                                .as_ref()
                                .is_some_and(|node| node.id == old.id)
                    })
                    .ok_or_else(|| MigrationRenderError::InvalidChangeShape(anchor.id.clone()))?;
                all_drops.push(change);
            }
        }
        if next.is_some() {
            return unsupported(anchor, after);
        }
        // A selected table/ordinal change describes the full target, so partial
        // removals cannot claim it. Column-only choices may remove any subset.
        if companions.iter().any(|id| selected.contains(id)) {
            if let Some(missing) = all_drops
                .iter()
                .find(|change| !selected.contains(&change.id))
            {
                return Err(MigrationRenderError::MissingPrerequisite(
                    missing.id.clone(),
                ));
            }
        }
        result
            .covered
            .extend(companions.into_iter().filter(|id| selected.contains(id)));
        let steps = drops
            .iter()
            .map(|change| Step {
                change: change.id.clone(),
                forward: format!(
                    "ALTER TABLE \"main\".{} DROP COLUMN {};",
                    crate::ddl::quote_ident(&before.name, Engine::Sqlite),
                    crate::ddl::quote_ident(
                        &change.object_before.as_ref().unwrap().name,
                        Engine::Sqlite
                    )
                ),
                rollback: None,
            })
            .collect();
        result
            .covered
            .extend(drops.iter().skip(1).map(|change| change.id.clone()));
        result.groups.insert(anchor.id.clone(), steps);
    }
    Ok(result)
}
