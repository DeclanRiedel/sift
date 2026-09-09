use sift_protocol::DatabaseProcess;

use super::RequestState;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum DatabaseMonitorView {
    #[default]
    Activity,
    Locks,
    Alerts,
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
    request: RequestState,
    selected: Option<i64>,
    view: DatabaseMonitorView,
    alerts: std::collections::HashMap<i64, DatabaseAlertKind>,
}

impl DatabaseMonitorState {
    pub(super) fn request(&self) -> &RequestState {
        &self.request
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
            DatabaseMonitorView::Alerts => !self.alerts.contains_key(&selected),
        }) {
            self.selected = None;
        }
    }

    pub(super) fn visible_processes(&self) -> Vec<DatabaseProcess> {
        let included = match self.view {
            DatabaseMonitorView::Activity => return self.processes.clone(),
            DatabaseMonitorView::Locks => self.lock_process_ids(),
            DatabaseMonitorView::Alerts => self.alerts.keys().copied().collect(),
        };
        self.processes
            .iter()
            .filter(|process| included.contains(&process.process_id))
            .cloned()
            .collect()
    }

    pub(super) fn lock_process_count(&self) -> usize {
        self.lock_process_ids().len()
    }

    pub(super) fn alert_count(&self) -> usize {
        self.alerts.len()
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
}
