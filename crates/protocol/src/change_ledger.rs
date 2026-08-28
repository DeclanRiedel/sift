use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{WorkspaceCheckpointId, WorkspaceId, WorkspacePath, WorkspaceRevision};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChangeLedgerOperation {
    GridInsert,
    GridUpdate,
    GridDelete,
    DirectDml,
    DdlApply,
    MigrationApply,
    MigrationRollback,
    CsvImport,
    BulkMutation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChangeLedgerOutcome {
    Committed,
    Failed,
    Conflicted,
    RolledBack,
    Partial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChangeIdentitySource {
    Sift,
    Postgres,
    SqlServer,
    External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChangeIdentityConfidence {
    Authenticated,
    DatabaseNative,
    Mapped,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VersionedExecutionContext {
    pub workspace_id: WorkspaceId,
    pub workspace_revision: WorkspaceRevision,
    #[serde(default)]
    pub checkpoint_id: Option<WorkspaceCheckpointId>,
    #[serde(default)]
    pub path: Option<WorkspacePath>,
    #[serde(default)]
    pub git_commit: Option<String>,
    #[serde(default)]
    pub source_workflow: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChangeLedgerEntry {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub tenant_id: Option<i64>,
    pub room_id: Option<i64>,
    pub connection_profile_id: Option<i64>,
    pub database_target: Option<String>,
    pub operation: ChangeLedgerOperation,
    pub affected_object: Option<String>,
    pub row_count: Option<i64>,
    pub sql_fingerprint: Option<String>,
    pub row_identity_fingerprint: Option<String>,
    pub transaction_id: Option<String>,
    pub correlation_id: Option<String>,
    pub workspace_id: Option<WorkspaceId>,
    pub workspace_revision: Option<WorkspaceRevision>,
    pub checkpoint_id: Option<WorkspaceCheckpointId>,
    pub workspace_path: Option<WorkspacePath>,
    pub git_commit: Option<String>,
    pub source_workflow: String,
    pub authored_by: Option<i64>,
    pub approved_by: Option<i64>,
    pub executed_by: i64,
    /// Database-native identity for imported audit/CDC events. Never replaces
    /// the authenticated Sift executor identity.
    pub database_actor: Option<String>,
    pub outcome: ChangeLedgerOutcome,
    pub result_code: Option<String>,
    pub identity_source: ChangeIdentitySource,
    pub identity_confidence: ChangeIdentityConfidence,
    pub previous_hash: String,
    pub entry_hash: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChangeLedgerFilter {
    pub tenant_id: Option<i64>,
    pub connection_profile_id: Option<i64>,
    pub database_target: Option<String>,
    pub affected_object: Option<String>,
    pub executed_by: Option<i64>,
    pub operation: Option<ChangeLedgerOperation>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub git_commit: Option<String>,
    pub before_id: Option<i64>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChangeLedgerPage {
    pub entries: Vec<ChangeLedgerEntry>,
    pub next_before_id: Option<i64>,
    pub chain_verified: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExternalChangeLedgerEvent {
    pub tenant_id: i64,
    pub connection_profile_id: Option<i64>,
    pub database_target: Option<String>,
    pub operation: ChangeLedgerOperation,
    pub affected_object: Option<String>,
    pub row_count: Option<i64>,
    pub sql_fingerprint: Option<String>,
    pub row_identity_fingerprint: Option<String>,
    pub transaction_id: Option<String>,
    pub correlation_id: Option<String>,
    pub database_actor: String,
    pub outcome: ChangeLedgerOutcome,
    pub result_code: Option<String>,
    pub identity_source: ChangeIdentitySource,
    pub identity_confidence: ChangeIdentityConfidence,
}
