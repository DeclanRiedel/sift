//! Host-owned app-bar menu model.

use super::*;

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
            Item::available(CommandId::ToggleQueryResultsPlacement),
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
            Item::available(CommandId::ViewMetadata),
        ],
    }
}

pub(super) fn render_database_breadcrumb(
    item_id: u64,
    source: DatabaseObjectSource,
    colors: sift_ui::ThemeColors,
    cx: &mut Context<WorkspaceShell>,
) -> gpui::AnyElement {
    let segments = [
        (
            DatabaseBreadcrumbLevel::Connection,
            source.profile_name.clone(),
        ),
        (
            DatabaseBreadcrumbLevel::Catalog,
            source.catalog.clone().unwrap_or_else(|| "default".into()),
        ),
        (DatabaseBreadcrumbLevel::Schema, source.schema.clone()),
        (DatabaseBreadcrumbLevel::Object, source.object.clone()),
    ];
    let mut breadcrumb = div()
        .id(("database-breadcrumb", item_id as usize))
        .debug_selector(|| "database-breadcrumb".into())
        .min_w_0()
        .flex()
        .items_center()
        .overflow_hidden()
        .text_xs();
    for (index, (level, label)) in segments.into_iter().enumerate() {
        if index > 0 {
            breadcrumb = breadcrumb.child(icon(IconName::ChevronRight, colors.disabled_text, 9.));
        }
        let source = source.clone();
        breadcrumb = breadcrumb.child(
            div()
                .id(format!("database-breadcrumb-segment-{item_id}-{index}"))
                .debug_selector(move || {
                    match level {
                        DatabaseBreadcrumbLevel::Connection => "breadcrumb-connection",
                        DatabaseBreadcrumbLevel::Catalog => "breadcrumb-catalog",
                        DatabaseBreadcrumbLevel::Schema => "breadcrumb-schema",
                        DatabaseBreadcrumbLevel::Object => "breadcrumb-object",
                    }
                    .into()
                })
                .min_w_0()
                .max_w(px(130.))
                .px_1()
                .truncate()
                .rounded_sm()
                .text_color(if level == DatabaseBreadcrumbLevel::Object {
                    colors.text
                } else {
                    colors.muted_text
                })
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .role(Role::Button)
                .aria_label(format!("Reveal {label} in connections"))
                .hover(|segment| segment.bg(colors.hovered_surface).text_color(colors.text))
                .on_click(cx.listener(move |shell, _, window, cx| {
                    shell.reveal_database_object(&source, level, window, cx);
                }))
                .child(label),
        );
    }
    breadcrumb.into_any_element()
}
