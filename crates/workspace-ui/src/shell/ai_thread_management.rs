//! Thread actions stay in the picker; mutations use the governed server API.
use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ThreadAction {
    Menu,
    Rename,
    Delete,
}

impl WorkspaceShell {
    pub(super) fn can_manage_ai_thread(&self, chat: &sift_protocol::AiChat) -> bool {
        self.lifecycle.identity.as_ref().is_some_and(|identity| {
            identity.principal.id == chat.owner_principal_id
                || identity.memberships.iter().any(|membership| {
                    membership.tenant_id == chat.tenant_id
                        && matches!(membership.role.as_str(), "owner" | "admin")
                })
        })
    }

    pub(super) fn start_new_ai_thread(&mut self, cx: &mut Context<Self>) {
        if self.ai.pending || self.ai.thread_pending.is_some() {
            return;
        }
        self.roll_ai_view_scope(cx);
        self.ai.live_work_log.clear();
        self.ai.streaming.clear();
        self.ai.submitted_prompt = None;
        self.ai.activity = None;
        self.ai.retry_prompt = None;
        self.ai.error_details = false;
        self.resume_ai_follow();
        self.ai.attachments = AiAttachmentState::default();
        self.ai.inclusion = Default::default();
        self.ai.chat = None;
        self.ai.new_chat_pending = true;
        self.ai.runs.clear();
        self.ai.events.clear();
        self.ai.proposals.clear();
        self.ai.database_proposals = Arc::new(Vec::new());
        self.reset_ai_database_review(cx);
        self.ai.error = None;
        self.ai.sources_expanded = false;
        self.ai.review_expanded = false;
        self.ai
            .transcript_scroll
            .set_offset(gpui::point(px(0.), px(0.)));
        cx.notify();
    }

    pub(super) fn choose_ai_thread_action(
        &mut self,
        id: uuid::Uuid,
        action: ThreadAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.ai.pending || self.ai.thread_pending.is_some() {
            return;
        }
        let Some(chat) = self
            .ai
            .chats
            .iter()
            .find(|chat| chat.id == id && self.can_manage_ai_thread(chat))
        else {
            return;
        };
        if action == ThreadAction::Rename {
            let title = chat.title.clone();
            self.ai
                .thread_title
                .update(cx, |input, cx| input.set_text(title, cx));
            self.ai.thread_title.focus_handle(cx).focus(window, cx);
        } else {
            self.ai.transcript_focus.focus(window, cx);
        }
        self.ai.thread_action = Some((id, action));
        self.ai.popup_selected = 0;
        self.ai.thread_selected = self
            .filtered_ai_threads(cx)
            .iter()
            .position(|chat| chat.id == id)
            .unwrap_or(0);
        self.ai.thread_error = None;
        cx.notify();
    }

    pub(super) fn manage_ai_thread(&mut self, rename: bool, cx: &mut Context<Self>) {
        if self.ai.pending || self.ai.thread_pending.is_some() {
            return;
        }
        let expected_action = if rename {
            ThreadAction::Rename
        } else {
            ThreadAction::Delete
        };
        let Some((id, action)) = self.ai.thread_action else {
            return;
        };
        if action != expected_action {
            return;
        }
        let Some(chat) = self
            .ai
            .chats
            .iter()
            .find(|chat| chat.id == id && self.can_manage_ai_thread(chat))
        else {
            return;
        };
        let title = self.ai.thread_title.read(cx).text().trim().to_owned();
        if rename && (title.is_empty() || title.len() > 256 || title.contains(['\r', '\n'])) {
            self.ai.thread_error = Some("Use a title of 1–256 bytes on one line.".into());
            cx.notify();
            return;
        }
        let request = rename.then_some(sift_protocol::RenameAiChatRequest {
            title,
            expected_revision: chat.revision,
        });
        // Reject observer snapshots from before this mutation without disturbing source choices.
        self.ai.view_id = uuid::Uuid::new_v4();
        self.bind_ai_view_scope();
        let Some(sender) = &self.executor_sender else {
            return;
        };
        if sender
            .send(ExecutorCommand::ManageAiThread {
                instance_id: self
                    .selected_instance_id
                    .clone()
                    .unwrap_or_else(|| "local".into()),
                chat_id: id,
                rename: request,
            })
            .is_ok()
        {
            self.ai.thread_pending = Some(id);
            self.ai.thread_error = None;
            cx.notify();
        }
    }

    pub(super) fn accept_ai_thread_change(
        &mut self,
        id: uuid::Uuid,
        result: Result<Option<sift_protocol::AiChat>, String>,
        cx: &mut Context<Self>,
    ) {
        if self.ai.thread_pending != Some(id) {
            return;
        }
        self.ai.thread_pending = None;
        match result {
            Ok(Some(chat)) => {
                if self
                    .ai
                    .chat
                    .as_ref()
                    .is_some_and(|current| current.id == id)
                {
                    self.ai.chat = Some(chat.clone());
                }
                if let Some(existing) = self.ai.chats.iter_mut().find(|current| current.id == id) {
                    *existing = chat;
                }
                self.ai.thread_action = None;
                self.ai.thread_error = None;
            }
            Ok(None) => {
                self.ai.chats.retain(|chat| chat.id != id);
                self.ai.thread_action = None;
                self.ai.thread_error = None;
                if self.ai.chat.as_ref().is_some_and(|chat| chat.id == id) {
                    self.start_new_ai_thread(cx);
                }
            }
            Err(error) => {
                if !self.ai.thread_picker_expanded {
                    self.show_error_toast(error.clone(), cx);
                }
                self.ai.thread_error = Some(error);
            }
        }
        self.ai.thread_selected = self
            .ai
            .thread_selected
            .min(self.filtered_ai_threads(cx).len().saturating_sub(1));
        if let (Some(chat), Some(sender)) = (&self.ai.chat, &self.executor_sender) {
            let _ = sender.send(ExecutorCommand::LoadAiChat {
                instance_id: self
                    .selected_instance_id
                    .clone()
                    .unwrap_or_else(|| "local".into()),
                tenant_id: chat.tenant_id,
                chat_id: Some(chat.id),
            });
        }
        cx.notify();
    }

    pub(super) fn render_ai_thread_actions(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some((id, action)) = self.ai.thread_action else {
            return div().into_any_element();
        };
        let colors = cx.theme().colors;
        let pending = self.ai.thread_pending.is_some();
        let mut view = div()
            .debug_selector(|| "ai-thread-actions".into())
            .flex()
            .flex_col()
            .gap_1()
            .border_t_1()
            .border_color(colors.subtle_border)
            .pt_1()
            .text_xs();
        view = view.child(
            div().px_2().truncate().text_color(colors.muted_text).child(
                self.ai
                    .chats
                    .iter()
                    .find(|chat| chat.id == id)
                    .map_or("Thread actions", |chat| chat.title.as_str())
                    .to_owned(),
            ),
        );
        match action {
            ThreadAction::Menu => {
                for (index, (action, label, selector)) in [
                    (ThreadAction::Rename, "Rename", "ai-thread-rename"),
                    (ThreadAction::Delete, "Delete…", "ai-thread-delete"),
                ]
                .into_iter()
                .enumerate()
                {
                    view = view.child(
                        Button::new(selector, label)
                            .debug_selector(selector)
                            .tone(ButtonTone::Ghost)
                            .align_start()
                            .full_width()
                            .start_icon(if index == self.ai.popup_selected {
                                IconName::ChevronRight
                            } else if action == ThreadAction::Rename {
                                IconName::Edit
                            } else {
                                IconName::Close
                            })
                            .disabled(pending)
                            .on_click(cx.listener(move |shell, _, window, cx| {
                                shell.choose_ai_thread_action(id, action, window, cx)
                            })),
                    );
                }
            }
            ThreadAction::Rename => {
                view = view.child(self.ai.thread_title.clone()).child(
                    div()
                        .flex()
                        .justify_end()
                        .gap_1()
                        .child(
                            Button::new("ai-thread-action-cancel", "Cancel")
                                .tone(ButtonTone::Ghost)
                                .disabled(pending)
                                .on_click(cx.listener(|shell, _, window, cx| {
                                    shell.ai.thread_action = None;
                                    shell.ai.transcript_focus.focus(window, cx);
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new(
                                "ai-thread-save-title",
                                if pending { "Saving…" } else { "Save" },
                            )
                            .debug_selector("ai-thread-save-title")
                            .tone(ButtonTone::Ghost)
                            .disabled(pending)
                            .on_click(
                                cx.listener(|shell, _, _, cx| shell.manage_ai_thread(true, cx)),
                            ),
                        ),
                );
            }
            ThreadAction::Delete => {
                view = view
                    .child(
                        div()
                            .px_2()
                            .whitespace_normal()
                            .child("Delete this thread and its history? This cannot be undone."),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_1()
                            .child(
                                Button::new("ai-thread-action-cancel", "Cancel")
                                    .tone(ButtonTone::Ghost)
                                    .disabled(pending)
                                    .on_click(cx.listener(|shell, _, _, cx| {
                                        shell.ai.thread_action = None;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new(
                                    "ai-thread-confirm-delete",
                                    if pending { "Deleting…" } else { "Delete" },
                                )
                                .debug_selector("ai-thread-confirm-delete")
                                .tone(ButtonTone::Ghost)
                                .disabled(pending)
                                .on_click(
                                    cx.listener(|shell, _, _, cx| {
                                        shell.manage_ai_thread(false, cx)
                                    }),
                                ),
                            ),
                    );
            }
        }
        view.children(self.ai.thread_error.as_ref().map(|error| {
            div()
                .px_2()
                .whitespace_normal()
                .text_color(colors.danger)
                .child(error.clone())
        }))
        .into_any_element()
    }
}
