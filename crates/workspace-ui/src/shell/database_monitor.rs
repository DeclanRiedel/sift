use sift_protocol::{
    DatabaseDeadlockEvent, DatabaseProcess, PostgresSetting, PostgresSettingsPage,
};

use super::RequestState;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub(super) enum DatabaseMonitorView {
    Overview,
    #[default]
    Activity,
    Locks,
    Deadlocks,
    History,
    Alerts,
    Settings,
    Extensions,
    Partitions,
    Policies,
    Roles,
    Ownership,
    SchemaGrants,
    Replication,
    Statistics,
    QueryStore,
    AgentJobs,
    SqlServerSettings,
    Security,
    Maintenance,
}

impl DatabaseMonitorView {
    pub(super) const ALL: [Self; 20] = [
        Self::Overview,
        Self::Activity,
        Self::Locks,
        Self::Deadlocks,
        Self::History,
        Self::Alerts,
        Self::QueryStore,
        Self::AgentJobs,
        Self::SqlServerSettings,
        Self::Security,
        Self::Maintenance,
        Self::Settings,
        Self::Extensions,
        Self::Partitions,
        Self::Policies,
        Self::Roles,
        Self::Ownership,
        Self::SchemaGrants,
        Self::Replication,
        Self::Statistics,
    ];

    pub(super) const fn button_id(self) -> &'static str {
        match self {
            Self::Overview => "monitor-view-overview",
            Self::Activity => "monitor-view-activity",
            Self::Locks => "monitor-view-locks",
            Self::Deadlocks => "monitor-view-deadlocks",
            Self::History => "monitor-view-deadlock-history",
            Self::Alerts => "monitor-view-alerts",
            Self::QueryStore => "monitor-view-query-store",
            Self::AgentJobs => "monitor-view-agent-jobs",
            Self::SqlServerSettings => "monitor-view-sqlserver-settings",
            Self::Security => "monitor-view-sqlserver-security",
            Self::Maintenance => "monitor-view-sqlserver-maintenance",
            Self::Settings => "monitor-view-settings",
            Self::Extensions => "monitor-view-extensions",
            Self::Partitions => "monitor-view-partitions",
            Self::Policies => "monitor-view-policies",
            Self::Roles => "monitor-view-roles",
            Self::Ownership => "monitor-view-owners",
            Self::SchemaGrants => "monitor-view-schema-grants",
            Self::Replication => "monitor-view-replication",
            Self::Statistics => "monitor-view-statistics",
        }
    }

    pub(super) fn label(self, state: &DatabaseMonitorState) -> String {
        match self {
            Self::Overview => "Overview".into(),
            Self::Activity => "Activity".into(),
            Self::Locks => format!("Locks {}", state.lock_process_count()),
            Self::Deadlocks => format!("Cycles {}", state.deadlock_process_count()),
            Self::History => format!("History {}", state.deadlocks().len()),
            Self::Alerts => format!("Alerts {}", state.alert_count()),
            Self::QueryStore => "Query Store".into(),
            Self::AgentJobs => "Agent jobs".into(),
            Self::SqlServerSettings => "Server settings".into(),
            Self::Security => "Security".into(),
            Self::Maintenance => "Maintenance".into(),
            Self::Settings => "Settings".into(),
            Self::Extensions => "Extensions".into(),
            Self::Partitions => "Partitions".into(),
            Self::Policies => "Policies".into(),
            Self::Roles => "Roles".into(),
            Self::Ownership => "Ownership".into(),
            Self::SchemaGrants => "Schema grants".into(),
            Self::Replication => "Replication".into(),
            Self::Statistics => "Statistics".into(),
        }
    }

    pub(super) fn available_for(self, provider: Option<&str>, connected: bool) -> bool {
        match self {
            Self::QueryStore | Self::AgentJobs | Self::SqlServerSettings | Self::Security => {
                connected && provider == Some("sift/sql-server")
            }
            Self::Maintenance => {
                connected && matches!(provider, Some("sift/sql-server" | "sift/postgres"))
            }
            Self::Settings
            | Self::Extensions
            | Self::Partitions
            | Self::Policies
            | Self::Roles
            | Self::Ownership
            | Self::SchemaGrants
            | Self::Replication
            | Self::Statistics => provider == Some("sift/postgres"),
            Self::Overview
            | Self::Activity
            | Self::Locks
            | Self::Deadlocks
            | Self::History
            | Self::Alerts => true,
        }
    }

    pub(super) fn adjacent_available(
        self,
        forward: bool,
        provider: Option<&str>,
        connected: bool,
    ) -> Self {
        let len = Self::ALL.len();
        let mut index = Self::ALL
            .iter()
            .position(|view| *view == self)
            .expect("every Monitor view has a tab");
        for _ in 0..len {
            index = if forward {
                (index + 1) % len
            } else {
                (index + len - 1) % len
            };
            let candidate = Self::ALL[index];
            if candidate.available_for(provider, connected) {
                return candidate;
            }
        }
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DatabaseAlertKind {
    DeadlockRisk,
    LongRunning,
    IdleInTransaction,
}

impl DatabaseAlertKind {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::DeadlockRisk => "blocking cycle",
            Self::LongRunning => "long running",
            Self::IdleInTransaction => "idle in transaction",
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct DatabaseMonitorState {
    dashboard: Option<sift_protocol::ServerDashboard>,
    dashboard_request: RequestState,
    processes: Vec<DatabaseProcess>,
    process_generation: u64,
    process_cursor: Option<i64>,
    termination_preview: Option<ProcessTerminationPreview>,
    termination_pending: Option<(u64, i64)>,
    termination_sequence: u64,
    deadlocks: Vec<DatabaseDeadlockEvent>,
    request: RequestState,
    deadlock_request: RequestState,
    selected: Option<i64>,
    view: DatabaseMonitorView,
    alerts: std::collections::HashMap<i64, DatabaseAlertKind>,
    settings: Vec<PostgresSetting>,
    settings_request: RequestState,
    settings_offset: u32,
    settings_next_offset: Option<u32>,
    extensions: Vec<sift_protocol::PostgresExtension>,
    partitions: Vec<sift_protocol::PostgresPartition>,
    policies: Vec<sift_protocol::PostgresPolicy>,
    roles: Vec<sift_protocol::PostgresRole>,
    owners: Vec<sift_protocol::PostgresOwnedObject>,
    schema_grants: Vec<sift_protocol::PostgresSchemaGrant>,
    objects_request: RequestState,
    objects_offset: u32,
    objects_next_offset: Option<u32>,
    objects_selected: usize,
    object_preview: Option<sift_protocol::PostgresObjectPreview>,
    object_action_request: RequestState,
    replication: Option<sift_protocol::PostgresReplicationReport>,
    replication_request: RequestState,
    replication_epoch: u64,
    replication_selected: usize,
    statistics: Option<sift_protocol::PostgresStatisticsReport>,
    statistics_request: RequestState,
    statistics_epoch: u64,
    statistics_offset: u32,
    statistics_selected: usize,
    query_store: Option<sift_protocol::QueryStoreReport>,
    query_store_request: RequestState,
    agent_jobs: Option<sift_protocol::AgentJobsReport>,
    agent_jobs_request: RequestState,
    agent_jobs_selected: usize,
    sqlserver_settings: Option<sift_protocol::SqlServerSettingsReport>,
    security: Option<sift_protocol::SqlServerSecurityReport>,
    security_request: RequestState,
    security_selected: usize,
    security_preview: Option<sift_protocol::SqlServerSecurityPreview>,
    security_action_request: RequestState,
    sqlserver_settings_request: RequestState,
    sqlserver_settings_selected: usize,
}

#[derive(Debug, Clone)]
pub(super) struct ProcessTerminationPreview {
    pub process: DatabaseProcess,
    generation: u64,
}

impl DatabaseMonitorState {
    pub(super) fn replication(&self) -> Option<&sift_protocol::PostgresReplicationReport> {
        self.replication.as_ref()
    }
    pub(super) fn replication_request(&self) -> &RequestState {
        &self.replication_request
    }
    pub(super) fn replication_selected(&self) -> usize {
        self.replication_selected
    }
    pub(super) fn start_replication(&mut self) -> u64 {
        self.replication_epoch = self.replication_epoch.wrapping_add(1);
        self.replication_request.start();
        self.replication_epoch
    }
    pub(super) fn fail_replication(&mut self, message: impl Into<String>) {
        self.replication_request.fail(message);
    }
    pub(super) fn finish_replication(
        &mut self,
        epoch: u64,
        result: Result<sift_protocol::PostgresReplicationReport, String>,
    ) {
        if self.view != DatabaseMonitorView::Replication
            || !self.replication_request.loading()
            || epoch != self.replication_epoch
        {
            return;
        }
        match result {
            Ok(report) => {
                let count = report.senders.len()
                    + report.slots.len()
                    + usize::from(report.receiver.is_some());
                self.replication_selected = self.replication_selected.min(count.saturating_sub(1));
                self.replication = Some(report);
                self.replication_request.succeed();
            }
            Err(message) => self.replication_request.fail(message),
        }
    }
    pub(super) fn move_replication_selection(&mut self, delta: isize) {
        let count = self.replication.as_ref().map_or(0, |report| {
            report.senders.len() + report.slots.len() + usize::from(report.receiver.is_some())
        });
        if count > 0 {
            self.replication_selected = self
                .replication_selected
                .saturating_add_signed(delta)
                .min(count - 1);
        }
    }
    pub(super) fn statistics(&self) -> Option<&sift_protocol::PostgresStatisticsReport> {
        self.statistics.as_ref()
    }
    pub(super) fn statistics_request(&self) -> &RequestState {
        &self.statistics_request
    }
    pub(super) fn statistics_offset(&self) -> u32 {
        self.statistics_offset
    }
    pub(super) fn statistics_selected(&self) -> usize {
        self.statistics_selected
    }
    pub(super) fn start_statistics(&mut self) -> u64 {
        self.statistics_epoch = self.statistics_epoch.wrapping_add(1);
        self.statistics_request.start();
        self.statistics_epoch
    }
    pub(super) fn fail_statistics(&mut self, message: impl Into<String>) {
        self.statistics_request.fail(message);
    }
    pub(super) fn finish_statistics(
        &mut self,
        epoch: u64,
        offset: u32,
        result: Result<sift_protocol::PostgresStatisticsReport, String>,
    ) {
        if self.view != DatabaseMonitorView::Statistics
            || !self.statistics_request.loading()
            || epoch != self.statistics_epoch
        {
            return;
        }
        match result {
            Ok(report) => {
                self.statistics_selected = 0;
                self.statistics_offset = offset;
                self.statistics = Some(report);
                self.statistics_request.succeed();
            }
            Err(message) => self.statistics_request.fail(message),
        }
    }
    pub(super) fn move_statistics_selection(&mut self, delta: isize) {
        let count = self
            .statistics
            .as_ref()
            .map_or(0, |report| report.tables.len());
        if count > 0 {
            self.statistics_selected = self
                .statistics_selected
                .saturating_add_signed(delta)
                .min(count - 1);
        }
    }
    pub(super) fn clear_postgres_diagnostics(&mut self) {
        self.replication_epoch = self.replication_epoch.wrapping_add(1);
        self.statistics_epoch = self.statistics_epoch.wrapping_add(1);
        self.replication = None;
        self.replication_request = RequestState::default();
        self.replication_selected = 0;
        self.statistics = None;
        self.statistics_request = RequestState::default();
        self.statistics_offset = 0;
        self.statistics_selected = 0;
        if matches!(
            self.view,
            DatabaseMonitorView::Replication | DatabaseMonitorView::Statistics
        ) {
            self.view = DatabaseMonitorView::Activity;
        }
    }
    pub(super) fn dashboard(&self) -> Option<&sift_protocol::ServerDashboard> {
        self.dashboard.as_ref()
    }

    pub(super) fn dashboard_request(&self) -> &RequestState {
        &self.dashboard_request
    }

    pub(super) fn start_dashboard(&mut self) {
        self.dashboard = None;
        self.dashboard_request.start();
    }

    pub(super) fn clear_dashboard(&mut self) {
        self.dashboard = None;
        self.dashboard_request = RequestState::Idle;
    }

    pub(super) fn finish_dashboard(
        &mut self,
        result: Result<sift_protocol::ServerDashboard, String>,
    ) {
        match result {
            Ok(report) => {
                self.dashboard = Some(report);
                self.dashboard_request.succeed();
            }
            Err(message) => self.dashboard_request.fail(message),
        }
    }

    pub(super) fn request(&self) -> &RequestState {
        &self.request
    }

    pub(super) fn deadlock_request(&self) -> &RequestState {
        &self.deadlock_request
    }

    pub(super) fn deadlocks(&self) -> &[DatabaseDeadlockEvent] {
        &self.deadlocks
    }

    pub(super) fn start_deadlocks(&mut self) {
        self.deadlocks.clear();
        self.deadlock_request.start();
    }

    pub(super) fn fail_deadlocks(&mut self, message: impl Into<String>) {
        self.deadlocks.clear();
        self.deadlock_request.fail(message);
    }

    pub(super) fn finish_deadlocks(&mut self, result: Result<Vec<DatabaseDeadlockEvent>, String>) {
        match result {
            Ok(events) => {
                self.deadlocks = events;
                self.deadlock_request.succeed();
            }
            Err(message) => self.deadlock_request.fail(message),
        }
    }

    pub(super) fn query_store(&self) -> Option<&sift_protocol::QueryStoreReport> {
        self.query_store.as_ref()
    }

    pub(super) fn query_store_request(&self) -> &RequestState {
        &self.query_store_request
    }

    pub(super) fn start_query_store(&mut self) {
        self.query_store = None;
        self.query_store_request.start();
    }

    pub(super) fn clear_query_store(&mut self) {
        self.query_store = None;
        self.query_store_request = RequestState::Idle;
        if self.view == DatabaseMonitorView::QueryStore {
            self.view = DatabaseMonitorView::Activity;
        }
    }

    pub(super) fn finish_query_store(
        &mut self,
        result: Result<sift_protocol::QueryStoreReport, String>,
    ) {
        match result {
            Ok(report) => {
                self.query_store = Some(report);
                self.query_store_request.succeed();
            }
            Err(message) => self.query_store_request.fail(message),
        }
    }

    pub(super) fn agent_jobs(&self) -> Option<&sift_protocol::AgentJobsReport> {
        self.agent_jobs.as_ref()
    }

    pub(super) fn agent_jobs_request(&self) -> &RequestState {
        &self.agent_jobs_request
    }

    pub(super) fn agent_jobs_selected(&self) -> usize {
        self.agent_jobs_selected
    }

    pub(super) fn move_agent_jobs_selection(&mut self, key: &str) {
        let count = self
            .agent_jobs
            .as_ref()
            .map_or(0, |report| report.jobs.len());
        if count == 0 {
            return;
        }
        self.agent_jobs_selected = match key {
            "j" => (self.agent_jobs_selected + 1).min(count - 1),
            "k" => self.agent_jobs_selected.saturating_sub(1),
            "g" => 0,
            "G" => count - 1,
            _ => self.agent_jobs_selected,
        };
    }

    pub(super) fn start_agent_jobs(&mut self) {
        self.agent_jobs = None;
        self.agent_jobs_request.start();
    }

    pub(super) fn clear_agent_jobs(&mut self) {
        self.agent_jobs = None;
        self.agent_jobs_request = RequestState::Idle;
        self.agent_jobs_selected = 0;
        if self.view == DatabaseMonitorView::AgentJobs {
            self.view = DatabaseMonitorView::Activity;
        }
    }

    pub(super) fn finish_agent_jobs(
        &mut self,
        result: Result<sift_protocol::AgentJobsReport, String>,
    ) {
        match result {
            Ok(report) => {
                self.agent_jobs_selected = self
                    .agent_jobs_selected
                    .min(report.jobs.len().saturating_sub(1));
                self.agent_jobs = Some(report);
                self.agent_jobs_request.succeed();
            }
            Err(message) => self.agent_jobs_request.fail(message),
        }
    }

    pub(super) fn sqlserver_settings(&self) -> Option<&sift_protocol::SqlServerSettingsReport> {
        self.sqlserver_settings.as_ref()
    }

    pub(super) fn security(&self) -> Option<&sift_protocol::SqlServerSecurityReport> {
        self.security.as_ref()
    }
    pub(super) fn security_request(&self) -> &RequestState {
        &self.security_request
    }
    pub(super) fn security_selected(&self) -> usize {
        self.security_selected
    }
    pub(super) fn security_preview(&self) -> Option<&sift_protocol::SqlServerSecurityPreview> {
        self.security_preview.as_ref()
    }
    pub(super) fn security_action_request(&self) -> &RequestState {
        &self.security_action_request
    }
    pub(super) fn selected_security_action(
        &self,
    ) -> Option<sift_protocol::SqlServerSecurityAction> {
        let report = self.security.as_ref()?;
        if let Some(item) = report.memberships.items.get(self.security_selected) {
            return Some(sift_protocol::SqlServerSecurityAction::DropRoleMember {
                role: item.role.clone(),
                member: item.member.clone(),
            });
        }
        let index = self
            .security_selected
            .checked_sub(report.memberships.items.len())?;
        let item = report
            .schema_permissions
            .items
            .iter()
            .filter(|item| {
                item.permission == "SELECT"
                    && (item.state == "GRANT" || item.state == "GRANT_WITH_GRANT_OPTION")
            })
            .nth(index)?;
        Some(sift_protocol::SqlServerSecurityAction::RevokeSchemaSelect {
            schema: item.schema.clone(),
            grantee: item.grantee.clone(),
        })
    }
    pub(super) fn move_security_selection(&mut self, delta: isize) {
        let Some(report) = &self.security else {
            return;
        };
        let count = report.memberships.items.len()
            + report
                .schema_permissions
                .items
                .iter()
                .filter(|item| {
                    item.permission == "SELECT"
                        && (item.state == "GRANT" || item.state == "GRANT_WITH_GRANT_OPTION")
                })
                .count();
        if count > 0 {
            self.security_selected = self
                .security_selected
                .saturating_add_signed(delta)
                .min(count - 1);
        }
    }
    pub(super) fn clear_security_preview(&mut self) {
        self.security_preview = None;
        self.security_action_request = RequestState::default();
    }
    pub(super) fn start_security_action(&mut self) {
        self.security_action_request.start();
    }
    pub(super) fn fail_security_action(&mut self, message: impl Into<String>) {
        self.security_action_request.fail(message);
    }
    pub(super) fn finish_security_preview(
        &mut self,
        result: Result<sift_protocol::SqlServerSecurityPreview, String>,
    ) {
        if !self.security_action_request.loading() || self.view != DatabaseMonitorView::Security {
            return;
        }
        match result {
            Ok(preview) => {
                self.security_preview = Some(preview);
                self.security_action_request.succeed();
            }
            Err(message) => self.security_action_request.fail(message),
        }
    }
    pub(super) fn finish_security_apply(&mut self, result: Result<(), String>) -> bool {
        if !self.security_action_request.loading() {
            return false;
        }
        match result {
            Ok(()) => {
                self.security_preview = None;
                self.security_action_request.succeed();
                true
            }
            Err(message) => {
                self.security_action_request.fail(message);
                false
            }
        }
    }
    pub(super) fn start_security(&mut self) {
        self.security = None;
        self.security_selected = 0;
        self.clear_security_preview();
        self.security_request.start();
    }
    pub(super) fn clear_security(&mut self) {
        self.security = None;
        self.security_selected = 0;
        self.clear_security_preview();
        self.security_request = RequestState::Idle;
        if self.view == DatabaseMonitorView::Security {
            self.view = DatabaseMonitorView::Activity;
        }
    }
    pub(super) fn finish_security(
        &mut self,
        result: Result<sift_protocol::SqlServerSecurityReport, String>,
    ) {
        match result {
            Ok(report) => {
                self.security = Some(report);
                self.security_request.succeed();
            }
            Err(message) => self.security_request.fail(message),
        }
    }

    pub(super) fn sqlserver_settings_request(&self) -> &RequestState {
        &self.sqlserver_settings_request
    }

    pub(super) fn sqlserver_settings_selected(&self) -> usize {
        self.sqlserver_settings_selected
    }

    pub(super) fn move_sqlserver_settings_selection(&mut self, key: &str) {
        let count = self
            .sqlserver_settings
            .as_ref()
            .map_or(0, |report| report.settings.len());
        if count == 0 {
            return;
        }
        self.sqlserver_settings_selected = match key {
            "j" => (self.sqlserver_settings_selected + 1).min(count - 1),
            "k" => self.sqlserver_settings_selected.saturating_sub(1),
            "g" => 0,
            "G" => count - 1,
            _ => self.sqlserver_settings_selected,
        };
    }

    pub(super) fn start_sqlserver_settings(&mut self) {
        self.sqlserver_settings = None;
        self.sqlserver_settings_request.start();
    }

    pub(super) fn clear_sqlserver_settings(&mut self) {
        self.sqlserver_settings = None;
        self.sqlserver_settings_request = RequestState::Idle;
        self.sqlserver_settings_selected = 0;
        if self.view == DatabaseMonitorView::SqlServerSettings {
            self.view = DatabaseMonitorView::Activity;
        }
    }

    pub(super) fn finish_sqlserver_settings(
        &mut self,
        result: Result<sift_protocol::SqlServerSettingsReport, String>,
    ) {
        match result {
            Ok(report) => {
                self.sqlserver_settings_selected = self
                    .sqlserver_settings_selected
                    .min(report.settings.len().saturating_sub(1));
                self.sqlserver_settings = Some(report);
                self.sqlserver_settings_request.succeed();
            }
            Err(message) => self.sqlserver_settings_request.fail(message),
        }
    }

    pub(super) fn selected(&self) -> Option<i64> {
        self.selected
    }

    pub(super) const fn view(&self) -> DatabaseMonitorView {
        self.view
    }

    pub(super) fn set_view(&mut self, view: DatabaseMonitorView) {
        if self.view != view {
            self.termination_preview = None;
        }
        if self.view != view
            && matches!(
                view,
                DatabaseMonitorView::Extensions
                    | DatabaseMonitorView::Partitions
                    | DatabaseMonitorView::Policies
                    | DatabaseMonitorView::Roles
                    | DatabaseMonitorView::Ownership
                    | DatabaseMonitorView::SchemaGrants
            )
        {
            self.objects_request = RequestState::default();
            self.objects_offset = 0;
            self.objects_next_offset = None;
            self.objects_selected = 0;
            self.clear_object_preview();
        }
        if self.view != view && view == DatabaseMonitorView::Replication {
            self.replication_request = RequestState::default();
            self.replication_selected = 0;
        }
        if self.view != view && view == DatabaseMonitorView::Statistics {
            self.statistics_request = RequestState::default();
            self.statistics_offset = 0;
            self.statistics_selected = 0;
        }
        self.view = view;
        if self.selected.is_some_and(|selected| match view {
            DatabaseMonitorView::Overview => true,
            DatabaseMonitorView::Activity => false,
            DatabaseMonitorView::Locks => !self.lock_process_ids().contains(&selected),
            DatabaseMonitorView::Deadlocks => !self.deadlock_process_ids().contains(&selected),
            DatabaseMonitorView::History => true,
            DatabaseMonitorView::Alerts => !self.alerts.contains_key(&selected),
            DatabaseMonitorView::Settings => true,
            DatabaseMonitorView::Extensions
            | DatabaseMonitorView::Partitions
            | DatabaseMonitorView::Policies
            | DatabaseMonitorView::Roles
            | DatabaseMonitorView::Ownership
            | DatabaseMonitorView::SchemaGrants => true,
            DatabaseMonitorView::Replication | DatabaseMonitorView::Statistics => true,
            DatabaseMonitorView::QueryStore => true,
            DatabaseMonitorView::AgentJobs => true,
            DatabaseMonitorView::SqlServerSettings => true,
            DatabaseMonitorView::Security => true,
            DatabaseMonitorView::Maintenance => true,
        }) {
            self.selected = None;
        }
        let visible = self.visible_processes();
        if !visible
            .iter()
            .any(|process| Some(process.process_id) == self.process_cursor)
        {
            self.process_cursor = visible.first().map(|process| process.process_id);
        }
    }

    pub(super) fn visible_processes(&self) -> Vec<DatabaseProcess> {
        let included = match self.view {
            DatabaseMonitorView::Overview => return Vec::new(),
            DatabaseMonitorView::Activity => return self.processes.clone(),
            DatabaseMonitorView::Locks => self.lock_process_ids(),
            DatabaseMonitorView::Deadlocks => self.deadlock_process_ids(),
            DatabaseMonitorView::History => return Vec::new(),
            DatabaseMonitorView::Alerts => self.alerts.keys().copied().collect(),
            DatabaseMonitorView::Settings => return Vec::new(),
            DatabaseMonitorView::Extensions
            | DatabaseMonitorView::Partitions
            | DatabaseMonitorView::Policies
            | DatabaseMonitorView::Roles
            | DatabaseMonitorView::Ownership
            | DatabaseMonitorView::SchemaGrants => return Vec::new(),
            DatabaseMonitorView::Replication | DatabaseMonitorView::Statistics => {
                return Vec::new()
            }
            DatabaseMonitorView::QueryStore => return Vec::new(),
            DatabaseMonitorView::AgentJobs => return Vec::new(),
            DatabaseMonitorView::SqlServerSettings => return Vec::new(),
            DatabaseMonitorView::Security => return Vec::new(),
            DatabaseMonitorView::Maintenance => return Vec::new(),
        };
        self.processes
            .iter()
            .filter(|process| included.contains(&process.process_id))
            .cloned()
            .collect()
    }

    pub(super) fn settings(&self) -> &[PostgresSetting] {
        &self.settings
    }

    pub(super) fn extensions(&self) -> &[sift_protocol::PostgresExtension] {
        &self.extensions
    }
    pub(super) fn partitions(&self) -> &[sift_protocol::PostgresPartition] {
        &self.partitions
    }
    pub(super) fn policies(&self) -> &[sift_protocol::PostgresPolicy] {
        &self.policies
    }
    pub(super) fn roles(&self) -> &[sift_protocol::PostgresRole] {
        &self.roles
    }
    pub(super) fn owners(&self) -> &[sift_protocol::PostgresOwnedObject] {
        &self.owners
    }
    pub(super) fn schema_grants(&self) -> &[sift_protocol::PostgresSchemaGrant] {
        &self.schema_grants
    }
    pub(super) fn objects_request(&self) -> &RequestState {
        &self.objects_request
    }
    pub(super) fn object_action_request(&self) -> &RequestState {
        &self.object_action_request
    }
    pub(super) fn objects_offset(&self) -> u32 {
        self.objects_offset
    }
    pub(super) fn objects_next_offset(&self) -> Option<u32> {
        self.objects_next_offset
    }
    pub(super) fn objects_selected(&self) -> usize {
        self.objects_selected
    }
    pub(super) fn object_preview(&self) -> Option<&sift_protocol::PostgresObjectPreview> {
        self.object_preview.as_ref()
    }
    pub(super) fn clear_object_preview(&mut self) {
        self.object_preview = None;
        self.object_action_request = RequestState::default();
    }
    pub(super) fn move_object_selection(&mut self, delta: isize) {
        let len = match self.view {
            DatabaseMonitorView::Extensions => self.extensions.len(),
            DatabaseMonitorView::Partitions => self.partitions.len(),
            DatabaseMonitorView::Policies => self.policies.len(),
            DatabaseMonitorView::Roles => self.roles.len(),
            DatabaseMonitorView::Ownership => self.owners.len(),
            DatabaseMonitorView::SchemaGrants => self.schema_grants.len(),
            _ => 0,
        };
        if len > 0 {
            self.objects_selected = self
                .objects_selected
                .saturating_add_signed(delta)
                .min(len - 1);
        }
    }
    pub(super) fn clear_objects(&mut self) {
        self.extensions.clear();
        self.partitions.clear();
        self.policies.clear();
        self.roles.clear();
        self.owners.clear();
        self.schema_grants.clear();
        self.objects_request = RequestState::default();
        self.objects_offset = 0;
        self.objects_next_offset = None;
        self.objects_selected = 0;
        self.clear_object_preview();
        if matches!(
            self.view,
            DatabaseMonitorView::Extensions
                | DatabaseMonitorView::Partitions
                | DatabaseMonitorView::Policies
                | DatabaseMonitorView::Roles
                | DatabaseMonitorView::Ownership
                | DatabaseMonitorView::SchemaGrants
        ) {
            self.view = DatabaseMonitorView::Activity;
        }
    }
    pub(super) fn start_objects_load(&mut self) {
        self.objects_request.start();
        self.clear_object_preview();
    }
    pub(super) fn fail_objects_load(&mut self, message: impl Into<String>) {
        self.objects_request.fail(message);
    }
    pub(super) fn finish_extensions(
        &mut self,
        offset: u32,
        result: Result<sift_protocol::PostgresObjectPage<sift_protocol::PostgresExtension>, String>,
    ) {
        if self.view != DatabaseMonitorView::Extensions {
            return;
        }
        match result {
            Ok(page) => {
                self.extensions = page.items;
                self.objects_offset = offset;
                self.objects_next_offset = page.next_offset;
                self.objects_selected = 0;
                self.objects_request.succeed();
            }
            Err(message) => self.objects_request.fail(message),
        }
    }
    pub(super) fn finish_partitions(
        &mut self,
        offset: u32,
        result: Result<sift_protocol::PostgresObjectPage<sift_protocol::PostgresPartition>, String>,
    ) {
        if self.view != DatabaseMonitorView::Partitions {
            return;
        }
        match result {
            Ok(page) => {
                self.partitions = page.items;
                self.objects_offset = offset;
                self.objects_next_offset = page.next_offset;
                self.objects_selected = 0;
                self.objects_request.succeed();
            }
            Err(message) => self.objects_request.fail(message),
        }
    }
    pub(super) fn finish_policies(
        &mut self,
        offset: u32,
        result: Result<sift_protocol::PostgresObjectPage<sift_protocol::PostgresPolicy>, String>,
    ) {
        if self.view != DatabaseMonitorView::Policies {
            return;
        }
        match result {
            Ok(page) => {
                self.policies = page.items;
                self.objects_offset = offset;
                self.objects_next_offset = page.next_offset;
                self.objects_selected = 0;
                self.objects_request.succeed();
            }
            Err(message) => self.objects_request.fail(message),
        }
    }
    pub(super) fn finish_roles(
        &mut self,
        offset: u32,
        result: Result<sift_protocol::PostgresObjectPage<sift_protocol::PostgresRole>, String>,
    ) {
        if self.view != DatabaseMonitorView::Roles {
            return;
        }
        match result {
            Ok(page) => {
                self.roles = page.items;
                self.objects_offset = offset;
                self.objects_next_offset = page.next_offset;
                self.objects_selected = 0;
                self.objects_request.succeed();
            }
            Err(message) => self.objects_request.fail(message),
        }
    }
    pub(super) fn finish_owners(
        &mut self,
        offset: u32,
        result: Result<
            sift_protocol::PostgresObjectPage<sift_protocol::PostgresOwnedObject>,
            String,
        >,
    ) {
        if self.view != DatabaseMonitorView::Ownership {
            return;
        }
        match result {
            Ok(page) => {
                self.owners = page.items;
                self.objects_offset = offset;
                self.objects_next_offset = page.next_offset;
                self.objects_selected = 0;
                self.objects_request.succeed();
            }
            Err(message) => self.objects_request.fail(message),
        }
    }
    pub(super) fn finish_schema_grants(
        &mut self,
        offset: u32,
        result: Result<
            sift_protocol::PostgresObjectPage<sift_protocol::PostgresSchemaGrant>,
            String,
        >,
    ) {
        if self.view != DatabaseMonitorView::SchemaGrants {
            return;
        }
        match result {
            Ok(page) => {
                self.schema_grants = page.items;
                self.objects_offset = offset;
                self.objects_next_offset = page.next_offset;
                self.objects_selected = 0;
                self.objects_request.succeed();
            }
            Err(message) => self.objects_request.fail(message),
        }
    }
    pub(super) fn start_object_action(&mut self) {
        self.object_action_request.start();
    }
    pub(super) fn fail_object_action(&mut self, message: impl Into<String>) {
        self.object_action_request.fail(message);
    }
    pub(super) fn finish_object_preview(
        &mut self,
        result: Result<sift_protocol::PostgresObjectPreview, String>,
    ) {
        if !self.object_action_request.loading() {
            return;
        }
        if !matches!(
            self.view,
            DatabaseMonitorView::Extensions
                | DatabaseMonitorView::Partitions
                | DatabaseMonitorView::Policies
        ) {
            return;
        }
        if let Ok(preview) = &result {
            let matches_view = matches!(
                (self.view, &preview.action),
                (
                    DatabaseMonitorView::Extensions,
                    sift_protocol::PostgresObjectAction::InstallExtension { .. }
                        | sift_protocol::PostgresObjectAction::DropExtension { .. }
                ) | (
                    DatabaseMonitorView::Partitions,
                    sift_protocol::PostgresObjectAction::DetachPartition { .. }
                ) | (
                    DatabaseMonitorView::Policies,
                    sift_protocol::PostgresObjectAction::RenamePolicy { .. }
                )
            );
            if !matches_view {
                return;
            }
        }
        match result {
            Ok(preview) => {
                self.object_preview = Some(preview);
                self.object_action_request.succeed();
            }
            Err(message) => self.object_action_request.fail(message),
        }
    }
    pub(super) fn finish_object_apply(&mut self, result: Result<(), String>) {
        if !self.object_action_request.loading() {
            return;
        }
        match result {
            Ok(()) => {
                self.object_preview = None;
                self.object_action_request.succeed();
            }
            Err(message) => self.object_action_request.fail(message),
        }
    }

    pub(super) fn clear_settings(&mut self) {
        self.settings.clear();
        self.settings_request = RequestState::default();
        self.settings_offset = 0;
        self.settings_next_offset = None;
        if self.view == DatabaseMonitorView::Settings {
            self.view = DatabaseMonitorView::Activity;
        }
    }

    pub(super) fn settings_request(&self) -> &RequestState {
        &self.settings_request
    }

    pub(super) fn settings_offset(&self) -> u32 {
        self.settings_offset
    }

    pub(super) fn settings_next_offset(&self) -> Option<u32> {
        self.settings_next_offset
    }

    pub(super) fn start_settings_load(&mut self) {
        self.settings_request.start();
    }

    pub(super) fn fail_settings_load(&mut self, message: impl Into<String>) {
        self.settings_request.fail(message);
    }

    pub(super) fn finish_settings_load(
        &mut self,
        offset: u32,
        result: Result<PostgresSettingsPage, String>,
    ) {
        match result {
            Ok(page) => {
                self.settings = page.settings;
                self.settings_offset = offset;
                self.settings_next_offset = page.next_offset;
                self.settings_request.succeed();
            }
            Err(message) => self.settings_request.fail(message),
        }
    }

    pub(super) fn lock_process_count(&self) -> usize {
        self.lock_process_ids().len()
    }

    pub(super) fn alert_count(&self) -> usize {
        self.alerts.len()
    }

    pub(super) fn deadlock_process_count(&self) -> usize {
        self.deadlock_process_ids().len()
    }

    pub(super) fn alert(&self, process_id: i64) -> Option<DatabaseAlertKind> {
        self.alerts.get(&process_id).copied()
    }

    fn lock_process_ids(&self) -> std::collections::HashSet<i64> {
        self.processes
            .iter()
            .filter(|process| !process.blocked_by.is_empty())
            .flat_map(|process| {
                std::iter::once(process.process_id).chain(process.blocked_by.iter().copied())
            })
            .collect()
    }

    fn deadlock_process_ids(&self) -> std::collections::HashSet<i64> {
        self.alerts
            .iter()
            .filter_map(|(id, kind)| (*kind == DatabaseAlertKind::DeadlockRisk).then_some(*id))
            .collect()
    }

    pub(super) fn start_loading(&mut self) -> u64 {
        self.process_generation = self.process_generation.wrapping_add(1);
        self.termination_preview = None;
        self.request.start();
        self.process_generation
    }

    pub(super) fn fail_loading(&mut self, message: impl Into<String>) {
        self.request.fail(message);
    }

    pub(super) fn finish_loading(
        &mut self,
        generation: u64,
        result: Result<Vec<DatabaseProcess>, String>,
    ) {
        if generation != self.process_generation || !self.request.loading() {
            return;
        }
        match result {
            Ok(processes) => {
                if self.selected.is_some_and(|selected| {
                    !processes
                        .iter()
                        .any(|process| process.process_id == selected)
                }) {
                    self.selected = None;
                }
                self.alerts = classify_alerts(&processes, chrono::Utc::now());
                self.processes = processes;
                self.set_view(self.view);
                self.request.succeed();
            }
            Err(message) => self.request.fail(message),
        }
    }

    pub(super) fn clear_processes(&mut self) {
        self.process_generation = self.process_generation.wrapping_add(1);
        self.termination_sequence = self.termination_sequence.wrapping_add(1);
        self.processes.clear();
        self.selected = None;
        self.process_cursor = None;
        self.termination_preview = None;
        self.termination_pending = None;
        self.alerts.clear();
        self.request = RequestState::default();
    }

    pub(super) fn process_cursor(&self) -> Option<i64> {
        self.process_cursor
    }

    pub(super) fn move_process_cursor(&mut self, delta: isize) {
        let visible = self.visible_processes();
        if visible.is_empty() {
            self.process_cursor = None;
            return;
        }
        let index = visible
            .iter()
            .position(|process| Some(process.process_id) == self.process_cursor)
            .unwrap_or(0);
        let next = index.saturating_add_signed(delta).min(visible.len() - 1);
        self.process_cursor = Some(visible[next].process_id);
    }

    pub(super) fn termination_preview(&self) -> Option<&ProcessTerminationPreview> {
        self.termination_preview.as_ref()
    }
    pub(super) fn termination_pending(&self) -> bool {
        self.termination_pending.is_some()
    }

    pub(super) fn preview_termination(&mut self, process_id: i64) -> bool {
        if self.termination_pending.is_some() {
            return false;
        }
        let Some(process) = self
            .visible_processes()
            .into_iter()
            .find(|process| process.process_id == process_id)
        else {
            return false;
        };
        self.termination_preview = Some(ProcessTerminationPreview {
            process,
            generation: self.process_generation,
        });
        true
    }

    pub(super) fn clear_termination_preview(&mut self) {
        self.termination_preview = None;
    }

    pub(super) fn start_termination(&mut self, process_id: i64) -> Option<u64> {
        if self.termination_pending.is_some() {
            return None;
        }
        let preview = self.termination_preview.as_ref()?;
        if preview.generation != self.process_generation || preview.process.process_id != process_id
        {
            return None;
        }
        let current = self
            .processes
            .iter()
            .find(|process| process.process_id == process_id)?;
        if current.user != preview.process.user
            || current.database != preview.process.database
            || current.started_at != preview.process.started_at
        {
            return None;
        }
        self.termination_sequence = self.termination_sequence.wrapping_add(1);
        let sequence = self.termination_sequence;
        self.termination_pending = Some((sequence, process_id));
        Some(sequence)
    }

    pub(super) fn finish_termination(&mut self, sequence: u64, process_id: i64) -> bool {
        if self.termination_pending != Some((sequence, process_id)) {
            return false;
        }
        self.termination_pending = None;
        self.termination_preview = None;
        true
    }

    pub(super) fn toggle(&mut self, process_id: i64) {
        self.selected = (self.selected != Some(process_id)).then_some(process_id);
    }

    pub(super) fn clear_selection(&mut self) {
        self.selected = None;
    }

    pub(super) fn statement(&self, process_id: i64) -> Option<&str> {
        self.processes
            .iter()
            .find(|process| process.process_id == process_id)
            .and_then(|process| process.statement.as_deref())
    }
}

fn classify_alerts(
    processes: &[DatabaseProcess],
    now: chrono::DateTime<chrono::Utc>,
) -> std::collections::HashMap<i64, DatabaseAlertKind> {
    let blockers = processes
        .iter()
        .map(|process| (process.process_id, process.blocked_by.as_slice()))
        .collect::<std::collections::HashMap<_, _>>();
    let in_cycle = |origin: i64| {
        let mut frontier = vec![origin];
        let mut visited = std::collections::HashSet::new();
        while let Some(current) = frontier.pop() {
            if !visited.insert(current) {
                continue;
            }
            for blocker in blockers.get(&current).into_iter().flat_map(|ids| *ids) {
                if *blocker == origin {
                    return true;
                }
                frontier.push(*blocker);
            }
        }
        false
    };
    processes
        .iter()
        .filter_map(|process| {
            let elapsed = process
                .started_at
                .map(|started| now.signed_duration_since(started))
                .unwrap_or_default();
            let state = process
                .state
                .as_deref()
                .unwrap_or_default()
                .to_ascii_lowercase();
            let kind = if in_cycle(process.process_id) {
                Some(DatabaseAlertKind::DeadlockRisk)
            } else if state.contains("idle in transaction") && elapsed.num_seconds() >= 60 {
                Some(DatabaseAlertKind::IdleInTransaction)
            } else if process.statement.is_some()
                && !state.contains("idle")
                && elapsed.num_seconds() >= 30
            {
                Some(DatabaseAlertKind::LongRunning)
            } else {
                None
            }?;
            Some((process.process_id, kind))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::commands::{CommandId, CommandLanguageMatch, CommandRegistry};
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_rendered_monitor_tab_is_reachable_with_vim_commands() {
        assert_eq!(
            CommandRegistry::resolve_language(&["d".into(), "s".into()]),
            CommandLanguageMatch::Command(CommandId::OpenServerDashboard)
        );
        assert_eq!(
            CommandRegistry::resolve_language(&["d".into(), "h".into()]),
            CommandLanguageMatch::Command(CommandId::PreviousMonitorTab)
        );
        assert_eq!(
            CommandRegistry::resolve_language(&["d".into(), "l".into()]),
            CommandLanguageMatch::Command(CommandId::NextMonitorTab)
        );

        let mut button_ids = HashSet::new();
        let state = DatabaseMonitorState::default();
        for view in DatabaseMonitorView::ALL {
            assert!(button_ids.insert(view.button_id()));
            assert!(!view.label(&state).is_empty());
            assert!(
                ["sift/postgres", "sift/sql-server", "sift/sqlite"]
                    .into_iter()
                    .any(|provider| view.available_for(Some(provider), true)),
                "{} has no supported keyboard route",
                view.button_id()
            );
        }

        for provider in ["sift/postgres", "sift/sql-server", "sift/sqlite"] {
            let expected = DatabaseMonitorView::ALL
                .into_iter()
                .filter(|view| view.available_for(Some(provider), true))
                .collect::<HashSet<_>>();
            let mut seen = HashSet::new();
            let mut view = DatabaseMonitorView::Overview;
            for _ in 0..expected.len() {
                view = view.adjacent_available(true, Some(provider), true);
                assert!(seen.insert(view), "Monitor navigation repeated a tab early");
                assert!(view.available_for(Some(provider), true));
                assert_eq!(
                    view.adjacent_available(false, Some(provider), true)
                        .adjacent_available(true, Some(provider), true),
                    view
                );
            }
            assert_eq!(seen, expected);
            assert_eq!(view, DatabaseMonitorView::Overview);
        }
        assert!(!DatabaseMonitorView::QueryStore.available_for(Some("sift/sql-server"), false));
    }

    #[test]
    fn postgres_diagnostics_ignore_stale_results_after_connection_change() {
        let mut monitor = DatabaseMonitorState::default();
        monitor.set_view(DatabaseMonitorView::Replication);
        let old_replication = monitor.start_replication();
        monitor.clear_postgres_diagnostics();
        monitor.set_view(DatabaseMonitorView::Replication);
        let current_replication = monitor.start_replication();
        monitor.finish_replication(old_replication, Err("previous connection".into()));
        assert!(monitor.replication_request().loading());
        monitor.finish_replication(current_replication, Err("current connection".into()));
        assert_eq!(
            monitor.replication_request().error(),
            Some("current connection")
        );

        monitor.set_view(DatabaseMonitorView::Statistics);
        let old_statistics = monitor.start_statistics();
        monitor.clear_postgres_diagnostics();
        monitor.set_view(DatabaseMonitorView::Statistics);
        let current_statistics = monitor.start_statistics();
        monitor.finish_statistics(old_statistics, 0, Err("previous connection".into()));
        assert!(monitor.statistics_request().loading());
        monitor.finish_statistics(current_statistics, 0, Err("current connection".into()));
        assert_eq!(
            monitor.statistics_request().error(),
            Some("current connection")
        );
    }

    fn process(process_id: i64, blocked_by: Vec<i64>) -> DatabaseProcess {
        DatabaseProcess {
            engine: sift_protocol::Engine::Postgres,
            process_id,
            user: None,
            database: None,
            state: None,
            statement: None,
            started_at: None,
            transaction_started_at: None,
            state_changed_at: None,
            wait: None,
            blocked_by,
            lock_wait: None,
            held_locks: Vec::new(),
            held_locks_truncated: false,
        }
    }

    #[test]
    fn lock_view_keeps_waiters_and_their_blockers() {
        let mut monitor = DatabaseMonitorState::default();
        let generation = monitor.start_loading();
        monitor.finish_loading(
            generation,
            Ok(vec![
                process(1, vec![]),
                process(2, vec![1]),
                process(3, vec![]),
            ]),
        );
        monitor.set_view(DatabaseMonitorView::Locks);
        assert_eq!(monitor.lock_process_count(), 2);
        assert_eq!(
            monitor
                .visible_processes()
                .into_iter()
                .map(|process| process.process_id)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        monitor.toggle(1);
        let generation = monitor.start_loading();
        monitor.finish_loading(generation, Ok(vec![process(1, vec![]), process(2, vec![])]));
        assert_eq!(monitor.selected(), None);
    }

    #[test]
    fn query_store_report_is_cleared_on_connection_change() {
        let mut monitor = DatabaseMonitorState::default();
        monitor.start_query_store();
        assert!(monitor.query_store_request().loading());
        monitor.finish_query_store(Ok(sift_protocol::QueryStoreReport {
            database: "app".into(),
            state: sift_protocol::QueryStoreState::ReadWrite,
            plans: vec![sift_protocol::QueryStorePlan {
                query_id: 1,
                plan_id: 2,
                sql_text: "SELECT secret".into(),
                executions: 1,
                average_duration_ms: 1.0,
                last_execution_at: None,
            }],
            truncated: false,
        }));
        assert!(monitor.query_store().is_some());
        monitor.clear_query_store();
        assert!(monitor.query_store().is_none());
        assert!(!monitor.query_store_request().loading());
    }

    #[test]
    fn alerts_prioritize_cycles_and_classify_slow_sessions() {
        let now = chrono::Utc::now();
        let mut long = process(10, vec![]);
        long.state = Some("active".into());
        long.statement = Some("select pg_sleep(60)".into());
        long.started_at = Some(now - chrono::Duration::seconds(31));
        let mut idle = process(11, vec![]);
        idle.state = Some("idle in transaction".into());
        idle.started_at = Some(now - chrono::Duration::seconds(61));
        let alerts = classify_alerts(
            &[long, idle, process(12, vec![13]), process(13, vec![12])],
            now,
        );
        assert_eq!(alerts.get(&10), Some(&DatabaseAlertKind::LongRunning));
        assert_eq!(alerts.get(&11), Some(&DatabaseAlertKind::IdleInTransaction));
        assert_eq!(alerts.get(&12), Some(&DatabaseAlertKind::DeadlockRisk));
        assert_eq!(alerts.get(&13), Some(&DatabaseAlertKind::DeadlockRisk));
    }

    #[test]
    fn deadlock_view_contains_only_cycle_participants() {
        let mut monitor = DatabaseMonitorState::default();
        let generation = monitor.start_loading();
        monitor.finish_loading(
            generation,
            Ok(vec![
                process(1, vec![2]),
                process(2, vec![1]),
                process(3, vec![2]),
            ]),
        );
        monitor.set_view(DatabaseMonitorView::Deadlocks);
        assert_eq!(monitor.deadlock_process_count(), 2);
        assert_eq!(
            monitor
                .visible_processes()
                .iter()
                .map(|process| process.process_id)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn process_termination_preview_expires_on_refresh_and_connection_change() {
        let mut monitor = DatabaseMonitorState::default();
        let generation = monitor.start_loading();
        let mut target = process(42, vec![]);
        target.engine = sift_protocol::Engine::SqlServer;
        target.user = Some("operator".into());
        monitor.finish_loading(generation, Ok(vec![target.clone()]));
        assert!(monitor.preview_termination(42));
        assert_eq!(
            monitor
                .termination_preview()
                .unwrap()
                .process
                .user
                .as_deref(),
            Some("operator")
        );
        let newer = monitor.start_loading();
        assert!(monitor.termination_preview().is_none());
        assert!(monitor.start_termination(42).is_none());
        monitor.finish_loading(generation, Ok(vec![process(99, vec![])]));
        assert_eq!(monitor.process_cursor(), Some(42));
        monitor.finish_loading(newer, Ok(vec![target]));
        assert!(monitor.preview_termination(42));
        let sequence = monitor.start_termination(42).unwrap();
        assert!(monitor.start_termination(42).is_none());
        monitor.clear_processes();
        assert!(!monitor.finish_termination(sequence, 42));
        assert!(monitor.visible_processes().is_empty());
    }

    #[test]
    fn process_cursor_tracks_visible_sessions() {
        let mut monitor = DatabaseMonitorState::default();
        let generation = monitor.start_loading();
        monitor.finish_loading(
            generation,
            Ok(vec![
                process(1, vec![]),
                process(2, vec![1]),
                process(3, vec![]),
            ]),
        );
        assert_eq!(monitor.process_cursor(), Some(1));
        monitor.move_process_cursor(1);
        assert_eq!(monitor.process_cursor(), Some(2));
        monitor.set_view(DatabaseMonitorView::Locks);
        assert_eq!(monitor.process_cursor(), Some(2));
        monitor.move_process_cursor(1);
        assert_eq!(monitor.process_cursor(), Some(2));
        monitor.move_process_cursor(-1);
        assert_eq!(monitor.process_cursor(), Some(1));
    }
}
