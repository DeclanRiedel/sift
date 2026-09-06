//! Vault interaction commands; secret access remains server-owned.

use super::*;

impl WorkspaceShell {
    pub(super) fn selected_vault(&self) -> Option<&sift_api_types::Vault> {
        self.vaults.get(self.vault_selected)
    }

    pub(super) fn request_vaults(&mut self, cx: &mut Context<Self>) {
        let Some(tenant_id) = self.selected_tenant_id() else {
            self.vault_error = Some("No tenant is available".into());
            cx.notify();
            return;
        };
        let Some(sender) = &self.executor_sender else {
            self.vault_error = Some("Vault is unavailable".into());
            cx.notify();
            return;
        };
        self.vault_loading = sender
            .send(ExecutorCommand::LoadVaults { tenant_id })
            .is_ok();
        if self.vault_loading {
            self.vault_error = None;
        }
        cx.notify();
    }

    pub(super) fn request_selected_vault_items(&mut self, cx: &mut Context<Self>) {
        let Some(vault_id) = self.selected_vault().map(|vault| vault.id.0) else {
            self.vault_items.clear();
            cx.notify();
            return;
        };
        let Some(sender) = &self.executor_sender else {
            self.vault_error = Some("Vault is unavailable".into());
            cx.notify();
            return;
        };
        self.vault_loading = sender
            .send(ExecutorCommand::LoadVaultItems { vault_id })
            .is_ok();
        if self.vault_loading {
            self.vault_error = None;
        }
        cx.notify();
    }

    pub(super) fn select_vault(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.vaults.is_empty() {
            return;
        }
        self.vault_selected = index.min(self.vaults.len() - 1);
        self.vault_item_selected = 0;
        self.request_selected_vault_items(cx);
    }

    pub(super) fn open_create_vault(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.vault_error = None;
        self.modal = Some(Modal::CreateVault);
        self.vault_name_input
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
        cx.notify();
    }

    pub(super) fn open_edit_selected_vault(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(vault) = self.selected_vault() else {
            return;
        };
        if vault.scope != sift_protocol::VaultScope::Team || !vault.effective_capabilities.manage {
            self.vault_error = Some("Only manageable team vaults can be edited".into());
            cx.notify();
            return;
        }
        let name = vault.name.clone();
        self.vault_name_input
            .update(cx, |input, cx| input.set_text(name, cx));
        self.vault_error = None;
        self.modal = Some(Modal::EditVault);
        self.vault_name_input
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
        cx.notify();
    }

    pub(super) fn submit_vault_rename(&mut self, cx: &mut Context<Self>) {
        let Some(vault) = self.selected_vault() else {
            return;
        };
        let vault_id = vault.id.0;
        let expected_revision = vault.revision;
        let name = self.vault_name_input.read(cx).text().trim().to_owned();
        if name.is_empty() {
            self.vault_error = Some("Enter a vault name".into());
            cx.notify();
            return;
        }
        let Some(sender) = &self.executor_sender else {
            return;
        };
        self.vault_loading = sender
            .send(ExecutorCommand::UpdateVault {
                vault_id,
                request: sift_api_types::UpdateVaultRequest {
                    expected_revision,
                    name,
                },
            })
            .is_ok();
        cx.notify();
    }

    pub(super) fn delete_selected_vault(&mut self, cx: &mut Context<Self>) {
        let Some(vault) = self.selected_vault() else {
            return;
        };
        let Some(sender) = &self.executor_sender else {
            return;
        };
        self.vault_loading = sender
            .send(ExecutorCommand::DeleteVault {
                vault_id: vault.id.0,
                expected_revision: vault.revision,
            })
            .is_ok();
        cx.notify();
    }

    pub(super) fn filtered_vault_item_indices(&self, cx: &App) -> Vec<usize> {
        let query = self
            .vault_filter_input
            .read(cx)
            .text()
            .trim()
            .to_lowercase();
        self.vault_items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                (query.is_empty()
                    || item.label.to_lowercase().contains(&query)
                    || format!("{:?}", item.kind).to_lowercase().contains(&query))
                .then_some(index)
            })
            .collect()
    }

    pub(super) fn open_vault_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.vault_filter_input
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
    }

    pub(super) fn open_vault_item_shortcut(
        &mut self,
        item_id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self
            .vault_items
            .iter()
            .position(|item| item.id.0 == item_id)
        else {
            return;
        };
        self.vault_item_selected = index;
        self.active_left_panel = LeftPanel::Collaboration;
        self.collaboration_section = CollaborationSection::Vault;
        self.open_selected_vault_item(window, cx);
    }

    pub(super) fn submit_create_vault(&mut self, cx: &mut Context<Self>) {
        let Some(tenant_id) = self.selected_tenant_id() else {
            self.vault_error = Some("No tenant is available".into());
            return;
        };
        let name = self.vault_name_input.read(cx).text().trim().to_owned();
        if name.is_empty() {
            self.vault_error = Some("Enter a vault name".into());
            cx.notify();
            return;
        }
        let Some(sender) = &self.executor_sender else {
            self.vault_error = Some("Vault is unavailable".into());
            return;
        };
        self.vault_loading = sender
            .send(ExecutorCommand::CreateTeamVault { tenant_id, name })
            .is_ok();
        cx.notify();
    }

    pub(super) fn open_create_vault_item(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_vault().is_none() {
            self.show_toast("Select a vault first".into(), cx);
            return;
        }
        self.vault_error = None;
        self.modal = Some(Modal::CreateVaultItem);
        self.vault_item_label_input
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
        cx.notify();
    }

    pub(super) fn open_selected_vault_item(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.vault_items.get(self.vault_item_selected).cloned() else {
            self.show_toast("Select a vault item first".into(), cx);
            return;
        };
        let item_id = item.id.0;
        let Some(sender) = &self.executor_sender else {
            self.vault_error = Some("Vault is unavailable".into());
            return;
        };
        self.vault_detail_item_id = Some(item_id);
        self.vault_item_versions.clear();
        self.vault_grants.clear();
        self.vault_editor_section = VaultEditorSection::Overview;
        self.vault_editor_label_input
            .update(cx, |input, cx| input.set_text(item.label.clone(), cx));
        let detail = match &item.metadata {
            sift_api_types::VaultItemMetadata::Login { username, .. } => username.clone(),
            sift_api_types::VaultItemMetadata::Token { service, .. } => service.clone(),
            _ => String::new(),
        };
        self.vault_editor_detail_input
            .update(cx, |input, cx| input.set_text(detail, cx));
        self.vault_editor_secret_input
            .update(cx, |input, cx| input.set_text("", cx));
        self.vault_revealed_value = None;
        self.vault_error = None;
        self.vault_loading = sender
            .send(ExecutorCommand::LoadVaultItemVersions { item_id })
            .is_ok();
        if let Some(vault_id) = self.selected_vault().map(|vault| vault.id.0) {
            let _ = sender.send(ExecutorCommand::LoadVaultGrants { vault_id });
        }
        self.modal = Some(Modal::VaultItemDetails);
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    pub(super) fn reveal_selected_vault_item(&mut self, cx: &mut Context<Self>) {
        if self.vault_loading {
            return;
        }
        let Some(item_id) = self.vault_detail_item_id else {
            return;
        };
        let password = self.vault_reveal_password_input.read(cx).text().to_owned();
        let Some(sender) = &self.executor_sender else {
            self.vault_error = Some("Vault is unavailable".into());
            return;
        };
        self.vault_loading = sender
            .send(ExecutorCommand::RevealVaultItem {
                item_id,
                password: (!password.is_empty()).then_some(password),
            })
            .is_ok();
        cx.notify();
    }

    pub(super) fn copy_revealed_vault_value(&mut self, cx: &mut Context<Self>) {
        let Some(value) = self.vault_revealed_value.clone() else {
            return;
        };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(value.clone()));
        self.show_success_toast("Copied; clipboard clears in 30 seconds".into(), cx);
        cx.spawn(async move |shell, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(30))
                .await;
            let _ = shell.update(cx, |_, cx| {
                if cx
                    .read_from_clipboard()
                    .and_then(|item| item.text())
                    .as_deref()
                    == Some(value.as_str())
                {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(String::new()));
                }
            });
        })
        .detach();
    }

    pub(super) fn selected_vault_detail_item(&self) -> Option<&sift_api_types::VaultItem> {
        let id = self.vault_detail_item_id?;
        self.vault_items.iter().find(|item| item.id.0 == id)
    }

    pub(super) fn submit_vault_overview_update(&mut self, cx: &mut Context<Self>) {
        let Some(item) = self.selected_vault_detail_item().cloned() else {
            return;
        };
        let label = self
            .vault_editor_label_input
            .read(cx)
            .text()
            .trim()
            .to_owned();
        if label.is_empty() {
            self.vault_error = Some("Item name is required".into());
            cx.notify();
            return;
        }
        let detail = self
            .vault_editor_detail_input
            .read(cx)
            .text()
            .trim()
            .to_owned();
        let metadata = match item.metadata {
            sift_api_types::VaultItemMetadata::Login { url, .. } => {
                sift_api_types::VaultItemMetadata::Login {
                    username: detail,
                    url,
                }
            }
            sift_api_types::VaultItemMetadata::Token { expires_at, .. } => {
                sift_api_types::VaultItemMetadata::Token {
                    service: detail,
                    expires_at,
                }
            }
            metadata => metadata,
        };
        let Some(sender) = &self.executor_sender else {
            self.vault_error = Some("Vault is unavailable".into());
            return;
        };
        self.vault_loading = sender
            .send(ExecutorCommand::UpdateVaultItem {
                item_id: item.id.0,
                request: sift_api_types::UpdateVaultItemRequest {
                    expected_revision: item.revision,
                    label,
                    metadata,
                    secret: None,
                },
            })
            .is_ok();
        cx.notify();
    }

    pub(super) fn rotate_vault_item_secret(&mut self, cx: &mut Context<Self>) {
        let Some(item) = self.selected_vault_detail_item().cloned() else {
            return;
        };
        if item.kind == sift_protocol::VaultItemKind::Connection {
            self.vault_error = Some("Edit connection credentials from Connections".into());
            cx.notify();
            return;
        }
        let secret = self.vault_editor_secret_input.read(cx).text().to_owned();
        if secret.is_empty() {
            self.vault_error = Some("Enter a new secret value".into());
            cx.notify();
            return;
        }
        let Some(sender) = &self.executor_sender else {
            return;
        };
        self.vault_loading = sender
            .send(ExecutorCommand::SetVaultItemSecret {
                item_id: item.id.0,
                request: sift_api_types::SetVaultSecretRequest {
                    expected_revision: item.revision,
                    secret: serde_json::Value::String(secret),
                },
            })
            .is_ok();
        cx.notify();
    }

    pub(super) fn clear_vault_item_secret(&mut self, cx: &mut Context<Self>) {
        let Some(item) = self.selected_vault_detail_item() else {
            return;
        };
        let Some(sender) = &self.executor_sender else {
            return;
        };
        self.vault_loading = sender
            .send(ExecutorCommand::ClearVaultItemSecret {
                item_id: item.id.0,
                expected_revision: item.revision,
            })
            .is_ok();
        cx.notify();
    }

    pub(super) fn restore_vault_item_version(&mut self, version: u64, cx: &mut Context<Self>) {
        let Some(item) = self.selected_vault_detail_item() else {
            return;
        };
        let Some(sender) = &self.executor_sender else {
            return;
        };
        self.vault_loading = sender
            .send(ExecutorCommand::RestoreVaultItem {
                item_id: item.id.0,
                request: sift_api_types::RestoreVaultItemRequest {
                    expected_revision: item.revision,
                    version,
                },
            })
            .is_ok();
        cx.notify();
    }

    pub(super) fn delete_vault_detail_item(&mut self, cx: &mut Context<Self>) {
        let Some(item) = self.selected_vault_detail_item() else {
            return;
        };
        let Some(vault_id) = self.selected_vault().map(|vault| vault.id.0) else {
            return;
        };
        let Some(sender) = &self.executor_sender else {
            return;
        };
        self.vault_loading = sender
            .send(ExecutorCommand::DeleteVaultItem {
                vault_id,
                item_id: item.id.0,
                expected_revision: item.revision,
            })
            .is_ok();
        cx.notify();
    }

    pub(super) fn test_vault_detail_connection(&mut self, cx: &mut Context<Self>) {
        let Some(item_id) = self.vault_detail_item_id else {
            return;
        };
        let Some(sender) = &self.executor_sender else {
            return;
        };
        self.vault_loading = sender
            .send(ExecutorCommand::TestVaultItem { item_id })
            .is_ok();
        cx.notify();
    }

    pub(super) fn set_vault_grant_preset(
        &mut self,
        capabilities: sift_protocol::VaultCapabilities,
        cx: &mut Context<Self>,
    ) {
        let Some(vault_id) = self.selected_vault().map(|vault| vault.id.0) else {
            return;
        };
        let Ok(principal_id) = self
            .vault_grant_principal_input
            .read(cx)
            .text()
            .trim()
            .parse::<i64>()
        else {
            self.vault_error = Some("Enter a numeric principal ID".into());
            cx.notify();
            return;
        };
        let expected_revision = self
            .vault_grants
            .iter()
            .find(|grant| grant.principal_id.0 == principal_id)
            .map(|grant| grant.revision);
        let Some(sender) = &self.executor_sender else {
            return;
        };
        self.vault_loading = sender
            .send(ExecutorCommand::SetVaultGrant {
                vault_id,
                principal_id,
                request: sift_api_types::SetVaultGrantRequest {
                    expected_revision,
                    capabilities,
                },
            })
            .is_ok();
        cx.notify();
    }

    pub(super) fn revoke_vault_grant(
        &mut self,
        principal_id: i64,
        revision: u64,
        cx: &mut Context<Self>,
    ) {
        let Some(vault_id) = self.selected_vault().map(|vault| vault.id.0) else {
            return;
        };
        let Some(sender) = &self.executor_sender else {
            return;
        };
        self.vault_loading = sender
            .send(ExecutorCommand::DeleteVaultGrant {
                vault_id,
                principal_id,
                expected_revision: revision,
            })
            .is_ok();
        cx.notify();
    }

    pub(super) fn handle_vault_editor_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.modifiers.modified() {
            return;
        }
        let current = VaultEditorSection::ALL
            .iter()
            .position(|section| *section == self.vault_editor_section)
            .unwrap_or_default();
        match event.keystroke.key.as_str() {
            "h" | "left" => {
                self.vault_editor_section = VaultEditorSection::ALL[current.saturating_sub(1)]
            }
            "l" | "right" => {
                self.vault_editor_section =
                    VaultEditorSection::ALL[(current + 1).min(VaultEditorSection::ALL.len() - 1)]
            }
            "1" => self.vault_editor_section = VaultEditorSection::Overview,
            "2" => self.vault_editor_section = VaultEditorSection::Secret,
            "3" => self.vault_editor_section = VaultEditorSection::Access,
            "4" => self.vault_editor_section = VaultEditorSection::History,
            "s" if self.vault_editor_section == VaultEditorSection::Overview => {
                self.submit_vault_overview_update(cx)
            }
            "r" if self.vault_editor_section == VaultEditorSection::Secret => {
                self.rotate_vault_item_secret(cx)
            }
            "c" if self.vault_editor_section == VaultEditorSection::Secret => {
                self.clear_vault_item_secret(cx)
            }
            "t" if self.vault_editor_section == VaultEditorSection::Secret => {
                self.test_vault_detail_connection(cx)
            }
            "escape" => self.dismiss_modal(&DismissModal, window, cx),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    pub(super) fn submit_create_vault_item(&mut self, cx: &mut Context<Self>) {
        let Some(vault_id) = self.selected_vault().map(|vault| vault.id.0) else {
            return;
        };
        let label = self
            .vault_item_label_input
            .read(cx)
            .text()
            .trim()
            .to_owned();
        let detail = self
            .vault_item_detail_input
            .read(cx)
            .text()
            .trim()
            .to_owned();
        let secret = self.vault_item_secret_input.read(cx).text().to_owned();
        if label.is_empty() || secret.is_empty() {
            self.vault_error = Some("Item name and secret are required".into());
            cx.notify();
            return;
        }
        let metadata = match self.vault_item_draft_kind {
            VaultItemDraftKind::Login => sift_api_types::VaultItemMetadata::Login {
                username: detail,
                url: None,
            },
            VaultItemDraftKind::Token => sift_api_types::VaultItemMetadata::Token {
                service: detail,
                expires_at: None,
            },
            VaultItemDraftKind::SecureNote => sift_api_types::VaultItemMetadata::SecureNote,
        };
        let request = sift_api_types::CreateVaultItemRequest {
            label,
            metadata,
            secret: Some(serde_json::Value::String(secret)),
        };
        let Some(sender) = &self.executor_sender else {
            self.vault_error = Some("Vault is unavailable".into());
            return;
        };
        self.vault_loading = sender
            .send(ExecutorCommand::CreateVaultItem { vault_id, request })
            .is_ok();
        cx.notify();
    }

    pub(super) fn share_selected_vault_with_room(&mut self, cx: &mut Context<Self>) {
        let Some(vault_id) = self.selected_vault().and_then(|vault| {
            (vault.scope == sift_protocol::VaultScope::Team && vault.effective_capabilities.manage)
                .then_some(vault.id.0)
        }) else {
            self.vault_error = Some("Select a manageable team vault".into());
            cx.notify();
            return;
        };
        let mut principal_ids = self
            .room_members
            .iter()
            .map(|member| member.principal_id.0)
            .collect::<Vec<_>>();
        principal_ids.sort_unstable();
        principal_ids.dedup();
        if principal_ids.is_empty() {
            self.vault_error = Some("No room members are available to share with".into());
            cx.notify();
            return;
        }
        let Some(sender) = &self.executor_sender else {
            self.vault_error = Some("Vault is unavailable".into());
            return;
        };
        self.vault_loading = sender
            .send(ExecutorCommand::ShareVault {
                vault_id,
                principal_ids,
            })
            .is_ok();
        cx.notify();
    }
}
