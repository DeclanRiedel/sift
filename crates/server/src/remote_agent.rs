//! Commands executed through the authenticated SSH channel.

use crate::config::{
    Config, DeploymentPolicy, MetadataConfig, RuntimeConfig, RuntimeMode, Transport,
};
use crate::metadata_runtime::build_metadata_store;
use crate::runtime::{read_daemon_descriptor, DaemonDescriptor};
use anyhow::{bail, Context};
use base64::Engine as _;
use ed25519_dalek::Verifier as _;
use sift_metadata::NewOperationAudit;
use sift_protocol::{
    ProtocolRange, RemoteCapabilityResponse, RemoteDaemonDescriptor, RemoteKeyChallenge,
    RemoteProbeResponse, SshProxyCapabilityClaims,
};
use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

const REMOTE_CONFIG_FILE: &str = "sift.toml";
const REMOTE_METADATA_FILE: &str = "metadata.sqlite";
const REMOTE_SECRET_KEY_FILE: &str = "secret.key";
const RUNTIME_DIR: &str = "runtime";

pub fn prepare_remote_config(state_dir: &Path) -> anyhow::Result<Config> {
    ensure_private_dir(state_dir)?;
    let config_path = state_dir.join(REMOTE_CONFIG_FILE);
    let mut config = if config_path.exists() {
        crate::config::load_path(&config_path)
            .with_context(|| format!("loading remote config: {}", config_path.display()))?
    } else {
        Config::default()
    };
    let runtime_dir = state_dir.join(RUNTIME_DIR);
    ensure_private_dir(&runtime_dir)?;
    let secret_key = state_dir.join(REMOTE_SECRET_KEY_FILE);
    ensure_secret_key(&secret_key)?;

    config.mode = RuntimeMode::Daemon;
    config.transport = Transport::SshProxy;
    config.bind = "127.0.0.1:0".into();
    config.auth.loopback_bypass = false;
    config.runtime = RuntimeConfig {
        state_dir: Some(runtime_dir.display().to_string()),
    };
    config.metadata = MetadataConfig {
        enabled: true,
        path: Some(state_dir.join(REMOTE_METADATA_FILE).display().to_string()),
        secret_backend: "file".into(),
        secret_key_file: Some(secret_key.display().to_string()),
        bootstrap_local: config.deployment == DeploymentPolicy::Personal,
        store_sql: config.metadata.store_sql,
    };
    config.validate()?;
    Ok(config)
}

pub fn probe(state_dir: &Path) -> anyhow::Result<RemoteProbeResponse> {
    let descriptor_path = state_dir.join(RUNTIME_DIR).join("daemon.json");
    let daemon = if descriptor_path.exists() {
        let descriptor = read_daemon_descriptor(&state_dir.join(RUNTIME_DIR))?;
        Some(remote_descriptor(descriptor))
    } else {
        None
    };
    Ok(RemoteProbeResponse {
        schema_version: 1,
        server_version: crate::VERSION.into(),
        protocol: ProtocolRange::exact(sift_protocol::PROTOCOL_VERSION_NUMBER),
        target_os: std::env::consts::OS.into(),
        target_arch: std::env::consts::ARCH.into(),
        install_layout_version: 1,
        daemon,
    })
}

pub fn challenge(state_dir: &Path, fingerprint: &str) -> anyhow::Result<RemoteKeyChallenge> {
    let config = prepare_remote_config(state_dir)?;
    let metadata = build_metadata_store(&config)?.context("remote metadata is disabled")?;
    let challenge = metadata
        .issue_key_challenge(fingerprint)
        .context("issuing registered-key challenge")?;
    let nonce = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&challenge.nonce);
    let audience = instance_audience(&config, &probe_ready_daemon(state_dir)?);
    Ok(RemoteKeyChallenge {
        message: key_challenge_message(&audience, &nonce),
        nonce,
        expires_at: challenge.expires_at,
    })
}

pub async fn issue_capability(
    state_dir: &Path,
    proof: Option<(&str, &str)>,
) -> anyhow::Result<RemoteCapabilityResponse> {
    let config = prepare_remote_config(state_dir)?;
    let daemon = probe_ready_daemon(state_dir)?;
    let metadata = build_metadata_store(&config)?.context("remote metadata is disabled")?;
    let audience = instance_audience(&config, &daemon);

    let (principal, principal_key_id) = match proof {
        Some((nonce, signature)) => {
            let nonce_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(nonce)
                .context("invalid challenge nonce")?;
            let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(signature)
                .context("invalid challenge signature")?;
            let consumed = metadata
                .consume_key_challenge(&nonce_bytes)
                .context("invalid or consumed registered-key challenge")?;
            let public_key: [u8; 32] = consumed
                .principal_key
                .public_key
                .as_slice()
                .try_into()
                .context("registered key is not Ed25519")?;
            let verifying_key = ed25519_dalek::VerifyingKey::from_bytes(&public_key)
                .context("invalid registered Ed25519 key")?;
            let signature =
                ed25519_dalek::Signature::from_slice(&signature).context("invalid signature")?;
            verifying_key
                .verify(
                    key_challenge_message(&audience, nonce).as_bytes(),
                    &signature,
                )
                .context("registered-key proof failed")?;
            (
                consumed.principal_key.principal_id,
                Some(consumed.principal_key.id),
            )
        }
        None => {
            if config.deployment != DeploymentPolicy::Personal {
                bail!("team SSH bootstrap requires registered-key proof");
            }
            verify_private_personal_state(state_dir)?;
            let principal = metadata
                .resolve_principal_by_external_id("local:1")?
                .context("personal instance has no bootstrapped local principal")?;
            (principal.id, None)
        }
    };

    let now = chrono::Utc::now();
    let claims = SshProxyCapabilityClaims {
        version: 1,
        instance_audience: audience,
        principal_id: principal.0,
        capability_id: uuid::Uuid::new_v4().to_string(),
        issued_at: now,
        expires_at: now + chrono::Duration::minutes(2),
    };
    let issued = metadata
        .issue_ssh_proxy_capability(
            &claims,
            &daemon.daemon_generation,
            principal_key_id,
            NewOperationAudit {
                actor_principal_id: Some(principal),
                action: "ssh_proxy.issue".into(),
                target: "ssh_proxy_capability".into(),
                target_id: None,
                status: "succeeded".into(),
                result_code: None,
                row_count: None,
                error_message: None,
                correlation_id: None,
            },
        )
        .await?;
    Ok(RemoteCapabilityResponse {
        capability: issued.capability,
        claims,
        daemon,
    })
}

pub fn write_json<T: serde::Serialize>(value: &T) -> anyhow::Result<()> {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    serde_json::to_writer(&mut lock, value)?;
    lock.write_all(b"\n")?;
    lock.flush()?;
    Ok(())
}

fn probe_ready_daemon(state_dir: &Path) -> anyhow::Result<RemoteDaemonDescriptor> {
    let descriptor = probe(state_dir)?
        .daemon
        .context("remote daemon is not ready")?;
    let endpoint: SocketAddr = descriptor
        .endpoint
        .parse()
        .context("daemon descriptor has an invalid endpoint")?;
    if !endpoint.ip().is_loopback() {
        bail!("SSH proxy daemon endpoint is not loopback");
    }
    TcpStream::connect_timeout(&endpoint, Duration::from_secs(2))
        .context("remote daemon descriptor is stale")?;
    Ok(descriptor)
}

fn remote_descriptor(descriptor: DaemonDescriptor) -> RemoteDaemonDescriptor {
    RemoteDaemonDescriptor {
        instance_id: descriptor.instance_id,
        daemon_generation: descriptor.daemon_generation,
        pid: descriptor.pid,
        endpoint: descriptor.endpoint.to_string(),
        server_version: descriptor.server_version,
        protocol: descriptor.protocol,
    }
}

fn instance_audience(config: &Config, daemon: &RemoteDaemonDescriptor) -> String {
    config
        .auth
        .public_base_url
        .clone()
        .unwrap_or_else(|| format!("sift:instance:{}", daemon.instance_id))
}

fn key_challenge_message(audience: &str, nonce: &str) -> String {
    format!("sift-key-auth-v1\n{audience}\n{nonce}")
}

fn ensure_secret_key(path: &Path) -> anyhow::Result<()> {
    if path.exists() {
        return verify_private_file(path);
    }
    let mut bytes = [0_u8; 32];
    getrandom::getrandom(&mut bytes).context("generating remote secret key")?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    private_file_mode(&mut options);
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return verify_private_file(path)
        }
        Err(error) => return Err(error).context("creating remote secret key"),
    };
    for byte in bytes {
        write!(file, "{byte:02x}")?;
    }
    file.write_all(b"\n")?;
    file.sync_all()?;
    verify_private_file(path)
}

fn ensure_private_dir(path: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(path)
        .with_context(|| format!("creating private directory: {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn verify_private_personal_state(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)?.permissions().mode();
        if mode & 0o077 != 0 {
            bail!("personal remote state must not be accessible by group or other users");
        }
    }
    Ok(())
}

fn verify_private_file(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)?.permissions().mode();
        if mode & 0o077 != 0 {
            bail!("secret key file must have mode 0600 or stricter");
        }
    }
    Ok(())
}

#[cfg(unix)]
fn private_file_mode(options: &mut std::fs::OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn private_file_mode(_options: &mut std::fs::OpenOptions) {}

pub fn default_remote_state_dir() -> PathBuf {
    PathBuf::from(".local/state/sift/remote")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_config_is_daemon_ssh_proxy_with_durable_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let config = prepare_remote_config(dir.path()).unwrap();
        assert_eq!(config.mode, RuntimeMode::Daemon);
        assert_eq!(config.transport, Transport::SshProxy);
        assert!(!config.auth.loopback_bypass);
        assert_eq!(config.metadata.secret_backend, "file");
        assert!(config.metadata.secret_key_file.is_some());
        config.validate().unwrap();
    }

    #[test]
    fn probe_without_daemon_is_bounded_and_explicit() {
        let dir = tempfile::tempdir().unwrap();
        let response = probe(dir.path()).unwrap();
        assert_eq!(response.protocol, ProtocolRange::exact(1));
        assert!(response.daemon.is_none());
    }

    #[test]
    fn remote_secret_key_is_private_and_stable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key");
        ensure_secret_key(&path).unwrap();
        let first = std::fs::read(&path).unwrap();
        ensure_secret_key(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), first);
        assert_eq!(first.len(), 65);
    }
}
