use gpui::{
    div, prelude::*, px, svg, AnyElement, App, Context, CursorStyle, Div, ElementId, FocusHandle,
    Hsla, IntoElement, MouseButton, RenderOnce, Role, SharedString, Stateful, Window,
};

use crate::ActiveTheme;

/// Shared handler shape for clickable controls; matches gpui's
/// `InteractiveElement::on_click`.
pub type ClickHandler = Box<dyn Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static>;

/// Capability shared by every clickable Sift control. Mirrors gpui's
/// `InteractiveElement::on_click` signature so `cx.listener` closures plug in
/// directly.
pub trait Clickable: Sized {
    fn on_click(self, handler: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static)
        -> Self;
}

/// Capability for controls with an on/off visual state.
pub trait Toggleable: Sized {
    fn toggle_state(self, selected: bool) -> Self;
}

/// Capability for controls that can stop accepting input.
pub trait Disableable: Sized {
    fn disabled(self, disabled: bool) -> Self;
}

/// Shared tab chrome used by pane tab bars and their drag proxies. Feature
/// views supply content/actions; this component owns geometry and state colors.
#[derive(IntoElement)]
pub struct PaneTab {
    div: Stateful<Div>,
    selected: bool,
    dirty: bool,
    staged: bool,
    children: Vec<AnyElement>,
}

impl PaneTab {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            div: div().id(id),
            selected: false,
            dirty: false,
            staged: false,
            children: Vec::new(),
        }
    }

    pub fn debug_selector(mut self, selector: impl Fn() -> String + 'static) -> Self {
        self.div = self.div.debug_selector(selector);
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn dirty(mut self, dirty: bool) -> Self {
        self.dirty = dirty;
        self
    }

    pub fn staged(mut self, staged: bool) -> Self {
        self.staged = staged;
        self
    }
}

impl gpui::InteractiveElement for PaneTab {
    fn interactivity(&mut self) -> &mut gpui::Interactivity {
        self.div.interactivity()
    }
}

impl gpui::StatefulInteractiveElement for PaneTab {}

impl ParentElement for PaneTab {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for PaneTab {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors;
        self.div
            .relative()
            .flex_none()
            .flex()
            .items_center()
            .h(cx.theme().metrics.tab_height)
            .min_w(px(110.))
            .max_w(px(240.))
            .border_r_1()
            .border_color(colors.subtle_border)
            .bg(if self.selected {
                colors.background
            } else {
                colors.toolbar
            })
            .text_color(if self.selected {
                colors.text
            } else {
                colors.muted_text
            })
            .children(self.dirty.then(|| {
                div()
                    .ml_2()
                    .size(px(6.))
                    .flex_none()
                    .rounded_full()
                    .bg(colors.accent)
            }))
            .children(self.staged.then(|| {
                div()
                    .debug_selector(|| "tab-staged-changes".into())
                    .ml_2()
                    .size(px(7.))
                    .flex_none()
                    .rounded_full()
                    .border_1()
                    .border_color(colors.staged)
            }))
            .children(self.children)
    }
}

/// Semantic intent for a labeled control. Feature views select intent; the
/// theme remains responsible for concrete colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ButtonTone {
    /// Bordered quiet action (Cancel, Back, secondary submits).
    #[default]
    Neutral,
    /// Filled primary action.
    Accent,
    /// Filled confirmation action.
    Success,
    /// Filled destructive action.
    Danger,
    /// Quiet destructive action that fills with danger on hover.
    DangerMuted,
    /// Borderless quiet action tinted on hover.
    Ghost,
    /// Borderless destructive action tinted on hover.
    DangerGhost,
}

/// Small monochrome marks used throughout Sift's native chrome. They are
/// vendored from Qlementine Icons. Callers must choose `Fallback` when the
/// upstream set has no semantically appropriate mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconName {
    Activity,
    Add,
    Automations,
    Check,
    ChevronDown,
    ChevronLeft,
    ChevronRight,
    Close,
    CloseRightPane,
    Copy,
    Database,
    Edit,
    Fallback,
    Folder,
    Function,
    Github,
    Info,
    Keyboard,
    Maximize,
    Menu,
    Minimize,
    Outline,
    Play,
    Refresh,
    Search,
    Sequence,
    Server,
    Table,
    Terminal,
    User,
    Users,
    VersionControl,
    View,
    Warning,
    Workspace,
}

impl IconName {
    pub const ALL: [Self; 35] = [
        Self::Activity,
        Self::Add,
        Self::Automations,
        Self::Check,
        Self::ChevronDown,
        Self::ChevronLeft,
        Self::ChevronRight,
        Self::Close,
        Self::CloseRightPane,
        Self::Copy,
        Self::Database,
        Self::Edit,
        Self::Fallback,
        Self::Folder,
        Self::Function,
        Self::Github,
        Self::Info,
        Self::Keyboard,
        Self::Maximize,
        Self::Menu,
        Self::Minimize,
        Self::Outline,
        Self::Play,
        Self::Refresh,
        Self::Search,
        Self::Sequence,
        Self::Server,
        Self::Table,
        Self::Terminal,
        Self::User,
        Self::Users,
        Self::VersionControl,
        Self::View,
        Self::Warning,
        Self::Workspace,
    ];

    pub const fn path(self) -> &'static str {
        match self {
            Self::Activity => "icons/activity.svg",
            Self::Add => "icons/add.svg",
            Self::Automations => "icons/automations.svg",
            Self::Check => "icons/check.svg",
            Self::ChevronDown => "icons/chevron-down.svg",
            Self::ChevronLeft => "icons/chevron-left.svg",
            Self::ChevronRight => "icons/chevron-right.svg",
            Self::Close => "icons/close.svg",
            Self::CloseRightPane => "icons/close-right-pane.svg",
            Self::Copy => "icons/copy.svg",
            Self::Database => "icons/database.svg",
            Self::Edit => "icons/edit.svg",
            Self::Fallback => "icons/fallback.svg",
            Self::Folder => "icons/folder.svg",
            Self::Function => "icons/function.svg",
            Self::Github => "icons/github.svg",
            Self::Info => "icons/info.svg",
            Self::Keyboard => "icons/keyboard.svg",
            Self::Maximize => "icons/maximize.svg",
            Self::Menu => "icons/menu.svg",
            Self::Minimize => "icons/minimize.svg",
            Self::Outline => "icons/outline.svg",
            Self::Play => "icons/play.svg",
            Self::Refresh => "icons/refresh.svg",
            Self::Search => "icons/search.svg",
            Self::Sequence => "icons/sequence.svg",
            Self::Server => "icons/server.svg",
            Self::Table => "icons/table.svg",
            Self::Terminal => "icons/terminal.svg",
            Self::User => "icons/user.svg",
            Self::Users => "icons/users.svg",
            Self::VersionControl => "icons/version-control.svg",
            Self::View => "icons/view.svg",
            Self::Warning => "icons/warning.svg",
            Self::Workspace => "icons/workspace.svg",
        }
    }
}

pub fn icon(name: IconName, color: Hsla, size: f32) -> AnyElement {
    svg()
        .path(name.path())
        .size(px(size))
        .text_color(color)
        .flex_none()
        .into_any_element()
}

/// The one tooltip surface in the product. Views attach it through gpui's
/// `.tooltip()`; the active theme supplies colors at render time.
pub struct Tooltip {
    message: SharedString,
}

impl Tooltip {
    pub fn new(message: impl Into<SharedString>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl gpui::Render for Tooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;
        div()
            .max_w(px(360.))
            .px_2()
            .py_1()
            .rounded_sm()
            .border_1()
            .border_color(colors.strong_border)
            .bg(colors.elevated_surface)
            .shadow_md()
            .text_xs()
            .text_color(colors.text)
            .whitespace_normal()
            .child(self.message.clone())
    }
}

/// Labeled action rendered from theme tokens. Handlers stay with the owning
/// entity via `cx.listener`, so child controls never reach across the
/// ownership tree.
#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    label: SharedString,
    tone: ButtonTone,
    wide: bool,
    full_width: bool,
    disabled: bool,
    loading: bool,
    start_icon: Option<IconName>,
    key_binding: Option<SharedString>,
    debug_selector: Option<SharedString>,
    on_click: Option<ClickHandler>,
}

impl Button {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            tone: ButtonTone::default(),
            wide: false,
            full_width: false,
            disabled: false,
            loading: false,
            start_icon: None,
            key_binding: None,
            debug_selector: None,
            on_click: None,
        }
    }

    pub fn tone(mut self, tone: ButtonTone) -> Self {
        self.tone = tone;
        self
    }

    /// Primary call-to-action padding.
    pub fn wide(mut self, wide: bool) -> Self {
        self.wide = wide;
        self
    }

    pub fn full_width(mut self) -> Self {
        self.full_width = true;
        self
    }

    pub fn start_icon(mut self, name: impl Into<Option<IconName>>) -> Self {
        self.start_icon = name.into();
        self
    }

    /// Display the shortcut that triggers this action next to the label.
    pub fn key_binding(mut self, shortcut: impl Into<SharedString>) -> Self {
        self.key_binding = Some(shortcut.into());
        self
    }

    /// Stable selector exposed to UI tests through `debug_bounds`.
    pub fn debug_selector(mut self, selector: impl Into<SharedString>) -> Self {
        self.debug_selector = Some(selector.into());
        self
    }

    /// Swap to the muted pending surface and ignore clicks while an async
    /// action is in flight.
    pub fn loading(mut self, loading: bool) -> Self {
        self.loading = loading;
        self
    }

    fn fill_colors(tone: ButtonTone, colors: crate::ThemeColors) -> (Hsla, Hsla, Option<Hsla>) {
        // (background, foreground, hover background)
        match tone {
            ButtonTone::Neutral => (colors.surface, colors.text, Some(colors.hovered_surface)),
            ButtonTone::Accent => (colors.accent, colors.on_accent, Some(colors.accent_hover)),
            ButtonTone::Success => (colors.success, colors.on_accent, Some(colors.success_muted)),
            ButtonTone::Danger => (colors.danger, colors.on_accent, Some(colors.danger_muted)),
            ButtonTone::DangerMuted => (colors.danger_muted, colors.danger, Some(colors.danger)),
            ButtonTone::Ghost => (
                gpui::transparent_black(),
                colors.text,
                Some(colors.hovered_surface),
            ),
            ButtonTone::DangerGhost => (
                gpui::transparent_black(),
                colors.danger,
                Some(colors.danger_muted),
            ),
        }
    }
}

impl Clickable for Button {
    fn on_click(
        mut self,
        handler: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl Disableable for Button {
    fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

impl RenderOnce for Button {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let interactive = !self.disabled && !self.loading;
        let (background, foreground, hover_background) = Self::fill_colors(self.tone, colors);
        let (background, foreground) = if self.loading {
            (colors.hovered_surface, colors.muted_text)
        } else if self.disabled {
            (gpui::transparent_black(), colors.disabled_text)
        } else {
            (background, foreground)
        };
        let bordered = matches!(self.tone, ButtonTone::Neutral) && !self.disabled;
        let debug_selector = self.debug_selector.clone();
        let mut button = div()
            .id(self.id.clone())
            .when_some(debug_selector, |el, selector| {
                el.debug_selector(move || selector.to_string())
            })
            .role(Role::Button)
            .aria_label(self.label.clone())
            .h(theme.metrics.control_height)
            .when(self.wide, |el| el.px_3())
            .when(!self.wide, |el| el.px_2())
            .when(self.full_width, |el| el.w_full())
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .gap_1()
            .rounded(theme.metrics.radius)
            .text_color(foreground)
            .when(bordered, |el| {
                el.border_1()
                    .border_color(colors.subtle_border)
                    .bg(background)
            })
            .when(!bordered, |el| el.bg(background))
            .when_some(self.start_icon, |el, name| {
                el.child(icon(name, foreground, 13.))
            })
            .child(div().min_w_0().truncate().child(self.label.clone()))
            .children(self.key_binding.map(KeyBinding::new));
        if interactive {
            if let Some(on_click) = self.on_click {
                button = button.on_click(on_click);
            }
            if let Some(hover) = hover_background {
                button = button.hover(move |el| el.bg(hover));
            }
        }
        button
    }
}

/// Icon-only action. The label is announced to assistive technology and shown
/// as a tooltip; it is never rendered inline.
#[derive(IntoElement)]
pub struct IconButton {
    id: ElementId,
    name: IconName,
    label: SharedString,
    selected: bool,
    danger: bool,
    disabled: bool,
    square: gpui::Pixels,
    icon_size: f32,
    badge: Option<usize>,
    text: Option<SharedString>,
    tooltip: Option<SharedString>,
    on_click: Option<ClickHandler>,
}

impl IconButton {
    pub fn new(id: impl Into<ElementId>, name: IconName, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            name,
            label: label.into(),
            selected: false,
            danger: false,
            disabled: false,
            square: px(0.),
            icon_size: 14.,
            badge: None,
            text: None,
            tooltip: None,
            on_click: None,
        }
    }

    /// Optional inline text rendered after the icon.
    pub fn text(mut self, text: impl Into<SharedString>) -> Self {
        self.text = Some(text.into());
        self
    }

    /// Tint the icon and hover surface with the danger color.
    pub fn danger(mut self, danger: bool) -> Self {
        self.danger = danger;
        self
    }

    /// Override the square hit target instead of the theme's compact height.
    pub fn square(mut self, square: gpui::Pixels) -> Self {
        self.square = square;
        self
    }

    pub fn icon_size(mut self, size: f32) -> Self {
        self.icon_size = size;
        self
    }

    /// Small count bubble rendered next to the icon.
    pub fn badge(mut self, badge: impl Into<Option<usize>>) -> Self {
        self.badge = badge.into();
        self
    }

    pub fn tooltip(mut self, message: impl Into<SharedString>) -> Self {
        self.tooltip = Some(message.into());
        self
    }
}

impl Clickable for IconButton {
    fn on_click(
        mut self,
        handler: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl Toggleable for IconButton {
    fn toggle_state(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
}

impl Disableable for IconButton {
    fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

impl RenderOnce for IconButton {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let interactive = !self.disabled;
        let rest_foreground = if self.danger {
            colors.danger
        } else if self.selected {
            colors.text
        } else {
            colors.muted_text
        };
        let hover_background = if self.danger {
            colors.danger_muted
        } else {
            colors.hovered_surface
        };
        let mut button = div()
            .id(self.id.clone())
            .role(Role::Button)
            .aria_label(self.label.clone())
            .when(self.square > px(0.), |el| el.size(self.square))
            .when(self.square <= px(0.), |el| {
                el.h(theme.metrics.compact_control_height)
                    .min_w(theme.metrics.compact_control_height)
            })
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .gap_1()
            .px_1()
            .rounded_sm()
            .text_color(rest_foreground)
            .when(self.selected, |el| el.bg(colors.active_surface))
            .child(icon(self.name, rest_foreground, self.icon_size))
            .when_some(self.text, |el, text| {
                el.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(rest_foreground)
                        .child(text),
                )
            })
            .children(self.badge.filter(|count| *count > 0).map(|count| {
                div()
                    .min_w(px(12.))
                    .h(px(12.))
                    .px(px(3.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(if self.danger {
                        colors.danger_muted
                    } else {
                        colors.active_surface
                    })
                    .text_color(rest_foreground)
                    .text_size(px(9.))
                    .child(count.to_string())
            }))
            .when_some(self.tooltip.filter(|_| interactive), |el, message| {
                el.tooltip(move |_, cx| cx.new(|_| Tooltip::new(message.clone())).into())
            });
        if interactive {
            if let Some(on_click) = self.on_click {
                button = button.on_click(on_click);
            }
            button = button.hover(move |el| el.bg(hover_background).text_color(colors.text));
        }
        button
    }
}

/// Intent carried by a [`Badge`] and error surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tone {
    #[default]
    Neutral,
    Accent,
    Danger,
    Warning,
    Success,
}

/// Compact count/status pill.
#[derive(IntoElement)]
pub struct Badge {
    label: SharedString,
    tone: Tone,
}

impl Badge {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            tone: Tone::default(),
        }
    }

    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }
}

impl RenderOnce for Badge {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors;
        let (background, foreground) = match self.tone {
            Tone::Neutral => (colors.hovered_surface, colors.muted_text),
            Tone::Accent => (colors.accent_muted, colors.accent_hover),
            Tone::Danger => (colors.danger_muted, colors.danger),
            Tone::Warning => (colors.warning_muted, colors.warning),
            Tone::Success => (colors.success_muted, colors.success),
        };
        div()
            .h(px(18.))
            .px_1()
            .flex()
            .flex_none()
            .items_center()
            .rounded(cx.theme().metrics.radius)
            .bg(background)
            .text_xs()
            .text_color(foreground)
            .child(self.label)
    }
}

/// Uppercase section heading used above groups of rows.
#[derive(IntoElement)]
pub struct SectionLabel {
    label: SharedString,
}

impl SectionLabel {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
        }
    }
}

impl RenderOnce for SectionLabel {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        div()
            .h(cx.theme().metrics.row_height)
            .flex()
            .items_center()
            .text_xs()
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(cx.theme().colors.muted_text)
            .child(self.label)
    }
}

/// Labeled input frame: one field surface shared by every form in the shell.
/// Clicking the frame focuses the wrapped input without letting the click
/// escape to surrounding surfaces.
#[derive(IntoElement)]
pub struct Field {
    label: SharedString,
    focus_handle: Option<FocusHandle>,
    child: AnyElement,
}

impl Field {
    pub fn new(
        label: impl Into<SharedString>,
        focus_handle: Option<FocusHandle>,
        child: impl IntoElement,
    ) -> Self {
        Self {
            label: label.into(),
            focus_handle,
            child: child.into_any_element(),
        }
    }
}

impl RenderOnce for Field {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors;
        let debug_label = self.label.to_string();
        let focus_handle = self.focus_handle.clone();
        div()
            .debug_selector(move || debug_label.clone())
            .flex()
            .flex_col()
            .min_w_0()
            .gap_1()
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(colors.muted_text)
                    .child(self.label.clone()),
            )
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .border_1()
                    .border_color(colors.subtle_border)
                    .rounded_sm()
                    .bg(colors.background)
                    .cursor(CursorStyle::IBeam)
                    .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                        cx.stop_propagation();
                        if let Some(focus_handle) = focus_handle.as_ref() {
                            focus_handle.focus(window, cx);
                        }
                    })
                    .child(self.child),
            )
    }
}

/// Shared inline error/warning surface for modals and panels.
#[derive(IntoElement)]
pub struct ErrorBanner {
    message: SharedString,
    tone: Tone,
}

impl ErrorBanner {
    pub fn new(message: impl Into<SharedString>) -> Self {
        Self {
            message: message.into(),
            tone: Tone::Danger,
        }
    }

    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }
}

impl RenderOnce for ErrorBanner {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors;
        let (border, background, foreground) = match self.tone {
            Tone::Warning => (colors.warning, colors.warning_muted, colors.warning),
            _ => (colors.danger, colors.danger_muted, colors.danger),
        };
        div()
            .min_w_0()
            .p_2()
            .flex()
            .items_start()
            .gap_2()
            .rounded_sm()
            .border_1()
            .border_color(border)
            .bg(background)
            .text_color(foreground)
            .child(icon(IconName::Warning, foreground, 14.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .whitespace_normal()
                    .child(self.message),
            )
    }
}

/// Split a shortcut display string into chord groups of key chips, e.g.
/// `"Ctrl+K Ctrl+→"` becomes `[[Ctrl, K], [Ctrl, →]]`.
pub fn keybinding_chords(shortcut: &str) -> Vec<Vec<SharedString>> {
    shortcut
        .split_whitespace()
        .map(|chord| {
            chord
                .split('+')
                .filter(|key| !key.is_empty())
                .map(SharedString::from)
                .collect::<Vec<_>>()
        })
        .filter(|chord| !chord.is_empty())
        .collect()
}

/// Keyboard shortcut display: one bordered chip per key, grouped per chord.
#[derive(IntoElement)]
pub struct KeyBinding {
    shortcut: SharedString,
}

impl KeyBinding {
    pub fn new(shortcut: impl Into<SharedString>) -> Self {
        Self {
            shortcut: shortcut.into(),
        }
    }
}

impl RenderOnce for KeyBinding {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors;
        div().flex().flex_none().items_center().gap_0p5().children(
            keybinding_chords(&self.shortcut).into_iter().map(|chord| {
                div()
                    .flex()
                    .items_center()
                    .gap_0p5()
                    .children(chord.into_iter().map(|key| {
                        div()
                            .h(px(18.))
                            .min_w(px(18.))
                            .px_1()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(cx.theme().metrics.radius)
                            .border_1()
                            .border_color(colors.subtle_border)
                            .bg(colors.surface)
                            .text_xs()
                            .text_color(colors.muted_text)
                            .child(key)
                    }))
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Theme;

    #[test]
    fn keybindings_split_into_chords_of_keys() {
        assert_eq!(
            keybinding_chords("Ctrl+Shift+P")
                .into_iter()
                .flatten()
                .collect::<Vec<_>>(),
            vec!["Ctrl", "Shift", "P"]
        );
        let chords = keybinding_chords("Ctrl+K Ctrl+→");
        assert_eq!(chords.len(), 2);
        assert_eq!(
            chords[1].last().map(|key| key.to_string()),
            Some("→".into())
        );
        assert!(keybinding_chords("").is_empty());
    }

    #[test]
    fn tones_map_to_distinct_fills() {
        let colors = Theme::dark().colors;
        let accent = Button::fill_colors(ButtonTone::Accent, colors);
        let danger = Button::fill_colors(ButtonTone::Danger, colors);
        let ghost = Button::fill_colors(ButtonTone::Ghost, colors);
        assert_eq!(accent.0, colors.accent);
        assert_eq!(accent.1, colors.on_accent);
        assert_eq!(danger.0, colors.danger);
        assert_ne!(accent.0, danger.0);
        assert_ne!(accent.0, ghost.0);
        assert_eq!(IconName::Play.path(), "icons/play.svg");
        assert_eq!(IconName::Fallback.path(), "icons/fallback.svg");
    }
}
