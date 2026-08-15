use gpui::{div, prelude::*, px, svg, AnyElement, ElementId, Hsla, Role, SharedString};

use crate::Theme;

/// Semantic intent for an interactive control. Feature views select intent;
/// the theme remains responsible for concrete colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlTone {
    Neutral,
    Accent,
    Destructive,
}

/// Small monochrome marks used throughout Sift's native chrome. They are
/// intentionally application-owned rather than borrowed from another IDE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconName {
    Add,
    Check,
    ChevronDown,
    ChevronRight,
    Close,
    Copy,
    Database,
    Info,
    Menu,
    Play,
    Search,
    Server,
    User,
    Warning,
    Workspace,
}

impl IconName {
    pub const fn path(self) -> &'static str {
        match self {
            Self::Add => "icons/add.svg",
            Self::Check => "icons/check.svg",
            Self::ChevronDown => "icons/chevron-down.svg",
            Self::ChevronRight => "icons/chevron-right.svg",
            Self::Close => "icons/close.svg",
            Self::Copy => "icons/copy.svg",
            Self::Database => "icons/database.svg",
            Self::Info => "icons/info.svg",
            Self::Menu => "icons/menu.svg",
            Self::Play => "icons/play.svg",
            Self::Search => "icons/search.svg",
            Self::Server => "icons/server.svg",
            Self::User => "icons/user.svg",
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

/// Interaction states shared by the initial Sift component set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlState {
    Rest,
    Hover,
    Active,
    FocusVisible,
    Selected,
    Disabled,
    Loading,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ControlVisual {
    pub background: Hsla,
    pub foreground: Hsla,
    pub border: Hsla,
    pub opacity: f32,
}

impl ControlVisual {
    pub fn resolve(theme: Theme, tone: ControlTone, state: ControlState) -> Self {
        let colors = theme.colors;
        let tone_color = match tone {
            ControlTone::Neutral => colors.elevated_surface,
            ControlTone::Accent => colors.accent,
            ControlTone::Destructive => colors.danger,
        };
        let background = match state {
            ControlState::Hover => match tone {
                ControlTone::Neutral => colors.hovered_surface,
                _ => colors.accent_hover,
            },
            ControlState::Active | ControlState::Selected | ControlState::FocusVisible => {
                colors.accent
            }
            ControlState::Error => colors.danger,
            ControlState::Rest if tone == ControlTone::Neutral => colors.elevated_surface,
            _ => tone_color,
        };
        Self {
            background,
            foreground: if state == ControlState::Disabled {
                colors.disabled_text
            } else {
                colors.text
            },
            border: if state == ControlState::FocusVisible {
                colors.focus_ring
            } else {
                colors.border
            },
            opacity: if state == ControlState::Disabled {
                0.45
            } else {
                1.0
            },
        }
    }
}

/// Compact button surface used by shell actions, menus, and modals. Event
/// handlers remain with the owning entity so child controls never reach across
/// the ownership tree.
pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<String>,
    theme: Theme,
    tone: ControlTone,
    state: ControlState,
) -> AnyElement {
    let visual = ControlVisual::resolve(theme, tone, state);
    let label = label.into();
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label.clone())
        .h(theme.metrics.control_height)
        .px_2()
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .border_1()
        .border_color(visual.border)
        .bg(visual.background)
        .text_color(visual.foreground)
        .opacity(visual.opacity)
        .child(label)
        .into_any_element()
}

pub fn icon_button(
    id: impl Into<ElementId>,
    name: IconName,
    label: impl Into<SharedString>,
    theme: Theme,
    selected: bool,
) -> AnyElement {
    let colors = theme.colors;
    let label = label.into();
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label)
        .size(theme.metrics.control_height)
        .flex()
        .items_center()
        .justify_center()
        .rounded(theme.metrics.radius)
        .text_color(if selected {
            colors.text
        } else {
            colors.muted_text
        })
        .when(selected, |el| el.bg(colors.active_surface))
        .hover(|el| el.bg(colors.hovered_surface).text_color(colors.text))
        .child(icon(
            name,
            if selected {
                colors.text
            } else {
                colors.muted_text
            },
            15.,
        ))
        .into_any_element()
}

pub fn badge(label: impl Into<String>, theme: Theme, tone: ControlTone) -> AnyElement {
    let colors = theme.colors;
    let (background, foreground) = match tone {
        ControlTone::Neutral => (colors.hovered_surface, colors.muted_text),
        ControlTone::Accent => (colors.accent_muted, colors.accent_hover),
        ControlTone::Destructive => (colors.danger_muted, colors.danger),
    };
    div()
        .h(px(18.))
        .px_1()
        .flex()
        .items_center()
        .rounded(px(4.))
        .bg(background)
        .text_xs()
        .text_color(foreground)
        .child(label.into())
        .into_any_element()
}

pub fn section_label(label: impl Into<String>, theme: Theme) -> AnyElement {
    div()
        .h(px(22.))
        .flex()
        .items_center()
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme.colors.muted_text)
        .child(label.into())
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_states_have_distinct_focus_disabled_and_error_treatment() {
        let theme = Theme::dark();
        let rest = ControlVisual::resolve(theme, ControlTone::Neutral, ControlState::Rest);
        let focus = ControlVisual::resolve(theme, ControlTone::Neutral, ControlState::FocusVisible);
        let disabled = ControlVisual::resolve(theme, ControlTone::Neutral, ControlState::Disabled);
        let error = ControlVisual::resolve(theme, ControlTone::Neutral, ControlState::Error);
        assert_ne!(rest.border, focus.border);
        assert!(disabled.opacity < rest.opacity);
        assert_eq!(error.background, theme.colors.danger);
        assert_eq!(IconName::Play.path(), "icons/play.svg");
    }
}
