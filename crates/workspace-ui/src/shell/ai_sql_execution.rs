//! Human SQL review stays separate from provider output and approval claims.
use super::*;
use sift_protocol::{AiExecutionState, ApproveAiSqlExecution};

impl WorkspaceShell {
    pub(super) fn reset_ai_sql_execution(&mut self) {
        self.ai.query_executions = Arc::new(Vec::new());
        self.ai.query_execution_view = None;
        self.ai.query_execution_request = None;
        self.ai.query_execution_stopping = false;
        self.ai.typed_executions = Arc::new(Vec::new());
        self.ai.typed_execution_view = None;
        self.ai.sql_executions = Arc::new(Vec::new());
        self.ai.sql_execution_review = None;
        self.ai.sql_execution_pending = false;
        self.ai.sql_execution_focus = false;
        self.ai.sql_execution_selected = 0;
    }
    pub(super) fn send_ai_sql_execution(
        &mut self,
        id: uuid::Uuid,
        approval: Option<ApproveAiSqlExecution>,
        reconcile: bool,
        deny: bool,
        cx: &mut Context<Self>,
    ) {
        if self.ai.sql_execution_pending {
            return;
        }
        let Some(chat) = self.ai.chat.as_ref() else {
            return;
        };
        let Some(sender) = &self.executor_sender else {
            return;
        };
        if approval.is_none() {
            self.ai.sql_execution_review = None;
            self.ai
                .production_input
                .update(cx, |input, cx| input.set_text("", cx));
        }
        let command = ExecutorCommand::AiSqlExecution {
            instance_id: self
                .selected_instance_id
                .clone()
                .unwrap_or_else(|| "local".into()),
            chat_id: chat.id,
            id,
            approval,
            reconcile,
            deny,
        };
        if sender.send(command).is_ok() {
            self.ai.sql_execution_pending = true;
        }
        cx.notify();
    }
    pub(super) fn deny_ai_sql_execution(&mut self, cx: &mut Context<Self>) {
        let Some(detail) = self
            .ai
            .sql_execution_review
            .as_ref()
            .filter(|d| d.summary.state == AiExecutionState::AwaitingApproval)
        else {
            return;
        };
        let id = detail.summary.id;
        let request = ApproveAiSqlExecution {
            fingerprint: detail.summary.fingerprint.clone(),
            production_confirmation: None,
        };
        self.send_ai_sql_execution(id, Some(request), false, true, cx);
    }
    pub(super) fn approve_ai_sql_execution(&mut self, reconcile: bool, cx: &mut Context<Self>) {
        let Some(detail) = self.ai.sql_execution_review.as_ref() else {
            return;
        };
        let required = if reconcile {
            AiExecutionState::OutcomeUnknown
        } else {
            AiExecutionState::AwaitingApproval
        };
        if detail.summary.state != required
            || self
                .ai
                .sql_executions
                .get(self.ai.sql_execution_selected)
                .is_none_or(|s| s.id != detail.summary.id)
        {
            return;
        }
        let confirmation = self.ai.production_input.read(cx).text().trim().to_owned();
        if !reconcile
            && detail.input.source.production
            && confirmation != detail.input.source.database_label
        {
            self.ai.error = Some(
                "Type the exact reviewed production database label before approving SQL".into(),
            );
            cx.notify();
            return;
        }
        let id = detail.summary.id;
        let approval = ApproveAiSqlExecution {
            fingerprint: detail.summary.fingerprint.clone(),
            production_confirmation: (!confirmation.is_empty()).then_some(confirmation),
        };
        self.send_ai_sql_execution(id, Some(approval), reconcile, false, cx);
    }
    pub(super) fn render_ai_sql_executions(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let colors = cx.theme().colors;
        let mut view = div().flex().flex_col().gap_2();
        if !self.ai.sql_executions.is_empty() {
            view=view.child("SQL execution · Ctrl-E focus · j/k select · Enter review · a approve · d deny · r acknowledge unknown · Esc leave");
        }
        for (index, item) in self.ai.sql_executions.iter().enumerate() {
            let id = item.id;
            view = view.child(
                Button::new(
                    format!("ai-sql-action-{id}"),
                    format!(
                        "{}SQL {} · {:?}",
                        if self.ai.sql_execution_focus && index == self.ai.sql_execution_selected {
                            "> "
                        } else {
                            ""
                        },
                        id,
                        item.state
                    ),
                )
                .tone(ButtonTone::Ghost)
                .disabled(self.ai.sql_execution_pending)
                .on_click(cx.listener(move |shell, _, _, cx| {
                    shell.ai.sql_execution_selected = index;
                    shell.send_ai_sql_execution(id, None, false, false, cx);
                })),
            )
        }
        if let Some(detail) = &self.ai.sql_execution_review {
            let source = &detail.input.source;
            let mut review=div().border_1().border_color(colors.subtle_border).p_2().flex().flex_col().gap_2()
                .child(format!("Configured source: {} · {} · {:?}",source.database_label,if source.production {"PRODUCTION"}else{"not tagged production"},detail.summary.state))
                .child(format!("Profile {} · session {} · connection {} · generation {}",source.profile_id,source.session.0,source.connection.0,source.generation))
                .child(format!("Exact action {} · expires {}",detail.summary.fingerprint,detail.summary.expires_at))
                .child("Approval executes this exact SQL and binds. SQL may commit data or schema changes; returned rows and native outcome are recorded separately. No affected-row limit is enforced; result limits do not bound writes.")
                .child(div().whitespace_normal().font_family("monospace").child(detail.input.sql.clone()))
                .child(div().whitespace_normal().font_family("monospace").child(serde_json::to_string_pretty(&detail.input.params).unwrap_or_else(|_|"Binds unavailable".into())));
            if let Some(receipt) = &detail.summary.receipt {
                review = review.child(format!(
                    "Native {:?} · affected {:?} · observation {:?}",
                    receipt.native_outcome, receipt.affected_rows, receipt.result_code
                ));
            }
            if detail.summary.state == AiExecutionState::AwaitingApproval {
                if source.production {
                    review = review
                        .child(format!("Type exactly: {}", source.database_label))
                        .child(self.ai.production_input.clone());
                }
                review =
                    review.child(
                        Button::new("ai-sql-approve", "Approve exact SQL (a)")
                            .disabled(self.ai.sql_execution_pending)
                            .on_click(cx.listener(|shell, _, _, cx| {
                                shell.approve_ai_sql_execution(false, cx)
                            })),
                    );
            }
            if detail.summary.state == AiExecutionState::AwaitingApproval {
                review = review.child(
                    Button::new("ai-sql-deny", "Deny SQL (d)")
                        .tone(ButtonTone::Ghost)
                        .disabled(self.ai.sql_execution_pending)
                        .on_click(cx.listener(|shell, _, _, cx| shell.deny_ai_sql_execution(cx))),
                );
            }
            if detail.summary.state == AiExecutionState::OutcomeUnknown {
                review=review.child("Inspect the native database before acknowledging this action. Acknowledgment keeps the outcome unknown and permits later actions; it never retries this SQL.")
                    .child(Button::new("ai-sql-reconcile","Acknowledge unknown outcome (r)").disabled(self.ai.sql_execution_pending).on_click(cx.listener(|shell,_,_,cx|shell.approve_ai_sql_execution(true,cx))));
            }
            let id = detail.summary.id;
            review = review.child(
                Button::new("ai-sql-refresh", "Refresh receipt")
                    .tone(ButtonTone::Ghost)
                    .disabled(self.ai.sql_execution_pending)
                    .on_click(cx.listener(move |shell, _, _, cx| {
                        shell.send_ai_sql_execution(id, None, false, false, cx)
                    })),
            );
            view = view.child(review);
        }
        view.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    #[gpui::test]
    fn execution_review_remains_human_owned_while_provider_waits(cx: &mut TestAppContext) {
        let window = crate::shell::tests::shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let (sender, mut commands) = ExecutorSender::channel(8);
        workspace.update(&mut cx, |shell, cx| {
            let now = chrono::Utc::now();
            let chat = uuid::Uuid::new_v4();
            shell.ai.chat = Some(sift_protocol::AiChat {
                id: chat,
                tenant_id: 1,
                room_id: None,
                owner_principal_id: 1,
                visibility: sift_protocol::AiVisibility::Private,
                title: "Execute".into(),
                revision: 1,
                created_at: now,
                updated_at: now,
            });
            shell.executor_sender = Some(sender);
            shell.ai.pending = true;
            let summary = sift_protocol::AiSqlExecutionSummary {
                id: uuid::Uuid::new_v4(),
                run_id: uuid::Uuid::new_v4(),
                chat_id: chat,
                call_id: uuid::Uuid::new_v4(),
                initiator_id: 1,
                approved_by: None,
                fingerprint: "immutable-action".into(),
                state: AiExecutionState::AwaitingApproval,
                created_at: now,
                expires_at: now + chrono::Duration::minutes(1),
                receipt: None,
            };
            shell.on_executor_event(ExecutorEvent::AiSqlExecutionStatus(summary.clone()), cx);
            assert_eq!(
                shell.ai.sql_executions.len(),
                1,
                "provider waiting must not hide human approval work"
            );
            shell.on_executor_event(
                ExecutorEvent::AiSqlExecutionStatus(sift_protocol::AiSqlExecutionSummary {
                    chat_id: uuid::Uuid::new_v4(),
                    ..summary.clone()
                }),
                cx,
            );
            assert_eq!(
                shell.ai.sql_executions.len(),
                1,
                "another chat cannot supply an approval target"
            );
            shell.ai.sql_execution_review = Some(sift_protocol::AiSqlExecutionDetail {
                summary: summary.clone(),
                input: sift_protocol::AiSqlExecutionInput {
                    sql: "UPDATE accounts SET enabled=1".into(),
                    params: Vec::new(),
                    source: sift_protocol::AiExecutionSource {
                        tenant_id: 1,
                        profile_id: 2,
                        session: sift_protocol::SessionId(3),
                        connection: sift_protocol::ConnectionId(4),
                        generation: uuid::Uuid::new_v4(),
                        profile_digest: "profile-proof".into(),
                        database_label: "Production / accounts".into(),
                        production: true,
                        engine: sift_protocol::Engine::Postgres,
                    },
                },
            });
            shell.approve_ai_sql_execution(false, cx);
            assert!(
                commands.try_recv().is_err(),
                "provider output cannot supply production confirmation"
            );
            shell
                .ai
                .production_input
                .update(cx, |input, cx| input.set_text("Production / accounts", cx));
            shell.approve_ai_sql_execution(false, cx);
            let command = commands.try_recv().unwrap();
            let command = match command {
                ExecutorCommand::AiScoped { command, .. } => *command,
                command => command,
            };
            match command {
                ExecutorCommand::AiSqlExecution {
                    chat_id,
                    id,
                    approval: Some(request),
                    reconcile,
                    deny,
                    ..
                } => {
                    assert_eq!((chat_id, id), (chat, summary.id));
                    assert_eq!(request.fingerprint, summary.fingerprint);
                    assert_eq!(
                        request.production_confirmation.as_deref(),
                        Some("Production / accounts")
                    );
                    assert!(!reconcile && !deny);
                }
                _ => panic!("expected the exact human SQL approval"),
            }
            shell.reset_ai_sql_execution();
            assert!(shell.ai.sql_execution_review.is_none() && shell.ai.sql_executions.is_empty());
        });
    }
}
