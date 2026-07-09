//! `sift-server` binary entry point. Loads config, initialises tracing,
//! builds the driver registry, binds the HTTP server. Local-first
//! (ADR-010): same binary runs in-process alongside the desktop client or
//! as a standalone daemon.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context};
use sift_metadata::{FileSecretStore, MemorySecretStore, MetadataStore};
use sift_server::{
    config::{load as load_config, Config},
    http::{app, AppState},
    registry::DriverRegistry,
    room_runtime::RoomRuntime,
    session::SessionStore,
    Shutdown,
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
    sessions.set_request_timeout(std::time::Duration::from_secs(cfg.timeouts.request_secs));
    sessions.set_store_sql(cfg.metadata.store_sql);
    sessions.set_result_limits(
        cfg.limits.max_http_result_rows,
        cfg.limits.max_http_result_bytes,
    );
    // Wire ADR-011 cursor registry cap.
    {
        let mut cursor_cfg = sessions.cursor_registry().config();
        cursor_cfg.max_per_session = cfg.limits.max_cursors_per_session;
        sessions.cursor_registry().set_config(cursor_cfg);
    }
    let metadata = build_metadata_store(&cfg)?;
    if let Some(store) = &metadata {
        sessions.set_audit_store(store.clone());
    }
    let shutdown = Shutdown::default();
    let state = AppState {
        sessions,
        rooms: RoomRuntime::default(),
        auth: sift_server::http::AuthState {
            bearer_token: cfg.auth.bearer_token.clone(),
            loopback_bypass: cfg.auth.loopback_bypass,
        },
        metadata,
        shutdown: shutdown.clone(),
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

    let drain_deadline = std::time::Duration::from_secs(cfg.timeouts.shutdown_drain_secs);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_sequence(shutdown, drain_deadline))
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
        let server_info = sift_protocol::ServerInfo {
            engine: sift_protocol::Engine::Postgres,
            server_version: "MockDB 0.1".to_string(),
            current_database: "mock".to_string(),
            current_user: "mock".to_string(),
        };
        let schema = sift_protocol::SchemaSnapshot::empty(SchemaScope::shallow());
        let mut mock =
            sift_driver_api::mock::MockDriver::builder().engine(sift_protocol::Engine::Postgres);
        for _ in 0..32 {
            mock = mock
                .ping_ok(server_info.clone())
                .schema_ok(schema.clone())
                .execute_ok(demo_execute_pages());
        }
        let mock = mock.build();
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

fn build_metadata_store(cfg: &Config) -> anyhow::Result<Option<MetadataStore>> {
    if !cfg.metadata.enabled {
        return Ok(None);
    }

    let path = cfg
        .metadata
        .path
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(MetadataStore::default_local_path);
    let secrets = build_secret_store(cfg, &path)?;
    let store = MetadataStore::open(&path, secrets)
        .with_context(|| format!("opening metadata store: {}", path.display()))?;
    if cfg.metadata.bootstrap_local {
        let display_name = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "local".to_string());
        store
            .bootstrap_local(&display_name)
            .context("bootstrapping local metadata principal")?;
    }
    Ok(Some(store))
}

fn build_secret_store(
    cfg: &Config,
    metadata_path: &std::path::Path,
) -> anyhow::Result<Arc<dyn sift_metadata::SecretStore>> {
    match cfg.metadata.secret_backend.as_str() {
        "memory" => Ok(Arc::new(MemorySecretStore::new())),
        "file" => {
            let key_file = cfg.metadata.secret_key_file.as_deref().context(
                "metadata.secret_backend = \"file\" requires metadata.secret_key_file \
                 (e.g. SIFT_METADATA__SECRET_KEY_FILE)",
            )?;
            let secrets_path = metadata_path
                .parent()
                .map(|dir| dir.join("secrets.enc"))
                .unwrap_or_else(|| PathBuf::from("secrets.enc"));
            let store = FileSecretStore::open(&secrets_path, key_file)
                .with_context(|| format!("opening encrypted secret store: {}", secrets_path.display()))?;
            Ok(Arc::new(store))
        }
        "keychain" => build_keychain_store(),
        other => bail!(
            "unsupported metadata.secret_backend `{other}`; expected `memory`, `file`, or `keychain`"
        ),
    }
}

#[cfg(feature = "os-keychain")]
fn build_keychain_store() -> anyhow::Result<Arc<dyn sift_metadata::SecretStore>> {
    Ok(Arc::new(sift_metadata::OsKeychainSecretStore::new()))
}

#[cfg(not(feature = "os-keychain"))]
fn build_keychain_store() -> anyhow::Result<Arc<dyn sift_metadata::SecretStore>> {
    bail!("metadata.secret_backend = \"keychain\" requires building sift-server with the `os-keychain` feature")
}

fn demo_execute_pages() -> Vec<sift_protocol::Page> {
    use sift_protocol::{ColumnMetadata, Nullability, Page, PrimitiveType, Row, TypeRef, Value};

    vec![
        Page::NextResult {
            columns: vec![
                ColumnMetadata {
                    name: "id".to_string(),
                    type_ref: TypeRef::Primitive(PrimitiveType::Int32),
                    nullable: Nullability::NotNullable,
                    auto_increment: false,
                    primary_key: false,
                    facets: Default::default(),
                },
                ColumnMetadata {
                    name: "name".to_string(),
                    type_ref: TypeRef::Primitive(PrimitiveType::Text),
                    nullable: Nullability::NotNullable,
                    auto_increment: false,
                    primary_key: false,
                    facets: Default::default(),
                },
            ],
        },
        Page::Rows {
            rows: vec![
                Row::new(vec![Value::Int32(1), Value::Text("demo alice".into())]),
                Row::new(vec![Value::Int32(2), Value::Text("demo bob".into())]),
            ],
        },
        Page::Done {
            affected_rows: Some(2),
            warnings: Vec::new(),
        },
    ]
}

/// Drives the ADR-018 graceful-shutdown sequence. Resolving this future is
/// what tells axum to stop the listener, so we hold it open through the drain
/// window: on signal we flip the drain gate (new work is refused) and wait for
/// in-flight queries to finish, bounded by `drain_deadline`, before returning.
async fn shutdown_sequence(shutdown: Shutdown, drain_deadline: std::time::Duration) {
    wait_for_signal().await;
    tracing::info!("shutdown signal received; draining");
    shutdown.begin_drain();
    let remaining = shutdown.await_drain(drain_deadline).await;
    if remaining > 0 {
        tracing::warn!(
            remaining,
            "drain deadline elapsed with queries still in flight; abandoning them"
        );
    } else {
        tracing::info!("in-flight queries drained cleanly");
    }
}

async fn wait_for_signal() {
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
}

// Re-export to satisfy the `SchemaScope::shallow()` call above without
// pulling it into the local scope via a `use` (keeps the registry function
// visually focused on driver wiring).
use sift_protocol::SchemaScope;
