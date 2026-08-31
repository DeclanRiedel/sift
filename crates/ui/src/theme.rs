use gpui::{hsla, px, App, Hsla, Pixels};
use serde::Deserialize;

const THEME_VERSION: u32 = 1;
const AYU_DARK_SOURCE: &str = include_str!("../themes/ayu-dark.toml");
const LIGHT_SOURCE: &str = include_str!("../themes/light.toml");

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeAppearance {
    Light,
    #[default]
    Dark,
}

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
    pub drop_target_background: Hsla,
    pub drop_target_border: Hsla,
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
    pub appearance: ThemeAppearance,
    pub colors: ThemeColors,
    pub metrics: ThemeMetrics,
}

/// User-editable TOML theme. Every color is optional and inherits from the
/// built-in palette for the chosen appearance.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeConfig {
    pub version: u32,
    pub name: String,
    #[serde(default)]
    pub appearance: ThemeAppearance,
    #[serde(default)]
    colors: ThemeColorOverrides,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ThemeColorOverrides {
    background: Option<Hsla>,
    surface: Option<Hsla>,
    panel: Option<Hsla>,
    toolbar: Option<Hsla>,
    elevated_surface: Option<Hsla>,
    hovered_surface: Option<Hsla>,
    selected_surface: Option<Hsla>,
    active_surface: Option<Hsla>,
    scrim: Option<Hsla>,
    border: Option<Hsla>,
    subtle_border: Option<Hsla>,
    strong_border: Option<Hsla>,
    text: Option<Hsla>,
    muted_text: Option<Hsla>,
    disabled_text: Option<Hsla>,
    accent: Option<Hsla>,
    accent_muted: Option<Hsla>,
    accent_hover: Option<Hsla>,
    drop_target_background: Option<Hsla>,
    drop_target_border: Option<Hsla>,
    on_accent: Option<Hsla>,
    focus_ring: Option<Hsla>,
    danger: Option<Hsla>,
    danger_muted: Option<Hsla>,
    warning: Option<Hsla>,
    warning_muted: Option<Hsla>,
    success: Option<Hsla>,
    success_muted: Option<Hsla>,
    editor_active_line: Option<Hsla>,
    grid_stripe: Option<Hsla>,
    syntax_keyword: Option<Hsla>,
    syntax_string: Option<Hsla>,
    syntax_number: Option<Hsla>,
    syntax_comment: Option<Hsla>,
}

impl ThemeConfig {
    pub fn decode(source: &str) -> Result<Self, String> {
        let config: Self =
            toml::from_str(source).map_err(|error| format!("theme file is invalid: {error}"))?;
        if config.version != THEME_VERSION {
            return Err(format!(
                "theme version {} is unsupported; expected {THEME_VERSION}",
                config.version
            ));
        }
        if config.name.trim().is_empty() {
            return Err("theme name must not be empty".into());
        }
        Ok(config)
    }

    pub fn theme(&self) -> Theme {
        let mut theme = match self.appearance {
            ThemeAppearance::Dark => Theme::dark_base(),
            ThemeAppearance::Light => Theme::light_base(),
        };
        theme.appearance = self.appearance;
        self.colors.apply(&mut theme.colors);
        theme
    }
}

impl ThemeColorOverrides {
    fn apply(&self, colors: &mut ThemeColors) {
        macro_rules! apply {
            ($($field:ident),+ $(,)?) => {
                $(if let Some(color) = self.$field { colors.$field = color; })+
            };
        }
        apply!(
            background,
            surface,
            panel,
            toolbar,
            elevated_surface,
            hovered_surface,
            selected_surface,
            active_surface,
            scrim,
            border,
            subtle_border,
            strong_border,
            text,
            muted_text,
            disabled_text,
            accent,
            accent_muted,
            accent_hover,
            drop_target_background,
            drop_target_border,
            on_accent,
            focus_ring,
            danger,
            danger_muted,
            warning,
            warning_muted,
            success,
            success_muted,
            editor_active_line,
            grid_stripe,
            syntax_keyword,
            syntax_string,
            syntax_number,
            syntax_comment,
        );
    }
}

impl Theme {
    pub fn dark() -> Self {
        ThemeConfig::decode(AYU_DARK_SOURCE)
            .expect("bundled Ayu Dark theme must be valid")
            .theme()
    }

    pub fn builtin(name: &str) -> Option<Self> {
        match name {
            "ayu-dark" | "dark" => Some(Self::dark()),
            "light" => Some(Self::light()),
            _ => None,
        }
    }

    pub fn builtin_source(name: &str) -> Option<&'static str> {
        match name {
            "ayu-dark" | "dark" => Some(AYU_DARK_SOURCE),
            "light" => Some(LIGHT_SOURCE),
            _ => None,
        }
    }

    fn dark_base() -> Self {
        Self {
            appearance: ThemeAppearance::Dark,
            colors: ThemeColors {
                background: hsla(0.625, 0.16, 0.105, 1.0),
                surface: hsla(0.625, 0.14, 0.125, 1.0),
                panel: hsla(0.625, 0.14, 0.142, 1.0),
                toolbar: hsla(0.625, 0.15, 0.118, 1.0),
                elevated_surface: hsla(0.625, 0.13, 0.18, 1.0),
                hovered_surface: hsla(0.60, 0.12, 1.0, 0.055),
                selected_surface: hsla(0.05029586, 0.86, 0.38627452, 0.14),
                active_surface: hsla(0.05029586, 0.86, 0.38627452, 0.20),
                scrim: hsla(0.0, 0.0, 0.0, 0.40),
                border: hsla(0.625, 0.10, 0.235, 1.0),
                subtle_border: hsla(0.625, 0.10, 0.195, 1.0),
                strong_border: hsla(0.61, 0.10, 0.33, 1.0),
                text: hsla(0.60, 0.12, 0.90, 1.0),
                muted_text: hsla(0.60, 0.08, 0.62, 1.0),
                disabled_text: hsla(0.60, 0.06, 0.42, 1.0),
                accent: hsla(0.05029586, 0.857868, 0.38627452, 1.0),
                accent_muted: hsla(0.05029586, 0.86, 0.38627452, 0.16),
                accent_hover: hsla(0.05029586, 0.82, 0.48, 1.0),
                drop_target_background: hsla(0.05029586, 0.86, 0.38627452, 0.16),
                drop_target_border: hsla(0.60, 0.12, 0.78, 1.0),
                on_accent: hsla(0.60, 0.10, 0.98, 1.0),
                focus_ring: hsla(0.05029586, 0.82, 0.48, 1.0),
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
        ThemeConfig::decode(LIGHT_SOURCE)
            .expect("bundled light theme must be valid")
            .theme()
    }

    fn light_base() -> Self {
        Self {
            appearance: ThemeAppearance::Light,
            colors: ThemeColors {
                background: hsla(0.60, 0.18, 0.965, 1.0),
                surface: hsla(0.60, 0.12, 0.995, 1.0),
                panel: hsla(0.60, 0.14, 0.98, 1.0),
                toolbar: hsla(0.60, 0.16, 0.955, 1.0),
                elevated_surface: hsla(0.60, 0.12, 1.0, 1.0),
                hovered_surface: hsla(0.60, 0.30, 0.35, 0.055),
                selected_surface: hsla(0.05029586, 0.86, 0.38627452, 0.10),
                active_surface: hsla(0.05029586, 0.86, 0.38627452, 0.16),
                scrim: hsla(0.0, 0.0, 0.0, 0.28),
                border: hsla(0.60, 0.10, 0.82, 1.0),
                subtle_border: hsla(0.60, 0.10, 0.89, 1.0),
                strong_border: hsla(0.60, 0.10, 0.70, 1.0),
                text: hsla(0.625, 0.20, 0.18, 1.0),
                muted_text: hsla(0.62, 0.09, 0.43, 1.0),
                disabled_text: hsla(0.62, 0.07, 0.63, 1.0),
                accent: hsla(0.05029586, 0.857868, 0.38627452, 1.0),
                accent_muted: hsla(0.05029586, 0.86, 0.38627452, 0.12),
                accent_hover: hsla(0.05029586, 0.88, 0.32, 1.0),
                drop_target_background: hsla(0.05029586, 0.86, 0.38627452, 0.12),
                drop_target_border: hsla(0.62, 0.16, 0.28, 1.0),
                on_accent: hsla(0.60, 0.10, 0.99, 1.0),
                focus_ring: hsla(0.05029586, 0.88, 0.32, 1.0),
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

    #[test]
    fn light_theme_keeps_the_rust_orange_accent() {
        assert_eq!(Theme::light().colors.accent, gpui::rgb(0x985f12).into());
    }

    #[test]
    fn partial_theme_inherits_and_rejects_unknown_tokens() {
        let config = ThemeConfig::decode(
            "version = 1\nname = \"Personal\"\nappearance = \"dark\"\n[colors]\naccent = \"#ff0000\"\n",
        )
        .unwrap();
        let theme = config.theme();
        assert_eq!(theme.colors.accent, gpui::rgb(0xff0000).into());
        assert_eq!(
            theme.colors.background,
            Theme::dark_base().colors.background
        );
        assert!(ThemeConfig::decode(
            "version = 1\nname = \"Broken\"\n[colors]\nmade_up = \"#fff\"\n"
        )
        .is_err());
    }

    #[test]
    fn bundled_dark_theme_is_ayu_based_and_darker_than_ayu_background() {
        let theme = Theme::dark();
        assert_eq!(theme.appearance, ThemeAppearance::Dark);
        assert_eq!(theme.colors.background, gpui::rgb(0x0a0b0d).into());
        assert_eq!(theme.colors.syntax_keyword, gpui::rgb(0xff8f40).into());
    }
}
