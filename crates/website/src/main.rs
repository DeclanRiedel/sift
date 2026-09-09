//! Public product page and bundled wiki. No Sift credentials or database access.
use std::{fs, path::Path};
use topcoat::{
    Result,
    context::Cx,
    router::{
        Body, HeaderMap, Router, RouterBuilderDiscoverExt, request, response::Response, route,
    },
};

struct Asset {
    path: &'static str,
    file: &'static str,
    mime: &'static str,
    title: Option<&'static str>,
    etag: &'static str,
    raw: &'static [u8],
    gzip: &'static [u8],
    br: &'static [u8],
}

include!(concat!(env!("OUT_DIR"), "/assets.rs"));
include!(concat!(env!("OUT_DIR"), "/aliases.rs"));

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "127.0.0.1:8787".into());
    if command == "--export" {
        let directory = args.next().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "usage: sift-website --export <directory>",
            )
        })?;
        export(Path::new(&directory))?;
        println!("Static website exported to {directory}");
        return Ok(());
    }
    let listener = tokio::net::TcpListener::bind(&command).await?;
    println!("Website: http://{}", listener.local_addr()?);
    println!("Wiki: http://{}/keyboard", listener.local_addr()?);
    topcoat::serve(listener, Router::builder().discover().build()).await
}

fn export(directory: &Path) -> std::io::Result<()> {
    fs::create_dir_all(directory)?;
    for asset in ASSETS {
        let file = directory.join(asset.file);
        fs::create_dir_all(file.parent().unwrap())?;
        // Static CDNs negotiate compression themselves. Do not publish .gz/.br
        // siblings as independent downloadable files.
        fs::write(file, asset.raw)?;
    }
    fs::write(
        directory.join("_headers"),
        include_str!(concat!(env!("OUT_DIR"), "/headers.txt")),
    )?;
    fs::write(
        directory.join("_redirects"),
        include_str!(concat!(env!("OUT_DIR"), "/redirects.txt")),
    )?;
    Ok(())
}

// Honor explicit exclusions, wildcard encodings, and quality preferences.
// Prefer Brotli over gzip over identity on equal quality.
fn quality(headers: &HeaderMap, encoding: &str) -> f32 {
    let mut wildcard = None;
    for header in headers.get_all("accept-encoding") {
        let Ok(value) = header.to_str() else { continue };
        for item in value.split(',') {
            let mut parts = item.trim().split(';');
            let name = parts.next().unwrap().trim();
            let mut q = 1.0;
            for parameter in parts {
                if let Some((key, value)) = parameter.trim().split_once('=')
                    && key.trim().eq_ignore_ascii_case("q")
                {
                    q = value
                        .trim()
                        .parse::<f32>()
                        .ok()
                        .filter(|q| (0.0..=1.0).contains(q))
                        .unwrap_or(0.0);
                }
            }
            if name.eq_ignore_ascii_case(encoding) {
                return q;
            }
            if name == "*" {
                wildcard = Some(q);
            }
        }
    }
    if encoding == "identity" {
        if wildcard == Some(0.0) { 0.0 } else { 1.0 }
    } else {
        wildcard.unwrap_or(0.0)
    }
}

fn serve(cx: &Cx) -> Result<Response> {
    let path = request::uri(cx).path();
    let legacy = match path {
        "/index.html" => Some("/keyboard"),
        "/configuration.html" => Some("/configuration"),
        "/hosting.html" => Some("/hosting"),
        "/shared-rooms.html" => Some("/shared-rooms"),
        _ => None,
    };
    if let Some(target) = legacy {
        return Ok(Response::builder()
            .status(301)
            .header("Location", target)
            .body(Body::empty())?);
    }
    if let Some((_, target)) = ALIASES.iter().find(|(from, _)| *from == path) {
        return Ok(Response::builder()
            .status(302)
            .header("Location", *target)
            .header("Cache-Control", "no-cache")
            .body(Body::empty())?);
    }
    let Some(asset) = ASSETS.iter().find(|asset| asset.path == path) else {
        return Ok(Response::builder().status(404).body(Body::empty())?);
    };
    let headers = request::headers(cx);
    let mut selected = None;
    let mut best = 0.0;
    for (encoding, bytes) in [
        ("br", asset.br),
        ("gzip", asset.gzip),
        ("identity", asset.raw),
    ] {
        let q = quality(headers, encoding);
        if q > best {
            best = q;
            selected = Some((encoding, bytes));
        }
    }
    let Some((encoding, bytes)) = selected else {
        return Ok(Response::builder()
            .status(406)
            .header("Vary", "Accept-Encoding")
            .body(Body::empty())?);
    };
    let unchanged = headers
        .get("if-none-match")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value.split(',').any(|tag| {
                let tag = tag.trim();
                tag == "*" || tag.trim_start_matches("W/") == asset.etag.trim_start_matches("W/")
            })
        });
    let mut response = Response::builder()
        .header("Content-Type", asset.mime)
        .header(
            "Cache-Control",
            if path.starts_with("/assets/") {
                "public, max-age=31536000, immutable"
            } else {
                "public, no-cache"
            },
        )
        .header("ETag", asset.etag)
        .header("Vary", "Accept-Encoding");
    if let Some(title) = asset.title {
        response = response
            .header("X-Wiki-Title", title)
            .header("X-Robots-Tag", "noindex");
    }
    if encoding != "identity" {
        response = response.header("Content-Encoding", encoding);
    }
    Ok(if unchanged {
        response.status(304).body(Body::empty())?
    } else {
        response
            .header("Content-Length", bytes.len())
            .body(if request::method(cx) == "HEAD" {
                Body::empty()
            } else {
                Body::from(bytes)
            })?
    })
}

#[route(GET "/")]
async fn home(cx: &Cx) -> Result<Response> {
    serve(cx)
}
#[route(GET "/{*path}")]
async fn site(cx: &Cx) -> Result<Response> {
    serve(cx)
}
#[route(HEAD "/")]
async fn home_head(cx: &Cx) -> Result<Response> {
    serve(cx)
}
#[route(HEAD "/{*path}")]
async fn site_head(cx: &Cx) -> Result<Response> {
    serve(cx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn favicon_uses_desktop_icon_in_pages_and_exported_assets() {
        let target = ALIASES
            .iter()
            .find(|(from, _)| *from == "/favicon.ico")
            .unwrap()
            .1;
        assert!(ALIASES.contains(&("/sift.ico", target)));
        let icon = ASSETS.iter().find(|asset| asset.path == target).unwrap();
        assert_eq!(icon.mime, "image/x-icon");
        assert_eq!(
            icon.raw,
            include_bytes!("../../desktop/assets/sift-icon.ico")
        );
        assert_eq!(icon.file, &target[1..]);
        for path in [
            "/",
            "/keyboard",
            "/configuration",
            "/hosting",
            "/shared-rooms",
        ] {
            let page = ASSETS.iter().find(|asset| asset.path == path).unwrap();
            let html = std::str::from_utf8(page.raw).unwrap();
            assert_eq!(html.matches("rel=\"icon\"").count(), 1);
            assert!(html.contains(&format!("rel=\"icon\" href=\"{target}\"")));
            assert!(!html.contains("href=\"data:,\""));
        }
        assert!(
            include_str!(concat!(env!("OUT_DIR"), "/redirects.txt"))
                .contains(&format!("/favicon.ico {target} 302"))
        );
    }

    #[test]
    fn encoding_preferences_and_exclusions() {
        for (header, expected) in [
            ("", [0.0, 0.0, 1.0]),
            ("gzip, br", [1.0, 1.0, 1.0]),
            ("br;q=0, gzip;q=0.8", [0.0, 0.8, 1.0]),
            ("*;q=0.5, identity;q=0", [0.5, 0.5, 0.0]),
            ("*;q=0", [0.0, 0.0, 0.0]),
            ("BR;Q=1, gzip;q=invalid", [1.0, 0.0, 1.0]),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("accept-encoding", header.parse().unwrap());
            assert_eq!(
                [
                    quality(&headers, "br"),
                    quality(&headers, "gzip"),
                    quality(&headers, "identity")
                ],
                expected,
                "{header}"
            );
        }
    }
}
