//! Host-owned app-bar menu model.

use super::{CommandId, CommandRegistry};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AppBarMenu {
    Main,
    File,
    Edit,
    Selection,
    View,
    Go,
    Run,
    Window,
    Help,
    Profile,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct AppBarMenuItem {
    pub label: &'static str,
    pub shortcut: &'static str,
    pub command: Option<CommandId>,
}

impl AppBarMenuItem {
    fn available(command: CommandId) -> Self {
        let definition = CommandRegistry::definition(command);
        Self {
            label: definition.label,
            shortcut: definition.shortcut,
            command: Some(definition.id),
        }
    }

    const fn unimplemented(label: &'static str) -> Self {
        Self {
            label,
            shortcut: "",
            command: None,
        }
    }
}

pub(super) fn menu_items(menu: AppBarMenu) -> Vec<AppBarMenuItem> {
    use AppBarMenuItem as Item;
    match menu {
        AppBarMenu::Main => vec![
            Item::unimplemented("About Sift"),
            Item::unimplemented("Check for Updates…"),
            Item::available(CommandId::Quit),
        ],
        AppBarMenu::File => vec![
            Item::unimplemented("New Query"),
            Item::unimplemented("Open…"),
            Item::available(CommandId::SaveItem),
            Item::available(CommandId::CloseItem),
        ],
        AppBarMenu::Edit => vec![
            Item::available(CommandId::UndoQuery),
            Item::available(CommandId::RedoQuery),
            Item::unimplemented("Cut"),
            Item::unimplemented("Copy"),
            Item::unimplemented("Paste"),
        ],
        AppBarMenu::Selection => vec![
            Item::unimplemented("Select All"),
            Item::unimplemented("Expand Selection"),
            Item::unimplemented("Shrink Selection"),
            Item::unimplemented("Add Cursor Above"),
            Item::unimplemented("Add Cursor Below"),
        ],
        AppBarMenu::View => vec![
            Item::available(CommandId::ToggleLeftDock),
            Item::available(CommandId::ToggleInspectorDock),
            Item::available(CommandId::ToggleBottomDock),
            Item::unimplemented("Appearance"),
            Item::unimplemented("Full Screen"),
        ],
        AppBarMenu::Go => vec![
            Item::available(CommandId::FocusNextPane),
            Item::unimplemented("Go to Query"),
            Item::unimplemented("Go to Symbol"),
            Item::unimplemented("Back"),
            Item::unimplemented("Forward"),
        ],
        AppBarMenu::Run => vec![
            Item::available(CommandId::ExecuteStatement),
            Item::available(CommandId::ExecuteDocument),
            Item::unimplemented("Run Configuration…"),
            Item::unimplemented("Stop"),
        ],
        AppBarMenu::Window => vec![
            Item::available(CommandId::SplitPane),
            Item::available(CommandId::ClosePane),
            Item::unimplemented("New Window"),
            Item::unimplemented("Previous Window"),
            Item::unimplemented("Next Window"),
        ],
        AppBarMenu::Help => vec![
            Item::available(CommandId::OpenCommandPalette),
            Item::unimplemented("Sift Documentation"),
            Item::unimplemented("Keyboard Shortcuts"),
            Item::unimplemented("Report Issue"),
            Item::unimplemented("About Sift"),
        ],
        AppBarMenu::Profile => vec![
            Item::available(CommandId::OpenSettings),
            Item::unimplemented("Keymaps"),
            Item::unimplemented("Themes"),
            Item::unimplemented("Server Configuration"),
        ],
    }
}
