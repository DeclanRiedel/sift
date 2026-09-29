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
        BottomTool::Console => Some("Press <leader> q n to open a query tab.".to_owned()),
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
        .when(
            shell.active_bottom_tool == BottomTool::Monitor
                && shell.database_monitor.view() == DatabaseMonitorView::Settings,
            |dock| {
                dock.key_context("SiftPostgresSettings")
                    .on_key_down(cx.listener(WorkspaceShell::handle_postgres_settings_key))
            },
        )
        .when(
            shell.active_bottom_tool == BottomTool::Monitor
                && matches!(shell.database_monitor.view(), DatabaseMonitorView::Activity | DatabaseMonitorView::Locks | DatabaseMonitorView::Deadlocks | DatabaseMonitorView::Alerts)
                && shell.active_connection_provider_id() == Some(&sift_protocol::Engine::SqlServer.provider_id()),
            |dock| dock.key_context("SiftSqlServerProcesses")
                .on_key_down(cx.listener(WorkspaceShell::handle_sqlserver_process_key)),
        )
        .when(
            shell.active_bottom_tool == BottomTool::Monitor
                && matches!(shell.database_monitor.view(), DatabaseMonitorView::Extensions | DatabaseMonitorView::Partitions | DatabaseMonitorView::Policies | DatabaseMonitorView::Roles | DatabaseMonitorView::Ownership | DatabaseMonitorView::SchemaGrants),
            |dock| dock.key_context("SiftPostgresObjects")
                .on_key_down(cx.listener(WorkspaceShell::handle_postgres_objects_key)),
        )
        .when(
            shell.active_bottom_tool == BottomTool::Monitor
                && matches!(shell.database_monitor.view(), DatabaseMonitorView::Replication | DatabaseMonitorView::Statistics),
            |dock| dock.key_context("SiftPostgresDiagnostics")
                .on_key_down(cx.listener(WorkspaceShell::handle_postgres_diagnostics_key)),
        )
        .when(
            shell.active_bottom_tool == BottomTool::Monitor
                && shell.database_monitor.view() == DatabaseMonitorView::AgentJobs,
            |dock| {
                dock.key_context("SiftAgentJobs")
                    .on_key_down(cx.listener(WorkspaceShell::handle_agent_jobs_key))
            },
        )
        .when(
            shell.active_bottom_tool == BottomTool::Monitor
                && shell.database_monitor.view() == DatabaseMonitorView::SqlServerSettings,
            |dock| {
                dock.key_context("SiftSqlServerSettings")
                    .on_key_down(cx.listener(WorkspaceShell::handle_sqlserver_settings_key))
            },
        )
        .when(
            shell.active_bottom_tool == BottomTool::Monitor
                && shell.database_monitor.view() == DatabaseMonitorView::Security,
            |dock| dock.key_context("SiftSqlServerSecurity")
                .on_key_down(cx.listener(WorkspaceShell::handle_sqlserver_security_key)),
        )
        .when(
            shell.active_bottom_tool == BottomTool::Monitor
                && shell.database_monitor.view() == DatabaseMonitorView::Maintenance
                && shell.active_connection_provider_id().is_some_and(|id| id.as_str() == "sift/sql-server"),
            |dock| dock.key_context("SiftSqlServerMaintenance")
                .on_key_down(cx.listener(WorkspaceShell::handle_sqlserver_maintenance_key)),
        )
        .when(
            shell.active_bottom_tool == BottomTool::Monitor
                && shell.database_monitor.view() == DatabaseMonitorView::Maintenance
                && shell.active_connection_provider_id().is_some_and(|id| id.as_str() == "sift/postgres"),
            |dock| dock.key_context("SiftPostgresMaintenance")
                .on_key_down(cx.listener(WorkspaceShell::handle_pg_maintenance_key)),
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
                                Button::new("open-transfer-recipes", "Transfers")
                                    .debug_selector("open-transfer-recipes")
                                    .tone(ButtonTone::Ghost)
                                    .disabled(shell.selected_workspace_id.is_none())
                                    .on_click(cx.listener(|shell, _, window, cx| {
                                        shell.open_transfer_recipes(window, cx)
                                    })),
                            )
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
                )
                .children((shell.active_bottom_tool == BottomTool::Monitor).then(|| {
                    let selected = shell.database_monitor.view();
                    let provider = shell.active_connection_provider_id().map(|id| id.as_str());
                    let connected =
                        matches!(shell.connection_status, ConnectionStatus::Connected { .. });
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .children(DatabaseMonitorView::ALL.into_iter().map(|view| {
                            Button::new(view.button_id(), view.label(&shell.database_monitor))
                                .tone(if selected == view {
                                    ButtonTone::Neutral
                                } else {
                                    ButtonTone::Ghost
                                })
                                .disabled(!view.available_for(provider, connected))
                                .on_click(cx.listener(move |shell, _, window, cx| {
                                    shell.set_database_monitor_view(view, cx);
                                    shell.automation_focus_handle.focus(window, cx);
                                }))
                        }))
                })),
        )
        .child(if shell.active_bottom_tool == BottomTool::Monitor {
            if shell.database_monitor.view() == DatabaseMonitorView::Overview {
                render_server_dashboard(shell, cx)
            } else if shell.database_monitor.view() == DatabaseMonitorView::History {
                render_database_deadlock_history(shell, cx)
            } else if shell.database_monitor.view() == DatabaseMonitorView::Settings {
                render_postgres_settings(shell, cx).into_any_element()
            } else if matches!(shell.database_monitor.view(), DatabaseMonitorView::Extensions | DatabaseMonitorView::Partitions | DatabaseMonitorView::Policies | DatabaseMonitorView::Roles | DatabaseMonitorView::Ownership | DatabaseMonitorView::SchemaGrants) {
                render_postgres_objects(shell, cx).into_any_element()
            } else if shell.database_monitor.view() == DatabaseMonitorView::Replication {
                render_postgres_replication(shell, cx).into_any_element()
            } else if shell.database_monitor.view() == DatabaseMonitorView::Statistics {
                render_postgres_statistics(shell, cx).into_any_element()
            } else if shell.database_monitor.view() == DatabaseMonitorView::QueryStore {
                render_query_store(shell, cx)
            } else if shell.database_monitor.view() == DatabaseMonitorView::AgentJobs {
                render_agent_jobs(shell, cx)
            } else if shell.database_monitor.view() == DatabaseMonitorView::SqlServerSettings {
                render_sqlserver_settings(shell, cx)
            } else if shell.database_monitor.view() == DatabaseMonitorView::Security {
                render_sqlserver_security(shell, cx)
            } else if shell.database_monitor.view() == DatabaseMonitorView::Maintenance {
                if shell.active_connection_provider_id().is_some_and(|id| id.as_str() == "sift/postgres") {
                    render_postgres_maintenance(shell, cx)
                } else {
                    render_sqlserver_maintenance(shell, cx)
                }
            } else {
                let transaction =
                    shell.transaction_state.transaction().map(|transaction| {
                        let savepoints = shell.savepoints.iter().rev().cloned().enumerate().map(
                            |(index, name)| {
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
                                                shell.rollback_to_savepoint(
                                                    rollback_name.clone(),
                                                    cx,
                                                )
                                            })),
                                    )
                                    .child(
                                        Button::new(("release-savepoint", index), "Release")
                                            .debug_selector(format!(
                                                "release-savepoint-{release_name}"
                                            ))
                                            .tone(ButtonTone::Ghost)
                                            .disabled(shell.transaction_state.is_pending())
                                            .on_click(cx.listener(move |shell, _, _, cx| {
                                                shell.release_savepoint(release_name.clone(), cx)
                                            })),
                                    )
                            },
                        );
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
                                            .on_click(cx.listener(|shell, _, _, cx| {
                                                shell.create_savepoint(cx)
                                            })),
                                    ),
                            )
                            .when(shell.savepoints.is_empty(), |panel| {
                                panel.child(div().px_3().pb_2().text_xs().child("No savepoints"))
                            })
                            .children(savepoints)
                    });
                let visible_processes = shell.database_monitor.visible_processes();
                let processes = database_process_rows(&visible_processes);
                let selected_process = shell.database_monitor.selected();
                let process_cursor = shell.database_monitor.process_cursor();
                let kill_reason = shell.operation_unavailable_reason(sift_protocol::OperationKind::KillProcess);
                let sqlserver_processes = shell.active_connection_provider_id() == Some(&sift_protocol::Engine::SqlServer.provider_id());
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .flex_col()
                    .children(transaction)
                    .children(sqlserver_processes.then(|| div().px_3().text_xs().child("SQL Server: j/k choose session · Enter details · d review termination · r refresh. KILL requires ALTER ANY CONNECTION and can roll back work; use editor Cancel for your own query.")))
                    .children(kill_reason.clone().map(|reason| div().px_3().text_xs().text_color(colors.warning).child(format!("Termination unavailable: {reason}"))))
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
                            .child(div().flex_1().min_w_0().child("USER / DATABASE"))
                            .child(div().flex_1().min_w_0().child("STATE / WAIT"))
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
                                    .on_click(cx.listener(
                                        |shell, _, _, cx| shell.load_database_processes(cx),
                                    )),
                                ),
                            ),
                    )
                    .child(
                        div()
                            .id("database-process-list")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .children(processes.iter().cloned().map(|process| {
                                let expanded = selected_process == Some(process.process.process_id);
                                let alert =
                                    shell.database_monitor.alert(process.process.process_id);
                                let focused = process_cursor == Some(process.process.process_id);
                                render_database_process_row(process, expanded, focused, kill_reason.is_none(), alert, cx)
                            })),
                    )
                    .children(shell.database_monitor.request().error().map(|message| {
                        div()
                            .p_2()
                            .text_color(colors.danger)
                            .child(message.to_string())
                    }))
                    .when(
                        visible_processes.is_empty()
                            && !shell.database_monitor.request().loading()
                            && shell.database_monitor.request().error().is_none(),
                        |panel| {
                            panel.child(div().p_4().text_center().child(
                                match shell.database_monitor.view() {
                                    DatabaseMonitorView::Locks => {
                                        "No waiting or blocking sessions."
                                    }
                                    DatabaseMonitorView::Deadlocks => "No live blocking cycles observed. Resolved deadlocks require server logs.",
                                    DatabaseMonitorView::History => unreachable!(),
                                    DatabaseMonitorView::Alerts => "No database health alerts.",
                                    DatabaseMonitorView::Activity => {
                                        "No database activity reported."
                                    }
                                    DatabaseMonitorView::Overview => unreachable!(),
                                    DatabaseMonitorView::Settings => unreachable!(),
                                    DatabaseMonitorView::Extensions | DatabaseMonitorView::Partitions | DatabaseMonitorView::Policies | DatabaseMonitorView::Roles | DatabaseMonitorView::Ownership | DatabaseMonitorView::SchemaGrants => unreachable!(),
                                    DatabaseMonitorView::Replication | DatabaseMonitorView::Statistics => unreachable!(),
                                    DatabaseMonitorView::QueryStore => unreachable!(),
                                    DatabaseMonitorView::AgentJobs => unreachable!(),
                                    DatabaseMonitorView::SqlServerSettings => unreachable!(),
                                    DatabaseMonitorView::Security => unreachable!(),
                                    DatabaseMonitorView::Maintenance => unreachable!(),
                                },
                            ))
                        },
                    )
                    .into_any_element()
            }
        } else if shell.active_bottom_tool == BottomTool::Automations {
            let rows =
                shell
                    .automation_configurations
                    .iter()
                    .enumerate()
                    .map(|(index, configuration)| {
                        let edit_configuration = configuration.clone();
                        let run = shell.automation_runs.get(&configuration.id);
                        let selected = index == shell.automation_selected;
                        let expanded = shell.automation_expanded == Some(configuration.id);
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
                            .min_h(px(36.))
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
                                        shell.select_automation(index, cx);
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
                                    .w(px(56.))
                                    .flex_none()
                                    .text_xs()
                                    .text_color(colors.muted_text)
                                    .child(format!("{}", configuration.scripts.len())),
                            )
                            .child(
                                div().w(px(96.)).flex_none().flex().items_center().child(
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
                                    .w(px(18.))
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .justify_end()
                                    .child(icon(
                                        if expanded {
                                            IconName::ChevronDown
                                        } else {
                                            IconName::ChevronRight
                                        },
                                        if selected {
                                            colors.text
                                        } else {
                                            colors.disabled_text
                                        },
                                        10.,
                                    )),
                            )
                    });
            let schedules =
                shell
                    .automation_schedules
                    .iter()
                    .cloned()
                    .enumerate()
                    .map(|(index, schedule)| {
                        let edit_schedule = schedule.clone();
                        let toggle_schedule = schedule.clone();
                        let delete_schedule = schedule.clone();
                        let next_fire = schedule.next_fire_at.map_or_else(
                            || "No next run".into(),
                            |time| time.format("%Y-%m-%d %H:%M UTC").to_string(),
                        );
                        div()
                            .id(("automation-schedule", index))
                            .debug_selector(move || format!("automation-schedule-{index}"))
                            .min_h(px(38.))
                            .px_2()
                            .py_1()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_2()
                            .border_b_1()
                            .border_color(colors.subtle_border)
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .truncate()
                                            .font_family("monospace")
                                            .text_color(colors.text)
                                            .child(format!(
                                                "{} · {}",
                                                schedule.cron, schedule.timezone
                                            )),
                                    )
                                    .child(
                                        div()
                                            .truncate()
                                            .text_xs()
                                            .text_color(colors.disabled_text)
                                            .child(format!(
                                                "{} · {:?} · {:?}",
                                                next_fire,
                                                schedule.misfire_policy,
                                                schedule.concurrency_policy
                                            )),
                                    ),
                            )
                            .child(
                                Button::new(("edit-automation-schedule", index), "Edit")
                                    .tone(ButtonTone::Ghost)
                                    .disabled(shell.automation_details_loading)
                                    .on_click(cx.listener(move |shell, _, _, cx| {
                                        shell.edit_automation_schedule(edit_schedule.clone(), cx)
                                    })),
                            )
                            .child(
                                Button::new(
                                    ("toggle-automation-schedule", index),
                                    if schedule.enabled {
                                        "Disable"
                                    } else {
                                        "Enable"
                                    },
                                )
                                .tone(ButtonTone::Ghost)
                                .disabled(shell.automation_details_loading)
                                .on_click(cx.listener(
                                    move |shell, _, _, cx| {
                                        shell.set_automation_schedule_enabled(
                                            toggle_schedule.clone(),
                                            cx,
                                        )
                                    },
                                )),
                            )
                            .child(
                                Button::new(("delete-automation-schedule", index), "Delete")
                                    .tone(ButtonTone::DangerGhost)
                                    .disabled(shell.automation_details_loading)
                                    .on_click(cx.listener(move |shell, _, _, cx| {
                                        shell
                                            .delete_automation_schedule(delete_schedule.clone(), cx)
                                    })),
                            )
                    });
            let occurrences = shell
                .automation_occurrences
                .iter()
                .take(12)
                .cloned()
                .enumerate()
                .map(|(index, occurrence)| {
                    let resumable =
                        occurrence.state == sift_protocol::ScheduleOccurrenceState::Blocked;
                    let resume_occurrence = occurrence.clone();
                    div()
                        .id(("automation-occurrence", index))
                        .min_h(px(30.))
                        .px_2()
                        .flex()
                        .items_center()
                        .gap_2()
                        .border_b_1()
                        .border_color(colors.subtle_border)
                        .child(
                            div().w(px(132.)).text_xs().child(
                                occurrence
                                    .scheduled_for
                                    .format("%Y-%m-%d %H:%M")
                                    .to_string(),
                            ),
                        )
                        .child(
                            div()
                                .w(px(92.))
                                .text_xs()
                                .text_color(if resumable {
                                    colors.warning
                                } else {
                                    colors.muted_text
                                })
                                .child(format!("{:?}", occurrence.state)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(colors.disabled_text)
                                .child(occurrence.error_code.clone().unwrap_or_default()),
                        )
                        .when(resumable, |row| {
                            row.child(
                                Button::new(("resume-automation-occurrence", index), "Resume")
                                    .debug_selector(format!("resume-automation-occurrence-{index}"))
                                    .tone(ButtonTone::Accent)
                                    .disabled(shell.automation_details_loading)
                                    .on_click(cx.listener(move |shell, _, _, cx| {
                                        shell.resume_automation_occurrence(
                                            resume_occurrence.clone(),
                                            cx,
                                        )
                                    })),
                            )
                        })
                });
            let steps = shell.automation_run_steps.iter().map(|step| {
                div()
                    .h(px(28.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(36.))
                            .text_xs()
                            .child(format!("#{}", step.ordinal + 1)),
                    )
                    .child(
                        div()
                            .w(px(88.))
                            .text_xs()
                            .child(format!("{:?}", step.state)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(colors.disabled_text)
                            .child(step.error_code.clone().unwrap_or_else(|| {
                                step.row_count
                                    .map_or_else(String::new, |rows| format!("{rows} rows"))
                            })),
                    )
            });
            let logs = shell.automation_run_logs.iter().map(|entry| {
                div()
                    .min_h(px(26.))
                    .px_2()
                    .py_1()
                    .flex()
                    .items_start()
                    .gap_2()
                    .child(
                        div()
                            .w(px(52.))
                            .flex_none()
                            .text_xs()
                            .text_color(colors.disabled_text)
                            .child(entry.sequence.to_string()),
                    )
                    .child(
                        div()
                            .w(px(54.))
                            .flex_none()
                            .text_xs()
                            .text_color(colors.muted_text)
                            .child(entry.level.clone()),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .font_family("monospace")
                            .text_xs()
                            .whitespace_normal()
                            .text_color(colors.text)
                            .child(entry.message.clone()),
                    )
            });
            let editing_schedule = shell.automation_schedule_edit.is_some();
            let misfire_label = format!("Misfire: {:?}", shell.automation_schedule_misfire);
            let concurrency_label =
                format!("Concurrency: {:?}", shell.automation_schedule_concurrency);
            let run_heading = shell.automation_detail_run.as_ref().map_or_else(
                || "LATEST RUN · NONE".into(),
                |run| format!("LATEST RUN {} · {:?}", run.id.0, run.state),
            );
            let details = div()
                .id("automation-details-scroll")
                .debug_selector(|| "automation-details".into())
                .flex_1()
                .min_w_0()
                .min_h_0()
                .border_t_1()
                .border_color(colors.subtle_border)
                .overflow_y_scroll()
                .child(
                    div()
                        .h(px(28.))
                        .px_2()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .justify_between()
                        .child(SectionLabel::new("SCHEDULES"))
                        .child(
                            Button::new("new-automation-schedule", "New schedule")
                                .tone(ButtonTone::Ghost)
                                .disabled(shell.automation_details_loading)
                                .on_click(cx.listener(|shell, _, _, cx| {
                                    shell.clear_automation_schedule_editor(cx);
                                    cx.notify();
                                })),
                        ),
                )
                .child(
                    div()
                        .px_2()
                        .pb_2()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(180.))
                                .child(shell.automation_schedule_cron_input.clone()),
                        )
                        .child(
                            div()
                                .w(px(130.))
                                .child(shell.automation_schedule_timezone_input.clone()),
                        )
                        .child(
                            Button::new("automation-misfire-policy", misfire_label)
                                .tone(ButtonTone::Ghost)
                                .on_click(cx.listener(|shell, _, _, cx| {
                                    shell.cycle_automation_misfire_policy(cx)
                                })),
                        )
                        .child(
                            Button::new("automation-concurrency-policy", concurrency_label)
                                .tone(ButtonTone::Ghost)
                                .on_click(cx.listener(|shell, _, _, cx| {
                                    shell.cycle_automation_concurrency_policy(cx)
                                })),
                        )
                        .child(
                            Button::new(
                                "save-automation-schedule",
                                if editing_schedule { "Update" } else { "Create" },
                            )
                            .debug_selector("save-automation-schedule")
                            .tone(ButtonTone::Accent)
                            .loading(shell.automation_details_loading)
                            .disabled(
                                shell.selected_automation_id().is_none()
                                    || shell.automation_details_loading,
                            )
                            .on_click(
                                cx.listener(|shell, _, _, cx| shell.save_automation_schedule(cx)),
                            ),
                        ),
                )
                .when(shell.automation_schedules.is_empty(), |panel| {
                    panel.child(
                        div()
                            .px_2()
                            .pb_2()
                            .text_xs()
                            .child("No schedules configured."),
                    )
                })
                .children(schedules)
                .child(
                    div()
                        .h(px(28.))
                        .px_2()
                        .mt_1()
                        .flex()
                        .items_center()
                        .child(SectionLabel::new("RECENT OCCURRENCES")),
                )
                .when(shell.automation_occurrences.is_empty(), |panel| {
                    panel.child(
                        div()
                            .px_2()
                            .pb_2()
                            .text_xs()
                            .child("No scheduled occurrences yet."),
                    )
                })
                .children(occurrences)
                .child(
                    div()
                        .h(px(28.))
                        .px_2()
                        .mt_1()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(SectionLabel::new(run_heading))
                        .child(
                            Button::new("refresh-automation-details", "Refresh")
                                .tone(ButtonTone::Ghost)
                                .loading(shell.automation_details_loading)
                                .on_click(cx.listener(|shell, _, _, cx| {
                                    shell.request_automation_details(cx)
                                })),
                        ),
                )
                .children(steps)
                .child(
                    div()
                        .h(px(28.))
                        .px_2()
                        .mt_1()
                        .flex()
                        .items_center()
                        .child(SectionLabel::new("DURABLE LOG")),
                )
                .when(shell.automation_run_logs.is_empty(), |panel| {
                    panel.child(
                        div()
                            .px_2()
                            .pb_2()
                            .text_xs()
                            .child("No log entries for this run."),
                    )
                })
                .children(logs);
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
                        .child(div().w(px(56.)).flex_none().child("STEPS"))
                        .child(div().w(px(96.)).flex_none().child("STATUS"))
                        .child(div().w(px(18.)).flex_none()),
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
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .id("automation-configuration-list")
                                .w_full()
                                .min_w_0()
                                .min_h_0()
                                .when(shell.automation_expanded.is_some(), |list| {
                                    list.flex_none().max_h(px(180.))
                                })
                                .when(shell.automation_expanded.is_none(), |list| list.flex_1())
                                .overflow_y_scroll()
                                .children(rows),
                        )
                        .children(shell.automation_expanded.map(|_| details)),
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

fn render_server_dashboard(
    shell: &WorkspaceShell,
    cx: &mut Context<WorkspaceShell>,
) -> gpui::AnyElement {
    let colors = cx.theme().colors;
    let report = shell.database_monitor.dashboard();
    div()
        .flex()
        .flex_1()
        .min_h_0()
        .flex_col()
        .child(
            div()
                .h(px(30.))
                .px_3()
                .flex()
                .items_center()
                .gap_2()
                .child(SectionLabel::new("SERVER OVERVIEW"))
                .child(div().flex_1())
                .child(
                    Button::new("refresh-server-dashboard", if shell.database_monitor.dashboard_request().loading() { "Loading…" } else { "Refresh" })
                        .tone(ButtonTone::Ghost)
                        .disabled(shell.database_monitor.dashboard_request().loading())
                        .on_click(cx.listener(|shell, _, _, cx| shell.load_server_dashboard(cx))),
                ),
        )
        .children(shell.database_monitor.dashboard_request().error().map(|message| {
            div().p_2().text_color(colors.danger).child(message.to_string())
        }))
        .children(report.map(|report| {
            let process_text = match &report.processes.summary {
                Some(summary) => format!(
                    "Processes sampled: {}{}  ·  Active: {}  ·  Waiting: {}  ·  Blocked: {}  ·  Idle in transaction: {}",
                    summary.observed,
                    if summary.incomplete { "+" } else { "" },
                    summary.active,
                    summary.waiting,
                    summary.blocked,
                    summary.idle_in_transaction,
                ),
                None => format!(
                    "Processes: {}",
                    report.processes.reason.as_deref().unwrap_or("unavailable")
                ),
            };
            div()
                .px_3()
                .py_2()
                .flex()
                .flex_col()
                .gap_1()
                .child(format!("{:?} · sampled {}", report.engine, report.sampled_at.format("%Y-%m-%d %H:%M:%S UTC")))
                .child(process_text)
                .children(report.capabilities.iter().map(|capability| {
                    let label = match capability.operation {
                        sift_protocol::OperationKind::ListProcesses => "Activity and locks",
                        sift_protocol::OperationKind::ListDeadlocks => "Deadlock history",
                        sift_protocol::OperationKind::ListPostgresSettings => "PostgreSQL settings",
                        sift_protocol::OperationKind::ReadQueryStore => "Query Store",
                        sift_protocol::OperationKind::ReadAgentJobs => "Agent jobs",
                        sift_protocol::OperationKind::ReadSqlServerSettings => "SQL Server settings",
                        sift_protocol::OperationKind::ReadSqlServerSecurity => "SQL Server security",
                        _ => "Inspection",
                    };
                    div().child(format!("{label}: {}", if capability.available { "available" } else { capability.reason.as_deref().unwrap_or("unavailable") }))
                }))
        }))
        .into_any_element()
}

fn render_query_store(
    shell: &WorkspaceShell,
    cx: &mut Context<WorkspaceShell>,
) -> gpui::AnyElement {
    let colors = cx.theme().colors;
    let report = shell.database_monitor.query_store();
    let state = report.map(|report| match report.state {
        sift_protocol::QueryStoreState::ReadWrite => "Read/write",
        sift_protocol::QueryStoreState::ReadOnly => "Read-only",
        sift_protocol::QueryStoreState::ReadCaptureSecondary => "Secondary capture",
        sift_protocol::QueryStoreState::Off => "Off",
        sift_protocol::QueryStoreState::Error => "Error",
        sift_protocol::QueryStoreState::PermissionRequired => "Permission required",
    });
    div()
        .flex()
        .flex_1()
        .min_h_0()
        .flex_col()
        .child(
            div()
                .h(px(30.))
                .px_3()
                .flex()
                .items_center()
                .gap_2()
                .child(SectionLabel::new("SQL SERVER QUERY STORE"))
                .child(div().flex_1())
                .child(
                    Button::new(
                        "refresh-query-store",
                        if shell.database_monitor.query_store_request().loading() {
                            "Loading…"
                        } else {
                            "Refresh"
                        },
                    )
                    .tone(ButtonTone::Ghost)
                    .disabled(shell.database_monitor.query_store_request().loading())
                    .on_click(cx.listener(|shell, _, _, cx| shell.load_query_store(cx))),
                ),
        )
        .children(shell.database_monitor.query_store_request().error().map(|message| {
            div().p_2().text_color(colors.danger).child(message.to_string())
        }))
        .children(report.map(|report| {
            div()
                .px_3()
                .py_1()
                .text_xs()
                .child(format!("{} · {} · {} plan{}{}", report.database, state.unwrap_or(""), report.plans.len(), if report.plans.len() == 1 { "" } else { "s" }, if report.truncated { " (first 100 shown)" } else { "" }))
        }))
        .when(report.is_some_and(|report| report.state == sift_protocol::QueryStoreState::PermissionRequired), |panel| {
            panel.child(div().px_3().py_2().text_color(colors.warning).child("Query Store requires VIEW DATABASE STATE, or VIEW DATABASE PERFORMANCE STATE on SQL Server 2022+."))
        })
        .when(report.is_some_and(|report| report.state == sift_protocol::QueryStoreState::Off), |panel| {
            panel.child(div().px_3().py_2().child("Query Store is disabled for this database."))
        })
        .child(
            div()
                .id("query-store-plan-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .children(report.into_iter().flat_map(|report| report.plans.iter()).map(|plan| {
                    let last = plan.last_execution_at.as_ref().map(|time| time.to_rfc3339()).unwrap_or_else(|| "Never".into());
                    div()
                        .px_3()
                        .py_2()
                        .border_b_1()
                        .border_color(colors.subtle_border)
                        .child(div().text_xs().child(format!("Query {} · Plan {} · {} executions · {:.3} ms average · Last {}", plan.query_id, plan.plan_id, plan.executions, plan.average_duration_ms, last)))
                        .child(div().text_xs().whitespace_normal().text_color(colors.muted_text).child(plan.sql_text.clone()))
                })),
        )
        .into_any_element()
}

fn render_agent_jobs(shell: &WorkspaceShell, cx: &mut Context<WorkspaceShell>) -> gpui::AnyElement {
    let colors = cx.theme().colors;
    let report = shell.database_monitor.agent_jobs();
    let loading = shell.database_monitor.agent_jobs_request().loading();
    let selected = shell.database_monitor.agent_jobs_selected();
    div()
        .debug_selector(|| "agent-jobs-browser".into())
        .flex()
        .flex_1()
        .min_h_0()
        .flex_col()
        .child(
            div()
                .h(px(30.))
                .px_3()
                .flex()
                .items_center()
                .gap_2()
                .child(SectionLabel::new("SQL SERVER AGENT JOBS"))
                .child(div().text_xs().child("j/k select · g/G ends · r refresh"))
                .child(div().flex_1())
                .child(
                    Button::new(
                        "refresh-agent-jobs",
                        if loading { "Loading…" } else { "Refresh" },
                    )
                    .tone(ButtonTone::Ghost)
                    .disabled(loading)
                    .on_click(cx.listener(|shell, _, _, cx| shell.load_agent_jobs(cx))),
                ),
        )
        .children(
            shell
                .database_monitor
                .agent_jobs_request()
                .error()
                .map(|message| {
                    div()
                        .p_2()
                        .text_color(colors.danger)
                        .child(message.to_string())
                }),
        )
        .children(report.map(|report| {
            div().px_3().py_1().text_xs().child(format!(
                "{} job{}{} · owned jobs for non-sysadmins · whole-job history only · server-local run times",
                report.jobs.len(),
                if report.jobs.len() == 1 { "" } else { "s" },
                if report.truncated {
                    " (first 100 shown)"
                } else {
                    ""
                },
            ))
        }))
        .when(
            report.is_some_and(|report| {
                report.state == sift_protocol::AgentJobsState::PermissionRequired
            }),
            |panel| {
                panel.child(
                    div()
                        .px_3()
                        .py_2()
                        .text_color(colors.warning)
                        .child("This login cannot read SQL Server Agent jobs and history in msdb."),
                )
            },
        )
        .child(
            div()
                .id("agent-jobs-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .children(
                    report
                        .into_iter()
                        .flat_map(|report| report.jobs.iter())
                        .enumerate()
                        .map(|(index, job)| {
                            let outcome = match job.last_outcome {
                                Some(sift_protocol::AgentJobOutcome::Failed) => "Failed",
                                Some(sift_protocol::AgentJobOutcome::Succeeded) => "Succeeded",
                                Some(sift_protocol::AgentJobOutcome::Retry) => "Retry",
                                Some(sift_protocol::AgentJobOutcome::Canceled) => "Canceled",
                                Some(sift_protocol::AgentJobOutcome::InProgress) => "In progress",
                                Some(sift_protocol::AgentJobOutcome::Unknown) => "Unknown",
                                None => "Never run",
                            };
                            div()
                                .debug_selector(move || format!("agent-job-{index}"))
                                .px_3()
                                .py_2()
                                .border_b_1()
                                .border_color(colors.subtle_border)
                                .when(index == selected, |row| row.bg(colors.active_surface))
                                .child(
                                    div()
                                        .flex()
                                        .gap_3()
                                        .child(
                                            div()
                                                .flex_1()
                                                .font_family("monospace")
                                                .child(job.name.clone()),
                                        )
                                        .child(div().w(px(80.)).child(if job.enabled {
                                            "Enabled"
                                        } else {
                                            "Disabled"
                                        }))
                                        .child(div().w(px(100.)).child(outcome)),
                                )
                                .child(div().text_xs().text_color(colors.disabled_text).child(
                                    format!(
                                            "Owner: {} · Last run: {} · Duration: {}",
                                            job.owner.as_deref().unwrap_or("Unavailable"),
                                            job.last_run_local.as_deref().unwrap_or("Never"),
                                            job.last_duration_seconds
                                                .map_or("—".into(), |seconds| format!(
                                                    "{seconds}s"
                                                )),
                                        ),
                                ))
                        }),
                ),
        )
        .when(
            report.is_some_and(|report| {
                report.state == sift_protocol::AgentJobsState::Available && report.jobs.is_empty()
            }),
            |panel| {
                panel.child(
                    div()
                        .p_4()
                        .text_center()
                        .child("No Agent jobs are visible to this login."),
                )
            },
        )
        .into_any_element()
}

fn render_postgres_maintenance(
    shell: &WorkspaceShell,
    cx: &mut Context<WorkspaceShell>,
) -> gpui::AnyElement {
    let state = &shell.pg_maintenance;
    let colors = cx.theme().colors;
    let busy = state.pending.is_some();
    let reason = shell.operation_unavailable_reason(sift_protocol::OperationKind::ExecuteQuery);
    let unavailable = reason.is_some();
    let choices = [
        (PgMaintenanceChoice::Vacuum, "VACUUM"),
        (PgMaintenanceChoice::Analyze, "ANALYZE"),
        (PgMaintenanceChoice::ReindexTable, "REINDEX table"),
        (PgMaintenanceChoice::ReindexIndex, "REINDEX index"),
        (PgMaintenanceChoice::HeapIntegrity, "Heap integrity"),
    ];
    let mut panel = div()
        .debug_selector(|| "postgres-maintenance".into())
        .id("postgres-maintenance-scroll")
        .flex().flex_1().min_h_0().flex_col().overflow_y_scroll().p_3().gap_2()
        .child(SectionLabel::new("POSTGRESQL MAINTENANCE"))
        .child(div().text_xs().child("Explicit schema and table/index only. v vacuum · n analyze · t reindex table · x reindex index · h heap check · p preview · a apply · r run check"))
        .children(reason.map(|message| div().text_color(colors.warning).child(message)))
        .child(div().flex().flex_wrap().gap_2().children(choices.into_iter().enumerate().map(|(index, (choice, label))|
            Button::new(("pg-maintenance-choice", index), label)
                .tone(if state.choice == choice { ButtonTone::Neutral } else { ButtonTone::Ghost })
                .disabled(busy)
                .on_click(cx.listener(move |shell, _, _, cx| shell.set_pg_maintenance_choice(choice, cx)))
        )))
        .child(div().child("Schema").child(state.schema.clone()))
        .child(div().child(if state.choice == PgMaintenanceChoice::ReindexIndex { "Index name" } else { "Table name" }).child(state.name.clone()));
    if state.choice == PgMaintenanceChoice::HeapIntegrity {
        panel = panel
            .child(div().text_xs().child("Checks heap pages with an already-installed amcheck extension. Indexes and TOAST are excluded; findings are capped."))
            .child(Button::new("pg-maintenance-run-integrity", if busy { "Checking…" } else { "Run heap check" })
                .disabled(busy || unavailable)
                .on_click(cx.listener(|shell, _, _, cx| shell.run_pg_integrity(cx))));
        if let Some(report) = &state.integrity {
            panel = panel.child(format!(
                "Outcome: {:?} · {} finding(s) · {} warning(s)",
                report.outcome,
                report.findings.len(),
                report.warnings.len()
            ));
            for finding in report.findings.iter().take(50) {
                panel = panel.child(div().font_family("monospace").child(finding.clone()));
            }
            if report.findings.len() > 50 {
                panel = panel.child(format!(
                    "Showing first 50 of {} findings",
                    report.findings.len()
                ));
            }
            for warning in report.warnings.iter().take(20) {
                panel = panel.child(
                    div()
                        .text_color(colors.warning)
                        .child(format!("{warning:?}")),
                );
            }
        }
    } else {
        if state.choice == PgMaintenanceChoice::Vacuum {
            panel = panel.child(
                Button::new(
                    "pg-maintenance-analyze-option",
                    if state.analyze_with_vacuum {
                        "Also ANALYZE: on"
                    } else {
                        "Also ANALYZE: off"
                    },
                )
                .disabled(busy)
                .tone(ButtonTone::Ghost)
                .on_click(cx.listener(|shell, _, _, cx| {
                    shell.pg_maintenance.analyze_with_vacuum =
                        !shell.pg_maintenance.analyze_with_vacuum;
                    shell.pg_maintenance.invalidate();
                    cx.notify();
                })),
            );
        }
        if matches!(
            state.choice,
            PgMaintenanceChoice::ReindexTable | PgMaintenanceChoice::ReindexIndex
        ) {
            panel = panel.child(
                Button::new(
                    "pg-maintenance-concurrently-option",
                    if state.concurrently {
                        "Concurrently: on"
                    } else {
                        "Concurrently: off"
                    },
                )
                .disabled(busy)
                .tone(ButtonTone::Ghost)
                .on_click(cx.listener(|shell, _, _, cx| {
                    shell.pg_maintenance.concurrently = !shell.pg_maintenance.concurrently;
                    shell.pg_maintenance.invalidate();
                    cx.notify();
                })),
            );
        }
        panel = panel.child(
            Button::new(
                "pg-maintenance-preview",
                if busy { "Working…" } else { "Preview SQL" },
            )
            .disabled(busy || unavailable)
            .on_click(cx.listener(|shell, _, _, cx| shell.preview_pg_maintenance(cx))),
        );
        if let Some((request, report)) = &state.preview {
            panel = panel
                .child(SectionLabel::new("REVIEWED SQL"))
                .child(div().font_family("monospace").child(report.sql.clone()))
                .child(div().text_xs().child(format!(
                    "Type APPLY {}.{} to run this statement",
                    request.schema, request.name
                )))
                .child(state.confirmation.clone())
                .child(
                    Button::new("pg-maintenance-apply", "Apply confirmed maintenance")
                        .disabled(busy || unavailable)
                        .tone(ButtonTone::Danger)
                        .on_click(cx.listener(|shell, _, _, cx| shell.apply_pg_maintenance(cx))),
                );
        }
    }
    if let Some(message) = &state.message {
        panel = panel.child(div().text_color(colors.warning).child(message.clone()));
    }
    if let Some(report) = &state.last_report {
        panel = panel.child(div().font_family("monospace").child(report.sql.clone()));
        for warning in &report.warnings {
            panel = panel.child(
                div()
                    .text_color(colors.warning)
                    .child(format!("{warning:?}")),
            );
        }
    }
    panel.into_any_element()
}

fn render_sqlserver_maintenance(
    shell: &WorkspaceShell,
    cx: &mut Context<WorkspaceShell>,
) -> gpui::AnyElement {
    let state = &shell.maintenance;
    let colors = cx.theme().colors;
    let mode = state.mode;
    let busy = state.pending.is_some();
    let mut panel = div()
        .debug_selector(|| "sqlserver-maintenance".into())
        .id("sqlserver-maintenance-scroll")
        .flex().flex_1().min_h_0().flex_col().overflow_y_scroll().p_3().gap_2()
        .child(SectionLabel::new("SQL SERVER MAINTENANCE"))
        .child(div().text_xs().child("Recovery requires a master connection with VIEW ANY DATABASE. Paths are on the SQL Server host. b backup · n restore · i integrity · p preview · a apply · r run check"))
        .child(div().flex().gap_2()
            .child(Button::new("maintenance-backup-mode", "Backup")
                .disabled(busy)
                .tone(if mode == MaintenanceMode::Backup { ButtonTone::Neutral } else { ButtonTone::Ghost })
                .on_click(cx.listener(|shell, _, _, cx| { shell.maintenance.mode = MaintenanceMode::Backup; shell.maintenance.invalidate(); cx.notify(); })))
            .child(Button::new("maintenance-restore-mode", "Restore to new name")
                .disabled(busy)
                .tone(if mode == MaintenanceMode::Restore { ButtonTone::Neutral } else { ButtonTone::Ghost })
                .on_click(cx.listener(|shell, _, _, cx| { shell.maintenance.mode = MaintenanceMode::Restore; shell.maintenance.invalidate(); cx.notify(); })))
            .child(Button::new("maintenance-integrity-mode", "Integrity check")
                .disabled(busy)
                .tone(if mode == MaintenanceMode::Integrity { ButtonTone::Neutral } else { ButtonTone::Ghost })
                .on_click(cx.listener(|shell, _, _, cx| { shell.maintenance.mode = MaintenanceMode::Integrity; shell.maintenance.invalidate(); cx.notify(); }))));
    if mode == MaintenanceMode::Integrity {
        panel = panel
            .child("Runs DBCC CHECKDB on the connected database. No repair mode is available.")
            .child(
                Button::new(
                    "maintenance-physical-only",
                    if state.physical_only {
                        "Physical only: on"
                    } else {
                        "Physical only: off"
                    },
                )
                .disabled(busy)
                .tone(ButtonTone::Ghost)
                .on_click(cx.listener(|shell, _, _, cx| {
                    shell.maintenance.physical_only = !shell.maintenance.physical_only;
                    shell.maintenance.invalidate();
                    cx.notify();
                })),
            )
            .child(
                Button::new(
                    "maintenance-run-integrity",
                    if busy {
                        "Checking…"
                    } else {
                        "Run integrity check"
                    },
                )
                .disabled(busy)
                .on_click(cx.listener(|shell, _, _, cx| shell.run_sqlserver_integrity(cx))),
            );
        if let Some(report) = &state.integrity {
            panel = panel.child(format!("Outcome: {:?}", report.outcome));
            for finding in &report.findings {
                panel = panel.child(div().child(finding.clone()));
            }
            for warning in &report.warnings {
                panel = panel.child(
                    div()
                        .text_color(colors.warning)
                        .child(format!("{warning:?}")),
                );
            }
        }
    } else {
        panel = panel
            .child(
                div()
                    .child(if mode == MaintenanceMode::Backup {
                        "Existing source database"
                    } else {
                        "New destination database name"
                    })
                    .child(state.database.clone()),
            )
            .child(
                div()
                    .child("Absolute archive path on SQL Server host")
                    .child(state.archive.clone()),
            );
        if mode == MaintenanceMode::Restore {
            panel = panel
                .child(div().child("Backup set number").child(state.backup_set.clone()))
                .child(div().child("MOVE mappings JSON: [{\"logical_name\":\"data\",\"destination\":\"/data/new.mdf\"}]").child(state.moves.clone()));
        }
        panel = panel.child(
            Button::new(
                "maintenance-preview",
                if busy { "Working…" } else { "Preview" },
            )
            .disabled(busy)
            .on_click(cx.listener(|shell, _, _, cx| shell.preview_sqlserver_recovery(cx))),
        );
        if let Some((_, report)) = &state.preview {
            panel = panel
                .child(SectionLabel::new("PREVIEW"))
                .child(div().font_family("monospace").child(report.sql.clone()));
            if let Some(source) = &report.source_database {
                panel = panel.child(format!("Source database: {source}"));
            }
            for warning in &report.warnings {
                panel = panel.child(
                    div()
                        .text_color(colors.warning)
                        .child(format!("{warning:?}")),
                );
            }
            panel = panel
                .child(
                    div()
                        .child(format!(
                            "Type {} {} to apply",
                            if mode == MaintenanceMode::Backup {
                                "BACKUP"
                            } else {
                                "RESTORE"
                            },
                            state.database.read(cx).text()
                        ))
                        .child(state.confirmation.clone()),
                )
                .child(
                    Button::new("maintenance-apply", "Apply confirmed recovery")
                        .disabled(busy)
                        .tone(ButtonTone::Danger)
                        .on_click(
                            cx.listener(|shell, _, _, cx| shell.apply_sqlserver_recovery(cx)),
                        ),
                );
        }
    }
    if let Some(message) = &state.message {
        panel = panel.child(div().text_color(colors.warning).child(message.clone()));
    }
    if let Some(report) = &state.last_recovery {
        panel = panel.child(div().font_family("monospace").child(report.sql.clone()));
        for warning in &report.warnings {
            panel = panel.child(
                div()
                    .text_color(colors.warning)
                    .child(format!("{warning:?}")),
            );
        }
    }
    panel.into_any_element()
}

fn render_sqlserver_settings(
    shell: &WorkspaceShell,
    cx: &mut Context<WorkspaceShell>,
) -> gpui::AnyElement {
    let colors = cx.theme().colors;
    let report = shell.database_monitor.sqlserver_settings();
    let loading = shell
        .database_monitor
        .sqlserver_settings_request()
        .loading();
    let selected = shell.database_monitor.sqlserver_settings_selected();
    div()
        .debug_selector(|| "sqlserver-settings-browser".into())
        .flex()
        .flex_1()
        .min_h_0()
        .flex_col()
        .child(
            div()
                .h(px(30.))
                .px_3()
                .flex()
                .items_center()
                .gap_2()
                .child(SectionLabel::new("SQL SERVER SETTINGS"))
                .child(div().text_xs().child("j/k select · g/G ends · r refresh"))
                .child(div().flex_1())
                .child(
                    Button::new(
                        "refresh-sqlserver-settings",
                        if loading { "Loading…" } else { "Refresh" },
                    )
                    .tone(ButtonTone::Ghost)
                    .disabled(loading)
                    .on_click(cx.listener(|shell, _, _, cx| shell.load_sqlserver_settings(cx))),
                ),
        )
        .children(
            shell
                .database_monitor
                .sqlserver_settings_request()
                .error()
                .map(|message| {
                    div()
                        .p_2()
                        .text_color(colors.danger)
                        .child(message.to_string())
                }),
        )
        .children(report.map(|report| {
            div().px_3().py_1().text_xs().child(format!(
                "{} settings{} · configured and effective values",
                report.settings.len(),
                if report.truncated { " (first 200 shown)" } else { "" },
            ))
        }))
        .when(
            report.is_some_and(|report| {
                report.state == sift_protocol::SqlServerSettingsState::PermissionRequired
            }),
            |panel| {
                panel.child(div().px_3().py_2().text_color(colors.warning).child(
                    "This login cannot read sys.configurations. SQL Server 2022 and later require VIEW SERVER PERFORMANCE STATE.",
                ))
            },
        )
        .child(
            div()
                .id("sqlserver-settings-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .children(
                    report
                        .into_iter()
                        .flat_map(|report| report.settings.iter())
                        .enumerate()
                        .map(|(index, setting)| {
                            div()
                                .debug_selector(move || format!("sqlserver-setting-{index}"))
                                .px_3()
                                .py_2()
                                .border_b_1()
                                .border_color(colors.subtle_border)
                                .when(index == selected, |row| row.bg(colors.active_surface))
                                .child(
                                    div()
                                        .flex()
                                        .gap_3()
                                        .child(
                                            div()
                                                .flex_1()
                                                .font_family("monospace")
                                                .child(setting.name.clone()),
                                        )
                                        .child(div().w(px(120.)).child(format!(
                                            "Configured: {}",
                                            setting.configured_value
                                        )))
                                        .child(div().w(px(120.)).child(format!(
                                            "Effective: {}",
                                            setting.effective_value
                                        ))),
                                )
                                .child(div().text_xs().text_color(colors.disabled_text).child(
                                    format!(
                                        "{} · Range: {}–{} · {} · {}",
                                        setting.description,
                                        setting.minimum,
                                        setting.maximum,
                                        if setting.is_dynamic { "Dynamic" } else { "Restart required" },
                                        if setting.is_advanced { "Advanced" } else { "Standard" },
                                    ),
                                ))
                        }),
                ),
        )
        .when(
            report.is_some_and(|report| {
                report.state == sift_protocol::SqlServerSettingsState::Available
                    && report.settings.is_empty()
            }),
            |panel| panel.child(div().p_4().text_center().child("No settings returned.")),
        )
        .into_any_element()
}

fn append_security_section<T>(
    lines: &mut Vec<String>,
    title: &str,
    section: &sift_protocol::SqlServerSecuritySection<T>,
    format: impl Fn(&T) -> String,
) {
    if section.state == sift_protocol::SqlServerSecurityState::PermissionRequired {
        lines.push(format!("{title}: permission required"));
        return;
    }
    lines.push(format!(
        "{title}: {}{}",
        section.items.len(),
        if section.truncated { "+" } else { "" }
    ));
    lines.extend(
        section
            .items
            .iter()
            .map(|item| format!("  {}", format(item))),
    );
}

fn render_sqlserver_security(
    shell: &WorkspaceShell,
    cx: &mut Context<WorkspaceShell>,
) -> gpui::AnyElement {
    let colors = cx.theme().colors;
    let state = &shell.database_monitor;
    let mut lines = Vec::new();
    if let Some(report) = state.security() {
        lines.push(format!(
            "Database: {} · metadata-visible principals · explicit permissions only",
            report.database
        ));
        lines.push("REVIEWABLE CHANGES · d preview selected removal".into());
        for (index, item) in report.memberships.items.iter().enumerate() {
            lines.push(format!(
                "{} Drop membership: {} → {}",
                if index == state.security_selected() {
                    "▶"
                } else {
                    " "
                },
                item.role,
                item.member
            ));
        }
        for (offset, item) in report
            .schema_permissions
            .items
            .iter()
            .filter(|item| {
                item.permission == "SELECT"
                    && (item.state == "GRANT" || item.state == "GRANT_WITH_GRANT_OPTION")
            })
            .enumerate()
        {
            let index = report.memberships.items.len() + offset;
            lines.push(format!(
                "{} Revoke SELECT: {} → {}",
                if index == state.security_selected() {
                    "▶"
                } else {
                    " "
                },
                item.schema,
                item.grantee
            ));
        }
        append_security_section(&mut lines, "Visible logins", &report.logins, |item| {
            format!("{} · {}", item.name, item.kind)
        });
        append_security_section(
            &mut lines,
            "Database principals",
            &report.principals,
            |item| format!("{} · {} · {}", item.name, item.kind, item.authentication),
        );
        append_security_section(
            &mut lines,
            "Role memberships",
            &report.memberships,
            |item| format!("{} → {}", item.role, item.member),
        );
        append_security_section(&mut lines, "Schema ownership", &report.schemas, |item| {
            format!("{} · {}", item.schema, item.owner)
        });
        append_security_section(
            &mut lines,
            "Schema permissions",
            &report.schema_permissions,
            |item| {
                format!(
                    "{} · {} · {} · {}",
                    item.schema, item.grantee, item.permission, item.state
                )
            },
        );
    }
    div()
        .flex()
        .flex_1()
        .min_h_0()
        .flex_col()
        .child(
            div()
                .h(px(30.))
                .px_3()
                .flex()
                .items_center()
                .gap_2()
                .child(SectionLabel::new("SQL SERVER SECURITY"))
                .child(
                    div()
                        .text_xs()
                        .child("j/k select · d preview · Enter confirm · Esc cancel · r refresh"),
                )
                .child(div().flex_1())
                .child(
                    Button::new(
                        "refresh-sqlserver-security",
                        if state.security_request().loading() {
                            "Loading…"
                        } else {
                            "Refresh"
                        },
                    )
                    .tone(ButtonTone::Ghost)
                    .disabled(state.security_request().loading())
                    .on_click(cx.listener(|shell, _, _, cx| shell.load_sqlserver_security(cx))),
                ),
        )
        .children(state.security_request().error().map(|message| {
            div()
                .p_2()
                .text_color(colors.danger)
                .child(message.to_string())
        }))
        .children(state.security_action_request().error().map(|message| {
            div()
                .p_2()
                .text_color(colors.danger)
                .child(message.to_string())
        }))
        .children(state.security_preview().map(|preview| {
            div()
                .px_3()
                .py_2()
                .border_t_1()
                .border_color(colors.warning)
                .flex()
                .flex_col()
                .gap_1()
                .child("REVIEW SQL SERVER SECURITY CHANGE")
                .child(div().font_family("monospace").child(preview.sql.clone()))
                .child(
                    div()
                        .text_color(colors.warning)
                        .child(preview.warning.clone()),
                )
                .child("Enter confirms production apply · Esc cancels")
                .child(
                    Button::new("sqlserver-security-apply", "Confirm production and apply")
                        .disabled(state.security_action_request().loading())
                        .on_click(
                            cx.listener(|shell, _, _, cx| shell.apply_sqlserver_security(cx)),
                        ),
                )
        }))
        .child(
            div()
                .id("sqlserver-security-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .children(
                    lines
                        .into_iter()
                        .map(|line| div().px_3().py_1().font_family("monospace").child(line)),
                ),
        )
        .into_any_element()
}

fn render_postgres_settings(
    shell: &WorkspaceShell,
    cx: &mut Context<WorkspaceShell>,
) -> impl IntoElement {
    let colors = cx.theme().colors;
    let state = &shell.database_monitor;
    let offset = state.settings_offset();
    let next_offset = state.settings_next_offset();
    let loading = state.settings_request().loading();
    div()
        .debug_selector(|| "postgres-settings-browser".into())
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
                .gap_2()
                .child(SectionLabel::new("POSTGRESQL SETTINGS"))
                .child(div().text_xs().child("n next · p previous · r refresh"))
                .child(div().flex_1())
                .child(
                    Button::new("settings-previous", "Previous")
                        .tone(ButtonTone::Ghost)
                        .disabled(offset == 0 || loading)
                        .on_click(cx.listener(move |shell, _, _, cx| {
                            shell.load_postgres_settings(offset.saturating_sub(100), cx)
                        })),
                )
                .child(
                    Button::new("settings-next", "Next")
                        .tone(ButtonTone::Ghost)
                        .disabled(next_offset.is_none() || loading)
                        .on_click(cx.listener(move |shell, _, _, cx| {
                            if let Some(next) = next_offset {
                                shell.load_postgres_settings(next, cx);
                            }
                        })),
                )
                .child(
                    Button::new(
                        "settings-refresh",
                        if loading { "Loading…" } else { "Refresh" },
                    )
                    .tone(ButtonTone::Ghost)
                    .disabled(loading)
                    .on_click(
                        cx.listener(move |shell, _, _, cx| {
                            shell.load_postgres_settings(offset, cx)
                        }),
                    ),
                ),
        )
        .child(
            div()
                .id("postgres-settings-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .children(state.settings().iter().map(|setting| {
                    div()
                        .px_3()
                        .py_2()
                        .border_b_1()
                        .border_color(colors.subtle_border)
                        .child(
                            div()
                                .flex()
                                .gap_3()
                                .child(
                                    div()
                                        .flex_1()
                                        .font_family("monospace")
                                        .child(setting.name.clone()),
                                )
                                .child(div().flex_1().font_family("monospace").child(
                                    if setting.redacted {
                                        "[redacted]".to_string()
                                    } else {
                                        setting
                                            .value
                                            .clone()
                                            .unwrap_or_else(|| "[restricted]".into())
                                    },
                                ))
                                .child(div().w(px(115.)).child(setting.source.clone())),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(colors.disabled_text)
                                .child(format!(
                                    "{} · {}{}{}",
                                    setting.category,
                                    setting.context,
                                    if setting.pending_restart {
                                        " · restart pending"
                                    } else {
                                        ""
                                    },
                                    setting
                                        .description
                                        .as_deref()
                                        .map(|text| format!(" · {text}"))
                                        .unwrap_or_default(),
                                )),
                        )
                })),
        )
        .children(state.settings_request().error().map(|message| {
            div()
                .p_2()
                .text_color(colors.danger)
                .child(message.to_string())
        }))
        .when(
            state.settings().is_empty() && !loading && state.settings_request().error().is_none(),
            |panel| {
                panel.child(
                    div()
                        .p_4()
                        .text_center()
                        .child("No PostgreSQL settings reported."),
                )
            },
        )
}

fn render_postgres_objects(
    shell: &WorkspaceShell,
    cx: &mut Context<WorkspaceShell>,
) -> impl IntoElement {
    let colors = cx.theme().colors;
    let state = &shell.database_monitor;
    let extensions = state.view() == DatabaseMonitorView::Extensions;
    let offset = state.objects_offset();
    let next = state.objects_next_offset();
    let selected = state.objects_selected();
    let rows = if extensions {
        state
            .extensions()
            .iter()
            .enumerate()
            .map(|(index, item)| {
                div()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(colors.subtle_border)
                    .bg(if index == selected {
                        colors.accent_muted
                    } else {
                        colors.panel
                    })
                    .child(format!(
                        "{} · installed {} · available {}{}",
                        item.name,
                        item.installed_version.as_deref().unwrap_or("no"),
                        item.default_version.as_deref().unwrap_or("unknown"),
                        item.schema
                            .as_deref()
                            .map_or_else(String::new, |schema| format!(" · schema {schema}"))
                    ))
                    .into_any_element()
            })
            .collect::<Vec<_>>()
    } else if state.view() == DatabaseMonitorView::Partitions {
        state
            .partitions()
            .iter()
            .enumerate()
            .map(|(index, item)| {
                div()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(colors.subtle_border)
                    .bg(if index == selected {
                        colors.accent_muted
                    } else {
                        colors.panel
                    })
                    .child(format!(
                        "{}.{} → {}.{} · {}",
                        item.parent_schema,
                        item.parent,
                        item.child_schema,
                        item.child,
                        item.bound.as_deref().unwrap_or("bound unavailable")
                    ))
                    .into_any_element()
            })
            .collect::<Vec<_>>()
    } else if state.view() == DatabaseMonitorView::Policies {
        state
            .policies()
            .iter()
            .enumerate()
            .map(|(index, item)| {
                div()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(colors.subtle_border)
                    .bg(if index == selected {
                        colors.accent_muted
                    } else {
                        colors.panel
                    })
                    .child(format!(
                        "{}.{} · {} · {} · {} · {}{}{}{}",
                        item.schema,
                        item.table,
                        item.name,
                        item.command,
                        if item.permissive {
                            "permissive"
                        } else {
                            "restrictive"
                        },
                        item.roles,
                        if item.roles_truncated {
                            " [truncated]"
                        } else {
                            ""
                        },
                        if item.row_security_enabled {
                            " · RLS enabled"
                        } else {
                            " · RLS disabled"
                        },
                        if item.row_security_forced {
                            " · forced"
                        } else {
                            ""
                        }
                    ))
                    .child(format!(
                        "USING {}{} · CHECK {}{}",
                        item.using_expression.as_deref().unwrap_or("—"),
                        if item.using_truncated {
                            " [truncated]"
                        } else {
                            ""
                        },
                        item.check_expression.as_deref().unwrap_or("—"),
                        if item.check_truncated {
                            " [truncated]"
                        } else {
                            ""
                        }
                    ))
                    .into_any_element()
            })
            .collect::<Vec<_>>()
    } else if state.view() == DatabaseMonitorView::Roles {
        state
            .roles()
            .iter()
            .enumerate()
            .map(|(index, item)| {
                div()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(colors.subtle_border)
                    .bg(if index == selected {
                        colors.accent_muted
                    } else {
                        colors.panel
                    })
                    .child(format!(
                        "{} · {}{}{}",
                        item.name,
                        if item.can_login { "LOGIN" } else { "NOLOGIN" },
                        if item.can_create_role {
                            " · CREATEROLE"
                        } else {
                            ""
                        },
                        if item.superuser { " · SUPERUSER" } else { "" }
                    ))
                    .into_any_element()
            })
            .collect::<Vec<_>>()
    } else if state.view() == DatabaseMonitorView::Ownership {
        state
            .owners()
            .iter()
            .enumerate()
            .map(|(index, item)| {
                div()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(colors.subtle_border)
                    .bg(if index == selected {
                        colors.accent_muted
                    } else {
                        colors.panel
                    })
                    .child(format!(
                        "{:?} {} · owner {}",
                        item.kind, item.name, item.owner
                    ))
                    .into_any_element()
            })
            .collect::<Vec<_>>()
    } else {
        state
            .schema_grants()
            .iter()
            .enumerate()
            .map(|(index, item)| {
                div()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(colors.subtle_border)
                    .bg(if index == selected {
                        colors.accent_muted
                    } else {
                        colors.panel
                    })
                    .child(format!(
                        "{} · {} → {}{}",
                        item.schema,
                        item.grantee,
                        item.privilege,
                        if item.grantable { " (grantable)" } else { "" }
                    ))
                    .into_any_element()
            })
            .collect::<Vec<_>>()
    };
    div()
        .debug_selector(|| "postgres-objects-browser".into())
        .flex()
        .flex_1()
        .min_h_0()
        .flex_col()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .h(px(30.))
                .child(SectionLabel::new(match state.view() {
                    DatabaseMonitorView::Extensions => "POSTGRESQL EXTENSIONS",
                    DatabaseMonitorView::Partitions => "POSTGRESQL PARTITIONS",
                    DatabaseMonitorView::Policies => "POSTGRESQL POLICIES",
                    DatabaseMonitorView::Roles => "POSTGRESQL ROLES",
                    DatabaseMonitorView::Ownership => "POSTGRESQL OWNERSHIP",
                    DatabaseMonitorView::SchemaGrants => "EXPLICIT SCHEMA GRANTS",
                    _ => unreachable!(),
                }))
                .child(div().text_xs().child(match state.view() {
                    DatabaseMonitorView::Extensions => {
                        "j/k select · i install · d drop · n/p pages · r refresh"
                    }
                    DatabaseMonitorView::Partitions => {
                        "j/k select · d detach · n/p pages · r refresh"
                    }
                    DatabaseMonitorView::Policies => {
                        "j/k select · m rename · n/p pages · r refresh"
                    }
                    _ => "j/k select · n/p pages · r refresh",
                }))
                .child(div().flex_1())
                .when(state.view() == DatabaseMonitorView::Policies, |header| {
                    header.child(
                        Button::new("pg-policy-rename", "Rename selected")
                            .disabled(state.policies().is_empty())
                            .on_click(cx.listener(|shell, _, window, cx| {
                                shell.begin_policy_rename(window, cx)
                            })),
                    )
                })
                .child(
                    Button::new("pg-objects-prev", "Previous")
                        .tone(ButtonTone::Ghost)
                        .disabled(offset == 0 || state.objects_request().loading())
                        .on_click(cx.listener(move |shell, _, _, cx| {
                            shell.load_postgres_objects(offset.saturating_sub(100), cx)
                        })),
                )
                .child(
                    Button::new("pg-objects-next", "Next")
                        .tone(ButtonTone::Ghost)
                        .disabled(next.is_none() || state.objects_request().loading())
                        .on_click(cx.listener(move |shell, _, _, cx| {
                            if let Some(next) = next {
                                shell.load_postgres_objects(next, cx);
                            }
                        })),
                )
                .child(
                    Button::new("pg-objects-refresh", "Refresh")
                        .tone(ButtonTone::Ghost)
                        .disabled(state.objects_request().loading())
                        .on_click(cx.listener(move |shell, _, _, cx| {
                            shell.load_postgres_objects(offset, cx)
                        })),
                ),
        )
        .child(
            div()
                .id("postgres-objects-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .children(rows),
        )
        .children(state.objects_request().error().map(|message| {
            div()
                .px_3()
                .text_color(colors.danger)
                .child(message.to_string())
        }))
        .children(state.object_action_request().error().map(|message| {
            div()
                .px_3()
                .text_color(colors.danger)
                .child(message.to_string())
        }))
        .when(shell.policy_rename_active, |panel| {
            panel.child(
                div()
                    .px_3()
                    .py_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child("New policy name")
                    .child(shell.policy_rename_input.clone())
                    .child(
                        Button::new("pg-policy-rename-preview", "Preview rename").on_click(
                            cx.listener(|shell, _, window, cx| {
                                shell.preview_policy_rename(window, cx)
                            }),
                        ),
                    )
                    .child(
                        Button::new("pg-policy-rename-cancel", "Cancel").on_click(cx.listener(
                            |shell, _, _, cx| {
                                shell.policy_rename_active = false;
                                cx.notify();
                            },
                        )),
                    ),
            )
        })
        .children(state.object_preview().map(|preview| {
            div()
                .px_3()
                .py_2()
                .border_t_1()
                .border_color(colors.warning)
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("REVIEW POSTGRESQL CHANGE"),
                )
                .child(div().font_family("monospace").child(preview.sql.clone()))
                .child(
                    div()
                        .text_xs()
                        .text_color(colors.warning)
                        .child(preview.warning.clone()),
                )
                .child(div().text_xs().child(
                    if matches!(
                        preview.action,
                        sift_protocol::PostgresObjectAction::RenamePolicy { .. }
                    ) {
                        "x confirms production policy rename · Esc cancels"
                    } else {
                        "Enter applies exactly this preview · Esc cancels"
                    },
                ))
                .child(
                    Button::new(
                        "pg-objects-apply",
                        if matches!(
                            preview.action,
                            sift_protocol::PostgresObjectAction::RenamePolicy { .. }
                        ) {
                            "Confirm production policy rename"
                        } else {
                            "Apply reviewed change"
                        },
                    )
                    .disabled(state.object_action_request().loading())
                    .on_click(cx.listener(|shell, _, _, cx| shell.apply_postgres_object(true, cx))),
                )
        }))
}

fn render_postgres_replication(
    shell: &WorkspaceShell,
    cx: &mut Context<WorkspaceShell>,
) -> impl IntoElement {
    let colors = cx.theme().colors;
    let state = &shell.database_monitor;
    let report = state.replication();
    let selected = state.replication_selected();
    let mut index = 0;
    let mut rows = Vec::new();
    if let Some(report) = report {
        for sender in &report.senders {
            rows.push(
                div()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(colors.subtle_border)
                    .bg(if index == selected {
                        colors.accent_muted
                    } else {
                        colors.panel
                    })
                    .child(format!(
                        "Sender {} · {} · {} · sync {} · write/flush/replay lag {} / {} / {} ms",
                        sender.pid,
                        sender.application_name,
                        sender.state,
                        sender.sync_state,
                        sender
                            .write_lag_ms
                            .map_or_else(|| "unknown".into(), |v| v.to_string()),
                        sender
                            .flush_lag_ms
                            .map_or_else(|| "unknown".into(), |v| v.to_string()),
                        sender
                            .replay_lag_ms
                            .map_or_else(|| "unknown".into(), |v| v.to_string())
                    ))
                    .into_any_element(),
            );
            index += 1;
        }
        if let Some(receiver) = &report.receiver {
            rows.push(
                div()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(colors.subtle_border)
                    .bg(if index == selected {
                        colors.accent_muted
                    } else {
                        colors.panel
                    })
                    .child(format!(
                        "WAL receiver · {} · received {} · latest {}",
                        receiver.status,
                        receiver.received_lsn.as_deref().unwrap_or("unknown"),
                        receiver.latest_end_lsn.as_deref().unwrap_or("unknown")
                    ))
                    .into_any_element(),
            );
            index += 1;
        }
        for slot in &report.slots {
            rows.push(
                div()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(colors.subtle_border)
                    .bg(if index == selected {
                        colors.accent_muted
                    } else {
                        colors.panel
                    })
                    .child(format!(
                        "Slot {} · {} · database {} · {} · restart {} · confirmed {}",
                        slot.name,
                        slot.slot_type,
                        slot.database.as_deref().unwrap_or("none"),
                        if slot.active { "active" } else { "inactive" },
                        slot.restart_lsn.as_deref().unwrap_or("unknown"),
                        slot.confirmed_flush_lsn.as_deref().unwrap_or("unknown")
                    ))
                    .into_any_element(),
            );
            index += 1;
        }
    }
    div()
        .debug_selector(|| "postgres-replication-browser".into())
        .flex()
        .flex_1()
        .min_h_0()
        .flex_col()
        .child(
            div()
                .h(px(30.))
                .px_3()
                .flex()
                .items_center()
                .gap_2()
                .child(SectionLabel::new("POSTGRESQL REPLICATION"))
                .child(div().text_xs().child(
                    "Snapshot · lag is not catch-up ETA · j/k select · r refresh · Esc exit",
                ))
                .child(div().flex_1())
                .child(
                    Button::new("pg-replication-refresh", "Refresh")
                        .tone(ButtonTone::Ghost)
                        .disabled(state.replication_request().loading())
                        .on_click(
                            cx.listener(|shell, _, _, cx| shell.load_postgres_replication(cx)),
                        ),
                ),
        )
        .child(
            div()
                .id("postgres-replication-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .children(rows),
        )
        .children(
            report
                .filter(|report| report.senders_truncated || report.slots_truncated)
                .map(|_| {
                    div()
                        .px_3()
                        .text_color(colors.warning)
                        .child("Snapshot truncated at 200 senders or slots")
                }),
        )
        .children((report.is_some() && index == 0).then(|| {
            div()
                .p_4()
                .text_center()
                .child("No replication senders, receiver, or slots reported.")
        }))
        .children(state.replication_request().error().map(|message| {
            div()
                .px_3()
                .text_color(colors.danger)
                .child(message.to_string())
        }))
}

fn render_postgres_statistics(
    shell: &WorkspaceShell,
    cx: &mut Context<WorkspaceShell>,
) -> impl IntoElement {
    let colors = cx.theme().colors;
    let state = &shell.database_monitor;
    let report = state.statistics();
    let offset = state.statistics_offset();
    let next = report.and_then(|report| report.next_offset);
    let selected = state.statistics_selected();
    div().debug_selector(|| "postgres-statistics-browser".into()).flex().flex_1().min_h_0().flex_col()
        .child(div().h(px(30.)).px_3().flex().items_center().gap_2()
            .child(SectionLabel::new("POSTGRESQL STATISTICS"))
            .child(div().text_xs().child("Cumulative snapshot · j/k select · n/p pages · r refresh"))
            .child(div().flex_1())
            .child(Button::new("pg-statistics-prev", "Previous").tone(ButtonTone::Ghost)
                .disabled(offset == 0 || state.statistics_request().loading())
                .on_click(cx.listener(move |shell, _, _, cx| shell.load_postgres_statistics(offset.saturating_sub(100), cx))))
            .child(Button::new("pg-statistics-next", "Next").tone(ButtonTone::Ghost)
                .disabled(next.is_none() || state.statistics_request().loading())
                .on_click(cx.listener(move |shell, _, _, cx| { if let Some(next) = next { shell.load_postgres_statistics(next, cx); } })))
            .child(Button::new("pg-statistics-refresh", "Refresh").tone(ButtonTone::Ghost)
                .disabled(state.statistics_request().loading())
                .on_click(cx.listener(move |shell, _, _, cx| shell.load_postgres_statistics(offset, cx)))))
        .children(report.map(|report| {
            let db = &report.database;
            div().px_3().py_2().border_b_1().border_color(colors.subtle_border)
                .child(format!("Database {} · {} backends · {} commits · {} rollbacks · {} blocks read / {} hit · reset {}",
                    db.database, db.backends, db.commits, db.rollbacks, db.blocks_read, db.blocks_hit,
                    db.stats_reset.as_deref().unwrap_or("unknown")))
                .child(div().text_xs().child(format!("Tuples returned {} · fetched {} · inserted {} · updated {} · deleted {}",
                    db.tuples_returned, db.tuples_fetched, db.tuples_inserted, db.tuples_updated, db.tuples_deleted)))
        }))
        .child(div().id("postgres-statistics-list").flex_1().min_h_0().overflow_y_scroll()
            .children(report.into_iter().flat_map(|report| report.tables.iter().enumerate()).map(|(index, table)| {
                div().px_3().py_2().border_b_1().border_color(colors.subtle_border)
                    .bg(if index == selected { colors.accent_muted } else { colors.panel })
                    .child(format!("{}.{} · seq scans {} · index scans {} · live/dead estimate {} / {}",
                        table.schema, table.table, table.sequential_scans,
                        table.index_scans.map_or_else(|| "unavailable".into(), |v| v.to_string()),
                        table.live_tuples_estimate, table.dead_tuples_estimate))
                    .child(div().text_xs().child(format!("Vacuum {} · auto {} · analyze {} · auto {}",
                        table.last_vacuum.as_deref().unwrap_or("never"),
                        table.last_autovacuum.as_deref().unwrap_or("never"),
                        table.last_analyze.as_deref().unwrap_or("never"),
                        table.last_autoanalyze.as_deref().unwrap_or("never"))))
            })))
        .children(report.filter(|report| report.tables.is_empty()).map(|_| div().p_4().text_center().child("No accessible user-table statistics on this page.")))
        .children(state.statistics_request().error().map(|message| div().px_3().text_color(colors.danger).child(message.to_string())))
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

fn render_database_deadlock_history(
    shell: &WorkspaceShell,
    cx: &mut Context<WorkspaceShell>,
) -> gpui::AnyElement {
    let colors = cx.theme().colors;
    let request = shell.database_monitor.deadlock_request();
    let rows = shell
        .database_monitor
        .deadlocks()
        .iter()
        .enumerate()
        .map(|(index, event)| {
            let participants = event
                .participants
                .iter()
                .map(|participant| {
                    let role = if participant.victim {
                        "victim"
                    } else {
                        "session"
                    };
                    format!(
                        "{role} #{} · {} · {} · {} ms",
                        participant.process_id,
                        participant.lock_mode.as_deref().unwrap_or("unknown mode"),
                        participant
                            .wait_resource
                            .as_deref()
                            .unwrap_or("unknown resource"),
                        participant.wait_ms.unwrap_or(0),
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            div()
                .id(("database-deadlock-event", index))
                .debug_selector(move || format!("database-deadlock-event-{index}"))
                .p_2()
                .border_b_1()
                .border_color(colors.subtle_border)
                .flex()
                .flex_col()
                .gap_1()
                .child(SectionLabel::new(format!(
                    "{} · {} session{}{}",
                    event.occurred_at.format("%Y-%m-%d %H:%M:%S UTC"),
                    event.participants.len(),
                    if event.participants.len() == 1 {
                        ""
                    } else {
                        "s"
                    },
                    if event.participants_truncated {
                        "+"
                    } else {
                        ""
                    },
                )))
                .child(div().text_xs().font_family("monospace").child(participants))
        })
        .collect::<Vec<_>>();
    div()
        .flex()
        .flex_1()
        .min_h_0()
        .flex_col()
        .child(
            div()
                .h(px(30.))
                .px_3()
                .flex()
                .items_center()
                .gap_2()
                .child(SectionLabel::new("RETAINED DEADLOCKS · SQL SERVER"))
                .child(div().flex_1())
                .child(
                    Button::new(
                        "refresh-database-deadlocks",
                        if request.loading() {
                            "Loading…"
                        } else {
                            "Refresh"
                        },
                    )
                    .tone(ButtonTone::Ghost)
                    .disabled(request.loading())
                    .on_click(cx.listener(|shell, _, _, cx| shell.load_database_deadlocks(cx))),
                ),
        )
        .child(
            div()
                .id("database-deadlock-history-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .children(rows),
        )
        .children(request.error().map(|message| {
            div()
                .p_2()
                .text_color(colors.danger)
                .child(message.to_string())
        }))
        .when(
            shell.database_monitor.deadlocks().is_empty()
                && !request.loading()
                && request.error().is_none(),
            |panel| {
                panel.child(
                    div()
                        .p_4()
                        .text_center()
                        .child("No retained deadlock events."),
                )
            },
        )
        .into_any_element()
}

fn render_database_process_row(
    row: DatabaseProcessRow,
    expanded: bool,
    focused: bool,
    kill_available: bool,
    alert: Option<DatabaseAlertKind>,
    cx: &mut Context<WorkspaceShell>,
) -> gpui::AnyElement {
    let colors = cx.theme().colors;
    let process = row.process;
    let details_process = process.clone();
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
        .flex_col()
        .min_w_0()
        .overflow_hidden()
        .border_b_1()
        .border_color(colors.subtle_border)
        .child(
            div()
                .id(("database-process-summary", process_id as usize))
                .h(px(30.))
                .px_3()
                .flex()
                .items_center()
                .gap_3()
                .cursor_pointer()
                .when(row.block_depth > 0, |row| row.bg(colors.warning_muted))
                .when(alert == Some(DatabaseAlertKind::DeadlockRisk), |row| {
                    row.bg(colors.danger_muted)
                })
                .when(expanded, |row| row.bg(colors.active_surface))
                .when(
                    focused
                        && !expanded
                        && row.block_depth == 0
                        && alert != Some(DatabaseAlertKind::DeadlockRisk),
                    |row| row.bg(colors.accent_muted),
                )
                .when(focused, |row| row.border_l_2().border_color(colors.accent))
                .on_click(cx.listener(move |shell, _, window, cx| {
                    shell.select_database_process(process_id, cx);
                    shell.automation_focus_handle.focus(window, cx);
                }))
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
                .child(div().flex_1().min_w_0().truncate().child(user_database))
                .child(div().flex_1().min_w_0().truncate().child(state))
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
                        .child(statement),
                )
                .children(alert.map(|alert| {
                    Badge::new(alert.label()).tone(if alert == DatabaseAlertKind::DeadlockRisk {
                        Tone::Danger
                    } else {
                        Tone::Warning
                    })
                }))
                .child(
                    div().w(px(84.)).flex().justify_end().child(
                        Button::new(("terminate-process", process_id as usize), "Terminate")
                            .tone(ButtonTone::DangerGhost)
                            .disabled(!kill_available)
                            .on_click(cx.listener(move |shell, _, _, cx| {
                                shell.request_terminate_process(process_id, cx)
                            })),
                    ),
                ),
        )
        .children(expanded.then(|| render_database_process_details(details_process, cx)))
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
    let lock = process.lock_wait.map_or_else(
        || "none".to_string(),
        |lock| {
            let age = lock.started_at.map_or_else(String::new, |started| {
                let seconds = chrono::Utc::now()
                    .signed_duration_since(started)
                    .num_seconds()
                    .max(0);
                format!(" · waiting {seconds}s")
            });
            format!("{} on {}{}", lock.mode, lock.resource, age)
        },
    );
    let held_count = process.held_locks.len();
    let held_locks = process
        .held_locks
        .iter()
        .map(|lock| format!("{} on {}", lock.mode, lock.resource))
        .collect::<Vec<_>>()
        .join("\n");
    let held_label = if process.held_locks_truncated {
        format!("HELD LOCKS {held_count}+")
    } else {
        format!("HELD LOCKS {held_count}")
    };
    let metadata = format!(
        "{:?} · {} @ {} · {} · wait: {} · lock: {} · blocked by: {} · started: {}{}",
        process.engine,
        process.user.unwrap_or_else(|| "unknown user".into()),
        process
            .database
            .unwrap_or_else(|| "unknown database".into()),
        process.state.unwrap_or_else(|| "unknown state".into()),
        process.wait.unwrap_or_else(|| "none".into()),
        lock,
        blockers,
        started,
        elapsed.map_or_else(String::new, |elapsed| format!(" · elapsed: {elapsed}")),
    );

    div()
        .debug_selector(move || format!("database-process-details-{process_id}"))
        .flex_none()
        .border_t_1()
        .border_color(colors.subtle_border)
        .bg(colors.elevated_surface)
        .p_3()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .items_start()
                .gap_2()
                .child(SectionLabel::new(format!("PROCESS {process_id}")))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .whitespace_normal()
                        .text_xs()
                        .child(metadata),
                )
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
                .debug_selector(move || format!("database-process-held-locks-{process_id}"))
                .flex()
                .items_start()
                .gap_2()
                .child(SectionLabel::new(held_label))
                .child(
                    div()
                        .id(("database-process-held-lock-list", process_id as usize))
                        .max_h(px(112.))
                        .overflow_y_scroll()
                        .font_family("monospace")
                        .text_xs()
                        .whitespace_normal()
                        .child(if held_locks.is_empty() {
                            "None".to_string()
                        } else {
                            held_locks
                        }),
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
            transaction_started_at: None,
            state_changed_at: None,
            wait: None,
            blocked_by,
            lock_wait: None,
            held_locks: Vec::new(),
            held_locks_truncated: false,
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
