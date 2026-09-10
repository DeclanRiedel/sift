//! Backend-owned tailnet discovery and connection-scoped OpenSSH forwarding.
use crate::error::{ApiError, ApiResult};
use sift_api_types::{
    TailnetMode, TailnetPeer, TailnetProbeReport, TailnetProbeRequest, TailnetSettings,
    TailnetStatus,
};
use std::{net::IpAddr, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    process::Command,
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

const LIMIT: u64 = 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(10);

fn failure(message: impl Into<String>) -> ApiError {
    sift_protocol::DriverError::new(sift_protocol::Code::ConnectionFailed, message).into()
}

pub(crate) fn database_failure(
    configuration: &serde_json::Value,
    tunneled: bool,
    mut error: sift_protocol::DriverError,
) -> sift_protocol::DriverError {
    if tunneled {
        error.message = format!(
            "Database stage through SSH to {}, remote 127.0.0.1:{} failed ({:?}). SSH authentication and host verification succeeded. Check the remote database listener, database name and credentials. The database connection uses a local forwarding socket, not a local database.",
            configuration["host"].as_str().unwrap_or("tailnet device"), configuration["port"], error.code,
        );
    }
    error
}

/// Bounded output and process lifetime. Never return raw SSH output to clients.
async fn output(mut command: Command, stage: &str) -> ApiResult<Vec<u8>> {
    output_with_input(&mut command, stage, None).await
}

async fn output_with_input(
    command: &mut Command,
    stage: &str,
    input: Option<Vec<u8>>,
) -> ApiResult<Vec<u8>> {
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|_| failure(format!("{stage}: executable unavailable on Sift backend")))?;
    let mut stdout = child.stdout.take().unwrap().take(LIMIT + 1);
    let mut stderr = child.stderr.take().unwrap().take(16384);
    let result = tokio::time::timeout(
        if input.is_some() {
            Duration::from_secs(40)
        } else {
            TIMEOUT
        },
        async {
            if let Some(input) = input {
                let mut stdin = child.stdin.take().unwrap();
                stdin.write_all(&input).await?;
                stdin.shutdown().await?;
            }
            let mut bytes = Vec::new();
            let mut errors = Vec::new();
            let (_, _, status) = tokio::try_join!(
                stdout.read_to_end(&mut bytes),
                stderr.read_to_end(&mut errors),
                child.wait()
            )?;
            Ok::<_, std::io::Error>((status, bytes, errors))
        },
    )
    .await;
    match result {
        Ok(Ok((status, bytes, _))) if status.success() && bytes.len() <= LIMIT as usize => Ok(bytes),
        Ok(Ok((_, _, errors))) => Err(failure(format!("{stage}: {}", process_failure(&errors)))),
        _ => Err(failure(format!("{stage}: failed or timed out. Check backend daemon, SSH agent/key, known_hosts and access policy; interactive prompts are disabled"))),
    }
}

fn process_failure(errors: &[u8]) -> &'static str {
    let errors = String::from_utf8_lossy(errors);
    if errors.contains("REMOTE HOST IDENTIFICATION HAS CHANGED") {
        "SSH host key changed. Connection blocked; verify the server identity before replacing the pin."
    } else if errors.contains("Host key verification failed") {
        "SSH host key is unknown or does not match. Read and independently verify its fingerprint in Sift."
    } else if errors.contains("Permission denied") {
        "SSH authentication denied. Check SSH user, backend agent/key and server policy."
    } else if errors.contains("Connection refused") {
        "SSH port refused the connection. Check the remote SSH listener and port."
    } else {
        "Command failed. Check backend Tailscale daemon, SSH agent, known_hosts and policy. No interactive prompts are allowed."
    }
}

pub async fn serve(
    request: sift_api_types::TailnetServeRequest,
    owner: String,
) -> ApiResult<sift_api_types::TailnetServeReport> {
    use base64::Engine as _;
    validate(&request.connection.settings)?;
    if request.connection.settings.ssh_user.is_empty() || request.connection.port == 0 {
        return Err(ApiError::BadRequest(
            "SSH user and database port required for Serve management".into(),
        ));
    }
    if matches!(
        request.action,
        sift_api_types::TailnetServeAction::Apply | sift_api_types::TailnetServeAction::Remove
    ) && !request.acknowledge_exposure
    {
        return Err(ApiError::BadRequest(
            "Confirm tailnet exposure and localhost authentication review first".into(),
        ));
    }
    let peer = peer(&request.connection.host).await?;
    let script = base64::engine::general_purpose::STANDARD.encode(include_str!("tailnet_serve.py"));
    let (mut command, _known_hosts) = ssh(&peer.address, &request.connection.settings)?;
    // Encoded fixed source only. All request data travels as JSON on stdin.
    command.arg(format!(
        "python3 -c 'import base64;exec(base64.b64decode(\"{script}\"))'"
    ));
    let input = serde_json::to_vec(&serde_json::json!({
        "owner": owner, "port": request.connection.port, "action": request.action,
        "acknowledge_exposure": request.acknowledge_exposure,
        "expected_revision": request.expected_revision,
    }))
    .map_err(|_| ApiError::BadRequest("Invalid Serve request".into()))?;
    let bytes = output_with_input(&mut command, "Remote Serve management", Some(input)).await?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| failure("Remote Serve response invalid"))?;
    if let Some(message) = value["error"].as_str() {
        return Err(failure(message));
    }
    serde_json::from_value(value).map_err(|_| failure("Remote Serve response invalid"))
}

pub async fn status() -> ApiResult<TailnetStatus> {
    let mut command = Command::new("tailscale");
    command.args(["status", "--json"]);
    parse_status(&output(command, "Tailscale discovery").await?)
}

fn parse_status(bytes: &[u8]) -> ApiResult<TailnetStatus> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| failure("Tailscale returned invalid status"))?;
    let mut peers = Vec::new();
    if let Some(rows) = value["Peer"].as_object() {
        for row in rows.values().take(2048) {
            let Some(address) = row["TailscaleIPs"].as_array().and_then(|ips| {
                ips.iter()
                    .filter_map(|ip| ip.as_str())
                    .find(|ip| ip.parse::<IpAddr>().is_ok())
            }) else {
                continue;
            };
            let name = row["DNSName"]
                .as_str()
                .filter(|s| !s.is_empty())
                .or_else(|| row["HostName"].as_str())
                .unwrap_or(address)
                .trim_end_matches('.')
                .to_owned();
            peers.push(TailnetPeer {
                name,
                address: address.into(),
                online: row["Online"].as_bool().unwrap_or(false),
            });
        }
    }
    peers.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(TailnetStatus {
        backend_state: value["BackendState"].as_str().unwrap_or("Unknown").into(),
        backend_name: value["Self"]["HostName"]
            .as_str()
            .unwrap_or("Sift backend")
            .into(),
        peers,
    })
}

pub fn settings(configuration: &serde_json::Value) -> ApiResult<Option<TailnetSettings>> {
    configuration
        .get("sift_network")
        .map(|value| {
            let settings: TailnetSettings = serde_json::from_value(value.clone())
                .map_err(|_| ApiError::BadRequest("Invalid tailnet settings".into()))?;
            validate(&settings)?;
            Ok(settings)
        })
        .transpose()
}

fn validate(settings: &TailnetSettings) -> ApiResult<()> {
    if settings.ssh_port == 0
        || (settings.mode != TailnetMode::Direct && settings.ssh_user.is_empty())
        || (!settings.ssh_user.is_empty()
            && (settings.ssh_user.len() > 64
                || !settings
                    .ssh_user
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
                || settings.ssh_user.starts_with('-')))
    {
        return Err(ApiError::BadRequest(
            "SSH user must be a simple account name and SSH port must be positive".into(),
        ));
    }
    if let Some(key) = &settings.host_key {
        host_fingerprint(key)?;
    }
    Ok(())
}

async fn peer(host: &str) -> ApiResult<TailnetPeer> {
    let status = status().await?;
    if status.backend_state != "Running" {
        return Err(failure(format!(
            "Tailscale backend is {}",
            status.backend_state
        )));
    }
    let mut matches = status.peers.into_iter().filter(|p| {
        p.address == host
            || p.name.eq_ignore_ascii_case(host.trim_end_matches('.'))
            || p.name
                .split('.')
                .next()
                .is_some_and(|name| name.eq_ignore_ascii_case(host))
    });
    let peer = matches.next().ok_or_else(|| {
        failure("Device not found in Sift backend's tailnet; refresh devices and select an address")
    })?;
    if matches.next().is_some() {
        return Err(failure(
            "Ambiguous tailnet name; select an exact device address",
        ));
    }
    if !peer.online {
        return Err(failure("Tailnet device is offline"));
    }
    Ok(peer)
}

async fn tcp(host: &str, port: u16) -> Result<(), String> {
    match tokio::time::timeout(Duration::from_secs(3), TcpStream::connect((host, port))).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
            Err("TCP connection refused; service may listen only on remote localhost".into())
        }
        Ok(Err(_)) => Err("TCP connection failed; check tailnet policy and routing".into()),
        Err(_) => Err("TCP connection timed out; check device, firewall and tailnet policy".into()),
    }
}

fn ssh(
    address: &str,
    settings: &TailnetSettings,
) -> ApiResult<(Command, Option<tempfile::NamedTempFile>)> {
    let mut command = Command::new("ssh");
    let known_hosts = if let Some(key) = &settings.host_key {
        host_fingerprint(key)?;
        let mut file = tempfile::NamedTempFile::new()
            .map_err(|_| failure("Cannot create private host verification file"))?;
        let line = if settings.ssh_port == 22 {
            format!("{address} {key}\n")
        } else {
            format!("[{address}]:{} {key}\n", settings.ssh_port)
        };
        std::io::Write::write_all(&mut file, line.as_bytes())
            .map_err(|_| failure("Cannot write host verification file"))?;
        command
            .arg("-o")
            .arg(format!("UserKnownHostsFile={}", file.path().display()))
            .args(["-o", "GlobalKnownHostsFile=none"]);
        Some(file)
    } else {
        None
    };
    // No SSH config execution/ProxyCommand, agent forwarding, password prompts,
    // automatic host trust, or user-supplied shell fragments.
    command.args([
        "-F",
        "none",
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "StrictHostKeyChecking=yes",
        "-o",
        "ForwardAgent=no",
        "-o",
        "ClearAllForwardings=yes",
        "-o",
        "ConnectTimeout=8",
        "-o",
        "ServerAliveInterval=10",
        "-o",
        "ServerAliveCountMax=2",
        "-p",
        &settings.ssh_port.to_string(),
        "-l",
        &settings.ssh_user,
    ]);
    command.arg(address);
    Ok((command, known_hosts))
}

fn host_fingerprint(key: &str) -> ApiResult<String> {
    use base64::Engine as _;
    use sha2::Digest as _;
    let parts: Vec<_> = key.split(' ').collect();
    if parts.len() != 2 || parts[0] != "ssh-ed25519" || key.len() > 256 {
        return Err(ApiError::BadRequest(
            "Expected an Ed25519 public SSH host key".into(),
        ));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(parts[1])
        .map_err(|_| ApiError::BadRequest("Invalid public host key".into()))?;
    if bytes.len() != 51 || &bytes[..19] != b"\0\0\0\x0bssh-ed25519\0\0\0\x20" {
        return Err(ApiError::BadRequest(
            "Invalid Ed25519 host key encoding".into(),
        ));
    }
    Ok(format!(
        "SHA256:{}",
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(sha2::Sha256::digest(bytes))
    ))
}

pub async fn scan_host_key(
    request: TailnetProbeRequest,
) -> ApiResult<sift_api_types::TailnetHostKey> {
    if request.settings.ssh_port == 0 {
        return Err(ApiError::BadRequest("SSH port required".into()));
    }
    let peer = peer(&request.host).await?;
    let mut command = Command::new("ssh-keyscan");
    command.args([
        "-T",
        "5",
        "-t",
        "ed25519",
        "-p",
        &request.settings.ssh_port.to_string(),
        &peer.address,
    ]);
    let bytes = output(command, "SSH host key scan").await?;
    let text = String::from_utf8(bytes).map_err(|_| failure("Invalid host key response"))?;
    for line in text.lines().filter(|line| !line.starts_with('#')) {
        let parts: Vec<_> = line.split_whitespace().collect();
        if parts.len() == 3 && parts[1] == "ssh-ed25519" {
            let key = format!("{} {}", parts[1], parts[2]);
            return Ok(sift_api_types::TailnetHostKey {
                fingerprint: host_fingerprint(&key)?,
                key,
                host: request.host,
            });
        }
    }
    Err(failure("Server did not advertise an Ed25519 host key"))
}

pub async fn probe(request: TailnetProbeRequest) -> ApiResult<TailnetProbeReport> {
    validate(&request.settings)?;
    if request.port == 0 {
        return Err(ApiError::BadRequest(
            "Database port must be positive".into(),
        ));
    }
    let peer = peer(&request.host).await?;
    let direct = tcp(&peer.address, request.port).await;
    if request.settings.mode != TailnetMode::Tunnel && direct.is_ok() {
        return Ok(TailnetProbeReport {
            stage: "tcp".into(),
            reachable: true,
            message: "Tailnet TCP reachable. Test database authentication before saving.".into(),
        });
    }
    if request.settings.mode == TailnetMode::Direct {
        return Ok(TailnetProbeReport {
            stage: "tcp".into(),
            reachable: false,
            message: direct.unwrap_err(),
        });
    }
    let (mut command, _known_hosts) = ssh(&peer.address, &request.settings)?;
    command.arg("true");
    output(command, "SSH authentication / host verification").await?;
    Ok(TailnetProbeReport {
        stage: "ssh".into(),
        reachable: true,
        message:
            "SSH verified. Database test will connect to remote 127.0.0.1 through a managed tunnel."
                .into(),
    })
}

pub struct TunnelLease {
    cancel: CancellationToken,
}
impl Drop for TunnelLease {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Local listener is owned by us throughout: no allocate/drop/rebind port race.
async fn tunnel(
    address: String,
    port: u16,
    settings: TailnetSettings,
) -> ApiResult<(u16, TunnelLease)> {
    let (mut check, _known_hosts) = ssh(&address, &settings)?;
    check.arg("true");
    output(check, "SSH authentication / host verification").await?;
    forwarder(address, port, settings).await
}

async fn forwarder(
    address: String,
    port: u16,
    settings: TailnetSettings,
) -> ApiResult<(u16, TunnelLease)> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|_| failure("Cannot bind tunnel listener"))?;
    let local_port = listener
        .local_addr()
        .map_err(|_| failure("Cannot inspect tunnel listener"))?
        .port();
    let cancel = CancellationToken::new();
    let stopped = cancel.clone();
    tokio::spawn(async move {
        let mut channels = JoinSet::new();
        loop {
            tokio::select! {
                _ = stopped.cancelled() => break,
                _ = channels.join_next(), if !channels.is_empty() => {},
                accepted = listener.accept() => {
                    let Ok((stream, _)) = accepted else { break; };
                    if channels.len() >= 32 { continue; }
                    let Ok((mut command, known_hosts)) = ssh(&address, &settings) else { continue; };
                    // Insert -W before destination: OpenSSH stops parsing at host.
                    let args: Vec<_> = command.as_std().get_args().map(|s| s.to_os_string()).collect();
                    command = Command::new("ssh");
                    command.args(&args[..args.len()-1]).arg("-W").arg(format!("127.0.0.1:{port}")).arg(&address);
                    channels.spawn(async move {
                        let _known_hosts = known_hosts;
                        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
                        let Ok(mut child) = command.spawn() else { return; };
                        let mut input = child.stdin.take().unwrap();
                        let mut output = child.stdout.take().unwrap();
                        let (mut reader, mut writer) = stream.into_split();
                        tokio::select! {
                            _ = async { let _ = tokio::io::copy(&mut reader, &mut input).await; let _ = input.shutdown().await; } => {},
                            _ = tokio::io::copy(&mut output, &mut writer) => {},
                            _ = child.wait() => {},
                        }
                    });
                }
            }
        }
        channels.abort_all();
        while channels.join_next().await.is_some() {}
    });
    Ok((local_port, TunnelLease { cancel }))
}

pub async fn prepare(
    configuration: &serde_json::Value,
) -> ApiResult<(serde_json::Value, Option<TunnelLease>)> {
    let mut result = configuration.clone();
    let Some(settings) = settings(configuration)? else {
        return Ok((result, None));
    };
    let host = configuration["host"]
        .as_str()
        .ok_or_else(|| ApiError::BadRequest("Tailnet host required".into()))?;
    let port = configuration["port"]
        .as_u64()
        .and_then(|p| u16::try_from(p).ok())
        .filter(|p| *p > 0)
        .ok_or_else(|| {
            ApiError::BadRequest("Explicit database port required for tailnet connections".into())
        })?;
    let peer = peer(host).await?;
    result.as_object_mut().unwrap().remove("sift_network");
    let use_tunnel = settings.mode == TailnetMode::Tunnel
        || (settings.mode == TailnetMode::Automatic && tcp(&peer.address, port).await.is_err());
    if use_tunnel {
        // Preserve TLS verification semantics: loopback substitution must not
        // silently change the identity being verified.
        if matches!(configuration["ssl_mode"].as_str(), Some("verify_full")) {
            return Err(ApiError::BadRequest("SSH tunnelling with verify_full needs separate TLS server-name support; use direct tailnet access".into()));
        }
        let (port, lease) = tunnel(peer.address, port, settings).await?;
        result["host"] = "127.0.0.1".into();
        result["port"] = port.into();
        Ok((result, Some(lease)))
    } else {
        // Retain DNS name for TLS verification; selected tailnet address remains
        // unchanged for the common IP-based configuration.
        Ok((result, None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn dropping_tunnel_lease_releases_listener() {
        let (port, lease) = forwarder(
            "127.0.0.1".into(),
            5432,
            TailnetSettings {
                mode: TailnetMode::Tunnel,
                ssh_user: "fixture".into(),
                ssh_port: 22,
                host_key: None,
            },
        )
        .await
        .unwrap();
        assert!(TcpListener::bind(("127.0.0.1", port)).await.is_err());
        drop(lease);
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if TcpListener::bind(("127.0.0.1", port)).await.is_ok() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[test]
    fn pinned_host_keys_are_public_and_strictly_verified() {
        use base64::Engine as _;
        let mut bytes = b"\0\0\0\x0bssh-ed25519\0\0\0\x20".to_vec();
        bytes.extend([1; 32]);
        let key = format!(
            "ssh-ed25519 {}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        );
        assert!(host_fingerprint(&key).unwrap().starts_with("SHA256:"));
        assert!(host_fingerprint("ssh-ed25519 invalid;command").is_err());
        let (command, file) = ssh(
            "100.83.175.73",
            &TailnetSettings {
                mode: TailnetMode::Tunnel,
                ssh_user: "fixture".into(),
                ssh_port: 2222,
                host_key: Some(key.clone()),
            },
        )
        .unwrap();
        let args: Vec<_> = command
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy())
            .collect();
        assert!(args.iter().any(|arg| arg == "StrictHostKeyChecking=yes"));
        assert_eq!(
            std::fs::read_to_string(file.unwrap().path()).unwrap(),
            format!("[100.83.175.73]:2222 {key}\n")
        );
    }
    #[test]
    fn discovery_uses_only_valid_peer_addresses() {
        let status = parse_status(br#"{"BackendState":"Running","Self":{"HostName":"backend"},"Peer":{"a":{"DNSName":"pi.example.ts.net.","TailscaleIPs":["100.83.175.73"],"Online":true},"b":{"TailscaleIPs":["-oProxyCommand=bad"]}}}"#).unwrap();
        assert_eq!(status.peers.len(), 1);
        assert_eq!(status.peers[0].name, "pi.example.ts.net");
        assert!(status.peers[0].online);
    }
    #[test]
    fn ssh_settings_reject_command_fragments() {
        for user in ["", "-root", "root;id", "root\n", "root@host"] {
            assert!(validate(&TailnetSettings {
                mode: TailnetMode::Tunnel,
                ssh_user: user.into(),
                ssh_port: 22,
                host_key: None
            })
            .is_err());
        }
    }
    #[tokio::test]
    async fn direct_tcp_reports_refusal() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(tcp("127.0.0.1", port).await.is_ok());
        drop(listener);
        assert!(tcp("127.0.0.1", port)
            .await
            .unwrap_err()
            .contains("refused"));
    }
}
