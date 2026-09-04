use sift_protocol::DatabaseProcess;

use super::RequestState;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum DatabaseMonitorView {
    #[default]
    Activity,
    Locks,
}

#[derive(Debug, Default)]
pub(super) struct DatabaseMonitorState {
    processes: Vec<DatabaseProcess>,
    request: RequestState,
    selected: Option<i64>,
    view: DatabaseMonitorView,
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
        if view == DatabaseMonitorView::Locks
            && self
                .selected
                .is_some_and(|selected| !self.lock_process_ids().contains(&selected))
        {
            self.selected = None;
        }
    }

    pub(super) fn visible_processes(&self) -> Vec<DatabaseProcess> {
        if self.view == DatabaseMonitorView::Activity {
            return self.processes.clone();
        }
        let lock_ids = self.lock_process_ids();
        self.processes
            .iter()
            .filter(|process| lock_ids.contains(&process.process_id))
            .cloned()
            .collect()
    }

    pub(super) fn lock_process_count(&self) -> usize {
        self.lock_process_ids().len()
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
                self.processes = processes;
                self.request.succeed();
            }
            Err(message) => self.request.fail(message),
        }
    }

    pub(super) fn terminated(&mut self, process_id: i64) {
        self.processes
            .retain(|process| process.process_id != process_id);
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
}
