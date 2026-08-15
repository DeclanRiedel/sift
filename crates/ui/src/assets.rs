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
            _ => None,
        };
        Ok(bytes.map(Cow::Borrowed))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(match path {
            "databases" => vec!["postgres.svg".into(), "sql-server.svg".into()],
            _ => Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_marks_are_embedded_in_the_desktop_binary() {
        let assets = SiftAssets;
        for path in ["databases/postgres.svg", "databases/sql-server.svg"] {
            let bytes = assets.load(path).unwrap().expect("database asset");
            assert!(bytes.starts_with(b"<svg"));
            let _rendered = database_logo(path);
        }
    }
}
