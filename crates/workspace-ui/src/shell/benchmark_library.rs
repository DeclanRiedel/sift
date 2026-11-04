mod plan_comparison;
mod retention;
use super::*;
use sift_protocol::{SaveBenchmarkRunRequest, SavedBenchmarkRun, SavedBenchmarkRunSummary};

#[derive(Debug, Clone)]
pub enum BenchmarkLibraryAction {
    GetRetention,
    PreviewRetention(sift_protocol::PreviewBenchmarkRetentionRequest),
    UpdateRetention(sift_protocol::UpdateBenchmarkRetentionRequest),
    List {
        cursor: Option<uuid::Uuid>,
    },
    Save(Box<SaveBenchmarkRunRequest>),
    Get(uuid::Uuid),
    Delete(uuid::Uuid),
    UpdateAnnotations {
        id: uuid::Uuid,
        request: sift_protocol::UpdateBenchmarkRunAnnotationsRequest,
    },
}

#[derive(Debug, Clone)]
pub enum BenchmarkLibraryReply {
    Retention(Box<sift_protocol::BenchmarkRetentionState>),
    RetentionPreview(Box<sift_protocol::BenchmarkRetentionPreview>),
    RetentionUpdated(Box<sift_protocol::BenchmarkRetentionUpdate>),
    Page(sift_protocol::CursorPage<SavedBenchmarkRunSummary>),
    Saved(Box<SavedBenchmarkRun>),
    Loaded(Box<SavedBenchmarkRun>),
    Deleted(uuid::Uuid),
    Updated(Box<SavedBenchmarkRun>),
}

pub(super) struct BenchmarkLibraryState {
    plan_comparison: Option<plan_comparison::PlanComparisonState>,
    plan_comparison_pending: Option<uuid::Uuid>,
    retention: retention::RetentionState,
    instance_id: Option<String>,
    tenant_id: Option<i64>,
    pending: Option<uuid::Uuid>,
    items: Vec<SavedBenchmarkRunSummary>,
    next_cursor: Option<uuid::Uuid>,
    selected: usize,
    scroll: ScrollHandle,
    detail: Option<SavedBenchmarkRun>,
    comparison_baseline: Option<SavedBenchmarkRun>,
    candidate: Option<sift_protocol::BenchmarkReport>,
    candidate_context: Option<sift_protocol::BenchmarkSourceContext>,
    candidate_profile: Option<sift_protocol::BenchmarkProfileAttachment>,
    include_profile: bool,
    include_context: bool,
    name: Entity<TextInput>,
    notes: Entity<TextInput>,
    tags: Entity<TextInput>,
    annotation_draft: Option<(uuid::Uuid, u64)>,
    annotation_submission: Option<(String, String)>,
    annotation_status: Option<String>,
    annotation_subscriptions: Vec<Subscription>,
    delete_confirmation: Option<uuid::Uuid>,
    error: Option<String>,
}

impl BenchmarkLibraryState {
    pub(super) fn new(cx: &mut Context<WorkspaceShell>) -> Self {
        Self {
            plan_comparison: None,
            plan_comparison_pending: None,
            retention: retention::RetentionState::new(cx),
            instance_id: None,
            tenant_id: None,
            pending: None,
            items: Vec::new(),
            next_cursor: None,
            selected: 0,
            scroll: ScrollHandle::new(),
            detail: None,
            comparison_baseline: None,
            candidate: None,
            candidate_context: None,
            candidate_profile: None,
            include_profile: false,
            include_context: false,
            name: cx.new(|cx| {
                TextInput::new("", "Saved run name", cx).aria_label("Saved benchmark run name")
            }),
            notes: cx.new(|cx| {
                TextInput::new("", "Private notes (8 KiB maximum)", cx)
                    .multiline()
                    .aria_label("Private saved benchmark notes")
            }),
            tags: cx.new(|cx| {
                TextInput::new("", "Tags separated by commas", cx)
                    .aria_label("Private saved benchmark tags")
            }),
            annotation_draft: None,
            annotation_submission: None,
            annotation_status: None,
            annotation_subscriptions: Vec::new(),
            delete_confirmation: None,
            error: None,
        }
    }
}

impl WorkspaceShell {
    pub(super) fn capture_benchmark_source_context(
        &self,
        item_id: u64,
        cx: &App,
    ) -> Option<sift_protocol::BenchmarkSourceContext> {
        let document = self
            .panes
            .iter()
            .find_map(|pane| pane.read(cx).room_document_source(item_id))?;
        let snapshot = self.workspace_files.snapshot()?;
        if self.selected_instance_id.as_deref() != Some(document.instance_id.as_str())
            || self.selected_workspace_id != Some(snapshot.tree.workspace.id.0)
            || document.room_id != snapshot.tree.workspace.room_id
        {
            return None;
        }
        let node = snapshot
            .tree
            .nodes
            .iter()
            .find(|node| node.document_id == Some(document.document_id))?;
        let source = sift_protocol::VersionedExecutionContext {
            workspace_id: snapshot.tree.workspace.id,
            workspace_revision: snapshot.tree.workspace.revision,
            path: Some(node.path.clone()),
            checkpoint_id: None,
            git_commit: None,
            source_workflow: None,
        };
        let git = self
            .repository
            .observed_binding()
            .filter(|binding| binding.workspace_id == source.workspace_id)
            .and_then(|binding| {
                self.repository.status().filter(|status| {
                    status.binding_id == binding.id && status.binding_revision == binding.revision
                })
            })
            .map(|status| sift_protocol::BenchmarkSourceGit {
                observed_at: status.observed_at,
                workspace_revision: status.workspace_revision,
                head_oid: status.head_oid.clone(),
                branch: status.branch.clone(),
                dirty: if !status.entries.is_empty() {
                    Some(true)
                } else if status.truncated {
                    None
                } else {
                    Some(false)
                },
                truncated: status.truncated,
            });
        let context = sift_protocol::BenchmarkSourceContext {
            captured_at: chrono::Utc::now(),
            instance_id: self.selected_instance_id.clone()?,
            workspace_id: source.workspace_id,
            workspace_revision: source.workspace_revision,
            path: source.path?,
            git,
        };
        context.validate().ok()?;
        Some(context)
    }
    pub(super) fn open_benchmark_library_candidate(
        &mut self,
        report: sift_protocol::BenchmarkReport,
        context: Option<sift_protocol::BenchmarkSourceContext>,
        profile: Option<sift_protocol::BenchmarkProfileAttachment>,
        cx: &mut Context<Self>,
    ) {
        let can_open = self.selected_tenant_id().is_some();
        self.open_benchmark_library(Some(report), cx);
        if can_open {
            self.benchmark_library.candidate_context = context;
            self.benchmark_library.candidate_profile = profile;
        }
    }
    fn toggle_benchmark_source_context(&mut self, cx: &mut Context<Self>) {
        if !self.benchmark_library_scope_current()
            || self.benchmark_library.pending.is_some()
            || self.benchmark_library.candidate_context.is_none()
        {
            return;
        }
        self.benchmark_library.include_context = !self.benchmark_library.include_context;
        cx.notify();
    }

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
            self.benchmark_library.plan_comparison = None;
            self.benchmark_library.plan_comparison_pending = None;
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
        self.benchmark_library.plan_comparison = None;
        self.benchmark_library.plan_comparison_pending = None;
        match result {
            Err(error) => {
                self.invalidate_benchmark_retention_review(cx);
                self.benchmark_library.error = Some(error);
            }
            Ok(BenchmarkLibraryReply::Retention(state)) => {
                self.receive_benchmark_retention(*state, cx)
            }
            Ok(BenchmarkLibraryReply::RetentionPreview(preview)) => {
                self.receive_benchmark_retention_preview(*preview, cx)
            }
            Ok(BenchmarkLibraryReply::RetentionUpdated(update)) => {
                self.receive_benchmark_retention_update(*update, cx)
            }
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
                self.benchmark_library.candidate_context = None;
                self.benchmark_library.candidate_profile = None;
                self.benchmark_library.include_profile = false;
                self.benchmark_library.include_context = false;
                self.benchmark_library.annotation_draft = None;
                self.benchmark_library.annotation_status = None;
                self.benchmark_library.detail = Some(*saved);
                self.show_toast("Benchmark saved privately".into(), cx);
                self.send_benchmark_library(BenchmarkLibraryAction::List { cursor: None }, cx);
            }
            Ok(BenchmarkLibraryReply::Loaded(saved)) => {
                if self
                    .benchmark_library
                    .annotation_draft
                    .is_some_and(|(id, _)| id == saved.id)
                {
                    self.benchmark_library.annotation_draft =
                        Some((saved.id, saved.annotations.revision));
                    self.benchmark_library.annotation_status = Some("Latest stored revision loaded; draft retained. Review stored notes/tags before saving.".into());
                } else {
                    self.benchmark_library.annotation_draft = None;
                    self.benchmark_library.annotation_status = None;
                }
                self.benchmark_library.detail = Some(*saved)
            }
            Ok(BenchmarkLibraryReply::Updated(saved)) => {
                if self
                    .benchmark_library
                    .detail
                    .as_ref()
                    .is_some_and(|detail| detail.id == saved.id)
                {
                    let current = (
                        self.benchmark_library.notes.read(cx).text().to_string(),
                        self.benchmark_library.tags.read(cx).text().to_string(),
                    );
                    if self.benchmark_library.annotation_draft.is_none()
                        || self.benchmark_library.annotation_submission.as_ref() == Some(&current)
                    {
                        self.benchmark_library.annotation_draft = None;
                        self.benchmark_library.annotation_status =
                            Some("Private notes/tags saved; measurements unchanged.".into());
                    } else {
                        self.benchmark_library.annotation_draft =
                            Some((saved.id, saved.annotations.revision));
                        self.benchmark_library.annotation_status =
                            Some("Earlier draft saved; newer edits remain unsaved.".into());
                    }
                    for item in &mut self.benchmark_library.items {
                        if item.id == saved.id {
                            item.tags = saved.annotations.tags.clone();
                        }
                    }
                    self.benchmark_library.detail = Some(*saved);
                }
                self.benchmark_library.annotation_submission = None;
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
                    self.benchmark_library.annotation_draft = None;
                    self.benchmark_library.annotation_status = None;
                }
                if self
                    .benchmark_library
                    .comparison_baseline
                    .as_ref()
                    .is_some_and(|baseline| baseline.id == id)
                {
                    self.benchmark_library.comparison_baseline = None;
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

    fn edit_benchmark_annotations(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.benchmark_library.retention.open {
            self.benchmark_library.error =
                Some("Close the retention editor before editing notes/tags".into());
            cx.notify();
            return;
        }
        if !self.benchmark_library_scope_current() || self.benchmark_library.pending.is_some() {
            return;
        }
        if self.benchmark_library.annotation_draft.is_some() {
            self.benchmark_library
                .notes
                .focus_handle(cx)
                .focus(window, cx);
            return;
        }
        let Some(saved) = self.benchmark_library.detail.as_ref() else {
            return;
        };
        let (id, revision, notes, tags) = (
            saved.id,
            saved.annotations.revision,
            saved.annotations.notes.clone(),
            saved.annotations.tags.join(", "),
        );
        self.benchmark_library
            .notes
            .update(cx, |input, cx| input.set_text(notes, cx));
        self.benchmark_library
            .tags
            .update(cx, |input, cx| input.set_text(tags, cx));
        if self.benchmark_library.annotation_subscriptions.is_empty() {
            for input in [
                self.benchmark_library.notes.clone(),
                self.benchmark_library.tags.clone(),
            ] {
                let subscription = cx.subscribe_in(
                    &input,
                    window,
                    |shell, _, event: &TextInputEvent, window, cx| {
                        if *event == TextInputEvent::Submitted
                            && shell.modal == Some(Modal::BenchmarkLibrary)
                        {
                            shell.save_benchmark_annotations(cx);
                            shell.focus_handle.focus(window, cx);
                        }
                    },
                );
                self.benchmark_library
                    .annotation_subscriptions
                    .push(subscription);
            }
        }
        self.benchmark_library.annotation_draft = Some((id, revision));
        self.benchmark_library.delete_confirmation = None;
        self.benchmark_library.annotation_status = None;
        self.benchmark_library.error = None;
        self.benchmark_library
            .notes
            .focus_handle(cx)
            .focus(window, cx);
        cx.notify();
    }

    fn save_benchmark_annotations(&mut self, cx: &mut Context<Self>) {
        if !self.benchmark_library_scope_current() || self.benchmark_library.pending.is_some() {
            return;
        }
        let Some((id, expected_revision)) = self.benchmark_library.annotation_draft else {
            return;
        };
        if !self
            .benchmark_library
            .detail
            .as_ref()
            .is_some_and(|saved| saved.id == id && saved.annotations.revision == expected_revision)
        {
            return;
        }
        let notes = self.benchmark_library.notes.read(cx).text().to_string();
        let tags = self.benchmark_library.tags.read(cx).text().to_string();
        let mut request = sift_protocol::UpdateBenchmarkRunAnnotationsRequest {
            expected_revision,
            notes: notes.clone(),
            tags: if tags.trim().is_empty() {
                vec![]
            } else {
                tags.split(',').map(str::to_string).collect()
            },
        };
        if let Err(message) = request.normalize() {
            self.benchmark_library.error = Some(message.into());
            cx.notify();
            return;
        }
        self.benchmark_library.annotation_submission = Some((notes, tags));
        self.send_benchmark_library(
            BenchmarkLibraryAction::UpdateAnnotations { id, request },
            cx,
        );
    }

    fn toggle_benchmark_profile(&mut self, cx: &mut Context<Self>) {
        if self.benchmark_library_scope_current()
            && self.benchmark_library.pending.is_none()
            && self.benchmark_library.candidate_profile.is_some()
        {
            self.benchmark_library.include_profile = !self.benchmark_library.include_profile;
            cx.notify();
        }
    }
    fn open_saved_benchmark_profile(&mut self, cx: &mut Context<Self>) {
        if !self.benchmark_library_scope_current()
            || self.benchmark_library.annotation_draft.is_some()
        {
            return;
        }
        let Some(profile) = self
            .benchmark_library
            .detail
            .as_ref()
            .and_then(|saved| saved.profile.clone())
        else {
            return;
        };
        let Some(results) = self.focused_pane_results(cx) else {
            self.benchmark_library.error =
                Some("Open a query tab to view the saved Profile; no query will execute".into());
            cx.notify();
            return;
        };
        if results.update(cx, |view, cx| view.show_saved_profile(profile, cx)) {
            self.modal = None;
            self.show_toast(
                "Opened private caller-associated Profile; no workload executed".into(),
                cx,
            );
        } else {
            self.benchmark_library.error =
                Some("Wait for the active Benchmark/Profile before opening a saved plan".into());
        }
        cx.notify();
    }

    fn save_library_candidate(&mut self, cx: &mut Context<Self>) {
        if self.benchmark_library.annotation_draft.is_some() {
            return;
        }
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
            BenchmarkLibraryAction::Save(Box::new(SaveBenchmarkRunRequest {
                profile: self
                    .benchmark_library
                    .include_profile
                    .then(|| self.benchmark_library.candidate_profile.clone())
                    .flatten(),
                source_context: self
                    .benchmark_library
                    .include_context
                    .then(|| self.benchmark_library.candidate_context.clone())
                    .flatten(),
                name,
                report,
            })),
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
        if self.benchmark_library.annotation_draft.is_some() {
            self.benchmark_library.error =
                Some("Save or cancel the annotation draft before deleting a run".into());
            cx.notify();
            return;
        }
        self.benchmark_library.error = None;
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

    fn pin_saved_comparison_baseline(&mut self, cx: &mut Context<Self>) {
        if !self.benchmark_library_scope_current() {
            return;
        }
        if let Some(saved) = self.benchmark_library.detail.clone() {
            self.benchmark_library.plan_comparison = None;
            self.benchmark_library.plan_comparison_pending = None;
            self.benchmark_library.comparison_baseline = Some(saved);
            self.show_toast(
                "Saved run A pinned; open another saved run to compare".into(),
                cx,
            );
        }
    }

    pub(super) fn escape_benchmark_library(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let comparing = self
            .benchmark_library
            .plan_comparison_pending
            .take()
            .is_some();
        if self.benchmark_library.plan_comparison.take().is_some() || comparing {
            cx.notify();
            return;
        }
        if self.escape_benchmark_retention(window, cx) {
            return;
        }
        let input_focused = [
            &self.benchmark_library.notes,
            &self.benchmark_library.tags,
            &self.benchmark_library.name,
        ]
        .iter()
        .any(|input| input.focus_handle(cx).is_focused(window));
        if input_focused {
            self.focus_handle.focus(window, cx);
        } else if self.benchmark_library.annotation_draft.take().is_some() {
            self.benchmark_library.annotation_status = None;
            self.benchmark_library.error = None;
        } else if self.benchmark_library.delete_confirmation.take().is_none() {
            self.dismiss_modal(&DismissModal, window, cx);
        }
        cx.notify();
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
        if self.handle_benchmark_plan_comparison_key(event, cx) {
            return;
        }
        if self.handle_benchmark_retention_key(event, window, cx) {
            return;
        }
        let note_focus = self
            .benchmark_library
            .notes
            .focus_handle(cx)
            .is_focused(window);
        let tag_focus = self
            .benchmark_library
            .tags
            .focus_handle(cx)
            .is_focused(window);
        if self.benchmark_library.annotation_draft.is_some() && (note_focus || tag_focus) {
            match event.keystroke.key.as_str() {
                "escape" => self.focus_handle.focus(window, cx),
                "enter" => {
                    self.save_benchmark_annotations(cx);
                    self.focus_handle.focus(window, cx);
                }
                "tab" => {
                    if note_focus {
                        self.benchmark_library
                            .tags
                            .focus_handle(cx)
                            .focus(window, cx);
                    } else {
                        self.benchmark_library
                            .notes
                            .focus_handle(cx)
                            .focus(window, cx);
                    }
                }
                _ => return,
            }
            cx.stop_propagation();
            cx.notify();
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
                "escape" => self.escape_benchmark_library(window, cx),
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
                    let action = self
                        .benchmark_library
                        .annotation_draft
                        .map_or(BenchmarkLibraryAction::List { cursor: None }, |(id, _)| {
                            BenchmarkLibraryAction::Get(id)
                        });
                    self.send_benchmark_library(action, cx);
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
                "s" if self.benchmark_library.annotation_draft.is_some() => {
                    self.save_benchmark_annotations(cx)
                }
                "s" => self.save_library_candidate(cx),
                "o" => self.toggle_benchmark_source_context(cx),
                "f" => self.toggle_benchmark_profile(cx),
                "v" => self.open_saved_benchmark_profile(cx),
                "t" => self.open_benchmark_plan_comparison(cx),
                "p" => self.toggle_benchmark_retention(cx),
                "e" => self.edit_benchmark_annotations(window, cx),
                "c" if self.benchmark_library.candidate.is_some() => self
                    .benchmark_library
                    .name
                    .read(cx)
                    .focus_handle(cx)
                    .focus(window, cx),
                "b" => self.reuse_benchmark_baseline(cx),
                "a" => self.pin_saved_comparison_baseline(cx),
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
        if self.benchmark_library.plan_comparison.is_some() {
            return self.render_benchmark_plan_comparison(cx);
        }
        let state = &self.benchmark_library;
        let pending = state.pending.is_some();
        let colors = cx.theme().colors;
        div().id("benchmark-library-scroll").flex().flex_col().gap_2().min_h_0().max_h(px(720.)).overflow_y_scroll()
            .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child("Saved benchmark runs · private to you"))
            .child(div().text_xs().text_color(colors.muted_text).child("Immutable user-saved measurements with editable private notes/tags, not server attestations. No bind list is stored; optional raw plans may contain values. No automatic rerun. Up to 500 runs / 64 MiB. Opt-in retention may automatically remove snapshots, notes, plans and source context. Backup both metadata and secrets."))
            .child(div().flex().flex_wrap().gap_2()
                .child(Button::new("benchmark-library-refresh","[r] Refresh / first page").disabled(pending).on_click(cx.listener(|shell,_,window,cx| { shell.focus_handle.focus(window,cx); shell.send_benchmark_library(BenchmarkLibraryAction::List { cursor: None },cx); })))
                .child(Button::new("benchmark-library-next","[n] Next page").disabled(pending || state.next_cursor.is_none()).on_click(cx.listener(|shell,_,_,cx| { if let Some(cursor)=shell.benchmark_library.next_cursor { shell.send_benchmark_library(BenchmarkLibraryAction::List { cursor: Some(cursor) },cx); } })))
                .child(Button::new("benchmark-library-open","[Enter] Open selected").disabled(pending || state.items.is_empty()).on_click(cx.listener(|shell,_,_,cx| shell.load_selected_benchmark(cx))))
                .child(Button::new("benchmark-library-delete",if state.delete_confirmation.is_some() { "Confirm permanent delete" } else { "[d] Delete selected" }).disabled(pending || state.items.is_empty()).on_click(cx.listener(|shell,_,_,cx| shell.confirm_benchmark_delete(cx)))))
            .child(self.render_benchmark_retention(cx))
            .children(state.plan_comparison_pending.is_some().then(||div().text_sm().child("Comparing attached plans in the background… Escape cancels.")))
            .children(state.candidate.as_ref().map(|_| div().flex().items_center().gap_2().child(state.name.clone()).child(Button::new("benchmark-library-save","[s] Save privately (includes SQL)").disabled(pending).on_click(cx.listener(|shell,_,window,cx| { shell.focus_handle.focus(window,cx); shell.save_library_candidate(cx); })))))
            .children(state.candidate.as_ref().map(|_|div().flex().flex_col().gap_1()
                .child(Button::new("benchmark-library-context",if state.include_context {"[o] Include source context: on"} else {"[o] Include source context: off"}).disabled(pending || state.candidate_context.is_none()).on_click(cx.listener(|shell,_,_,cx|shell.toggle_benchmark_source_context(cx))))
                .child(div().text_xs().whitespace_normal().child(source_context_label(state.candidate_context.as_ref())))
                .child(Button::new("benchmark-library-include-profile",if state.include_profile {"[f] Include private Profile: on"} else {"[f] Include private Profile: off"}).disabled(pending || state.candidate_profile.is_none()).on_click(cx.listener(|shell,_,_,cx|shell.toggle_benchmark_profile(cx))))
                .child(div().text_xs().whitespace_normal().child(profile_attachment_label(state.candidate_profile.as_ref())))))
            .children(pending.then(|| div().text_sm().child("Loading / saving…")))
            .children(state.error.as_ref().map(|error| ErrorBanner::new(error.clone())))
            .children(state.annotation_status.as_ref().map(|message|div().text_sm().child(message.clone())))
            .children(state.delete_confirmation.map(|id| div().text_sm().text_color(colors.danger).child(format!("Permanently delete saved snapshot {id}? Enter confirms; Escape cancels. The database is not affected."))))
            .child(div().text_xs().child("j/k selects · Enter opens · c edits name · a pins run A for saved A/B · b pins query baseline · e edits notes/tags · y copies JSON (includes SQL/notes/context)"))
            .child(div().id("saved-benchmark-list").max_h(px(200.)).overflow_y_scroll().track_scroll(&state.scroll).flex().flex_col().children(state.items.iter().enumerate().map(|(index,item)| {
                div().id(("saved-benchmark",index)).p_2().cursor(CursorStyle::PointingHand).when(index==state.selected,|row| row.bg(colors.active_surface))
                    .on_click(cx.listener(move |shell,_,window,cx| { shell.focus_handle.focus(window,cx); shell.benchmark_library.selected=index; shell.benchmark_library.delete_confirmation=None; shell.load_selected_benchmark(cx); }))
                    .child(format!("{} · {} · {} · client median {} · {}",item.name,item.engine.map_or_else(|| "engine unavailable".into(), |engine| format!("{engine:?}")),if !item.payload_available {"payload unavailable"} else if item.completed {"complete"} else {"partial"},saved_ms(item.median_ns),item.saved_at.format("%Y-%m-%d %H:%M:%S")))
                    .children((!item.tags.is_empty()).then(||div().text_xs().child(bounded_preview(&item.tags.join(" · "),256))))
            })))
            .children((state.items.is_empty() && !pending).then(|| div().text_sm().child("No saved runs on this page.")))
            .children(state.detail.as_ref().map(|saved| {
                let report=&saved.report;
                div().flex().flex_col().gap_2()
                    .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(format!("Opened: {}",saved.name)))
                    .child(div().text_xs().whitespace_normal().child(source_context_label(saved.source_context.as_ref())))
                    .child(div().text_xs().whitespace_normal().child(profile_attachment_label(saved.profile.as_ref())))
                    .children(saved.profile.as_ref().map(|_|Button::new("benchmark-library-view-profile","[v] View saved Profile; no execution").disabled(pending || state.annotation_draft.is_some()).on_click(cx.listener(|shell,_,_,cx|shell.open_saved_benchmark_profile(cx)))))
                    .child(div().id("saved-benchmark-notes").max_h(px(80.)).overflow_y_scroll().text_sm().child(format!("Stored notes (revision {}): {}", saved.annotations.revision,bounded_preview(&saved.annotations.notes,1024))))
                    .child(div().id("saved-benchmark-tags").max_h(px(48.)).overflow_y_scroll().text_xs().child(format!("Tags: {}", bounded_preview(&saved.annotations.tags.join(" · "),1024))))
                    .child(Button::new("benchmark-library-edit-notes","[e] Edit private notes/tags").disabled(pending || state.annotation_draft.is_some()).on_click(cx.listener(|shell,_,window,cx|shell.edit_benchmark_annotations(window,cx))))
                    .children(state.annotation_draft.filter(|(id,_)|*id==saved.id).map(|(_,revision)| div().flex().flex_col().gap_2()
                        .child(format!("Annotation draft · revision {revision}. Enter/s saves; Shift-Enter inserts a note line; Tab switches fields; Escape returns to commands; r reloads stored revision and retains draft."))
                        .child(state.notes.clone()).child(state.tags.clone())
                        .child(Button::new("benchmark-library-save-notes","[s] Save notes/tags").disabled(pending).on_click(cx.listener(|shell,_,window,cx|{ shell.focus_handle.focus(window,cx); shell.save_benchmark_annotations(cx); })))))
                    .child(div().flex().gap_2().child(Button::new("benchmark-library-saved-a","[a] Pin run A").on_click(cx.listener(|shell,_,_,cx| shell.pin_saved_comparison_baseline(cx)))).child(Button::new("benchmark-library-baseline","[b] Use as query baseline").on_click(cx.listener(|shell,_,_,cx| shell.reuse_benchmark_baseline(cx)))).child(Button::new("benchmark-library-copy","[y] Copy JSON (includes SQL/notes/context/plans)").on_click(cx.listener(|shell,_,_,cx| shell.copy_saved_benchmark(cx)))))
                    .child(Button::new("benchmark-compare-plans","[t] Compare attached Profile plans").disabled(pending || state.annotation_draft.is_some() || state.comparison_baseline.is_none()).on_click(cx.listener(|shell,_,_,cx|shell.open_benchmark_plan_comparison(cx))))
                    .children(state.comparison_baseline.as_ref().map(|baseline| div().text_sm().child(format!("Saved A/B · A: {} · B: {}. {}", baseline.name, saved.name, crate::results::benchmark_comparison(&baseline.report, report)))))
                    .children(saved.report.environment.as_ref().map(|environment| div().text_xs().child(crate::results::performance_settings_label(&environment.session_settings, environment.observed_settings.as_ref()))))
                    .child(div().text_sm().child(format!("Client elapsed median {} · mean {} · deviation {} · p95 {} · p99 {} · {} warm-ups / {} requested runs · {} ms timeout / {} ms budget / {} ms delay",saved_ms(report.median_ns),saved_ms(report.mean_ns),saved_ms(report.standard_deviation_ns),saved_ms(report.p95_ns.map(|v|v as f64)),saved_ms(report.p99_ns.map(|v|v as f64)),report.warmups,report.requested_iterations,report.query_timeout_ms,report.total_budget_ms,report.delay_ms)))
                    .child(div().id("saved-benchmark-sql").max_h(px(80.)).overflow_y_scroll().text_sm().child(bounded_preview(&report.sql,4096)))
                    .child(div().text_xs().text_color(colors.muted_text).child(bounded_preview(&report.warnings.join(" · "),2048)))
                    .child(div().text_xs().child("Run · phase · outcome · client elapsed · first row · full consumption · database execution · rows"))
                    .child(gpui::uniform_list("saved-benchmark-samples",report.samples.len(),cx.processor(|shell,range: std::ops::Range<usize>,_,_| {
                        range.filter_map(|index| {
                            let report = &shell.benchmark_library.detail.as_ref()?.report;
                            let sample = report.samples.get(index)?;
                            let client = sample.client_elapsed_ns.unwrap_or(sample.elapsed_ns);
                            Some(div().h(px(24.)).text_xs().child(format!("{} · {} · {:?} · {} · {} · {} · {} · {}",sample.ordinal+1,if sample.warmup {"warm-up"} else {"measured"},sample.outcome,saved_ms(Some(client as f64)),saved_ms(sample.first_row_ns.map(|v|v as f64)),saved_ms(sample.full_consumption_ns.map(|v|v as f64)),saved_ms(sample.database_execution_ns.map(|v|v as f64)),sample.rows.map_or_else(||"unavailable".into(),|rows|rows.to_string()))))
                        }).collect::<Vec<_>>()
                    })).h(px(180.)))
            })).into_any_element()
    }
}

fn source_context_label(context: Option<&sift_protocol::BenchmarkSourceContext>) -> String {
    let Some(context) = context else {
        return "Source context unavailable or not included. Only workspace-bound benchmark queries can capture it.".into();
    };
    let git = context.git.as_ref().map_or_else(
        || "Git observation unavailable".into(),
        |git| {
            format!(
                "Cached Git at {} / workspace revision {}: branch {}, head {}, worktree {}{}",
                git.observed_at,
                git.workspace_revision.0,
                git.branch.as_deref().unwrap_or("unavailable"),
                git.head_oid.as_deref().unwrap_or("unavailable"),
                match git.dirty {
                    Some(true) => "dirty",
                    Some(false) => "clean",
                    None => "unknown",
                },
                if git.truncated {
                    " (status truncated)"
                } else {
                    ""
                }
            )
        },
    );
    format!("Client-reported source captured before review at {}: instance {}, workspace {} / revision {}, document {}. {}. Cached observations do not prove execution provenance, saved editor content or database/data equivalence.",context.captured_at,context.instance_id,context.workspace_id.0,context.workspace_revision.0,context.path.0,git)
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
                            tags: Vec::new(),
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
    #[gpui::test]
    fn annotations_keep_full_drafts_across_conflicts_refresh_and_late_replies(
        cx: &mut TestAppContext,
    ) {
        let window = super::super::tests::shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let (sender, mut commands) = ExecutorSender::channel(16);
        let id = uuid::Uuid::new_v4();
        let mut saved: SavedBenchmarkRun = serde_json::from_value(serde_json::json!({
            "id":id,"name":"private fixture","saved_at":"2026-01-01T00:00:00Z",
            "report":{"version":1,"run_id":uuid::Uuid::new_v4(),"engine":"postgres","sql":"SELECT 1","captured_at":"2026-01-01T00:00:00Z","warmups":0,"requested_iterations":1,"query_timeout_ms":100,"total_budget_ms":1000,"delay_ms":0,"parameter_count":0,"samples":[],"completed":false,"warnings":[]}
        })).unwrap();
        saved.annotations.notes = "full private text ".repeat(100);
        workspace.update_in(&mut cx, |shell, window, cx| {
            shell.selected_instance_id = Some("fixture".into());
            shell.lifecycle.tenants = vec![crate::TenantNavEntry {
                id: sift_api_types::TenantId(1),
                name: "Personal".into(),
                rooms: vec![],
                connections: vec![],
            }];
            shell.executor_sender = Some(sender);
            shell.open_benchmark_library(None, cx);
            shell.focus_handle.focus(window, cx);
        });
        let list = match commands.try_recv().unwrap() {
            ExecutorCommand::BenchmarkLibrary { request_id, .. } => request_id,
            _ => panic!("expected list"),
        };
        workspace.update(&mut cx, |shell, cx| {
            shell.receive_benchmark_library(
                "fixture".into(),
                list,
                Ok(BenchmarkLibraryReply::Page(sift_protocol::CursorPage {
                    items: vec![SavedBenchmarkRunSummary {
                        id,
                        saved_at: saved.saved_at,
                        name: saved.name.clone(),
                        engine: Some(saved.report.engine),
                        payload_available: true,
                        tags: vec![],
                        completed: false,
                        median_ns: None,
                    }],
                    next_cursor: None,
                })),
                cx,
            );
            shell.load_selected_benchmark(cx);
        });
        let opened = match commands.try_recv().unwrap() {
            ExecutorCommand::BenchmarkLibrary {
                request_id,
                action: BenchmarkLibraryAction::Get(opened),
                ..
            } => {
                assert_eq!(opened, id);
                request_id
            }
            _ => panic!("expected get"),
        };
        workspace.update(&mut cx, |shell, cx| {
            shell.receive_benchmark_library(
                "fixture".into(),
                opened,
                Ok(BenchmarkLibraryReply::Loaded(Box::new(saved.clone()))),
                cx,
            )
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("e");
        cx.run_until_parked();
        workspace.update(&mut cx, |shell, cx| {
            assert_eq!(
                shell.benchmark_library.notes.read(cx).text(),
                saved.annotations.notes
            );
            shell.confirm_benchmark_delete(cx);
            assert!(
                shell.benchmark_library.delete_confirmation.is_none(),
                "draft editing cannot arm permanent deletion"
            );
            shell
                .benchmark_library
                .notes
                .update(cx, |input, cx| input.set_text("local private draft", cx));
            shell
                .benchmark_library
                .tags
                .update(cx, |input, cx| input.set_text(" z, a, a ", cx));
        });
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        workspace.read_with(&cx, |shell, _| {
            assert_eq!(shell.modal, Some(Modal::BenchmarkLibrary));
            assert_eq!(shell.benchmark_library.annotation_draft, Some((id, 0)));
        });
        cx.simulate_keystrokes("s");
        cx.run_until_parked();
        let update = match commands.try_recv().unwrap() {
            ExecutorCommand::BenchmarkLibrary {
                request_id,
                action:
                    BenchmarkLibraryAction::UpdateAnnotations {
                        id: target,
                        request,
                    },
                ..
            } => {
                assert_eq!(target, id);
                assert_eq!(request.expected_revision, 0);
                assert_eq!(request.notes, "local private draft");
                assert_eq!(request.tags, vec!["a", "z"]);
                request_id
            }
            _ => panic!("expected typed annotation save"),
        };
        workspace.update(&mut cx, |shell, cx| {
            shell.receive_benchmark_library(
                "fixture".into(),
                update,
                Err("revision conflict".into()),
                cx,
            )
        });
        cx.simulate_keystrokes("r");
        cx.run_until_parked();
        let reload = match commands.try_recv().unwrap() {
            ExecutorCommand::BenchmarkLibrary {
                request_id,
                action: BenchmarkLibraryAction::Get(target),
                ..
            } => {
                assert_eq!(target, id);
                request_id
            }
            _ => panic!("expected revision refresh"),
        };
        saved.annotations.revision = 3;
        saved.annotations.notes = "remote stored note".into();
        workspace.update(&mut cx, |shell, cx| {
            shell.receive_benchmark_library(
                "fixture".into(),
                reload,
                Ok(BenchmarkLibraryReply::Loaded(Box::new(saved.clone()))),
                cx,
            );
            assert_eq!(
                shell.benchmark_library.notes.read(cx).text(),
                "local private draft"
            );
            assert_eq!(shell.benchmark_library.annotation_draft, Some((id, 3)));
            shell.selected_instance_id = Some("other".into());
            shell.save_benchmark_annotations(cx);
        });
        assert!(
            commands.try_recv().is_err(),
            "server changes cannot retarget private drafts"
        );
        workspace.update(&mut cx, |shell, _| {
            shell.selected_instance_id = Some("fixture".into());
        });
        cx.simulate_keystrokes("e");
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        let last = match commands.try_recv().unwrap() {
            ExecutorCommand::BenchmarkLibrary {
                request_id,
                action: BenchmarkLibraryAction::UpdateAnnotations { request, .. },
                ..
            } => {
                assert_eq!(request.expected_revision, 3);
                request_id
            }
            _ => panic!("expected explicit save after refresh"),
        };
        saved.annotations.revision = 4;
        saved.annotations.notes = "local private draft".into();
        saved.annotations.tags = vec!["a".into(), "z".into()];
        workspace.update(&mut cx, |shell, cx| {
            shell
                .benchmark_library
                .notes
                .update(cx, |input, cx| input.set_text("newer unsaved edit", cx));
            shell.receive_benchmark_library(
                "fixture".into(),
                update,
                Ok(BenchmarkLibraryReply::Updated(Box::new(saved.clone()))),
                cx,
            );
            assert_eq!(shell.benchmark_library.pending, Some(last));
            shell.receive_benchmark_library(
                "fixture".into(),
                last,
                Ok(BenchmarkLibraryReply::Updated(Box::new(saved))),
                cx,
            );
            assert_eq!(shell.benchmark_library.annotation_draft, Some((id, 4)));
            assert_eq!(
                shell.benchmark_library.notes.read(cx).text(),
                "newer unsaved edit"
            );
        });
    }
}

fn profile_attachment_label(profile: Option<&sift_protocol::BenchmarkProfileAttachment>) -> String {
    match profile {
        None => "No matching private Profile attachment. Capture Benchmark and Profile with the same reviewed workload and connection to offer one.".into(),
        Some(profile) => format!("Private caller-associated Profile {} captured {} · {} plan · Sift elapsed {:.3} ms · native execution {}. Separate instrumented population; matching data/cache conditions are unproven. Raw plans can expose literals, names and values; excluded from measurement exports and AI/MCP.",profile.response.run_id,profile.captured_at,if profile.response.plan.analyzed {"analyzed"} else {"estimated"},profile.response.server_elapsed_ns as f64 / 1_000_000.0,profile.response.execution_ms.map_or_else(||"unavailable".into(),|ms|format!("{ms:.3} ms"))),
    }
}

#[cfg(test)]
mod profile_tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    #[gpui::test]
    fn vim_profile_attachment_is_opt_in_frozen_and_opens_without_execution(
        cx: &mut TestAppContext,
    ) {
        let window = super::super::tests::shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let (sender, mut commands) = ExecutorSender::channel(16);
        let report: sift_protocol::BenchmarkReport=serde_json::from_value(serde_json::json!({"version":2,"run_id":uuid::Uuid::new_v4(),"engine":"postgres","sql":"SELECT 1","captured_at":chrono::Utc::now(),"parameter_count":0,"warmups":0,"requested_iterations":1,"query_timeout_ms":100,"total_budget_ms":1000,"delay_ms":0,"samples":[],"completed":false,"warnings":[]})).unwrap();
        let mut profile:sift_protocol::BenchmarkProfileAttachment=serde_json::from_value(serde_json::json!({"captured_at":chrono::Utc::now(),"sql":report.sql,"parameter_count":0,"response":{"run_id":uuid::Uuid::new_v4(),"plan":{"engine":"postgres","analyzed":true,"raw":"private-plan-frozen","root":{"op":"Result","actual_rows":1.0}},"server_elapsed_ns":100,"execution_ms":0.01}})).unwrap();
        let frozen = profile.response.run_id;
        workspace.update_in(&mut cx, |shell, window, cx| {
            shell.selected_instance_id = Some("fixture".into());
            shell.lifecycle.tenants = vec![crate::TenantNavEntry {
                id: sift_api_types::TenantId(1),
                name: "Personal".into(),
                rooms: vec![],
                connections: vec![],
            }];
            shell.executor_sender = Some(sender);
            shell.open_benchmark_library_candidate(report.clone(), None, Some(profile.clone()), cx);
            shell.focus_handle.focus(window, cx);
        });
        profile.response.run_id = uuid::Uuid::new_v4();
        let list = match commands.try_recv().unwrap() {
            ExecutorCommand::BenchmarkLibrary { request_id, .. } => request_id,
            _ => panic!("list"),
        };
        workspace.update(&mut cx, |shell, cx| {
            shell.receive_benchmark_library(
                "fixture".into(),
                list,
                Ok(BenchmarkLibraryReply::Page(sift_protocol::CursorPage {
                    items: vec![],
                    next_cursor: None,
                })),
                cx,
            )
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("s");
        let save = match commands.try_recv().unwrap() {
            ExecutorCommand::BenchmarkLibrary {
                request_id,
                action: BenchmarkLibraryAction::Save(request),
                ..
            } => {
                assert!(request.profile.is_none());
                request_id
            }
            _ => panic!("opt-out save"),
        };
        workspace.update(&mut cx, |shell, cx| {
            shell.receive_benchmark_library("fixture".into(), save, Err("retry fixture".into()), cx)
        });
        cx.simulate_keystrokes("f");
        cx.simulate_keystrokes("s");
        let (save, request) = match commands.try_recv().unwrap() {
            ExecutorCommand::BenchmarkLibrary {
                request_id,
                action: BenchmarkLibraryAction::Save(request),
                ..
            } => (request_id, request),
            _ => panic!("opt-in save"),
        };
        assert_eq!(request.profile.as_ref().unwrap().response.run_id, frozen);
        assert_ne!(frozen, profile.response.run_id);
        let saved = SavedBenchmarkRun {
            profile: request.profile,
            source_context: None,
            id: uuid::Uuid::new_v4(),
            saved_at: chrono::Utc::now(),
            name: request.name,
            report: request.report,
            annotations: Default::default(),
        };
        workspace.update(&mut cx, |shell, cx| {
            shell.receive_benchmark_library(
                "fixture".into(),
                save,
                Ok(BenchmarkLibraryReply::Saved(Box::new(saved))),
                cx,
            )
        });
        let list = match commands.try_recv().unwrap() {
            ExecutorCommand::BenchmarkLibrary {
                request_id,
                action: BenchmarkLibraryAction::List { .. },
                ..
            } => request_id,
            _ => panic!("refresh"),
        };
        workspace.update(&mut cx, |shell, cx| {
            shell.receive_benchmark_library(
                "fixture".into(),
                list,
                Ok(BenchmarkLibraryReply::Page(sift_protocol::CursorPage {
                    items: vec![],
                    next_cursor: None,
                })),
                cx,
            )
        });
        workspace.update(&mut cx, |shell, _| {
            shell.selected_instance_id = Some("other".into())
        });
        cx.simulate_keystrokes("v");
        assert!(workspace.read_with(&cx, |shell, _| shell.modal == Some(Modal::BenchmarkLibrary)));
        assert!(commands.try_recv().is_err());
        workspace.update_in(&mut cx, |shell, window, cx| {
            shell.selected_instance_id = Some("fixture".into());
            shell.focus_handle.focus(window, cx);
        });
        cx.simulate_keystrokes("v");
        cx.run_until_parked();
        assert!(workspace.read_with(&cx, |shell, _| shell.modal.is_none()));
        assert!(
            commands.try_recv().is_err(),
            "viewing a saved plan dispatches no execution or network operation"
        );
        assert!(cx.debug_bounds("saved-profile-notice").is_some());
    }
}
