//! Source-bound mapping drafts. File bytes never enter saved recipes.
use super::*;
use sift_protocol::{
    PreviewTransferSourceRequest, TransferMappedPreview, TransferRecipe, TransferSourcePreview,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferMappingSource {
    pub instance_id: String,
    pub tenant_id: i64,
    pub workspace_id: sift_protocol::WorkspaceId,
    pub recipe_id: sift_protocol::TransferRecipeId,
    pub revision: u64,
    pub request_id: uuid::Uuid,
}
#[derive(Clone)]
pub enum TransferMappingAction {
    Preview(Box<PreviewTransferSourceRequest>),
    Save(Box<sift_api_types::UpdateTransferRecipeRequest>),
}
pub enum TransferMappingReply {
    Preview(Box<TransferSourcePreview>),
    Saved(Box<TransferRecipe>),
}
impl std::fmt::Debug for TransferMappingReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Preview(preview) => f
                .debug_struct("SourcePreview")
                .field("recipe_id", &preview.recipe_id)
                .finish_non_exhaustive(),
            Self::Saved(recipe) => f
                .debug_struct("MappingSaved")
                .field("recipe_id", &recipe.id)
                .finish_non_exhaustive(),
        }
    }
}
struct Output {
    source: Option<String>,
    target: Entity<TextInput>,
    expression: Entity<TextInput>,
}
struct Review {
    version: String,
    options: serde_json::Value,
    data: Arc<[u8]>,
}
pub(super) struct State {
    source: TransferMappingSource,
    recipe: TransferRecipe,
    editor: String,
    sheet: String,
    data: Option<Arc<[u8]>>,
    digest: String,
    outputs: Vec<Output>,
    selected: usize,
    source_selected: usize,
    pending: bool,
    initial: bool,
    saving: bool,
    submitted: Option<Review>,
    review: Option<Review>,
    preview: Option<TransferSourcePreview>,
    error: Option<String>,
    scroll: ScrollHandle,
    output_scroll: ScrollHandle,
}
fn output(
    source: Option<String>,
    target: String,
    expression: String,
    cx: &mut Context<WorkspaceShell>,
) -> Output {
    Output {
        source,
        target: cx.new(|cx| {
            TextInput::new(target, "Target column", cx).aria_label("Mapping target column")
        }),
        expression: cx.new(|cx| {
            TextInput::new(expression, "Optional portable expression JSON", cx)
                .multiline()
                .aria_label("Portable mapping expression")
        }),
    }
}
impl State {
    fn candidate(&self, cx: &App) -> Result<(String, serde_json::Value), String> {
        if self.outputs.is_empty() {
            return Err("Select at least one output column".into());
        }
        let expressions = self
            .outputs
            .iter()
            .any(|row| !row.expression.read(cx).text().trim().is_empty());
        let mut options = self
            .recipe
            .options
            .as_object()
            .cloned()
            .ok_or("Recipe options must be an object")?;
        for key in ["column_projection", "column_mappings", "column_expressions"] {
            options.remove(key);
        }
        let mut columns = Vec::new();
        for row in &self.outputs {
            let target = row.target.read(cx).text();
            let expression = row.expression.read(cx).text().trim();
            if expression.len() > 64 * 1024 {
                return Err("Mapping expression exceeds the 64 KiB editor limit; retain it through recipe JSON".into());
            }
            columns.push(if expressions {
                let expression=if expression.is_empty() {serde_json::json!({"op":"column","source":row.source.as_ref().ok_or("Select a source or enter an expression")?})}else{serde_json::from_str(expression).map_err(|_|"Expression must be valid portable expression JSON")?};
                serde_json::json!({"target":target,"expression":expression})
            }else{serde_json::json!({"target":target,"source":row.source.as_ref().ok_or("Select a source or enter an expression")?})});
        }
        let version = if expressions || self.recipe.format_version == "3" {
            "3"
        } else {
            "2"
        }
        .to_owned();
        options.insert(
            if expressions {
                "column_expressions"
            } else {
                "column_projection"
            }
            .into(),
            serde_json::Value::Array(columns),
        );
        if self.recipe.format_id == "xlsx" {
            options.insert("sheet".into(), self.sheet.clone().into());
        }
        let options = serde_json::Value::Object(options);
        sift_core::transfer_columns::validate_options(
            self.recipe.direction,
            &self.recipe.format_id,
            &version,
            &options,
        )
        .map_err(str::to_owned)?;
        Ok((version, options))
    }
    fn fields(&self) -> Vec<Entity<TextInput>> {
        self.outputs
            .get(self.selected)
            .map(|row| vec![row.target.clone(), row.expression.clone()])
            .unwrap_or_default()
    }
}
impl WorkspaceShell {
    fn mapping_editor_snapshot(&self, cx: &App) -> String {
        serde_json::json!([
            self.transfer.recipe_name_input.read(cx).text(),
            self.transfer.recipe_format_input.read(cx).text(),
            self.transfer.recipe_version_input.read(cx).text(),
            self.transfer.recipe_options_input.read(cx).text(),
            self.transfer.recipe_table_input.read(cx).text(),
            self.transfer.recipe_sheet_input.read(cx).text(),
            self.transfer.recipe_direction,
            self.transfer.import_create_table,
            self.transfer.import_conflict_policy,
        ])
        .to_string()
    }
    fn mapping_scope_current(&self, state: &State) -> bool {
        self.selected_instance_id.as_deref() == Some(state.source.instance_id.as_str())
            && self.selected_tenant_id() == Some(state.source.tenant_id)
            && self.selected_workspace_id == Some(state.source.workspace_id.0)
            && self
                .transfer
                .recipes
                .get(self.transfer.recipe_selected)
                .is_some_and(|recipe| {
                    recipe.id == state.source.recipe_id && recipe.revision == state.source.revision
                })
    }
    pub(super) fn mapping_input_focused(&self, window: &Window, cx: &App) -> bool {
        self.transfer_mapping.as_ref().is_some_and(|state| {
            state
                .fields()
                .iter()
                .any(|input| input.focus_handle(cx).is_focused(window))
        })
    }
    pub(super) fn open_transfer_mapping(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.transfer.execution_pending || self.transfer.recipes_loading {
            return;
        }
        if let Some(state) = &self.transfer_mapping {
            if state.pending {
                if self.mapping_scope_current(state) {
                    self.modal = Some(Modal::TransferMapping);
                    self.focus_handle.focus(window, cx);
                }
                return;
            }
        }
        let Some(recipe) = self
            .transfer
            .recipes
            .get(self.transfer.recipe_selected)
            .cloned()
        else {
            return;
        };
        if recipe.direction != sift_protocol::TransferDirection::Import
            || !matches!(recipe.format_id.as_str(), "xlsx" | "parquet")
        {
            self.show_error_toast("Select a saved XLSX or Parquet import recipe".into(), cx);
            return;
        }
        let original = serde_json::from_str::<serde_json::Value>(
            self.transfer.recipe_options_input.read(cx).text(),
        )
        .ok();
        if self.transfer.recipe_edit != Some(recipe.id)
            || self.transfer.recipe_name_input.read(cx).text() != recipe.name
            || self.transfer.recipe_format_input.read(cx).text() != recipe.format_id
            || self.transfer.recipe_version_input.read(cx).text() != recipe.format_version
            || original.as_ref() != Some(&recipe.options)
            || self.transfer.recipe_direction != recipe.direction
        {
            self.show_error_toast(
                "Save the recipe draft before opening its mapping grid".into(),
                cx,
            );
            return;
        }
        let (Some(instance), Some(tenant)) =
            (self.selected_instance_id.clone(), self.selected_tenant_id())
        else {
            return;
        };
        if self.selected_workspace_id != Some(recipe.workspace_id.0) {
            return;
        }
        if !self.require_operation(
            sift_protocol::OperationKind::ReadTransferRecipe,
            "Preview transfer source",
            cx,
        ) {
            return;
        }
        let sheet = self
            .transfer
            .recipe_sheet_input
            .read(cx)
            .text()
            .trim()
            .to_owned();
        if recipe.format_id == "xlsx" && sheet.is_empty() {
            self.show_error_toast(
                "Select an XLSX sheet before opening its mapping grid".into(),
                cx,
            );
            return;
        }
        let source = TransferMappingSource {
            instance_id: instance,
            tenant_id: tenant,
            workspace_id: recipe.workspace_id,
            recipe_id: recipe.id,
            revision: recipe.revision,
            request_id: uuid::Uuid::new_v4(),
        };
        let token = source.request_id;
        self.transfer_mapping = Some(State {
            source,
            recipe,
            editor: self.mapping_editor_snapshot(cx),
            sheet,
            data: None,
            digest: String::new(),
            outputs: Vec::new(),
            selected: 0,
            source_selected: 0,
            pending: true,
            initial: true,
            saving: false,
            submitted: None,
            review: None,
            preview: None,
            error: None,
            scroll: ScrollHandle::new(),
            output_scroll: ScrollHandle::new(),
        });
        self.modal = Some(Modal::TransferMapping);
        self.focus_handle.focus(window, cx);
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose the source file to map".into()),
        });
        let background = cx.background_executor().clone();
        cx.spawn(async move |shell, cx| {
            let choice = prompt.await;
            let data = if let Ok(Ok(Some(paths))) = choice {
                if let Some(path) = paths.first().cloned() {
                    Some(
                        background
                            .spawn(async move {
                                use sha2::Digest;
                                let data = transfer_input::read(&path).map_err(|_| {
                                    "Could not read a bounded transfer source".to_owned()
                                })?;
                                let digest = format!("{:x}", sha2::Sha256::digest(&data));
                                Ok::<_, String>((Arc::<[u8]>::from(data), digest))
                            })
                            .await,
                    )
                } else {
                    None
                }
            } else {
                None
            };
            let _ = shell.update(cx, |shell, cx| {
                let Some(state) = &shell.transfer_mapping else {
                    return;
                };
                if state.source.request_id != token {
                    return;
                }
                let current = shell.mapping_scope_current(state)
                    && state.editor == shell.mapping_editor_snapshot(cx);
                let state = shell.transfer_mapping.as_mut().unwrap();
                state.pending = false;
                if !current {
                    state.error = Some(
                        "Transfer scope or recipe draft changed while choosing the file".into(),
                    );
                    cx.notify();
                    return;
                }
                match data {
                    Some(Ok((data, digest))) => {
                        state.data = Some(data);
                        state.digest = digest;
                        shell.send_mapping_preview(true, cx);
                    }
                    Some(Err(error)) => state.error = Some(error),
                    None => {
                        shell.transfer_mapping = None;
                        if shell.modal == Some(Modal::TransferMapping) {
                            shell.modal = Some(Modal::TransferRecipes);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn mapping_ready(&self, state: &State, cx: &App) -> bool {
        self.mapping_scope_current(state)
            && state.editor == self.mapping_editor_snapshot(cx)
            && !state.pending
            && state.review.as_ref().is_some_and(|review| {
                state
                    .data
                    .as_ref()
                    .is_some_and(|data| Arc::ptr_eq(data, &review.data))
                    && state.candidate(cx).ok()
                        == Some((review.version.clone(), review.options.clone()))
            })
            && state.preview.as_ref().is_some_and(|preview| {
                matches!(preview.mapping, TransferMappedPreview::Ready { .. })
            })
    }
    fn send_mapping_preview(&mut self, initial: bool, cx: &mut Context<Self>) {
        let Some(state) = &self.transfer_mapping else {
            return;
        };
        if state.pending
            || !self.mapping_scope_current(state)
            || state.editor != self.mapping_editor_snapshot(cx)
        {
            return;
        }
        let Some(data) = state.data.clone() else {
            return;
        };
        let candidate = if initial {
            Ok((
                state.recipe.format_version.clone(),
                state.recipe.options.clone(),
            ))
        } else {
            state.candidate(cx)
        };
        let (version, options) = match candidate {
            Ok(candidate) => candidate,
            Err(error) => {
                self.transfer_mapping.as_mut().unwrap().error = Some(error);
                cx.notify();
                return;
            }
        };
        let state = self.transfer_mapping.as_mut().unwrap();
        state.source.request_id = uuid::Uuid::new_v4();
        let source = state.source.clone();
        let request = PreviewTransferSourceRequest {
            expected_revision: source.revision,
            format_version: version.clone(),
            options: options.clone(),
            data: data.to_vec(),
            sheet: (!state.sheet.is_empty()).then(|| state.sheet.clone()),
        };
        let sent = self.executor_sender.as_ref().is_some_and(|sender| {
            sender
                .send(ExecutorCommand::TransferMapping {
                    source,
                    action: TransferMappingAction::Preview(Box::new(request)),
                })
                .is_ok()
        });
        state.pending = sent;
        state.saving = false;
        state.initial = initial;
        state.review = None;
        state.submitted = Some(Review {
            version,
            options,
            data,
        });
        state.error = (!sent).then(|| "Transfer manager unavailable".into());
        cx.notify();
    }
    fn save_transfer_mapping(&mut self, cx: &mut Context<Self>) {
        let Some(state) = &self.transfer_mapping else {
            return;
        };
        if !self.mapping_ready(state, cx) {
            self.transfer_mapping.as_mut().unwrap().error =
                Some("Preview these exact columns, source and recipe draft before saving".into());
            cx.notify();
            return;
        }
        let review = state.review.as_ref().unwrap();
        let mut source = state.source.clone();
        source.request_id = uuid::Uuid::new_v4();
        let recipe = &state.recipe;
        let request = sift_api_types::UpdateTransferRecipeRequest {
            expected_revision: recipe.revision,
            recipe: sift_api_types::CreateTransferRecipeRequest {
                name: recipe.name.clone(),
                direction: recipe.direction,
                source: recipe.source.clone(),
                sink: recipe.sink.clone(),
                format_id: recipe.format_id.clone(),
                format_version: review.version.clone(),
                options: review.options.clone(),
            },
        };
        if !self.require_operation(
            sift_protocol::OperationKind::ManageTransferRecipe,
            "Save transfer mapping",
            cx,
        ) {
            return;
        }
        let sent = self.executor_sender.as_ref().is_some_and(|sender| {
            sender
                .send(ExecutorCommand::TransferMapping {
                    source: source.clone(),
                    action: TransferMappingAction::Save(Box::new(request)),
                })
                .is_ok()
        });
        let state = self.transfer_mapping.as_mut().unwrap();
        state.source = source;
        state.pending = sent;
        state.saving = sent;
        state.error = (!sent).then(|| "Transfer manager unavailable".into());
        cx.notify();
    }
    pub(super) fn on_transfer_mapping_reply(
        &mut self,
        source: TransferMappingSource,
        result: Result<TransferMappingReply, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = &self.transfer_mapping else {
            return;
        };
        if !state.pending || state.source != source {
            return;
        }
        let scope = self.mapping_scope_current(state);
        let draft = state.editor == self.mapping_editor_snapshot(cx);
        let state = self.transfer_mapping.as_mut().unwrap();
        state.pending = false;
        if !scope {
            state.submitted = None;
            state.review = None;
            state.error =
                Some("Transfer source or recipe revision changed; reopen mapping review".into());
            cx.notify();
            return;
        }
        match result {
            Err(error) => {
                state.submitted = None;
                state.review = None;
                state.error = Some(error);
            }
            Ok(TransferMappingReply::Saved(recipe)) => {
                if !state.saving
                    || recipe.id != source.recipe_id
                    || recipe.workspace_id != source.workspace_id
                    || recipe.revision != source.revision.saturating_add(1)
                    || recipe.name != state.recipe.name
                    || recipe.direction != state.recipe.direction
                    || recipe.source != state.recipe.source
                    || recipe.sink != state.recipe.sink
                    || recipe.format_id != state.recipe.format_id
                    || !state.review.as_ref().is_some_and(|review| {
                        recipe.format_version == review.version && recipe.options == review.options
                    })
                {
                    state.error = Some("Mapping save reply differs from its source".into());
                    cx.notify();
                    return;
                }
                if let Some(index) = self.transfer.recipes.iter().position(|r| r.id == recipe.id) {
                    let version = recipe.format_version.clone();
                    let options = recipe.options.to_string();
                    self.transfer.recipes[index] = *recipe;
                    if draft {
                        self.transfer
                            .recipe_version_input
                            .update(cx, |input, cx| input.set_text(version, cx));
                        self.transfer
                            .recipe_options_input
                            .update(cx, |input, cx| input.set_text(options, cx));
                    }
                    self.transfer.execution_result = None;
                    self.transfer.import_progress = None;
                }
                self.transfer_mapping = None;
                if self.modal == Some(Modal::TransferMapping) {
                    self.modal = Some(Modal::TransferRecipes);
                }
                self.show_success_toast(
                    if draft {
                        "Mapping saved; dry-run the recipe before importing"
                    } else {
                        "Mapping saved; current recipe draft was preserved"
                    }
                    .into(),
                    cx,
                );
            }
            Ok(TransferMappingReply::Preview(preview)) => {
                let submitted = state.submitted.take();
                let Some(submitted) = submitted else {
                    return;
                };
                let valid = preview.recipe_id == source.recipe_id
                    && preview.recipe_revision == source.revision
                    && preview.format_id == state.recipe.format_id
                    && preview.format_version == submitted.version
                    && preview.source_digest == state.digest
                    && state
                        .data
                        .as_ref()
                        .is_some_and(|data| Arc::ptr_eq(data, &submitted.data))
                    && !preview.source_columns.is_empty()
                    && preview.source_columns.len() <= 2000
                    && match &preview.mapping {
                        TransferMappedPreview::Ready {
                            columns,
                            sample_rows,
                            ..
                        } => {
                            !columns.is_empty()
                                && columns.len() <= 256
                                && sample_rows.len() <= 20
                                && sample_rows
                                    .iter()
                                    .all(|row| row.values.len() == columns.len())
                        }
                        TransferMappedPreview::Unavailable { .. } => true,
                    };
                if !valid || !draft {
                    state.review = None;
                    state.error =
                        Some("Source preview reply or recipe draft changed; preview again".into());
                    cx.notify();
                    return;
                }
                if state.initial {
                    match initial_outputs(&state.recipe, &preview, cx) {
                        Ok(outputs) => state.outputs = outputs,
                        Err(error) => state.error = Some(error),
                    }
                } else if state.candidate(cx).ok()
                    == Some((submitted.version.clone(), submitted.options.clone()))
                {
                    let targets = state
                        .outputs
                        .iter()
                        .map(|row| row.target.read(cx).text())
                        .collect::<Vec<_>>();
                    let aligned = match &preview.mapping {
                        TransferMappedPreview::Ready { columns, .. } => {
                            columns.iter().map(|c| c.name.as_str()).eq(targets)
                        }
                        TransferMappedPreview::Unavailable { .. } => true,
                    };
                    if aligned {
                        state.review = Some(submitted);
                    } else {
                        state.error = Some("Mapped output schema differs from this draft".into());
                    }
                }
                state.preview = Some(*preview);
            }
        }
        cx.notify();
    }
    pub(super) fn transfer_mapping_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal != Some(Modal::TransferMapping) {
            return;
        }
        let Some(state) = &self.transfer_mapping else {
            return;
        };
        let fields = state.fields();
        if let Some(i) = fields
            .iter()
            .position(|field| field.focus_handle(cx).is_focused(window))
        {
            match event.keystroke.key.as_str() {
                "escape" => self.focus_handle.focus(window, cx),
                "tab" => fields[(i + 1) % fields.len()]
                    .focus_handle(cx)
                    .focus(window, cx),
                _ => return,
            }
            cx.stop_propagation();
            return;
        }
        let m = event.keystroke.modifiers;
        if m.control || m.alt || m.platform || m.function {
            return;
        }
        let key = event.keystroke.key.as_str();
        if key == "escape" {
            self.modal = Some(Modal::TransferRecipes);
            self.transfer.recipe_focus_handle.focus(window, cx);
            cx.stop_propagation();
            return;
        }
        if state.pending || !self.mapping_scope_current(state) {
            return;
        }
        match key {
            "p" => self.send_mapping_preview(state.preview.is_none(), cx),
            "x" => self.save_transfer_mapping(cx),
            "i" | "tab" if !fields.is_empty() => fields[0].focus_handle(cx).focus(window, cx),
            "e" if fields.len() > 1 => fields[1].focus_handle(cx).focus(window, cx),
            "j" | "k" => {
                let state = self.transfer_mapping.as_mut().unwrap();
                let n = if m.shift {
                    state.preview.as_ref().map_or(0, |p| p.source_columns.len())
                } else {
                    state.outputs.len()
                };
                if n == 0 {
                    return;
                }
                let selected = if m.shift {
                    &mut state.source_selected
                } else {
                    &mut state.selected
                };
                *selected = if key == "j" {
                    (*selected + 1) % n
                } else {
                    (*selected + n - 1) % n
                };
            }
            "h" | "l" => {
                let state = self.transfer_mapping.as_mut().unwrap();
                let n = state.outputs.len();
                if n > 1 {
                    let next = if key == "h" {
                        state.selected.saturating_sub(1)
                    } else {
                        (state.selected + 1).min(n - 1)
                    };
                    state.outputs.swap(state.selected, next);
                    state.selected = next;
                    state.review = None;
                }
            }
            "d" => {
                let state = self.transfer_mapping.as_mut().unwrap();
                if !state.outputs.is_empty() {
                    state.outputs.remove(state.selected);
                    state.selected = state.selected.min(state.outputs.len().saturating_sub(1));
                    state.review = None;
                }
            }
            "a" | "c" => {
                let state = self.transfer_mapping.as_mut().unwrap();
                if state.outputs.len() >= 256 {
                    return;
                }
                let existing = state
                    .outputs
                    .iter()
                    .map(|row| row.target.read(cx).text().to_owned())
                    .collect::<Vec<_>>();
                let (source, target, expression) = if key == "c" {
                    let Some(row) = state.outputs.get(state.selected) else {
                        return;
                    };
                    (
                        row.source.clone(),
                        csv_projection::copy_name(row.target.read(cx).text(), &existing),
                        row.expression.read(cx).text().to_owned(),
                    )
                } else {
                    let Some(column) = state
                        .preview
                        .as_ref()
                        .and_then(|p| p.source_columns.get(state.source_selected))
                    else {
                        return;
                    };
                    if !column.selectable {
                        state.error = Some(
                            "This source root is unsupported or ambiguous; drop it from outputs"
                                .into(),
                        );
                        cx.notify();
                        return;
                    }
                    (
                        Some(column.name.clone()),
                        csv_projection::copy_name(&column.name, &existing),
                        String::new(),
                    )
                };
                state.outputs.push(output(source, target, expression, cx));
                state.selected = state.outputs.len() - 1;
                state.review = None;
            }
            _ => return,
        }
        if let Some(state) = &self.transfer_mapping {
            state.scroll.scroll_to_item(state.source_selected);
            state.output_scroll.scroll_to_item(state.selected);
        }
        cx.stop_propagation();
        cx.notify();
    }
    pub(super) fn render_transfer_mapping(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(state) = &self.transfer_mapping else {
            return div().child("Mapping unavailable").into_any_element();
        };
        let base = div()
            .id("transfer-mapping-grid")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .gap_2()
            .max_h(px(720.))
            .overflow_y_scroll();
        if !self.mapping_scope_current(state) {
            return base
                .child("Original server/workspace/recipe changed. Esc closes this review.")
                .into_any_element();
        }
        let ready = self.mapping_ready(state, cx);
        let base=base.child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(format!("{} mapping · {} · revision {}",state.recipe.format_id,state.recipe.name,state.recipe.revision)))
            .child("j/k outputs · Shift-J/K source columns · a Add source · c Copy · d Drop · h/l Reorder · i Rename · e Expression · p Preview · x Save reviewed mapping · Esc close")
            .child("Saving changes recipe settings only. Dry-run and native write confirmation remain separate.")
            .children(state.error.clone().map(ErrorBanner::new))
            .children(state.pending.then(||div().child(if state.saving {"Saving reviewed mapping…"}else{"Loading bounded source preview…"})))
            .child(if ready {"Preview matches this exact draft and upload; x saves it"}else{"Preview is missing or outdated; p reviews the current mapping"});
        if state.pending && state.saving {
            return base.child("The reviewed mapping is being saved. Fields remain frozen until the receipt arrives; Esc returns to recipes without cancelling the save.").into_any_element();
        }
        let base = if let Some(preview) = &state.preview {
            base.child(format!(
                "{} source rows · upload {} · sheet {}",
                preview.source_rows,
                &state.digest[..state.digest.len().min(12)],
                state.sheet
            ))
            .child(
                div()
                    .id("mapping-source-columns")
                    .max_h(px(140.))
                    .overflow_y_scroll()
                    .track_scroll(&state.scroll)
                    .children(
                        preview
                            .source_columns
                            .iter()
                            .enumerate()
                            .map(|(i, column)| {
                                div().child(format!(
                                    "{} [{}] {} · {}{}",
                                    if i == state.source_selected { ">" } else { " " },
                                    i + 1,
                                    if column.name.is_empty() {
                                        "(unnamed)"
                                    } else {
                                        &column.name
                                    },
                                    column.source_type,
                                    if column.selectable {
                                        ""
                                    } else {
                                        " · cannot select"
                                    }
                                ))
                            }),
                    ),
            )
        } else {
            base
        };
        let base = base.child(
            div()
                .id("mapping-output-columns")
                .max_h(px(160.))
                .overflow_y_scroll()
                .track_scroll(&state.output_scroll)
                .children(state.outputs.iter().enumerate().map(|(i, row)| {
                    div().child(format!(
                        "{} {} → {}{}",
                        if i == state.selected { ">" } else { " " },
                        row.source.as_deref().unwrap_or("expression"),
                        row.target.read(cx).text(),
                        if row.expression.read(cx).text().trim().is_empty() {
                            ""
                        } else {
                            " (computed)"
                        }
                    ))
                })),
        );
        let base = if let Some(row) = state.outputs.get(state.selected) {
            base.child("Destination column")
                .child(row.target.clone())
                .child("Optional portable expression JSON; blank keeps the direct source column")
                .child(row.expression.clone())
        } else {
            base.child("No outputs selected; choose a source and press a")
        };
        base.children(state.preview.as_ref().map(|preview|match &preview.mapping {
            TransferMappedPreview::Unavailable {reason}=>div().child(reason.clone()).into_any_element(),
            TransferMappedPreview::Ready {columns,sample_rows,representation,sample_rows_limited}=> {
                let representation=match representation {
                    sift_protocol::TransferSampleRepresentation::CsvTextBeforeDestinationInference=>"Text before destination inference",
                    sift_protocol::TransferSampleRepresentation::TypedParquet=>"Typed Parquet values",
                };
                div().flex().flex_col().gap_1()
                    .child(format!("Projected samples · {representation}{} · cell display limited to 512 characters · destination constraints are reviewed separately",if *sample_rows_limited {" · limited"}else{""}))
                    .child(div().id("mapping-projected-samples").max_h(px(180.)).overflow_y_scroll().overflow_x_scroll().flex().flex_col().gap_1()
                        .child(div().flex().gap_1().children(columns.iter().map(|column|div().w(px(160.)).min_w(px(160.)).flex_none().flex().flex_col().child(column.name.clone()).child(div().text_xs().child(column.source_type.clone())))))
                        .children(sample_rows.iter().map(|row|div().flex().gap_1().children(row.values.iter().map(|value|div().w(px(160.)).min_w(px(160.)).flex_none().text_xs().font_family("monospace").whitespace_normal().child(sample_value(value)))))))
                    .into_any_element()
            },
        })).into_any_element()
    }
}
fn initial_outputs(
    recipe: &TransferRecipe,
    preview: &TransferSourcePreview,
    cx: &mut Context<WorkspaceShell>,
) -> Result<Vec<Output>, String> {
    if let Some(expressions) = recipe
        .options
        .get("column_expressions")
        .and_then(serde_json::Value::as_array)
    {
        return expressions.iter().map(|item|{let expr=serde_json::to_string(item.get("expression").ok_or("Expression is missing")?).map_err(|_|"Cannot encode expression")?;if expr.len()>64*1024{return Err("Existing expression exceeds the grid editor bound; retain it through recipe JSON".into());}Ok(output(None,item.get("target").and_then(serde_json::Value::as_str).ok_or("Target is missing")?.into(),expr,cx))}).collect();
    }
    if let Some(columns) = recipe
        .options
        .get("column_projection")
        .and_then(serde_json::Value::as_array)
    {
        return columns
            .iter()
            .map(|item| {
                Ok(output(
                    Some(
                        item.get("source")
                            .and_then(serde_json::Value::as_str)
                            .ok_or("Source is missing")?
                            .into(),
                    ),
                    item.get("target")
                        .and_then(serde_json::Value::as_str)
                        .ok_or("Target is missing")?
                        .into(),
                    String::new(),
                    cx,
                ))
            })
            .collect();
    }
    if preview.source_columns.len() > 256 {
        return Err(
            "Select outputs from the source list; the mapping grid allows at most 256 outputs"
                .into(),
        );
    }
    Ok(preview
        .source_columns
        .iter()
        .enumerate()
        .map(|(i, column)| {
            let target = recipe
                .options
                .get("column_mappings")
                .and_then(|m| m.get(&column.name))
                .and_then(serde_json::Value::as_str)
                .unwrap_or(&column.name);
            let target = if target.is_empty() || target.trim() != target || target.len() > 128 {
                format!("column_{}", i + 1)
            } else {
                target.to_owned()
            };
            output(Some(column.name.clone()), target, String::new(), cx)
        })
        .collect())
}

fn sample_value(value: &sift_protocol::Value) -> String {
    let text = match value {
        sift_protocol::Value::Text(text) => format!("{text:?}"),
        other => render_value(other).text,
    };
    let mut chars = text.chars();
    let display = chars.by_ref().take(512).collect::<String>();
    if chars.next().is_some() {
        format!("{display}…")
    } else {
        display
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    fn preview(
        source: &TransferMappingSource,
        version: String,
        digest: String,
        ready: bool,
    ) -> TransferSourcePreview {
        use sift_protocol::{Row, TransferSampleRepresentation, TransferSourceColumn, Value};
        TransferSourcePreview {
            recipe_id: source.recipe_id,
            recipe_revision: source.revision,
            format_id: "parquet".into(),
            format_version: version,
            source_digest: digest,
            source_rows: 1,
            source_columns: vec![
                TransferSourceColumn {
                    name: "keep".into(),
                    source_type: "utf8".into(),
                    nullable: false,
                    selectable: true,
                },
                TransferSourceColumn {
                    name: "drop".into(),
                    source_type: "unsupported root".into(),
                    nullable: false,
                    selectable: false,
                },
            ],
            mapping: if ready {
                TransferMappedPreview::Ready {
                    columns: ["renamed_copy", "kept"]
                        .into_iter()
                        .map(|name| TransferSourceColumn {
                            name: name.into(),
                            source_type: "utf8".into(),
                            nullable: true,
                            selectable: true,
                        })
                        .collect(),
                    sample_rows: vec![Row {
                        values: vec![Value::Text("KEEP".into()), Value::Text("keep".into())],
                    }],
                    representation: TransferSampleRepresentation::TypedParquet,
                    sample_rows_limited: false,
                }
            } else {
                TransferMappedPreview::Unavailable {
                    reason: "Drop unsupported root".into(),
                }
            },
        }
    }
    #[gpui::test]
    fn vim_mapping_reviews_exact_upload_and_preserves_scope_recipe_and_other_drafts(
        cx: &mut TestAppContext,
    ) {
        use sha2::Digest;
        let window = super::super::tests::shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let (sender, mut commands) = ExecutorSender::channel(32);
        let data: Arc<[u8]> = Arc::from(&b"PRIVATE-UPLOAD"[..]);
        let digest = format!("{:x}", sha2::Sha256::digest(&data));
        let recipe:TransferRecipe=serde_json::from_value(serde_json::json!({"id":4,"workspace_id":9,"name":"Mapping","direction":"import","source":{"kind":"upload"},"sink":{"kind":"table"},"format_id":"parquet","format_version":"2","options":{"column_projection":[{"source":"keep","target":"kept"},{"source":"drop","target":"extra"}],"unrelated":"retain"},"revision":5,"created_at":"2026-11-06T00:00:00Z","updated_at":"2026-11-06T00:00:00Z"})).unwrap();
        workspace.update_in(&mut cx, |shell, window, cx| {
            shell.executor_sender = Some(sender);
            shell.selected_instance_id = Some("local".into());
            shell.selected_workspace_id = Some(9);
            shell.lifecycle.tenants = vec![crate::TenantNavEntry {
                id: sift_api_types::TenantId(1),
                name: "Local".into(),
                rooms: vec![],
                connections: vec![],
            }];
            shell.transfer.recipes = vec![recipe.clone()];
            shell.edit_transfer_recipe(0, cx);
            shell.transfer.import_create_table = true;
            let source = TransferMappingSource {
                instance_id: "local".into(),
                tenant_id: 1,
                workspace_id: recipe.workspace_id,
                recipe_id: recipe.id,
                revision: recipe.revision,
                request_id: uuid::Uuid::new_v4(),
            };
            shell.transfer_mapping = Some(State {
                source,
                recipe: recipe.clone(),
                editor: shell.mapping_editor_snapshot(cx),
                sheet: "Sheet1".into(),
                data: Some(data.clone()),
                digest: digest.clone(),
                outputs: vec![],
                selected: 0,
                source_selected: 0,
                pending: false,
                initial: true,
                saving: false,
                submitted: None,
                review: None,
                preview: None,
                error: None,
                scroll: ScrollHandle::new(),
                output_scroll: ScrollHandle::new(),
            });
            shell.modal = Some(Modal::TransferMapping);
            shell.focus_handle.focus(window, cx);
            shell.send_mapping_preview(true, cx);
        });
        let initial = match commands.try_recv().unwrap() {
            ExecutorCommand::TransferMapping {
                source,
                action: TransferMappingAction::Preview(request),
            } => {
                assert_eq!(source.instance_id, "local");
                assert_eq!(source.tenant_id, 1);
                assert_eq!(source.workspace_id.0, 9);
                assert_eq!(request.data, &*data);
                source
            }
            _ => panic!("expected source preview"),
        };
        workspace.update(&mut cx, |shell, cx| {
            let mut late = initial.clone();
            late.request_id = uuid::Uuid::new_v4();
            shell.on_executor_event(
                ExecutorEvent::TransferMappingFinished {
                    source: late,
                    result: Ok(TransferMappingReply::Preview(Box::new(preview(
                        &initial,
                        "2".into(),
                        digest.clone(),
                        false,
                    )))),
                },
                cx,
            );
            assert!(shell.transfer_mapping.as_ref().unwrap().preview.is_none());
            shell.on_executor_event(
                ExecutorEvent::TransferMappingFinished {
                    source: initial.clone(),
                    result: Ok(TransferMappingReply::Preview(Box::new(preview(
                        &initial,
                        "2".into(),
                        digest.clone(),
                        false,
                    )))),
                },
                cx,
            );
            assert_eq!(shell.transfer_mapping.as_ref().unwrap().outputs.len(), 2);
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("j d c h i");
        workspace.update(&mut cx, |shell, cx| {
            let state = shell.transfer_mapping.as_ref().unwrap();
            assert_eq!(state.outputs.len(), 2);
            state.outputs[0]
                .target
                .update(cx, |input, cx| input.set_text("renamed_copy", cx));
            state.outputs[0].expression.update(cx, |input, cx| {
                input.set_text(
                    r#"{"op":"upper","input":{"op":"column","source":"keep"}}"#,
                    cx,
                )
            });
        });
        cx.simulate_keystrokes("escape x");
        assert!(
            commands.try_recv().is_err(),
            "saving requires an exact successful preview"
        );
        cx.simulate_keystrokes("p enter");
        let candidate = match commands.try_recv().unwrap() {
            ExecutorCommand::TransferMapping {
                source,
                action: TransferMappingAction::Preview(request),
            } => {
                assert_eq!(request.format_version, "3");
                assert_eq!(request.options["unrelated"], "retain");
                assert!(request.options.get("column_projection").is_none());
                assert_eq!(
                    request.options["column_expressions"][0]["target"],
                    "renamed_copy"
                );
                assert_eq!(
                    request.options["column_expressions"][1]["expression"]["source"],
                    "keep"
                );
                source
            }
            _ => panic!("expected mapped preview"),
        };
        assert!(commands.try_recv().is_err());
        workspace.update(&mut cx, |shell, cx| {
            let state = shell.transfer_mapping.as_ref().unwrap();
            state.outputs[0]
                .target
                .update(cx, |input, cx| input.set_text("changed_after_send", cx));
            shell.on_executor_event(
                ExecutorEvent::TransferMappingFinished {
                    source: candidate.clone(),
                    result: Ok(TransferMappingReply::Preview(Box::new(preview(
                        &candidate,
                        "3".into(),
                        digest.clone(),
                        true,
                    )))),
                },
                cx,
            );
            assert!(!shell.mapping_ready(shell.transfer_mapping.as_ref().unwrap(), cx));
            shell.save_transfer_mapping(cx);
            assert_eq!(shell.transfer.recipes[0], recipe);
            shell.transfer_mapping.as_ref().unwrap().outputs[0]
                .target
                .update(cx, |input, cx| input.set_text("renamed_copy", cx));
        });
        assert!(commands.try_recv().is_err());
        cx.simulate_keystrokes("p");
        let candidate = match commands.try_recv().unwrap() {
            ExecutorCommand::TransferMapping { source, .. } => source,
            _ => panic!("expected fresh preview"),
        };
        workspace.update(&mut cx, |shell, cx| {
            shell.on_executor_event(
                ExecutorEvent::TransferMappingFinished {
                    source: candidate.clone(),
                    result: Ok(TransferMappingReply::Preview(Box::new(preview(
                        &candidate,
                        "3".into(),
                        digest.clone(),
                        true,
                    )))),
                },
                cx,
            )
        });
        workspace.update(&mut cx, |shell, cx| {
            shell
                .transfer
                .recipe_name_input
                .update(cx, |input, cx| input.set_text("unsaved main draft", cx));
            shell.save_transfer_mapping(cx);
            shell
                .transfer
                .recipe_name_input
                .update(cx, |input, cx| input.set_text("Mapping", cx));
        });
        assert!(commands.try_recv().is_err());
        workspace.update(&mut cx, |shell, cx| {
            shell.selected_instance_id = Some("other".into());
            shell.save_transfer_mapping(cx);
            shell.selected_instance_id = Some("local".into());
        });
        assert!(commands.try_recv().is_err());
        cx.simulate_keystrokes("x");
        let (saved_source, request) = match commands.try_recv().unwrap() {
            ExecutorCommand::TransferMapping {
                source,
                action: TransferMappingAction::Save(request),
            } => (source, *request),
            _ => panic!("expected reviewed recipe update"),
        };
        assert_eq!(request.expected_revision, 5);
        assert_eq!(request.recipe.options["unrelated"], "retain");
        assert_eq!(request.recipe.format_version, "3");
        let mut saved = recipe.clone();
        saved.revision += 1;
        saved.format_version = request.recipe.format_version;
        saved.options = request.recipe.options;
        workspace.update(&mut cx, |shell, cx| {
            shell.on_executor_event(
                ExecutorEvent::TransferMappingFinished {
                    source: saved_source.clone(),
                    result: Ok(TransferMappingReply::Saved(Box::new(saved.clone()))),
                },
                cx,
            )
        });
        workspace.read_with(&cx, |shell, cx| {
            assert!(shell.transfer_mapping.is_none());
            assert_eq!(shell.transfer.recipes[0], saved);
            assert!(
                shell.transfer.import_create_table,
                "unrelated unsaved controls survive mapping save"
            );
            assert_eq!(shell.transfer.recipe_version_input.read(cx).text(), "3");
        });
        workspace.update(&mut cx, |shell, cx| {
            shell.on_executor_event(
                ExecutorEvent::TransferMappingFinished {
                    source: saved_source,
                    result: Ok(TransferMappingReply::Preview(Box::new(preview(
                        &candidate,
                        "3".into(),
                        digest.clone(),
                        true,
                    )))),
                },
                cx,
            )
        });
        assert!(commands.try_recv().is_err());
        assert_eq!(sample_value(&sift_protocol::Value::Null), "NULL");
        assert_eq!(
            sample_value(&sift_protocol::Value::Text("NULL".into())),
            "\"NULL\""
        );
        let debug = format!(
            "{:?}",
            TransferMappingReply::Preview(Box::new(preview(&candidate, "3".into(), digest, true)))
        );
        assert!(!debug.contains("KEEP"));
    }
}
