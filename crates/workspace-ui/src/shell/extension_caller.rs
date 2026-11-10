//! Capture account/server assertions before entering the desktop command queue.
use super::*;

impl WorkspaceShell {
    pub(super) fn extension_caller_scope(&self) -> Option<ExtensionCallerScope> {
        let instance = self.lifecycle.selected_instance.as_ref()?;
        if instance.id != self.selected_instance_id.as_deref().unwrap_or("local") {
            return None;
        }
        let handshake = self.lifecycle.handshake.as_ref()?;
        if !handshake
            .capabilities
            .iter()
            .any(|c| c == sift_protocol::handshake::CAPABILITY_EXTENSION_CALLER_SCOPE)
        {
            return None;
        }
        Some(ExtensionCallerScope {
            instance_id: instance.id.clone(),
            base_url: instance.base_url.clone(),
            identity: sift_protocol::handshake::ExtensionCallerIdentity {
                instance_id: handshake.instance_id.clone(),
                daemon_generation: handshake.daemon_generation.clone(),
                principal_id: self.lifecycle.identity.as_ref()?.principal.id,
                auth_session_id: self.lifecycle.identity.as_ref()?.auth_session_id.clone(),
            },
        })
    }

    pub(super) fn bind_extension_caller_scope(&mut self) {
        let scope = self.extension_caller_scope();
        if let Some(sender) = &mut self.executor_sender {
            *sender = sender.with_extension_scope(scope);
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use sift_protocol::handshake::*;

    pub(crate) fn authenticate(shell: &mut WorkspaceShell) {
        let id = shell
            .selected_instance_id
            .clone()
            .unwrap_or_else(|| "local".into());
        shell.lifecycle.selected_instance = Some(crate::InstanceSpec {
            id,
            name: "Scoped fixture".into(),
            base_url: "http://127.0.0.1:7474".into(),
            kind: crate::InstanceKind::Local,
        });
        shell.lifecycle.handshake = Some(HandshakeResponse {
            server_version: "test".into(),
            protocol: ProtocolRange::exact(sift_protocol::PROTOCOL_VERSION_NUMBER),
            selected_protocol: sift_protocol::PROTOCOL_VERSION_NUMBER,
            instance_id: "scoped-fixture".into(),
            daemon_generation: "original-generation".into(),
            deployment: HandshakeDeployment::Personal,
            transport: HandshakeTransport::Loopback,
            runtime_mode: HandshakeRuntimeMode::InProcess,
            capabilities: vec![CAPABILITY_EXTENSION_CALLER_SCOPE.into()],
        });
        shell.lifecycle.identity = Some(sift_protocol::WhoAmIResponse {
            principal: sift_protocol::AuthPrincipal {
                id: 1,
                display_name: "Original owner".into(),
                email: None,
                avatar_url: None,
                is_instance_admin: true,
            },
            memberships: vec![],
            github_login: None,
            auth_session_id: None,
        });
    }

    pub(crate) fn command(command: ExecutorCommand) -> ExecutorCommand {
        match command {
            ExecutorCommand::ExtensionScoped { scope, command } => {
                assert_eq!(scope.identity.instance_id, "scoped-fixture");
                assert_eq!(scope.identity.principal_id, 1);
                *command
            }
            _ => panic!("extension commands require a captured caller"),
        }
    }

    #[gpui::test]
    fn queued_extension_identity_and_replies_survive_account_endpoint_and_daemon_changes(
        cx: &mut gpui::TestAppContext,
    ) {
        let window = super::super::tests::shell(cx);
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        let shell = window.root(&mut cx).unwrap();
        let (sender, mut commands) = ExecutorSender::channel(8);
        shell.update(&mut cx, |s, cx| {
            authenticate(s);
            s.executor_sender = Some(sender);
            s.open_extension_receipts(cx);
        });
        let ExecutorCommand::ExtensionScoped { scope, command } = commands.try_recv().unwrap()
        else {
            panic!("captured command")
        };
        let ExecutorCommand::LoadExtensionReceipts {
            generation,
            instance_id,
        } = *command
        else {
            panic!("receipt read")
        };
        assert_eq!(scope.identity.principal_id, 1);
        shell.update(&mut cx, |s, cx| {
            let instance = s.lifecycle.selected_instance.clone();
            let handshake = s.lifecycle.handshake.clone();
            for changed in ["account", "session", "endpoint", "daemon", "instance"] {
                authenticate(s);
                match changed {
                    "account" => s.lifecycle.identity.as_mut().unwrap().principal.id = 2,
                    "session" => {
                        s.lifecycle.identity.as_mut().unwrap().auth_session_id =
                            Some("replacement".into())
                    }
                    "endpoint" => s
                        .lifecycle
                        .selected_instance
                        .as_mut()
                        .unwrap()
                        .base_url
                        .push_str("/replacement"),
                    "daemon" => {
                        s.lifecycle.handshake.as_mut().unwrap().daemon_generation =
                            "replacement".into()
                    }
                    "instance" => {
                        s.lifecycle.handshake.as_mut().unwrap().instance_id = "replacement".into()
                    }
                    _ => unreachable!(),
                }
                s.on_executor_event(
                    ExecutorEvent::ExtensionScoped {
                        scope: scope.clone(),
                        event: Box::new(ExecutorEvent::ExtensionReceiptsLoaded {
                            generation,
                            instance_id: instance_id.clone(),
                            result: Err("private old account response".into()),
                        }),
                    },
                    cx,
                );
                assert!(s.extension_contributions.error.is_none(), "{changed}");
                assert!(s.extension_contributions.loading, "{changed}");
            }
            authenticate(s);
            s.lifecycle.selected_instance = instance;
            s.lifecycle.handshake = handshake;
            s.on_executor_event(
                ExecutorEvent::ExtensionScoped {
                    scope: scope.clone(),
                    event: Box::new(ExecutorEvent::ExtensionReceiptsLoaded {
                        generation,
                        instance_id,
                        result: Ok(vec![]),
                    }),
                },
                cx,
            );
            assert!(!s.extension_contributions.loading);
            // Losing authenticated identity refuses the next command at enqueue.
            s.lifecycle.identity = None;
            s.open_extension_receipts(cx);
        });
        assert!(commands.try_recv().is_err());
        assert_eq!(scope.identity.principal_id, 1);
        assert_eq!(scope.identity.daemon_generation, "original-generation");
        shell.update(&mut cx, |s, cx| {
            authenticate(s);
            s.connection_status = ConnectionStatus::Connected {
                profile_id: 7,
                name: "Original database".into(),
            };
            s.active_query_connection = Some((
                7,
                sift_protocol::SessionId(10),
                sift_protocol::ConnectionId(20),
            ));
            let original = s.current_extension_context(cx);
            assert_eq!(original.connection_id.as_deref(), Some("10:20"));
            s.extension_contributions.pending_request =
                Some(sift_protocol::InvokeExtensionRequest {
                    request_id: Some(uuid::Uuid::new_v4()),
                    operation: sift_protocol::ExtensionOperation {
                        extension_id: sift_protocol::ExtensionId::new("acme/fixture").unwrap(),
                        contribution_id: sift_protocol::ContributionId::new(
                            "acme/fixture/command/read",
                        )
                        .unwrap(),
                        action: sift_protocol::SegmentId::new("read").unwrap(),
                        classification: sift_protocol::OperationClassification::Read,
                        target_kind: sift_protocol::SegmentId::new("connection").unwrap(),
                        target_id: original.connection_id.clone(),
                        sanitized_arguments: Default::default(),
                    },
                    arguments: serde_json::json!({}),
                    context: Some(original),
                    approval_id: None,
                });
            s.extension_contributions.pending = true;
            s.active_query_connection = Some((
                7,
                sift_protocol::SessionId(10),
                sift_protocol::ConnectionId(21),
            ));
            s.on_executor_event(
                ExecutorEvent::ExtensionScoped {
                    scope,
                    event: Box::new(ExecutorEvent::ExtensionContributionInvoked {
                        generation: s.extension_contributions.generation,
                        result: Ok(sift_protocol::InvokeExtensionOutcome::Completed {
                            result: serde_json::json!({"private":"old source"}),
                        }),
                    }),
                },
                cx,
            );
            assert!(s.extension_contributions.result.is_none());
            assert!(!s.extension_contributions.pending);
            assert!(s
                .extension_contributions
                .error
                .as_deref()
                .unwrap()
                .contains("context changed"));
        });
    }
}
