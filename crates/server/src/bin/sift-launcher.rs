//! Lifecycle owner for restart-activated signed releases (ADR-015).

use anyhow::{bail, Context};
use sift_server::config::{Config, DeploymentPolicy, RuntimeMode};
use sift_server::updater::{InstalledRelease, Updater};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::{Child, Command};

const ACTIVATION_DEADLINE: Duration = Duration::from_secs(20);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|argument| argument == "remote") {
        bail!("sift-launcher owns serving lifecycles only; invoke remote agent commands directly");
    }
    let mut config = sift_server::config::load().context("loading launcher config")?;
    if let Some(mode) = mode_override(&args)? {
        config.mode = mode;
    }
    config.validate().context("validating launcher config")?;

    let bundled = sibling_server()?;
    if !config.updater.enabled {
        apply_automatic_migrations(&bundled, &config).await?;
        return supervise(spawn_server(&bundled, &args)?).await;
    }
    if config.mode == RuntimeMode::Container {
        bail!("container lifecycle is owned by the image orchestrator");
    }

    let updater = Updater::from_config(&config)?;
    let pending = updater.pending_release()?;
    if let Some(candidate) = pending {
        let activation = match apply_automatic_migrations(&candidate.executable, &config).await {
            Ok(()) => match spawn_server(&candidate.executable, &args) {
                Ok(mut child) => {
                    match await_candidate_health(&mut child, &config, &candidate).await {
                        Ok(()) => Ok(child),
                        Err(error) => {
                            let _ = child.kill().await;
                            let _ = child.wait().await;
                            Err(error)
                        }
                    }
                }
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        };
        let child = match activation {
            Ok(child) => {
                updater.commit_healthy_candidate()?;
                eprintln!(
                    "sift: activated signed release {} after readiness and protocol handshake",
                    candidate.release_version
                );
                child
            }
            Err(error) => {
                updater.rollback_candidate()?;
                eprintln!(
                    "sift: candidate {} failed activation and was rolled back: {error:#}",
                    candidate.release_version
                );
                let fallback = updater
                    .current_release()?
                    .map(|release| release.executable)
                    .filter(|path| path.is_file())
                    .unwrap_or(bundled);
                spawn_server(&fallback, &args)?
            }
        };
        return supervise(child).await;
    }

    let selected = updater
        .current_release()?
        .map(|release| release.executable)
        .filter(|path| path.is_file())
        .unwrap_or(bundled);
    apply_automatic_migrations(&selected, &config).await?;
    supervise(spawn_server(&selected, &args)?).await
}

async fn apply_automatic_migrations(executable: &Path, config: &Config) -> anyhow::Result<()> {
    if config.deployment != DeploymentPolicy::Personal || config.mode != RuntimeMode::InProcess {
        return Ok(());
    }
    if !executable.is_file() {
        bail!(
            "selected sift-server executable is missing: {}",
            executable.display()
        );
    }
    let status = Command::new(executable)
        .args(["migrate", "apply", "--automatic"])
        .env("SIFT_MODE", "in-process")
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await
        .with_context(|| format!("running metadata migration with {}", executable.display()))?;
    if !status.success() {
        bail!("metadata migration exited with {status}");
    }
    Ok(())
}

fn probe_address(bind: std::net::SocketAddr) -> std::net::SocketAddr {
    match bind {
        std::net::SocketAddr::V4(mut address) if address.ip().is_unspecified() => {
            address.set_ip(std::net::Ipv4Addr::LOCALHOST);
            address.into()
        }
        std::net::SocketAddr::V6(mut address) if address.ip().is_unspecified() => {
            address.set_ip(std::net::Ipv6Addr::LOCALHOST);
            address.into()
        }
        address => address,
    }
}

fn sibling_server() -> anyhow::Result<PathBuf> {
    Ok(std::env::current_exe()?
        .parent()
        .context("sift-launcher executable has no parent directory")?
        .join(if cfg!(windows) {
            "sift-server.exe"
        } else {
            "sift-server"
        }))
}

fn spawn_server(executable: &Path, args: &[String]) -> anyhow::Result<Child> {
    if !executable.is_file() {
        bail!(
            "selected sift-server executable is missing: {}",
            executable.display()
        );
    }
    Command::new(executable)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("starting {}", executable.display()))
}

async fn await_candidate_health(
    child: &mut Child,
    config: &Config,
    candidate: &InstalledRelease,
) -> anyhow::Result<()> {
    let bind = probe_address(config.bind.parse()?);
    if bind.port() == 0 {
        bail!("launcher activation requires a configured nonzero bind port");
    }
    let base_url = format!("http://{bind}");
    let readiness_url = format!("{base_url}/ready");
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?;
    let sdk = sift_client_sdk::Client::new(&base_url);
    let deadline = tokio::time::Instant::now() + ACTIVATION_DEADLINE;
    loop {
        if let Some(status) = child.try_wait()? {
            bail!("candidate exited before readiness with {status}");
        }
        if let Ok(response) = http.get(&readiness_url).send().await {
            if response.status().is_success() {
                if let Ok(handshake) = sdk.connect().await {
                    if handshake.server_version != candidate.release_version {
                        bail!(
                            "candidate reported release {}, expected {}",
                            handshake.server_version,
                            candidate.release_version
                        );
                    }
                    return Ok(());
                }
            }
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("candidate missed the 20-second readiness deadline");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn supervise(mut child: Child) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            status = child.wait() => return status_result(status?),
            signal = tokio::signal::ctrl_c() => {
                signal?;
                forward_unix_signal(&child, libc::SIGINT)?;
            }
            _ = terminate.recv() => {
                forward_unix_signal(&child, libc::SIGTERM)?;
            }
        }
        return status_result(child.wait().await?);
    }
    #[cfg(not(unix))]
    {
        tokio::select! {
            status = child.wait() => status_result(status?),
            signal = tokio::signal::ctrl_c() => {
                signal?;
                child.kill().await?;
                status_result(child.wait().await?)
            }
        }
    }
}

#[cfg(unix)]
fn forward_unix_signal(child: &Child, signal: libc::c_int) -> anyhow::Result<()> {
    let pid = child.id().context("server child has no process id")?;
    // SAFETY: `pid` is the live child returned by Tokio and `signal` is one
    // of the platform constants selected above.
    if unsafe { libc::kill(pid as libc::pid_t, signal) } == -1 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

fn status_result(status: std::process::ExitStatus) -> anyhow::Result<()> {
    if status.success() {
        Ok(())
    } else {
        bail!("sift-server exited with {status}")
    }
}

fn mode_override(args: &[String]) -> anyhow::Result<Option<RuntimeMode>> {
    let mut mode = None;
    let mut arguments = args.iter();
    while let Some(argument) = arguments.next() {
        let value = if argument == "--mode" {
            Some(
                arguments
                    .next()
                    .context("--mode requires in-process, daemon, or container")?
                    .as_str(),
            )
        } else {
            argument.strip_prefix("--mode=")
        };
        let Some(value) = value else {
            bail!("unknown sift-launcher argument `{argument}`");
        };
        if mode.is_some() {
            bail!("--mode may be specified only once");
        }
        mode = Some(value.parse().map_err(anyhow::Error::msg)?);
    }
    Ok(mode)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launcher_mode_parser_matches_server_syntax() {
        assert_eq!(
            mode_override(&["--mode=daemon".into()]).unwrap(),
            Some(RuntimeMode::Daemon)
        );
        assert_eq!(
            mode_override(&["--mode".into(), "container".into()]).unwrap(),
            Some(RuntimeMode::Container)
        );
        assert!(mode_override(&["remote".into(), "probe".into()]).is_err());
    }

    #[test]
    fn wildcard_bind_is_probed_through_loopback() {
        assert_eq!(
            probe_address("0.0.0.0:7474".parse().unwrap()),
            "127.0.0.1:7474".parse().unwrap()
        );
        assert_eq!(
            probe_address("[::]:7474".parse().unwrap()),
            "[::1]:7474".parse().unwrap()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn candidate_health_requires_ready_and_a_matching_handshake() {
        use axum::routing::{get, post};
        use axum::{Json, Router};
        use sift_protocol::{
            HandshakeDeployment, HandshakeResponse, HandshakeRuntimeMode, HandshakeTransport,
            ProtocolRange,
        };

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/ready", get(|| async { "ready" }))
            .route(
                "/v1/handshake",
                post(|| async {
                    let response = HandshakeResponse {
                        server_version: sift_server::VERSION.into(),
                        protocol: ProtocolRange::exact(sift_protocol::PROTOCOL_VERSION_NUMBER),
                        selected_protocol: sift_protocol::PROTOCOL_VERSION_NUMBER,
                        instance_id: "test-instance".into(),
                        daemon_generation: "test-generation".into(),
                        deployment: HandshakeDeployment::Personal,
                        transport: HandshakeTransport::Loopback,
                        runtime_mode: HandshakeRuntimeMode::InProcess,
                        capabilities: vec![],
                    };
                    (
                        [("x-sift-protocol-version", sift_protocol::PROTOCOL_VERSION)],
                        Json(response),
                    )
                }),
            );
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let config = Config {
            bind: address.to_string(),
            ..Config::default()
        };
        let candidate = InstalledRelease {
            release_version: sift_server::VERSION.into(),
            sequence: 1,
            target: "test".into(),
            sha256: "00".repeat(32),
            executable: PathBuf::from("unused"),
        };
        await_candidate_health(&mut child, &config, &candidate)
            .await
            .unwrap();
        child.kill().await.unwrap();
        server.abort();
    }
}
