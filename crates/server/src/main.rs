//! `sift-server` binary entry point. Loads config, initialises tracing,
//! builds the driver registry, binds the HTTP server. Local-first
//! (ADR-010): same binary runs in-process alongside the desktop client or
//! as a standalone daemon.

use std::time::Duration;

use anyhow::Context;
use sift_server::{
    config::{load as load_config, Config},
    http::{app, AppState},
    registry::DriverRegistry,
    session::SessionStore,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg = load_config().context("loading config")?;
    init_tracing(&cfg);

    tracing::info!(version = sift_server::VERSION, bind = %cfg.bind, "sift-server starting");

    let registry = build_registry(&cfg);
    let sessions = if let Some(path) = &cfg.audit.operation_log_path {
        SessionStore::new_with_operation_log_path(registry, path)
            .with_context(|| format!("opening operation audit log: {path}"))?
    } else {
        SessionStore::new(registry)
    };
    let state = AppState {
        sessions,
        auth: sift_server::http::AuthState {
            bearer_token: cfg.auth.bearer_token.clone(),
        },
    };

    let app = app(state);

    let bind: std::net::SocketAddr = cfg
        .bind
        .parse()
        .with_context(|| format!("invalid bind address: {}", cfg.bind))?;

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    tracing::info!("listening on http://{bind}");

    axum::serve(listener, app.into_make_service())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server runtime")?;

    tracing::info!("sift-server stopped");
    Ok(())
}

fn init_tracing(cfg: &Config) {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&cfg.log.filter));
    let _ = fmt().with_env_filter(filter).with_target(true).try_init();
}

fn build_registry(cfg: &Config) -> DriverRegistry {
    let mut builder = DriverRegistry::builder();

    if cfg.drivers.mock {
        // MockDriver is registered for engine=postgres; useful for headless
        // tests without a DB. Real driver registration is gated behind
        // config so a `mock=true` sift.toml gives a runnable-no-PG server.
        let mock = sift_driver_api::mock::MockDriver::builder()
            .engine(sift_protocol::Engine::Postgres)
            .ping_ok(sift_protocol::ServerInfo {
                engine: sift_protocol::Engine::Postgres,
                server_version: "MockDB 0.1".to_string(),
                current_database: "mock".to_string(),
                current_user: "mock".to_string(),
            })
            .schema_ok(sift_protocol::SchemaSnapshot::empty(SchemaScope::shallow()))
            .build();
        builder = builder.register(mock);
    } else {
        // Real PG driver. Connections are not actually opened here; the
        // driver just owns pool config + state. `open()` is called per
        // `OpenConnection` request.
        builder = builder.register(sift_driver_postgres::PgDriver::new());
    }

    // Register SQL Server via tiberius. Connections still open lazily per
    // OpenConnection request.
    builder = builder.register(sift_driver_sqlserver::MssqlDriver::new());

    builder.build()
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl-C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("shutdown signal received");
}

// Re-export to satisfy the `SchemaScope::shallow()` call above without
// pulling it into the local scope via a `use` (keeps the registry function
// visually focused on driver wiring).
use sift_protocol::SchemaScope;

// Unused-Duration marker; reap_idle wiring lands with Tier 1 health checks.
#[allow(dead_code)]
fn _duration_marker(_d: Duration) {}
