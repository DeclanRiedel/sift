//! Source-bound administration drafts use existing audited preview/apply APIs.
use super::*;
use sift_protocol::{
    PostgresObjectAction as Pg, PostgresOwnedObjectKind as Owner, PostgresSchemaPrivilege as Priv,
    SqlServerSecurityAction as Ms,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode {
    PgRole,
    PgRenameRole,
    PgAttributes,
    PgDropRole,
    PgGrantMembership,
    PgRevokeMembership,
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
    MsSchemaOwner,
    MsDatabaseOwner,
    MsCreateUser,
    MsRemapUser,
    MsUserSchema,
    MsDropUser,
}
impl Mode {
    fn next(self) -> Self {
        use Mode::*;
        match self {
            PgRole => PgRenameRole,
            PgRenameRole => PgAttributes,
            PgAttributes => PgDropRole,
            PgDropRole => PgRole,
            PgGrantMembership => PgRevokeMembership,
            PgRevokeMembership => PgGrantMembership,
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
            MsRevoke => MsSchemaOwner,
            MsSchemaOwner => MsDatabaseOwner,
            MsDatabaseOwner => MsCreateUser,
            MsCreateUser => MsRemapUser,
            MsRemapUser => MsUserSchema,
            MsUserSchema => MsDropUser,
            MsDropUser => MsRole,
        }
    }
    fn labels(self) -> (&'static str, &'static str, Option<&'static str>) {
        use Mode::*;
        match self {
            PgRole => (
                "Create PostgreSQL role (NOLOGIN by default)",
                "Role name",
                None,
            ),
            PgRenameRole => ("Rename PostgreSQL role", "Role", Some("New name")),
            PgAttributes => ("Change PostgreSQL public role attributes", "Role", None),
            PgDropRole => ("Drop PostgreSQL role", "Role", None),
            PgGrantMembership => (
                "Grant or update PostgreSQL membership",
                "Role",
                Some("Member"),
            ),
            PgRevokeMembership => (
                "Revoke PostgreSQL membership (RESTRICT)",
                "Role",
                Some("Member"),
            ),
            PgGrantUsage => ("Grant schema USAGE", "Schema", Some("Grantee")),
            PgGrantCreate => ("Grant schema CREATE", "Schema", Some("Grantee")),
            PgRevokeUsage => ("Revoke schema USAGE", "Schema", Some("Grantee")),
            PgRevokeCreate => ("Revoke schema CREATE", "Schema", Some("Grantee")),
            PgSchemaOwner => ("Change schema owner", "Schema", Some("New owner")),
            PgDatabaseOwner => ("Change database owner", "Database", Some("New owner")),
            MsRole => ("Create SQL Server database role", "Role name", None),
            MsCreateUser => (
                "Create SQL login-mapped user (default schema dbo)",
                "User name",
                Some("Existing SQL login"),
            ),
            MsRemapUser => (
                "Remap SQL user to an existing login",
                "User name",
                Some("Existing SQL login"),
            ),
            MsUserSchema => (
                "Change SQL user's default schema",
                "User name",
                Some("Existing schema"),
            ),
            MsDropUser => ("Drop SQL database user (login survives)", "User name", None),
            MsAddMember => ("Add role member", "Role", Some("Member")),
            MsDropMember => ("Remove role member", "Role", Some("Member")),
            MsGrant => ("Grant schema SELECT", "Schema", Some("Grantee")),
            MsRevoke => ("Revoke schema SELECT", "Schema", Some("Grantee")),
            MsSchemaOwner => (
                "Change SQL Server schema owner",
                "Schema",
                Some("Database principal"),
            ),
            MsDatabaseOwner => (
                "Change SQL Server database owner",
                "Current database",
                Some("Server login"),
            ),
        }
    }
    fn role_patch(self) -> bool {
        matches!(self, Self::PgRole | Self::PgAttributes)
    }
    fn membership(self) -> bool {
        matches!(self, Self::PgGrantMembership | Self::PgRevokeMembership)
    }
    fn postgres(self) -> bool {
        !matches!(
            self,
            Self::MsRole
                | Self::MsAddMember
                | Self::MsDropMember
                | Self::MsGrant
                | Self::MsRevoke
                | Self::MsSchemaOwner
                | Self::MsDatabaseOwner
                | Self::MsCreateUser
                | Self::MsRemapUser
                | Self::MsUserSchema
                | Self::MsDropUser
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
    fields: [Entity<TextInput>; 3],
    membership_options: [bool; 3],
    role_options: [Option<bool>; 7],
    source: Option<EditSource>,
}
impl SecurityEditState {
    pub fn new(window: &mut Window, cx: &mut Context<WorkspaceShell>) -> Self {
        let fields = [
            cx.new(|cx| TextInput::new("", "Object or role name", cx)),
            cx.new(|cx| TextInput::new("", "Member, grantee or owner", cx)),
            cx.new(|cx| TextInput::new("", "Explicit grantor role", cx)),
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
            membership_options: [false, true, true],
            role_options: [None; 7],
            source: None,
        }
    }
}

enum Draft {
    Pg(Pg),
    Ms(Ms),
}
fn draft(mode: Mode, first: String, second: String) -> Result<Draft, String> {
    if mode == Mode::MsCreateUser && first.contains(char::from(92)) {
        return Err("SQL login-mapped user names cannot contain a backslash".into());
    }
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
        PgRenameRole => Draft::Pg(Pg::RenameRole {
            name: first,
            new_name: second,
        }),
        PgDropRole => Draft::Pg(Pg::DropRole { name: first }),
        PgAttributes => return Err("Choose public attribute patches in the role form".into()),
        PgGrantMembership | PgRevokeMembership => {
            return Err("Membership changes require an explicit grantor".into())
        }
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
        MsSchemaOwner | MsDatabaseOwner => Draft::Ms(Ms::ChangeOwner {
            object_kind: if mode == MsSchemaOwner {
                sift_protocol::SqlServerOwnedObjectKind::Schema
            } else {
                sift_protocol::SqlServerOwnedObjectKind::Database
            },
            name: first,
            new_owner: second,
        }),
        MsRole => Draft::Ms(Ms::CreateDatabaseRole { name: first }),
        MsCreateUser => Draft::Ms(Ms::CreateDatabaseUser {
            name: first,
            login: second,
        }),
        MsRemapUser => Draft::Ms(Ms::RemapDatabaseUser {
            name: first,
            login: second,
        }),
        MsUserSchema => Draft::Ms(Ms::SetUserDefaultSchema {
            name: first,
            schema: second,
        }),
        MsDropUser => Draft::Ms(Ms::DropDatabaseUser { name: first }),
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
fn membership_draft(
    mode: Mode,
    role: String,
    member: String,
    grantor: String,
    options: [bool; 3],
) -> Result<Draft, String> {
    if [&role, &member, &grantor]
        .iter()
        .any(|name| name.is_empty() || name.len() > 63 || name.chars().any(char::is_control))
    {
        return Err(
            "Enter role, member and explicit grantor names (1..63 bytes, without controls)".into(),
        );
    }
    if role == member {
        return Err("A role cannot be its own member".into());
    }
    Ok(Draft::Pg(if mode == Mode::PgGrantMembership {
        Pg::GrantRoleMembership {
            role,
            member,
            grantor,
            admin_option: options[0],
            inherit_option: options[1],
            set_option: options[2],
        }
    } else {
        Pg::RevokeRoleMembership {
            role,
            member,
            grantor,
        }
    }))
}

fn role_draft(
    mode: Mode,
    name: String,
    flags: [Option<bool>; 7],
    limit: &str,
    validity: &str,
) -> Result<Draft, String> {
    if name.is_empty() || name.len() > 63 || name.chars().any(char::is_control) {
        return Err("Role names require 1..63 bytes without controls".into());
    }
    let connection_limit = if limit.trim().is_empty() {
        None
    } else {
        Some(
            limit
                .trim()
                .parse::<i32>()
                .ok()
                .filter(|limit| *limit >= -1)
                .ok_or("Connection limit must be -1 or a nonnegative integer")?,
        )
    };
    let valid_until = match validity.trim() {
        "" => None,
        "infinity" => Some(sift_protocol::PostgresRoleValidity::Infinite),
        value => Some(sift_protocol::PostgresRoleValidity::Until {
            at: chrono::DateTime::parse_from_rfc3339(value)
                .map_err(|_| "Use an ISO timestamp with timezone or infinity")?
                .with_timezone(&chrono::Utc),
        }),
    };
    let attributes = sift_protocol::PostgresRoleAttributePatch {
        login: flags[0],
        inherit: flags[1],
        create_database: flags[2],
        create_role: flags[3],
        superuser: flags[4],
        replication: flags[5],
        bypass_rls: flags[6],
        connection_limit,
        valid_until,
    };
    Ok(Draft::Pg(if mode == Mode::PgRole {
        if attributes == Default::default() {
            Pg::CreateRole { name }
        } else {
            Pg::CreateRoleWithAttributes { name, attributes }
        }
    } else {
        Pg::AlterRoleAttributes { name, attributes }
    }))
}

impl WorkspaceShell {
    pub(super) fn current_monitor_source(
        &self,
        request_id: uuid::Uuid,
    ) -> Option<MonitorActionSource> {
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
            Memberships => Mode::PgGrantMembership,
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
        self.security_edit.membership_options = [false, true, true];
        self.security_edit.role_options = [None; 7];
        if mode.membership() {
            if let Some(item) = self
                .database_monitor
                .memberships()
                .get(self.database_monitor.objects_selected())
                .cloned()
            {
                for (input, value) in
                    self.security_edit
                        .fields
                        .iter()
                        .zip([item.role, item.member, item.grantor])
                {
                    input.update(cx, |input, cx| input.set_text(value, cx));
                }
                self.security_edit.membership_options =
                    [item.admin_option, item.inherit_option, item.set_option];
            }
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
        let first = self.security_edit.fields[0].read(cx).text().to_owned();
        let second = self.security_edit.fields[1].read(cx).text().to_owned();
        let action = if mode.role_patch() {
            role_draft(
                mode,
                first,
                self.security_edit.role_options,
                &second,
                self.security_edit.fields[2].read(cx).text(),
            )
        } else if mode.membership() {
            membership_draft(
                mode,
                first,
                second,
                self.security_edit.fields[2].read(cx).text().to_owned(),
                self.security_edit.membership_options,
            )
        } else {
            draft(mode, first, second)
        };
        match action {
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
                    current
                        .checked_sub(1)
                        .unwrap_or(self.security_edit.fields.len())
                };
                let count = if self.security_edit.mode.unwrap().membership()
                    || self.security_edit.mode.unwrap().role_patch()
                {
                    3
                } else if self.security_edit.mode.unwrap().labels().2.is_some() {
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
            "1" | "2" | "3"
                if focused.is_none()
                    && self.security_edit.mode == Some(Mode::PgGrantMembership) =>
            {
                let index = key.parse::<usize>().unwrap() - 1;
                self.security_edit.membership_options[index] =
                    !self.security_edit.membership_options[index];
            }
            "1" | "2" | "3" | "4" | "5" | "6" | "7"
                if focused.is_none() && self.security_edit.mode.is_some_and(Mode::role_patch) =>
            {
                let slot = &mut self.security_edit.role_options[key.parse::<usize>().unwrap() - 1];
                *slot = match *slot {
                    None => Some(true),
                    Some(true) => Some(false),
                    Some(false) => None,
                };
                self.database_monitor.clear_object_preview();
            }
            "m" if focused.is_none() => {
                let was_role = matches!(
                    self.security_edit.mode,
                    Some(Mode::PgRole | Mode::PgRenameRole | Mode::PgAttributes | Mode::PgDropRole)
                );
                self.security_edit.mode = self.security_edit.mode.map(Mode::next);
                if was_role {
                    for field in &self.security_edit.fields[1..] {
                        field.update(cx, |input, cx| input.set_text("", cx));
                    }
                    self.security_edit.role_options = [None; 7];
                    if self.security_edit.fields[0].read(cx).text().is_empty() {
                        if let Some(role) = self
                            .database_monitor
                            .roles()
                            .get(self.database_monitor.objects_selected())
                        {
                            let name = role.name.clone();
                            self.security_edit.fields[0]
                                .update(cx, |input, cx| input.set_text(name, cx));
                        }
                    }
                    self.database_monitor.clear_object_preview();
                }
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
            .child(div().w(px(130.)).text_sm().child(label))
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
        .when(mode.role_patch(),|element|element.child(field("Connection limit",&shell.security_edit.fields[1])).child(field("Password expiry",&shell.security_edit.fields[2])).child("Blank keeps the attribute/default. Limit -1 is unlimited; expiry accepts ISO timestamp with timezone or infinity.").child(shell.security_edit.role_options.iter().zip(["LOGIN","INHERIT","CREATEDB","CREATEROLE","SUPERUSER","REPLICATION","BYPASSRLS"]).enumerate().map(|(i,(value,name))|format!("{} {} {}",i+1,name,match value {None=>"keep/default",Some(true)=>"on",Some(false)=>"off"})).collect::<Vec<_>>().join(" · ")))
        .when(mode.membership(),|element| element.child(field("Explicit grantor",&shell.security_edit.fields[2]))
            .child("Cluster-wide role privileges. Other grantors can preserve membership; RESTRICT refuses dependent revocations."))
        .when(mode==Mode::PgGrantMembership,|element|element.child(format!("1 ADMIN {} · 2 INHERIT {} · 3 SET {} (Esc to commands, then toggle)",shell.security_edit.membership_options[0],shell.security_edit.membership_options[1],shell.security_edit.membership_options[2])))
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
        Pg::CreatePolicy { .. }
            | Pg::AlterPolicy { .. }
            | Pg::DropPolicy { .. }
            | Pg::SetRowSecurity { .. }
            | Pg::RenamePolicy { .. }
            | Pg::CreateRole { .. }
            | Pg::CreateRoleWithAttributes { .. }
            | Pg::RenameRole { .. }
            | Pg::AlterRoleAttributes { .. }
            | Pg::DropRole { .. }
            | Pg::GrantRoleMembership { .. }
            | Pg::RevokeRoleMembership { .. }
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
        assert!(draft(
            Mode::MsCreateUser,
            format!("DOMAIN{}user", char::from(92)),
            "sql_login".into()
        )
        .is_err());
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
        workspace.update(&mut cx, |shell, _| {
            shell.database_monitor.finish_object_apply(Ok(()))
        });
        for (keys, role_mode) in [
            ("a escape m", Mode::PgRenameRole),
            ("a escape m m", Mode::PgAttributes),
            ("a escape m m m", Mode::PgDropRole),
        ] {
            cx.simulate_keystrokes(keys);
            workspace.update(&mut cx, |shell, cx| {
                assert_eq!(shell.security_edit.mode, Some(role_mode));
                shell.security_edit.fields[0].update(cx, |input, cx| input.set_text("readers", cx));
                if role_mode == Mode::PgRenameRole {
                    shell.security_edit.fields[1]
                        .update(cx, |input, cx| input.set_text("readers_new", cx));
                }
                if role_mode == Mode::PgAttributes {
                    shell.security_edit.fields[1].update(cx, |input, cx| input.set_text("-1", cx));
                    shell.security_edit.fields[2]
                        .update(cx, |input, cx| input.set_text("infinity", cx));
                }
            });
            if role_mode == Mode::PgAttributes {
                cx.simulate_keystrokes("1");
            }
            cx.simulate_keystrokes("p");
            let (source, action) = match commands.try_recv().unwrap() {
                ExecutorCommand::PreviewPostgresObject { source, action } => (source, action),
                _ => panic!("expected typed role preview"),
            };
            match (&action, role_mode) {
                (Pg::RenameRole { name, new_name }, Mode::PgRenameRole) => assert_eq!(
                    (name.as_str(), new_name.as_str()),
                    ("readers", "readers_new")
                ),
                (Pg::AlterRoleAttributes { attributes, .. }, Mode::PgAttributes) => {
                    assert_eq!(attributes.login, Some(true));
                    assert_eq!(attributes.inherit, None);
                    assert_eq!(attributes.connection_limit, Some(-1));
                    assert_eq!(
                        attributes.valid_until,
                        Some(sift_protocol::PostgresRoleValidity::Infinite)
                    );
                }
                (Pg::DropRole { name }, Mode::PgDropRole) => assert_eq!(name, "readers"),
                _ => panic!("wrong lifecycle action"),
            }
            workspace.update(&mut cx, |shell, cx| {
                shell.on_executor_event(
                    ExecutorEvent::PostgresObjectPreviewed {
                        source,
                        result: Ok(sift_protocol::PostgresObjectPreview {
                            action,
                            sql: "reviewed role SQL".into(),
                            precondition: "e".repeat(64),
                            warning: "Cluster-wide role action".into(),
                            impact: None,
                        }),
                    },
                    cx,
                )
            });
            cx.simulate_keystrokes("x");
            assert!(
                matches!(commands.try_recv(),Ok(ExecutorCommand::ApplyPostgresObject {request,..}) if request.confirmed && request.production_confirmed)
            );
            workspace.update(&mut cx, |shell, _| {
                shell.database_monitor.finish_object_apply(Ok(()))
            });
        }
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
            shell.set_database_monitor_view(DatabaseMonitorView::Memberships, cx);
            shell.automation_focus_handle.focus(window, cx);
        });
        let (source, epoch) = match commands.try_recv().unwrap() {
            ExecutorCommand::LoadPostgresRoleMemberships {
                source,
                epoch,
                offset: 0,
            } => (source, epoch),
            _ => panic!("expected source-bound membership read"),
        };
        let page = || sift_protocol::PostgresObjectPage {
            items: vec![sift_protocol::PostgresRoleMembership {
                role: "readers".into(),
                member: "analyst".into(),
                grantor: "owner".into(),
                admin_option: false,
                inherit_option: true,
                set_option: true,
            }],
            next_offset: None,
        };
        workspace.update(&mut cx, |shell, cx| {
            shell.on_executor_event(
                ExecutorEvent::PostgresRoleMembershipsLoaded {
                    source: source.clone(),
                    epoch: epoch.wrapping_sub(1),
                    offset: 0,
                    result: Ok(page()),
                },
                cx,
            );
            assert!(shell.database_monitor.memberships().is_empty());
            let mut stale = source.clone();
            stale.profile_id += 1;
            shell.on_executor_event(
                ExecutorEvent::PostgresRoleMembershipsLoaded {
                    source: stale,
                    epoch,
                    offset: 0,
                    result: Ok(page()),
                },
                cx,
            );
            assert!(shell.database_monitor.memberships().is_empty());
            let mut wrong_request = source.clone();
            wrong_request.request_id = uuid::Uuid::new_v4();
            shell.on_executor_event(
                ExecutorEvent::PostgresRoleMembershipsLoaded {
                    source: wrong_request,
                    epoch,
                    offset: 0,
                    result: Ok(page()),
                },
                cx,
            );
            assert!(shell.database_monitor.memberships().is_empty());
            assert!(shell.database_monitor.objects_request().loading());
            shell.on_executor_event(
                ExecutorEvent::PostgresRoleMembershipsLoaded {
                    source,
                    epoch,
                    offset: 0,
                    result: Ok(page()),
                },
                cx,
            );
            assert_eq!(shell.database_monitor.memberships().len(), 1);
        });
        cx.simulate_keystrokes("a tab tab");
        workspace.update_in(&mut cx, |shell, window, cx| {
            assert_eq!(shell.security_edit.mode, Some(Mode::PgGrantMembership));
            assert!(shell.security_edit.fields[2]
                .focus_handle(cx)
                .is_focused(window));
            assert_eq!(shell.security_edit.fields[2].read(cx).text(), "owner");
        });
        cx.simulate_keystrokes("escape 1 2 3 p");
        let (action, source) = match commands.try_recv().unwrap() {
            ExecutorCommand::PreviewPostgresObject { action, source } => (action, source),
            _ => panic!("expected membership preview"),
        };
        assert!(
            matches!(&action,Pg::GrantRoleMembership{role,member,grantor,admin_option:true,inherit_option:false,set_option:false} if role=="readers"&&member=="analyst"&&grantor=="owner")
        );
        workspace.update(&mut cx,|shell,cx| {
            shell.on_executor_event(ExecutorEvent::PostgresObjectPreviewed{source,result:Ok(sift_protocol::PostgresObjectPreview{action,sql:"GRANT readers TO analyst WITH ADMIN TRUE, INHERIT FALSE, SET FALSE GRANTED BY owner".into(),precondition:"a".repeat(64),warning:"Cluster-wide role privileges".into(),impact:None})},cx);
        });
        cx.simulate_keystrokes("enter");
        assert!(commands.try_recv().is_err());
        cx.simulate_keystrokes("x");
        assert!(
            matches!(commands.try_recv(),Ok(ExecutorCommand::ApplyPostgresObject{request,..}) if request.confirmed&&request.production_confirmed)
        );
        workspace.update(&mut cx, |shell, _| {
            shell.database_monitor.finish_object_apply(Ok(()))
        });
        cx.simulate_keystrokes("a escape m p");
        assert!(
            matches!(commands.try_recv(),Ok(ExecutorCommand::PreviewPostgresObject{action:Pg::RevokeRoleMembership{role,member,grantor},..}) if role=="readers"&&member=="analyst"&&grantor=="owner")
        );
        workspace.update(&mut cx, |shell, _| {
            shell.database_monitor.clear_object_preview()
        });
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
        for (mode, steps, kind, name, owner) in [
            (
                Mode::MsSchemaOwner,
                5,
                sift_protocol::SqlServerOwnedObjectKind::Schema,
                " sales ",
                "schema_owner",
            ),
            (
                Mode::MsDatabaseOwner,
                6,
                sift_protocol::SqlServerOwnedObjectKind::Database,
                "app",
                "server_login",
            ),
        ] {
            workspace.update_in(&mut cx, |shell, window, cx| {
                shell.database_monitor.finish_security_apply(Ok(()));
                shell.automation_focus_handle.focus(window, cx);
            });
            cx.simulate_keystrokes("a");
            cx.simulate_keystrokes("escape");
            for _ in 0..steps {
                cx.simulate_keystrokes("m");
            }
            cx.simulate_keystrokes("i");
            workspace.update(&mut cx, |shell, cx| {
                assert_eq!(shell.security_edit.mode, Some(mode));
                shell.security_edit.fields[0].update(cx, |input, cx| input.set_text(name, cx));
                shell.security_edit.fields[1].update(cx, |input, cx| input.set_text(owner, cx));
            });
            cx.simulate_keystrokes("enter");
            let (action, source) = match commands.try_recv().unwrap() {
                ExecutorCommand::PreviewSqlServerSecurity { action, source } => (action, source),
                _ => panic!("expected ownership preview only"),
            };
            assert!(
                matches!(&action, Ms::ChangeOwner { object_kind, name: actual, new_owner } if *object_kind == kind && actual == name && new_owner == owner)
            );
            assert!(commands.try_recv().is_err());
            workspace.update(&mut cx, |shell, cx| {
                shell.on_executor_event(
                    ExecutorEvent::SqlServerSecurityPreviewed {
                        source,
                        result: Ok(sift_protocol::SqlServerSecurityPreview {
                            action,
                            sql: "reviewed ownership SQL".into(),
                            precondition: "c".repeat(64),
                            warning: "Review permission loss".into(),
                        }),
                    },
                    cx,
                )
            });
            cx.simulate_keystrokes("enter");
            assert!(
                matches!(commands.try_recv().unwrap(), ExecutorCommand::ApplySqlServerSecurity { request, .. } if request.production_confirmed && request.precondition == "c".repeat(64))
            );
        }
        for (mode, steps, expected, first, second) in [
            (
                Mode::MsCreateUser,
                7,
                Ms::CreateDatabaseUser {
                    name: "user]'; --".into(),
                    login: "existing_login".into(),
                },
                "user]'; --",
                "existing_login",
            ),
            (
                Mode::MsRemapUser,
                8,
                Ms::RemapDatabaseUser {
                    name: "analyst".into(),
                    login: "replacement_login".into(),
                },
                "analyst",
                "replacement_login",
            ),
            (
                Mode::MsUserSchema,
                9,
                Ms::SetUserDefaultSchema {
                    name: "analyst".into(),
                    schema: "sales".into(),
                },
                "analyst",
                "sales",
            ),
            (
                Mode::MsDropUser,
                10,
                Ms::DropDatabaseUser {
                    name: "analyst".into(),
                },
                "analyst",
                "ignored",
            ),
        ] {
            workspace.update_in(&mut cx, |shell, window, cx| {
                shell.database_monitor.finish_security_apply(Ok(()));
                shell.automation_focus_handle.focus(window, cx);
            });
            cx.simulate_keystrokes("a escape");
            for _ in 0..steps {
                cx.simulate_keystrokes("m");
            }
            cx.simulate_keystrokes("i");
            workspace.update(&mut cx, |shell, cx| {
                assert_eq!(shell.security_edit.mode, Some(mode));
                shell.security_edit.fields[0].update(cx, |field, cx| field.set_text(first, cx));
                shell.security_edit.fields[1].update(cx, |field, cx| field.set_text(second, cx));
            });
            cx.simulate_keystrokes("enter");
            let (action, source) = match commands.try_recv().unwrap() {
                ExecutorCommand::PreviewSqlServerSecurity { action, source } => (action, source),
                _ => panic!("expected typed user preview only"),
            };
            assert_eq!(
                serde_json::to_value(&action).unwrap(),
                serde_json::to_value(expected).unwrap()
            );
            assert!(commands.try_recv().is_err());
            workspace.update(&mut cx, |shell, cx| {
                shell.on_executor_event(
                    ExecutorEvent::SqlServerSecurityPreviewed {
                        source,
                        result: Ok(sift_protocol::SqlServerSecurityPreview {
                            action,
                            sql: "reviewed user SQL".into(),
                            precondition: "d".repeat(64),
                            warning: "Review access and login survival".into(),
                        }),
                    },
                    cx,
                )
            });
            if mode == Mode::MsDropUser {
                workspace.update(&mut cx, |shell, _| {
                    shell.active_query_connection = Some((
                        7,
                        sift_protocol::SessionId(4),
                        sift_protocol::ConnectionId(99),
                    ))
                });
                cx.simulate_keystrokes("enter");
                assert!(
                    commands.try_recv().is_err(),
                    "a changed source cannot apply a user drop"
                );
            } else {
                cx.simulate_keystrokes("enter");
                assert!(
                    matches!(commands.try_recv().unwrap(),ExecutorCommand::ApplySqlServerSecurity {request,..} if request.production_confirmed && request.precondition=="d".repeat(64))
                );
            }
        }
    }
}
