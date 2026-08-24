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
    let key_buffer = if shell.key_buffer.is_empty() {
        vim_entered.unwrap_or_default()
    } else {
        shell.key_buffer.clone()
    };
    let key_buffer_label = if key_buffer.is_empty() {
        "—".to_owned()
    } else {
        key_buffer.clone()
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
                        "footer-problems",
                        IconName::Warning,
                        format!("Open problems ({})", shell.global_problems.len()),
                        false,
                        Some(shell.global_problems.len()),
                        !shell.global_problems.is_empty(),
                    )
                    .on_click(
                        cx.listener(|shell, _, window, cx| shell.show_global_problems(window, cx)),
                    ),
                )
                .child(
                    button(
                        "footer-copy-problems",
                        IconName::Copy,
                        "Copy problems list".into(),
                        false,
                        None,
                        false,
                    )
                    .disabled(shell.global_problems.is_empty())
                    .on_click(cx.listener(|shell, _, _, cx| shell.copy_all_global_problems(cx))),
                )
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
                .child(div().flex_none().child(shell.status.transaction.clone())),
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
                        .role(Role::Button)
                        .aria_label(mode_tooltip)
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
                        .tooltip(move |_, cx| cx.new(|_| Tooltip::new(mode_tooltip)).into())
                })
                .child({
                    div()
                        .id("footer-key-buffer")
                        .aria_label(if key_buffer.is_empty() {
                            "Key buffer is empty".to_owned()
                        } else {
                            format!("Key buffer: {key_buffer}")
                        })
                        .h(theme.metrics.compact_control_height)
                        .min_w(px(64.))
                        .px_1()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(div().text_color(colors.muted_text).child("BUFFER"))
                        .child(
                            div()
                                .font_family("monospace")
                                .text_color(colors.accent)
                                .child(key_buffer_label),
                        )
                        .tooltip({
                            let message: SharedString =
                                "Pending IDE chord or Vim key sequence".into();
                            move |_, cx| cx.new(|_| Tooltip::new(message.clone())).into()
                        })
                })
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
