//! Transfer recipe state, commands, progress, and correlated completion.

use super::*;

pub(super) struct TransferState {
    pub(super) recipes: Vec<sift_protocol::TransferRecipe>,
    pub(super) recipe_selected: usize,
    pub(super) recipe_focus_handle: FocusHandle,
    pub(super) recipe_edit: Option<sift_protocol::TransferRecipeId>,
    pub(super) recipe_delete_confirmation: Option<sift_protocol::TransferRecipeId>,
    pub(super) recipe_name_input: Entity<TextInput>,
    pub(super) recipe_format_input: Entity<TextInput>,
    pub(super) recipe_version_input: Entity<TextInput>,
    pub(super) recipe_options_input: Entity<TextInput>,
    pub(super) recipe_table_input: Entity<TextInput>,
    pub(super) recipe_sheet_input: Entity<TextInput>,
    pub(super) advanced_open: bool,
    pub(super) recipe_direction: sift_protocol::TransferDirection,
    pub(super) import_create_table: bool,
    pub(super) import_conflict_policy: sift_protocol::CsvConflictPolicy,
    pub(super) recipes_loading: bool,
    pub(super) recipes_error: Option<String>,
    pub(super) execution_generation: u64,
    pub(super) execution_pending: bool,
    pub(super) execution_result: Option<sift_protocol::TransferExecutionResult>,
}

impl WorkspaceShell {
    pub(super) fn open_transfer_recipes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.require_operation(
            sift_protocol::OperationKind::ReadTransferRecipe,
            "Open transfer recipes",
            cx,
        ) {
            return;
        }
        self.modal = Some(Modal::TransferRecipes);
        self.request_transfer_recipes(cx);
        self.transfer
            .recipe_name_input
            .focus_handle(cx)
            .focus(window, cx);
        cx.notify();
    }

    pub(super) fn handle_transfer_recipe_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.modifiers.modified()
            || [
                &self.transfer.recipe_name_input,
                &self.transfer.recipe_format_input,
                &self.transfer.recipe_version_input,
                &self.transfer.recipe_options_input,
                &self.transfer.recipe_table_input,
                &self.transfer.recipe_sheet_input,
            ]
            .iter()
            .any(|input| input.focus_handle(cx).is_focused(window))
        {
            return;
        }
        let count = self.transfer.recipes.len();
        match event.keystroke.key.as_str() {
            "j" | "down" if count > 0 => {
                self.edit_transfer_recipe((self.transfer.recipe_selected + 1).min(count - 1), cx)
            }
            "k" | "up" if count > 0 => {
                self.edit_transfer_recipe(self.transfer.recipe_selected.saturating_sub(1), cx)
            }
            "g" if count > 0 => self.edit_transfer_recipe(0, cx),
            "G" if count > 0 => self.edit_transfer_recipe(count - 1, cx),
            "n" => self.clear_transfer_recipe_editor(cx),
            "v" if self.transfer.recipe_edit.is_some() => self.validate_transfer_recipe(cx),
            "x" if self.transfer.recipe_edit.is_some() => {
                self.execute_selected_transfer_recipe(false, cx)
            }
            "p" if self.transfer.recipe_edit.is_some() => {
                self.execute_selected_transfer_recipe(true, cx)
            }
            "R" => self.request_transfer_recipes(cx),
            "escape" => self.dismiss_modal(&DismissModal, window, cx),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    pub(super) fn request_transfer_recipes(&mut self, cx: &mut Context<Self>) {
        let Some(workspace_id) = self.selected_workspace_id.map(sift_protocol::WorkspaceId) else {
            self.transfer.recipes_error = Some("Select a workspace to manage transfers".into());
            return;
        };
        let Some(sender) = &self.executor_sender else {
            self.transfer.recipes_error = Some("Transfer manager is unavailable".into());
            return;
        };
        self.transfer.recipes_loading = sender
            .send(ExecutorCommand::LoadTransferRecipes { workspace_id })
            .is_ok();
        if self.transfer.recipes_loading {
            self.transfer.recipes_error = None;
        }
        cx.notify();
    }

    pub(super) fn clear_transfer_recipe_editor(&mut self, cx: &mut Context<Self>) {
        self.transfer.recipe_edit = None;
        self.transfer.recipe_delete_confirmation = None;
        self.transfer.recipe_direction = sift_protocol::TransferDirection::Export;
        self.transfer
            .recipe_name_input
            .update(cx, |input, cx| input.set_text("", cx));
        self.transfer
            .recipe_format_input
            .update(cx, |input, cx| input.set_text("csv", cx));
        self.transfer
            .recipe_version_input
            .update(cx, |input, cx| input.set_text("1", cx));
        self.transfer
            .recipe_options_input
            .update(cx, |input, cx| input.set_text("{\"header\":true}", cx));
        self.transfer
            .recipe_table_input
            .update(cx, |input, cx| input.set_text("", cx));
        self.transfer
            .recipe_sheet_input
            .update(cx, |input, cx| input.set_text("Sheet1", cx));
        self.transfer.import_create_table = false;
        self.transfer.import_conflict_policy = sift_protocol::CsvConflictPolicy::Abort;
        self.transfer.execution_result = None;
        self.transfer.recipes_error = None;
    }

    pub(super) fn edit_transfer_recipe(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(recipe) = self.transfer.recipes.get(index).cloned() else {
            return;
        };
        self.transfer.recipe_selected = index;
        self.transfer.recipe_edit = Some(recipe.id);
        self.transfer.recipe_delete_confirmation = None;
        self.transfer.recipe_direction = recipe.direction;
        self.transfer
            .recipe_name_input
            .update(cx, |input, cx| input.set_text(recipe.name, cx));
        self.transfer
            .recipe_format_input
            .update(cx, |input, cx| input.set_text(recipe.format_id, cx));
        self.transfer
            .recipe_version_input
            .update(cx, |input, cx| input.set_text(recipe.format_version, cx));
        self.transfer.recipe_options_input.update(cx, |input, cx| {
            input.set_text(recipe.options.to_string(), cx)
        });
        self.transfer.execution_result = None;
        self.transfer.recipes_error = None;
        cx.notify();
    }

    pub(super) fn toggle_transfer_recipe_direction(&mut self, cx: &mut Context<Self>) {
        self.transfer.recipe_direction = match self.transfer.recipe_direction {
            sift_protocol::TransferDirection::Export => sift_protocol::TransferDirection::Import,
            sift_protocol::TransferDirection::Import => sift_protocol::TransferDirection::Export,
        };
        let format = match self.transfer.recipe_direction {
            sift_protocol::TransferDirection::Export => "csv",
            sift_protocol::TransferDirection::Import => "csv",
        };
        self.transfer
            .recipe_format_input
            .update(cx, |input, cx| input.set_text(format, cx));
        cx.notify();
    }

    pub(super) fn save_transfer_recipe(&mut self, cx: &mut Context<Self>) {
        if self.transfer.recipes_loading {
            return;
        }
        if !self.require_operation(
            sift_protocol::OperationKind::ManageTransferRecipe,
            "Save transfer recipe",
            cx,
        ) {
            return;
        }
        let Some(workspace_id) = self.selected_workspace_id.map(sift_protocol::WorkspaceId) else {
            return;
        };
        let name = self
            .transfer
            .recipe_name_input
            .read(cx)
            .text()
            .trim()
            .to_owned();
        let format_id = self
            .transfer
            .recipe_format_input
            .read(cx)
            .text()
            .trim()
            .to_owned();
        let format_version = self
            .transfer
            .recipe_version_input
            .read(cx)
            .text()
            .trim()
            .to_owned();
        if name.is_empty() || format_id.is_empty() || format_version.is_empty() {
            self.transfer.recipes_error =
                Some("Recipe name, format, and format version are required".into());
            cx.notify();
            return;
        }
        let options =
            match serde_json::from_str(self.transfer.recipe_options_input.read(cx).text().trim()) {
                Ok(serde_json::Value::Object(options)) => serde_json::Value::Object(options),
                Ok(_) => {
                    self.transfer.recipes_error =
                        Some("Recipe options must be a JSON object".into());
                    cx.notify();
                    return;
                }
                Err(error) => {
                    self.transfer.recipes_error =
                        Some(format!("Recipe options are invalid: {error}"));
                    cx.notify();
                    return;
                }
            };
        let (source, sink) = match self.transfer.recipe_direction {
            sift_protocol::TransferDirection::Export => (
                sift_protocol::TransferEndpoint::Query,
                sift_protocol::TransferEndpoint::Artifact,
            ),
            sift_protocol::TransferDirection::Import => (
                sift_protocol::TransferEndpoint::Upload,
                sift_protocol::TransferEndpoint::Table,
            ),
        };
        let expected_revision = self.transfer.recipe_edit.and_then(|recipe_id| {
            self.transfer
                .recipes
                .iter()
                .find(|recipe| recipe.id == recipe_id)
                .map(|recipe| recipe.revision)
        });
        let Some(sender) = &self.executor_sender else {
            return;
        };
        self.transfer.recipes_loading = sender
            .send(ExecutorCommand::SaveTransferRecipe {
                workspace_id,
                recipe_id: self.transfer.recipe_edit,
                expected_revision,
                request: sift_api_types::CreateTransferRecipeRequest {
                    name,
                    direction: self.transfer.recipe_direction,
                    source,
                    sink,
                    format_id,
                    format_version,
                    options,
                },
            })
            .is_ok();
        cx.notify();
    }

    pub(super) fn delete_transfer_recipe(&mut self, cx: &mut Context<Self>) {
        if !self.require_operation(
            sift_protocol::OperationKind::ManageTransferRecipe,
            "Delete transfer recipe",
            cx,
        ) {
            return;
        }
        let Some(recipe) = self
            .transfer
            .recipes
            .get(self.transfer.recipe_selected)
            .cloned()
        else {
            return;
        };
        if self.transfer.recipe_delete_confirmation != Some(recipe.id) {
            self.transfer.recipe_delete_confirmation = Some(recipe.id);
            self.transfer.recipes_error =
                Some("Press Confirm delete to permanently remove this recipe".into());
            cx.notify();
            return;
        }
        let Some(sender) = &self.executor_sender else {
            return;
        };
        self.transfer.recipes_loading = sender
            .send(ExecutorCommand::DeleteTransferRecipe {
                workspace_id: recipe.workspace_id,
                recipe_id: recipe.id,
                expected_revision: recipe.revision,
            })
            .is_ok();
        cx.notify();
    }

    pub(super) fn validate_transfer_recipe(&mut self, cx: &mut Context<Self>) {
        let Some(recipe) = self
            .transfer
            .recipes
            .get(self.transfer.recipe_selected)
            .cloned()
        else {
            return;
        };
        let Some(sender) = &self.executor_sender else {
            return;
        };
        self.transfer.recipes_loading = sender
            .send(ExecutorCommand::ValidateTransferRecipe {
                workspace_id: recipe.workspace_id,
                recipe_id: recipe.id,
            })
            .is_ok();
        cx.notify();
    }

    pub(super) fn parse_transfer_table(
        &self,
        cx: &App,
    ) -> Result<sift_protocol::ObjectPath, String> {
        let raw = self.transfer.recipe_table_input.read(cx).text().trim();
        if raw.is_empty() {
            return Err("Choose an import destination table".into());
        }
        let mut parts = raw.rsplitn(2, '.');
        let name = parts.next().unwrap_or_default().trim();
        let schema = parts.next().map(str::trim).filter(|part| !part.is_empty());
        if name.is_empty() {
            return Err("Import destination table is invalid".into());
        }
        Ok(sift_protocol::ObjectPath {
            catalog: None,
            schema: schema.map(str::to_owned),
            name: name.to_owned(),
            kind: Some(sift_protocol::ObjectKind::Table),
            routine_args: None,
        })
    }

    pub(super) fn toggle_transfer_create_table(&mut self, cx: &mut Context<Self>) {
        self.transfer.import_create_table = !self.transfer.import_create_table;
        cx.notify();
    }

    pub(super) fn toggle_transfer_conflict_policy(&mut self, cx: &mut Context<Self>) {
        self.transfer.import_conflict_policy = match self.transfer.import_conflict_policy {
            sift_protocol::CsvConflictPolicy::Abort => sift_protocol::CsvConflictPolicy::Skip,
            sift_protocol::CsvConflictPolicy::Skip => sift_protocol::CsvConflictPolicy::Quarantine,
            sift_protocol::CsvConflictPolicy::Quarantine => sift_protocol::CsvConflictPolicy::Abort,
        };
        cx.notify();
    }

    pub(super) fn execute_selected_transfer_recipe(
        &mut self,
        preview_only: bool,
        cx: &mut Context<Self>,
    ) {
        if self.transfer.execution_pending {
            return;
        }
        if !self.require_operation(
            sift_protocol::OperationKind::ExecuteTransferRecipe,
            "Execute transfer recipe",
            cx,
        ) {
            return;
        }
        let Some(recipe) = self
            .transfer
            .recipes
            .get(self.transfer.recipe_selected)
            .cloned()
        else {
            return;
        };
        match recipe.direction {
            sift_protocol::TransferDirection::Export => {
                let Some(query) = self.active_query_snapshot(cx) else {
                    self.transfer.recipes_error =
                        Some("Focus a query tab before running an export recipe".into());
                    cx.notify();
                    return;
                };
                self.start_transfer_recipe(
                    recipe.id,
                    Some(query.sql_text),
                    None,
                    None,
                    preview_only,
                    cx,
                );
            }
            sift_protocol::TransferDirection::Import => {
                let table = match self.parse_transfer_table(cx) {
                    Ok(table) => table,
                    Err(message) => {
                        self.transfer.recipes_error = Some(message);
                        cx.notify();
                        return;
                    }
                };
                let prompt = cx.prompt_for_paths(PathPromptOptions {
                    files: true,
                    directories: false,
                    multiple: false,
                    prompt: Some("Choose data for this transfer recipe".into()),
                });
                let background = cx.background_executor().clone();
                cx.spawn(async move |shell, cx| {
                    let Ok(Ok(Some(paths))) = prompt.await else {
                        return;
                    };
                    let Some(path) = paths.into_iter().next() else {
                        return;
                    };
                    let read_path = path.clone();
                    let data = background
                        .spawn(async move { transfer_input::read(&read_path) })
                        .await;
                    let _ = shell.update(cx, |shell, cx| match data {
                        Ok(data) if data.len() <= 64 * 1024 * 1024 => shell.start_transfer_recipe(
                            recipe.id,
                            None,
                            Some(data),
                            Some(table),
                            preview_only,
                            cx,
                        ),
                        Ok(_) => shell.show_error_toast(
                            "Transfer input exceeds the 64 MiB payload limit".into(),
                            cx,
                        ),
                        Err(error) => shell.show_error_toast(
                            format!("reading transfer input {}: {error}", path.display()),
                            cx,
                        ),
                    });
                })
                .detach();
            }
        }
    }

    pub(super) fn start_transfer_recipe(
        &mut self,
        recipe_id: sift_protocol::TransferRecipeId,
        sql: Option<String>,
        data: Option<Vec<u8>>,
        table: Option<sift_protocol::ObjectPath>,
        preview_only: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(sender) = &self.executor_sender else {
            return;
        };
        let sheet = self
            .transfer
            .recipe_sheet_input
            .read(cx)
            .text()
            .trim()
            .to_owned();
        self.transfer.execution_generation = self.transfer.execution_generation.wrapping_add(1);
        let generation = self.transfer.execution_generation;
        let reliability = self
            .transfer
            .recipes
            .iter()
            .find(|recipe| recipe.id == recipe_id)
            .map(|recipe| &recipe.options);
        let dry_run = preview_only
            || reliability
                .and_then(|options| options.get("dry_run"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
        let resume_from_row = reliability
            .and_then(|options| options.get("resume_from_row"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let type_mappings = reliability
            .and_then(|options| options.get("type_mappings"))
            .and_then(serde_json::Value::as_object)
            .map(|mappings| {
                mappings
                    .iter()
                    .filter_map(|(column, sql_type)| {
                        sql_type
                            .as_str()
                            .map(|sql_type| (column.clone(), sql_type.into()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.transfer.execution_pending = sender
            .send(ExecutorCommand::ExecuteTransferRecipe {
                generation,
                recipe_id,
                sql,
                data,
                table,
                sheet: (!sheet.is_empty()).then_some(sheet),
                create_table: self.transfer.import_create_table,
                conflict_policy: self.transfer.import_conflict_policy,
                dry_run,
                resume_from_row,
                type_mappings,
            })
            .is_ok();
        self.transfer.execution_result = None;
        self.transfer.recipes_error = None;
        cx.notify();
    }

    pub(super) fn cancel_transfer_recipe(&mut self, cx: &mut Context<Self>) {
        if !self.transfer.execution_pending {
            return;
        }
        if let Some(sender) = &self.executor_sender {
            let _ = sender.send(ExecutorCommand::CancelTransferRecipe {
                generation: self.transfer.execution_generation,
            });
        }
        // A completion already queued by the worker must not replace cancellation.
        self.transfer.execution_generation = self.transfer.execution_generation.wrapping_add(1);
        self.transfer.execution_pending = false;
        self.transfer.recipes_error =
            Some("Transfer request cancelled locally; refresh artifacts before retrying".into());
        cx.notify();
    }

    pub(super) fn on_transfer_event(&mut self, event: ExecutorEvent, cx: &mut Context<Self>) {
        match event {
            ExecutorEvent::TransferRecipesLoaded {
                workspace_id,
                result,
            } => {
                if self.selected_workspace_id != Some(workspace_id.0) {
                    return;
                }
                self.transfer.recipes_loading = false;
                match result {
                    Ok(recipes) => {
                        self.transfer.recipes = recipes;
                        self.transfer.recipe_selected = self
                            .transfer
                            .recipe_selected
                            .min(self.transfer.recipes.len().saturating_sub(1));
                        self.transfer.recipes_error = None;
                    }
                    Err(message) => self.transfer.recipes_error = Some(message),
                }
                cx.notify();
            }
            ExecutorEvent::TransferRecipeMutationFinished {
                workspace_id,
                action,
                result,
            } => {
                if self.selected_workspace_id != Some(workspace_id.0) {
                    return;
                }
                self.transfer.recipes_loading = false;
                match result {
                    Ok(()) => {
                        self.show_success_toast(action.into(), cx);
                        if action != "Transfer recipe validated" {
                            self.clear_transfer_recipe_editor(cx);
                        }
                        self.request_transfer_recipes(cx);
                    }
                    Err(message) => self.transfer.recipes_error = Some(message),
                }
                cx.notify();
            }
            ExecutorEvent::TransferRecipeExecutionFinished { generation, result } => {
                if generation != self.transfer.execution_generation {
                    return;
                }
                self.transfer.execution_pending = false;
                match result {
                    Ok(result) => {
                        let message = match &result {
                            sift_protocol::TransferExecutionResult::Artifact { artifact } => {
                                format!(
                                    "Transfer created artifact {} · {} bytes",
                                    artifact.id.0, artifact.byte_len
                                )
                            }
                            sift_protocol::TransferExecutionResult::Import { result, .. }
                                if result.dry_run =>
                            {
                                format!(
                                    "Transfer preview validated {} source row(s); no rows written",
                                    result.rows_validated
                                )
                            }
                            sift_protocol::TransferExecutionResult::Import { result, .. } => {
                                format!(
                                    "Transfer imported {} row(s) into {}",
                                    result.rows_inserted, result.table
                                )
                            }
                            sift_protocol::TransferExecutionResult::Validated {
                                format_id, ..
                            } => {
                                format!("Transfer {format_id} configuration validated")
                            }
                        };
                        self.transfer.execution_result = Some(result);
                        self.transfer.recipes_error = None;
                        self.show_success_toast(message, cx);
                    }
                    Err(message) => self.transfer.recipes_error = Some(message),
                }
                cx.notify();
            }
            _ => unreachable!("only transfer events are routed here"),
        }
    }
}
