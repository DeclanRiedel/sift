//! Explicit, bounded query benchmarking. Timings are server-observed and exclude
//! network-to-desktop time and UI rendering. Native database time is optional.
use serde::{Deserialize, Serialize};

/// Portable measurement artifact, not execution authority or provenance proof.
/// Version 1 wraps report versions 1/2. Saved IDs, owners and names are omitted.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkArtifact {
    pub artifact_version: u32,
    pub sql_redacted: bool,
    pub report: BenchmarkReport,
}

/// User-saved snapshot, not a server attestation or a rerunnable definition.
/// Bind values are deliberately absent. SQL and names may be sensitive.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveBenchmarkRunRequest {
    /// Explicit private caller-associated Profile evidence, separate from samples.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<BenchmarkProfileAttachment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_context: Option<BenchmarkSourceContext>,
    pub name: String,
    pub report: BenchmarkReport,
}

/// Private caller-associated instrumentation, never a benchmark timing sample or
/// provenance proof. Raw plans can contain sensitive literals and identifiers.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkProfileAttachment {
    pub captured_at: chrono::DateTime<chrono::Utc>,
    pub sql: String,
    pub parameter_count: usize,
    pub response: crate::ProfileResponse,
}
impl BenchmarkProfileAttachment {
    pub fn validate_for(&self, report: &BenchmarkReport) -> Result<(), &'static str> {
        if self.sql != report.sql
            || self.parameter_count != report.parameter_count
            || self.response.plan.engine != report.engine
            || self.response.run_id == report.run_id
        {
            return Err("attached Profile must match SQL, engine and parameter count and have a separate run ID");
        }
        let plan = &self.response.plan;
        if plan.raw.is_empty()
            || plan.raw.len() > 1024 * 1024
            || plan.analyzed != (plan.engine != crate::Engine::Sqlite)
        {
            return Err(
                "attached Profile needs bounded raw plan and correct estimated/analyzed state",
            );
        }
        let valid = |value: Option<f64>| value.map_or(true, |v| v.is_finite() && v >= 0.0);
        if !valid(self.response.planning_ms)
            || !valid(self.response.execution_ms)
            || plan.engine == crate::Engine::Sqlite
                && (self.response.planning_ms.is_some() || self.response.execution_ms.is_some())
        {
            return Err("attached Profile has invalid or unavailable native clocks");
        }
        let mut pending = vec![(&plan.root, 1usize)];
        let mut count = 0;
        let mut attributes = 0;
        while let Some((node, depth)) = pending.pop() {
            count += 1;
            if count > 4096 || depth > 64 {
                return Err("attached Profile tree exceeds 4096 nodes or 64 levels");
            }
            if !valid(node.est_rows)
                || !valid(node.est_cost)
                || !valid(node.actual_rows)
                || !valid(node.actual_ms)
                || plan.engine == crate::Engine::Sqlite
                    && (node.actual_rows.is_some() || node.actual_ms.is_some())
            {
                return Err("attached Profile has invalid or unavailable node counters");
            }
            if node.children.len() + pending.len() + count > 4096 {
                return Err("attached Profile tree exceeds 4096 nodes");
            }
            pending.extend(node.children.iter().map(|child| (child, depth + 1)));
            let mut values = node
                .extra
                .values()
                .map(|value| (value, 1usize))
                .collect::<Vec<_>>();
            while let Some((value, level)) = values.pop() {
                attributes += 1;
                if level > 64 || attributes > 65536 {
                    return Err("attached Profile attributes exceed traversal limits");
                }
                match value {
                    serde_json::Value::Array(array) => {
                        values.extend(array.iter().map(|v| (v, level + 1)))
                    }
                    serde_json::Value::Object(object) => {
                        values.extend(object.values().map(|v| (v, level + 1)))
                    }
                    _ => {}
                }
            }
        }
        if serde_json::to_vec(self)
            .map_err(|_| "attached Profile cannot be encoded")?
            .len()
            > 2 * 1024 * 1024
        {
            return Err("attached Profile exceeds 2 MiB");
        }
        Ok(())
    }
}

/// Optional client-reported source observation. Never execution authority or a
/// server attestation. Kept outside measurement reports and portable artifacts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkSourceContext {
    pub captured_at: chrono::DateTime<chrono::Utc>,
    pub instance_id: String,
    pub workspace_id: crate::WorkspaceId,
    pub workspace_revision: crate::WorkspaceRevision,
    pub path: crate::WorkspacePath,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<BenchmarkSourceGit>,
}

/// A cached status observation, with its own age/revision and explicit coverage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkSourceGit {
    pub observed_at: chrono::DateTime<chrono::Utc>,
    pub workspace_revision: crate::WorkspaceRevision,
    pub head_oid: Option<String>,
    pub branch: Option<String>,
    pub dirty: Option<bool>,
    pub truncated: bool,
}
impl BenchmarkSourceContext {
    pub fn validate(&self) -> Result<(), &'static str> {
        let printable = |value: &str, max: usize| {
            !value.is_empty()
                && value.len() <= max
                && value.trim() == value
                && !value.chars().any(char::is_control)
        };
        if !printable(&self.instance_id, 200)
            || self.workspace_id.0 <= 0
            || !self.path.is_valid()
            || self.path.0.contains(':')
            || self.path.0.chars().any(char::is_control)
        {
            return Err("invalid benchmark instance, workspace or relative document path");
        }
        if let Some(git) = &self.git {
            if git.head_oid.as_ref().is_some_and(|oid| {
                !matches!(oid.len(), 40 | 64) || !oid.bytes().all(|byte| byte.is_ascii_hexdigit())
            }) || git
                .branch
                .as_ref()
                .is_some_and(|branch| !printable(branch, 255))
                || git.truncated && git.dirty == Some(false)
            {
                return Err("invalid benchmark Git observation or unsupported clean claim");
            }
        }
        Ok(())
    }
}

/// Owner-private commentary. Measurements remain unchanged when this is edited.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkRunAnnotations {
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateBenchmarkRunAnnotationsRequest {
    pub expected_revision: u64,
    pub notes: String,
    pub tags: Vec<String>,
}
impl UpdateBenchmarkRunAnnotationsRequest {
    pub fn normalize(&mut self) -> Result<(), &'static str> {
        if self.notes.len() > 8192
            || self
                .notes
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
        {
            return Err("notes must be at most 8 KiB without unsupported controls");
        }
        if self.tags.len() > 16 {
            return Err("at most 16 tags are allowed");
        }
        for tag in &mut self.tags {
            *tag = tag.trim().to_string();
            if tag.is_empty() || tag.len() > 64 || tag.chars().any(|c| c.is_control() || c == ',') {
                return Err("tags must be 1–64 bytes without controls or commas");
            }
        }
        self.tags.sort_unstable();
        self.tags.dedup();
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SavedBenchmarkRun {
    /// Explicit private caller-associated Profile evidence, separate from samples.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<BenchmarkProfileAttachment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_context: Option<BenchmarkSourceContext>,
    pub id: uuid::Uuid,
    pub saved_at: chrono::DateTime<chrono::Utc>,
    pub name: String,
    pub report: BenchmarkReport,
    #[serde(default)]
    pub annotations: BenchmarkRunAnnotations,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SavedBenchmarkRunSummary {
    pub id: uuid::Uuid,
    pub saved_at: chrono::DateTime<chrono::Utc>,
    pub name: String,
    pub engine: Option<crate::Engine>,
    pub payload_available: bool,
    #[serde(default)]
    pub tags: Vec<String>,
    pub completed: bool,
    pub median_ns: Option<f64>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListBenchmarkRunsRequest {
    pub cursor: Option<uuid::Uuid>,
    pub limit: Option<u32>,
}

/// Reusable private workload. Bind values are supplied anew for every run.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveBenchmarkDefinitionRequest {
    pub name: String,
    pub engine: crate::Engine,
    pub sql: String,
    pub parameter_count: u32,
    pub limits: BenchmarkLimits,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateBenchmarkDefinitionRequest {
    pub expected_revision: u64,
    pub definition: SaveBenchmarkDefinitionRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SavedBenchmarkDefinition {
    pub id: uuid::Uuid,
    pub revision: u64,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub name: String,
    pub engine: crate::Engine,
    pub sql: String,
    pub parameter_count: u32,
    pub limits: BenchmarkLimits,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SavedBenchmarkDefinitionSummary {
    pub id: uuid::Uuid,
    pub revision: u64,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub name: String,
    pub engine: Option<crate::Engine>,
    pub parameter_count: u32,
    pub payload_available: bool,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListBenchmarkDefinitionsRequest {
    pub cursor: Option<uuid::Uuid>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunBenchmarkDefinitionRequest {
    pub tenant_id: i64,
    pub run_id: uuid::Uuid,
    /// Revision reviewed by the caller. A changed workload must be reviewed again.
    pub expected_revision: u64,
    #[serde(default)]
    pub params: Vec<crate::Value>,
    pub workload_confirmed: bool,
}

/// Serial-run budgets. Execution must additionally enforce deployment policy,
/// query permissions and read-only protections; these limits are not a sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkLimits {
    pub warmups: u32,
    pub iterations: u32,
    pub query_timeout_ms: u64,
    pub total_budget_ms: u64,
    pub delay_ms: u64,
}

impl Default for BenchmarkLimits {
    fn default() -> Self {
        Self {
            warmups: 2,
            iterations: 10,
            query_timeout_ms: 30_000,
            total_budget_ms: 120_000,
            delay_ms: 0,
        }
    }
}

impl BenchmarkLimits {
    /// Hard ceilings bound the future runner's sample count and wall time.
    /// The total budget may intentionally stop a run before all iterations.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.warmups > 100 {
            return Err("warm-ups must not exceed 100");
        }
        if !(1..=10_000).contains(&self.iterations) {
            return Err("measured iterations must be between 1 and 10000");
        }
        if !(1..=3_600_000).contains(&self.total_budget_ms) {
            return Err("total budget must be between 1 ms and one hour");
        }
        if self.query_timeout_ms == 0 || self.query_timeout_ms > self.total_budget_ms {
            return Err("query timeout must be positive and no greater than the total budget");
        }
        if self.delay_ms >= self.total_budget_ms {
            return Err("inter-query delay must be smaller than the total budget");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkRequest {
    pub run_id: uuid::Uuid,
    pub sql: String,
    #[serde(default)]
    pub params: Vec<crate::Value>,
    pub warmups: u32,
    pub iterations: u32,
    pub query_timeout_ms: u64,
    pub total_budget_ms: u64,
    #[serde(default)]
    pub delay_ms: u64,
    /// Explicit acknowledgement that repeated reads can load the database and
    /// invoke externally visible functions. Does not override server policy.
    pub workload_confirmed: bool,
}

/// Active-run progress has no terminal meaning; only the original result ends a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PerformanceRunKind {
    Benchmark,
    Profile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PerformanceStage {
    SettingUp,
    WarmingUp,
    Measuring,
    Delay,
    Profiling,
    CleaningUp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BenchmarkProgressCounters {
    pub requested_warmups: u32,
    pub requested_iterations: u32,
    /// Completed attempts, including unsuccessful attempts; never a latency population.
    pub finished_warmups: u32,
    pub finished_iterations: u32,
    pub last_outcome: Option<BenchmarkOutcome>,
}

/// Fixed-size, row-free observation of an exact active run on its source connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PerformanceProgress {
    pub run_id: uuid::Uuid,
    pub kind: PerformanceRunKind,
    pub stage: PerformanceStage,
    /// Monotonic server elapsed time including setup/cleanup, not sample latency.
    pub elapsed_ms: u64,
    pub benchmark: Option<BenchmarkProgressCounters>,
    pub cancellation_requested: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkOutcome {
    Success,
    Failed,
    TimedOut,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BenchmarkSample {
    pub ordinal: u32,
    pub warmup: bool,
    pub outcome: BenchmarkOutcome,
    /// Legacy server-side client elapsed clock. Version 2 summaries still use
    /// this dimension so saved version 1 reports remain comparable.
    pub elapsed_ns: u64,
    /// Native database execution measurement, only when independently observed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database_execution_ns: Option<u64>,
    /// From server dispatch to completion of the driver task. When absent,
    /// elapsed_ns remains the same legacy clock for display and summaries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_elapsed_ns: Option<u64>,
    /// From server dispatch until the first nonempty row page is received.
    /// Currently retained only for successful iterations.
    pub first_row_ns: Option<u64>,
    /// From server dispatch until the completed stream's Done page is received.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_consumption_ns: Option<u64>,
    /// Complete row count is unavailable when an iteration does not finish.
    pub rows: Option<u64>,
}

/// A bounded allowlist observed before workload-specific transaction/setup.
/// Does not describe the editor session, every setting, or later overrides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionSettingsObservation {
    pub phase: String,
    pub values: std::collections::BTreeMap<String, String>,
}

impl SessionSettingsObservation {
    pub fn is_bounded(&self) -> bool {
        self.phase == "dedicated_connection_before_workload"
            && !self.values.is_empty()
            && self.values.len() <= 16
            && self.values.iter().all(|(key, value)| {
                !key.is_empty()
                    && key.len() <= 64
                    && key
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
                    && !value.is_empty()
                    && value.len() <= 128
                    && !value.chars().any(char::is_control)
            })
    }
}

/// Observed execution context. Missing values were not measured; profile
/// defaults do not imply that database-side settings were inspected.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BenchmarkEnvironment {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_version: Option<String>,
    pub preparation: String,
    pub connection_reuse: String,
    pub isolation: String,
    pub session_settings: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_settings: Option<SessionSettingsObservation>,
    pub cache_state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BenchmarkReport {
    pub version: u16,
    pub run_id: uuid::Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pair_context: Option<Box<crate::BenchmarkPairContext>>,
    pub engine: crate::Engine,
    pub sql: String,
    pub captured_at: chrono::DateTime<chrono::Utc>,
    pub warmups: u32,
    pub requested_iterations: u32,
    pub query_timeout_ms: u64,
    pub total_budget_ms: u64,
    pub delay_ms: u64,
    /// Values are not included in exported reports.
    pub parameter_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<BenchmarkEnvironment>,
    pub samples: Vec<BenchmarkSample>,
    pub completed: bool,
    pub warnings: Vec<String>,
    /// Summary fields cover elapsed_ns (server-side client elapsed) only.
    pub median_ns: Option<f64>,
    pub mean_ns: Option<f64>,
    pub min_ns: Option<u64>,
    pub max_ns: Option<u64>,
    pub standard_deviation_ns: Option<f64>,
    pub p95_ns: Option<u64>,
    pub p99_ns: Option<u64>,
}

/// Descriptive comparison of user-saved benchmark measurements, never a causal verdict.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BenchmarkComparison {
    pub timing: String,
    pub baseline: BenchmarkStatistics,
    pub candidate: BenchmarkStatistics,
    pub delta_ns: Option<f64>,
    pub delta_percent: Option<f64>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BenchmarkStatistics {
    pub successful: usize,
    pub failed: usize,
    pub timed_out: usize,
    pub cancelled: usize,
    pub median_ns: Option<f64>,
}

#[cfg(test)]
mod source_context_tests {
    use super::*;
    #[test]
    fn source_context_refuses_absolute_paths_unbounded_labels_and_false_clean_claims() {
        let mut context = BenchmarkSourceContext {
            captured_at: chrono::Utc::now(),
            instance_id: "local".into(),
            workspace_id: crate::WorkspaceId(1),
            workspace_revision: crate::WorkspaceRevision(2),
            path: crate::WorkspacePath("queries/test.sql".into()),
            git: None,
        };
        assert!(context.validate().is_ok());
        for path in [
            "/root/query.sql",
            "C:/query.sql",
            "../query.sql",
            "query\n.sql",
        ] {
            context.path.0 = path.into();
            assert!(context.validate().is_err());
        }
        context.path.0 = "queries/test.sql".into();
        context.instance_id = "x".repeat(201);
        assert!(context.validate().is_err());
        context.instance_id = "local".into();
        context.git = Some(BenchmarkSourceGit {
            observed_at: chrono::Utc::now(),
            workspace_revision: crate::WorkspaceRevision(1),
            head_oid: Some("a".repeat(40)),
            branch: Some("main".into()),
            dirty: None,
            truncated: true,
        });
        assert!(context.validate().is_ok());
        context.git.as_mut().unwrap().dirty = Some(false);
        assert!(context.validate().is_err());
        context.git.as_mut().unwrap().dirty = Some(true);
        assert!(context.validate().is_ok());
        context.git.as_mut().unwrap().head_oid = Some("g".repeat(40));
        assert!(context.validate().is_err());
        context.git.as_mut().unwrap().head_oid = Some("b".repeat(64));
        assert!(context.validate().is_ok());
        context.git.as_mut().unwrap().branch = Some("x".repeat(256));
        assert!(context.validate().is_err());
        context.git.as_mut().unwrap().branch = Some("main".into());
        let mut raw = serde_json::to_value(context).unwrap();
        raw["remote_url"] = serde_json::json!("unrequested");
        assert!(serde_json::from_value::<BenchmarkSourceContext>(raw).is_err());
    }
}

/// Disabled by default. Limits apply to the owner's saved snapshots, including
/// annotations and source context, using immutable server save timestamps.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkRetentionPolicy {
    pub max_age_days: Option<u32>,
    pub max_runs: Option<u32>,
}
impl BenchmarkRetentionPolicy {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self
            .max_age_days
            .is_some_and(|days| !(1..=36500).contains(&days))
            || self.max_runs.is_some_and(|runs| !(1..=500).contains(&runs))
        {
            return Err("retention age must be 1–36500 days and retained count 1–500; null disables a limit");
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BenchmarkRetentionState {
    pub policy: BenchmarkRetentionPolicy,
    pub revision: u64,
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PreviewBenchmarkRetentionRequest {
    pub expected_revision: u64,
    pub policy: BenchmarkRetentionPolicy,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BenchmarkRetentionPreview {
    pub revision: u64,
    pub policy: BenchmarkRetentionPolicy,
    pub as_of: chrono::DateTime<chrono::Utc>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub remove_count: u32,
    pub remove_bytes: u64,
    /// A state-change fence, not authentication or execution authority.
    pub review_token: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateBenchmarkRetentionRequest {
    pub expected_revision: u64,
    pub policy: BenchmarkRetentionPolicy,
    pub as_of: chrono::DateTime<chrono::Utc>,
    pub review_token: String,
    pub confirmed: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BenchmarkRetentionUpdate {
    pub retention: BenchmarkRetentionState,
    pub removed: u32,
    pub removed_bytes: u64,
}

#[cfg(test)]
mod profile_attachment_tests {
    use super::*;
    #[test]
    fn private_profile_validation_preserves_population_and_bounds() {
        let report: BenchmarkReport = serde_json::from_value(serde_json::json!({"version":2,"run_id":uuid::Uuid::new_v4(),"engine":"postgres","sql":"SELECT $1","captured_at":chrono::Utc::now(),"parameter_count":1,"warmups":0,"requested_iterations":1,"query_timeout_ms":100,"total_budget_ms":1000,"delay_ms":0,"samples":[],"completed":false,"warnings":[]})).unwrap();
        let attachment: BenchmarkProfileAttachment = serde_json::from_value(serde_json::json!({"captured_at":chrono::Utc::now(),"sql":report.sql,"parameter_count":1,"response":{"run_id":uuid::Uuid::new_v4(),"plan":{"engine":"postgres","analyzed":true,"raw":"private native plan","root":{"op":"Result","actual_rows":1.0}},"server_elapsed_ns":100,"execution_ms":5000.0}})).unwrap();
        assert!(attachment.validate_for(&report).is_ok());
        let mut legacy = serde_json::json!({"name":"legacy","report":report});
        assert!(
            serde_json::from_value::<SaveBenchmarkRunRequest>(legacy.clone())
                .unwrap()
                .profile
                .is_none()
        );
        legacy["profile"] = serde_json::to_value(&attachment).unwrap();
        assert!(serde_json::from_value::<SaveBenchmarkRunRequest>(legacy)
            .unwrap()
            .profile
            .is_some());
        for changed in 0..8 {
            let mut invalid = attachment.clone();
            match changed {
                0 => invalid.sql.push_str(" LIMIT 1"),
                1 => invalid.parameter_count = 2,
                2 => invalid.response.run_id = report.run_id,
                3 => invalid.response.plan.engine = crate::Engine::SqlServer,
                4 => invalid.response.plan.analyzed = false,
                5 => invalid.response.execution_ms = Some(f64::NAN),
                6 => invalid.response.plan.root.actual_rows = Some(-1.0),
                _ => invalid.response.plan.raw = "x".repeat(1024 * 1024 + 1),
            }
            assert!(invalid.validate_for(&report).is_err(), "case {changed}");
        }
        let mut deep = attachment.clone();
        for _ in 0..64 {
            let old =
                std::mem::replace(&mut deep.response.plan.root, crate::PlanNode::new("Parent"));
            deep.response.plan.root.children.push(old);
        }
        assert!(deep.validate_for(&report).is_err());
        let mut wide = attachment.clone();
        wide.response.plan.root.children = vec![crate::PlanNode::new("Child"); 4096];
        assert!(wide.validate_for(&report).is_err());
        let mut attributes = attachment.clone();
        let mut nested = serde_json::json!("value");
        for _ in 0..64 {
            nested = serde_json::json!([nested]);
        }
        attributes
            .response
            .plan
            .root
            .extra
            .insert("nested".into(), nested);
        assert!(attributes.validate_for(&report).is_err());
        let mut oversized = attachment.clone();
        oversized.response.warnings = vec!["x".repeat(1024 * 1024); 2];
        assert!(oversized.validate_for(&report).is_err());
        let mut sqlite = attachment;
        let mut sqlite_report = report;
        sqlite_report.engine = crate::Engine::Sqlite;
        sqlite.response.plan.engine = crate::Engine::Sqlite;
        sqlite.response.plan.analyzed = false;
        sqlite.response.execution_ms = None;
        sqlite.response.plan.root.actual_rows = None;
        assert!(sqlite.validate_for(&sqlite_report).is_ok());
        sqlite.response.plan.root.actual_ms = Some(0.1);
        assert!(
            sqlite.validate_for(&sqlite_report).is_err(),
            "estimated SQLite nodes cannot acquire actual clocks"
        );
    }
}
