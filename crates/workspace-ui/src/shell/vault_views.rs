//! Vault forms share the modal host's scroll and action visibility policy.

use super::*;

impl WorkspaceShell {
    pub(super) fn render_vault_form(
        &self,
        editing: bool,
        max_card_height: Pixels,
        cx: &mut Context<Self>,
    ) -> Div {
        let colors = cx.theme().colors;
        let body = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(if editing {
                        "Edit team vault"
                    } else {
                        "Create team vault"
                    }),
            )
            .when(!editing, |body| {
                body.child(div().text_sm().text_color(colors.muted_text).child(
                    "Team vaults start private. Share explicit capabilities after creation.",
                ))
            })
            .child(self.vault_name_input.clone())
            .children(
                self.vault_error
                    .as_ref()
                    .map(|message| ErrorBanner::new(message.clone())),
            );
        let actions = modal_layout::actions()
            .when(editing, |actions| {
                actions.child(
                    Button::new("delete-team-vault", "Delete vault")
                        .tone(ButtonTone::DangerMuted)
                        .disabled(self.vault_loading)
                        .on_click(cx.listener(|shell, _, _, cx| shell.delete_selected_vault(cx))),
                )
            })
            .child(
                Button::new(
                    if editing {
                        "cancel-edit-vault"
                    } else {
                        "cancel-create-vault"
                    },
                    "Cancel",
                )
                .tone(ButtonTone::Neutral)
                .on_click(cx.listener(|shell, _, window, cx| {
                    shell.dismiss_modal(&DismissModal, window, cx)
                })),
            )
            .child(
                Button::new(
                    if editing {
                        "submit-edit-vault"
                    } else {
                        "submit-create-vault"
                    },
                    if editing { "Rename" } else { "Create vault" },
                )
                .tone(ButtonTone::Accent)
                .disabled(self.vault_loading)
                .on_click(cx.listener(move |shell, _, _, cx| {
                    if editing {
                        shell.submit_vault_rename(cx)
                    } else {
                        shell.submit_create_vault(cx)
                    }
                })),
            );
        modal_layout::dialog(body, actions, max_card_height).debug_selector(move || {
            if editing {
                "edit-vault".into()
            } else {
                "create-vault".into()
            }
        })
    }
}
