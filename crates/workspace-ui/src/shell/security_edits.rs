//! Source-bound administration drafts use existing audited preview/apply APIs.
use super::*;
use sift_protocol::{
    PostgresObjectAction as Pg, PostgresOwnedObjectKind as Owner, PostgresSchemaPrivilege as Priv,
    SqlServerSecurityAction as Ms,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode {
    PgRole,
    PgGrantUsage,
    PgGrantCreate,
    PgRevokeUsage,
    PgRevokeCreate,
    PgSchemaOwner,
    PgDatabaseOwner,
    MsRole,
    MsAddMember,
    MsDropMember,
    MsGrant,
    MsRevoke,
}
impl Mode {
    fn next(self) -> Self {
        use Mode::*;
        match self {
            PgRole => PgRole,
            PgGrantUsage => PgGrantCreate,
            PgGrantCreate => PgRevokeUsage,
            PgRevokeUsage => PgRevokeCreate,
            PgRevokeCreate => PgGrantUsage,
            PgSchemaOwner => PgDatabaseOwner,
            PgDatabaseOwner => PgSchemaOwner,
            MsRole => MsAddMember,
            MsAddMember => MsDropMember,
            MsDropMember => MsGrant,
            MsGrant => MsRevoke,
            MsRevoke => MsRole,
        }
    }
    fn labels(self) -> (&'static str, &'static str, Option<&'static str>) {
        use Mode::*;
        match self {
            PgRole => ("Create PostgreSQL NOLOGIN role", "Role name", None),
            PgGrantUsage => ("Grant schema USAGE", "Schema", Some("Grantee")),
            PgGrantCreate => ("Grant schema CREATE", "Schema", Some("Grantee")),
            PgRevokeUsage => ("Revoke schema USAGE", "Schema", Some("Grantee")),
            PgRevokeCreate => ("Revoke schema CREATE", "Schema", Some("Grantee")),
            PgSchemaOwner => ("Change schema owner", "Schema", Some("New owner")),
            PgDatabaseOwner => ("Change database owner", "Database", Some("New owner")),
            MsRole => ("Create SQL Server database role", "Role name", None),
            MsAddMember => ("Add role member", "Role", Some("Member")),
            MsDropMember => ("Remove role member", "Role", Some("Member")),
            MsGrant => ("Grant schema SELECT", "Schema", Some("Grantee")),
            MsRevoke => ("Revoke schema SELECT", "Schema", Some("Grantee")),
        }
    }
    fn postgres(self) -> bool {
        !matches!(
            self,
            Self::MsRole | Self::MsAddMember | Self::MsDropMember | Self::MsGrant | Self::MsRevoke
        )
    }
}

#[derive(PartialEq, Eq)]
struct EditSource {
    instance: String,
    tenant: i64,
    connection: (i64, sift_protocol::SessionId, sift_protocol::ConnectionId),
    view: DatabaseMonitorView,
}

pub(super) struct SecurityEditState {
    pub mode: Option<Mode>,
    fields: [Entity<TextInput>; 2],
    source: Option<EditSource>,
}
impl SecurityEditState {
    pub fn new(window: &mut Window, cx: &mut Context<WorkspaceShell>) -> Self {
        let fields = [
            cx.new(|cx| TextInput::new("", "Object or role name", cx)),
            cx.new(|cx| TextInput::new("", "Member, grantee or owner", cx)),
        ];
        for field in &fields {
            cx.subscribe_in(
                field,
                window,
                |shell, _, event: &TextInputEvent, window, cx| {
                    if *event == TextInputEvent::Submitted && shell.security_edit.mode.is_some() {
                        shell.preview_security_edit(window, cx);
                    }
                },
            )
            .detach();
        }
        Self {
            mode: None,
            fields,
            source: None,
        }
    }
}

enum Draft {
    Pg(Pg),
    Ms(Ms),
}
fn draft(mode: Mode, first: String, second: String) -> Result<Draft, String> {
    let names = if mode.labels().2.is_some() {
        vec![&first, &second]
    } else {
        vec![&first]
    };
    if names.into_iter().any(|name| {
        name.is_empty()
            || name.chars().any(char::is_control)
            || if mode.postgres() {
                name.len() > 63
            } else {
                name.chars().count() > 128
            }
    }) {
        return Err("Enter nonempty names without controls (PostgreSQL: 63 bytes; SQL Server: 128 characters)".into());
    }
    use Mode::*;
    Ok(match mode {
        PgRole => Draft::Pg(Pg::CreateRole { name: first }),
        PgGrantUsage | PgGrantCreate => Draft::Pg(Pg::GrantSchemaPrivilege {
            schema: first,
            grantee: second,
            privilege: if mode == PgGrantUsage {
                Priv::Usage
            } else {
                Priv::Create
            },
        }),
        PgRevokeUsage | PgRevokeCreate => Draft::Pg(Pg::RevokeSchemaPrivilege {
            schema: first,
            grantee: second,
            privilege: if mode == PgRevokeUsage {
                Priv::Usage
            } else {
                Priv::Create
            },
        }),
        PgSchemaOwner | PgDatabaseOwner => Draft::Pg(Pg::ChangeOwner {
            object_kind: if mode == PgSchemaOwner {
                Owner::Schema
            } else {
                Owner::Database
            },
            name: first,
            new_owner: second,
        }),
        MsRole => Draft::Ms(Ms::CreateDatabaseRole { name: first }),
        MsAddMember => Draft::Ms(Ms::AddRoleMember {
            role: first,
            member: second,
        }),
        MsDropMember => Draft::Ms(Ms::DropRoleMember {
            role: first,
            member: second,
        }),
        MsGrant => Draft::Ms(Ms::GrantSchemaSelect {
            schema: first,
            grantee: second,
        }),
        MsRevoke => Draft::Ms(Ms::RevokeSchemaSelect {
            schema: first,
            grantee: second,
        }),
    })
}
impl WorkspaceShell {
    fn current_monitor_source(&self, request_id: uuid::Uuid) -> Option<MonitorActionSource> {
        let (profile_id, session_id, connection_id) = self.active_query_connection?;
        Some(MonitorActionSource {
            instance_id: self.selected_instance_id.clone()?,
            tenant_id: self.selected_tenant_id()?,
            profile_id,
            session_id,
            connection_id,
            request_id,
        })
    }
    pub(super) fn start_monitor_action(&mut self) -> Option<MonitorActionSource> {
        let source = self.current_monitor_source(uuid::Uuid::new_v4())?;
        self.monitor_action_scope = Some(source.clone());
        Some(source)
    }
    pub(super) fn accepts_monitor_reply(&self, source: &MonitorActionSource) -> bool {
        self.monitor_action_scope.as_ref() == Some(source)
            && self.current_monitor_source(source.request_id).as_ref() == Some(source)
    }

    pub(super) fn begin_security_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use DatabaseMonitorView::*;
        let mode = match self.database_monitor.view() {
            Roles => Mode::PgRole,
            SchemaGrants => Mode::PgGrantUsage,
            Ownership => Mode::PgSchemaOwner,
            Security => Mode::MsRole,
            _ => return,
        };
        if self.database_monitor.object_action_request().loading()
            || self.database_monitor.security_action_request().loading()
        {
            return;
        }
        let (Some(instance), Some(tenant), Some(connection)) = (
            self.selected_instance_id.clone(),
            self.selected_tenant_id(),
            self.active_query_connection,
        ) else {
            self.show_error_toast(
                "Connect a database before editing administration objects".into(),
                cx,
            );
            return;
        };
        for input in &self.security_edit.fields {
            input.update(cx, |input, cx| input.set_text("", cx));
        }
        self.security_edit.source = Some(EditSource {
            instance,
            tenant,
            connection,
            view: self.database_monitor.view(),
        });
        self.security_edit.mode = Some(mode);
        self.database_monitor.clear_object_preview();
        self.database_monitor.clear_security_preview();
        self.security_edit.fields[0]
            .focus_handle(cx)
            .focus(window, cx);
        cx.notify();
    }
    pub(super) fn preview_security_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mode) = self.security_edit.mode else {
            return;
        };
        let current = self
            .selected_instance_id
            .clone()
            .zip(self.selected_tenant_id())
            .zip(self.active_query_connection)
            .map(|((instance, tenant), connection)| EditSource {
                instance,
                tenant,
                connection,
                view: self.database_monitor.view(),
            });
        if current != self.security_edit.source {
            self.security_edit.mode = None;
            self.show_error_toast(
                "Database context changed; reopen administration form".into(),
                cx,
            );
            return;
        }
        let first = self.security_edit.fields[0]
            .read(cx)
            .text()
            .trim()
            .to_owned();
        let second = self.security_edit.fields[1]
            .read(cx)
            .text()
            .trim()
            .to_owned();
        match draft(mode, first, second) {
            Ok(action) => {
                self.security_edit.mode = None;
                self.automation_focus_handle.focus(window, cx);
                match action {
                    Draft::Pg(action) => self.preview_postgres_object(action, cx),
                    Draft::Ms(action) => self.preview_sqlserver_security_action(action, cx),
                }
            }
            Err(message) => self.show_error_toast(message, cx),
        }
    }
    pub(super) fn security_edit_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focused = self
            .security_edit
            .fields
            .iter()
            .position(|input| input.focus_handle(cx).is_focused(window));
        let key = event.keystroke.unparse();
        match key.as_str() {
            "escape" if focused.is_some() => self.automation_focus_handle.focus(window, cx),
            "escape" => self.security_edit.mode = None,
            "tab" | "shift-tab" if focused.is_some() => {
                let current = focused.unwrap();
                let next = if key == "tab" {
                    current + 1
                } else {
                    current.checked_sub(1).unwrap_or(2)
                };
                let count = if self.security_edit.mode.unwrap().labels().2.is_some() {
                    2
                } else {
                    1
                };
                if next < count {
                    self.security_edit.fields[next]
                        .focus_handle(cx)
                        .focus(window, cx);
                } else {
                    self.automation_focus_handle.focus(window, cx);
                }
            }
            "m" if focused.is_none() => {
                self.security_edit.mode = self.security_edit.mode.map(Mode::next);
            }
            "i" if focused.is_none() => self.security_edit.fields[0]
                .focus_handle(cx)
                .focus(window, cx),
            "p" if focused.is_none() => self.preview_security_edit(window, cx),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }
}
pub(super) fn render_form(shell: &WorkspaceShell, cx: &mut Context<WorkspaceShell>) -> AnyElement {
    let mode = shell.security_edit.mode.unwrap();
    let (title, first, second) = mode.labels();
    let field = |label: &'static str, input: &Entity<TextInput>| {
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(div().w(px(95.)).text_sm().child(label))
            .child(div().flex_1().min_w_0().child(input.clone()))
    };
    div()
        .id("administration-edit-form")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .child(title)
        .child(field(first, &shell.security_edit.fields[0]))
        .children(second.map(|label| field(label, &shell.security_edit.fields[1])))
        .child(
            div()
                .text_xs()
                .child("Tab next · Esc command/cancel · i edit · m action · p preview"),
        )
        .child(
            Button::new("administration-preview", "Preview exact SQL").on_click(
                cx.listener(|shell, _, window, cx| shell.preview_security_edit(window, cx)),
            ),
        )
        .into_any_element()
}

pub(super) fn explicit_confirmation(action: &Pg) -> bool {
    matches!(
        action,
        Pg::RenamePolicy { .. }
            | Pg::CreateRole { .. }
            | Pg::GrantSchemaPrivilege { .. }
            | Pg::RevokeSchemaPrivilege { .. }
            | Pg::ChangeOwner { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};

    #[test]
    fn names_remain_typed_data_and_native_limits_are_checked() {
        let name = "reader'); DROP ROLE admin; --";
        assert!(
            matches!(draft(Mode::PgRole, name.into(), String::new()), Ok(Draft::Pg(Pg::CreateRole { name: actual })) if actual == name)
        );
        assert!(draft(Mode::PgRole, "é".repeat(33), String::new()).is_err());
        assert!(draft(Mode::MsRole, "é".repeat(128), String::new()).is_ok());
        assert!(draft(Mode::MsRole, "x".repeat(129), String::new()).is_err());
        assert!(draft(Mode::PgGrantUsage, "public".into(), String::new()).is_err());
        assert!(draft(Mode::MsAddMember, "readers".into(), "bad\nname".into()).is_err());
    }

    #[test]
    fn executor_target_requires_every_identity_component() {
        let source = MonitorActionSource {
            instance_id: "a".into(),
            tenant_id: 1,
            profile_id: 7,
            session_id: sift_protocol::SessionId(3),
            connection_id: sift_protocol::ConnectionId(9),
            request_id: uuid::Uuid::new_v4(),
        };
        assert!(source.matches_context(
            "a",
            1,
            7,
            sift_protocol::SessionId(3),
            sift_protocol::ConnectionId(9)
        ));
        assert!(!source.matches_context(
            "b",
            1,
            7,
            sift_protocol::SessionId(3),
            sift_protocol::ConnectionId(9)
        ));
        assert!(!source.matches_context(
            "a",
            2,
            7,
            sift_protocol::SessionId(3),
            sift_protocol::ConnectionId(9)
        ));
        assert!(!source.matches_context(
            "a",
            1,
            8,
            sift_protocol::SessionId(3),
            sift_protocol::ConnectionId(9)
        ));
        assert!(!source.matches_context(
            "a",
            1,
            7,
            sift_protocol::SessionId(4),
            sift_protocol::ConnectionId(9)
        ));
        assert!(!source.matches_context(
            "a",
            1,
            7,
            sift_protocol::SessionId(3),
            sift_protocol::ConnectionId(10)
        ));
    }

    #[gpui::test]
    fn administration_forms_preview_then_confirm_and_reject_changed_source(
        cx: &mut TestAppContext,
    ) {
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |window, cx| {
                cx.new(|cx| {
                    WorkspaceShell::new(
                        Default::default(),
                        Default::default(),
                        None,
                        None,
                        window,
                        cx,
                    )
                })
            })
            .unwrap()
        });
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|_, cx| {
            cx.bind_keys([gpui::KeyBinding::new(
                "enter",
                sift_ui::Submit,
                Some("SiftTextInput"),
            )])
        });
        let workspace = window.root(&mut cx).unwrap();
        let (sender, mut commands) = ExecutorSender::channel(32);
        workspace.update_in(&mut cx, |shell, window, cx| {
            shell.executor_sender = Some(sender);
            shell.selected_instance_id = Some("fixture".into());
            shell.lifecycle.tenants = vec![crate::TenantNavEntry {
                id: sift_api_types::TenantId(1),
                name: "Personal".into(),
                rooms: vec![],
                connections: vec![ConnectionNavEntry {
                    id: 7,
                    tenant_id: 1,
                    name: "Postgres".into(),
                    provider_id: sift_protocol::Engine::Postgres.provider_id(),
                    tags: vec![],
                }],
            }];
            shell.connection_status = ConnectionStatus::Connected {
                profile_id: 7,
                name: "Postgres".into(),
            };
            shell.active_query_connection = Some((
                7,
                sift_protocol::SessionId(3),
                sift_protocol::ConnectionId(9),
            ));
            shell.active_bottom_tool = BottomTool::Monitor;
            shell.bottom_dock.presentation.open = true;
            shell.database_monitor.set_view(DatabaseMonitorView::Roles);
            shell.automation_focus_handle.focus(window, cx);
            cx.notify();
        });
        cx.simulate_keystrokes("a");
        workspace.update(&mut cx, |shell, cx| {
            assert_eq!(shell.security_edit.mode, Some(Mode::PgRole));
            shell.security_edit.fields[0].update(cx, |input, cx| input.set_text("readers", cx));
        });
        cx.simulate_keystrokes("enter");
        let action = match commands.try_recv().unwrap() {
            ExecutorCommand::PreviewPostgresObject { action, .. } => action,
            _ => panic!("expected preview only"),
        };
        assert!(matches!(&action, Pg::CreateRole { name } if name == "readers"));
        assert!(commands.try_recv().is_err());
        workspace.update(&mut cx, |shell, cx| {
            let mut stale = shell.monitor_action_scope.clone().unwrap();
            stale.request_id = uuid::Uuid::new_v4();
            shell.on_executor_event(
                ExecutorEvent::PostgresObjectPreviewed {
                    source: stale,
                    result: Ok(sift_protocol::PostgresObjectPreview {
                        action: action.clone(),
                        sql: "stale SQL".into(),
                        precondition: "old".into(),
                        warning: String::new(),
                        impact: None,
                    }),
                },
                cx,
            );
            assert!(shell.database_monitor.object_preview().is_none());
            shell.on_executor_event(
                ExecutorEvent::PostgresObjectPreviewed {
                    source: shell.monitor_action_scope.clone().unwrap(),
                    result: Ok(sift_protocol::PostgresObjectPreview {
                        action,
                        sql: "CREATE ROLE readers NOLOGIN".into(),
                        precondition: "f".repeat(64),
                        warning: "Review ownership".into(),
                        impact: None,
                    }),
                },
                cx,
            )
        });
        cx.simulate_keystrokes("enter");
        assert!(commands.try_recv().is_err());
        cx.simulate_keystrokes("x");
        assert!(
            matches!(commands.try_recv(), Ok(ExecutorCommand::ApplyPostgresObject { request, .. }) if request.confirmed && request.production_confirmed)
        );
        workspace.update_in(&mut cx, |shell, window, cx| {
            shell.database_monitor.finish_object_apply(Ok(()));
            shell.begin_security_edit(window, cx);
            shell.security_edit.fields[0].update(cx, |input, cx| input.set_text("other", cx));
            shell.selected_instance_id = Some("different".into());
            shell.preview_security_edit(window, cx);
            assert!(shell.security_edit.mode.is_none());
        });
        assert!(commands.try_recv().is_err());
        workspace.update_in(&mut cx, |shell, window, cx| {
            shell.selected_instance_id = Some("fixture".into());
            shell.lifecycle.tenants[0].connections[0].provider_id =
                sift_protocol::Engine::SqlServer.provider_id();
            shell.connection_status = ConnectionStatus::Connected {
                profile_id: 7,
                name: "SQL Server".into(),
            };
            shell.active_query_connection = Some((
                7,
                sift_protocol::SessionId(4),
                sift_protocol::ConnectionId(10),
            ));
            shell
                .database_monitor
                .set_view(DatabaseMonitorView::Security);
            shell.automation_focus_handle.focus(window, cx);
            cx.notify();
        });
        cx.simulate_keystrokes("a");
        cx.simulate_keystrokes("escape");
        cx.simulate_keystrokes("m");
        cx.simulate_keystrokes("i");
        workspace.update(&mut cx, |shell, cx| {
            assert_eq!(shell.security_edit.mode, Some(Mode::MsAddMember));
            shell.security_edit.fields[0].update(cx, |input, cx| input.set_text("readers", cx));
            shell.security_edit.fields[1].update(cx, |input, cx| input.set_text("analyst", cx));
        });
        cx.simulate_keystrokes("tab");
        cx.simulate_keystrokes("enter");
        let (action, source) = match commands.try_recv().unwrap() {
            ExecutorCommand::PreviewSqlServerSecurity { action, source } => (action, source),
            _ => panic!("expected source-bound SQL Server preview"),
        };
        assert!(
            matches!(&action, Ms::AddRoleMember { role, member } if role == "readers" && member == "analyst")
        );
        assert_eq!(source.session_id, sift_protocol::SessionId(4));
        assert_eq!(source.connection_id, sift_protocol::ConnectionId(10));
        assert!(commands.try_recv().is_err());
        workspace.update(&mut cx, |shell, cx| {
            shell.on_executor_event(
                ExecutorEvent::SqlServerSecurityPreviewed {
                    source: source.clone(),
                    result: Ok(sift_protocol::SqlServerSecurityPreview {
                        action,
                        sql: "ALTER ROLE readers ADD MEMBER analyst".into(),
                        precondition: "f".repeat(64),
                        warning: "Review membership".into(),
                    }),
                },
                cx,
            )
        });
        cx.simulate_keystrokes("enter");
        match commands.try_recv().unwrap() {
            ExecutorCommand::ApplySqlServerSecurity {
                request,
                source: applied,
            } => {
                assert!(request.production_confirmed);
                assert_ne!(applied.request_id, source.request_id);
                assert_eq!(applied.connection_id, source.connection_id);
            }
            _ => panic!("expected confirmed source-bound apply"),
        }
    }
}
