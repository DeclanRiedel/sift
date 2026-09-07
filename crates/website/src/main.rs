//! Public product page and bundled wiki. No Sift credentials or database access.
use topcoat::{
    Result,
    router::{Body, Router, RouterBuilderDiscoverExt, page, response::Response, route},
    view::{View, view},
};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let bind = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:8787".into());
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    println!("Website: http://{}", listener.local_addr()?);
    println!("Wiki: http://{}/index.html", listener.local_addr()?);
    topcoat::serve(listener, Router::builder().discover().build()).await
}

#[page("/")]
async fn home() -> Result {
    view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8"/>
                <meta name="viewport" content="width=device-width, initial-scale=1"/>
                <title>"Sift overview"</title>
                <link rel="stylesheet" href="/styles.css"/>
            </head>
            <body>
                <main>
                    <header>
                        <div class="doc-tabs" aria-label="Sift documentation">
                            <a class="active" href="/">"Overview"</a>
                            <a href="/index.html">"Keyboard"</a>
                            <a href="/configuration.html">"Sift configuration"</a>
                            <a href="/hosting.html">"Hosting"</a>
                            <a href="/shared-rooms.html">"Shared rooms"</a>
                        </div>
                        <h1>"Sift"</h1>
                        <p>"A fast, Vim-like SQL workspace for PostgreSQL, SQL Server, and SQLite."</p>
                    </header>
                    <nav aria-label="On this page">
                        <a href="#about">"About"</a>
                        <a href="#downloads">"Downloads"</a>
                    </nav>
                    <section id="about">
                        <h2>"About"</h2>
                        <p>"Run it locally or host the same server for a team. Query, inspect schemas, and work with results in one keyboard-driven workspace."</p>
                    </section>
                    <section id="downloads">
                        <h2>"Downloads"</h2>
                        <p>"No binary downloads are published on this site yet."</p>
                        <p>"Run from a checkout with Nix:"</p>
                        <pre><code>"nix run .#desktop"</code></pre>
                    </section>
                    <footer>
                        <p>"AGPL-3.0-only · "<a href="https://github.com/DeclanRiedel/sift">"GitHub"</a></p>
                    </footer>
                </main>
            </body>
        </html>
    }
}

// Only trusted, compile-time documentation is rendered without escaping.
#[page("/index.html")]
async fn keyboard() -> Result {
    Ok(View::unescaped_unchecked(include_str!(
        "../../../docs/keyboard-wiki/index.html"
    )))
}
#[page("/configuration.html")]
async fn configuration() -> Result {
    Ok(View::unescaped_unchecked(include_str!(
        "../../../docs/keyboard-wiki/configuration.html"
    )))
}
#[page("/hosting.html")]
async fn hosting() -> Result {
    Ok(View::unescaped_unchecked(include_str!(
        "../../../docs/keyboard-wiki/hosting.html"
    )))
}
#[page("/shared-rooms.html")]
async fn rooms() -> Result {
    Ok(View::unescaped_unchecked(include_str!(
        "../../../docs/keyboard-wiki/shared-rooms.html"
    )))
}

fn asset(content: &'static str, mime: &'static str) -> Result<Response> {
    Ok(Response::builder()
        .header("Content-Type", mime)
        .body(Body::from(content))?)
}
#[route(GET "/styles.css")]
async fn stylesheet() -> Result<Response> {
    asset(
        include_str!("../../../docs/keyboard-wiki/styles.css"),
        "text/css; charset=utf-8",
    )
}
#[route(GET "/cells.svg")]
async fn cells() -> Result<Response> {
    asset(
        include_str!("../../../docs/keyboard-wiki/cells.svg"),
        "image/svg+xml",
    )
}
// Browsers may request this implicitly; there is deliberately no favicon.
#[route(GET "/favicon.ico")]
async fn favicon() -> Result<Response> {
    Ok(Response::builder().status(204).body(Body::empty())?)
}
