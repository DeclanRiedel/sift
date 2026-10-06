//! Operator source setup: explicit review, masked credentials, independent room grants.
use super::*;
use sift_protocol::{AiExternalSource, AiExternalSourceState, AiExternalToolPolicy, AiMcpRevision};

#[derive(Clone)]
pub enum AiExternalSourceAction {
    Load,
    Discover(sift_protocol::DiscoverAiExternalSourceRequest),
    Refresh {
        id: uuid::Uuid,
        request: sift_protocol::RefreshAiExternalSourceRequest,
    },
    Activate {
        id: uuid::Uuid,
        request: sift_protocol::ActivateAiExternalSourceRequest,
    },
    Disable {
        id: uuid::Uuid,
        revision: u64,
    },
    Delete {
        id: uuid::Uuid,
        revision: u64,
    },
    Publish {
        room: i64,
        request: sift_protocol::PublishAiExternalSourceRequest,
    },
    Revoke {
        room: i64,
        grant: uuid::Uuid,
        source_id: uuid::Uuid,
    },
}
#[derive(Debug, Clone)]
pub struct AiSourceManagerSnapshot {
    pub sources: Vec<AiExternalSource>,
    pub vaults: Vec<sift_api_types::Vault>,
    pub grants: Vec<sift_protocol::AiExternalRoomGrantHeader>,
    pub can_register: bool,
    pub can_publish: bool,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum CredentialChoice {
    Keep,
    Replace,
    Clear,
}

pub(super) struct AiSourceManager {
    pub open: bool,
    pub pending: bool,
    pub snapshot: Option<AiSourceManagerSnapshot>,
    pub selected: Option<uuid::Uuid>,
    pub room: Option<i64>,
    pub focus: FocusHandle,
    label: Entity<TextInput>,
    endpoint: Entity<TextInput>,
    token: Entity<TextInput>,
    protocol: AiMcpRevision,
    vault: Option<i64>,
    credential: CredentialChoice,
    approvals: Vec<AiExternalToolPolicy>,
    cursor: usize,
    review_ack: bool,
    room_ack: bool,
    room_aliases: HashSet<String>,
    schema_open: bool,
}
impl AiSourceManager {
    pub(super) fn new(cx: &mut Context<WorkspaceShell>) -> Self {
        let label =
            cx.new(|cx| TextInput::new("", "Source label", cx).aria_label("AI source label"));
        let endpoint = cx.new(|cx| {
            TextInput::new("", "Actual Streamable HTTP endpoint", cx)
                .aria_label("AI source endpoint")
        });
        let token = cx.new(|cx| {
            TextInput::new("", "Bearer credential (optional)", cx)
                .aria_label("AI source bearer credential")
                .masked()
        });
        let manager = Self {
            open: false,
            pending: false,
            snapshot: None,
            selected: None,
            room: None,
            focus: cx.focus_handle(),
            label,
            endpoint,
            token,
            protocol: AiMcpRevision::Modern20260728,
            vault: None,
            credential: CredentialChoice::Clear,
            approvals: Vec::new(),
            cursor: 0,
            review_ack: false,
            room_ack: false,
            room_aliases: HashSet::new(),
            schema_open: false,
        };
        manager.wire_tabs(cx);
        manager
    }
    fn wire_tabs(&self, cx: &mut Context<WorkspaceShell>) {
        let mut inputs = vec![self.label.clone(), self.endpoint.clone()];
        if self.selected.is_none() || self.credential == CredentialChoice::Replace {
            inputs.push(self.token.clone());
        }
        let handles = inputs
            .iter()
            .map(|input| input.focus_handle(cx))
            .collect::<Vec<_>>();
        for (index, input) in inputs.iter().enumerate() {
            let previous = if index == 0 {
                self.focus.clone()
            } else {
                handles[index - 1].clone()
            };
            let next = handles
                .get(index + 1)
                .cloned()
                .unwrap_or_else(|| self.focus.clone());
            input.update(cx, |input, cx| {
                input.set_tab_targets(Some(previous), Some(next), cx)
            });
        }
    }
    fn source(&self) -> Option<&AiExternalSource> {
        self.selected.and_then(|id| {
            self.snapshot
                .as_ref()?
                .sources
                .iter()
                .find(|source| source.id == id)
        })
    }
}
fn policy_label(policy: AiExternalToolPolicy) -> &'static str {
    match policy {
        AiExternalToolPolicy::Unavailable => "Unavailable",
        AiExternalToolPolicy::Read => "Read",
        AiExternalToolPolicy::LocalQueryDraft => "SQL draft",
        AiExternalToolPolicy::LocalRowDraft => "Row draft",
        AiExternalToolPolicy::LocalMigrationDraft => "Schema draft",
    }
}
fn next_policy(policy: AiExternalToolPolicy) -> AiExternalToolPolicy {
    match policy {
        AiExternalToolPolicy::Unavailable => AiExternalToolPolicy::Read,
        AiExternalToolPolicy::Read => AiExternalToolPolicy::LocalQueryDraft,
        AiExternalToolPolicy::LocalQueryDraft => AiExternalToolPolicy::LocalRowDraft,
        AiExternalToolPolicy::LocalRowDraft => AiExternalToolPolicy::LocalMigrationDraft,
        AiExternalToolPolicy::LocalMigrationDraft => AiExternalToolPolicy::Unavailable,
    }
}

impl WorkspaceShell {
    fn select_managed_ai_source(&mut self, id: Option<uuid::Uuid>, cx: &mut Context<Self>) {
        if self.ai.source_manager.pending {
            return;
        }
        let source = id
            .and_then(|id| {
                self.ai
                    .source_manager
                    .snapshot
                    .as_ref()?
                    .sources
                    .iter()
                    .find(|source| source.id == id)
            })
            .cloned();
        let manager = &mut self.ai.source_manager;
        manager.selected = source.as_ref().map(|source| source.id);
        manager.label.update(cx, |input, cx| {
            input.set_text(
                source
                    .as_ref()
                    .map(|source| source.definition.label.clone())
                    .unwrap_or_default(),
                cx,
            )
        });
        manager.endpoint.update(cx, |input, cx| {
            input.set_text(
                source
                    .as_ref()
                    .map(|source| source.definition.endpoint.clone())
                    .unwrap_or_default(),
                cx,
            )
        });
        manager.token.update(cx, |input, cx| input.set_text("", cx));
        manager.protocol = source
            .as_ref()
            .map(|source| source.definition.protocol)
            .unwrap_or(AiMcpRevision::Modern20260728);
        manager.vault = source.as_ref().and_then(|source| source.vault_id);
        manager.credential = if source.is_some() {
            CredentialChoice::Keep
        } else {
            CredentialChoice::Clear
        };
        manager.approvals = source
            .as_ref()
            .map(|source| {
                source
                    .definition
                    .tools
                    .iter()
                    .map(|tool| {
                        if source.state == AiExternalSourceState::Draft {
                            AiExternalToolPolicy::Unavailable
                        } else {
                            tool.policy
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        manager.cursor = 0;
        manager.review_ack = false;
        manager.room_ack = false;
        manager.room_aliases.clear();
        manager.schema_open = false;
        manager.wire_tabs(cx);
        cx.notify();
    }
    fn manage_ai_source(&mut self, action: AiExternalSourceAction, cx: &mut Context<Self>) {
        if self.ai.source_manager.pending || self.ai.pending {
            return;
        }
        let Some(tenant_id) = self.selected_tenant_id() else {
            return;
        };
        let Some(sender) = &self.executor_sender else {
            return;
        };
        if sender
            .send(ExecutorCommand::ManageAiExternalSources {
                instance_id: self
                    .selected_instance_id
                    .clone()
                    .unwrap_or_else(|| "local".into()),
                tenant_id,
                room_id: self.ai.source_manager.room,
                action,
            })
            .is_ok()
        {
            self.ai.source_manager.pending = true;
            self.ai
                .source_manager
                .token
                .update(cx, |input, cx| input.set_text("", cx));
            cx.notify();
        }
    }
    fn open_ai_source_manager(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_ai_view_scope(cx);
        if self.ai.pending {
            return;
        }
        self.ai.source_manager.open = !self.ai.source_manager.open;
        if self.ai.source_manager.open {
            self.ai.source_manager.room = self
                .ai_context_snapshot(cx)
                .ok()
                .and_then(|context| context.target.room_id);
            self.ai.source_manager.focus.focus(window, cx);
            self.manage_ai_source(AiExternalSourceAction::Load, cx);
        } else {
            self.ai
                .source_manager
                .token
                .update(cx, |input, cx| input.set_text("", cx));
        }
        cx.notify();
    }
    fn discover_managed_ai_source(&mut self, cx: &mut Context<Self>) {
        let Some(tenant) = self.selected_tenant_id() else {
            return;
        };
        let manager = &self.ai.source_manager;
        let label = manager.label.read(cx).text().trim().to_owned();
        let endpoint = manager.endpoint.read(cx).text().trim().to_owned();
        if label.is_empty() || endpoint.is_empty() {
            self.ai.error = Some("Enter a source label and its actual endpoint".into());
            cx.notify();
            return;
        }
        let token = manager.token.read(cx).text().to_owned();
        let action = if let Some(source) = manager.source() {
            let credentials = match manager.credential {
                CredentialChoice::Keep => sift_protocol::AiExternalCredentialUpdate::Keep,
                CredentialChoice::Replace => sift_protocol::AiExternalCredentialUpdate::Replace {
                    bearer_token: token,
                },
                CredentialChoice::Clear => sift_protocol::AiExternalCredentialUpdate::Clear,
            };
            AiExternalSourceAction::Refresh {
                id: source.id,
                request: sift_protocol::RefreshAiExternalSourceRequest {
                    expected_revision: source.revision,
                    label,
                    endpoint,
                    protocol: manager.protocol,
                    credentials,
                },
            }
        } else {
            AiExternalSourceAction::Discover(sift_protocol::DiscoverAiExternalSourceRequest {
                tenant_id: tenant,
                vault_id: manager.vault,
                label,
                endpoint,
                protocol: manager.protocol,
                bearer_token: (!token.is_empty()).then_some(token),
            })
        };
        self.manage_ai_source(action, cx);
    }
    fn activate_managed_ai_source(&mut self, cx: &mut Context<Self>) {
        let manager = &self.ai.source_manager;
        let Some(source) = manager.source() else {
            return;
        };
        let tools = source
            .definition
            .tools
            .iter()
            .zip(&manager.approvals)
            .filter(|(_, policy)| **policy != AiExternalToolPolicy::Unavailable)
            .map(|(tool, policy)| sift_protocol::AiExternalToolApproval {
                alias: tool.alias.clone(),
                expected_schema_sha256: tool.schema_sha256.clone(),
                policy: *policy,
            })
            .collect::<Vec<_>>();
        if !manager.review_ack || tools.is_empty() {
            self.ai.error =
                Some("Review the credential scope and choose at least one tool permission".into());
            cx.notify();
            return;
        }
        let action = AiExternalSourceAction::Activate {
            id: source.id,
            request: sift_protocol::ActivateAiExternalSourceRequest {
                expected_revision: source.revision,
                expected_config_sha256: source.config_sha256.clone(),
                credential_scope_reviewed: true,
                tools,
            },
        };
        self.manage_ai_source(action, cx);
    }
    fn publish_managed_ai_source(&mut self, cx: &mut Context<Self>) {
        let manager = &self.ai.source_manager;
        let (Some(source), Some(room)) = (manager.source(), manager.room) else {
            return;
        };
        if !manager.room_ack || manager.room_aliases.is_empty() {
            self.ai.error =
                Some("Choose room tools and acknowledge future room requests and results".into());
            cx.notify();
            return;
        }
        let proof = sift_protocol::AiExternalSourceProof {
            source_id: source.id,
            source_revision: source.revision,
            config_sha256: source.config_sha256.clone(),
            credential_identity: source.credential_identity,
            label: source.definition.label.clone(),
            room_grant_id: None,
        };
        let previous = manager
            .snapshot
            .as_ref()
            .and_then(|snapshot| {
                snapshot
                    .grants
                    .iter()
                    .find(|grant| grant.source_id == source.id)
            })
            .map(|grant| grant.id);
        let mut aliases = manager.room_aliases.iter().cloned().collect::<Vec<_>>();
        aliases.sort();
        self.manage_ai_source(
            AiExternalSourceAction::Publish {
                room,
                request: sift_protocol::PublishAiExternalSourceRequest {
                    expected_grant_id: previous,
                    source: proof,
                    tool_aliases: aliases,
                    publish_future_results_to_room: true,
                },
            },
            cx,
        );
    }
    pub(super) fn accept_ai_source_management(
        &mut self,
        result: Result<AiSourceManagerSnapshot, String>,
        changed: Option<uuid::Uuid>,
        cx: &mut Context<Self>,
    ) {
        self.ai.source_manager.pending = false;
        match result {
            Ok(snapshot) => {
                self.ai.source_manager.snapshot = Some(snapshot);
                if let Some(id) = changed {
                    self.select_managed_ai_source(Some(id), cx);
                } else if self.ai.source_manager.selected.is_some()
                    && self.ai.source_manager.source().is_none()
                {
                    self.select_managed_ai_source(None, cx);
                }
                // Existing selections retain their old pins until explicit re-review.
                if let Some(id) = changed {
                    self.ai
                        .sources
                        .choices
                        .retain(|choice| choice.proof.source_id != id);
                }
            }
            Err(error) => self.ai.error = Some(error),
        }
        cx.notify();
    }
    pub(super) fn render_ai_source_manager(&self, cx: &mut Context<Self>) -> AnyElement {
        let manager = &self.ai.source_manager;
        let mut view = div().flex().flex_col().gap_1().text_xs().child(
            Button::new("ai-source-manager", "Manage sources")
                .tone(ButtonTone::Ghost)
                .on_click(
                    cx.listener(|shell, _, window, cx| shell.open_ai_source_manager(window, cx)),
                ),
        );
        if !manager.open {
            return view.into_any_element();
        }
        view = view.track_focus(&manager.focus).on_key_down(cx.listener(
            |shell, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.modifiers.modified() {
                    return;
                }
                if !shell.ai.source_manager.focus.is_focused(window) {
                    if event.keystroke.key == "escape" {
                        shell.ai.source_manager.focus.focus(window, cx);
                        cx.stop_propagation();
                        cx.notify();
                    }
                    return;
                }
                let manager = &mut shell.ai.source_manager;
                match event.keystroke.key.as_str() {
                    "i" | "tab" => manager.label.focus_handle(cx).focus(window, cx),
                    "j" => {
                        if !manager.approvals.is_empty() {
                            manager.cursor = (manager.cursor + 1) % manager.approvals.len();
                        }
                    }
                    "k" => {
                        if !manager.approvals.is_empty() {
                            manager.cursor = (manager.cursor + manager.approvals.len() - 1)
                                % manager.approvals.len();
                        }
                    }
                    "l" | "space"
                        if !manager.pending
                            && manager.source().is_some_and(|source| {
                                source.state == AiExternalSourceState::Draft
                            }) =>
                    {
                        if let Some(policy) = manager.approvals.get_mut(manager.cursor) {
                            *policy = next_policy(*policy);
                        }
                    }
                    "escape" => {
                        manager.open = false;
                        manager.token.update(cx, |input, cx| input.set_text("", cx));
                        shell.ai.input.focus_handle(cx).focus(window, cx);
                    }
                    _ => return,
                }
                cx.stop_propagation();
                cx.notify();
            },
        ));
        view = view
            .child("Operator review · j/k tool · l cycles permission · credentials stay masked");
        if manager.pending {
            return view
                .child("Working… refresh sources after a lost response before retrying")
                .into_any_element();
        }
        view = view.child(
            Button::new("ai-source-manager-reload", "Refresh list")
                .tone(ButtonTone::Ghost)
                .on_click(cx.listener(|shell, _, _, cx| {
                    shell.manage_ai_source(AiExternalSourceAction::Load, cx)
                })),
        );
        let Some(snapshot) = &manager.snapshot else {
            return view.into_any_element();
        };
        if !snapshot.can_register {
            return view
                .child("Source registration and approval require a tenant administrator")
                .into_any_element();
        }
        view = view.child(
            Button::new("ai-source-new", "New source")
                .tone(ButtonTone::Ghost)
                .on_click(cx.listener(|shell, _, _, cx| shell.select_managed_ai_source(None, cx))),
        );
        for source in &snapshot.sources {
            let id = source.id;
            view =
                view.child(
                    Button::new(
                        format!("ai-source-manage-{id}"),
                        format!("{} · {:?}", source.definition.label, source.state),
                    )
                    .tone(if manager.selected == Some(id) {
                        ButtonTone::Accent
                    } else {
                        ButtonTone::Ghost
                    })
                    .on_click(cx.listener(move |shell, _, _, cx| {
                        shell.select_managed_ai_source(Some(id), cx)
                    })),
                );
        }
        view = view
            .child(manager.label.clone())
            .child(manager.endpoint.clone());
        for (id, protocol, label) in [
            ("modern", AiMcpRevision::Modern20260728, "2026-07-28"),
            ("legacy", AiMcpRevision::Legacy20251125, "Legacy 2025-11-25"),
        ] {
            view = view.child(
                Button::new(format!("ai-source-protocol-{id}"), label)
                    .tone(if manager.protocol == protocol {
                        ButtonTone::Accent
                    } else {
                        ButtonTone::Ghost
                    })
                    .on_click(cx.listener(move |shell, _, _, cx| {
                        shell.ai.source_manager.protocol = protocol;
                        cx.notify();
                    })),
            );
        }
        if manager.selected.is_none() {
            view = view.child(
                Button::new("ai-source-private-scope", "Private")
                    .tone(if manager.vault.is_none() {
                        ButtonTone::Accent
                    } else {
                        ButtonTone::Ghost
                    })
                    .on_click(cx.listener(|shell, _, _, cx| {
                        shell.ai.source_manager.vault = None;
                        cx.notify();
                    })),
            );
            for vault in &snapshot.vaults {
                let id = vault.id.0;
                view = view.child(
                    Button::new(
                        format!("ai-source-vault-{id}"),
                        format!("Share with users who may use {}", vault.name),
                    )
                    .tone(if manager.vault == Some(id) {
                        ButtonTone::Accent
                    } else {
                        ButtonTone::Ghost
                    })
                    .on_click(cx.listener(move |shell, _, _, cx| {
                        shell.ai.source_manager.vault = Some(id);
                        cx.notify();
                    })),
                );
            }
        } else {
            for (choice, label) in [
                (
                    CredentialChoice::Keep,
                    "Keep credential at the same endpoint",
                ),
                (CredentialChoice::Replace, "Replace credential"),
                (CredentialChoice::Clear, "Clear credential · anonymous"),
            ] {
                view = view.child(
                    Button::new(format!("ai-source-credential-{label}"), label)
                        .tone(if manager.credential == choice {
                            ButtonTone::Accent
                        } else {
                            ButtonTone::Ghost
                        })
                        .on_click(cx.listener(move |shell, _, _, cx| {
                            shell.ai.source_manager.credential = choice;
                            shell.ai.source_manager.wire_tabs(cx);
                            shell
                                .ai
                                .source_manager
                                .token
                                .update(cx, |input, cx| input.set_text("", cx));
                            cx.notify();
                        })),
                );
            }
        }
        if manager.selected.is_none() || manager.credential == CredentialChoice::Replace {
            view = view.child(manager.token.clone());
        }
        view = view.child(
            Button::new(
                "ai-source-discover",
                if manager.selected.is_none() {
                    "Discover for review"
                } else {
                    "Rediscover · reset approvals"
                },
            )
            .tone(ButtonTone::Accent)
            .on_click(cx.listener(|shell, _, _, cx| shell.discover_managed_ai_source(cx))),
        );
        let Some(source) = manager.source() else {
            return view.into_any_element();
        };
        let id = source.id;
        let revision = source.revision;
        view = view.child(format!(
            "Credential: {} · source revision {revision}",
            if source.credential_configured {
                "configured"
            } else {
                "anonymous"
            }
        ));
        view = view.child(
            Button::new("ai-source-disable", "Disable future source use")
                .tone(ButtonTone::Ghost)
                .on_click(cx.listener(move |shell, _, _, cx| {
                    shell.manage_ai_source(AiExternalSourceAction::Disable { id, revision }, cx)
                })),
        );
        view = view.child(
            Button::new("ai-source-delete", "Delete registration")
                .tone(ButtonTone::Ghost)
                .on_click(cx.listener(move |shell, _, _, cx| {
                    shell.manage_ai_source(AiExternalSourceAction::Delete { id, revision }, cx)
                })),
        );
        for (index, tool) in source.definition.tools.iter().enumerate() {
            let policy = manager
                .approvals
                .get(index)
                .copied()
                .unwrap_or(AiExternalToolPolicy::Unavailable);
            view = view.child(
                Button::new(
                    format!("ai-source-policy-{index}"),
                    format!(
                        "{} · {}",
                        tool.title.as_deref().unwrap_or(&tool.name),
                        policy_label(policy)
                    ),
                )
                .tone(if index == manager.cursor {
                    ButtonTone::Accent
                } else {
                    ButtonTone::Ghost
                })
                .on_click(cx.listener(move |shell, _, _, cx| {
                    let manager = &mut shell.ai.source_manager;
                    manager.cursor = index;
                    if !manager.pending
                        && manager
                            .source()
                            .is_some_and(|source| source.state == AiExternalSourceState::Draft)
                    {
                        if let Some(policy) = manager.approvals.get_mut(index) {
                            *policy = next_policy(*policy);
                        }
                    }
                    cx.notify();
                })),
            );
            if index == manager.cursor {
                view = view.child(tool.description.clone());
            }
        }
        view = view.child(
            Button::new("ai-source-schemas", "Show selected tool schemas")
                .tone(ButtonTone::Ghost)
                .on_click(cx.listener(|shell, _, _, cx| {
                    shell.ai.source_manager.schema_open = !shell.ai.source_manager.schema_open;
                    cx.notify();
                })),
        );
        if manager.schema_open {
            if let Some(tool) = source.definition.tools.get(manager.cursor) {
                view = view
                    .child(serde_json::to_string_pretty(&tool.input_schema).unwrap_or_default());
                if let Some(output) = &tool.output_schema {
                    view = view.child(serde_json::to_string_pretty(output).unwrap_or_default());
                }
            }
        }
        if source.state == AiExternalSourceState::Draft {
            view=view.child("Remote hints do not approve tools. Draft adapters only stage Sift changes for human review.");
            view = view.child(
                Button::new(
                    "ai-source-review-ack",
                    if manager.review_ack {
                        "✓ Credential scope and selected tool permissions reviewed"
                    } else {
                        "Review credential scope and selected tool permissions"
                    },
                )
                .tone(ButtonTone::Ghost)
                .on_click(cx.listener(|shell, _, _, cx| {
                    shell.ai.source_manager.review_ack = !shell.ai.source_manager.review_ack;
                    cx.notify();
                })),
            );
            view = view.child(
                Button::new("ai-source-activate", "Activate reviewed tools")
                    .tone(ButtonTone::Accent)
                    .on_click(cx.listener(|shell, _, _, cx| shell.activate_managed_ai_source(cx))),
            );
        }
        if let Some(room) = manager.room {
            if let Some(grant) = snapshot.grants.iter().find(|grant| grant.source_id == id) {
                let grant = grant.id;
                view = view.child(
                    Button::new("ai-source-grant-revoke", "Revoke room source grant")
                        .tone(ButtonTone::Ghost)
                        .on_click(cx.listener(move |shell, _, _, cx| {
                            shell.manage_ai_source(
                                AiExternalSourceAction::Revoke {
                                    room,
                                    grant,
                                    source_id: id,
                                },
                                cx,
                            )
                        })),
                );
            }
            if snapshot.can_publish && source.state == AiExternalSourceState::Active {
                view = view.child(
                    "Choose the tools this room may use independently of database publication",
                );
                for tool in source
                    .definition
                    .tools
                    .iter()
                    .filter(|tool| tool.policy != AiExternalToolPolicy::Unavailable)
                {
                    let alias = tool.alias.clone();
                    view = view.child(
                        Button::new(
                            format!("ai-source-room-alias-{alias}"),
                            format!(
                                "{} {}",
                                if manager.room_aliases.contains(&alias) {
                                    "✓"
                                } else {
                                    "○"
                                },
                                tool.title.as_deref().unwrap_or(&tool.name)
                            ),
                        )
                        .tone(ButtonTone::Ghost)
                        .on_click(cx.listener(move |shell, _, _, cx| {
                            if !shell.ai.source_manager.room_aliases.remove(&alias) {
                                shell.ai.source_manager.room_aliases.insert(alias.clone());
                            }
                            cx.notify();
                        })),
                    );
                }
                view=view.child(Button::new("ai-source-room-ack",if manager.room_ack {"✓ Future source tool lists, request arguments and results may be shared with room members"}else{"Acknowledge future source tool lists, request arguments and results may be shared with room members"}).tone(ButtonTone::Ghost).on_click(cx.listener(|shell,_,_,cx|{shell.ai.source_manager.room_ack = !shell.ai.source_manager.room_ack;cx.notify();})));
                view = view.child(
                    Button::new(
                        "ai-source-room-publish",
                        "Publish reviewed room source grant",
                    )
                    .tone(ButtonTone::Accent)
                    .on_click(cx.listener(|shell, _, _, cx| shell.publish_managed_ai_source(cx))),
                );
            }
        }
        view.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};

    #[gpui::test]
    fn source_review_resets_disclosures_and_never_restores_credentials(cx: &mut TestAppContext) {
        let window = crate::shell::tests::shell(cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let workspace = window.root(&mut cx).unwrap();
        workspace.update(&mut cx, |shell, cx| {
            let source = AiExternalSource {
                id: uuid::Uuid::new_v4(),
                tenant_id: 1,
                owner_principal_id: 1,
                vault_id: None,
                revision: 2,
                state: AiExternalSourceState::Draft,
                definition: sift_protocol::AiExternalSourceDefinition {
                    label: "Reviewed source".into(),
                    endpoint: "https://fixture.invalid/mcp".into(),
                    protocol: AiMcpRevision::Modern20260728,
                    tools: vec![sift_protocol::AiExternalToolDefinition {
                        name: "search".into(),
                        alias: "search".into(),
                        title: None,
                        description: String::new(),
                        input_schema: serde_json::json!({"type":"object"}),
                        output_schema: None,
                        annotations: None,
                        schema_sha256: "a".repeat(64),
                        policy: AiExternalToolPolicy::Read,
                    }],
                },
                config_sha256: "b".repeat(64),
                credential_identity: uuid::Uuid::new_v4(),
                credential_configured: true,
                credential_scope_reviewed: false,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            };
            shell.ai.source_manager.snapshot = Some(AiSourceManagerSnapshot {
                sources: vec![source.clone()],
                vaults: vec![],
                grants: vec![],
                can_register: true,
                can_publish: true,
            });
            shell
                .ai
                .source_manager
                .token
                .update(cx, |input, cx| input.set_text("typed-fixture-token", cx));
            shell.ai.source_manager.review_ack = true;
            shell.ai.source_manager.room_ack = true;
            shell.ai.source_manager.room_aliases.insert("search".into());
            shell.select_managed_ai_source(Some(source.id), cx);
            assert!(shell.ai.source_manager.token.read(cx).text().is_empty());
            assert_eq!(
                shell.ai.source_manager.approvals,
                vec![AiExternalToolPolicy::Unavailable]
            );
            assert!(!shell.ai.source_manager.review_ack);
            assert!(!shell.ai.source_manager.room_ack);
            assert!(shell.ai.source_manager.room_aliases.is_empty());
            assert!(shell.ai.source_manager.credential == CredentialChoice::Keep);
            shell.accept_ai_source_management(
                Ok(AiSourceManagerSnapshot {
                    sources: vec![],
                    vaults: vec![],
                    grants: vec![],
                    can_register: false,
                    can_publish: false,
                }),
                None,
                cx,
            );
            assert!(shell.ai.source_manager.selected.is_none());
            assert!(shell.ai.source_manager.endpoint.read(cx).text().is_empty());
            shell
                .ai
                .source_manager
                .token
                .update(cx, |input, cx| input.set_text("another-fixture-token", cx));
            shell.ai.source_manager.room_ack = true;
            shell.roll_ai_view_scope(cx);
            assert!(shell.ai.source_manager.token.read(cx).text().is_empty());
            assert!(shell.ai.source_manager.snapshot.is_none());
            assert!(!shell.ai.source_manager.room_ack);
        });
    }
}
