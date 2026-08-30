use sift_protocol::DatabaseProcess;

use super::RequestState;

#[derive(Debug, Default)]
pub(super) struct DatabaseMonitorState {
    processes: Vec<DatabaseProcess>,
    request: RequestState,
    selected: Option<i64>,
}

impl DatabaseMonitorState {
    pub(super) fn processes(&self) -> &[DatabaseProcess] {
        &self.processes
    }

    pub(super) fn request(&self) -> &RequestState {
        &self.request
    }

    pub(super) fn selected(&self) -> Option<i64> {
        self.selected
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

    pub(super) fn select(&mut self, process_id: i64) {
        self.selected = Some(process_id);
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
