use super::*;
use sift_protocol::{SaveBenchmarkRunRequest, SavedBenchmarkRun, SavedBenchmarkRunSummary};

#[derive(Debug, Clone)]
pub enum BenchmarkLibraryAction {
    List { cursor: Option<uuid::Uuid> },
    Save(Box<SaveBenchmarkRunRequest>),
    Get(uuid::Uuid),
    Delete(uuid::Uuid),
}

#[derive(Debug, Clone)]
pub enum BenchmarkLibraryReply {
    Page(sift_protocol::CursorPage<SavedBenchmarkRunSummary>),
    Saved(Box<SavedBenchmarkRun>),
    Loaded(Box<SavedBenchmarkRun>),
    Deleted(uuid::Uuid),
}

pub(super) struct BenchmarkLibraryState {
    instance_id: Option<String>,
    tenant_id: Option<i64>,
    pending: Option<uuid::Uuid>,
    items: Vec<SavedBenchmarkRunSummary>,
    next_cursor: Option<uuid::Uuid>,
    selected: usize,
    scroll: ScrollHandle,
    detail: Option<SavedBenchmarkRun>,
    candidate: Option<sift_protocol::BenchmarkReport>,
    name: Entity<TextInput>,
    delete_confirmation: Option<uuid::Uuid>,
    error: Option<String>,
}

impl BenchmarkLibraryState {
    pub(super) fn new(cx: &mut Context<WorkspaceShell>) -> Self {
        Self {
            instance_id: None,
            tenant_id: None,
            pending: None,
            items: Vec::new(),
            next_cursor: None,
            selected: 0,
            scroll: ScrollHandle::new(),
            detail: None,
            candidate: None,
            name: cx.new(|cx| {
                TextInput::new("", "Saved run name", cx).aria_label("Saved benchmark run name")
            }),
            delete_confirmation: None,
            error: None,
        }
    }
}

impl WorkspaceShell {
    pub(super) fn open_benchmark_library(
        &mut self,
        candidate: Option<sift_protocol::BenchmarkReport>,
        cx: &mut Context<Self>,
    ) {
        let Some(tenant) = self.selected_tenant_id() else {
            self.show_error_toast("Select a tenant to browse saved benchmarks".into(), cx);
            return;
        };
        self.benchmark_library = BenchmarkLibraryState::new(cx);
        self.benchmark_library.instance_id = self.selected_instance_id.clone();
        self.benchmark_library.tenant_id = Some(tenant);
        if let Some(report) = &candidate {
            self.benchmark_library.name.update(cx, |input, cx| {
                input.set_text(
                    format!(
                        "{:?} · {}",
                        report.engine,
                        report.captured_at.format("%Y-%m-%d %H:%M:%S")
                    ),
                    cx,
                )
            });
        }
        self.benchmark_library.candidate = candidate;
        self.modal = Some(Modal::BenchmarkLibrary);
        self.send_benchmark_library(BenchmarkLibraryAction::List { cursor: None }, cx);
    }

    fn send_benchmark_library(&mut self, action: BenchmarkLibraryAction, cx: &mut Context<Self>) {
        if self.benchmark_library.pending.is_some() {
            return;
        }
        let (Some(instance_id), Some(tenant_id)) = (
            self.benchmark_library.instance_id.clone(),
            self.benchmark_library.tenant_id,
        ) else {
            return;
        };
        if self.selected_instance_id.as_ref() != Some(&instance_id)
            || self.selected_tenant_id() != Some(tenant_id)
        {
            self.benchmark_library.error =
                Some("Server or tenant changed; reopen the saved-run browser".into());
            cx.notify();
            return;
        }
        let request_id = uuid::Uuid::new_v4();
        self.benchmark_library.error = None;
        if self.executor_sender.as_ref().is_some_and(|sender| {
            sender
                .send(ExecutorCommand::BenchmarkLibrary {
                    instance_id,
                    tenant_id,
                    request_id,
                    action,
                })
                .is_ok()
        }) {
            self.benchmark_library.pending = Some(request_id);
        } else {
            self.benchmark_library.error = Some("Server executor unavailable".into());
        }
        cx.notify();
    }

    pub(super) fn receive_benchmark_library(
        &mut self,
        instance_id: String,
        request_id: uuid::Uuid,
        result: Result<BenchmarkLibraryReply, String>,
        cx: &mut Context<Self>,
    ) {
        if self.benchmark_library.pending != Some(request_id)
            || self.selected_instance_id.as_ref() != Some(&instance_id)
            || self.benchmark_library.instance_id.as_ref() != Some(&instance_id)
            || self.selected_tenant_id() != self.benchmark_library.tenant_id
        {
            return;
        }
        self.benchmark_library.pending = None;
        match result {
            Err(error) => self.benchmark_library.error = Some(error),
            Ok(BenchmarkLibraryReply::Page(page)) => {
                self.benchmark_library.delete_confirmation = None;
                self.benchmark_library.items = page.items;
                self.benchmark_library.next_cursor = page
                    .next_cursor
                    .and_then(|id| uuid::Uuid::parse_str(&id).ok());
                self.benchmark_library.selected = 0;
                self.benchmark_library.scroll.scroll_to_item(0);
            }
            Ok(BenchmarkLibraryReply::Saved(saved)) => {
                self.benchmark_library.candidate = None;
                self.benchmark_library.detail = Some(*saved);
                self.show_toast("Benchmark saved privately".into(), cx);
                self.send_benchmark_library(BenchmarkLibraryAction::List { cursor: None }, cx);
            }
            Ok(BenchmarkLibraryReply::Loaded(saved)) => {
                self.benchmark_library.detail = Some(*saved)
            }
            Ok(BenchmarkLibraryReply::Deleted(id)) => {
                self.benchmark_library.delete_confirmation = None;
                if self
                    .benchmark_library
                    .detail
                    .as_ref()
                    .is_some_and(|detail| detail.id == id)
                {
                    self.benchmark_library.detail = None;
                }
                self.show_toast(
                    "Saved benchmark deleted; recovery requires a backup".into(),
                    cx,
                );
                self.send_benchmark_library(BenchmarkLibraryAction::List { cursor: None }, cx);
            }
        }
        cx.notify();
    }

    fn save_library_candidate(&mut self, cx: &mut Context<Self>) {
        let Some(report) = self.benchmark_library.candidate.clone() else {
            return;
        };
        let name = self
            .benchmark_library
            .name
            .read(cx)
            .text()
            .trim()
            .to_string();
        if name.is_empty() || name.len() > 200 {
            self.benchmark_library.error = Some("Name must be 1–200 bytes".into());
            cx.notify();
            return;
        }
        self.send_benchmark_library(
            BenchmarkLibraryAction::Save(Box::new(SaveBenchmarkRunRequest { name, report })),
            cx,
        );
    }

    fn load_selected_benchmark(&mut self, cx: &mut Context<Self>) {
        if let Some(item) = self
            .benchmark_library
            .items
            .get(self.benchmark_library.selected)
        {
            self.send_benchmark_library(BenchmarkLibraryAction::Get(item.id), cx);
        }
    }

    fn confirm_benchmark_delete(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.benchmark_library.delete_confirmation {
            self.send_benchmark_library(BenchmarkLibraryAction::Delete(id), cx);
        } else {
            self.benchmark_library.delete_confirmation = self
                .benchmark_library
                .items
                .get(self.benchmark_library.selected)
                .map(|item| item.id);
            cx.notify();
        }
    }

    fn reuse_benchmark_baseline(&mut self, cx: &mut Context<Self>) {
        if !self.benchmark_library_scope_current() {
            return;
        }
        let Some(saved) = &self.benchmark_library.detail else {
            return;
        };
        if let Some(results) = self.focused_pane_results(cx) {
            let report = saved.report.clone();
            results.update(cx, |view, cx| view.use_saved_benchmark_baseline(report, cx));
            if let Some(pane) = self.panes.get(self.active_pane) {
                pane.update(cx, |_, cx| cx.notify());
            }
            self.show_toast("Saved run pinned as this query's baseline".into(), cx);
        } else {
            self.show_error_toast("Open a query tab before pinning a baseline".into(), cx);
        }
    }

    pub(super) fn handle_benchmark_library_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal != Some(Modal::BenchmarkLibrary) {
            return;
        }
        if self
            .benchmark_library
            .name
            .read(cx)
            .focus_handle(cx)
            .is_focused(window)
        {
            match event.keystroke.key.as_str() {
                "enter" => {
                    self.save_library_candidate(cx);
                    self.focus_handle.focus(window, cx);
                }
                "escape" => self.focus_handle.focus(window, cx),
                _ => return,
            }
        } else {
            match event.keystroke.key.as_str() {
                "escape" => {
                    if self.benchmark_library.delete_confirmation.take().is_none() {
                        self.dismiss_modal(&DismissModal, window, cx);
                    }
                }
                "j" => {
                    self.benchmark_library.selected = (self.benchmark_library.selected + 1)
                        .min(self.benchmark_library.items.len().saturating_sub(1));
                    self.benchmark_library.delete_confirmation = None;
                }
                "k" => {
                    self.benchmark_library.selected =
                        self.benchmark_library.selected.saturating_sub(1);
                    self.benchmark_library.delete_confirmation = None;
                }
                "enter" if self.benchmark_library.delete_confirmation.is_some() => {
                    self.confirm_benchmark_delete(cx)
                }
                "enter" => self.load_selected_benchmark(cx),
                "d" if self.benchmark_library.delete_confirmation.is_none() => {
                    self.confirm_benchmark_delete(cx)
                }
                "r" => {
                    self.send_benchmark_library(BenchmarkLibraryAction::List { cursor: None }, cx)
                }
                "n" => {
                    if let Some(cursor) = self.benchmark_library.next_cursor {
                        self.send_benchmark_library(
                            BenchmarkLibraryAction::List {
                                cursor: Some(cursor),
                            },
                            cx,
                        );
                    }
                }
                "s" => self.save_library_candidate(cx),
                "c" if self.benchmark_library.candidate.is_some() => self
                    .benchmark_library
                    .name
                    .read(cx)
                    .focus_handle(cx)
                    .focus(window, cx),
                "b" => self.reuse_benchmark_baseline(cx),
                "y" => self.copy_saved_benchmark(cx),
                _ => {}
            }
        }
        cx.stop_propagation();
        self.benchmark_library
            .scroll
            .scroll_to_item(self.benchmark_library.selected);
        cx.notify();
    }

    fn copy_saved_benchmark(&self, cx: &mut Context<Self>) {
        if !self.benchmark_library_scope_current() {
            return;
        }
        if let Some(saved) = &self.benchmark_library.detail {
            if let Ok(json) = serde_json::to_string_pretty(saved) {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(json));
            }
        }
    }

    fn benchmark_library_scope_current(&self) -> bool {
        self.selected_instance_id == self.benchmark_library.instance_id
            && self.selected_tenant_id() == self.benchmark_library.tenant_id
    }

    pub(super) fn render_benchmark_library(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if !self.benchmark_library_scope_current() {
            return div()
                .child("Server or tenant changed. Reopen the saved-run browser.")
                .into_any_element();
        }
        let state = &self.benchmark_library;
        let pending = state.pending.is_some();
        let colors = cx.theme().colors;
        div().flex().flex_col().gap_2().max_h(px(720.))
            .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child("Saved benchmark runs · private to you"))
            .child(div().text_xs().text_color(colors.muted_text).child("Immutable user-saved snapshots, not server attestations. Bind values are omitted; no automatic rerun. Up to 500 runs / 64 MiB, retained until deleted. Backup both metadata and secrets."))
            .child(div().flex().flex_wrap().gap_2()
                .child(Button::new("benchmark-library-refresh","[r] Refresh / first page").disabled(pending).on_click(cx.listener(|shell,_,window,cx| { shell.focus_handle.focus(window,cx); shell.send_benchmark_library(BenchmarkLibraryAction::List { cursor: None },cx); })))
                .child(Button::new("benchmark-library-next","[n] Next page").disabled(pending || state.next_cursor.is_none()).on_click(cx.listener(|shell,_,_,cx| { if let Some(cursor)=shell.benchmark_library.next_cursor { shell.send_benchmark_library(BenchmarkLibraryAction::List { cursor: Some(cursor) },cx); } })))
                .child(Button::new("benchmark-library-open","[Enter] Open selected").disabled(pending || state.items.is_empty()).on_click(cx.listener(|shell,_,_,cx| shell.load_selected_benchmark(cx))))
                .child(Button::new("benchmark-library-delete",if state.delete_confirmation.is_some() { "Confirm permanent delete" } else { "[d] Delete selected" }).disabled(pending || state.items.is_empty()).on_click(cx.listener(|shell,_,_,cx| shell.confirm_benchmark_delete(cx)))))
            .children(state.candidate.as_ref().map(|_| div().flex().items_center().gap_2().child(state.name.clone()).child(Button::new("benchmark-library-save","[s] Save privately (includes SQL)").disabled(pending).on_click(cx.listener(|shell,_,window,cx| { shell.focus_handle.focus(window,cx); shell.save_library_candidate(cx); })))))
            .children(pending.then(|| div().text_sm().child("Loading / saving…")))
            .children(state.error.as_ref().map(|error| ErrorBanner::new(error.clone())))
            .children(state.delete_confirmation.map(|id| div().text_sm().text_color(colors.danger).child(format!("Permanently delete saved snapshot {id}? Enter confirms; Escape cancels. The database is not affected."))))
            .child(div().text_xs().child("j/k selects · Enter opens · c edits name · b pins opened run as baseline · y copies opened JSON (includes SQL)"))
            .child(div().id("saved-benchmark-list").max_h(px(200.)).overflow_y_scroll().track_scroll(&state.scroll).flex().flex_col().children(state.items.iter().enumerate().map(|(index,item)| {
                div().id(("saved-benchmark",index)).p_2().cursor(CursorStyle::PointingHand).when(index==state.selected,|row| row.bg(colors.active_surface))
                    .on_click(cx.listener(move |shell,_,window,cx| { shell.focus_handle.focus(window,cx); shell.benchmark_library.selected=index; shell.benchmark_library.delete_confirmation=None; shell.load_selected_benchmark(cx); }))
                    .child(format!("{} · {} · {} · median {} · {}",item.name,item.engine.map_or_else(|| "engine unavailable".into(), |engine| format!("{engine:?}")),if !item.payload_available {"payload unavailable"} else if item.completed {"complete"} else {"partial"},saved_ms(item.median_ns),item.saved_at.format("%Y-%m-%d %H:%M:%S")))
            })))
            .children((state.items.is_empty() && !pending).then(|| div().text_sm().child("No saved runs on this page.")))
            .children(state.detail.as_ref().map(|saved| {
                let report=&saved.report;
                div().flex().flex_col().gap_2()
                    .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(format!("Opened: {}",saved.name)))
                    .child(div().flex().gap_2().child(Button::new("benchmark-library-baseline","[b] Use as baseline").on_click(cx.listener(|shell,_,_,cx| shell.reuse_benchmark_baseline(cx)))).child(Button::new("benchmark-library-copy","[y] Copy JSON (includes SQL)").on_click(cx.listener(|shell,_,_,cx| shell.copy_saved_benchmark(cx)))))
                    .child(div().text_sm().child(format!("Median {} · mean {} · deviation {} · p95 {} · p99 {} · {} warm-ups / {} requested runs · {} ms timeout / {} ms budget / {} ms delay",saved_ms(report.median_ns),saved_ms(report.mean_ns),saved_ms(report.standard_deviation_ns),saved_ms(report.p95_ns.map(|v|v as f64)),saved_ms(report.p99_ns.map(|v|v as f64)),report.warmups,report.requested_iterations,report.query_timeout_ms,report.total_budget_ms,report.delay_ms)))
                    .child(div().id("saved-benchmark-sql").max_h(px(80.)).overflow_y_scroll().text_sm().child(bounded_preview(&report.sql,4096)))
                    .child(div().text_xs().text_color(colors.muted_text).child(bounded_preview(&report.warnings.join(" · "),2048)))
                    .child(div().text_xs().child("Run · phase · outcome · full drain · first row · rows"))
                    .child(gpui::uniform_list("saved-benchmark-samples",report.samples.len(),cx.processor(|shell,range: std::ops::Range<usize>,_,_| {
                        range.filter_map(|index| shell.benchmark_library.detail.as_ref()?.report.samples.get(index)).map(|sample| div().h(px(24.)).text_xs().child(format!("{} · {} · {:?} · {} · {} · {}",sample.ordinal+1,if sample.warmup {"warm-up"} else {"measured"},sample.outcome,saved_ms(Some(sample.elapsed_ns as f64)),saved_ms(sample.first_row_ns.map(|v|v as f64)),sample.rows.map_or_else(||"unavailable".into(),|rows|rows.to_string())))).collect::<Vec<_>>()
                    })).h(px(180.)))
            })).into_any_element()
    }
}

fn bounded_preview(text: &str, limit: usize) -> String {
    let mut characters = text.chars();
    let mut preview: String = characters.by_ref().take(limit).collect();
    if characters.next().is_some() {
        preview.push_str("… [preview truncated; copy JSON for full text]");
    }
    preview
}

fn saved_ms(value: Option<f64>) -> String {
    value.map_or_else(
        || "unavailable".into(),
        |ns| format!("{:.3} ms", ns / 1_000_000.),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};

    #[gpui::test]
    fn saved_browser_works_disconnected_and_requires_delete_confirmation(cx: &mut TestAppContext) {
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |window, cx| {
                cx.new(|cx| {
                    WorkspaceShell::new(
                        Default::default(),
                        Default::default(),
                        None,
                        None,
                        window,
                        cx,
                    )
                })
            })
            .unwrap()
        });
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let (sender, mut receiver) = ExecutorSender::channel(8);
        workspace.update_in(&mut cx, |shell, window, cx| {
            shell.selected_instance_id = Some("fixture".into());
            shell.lifecycle.tenants = vec![crate::TenantNavEntry {
                id: sift_api_types::TenantId(1),
                name: "Personal".into(),
                rooms: Vec::new(),
                connections: Vec::new(),
            }];
            shell.executor_sender = Some(sender);
            shell.run_command(CommandId::ShowBenchmarkLibrary, window, cx);
        });
        let request_id = match receiver.try_recv().unwrap() {
            ExecutorCommand::BenchmarkLibrary {
                request_id,
                tenant_id: 1,
                action: BenchmarkLibraryAction::List { cursor: None },
                ..
            } => request_id,
            _ => panic!("expected disconnected library request"),
        };
        let ids = [uuid::Uuid::new_v4(), uuid::Uuid::new_v4()];
        workspace.update(&mut cx, |shell, cx| {
            shell.receive_benchmark_library(
                "fixture".into(),
                uuid::Uuid::new_v4(),
                Err("stale".into()),
                cx,
            );
            assert_eq!(shell.benchmark_library.pending, Some(request_id));
            shell.receive_benchmark_library(
                "fixture".into(),
                request_id,
                Ok(BenchmarkLibraryReply::Page(sift_protocol::CursorPage {
                    items: ids
                        .iter()
                        .map(|id| SavedBenchmarkRunSummary {
                            id: *id,
                            saved_at: chrono::Utc::now(),
                            name: "fixture".into(),
                            engine: Some(sift_protocol::Engine::Postgres),
                            payload_available: true,
                            completed: true,
                            median_ns: Some(100.),
                        })
                        .collect(),
                    next_cursor: None,
                })),
                cx,
            );
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("j");
        cx.simulate_keystrokes("d");
        cx.run_until_parked();
        assert!(
            receiver.try_recv().is_err(),
            "first delete only requests confirmation"
        );
        workspace.read_with(&cx, |shell, _| {
            assert_eq!(shell.benchmark_library.delete_confirmation, Some(ids[1]))
        });
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(
            matches!(receiver.try_recv(),Ok(ExecutorCommand::BenchmarkLibrary {action:BenchmarkLibraryAction::Delete(id),..}) if id==ids[1])
        );
    }
}
