use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{WorkspaceId, WorkspaceNodeId, WorkspaceRevision};

macro_rules! integer_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
        pub struct $name(pub i64);
    };
}

integer_id!(RunConfigurationId);
integer_id!(RunId);
integer_id!(ScheduleId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScriptRevisionPolicy {
    Pinned,
    LatestAtRunStart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunTransactionPolicy {
    None,
    PerScript,
    AllScripts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunErrorPolicy {
    Stop,
    Continue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunVariableKind {
    String,
    Integer,
    Decimal,
    Boolean,
    Identifier,
    Secret,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunVariableDefinition {
    pub name: String,
    pub kind: RunVariableKind,
    pub required: bool,
    pub persist_non_secret_value: bool,
    pub secret_handle_present: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunScriptStep {
    pub node_id: WorkspaceNodeId,
    pub revision_policy: ScriptRevisionPolicy,
    pub pinned_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunConfiguration {
    pub id: RunConfigurationId,
    pub workspace_id: WorkspaceId,
    pub name: String,
    pub scripts: Vec<RunScriptStep>,
    pub connection_profile_id: i64,
    pub target_schema: Option<String>,
    pub variables: Vec<RunVariableDefinition>,
    pub transaction_policy: RunTransactionPolicy,
    pub error_policy: RunErrorPolicy,
    pub revision: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunTrigger {
    Interactive,
    Schedule,
    Rerun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Queued,
    Admitted,
    Preparing,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    OutcomeUnknown,
    Blocked,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunManifestScript {
    pub node_id: WorkspaceNodeId,
    pub content_digest: String,
    pub document_frontier_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunManifest {
    pub workspace_revision: WorkspaceRevision,
    pub scripts: Vec<RunManifestScript>,
    pub connection_profile_id: i64,
    pub target_schema: Option<String>,
    pub provider_id: String,
    pub variable_names: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Run {
    pub id: RunId,
    pub configuration_id: RunConfigurationId,
    pub trigger: RunTrigger,
    pub actor_principal_id: i64,
    pub state: RunState,
    pub manifest: RunManifest,
    pub previous_run_id: Option<RunId>,
    pub revision: u64,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleMisfirePolicy {
    Skip,
    RunOnce,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScheduleConcurrencyPolicy {
    Forbid,
    QueueOne,
    Parallel { maximum: u16 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunSchedule {
    pub id: ScheduleId,
    pub configuration_id: RunConfigurationId,
    pub owner_principal_id: i64,
    pub cron: String,
    pub timezone: String,
    pub misfire_policy: ScheduleMisfirePolicy,
    pub concurrency_policy: ScheduleConcurrencyPolicy,
    pub enabled: bool,
    pub next_fire_at: Option<DateTime<Utc>>,
    pub revision: u64,
}
