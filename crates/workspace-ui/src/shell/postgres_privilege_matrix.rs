use super::*;
use sift_protocol::{PostgresPrivilegeQuery, PostgresPrivilegeReport};

pub(super) struct State {
    pub active: bool,
    pub role: String,
    pub source: Option<MonitorActionSource>,
    pub pending: bool,
    pub offset: u32,
    pub report: Option<PostgresPrivilegeReport>,
    pub error: Option<String>,
    pub selected: usize,
    pub show_provenance: bool,
    pub grant_selected: usize,
    pub grant_scroll: UniformListScrollHandle,
    pub scroll: UniformListScrollHandle,
}
impl Default for State {
    fn default() -> Self {
        Self {
            active: false,
            role: String::new(),
            source: None,
            pending: false,
            offset: 0,
            report: None,
            error: None,
            selected: 0,
            show_provenance: false,
            grant_selected: 0,
            grant_scroll: UniformListScrollHandle::new(),
            scroll: UniformListScrollHandle::new(),
        }
    }
}
impl WorkspaceShell {
    pub(super) fn begin_privilege_matrix(&mut self, cx: &mut Context<Self>) {
        let Some(role) = self
            .database_monitor
            .roles()
            .get(self.database_monitor.objects_selected())
            .map(|r| r.name.clone())
        else {
            return;
        };
        self.privilege_matrix = State {
            active: true,
            role,
            ..Default::default()
        };
        self.load_privilege_matrix(0, cx);
    }
    fn load_privilege_matrix(&mut self, offset: u32, cx: &mut Context<Self>) {
        if self.privilege_matrix.pending
            || !self.require_operation(
                sift_protocol::OperationKind::ReadPostgresPrivileges,
                "Inspect PostgreSQL privileges",
                cx,
            )
        {
            return;
        }
        if self.privilege_matrix.source.as_ref().is_some_and(|source| {
            self.current_monitor_source(source.request_id).as_ref() != Some(source)
        }) {
            self.privilege_matrix.error = Some("Source changed; reopen the role matrix".into());
            cx.notify();
            return;
        }
        let Some(source) = self.current_monitor_source(uuid::Uuid::new_v4()) else {
            self.privilege_matrix.error = Some("Connect before inspecting role privileges".into());
            cx.notify();
            return;
        };
        let query = PostgresPrivilegeQuery {
            role: Some(self.privilege_matrix.role.clone()),
            offset,
            limit: Some(100),
        };
        self.privilege_matrix.source = Some(source.clone());
        self.privilege_matrix.offset = offset;
        self.privilege_matrix.report = None;
        self.privilege_matrix.selected = 0;
        self.privilege_matrix.grant_selected = 0;
        self.privilege_matrix
            .grant_scroll
            .scroll_to_item(0, ScrollStrategy::Top);
        self.privilege_matrix.pending = true;
        self.privilege_matrix.error = None;
        if !self.executor_sender.as_ref().is_some_and(|sender| {
            sender
                .send(ExecutorCommand::ReadPostgresPrivileges { source, query })
                .is_ok()
        }) {
            self.privilege_matrix.pending = false;
            self.privilege_matrix.error = Some("Database executor is unavailable".into());
        }
        cx.notify();
    }
    pub(super) fn finish_privilege_matrix(
        &mut self,
        source: MonitorActionSource,
        result: Result<PostgresPrivilegeReport, String>,
        cx: &mut Context<Self>,
    ) {
        if !self.privilege_matrix.active
            || !self.privilege_matrix.pending
            || self.privilege_matrix.source.as_ref() != Some(&source)
            || self.current_monitor_source(source.request_id).as_ref() != Some(&source)
        {
            return;
        }
        self.privilege_matrix.pending = false;
        match result {
            Ok(report) if report.role == self.privilege_matrix.role => {
                self.privilege_matrix.report = Some(report);
                self.privilege_matrix.selected = 0;
                self.privilege_matrix.grant_selected = 0;
                self.privilege_matrix
                    .scroll
                    .scroll_to_item(0, ScrollStrategy::Top);
            }
            Ok(_) => self.privilege_matrix.error = Some("Role changed; reopen the matrix".into()),
            Err(message) => self.privilege_matrix.error = Some(message),
        }
        cx.notify();
    }
    pub(super) fn privilege_matrix_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        cx: &mut Context<Self>,
    ) {
        let modifiers = &event.keystroke.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform || modifiers.function {
            return;
        }
        let key = match (event.keystroke.key.as_str(), modifiers.shift) {
            ("j", true) => "J",
            ("k", true) => "K",
            ("J" | "K", _) => event.keystroke.key.as_str(),
            (_, true) => return,
            (key, false) => key,
        };
        match key {
            "escape" => self.privilege_matrix = State::default(),
            "g" => self.privilege_matrix.show_provenance = !self.privilege_matrix.show_provenance,
            "J" | "K" if self.privilege_matrix.show_provenance => {
                let count = self
                    .privilege_matrix
                    .report
                    .as_ref()
                    .and_then(|r| r.objects.get(self.privilege_matrix.selected))
                    .and_then(|o| o.acl.as_ref())
                    .map_or(0, |acl| acl.grants.len());
                let delta = if key == "J" { 1 } else { -1 };
                self.privilege_matrix.grant_selected = self
                    .privilege_matrix
                    .grant_selected
                    .saturating_add_signed(delta)
                    .min(count.saturating_sub(1));
                self.privilege_matrix.grant_scroll.scroll_to_item(
                    self.privilege_matrix.grant_selected,
                    ScrollStrategy::Nearest,
                );
            }
            "r" => self.load_privilege_matrix(self.privilege_matrix.offset, cx),
            "n" => {
                if let Some(next) = self
                    .privilege_matrix
                    .report
                    .as_ref()
                    .and_then(|r| r.next_offset)
                {
                    self.load_privilege_matrix(next, cx);
                }
            }
            "p" if self.privilege_matrix.offset > 0 => {
                self.load_privilege_matrix(self.privilege_matrix.offset.saturating_sub(100), cx)
            }
            "j" | "k" => {
                let count = self
                    .privilege_matrix
                    .report
                    .as_ref()
                    .map_or(0, |r| r.objects.len());
                let delta = if event.keystroke.key == "j" { 1 } else { -1 };
                self.privilege_matrix.selected = self
                    .privilege_matrix
                    .selected
                    .saturating_add_signed(delta)
                    .min(count.saturating_sub(1));
                self.privilege_matrix
                    .scroll
                    .scroll_to_item(self.privilege_matrix.selected, ScrollStrategy::Nearest);
                self.privilege_matrix.grant_selected = 0;
                self.privilege_matrix
                    .grant_scroll
                    .scroll_to_item(0, ScrollStrategy::Top);
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }
}
fn label(object: &sift_protocol::PostgresPrivilegeObject) -> String {
    format!(
        "{:?} {}{}{} · OID {}{}",
        object.kind,
        if object.kind == sift_protocol::PostgresPrivilegeObjectKind::Schema {
            String::new()
        } else {
            object
                .schema
                .as_ref()
                .map_or(String::new(), |s| format!("{s}."))
        },
        object
            .relation
            .as_ref()
            .map_or(String::new(), |s| format!("{s}.")),
        object.name,
        object.object_oid,
        object
            .column_number
            .map_or(String::new(), |n| format!(" column {n}"))
    )
}
fn role_label(oid: u32, name: Option<&str>) -> String {
    name.map_or_else(
        || format!("unknown role OID {oid}"),
        |name| format!("{name:?} (OID {oid})"),
    )
}
fn grant_label(grant: &sift_protocol::PostgresPrivilegeGrant) -> String {
    let grantee = if grant.grantee_oid == 0 {
        "PUBLIC".into()
    } else {
        role_label(grant.grantee_oid, grant.grantee.as_deref())
    };
    format!(
        "{} → {} · {} · grant option {} · subject grantee access {}",
        role_label(grant.grantor_oid, grant.grantor.as_deref()),
        grantee,
        grant.privilege,
        grant.grantable,
        grant.applies_to_subject
    )
}
fn render_acl(shell: &WorkspaceShell, cx: &mut Context<WorkspaceShell>) -> AnyElement {
    let state = &shell.privilege_matrix;
    let Some(object) = state
        .report
        .as_ref()
        .and_then(|r| r.objects.get(state.selected))
    else {
        return div()
            .child("Select an object to inspect native ACL provenance")
            .into_any_element();
    };
    let Some(acl) = &object.acl else {
        return div()
            .child("ACL provenance unavailable from this server")
            .into_any_element();
    };
    div().flex().flex_col().flex_1().min_h_0().gap_2()
        .child(label(object))
        .child(format!("Owner {} · {}", role_label(acl.owner_oid, acl.owner.as_deref()),
            if acl.defaulted { "NULL ACL: PostgreSQL built-in defaults" } else { "Stored object ACL" }))
        .children(acl.truncated.then(|| ErrorBanner::new("ACL provenance is incomplete: native item or 64-grant limit reached")))
        .children(acl.grants.is_empty().then(|| div().child("No ACL grant rows. Effective checks may still include table grants or owner inherent grant options.")))
        .child(uniform_list("postgres-acl-grants", acl.grants.len(),
            cx.processor(|shell, range: Range<usize>, _, cx| {
                let Some(acl) = shell.privilege_matrix.report.as_ref()
                    .and_then(|r| r.objects.get(shell.privilege_matrix.selected))
                    .and_then(|o| o.acl.as_ref()) else { return vec![]; };
                range.filter_map(|index| acl.grants.get(index).map(|grant| div()
                    .h(px(30.)).overflow_hidden().text_sm()
                    .when(index == shell.privilege_matrix.grant_selected, |row| row.bg(cx.theme().colors.active_surface))
                    .child(grant_label(grant)))).collect::<Vec<_>>()
            })).flex_1().min_h_0().track_scroll(&state.grant_scroll))
        .children(acl.grants.get(state.grant_selected).map(|grant| div().text_sm().whitespace_normal().child(grant_label(grant))))
        .into_any_element()
}
pub(super) fn render(shell: &WorkspaceShell, cx: &mut Context<WorkspaceShell>) -> AnyElement {
    let state = &shell.privilege_matrix;
    let colors = cx.theme().colors;
    let count = state.report.as_ref().map_or(0, |r| r.objects.len());
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .gap_2()
        .p_3()
        .child(SectionLabel::new("NATIVE PRIVILEGE MATRIX"))
        .child(format!(
            "Role {} · page offset {} · j/k objects · g ACL view · J/K grants · n/p pages · r refresh · Escape return",
            state.role, state.offset
        ))
        .children(
            state
                .pending
                .then(|| div().child("Reading native privileges…")),
        )
        .children(state.error.clone().map(ErrorBanner::new))
        .children(
            state
                .report
                .as_ref()
                .is_some_and(|r| r.page_limit_reached)
                .then(|| div().child("Catalog page ceiling reached; this matrix is incomplete")),
        )
        .children(state.report.as_ref().map(|r| {
            div().text_xs().whitespace_normal().child(format!(
                "Database {} · connected actor {} · subject superuser {} / bypass RLS {}. {}",
                r.database, r.actor, r.role_superuser, r.role_bypass_rls, r.warning
            ))
        }))
        .child(if state.show_provenance {
            render_acl(shell, cx)
        } else {
            uniform_list(
                "postgres-privilege-rows",
                count,
                cx.processor(|shell, range: Range<usize>, _, cx| {
                    let Some(report) = &shell.privilege_matrix.report else {
                        return vec![];
                    };
                    range
                        .filter_map(|index| {
                            report.objects.get(index).map(|object| {
                                div()
                                    .h(px(30.))
                                    .overflow_hidden()
                                    .text_sm()
                                    .when(index == shell.privilege_matrix.selected, |row| {
                                        row.bg(cx.theme().colors.active_surface)
                                    })
                                    .child(label(object))
                            })
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .flex_1()
            .min_h_0()
            .track_scroll(&state.scroll).into_any_element()
        })
        .children(
            state
                .report
                .as_ref()
                .and_then(|r| r.objects.get(state.selected))
                .map(|object| {
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .text_sm()
                        .child(label(object))
                        .child(format!(
                            "RLS enabled {} / forced {} (flags, not policy evaluation)",
                            object.row_security_enabled, object.row_security_forced
                        ))
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap_2()
                                .children(object.checks.iter().map(|check| {
                                    div()
                                        .px_2()
                                        .py_1()
                                        .border_1()
                                        .border_color(colors.subtle_border)
                                        .child(format!(
                                            "{}: {} · grant {}",
                                            check.privilege,
                                            if check.allowed { "yes" } else { "no" },
                                            if check.grantable { "yes" } else { "no" }
                                        ))
                                })),
                        )
                }),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    fn report() -> PostgresPrivilegeReport {
        PostgresPrivilegeReport {
            database: "db".into(),
            actor: "actor".into(),
            role: "subject".into(),
            role_superuser: false,
            role_bypass_rls: false,
            objects: vec![serde_json::from_value(serde_json::json!({
                "kind":"table","object_oid":10,"column_number":null,"schema":"public","relation":null,"name":"items",
                "row_security_enabled":false,"row_security_forced":false,"checks":[],
                "acl":{"owner_oid":1,"owner":"owner","defaulted":false,"truncated":true,"grants":[
                    {"grantor_oid":1,"grantor":"owner","grantee_oid":0,"grantee":null,"privilege":"SELECT","grantable":false,"applies_to_subject":true},
                    {"grantor_oid":2,"grantor":null,"grantee_oid":3,"grantee":"line\nrole","privilege":"UPDATE","grantable":true,"applies_to_subject":false}
                ]}
            })).unwrap(), serde_json::from_value(serde_json::json!({
                "kind":"table","object_oid":11,"column_number":null,"schema":"public","relation":null,"name":"legacy",
                "row_security_enabled":false,"row_security_forced":false,"checks":[]
            })).unwrap()],
            next_offset: Some(100),
            page_limit_reached: false,
            warning: "Native ACL checks only".into(),
        }
    }
    #[test]
    fn provenance_labels_distinguish_public_unknown_and_unavailable() {
        let report = report();
        assert!(
            report.objects[1].acl.is_none(),
            "old responses must stay unavailable, not become empty ACLs"
        );
        let acl = report.objects[0].acl.as_ref().unwrap();
        assert!(grant_label(&acl.grants[0]).contains("PUBLIC"));
        let unknown = grant_label(&acl.grants[1]);
        assert!(unknown.contains("unknown role OID 2"));
        assert!(
            !unknown.contains('\n'),
            "native role control characters must be escaped"
        );
        assert!(!unknown.contains("PUBLIC"));
    }
    #[gpui::test]
    fn vim_role_matrix_binds_pages_requests_and_original_query_source(cx: &mut TestAppContext) {
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
                    name: "PG".into(),
                    provider_id: sift_protocol::Engine::Postgres.provider_id(),
                    tags: vec![],
                }],
            }];
            shell.connection_status = ConnectionStatus::Connected {
                profile_id: 7,
                name: "PG".into(),
            };
            shell.active_query_connection = Some((
                7,
                sift_protocol::SessionId(4),
                sift_protocol::ConnectionId(10),
            ));
            shell.active_bottom_tool = BottomTool::Monitor;
            shell.bottom_dock.presentation.open = true;
            shell.database_monitor.set_view(DatabaseMonitorView::Roles);
            shell.database_monitor.finish_roles(
                0,
                Ok(sift_protocol::PostgresObjectPage {
                    items: vec![sift_protocol::PostgresRole {
                        name: "subject".into(),
                        can_login: true,
                        can_create_role: false,
                        superuser: false,
                        details: None,
                    }],
                    next_offset: None,
                }),
            );
            shell.automation_focus_handle.focus(window, cx);
            cx.notify();
        });
        cx.simulate_keystrokes("v");
        let source = match commands.try_recv().unwrap() {
            ExecutorCommand::ReadPostgresPrivileges { source, query } => {
                assert_eq!(query.role.as_deref(), Some("subject"));
                assert_eq!(query.offset, 0);
                source
            }
            _ => panic!("expected native role matrix"),
        };
        workspace.update(&mut cx, |shell, cx| {
            let mut wrong = source.clone();
            wrong.request_id = uuid::Uuid::new_v4();
            shell.finish_privilege_matrix(wrong, Ok(report()), cx);
            assert!(shell.privilege_matrix.pending);
            shell.finish_privilege_matrix(source.clone(), Ok(report()), cx);
            assert!(shell.privilege_matrix.report.is_some());
        });
        cx.simulate_keystrokes("g J");
        workspace.update(&mut cx, |shell, _| {
            assert!(shell.privilege_matrix.show_provenance);
            assert_eq!(shell.privilege_matrix.grant_selected, 1);
        });
        cx.simulate_keystrokes("K j");
        workspace.update(&mut cx, |shell, _| {
            assert_eq!(shell.privilege_matrix.selected, 1);
            assert_eq!(shell.privilege_matrix.grant_selected, 0);
            assert!(shell.privilege_matrix.report.as_ref().unwrap().objects[1]
                .acl
                .is_none());
        });
        cx.simulate_keystrokes("k g");
        workspace.update(&mut cx, |shell, _| {
            assert!(!shell.privilege_matrix.show_provenance)
        });
        cx.simulate_keystrokes("n");
        let page = match commands.try_recv().unwrap() {
            ExecutorCommand::ReadPostgresPrivileges { source, query } => {
                assert_eq!(query.offset, 100);
                source
            }
            _ => panic!("expected next native page"),
        };
        workspace.update(&mut cx, |shell, cx| {
            shell.finish_privilege_matrix(source.clone(), Ok(report()), cx);
            assert!(
                shell.privilege_matrix.pending,
                "old page cannot replace a newer request"
            );
            shell.finish_privilege_matrix(page.clone(), Ok(report()), cx);
            shell.active_query_connection = Some((
                7,
                sift_protocol::SessionId(4),
                sift_protocol::ConnectionId(11),
            ));
        });
        cx.simulate_keystrokes("r");
        assert!(
            commands.try_recv().is_err(),
            "refresh cannot silently switch native source"
        );
        workspace.update(&mut cx, |shell, _| {
            assert!(shell
                .privilege_matrix
                .error
                .as_ref()
                .unwrap()
                .contains("Source changed"))
        });
        cx.simulate_keystrokes("escape");
        workspace.update(&mut cx, |shell, cx| {
            shell.finish_privilege_matrix(page, Ok(report()), cx);
            assert!(!shell.privilege_matrix.active && shell.privilege_matrix.report.is_none());
        });
    }
}
