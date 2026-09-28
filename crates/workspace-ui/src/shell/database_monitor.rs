use sift_protocol::{
    DatabaseDeadlockEvent, DatabaseProcess, PostgresSetting, PostgresSettingsPage,
};

use super::RequestState;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum DatabaseMonitorView {
    #[default]
    Activity,
    Locks,
    Deadlocks,
    History,
    Alerts,
    Settings,
    QueryStore,
    AgentJobs,
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
    processes: Vec<DatabaseProcess>,
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
    query_store: Option<sift_protocol::QueryStoreReport>,
    query_store_request: RequestState,
    agent_jobs: Option<sift_protocol::AgentJobsReport>,
    agent_jobs_request: RequestState,
    agent_jobs_selected: usize,
}

impl DatabaseMonitorState {
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

    pub(super) fn selected(&self) -> Option<i64> {
        self.selected
    }

    pub(super) const fn view(&self) -> DatabaseMonitorView {
        self.view
    }

    pub(super) fn set_view(&mut self, view: DatabaseMonitorView) {
        self.view = view;
        if self.selected.is_some_and(|selected| match view {
            DatabaseMonitorView::Activity => false,
            DatabaseMonitorView::Locks => !self.lock_process_ids().contains(&selected),
            DatabaseMonitorView::Deadlocks => !self.deadlock_process_ids().contains(&selected),
            DatabaseMonitorView::History => true,
            DatabaseMonitorView::Alerts => !self.alerts.contains_key(&selected),
            DatabaseMonitorView::Settings => true,
            DatabaseMonitorView::QueryStore => true,
            DatabaseMonitorView::AgentJobs => true,
        }) {
            self.selected = None;
        }
    }

    pub(super) fn visible_processes(&self) -> Vec<DatabaseProcess> {
        let included = match self.view {
            DatabaseMonitorView::Activity => return self.processes.clone(),
            DatabaseMonitorView::Locks => self.lock_process_ids(),
            DatabaseMonitorView::Deadlocks => self.deadlock_process_ids(),
            DatabaseMonitorView::History => return Vec::new(),
            DatabaseMonitorView::Alerts => self.alerts.keys().copied().collect(),
            DatabaseMonitorView::Settings => return Vec::new(),
            DatabaseMonitorView::QueryStore => return Vec::new(),
            DatabaseMonitorView::AgentJobs => return Vec::new(),
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

    pub(super) fn start_loading(&mut self) {
        self.request.start();
    }

    pub(super) fn fail_loading(&mut self, message: impl Into<String>) {
        self.request.fail(message);
    }

    pub(super) fn finish_loading(&mut self, result: Result<Vec<DatabaseProcess>, String>) {
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

    pub(super) fn terminated(&mut self, process_id: i64) {
        self.processes
            .retain(|process| process.process_id != process_id);
        self.alerts.remove(&process_id);
        if self.selected == Some(process_id) {
            self.selected = None;
        }
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
    use super::*;

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
        monitor.finish_loading(Ok(vec![
            process(1, vec![]),
            process(2, vec![1]),
            process(3, vec![]),
        ]));
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
        monitor.finish_loading(Ok(vec![process(1, vec![]), process(2, vec![])]));
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
        monitor.finish_loading(Ok(vec![
            process(1, vec![2]),
            process(2, vec![1]),
            process(3, vec![2]),
        ]));
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
}
