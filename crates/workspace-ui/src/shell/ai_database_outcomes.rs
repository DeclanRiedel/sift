//! Human acknowledgement preserves the original uncertain database receipt.
use super::*;

impl WorkspaceShell {
    pub(super) fn ai_database_unknown_ids(&self) -> Vec<uuid::Uuid> {
        self.ai
            .database_proposals
            .iter()
            .filter(|detail| {
                detail.apply_state == Some(sift_protocol::AiDatabaseApplyState::OutcomeUnknown)
            })
            .map(|detail| detail.proposal.id)
            .collect()
    }
    pub(super) fn selected_ai_database_outcome(&self) -> Option<uuid::Uuid> {
        self.ai
            .sql_execution_selected
            .checked_sub(self.ai.sql_executions.len() + self.ai.typed_executions.len())
            .and_then(|index| self.ai_database_unknown_ids().get(index).copied())
    }
    pub(super) fn inspect_ai_database_outcome(&mut self, id: uuid::Uuid, cx: &mut Context<Self>) {
        if self.ai.database_pending {
            return;
        }
        if let Some(index) = self
            .ai_database_unknown_ids()
            .iter()
            .position(|item| *item == id)
        {
            self.ai.sql_execution_selected =
                self.ai.sql_executions.len() + self.ai.typed_executions.len() + index;
        } else {
            return;
        }
        self.ai.database_status = None;
        self.ai.database_status_id = Some(id);
        self.send_ai_database_outcome(id, None, cx);
    }
    pub(super) fn acknowledge_ai_database_outcome(&mut self, cx: &mut Context<Self>) {
        let Some(status) = self.ai.database_status.as_ref().filter(|status| {
            status.state == sift_protocol::AiDatabaseApplyState::OutcomeUnknown
                && status.reconciled_at.is_none()
                && self.selected_ai_database_outcome() == Some(status.proposal_id)
                && self.ai.database_status_id == Some(status.proposal_id)
        }) else {
            return;
        };
        let id = status.proposal_id;
        let request = sift_protocol::ReconcileAiDatabaseApply {
            review_id: status.review_id,
            client_request_id: status.client_request_id,
            review_digest: status.review_digest.clone(),
        };
        self.send_ai_database_outcome(id, Some(request), cx);
    }
    fn send_ai_database_outcome(
        &mut self,
        id: uuid::Uuid,
        reconcile: Option<sift_protocol::ReconcileAiDatabaseApply>,
        cx: &mut Context<Self>,
    ) {
        if self.ai.database_pending {
            return;
        }
        let Some(chat) = self.ai.chat.as_ref() else {
            return;
        };
        let Some(sender) = self.executor_sender.as_ref() else {
            return;
        };
        if sender
            .send(ExecutorCommand::AiDatabaseApplyStatus {
                instance_id: self
                    .selected_instance_id
                    .clone()
                    .unwrap_or_else(|| "local".into()),
                chat_id: chat.id,
                proposal_id: id,
                reconcile,
            })
            .is_ok()
        {
            self.ai.database_pending = true;
        }
        cx.notify();
    }
    pub(super) fn render_ai_database_outcome(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut view = div().flex().flex_col().gap_2();
        if !self.ai_database_unknown_ids().is_empty() {
            view = view.child("Database outcomes · Ctrl-E focus · j/k select · Enter inspect · r acknowledge after inspection");
        }
        if let Some(status) = &self.ai.database_status {
            view = view.child(format!("Proposal {} · {:?} · approved by {}", status.proposal_id, status.state, status.approved_by))
                .child("Inspect the database before acknowledging. The outcome stays unknown, and this proposal will never execute again.")
                .child(format!("Reviewed claim {} · {}", status.client_request_id, status.review_digest));
            if let Some(at) = status.reconciled_at {
                view = view.child(format!(
                    "Acknowledged {at}; the original outcome is unchanged"
                ));
            } else if status.state == sift_protocol::AiDatabaseApplyState::OutcomeUnknown {
                view = view.child(
                    Button::new(
                        "ai-acknowledge-database-outcome",
                        "Acknowledge inspected outcome (r)",
                    )
                    .disabled(
                        self.ai.database_pending
                            || self.selected_ai_database_outcome() != Some(status.proposal_id),
                    )
                    .on_click(
                        cx.listener(|shell, _, _, cx| shell.acknowledge_ai_database_outcome(cx)),
                    ),
                );
            }
        }
        view.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    use sift_protocol::*;

    #[gpui::test]
    fn acknowledgement_requires_the_selected_inspected_claim(cx: &mut TestAppContext) {
        let window = crate::shell::tests::shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let (sender, mut commands) = ExecutorSender::channel(8);
        workspace.update(&mut cx, |shell, cx| {
            let now = chrono::Utc::now();
            let chat = uuid::Uuid::new_v4();
            let id = uuid::Uuid::new_v4();
            shell.ai.chat = Some(AiChat {
                id: chat,
                tenant_id: 1,
                room_id: None,
                owner_principal_id: 1,
                visibility: AiVisibility::Private,
                title: "outcomes".into(),
                revision: 1,
                created_at: now,
                updated_at: now,
            });
            shell.executor_sender = Some(sender);
            Arc::make_mut(&mut shell.ai.database_proposals).push(AiDatabaseProposalDetail {
                proposal: AiProposal {
                    id,
                    chat_id: chat,
                    run_id: uuid::Uuid::new_v4(),
                    kind: AiProposalKind::RowEditSet,
                    status: AiProposalStatus::Staged,
                    target: ToolContext {
                        tenant_id: Some(1),
                        room_id: None,
                        profile_id: Some(1),
                        connection_id: None,
                        document_id: None,
                    },
                    base_revision: None,
                    content_sha256: "proof".into(),
                    created_by: 1,
                    applied_by: None,
                    created_at: now,
                    updated_at: now,
                },
                draft: AiDatabaseDraft::RowEditSet {
                    edit_set: EditSet {
                        table: ObjectPath::new("items"),
                        edits: vec![],
                    },
                    expected_catalog_revision: CatalogRevision(1),
                },
                source_digest: "source".into(),
                database_identity: "database".into(),
                publication_id: None,
                apply_state: Some(AiDatabaseApplyState::OutcomeUnknown),
                external_origin: None,
            });
            let status = AiDatabaseApplyStatus {
                proposal_id: id,
                review_id: uuid::Uuid::new_v4(),
                client_request_id: uuid::Uuid::new_v4(),
                review_digest: "exact reviewed claim".into(),
                approved_by: 1,
                state: AiDatabaseApplyState::OutcomeUnknown,
                reconciled_at: None,
                reconciled_by: None,
            };
            shell.ai.database_status = Some(status.clone());
            shell.acknowledge_ai_database_outcome(cx);
            assert!(
                commands.try_recv().is_err(),
                "unbound status cannot authorize acknowledgement"
            );
            shell.ai.database_status_id = Some(id);
            shell.ai.sql_execution_selected = 1;
            shell.acknowledge_ai_database_outcome(cx);
            assert!(
                commands.try_recv().is_err(),
                "selection changes invalidate the inspected target"
            );
            shell.ai.sql_execution_selected = 0;
            shell.acknowledge_ai_database_outcome(cx);
            let command = match commands.try_recv().unwrap() {
                ExecutorCommand::AiScoped { command, .. } => *command,
                command => command,
            };
            match command {
                ExecutorCommand::AiDatabaseApplyStatus {
                    chat_id,
                    proposal_id,
                    reconcile: Some(proof),
                    ..
                } => {
                    assert_eq!((chat_id, proposal_id), (chat, id));
                    assert_eq!(proof.review_id, status.review_id);
                    assert_eq!(proof.client_request_id, status.client_request_id);
                    assert_eq!(proof.review_digest, status.review_digest);
                }
                _ => panic!("expected exact acknowledgement without native apply"),
            }
            shell.reset_ai_database_review(cx);
            assert!(shell.ai.database_status.is_none() && shell.ai.database_status_id.is_none());
        });
    }
}
