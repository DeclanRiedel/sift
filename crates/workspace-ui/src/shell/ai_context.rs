//! Automatic context is derived once for disclosure and send. It remains
//! advisory: only the server authorizes tools and publication.
use super::*;

type ContextToggle = fn(&mut sift_protocol::AiContextInclusion);

impl WorkspaceShell {
    pub(super) fn finalize_ai_context(
        &self,
        mut context: sift_protocol::AiTurnContext,
        results_focused: bool,
        editor: Option<(u64, Entity<QueryEditor>)>,
        cx: &App,
    ) -> sift_protocol::AiTurnContext {
        context.inclusion = self.ai.inclusion;
        let mut workspace = sift_protocol::AiWorkspaceContext::default();
        if results_focused {
            if let Some(sql) = &context.sql {
                workspace.document_revision = sql.document_revision;
                workspace.current_statement = u32::try_from(sql.text.len())
                    .ok()
                    .map(|end| sift_protocol::TextRange { start: 0, end });
            }
        } else if let Some((_, editor)) = editor {
            let editor = editor.read(cx);
            workspace.document_revision = Some(ai_sql_revision(editor.document().text()));
            workspace.editor_read_only = Some(editor.is_read_only());
            workspace.current_statement = editor.document().active_statement().and_then(|range| {
                Some(sift_protocol::TextRange {
                    start: u32::try_from(range.start).ok()?,
                    end: u32::try_from(range.end).ok()?,
                })
            });
            workspace.diagnostics_stale =
                editor.semantic().diagnostics_stale(editor.text_revision());
            let diagnostics = editor.semantic().diagnostics();
            workspace.diagnostics_truncated = context.inclusion.diagnostics
                && (diagnostics.len() > 32
                    || diagnostics.iter().take(32).any(|diagnostic| {
                        diagnostic.code.len() > 128 || diagnostic.message.len() > 2048
                    }));
            if !workspace.diagnostics_stale && context.inclusion.diagnostics {
                workspace.diagnostics = diagnostics
                    .iter()
                    .take(32)
                    .map(|diagnostic| sift_protocol::AiContextDiagnostic {
                        severity: diagnostic.severity,
                        code: bounded_context_text(&diagnostic.code, 128),
                        message: bounded_context_text(&diagnostic.message, 2048),
                        range: sift_protocol::TextRange {
                            start: diagnostic.range.start as u32,
                            end: diagnostic.range.end as u32,
                        },
                    })
                    .collect();
                workspace.diagnostics_omitted =
                    diagnostics.len().saturating_sub(32).min(u32::MAX as usize) as u32;
            } else if workspace.diagnostics_stale && context.inclusion.diagnostics {
                workspace.diagnostics_omitted = diagnostics.len().min(u32::MAX as usize) as u32;
            }
        }
        let current_connection =
            self.active_query_connection
                .is_some_and(|(profile, session, connection)| {
                    context.target.profile_id == Some(profile)
                        && context
                            .target
                            .connection_id
                            .as_deref()
                            .is_some_and(|target| {
                                target == "active"
                                    || target == format!("{}:{}", session.0, connection.0)
                            })
                });
        if context.inclusion.connection_state && current_connection {
            if !self.operation_capabilities.is_empty() {
                workspace.connection_read_only = Some(self.connection_is_read_only());
            }
            workspace.transaction = Some(sift_protocol::AiTransactionContext {
                active: self.transaction_state.transaction().is_some(),
                pending: self.transaction_state.is_pending(),
                condition: self.transaction_state.condition(),
                mode: self
                    .transaction_state
                    .transaction()
                    .map(|transaction| transaction.mode),
            });
        }
        if !context.inclusion.sql || context.sql.is_none() {
            context.sql = None;
            workspace.document_revision = None;
            workspace.current_statement = None;
        }
        if !context.inclusion.selection {
            if let Some(sql) = &mut context.sql {
                sql.selected_start = None;
                sql.selected_end = None;
            }
        }
        if !context.inclusion.errors {
            context.current_error = None;
        } else if let Some(error) = &mut context.current_error {
            *error = bounded_context_text(error, 4096);
        }
        if !context.inclusion.environment {
            context.database = None;
            context.dialect = None;
            context.environment_label = None;
        }
        if !context.inclusion.diagnostics {
            workspace.diagnostics_stale = false;
            workspace.diagnostics_truncated = false;
        }
        if !context.inclusion.connection_state {
            workspace.editor_read_only = None;
            workspace.connection_read_only = None;
            workspace.transaction = None;
        }
        let public = self
            .ai
            .chat
            .as_ref()
            .map(|chat| chat.visibility)
            .or_else(|| {
                self.ai
                    .policy
                    .as_ref()
                    .map(|policy| policy.new_chat_visibility)
            })
            == Some(sift_protocol::AiVisibility::RoomPublic);
        if public {
            context.workspace = None;
            context.current_error = None;
            context.database = None;
            context.dialect = None;
            context.environment_label = None;
        } else {
            context.workspace = Some(workspace);
        }
        context
    }

    pub(super) fn render_ai_context_state(
        workspace: &sift_protocol::AiWorkspaceContext,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let transaction =
            workspace
                .transaction
                .as_ref()
                .map_or("unknown or excluded", |transaction| {
                    if transaction.pending {
                        "pending"
                    } else if !transaction.active {
                        "idle"
                    } else if transaction.condition == sift_protocol::TransactionCondition::Failed {
                        "failed"
                    } else {
                        "active"
                    }
                });
        let connection =
            workspace
                .connection_read_only
                .map_or("state unknown or excluded", |read_only| {
                    if read_only {
                        "read-only"
                    } else {
                        "write operations available"
                    }
                });
        div()
            .text_xs()
            .text_color(cx.theme().colors.muted_text)
            .whitespace_normal()
            .child(format!(
                "{} diagnostics{}{} · transaction {transaction} · connection {connection}",
                workspace.diagnostics.len(),
                if workspace.diagnostics_stale {
                    " (stale diagnostics omitted)"
                } else {
                    ""
                },
                if workspace.diagnostics_omitted > 0 || workspace.diagnostics_truncated {
                    " (bounded excerpt)"
                } else {
                    ""
                }
            ))
            .into_any_element()
    }

    pub(super) fn render_ai_context_choices(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut view = div().flex().flex_col().gap_1().text_xs();
        if !self.ai.context_choices_open {
            return view.into_any_element();
        }
        view = view.child("Automatic context · remembered for this chat");
        let options: [(&str, &str, bool, ContextToggle); 6] = [
            ("sql", "SQL", self.ai.inclusion.sql, |options| {
                options.sql = !options.sql
            }),
            (
                "selection",
                "Selection",
                self.ai.inclusion.selection,
                |options| options.selection = !options.selection,
            ),
            (
                "errors",
                "Query error",
                self.ai.inclusion.errors,
                |options| options.errors = !options.errors,
            ),
            (
                "diagnostics",
                "Editor diagnostics",
                self.ai.inclusion.diagnostics,
                |options| options.diagnostics = !options.diagnostics,
            ),
            (
                "environment",
                "Connection labels",
                self.ai.inclusion.environment,
                |options| options.environment = !options.environment,
            ),
            (
                "connection-state",
                "Transaction / read-only state",
                self.ai.inclusion.connection_state,
                |options| options.connection_state = !options.connection_state,
            ),
        ];
        for (id, label, included, toggle) in options {
            view = view.child(
                Button::new(
                    format!("ai-context-{id}"),
                    format!("{} {label}", if included { "✓" } else { "○" }),
                )
                .tone(if included {
                    ButtonTone::Accent
                } else {
                    ButtonTone::Ghost
                })
                .disabled(self.ai.pending)
                .on_click(cx.listener(move |shell, _, _, cx| {
                    toggle(&mut shell.ai.inclusion);
                    cx.notify();
                })),
            );
        }
        view.child("Rows and bind values are never automatic. Reviewed attachments keep their own explicit selection.").into_any_element()
    }
}

fn bounded_context_text(text: &str, max_bytes: usize) -> String {
    let mut end = text.len().min(max_bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}
