//! Reviewed local CSV projection and background preparation for existing targets.
use super::csv_projection::Selection;
use super::*;

pub(super) fn projected_samples(preview: &CsvImportPreviewState) -> Vec<Vec<String>> {
    preview
        .rows
        .iter()
        .map(|row| {
            preview
                .columns
                .iter()
                .map(|column| row[column.source_index].clone())
                .collect()
        })
        .collect()
}

#[derive(Clone)]
struct FrozenImport {
    source: CsvImportSource,
    identity: PerformanceConnectionIdentity,
    selection: Vec<Selection>,
    data: Arc<[u8]>,
    types: Vec<String>,
    table: String,
    policy: sift_protocol::CsvConflictPolicy,
    native: bool,
    preview_only: bool,
    review: Option<NativeImportReview>,
    sql_server: Option<sift_protocol::SqlServerImportRequest>,
}
enum PreparedImport {
    Csv(Box<sift_protocol::CsvImportRequest>),
    Native(Box<(sift_protocol::BulkInsertRequest, NativeImportReview)>),
}
impl FrozenImport {
    fn prepare(self) -> Result<PreparedImport, String> {
        let mapped = if self.native && !self.preview_only {
            None
        } else {
            Some(csv_projection::rewrite(
                &self.data,
                &self.selection,
                csv_projection::MAX_BYTES,
            )?)
        };
        if !self.native {
            return Ok(PreparedImport::Csv(Box::new(
                sift_protocol::CsvImportRequest {
                    table: self.table,
                    data: mapped.unwrap(),
                    header: true,
                    delimiter: ',',
                    null_value: Some("NULL".into()),
                    create_table: false,
                    conflict_policy: self.policy,
                    dry_run: self.preview_only,
                    resume_from_row: 0,
                    type_mappings: Default::default(),
                    sql_server: self.sql_server,
                },
            )));
        }
        let (rows, token) = if self.preview_only {
            let columns = self
                .selection
                .iter()
                .map(|column| column.target.clone())
                .collect();
            (
                Arc::new(sqlite_native_import::rows(
                    mapped.as_ref().unwrap(),
                    columns,
                    &self.types,
                )?),
                None,
            )
        } else {
            let review = self
                .review
                .as_ref()
                .ok_or("Preview these exact native rows before importing")?;
            if review.source != self.source
                || review.selection != self.selection
                || review.types != self.types
                || !Arc::ptr_eq(&review.data, &self.data)
            {
                return Err("Columns, types, source or connection changed; preview again".into());
            }
            (review.rows.clone(), Some(review.token.clone()))
        };
        let request = sift_protocol::BulkInsertRequest {
            table: self.table,
            data: Vec::new(),
            format: sift_protocol::BulkInsertFormat::Native,
            native: Some(rows.as_ref().clone()),
            preview: self.preview_only,
            preview_token: token,
            confirm_write: !self.preview_only,
        };
        let review = NativeImportReview {
            source: self.source,
            types: self.types,
            rows,
            data: self.data,
            selection: self.selection,
            token: String::new(),
            preview_only: self.preview_only,
        };
        Ok(PreparedImport::Native(Box::new((request, review))))
    }
}

impl WorkspaceShell {
    fn csv_source(
        &self,
        preview: &CsvImportPreviewState,
    ) -> Result<(CsvImportSource, PerformanceConnectionIdentity), String> {
        let target = &preview.target;
        if self.selected_instance_id.as_deref() != Some(target.instance_id.as_str())
            || self.selected_tenant_id() != Some(target.tenant_id)
            || !self.profile_is_connected(target.profile_id)
        {
            return Err(
                "Connect the original server, tenant and CSV target before importing".into(),
            );
        }
        let identity = self
            .performance_connection_ids
            .get(&target.profile_id)
            .copied()
            .ok_or("Connection identity is unavailable; reconnect before importing")?;
        if preview
            .source_identity
            .is_some_and(|bound| bound != identity)
        {
            return Err(
                "The CSV target connection changed; reopen the file and review again".into(),
            );
        }
        Ok((
            CsvImportSource {
                target: target.clone(),
                session_id: identity.session,
                connection_id: identity.connection,
            },
            identity,
        ))
    }

    pub(super) fn csv_target_names(
        &self,
        preview: &CsvImportPreviewState,
        cx: &Context<Self>,
    ) -> Vec<String> {
        preview
            .columns
            .iter()
            .enumerate()
            .map(|(index, column)| {
                preview.target_inputs.get(index).map_or_else(
                    || column.target.clone(),
                    |input| input.read(cx).text().to_owned(),
                )
            })
            .collect()
    }
    fn csv_selection(
        &self,
        preview: &CsvImportPreviewState,
        cx: &Context<Self>,
    ) -> Result<Vec<Selection>, String> {
        let columns = preview
            .columns
            .iter()
            .zip(self.csv_target_names(preview, cx))
            .map(|(column, target)| Selection {
                index: column.source_index,
                target,
            })
            .collect::<Vec<_>>();
        csv_projection::validate(&columns, preview.source_columns.len())?;
        Ok(columns)
    }
    pub(super) fn csv_type_mappings(
        &self,
        preview: &CsvImportPreviewState,
        cx: &Context<Self>,
    ) -> Result<std::collections::BTreeMap<String, String>, String> {
        let mut mappings = std::collections::BTreeMap::new();
        if !preview.create_table {
            return Ok(mappings);
        }
        for (index, target) in self.csv_target_names(preview, cx).into_iter().enumerate() {
            let Some(input) = preview.type_inputs.get(index) else {
                continue;
            };
            let sql_type = input.read(cx).text().trim();
            if sql_type.is_empty() {
                continue;
            }
            if !validate_csv_sql_type(sql_type, preview.target.provider_id.as_str()) {
                return Err(format!(
                    "SQL type for `{target}` is invalid for this provider"
                ));
            }
            mappings.insert(target, sql_type.into());
        }
        Ok(mappings)
    }

    fn csv_inputs(
        column: &CsvPreviewColumn,
        provider: &sift_protocol::ProviderId,
        native: bool,
        kind: String,
        cx: &mut Context<Self>,
    ) -> (Entity<TextInput>, Entity<TextInput>) {
        let target = cx.new(|cx| {
            TextInput::new(column.target.clone(), "Target column", cx)
                .aria_label(format!("Target column for {}", column.source))
        });
        let inferred = if native {
            "TEXT"
        } else {
            csv_inferred_sql(column.inferred_type, provider.as_str())
        };
        let input = cx.new(|cx| {
            TextInput::new(kind, inferred, cx).aria_label(format!("SQL type for {}", column.source))
        });
        // The emitter owns each detached subscription; dropping a removed field releases it.
        for field in [&target, &input] {
            cx.subscribe(field, |shell, emitter, event: &TextInputEvent, cx| {
                if *event == TextInputEvent::Changed
                    && shell.csv_import_preview.as_ref().is_some_and(|preview| {
                        preview
                            .target_inputs
                            .iter()
                            .chain(&preview.type_inputs)
                            .any(|field| field.entity_id() == emitter.entity_id())
                    })
                {
                    shell.invalidate_csv_review(cx);
                }
            })
            .detach();
        }
        (target, input)
    }
    pub(super) fn initialize_csv_inputs(
        preview: &mut CsvImportPreviewState,
        cx: &mut Context<Self>,
    ) {
        preview.target_inputs.clear();
        preview.type_inputs.clear();
        for column in &preview.columns {
            let (target, kind) = Self::csv_inputs(
                column,
                &preview.target.provider_id,
                preview.native_mode,
                if preview.native_mode {
                    "TEXT".into()
                } else {
                    String::new()
                },
                cx,
            );
            preview.target_inputs.push(target);
            preview.type_inputs.push(kind);
        }
    }
    fn invalidate_csv_review(&mut self, cx: &mut Context<Self>) {
        if let Some(preview) = self.csv_import_preview.as_mut() {
            preview.validated = false;
            preview.validated_selection = None;
            preview.validated_source = None;
            preview.native_review = None;
            preview.sql_server_review = None;
            if preview
                .native_pending
                .as_ref()
                .is_some_and(|review| review.preview_only)
            {
                preview.native_pending = None;
            }
            preview.status = None;
        }
        cx.notify();
    }
    pub(super) fn set_csv_conflict_policy(
        &mut self,
        policy: sift_protocol::CsvConflictPolicy,
        cx: &mut Context<Self>,
    ) {
        let Some(preview) = self
            .csv_import_preview
            .as_mut()
            .filter(|preview| !preview.pending)
        else {
            return;
        };
        preview.conflict_policy = policy;
        self.invalidate_csv_review(cx);
    }

    fn reveal_csv_column(&self) {
        let Some(preview) = &self.csv_import_preview else {
            return;
        };
        for handle in [&self.csv_mapping_scroll, &self.csv_sample_scroll] {
            let width = f32::from(handle.bounds().size.width).max(150.);
            let left = -f32::from(handle.offset().x);
            let column_left = preview.selected_column as f32 * 150.;
            let target = if column_left < left {
                column_left
            } else if column_left + 150. > left + width {
                column_left + 150. - width
            } else {
                continue;
            };
            let mut offset = handle.offset();
            offset.x = px(-target.max(0.));
            handle.set_offset(offset);
        }
    }

    pub(super) fn select_csv_column(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(preview) = self.csv_import_preview.as_mut() {
            preview.selected_column = index.min(preview.columns.len().saturating_sub(1));
        }
        self.reveal_csv_column();
        cx.notify();
    }
    pub(super) fn change_csv_columns(&mut self, action: &str, cx: &mut Context<Self>) {
        let Some(preview) = self
            .csv_import_preview
            .as_ref()
            .filter(|preview| !preview.pending)
        else {
            return;
        };
        let selected = preview.selected_column;
        if action == "drop" && preview.columns.len() == 1 {
            self.show_error_toast("Keep at least one output column".into(), cx);
            return;
        }
        if action == "copy" && preview.columns.len() >= csv_projection::MAX_COLUMNS {
            self.show_error_toast("CSV review is limited to 256 outputs".into(), cx);
            return;
        }
        let copied = if action == "copy" {
            let mut column = preview.columns[selected].clone();
            column.target = csv_projection::copy_name(
                &self.csv_target_names(preview, cx)[selected],
                &self.csv_target_names(preview, cx),
            );
            let kind = preview
                .type_inputs
                .get(selected)
                .map_or(String::new(), |input| input.read(cx).text().to_owned());
            Some((column, kind))
        } else {
            None
        };
        let preview = self.csv_import_preview.as_mut().unwrap();
        match action {
            "drop" => {
                preview.columns.remove(selected);
                preview.target_inputs.remove(selected);
                preview.type_inputs.remove(selected);
                preview.selected_column = selected.min(preview.columns.len() - 1);
            }
            "left" | "right" => {
                let next = if action == "left" {
                    selected.saturating_sub(1)
                } else {
                    (selected + 1).min(preview.columns.len() - 1)
                };
                preview.columns.swap(selected, next);
                preview.target_inputs.swap(selected, next);
                preview.type_inputs.swap(selected, next);
                preview.selected_column = next;
            }
            "copy" => {
                let (column, kind) = copied.unwrap();
                let (target, kind) = Self::csv_inputs(
                    &column,
                    &preview.target.provider_id,
                    preview.native_mode,
                    kind,
                    cx,
                );
                preview.columns.insert(selected + 1, column);
                preview.target_inputs.insert(selected + 1, target);
                preview.type_inputs.insert(selected + 1, kind);
                preview.selected_column = selected + 1;
            }
            "reset" => {
                preview.columns = preview.source_columns.clone();
                preview.selected_column = 0;
                Self::initialize_csv_inputs(preview, cx);
            }
            _ => return,
        }
        self.reveal_csv_column();
        self.invalidate_csv_review(cx);
    }

    pub(super) fn csv_preview_to_transfer_recipe(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(preview) = self
            .csv_import_preview
            .as_ref()
            .filter(|preview| !preview.pending)
        else {
            return;
        };
        if preview.native_mode {
            self.show_toast(
                "Native typed rows cannot be saved as a CSV transfer recipe".into(),
                cx,
            );
            return;
        }
        if self.selected_instance_id.as_deref() != Some(preview.target.instance_id.as_str())
            || self.selected_tenant_id() != Some(preview.target.tenant_id)
        {
            self.show_error_toast(
                "Return to the CSV source server and tenant before saving its recipe".into(),
                cx,
            );
            return;
        }
        let options = (|| {
            let selection = self.csv_selection(preview, cx)?;
            let headers = preview
                .source_columns
                .iter()
                .map(|column| column.source.clone())
                .collect::<Vec<_>>();
            let projection = csv_projection::recipe(&selection, &headers)?;
            let mut options = serde_json::json!({"header":true,"type_mappings":self.csv_type_mappings(preview,cx)?,"column_projection":projection});
            if let Some(mode) = preview.sql_server_mode {
                options["sql_server_mode"] =
                    serde_json::to_value(mode).map_err(|_| "Invalid column modes")?;
            }
            Ok::<_, String>(options)
        })();
        let options = match options {
            Ok(options) => options,
            Err(error) => {
                self.show_error_toast(error, cx);
                return;
            }
        };
        let table = preview.table.clone();
        let create_table = preview.create_table;
        let policy = preview.conflict_policy;
        let resume = preview.sql_server_mode.is_none()
            && preview.target.provider_id.as_str() == "sift/sql-server"
            && !create_table
            && policy == sift_protocol::CsvConflictPolicy::Abort;
        if self.selected_workspace_id.is_none() {
            self.show_error_toast(
                "Select a workspace before saving a transfer recipe".into(),
                cx,
            );
            return;
        }
        self.clear_transfer_recipe_editor(cx);
        self.transfer.recipe_direction = sift_protocol::TransferDirection::Import;
        self.transfer.advanced_open = true;
        self.transfer.import_create_table = create_table;
        self.transfer.import_conflict_policy = policy;
        self.transfer.recipe_name_input.update(cx, |input, cx| {
            input.set_text(format!("Import {table}"), cx)
        });
        self.transfer
            .recipe_table_input
            .update(cx, |input, cx| input.set_text(table, cx));
        self.transfer
            .recipe_version_input
            .update(cx, |input, cx| input.set_text("2", cx));
        self.transfer
            .recipe_options_input
            .update(cx, |input, cx| input.set_text(options.to_string(), cx));
        if resume {
            self.toggle_transfer_durable_resume(cx);
        }
        self.open_transfer_recipes(window, cx);
    }

    pub(super) fn confirm_csv_import(
        &mut self,
        preview_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(preview) = self
            .csv_import_preview
            .as_ref()
            .filter(|preview| !preview.pending)
        else {
            return;
        };
        if preview.native_mode {
            self.confirm_sqlite_native_import(preview_only, cx);
            return;
        }
        if !self.require_operation(sift_protocol::OperationKind::ImportCsv, "Import CSV", cx) {
            return;
        }
        if self.csv_import_preview.as_ref().unwrap().create_table {
            let preview = self.csv_import_preview.as_ref().unwrap();
            let ddl = (|| {
                let selection = self.csv_selection(preview, cx)?;
                let targets = selection
                    .iter()
                    .map(|column| column.target.clone())
                    .collect::<Vec<_>>();
                csv_create_table_sql(preview, &self.csv_type_mappings(preview, cx)?, &targets)
            })();
            let sql = match ddl {
                Ok(sql) => sql,
                Err(error) => {
                    self.show_error_toast(error, cx);
                    return;
                }
            };
            let preview = self.csv_import_preview.take().unwrap();
            self.modal = None;
            let item =
                self.open_sql_scratch(format!("create-{}.sql", preview.table), sql, window, cx);
            self.query_semantic_targets
                .insert(item, preview.target.clone());
            self.show_success_toast(
                format!(
                    "Created CREATE TABLE query for {}. Review and run it before importing rows.",
                    preview.target.profile_name
                ),
                cx,
            );
            return;
        }
        self.prepare_csv_import(preview_only, false, cx);
    }
    pub(super) fn confirm_sqlite_native_import(
        &mut self,
        preview_only: bool,
        cx: &mut Context<Self>,
    ) {
        if !self.require_operation(
            sift_protocol::OperationKind::BulkInsert,
            "Import typed SQLite rows",
            cx,
        ) {
            return;
        }
        self.prepare_csv_import(preview_only, true, cx);
    }
    fn prepare_csv_import(&mut self, preview_only: bool, native: bool, cx: &mut Context<Self>) {
        let frozen = (|| {
            let preview = self
                .csv_import_preview
                .as_ref()
                .filter(|preview| !preview.pending)
                .ok_or("CSV request is already pending")?;
            if native
                && (!preview.native_mode
                    || self.active_semantic_target().as_ref() != Some(&preview.target))
            {
                return Err("Select the original SQLite target connection before importing".into());
            }
            let (source, identity) = self.csv_source(preview)?;
            let selection = self.csv_selection(preview, cx)?;
            if !native
                && preview.target.provider_id.as_str() == "sift/sql-server"
                && !preview_only
                && (!preview.validated
                    || preview.validated_selection.as_ref() != Some(&selection)
                    || preview.validated_source.as_ref() != Some(&source))
            {
                return Err(
                    "Preview this SQL Server import after reviewing its exact column selection"
                        .into(),
                );
            }
            let types = preview
                .type_inputs
                .iter()
                .map(|input| input.read(cx).text().to_owned())
                .collect::<Vec<_>>();
            let review = if native && !preview_only {
                let review = preview
                    .native_review
                    .as_ref()
                    .ok_or("Preview these exact native rows before importing")?;
                if review.source != source
                    || review.selection != selection
                    || review.types != types
                    || !Arc::ptr_eq(&review.data, &preview.data)
                {
                    return Err(
                        "Columns, types, source or connection changed; preview again".into(),
                    );
                }
                Some(review.clone())
            } else {
                None
            };
            let sql_server = preview.sql_server_mode.map(|mode| {
                let confirmation = if preview_only {
                    None
                } else {
                    preview
                        .sql_server_review
                        .as_ref()
                        .filter(|review| review.mode == mode)
                        .map(|review| sift_protocol::SqlServerImportConfirmation {
                            precondition: review.precondition.clone(),
                            confirmed: true,
                        })
                };
                sift_protocol::SqlServerImportRequest {
                    run_id: uuid::Uuid::new_v4(),
                    mode,
                    confirmation,
                }
            });
            if sql_server
                .as_ref()
                .is_some_and(|control| !preview_only && control.confirmation.is_none())
            {
                return Err("Preview these generated-column modes before applying".into());
            }
            Ok::<_, String>(FrozenImport {
                source,
                identity,
                selection,
                data: preview.data.clone(),
                types,
                table: preview.table.clone(),
                policy: preview.conflict_policy,
                native,
                preview_only,
                review,
                sql_server,
            })
        })();
        let frozen = match frozen {
            Ok(frozen) => frozen,
            Err(error) => {
                self.show_error_toast(error, cx);
                return;
            }
        };
        self.csv_import_generation = self.csv_import_generation.wrapping_add(1);
        let generation = self.csv_import_generation;
        let preview = self.csv_import_preview.as_mut().unwrap();
        preview.pending = true;
        preview.sql_server_run = frozen.sql_server.as_ref().map(|c| c.run_id);
        preview.import_progress = None;
        preview.validated = false;
        preview.source_identity = Some(frozen.identity);
        preview.status = Some("Preparing selected columns…".into());
        let background = cx.background_executor().clone();
        let worker = frozen.clone();
        cx.spawn(async move |shell, cx| {
            let prepared = background.spawn(async move { worker.prepare() }).await;
            let _ = shell.update(cx, |shell, cx| {
                shell.finish_csv_preparation(generation, frozen, prepared, cx)
            });
        })
        .detach();
        cx.notify();
    }
    fn finish_csv_preparation(
        &mut self,
        generation: u64,
        frozen: FrozenImport,
        prepared: Result<PreparedImport, String>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.csv_import_generation || self.modal != Some(Modal::CsvImport) {
            return;
        }
        let current = self.csv_import_preview.as_ref().is_some_and(|preview| {
            preview.pending
                && self.csv_source(preview).ok().as_ref()
                    == Some(&(frozen.source.clone(), frozen.identity))
                && self.csv_selection(preview, cx).ok().as_ref() == Some(&frozen.selection)
                && Arc::ptr_eq(&preview.data, &frozen.data)
                && preview.sql_server_mode == frozen.sql_server.as_ref().map(|control| control.mode)
                && preview.native_mode == frozen.native
                && preview.conflict_policy == frozen.policy
                && preview
                    .type_inputs
                    .iter()
                    .map(|input| input.read(cx).text().to_owned())
                    .collect::<Vec<_>>()
                    == frozen.types
        });
        let prepared = if current {
            prepared
        } else {
            Err("Mapping, source or connection changed during preparation; review again".into())
        };
        let command = match prepared {
            Ok(PreparedImport::Csv(request)) => {
                let preview = self.csv_import_preview.as_mut().unwrap();
                preview.validated_selection = frozen.preview_only.then_some(frozen.selection);
                preview.validated_source = frozen.preview_only.then_some(frozen.source.clone());
                ExecutorCommand::ImportCsv {
                    generation,
                    source: frozen.source,
                    request: *request,
                }
            }
            Ok(PreparedImport::Native(prepared)) => {
                let (request, review) = *prepared;
                self.csv_import_preview.as_mut().unwrap().native_pending = Some(review);
                ExecutorCommand::ImportSqliteNative {
                    generation,
                    source: frozen.source,
                    request,
                }
            }
            Err(error) => {
                if let Some(preview) = self.csv_import_preview.as_mut() {
                    preview.pending = false;
                    preview.status = Some(error.clone());
                }
                self.show_error_toast(error, cx);
                return;
            }
        };
        if self
            .executor_sender
            .as_ref()
            .is_some_and(|sender| sender.send(command).is_ok())
        {
            if let Some(preview) = self.csv_import_preview.as_mut() {
                preview.status = Some(
                    if frozen.preview_only {
                        "Validating selected columns…"
                    } else {
                        "Importing reviewed columns…"
                    }
                    .into(),
                );
            }
        } else if let Some(preview) = self.csv_import_preview.as_mut() {
            preview.pending = false;
            preview.native_pending = None;
            preview.status = Some("Database executor is unavailable".into());
        }
        cx.notify();
    }

    pub(super) fn receive_csv_import(
        &mut self,
        generation: u64,
        result: Result<sift_protocol::CsvImportResponse, String>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.csv_import_generation {
            return;
        }
        let valid = self.csv_import_preview.as_ref().is_some_and(|preview| {
            preview.validated_selection.is_some()
                && preview.validated_source.is_some()
                && self.csv_selection(preview, cx).ok().as_ref()
                    == preview.validated_selection.as_ref()
                && self
                    .csv_source(preview)
                    .ok()
                    .map(|(source, _)| source)
                    .as_ref()
                    == preview.validated_source.as_ref()
        });
        if let Some(preview) = self.csv_import_preview.as_mut() {
            preview.pending = false;
            match result {
                Ok(response) => {
                    preview.validated = response.dry_run && valid;
                    preview.sql_server_review = response
                        .sql_server_review
                        .clone()
                        .map(|review| *review)
                        .filter(|_| response.dry_run && valid);
                    preview.status = Some(if response.dry_run && !valid {
                        "Columns or source changed; preview again".into()
                    } else if response.dry_run {
                        format!("Validated {} source rows; no data written. Database conversions and constraints are checked when importing.",response.rows_validated)
                    } else {
                        format!(
                            "Imported {} rows, skipped {} into {}.",
                            response.rows_inserted, response.rows_skipped, response.table
                        )
                    });
                    if response
                        .sql_server_review
                        .as_ref()
                        .is_some_and(|review| review.cleanup_pending)
                    {
                        preview
                            .status
                            .get_or_insert_with(String::new)
                            .push_str(" Native connection cleanup remains pending.");
                    }
                    if !response.dry_run {
                        if let Some(sender) = &self.executor_sender {
                            let _ = sender.send(ExecutorCommand::RefreshSchema);
                        }
                    }
                }
                Err(error) => {
                    preview.validated = false;
                    preview.status = Some(error.clone());
                    self.record_runtime_error(None, "CSV import", error.clone(), cx);
                    self.show_error_toast(error, cx);
                }
            }
        }
        cx.notify();
    }
    pub(super) fn receive_sqlite_native_import(
        &mut self,
        generation: u64,
        result: Result<sift_protocol::BulkInsertResponse, String>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.csv_import_generation {
            return;
        }
        let valid = self.csv_import_preview.as_ref().is_some_and(|preview| {
            preview.native_pending.as_ref().is_some_and(|review| {
                self.csv_source(preview)
                    .ok()
                    .map(|(source, _)| source)
                    .as_ref()
                    == Some(&review.source)
                    && self.csv_selection(preview, cx).ok().as_ref() == Some(&review.selection)
                    && preview
                        .type_inputs
                        .iter()
                        .map(|input| input.read(cx).text().to_owned())
                        .collect::<Vec<_>>()
                        == review.types
            })
        });
        if let Some(preview) = self.csv_import_preview.as_mut() {
            preview.pending = false;
            let pending = preview.native_pending.take();
            match result {
                Ok(response) => match (pending, response.preview_token) {
                    (Some(mut review), Some(token)) if review.preview_only && valid => {
                        review.token = token;
                        preview.native_review = Some(review);
                        preview.status=Some(format!("Validated {} typed rows for {}. Affinities: {}. Press x to apply exactly this preview.",response.rows_validated,preview.table,response.target_affinities.join(", ")));
                    }
                    (Some(review), None) if !review.preview_only => {
                        preview.native_review = None;
                        preview.status = Some(format!(
                            "Inserted {} typed rows into {}.",
                            response.rows_inserted, preview.table
                        ));
                        if let Some(sender) = &self.executor_sender {
                            let _ = sender.send(ExecutorCommand::RefreshSchema);
                        }
                    }
                    _ => {
                        preview.native_review = None;
                        preview.status=Some("Native review no longer matches the current columns/source; preview again".into());
                    }
                },
                Err(error) => {
                    preview.native_review = None;
                    preview.status = Some(error.clone());
                    self.record_runtime_error(None, "Native SQLite import", error.clone(), cx);
                    self.show_error_toast(error, cx);
                }
            }
        }
        cx.notify();
    }

    pub(super) fn toggle_sqlite_native_import(&mut self, cx: &mut Context<Self>) {
        let native = self
            .csv_import_preview
            .as_ref()
            .is_some_and(|preview| preview.native_mode);
        let operation = if native {
            sift_protocol::OperationKind::ImportCsv
        } else {
            sift_protocol::OperationKind::BulkInsert
        };
        if !self.require_operation(operation, "Switch import mode", cx) {
            return;
        }
        let Some(preview) = self.csv_import_preview.as_mut().filter(|preview| {
            !preview.pending
                && !preview.create_table
                && preview.target.provider_id.as_str() == "sift/sqlite"
        }) else {
            return;
        };
        preview.native_mode = !preview.native_mode;
        preview.native_pending = None;
        if preview.native_mode {
            for input in &preview.type_inputs {
                input.update(cx, |input, cx| input.set_text("TEXT", cx));
            }
        }
        self.invalidate_csv_review(cx);
    }
    pub(super) fn cycle_sqlserver_import_mode(&mut self, key: &str, cx: &mut Context<Self>) {
        if let Some(preview) = self.csv_import_preview.as_mut().filter(|p| {
            !p.pending && !p.create_table && p.target.provider_id.as_str() == "sift/sql-server"
        }) {
            use sift_protocol::{SqlServerComputedMode as C, SqlServerIdentityMode as I};
            if key == "g" {
                preview.sql_server_mode = if preview.sql_server_mode.is_some() {
                    None
                } else {
                    Some(Default::default())
                };
            } else if let Some(mode) = &mut preview.sql_server_mode {
                if key == "1" {
                    mode.identity = match mode.identity {
                        I::Reject => I::Generate,
                        I::Generate => I::Preserve,
                        I::Preserve => I::Reject,
                    };
                } else {
                    mode.computed = if mode.computed == C::Reject {
                        C::Omit
                    } else {
                        C::Reject
                    };
                }
            }
            preview.conflict_policy = sift_protocol::CsvConflictPolicy::Abort;
            self.invalidate_csv_review(cx);
        }
        cx.notify();
    }
    pub(super) fn cancel_csv_import(&mut self, cx: &mut Context<Self>) {
        let Some(preview) = self
            .csv_import_preview
            .as_mut()
            .filter(|preview| preview.pending)
        else {
            return;
        };
        if let Some(sender) = &self.executor_sender {
            let _ = sender.send(ExecutorCommand::CancelCsvImport {
                generation: self.csv_import_generation,
            });
        }
        if preview.sql_server_run.is_some() {
            preview.status = Some("Cancellation requested; waiting for the original outcome. COMMIT may be uncertain; inspect before retrying.".into());
            cx.notify();
            return;
        }
        self.csv_import_generation = self.csv_import_generation.wrapping_add(1);
        preview.pending = false;
        preview.validated = false;
        preview.native_review = None;
        preview.native_pending = None;
        preview.status=Some("CSV import cancelled locally. If a server request had started, inspect the original target before retrying; committed rows may remain.".into());
        cx.notify();
    }
    pub(super) fn handle_csv_import_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(preview) = self.csv_import_preview.as_ref() else {
            return;
        };
        let fields = preview
            .columns
            .iter()
            .enumerate()
            .flat_map(|(index, _)| {
                let mut fields = Vec::new();
                if let Some(field) = preview.target_inputs.get(index) {
                    fields.push(field.clone());
                }
                if preview.create_table || preview.native_mode {
                    if let Some(field) = preview.type_inputs.get(index) {
                        fields.push(field.clone());
                    }
                }
                fields
            })
            .collect::<Vec<_>>();
        let focused = fields
            .iter()
            .position(|field| field.focus_handle(cx).is_focused(window));
        let key = event.keystroke.unparse();
        if let Some(index) = focused {
            match key.as_str() {
                "escape" => self.focus_handle.focus(window, cx),
                "tab" | "shift-tab" => {
                    let next = if key == "tab" {
                        index.checked_add(1)
                    } else {
                        index.checked_sub(1)
                    };
                    if let Some(field) = next.and_then(|index| fields.get(index)) {
                        field.focus_handle(cx).focus(window, cx);
                    } else {
                        self.focus_handle.focus(window, cx);
                    }
                }
                _ => return,
            }
        } else {
            let selected = preview.selected_column;
            match key.as_str() {
                "h" | "left" => self.select_csv_column(selected.saturating_sub(1), cx),
                "l" | "right" => self.select_csv_column(selected + 1, cx),
                "shift-h" => self.change_csv_columns("left", cx),
                "shift-l" => self.change_csv_columns("right", cx),
                "d" => self.change_csv_columns("drop", cx),
                "c" => self.change_csv_columns("copy", cx),
                "shift-r" => self.change_csv_columns("reset", cx),
                "i" | "t" | "tab" if !preview.pending => {
                    let field = if key == "t" && (preview.create_table || preview.native_mode) {
                        preview.type_inputs.get(selected)
                    } else {
                        preview.target_inputs.get(selected)
                    };
                    if let Some(field) = field {
                        field.focus_handle(cx).focus(window, cx);
                    }
                }
                "v" => self.confirm_csv_import(true, window, cx),
                "x" => self.confirm_csv_import(false, window, cx),
                "n" => self.toggle_sqlite_native_import(cx),
                "g" | "1" | "2"
                    if !preview.pending
                        && !preview.create_table
                        && preview.target.provider_id.as_str() == "sift/sql-server" =>
                {
                    self.cycle_sqlserver_import_mode(key.as_str(), cx)
                }
                "r" => self.csv_preview_to_transfer_recipe(window, cx),
                "escape" if preview.pending => self.cancel_csv_import(cx),
                "escape" => self.dismiss_modal(&DismissModal, window, cx),
                _ => return,
            }
        }
        cx.stop_propagation();
        cx.notify();
    }
}
