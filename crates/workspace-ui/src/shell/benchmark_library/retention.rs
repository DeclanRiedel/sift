//! Reviewed owner-private automatic retention. All drafts retain browser scope.
use super::*;
use sift_protocol::{
    BenchmarkRetentionPolicy, BenchmarkRetentionPreview, BenchmarkRetentionState as PolicyState,
    BenchmarkRetentionUpdate, PreviewBenchmarkRetentionRequest, UpdateBenchmarkRetentionRequest,
};

pub(super) struct RetentionState {
    pub(super) open: bool,
    current: Option<PolicyState>,
    preview: Option<BenchmarkRetentionPreview>,
    age: Entity<TextInput>,
    count: Entity<TextInput>,
    status: Option<String>,
    _subscriptions: Vec<Subscription>,
}
impl RetentionState {
    pub(super) fn new(cx: &mut Context<WorkspaceShell>) -> Self {
        let age = cx.new(|cx| {
            TextInput::new("", "No age limit", cx)
                .aria_label("Benchmark retention maximum age in days")
        });
        let count = cx.new(|cx| {
            TextInput::new("", "No count limit", cx)
                .aria_label("Benchmark retention newest saved run count")
        });
        let age_focus = age.focus_handle(cx);
        let count_focus = count.focus_handle(cx);
        age.update(cx, |input, cx| {
            input.set_tab_targets(Some(count_focus.clone()), Some(count_focus), cx)
        });
        count.update(cx, |input, cx| {
            input.set_tab_targets(Some(age_focus.clone()), Some(age_focus), cx)
        });
        let subscriptions = [&age, &count]
            .into_iter()
            .map(|field| {
                cx.subscribe(field, |shell, emitter, event: &TextInputEvent, cx| {
                    let state = &shell.benchmark_library.retention;
                    if *event == TextInputEvent::Changed
                        && [state.age.entity_id(), state.count.entity_id()]
                            .contains(&emitter.entity_id())
                    {
                        shell.invalidate_benchmark_retention_review(cx);
                    } else if *event == TextInputEvent::Submitted
                        && [state.age.entity_id(), state.count.entity_id()]
                            .contains(&emitter.entity_id())
                    {
                        shell.preview_benchmark_retention_draft(cx);
                    }
                })
            })
            .collect();
        Self {
            open: false,
            current: None,
            preview: None,
            age,
            count,
            status: None,
            _subscriptions: subscriptions,
        }
    }
}
impl WorkspaceShell {
    pub(super) fn invalidate_benchmark_retention_review(&mut self, cx: &mut Context<Self>) {
        let state = &mut self.benchmark_library.retention;
        state.preview = None;
        state.status = None;
        cx.notify();
    }
    pub(super) fn toggle_benchmark_retention(&mut self, cx: &mut Context<Self>) {
        if !self.benchmark_library_scope_current() || self.benchmark_library.pending.is_some() {
            return;
        }
        if self.benchmark_library.annotation_draft.is_some() {
            self.benchmark_library.error =
                Some("Close the note/tag draft before editing retention".into());
            cx.notify();
            return;
        }
        self.benchmark_library.retention.open = !self.benchmark_library.retention.open;
        self.invalidate_benchmark_retention_review(cx);
        if self.benchmark_library.retention.open {
            self.send_benchmark_library(BenchmarkLibraryAction::GetRetention, cx);
        }
    }
    fn retention_draft(&self, cx: &Context<Self>) -> Result<BenchmarkRetentionPolicy, String> {
        let state = &self.benchmark_library.retention;
        let number = |input: &Entity<TextInput>| -> Result<Option<u32>, String> {
            let text = input.read(cx).text().trim();
            if text.is_empty() {
                return Ok(None);
            }
            if !text.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err("Retention limits need whole positive numbers or empty fields".into());
            }
            text.parse()
                .map(Some)
                .map_err(|_| "Retention limit is too large".into())
        };
        let policy = BenchmarkRetentionPolicy {
            max_age_days: number(&state.age)?,
            max_runs: number(&state.count)?,
        };
        policy.validate().map_err(str::to_string)?;
        Ok(policy)
    }
    pub(super) fn receive_benchmark_retention(
        &mut self,
        current: PolicyState,
        cx: &mut Context<Self>,
    ) {
        let state = &mut self.benchmark_library.retention;
        if !state.open {
            return;
        }
        let first = state.current.is_none();
        state.current = Some(current);
        state.preview = None;
        if first {
            self.reset_benchmark_retention_draft(cx);
        } else {
            state.status = Some(
                "Current policy reloaded; draft retained. Preview again before activation.".into(),
            );
        }
        cx.notify();
    }
    fn reset_benchmark_retention_draft(&mut self, cx: &mut Context<Self>) {
        let state = &mut self.benchmark_library.retention;
        let Some(current) = &state.current else {
            return;
        };
        state.age.update(cx, |input, cx| {
            input.set_text(
                current
                    .policy
                    .max_age_days
                    .map_or_else(String::new, |value| value.to_string()),
                cx,
            )
        });
        state.count.update(cx, |input, cx| {
            input.set_text(
                current
                    .policy
                    .max_runs
                    .map_or_else(String::new, |value| value.to_string()),
                cx,
            )
        });
        self.invalidate_benchmark_retention_review(cx);
    }
    fn preview_benchmark_retention_draft(&mut self, cx: &mut Context<Self>) {
        if !self.benchmark_library.retention.open || self.benchmark_library.pending.is_some() {
            return;
        }
        let Some(current) = &self.benchmark_library.retention.current else {
            return;
        };
        let revision = current.revision;
        let policy = match self.retention_draft(cx) {
            Ok(policy) => policy,
            Err(error) => {
                self.benchmark_library.error = Some(error);
                cx.notify();
                return;
            }
        };
        self.benchmark_library.retention.preview = None;
        self.send_benchmark_library(
            BenchmarkLibraryAction::PreviewRetention(PreviewBenchmarkRetentionRequest {
                expected_revision: revision,
                policy,
            }),
            cx,
        );
    }
    pub(super) fn receive_benchmark_retention_preview(
        &mut self,
        preview: BenchmarkRetentionPreview,
        cx: &mut Context<Self>,
    ) {
        let state = &self.benchmark_library.retention;
        if !state.open {
            return;
        }
        if state.current.as_ref().map(|current| current.revision) != Some(preview.revision)
            || self.retention_draft(cx).ok().as_ref() != Some(&preview.policy)
        {
            self.benchmark_library.retention.status =
                Some("Policy draft changed during preview; preview again".into());
            self.benchmark_library.retention.preview = None;
        } else {
            self.benchmark_library.retention.preview = Some(preview);
        }
        cx.notify();
    }
    fn apply_benchmark_retention_draft(&mut self, cx: &mut Context<Self>) {
        if !self.benchmark_library.retention.open || self.benchmark_library.pending.is_some() {
            return;
        }
        let Some(preview) = self.benchmark_library.retention.preview.clone() else {
            self.benchmark_library.error =
                Some("Preview the exact retention draft before activating it".into());
            cx.notify();
            return;
        };
        if self.retention_draft(cx).ok().as_ref() != Some(&preview.policy)
            || preview.expires_at < chrono::Utc::now()
        {
            self.invalidate_benchmark_retention_review(cx);
            self.benchmark_library.error =
                Some("Retention draft changed or review expired; preview again".into());
            return;
        }
        self.send_benchmark_library(
            BenchmarkLibraryAction::UpdateRetention(UpdateBenchmarkRetentionRequest {
                expected_revision: preview.revision,
                policy: preview.policy,
                as_of: preview.as_of,
                review_token: preview.review_token,
                confirmed: true,
            }),
            cx,
        );
    }
    pub(super) fn receive_benchmark_retention_update(
        &mut self,
        update: BenchmarkRetentionUpdate,
        cx: &mut Context<Self>,
    ) {
        let state = &mut self.benchmark_library.retention;
        state.current = Some(update.retention);
        state.preview = None;
        state.status=Some(format!("Policy activated; removed {} snapshots ({} payload bytes). Encrypted cleanup is queued; future saves and periodic maintenance enforce this policy.",update.removed,update.removed_bytes));
        self.benchmark_library.detail = None;
        self.benchmark_library.comparison_baseline = None;
        self.send_benchmark_library(BenchmarkLibraryAction::List { cursor: None }, cx);
        cx.notify();
    }
    pub(super) fn escape_benchmark_retention(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let state = &mut self.benchmark_library.retention;
        if !state.open {
            return false;
        }
        if [state.age.focus_handle(cx), state.count.focus_handle(cx)]
            .iter()
            .any(|focus| focus.is_focused(window))
        {
            self.focus_handle.focus(window, cx);
        } else {
            state.open = false;
            state.preview = None;
        }
        cx.notify();
        true
    }
    pub(super) fn handle_benchmark_retention_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let state = &self.benchmark_library.retention;
        if !state.open {
            return false;
        }
        let age = state.age.focus_handle(cx).is_focused(window);
        let count = state.count.focus_handle(cx).is_focused(window);
        let key = event.keystroke.unparse();
        if age || count {
            match key.as_str() {
                "escape" => {
                    self.escape_benchmark_retention(window, cx);
                }
                "enter" => {
                    self.focus_handle.focus(window, cx);
                    self.preview_benchmark_retention_draft(cx);
                }
                "tab" | "shift-tab" => {
                    let state = &self.benchmark_library.retention;
                    let target = if age { &state.count } else { &state.age };
                    target.focus_handle(cx).focus(window, cx);
                }
                _ => return true,
            }
        } else {
            match key.as_str() {
                "i" if self.benchmark_library.pending.is_none() => {
                    state.age.focus_handle(cx).focus(window, cx)
                }
                "v" => self.preview_benchmark_retention_draft(cx),
                "x" => self.apply_benchmark_retention_draft(cx),
                "r" => self.send_benchmark_library(BenchmarkLibraryAction::GetRetention, cx),
                "z" if self.benchmark_library.pending.is_none() => {
                    self.reset_benchmark_retention_draft(cx)
                }
                "p" | "escape" => {
                    self.escape_benchmark_retention(window, cx);
                }
                _ => return false,
            }
        }
        cx.stop_propagation();
        cx.notify();
        true
    }
    pub(super) fn render_benchmark_retention(&self, cx: &Context<Self>) -> gpui::AnyElement {
        let state = &self.benchmark_library.retention;
        let pending = self.benchmark_library.pending.is_some();
        let colors = cx.theme().colors;
        let description = state.current.as_ref().map_or_else(
            || "Inspect your current automatic retention policy".into(),
            |current| {
                format!(
                    "Current revision {} · age {} · count {}",
                    current.revision,
                    current
                        .policy
                        .max_age_days
                        .map_or_else(|| "unlimited".into(), |value| format!("{value} days")),
                    current
                        .policy
                        .max_runs
                        .map_or_else(|| "unlimited".into(), |value| format!("{value} newest"))
                )
            },
        );
        div().flex().flex_col().gap_1()
            .child(Button::new("benchmark-retention-open",if state.open {"[p] Close retention editor"} else {"[p] Retention policy"}).disabled(pending || self.benchmark_library.annotation_draft.is_some()).on_click(cx.listener(|shell,_,_,cx|shell.toggle_benchmark_retention(cx))))
            .child(div().text_xs().child(description))
            .children(state.open.then(||div().flex().flex_col().gap_2().border_1().border_color(colors.subtle_border).p_2()
                .child(div().text_sm().whitespace_normal().child("Automatic removal applies to your private saved measurements, notes, tags and source context. Age uses the original save time, including restored snapshots. Backups keep their own copies. Empty limits disable automatic retention."))
                .children(state.current.as_ref().map(|_|div().flex().flex_col().gap_1()
                    .child(div().text_xs().child("Maximum age (1–36500 days) · i edits; Tab switches fields; Escape returns to commands"))
                    .children((!pending).then(||state.age.clone()))
                    .child(div().text_xs().child("Keep newest (1–500 snapshots)"))
                    .children((!pending).then(||state.count.clone()))
                    .child(div().flex().gap_2()
                        .child(Button::new("benchmark-retention-preview","[v] Preview policy").disabled(pending).on_click(cx.listener(|shell,_,window,cx|{shell.focus_handle.focus(window,cx);shell.preview_benchmark_retention_draft(cx);})))
                        .child(Button::new("benchmark-retention-reset","[z] Reset draft to stored policy").disabled(pending).on_click(cx.listener(|shell,_,_,cx|shell.reset_benchmark_retention_draft(cx))))
                        .child(Button::new("benchmark-retention-reload","[r] Reload policy; keep draft").disabled(pending).on_click(cx.listener(|shell,_,_,cx|shell.send_benchmark_library(BenchmarkLibraryAction::GetRetention,cx)))))
                ))
                .children(state.preview.as_ref().map(|preview|div().id("benchmark-retention-review").debug_selector(||"benchmark-retention-review".into()).flex().flex_col().gap_1()
                    .child(format!("Remove {} saved snapshots / {} payload bytes now. Review expires {}. The reviewed criteria continue removing future expired/excess saves automatically.",preview.remove_count,preview.remove_bytes,preview.expires_at))
                    .child(Button::new("benchmark-retention-apply","[x] Activate reviewed policy and cleanup").disabled(pending).tone(ButtonTone::Accent).on_click(cx.listener(|shell,_,_,cx|shell.apply_benchmark_retention_draft(cx))))))
                .children(state.status.as_ref().map(|status|div().text_xs().whitespace_normal().child(status.clone())))
            )).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    #[gpui::test]
    fn vim_retention_requires_current_review_and_preserves_conflict_drafts(
        cx: &mut TestAppContext,
    ) {
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
        cx.update(|_, cx| {
            cx.bind_keys([
                gpui::KeyBinding::new("ctrl-a", sift_ui::SelectAll, Some("SiftTextInput")),
                gpui::KeyBinding::new("tab", sift_ui::Tab, Some("SiftTextInput")),
                gpui::KeyBinding::new("shift-tab", sift_ui::Backtab, Some("SiftTextInput")),
                gpui::KeyBinding::new("enter", sift_ui::Submit, Some("SiftTextInput")),
            ])
        });
        let workspace = window.root(&mut cx).unwrap();
        let (sender, mut commands) = ExecutorSender::channel(16);
        workspace.update_in(&mut cx, |shell, window, cx| {
            shell.selected_instance_id = Some("fixture".into());
            shell.lifecycle.tenants = vec![crate::TenantNavEntry {
                id: sift_api_types::TenantId(1),
                name: "Local".into(),
                rooms: vec![],
                connections: vec![],
            }];
            shell.executor_sender = Some(sender);
            shell.open_benchmark_library(None, cx);
            shell.focus_handle.focus(window, cx);
        });
        let request = match commands.try_recv().unwrap() {
            ExecutorCommand::BenchmarkLibrary { request_id, .. } => request_id,
            _ => panic!("list"),
        };
        workspace.update(&mut cx, |shell, cx| {
            shell.receive_benchmark_library(
                "fixture".into(),
                request,
                Ok(BenchmarkLibraryReply::Page(sift_protocol::CursorPage {
                    items: vec![],
                    next_cursor: None,
                })),
                cx,
            );
            shell.benchmark_library.annotation_draft = Some((uuid::Uuid::new_v4(), 0));
            shell.benchmark_library.notes.update(cx, |input, cx| {
                input.set_text("retained private note draft", cx)
            });
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("p");
        assert!(commands.try_recv().is_err());
        workspace.read_with(&cx, |shell, cx| {
            assert_eq!(
                shell.benchmark_library.notes.read(cx).text(),
                "retained private note draft"
            )
        });
        cx.simulate_keystrokes("escape p");
        let request = match commands.try_recv().unwrap() {
            ExecutorCommand::BenchmarkLibrary {
                request_id,
                action: BenchmarkLibraryAction::GetRetention,
                ..
            } => request_id,
            _ => panic!("read retention"),
        };
        workspace.update(&mut cx, |shell, cx| {
            shell.receive_benchmark_library(
                "fixture".into(),
                request,
                Ok(BenchmarkLibraryReply::Retention(Box::new(PolicyState {
                    policy: Default::default(),
                    revision: 0,
                    updated_at: None,
                }))),
                cx,
            )
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("i");
        cx.simulate_input("2");
        cx.simulate_keystrokes("tab");
        cx.simulate_input("1");
        cx.simulate_keystrokes("escape v");
        let (request, draft) = match commands.try_recv().unwrap() {
            ExecutorCommand::BenchmarkLibrary {
                request_id,
                action: BenchmarkLibraryAction::PreviewRetention(request),
                ..
            } => {
                assert_eq!(request.policy.max_age_days, Some(2));
                assert_eq!(request.policy.max_runs, Some(1));
                (request_id, request)
            }
            _ => panic!("preview"),
        };
        let preview = |draft: PreviewBenchmarkRetentionRequest| BenchmarkRetentionPreview {
            revision: draft.expected_revision,
            policy: draft.policy,
            as_of: chrono::Utc::now(),
            expires_at: chrono::Utc::now() + chrono::Duration::minutes(5),
            remove_count: 1,
            remove_bytes: 512,
            review_token: "a".repeat(64),
        };
        workspace.update(&mut cx, |shell, cx| {
            shell
                .benchmark_library
                .retention
                .age
                .update(cx, |input, cx| input.set_text("3", cx))
        });
        cx.run_until_parked();
        workspace.update(&mut cx, |shell, cx| {
            shell.receive_benchmark_library(
                "fixture".into(),
                request,
                Ok(BenchmarkLibraryReply::RetentionPreview(Box::new(preview(
                    draft,
                )))),
                cx,
            )
        });
        cx.run_until_parked();
        assert!(workspace.read_with(&cx, |shell, _| shell
            .benchmark_library
            .retention
            .preview
            .is_none()));
        assert!(cx.debug_bounds("benchmark-retention-review").is_none());
        cx.simulate_keystrokes("x");
        assert!(commands.try_recv().is_err());
        cx.simulate_keystrokes("v");
        let (request, draft) = match commands.try_recv().unwrap() {
            ExecutorCommand::BenchmarkLibrary {
                request_id,
                action: BenchmarkLibraryAction::PreviewRetention(request),
                ..
            } => (request_id, request),
            _ => panic!("new preview"),
        };
        workspace.update(&mut cx, |shell, cx| {
            shell.receive_benchmark_library(
                "fixture".into(),
                request,
                Ok(BenchmarkLibraryReply::RetentionPreview(Box::new(preview(
                    draft,
                )))),
                cx,
            )
        });
        cx.run_until_parked();
        assert!(workspace.read_with(&cx, |shell, _| shell
            .benchmark_library
            .retention
            .preview
            .is_some()));
        assert!(cx.debug_bounds("benchmark-retention-review").is_some());
        cx.simulate_keystrokes("i ctrl-a");
        cx.simulate_input("4");
        cx.simulate_keystrokes("escape x");
        assert!(commands.try_recv().is_err());
        cx.simulate_keystrokes("v");
        let (request, draft) = match commands.try_recv().unwrap() {
            ExecutorCommand::BenchmarkLibrary {
                request_id,
                action: BenchmarkLibraryAction::PreviewRetention(request),
                ..
            } => (request_id, request),
            _ => panic!("edited preview"),
        };
        workspace.update(&mut cx, |shell, cx| {
            shell.receive_benchmark_library(
                "fixture".into(),
                request,
                Ok(BenchmarkLibraryReply::RetentionPreview(Box::new(preview(
                    draft,
                )))),
                cx,
            )
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("x");
        let request = match commands.try_recv().unwrap() {
            ExecutorCommand::BenchmarkLibrary {
                request_id,
                action: BenchmarkLibraryAction::UpdateRetention(request),
                ..
            } => {
                assert!(request.confirmed);
                assert_eq!(request.expected_revision, 0);
                assert_eq!(request.policy.max_age_days, Some(4));
                request_id
            }
            _ => panic!("activate"),
        };
        workspace.update(&mut cx, |shell, cx| {
            shell.receive_benchmark_library(
                "fixture".into(),
                request,
                Err("revision conflict".into()),
                cx,
            )
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("r");
        let request = match commands.try_recv().unwrap() {
            ExecutorCommand::BenchmarkLibrary {
                request_id,
                action: BenchmarkLibraryAction::GetRetention,
                ..
            } => request_id,
            _ => panic!("reload"),
        };
        workspace.update(&mut cx, |shell, cx| {
            shell.receive_benchmark_library(
                "fixture".into(),
                uuid::Uuid::new_v4(),
                Err("late".into()),
                cx,
            );
            assert_eq!(shell.benchmark_library.pending, Some(request));
            shell.receive_benchmark_library(
                "fixture".into(),
                request,
                Ok(BenchmarkLibraryReply::Retention(Box::new(PolicyState {
                    policy: BenchmarkRetentionPolicy {
                        max_age_days: Some(7),
                        max_runs: Some(2),
                    },
                    revision: 2,
                    updated_at: None,
                }))),
                cx,
            );
            assert_eq!(shell.benchmark_library.retention.age.read(cx).text(), "4");
            assert_eq!(shell.benchmark_library.retention.count.read(cx).text(), "1");
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("v");
        let (request, draft) = match commands.try_recv().unwrap() {
            ExecutorCommand::BenchmarkLibrary {
                request_id,
                action: BenchmarkLibraryAction::PreviewRetention(request),
                ..
            } => {
                assert_eq!(request.expected_revision, 2);
                (request_id, request)
            }
            _ => panic!("revised preview"),
        };
        let accepted = preview(draft);
        workspace.update(&mut cx, |shell, cx| {
            shell.receive_benchmark_library(
                "fixture".into(),
                request,
                Ok(BenchmarkLibraryReply::RetentionPreview(Box::new(accepted))),
                cx,
            )
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("x");
        let (request, policy) = match commands.try_recv().unwrap() {
            ExecutorCommand::BenchmarkLibrary {
                request_id,
                action: BenchmarkLibraryAction::UpdateRetention(request),
                ..
            } => (request_id, request.policy),
            _ => panic!("revised apply"),
        };
        workspace.update(&mut cx, |shell, cx| {
            shell.receive_benchmark_library(
                "fixture".into(),
                request,
                Ok(BenchmarkLibraryReply::RetentionUpdated(Box::new(
                    BenchmarkRetentionUpdate {
                        retention: PolicyState {
                            policy,
                            revision: 3,
                            updated_at: None,
                        },
                        removed: 1,
                        removed_bytes: 512,
                    },
                ))),
                cx,
            )
        });
        let listed = match commands.try_recv().unwrap() {
            ExecutorCommand::BenchmarkLibrary {
                request_id,
                action: BenchmarkLibraryAction::List { .. },
                ..
            } => request_id,
            _ => panic!("refresh after policy update"),
        };
        workspace.update(&mut cx, |shell, cx| {
            shell.receive_benchmark_library(
                "fixture".into(),
                listed,
                Ok(BenchmarkLibraryReply::Page(sift_protocol::CursorPage {
                    items: vec![],
                    next_cursor: None,
                })),
                cx,
            )
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("i enter");
        let (late, draft) = match commands.try_recv().unwrap() {
            ExecutorCommand::BenchmarkLibrary {
                request_id,
                action: BenchmarkLibraryAction::PreviewRetention(request),
                ..
            } => {
                assert_eq!(request.expected_revision, 3);
                (request_id, request)
            }
            _ => panic!("field submit previews retention"),
        };
        workspace.update(&mut cx, |shell, cx| {
            shell.selected_instance_id = Some("other".into());
            shell.receive_benchmark_library(
                "fixture".into(),
                late,
                Ok(BenchmarkLibraryReply::RetentionPreview(Box::new(preview(
                    draft,
                )))),
                cx,
            );
            assert!(shell.benchmark_library.retention.preview.is_none());
        });
        workspace.update_in(&mut cx, |shell, window, cx| {
            shell.benchmark_library.pending = None;
            shell.selected_instance_id = Some("other".into());
            shell.preview_benchmark_retention_draft(cx);
            shell.focus_handle.focus(window, cx);
        });
        assert!(commands.try_recv().is_err());
    }
}
