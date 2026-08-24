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
use crate::platform::{primary_modifier, shell_key_bindings};
use sift_workspace_ui::{
    editor as ed, results as res, CancelExecution, CloseActiveItem, CloseActivePane, FocusNextPane,
    OpenCommandPalette, OpenServerConnection, SaveActiveItem, SplitPane, ToggleBottomDock,
    ToggleLeftDock, ToggleRightDock,
};

/// Keymap for the SQL editor. Bound under the `SiftEditor` focus context so
/// these never intercept workspace or text-field commands. Character and IME
/// input arrive through the editor's input handler, not these bindings.
fn editor_key_bindings() -> Vec<gpui::KeyBinding> {
    let ctx = Some("SiftEditor");
    let insert_ctx = Some("SiftEditor && vim_mode == insert");
    let standard_ctx = Some("SiftEditor && keymap_profile != vim");
    let primary = primary_modifier();
    vec![
        gpui::KeyBinding::new("backspace", ed::Backspace, insert_ctx),
        gpui::KeyBinding::new("delete", ed::DeleteForward, insert_ctx),
        gpui::KeyBinding::new("enter", ed::Newline, insert_ctx),
        gpui::KeyBinding::new("tab", ed::Indent, insert_ctx),
        gpui::KeyBinding::new("left", ed::MoveLeft, ctx),
        gpui::KeyBinding::new("right", ed::MoveRight, ctx),
        gpui::KeyBinding::new("up", ed::MoveUp, ctx),
        gpui::KeyBinding::new("down", ed::MoveDown, ctx),
        gpui::KeyBinding::new("shift-left", ed::SelectLeft, standard_ctx),
        gpui::KeyBinding::new("shift-right", ed::SelectRight, standard_ctx),
        gpui::KeyBinding::new("shift-up", ed::SelectUp, standard_ctx),
        gpui::KeyBinding::new("shift-down", ed::SelectDown, standard_ctx),
        gpui::KeyBinding::new("home", ed::LineStart, standard_ctx),
        gpui::KeyBinding::new("end", ed::LineEnd, standard_ctx),
        gpui::KeyBinding::new("ctrl-a", ed::SelectAll, standard_ctx),
        gpui::KeyBinding::new(&format!("{primary}-c"), ed::Copy, standard_ctx),
        gpui::KeyBinding::new(&format!("{primary}-x"), ed::Cut, standard_ctx),
        gpui::KeyBinding::new(&format!("{primary}-v"), ed::Paste, standard_ctx),
        gpui::KeyBinding::new(&format!("{primary}-z"), ed::Undo, standard_ctx),
        gpui::KeyBinding::new(&format!("{primary}-shift-z"), ed::Redo, standard_ctx),
        gpui::KeyBinding::new("escape", ed::ExitInsertMode, ctx),
        gpui::KeyBinding::new(
            &format!("{primary}-enter"),
            ed::ExecuteStatement,
            standard_ctx,
        ),
        gpui::KeyBinding::new(
            &format!("{primary}-shift-enter"),
            ed::ExecuteDocument,
            standard_ctx,
        ),
        gpui::KeyBinding::new("ctrl-space", ed::Complete, insert_ctx),
        gpui::KeyBinding::new(
            &format!("{primary}-alt-l"),
            ed::FormatDocument,
            standard_ctx,
        ),
        gpui::KeyBinding::new("alt-enter", ed::ApplyQuickFix, standard_ctx),
        gpui::KeyBinding::new(
            &format!("{primary}-c"),
            res::CopySelectedCell,
            Some("SiftResults"),
        ),
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
                    MenuItem::action("Cancel Query", CancelExecution),
                    MenuItem::separator(),
                    MenuItem::action("Suggest Completions", ed::Complete),
                    MenuItem::action("Format SQL", ed::FormatDocument),
                    MenuItem::action("Apply Quick Fix", ed::ApplyQuickFix),
                    MenuItem::action("Find Usages", ed::FindUsages),
                    MenuItem::action("Go to Next Problem", ed::GoToNextDiagnostic),
                ]),
                Menu::new("View").items([
                    MenuItem::action("Connections Dock", ToggleLeftDock),
                    MenuItem::action("Inspector Dock", ToggleRightDock),
                    MenuItem::action("Results Dock", ToggleBottomDock),
                ]),
            ]);
            let text = Some("SiftTextInput");
            let primary = primary_modifier();
            cx.bind_keys([
                gpui::KeyBinding::new("backspace", Backspace, text),
                gpui::KeyBinding::new("delete", Delete, text),
                gpui::KeyBinding::new("left", Left, text),
                gpui::KeyBinding::new("right", Right, text),
                gpui::KeyBinding::new("home", Home, text),
                gpui::KeyBinding::new("end", End, text),
                gpui::KeyBinding::new(&format!("{primary}-c"), Copy, text),
                gpui::KeyBinding::new(&format!("{primary}-x"), Cut, text),
                gpui::KeyBinding::new(&format!("{primary}-v"), Paste, text),
                gpui::KeyBinding::new("ctrl-a", SelectAll, text),
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
