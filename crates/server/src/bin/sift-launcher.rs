//! Lifecycle owner for restart-activated signed releases (ADR-015).

use anyhow::{bail, Context};
use sift_server::config::{Config, DeploymentPolicy, RuntimeMode, Transport};
use sift_server::updater::{InstalledRelease, Updater};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::{Child, Command};

#[cfg(windows)]
#[path = "sift-launcher/windows_job.rs"]
mod windows_job;

const ACTIVATION_DEADLINE: Duration = Duration::from_secs(20);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    #[cfg(windows)]
    windows_job::own_descendants().context("establishing launcher process ownership")?;
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|argument| argument == "remote") {
        bail!("sift-launcher owns serving lifecycles only; invoke remote agent commands directly");
    }
    let personal_desktop = take_personal_desktop_flag(&mut args)?;
    let mut config = sift_server::config::load().context("loading launcher config")?;
    if let Some(mode) = mode_override(&args)? {
        config.mode = mode;
    }
    config.validate().context("validating launcher config")?;
    let migrate = automatic_migration_policy(&config, personal_desktop)?;

    let bundled = sibling_server()?;
    if !config.updater.enabled {
        apply_automatic_migrations(&bundled, migrate).await?;
        return supervise(spawn_server(&bundled, &args)?).await;
    }
    if config.mode == RuntimeMode::Container {
        bail!("container lifecycle is owned by the image orchestrator");
    }

    let updater = Updater::from_config(&config)?;
    let pending = updater.pending_release()?;
    if let Some(candidate) = pending {
        let activation = match apply_automatic_migrations(&candidate.executable, migrate).await {
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
    apply_automatic_migrations(&selected, migrate).await?;
    supervise(spawn_server(&selected, &args)?).await
}

fn take_personal_desktop_flag(args: &mut Vec<String>) -> anyhow::Result<bool> {
    let count = args
        .iter()
        .filter(|arg| *arg == "--personal-desktop")
        .count();
    if count > 1 {
        bail!("--personal-desktop may be specified only once");
    }
    args.retain(|arg| arg != "--personal-desktop");
    Ok(count == 1)
}

fn automatic_migration_policy(config: &Config, personal_desktop: bool) -> anyhow::Result<bool> {
    if personal_desktop {
        let bind: std::net::SocketAddr = config.bind.parse().context("parsing desktop bind")?;
        if config.deployment != DeploymentPolicy::Personal
            || config.mode != RuntimeMode::Daemon
            || config.transport != Transport::Loopback
            || !bind.ip().is_loopback()
        {
            bail!("--personal-desktop requires a personal loopback daemon");
        }
        return Ok(config.metadata.enabled);
    }
    Ok(config.metadata.enabled
        && config.deployment == DeploymentPolicy::Personal
        && config.mode == RuntimeMode::InProcess)
}

async fn apply_automatic_migrations(executable: &Path, enabled: bool) -> anyhow::Result<()> {
    if !enabled {
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
    let sdk = sift_client_sdk::Client::new(&base_url);
    let deadline = tokio::time::Instant::now() + ACTIVATION_DEADLINE;
    loop {
        if let Some(status) = child.try_wait()? {
            bail!("candidate exited before readiness with {status}");
        }
        let ready = matches!(
            tokio::time::timeout_at(deadline, sdk.ready()).await,
            Ok(Ok(readiness)) if readiness.ready
        );
        if ready {
            if let Ok(Ok(handshake)) =
                tokio::time::timeout_at(deadline, sdk.probe_handshake()).await
            {
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
    fn desktop_flag_is_consumed_before_forwarding_server_arguments() {
        let mut args = vec![
            "--personal-desktop".into(),
            "--mode".into(),
            "daemon".into(),
        ];
        assert!(take_personal_desktop_flag(&mut args).unwrap());
        assert_eq!(args, ["--mode", "daemon"]);
        assert_eq!(mode_override(&args).unwrap(), Some(RuntimeMode::Daemon));
        assert!(take_personal_desktop_flag(&mut vec![
            "--personal-desktop".into(),
            "--personal-desktop".into()
        ])
        .is_err());
    }

    #[test]
    fn desktop_migrations_require_explicit_local_personal_authority() {
        let desktop = Config {
            mode: RuntimeMode::Daemon,
            ..Config::default()
        };
        assert!(!automatic_migration_policy(&desktop, false).unwrap());
        assert!(automatic_migration_policy(&desktop, true).unwrap());
        for other in [
            Config {
                deployment: DeploymentPolicy::Team,
                ..desktop.clone()
            },
            Config {
                transport: Transport::Network,
                ..desktop.clone()
            },
            Config {
                transport: Transport::SshProxy,
                ..desktop.clone()
            },
            Config {
                bind: "0.0.0.0:7474".into(),
                ..desktop.clone()
            },
            Config {
                mode: RuntimeMode::Container,
                ..desktop.clone()
            },
        ] {
            assert!(automatic_migration_policy(&other, true).is_err());
            assert!(!automatic_migration_policy(&other, false).unwrap());
        }
        assert!(automatic_migration_policy(&Config::default(), false).unwrap());
        let mut disabled = desktop;
        disabled.metadata.enabled = false;
        assert!(!automatic_migration_policy(&disabled, true).unwrap());
        disabled.mode = RuntimeMode::InProcess;
        assert!(!automatic_migration_policy(&disabled, false).unwrap());
    }

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

    #[tokio::test]
    async fn candidate_health_requires_ready_and_a_matching_handshake() {
        use sift_driver_api::mock::MockDriver;
        use sift_server::http::{app, AppState, AuthState};
        use sift_server::{DriverRegistry, RoomRuntime, SessionStore, Shutdown};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = app(AppState {
            sessions: SessionStore::new(
                DriverRegistry::builder()
                    .register(MockDriver::builder().build())
                    .build(),
            ),
            rooms: RoomRuntime::default(),
            auth: AuthState::default(),
            metadata: None,
            shutdown: Shutdown::default(),
        });
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut child = if cfg!(windows) {
            Command::new("powershell.exe")
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "Start-Sleep -Seconds 30",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap()
        } else {
            Command::new("sleep").arg("30").spawn().unwrap()
        };
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
