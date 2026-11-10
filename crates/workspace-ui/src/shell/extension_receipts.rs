//! Owner-private RPC receipts. Inspection never depends on a live SQL connection.
use super::*;

impl WorkspaceShell {
    pub(super) fn open_extension_receipts(&mut self, cx: &mut Context<Self>) {
        self.modal = Some(Modal::ExtensionContributions);
        let owner = self.lifecycle.identity.as_ref().map(|i| i.principal.id);
        let ui = &mut self.extension_contributions;
        ui.generation = ui.generation.saturating_add(1);
        ui.receipt_selection_id = ui
            .receipt_rows
            .get(ui.receipt_selected.unwrap_or(0))
            .map(|r| r.id);
        ui.receipt_rows.clear();
        ui.receipt_selected = None;
        ui.receipt_owner = owner;
        ui.show_receipts = true;
        ui.pending = false;
        ui.loading = true;
        ui.error = None;
        let command = ExecutorCommand::LoadExtensionReceipts {
            generation: ui.generation,
            instance_id: self
                .selected_instance_id
                .as_deref()
                .unwrap_or("local")
                .to_owned(),
        };
        if self
            .executor_sender
            .as_ref()
            .is_none_or(|s| s.send(command).is_err())
        {
            ui.loading = false;
            ui.error = Some("Receipt service is unavailable".into());
        }
        cx.notify();
    }

    pub(super) fn handle_extension_receipts_key(
        &mut self,
        event: &KeystrokeEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        if event.keystroke.modifiers.modified() {
            return false;
        }
        let key = event.keystroke.unparse();
        let ui = &mut self.extension_contributions;
        match key.as_str() {
            "j" | "down" if !ui.receipt_rows.is_empty() => {
                ui.receipt_selected = Some(
                    ui.receipt_selected
                        .map_or(0, |i| (i + 1).min(ui.receipt_rows.len() - 1)),
                )
            }
            "k" | "up" if !ui.receipt_rows.is_empty() => {
                ui.receipt_selected = Some(ui.receipt_selected.unwrap_or(0).saturating_sub(1))
            }
            "home" if !ui.receipt_rows.is_empty() => ui.receipt_selected = Some(0),
            "end" if !ui.receipt_rows.is_empty() => {
                ui.receipt_selected = Some(ui.receipt_rows.len() - 1)
            }
            "r" | "enter" => self.open_extension_receipts(cx),
            "b" => self.open_extension_contributions(cx),
            _ => return false,
        }
        cx.notify();
        true
    }

    pub(super) fn render_extension_receipts(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let ui = &self.extension_contributions;
        if ui.receipt_owner != self.lifecycle.identity.as_ref().map(|i| i.principal.id) {
            return div()
                .p_3()
                .child("Account changed; refresh execution receipts")
                .into_any_element();
        }
        let colors = cx.theme().colors;
        let rows = ui
            .receipt_rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                div()
                    .id(("extension-receipt-row", index))
                    .p_2()
                    .cursor_pointer()
                    .child(format!(
                        "{}{} · {:?}",
                        if ui.receipt_selected == Some(index) {
                            "› "
                        } else {
                            ""
                        },
                        row.operation_id,
                        row.state
                    ))
                    .on_click(cx.listener(move |shell, _, _, cx| {
                        shell.extension_contributions.receipt_selected = Some(index);
                        cx.notify();
                    }))
            })
            .collect::<Vec<_>>();
        let detail=ui.receipt_selected.and_then(|i|ui.receipt_rows.get(i)).map(|receipt| {
            div().p_3().flex().flex_col().gap_2()
                .child(extension_contributions::receipt_text(receipt))
                .child(format!("Request {} · tenant {} · {:?}",receipt.request_id,
                    receipt.tenant_id.map_or_else(||"instance".into(),|id|id.to_string()),receipt.classification))
                .child(format!("Created {} · observed {}",receipt.created_at,receipt.observed_at.as_deref().unwrap_or("pending")))
                .child("A response is not a database commit confirmation. Inspect the extension or database before issuing a new action after an unknown outcome.")
        });
        let back = Button::new("extension-receipts-back", "Actions")
            .tone(ButtonTone::Ghost)
            .on_click(cx.listener(|s, _, _, cx| s.open_extension_contributions(cx)));
        let refresh = Button::new("extension-receipts-refresh", "Refresh")
            .tone(ButtonTone::Neutral)
            .loading(ui.loading)
            .on_click(cx.listener(|s, _, _, cx| s.open_extension_receipts(cx)));
        let toolbar = div()
            .flex()
            .items_center()
            .justify_between()
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Extension execution receipts"),
            )
            .child(back)
            .child(refresh);
        div()
            .h(px(560.))
            .flex()
            .flex_col()
            .gap_3()
            .child(toolbar)
            .child(div().text_xs().text_color(colors.muted_text).child(
                "Own recent receipts · j/k select · Enter or r refresh · b actions · Esc close",
            ))
            .children(
                ui.error
                    .clone()
                    .map(|e| div().text_sm().text_color(colors.danger).child(e)),
            )
            .child(
                div()
                    .id("extension-receipts-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(rows)
                    .children(
                        (ui.receipt_rows.is_empty() && !ui.loading)
                            .then(|| div().p_3().child("No visible execution receipts")),
                    ),
            )
            .children(detail)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    fn receipt() -> sift_protocol::ExtensionExecutionReceipt {
        serde_json::from_value(serde_json::json!({
            "id":uuid::Uuid::new_v4(),"request_id":uuid::Uuid::new_v4(),"principal_id":1,"tenant_id":1,
            "operation_id":"acme/fixture/command/run#run","extension_id":"acme/fixture",
            "contribution_id":"acme/fixture/command/run","action":"run","classification":"read",
            "state":"outcome_unknown","created_at":"2026-11-10T00:00:00Z","deadline_unix_ms":1,
            "observed_at":"2026-11-10T00:00:01Z","frame_started":true,"response_kind":null,
            "late_response":false,"result_code":"request_timed_out","audit_id":1,
        })).unwrap()
    }
    #[gpui::test]
    fn extension_receipts_browse_without_a_connection_and_fence_refreshes(cx: &mut TestAppContext) {
        let window = super::super::tests::shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let (sender, mut receiver) = ExecutorSender::channel(8);
        let rows = vec![receipt(), receipt()];
        workspace.update(&mut cx, |s, cx| {
            s.executor_sender = Some(sender);
            s.selected_instance_id = Some("server-a".into());
            s.open_extension_receipts(cx);
        });
        let ExecutorCommand::LoadExtensionReceipts {
            generation,
            instance_id,
        } = receiver.try_recv().unwrap()
        else {
            panic!("receipt inspection only")
        };
        assert_eq!(instance_id, "server-a");
        workspace.update(&mut cx, |s, cx| {
            assert!(s.active_query_connection.is_none());
            s.on_executor_event(
                ExecutorEvent::ExtensionReceiptsLoaded {
                    generation,
                    instance_id: instance_id.clone(),
                    result: Ok(rows.clone()),
                },
                cx,
            );
            assert_eq!(s.extension_contributions.receipt_selected, Some(0));
        });
        cx.simulate_keystrokes("j enter");
        let ExecutorCommand::LoadExtensionReceipts {
            generation: next,
            instance_id,
        } = receiver.try_recv().unwrap()
        else {
            panic!("refresh cannot invoke an action")
        };
        workspace.update(&mut cx, |s, cx| {
            assert!(s.extension_contributions.receipt_rows.is_empty());
            s.on_executor_event(
                ExecutorEvent::ExtensionReceiptsLoaded {
                    generation,
                    instance_id: instance_id.clone(),
                    result: Ok(vec![receipt()]),
                },
                cx,
            );
            assert!(
                s.extension_contributions.receipt_rows.is_empty(),
                "stale reply ignored"
            );
            s.on_executor_event(
                ExecutorEvent::ExtensionReceiptsLoaded {
                    generation: next,
                    instance_id: instance_id.clone(),
                    result: Ok(vec![rows[1].clone(), rows[0].clone()]),
                },
                cx,
            );
            assert_eq!(s.extension_contributions.receipt_selected, Some(0));
            assert_eq!(s.extension_contributions.receipt_rows[0].id, rows[1].id);
            assert!(extension_contributions::receipt_text(&rows[1]).contains("do not retry"));
            s.selected_instance_id = Some("server-b".into());
            s.open_extension_receipts(cx);
            s.on_executor_event(
                ExecutorEvent::ExtensionReceiptsLoaded {
                    generation: s.extension_contributions.generation,
                    instance_id,
                    result: Ok(rows),
                },
                cx,
            );
            assert!(
                s.extension_contributions.receipt_rows.is_empty(),
                "foreign instance reply ignored"
            );
        });
        assert!(matches!(
            receiver.try_recv(),
            Ok(ExecutorCommand::LoadExtensionReceipts { .. })
        ));
        assert!(receiver.try_recv().is_err());
    }
}
