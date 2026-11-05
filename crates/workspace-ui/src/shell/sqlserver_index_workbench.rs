use super::*;
use sift_protocol::{
    SqlServerIndexAction as Action, SqlServerIndexReport as Report,
    SqlServerIndexRequest as Request,
};
pub(super) fn phrase(request: &Request, report: &Report) -> String {
    format!(
        "{} {} {}.{} INDEX {}",
        match request.action {
            Action::Rebuild => "REBUILD",
            Action::Reorganize => "REORGANIZE",
            Action::UpdateStatistics => "UPDATE STATISTICS",
        },
        report.database,
        request.schema,
        request.table,
        request.index
    )
}
impl WorkspaceShell {
    fn index_request(&self, cx: &App) -> Result<Request, String> {
        let action = match self.maintenance.mode {
            MaintenanceMode::IndexRebuild => Action::Rebuild,
            MaintenanceMode::IndexReorganize => Action::Reorganize,
            MaintenanceMode::IndexStatistics => Action::UpdateStatistics,
            _ => return Err("Choose index maintenance".into()),
        };
        let request = Request {
            schema: self.maintenance.database.read(cx).text().into(),
            table: self.maintenance.archive.read(cx).text().into(),
            index: self.maintenance.backup_set.read(cx).text().into(),
            action,
            apply: false,
            production_confirmed: false,
            backup_verified: false,
            precondition: None,
        };
        sift_core::sqlserver_index_maintenance::sql(&request).map_err(str::to_owned)?;
        Ok(request)
    }
    pub(super) fn dispatch_index_maintenance(&mut self, apply: bool, cx: &mut Context<Self>) {
        if self.maintenance.pending.is_some()
            || !self.require_operation(
                sift_protocol::OperationKind::MaintainSqlServerIndex,
                "Maintain SQL Server index",
                cx,
            )
        {
            return;
        }
        let result = (|| -> Result<_, String> {
            let mut request = self.index_request(cx)?;
            if apply {
                let (reviewed, report, source) = self
                    .maintenance
                    .index_preview
                    .as_ref()
                    .ok_or("Preview the index before applying")?;
                if self.current_monitor_source(source.request_id).as_ref() != Some(source)
                    || &request != reviewed
                    || report.applied
                    || report.sql
                        != sift_core::sqlserver_index_maintenance::sql(&request)
                            .map_err(str::to_owned)?
                {
                    return Err("Source or inputs changed; review the index again".into());
                }
                if !self.maintenance.backup_verified {
                    return Err("A verified backup acknowledgment is required".into());
                }
                let expected = phrase(&request, report);
                if self.maintenance.confirmation.read(cx).text() != expected {
                    return Err(format!("Type {expected} to confirm"));
                }
                request.apply = true;
                request.production_confirmed = true;
                request.backup_verified = true;
                request.precondition = Some(report.precondition.clone());
            }
            let source = self
                .current_monitor_source(uuid::Uuid::new_v4())
                .ok_or("Connect the original source before index maintenance")?;
            Ok((request, source))
        })();
        let (request, source) = match result {
            Ok(result) => result,
            Err(message) => {
                self.maintenance.message = Some(message);
                cx.notify();
                return;
            }
        };
        let generation = self.maintenance.begin(if apply {
            MaintenanceAction::Apply
        } else {
            MaintenanceAction::Preview
        });
        if !apply {
            self.maintenance.index_preview = None;
            self.maintenance.backup_verified = false;
            self.maintenance.last_index = None;
        }
        self.maintenance.index_pending = Some((request.clone(), source.clone()));
        if !self.executor_sender.as_ref().is_some_and(|sender| {
            sender
                .send(ExecutorCommand::SqlServerIndexMaintenance {
                    source,
                    generation,
                    request,
                })
                .is_ok()
        }) {
            self.maintenance.invalidate();
            self.maintenance.message = Some("Database executor is unavailable".into());
        }
        cx.notify();
    }
    pub(super) fn finish_index_maintenance(
        &mut self,
        source: MonitorActionSource,
        generation: u64,
        apply: bool,
        result: Result<Report, String>,
        cx: &mut Context<Self>,
    ) {
        let phase = if apply {
            MaintenanceAction::Apply
        } else {
            MaintenanceAction::Preview
        };
        if !self.maintenance.accepts(generation, phase)
            || self.current_monitor_source(source.request_id).as_ref() != Some(&source)
            || !self
                .maintenance
                .index_pending
                .as_ref()
                .is_some_and(|(_, pending)| pending == &source)
        {
            return;
        }
        self.maintenance.pending = None;
        let (mut request, _) = self.maintenance.index_pending.take().unwrap();
        match result {
            Ok(report) if !apply && !report.applied => {
                request.apply = false;
                self.maintenance.index_preview = Some((request, report, source));
            }
            Ok(report) => {
                self.maintenance.index_preview = None;
                self.maintenance.backup_verified = false;
                self.maintenance.message = Some(
                    if report.applied {
                        "Native index command acknowledged"
                    } else {
                        "Native command was not applied"
                    }
                    .into(),
                );
                self.maintenance.last_index = Some(report);
            }
            Err(message) => {
                self.maintenance.index_preview = None;
                self.maintenance.backup_verified = false;
                self.maintenance.message = Some(message);
            }
        }
        cx.notify();
    }
}
pub(super) fn render(shell: &WorkspaceShell, cx: &mut Context<WorkspaceShell>) -> AnyElement {
    let state = &shell.maintenance;
    let busy = state.pending.is_some();
    let mut panel=div().flex().flex_col().gap_2()
        .child("One explicit enabled ordinary rowstore index in the connected user database. e offline rebuild · o reorganize · u FULLSCAN statistics · p preview · k backup acknowledgment · a apply")
        .child(div().child("Schema").child(state.database.clone()))
        .child(div().child("Table").child(state.archive.clone()))
        .child(div().child("Index").child(state.backup_set.clone()))
        .child(Button::new("index-maintenance-preview","Preview native target and SQL").disabled(busy).on_click(cx.listener(|shell,_,_,cx|shell.dispatch_index_maintenance(false,cx))));
    if let Some((request, report, _)) = &state.index_preview {
        panel = panel
            .child(div().font_family("monospace").child(report.sql.clone()))
            .child(report.warning.clone())
            .child(
                Button::new(
                    "index-maintenance-backup",
                    if state.backup_verified {
                        "Verified backup: yes (k)"
                    } else {
                        "Verified backup: no (k)"
                    },
                )
                .disabled(busy)
                .on_click(cx.listener(|shell, _, _, cx| {
                    shell.maintenance.backup_verified = !shell.maintenance.backup_verified;
                    cx.notify();
                })),
            )
            .child(format!("Type {}", phrase(request, report)))
            .child(state.confirmation.clone())
            .child(
                Button::new(
                    "index-maintenance-apply",
                    "Apply confirmed index maintenance",
                )
                .disabled(busy)
                .tone(ButtonTone::Danger)
                .on_click(
                    cx.listener(|shell, _, _, cx| shell.dispatch_index_maintenance(true, cx)),
                ),
            );
    }
    if let Some(report) = &state.last_index {
        panel = panel
            .child(report.sql.clone())
            .child(report.warning.clone());
    }
    panel.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    #[gpui::test]
    fn vim_index_review_requires_backup_phrase_and_original_source(cx: &mut TestAppContext) {
        let window = super::super::tests::shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let (sender, mut commands) = ExecutorSender::channel(16);
        workspace.update_in(&mut cx, |shell, window, cx| {
            shell.executor_sender = Some(sender);
            shell.selected_instance_id = Some("fixture".into());
            shell.lifecycle.tenants = vec![crate::TenantNavEntry {
                id: sift_api_types::TenantId(1),
                name: "local".into(),
                rooms: vec![],
                connections: vec![ConnectionNavEntry {
                    id: 7,
                    tenant_id: 1,
                    name: "SQL Server".into(),
                    provider_id: sift_protocol::Engine::SqlServer.provider_id(),
                    tags: vec![],
                }],
            }];
            shell.connection_status = ConnectionStatus::Connected {
                profile_id: 7,
                name: "SQL Server".into(),
            };
            shell.active_query_connection = Some((
                7,
                sift_protocol::SessionId(4),
                sift_protocol::ConnectionId(10),
            ));
            shell.active_bottom_tool = BottomTool::Monitor;
            shell.bottom_dock.presentation.open = true;
            shell
                .database_monitor
                .set_view(DatabaseMonitorView::Maintenance);
            shell.automation_focus_handle.focus(window, cx);
            cx.notify();
        });
        for (key, action) in [
            ("e", Action::Rebuild),
            ("o", Action::Reorganize),
            ("u", Action::UpdateStatistics),
        ] {
            cx.simulate_keystrokes(key);
            workspace.update(&mut cx, |shell, cx| {
                shell
                    .maintenance
                    .database
                    .update(cx, |input, cx| input.set_text("dbo", cx));
                shell
                    .maintenance
                    .archive
                    .update(cx, |input, cx| input.set_text("items", cx));
                shell
                    .maintenance
                    .backup_set
                    .update(cx, |input, cx| input.set_text("index] name", cx));
            });
            cx.simulate_keystrokes("p");
            let (source, generation, request) = match commands.try_recv().unwrap() {
                ExecutorCommand::SqlServerIndexMaintenance {
                    source,
                    generation,
                    request,
                } => (source, generation, request),
                _ => panic!("expected source-bound index preview"),
            };
            assert_eq!(request.action, action);
            assert!(!request.apply);
            let report = Report {
                sql: sift_core::sqlserver_index_maintenance::sql(&request).unwrap(),
                applied: false,
                database: "db".into(),
                precondition: "a".repeat(64),
                warning: "Review native writes".into(),
            };
            let confirm = phrase(&request, &report);
            workspace.update(&mut cx, |shell, cx| {
                let mut wrong = source.clone();
                wrong.connection_id = sift_protocol::ConnectionId(99);
                shell.on_executor_event(
                    ExecutorEvent::SqlServerIndexFinished {
                        source: wrong,
                        generation,
                        apply: false,
                        result: Ok(report.clone()),
                    },
                    cx,
                );
                assert!(shell.maintenance.index_preview.is_none());
                shell.on_executor_event(
                    ExecutorEvent::SqlServerIndexFinished {
                        source: source.clone(),
                        generation,
                        apply: false,
                        result: Ok(report),
                    },
                    cx,
                );
                shell
                    .maintenance
                    .confirmation
                    .update(cx, |input, cx| input.set_text(confirm, cx));
            });
            cx.simulate_keystrokes("a");
            assert!(
                commands.try_recv().is_err(),
                "backup acknowledgment is mandatory"
            );
            cx.simulate_keystrokes("k a");
            let (source, generation) = match commands.try_recv().unwrap() {
                ExecutorCommand::SqlServerIndexMaintenance {
                    source,
                    generation,
                    request,
                } => {
                    assert!(
                        request.apply && request.production_confirmed && request.backup_verified
                    );
                    assert_eq!(
                        request.precondition.as_deref(),
                        Some("a".repeat(64).as_str())
                    );
                    (source, generation)
                }
                _ => panic!("expected confirmed index command"),
            };
            workspace.update(&mut cx, |shell, cx| {
                shell.on_executor_event(
                    ExecutorEvent::SqlServerIndexFinished {
                        source,
                        generation,
                        apply: true,
                        result: Err("native failure may retain partial work".into()),
                    },
                    cx,
                )
            });
        }
        cx.simulate_keystrokes("e p");
        let (source, generation, request) = match commands.try_recv().unwrap() {
            ExecutorCommand::SqlServerIndexMaintenance {
                source,
                generation,
                request,
            } => (source, generation, request),
            _ => panic!("expected preview"),
        };
        workspace.update(&mut cx, |shell, cx| {
            let report = Report {
                sql: sift_core::sqlserver_index_maintenance::sql(&request).unwrap(),
                applied: false,
                database: "db".into(),
                precondition: "b".repeat(64),
                warning: String::new(),
            };
            shell.on_executor_event(
                ExecutorEvent::SqlServerIndexFinished {
                    source,
                    generation,
                    apply: false,
                    result: Ok(report.clone()),
                },
                cx,
            );
            shell.maintenance.confirmation.update(cx, |input, cx| {
                input.set_text(phrase(&request, &report), cx)
            });
            shell.maintenance.backup_verified = true;
            shell.active_query_connection = Some((
                7,
                sift_protocol::SessionId(4),
                sift_protocol::ConnectionId(11),
            ));
        });
        cx.simulate_keystrokes("a");
        assert!(commands.try_recv().is_err(), "reconnect invalidates review");
    }
}
