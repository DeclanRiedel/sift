use std::path::PathBuf;

use gpui::KeyBinding;
use sift_workspace_ui::{
    CloseActiveItem, DismissModal, FocusNextPane, OpenCommandPalette, SaveActiveItem, SplitPane,
    ToggleBottomDock, ToggleLeftDock, ToggleRightDock, ToggleShellTheme,
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

pub fn shell_key_bindings() -> Vec<KeyBinding> {
    let primary = if current_platform() == PlatformKind::MacOS {
        "cmd"
    } else {
        "ctrl"
    };
    let context = Some("SiftWorkspace");
    vec![
        KeyBinding::new(&format!("{primary}-shift-p"), OpenCommandPalette, context),
        KeyBinding::new("escape", DismissModal, context),
        KeyBinding::new(&format!("{primary}-\\"), SplitPane, context),
        KeyBinding::new(
            &format!("{primary}-k {primary}-right"),
            FocusNextPane,
            context,
        ),
        KeyBinding::new(&format!("{primary}-w"), CloseActiveItem, context),
        KeyBinding::new(&format!("{primary}-s"), SaveActiveItem, context),
        KeyBinding::new(&format!("{primary}-shift-b"), ToggleLeftDock, context),
        KeyBinding::new(&format!("{primary}-shift-i"), ToggleRightDock, context),
        KeyBinding::new(&format!("{primary}-j"), ToggleBottomDock, context),
        KeyBinding::new(&format!("{primary}-shift-t"), ToggleShellTheme, context),
    ]
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
        assert_eq!(shell_key_bindings().len(), 10);
    }
}
