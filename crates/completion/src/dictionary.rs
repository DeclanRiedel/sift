//! Denormalized index over a [`SchemaSnapshot`] for fast completion lookups.
//!
//! The ranker walks this once per request. Construction is cheap
//! (linear over the snapshot), so we don't try to cache it — the
//! upstream `SchemaCache` already deduplicates the expensive part
//! (fetching the snapshot from the DB).

use std::collections::HashMap;

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
    pub by_qualified: HashMap<(String, String), usize>,
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
        let mut objects: Vec<ObjectEntry> = Vec::new();
        for catalog in &snapshot.trees {
            for schema in &catalog.schemas {
                if !schemas.iter().any(|s| s.eq_ignore_ascii_case(&schema.name)) {
                    schemas.push(schema.name.clone());
                }
                for obj in &schema.objects {
                    objects.push(object_entry(obj, Some(&catalog.name), Some(&schema.name)));
                }
            }
        }
        let by_qualified = build_qualified_index(&objects);
        let by_name = build_name_index(&objects);
        let objects_by_name = build_sorted_name_index(&objects);
        Self {
            schemas,
            objects,
            by_qualified,
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
        self.by_qualified.get(&key).map(|i| &self.objects[*i])
    }

    /// Resolve an unqualified object name to the fully qualified path needed
    /// for a deep schema fetch. Returns `None` when absent or ambiguous.
    pub fn resolve_object_path(&self, name: &str) -> Option<ObjectPath> {
        let obj = self.resolve_by_name(name)?;
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
        .map(|c| ColumnEntry {
            name: c.name.clone(),
            name_lower: c.name.to_ascii_lowercase(),
            type_display: type_display(&c.type_ref),
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
        sift_protocol::TypeRef::Engine { name, .. } => name.clone(),
        sift_protocol::TypeRef::Primitive(p) => format!("{p:?}").to_ascii_lowercase(),
    }
}

fn build_qualified_index(objects: &[ObjectEntry]) -> HashMap<(String, String), usize> {
    let mut out = HashMap::new();
    for (i, o) in objects.iter().enumerate() {
        if let Some(s) = &o.schema {
            out.insert((s.to_ascii_lowercase(), o.name.to_ascii_lowercase()), i);
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
