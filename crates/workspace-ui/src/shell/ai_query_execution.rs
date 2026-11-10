//! Exact query review and a one-use reply lane for the original desktop owner.
use super::*;
use sift_protocol::{AiQueryEditOutcome, AiQueryExecutionDetail, AiQueryExecutionSummary};
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Debug, Clone)]
pub struct AiQueryExecutionView {
    pub summary: AiQueryExecutionSummary,
    pub detail: Option<AiQueryExecutionDetail>,
    pub run: Option<sift_protocol::AiRunDetail>,
}
#[derive(Debug)]
pub struct AiQueryEditorObservation {
    pub outcome: AiQueryEditOutcome,
    pub before_sha256: Option<String>,
    pub after_sha256: Option<String>,
}
impl AiQueryEditorObservation {
    fn unobserved(admitted: bool) -> Self {
        Self {
            outcome: if admitted {
                AiQueryEditOutcome::OutcomeUnknown
            } else {
                AiQueryEditOutcome::Refused
            },
            before_sha256: None,
            after_sha256: None,
        }
    }
}
pub struct AiQueryEditorWork {
    pub instance_id: String,
    pub chat_id: uuid::Uuid,
    pub request_id: uuid::Uuid,
    pub desktop_id: uuid::Uuid,
    pub detail: AiQueryExecutionDetail,
    admission: Arc<AtomicU8>,
    reply: tokio::sync::oneshot::Sender<AiQueryEditorObservation>,
}
impl std::fmt::Debug for AiQueryEditorWork {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AiQueryEditorWork")
            .field("id", &self.detail.summary.id)
            .finish_non_exhaustive()
    }
}
pub struct AiQueryEditorWait {
    admission: Arc<AtomicU8>,
    reply: tokio::sync::oneshot::Receiver<AiQueryEditorObservation>,
}
impl AiQueryEditorWork {
    pub fn channel(
        instance_id: String,
        chat_id: uuid::Uuid,
        request_id: uuid::Uuid,
        desktop_id: uuid::Uuid,
        detail: AiQueryExecutionDetail,
    ) -> (Self, AiQueryEditorWait) {
        let (send, receive) = tokio::sync::oneshot::channel();
        let admission = Arc::new(AtomicU8::new(0));
        (
            Self {
                instance_id,
                chat_id,
                request_id,
                desktop_id,
                detail,
                admission: admission.clone(),
                reply: send,
            },
            AiQueryEditorWait {
                admission,
                reply: receive,
            },
        )
    }
    fn begin(&self) -> bool {
        self.admission
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
    fn complete(self, observation: AiQueryEditorObservation) {
        let _ = self.reply.send(observation);
    }
}
impl AiQueryEditorWait {
    /// Expiry closes queued admission. An admitted edit retains its original
    /// owner until the synchronous UI edit/reply boundary actually settles.
    pub async fn observe(
        mut self,
        expires: chrono::DateTime<chrono::Utc>,
    ) -> AiQueryEditorObservation {
        let duration = (expires - chrono::Utc::now()).to_std().unwrap_or_default();
        tokio::select! {
            result=&mut self.reply=>result.unwrap_or_else(|_|AiQueryEditorObservation::unobserved(self.admission.load(Ordering::Acquire)==1)),
            _=tokio::time::sleep(duration)=> {
                if self.admission.compare_exchange(0,2,Ordering::AcqRel,Ordering::Acquire).is_ok() {
                    AiQueryEditorObservation::unobserved(false)
                } else {
                    self.reply.await.unwrap_or_else(|_|AiQueryEditorObservation::unobserved(true))
                }
            }
        }
    }
}

impl WorkspaceShell {
    pub(super) fn selected_ai_query_execution(&self) -> Option<uuid::Uuid> {
        self.ai
            .sql_execution_selected
            .checked_sub(self.ai.sql_executions.len() + self.ai.typed_executions.len())
            .and_then(|i| self.ai.query_executions.get(i))
            .map(|s| s.id)
    }
    pub(super) fn send_ai_query_execution(
        &mut self,
        id: uuid::Uuid,
        approval: Option<String>,
        deny: bool,
        reconcile: bool,
        cx: &mut Context<Self>,
    ) {
        if self.ai.query_execution_request.is_some() {
            return;
        }
        let Some(chat) = self.ai.chat.as_ref() else {
            return;
        };
        let Some(index) = self.ai.query_executions.iter().position(|s| s.id == id) else {
            return;
        };
        self.ai.sql_execution_selected =
            self.ai.sql_executions.len() + self.ai.typed_executions.len() + index;
        if approval.is_none() {
            self.ai.query_execution_view = None;
        }
        let request_id = uuid::Uuid::new_v4();
        if self.executor_sender.as_ref().is_some_and(|sender| {
            sender
                .send(ExecutorCommand::AiQueryExecution {
                    instance_id: self
                        .selected_instance_id
                        .clone()
                        .unwrap_or_else(|| "local".into()),
                    chat_id: chat.id,
                    id,
                    request_id,
                    approval,
                    deny,
                    reconcile,
                })
                .is_ok()
        }) {
            self.ai.query_execution_request = Some((id, request_id));
        }
        cx.notify();
    }
    fn query_execution_editor(
        &self,
        detail: &AiQueryExecutionDetail,
        cx: &App,
    ) -> Result<Entity<QueryEditor>, &'static str> {
        let editor = self.ai_query_proposal_editor(&detail.proposal, cx)?;
        let item = match detail.target {
            sift_protocol::AiQueryExecutionTarget::PrivateEditor { item_id, .. } => item_id,
            _ => return Err("This action requires the room document owner"),
        };
        let current = self
            .query_semantic_targets
            .get(&item)
            .cloned()
            .or_else(|| self.sourced_semantic_target(item, cx))
            .or_else(|| self.active_semantic_target())
            .ok_or("Original query connection binding is unavailable")?;
        if current.instance_id != self.selected_instance_id.as_deref().unwrap_or("local")
            || detail.proposal.proposal.target.tenant_id != Some(current.tenant_id)
            || detail.proposal.proposal.target.profile_id != Some(current.profile_id)
        {
            return Err("Query connection binding changed since review");
        }
        if self
            .ai
            .runs
            .iter()
            .find(|r| r.run.id == detail.summary.run_id)
            .and_then(|r| r.context.database.as_ref())
            .is_some_and(|database| current.database.as_ref() != Some(database))
        {
            return Err("Query database binding changed since review");
        }
        Ok(editor)
    }
    pub(super) fn decide_ai_query_execution(
        &mut self,
        deny: bool,
        reconcile: bool,
        cx: &mut Context<Self>,
    ) {
        use sift_protocol::AiExecutionState;
        let Some(view) = &self.ai.query_execution_view else {
            return;
        };
        if self.selected_ai_query_execution() != Some(view.summary.id)
            || self.ai.query_execution_request.is_some()
        {
            return;
        }
        if view.summary.state
            != if reconcile {
                AiExecutionState::OutcomeUnknown
            } else {
                AiExecutionState::AwaitingApproval
            }
        {
            return;
        }
        if !deny && !reconcile {
            let Some(detail) = &view.detail else {
                return;
            };
            if detail.summary.expires_at <= chrono::Utc::now() {
                self.ai.error = Some("Query edit review expired".into());
                cx.notify();
                return;
            }
            if matches!(
                detail.target,
                sift_protocol::AiQueryExecutionTarget::PrivateEditor { .. }
            ) {
                if let Err(error) = self.query_execution_editor(detail, cx) {
                    self.ai.error = Some(error.into());
                    cx.notify();
                    return;
                }
            }
        }
        self.send_ai_query_execution(
            view.summary.id,
            Some(view.summary.fingerprint.clone()),
            deny,
            reconcile,
            cx,
        );
    }
    pub(super) fn accept_ai_query_editor_work(
        &mut self,
        work: AiQueryEditorWork,
        cx: &mut Context<Self>,
    ) {
        use sha2::{Digest, Sha256};
        let id = work.detail.summary.id;
        let reviewed = self
            .ai
            .query_execution_view
            .as_ref()
            .and_then(|v| v.detail.as_ref());
        let same_review = reviewed.is_some_and(|d| {
            d.summary.id == id
                && d.summary.fingerprint == work.detail.summary.fingerprint
                && d.target == work.detail.target
                && d.original_sql == work.detail.original_sql
                && d.proposal.proposed_sql == work.detail.proposal.proposed_sql
        });
        let private = matches!(work.detail.target,sift_protocol::AiQueryExecutionTarget::PrivateEditor{desktop_id,..} if desktop_id==work.desktop_id);
        let running = self
            .ai
            .runs
            .iter()
            .find(|r| r.run.id == work.detail.summary.run_id)
            .is_some_and(|r| r.run.status == sift_protocol::AiRunStatus::Running);
        let valid = self.ai.view_id == work.desktop_id
            && !self.ai.query_execution_stopping
            && self.selected_instance_id.as_deref().unwrap_or("local") == work.instance_id
            && self.ai.chat.as_ref().map(|c| c.id) == Some(work.chat_id)
            && self.ai.query_execution_request == Some((id, work.request_id))
            && self.selected_ai_query_execution() == Some(id)
            && same_review
            && private
            && running
            && work.detail.summary.expires_at > chrono::Utc::now();
        if !valid {
            work.complete(AiQueryEditorObservation::unobserved(false));
            return;
        }
        let editor = match self.query_execution_editor(&work.detail, cx) {
            Ok(editor) => editor,
            Err(_) => {
                work.complete(AiQueryEditorObservation::unobserved(false));
                return;
            }
        };
        if !work.begin() {
            return;
        }
        let observed = editor.update(cx, |editor, cx| {
            let before = editor.document().text().to_owned();
            if before != work.detail.original_sql {
                return AiQueryEditorObservation::unobserved(false);
            }
            if !editor.replace_text_with_generated_sql(&work.detail.proposal.proposed_sql, cx) {
                return AiQueryEditorObservation::unobserved(false);
            }
            let after = editor.document().text();
            if after != work.detail.proposal.proposed_sql {
                return AiQueryEditorObservation::unobserved(true);
            }
            AiQueryEditorObservation {
                outcome: AiQueryEditOutcome::Applied,
                before_sha256: Some(format!("{:x}", Sha256::digest(before.as_bytes()))),
                after_sha256: Some(format!("{:x}", Sha256::digest(after.as_bytes()))),
            }
        });
        work.complete(observed);
        cx.notify();
    }
    pub(super) fn render_ai_query_executions(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut view = div().flex().flex_col().gap_2();
        if !self.ai.query_executions.is_empty() {
            view=view.child("Query edits · Ctrl-E · j/k select · Enter review · a approve · d deny · r acknowledge inspected uncertainty");
        }
        for summary in self.ai.query_executions.iter() {
            let id = summary.id;
            view = view.child(
                Button::new(
                    format!("ai-query-edit-{id}"),
                    format!(
                        "{}Query edit {id} · {:?}",
                        if self.ai.sql_execution_focus
                            && self.selected_ai_query_execution() == Some(id)
                        {
                            "> "
                        } else {
                            ""
                        },
                        summary.state
                    ),
                )
                .tone(ButtonTone::Ghost)
                .disabled(self.ai.query_execution_request.is_some())
                .on_click(cx.listener(move |shell, _, _, cx| {
                    shell.send_ai_query_execution(id, None, false, false, cx)
                })),
            );
        }
        if let Some(current) = &self.ai.query_execution_view {
            view = view.child(format!(
                "Exact query claim {} · {:?}",
                current.summary.fingerprint, current.summary.state
            ));
            if let Some(detail) = &current.detail {
                view = view.child(format!(
                    "Target {:?} · expires {}",
                    detail.target, current.summary.expires_at
                ));
                view = view
                    .child("Original SQL")
                    .child(detail.original_sql.clone())
                    .child("Reviewed replacement SQL")
                    .child(detail.proposal.proposed_sql.clone());
            }
            if let Some(receipt) = &current.summary.receipt {
                view = view.child(format!(
                    "{:?} · {:?} · room sequence {:?} · late companion {:?}",
                    receipt.outcome,
                    receipt.evidence,
                    receipt.document_sequence,
                    receipt.late_companion_outcome
                ));
            }
            for (name, label, deny, reconcile) in [
                ("approve", "Approve exact query edit (a)", false, false),
                ("deny", "Deny query edit (d)", true, false),
                (
                    "reconcile",
                    "Acknowledge inspected unknown edit (r)",
                    false,
                    true,
                ),
            ] {
                let required = if reconcile {
                    sift_protocol::AiExecutionState::OutcomeUnknown
                } else {
                    sift_protocol::AiExecutionState::AwaitingApproval
                };
                if current.summary.state == required {
                    view = view.child(
                        Button::new(format!("ai-query-{name}"), label)
                            .disabled(
                                self.ai.query_execution_request.is_some()
                                    || self.selected_ai_query_execution()
                                        != Some(current.summary.id),
                            )
                            .on_click(cx.listener(move |shell, _, _, cx| {
                                shell.decide_ai_query_execution(deny, reconcile, cx)
                            })),
                    );
                }
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
    use uuid::Uuid;
    fn fixture(item: u64) -> (AiRunDetail, AiQueryExecutionDetail) {
        let now = chrono::Utc::now();
        let run = Uuid::new_v4();
        let chat = Uuid::new_v4();
        let desktop = Uuid::new_v4();
        let proposal = Uuid::new_v4();
        let target = ToolContext {
            tenant_id: Some(1),
            room_id: None,
            profile_id: Some(1),
            connection_id: None,
            document_id: None,
        };
        let context:AiTurnContext=serde_json::from_value(serde_json::json!({"target":target,"editor_item_id":item,"database":"original","sql":{"text":"SELECT 1","document_revision":ai_sql_revision("SELECT 1")},"staged_change_count":0})).unwrap();
        let run = AiRunDetail {
            run: AiRun {
                id: run,
                chat_id: chat,
                desktop_id: desktop,
                initiator_principal_id: 1,
                provider: AiProvider::Codex,
                model: None,
                mode: AiMode::Execute,
                status: AiRunStatus::Running,
                next_sequence: 1,
                started_at: now,
                ended_at: None,
            },
            prompt: "edit".into(),
            context,
        };
        let detail = AiQueryExecutionDetail {
            summary: AiQueryExecutionSummary {
                id: Uuid::new_v4(),
                run_id: run.run.id,
                chat_id: chat,
                proposal_id: proposal,
                call_id: Uuid::new_v4(),
                initiator_id: 1,
                fingerprint: "exact query fingerprint".into(),
                state: AiExecutionState::AwaitingApproval,
                created_at: now,
                expires_at: now + chrono::Duration::minutes(5),
                receipt: None,
            },
            target: AiQueryExecutionTarget::PrivateEditor {
                desktop_id: desktop,
                item_id: item,
            },
            original_sql: "SELECT 1".into(),
            proposal: AiQueryProposalDetail {
                proposal: AiProposal {
                    id: proposal,
                    chat_id: chat,
                    run_id: run.run.id,
                    kind: AiProposalKind::QueryTextPatch,
                    status: AiProposalStatus::Staged,
                    target,
                    base_revision: Some(ai_sql_revision("SELECT 1")),
                    content_sha256: "reviewed replacement hash".into(),
                    created_by: 1,
                    applied_by: None,
                    created_at: now,
                    updated_at: now,
                },
                proposed_sql: "SELECT 2".into(),
                external_origin: None,
            },
        };
        (run, detail)
    }
    #[gpui::test]
    fn query_companion_edits_only_the_exact_reviewed_target_and_ignores_stale_dispatch(
        cx: &mut TestAppContext,
    ) {
        let window = crate::shell::tests::shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        workspace.update_in(&mut cx, |shell, window, cx| {
            shell.open_sql_scratch("query.sql".into(), "SELECT 1".into(), window, cx);
            for mode in [
                "applied",
                "wrong_tab",
                "wrong_instance",
                "other_window",
                "changed_source",
                "changed_database",
                "stale",
                "nonce",
                "stopping",
            ] {
                let (item, editor) = shell.active_query_outline_editor(cx).unwrap();
                editor.update(cx, |e, cx| {
                    e.replace_text_with_generated_sql("SELECT 1", cx);
                });
                let (mut run, mut detail) = fixture(item);
                shell.query_semantic_targets.insert(
                    item,
                    SemanticConnectionTarget {
                        instance_id: shell
                            .selected_instance_id
                            .clone()
                            .unwrap_or_else(|| "local".into()),
                        tenant_id: 1,
                        profile_id: if mode == "changed_source" { 2 } else { 1 },
                        profile_name: "Original".into(),
                        provider_id: Engine::Postgres.provider_id(),
                        database: Some(
                            if mode == "changed_database" {
                                "changed"
                            } else {
                                "original"
                            }
                            .into(),
                        ),
                    },
                );
                run.run.desktop_id = shell.ai.view_id;
                detail.target = AiQueryExecutionTarget::PrivateEditor {
                    desktop_id: shell.ai.view_id,
                    item_id: item,
                };
                let id = detail.summary.id;
                let nonce = Uuid::new_v4();
                let now = chrono::Utc::now();
                shell.ai.chat = Some(AiChat {
                    id: run.run.chat_id,
                    tenant_id: 1,
                    room_id: None,
                    owner_principal_id: 1,
                    visibility: AiVisibility::Private,
                    title: "query".into(),
                    revision: 1,
                    created_at: now,
                    updated_at: now,
                });
                shell.ai.runs = vec![run.clone()];
                shell.ai.sql_executions = Arc::new(vec![]);
                shell.ai.typed_executions = Arc::new(vec![]);
                shell.ai.query_executions = Arc::new(vec![detail.summary.clone()]);
                shell.ai.sql_execution_selected = 0;
                shell.ai.query_execution_view = Some(AiQueryExecutionView {
                    summary: detail.summary.clone(),
                    detail: Some(detail.clone()),
                    run: Some(run.clone()),
                });
                shell.ai.query_execution_request = Some((id, nonce));
                shell.ai.query_execution_stopping = mode == "stopping";
                if mode == "wrong_tab" {
                    shell.open_sql_scratch("another.sql".into(), "SELECT 1".into(), window, cx);
                }
                if mode == "stale" {
                    editor.update(cx, |e, cx| {
                        e.replace_text_with_generated_sql("SELECT 3", cx);
                    });
                }
                if mode == "other_window" {
                    shell.ai.view_id = Uuid::new_v4();
                }
                let instance = if mode == "wrong_instance" {
                    "elsewhere"
                } else {
                    shell.selected_instance_id.as_deref().unwrap_or("local")
                };
                let (work, mut wait) = AiQueryEditorWork::channel(
                    instance.into(),
                    run.run.chat_id,
                    if mode == "nonce" {
                        Uuid::new_v4()
                    } else {
                        nonce
                    },
                    run.run.desktop_id,
                    detail.clone(),
                );
                shell.accept_ai_query_editor_work(work, cx);
                let observation = wait.reply.try_recv().unwrap();
                assert_eq!(
                    observation.outcome,
                    if mode == "applied" {
                        AiQueryEditOutcome::Applied
                    } else {
                        AiQueryEditOutcome::Refused
                    },
                    "{mode}"
                );
                assert_eq!(
                    editor.read(cx).document().text(),
                    if mode == "applied" {
                        "SELECT 2"
                    } else if mode == "stale" {
                        "SELECT 3"
                    } else {
                        "SELECT 1"
                    },
                    "{mode}"
                );
                if mode == "applied" {
                    assert!(
                        observation.before_sha256.is_some() && observation.after_sha256.is_some()
                    );
                    let (duplicate, mut reply) = AiQueryEditorWork::channel(
                        shell
                            .selected_instance_id
                            .clone()
                            .unwrap_or_else(|| "local".into()),
                        run.run.chat_id,
                        nonce,
                        run.run.desktop_id,
                        detail.clone(),
                    );
                    shell.accept_ai_query_editor_work(duplicate, cx);
                    assert_eq!(
                        reply.reply.try_recv().unwrap().outcome,
                        AiQueryEditOutcome::Refused
                    );
                    assert_eq!(editor.read(cx).document().text(), "SELECT 2");
                }
                shell.on_executor_event(
                    ExecutorEvent::AiQueryExecution {
                        instance_id: shell
                            .selected_instance_id
                            .clone()
                            .unwrap_or_else(|| "local".into()),
                        chat_id: run.run.chat_id,
                        id,
                        request_id: Uuid::new_v4(),
                        result: Ok(Box::new(AiQueryExecutionView {
                            summary: detail.summary.clone(),
                            detail: None,
                            run: None,
                        })),
                    },
                    cx,
                );
                assert_eq!(
                    shell.ai.query_execution_request,
                    Some((id, nonce)),
                    "a stale reply cannot release pending ownership"
                );
            }
        });
    }
    #[tokio::test]
    async fn query_companion_queue_loss_expiry_and_admitted_reply_have_distinct_evidence() {
        let (run, detail) = fixture(7);
        let create = || {
            AiQueryEditorWork::channel(
                "local".into(),
                run.run.chat_id,
                Uuid::new_v4(),
                run.run.desktop_id,
                detail.clone(),
            )
        };
        let (work, wait) = create();
        drop(work);
        assert_eq!(
            wait.observe(detail.summary.expires_at).await.outcome,
            AiQueryEditOutcome::Refused
        );
        let (work, wait) = create();
        let observed = wait.observe(chrono::Utc::now()).await;
        assert_eq!(observed.outcome, AiQueryEditOutcome::Refused);
        assert!(!work.begin());
        drop(work);
        let (work, wait) = create();
        assert!(work.begin());
        drop(work);
        assert_eq!(
            wait.observe(detail.summary.expires_at).await.outcome,
            AiQueryEditOutcome::OutcomeUnknown
        );
        let (work, wait) = create();
        assert!(work.begin());
        let owner = tokio::spawn(wait.observe(chrono::Utc::now()));
        tokio::task::yield_now().await;
        assert!(
            !owner.is_finished(),
            "expiry cannot invent settlement after local admission"
        );
        work.complete(AiQueryEditorObservation {
            outcome: AiQueryEditOutcome::Applied,
            before_sha256: Some("before".into()),
            after_sha256: Some("after".into()),
        });
        assert_eq!(owner.await.unwrap().outcome, AiQueryEditOutcome::Applied);
    }
}
