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
    println!("Sift website: http://{}", listener.local_addr()?);
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
                <meta name="description" content="A fast, Vim-like SQL workspace for PostgreSQL, SQL Server, and SQLite."/>
                <title>"Sift — SQL workspace"</title>
                <link rel="icon" href="/favicon.svg" type="image/svg+xml"/>
                <link rel="stylesheet" href="/styles.css"/>
            </head>
            <body>
                <a class="skip-link" href="#content">"Skip to content"</a>
                <main class="product" id="content">
                    <header class="site-header">
                        <a class="wordmark" href="/" aria-label="Sift home">"sift"<span>"_"</span></a>
                        <nav aria-label="Main navigation">
                            <a href="/index.html">"Wiki"</a>
                            <a href="#about">"About"</a>
                            <a href="#downloads">"Downloads"</a>
                        </nav>
                    </header>
                    <section class="hero" aria-labelledby="intro">
                        <p class="eyebrow">"THE SQL WORKSPACE"</p>
                        <h1 id="intro">"A keyboard-first"<br/>"SQL workspace."</h1>
                        <p class="hero-description">"A fast, Vim-like SQL workspace for PostgreSQL, SQL Server, and SQLite."</p>
                        <div class="hero-actions">
                            <a class="button primary" href="#downloads">"Get Sift"<span aria-hidden="true">"↗"</span></a>
                            <a class="text-link" href="/index.html">"Read the wiki →"</a>
                        </div>
                    </section>
                    <div class="providers" aria-label="Supported databases">
                        <span><img src="/postgres.svg" alt=""/>"PostgreSQL"</span>
                        <span><img src="/sql-server.svg" alt=""/>"SQL Server"</span>
                        <span><img class="sqlite-logo" src="/sqlite.svg" alt="SQLite"/></span>
                    </div>
                    <section class="section-row" id="about">
                        <h2>"About"</h2>
                        <div><p>"Run it locally or host the same server for a team. Query, inspect schemas, and work with results in one keyboard-driven workspace."</p>
                        <a class="text-link" href="/shared-rooms.html">"Shared rooms →"</a></div>
                    </section>
                    <section class="section-row" id="downloads">
                        <h2>"Downloads"</h2>
                        <div><p>"No binary downloads are published on this site yet."</p>
                        <p class="command-label">"Run from a checkout with Nix"</p>
                        <pre class="install-command"><code>"nix run .#desktop"</code></pre></div>
                    </section>
                    <section class="section-row wiki-row" id="wiki">
                        <h2>"Wiki"</h2>
                        <div class="wiki-links">
                            <a href="/index.html">"Keyboard reference"<span aria-hidden="true">"↗"</span></a>
                            <a href="/configuration.html">"Configuration"<span aria-hidden="true">"↗"</span></a>
                            <a href="/hosting.html">"Hosting"<span aria-hidden="true">"↗"</span></a>
                            <a href="/shared-rooms.html">"Shared rooms"<span aria-hidden="true">"↗"</span></a>
                        </div>
                    </section>
                    <footer class="site-footer"><span>"sift"</span><span>"AGPL-3.0-only"</span></footer>
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
#[route(GET "/favicon.svg")]
async fn favicon() -> Result<Response> {
    asset(
        include_str!("../../../docs/keyboard-wiki/favicon.svg"),
        "image/svg+xml",
    )
}
#[route(GET "/postgres.svg")]
async fn postgres() -> Result<Response> {
    asset(
        include_str!("../../ui/assets/databases/postgres.svg"),
        "image/svg+xml",
    )
}
#[route(GET "/sql-server.svg")]
async fn sql_server() -> Result<Response> {
    asset(
        include_str!("../../ui/assets/databases/sql-server.svg"),
        "image/svg+xml",
    )
}
#[route(GET "/sqlite.svg")]
async fn sqlite() -> Result<Response> {
    asset(
        include_str!("../../ui/assets/databases/sqlite.svg"),
        "image/svg+xml",
    )
}
