//! Host-owned bottom tool panel.

use super::*;

pub(super) fn render_bottom_panel(
    shell: &WorkspaceShell,
    cx: &mut Context<WorkspaceShell>,
) -> gpui::AnyElement {
    let dock = &shell.bottom_dock;
    debug_assert_eq!(dock.id, DockId::Bottom);
    let theme = cx.theme();
    let colors = theme.colors;
    let body = match shell.active_bottom_tool {
        BottomTool::Console => Some(
            "Query editors are Sift consoles. Dedicated scratch-console creation arrives with restored query documents."
                .to_owned(),
        ),
        BottomTool::Monitor => None,
        BottomTool::Automations => Some(shell.selected_workspace().map_or_else(
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
        )),
    };
    div()
        .debug_selector(|| "bottom-dock".into())
        .relative()
        .h(px(dock.presentation.size))
        .flex_none()
        .flex()
        .flex_col()
        .bg(colors.panel)
        .text_sm()
        .text_color(colors.muted_text)
        .child(
            div()
                .flex_none()
                .border_b_1()
                .border_color(colors.subtle_border)
                .pl_3()
                .pr_2()
                .flex()
                .items_center()
                .justify_between()
                .child(SectionLabel::new(
                    shell.active_bottom_tool.label().to_uppercase(),
                )),
        )
        .child(if shell.active_bottom_tool == BottomTool::Monitor {
            let rows = shell.database_processes.iter().map(|process| {
                let process_id = process.process_id;
                div()
                    .id(("database-process", process_id as usize))
                    .flex_none()
                    .min_h(px(30.))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_3()
                    .border_b_1()
                    .border_color(colors.subtle_border)
                    .child(div().w(px(72.)).child(process_id.to_string()))
                    .child(
                        div()
                            .w(px(120.))
                            .truncate()
                            .child(process.user.clone().unwrap_or_else(|| "—".into())),
                    )
                    .child(
                        div()
                            .w(px(100.))
                            .truncate()
                            .child(process.state.clone().unwrap_or_else(|| "—".into())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family("monospace")
                            .child(process.statement.clone().unwrap_or_else(|| "Idle".into())),
                    )
                    .child(
                        Button::new(("terminate-process", process_id as usize), "Terminate")
                            .tone(ButtonTone::DangerGhost)
                            .on_click(cx.listener(move |shell, _, _, cx| {
                                shell.request_terminate_process(process_id, cx)
                            })),
                    )
            });
            div()
                .flex()
                .flex_1()
                .min_h_0()
                .flex_col()
                .child(
                    div()
                        .flex_none()
                        .h(px(30.))
                        .px_3()
                        .flex()
                        .items_center()
                        .gap_3()
                        .text_xs()
                        .text_color(colors.disabled_text)
                        .child(div().w(px(72.)).child("PROCESS"))
                        .child(div().w(px(120.)).child("USER"))
                        .child(div().w(px(100.)).child("STATE"))
                        .child(div().flex_1().child("STATEMENT"))
                        .child(
                            Button::new(
                                "refresh-database-processes",
                                if shell.database_processes_loading {
                                    "Loading…"
                                } else {
                                    "Refresh"
                                },
                            )
                            .tone(ButtonTone::Ghost)
                            .disabled(shell.database_processes_loading)
                            .on_click(
                                cx.listener(|shell, _, _, cx| shell.load_database_processes(cx)),
                            ),
                        ),
                )
                .child(
                    div()
                        .id("database-process-list")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .children(rows),
                )
                .children(
                    shell
                        .database_processes_error
                        .clone()
                        .map(|message| div().p_2().text_color(colors.danger).child(message)),
                )
                .when(
                    shell.database_processes.is_empty()
                        && !shell.database_processes_loading
                        && shell.database_processes_error.is_none(),
                    |panel| {
                        panel.child(
                            div()
                                .p_4()
                                .text_center()
                                .child("No database activity reported."),
                        )
                    },
                )
                .into_any_element()
        } else {
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
                        .child(body.unwrap_or_default()),
                )
                .into_any_element()
        })
        .into_any_element()
}
