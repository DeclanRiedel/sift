//! Result paging protocol. `Page::NextResult` carries the multi-result
//! batch boundary (SQL Server native, PG simple-query protocol); single-
//! statement queries emit one `NextResult` followed by `Rows` then `Done`.

use crate::ColumnMetadata;
use crate::Value;
use serde::{Deserialize, Serialize};

/// Opaque id by which the server references a server-side cursor. The
/// driver maps it to its own cursor / cancel token / row stream state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CursorId(pub u64);

impl CursorId {
    pub fn new(id: u64) -> Self {
        Self(id)
    }
}

impl std::fmt::Display for CursorId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A single row of values, positional (matched against the column layout
/// of the current result set as declared by the preceding `Page::NextResult`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Row {
    pub values: Vec<Value>,
}

impl Row {
    pub fn new(values: Vec<Value>) -> Self {
        Self { values }
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

/// One chunk of a result stream. The consumer reads pages from
/// `ResultSetStream::rows` until `Done` (or stream close).
///
/// - First message for each result set in a batch is `NextResult` declaring
///   its column layout.
/// - `Rows` carries a backpressure-bounded batch of rows.
/// - `Done` ends the stream (or the current result set in a multi-result
///   batch — followed by another `NextResult` if more result sets remain).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Page {
    Rows(Vec<Row>),
    NextResult {
        columns: Vec<ColumnMetadata>,
    },
    Error {
        error: crate::DriverError,
    },
    Done {
        /// Cumulative affected-row count for the whole batch, when known.
        affected_rows: Option<u64>,
        warnings: Vec<crate::DriverWarning>,
    },
}

/// Execute query request. SQL string + bind parameters (empty for now —
/// parameterized queries are FEATURES.md Tier 2 #33; the trait carries
/// the slot so adding them later is not a breaking change).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecuteRequest {
    pub sql: String,
    #[serde(default)]
    pub params: Vec<Value>,
}

impl ExecuteRequest {
    pub fn new(sql: impl Into<String>) -> Self {
        Self {
            sql: sql.into(),
            params: Vec::new(),
        }
    }
}
