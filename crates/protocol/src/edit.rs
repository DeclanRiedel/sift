//! Inline-edit → DML generation (Phase D).
//!
//! Turns a set of result-grid edits (cell changes, new rows, deletes) into
//! minimal, parameterized `INSERT` / `UPDATE` / `DELETE` statements with a
//! preview step and a transactional apply that detects concurrent
//! modification. See `docs/PLANS/inline-edit-dml.md` (ADR-023 candidate).
//!
//! Pure serde: the actual SQL generation and transactional apply live in the
//! server (`crates/server/src/edit.rs`), composed over existing `Driver`
//! methods so the ADR-017 trait lock is undisturbed.

use serde::{Deserialize, Serialize};

use crate::{ConnectionId, ObjectPath, Row, TxHandleRef, Value};

/// A single column assignment or comparison: `column = value`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CellEdit {
    pub column: String,
    pub value: Value,
}

/// The identity of an existing row: the identity columns and the values the
/// client last saw for them. Used to build the `WHERE` clause that targets
/// exactly one row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RowKey {
    pub columns: Vec<CellEdit>,
}

/// One edit against a single table row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RowEdit {
    /// Insert a new row. Identity / auto-increment columns are omitted by the
    /// generator and assigned by the database.
    Insert { values: Vec<CellEdit> },
    /// Update the row identified by `key`. `changes` are the new values;
    /// `expected` carries the original values of columns to include in the
    /// optimistic-concurrency `WHERE` clause (empty = key-only match).
    Update {
        key: RowKey,
        changes: Vec<CellEdit>,
        #[serde(default)]
        expected: Vec<CellEdit>,
    },
    /// Delete the row identified by `key`. `expected` optionally adds original
    /// values to the `WHERE` clause for optimistic-concurrency safety.
    Delete {
        key: RowKey,
        #[serde(default)]
        expected: Vec<CellEdit>,
    },
}

/// A batch of edits against exactly one base table on one connection.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EditSet {
    /// Schema-qualified base table. `kind` should be `Table` (or absent).
    pub table: ObjectPath,
    pub edits: Vec<RowEdit>,
}

/// Where a table's row identity came from. Reported in the plan so the client
/// can gray out editing when a grid isn't backed by a stable key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum IdentitySource {
    PrimaryKey { columns: Vec<String> },
    UniqueIndex { name: String, columns: Vec<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EditStatementKind {
    Insert,
    Update,
    Delete,
}

/// One generated, parameterized statement. Multiple statements may derive from
/// a single [`RowEdit`]; `edit_index` maps back to the originating edit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EditStatement {
    pub edit_index: usize,
    pub kind: EditStatementKind,
    /// Parameterized SQL, engine-quoted. Bind placeholders match the engine
    /// (`$1` for Postgres, `@p1` for SQL Server).
    pub sql: String,
    pub params: Vec<Value>,
}

/// Result of a preview: the exact statements that *would* run, never executed.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EditPlan {
    pub table: ObjectPath,
    pub identity: IdentitySource,
    pub statements: Vec<EditStatement>,
}

/// Body of `POST /v1/sessions/:id/connections/:conn_id/edits/preview`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PreviewEditsRequest {
    pub connection: ConnectionId,
    pub edit_set: EditSet,
}

/// Body of `POST /v1/sessions/:id/connections/:conn_id/edits/apply`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ApplyEditsRequest {
    pub connection: ConnectionId,
    pub edit_set: EditSet,
    /// Optional caller-owned transaction to apply under. When present the
    /// server does not commit — the caller commits/rolls back. When absent the
    /// server wraps the whole edit set in its own transaction and commits on
    /// full success.
    #[serde(default)]
    pub tx: Option<TxHandleRef>,
}

/// Per-statement outcome after a successful apply.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EditOutcome {
    pub edit_index: usize,
    pub kind: EditStatementKind,
    pub affected_rows: u64,
    /// Rows returned by `RETURNING` / `OUTPUT` (e.g. a database-assigned
    /// identity key for an insert). Empty when the statement returns nothing.
    #[serde(default)]
    pub returned: Vec<Row>,
}

/// Result of `apply`. `committed` is false when the caller owns the `tx`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ApplyEditsResult {
    pub applied: Vec<EditOutcome>,
    pub committed: bool,
}
