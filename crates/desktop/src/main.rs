use gpui::{prelude::*, px, size, App, Bounds, KeyBinding, WindowBounds, WindowOptions};
use gpui_platform::application;
use sift_ui::{Copy, Cut, Paste, SelectAll};
use sift_workspace_ui::{FeasibilityWorkspace, FocusQueryInput, RefreshProbe, ToggleTheme};

fn main() {
    application().run(|cx: &mut App| {
        cx.bind_keys([
            KeyBinding::new("ctrl-t", ToggleTheme, Some("SiftWorkspace")),
            KeyBinding::new("ctrl-r", RefreshProbe, Some("SiftWorkspace")),
            KeyBinding::new("ctrl-l", FocusQueryInput, Some("SiftWorkspace")),
            KeyBinding::new("ctrl-c", Copy, Some("SiftTextInput")),
            KeyBinding::new("ctrl-x", Cut, Some("SiftTextInput")),
            KeyBinding::new("ctrl-v", Paste, Some("SiftTextInput")),
            KeyBinding::new("ctrl-a", SelectAll, Some("SiftTextInput")),
            KeyBinding::new("cmd-c", Copy, Some("SiftTextInput")),
            KeyBinding::new("cmd-x", Cut, Some("SiftTextInput")),
            KeyBinding::new("cmd-v", Paste, Some("SiftTextInput")),
            KeyBinding::new("cmd-a", SelectAll, Some("SiftTextInput")),
        ]);

        let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("Sift".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| FeasibilityWorkspace::new(window, cx)),
        )
        .expect("failed to open the Sift desktop window");
        cx.activate(true);
    });
}
