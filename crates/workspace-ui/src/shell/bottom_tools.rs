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
            "Use New Query in the footer or press <leader> q n to open a query tab.".to_owned(),
        ),
        BottomTool::Monitor => None,
        BottomTool::Automations => None,
    };
    div()
        .debug_selector(|| "bottom-dock".into())
        .track_focus(&shell.automation_focus_handle)
        .when(
            shell.active_bottom_tool == BottomTool::Automations,
            |dock| {
                dock.key_context("SiftAutomations")
                    .on_key_down(cx.listener(WorkspaceShell::handle_automation_key))
            },
        )
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
                .h(px(30.))
                .border_b_1()
                .border_color(colors.subtle_border)
                .pl_3()
                .pr_2()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(SectionLabel::new(
                            shell.active_bottom_tool.label().to_uppercase(),
                        ))
                        .children((shell.active_bottom_tool == BottomTool::Automations).then(
                            || {
                                div()
                                    .text_xs()
                                    .text_color(colors.disabled_text)
                                    .child(shell.automation_configurations.len().to_string())
                            },
                        )),
                )
                .children(
                    (shell.active_bottom_tool == BottomTool::Automations).then(|| {
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                Button::new("new-automation", "New")
                                    .debug_selector("new-automation")
                                    .tone(ButtonTone::Ghost)
                                    .start_icon(IconName::Add)
                                    .disabled(shell.selected_workspace_id.is_none())
                                    .on_click(cx.listener(|shell, _, window, cx| {
                                        shell.open_run_configuration_editor(None, window, cx)
                                    })),
                            )
                            .child(
                                div().debug_selector(|| "refresh-automations".into()).child(
                                    IconButton::new(
                                        "refresh-automations",
                                        IconName::Refresh,
                                        "Refresh automations",
                                    )
                                    .square(px(24.))
                                    .icon_size(12.)
                                    .tooltip("Refresh automations · Shift+R")
                                    .disabled(shell.automations_loading)
                                    .on_click(
                                        cx.listener(|shell, _, _, cx| {
                                            shell.request_automations(cx)
                                        }),
                                    ),
                                ),
                            )
                    }),
                ),
        )
        .child(if shell.active_bottom_tool == BottomTool::Monitor {
            let transaction = shell.transaction_state.transaction().map(|transaction| {
                let savepoints =
                    shell
                        .savepoints
                        .iter()
                        .rev()
                        .cloned()
                        .enumerate()
                        .map(|(index, name)| {
                            let selector_name = name.clone();
                            let rollback_name = name.clone();
                            let release_name = name.clone();
                            div()
                                .debug_selector(move || {
                                    format!("transaction-savepoint-{selector_name}")
                                })
                                .h(px(28.))
                                .flex_none()
                                .px_3()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(div().flex_1().font_family("monospace").child(name))
                                .child(
                                    Button::new(("rollback-savepoint", index), "Rollback to")
                                        .debug_selector(format!(
                                            "rollback-savepoint-{rollback_name}"
                                        ))
                                        .tone(ButtonTone::Neutral)
                                        .disabled(shell.transaction_state.is_pending())
                                        .on_click(cx.listener(move |shell, _, _, cx| {
                                            shell.rollback_to_savepoint(rollback_name.clone(), cx)
                                        })),
                                )
                                .child(
                                    Button::new(("release-savepoint", index), "Release")
                                        .debug_selector(format!("release-savepoint-{release_name}"))
                                        .tone(ButtonTone::Ghost)
                                        .disabled(shell.transaction_state.is_pending())
                                        .on_click(cx.listener(move |shell, _, _, cx| {
                                            shell.release_savepoint(release_name.clone(), cx)
                                        })),
                                )
                        });
                let mode = format!(
                    "{:?} · {:?}",
                    transaction.mode.isolation, transaction.mode.access
                );
                div()
                    .debug_selector(|| "transaction-monitor".into())
                    .flex_none()
                    .border_b_1()
                    .border_color(colors.subtle_border)
                    .child(
                        div()
                            .h(px(30.))
                            .px_3()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(SectionLabel::new(format!(
                                "TRANSACTION {}",
                                transaction.tx_id
                            )))
                            .child(div().text_xs().child(mode))
                            .child(div().flex_1())
                            .child(
                                Button::new("monitor-create-savepoint", "New savepoint")
                                    .tone(ButtonTone::Neutral)
                                    .disabled(
                                        shell.transaction_state.is_pending()
                                            || shell.transaction_state.is_aborted(),
                                    )
                                    .on_click(
                                        cx.listener(|shell, _, _, cx| shell.create_savepoint(cx)),
                                    ),
                            ),
                    )
                    .when(shell.savepoints.is_empty(), |panel| {
                        panel.child(div().px_3().pb_2().text_xs().child("No savepoints"))
                    })
                    .children(savepoints)
            });
            let processes = Arc::new(database_process_rows(shell.database_monitor.processes()));
            let process_count = processes.len();
            let process_details = shell.database_monitor.selected().and_then(|process_id| {
                shell
                    .database_monitor
                    .processes()
                    .iter()
                    .find(|process| process.process_id == process_id)
                    .cloned()
            });
            div()
                .flex()
                .flex_1()
                .min_h_0()
                .flex_col()
                .children(transaction)
                .children(
                    process_details.map(|process| render_database_process_details(process, cx)),
                )
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
                        .child(div().w(px(180.)).child("USER / DATABASE"))
                        .child(div().w(px(220.)).child("STATE / WAIT"))
                        .child(
                            div()
                                .debug_selector(|| "database-process-statement-header".into())
                                .flex_1()
                                .min_w_0()
                                .text_right()
                                .child("STATEMENT"),
                        )
                        .child(
                            div().w(px(84.)).flex().justify_end().child(
                                Button::new(
                                    "refresh-database-processes",
                                    if shell.database_monitor.request().loading() {
                                        "Loading…"
                                    } else {
                                        "Refresh"
                                    },
                                )
                                .tone(ButtonTone::Ghost)
                                .disabled(shell.database_monitor.request().loading())
                                .on_click(
                                    cx.listener(|shell, _, _, cx| {
                                        shell.load_database_processes(cx)
                                    }),
                                ),
                            ),
                        ),
                )
                .child(
                    div().id("database-process-list").flex_1().min_h_0().child(
                        uniform_list(
                            "database-process-rows",
                            process_count,
                            cx.processor(move |_, range: Range<usize>, _, cx| {
                                range
                                    .filter_map(|index| processes.get(index).cloned())
                                    .map(|process| render_database_process_row(process, cx))
                                    .collect()
                            }),
                        )
                        .size_full(),
                    ),
                )
                .children(shell.database_monitor.request().error().map(|message| {
                    div()
                        .p_2()
                        .text_color(colors.danger)
                        .child(message.to_string())
                }))
                .when(
                    shell.database_monitor.processes().is_empty()
                        && !shell.database_monitor.request().loading()
                        && shell.database_monitor.request().error().is_none(),
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
        } else if shell.active_bottom_tool == BottomTool::Automations {
            let rows =
                shell
                    .automation_configurations
                    .iter()
                    .enumerate()
                    .map(|(index, configuration)| {
                        let edit_configuration = configuration.clone();
                        let run = shell.automation_runs.get(&configuration.id);
                        let running = run.is_some_and(automation_run_is_active);
                        let requires_variables = configuration
                            .variables
                            .iter()
                            .any(|variable| variable.required);
                        let selected = index == shell.automation_selected;
                        let (status, status_color, status_background) =
                            match run.map(|run| run.state) {
                                None => ("Never run", colors.disabled_text, colors.panel),
                                Some(sift_protocol::RunState::Queued) => {
                                    ("Queued", colors.warning, colors.warning_muted)
                                }
                                Some(sift_protocol::RunState::Admitted) => {
                                    ("Admitted", colors.warning, colors.warning_muted)
                                }
                                Some(sift_protocol::RunState::Preparing) => {
                                    ("Preparing", colors.warning, colors.warning_muted)
                                }
                                Some(sift_protocol::RunState::Running) => {
                                    ("Running", colors.accent_hover, colors.accent_muted)
                                }
                                Some(sift_protocol::RunState::Succeeded) => {
                                    ("Succeeded", colors.success, colors.success_muted)
                                }
                                Some(sift_protocol::RunState::Failed) => {
                                    ("Failed", colors.danger, colors.danger_muted)
                                }
                                Some(sift_protocol::RunState::Cancelled) => {
                                    ("Cancelled", colors.muted_text, colors.panel)
                                }
                                Some(sift_protocol::RunState::OutcomeUnknown) => {
                                    ("Unknown", colors.danger, colors.danger_muted)
                                }
                                Some(sift_protocol::RunState::Blocked) => {
                                    ("Blocked", colors.warning, colors.warning_muted)
                                }
                                Some(sift_protocol::RunState::Rejected) => {
                                    ("Rejected", colors.danger, colors.danger_muted)
                                }
                            };
                        div()
                            .id(("automation-configuration", index))
                            .debug_selector(move || format!("automation-configuration-{index}"))
                            .role(Role::Button)
                            .h(px(36.))
                            .px_3()
                            .flex()
                            .items_center()
                            .gap_3()
                            .border_b_1()
                            .border_color(colors.subtle_border)
                            .hover(|row| row.bg(colors.hovered_surface))
                            .when(selected, |row| row.bg(colors.active_surface))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(
                                    move |shell, event: &gpui::MouseDownEvent, window, cx| {
                                        shell.automation_selected = index;
                                        shell.automation_focus_handle.focus(window, cx);
                                        if event.click_count >= 2 {
                                            shell.open_run_configuration_editor(
                                                Some(edit_configuration.clone()),
                                                window,
                                                cx,
                                            );
                                        }
                                        cx.stop_propagation();
                                        cx.notify();
                                    },
                                ),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(colors.text)
                                    .child(configuration.name.clone()),
                            )
                            .child(
                                div()
                                    .w(px(72.))
                                    .text_xs()
                                    .text_color(colors.muted_text)
                                    .child(format!("{}", configuration.scripts.len())),
                            )
                            .child(
                                div().w(px(104.)).flex().items_center().child(
                                    div()
                                        .px_2()
                                        .py(px(2.))
                                        .rounded_sm()
                                        .bg(status_background)
                                        .text_xs()
                                        .text_color(status_color)
                                        .child(status),
                                ),
                            )
                            .child(
                                div()
                                    .w(px(224.))
                                    .flex()
                                    .items_center()
                                    .justify_end()
                                    .gap_1()
                                    .text_xs()
                                    .text_color(colors.disabled_text)
                                    .when(selected, |hints| {
                                        hints
                                            .child(KeyBinding::new("Enter"))
                                            .child("Edit")
                                            .when(running, |hints| {
                                                hints.child(KeyBinding::new("c")).child("Cancel")
                                            })
                                            .when(!running && !requires_variables, |hints| {
                                                hints.child(KeyBinding::new("r")).child("Run")
                                            })
                                            .when(requires_variables, |hints| {
                                                hints.child("Values required")
                                            })
                                    }),
                            )
                    });
            div()
                .flex()
                .flex_1()
                .min_h_0()
                .flex_col()
                .child(
                    div()
                        .h(px(26.))
                        .px_3()
                        .flex()
                        .items_center()
                        .gap_3()
                        .border_b_1()
                        .border_color(colors.subtle_border)
                        .text_xs()
                        .text_color(colors.disabled_text)
                        .child(div().flex_1().min_w_0().child("CONFIGURATION"))
                        .child(div().w(px(72.)).child("STEPS"))
                        .child(div().w(px(104.)).child("STATUS"))
                        .child(
                            div()
                                .w(px(224.))
                                .text_right()
                                .child("j/k SELECT · DOUBLE-CLICK EDIT"),
                        ),
                )
                .children(
                    shell.automations_error.as_ref().map(|message| {
                        div().mx_3().mb_2().child(ErrorBanner::new(message.clone()))
                    }),
                )
                .when(
                    shell.automation_configurations.is_empty()
                        && shell.automations_error.is_none()
                        && !shell.automations_loading,
                    |panel| {
                        panel.child(
                            div()
                                .p_4()
                                .text_center()
                                .whitespace_normal()
                                .child("No automations yet. Press n or choose New to create one."),
                        )
                    },
                )
                .child(
                    div()
                        .id("automation-configuration-list")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .children(rows),
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

#[derive(Clone)]
struct DatabaseProcessRow {
    process: sift_protocol::DatabaseProcess,
    block_depth: usize,
    cycle: bool,
}

fn database_process_rows(processes: &[sift_protocol::DatabaseProcess]) -> Vec<DatabaseProcessRow> {
    fn depth(
        process_id: i64,
        blockers: &HashMap<i64, Vec<i64>>,
        visiting: &mut HashSet<i64>,
        memo: &mut HashMap<i64, (usize, bool)>,
    ) -> (usize, bool) {
        if let Some(result) = memo.get(&process_id) {
            return *result;
        }
        if !visiting.insert(process_id) {
            return (0, true);
        }
        let mut result = (0, false);
        for blocker in blockers.get(&process_id).into_iter().flatten() {
            let (blocker_depth, cycle) = if blockers.contains_key(blocker) {
                depth(*blocker, blockers, visiting, memo)
            } else {
                (0, false)
            };
            result.0 = result.0.max(blocker_depth.saturating_add(1));
            result.1 |= cycle;
        }
        visiting.remove(&process_id);
        memo.insert(process_id, result);
        result
    }

    let blockers = processes
        .iter()
        .map(|process| (process.process_id, process.blocked_by.clone()))
        .collect::<HashMap<_, _>>();
    let mut memo = HashMap::new();
    processes
        .iter()
        .cloned()
        .map(|process| {
            let (block_depth, cycle) = depth(
                process.process_id,
                &blockers,
                &mut HashSet::new(),
                &mut memo,
            );
            DatabaseProcessRow {
                process,
                block_depth,
                cycle,
            }
        })
        .collect()
}

fn render_database_process_row(
    row: DatabaseProcessRow,
    cx: &mut Context<WorkspaceShell>,
) -> gpui::AnyElement {
    let colors = cx.theme().colors;
    let process = row.process;
    let process_id = process.process_id;
    let user_database = match (process.user, process.database) {
        (Some(user), Some(database)) => format!("{user} @ {database}"),
        (Some(user), None) => user,
        (None, Some(database)) => database,
        (None, None) => "—".into(),
    };
    let mut state = process.state.unwrap_or_else(|| "—".into());
    if let Some(wait) = process.wait {
        state.push_str(" · ");
        state.push_str(&wait);
    }
    if !process.blocked_by.is_empty() {
        state.push_str(" · blocked by ");
        state.push_str(
            &process
                .blocked_by
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    if row.cycle {
        state.push_str(" · blocking cycle");
    }
    let statement = process.statement.unwrap_or_else(|| "Idle".into());

    div()
        .id(("database-process", process_id as usize))
        .debug_selector(move || format!("database-process-{process_id}"))
        .flex_none()
        .w_full()
        .min_h(px(30.))
        .px_3()
        .flex()
        .items_center()
        .gap_3()
        .border_b_1()
        .border_color(colors.subtle_border)
        .when(row.block_depth > 0, |row| row.bg(colors.warning_muted))
        .child(
            div()
                .w(px(72.))
                .pl(px((row.block_depth.min(4) * 8) as f32))
                .child(if row.block_depth > 0 {
                    format!("↳ {process_id}")
                } else {
                    process_id.to_string()
                }),
        )
        .child(div().w(px(180.)).truncate().child(user_database))
        .child(div().w(px(220.)).truncate().child(state))
        .child(
            div()
                .id(("database-process-statement", process_id as usize))
                .debug_selector(move || format!("database-process-statement-{process_id}"))
                .flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .items_center()
                .justify_end()
                .truncate()
                .text_right()
                .font_family("monospace")
                .cursor_pointer()
                .on_click(
                    cx.listener(move |shell, _, _, cx| {
                        shell.select_database_process(process_id, cx)
                    }),
                )
                .child(statement),
        )
        .child(
            div().w(px(84.)).flex().justify_end().child(
                Button::new(("terminate-process", process_id as usize), "Terminate")
                    .tone(ButtonTone::DangerGhost)
                    .on_click(cx.listener(move |shell, _, _, cx| {
                        shell.request_terminate_process(process_id, cx)
                    })),
            ),
        )
        .into_any_element()
}

fn render_database_process_details(
    process: sift_protocol::DatabaseProcess,
    cx: &mut Context<WorkspaceShell>,
) -> gpui::AnyElement {
    let colors = cx.theme().colors;
    let process_id = process.process_id;
    let started = process
        .started_at
        .map(|started| started.to_rfc3339())
        .unwrap_or_else(|| "Unknown".into());
    let elapsed = process.started_at.map(|started| {
        let elapsed_ms = epoch_millis().saturating_sub(started.timestamp_millis().max(0) as u64);
        format!("{}.{:01}s", elapsed_ms / 1_000, (elapsed_ms % 1_000) / 100)
    });
    let blockers = if process.blocked_by.is_empty() {
        "None".into()
    } else {
        process
            .blocked_by
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let statement = process.statement.unwrap_or_else(|| "Idle".into());
    let metadata = format!(
        "{:?} · {} @ {} · {} · wait: {} · blocked by: {} · started: {}{}",
        process.engine,
        process.user.unwrap_or_else(|| "unknown user".into()),
        process
            .database
            .unwrap_or_else(|| "unknown database".into()),
        process.state.unwrap_or_else(|| "unknown state".into()),
        process.wait.unwrap_or_else(|| "none".into()),
        blockers,
        started,
        elapsed.map_or_else(String::new, |elapsed| format!(" · elapsed: {elapsed}")),
    );

    div()
        .debug_selector(move || format!("database-process-details-{process_id}"))
        .flex_none()
        .border_b_1()
        .border_color(colors.subtle_border)
        .bg(colors.elevated_surface)
        .p_3()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(SectionLabel::new(format!("PROCESS {process_id}")))
                .child(div().flex_1().truncate().text_xs().child(metadata))
                .child(
                    Button::new(("copy-process-statement", process_id as usize), "Copy")
                        .debug_selector(format!("copy-process-statement-{process_id}"))
                        .tone(ButtonTone::Ghost)
                        .on_click(cx.listener(move |shell, _, _, cx| {
                            shell.copy_database_process_statement(process_id, cx)
                        })),
                )
                .child(
                    Button::new("close-process-details", "Close")
                        .tone(ButtonTone::Ghost)
                        .on_click(
                            cx.listener(|shell, _, _, cx| shell.close_database_process_details(cx)),
                        ),
                ),
        )
        .child(
            div()
                .id(("database-process-sql", process_id as usize))
                .max_h(px(96.))
                .overflow_y_scroll()
                .font_family("monospace")
                .text_color(colors.text)
                .whitespace_normal()
                .child(statement),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(process_id: i64, blocked_by: Vec<i64>) -> sift_protocol::DatabaseProcess {
        sift_protocol::DatabaseProcess {
            engine: sift_protocol::Engine::Postgres,
            process_id,
            user: None,
            database: None,
            state: None,
            statement: None,
            started_at: None,
            wait: None,
            blocked_by,
        }
    }

    #[test]
    fn blocking_chains_compute_depth_and_detect_cycles() {
        let rows =
            database_process_rows(&[process(1, vec![]), process(2, vec![1]), process(3, vec![2])]);
        assert_eq!(
            rows.iter().map(|row| row.block_depth).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert!(rows.iter().all(|row| !row.cycle));

        let cycle = database_process_rows(&[process(4, vec![5]), process(5, vec![4])]);
        assert!(cycle.iter().all(|row| row.cycle));
    }
}
