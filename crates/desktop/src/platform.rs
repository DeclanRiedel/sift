use std::path::PathBuf;

use gpui::KeyBinding;
use sift_workspace_ui::{
    CloseActiveItem, CloseActivePane, DismissModal, FocusNextPane, OpenCommandPalette,
    OpenServerConnection, PaletteConfirm, PaletteDown, PaletteUp, SaveActiveItem, SplitPane,
    ToggleBottomDock, ToggleLeftDock, ToggleRightDock,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformKind {
    Linux,
    MacOS,
    Windows,
}

pub const fn current_platform() -> PlatformKind {
    if cfg!(target_os = "macos") {
        PlatformKind::MacOS
    } else if cfg!(target_os = "windows") {
        PlatformKind::Windows
    } else {
        PlatformKind::Linux
    }
}

pub fn presentation_state_path() -> PathBuf {
    #[cfg(target_os = "windows")]
    if let Some(root) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(root).join("Sift").join("presentation.json");
    }
    #[cfg(target_os = "macos")]
    if let Some(root) = std::env::var_os("HOME") {
        return PathBuf::from(root)
            .join("Library")
            .join("Application Support")
            .join("Sift")
            .join("presentation.json");
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(root) = std::env::var_os("XDG_STATE_HOME") {
            return PathBuf::from(root).join("sift").join("presentation.json");
        }
        if let Some(root) = std::env::var_os("HOME") {
            return PathBuf::from(root)
                .join(".local")
                .join("state")
                .join("sift")
                .join("presentation.json");
        }
    }
    std::env::temp_dir().join("sift-presentation.json")
}

pub fn instance_state_path() -> PathBuf {
    presentation_state_path().with_file_name("instances.json")
}

pub fn settings_path() -> PathBuf {
    #[cfg(target_os = "windows")]
    if let Some(root) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(root).join("Sift").join("settings.toml");
    }
    #[cfg(target_os = "macos")]
    if let Some(root) = std::env::var_os("HOME") {
        return PathBuf::from(root)
            .join("Library")
            .join("Application Support")
            .join("Sift")
            .join("settings.toml");
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(root) = std::env::var_os("XDG_CONFIG_HOME") {
            return PathBuf::from(root).join("sift").join("settings.toml");
        }
        if let Some(root) = std::env::var_os("HOME") {
            return PathBuf::from(root)
                .join(".config")
                .join("sift")
                .join("settings.toml");
        }
    }
    std::env::temp_dir().join("sift-settings.toml")
}

/// The platform's primary modifier: `cmd` on macOS, `ctrl` elsewhere. Every
/// chord-style binding builds its keystroke from this so keymaps stay
/// platform-native instead of binding both families unconditionally.
pub fn primary_modifier() -> &'static str {
    if current_platform() == PlatformKind::MacOS {
        "cmd"
    } else {
        "ctrl"
    }
}

pub fn shell_key_bindings() -> Vec<KeyBinding> {
    let primary = primary_modifier();
    let context = Some("SiftWorkspace");
    let mut bindings = vec![
        KeyBinding::new(&format!("{primary}-shift-p"), OpenCommandPalette, context),
        KeyBinding::new("escape", DismissModal, context),
        KeyBinding::new("up", PaletteUp, context),
        KeyBinding::new("down", PaletteDown, context),
        KeyBinding::new("enter", PaletteConfirm, context),
        KeyBinding::new(&format!("{primary}-\\"), SplitPane, context),
        KeyBinding::new(
            &format!("{primary}-k {primary}-right"),
            FocusNextPane,
            context,
        ),
        KeyBinding::new(&format!("{primary}-w"), CloseActiveItem, context),
        KeyBinding::new(&format!("{primary}-shift-w"), CloseActivePane, context),
        KeyBinding::new(&format!("{primary}-s"), SaveActiveItem, context),
        KeyBinding::new(
            &format!("{primary}-alt-c"),
            sift_workspace_ui::CancelExecution,
            context,
        ),
        KeyBinding::new(&format!("{primary}-shift-b"), ToggleLeftDock, context),
        KeyBinding::new(&format!("{primary}-shift-i"), ToggleRightDock, context),
        KeyBinding::new(&format!("{primary}-j"), ToggleBottomDock, context),
    ];

    // Vim-like Sift command language. Space is leader only in Vim normal
    // mode or non-text UI surfaces. Ctrl+K is the standard-keymap fallback.
    // Exact prefix actions keep the compact which-key guide synchronized with
    // GPUI's multi-keystroke resolver.
    for (leader, language_context) in [
        ("space", "vim_mode == normal"),
        ("space", "SiftWorkspace && !SiftEditor && !SiftTextInput"),
        ("ctrl-k", "SiftWorkspace && !SiftTextInput"),
    ] {
        bindings.extend([
            KeyBinding::new(
                leader,
                sift_workspace_ui::KeyLanguageRoot,
                Some(language_context),
            ),
            KeyBinding::new(
                &format!("{leader} f"),
                sift_workspace_ui::KeyLanguageFind,
                Some(language_context),
            ),
            KeyBinding::new(
                &format!("{leader} v"),
                sift_workspace_ui::KeyLanguageView,
                Some(language_context),
            ),
            KeyBinding::new(
                &format!("{leader} x"),
                sift_workspace_ui::KeyLanguageExecute,
                Some(language_context),
            ),
            KeyBinding::new(
                &format!("{leader} t"),
                sift_workspace_ui::KeyLanguageTab,
                Some(language_context),
            ),
            KeyBinding::new(
                &format!("{leader} e"),
                sift_workspace_ui::KeyLanguageEdit,
                Some(language_context),
            ),
            KeyBinding::new(
                &format!("{leader} d"),
                sift_workspace_ui::KeyLanguageDatabase,
                Some(language_context),
            ),
            KeyBinding::new(
                &format!("{leader} w"),
                sift_workspace_ui::KeyLanguageWorkspace,
                Some(language_context),
            ),
            KeyBinding::new(
                &format!("{leader} f c"),
                OpenCommandPalette,
                Some(language_context),
            ),
            KeyBinding::new(
                &format!("{leader} f d"),
                sift_workspace_ui::OpenSchemaSearch,
                Some(language_context),
            ),
            KeyBinding::new(
                &format!("{leader} f u"),
                sift_workspace_ui::editor::FindUsages,
                Some(language_context),
            ),
            KeyBinding::new(
                &format!("{leader} v d"),
                ToggleLeftDock,
                Some(language_context),
            ),
            KeyBinding::new(
                &format!("{leader} v i"),
                ToggleRightDock,
                Some(language_context),
            ),
            KeyBinding::new(
                &format!("{leader} v b"),
                ToggleBottomDock,
                Some(language_context),
            ),
            KeyBinding::new(
                &format!("{leader} x s"),
                sift_workspace_ui::editor::ExecuteStatement,
                Some(language_context),
            ),
            KeyBinding::new(
                &format!("{leader} x q"),
                sift_workspace_ui::editor::ExecuteDocument,
                Some(language_context),
            ),
            KeyBinding::new(
                &format!("{leader} x c"),
                sift_workspace_ui::CancelExecution,
                Some(language_context),
            ),
            KeyBinding::new(
                &format!("{leader} t c"),
                CloseActiveItem,
                Some(language_context),
            ),
            KeyBinding::new(
                &format!("{leader} t s"),
                SaveActiveItem,
                Some(language_context),
            ),
            KeyBinding::new(
                &format!("{leader} e f"),
                sift_workspace_ui::editor::FormatDocument,
                Some(language_context),
            ),
            KeyBinding::new(
                &format!("{leader} e q"),
                sift_workspace_ui::editor::ApplyQuickFix,
                Some(language_context),
            ),
            KeyBinding::new(
                &format!("{leader} d c"),
                OpenServerConnection,
                Some(language_context),
            ),
            KeyBinding::new(&format!("{leader} w s"), SplitPane, Some(language_context)),
            KeyBinding::new(
                &format!("{leader} w n"),
                FocusNextPane,
                Some(language_context),
            ),
            KeyBinding::new(
                &format!("{leader} w c"),
                CloseActivePane,
                Some(language_context),
            ),
        ]);
    }

    bindings.extend([
        KeyBinding::new(
            ":",
            OpenCommandPalette,
            Some("vim_mode == normal || (SiftWorkspace && !SiftEditor && !SiftTextInput)"),
        ),
        KeyBinding::new(
            "] d",
            sift_workspace_ui::editor::GoToNextDiagnostic,
            Some("vim_mode == normal"),
        ),
    ]);
    bindings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_boundary_selects_one_supported_desktop() {
        assert!(matches!(
            current_platform(),
            PlatformKind::Linux | PlatformKind::MacOS | PlatformKind::Windows
        ));
    }

    #[test]
    fn keymap_has_stable_action_coverage() {
        assert_eq!(shell_key_bindings().len(), 91);
    }
}
