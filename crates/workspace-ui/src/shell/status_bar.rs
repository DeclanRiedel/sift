//! Status-bar projection and rendering.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusBar {
    pub connection: String,
    pub database: String,
    pub transaction: String,
    pub room: String,
    pub execution: String,
}

impl Default for StatusBar {
    fn default() -> Self {
        Self {
            connection: "Offline".into(),
            database: "No database".into(),
            transaction: "TX: None".into(),
            room: "Local workspace".into(),
            execution: "Ready".into(),
        }
    }
}

fn render_database_breadcrumb(
    item_id: u64,
    source: DatabaseObjectSource,
    colors: sift_ui::ThemeColors,
    cx: &mut Context<WorkspaceShell>,
) -> gpui::AnyElement {
    let segments = [
        (
            DatabaseBreadcrumbLevel::Connection,
            source.profile_name.clone(),
        ),
        (
            DatabaseBreadcrumbLevel::Catalog,
            source.catalog.clone().unwrap_or_else(|| "default".into()),
        ),
        (DatabaseBreadcrumbLevel::Schema, source.schema.clone()),
        (DatabaseBreadcrumbLevel::Object, source.object.clone()),
    ];
    let mut breadcrumb = div()
        .id(("database-breadcrumb", item_id as usize))
        .debug_selector(|| "database-breadcrumb".into())
        .min_w_0()
        .flex()
        .items_center()
        .overflow_hidden()
        .text_xs();
    for (index, (level, label)) in segments.into_iter().enumerate() {
        if index > 0 {
            breadcrumb = breadcrumb.child(icon(IconName::ChevronRight, colors.disabled_text, 9.));
        }
        let source = source.clone();
        breadcrumb = breadcrumb.child(
            div()
                .id(format!("database-breadcrumb-segment-{item_id}-{index}"))
                .min_w_0()
                .max_w(px(130.))
                .px_1()
                .truncate()
                .rounded_sm()
                .text_color(if level == DatabaseBreadcrumbLevel::Object {
                    colors.text
                } else {
                    colors.muted_text
                })
                .role(Role::Button)
                .aria_label(format!("Reveal {label} in connections"))
                .hover(|segment| segment.bg(colors.hovered_surface).text_color(colors.text))
                .on_click(cx.listener(move |shell, _, window, cx| {
                    shell.reveal_database_object(&source, level, window, cx);
                }))
                .child(label),
        );
    }
    breadcrumb.into_any_element()
}

pub(super) fn render_status_bar(
    shell: &WorkspaceShell,
    cx: &mut Context<WorkspaceShell>,
) -> gpui::AnyElement {
    let theme = cx.theme();
    let colors = theme.colors;
    let button = |id: &'static str,
                  icon_name: IconName,
                  tooltip: String,
                  selected: bool,
                  badge: Option<usize>,
                  danger: bool| {
        IconButton::new(id, icon_name, tooltip.clone())
            .toggle_state(selected)
            .danger(danger)
            .badge(badge)
            .tooltip(tooltip)
    };
    let separator = || {
        div()
            .flex_none()
            .w(px(1.))
            .h(px(14.))
            .mx_1()
            .bg(colors.border)
    };
    let (error_count, warning_count) =
        shell
            .global_problems
            .iter()
            .fold(
                (0usize, 0usize),
                |(errors, warnings), problem| match problem.severity {
                    ProblemSeverity::Error => (errors.saturating_add(1), warnings),
                    ProblemSeverity::Warning => (errors, warnings.saturating_add(1)),
                },
            );
    let problem_count = error_count.saturating_add(warning_count);
    let problem_icon_color = if error_count > 0 {
        colors.danger
    } else if warning_count > 0 {
        colors.warning
    } else {
        colors.muted_text
    };
    let (cursor_label, cursor_tooltip) = shell.active_cursor_position(cx).map_or_else(
        || ("-:-".into(), "No active query cursor".into()),
        |(line, column)| {
            (
                format!("{line}:{column}"),
                format!("Line {line}, column {column}"),
            )
        },
    );
    let (mode_label, mode_tooltip, vim_entered) = match shell.active_editor_mode(cx) {
        Some((EditorKeymap::Vim, VimMode::Normal, entered)) => (
            "NORMAL",
            "Vim normal mode; click to use the standard keymap",
            Some(entered),
        ),
        Some((EditorKeymap::Vim, VimMode::Insert, entered)) => (
            "INSERT",
            "Vim insert mode; Escape returns to normal mode",
            Some(entered),
        ),
        Some((EditorKeymap::Vim, VimMode::Visual, entered)) => (
            "VISUAL",
            "Vim visual mode; Escape returns to normal mode",
            Some(entered),
        ),
        Some((EditorKeymap::Vim, VimMode::Select, entered)) => (
            "SELECT",
            "Vim select mode; Escape returns to normal mode",
            Some(entered),
        ),
        Some((EditorKeymap::Vim, VimMode::OperatorPending, entered)) => {
            ("OPERATOR", "Vim operator-pending mode", Some(entered))
        }
        Some((EditorKeymap::Vim, VimMode::Command, entered)) => {
            ("COMMAND", "Vim command mode", Some(entered))
        }
        Some((EditorKeymap::Standard, _, _)) => (
            "STANDARD",
            "Standard editor keymap; click to enable Vim mode",
            None,
        ),
        None => ("NO EDITOR", "No active editor", None),
    };
    let mode_can_toggle = shell.keyboard_profile() == KeyboardProfile::Hybrid
        && shell.active_editor_mode(cx).is_some();
    let mode_tooltip = if mode_can_toggle {
        mode_tooltip.to_owned()
    } else {
        match shell.keyboard_profile() {
            KeyboardProfile::Vim => "Vim editor keymap is fixed by the Vim keyboard profile".into(),
            KeyboardProfile::Standard => {
                "Standard editor keymap is fixed by the Standard keyboard profile".into()
            }
            KeyboardProfile::Hybrid => mode_tooltip.to_owned(),
        }
    };
    let editor_buffer = vim_entered.unwrap_or_default();
    let ide_buffer = shell.ide_key_buffer();
    let ide_buffer_label = if ide_buffer.is_empty() {
        "—".to_owned()
    } else {
        ide_buffer.clone()
    };
    let active_database_source = shell.panes.get(shell.active_pane).and_then(|pane| {
        let pane = pane.read(cx);
        let item = pane.active_item()?;
        pane.database_source(item.id)
            .map(|source| (item.id, source))
    });
    let database_breadcrumb = active_database_source
        .map(|(item_id, source)| render_database_breadcrumb(item_id, source, colors, cx));

    div()
        .id("status-bar")
        .debug_selector(|| "status-bar".into())
        .role(Role::Toolbar)
        .aria_label("Workspace status")
        .tab_group()
        .h(theme.metrics.status_height)
        .w_full()
        .flex()
        .items_center()
        .gap_1()
        .px_1()
        .pt(px(1.))
        .border_t_1()
        .border_color(colors.subtle_border)
        .bg(colors.toolbar)
        .text_xs()
        .text_color(colors.muted_text)
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(
                    button(
                        "footer-connections",
                        IconName::Database,
                        "Connections".into(),
                        shell.left_dock.presentation.open
                            && shell.active_left_panel == LeftPanel::Connections,
                        None,
                        false,
                    )
                    .on_click(cx.listener(|shell, _, window, cx| {
                        shell.select_left_panel(LeftPanel::Connections, window, cx)
                    })),
                )
                .child(
                    button(
                        "footer-git",
                        IconName::VersionControl,
                        "Git workspace".into(),
                        shell.left_dock.presentation.open
                            && shell.active_left_panel == LeftPanel::Git,
                        shell.repository.status().and_then(|status| {
                            (!status.entries.is_empty()).then_some(status.entries.len())
                        }),
                        false,
                    )
                    .on_click(cx.listener(|shell, _, window, cx| {
                        shell.select_left_panel(LeftPanel::Git, window, cx)
                    })),
                )
                .children(shell.repository.status().map(|status| {
                    let branch = status.branch.as_deref().unwrap_or("detached");
                    let changed = status.entries.len();
                    div()
                        .id("footer-git-branch")
                        .debug_selector(|| "footer-git-branch".into())
                        .aria_label(format!("Git branch {branch}, {changed} changed path(s)"))
                        .max_w(px(180.))
                        .truncate()
                        .font_family("monospace")
                        .text_color(if changed == 0 {
                            colors.muted_text
                        } else {
                            colors.accent
                        })
                        .child(format!("{branch} · {changed}"))
                }))
                .child(
                    button(
                        "footer-collaboration",
                        IconName::Users,
                        format!(
                            "Collaboration ({} participants)",
                            shell.presence.participants.len()
                        ),
                        shell.left_dock.presentation.open
                            && shell.active_left_panel == LeftPanel::Collaboration,
                        Some(shell.presence.participants.len()),
                        false,
                    )
                    .on_click(cx.listener(|shell, _, window, cx| {
                        shell.select_left_panel(LeftPanel::Collaboration, window, cx)
                    })),
                )
                .child(
                    div()
                        .debug_selector(|| "footer-query-outline".into())
                        .child(
                            button(
                                "footer-query-outline",
                                IconName::Outline,
                                "Query outline".into(),
                                shell.left_dock.presentation.open
                                    && shell.active_left_panel == LeftPanel::QueryOutline,
                                None,
                                false,
                            )
                            .on_click(cx.listener(
                                |shell, _, window, cx| {
                                    shell.select_left_panel(LeftPanel::QueryOutline, window, cx)
                                },
                            )),
                        ),
                )
                .child(
                    div()
                        .debug_selector(|| "footer-saved-queries".into())
                        .child(
                            button(
                                "footer-saved-queries",
                                IconName::Folder,
                                "Saved queries".into(),
                                shell.left_dock.presentation.open
                                    && shell.active_left_panel == LeftPanel::SavedQueries,
                                None,
                                false,
                            )
                            .on_click(cx.listener(
                                |shell, _, window, cx| {
                                    shell.select_left_panel(LeftPanel::SavedQueries, window, cx)
                                },
                            )),
                        ),
                )
                .child(
                    div()
                        .debug_selector(|| "footer-query-history".into())
                        .child(
                            button(
                                "footer-query-history",
                                IconName::Activity,
                                "Query history".into(),
                                shell.left_dock.presentation.open
                                    && shell.active_left_panel == LeftPanel::QueryHistory,
                                None,
                                false,
                            )
                            .on_click(cx.listener(
                                |shell, _, window, cx| {
                                    shell.select_left_panel(LeftPanel::QueryHistory, window, cx)
                                },
                            )),
                        ),
                ),
        )
        .child(separator())
        .child(
            div()
                .flex()
                .flex_1()
                .min_w_0()
                .overflow_x_hidden()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .id("footer-problems")
                        .role(Role::Button)
                        .aria_label(format!(
                            "Open problems: {error_count} error(s), {warning_count} warning(s)"
                        ))
                        .h(theme.metrics.compact_control_height)
                        .min_w(theme.metrics.compact_control_height)
                        .px_1()
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .gap_1()
                        .rounded_sm()
                        .hover(|button| button.bg(colors.hovered_surface))
                        .on_click(cx.listener(|shell, _, window, cx| {
                            shell.show_global_problems(window, cx)
                        }))
                        .child(icon(IconName::Warning, problem_icon_color, 14.))
                        .children((error_count > 0).then(|| {
                            div()
                                .id("footer-error-count")
                                .debug_selector(|| "footer-error-count".into())
                                .font_family("monospace")
                                .text_color(colors.danger)
                                .child(error_count.to_string())
                        }))
                        .children((warning_count > 0).then(|| {
                            div()
                                .id("footer-warning-count")
                                .debug_selector(|| "footer-warning-count".into())
                                .font_family("monospace")
                                .text_color(colors.warning)
                                .child(warning_count.to_string())
                        })),
                )
                .children((problem_count > 0).then(|| {
                    button(
                        "footer-copy-problems",
                        IconName::Copy,
                        "Copy errors and warnings".into(),
                        false,
                        None,
                        false,
                    )
                    .on_click(cx.listener(|shell, _, _, cx| shell.copy_all_global_problems(cx)))
                }))
                .children(database_breadcrumb)
                .children(shell.transaction_state.transaction().map(|_| {
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(separator())
                        .child(shell.status.transaction.clone())
                        .child(
                            Button::new("footer-create-savepoint", "Savepoint")
                                .tone(ButtonTone::Neutral)
                                .disabled(
                                    shell.transaction_state.is_pending()
                                        || shell.transaction_state.is_aborted(),
                                )
                                .on_click(
                                    cx.listener(|shell, _, _, cx| shell.create_savepoint(cx)),
                                ),
                        )
                        .children(shell.savepoints.last().cloned().map(|name| {
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .child(format!("{} · {}", shell.savepoints.len(), name))
                                .child(
                                    Button::new("footer-rollback-savepoint", "Undo to")
                                        .tone(ButtonTone::Neutral)
                                        .disabled(shell.transaction_state.is_pending())
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.rollback_last_savepoint(cx)
                                        })),
                                )
                                .child(
                                    Button::new("footer-release-savepoint", "Release")
                                        .tone(ButtonTone::Ghost)
                                        .disabled(shell.transaction_state.is_pending())
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.release_last_savepoint(cx)
                                        })),
                                )
                        }))
                        .child(
                            Button::new("footer-commit-transaction", "Commit")
                                .debug_selector("footer-commit-transaction")
                                .tone(ButtonTone::Accent)
                                .disabled(
                                    shell.transaction_state.is_pending()
                                        || !shell.running_queries.is_empty()
                                        || shell.transaction_state.is_aborted(),
                                )
                                .on_click(cx.listener(|shell, _, _, cx| {
                                    shell.finish_transaction(true, cx)
                                })),
                        )
                        .child(
                            Button::new("footer-rollback-transaction", "Rollback")
                                .debug_selector("footer-rollback-transaction")
                                .tone(ButtonTone::DangerGhost)
                                .disabled(shell.transaction_state.is_pending())
                                .on_click(cx.listener(|shell, _, _, cx| {
                                    shell.finish_transaction(false, cx)
                                })),
                        )
                })),
        )
        .child(separator())
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_1()
                .child({
                    div()
                        .id("footer-cursor-position")
                        .aria_label(cursor_tooltip.clone())
                        .h(theme.metrics.compact_control_height)
                        .min_w(px(30.))
                        .px_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .font_family("monospace")
                        .text_color(colors.muted_text)
                        .child(cursor_label)
                        .tooltip(move |_, cx| {
                            cx.new(|_| Tooltip::new(cursor_tooltip.clone())).into()
                        })
                })
                .child({
                    div()
                        .id("footer-editor-mode")
                        .debug_selector(|| "footer-editor-mode".into())
                        .aria_label(mode_tooltip.clone())
                        .h(theme.metrics.compact_control_height)
                        .px_1()
                        .flex()
                        .items_center()
                        .text_color(colors.muted_text)
                        .when(mode_can_toggle, |mode| {
                            mode.role(Role::Button)
                                .hover(|button| {
                                    button.bg(colors.hovered_surface).text_color(colors.text)
                                })
                                .on_click(cx.listener(|shell, _, _, cx| {
                                    shell.toggle_active_editor_keymap(cx)
                                }))
                        })
                        .child(mode_label)
                        .tooltip(move |_, cx| cx.new(|_| Tooltip::new(mode_tooltip.clone())).into())
                })
                .children((!editor_buffer.is_empty()).then(|| {
                    div()
                        .id("footer-editor-buffer")
                        .aria_label(format!("Pending editor keys: {editor_buffer}"))
                        .h(theme.metrics.compact_control_height)
                        .px_1()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(div().text_color(colors.muted_text).child("INPUT"))
                        .child(
                            div()
                                .font_family("monospace")
                                .text_color(colors.accent)
                                .child(editor_buffer),
                        )
                }))
                .child(separator())
                .child({
                    div()
                        .id("footer-ide-buffer")
                        .aria_label(if ide_buffer.is_empty() {
                            "IDE command input is inactive".to_owned()
                        } else {
                            format!("IDE command input: {ide_buffer}")
                        })
                        .h(theme.metrics.compact_control_height)
                        .min_w(px(64.))
                        .px_1()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(div().text_color(colors.muted_text).child("IDE"))
                        .child(
                            div()
                                .font_family("monospace")
                                .text_color(if ide_buffer.is_empty() {
                                    colors.muted_text
                                } else {
                                    colors.accent
                                })
                                .child(ide_buffer_label),
                        )
                        .tooltip({
                            let message: SharedString = "Pending IDE command sequence".into();
                            move |_, cx| cx.new(|_| Tooltip::new(message.clone())).into()
                        })
                })
                .child(separator())
                .child(
                    button(
                        "footer-monitor",
                        IconName::Activity,
                        "Connection and execution monitor".into(),
                        shell.bottom_dock.presentation.open
                            && shell.active_bottom_tool == BottomTool::Monitor,
                        None,
                        false,
                    )
                    .on_click(cx.listener(|shell, _, _, cx| {
                        shell.select_bottom_tool(BottomTool::Monitor, cx)
                    })),
                )
                .child(
                    button(
                        "footer-automations",
                        IconName::Automations,
                        "Automations".into(),
                        shell.bottom_dock.presentation.open
                            && shell.active_bottom_tool == BottomTool::Automations,
                        None,
                        false,
                    )
                    .on_click(cx.listener(|shell, _, _, cx| {
                        shell.select_bottom_tool(BottomTool::Automations, cx)
                    })),
                )
                .child(separator())
                .child(
                    div().id("footer-inspector-toggle-slot").flex_none().child(
                        button(
                            "footer-toggle-inspector",
                            IconName::CloseRightPane,
                            if shell.right_dock.presentation.open {
                                "Close Inspector".into()
                            } else {
                                "Open Inspector".into()
                            },
                            shell.right_dock.presentation.open,
                            None,
                            false,
                        )
                        .on_click(cx.listener(|shell, _, window, cx| {
                            shell.toggle_right_dock(&super::ToggleRightDock, window, cx)
                        })),
                    ),
                ),
        )
        .into_any_element()
}
