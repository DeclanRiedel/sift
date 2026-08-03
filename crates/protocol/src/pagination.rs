use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// One keyset-paginated collection page. Cursors are opaque to clients and
/// remain valid while newer rows are appended ahead of the current window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CursorPage<T> {
    pub items: Vec<T>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}
