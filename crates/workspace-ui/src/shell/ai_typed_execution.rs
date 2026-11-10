//! Human controls for immutable typed requests; provider status is not approval.
use super::*;
use sift_protocol::{AiExecutionState, ApproveAiDatabaseExecution};
impl WorkspaceShell {
    pub(super) fn selected_ai_typed_execution(&self) -> Option<uuid::Uuid> {
        self.ai
            .sql_execution_selected
            .checked_sub(self.ai.sql_executions.len())
            .and_then(|i| self.ai.typed_executions.get(i))
            .map(|s| s.id)
    }
    pub(super) fn send_ai_typed_execution(
        &mut self,
        id: uuid::Uuid,
        approval: Option<ApproveAiDatabaseExecution>,
        deny: bool,
        reconcile: bool,
        cx: &mut Context<Self>,
    ) {
        if self.ai.database_pending {
            return;
        }
        let Some(chat) = self.ai.chat.as_ref() else {
            return;
        };
        let chat_id = chat.id;
        let Some(index) = self.ai.typed_executions.iter().position(|s| s.id == id) else {
            return;
        };
        self.ai.sql_execution_selected = self.ai.sql_executions.len() + index;
        if approval.is_none() {
            self.reset_ai_database_review(cx);
        }
        let Some(sender) = &self.executor_sender else {
            return;
        };
        let request_id = uuid::Uuid::new_v4();
        if sender
            .send(ExecutorCommand::AiDatabaseExecution {
                instance_id: self
                    .selected_instance_id
                    .clone()
                    .unwrap_or_else(|| "local".into()),
                chat_id,
                id,
                request_id,
                approval,
                deny,
                reconcile,
            })
            .is_ok()
        {
            self.ai.database_pending = true;
            self.ai.typed_execution_request = Some((id, request_id));
        }
        cx.notify();
    }
    pub(super) fn decide_ai_typed_execution(
        &mut self,
        deny: bool,
        reconcile: bool,
        cx: &mut Context<Self>,
    ) {
        if self.ai.database_pending {
            return;
        }
        let Some(view) = &self.ai.typed_execution_view else {
            return;
        };
        if self.selected_ai_typed_execution() != Some(view.summary.id) {
            return;
        }
        if reconcile && view.summary.state != AiExecutionState::OutcomeUnknown
            || !reconcile && view.summary.state != AiExecutionState::AwaitingApproval
        {
            return;
        }
        let confirmation = self.ai.production_input.read(cx).text().trim().to_owned();
        if !deny && !reconcile {
            let Some(detail) = &view.detail else {
                return;
            };
            if detail.review.expires_at <= chrono::Utc::now() {
                self.ai.error = Some("Typed execution preview expired".into());
                cx.notify();
                return;
            }
            if detail.review.production && confirmation != detail.review.database_label {
                self.ai.error = Some("Type the exact reviewed production database label".into());
                cx.notify();
                return;
            }
            if let sift_protocol::AiDatabasePreview::MigrationDraft { plan } =
                &detail.review.preview
            {
                if plan
                    .required_acknowledgements
                    .iter()
                    .any(|risk| !self.ai.database_acknowledgements.contains(risk))
                {
                    self.ai.error = Some("Acknowledge each typed migration risk".into());
                    cx.notify();
                    return;
                }
            }
        }
        let id = view.summary.id;
        let request = ApproveAiDatabaseExecution {
            fingerprint: view.summary.fingerprint.clone(),
            production_confirmation: (!confirmation.is_empty()).then_some(confirmation),
            acknowledgements: self.ai.database_acknowledgements.iter().copied().collect(),
        };
        self.send_ai_typed_execution(id, Some(request), deny, reconcile, cx);
    }
    pub(super) fn typed_risk_key(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(view) = &self.ai.typed_execution_view else {
            return;
        };
        if self.ai.database_pending
            || view.summary.state != AiExecutionState::AwaitingApproval
            || self.selected_ai_typed_execution() != Some(view.summary.id)
        {
            return;
        }
        let Some(detail) = &view.detail else {
            return;
        };
        let sift_protocol::AiDatabasePreview::MigrationDraft { plan } = &detail.review.preview
        else {
            return;
        };
        match key {
            "[" => self.ai.typed_risk_selected = self.ai.typed_risk_selected.saturating_sub(1),
            "]" => {
                self.ai.typed_risk_selected = (self.ai.typed_risk_selected + 1)
                    .min(plan.required_acknowledgements.len().saturating_sub(1))
            }
            "x" => {
                if let Some(risk) = plan
                    .required_acknowledgements
                    .get(self.ai.typed_risk_selected)
                {
                    if !self.ai.database_acknowledgements.remove(risk) {
                        self.ai.database_acknowledgements.insert(*risk);
                    }
                }
            }
            _ => {}
        }
        cx.notify();
    }
    pub(super) fn render_ai_typed_executions(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut view = div().flex().flex_col().gap_2();
        if !self.ai.typed_executions.is_empty() {
            view=view.child("Typed execution · Ctrl-E focus · j/k select · Enter inspect · a approve · d deny · r acknowledge unknown after inspection · [/] risks · x toggle");
        }
        for summary in self.ai.typed_executions.iter() {
            let id = summary.id;
            view = view.child(
                Button::new(
                    format!("ai-typed-{id}"),
                    format!(
                        "{}Typed {} · {:?}",
                        if self.ai.sql_execution_focus
                            && self.selected_ai_typed_execution() == Some(id)
                        {
                            "> "
                        } else {
                            ""
                        },
                        id,
                        summary.state
                    ),
                )
                .tone(ButtonTone::Ghost)
                .disabled(self.ai.database_pending)
                .on_click(cx.listener(move |shell, _, _, cx| {
                    shell.send_ai_typed_execution(id, None, false, false, cx)
                })),
            );
        }
        if let Some(current) = &self.ai.typed_execution_view {
            view = view.child(format!(
                "Exact typed claim {} · {:?} · expires {}",
                current.summary.fingerprint, current.summary.state, current.summary.expires_at
            ));
            if let Some(receipt) = &current.summary.receipt {
                view = view.child(format!("Native {:?} · {}", receipt.state, receipt.message));
            }
            if current.detail.is_none() {
                view=view.child("The original source is unavailable. Native outcome remains recorded; inspect the database independently before acknowledging uncertainty.");
            }
            let id = current.summary.id;
            if current.summary.state == AiExecutionState::AwaitingApproval {
                view = view.child(
                    Button::new("ai-typed-deny", "Deny typed request (d)")
                        .disabled(self.ai.database_pending)
                        .on_click(cx.listener(|shell, _, _, cx| {
                            shell.decide_ai_typed_execution(true, false, cx)
                        })),
                );
            }
            if current.summary.state == AiExecutionState::OutcomeUnknown {
                view = view.child(
                    Button::new(
                        "ai-typed-reconcile",
                        "Acknowledge inspected unknown outcome (r)",
                    )
                    .disabled(
                        self.ai.database_pending || self.selected_ai_typed_execution() != Some(id),
                    )
                    .on_click(cx.listener(|shell, _, _, cx| {
                        shell.decide_ai_typed_execution(false, true, cx)
                    })),
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
    fn typed_confirmation_and_reply_bind_to_the_selected_human_request(cx: &mut TestAppContext) {
        let window = crate::shell::tests::shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let (sender, mut commands) = ExecutorSender::channel(8);
        workspace.update(&mut cx, |shell, cx| {
            let now = chrono::Utc::now();
            let chat = uuid::Uuid::new_v4();
            let id = uuid::Uuid::new_v4();
            let proposal = uuid::Uuid::new_v4();
            let review = uuid::Uuid::new_v4();
            shell.ai.chat = Some(AiChat {
                id: chat,
                tenant_id: 1,
                room_id: None,
                owner_principal_id: 1,
                visibility: AiVisibility::Private,
                title: "typed review".into(),
                revision: 1,
                created_at: now,
                updated_at: now,
            });
            shell.executor_sender = Some(sender);
            let summary = AiDatabaseExecutionSummary {
                id,
                run_id: uuid::Uuid::new_v4(),
                chat_id: chat,
                proposal_id: proposal,
                review_id: review,
                call_id: uuid::Uuid::new_v4(),
                initiator_id: 1,
                fingerprint: "exact typed fingerprint".into(),
                state: AiExecutionState::AwaitingApproval,
                created_at: now,
                expires_at: now + chrono::Duration::minutes(5),
                receipt: None,
            };
            let preview = AiDatabaseProposalReview {
                id: review,
                proposal_id: proposal,
                reviewer_id: 1,
                session: SessionId(1),
                connection: ConnectionId(1),
                source_digest: "source".into(),
                content_sha256: "content".into(),
                review_digest: "review".into(),
                database_label: "Production / typed".into(),
                production: true,
                preview: AiDatabasePreview::RowEditSet {
                    plan: EditPlan {
                        table: ObjectPath::new("items"),
                        identity: IdentitySource::PrimaryKey {
                            columns: vec!["id".into()],
                        },
                        statements: vec![],
                    },
                },
                expires_at: summary.expires_at,
            };
            let detail = AiDatabaseExecutionDetail {
                summary: summary.clone(),
                source: sift_protocol::AiExecutionSource {
                    tenant_id: 1,
                    profile_id: 1,
                    session: SessionId(1),
                    connection: ConnectionId(1),
                    generation: uuid::Uuid::new_v4(),
                    profile_digest: "source".into(),
                    database_label: preview.database_label.clone(),
                    production: true,
                    engine: Engine::Postgres,
                },
                proposal: AiDatabaseProposalDetail {
                    proposal: AiProposal {
                        id: proposal,
                        chat_id: chat,
                        run_id: summary.run_id,
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
                        content_sha256: "content".into(),
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
                    database_identity: "db".into(),
                    publication_id: None,
                    apply_state: None,
                    external_origin: None,
                },
                review: preview.clone(),
            };
            shell.ai.typed_executions = Arc::new(vec![summary.clone()]);
            shell.ai.typed_execution_view = Some(AiDatabaseExecutionView {
                summary: summary.clone(),
                detail: Some(detail.clone()),
            });
            shell.ai.database_review = Some(preview.clone());
            shell.decide_ai_typed_execution(false, false, cx);
            assert!(
                commands.try_recv().is_err(),
                "provider output cannot supply production confirmation"
            );
            shell
                .ai
                .production_input
                .update(cx, |input, cx| input.set_text(preview.database_label, cx));
            shell.ai.sql_execution_selected = 1;
            shell.decide_ai_typed_execution(false, false, cx);
            assert!(
                commands.try_recv().is_err(),
                "a changed selection invalidates the review action"
            );
            shell.ai.sql_execution_selected = 0;
            shell.decide_ai_typed_execution(false, false, cx);
            let command = match commands.try_recv().unwrap() {
                ExecutorCommand::AiScoped { command, .. } => *command,
                c => c,
            };
            let (instance_id, request_id) = match command {
                ExecutorCommand::AiDatabaseExecution {
                    instance_id,
                    chat_id,
                    id: sent,
                    request_id,
                    approval: Some(request),
                    deny,
                    reconcile,
                } => {
                    assert_eq!((chat_id, sent), (chat, id));
                    assert_eq!(request.fingerprint, summary.fingerprint);
                    assert_eq!(
                        request.production_confirmation.as_deref(),
                        Some("Production / typed")
                    );
                    assert!(!deny && !reconcile);
                    (instance_id, request_id)
                }
                _ => panic!("expected exact typed human approval"),
            };
            shell.on_executor_event(
                ExecutorEvent::AiDatabaseExecution {
                    instance_id: instance_id.clone(),
                    chat_id: chat,
                    id,
                    request_id: uuid::Uuid::new_v4(),
                    result: Ok(Box::new(AiDatabaseExecutionView {
                        summary: summary.clone(),
                        detail: None,
                    })),
                },
                cx,
            );
            assert!(
                shell.ai.database_pending,
                "a stale reply cannot release another request"
            );
            shell.ai.sql_execution_selected = 1;
            shell.on_executor_event(
                ExecutorEvent::AiDatabaseExecution {
                    instance_id,
                    chat_id: chat,
                    id,
                    request_id,
                    result: Ok(Box::new(AiDatabaseExecutionView {
                        summary: AiDatabaseExecutionSummary {
                            state: AiExecutionState::Completed,
                            ..summary
                        },
                        detail: None,
                    })),
                },
                cx,
            );
            assert!(!shell.ai.database_pending && shell.ai.typed_execution_request.is_none());
            assert_eq!(
                shell.ai.database_review.as_ref().unwrap().id,
                review,
                "late settlement cannot replace another selected review"
            );
        });
    }
}
