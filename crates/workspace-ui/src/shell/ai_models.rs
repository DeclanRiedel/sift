use super::*;

/// Desktop-local capabilities reported by the installed provider adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiModelOption {
    pub id: String,
    pub name: String,
    pub reasoning: Vec<String>,
    pub default_reasoning: Option<String>,
    pub is_default: bool,
}

impl WorkspaceShell {
    pub(super) fn load_ai_models(&mut self, cx: &mut Context<Self>) {
        if self.ai.models_loading {
            return;
        }
        if let Some(sender) = &self.executor_sender {
            self.ai.models_loading = sender
                .send(ExecutorCommand::LoadAiModels {
                    provider: self.ai.provider,
                })
                .is_ok();
            self.ai.models_error = None;
            cx.notify();
        }
    }

    pub(super) fn render_ai_model_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.ai.model_input.read(cx).text().trim();
        let current = self.ai.models.iter().find(|model| model.id == selected);
        div()
            .id("ai-model-picker")
            .debug_selector(|| "ai-model-picker".into())
            .on_mouse_down_out(
                cx.listener(|shell, event, _, cx| shell.dismiss_ai_popup_outside(event, cx)),
            )
            .flex()
            .flex_col()
            .gap_1()
            .max_h(px(280.))
            .overflow_y_scroll()
            .child(
                div().flex().flex_wrap().gap_1().children(
                    [
                        sift_protocol::AiProvider::Codex,
                        sift_protocol::AiProvider::ClaudeCode,
                        sift_protocol::AiProvider::OpenCode,
                    ]
                    .into_iter()
                    .enumerate()
                    .map(|(index, provider)| {
                        Button::new(
                            format!("ai-provider-{provider:?}"),
                            ai_provider_label(provider),
                        )
                        .tone(if self.ai.popup_selected == index {
                            ButtonTone::Neutral
                        } else if self.ai.provider == provider {
                            ButtonTone::Accent
                        } else {
                            ButtonTone::Ghost
                        })
                        .disabled(self.ai.pending)
                        .on_click(cx.listener(move |shell, _, _, cx| {
                            if shell.ai.pending || shell.ai.provider == provider {
                                return;
                            }
                            shell.ai.provider = provider;
                            shell.ai.models.clear();
                            shell.ai.models_loading = false;
                            shell.ai.models_error = None;
                            shell.ai.reasoning_effort = None;
                            shell
                                .ai
                                .model_input
                                .update(cx, |input, cx| input.set_text("", cx));
                            shell.load_ai_models(cx);
                        }))
                    }),
                ),
            )
            .when(self.ai.models_loading, |view| {
                view.child(div().text_xs().child("Loading models…"))
            })
            .children(
                self.ai
                    .models_error
                    .as_ref()
                    .map(|error| div().text_xs().whitespace_normal().child(error.clone())),
            )
            .child(
                div()
                    .id("ai-model-options")
                    .track_scroll(&self.ai.model_scroll)
                    .max_h(px(120.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .children(
                        self.ai
                            .models
                            .iter()
                            .cloned()
                            .enumerate()
                            .map(|(index, model)| {
                                let active = selected == model.id;
                                Button::new(format!("ai-model-{}", model.id), model.name.clone())
                                    .align_start()
                                    .tone(if self.ai.popup_selected == index + 3 {
                                        ButtonTone::Neutral
                                    } else if active {
                                        ButtonTone::Accent
                                    } else {
                                        ButtonTone::Ghost
                                    })
                                    .disabled(self.ai.pending)
                                    .on_click(cx.listener(move |shell, _, _, cx| {
                                        if shell.ai.pending {
                                            return;
                                        }
                                        shell.ai.model_input.update(cx, |input, cx| {
                                            input.set_text(model.id.clone(), cx)
                                        });
                                        shell.ai.reasoning_effort = model.default_reasoning.clone();
                                        cx.notify();
                                    }))
                            }),
                    ),
            )
            .when_some(
                current.filter(|model| !model.reasoning.is_empty()),
                |view, model| {
                    view.child(div().text_xs().child("Reasoning")).child(
                        div().flex().flex_wrap().gap_1().children(
                            model
                                .reasoning
                                .iter()
                                .cloned()
                                .enumerate()
                                .map(|(index, effort)| {
                                    let active = self.ai.reasoning_effort.as_ref() == Some(&effort);
                                    Button::new(format!("ai-reasoning-{effort}"), effort.clone())
                                        .debug_selector(format!("ai-reasoning-{effort}"))
                                        .tone(
                                            if self.ai.popup_selected
                                                == index + self.ai.models.len() + 3
                                            {
                                                ButtonTone::Neutral
                                            } else if active {
                                                ButtonTone::Accent
                                            } else {
                                                ButtonTone::Ghost
                                            },
                                        )
                                        .disabled(self.ai.pending)
                                        .on_click(cx.listener(move |shell, _, _, cx| {
                                            if shell.ai.pending {
                                                return;
                                            }
                                            shell.ai.reasoning_effort = Some(effort.clone());
                                            cx.notify();
                                        }))
                                }),
                        ),
                    )
                },
            )
            .child(div().text_xs().child("Custom model"))
            .child(self.ai.model_input.clone())
            .child(
                Button::new("ai-refresh-models", "Refresh models")
                    .tone(ButtonTone::Ghost)
                    .disabled(self.ai.models_loading || self.ai.pending)
                    .on_click(cx.listener(|shell, _, _, cx| shell.load_ai_models(cx))),
            )
            .into_any_element()
    }
}
