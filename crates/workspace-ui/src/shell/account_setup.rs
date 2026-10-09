//! Account onboarding and invitations use transient, instance-scoped state.
use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountScope {
    pub instance_id: String,
    pub principal_id: Option<i64>,
    pub generation: u64,
}

#[derive(Debug, Clone)]
pub enum AccountAction {
    LoadMethods,
    ListInvitations {
        tenant_id: i64,
    },
    CreateInvitation {
        tenant_id: i64,
        request: sift_protocol::CreateTenantInvitationRequest,
    },
    RevokeInvitation {
        tenant_id: i64,
        invitation_id: i64,
    },
    AcceptInvitation {
        request: sift_protocol::AcceptTenantInvitationRequest,
    },
}

#[derive(Debug, Clone)]
pub enum AccountReply {
    Methods(sift_protocol::AuthMethodsResponse),
    Invitations {
        tenant_id: i64,
        invitations: Vec<sift_api_types::TenantInvitation>,
    },
    Issued {
        tenant_id: i64,
        invitation: sift_protocol::IssuedTenantInvitationResponse,
    },
    Accepted(sift_protocol::WhoAmIResponse),
}

#[derive(Debug, Clone)]
pub enum GithubOwnerSetupEvent {
    Authorization {
        url: String,
        code: sift_protocol::RedactedString,
    },
    Linked(sift_protocol::WhoAmIResponse),
    Failed(String),
}

pub(super) struct AccountSetup {
    generation: u64,
    scope: Option<AccountScope>,
    pub methods: Option<sift_protocol::AuthMethodsResponse>,
    pub methods_pending: bool,
    pub owner_pending: bool,
    pub device_code: Option<String>,
    pub device_url: Option<String>,
    pub invitations_open: bool,
    pub tenant_id: Option<i64>,
    pub role: sift_protocol::InvitationRole,
    pub target_input: Entity<TextInput>,
    pub accept_input: Entity<TextInput>,
    pub invitations: Vec<sift_api_types::TenantInvitation>,
    pub invitation_cursor: usize,
    pub issued: Option<sift_protocol::IssuedTenantInvitationResponse>,
    pub pending: bool,
    pub error: Option<String>,
}

impl AccountSetup {
    pub fn new(cx: &mut Context<WorkspaceShell>) -> Self {
        let state = Self {
            generation: 0,
            scope: None,
            methods: None,
            methods_pending: false,
            owner_pending: false,
            device_code: None,
            device_url: None,
            invitations_open: false,
            tenant_id: None,
            role: sift_protocol::InvitationRole::Member,
            target_input: cx.new(|cx| TextInput::new("", "Principal ID (optional)", cx)),
            accept_input: cx.new(|cx| TextInput::new("", "Invitation token", cx).masked()),
            invitations: vec![],
            invitation_cursor: 0,
            issued: None,
            pending: false,
            error: None,
        };
        cx.subscribe(&state.accept_input, |shell, _, event, cx| {
            if *event == TextInputEvent::Submitted
                && shell.modal == Some(Modal::Account)
                && shell.account_setup.invitations_open
            {
                shell.accept_account_invitation(cx);
            }
        })
        .detach();
        cx.subscribe(&state.target_input, |shell, _, event, cx| {
            if *event == TextInputEvent::Submitted
                && shell.modal == Some(Modal::Account)
                && shell.account_setup.invitations_open
            {
                shell.issue_account_invitation(cx);
            }
        })
        .detach();
        state
    }
}

impl WorkspaceShell {
    pub(super) fn account_scope(&self) -> AccountScope {
        AccountScope {
            instance_id: self
                .lifecycle
                .selected_instance
                .as_ref()
                .map(|i| i.id.clone())
                .or_else(|| self.selected_instance_id.clone())
                .unwrap_or_else(|| "local".into()),
            principal_id: self.lifecycle.identity.as_ref().map(|i| i.principal.id),
            generation: self.account_setup.generation,
        }
    }

    pub(super) fn reset_account_setup(&mut self, cx: &mut Context<Self>) {
        if self.account_setup.owner_pending {
            if let Some(sender) = &self.instance_sender {
                let _ = sender.send(InstanceCommand::CancelGithubOwnerSetup);
            }
        }
        self.account_setup.generation = self.account_setup.generation.wrapping_add(1);
        self.account_setup.scope = None;
        self.account_setup.methods = None;
        self.account_setup.methods_pending = false;
        self.account_setup.owner_pending = false;
        self.account_setup.device_code = None;
        self.account_setup.device_url = None;
        self.account_setup.invitations_open = false;
        self.account_setup.pending = false;
        self.account_setup.tenant_id = None;
        self.account_setup.invitations.clear();
        self.account_setup.invitation_cursor = 0;
        self.account_setup.issued = None;
        self.account_setup.error = None;
        for input in [
            &self.account_setup.accept_input,
            &self.account_setup.target_input,
        ] {
            input.update(cx, |input, cx| input.set_text("", cx));
        }
    }

    pub(super) fn sync_account_setup(&mut self, cx: &mut Context<Self>) {
        if self.modal != Some(Modal::Account) && self.account_setup.scope.is_some() {
            self.reset_account_setup(cx);
            return;
        }
        if self
            .account_setup
            .scope
            .as_ref()
            .is_some_and(|scope| *scope != self.account_scope())
        {
            self.reset_account_setup(cx);
            if self.modal == Some(Modal::Account) {
                self.load_account_methods(cx);
            }
        }
    }

    pub(super) fn load_account_methods(&mut self, cx: &mut Context<Self>) {
        self.account_setup.scope = Some(self.account_scope());
        self.account_setup.methods_pending = true;
        self.send_account_action(AccountAction::LoadMethods, cx);
    }

    fn send_account_action(&mut self, action: AccountAction, cx: &mut Context<Self>) {
        let scope = self.account_scope();
        self.account_setup.scope = Some(scope.clone());
        self.account_setup.error = None;
        if !matches!(action, AccountAction::LoadMethods) {
            self.account_setup.pending = true;
        }
        if self.executor_sender.as_ref().is_none_or(|sender| {
            sender
                .send(ExecutorCommand::Account { scope, action })
                .is_err()
        }) {
            self.account_setup.pending = false;
            self.account_setup.methods_pending = false;
            self.account_setup.error = Some("Account service is unavailable".into());
        }
        cx.notify();
    }

    pub(super) fn on_account_reply(
        &mut self,
        scope: AccountScope,
        result: Result<AccountReply, String>,
        cx: &mut Context<Self>,
    ) {
        if self.modal != Some(Modal::Account) || scope != self.account_scope() {
            return;
        }
        self.account_setup.pending = false;
        self.account_setup.methods_pending = false;
        match result {
            Ok(AccountReply::Methods(methods)) => self.account_setup.methods = Some(methods),
            Ok(AccountReply::Invitations {
                tenant_id,
                invitations,
            }) => {
                if self.account_setup.tenant_id == Some(tenant_id) {
                    self.account_setup.invitations = invitations;
                }
            }
            Ok(AccountReply::Issued {
                tenant_id,
                invitation,
            }) => {
                if self.account_setup.tenant_id == Some(tenant_id) {
                    self.account_setup.issued = Some(invitation);
                    self.load_account_invitations(cx);
                }
            }
            Ok(AccountReply::Accepted(identity)) => {
                self.account_setup
                    .accept_input
                    .update(cx, |input, cx| input.set_text("", cx));
                self.lifecycle
                    .apply(LifecycleEvent::Authenticated(identity));
                // Membership changes require the navigation projection to reload.
                if let Some(sender) = &self.instance_sender {
                    let _ = sender.send(InstanceCommand::RefreshNavigation);
                }
                self.show_success_toast(
                    "Joined the workspace; room access is granted separately".into(),
                    cx,
                );
            }
            Err(error) => self.account_setup.error = Some(error),
        }
        cx.notify();
    }

    pub(super) fn start_github_owner_setup(&mut self, cx: &mut Context<Self>) {
        if self.account_setup.owner_pending {
            return;
        }
        self.account_setup.error = None;
        self.account_setup.owner_pending = true;
        let scope = self.account_scope();
        self.account_setup.scope = Some(scope.clone());
        if self.instance_sender.as_ref().is_none_or(|sender| {
            sender
                .send(InstanceCommand::LinkGithubOwner { scope })
                .is_err()
        }) {
            self.account_setup.owner_pending = false;
            self.account_setup.error = Some("Local account manager is unavailable".into());
        }
        cx.notify();
    }

    pub(super) fn on_github_owner_setup(
        &mut self,
        scope: AccountScope,
        event: GithubOwnerSetupEvent,
        cx: &mut Context<Self>,
    ) {
        if scope != self.account_scope() || self.modal != Some(Modal::Account) {
            return;
        }
        match event {
            GithubOwnerSetupEvent::Authorization { url, code } => {
                self.account_setup.device_code = Some(code.0);
                self.account_setup.device_url = Some(url.clone());
                cx.open_url(&url);
            }
            GithubOwnerSetupEvent::Linked(identity) => {
                self.account_setup.owner_pending = false;
                self.account_setup.device_code = None;
                self.account_setup.device_url = None;
                self.lifecycle
                    .apply(LifecycleEvent::Authenticated(identity));
                self.show_success_toast("GitHub owner verified".into(), cx);
            }
            GithubOwnerSetupEvent::Failed(error) => {
                self.account_setup.owner_pending = false;
                self.account_setup.device_code = None;
                self.account_setup.device_url = None;
                self.account_setup.error = Some(error);
            }
        }
        cx.notify();
    }

    pub(super) fn toggle_account_invitations(&mut self, cx: &mut Context<Self>) {
        if self.account_setup.pending || self.account_setup.methods_pending {
            return;
        }
        self.account_setup.invitations_open = !self.account_setup.invitations_open;
        self.account_setup.issued = None;
        self.account_setup
            .accept_input
            .update(cx, |input, cx| input.set_text("", cx));
        if self.account_setup.invitations_open {
            self.account_setup.tenant_id = self.lifecycle.identity.as_ref().and_then(|i| {
                i.memberships
                    .iter()
                    .find(|m| matches!(m.role.as_str(), "owner" | "admin"))
                    .map(|m| m.tenant_id)
            });
            self.load_account_invitations(cx);
        }
        cx.notify();
    }

    fn load_account_invitations(&mut self, cx: &mut Context<Self>) {
        if let Some(tenant_id) = self.account_setup.tenant_id {
            self.send_account_action(AccountAction::ListInvitations { tenant_id }, cx);
        }
    }

    pub(super) fn select_invitation_tenant(&mut self, tenant_id: i64, cx: &mut Context<Self>) {
        if self.account_setup.pending {
            return;
        }
        self.account_setup.tenant_id = Some(tenant_id);
        self.account_setup.issued = None;
        self.account_setup.invitations.clear();
        self.account_setup.invitation_cursor = 0;
        self.load_account_invitations(cx);
    }

    pub(super) fn issue_account_invitation(&mut self, cx: &mut Context<Self>) {
        if self.account_setup.pending {
            return;
        }
        let Some(tenant_id) = self.account_setup.tenant_id else {
            return;
        };
        let value = self.account_setup.target_input.read(cx).text().trim();
        let target_principal_id = if value.is_empty() {
            None
        } else {
            match value.parse::<i64>() {
                Ok(id) if id > 0 => Some(id),
                _ => {
                    self.account_setup.error =
                        Some("Enter a positive Sift principal ID, or leave it empty".into());
                    cx.notify();
                    return;
                }
            }
        };
        self.account_setup.issued = None;
        self.send_account_action(
            AccountAction::CreateInvitation {
                tenant_id,
                request: sift_protocol::CreateTenantInvitationRequest {
                    role: self.account_setup.role,
                    target_principal_id,
                    expires_at: chrono::Utc::now() + chrono::Duration::days(7),
                },
            },
            cx,
        );
    }

    pub(super) fn accept_account_invitation(&mut self, cx: &mut Context<Self>) {
        if self.account_setup.pending {
            return;
        }
        let token = self
            .account_setup
            .accept_input
            .read(cx)
            .text()
            .trim()
            .to_owned();
        if token.is_empty() {
            return;
        }
        self.account_setup
            .accept_input
            .update(cx, |input, cx| input.set_text("", cx));
        self.send_account_action(
            AccountAction::AcceptInvitation {
                request: sift_protocol::AcceptTenantInvitationRequest { token },
            },
            cx,
        );
    }

    fn active_account_invitations(&self) -> Vec<&sift_api_types::TenantInvitation> {
        self.account_setup
            .invitations
            .iter()
            .filter(|i| {
                i.revoked_at.is_none()
                    && i.consumed_at.is_none()
                    && i.expires_at > chrono::Utc::now()
            })
            .collect()
    }

    pub(super) fn navigate_account_invitation(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.active_account_invitations().len();
        self.account_setup.invitation_cursor = self
            .account_setup
            .invitation_cursor
            .saturating_add_signed(delta)
            .min(count.saturating_sub(1));
        cx.notify();
    }

    pub(super) fn revoke_selected_account_invitation(&mut self, cx: &mut Context<Self>) {
        if self.account_setup.pending {
            return;
        }
        let selected = self
            .active_account_invitations()
            .get(self.account_setup.invitation_cursor)
            .map(|i| (i.tenant_id.0, i.id.0));
        if let Some((tenant_id, invitation_id)) = selected {
            self.send_account_action(
                AccountAction::RevokeInvitation {
                    tenant_id,
                    invitation_id,
                },
                cx,
            );
        }
    }

    pub(super) fn cycle_account_invitation_tenant(&mut self, delta: isize, cx: &mut Context<Self>) {
        let tenants = self
            .lifecycle
            .identity
            .as_ref()
            .map(|i| {
                i.memberships
                    .iter()
                    .filter(|m| matches!(m.role.as_str(), "owner" | "admin"))
                    .map(|m| m.tenant_id)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if tenants.is_empty() {
            return;
        }
        let current = tenants
            .iter()
            .position(|id| Some(*id) == self.account_setup.tenant_id)
            .unwrap_or(0);
        let next = (current as isize + delta).rem_euclid(tenants.len() as isize) as usize;
        self.select_invitation_tenant(tenants[next], cx);
    }

    pub(super) fn render_account_setup(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors;
        let local = self
            .lifecycle
            .selected_instance
            .as_ref()
            .is_some_and(|i| i.kind == crate::InstanceKind::Local);
        let linked = self
            .lifecycle
            .identity
            .as_ref()
            .is_some_and(|i| i.github_login.is_some());
        let available = self
            .account_setup
            .methods
            .as_ref()
            .is_some_and(|m| m.github_owner_device);
        let waiting = self.account_setup.owner_pending;
        div()
            .border_t_1()
            .border_color(colors.subtle_border)
            .px_3()
            .py_2()
            .flex()
            .flex_col()
            .gap_2()
            .when(local, |view| {
                view.child(
                    div()
                        .text_xs()
                        .text_color(colors.muted_text)
                        .whitespace_normal()
                        .child("GitHub sign-in is optional. Local access works offline."),
                )
                .child(
                    Button::new(
                        "account-link-github-owner",
                        if waiting {
                            "Waiting for GitHub…"
                        } else if linked {
                            "Verify GitHub account"
                        } else {
                            "Sign in with GitHub"
                        },
                    )
                    .tone(ButtonTone::Neutral)
                    .start_icon(IconName::Github)
                    .disabled(waiting || !available)
                    .debug_selector("account-link-github-owner")
                    .on_click(cx.listener(|shell, _, _, cx| shell.start_github_owner_setup(cx))),
                )
                .when(!available && !self.account_setup.methods_pending, |view| {
                    view.child(
                        div()
                            .text_xs()
                            .text_color(colors.muted_text)
                            .whitespace_normal()
                            .child("GitHub sign-in is unavailable in this build."),
                    )
                })
                .children(self.account_setup.device_code.as_ref().map(|code| {
                    let copy = code.clone();
                    let url = self.account_setup.device_url.clone();
                    div()
                        .debug_selector(|| "github-owner-device-code".into())
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_sm()
                                .child(format!("Enter code {code} on GitHub")),
                        )
                        .child(
                            div()
                                .flex()
                                .gap_1()
                                .child(
                                    Button::new("copy-github-device-code", "Copy code")
                                        .tone(ButtonTone::Ghost)
                                        .on_click(move |_, _, cx| {
                                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                                copy.clone(),
                                            ))
                                        }),
                                )
                                .child(
                                    Button::new("open-github-device-page", "Open GitHub")
                                        .tone(ButtonTone::Ghost)
                                        .on_click(move |_, _, cx| {
                                            if let Some(url) = &url {
                                                cx.open_url(url);
                                            }
                                        }),
                                ),
                        )
                }))
                .when(waiting, |view| {
                    view.child(
                        Button::new("cancel-github-owner", "Cancel")
                            .tone(ButtonTone::Ghost)
                            .on_click(cx.listener(|shell, _, _, cx| {
                                shell.reset_account_setup(cx);
                                shell.load_account_methods(cx);
                            })),
                    )
                })
            })
            .when(self.lifecycle.identity.is_some(), |view| {
                view.child(
                    Button::new("account-workspace-invitations", "Workspace invitations…")
                        .tone(ButtonTone::Ghost)
                        .debug_selector("account-workspace-invitations")
                        .on_click(
                            cx.listener(|shell, _, _, cx| shell.toggle_account_invitations(cx)),
                        ),
                )
            })
            .when(self.account_setup.invitations_open, |view| {
                view.child(self.render_account_invitations(cx))
            })
            .children(
                self.account_setup
                    .error
                    .as_ref()
                    .map(|error| ErrorBanner::new(error.clone())),
            )
            .into_any_element()
    }

    fn render_account_invitations(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors;
        let pending = self.account_setup.pending;
        let memberships = self
            .lifecycle
            .identity
            .as_ref()
            .map(|i| i.memberships.clone())
            .unwrap_or_default();
        let roles = [
            (sift_protocol::InvitationRole::Viewer, "Viewer"),
            (sift_protocol::InvitationRole::Member, "Member"),
            (sift_protocol::InvitationRole::Admin, "Admin"),
        ];
        div().flex().flex_col().gap_2().debug_selector(|| "account-invitations".into())
            .child(div().text_xs().text_color(colors.muted_text).whitespace_normal()
                .child("Sign in on this server first. An invitation adds workspace membership; room access is granted separately."))
            .child(self.account_setup.accept_input.clone())
            .child(Button::new("accept-workspace-invitation", "Join workspace").tone(ButtonTone::Neutral).disabled(pending)
                .on_click(cx.listener(|shell, _, _, cx| shell.accept_account_invitation(cx))))
            .when(self.account_setup.tenant_id.is_some(), |view| view
                .child(div().flex().flex_wrap().gap_1().children(memberships.into_iter()
                    .filter(|m| matches!(m.role.as_str(), "owner" | "admin")).map(|m| {
                        let id = m.tenant_id;
                        Button::new(("invitation-tenant", id as usize), m.tenant_name)
                            .tone(if self.account_setup.tenant_id == Some(id) { ButtonTone::Neutral } else { ButtonTone::Ghost })
                            .disabled(pending).on_click(cx.listener(move |shell, _, _, cx| shell.select_invitation_tenant(id, cx)))
                    })))
                .child(div().flex().gap_1().children(roles.into_iter().enumerate().map(|(index, (role, label))| {
                    Button::new(("invitation-role", index), label)
                        .tone(if std::mem::discriminant(&self.account_setup.role) == std::mem::discriminant(&role) { ButtonTone::Neutral } else { ButtonTone::Ghost })
                        .disabled(pending).on_click(cx.listener(move |shell, _, _, cx| { shell.account_setup.role = role; cx.notify(); }))
                })))
                .child(self.account_setup.target_input.clone())
                .child(Button::new("issue-workspace-invitation", "Create invitation · 7 days").tone(ButtonTone::Neutral).disabled(pending)
                    .on_click(cx.listener(|shell, _, _, cx| shell.issue_account_invitation(cx)))))
            .children(self.account_setup.issued.as_ref().map(|issued| {
                let token = issued.token.clone();
                div().flex().flex_col().gap_1()
                    .child(div().text_xs().text_color(colors.muted_text).child("One-use invitation ready. Copy it now; it is not saved."))
                    .child(Button::new("copy-workspace-invitation", "Copy invitation token").tone(ButtonTone::Neutral)
                        .on_click(move |_, _, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(token.clone()))))
            }))
            .children(self.active_account_invitations().into_iter().enumerate().map(|(index, invitation)| {
                let tenant_id = invitation.tenant_id.0;
                let invitation_id = invitation.id.0;
                div().flex().items_center().gap_1()
                    .when(index == self.account_setup.invitation_cursor, |row| row.bg(colors.active_surface))
                    .child(div().flex_1().min_w_0().text_xs().whitespace_normal().child(format!("#{invitation_id} · {:?} · expires {}", invitation.intended_role, invitation.expires_at.format("%d %b"))))
                    .child(Button::new(("revoke-workspace-invitation", invitation_id as usize), "Revoke").tone(ButtonTone::Ghost).disabled(pending)
                        .on_click(cx.listener(move |shell, _, _, cx| shell.send_account_action(AccountAction::RevokeInvitation { tenant_id, invitation_id }, cx))))
            })).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};

    fn identity() -> sift_protocol::WhoAmIResponse {
        sift_protocol::WhoAmIResponse {
            principal: sift_protocol::AuthPrincipal {
                id: 1,
                display_name: "Owner".into(),
                email: None,
                avatar_url: None,
                is_instance_admin: true,
            },
            memberships: vec![sift_protocol::AuthTenantMembership {
                tenant_id: 7,
                tenant_name: "Team".into(),
                role: "owner".into(),
            }],
            github_login: None,
            auth_session_id: None,
        }
    }

    #[gpui::test]
    fn account_setup_dismissal_cancels_owner_flow_and_drops_late_codes(cx: &mut TestAppContext) {
        let window = super::super::tests::shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        let (sender, mut commands) = tokio::sync::mpsc::unbounded_channel();
        let (_events, events) = tokio::sync::mpsc::unbounded_channel();
        workspace.update(&mut cx, |shell, cx| {
            shell.attach_instance_manager(sender, events, vec![], cx);
            shell
                .lifecycle
                .apply(LifecycleEvent::Selected(crate::InstanceSpec {
                    id: "local".into(),
                    name: "Local".into(),
                    base_url: String::new(),
                    kind: crate::InstanceKind::Local,
                }));
            shell
                .lifecycle
                .apply(LifecycleEvent::Authenticated(identity()));
            shell.open_app_bar_modal(Modal::Account, cx);
            shell.start_github_owner_setup(cx);
        });
        let InstanceCommand::LinkGithubOwner { scope } = commands.try_recv().unwrap() else {
            panic!("expected owner setup");
        };
        workspace.update(&mut cx, |shell, cx| {
            shell.on_github_owner_setup(
                scope.clone(),
                GithubOwnerSetupEvent::Authorization {
                    url: "https://github.com/login/device".into(),
                    code: sift_protocol::RedactedString("ABCD-EFGH".into()),
                },
                cx,
            )
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("github-owner-device-code").is_some());
        workspace.update(&mut cx, |shell, cx| {
            shell.close_app_bar_modal(cx);
            shell.open_app_bar_modal(Modal::Account, cx);
            shell.on_github_owner_setup(
                scope,
                GithubOwnerSetupEvent::Authorization {
                    url: "https://github.com/login/device".into(),
                    code: sift_protocol::RedactedString("LATE-CODE".into()),
                },
                cx,
            );
            assert!(shell.account_setup.device_code.is_none());
            assert!(!shell.account_setup.owner_pending);
        });
        assert!(matches!(
            commands.try_recv().unwrap(),
            InstanceCommand::CancelGithubOwnerSetup
        ));
    }

    #[gpui::test]
    fn account_invitations_keep_selected_role_target_and_tokens_transient(cx: &mut TestAppContext) {
        let window = super::super::tests::shell(cx);
        let workspace = window.root(cx).unwrap();
        let (sender, mut commands) = ExecutorSender::channel(32);
        workspace.update(cx, |shell, cx| {
            shell.executor_sender = Some(sender);
            shell
                .lifecycle
                .apply(LifecycleEvent::Authenticated(identity()));
            shell.modal = Some(Modal::Account);
            shell.account_setup.invitations_open = true;
            shell.account_setup.tenant_id = Some(7);
            shell.account_setup.role = sift_protocol::InvitationRole::Viewer;
            shell
                .account_setup
                .target_input
                .update(cx, |input, cx| input.set_text("22", cx));
            shell.issue_account_invitation(cx);
        });
        let ExecutorCommand::Account {
            scope,
            action: AccountAction::CreateInvitation { tenant_id, request },
        } = commands.try_recv().unwrap()
        else {
            panic!("expected invitation");
        };
        assert_eq!(tenant_id, 7);
        assert_eq!(request.target_principal_id, Some(22));
        assert!(matches!(
            request.role,
            sift_protocol::InvitationRole::Viewer
        ));
        assert!(request.expires_at > chrono::Utc::now() + chrono::Duration::days(6));
        workspace.update(cx, |shell, cx| {
            shell.on_account_reply(
                scope.clone(),
                Ok(AccountReply::Issued {
                    tenant_id: 7,
                    invitation: sift_protocol::IssuedTenantInvitationResponse {
                        invitation_id: 11,
                        token: "fixture-invitation-secret".into(),
                        expires_at: request.expires_at,
                    },
                }),
                cx,
            );
            assert!(shell.account_setup.issued.is_some());
            shell.close_app_bar_modal(cx);
            assert!(shell.account_setup.issued.is_none());
            assert!(shell.account_setup.accept_input.read(cx).text().is_empty());
            shell.modal = Some(Modal::Account);
            shell.on_account_reply(
                scope,
                Ok(AccountReply::Issued {
                    tenant_id: 7,
                    invitation: sift_protocol::IssuedTenantInvitationResponse {
                        invitation_id: 12,
                        token: "late-invitation-secret".into(),
                        expires_at: request.expires_at,
                    },
                }),
                cx,
            );
            assert!(shell.account_setup.issued.is_none());
        });
    }
}
