//! Runtime identity and daemon singleton state for Phase H.

use crate::config::{Config, RuntimeMode};
use anyhow::{bail, Context};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

const INSTANCE_ID_FILE: &str = "instance-id";
const DAEMON_LOCK_FILE: &str = "daemon.lock";
const DAEMON_DESCRIPTOR_FILE: &str = "daemon.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonDescriptor {
    pub schema_version: u8,
    pub instance_id: String,
    pub daemon_generation: String,
    pub pid: u32,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub endpoint: SocketAddr,
    pub server_version: String,
    pub protocol: sift_protocol::ProtocolRange,
}

/// Holds the daemon lock for the serving lifetime and removes its ready
/// descriptor on clean shutdown.
pub struct RuntimeState {
    pub instance_id: String,
    pub daemon_generation: String,
    mode: RuntimeMode,
    state_dir: PathBuf,
    _lock: Option<File>,
    descriptor_published: bool,
}

impl RuntimeState {
    pub fn acquire(config: &Config) -> anyhow::Result<Self> {
        let state_dir = config.runtime_state_dir();
        std::fs::create_dir_all(&state_dir)
            .with_context(|| format!("creating runtime state dir: {}", state_dir.display()))?;
        make_private_dir(&state_dir)?;

        let instance_id = load_or_create_instance_id(&state_dir)?;
        let daemon_generation = uuid::Uuid::new_v4().to_string();
        let lock = if config.mode == RuntimeMode::Daemon {
            let path = state_dir.join(DAEMON_LOCK_FILE);
            let file = private_open(&path, true)?;
            if let Err(error) = FileExt::try_lock_exclusive(&file) {
                if error.kind() == std::io::ErrorKind::WouldBlock {
                    bail!(
                        "another sift daemon owns runtime state {}",
                        state_dir.display()
                    );
                }
                return Err(error)
                    .with_context(|| format!("locking daemon state: {}", path.display()));
            }
            let descriptor = state_dir.join(DAEMON_DESCRIPTOR_FILE);
            match std::fs::remove_file(&descriptor) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("removing stale daemon descriptor: {}", descriptor.display())
                    })
                }
            }
            Some(file)
        } else {
            None
        };

        Ok(Self {
            instance_id,
            daemon_generation,
            mode: config.mode,
            state_dir,
            _lock: lock,
            descriptor_published: false,
        })
    }

    pub fn publish_daemon(&mut self, endpoint: SocketAddr) -> anyhow::Result<()> {
        if self.mode != RuntimeMode::Daemon {
            return Ok(());
        }
        let descriptor = DaemonDescriptor {
            schema_version: 1,
            instance_id: self.instance_id.clone(),
            daemon_generation: self.daemon_generation.clone(),
            pid: std::process::id(),
            started_at: chrono::Utc::now(),
            endpoint,
            server_version: crate::VERSION.into(),
            protocol: sift_protocol::ProtocolRange::exact(sift_protocol::PROTOCOL_VERSION_NUMBER),
        };
        let final_path = self.state_dir.join(DAEMON_DESCRIPTOR_FILE);
        let staging_path = self.state_dir.join(format!(
            ".{DAEMON_DESCRIPTOR_FILE}.{}",
            self.daemon_generation
        ));
        let mut file = private_create_new(&staging_path)?;
        serde_json::to_writer(&mut file, &descriptor).context("encoding daemon descriptor")?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        std::fs::rename(&staging_path, &final_path)
            .with_context(|| format!("publishing daemon descriptor {}", final_path.display()))?;
        sync_dir(&self.state_dir)?;
        self.descriptor_published = true;
        Ok(())
    }
}

impl Drop for RuntimeState {
    fn drop(&mut self) {
        if self.descriptor_published {
            let _ = std::fs::remove_file(self.state_dir.join(DAEMON_DESCRIPTOR_FILE));
        }
    }
}

pub fn read_daemon_descriptor(state_dir: &Path) -> anyhow::Result<DaemonDescriptor> {
    let path = state_dir.join(DAEMON_DESCRIPTOR_FILE);
    let bytes = std::fs::read(&path)
        .with_context(|| format!("reading daemon descriptor: {}", path.display()))?;
    if bytes.len() > 16 * 1024 {
        bail!("daemon descriptor exceeds 16 KiB");
    }
    serde_json::from_slice(&bytes).context("decoding daemon descriptor")
}

fn load_or_create_instance_id(state_dir: &Path) -> anyhow::Result<String> {
    let path = state_dir.join(INSTANCE_ID_FILE);
    match private_create_new(&path) {
        Ok(mut file) => {
            let id = uuid::Uuid::new_v4().to_string();
            file.write_all(id.as_bytes())?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            sync_dir(state_dir)?;
            Ok(id)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let file = File::open(&path)
                .with_context(|| format!("opening instance id: {}", path.display()))?;
            let mut value = String::new();
            file.take(128).read_to_string(&mut value)?;
            let value = value.trim();
            let id = uuid::Uuid::parse_str(value)
                .with_context(|| format!("invalid instance id in {}", path.display()))?;
            Ok(id.to_string())
        }
        Err(error) => {
            Err(error).with_context(|| format!("creating instance id: {}", path.display()))
        }
    }
}

fn private_open(path: &Path, create: bool) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(create);
    private_mode(&mut options);
    options.open(path)
}

fn private_create_new(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    private_mode(&mut options);
    options.open(path)
}

#[cfg(unix)]
fn private_mode(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn private_mode(_options: &mut OpenOptions) {}

fn make_private_dir(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("securing runtime state dir: {}", path.display()))?;
    }
    Ok(())
}

fn sync_dir(path: &Path) -> std::io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn daemon_config(dir: &Path) -> Config {
        Config {
            mode: RuntimeMode::Daemon,
            runtime: crate::config::RuntimeConfig {
                state_dir: Some(dir.display().to_string()),
            },
            ..Config::default()
        }
    }

    #[test]
    fn instance_id_persists_but_generation_changes() {
        let dir = tempfile::tempdir().unwrap();
        let config = daemon_config(dir.path());
        let first = RuntimeState::acquire(&config).unwrap();
        let instance = first.instance_id.clone();
        let generation = first.daemon_generation.clone();
        drop(first);
        let second = RuntimeState::acquire(&config).unwrap();
        assert_eq!(second.instance_id, instance);
        assert_ne!(second.daemon_generation, generation);
    }

    #[test]
    fn daemon_lock_is_exclusive_and_descriptor_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let config = daemon_config(dir.path());
        let mut first = RuntimeState::acquire(&config).unwrap();
        assert!(RuntimeState::acquire(&config).is_err());
        let endpoint = "127.0.0.1:43123".parse().unwrap();
        first.publish_daemon(endpoint).unwrap();
        let descriptor = read_daemon_descriptor(dir.path()).unwrap();
        assert_eq!(descriptor.endpoint, endpoint);
        assert_eq!(descriptor.instance_id, first.instance_id);
        drop(first);
        assert!(!dir.path().join(DAEMON_DESCRIPTOR_FILE).exists());
    }
}
