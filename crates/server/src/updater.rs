//! Signed, restart-activated release staging (ADR-015).

use crate::config::{Config, RuntimeMode, UpdaterConfig};
use anyhow::{bail, Context};
use base64::Engine as _;
use chrono::{DateTime, Utc};
use ed25519_dalek::Verifier as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use sift_protocol::ProtocolRange;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncWriteExt as _;

const MANIFEST_SCHEMA_VERSION: u8 = 1;
const ARCHIVE_FORMAT: &str = "raw";
const EXECUTABLE_PATH: &str = "sift-server";
const MAX_MANIFEST_BYTES: usize = 256 * 1024;
const MAX_SIGNATURE_BYTES: usize = 1024;
const EMBEDDED_RELEASE_KEYS: &str = match option_env!("SIFT_RELEASE_PUBLIC_KEYS") {
    Some(value) => value,
    None => "",
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseManifest {
    pub schema_version: u8,
    pub channel: String,
    pub sequence: u64,
    pub release_version: String,
    pub published_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub minimum_updater_version: String,
    pub protocol: ProtocolRange,
    pub targets: Vec<ReleaseArtifact>,
    #[serde(default)]
    pub rollout: Option<Rollout>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseArtifact {
    pub target: String,
    pub artifact_url: String,
    pub byte_length: u64,
    pub sha256: String,
    pub archive_format: String,
    pub executable_path: String,
    #[serde(default)]
    pub sbom_url: Option<String>,
    #[serde(default)]
    pub symbols_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rollout {
    pub percentage: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
struct VersionPointers {
    current: Option<InstalledRelease>,
    previous: Option<InstalledRelease>,
    pending: Option<InstalledRelease>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstalledRelease {
    pub release_version: String,
    pub sequence: u64,
    pub target: String,
    pub sha256: String,
    pub executable: PathBuf,
}

#[derive(Debug, Clone)]
pub enum CheckOutcome {
    Current,
    Staged(InstalledRelease),
}

#[derive(Debug, Clone)]
pub struct Updater {
    config: UpdaterConfig,
    state_dir: PathBuf,
    trusted_keys: Vec<ed25519_dalek::VerifyingKey>,
    client: reqwest::Client,
}

impl Updater {
    pub fn from_config(config: &Config) -> anyhow::Result<Self> {
        if config.mode == RuntimeMode::Container {
            bail!("container mode cannot construct the self-updater");
        }
        let state_dir = config
            .updater
            .state_dir
            .as_ref()
            .map(PathBuf::from)
            .or_else(|| {
                config
                    .runtime
                    .state_dir
                    .as_ref()
                    .map(|path| Path::new(path).join("updates"))
            })
            .unwrap_or_else(|| PathBuf::from(".sift/updates"));
        Self::new(config.updater.clone(), state_dir, EMBEDDED_RELEASE_KEYS)
    }

    fn new(config: UpdaterConfig, state_dir: PathBuf, encoded_keys: &str) -> anyhow::Result<Self> {
        let trusted_keys = parse_release_keys(encoded_keys)?;
        if trusted_keys.is_empty() {
            bail!(
                "this binary has no embedded release keys; build with \
                 SIFT_RELEASE_PUBLIC_KEYS"
            );
        }
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(10 * 60))
            .redirect(reqwest::redirect::Policy::limited(3))
            .build()?;
        Ok(Self {
            config,
            state_dir,
            trusted_keys,
            client,
        })
    }

    pub async fn check_and_stage(&self) -> anyhow::Result<CheckOutcome> {
        ensure_private_dir(&self.state_dir)?;
        let manifest_url = explicit_https_url(
            self.config
                .manifest_url
                .as_deref()
                .context("updater manifest URL is not configured")?,
        )?;
        let signature_url = explicit_https_url(
            self.config
                .signature_url
                .as_deref()
                .context("updater signature URL is not configured")?,
        )?;
        let manifest_bytes = fetch_bounded(&self.client, manifest_url, MAX_MANIFEST_BYTES).await?;
        let signature = fetch_bounded(&self.client, signature_url, MAX_SIGNATURE_BYTES).await?;
        let (manifest, artifact) = self.verify_manifest_owned(&manifest_bytes, &signature)?;
        if semver::Version::parse(&manifest.release_version)?
            <= semver::Version::parse(crate::VERSION)?
        {
            return Ok(CheckOutcome::Current);
        }
        let installed = self
            .download_and_install(&manifest, artifact)
            .await
            .context("staging signed release")?;
        self.record_sequence(manifest.sequence)?;
        let mut pointers = self.read_pointers()?;
        pointers.pending = Some(installed.clone());
        self.write_pointers(&pointers)?;
        Ok(CheckOutcome::Staged(installed))
    }

    fn verify_manifest_owned(
        &self,
        raw: &[u8],
        detached_signature: &[u8],
    ) -> anyhow::Result<(ReleaseManifest, ReleaseArtifact)> {
        if raw.len() > MAX_MANIFEST_BYTES {
            bail!("release manifest exceeds 256 KiB");
        }
        verify_signature(&self.trusted_keys, raw, detached_signature)?;
        let manifest: ReleaseManifest =
            serde_json::from_slice(raw).context("parsing signed release manifest")?;
        validate_manifest(
            &manifest,
            &self.config.channel,
            self.observed_sequence()?,
            self.config.max_artifact_bytes,
        )?;
        let target = current_target();
        let mut matches = manifest
            .targets
            .iter()
            .filter(|artifact| artifact.target == target);
        let artifact = matches
            .next()
            .cloned()
            .with_context(|| format!("manifest has no artifact for target {target}"))?;
        if matches.next().is_some() {
            bail!("manifest contains duplicate artifacts for target {target}");
        }
        validate_artifact(&artifact, self.config.max_artifact_bytes)?;
        Ok((manifest, artifact))
    }

    async fn download_and_install(
        &self,
        manifest: &ReleaseManifest,
        artifact: ReleaseArtifact,
    ) -> anyhow::Result<InstalledRelease> {
        let url = explicit_https_url(&artifact.artifact_url)?;
        let staging_dir = self.state_dir.join("staging");
        ensure_private_dir(&staging_dir)?;
        let staging = staging_dir.join(format!("{}.part", uuid::Uuid::new_v4().simple()));
        let result = self
            .download_to_staging(url, &artifact, &staging)
            .await
            .and_then(|()| self.install_staging(manifest, &artifact, &staging));
        if result.is_err() {
            let _ = std::fs::remove_file(&staging);
        }
        result
    }

    async fn download_to_staging(
        &self,
        url: reqwest::Url,
        artifact: &ReleaseArtifact,
        staging: &Path,
    ) -> anyhow::Result<()> {
        let mut response = self.client.get(url).send().await?.error_for_status()?;
        if response.url().scheme() != "https" {
            bail!("artifact redirect left HTTPS");
        }
        if response
            .content_length()
            .is_some_and(|length| length != artifact.byte_length)
        {
            bail!("artifact Content-Length does not match signed length");
        }
        let file = create_private_file(staging)?;
        let mut file = tokio::fs::File::from_std(file);
        let mut length = 0_u64;
        let mut digest = Sha256::new();
        while let Some(chunk) = response.chunk().await? {
            length = length
                .checked_add(chunk.len() as u64)
                .context("artifact length overflow")?;
            if length > artifact.byte_length || length > self.config.max_artifact_bytes {
                bail!("artifact exceeded its signed or configured size limit");
            }
            digest.update(&chunk);
            file.write_all(&chunk).await?;
        }
        if length != artifact.byte_length {
            bail!("artifact was truncated");
        }
        if hex_digest(digest.finalize().as_slice()) != artifact.sha256 {
            bail!("artifact SHA-256 digest mismatch");
        }
        file.sync_all().await?;
        Ok(())
    }

    fn install_staging(
        &self,
        manifest: &ReleaseManifest,
        artifact: &ReleaseArtifact,
        staging: &Path,
    ) -> anyhow::Result<InstalledRelease> {
        verify_file(staging, artifact.byte_length, &artifact.sha256)?;
        let directory = self.state_dir.join("versions").join(format!(
            "{}-{}",
            manifest.release_version,
            &artifact.sha256[..16]
        ));
        ensure_private_dir(&directory)?;
        let executable = directory.join(EXECUTABLE_PATH);
        if executable.exists() {
            verify_file(&executable, artifact.byte_length, &artifact.sha256)?;
            std::fs::remove_file(staging)?;
        } else {
            std::fs::rename(staging, &executable)?;
            make_executable(&executable)?;
            sync_directory(&directory)?;
        }
        Ok(InstalledRelease {
            release_version: manifest.release_version.clone(),
            sequence: manifest.sequence,
            target: artifact.target.clone(),
            sha256: artifact.sha256.clone(),
            executable,
        })
    }

    pub fn pending_release(&self) -> anyhow::Result<Option<InstalledRelease>> {
        Ok(self.read_pointers()?.pending)
    }

    /// Release selected for the next owner launch. Remote bootstrap uses this
    /// same verified cache instead of adding an SSH-specific download path.
    pub fn selected_release(&self) -> anyhow::Result<Option<InstalledRelease>> {
        let pointers = self.read_pointers()?;
        Ok(pointers.pending.or(pointers.current))
    }

    /// Commit a candidate only after its owner observed readiness and a
    /// compatible ADR-016 handshake.
    pub fn commit_healthy_candidate(&self) -> anyhow::Result<InstalledRelease> {
        let mut pointers = self.read_pointers()?;
        let candidate = pointers.pending.take().context("no pending release")?;
        pointers.previous = pointers.current.take();
        pointers.current = Some(candidate.clone());
        self.write_pointers(&pointers)?;
        Ok(candidate)
    }

    /// Leave the known-good current pointer intact and discard selection of a
    /// candidate that failed process readiness or protocol negotiation.
    pub fn rollback_candidate(&self) -> anyhow::Result<Option<InstalledRelease>> {
        let mut pointers = self.read_pointers()?;
        let failed = pointers.pending.take();
        self.write_pointers(&pointers)?;
        Ok(failed)
    }

    fn observed_sequence(&self) -> anyhow::Result<u64> {
        let path = self
            .state_dir
            .join(format!("sequence-{}", self.config.channel));
        match std::fs::read_to_string(path) {
            Ok(value) => value
                .trim()
                .parse()
                .context("invalid updater sequence state"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(error.into()),
        }
    }

    fn record_sequence(&self, sequence: u64) -> anyhow::Result<()> {
        atomic_write(
            &self
                .state_dir
                .join(format!("sequence-{}", self.config.channel)),
            format!("{sequence}\n").as_bytes(),
        )
    }

    fn read_pointers(&self) -> anyhow::Result<VersionPointers> {
        let path = self.state_dir.join("versions.json");
        match std::fs::read(&path) {
            Ok(bytes) => {
                if bytes.len() > 64 * 1024 {
                    bail!("updater version pointer file is oversized");
                }
                serde_json::from_slice(&bytes).context("parsing updater version pointers")
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(VersionPointers::default())
            }
            Err(error) => Err(error.into()),
        }
    }

    fn write_pointers(&self, pointers: &VersionPointers) -> anyhow::Result<()> {
        atomic_write(
            &self.state_dir.join("versions.json"),
            &serde_json::to_vec(pointers)?,
        )
    }
}

pub fn spawn_background(config: &Config) -> anyhow::Result<()> {
    if !config.updater.enabled {
        return Ok(());
    }
    let updater = Updater::from_config(config)?;
    let delay = Duration::from_secs(config.updater.initial_delay_secs);
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        match updater.check_and_stage().await {
            Ok(CheckOutcome::Current) => tracing::debug!("release is current"),
            Ok(CheckOutcome::Staged(release)) => tracing::info!(
                release = %release.release_version,
                "signed update staged for activation on restart"
            ),
            Err(error) => tracing::warn!(%error, "background update check failed"),
        }
    });
    Ok(())
}

fn validate_manifest(
    manifest: &ReleaseManifest,
    channel: &str,
    observed_sequence: u64,
    max_artifact_bytes: u64,
) -> anyhow::Result<()> {
    if manifest.schema_version != MANIFEST_SCHEMA_VERSION {
        bail!("unsupported release manifest schema");
    }
    if manifest.channel != channel {
        bail!("release manifest channel mismatch");
    }
    if manifest.sequence <= observed_sequence {
        bail!("release manifest sequence is stale or replayed");
    }
    if manifest.published_at > Utc::now() + chrono::Duration::minutes(5) {
        bail!("release manifest publication time is in the future");
    }
    if manifest.expires_at <= Utc::now() || manifest.expires_at <= manifest.published_at {
        bail!("release manifest is expired or has an invalid lifetime");
    }
    let minimum = semver::Version::parse(&manifest.minimum_updater_version)
        .context("invalid minimum updater version")?;
    if minimum > semver::Version::parse(crate::VERSION)? {
        bail!("release requires a newer updater");
    }
    let release =
        semver::Version::parse(&manifest.release_version).context("invalid release version")?;
    if release < semver::Version::parse(crate::VERSION)? {
        bail!("release manifest attempts a downgrade");
    }
    if manifest
        .protocol
        .highest_common(ProtocolRange::exact(sift_protocol::PROTOCOL_VERSION_NUMBER))
        .is_none()
    {
        bail!("release protocol range is incompatible");
    }
    if manifest.targets.is_empty() || manifest.targets.len() > 64 {
        bail!("release manifest target count is invalid");
    }
    if manifest
        .rollout
        .as_ref()
        .is_some_and(|rollout| rollout.percentage > 100)
    {
        bail!("release rollout percentage exceeds 100");
    }
    if max_artifact_bytes == 0 {
        bail!("artifact size ceiling is zero");
    }
    Ok(())
}

fn validate_artifact(artifact: &ReleaseArtifact, max_bytes: u64) -> anyhow::Result<()> {
    if artifact.archive_format != ARCHIVE_FORMAT || artifact.executable_path != EXECUTABLE_PATH {
        bail!("unsupported or unsafe release archive layout");
    }
    if artifact.byte_length == 0 || artifact.byte_length > max_bytes {
        bail!("release artifact has an invalid or oversized length");
    }
    if artifact.sha256.len() != 64
        || !artifact
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        bail!("release artifact has an invalid SHA-256 digest");
    }
    explicit_https_url(&artifact.artifact_url)?;
    for optional_url in [&artifact.sbom_url, &artifact.symbols_url]
        .into_iter()
        .flatten()
    {
        explicit_https_url(optional_url)?;
    }
    Ok(())
}

fn verify_signature(
    keys: &[ed25519_dalek::VerifyingKey],
    raw: &[u8],
    detached: &[u8],
) -> anyhow::Result<()> {
    let encoded = std::str::from_utf8(detached)
        .context("release signature is not UTF-8")?
        .trim();
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .context("release signature is not base64url")?;
    let signature =
        ed25519_dalek::Signature::from_slice(&bytes).context("invalid Ed25519 signature")?;
    if !keys.iter().any(|key| key.verify(raw, &signature).is_ok()) {
        bail!("release manifest signature is not trusted");
    }
    Ok(())
}

fn parse_release_keys(encoded: &str) -> anyhow::Result<Vec<ed25519_dalek::VerifyingKey>> {
    encoded
        .split(',')
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(value.trim())
                .context("embedded release key is not base64url")?;
            let bytes: [u8; 32] = bytes
                .try_into()
                .map_err(|_| anyhow::anyhow!("embedded release key is not 32 bytes"))?;
            ed25519_dalek::VerifyingKey::from_bytes(&bytes)
                .context("embedded release key is not Ed25519")
        })
        .collect()
}

fn explicit_https_url(value: &str) -> anyhow::Result<reqwest::Url> {
    let url = reqwest::Url::parse(value).context("invalid release URL")?;
    if url.scheme() != "https" || url.host_str().is_none() {
        bail!("release URLs must be absolute HTTPS URLs");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("release URLs must not contain credentials");
    }
    Ok(url)
}

async fn fetch_bounded(
    client: &reqwest::Client,
    url: reqwest::Url,
    maximum: usize,
) -> anyhow::Result<Vec<u8>> {
    let mut response = client.get(url).send().await?.error_for_status()?;
    if response.url().scheme() != "https" {
        bail!("release metadata redirect left HTTPS");
    }
    if response
        .content_length()
        .is_some_and(|length| length > maximum as u64)
    {
        bail!("release metadata exceeds its size limit");
    }
    let mut bytes = Vec::with_capacity(maximum.min(4096));
    while let Some(chunk) = response.chunk().await? {
        if bytes.len().saturating_add(chunk.len()) > maximum {
            bail!("release metadata exceeds its size limit");
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn current_target() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

fn create_private_file(path: &Path) -> anyhow::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .with_context(|| format!("creating private staging file: {}", path.display()))
}

fn ensure_private_dir(path: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(path)?;
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("updater state path is not a real directory");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn make_executable(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn verify_file(path: &Path, expected_length: u64, expected_digest: &str) -> anyhow::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!("installed release path is not a regular file");
    }
    if metadata.len() != expected_length {
        bail!("installed release length mismatch");
    }
    let mut file = std::fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    if hex_digest(digest.finalize().as_slice()) != expected_digest {
        bail!("installed release digest mismatch");
    }
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = path.parent().context("atomic state path has no parent")?;
    ensure_private_dir(parent)?;
    let staging = parent.join(format!(".state-{}.tmp", uuid::Uuid::new_v4().simple()));
    let mut file = create_private_file(&staging)?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&staging, path)?;
        sync_directory(parent)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(staging);
    }
    result
}

fn sync_directory(path: &Path) -> anyhow::Result<()> {
    std::fs::File::open(path)?.sync_all()?;
    Ok(())
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Signer as _;

    fn fixture(directory: &Path) -> (Updater, ed25519_dalek::SigningKey, ReleaseManifest, Vec<u8>) {
        let signing = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(signing.verifying_key().to_bytes());
        let config = UpdaterConfig {
            enabled: true,
            channel: "stable".into(),
            manifest_url: Some("https://releases.example/manifest.json".into()),
            signature_url: Some("https://releases.example/manifest.sig".into()),
            state_dir: Some(directory.display().to_string()),
            max_artifact_bytes: 1024,
            initial_delay_secs: 0,
        };
        let updater = Updater::new(config, directory.into(), &encoded).unwrap();
        let artifact_bytes = b"fixture executable".to_vec();
        let manifest = ReleaseManifest {
            schema_version: 1,
            channel: "stable".into(),
            sequence: 1,
            release_version: "0.2.0".into(),
            published_at: Utc::now() - chrono::Duration::minutes(1),
            expires_at: Utc::now() + chrono::Duration::hours(1),
            minimum_updater_version: crate::VERSION.into(),
            protocol: ProtocolRange::exact(sift_protocol::PROTOCOL_VERSION_NUMBER),
            targets: vec![ReleaseArtifact {
                target: current_target(),
                artifact_url: "https://releases.example/sift-server".into(),
                byte_length: artifact_bytes.len() as u64,
                sha256: hex_digest(Sha256::digest(&artifact_bytes).as_slice()),
                archive_format: ARCHIVE_FORMAT.into(),
                executable_path: EXECUTABLE_PATH.into(),
                sbom_url: None,
                symbols_url: None,
            }],
            rollout: None,
        };
        (updater, signing, manifest, artifact_bytes)
    }

    fn signed(
        signing: &ed25519_dalek::SigningKey,
        manifest: &ReleaseManifest,
    ) -> (Vec<u8>, Vec<u8>) {
        let raw = serde_json::to_vec(manifest).unwrap();
        let signature = signing.sign(&raw);
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature.to_bytes());
        (raw, encoded.into_bytes())
    }

    #[test]
    fn signature_is_checked_before_json_and_tampering_fails() {
        let directory = tempfile::tempdir().unwrap();
        let (updater, signing, manifest, _) = fixture(directory.path());
        let (raw, signature) = signed(&signing, &manifest);
        updater.verify_manifest_owned(&raw, &signature).unwrap();

        let mut tampered = raw;
        tampered[0] ^= 1;
        assert!(updater
            .verify_manifest_owned(&tampered, &signature)
            .unwrap_err()
            .to_string()
            .contains("signature"));
        assert!(updater
            .verify_manifest_owned(b"not json", b"not a signature")
            .unwrap_err()
            .to_string()
            .contains("base64url"));
    }

    #[test]
    fn replay_expiry_target_and_archive_attacks_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let (updater, signing, mut manifest, _) = fixture(directory.path());
        updater.record_sequence(1).unwrap();
        let (raw, signature) = signed(&signing, &manifest);
        assert!(updater.verify_manifest_owned(&raw, &signature).is_err());

        manifest.sequence = 2;
        manifest.expires_at = Utc::now() - chrono::Duration::seconds(1);
        let (raw, signature) = signed(&signing, &manifest);
        assert!(updater.verify_manifest_owned(&raw, &signature).is_err());

        manifest.expires_at = Utc::now() + chrono::Duration::hours(1);
        manifest.targets[0].target = "other-target".into();
        let (raw, signature) = signed(&signing, &manifest);
        assert!(updater.verify_manifest_owned(&raw, &signature).is_err());

        manifest.targets[0].target = current_target();
        manifest.targets[0].archive_format = "tar".into();
        manifest.targets[0].executable_path = "../../sift-server".into();
        let (raw, signature) = signed(&signing, &manifest);
        assert!(updater.verify_manifest_owned(&raw, &signature).is_err());
    }

    #[test]
    fn immutable_install_pointer_commit_and_rollback() {
        let directory = tempfile::tempdir().unwrap();
        let (updater, _, manifest, artifact_bytes) = fixture(directory.path());
        ensure_private_dir(&updater.state_dir).unwrap();
        let staging_dir = updater.state_dir.join("staging");
        ensure_private_dir(&staging_dir).unwrap();
        let staging = staging_dir.join("fixture.part");
        std::fs::write(&staging, &artifact_bytes).unwrap();
        let installed = updater
            .install_staging(&manifest, &manifest.targets[0], &staging)
            .unwrap();
        assert!(installed.executable.is_file());

        let mut pointers = VersionPointers {
            pending: Some(installed.clone()),
            ..VersionPointers::default()
        };
        updater.write_pointers(&pointers).unwrap();
        assert_eq!(updater.commit_healthy_candidate().unwrap(), installed);
        pointers = updater.read_pointers().unwrap();
        assert_eq!(pointers.current, Some(installed.clone()));
        pointers.pending = Some(installed.clone());
        updater.write_pointers(&pointers).unwrap();
        assert_eq!(updater.rollback_candidate().unwrap(), Some(installed));
        assert!(updater.read_pointers().unwrap().pending.is_none());
    }

    #[test]
    fn container_mode_refuses_self_update() {
        let config = Config {
            mode: RuntimeMode::Container,
            updater: UpdaterConfig {
                enabled: true,
                ..UpdaterConfig::default()
            },
            ..Config::default()
        };
        assert!(config.validate().is_err());
        assert!(Updater::from_config(&config).is_err());
    }
}
