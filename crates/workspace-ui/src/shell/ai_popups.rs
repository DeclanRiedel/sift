//! Small, transient AI controls; the transcript remains in place.
use super::*;

impl WorkspaceShell {
    pub(super) fn close_ai_popups(&mut self) {
        self.ai.menu_expanded = false;
        self.ai.thread_picker_expanded = false;
        self.ai.response_menu = None;
        self.ai.settings_expanded = false;
        self.ai.model_picker_expanded = false;
        self.ai.permission_picker_expanded = false;
        self.ai.context_choices_open = false;
        self.ai.popup_selected = 0;
    }

    pub(super) fn dismiss_ai_popup_outside(
        &mut self,
        event: &gpui::MouseDownEvent,
        cx: &mut Context<Self>,
    ) {
        // Only the active control owns its toggle; every other outside click dismisses.
        let trigger = if self.ai.menu_expanded {
            0
        } else if self.ai.thread_picker_expanded {
            1
        } else if self.ai.settings_expanded {
            2
        } else if self.ai.context_choices_open {
            3
        } else if self.ai.model_picker_expanded {
            4
        } else {
            5
        };
        if self.ai.popup_trigger_bounds[trigger]
            .get()
            .is_some_and(|bounds| bounds.contains(&event.position))
        {
            return;
        }
        self.close_ai_popups();
        cx.notify();
    }

    pub(super) fn render_ai_popup_trigger(
        &self,
        index: usize,
        child: impl IntoElement,
    ) -> AnyElement {
        let bounds = self.ai.popup_trigger_bounds[index].clone();
        div()
            .relative()
            .flex()
            .min_w_0()
            .when(index == 1, |view| view.flex_1())
            .when(index != 1, |view| view.flex_none())
            .child(child)
            .child(
                canvas(move |rect, _, _| bounds.set(Some(rect)), |_, _, _, _| {})
                    .absolute()
                    .inset_0(),
            )
            .into_any_element()
    }

    pub(super) fn filtered_ai_threads(&self, cx: &App) -> Vec<sift_protocol::AiChat> {
        let query = self.ai.thread_search.read(cx).text().trim().to_lowercase();
        let mut chats = self
            .ai
            .chats
            .iter()
            .filter(|chat| chat.title.to_lowercase().contains(&query))
            .cloned()
            .collect::<Vec<_>>();
        chats.sort_by_key(|chat| std::cmp::Reverse(chat.updated_at));
        chats
    }

    pub(super) fn render_ai_thread_picker(
        &self,
        max_width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors;
        let chats = self.filtered_ai_threads(cx);
        div()
            .id("ai-thread-picker")
            .debug_selector(|| "ai-thread-picker".into())
            .absolute()
            .right_3()
            .top(px(40.))
            .w(px(300.))
            .max_w(px(max_width))
            .border_1()
            .border_color(colors.subtle_border)
            .rounded_md()
            .bg(colors.elevated_surface)
            .shadow_md()
            .occlude()
            .p_1()
            .flex()
            .flex_col()
            .gap_1()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down_out(
                cx.listener(|shell, event, _, cx| shell.dismiss_ai_popup_outside(event, cx)),
            )
            .child(self.ai.thread_search.clone())
            .child(
                div()
                    .id("ai-thread-list")
                    .max_h(px(240.))
                    .overflow_y_scroll()
                    .track_scroll(&self.ai.thread_scroll)
                    .flex()
                    .flex_col()
                    .when(chats.is_empty(), |view| {
                        view.child(
                            div()
                                .p_2()
                                .text_xs()
                                .text_color(colors.muted_text)
                                .child("No matching threads"),
                        )
                    })
                    .children(chats.into_iter().enumerate().map(|(index, chat)| {
                        let selected = index == self.ai.thread_selected;
                        let current = self
                            .ai
                            .chat
                            .as_ref()
                            .is_some_and(|current| current.id == chat.id);
                        div()
                            .when(selected, |view| view.bg(colors.selected_surface))
                            .rounded_sm()
                            .child(
                                Button::new(format!("ai-thread-{}", chat.id), chat.title.clone())
                                    .debug_selector(format!("ai-thread-option-{index}"))
                                    .tone(ButtonTone::Ghost)
                                    .align_start()
                                    .full_width()
                                    .start_icon(if current {
                                        IconName::Check
                                    } else {
                                        IconName::Document
                                    })
                                    .disabled(self.ai.pending)
                                    .on_click(cx.listener(move |shell, _, _, cx| {
                                        shell.select_ai_thread(chat.clone(), cx)
                                    })),
                            )
                    })),
            )
            .child(
                div()
                    .px_2()
                    .text_xs()
                    .text_color(colors.muted_text)
                    .whitespace_normal()
                    .child("j/k navigate · / search · Enter open · Esc close"),
            )
            .into_any_element()
    }

    pub(super) fn render_ai_attachment_overlay(
        &self,
        max_width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors;
        div()
            .debug_selector(|| "ai-attachment-overlay".into())
            .absolute()
            .right_3()
            .top(px(40.))
            .w(px(max_width.min(260.)))
            .flex()
            .flex_col()
            .items_end()
            .gap_1()
            .children(self.ai.attachments.accepted.iter().map(|review| {
                let id = review.preview.id;
                let expired = review.preview.expires_at <= chrono::Utc::now();
                div()
                    .w_full()
                    .id(format!("ai-attachment-chip-{id}"))
                    .debug_selector(move || format!("ai-attachment-chip-{id}"))
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .rounded_md()
                    .border_1()
                    .border_color(colors.subtle_border)
                    .bg(colors.elevated_surface)
                    .occlude()
                    .text_xs()
                    .min_w_0()
                    .child(icon(
                        IconName::Table,
                        if expired {
                            colors.danger
                        } else {
                            colors.muted_text
                        },
                        12.,
                    ))
                    .child(div().max_w(px(190.)).truncate().child(format!(
                        "{}{}",
                        review.preview.attachment.label,
                        if expired { " · expired" } else { "" }
                    )))
                    .child(
                        IconButton::new(
                            format!("ai-chip-remove-{id}"),
                            IconName::Close,
                            "Remove attachment",
                        )
                        .debug_selector(format!("ai-chip-remove-{id}"))
                        .disabled(self.ai.pending)
                        .on_click(cx.listener(move |shell, _, _, cx| {
                            shell
                                .ai
                                .attachments
                                .accepted
                                .retain(|review| review.preview.id != id);
                            cx.notify();
                        })),
                    )
            }))
            .into_any_element()
    }

    pub(super) fn copy_ai_response(&mut self, action: usize, cx: &mut Context<Self>) {
        let Some(menu) = self.ai.response_menu.take() else {
            return;
        };
        let text = match (menu.selected, action) {
            (Some(selection), 0) => selection,
            (Some(_), 1) | (None, 0) => menu.plain,
            _ => menu.markdown,
        };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        cx.notify();
    }

    pub(super) fn render_ai_response_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(menu) = &self.ai.response_menu else {
            return div().into_any_element();
        };
        let colors = cx.theme().colors;
        let labels = if menu.selected.is_some() {
            vec![
                "Copy selection",
                "Copy response",
                "Copy response as Markdown",
            ]
        } else {
            vec!["Copy response", "Copy response as Markdown"]
        };
        deferred(
            anchored()
                .position(menu.position)
                .anchor(Anchor::TopLeft)
                .child(
                    div()
                        .id("ai-response-menu")
                        .debug_selector(|| "ai-response-menu".into())
                        .w(px(220.))
                        .p_1()
                        .border_1()
                        .border_color(colors.subtle_border)
                        .rounded_md()
                        .bg(colors.elevated_surface)
                        .shadow_md()
                        .occlude()
                        .flex()
                        .flex_col()
                        .role(Role::Menu)
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_mouse_down_out(cx.listener(|shell, _, _, cx| {
                            shell.ai.response_menu = None;
                            cx.notify();
                        }))
                        .children(labels.into_iter().enumerate().map(|(index, label)| {
                            div()
                                .when(index == self.ai.popup_selected, |view| {
                                    view.bg(colors.selected_surface)
                                })
                                .child(
                                    Button::new(format!("ai-copy-response-{index}"), label)
                                        .debug_selector(format!("ai-copy-response-{index}"))
                                        .tone(ButtonTone::Ghost)
                                        .align_start()
                                        .full_width()
                                        .on_click(cx.listener(move |shell, _, _, cx| {
                                            shell.copy_ai_response(index, cx)
                                        })),
                                )
                        })),
                ),
        )
        .with_priority(4)
        .into_any_element()
    }

    pub(super) fn handle_ai_popup_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let key = event.keystroke.key.as_str();
        let open = self.ai.menu_expanded
            || self.ai.thread_picker_expanded
            || self.ai.response_menu.is_some()
            || self.ai.settings_expanded
            || self.ai.model_picker_expanded
            || self.ai.permission_picker_expanded
            || self.ai.context_choices_open;
        if key == "escape" && open {
            self.close_ai_popups();
            self.ai.transcript_focus.focus(window, cx);
            cx.notify();
            return true;
        }
        if self.ai.transcript_focus.is_focused(window)
            && (key == "y"
                || (key == "c"
                    && (event.keystroke.modifiers.control || event.keystroke.modifiers.platform)))
        {
            if let Some(text) = self.ai.text_selection.borrow().selected_text() {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                return true;
            }
        }
        if self.ai.thread_picker_expanded {
            let searching = self.ai.thread_search.focus_handle(cx).is_focused(window);
            if key == "/" && !searching {
                self.ai.thread_search.focus_handle(cx).focus(window, cx);
                return true;
            }
            let chats = self.filtered_ai_threads(cx);
            let last = chats.len().saturating_sub(1);
            let navigate = !searching || event.keystroke.modifiers.control;
            match key {
                "j" | "down" if navigate => {
                    self.ai.thread_selected = (self.ai.thread_selected + 1).min(last)
                }
                "k" | "up" if navigate => {
                    self.ai.thread_selected = self.ai.thread_selected.saturating_sub(1)
                }
                "g" if navigate => {
                    self.ai.thread_selected = if event.keystroke.modifiers.shift {
                        last
                    } else {
                        0
                    }
                }
                "enter" => {
                    if let Some(chat) = chats.get(self.ai.thread_selected).cloned() {
                        self.select_ai_thread(chat, cx);
                    }
                    return true;
                }
                _ => return false,
            }
            self.ai
                .thread_scroll
                .scroll_to_item(self.ai.thread_selected);
            cx.notify();
            return true;
        }
        if self.ai.context_choices_open
            || self.ai.permission_picker_expanded
            || self.ai.settings_expanded
        {
            let count = if self.ai.context_choices_open {
                6
            } else if self.ai.permission_picker_expanded {
                2
            } else {
                1
            };
            match key {
                "j" | "down" => self.ai.popup_selected = (self.ai.popup_selected + 1) % count,
                "k" | "up" => self.ai.popup_selected = (self.ai.popup_selected + count - 1) % count,
                "enter" | "space" if self.ai.pending => {}
                "enter" | "space" if self.ai.context_choices_open => {
                    let options = &mut self.ai.inclusion;
                    let included = match self.ai.popup_selected {
                        0 => &mut options.sql,
                        1 => &mut options.selection,
                        2 => &mut options.errors,
                        3 => &mut options.diagnostics,
                        4 => &mut options.environment,
                        _ => &mut options.connection_state,
                    };
                    *included = !*included;
                }
                "enter" | "space" if self.ai.permission_picker_expanded => {
                    self.ai.mode = if self.ai.popup_selected == 0 {
                        sift_protocol::AiMode::Read
                    } else {
                        sift_protocol::AiMode::Propose
                    };
                    self.close_ai_popups();
                }
                "enter" | "space" => {
                    self.ai
                        .model_input
                        .update(cx, |input, cx| input.set_text("", cx));
                }
                _ => return false,
            }
            cx.notify();
            return true;
        }
        if self.ai.model_picker_expanded && self.ai.transcript_focus.is_focused(window) {
            let efforts = self
                .ai
                .models
                .iter()
                .find(|model| model.id == self.ai.model_input.read(cx).text())
                .map_or_else(Vec::new, |model| model.reasoning.clone());
            let count = 3 + self.ai.models.len() + efforts.len();
            match key {
                "j" | "down" => self.ai.popup_selected = (self.ai.popup_selected + 1) % count,
                "k" | "up" => self.ai.popup_selected = (self.ai.popup_selected + count - 1) % count,
                "/" => self.ai.model_input.focus_handle(cx).focus(window, cx),
                "enter" | "space" if self.ai.pending => {}
                "enter" | "space" => {
                    let index = self.ai.popup_selected;
                    if index < 3 {
                        let provider = [
                            sift_protocol::AiProvider::Codex,
                            sift_protocol::AiProvider::ClaudeCode,
                            sift_protocol::AiProvider::OpenCode,
                        ][index];
                        if self.ai.provider != provider {
                            self.ai.provider = provider;
                            self.ai.models.clear();
                            self.ai.models_loading = false;
                            self.ai.models_error = None;
                            self.ai.reasoning_effort = None;
                            self.ai
                                .model_input
                                .update(cx, |input, cx| input.set_text("", cx));
                            self.load_ai_models(cx);
                        }
                    } else if let Some(model) = self.ai.models.get(index - 3).cloned() {
                        self.ai
                            .model_input
                            .update(cx, |input, cx| input.set_text(model.id, cx));
                        self.ai.reasoning_effort = model.default_reasoning;
                    } else if let Some(effort) = efforts.get(index - 3 - self.ai.models.len()) {
                        self.ai.reasoning_effort = Some(effort.clone());
                    }
                }
                _ => return false,
            }
            if self.ai.popup_selected >= 3 && self.ai.popup_selected < self.ai.models.len() + 3 {
                self.ai
                    .model_scroll
                    .scroll_to_item(self.ai.popup_selected - 3);
            }
            cx.notify();
            return true;
        }
        if !self.ai.transcript_focus.is_focused(window) {
            return false;
        }
        let count = if let Some(menu) = &self.ai.response_menu {
            if menu.selected.is_some() {
                3
            } else {
                2
            }
        } else if self.ai.menu_expanded {
            7
        } else {
            return false;
        };
        match key {
            "j" | "down" => self.ai.popup_selected = (self.ai.popup_selected + 1) % count,
            "k" | "up" => self.ai.popup_selected = (self.ai.popup_selected + count - 1) % count,
            "enter" | "space" if self.ai.response_menu.is_some() => {
                self.copy_ai_response(self.ai.popup_selected, cx)
            }
            "enter" | "space" => match self.ai.popup_selected {
                0 => {
                    self.ai.sources_expanded = !self.ai.sources_expanded;
                    self.ai.review_expanded = false;
                }
                1 => {
                    self.close_ai_popups();
                    self.ai.context_choices_open = true;
                }
                2 => {
                    self.ai.review_expanded = !self.ai.review_expanded;
                    self.ai.sources_expanded = false;
                }
                3 => {
                    self.ai.work_log_expanded = !self.ai.work_log_expanded;
                    self.close_ai_popups();
                }
                4 => self.switch_ai_chat(-1, cx),
                5 => self.switch_ai_chat(1, cx),
                _ => self.toggle_ai_chat(window, cx),
            },
            _ => return false,
        }
        cx.notify();
        true
    }
}
