//! Scope asynchronous AI presentation to its original caller, tenant and view.
use super::*;

impl WorkspaceShell {
    pub(super) fn current_ai_view_scope(&self) -> AiViewScope {
        AiViewScope {
            instance_id: self
                .selected_instance_id
                .clone()
                .unwrap_or_else(|| "local".into()),
            tenant_id: self.selected_tenant_id(),
            view_id: self.ai.view_id,
            caller: self
                .authenticated_caller_scope(sift_protocol::handshake::CAPABILITY_AI_CALLER_SCOPE),
        }
    }

    pub(super) fn bind_ai_view_scope(&mut self) {
        let scope = self.current_ai_view_scope();
        if let Some(sender) = &mut self.executor_sender {
            if sender.ai_scope() != Some(&scope) {
                *sender = sender.with_ai_scope(scope.clone());
            }
        }
        self.ai.bound_scope = Some(scope);
    }

    pub(super) fn roll_ai_view_scope(&mut self, cx: &mut Context<Self>) {
        self.ai.pending_room_commit = None;
        self.close_ai_popups();
        self.ai.text_selection.borrow_mut().clear();
        self.ai.sources = AiSourceSelection::default();
        self.ai.source_manager = AiSourceManager::new(cx);
        self.ai.view_id = uuid::Uuid::new_v4();
        self.bind_ai_view_scope();
    }

    pub(super) fn sync_ai_view_scope(&mut self, cx: &mut Context<Self>) {
        let current = self.current_ai_view_scope();
        let changed = self
            .ai
            .bound_scope
            .as_ref()
            .is_some_and(|scope| scope != &current);
        if changed {
            self.close_ai_popups();
            self.ai.text_selection.borrow_mut().clear();
            if self.ai.pending {
                if let Some(sender) = &self.executor_sender {
                    self.ai.query_execution_stopping = true;
                    let _ = sender.send(ExecutorCommand::StopAiTurn);
                }
            }
            let caller_changed = self
                .ai
                .bound_scope
                .as_ref()
                .is_some_and(|scope| scope.caller != current.caller);
            if caller_changed {
                for input in [
                    &self.ai.input,
                    &self.ai.thread_title,
                    &self.ai.thread_search,
                ] {
                    input.update(cx, |input, cx| input.set_text("", cx));
                }
            } else if self.ai.input.read(cx).text().is_empty() {
                if let Some(prompt) = self.ai.submitted_prompt.take() {
                    self.ai
                        .input
                        .update(cx, |input, cx| input.set_text(prompt, cx));
                }
            }
            self.ai.view_id = uuid::Uuid::new_v4();
            self.ai.sources = AiSourceSelection::default();
            self.ai.source_manager = AiSourceManager::new(cx);
            self.ai.context_origin = WorkspaceSurface::Editor;
            self.ai.chat = None;
            self.ai.chats.clear();
            self.ai.thread_pending = None;
            self.ai.thread_error = None;
            self.ai.runs.clear();
            self.ai.events.clear();
            self.ai.proposals.clear();
            self.ai.database_proposals = Arc::new(Vec::new());
            self.reset_ai_sql_execution();
            self.reset_ai_database_review(cx);
            self.ai.attachments = AiAttachmentState::default();
            self.ai.publication = None;
            self.ai.publication_preview = None;
            self.ai.preferences_chat = None;
            self.ai.provider = sift_protocol::AiProvider::Codex;
            self.ai.models.clear();
            self.ai.models_loading = false;
            self.ai.models_error = None;
            self.ai.reasoning_effort = None;
            self.ai
                .model_input
                .update(cx, |input, cx| input.set_text("", cx));
            self.ai.inclusion = Default::default();
            self.ai.context_choices_open = false;
            self.ai.mode = sift_protocol::AiMode::Read;
            self.ai.policy = None;
            self.ai.new_chat_pending = false;
            self.ai.pending = false;
            self.ai.submitted_prompt = None;
            self.ai.retry_prompt = None;
            self.ai.error_details = false;
            self.ai.streaming.clear();
            self.ai.live_work_log.clear();
            self.resume_ai_follow();
            self.ai.activity = None;
            self.ai.error = None;
            self.ai.work_log_expanded = false;
        }
        self.bind_ai_view_scope();
        if changed && self.ai_dock_active {
            if let (Some(tenant_id), Some(sender)) =
                (self.selected_tenant_id(), &self.executor_sender)
            {
                let _ = sender.send(ExecutorCommand::LoadAiChat {
                    instance_id: self
                        .selected_instance_id
                        .clone()
                        .unwrap_or_else(|| "local".into()),
                    tenant_id,
                    chat_id: None,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn ai_caller_changes_clear_private_views_and_stop_only_the_original_owner(
        cx: &mut gpui::TestAppContext,
    ) {
        let window = super::super::tests::shell(cx);
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let (sender, mut commands) = ExecutorSender::channel(16);
        workspace.update(&mut cx, |shell, cx| {
            shell.executor_sender = Some(sender);
            for changed in [
                "account",
                "session",
                "endpoint",
                "daemon",
                "instance",
                "signed_out",
                "capability",
            ] {
                super::super::extension_caller::tests::authenticate(shell);
                // Extension-only support must not imply AI transport support.
                assert!(shell.current_ai_view_scope().caller.is_none());
                shell
                    .lifecycle
                    .handshake
                    .as_mut()
                    .unwrap()
                    .capabilities
                    .push(sift_protocol::handshake::CAPABILITY_AI_CALLER_SCOPE.into());
                shell.sync_ai_view_scope(cx);
                let original = shell.current_ai_view_scope();
                assert_eq!(original.caller.as_ref().unwrap().identity.principal_id, 1);
                shell.ai.pending = true;
                shell.ai.submitted_prompt = Some("Private submitted prompt".into());
                shell.ai.streaming = "Private old response".into();
                for input in [
                    &shell.ai.input,
                    &shell.ai.thread_title,
                    &shell.ai.thread_search,
                ] {
                    input.update(cx, |input, cx| input.set_text("Private draft text", cx));
                }
                match changed {
                    "account" => shell.lifecycle.identity.as_mut().unwrap().principal.id = 2,
                    "session" => {
                        shell.lifecycle.identity.as_mut().unwrap().auth_session_id =
                            Some("replacement".into())
                    }
                    "endpoint" => shell
                        .lifecycle
                        .selected_instance
                        .as_mut()
                        .unwrap()
                        .base_url
                        .push_str("/replacement"),
                    "daemon" => {
                        shell
                            .lifecycle
                            .handshake
                            .as_mut()
                            .unwrap()
                            .daemon_generation = "replacement".into()
                    }
                    "instance" => {
                        shell.lifecycle.handshake.as_mut().unwrap().instance_id =
                            "replacement".into()
                    }
                    "signed_out" => shell.lifecycle.identity = None,
                    "capability" => shell
                        .lifecycle
                        .handshake
                        .as_mut()
                        .unwrap()
                        .capabilities
                        .retain(|value| {
                            value != sift_protocol::handshake::CAPABILITY_AI_CALLER_SCOPE
                        }),
                    _ => unreachable!(),
                }
                // Event admission must synchronize even before the next render.
                shell.on_executor_event(
                    ExecutorEvent::AiScoped {
                        scope: original.clone(),
                        event: Box::new(ExecutorEvent::AiTextDelta("Late private response".into())),
                    },
                    cx,
                );
                let ExecutorCommand::AiScoped { scope, command } = commands.try_recv().unwrap()
                else {
                    panic!("Stop must retain its original caller");
                };
                assert_eq!(scope, original, "{changed}");
                assert!(matches!(*command, ExecutorCommand::StopAiTurn));
                assert!(commands.try_recv().is_err());
                assert_ne!(
                    shell.current_ai_view_scope().view_id,
                    original.view_id,
                    "{changed}"
                );
                assert!(!shell.ai.pending, "{changed}");
                assert!(shell.ai.streaming.is_empty(), "{changed}");
                for input in [
                    &shell.ai.input,
                    &shell.ai.thread_title,
                    &shell.ai.thread_search,
                ] {
                    assert!(input.read(cx).text().is_empty(), "{changed}");
                }
                assert!(shell.ai.submitted_prompt.is_none(), "{changed}");
                assert!(shell.ai.chat.is_none(), "{changed}");
                shell.on_executor_event(
                    ExecutorEvent::AiScoped {
                        scope: original,
                        event: Box::new(ExecutorEvent::AiFinished(Err(
                            "Private old failure".into()
                        ))),
                    },
                    cx,
                );
                assert!(shell.ai.error.is_none(), "{changed}");
                assert!(commands.try_recv().is_err());
            }
        });
    }
}
