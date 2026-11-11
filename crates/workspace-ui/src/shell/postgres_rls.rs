//! Typed RLS drafts preserve omitted ALTER fields and never reuse lossy catalog text.
use super::*;
use sift_protocol::{
    PostgresObjectAction as Action, PostgresPolicyCommand as Command, PostgresPolicyDefinition,
    PostgresPolicyRole as Role, PostgresRowSecuritySetting as Setting,
};
use sqlparser::{
    dialect::PostgreSqlDialect,
    tokenizer::{Token, Tokenizer},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Create,
    Alter,
    Drop,
    Enable,
    Disable,
    Force,
    NoForce,
}
impl Mode {
    fn next(self) -> Self {
        match self {
            Self::Create => Self::Alter,
            Self::Alter => Self::Drop,
            Self::Drop => Self::Enable,
            Self::Enable => Self::Disable,
            Self::Disable => Self::Force,
            Self::Force => Self::NoForce,
            Self::NoForce => Self::Create,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Create => "Create policy",
            Self::Alter => "Alter policy",
            Self::Drop => "Drop policy",
            Self::Enable => "Enable row security",
            Self::Disable => "Disable row security",
            Self::Force => "Force row security for owner",
            Self::NoForce => "Remove owner force",
        }
    }
    fn field_count(self) -> usize {
        match self {
            Self::Create | Self::Alter => 6,
            Self::Drop => 3,
            _ => 2,
        }
    }
}
fn next_command(command: Command) -> Command {
    match command {
        Command::All => Command::Select,
        Command::Select => Command::Insert,
        Command::Insert => Command::Update,
        Command::Update => Command::Delete,
        Command::Delete => Command::All,
    }
}
fn name(value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > 63 || value.chars().any(char::is_control) {
        Err("Names require 1–63 bytes without controls".into())
    } else {
        Ok(())
    }
}
fn roles(input: &str) -> Result<Vec<Role>, String> {
    let tokens = Tokenizer::new(&PostgreSqlDialect {}, input)
        .tokenize()
        .map_err(|_| "Enter comma-separated role names; double-quote special names".to_owned())?;
    let mut result = vec![];
    let mut expecting_name = true;
    for token in tokens
        .into_iter()
        .filter(|t| !matches!(t, Token::Whitespace(_)))
    {
        match token {
            Token::Word(word) if expecting_name => {
                name(&word.value)?;
                result.push(
                    if word.quote_style.is_none() && word.value.eq_ignore_ascii_case("public") {
                        Role::Public
                    } else {
                        Role::Named(word.value)
                    },
                );
                expecting_name = false;
            }
            Token::Comma if !expecting_name => expecting_name = true,
            _ => {
                return Err(
                    "Use only comma-separated role names; double-quote special names".into(),
                )
            }
        }
    }
    if expecting_name || result.len() > 32 {
        return Err("Enter 1–32 explicit roles (PUBLIC includes everyone)".into());
    }
    Ok(result)
}
fn draft(
    mode: Mode,
    command: Command,
    permissive: bool,
    fields: [String; 6],
) -> Result<Action, String> {
    let [schema, table, policy, role_text, using, check] = fields;
    name(&schema)?;
    name(&table)?;
    if mode.field_count() >= 3 {
        name(&policy)?;
    }
    let optional = |value: String| {
        if value.trim().is_empty() {
            None
        } else {
            Some(value)
        }
    };
    Ok(match mode {
        Mode::Create => Action::CreatePolicy {
            schema,
            table,
            name: policy,
            definition: PostgresPolicyDefinition {
                command,
                permissive,
                roles: roles(&role_text)?,
                using_expression: optional(using),
                check_expression: optional(check),
            },
        },
        Mode::Alter => {
            let roles = if role_text.trim().is_empty() {
                None
            } else {
                Some(roles(&role_text)?)
            };
            let using_expression = optional(using);
            let check_expression = optional(check);
            if roles.is_none() && using_expression.is_none() && check_expression.is_none() {
                return Err(
                    "Enter roles or a condition to change; blank ALTER fields stay unchanged"
                        .into(),
                );
            }
            Action::AlterPolicy {
                schema,
                table,
                name: policy,
                roles,
                using_expression,
                check_expression,
            }
        }
        Mode::Drop => Action::DropPolicy {
            schema,
            table,
            name: policy,
        },
        _ => Action::SetRowSecurity {
            schema,
            table,
            setting: match mode {
                Mode::Enable => Setting::Enable,
                Mode::Disable => Setting::Disable,
                Mode::Force => Setting::Force,
                Mode::NoForce => Setting::NoForce,
                _ => unreachable!(),
            },
        },
    })
}

pub(super) struct RlsEditState {
    pub active: bool,
    mode: Mode,
    command: Command,
    permissive: bool,
    fields: [Entity<TextInput>; 6],
    source: Option<MonitorActionSource>,
}
impl RlsEditState {
    pub fn new(window: &mut Window, cx: &mut Context<WorkspaceShell>) -> Self {
        let fields = [
            "Schema",
            "Table",
            "Policy name",
            "Roles, e.g. PUBLIC or readers",
            "USING expression",
            "WITH CHECK expression",
        ]
        .map(|placeholder| cx.new(|cx| TextInput::new("", placeholder, cx)));
        for field in &fields {
            cx.subscribe_in(
                field,
                window,
                |shell, _, event: &TextInputEvent, window, cx| {
                    if *event == TextInputEvent::Submitted && shell.rls_edit.active {
                        shell.preview_rls_edit(window, cx);
                    }
                },
            )
            .detach();
        }
        Self {
            active: false,
            mode: Mode::Create,
            command: Command::All,
            permissive: true,
            fields,
            source: None,
        }
    }
}
impl WorkspaceShell {
    pub(super) fn begin_rls_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.database_monitor.view() != DatabaseMonitorView::Policies
            || self.database_monitor.object_action_request().loading()
        {
            return;
        }
        let Some(source) = self.current_monitor_source(uuid::Uuid::new_v4()) else {
            self.show_error_toast("Connect a database before editing row security".into(), cx);
            return;
        };
        let selected = self
            .database_monitor
            .policies()
            .get(self.database_monitor.objects_selected());
        let initial = if let Some(policy) = selected {
            // Conditions and role labels may be truncated. Never turn display text into a mutation.
            [
                policy.schema.clone(),
                policy.table.clone(),
                policy.name.clone(),
                String::new(),
                String::new(),
                String::new(),
            ]
        } else {
            [
                String::new(),
                String::new(),
                String::new(),
                "PUBLIC".into(),
                String::new(),
                String::new(),
            ]
        };
        self.rls_edit.mode = if selected.is_some() {
            Mode::Alter
        } else {
            Mode::Create
        };
        for (input, value) in self.rls_edit.fields.iter().zip(initial) {
            input.update(cx, |input, cx| input.set_text(value, cx));
        }
        self.rls_edit.command = Command::All;
        self.rls_edit.permissive = true;
        self.rls_edit.source = Some(source);
        self.rls_edit.active = true;
        self.policy_rename_active = false;
        self.monitor_action_scope = None;
        self.database_monitor.clear_object_preview();
        self.rls_edit.fields[0].focus_handle(cx).focus(window, cx);
        cx.notify();
    }
    pub(super) fn preview_rls_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let source_matches = self.rls_edit.source.as_ref().is_some_and(|source| {
            self.current_monitor_source(source.request_id).as_ref() == Some(source)
        });
        if !source_matches || self.database_monitor.view() != DatabaseMonitorView::Policies {
            self.rls_edit.active = false;
            self.show_error_toast(
                "Database context changed; reopen row security editor".into(),
                cx,
            );
            return;
        }
        let fields =
            std::array::from_fn(|index| self.rls_edit.fields[index].read(cx).text().to_owned());
        match draft(
            self.rls_edit.mode,
            self.rls_edit.command,
            self.rls_edit.permissive,
            fields,
        ) {
            Ok(action) => {
                self.rls_edit.active = false;
                self.automation_focus_handle.focus(window, cx);
                self.preview_postgres_object(action, cx);
            }
            Err(message) => self.show_error_toast(message, cx),
        }
    }
    pub(super) fn rls_edit_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focused = self
            .rls_edit
            .fields
            .iter()
            .position(|input| input.focus_handle(cx).is_focused(window));
        let key = event.keystroke.unparse();
        match key.as_str() {
            "escape" if focused.is_some() => self.automation_focus_handle.focus(window, cx),
            "escape" => self.rls_edit.active = false,
            "tab" | "shift-tab" if focused.is_some() => {
                let index = focused.unwrap();
                let next = if key == "tab" {
                    index + 1
                } else {
                    index.checked_sub(1).unwrap_or(6)
                };
                if next < self.rls_edit.mode.field_count() {
                    self.rls_edit.fields[next]
                        .focus_handle(cx)
                        .focus(window, cx);
                } else {
                    self.automation_focus_handle.focus(window, cx);
                }
            }
            "m" if focused.is_none() => self.rls_edit.mode = self.rls_edit.mode.next(),
            "c" if focused.is_none() && self.rls_edit.mode == Mode::Create => {
                self.rls_edit.command = next_command(self.rls_edit.command)
            }
            "v" if focused.is_none() && self.rls_edit.mode == Mode::Create => {
                self.rls_edit.permissive = !self.rls_edit.permissive
            }
            "i" if focused.is_none() => self.rls_edit.fields[0].focus_handle(cx).focus(window, cx),
            "p" if focused.is_none() => self.preview_rls_edit(window, cx),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }
}
pub(super) fn render_form(shell: &WorkspaceShell, cx: &mut Context<WorkspaceShell>) -> AnyElement {
    let state = &shell.rls_edit;
    let labels = ["Schema", "Table", "Policy", "Roles", "USING", "WITH CHECK"];
    div().id("postgres-rls-form").flex_1().min_h_0().overflow_y_scroll().flex().flex_col().gap_1().p_2()
        .child(state.mode.label())
        .when(state.mode == Mode::Create, |form| form.child(format!("Command: {:?} · {}", state.command,
            if state.permissive { "Permissive" } else { "Restrictive" })))
        .children(state.fields.iter().zip(labels).take(state.mode.field_count()).map(|(input, label)|
            div().flex().items_center().gap_2().child(div().w(px(95.)).text_sm().child(label))
                .child(div().flex_1().min_w_0().child(input.clone()))))
        .child(div().text_xs().child(if state.mode == Mode::Alter {
            "Blank ALTER fields stay unchanged. Enter true to allow all rows. Command/mode changes require separate reviewed drop/create."
        } else { "PUBLIC includes everyone. Missing CREATE conditions allow all rows; restrictive policies need a permissive policy too." }))
        .child(div().text_xs().child("Tab next · Esc command/cancel · i edit · m action · c command · v permissive/restrictive · p preview"))
        .child(Button::new("postgres-rls-preview", "Preview exact SQL").on_click(cx.listener(|shell, _, window, cx| shell.preview_rls_edit(window, cx))))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn policy_drafts_preserve_native_semantics_and_omissions() {
        assert_eq!(
            roles("PUBLIC, \"public\", \"comma,role\"").unwrap(),
            vec![
                Role::Public,
                Role::Named("public".into()),
                Role::Named("comma,role".into())
            ]
        );
        for input in ["", "PUBLIC,", "PUBLIC; DROP ROLE readers"] {
            assert!(roles(input).is_err(), "{input}");
        }
        let fields = ["s", "t", "p", "", "false", ""].map(str::to_owned);
        assert!(matches!(
            draft(Mode::Alter, Command::All, true, fields).unwrap(),
            Action::AlterPolicy {
                roles: None,
                using_expression: Some(_),
                check_expression: None,
                ..
            }
        ));
        let fields = ["s", "t", "p", "PUBLIC", "", ""].map(str::to_owned);
        assert!(matches!(
            draft(Mode::Create, Command::Insert, false, fields).unwrap(),
            Action::CreatePolicy {
                definition: PostgresPolicyDefinition {
                    command: Command::Insert,
                    permissive: false,
                    ..
                },
                ..
            }
        ));
        let fields = ["s", "t", "", "", "", ""].map(str::to_owned);
        assert!(matches!(
            draft(Mode::Force, Command::All, true, fields).unwrap(),
            Action::SetRowSecurity {
                setting: Setting::Force,
                ..
            }
        ));
    }
    #[gpui::test]
    fn rls_editor_preserves_omissions_and_requires_current_source_and_review(
        cx: &mut gpui::TestAppContext,
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
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
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
            shell
                .database_monitor
                .set_view(DatabaseMonitorView::Policies);
            shell.database_monitor.finish_policies(
                0,
                Ok(sift_protocol::PostgresObjectPage {
                    items: vec![sift_protocol::PostgresPolicy {
                        schema: "proof".into(),
                        table: "rows".into(),
                        name: "readable".into(),
                        command: "r".into(),
                        permissive: false,
                        roles: "truncated,...".into(),
                        roles_truncated: true,
                        using_expression: Some("truncated...".into()),
                        using_truncated: true,
                        check_expression: None,
                        check_truncated: false,
                        row_security_enabled: true,
                        row_security_forced: false,
                    }],
                    next_offset: None,
                }),
            );
            shell.automation_focus_handle.focus(window, cx);
            cx.notify();
        });
        cx.simulate_keystrokes("a");
        workspace.update(&mut cx, |shell, cx| {
            assert!(shell.rls_edit.active);
            assert_eq!(shell.rls_edit.mode, Mode::Alter);
            for field in &shell.rls_edit.fields[3..] {
                assert!(field.read(cx).text().is_empty());
            }
            shell.rls_edit.fields[4].update(cx, |input, cx| input.set_text("false", cx));
        });
        cx.simulate_keystrokes("enter");
        let (action, source) = match commands.try_recv().unwrap() {
            ExecutorCommand::PreviewPostgresObject { action, source } => (action, source),
            _ => panic!("expected preview"),
        };
        assert!(
            matches!(&action, Action::AlterPolicy { roles: None, using_expression: Some(expr), check_expression: None, .. } if expr == "false")
        );
        assert!(commands.try_recv().is_err());
        workspace.update(&mut cx, |shell, cx| {
            shell.on_executor_event(
                ExecutorEvent::PostgresObjectPreviewed {
                    source,
                    result: Ok(sift_protocol::PostgresObjectPreview {
                        action,
                        sql: "ALTER POLICY readable ON proof.rows USING (false)".into(),
                        precondition: "reviewed".into(),
                        warning: String::new(),
                        impact: None,
                    }),
                },
                cx,
            );
            assert!(shell.database_monitor.object_preview().is_some());
        });
        cx.simulate_keystrokes("enter");
        assert!(commands.try_recv().is_err());
        cx.simulate_keystrokes("x");
        assert!(
            matches!(commands.try_recv().unwrap(), ExecutorCommand::ApplyPostgresObject { request, .. } if request.production_confirmed && request.precondition == "reviewed")
        );
        workspace.update_in(&mut cx, |shell, window, cx| {
            shell.database_monitor.finish_object_apply(Ok(()));
            shell.begin_rls_edit(window, cx);
            shell.rls_edit.fields[4].update(cx, |input, cx| input.set_text("true", cx));
            shell.selected_instance_id = Some("different".into());
            shell.preview_rls_edit(window, cx);
            assert!(!shell.rls_edit.active);
        });
        assert!(commands.try_recv().is_err());
        workspace.update_in(&mut cx, |shell, window, cx| {
            shell.selected_instance_id = Some("fixture".into());
            shell.begin_policy_rename(window, cx);
            assert!(shell.policy_rename_active);
            shell
                .policy_rename_input
                .update(cx, |input, cx| input.set_text("renamed", cx));
            shell.selected_instance_id = Some("different".into());
            shell.preview_policy_rename(window, cx);
            assert!(!shell.policy_rename_active);
        });
        assert!(commands.try_recv().is_err());
    }
}
