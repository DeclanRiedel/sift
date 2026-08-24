//! Host-owned command metadata shared by menus, keybindings, and palette UI.
//!
//! Commands stay compile-time Rust values. Extensions may expose governed
//! server operations, but cannot register desktop commands or render UI.

use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommandId {
    ConnectServer,
    ExecuteStatement,
    ExecuteDocument,
    CancelExecution,
    UndoQuery,
    RedoQuery,
    CompleteSql,
    FormatSql,
    ApplySqlQuickFix,
    FindSqlUsages,
    SearchSchema,
    NextSqlProblem,
    SplitPane,
    FocusNextPane,
    FocusPaneLeft,
    FocusPaneDown,
    FocusPaneUp,
    FocusPaneRight,
    FocusConnections,
    FocusEditor,
    FocusInspector,
    FocusResults,
    FocusProblems,
    PreviousTab,
    NextTab,
    ClosePane,
    SaveItem,
    CloseItem,
    ToggleLeftDock,
    ToggleInspectorDock,
    ToggleBottomDock,
    OpenSettings,
    OpenKeymaps,
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
            Self::CancelExecution => "query.cancel",
            Self::UndoQuery => "query.undo",
            Self::RedoQuery => "query.redo",
            Self::CompleteSql => "query.complete",
            Self::FormatSql => "query.format",
            Self::ApplySqlQuickFix => "query.quick-fix",
            Self::FindSqlUsages => "query.find-usages",
            Self::SearchSchema => "database.search-schema",
            Self::NextSqlProblem => "query.next-problem",
            Self::SplitPane => "workspace.split-pane",
            Self::FocusNextPane => "workspace.focus-next-pane",
            Self::FocusPaneLeft => "workspace.focus-pane-left",
            Self::FocusPaneDown => "workspace.focus-pane-down",
            Self::FocusPaneUp => "workspace.focus-pane-up",
            Self::FocusPaneRight => "workspace.focus-pane-right",
            Self::FocusConnections => "workspace.focus-connections",
            Self::FocusEditor => "workspace.focus-editor",
            Self::FocusInspector => "workspace.focus-inspector",
            Self::FocusResults => "workspace.focus-results",
            Self::FocusProblems => "workspace.focus-problems",
            Self::PreviousTab => "workspace.previous-tab",
            Self::NextTab => "workspace.next-tab",
            Self::ClosePane => "workspace.close-pane",
            Self::SaveItem => "workspace.save-item",
            Self::CloseItem => "workspace.close-item",
            Self::ToggleLeftDock => "workspace.toggle-left-dock",
            Self::ToggleInspectorDock => "workspace.toggle-right-dock",
            Self::ToggleBottomDock => "workspace.toggle-bottom-dock",
            Self::OpenSettings => "ui.open-settings",
            Self::OpenKeymaps => "ui.open-keymaps",
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
    RunningQuery,
    ConnectedDatabase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandDefinition {
    pub id: CommandId,
    pub label: &'static str,
    pub shortcut: &'static str,
    /// Sift's mnemonic command-language spelling. `<leader>` is Space in
    /// Vim/UI normal mode and Ctrl+K from the standard keymap.
    pub language: &'static str,
    pub palette_visible: bool,
    availability: AvailabilityRule,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandContext {
    pub has_active_item: bool,
    pub pane_count: usize,
    pub has_editable_instance: bool,
    pub active_query_running: bool,
    pub database_connected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub id: CommandId,
    pub label: &'static str,
    pub shortcut: &'static str,
    pub language: String,
    pub disabled_reason: Option<&'static str>,
}

impl CommandSpec {
    pub fn enabled(&self) -> bool {
        self.disabled_reason.is_none()
    }
}

pub struct CommandRegistry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandLanguageMatch {
    Prefix,
    Command(CommandId),
    Invalid,
}

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
            language: definition.language.into(),
            disabled_reason: match definition.availability {
                AvailabilityRule::Always => None,
                AvailabilityRule::ActiveItem if !context.has_active_item => Some("No active item"),
                AvailabilityRule::MultiplePanes if context.pane_count < 2 => Some("Only one pane"),
                AvailabilityRule::EditableInstance if !context.has_editable_instance => {
                    Some("Bundled Local Sift has no sift.toml")
                }
                AvailabilityRule::RunningQuery if !context.active_query_running => {
                    Some("Active query is not running")
                }
                AvailabilityRule::ConnectedDatabase if !context.database_connected => {
                    Some("No database connected")
                }
                AvailabilityRule::ActiveItem
                | AvailabilityRule::MultiplePanes
                | AvailabilityRule::EditableInstance
                | AvailabilityRule::RunningQuery
                | AvailabilityRule::ConnectedDatabase => None,
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

    pub fn palette_with(
        context: CommandContext,
        bindings: &BTreeMap<String, String>,
    ) -> Vec<CommandSpec> {
        Self::palette(context)
            .into_iter()
            .map(|mut spec| {
                if let Some(language) = bindings.get(spec.id.as_str()) {
                    spec.language.clone_from(language);
                }
                spec
            })
            .collect()
    }

    pub const fn definitions() -> &'static [CommandDefinition] {
        DEFINITIONS
    }

    pub fn id_from_str(id: &str) -> Option<CommandId> {
        DEFINITIONS
            .iter()
            .find(|definition| definition.id.as_str() == id)
            .map(|definition| definition.id)
    }

    /// Resolve a workspace-owned leader sequence without relying on GPUI's
    /// timed multi-stroke replay. `keys` excludes the leader itself.
    pub fn resolve_language(keys: &[String]) -> CommandLanguageMatch {
        Self::resolve_language_with(keys, &BTreeMap::new())
    }

    pub fn resolve_language_with(
        keys: &[String],
        bindings: &BTreeMap<String, String>,
    ) -> CommandLanguageMatch {
        if keys.is_empty() {
            return CommandLanguageMatch::Prefix;
        }
        let entered = format!("<leader> {}", keys.join(" "));
        if let Some(command) = DEFINITIONS.iter().find(|definition| {
            bindings
                .get(definition.id.as_str())
                .map_or(definition.language, String::as_str)
                == entered
        }) {
            return CommandLanguageMatch::Command(command.id);
        }

        let prefix = format!("{entered} ");
        if DEFINITIONS.iter().any(|definition| {
            let language = bindings
                .get(definition.id.as_str())
                .map_or(definition.language, String::as_str);
            language.starts_with("<leader>") && language.starts_with(&prefix)
        }) {
            CommandLanguageMatch::Prefix
        } else {
            CommandLanguageMatch::Invalid
        }
    }
}

const DEFINITIONS: &[CommandDefinition] = &[
    command(
        CommandId::ConnectServer,
        "Connect to Server…",
        "",
        "<leader> d c",
        true,
        AvailabilityRule::Always,
    ),
    command(
        CommandId::ExecuteStatement,
        "Run Current Statement",
        "Ctrl+Enter",
        "<leader> x s",
        true,
        AvailabilityRule::ActiveItem,
    ),
    command(
        CommandId::ExecuteDocument,
        "Run Entire Query Tab",
        "Ctrl+Shift+Enter",
        "<leader> x q",
        true,
        AvailabilityRule::ActiveItem,
    ),
    command(
        CommandId::CancelExecution,
        "Cancel Query",
        "Ctrl+Alt+C",
        "<leader> x c",
        true,
        AvailabilityRule::RunningQuery,
    ),
    command(
        CommandId::UndoQuery,
        "Undo Query Edit",
        "Ctrl+Z",
        "u",
        true,
        AvailabilityRule::ActiveItem,
    ),
    command(
        CommandId::RedoQuery,
        "Redo Query Edit",
        "Ctrl+Shift+Z",
        "Ctrl+R",
        true,
        AvailabilityRule::ActiveItem,
    ),
    command(
        CommandId::CompleteSql,
        "Suggest Completions",
        "Ctrl+Space",
        "Ctrl+Space",
        true,
        AvailabilityRule::ActiveItem,
    ),
    command(
        CommandId::FormatSql,
        "Format SQL",
        "Ctrl+Alt+L",
        "<leader> e f",
        true,
        AvailabilityRule::ActiveItem,
    ),
    command(
        CommandId::ApplySqlQuickFix,
        "Apply Quick Fix at Caret",
        "Alt+Enter",
        "<leader> e q",
        true,
        AvailabilityRule::ActiveItem,
    ),
    command(
        CommandId::FindSqlUsages,
        "Find Usages at Caret",
        "",
        "<leader> f u",
        true,
        AvailabilityRule::ActiveItem,
    ),
    command(
        CommandId::SearchSchema,
        "Search Database Schema…",
        "",
        "<leader> f d",
        true,
        AvailabilityRule::ConnectedDatabase,
    ),
    command(
        CommandId::NextSqlProblem,
        "Go to Next Problem",
        "",
        "] d",
        true,
        AvailabilityRule::ActiveItem,
    ),
    command(
        CommandId::SplitPane,
        "Split Pane",
        "Ctrl+\\",
        "<leader> w s",
        true,
        AvailabilityRule::Always,
    ),
    command(
        CommandId::FocusNextPane,
        "Focus Next Pane",
        "",
        "<leader> w n",
        true,
        AvailabilityRule::MultiplePanes,
    ),
    command(
        CommandId::FocusPaneLeft,
        "Focus Pane Left",
        "Ctrl+W h",
        "<leader> w h",
        true,
        AvailabilityRule::MultiplePanes,
    ),
    command(
        CommandId::FocusPaneDown,
        "Focus Pane Below",
        "Ctrl+W j",
        "<leader> w j",
        true,
        AvailabilityRule::MultiplePanes,
    ),
    command(
        CommandId::FocusPaneUp,
        "Focus Pane Above",
        "Ctrl+W k",
        "<leader> w k",
        true,
        AvailabilityRule::MultiplePanes,
    ),
    command(
        CommandId::FocusPaneRight,
        "Focus Pane Right",
        "Ctrl+W l",
        "<leader> w l",
        true,
        AvailabilityRule::MultiplePanes,
    ),
    command(
        CommandId::FocusConnections,
        "Focus Connections",
        "",
        "<leader> g c",
        true,
        AvailabilityRule::Always,
    ),
    command(
        CommandId::FocusEditor,
        "Focus Editor",
        "",
        "<leader> g e",
        true,
        AvailabilityRule::ActiveItem,
    ),
    command(
        CommandId::FocusInspector,
        "Focus Inspector",
        "",
        "<leader> g i",
        true,
        AvailabilityRule::ActiveItem,
    ),
    command(
        CommandId::FocusResults,
        "Focus Query Results",
        "",
        "<leader> g r",
        true,
        AvailabilityRule::ActiveItem,
    ),
    command(
        CommandId::FocusProblems,
        "Focus Problems",
        "",
        "<leader> g p",
        true,
        AvailabilityRule::Always,
    ),
    command(
        CommandId::PreviousTab,
        "Focus Tab Left",
        "",
        "<leader> h",
        true,
        AvailabilityRule::ActiveItem,
    ),
    command(
        CommandId::NextTab,
        "Focus Tab Right",
        "",
        "<leader> l",
        true,
        AvailabilityRule::ActiveItem,
    ),
    command(
        CommandId::ClosePane,
        "Close Tabs in Pane",
        "Ctrl+Shift+W",
        "<leader> w c",
        true,
        AvailabilityRule::MultiplePanes,
    ),
    command(
        CommandId::SaveItem,
        "Save Active Item",
        "Ctrl+S",
        "<leader> t s",
        true,
        AvailabilityRule::ActiveItem,
    ),
    command(
        CommandId::CloseItem,
        "Close Active Item",
        "Ctrl+W",
        "<leader> t c",
        true,
        AvailabilityRule::ActiveItem,
    ),
    command(
        CommandId::ToggleLeftDock,
        "Toggle Left Dock",
        "Ctrl+Shift+B",
        "<leader> v d",
        true,
        AvailabilityRule::Always,
    ),
    command(
        CommandId::ToggleInspectorDock,
        "Toggle Inspector Dock",
        "Ctrl+Shift+I",
        "<leader> v i",
        true,
        AvailabilityRule::Always,
    ),
    command(
        CommandId::ToggleBottomDock,
        "Toggle Bottom Dock",
        "Ctrl+J",
        "<leader> v b",
        true,
        AvailabilityRule::Always,
    ),
    command(
        CommandId::OpenSettings,
        "Settings",
        "",
        "",
        false,
        AvailabilityRule::Always,
    ),
    command(
        CommandId::OpenKeymaps,
        "Keymaps",
        "",
        "<leader> e k",
        true,
        AvailabilityRule::Always,
    ),
    command(
        CommandId::OpenServerConfiguration,
        "Edit Current sift.toml…",
        "",
        "",
        false,
        AvailabilityRule::EditableInstance,
    ),
    command(
        CommandId::OpenCommandPalette,
        "Command Palette…",
        "Ctrl+Shift+P",
        ":",
        false,
        AvailabilityRule::Always,
    ),
    command(
        CommandId::ToggleTheme,
        "Toggle Light/Dark Theme",
        "",
        "",
        true,
        AvailabilityRule::Always,
    ),
    command(
        CommandId::Quit,
        "Quit Sift",
        "",
        "",
        false,
        AvailabilityRule::Always,
    ),
];

const fn command(
    id: CommandId,
    label: &'static str,
    shortcut: &'static str,
    language: &'static str,
    palette_visible: bool,
    availability: AvailabilityRule,
) -> CommandDefinition {
    CommandDefinition {
        id,
        label,
        shortcut,
        language,
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
    fn defaults_have_no_function_key_dependency() {
        for definition in DEFINITIONS {
            assert!(
                !definition
                    .shortcut
                    .split('+')
                    .any(|part| part.starts_with('F')
                        && part[1..].chars().all(|c| c.is_ascii_digit())),
                "{} still advertises a function-key shortcut",
                definition.id.as_str()
            );
        }
    }

    #[test]
    fn wiki_covers_every_available_leader_command() {
        let wiki = include_str!("../../../../docs/keyboard-wiki/index.html");
        for definition in DEFINITIONS
            .iter()
            .filter(|definition| definition.language.starts_with("<leader>"))
        {
            let encoded = definition
                .language
                .replace('<', "&lt;")
                .replace('>', "&gt;");
            assert!(
                wiki.contains(&encoded),
                "keyboard wiki is missing {} ({})",
                definition.id.as_str(),
                definition.language
            );
        }
    }

    #[test]
    fn contextual_availability_is_resolved_once() {
        let empty = CommandContext {
            has_active_item: false,
            pane_count: 1,
            has_editable_instance: false,
            active_query_running: false,
            database_connected: false,
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
        assert_eq!(
            CommandRegistry::spec(CommandId::CancelExecution, empty).disabled_reason,
            Some("Active query is not running")
        );
        assert_eq!(
            CommandRegistry::spec(CommandId::SearchSchema, empty).disabled_reason,
            Some("No database connected")
        );
        assert!(CommandRegistry::spec(
            CommandId::CancelExecution,
            CommandContext {
                active_query_running: true,
                ..empty
            }
        )
        .enabled());
        assert!(CommandRegistry::spec(
            CommandId::SearchSchema,
            CommandContext {
                database_connected: true,
                ..empty
            }
        )
        .enabled());
    }

    #[test]
    fn leader_language_distinguishes_prefix_command_and_invalid_input() {
        assert_eq!(
            CommandRegistry::resolve_language(&[]),
            CommandLanguageMatch::Prefix
        );
        assert_eq!(
            CommandRegistry::resolve_language(&["x".into()]),
            CommandLanguageMatch::Prefix
        );
        assert_eq!(
            CommandRegistry::resolve_language(&["x".into(), "s".into()]),
            CommandLanguageMatch::Command(CommandId::ExecuteStatement)
        );
        assert_eq!(
            CommandRegistry::resolve_language(&["x".into(), "z".into()]),
            CommandLanguageMatch::Invalid
        );
        assert_eq!(
            CommandRegistry::resolve_language(&["h".into()]),
            CommandLanguageMatch::Command(CommandId::PreviousTab)
        );
        assert_eq!(
            CommandRegistry::resolve_language(&["l".into()]),
            CommandLanguageMatch::Command(CommandId::NextTab)
        );

        let bindings = BTreeMap::from([(
            CommandId::ExecuteStatement.as_str().to_owned(),
            "<leader> z s".to_owned(),
        )]);
        assert_eq!(
            CommandRegistry::resolve_language_with(&["x".into(), "s".into()], &bindings),
            CommandLanguageMatch::Invalid
        );
        assert_eq!(
            CommandRegistry::resolve_language_with(&["z".into(), "s".into()], &bindings),
            CommandLanguageMatch::Command(CommandId::ExecuteStatement)
        );
    }
}
