use flate2::{Compression, write::GzEncoder};
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

const HTML: &str = "text/html; charset=utf-8";

struct Bundle {
    out: PathBuf,
    assets: String,
    headers: String,
}

impl Bundle {
    fn add(
        &mut self,
        route: &str,
        file: &str,
        content: impl AsRef<[u8]>,
        mime: &str,
        title: Option<&str>,
    ) -> io::Result<()> {
        let content = content.as_ref();
        let raw = self.out.join(file);
        fs::create_dir_all(raw.parent().unwrap())?;
        fs::write(&raw, content)?;
        let gzip = raw.with_file_name(format!("{}.gz", raw.file_name().unwrap().to_str().unwrap()));
        let br = raw.with_file_name(format!("{}.br", raw.file_name().unwrap().to_str().unwrap()));
        let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(content)?;
        fs::write(&gzip, encoder.finish()?)?;
        let mut compressed = Vec::new();
        {
            let mut encoder = brotli::CompressorWriter::new(&mut compressed, 4096, 11, 22);
            encoder.write_all(content)?;
        }
        fs::write(&br, compressed)?;
        let etag = format!("W/\"{}\"", digest(content));
        self.assets.push_str(&format!(
            "Asset {{ path: {route:?}, file: {file:?}, mime: {mime:?}, title: {title:?}, etag: {etag:?}, raw: include_bytes!({raw:?}), gzip: include_bytes!({gzip:?}), br: include_bytes!({br:?}) }},\n"
        ));
        if let Some(title) = title {
            self.headers.push_str(&format!("{route}\n  Content-Type: {HTML}\n  X-Wiki-Title: {title}\n  X-Robots-Tag: noindex\n\n"));
        }
        Ok(())
    }

    fn fingerprint(
        &mut self,
        name: &str,
        content: impl AsRef<[u8]>,
        mime: &str,
    ) -> io::Result<String> {
        let content = content.as_ref();
        let (stem, extension) = name.rsplit_once('.').unwrap();
        let path = format!("/assets/{stem}.{}.{extension}", &digest(content)[..16]);
        self.add(&path, &path[1..], content, mime, None)?;
        Ok(path)
    }
}

fn digest(content: &[u8]) -> String {
    format!("{:x}", Sha256::digest(content))
}

fn main() -> io::Result<()> {
    let docs = Path::new("../../docs/keyboard-wiki");
    println!("cargo:rerun-if-changed={}", docs.display());
    let icon = Path::new("../desktop/assets/sift-icon.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let mut bundle = Bundle {
        out: out.join("site"),
        assets: "static ASSETS: &[Asset] = &[\n".into(),
        // Leave unversioned HTML/fragments with the CDN's revalidation default.
        headers: "/assets/*\n  Cache-Control: public, max-age=31536000, immutable\n\n".into(),
    };
    let favicon = bundle.fingerprint("sift.ico", fs::read(icon)?, "image/x-icon")?;
    let cells = bundle.fingerprint(
        "cells.svg",
        &fs::read_to_string(docs.join("cells.svg"))?,
        "image/svg+xml",
    )?;
    let css = fs::read_to_string(docs.join("styles.css"))?
        .replace("\"cells.svg\"", &format!("{cells:?}"));
    let styles = bundle.fingerprint("styles.css", &css, "text/css; charset=utf-8")?;
    let script = bundle.fingerprint(
        "wiki.js",
        &fs::read_to_string(docs.join("wiki.js"))?,
        "application/javascript; charset=utf-8",
    )?;

    for (slug, source) in [
        ("overview", "overview.html"),
        ("keyboard", "index.html"),
        ("configuration", "configuration.html"),
        ("hosting", "hosting.html"),
        ("shared-rooms", "shared-rooms.html"),
    ] {
        let html = fs::read_to_string(docs.join(source))?
            .replace("href=\"styles.css\"", &format!("href={styles:?}"))
            .replace("href=\"/styles.css\"", &format!("href={styles:?}"))
            .replace("src=\"/wiki.js\"", &format!("src={script:?}"))
            .replace("href=\"sift.ico\"", &format!("href={favicon:?}"));
        let title = html
            .split_once("<title>")
            .expect("wiki title")
            .1
            .split_once("</title>")
            .expect("wiki title end")
            .0;
        let main = html
            .split_once("<main>")
            .expect("wiki main")
            .1
            .split_once("</main>")
            .expect("wiki main end")
            .0;
        let fragment = main.split_once("</div>").expect("wiki tabs end").1.trim();
        let route = if slug == "overview" {
            "/".into()
        } else {
            format!("/{slug}")
        };
        let file = if slug == "overview" {
            "index.html".into()
        } else {
            format!("{slug}.html")
        };
        bundle.add(&route, &file, &html, HTML, None)?;
        bundle.add(
            &format!("/fragments/{slug}"),
            &format!("fragments/{slug}"),
            fragment,
            HTML,
            Some(title),
        )?;
    }
    bundle.add("/404.html", "404.html", "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><title>Page not found</title><h1>Page not found</h1><p><a href=\"/\">Sift documentation</a></p></html>", HTML, None)?;
    bundle.assets.push_str("];");
    fs::write(out.join("assets.rs"), bundle.assets)?;
    fs::write(out.join("headers.txt"), bundle.headers)?;
    let aliases = [
        ("/favicon.ico", favicon.as_str()),
        ("/sift.ico", favicon.as_str()),
        ("/wiki.js", script.as_str()),
        ("/styles.css", styles.as_str()),
        ("/cells.svg", cells.as_str()),
    ];
    fs::write(
        out.join("aliases.rs"),
        format!("static ALIASES: &[(&str, &str)] = &{aliases:?};"),
    )?;
    let mut redirects = String::from(
        "/index.html /keyboard 301\n/configuration.html /configuration 301\n/hosting.html /hosting 301\n/shared-rooms.html /shared-rooms 301\n",
    );
    for (from, to) in aliases {
        redirects.push_str(&format!("{from} {to} 302\n"));
    }
    fs::write(out.join("redirects.txt"), redirects)?;
    Ok(())
}
