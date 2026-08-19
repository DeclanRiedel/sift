//! Host-owned command metadata shared by menus, keybindings, and palette UI.
//!
//! Commands stay compile-time Rust values. Extensions may expose governed
//! server operations, but cannot register desktop commands or render UI.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommandId {
    ConnectServer,
    ExecuteStatement,
    ExecuteDocument,
    UndoQuery,
    RedoQuery,
    SplitPane,
    FocusNextPane,
    ClosePane,
    SaveItem,
    CloseItem,
    ToggleLeftDock,
    ToggleInspectorDock,
    ToggleBottomDock,
    OpenSettings,
    OpenServerConfiguration,
    OpenCommandPalette,
    ToggleTheme,
    Quit,
}

impl CommandId {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ConnectServer => "instance.connect-server",
            Self::ExecuteStatement => "query.execute-statement",
            Self::ExecuteDocument => "query.execute-document",
            Self::UndoQuery => "query.undo",
            Self::RedoQuery => "query.redo",
            Self::SplitPane => "workspace.split-pane",
            Self::FocusNextPane => "workspace.focus-next-pane",
            Self::ClosePane => "workspace.close-pane",
            Self::SaveItem => "workspace.save-item",
            Self::CloseItem => "workspace.close-item",
            Self::ToggleLeftDock => "workspace.toggle-left-dock",
            Self::ToggleInspectorDock => "workspace.toggle-right-dock",
            Self::ToggleBottomDock => "workspace.toggle-bottom-dock",
            Self::OpenSettings => "ui.open-settings",
            Self::OpenServerConfiguration => "instance.open-configuration",
            Self::OpenCommandPalette => "ui.command-palette",
            Self::ToggleTheme => "ui.toggle-theme",
            Self::Quit => "window.quit",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AvailabilityRule {
    Always,
    ActiveItem,
    MultiplePanes,
    EditableInstance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandDefinition {
    pub id: CommandId,
    pub label: &'static str,
    pub shortcut: &'static str,
    pub palette_visible: bool,
    availability: AvailabilityRule,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandContext {
    pub has_active_item: bool,
    pub pane_count: usize,
    pub has_editable_instance: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandSpec {
    pub id: CommandId,
    pub label: &'static str,
    pub shortcut: &'static str,
    pub disabled_reason: Option<&'static str>,
}

impl CommandSpec {
    pub fn enabled(&self) -> bool {
        self.disabled_reason.is_none()
    }
}

pub struct CommandRegistry;

impl CommandRegistry {
    pub fn definition(id: CommandId) -> &'static CommandDefinition {
        DEFINITIONS
            .iter()
            .find(|definition| definition.id == id)
            .expect("every command id must have one definition")
    }

    pub fn spec(id: CommandId, context: CommandContext) -> CommandSpec {
        let definition = Self::definition(id);
        CommandSpec {
            id,
            label: definition.label,
            shortcut: definition.shortcut,
            disabled_reason: match definition.availability {
                AvailabilityRule::Always => None,
                AvailabilityRule::ActiveItem if !context.has_active_item => Some("No active item"),
                AvailabilityRule::MultiplePanes if context.pane_count < 2 => Some("Only one pane"),
                AvailabilityRule::EditableInstance if !context.has_editable_instance => {
                    Some("Bundled Local Sift has no sift.toml")
                }
                AvailabilityRule::ActiveItem
                | AvailabilityRule::MultiplePanes
                | AvailabilityRule::EditableInstance => None,
            },
        }
    }

    pub fn palette(context: CommandContext) -> Vec<CommandSpec> {
        DEFINITIONS
            .iter()
            .filter(|definition| definition.palette_visible)
            .map(|definition| Self::spec(definition.id, context))
            .collect()
    }
}

const DEFINITIONS: &[CommandDefinition] = &[
    command(
        CommandId::ConnectServer,
        "Connect to Server…",
        "",
        true,
        AvailabilityRule::Always,
    ),
    command(
        CommandId::ExecuteStatement,
        "Run Current Statement",
        "Ctrl+Enter",
        true,
        AvailabilityRule::ActiveItem,
    ),
    command(
        CommandId::ExecuteDocument,
        "Run Entire Query Tab",
        "Ctrl+Shift+Enter",
        true,
        AvailabilityRule::ActiveItem,
    ),
    command(
        CommandId::UndoQuery,
        "Undo Query Edit",
        "Ctrl+Z",
        true,
        AvailabilityRule::ActiveItem,
    ),
    command(
        CommandId::RedoQuery,
        "Redo Query Edit",
        "Ctrl+Shift+Z",
        true,
        AvailabilityRule::ActiveItem,
    ),
    command(
        CommandId::SplitPane,
        "Split Pane",
        "Ctrl+\\",
        true,
        AvailabilityRule::Always,
    ),
    command(
        CommandId::FocusNextPane,
        "Focus Next Pane",
        "Ctrl+K Ctrl+→",
        true,
        AvailabilityRule::MultiplePanes,
    ),
    command(
        CommandId::ClosePane,
        "Close Pane",
        "Ctrl+Shift+W",
        true,
        AvailabilityRule::MultiplePanes,
    ),
    command(
        CommandId::SaveItem,
        "Save Active Item",
        "Ctrl+S",
        true,
        AvailabilityRule::ActiveItem,
    ),
    command(
        CommandId::CloseItem,
        "Close Active Item",
        "Ctrl+W",
        true,
        AvailabilityRule::ActiveItem,
    ),
    command(
        CommandId::ToggleLeftDock,
        "Toggle Left Dock",
        "Ctrl+Shift+B",
        true,
        AvailabilityRule::Always,
    ),
    command(
        CommandId::ToggleInspectorDock,
        "Toggle Inspector Dock",
        "Ctrl+Shift+I",
        true,
        AvailabilityRule::Always,
    ),
    command(
        CommandId::ToggleBottomDock,
        "Toggle Bottom Dock",
        "Ctrl+J",
        true,
        AvailabilityRule::Always,
    ),
    command(
        CommandId::OpenSettings,
        "Settings",
        "",
        false,
        AvailabilityRule::Always,
    ),
    command(
        CommandId::OpenServerConfiguration,
        "Edit Current sift.toml…",
        "",
        false,
        AvailabilityRule::EditableInstance,
    ),
    command(
        CommandId::OpenCommandPalette,
        "Command Palette…",
        "Ctrl+Shift+P",
        false,
        AvailabilityRule::Always,
    ),
    command(
        CommandId::ToggleTheme,
        "Toggle Light/Dark Theme",
        "",
        true,
        AvailabilityRule::Always,
    ),
    command(
        CommandId::Quit,
        "Quit Sift",
        "",
        false,
        AvailabilityRule::Always,
    ),
];

const fn command(
    id: CommandId,
    label: &'static str,
    shortcut: &'static str,
    palette_visible: bool,
    availability: AvailabilityRule,
) -> CommandDefinition {
    CommandDefinition {
        id,
        label,
        shortcut,
        palette_visible,
        availability,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn built_in_ids_are_unique_and_round_trip_to_definitions() {
        let mut ids = HashSet::new();
        for definition in DEFINITIONS {
            assert!(ids.insert(definition.id.as_str()));
            assert_eq!(CommandRegistry::definition(definition.id), definition);
        }
    }

    #[test]
    fn contextual_availability_is_resolved_once() {
        let empty = CommandContext {
            has_active_item: false,
            pane_count: 1,
            has_editable_instance: false,
        };
        assert_eq!(
            CommandRegistry::spec(CommandId::ExecuteStatement, empty).disabled_reason,
            Some("No active item")
        );
        assert_eq!(
            CommandRegistry::spec(CommandId::ClosePane, empty).disabled_reason,
            Some("Only one pane")
        );
        assert!(CommandRegistry::spec(CommandId::SplitPane, empty).enabled());
        assert_eq!(
            CommandRegistry::spec(CommandId::OpenServerConfiguration, empty).disabled_reason,
            Some("Bundled Local Sift has no sift.toml")
        );
    }
}
