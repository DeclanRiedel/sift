//! Local OpenSSH bootstrap and byte-forwarding helper (ADR-021).

use anyhow::{bail, Context};
use base64::Engine as _;
use ed25519_dalek::{Signer as _, SigningKey};
use sha2::{Digest as _, Sha256};
use sift_protocol::{
    RemoteCapabilityResponse, RemoteKeyChallenge, RemoteProbeResponse, RemoteReady,
};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::process::Command;
use tokio::sync::Mutex;

const MAX_AGENT_OUTPUT: usize = 64 * 1024;

#[derive(Debug)]
struct Options {
    destination: String,
    state_dir: String,
    remote_binary: String,
    remote_binary_explicit: bool,
    local_server_binary: Option<PathBuf>,
    local_update_candidate: bool,
    sift_key_file: Option<PathBuf>,
    local_port: u16,
}

#[derive(Clone)]
struct SshSession {
    destination: Arc<str>,
    control_socket: Arc<PathBuf>,
    control_guard: Arc<Mutex<()>>,
    multiplex: Arc<AtomicBool>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let options = parse_options(std::env::args().skip(1))?;
    validate_remote_path(&options.state_dir)?;
    validate_remote_path(&options.remote_binary)?;
    let control_dir =
        std::env::temp_dir().join(format!("sift-remote-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir(&control_dir).context("creating SSH control directory")?;
    make_private_dir(&control_dir)?;
    let session = SshSession {
        destination: options.destination.clone().into(),
        control_socket: Arc::new(control_dir.join("control")),
        control_guard: Arc::new(Mutex::new(())),
        multiplex: Arc::new(AtomicBool::new(true)),
    };

    session.ensure_master().await?;
    let result = run(options, session.clone()).await;
    session.close_master().await;
    let _ = std::fs::remove_dir_all(&control_dir);
    result
}

async fn run(mut options: Options, session: SshSession) -> anyhow::Result<()> {
    if options.local_server_binary.is_none() {
        let (local, pending) = select_local_server(&options).await?;
        options.local_server_binary = Some(local);
        options.local_update_candidate = pending;
    }
    if !options.remote_binary_explicit {
        let local = options
            .local_server_binary
            .as_ref()
            .context("local server artifact was not selected")?;
        let digest = sha256_file(local)?;
        options.remote_binary = format!(
            ".local/share/sift/bin/{}/{os}-{arch}/{digest}/sift-server",
            sift_server::VERSION,
            os = std::env::consts::OS,
            arch = std::env::consts::ARCH
        );
    }
    validate_remote_path(&options.remote_binary)?;
    let mut probe = match session
        .agent_json::<RemoteProbeResponse>(&[
            &options.remote_binary,
            "remote",
            "probe",
            "--state-dir",
            &options.state_dir,
        ])
        .await
    {
        Ok(probe) => probe,
        Err(_) => {
            install_server(&options, &session).await?;
            session
                .agent_json(&[
                    &options.remote_binary,
                    "remote",
                    "probe",
                    "--state-dir",
                    &options.state_dir,
                ])
                .await?
        }
    };
    if probe
        .protocol
        .highest_common(sift_protocol::ProtocolRange::exact(
            sift_protocol::PROTOCOL_VERSION_NUMBER,
        ))
        .is_none()
    {
        install_server(&options, &session).await?;
        probe = session
            .agent_json(&[
                &options.remote_binary,
                "remote",
                "probe",
                "--state-dir",
                &options.state_dir,
            ])
            .await?;
    }
    if probe.daemon.is_none() {
        start_daemon(&options, &session).await?;
        probe = wait_for_daemon(&options, &session).await?;
    }
    let daemon = probe
        .daemon
        .context("remote daemon did not publish readiness")?;
    let endpoint = daemon.endpoint.clone();
    let listener = TcpListener::bind(("127.0.0.1", options.local_port))
        .await
        .context("binding local remote-proxy listener")?;
    let local_addr = listener.local_addr()?;
    let forward_session = session.clone();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let session = forward_session.clone();
            let endpoint = endpoint.clone();
            tokio::spawn(async move {
                if let Err(error) = forward_connection(session, &endpoint, stream).await {
                    tracing::debug!(%error, "SSH forwarding connection ended");
                }
            });
        }
    });

    let base = format!("http://{local_addr}");
    let capability = issue_capability(&options, &session).await?;
    let client = sift_client_sdk::Client::new(&base);
    let negotiated = client.connect().await?;
    if negotiated.instance_id != capability.daemon.instance_id
        || negotiated.daemon_generation != capability.daemon.daemon_generation
    {
        bail!("forwarded handshake reached a different remote daemon generation");
    }
    let grant = client
        .exchange_ssh_proxy_capability(capability.capability)
        .await?;
    let ready = RemoteReady {
        local_base_url: base,
        access_token: grant.access_token,
        access_expires_at: grant.expires_at,
        principal_id: grant.principal_id,
        instance_id: negotiated.instance_id,
        daemon_generation: negotiated.daemon_generation,
        server_version: negotiated.server_version,
        selected_protocol: negotiated.selected_protocol,
    };
    if options.local_update_candidate {
        let config = sift_server::config::load().context("reloading local update configuration")?;
        let updater = sift_server::updater::Updater::from_config(&config)?;
        let pending = updater
            .pending_release()?
            .context("pending local update candidate disappeared during remote bootstrap")?;
        if Some(&pending.executable) != options.local_server_binary.as_ref() {
            bail!("verified remote server did not match the pending local update candidate");
        }
        updater.commit_healthy_candidate()?;
    }
    write_secret_json(&ready)?;

    // The consuming client watches stdout for replacement grants. Renewal is
    // rooted in a fresh SSH capability rather than a portable refresh token.
    let renewal_options = Arc::new(options);
    let renewal_session = session.clone();
    let renewal_base = ready.local_base_url.clone();
    tokio::spawn(async move {
        let mut expires_at = ready.access_expires_at;
        loop {
            let delay = (expires_at - chrono::Utc::now() - chrono::Duration::minutes(2))
                .to_std()
                .unwrap_or(Duration::from_secs(1));
            tokio::time::sleep(delay).await;
            let renewed = async {
                let capability = issue_capability(&renewal_options, &renewal_session).await?;
                let client = sift_client_sdk::Client::new(&renewal_base);
                let negotiated = client.connect().await?;
                let grant = client
                    .exchange_ssh_proxy_capability(capability.capability)
                    .await?;
                anyhow::Ok(RemoteReady {
                    local_base_url: renewal_base.clone(),
                    access_token: grant.access_token,
                    access_expires_at: grant.expires_at,
                    principal_id: grant.principal_id,
                    instance_id: negotiated.instance_id,
                    daemon_generation: negotiated.daemon_generation,
                    server_version: negotiated.server_version,
                    selected_protocol: negotiated.selected_protocol,
                })
            }
            .await;
            match renewed {
                Ok(ready) => {
                    expires_at = ready.access_expires_at;
                    if write_secret_json(&ready).is_err() {
                        return;
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "SSH access-grant renewal failed");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    });

    tokio::signal::ctrl_c().await?;
    Ok(())
}

async fn issue_capability(
    options: &Options,
    session: &SshSession,
) -> anyhow::Result<RemoteCapabilityResponse> {
    let personal = session
        .agent_json::<RemoteCapabilityResponse>(&[
            &options.remote_binary,
            "remote",
            "issue",
            "--state-dir",
            &options.state_dir,
        ])
        .await;
    if personal.is_ok() || options.sift_key_file.is_none() {
        return personal;
    }

    let key = load_signing_key(options.sift_key_file.as_deref().unwrap())?;
    let public = key.verifying_key().to_bytes();
    let fingerprint = format!(
        "SHA256:{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(public))
    );
    let challenge: RemoteKeyChallenge = session
        .agent_json(&[
            &options.remote_binary,
            "remote",
            "challenge",
            "--state-dir",
            &options.state_dir,
            "--fingerprint",
            &fingerprint,
        ])
        .await?;
    let signature = key.sign(challenge.message.as_bytes());
    let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature.to_bytes());
    session
        .agent_json(&[
            &options.remote_binary,
            "remote",
            "issue",
            "--state-dir",
            &options.state_dir,
            "--nonce",
            &challenge.nonce,
            "--signature",
            &signature,
        ])
        .await
}

async fn wait_for_daemon(
    options: &Options,
    session: &SshSession,
) -> anyhow::Result<RemoteProbeResponse> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(probe) = session
            .agent_json::<RemoteProbeResponse>(&[
                &options.remote_binary,
                "remote",
                "probe",
                "--state-dir",
                &options.state_dir,
            ])
            .await
        {
            if probe.daemon.is_some() {
                return Ok(probe);
            }
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("remote daemon did not become ready within 20 seconds");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn start_daemon(options: &Options, session: &SshSession) -> anyhow::Result<()> {
    let log = format!("{}/daemon.log", options.state_dir);
    let command = format!(
        "mkdir -p {state} && chmod 700 {state} && nohup {binary} remote daemon --state-dir {state} >{log} 2>&1 </dev/null &",
        state = options.state_dir,
        binary = options.remote_binary,
    );
    session.ssh_status(&[&command]).await
}

async fn install_server(options: &Options, session: &SshSession) -> anyhow::Result<()> {
    let local = options
        .local_server_binary
        .as_ref()
        .context("local server artifact was not selected")?
        .clone();
    if !local.is_file() {
        bail!("local sift-server binary is missing: {}", local.display());
    }
    let target = session.ssh_output(&["uname -s && uname -m"]).await?;
    ensure_same_target(&target)?;

    let parent = Path::new(&options.remote_binary)
        .parent()
        .context("remote binary path has no parent")?
        .to_string_lossy()
        .to_string();
    let staging = format!(
        "{}.upload-{}",
        options.remote_binary,
        uuid::Uuid::new_v4().simple()
    );
    let digest = sha256_file(&local)?;
    session
        .ssh_status(&[&format!("mkdir -p {parent} && chmod 700 {parent}")])
        .await?;
    let mut scp = Command::new("scp");
    scp.arg("-q");
    if session.multiplex.load(Ordering::Acquire) {
        scp.arg("-o")
            .arg(format!("ControlPath={}", session.control_socket.display()));
    }
    let status = scp
        .arg(&local)
        .arg(format!("{}:{staging}", session.destination))
        .status()
        .await
        .context("starting SFTP-backed scp upload")?;
    if !status.success() {
        bail!("uploading remote sift-server failed with {status}");
    }
    let _: RemoteProbeResponse = session
        .agent_json(&[
            &staging,
            "remote",
            "install",
            "--state-dir",
            &options.state_dir,
            "--destination",
            &options.remote_binary,
            "--sha256",
            &digest,
        ])
        .await?;
    Ok(())
}

async fn select_local_server(options: &Options) -> anyhow::Result<(PathBuf, bool)> {
    if let Some(path) = &options.local_server_binary {
        return Ok((path.clone(), false));
    }
    let config = sift_server::config::load().context("loading local update configuration")?;
    if config.updater.enabled {
        let updater = sift_server::updater::Updater::from_config(&config)?;
        let _ = updater.check_and_stage().await?;
        let selected = updater
            .selected_release()?
            .context("signed updater did not select a server artifact")?;
        let pending = updater
            .pending_release()?
            .is_some_and(|pending| pending.executable == selected.executable);
        return Ok((selected.executable, pending));
    }
    Ok((
        std::env::current_exe()?
            .parent()
            .context("sift-remote executable has no parent directory")?
            .join("sift-server"),
        false,
    ))
}

async fn forward_connection(
    session: SshSession,
    endpoint: &str,
    stream: TcpStream,
) -> anyhow::Result<()> {
    session.ensure_master().await?;
    let mut command = Command::new("ssh");
    if session.multiplex.load(Ordering::Acquire) {
        command.arg("-S").arg(session.control_socket.as_ref());
    }
    let mut child = command
        .arg("-W")
        .arg(endpoint)
        .arg(session.destination.as_ref())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("starting SSH direct-stream channel")?;
    let (mut socket_read, mut socket_write) = stream.into_split();
    let mut child_stdin = child.stdin.take().context("SSH channel omitted stdin")?;
    let mut child_stdout = child.stdout.take().context("SSH channel omitted stdout")?;
    let upload = tokio::io::copy(&mut socket_read, &mut child_stdin);
    let download = tokio::io::copy(&mut child_stdout, &mut socket_write);
    let _ = tokio::try_join!(upload, download);
    let _ = child.kill().await;
    let _ = child.wait().await;
    Ok(())
}

impl SshSession {
    async fn ensure_master(&self) -> anyhow::Result<()> {
        if !self.multiplex.load(Ordering::Acquire) {
            return Ok(());
        }
        let _guard = self.control_guard.lock().await;
        if !self.multiplex.load(Ordering::Acquire) {
            return Ok(());
        }
        let check = Command::new("ssh")
            .arg("-S")
            .arg(self.control_socket.as_ref())
            .arg("-O")
            .arg("check")
            .arg(self.destination.as_ref())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
        if check.is_ok_and(|status| status.success()) {
            return Ok(());
        }
        let status = Command::new("ssh")
            .arg("-M")
            .arg("-S")
            .arg(self.control_socket.as_ref())
            .arg("-o")
            .arg("ControlPersist=60")
            .arg("-f")
            .arg("-N")
            .arg(self.destination.as_ref())
            .status()
            .await
            .context("starting OpenSSH control master")?;
        if !status.success() {
            self.multiplex.store(false, Ordering::Release);
        }
        Ok(())
    }

    async fn close_master(&self) {
        if !self.multiplex.load(Ordering::Acquire) {
            return;
        }
        let _ = Command::new("ssh")
            .arg("-S")
            .arg(self.control_socket.as_ref())
            .arg("-O")
            .arg("exit")
            .arg(self.destination.as_ref())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }

    async fn agent_json<T: serde::de::DeserializeOwned>(
        &self,
        remote_args: &[&str],
    ) -> anyhow::Result<T> {
        let output = self.ssh_output(remote_args).await?;
        serde_json::from_slice(&output).context("decoding bounded remote-agent response")
    }

    async fn ssh_output(&self, remote_args: &[&str]) -> anyhow::Result<Vec<u8>> {
        self.ensure_master().await?;
        let mut command = Command::new("ssh");
        if self.multiplex.load(Ordering::Acquire) {
            command.arg("-S").arg(self.control_socket.as_ref());
        }
        let mut child = command
            .arg(self.destination.as_ref())
            .args(remote_args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("starting remote SSH command")?;
        let stdout = child.stdout.take().context("SSH command omitted stdout")?;
        let stderr = child.stderr.take().context("SSH command omitted stderr")?;
        let stdout_read = async {
            let mut bytes = Vec::with_capacity(MAX_AGENT_OUTPUT.min(4096));
            stdout
                .take((MAX_AGENT_OUTPUT + 1) as u64)
                .read_to_end(&mut bytes)
                .await?;
            std::io::Result::Ok(bytes)
        };
        let stderr_read = async {
            let mut bytes = Vec::with_capacity(MAX_AGENT_OUTPUT.min(4096));
            stderr
                .take((MAX_AGENT_OUTPUT + 1) as u64)
                .read_to_end(&mut bytes)
                .await?;
            std::io::Result::Ok(bytes)
        };
        let (stdout, stderr, status) = tokio::try_join!(stdout_read, stderr_read, child.wait())
            .context("running remote SSH command")?;
        if stdout.len() > MAX_AGENT_OUTPUT || stderr.len() > MAX_AGENT_OUTPUT {
            bail!("remote agent output exceeded 64 KiB");
        }
        if !status.success() {
            bail!(
                "remote agent failed: {}",
                String::from_utf8_lossy(&stderr).trim()
            );
        }
        Ok(stdout)
    }

    async fn ssh_status(&self, remote_args: &[&str]) -> anyhow::Result<()> {
        self.ensure_master().await?;
        let mut command = Command::new("ssh");
        if self.multiplex.load(Ordering::Acquire) {
            command.arg("-S").arg(self.control_socket.as_ref());
        }
        let status = command
            .arg(self.destination.as_ref())
            .args(remote_args)
            .status()
            .await
            .context("running remote SSH command")?;
        if !status.success() {
            bail!("remote SSH command failed with {status}");
        }
        Ok(())
    }
}

fn parse_options(args: impl IntoIterator<Item = String>) -> anyhow::Result<Options> {
    let mut args = args.into_iter();
    let destination = args
        .next()
        .context("usage: sift-remote <ssh-destination> [options]")?;
    if destination.starts_with('-') {
        bail!("SSH destination must not start with '-'");
    }
    let mut options = Options {
        destination,
        state_dir: ".local/state/sift/remote".into(),
        remote_binary: format!(
            ".local/share/sift/bin/{}/{os}-{arch}/sift-server",
            sift_server::VERSION,
            os = std::env::consts::OS,
            arch = std::env::consts::ARCH
        ),
        remote_binary_explicit: false,
        local_server_binary: None,
        local_update_candidate: false,
        sift_key_file: None,
        local_port: 0,
    };
    while let Some(argument) = args.next() {
        let value = args
            .next()
            .with_context(|| format!("{argument} requires a value"))?;
        match argument.as_str() {
            "--state-dir" => options.state_dir = value,
            "--remote-binary" => {
                options.remote_binary = value;
                options.remote_binary_explicit = true;
            }
            "--local-server-binary" => options.local_server_binary = Some(value.into()),
            "--sift-key-file" => options.sift_key_file = Some(value.into()),
            "--local-port" => options.local_port = value.parse().context("invalid local port")?,
            _ => bail!("unknown sift-remote argument `{argument}`"),
        }
    }
    Ok(options)
}

fn validate_remote_path(path: &str) -> anyhow::Result<()> {
    if path.is_empty()
        || path.starts_with('/')
        || path.split('/').any(|part| part == ".." || part.is_empty())
        || !path
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-/".contains(&byte))
    {
        bail!("remote path must be a safe relative path: {path}");
    }
    Ok(())
}

fn ensure_same_target(value: &[u8]) -> anyhow::Result<()> {
    let value = String::from_utf8_lossy(value).to_ascii_lowercase();
    let os_matches = match std::env::consts::OS {
        "linux" => value.contains("linux"),
        "macos" => value.contains("darwin"),
        other => value.contains(other),
    };
    let arch_matches = match std::env::consts::ARCH {
        "x86_64" => value.contains("x86_64") || value.contains("amd64"),
        "aarch64" => value.contains("aarch64") || value.contains("arm64"),
        other => value.contains(other),
    };
    if !os_matches || !arch_matches {
        bail!(
            "remote target does not match local artifact target: {}",
            value.trim()
        );
    }
    Ok(())
}

fn load_signing_key(path: &Path) -> anyhow::Result<SigningKey> {
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("reading Sift signing key metadata: {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            bail!("Sift signing key must have mode 0600 or stricter");
        }
    }
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading Sift signing key: {}", path.display()))?;
    let bytes = decode_hex_32(text.trim())?;
    Ok(SigningKey::from_bytes(&bytes))
}

fn decode_hex_32(value: &str) -> anyhow::Result<[u8; 32]> {
    if value.len() != 64 {
        bail!("Sift signing key must contain exactly 64 hexadecimal characters");
    }
    let mut out = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let pair = std::str::from_utf8(pair)?;
        out[index] = u8::from_str_radix(pair, 16).context("invalid signing-key hex")?;
    }
    Ok(out)
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("opening artifact: {}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = std::io::Read::read(&mut file, &mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    let mut output = String::with_capacity(64);
    for byte in digest.finalize() {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    Ok(output)
}

fn write_secret_json(value: &RemoteReady) -> anyhow::Result<()> {
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    serde_json::to_writer(&mut stdout, value)?;
    std::io::Write::write_all(&mut stdout, b"\n")?;
    std::io::Write::flush(&mut stdout)?;
    Ok(())
}

fn make_private_dir(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_paths_reject_shell_and_parent_traversal() {
        for invalid in [
            "",
            "/tmp/sift",
            "../sift",
            "state/../sift",
            "state dir",
            "x;id",
        ] {
            assert!(validate_remote_path(invalid).is_err(), "{invalid}");
        }
        assert!(validate_remote_path(".local/state/sift/remote").is_ok());
    }

    #[test]
    fn signing_key_parser_is_exact() {
        let key = decode_hex_32(&"11".repeat(32)).unwrap();
        assert_eq!(key, [0x11; 32]);
        assert!(decode_hex_32("11").is_err());
        assert!(decode_hex_32(&"zz".repeat(32)).is_err());
    }

    #[test]
    fn option_parser_rejects_option_shaped_destination() {
        assert!(parse_options(["-oProxyCommand=x".to_string()]).is_err());
        let options = parse_options([
            "user@example".to_string(),
            "--local-port".to_string(),
            "7777".to_string(),
        ])
        .unwrap();
        assert_eq!(options.local_port, 7777);
    }
}
