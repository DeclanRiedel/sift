use gpui::{hsla, Hsla};

/// Semantic colors consumed by Sift components.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThemeColors {
    pub background: Hsla,
    pub surface: Hsla,
    pub elevated_surface: Hsla,
    pub selected_surface: Hsla,
    pub scrim: Hsla,
    pub border: Hsla,
    pub text: Hsla,
    pub muted_text: Hsla,
    pub accent: Hsla,
    pub accent_hover: Hsla,
    pub focus_ring: Hsla,
    pub danger: Hsla,
    pub warning: Hsla,
    pub success: Hsla,
}

/// Complete semantic theme used by the desktop shell.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    pub colors: ThemeColors,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            colors: ThemeColors {
                background: hsla(0.625, 0.16, 0.105, 1.0),
                surface: hsla(0.625, 0.14, 0.135, 1.0),
                elevated_surface: hsla(0.625, 0.13, 0.17, 1.0),
                selected_surface: hsla(0.0, 0.0, 1.0, 0.07),
                scrim: hsla(0.0, 0.0, 0.0, 0.40),
                border: hsla(0.625, 0.10, 0.245, 1.0),
                text: hsla(0.60, 0.12, 0.90, 1.0),
                muted_text: hsla(0.60, 0.08, 0.62, 1.0),
                accent: hsla(0.55, 0.62, 0.56, 1.0),
                accent_hover: hsla(0.55, 0.68, 0.64, 1.0),
                focus_ring: hsla(0.55, 0.72, 0.62, 1.0),
                danger: hsla(0.005, 0.68, 0.58, 1.0),
                warning: hsla(0.105, 0.72, 0.58, 1.0),
                success: hsla(0.39, 0.50, 0.48, 1.0),
            },
        }
    }

    pub fn light() -> Self {
        Self {
            colors: ThemeColors {
                background: hsla(0.60, 0.18, 0.965, 1.0),
                surface: hsla(0.60, 0.12, 0.995, 1.0),
                elevated_surface: hsla(0.60, 0.12, 1.0, 1.0),
                selected_surface: hsla(0.55, 0.40, 0.43, 0.10),
                scrim: hsla(0.0, 0.0, 0.0, 0.28),
                border: hsla(0.60, 0.10, 0.82, 1.0),
                text: hsla(0.625, 0.20, 0.18, 1.0),
                muted_text: hsla(0.62, 0.09, 0.43, 1.0),
                accent: hsla(0.55, 0.68, 0.43, 1.0),
                accent_hover: hsla(0.55, 0.72, 0.36, 1.0),
                focus_ring: hsla(0.55, 0.72, 0.43, 1.0),
                danger: hsla(0.005, 0.72, 0.48, 1.0),
                warning: hsla(0.105, 0.78, 0.43, 1.0),
                success: hsla(0.39, 0.58, 0.36, 1.0),
            },
        }
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
    }
}
