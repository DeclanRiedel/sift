use std::borrow::Cow;
use std::sync::{Arc, OnceLock};

use gpui::{AssetSource, RenderImage, Result, SharedString, SvgRenderer};

static POSTGRES_LOGO: OnceLock<Arc<RenderImage>> = OnceLock::new();
static SQL_SERVER_LOGO: OnceLock<Arc<RenderImage>> = OnceLock::new();

/// Small, compile-time asset bundle used by the native desktop application.
#[derive(Clone, Copy, Default)]
pub struct SiftAssets;

/// Render a database mark as a full-colour image. GPUI's `svg()` element is a
/// monochrome icon mask, which is not suitable for vendor artwork.
pub fn database_logo(path: &str) -> Arc<RenderImage> {
    let (cache, bytes) = match path {
        "databases/postgres.svg" => (
            &POSTGRES_LOGO,
            include_bytes!("../assets/databases/postgres.svg").as_slice(),
        ),
        "databases/sql-server.svg" => (
            &SQL_SERVER_LOGO,
            include_bytes!("../assets/databases/sql-server.svg").as_slice(),
        ),
        _ => panic!("unknown database logo: {path}"),
    };
    cache
        .get_or_init(|| {
            SvgRenderer::new(Arc::new(SiftAssets))
                .render_single_frame(bytes, 1.0)
                .expect("embedded database SVG must be valid")
        })
        .clone()
}

impl AssetSource for SiftAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let bytes: Option<&'static [u8]> = match path {
            "databases/postgres.svg" => Some(include_bytes!("../assets/databases/postgres.svg")),
            "databases/sql-server.svg" => {
                Some(include_bytes!("../assets/databases/sql-server.svg"))
            }
            "icons/add.svg" => Some(include_bytes!("../assets/icons/add.svg")),
            "icons/activity.svg" => Some(include_bytes!("../assets/icons/activity.svg")),
            "icons/automations.svg" => Some(include_bytes!("../assets/icons/automations.svg")),
            "icons/check.svg" => Some(include_bytes!("../assets/icons/check.svg")),
            "icons/chevron-down.svg" => Some(include_bytes!("../assets/icons/chevron-down.svg")),
            "icons/chevron-left.svg" => Some(include_bytes!("../assets/icons/chevron-left.svg")),
            "icons/chevron-right.svg" => Some(include_bytes!("../assets/icons/chevron-right.svg")),
            "icons/close.svg" => Some(include_bytes!("../assets/icons/close.svg")),
            "icons/copy.svg" => Some(include_bytes!("../assets/icons/copy.svg")),
            "icons/database.svg" => Some(include_bytes!("../assets/icons/database.svg")),
            "icons/fallback.svg" => Some(include_bytes!("../assets/icons/fallback.svg")),
            "icons/github.svg" => Some(include_bytes!("../assets/icons/github.svg")),
            "icons/info.svg" => Some(include_bytes!("../assets/icons/info.svg")),
            "icons/keyboard.svg" => Some(include_bytes!("../assets/icons/keyboard.svg")),
            "icons/maximize.svg" => Some(include_bytes!("../assets/icons/maximize.svg")),
            "icons/menu.svg" => Some(include_bytes!("../assets/icons/menu.svg")),
            "icons/minimize.svg" => Some(include_bytes!("../assets/icons/minimize.svg")),
            "icons/outline.svg" => Some(include_bytes!("../assets/icons/outline.svg")),
            "icons/play.svg" => Some(include_bytes!("../assets/icons/play.svg")),
            "icons/search.svg" => Some(include_bytes!("../assets/icons/search.svg")),
            "icons/server.svg" => Some(include_bytes!("../assets/icons/server.svg")),
            "icons/terminal.svg" => Some(include_bytes!("../assets/icons/terminal.svg")),
            "icons/user.svg" => Some(include_bytes!("../assets/icons/user.svg")),
            "icons/users.svg" => Some(include_bytes!("../assets/icons/users.svg")),
            "icons/version-control.svg" => {
                Some(include_bytes!("../assets/icons/version-control.svg"))
            }
            "icons/warning.svg" => Some(include_bytes!("../assets/icons/warning.svg")),
            "icons/workspace.svg" => Some(include_bytes!("../assets/icons/workspace.svg")),
            "icons/LICENSE.qlementine-icons" => {
                Some(include_bytes!("../assets/icons/LICENSE.qlementine-icons"))
            }
            _ => None,
        };
        Ok(bytes.map(Cow::Borrowed))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(match path {
            "databases" => vec!["postgres.svg".into(), "sql-server.svg".into()],
            "icons" => vec![
                "activity.svg".into(),
                "add.svg".into(),
                "automations.svg".into(),
                "check.svg".into(),
                "chevron-down.svg".into(),
                "chevron-left.svg".into(),
                "chevron-right.svg".into(),
                "close.svg".into(),
                "copy.svg".into(),
                "database.svg".into(),
                "fallback.svg".into(),
                "github.svg".into(),
                "info.svg".into(),
                "keyboard.svg".into(),
                "maximize.svg".into(),
                "menu.svg".into(),
                "minimize.svg".into(),
                "outline.svg".into(),
                "play.svg".into(),
                "search.svg".into(),
                "server.svg".into(),
                "terminal.svg".into(),
                "user.svg".into(),
                "users.svg".into(),
                "version-control.svg".into(),
                "warning.svg".into(),
                "workspace.svg".into(),
            ],
            _ => Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IconName;

    #[test]
    fn visual_assets_are_embedded_in_the_desktop_binary() {
        let assets = SiftAssets;
        for path in ["databases/postgres.svg", "databases/sql-server.svg"] {
            let bytes = assets.load(path).unwrap().expect("database asset");
            assert!(bytes.starts_with(b"<svg"));
            let _rendered = database_logo(path);
        }
        for name in IconName::ALL {
            let bytes = assets.load(name.path()).unwrap().expect("icon asset");
            assert!(bytes.starts_with(b"<svg"));
        }
        let fallback = assets
            .load("icons/fallback.svg")
            .unwrap()
            .expect("fallback icon");
        assert!(fallback.windows(7).any(|bytes| bytes == b"#e5484d"));
        let license = assets
            .load("icons/LICENSE.qlementine-icons")
            .unwrap()
            .expect("Qlementine license");
        assert!(license
            .windows(14)
            .any(|bytes| bytes == b"Olivier Cl\xC3\xA9ro"));
    }
}
