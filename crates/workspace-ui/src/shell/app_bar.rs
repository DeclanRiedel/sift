//! Host-owned app-bar menu model.

use super::{CommandId, CommandRegistry};

pub(super) const DEV_WIKI_URL: &str = "http://127.0.0.1:8787";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AppBarMenu {
    Main,
    File,
    Edit,
    Selection,
    View,
    Go,
    Run,
    Terminal,
    Help,
    Profile,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct AppBarMenuItem {
    pub label: &'static str,
    pub shortcut: &'static str,
    pub command: Option<CommandId>,
    pub url: Option<&'static str>,
}

impl AppBarMenuItem {
    fn available(command: CommandId) -> Self {
        let definition = CommandRegistry::definition(command);
        Self {
            label: definition.label,
            // The desktop supports Vim interaction only. The conventional
            // shortcut is retained for command metadata, but advertising it
            // here is misleading when that key context is deliberately off.
            shortcut: definition.language,
            command: Some(definition.id),
            url: None,
        }
    }

    fn wiki() -> Self {
        Self {
            label: "Wiki",
            shortcut: "",
            command: None,
            url: cfg!(debug_assertions).then_some(DEV_WIKI_URL),
        }
    }

    fn license() -> Self {
        Self {
            label: "License",
            shortcut: "AGPL-3.0-only",
            command: None,
            url: Some("https://github.com/declan/sift/blob/master/LICENSE"),
        }
    }
}

pub(super) fn menu_items(menu: AppBarMenu) -> Vec<AppBarMenuItem> {
    use AppBarMenuItem as Item;
    match menu {
        AppBarMenu::Main => vec![
            Item::available(CommandId::OpenCommandPalette),
            Item::available(CommandId::OpenSettings),
            Item::available(CommandId::Quit),
        ],
        AppBarMenu::File => vec![
            Item::available(CommandId::NewQuery),
            Item::available(CommandId::OpenSavedQuery),
            Item::available(CommandId::RenameQuery),
            Item::available(CommandId::SaveItem),
            Item::available(CommandId::CloseItem),
            Item::available(CommandId::ClosePane),
        ],
        AppBarMenu::Edit => vec![
            Item::available(CommandId::UndoQuery),
            Item::available(CommandId::RedoQuery),
            Item::available(CommandId::Cut),
            Item::available(CommandId::Copy),
            Item::available(CommandId::Paste),
        ],
        AppBarMenu::Selection => vec![Item::available(CommandId::SelectAll)],
        AppBarMenu::View => vec![
            Item::available(CommandId::ToggleLeftDock),
            Item::available(CommandId::ToggleInspectorDock),
            Item::available(CommandId::ToggleBottomDock),
            Item::available(CommandId::ToggleTheme),
        ],
        AppBarMenu::Go => vec![
            Item::available(CommandId::PreviousTab),
            Item::available(CommandId::NextTab),
            Item::available(CommandId::FocusNextPane),
            Item::available(CommandId::FocusPaneLeft),
            Item::available(CommandId::FocusPaneDown),
            Item::available(CommandId::FocusPaneUp),
            Item::available(CommandId::FocusPaneRight),
            Item::available(CommandId::FocusConnections),
            Item::available(CommandId::FocusEditor),
            Item::available(CommandId::FocusResults),
            Item::available(CommandId::FocusInspector),
        ],
        AppBarMenu::Run => vec![
            Item::available(CommandId::ExecuteStatement),
            Item::available(CommandId::ExecuteDocument),
            Item::available(CommandId::CancelExecution),
            Item::available(CommandId::BeginTransaction),
            Item::available(CommandId::CommitTransaction),
            Item::available(CommandId::RollbackTransaction),
        ],
        AppBarMenu::Terminal => vec![
            Item::available(CommandId::ToggleBottomDock),
            Item::available(CommandId::FocusProblems),
        ],
        AppBarMenu::Help => vec![
            Item::wiki(),
            Item::license(),
            Item::available(CommandId::OpenCommandPalette),
            Item::available(CommandId::OpenKeymaps),
            Item::available(CommandId::OpenSettings),
        ],
        AppBarMenu::Profile => vec![
            Item::available(CommandId::OpenSettings),
            Item::available(CommandId::OpenKeymaps),
            Item::available(CommandId::ToggleTheme),
            Item::available(CommandId::OpenServerConfiguration),
        ],
    }
}
