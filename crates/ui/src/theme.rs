use gpui::{hsla, px, App, Hsla, Pixels};

/// Semantic colors consumed by Sift components.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThemeColors {
    pub background: Hsla,
    pub surface: Hsla,
    pub panel: Hsla,
    pub toolbar: Hsla,
    pub elevated_surface: Hsla,
    pub hovered_surface: Hsla,
    pub selected_surface: Hsla,
    pub active_surface: Hsla,
    pub scrim: Hsla,
    pub border: Hsla,
    pub subtle_border: Hsla,
    pub strong_border: Hsla,
    pub text: Hsla,
    pub muted_text: Hsla,
    pub disabled_text: Hsla,
    pub accent: Hsla,
    pub accent_muted: Hsla,
    pub accent_hover: Hsla,
    /// Foreground that stays legible on accent/danger/success fills.
    pub on_accent: Hsla,
    pub focus_ring: Hsla,
    pub danger: Hsla,
    pub danger_muted: Hsla,
    pub warning: Hsla,
    pub warning_muted: Hsla,
    pub success: Hsla,
    pub success_muted: Hsla,
    pub editor_active_line: Hsla,
    pub grid_stripe: Hsla,
    pub syntax_keyword: Hsla,
    pub syntax_string: Hsla,
    pub syntax_number: Hsla,
    pub syntax_comment: Hsla,
}

/// Shared density and shape values. Keeping these here makes the shell feel
/// like one product instead of a collection of locally-sized controls.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThemeMetrics {
    pub control_height: Pixels,
    pub compact_control_height: Pixels,
    pub row_height: Pixels,
    pub tab_height: Pixels,
    pub toolbar_height: Pixels,
    pub status_height: Pixels,
    pub radius: Pixels,
    pub radius_large: Pixels,
}

/// Complete semantic theme used by the desktop shell.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    pub colors: ThemeColors,
    pub metrics: ThemeMetrics,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            colors: ThemeColors {
                background: hsla(0.625, 0.16, 0.105, 1.0),
                surface: hsla(0.625, 0.14, 0.125, 1.0),
                panel: hsla(0.625, 0.14, 0.142, 1.0),
                toolbar: hsla(0.625, 0.15, 0.118, 1.0),
                elevated_surface: hsla(0.625, 0.13, 0.18, 1.0),
                hovered_surface: hsla(0.60, 0.12, 1.0, 0.055),
                selected_surface: hsla(0.55, 0.42, 0.52, 0.14),
                active_surface: hsla(0.55, 0.50, 0.52, 0.20),
                scrim: hsla(0.0, 0.0, 0.0, 0.40),
                border: hsla(0.625, 0.10, 0.235, 1.0),
                subtle_border: hsla(0.625, 0.10, 0.195, 1.0),
                strong_border: hsla(0.61, 0.10, 0.33, 1.0),
                text: hsla(0.60, 0.12, 0.90, 1.0),
                muted_text: hsla(0.60, 0.08, 0.62, 1.0),
                disabled_text: hsla(0.60, 0.06, 0.42, 1.0),
                accent: hsla(0.55, 0.62, 0.56, 1.0),
                accent_muted: hsla(0.55, 0.55, 0.52, 0.16),
                accent_hover: hsla(0.55, 0.68, 0.64, 1.0),
                on_accent: hsla(0.60, 0.10, 0.98, 1.0),
                focus_ring: hsla(0.55, 0.72, 0.62, 1.0),
                danger: hsla(0.005, 0.68, 0.58, 1.0),
                danger_muted: hsla(0.005, 0.60, 0.52, 0.16),
                warning: hsla(0.105, 0.72, 0.58, 1.0),
                warning_muted: hsla(0.105, 0.65, 0.52, 0.16),
                success: hsla(0.39, 0.50, 0.48, 1.0),
                success_muted: hsla(0.39, 0.45, 0.46, 0.16),
                editor_active_line: hsla(0.60, 0.10, 1.0, 0.025),
                grid_stripe: hsla(0.60, 0.08, 1.0, 0.018),
                syntax_keyword: hsla(0.76, 0.58, 0.72, 1.0),
                syntax_string: hsla(0.39, 0.42, 0.66, 1.0),
                syntax_number: hsla(0.08, 0.68, 0.70, 1.0),
                syntax_comment: hsla(0.60, 0.08, 0.48, 1.0),
            },
            metrics: ThemeMetrics::default(),
        }
    }

    pub fn light() -> Self {
        Self {
            colors: ThemeColors {
                background: hsla(0.60, 0.18, 0.965, 1.0),
                surface: hsla(0.60, 0.12, 0.995, 1.0),
                panel: hsla(0.60, 0.14, 0.98, 1.0),
                toolbar: hsla(0.60, 0.16, 0.955, 1.0),
                elevated_surface: hsla(0.60, 0.12, 1.0, 1.0),
                hovered_surface: hsla(0.60, 0.30, 0.35, 0.055),
                selected_surface: hsla(0.55, 0.40, 0.43, 0.10),
                active_surface: hsla(0.55, 0.50, 0.43, 0.16),
                scrim: hsla(0.0, 0.0, 0.0, 0.28),
                border: hsla(0.60, 0.10, 0.82, 1.0),
                subtle_border: hsla(0.60, 0.10, 0.89, 1.0),
                strong_border: hsla(0.60, 0.10, 0.70, 1.0),
                text: hsla(0.625, 0.20, 0.18, 1.0),
                muted_text: hsla(0.62, 0.09, 0.43, 1.0),
                disabled_text: hsla(0.62, 0.07, 0.63, 1.0),
                accent: hsla(0.55, 0.68, 0.43, 1.0),
                accent_muted: hsla(0.55, 0.58, 0.43, 0.12),
                accent_hover: hsla(0.55, 0.72, 0.36, 1.0),
                on_accent: hsla(0.60, 0.10, 0.99, 1.0),
                focus_ring: hsla(0.55, 0.72, 0.43, 1.0),
                danger: hsla(0.005, 0.72, 0.48, 1.0),
                danger_muted: hsla(0.005, 0.62, 0.48, 0.12),
                warning: hsla(0.105, 0.78, 0.43, 1.0),
                warning_muted: hsla(0.105, 0.70, 0.43, 0.14),
                success: hsla(0.39, 0.58, 0.36, 1.0),
                success_muted: hsla(0.39, 0.50, 0.36, 0.12),
                editor_active_line: hsla(0.55, 0.30, 0.43, 0.035),
                grid_stripe: hsla(0.60, 0.15, 0.40, 0.025),
                syntax_keyword: hsla(0.76, 0.50, 0.47, 1.0),
                syntax_string: hsla(0.39, 0.50, 0.35, 1.0),
                syntax_number: hsla(0.06, 0.62, 0.46, 1.0),
                syntax_comment: hsla(0.60, 0.08, 0.52, 1.0),
            },
            metrics: ThemeMetrics::default(),
        }
    }
}

impl Default for ThemeMetrics {
    fn default() -> Self {
        Self {
            control_height: px(24.),
            compact_control_height: px(20.),
            row_height: px(26.),
            tab_height: px(32.),
            toolbar_height: px(34.),
            status_height: px(22.),
            radius: px(4.),
            radius_large: px(8.),
        }
    }
}

/// Process-wide active theme. Views read it through [`ActiveTheme`] instead of
/// holding their own copy, so swapping the global re-themes every window.
pub struct GlobalTheme(pub Theme);

impl gpui::Global for GlobalTheme {}

impl Default for GlobalTheme {
    fn default() -> Self {
        Self(Theme::dark())
    }
}

/// Install the active theme. Safe to call again to switch appearance at
/// runtime; call sites then refresh their views.
pub fn init_theme(theme: Theme, cx: &mut App) {
    cx.set_global(GlobalTheme(theme));
}

/// Replace the active theme at runtime and re-render every window.
pub fn set_theme(theme: Theme, cx: &mut App) {
    cx.set_global(GlobalTheme(theme));
    cx.refresh_windows();
}

/// Read the active theme from any context. Falls back to the dark palette
/// when no global was installed (for example in isolated tests).
pub trait ActiveTheme {
    fn theme(&self) -> Theme;
}

impl ActiveTheme for App {
    fn theme(&self) -> Theme {
        self.try_global::<GlobalTheme>()
            .map_or_else(Theme::dark, |global| global.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_and_dark_themes_cover_every_semantic_token() {
        assert_ne!(Theme::light(), Theme::dark());
        assert_eq!(Theme::dark().colors.background.a, 1.0);
        assert_eq!(Theme::light().colors.background.a, 1.0);
        // The on-accent foreground must contrast with its fill in both modes.
        assert_ne!(Theme::dark().colors.on_accent, Theme::dark().colors.accent);
        assert_ne!(
            Theme::light().colors.on_accent,
            Theme::light().colors.accent
        );
    }
}
