//! `sift-server` binary entry point. Loads config, initialises tracing,
//! builds the driver registry, binds the HTTP server. Local-first
//! (ADR-010): same binary runs in-process alongside the desktop client or
//! as a standalone daemon.

use anyhow::Context;
use sift_server::{
    config::{load as load_config, Config, RuntimeMode},
    http::{app, AppState},
    metadata_runtime::build_metadata_store,
    registry::DriverRegistry,
    room_runtime::RoomRuntime,
    session::SessionStore,
    Shutdown,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let command = parse_command(std::env::args().skip(1))?;
    let cfg = match command {
        ServerCommand::Serve { mode } => {
            let mut config = load_config().context("loading config")?;
            if let Some(mode) = mode {
                config.mode = mode;
            }
            config
        }
        ServerCommand::RemoteDaemon { state_dir } => {
            let config = sift_server::remote_agent::prepare_remote_config(&state_dir)?;
            let metadata =
                build_metadata_store(&config)?.context("remote daemon metadata is disabled")?;
            metadata
                .ensure_auth_system_keys()
                .await
                .context("provisioning remote authentication keys")?;
            config
        }
        ServerCommand::RemoteProbe { state_dir } => {
            let response = sift_server::remote_agent::probe(&state_dir)?;
            sift_server::remote_agent::write_json(&response)?;
            return Ok(());
        }
        ServerCommand::RemoteChallenge {
            state_dir,
            fingerprint,
        } => {
            let response = sift_server::remote_agent::challenge(&state_dir, &fingerprint)?;
            sift_server::remote_agent::write_json(&response)?;
            return Ok(());
        }
        ServerCommand::RemoteIssue { state_dir, proof } => {
            let response = sift_server::remote_agent::issue_capability(
                &state_dir,
                proof
                    .as_ref()
                    .map(|proof| (proof.nonce.as_str(), proof.signature.as_str())),
            )
            .await?;
            sift_server::remote_agent::write_json(&response)?;
            return Ok(());
        }
        ServerCommand::RemoteInstall {
            state_dir,
            destination,
            sha256,
        } => {
            let response =
                sift_server::remote_agent::install_uploaded(&state_dir, &destination, &sha256)?;
            sift_server::remote_agent::write_json(&response)?;
            return Ok(());
        }
    };
    cfg.validate().context("validating config")?;
    let mut runtime =
        sift_server::runtime::RuntimeState::acquire(&cfg).context("acquiring runtime state")?;
    init_tracing(&cfg);
    sift_server::updater::spawn_background(&cfg).context("starting signed background updater")?;

    tracing::info!(
        version = sift_server::VERSION,
        bind = %cfg.bind,
        mode = ?cfg.mode,
        instance_id = %runtime.instance_id,
        daemon_generation = %runtime.daemon_generation,
        "sift-server starting"
    );

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
    // Wire ADR-011 cursor registry config.
    {
        let mut cursor_cfg = sessions.cursor_registry().config();
        cursor_cfg.max_per_session = cfg.limits.max_cursors_per_session;
        cursor_cfg.prefetch_pages = cfg.limits.cursor_prefetch_pages.max(1);
        cursor_cfg.spill_dir = cfg
            .limits
            .cursor_spill_dir
            .as_ref()
            .filter(|s| !s.is_empty())
            .map(std::path::PathBuf::from);
        cursor_cfg.spill_ttl = std::time::Duration::from_secs(cfg.limits.cursor_spill_ttl_secs);
        sessions.cursor_registry().set_config(cursor_cfg);
    }
    // Wire schema cache config.
    {
        let mut cfg2 = sessions.schema_cache().config();
        cfg2.ttl = std::time::Duration::from_secs(cfg.limits.schema_cache_ttl_secs);
        cfg2.mssql_poll_interval =
            std::time::Duration::from_secs(cfg.limits.schema_mssql_poll_secs);
        sessions.schema_cache().set_config(cfg2);
    }
    // Periodic reaper for expired spill files. Ticks at spill_ttl/6
    // (min 30s) so an abandoned file is closed within roughly 20% of
    // its TTL past deadline.
    {
        let registry = sessions.cursor_registry().clone();
        let ttl_secs = cfg.limits.cursor_spill_ttl_secs.max(60);
        let tick = std::time::Duration::from_secs((ttl_secs / 6).max(30));
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tick);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                let reaped = registry.reap_expired_spills();
                if reaped > 0 {
                    tracing::debug!(reaped, "cursor spill reaper");
                }
            }
        });
    }
    let metadata = build_metadata_store(&cfg)?;
    sessions.set_resource_manager(sift_server::resources::ResourceManager::new(
        &cfg.tenant_limits,
        metadata.clone(),
    ));
    if let Some(store) = &metadata {
        sessions.set_audit_store(store.clone());
        sessions.set_authorization_store(store.clone());
        let package_registry = sift_plugin_host::ExtensionPackageRegistry::new(
            cfg.runtime_state_dir().join("extensions"),
            sift_plugin_host::PackageLimits::default(),
            store.clone(),
        );
        for path in &cfg.extensions.development_overrides {
            package_registry.register_development_override(
                std::path::Path::new(path),
                cfg.deployment == sift_server::config::DeploymentPolicy::Team,
                cfg.extensions.allow_hosted_development,
            )?;
        }
        let package_registry = std::sync::Arc::new(package_registry);
        sessions.set_package_registry(package_registry);
        let metadata = std::sync::Arc::new(store.clone());
        let dispatcher =
            sift_server::extension_dispatch::ExtensionOperationDispatcher::new(metadata.clone());
        sessions.set_tool_registry(sift_server::automation::GovernedToolRegistry::new(
            dispatcher,
            metadata,
            sift_server::automation::ToolApprovalPolicy::default(),
        ));
        sessions.refresh_extension_runtimes().await?;
    }
    let shutdown = Shutdown::default();
    let state = AppState {
        sessions,
        rooms: RoomRuntime::default(),
        auth: sift_server::http::AuthState {
            bearer_token: cfg.auth.bearer_token.clone(),
            loopback_bypass: cfg.auth.loopback_bypass,
            deployment: cfg.deployment,
            transport: cfg.transport,
            runtime_mode: cfg.mode,
            instance_audience: cfg
                .auth
                .public_base_url
                .clone()
                .unwrap_or_else(|| format!("sift:instance:{}", runtime.instance_id)),
            instance_id: runtime.instance_id.clone(),
            daemon_generation: runtime.daemon_generation.clone(),
            allow_legacy_unversioned: false,
            rate_limiter: sift_server::rate_limit::RateLimiter::from_config(&cfg.rate_limits),
            github: match (
                cfg.auth.github_client_id.clone(),
                cfg.auth.github_client_secret.clone(),
                cfg.auth.public_base_url.clone(),
            ) {
                (Some(client_id), Some(client_secret), Some(public_base_url)) => {
                    Some(sift_server::identity::GithubOAuthConfig {
                        client_id,
                        client_secret,
                        public_base_url,
                        http: reqwest::Client::new(),
                    })
                }
                _ => None,
            },
            ..Default::default()
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
    let local_addr = listener.local_addr().context("reading bound address")?;
    runtime
        .publish_daemon(local_addr)
        .context("publishing daemon readiness")?;
    tracing::info!("listening on http://{local_addr}");

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

#[derive(Debug, PartialEq, Eq)]
enum ServerCommand {
    Serve {
        mode: Option<RuntimeMode>,
    },
    RemoteDaemon {
        state_dir: std::path::PathBuf,
    },
    RemoteProbe {
        state_dir: std::path::PathBuf,
    },
    RemoteChallenge {
        state_dir: std::path::PathBuf,
        fingerprint: String,
    },
    RemoteIssue {
        state_dir: std::path::PathBuf,
        proof: Option<RemoteProof>,
    },
    RemoteInstall {
        state_dir: std::path::PathBuf,
        destination: std::path::PathBuf,
        sha256: String,
    },
}

#[derive(Debug, PartialEq, Eq)]
struct RemoteProof {
    nonce: String,
    signature: String,
}

fn parse_command(args: impl IntoIterator<Item = String>) -> anyhow::Result<ServerCommand> {
    let mut args = args.into_iter();
    let Some(first) = args.next() else {
        return Ok(ServerCommand::Serve { mode: None });
    };
    if first != "remote" {
        let mut mode = None;
        let mut arguments = std::iter::once(first).chain(args);
        while let Some(argument) = arguments.next() {
            let value = if argument == "--mode" {
                Some(
                    arguments
                        .next()
                        .context("--mode requires in-process, daemon, or container")?,
                )
            } else {
                argument.strip_prefix("--mode=").map(str::to_owned)
            };
            let Some(value) = value else {
                anyhow::bail!("unknown sift-server argument `{argument}`");
            };
            if mode.is_some() {
                anyhow::bail!("--mode may be specified only once");
            }
            mode = Some(value.parse().map_err(anyhow::Error::msg)?);
        }
        return Ok(ServerCommand::Serve { mode });
    }

    let action = args
        .next()
        .context("remote requires one of: daemon, probe, challenge, issue, install")?;
    let mut state_dir = sift_server::remote_agent::default_remote_state_dir();
    let mut fingerprint = None;
    let mut nonce = None;
    let mut signature = None;
    let mut destination = None;
    let mut sha256 = None;
    while let Some(argument) = args.next() {
        let value = args
            .next()
            .with_context(|| format!("{argument} requires a value"))?;
        match argument.as_str() {
            "--state-dir" => state_dir = value.into(),
            "--fingerprint" => fingerprint = Some(value),
            "--nonce" => nonce = Some(value),
            "--signature" => signature = Some(value),
            "--destination" => destination = Some(value.into()),
            "--sha256" => sha256 = Some(value),
            _ => anyhow::bail!("unknown remote argument `{argument}`"),
        }
    }
    match action.as_str() {
        "daemon" => Ok(ServerCommand::RemoteDaemon { state_dir }),
        "probe" => Ok(ServerCommand::RemoteProbe { state_dir }),
        "challenge" => Ok(ServerCommand::RemoteChallenge {
            state_dir,
            fingerprint: fingerprint.context("remote challenge requires --fingerprint")?,
        }),
        "issue" => {
            let proof = match (nonce, signature) {
                (Some(nonce), Some(signature)) => Some(RemoteProof { nonce, signature }),
                (None, None) => None,
                _ => anyhow::bail!("remote issue requires both --nonce and --signature"),
            };
            Ok(ServerCommand::RemoteIssue { state_dir, proof })
        }
        "install" => Ok(ServerCommand::RemoteInstall {
            state_dir,
            destination: destination.context("remote install requires --destination")?,
            sha256: sha256.context("remote install requires --sha256")?,
        }),
        _ => anyhow::bail!("unknown remote action `{action}`"),
    }
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
            provider: sift_protocol::Engine::Postgres.provider_ref(env!("CARGO_PKG_VERSION")),
            server_version: "MockDB 0.1".to_string(),
            current_database: "mock".to_string(),
            current_user: "mock".to_string(),
            pool_warm_slots: None,
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

#[cfg(test)]
mod command_tests {
    use super::*;

    fn args<'a>(values: &'a [&'a str]) -> impl Iterator<Item = String> + 'a {
        values.iter().map(|value| (*value).to_string())
    }

    #[test]
    fn parses_serve_and_remote_commands_without_secret_capability_arguments() {
        assert_eq!(
            parse_command(args(&["--mode=container"])).unwrap(),
            ServerCommand::Serve {
                mode: Some(RuntimeMode::Container)
            }
        );
        assert_eq!(
            parse_command(args(&[
                "remote",
                "issue",
                "--state-dir",
                "state",
                "--nonce",
                "nonce",
                "--signature",
                "signature"
            ]))
            .unwrap(),
            ServerCommand::RemoteIssue {
                state_dir: "state".into(),
                proof: Some(RemoteProof {
                    nonce: "nonce".into(),
                    signature: "signature".into()
                })
            }
        );
        assert!(parse_command(args(&["remote", "issue", "--capability", "secret"])).is_err());
    }
}
