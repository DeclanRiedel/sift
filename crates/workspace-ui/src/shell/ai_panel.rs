//! Minimal AI thread panel, independent from the result inspector.
use super::*;

impl WorkspaceShell {
    pub(super) fn render_ai_chat(&self, panel_width: f32, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors;
        let popup_width = (panel_width - 25.).max(0.);
        let context_preview = self.ai_context_snapshot(cx);
        let messages = self
            .ai
            .runs
            .iter()
            .flat_map(|run| {
                let has_completed = self.ai.events.iter().any(|event| {
                    event.run_id == run.run.id
                        && event.kind == sift_protocol::AiEventKind::MessageCompleted
                });
                let answer_kind = if has_completed {
                    sift_protocol::AiEventKind::MessageCompleted
                } else {
                    sift_protocol::AiEventKind::MessageDelta
                };
                let answer = self
                    .ai
                    .events
                    .iter()
                    .filter(|event| event.run_id == run.run.id && event.kind == answer_kind)
                    .filter_map(|event| {
                        event
                            .content
                            .as_ref()
                            .and_then(|content| content.get("text"))
                            .and_then(serde_json::Value::as_str)
                    })
                    .collect::<String>();
                [
                    ("You", run.prompt.clone()),
                    (ai_provider_label(run.run.provider), answer),
                ]
            })
            .collect::<Vec<_>>();
        let proposal_cards = self.ai.proposals.clone();
        let visibility = self
            .ai
            .chat
            .as_ref()
            .map(|chat| chat.visibility)
            .or_else(|| {
                self.ai
                    .policy
                    .as_ref()
                    .map(|policy| policy.new_chat_visibility)
            });
        let public_chat = visibility == Some(sift_protocol::AiVisibility::RoomPublic);
        let context_description = match &context_preview {
            Ok(context) => {
                let shared = context
                    .sql
                    .as_ref()
                    .is_some_and(|sql| sql.room_document_id.is_some());
                let sql = if public_chat && !shared {
                    "Private SQL omitted".to_owned()
                } else {
                    context.sql.as_ref().map_or_else(
                        || "SQL omitted or unavailable".into(),
                        |sql| {
                            format!(
                                "{} bytes of SQL · {}",
                                sql.text.len(),
                                sql.text.chars().take(120).collect::<String>()
                            )
                        },
                    )
                };
                let source = if self.ai.context_origin == WorkspaceSurface::Results {
                    "Executed query"
                } else {
                    "Editor"
                };
                if public_chat {
                    format!("Context: {source} · {sql} · server checks room revision")
                } else {
                    format!(
                        "Context: {source} · {} · {sql}{}",
                        context.environment_label.as_deref().unwrap_or(
                            if context.inclusion.environment {
                                "No source connection"
                            } else {
                                "Connection labels excluded"
                            }
                        ),
                        if context.current_error.is_some() {
                            " · query error included"
                        } else {
                            ""
                        }
                    )
                }
            }
            Err(error) => format!("Context unavailable: {error}"),
        };
        let work_log = self
            .ai
            .events
            .iter()
            .filter(|event| {
                matches!(
                    event.kind,
                    sift_protocol::AiEventKind::ToolRequested
                        | sift_protocol::AiEventKind::ProgressSummary
                )
            })
            .map(|event| {
                if event.kind == sift_protocol::AiEventKind::ProgressSummary {
                    let text = event
                        .content
                        .as_ref()
                        .and_then(|content| content.get("text"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("");
                    let provider = self
                        .ai
                        .runs
                        .iter()
                        .find(|run| run.run.id == event.run_id)
                        .map(|run| ai_provider_label(run.run.provider))
                        .unwrap_or("AI");
                    return format!(
                        "{provider} · {}",
                        text.chars().take(500).collect::<String>()
                    );
                }
                let name = event
                    .content
                    .as_ref()
                    .and_then(|content| content.get("tool"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("tool");
                let outcome = self.ai.events.iter().find(|candidate| {
                    candidate.run_id == event.run_id
                        && candidate.tool_call_id == event.tool_call_id
                        && matches!(
                            candidate.kind,
                            sift_protocol::AiEventKind::ToolCompleted
                                | sift_protocol::AiEventKind::ToolDenied
                        )
                });
                format!(
                    "{name} · {}",
                    match outcome.map(|event| event.kind) {
                        Some(sift_protocol::AiEventKind::ToolCompleted) => "completed",
                        Some(sift_protocol::AiEventKind::ToolDenied) => "denied",
                        _ => "running",
                    }
                )
            })
            .collect::<Vec<_>>();
        let header = div()
            .debug_selector(|| "ai-thread-header".into())
            .flex()
            .items_center()
            .gap_1()
            .child(
                IconButton::new("ai-follow-agent", IconName::View, "Follow agent")
                    .debug_selector("ai-follow-agent")
                    .toggle_state(self.ai.follow_agent)
                    .on_click(cx.listener(|shell, _, _, cx| {
                        shell.ai.follow_agent = !shell.ai.follow_agent;
                        if shell.ai.follow_agent {
                            shell.resume_ai_follow();
                        }
                        cx.notify();
                    })),
            )
            .child(
                self.render_ai_popup_trigger(
                    1,
                    div()
                        .id("ai-thread-title")
                        .debug_selector(|| "ai-thread-title".into())
                        .role(Role::Button)
                        .aria_label("Choose a thread")
                        .cursor(CursorStyle::PointingHand)
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .on_click(cx.listener(|shell, _, window, cx| {
                            let expanded = !shell.ai.thread_picker_expanded;
                            shell.close_ai_popups();
                            shell.ai.thread_picker_expanded = expanded;
                            shell.ai.thread_selected = 0;
                            shell
                                .ai
                                .thread_search
                                .update(cx, |input, cx| input.set_text("", cx));
                            shell.ai.transcript_focus.focus(window, cx);
                            cx.notify();
                        }))
                        .child(
                            self.ai
                                .chat
                                .as_ref()
                                .map_or_else(|| "New thread".to_owned(), |chat| chat.title.clone()),
                        ),
                ),
            )
            .child(
                self.render_ai_popup_trigger(
                    2,
                    IconButton::new("ai-thread-settings", IconName::Settings, "Thread settings")
                        .debug_selector("ai-thread-settings")
                        .toggle_state(self.ai.settings_expanded)
                        .on_click(cx.listener(|shell, _, window, cx| {
                            let expanded = !shell.ai.settings_expanded;
                            shell.close_ai_popups();
                            shell.ai.settings_expanded = expanded;
                            shell.ai.transcript_focus.focus(window, cx);
                            cx.notify();
                        })),
                ),
            )
            .child(
                IconButton::new("ai-new-chat", IconName::Add, "New thread")
                    .debug_selector("ai-new-chat")
                    .disabled(self.ai.pending)
                    .on_click(cx.listener(|shell, _, _, cx| {
                        if shell.ai.pending {
                            return;
                        }
                        shell.close_ai_popups();
                        shell.ai.text_selection.borrow_mut().clear();
                        shell.ai.live_work_log.clear();
                        shell.ai.streaming.clear();
                        shell.ai.submitted_prompt = None;
                        shell.ai.activity = None;
                        shell.resume_ai_follow();
                        shell.ai.attachments = AiAttachmentState::default();
                        shell.ai.inclusion = Default::default();
                        shell.roll_ai_view_scope(cx);
                        shell.ai.chat = None;
                        shell.ai.new_chat_pending = true;
                        shell.ai.runs.clear();
                        shell.ai.events.clear();
                        shell.ai.proposals.clear();
                        shell.ai.database_proposals = Arc::new(Vec::new());
                        shell.reset_ai_database_review(cx);
                        shell.ai.error = None;
                        shell.ai.menu_expanded = false;
                        shell.ai.settings_expanded = false;
                        shell.ai.context_choices_open = false;
                        shell.ai.sources_expanded = false;
                        shell.ai.review_expanded = false;
                        shell.ai.model_picker_expanded = false;
                        shell.ai.permission_picker_expanded = false;
                        shell
                            .ai
                            .transcript_scroll
                            .set_offset(gpui::point(px(0.), px(0.)));
                        cx.notify();
                    })),
            )
            .child(
                self.render_ai_popup_trigger(
                    0,
                    IconButton::new("ai-thread-menu", IconName::Menu, "Thread menu")
                        .debug_selector("ai-thread-menu")
                        .toggle_state(self.ai.menu_expanded)
                        .on_click(cx.listener(|shell, _, window, cx| {
                            let expanded = !shell.ai.menu_expanded;
                            shell.close_ai_popups();
                            shell.ai.menu_expanded = expanded;
                            shell.ai.transcript_focus.focus(window, cx);
                            cx.notify();
                        })),
                ),
            );
        let timeline = div()
            .id("ai-chat-timeline")
            .debug_selector(|| "ai-chat-timeline".into())
            .track_focus(&self.ai.transcript_focus)
            .w_full()
            .min_w_0()
            .when(!self.ai.attachments.accepted.is_empty(), |view| {
                view.pt(px(self.ai.attachments.accepted.len() as f32 * 28.))
            })
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.ai.transcript_scroll)
            .on_scroll_wheel(
                cx.listener(|shell, event: &gpui::ScrollWheelEvent, window, cx| {
                    if event.delta.pixel_delta(window.line_height()).y > px(0.)
                        && shell.ai.transcript_scroll.max_offset().y > px(0.)
                    {
                        shell.ai.follow_agent = false;
                        cx.notify();
                    }
                }),
            )
            .flex()
            .flex_col()
            .gap_2()
            .when(
                messages.is_empty() && !self.ai.pending && self.ai.streaming.is_empty(),
                |view| {
                    view.child(
                        div()
                            .debug_selector(|| "ai-empty-thread".into())
                            .flex()
                            .flex_col()
                            .pt_2()
                            .gap_2()
                            .text_color(colors.muted_text)
                            .child("What are you working on?")
                            .child(
                                div()
                                    .text_xs()
                                    .child("Ask about SQL, explore your schema, or draft a query."),
                            ),
                    )
                },
            )
            .children(
                messages
                    .into_iter()
                    .filter(|(_, text)| !text.is_empty())
                    .enumerate()
                    .map(|(index, (role, text))| {
                        div()
                            .w_full()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .py_1()
                            .child(self.render_ai_message_author(role, cx))
                            .child(self.render_ai_markdown(&format!("message-{index}"), &text, cx))
                    }),
            )
            .children(
                self.ai
                    .submitted_prompt
                    .as_ref()
                    .filter(|_| self.ai.pending || !self.ai.streaming.is_empty())
                    .map(|prompt| {
                        div()
                            .w_full()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .py_1()
                            .child(self.render_ai_message_author("You", cx))
                            .child(self.render_ai_markdown("pending", prompt, cx))
                    }),
            )
            .when(self.ai.work_log_expanded, |view| {
                view.children(
                    work_log
                        .into_iter()
                        .chain(self.ai.live_work_log.iter().cloned())
                        .map(|activity| {
                            div()
                                .debug_selector(|| "ai-work-log-entry".into())
                                .text_xs()
                                .text_color(colors.muted_text)
                                .whitespace_normal()
                                .child(activity)
                        }),
                )
            })
            .children(proposal_cards.into_iter().map(|proposal| {
                let id = proposal.proposal.id;
                let staged = proposal.proposal.status == sift_protocol::AiProposalStatus::Staged;
                div()
                    .whitespace_normal()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .py_1()
                    .children(proposal.external_origin.as_ref().map(|origin| {
                        div().text_xs().child(format!(
                            "Source intent · {} · {} · reviewed revision {}",
                            origin.source.label, origin.tool_alias, origin.source.source_revision
                        ))
                    }))
                    .child(
                        div()
                            .text_xs()
                            .text_color(colors.muted_text)
                            .child(format!("SQL draft · {:?}", proposal.proposal.status)),
                    )
                    .child(self.render_ai_code(
                        &format!("proposal-{id}"),
                        "sql",
                        &proposal.proposed_sql,
                        cx,
                    ))
                    .when(staged, |view| {
                        view.child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap_1()
                                .child(
                                    Button::new(
                                        format!("ai-apply-proposal-{id}"),
                                        "Replace editor SQL",
                                    )
                                    .on_click(cx.listener(
                                        move |shell, _, _, cx| shell.apply_ai_proposal(id, cx),
                                    )),
                                )
                                .child(
                                    Button::new(
                                        format!("ai-copy-proposal-{id}"),
                                        "Open as new query",
                                    )
                                    .tone(ButtonTone::Ghost)
                                    .on_click(cx.listener(
                                        move |shell, _, window, cx| {
                                            shell.open_ai_proposal_copy(id, window, cx)
                                        },
                                    )),
                                ),
                        )
                    })
            }))
            .child(self.render_ai_database_reviews(cx))
            .when(!self.ai.streaming.is_empty(), |view| {
                view.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .py_1()
                        .child(
                            self.render_ai_message_author(ai_provider_label(self.ai.provider), cx),
                        )
                        .child(self.render_ai_markdown("streaming", &self.ai.streaming, cx)),
                )
            })
            .children(self.ai.error.as_ref().map(|error| {
                div()
                    .text_color(colors.danger)
                    .whitespace_normal()
                    .child(error.clone())
            }));
        let timeline = div()
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .w_full()
            .child(timeline)
            .when(!self.ai.follow_agent && self.ai.unseen_content, |view| {
                view.child(
                    div()
                        .absolute()
                        .bottom_2()
                        .right_2()
                        .border_1()
                        .border_color(colors.subtle_border)
                        .rounded_md()
                        .bg(colors.elevated_surface)
                        .occlude()
                        .child(
                            IconButton::new(
                                "ai-jump-latest",
                                IconName::ChevronDown,
                                "Jump to latest",
                            )
                            .text("Jump to latest")
                            .debug_selector("ai-jump-latest")
                            .on_click(cx.listener(
                                |shell, _, _, cx| {
                                    shell.resume_ai_follow();
                                    cx.notify();
                                },
                            )),
                        ),
                )
            });
        let composer = div()
            .flex_none()
            .border_t_1()
            .border_color(colors.subtle_border)
            .pt_2()
            .flex()
            .flex_col()
            .gap_2()
            .when(self.ai.context_choices_open, |view| {
                view.child(
                    div()
                        .id("ai-context-panel")
                        .debug_selector(|| "ai-context-panel".into())
                        .on_mouse_down_out(cx.listener(|shell, event, _, cx| {
                            shell.dismiss_ai_popup_outside(event, cx)
                        }))
                        .max_h(px(220.))
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .text_color(colors.muted_text)
                                .whitespace_normal()
                                .child(context_description),
                        )
                        .when_some(
                            context_preview
                                .as_ref()
                                .ok()
                                .and_then(|context| context.workspace.as_ref()),
                            |view, workspace| {
                                view.child(Self::render_ai_context_state(workspace, cx))
                            },
                        )
                        .child(self.render_ai_context_choices(cx)),
                )
            })
            .when(self.ai.model_picker_expanded, |view| {
                view.child(self.render_ai_model_picker(cx))
            })
            .when(self.ai.permission_picker_expanded, |view| {
                view.child(
                    div()
                        .id("ai-permission-picker")
                        .debug_selector(|| "ai-permission-picker".into())
                        .on_mouse_down_out(cx.listener(|shell, event, _, cx| {
                            shell.dismiss_ai_popup_outside(event, cx)
                        }))
                        .flex()
                        .gap_1()
                        .children(
                            [
                                (sift_protocol::AiMode::Read, "Read"),
                                (sift_protocol::AiMode::Propose, "Propose"),
                            ]
                            .into_iter()
                            .map(|(mode, label)| {
                                Button::new(format!("ai-mode-{mode:?}"), label)
                                    .tone(if self.ai.mode == mode {
                                        ButtonTone::Accent
                                    } else {
                                        ButtonTone::Ghost
                                    })
                                    .disabled(self.ai.pending)
                                    .on_click(cx.listener(move |shell, _, _, cx| {
                                        if shell.ai.pending {
                                            return;
                                        }
                                        shell.ai.mode = mode;
                                        shell.ai.permission_picker_expanded = false;
                                        cx.notify();
                                    }))
                            }),
                        ),
                )
            })
            .when(self.ai.pending || self.ai.activity.is_some(), |view| {
                view.child(
                    div()
                        .debug_selector(|| "ai-activity-status".into())
                        .flex()
                        .items_center()
                        .gap_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(colors.muted_text)
                        .child(icon(IconName::Activity, colors.muted_text, 12.))
                        .child(
                            div().min_w_0().truncate().child(
                                self.ai
                                    .activity
                                    .as_deref()
                                    .map(ai_feedback::activity_label)
                                    .unwrap_or("Thinking…"),
                            ),
                        ),
                )
            })
            .child(self.ai.input.clone())
            .child(
                div()
                    .relative()
                    .debug_selector(|| "ai-composer-footer".into())
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_1()
                    .child(
                        self.render_ai_popup_trigger(
                            3,
                            IconButton::new("ai-context-toggle", IconName::Document, "Context")
                                .debug_selector("ai-context-toggle")
                                .badge(
                                    (!self.ai.attachments.accepted.is_empty())
                                        .then_some(self.ai.attachments.accepted.len()),
                                )
                                .toggle_state(self.ai.context_choices_open)
                                .on_click(cx.listener(|shell, _, window, cx| {
                                    let expanded = !shell.ai.context_choices_open;
                                    shell.close_ai_popups();
                                    shell.ai.context_choices_open = expanded;
                                    shell.ai.transcript_focus.focus(window, cx);
                                    shell.ai.model_picker_expanded = false;
                                    shell.ai.permission_picker_expanded = false;
                                    cx.notify();
                                })),
                        ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .min_w_0()
                            .child(
                                self.render_ai_popup_trigger(
                                    4,
                                    IconButton::new(
                                        "ai-model-selector",
                                        IconName::ChevronDown,
                                        "Choose provider and model",
                                    )
                                    .debug_selector("ai-model-selector")
                                    .text(self.ai_model_label(cx))
                                    .toggle_state(self.ai.model_picker_expanded)
                                    .disabled(self.ai.pending)
                                    .on_click(cx.listener(
                                        |shell, _, window, cx| {
                                            let expanded = !shell.ai.model_picker_expanded;
                                            shell.close_ai_popups();
                                            shell.ai.model_picker_expanded = expanded;
                                            shell.ai.transcript_focus.focus(window, cx);
                                            shell.ai.permission_picker_expanded = false;
                                            shell.ai.context_choices_open = false;
                                            if shell.ai.model_picker_expanded
                                                && shell.ai.models.is_empty()
                                                && !shell.ai.models_loading
                                            {
                                                shell.load_ai_models(cx);
                                            }
                                            cx.notify();
                                        },
                                    )),
                                ),
                            )
                            .child(
                                self.render_ai_popup_trigger(
                                    5,
                                    IconButton::new(
                                        "ai-permission-selector",
                                        IconName::ChevronDown,
                                        "Sift permission level",
                                    )
                                    .debug_selector("ai-permission-selector")
                                    .text(match self.ai.mode {
                                        sift_protocol::AiMode::Read => "Read",
                                        sift_protocol::AiMode::Propose => "Propose",
                                    })
                                    .toggle_state(self.ai.permission_picker_expanded)
                                    .disabled(self.ai.pending)
                                    .on_click(cx.listener(
                                        |shell, _, window, cx| {
                                            let expanded = !shell.ai.permission_picker_expanded;
                                            shell.close_ai_popups();
                                            shell.ai.permission_picker_expanded = expanded;
                                            shell.ai.transcript_focus.focus(window, cx);
                                            shell.ai.model_picker_expanded = false;
                                            shell.ai.context_choices_open = false;
                                            cx.notify();
                                        },
                                    )),
                                ),
                            )
                            .when(!self.ai.pending, |view| {
                                view.child(
                                    IconButton::new("ai-send", IconName::Play, "Send message")
                                        .debug_selector("ai-send")
                                        .on_click(
                                            cx.listener(|shell, _, _, cx| shell.send_ai_turn(cx)),
                                        ),
                                )
                            })
                            .when(self.ai.pending, |view| {
                                view.child(
                                    IconButton::new("ai-stop", IconName::Close, "Stop agent")
                                        .debug_selector("ai-stop")
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            if let Some(sender) = &shell.executor_sender {
                                                let _ = sender.send(ExecutorCommand::StopAiTurn);
                                            }
                                            cx.notify();
                                        })),
                                )
                            }),
                    ),
            );
        let settings = self.ai.settings_expanded.then(|| {
            div()
                .id("ai-thread-settings-panel")
                .debug_selector(|| "ai-thread-settings-panel".into())
                .on_mouse_down_out(
                    cx.listener(|shell, event, _, cx| shell.dismiss_ai_popup_outside(event, cx)),
                )
                .p_2()
                .text_xs()
                .flex()
                .flex_col()
                .gap_1()
                .child(match visibility {
                    Some(sift_protocol::AiVisibility::RoomPublic) => {
                        "Room public · visible to room members"
                    }
                    Some(sift_protocol::AiVisibility::Private) => "Private · only you",
                    None => "Loading thread policy…",
                })
                .child(
                    Button::new("ai-default-model", "Use provider default model")
                        .tone(ButtonTone::Ghost)
                        .disabled(self.ai.pending)
                        .on_click(cx.listener(|shell, _, _, cx| {
                            if shell.ai.pending {
                                return;
                            }
                            shell
                                .ai
                                .model_input
                                .update(cx, |input, cx| input.set_text("", cx));
                            cx.notify();
                        })),
                )
        });
        let menu = self.ai.menu_expanded.then(|| {
            let mut menu = div()
                .id("ai-thread-menu-panel")
                .debug_selector(|| "ai-thread-menu-panel".into())
                .on_mouse_down_out(cx.listener(|shell, event, _, cx| shell.dismiss_ai_popup_outside(event, cx)))
                .absolute().right_3().top(px(40.))
                .w(px(if self.ai.sources_expanded || self.ai.review_expanded { 360. } else { 248. }))
                .max_w(px(popup_width)).max_h(px(260.))
                .bg(colors.elevated_surface).border_1().border_color(colors.subtle_border)
                .rounded_md().shadow_md().occlude()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .overflow_y_scroll().flex().flex_col().gap_1().p_1()
                .child(Button::new("ai-menu-sources", "Sources")
                    .tone(if self.ai.popup_selected == 0 { ButtonTone::Neutral } else { ButtonTone::Ghost }).align_start().start_icon(IconName::Database)
                    .debug_selector("ai-menu-sources")
                    .on_click(cx.listener(|shell, _, _, cx| {
                        shell.ai.sources_expanded = !shell.ai.sources_expanded;
                        shell.ai.review_expanded = false;
                        cx.notify();
                    })));
            if self.ai.sources_expanded {
                menu = menu.child(self.render_ai_sources(cx)).child(self.render_ai_source_manager(cx));
            }
            menu = menu
                .child(Button::new("ai-menu-context", "Review automatic context")
                    .tone(if self.ai.popup_selected == 1 { ButtonTone::Neutral } else { ButtonTone::Ghost }).align_start().start_icon(IconName::Document)
                    .on_click(cx.listener(|shell, _, _, cx| {
                        shell.ai.context_choices_open = true;
                        shell.ai.model_picker_expanded = false;
                        shell.ai.permission_picker_expanded = false;
                        shell.ai.menu_expanded = false;
                        cx.notify();
                    })))
                .child(Button::new("ai-menu-review", "Attachments and sharing")
                    .tone(if self.ai.popup_selected == 2 { ButtonTone::Neutral } else { ButtonTone::Ghost }).align_start().start_icon(IconName::Users)
                    .on_click(cx.listener(|shell, _, _, cx| {
                        shell.ai.review_expanded = !shell.ai.review_expanded;
                        shell.ai.sources_expanded = false;
                        cx.notify();
                    })));
            if self.ai.review_expanded {
                menu = menu.child(self.render_ai_attachments(cx))
                            .when(public_chat, |view| {
                                let room=self.ai_publication_room(cx);
                                let publication=self.ai.publication.as_ref().filter(|grant|Some(grant.source.room_id)==room);
                                let preview=self.ai.publication_preview.as_ref().filter(|preview|Some(preview.room_id)==room);
                                view.child(div().text_xs().flex().flex_col().gap_1()
                                    .child(publication.map_or_else(||"Database context unpublished · owner review required".into(),|grant|format!("Published: {} · {} · {}",grant.source.profile_name,grant.source.database.as_deref().unwrap_or("default database"),if grant.allow_rows {"schema, estimated plans and bounded rows"}else{"schema and estimated plans"})))
                                    .child(Button::new("ai-review-publication","Review database publication").tone(ButtonTone::Ghost).align_start()
                                        .on_click(cx.listener(|shell,_,_,cx|shell.review_ai_publication(cx))))
                                    .when(publication.is_some(),|view|view.child(Button::new("ai-revoke-publication","Revoke future database reads").tone(ButtonTone::Ghost).align_start()
                                        .on_click(cx.listener(|shell,_,_,cx|shell.change_ai_publication(None,cx)))))
                                    .when_some(preview,|view,preview|view.child(div().whitespace_normal()
                                        .child(format!("Share {} / {} ({}) with current and future room members. Published content remains in chat history after revocation.",preview.profile_name,preview.database.as_deref().unwrap_or("default database"),preview.dialect))
                                        .child(div().flex().flex_wrap().gap_1()
                                            .child(Button::new("ai-publish-schema","Publish schema and plans").on_click(cx.listener(|shell,_,_,cx|shell.change_ai_publication(Some(false),cx))))
                                            .child(Button::new("ai-publish-rows","Also publish bounded rows").tone(ButtonTone::Ghost).align_start().on_click(cx.listener(|shell,_,_,cx|shell.change_ai_publication(Some(true),cx))))))))
                            });
            }
            menu
                .child(Button::new("ai-menu-work-log", if self.ai.work_log_expanded { "Hide work log" } else { "Show work log" })
                    .tone(if self.ai.popup_selected == 3 { ButtonTone::Neutral } else { ButtonTone::Ghost }).align_start().start_icon(IconName::Activity)
                    .on_click(cx.listener(|shell, _, _, cx| {
                        shell.ai.work_log_expanded = !shell.ai.work_log_expanded;
                        shell.ai.menu_expanded = false;
                        cx.notify();
                    })))
                .child(div().flex().items_center().justify_between().border_t_1().border_color(colors.subtle_border).pt_1()
                    .child(div().flex().gap_1()
                        .child(IconButton::new("ai-previous-chat", IconName::ChevronLeft, "Previous thread")
                            .toggle_state(self.ai.popup_selected == 4)
                        .on_click(cx.listener(|shell, _, _, cx| shell.switch_ai_chat(-1, cx))))
                        .child(IconButton::new("ai-next-chat", IconName::ChevronRight, "Next thread")
                            .toggle_state(self.ai.popup_selected == 5)
                        .on_click(cx.listener(|shell, _, _, cx| shell.switch_ai_chat(1, cx)))))
                    .child(IconButton::new("ai-close-panel", IconName::Close, "Close AI panel")
                        .toggle_state(self.ai.popup_selected == 6)
                        .on_click(cx.listener(|shell, _, window, cx| shell.toggle_ai_chat(window, cx)))))
        });
        div()
            .size_full()
            .flex()
            .flex_col()
            .min_h_0()
            .child(
                div()
                    .id("ai-chat-panel")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|shell, _, _, cx| {
                            shell.focused_surface = WorkspaceSurface::Ai;
                            cx.notify();
                        }),
                    )
                    .on_key_down(
                        cx.listener(|shell, event: &gpui::KeyDownEvent, window, cx| {
                            if shell.handle_ai_popup_key(event, window, cx)
                                || shell.handle_ai_transcript_scroll_key(event, window, cx)
                            {
                                cx.stop_propagation();
                                return;
                            }
                            if event.keystroke.key == "escape" {
                                shell.focus_active_pane(window, cx);
                                cx.stop_propagation();
                            }
                        }),
                    )
                    .debug_selector(|| "ai-chat-dock".into())
                    .relative()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .min_w_0()
                    .text_sm()
                    .flex_1()
                    .min_h_0()
                    .child(header)
                    .children(settings)
                    .child(timeline)
                    .child(composer)
                    .when(!self.ai.attachments.accepted.is_empty(), |view| {
                        view.child(self.render_ai_attachment_overlay(popup_width, cx))
                    })
                    .children(menu)
                    .when(self.ai.thread_picker_expanded, |view| {
                        view.child(self.render_ai_thread_picker(popup_width, cx))
                    })
                    .child(self.render_ai_response_menu(cx)),
            )
            .into_any_element()
    }

    fn render_ai_message_author(&self, role: &str, cx: &App) -> AnyElement {
        let human = role == "You";
        let color = cx.theme().colors.muted_text;
        div()
            .debug_selector(move || {
                if human {
                    "ai-message-human"
                } else {
                    "ai-message-agent"
                }
                .into()
            })
            .flex()
            .items_center()
            .gap_1()
            .text_xs()
            .text_color(color)
            .child(icon(
                if human {
                    IconName::User
                } else {
                    IconName::Robot
                },
                color,
                13.,
            ))
            .child(role.to_owned())
            .into_any_element()
    }

    fn ai_model_label(&self, cx: &App) -> String {
        let selected = self.ai.model_input.read(cx).text().trim();
        let model = self
            .ai
            .models
            .iter()
            .find(|model| model.id == selected)
            .map_or(selected, |model| model.name.as_str());
        if model.is_empty() {
            format!("{} model", ai_provider_label(self.ai.provider))
        } else {
            let mut label = model.chars().take(18).collect::<String>();
            if model.chars().count() > 18 {
                label.push('…');
            }
            label
        }
    }
}

pub(super) fn resize_separator(border: gpui::Hsla) -> AnyElement {
    div()
        .relative()
        .flex_none()
        .w(px(1.))
        .h_full()
        .bg(border)
        .child(
            div()
                .id("resize-ai-panel")
                .debug_selector(|| "resize-ai-panel".into())
                .absolute()
                .left(px(-(DOCK_RESIZE_HANDLE_SIZE - 1.0) / 2.0))
                .top_0()
                .w(px(DOCK_RESIZE_HANDLE_SIZE))
                .h_full()
                .cursor(CursorStyle::ResizeLeftRight)
                .block_mouse_except_scroll()
                .on_drag(AiResizeDrag, |_, _, _, cx| cx.new(|_| gpui::Empty)),
        )
        .into_any_element()
}
