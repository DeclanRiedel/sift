//! Host-owned bottom tool panel.

use super::*;

pub(super) fn render_bottom_panel(shell: &WorkspaceShell) -> gpui::AnyElement {
    let dock = &shell.bottom_dock;
    debug_assert_eq!(dock.id, DockId::Bottom);
    let colors = shell.theme.colors;
    let body = match shell.active_bottom_tool {
        BottomTool::Console => {
            "Query editors are Sift consoles. Dedicated scratch-console creation arrives with restored query documents."
                .to_owned()
        }
        BottomTool::Monitor => format!(
            "Connection: {} · Transaction: {} · Execution: {}",
            shell.status.database, shell.status.transaction, shell.status.execution
        ),
        BottomTool::Automations => shell.selected_workspace().map_or_else(
            || "Select a workspace to inspect runs, schedules, and transfer recipes.".into(),
            |workspace| {
                if workspace.scheduling_enabled {
                    format!(
                        "Automations enabled for {}. Run and schedule rendering arrives with desktop automation integration.",
                        workspace.name
                    )
                } else {
                    format!("Scheduling is disabled for {}.", workspace.name)
                }
            },
        ),
    };
    div()
        .debug_selector(|| "bottom-dock".into())
        .relative()
        .h(px(dock.presentation.size))
        .flex_none()
        .flex()
        .flex_col()
        .border_t_1()
        .border_color(colors.subtle_border)
        .bg(colors.panel)
        .text_sm()
        .text_color(colors.muted_text)
        .child(
            div()
                .h(px(28.))
                .flex_none()
                .flex()
                .items_center()
                .px_3()
                .border_b_1()
                .border_color(colors.subtle_border)
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(shell.active_bottom_tool.label().to_uppercase()),
        )
        .child(
            div()
                .flex()
                .flex_1()
                .min_h_0()
                .items_center()
                .justify_center()
                .px_4()
                .child(
                    div()
                        .max_w(px(420.))
                        .min_w_0()
                        .text_center()
                        .whitespace_normal()
                        .child(body),
                ),
        )
        .child(dock_resize_handle(DockId::Bottom, colors.accent))
        .into_any_element()
}
