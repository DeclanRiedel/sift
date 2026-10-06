//! Explicit source selection; pins never upgrade when a registration changes.
use super::*;

#[derive(Debug, Clone)]
pub struct AiExternalSourceChoice {
    pub proof: sift_protocol::AiExternalSourceProof,
    pub tools: Vec<sift_protocol::AiExternalToolDefinition>,
}

#[derive(Default)]
pub(super) struct AiSourceSelection {
    pub choices: Vec<AiExternalSourceChoice>,
    pub selected: Vec<sift_protocol::AiExternalSourceProof>,
    pub loading: bool,
    pub open: bool,
    pub cursor: usize,
    pub scope: Option<(sift_protocol::AiVisibility, Option<i64>)>,
    pub focus: Option<FocusHandle>,
}
impl AiSourceSelection {
    fn toggle(&mut self, index: usize) -> Result<(), String> {
        let choice = self.choices.get(index).ok_or("No source selected")?;
        if let Some(index) = self
            .selected
            .iter()
            .position(|source| source.source_id == choice.proof.source_id)
        {
            self.selected.remove(index);
        } else {
            if self.selected.len() >= 4 {
                return Err("Choose at most four sources for a turn".into());
            }
            self.selected.push(choice.proof.clone());
        }
        Ok(())
    }
    pub(super) fn validate(
        &self,
        visibility: sift_protocol::AiVisibility,
        room: Option<i64>,
    ) -> Result<(), String> {
        if self.selected.is_empty() {
            return Ok(());
        }
        let room = if visibility == sift_protocol::AiVisibility::RoomPublic {
            room
        } else {
            None
        };
        if self.loading {
            return Err("Source review is loading; your prompt is kept".into());
        }
        if self.selected.iter().any(|proof| {
            (visibility == sift_protocol::AiVisibility::RoomPublic) != proof.room_grant_id.is_some()
        }) || self.scope != Some((visibility, room))
            || self
                .selected
                .iter()
                .any(|proof| !self.choices.iter().any(|choice| choice.proof == *proof))
        {
            return Err("A selected source changed or lost access. Remove it and review the available sources; your prompt is kept.".into());
        }
        Ok(())
    }
}

impl WorkspaceShell {
    fn open_ai_sources(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_ai_view_scope(cx);
        if self.ai.pending {
            return;
        }
        self.ai.sources.open = !self.ai.sources.open;
        if !self.ai.sources.open {
            cx.notify();
            return;
        }
        let context = match self.ai_context_snapshot(cx) {
            Ok(context) => context,
            Err(error) => {
                self.ai.error = Some(error);
                cx.notify();
                return;
            }
        };
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
        let Some(visibility) = visibility else {
            return;
        };
        let room = if visibility == sift_protocol::AiVisibility::RoomPublic {
            context.target.room_id
        } else {
            None
        };
        if visibility == sift_protocol::AiVisibility::RoomPublic && room.is_none() {
            self.ai.error = Some("Select a shared room before reviewing its sources".into());
            cx.notify();
            return;
        }
        if self.ai.sources.loading {
            return;
        }
        if self.ai.sources.scope != Some((visibility, room)) {
            self.ai.sources.choices.clear();
        }
        self.ai.sources.scope = Some((visibility, room));
        self.ai.sources.focus = Some(cx.focus_handle());
        self.ai
            .sources
            .focus
            .as_ref()
            .expect("source focus")
            .focus(window, cx);
        let Some(sender) = &self.executor_sender else {
            return;
        };
        self.ai.sources.loading = sender
            .send(ExecutorCommand::LoadAiExternalSources {
                instance_id: self
                    .selected_instance_id
                    .clone()
                    .unwrap_or_else(|| "local".into()),
                tenant_id: context.target.tenant_id.expect("AI tenant"),
                room_id: room,
            })
            .is_ok();
        cx.notify();
    }

    pub(super) fn render_ai_sources(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut view = div().flex().flex_col().gap_1().text_xs().child(
            Button::new(
                "ai-sources",
                format!("Sources · {}/4 selected", self.ai.sources.selected.len()),
            )
            .tone(ButtonTone::Ghost)
            .on_click(cx.listener(|shell, _, window, cx| shell.open_ai_sources(window, cx))),
        );
        if !self.ai.sources.open {
            for proof in &self.ai.sources.selected {
                let changed = !self
                    .ai
                    .sources
                    .choices
                    .iter()
                    .any(|choice| choice.proof == *proof);
                view = view.child(format!(
                    "{}{}",
                    proof.label,
                    if changed { " · review needed" } else { "" }
                ));
            }
            return view.into_any_element();
        }
        if let Some(focus) = &self.ai.sources.focus {
            view = view.track_focus(focus);
        }
        view = view.on_key_down(
            cx.listener(|shell, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.modifiers.modified() {
                    return;
                }
                match event.keystroke.key.as_str() {
                    "j" => {
                        if !shell.ai.sources.choices.is_empty() {
                            shell.ai.sources.cursor =
                                (shell.ai.sources.cursor + 1) % shell.ai.sources.choices.len();
                        }
                    }
                    "k" => {
                        if !shell.ai.sources.choices.is_empty() {
                            shell.ai.sources.cursor =
                                (shell.ai.sources.cursor + shell.ai.sources.choices.len() - 1)
                                    % shell.ai.sources.choices.len();
                        }
                    }
                    "space" | "enter" => {
                        if !shell.ai.sources.loading {
                            if let Err(error) = shell.ai.sources.toggle(shell.ai.sources.cursor) {
                                shell.ai.error = Some(error);
                            }
                        }
                    }
                    "escape" => {
                        shell.ai.sources.open = false;
                        shell.ai.input.read(cx).focus_handle(cx).focus(window, cx);
                    }
                    _ => return,
                }
                cx.stop_propagation();
                cx.notify();
            }),
        );
        view=view.child("j/k move · Space toggles · Esc returns to prompt · No sources are selected automatically");
        view = view.child(
            Button::new("ai-sources-clear", "Clear selections")
                .tone(ButtonTone::Ghost)
                .on_click(cx.listener(|shell, _, _, cx| {
                    shell.ai.sources.selected.clear();
                    cx.notify();
                })),
        );
        if self.ai.sources.loading {
            return view.child("Loading reviewed sources…").into_any_element();
        }
        if self.ai.sources.choices.is_empty() {
            view = view.child("No reviewed sources available in this scope");
        }
        for (index, choice) in self.ai.sources.choices.iter().enumerate() {
            let selected = self
                .ai
                .sources
                .selected
                .iter()
                .any(|proof| proof.source_id == choice.proof.source_id);
            let changed = selected && !self.ai.sources.selected.contains(&choice.proof);
            view = view.child(
                Button::new(
                    format!("ai-source-{index}"),
                    format!(
                        "{} {}{}",
                        if selected { "✓" } else { "○" },
                        choice.proof.label,
                        if changed {
                            " · changed; remove and review"
                        } else {
                            ""
                        }
                    ),
                )
                .tone(if index == self.ai.sources.cursor {
                    ButtonTone::Accent
                } else {
                    ButtonTone::Ghost
                })
                .on_click(cx.listener(move |shell, _, _, cx| {
                    shell.ai.sources.cursor = index;
                    if let Err(error) = shell.ai.sources.toggle(index) {
                        shell.ai.error = Some(error);
                    }
                    cx.notify();
                })),
            );
            if index == self.ai.sources.cursor {
                for tool in &choice.tools {
                    let policy = match tool.policy {
                        sift_protocol::AiExternalToolPolicy::Read => "Read",
                        sift_protocol::AiExternalToolPolicy::LocalQueryDraft => "SQL draft",
                        sift_protocol::AiExternalToolPolicy::LocalRowDraft => "Row draft",
                        sift_protocol::AiExternalToolPolicy::LocalMigrationDraft => "Schema draft",
                        sift_protocol::AiExternalToolPolicy::Unavailable => "Unavailable",
                    };
                    view = view.child(format!(
                        "{policy} · {}",
                        tool.title.as_deref().unwrap_or(&tool.name)
                    ));
                }
            }
        }
        view.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_selection_is_bounded_and_changed_pins_require_explicit_review() {
        let choices = (0..5)
            .map(|index| AiExternalSourceChoice {
                proof: sift_protocol::AiExternalSourceProof {
                    source_id: uuid::Uuid::new_v4(),
                    source_revision: 1,
                    config_sha256: "a".repeat(64),
                    credential_identity: uuid::Uuid::new_v4(),
                    label: format!("Source {index}"),
                    room_grant_id: None,
                },
                tools: Vec::new(),
            })
            .collect();
        let mut selection = AiSourceSelection {
            choices,
            scope: Some((sift_protocol::AiVisibility::Private, None)),
            ..Default::default()
        };
        assert!(selection.selected.is_empty());
        for index in 0..4 {
            selection.toggle(index).unwrap();
        }
        assert!(selection.toggle(4).is_err());
        selection.choices[1].proof.source_revision = 2;
        assert!(selection
            .validate(sift_protocol::AiVisibility::Private, None)
            .is_err());
        assert_eq!(selection.selected[1].source_revision, 1);
        selection.toggle(1).unwrap();
        assert_eq!(selection.selected.len(), 3);
        selection.toggle(1).unwrap();
        assert!(selection
            .validate(sift_protocol::AiVisibility::Private, None)
            .is_ok());
        assert_eq!(selection.selected[3].source_revision, 2);
        assert!(selection
            .validate(sift_protocol::AiVisibility::RoomPublic, Some(1))
            .is_err());
        selection.choices.clear();
        assert!(selection
            .validate(sift_protocol::AiVisibility::Private, None)
            .is_err());
        selection.selected.clear();
        assert!(selection
            .validate(sift_protocol::AiVisibility::Private, None)
            .is_ok());
    }
}
