//! SQL draft review shares the same destination and revision checks as apply.
use super::*;

impl WorkspaceShell {
    pub(super) fn ai_query_proposal_editor(
        &self,
        proposal: &sift_protocol::AiQueryProposalDetail,
        cx: &App,
    ) -> Result<Entity<QueryEditor>, &'static str> {
        let run = self
            .ai
            .runs
            .iter()
            .find(|run| run.run.id == proposal.proposal.run_id)
            .ok_or("Original query context is unavailable; open a new query instead")?;
        let (item_id, editor) = self
            .active_query_outline_editor(cx)
            .ok_or("Open the original query tab before replacing its SQL")?;
        let current_document_id = self
            .panes
            .get(self.active_pane)
            .and_then(|pane| pane.read(cx).room_document_source(item_id))
            .map(|source| source.document_id.to_string());
        let same_target = if proposal.proposal.target.document_id.is_some() {
            current_document_id == proposal.proposal.target.document_id
        } else {
            run.context.editor_item_id == Some(item_id)
        };
        if !same_target {
            return Err("Open the original query tab before replacing its SQL");
        }
        let text = editor.read(cx).document().text();
        if proposal.proposal.base_revision != Some(ai_sql_revision(text))
            || run.context.sql.as_ref().is_none_or(|sql| sql.text != text)
        {
            return Err("SQL changed since the draft was staged; review it again");
        }
        Ok(editor)
    }

    pub(super) fn render_ai_sql_draft(
        &self,
        proposal: sift_protocol::AiQueryProposalDetail,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors;
        let id = proposal.proposal.id;
        let run = self
            .ai
            .runs
            .iter()
            .find(|run| run.run.id == proposal.proposal.run_id);
        let original = run.and_then(|run| run.context.sql.as_ref());
        let title = run
            .and_then(|run| run.context.editor_item_id)
            .and_then(|id| {
                self.panes.iter().find_map(|pane| {
                    pane.read(cx)
                        .items
                        .iter()
                        .find(|item| item.id == id)
                        .map(|item| item.title.clone())
                })
            })
            .unwrap_or_else(|| {
                proposal
                    .proposal
                    .target
                    .document_id
                    .as_ref()
                    .map_or("Original query".into(), |id| format!("Shared query {id}"))
            });
        let connection = run
            .and_then(|run| run.context.environment_label.clone())
            .or_else(|| {
                proposal.proposal.target.profile_id.and_then(|id| {
                    self.semantic_target_for_profile(id)
                        .map(|target| target.profile_name)
                })
            });
        let database = run.and_then(|run| run.context.database.as_deref());
        let destination = [Some(title), connection, database.map(str::to_owned)]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        let staged = proposal.proposal.status == sift_protocol::AiProposalStatus::Staged;
        let ready = self.ai_query_proposal_editor(&proposal, cx);
        div()
            .debug_selector(move || format!("ai-sql-draft-{id}"))
            .whitespace_normal()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_1()
            .py_1()
            .children(proposal.external_origin.as_ref().map(|origin| {
                div().text_xs().text_color(colors.muted_text).child(format!(
                    "Source intent · {} · {} · reviewed revision {}",
                    origin.source.label, origin.tool_alias, origin.source.source_revision
                ))
            }))
            .child(
                div()
                    .debug_selector(|| "ai-sql-destination".into())
                    .text_xs()
                    .text_color(colors.muted_text)
                    .child(format!(
                        "SQL draft · {:?} → {destination}",
                        proposal.proposal.status
                    )),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(colors.muted_text)
                    .child(original.map_or_else(
                        || format!("{} proposed lines", proposal.proposed_sql.lines().count()),
                        |sql| {
                            format!(
                                "{} original lines → {} proposed lines",
                                sql.text.lines().count(),
                                proposal.proposed_sql.lines().count()
                            )
                        },
                    )),
            )
            .when(staged, |view| {
                view.child(
                    div()
                        .debug_selector(|| "ai-sql-review-state".into())
                        .text_xs()
                        .text_color(colors.muted_text)
                        .child(ready.as_ref().map_or_else(
                            |error| *error,
                            |_| "Replaces editor text · review before running",
                        )),
                )
            })
            .child(self.render_ai_code(
                &format!("proposal-{id}"),
                "sql",
                &proposal.proposed_sql,
                cx,
            ))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_1()
                    .when(original.is_some(), |view| {
                        view.child(
                            Button::new(format!("ai-compare-proposal-{id}"), "Compare original")
                                .debug_selector("ai-compare-original")
                                .tone(ButtonTone::Ghost)
                                .on_click(cx.listener(move |shell, _, window, cx| {
                                    shell.close_ai_popups();
                                    shell.ai.sql_comparison = Some(id);
                                    shell
                                        .ai
                                        .sql_comparison_scroll
                                        .set_offset(gpui::point(px(0.), px(0.)));
                                    shell.ai.transcript_focus.focus(window, cx);
                                    cx.notify();
                                })),
                        )
                    })
                    .when(staged, |view| {
                        view.child(
                            Button::new(format!("ai-apply-proposal-{id}"), "Replace editor SQL")
                                .debug_selector("ai-replace-sql")
                                .disabled(ready.is_err() || self.ai.pending_room_apply.is_some())
                                .on_click(cx.listener(move |shell, _, _, cx| {
                                    shell.apply_ai_proposal(id, cx)
                                })),
                        )
                        .child(
                            Button::new(format!("ai-copy-proposal-{id}"), "Open as new query")
                                .tone(ButtonTone::Ghost)
                                .on_click(cx.listener(move |shell, _, window, cx| {
                                    shell.open_ai_proposal_copy(id, window, cx)
                                })),
                        )
                    }),
            )
            .into_any_element()
    }

    pub(super) fn render_ai_sql_comparison(
        &self,
        max_width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(proposal) = self.ai.sql_comparison.and_then(|id| {
            self.ai
                .proposals
                .iter()
                .find(|proposal| proposal.proposal.id == id)
        }) else {
            return div().into_any_element();
        };
        let Some(original) = self
            .ai
            .runs
            .iter()
            .find(|run| run.run.id == proposal.proposal.run_id)
            .and_then(|run| run.context.sql.as_ref())
        else {
            return div().into_any_element();
        };
        let colors = cx.theme().colors;
        div()
            .id("ai-sql-comparison")
            .debug_selector(|| "ai-sql-comparison".into())
            .absolute()
            .right_3()
            .top(px(40.))
            .w(px(max_width.min(640.)))
            .max_w(px(max_width))
            .border_1()
            .border_color(colors.subtle_border)
            .rounded_md()
            .bg(colors.elevated_surface)
            .shadow_md()
            .occlude()
            .p_2()
            .flex()
            .flex_col()
            .gap_2()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down_out(cx.listener(|shell, _, _, cx| {
                shell.ai.sql_comparison = None;
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_xs()
                    .child("Review SQL changes")
                    .child(
                        IconButton::new(
                            "ai-close-sql-comparison",
                            IconName::Close,
                            "Close comparison",
                        )
                        .on_click(cx.listener(|shell, _, _, cx| {
                            shell.ai.sql_comparison = None;
                            cx.notify();
                        })),
                    ),
            )
            .child(
                div()
                    .id("ai-sql-comparison-scroll")
                    .track_scroll(&self.ai.sql_comparison_scroll)
                    .max_h(px(360.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .min_w_0()
                    .child(
                        div()
                            .text_xs()
                            .text_color(colors.muted_text)
                            .child("Before · original text when the draft was staged"),
                    )
                    .child(self.render_ai_code("comparison-before", "sql", &original.text, cx))
                    .child(
                        div()
                            .text_xs()
                            .text_color(colors.muted_text)
                            .child("After · proposed replacement"),
                    )
                    .child(self.render_ai_code(
                        "comparison-after",
                        "sql",
                        &proposal.proposed_sql,
                        cx,
                    )),
            )
            .into_any_element()
    }
}
