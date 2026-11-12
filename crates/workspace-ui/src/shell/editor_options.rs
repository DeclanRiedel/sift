use super::*;
use crate::editor::line_numbers::{LineNumberCommand, LineNumberOptions};

impl WorkspaceShell {
    pub(super) fn apply_line_number_command(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = (|| -> Result<Vec<String>, String> {
            let command =
                LineNumberCommand::parse(text).ok_or("Not an editor option command")??;
            if self.command_palette_origin != WorkspaceSurface::Editor {
                return Err("Focus an editor before changing its line numbers".into());
            }
            let editor = self
                .panes
                .get(self.active_pane)
                .and_then(|pane| {
                    let pane = pane.read(cx);
                    pane.active_item().and_then(|item| pane.editor(item.id))
                })
                .ok_or("Active tab has no editor")?;
            let current = editor.read(cx).line_number_options(cx);
            let (updated, changed, reports) = command.apply(current);
            if changed.iter().any(|changed| *changed) {
                if !command.local {
                    let mut defaults = cx
                        .try_global::<LineNumberOptions>()
                        .copied()
                        .unwrap_or_default();
                    if changed[0] {
                        defaults.number = updated.number;
                    }
                    if changed[1] {
                        defaults.relativenumber = updated.relativenumber;
                    }
                    if let Some(store) = &self.settings_store {
                        self.settings = store.save_line_numbers(defaults)?;
                    } else {
                        self.settings.editor.line_numbers = defaults;
                    }
                    cx.set_global(defaults);
                    cx.refresh_windows();
                }
                editor.update(cx, |editor, cx| {
                    editor.set_line_number_options(updated, changed, command.local, cx)
                });
            }
            Ok(reports)
        })();
        match result {
            Ok(reports) => {
                self.dismiss_modal(&DismissModal, window, cx);
                self.show_toast(
                    if reports.is_empty() {
                        "Line numbers updated".into()
                    } else {
                        reports.join(" ")
                    },
                    cx,
                );
            }
            Err(error) => self.show_toast(error, cx),
        }
    }
}
