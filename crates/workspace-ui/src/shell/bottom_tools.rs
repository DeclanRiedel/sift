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
        BottomTool::Monitor => Some(format!(
            "Connection: {} · Transaction: {} · Execution: {}",
            shell.status.database, shell.status.transaction, shell.status.execution
        )),
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
        BottomTool::Problems => None,
    };
    let problems = shell.visible_problems();
    let has_problems = !problems.is_empty();
    let problem_rows = problems
        .into_iter()
        .enumerate()
        .map(|(index, problem)| {
            let severity_color = match problem.severity {
                SqlProblemSeverity::Error => colors.danger,
                SqlProblemSeverity::Warning => colors.warning,
            };
            div()
                .id(("problem-row", index))
                .debug_selector(move || format!("problem-row-{index}"))
                .flex()
                .items_start()
                .gap_2()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(colors.subtle_border)
                .child(icon(IconName::Warning, severity_color, 14.))
                .child(
                    div()
                        .flex()
                        .flex_1()
                        .min_w_0()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .text_color(severity_color)
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child(problem.severity.label()),
                                )
                                .child(div().truncate().child(problem.title)),
                        )
                        .child(
                            div()
                                .whitespace_normal()
                                .text_color(colors.text)
                                .child(problem.message),
                        ),
                )
                .child(
                    IconButton::new(("copy-problem", index), IconName::Copy, "Copy problem")
                        .square(px(24.))
                        .icon_size(13.)
                        .tooltip("Copy problem")
                        .on_click(
                            cx.listener(move |shell, _, _, cx| shell.copy_problem(index, cx)),
                        ),
                )
                .into_any_element()
        })
        .collect::<Vec<_>>();
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
                ))
                .children((shell.active_bottom_tool == BottomTool::Problems).then(|| {
                    Button::new("copy-all-problems", "Copy all")
                        .debug_selector("copy-all-problems")
                        .tone(ButtonTone::Ghost)
                        .start_icon(IconName::Copy)
                        .disabled(!has_problems)
                        .on_click(cx.listener(|shell, _, _, cx| shell.copy_all_problems(cx)))
                })),
        )
        .child(if shell.active_bottom_tool == BottomTool::Problems {
            if has_problems {
                div()
                    .id("problems-scroll")
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .flex_col()
                    .overflow_y_scroll()
                    .children(problem_rows)
                    .into_any_element()
            } else {
                div()
                    .id("problems-empty")
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .items_center()
                    .justify_center()
                    .text_color(colors.muted_text)
                    .child("No SQL errors or warnings")
                    .into_any_element()
            }
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
