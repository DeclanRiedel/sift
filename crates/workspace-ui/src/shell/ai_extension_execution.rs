//! Human extension review is bound to the original native source and caller.
use super::*;
use sift_protocol::{AiExecutionState, AiExtensionExecutionDetail, ApproveAiExtensionExecution};

impl WorkspaceShell {
    pub(super) fn ai_extension_source_current(&self, detail: &AiExtensionExecutionDetail) -> bool {
        let source = &detail.review.source;
        self.active_query_connection == Some((source.profile_id, source.session, source.connection))
    }
    pub(super) fn selected_ai_extension_execution(&self) -> Option<uuid::Uuid> {
        self.ai
            .sql_execution_selected
            .checked_sub(
                self.ai.sql_executions.len()
                    + self.ai.typed_executions.len()
                    + self.ai.query_executions.len()
                    + self.ai_database_unknown_ids().len(),
            )
            .and_then(|index| self.ai.extension_executions.get(index))
            .map(|item| item.id)
    }
    pub(super) fn send_ai_extension_execution(
        &mut self,
        id: uuid::Uuid,
        approval: Option<ApproveAiExtensionExecution>,
        deny: bool,
        acknowledge: Option<sift_protocol::AcknowledgeAiExtensionExecution>,
        cx: &mut Context<Self>,
    ) {
        if self.ai.extension_execution_request.is_some() {
            return;
        }
        let Some(chat_id) = self.ai.chat.as_ref().map(|chat| chat.id) else {
            return;
        };
        if approval.is_none() && acknowledge.is_none() {
            self.ai.extension_execution_status = None;
            self.ai.extension_execution_review = None;
            self.ai.extension_execution_caller = None;
            self.ai
                .production_input
                .update(cx, |input, cx| input.set_text("", cx));
        }
        self.bind_extension_caller_scope();
        let Some(sender) = &self.executor_sender else {
            return;
        };
        let request_id = uuid::Uuid::new_v4();
        if sender
            .send(ExecutorCommand::AiExtensionExecution {
                chat_id,
                id,
                request_id,
                approval,
                deny,
                acknowledge,
            })
            .is_ok()
        {
            self.ai.extension_execution_request = Some((id, request_id));
        }
        cx.notify();
    }
    pub(super) fn decide_ai_extension_execution(
        &mut self,
        deny: bool,
        acknowledge: bool,
        cx: &mut Context<Self>,
    ) {
        if acknowledge {
            let Some(summary) = self.ai.extension_execution_status.as_ref() else {
                return;
            };
            if summary.state != AiExecutionState::OutcomeUnknown
                || self.selected_ai_extension_execution() != Some(summary.id)
                || !self.ai.extension_executions.iter().any(|item| {
                    item.id == summary.id && item.state == AiExecutionState::OutcomeUnknown
                })
                || self.ai.extension_execution_caller.is_none()
                || self.ai.extension_execution_caller != self.extension_caller_scope()
            {
                return;
            }
            let Some(execution_id) = summary.execution_id else {
                return;
            };
            let id = summary.id;
            let request = sift_protocol::AcknowledgeAiExtensionExecution {
                fingerprint: summary.fingerprint.clone(),
                execution_id,
            };
            self.send_ai_extension_execution(id, None, false, Some(request), cx);
            return;
        }
        let Some(detail) = self.ai.extension_execution_review.as_ref() else {
            return;
        };
        let required = AiExecutionState::AwaitingApproval;
        if self.selected_ai_extension_execution() != Some(detail.summary.id)
            || detail.summary.state != required
            || !self
                .ai
                .extension_executions
                .iter()
                .any(|item| item.id == detail.summary.id && item.state == required)
            || !self.ai_extension_source_current(detail)
            || self.ai.extension_execution_caller.is_none()
            || self.ai.extension_execution_caller != self.extension_caller_scope()
        {
            self.ai.error = Some("Extension caller, source or review changed; review again".into());
            cx.notify();
            return;
        }
        let confirmation = self.ai.production_input.read(cx).text().trim().to_owned();
        if !deny
            && detail.review.source.production
            && confirmation != detail.review.source.database_label
        {
            self.ai.error =
                Some("Type the exact reviewed production database label before approving".into());
            cx.notify();
            return;
        }
        let id = detail.summary.id;
        let approval = ApproveAiExtensionExecution {
            fingerprint: detail.summary.fingerprint.clone(),
            acknowledged_classification: detail.review.tool.operation.classification,
            production_confirmation: (!confirmation.is_empty()).then_some(confirmation),
        };
        self.send_ai_extension_execution(id, Some(approval), deny, None, cx);
    }
    pub(super) fn render_ai_extension_executions(
        &self,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let mut view = div().flex().flex_col().gap_2();
        if !self.ai.extension_executions.is_empty() {
            view=view.child("Extension execution · Ctrl-E focus · j/k select · Enter review · a approve · d deny · r acknowledge unknown");
        }
        for (index, item) in self.ai.extension_executions.iter().enumerate() {
            let id = item.id;
            let offset = self.ai.sql_executions.len()
                + self.ai.typed_executions.len()
                + self.ai.query_executions.len()
                + self.ai_database_unknown_ids().len()
                + index;
            view = view.child(
                Button::new(
                    format!("ai-extension-{id}"),
                    format!(
                        "{}{} · {:?}",
                        if self.ai.sql_execution_focus && self.ai.sql_execution_selected == offset {
                            "> "
                        } else {
                            ""
                        },
                        item.tool_id,
                        item.state
                    ),
                )
                .tone(ButtonTone::Ghost)
                .disabled(self.ai.extension_execution_request.is_some())
                .on_click(cx.listener(move |shell, _, _, cx| {
                    shell.ai.sql_execution_selected = offset;
                    shell.send_ai_extension_execution(id, None, false, None, cx);
                })),
            );
        }
        if let Some(summary) = self.ai.extension_execution_status.as_ref().filter(|_| {
            self.ai.extension_execution_caller.is_some()
                && self.ai.extension_execution_caller == self.extension_caller_scope()
        }) {
            view = view.child(format!(
                "Request {} · {:?} · RPC {:?}",
                summary.id,
                summary.state,
                summary
                    .receipt
                    .as_ref()
                    .map(|receipt| (receipt.state, receipt.result_code))
            ));
            if summary.state == AiExecutionState::OutcomeUnknown
                && self
                    .ai
                    .extension_execution_review
                    .as_ref()
                    .is_none_or(|detail| !self.ai_extension_source_current(detail))
            {
                view=view.child("The original connection or private review is unavailable. Inspect the native outcome before acknowledging this exact request. Acknowledgement never retries work.")
                    .child(Button::new("ai-extension-ack-redacted","Acknowledge exact unknown outcome (r)").on_click(cx.listener(|shell,_,_,cx|shell.decide_ai_extension_execution(false,true,cx))));
            }
        }
        if let Some(detail) = self
            .ai
            .extension_execution_review
            .as_ref()
            .filter(|detail| {
                self.ai_extension_source_current(detail)
                    && self.ai.extension_execution_caller == self.extension_caller_scope()
            })
        {
            let source = &detail.review.source;
            let mut review=div().p_2().border_1().border_color(cx.theme().colors.subtle_border).flex().flex_col().gap_2()
                .child(format!("{} · {:?} · {}{}",detail.review.tool.title,detail.review.tool.operation.classification,source.database_label,if source.production {" · PRODUCTION"} else {""}))
                .child(format!("Profile {} · session {} · connection {} · generation {}",source.profile_id,source.session.0,source.connection.0,source.generation))
                .child(format!("Exact review {} · expires {}",detail.summary.fingerprint,detail.summary.expires_at))
                .child("Approval invokes this exact extension action. Its declared classification does not prove purity. An RPC reply does not prove database commit or rollback.")
                .child(div().whitespace_normal().font_family("monospace").child(serde_json::to_string_pretty(&detail.review.arguments).unwrap_or_else(|_|"Arguments unavailable".into())))
                .child(div().whitespace_normal().font_family("monospace").child(serde_json::to_string_pretty(&detail.review.prepared_contract).unwrap_or_else(|_|"Contract unavailable".into())))
                .child(format!("RPC {:?} · private result {:?}",detail.summary.receipt.as_ref().map(|receipt|(receipt.state,receipt.result_code)),detail.summary.result_availability));
            if detail.summary.state == AiExecutionState::AwaitingApproval {
                if source.production {
                    review = review
                        .child(format!("Type exactly: {}", source.database_label))
                        .child(self.ai.production_input.clone());
                }
                review = review
                    .child(
                        Button::new(
                            "ai-extension-approve",
                            "Approve reviewed classification and action (a)",
                        )
                        .on_click(cx.listener(|shell, _, _, cx| {
                            shell.decide_ai_extension_execution(false, false, cx)
                        })),
                    )
                    .child(
                        Button::new("ai-extension-deny", "Deny action (d)")
                            .tone(ButtonTone::Ghost)
                            .on_click(cx.listener(|shell, _, _, cx| {
                                shell.decide_ai_extension_execution(true, false, cx)
                            })),
                    );
            }
            if detail.summary.state == AiExecutionState::OutcomeUnknown {
                review=review.child("Inspect the extension and native database before acknowledging. This releases the shared gate without retrying or changing the RPC evidence.")
                    .child(Button::new("ai-extension-ack","Acknowledge exact unknown outcome (r)").on_click(cx.listener(|shell,_,_,cx|shell.decide_ai_extension_execution(false,true,cx))));
            }
            let id = detail.summary.id;
            review = review.child(
                Button::new("ai-extension-refresh", "Refresh review and receipt")
                    .tone(ButtonTone::Ghost)
                    .on_click(cx.listener(move |shell, _, _, cx| {
                        shell.send_ai_extension_execution(id, None, false, None, cx)
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
    use sift_protocol::*;
    fn detail(chat: uuid::Uuid) -> AiExtensionExecutionDetail {
        let now = chrono::Utc::now();
        let operation = ExtensionOperation {
            extension_id: ExtensionId::new("acme/fixture").unwrap(),
            contribution_id: ContributionId::new("acme/fixture/command/run").unwrap(),
            action: SegmentId::new("run").unwrap(),
            classification: OperationClassification::Administrative,
            target_kind: SegmentId::new("tenant").unwrap(),
            target_id: Some("1".into()),
            sanitized_arguments: Default::default(),
        };
        AiExtensionExecutionDetail {
            summary: AiExtensionExecutionSummary {
                id: uuid::Uuid::new_v4(),
                run_id: uuid::Uuid::new_v4(),
                chat_id: chat,
                call_id: uuid::Uuid::new_v4(),
                initiator_id: 1,
                approved_by: None,
                tool_id: "fixture".into(),
                fingerprint: "frozen-review".into(),
                state: AiExecutionState::AwaitingApproval,
                created_at: now,
                expires_at: now + chrono::Duration::minutes(1),
                execution_id: None,
                receipt: None,
                result_availability: AiExtensionResultAvailability::NotRecorded,
                acknowledged_at: None,
            },
            review: AiExtensionExecutionReview {
                tool: GovernedToolDescriptor {
                    id: "fixture".into(),
                    title: "Admin fixture".into(),
                    description: "Reviewed admin".into(),
                    operation,
                    input_schema: serde_json::json!({"type":"object"}),
                    output_schema: serde_json::json!({"type":"object"}),
                    required_context: vec![ContributionContext::Tenant],
                    mcp_exposable: true,
                    schedulable: false,
                    interactive: false,
                },
                context: ToolContext {
                    tenant_id: Some(1),
                    room_id: None,
                    profile_id: Some(2),
                    connection_id: Some("3:4".into()),
                    document_id: None,
                },
                arguments: serde_json::json!({"review":"private"}),
                prepared_contract: serde_json::json!({"contract":"frozen"}),
                source: sift_protocol::AiExecutionSource {
                    tenant_id: 1,
                    profile_id: 2,
                    session: SessionId(3),
                    connection: ConnectionId(4),
                    generation: uuid::Uuid::new_v4(),
                    profile_digest: "source-proof".into(),
                    database_label: "Production / fixture".into(),
                    production: true,
                    engine: Engine::Postgres,
                },
            },
            result: None,
        }
    }
    fn unwrap_scopes(command: ExecutorCommand) -> ExecutorCommand {
        match command {
            ExecutorCommand::AiScoped { command, .. } => unwrap_scopes(*command),
            ExecutorCommand::ExtensionScoped { scope, command } => {
                assert_eq!(scope.identity.principal_id, 1);
                assert_eq!(scope.identity.daemon_generation, "original-generation");
                *command
            }
            _ => panic!("extension caller scope required"),
        }
    }
    #[gpui::test]
    fn vim_extension_review_pins_caller_source_and_human_production_confirmation(
        cx: &mut TestAppContext,
    ) {
        let window = crate::shell::tests::shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let (sender, mut commands) = ExecutorSender::channel(8);
        let chat = uuid::Uuid::new_v4();
        let reviewed = detail(chat);
        workspace.update_in(&mut cx, |shell, window, cx| {
            super::super::extension_caller::tests::authenticate(shell);
            shell.ai_dock_active = true;
            shell.executor_sender = Some(sender.with_ai_scope(shell.current_ai_view_scope()));
            shell.active_query_connection = Some((2, SessionId(3), ConnectionId(4)));
            let now = chrono::Utc::now();
            shell.ai.chat = Some(AiChat {
                id: chat,
                tenant_id: 1,
                room_id: None,
                owner_principal_id: 1,
                visibility: AiVisibility::Private,
                title: "Review".into(),
                revision: 1,
                created_at: now,
                updated_at: now,
            });
            shell.ai.pending = true;
            shell.on_executor_event(
                ExecutorEvent::AiExtensionExecutionStatus(reviewed.summary.clone()),
                cx,
            );
            shell.ai.transcript_focus.focus(window, cx);
            cx.notify();
        });
        cx.simulate_keystrokes("ctrl-e enter");
        let ExecutorCommand::AiExtensionExecution {
            id,
            request_id,
            approval: None,
            ..
        } = unwrap_scopes(commands.try_recv().unwrap())
        else {
            panic!("exact review request required")
        };
        assert_eq!(id, reviewed.summary.id);
        workspace.update(&mut cx, |shell, cx| {
            shell.on_executor_event(
                ExecutorEvent::ExtensionScoped {
                    scope: shell.extension_caller_scope().unwrap(),
                    event: Box::new(ExecutorEvent::AiExtensionExecution {
                        chat_id: chat,
                        id,
                        request_id,
                        result: Ok(Box::new(AiExtensionExecutionView {
                            summary: reviewed.summary.clone(),
                            detail: Some(reviewed.clone()),
                        })),
                    }),
                },
                cx,
            );
        });
        cx.simulate_keystrokes("a");
        assert!(
            commands.try_recv().is_err(),
            "provider waiting cannot supply production confirmation"
        );
        workspace.update(&mut cx, |shell, cx| {
            shell
                .ai
                .production_input
                .update(cx, |input, cx| input.set_text("Production / fixture", cx));
            shell.active_query_connection = Some((2, SessionId(3), ConnectionId(5)));
        });
        cx.simulate_keystrokes("a");
        assert!(
            commands.try_recv().is_err(),
            "changed native source refuses queued approval"
        );
        workspace.update(&mut cx, |shell, _| {
            shell.active_query_connection = Some((2, SessionId(3), ConnectionId(4)));
            shell.lifecycle.identity = None;
        });
        cx.simulate_keystrokes("a");
        assert!(
            commands.try_recv().is_err(),
            "changed account refuses reviewed authority"
        );
        workspace.update(&mut cx, |shell, _| {
            super::super::extension_caller::tests::authenticate(shell)
        });
        cx.simulate_keystrokes("a");
        let ExecutorCommand::AiExtensionExecution {
            id,
            approval: Some(request),
            deny: false,
            acknowledge: None,
            ..
        } = unwrap_scopes(commands.try_recv().unwrap())
        else {
            panic!("exact classified approval required")
        };
        assert_eq!(id, reviewed.summary.id);
        assert_eq!(request.fingerprint, reviewed.summary.fingerprint);
        assert_eq!(
            request.acknowledged_classification,
            OperationClassification::Administrative
        );
        assert_eq!(
            request.production_confirmation.as_deref(),
            Some("Production / fixture")
        );
    }
    #[gpui::test]
    fn vim_extension_unknown_acknowledgement_survives_original_connection_loss(
        cx: &mut TestAppContext,
    ) {
        let window = crate::shell::tests::shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let (sender, mut commands) = ExecutorSender::channel(8);
        let chat = uuid::Uuid::new_v4();
        let mut summary = detail(chat).summary;
        summary.state = AiExecutionState::OutcomeUnknown;
        summary.execution_id = Some(uuid::Uuid::new_v4());
        workspace.update_in(&mut cx, |shell, window, cx| {
            super::super::extension_caller::tests::authenticate(shell);
            shell.ai_dock_active = true;
            shell.executor_sender = Some(sender.with_ai_scope(shell.current_ai_view_scope()));
            let now = chrono::Utc::now();
            shell.ai.chat = Some(AiChat {
                id: chat,
                tenant_id: 1,
                room_id: None,
                owner_principal_id: 1,
                visibility: AiVisibility::Private,
                title: "Unknown extension".into(),
                revision: 1,
                created_at: now,
                updated_at: now,
            });
            shell.active_query_connection = None;
            shell.on_executor_event(
                ExecutorEvent::AiExtensionExecutionStatus(summary.clone()),
                cx,
            );
            shell.ai.transcript_focus.focus(window, cx);
            cx.notify();
        });
        cx.simulate_keystrokes("ctrl-e enter");
        let ExecutorCommand::AiExtensionExecution {
            id,
            request_id,
            approval: None,
            ..
        } = unwrap_scopes(commands.try_recv().unwrap())
        else {
            panic!("redacted status inspection required")
        };
        workspace.update(&mut cx, |shell, cx| {
            shell.on_executor_event(
                ExecutorEvent::ExtensionScoped {
                    scope: shell.extension_caller_scope().unwrap(),
                    event: Box::new(ExecutorEvent::AiExtensionExecution {
                        chat_id: chat,
                        id,
                        request_id,
                        result: Ok(Box::new(AiExtensionExecutionView {
                            summary: summary.clone(),
                            detail: None,
                        })),
                    }),
                },
                cx,
            );
            assert!(shell.ai.extension_execution_review.is_none());
            assert!(shell.active_query_connection.is_none());
        });
        cx.simulate_keystrokes("a");
        assert!(
            commands.try_recv().is_err(),
            "redacted receipt is never approval authority"
        );
        cx.simulate_keystrokes("r");
        let ExecutorCommand::AiExtensionExecution {
            id,
            approval: None,
            acknowledge: Some(request),
            deny: false,
            ..
        } = unwrap_scopes(commands.try_recv().unwrap())
        else {
            panic!("exact unknown acknowledgement required")
        };
        assert_eq!(id, summary.id);
        assert_eq!(request.fingerprint, summary.fingerprint);
        assert_eq!(Some(request.execution_id), summary.execution_id);
    }
}
