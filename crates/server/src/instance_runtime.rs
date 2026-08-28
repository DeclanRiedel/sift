//! Destination-private realization state for a two-file Sift instance root.

use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context as _};
use fs2::FileExt as _;
use serde::{Deserialize, Serialize};
use sift_instance_config::{
    Deployment, LockFile, Manifest, RuntimeMode as InstanceRuntimeMode, SecretBackend, StaticPlan,
    Transport as InstanceTransport,
};

use crate::config::{Config, DeploymentPolicy, RuntimeMode, Transport};

pub const MANIFEST_FILE: &str = "sift.toml";
pub const LOCK_FILE: &str = "sift.lock";
const STATE_FORMAT_VERSION: u32 = 1;
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
const GENERATIONS_DIR: &str = "generations";
const CURRENT_FILE: &str = "current-generation.json";
const APPLY_LOCK_FILE: &str = "instance-apply.lock";

#[derive(Debug, Clone)]
pub struct InstanceRoot {
    pub root: PathBuf,
    pub manifest: Manifest,
    pub lock: LockFile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationRecord {
    pub format_version: u32,
    pub generation: u64,
    pub manifest_id: uuid::Uuid,
    pub configuration_digest: String,
    pub lock_digest: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub resolved_bindings: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyReport {
    pub changed: bool,
    pub generation: u64,
    pub configuration_digest: String,
    pub lock_digest: String,
    pub state_dir: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<sift_metadata::InstanceApplySummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationView {
    pub current: bool,
    #[serde(flatten)]
    pub record: GenerationRecord,
}

impl InstanceRoot {
    pub fn open(root: impl AsRef<Path>) -> anyhow::Result<Self> {
        let root = root.as_ref();
        let metadata = std::fs::symlink_metadata(root)
            .with_context(|| format!("reading server root metadata: {}", root.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("server root must be a real directory, not a symlink");
        }
        let root = std::fs::canonicalize(root)
            .with_context(|| format!("canonicalizing server root: {}", root.display()))?;
        let manifest = Manifest::parse(&read_bounded(&root.join(MANIFEST_FILE))?)
            .context("validating sift.toml")?;
        let lock =
            LockFile::parse(&read_bounded(&root.join(LOCK_FILE))?).context("parsing sift.lock")?;
        lock.verify_runtime(
            &manifest,
            crate::VERSION,
            sift_protocol::PROTOCOL_VERSION_NUMBER,
        )
        .context("verifying sift.lock against this Sift runtime")?;
        Ok(Self {
            root,
            manifest,
            lock,
        })
    }

    pub fn default_state_dir(&self) -> PathBuf {
        default_instance_state_root().join(self.manifest.manifest_id.to_string())
    }

    pub fn runtime_config(&self, state_dir: &Path) -> anyhow::Result<Config> {
        let deployment = match self.manifest.server.deployment {
            Deployment::Personal => DeploymentPolicy::Personal,
            Deployment::Team => DeploymentPolicy::Team,
        };
        let transport = match self.manifest.server.transport {
            InstanceTransport::Loopback => Transport::Loopback,
            InstanceTransport::Network => Transport::Network,
            InstanceTransport::SshProxy => Transport::SshProxy,
        };
        let mode = match self.manifest.server.mode {
            InstanceRuntimeMode::InProcess => RuntimeMode::InProcess,
            InstanceRuntimeMode::Daemon => RuntimeMode::Daemon,
            InstanceRuntimeMode::Container => RuntimeMode::Container,
        };
        let bind = match self.manifest.server.bind.as_str() {
            "auto-loopback" => "127.0.0.1:0".into(),
            "prompt-required" => bail!("server.bind is unresolved and requires operator input"),
            exact => exact.into(),
        };
        let mut config = Config {
            deployment,
            transport,
            mode,
            bind,
            ..Config::default()
        };
        config.runtime.state_dir = Some(state_dir.display().to_string());
        config.timeouts.request_secs = self.manifest.server.timeouts.request_secs;
        config.timeouts.shutdown_drain_secs = self.manifest.server.timeouts.shutdown_drain_secs;
        config.metadata.enabled = true;
        config.metadata.path = Some(state_dir.join("metadata.sqlite3").display().to_string());
        config.metadata.secret_backend = match self.manifest.server.metadata.secret_backend {
            SecretBackend::File => "file",
            SecretBackend::Keychain => "keychain",
            SecretBackend::Memory => "memory",
        }
        .into();
        config.metadata.secret_key_file = (self.manifest.server.metadata.secret_backend
            == SecretBackend::File)
            .then(|| state_dir.join("secret.key").display().to_string());
        // The declarative bootstrap identity is reconciled before runtime
        // startup. Never create the legacy implicit local principal here.
        config.metadata.bootstrap_local = false;
        config.metadata.store_sql = self.manifest.server.metadata.store_sql;
        // Personal local-device instances deliberately use the OS account and
        // verified loopback peer as their zero-sign-in bootstrap guard. Team,
        // network, and hosted-code instances never receive this bypass.
        config.auth.loopback_bypass =
            self.manifest.auth.github.flow == sift_instance_config::GithubFlow::LocalDevice;
        config.auth.public_base_url = self.manifest.server.public_base_url.clone();
        config.limits.max_http_result_rows =
            usize::try_from(self.manifest.server.limits.max_http_result_rows)
                .context("max_http_result_rows does not fit this platform")?;
        config.limits.max_http_result_bytes =
            usize::try_from(self.manifest.server.limits.max_http_result_bytes)
                .context("max_http_result_bytes does not fit this platform")?;
        config.workspaces.enabled = self.manifest.server.workspaces.enabled;
        config.workspaces.roots = self
            .manifest
            .server
            .workspaces
            .roots
            .iter()
            .map(|root| crate::config::WorkspaceRootConfig {
                handle: root.handle.clone(),
                path: root.path.clone(),
                read_only: root.read_only,
            })
            .collect();
        config.vcs.enabled = self.manifest.server.vcs.enabled;
        config.vcs.network_enabled = self.manifest.server.vcs.network_enabled;
        config.vcs.executable = self.manifest.server.vcs.executable.clone();
        config.vcs.local_timeout_secs = self.manifest.server.vcs.local_timeout_secs;
        config.vcs.network_timeout_secs = self.manifest.server.vcs.network_timeout_secs;
        config.vcs.max_output_bytes = usize::try_from(self.manifest.server.vcs.max_output_bytes)
            .context("vcs.max_output_bytes does not fit this platform")?;
        config.vcs.max_file_bytes = usize::try_from(self.manifest.server.vcs.max_file_bytes)
            .context("vcs.max_file_bytes does not fit this platform")?;
        config.vcs.max_status_entries =
            usize::try_from(self.manifest.server.vcs.max_status_entries)
                .context("vcs.max_status_entries does not fit this platform")?;
        config.vcs.max_history_page = self.manifest.server.vcs.max_history_page;
        config.vcs.max_commit_files = usize::try_from(self.manifest.server.vcs.max_commit_files)
            .context("vcs.max_commit_files does not fit this platform")?;
        config.vcs.max_diff_files = usize::try_from(self.manifest.server.vcs.max_diff_files)
            .context("vcs.max_diff_files does not fit this platform")?;
        config.vcs.max_diff_hunks = usize::try_from(self.manifest.server.vcs.max_diff_hunks)
            .context("vcs.max_diff_hunks does not fit this platform")?;
        config.vcs.max_diff_lines = usize::try_from(self.manifest.server.vcs.max_diff_lines)
            .context("vcs.max_diff_lines does not fit this platform")?;
        config.tenant_limits.ceilings.connections =
            Some(u64::from(self.manifest.server.limits.max_connections));
        config.tenant_limits.ceilings.concurrent_queries = Some(u64::from(
            self.manifest.server.limits.max_concurrent_queries,
        ));
        config
            .validate()
            .context("validating realized server settings")?;
        Ok(config)
    }

    pub fn static_plan(&self) -> anyhow::Result<StaticPlan> {
        self.manifest.static_plan(&self.lock).map_err(Into::into)
    }

    pub fn generations(&self, state_dir: &Path) -> anyhow::Result<Vec<GenerationView>> {
        let current = read_current(state_dir)?;
        let generations = state_dir.join(GENERATIONS_DIR);
        let entries = match std::fs::read_dir(&generations) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("reading generation store: {}", generations.display())
                })
            }
        };
        let mut records = Vec::new();
        for entry in entries {
            let entry = entry?;
            let Some(number) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u64>().ok())
            else {
                continue;
            };
            let file_type = entry.file_type()?;
            if !file_type.is_dir() || file_type.is_symlink() {
                bail!("generation {number} must be a regular directory");
            }
            let bytes = read_bounded_bytes(&entry.path().join("realization.json"), 64 * 1024)?;
            let record: GenerationRecord = serde_json::from_slice(&bytes)
                .with_context(|| format!("decoding generation {number}"))?;
            if record.format_version != STATE_FORMAT_VERSION
                || record.generation != number
                || record.manifest_id != self.manifest.manifest_id
            {
                bail!("generation {number} does not belong to this instance");
            }
            records.push(GenerationView {
                current: current
                    .as_ref()
                    .is_some_and(|selected| selected.generation == number),
                record,
            });
        }
        records.sort_by_key(|view| view.record.generation);
        Ok(records)
    }

    pub fn apply_generation(&self, state_dir: &Path) -> anyhow::Result<ApplyReport> {
        let (_lock_file, generations) = prepare_state(state_dir)?;

        let configuration_digest = self.manifest.configuration_digest()?;
        let lock_digest = self.lock.digest()?;
        if let Some(current) = read_current(state_dir)? {
            if current.configuration_digest == configuration_digest
                && current.lock_digest == lock_digest
            {
                return Ok(ApplyReport {
                    changed: false,
                    generation: current.generation,
                    configuration_digest,
                    lock_digest,
                    state_dir: state_dir.to_path_buf(),
                    metadata: None,
                });
            }
        }

        let record = self.stage_generation(state_dir, &generations)?;
        write_atomic_private(
            state_dir,
            CURRENT_FILE,
            &serde_json::to_vec_pretty(&record)?,
        )?;
        Ok(ApplyReport {
            changed: true,
            generation: record.generation,
            configuration_digest,
            lock_digest,
            state_dir: state_dir.to_path_buf(),
            metadata: None,
        })
    }

    /// Apply a complete instance while holding both the generation lock and
    /// the runtime maintenance lock. The generation pointer is switched only
    /// after metadata reconciliation commits.
    pub async fn apply(
        &self,
        state_dir: &Path,
        allow_destroy: bool,
    ) -> anyhow::Result<ApplyReport> {
        let (_apply_lock, generations) = prepare_state(state_dir)?;
        let configuration_digest = self.manifest.configuration_digest()?;
        let lock_digest = self.lock.digest()?;
        let current = read_current(state_dir)?;
        let unchanged = current.as_ref().is_some_and(|record| {
            record.manifest_id == self.manifest.manifest_id
                && record.configuration_digest == configuration_digest
                && record.lock_digest == lock_digest
        });
        let record = if unchanged {
            current.expect("checked current generation")
        } else {
            self.stage_generation(state_dir, &generations)?
        };

        let config = self.runtime_config(state_dir)?;
        ensure_file_secret_key(&config)?;
        let _maintenance = crate::runtime::acquire_maintenance_exclusive(&config)
            .context("a Sift runtime is using this destination; stop it before apply")?;
        let store = crate::metadata_runtime::open_metadata_store(&config)?
            .context("instance configuration unexpectedly disabled metadata")?;
        store
            .apply_migrations(false)
            .context("preparing instance metadata schema")?;
        let metadata = store
            .apply_instance_manifest(&self.manifest, &self.lock, record.generation, allow_destroy)
            .await
            .context("reconciling manifest-managed resources")?;

        if !unchanged {
            write_atomic_private(
                state_dir,
                CURRENT_FILE,
                &serde_json::to_vec_pretty(&record)?,
            )?;
        }
        Ok(ApplyReport {
            changed: !unchanged || metadata.changed,
            generation: record.generation,
            configuration_digest,
            lock_digest,
            state_dir: state_dir.to_path_buf(),
            metadata: Some(metadata),
        })
    }

    fn stage_generation(
        &self,
        state_dir: &Path,
        generations: &Path,
    ) -> anyhow::Result<GenerationRecord> {
        let configuration_digest = self.manifest.configuration_digest()?;
        let lock_digest = self.lock.digest()?;
        let generation = next_generation(generations)?;
        let staging = generations.join(format!(".{generation}.staging-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&staging)
            .with_context(|| format!("creating staged generation: {}", staging.display()))?;
        make_private_dir(&staging)?;
        let plan = self.static_plan()?;
        let mut bindings = std::collections::BTreeMap::new();
        let config = self.runtime_config(state_dir)?;
        bindings.insert("server.bind".into(), config.bind.clone());
        bindings.insert("runtime.state_dir".into(), state_dir.display().to_string());
        let record = GenerationRecord {
            format_version: STATE_FORMAT_VERSION,
            generation,
            manifest_id: self.manifest.manifest_id,
            configuration_digest: configuration_digest.clone(),
            lock_digest: lock_digest.clone(),
            created_at: chrono::Utc::now(),
            resolved_bindings: bindings,
        };
        write_private(
            &staging.join("normalized-manifest.json"),
            &serde_json::to_vec_pretty(&self.manifest)?,
        )?;
        write_private(
            &staging.join("lock.json"),
            &serde_json::to_vec_pretty(&self.lock)?,
        )?;
        write_private(
            &staging.join("plan.json"),
            &serde_json::to_vec_pretty(&plan)?,
        )?;
        write_private(
            &staging.join("realization.json"),
            &serde_json::to_vec_pretty(&record)?,
        )?;
        sync_dir(&staging)?;
        let final_dir = generations.join(generation.to_string());
        std::fs::rename(&staging, &final_dir)
            .with_context(|| format!("committing generation: {}", final_dir.display()))?;
        sync_dir(generations)?;
        Ok(record)
    }
}

fn prepare_state(state_dir: &Path) -> anyhow::Result<(File, PathBuf)> {
    std::fs::create_dir_all(state_dir)
        .with_context(|| format!("creating instance state: {}", state_dir.display()))?;
    make_private_dir(state_dir)?;
    let lock_file = private_open(&state_dir.join(APPLY_LOCK_FILE), true)?;
    lock_file
        .try_lock_exclusive()
        .context("another instance apply owns this destination")?;
    let generations = state_dir.join(GENERATIONS_DIR);
    std::fs::create_dir_all(&generations)
        .with_context(|| format!("creating generation store: {}", generations.display()))?;
    make_private_dir(&generations)?;
    Ok((lock_file, generations))
}

pub fn load_current_config(
    root: impl AsRef<Path>,
    state_dir: Option<&Path>,
) -> anyhow::Result<(InstanceRoot, GenerationRecord, Config)> {
    let instance = InstanceRoot::open(root)?;
    let state_dir = state_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| instance.default_state_dir());
    let current = read_current(&state_dir)?.context(
        "instance has no applied generation; run `sift instance apply <server-root>` first",
    )?;
    if current.manifest_id != instance.manifest.manifest_id
        || current.configuration_digest != instance.manifest.configuration_digest()?
        || current.lock_digest != instance.lock.digest()?
    {
        bail!("server root differs from the applied generation; review and apply it first");
    }
    let config = instance.runtime_config(&state_dir)?;
    Ok((instance, current, config))
}

pub fn ensure_file_secret_key(config: &Config) -> anyhow::Result<()> {
    if config.metadata.secret_backend != "file" {
        return Ok(());
    }
    let path = Path::new(
        config
            .metadata
            .secret_key_file
            .as_deref()
            .context("file secret backend has no destination key path")?,
    );
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                bail!("secret key path must be a regular non-symlink file");
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                if metadata.permissions().mode() & 0o077 != 0 {
                    bail!("secret key file must not be accessible by group or other users");
                }
            }
            return Ok(());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("checking destination secret key"),
    }
    let mut key = [0_u8; 32];
    getrandom::getrandom(&mut key)
        .map_err(|error| anyhow::anyhow!("generating destination secret key: {error}"))?;
    let mut encoded = String::with_capacity(65);
    for byte in key {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded.push('\n');
    write_private(path, encoded.as_bytes()).context("writing destination secret key")
}

fn default_instance_state_root() -> PathBuf {
    #[cfg(target_os = "windows")]
    if let Some(root) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(root).join("Sift").join("instances");
    }
    #[cfg(target_os = "macos")]
    if let Some(root) = std::env::var_os("HOME") {
        return PathBuf::from(root)
            .join("Library")
            .join("Application Support")
            .join("Sift")
            .join("instances");
    }
    if let Some(root) = std::env::var_os("XDG_STATE_HOME") {
        return PathBuf::from(root).join("sift").join("instances");
    }
    if let Some(root) = std::env::var_os("HOME") {
        return PathBuf::from(root)
            .join(".local")
            .join("state")
            .join("sift")
            .join("instances");
    }
    std::env::temp_dir().join("sift").join("instances")
}

fn read_bounded(path: &Path) -> anyhow::Result<String> {
    String::from_utf8(read_bounded_bytes(path, MAX_CONFIG_BYTES)?)
        .with_context(|| format!("{} must be UTF-8", path.display()))
}

fn read_bounded_bytes(path: &Path, limit: u64) -> anyhow::Result<Vec<u8>> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("reading metadata: {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("{} must be a regular non-symlink file", path.display());
    }
    if metadata.len() > limit {
        bail!("{} exceeds the configuration byte limit", path.display());
    }
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("reading {}", path.display()))?;
    if bytes.len() as u64 > limit {
        bail!(
            "{} changed while reading or exceeds the byte limit",
            path.display()
        );
    }
    Ok(bytes)
}

fn read_current(state_dir: &Path) -> anyhow::Result<Option<GenerationRecord>> {
    let path = state_dir.join(CURRENT_FILE);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("reading metadata for {}", path.display()))
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("current generation pointer must be a regular non-symlink file");
    }
    if metadata.len() > 64 * 1024 {
        bail!("current generation record exceeds the byte limit");
    }
    let mut source = Vec::with_capacity(metadata.len() as usize);
    File::open(&path)
        .with_context(|| format!("opening {}", path.display()))?
        .take(64 * 1024 + 1)
        .read_to_end(&mut source)
        .with_context(|| format!("reading {}", path.display()))?;
    if source.len() > 64 * 1024 {
        bail!("current generation pointer changed while reading");
    }
    let record: GenerationRecord =
        serde_json::from_slice(&source).context("decoding current generation record")?;
    if record.format_version != STATE_FORMAT_VERSION {
        bail!("unsupported private generation format");
    }
    Ok(Some(record))
}

fn next_generation(generations: &Path) -> anyhow::Result<u64> {
    let mut maximum = 0_u64;
    for entry in std::fs::read_dir(generations)
        .with_context(|| format!("reading generation store: {}", generations.display()))?
    {
        let entry = entry?;
        if let Some(number) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse().ok())
        {
            maximum = maximum.max(number);
        }
    }
    maximum
        .checked_add(1)
        .context("generation counter overflow")
}

fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        make_private_dir(parent)?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    private_mode(&mut options);
    let mut file = options
        .open(path)
        .with_context(|| format!("creating private file: {}", path.display()))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn write_atomic_private(parent: &Path, name: &str, bytes: &[u8]) -> anyhow::Result<()> {
    let staging_name = format!(".{name}.{}", uuid::Uuid::new_v4());
    let staging = parent.join(staging_name);
    write_private(&staging, bytes)?;
    let destination = parent.join(name);
    #[cfg(windows)]
    if destination.exists() {
        let backup = parent.join(format!(".{name}.previous"));
        let _ = std::fs::remove_file(&backup);
        std::fs::rename(&destination, &backup)?;
        if let Err(error) = std::fs::rename(&staging, &destination) {
            let _ = std::fs::rename(&backup, &destination);
            return Err(error).context("replacing private generation pointer");
        }
        let _ = std::fs::remove_file(backup);
    }
    #[cfg(not(windows))]
    std::fs::rename(&staging, &destination)?;
    sync_dir(parent)
}

fn private_open(path: &Path, create: bool) -> anyhow::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(create);
    private_mode(&mut options);
    options
        .open(path)
        .with_context(|| format!("opening private file: {}", path.display()))
}

#[cfg(unix)]
fn private_mode(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt as _;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn private_mode(_options: &mut OpenOptions) {}

fn make_private_dir(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = std::fs::metadata(path)?.permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

fn sync_dir(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn copy_demo(root: &Path) {
        std::fs::create_dir(root).unwrap();
        let demo =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/reproducible-instance");
        std::fs::copy(demo.join(MANIFEST_FILE), root.join(MANIFEST_FILE)).unwrap();
        std::fs::copy(demo.join(LOCK_FILE), root.join(LOCK_FILE)).unwrap();
    }

    #[test]
    fn apply_is_immutable_and_idempotent() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("root");
        let state = directory.path().join("state");
        copy_demo(&root);
        let instance = InstanceRoot::open(&root).unwrap();
        let first = instance.apply_generation(&state).unwrap();
        let second = instance.apply_generation(&state).unwrap();
        assert!(first.changed);
        assert!(!second.changed);
        assert_eq!(first.generation, second.generation);
        assert!(state.join("generations/1/realization.json").is_file());
        let generations = instance.generations(&state).unwrap();
        assert_eq!(generations.len(), 1);
        assert!(generations[0].current);
    }

    #[test]
    fn unapplied_edit_cannot_start() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("root");
        let state = directory.path().join("state");
        copy_demo(&root);
        let instance = InstanceRoot::open(&root).unwrap();
        instance.apply_generation(&state).unwrap();
        let mut manifest = instance.manifest.clone();
        manifest.name = "edited-sift".into();
        std::fs::write(root.join(MANIFEST_FILE), manifest.to_toml_pretty().unwrap()).unwrap();
        assert!(load_current_config(&root, Some(&state)).is_err());
    }

    #[test]
    fn runtime_config_realizes_workspace_and_vcs_settings() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("root");
        let state = directory.path().join("state");
        let workspace = directory.path().join("workspace");
        copy_demo(&root);
        std::fs::create_dir(&workspace).unwrap();
        let mut instance = InstanceRoot::open(&root).unwrap();
        instance.manifest.server.workspaces.enabled = true;
        instance.manifest.server.workspaces.roots =
            vec![sift_instance_config::WorkspaceRootConfig {
                handle: "demo-postgres".into(),
                path: workspace.display().to_string(),
                read_only: false,
            }];
        instance.manifest.server.vcs.enabled = true;

        let config = instance.runtime_config(&state).unwrap();
        assert!(config.workspaces.enabled);
        assert_eq!(config.workspaces.roots[0].handle, "demo-postgres");
        assert_eq!(
            config.workspaces.roots[0].path,
            workspace.display().to_string()
        );
        assert!(config.vcs.enabled);
        assert!(!config.vcs.network_enabled);
    }
}
