//! Wire types for the autocomplete API (Phase D).
//!
//! The server exposes `POST /v1/sessions/:id/connections/:conn_id/complete`
//! taking [`CompletionRequest`] and returning [`CompletionResponse`].
//!
//! Behavior — parsing, ranking, engine-specific keyword tables — lives in
//! `sift-completion`. The protocol crate only defines shapes (ADR-004).

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

/// Client asks: "what can I complete at `cursor` in this SQL?"
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CompletionRequest {
    /// Raw SQL text as it currently sits in the editor. May be
    /// syntactically incomplete — the server's parser is tolerant.
    pub sql: String,
    /// Byte offset into `sql` where the cursor is. Clamped to
    /// `sql.len()` if out of range.
    pub cursor: u32,
    /// Cap on candidates returned. Server-side clamp is 200.
    #[serde(default)]
    pub limit: Option<u32>,
}

/// Server response: ranked candidates + the byte range they replace.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CompletionResponse {
    pub candidates: Vec<CompletionCandidate>,
    /// Byte range in the request SQL that the client should replace with
    /// `candidate.insert` when the user accepts a suggestion. Typically
    /// covers the current partial identifier.
    pub replaced_range: Range,
    /// Detected completion context. Included for client debugging and
    /// telemetry; clients that just render candidates can ignore it.
    pub context: CompletionContext,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Range {
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CompletionCandidate {
    /// Text shown in the completion list. For identifiers this is the raw
    /// name (unquoted); for keywords it's the upper-cased form.
    ///
    /// `Cow` so the ranker can hand back `&'static str` for the fixed
    /// keyword/function tables without allocating on every keystroke.
    pub label: Cow<'static, str>,
    /// Text to actually insert. May differ from `label` when the
    /// identifier requires engine-specific quoting (`"MyTable"` on PG,
    /// `[MyTable]` on SQL Server).
    pub insert: Cow<'static, str>,
    pub kind: CompletionKind,
    /// Optional inline hint — e.g. `text NOT NULL` for a column,
    /// `(a int) -> int` for a function.
    #[serde(default)]
    pub detail: Option<String>,
    /// Schema-qualified name (`"public.users"`) when applicable.
    #[serde(default)]
    pub qualified_name: Option<String>,
    /// Server-assigned rank score. Higher is better. Clients that render
    /// their own order can ignore; the `candidates` list is already sorted
    /// descending by score.
    pub score: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CompletionKind {
    Keyword,
    Function,
    Schema,
    Table,
    View,
    MaterializedView,
    Column,
    Alias,
    Procedure,
    Type,
}

/// The kind of SQL slot the cursor is in. Drives which candidates the
/// ranker surfaces first.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CompletionContext {
    /// Top of a statement or between statements — expect a leading
    /// keyword (`SELECT`, `INSERT`, ...).
    Statement,
    /// After `FROM`, `JOIN`, `UPDATE`, `INTO`, or `TABLE` — expect a
    /// table or view name (optionally schema-qualified).
    ExpectingTable,
    /// Inside an expression slot — expect columns and functions. When
    /// `qualifier` is `Some("t")` the cursor is right after `t.` and the
    /// candidates should be columns of the object `t` binds to (via
    /// alias resolution or direct table name).
    ExpectingColumn {
        #[serde(default)]
        qualifier: Option<String>,
    },
    /// Cursor is right after a schema-qualifier dot (`public.`) but not
    /// inside a select-list — expect objects in that schema.
    ExpectingObjectInSchema { schema: String },
    /// Unknown — fall back to a keyword-heavy ranking.
    Unknown,
}
