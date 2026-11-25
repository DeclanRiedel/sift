use super::*;

impl WorkspaceShell {
    pub fn desktop_update_restart_blocker(&self, cx: &App) -> Option<&'static str> {
        if !self.running_queries.is_empty()
            || self.transaction_state.transaction().is_some()
            || self.transaction_state.is_pending()
            || self.pending_transaction_control.is_some()
            || !self.outcome_unknown_targets.is_empty()
        {
            return Some("Finish queries and resolve transaction outcomes before restarting");
        }
        if self.modal.is_some()
            || self.result_edit_pending
            || self.catalog_migration_pending
            || self.instance_operation_pending
            || self.transfer.execution_pending
            || self.ai.pending
            || self.ai.database_pending
            || self.ai.sql_execution_pending
            || self.ai.query_execution_request.is_some()
            || self.ai.typed_execution_request.is_some()
            || self.ai.extension_execution_request.is_some()
            || self.ai.pending_room_commit.is_some()
        {
            return Some("Finish or cancel the current operation before restarting");
        }
        for pane in self.panes.iter().chain(
            self.workspace_sessions
                .values()
                .flat_map(|session| &session.panes),
        ) {
            let pane_state = pane.read(cx);
            if pane_state
                .items
                .iter()
                .any(|item| self.item_needs_close_confirmation(pane, item.id, cx))
            {
                return Some("Save or discard unsaved changes before restarting");
            }
            if pane_state
                .results
                .values()
                .any(|results| results.read(cx).export_in_progress())
            {
                return Some("Finish result exports before restarting");
            }
        }
        None
    }

    pub fn save_desktop_update_snapshot(
        &self,
        cx: &mut Context<Self>,
    ) -> Result<(PresentationState, Task<Result<(), String>>), String> {
        if let Some(reason) = self.desktop_update_restart_blocker(cx) {
            return Err(reason.into());
        }
        let store = self
            .store
            .as_ref()
            .ok_or("This window cannot restore its workspace after restart")?;
        let state = self.snapshot(cx);
        let save = store.prepare_save(state.clone());
        let task = cx.background_spawn(async move {
            save.commit()
                .map_err(|_| "Workspace could not be saved; restart cancelled".into())
        });
        Ok((state, task))
    }

    pub fn report_desktop_update_error(&mut self, message: String, cx: &mut Context<Self>) {
        self.show_error_toast(message, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};

    #[gpui::test]
    fn desktop_update_restart_preserves_dirty_and_running_work(cx: &mut TestAppContext) {
        let window = super::super::tests::shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let shell = window.root(&mut cx).unwrap();
        shell.update(&mut cx, |shell, cx| {
            assert!(shell.desktop_update_restart_blocker(cx).is_none());
            shell.running_queries.insert(1, 1);
            assert!(shell
                .desktop_update_restart_blocker(cx)
                .unwrap()
                .contains("queries"));
            shell.running_queries.clear();
            shell.transfer.execution_pending = true;
            assert!(shell
                .desktop_update_restart_blocker(cx)
                .unwrap()
                .contains("operation"));
            shell.transfer.execution_pending = false;
            shell.ai.database_pending = true;
            assert!(shell.desktop_update_restart_blocker(cx).is_some());
            shell.ai.database_pending = false;
            shell.panes[0].update(cx, |pane, _| pane.items[0].dirty = true);
            assert!(shell
                .desktop_update_restart_blocker(cx)
                .unwrap()
                .contains("unsaved"));
            shell.panes[0].update(cx, |pane, _| pane.items[0].dirty = false);
            assert!(shell.desktop_update_restart_blocker(cx).is_none());
        });
    }

    #[gpui::test]
    fn desktop_update_snapshot_is_durable_before_restart(cx: &mut TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(PresentationStore::new(
            root.path().join("presentation.json"),
        ));
        let window = super::super::tests::shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let shell = window.root(&mut cx).unwrap();
        let expected = shell.update(&mut cx, |shell, cx| {
            shell.store = Some(store.clone());
            shell.palette_recents = vec!["command:local:item.save".into()];
            let (state, save) = shell.save_desktop_update_snapshot(cx).unwrap();
            save.detach();
            state
        });
        cx.run_until_parked();
        assert_eq!(store.load(), expected);
    }
}
