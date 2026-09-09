use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "check", rename_all = "snake_case", deny_unknown_fields)]
pub enum IntegrityCheckRequest {
    /// Checks the main SQLite database, not attached databases or foreign keys.
    Sqlite {
        #[serde(default)]
        quick: bool,
    },
    /// Requires an already-installed amcheck extension. Heap only, not indexes/TOAST.
    PostgresHeap { schema: String, name: String },
    /// Checks the current database only. Never requests a repair mode.
    SqlServer {
        #[serde(default)]
        physical_only: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IntegrityOutcome {
    NoIssuesReported,
    IssuesReported,
    Incomplete,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct IntegrityCheckReport {
    pub check: IntegrityCheckRequest,
    /// NoIssuesReported is scoped to this check, not proof of database health.
    pub outcome: IntegrityOutcome,
    pub findings: Vec<String>,
    pub warnings: Vec<crate::DriverWarning>,
}
