//! Reviewed run-policy material. These types alone confer no execution authority.
//!
//! Store policy bodies encrypted: exact SQL, arguments and object names can be
//! private. Native admission still checks the original actor, source and lease.
use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AiDelegationPolicy {
    pub budget: AiDelegationBudget,
    pub rules: Vec<AiDelegationRule>,
}

impl std::fmt::Debug for AiDelegationPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AiDelegationPolicy")
            .field("budget", &self.budget)
            .field("rule_count", &self.rules.len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AiDelegationBudget {
    /// Includes reads, staging, status and failed tool requests, not just writes.
    pub max_tool_calls: u32,
    pub max_input_bytes: u64,
    /// Reserve a response bound before dispatch; uncertain work retains its debit.
    pub max_output_bytes: u64,
    pub max_duration_secs: u32,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AiDelegationRule {
    pub id: Uuid,
    pub max_uses: u32,
    pub action: AiDelegationActionRule,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AiDelegationActionRule {
    ExactSql {
        sql: String,
        params: Vec<crate::Value>,
    },
    /// Instance AND tenant opt-in are required independently of human review.
    /// This grants no bound on affected rows, triggers or external effects.
    ArbitrarySql {
        acknowledge_unbounded_effects: bool,
    },
    ExactRowEdits {
        edit_set: crate::EditSet,
    },
    ExactMigration {
        /// Canonical immutable plan content and changes, excluding generated
        /// journal IDs/expiry. Actual native plan identity/deadline still apply.
        contract_sha256: String,
        change_count: u32,
    },
    RowEdits {
        table: crate::ObjectPath,
        operations: Vec<crate::EditStatementKind>,
        write_columns: Vec<String>,
        max_submitted_rows: u32,
        max_total_submitted_rows: u32,
    },
    Migration {
        objects: Vec<AiDelegationSchemaObject>,
        changes: Vec<crate::SchemaChangeKind>,
        acknowledged_risks: Vec<crate::SchemaChangeRisk>,
        max_changes: u32,
        max_total_changes: u32,
    },
    QueryEdit {
        target: crate::AiQueryExecutionTarget,
        before_sha256: String,
        base_revision: u64,
        replacement: AiDelegationTextRule,
    },
    Extension {
        /// Canonical digest of descriptor, prepared contract AND tool context.
        /// Package/registration/schema/classification changes invalidate it.
        contract_sha256: String,
        arguments: AiDelegationArgumentRule,
        max_argument_bytes: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AiDelegationSchemaObject {
    pub kind: crate::CatalogNodeKind,
    pub qualified_name: String,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AiDelegationTextRule {
    Exact { text: String },
    Bounded { max_utf8_bytes: u32 },
}

/// Deliberately bounded, offline argument constraints, intersected with the
/// frozen tool's own input schema by its existing native admission owner.
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AiDelegationArgumentRule {
    Exact {
        value: serde_json::Value,
    },
    String {
        max_utf8_bytes: u32,
        /// Empty means any string within the explicit byte bound.
        one_of: Vec<String>,
    },
    Integer {
        min: i64,
        max: i64,
    },
    Boolean,
    Null,
    Array {
        max_items: u32,
        items: Box<AiDelegationArgumentRule>,
    },
    Object {
        /// All other properties are refused; optional fields remain constrained.
        fields: BTreeMap<String, AiDelegationArgumentRule>,
        required: Vec<String>,
    },
}
