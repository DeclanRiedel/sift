//! Query history state, request correlation, filtering, and navigation.

use super::*;

pub(super) struct QueryHistoryState {
    pub(super) focus_handle: FocusHandle,
    pub(super) input: Entity<TextInput>,
    pub(super) filter_open: bool,
    pub(super) status_filter: QueryHistoryStatusFilter,
    pub(super) selected: usize,
    pub(super) scroll_handle: UniformListScrollHandle,
    pub(super) rows: Vec<sift_api_types::QueryHistory>,
    pub(super) instance: Option<String>,
    pub(super) generation: u64,
    pub(super) loading: bool,
    pub(super) error: Option<String>,
    pub(super) next_cursor: Option<String>,
}

impl QueryHistoryState {
    pub(super) fn new(input: Entity<TextInput>, focus_handle: FocusHandle) -> Self {
        Self {
            input,
            focus_handle,
            filter_open: false,
            status_filter: QueryHistoryStatusFilter::All,
            selected: 0,
            scroll_handle: UniformListScrollHandle::new(),
            rows: Vec::new(),
            instance: None,
            generation: 0,
            loading: false,
            error: None,
            next_cursor: None,
        }
    }
}

impl WorkspaceShell {
    pub(super) fn query_history_connection_name(&self, profile_id: Option<i64>) -> String {
        let Some(profile_id) = profile_id else {
            return "No connection".into();
        };
        self.lifecycle
            .tenants
            .iter()
            .flat_map(|tenant| &tenant.connections)
            .find(|connection| connection.id == profile_id)
            .map(|connection| connection.name.clone())
            .unwrap_or_else(|| format!("Connection {profile_id}"))
    }

    pub(super) fn filtered_query_history(&self, cx: &App) -> Vec<sift_api_types::QueryHistory> {
        let query = self
            .query_history
            .input
            .read(cx)
            .text()
            .trim()
            .to_lowercase();
        self.query_history
            .rows
            .iter()
            .filter(|entry| {
                if !self.query_history.status_filter.matches(&entry.status) {
                    return false;
                }
                if query.is_empty() {
                    return true;
                }
                let status = match entry.status {
                    sift_api_types::QueryStatus::Ok => "ok success",
                    sift_api_types::QueryStatus::Error => "error failed",
                    sift_api_types::QueryStatus::Canceled => "canceled cancelled",
                };
                entry.sql_text.to_lowercase().contains(&query)
                    || entry
                        .error_message
                        .as_deref()
                        .is_some_and(|message| message.to_lowercase().contains(&query))
                    || self
                        .query_history_connection_name(
                            entry.connection_profile_id.map(|profile| profile.0),
                        )
                        .to_lowercase()
                        .contains(&query)
                    || status.contains(&query)
                    || entry
                        .started_at
                        .format("%Y-%m-%d %H:%M:%S")
                        .to_string()
                        .contains(&query)
            })
            .cloned()
            .collect()
    }

    pub(super) fn request_global_query_history(
        &mut self,
        cursor: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.query_history.loading {
            return;
        }
        let instance_id = self
            .selected_instance_id
            .clone()
            .unwrap_or_else(|| "local".into());
        let Some(sender) = &self.executor_sender else {
            let message = "Query history executor is unavailable".to_owned();
            self.query_history.error = Some(message.clone());
            self.record_runtime_error(None, "Load query history", message, cx);
            return;
        };
        let sent = sender
            .send(ExecutorCommand::LoadGlobalHistory {
                instance_id: instance_id.clone(),
                generation: self.query_history.generation.wrapping_add(1),
                cursor,
            })
            .is_ok();
        if sent {
            self.query_history.instance = Some(instance_id);
            self.query_history.generation = self.query_history.generation.wrapping_add(1);
            self.query_history.loading = true;
            self.query_history.error = None;
        } else {
            let message = "Query history executor stopped".to_owned();
            self.query_history.error = Some(message.clone());
            self.record_runtime_error(None, "Load query history", message, cx);
        }
        cx.notify();
    }

    pub(super) fn refresh_query_history(&mut self, cx: &mut Context<Self>) {
        self.query_history.rows.clear();
        self.query_history.loading = false;
        self.query_history.next_cursor = None;
        self.query_history.error = None;
        self.query_history.selected = 0;
        self.query_history
            .scroll_handle
            .scroll_to_item(0, ScrollStrategy::Top);
        self.request_global_query_history(None, cx);
    }

    pub(super) fn open_query_history_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.active_left_panel = LeftPanel::QueryHistory;
        self.left_dock.presentation.open = true;
        self.query_history.filter_open = false;
        self.query_history
            .input
            .update(cx, |input, cx| input.set_text("", cx));
        self.refresh_query_history(cx);
        self.focused_surface = WorkspaceSurface::QueryHistory;
        self.query_history.focus_handle.focus(window, cx);
        self.fit_side_docks_to_width(self.window_presentation.bounds.width);
        self.persist(cx);
        cx.notify();
    }

    pub(super) fn activate_selected_query_history(
        &mut self,
        action: QueryHistoryAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entry = self
            .filtered_query_history(cx)
            .get(self.query_history.selected)
            .cloned();
        let Some(entry) = entry else {
            return;
        };
        self.activate_query_history_entry(entry, action, window, cx);
    }

    pub(super) fn activate_query_history_entry(
        &mut self,
        entry: sift_api_types::QueryHistory,
        action: QueryHistoryAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if entry.sql_text.starts_with("sqlfp:") {
            self.show_toast(
                "This history entry stores only a query fingerprint".into(),
                cx,
            );
            return;
        }
        if action == QueryHistoryAction::Run {
            let historical_profile = entry.connection_profile_id.map(|profile| profile.0);
            let active_profile = match self.connection_status {
                ConnectionStatus::Connected { profile_id, .. } => Some(profile_id),
                _ => None,
            };
            if historical_profile.is_some() && historical_profile != active_profile {
                let connection = self.query_history_connection_name(historical_profile);
                let message =
                    format!("Connect to {connection} before rerunning this history entry");
                self.query_history.error = Some(message.clone());
                self.record_runtime_error(None, "Rerun query history", message, cx);
                cx.notify();
                return;
            }
        }
        self.new_query(window, cx);
        let Some(pane) = self.panes.get(self.active_pane).cloned() else {
            return;
        };
        let Some(item_id) = pane.read(cx).active_item().map(|item| item.id) else {
            return;
        };
        pane.update(cx, |pane, cx| {
            if let Some(editor) = pane.editor(item_id) {
                editor.update(cx, |editor, cx| {
                    editor.replace_text_from_owner(&entry.sql_text, cx)
                });
            }
            if let Some(item) = pane.items.iter_mut().find(|item| item.id == item_id) {
                item.dirty = true;
            }
            cx.notify();
        });
        self.persist(cx);
        if action == QueryHistoryAction::Save {
            self.save_active_query_with_profile(
                entry.connection_profile_id.map(|profile| profile.0),
                cx,
            );
        } else if action == QueryHistoryAction::Run {
            self.execute_database_item(item_id, entry.sql_text, cx);
        }
    }

    pub(super) fn open_selected_query_history(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_selected_query_history(QueryHistoryAction::Open, window, cx);
    }

    pub(super) fn move_query_history_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let last = self.filtered_query_history(cx).len().saturating_sub(1);
        self.query_history.selected = self
            .query_history
            .selected
            .saturating_add_signed(delta)
            .min(last);
        self.query_history
            .scroll_handle
            .scroll_to_item(self.query_history.selected, ScrollStrategy::Nearest);
        cx.notify();
    }

    pub(super) fn open_query_history_filter(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.query_history.filter_open = true;
        self.query_history.input.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    pub(super) fn close_query_history_filter(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.query_history.filter_open = false;
        self.query_history
            .input
            .update(cx, |input, cx| input.set_text("", cx));
        self.query_history.focus_handle.focus(window, cx);
        cx.notify();
    }

    pub(super) fn set_query_history_status_filter(
        &mut self,
        filter: QueryHistoryStatusFilter,
        cx: &mut Context<Self>,
    ) {
        self.query_history.status_filter = filter;
        self.query_history.selected = 0;
        self.query_history
            .scroll_handle
            .scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    pub(super) fn handle_query_history_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.query_history.filter_open {
            return;
        }
        let modifiers = event.keystroke.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform || modifiers.function {
            return;
        }
        if modifiers.shift {
            match event.keystroke.key.as_str() {
                "l" | "L" => {
                    if let Some(cursor) = self.query_history.next_cursor.clone() {
                        self.request_global_query_history(Some(cursor), cx);
                    }
                }
                "r" | "R" => self.refresh_query_history(cx),
                _ => return,
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        match event.keystroke.key.as_str() {
            "j" | "down" => self.move_query_history_selection(1, cx),
            "k" | "up" => self.move_query_history_selection(-1, cx),
            "enter" => self.open_selected_query_history(window, cx),
            "/" => self.open_query_history_filter(window, cx),
            "r" => self.activate_selected_query_history(QueryHistoryAction::Run, window, cx),
            "s" => self.activate_selected_query_history(QueryHistoryAction::Save, window, cx),
            "1" => self.set_query_history_status_filter(QueryHistoryStatusFilter::All, cx),
            "2" => self.set_query_history_status_filter(QueryHistoryStatusFilter::Success, cx),
            "3" => self.set_query_history_status_filter(QueryHistoryStatusFilter::Failed, cx),
            "4" => self.set_query_history_status_filter(QueryHistoryStatusFilter::Canceled, cx),
            "escape" => self.focus_active_pane(window, cx),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    pub(super) fn apply_global_query_history(
        &mut self,
        instance_id: String,
        generation: u64,
        append: bool,
        page: Result<sift_protocol::CursorPage<sift_api_types::QueryHistory>, String>,
        cx: &mut Context<Self>,
    ) {
        if self.query_history.instance.as_deref() != Some(instance_id.as_str())
            || self.query_history.generation != generation
        {
            return;
        }
        self.query_history.loading = false;
        let selected_id = self
            .filtered_query_history(cx)
            .get(self.query_history.selected)
            .map(|entry| entry.id);
        match page {
            Ok(page) => {
                if append {
                    let known = self
                        .query_history
                        .rows
                        .iter()
                        .map(|entry| entry.id)
                        .collect::<HashSet<_>>();
                    self.query_history.rows.extend(
                        page.items
                            .into_iter()
                            .filter(|entry| !known.contains(&entry.id)),
                    );
                } else {
                    self.query_history.rows = page.items;
                }
                self.query_history.next_cursor = page.next_cursor;
                self.query_history.error = None;
                self.query_history.selected = selected_id
                    .and_then(|id| {
                        self.filtered_query_history(cx)
                            .iter()
                            .position(|entry| entry.id == id)
                    })
                    .unwrap_or_else(|| {
                        self.query_history
                            .selected
                            .min(self.filtered_query_history(cx).len().saturating_sub(1))
                    });
            }
            Err(message) => {
                self.query_history.error = Some(message.clone());
                self.record_runtime_error(None, "Load query history", message, cx);
            }
        }
        cx.notify();
    }
}
