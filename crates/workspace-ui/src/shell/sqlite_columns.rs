//! SQLite removal remains a typed preview followed by explicit destructive review.
use super::*;

impl WorkspaceShell {
    pub(super) fn handle_table_column_review_key(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(designer) = self.table_designer.as_ref() else {
            return false;
        };
        let acknowledgement = designer.data_loss_focus.is_focused(window);
        let removal = designer.removal_focus.is_focused(window);
        let apply = designer.apply_focus.is_focused(window);
        if acknowledgement {
            match key {
                "space" | "enter" => self.toggle_table_column_loss_acknowledgement(cx),
                "tab" => designer.apply_focus.focus(window, cx),
                "shift-tab" | "escape" => designer.removal_focus.focus(window, cx),
                _ => return false,
            }
        } else if removal && matches!(key, "enter" | "space") {
            self.preview_sqlite_column_removal(cx);
        } else if apply {
            match key {
                "enter" | "space" => self.apply_table_change(cx),
                "shift-tab"
                    if designer.plan.as_ref().is_some_and(|plan| {
                        plan.required_acknowledgements
                            .contains(&sift_protocol::SchemaChangeRisk::DataLoss)
                    }) =>
                {
                    designer.data_loss_focus.focus(window, cx)
                }
                _ => return false,
            }
        } else {
            return false;
        }
        true
    }
    pub(super) fn preview_sqlite_column_removal(&mut self, cx: &mut Context<Self>) {
        if !self.sqlite_table_designer() {
            return;
        }
        let Some(designer) = self
            .table_designer
            .as_ref()
            .filter(|designer| !designer.pending)
        else {
            return;
        };
        let Some(column_id) = designer.selected_column.clone() else {
            return;
        };
        let item_id = designer.item_id;
        let Some(TableDefinitionState::Ready {
            graph, table_id, ..
        }) = self.table_definitions.get(&item_id)
        else {
            return;
        };
        if !graph.data.nodes.iter().any(|node| {
            node.id == column_id
                && node.kind == sift_protocol::CatalogNodeKind::Column
                && node.parent_id.as_ref() == Some(table_id)
        }) {
            return;
        }
        let revision = graph.revision;
        let Some(source) = self.table_action_source(item_id) else {
            self.show_error_toast(
                "Connect to this table before reviewing column removal".into(),
                cx,
            );
            return;
        };
        let sent = self.executor_sender.as_ref().is_some_and(|sender| {
            sender
                .send(ExecutorCommand::PreviewTableMutation {
                    item_id,
                    source: source.clone(),
                    expected_catalog_revision: revision,
                    mutation: sift_protocol::CatalogDiagramMutation::DropColumn { column_id },
                })
                .is_ok()
        });
        let designer = self.table_designer.as_mut().unwrap();
        designer.plan = None;
        designer.acknowledged_plan = None;
        designer.pending = sent;
        designer.request = sent.then_some(source);
        designer.error = (!sent).then(|| "Database executor is unavailable".into());
        cx.notify();
    }
    pub(super) fn toggle_table_column_loss_acknowledgement(&mut self, cx: &mut Context<Self>) {
        let Some(designer) = self
            .table_designer
            .as_mut()
            .filter(|designer| !designer.pending)
        else {
            return;
        };
        let Some(plan) = designer.plan.as_ref().filter(|plan| {
            plan.required_acknowledgements
                .contains(&sift_protocol::SchemaChangeRisk::DataLoss)
        }) else {
            return;
        };
        let identity = (plan.id, plan.digest.clone());
        designer.acknowledged_plan = if designer.acknowledged_plan.as_ref() == Some(&identity) {
            None
        } else {
            Some(identity)
        };
        designer.error = None;
        cx.notify();
    }
}
