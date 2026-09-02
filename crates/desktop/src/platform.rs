use std::path::PathBuf;

use gpui::KeyBinding;
use sift_workspace_ui::{
    CloseActiveItem, CloseActivePane, DismissModal, OpenCommandPalette, PaletteConfirm,
    PaletteDown, PaletteUp, PaneNavigateBack, PaneNavigateForward, SaveActiveItem, SplitPane,
    StageJsonResultEdit, ToggleBottomDock, ToggleFrameMetrics, ToggleLeftDock, ToggleRightDock,
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
    let standard_context = Some("SiftWorkspace && keymap_profile != vim");
    let mut bindings = vec![
        KeyBinding::new("escape", DismissModal, context),
        KeyBinding::new("up", PaletteUp, context),
        KeyBinding::new("down", PaletteDown, context),
        KeyBinding::new("ctrl-k", PaletteUp, Some("SiftModal")),
        KeyBinding::new("ctrl-j", PaletteDown, Some("SiftModal")),
        KeyBinding::new("enter", PaletteConfirm, context),
        KeyBinding::new("ctrl-left", PaneNavigateBack, Some("SiftPane")),
        KeyBinding::new("ctrl-right", PaneNavigateForward, Some("SiftPane")),
        KeyBinding::new(
            &format!("{primary}-enter"),
            StageJsonResultEdit,
            Some("SiftJsonResultEditor"),
        ),
        KeyBinding::new(
            &format!("{primary}-shift-p"),
            OpenCommandPalette,
            standard_context,
        ),
        KeyBinding::new(&format!("{primary}-\\"), SplitPane, standard_context),
        KeyBinding::new(&format!("{primary}-w"), CloseActiveItem, standard_context),
        KeyBinding::new(
            &format!("{primary}-shift-w"),
            CloseActivePane,
            standard_context,
        ),
        KeyBinding::new(&format!("{primary}-s"), SaveActiveItem, standard_context),
        KeyBinding::new(
            &format!("{primary}-alt-c"),
            sift_workspace_ui::CancelExecution,
            standard_context,
        ),
        KeyBinding::new(
            &format!("{primary}-shift-b"),
            ToggleLeftDock,
            standard_context,
        ),
        KeyBinding::new(
            &format!("{primary}-shift-i"),
            ToggleRightDock,
            standard_context,
        ),
        KeyBinding::new(&format!("{primary}-j"), ToggleBottomDock, standard_context),
        KeyBinding::new("ctrl-alt-shift-p", ToggleFrameMetrics, context),
    ];

    bindings.extend([
        KeyBinding::new(
            ":",
            OpenCommandPalette,
            Some("keymap_profile != standard && (vim_mode == normal || (SiftWorkspace && !SiftEditor && !SiftTextInput))"),
        ),
        KeyBinding::new(
            "] d",
            sift_workspace_ui::editor::GoToNextDiagnostic,
            Some("keymap_profile != standard && vim_mode == normal"),
        ),
        KeyBinding::new(
            "g *",
            sift_workspace_ui::editor::ExpandStar,
            Some("keymap_profile != standard && vim_mode == normal"),
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
        assert_eq!(shell_key_bindings().len(), 21);
    }
}
