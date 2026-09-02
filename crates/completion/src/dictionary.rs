//! Denormalized index over a [`SchemaSnapshot`] for fast completion lookups.
//!
//! The ranker walks this once per request. Construction is cheap
//! (linear over the snapshot), so we don't try to cache it — the
//! upstream `SchemaCache` already deduplicates the expensive part
//! (fetching the snapshot from the DB).

use std::collections::{HashMap, HashSet};

use sift_protocol::{ObjectInfo, ObjectKind, ObjectPath, SchemaSnapshot};

/// A schema-qualified object in the connected database.
#[derive(Debug, Clone)]
pub struct ObjectEntry {
    pub catalog: Option<String>,
    pub schema: Option<String>,
    pub name: String,
    pub name_lower: String,
    pub kind: ObjectKind,
    pub routine_args: Option<Vec<String>>,
    pub comment: Option<String>,
    /// Populated only if the snapshot was fetched with `SchemaDepth::Deep`
    /// for this object. Empty otherwise.
    pub columns: Vec<ColumnEntry>,
}

#[derive(Debug, Clone)]
pub struct ColumnEntry {
    pub name: String,
    pub name_lower: String,
    /// Rendered type text — engine-native when known, otherwise a
    /// primitive-to-SQL mapping.
    pub type_display: String,
    pub type_ref: sift_protocol::TypeRef,
    pub nullable: sift_protocol::Nullability,
    pub ordinal: usize,
    pub not_null: bool,
    pub primary_key: bool,
}

/// Denormalized completion dictionary. Cheap to build; owns its data so
/// the ranker can hand `label` / `insert` strings to the response without
/// borrowing.
pub struct Dictionary {
    pub schemas: Vec<String>,
    pub objects: Vec<ObjectEntry>,
    /// `(schema.lower, name.lower) -> index into objects`. Enables O(1)
    /// alias resolution when the parser reports a qualifier.
    pub by_qualified: HashMap<(String, String), Vec<usize>>,
    /// `(catalog.lower, schema.lower, name.lower) -> object` for SQL Server
    /// three-part names and cross-catalog snapshots.
    pub by_catalog_qualified: HashMap<(String, String, String), usize>,
    /// Case-insensitive name → all object indices with that name. Used
    /// when a qualifier is unqualified (e.g. `SELECT u.foo FROM users u`
    /// after the alias resolves to `users` — we look `users` up here
    /// without knowing its schema).
    pub by_name: HashMap<String, Vec<usize>>,
    /// Object indices sorted by lowercased object name. Used for O(log n)
    /// prefix windows in the common table/object completion path.
    pub objects_by_name: Vec<usize>,
}

impl Dictionary {
    pub fn from_snapshot(snapshot: &SchemaSnapshot) -> Self {
        let mut schemas: Vec<String> = Vec::new();
        let mut seen_schemas = HashSet::new();
        let mut objects: Vec<ObjectEntry> = Vec::new();
        for catalog in &snapshot.trees {
            for schema in &catalog.schemas {
                if seen_schemas.insert(schema.name.to_ascii_lowercase()) {
                    schemas.push(schema.name.clone());
                }
                for obj in &schema.objects {
                    objects.push(object_entry(obj, Some(&catalog.name), Some(&schema.name)));
                }
            }
        }
        let by_qualified = build_qualified_index(&objects);
        let by_catalog_qualified = build_catalog_qualified_index(&objects);
        let by_name = build_name_index(&objects);
        let objects_by_name = build_sorted_name_index(&objects);
        Self {
            schemas,
            objects,
            by_qualified,
            by_catalog_qualified,
            by_name,
            objects_by_name,
        }
    }

    /// Resolve the object an unqualified name refers to, if unambiguous.
    /// Returns `None` when the name is absent or ambiguous across schemas.
    pub fn resolve_by_name(&self, name: &str) -> Option<&ObjectEntry> {
        let key = name.to_ascii_lowercase();
        let idxs = self.by_name.get(&key)?;
        if idxs.len() == 1 {
            Some(&self.objects[idxs[0]])
        } else {
            None
        }
    }

    /// Resolve `schema.name` (case-insensitive) to an object.
    pub fn resolve_qualified(&self, schema: &str, name: &str) -> Option<&ObjectEntry> {
        let key = (schema.to_ascii_lowercase(), name.to_ascii_lowercase());
        let matches = self.by_qualified.get(&key)?;
        (matches.len() == 1).then(|| &self.objects[matches[0]])
    }

    /// Resolve one-, two-, or three-part catalog references. Unqualified
    /// references resolve only when unique, preventing cross-schema leakage.
    pub fn resolve_reference(&self, reference: &str) -> Option<&ObjectEntry> {
        let parts = reference.split('.').collect::<Vec<_>>();
        match parts.as_slice() {
            [name] => self.resolve_by_name(name),
            [schema, name] => self.resolve_qualified(schema, name),
            [catalog, schema, name] => self
                .by_catalog_qualified
                .get(&(
                    catalog.to_ascii_lowercase(),
                    schema.to_ascii_lowercase(),
                    name.to_ascii_lowercase(),
                ))
                .map(|index| &self.objects[*index]),
            _ => None,
        }
    }

    /// Resolve an unqualified object name to the fully qualified path needed
    /// for a deep schema fetch. Returns `None` when absent or ambiguous.
    pub fn resolve_object_path(&self, name: &str) -> Option<ObjectPath> {
        let obj = self.resolve_reference(name)?;
        Some(ObjectPath {
            catalog: obj.catalog.clone(),
            schema: obj.schema.clone(),
            name: obj.name.clone(),
            kind: Some(obj.kind),
            routine_args: obj.routine_args.clone(),
        })
    }
}

fn object_entry(obj: &ObjectInfo, catalog: Option<&str>, schema: Option<&str>) -> ObjectEntry {
    let columns = obj
        .columns
        .iter()
        .enumerate()
        .map(|(ordinal, c)| ColumnEntry {
            name: c.name.clone(),
            name_lower: c.name.to_ascii_lowercase(),
            type_display: type_display(&c.type_ref),
            type_ref: c.type_ref.clone(),
            nullable: c.nullable,
            ordinal,
            not_null: matches!(c.nullable, sift_protocol::Nullability::NotNullable),
            primary_key: c.primary_key,
        })
        .collect();
    ObjectEntry {
        catalog: catalog.map(str::to_string),
        schema: schema.map(str::to_string),
        name: obj.name.clone(),
        name_lower: obj.name.to_ascii_lowercase(),
        kind: obj.kind,
        routine_args: obj.routine_args.clone(),
        comment: obj.comment.clone(),
        columns,
    }
}

fn build_sorted_name_index(objects: &[ObjectEntry]) -> Vec<usize> {
    let mut out: Vec<usize> = (0..objects.len()).collect();
    out.sort_by(|a, b| {
        objects[*a]
            .name_lower
            .cmp(&objects[*b].name_lower)
            .then_with(|| objects[*a].name.cmp(&objects[*b].name))
    });
    out
}

fn type_display(t: &sift_protocol::TypeRef) -> String {
    match t {
        sift_protocol::TypeRef::Native { name, .. } => name.clone(),
        sift_protocol::TypeRef::Primitive(p) => format!("{p:?}").to_ascii_lowercase(),
    }
}

fn build_qualified_index(objects: &[ObjectEntry]) -> HashMap<(String, String), Vec<usize>> {
    let mut out = HashMap::<_, Vec<_>>::new();
    for (i, o) in objects.iter().enumerate() {
        if let Some(s) = &o.schema {
            out.entry((s.to_ascii_lowercase(), o.name.to_ascii_lowercase()))
                .or_default()
                .push(i);
        }
    }
    out
}

fn build_catalog_qualified_index(
    objects: &[ObjectEntry],
) -> HashMap<(String, String, String), usize> {
    let mut out = HashMap::new();
    for (index, object) in objects.iter().enumerate() {
        if let (Some(catalog), Some(schema)) = (&object.catalog, &object.schema) {
            out.insert(
                (
                    catalog.to_ascii_lowercase(),
                    schema.to_ascii_lowercase(),
                    object.name.to_ascii_lowercase(),
                ),
                index,
            );
        }
    }
    out
}

fn build_name_index(objects: &[ObjectEntry]) -> HashMap<String, Vec<usize>> {
    let mut out: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, o) in objects.iter().enumerate() {
        out.entry(o.name.to_ascii_lowercase()).or_default().push(i);
    }
    out
}
