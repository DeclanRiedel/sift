//! Private reusable benchmark definitions and explicit current-query reruns.

use super::*;
use sift_protocol::{
    RunBenchmarkDefinitionRequest, SaveBenchmarkDefinitionRequest, SavedBenchmarkDefinition,
    SavedBenchmarkDefinitionSummary, UpdateBenchmarkDefinitionRequest,
};

#[derive(Debug, Clone)]
pub enum BenchmarkDefinitionAction {
    List {
        cursor: Option<uuid::Uuid>,
    },
    Save(Box<SaveBenchmarkDefinitionRequest>),
    Get(uuid::Uuid),
    Update {
        id: uuid::Uuid,
        request: Box<UpdateBenchmarkDefinitionRequest>,
    },
    Delete(uuid::Uuid),
    Run {
        id: uuid::Uuid,
        item_id: u64,
        profile_id: i64,
        session_id: sift_protocol::SessionId,
        connection_id: sift_protocol::ConnectionId,
        request: RunBenchmarkDefinitionRequest,
    },
}

#[derive(Debug, Clone)]
pub enum BenchmarkDefinitionReply {
    Page(sift_protocol::CursorPage<SavedBenchmarkDefinitionSummary>),
    Saved(Box<SavedBenchmarkDefinition>),
    Loaded(Box<SavedBenchmarkDefinition>),
    Deleted(uuid::Uuid),
    Ran {
        item_id: u64,
        report: Box<sift_protocol::BenchmarkReport>,
    },
}

struct DefinitionEdit {
    id: uuid::Uuid,
    revision: u64,
    engine: sift_protocol::Engine,
    fields: Vec<Entity<TextInput>>,
}

const EDIT_LABELS: [&str; 8] = [
    "Name",
    "SQL",
    "Parameter count",
    "Warm-ups",
    "Measured runs",
    "Timeout (ms)",
    "Total budget (ms)",
    "Delay (ms)",
];

struct RunReview {
    source_context: Option<sift_protocol::BenchmarkSourceContext>,
    definition_id: uuid::Uuid,
    revision: u64,
    connection_epoch: u64,
    item_id: u64,
    profile_id: i64,
    session_id: sift_protocol::SessionId,
    connection_id: sift_protocol::ConnectionId,
    target: SemanticConnectionTarget,
    sql: String,
    params: Vec<Entity<TextInput>>,
    labels: Vec<String>,
}

struct RunningDefinition {
    source_context: Option<sift_protocol::BenchmarkSourceContext>,
    item_id: u64,
    profile_id: i64,
    session_id: sift_protocol::SessionId,
    connection_id: sift_protocol::ConnectionId,
    run_id: uuid::Uuid,
    connection_epoch: u64,
    target: SemanticConnectionTarget,
    sql: String,
}

pub(super) struct BenchmarkDefinitionState {
    pub(super) connection_epoch: u64,
    instance_id: Option<String>,
    tenant_id: Option<i64>,
    pending: Option<uuid::Uuid>,
    items: Vec<SavedBenchmarkDefinitionSummary>,
    next_cursor: Option<uuid::Uuid>,
    selected: usize,
    scroll: ScrollHandle,
    detail: Option<SavedBenchmarkDefinition>,
    candidate: Option<SaveBenchmarkDefinitionRequest>,
    name: Entity<TextInput>,
    delete_confirmation: Option<uuid::Uuid>,
    run_review: Option<RunReview>,
    edit: Option<DefinitionEdit>,
    running: Option<RunningDefinition>,
    progress: Option<sift_protocol::PerformanceProgress>,
    error: Option<String>,
}

impl BenchmarkDefinitionState {
    pub(super) fn new(cx: &mut Context<WorkspaceShell>) -> Self {
        Self {
            connection_epoch: 0,
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
                TextInput::new("", "Definition name", cx).aria_label("Benchmark definition name")
            }),
            delete_confirmation: None,
            run_review: None,
            edit: None,
            running: None,
            progress: None,
            error: None,
        }
    }
}

impl WorkspaceShell {
    pub(super) fn open_benchmark_definitions(
        &mut self,
        candidate: Option<SaveBenchmarkDefinitionRequest>,
        cx: &mut Context<Self>,
    ) {
        let Some(tenant_id) = self.selected_tenant_id() else {
            self.show_error_toast("Select a tenant to browse benchmark definitions".into(), cx);
            return;
        };
        self.benchmark_definitions = BenchmarkDefinitionState::new(cx);
        self.benchmark_definitions.instance_id = self.selected_instance_id.clone();
        self.benchmark_definitions.tenant_id = Some(tenant_id);
        if let Some(candidate) = &candidate {
            self.benchmark_definitions
                .name
                .update(cx, |input, cx| input.set_text(candidate.name.clone(), cx));
        }
        self.benchmark_definitions.candidate = candidate;
        self.modal = Some(Modal::BenchmarkDefinitions);
        self.send_benchmark_definition(BenchmarkDefinitionAction::List { cursor: None }, cx);
    }

    fn benchmark_definition_scope_current(&self) -> bool {
        self.selected_instance_id == self.benchmark_definitions.instance_id
            && self.selected_tenant_id() == self.benchmark_definitions.tenant_id
    }

    fn send_benchmark_definition(
        &mut self,
        action: BenchmarkDefinitionAction,
        cx: &mut Context<Self>,
    ) {
        if self.benchmark_definitions.pending.is_some()
            || (self.benchmark_definitions.edit.is_some()
                && !matches!(&action, BenchmarkDefinitionAction::Update { .. }))
        {
            return;
        }
        if !self.benchmark_definition_scope_current() {
            self.benchmark_definitions.error =
                Some("Server or tenant changed; reopen definitions".into());
            cx.notify();
            return;
        }
        let (Some(instance_id), Some(tenant_id)) = (
            self.benchmark_definitions.instance_id.clone(),
            self.benchmark_definitions.tenant_id,
        ) else {
            return;
        };
        let request_id = uuid::Uuid::new_v4();
        self.benchmark_definitions.error = None;
        if self.executor_sender.as_ref().is_some_and(|sender| {
            sender
                .send(ExecutorCommand::BenchmarkDefinitions {
                    instance_id,
                    tenant_id,
                    request_id,
                    action,
                })
                .is_ok()
        }) {
            self.benchmark_definitions.pending = Some(request_id);
        } else {
            self.benchmark_definitions.error = Some("Server executor unavailable".into());
        }
        cx.notify();
    }

    pub(super) fn receive_benchmark_definition(
        &mut self,
        instance_id: String,
        request_id: uuid::Uuid,
        result: Result<BenchmarkDefinitionReply, String>,
        cx: &mut Context<Self>,
    ) {
        if self.benchmark_definitions.pending != Some(request_id)
            || self.benchmark_definitions.instance_id.as_ref() != Some(&instance_id)
        {
            return;
        }
        self.benchmark_definitions.pending = None;
        if !self.benchmark_definition_scope_current() {
            self.benchmark_definitions.running = None;
            self.benchmark_definitions.run_review = None;
            self.benchmark_definitions.error =
                Some("Server or tenant changed; reopen definitions".into());
            cx.notify();
            return;
        }
        match result {
            Err(error) => {
                self.benchmark_definitions.running = None;
                self.benchmark_definitions.error = Some(error);
            }
            Ok(BenchmarkDefinitionReply::Page(page)) => {
                self.benchmark_definitions.items = page.items;
                self.benchmark_definitions.next_cursor = page
                    .next_cursor
                    .and_then(|id| uuid::Uuid::parse_str(&id).ok());
                self.benchmark_definitions.selected = 0;
                self.benchmark_definitions.scroll.scroll_to_item(0);
                self.benchmark_definitions.delete_confirmation = None;
            }
            Ok(BenchmarkDefinitionReply::Saved(saved)) => {
                self.benchmark_definitions.candidate = None;
                self.benchmark_definitions.edit = None;
                self.benchmark_definitions.detail = Some(*saved);
                self.show_toast(
                    "Benchmark definition saved privately; bind values omitted".into(),
                    cx,
                );
                self.send_benchmark_definition(
                    BenchmarkDefinitionAction::List { cursor: None },
                    cx,
                );
            }
            Ok(BenchmarkDefinitionReply::Loaded(saved)) => {
                self.benchmark_definitions.detail = Some(*saved)
            }
            Ok(BenchmarkDefinitionReply::Deleted(id)) => {
                self.benchmark_definitions.delete_confirmation = None;
                if self
                    .benchmark_definitions
                    .detail
                    .as_ref()
                    .is_some_and(|detail| detail.id == id)
                {
                    self.benchmark_definitions.detail = None;
                }
                self.send_benchmark_definition(
                    BenchmarkDefinitionAction::List { cursor: None },
                    cx,
                );
            }
            Ok(BenchmarkDefinitionReply::Ran { item_id, report }) => {
                let running = self.benchmark_definitions.running.take();
                if let Some(running) =
                    running.filter(|run| run.item_id == item_id && run.run_id == report.run_id)
                {
                    if !self.definition_run_context_current(&running, cx) {
                        self.benchmark_definitions.error = Some("Query or connection changed; definition result was not applied to the current tab".into());
                    } else if let Some(results) = self
                        .panes
                        .iter()
                        .find_map(|pane| pane.read(cx).results.get(&item_id).cloned())
                    {
                        results.update(cx, |view, cx| {
                            view.show_definition_benchmark(*report, running.source_context, cx)
                        });
                        self.modal = None;
                        self.show_toast("Definition report opened in Performance".into(), cx);
                    }
                }
            }
        }
        cx.notify();
    }

    pub(super) fn receive_definition_progress(
        &mut self,
        item_id: u64,
        profile_id: i64,
        session: sift_protocol::SessionId,
        connection: sift_protocol::ConnectionId,
        progress: &sift_protocol::PerformanceProgress,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(running) = self.benchmark_definitions.running.as_ref() else {
            return false;
        };
        if progress.kind != sift_protocol::PerformanceRunKind::Benchmark
            || running.run_id != progress.run_id
            || running.item_id != item_id
            || running.profile_id != profile_id
            || running.session_id != session
            || running.connection_id != connection
            || !self.definition_run_context_current(running, cx)
        {
            return false;
        }
        if self
            .benchmark_definitions
            .progress
            .as_ref()
            .is_some_and(|previous| previous.elapsed_ms > progress.elapsed_ms)
        {
            return true;
        }
        self.benchmark_definitions.progress = Some(progress.clone());
        cx.notify();
        true
    }

    fn save_definition_candidate(&mut self, cx: &mut Context<Self>) {
        let Some(mut candidate) = self.benchmark_definitions.candidate.clone() else {
            return;
        };
        candidate.name = self
            .benchmark_definitions
            .name
            .read(cx)
            .text()
            .trim()
            .to_owned();
        if candidate.name.is_empty() || candidate.name.len() > 200 {
            self.benchmark_definitions.error = Some("Name must be 1–200 bytes".into());
            cx.notify();
            return;
        }
        self.send_benchmark_definition(BenchmarkDefinitionAction::Save(Box::new(candidate)), cx);
    }

    fn edit_opened_definition(&mut self, cx: &mut Context<Self>) {
        if self.benchmark_definitions.pending.is_some()
            || !self.benchmark_definition_scope_current()
        {
            return;
        }
        let Some(detail) = self.benchmark_definitions.detail.as_ref() else {
            return;
        };
        let values = [
            detail.name.clone(),
            detail.sql.clone(),
            detail.parameter_count.to_string(),
            detail.limits.warmups.to_string(),
            detail.limits.iterations.to_string(),
            detail.limits.query_timeout_ms.to_string(),
            detail.limits.total_budget_ms.to_string(),
            detail.limits.delay_ms.to_string(),
        ];
        let edit = DefinitionEdit {
            id: detail.id,
            revision: detail.revision,
            engine: detail.engine,
            fields: values
                .into_iter()
                .enumerate()
                .map(|(index, value)| {
                    cx.new(|cx| {
                        let input = TextInput::new(value, EDIT_LABELS[index], cx)
                            .aria_label(EDIT_LABELS[index]);
                        if index == 1 {
                            input.multiline()
                        } else {
                            input
                        }
                    })
                })
                .collect(),
        };
        self.benchmark_definitions.run_review = None;
        self.benchmark_definitions.delete_confirmation = None;
        self.benchmark_definitions.candidate = None;
        self.benchmark_definitions.edit = Some(edit);
        self.benchmark_definitions.error = None;
        cx.notify();
    }

    fn save_definition_edit(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.benchmark_definitions.edit.as_ref() else {
            return;
        };
        let values: Vec<String> = edit
            .fields
            .iter()
            .map(|field| field.read(cx).text().to_owned())
            .collect();
        let request = definition_edit_request(edit.revision, edit.engine, &values);
        match request {
            Ok(request) => self.send_benchmark_definition(
                BenchmarkDefinitionAction::Update {
                    id: edit.id,
                    request: Box::new(request),
                },
                cx,
            ),
            Err(error) => {
                self.benchmark_definitions.error = Some(error);
                cx.notify();
            }
        }
    }

    fn load_selected_definition(&mut self, cx: &mut Context<Self>) {
        if let Some(item) = self
            .benchmark_definitions
            .items
            .get(self.benchmark_definitions.selected)
        {
            self.send_benchmark_definition(BenchmarkDefinitionAction::Get(item.id), cx);
        }
    }

    fn delete_selected_definition(&mut self, cx: &mut Context<Self>) {
        if self.benchmark_definitions.edit.is_some() {
            return;
        }
        self.benchmark_definitions.delete_confirmation = self
            .benchmark_definitions
            .items
            .get(self.benchmark_definitions.selected)
            .map(|item| item.id);
        cx.notify();
    }

    fn confirm_delete_selected_definition(&mut self, cx: &mut Context<Self>) {
        let selected = self
            .benchmark_definitions
            .items
            .get(self.benchmark_definitions.selected)
            .map(|item| item.id);
        if let Some(id) =
            selected.filter(|id| Some(*id) == self.benchmark_definitions.delete_confirmation)
        {
            self.benchmark_definitions.delete_confirmation = None;
            self.send_benchmark_definition(BenchmarkDefinitionAction::Delete(id), cx);
        }
    }

    fn current_definition_query(
        &self,
        detail: &SavedBenchmarkDefinition,
        cx: &App,
    ) -> Result<
        (
            u64,
            i64,
            sift_protocol::SessionId,
            sift_protocol::ConnectionId,
            SemanticConnectionTarget,
        ),
        String,
    > {
        if !self.benchmark_definition_scope_current() {
            return Err("Server or tenant changed; reopen definitions".into());
        }
        let item_id = self
            .panes
            .get(self.active_pane)
            .and_then(|pane| pane.read(cx).active_item().map(|item| item.id))
            .ok_or("Open the matching query tab before running this definition")?;
        let target = self
            .query_semantic_targets
            .get(&item_id)
            .cloned()
            .or_else(|| self.sourced_semantic_target(item_id, cx))
            .ok_or("Current query has no bound database connection")?;
        if Some(&target) != self.active_semantic_target().as_ref()
            || self.selected_instance_id.as_deref() != Some(target.instance_id.as_str())
            || self.selected_tenant_id() != Some(target.tenant_id)
        {
            return Err("Select this query's original server, tenant and connection".into());
        }
        let profile_id = self
            .query_profile_id(item_id, cx)
            .filter(|id| self.profile_is_connected(*id))
            .ok_or("Connect this query's database before rerunning")?;
        let (identity_profile, session_id, connection_id) = self
            .active_query_connection
            .ok_or("Wait for the current database connection to be ready")?;
        if identity_profile != profile_id {
            return Err("Current database connection differs from this query".into());
        }
        if profile_id != target.profile_id || self.targeted_query_sql(item_id, cx) != detail.sql {
            return Err("Current query text differs from the saved definition".into());
        }
        let engine = match target.provider_id.as_str() {
            "sift/postgres" => sift_protocol::Engine::Postgres,
            "sift/sql-server" => sift_protocol::Engine::SqlServer,
            "sift/sqlite" => sift_protocol::Engine::Sqlite,
            _ => return Err("This provider cannot rerun the definition".into()),
        };
        if engine != detail.engine {
            return Err("Current query engine differs from the definition".into());
        }
        Ok((item_id, profile_id, session_id, connection_id, target))
    }

    fn review_definition_run(&mut self, cx: &mut Context<Self>) {
        if self.benchmark_definitions.edit.is_some() {
            return;
        }
        if self.benchmark_definitions.pending.is_some() {
            return;
        }
        let Some(detail) = self.benchmark_definitions.detail.as_ref() else {
            return;
        };
        let (item_id, profile_id, session_id, connection_id, target) =
            match self.current_definition_query(detail, cx) {
                Ok(context) => context,
                Err(error) => {
                    self.benchmark_definitions.error = Some(error);
                    cx.notify();
                    return;
                }
            };
        let count = detail.parameter_count as usize;
        let labels = if detail.engine == sift_protocol::Engine::Sqlite {
            match sift_core::sqlite_parameters::layout(&detail.sql, 256) {
                Ok(layout) if layout.slots.len() == count => layout
                    .slots
                    .iter()
                    .enumerate()
                    .map(|(index, slot)| slot.label(index))
                    .collect::<Vec<_>>(),
                _ => {
                    self.benchmark_definitions.error=Some("Definition parameter count differs from native slots; edit the definition before rerunning".into());
                    cx.notify();
                    return;
                }
            }
        } else {
            (1..=count)
                .map(|index| format!("parameter {index}"))
                .collect()
        };
        let params = (0..count)
            .map(|index| {
                cx.new(|cx| {
                    TextInput::new("", format!("Value for {}", labels[index]), cx)
                        .aria_label(format!("Fresh value for {}", labels[index]))
                })
            })
            .collect::<Vec<_>>();
        let focuses = params
            .iter()
            .map(|input| input.focus_handle(cx))
            .collect::<Vec<_>>();
        for (index, input) in params.iter().enumerate() {
            let previous = index
                .checked_sub(1)
                .and_then(|index| focuses.get(index))
                .cloned()
                .unwrap_or_else(|| self.focus_handle.clone());
            let next = focuses
                .get(index + 1)
                .cloned()
                .unwrap_or_else(|| self.focus_handle.clone());
            input.update(cx, |input, cx| {
                input.set_tab_targets(Some(previous), Some(next), cx)
            });
        }
        self.benchmark_definitions.run_review = Some(RunReview {
            source_context: self.capture_benchmark_source_context(item_id, cx),
            definition_id: detail.id,
            revision: detail.revision,
            connection_epoch: self.benchmark_definitions.connection_epoch,
            item_id,
            profile_id,
            session_id,
            connection_id,
            target,
            sql: detail.sql.clone(),
            params,
            labels,
        });
        self.benchmark_definitions.error = None;
        cx.notify();
    }

    fn definition_run_context_current(&self, running: &RunningDefinition, cx: &App) -> bool {
        self.benchmark_definition_scope_current()
            && self.benchmark_definitions.connection_epoch == running.connection_epoch
            && self.active_semantic_target().as_ref() == Some(&running.target)
            && self
                .panes
                .get(self.active_pane)
                .and_then(|pane| pane.read(cx).active_item().map(|item| item.id))
                == Some(running.item_id)
            && self.query_profile_id(running.item_id, cx) == Some(running.profile_id)
            && self.active_query_connection
                == Some((
                    running.profile_id,
                    running.session_id,
                    running.connection_id,
                ))
            && self.profile_is_connected(running.profile_id)
            && self.targeted_query_sql(running.item_id, cx) == running.sql
    }

    fn confirm_definition_run(&mut self, cx: &mut Context<Self>) {
        if self.benchmark_definitions.pending.is_some() {
            return;
        }
        let Some(review) = self.benchmark_definitions.run_review.as_ref() else {
            return;
        };
        let context = RunningDefinition {
            source_context: review.source_context.clone(),
            item_id: review.item_id,
            profile_id: review.profile_id,
            session_id: review.session_id,
            connection_id: review.connection_id,
            run_id: uuid::Uuid::new_v4(),
            connection_epoch: review.connection_epoch,
            target: review.target.clone(),
            sql: review.sql.clone(),
        };
        if !self.definition_run_context_current(&context, cx)
            || self
                .benchmark_definitions
                .detail
                .as_ref()
                .is_none_or(|detail| {
                    detail.id != review.definition_id || detail.revision != review.revision
                })
        {
            self.benchmark_definitions.error =
                Some("Query or connection changed; review the definition again".into());
            self.benchmark_definitions.run_review = None;
            cx.notify();
            return;
        }
        let params = match review
            .params
            .iter()
            .enumerate()
            .map(|(index, input)| {
                let text = input.read(cx).text().to_owned();
                if text.is_empty() {
                    return Err(format!(
                        "Enter parameter {} (use JSON \"\" for empty text)",
                        index + 1
                    ));
                }
                parse_parameter_value(&text)
                    .map_err(|error| format!("Parameter {}: {error}", index + 1))
            })
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(params) => params,
            Err(error) => {
                self.benchmark_definitions.error = Some(error);
                cx.notify();
                return;
            }
        };
        let id = review.definition_id;
        let item_id = review.item_id;
        let profile_id = review.profile_id;
        let session_id = review.session_id;
        let connection_id = review.connection_id;
        let tenant_id = self
            .benchmark_definitions
            .tenant_id
            .expect("scoped definition browser");
        let request = RunBenchmarkDefinitionRequest {
            tenant_id,
            run_id: context.run_id,
            expected_revision: review.revision,
            params,
            workload_confirmed: true,
        };
        self.benchmark_definitions.run_review = None;
        self.benchmark_definitions.progress = None;
        self.benchmark_definitions.running = Some(context);
        self.send_benchmark_definition(
            BenchmarkDefinitionAction::Run {
                id,
                item_id,
                profile_id,
                session_id,
                connection_id,
                request,
            },
            cx,
        );
        if self.benchmark_definitions.pending.is_none() {
            self.benchmark_definitions.running = None;
        }
    }

    pub(super) fn handle_benchmark_definition_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal != Some(Modal::BenchmarkDefinitions) {
            return;
        }
        let name_focused = self
            .benchmark_definitions
            .name
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        let focused_edit = self.benchmark_definitions.edit.as_ref().and_then(|edit| {
            edit.fields
                .iter()
                .position(|input| input.focus_handle(cx).is_focused(window))
        });
        let focused_parameter = self
            .benchmark_definitions
            .run_review
            .as_ref()
            .and_then(|review| {
                review
                    .params
                    .iter()
                    .position(|input| input.focus_handle(cx).is_focused(window))
            });
        if let Some(index) = focused_edit {
            match event.keystroke.unparse().as_str() {
                "escape" | "enter" => self.focus_handle.focus(window, cx),
                "tab" | "shift-tab" => {
                    let next = if event.keystroke.unparse() == "tab" {
                        index + 1
                    } else {
                        index.checked_sub(1).unwrap_or(8)
                    };
                    if let Some(input) = self
                        .benchmark_definitions
                        .edit
                        .as_ref()
                        .and_then(|edit| edit.fields.get(next))
                    {
                        input.focus_handle(cx).focus(window, cx);
                    } else {
                        self.focus_handle.focus(window, cx);
                    }
                }
                _ => return,
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if name_focused || focused_parameter.is_some() {
            let key = event.keystroke.unparse();
            match key.as_str() {
                "escape" | "enter" => self.focus_handle.focus(window, cx),
                "tab" | "shift-tab" if focused_parameter.is_some() => {
                    let current = focused_parameter.unwrap();
                    let next = if key == "tab" {
                        current + 1
                    } else {
                        current.saturating_sub(1)
                    };
                    if let Some(input) = self
                        .benchmark_definitions
                        .run_review
                        .as_ref()
                        .and_then(|review| review.params.get(next))
                    {
                        input.focus_handle(cx).focus(window, cx);
                    } else {
                        self.focus_handle.focus(window, cx);
                    }
                }
                _ => return,
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if event.keystroke.modifiers.modified() {
            return;
        }
        if self.benchmark_definitions.pending.is_some() && event.keystroke.key != "escape" {
            return;
        }
        if self.benchmark_definitions.edit.is_some() {
            match event.keystroke.key.as_str() {
                "escape" if self.benchmark_definitions.pending.is_none() => {
                    self.benchmark_definitions.edit = None
                }
                "s" => self.save_definition_edit(cx),
                "i" => {
                    if let Some(input) = self
                        .benchmark_definitions
                        .edit
                        .as_ref()
                        .and_then(|edit| edit.fields.first())
                    {
                        input.focus_handle(cx).focus(window, cx);
                    }
                }
                _ => return,
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        match event.keystroke.key.as_str() {
            "escape"
                if self.benchmark_definitions.run_review.is_some()
                    || self.benchmark_definitions.delete_confirmation.is_some() =>
            {
                self.benchmark_definitions.run_review = None;
                self.benchmark_definitions.delete_confirmation = None;
            }
            "escape" => self.dismiss_modal(&DismissModal, window, cx),
            "j" => {
                self.benchmark_definitions.selected = (self.benchmark_definitions.selected + 1)
                    .min(self.benchmark_definitions.items.len().saturating_sub(1));
                self.benchmark_definitions.delete_confirmation = None;
            }
            "k" => {
                self.benchmark_definitions.selected =
                    self.benchmark_definitions.selected.saturating_sub(1);
                self.benchmark_definitions.delete_confirmation = None;
            }
            "enter" if self.benchmark_definitions.delete_confirmation.is_some() => {
                self.confirm_delete_selected_definition(cx)
            }
            "enter" => self.load_selected_definition(cx),
            "e" => self.edit_opened_definition(cx),
            "d" => self.delete_selected_definition(cx),
            "r" => {
                self.review_definition_run(cx);
                if let Some(input) = self
                    .benchmark_definitions
                    .run_review
                    .as_ref()
                    .and_then(|review| review.params.first())
                {
                    input.focus_handle(cx).focus(window, cx);
                }
            }
            "x" => self.confirm_definition_run(cx),
            "s" => self.save_definition_candidate(cx),
            "i" if self.benchmark_definitions.candidate.is_some() => self
                .benchmark_definitions
                .name
                .read(cx)
                .focus_handle(cx)
                .focus(window, cx),
            "u" => {
                self.send_benchmark_definition(BenchmarkDefinitionAction::List { cursor: None }, cx)
            }
            "n" if self.benchmark_definitions.next_cursor.is_some() => self
                .send_benchmark_definition(
                    BenchmarkDefinitionAction::List {
                        cursor: self.benchmark_definitions.next_cursor,
                    },
                    cx,
                ),
            _ => return,
        }
        cx.stop_propagation();
        self.benchmark_definitions
            .scroll
            .scroll_to_item(self.benchmark_definitions.selected);
        cx.notify();
    }

    pub(super) fn render_benchmark_definitions(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if !self.benchmark_definition_scope_current() {
            return div()
                .child("Server or tenant changed. Reopen benchmark definitions.")
                .into_any_element();
        }
        let state = &self.benchmark_definitions;
        let pending = state.pending.is_some();
        let colors = cx.theme().colors;
        div().flex().flex_col().gap_2().max_h(px(720.))
            .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child("Benchmark definitions · private to you"))
            .child(div().text_xs().text_color(colors.muted_text).child("Stores SQL and limits, never bind values. Reruns require the matching current query, active connection and fresh parameter values. Repeated reads may load the database or invoke side effects."))
            .child(div().flex().flex_wrap().gap_2()
                .child(Button::new("definition-refresh", "[u] Refresh").disabled(pending).on_click(cx.listener(|shell,_,_,cx| shell.send_benchmark_definition(BenchmarkDefinitionAction::List {cursor: None}, cx))))
                .child(Button::new("definition-next", "[n] Next page").disabled(pending || state.next_cursor.is_none()).on_click(cx.listener(|shell,_,_,cx| { if let Some(cursor)=shell.benchmark_definitions.next_cursor {shell.send_benchmark_definition(BenchmarkDefinitionAction::List {cursor: Some(cursor)},cx);} })))
                .child(Button::new("definition-open", "[Enter] Open").disabled(pending || state.items.is_empty()).on_click(cx.listener(|shell,_,_,cx| shell.load_selected_definition(cx))))
                .child(Button::new("definition-delete", if state.delete_confirmation.is_some() { "Confirm delete" } else { "[d] Delete" }).disabled(pending || state.items.is_empty()).on_click(cx.listener(|shell,_,_,cx| { if shell.benchmark_definitions.delete_confirmation.is_some() { shell.confirm_delete_selected_definition(cx) } else { shell.delete_selected_definition(cx) } }))))
            .children(state.edit.as_ref().map(|edit| div().id("definition-edit-fields").max_h(px(350.)).overflow_y_scroll().flex().flex_col().gap_1()
                .child(format!("Edit opened definition · {} · revision {} · {}", edit.id, edit.revision, edit.engine))
                .children(edit.fields.iter().enumerate().map(|(index, input)| div().flex().flex_col().gap_1().child(EDIT_LABELS[index]).child(input.clone())))
                .child(Button::new("definition-update", "[s] Save changes").disabled(pending).on_click(cx.listener(|shell,_,_,cx| shell.save_definition_edit(cx))))
                .child("Vim: i edit · Tab next field · Enter/Escape command focus · Shift-Enter SQL newline · s save · Escape cancel")))
            .children(state.candidate.as_ref().map(|candidate| div().flex().flex_col().gap_1()
                .child(div().text_xs().child(format!("Current query · {:?} · {} parameters · {} warm-ups / {} measured", candidate.engine, candidate.parameter_count, candidate.limits.warmups, candidate.limits.iterations)))
                .child(div().flex().items_center().gap_2().child(state.name.clone())
                    .child(Button::new("definition-save", "[s] Save definition").disabled(pending).on_click(cx.listener(|shell,_,_,cx| shell.save_definition_candidate(cx)))))))
            .children(state.error.as_ref().map(|error| ErrorBanner::new(error.clone())))
            .children(state.delete_confirmation.map(|id| div().text_sm().text_color(colors.danger).child(format!("Permanently delete definition {id}? Enter confirms; Escape cancels."))))
            .children(pending.then(|| div().text_sm().child(state.progress.as_ref()
                .filter(|_| state.running.as_ref().is_some_and(|running| self.definition_run_context_current(running, cx)))
                .map(crate::results::performance_progress_label).unwrap_or_else(|| "Request running…".into()))))
            .child(div().text_xs().child("Vim: j/k select · Enter detail · e edit opened · i edit name · s save · r review rerun · Tab next value · Esc return · x confirm workload · d then Enter delete · u refresh · n next"))
            .child(div().id("benchmark-definition-list").max_h(px(180.)).overflow_y_scroll().track_scroll(&state.scroll).flex().flex_col().children(state.items.iter().enumerate().map(|(index,item)|
                div().id(("benchmark-definition",index)).p_2().cursor(CursorStyle::PointingHand).when(index==state.selected,|row| row.bg(colors.active_surface))
                    .on_click(cx.listener(move |shell,_,_,cx| {if shell.benchmark_definitions.edit.is_some() { return; } shell.benchmark_definitions.selected=index; shell.benchmark_definitions.delete_confirmation=None; shell.load_selected_definition(cx);} ))
                    .child(format!("{} · {} · {} parameters · {}", item.name, item.engine.map_or_else(|| "engine unavailable".into(), |engine| format!("{engine:?}")), item.parameter_count, if item.payload_available {"ready"} else {"payload unavailable"})))))
            .children((state.items.is_empty() && !pending).then(|| div().text_sm().child("No definitions on this page.")))
            .children(state.detail.as_ref().map(|detail| div().flex().flex_col().gap_2()
                .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(format!("Opened: {}",detail.name)))
                .child(div().text_xs().child(format!("{:?} · {} parameters · {} warm-ups / {} measured · {} ms timeout / {} ms budget / {} ms delay", detail.engine, detail.parameter_count, detail.limits.warmups, detail.limits.iterations, detail.limits.query_timeout_ms, detail.limits.total_budget_ms, detail.limits.delay_ms)))
                .child(div().id("benchmark-definition-sql").max_h(px(90.)).overflow_y_scroll().text_sm().child(bounded_sql(&detail.sql)))
                 .child(Button::new("definition-edit", "[e] Edit opened definition").disabled(pending || state.edit.is_some()).on_click(cx.listener(|shell,_,_,cx| shell.edit_opened_definition(cx))))
                .child(Button::new("definition-review", "[r] Review rerun on current query").disabled(pending).on_click(cx.listener(|shell,_,_,cx| shell.review_definition_run(cx))))) )
            .children(state.run_review.as_ref().map(|review| div().flex().flex_col().gap_2()
                .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child("Confirm repeated read workload"))
                .child(div().text_xs().child("Current query, connection and SQL must remain unchanged. Enter fresh values; these inputs are discarded when dispatched. Use JSON \"\" for empty text, null for NULL."))
                .children(review.params.iter().enumerate().map(|(index,input)| div().flex().items_center().gap_2().child(review.labels[index].clone()).child(input.clone())))
                .child(Button::new("definition-run-confirm", "[x] Confirm and run").disabled(pending).tone(ButtonTone::Accent).on_click(cx.listener(|shell,_,_,cx| shell.confirm_definition_run(cx))))) )
            .into_any_element()
    }
}

fn definition_edit_request(
    revision: u64,
    engine: sift_protocol::Engine,
    values: &[String],
) -> Result<UpdateBenchmarkDefinitionRequest, String> {
    if values.len() != 8 {
        return Err("Incomplete definition form".into());
    }
    let number = |index: usize| {
        values[index]
            .trim()
            .parse::<u64>()
            .map_err(|_| format!("{} must be a nonnegative integer", EDIT_LABELS[index]))
    };
    let u32_number = |index| {
        u32::try_from(number(index)?).map_err(|_| format!("{} is too large", EDIT_LABELS[index]))
    };
    let limits = sift_protocol::BenchmarkLimits {
        warmups: u32_number(3)?,
        iterations: u32_number(4)?,
        query_timeout_ms: number(5)?,
        total_budget_ms: number(6)?,
        delay_ms: number(7)?,
    };
    limits.validate().map_err(|error| error.to_string())?;
    let name = values[0].trim().to_owned();
    let parameter_count = u32_number(2)?;
    if name.is_empty()
        || name.len() > 200
        || values[1].trim().is_empty()
        || values[1].len() > 1024 * 1024
        || parameter_count > 256
    {
        return Err(
            "Use a 1–200 byte name, nonblank SQL up to 1 MiB, and at most 256 parameters".into(),
        );
    }
    if engine == sift_protocol::Engine::Sqlite
        && sqlite_parameter_count(&values[1])? != parameter_count
    {
        return Err("SQLite parameter count must match its native bind slots".into());
    }
    Ok(UpdateBenchmarkDefinitionRequest {
        expected_revision: revision,
        definition: SaveBenchmarkDefinitionRequest {
            name,
            engine,
            sql: values[1].clone(),
            parameter_count,
            limits,
        },
    })
}

fn bounded_sql(sql: &str) -> String {
    let mut chars = sql.chars();
    let mut shown: String = chars.by_ref().take(4096).collect();
    if chars.next().is_some() {
        shown.push_str("… [SQL preview truncated]");
    }
    shown
}

/// Native SQLite slot cardinality, including repeated names and numbered aliases.
pub(super) fn sqlite_parameter_count(sql: &str) -> Result<u32, String> {
    sift_core::sqlite_parameters::layout(sql, 256)
        .map(|layout| layout.slots.len() as u32)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};

    #[test]
    fn definition_edit_preserves_revision_and_checks_budgets() {
        let mut values: Vec<String> =
            [" changed ", "SELECT ?", "1", "2", "10", "1000", "5000", "0"]
                .into_iter()
                .map(str::to_owned)
                .collect();
        let request = definition_edit_request(7, sift_protocol::Engine::Sqlite, &values).unwrap();
        assert_eq!(request.expected_revision, 7);
        assert_eq!(request.definition.name, "changed");
        assert_eq!(request.definition.sql, "SELECT ?");
        values[6] = "1".into();
        assert!(definition_edit_request(7, sift_protocol::Engine::Sqlite, &values).is_err());
        values[6] = "5000".into();
        values[2] = "257".into();
        assert!(definition_edit_request(7, sift_protocol::Engine::Sqlite, &values).is_err());
    }

    #[test]
    fn sqlite_bind_count_skips_literals_and_comments() {
        assert_eq!(
            sqlite_parameter_count("select ?, '?', \"?\", /* ? */ ?2, -- ?\n ?").unwrap(),
            3
        );
        assert!(sqlite_parameter_count("select ?0").is_err());
        assert!(sqlite_parameter_count("select ?2").is_err());
        assert!(sqlite_parameter_count("select :name, :name").unwrap() == 1);
        assert!(sqlite_parameter_count("select $1").unwrap() == 1);
    }

    #[gpui::test]
    fn browser_rejects_stale_replies_and_requires_matching_delete_confirmation(
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
            shell.run_command(CommandId::ShowBenchmarkDefinitions, window, cx);
        });
        let request_id = match receiver.try_recv().unwrap() {
            ExecutorCommand::BenchmarkDefinitions {
                request_id,
                tenant_id: 1,
                action: BenchmarkDefinitionAction::List { cursor: None },
                ..
            } => request_id,
            _ => panic!("expected scoped definition browser request"),
        };
        let ids = [uuid::Uuid::new_v4(), uuid::Uuid::new_v4()];
        workspace.update(&mut cx, |shell, cx| {
            shell.receive_benchmark_definition(
                "fixture".into(),
                uuid::Uuid::new_v4(),
                Err("stale".into()),
                cx,
            );
            assert_eq!(shell.benchmark_definitions.pending, Some(request_id));
            shell.receive_benchmark_definition(
                "fixture".into(),
                request_id,
                Ok(BenchmarkDefinitionReply::Page(sift_protocol::CursorPage {
                    items: ids
                        .iter()
                        .map(|id| SavedBenchmarkDefinitionSummary {
                            id: *id,
                            revision: 1,
                            updated_at: chrono::Utc::now(),
                            name: "fixture".into(),
                            engine: Some(sift_protocol::Engine::Postgres),
                            parameter_count: 1,
                            payload_available: true,
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
        assert!(receiver.try_recv().is_err());
        workspace.read_with(&cx, |shell, _| {
            assert_eq!(
                shell.benchmark_definitions.delete_confirmation,
                Some(ids[1])
            );
        });
        cx.simulate_keystrokes("k");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        let load_id = match receiver.try_recv().unwrap() {
            ExecutorCommand::BenchmarkDefinitions {
                request_id,
                action: BenchmarkDefinitionAction::Get(id),
                ..
            } => {
                assert_eq!(id, ids[0]);
                request_id
            }
            _ => panic!("expected definition load"),
        };
        workspace.update(&mut cx, |shell, cx| {
            shell.receive_benchmark_definition(
                "fixture".into(),
                load_id,
                Ok(BenchmarkDefinitionReply::Loaded(Box::new(
                    SavedBenchmarkDefinition {
                        id: ids[0],
                        revision: 9,
                        created_at: chrono::Utc::now(),
                        updated_at: chrono::Utc::now(),
                        name: "original".into(),
                        engine: sift_protocol::Engine::Postgres,
                        sql: "SELECT 1".into(),
                        parameter_count: 0,
                        limits: Default::default(),
                    },
                ))),
                cx,
            );
        });
        cx.simulate_keystrokes("e");
        cx.run_until_parked();
        workspace.update(&mut cx, |shell, cx| {
            let edit = shell.benchmark_definitions.edit.as_ref().unwrap();
            edit.fields[0].update(cx, |input, cx| input.set_text("changed", cx));
        });
        cx.simulate_keystrokes("s");
        cx.run_until_parked();
        let update_id = match receiver.try_recv().unwrap() {
            ExecutorCommand::BenchmarkDefinitions {
                request_id,
                action: BenchmarkDefinitionAction::Update { id, request },
                ..
            } => {
                assert_eq!(id, ids[0]);
                assert_eq!(request.expected_revision, 9);
                assert_eq!(request.definition.name, "changed");
                request_id
            }
            _ => panic!("expected revision-bound edit"),
        };
        workspace.update(&mut cx, |shell, cx| {
            shell.receive_benchmark_definition(
                "fixture".into(),
                update_id,
                Err("stale revision".into()),
                cx,
            );
            assert!(shell.benchmark_definitions.edit.is_some());
            shell.selected_instance_id = Some("changed-server".into());
            shell.save_definition_edit(cx);
            assert!(shell
                .benchmark_definitions
                .error
                .as_ref()
                .unwrap()
                .contains("Server or tenant changed"));
        });
        assert!(receiver.try_recv().is_err());
    }
}

#[cfg(test)]
mod sqlite_named_ui_tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    #[gpui::test]
    fn sqlite_named_slots_use_vim_inputs_and_fresh_definition_review(cx: &mut TestAppContext) {
        let window = super::super::tests::shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        cx.update(|_, cx| {
            cx.bind_keys([
                gpui::KeyBinding::new("tab", sift_ui::Tab, Some("SiftTextInput")),
                gpui::KeyBinding::new("shift-tab", sift_ui::Backtab, Some("SiftTextInput")),
                gpui::KeyBinding::new("enter", sift_ui::Submit, Some("SiftTextInput")),
            ])
        });
        let (sender, mut commands) = ExecutorSender::channel(16);
        let sql = "SELECT :x,:x,?1,$ns::tail(key)";
        workspace.update_in(&mut cx, |shell, window, cx| {
            shell.selected_instance_id = Some("fixture".into());
            shell.lifecycle.tenants = vec![crate::TenantNavEntry {
                id: sift_api_types::TenantId(1),
                name: "Personal".into(),
                rooms: vec![],
                connections: vec![crate::ConnectionNavEntry {
                    id: 7,
                    tenant_id: 1,
                    name: "SQLite".into(),
                    provider_id: sift_protocol::Engine::Sqlite.provider_id(),
                    tags: vec![],
                }],
            }];
            shell.connection_status = ConnectionStatus::Connected {
                profile_id: 7,
                name: "SQLite".into(),
            };
            shell.active_query_connection = Some((
                7,
                sift_protocol::SessionId(1),
                sift_protocol::ConnectionId(2),
            ));
            shell.query_semantic_targets.insert(
                1,
                SemanticConnectionTarget {
                    instance_id: "fixture".into(),
                    tenant_id: 1,
                    profile_id: 7,
                    profile_name: "SQLite".into(),
                    provider_id: sift_protocol::Engine::Sqlite.provider_id(),
                    database: None,
                },
            );
            shell.executor_sender = Some(sender);
            shell.execute_database_item(1, sql.into(), cx);
            assert_eq!(shell.parameter_binding_inputs.len(), 2);
            assert!(shell.parameter_binding_inputs[0].label.contains(":x / ?1"));
            assert!(shell.parameter_binding_inputs[1]
                .label
                .contains("$ns::tail(key)"));
            shell.focus_handle.focus(window, cx);
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("i");
        cx.simulate_input("11");
        cx.simulate_keystrokes("tab");
        cx.simulate_input("22");
        cx.simulate_keystrokes("enter");
        match commands.try_recv().unwrap() {
            ExecutorCommand::Execute {
                sql: executed,
                params,
                ..
            } => {
                assert_eq!(executed, sql);
                assert_eq!(
                    params,
                    vec![
                        sift_protocol::Value::Int64(11),
                        sift_protocol::Value::Int64(22)
                    ]
                );
            }
            _ => panic!("native execution"),
        }
        workspace.update_in(&mut cx,|shell,window,cx|{
            assert!(shell.parameter_binding_subscriptions.is_empty());
            let editor=shell.editor_for_item(1,cx).unwrap();editor.update(cx,|editor,cx|editor.replace_text_from_owner(sql,cx));
            shell.benchmark_definitions.instance_id=Some("fixture".into());shell.benchmark_definitions.tenant_id=Some(1);
            shell.benchmark_definitions.detail=Some(serde_json::from_value(serde_json::json!({"id":uuid::Uuid::new_v4(),"revision":1,"created_at":chrono::Utc::now(),"updated_at":chrono::Utc::now(),"name":"Named fixture","engine":"sqlite","sql":sql,"parameter_count":2,"limits":{"warmups":0,"iterations":1,"query_timeout_ms":1000,"total_budget_ms":5000,"delay_ms":0}})).unwrap());
            shell.modal=Some(Modal::BenchmarkDefinitions);shell.focus_handle.focus(window,cx);
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("r");
        workspace.read_with(&cx, |shell, cx| {
            let review = shell
                .benchmark_definitions
                .run_review
                .as_ref()
                .expect("named review");
            assert_eq!(review.labels, vec!["?1 · :x / ?1", "?2 · $ns::tail(key)"]);
            assert!(
                review
                    .params
                    .iter()
                    .all(|input| input.read(cx).text().is_empty()),
                "definition review never reuses remembered query values"
            );
        });
        workspace.update_in(&mut cx, |shell, window, cx| {
            assert!(shell
                .benchmark_definitions
                .run_review
                .as_ref()
                .unwrap()
                .params[0]
                .focus_handle(cx)
                .is_focused(window))
        });
        cx.simulate_input("31");
        cx.simulate_keystrokes("tab");
        cx.simulate_input("41");
        cx.simulate_keystrokes("tab");
        cx.simulate_keystrokes("x");
        match commands.try_recv().unwrap() {
            ExecutorCommand::BenchmarkDefinitions {
                action: BenchmarkDefinitionAction::Run { request, .. },
                ..
            } => {
                assert_eq!(
                    request.params,
                    vec![
                        sift_protocol::Value::Int64(31),
                        sift_protocol::Value::Int64(41)
                    ]
                );
                assert!(request.workload_confirmed);
            }
            _ => panic!("reviewed named definition execution"),
        }
    }
}
