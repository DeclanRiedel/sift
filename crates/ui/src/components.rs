use gpui::{div, prelude::*, px, AnyElement, ElementId, Hsla, Role};

use crate::Theme;

/// Semantic intent for an interactive control. Feature views select intent;
/// the theme remains responsible for concrete colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlTone {
    Neutral,
    Accent,
    Destructive,
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
            ControlState::Hover => colors.accent_hover,
            ControlState::Active | ControlState::Selected | ControlState::FocusVisible => {
                colors.accent
            }
            ControlState::Error => colors.danger,
            _ => tone_color,
        };
        Self {
            background,
            foreground: colors.text,
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
        .h(px(26.))
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
    }
}
