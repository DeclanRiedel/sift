//! Signed desktop bundles. Hosted-server updates retain their own lifecycle.

use anyhow::{ensure, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, VerifyingKey};
use fs2::FileExt as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    fs::File,
    io::Read as _,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::AsyncWriteExt as _;

pub const LAUNCHER_VERSION: u32 = 1;
pub const RESTART_EXIT_CODE: i32 = 75;
pub const CHANNEL_HEADER: &[u8] = b"SIFT-DESKTOP-1\n";
pub const PAYLOADS: [&str; 7] = [
    "sift-desktop",
    "sift-server",
    "sift-launcher",
    "sift-admin",
    "sift-remote",
    "sift",
    "sift-desktop-launcher",
];
const MAX_MANIFEST: usize = 64 * 1024;
const MAX_ARTIFACT: u64 = 512 * 1024 * 1024;
const MAX_BUNDLE: u64 = 2 * 1024 * 1024 * 1024;
const EMBEDDED_KEYS: &str = match option_env!("SIFT_DESKTOP_RELEASE_PUBLIC_KEYS") {
    Some(keys) => keys,
    None => "",
};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub channel: String,
    pub channel_url: String,
    pub state_dir: PathBuf,
    /// Existing trusted installation; never changed or cleaned by the updater.
    pub bootstrap_dir: PathBuf,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let config: Self = serde_json::from_slice(&read_bounded(path, MAX_MANIFEST)?)?;
        ensure!(
            config.state_dir.is_absolute() && config.bootstrap_dir.is_absolute(),
            "update directories must be absolute"
        );
        ensure!(
            !config.state_dir.starts_with(&config.bootstrap_dir)
                && !config.bootstrap_dir.starts_with(&config.state_dir),
            "update state must be separate from the installed package"
        );
        ensure!(safe_channel(&config.channel), "invalid update channel");
        https(&config.channel_url)?;
        Ok(config)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: u32,
    pub product: String,
    pub channel: String,
    pub sequence: u64,
    pub version: String,
    pub source_commit: String,
    pub published_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub minimum_launcher_version: u32,
    pub protocol: sift_protocol::ProtocolRange,
    pub targets: Vec<Target>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub target: String,
    pub files: Vec<Payload>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Payload {
    pub name: String,
    pub url: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Pointers {
    pub current: Option<String>,
    pub previous: Option<String>,
    pub pending: Option<String>,
    pub attempting: Option<String>,
    pub failed: Option<String>,
    pub highest_sequence: u64,
    pub highest_manifest: Option<String>,
}

#[derive(Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct Status {
    pub current_version: Option<String>,
    pub current_commit: Option<String>,
    pub pending_version: Option<String>,
    pub last_check: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct Updater {
    pub config: Config,
    keys: Vec<VerifyingKey>,
    client: reqwest::Client,
}

pub struct Release {
    pub digest: String,
    pub manifest: Manifest,
    pub directory: PathBuf,
}

impl Updater {
    pub fn new(config: Config) -> Result<Self> {
        Self::with_keys(config, EMBEDDED_KEYS)
    }

    fn with_keys(config: Config, encoded: &str) -> Result<Self> {
        let keys = encoded
            .split(',')
            .filter(|value| !value.is_empty())
            .map(|value| {
                let raw: [u8; 32] = URL_SAFE_NO_PAD
                    .decode(value)?
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("release public key must be 32 bytes"))?;
                Ok(VerifyingKey::from_bytes(&raw)?)
            })
            .collect::<Result<Vec<_>>>()?;
        ensure!(!keys.is_empty(), "launcher has no embedded release trust");
        ensure!(
            cfg!(target_os = "linux") && cfg!(target_arch = "x86_64"),
            "desktop updates currently support Linux x86-64 only"
        );
        private_dir(&config.state_dir)?;
        private_dir(&config.state_dir.join("versions"))?;
        let tls = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()?
        .with_root_certificates(rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        })
        .with_no_client_auth();
        let client = reqwest::Client::builder()
            .use_preconfigured_tls(tls)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(600))
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 5 || attempt.url().scheme() != "https" {
                    attempt.stop()
                } else {
                    attempt.follow()
                }
            }))
            .build()?;
        Ok(Self {
            config,
            keys,
            client,
        })
    }

    pub fn lock(&self, name: &str) -> Result<File> {
        ensure!(
            matches!(name, "run" | "state" | "download"),
            "invalid update lock"
        );
        let file = ordinary_file(&self.config.state_dir.join(format!("{name}.lock")), false)?;
        if name == "state" {
            file.lock_exclusive()?;
        } else {
            file.try_lock_exclusive()
                .context("another update owner is active")?;
        }
        Ok(file)
    }

    pub fn pointers(&self) -> Result<Pointers> {
        let path = self.config.state_dir.join("versions.json");
        if !path.try_exists()? {
            return Ok(Pointers::default());
        }
        let pointers: Pointers = serde_json::from_slice(&read_bounded(&path, MAX_MANIFEST)?)?;
        for digest in [
            &pointers.current,
            &pointers.previous,
            &pointers.pending,
            &pointers.attempting,
            &pointers.failed,
            &pointers.highest_manifest,
        ]
        .into_iter()
        .flatten()
        {
            ensure!(is_digest(digest), "invalid release pointer");
        }
        Ok(pointers)
    }

    fn write_pointers(&self, pointers: &Pointers) -> Result<()> {
        atomic_json(&self.config.state_dir.join("versions.json"), pointers)
    }

    fn verify_manifest(&self, raw: &[u8], signature: &[u8], fresh: bool) -> Result<Manifest> {
        ensure!(
            raw.len() <= MAX_MANIFEST && signature.len() <= 1024,
            "release metadata is oversized"
        );
        let signature = Signature::from_slice(
            &URL_SAFE_NO_PAD.decode(std::str::from_utf8(signature)?.trim())?,
        )?;
        ensure!(
            self.keys
                .iter()
                .any(|key| key.verify_strict(raw, &signature).is_ok()),
            "desktop release signature is invalid"
        );
        let manifest: Manifest = serde_json::from_slice(raw)?;
        ensure!(
            manifest.schema_version == 1 && manifest.product == "sift-desktop",
            "unsupported desktop manifest"
        );
        ensure!(
            manifest.channel == self.config.channel,
            "desktop release channel mismatch"
        );
        ensure!(
            manifest.sequence > 0 && manifest.minimum_launcher_version <= LAUNCHER_VERSION,
            "desktop release requires another launcher"
        );
        ensure!(
            is_commit(&manifest.source_commit),
            "invalid release source identity"
        );
        semver::Version::parse(&manifest.version)?;
        ensure!(
            manifest.expires_at > manifest.published_at,
            "invalid release lifetime"
        );
        if fresh {
            ensure!(
                manifest.expires_at > Utc::now()
                    && manifest.published_at <= Utc::now() + chrono::Duration::minutes(5),
                "desktop release is expired or not yet published"
            );
        }
        ensure!(
            manifest.protocol.minimum > 0 && manifest.protocol.maximum >= manifest.protocol.minimum,
            "desktop release protocol range is invalid"
        );
        ensure!(
            !manifest.targets.is_empty() && manifest.targets.len() <= 8,
            "invalid release targets"
        );
        let mut names = std::collections::HashSet::new();
        for target in &manifest.targets {
            ensure!(
                target.target == "linux-x86_64" && names.insert(&target.target),
                "unsupported or duplicate desktop target"
            );
            ensure!(
                target.files.len() == PAYLOADS.len(),
                "desktop bundle must contain all sibling executables"
            );
            let mut files = std::collections::HashSet::new();
            let mut size = 0_u64;
            for file in &target.files {
                ensure!(
                    PAYLOADS.contains(&file.name.as_str()) && files.insert(&file.name),
                    "unsafe or duplicate desktop payload"
                );
                ensure!(
                    file.bytes > 0 && file.bytes <= MAX_ARTIFACT && is_digest(&file.sha256),
                    "invalid desktop payload bounds"
                );
                size = size
                    .checked_add(file.bytes)
                    .context("desktop bundle size overflow")?;
                https(&file.url)?;
            }
            ensure!(size <= MAX_BUNDLE, "desktop bundle exceeds size limit");
        }
        Ok(manifest)
    }

    fn release_manifest(&self, digest: &str) -> Result<Release> {
        ensure!(is_digest(digest), "invalid release pointer");
        let directory = self.config.state_dir.join("versions").join(digest);
        check_directory(&directory)?;
        let raw = read_bounded(&directory.join("manifest.json"), MAX_MANIFEST)?;
        ensure!(
            hex_digest(&raw) == digest,
            "cached release manifest changed"
        );
        let signature = read_bounded(&directory.join("manifest.sig"), 1024)?;
        // Expiry gates new downloads, not launching a previously accepted installation.
        let manifest = self.verify_manifest(&raw, &signature, false)?;
        Ok(Release {
            digest: digest.into(),
            manifest,
            directory,
        })
    }

    pub fn release(&self, digest: &str) -> Result<Release> {
        let release = self.release_manifest(digest)?;
        for file in &release.manifest.targets[0].files {
            verify_payload(&release.directory.join(&file.name), file)?;
        }
        Ok(release)
    }

    pub async fn check(&self) -> Result<Option<String>> {
        let _download = self.lock("download")?;
        let channel = self
            .fetch_small(&self.config.channel_url, MAX_MANIFEST + 128)
            .await?;
        let (raw, signature) = split_channel(&channel)?;
        let signature = URL_SAFE_NO_PAD.encode(signature).into_bytes();
        let manifest = self.verify_manifest(raw, &signature, true)?;
        let digest = hex_digest(raw);
        {
            let _state = self.lock("state")?;
            let state = self.pointers()?;
            if !new_sequence(&state, &manifest, &digest)? {
                return Ok(None);
            }
            if let Some(current) = state.current.as_ref() {
                ensure!(
                    semver::Version::parse(&manifest.version)?
                        >= semver::Version::parse(
                            &self.release_manifest(current)?.manifest.version
                        )?,
                    "desktop release attempts a version downgrade"
                );
            }
        }
        let stage = tempfile::Builder::new()
            .prefix(".download-")
            .tempdir_in(self.config.state_dir.join("versions"))?;
        for payload in &manifest.targets[0].files {
            self.download(payload, &stage.path().join(&payload.name))
                .await?;
        }
        write_new(&stage.path().join("manifest.json"), raw)?;
        write_new(&stage.path().join("manifest.sig"), &signature)?;
        sync_dir(stage.path())?;
        let directory = self.config.state_dir.join("versions").join(&digest);
        if directory.try_exists()? {
            self.release(&digest)?;
        } else {
            std::fs::rename(stage.path(), &directory)?;
            sync_dir(&self.config.state_dir.join("versions"))?;
        }
        let _state = self.lock("state")?;
        let mut pointers = self.pointers()?;
        if !new_sequence(&pointers, &manifest, &digest)? {
            return Ok(None);
        }
        if let Some(current) = pointers.current.as_ref() {
            ensure!(
                semver::Version::parse(&manifest.version)?
                    >= semver::Version::parse(&self.release_manifest(current)?.manifest.version)?,
                "desktop release attempts a version downgrade"
            );
        }
        let old_pending = pointers.pending.replace(digest.clone());
        pointers.highest_sequence = manifest.sequence;
        pointers.highest_manifest = Some(digest);
        self.write_pointers(&pointers)?;
        if let Some(old) = old_pending {
            let _ = self.prune(&old, &pointers);
        }
        Ok(Some(manifest.version))
    }

    async fn fetch_small(&self, url: &str, maximum: usize) -> Result<Vec<u8>> {
        let mut response = self
            .client
            .get(https(url)?)
            .send()
            .await?
            .error_for_status()?;
        ensure!(
            response.url().scheme() == "https",
            "release transport must remain HTTPS"
        );
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                bytes.len().saturating_add(chunk.len()) <= maximum,
                "release metadata exceeded its limit"
            );
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    async fn download(&self, payload: &Payload, path: &Path) -> Result<()> {
        let file = ordinary_file(path, true)?;
        let mut file = tokio::fs::File::from_std(file);
        let mut response = self
            .client
            .get(https(&payload.url)?)
            .send()
            .await?
            .error_for_status()?;
        ensure!(
            response.url().scheme() == "https",
            "release transport must remain HTTPS"
        );
        let mut bytes = 0_u64;
        let mut digest = Sha256::new();
        while let Some(chunk) = response.chunk().await? {
            bytes = bytes
                .checked_add(chunk.len() as u64)
                .context("payload length overflow")?;
            ensure!(
                bytes <= payload.bytes,
                "desktop payload exceeds signed size"
            );
            digest.update(&chunk);
            file.write_all(&chunk).await?;
        }
        ensure!(
            bytes == payload.bytes && hex_bytes(&digest.finalize()) == payload.sha256,
            "desktop payload length or digest mismatch"
        );
        file.sync_all().await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            file.set_permissions(std::fs::Permissions::from_mode(0o700))
                .await?;
        }
        Ok(())
    }

    /// Claim before starting a candidate. A crashed owner never silently retries it.
    pub fn claim(&self) -> Result<Option<Release>> {
        let _lock = self.lock("state")?;
        let mut pointers = self.pointers()?;
        if let Some(interrupted) = pointers.attempting.take() {
            pointers.failed = Some(interrupted);
        }
        let candidate = pointers.pending.take();
        pointers.attempting = candidate.clone();
        self.write_pointers(&pointers)?;
        candidate.map(|digest| self.release(&digest)).transpose()
    }

    pub fn settle(&self, digest: &str, healthy: bool) -> Result<()> {
        let _lock = self.lock("state")?;
        let mut pointers = self.pointers()?;
        ensure!(
            pointers.attempting.as_deref() == Some(digest),
            "desktop candidate ownership changed"
        );
        pointers.attempting = None;
        let mut obsolete = pointers.failed.take().into_iter().collect::<Vec<_>>();
        if healthy {
            obsolete.extend(pointers.previous.take());
            pointers.previous = pointers.current.replace(digest.into());
        } else {
            pointers.failed = Some(digest.into());
        }
        self.write_pointers(&pointers)?;
        for old in obsolete {
            let _ = self.prune(&old, &pointers);
        }
        Ok(())
    }

    fn prune(&self, digest: &str, pointers: &Pointers) -> Result<()> {
        if [
            &pointers.current,
            &pointers.previous,
            &pointers.pending,
            &pointers.attempting,
            &pointers.failed,
        ]
        .into_iter()
        .flatten()
        .any(|value| value == digest)
        {
            return Ok(());
        }
        let release = self.release(digest)?;
        // Only an explicitly superseded, authenticated bundle inside this root is removed.
        for name in PAYLOADS
            .into_iter()
            .chain(["manifest.json", "manifest.sig"])
        {
            std::fs::remove_file(release.directory.join(name))?;
        }
        std::fs::remove_dir(&release.directory)?;
        Ok(())
    }

    pub fn current_directory(&self) -> Result<PathBuf> {
        let _lock = self.lock("state")?;
        match self.pointers()?.current {
            Some(digest) => Ok(self.release(&digest)?.directory),
            None => Ok(self.config.bootstrap_dir.clone()),
        }
    }

    pub fn publish_status(&self, error: Option<String>) -> Result<Status> {
        let _lock = self.lock("state")?;
        let pointers = self.pointers()?;
        let current = pointers
            .current
            .map(|digest| self.release_manifest(&digest))
            .transpose()?;
        let pending_version = pointers
            .pending
            .map(|digest| {
                self.release_manifest(&digest)
                    .map(|release| release.manifest.version)
            })
            .transpose()?;
        let status = Status {
            current_version: current
                .as_ref()
                .map(|release| release.manifest.version.clone()),
            current_commit: current.map(|release| release.manifest.source_commit),
            pending_version,
            last_check: Some(Utc::now()),
            error,
        };
        atomic_json(&self.config.state_dir.join("status.json"), &status)?;
        Ok(status)
    }
}

fn new_sequence(pointers: &Pointers, manifest: &Manifest, digest: &str) -> Result<bool> {
    ensure!(
        manifest.sequence >= pointers.highest_sequence,
        "desktop release sequence is stale"
    );
    if manifest.sequence == pointers.highest_sequence {
        ensure!(
            pointers.highest_manifest.as_deref() == Some(digest),
            "desktop release reuses a sequence with different content"
        );
        return Ok(false);
    }
    Ok(true)
}

pub fn split_channel(bytes: &[u8]) -> Result<(&[u8], &[u8])> {
    let framed = bytes
        .strip_prefix(CHANNEL_HEADER)
        .context("invalid desktop channel framing")?;
    ensure!(
        framed.len() > 64 && framed.len() <= MAX_MANIFEST + 64,
        "invalid desktop channel length"
    );
    let (signature, manifest) = framed.split_at(64);
    Ok((manifest, signature))
}

pub fn read_status(path: &Path) -> Result<Status> {
    Ok(serde_json::from_slice(&read_bounded(path, MAX_MANIFEST)?)?)
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StartupReceipt {
    pub nonce: String,
    pub source_commit: String,
    pub protocol: u32,
}

/// Called after the desktop creates its first window, never from a probe process.
pub fn acknowledge_startup(source_commit: &str) -> Result<()> {
    let Some(path) = std::env::var_os("SIFT_DESKTOP_UPDATE_READY_FILE") else {
        return Ok(());
    };
    let nonce = std::env::var("SIFT_DESKTOP_UPDATE_READY_NONCE")?;
    write_new(
        Path::new(&path),
        &serde_json::to_vec(&StartupReceipt {
            nonce,
            source_commit: source_commit.into(),
            protocol: sift_protocol::PROTOCOL_VERSION_NUMBER,
        })?,
    )
}

pub fn read_startup(path: &Path, nonce: &str, release: &Release) -> Result<()> {
    let receipt: StartupReceipt = serde_json::from_slice(&read_bounded(path, 4096)?)?;
    ensure!(
        receipt.nonce == nonce
            && receipt.source_commit == release.manifest.source_commit
            && receipt.protocol >= release.manifest.protocol.minimum
            && receipt.protocol <= release.manifest.protocol.maximum,
        "desktop startup acknowledgement mismatch"
    );
    Ok(())
}

fn https(value: &str) -> Result<url::Url> {
    let url = url::Url::parse(value)?;
    ensure!(
        url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none(),
        "release URLs must be HTTPS without credentials or fragments"
    );
    Ok(url)
}

fn safe_channel(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn is_digest(value: &str) -> bool {
    value.len() == 64 && lower_hex(value)
}
fn is_commit(value: &str) -> bool {
    value.len() == 40 && lower_hex(value)
}
fn lower_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn hex_digest(bytes: &[u8]) -> String {
    hex_bytes(&Sha256::digest(bytes))
}

pub fn private_dir(path: &Path) -> Result<()> {
    ensure!(
        path.is_absolute(),
        "private update directory must be absolute"
    );
    let mut parent = PathBuf::new();
    for component in path.components() {
        ensure!(
            !matches!(component, std::path::Component::ParentDir),
            "private update directory cannot escape its root"
        );
        parent.push(component);
        match std::fs::symlink_metadata(&parent) {
            Ok(_) => check_directory(&parent)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut builder = std::fs::DirBuilder::new();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::DirBuilderExt as _;
                    builder.mode(0o700);
                }
                builder.create(&parent)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        ensure!(
            std::fs::metadata(path)?.uid() == unsafe { libc::geteuid() },
            "update directory belongs to another user"
        );
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn check_directory(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "update directory is not an ordinary directory"
    );
    Ok(())
}

fn ordinary_file(path: &Path, exclusive: bool) -> Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).read(true);
    if exclusive {
        options.create_new(true);
    } else {
        options.create(true).truncate(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    check_file(&file)?;
    Ok(file)
}

fn check_file(file: &File) -> Result<()> {
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "update input is not an ordinary file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        ensure!(metadata.nlink() == 1, "update input has hardlink aliases");
    }
    Ok(())
}

fn open_read(path: &Path) -> Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    check_file(&file)?;
    Ok(file)
}

fn read_bounded(path: &Path, maximum: usize) -> Result<Vec<u8>> {
    let file = open_read(path)?;
    ensure!(
        file.metadata()?.len() <= maximum as u64,
        "update input is oversized"
    );
    let mut bytes = Vec::new();
    file.take(maximum as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= maximum, "update input grew past its limit");
    Ok(bytes)
}

fn verify_payload(path: &Path, payload: &Payload) -> Result<()> {
    let mut file = open_read(path)?;
    ensure!(
        file.metadata()?.len() == payload.bytes,
        "installed desktop payload length changed"
    );
    let mut digest = Sha256::new();
    let mut length = 0_u64;
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        length += count as u64;
        ensure!(length <= payload.bytes, "installed desktop payload grew");
        digest.update(&buffer[..count]);
    }
    ensure!(
        length == payload.bytes && hex_bytes(&digest.finalize()) == payload.sha256,
        "installed desktop payload digest changed"
    );
    Ok(())
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write as _;
    let mut file = ordinary_file(path, true)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

pub fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    use std::io::Write as _;
    let parent = path.parent().context("update state has no parent")?;
    check_directory(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(&serde_json::to_vec(value)?)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    sync_dir(parent)
}

fn sync_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(all(test, target_os = "linux", target_arch = "x86_64"))]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer as _, SigningKey};
    mod https;

    struct Fixture {
        root: tempfile::TempDir,
        updater: Updater,
        signing: SigningKey,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let seed: [u8; 32] = Sha256::digest(uuid::Uuid::new_v4().as_bytes()).into();
            let signing = SigningKey::from_bytes(&seed);
            let config = Config {
                channel: "development".into(),
                channel_url: "https://updates.invalid/desktop-channel".into(),
                state_dir: root.path().join("updates"),
                bootstrap_dir: root.path().join("original-package"),
            };
            let updater = Updater::with_keys(
                config,
                &URL_SAFE_NO_PAD.encode(signing.verifying_key().as_bytes()),
            )
            .unwrap();
            Self {
                root,
                updater,
                signing,
            }
        }

        fn manifest(&self, sequence: u64) -> Manifest {
            Manifest {
                schema_version: 1,
                product: "sift-desktop".into(),
                channel: "development".into(),
                sequence,
                version: format!("0.1.0-dev.{sequence}"),
                source_commit: "a".repeat(40),
                published_at: Utc::now() - chrono::Duration::minutes(1),
                expires_at: Utc::now() + chrono::Duration::days(1),
                minimum_launcher_version: 1,
                protocol: sift_protocol::ProtocolRange::exact(5),
                targets: vec![Target {
                    target: "linux-x86_64".into(),
                    files: PAYLOADS
                        .iter()
                        .map(|name| Payload {
                            name: (*name).into(),
                            url: format!("https://updates.invalid/{name}"),
                            bytes: name.len() as u64,
                            sha256: hex_digest(name.as_bytes()),
                        })
                        .collect(),
                }],
            }
        }

        fn signed(&self, manifest: &Manifest) -> (Vec<u8>, Vec<u8>) {
            let raw = serde_json::to_vec(manifest).unwrap();
            let signature = URL_SAFE_NO_PAD
                .encode(self.signing.sign(&raw).to_bytes())
                .into_bytes();
            (raw, signature)
        }

        fn install(&self, manifest: &Manifest) -> String {
            let (raw, signature) = self.signed(manifest);
            let digest = hex_digest(&raw);
            let directory = self.updater.config.state_dir.join("versions").join(&digest);
            private_dir(&directory).unwrap();
            write_new(&directory.join("manifest.json"), &raw).unwrap();
            write_new(&directory.join("manifest.sig"), &signature).unwrap();
            for name in PAYLOADS {
                write_new(&directory.join(name), name.as_bytes()).unwrap();
            }
            digest
        }

        fn assert_rejected(&self, manifest: &Manifest) {
            let (raw, signature) = self.signed(manifest);
            assert!(self
                .updater
                .verify_manifest(&raw, &signature, true)
                .is_err());
        }
    }

    #[test]
    fn signature_precedes_parsing_and_rejects_tampering_wrong_keys_and_duplicate_fields() {
        let fixture = Fixture::new();
        let manifest = fixture.manifest(1);
        let (mut raw, signature) = fixture.signed(&manifest);
        fixture
            .updater
            .verify_manifest(&raw, &signature, true)
            .unwrap();
        let frame = [
            CHANNEL_HEADER,
            &URL_SAFE_NO_PAD.decode(&signature).unwrap(),
            &raw,
        ]
        .concat();
        let (framed, detached) = split_channel(&frame).unwrap();
        fixture
            .updater
            .verify_manifest(framed, URL_SAFE_NO_PAD.encode(detached).as_bytes(), true)
            .unwrap();
        assert!(split_channel(b"SIFT-DESKTOP-1\nshort").is_err());
        assert!(split_channel(&vec![0; MAX_MANIFEST + 128]).is_err());
        raw[0] = b'!';
        let error = fixture
            .updater
            .verify_manifest(&raw, &signature, true)
            .unwrap_err();
        assert!(error.to_string().contains("signature"));
        let other = Fixture::new();
        let (raw, signature) = fixture.signed(&manifest);
        assert!(other
            .updater
            .verify_manifest(&raw, &signature, true)
            .is_err());
        let duplicate = String::from_utf8(raw)
            .unwrap()
            .replacen("{", "{\"sequence\":1,", 1)
            .into_bytes();
        let signature = URL_SAFE_NO_PAD.encode(fixture.signing.sign(&duplicate).to_bytes());
        assert!(fixture
            .updater
            .verify_manifest(&duplicate, signature.as_bytes(), true)
            .is_err());
    }

    #[test]
    fn authenticated_metadata_still_requires_safe_scope_layout_and_bounds() {
        let fixture = Fixture::new();
        let base = fixture.manifest(1);
        let mut manifest = base.clone();
        manifest.channel = "stable".into();
        fixture.assert_rejected(&manifest);
        let mut manifest = base.clone();
        manifest.product = "sift-server".into();
        fixture.assert_rejected(&manifest);
        let mut manifest = base.clone();
        manifest.expires_at = Utc::now() - chrono::Duration::seconds(1);
        fixture.assert_rejected(&manifest);
        let mut manifest = base.clone();
        manifest.minimum_launcher_version = 2;
        fixture.assert_rejected(&manifest);
        let mut manifest = base.clone();
        manifest.targets.push(manifest.targets[0].clone());
        fixture.assert_rejected(&manifest);
        let mut manifest = base.clone();
        manifest.targets[0].target = "windows-x86_64".into();
        fixture.assert_rejected(&manifest);
        let mut manifest = base.clone();
        manifest.targets[0].files[0].name = "../sift-desktop".into();
        fixture.assert_rejected(&manifest);
        let mut manifest = base.clone();
        manifest.targets[0].files[0] = manifest.targets[0].files[1].clone();
        fixture.assert_rejected(&manifest);
        let mut manifest = base.clone();
        manifest.targets[0].files[0].bytes = MAX_ARTIFACT + 1;
        fixture.assert_rejected(&manifest);
        let mut manifest = base.clone();
        manifest.targets[0].files[0].url = "http://updates.invalid/app".into();
        fixture.assert_rejected(&manifest);
        let mut manifest = base.clone();
        manifest.targets[0].files[0].url = "https://token@updates.invalid/app".into();
        fixture.assert_rejected(&manifest);
        let mut manifest = base;
        manifest.protocol = sift_protocol::ProtocolRange {
            minimum: 6,
            maximum: 5,
        };
        fixture.assert_rejected(&manifest);
    }

    #[test]
    fn sequence_replay_and_equivocation_cannot_replace_a_candidate() {
        let fixture = Fixture::new();
        let manifest = fixture.manifest(2);
        let (raw, _) = fixture.signed(&manifest);
        let digest = hex_digest(&raw);
        let state = Pointers {
            highest_sequence: 2,
            highest_manifest: Some(digest.clone()),
            ..Default::default()
        };
        assert!(!new_sequence(&state, &manifest, &digest).unwrap());
        assert!(new_sequence(&state, &fixture.manifest(1), &digest).is_err());
        assert!(new_sequence(&state, &manifest, &"b".repeat(64)).is_err());
        assert!(new_sequence(&state, &fixture.manifest(3), &"c".repeat(64)).unwrap());
    }

    #[test]
    fn startup_claims_survive_owner_crashes_without_retrying_or_committing() {
        let fixture = Fixture::new();
        let current = fixture.install(&fixture.manifest(1));
        let candidate = fixture.install(&fixture.manifest(2));
        fixture
            .updater
            .write_pointers(&Pointers {
                current: Some(current.clone()),
                pending: Some(candidate.clone()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(fixture.updater.claim().unwrap().unwrap().digest, candidate);
        assert_eq!(
            fixture.updater.pointers().unwrap().current.as_deref(),
            Some(current.as_str())
        );
        assert!(
            fixture.updater.claim().unwrap().is_none(),
            "interrupted startup must not retry itself"
        );
        let pointers = fixture.updater.pointers().unwrap();
        assert_eq!(pointers.failed.as_deref(), Some(candidate.as_str()));
        assert_eq!(pointers.current.as_deref(), Some(current.as_str()));
        assert!(fixture.updater.settle(&candidate, true).is_err());
    }

    #[test]
    fn activation_retains_known_good_and_only_prunes_owned_superseded_payloads() {
        let fixture = Fixture::new();
        let original = fixture.root.path().join("original-package");
        std::fs::create_dir(&original).unwrap();
        std::fs::write(original.join("sift-desktop"), b"package-manager-owned").unwrap();
        let first = fixture.install(&fixture.manifest(1));
        let second = fixture.install(&fixture.manifest(2));
        let third = fixture.install(&fixture.manifest(3));
        fixture
            .updater
            .write_pointers(&Pointers {
                current: Some(second.clone()),
                previous: Some(first.clone()),
                pending: Some(third.clone()),
                ..Default::default()
            })
            .unwrap();
        fixture.updater.claim().unwrap();
        assert!(fixture.updater.settle(&first, true).is_err());
        fixture.updater.settle(&third, true).unwrap();
        let state = fixture.updater.pointers().unwrap();
        assert_eq!(state.current.as_deref(), Some(third.as_str()));
        assert_eq!(state.previous.as_deref(), Some(second.as_str()));
        assert!(!fixture
            .updater
            .config
            .state_dir
            .join("versions")
            .join(first)
            .exists());
        assert_eq!(
            std::fs::read(original.join("sift-desktop")).unwrap(),
            b"package-manager-owned"
        );
        let failed = fixture.install(&fixture.manifest(4));
        let mut state = fixture.updater.pointers().unwrap();
        state.pending = Some(failed.clone());
        fixture.updater.write_pointers(&state).unwrap();
        fixture.updater.claim().unwrap();
        fixture.updater.settle(&failed, false).unwrap();
        assert_eq!(
            fixture.updater.pointers().unwrap().current.as_deref(),
            Some(third.as_str())
        );
    }

    #[test]
    fn cached_payloads_are_reverified_and_installed_expiry_does_not_brick_launches() {
        let fixture = Fixture::new();
        let mut manifest = fixture.manifest(1);
        manifest.published_at = Utc::now() - chrono::Duration::days(2);
        manifest.expires_at = Utc::now() - chrono::Duration::days(1);
        let digest = fixture.install(&manifest);
        fixture.updater.release(&digest).unwrap();
        fixture.assert_rejected(&manifest);
        let binary = fixture
            .updater
            .release(&digest)
            .unwrap()
            .directory
            .join("sift-desktop");
        std::fs::write(&binary, b"tamper").unwrap();
        assert!(fixture.updater.release(&digest).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_hardlinks_and_pointer_escapes_are_refused() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let digest = fixture.install(&fixture.manifest(1));
        let release = fixture.updater.release(&digest).unwrap();
        let binary = release.directory.join("sift-desktop");
        std::fs::hard_link(&binary, fixture.root.path().join("alias")).unwrap();
        assert!(fixture.updater.release(&digest).is_err());
        std::fs::remove_file(fixture.root.path().join("alias")).unwrap();
        std::fs::remove_file(&binary).unwrap();
        symlink(fixture.root.path().join("outside"), &binary).unwrap();
        assert!(fixture.updater.release(&digest).is_err());
        assert!(fixture.updater.release("../outside").is_err());
        symlink(fixture.root.path(), fixture.root.path().join("escape")).unwrap();
        assert!(private_dir(&fixture.root.path().join("escape/nested")).is_err());
        assert!(!fixture.root.path().join("nested").exists());
    }

    #[test]
    fn readiness_binds_fresh_attempt_source_and_signed_protocol() {
        let fixture = Fixture::new();
        let mut manifest = fixture.manifest(1);
        // Updating the desktop does not require upgrading this launcher with every API change.
        manifest.protocol = sift_protocol::ProtocolRange::exact(6);
        let release = fixture
            .updater
            .release(&fixture.install(&manifest))
            .unwrap();
        let path = fixture.root.path().join("receipt.json");
        let mut receipt = StartupReceipt {
            nonce: "new-attempt".into(),
            source_commit: manifest.source_commit,
            protocol: 6,
        };
        atomic_json(&path, &receipt).unwrap();
        read_startup(&path, "new-attempt", &release).unwrap();
        assert!(read_startup(&path, "old-attempt", &release).is_err());
        receipt.source_commit = "b".repeat(40);
        atomic_json(&path, &receipt).unwrap();
        assert!(read_startup(&path, "new-attempt", &release).is_err());
        receipt.source_commit = release.manifest.source_commit.clone();
        receipt.protocol = 5;
        atomic_json(&path, &receipt).unwrap();
        assert!(read_startup(&path, "new-attempt", &release).is_err());
    }
}
