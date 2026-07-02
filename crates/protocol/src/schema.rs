//! Schema introspection model. Progressive: `Shallow` returns names + kinds
//! only (used at session-open); `Deep` returns one object's columns, types,
//! indexes (used on tree-expand). Matches Zed lesson §2.2.

use crate::ColumnMetadata;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaScope {
    pub depth: SchemaDepth,
    #[serde(default)]
    pub filter: Option<SchemaFilter>,
}

impl SchemaScope {
    pub fn shallow() -> Self {
        Self {
            depth: SchemaDepth::Shallow,
            filter: None,
        }
    }

    pub fn deep(object: ObjectPath) -> Self {
        Self {
            depth: SchemaDepth::Deep { object },
            filter: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "depth", rename_all = "snake_case")]
pub enum SchemaDepth {
    /// Names only: catalogs → databases → schemas → object names + kinds.
    Shallow,
    /// One object fully described: columns, indexes, constraints.
    Deep { object: ObjectPath },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SchemaFilter {
    #[serde(default)]
    pub catalogs: Option<Vec<String>>,
    #[serde(default)]
    pub schemas: Option<Vec<String>>,
    #[serde(default)]
    pub kinds: Option<Vec<ObjectKind>>,
    /// Glob pattern matched against object names (`public.*`, `user_*`).
    #[serde(default)]
    pub name_pattern: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectPath {
    /// Catalog / database name. `None` for engines with a single catalog.
    #[serde(default)]
    pub catalog: Option<String>,
    /// Schema name. `None` for the engine default schema.
    #[serde(default)]
    pub schema: Option<String>,
    pub name: String,
    #[serde(default)]
    pub kind: Option<ObjectKind>,
}

impl ObjectPath {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            catalog: None,
            schema: None,
            name: name.into(),
            kind: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectKind {
    Table,
    View,
    MaterializedView,
    TableValuedFunction,
    ScalarFunction,
    Procedure,
    Synonym,
    Sequence,
    Trigger,
    Type,
    Extension,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaSnapshot {
    pub trees: Vec<CatalogTree>,
    pub fetched_at: chrono::DateTime<chrono::Utc>,
    pub scope: SchemaScope,
    /// True if the snapshot was truncated by `filter` or timed out mid-fetch.
    #[serde(default)]
    pub incomplete: bool,
}

impl SchemaSnapshot {
    pub fn empty(scope: SchemaScope) -> Self {
        Self {
            trees: Vec::new(),
            fetched_at: chrono::Utc::now(),
            scope,
            incomplete: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogTree {
    pub name: String,
    pub schemas: Vec<SchemaTree>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaTree {
    pub name: String,
    /// Populated only when `SchemaScope` requested objects in this schema.
    /// Empty otherwise (Shallow pass with no filter, or filter excluded it).
    #[serde(default)]
    pub objects: Vec<ObjectInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectInfo {
    pub name: String,
    pub kind: ObjectKind,
    /// Populated only for `SchemaDepth::Deep` requests targeting this object.
    #[serde(default)]
    pub columns: Vec<ColumnMetadata>,
}
