//! Sift's application-specific GPUI component and theme boundary.

mod assets;
mod components;
mod text_input;
mod theme;

pub use assets::{database_logo, SiftAssets};
pub use components::{
    icon, keybinding_chords, Badge, Button, ButtonTone, ClickHandler, Clickable, Disableable,
    ErrorBanner, Field, IconButton, IconName, KeyBinding, PaneTab, SectionLabel, Toggleable, Tone,
    Tooltip,
};
pub use text_input::{
    Backspace, Backtab, Copy, Cut, Delete, End, Home, Left, Paste, Right, SelectAll, Tab,
    TextInput, TextInputEvent,
};
pub use theme::{
    init_theme, set_theme, ActiveTheme, GlobalTheme, Theme, ThemeColors, ThemeMetrics,
};
