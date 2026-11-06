//! Source-bound inspection; no grants or selected-user workloads are executed.
use super::*;
use sift_protocol::{SqlServerPrivilegeObject, SqlServerPrivilegeQuery, SqlServerPrivilegeReport};
pub(super) struct State {
    pub active: bool,
    pub input: Option<Entity<TextInput>>,
    _input_subscription: Option<gpui::Subscription>,
    pub principal: Option<String>,
    pub source: Option<MonitorActionSource>,
    pub pending: bool,
    pub offset: u32,
    pub report: Option<SqlServerPrivilegeReport>,
    pub error: Option<String>,
    pub selected: usize,
    pub scroll: UniformListScrollHandle,
    pub panel_scroll: gpui::ScrollHandle,
}
impl Default for State {
    fn default() -> Self {
        Self {
            active: false,
            input: None,
            _input_subscription: None,
            principal: None,
            source: None,
            pending: false,
            offset: 0,
            report: None,
            error: None,
            selected: 0,
            scroll: UniformListScrollHandle::new(),
            panel_scroll: gpui::ScrollHandle::new(),
        }
    }
}
impl WorkspaceShell {
    pub(super) fn begin_sqlserver_privileges(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(source) = self.current_monitor_source(uuid::Uuid::new_v4()) else {
            return;
        };
        let input = cx.new(|cx| {
            TextInput::new("", "Blank: fresh connected actor; or SQL database user", cx)
                .aria_label("SQL Server privilege subject")
        });
        let subscription = cx.subscribe_in(
            &input,
            window,
            |shell, emitter, event: &TextInputEvent, window, cx| {
                if *event == TextInputEvent::Submitted
                    && shell.sqlserver_privilege_matrix.active
                    && shell.sqlserver_privilege_matrix.input.as_ref() == Some(emitter)
                {
                    shell.submit_sqlserver_privilege_subject(window, cx);
                }
            },
        );
        self.sqlserver_privilege_matrix = State {
            active: true,
            input: Some(input.clone()),
            _input_subscription: Some(subscription),
            source: Some(source),
            ..Default::default()
        };
        input.focus_handle(cx).focus(window, cx);
        cx.notify();
    }
    fn submit_sqlserver_privilege_subject(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = self.sqlserver_privilege_matrix.input.take() else {
            return;
        };
        let name = input.read(cx).text().to_string();
        self.sqlserver_privilege_matrix._input_subscription = None;
        self.sqlserver_privilege_matrix.principal = (!name.is_empty()).then_some(name);
        self.automation_focus_handle.focus(window, cx);
        self.load_sqlserver_privileges(0, cx);
    }
    pub(super) fn load_sqlserver_privileges(&mut self, offset: u32, cx: &mut Context<Self>) {
        if self.sqlserver_privilege_matrix.pending
            || !self.require_operation(
                sift_protocol::OperationKind::ReadSqlServerPrivileges,
                "Inspect SQL Server effective permissions",
                cx,
            )
        {
            return;
        }
        if self
            .sqlserver_privilege_matrix
            .source
            .as_ref()
            .is_some_and(|source| {
                self.current_monitor_source(source.request_id).as_ref() != Some(source)
            })
        {
            self.sqlserver_privilege_matrix.error =
                Some("Source changed; reopen the privilege matrix".into());
            cx.notify();
            return;
        }
        let Some(source) = self.current_monitor_source(uuid::Uuid::new_v4()) else {
            return;
        };
        let query = SqlServerPrivilegeQuery {
            principal: self.sqlserver_privilege_matrix.principal.clone(),
            offset,
            limit: Some(100),
        };
        let state = &mut self.sqlserver_privilege_matrix;
        state.source = Some(source.clone());
        state.pending = true;
        state.offset = offset;
        state.report = None;
        state.error = None;
        state.selected = 0;
        state.scroll.scroll_to_item(0, ScrollStrategy::Top);
        state.panel_scroll.set_offset(gpui::point(px(0.), px(0.)));
        if !self.executor_sender.as_ref().is_some_and(|sender| {
            sender
                .send(ExecutorCommand::ReadSqlServerPrivileges { source, query })
                .is_ok()
        }) {
            self.sqlserver_privilege_matrix.pending = false;
            self.sqlserver_privilege_matrix.error = Some("Database executor is unavailable".into());
        }
        cx.notify();
    }
    pub(super) fn finish_sqlserver_privileges(
        &mut self,
        source: MonitorActionSource,
        result: Result<SqlServerPrivilegeReport, String>,
        cx: &mut Context<Self>,
    ) {
        if !self.sqlserver_privilege_matrix.active
            || !self.sqlserver_privilege_matrix.pending
            || self.sqlserver_privilege_matrix.source.as_ref() != Some(&source)
            || self.current_monitor_source(source.request_id).as_ref() != Some(&source)
        {
            return;
        }
        self.sqlserver_privilege_matrix.pending = false;
        match result {
            Ok(report) => self.sqlserver_privilege_matrix.report = Some(report),
            Err(error) => self.sqlserver_privilege_matrix.error = Some(error),
        }
        cx.notify();
    }
    pub(super) fn sqlserver_privileges_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.modifiers.modified() {
            return;
        }
        let key = event.keystroke.key.as_str();
        if self.sqlserver_privilege_matrix.input.is_some() {
            match key {
                "enter" => self.submit_sqlserver_privilege_subject(window, cx),
                "escape" => {
                    self.sqlserver_privilege_matrix = Default::default();
                    self.automation_focus_handle.focus(window, cx);
                }
                _ => return,
            }
        } else {
            match key {
                "escape" => self.sqlserver_privilege_matrix = Default::default(),
                "e" => self.begin_sqlserver_privileges(window, cx),
                "r" => self.load_sqlserver_privileges(self.sqlserver_privilege_matrix.offset, cx),
                "n" => {
                    if let Some(offset) = self
                        .sqlserver_privilege_matrix
                        .report
                        .as_ref()
                        .and_then(|r| r.next_offset)
                    {
                        self.load_sqlserver_privileges(offset, cx);
                    }
                }
                "p" => self.load_sqlserver_privileges(
                    self.sqlserver_privilege_matrix.offset.saturating_sub(100),
                    cx,
                ),
                "j" | "k" => {
                    let state = &mut self.sqlserver_privilege_matrix;
                    let count = state.report.as_ref().map_or(0, |r| r.objects.len());
                    if count > 0 {
                        state.selected = state
                            .selected
                            .saturating_add_signed(if key == "j" { 1 } else { -1 })
                            .min(count - 1);
                        state
                            .scroll
                            .scroll_to_item(state.selected, ScrollStrategy::Center);
                        state
                            .panel_scroll
                            .set_offset(gpui::point(px(0.), -state.panel_scroll.max_offset().y));
                    }
                }
                _ => return,
            }
        }
        cx.stop_propagation();
        cx.notify();
    }
}
fn display_identifier(value: &str) -> String {
    let value = value
        .chars()
        .map(|c| {
            if c.is_control() {
                c.escape_default().to_string()
            } else {
                c.to_string()
            }
        })
        .collect::<String>();
    format!("[{}]", value.replace(']', "]]"))
}
fn label(object: &SqlServerPrivilegeObject) -> String {
    format!(
        "{:?} · {}{}{}",
        object.kind,
        object
            .schema
            .as_ref()
            .map_or_else(String::new, |s| format!("{}.", display_identifier(s))),
        object
            .relation
            .as_ref()
            .map_or_else(String::new, |s| format!("{}.", display_identifier(s))),
        display_identifier(&object.name)
    )
}

fn checks(object: &SqlServerPrivilegeObject) -> String {
    object
        .checks
        .iter()
        .map(|c| {
            format!(
                "{}: {}",
                c.privilege,
                match c.allowed {
                    Some(true) => "allowed",
                    Some(false) => "denied",
                    None => "unavailable",
                }
            )
        })
        .collect::<Vec<_>>()
        .join(" · ")
}
pub(super) fn render(shell: &WorkspaceShell, cx: &mut Context<WorkspaceShell>) -> AnyElement {
    let state = &shell.sqlserver_privilege_matrix;
    let colors = cx.theme().colors;
    if let Some(input) = &state.input {
        return div().flex().flex_col().gap_2().p_3().child(SectionLabel::new("SQL SERVER EFFECTIVE PERMISSIONS")).child("Enter a SQL database user, or leave blank for the fresh connected actor. Enter inspect · Escape return").child(input.clone()).into_any_element();
    }
    let count = state.report.as_ref().map_or(0, |r| r.objects.len());
    div()
        .id("sqlserver-privilege-matrix")
        .overflow_y_scroll()
        .track_scroll(&state.panel_scroll)
        .debug_selector(|| "sqlserver-privilege-matrix".into())
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .gap_2()
        .p_3()
        .child(SectionLabel::new("NATIVE SQL SERVER PERMISSION MATRIX"))
        .child(format!(
            "Offset {} · j/k objects · n/p pages · r reload · e subject · Escape return",
            state.offset
        ))
        .children(
            state
                .pending
                .then(|| div().child("Reading effective permissions on a dedicated connection…")),
        )
        .children(state.error.as_ref().map(|e| ErrorBanner::new(e.clone())))
        .children(state.report.as_ref().map(|r| {
            div().text_sm().whitespace_normal().child(format!(
                "{} · fresh actor {} · subject {} · {}",
                r.database,
                r.actor,
                r.principal,
                if r.database_user_token {
                    "database-user token; login/server rights excluded"
                } else {
                    "fresh connected actor token"
                }
            ))
        }))
        .children(state.report.as_ref().map(|r| {
            div()
                .text_xs()
                .whitespace_normal()
                .text_color(colors.muted_text)
                .child(r.warning.clone())
        }))
        .child(
            uniform_list(
                "sqlserver-privilege-objects",
                count,
                cx.processor(|shell, range: Range<usize>, _, cx| {
                    let Some(report) = &shell.sqlserver_privilege_matrix.report else {
                        return vec![];
                    };
                    range
                        .filter_map(|index| {
                            report.objects.get(index).map(|o| {
                                div()
                                    .h(px(54.))
                                    .overflow_hidden()
                                    .text_sm()
                                    .when(
                                        index == shell.sqlserver_privilege_matrix.selected,
                                        |row| row.bg(cx.theme().colors.active_surface),
                                    )
                                    .child(div().child(label(o)))
                                    .child(div().text_xs().child(checks(o)))
                            })
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(&state.scroll)
            .h(px(180.))
            .flex_shrink_0(),
        )
        .children(
            state
                .report
                .as_ref()
                .and_then(|r| r.objects.get(state.selected))
                .map(|o| {
                    div().text_sm().whitespace_normal().child(format!(
                        "{} · {}",
                        label(o),
                        checks(o)
                    ))
                }),
        )
        .children(
            state
                .report
                .as_ref()
                .filter(|r| r.page_limit_reached)
                .map(|_| ErrorBanner::new("Matrix page cap reached; more objects are omitted.")),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    fn report() -> SqlServerPrivilegeReport {
        serde_json::from_value(serde_json::json!({"database":"db","actor":"actor","principal":"subject","principal_id":5,"database_user_token":true,"objects":[{"kind":"table","object_id":1,"column_number":null,"schema":"dbo","relation":null,"name":"first","checks":[{"privilege":"SELECT","allowed":null}]},{"kind":"column","object_id":1,"column_number":1,"schema":"dbo","relation":"first","name":"value","checks":[{"privilege":"SELECT","allowed":true}]}],"next_offset":100,"page_limit_reached":false,"warning":"database-user token; server rights excluded"})).unwrap()
    }
    #[test]
    fn native_identifiers_do_not_spoof_rows_or_qualified_components() {
        assert_eq!(
            display_identifier("schema.name]\nother"),
            "[schema.name]]\\nother]"
        );
        assert!(!display_identifier("name\n\t").contains(['\n', '\t']));
    }
    #[gpui::test]
    fn vim_matrix_binds_subject_pages_and_source_without_query_execution(cx: &mut TestAppContext) {
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
                .set_view(DatabaseMonitorView::Security);
            shell.automation_focus_handle.focus(window, cx);
            cx.notify();
        });
        cx.simulate_keystrokes("v");
        workspace.update(&mut cx, |shell, cx| {
            assert!(shell.sqlserver_privilege_matrix.active);
            shell
                .sqlserver_privilege_matrix
                .input
                .as_ref()
                .unwrap()
                .update(cx, |input, cx| input.set_text("subject", cx));
        });
        let original_input = workspace.read_with(&cx, |shell, _| {
            shell.sqlserver_privilege_matrix.input.clone().unwrap()
        });
        cx.simulate_keystrokes("enter");
        let source = match commands.try_recv().unwrap() {
            ExecutorCommand::ReadSqlServerPrivileges { source, query } => {
                assert_eq!(query.principal.as_deref(), Some("subject"));
                assert_eq!(query.offset, 0);
                source
            }
            _ => panic!("read-only permission inspection expected"),
        };
        workspace.update(&mut cx, |shell, cx| {
            let mut wrong = source.clone();
            wrong.request_id = uuid::Uuid::new_v4();
            shell.finish_sqlserver_privileges(wrong, Ok(report()), cx);
            assert!(shell.sqlserver_privilege_matrix.pending);
            shell.finish_sqlserver_privileges(source.clone(), Ok(report()), cx);
            assert!(!shell.sqlserver_privilege_matrix.pending);
            assert!(checks(
                &shell
                    .sqlserver_privilege_matrix
                    .report
                    .as_ref()
                    .unwrap()
                    .objects[0]
            )
            .contains("SELECT: unavailable"));
        });
        cx.simulate_keystrokes("j");
        workspace.read_with(&cx, |shell, _| {
            assert_eq!(shell.sqlserver_privilege_matrix.selected, 1)
        });
        cx.simulate_keystrokes("n");
        let next = match commands.try_recv().unwrap() {
            ExecutorCommand::ReadSqlServerPrivileges { source, query } => {
                assert_eq!(query.offset, 100);
                assert_eq!(query.principal.as_deref(), Some("subject"));
                source
            }
            _ => panic!("native next page expected"),
        };
        workspace.update(&mut cx, |shell, cx| {
            shell.finish_sqlserver_privileges(source.clone(), Ok(report()), cx);
            assert!(shell.sqlserver_privilege_matrix.pending);
            shell.active_query_connection = Some((
                7,
                sift_protocol::SessionId(4),
                sift_protocol::ConnectionId(11),
            ));
            shell.finish_sqlserver_privileges(next.clone(), Ok(report()), cx);
            assert!(shell.sqlserver_privilege_matrix.pending);
            assert!(shell.sqlserver_privilege_matrix.report.is_none());
        });
        cx.simulate_keystrokes("escape v enter");
        let fresh = match commands.try_recv().unwrap() {
            ExecutorCommand::ReadSqlServerPrivileges { source, query } => {
                assert!(query.principal.is_none());
                assert_eq!(source.connection_id, sift_protocol::ConnectionId(11));
                source
            }
            _ => panic!("fresh actor expected"),
        };
        workspace.update(&mut cx, |shell, cx| {
            shell.finish_sqlserver_privileges(next, Ok(report()), cx);
            assert!(shell.sqlserver_privilege_matrix.pending);
            shell.finish_sqlserver_privileges(fresh, Err("native authority required".into()), cx);
            assert!(shell
                .sqlserver_privilege_matrix
                .error
                .as_deref()
                .unwrap()
                .contains("authority"));
        });
        cx.simulate_keystrokes("e");
        original_input.update(&mut cx, |_, cx| cx.emit(TextInputEvent::Submitted));
        cx.run_until_parked();
        assert!(
            commands.try_recv().is_err(),
            "discarded subject input cannot submit the new form"
        );
        workspace.update(&mut cx, |shell, _| {
            shell.active_query_connection = Some((
                7,
                sift_protocol::SessionId(4),
                sift_protocol::ConnectionId(12),
            ))
        });
        cx.simulate_keystrokes("enter");
        workspace.read_with(&cx, |shell, _| {
            assert!(shell
                .sqlserver_privilege_matrix
                .error
                .as_deref()
                .unwrap()
                .contains("Source changed"))
        });
        assert!(
            commands.try_recv().is_err(),
            "source change must not dispatch a read or query"
        );
    }
}
