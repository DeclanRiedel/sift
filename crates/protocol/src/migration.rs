//! Engine-aware migration preview and apply contract (ADR-033).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    CatalogRevision, ConnectionId, SchemaChangeId, SchemaChangeRisk, SchemaDiffRequest, SessionId,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct MigrationPlanId(pub Uuid);

impl std::fmt::Display for MigrationPlanId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct MigrationRunId(pub Uuid);

impl std::fmt::Display for MigrationRunId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MigrationOptions {
    #[serde(default = "default_true")]
    pub prefer_transactional: bool,
    #[serde(default)]
    pub online_indexes: bool,
}

impl Default for MigrationOptions {
    fn default() -> Self {
        Self {
            prefer_transactional: true,
            online_indexes: false,
        }
    }
}

const fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PreviewMigrationRequest {
    pub diff: SchemaDiffRequest,
    pub expected_diff_digest: String,
    #[serde(default)]
    pub selected_changes: Vec<SchemaChangeId>,
    pub expected_live_revision: CatalogRevision,
    #[serde(default)]
    pub options: MigrationOptions,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MigrationStatement {
    pub ordinal: u32,
    pub sql: String,
    pub fingerprint: String,
    #[serde(default)]
    pub change_ids: Vec<SchemaChangeId>,
    pub risk: SchemaChangeRisk,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MigrationGroup {
    pub ordinal: u32,
    pub transactional: bool,
    #[serde(default)]
    pub statements: Vec<MigrationStatement>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MigrationPlan {
    pub id: MigrationPlanId,
    pub run_id: MigrationRunId,
    pub digest: String,
    pub diff_digest: String,
    pub expected_live_revision: CatalogRevision,
    pub groups: Vec<MigrationGroup>,
    #[serde(default)]
    pub required_acknowledgements: Vec<SchemaChangeRisk>,
    #[serde(default)]
    pub warnings: Vec<String>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApplyMigrationRequest {
    pub plan_id: MigrationPlanId,
    pub plan_digest: String,
    #[serde(default)]
    pub acknowledgements: Vec<SchemaChangeRisk>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MigrationStatementStatus {
    Applied,
    RolledBack,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MigrationStatementOutcome {
    pub group_ordinal: u32,
    pub statement_ordinal: u32,
    pub fingerprint: String,
    pub status: MigrationStatementStatus,
    #[serde(default)]
    pub affected_rows: Option<u64>,
    #[serde(default)]
    pub result_code: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MigrationRunState {
    Running,
    Applied,
    RolledBack,
    Partial,
    Canceled,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MigrationRun {
    pub id: MigrationRunId,
    pub plan_id: MigrationPlanId,
    pub session: SessionId,
    pub connection: ConnectionId,
    pub plan_digest: String,
    pub state: MigrationRunState,
    pub started_at: chrono::DateTime<chrono::Utc>,
    #[serde(default)]
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub outcomes: Vec<MigrationStatementOutcome>,
    #[serde(default)]
    pub resulting_catalog_revision: Option<CatalogRevision>,
}
