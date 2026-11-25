use anyhow::{ensure, Context, Result};
use sift_desktop_updater::{Config, Release, Updater, RESTART_EXIT_CODE};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::process::{Child, Command};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if run().await.is_err() {
        eprintln!(
            "sift-desktop-launcher: update or launch failed; installed package was not changed"
        );
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    ensure!(
        args.next().as_deref() == Some(std::ffi::OsStr::new("--config")),
        "expected --config"
    );
    let config_path = PathBuf::from(args.next().context("missing configuration")?);
    let action = args.next().context("missing action")?;
    let updater = Updater::new(Config::load(&config_path)?)?;
    match action.to_str() {
        Some("check") => {
            ensure!(args.next().is_none(), "unexpected check argument");
            let result = updater.check().await;
            let status = updater.publish_status(
                result
                    .as_ref()
                    .err()
                    .map(|_| "Update check failed; current desktop remains available".into()),
            )?;
            result?;
            println!("{}", serde_json::to_string(&status)?);
            Ok(())
        }
        Some("status") => {
            ensure!(args.next().is_none(), "unexpected status argument");
            println!(
                "{}",
                serde_json::to_string(&sift_desktop_updater::read_status(
                    &updater.config.state_dir.join("status.json")
                )?)?
            );
            Ok(())
        }
        Some("run") => {
            let args = args.collect::<Vec<_>>();
            ensure!(
                args.first().is_some_and(|arg| arg == "--"),
                "desktop arguments must follow --"
            );
            supervise(updater, args[1..].to_vec()).await
        }
        _ => anyhow::bail!("unknown launcher action"),
    }
}

async fn supervise(updater: Updater, args: Vec<OsString>) -> Result<()> {
    let _run = updater.lock("run")?;
    let checks = updater.clone();
    let checker = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(300));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let result = checks.check().await;
            let _ = checks.publish_status(
                result
                    .err()
                    .map(|_| "Update check failed; current desktop remains available".into()),
            );
        }
    });
    let outcome = async {
        loop {
            let candidate = updater.claim();
            let mut child = match candidate {
                Ok(Some(release)) => match start_candidate(&updater, &release, &args).await {
                    Ok(child) => child,
                    Err(_) => {
                        updater.settle(&release.digest, false)?;
                        let _ = updater.publish_status(Some(
                            "New desktop failed startup; previous version retained".into(),
                        ));
                        start(&updater.current_directory()?, &args, &updater)?
                    }
                },
                Ok(None) => start(&updater.current_directory()?, &args, &updater)?,
                Err(_) => {
                    // Claim persists before verification, so tampered candidates cannot loop.
                    let attempting = updater.pointers()?.attempting;
                    if let Some(digest) = attempting {
                        updater.settle(&digest, false)?;
                    }
                    let _ = updater.publish_status(Some(
                        "New desktop could not be verified; previous version retained".into(),
                    ));
                    start(&updater.current_directory()?, &args, &updater)?
                }
            };
            let status = wait_for_exit(&mut child).await?;
            if status.code() != Some(RESTART_EXIT_CODE) {
                break;
            }
        }
        Ok(())
    }
    .await;
    checker.abort();
    let _ = checker.await;
    outcome
}

fn command(directory: &Path, args: &[OsString], updater: &Updater) -> Command {
    let mut command = Command::new(directory.join("sift-desktop"));
    command
        .args(args)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .env(
            "SIFT_DESKTOP_UPDATE_STATUS",
            updater.config.state_dir.join("status.json"),
        )
        .env_remove("SIFT_DESKTOP_UPDATE_READY_FILE")
        .env_remove("SIFT_DESKTOP_UPDATE_READY_NONCE");
    #[cfg(target_os = "linux")]
    unsafe {
        let parent = libc::getpid();
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                return Err(std::io::Error::other(
                    "desktop launcher exited during spawn",
                ));
            }
            Ok(())
        });
    }
    command
}

fn start(directory: &Path, args: &[OsString], updater: &Updater) -> Result<Child> {
    Ok(command(directory, args, updater).spawn()?)
}

async fn start_candidate(updater: &Updater, release: &Release, args: &[OsString]) -> Result<Child> {
    let probes = updater.config.state_dir.join("probes");
    sift_desktop_updater::private_dir(&probes)?;
    let receipt = probes.join(format!("{}.json", uuid::Uuid::new_v4()));
    let nonce = uuid::Uuid::new_v4().to_string();
    let mut child = command(&release.directory, args, updater)
        .env("SIFT_DESKTOP_UPDATE_READY_FILE", &receipt)
        .env("SIFT_DESKTOP_UPDATE_READY_NONCE", &nonce)
        .spawn()?;
    let ready = await_startup(&mut child, &receipt, &nonce, release).await;
    let _ = std::fs::remove_file(&receipt);
    if ready.is_err() {
        stop(&mut child).await;
        ready?;
    }
    updater.settle(&release.digest, true)?;
    let _ = updater.publish_status(None);
    Ok(child)
}

async fn await_startup(
    child: &mut Child,
    path: &Path,
    nonce: &str,
    release: &Release,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        ensure!(
            child.try_wait()?.is_none(),
            "candidate exited before startup acknowledgement"
        );
        if path.try_exists()? {
            return sift_desktop_updater::read_startup(path, nonce, release);
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "candidate startup timed out"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn stop(child: &mut Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
    }
    if tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .is_err()
    {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
}

async fn wait_for_exit(child: &mut Child) -> Result<std::process::ExitStatus> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            status = child.wait() => return Ok(status?),
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
        stop(child).await;
        Ok(child.wait().await?)
    }
    #[cfg(not(unix))]
    {
        Ok(child.wait().await?)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt as _, BufReader};

    fn release(root: &Path) -> Release {
        let manifest = serde_json::from_value(serde_json::json!({
            "schema_version": 1, "product": "sift-desktop", "channel": "development",
            "sequence": 1, "version": "0.1.0-dev.1", "source_commit": "a".repeat(40),
            "published_at": "2026-01-01T00:00:00Z", "expires_at": "2030-01-01T00:00:00Z",
            "minimum_launcher_version": 1, "protocol": {"minimum": 5, "maximum": 5}, "targets": []
        }))
        .unwrap();
        Release {
            digest: "a".repeat(64),
            manifest,
            directory: root.into(),
        }
    }

    async fn waiting_child() -> Child {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "printf 'ready\\n'; read -r finish"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .await
            .unwrap();
        assert_eq!(line, "ready\n");
        child
    }

    #[tokio::test]
    async fn startup_accepts_only_the_live_owned_candidate_with_its_receipt() {
        let root = tempfile::tempdir().unwrap();
        let release = release(root.path());
        let receipt = root.path().join("ready.json");
        sift_desktop_updater::atomic_json(
            &receipt,
            &sift_desktop_updater::StartupReceipt {
                nonce: "attempt".into(),
                source_commit: release.manifest.source_commit.clone(),
                protocol: 5,
            },
        )
        .unwrap();
        let mut child = waiting_child().await;
        await_startup(&mut child, &receipt, "attempt", &release)
            .await
            .unwrap();
        assert!(
            await_startup(&mut child, &receipt, "stale-attempt", &release)
                .await
                .is_err()
        );
        stop(&mut child).await;
        assert!(await_startup(&mut child, &receipt, "attempt", &release)
            .await
            .is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn missing_startup_receipt_has_a_bounded_deadline() {
        let root = tempfile::tempdir().unwrap();
        let mut child = waiting_child().await;
        let error = await_startup(
            &mut child,
            &root.path().join("missing.json"),
            "attempt",
            &release(root.path()),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        stop(&mut child).await;
    }
}
