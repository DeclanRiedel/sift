//! Modal content and presentation; geometry is shared through modal_layout.

use super::*;

impl WorkspaceShell {
    pub(super) fn render_modal(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let colors = cx.theme().colors;
        self.modal.as_ref().map(|modal| {
            let server_picker = matches!(modal, Modal::ServerPicker);
            let account = matches!(modal, Modal::Account);
            let app_bar_modal = matches!(
                modal,
                Modal::ServerPicker
                    | Modal::ServerConnection
                    | Modal::Account
                    | Modal::CommandPalette
            );
            let database_connection = matches!(modal, Modal::DatabaseConnection);
            let command_palette = matches!(modal, Modal::CommandPalette);
            let data_results = matches!(modal, Modal::DataResults(_));
            let padded = !database_connection && !command_palette && !data_results && !account && !server_picker;
            let card_width = modal_layout::content_width(modal, self.database_wizard_step)
                + if padded { 24.0 } else { 0.0 };
            let toolbar_height = cx.theme().metrics.toolbar_height;
            let viewport = window.viewport_size();
            let layer_height = viewport.height - if app_bar_modal { toolbar_height } else { px(0.) };
            let popover = server_picker || account || command_palette;
            let max_card_height = (layer_height - px(if popover { 12.0 } else { 32.0 })).max(px(1.));
            let content = match modal {
                Modal::CommandPalette => {
                    let input = self.query_input.read(cx).text();
                    let (mode, _) = CommandPaletteMode::parse(input);
                    let show_prefix_guide = input.trim().is_empty();
                    let items = self.command_palette_items(cx);
                    let item_count = items.len();
                    let palette_height =
                        item_count.min(PALETTE_VISIBLE_ROWS) as f32 * PALETTE_ROW_HEIGHT;
                    let empty_message: SharedString = match mode {
                        CommandPaletteMode::Commands => "No matching commands".into(),
                        CommandPaletteMode::WorkspaceFiles if self.workspace_files.loading() => {
                            "Loading workspace files…".into()
                        }
                        CommandPaletteMode::WorkspaceFiles => {
                            "No matching workspace files".into()
                        }
                        CommandPaletteMode::Schema => match &self.schema_search_state {
                            SchemaSearchState::Loading => "Searching schema…".into(),
                            SchemaSearchState::Failed(message) => message.clone().into(),
                            _ => "No matching database objects".into(),
                        },
                        CommandPaletteMode::Data => match &self.data_search_state {
                            DataSearchState::Loading => "Searching table data…".into(),
                            DataSearchState::Failed(message) => message.clone().into(),
                            _ => "No matching table rows".into(),
                        },
                        CommandPaletteMode::Checkpoints if self.workspace_files.loading() => {
                            "Loading workspace checkpoints…".into()
                        }
                        CommandPaletteMode::Checkpoints => {
                            "No matching workspace checkpoints".into()
                        }
                        CommandPaletteMode::OpenTabs => "No matching open tabs".into(),
                        CommandPaletteMode::SavedQueries if self.saved_queries_loading => {
                            "Loading saved queries…".into()
                        }
                        CommandPaletteMode::SavedQueries => "No matching saved queries".into(),
                        CommandPaletteMode::QueryHistory if self.query_history.loading => {
                            "Loading query history…".into()
                        }
                        CommandPaletteMode::QueryHistory => self
                            .query_history.error
                            .clone()
                            .unwrap_or_else(|| "No matching query history".into())
                            .into(),
                    };
                    div()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .h(px(28.))
                                .px_2()
                                .flex()
                                .items_center()
                                .justify_between()
                                .border_b_1()
                                .border_color(colors.subtle_border)
                                .text_xs()
                                .text_color(colors.muted_text)
                                .child(
                                    div()
                                        .debug_selector(|| "palette-mode-heading".into())
                                        .child(mode.label()),
                                )
                                .when(show_prefix_guide, |heading| {
                                    heading.child(
                                        div()
                                            .debug_selector(|| "palette-prefix-guide".into())
                                            .flex()
                                            .items_center()
                                            .gap_3()
                                            .child("/ files")
                                            .child("@ schema")
                                            .child("$ data")
                                            .child("^ checkpoints")
                                            .child("# tabs")
                                            .child("? saved")
                                            .child("! history"),
                                    )
                                }),
                        )
                        .when(items.is_empty(), |palette| {
                            palette.child(
                                div()
                                    .px_2()
                                    .py_4()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_color(colors.muted_text)
                                    .child(empty_message),
                            )
                        })
                        .when(item_count > 0, |palette| {
                            palette.child(
                                uniform_list(
                                    "command-list",
                                    item_count,
                                    cx.processor(move |shell, range: Range<usize>, _, cx| {
                                        let items = shell.command_palette_items(cx);
                                        let selected_idx = shell
                                            .palette_selected
                                            .min(items.len().saturating_sub(1));
                                        range
                                            .filter_map(|idx| {
                                                items
                                                    .get(idx)
                                                    .cloned()
                                                    .map(|item| (idx, item))
                                            })
                                            .map(|(idx, matched)| {
                                                let ranges = matched.ranges;
                                                let (label, right, enabled, key_binding) = match matched.item {
                                                    CommandPaletteItem::Command(command) => {
                                                        let right = command.disabled_reason.clone().unwrap_or_else(|| {
                                                            if command.language.is_empty() {
                                                                command.shortcut.into()
                                                            } else {
                                                                command.language.clone()
                                                            }
                                                        });
                                                        (command.label.to_owned(), right, command.enabled(), true)
                                                    }
                                                    CommandPaletteItem::Line(line) => {
                                                        let target = if shell.command_palette_origin
                                                            == WorkspaceSurface::Results
                                                        {
                                                            "ROW"
                                                        } else {
                                                            "LINE"
                                                        };
                                                        (format!("Go to {} {line}", target.to_lowercase()), target.into(), true, false)
                                                    }
                                                    CommandPaletteItem::WorkspaceFile(_, path) => (path, "FILE".into(), true, false),
                                                    CommandPaletteItem::Schema(hit) => {
                                                        let right = hit.type_display.unwrap_or_else(|| "SCHEMA".into());
                                                        (hit.display, right, true, false)
                                                    }
                                                    CommandPaletteItem::Data(hit) => {
                                                        let table = [
                                                            hit.table.schema.as_deref(),
                                                            Some(hit.table.name.as_str()),
                                                        ]
                                                        .into_iter()
                                                        .flatten()
                                                        .collect::<Vec<_>>()
                                                        .join(".");
                                                        let label = hit
                                                            .columns
                                                            .iter()
                                                            .zip(hit.row.values.iter())
                                                            .take(4)
                                                            .map(|(column, value)| format!(
                                                                "{column}: {}",
                                                                render_value(value).text
                                                            ))
                                                            .collect::<Vec<_>>()
                                                            .join(" · ");
                                                        (label, table, true, false)
                                                    }
                                                    CommandPaletteItem::Checkpoint(checkpoint) => {
                                                        let label = checkpoint.name.unwrap_or_else(|| {
                                                            format!("{:?}", checkpoint.reason)
                                                        });
                                                        (
                                                            label,
                                                            format!("REV {}", checkpoint.workspace_revision.0),
                                                            true,
                                                            false,
                                                        )
                                                    }
                                                    CommandPaletteItem::OpenTab { pane_index, title, .. } => {
                                                        (title, format!("PANE {}", pane_index + 1), true, false)
                                                    }
                                                    CommandPaletteItem::SavedQuery(saved) => {
                                                        let right = if saved.tags.is_empty() {
                                                            "SAVED".into()
                                                        } else {
                                                            saved.tags.join(", ")
                                                        };
                                                        (saved.name, right, true, false)
                                                    }
                                                    CommandPaletteItem::QueryHistory(entry) => {
                                                        let status = match entry.status {
                                                            sift_api_types::QueryStatus::Ok => "OK",
                                                            sift_api_types::QueryStatus::Error => "ERROR",
                                                            sift_api_types::QueryStatus::Canceled => "CANCELED",
                                                        };
                                                        let connection = shell.query_history_connection_name(
                                                            entry.connection_profile_id.map(|profile| profile.0),
                                                        );
                                                        (
                                                            entry.sql_text.replace(['\n', '\r'], " "),
                                                            format!("{status} · {connection}"),
                                                            true,
                                                            false,
                                                        )
                                                    }
                                                };
                                                let selected = idx == selected_idx;
                                                let mut row = div()
                                                    .id(SharedString::from(format!("command-palette-item-{idx}")))
                                                    .w_full()
                                                    .flex()
                                                    .items_center()
                                                    .justify_between()
                                                    .gap_2()
                                                    .h(px(PALETTE_ROW_HEIGHT))
                                                    .px_2()
                                                    .rounded_sm()
                                                    .when(selected && enabled, |row| {
                                                        row.bg(colors.active_surface)
                                                    })
                                                    .when(!enabled, |row| {
                                                        row.text_color(colors.muted_text)
                                                    })
                                                    .child(highlight_fuzzy_ranges(
                                                        label,
                                                        &ranges,
                                                        colors.accent,
                                                    ))
                                                    .when_some(
                                                        (!right.is_empty()).then_some(right),
                                                        |row, right| {
                                                            if enabled && key_binding {
                                                                row.child(KeyBinding::new(right))
                                                            } else {
                                                                row.child(
                                                                    div()
                                                                        .flex_none()
                                                                        .max_w(px(220.))
                                                                        .truncate()
                                                                        .text_xs()
                                                                        .text_color(
                                                                            colors.muted_text,
                                                                        )
                                                                        .child(right),
                                                                )
                                                            }
                                                        },
                                                    );
                                                if enabled {
                                                    row = row
                                                        .hover(|row| {
                                                            row.bg(colors.hovered_surface)
                                                        })
                                                        .on_click(cx.listener(
                                                            move |shell, _, window, cx| {
                                                                shell.activate_command_palette_item(idx, window, cx)
                                                            },
                                                        ));
                                                }
                                                row
                                            })
                                            .collect()
                                    }),
                                )
                                .h(px(palette_height))
                                .w_full()
                                .track_scroll(&self.palette_scroll_handle),
                            )
                        })
                        .into_any_element()
                }
                Modal::DataSearch => {
                    let state = self.data_search_state.clone();
                    let picking_foreign_key = self.foreign_key_pick_target.is_some();
                    let (hits, summary) = match &state {
                        DataSearchState::Ready { response, .. } => (
                            response.hits.clone(),
                            Some(format!(
                                "{} match(es) across {} table(s){}",
                                response.hits.len(),
                                response.tables_searched,
                                if response.truncated { " · capped" } else { "" }
                            )),
                        ),
                        _ => (Vec::new(), None),
                    };
                    let has_hits = !hits.is_empty();
                    div()
                        .flex()
                        .flex_col()
                        .max_h(px(640.))
                        .child(
                            div()
                                .h(px(42.))
                                .px_2()
                                .flex()
                                .items_center()
                                .gap_2()
                                .border_b_1()
                                .border_color(colors.subtle_border)
                                .bg(colors.toolbar)
                                .child(icon(IconName::Search, colors.muted_text, 15.))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .child(self.data_search_input.clone()),
                                )
                                .child(
                                    IconButton::new(
                                        "close-data-search",
                                        IconName::Close,
                                        "Close data search",
                                    )
                                    .square(px(26.))
                                    .icon_size(13.)
                                    .on_click(cx.listener(|shell, _, window, cx| {
                                        shell.dismiss_modal(&DismissModal, window, cx)
                                    })),
                                ),
                        )
                        .when(has_hits, |modal| {
                            modal.child(
                                div()
                                    .id("data-search-results")
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .p_2()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .children(hits.into_iter().enumerate().map(|(index, hit)| {
                                        let pick_hit = hit.clone();
                                        let table = [
                                            hit.table.catalog.as_deref(),
                                            hit.table.schema.as_deref(),
                                            Some(hit.table.name.as_str()),
                                        ]
                                        .into_iter()
                                        .flatten()
                                        .collect::<Vec<_>>()
                                        .join(".");
                                        let matched = hit.matched_columns.clone();
                                        div()
                                            .id(("data-search-hit", index))
                                            .p_2()
                                            .rounded_sm()
                                            .border_1()
                                            .border_color(colors.subtle_border)
                                            .child(
                                                div()
                                                    .mb_1()
                                                    .text_xs()
                                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                                    .text_color(colors.muted_text)
                                                    .child(table),
                                            )
                                            .child(
                                                div()
                                                    .flex()
                                                    .flex_wrap()
                                                    .gap_1()
                                                    .children(hit.columns.into_iter().zip(hit.row.values).map(
                                                        |(column, value)| {
                                                            let is_match = matched.contains(&column);
                                                            div()
                                                                .px_1()
                                                                .rounded_sm()
                                                                .bg(if is_match {
                                                                    colors.active_surface
                                                                } else {
                                                                    colors.surface
                                                                })
                                                                .child(format!(
                                                                    "{column}: {}",
                                                                    render_value(&value).text
                                                                ))
                                                        },
                                                    )),
                                            )
                                            .when(picking_foreign_key, |row| {
                                                row.cursor_pointer()
                                                    .hover(|row| row.bg(colors.hovered_surface))
                                                    .on_click(cx.listener(
                                                        move |shell, _, window, cx| {
                                                            shell.choose_foreign_key_hit(
                                                                pick_hit.clone(),
                                                                window,
                                                                cx,
                                                            )
                                                        },
                                                    ))
                                            })
                                    })),
                            )
                        })
                        .when(!has_hits, |modal| {
                            let (message, color) = match state {
                                DataSearchState::Idle => (
                                    if picking_foreign_key {
                                        "Type to search the referenced key, then select a row"
                                            .into()
                                    } else {
                                        "Type to search text-like columns across loaded tables"
                                            .into()
                                    },
                                    colors.muted_text,
                                ),
                                DataSearchState::Loading => {
                                    ("Searching table data…".into(), colors.muted_text)
                                }
                                DataSearchState::Failed(message) => (message, colors.danger),
                                DataSearchState::Ready { .. } => {
                                    ("No matching rows".into(), colors.muted_text)
                                }
                            };
                            modal.child(
                                div()
                                    .h(px(PALETTE_ROW_HEIGHT * 3.0))
                                    .px_3()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_color(color)
                                    .child(message),
                            )
                        })
                        .child(
                            div()
                                .h(px(28.))
                                .px_2()
                                .flex()
                                .items_center()
                                .justify_between()
                                .border_t_1()
                                .border_color(colors.subtle_border)
                                .text_xs()
                                .text_color(colors.muted_text)
                                .child(summary.unwrap_or_else(|| "Bounded to 20 tables".into()))
                                .child("Esc close"),
                        )
                        .into_any_element()
                }
                Modal::QueryParameters => {
                    let parameter_count = self.parameter_binding_inputs.len();
                    div()
                        .flex()
                        .flex_col()
                        .max_h(px(640.))
                        .child(
                            div()
                                .h(px(42.))
                                .px_3()
                                .flex()
                                .items_center()
                                .justify_between()
                                .border_b_1()
                                .border_color(colors.subtle_border)
                                .bg(colors.toolbar)
                                .child(
                                    div()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child("Query parameters"),
                                )
                                .child(
                                    IconButton::new(
                                        "close-query-parameters",
                                        IconName::Close,
                                        "Cancel query",
                                    )
                                    .square(px(26.))
                                    .icon_size(13.)
                                    .on_click(cx.listener(|shell, _, window, cx| {
                                        shell.dismiss_modal(&DismissModal, window, cx)
                                    })),
                                ),
                        )
                        .child(
                            div()
                                .id("query-parameters-body")
                                .p_3()
                                .flex()
                                .flex_col()
                                .gap_3()
                                .overflow_y_scroll()
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(colors.muted_text)
                                        .child(format!(
                                            "Bind {parameter_count} detected parameter{} before running.",
                                            if parameter_count == 1 { "" } else { "s" }
                                        )),
                                )
                                .children(self.parameter_binding_inputs.iter().map(|binding| {
                                    let kind = match binding.kind {
                                        ParameterBindingKind::Native => "native",
                                        ParameterBindingKind::Template(
                                            sift_protocol::SqlVariableKind::Value,
                                        ) => "value",
                                        ParameterBindingKind::Template(
                                            sift_protocol::SqlVariableKind::Identifier,
                                        ) => "identifier",
                                        ParameterBindingKind::Template(
                                            sift_protocol::SqlVariableKind::List,
                                        ) => "list",
                                    };
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_3()
                                        .child(
                                            div()
                                                .w(px(104.))
                                                .font_family("monospace")
                                                .text_sm()
                                                .text_color(colors.muted_text)
                                                .child(format!("{} · {kind}", binding.label)),
                                        )
                                        .child(div().flex_1().min_w_0().child(binding.input.clone()))
                                }))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(colors.muted_text)
                                        .child("Values use JSON typing; null is SQL NULL. Lists require a non-empty JSON array. Identifiers are dialect-quoted, never raw SQL."),
                                )
                                .children(self.parameter_binding_error.as_ref().map(|error| {
                                    div()
                                        .p_2()
                                        .rounded_sm()
                                        .bg(colors.danger_muted)
                                        .text_sm()
                                        .text_color(colors.danger)
                                        .child(error.clone())
                                })),
                        )
                        .child(
                            div()
                                .px_3()
                                .py_2()
                                .flex()
                                .items_center()
                                .justify_between()
                                .border_t_1()
                                .border_color(colors.subtle_border)
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(colors.muted_text)
                                        .child("Run-prompt values override remembered query bindings"),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            Button::new("cancel-query-parameters", "Cancel")
                                                .tone(ButtonTone::Neutral)
                                                .on_click(cx.listener(|shell, _, window, cx| {
                                                    shell.dismiss_modal(&DismissModal, window, cx)
                                                })),
                                        )
                                        .child(
                                            Button::new("run-with-query-parameters", "Run")
                                                .tone(ButtonTone::Accent)
                                                .on_click(cx.listener(|shell, _, _, cx| {
                                                    shell.submit_query_parameters(cx)
                                                })),
                                        ),
                                ),
                        )
                        .into_any_element()
                }
                Modal::EditResultCell => {
                    if let Some(editor) = self.result_json_edit_editor.clone() {
                        let target = self.result_cell_edit_target.as_ref();
                        div()
                            .key_context("SiftJsonResultEditor")
                            .flex()
                            .flex_col()
                            .h(px(600.))
                            .child(
                                div()
                                    .h(px(42.))
                                    .px_3()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_2()
                                    .border_b_1()
                                    .border_color(colors.subtle_border)
                                    .bg(colors.toolbar)
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child(target.map_or_else(
                                                || "Edit JSON value".into(),
                                                |edit| {
                                                    format!(
                                                        "Edit JSON · {}.{}",
                                                        edit.source.object,
                                                        edit.primary().column
                                                    )
                                                },
                                            )),
                                    )
                                    .child(
                                        IconButton::new(
                                            "close-json-result-cell-edit",
                                            IconName::Close,
                                            "Cancel JSON edit",
                                        )
                                        .square(px(26.))
                                        .icon_size(13.)
                                        .on_click(cx.listener(|shell, _, window, cx| {
                                            shell.dismiss_modal(&DismissModal, window, cx)
                                        })),
                                    ),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_h_0()
                                    .flex()
                                    .flex_col()
                                    .border_b_1()
                                    .border_color(colors.subtle_border)
                                    .child(div().flex_1().min_h_0().child(editor))
                                    .children(self.result_edit_error.as_ref().map(|message| {
                                        div()
                                            .px_3()
                                            .py_2()
                                            .child(ErrorBanner::new(message.clone()))
                                    })),
                            )
                            .child(
                                div()
                                    .px_3()
                                    .py_2()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(colors.muted_text)
                                            .child("Formatted JSON · Vim editing · validation on stage"),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(
                                                Button::new("cancel-json-result-edit", "Cancel")
                                                    .tone(ButtonTone::Neutral)
                                                    .key_binding("Esc")
                                                    .on_click(cx.listener(
                                                        |shell, _, window, cx| {
                                                            shell.dismiss_modal(
                                                                &DismissModal,
                                                                window,
                                                                cx,
                                                            )
                                                        },
                                                    )),
                                            )
                                            .child(
                                                Button::new(
                                                    "stage-json-result-edit",
                                                    "Save changes",
                                                )
                                                .tone(ButtonTone::Accent)
                                                .key_binding(":w")
                                                .on_click(cx.listener(
                                                    |shell, _, window, cx| {
                                                        shell.stage_json_result_edit(
                                                            &StageJsonResultEdit,
                                                            window,
                                                            cx,
                                                        )
                                                    },
                                                )),
                                            ),
                                    ),
                            )
                            .into_any_element()
                    } else {
                    let target = self.result_cell_edit_target.as_ref();
                    let staged_edits = self.staged_result_edits.clone();
                    let staged_deletes = self.staged_result_deletes.clone();
                    let pending = self.result_edit_pending;
                    let plan = self.result_edit_plan.as_ref();
                    let can_apply = !staged_edits.is_empty() && !pending && plan.is_some();
                    div()
                        .on_key_down(cx.listener(
                            |shell, event: &gpui::KeyDownEvent, window, cx| {
                                if event.keystroke.key == "escape" {
                                    shell.dismiss_modal(&DismissModal, window, cx);
                                    cx.stop_propagation();
                                }
                            },
                        ))
                        .flex()
                        .flex_col()
                        .max_h(px(640.))
                        .child(
                            div()
                                .h(px(42.))
                                .px_3()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_2()
                                .border_b_1()
                                .border_color(colors.subtle_border)
                                .bg(colors.toolbar)
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child(target.map_or_else(
                                            || "Review staged table changes".into(),
                                            |edit| {
                                                if edit.cells.len() == 1 {
                                                    format!(
                                                        "Edit {}.{}",
                                                        edit.source.object,
                                                        edit.primary().column
                                                    )
                                                } else {
                                                    format!(
                                                        "Edit {} selected cells · {}",
                                                        edit.cells.len(),
                                                        edit.source.object
                                                    )
                                                }
                                            },
                                        )),
                                )
                                .child(
                                    IconButton::new(
                                        "close-result-cell-edit",
                                        IconName::Close,
                                        "Keep staged changes and close review",
                                    )
                                    .square(px(26.))
                                    .icon_size(13.)
                                    .on_click(cx.listener(|shell, _, window, cx| {
                                        shell.dismiss_modal(&DismissModal, window, cx)
                                    })),
                                ),
                        )
                        .child(
                            div()
                                .id("result-cell-edit-body")
                                .p_3()
                                .flex()
                                .flex_col()
                                .gap_3()
                                .overflow_y_scroll()
                                .children(target.map(|edit| {
                                    div()
                                        .text_xs()
                                        .text_color(colors.muted_text)
                                        .child(format!(
                                            "Original: {} · Enter NULL to store a null value",
                                            render_value(&edit.primary().original).text
                                        ))
                                }))
                                .children((!staged_edits.is_empty() || !staged_deletes.is_empty()).then(|| {
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .child(
                                            div()
                                                .text_xs()
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .text_color(colors.muted_text)
                                                .child(format!(
                                                    "Staged changes ({})",
                                                    staged_edits.len() + staged_deletes.len()
                                                )),
                                        )
                                        .children(staged_edits.iter().enumerate().map(
                                            |(index, edit)| {
                                                let row_index = staged_result_row_index(
                                                    &staged_edits,
                                                    index,
                                                );
                                                let conflict = self
                                                    .result_edit_conflicts
                                                    .get(&row_index)
                                                    .cloned();
                                                div()
                                                    .id(("staged-result-edit", index))
                                                    .p_2()
                                                    .flex()
                                                    .flex_col()
                                                    .gap_1()
                                                    .rounded_sm()
                                                    .border_1()
                                                    .border_color(if conflict.is_some() {
                                                        colors.danger
                                                    } else {
                                                        colors.subtle_border
                                                    })
                                                    .child(
                                                        div()
                                                            .flex()
                                                            .items_center()
                                                            .justify_between()
                                                            .gap_2()
                                                            .child(
                                                                div()
                                                                    .min_w_0()
                                                                    .whitespace_normal()
                                                                    .text_sm()
                                                                    .child(format!(
                                                                        "Row {} · {}\nBefore: {}\nAfter: {}",
                                                                        row_index + 1,
                                                                        edit.column,
                                                                        render_value(&edit.original).text,
                                                                        render_value(&edit.value).text,
                                                                    )),
                                                            )
                                                            .child(
                                                                Button::new(
                                                                    ("revert-result-edit", index),
                                                                    "Revert",
                                                                )
                                                                .tone(ButtonTone::Ghost)
                                                                .on_click(cx.listener(
                                                                    move |shell, _, _, cx| {
                                                                        shell.revert_staged_result_edit(
                                                                            index, cx,
                                                                        )
                                                                    },
                                                                )),
                                                            ),
                                                    )
                                                    .children(conflict.map(|message| {
                                                        div()
                                                            .text_xs()
                                                            .text_color(colors.danger)
                                                            .whitespace_normal()
                                                            .child(message)
                                                    }))
                                            },
                                        ))
                                        .children(staged_deletes.iter().enumerate().map(
                                            |(index, delete)| {
                                                let identity = delete
                                                    .original_row
                                                    .iter()
                                                    .take(3)
                                                    .map(|(column, value)| format!(
                                                        "{column}={}",
                                                        render_value(value).text
                                                    ))
                                                    .collect::<Vec<_>>()
                                                    .join(", ");
                                                div()
                                                    .id(("staged-result-delete", index))
                                                    .p_2()
                                                    .rounded_sm()
                                                    .border_1()
                                                    .border_color(colors.danger)
                                                    .text_sm()
                                                    .text_color(colors.danger)
                                                    .child(format!("Delete row · {identity}"))
                                            },
                                        ))
                                }))
                                .children(plan.map(|plan| {
                                    let identity = match &plan.identity {
                                        sift_protocol::IdentitySource::PrimaryKey { columns } => {
                                            format!("Primary key: {}", columns.join(", "))
                                        }
                                        sift_protocol::IdentitySource::UniqueIndex {
                                            name,
                                            columns,
                                        } => format!(
                                            "Unique index {name}: {}",
                                            columns.join(", ")
                                        ),
                                    };
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_2()
                                        .child(
                                            div()
                                                .text_xs()
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .text_color(colors.muted_text)
                                                .child(format!("Preview · {identity}")),
                                        )
                                        .children(plan.statements.iter().enumerate().map(
                                            |(index, statement)| {
                                                div()
                                                    .id(("result-edit-statement", index))
                                                    .debug_selector(move || {
                                                        format!("result-edit-statement-{index}")
                                                    })
                                                    .p_2()
                                                    .rounded_sm()
                                                    .border_1()
                                                    .border_color(colors.subtle_border)
                                                    .bg(colors.surface)
                                                    .font_family("monospace")
                                                    .text_xs()
                                                    .whitespace_normal()
                                                    .child(statement.sql.clone())
                                            },
                                        ))
                                }))
                                .children(
                                    self.result_edit_error
                                        .as_ref()
                                        .map(|message| ErrorBanner::new(message.clone())),
                                ),
                        )
                        .child(
                            div()
                                .px_3()
                                .py_2()
                                .flex()
                                .items_center()
                                .justify_end()
                                .gap_2()
                                .border_t_1()
                                .border_color(colors.subtle_border)
                                .child(
                                    Button::new("cancel-result-cell-edit", "Keep staged")
                                        .tone(ButtonTone::Neutral)
                                        .key_binding("Esc")
                                        .on_click(cx.listener(|shell, _, window, cx| {
                                            shell.dismiss_modal(&DismissModal, window, cx)
                                        })),
                                )
                                .child(
                                    Button::new("discard-result-cell-edits", "Discard all")
                                        .tone(ButtonTone::DangerGhost)
                                        .disabled(pending)
                                        .on_click(cx.listener(|shell, _, window, cx| {
                                            shell.discard_staged_result_edits(window, cx)
                                        })),
                                )
                                .child(
                                    Button::new(
                                        "preview-result-cell-edit",
                                        if plan.is_some() { "Refresh SQL" } else { "View SQL" },
                                    )
                                        .tone(ButtonTone::Neutral)
                                        .loading(pending && plan.is_none())
                                        .disabled(pending)
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.preview_result_cell_edit(cx)
                                        })),
                                )
                                .child(
                                    Button::new(
                                        "apply-result-cell-edit",
                                        "Apply staged changes",
                                    )
                                        .debug_selector("apply-result-cell-edit")
                                        .tone(ButtonTone::Accent)
                                        .key_binding("Enter")
                                        .loading(pending && plan.is_some())
                                        .disabled(!can_apply)
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.apply_result_cell_edit(cx)
                                        })),
                                ),
                        )
                        .into_any_element()
                    }
                }
                Modal::PlanCaptures => {
                    let captures = self.plan_captures.clone();
                    let comparison = self.plan_capture_comparison.as_ref();
                    let selected = self.selected_plan_captures.clone();
                    div().flex().flex_col().max_h(px(640.)).child(
                        div().h(px(42.)).px_3().flex().items_center().justify_between()
                            .border_b_1().border_color(colors.subtle_border).bg(colors.toolbar)
                            .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child("Plan captures"))
                            .child(Button::new("compare-selected-plans", "Compare selected")
                                .tone(ButtonTone::Accent).disabled(selected.len() != 2)
                                .on_click(cx.listener(|shell, _, _, cx| {
                                    let (Some(item_id), Some(left), Some(right)) = (shell.plan_capture_item, shell.selected_plan_captures.first().copied(), shell.selected_plan_captures.get(1).copied()) else { return };
                                    let Some(source) = shell.database_source(item_id, cx) else { return };
                                    if let Some(sender) = &shell.executor_sender {
                                        let _ = sender.send(ExecutorCommand::ComparePlanCaptures { item_id, tenant_id: source.tenant_id, left, right });
                                    }
                                }))),
                    ).child(
                        div().id("plan-capture-list").p_3().flex().flex_col().gap_2().overflow_y_scroll()
                            .children(captures.into_iter().enumerate().map(|(index, capture)| {
                                let capture_id = capture.id;
                                let tenant_id = capture.tenant_id;
                                let expected_revision = capture.revision;
                                let is_selected = selected.contains(&capture_id);
                                div().id(("plan-capture", index)).p_2().rounded_sm().border_1()
                                    .cursor(CursorStyle::PointingHand)
                                    .border_color(if is_selected { colors.accent } else { colors.subtle_border })
                                    .when(is_selected, |row| row.bg(colors.active_surface))
                                    .on_click(cx.listener(move |shell, _, _, cx| {
                                        shell.toggle_plan_capture_selection(capture_id, cx)
                                    }))
                                    .child(div().flex().items_center().justify_between().gap_2()
                                        .child(format!("{} · {} · {} ms", capture.root_operator, if capture.analyzed { "analyzed" } else { "estimated" }, capture.duration_ms))
                                        .child(Button::new(("delete-plan-capture", index), "Delete")
                                            .tone(ButtonTone::DangerMuted)
                                            .on_click(cx.listener(move |shell, _, _, cx| {
                                                cx.stop_propagation();
                                                let Some(item_id) = shell.plan_capture_item else { return };
                                                if let Some(sender) = &shell.executor_sender {
                                                    let _ = sender.send(ExecutorCommand::DeletePlanCapture {
                                                        item_id,
                                                        tenant_id,
                                                        capture_id,
                                                        expected_revision,
                                                    });
                                                }
                                            }))))
                                    .child(div().flex().items_center().justify_between().text_xs().text_color(colors.muted_text)
                                        .child(capture.captured_at.to_rfc3339())
                                        .child(if is_selected { "Selected" } else { "Select" }))
                            }))
                            .children(comparison.map(|comparison| div().p_2().rounded_sm().bg(colors.active_surface).child(format!(
                                "Comparison: {} operator · {} cardinality · {} cost · {} runtime changes{}",
                                comparison.operator_changes, comparison.cardinality_changes, comparison.cost_changes, comparison.runtime_changes,
                                if comparison.truncated { " · truncated" } else { "" }
                            ))))
                            .children(self.plan_capture_error.as_ref().map(|message| ErrorBanner::new(message.clone()))),
                    ).into_any_element()
                }
                Modal::DataResults(item_id) => match self.data_results_modal_target(*item_id, cx) {
                    Some((title, results)) => div()
                        .size_full()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .h(px(36.))
                                .flex_none()
                                .px_2()
                                .flex()
                                .items_center()
                                .gap_2()
                                .border_b_1()
                                .border_color(colors.subtle_border)
                                .bg(colors.toolbar)
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child(format!("Data · {title}")),
                                )
                                .child(
                                    div()
                                        .debug_selector(|| "close-data-results-modal".into())
                                        .child(
                                            IconButton::new(
                                                "close-data-results-modal",
                                                IconName::Close,
                                                "Close large Data view",
                                            )
                                            .square(px(26.))
                                            .icon_size(13.)
                                            .on_click(cx.listener(|shell, _, window, cx| {
                                                shell.dismiss_modal(&DismissModal, window, cx)
                                            })),
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .debug_selector(|| "data-results-modal-body".into())
                                .flex()
                                .flex_1()
                                .min_h_0()
                                .min_w_0()
                                .overflow_hidden()
                                .child(results),
                        )
                        .into_any_element(),
                    None => div()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(colors.muted_text)
                        .child("These query results are no longer available")
                        .into_any_element(),
                },
                Modal::ServerPicker => {
                    let current_id = self
                        .lifecycle
                        .selected_instance
                        .as_ref()
                        .map(|instance| instance.id.clone());
                    let pending = self.server_connection_pending;
                    let local_active = current_id.as_deref() == Some("local");
                    let status_label = self.lifecycle.status_label();
                    // One row shape for every switchable target: leading icon
                    // chip, title + subtitle, and a current-state slot.
                    let picker_row = |row: gpui::Stateful<gpui::Div>,
                                      name: SharedString,
                                      subtitle: SharedString,
                                      current: bool| {
                        row.flex()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .py_1()
                            .min_h(px(38.))
                            .rounded_sm()
                            .child(
                                div()
                                    .flex_none()
                                    .size(px(22.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_sm()
                                    .bg(colors.active_surface)
                                    .child(icon(
                                        if current {
                                            IconName::Check
                                        } else {
                                            IconName::Server
                                        },
                                        if current {
                                            colors.accent
                                        } else {
                                            colors.muted_text
                                        },
                                        12.,
                                    )),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_1()
                                    .flex_col()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_sm()
                                            .child(name),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_xs()
                                            .text_color(colors.muted_text)
                                            .child(subtitle),
                                    ),
                            )
                    };
                    let mut rows = Vec::new();
                    rows.push(
                        picker_row(
                            div()
                                .id("picker-local-sift")
                                .role(Role::Button)
                                .aria_label("Bundled Local Sift")
                                .when(local_active, |row| row.bg(colors.active_surface))
                                .when(!pending && !local_active, |row| {
                                    row.hover(|row| row.bg(colors.hovered_surface)).on_click(
                                        cx.listener(|shell, _, _, cx| {
                                            shell.use_local_server(cx)
                                        }),
                                    )
                                }),
                            "Bundled Local Sift".into(),
                            "Built into this app · no TOML".into(),
                            local_active,
                        )
                        .child(
                            Badge::new(if local_active { "Current" } else { "Built-in" })
                                .tone(if local_active {
                                    Tone::Success
                                } else {
                                    Tone::Neutral
                                }),
                        )
                        .into_any_element(),
                    );
                    for (index, profile) in self.saved_servers.iter().cloned().enumerate() {
                        let active = current_id.as_deref()
                            == Some(format!("hosted:{}", profile.id).as_str());
                        let profile_for_click = profile.clone();
                        let profile_for_edit = profile.clone();
                        rows.push(
                            picker_row(
                                div()
                                    .id(("picker-saved-server", index))
                                    .role(Role::Button)
                                    .aria_label(profile.name.clone())
                                    .when(active, |row| row.bg(colors.active_surface))
                                    .when(!pending && !active, |row| {
                                        row.hover(|row| row.bg(colors.hovered_surface)).on_click(
                                            cx.listener(move |shell, _, _, cx| {
                                                shell.connect_saved_server(
                                                    &profile_for_click,
                                                    cx,
                                                )
                                            }),
                                        )
                                    }),
                                profile.name.clone().into(),
                                profile.base_url.clone().into(),
                                active,
                            )
                            .child(if profile.has_saved_token {
                                Badge::new("Token saved").tone(Tone::Neutral)
                            } else {
                                Badge::new("Saved").tone(Tone::Neutral)
                            })
                            .child(
                                IconButton::new(
                                    ("edit-saved-server", index),
                                    IconName::Edit,
                                    format!("Edit {}", profile.name),
                                )
                                .tooltip(format!("Edit {}", profile.name))
                                .on_click(cx.listener(move |shell, _, window, cx| {
                                    cx.stop_propagation();
                                    shell.open_server_connection(
                                        &OpenServerConnection,
                                        window,
                                        cx,
                                    );
                                    shell.select_server_profile(
                                        &profile_for_edit,
                                        window,
                                        cx,
                                    );
                                })),
                            )
                            .into_any_element(),
                        );
                    }
                    for (index, instance) in self.instance_roots.iter().cloned().enumerate() {
                        let root = instance.root.clone();
                        let root_for_edit = instance.root.clone();
                        let root_for_remove = instance.root.clone();
                        let active = current_id.as_deref()
                            == Some(format!("config:{}", instance.manifest_id).as_str());
                        rows.push(
                            picker_row(
                                div()
                                    .id(("picker-instance-root", index))
                                    .role(Role::Button)
                                    .aria_label(instance.name.clone())
                                    .when(active, |row| row.bg(colors.active_surface))
                                    .when(!pending && !active, |row| {
                                        row.hover(|row| row.bg(colors.hovered_surface)).on_click(
                                            cx.listener(move |shell, _, _, cx| {
                                                shell.connect_instance_root(root.clone(), cx)
                                            }),
                                        )
                                    }),
                                instance.name.clone().into(),
                                instance.root.display().to_string().into(),
                                active,
                            )
                            .child(
                                Badge::new(if active { "Current" } else { "sift.toml" })
                                    .tone(if active {
                                        Tone::Success
                                    } else {
                                        Tone::Neutral
                                    }),
                            )
                            .child(
                                div()
                                    .id(("edit-instance-root", index))
                                    .role(Role::Button)
                                    .aria_label(format!("Edit {} sift.toml", instance.name))
                                    .flex_none()
                                    .p_1()
                                    .rounded_sm()
                                    .text_color(colors.muted_text)
                                    .hover(|button| {
                                        button.bg(colors.hovered_surface).text_color(colors.text)
                                    })
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        |_, _, cx| cx.stop_propagation(),
                                    )
                                    .on_click(cx.listener(move |shell, _, _, cx| {
                                        cx.stop_propagation();
                                        shell.open_root_configuration(root_for_edit.clone(), cx)
                                    }))
                                    .child(icon(IconName::Edit, colors.muted_text, 12.)),
                            )
                            .child(
                                div()
                                    .id(("forget-instance-root", index))
                                    .role(Role::Button)
                                    .aria_label(format!(
                                        "Remove {} from Sift; keep files",
                                        instance.name
                                    ))
                                    .flex_none()
                                    .p_1()
                                    .rounded_sm()
                                    .text_color(colors.muted_text)
                                    .hover(|button| {
                                        button
                                            .bg(colors.danger_muted)
                                            .text_color(colors.danger)
                                    })
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        |_, _, cx| cx.stop_propagation(),
                                    )
                                    .on_click(cx.listener(move |shell, _, _, cx| {
                                        cx.stop_propagation();
                                        shell.forget_instance_root(
                                            root_for_remove.clone(),
                                            cx,
                                        )
                                    }))
                                    .child(icon(IconName::Close, colors.danger, 12.)),
                            )
                            .into_any_element(),
                        );
                    }
                    div()
                        .id("server-picker-menu")
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .child(
                            div()
                                .h(px(36.))
                                .px_2()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_2()
                                .border_b_1()
                                .border_color(colors.subtle_border)
                                .bg(colors.toolbar)
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_sm()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child("Switch Sift server"),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .max_w(px(150.))
                                        .truncate()
                                        .text_xs()
                                        .text_color(colors.muted_text)
                                        .child(format!(
                                            "{} · {}",
                                            self.active_server_name(),
                                            status_label
                                        )),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_0p5()
                                .p_1()
                                .children(rows),
                        )
                        .children(self.server_connection_error.as_ref().map(|message| {
                            ErrorBanner::new(message.clone())
                        }))
                        .when(pending, |picker| {
                            picker.child(
                                div()
                                    .px_2()
                                    .py_1()
                                    .text_xs()
                                    .text_color(colors.muted_text)
                                    .child("Testing connection…"),
                            )
                        })
                        .child(
                            div()
                                .p_1()
                                .border_t_1()
                                .border_color(colors.subtle_border)
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_1()
                                .bg(colors.toolbar)
                                .child(
                                    IconButton::new(
                                        "picker-new-instance",
                                        IconName::Add,
                                        "Create Sift Instance",
                                    )
                                        .text("Create")
                                        .tooltip("Create Sift Instance")
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.prompt_for_new_instance_root(cx)
                                        })),
                                )
                                .child(
                                    IconButton::new(
                                        "picker-import-instance",
                                        IconName::Workspace,
                                        "Open Existing Sift Instance",
                                    )
                                    .text("Open")
                                    .tooltip("Open Existing Sift Instance")
                                    .on_click(cx.listener(|shell, _, _, cx| {
                                        shell.prompt_for_instance_root(cx)
                                    })),
                                )
                                .child(
                                    IconButton::new(
                                        "picker-add-server",
                                        IconName::Server,
                                        "Add remote Sift server",
                                    )
                                    .text("Add server")
                                    .tooltip("Add remote Sift server")
                                    .on_click(cx.listener(|shell, _, window, cx| {
                                        shell.open_server_connection(
                                            &OpenServerConnection,
                                            window,
                                            cx,
                                        );
                                        shell.new_server_profile(window, cx);
                                    })),
                                ),
                        )
                        .into_any_element()
                }
                Modal::InstanceSetup => {
                    let Some(plan) = self.instance_plan.clone() else {
                        unreachable!("instance setup opens only after a plan is loaded")
                    };
                    let pending = self.instance_operation_pending;
                    let selected_slot = self.selected_instance_credential.clone();
                    let missing_credentials = plan
                        .credentials
                        .iter()
                        .filter(|credential| credential.readiness != "ready")
                        .count();
                    let ready_to_start = plan.current_generation.is_some()
                        && !plan.drifted
                        && missing_credentials == 0;
                    let allow_destroy = plan.destroy_confirmation_required;
                    let manifest_path = plan.root.join("sift.toml");
                    let edit_manifest_root = plan.root.clone();
                    let lock_source = plan.lock.clone();
                    let refresh_root = plan.root.clone();
                    let configuration_digest = plan.configuration_digest.chars().take(12).collect::<String>();
                    let lock_digest = plan.lock_digest.chars().take(12).collect::<String>();
                    let credential_rows = plan
                        .credentials
                        .iter()
                        .cloned()
                        .enumerate()
                        .map(|(credential_index, credential)| {
                        let slot = credential.slot.clone();
                        let selected = selected_slot.as_deref() == Some(slot.as_str());
                        div()
                            .id(("instance-credential", credential_index))
                            .role(Role::Button)
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .when(selected, |row| row.bg(colors.active_surface))
                            .hover(|row| row.bg(colors.hovered_surface))
                            .on_click(cx.listener(move |shell, _, window, cx| {
                                shell.select_instance_credential(slot.clone(), window, cx);
                            }))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_3()
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .min_w_0()
                                            .child(credential.kind.label())
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(colors.muted_text)
                                                    .truncate()
                                                    .child(format!(
                                                        "{} · {}",
                                                        credential.slot, credential.consumer
                                                    )),
                                            ),
                                    )
                                    .child({
                                        let readiness = credential.readiness.clone();
                                        Badge::new(readiness).tone(
                                            if credential.readiness == "ready" {
                                                Tone::Success
                                            } else {
                                                Tone::Warning
                                            },
                                        )
                                    }),
                            )
                    });
                    let changed_resources = plan
                        .resource_changes
                        .iter()
                        .filter(|resource| resource.action != "unchanged")
                        .count();
                    let resource_rows = plan.resource_changes.iter().cloned().enumerate().map(
                        |(index, resource)| {
                            let tone = match resource.action.as_str() {
                                "create" => Tone::Success,
                                "delete" => Tone::Danger,
                                "update" => Tone::Warning,
                                _ => Tone::Neutral,
                            };
                            div()
                                .id(("instance-resource-change", index))
                                .min_h(px(32.))
                                .py_1()
                                .flex()
                                .flex_col()
                                .border_b_1()
                                .border_color(colors.subtle_border)
                                .child(
                                    div().h(px(28.)).px_2().flex().items_center().gap_2()
                                        .child(Badge::new(resource.action).tone(tone))
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .truncate()
                                                .text_xs()
                                                .font_family("monospace")
                                                .child(resource.address),
                                        )
                                        .children(resource.prevent_destroy.then(|| {
                                            Badge::new("protected").tone(Tone::Warning)
                                        })),
                                )
                                .children(resource.fields.into_iter().map(|field| {
                                    let before = field.before.unwrap_or_else(|| "∅".into());
                                    let after = field.after.unwrap_or_else(|| "∅".into());
                                    div().min_h(px(24.)).pl_8().pr_2().flex().items_center().gap_2()
                                        .text_xs().font_family("monospace")
                                        .child(div().w(px(150.)).truncate().text_color(colors.muted_text).child(field.path))
                                        .child(div().min_w_0().flex_1().truncate().child(format!("{before} → {after}")))
                                }))
                        },
                    );
                    let generation_rows = plan.generations.iter().cloned().enumerate().map(
                        |(index, generation)| {
                            let generation_number = generation.generation;
                            div().id(("instance-generation", index)).min_h(px(32.)).px_2()
                                .flex().items_center().gap_2().border_b_1().border_color(colors.subtle_border)
                                .child(Badge::new(format!("#{}", generation.generation)).tone(if generation.apply_status.as_deref() == Some("failed") { Tone::Danger } else if generation.current { Tone::Success } else { Tone::Neutral }))
                                .child(div().min_w_0().flex_1().truncate().text_xs().child(format!("{} · {}{}", generation.created_at, generation.configuration_digest.chars().take(15).collect::<String>(), generation.apply_message.as_deref().map(|message| format!(" · {message}")).unwrap_or_default())))
                                .child(Button::new(("review-generation", index), if generation.current { "Current" } else { "Restore for review" })
                                    .tone(ButtonTone::Ghost)
                                    .disabled(generation.current || self.instance_operation_pending)
                                    .on_click(cx.listener(move |shell, _, _, cx| shell.review_generation_rollback(generation_number, cx))))
                        },
                    );

                    div()
                        .id("instance-setup-scroll")
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .max_h(px(680.))
                        .overflow_y_scroll()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_3()
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .min_w_0()
                                        .child(
                                            div()
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .child(plan.name.clone()),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(colors.muted_text)
                                                .truncate()
                                                .child(plan.root.display().to_string()),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(colors.muted_text)
                                                .child(format!(
                                                    "Config {configuration_digest} · Lock {lock_digest}"
                                                )),
                                        ),
                                )
                                .child(
                                    Badge::new(if plan.drifted {
                                        "Unapplied drift"
                                    } else {
                                        "Applied"
                                    })
                                    .tone(if plan.drifted {
                                        Tone::Warning
                                    } else {
                                        Tone::Success
                                    }),
                                ),
                        )
                        .child(
                            div()
                                .rounded_sm()
                                .border_1()
                                .border_color(colors.subtle_border)
                                .p_2()
                                .grid()
                                .grid_cols(2)
                                .gap_2()
                                .text_sm()
                                .child(format!("Deployment: {}", plan.deployment))
                                .child(format!("Bind: {}", plan.bind))
                                .child(format!(
                                    "Generation: {}",
                                    plan.current_generation.map_or_else(
                                        || "not applied".into(),
                                        |generation| generation.to_string()
                                    )
                                ))
                                .child(format!("Generations: {}", plan.generation_count))
                                .child(format!("Principals: {}", plan.principals))
                                .child(format!("Tenants: {}", plan.tenants))
                                .child(format!("Memberships: {}", plan.memberships))
                                .child(format!("Connections: {}", plan.connections))
                                .child(format!("Extensions: {}", plan.extensions)),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child("RESOURCE PLAN"),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(if changed_resources == 0 {
                                            colors.muted_text
                                        } else {
                                            colors.warning
                                        })
                                        .child(format!("{changed_resources} change(s)")),
                                ),
                        )
                        .child(
                            div()
                                .id("instance-resource-plan")
                                .max_h(px(190.))
                                .overflow_y_scroll()
                                .rounded_sm()
                                .border_1()
                                .border_color(colors.subtle_border)
                                .children(resource_rows),
                        )
                        .children((!plan.generations.is_empty()).then(|| {
                            div().flex().flex_col().gap_1()
                                .child(div().text_xs().font_weight(gpui::FontWeight::SEMIBOLD).child("GENERATIONS"))
                                .child(div().id("instance-generation-history").max_h(px(128.)).overflow_y_scroll().rounded_sm().border_1().border_color(colors.subtle_border).children(generation_rows))
                        }))
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(
                                    Button::new(
                                        "edit-instance-manifest",
                                        "Edit sift.toml",
                                    )
                                    .tone(ButtonTone::Accent)
                                    .on_click(cx.listener(move |shell, _, _, cx| {
                                        shell.open_root_configuration(
                                            edit_manifest_root.clone(),
                                            cx,
                                        )
                                    })),
                                )
                                .child(
                                    Button::new(
                                        "open-instance-manifest",
                                        "Open externally",
                                    )
                                    .tone(ButtonTone::Neutral)
                                    .on_click(move |_, _, cx| {
                                        cx.open_with_system(&manifest_path)
                                    }),
                                )
                                .child(
                                    Button::new(
                                        "open-instance-lock",
                                        "Inspect sift.lock",
                                    )
                                    .tone(ButtonTone::Neutral)
                                    .on_click(cx.listener(move |shell, _, _, cx| {
                                        shell.open_instance_lock(lock_source.clone(), cx)
                                    })),
                                )
                                .child(
                                    Button::new(
                                        "refresh-instance-plan",
                                        "Refresh plan",
                                    )
                                    .tone(ButtonTone::Neutral)
                                    .on_click(cx.listener(move |shell, _, _, cx| {
                                        shell.inspect_instance_root(refresh_root.clone(), cx)
                                    })),
                                ),
                        )
                        .children((!plan.warnings.is_empty()).then(|| {
                            div()
                                .p_2()
                                .rounded_sm()
                                .bg(colors.warning_muted)
                                .text_color(colors.warning)
                                .children(plan.warnings.iter().cloned())
                        }))
                        .children(plan.last_apply.clone().map(|summary| {
                            let failed = summary.starts_with("Apply failed") || summary.starts_with("Apply blocked");
                            div()
                                .p_2()
                                .rounded_sm()
                                .bg(if failed { colors.danger_muted } else { colors.active_surface })
                                .text_color(if failed { colors.danger } else { colors.text })
                                .text_sm()
                                .child(summary)
                        }))
                        .when(!plan.credentials.is_empty(), |view| {
                            view.child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("CREDENTIAL SLOTS"),
                            )
                            .child(div().flex().flex_col().gap_1().children(credential_rows))
                        })
                        .children(selected_slot.and_then(|slot| plan.credentials.iter().find(|credential| credential.slot == slot).cloned()).map(|credential| {
                            div().p_2().rounded_sm().border_1().border_color(colors.subtle_border).bg(colors.surface).flex().flex_col().gap_2()
                                .child(div().text_sm().font_weight(gpui::FontWeight::SEMIBOLD).child(format!("Provide {}", credential.kind.label())))
                                .child(div().text_xs().text_color(colors.muted_text).child(format!("Used by {} · stored in the destination secret backend, never sift.toml or SQLite", credential.consumer)))
                                .child(
                                    div().flex().gap_2()
                                        .child(div().flex_1().min_w_0().rounded_sm().border_1().border_color(colors.subtle_border).child(self.instance_secret_input.clone()))
                                        .child(Button::new("import-instance-credential", "Store securely")
                                            .tone(ButtonTone::Accent)
                                            .loading(pending)
                                            .on_click(cx.listener(|shell, _, _, cx| shell.import_instance_credential(cx)))),
                                )
                        }))
                        .children(self.instance_operation_error.as_ref().map(|error| {
                            ErrorBanner::new(error.clone())
                        }))
                        .when(pending, |view| {
                            view.child(
                                div()
                                    .text_xs()
                                    .text_color(colors.muted_text)
                                    .child("Working…"),
                            )
                        })
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new(
                                        "apply-instance-plan",
                                        if plan.destroy_confirmation_required {
                                            "Apply destructive changes"
                                        } else {
                                            "Apply"
                                        },
                                    )
                                    .tone(if plan.destroy_confirmation_required {
                                        ButtonTone::Danger
                                    } else {
                                        ButtonTone::Accent
                                    })
                                    .loading(pending)
                                    .on_click(cx.listener(move |shell, _, _, cx| {
                                        shell.apply_instance_root(allow_destroy, cx)
                                    })),
                                )
                                .child(
                                    Button::new("start-instance-root", "Start & Connect")
                                        .tone(ButtonTone::Success)
                                        .disabled(!ready_to_start)
                                        .loading(pending)
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.start_instance_root(cx)
                                        })),
                                ),
                        )
                        .into_any_element()
                }
                Modal::ServerConnection => {
                    let profiles = self.saved_servers.clone();
                    let selected = self.selected_server_profile.clone();
                    let pending = self.server_connection_pending;
                    let remember = self.remember_server_token;
                    let ssh = self.server_connection_ssh;
                    let mut saved_rows = Vec::new();
                    for (profile_index, profile) in profiles.into_iter().enumerate() {
                        let active = selected.as_deref() == Some(profile.id.as_str());
                        let profile_for_click = profile.clone();
                        saved_rows.push(
                            div()
                                .id(("saved-server", profile_index))
                                .flex()
                                .items_center()
                                .justify_between()
                                .px_2()
                                .py_1()
                                .rounded_sm()
                                .when(active, |row| row.bg(colors.active_surface))
                                .hover(|row| row.bg(colors.hovered_surface))
                                .on_click(cx.listener(move |shell, _, window, cx| {
                                    shell.select_server_profile(&profile_for_click, window, cx)
                                }))
                                .child(
                                    div()
                                        .flex()
                                        .flex_1()
                                        .flex_col()
                                        .min_w_0()
                                        .child(
                                            div()
                                                .min_w_0()
                                                .truncate()
                                                .child(profile.name.clone()),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(colors.muted_text)
                                                .truncate()
                                                .child(if profile.kind == SavedServerKind::Ssh {
                                                    format!("SSH · {}", profile.base_url)
                                                } else {
                                                    profile.base_url.clone()
                                                }),
                                        ),
                                )
                                .when(profile.has_saved_token, |row| {
                                    row.child(Badge::new("Token saved").tone(Tone::Success))
                                })
                                .into_any_element(),
                        );
                    }
                    div()
                        .id("server-connection-scroll")
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .max_h(px(620.))
                        .overflow_y_scroll()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .min_w_0()
                                .items_center()
                                .justify_between()
                                .gap_2()
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child("Connect to Sift Server"),
                                )
                                .child(
                                    Button::new("new-server-profile", "New Server")
                                        .tone(ButtonTone::Ghost)
                                        .start_icon(IconName::Add)
                                        .on_click(cx.listener(|shell, _, window, cx| {
                                            shell.new_server_profile(window, cx)
                                        })),
                                ),
                        )
                        .child(
                            div()
                                .id("use-local-sift")
                                .role(Role::Button)
                                .h(cx.theme().metrics.row_height)
                                .flex()
                                .items_center()
                                .justify_between()
                                .px_2()
                                .rounded_sm()
                                .hover(|row| row.bg(colors.hovered_surface))
                                .on_click(cx.listener(|shell, _, _, cx| shell.use_local_server(cx)))
                                .child(
                                    div()
                                        .flex()
                                        .flex_1()
                                        .min_w_0()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            div().min_w_0().truncate().child("Local Sift"),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .text_xs()
                                        .text_color(colors.muted_text)
                                        .child("Bundled server"),
                                ),
                        )
                        .when(!saved_rows.is_empty(), |form| {
                            form.child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(colors.muted_text)
                                            .child("SAVED SERVERS"),
                                    )
                                    .children(saved_rows),
                            )
                        })
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .child(
                                    Button::new("server-mode-hosted", "URL")
                                        .tone(if ssh {
                                            ButtonTone::Ghost
                                        } else {
                                            ButtonTone::Neutral
                                        })
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.server_connection_ssh = false;
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new("server-mode-ssh", "SSH")
                                        .tone(if ssh {
                                            ButtonTone::Neutral
                                        } else {
                                            ButtonTone::Ghost
                                        })
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.server_connection_ssh = true;
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(colors.muted_text)
                                        .child(if ssh {
                                            "Uses your OpenSSH config and host-key policy"
                                        } else {
                                            "Connect directly to a Sift server"
                                        }),
                                ),
                        )
                        .child(
                            Field::new(
                                "NAME",
                                Some(self.server_name_input.focus_handle(cx)),
                                self.server_name_input.clone(),
                            ),
                        )
                        .child(
                            Field::new(
                                if ssh { "SSH DESTINATION" } else { "SERVER URL" },
                                Some(self.server_url_input.focus_handle(cx)),
                                self.server_url_input.clone(),
                            ),
                        )
                        .when(!ssh, |form| form.child(
                            Field::new(
                                "BEARER TOKEN",
                                Some(self.server_token_input.focus_handle(cx)),
                                self.server_token_input.clone(),
                            ),
                        ))
                        .when(!ssh, |form| form.child(
                            div()
                                .id("remember-server-token")
                                .role(Role::CheckBox)
                                .aria_label(if remember {
                                    "Remember token in the OS keychain, checked"
                                } else {
                                    "Remember token in the OS keychain, unchecked"
                                })
                                .flex()
                                .min_w_0()
                                .items_center()
                                .gap_2()
                                .cursor_pointer()
                                .on_click(cx.listener(|shell, _, _, cx| {
                                    shell.remember_server_token = !shell.remember_server_token;
                                    cx.notify();
                                }))
                                .child(
                                    div()
                                        .flex_none()
                                        .size(px(16.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded_sm()
                                        .border_1()
                                        .border_color(if remember {
                                            colors.accent
                                        } else {
                                            colors.strong_border
                                        })
                                        .when(remember, |box_view| {
                                            box_view
                                                .bg(colors.accent)
                                                .child(icon(
                                                    IconName::Check,
                                                    colors.on_accent,
                                                    11.,
                                                ))
                                        }),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .whitespace_normal()
                                        .child("Remember token in the OS keychain"),
                                ),
                        ))
                        .children(self.server_connection_error.as_ref().map(|message| {
                            ErrorBanner::new(message.clone())
                        }))
                        .child(
                            div()
                                .flex()
                                .min_w_0()
                                .justify_between()
                                .items_center()
                                .gap_2()
                                .children(selected.is_some().then(|| {
                                    Button::new("forget-server", "Forget")
                                        .tone(ButtonTone::DangerGhost)
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.forget_selected_server(cx)
                                        }))
                                }))
                                .child(
                                    Button::new(
                                        "connect-server",
                                        if pending {
                                            "Testing connection…"
                                        } else {
                                            "Test & Connect"
                                        },
                                    )
                                    .tone(ButtonTone::Accent)
                                    .wide(true)
                                    .loading(pending)
                                    .on_click(cx.listener(|shell, _, _, cx| {
                                        shell.submit_server_connection(cx)
                                    })),
                                ),
                        )
                        .into_any_element()
                }
                Modal::Snippets => {
                    let selected = self.snippet_selected;
                    let immutable = self
                        .snippets
                        .get(selected)
                        .is_some_and(|snippet| snippet.id.is_none());
                    div()
                        .w_full()
                        .h(px(620.))
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .h(px(42.))
                                .px_3()
                                .flex()
                                .items_center()
                                .justify_between()
                                .border_b_1()
                                .border_color(colors.subtle_border)
                                .child(
                                    div()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child("SQL snippets"),
                                )
                                .child(
                                    Button::new("new-snippet", "New")
                                        .tone(ButtonTone::Ghost)
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.new_snippet(cx)
                                        })),
                                ),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_h_0()
                                .flex()
                                .child(
                                    div()
                                        .id("snippet-list")
                                        .w(px(230.))
                                        .p_2()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .overflow_y_scroll()
                                        .border_r_1()
                                        .border_color(colors.subtle_border)
                                        .children(self.snippets.iter().enumerate().map(
                                            |(index, snippet)| {
                                                div()
                                                    .id(("snippet-row", index))
                                                    .px_2()
                                                    .py_1()
                                                    .rounded_sm()
                                                    .when(index == selected, |row| {
                                                        row.bg(colors.active_surface)
                                                    })
                                                    .hover(|row| row.bg(colors.hovered_surface))
                                                    .on_click(cx.listener(
                                                        move |shell, _, _, cx| {
                                                            shell.select_snippet(index, cx)
                                                        },
                                                    ))
                                                    .child(
                                                        div()
                                                            .font_family("monospace")
                                                            .child(snippet.trigger.clone()),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(colors.muted_text)
                                                            .child(format!(
                                                                "{} · {:?}",
                                                                snippet.title, snippet.scope
                                                            )),
                                                    )
                                            },
                                        )),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .p_3()
                                        .flex()
                                        .flex_col()
                                        .gap_3()
                                        .child(
                                            div()
                                                .flex()
                                                .gap_2()
                                                .child(
                                                    div()
                                                        .w(px(150.))
                                                        .child(self.snippet_trigger_input.clone()),
                                                )
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .child(self.snippet_title_input.clone()),
                                                )
                                                .child(
                                                    Button::new(
                                                        "snippet-scope",
                                                        format!("{:?}", self.snippet_scope),
                                                    )
                                                    .tone(ButtonTone::Neutral)
                                                    .disabled(immutable)
                                                    .on_click(cx.listener(|shell, _, _, cx| {
                                                        shell.cycle_snippet_scope(cx)
                                                    })),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_h_0()
                                                .border_1()
                                                .border_color(colors.subtle_border)
                                                .rounded_sm()
                                                .child(self.snippet_body_editor.clone()),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(colors.muted_text)
                                                .child("Tabstops: $1, ${1:default}, and final $0"),
                                        )
                                        .children(self.snippet_error.as_ref().map(|error| {
                                            div().id("snippet-error").debug_selector(|| "snippet-error".into())
                                                .max_h(px(120.)).flex_none().overflow_y_scroll()
                                                .text_sm().text_color(colors.danger).child(error.clone())
                                        })),
                                ),
                        )
                        .child(
                            div()
                                .px_3()
                                .py_2()
                                .flex()
                                .justify_between()
                                .border_t_1()
                                .border_color(colors.subtle_border)
                                .child(
                                    Button::new("delete-snippet", "Delete")
                                        .tone(ButtonTone::DangerGhost)
                                        .disabled(immutable || self.snippet_pending)
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.delete_selected_snippet(cx)
                                        })),
                                )
                                .child(
                                    Button::new("save-snippet", "Save")
                                        .debug_selector("save-snippet")
                                        .tone(ButtonTone::Accent)
                                        .disabled(immutable || self.snippet_pending)
                                        .loading(self.snippet_pending)
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.save_snippet(cx)
                                        })),
                                ),
                        )
                        .into_any_element()
                }
                Modal::ApiTokens => {
                    let rows = self.api_tokens.clone();
                    div()
                        .h(px(520.))
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(div().text_lg().font_weight(gpui::FontWeight::SEMIBOLD).child("API tokens"))
                                .child(Button::new("refresh-api-tokens", "Refresh").tone(ButtonTone::Ghost).disabled(self.api_tokens_pending).on_click(cx.listener(|shell, _, _, cx| shell.open_api_tokens(cx)))),
                        )
                        .children(self.api_token_plaintext.clone().map(|plaintext| {
                            let copy = plaintext.clone();
                            div().p_3().rounded_sm().border_1().border_color(colors.warning).bg(colors.active_surface).flex().flex_col().gap_2()
                                .child(div().text_sm().text_color(colors.warning).child("Copy this token now. It will not be shown again."))
                                .child(div().font_family("monospace").text_sm().child(plaintext))
                                .child(Button::new("copy-issued-api-token", "Copy token").tone(ButtonTone::Accent).on_click(cx.listener(move |shell, _, _, cx| {
                                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy.clone()));
                                    shell.show_success_toast("API token copied".into(), cx);
                                })))
                        }))
                        .child(
                            div().flex().items_center().gap_2()
                                .child(div().flex_1().child(self.api_token_name_input.clone()))
                                .child(Button::new("issue-api-token", "Create token").tone(ButtonTone::Accent).loading(self.api_tokens_pending).on_click(cx.listener(|shell, _, _, cx| shell.issue_api_token(cx)))),
                        )
                        .children(self.api_tokens_error.clone().map(|error| div().text_sm().text_color(colors.danger).child(error)))
                        .child(
                            div().id("api-token-list").flex_1().min_h_0().overflow_y_scroll().children(rows.into_iter().enumerate().map(|(index, token)| {
                                let token_id = token.id;
                                div().id(("api-token-row", index)).debug_selector(move || format!("api-token-row-{}", token_id.0)).h(px(48.)).px_2().flex().items_center().gap_3().border_b_1().border_color(colors.subtle_border)
                                    .child(div().min_w_0().flex_1().flex().flex_col().child(token.name).child(div().text_xs().text_color(colors.muted_text).child(format!("Created {}{}", token.created_at.format("%Y-%m-%d"), token.last_used_at.map(|at| format!(" · used {}", at.format("%Y-%m-%d"))).unwrap_or_default()))))
                                    .child(Button::new(("revoke-api-token", index), "Revoke").tone(ButtonTone::DangerGhost).disabled(self.api_tokens_pending).on_click(cx.listener(move |shell, _, _, cx| shell.revoke_api_token(token_id, cx))))
                            }))
                        )
                        .into_any_element()
                }
                Modal::ConnectionPolicy => {
                    let policy = self.connection_policy.clone();
                    let read_only = policy.as_ref().is_some_and(|policy| policy.read_only);
                    let role = policy
                        .as_ref()
                        .map(|policy| format!("{:?}", policy.minimum_tenant_role))
                        .unwrap_or_else(|| "Loading…".into());
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_lg()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child("Connection policy"),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(colors.muted_text)
                                        .child(format!(
                                            "Profile {}",
                                            self.connection_policy_profile
                                                .map_or_else(|| "—".into(), |id| id.to_string())
                                        )),
                                ),
                        )
                        .child(
                            div()
                                .p_3()
                                .rounded_sm()
                                .border_1()
                                .border_color(colors.subtle_border)
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .child("Read-only access")
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(colors.muted_text)
                                                .child("Block operations that can mutate data."),
                                        ),
                                )
                                .child(
                                    Button::new(
                                        "toggle-connection-policy-read-only",
                                        if read_only { "On" } else { "Off" },
                                    )
                                    .tone(if read_only {
                                        ButtonTone::Accent
                                    } else {
                                        ButtonTone::Neutral
                                    })
                                    .disabled(policy.is_none() || self.connection_policy_pending)
                                    .on_click(cx.listener(|shell, _, _, cx| {
                                        shell.toggle_connection_policy_read_only(cx)
                                    })),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_3()
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .child("Minimum tenant role")
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(colors.muted_text)
                                                .child("Click the role to cycle the access floor."),
                                        ),
                                )
                                .child(
                                    Button::new("cycle-connection-policy-role", role)
                                        .tone(ButtonTone::Neutral)
                                        .disabled(policy.is_none() || self.connection_policy_pending)
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.cycle_connection_policy_role(cx)
                                        })),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child("Allowed schemas")
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(colors.muted_text)
                                        .child("Comma-separated schema or catalog.schema names. Empty is unrestricted."),
                                )
                                .child(self.connection_policy_schemas_input.clone()),
                        )
                        .children(self.connection_policy_error.clone().map(|error| {
                            div().text_sm().text_color(colors.danger).child(error)
                        }))
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .child(
                                    Button::new("save-connection-policy", "Save policy")
                                        .tone(ButtonTone::Accent)
                                        .loading(self.connection_policy_pending)
                                        .disabled(policy.is_none())
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.save_connection_policy(cx)
                                        })),
                                ),
                        )
                        .into_any_element()
                }
                Modal::TenantUsage => {
                    let labels = [
                        "Connection profiles",
                        "Sessions",
                        "Connections",
                        "Concurrent queries",
                        "Cursors",
                        "Retained result bytes",
                    ];
                    let usage = self.tenant_usage.as_ref().map(|snapshot| {
                        [
                            snapshot.usage.connection_profiles,
                            snapshot.usage.sessions,
                            snapshot.usage.connections,
                            snapshot.usage.concurrent_queries,
                            snapshot.usage.cursors,
                            snapshot.usage.retained_result_bytes,
                        ]
                    });
                    let limits = self.tenant_usage.as_ref().map(|snapshot| {
                        [
                            snapshot.limits.connection_profiles,
                            snapshot.limits.sessions,
                            snapshot.limits.connections,
                            snapshot.limits.concurrent_queries,
                            snapshot.limits.cursors,
                            snapshot.limits.retained_result_bytes,
                        ]
                    });
                    let rows = labels.into_iter().enumerate().map(|(index, label)| {
                        div()
                            .h(px(44.))
                            .flex()
                            .items_center()
                            .gap_3()
                            .border_b_1()
                            .border_color(colors.subtle_border)
                            .child(div().flex_1().child(label))
                            .child(
                                div()
                                    .w(px(110.))
                                    .text_sm()
                                    .text_color(colors.muted_text)
                                    .child(usage.map_or_else(
                                        || "— used".into(),
                                        |values| format!("{} used", values[index]),
                                    )),
                            )
                            .child(
                                div()
                                    .w(px(160.))
                                    .text_sm()
                                    .child(limits.map_or_else(
                                        || "— limit".into(),
                                        |values| values[index].map_or_else(
                                            || "Unlimited".into(),
                                            |value| format!("{value} limit"),
                                        ),
                                    )),
                            )
                    });
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_lg()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child("Tenant usage and limits"),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(colors.muted_text)
                                        .child(self.tenant_usage.as_ref().map_or_else(
                                            || "Loading…".into(),
                                            |usage| format!("Tenant {}", usage.tenant_id),
                                        )),
                                ),
                        )
                        .child(div().children(rows))
                        .children(self.tenant_usage_error.clone().map(|error| {
                            div().text_sm().text_color(colors.danger).child(error)
                        }))
                        .child(
                            Button::new("edit-manifest-tenant-limits", "Edit limits in sift.toml")
                                .tone(ButtonTone::Accent)
                                .on_click(cx.listener(|shell, _, _, cx| {
                                    shell.edit_manifest_section("server.tenant_limits", cx)
                                })),
                        )
                        .into_any_element()
                }
                Modal::VcsDiagnostics => {
                    let diagnostics = self.vcs_diagnostics.clone();
                    let rows = diagnostics.as_ref().map(|diagnostics| {
                        let mut rows = vec![
                            ("Adapter", diagnostics.adapter_id.clone().unwrap_or_else(|| "Not configured".into())),
                            ("Generation", diagnostics.generation.clone().unwrap_or_else(|| "—".into())),
                            ("Executable", diagnostics.executable.clone().unwrap_or_else(|| "—".into())),
                            ("Version", diagnostics.executable_version.clone().unwrap_or_else(|| "—".into())),
                            ("Network", if diagnostics.network_enabled { "Enabled".into() } else { "Disabled".into() }),
                            ("Credential helper", if diagnostics.credential_helper_available { "Available".into() } else { "Unavailable".into() }),
                        ];
                        if let Some(limits) = &diagnostics.limits {
                            rows.push(("Timeouts", format!("{}s local · {}s network", limits.local_timeout_secs, limits.network_timeout_secs)));
                            rows.push(("Output limits", format!("{} bytes output · {} bytes/file", limits.max_output_bytes, limits.max_file_bytes)));
                            rows.push(("Change limits", format!("{} status · {} commits · {} diff files", limits.max_status_entries, limits.max_history_page, limits.max_diff_files)));
                        }
                        rows
                    }).unwrap_or_default();
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(div().text_lg().font_weight(gpui::FontWeight::SEMIBOLD).child("VCS diagnostics"))
                                .child(Button::new("refresh-vcs-diagnostics", "Refresh").tone(ButtonTone::Ghost).loading(self.vcs_diagnostics_pending).on_click(cx.listener(|shell, _, _, cx| shell.open_vcs_diagnostics(cx)))),
                        )
                        .children(diagnostics.as_ref().map(|diagnostics| {
                            div()
                                .p_3()
                                .rounded_sm()
                                .bg(if diagnostics.healthy { colors.success_muted } else { colors.danger_muted })
                                .text_color(if diagnostics.healthy { colors.success } else { colors.danger })
                                .child(if !diagnostics.enabled { "VCS integration is disabled" } else if diagnostics.healthy { "VCS adapter is healthy" } else { "VCS adapter needs attention" })
                        }))
                        .child(div().children(rows.into_iter().map(|(label, value)| {
                            div().min_h(px(36.)).py_1().flex().items_center().gap_3().border_b_1().border_color(colors.subtle_border)
                                .child(div().w(px(150.)).text_color(colors.muted_text).child(label))
                                .child(div().min_w_0().flex_1().child(value))
                        })))
                        .children(diagnostics.and_then(|diagnostics| diagnostics.diagnostic).map(|message| div().p_3().rounded_sm().bg(colors.active_surface).child(message)))
                        .children(self.vcs_diagnostics_error.clone().map(|error| div().text_sm().text_color(colors.danger).child(error)))
                        .into_any_element()
                }
                Modal::Administration => {
                    let keys = self.principal_keys.clone();
                    let approvals = self.operation_approvals.clone();
                    let audit_rows = self.operation_audit_rows.clone();
                    div().h(px(650.)).flex().flex_col().gap_3()
                        .child(div().flex().items_center().justify_between()
                            .child(div().text_lg().font_weight(gpui::FontWeight::SEMIBOLD).child("Runtime administration"))
                            .child(Button::new("refresh-administration", "Refresh").tone(ButtonTone::Ghost).disabled(self.principal_admin_pending).on_click(cx.listener(|shell, _, _, cx| match shell.administration_section {
                                AdministrationSection::Keys => shell.load_principal_keys(cx),
                                AdministrationSection::Approvals => shell.load_operation_approvals(cx),
                                AdministrationSection::Audit => shell.load_operation_audit(false, cx),
                            }))))
                        .child(div().flex().gap_1()
                            .child(Button::new("admin-keys-tab", "Signing keys").tone(if self.administration_section == AdministrationSection::Keys { ButtonTone::Accent } else { ButtonTone::Neutral }).on_click(cx.listener(|shell, _, _, cx| shell.select_administration_section(AdministrationSection::Keys, cx))))
                            .child(Button::new("admin-approvals-tab", "Approvals").tone(if self.administration_section == AdministrationSection::Approvals { ButtonTone::Accent } else { ButtonTone::Neutral }).on_click(cx.listener(|shell, _, _, cx| shell.select_administration_section(AdministrationSection::Approvals, cx))))
                            .child(Button::new("admin-audit-tab", "Audit").tone(if self.administration_section == AdministrationSection::Audit { ButtonTone::Accent } else { ButtonTone::Neutral }).on_click(cx.listener(|shell, _, _, cx| shell.select_administration_section(AdministrationSection::Audit, cx)))))
                        .when(self.administration_section == AdministrationSection::Keys, |view| view
                            .child(div().flex().gap_2().children(self.principal_key_inputs.iter().cloned()).child(Button::new("register-principal-key", "Register key").tone(ButtonTone::Accent).loading(self.principal_admin_pending).on_click(cx.listener(|shell, _, _, cx| shell.register_principal_key(cx)))))
                            .child(div().id("principal-key-list").flex_1().min_h_0().overflow_y_scroll().children(keys.into_iter().enumerate().map(|(index, key)| { let key_id = key.id.0; div().id(("principal-key", index)).min_h(px(48.)).px_2().flex().items_center().gap_2().border_b_1().border_color(colors.subtle_border).child(div().min_w_0().flex_1().flex().flex_col().child(key.label).child(div().text_xs().font_family("monospace").text_color(colors.muted_text).child(key.fingerprint))).child(Button::new(("revoke-principal-key", index), "Revoke").tone(ButtonTone::DangerGhost).disabled(key.revoked_at.is_some() || self.principal_admin_pending).on_click(cx.listener(move |shell, _, _, cx| shell.revoke_principal_key(key_id, cx)))) }))))
                        .when(self.administration_section == AdministrationSection::Approvals, |view| view
                            .child(div().text_xs().text_color(colors.muted_text).child("Pending extension operations are scoped to your identity and expire automatically."))
                            .child(div().id("operation-approval-list").flex_1().min_h_0().overflow_y_scroll().children(approvals.into_iter().enumerate().map(|(index, approval)| { let approval_id = approval.id.clone(); let revision = approval.revision; let pending = approval.approved_at.is_none() && approval.consumed_at.is_none(); div().id(("operation-approval", index)).min_h(px(52.)).px_2().flex().items_center().gap_2().border_b_1().border_color(colors.subtle_border).child(div().min_w_0().flex_1().flex().flex_col().child(approval.operation_id).child(div().text_xs().font_family("monospace").text_color(colors.muted_text).child(format!("{} · expires {}", approval.id, approval.expires_at)))).child(Button::new(("approve-operation", index), if pending { "Approve" } else if approval.consumed_at.is_some() { "Consumed" } else { "Approved" }).tone(if pending { ButtonTone::Accent } else { ButtonTone::Neutral }).disabled(!pending || self.principal_admin_pending).on_click(cx.listener(move |shell, _, _, cx| shell.approve_operation(approval_id.clone(), revision, cx)))) }))))
                        .when(self.administration_section == AdministrationSection::Audit, |view| view
                            .child(div().id("operation-audit-list").flex_1().min_h_0().overflow_y_scroll().children(audit_rows.into_iter().enumerate().map(|(index, row)| div().id(("operation-audit-row", index)).min_h(px(54.)).px_2().py_1().flex().items_center().gap_3().border_b_1().border_color(colors.subtle_border).child(div().w(px(145.)).flex_none().text_xs().text_color(colors.muted_text).child(row.at.format("%Y-%m-%d %H:%M:%S").to_string())).child(div().min_w_0().flex_1().flex().flex_col().child(format!("{} · {}", row.action, row.target)).child(div().truncate().text_xs().text_color(colors.muted_text).child(format!("actor {} · {}{}", row.actor_principal_id.map_or_else(|| "system".into(), |id| id.0.to_string()), row.status, row.error_message.map(|error| format!(" · {error}")).unwrap_or_default())))))))
                            .child(Button::new("load-more-operation-audit", "Load more").tone(ButtonTone::Ghost).disabled(self.operation_audit_cursor.is_none() || self.principal_admin_pending).on_click(cx.listener(|shell, _, _, cx| shell.load_operation_audit(true, cx)))))
                        .children(self.principal_admin_error.clone().map(|error| div().text_sm().text_color(colors.danger).child(error)))
                        .into_any_element()
                }
                Modal::Settings => {
                    let dark_theme = self.dark_theme;
                    let toggle_row = |id: &'static str,
                                      title: &'static str,
                                      description: &'static str,
                                      on: bool,
                                      on_click: sift_ui::ClickHandler| {
                        div()
                            .id(id)
                            .debug_selector(move || id.to_owned())
                            .role(Role::Button)
                            .aria_label(format!("{title}: {}", if on { "on" } else { "off" }))
                            .min_h(px(54.))
                            .px_2()
                            .flex()
                            .items_center()
                            .gap_3()
                            .rounded_sm()
                            .hover(|row| row.bg(colors.hovered_surface))
                            .on_click(on_click)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(title)
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(colors.muted_text)
                                            .child(description),
                                    ),
                            )
                            .child(
                                div()
                                    .w(px(32.))
                                    .h(px(18.))
                                    .p(px(2.))
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .rounded_full()
                                    .bg(if on {
                                        colors.accent
                                    } else {
                                        colors.strong_border
                                    })
                                    .when(on, |toggle| toggle.justify_end())
                                    .child(
                                        div()
                                            .size(px(14.))
                                            .rounded_full()
                                            .bg(colors.text),
                                    ),
                            )
                    };
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .text_lg()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child("Settings"),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(colors.muted_text)
                                        .child("Preferences are stored on this device."),
                                ),
                        )
                        .child(
                            div().pt_3().border_t_1().border_color(colors.subtle_border).flex().items_center().justify_between().gap_3()
                                .child(div().flex().flex_col().gap_1().child("API tokens").child(div().text_xs().text_color(colors.muted_text).child("Create and revoke tokens for API clients.")))
                                .child(Button::new("manage-api-tokens", "Manage…").tone(ButtonTone::Neutral).on_click(cx.listener(|shell, _, _, cx| shell.open_api_tokens(cx)))),
                        )
                        .child(
                            div().flex().items_center().justify_between().gap_3()
                                .child(div().flex().flex_col().gap_1().child("Connection policy").child(div().text_xs().text_color(colors.muted_text).child("Set role, write, and schema access for the active connection.")))
                                .child(Button::new("manage-connection-policy", "Manage…").tone(ButtonTone::Neutral).on_click(cx.listener(|shell, _, _, cx| shell.open_connection_policy(cx)))),
                        )
                        .child(
                            div().flex().items_center().justify_between().gap_3()
                                .child(div().flex().flex_col().gap_1().child("Tenant limits").child(div().text_xs().text_color(colors.muted_text).child("Inspect live usage and configure resource ceilings.")))
                                .child(Button::new("manage-tenant-usage", "Manage…").tone(ButtonTone::Neutral).on_click(cx.listener(|shell, _, _, cx| shell.open_tenant_usage(cx)))),
                        )
                        .child(
                            div().flex().items_center().justify_between().gap_3()
                                .child(div().flex().flex_col().gap_1().child("VCS diagnostics").child(div().text_xs().text_color(colors.muted_text).child("Inspect adapter health, executable details, and safety limits.")))
                                .child(Button::new("open-vcs-diagnostics", "Inspect…").tone(ButtonTone::Neutral).on_click(cx.listener(|shell, _, _, cx| shell.open_vcs_diagnostics(cx)))),
                        )
                        .child(
                            div().flex().items_center().justify_between().gap_3()
                                .child(div().flex().flex_col().gap_1().child("Server administration").child(div().text_xs().text_color(colors.muted_text).child("Manage identities, access, approvals, and audit records.")))
                                .child(Button::new("open-server-administration", "Manage…").tone(ButtonTone::Neutral).on_click(cx.listener(|shell, _, _, cx| shell.open_administration(cx)))),
                        )
                        .child(toggle_row(
                            "settings-theme",
                            "Dark appearance",
                            "Switch between Sift Ayu Dark and the light theme.",
                            dark_theme,
                            Box::new(cx.listener(
                                |shell: &mut WorkspaceShell, _, _, cx| shell.toggle_theme(cx),
                            )) as sift_ui::ClickHandler,
                        ))
                        .child(toggle_row(
                            "settings-query-results-right",
                            "Results beside query",
                            "Place SQL on the left and query results on the right.",
                            self.settings.data.query_results_placement
                                == QueryResultsPlacement::Right,
                            Box::new(cx.listener(
                                |shell: &mut WorkspaceShell, _, _, cx| {
                                    shell.toggle_query_results_placement(cx)
                                },
                            )) as sift_ui::ClickHandler,
                        ))
                        .child(toggle_row(
                            "settings-selection-aggregates",
                            "Selection sum and average",
                            "Show sum and average for numeric selections in Data tabs.",
                            self.settings.data.selection_aggregates,
                            Box::new(cx.listener(
                                |shell: &mut WorkspaceShell, _, _, cx| {
                                    shell.toggle_selection_aggregates(cx)
                                },
                            )) as sift_ui::ClickHandler,
                        ))
                        .child(toggle_row(
                            "settings-recent-objects",
                            "Recent database objects",
                            "Remember and show up to five recently opened database objects.",
                            self.settings.ui.recent_objects,
                            Box::new(cx.listener(
                                |shell: &mut WorkspaceShell, _, _, cx| {
                                    shell.toggle_recent_objects(cx)
                                },
                            )) as sift_ui::ClickHandler,
                        ))
                        .child(
                            div()
                                .min_h(px(64.))
                                .px_2()
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .child("Navigation hints")
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(colors.muted_text)
                                                .child("Show shortcut footers always, while holding Alt, or never."),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .flex()
                                        .gap_1()
                                        .children([
                                            ("settings-navigation-hints-always", "Always", NavigationHints::Always),
                                            ("settings-navigation-hints-hold", "Hold Alt", NavigationHints::Hold),
                                            ("settings-navigation-hints-hidden", "Hidden", NavigationHints::Hidden),
                                        ].map(|(id, label, mode)| {
                                            Button::new(id, label)
                                                .tone(if self.settings.ui.navigation_hints == mode {
                                                    ButtonTone::Accent
                                                } else {
                                                    ButtonTone::Neutral
                                                })
                                                .on_click(cx.listener(move |shell, _, _, cx| {
                                                    shell.set_navigation_hints(mode, cx)
                                                }))
                                        })),
                                ),
                        )
                        .child(
                            div()
                                .pt_3()
                                .border_t_1()
                                .border_color(colors.subtle_border)
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_3()
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .child("Themes")
                                        .child(
                                            div()
                                                .truncate()
                                                .text_xs()
                                                .text_color(colors.muted_text)
                                                .child(format!(
                                                    "Current: {}",
                                                    self.settings.appearance.theme
                                                )),
                                        ),
                                )
                                .child(
                                    Button::new("manage-themes", "Manage themes…")
                                        .tone(ButtonTone::Neutral)
                                        .debug_selector("manage-themes")
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.open_themes_modal(cx)
                                        })),
                                ),
                        )
                        .child(
                            div()
                                .pt_3()
                                .border_t_1()
                                .border_color(colors.subtle_border)
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_3()
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .child("Advanced settings")
                                        .child(
                                            div()
                                                .truncate()
                                                .text_xs()
                                                .text_color(colors.muted_text)
                                                .child(
                                                    self.settings_store
                                                        .as_ref()
                                                        .map(|store| {
                                                            store.path().display().to_string()
                                                        })
                                                        .unwrap_or_else(|| {
                                                            "settings.toml is unavailable".into()
                                                        }),
                                                ),
                                        ),
                                )
                                .child(
                                    Button::new("open-settings-file", "Open settings.toml")
                                        .tone(ButtonTone::Neutral)
                                        .debug_selector("open-settings-file")
                                        .on_click(cx.listener(|shell, _, window, cx| {
                                            shell.open_user_settings(window, cx)
                                        })),
                                ),
                        )
                        .into_any_element()
                }
                Modal::Themes => {
                    let current = self.settings.appearance.theme.clone();
                    let themes_path = self
                        .settings_store
                        .as_ref()
                        .map(|store| store.themes_dir().display().to_string())
                        .unwrap_or_else(|| "themes directory unavailable".into());
                    let builtin_button = |id: &'static str,
                                          label: &'static str,
                                          theme_id: &'static str,
                                          cx: &mut Context<Self>| {
                        Button::new(id, label)
                            .tone(if current == theme_id {
                                ButtonTone::Accent
                            } else {
                                ButtonTone::Neutral
                            })
                            .debug_selector(id)
                            .on_click(cx.listener(move |shell, _, _, cx| {
                                match shell.apply_theme_name(theme_id, cx) {
                                    Ok(()) => shell.theme_error = None,
                                    Err(error) => shell.theme_error = Some(error),
                                }
                                cx.notify();
                            }))
                    };
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child("Themes"),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(colors.muted_text)
                                        .child("Choose a base, edit TOML in a tab, or move themes between Sift installations."),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(builtin_button(
                                    "theme-ayu-dark",
                                    "Ayu Dark",
                                    "ayu-dark",
                                    cx,
                                ))
                                .child(builtin_button(
                                    "theme-soft-light",
                                    "Soft Light",
                                    "light",
                                    cx,
                                )),
                        )
                        .children((!self.custom_themes.is_empty()).then(|| {
                            div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(SectionLabel::new("Custom themes"))
                                .child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .gap_2()
                                        .children(self.custom_themes.iter().cloned().enumerate().map(
                                            |(index, theme_id)| {
                                                let selected = current == theme_id;
                                                let selected_id = theme_id.clone();
                                                Button::new(
                                                    ("custom-theme", index),
                                                    theme_id.clone(),
                                                )
                                                .tone(if selected {
                                                    ButtonTone::Accent
                                                } else {
                                                    ButtonTone::Neutral
                                                })
                                                .on_click(cx.listener(
                                                    move |shell, _, _, cx| {
                                                        match shell.apply_theme_name(
                                                            &selected_id,
                                                            cx,
                                                        ) {
                                                            Ok(()) => shell.theme_error = None,
                                                            Err(error) => {
                                                                shell.theme_error = Some(error)
                                                            }
                                                        }
                                                        cx.notify();
                                                    },
                                                ))
                                            },
                                        )),
                                )
                        }))
                        .child(
                            div()
                                .p_3()
                                .rounded_sm()
                                .border_1()
                                .border_color(colors.subtle_border)
                                .bg(colors.surface)
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(colors.muted_text)
                                        .child("SELECTED THEME"),
                                )
                                .child(
                                    div()
                                        .font_family("monospace")
                                        .child(current.clone()),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .text_xs()
                                        .text_color(colors.disabled_text)
                                        .child(themes_path),
                                ),
                        )
                        .children(
                            self.theme_error
                                .clone()
                                .map(|error| ErrorBanner::new(error).into_any_element()),
                        )
                        .child(
                            div()
                                .pt_2()
                                .border_t_1()
                                .border_color(colors.subtle_border)
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    Button::new("edit-current-theme", "Edit in tab")
                                        .tone(ButtonTone::Accent)
                                        .debug_selector("edit-current-theme")
                                        .disabled(self.settings_store.is_none())
                                        .on_click(cx.listener(|shell, _, window, cx| {
                                            shell.edit_current_theme(window, cx)
                                        })),
                                )
                                .child(
                                    Button::new("import-theme", "Import TOML…")
                                        .tone(ButtonTone::Neutral)
                                        .debug_selector("import-theme")
                                        .disabled(self.settings_store.is_none())
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.prompt_import_theme(cx)
                                        })),
                                )
                                .child(
                                    Button::new("export-theme", "Export TOML…")
                                        .tone(ButtonTone::Neutral)
                                        .debug_selector("export-theme")
                                        .disabled(self.settings_store.is_none())
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.prompt_export_theme(cx)
                                        })),
                                ),
                        )
                        .into_any_element()
                }
                Modal::Keymaps => {
                    let keymaps_path = self
                        .settings_store
                        .as_ref()
                        .map(|store| store.keymaps_path().display().to_string())
                        .unwrap_or_else(|| "keymaps.json unavailable".into());
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child("Keymaps"),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_xs()
                                        .text_color(colors.muted_text)
                                        .child(keymaps_path),
                                )
                                .child(
                                    Button::new("open-keymaps-file", "Open JSON")
                                        .tone(ButtonTone::Ghost)
                                        .debug_selector("open-keymaps-file")
                                        .on_click(cx.listener(|shell, _, window, cx| {
                                            shell.open_user_keymaps(window, cx)
                                        })),
                                ),
                        )
                        .child(
                            div()
                                .id("keymap-profile-vim")
                                .debug_selector(|| "keymap-profile-vim".into())
                                .text_sm()
                                .child("Vim navigation and editing. Customize leader commands below."),
                        )
                        .children(self.keymaps_error.clone().map(|error| {
                            ErrorBanner::new(error).into_any_element()
                        }))
                        .child(
                            div()
                                .pt_2()
                                .border_t_1()
                                .border_color(colors.subtle_border)
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(SectionLabel::new("IDE leader bindings"))
                                .child(
                                    Button::new("reset-keymaps", "Reset defaults")
                                        .tone(ButtonTone::Ghost)
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.reset_keymaps_modal(cx)
                                        })),
                                ),
                        )
                        .child(
                            div()
                                .h(px(26.))
                                .px_2()
                                .flex()
                                .items_center()
                                .text_xs()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(colors.muted_text)
                                .child(div().flex_1().child("COMMAND"))
                                .child(div().w(px(205.)).child("BINDING"))
                                .child(div().w(px(112.)).child("DEFAULT")),
                        )
                        .child(
                            div()
                                .id("keymap-bindings-scroll")
                                .max_h(px(360.))
                                .overflow_y_scroll()
                                .children(
                                    CommandRegistry::definitions()
                                        .iter()
                                        .filter(|definition| {
                                            definition.language.starts_with("<leader>")
                                        })
                                        .filter_map(|definition| {
                                            let input =
                                                self.keymap_inputs.get(&definition.id)?.clone();
                                            Some(
                                                div()
                                                    .h(px(34.))
                                                    .px_2()
                                                    .flex()
                                                    .items_center()
                                                    .gap_2()
                                                    .border_t_1()
                                                    .border_color(colors.subtle_border)
                                                    .child(
                                                        div()
                                                            .flex_1()
                                                            .min_w_0()
                                                            .flex()
                                                            .items_center()
                                                            .gap_2()
                                                            .child(
                                                                div()
                                                                    .min_w_0()
                                                                    .truncate()
                                                                    .child(definition.label),
                                                            )
                                                            .child(
                                                                div()
                                                                    .min_w_0()
                                                                    .truncate()
                                                                    .text_xs()
                                                                    .text_color(
                                                                        colors.disabled_text,
                                                                    )
                                                                    .child(definition.id.as_str()),
                                                            ),
                                                    )
                                                    .child(div().w(px(205.)).child(input))
                                                    .child(
                                                        div()
                                                            .w(px(112.))
                                                            .truncate()
                                                            .font_family("monospace")
                                                            .text_xs()
                                                            .text_color(colors.muted_text)
                                                            .child(definition.language),
                                                    ),
                                            )
                                        }),
                                ),
                        )
                        .child(
                            div()
                                .pt_2()
                                .border_t_1()
                                .border_color(colors.subtle_border)
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .flex_1()
                                        .text_xs()
                                        .text_color(colors.muted_text)
                                        .child("Empty binding disables a command."),
                                )
                                .child(
                                    Button::new("save-keymaps", "Save")
                                        .tone(ButtonTone::Accent)
                                        .debug_selector("save-keymaps")
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.save_keymaps_modal(cx)
                                        })),
                                ),
                        )
                        .into_any_element()
                }
                Modal::Account => {
                    let server_name = self.active_server_name();
                    let identity = self.lifecycle.identity.as_ref();
                    let is_local = self
                        .lifecycle
                        .selected_instance
                        .as_ref()
                        .is_some_and(|instance| instance.kind == crate::InstanceKind::Local);
                    let interactive = identity
                        .is_some_and(|identity| identity.auth_session_id.is_some());
                    let pending = self.account_pending;
                    let username = identity.map(|identity| {
                        identity
                            .github_login
                            .as_ref()
                            .map(|login| format!("@{login}"))
                            .unwrap_or_else(|| identity.principal.display_name.clone())
                    });
                    let github_link = identity
                        .and_then(|identity| identity.github_login.as_ref())
                        .map(|login| {
                            let github_url = format!("https://github.com/{login}");
                            div()
                                .id("account-github-profile")
                                .debug_selector(|| "account-github-profile".into())
                                .role(Role::Link)
                                .aria_label(format!("Open @{login} on GitHub"))
                                .size(px(24.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_sm()
                                .text_color(colors.muted_text)
                                .cursor(CursorStyle::PointingHand)
                                .hover(|link| {
                                    link.bg(colors.hovered_surface).text_color(colors.text)
                                })
                                .on_click(move |_, _, cx| cx.open_url(&github_url))
                                .child(icon(IconName::Github, colors.muted_text, 14.))
                        });
                    let session_cards = self
                        .server_sessions
                        .iter()
                        .map(|session| {
                            let session_id = session.id;
                            let connection_rows = session.connections.iter().map(|connection| {
                                let connection_id = connection.id;
                                div()
                                    .h(px(28.))
                                    .pl_3()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(icon(IconName::Database, colors.muted_text, 11.))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .flex_1()
                                            .truncate()
                                            .text_xs()
                                            .child(connection.display_name.clone()),
                                    )
                                    .child(
                                        Button::new(
                                            (
                                                "close-server-connection",
                                                connection_id.0 as usize,
                                            ),
                                            "Close",
                                        )
                                        .tone(ButtonTone::Ghost)
                                        .disabled(self.server_sessions_loading)
                                        .on_click(cx.listener(move |shell, _, _, cx| {
                                            shell.close_server_connection(
                                                session_id,
                                                connection_id,
                                                cx,
                                            )
                                        })),
                                    )
                            });
                            div()
                                .debug_selector(move || format!("server-session-{}", session_id.0))
                                .rounded_sm()
                                .border_1()
                                .border_color(colors.subtle_border)
                                .overflow_hidden()
                                .child(
                                    div()
                                        .h(px(32.))
                                        .px_2()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .bg(colors.surface)
                                        .child(
                                            div()
                                                .min_w_0()
                                                .flex_1()
                                                .truncate()
                                                .text_xs()
                                                .child(format!(
                                                    "Session {} · {} connection(s)",
                                                    session_id.0,
                                                    session.connections.len()
                                                )),
                                        )
                                        .child(
                                            Button::new(
                                                ("close-server-session", session_id.0 as usize),
                                                "Close session",
                                            )
                                            .tone(ButtonTone::DangerGhost)
                                            .disabled(self.server_sessions_loading)
                                            .on_click(cx.listener(move |shell, _, _, cx| {
                                                shell.close_server_session(session_id, cx)
                                            })),
                                        ),
                                )
                                .children(connection_rows)
                        })
                        .collect::<Vec<_>>();
                    let field = |label: &'static str, input: Entity<TextInput>| {
                        Field::new(label, Some(input.focus_handle(cx)), input.clone())
                    };

                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .when_some(username, |account, username| {
                            account.child(
                                div()
                                    .min_h(px(48.))
                                    .px_3()
                                    .py_2()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child(username),
                                    )
                                    .children(github_link),
                            )
                        })
                        .when(identity.is_none(), |account| {
                            account.child(
                                div()
                                    .px_3()
                                    .py_3()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child("Sign in to Sift"),
                                    )
                                    .child(
                                        div()
                                            .truncate()
                                            .text_sm()
                                            .text_color(colors.muted_text)
                                            .child(server_name),
                                    ),
                            )
                        })
                        .when(is_local, |account| {
                            account.child(
                                div()
                                    .border_t_1()
                                    .border_color(colors.subtle_border)
                                    .px_3()
                                    .py_3()
                                    .text_sm()
                                    .text_color(colors.muted_text)
                                    .whitespace_normal()
                                    .child("This local instance manages its built-in identity."),
                            )
                        })
                        .when(identity.is_some(), |account| {
                            account.child(
                                div()
                                    .debug_selector(|| "account-server-sessions".into())
                                    .border_t_1()
                                    .border_color(colors.subtle_border)
                                    .px_3()
                                    .py_2()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .justify_between()
                                            .child(SectionLabel::new("SERVER SESSIONS"))
                                            .child(
                                                Button::new("refresh-server-sessions", "Refresh")
                                                    .tone(ButtonTone::Ghost)
                                                    .loading(self.server_sessions_loading)
                                                    .on_click(cx.listener(|shell, _, _, cx| {
                                                        shell.load_server_sessions(cx)
                                                    })),
                                            ),
                                    )
                                    .when(
                                        self.server_sessions.is_empty()
                                            && !self.server_sessions_loading
                                            && self.server_sessions_error.is_none(),
                                        |sessions| {
                                            sessions.child(
                                                div()
                                                    .text_xs()
                                                    .text_color(colors.muted_text)
                                                    .child("No open server sessions"),
                                            )
                                        },
                                    )
                                    .children(session_cards)
                                    .children(
                                        self.server_sessions_error
                                            .as_ref()
                                            .map(|message| ErrorBanner::new(message.clone())),
                                    ),
                            )
                        })
                        .when(!is_local && identity.is_none(), |account| {
                            account.child(
                                div()
                                    .border_t_1()
                                    .border_color(colors.subtle_border)
                                    .px_3()
                                    .py_3()
                                    .flex()
                                    .flex_col()
                                    .gap_3()
                                    .child(
                                        Button::new(
                                            "account-github-sign-in",
                                            if pending {
                                                "Waiting for sign in…"
                                            } else {
                                                "Continue with GitHub"
                                            },
                                        )
                                        .tone(ButtonTone::Accent)
                                        .wide(true)
                                        .start_icon(IconName::Github)
                                        .loading(pending)
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.sign_in_with_github(cx)
                                        })),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .border_t_1()
                                                    .border_color(colors.subtle_border),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(colors.muted_text)
                                                    .child("OR"),
                                            )
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .border_t_1()
                                                    .border_color(colors.subtle_border),
                                            ),
                                    )
                                    .child(field("Username", self.account_username_input.clone()))
                                    .child(field("Password", self.account_password_input.clone()))
                                    .child(
                                        Button::new(
                                            "account-password-sign-in",
                                            "Sign in with password",
                                        )
                                        .tone(ButtonTone::Neutral)
                                        .wide(true)
                                        .loading(pending)
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.sign_in_with_password(cx)
                                        })),
                                    ),
                            )
                        })
                        .when(!is_local && identity.is_some() && !interactive, |account| {
                            account.child(
                                div()
                                    .border_t_1()
                                    .border_color(colors.subtle_border)
                                    .px_3()
                                    .py_3()
                                    .text_sm()
                                    .text_color(colors.muted_text)
                                    .whitespace_normal()
                                    .child("This identity is managed by the current instance."),
                            )
                        })
                        .when(!is_local && interactive, |account| {
                            account.child(
                                div()
                                    .border_t_1()
                                    .border_color(colors.subtle_border)
                                    .px_3()
                                    .py_2()
                                    .flex()
                                    .flex_wrap()
                                    .items_center()
                                    .justify_between()
                                    .gap_2()
                                    .child(
                                        Button::new("account-refresh-session", "Refresh session")
                                            .tone(ButtonTone::Ghost)
                                            .loading(pending)
                                            .on_click(cx.listener(|shell, _, _, cx| {
                                                shell.refresh_account_session(cx)
                                            })),
                                    )
                                    .child(
                                        Button::new(
                                            "account-sign-out-all",
                                            "Sign out everywhere",
                                        )
                                        .tone(ButtonTone::DangerGhost)
                                        .loading(pending)
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.sign_out(true, cx)
                                        })),
                                    )
                                    .child(
                                        Button::new(
                                            "account-sign-out",
                                            if pending {
                                                "Signing out…"
                                            } else {
                                                "Sign out"
                                            },
                                        )
                                        .tone(ButtonTone::Neutral)
                                        .loading(pending)
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.sign_out(false, cx)
                                        })),
                                    ),
                            )
                        })
                        .children(self.account_error.as_ref().map(|message| {
                            ErrorBanner::new(message.clone())
                        }))
                        .into_any_element()
                }
                Modal::ConnectionUrl => {
                    let pending = self.database_connection_pending;
                    let workspace_name = self
                        .selected_database_tenant
                        .and_then(|id| {
                            self.lifecycle
                                .tenants
                                .iter()
                                .find(|tenant| tenant.id.0 == id)
                        })
                        .map(|tenant| tenant.name.as_str())
                        .unwrap_or("current workspace");
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child("Add PostgreSQL connection"),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(colors.muted_text)
                                        .child(format!(
                                            "Paste a URL and press Enter · saves to {workspace_name}"
                                        )),
                                ),
                        )
                        .child(
                            div()
                                .id("connection-url-input")
                                .w_full()
                                .child(self.connection_url_input.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(colors.muted_text)
                                .whitespace_normal()
                                .child("Credentials are masked here, removed from the saved connection settings, and stored through the server's secure secret store."),
                        )
                        .children(
                            self.database_connection_error
                                .as_ref()
                                .map(|message| ErrorBanner::new(message.clone())),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_2()
                                .child(
                                    Button::new("connection-url-manual", "Manual setup")
                                        .tone(ButtonTone::Ghost)
                                        .disabled(pending)
                                        .on_click(cx.listener(|shell, _, window, cx| {
                                            shell.open_database_connection(window, cx)
                                        })),
                                )
                                .child(
                                    Button::new("connection-url-once", "Connect once")
                                        .tone(ButtonTone::Neutral)
                                        .disabled(pending)
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.connect_connection_url_once(cx)
                                        })),
                                )
                                .child(
                                    Button::new(
                                        "connection-url-submit",
                                        if pending { "Connecting…" } else { "Add & connect" },
                                    )
                                    .tone(ButtonTone::Accent)
                                    .loading(pending)
                                    .disabled(pending)
                                    .on_click(cx.listener(|shell, _, _, cx| {
                                        shell.submit_connection_url(cx)
                                    })),
                                ),
                        )
                        .into_any_element()
                }
                Modal::DatabaseConnection => {
                    let editing = self.editing_connection_profile.is_some();
                    let step = self.database_wizard_step;
                    let selected_tenant = self.selected_database_tenant;
                    let selected_provider = self.selected_database_provider.clone();
                    let selected_ssl_mode = self.selected_database_ssl_mode.clone();
                    let pending = self.database_connection_pending;
                    let tenant_rows = self.lifecycle.tenants.iter().map(|tenant| {
                        let tenant_id = tenant.id.0;
                        let selected = selected_tenant == Some(tenant_id);
                        div()
                            .id(("database-tenant", tenant_id as usize))
                            .role(Role::Button)
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .border_1()
                            .border_color(if selected {
                                colors.accent
                            } else {
                                colors.subtle_border
                            })
                            .when(selected, |row| row.bg(colors.accent_muted))
                            .when(!selected, |row| {
                                row.hover(|row| row.bg(colors.hovered_surface))
                            })
                            .on_click(cx.listener(move |shell, _, _, cx| {
                                shell.selected_database_tenant = Some(tenant_id);
                                cx.notify();
                            }))
                            .child(tenant.name.clone())
                    });
                    let provider_rows = [
                        (
                            "sift/postgres",
                            "PostgreSQL",
                            "databases/postgres.svg",
                        ),
                        (
                            "sift/sql-server",
                            "Microsoft SQL Server",
                            "databases/sql-server.svg",
                        ),
                    ]
                    .into_iter()
                    .enumerate()
                    .map(|(index, (provider_id, display_name, asset))| {
                            let available = self
                                .lifecycle
                                .providers
                                .iter()
                                .find(|provider| {
                                    provider.provider.provider_id.as_str() == provider_id
                                })
                                .is_some_and(|provider| provider.available);
                            let selected =
                                selected_provider.as_deref() == Some(provider_id);
                            let logo_size = if provider_id == "sift/postgres" {
                                82.0
                            } else {
                                76.0
                            };
                            let provider_id = provider_id.to_owned();
                            div()
                                .id(("database-provider", index))
                                .role(Role::Button)
                                .aria_label(format!("Select {display_name}"))
                                .relative()
                                .flex_1()
                                .min_w(px(280.))
                                .min_h(px(190.))
                                .p_4()
                                .rounded_lg()
                                .border_2()
                                .border_color(if selected {
                                    colors.accent
                                } else {
                                    colors.subtle_border
                                })
                                .when(selected, |row| row.bg(colors.accent_muted))
                                .when(!available, |row| row.opacity(0.45))
                                .when(!selected && available, |row| {
                                    row.hover(|row| row.bg(colors.hovered_surface))
                                })
                                .when(available, |row| {
                                    row.on_click(cx.listener(move |shell, _, _, cx| {
                                        shell.select_database_provider(provider_id.clone(), cx);
                                    }))
                                })
                                .child(
                                    div()
                                        .h(px(112.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(
                                            div()
                                                .size(px(96.))
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .rounded_lg()
                                                .bg(gpui::white())
                                                .border_1()
                                                .border_color(colors.subtle_border)
                                                .child(
                                                    img(database_logo(asset))
                                                        .size(px(logo_size))
                                                        .object_fit(gpui::ObjectFit::Contain),
                                                ),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_center()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child(display_name),
                                )
                                .when(!available, |card| card.child(
                                    div()
                                        .text_xs()
                                        .text_center()
                                        .when(!selected, |copy| copy.text_color(colors.muted_text))
                                        .child("Unavailable on this server"),
                                ))
                                .when(selected, |card| {
                                    card.child(
                                        div()
                                            .absolute()
                                            .top_2()
                                            .right_2()
                                            .size(px(22.))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                             .rounded_full()
                                             .bg(colors.accent)
                                             .child(icon(IconName::Check, colors.on_accent, 13.)),
                                    )
                                })
                        });
                    let (security_label, security_options): (&str, &[(&str, &str)]) =
                        if selected_provider.as_deref() == Some("sift/sql-server") {
                            (
                                "ENCRYPTION",
                                &[
                                    ("disable", "Disabled"),
                                    ("require", "Required"),
                                    ("trust_server_certificate", "Trust Server Certificate"),
                                ],
                            )
                        } else {
                            (
                                "SSL MODE",
                                &[
                                    ("disable", "Disabled"),
                                    ("prefer", "Prefer"),
                                    ("require", "Require"),
                                    ("verify_ca", "Verify CA"),
                                    ("verify_full", "Verify Full"),
                                ],
                            )
                        };
                    let ssl_rows = security_options.iter().copied().enumerate().map(
                        |(index, (value, label))| {
                            let selected = selected_ssl_mode.as_deref() == Some(value);
                            div()
                                .id(("database-ssl-mode", index))
                                .role(Role::Button)
                                .px_2()
                                .py_1()
                                .rounded_sm()
                                .border_1()
                                .border_color(if selected {
                                    colors.accent
                                } else {
                                    colors.subtle_border
                                })
                                .when(selected, |row| row.bg(colors.accent_muted))
                                .when(!selected, |row| {
                                    row.hover(|row| row.bg(colors.hovered_surface))
                                })
                                .on_click(cx.listener(move |shell, _, _, cx| {
                                    shell.selected_database_ssl_mode = Some(value.to_owned());
                                    cx.notify();
                                }))
                                .child(label)
                        },
                    );
                    let field = |label: &'static str, input: Entity<TextInput>| {
                        Field::new(
                            label,
                            Some(input.focus_handle(cx)),
                            input.clone(),
                        )
                    };
                    let step_number = match step {
                        DatabaseWizardStep::Provider => 1,
                        DatabaseWizardStep::Details => 2,
                        DatabaseWizardStep::Review => 3,
                    };
                    let step_rows = ["Database", "Connection", "Review"]
                        .into_iter()
                        .enumerate()
                        .map(|(index, label)| {
                            let number = index + 1;
                            let active = number == step_number;
                            let complete = number < step_number;
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .size(px(22.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded_full()
                                        .border_1()
                                        .border_color(if active || complete {
                                            colors.accent
                                        } else {
                                            colors.strong_border
                                        })
                                        .when(active || complete, |circle| {
                                            circle.bg(colors.accent).text_color(colors.on_accent)
                                        })
                                        .text_xs()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .when(complete, |circle| {
                                            circle
                                                .child(icon(IconName::Check, colors.on_accent, 12.))
                                        })
                                        .when(!complete, |circle| {
                                            circle.child(number.to_string())
                                        }),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(if active {
                                            gpui::FontWeight::SEMIBOLD
                                        } else {
                                            gpui::FontWeight::NORMAL
                                        })
                                        .text_color(if active {
                                            colors.text
                                        } else {
                                            colors.muted_text
                                        })
                                        .child(label),
                                )
                        });
                    let provider_name = selected_provider
                        .as_deref()
                        .map(|provider| match provider {
                            "sift/postgres" => "PostgreSQL",
                            "sift/sql-server" => "Microsoft SQL Server",
                            _ => provider,
                        })
                        .unwrap_or("Not selected");
                    let tenant_name = selected_tenant
                        .and_then(|id| {
                            self.lifecycle
                                .tenants
                                .iter()
                                .find(|tenant| tenant.id.0 == id)
                        })
                        .map(|tenant| tenant.name.clone())
                        .unwrap_or_else(|| "Not selected".into());
                    let review_row = |label: &'static str, value: String| {
                        div()
                            .flex()
                            .min_w_0()
                            .items_start()
                            .justify_between()
                            .gap_3()
                            .py_2()
                            .border_b_1()
                            .border_color(colors.subtle_border)
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(colors.muted_text)
                                    .child(label),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .whitespace_normal()
                                    .text_right()
                                    .line_clamp(2)
                                    .text_ellipsis()
                                    .child(value),
                            )
                    };
                    div()
                        .flex()
                        .flex_col()
                        .min_h_0()
                        .max_h(gpui::relative(1.))
                        .overflow_hidden()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .items_stretch()
                                .gap_2()
                                .px_3()
                                .pt_2()
                                .pb_2()
                                .border_b_1()
                                .border_color(colors.subtle_border)
                                .bg(colors.toolbar)
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child(
                                            div()
                                                .min_w_0()
                                                .truncate()
                                                .child(if editing {
                                                    "Edit Database Connection"
                                                } else {
                                                    "Add Database Connection"
                                                }),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .min_w_0()
                                        .overflow_x_hidden()
                                        .items_center()
                                        .gap_4()
                                        .children(step_rows),
                                ),
                        )
                        .child(
                            div()
                                .id("database-connection-form")
                                .tab_group()
                                .flex()
                                .flex_1()
                                .flex_col()
                                .min_h_0()
                                .gap_3()
                                .max_h(px(540.))
                                .overflow_y_scroll()
                                .p_3()
                                .when(step == DatabaseWizardStep::Provider, |form| {
                                    form.child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap_3()
                                            .child(
                                                div()
                                                    .flex()
                                                    .flex_wrap()
                                                    .gap_3()
                                                    .children(provider_rows),
                                            ),
                                    )
                                })
                                .when(step == DatabaseWizardStep::Details, |form| {
                                    form
                                        .child(
                                            div()
                                                .flex_col()
                                                .flex()
                                                .gap_2()
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(colors.muted_text)
                                                        .child("WORKSPACE"),
                                                )
                                                .child(div().flex().flex_1().flex_wrap().gap_1().children(tenant_rows)),
                                        )
                                        .child(
                                            div()
                                                .flex()
                                                .flex_wrap()
                                                .gap_3()
                                                .child(div().flex_1().min_w(px(220.)).child(field("CONNECTION NAME", self.database_name_input.clone())))
                                                .child(div().flex_1().min_w(px(220.)).child(field("DATABASE", self.database_catalog_input.clone()))),
                                        )
                                        .child(
                                            div()
                                                .flex()
                                                .min_w_0()
                                                .gap_3()
                                                .child(div().flex_1().min_w_0().child(field("HOST", self.database_host_input.clone())))
                                                .child(div().flex_none().w(px(112.)).child(field("PORT", self.database_port_input.clone()))),
                                        )
                                        .child(
                                            div()
                                                .flex()
                                                .flex_wrap()
                                                .gap_3()
                                                .child(div().flex_1().min_w(px(220.)).child(field("USERNAME", self.database_user_input.clone())))
                                                .child(div().flex_1().min_w(px(220.)).child(field("PASSWORD", self.database_password_input.clone()))),
                                        )
                                        .child(
                                            div()
                                                .flex()
                                                .flex_col()
                                                .gap_1()
                                                .child(div().text_xs().text_color(colors.muted_text).child(security_label))
                                                .child(div().flex().flex_wrap().gap_1().children(ssl_rows)),
                                        )
                                        .child(
                                            div()
                                                .pt_3()
                                                .border_t_1()
                                                .border_color(colors.subtle_border)
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .child("Advanced"),
                                        )
                                        .when(selected_provider.as_deref() == Some("sift/postgres"), |form| {
                                            form.child(
                                                div()
                                                    .flex()
                                                    .flex_wrap()
                                                    .gap_3()
                                                    .child(div().flex_1().min_w(px(220.)).child(field("SEARCH PATH", self.database_search_path_input.clone())))
                                                    .child(div().flex_1().min_w(px(220.)).child(field("APPLICATION NAME", self.database_application_name_input.clone()))),
                                            )
                                        })
                                        .child(
                                            div()
                                                .flex()
                                                .flex_wrap()
                                                .gap_3()
                                                .child(div().flex_1().min_w(px(180.)).child(field("CONNECT TIMEOUT (SECONDS)", self.database_timeout_input.clone())))
                                                .child(div().flex_1().min_w(px(180.)).child(field("MINIMUM POOL SIZE", self.database_pool_min_input.clone())))
                                                .when(selected_provider.as_deref() == Some("sift/postgres"), |row| {
                                                    row.child(div().flex_1().min_w(px(180.)).child(field("MAXIMUM POOL SIZE", self.database_pool_max_input.clone())))
                                                }),
                                        )
                                        .child(
                                            div()
                                                .flex()
                                                .flex_wrap()
                                                .gap_3()
                                                .child(div().flex_1().min_w(px(220.)).child(field("SESSION VARIABLES (JSON)", self.database_session_variables_input.clone())))
                                                .child(div().flex_1().min_w(px(220.)).child(field("STARTUP SQL", self.database_startup_sql_input.clone()))),
                                        )
                                        .child(
                                            div()
                                                .flex()
                                                .flex_wrap()
                                                .gap_3()
                                                .child(div().flex_1().min_w(px(220.)).child(field("FOLDER", self.database_folder_input.clone())))
                                                .child(div().flex_1().min_w(px(220.)).child(field("TAGS", self.database_tags_input.clone()))),
                                        )
                                        .child(
                                            Button::new(
                                                "database-connection-favorite",
                                                if self.editing_connection_favorite {
                                                    "★ Favorite"
                                                } else {
                                                    "☆ Add to favorites"
                                                },
                                            )
                                            .tone(ButtonTone::Neutral)
                                            .on_click(cx.listener(|shell, _, _, cx| {
                                                shell.editing_connection_favorite =
                                                    !shell.editing_connection_favorite;
                                                cx.notify();
                                            })),
                                        )
                                })
                                .when(step == DatabaseWizardStep::Review, |form| {
                                    form
                                        .child(review_row("Database type", provider_name.to_owned()))
                                        .child(review_row("Workspace", tenant_name))
                                        .child(review_row("Connection name", self.database_name_input.read(cx).text().to_owned()))
                                        .child(review_row(
                                            "Server",
                                            format!("{}:{}", self.database_host_input.read(cx).text(), self.database_port_input.read(cx).text()),
                                        ))
                                        .child(review_row(
                                            "Database",
                                            if self.database_catalog_input.read(cx).text().is_empty() {
                                                "Provider default".into()
                                            } else {
                                                self.database_catalog_input.read(cx).text().to_owned()
                                            },
                                        ))
                                        .child(review_row("Username", self.database_user_input.read(cx).text().to_owned()))
                                        .child(review_row(
                                            "Password",
                                            if self.database_password_input.read(cx).text().is_empty() {
                                                if editing {
                                                    "Keep existing credential".into()
                                                } else {
                                                    "Not provided".into()
                                                }
                                            } else {
                                                "Stored securely".into()
                                            },
                                        ))
                                        .child(review_row(
                                            "Transport security",
                                            selected_ssl_mode.clone().unwrap_or_else(|| "Provider default".into()).replace('_', " "),
                                        ))
                                        .child(review_row(
                                            "Session variables",
                                            if self.database_session_variables_input.read(cx).text().trim().is_empty() {
                                                "None".into()
                                            } else {
                                                self.database_session_variables_input.read(cx).text().trim().to_owned()
                                            },
                                        ))
                                        .child(review_row(
                                            "Startup SQL",
                                            if self.database_startup_sql_input.read(cx).text().trim().is_empty() {
                                                "None".into()
                                            } else {
                                                "Configured".into()
                                            },
                                        ))
                                        .child(review_row(
                                            "Folder",
                                            if self.database_folder_input.read(cx).text().trim().is_empty() {
                                                "None".into()
                                            } else {
                                                self.database_folder_input.read(cx).text().trim().to_owned()
                                            },
                                        ))
                                        .child(review_row(
                                            "Tags",
                                            if self.database_tags_input.read(cx).text().trim().is_empty() {
                                                "None".into()
                                            } else {
                                                self.database_tags_input.read(cx).text().trim().to_owned()
                                            },
                                        ))
                                        .child(review_row(
                                            "Favorite",
                                            if self.editing_connection_favorite {
                                                "Yes".into()
                                            } else {
                                                "No".into()
                                            },
                                        ))
                                })
                                .children(self.database_connection_error.as_ref().map(|message| {
                                    ErrorBanner::new(message.clone())
                                })),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_end()
                                .px_3()
                                .py_2()
                                .border_t_1()
                                .border_color(colors.subtle_border)
                                .bg(colors.toolbar)
                                .child(
                                    div()
                                        .flex()
                                        .gap_2()
                                        .child(
                                            Button::new("database-wizard-test", if self.database_connection_tested { "Tested" } else { "Test connection" })
                                                .tone(ButtonTone::Neutral)
                                                .wide(true)
                                                .loading(pending)
                                                .disabled(step != DatabaseWizardStep::Review || pending)
                                                .on_click(cx.listener(|shell, _, _, cx| shell.test_database_connection(cx))),
                                        )
                                        .child(
                                            Button::new(
                                                "database-wizard-secondary",
                                                if step == DatabaseWizardStep::Provider {
                                                    "Cancel"
                                                } else {
                                                    "Back"
                                                },
                                            )
                                            .tone(ButtonTone::Neutral)
                                            .wide(true)
                                            .loading(pending)
                                            .on_click(cx.listener(
                                                move |shell, _, window, cx| {
                                                    if step == DatabaseWizardStep::Provider {
                                                        shell.dismiss_modal(
                                                            &DismissModal,
                                                            window,
                                                            cx,
                                                        )
                                                    } else {
                                                        shell.database_wizard_back(window, cx)
                                                    }
                                                },
                                            )),
                                        )
                                        .child(
                                            Button::new(
                                                "database-wizard-primary",
                                                if pending {
                                                    "Saving & Testing…"
                                                } else if step == DatabaseWizardStep::Review {
                                                    if editing {
                                                        "Update & Connect"
                                                    } else {
                                                        "Save & Connect"
                                                    }
                                                } else {
                                                    "Continue"
                                                },
                                            )
                                            .tone(ButtonTone::Accent)
                                            .wide(true)
                                            .loading(
                                                pending
                                                    || (step == DatabaseWizardStep::Provider
                                                        && selected_provider.is_none()),
                                            )
                                            .on_click(cx.listener(
                                                move |shell, _, window, cx| {
                                                    if step == DatabaseWizardStep::Review {
                                                        shell.submit_database_connection(cx)
                                                    } else {
                                                        shell.database_wizard_next(window, cx)
                                                    }
                                                },
                                            )),
                                        ),
                                ),
                        )
                        .into_any_element()
                }
                Modal::ConfirmTransactionDisconnect => {
                    let action = match self.pending_connection_change.as_ref() {
                        Some(PendingConnectionChange::Connect(entry)) => {
                            format!("switch to {}", entry.name)
                        }
                        Some(PendingConnectionChange::SwitchServer { label, .. }) => label.clone(),
                        Some(PendingConnectionChange::Quit) => "quit Sift".into(),
                        Some(PendingConnectionChange::Disconnect) | None => "disconnect".into(),
                    };
                    div()
                        .flex()
                        .flex_col()
                        .gap_4()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(icon(IconName::Warning, colors.warning, 16.))
                                .child("Open transaction"),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(colors.muted_text)
                                .whitespace_normal()
                                .child(format!(
                                    "The current transaction will be rolled back if you {action}. Continue?"
                                )),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new("cancel-transaction-disconnect", "Keep working")
                                        .tone(ButtonTone::Neutral)
                                        .wide(true)
                                        .on_click(cx.listener(|shell, _, window, cx| {
                                            shell.dismiss_modal(&DismissModal, window, cx)
                                        })),
                                )
                                .child(
                                    Button::new(
                                        "confirm-transaction-disconnect",
                                        "Roll back and continue",
                                    )
                                    .tone(ButtonTone::DangerMuted)
                                    .wide(true)
                                    .on_click(cx.listener(|shell, _, window, cx| {
                                        shell.confirm_transaction_disconnect(window, cx)
                                    })),
                                ),
                        )
                        .into_any_element()
                }
                Modal::ConfirmProductionExecution => div()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(icon(IconName::Warning, colors.danger, 16.))
                            .child("Confirm production write"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(colors.muted_text)
                            .whitespace_normal()
                            .child("This statement may modify production data. Review the active connection and SQL before continuing."),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new("cancel-production-execution", "Cancel")
                                    .tone(ButtonTone::Neutral)
                                    .wide(true)
                                    .on_click(cx.listener(|shell, _, window, cx| {
                                        shell.dismiss_modal(&DismissModal, window, cx)
                                    })),
                            )
                            .child(
                                Button::new("confirm-production-execution", "Run on PROD")
                                    .tone(ButtonTone::DangerMuted)
                                    .wide(true)
                                    .on_click(cx.listener(|shell, _, _, cx| {
                                        shell.confirm_production_execution(cx)
                                    })),
                            ),
                    )
                    .into_any_element(),
                Modal::ConfirmOutcomeUnknownRerun(_, _) => div()
                    .debug_selector(|| "confirm-outcome-unknown-rerun".into())
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(icon(IconName::Warning, colors.danger, 16.))
                            .child("Previous outcome is unknown"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(colors.muted_text)
                            .whitespace_normal()
                            .child("The server may have completed the previous statement before the connection was lost. Check the database or query history first. Running again can duplicate a write."),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new("cancel-outcome-unknown-rerun", "Do not run")
                                    .tone(ButtonTone::Neutral)
                                    .on_click(cx.listener(|shell, _, window, cx| {
                                        shell.dismiss_modal(&DismissModal, window, cx)
                                    })),
                            )
                            .child(
                                Button::new("confirm-outcome-unknown-rerun", "Run again")
                                    .tone(ButtonTone::DangerMuted)
                                    .on_click(cx.listener(|shell, _, _, cx| {
                                        shell.confirm_outcome_unknown_rerun(cx)
                                    })),
                            ),
                    )
                    .into_any_element(),
                Modal::ConfirmDeleteConnection(entry) => {
                    let entry = entry.clone();
                    let entry_for_delete = entry.clone();
                    div()
                        .flex()
                        .flex_col()
                        .gap_4()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(icon(IconName::Warning, colors.danger, 16.))
                                .child(format!("Delete {}?", entry.name)),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(colors.muted_text)
                                .whitespace_normal()
                                .child("This removes the connection and its stored credentials. Connections managed by sift.toml must be removed from the manifest instead."),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new("cancel-delete-connection", "Cancel")
                                        .tone(ButtonTone::Neutral)
                                        .wide(true)
                                        .on_click(cx.listener(|shell, _, window, cx| {
                                            shell.dismiss_modal(&DismissModal, window, cx)
                                        })),
                                )
                                .child(
                                    Button::new("confirm-delete-connection", "Delete")
                                        .tone(ButtonTone::DangerMuted)
                                        .wide(true)
                                        .on_click(cx.listener(move |shell, _, _, cx| {
                                            shell.confirm_delete_connection(
                                                &entry_for_delete,
                                                cx,
                                            )
                                        })),
                                ),
                        )
                        .into_any_element()
                }
                Modal::ConfirmTerminateProcess(process_id) => {
                    let process_id = *process_id;
                    div()
                        .flex()
                        .flex_col()
                        .gap_4()
                        .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(format!("Terminate database process {process_id}?")))
                        .child(div().text_sm().text_color(colors.muted_text).whitespace_normal().child("This asks the database server to terminate the selected process. Any open work in that process may be rolled back."))
                        .child(div().flex().justify_end().gap_2()
                            .child(Button::new("cancel-terminate-process", "Cancel").tone(ButtonTone::Neutral).on_click(cx.listener(|shell, _, window, cx| shell.dismiss_modal(&DismissModal, window, cx))))
                            .child(Button::new("confirm-terminate-process", "Terminate").tone(ButtonTone::DangerMuted).on_click(cx.listener(move |shell, _, _, cx| shell.confirm_terminate_process(process_id, cx)))))
                        .into_any_element()
                }
                Modal::CatalogDiagram => {
                    let (summary, cards) = self.catalog_diagram.diagram().map_or_else(
                        || ("Catalog diagram".to_owned(), Vec::new()),
                        |diagram| {
                            let names = diagram
                                .nodes
                                .iter()
                                .map(|node| (node.id.clone(), node.qualified_name.clone()))
                                .collect::<HashMap<_, _>>();
                            let cards = diagram
                                .nodes
                                .iter()
                                .filter(|node| matches!(node.kind,
                                    sift_protocol::CatalogNodeKind::Table
                                        | sift_protocol::CatalogNodeKind::View
                                        | sift_protocol::CatalogNodeKind::MaterializedView
                                        | sift_protocol::CatalogNodeKind::ForeignTable
                                        | sift_protocol::CatalogNodeKind::PartitionedTable))
                                .take(100)
                                .enumerate()
                                .map(|(index, node)| {
                                    let relations = diagram.edges.iter().filter(|edge| edge.from == node.id).map(|edge| {
                                        let target = edge.to.as_ref().and_then(|id| names.get(id)).cloned().or_else(|| edge.referenced_path.clone()).unwrap_or_else(|| "unresolved target".into());
                                        format!("{:?} → {target}", edge.kind)
                                    }).collect::<Vec<_>>();
                                    div()
                                        .debug_selector(move || format!("catalog-diagram-card-{index}"))
                                        .w(px(300.))
                                        .min_h(px(84.))
                                        .p_3()
                                        .rounded_sm()
                                        .border_1()
                                        .border_color(colors.subtle_border)
                                        .bg(colors.elevated_surface)
                                        .child(div().font_weight(gpui::FontWeight::SEMIBOLD).truncate().child(node.qualified_name.clone()))
                                        .child(div().text_xs().text_color(colors.muted_text).child(format!("{:?}", node.kind)))
                                        .children(relations.into_iter().take(6).map(|relation| div().mt_1().truncate().text_xs().child(relation)))
                                })
                                .collect::<Vec<_>>();
                            (
                                format!(
                                    "{} nodes · {} relationships{}",
                                    diagram.nodes.len(),
                                    diagram.edges.len(),
                                    if diagram.partial { " · partial" } else { "" }
                                ),
                                cards,
                            )
                        },
                    );
                    div()
                        .debug_selector(|| "catalog-diagram-modal".into())
                        .w_full()
                        .max_h(px(720.))
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child("Catalog diagram"))
                                .child(div().text_xs().text_color(colors.muted_text).child(summary)),
                        )
                        .children(
                            self.catalog_diagram
                                .request()
                                .error()
                                .map(|message| ErrorBanner::new(message.to_string())),
                        )
                        .when(self.catalog_diagram.request().loading(), |diagram| diagram.child(div().p_4().text_center().text_color(colors.muted_text).child("Loading catalog relationships…")))
                        .child(
                            div()
                                .id("catalog-diagram-cards")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .flex()
                                .flex_wrap()
                                .content_start()
                                .gap_3()
                                .children(cards),
                        )
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(Button::new("copy-catalog-mermaid", "Copy Mermaid").debug_selector("copy-catalog-mermaid").tone(ButtonTone::Ghost).disabled(self.catalog_diagram.diagram().is_none()).on_click(cx.listener(|shell, _, _, cx| shell.copy_catalog_diagram_mermaid(cx))))
                                .child(Button::new("refresh-catalog-diagram", "Refresh").debug_selector("refresh-catalog-diagram").tone(ButtonTone::Neutral).loading(self.catalog_diagram.request().loading()).on_click(cx.listener(|shell, _, _, cx| shell.open_catalog_diagram(cx))))
                                .child(Button::new("compare-catalog-diagram", "Compare to baseline…").debug_selector("compare-catalog-diagram").tone(ButtonTone::Accent).disabled(self.catalog_diagram.request().loading()).on_click(cx.listener(|shell, _, _, cx| shell.prepare_catalog_migration(cx))))
                        )
                        .into_any_element()
                }
                Modal::CatalogSnapshots => {
                    let rows = self
                        .catalog_snapshots
                        .iter()
                        .enumerate()
                        .map(|(index, snapshot)| {
                            let snapshot_id = snapshot.id;
                            let selected = self.selected_catalog_snapshot == Some(snapshot_id);
                            Button::new(
                                ("catalog-snapshot", index),
                                snapshot
                                    .description
                                    .clone()
                                    .unwrap_or_else(|| format!("Baseline {}", snapshot.id)),
                            )
                            .debug_selector(format!("catalog-snapshot-{index}"))
                            .tone(if selected {
                                ButtonTone::Accent
                            } else {
                                ButtonTone::Neutral
                            })
                            .wide(true)
                            .on_click(cx.listener(move |shell, _, _, cx| {
                                shell.select_catalog_snapshot(snapshot_id, cx)
                            }))
                            .into_any_element()
                        })
                        .collect::<Vec<_>>();
                    let selected = self.selected_catalog_snapshot.and_then(|selected| {
                        self.catalog_snapshots
                            .iter()
                            .find(|snapshot| snapshot.id == selected)
                    });
                    div()
                        .debug_selector(|| "catalog-snapshot-manager".into())
                        .w_full()
                        .max_h(px(620.))
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child("Schema baselines"))
                        .child(div().text_sm().text_color(colors.muted_text).child("Choose an immutable baseline to compare against the current live catalog."))
                        .children(self.catalog_snapshots_error.clone().map(ErrorBanner::new))
                        .when(self.catalog_snapshots_loading, |manager| manager.child(div().p_3().text_center().text_color(colors.muted_text).child("Loading baselines…")))
                        .when(self.catalog_snapshots.is_empty() && !self.catalog_snapshots_loading, |manager| manager.child(div().p_3().text_center().text_color(colors.muted_text).child("No baselines for this connection.")))
                        .child(div().id("catalog-snapshot-list").flex_1().min_h_0().overflow_y_scroll().flex().flex_col().gap_1().children(rows))
                        .children(selected.map(|snapshot| div().p_2().rounded_sm().bg(colors.active_surface).text_xs().child(format!(
                            "Created {} · revision {:?} · {} bytes · {}",
                            snapshot.created_at.to_rfc3339(), snapshot.catalog_revision, snapshot.retained_bytes, snapshot.content_digest
                        ))))
                        .child(
                            div()
                                .flex()
                                .justify_between()
                                .child(
                                    Button::new(
                                        "capture-new-catalog-snapshot",
                                        "Capture current baseline",
                                    )
                                    .tone(ButtonTone::Ghost)
                                    .on_click(cx.listener(|shell, _, _, cx| {
                                        shell.capture_catalog_snapshot(cx)
                                    })),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .gap_2()
                                        .child(
                                            Button::new("refresh-catalog-snapshots", "Refresh")
                                                .tone(ButtonTone::Neutral)
                                                .loading(self.catalog_snapshots_loading)
                                                .on_click(cx.listener(|shell, _, _, cx| {
                                                    shell.load_catalog_snapshots(cx)
                                                })),
                                        )
                                        .child(
                                            Button::new(
                                                "compare-selected-catalog-snapshot",
                                                "Compare selected",
                                            )
                                            .debug_selector(
                                                "compare-selected-catalog-snapshot",
                                            )
                                            .tone(ButtonTone::Accent)
                                            .disabled(self.selected_catalog_snapshot.is_none())
                                            .on_click(cx.listener(|shell, _, _, cx| {
                                                shell.prepare_catalog_migration(cx)
                                            })),
                                        ),
                                ),
                        )
                        .into_any_element()
                }
                Modal::CatalogMigration => {
                    let change_count = self.catalog_diff.as_ref().map_or(0, |diff| diff.changes.len());
                    let statement_count = self.catalog_migration_plan.as_ref().map_or(0, |plan| {
                        plan.groups.iter().map(|group| group.statements.len()).sum()
                    });
                    let statements = self
                        .catalog_migration_plan
                        .iter()
                        .flat_map(|plan| &plan.groups)
                        .flat_map(|group| &group.statements)
                        .take(20)
                        .map(|statement| {
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .p_2()
                                .border_b_1()
                                .border_color(colors.subtle_border)
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(colors.warning)
                                        .child(format!("{:?}", statement.risk)),
                                )
                                .child(div().text_sm().whitespace_normal().child(statement.sql.clone()))
                        })
                        .collect::<Vec<_>>();
                    let run_running = self.catalog_migration_run.as_ref().is_some_and(|run| {
                        run.state == sift_protocol::MigrationRunState::Running
                    });
                    let run_summary = self.catalog_migration_run.as_ref().map(|run| {
                        div()
                            .debug_selector(|| "catalog-migration-run-status".into())
                            .p_2()
                            .rounded_sm()
                            .bg(colors.active_surface)
                            .child(format!(
                                "Run {} · {:?} · {}/{} statement outcome(s)",
                                run.id,
                                run.state,
                                run.outcomes.len(),
                                statement_count
                            ))
                    });
                    let validation_summary = self
                        .catalog_migration_validation
                        .as_ref()
                        .map(|validation| {
                            div().p_2().rounded_sm().bg(colors.active_surface).child(format!(
                                "Test validation: {} · {} statement(s) · rollback {}",
                                if validation.valid { "passed" } else { "failed" },
                                validation.outcomes.len(),
                                if validation.rolled_back { "confirmed" } else { "failed" }
                            ))
                        });
                    let outcomes = self
                        .catalog_migration_run
                        .iter()
                        .flat_map(|run| &run.outcomes)
                        .enumerate()
                        .map(|(index, outcome)| {
                            div()
                                .debug_selector(move || format!("migration-outcome-{index}"))
                                .grid()
                                .grid_cols(4)
                                .gap_2()
                                .px_2()
                                .py_1()
                                .text_xs()
                                .child(format!(
                                    "{}.{}",
                                    outcome.group_ordinal, outcome.statement_ordinal
                                ))
                                .child(format!("{:?}", outcome.status))
                                .child(
                                    outcome
                                        .affected_rows
                                        .map_or_else(|| "—".into(), |rows| format!("{rows} rows")),
                                )
                                .child(outcome.result_code.clone().unwrap_or_else(|| "—".into()))
                        })
                        .collect::<Vec<_>>();
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .w_full()
                        .max_h(px(640.))
                        .child(div().flex().items_center().justify_between().child(div().font_weight(gpui::FontWeight::SEMIBOLD).child("Schema baseline migration")).child(Button::new("manage-ddl-sources", "DDL sources…").tone(ButtonTone::Ghost).on_click(cx.listener(|shell, _, _, cx| shell.open_ddl_sources(cx)))))
                        .child(div().text_sm().text_color(colors.muted_text).child(format!("{change_count} change(s) · {statement_count} statement(s)")))
                        .children(self.catalog_migration_error.clone().map(ErrorBanner::new))
                        .children(run_summary)
                        .children(validation_summary)
                        .children((!outcomes.is_empty()).then(|| div().id("catalog-migration-outcomes").max_h(px(160.)).overflow_y_scroll().children(outcomes)))
                        .child(div().id("catalog-migration-statements").flex_1().min_h_0().overflow_y_scroll().children(statements))
                        .children((statement_count > 20).then(|| div().text_xs().text_color(colors.muted_text).child(format!("{} more statement(s) not shown", statement_count - 20))))
                        .child(div().flex().justify_end().gap_2()
                            .child(Button::new("generate-catalog-migration-artifacts", "Generate SQL + rollback").debug_selector("generate-catalog-migration-artifacts").tone(ButtonTone::Accent).disabled(self.catalog_migration_pending || self.catalog_migration_plan.is_none() || self.catalog_rollback_sql().is_none()).on_click(cx.listener(|shell, _, _, cx| shell.generate_catalog_migration_artifacts(cx))))
                            .child(Button::new("validate-catalog-migration", "Validate on test DB").debug_selector("validate-catalog-migration").tone(ButtonTone::Ghost).disabled(self.catalog_migration_pending || self.catalog_migration_plan.is_none()).on_click(cx.listener(|shell, _, _, cx| shell.validate_catalog_migration(cx))))
                            .child(Button::new("copy-catalog-rollback", "Copy rollback SQL").debug_selector("copy-catalog-rollback").tone(ButtonTone::Ghost).disabled(self.catalog_rollback_sql().is_none()).on_click(cx.listener(|shell, _, _, cx| shell.copy_catalog_rollback_sql(cx))))
                            .child(Button::new("refresh-catalog-migration-run", "Refresh run").debug_selector("refresh-catalog-migration-run").tone(ButtonTone::Ghost).disabled(self.catalog_migration_run.is_none() || self.catalog_migration_pending).on_click(cx.listener(|shell, _, _, cx| shell.refresh_catalog_migration_run(cx))))
                            .child(Button::new("cancel-catalog-migration-run", "Stop run").debug_selector("cancel-catalog-migration-run").tone(ButtonTone::DangerGhost).disabled(!run_running || self.catalog_migration_pending).on_click(cx.listener(|shell, _, _, cx| shell.cancel_catalog_migration_run(cx))))
                            .child(Button::new("cancel-catalog-migration", "Cancel").tone(ButtonTone::Neutral).on_click(cx.listener(|shell, _, window, cx| shell.dismiss_modal(&DismissModal, window, cx))))
                            .child(Button::new("apply-catalog-migration", if self.catalog_migration_pending { "Working…" } else { "Apply migration" }).tone(ButtonTone::DangerMuted).disabled(self.catalog_migration_pending || self.catalog_migration_plan.is_none() || self.catalog_migration_run.is_some()).on_click(cx.listener(|shell, _, _, cx| shell.apply_catalog_migration(cx)))))
                        .into_any_element()
                }
                Modal::DdlSources => {
                    let rows = self.ddl_sources.clone();
                    div().w_full().h(px(580.)).flex().flex_col().gap_3()
                        .child(div().flex().items_center().justify_between().child(div().font_weight(gpui::FontWeight::SEMIBOLD).child("Workspace DDL sources")).child(Button::new("refresh-ddl-sources", "Refresh").tone(ButtonTone::Ghost).loading(self.ddl_sources_pending).on_click(cx.listener(|shell, _, _, cx| shell.open_ddl_sources(cx)))))
                        .child(div().text_xs().text_color(colors.muted_text).child("Offline schema models used by comparison and migration. Root IDs refer to folders or files in the active workspace."))
                        .child(div().flex().gap_2().children(self.ddl_source_inputs.iter().cloned()).child(Button::new("save-ddl-source", if self.selected_ddl_source.is_some() { "Save" } else { "Create" }).tone(ButtonTone::Accent).loading(self.ddl_sources_pending).on_click(cx.listener(|shell, _, _, cx| shell.save_ddl_source(cx)))))
                        .children(self.ddl_sources_error.clone().map(ErrorBanner::new))
                        .child(div().id("ddl-source-list").flex_1().min_h_0().overflow_y_scroll().children(rows.into_iter().enumerate().map(|(index, source)| { let source_id = source.id.0; let selected = self.selected_ddl_source == Some(source_id); div().id(("ddl-source", index)).min_h(px(52.)).px_2().flex().items_center().gap_2().border_b_1().border_color(colors.subtle_border).when(selected, |row| row.bg(colors.active_surface)).on_click(cx.listener(move |shell, _, _, cx| shell.select_ddl_source(source_id, cx))).child(div().min_w_0().flex_1().flex().flex_col().child(source.name).child(div().text_xs().text_color(colors.muted_text).child(format!("{} · {:?} · model r{} · {} diagnostic(s)", source.dialect_id, source.coverage, source.model_revision, source.diagnostic_count)))) })))
                        .child(div().flex().justify_end().gap_2().child(Button::new("refresh-selected-ddl-source", "Rebuild model").tone(ButtonTone::Neutral).disabled(self.selected_ddl_source.is_none() || self.ddl_sources_pending).on_click(cx.listener(|shell, _, _, cx| shell.mutate_selected_ddl_source(false, cx)))).child(Button::new("delete-selected-ddl-source", "Delete").tone(ButtonTone::DangerGhost).disabled(self.selected_ddl_source.is_none() || self.ddl_sources_pending).on_click(cx.listener(|shell, _, _, cx| shell.mutate_selected_ddl_source(true, cx)))))
                        .into_any_element()
                }
                Modal::RoomAdministration => {
                    let rooms = self.administered_rooms.clone();
                    let members = self.admin_room_members.clone();
                    let results = self.room_admin_results.clone();
                    div().w_full().h(px(620.)).flex().flex_col().gap_3()
                        .child(div().flex().items_center().justify_between().child(div().font_weight(gpui::FontWeight::SEMIBOLD).child("Room administration")).child(Button::new("refresh-room-administration", "Refresh").tone(ButtonTone::Ghost).loading(self.room_admin_pending).on_click(cx.listener(|shell, _, _, cx| shell.open_room_administration(cx)))))
                        .child(div().flex().gap_2().child(div().flex_1().child(self.room_admin_inputs[0].clone())).child(Button::new("create-shared-room", "Create shared room").debug_selector("create-shared-room").tone(ButtonTone::Accent).on_click(cx.listener(|shell, _, _, cx| shell.create_admin_room(cx)))))
                        .children(self.room_admin_error.clone().map(ErrorBanner::new))
                        .child(div().flex_1().min_h_0().flex().gap_3()
                            .child(div().id("admin-room-list").w(px(230.)).flex_none().overflow_y_scroll().children(rooms.into_iter().enumerate().map(|(index, room)| { let room_id = room.id.0; let selected = self.selected_admin_room == Some(room_id); div().id(("admin-room", index)).min_h(px(48.)).px_2().flex().flex_col().justify_center().rounded_sm().when(selected, |row| row.bg(colors.active_surface)).on_click(cx.listener(move |shell, _, _, cx| shell.select_admin_room(room_id, cx))).child(room.name).child(div().text_xs().text_color(colors.muted_text).child(format!("{:?} · {}", room.kind, room.bound_connection_profile_id.map_or_else(|| "unbound".into(), |id| format!("profile {}", id.0))))) })))
                            .child(div().min_w_0().flex_1().flex().flex_col().gap_2()
                                .child(div().flex().gap_2().child(div().flex_1().child(self.room_admin_inputs[1].clone())).child(Button::new("bind-admin-room", "Bind / unbind").tone(ButtonTone::Neutral).disabled(self.selected_admin_room.is_none()).on_click(cx.listener(|shell, _, _, cx| shell.bind_admin_room(cx)))))
                                .child(div().flex().gap_2().child(div().flex_1().child(self.room_admin_inputs[2].clone())).child(Button::new("cycle-room-role", format!("{:?}", self.room_admin_member_role)).tone(ButtonTone::Ghost).on_click(cx.listener(|shell, _, _, cx| shell.cycle_room_member_role(cx)))).child(Button::new("add-admin-room-member", "Add").tone(ButtonTone::Accent).disabled(self.selected_admin_room.is_none()).on_click(cx.listener(|shell, _, _, cx| shell.add_admin_room_member(cx)))))
                                .child(SectionLabel::new("Members"))
                                .child(div().id("admin-room-members").max_h(px(125.)).flex_none().overflow_y_scroll().children(members.into_iter().enumerate().map(|(index, member)| div().id(("admin-room-member", index)).h(px(32.)).px_2().flex().items_center().justify_between().child(format!("Principal {}", member.principal_id.0)).child(div().text_xs().text_color(colors.muted_text).child(format!("{:?}", member.role))))))
                                .child(SectionLabel::new("Shared results"))
                                .child(div().id("admin-room-results").flex_1().min_h_0().overflow_y_scroll().children(results.into_iter().enumerate().map(|(index, result)| div().id(("admin-room-result", index)).min_h(px(48.)).px_2().flex().flex_col().justify_center().border_b_1().border_color(colors.subtle_border).child(format!("{:?} · {} row(s) · {} page(s)", result.status, result.row_count.unwrap_or_default(), result.page_count)).child(div().truncate().text_xs().font_family("monospace").text_color(colors.muted_text).child(result.result_id.to_string()))))))
                        )
                        .child(div().flex().justify_end().gap_2().child(Button::new("join-admin-room", "Join").tone(ButtonTone::Neutral).disabled(self.selected_admin_room.is_none()).on_click(cx.listener(|shell, _, _, cx| shell.admin_room_action("join", cx)))).child(Button::new("leave-admin-room", "Leave").tone(ButtonTone::Ghost).disabled(self.selected_admin_room.is_none()).on_click(cx.listener(|shell, _, _, cx| shell.admin_room_action("leave", cx)))).child(Button::new("delete-admin-room", "Delete room").tone(ButtonTone::DangerGhost).disabled(self.selected_admin_room.is_none()).on_click(cx.listener(|shell, _, _, cx| shell.admin_room_action("delete", cx)))))
                        .into_any_element()
                }
                Modal::WorkspaceCreateFile | Modal::WorkspaceCreateFolder => {
                    let folder = matches!(modal, Modal::WorkspaceCreateFolder);
                    let parent = self
                        .workspace_files
                        .selected_node()
                        .filter(|node| node.kind == sift_protocol::WorkspaceNodeKind::Folder)
                        .map(|node| node.path.0.as_str())
                        .unwrap_or("workspace root");
                    div()
                        .debug_selector(|| "workspace-create-node".into())
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(if folder { "New folder" } else { "New SQL file" }),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(colors.muted_text)
                                .child(format!("Create beneath {parent}")),
                        )
                        .child(self.workspace_path_input.clone())
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new("cancel-workspace-create", "Cancel")
                                        .tone(ButtonTone::Neutral)
                                        .on_click(cx.listener(|shell, _, window, cx| {
                                            shell.dismiss_modal(&DismissModal, window, cx)
                                        })),
                                )
                                .child(
                                    Button::new("submit-workspace-create", "Create")
                                        .tone(ButtonTone::Accent)
                                        .disabled(
                                            self.workspace_path_input.read(cx).text().trim().is_empty()
                                                || self.workspace_files.mutation_pending(),
                                        )
                                        .on_click(cx.listener(move |shell, _, _, cx| {
                                            shell.create_workspace_node(folder, cx)
                                        })),
                                ),
                        )
                        .into_any_element()
                }
                Modal::WorkspaceMove => div()
                    .debug_selector(|| "workspace-move-node".into())
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Move or rename workspace node"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(colors.muted_text)
                            .child("Enter the complete destination path. The destination folder must already exist."),
                    )
                    .child(self.workspace_path_input.clone())
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new("cancel-workspace-move", "Cancel")
                                    .tone(ButtonTone::Neutral)
                                    .on_click(cx.listener(|shell, _, window, cx| {
                                        shell.dismiss_modal(&DismissModal, window, cx)
                                    })),
                            )
                            .child(
                                Button::new("submit-workspace-move", "Move")
                                    .tone(ButtonTone::Accent)
                                    .disabled(self.workspace_files.mutation_pending())
                                    .on_click(cx.listener(|shell, _, _, cx| {
                                        shell.move_workspace_node(cx)
                                    })),
                            ),
                    )
                    .into_any_element(),
                Modal::ConfirmWorkspaceDelete => {
                    let path = self
                        .workspace_files
                        .selected_node()
                        .map(|node| node.path.0.clone())
                        .unwrap_or_else(|| "selected node".into());
                    div()
                        .debug_selector(|| "confirm-workspace-delete".into())
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(format!("Delete {path}?")),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(colors.muted_text)
                                .child("This deletes the virtual subtree and is recorded in the audit log. Create a checkpoint first if you need a recovery point."),
                        )
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new("cancel-workspace-delete", "Cancel")
                                        .tone(ButtonTone::Neutral)
                                        .on_click(cx.listener(|shell, _, window, cx| {
                                            shell.dismiss_modal(&DismissModal, window, cx)
                                        })),
                                )
                                .child(
                                    Button::new("submit-workspace-delete", "Delete")
                                        .tone(ButtonTone::DangerMuted)
                                        .disabled(self.workspace_files.mutation_pending())
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.delete_workspace_node(cx)
                                        })),
                                ),
                        )
                        .into_any_element()
                }
                Modal::RepositorySetup => {
                    div()
                        .debug_selector(|| "repository-setup".into())
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(div().text_lg().font_weight(gpui::FontWeight::SEMIBOLD).child("Set up Git repository"))
                        .child(div().text_sm().text_color(colors.muted_text).child("The root handle must be configured by the server operator. Sift never accepts an arbitrary filesystem path."))
                        .child(div().flex().flex_col().gap_1().child(SectionLabel::new("CONFIGURED ROOT HANDLE")).child(self.repository_root_input.clone()))
                        .child(div().flex().flex_col().gap_1().child(SectionLabel::new("HTTPS CLONE URL")).child(self.repository_remote_url_input.clone()))
                        .child(div().flex().gap_2()
                            .child(div().flex_1().flex().flex_col().gap_1().child(SectionLabel::new("USERNAME")).child(self.repository_username_input.clone()))
                            .child(div().flex_1().flex().flex_col().gap_1().child(SectionLabel::new("PAT / PASSWORD")).child(self.repository_password_input.clone())))
                        .child(div().text_xs().text_color(colors.muted_text).child("Credentials are sent once and stored behind SecretStore. Clone currently supports HTTPS; managed SSH keys are deliberately unavailable until they can run without ambient agents."))
                        .child(div().flex().justify_end().gap_2()
                            .child(Button::new("cancel-repository-setup", "Cancel").tone(ButtonTone::Neutral).on_click(cx.listener(|shell, _, window, cx| shell.dismiss_modal(&DismissModal, window, cx))))
                            .child(Button::new("bind-existing-repository", "Bind existing").tone(ButtonTone::Neutral).on_click(cx.listener(|shell, _, _, cx| shell.submit_repository_setup(RepositorySetupMode::BindExisting, cx))))
                            .child(Button::new("initialize-repository", "Initialize").tone(ButtonTone::Neutral).on_click(cx.listener(|shell, _, _, cx| shell.submit_repository_setup(RepositorySetupMode::Initialize, cx))))
                            .child(Button::new("clone-repository", "Clone HTTPS").tone(ButtonTone::Accent).on_click(cx.listener(|shell, _, _, cx| shell.submit_repository_setup(RepositorySetupMode::Clone, cx)))))
                        .into_any_element()
                }
                Modal::RepositoryRemotes => {
                    let remotes = self.repository.remotes();
                    let repository_permalink = self.selected_workspace_id.zip(
                        self.repository.status().map(|status| status.binding_id.0),
                    ).map(|(workspace_id, binding_id)| {
                        format!("sift://workspace/{workspace_id}/repository/{binding_id}")
                    });
                    let credential_present = self
                        .repository
                        .observed_binding()
                        .is_some_and(|binding| binding.credential_handle_present);
                    let remote_rows = remotes.iter().enumerate().map(|(index, remote)| {
                        let name = remote.name.clone();
                        let url = remote.fetch_url.clone();
                        div().id(("repository-remote-row", index)).p_2().rounded_sm().border_1().border_color(colors.subtle_border)
                            .flex().items_center().gap_2()
                            .child(div().flex_1().min_w_0().flex().flex_col()
                                .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(name.clone()))
                                .child(div().text_xs().truncate().text_color(colors.muted_text).child(url.clone())))
                            .child(Button::new(("edit-repository-remote", index), "Edit").tone(ButtonTone::Ghost).on_click(cx.listener(move |shell, _, _, cx| {
                                shell.repository_remote_selected = Some(name.clone());
                                shell.repository_remote_name_input.update(cx, |input, cx| input.set_text(name.clone(), cx));
                                shell.repository_remote_url_input.update(cx, |input, cx| input.set_text(url.clone(), cx));
                            })))
                    });
                    let result_rows = self.repository.remote_result().map(|result| {
                        let changes = result.ref_changes.iter().map(|change| {
                            let before = change.before.as_deref().map(|oid| oid.chars().take(8).collect::<String>()).unwrap_or_else(|| "new".into());
                            let after = change.after.as_deref().map(|oid| oid.chars().take(8).collect::<String>()).unwrap_or_else(|| "deleted".into());
                            div().text_xs().font_family("monospace").child(format!("{}  {before} → {after}", change.name))
                        });
                        div().p_2().rounded_sm().bg(colors.surface).flex().flex_col().gap_1()
                            .child(format!("Last {} · {} ref change(s)", result.operation, result.ref_changes.len()))
                            .children(changes)
                    });
                    div()
                        .debug_selector(|| "repository-remotes".into())
                        .flex().flex_col().gap_3()
                        .child(div().flex().items_center().justify_between()
                            .child(div().text_lg().font_weight(gpui::FontWeight::SEMIBOLD).child("Remotes and credentials"))
                            .child(div().flex().gap_2()
                                .children(repository_permalink.map(|permalink| Button::new("copy-repository-permalink", "Copy repository link").tone(ButtonTone::Ghost).on_click(cx.listener(move |shell, _, _, cx| {
                                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(permalink.clone()));
                                    shell.show_toast("Copied repository link".into(), cx);
                                }))))
                                .child(Button::new("close-repository-remotes", "Close").tone(ButtonTone::Ghost).on_click(cx.listener(|shell, _, window, cx| shell.dismiss_modal(&DismissModal, window, cx)))))
                        )
                        .children(remote_rows)
                        .when(remotes.is_empty(), |view| view.child(div().text_sm().text_color(colors.muted_text).child("No remotes configured.")))
                        .child(div().flex().gap_2()
                            .child(div().w(px(140.)).child(self.repository_remote_name_input.clone()))
                            .child(div().flex_1().child(self.repository_remote_url_input.clone())))
                        .child(div().flex().gap_2()
                            .child(Button::new("save-repository-remote", "Add / update remote").tone(ButtonTone::Neutral).on_click(cx.listener(|shell, _, _, cx| shell.mutate_repository_remote(false, cx))))
                            .child(Button::new("remove-repository-remote", "Remove").tone(ButtonTone::DangerGhost).on_click(cx.listener(|shell, _, _, cx| shell.mutate_repository_remote(true, cx)))))
                        .child(div().border_t_1().border_color(colors.subtle_border).pt_3().flex().flex_col().gap_2()
                            .child(SectionLabel::new(if credential_present { "CREDENTIAL · CONFIGURED FOR YOU" } else { "CREDENTIAL · NOT CONFIGURED" }))
                            .child(div().flex().gap_2().child(div().flex_1().child(self.repository_username_input.clone())).child(div().flex_1().child(self.repository_password_input.clone())))
                            .child(div().flex().gap_2()
                                .child(Button::new("save-repository-credential", "Save / replace").tone(ButtonTone::Neutral).on_click(cx.listener(|shell, _, _, cx| shell.repository_credential_action("save", cx))))
                                .child(Button::new("test-repository-credential", "Test").tone(ButtonTone::Neutral).disabled(!credential_present).on_click(cx.listener(|shell, _, _, cx| shell.repository_credential_action("test", cx))))
                                .child(Button::new("remove-repository-credential", "Remove").tone(ButtonTone::DangerGhost).disabled(!credential_present).on_click(cx.listener(|shell, _, _, cx| shell.repository_credential_action("remove", cx)))))
                            .child(div().text_xs().text_color(colors.muted_text).child("PAT/basic credentials are scoped to your principal and revealed only to one explicit network operation.")))
                        .child(div().border_t_1().border_color(colors.subtle_border).pt_3().flex().items_center().gap_2()
                            .child(Button::new("fetch-repository", "Fetch selected remote").tone(ButtonTone::Neutral).disabled(self.repository.loading()).on_click(cx.listener(|shell, _, _, cx| shell.run_repository_remote(false, cx))))
                            .child(Button::new("push-repository", "Push current branch").tone(ButtonTone::Accent).disabled(self.repository.loading()).on_click(cx.listener(|shell, _, _, cx| shell.run_repository_remote(true, cx))))
                            .child(div().flex_1())
                            .child(div().text_xs().text_color(colors.muted_text).child("Network actions are manual and visible. Force push is unavailable.")))
                        .children(result_rows)
                        .into_any_element()
                }
                Modal::RepositoryHosting => {
                    let summary = self.repository_hosting.clone();
                    let credential_present = summary.as_ref().is_some_and(|summary| summary.credential_present);
                    let links = summary.iter().flat_map(|summary| summary.links.iter()).enumerate().map(|(index, link)| {
                        let url = link.url.clone();
                        Button::new(("open-hosting-link", index), link.label.clone())
                            .tone(ButtonTone::Ghost)
                            .on_click(move |_, _, cx| cx.open_url(&url))
                    });
                    let pulls = summary.iter().flat_map(|summary| summary.pull_requests.iter()).enumerate().map(|(index, pull)| {
                        let url = pull.url.clone();
                        div().id(("hosting-pull", index)).px_2().py_1().flex().items_center().gap_2()
                            .child(div().flex_1().min_w_0().truncate().child(format!("#{} {}", pull.id, pull.title)))
                            .child(div().text_xs().text_color(colors.muted_text).child(format!("{:?}", pull.state)))
                            .child(Button::new(("open-hosting-pull", index), "Open").tone(ButtonTone::Ghost).on_click(move |_, _, cx| cx.open_url(&url)))
                    });
                    let checks = summary.iter().flat_map(|summary| summary.checks.iter()).enumerate().map(|(index, check)| {
                        let url = check.url.clone();
                        div().id(("hosting-check", index)).px_2().py_1().flex().items_center().gap_2()
                            .child(div().flex_1().truncate().child(check.name.clone()))
                            .child(div().text_xs().text_color(match check.state { sift_protocol::HostingCheckState::Success => colors.success, sift_protocol::HostingCheckState::Failure => colors.danger, _ => colors.muted_text }).child(format!("{:?}", check.state)))
                            .children(url.map(|url| Button::new(("open-hosting-check", index), "Open").tone(ButtonTone::Ghost).on_click(move |_, _, cx| cx.open_url(&url))))
                    });
                    let repositories = self.hosting_repositories.iter().enumerate().map(|(index, repository)| {
                        let url = repository.identity.web_url.clone();
                        div().id(("hosting-repository", index)).px_2().py_1().flex().items_center().gap_2()
                            .child(div().flex_1().truncate().child(format!("{}/{}", repository.identity.owner, repository.identity.name)))
                            .child(div().text_xs().text_color(colors.muted_text).child(if repository.private { "private" } else { "public" }))
                            .child(Button::new(("open-hosting-repository", index), "Open").tone(ButtonTone::Ghost).on_click(move |_, _, cx| cx.open_url(&url)))
                    });
                    div().debug_selector(|| "repository-hosting".into()).w_full().max_h(px(680.)).flex().flex_col().gap_3()
                        .child(div().flex().items_center().justify_between()
                            .child(div().text_lg().font_weight(gpui::FontWeight::SEMIBOLD).child(summary.as_ref().map_or_else(|| "Repository hosting".into(), |summary| format!("{:?} · {}/{}", summary.identity.provider, summary.identity.owner, summary.identity.name))))
                            .child(Button::new("close-repository-hosting", "Close").tone(ButtonTone::Ghost).on_click(cx.listener(|shell, _, window, cx| shell.dismiss_modal(&DismissModal, window, cx)))))
                        .children(self.repository_hosting_error.clone().map(ErrorBanner::new))
                        .when(self.repository_hosting_loading, |view| view.child(div().text_sm().text_color(colors.muted_text).child("Loading hosting state…")))
                        .child(div().flex().flex_wrap().gap_2().children(links))
                        .child(div().border_t_1().border_color(colors.subtle_border).pt_2().flex().flex_col().gap_2()
                            .child(SectionLabel::new(if credential_present { "HOSTING API CREDENTIAL · CONFIGURED FOR YOU" } else { "HOSTING API CREDENTIAL · NOT CONFIGURED" }))
                            .child(self.hosting_token_input.clone())
                            .child(div().flex().gap_2()
                                .child(Button::new("save-hosting-credential", "Save / replace").tone(ButtonTone::Neutral).on_click(cx.listener(|shell, _, _, cx| shell.hosting_action("save", cx))))
                                .child(Button::new("remove-hosting-credential", "Remove").tone(ButtonTone::DangerGhost).disabled(!credential_present).on_click(cx.listener(|shell, _, _, cx| shell.hosting_action("remove", cx))))
                                .child(Button::new("load-hosting-repositories", "Browse repositories").tone(ButtonTone::Ghost).disabled(!credential_present).on_click(cx.listener(|shell, _, _, cx| shell.hosting_action("repositories", cx)))))
                            .child(div().text_xs().text_color(colors.muted_text).child("Hosting API credentials are per-principal and separate from Git fetch/push credentials.")))
                        .children((!self.hosting_repositories.is_empty()).then(|| div().id("hosting-repositories-list").max_h(px(140.)).overflow_y_scroll().children(repositories)))
                        .child(div().border_t_1().border_color(colors.subtle_border).pt_2().flex().flex_col().gap_2()
                            .child(SectionLabel::new("CREATE PULL REQUEST FROM CURRENT PUSHED BRANCH"))
                            .child(self.hosting_pr_title_input.clone())
                            .child(self.hosting_pr_base_input.clone())
                            .child(Button::new("create-hosting-pull", "Create pull request").tone(ButtonTone::Accent).disabled(!credential_present).on_click(cx.listener(|shell, _, _, cx| shell.hosting_action("create_pr", cx)))))
                        .children(summary.as_ref().map(|summary| !summary.pull_requests.is_empty()).unwrap_or(false).then(|| div().border_t_1().border_color(colors.subtle_border).pt_2().flex().flex_col().child(SectionLabel::new("PULL REQUESTS")).children(pulls)))
                        .children(summary.as_ref().map(|summary| !summary.checks.is_empty()).unwrap_or(false).then(|| div().border_t_1().border_color(colors.subtle_border).pt_2().flex().flex_col().child(SectionLabel::new("CHECKS")).children(checks)))
                        .into_any_element()
                }
                Modal::RepositoryBranches => {
                    let query = self.query_input.read(cx).text().trim().to_lowercase();
                    let branches = self.repository.branches();
                    let filtered = branches
                        .iter()
                        .filter(|branch| query.is_empty() || branch.name.to_lowercase().contains(&query))
                        .cloned()
                        .collect::<Vec<_>>();
                    let status = self.repository.status().cloned();
                    let selected_index = self.repository_modal_selected;
                    let rows = filtered.into_iter().enumerate().map(|(index, branch)| {
                        let target = branch.name.clone();
                        let rename_name = branch.name.clone();
                        let upstream_name = branch.name.clone();
                        let delete_name = branch.name.clone();
                        let current = branch.current;
                        let remote = branch.remote;
                        let upstream = branch.upstream.clone().unwrap_or_else(|| "No upstream".into());
                        let switch_status = status.clone();
                        let checkpoint_changes = switch_status
                            .as_ref()
                            .is_some_and(|status| !status.entries.is_empty());
                        div()
                            .id(("repository-branch-row", index))
                            .role(Role::ListItem)
                            .aria_label(format!("Git branch {}", branch.name))
                            .px_2().py_2().flex().items_center().gap_2()
                            .border_b_1().border_color(colors.subtle_border)
                            .when(index == selected_index, |row| row.bg(colors.active_surface))
                            .child(div().w(px(14.)).text_color(colors.accent).child(if current { "●" } else { "" }))
                            .child(div().flex_1().min_w_0().flex().flex_col()
                                .child(div().truncate().child(branch.name))
                                .child(div().text_xs().text_color(colors.muted_text).child(format!(
                                    "{} · {upstream} · ↑{} ↓{}",
                                    if remote { "remote" } else { "local" }, branch.ahead, branch.behind
                                ))))
                            .child(Button::new(("switch-repository-branch", index), if current { "Current" } else if checkpoint_changes { "Checkpoint & switch" } else { "Switch" })
                                .tone(ButtonTone::Ghost)
                                .disabled(current || remote || switch_status.is_none())
                                .on_click(cx.listener(move |shell, _, _, cx| {
                                    let Some(status) = switch_status.as_ref() else { return };
                                    let Some(workspace_id) = shell.selected_workspace_id else { return };
                                    let Some(sender) = &shell.executor_sender else { return };
                                    let _ = sender.send(ExecutorCommand::SwitchRepositoryBranch {
                                        workspace_id,
                                        binding_id: status.binding_id.0,
                                        expected_revision: status.binding_revision,
                                        target: target.clone(),
                                        detached: false,
                                        checkpoint_changes,
                                    });
                                    cx.notify();
                                })))
                            .child(Button::new(("rename-repository-branch", index), "Rename")
                                .tone(ButtonTone::Ghost)
                                .disabled(remote)
                                .on_click(cx.listener(move |shell, _, window, cx| {
                                    shell.workspace_path_input.update(cx, |input, cx| input.set_text(rename_name.clone(), cx));
                                    shell.modal = Some(Modal::RepositoryRenameBranch(rename_name.clone()));
                                    shell.workspace_path_input.read(cx).focus_handle(cx).focus(window, cx);
                                    cx.notify();
                                })))
                            .child(Button::new(("upstream-repository-branch", index), "Upstream")
                                .tone(ButtonTone::Ghost)
                                .disabled(remote)
                                .on_click(cx.listener(move |shell, _, window, cx| {
                                    shell.workspace_path_input.update(cx, |input, cx| input.set_text("", cx));
                                    shell.modal = Some(Modal::RepositorySetUpstream(upstream_name.clone()));
                                    shell.workspace_path_input.read(cx).focus_handle(cx).focus(window, cx);
                                    cx.notify();
                                })))
                            .child(Button::new(("delete-repository-branch", index), "Delete")
                                .tone(ButtonTone::DangerMuted)
                                .disabled(remote || current)
                                .on_click(cx.listener(move |shell, _, _, cx| {
                                    shell.modal = Some(Modal::ConfirmRepositoryDeleteBranch { name: delete_name.clone(), force: false });
                                    cx.notify();
                                })))
                    });
                    let create_name = self.query_input.read(cx).text().trim().to_owned();
                    let create_status = status.clone();
                    div().debug_selector(|| "repository-branches-modal".into())
                        .w_full().max_h(px(620.)).flex().flex_col().gap_3()
                        .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child("Branches"))
                        .child(self.query_input.clone())
                        .child(div().id("repository-branch-list").flex_1().min_h(px(180.)).overflow_y_scroll().border_1().border_color(colors.subtle_border).children(rows))
                        .child(div().flex().justify_between().gap_2()
                            .child(Button::new("create-filtered-repository-branch", "Create from HEAD")
                                .tone(ButtonTone::Accent)
                                .disabled(create_name.is_empty() || create_status.is_none())
                                .on_click(cx.listener(move |shell, _, _, cx| {
                                    let Some(status) = create_status.as_ref() else { return };
                                    let Some(workspace_id) = shell.selected_workspace_id else { return };
                                    let Some(sender) = &shell.executor_sender else { return };
                                    let _ = sender.send(ExecutorCommand::CreateRepositoryBranch {
                                        workspace_id,
                                        binding_id: status.binding_id.0,
                                        expected_revision: status.binding_revision,
                                        name: create_name.clone(),
                                        start: None,
                                        checkpoint_id: None,
                                    });
                                    cx.notify();
                                })))
                            .child(Button::new("close-repository-branches", "Close").tone(ButtonTone::Neutral)
                                .on_click(cx.listener(|shell, _, window, cx| shell.dismiss_modal(&DismissModal, window, cx))))
                        )
                        .into_any_element()
                }
                Modal::RepositoryRenameBranch(old) => {
                    let old = old.clone();
                    let new = self.workspace_path_input.read(cx).text().trim().to_owned();
                    let status = self.repository.status().cloned();
                    div().w_full().flex().flex_col().gap_3()
                        .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(format!("Rename {old}")))
                        .child(self.workspace_path_input.clone())
                        .child(div().flex().justify_end().gap_2()
                            .child(Button::new("cancel-rename-repository-branch", "Cancel").tone(ButtonTone::Neutral)
                                .on_click(cx.listener(|shell, _, window, cx| shell.dismiss_modal(&DismissModal, window, cx))))
                            .child(Button::new("submit-rename-repository-branch", "Rename").tone(ButtonTone::Accent)
                                .disabled(new.is_empty() || new == old || status.is_none())
                                .on_click(cx.listener(move |shell, _, _, cx| {
                                    let Some(status) = status.as_ref() else { return };
                                    let Some(workspace_id) = shell.selected_workspace_id else { return };
                                    let Some(sender) = &shell.executor_sender else { return };
                                    let _ = sender.send(ExecutorCommand::RenameRepositoryBranch {
                                        workspace_id, binding_id: status.binding_id.0,
                                        expected_revision: status.binding_revision,
                                        old: old.clone(), new: new.clone(),
                                    });
                                    cx.notify();
                                })))
                        )
                        .into_any_element()
                }
                Modal::RepositoryBranchFromCheckpoint(checkpoint_id) => {
                    let checkpoint_id = *checkpoint_id;
                    let name = self.workspace_path_input.read(cx).text().trim().to_owned();
                    let status = self.repository.status().cloned();
                    div().w_full().flex().flex_col().gap_3()
                        .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(format!("Create branch from checkpoint {}", checkpoint_id.0)))
                        .child(div().text_sm().text_color(colors.muted_text).child("Only checkpoints captured by a successful Sift commit have a Git object to branch from."))
                        .child(self.workspace_path_input.clone())
                        .child(div().flex().justify_end().gap_2()
                            .child(Button::new("cancel-branch-from-checkpoint", "Cancel").tone(ButtonTone::Neutral)
                                .on_click(cx.listener(|shell, _, window, cx| shell.dismiss_modal(&DismissModal, window, cx))))
                            .child(Button::new("submit-branch-from-checkpoint", "Create branch").tone(ButtonTone::Accent)
                                .disabled(name.is_empty() || status.is_none())
                                .on_click(cx.listener(move |shell, _, _, cx| {
                                    let Some(status) = status.as_ref() else { return };
                                    let Some(workspace_id) = shell.selected_workspace_id else { return };
                                    let Some(sender) = &shell.executor_sender else { return };
                                    let _ = sender.send(ExecutorCommand::CreateRepositoryBranch {
                                        workspace_id, binding_id: status.binding_id.0,
                                        expected_revision: status.binding_revision,
                                        name: name.clone(), start: None,
                                        checkpoint_id: Some(checkpoint_id),
                                    });
                                    cx.notify();
                                })))
                        )
                        .into_any_element()
                }
                Modal::RepositorySetUpstream(branch) => {
                    let branch = branch.clone();
                    let value = self.workspace_path_input.read(cx).text().trim().to_owned();
                    let status = self.repository.status().cloned();
                    let clear_status = status.clone();
                    let clear_branch = branch.clone();
                    div().w_full().flex().flex_col().gap_3()
                        .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(format!("Upstream for {branch}")))
                        .child(div().text_sm().text_color(colors.muted_text).child("Enter a remote branch such as origin/main, or clear the current upstream."))
                        .child(self.workspace_path_input.clone())
                        .child(div().flex().justify_end().gap_2()
                            .child(Button::new("clear-repository-upstream", "Clear upstream").tone(ButtonTone::Ghost)
                                .disabled(clear_status.is_none())
                                .on_click(cx.listener(move |shell, _, _, cx| {
                                    let Some(status) = clear_status.as_ref() else { return };
                                    let Some(workspace_id) = shell.selected_workspace_id else { return };
                                    let Some(sender) = &shell.executor_sender else { return };
                                    let _ = sender.send(ExecutorCommand::SetRepositoryUpstream {
                                        workspace_id, binding_id: status.binding_id.0,
                                        expected_revision: status.binding_revision,
                                        branch: clear_branch.clone(), upstream: None,
                                    });
                                    cx.notify();
                                })))
                            .child(Button::new("submit-repository-upstream", "Set upstream").tone(ButtonTone::Accent)
                                .disabled(value.is_empty() || status.is_none())
                                .on_click(cx.listener(move |shell, _, _, cx| {
                                    let Some(status) = status.as_ref() else { return };
                                    let Some(workspace_id) = shell.selected_workspace_id else { return };
                                    let Some(sender) = &shell.executor_sender else { return };
                                    let _ = sender.send(ExecutorCommand::SetRepositoryUpstream {
                                        workspace_id, binding_id: status.binding_id.0,
                                        expected_revision: status.binding_revision,
                                        branch: branch.clone(), upstream: Some(value.clone()),
                                    });
                                    cx.notify();
                                })))
                        )
                        .into_any_element()
                }
                Modal::ConfirmRepositoryDeleteBranch { name, force } => {
                    let name = name.clone();
                    let force = *force;
                    let status = self.repository.status().cloned();
                    div().w_full().flex().flex_col().gap_3()
                        .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(format!("Delete branch {name}?")))
                        .child(div().text_sm().text_color(if force { colors.warning } else { colors.muted_text }).child(
                            if force { "This permanently deletes an unmerged local branch. The repository reflog may be the only recovery path." }
                            else { "The first attempt only deletes a branch Git confirms is merged." }
                        ))
                        .child(div().flex().justify_end().gap_2()
                            .child(Button::new("cancel-delete-repository-branch", "Cancel").tone(ButtonTone::Neutral)
                                .on_click(cx.listener(|shell, _, window, cx| shell.dismiss_modal(&DismissModal, window, cx))))
                            .when(!force, |actions| actions.child(Button::new("force-delete-repository-branch", "Delete unmerged…").tone(ButtonTone::DangerMuted)
                                .on_click(cx.listener({ let name = name.clone(); move |shell, _, _, cx| { shell.modal = Some(Modal::ConfirmRepositoryDeleteBranch { name: name.clone(), force: true }); cx.notify(); } }))))
                            .child(Button::new("submit-delete-repository-branch", if force { "Force delete" } else { "Delete merged" }).tone(ButtonTone::DangerMuted)
                                .disabled(status.is_none())
                                .on_click(cx.listener(move |shell, _, _, cx| {
                                    let Some(status) = status.as_ref() else { return };
                                    let Some(workspace_id) = shell.selected_workspace_id else { return };
                                    let Some(sender) = &shell.executor_sender else { return };
                                    let _ = sender.send(ExecutorCommand::DeleteRepositoryBranch {
                                        workspace_id, binding_id: status.binding_id.0,
                                        expected_revision: status.binding_revision,
                                        name: name.clone(), force,
                                    });
                                    cx.notify();
                                })))
                        )
                        .into_any_element()
                }
                Modal::RepositoryHistory => {
                    let commits: Arc<[sift_protocol::VcsCommitSummary]> = self.repository.history().to_vec().into();
                    let row_count = commits.len();
                    let binding_id = self.repository.status().map(|status| status.binding_id.0);
                    let has_more = self.repository.history_cursor().is_some();
                    let loading = self.repository.history_loading();
                    let selected_index = self.repository_modal_selected;
                    div().debug_selector(|| "repository-history-modal".into())
                        .w_full().h(px(620.)).flex().flex_col().gap_3()
                        .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child("Repository history"))
                        .child(div().flex().gap_2().child(self.query_input.clone())
                            .child(Button::new("search-repository-history", "Search").tone(ButtonTone::Accent)
                                .disabled(loading)
                                .on_click(cx.listener(|shell, _, _, cx| shell.request_repository_history(false, cx)))))
                        .child(uniform_list("repository-history-list", row_count, cx.processor(move |shell, range: Range<usize>, _, cx| {
                            range.filter_map(|index| commits.get(index).cloned().map(|commit| (index, commit))).map(|(index, commit)| {
                                let oid = commit.oid.clone();
                                let compare_oid = commit.oid.clone();
                                let compare_selected = shell.repository.comparison_base() == Some(compare_oid.as_str());
                                let short = commit.oid.chars().take(8).collect::<String>();
                                let refs = commit.refs.join(", ");
                                div().id(("repository-history-row", index)).role(Role::ListItem).aria_label(format!("Commit {}: {}", commit.oid, commit.subject)).h(px(50.)).px_2().flex().items_center().gap_2()
                                    .when(index == selected_index, |row| row.bg(colors.active_surface))
                                    .border_b_1().border_color(cx.theme().colors.subtle_border)
                                    .child(div().w(px(18.)).text_color(cx.theme().colors.accent).child("●"))
                                    .child(div().flex_1().min_w_0().flex().flex_col()
                                        .child(div().truncate().child(commit.subject))
                                        .child(div().text_xs().text_color(cx.theme().colors.muted_text).truncate().child(format!("{} · {} · {}", commit.author_name, commit.authored_at, refs))))
                                    .child(div().font_family("monospace").text_xs().text_color(cx.theme().colors.muted_text).child(short))
                                    .child(Button::new(("compare-repository-commit", index), if compare_selected { "Selected" } else { "Compare" })
                                        .tone(ButtonTone::Ghost)
                                        .on_click(cx.listener(move |shell, _, _, cx| {
                                            if let Some(base) = shell.repository.comparison_base().map(str::to_owned) {
                                                let Some(workspace_id) = shell.selected_workspace_id else { return };
                                                let Some(binding_id) = binding_id else { return };
                                                let Some(sender) = &shell.executor_sender else { return };
                                                let _ = sender.send(ExecutorCommand::CompareRepositoryCommits {
                                                    workspace_id, binding_id, base, target: compare_oid.clone(),
                                                });
                                            } else {
                                                shell.repository.set_comparison_base(Some(compare_oid.clone()));
                                            }
                                            cx.notify();
                                        })))
                                    .on_click(cx.listener(move |shell, _, _, _| {
                                        let Some(workspace_id) = shell.selected_workspace_id else { return };
                                        let Some(binding_id) = binding_id else { return };
                                        let Some(sender) = &shell.executor_sender else { return };
                                        let _ = sender.send(ExecutorCommand::LoadRepositoryCommit { workspace_id, binding_id, oid: oid.clone() });
                                    }))
                            }).collect()
                        })).flex_1().min_h_0().border_1().border_color(colors.subtle_border))
                        .child(div().flex().justify_between()
                            .child(Button::new("more-repository-history", if loading { "Loading…" } else { "Load more" }).tone(ButtonTone::Ghost)
                                .disabled(loading || !has_more).on_click(cx.listener(|shell, _, _, cx| shell.request_repository_history(true, cx))))
                            .child(Button::new("close-repository-history", "Close").tone(ButtonTone::Neutral)
                                .on_click(cx.listener(|shell, _, window, cx| shell.dismiss_modal(&DismissModal, window, cx)))))
                        .into_any_element()
                }
                Modal::ChangeLedger => {
                    let ledger_count = self.change_ledger_entries.len();
                    let filter_summary = [
                        self.change_ledger_filter.tenant_id.map(|id| format!("tenant {id}")),
                        self.change_ledger_filter.connection_profile_id.map(|id| format!("connection {id}")),
                        self.change_ledger_filter.affected_object.clone(),
                        self.change_ledger_filter.git_commit.as_deref().map(|oid| format!("commit {}", oid.chars().take(8).collect::<String>())),
                    ].into_iter().flatten().collect::<Vec<_>>().join(" · ");
                    div().debug_selector(|| "change-ledger-modal".into()).w_full().h(px(690.)).flex().flex_col().gap_3()
                        .child(div().flex().items_center().justify_between()
                            .child(div().flex().flex_col()
                                .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child("Database change ledger"))
                                .child(div().text_xs().text_color(colors.muted_text).child(if filter_summary.is_empty() { "All permitted database changes".into() } else { filter_summary })))
                            .child(div().text_xs().text_color(if self.change_ledger_chain_verified { colors.success } else { colors.muted_text }).child(if self.change_ledger_chain_verified { "Hash chain verified" } else { "Verification pending" })))
                        .children(self.change_ledger_error.clone().map(|error| ErrorBanner::new(error).into_any_element()))
                        .child(div().px_2().h(px(28.)).flex().items_center().gap_3().text_xs().font_weight(gpui::FontWeight::SEMIBOLD).text_color(colors.muted_text)
                            .child(div().w(px(150.)).child("TIME")).child(div().w(px(92.)).child("EXECUTOR")).child(div().w(px(118.)).child("OPERATION"))
                            .child(div().flex_1().child("OBJECT")).child(div().w(px(70.)).child("ROWS")).child(div().w(px(74.)).child("COMMIT")).child(div().w(px(90.)).child("OUTCOME")))
                        .child(div().id("change-ledger-list").flex_1().min_h_0().flex().flex_col().border_1().border_color(colors.subtle_border)
                            .when(self.change_ledger_entries.is_empty() && !self.change_ledger_loading, |view| view.child(div().p_4().text_color(colors.muted_text).child("No matching database changes")))
                            .child(uniform_list(
                                "change-ledger-rows",
                                ledger_count,
                                cx.processor(move |shell, range: Range<usize>, _, cx| {
                                    let colors = cx.theme().colors;
                                    range
                                        .filter_map(|index| {
                                            shell.change_ledger_entries.get(index).map(|entry| (index, entry))
                                        })
                                        .map(|(index, entry)| {
                                            let object = entry.affected_object.as_deref().unwrap_or("database");
                                            let commit = entry.git_commit.as_deref().map(|oid| oid.chars().take(8).collect::<String>());
                                            div().id(("change-ledger-row", index)).h(px(34.)).px_2().flex().items_center().gap_3()
                                                .border_b_1().border_color(colors.subtle_border)
                                                .child(div().w(px(150.)).text_xs().text_color(colors.muted_text).child(entry.at.to_rfc3339()))
                                                .child(div().w(px(92.)).font_family("monospace").text_xs().child(format!("user:{}", entry.executed_by)))
                                                .child(div().w(px(118.)).child(format!("{:?}", entry.operation)))
                                                .child(div().flex_1().min_w_0().truncate().child(object.to_owned()))
                                                .child(div().w(px(70.)).text_xs().text_color(colors.muted_text).child(entry.row_count.map(|count| format!("{count} rows")).unwrap_or_default()))
                                                .child(div().w(px(74.)).font_family("monospace").text_xs().text_color(colors.muted_text).child(commit.unwrap_or_default()))
                                                .child(div().w(px(90.)).text_xs().child(format!("{:?}", entry.outcome)))
                                                .into_any_element()
                                        })
                                        .collect()
                                }),
                            ).flex_1().min_h_0().w_full().track_scroll(&self.change_ledger_scroll_handle)))
                        .child(div().p_2().rounded_sm().border_1().border_color(colors.subtle_border).flex().items_end().gap_2()
                            .child(div().w(px(150.)).flex().flex_col().gap_1()
                                .child(div().text_xs().text_color(colors.muted_text).child("RETENTION DAYS"))
                                .child(self.change_ledger_retention_input.clone()))
                            .child(div().flex_1().flex().flex_col().gap_1()
                                .child(div().text_xs().text_color(colors.muted_text).child("EXTERNAL SINK LABEL"))
                                .child(self.change_ledger_sink_input.clone()))
                            .child(Button::new("save-change-ledger-policy", "Save policy").tone(ButtonTone::Neutral)
                                .loading(self.change_ledger_policy_pending)
                                .disabled(self.change_ledger_filter.tenant_id.is_none())
                                .on_click(cx.listener(|shell, _, _, cx| shell.save_change_ledger_policy(cx)))))
                        .child(div().flex().justify_between()
                            .child(Button::new("more-change-ledger", if self.change_ledger_loading { "Loading…" } else { "Load older" }).tone(ButtonTone::Ghost)
                                .disabled(self.change_ledger_loading || self.change_ledger_next_before_id.is_none()).on_click(cx.listener(|shell, _, _, cx| shell.request_change_ledger(true, cx))))
                            .child(div().flex().gap_2()
                                .child(Button::new("export-change-ledger", "Export CSV…").tone(ButtonTone::Accent).loading(self.change_ledger_export_pending).disabled(self.change_ledger_filter.tenant_id.is_none()).on_click(cx.listener(|shell, _, _, cx| shell.prompt_change_ledger_export(cx))))
                                .child(Button::new("refresh-change-ledger", "Refresh").tone(ButtonTone::Ghost).disabled(self.change_ledger_loading).on_click(cx.listener(|shell, _, _, cx| shell.request_change_ledger(false, cx))))
                                .child(Button::new("close-change-ledger", "Close").debug_selector("close-change-ledger").tone(ButtonTone::Neutral).on_click(cx.listener(|shell, _, window, cx| shell.dismiss_modal(&DismissModal, window, cx))))))
                        .into_any_element()
                }
                Modal::RepositoryCommitDetail => {
                    let detail = self
                        .repository
                        .commit_detail()
                        .cloned()
                        .expect("commit detail modal requires loaded commit");
                    let hash = detail.commit.oid.clone();
                    let message = detail.message.clone();
                    let permalink = self.repository.status().map(|status| {
                        format!("sift://repository/{}/commit/{hash}", status.binding_id.0)
                    });
                    let status = self.repository.status().cloned();
                    let parent = detail.commit.parents.first().cloned();
                    let file_hash = hash.clone();
                    let file_status = status.clone();
                    let rows = detail.files.into_iter().enumerate().map(|(index, file)| {
                        let path = file.path.clone();
                        let oid = file_hash.clone();
                        let status = file_status.clone();
                        div()
                            .id(("repository-commit-file", index))
                            .px_2()
                            .py_1()
                            .flex()
                            .items_center()
                            .justify_between()
                            .cursor_pointer()
                            .border_b_1()
                            .border_color(colors.subtle_border)
                            .child(div().truncate().child(file.path.0))
                            .child(
                                div().text_xs().text_color(colors.muted_text).child(
                                    if file.binary {
                                        "binary".into()
                                    } else {
                                        format!(
                                            "+{} −{}",
                                            file.additions.unwrap_or(0),
                                            file.deletions.unwrap_or(0)
                                        )
                                    },
                                ),
                            )
                            .on_click(cx.listener(move |shell, _, _, _| {
                                let Some(status) = status.as_ref() else { return };
                                let Some(workspace_id) = shell.selected_workspace_id else { return };
                                let Some(sender) = &shell.executor_sender else { return };
                                let _ = sender.send(ExecutorCommand::LoadRepositoryHistoricalFile {
                                    workspace_id,
                                    binding_id: status.binding_id.0,
                                    oid: oid.clone(),
                                    path: path.clone(),
                                });
                            }))
                    });
                    let create_hash = hash.clone();
                    let create_status = status.clone();
                    let branch_name = self.query_input.read(cx).text().trim().to_owned();
                    let detached_hash = hash.clone();
                    let detached_status = status.clone();
                    let revert_hash = hash.clone();
                    let revert_status = status.clone();
                    let copy_hash = hash.clone();
                    let executions_hash = hash.clone();
                    let compare_hash = hash.clone();
                    let compare_status = status.clone();
                    div()
                        .debug_selector(|| "repository-commit-detail".into())
                        .w_full()
                        .h(px(660.))
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(detail.commit.subject),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(colors.muted_text)
                                .child(format!(
                                    "{} <{}> · {}",
                                    detail.commit.author_name,
                                    detail.commit.author_email,
                                    detail.commit.authored_at
                                )),
                        )
                        .children(detail.checkpoint_id.map(|checkpoint| {
                            div().text_xs().text_color(colors.muted_text).child(format!(
                                "Sift checkpoint {} · workspace revision {}",
                                checkpoint.0,
                                detail.workspace_revision.map(|revision| revision.0).unwrap_or_default()
                            ))
                        }))
                        .child(
                            div()
                                .p_2()
                                .bg(colors.surface)
                                .font_family("monospace")
                                .whitespace_normal()
                                .child(detail.message),
                        )
                        .child(
                            div()
                                .id("repository-commit-files")
                                .flex_1()
                                .min_h(px(160.))
                                .overflow_y_scroll()
                                .border_1()
                                .border_color(colors.subtle_border)
                                .children(rows),
                        )
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(self.query_input.clone())
                                .child(
                                    Button::new("create-branch-from-commit", "Create branch")
                                        .tone(ButtonTone::Accent)
                                        .disabled(branch_name.is_empty() || create_status.is_none())
                                        .on_click(cx.listener(move |shell, _, _, cx| {
                                            let Some(status) = create_status.as_ref() else { return };
                                            let Some(workspace_id) = shell.selected_workspace_id else { return };
                                            let Some(sender) = &shell.executor_sender else { return };
                                            let _ = sender.send(ExecutorCommand::CreateRepositoryBranch {
                                                workspace_id,
                                                binding_id: status.binding_id.0,
                                                expected_revision: status.binding_revision,
                                                name: branch_name.clone(),
                                                start: Some(create_hash.clone()),
                                                checkpoint_id: None,
                                            });
                                            cx.notify();
                                        })),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new("compare-repository-parent", "Compare parent")
                                        .tone(ButtonTone::Ghost)
                                        .disabled(parent.is_none() || compare_status.is_none())
                                        .on_click(cx.listener(move |shell, _, _, cx| {
                                            let Some(base) = parent.as_ref() else { return };
                                            let Some(status) = compare_status.as_ref() else { return };
                                            let Some(workspace_id) = shell.selected_workspace_id else { return };
                                            let Some(sender) = &shell.executor_sender else { return };
                                            let _ = sender.send(ExecutorCommand::CompareRepositoryCommits {
                                                workspace_id,
                                                binding_id: status.binding_id.0,
                                                base: base.clone(),
                                                target: compare_hash.clone(),
                                            });
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new("detach-repository-head", "Checkout detached")
                                        .tone(ButtonTone::Ghost)
                                        .disabled(detached_status.is_none())
                                        .on_click(cx.listener(move |shell, _, _, cx| {
                                            let Some(status) = detached_status.as_ref() else { return };
                                            let Some(workspace_id) = shell.selected_workspace_id else { return };
                                            let Some(sender) = &shell.executor_sender else { return };
                                            let _ = sender.send(ExecutorCommand::SwitchRepositoryBranch {
                                                workspace_id,
                                                binding_id: status.binding_id.0,
                                                expected_revision: status.binding_revision,
                                                target: detached_hash.clone(),
                                                detached: true,
                                                checkpoint_changes: !status.entries.is_empty(),
                                            });
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new("revert-repository-commit", "Revert for review")
                                        .tone(ButtonTone::DangerMuted)
                                        .disabled(revert_status.is_none())
                                        .on_click(cx.listener(move |shell, _, _, cx| {
                                            let Some(status) = revert_status.as_ref() else { return };
                                            let Some(workspace_id) = shell.selected_workspace_id else { return };
                                            let Some(sender) = &shell.executor_sender else { return };
                                            let _ = sender.send(ExecutorCommand::RevertRepositoryCommit {
                                                workspace_id,
                                                binding_id: status.binding_id.0,
                                                expected_revision: status.binding_revision,
                                                oid: revert_hash.clone(),
                                            });
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new("repository-commit-executions", "Database executions")
                                        .tone(ButtonTone::Ghost)
                                        .on_click(cx.listener(move |shell, _, window, cx| {
                                            shell.open_change_ledger(Some(executions_hash.clone()), window, cx)
                                        })),
                                )
                                .child(
                                    Button::new("copy-repository-commit-hash", "Copy hash")
                                        .tone(ButtonTone::Ghost)
                                        .on_click(cx.listener(move |shell, _, _, cx| {
                                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy_hash.clone()));
                                            shell.show_toast("Copied commit hash".into(), cx);
                                        })),
                                )
                                .child(
                                    Button::new("copy-repository-commit-message", "Copy message")
                                        .tone(ButtonTone::Ghost)
                                        .on_click(cx.listener(move |shell, _, _, cx| {
                                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(message.clone()));
                                            shell.show_toast("Copied commit message".into(), cx);
                                        })),
                                )
                                .children(permalink.map(|permalink| {
                                    Button::new("copy-repository-commit-permalink", "Copy link")
                                        .tone(ButtonTone::Ghost)
                                        .on_click(cx.listener(move |shell, _, _, cx| {
                                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(permalink.clone()));
                                            shell.show_toast("Copied commit link".into(), cx);
                                        }))
                                }))
                                .child(
                                    Button::new("close-repository-commit-detail", "Close")
                                        .debug_selector("close-repository-commit-detail")
                                        .tone(ButtonTone::Neutral)
                                        .on_click(cx.listener(|shell, _, window, cx| {
                                            shell.dismiss_modal(&DismissModal, window, cx)
                                        })),
                                ),
                        )
                        .into_any_element()
                }
                Modal::RepositoryConflict => {
                    let conflict = self
                        .repository
                        .conflict()
                        .cloned()
                        .expect("conflict modal requires loaded conflict");
                    let status = self.repository.status().cloned();
                    let region = conflict.regions.first().cloned();
                    let region_id = region
                        .as_ref()
                        .map(|region| region.id.clone())
                        .unwrap_or_default();
                    let manual_ready = self.manual_conflict_path.as_ref() == Some(&conflict.path);
                    let manual_opened = manual_ready && self.manual_conflict_opened;
                    let manual_dirty = self.repository_conflict_editor_dirty(&conflict.path, cx);
                    let ours_conflict = conflict.clone();
                    let ours_status = status.clone();
                    let ours_region_id = region_id.clone();
                    let theirs_conflict = conflict.clone();
                    let theirs_status = status.clone();
                    let theirs_region_id = region_id.clone();
                    let both_conflict = conflict.clone();
                    let both_status = status.clone();
                    let both_region_id = region_id.clone();
                    let manual_conflict = conflict.clone();
                    let manual_status = status.clone();
                    let open_manual_path = conflict.path.clone();
                    let mark_conflict = conflict.clone();
                    let mark_status = status.clone();
                    let side = |id: &'static str,
                                title: &'static str,
                                text: Option<String>,
                                color: gpui::Hsla| {
                        div()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .flex()
                            .flex_col()
                            .border_1()
                            .border_color(color)
                            .rounded_sm()
                            .child(
                                div()
                                    .px_2()
                                    .py_1()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(color)
                                    .child(title),
                            )
                            .child(
                                div()
                                    .id(id)
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .p_2()
                                    .bg(colors.surface)
                                    .font_family("monospace")
                                    .text_xs()
                                    .whitespace_normal()
                                    .child(text.unwrap_or_else(|| "∅ no version on this side".into())),
                            )
                    };
                    div()
                        .debug_selector(|| "repository-conflict-modal".into())
                        .w_full()
                        .h(px(650.))
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .child(
                                            div()
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .child(format!("Resolve {}", conflict.path.0)),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(colors.muted_text)
                                                .child(format!("{:?} · index stages 1/2/3", conflict.kind)),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .gap_1()
                                        .child(
                                            Button::new("previous-modal-conflict", "Previous")
                                                .tone(ButtonTone::Ghost)
                                                .on_click(cx.listener(|shell, _, _, cx| {
                                                    shell.navigate_repository_conflict(-1, cx)
                                                })),
                                        )
                                        .child(
                                            Button::new("next-modal-conflict", "Next")
                                                .tone(ButtonTone::Ghost)
                                                .on_click(cx.listener(|shell, _, _, cx| {
                                                    shell.navigate_repository_conflict(1, cx)
                                                })),
                                        ),
                                ),
                        )
                        .when(conflict.binary, |panel| {
                            panel.child(
                                div()
                                    .p_3()
                                    .border_1()
                                    .border_color(colors.warning)
                                    .text_color(colors.warning)
                                    .child("Binary conflict. Choose ours or theirs; combined and inline views are unavailable."),
                            )
                        })
                        .when_some(region, |panel, region| {
                            panel.child(
                                div()
                                    .flex_1()
                                    .min_h_0()
                                    .flex()
                                    .gap_2()
                                    .child(side(
                                        "repository-conflict-base",
                                        "BASE",
                                        region.base,
                                        colors.muted_text,
                                    ))
                                    .child(side(
                                        "repository-conflict-ours",
                                        "OURS",
                                        region.ours,
                                        colors.success,
                                    ))
                                    .child(side(
                                        "repository-conflict-theirs",
                                        "THEIRS",
                                        region.theirs,
                                        colors.warning,
                                    )),
                            )
                        })
                        .child(
                            div()
                                .text_xs()
                                .text_color(colors.muted_text)
                                .child("Resolution is revision-guarded and creates a workspace checkpoint before changing the shared worktree."),
                        )
                        .when(manual_ready, |panel| {
                            panel.child(
                                div()
                                    .text_xs()
                                    .text_color(if manual_dirty {
                                        colors.warning
                                    } else {
                                        colors.success
                                    })
                                    .child(if !manual_opened {
                                        "Checkpoint ready. Open the workspace file, edit it, and save before marking resolved."
                                    } else if manual_dirty {
                                        "Manual resolution has unsaved edits. Save the file before marking it resolved."
                                    } else {
                                        "Manual resolution editor is saved and ready to mark resolved."
                                    }),
                            )
                        })
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    Button::new("use-ours-conflict", "Use ours")
                                        .tone(ButtonTone::Accent)
                                        .disabled(status.is_none())
                                        .on_click(cx.listener(move |shell, _, _, _cx| {
                                            let Some(status) = ours_status.as_ref() else { return };
                                            let Some(workspace_id) = shell.selected_workspace_id else { return };
                                            let Some(sender) = &shell.executor_sender else { return };
                                            let _ = sender.send(ExecutorCommand::ResolveRepositoryConflict {
                                                workspace_id,
                                                binding_id: status.binding_id.0,
                                                expected_revision: status.binding_revision,
                                                path: ours_conflict.path.clone(),
                                                region_id: ours_region_id.clone(),
                                                resolution: sift_protocol::VcsConflictResolution::Ours,
                                            });
                                        })),
                                )
                                .child(
                                    Button::new("use-theirs-conflict", "Use theirs")
                                        .tone(ButtonTone::Accent)
                                        .disabled(status.is_none())
                                        .on_click(cx.listener(move |shell, _, _, _cx| {
                                            let Some(status) = theirs_status.as_ref() else { return };
                                            let Some(workspace_id) = shell.selected_workspace_id else { return };
                                            let Some(sender) = &shell.executor_sender else { return };
                                            let _ = sender.send(ExecutorCommand::ResolveRepositoryConflict {
                                                workspace_id,
                                                binding_id: status.binding_id.0,
                                                expected_revision: status.binding_revision,
                                                path: theirs_conflict.path.clone(),
                                                region_id: theirs_region_id.clone(),
                                                resolution: sift_protocol::VcsConflictResolution::Theirs,
                                            });
                                        })),
                                )
                                .child(
                                    Button::new("use-both-conflict", "Use both")
                                        .tone(ButtonTone::Ghost)
                                        .disabled(conflict.binary || status.is_none())
                                        .on_click(cx.listener(move |shell, _, _, _cx| {
                                            let Some(status) = both_status.as_ref() else { return };
                                            let Some(workspace_id) = shell.selected_workspace_id else { return };
                                            let Some(sender) = &shell.executor_sender else { return };
                                            let _ = sender.send(ExecutorCommand::ResolveRepositoryConflict {
                                                workspace_id,
                                                binding_id: status.binding_id.0,
                                                expected_revision: status.binding_revision,
                                                path: both_conflict.path.clone(),
                                                region_id: both_region_id.clone(),
                                                resolution: sift_protocol::VcsConflictResolution::Both,
                                            });
                                        })),
                                )
                                .child(
                                    Button::new(
                                        "manual-conflict",
                                        if manual_ready { "Open manual editor" } else { "Edit manually" },
                                    )
                                    .tone(ButtonTone::Ghost)
                                    .disabled(status.is_none())
                                    .on_click(cx.listener(move |shell, _, window, cx| {
                                        if shell.manual_conflict_path.as_ref() == Some(&open_manual_path) {
                                            shell.repository.select_path(open_manual_path.clone());
                                            shell.manual_conflict_opened = true;
                                            shell.open_selected_repository_file(window, cx);
                                            return;
                                        }
                                        let Some(status) = manual_status.as_ref() else { return };
                                        let Some(workspace_id) = shell.selected_workspace_id else { return };
                                        let Some(sender) = &shell.executor_sender else { return };
                                        let _ = sender.send(ExecutorCommand::BeginManualRepositoryConflict {
                                            workspace_id,
                                            binding_id: status.binding_id.0,
                                            expected_revision: status.binding_revision,
                                            path: manual_conflict.path.clone(),
                                        });
                                    })),
                                )
                                .when(manual_ready, |actions| {
                                    actions.child(
                                        Button::new("mark-conflict-resolved", "Mark resolved")
                                            .tone(ButtonTone::Accent)
                                            .disabled(!manual_opened || manual_dirty || status.is_none())
                                            .on_click(cx.listener(move |shell, _, _, _cx| {
                                                let Some(status) = mark_status.as_ref() else { return };
                                                let Some(workspace_id) = shell.selected_workspace_id else { return };
                                                let Some(sender) = &shell.executor_sender else { return };
                                                let _ = sender.send(ExecutorCommand::MarkRepositoryConflictResolved {
                                                    workspace_id,
                                                    binding_id: status.binding_id.0,
                                                    expected_revision: status.binding_revision,
                                                    path: mark_conflict.path.clone(),
                                                });
                                            })),
                                    )
                                })
                                .child(div().flex_1())
                                .child(
                                    Button::new("close-repository-conflict", "Close")
                                        .tone(ButtonTone::Neutral)
                                        .on_click(cx.listener(|shell, _, window, cx| {
                                            shell.dismiss_modal(&DismissModal, window, cx)
                                        })),
                                ),
                        )
                        .into_any_element()
                }
                Modal::RepositoryComparison => {
                    let diff = self.repository.comparison().cloned()
                        .expect("comparison modal requires loaded diff");
                    let base = diff.base_revision.as_deref().unwrap_or("base");
                    let target = diff.target_revision.as_deref().unwrap_or("target");
                    let rows = diff.files.into_iter().enumerate().map(|(index, file)| {
                        div().id(("repository-comparison-file", index)).px_2().py_1().flex().items_center().justify_between()
                            .border_b_1().border_color(colors.subtle_border)
                            .child(div().truncate().child(file.path.0))
                            .child(div().text_xs().text_color(colors.muted_text).child(format!("+{} −{}", file.additions, file.deletions)))
                    });
                    div().w_full().h(px(560.)).flex().flex_col().gap_3()
                        .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(format!("Compare {} → {}", &base[..8.min(base.len())], &target[..8.min(target.len())])))
                        .child(div().id("repository-comparison-files").flex_1().min_h_0().overflow_y_scroll().border_1().border_color(colors.subtle_border).children(rows))
                        .child(div().flex().justify_end().child(Button::new("close-repository-comparison", "Close").tone(ButtonTone::Neutral)
                            .on_click(cx.listener(|shell, _, window, cx| shell.dismiss_modal(&DismissModal, window, cx)))))
                        .into_any_element()
                }
                Modal::RepositoryHistoricalFile => {
                    let file = self
                        .repository
                        .historical_file()
                        .cloned()
                        .expect("historical file modal requires loaded file");
                    let restore_file = file.clone();
                    let status = self.repository.status().cloned();
                    let file_permalink = status.as_ref().map(|status| format!(
                        "sift://repository/{}/commit/{}/file/{}",
                        status.binding_id.0, file.commit, file.path.0
                    ));
                    div()
                        .debug_selector(|| "repository-historical-file".into())
                        .w_full()
                        .h(px(620.))
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(format!("{} at {}", file.path.0, &file.commit[..8])),
                        )
                        .child(
                            div()
                                .id("repository-historical-file-text")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .p_2()
                                .bg(colors.surface)
                                .font_family("monospace")
                                .whitespace_normal()
                                .child(file.text),
                        )
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .children(file_permalink.map(|permalink| {
                                    Button::new("copy-repository-file-permalink", "Copy link")
                                        .tone(ButtonTone::Ghost)
                                        .on_click(cx.listener(move |shell, _, _, cx| {
                                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(permalink.clone()));
                                            shell.show_toast("Copied file link".into(), cx);
                                        }))
                                }))
                                .child(
                                    Button::new("restore-repository-historical-file", "Restore file")
                                        .tone(ButtonTone::DangerMuted)
                                        .disabled(status.is_none() || restore_file.truncated)
                                        .on_click(cx.listener(move |shell, _, _, cx| {
                                            let Some(status) = status.as_ref() else { return };
                                            let Some(workspace_id) = shell.selected_workspace_id else { return };
                                            let Some(sender) = &shell.executor_sender else { return };
                                            let _ = sender.send(ExecutorCommand::RestoreRepositoryHistoricalFile {
                                                workspace_id,
                                                binding_id: status.binding_id.0,
                                                expected_revision: status.binding_revision,
                                                oid: restore_file.commit.clone(),
                                                path: restore_file.path.clone(),
                                            });
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new("close-repository-historical-file", "Close")
                                        .tone(ButtonTone::Neutral)
                                        .on_click(cx.listener(|shell, _, window, cx| {
                                            shell.dismiss_modal(&DismissModal, window, cx)
                                        })),
                                ),
                        )
                        .into_any_element()
                }
                Modal::WorkspaceCheckpoint => div()
                    .debug_selector(|| "workspace-checkpoint".into())
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Create named checkpoint"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(colors.muted_text)
                            .child("Captures the workspace tree and every canonical SQL document frontier."),
                    )
                    .child(self.workspace_path_input.clone())
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new("cancel-workspace-checkpoint", "Cancel")
                                    .tone(ButtonTone::Neutral)
                                    .on_click(cx.listener(|shell, _, window, cx| {
                                        shell.dismiss_modal(&DismissModal, window, cx)
                                    })),
                            )
                            .child(
                                Button::new("submit-workspace-checkpoint", "Create checkpoint")
                                    .tone(ButtonTone::Accent)
                                    .disabled(
                                        self.workspace_path_input.read(cx).text().trim().is_empty()
                                            || self.workspace_files.mutation_pending(),
                                    )
                                    .on_click(cx.listener(|shell, _, _, cx| {
                                        shell.create_named_workspace_checkpoint(cx)
                                    })),
                            ),
                    )
                    .into_any_element(),
                Modal::WorkspaceHistory => {
                    let checkpoints = self
                        .workspace_files
                        .snapshot()
                        .map(|snapshot| snapshot.checkpoints.clone())
                        .unwrap_or_default();
                    let rows = checkpoints.into_iter().enumerate().map(|(index, checkpoint)| {
                        let checkpoint_id = checkpoint.id;
                        div()
                            .id(("workspace-checkpoint-row", index))
                            .px_2()
                            .py_1()
                            .flex()
                            .items_center()
                            .gap_2()
                            .border_b_1()
                            .border_color(colors.subtle_border)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div().truncate().child(
                                            checkpoint
                                                .name
                                                .clone()
                                                .unwrap_or_else(|| format!("{:?}", checkpoint.reason)),
                                        ),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(colors.muted_text)
                                            .child(format!(
                                                "revision {} · actor {} · {}",
                                                checkpoint.workspace_revision.0,
                                                checkpoint.created_by,
                                                checkpoint.created_at
                                            )),
                                    ),
                            )
                            .child(
                                Button::new(("branch-workspace-checkpoint", index), "Branch")
                                    .tone(ButtonTone::Ghost)
                                    .on_click(cx.listener(move |shell, _, window, cx| {
                                        shell.workspace_path_input.update(cx, |input, cx| {
                                            input.set_text("", cx)
                                        });
                                        shell.modal = Some(
                                            Modal::RepositoryBranchFromCheckpoint(checkpoint_id),
                                        );
                                        shell
                                            .workspace_path_input
                                            .read(cx)
                                            .focus_handle(cx)
                                            .focus(window, cx);
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new(("restore-workspace-checkpoint", index), "Restore")
                                    .tone(ButtonTone::Ghost)
                                    .disabled(self.workspace_files.mutation_pending())
                                    .on_click(cx.listener(move |shell, _, _, cx| {
                                        shell.modal =
                                            Some(Modal::ConfirmWorkspaceRestore(checkpoint_id));
                                        cx.notify();
                                    })),
                            )
                    });
                    div()
                        .debug_selector(|| "workspace-history".into())
                        .w_full()
                        .max_h(px(620.))
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("Workspace checkpoint history"),
                        )
                        .child(
                            div()
                                .id("workspace-history-scroll")
                                .flex_1()
                                .min_h(px(180.))
                                .overflow_y_scroll()
                                .border_1()
                                .border_color(colors.subtle_border)
                                .children(rows),
                        )
                        .child(
                            div().flex().justify_end().child(
                                Button::new("close-workspace-history", "Close")
                                    .tone(ButtonTone::Neutral)
                                    .on_click(cx.listener(|shell, _, window, cx| {
                                        shell.dismiss_modal(&DismissModal, window, cx)
                                    })),
                            ),
                        )
                        .into_any_element()
                }
                Modal::ConfirmWorkspaceRestore(checkpoint_id) => {
                    let checkpoint_id = *checkpoint_id;
                    div()
                        .debug_selector(|| "confirm-workspace-restore".into())
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(format!("Restore checkpoint {}?", checkpoint_id.0)),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(colors.muted_text)
                                .child("Restore creates a new audited workspace head; it does not rewrite checkpoint history."),
                        )
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new("cancel-workspace-restore", "Cancel")
                                        .tone(ButtonTone::Neutral)
                                        .on_click(cx.listener(|shell, _, window, cx| {
                                            shell.dismiss_modal(&DismissModal, window, cx)
                                        })),
                                )
                                .child(
                                    Button::new("submit-workspace-restore", "Restore as new head")
                                        .tone(ButtonTone::DangerMuted)
                                        .disabled(self.workspace_files.mutation_pending())
                                        .on_click(cx.listener(move |shell, _, _, cx| {
                                            shell.restore_workspace_checkpoint(checkpoint_id, cx)
                                        })),
                                ),
                        )
                        .into_any_element()
                }
                Modal::VaultItemDetails => {
                    let item = self
                        .vault_detail_item_id
                        .and_then(|id| self.vault_items.iter().find(|item| item.id.0 == id));
                    let capabilities = self
                        .selected_vault()
                        .map(|vault| vault.effective_capabilities)
                        .unwrap_or_default();
                    let can_reveal = item.is_some_and(|item| {
                        item.kind.revealable()
                            && item.secret_status
                                == sift_api_types::VaultSecretStatus::Configured
                            && capabilities.reveal
                    });
                    let can_edit = capabilities.edit;
                    let can_manage = capabilities.manage;
                    let is_connection = item
                        .is_some_and(|item| item.kind == sift_protocol::VaultItemKind::Connection);
                    let secret_status = item
                        .map(|item| format!("{:?}", item.secret_status))
                        .unwrap_or_else(|| "Unavailable".into());
                    let head_version = item.map_or(0, |item| item.head_version);
                    let title = item
                        .map(|item| item.label.clone())
                        .unwrap_or_else(|| "Vault item".into());
                    let metadata = item
                        .and_then(|item| serde_json::to_string_pretty(&item.metadata).ok())
                        .unwrap_or_default();
                    let version_rows = self.vault_item_versions.iter().enumerate().map(|(index, version)| {
                        let version_number = version.version;
                        div()
                            .id(("vault-version-row", index))
                            .px_2()
                            .py_1()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(div().flex_1().min_w_0().child(format!(
                                "v{} · {}",
                                version.version, version.change_summary
                            )))
                            .child(div().text_xs().text_color(colors.muted_text).child(
                                version.created_at.to_string(),
                            ))
                            .when(can_edit && version.version != head_version, |row| {
                                row.child(
                                    Button::new(("restore-vault-version", index), "Restore")
                                        .tone(ButtonTone::Ghost)
                                        .disabled(self.vault_loading)
                                        .on_click(cx.listener(move |shell, _, _, cx| {
                                            shell.restore_vault_item_version(version_number, cx)
                                        })),
                                )
                            })
                    });
                    let grant_rows = self.vault_grants.iter().enumerate().map(|(index, grant)| {
                        let principal_id = grant.principal_id.0;
                        let revision = grant.revision;
                        let mut names = Vec::new();
                        if grant.capabilities.use_secret { names.push("use"); }
                        if grant.capabilities.reveal { names.push("reveal"); }
                        if grant.capabilities.edit { names.push("edit"); }
                        if grant.capabilities.manage { names.push("manage"); }
                        div()
                            .id(("vault-grant-row", index))
                            .px_2()
                            .py_1()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().flex_1().child(format!(
                                "Principal {principal_id} · {}",
                                names.join(", ")
                            )))
                            .when(can_manage, |row| {
                                row.child(
                                    Button::new(("revoke-vault-grant", index), "Revoke")
                                        .tone(ButtonTone::DangerMuted)
                                        .disabled(self.vault_loading)
                                        .on_click(cx.listener(move |shell, _, _, cx| {
                                            shell.revoke_vault_grant(principal_id, revision, cx)
                                        })),
                                )
                            })
                    });
                    let section_tabs = VaultEditorSection::ALL.into_iter().enumerate().map(
                        |(index, section)| {
                            Button::new(("vault-editor-section", index), section.label())
                                .tone(if self.vault_editor_section == section {
                                    ButtonTone::Accent
                                } else {
                                    ButtonTone::Ghost
                                })
                                .on_click(cx.listener(move |shell, _, _, cx| {
                                    shell.vault_editor_section = section;
                                    cx.notify();
                                }))
                        },
                    );
                    let connection_capabilities = sift_protocol::VaultCapabilities {
                        inspect: true,
                        use_secret: true,
                        ..Default::default()
                    };
                    let reader_capabilities = sift_protocol::VaultCapabilities {
                        inspect: true,
                        reveal: true,
                        ..Default::default()
                    };
                    let editor_capabilities = sift_protocol::VaultCapabilities {
                        inspect: true,
                        use_secret: true,
                        edit: true,
                        ..Default::default()
                    };
                    let owner_capabilities = sift_protocol::VaultCapabilities::OWNER;
                    let secret_panel = div()
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .justify_between()
                                .child(SectionLabel::new("SECRET STATUS"))
                                .child(secret_status),
                        )
                        .when(is_connection, |section| {
                            section.child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .child(
                                        Button::new("test-vault-connection", "Test connection")
                                            .tone(ButtonTone::Accent)
                                            .disabled(self.vault_loading)
                                            .on_click(cx.listener(|shell, _, _, cx| {
                                                shell.test_vault_detail_connection(cx)
                                            })),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(colors.muted_text)
                                            .child("Credential edits stay in Connections."),
                                    ),
                            )
                        })
                        .when(!is_connection && can_edit, |section| {
                            section
                                .child(self.vault_editor_secret_input.clone())
                                .child(
                                    div()
                                        .flex()
                                        .gap_2()
                                        .child(
                                            Button::new("rotate-vault-secret", "Set / replace")
                                                .tone(ButtonTone::Accent)
                                                .disabled(self.vault_loading)
                                                .on_click(cx.listener(|shell, _, _, cx| {
                                                    shell.rotate_vault_item_secret(cx)
                                                })),
                                        )
                                        .child(
                                            Button::new("clear-vault-secret", "Clear")
                                                .tone(ButtonTone::DangerMuted)
                                                .disabled(self.vault_loading)
                                                .on_click(cx.listener(|shell, _, _, cx| {
                                                    shell.clear_vault_item_secret(cx)
                                                })),
                                        ),
                                )
                        })
                        .when(can_reveal, |section| {
                            section
                                .child(SectionLabel::new("TEMPORARY REVEAL"))
                                .child(self.vault_reveal_password_input.clone())
                                .child(
                                    div()
                                        .flex()
                                        .gap_2()
                                        .child(
                                            Button::new("reveal-vault-item", "Reveal for 30s")
                                                .tone(ButtonTone::Accent)
                                                .disabled(self.vault_loading)
                                                .on_click(cx.listener(|shell, _, _, cx| {
                                                    shell.reveal_selected_vault_item(cx)
                                                })),
                                        )
                                        .when(
                                            self.vault_revealed_value.is_some(),
                                            |actions| {
                                                actions.child(
                                                    Button::new("copy-vault-value", "Copy")
                                                        .tone(ButtonTone::Neutral)
                                                        .on_click(cx.listener(
                                                            |shell, _, _, cx| {
                                                                shell
                                                                    .copy_revealed_vault_value(cx)
                                                            },
                                                        )),
                                                )
                                            },
                                        ),
                                )
                        })
                        .when_some(self.vault_revealed_value.clone(), |section, value| {
                            section.child(
                                div()
                                    .p_2()
                                    .rounded_sm()
                                    .border_1()
                                    .border_color(colors.accent)
                                    .bg(colors.surface)
                                    .child(value),
                            )
                        });
                    let access_panel = div()
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .text_sm()
                                .text_color(colors.muted_text)
                                .child("Capabilities are independent. Edit does not imply reveal; tenant admins recover manage only."),
                        )
                        .when(can_manage, |section| {
                            section
                                .child(self.vault_grant_principal_input.clone())
                                .child(
                                    div()
                                        .flex()
                                        .gap_1()
                                        .child(Button::new("grant-vault-use", "Connection user").tone(ButtonTone::Ghost).on_click(cx.listener(move |shell, _, _, cx| shell.set_vault_grant_preset(connection_capabilities, cx))))
                                        .child(Button::new("grant-vault-reader", "Secret reader").tone(ButtonTone::Ghost).on_click(cx.listener(move |shell, _, _, cx| shell.set_vault_grant_preset(reader_capabilities, cx))))
                                        .child(Button::new("grant-vault-editor", "Editor").tone(ButtonTone::Ghost).on_click(cx.listener(move |shell, _, _, cx| shell.set_vault_grant_preset(editor_capabilities, cx))))
                                        .child(Button::new("grant-vault-owner", "Owner").tone(ButtonTone::Ghost).on_click(cx.listener(move |shell, _, _, cx| shell.set_vault_grant_preset(owner_capabilities, cx)))),
                                )
                        })
                        .child(
                            div()
                                .id("vault-grants-scroll")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .border_1()
                                .border_color(colors.subtle_border)
                                .children(grant_rows),
                        );
                    div()
                        .debug_selector(|| "vault-item-details".into())
                        .key_context("SiftVaultEditor")
                        .on_key_down(cx.listener(Self::handle_vault_editor_key))
                        .w_full()
                        .h(px(600.))
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child(title),
                                )
                                .when(self.navigation_hints_visible(), |header| {
                                    header.child(
                                        div()
                                            .text_xs()
                                            .text_color(colors.muted_text)
                                            .child("h/l or 1–4 switches sections"),
                                    )
                                }),
                        )
                        .child(div().flex().gap_1().children(section_tabs))
                        .children(self.vault_error.as_ref().map(|message| {
                            ErrorBanner::new(message.clone())
                        }))
                        .when(self.vault_editor_section == VaultEditorSection::Overview, |details| {
                            details.child(
                                div().flex_1().min_h_0().flex().flex_col().gap_3()
                                    .child(SectionLabel::new("NON-SECRET METADATA"))
                                    .child(self.vault_editor_label_input.clone())
                                    .when(!is_connection && item.is_some_and(|item| item.kind != sift_protocol::VaultItemKind::SecureNote), |section| {
                                        section.child(self.vault_editor_detail_input.clone())
                                    })
                                    .child(div().p_2().rounded_sm().bg(colors.surface).text_sm().child(metadata))
                                    .child(div().flex().justify_between().child(
                                        Button::new("delete-vault-item", "Delete item")
                                            .tone(ButtonTone::DangerMuted)
                                            .disabled(!can_edit || self.vault_loading)
                                            .on_click(cx.listener(|shell, _, _, cx| shell.delete_vault_detail_item(cx))),
                                    ).child(
                                        Button::new("save-vault-overview", "Save overview")
                                            .tone(ButtonTone::Accent)
                                            .disabled(!can_edit || self.vault_loading)
                                            .on_click(cx.listener(|shell, _, _, cx| shell.submit_vault_overview_update(cx))),
                                    ))
                            )
                        })
                        .when(self.vault_editor_section == VaultEditorSection::Secret, |details| {
                            details.child(secret_panel)
                        })
                        .when(self.vault_editor_section == VaultEditorSection::Access, |details| {
                            details.child(access_panel)
                        })
                        .when(self.vault_editor_section == VaultEditorSection::History, |details| details.child(
                            div().id("vault-version-history").flex_1().min_h_0().overflow_y_scroll().border_1().border_color(colors.subtle_border).children(version_rows)
                        ))
                        .child(
                            div().flex().justify_between()
                            .child(div().text_xs().text_color(colors.muted_text).child("s save · r rotate · c clear · t test · Esc close"))
                            .child(
                                Button::new("close-vault-details", "Close")
                                    .tone(ButtonTone::Neutral)
                                    .on_click(cx.listener(|shell, _, window, cx| {
                                        shell.dismiss_modal(&DismissModal, window, cx)
                                    })),
                            ),
                        )
                        .into_any_element()
                }
                Modal::CreateVault => self.render_vault_form(false, max_card_height, cx).into_any_element(),
                Modal::EditVault => self.render_vault_form(true, max_card_height, cx).into_any_element(),
                Modal::CreateVaultItem => div()
                    .debug_selector(|| "create-vault-item".into())
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Store secret"),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_1()
                            .children(VaultItemDraftKind::ALL.into_iter().enumerate().map(|(index, kind)| {
                                Button::new(("vault-item-kind", index), kind.label())
                                    .tone(if self.vault_item_draft_kind == kind {
                                        ButtonTone::Accent
                                    } else {
                                        ButtonTone::Ghost
                                    })
                                    .on_click(cx.listener(move |shell, _, _, cx| {
                                        shell.vault_item_draft_kind = kind;
                                        cx.notify();
                                    }))
                            })),
                    )
                    .child(self.vault_item_label_input.clone())
                    .when(
                        self.vault_item_draft_kind != VaultItemDraftKind::SecureNote,
                        |form| form.child(self.vault_item_detail_input.clone()),
                    )
                    .child(self.vault_item_secret_input.clone())
                    .child(
                        div()
                            .text_xs()
                            .text_color(colors.muted_text)
                            .child("Secret values are stored outside metadata and never shown in this list."),
                    )
                    .children(self.vault_error.as_ref().map(|message| {
                        ErrorBanner::new(message.clone())
                    }))
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new("cancel-create-vault-item", "Cancel")
                                    .tone(ButtonTone::Neutral)
                                    .on_click(cx.listener(|shell, _, window, cx| {
                                        shell.dismiss_modal(&DismissModal, window, cx)
                                    })),
                            )
                            .child(
                                Button::new("submit-create-vault-item", "Store")
                                    .tone(ButtonTone::Accent)
                                    .disabled(self.vault_loading)
                                    .on_click(cx.listener(|shell, _, _, cx| {
                                        shell.submit_create_vault_item(cx)
                                    })),
                            ),
                    )
                    .into_any_element(),
                Modal::WorkspaceReconcile => {
                    let changed = self
                        .workspace_files
                        .snapshot()
                        .and_then(|snapshot| snapshot.reconcile_plan.as_ref())
                        .map(|plan| {
                            plan.entries
                                .iter()
                                .filter(|entry| {
                                    entry.state != sift_protocol::ReconcileState::Unchanged
                                })
                                .cloned()
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    let rows = changed.iter().enumerate().map(|(index, entry)| {
                        let path = entry.path.clone();
                        let import_path = path.clone();
                        let materialize_path = path.clone();
                        let keep_both_path = path.clone();
                        let selected = self
                            .workspace_reconcile_resolutions
                            .get(&path)
                            .copied();
                        div()
                            .id(("workspace-reconcile-row", index))
                            .px_2()
                            .py_2()
                            .flex()
                            .items_center()
                            .gap_2()
                            .border_b_1()
                            .border_color(colors.subtle_border)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .child(div().truncate().child(path.0.clone()))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(colors.muted_text)
                                            .child(format!("{:?}", entry.state)),
                                    ),
                            )
                            .child(
                                Button::new(("reconcile-import", index), "Use filesystem")
                                    .tone(if selected
                                        == Some(sift_protocol::ReconcileResolution::ImportProjection)
                                    {
                                        ButtonTone::Accent
                                    } else {
                                        ButtonTone::Ghost
                                    })
                                    .on_click(cx.listener(move |shell, _, _, cx| {
                                        shell.workspace_reconcile_resolutions.insert(
                                            import_path.clone(),
                                            sift_protocol::ReconcileResolution::ImportProjection,
                                        );
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new(("reconcile-materialize", index), "Use virtual")
                                    .tone(if selected
                                        == Some(sift_protocol::ReconcileResolution::MaterializeWorkspace)
                                    {
                                        ButtonTone::Accent
                                    } else {
                                        ButtonTone::Ghost
                                    })
                                    .on_click(cx.listener(move |shell, _, _, cx| {
                                        shell.workspace_reconcile_resolutions.insert(
                                            materialize_path.clone(),
                                            sift_protocol::ReconcileResolution::MaterializeWorkspace,
                                        );
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new(("reconcile-keep-both", index), "Keep both")
                                    .tone(if selected
                                        == Some(sift_protocol::ReconcileResolution::KeepBoth)
                                    {
                                        ButtonTone::Accent
                                    } else {
                                        ButtonTone::Ghost
                                    })
                                    .disabled(
                                        entry.workspace_digest.is_none()
                                            || entry.projection_digest.is_none(),
                                    )
                                    .on_click(cx.listener(move |shell, _, _, cx| {
                                        shell.workspace_reconcile_resolutions.insert(
                                            keep_both_path.clone(),
                                            sift_protocol::ReconcileResolution::KeepBoth,
                                        );
                                        cx.notify();
                                    })),
                            )
                    });
                    let all_resolved = changed.iter().all(|entry| {
                        self.workspace_reconcile_resolutions.contains_key(&entry.path)
                    });
                    div()
                        .debug_selector(|| "workspace-reconcile".into())
                        .w_full()
                        .h(gpui::relative(0.78))
                        .max_h(px(760.))
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("Reconcile virtual workspace and filesystem"),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(colors.muted_text)
                                .child("Both-changed entries have no default. Choose explicitly; Sift captures a checkpoint before applying the complete plan."),
                        )
                        .child(
                            div()
                                .id("workspace-reconcile-scroll")
                                .flex_1()
                                .min_h(px(180.))
                                .overflow_y_scroll()
                                .border_1()
                                .border_color(colors.subtle_border)
                                .children(rows),
                        )
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new("cancel-workspace-reconcile", "Cancel")
                                        .tone(ButtonTone::Neutral)
                                        .on_click(cx.listener(|shell, _, window, cx| {
                                            shell.dismiss_modal(&DismissModal, window, cx)
                                        })),
                                )
                                .child(
                                    Button::new("apply-workspace-reconcile", "Apply plan")
                                        .tone(ButtonTone::Accent)
                                        .disabled(
                                            !all_resolved
                                                || changed.is_empty()
                                                || self.workspace_files.mutation_pending(),
                                        )
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.apply_workspace_reconcile(cx)
                                        })),
                                ),
                        )
                        .into_any_element()
                }
                Modal::ConfirmRepositoryDiscard(path) => {
                    let path = path.clone();
                    let confirm_path = path.clone();
                    div()
                        .debug_selector(|| "confirm-repository-discard".into())
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(format!("Discard worktree change to {}?", path.0)),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(colors.muted_text)
                                .child("Sift will capture a recovery checkpoint, restore the tracked file from the Git index, then import that exact text into the canonical room document."),
                        )
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new("cancel-repository-discard", "Cancel")
                                        .tone(ButtonTone::Neutral)
                                        .on_click(cx.listener(|shell, _, window, cx| {
                                            shell.dismiss_modal(&DismissModal, window, cx)
                                        })),
                                )
                                .child(
                                    Button::new("confirm-repository-discard", "Discard")
                                        .tone(ButtonTone::DangerMuted)
                                        .disabled(self.repository.loading())
                                        .on_click(cx.listener(move |shell, _, _, cx| {
                                            shell.discard_repository_path(confirm_path.clone(), cx)
                                        })),
                                ),
                        )
                        .into_any_element()
                }
                Modal::ConfirmRepositoryHunkRevert { path, hunk_id } => {
                    let path = path.clone();
                    let confirm_path = path.clone();
                    let hunk_id = hunk_id.clone();
                    div()
                        .debug_selector(|| "confirm-repository-hunk-revert".into())
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(format!("Revert current hunk in {}?", path.0)),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(colors.muted_text)
                                .child("Sift will capture a recovery checkpoint, reverse only this authoritative worktree hunk, then import the result into the canonical room document."),
                        )
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new("cancel-repository-hunk-revert", "Cancel")
                                        .tone(ButtonTone::Neutral)
                                        .on_click(cx.listener(|shell, _, window, cx| {
                                            shell.dismiss_modal(&DismissModal, window, cx)
                                        })),
                                )
                                .child(
                                    Button::new("confirm-repository-hunk-revert", "Revert hunk")
                                        .tone(ButtonTone::DangerMuted)
                                        .disabled(self.repository.loading())
                                        .on_click(cx.listener(move |shell, _, _, cx| {
                                            shell.revert_repository_hunk(
                                                confirm_path.clone(),
                                                hunk_id.clone(),
                                                cx,
                                            )
                                        })),
                                ),
                        )
                        .into_any_element()
                }
                Modal::ConfirmRepositoryUncommit => {
                    let head = self
                        .repository
                        .status()
                        .and_then(|status| status.head_oid.as_deref())
                        .map(|head| head.chars().take(12).collect::<String>())
                        .unwrap_or_else(|| "current HEAD".into());
                    div()
                        .debug_selector(|| "confirm-repository-uncommit".into())
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(format!("Uncommit {head}?")),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(colors.muted_text)
                                .child("Sift will capture a recovery checkpoint, move HEAD to its parent, and keep the removed commit staged."),
                        )
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new("cancel-repository-uncommit", "Cancel")
                                        .tone(ButtonTone::Neutral)
                                        .on_click(cx.listener(|shell, _, window, cx| {
                                            shell.dismiss_modal(&DismissModal, window, cx)
                                        })),
                                )
                                .child(
                                    Button::new("confirm-repository-uncommit", "Uncommit")
                                        .tone(ButtonTone::DangerMuted)
                                        .disabled(self.repository.loading())
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.modal = None;
                                            shell.uncommit_repository(cx);
                                        })),
                                ),
                        )
                        .into_any_element()
                }
                Modal::RepositoryCommit => {
                    let message = self.repository_commit_input.read(cx).text().to_owned();
                    let subject_length = message.lines().next().unwrap_or("").chars().count();
                    let subject_limit = self.settings.repository.commit_subject_limit.max(1);
                    let (author_name, author_email) = self.repository_commit_identity();
                    let can_commit = !message.trim().is_empty()
                        && self.repository.has_staged_changes()
                        && !self.repository.loading();
                    div()
                        .debug_selector(|| "repository-commit-editor".into())
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("Commit staged SQL changes"),
                        )
                        .child(self.repository_commit_input.clone())
                        .child(
                            div()
                                .flex()
                                .justify_between()
                                .text_xs()
                                .text_color(if subject_length > subject_limit {
                                    colors.warning
                                } else {
                                    colors.muted_text
                                })
                                .child(format!("Subject {subject_length}/{subject_limit}"))
                                .child(format!("{author_name} <{author_email}>")),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(colors.muted_text)
                                .child("The message stays client-local until commit. Sift creates an immutable checkpoint before committing the existing Git index."),
                        )
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new("cancel-expanded-repository-commit", "Cancel")
                                        .tone(ButtonTone::Neutral)
                                        .on_click(cx.listener(|shell, _, window, cx| {
                                            shell.dismiss_modal(&DismissModal, window, cx)
                                        })),
                                )
                                .child(
                                    Button::new("open-uncommit-confirmation", "Uncommit HEAD")
                                        .tone(ButtonTone::DangerGhost)
                                        .disabled(self.repository.status().is_none_or(|status| {
                                            status.head_oid.is_none() || self.repository.loading()
                                        }))
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.modal = Some(Modal::ConfirmRepositoryUncommit);
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new("amend-expanded-repository-commit", "Amend HEAD")
                                        .tone(ButtonTone::Neutral)
                                        .disabled(
                                            !can_commit
                                                || self.repository.status().is_none_or(|status| {
                                                    status.head_oid.is_none()
                                                }),
                                        )
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.commit_repository(true, cx);
                                            if shell.repository.loading() {
                                                shell.modal = None;
                                            }
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new("submit-expanded-repository-commit", "Commit")
                                        .debug_selector("submit-expanded-repository-commit")
                                        .tone(ButtonTone::Accent)
                                        .disabled(!can_commit)
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.commit_repository(false, cx);
                                            if shell.repository.loading() {
                                                shell.modal = None;
                                            }
                                            cx.notify();
                                        })),
                                ),
                        )
                        .into_any_element()
                }
                Modal::CsvImport => {
                    let preview = self.csv_import_preview.as_ref();
                    let mappings = preview
                        .into_iter()
                        .flat_map(|preview| &preview.columns)
                        .enumerate()
                        .map(|(index, column)| {
                            div()
                                .debug_selector(move || format!("csv-import-column-{index}"))
                                .grid()
                                .grid_cols(4)
                                .gap_2()
                                .px_2()
                                .py_1()
                                .border_b_1()
                                .border_color(colors.subtle_border)
                                .text_xs()
                                .child(column.source.clone())
                                .child(format!("→ {}", column.target))
                                .child(format!("{:?}", column.inferred_type))
                                .child(if column.nullable { "nullable" } else { "required" })
                        })
                        .collect::<Vec<_>>();
                    let rows = preview
                        .into_iter()
                        .flat_map(|preview| &preview.rows)
                        .enumerate()
                        .map(|(index, row)| {
                            div()
                                .debug_selector(move || format!("csv-import-preview-row-{index}"))
                                .px_2()
                                .py_1()
                                .border_b_1()
                                .border_color(colors.subtle_border)
                                .font_family("monospace")
                                .text_xs()
                                .truncate()
                                .child(row.join("  |  "))
                        })
                        .collect::<Vec<_>>();
                    let (table, row_count, conflict_policy, create_table) = preview.map_or_else(
                        || {
                            (
                                "CSV import".to_owned(),
                                0,
                                sift_protocol::CsvConflictPolicy::Abort,
                                true,
                            )
                        },
                        |preview| {
                            (
                                preview.table.clone(),
                                preview.row_count,
                                preview.conflict_policy,
                                preview.create_table,
                            )
                        },
                    );
                    div()
                        .debug_selector(|| "csv-import-preview".into())
                        .w_full()
                        .max_h(px(700.))
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(format!("Import CSV into {table}")))
                        .child(div().text_sm().text_color(colors.muted_text).child(format!("{row_count} data row(s) · inferred from first 200 rows · preview limited to 20")))
                        .child(div().text_xs().font_weight(gpui::FontWeight::SEMIBOLD).child("COLUMN MAPPING"))
                        .child(div().id("csv-import-mappings").max_h(px(180.)).overflow_y_scroll().border_1().border_color(colors.subtle_border).children(mappings))
                        .child(div().text_xs().font_weight(gpui::FontWeight::SEMIBOLD).child("DATA PREVIEW"))
                        .child(div().id("csv-import-preview-rows").flex_1().min_h_0().overflow_y_scroll().border_1().border_color(colors.subtle_border).children(rows))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    Button::new(
                                        "csv-import-conflict-policy",
                                        format!("Duplicates: {:?}", conflict_policy),
                                    )
                                    .debug_selector("csv-import-conflict-policy")
                                    .tone(ButtonTone::Ghost)
                                    .on_click(cx.listener(|shell, _, _, cx| {
                                        shell.toggle_csv_conflict_policy(cx)
                                    })),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .gap_2()
                                        .child(
                                            Button::new("cancel-csv-import", "Cancel")
                                                .tone(ButtonTone::Neutral)
                                                .on_click(cx.listener(
                                                    |shell, _, window, cx| {
                                                        shell.dismiss_modal(
                                                            &DismissModal,
                                                            window,
                                                            cx,
                                                        )
                                                    },
                                                )),
                                        )
                                        .child(
                                            Button::new(
                                                "confirm-csv-import",
                                                if create_table {
                                                    "Create table and import"
                                                } else {
                                                    "Import into table"
                                                },
                                            )
                                            .debug_selector("confirm-csv-import")
                                            .tone(ButtonTone::Accent)
                                            .disabled(preview.is_none())
                                            .on_click(cx.listener(|shell, _, _, cx| {
                                                shell.confirm_csv_import(cx)
                                            })),
                                        ),
                                ),
                        )
                        .into_any_element()
                }
                Modal::TransferRecipes => {
                    let rows = self.transfer_recipes.iter().cloned().enumerate().map(
                        |(index, recipe)| {
                            let selected = index == self.transfer_recipe_selected;
                            div()
                                .id(("transfer-recipe", index))
                                .debug_selector(move || format!("transfer-recipe-{index}"))
                                .role(Role::Button)
                                .h(px(44.))
                                .px_2()
                                .flex()
                                .items_center()
                                .gap_2()
                                .border_b_1()
                                .border_color(colors.subtle_border)
                                .when(selected, |row| row.bg(colors.active_surface))
                                .hover(|row| row.bg(colors.hovered_surface))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |shell, _, window, cx| {
                                        shell.edit_transfer_recipe(index, cx);
                                        shell.transfer_recipe_focus_handle.focus(window, cx);
                                    }),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex_1()
                                        .flex()
                                        .flex_col()
                                        .child(
                                            div()
                                                .truncate()
                                                .text_color(colors.text)
                                                .child(recipe.name.clone()),
                                        )
                                        .child(
                                            div()
                                                .truncate()
                                                .text_xs()
                                                .text_color(colors.disabled_text)
                                                .child(format!(
                                                    "{:?} · {} v{}",
                                                    recipe.direction,
                                                    recipe.format_id,
                                                    recipe.format_version
                                                )),
                                        ),
                                )
                        },
                    );
                    let direction = format!("{:?}", self.transfer_recipe_direction);
                    let editing = self.transfer_recipe_edit.is_some();
                    let result = self.transfer_execution_result.as_ref().map(|result| match result {
                        sift_protocol::TransferExecutionResult::Artifact { artifact } => format!(
                            "Artifact {} · {} · {} bytes · SHA-256 {}",
                            artifact.id.0,
                            artifact.content_type,
                            artifact.byte_len,
                            artifact.digest
                        ),
                        sift_protocol::TransferExecutionResult::Import { result, .. } if result.dry_run => format!(
                            "Preview: validated {} source row(s) · {}. No rows written. Resume row remains {}.",
                            result.rows_validated, result.table, result.resume_from_row
                        ),
                        sift_protocol::TransferExecutionResult::Import { result, quarantine_artifact } => format!(
                            "Imported {} row(s), skipped {}, quarantined {} · {}{}",
                            result.rows_inserted, result.rows_skipped, result.quarantined_rows.len(), result.table,
                            quarantine_artifact.as_ref().map(|artifact| format!(" · Quarantine report {}", artifact.id.0)).unwrap_or_default()
                        ),
                        sift_protocol::TransferExecutionResult::Validated {
                            direction,
                            format_id,
                            resume_from_row,
                        } => format!(
                            "Validated {direction:?} · {format_id} · resume row {resume_from_row}"
                        ),
                    });
                    let field = |label: &'static str, input: Entity<TextInput>| {
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(SectionLabel::new(label))
                            .child(input)
                    };
                    div()
                        .track_focus(&self.transfer_recipe_focus_handle)
                        .key_context("SiftTransferRecipes")
                        .on_key_down(cx.listener(Self::handle_transfer_recipe_key))
                        .h(px(560.))
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .child(
                                            div()
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .child("Transfer recipes"),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(colors.muted_text)
                                                .child("Revisioned query exports and upload-to-table imports"),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .child(
                                            Button::new("new-transfer-recipe", "New")
                                                .tone(ButtonTone::Ghost)
                                                .on_click(cx.listener(|shell, _, _, cx| {
                                                    shell.clear_transfer_recipe_editor(cx);
                                                    cx.notify();
                                                })),
                                        )
                                        .child(
                                            Button::new("refresh-transfer-recipes", "Refresh")
                                                .tone(ButtonTone::Ghost)
                                                .loading(self.transfer_recipes_loading)
                                                .on_click(cx.listener(|shell, _, _, cx| {
                                                    shell.request_transfer_recipes(cx)
                                                })),
                                        ),
                                ),
                        )
                        .children(
                            self.transfer_recipes_error
                                .clone()
                                .map(ErrorBanner::new),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_h_0()
                                .flex()
                                .border_1()
                                .border_color(colors.subtle_border)
                                .child(
                                    div()
                                        .id("transfer-recipe-list")
                                        .w(px(230.))
                                        .flex_none()
                                        .overflow_y_scroll()
                                        .when(self.transfer_recipes.is_empty(), |list| {
                                            list.child(
                                                div()
                                                    .p_3()
                                                    .text_xs()
                                                    .whitespace_normal()
                                                    .child("No transfer recipes yet."),
                                            )
                                        })
                                        .children(rows),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .p_3()
                                        .flex()
                                        .flex_col()
                                        .gap_3()
                                        .overflow_hidden()
                                        .child(
                                            div()
                                                .flex()
                                                .gap_2()
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .child(field(
                                                            "NAME",
                                                            self.transfer_recipe_name_input.clone(),
                                                        )),
                                                )
                                                .child(
                                                    div()
                                                        .w(px(110.))
                                                        .child(
                                                            Button::new(
                                                                "transfer-recipe-direction",
                                                                direction,
                                                            )
                                                            .tone(ButtonTone::Neutral)
                                                            .wide(true)
                                                            .on_click(cx.listener(
                                                                |shell, _, _, cx| {
                                                                    shell.toggle_transfer_recipe_direction(cx)
                                                                },
                                                            )),
                                                        ),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .flex()
                                                .gap_2()
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .child(field(
                                                            "FORMAT",
                                                            self.transfer_recipe_format_input.clone(),
                                                        )),
                                                )
                                                .child(
                                                    div()
                                                        .w(px(120.))
                                                        .child(field(
                                                            "VERSION",
                                                            self.transfer_recipe_version_input.clone(),
                                                        )),
                                                ),
                                        )
                                        .child(field(
                                            "OPTIONS JSON",
                                            self.transfer_recipe_options_input.clone(),
                                        ))
                                        .when(
                                            self.transfer_recipe_direction
                                                == sift_protocol::TransferDirection::Import,
                                            |editor| {
                                                editor
                                                    .child(field(
                                                        "DESTINATION TABLE",
                                                        self.transfer_recipe_table_input.clone(),
                                                    ))
                                                    .child(
                                                        div()
                                                            .flex()
                                                            .items_end()
                                                            .gap_2()
                                                            .child(
                                                                div()
                                                                    .w(px(180.))
                                                                    .child(field(
                                                                        "XLSX SHEET",
                                                                        self.transfer_recipe_sheet_input.clone(),
                                                                    )),
                                                            )
                                                            .child(
                                                                Button::new(
                                                                    "transfer-create-table",
                                                                    if self.transfer_import_create_table {
                                                                        "Create table: yes"
                                                                    } else {
                                                                        "Create table: no"
                                                                    },
                                                                )
                                                                .tone(ButtonTone::Ghost)
                                                                .on_click(cx.listener(
                                                                    |shell, _, _, cx| {
                                                                        shell.toggle_transfer_create_table(cx)
                                                                    },
                                                                )),
                                                            )
                                                            .child(
                                                                Button::new(
                                                                    "transfer-conflict-policy",
                                                                    format!(
                                                                        "Duplicates: {:?}",
                                                                        self.transfer_import_conflict_policy
                                                                    ),
                                                                )
                                                                .tone(ButtonTone::Ghost)
                                                                .on_click(cx.listener(
                                                                    |shell, _, _, cx| {
                                                                        shell.toggle_transfer_conflict_policy(cx)
                                                                    },
                                                                )),
                                                            ),
                                                    )
                                            },
                                        )
                                        .children(result.map(|message| {
                                            div()
                                                .p_2()
                                                .rounded_sm()
                                                .bg(colors.success_muted)
                                                .text_xs()
                                                .whitespace_normal()
                                                .child(message)
                                        }))
                                        .child(div().flex_1())
                                        .child(
                                            div()
                                                .pt_2()
                                                .border_t_1()
                                                .border_color(colors.subtle_border)
                                                .flex()
                                                .items_center()
                                                .gap_2()
                                                .child(
                                                    Button::new(
                                                        "save-transfer-recipe",
                                                        if editing { "Update" } else { "Create" },
                                                    )
                                                    .debug_selector("save-transfer-recipe")
                                                    .tone(ButtonTone::Accent)
                                                    .loading(self.transfer_recipes_loading)
                                                    .on_click(cx.listener(|shell, _, _, cx| {
                                                        shell.save_transfer_recipe(cx)
                                                    })),
                                                )
                                                .child(
                                                    Button::new(
                                                        "validate-transfer-recipe",
                                                        "Validate",
                                                    )
                                                    .tone(ButtonTone::Neutral)
                                                    .disabled(self.transfer_recipe_edit.is_none())
                                                    .on_click(cx.listener(|shell, _, _, cx| {
                                                        shell.validate_transfer_recipe(cx)
                                                    })),
                                                )
                                                .child(
                                                    Button::new(
                                                        "delete-transfer-recipe",
                                                        if self.transfer_recipe_edit.is_some()
                                                            && self.transfer_recipe_delete_confirmation
                                                                == self.transfer_recipe_edit
                                                        {
                                                            "Confirm delete"
                                                        } else {
                                                            "Delete"
                                                        },
                                                    )
                                                    .tone(ButtonTone::DangerGhost)
                                                    .disabled(self.transfer_recipe_edit.is_none())
                                                    .on_click(cx.listener(|shell, _, _, cx| {
                                                        shell.delete_transfer_recipe(cx)
                                                    })),
                                                )
                                                .child(div().flex_1())
                                                .when(self.transfer_execution_pending, |actions| {
                                                    actions.child(
                                                        Button::new(
                                                            "cancel-transfer-recipe",
                                                            "Cancel request",
                                                        )
                                                        .tone(ButtonTone::DangerGhost)
                                                        .on_click(cx.listener(
                                                            |shell, _, _, cx| {
                                                                shell.cancel_transfer_recipe(cx)
                                                            },
                                                        )),
                                                    )
                                                })
                                                .child(
                                                    Button::new("preview-transfer-recipe", "Preview")
                                                        .debug_selector("preview-transfer-recipe")
                                                        .tone(ButtonTone::Neutral)
                                                        .disabled(self.transfer_recipe_edit.is_none() || self.transfer_execution_pending)
                                                        .on_click(cx.listener(|shell, _, _, cx| {
                                                            shell.execute_selected_transfer_recipe(true, cx)
                                                        })),
                                                )
                                                .child(
                                                    Button::new(
                                                        "execute-transfer-recipe",
                                                        if self.transfer_execution_pending {
                                                            "Executing…"
                                                        } else {
                                                            "Execute"
                                                        },
                                                    )
                                                    .debug_selector("execute-transfer-recipe")
                                                    .tone(ButtonTone::Accent)
                                                    .loading(self.transfer_execution_pending)
                                                    .disabled(
                                                        self.transfer_recipe_edit.is_none()
                                                            || self.transfer_execution_pending,
                                                    )
                                                    .on_click(cx.listener(|shell, _, _, cx| {
                                                        shell.execute_selected_transfer_recipe(false, cx)
                                                    })),
                                                ),
                                        ),
                                ),
                        )
                        .into_any_element()
                }
                Modal::ConfirmDeleteDatabaseObject => {
                    let label = self
                        .pending_delete_database_object
                        .as_ref()
                        .map(|target| {
                            format!("{}.{}.{}", target.catalog, target.schema, target.object)
                        })
                        .unwrap_or_else(|| "selected table".into());
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("Delete table?"),
                        )
                        .child(
                            div()
                                .text_color(colors.muted_text)
                                .whitespace_normal()
                                .child(format!(
                                    "This will execute DROP TABLE for {label}. This cannot be undone."
                                )),
                        )
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new("cancel-delete-database-object", "Cancel")
                                        .tone(ButtonTone::Ghost)
                                        .on_click(cx.listener(|shell, _, window, cx| {
                                            shell.dismiss_modal(&DismissModal, window, cx)
                                        })),
                                )
                                .child(
                                    Button::new("confirm-delete-database-object", "Delete table")
                                        .tone(ButtonTone::DangerGhost)
                                        .on_click(cx.listener(|shell, _, window, cx| {
                                            shell.confirm_delete_database_object(window, cx)
                                        })),
                                ),
                        )
                        .into_any_element()
                }
                Modal::ObjectPeek => {
                    let peek = self.object_peek.clone();
                    let title = peek.as_ref().map_or_else(
                        || "Object definition".into(),
                        |peek| {
                            format!(
                                "{}.{}.{}",
                                peek.target.catalog, peek.target.schema, peek.target.object
                            )
                        },
                    );
                    let definition = match peek.as_ref().and_then(|peek| peek.ddl.as_ref()) {
                        None => div()
                            .flex_1()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(colors.muted_text)
                            .child("Loading definition…")
                            .into_any_element(),
                        Some(Err(message)) => div()
                            .flex_1()
                            .p_3()
                            .rounded_sm()
                            .bg(colors.danger_muted)
                            .text_color(colors.danger)
                            .whitespace_normal()
                            .child(message.clone())
                            .into_any_element(),
                        Some(Ok(ddl)) => div()
                            .id("object-peek-definition")
                            .debug_selector(|| "object-peek-definition".into())
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .p_3()
                            .rounded_sm()
                            .border_1()
                            .border_color(colors.subtle_border)
                            .bg(colors.background)
                            .font_family("monospace")
                            .text_xs()
                            .children(ddl.lines().map(|line| {
                                div().min_h(px(18.)).whitespace_normal().child(line.to_owned())
                            }))
                            .into_any_element(),
                    };
                    div()
                        .h(px(460.))
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(title),
                        )
                        .child(definition)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new("close-object-peek", "Close")
                                        .tone(ButtonTone::Ghost)
                                        .on_click(cx.listener(|shell, _, window, cx| {
                                            shell.dismiss_modal(&DismissModal, window, cx)
                                        })),
                                )
                                .child(
                                    Button::new("open-peeked-object", "Open DDL tab")
                                        .tone(ButtonTone::Accent)
                                        .disabled(peek.is_none())
                                        .on_click(cx.listener(|shell, _, window, cx| {
                                            shell.open_peeked_object_in_tab(window, cx)
                                        })),
                                ),
                        )
                        .into_any_element()
                }
                Modal::SemanticRename => {
                    let (pending, edit_count, warnings) = self.pending_semantic_rename.as_ref().map_or((false, None, Vec::new()), |rename| (rename.pending, rename.edits.as_ref().map(Vec::len), rename.warnings.clone()));
                    let preview_rows = self.semantic_rename_preview_rows(cx);
                    let can_apply = edit_count.is_some_and(|count| count > 0) && !pending;
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child("Rename SQL symbol"))
                        .child(self.semantic_rename_input.clone())
                        .children(edit_count.map(|count| div().rounded_sm().bg(colors.active_surface).p_2().child(format!("Preview: {count} reference(s) in this query will change."))))
                        .children(preview_rows.into_iter().take(20).enumerate().map(|(index, (line, before, after))| {
                            div()
                                .debug_selector(move || format!("semantic-rename-preview-{index}"))
                                .grid()
                                .grid_cols(3)
                                .gap_2()
                                .px_2()
                                .py_1()
                                .rounded_sm()
                                .bg(colors.elevated_surface)
                                .text_xs()
                                .child(div().text_color(colors.muted_text).child(format!("Line {line}")))
                                .child(div().font_family("monospace").child(before))
                                .child(div().font_family("monospace").text_color(colors.success).child(after))
                        }))
                        .children((edit_count.unwrap_or_default() > 20).then(|| div().text_xs().text_color(colors.muted_text).child(format!("{} more references", edit_count.unwrap_or_default() - 20))))
                        .children(warnings.into_iter().map(|warning| div().text_xs().text_color(colors.warning).child(warning)))
                        .child(div().flex().justify_end().gap_2()
                            .child(Button::new("cancel-semantic-rename", "Cancel").tone(ButtonTone::Neutral).on_click(cx.listener(|shell, _, window, cx| shell.dismiss_modal(&DismissModal, window, cx))))
                            .child(Button::new("preview-semantic-rename", if pending { "Preparing…" } else { "Preview" }).tone(ButtonTone::Neutral).disabled(pending).on_click(cx.listener(|shell, _, _, cx| shell.preview_semantic_rename(cx))))
                            .child(Button::new("apply-semantic-rename", "Apply rename").tone(ButtonTone::Accent).disabled(!can_apply).on_click(cx.listener(|shell, _, window, cx| shell.apply_semantic_rename(window, cx)))))
                        .into_any_element()
                }
            };
            // Scrim-clicking dismisses transient surfaces. Long-form dialogs
            // with typed-but-unsaved input keep their explicit cancel control.
            let dismiss_on_scrim = matches!(
                modal,
                Modal::ServerPicker
                    | Modal::Settings
                    | Modal::ApiTokens
                    | Modal::ConnectionPolicy
                    | Modal::TenantUsage
                    | Modal::VcsDiagnostics
                    | Modal::Administration
                    | Modal::Themes
                    | Modal::Keymaps
                    | Modal::Account
                    | Modal::CommandPalette
                    | Modal::DataSearch
                    | Modal::DataResults(_)
                    | Modal::QueryParameters
                    | Modal::EditResultCell
                    | Modal::PlanCaptures
                    | Modal::ConnectionUrl
                    | Modal::DatabaseConnection
                    | Modal::ConfirmTransactionDisconnect
                    | Modal::ConfirmProductionExecution
                    | Modal::ConfirmOutcomeUnknownRerun(_, _)
                    | Modal::ConfirmDeleteConnection(_)
                    | Modal::ConfirmTerminateProcess(_)
                    | Modal::SemanticRename
                    | Modal::CatalogDiagram
                    | Modal::CatalogMigration
                    | Modal::DdlSources
                    | Modal::RoomAdministration
                    | Modal::CatalogSnapshots
                    | Modal::CsvImport
                    | Modal::WorkspaceReconcile
                    | Modal::ObjectPeek
                    | Modal::ConfirmDeleteDatabaseObject
            );
            div()
                .id("modal-layer")
                .debug_selector(|| "modal-layer".into())
                .key_context("SiftModal")
                .absolute()
                .top(if app_bar_modal {
                    toolbar_height
                } else {
                    px(0.)
                })
                .right_0()
                .bottom_0()
                .left_0()
                .occlude()
                .when(dismiss_on_scrim, |layer| {
                    layer.on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|shell, _, window, cx| {
                            shell.dismiss_modal(&DismissModal, window, cx)
                        }),
                    )
                })
                .when(!dismiss_on_scrim, |layer| {
                    layer.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                })
                .flex()
                .items_start()
                .when(server_picker, |layer| {
                    layer.justify_start().pt_1().pl(px(38.)).pr_2()
                })
                .when(account, |layer| layer.justify_end().pt_1().px_2())
                .when(command_palette, |layer| layer.justify_center().pt_1().px_2())
                .when(!popover && !data_results, |layer| {
                    layer
                        .items_center()
                        .justify_center()
                        .px_4()
                        .py_4()
                        .bg(colors.scrim)
                })
                .when(data_results, |layer| {
                    layer.items_center().justify_center().bg(colors.scrim)
                })
                .child(
                    modal_layout::card(data_results, padded, card_width, max_card_height, colors, cx.theme().metrics)
                        .child(content),
                )
        })
    }
}
