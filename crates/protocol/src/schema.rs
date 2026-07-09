//! Schema introspection model. Progressive: `Shallow` returns names + kinds
//! only (used at session-open); `Deep` returns one object's columns, types,
//! indexes (used on tree-expand). Matches Zed lesson §2.2.

use crate::ColumnMetadata;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "depth", rename_all = "snake_case")]
pub enum SchemaDepth {
    /// Names only: catalogs → databases → schemas → object names + kinds.
    Shallow,
    /// One object fully described: columns, indexes, constraints.
    Deep { object: ObjectPath },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ObjectKind {
    Table,
    View,
    MaterializedView,
    /// PG foreign table (relkind 'f').
    ForeignTable,
    /// PG partitioned table (relkind 'p').
    PartitionedTable,
    TableValuedFunction,
    ScalarFunction,
    Procedure,
    Synonym,
    Sequence,
    Trigger,
    Type,
    Extension,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintKind {
    PrimaryKey,
    ForeignKey,
    Unique,
    Check,
    Exclusion,
    /// Engine-native constraint not in the union above (e.g. PG `NOT VALID`,
    /// SQL Server `DEFAULT` bound to a rule).
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ConstraintInfo {
    pub name: String,
    pub kind: ConstraintKind,
    /// Column names participating in the constraint. Empty for table-level
    /// constraints that don't reference columns (rare).
    #[serde(default)]
    pub columns: Vec<String>,
    /// The constraint definition as the engine reports it (e.g. the CHECK
    /// expression, the FK references). Rendered verbatim by the client.
    #[serde(default)]
    pub definition: Option<String>,
    /// For FKs: the referenced table.
    #[serde(default)]
    pub references: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IndexKind {
    Btree,
    Hash,
    Gist,
    Gin,
    Brin,
    Spgist,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct IndexInfo {
    pub name: String,
    /// Columns covered by the index, in index order. Expressions (computed
    /// indexes) surface as the engine's textual form rather than a column
    /// name.
    #[serde(default)]
    pub columns: Vec<String>,
    pub unique: bool,
    pub primary_key: bool,
    pub kind: IndexKind,
    /// Partial-index predicate (PG `WHERE ...`), if any.
    #[serde(default)]
    pub partial_predicate: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TriggerTiming {
    Before,
    After,
    InsteadOf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TriggerEvent {
    Insert,
    Update,
    Delete,
    Truncate,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TriggerInfo {
    pub name: String,
    pub timing: TriggerTiming,
    pub events: Vec<TriggerEvent>,
    /// Per-column triggers (e.g. UPDATE OF col1, col2). Empty if not scoped.
    #[serde(default)]
    pub columns: Vec<String>,
    #[serde(default)]
    pub definition: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CatalogTree {
    pub name: String,
    pub schemas: Vec<SchemaTree>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SchemaTree {
    pub name: String,
    /// Populated only when `SchemaScope` requested objects in this schema.
    /// Empty otherwise (Shallow pass with no filter, or filter excluded it).
    #[serde(default)]
    pub objects: Vec<ObjectInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ObjectInfo {
    pub name: String,
    pub kind: ObjectKind,
    /// Populated only for `SchemaDepth::Deep` requests targeting this object.
    #[serde(default)]
    pub columns: Vec<ColumnMetadata>,
    /// Populated only for `SchemaDepth::Deep` requests on tables/views.
    #[serde(default)]
    pub indexes: Vec<IndexInfo>,
    /// Populated only for `SchemaDepth::Deep` requests on tables/views.
    #[serde(default)]
    pub constraints: Vec<ConstraintInfo>,
    /// Populated only for `SchemaDepth::Deep` requests on tables/views.
    #[serde(default)]
    pub triggers: Vec<TriggerInfo>,
}

impl ObjectInfo {
    pub fn new(name: impl Into<String>, kind: ObjectKind) -> Self {
        Self {
            name: name.into(),
            kind,
            columns: Vec::new(),
            indexes: Vec::new(),
            constraints: Vec::new(),
            triggers: Vec::new(),
        }
    }
}

/// DDL text for a database object, plus the resolved object path.
/// Response body of the DDL generation endpoint (Phase D).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ObjectDdl {
    pub path: ObjectPath,
    /// Complete CREATE statement (for tables, includes any owned
    /// indexes and triggers as separate statements separated by `;\n`).
    pub ddl: String,
}
