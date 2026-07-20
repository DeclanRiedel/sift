//! Schema & data search (Phase D).
//!
//! Two surfaces, per the design in `docs/PLANS/schema-data-search.md`
//! (ADR-024 candidate):
//!
//! - **Schema search**: fuzzy search over object + column *names*, served from
//!   an in-memory per-connection index. Fast (no DB round-trip on the hot
//!   path).
//! - **Data search**: bounded, live search over row *contents* across a chosen
//!   scope. Parameterized `LIKE`, hard-capped, cancellable.
//!
//! Pure serde: the index, ranking, and fan-out live in the server
//! (`crates/server/src/search.rs`).

use serde::{Deserialize, Serialize};

use crate::{ObjectKind, ObjectPath, Row};

// ---------------------------------------------------------------------------
// Schema search
// ---------------------------------------------------------------------------

/// Body of `POST /v1/sessions/:id/connections/:conn_id/search/schema`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SchemaSearchRequest {
    pub query: String,
    /// Restrict object hits to these kinds. `None` = all kinds. Column hits are
    /// unaffected.
    #[serde(default)]
    pub kinds: Option<Vec<ObjectKind>>,
    /// Maximum hits to return (server clamps to a hard ceiling).
    #[serde(default)]
    pub limit: Option<u32>,
}

/// What a [`SearchHit`] points at.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SearchTarget {
    /// A schema object (table/view/etc.). `object_kind` is its kind.
    Object { object_kind: ObjectKind },
    /// A column on a table; the column name is on [`SearchHit::column`].
    Column,
}

/// A single ranked schema-search hit.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SearchHit {
    pub target: SearchTarget,
    /// Schema-qualified object (the table, for a column hit).
    pub path: ObjectPath,
    /// Column name for a column hit; `None` for an object hit.
    #[serde(default)]
    pub column: Option<String>,
    /// Human display string that was matched, e.g. `public.users.email`.
    pub display: String,
    /// Fuzzy score; higher is a better match.
    pub score: i32,
    /// Rendered column type, for column hits.
    #[serde(default)]
    pub type_display: Option<String>,
    /// Byte ranges into `display` that matched, for client highlighting.
    #[serde(default)]
    pub match_ranges: Vec<(u32, u32)>,
}

/// Whether the per-connection search index is fully built yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IndexState {
    Ready,
    Building,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SchemaSearchResponse {
    pub hits: Vec<SearchHit>,
    pub index_state: IndexState,
}

// ---------------------------------------------------------------------------
// Data search
// ---------------------------------------------------------------------------

/// What set of tables a data search covers.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum DataSearchScope {
    /// A single table.
    Table { table: ObjectPath },
    /// Every table/view in a schema.
    Schema { schema: String },
    /// An explicit list of tables.
    Tables { tables: Vec<ObjectPath> },
}

/// Body of `POST /v1/sessions/:id/connections/:conn_id/search/data`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DataSearchRequest {
    pub scope: DataSearchScope,
    pub query: String,
    /// Max rows per table (server clamps to a hard ceiling).
    #[serde(default)]
    pub per_table_limit: Option<u32>,
    /// Max tables to search (server clamps to a hard ceiling).
    #[serde(default)]
    pub max_tables: Option<u32>,
    /// Restrict the search to these columns; default = all text-ish columns.
    #[serde(default)]
    pub columns: Option<Vec<String>>,
}

/// One matching row from a table.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DataSearchHit {
    pub table: ObjectPath,
    /// Column names describing `row`'s value layout.
    pub columns: Vec<String>,
    pub row: Row,
    /// Columns that were searched against (the candidate match set).
    pub matched_columns: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DataSearchResponse {
    pub hits: Vec<DataSearchHit>,
    /// True if a per-table row cap or the table cap dropped results.
    pub truncated: bool,
    /// How many tables were actually searched.
    pub tables_searched: u32,
}
