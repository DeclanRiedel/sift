//! Status-bar projection and rendering.

use super::*;

struct StatusTooltip {
    message: String,
    theme: Theme,
}

impl gpui::Render for StatusTooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let colors = self.theme.colors;
        div()
            .max_w(px(360.))
            .px_2()
            .py_1()
            .rounded_sm()
            .border_1()
            .border_color(colors.strong_border)
            .bg(colors.elevated_surface)
            .shadow_md()
            .text_xs()
            .text_color(colors.text)
            .whitespace_normal()
            .child(self.message.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusBar {
    pub connection: String,
    pub database: String,
    pub transaction: String,
    pub room: String,
    pub execution: String,
    pub diagnostic_count: usize,
    pub current_error: Option<String>,
}

impl Default for StatusBar {
    fn default() -> Self {
        Self {
            connection: "Offline".into(),
            database: "No database".into(),
            transaction: "TX: None".into(),
            room: "Local workspace".into(),
            execution: "Ready".into(),
            diagnostic_count: 0,
            current_error: None,
        }
    }
}

pub(super) fn render_status_bar(
    shell: &WorkspaceShell,
    cx: &mut Context<WorkspaceShell>,
) -> gpui::AnyElement {
    let theme = shell.theme;
    let colors = theme.colors;
    let button = |id: &'static str,
                  icon_name: IconName,
                  tooltip: String,
                  selected: bool,
                  badge: Option<usize>,
                  danger: bool| {
        let foreground = if danger {
            colors.danger
        } else if selected {
            colors.text
        } else {
            colors.muted_text
        };
        let tooltip_message = tooltip.clone();
        div()
            .id(id)
            .role(Role::Button)
            .aria_label(tooltip)
            .h(theme.metrics.compact_control_height)
            .min_w(theme.metrics.compact_control_height)
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .gap_1()
            .px_1()
            .rounded_sm()
            .text_color(foreground)
            .when(selected, |button| button.bg(colors.active_surface))
            .hover(|button| button.bg(colors.hovered_surface).text_color(colors.text))
            .child(icon(icon_name, foreground, 14.))
            .children(badge.filter(|count| *count > 0).map(|count| {
                div()
                    .min_w(px(12.))
                    .h(px(12.))
                    .px(px(3.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(if danger {
                        colors.danger_muted
                    } else {
                        colors.active_surface
                    })
                    .text_color(foreground)
                    .text_size(px(9.))
                    .child(count.to_string())
            }))
            .tooltip(move |_, cx| {
                cx.new(|_| StatusTooltip {
                    message: tooltip_message.clone(),
                    theme,
                })
                .into()
            })
    };
    let separator = || {
        div()
            .flex_none()
            .w(px(1.))
            .h(px(14.))
            .mx_1()
            .bg(colors.border)
    };
    let target_label = match &shell.connection_status {
        ConnectionStatus::Connected { .. } => {
            format!("{} · {}", shell.status.database, shell.status.room)
        }
        _ => shell.status.database.clone(),
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
            "VIM NORMAL",
            "Vim normal mode; click to use the standard keymap",
            Some(entered),
        ),
        Some((EditorKeymap::Vim, VimMode::Insert, entered)) => (
            "VIM INSERT",
            "Vim insert mode; Escape returns to normal mode",
            Some(entered),
        ),
        Some((EditorKeymap::Vim, VimMode::Visual, entered)) => (
            "VIM VISUAL",
            "Vim visual mode; Escape returns to normal mode",
            Some(entered),
        ),
        Some((EditorKeymap::Vim, VimMode::Select, entered)) => (
            "VIM SELECT",
            "Vim select mode; Escape returns to normal mode",
            Some(entered),
        ),
        Some((EditorKeymap::Vim, VimMode::OperatorPending, entered)) => {
            ("VIM OPERATOR", "Vim operator-pending mode", Some(entered))
        }
        Some((EditorKeymap::Vim, VimMode::Command, entered)) => {
            ("VIM COMMAND", "Vim command mode", Some(entered))
        }
        Some((EditorKeymap::Standard, _, _)) => (
            "STANDARD",
            "Standard editor keymap; click to enable Vim mode",
            None,
        ),
        None => ("-", "No active editor", None),
    };

    div()
        .id("status-bar")
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
                    .on_click(cx.listener(|shell, _, _, cx| {
                        shell.select_left_panel(LeftPanel::Connections, cx)
                    })),
                )
                .child(
                    button(
                        "footer-git",
                        IconName::VersionControl,
                        "Git workspace".into(),
                        shell.left_dock.presentation.open
                            && shell.active_left_panel == LeftPanel::Git,
                        None,
                        false,
                    )
                    .on_click(
                        cx.listener(|shell, _, _, cx| shell.select_left_panel(LeftPanel::Git, cx)),
                    ),
                )
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
                    .on_click(cx.listener(|shell, _, _, cx| {
                        shell.select_left_panel(LeftPanel::Collaboration, cx)
                    })),
                )
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
                    .on_click(cx.listener(|shell, _, _, cx| {
                        shell.select_left_panel(LeftPanel::QueryOutline, cx)
                    })),
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
                    button(
                        "footer-project-search",
                        IconName::Search,
                        "Search project".into(),
                        false,
                        None,
                        false,
                    )
                    .on_click(cx.listener(|shell, _, _, cx| shell.show_project_search(cx))),
                )
                .child(
                    button(
                        "footer-diagnostics",
                        IconName::Warning,
                        format!("Problems ({})", shell.status.diagnostic_count),
                        false,
                        Some(shell.status.diagnostic_count),
                        shell.status.diagnostic_count > 0,
                    )
                    .on_click(cx.listener(|shell, _, _, cx| shell.show_diagnostics(cx))),
                )
                .children(shell.status.current_error.as_ref().map(|error| {
                    button(
                        "footer-current-error",
                        IconName::Copy,
                        format!("Copy current error: {}", compact_error(error)),
                        false,
                        None,
                        true,
                    )
                    .on_click(cx.listener(|shell, _, _, cx| shell.copy_current_error(cx)))
                }))
                .child(separator())
                .child(
                    div()
                        .flex()
                        .min_w_0()
                        .overflow_hidden()
                        .items_center()
                        .gap_1()
                        .child(div().size(px(6.)).flex_none().rounded_full().bg(
                            match &shell.connection_status {
                                ConnectionStatus::Connected { .. } => colors.success,
                                ConnectionStatus::Connecting { .. } => colors.warning,
                                ConnectionStatus::Failed { .. } => colors.danger,
                                ConnectionStatus::Disconnected => colors.muted_text,
                            },
                        ))
                        .child(div().min_w_0().truncate().child(target_label)),
                )
                .child(div().flex_none().child(shell.status.transaction.clone()))
                .child(
                    div()
                        .flex_none()
                        .px_1()
                        .rounded(px(3.))
                        .bg(colors.hovered_surface)
                        .child(shell.status.execution.clone()),
                ),
        )
        .child(separator())
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_1()
                .child({
                    let tooltip = cursor_tooltip.clone();
                    div()
                        .id("footer-cursor-position")
                        .aria_label(cursor_tooltip)
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
                            cx.new(|_| StatusTooltip {
                                message: tooltip.clone(),
                                theme,
                            })
                            .into()
                        })
                })
                .child({
                    let tooltip = mode_tooltip.to_string();
                    div()
                        .id("footer-editor-mode")
                        .role(Role::Button)
                        .aria_label(tooltip.clone())
                        .h(theme.metrics.compact_control_height)
                        .px_1()
                        .flex()
                        .items_center()
                        .text_color(colors.muted_text)
                        .hover(|button| button.bg(colors.hovered_surface).text_color(colors.text))
                        .on_click(
                            cx.listener(|shell, _, _, cx| shell.toggle_active_editor_keymap(cx)),
                        )
                        .child(mode_label)
                        .tooltip(move |_, cx| {
                            cx.new(|_| StatusTooltip {
                                message: tooltip.clone(),
                                theme,
                            })
                            .into()
                        })
                })
                .children(vim_entered.map(|entered| {
                    div()
                        .id("footer-vim-entered")
                        .aria_label(if entered.is_empty() {
                            "No pending Vim keys".to_owned()
                        } else {
                            format!("Pending Vim keys: {entered}")
                        })
                        .h(theme.metrics.compact_control_height)
                        .min_w(px(28.))
                        .px_1()
                        .flex()
                        .items_center()
                        .justify_end()
                        .font_family("monospace")
                        .text_color(colors.accent)
                        .child(entered)
                        .tooltip(move |_, cx| {
                            cx.new(|_| StatusTooltip {
                                message: "Pending Vim key sequence".into(),
                                theme,
                            })
                            .into()
                        })
                }))
                .child(separator())
                .child(
                    button(
                        "footer-console",
                        IconName::Terminal,
                        "Query console".into(),
                        shell.bottom_dock.presentation.open
                            && shell.active_bottom_tool == BottomTool::Console,
                        None,
                        false,
                    )
                    .on_click(cx.listener(|shell, _, _, cx| {
                        shell.select_bottom_tool(BottomTool::Console, cx)
                    })),
                )
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
                .children(shell.right_dock.presentation.open.then(separator))
                .children(shell.right_dock.presentation.open.then(|| {
                    button(
                        "footer-close-inspector",
                        IconName::Close,
                        "Close Inspector".into(),
                        false,
                        None,
                        false,
                    )
                    .on_click(cx.listener(|shell, _, _, cx| shell.close_inspector(cx)))
                })),
        )
        .into_any_element()
}

fn compact_error(error: &str) -> String {
    const MAX_CHARS: usize = 34;
    let mut compact = error.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() > MAX_CHARS {
        compact = compact.chars().take(MAX_CHARS - 1).collect::<String>() + "…";
    }
    compact
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_error_is_single_line_and_unicode_safe() {
        assert_eq!(compact_error("bad\n query"), "bad query");
        let compact =
            compact_error("数据库错误数据库错误数据库错误数据库错误数据库错误数据库错误数据库错误");
        assert!(compact.chars().count() <= 34);
        assert!(compact.ends_with('…'));
    }
}
