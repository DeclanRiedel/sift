use super::*;
use sift_core::performance::plan_comparison::{compare, PlanComparison, Row};

pub(super) struct PlanComparisonState {
    model: PlanComparison,
    selected: usize,
    inspect: bool,
    scroll: gpui::UniformListScrollHandle,
}
fn path(path: Option<&Vec<usize>>) -> String {
    path.map_or_else(
        || "absent".into(),
        |path| {
            format!(
                "root{}",
                path.iter().map(|n| format!("/{n}")).collect::<String>()
            )
        },
    )
}
fn label(row: &Row) -> String {
    let node = |n: Option<&sift_core::performance::plan_comparison::Node>| {
        n.map_or_else(
            || "absent".into(),
            |n| format!("{} {}", n.op, n.relation.as_deref().unwrap_or("")),
        )
    };
    format!(
        "{:?} · A {} {} → B {} {}{}",
        row.change,
        path(row.a_path.as_ref()),
        node(row.a.as_ref()),
        path(row.b_path.as_ref()),
        node(row.b.as_ref()),
        if row.positional_children {
            " · positional children (ambiguous keys or alignment limit)"
        } else {
            ""
        }
    )
}
impl WorkspaceShell {
    pub(super) fn open_benchmark_plan_comparison(&mut self, cx: &mut Context<Self>) {
        if !self.benchmark_library_scope_current()
            || self.benchmark_library.pending.is_some()
            || self.benchmark_library.plan_comparison_pending.is_some()
            || self.benchmark_library.annotation_draft.is_some()
            || self.benchmark_library.retention.open
        {
            return;
        }
        let (Some(a), Some(b)) = (
            &self.benchmark_library.comparison_baseline,
            &self.benchmark_library.detail,
        ) else {
            self.benchmark_library.error =
                Some("Pin run A and open run B before comparing attached plans".into());
            cx.notify();
            return;
        };
        let (a, b) = (a.clone(), b.clone());
        let source = (
            self.benchmark_library.instance_id.clone(),
            self.benchmark_library.tenant_id,
            a.id,
            b.id,
        );
        let request = uuid::Uuid::new_v4();
        self.benchmark_library.plan_comparison_pending = Some(request);
        self.benchmark_library.error = None;
        let work = cx
            .background_executor()
            .spawn(async move { compare(&a, &b) });
        cx.spawn(async move |shell, cx| {
            let result = work.await;
            let _ = shell.update(cx, |shell, cx| {
                if shell.benchmark_library.plan_comparison_pending != Some(request) {
                    return;
                }
                shell.benchmark_library.plan_comparison_pending = None;
                if !shell.benchmark_library_scope_current()
                    || shell.modal != Some(Modal::BenchmarkLibrary)
                    || shell.benchmark_library.instance_id != source.0
                    || shell.benchmark_library.tenant_id != source.1
                    || shell
                        .benchmark_library
                        .comparison_baseline
                        .as_ref()
                        .map(|run| run.id)
                        != Some(source.2)
                    || shell.benchmark_library.detail.as_ref().map(|run| run.id) != Some(source.3)
                {
                    cx.notify();
                    return;
                }
                match result {
                    Ok(model) => {
                        shell.benchmark_library.plan_comparison = Some(PlanComparisonState {
                            model,
                            selected: 0,
                            inspect: false,
                            scroll: gpui::UniformListScrollHandle::new(),
                        });
                        shell.benchmark_library.error = None;
                    }
                    Err(error) => shell.benchmark_library.error = Some(error.into()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    pub(super) fn handle_benchmark_plan_comparison_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.benchmark_library.plan_comparison.is_none() {
            return false;
        }
        if !self.benchmark_library_scope_current() {
            cx.stop_propagation();
            return true;
        }
        let state = self.benchmark_library.plan_comparison.as_mut().unwrap();
        match event.keystroke.unparse().as_str() {
            "escape" | "t" => {
                self.benchmark_library.plan_comparison = None;
            }
            "j" => {
                state.selected = (state.selected + 1).min(state.model.rows.len().saturating_sub(1));
                state
                    .scroll
                    .scroll_to_item(state.selected, gpui::ScrollStrategy::Top);
            }
            "k" => {
                state.selected = state.selected.saturating_sub(1);
                state
                    .scroll
                    .scroll_to_item(state.selected, gpui::ScrollStrategy::Top);
            }
            "enter" => state.inspect = !state.inspect,
            "y" => {
                if let Ok(json) = serde_json::to_string_pretty(&state.model) {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(json));
                }
            }
            _ => {}
        }
        cx.stop_propagation();
        cx.notify();
        true
    }
    pub(super) fn render_benchmark_plan_comparison(
        &self,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let state = self.benchmark_library.plan_comparison.as_ref().unwrap();
        let model = &state.model;
        div().id("saved-plan-comparison-scroll").max_h(px(720.)).overflow_y_scroll().debug_selector(||"saved-plan-comparison".into()).flex().flex_col().gap_2().min_h_0()
            .child("Saved Profile plan comparison · heuristic correspondences · no execution")
            .child(format!("{:?} · {} · added {} / removed {} / changed {} / unchanged {} · {} positional sibling groups",model.engine,if model.analyzed {"analyzed"} else {"estimated"},model.added,model.removed,model.changed,model.unchanged,model.positional_groups))
            .child(format!("A {} at {} · native planning {} / execution {} · Sift {} ns",model.a.run_id,model.a.captured_at,native_ms(model.a.native_planning_ms),native_ms(model.a.native_execution_ms),model.a.sift_elapsed_ns))
            .child(format!("B {} at {} · native planning {} / execution {} · Sift {} ns",model.b.run_id,model.b.captured_at,native_ms(model.b.native_planning_ms),native_ms(model.b.native_execution_ms),model.b.sift_elapsed_ns))
            .child(div().text_xs().whitespace_normal().child(model.warnings.join(" · ")))
            .child("j/k select · Enter inspect selected node · y copy private comparison JSON (includes identifiers/attributes) · Escape return")
            .child(gpui::uniform_list("saved-plan-comparison-rows",model.rows.len(),cx.processor(|shell,range:std::ops::Range<usize>,_,_|{
                let state=shell.benchmark_library.plan_comparison.as_ref().unwrap();
                range.filter_map(|index|state.model.rows.get(index).map(|row|div().h(px(30.)).overflow_hidden().text_sm().child(format!("{} {}",if index==state.selected {">"}else{" "},bounded_preview(&label(row),1024))))).collect::<Vec<_>>()
            })).h(px(260.)).track_scroll(&state.scroll))
            .children(state.inspect.then(||div().id("saved-plan-node-detail").max_h(px(180.)).overflow_y_scroll().text_xs().whitespace_normal().child(state.model.rows.get(state.selected).and_then(|row|serde_json::to_string_pretty(row).ok()).map_or_else(String::new,|text|bounded_preview(&text,16384)))))
            .into_any_element()
    }
}

fn native_ms(value: Option<f64>) -> String {
    value.map_or_else(|| "unavailable".into(), |value| format!("{value:.3} ms"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    fn saved(operator: &str) -> SavedBenchmarkRun {
        serde_json::from_value(serde_json::json!({"id":uuid::Uuid::new_v4(),"saved_at":chrono::Utc::now(),"name":"private fixture","report":{"version":2,"run_id":uuid::Uuid::new_v4(),"engine":"postgres","sql":"SELECT 1","captured_at":chrono::Utc::now(),"parameter_count":0,"warmups":0,"requested_iterations":1,"query_timeout_ms":100,"total_budget_ms":1000,"delay_ms":0,"samples":[],"completed":false,"warnings":[]},"profile":{"captured_at":chrono::Utc::now(),"sql":"SELECT 1","parameter_count":0,"response":{"run_id":uuid::Uuid::new_v4(),"plan":{"engine":"postgres","analyzed":true,"raw":"raw-plan-excluded","root":{"op":"Root","children":[{"op":operator,"extra":{"Filter":"private-attribute"}}]}},"server_elapsed_ns":100}}})).unwrap()
    }
    #[gpui::test]
    fn vim_saved_plan_comparison_navigates_copies_and_fences_stale_scope(cx: &mut TestAppContext) {
        let window = super::super::super::tests::shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let (sender, mut commands) = ExecutorSender::channel(8);
        let a = saved("Scan");
        let b = saved("Index Scan");
        workspace.update_in(&mut cx, |shell, window, cx| {
            shell.selected_instance_id = Some("fixture".into());
            shell.lifecycle.tenants = vec![crate::TenantNavEntry {
                id: sift_api_types::TenantId(1),
                name: "Personal".into(),
                rooms: vec![],
                connections: vec![],
            }];
            shell.executor_sender = Some(sender);
            shell.benchmark_library.instance_id = Some("fixture".into());
            shell.benchmark_library.tenant_id = Some(1);
            shell.benchmark_library.detail = Some(a.clone());
            shell.modal = Some(Modal::BenchmarkLibrary);
            shell.focus_handle.focus(window, cx);
        });
        cx.simulate_keystrokes("t");
        assert!(workspace.read_with(&cx, |shell, _| shell.benchmark_library.error.is_some()));
        cx.simulate_keystrokes("a");
        workspace.update(&mut cx, |shell, _| shell.benchmark_library.detail = Some(b));
        cx.simulate_keystrokes("t");
        cx.run_until_parked();
        assert!(cx.debug_bounds("saved-plan-comparison").is_some());
        cx.simulate_keystrokes("j");
        cx.simulate_keystrokes("enter");
        workspace.read_with(&cx, |shell, _| {
            let state = shell.benchmark_library.plan_comparison.as_ref().unwrap();
            assert_eq!(state.selected, 1);
            assert!(state.inspect);
            assert_eq!(state.model.changed, 1);
        });
        cx.simulate_keystrokes("y");
        let copied = cx.update(|_, cx| cx.read_from_clipboard().unwrap().text().unwrap());
        assert!(copied.contains("private-attribute"));
        assert!(!copied.contains("raw-plan-excluded"));
        workspace.update(&mut cx, |shell, _| {
            shell.selected_instance_id = Some("other".into())
        });
        cx.simulate_keystrokes("j");
        cx.simulate_keystrokes("y");
        assert_eq!(
            cx.update(|_, cx| cx.read_from_clipboard().unwrap().text().unwrap()),
            copied
        );
        assert_eq!(
            workspace.read_with(&cx, |shell, _| shell
                .benchmark_library
                .plan_comparison
                .as_ref()
                .unwrap()
                .selected),
            1
        );
        workspace.update_in(&mut cx, |shell, window, cx| {
            shell.selected_instance_id = Some("fixture".into());
            shell.focus_handle.focus(window, cx);
        });
        cx.simulate_keystrokes("escape");
        assert!(workspace.read_with(&cx, |shell, _| shell
            .benchmark_library
            .plan_comparison
            .is_none()
            && shell.modal == Some(Modal::BenchmarkLibrary)));
        cx.simulate_keystrokes("t");
        workspace.update(&mut cx, |shell, cx| {
            let request = uuid::Uuid::new_v4();
            shell.benchmark_library.pending = Some(request);
            shell.receive_benchmark_library(
                "fixture".into(),
                request,
                Ok(BenchmarkLibraryReply::Loaded(Box::new(a))),
                cx,
            );
            assert!(shell.benchmark_library.plan_comparison.is_none());
        });
        cx.run_until_parked();
        assert!(workspace.read_with(&cx, |shell, _| shell
            .benchmark_library
            .plan_comparison
            .is_none()));
        assert!(
            commands.try_recv().is_err(),
            "comparison and copy perform no network or execution dispatch"
        );
    }
}
