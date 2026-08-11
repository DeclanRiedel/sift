mod app;
mod local_server;
mod platform;

use gpui::{prelude::*, px, Bounds, Menu, MenuItem, WindowBounds, WindowOptions};
use gpui_platform::application;
use sift_ui::{Copy, Cut, Paste, SelectAll};

use crate::app::{display_rects, SiftApp, SiftWindow};
use crate::platform::shell_key_bindings;
use sift_workspace_ui::{
    CloseActiveItem, FocusNextPane, OpenCommandPalette, SaveActiveItem, SplitPane,
    ToggleBottomDock, ToggleLeftDock, ToggleRightDock, ToggleShellTheme,
};

fn main() {
    application().run(|cx| {
        cx.bind_keys(shell_key_bindings());
        cx.set_menus([
            Menu::new("Sift").items([MenuItem::action("Command Palette…", OpenCommandPalette)]),
            Menu::new("Workspace").items([
                MenuItem::action("Split Pane", SplitPane),
                MenuItem::action("Focus Next Pane", FocusNextPane),
                MenuItem::separator(),
                MenuItem::action("Save Item", SaveActiveItem),
                MenuItem::action("Close Item", CloseActiveItem),
            ]),
            Menu::new("View").items([
                MenuItem::action("Connections Dock", ToggleLeftDock),
                MenuItem::action("Inspector Dock", ToggleRightDock),
                MenuItem::action("Results Dock", ToggleBottomDock),
                MenuItem::separator(),
                MenuItem::action("Toggle Theme", ToggleShellTheme),
            ]),
        ]);
        cx.bind_keys([
            gpui::KeyBinding::new("ctrl-c", Copy, Some("SiftTextInput")),
            gpui::KeyBinding::new("ctrl-x", Cut, Some("SiftTextInput")),
            gpui::KeyBinding::new("ctrl-v", Paste, Some("SiftTextInput")),
            gpui::KeyBinding::new("ctrl-a", SelectAll, Some("SiftTextInput")),
            gpui::KeyBinding::new("cmd-c", Copy, Some("SiftTextInput")),
            gpui::KeyBinding::new("cmd-x", Cut, Some("SiftTextInput")),
            gpui::KeyBinding::new("cmd-v", Paste, Some("SiftTextInput")),
            gpui::KeyBinding::new("cmd-a", SelectAll, Some("SiftTextInput")),
        ]);

        let app = SiftApp::new();
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
        let store = app.presentation_store.clone();
        let runtime = app.runtime.clone();
        let local_server = app.local_server.clone();
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
            |window, cx| {
                cx.new(|cx| SiftWindow::new(state, store, runtime, local_server, window, cx))
            },
        )
        .expect("failed to open the Sift desktop window");
        cx.activate(true);
    });
}
