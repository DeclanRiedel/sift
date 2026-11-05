//! Derive private rename/add targets from a verified native declaration.
use super::*;

fn declaration(graph: &CatalogGraph, table: &CatalogNode) -> Option<String> {
    let mut columns = graph
        .data
        .nodes
        .iter()
        .filter(|node| node.parent_id.as_ref() == Some(&table.id))
        .collect::<Vec<_>>();
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
