mod app;
mod config;
mod instances;
mod local_server;
mod platform;

use gpui::{prelude::*, px, Bounds, Menu, MenuItem, WindowBounds, WindowOptions};
use gpui_platform::application;
use sift_ui::{
    Backspace, Backtab, Copy, Cut, Delete, End, Home, Left, Paste, Right, SelectAll, Tab,
};

use crate::app::{display_rects, SiftApp, SiftWindow};
use crate::config::DesktopConfig;
use crate::platform::shell_key_bindings;
use sift_workspace_ui::{
    editor as ed, results as res, CloseActiveItem, CloseActivePane, FocusNextPane,
    OpenCommandPalette, OpenServerConnection, SaveActiveItem, SplitPane, ToggleBottomDock,
    ToggleLeftDock, ToggleRightDock,
};

/// Keymap for the SQL editor. Bound under the `SiftEditor` focus context so
/// these never intercept workspace or text-field commands. Character and IME
/// input arrive through the editor's input handler, not these bindings.
fn editor_key_bindings() -> Vec<gpui::KeyBinding> {
    let ctx = Some("SiftEditor");
    vec![
        gpui::KeyBinding::new("backspace", ed::Backspace, ctx),
        gpui::KeyBinding::new("delete", ed::DeleteForward, ctx),
        gpui::KeyBinding::new("enter", ed::Newline, ctx),
        gpui::KeyBinding::new("tab", ed::Indent, ctx),
        gpui::KeyBinding::new("left", ed::MoveLeft, ctx),
        gpui::KeyBinding::new("right", ed::MoveRight, ctx),
        gpui::KeyBinding::new("up", ed::MoveUp, ctx),
        gpui::KeyBinding::new("down", ed::MoveDown, ctx),
        gpui::KeyBinding::new("shift-left", ed::SelectLeft, ctx),
        gpui::KeyBinding::new("shift-right", ed::SelectRight, ctx),
        gpui::KeyBinding::new("shift-up", ed::SelectUp, ctx),
        gpui::KeyBinding::new("shift-down", ed::SelectDown, ctx),
        gpui::KeyBinding::new("home", ed::LineStart, ctx),
        gpui::KeyBinding::new("end", ed::LineEnd, ctx),
        gpui::KeyBinding::new("ctrl-a", ed::SelectAll, ctx),
        gpui::KeyBinding::new("cmd-a", ed::SelectAll, ctx),
        gpui::KeyBinding::new("ctrl-c", ed::Copy, ctx),
        gpui::KeyBinding::new("cmd-c", ed::Copy, ctx),
        gpui::KeyBinding::new("ctrl-x", ed::Cut, ctx),
        gpui::KeyBinding::new("cmd-x", ed::Cut, ctx),
        gpui::KeyBinding::new("ctrl-v", ed::Paste, ctx),
        gpui::KeyBinding::new("cmd-v", ed::Paste, ctx),
        gpui::KeyBinding::new("ctrl-z", ed::Undo, ctx),
        gpui::KeyBinding::new("cmd-z", ed::Undo, ctx),
        gpui::KeyBinding::new("ctrl-shift-z", ed::Redo, ctx),
        gpui::KeyBinding::new("cmd-shift-z", ed::Redo, ctx),
        gpui::KeyBinding::new("escape", ed::ExitInsertMode, ctx),
        gpui::KeyBinding::new("ctrl-enter", ed::ExecuteStatement, ctx),
        gpui::KeyBinding::new("cmd-enter", ed::ExecuteStatement, ctx),
        gpui::KeyBinding::new("ctrl-shift-enter", ed::ExecuteDocument, ctx),
        gpui::KeyBinding::new("cmd-shift-enter", ed::ExecuteDocument, ctx),
        gpui::KeyBinding::new("ctrl-c", res::CopySelectedCell, Some("SiftResults")),
        gpui::KeyBinding::new("cmd-c", res::CopySelectedCell, Some("SiftResults")),
        gpui::KeyBinding::new("left", res::MoveCellLeft, Some("SiftResults")),
        gpui::KeyBinding::new("right", res::MoveCellRight, Some("SiftResults")),
        gpui::KeyBinding::new("up", res::MoveCellUp, Some("SiftResults")),
        gpui::KeyBinding::new("down", res::MoveCellDown, Some("SiftResults")),
    ]
}

fn main() {
    let config = DesktopConfig::load().unwrap_or_else(|error| {
        eprintln!("sift-desktop: {error}");
        std::process::exit(2);
    });
    application()
        .with_assets(sift_ui::SiftAssets)
        .run(move |cx| {
            cx.bind_keys(shell_key_bindings());
            cx.set_menus([
                Menu::new("Sift").items([
                    MenuItem::action("Connect to Server…", OpenServerConnection),
                    MenuItem::separator(),
                    MenuItem::action("Command Palette…", OpenCommandPalette),
                ]),
                Menu::new("Workspace").items([
                    MenuItem::action("Split Pane", SplitPane),
                    MenuItem::action("Focus Next Pane", FocusNextPane),
                    MenuItem::action("Close Pane", CloseActivePane),
                    MenuItem::separator(),
                    MenuItem::action("Save Item", SaveActiveItem),
                    MenuItem::action("Close Item", CloseActiveItem),
                ]),
                Menu::new("Query").items([
                    MenuItem::action("Run Statement", ed::ExecuteStatement),
                    MenuItem::action("Run Document", ed::ExecuteDocument),
                ]),
                Menu::new("View").items([
                    MenuItem::action("Connections Dock", ToggleLeftDock),
                    MenuItem::action("Inspector Dock", ToggleRightDock),
                    MenuItem::action("Results Dock", ToggleBottomDock),
                ]),
            ]);
            let text = Some("SiftTextInput");
            cx.bind_keys([
                gpui::KeyBinding::new("backspace", Backspace, text),
                gpui::KeyBinding::new("delete", Delete, text),
                gpui::KeyBinding::new("left", Left, text),
                gpui::KeyBinding::new("right", Right, text),
                gpui::KeyBinding::new("home", Home, text),
                gpui::KeyBinding::new("end", End, text),
                gpui::KeyBinding::new("ctrl-c", Copy, text),
                gpui::KeyBinding::new("ctrl-x", Cut, text),
                gpui::KeyBinding::new("ctrl-v", Paste, text),
                gpui::KeyBinding::new("ctrl-a", SelectAll, text),
                gpui::KeyBinding::new("cmd-c", Copy, text),
                gpui::KeyBinding::new("cmd-x", Cut, text),
                gpui::KeyBinding::new("cmd-v", Paste, text),
                gpui::KeyBinding::new("cmd-a", SelectAll, text),
                gpui::KeyBinding::new("tab", Tab, text),
                gpui::KeyBinding::new("shift-tab", Backtab, text),
            ]);
            cx.bind_keys(editor_key_bindings());

            let app = SiftApp::new(config.clone());
            let state = app.restore(&display_rects(cx));
            let saved = state.window.bounds;
            let bounds = Bounds {
                origin: gpui::point(px(saved.x), px(saved.y)),
                size: gpui::size(px(saved.width), px(saved.height)),
            };
            let window_bounds = if state.window.maximized {
                WindowBounds::Maximized(bounds)
            } else {
                WindowBounds::Windowed(bounds)
            };
            let services = app.window_services(&state);
            let platform = format!("{:?}", app.platform);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(window_bounds),
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some(format!("Sift · {platform}").into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                |window, cx| cx.new(|cx| SiftWindow::new(state, services, window, cx)),
            )
            .expect("failed to open the Sift desktop window");
            cx.activate(true);
        });
}
