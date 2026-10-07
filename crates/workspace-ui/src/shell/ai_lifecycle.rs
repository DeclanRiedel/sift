//! Scope asynchronous AI presentation to its original instance, tenant and view.
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
        }
    }

    fn bind_ai_view_scope(&mut self) {
        let scope = self.current_ai_view_scope();
        if let Some(sender) = &mut self.executor_sender {
            if sender.ai_scope() != Some(&scope) {
                *sender = sender.with_ai_scope(scope.clone());
            }
        }
        self.ai.bound_scope = Some(scope);
    }

    pub(super) fn roll_ai_view_scope(&mut self, cx: &mut Context<Self>) {
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
                    let _ = sender.send(ExecutorCommand::StopAiTurn);
                }
            }
            if self.ai.input.read(cx).text().is_empty() {
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
            self.ai.runs.clear();
            self.ai.events.clear();
            self.ai.proposals.clear();
            self.ai.database_proposals = Arc::new(Vec::new());
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
            self.ai.streaming.clear();
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
