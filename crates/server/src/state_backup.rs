//! Offline, encrypted backup and restore for state owned by Sift (ADR-039).

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sift_metadata::{
    FileSecretStore, MemorySecretStore, MetadataStore, MigrationStatus, NewOperationAudit,
    SecretStore,
};
use uuid::Uuid;
use zip::write::SimpleFileOptions;
use zip::{AesMode, CompressionMethod, ZipArchive, ZipWriter};

use crate::config::Config;
mod ai_content;
pub mod policy;
pub mod tenant;

const FORMAT_VERSION: u32 = 2;
const MANIFEST_ENTRY: &str = "manifest.json";
const METADATA_ENTRY: &str = "metadata.sqlite";
const SECRETS_ENTRY: &str = "secrets.enc";
const SOURCE_SECRET_KEY_ENTRY: &str = "source-secret.key";
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_PAYLOAD_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const RESTORE_JOURNAL: &str = ".sift-restore.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupPayload {
    pub name: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SecretDisposition {
    File { portable: bool },
    Memory { durable: bool },
    Keychain { external_secrets_required: bool },
}

impl SecretDisposition {
    fn backend_name(&self) -> &'static str {
        match self {
            Self::File { .. } => "file",
            Self::Memory { .. } => "memory",
            Self::Keychain { .. } => "keychain",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupManifest {
    pub format_version: u32,
    pub created_at: DateTime<Utc>,
    pub sift_version: String,
    pub source_instance_id: Option<String>,
    pub metadata: MigrationStatus,
    pub secrets: SecretDisposition,
    pub payloads: Vec<BackupPayload>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RestoreReport {
    pub archive: PathBuf,
    pub apply: bool,
    pub source_instance_id: Option<String>,
    pub destination_instance_id: Option<String>,
    pub metadata_version: u32,
    pub sessions_revoked: bool,
    pub external_secrets_required: bool,
    pub rescue_archive: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RestoreJournal {
    schema_version: u32,
    phase: RestorePhase,
    metadata_path: PathBuf,
    secrets_path: Option<PathBuf>,
    old_metadata_path: PathBuf,
    old_secrets_path: Option<PathBuf>,
    staging_dir: PathBuf,
    had_metadata: bool,
    had_secrets: bool,
    #[serde(default)]
    content_path: Option<PathBuf>,
    #[serde(default)]
    old_content_path: Option<PathBuf>,
    #[serde(default)]
    had_content: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RestorePhase {
    Prepared,
    SecretsInstalled,
    ContentInstalled,
    MetadataInstalled,
    Committed,
}

pub fn create(config: &Config, output: &Path, key_file: &Path) -> anyhow::Result<BackupManifest> {
    let _maintenance = crate::runtime::acquire_maintenance_exclusive(config)
        .context("acquiring offline maintenance lock")?;
    recover_interrupted_restore(config)?;
    create_locked(config, output, key_file, true)
}

pub fn inspect(archive: &Path, key_file: &Path) -> anyhow::Result<BackupManifest> {
    let password = read_archive_password(key_file)?;
    let directory = tempfile::Builder::new().prefix("sift-inspect-").tempdir()?;
    let manifest = extract_archive(archive, &password, directory.path())?;
    validate_staged_metadata(&directory.path().join(METADATA_ENTRY))?;
    Ok(manifest)
}

pub async fn restore(
    config: &Config,
    archive: &Path,
    key_file: &Path,
    apply: bool,
    allow_external_secrets: bool,
) -> anyhow::Result<RestoreReport> {
    let password = read_archive_password(key_file)?;
    if !apply {
        let directory = tempfile::Builder::new()
            .prefix("sift-restore-check-")
            .tempdir()?;
        let manifest = extract_archive(archive, &password, directory.path())?;
        validate_restore_compatibility(config, &manifest, allow_external_secrets)?;
        validate_staged_metadata(&directory.path().join(METADATA_ENTRY))?;
        return restore_report(config, archive, &manifest, false, None);
    }

    let _maintenance = crate::runtime::acquire_maintenance_exclusive(config)
        .context("acquiring offline maintenance lock")?;
    recover_interrupted_restore(config)?;
    let metadata_path = configured_metadata_path(config)?;
    let parent = metadata_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    std::fs::create_dir_all(&parent)?;
    make_private_dir(&parent)?;
    let staging_dir = parent.join(format!(".sift-restore-{}", Uuid::new_v4()));
    std::fs::create_dir(&staging_dir)?;
    make_private_dir(&staging_dir)?;

    let result = async {
        let manifest = extract_archive(archive, &password, &staging_dir)?;
        validate_restore_compatibility(config, &manifest, allow_external_secrets)?;
        let staged_metadata = staging_dir.join(METADATA_ENTRY);
        validate_staged_metadata(&staged_metadata)?;
        let staged_secrets = prepare_staged_secrets(config, &manifest, &staging_dir)?;
        let staged_secret_store = staged_secret_store(config, staged_secrets.as_deref())?;

        // MetadataStore uses WAL mode. Installing only the main database while
        // a sanitized store is still open can strand the revocation writes in
        // the staging WAL. Materialize a fresh SQLite backup after sanitation
        // so the file we rename is a self-contained recovery point.
        let sanitized_metadata = staging_dir.join("sanitized-metadata.sqlite");
        {
            let staged_store = MetadataStore::open(&staged_metadata, staged_secret_store)?;
            staged_store.sanitize_workspace_backup_snapshot()?;
            staged_store.sanitize_after_restore().await?;
            staged_store.integrity_check()?;
            staged_store.backup_database_to(&sanitized_metadata)?;
        }
        remove_if_exists(&staged_metadata)?;
        remove_if_exists(&staged_metadata.with_extension("sqlite-wal"))?;
        remove_if_exists(&staged_metadata.with_extension("sqlite-shm"))?;
        std::fs::rename(&sanitized_metadata, &staged_metadata)?;

        let rescue_archive = if metadata_path.exists() {
            let backup_dir = parent.join("backups");
            std::fs::create_dir_all(&backup_dir)?;
            make_private_dir(&backup_dir)?;
            let rescue = backup_dir.join(format!(
                "pre-restore-{}-{}.sift-backup",
                Utc::now().format("%Y%m%dT%H%M%S%.3fZ"),
                Uuid::new_v4().simple()
            ));
            create_locked(config, &rescue, key_file, false)?;
            Some(rescue)
        } else {
            None
        };

        install_staged_state(config, &staging_dir, staged_secrets.as_deref())?;
        let destination = crate::metadata_runtime::open_metadata_store(config)?
            .context("restored metadata is disabled")?;
        destination.ensure_schema_current()?;
        commit_restore_journal(config)?;

        Ok::<_, anyhow::Error>((manifest, rescue_archive))
    }
    .await;

    match result {
        Ok((manifest, rescue_archive)) => {
            finalize_restore_journal(config)?;
            Ok(restore_report(
                config,
                archive,
                &manifest,
                true,
                rescue_archive,
            )?)
        }
        Err(error) => {
            let rollback = recover_interrupted_restore(config);
            if let Err(rollback_error) = rollback {
                return Err(error.context(format!(
                    "restore failed and automatic rollback also failed: {rollback_error:#}"
                )));
            }
            // Failures before the replacement journal is written (for
            // example, a bad payload or incompatible schema) are not covered
            // by journal recovery, so remove their private staging directory
            // explicitly.
            if staging_dir.exists() {
                if let Err(cleanup_error) = std::fs::remove_dir_all(&staging_dir) {
                    return Err(error.context(format!(
                        "restore failed and staging cleanup also failed: {cleanup_error}"
                    )));
                }
            }
            Err(error)
        }
    }
}

fn create_locked(
    config: &Config,
    output: &Path,
    key_file: &Path,
    audit: bool,
) -> anyhow::Result<BackupManifest> {
    if output.exists() {
        bail!("backup output already exists: {}", output.display());
    }
    let password = read_archive_password(key_file)?;
    let metadata_path = configured_metadata_path(config)?;
    if !metadata_path.is_file() {
        bail!(
            "metadata database does not exist: {}",
            metadata_path.display()
        );
    }
    let store = MetadataStore::open(&metadata_path, Arc::new(MemorySecretStore::new()))?;
    store.ensure_schema_current()?;
    store.integrity_check()?;

    let directory = tempfile::Builder::new().prefix("sift-backup-").tempdir()?;
    let raw_snapshot = directory.path().join("raw-metadata.sqlite");
    let snapshot = directory.path().join(METADATA_ENTRY);
    store.backup_database_to(&raw_snapshot)?;
    {
        let snapshot_store =
            MetadataStore::open(&raw_snapshot, Arc::new(MemorySecretStore::new()))?;
        snapshot_store.sanitize_workspace_backup_snapshot()?;
        snapshot_store.integrity_check()?;
        snapshot_store.backup_database_to(&snapshot)?;
    }
    let status = store.migration_status()?;
    let mut payload_paths = BTreeMap::new();
    payload_paths.insert(METADATA_ENTRY.to_string(), snapshot);
    let secrets = collect_secret_payloads(config, directory.path(), &mut payload_paths)?;
    {
        let snapshot_store = MetadataStore::open(
            payload_paths.get(METADATA_ENTRY).unwrap(),
            Arc::new(MemorySecretStore::new()),
        )?;
        let handles = snapshot_store.ai_content_handles_offline(None)?;
        if !handles.is_empty() {
            anyhow::ensure!(
                matches!(secrets, SecretDisposition::File { portable: true }),
                "AI backup requires portable file-secret content keys"
            );
            let keys = FileSecretStore::open(
                payload_paths
                    .get(SECRETS_ENTRY)
                    .context("AI backup has no portable secret store")?,
                payload_paths
                    .get(SOURCE_SECRET_KEY_ENTRY)
                    .context("AI backup has no portable secret key")?,
            )?;
            let bundle = directory.path().join(ai_content::ENTRY);
            ai_content::collect(
                &snapshot_store,
                &metadata_path.with_extension("ai-content"),
                &keys,
                &bundle,
            )?;
            payload_paths.insert(ai_content::ENTRY.into(), bundle);
        }
    }
    let payloads = payload_paths
        .iter()
        .map(|(name, path)| payload_descriptor(name, path))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let manifest = BackupManifest {
        format_version: FORMAT_VERSION,
        created_at: Utc::now(),
        sift_version: crate::VERSION.to_string(),
        source_instance_id: crate::runtime::existing_instance_id(config)?,
        metadata: status,
        secrets,
        payloads,
    };
    write_archive(output, &password, &manifest, &payload_paths)?;

    if audit {
        let live_store = crate::metadata_runtime::open_metadata_store(config)?
            .context("metadata backup requires metadata.enabled=true")?;
        live_store.record_operation_audit(NewOperationAudit {
            actor_principal_id: None,
            action: "backup.create".to_string(),
            target: "instance_state".to_string(),
            target_id: None,
            status: "succeeded".to_string(),
            result_code: None,
            row_count: None,
            error_message: None,
            correlation_id: None,
        })?;
    }
    Ok(manifest)
}

fn collect_secret_payloads(
    config: &Config,
    workspace: &Path,
    payloads: &mut BTreeMap<String, PathBuf>,
) -> anyhow::Result<SecretDisposition> {
    match config.metadata.secret_backend.as_str() {
        "file" => {
            let metadata_path = configured_metadata_path(config)?;
            let secrets_path = secret_file_path(&metadata_path);
            let key_path = configured_secret_key_path(config)?;
            FileSecretStore::open(&secrets_path, &key_path)
                .context("validating file secret store before backup")?;
            let portable_secrets = if secrets_path.exists() {
                let filtered = workspace.join(SECRETS_ENTRY);
                FileSecretStore::copy_excluding_namespaces(
                    &secrets_path,
                    &key_path,
                    &filtered,
                    &["vcs-credential"],
                )?;
                filtered
            } else {
                let empty = workspace.join(SECRETS_ENTRY);
                FileSecretStore::initialize_empty(&empty, &key_path)?;
                empty
            };
            payloads.insert(SECRETS_ENTRY.to_string(), portable_secrets);
            payloads.insert(SOURCE_SECRET_KEY_ENTRY.to_string(), key_path);
            Ok(SecretDisposition::File { portable: true })
        }
        "memory" => Ok(SecretDisposition::Memory { durable: false }),
        "keychain" => Ok(SecretDisposition::Keychain {
            external_secrets_required: true,
        }),
        other => bail!("unsupported metadata secret backend `{other}`"),
    }
}

fn write_archive(
    output: &Path,
    password: &str,
    manifest: &BackupManifest,
    payloads: &BTreeMap<String, PathBuf>,
) -> anyhow::Result<()> {
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let file_name = output
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("backup");
    let partial = parent.join(format!(".{file_name}.{}.partial", Uuid::new_v4()));
    let file = private_create_new(&partial)?;
    let result = (|| -> anyhow::Result<()> {
        let mut writer = ZipWriter::new(file);
        for (name, path) in payloads {
            start_encrypted_entry(&mut writer, name, password)?;
            let mut source = File::open(path)?;
            std::io::copy(&mut source, &mut writer)?;
        }
        start_encrypted_entry(&mut writer, MANIFEST_ENTRY, password)?;
        serde_json::to_writer_pretty(&mut writer, manifest)?;
        writer.write_all(b"\n")?;
        let file = writer.finish()?;
        file.sync_all()?;
        std::fs::rename(&partial, output)?;
        sync_dir(parent)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    result
}

fn start_encrypted_entry<W: Write + Seek>(
    writer: &mut ZipWriter<W>,
    name: &str,
    password: &str,
) -> anyhow::Result<()> {
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o600)
        .with_aes_encryption(AesMode::Aes256, password);
    writer.start_file(name, options)?;
    Ok(())
}

fn extract_archive(
    archive_path: &Path,
    password: &str,
    destination: &Path,
) -> anyhow::Result<BackupManifest> {
    let mut file = File::open(archive_path)
        .with_context(|| format!("opening backup archive: {}", archive_path.display()))?;
    let declared_entries = declared_zip_entry_count(&mut file)?;
    file.seek(SeekFrom::Start(0))?;
    let mut archive = ZipArchive::new(file).context("decoding backup archive")?;
    if declared_entries > 5 || archive.len() > 5 {
        bail!("backup archive contains too many entries");
    }
    // `zip` indexes entries by name and intentionally collapses duplicates.
    // Compare its unique view with the central-directory count so ambiguous
    // archives cannot select one payload for validation and another for read.
    if declared_entries != archive.len() {
        bail!("backup archive contains duplicate entries");
    }
    let mut archive_names = BTreeSet::new();
    for index in 0..archive.len() {
        // `by_index` attempts to construct a plaintext reader and therefore
        // rejects encrypted entries before we have supplied the archive key.
        // The raw view is sufficient for validating central-directory
        // metadata and leaves decryption to the named reads below.
        let entry = archive.by_index_raw(index)?;
        let name = entry.name().to_string();
        if !matches!(
            name.as_str(),
            MANIFEST_ENTRY
                | METADATA_ENTRY
                | SECRETS_ENTRY
                | SOURCE_SECRET_KEY_ENTRY
                | ai_content::ENTRY
        ) {
            bail!("backup archive contains unknown entry `{name}`");
        }
        if !entry.encrypted() {
            bail!("backup archive entry `{name}` is not encrypted");
        }
        if !archive_names.insert(name.clone()) {
            bail!("backup archive contains duplicate entry `{name}`");
        }
    }
    if !archive_names.contains(MANIFEST_ENTRY) || !archive_names.contains(METADATA_ENTRY) {
        bail!("backup archive is missing its manifest or metadata payload");
    }

    let manifest: BackupManifest = {
        let mut entry = archive
            .by_name_decrypt(MANIFEST_ENTRY, password.as_bytes())
            .context("decrypting backup manifest")?;
        if entry.size() > MAX_MANIFEST_BYTES {
            bail!("backup manifest exceeds 64 KiB");
        }
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut bytes)?;
        serde_json::from_slice(&bytes).context("decoding backup manifest")?
    };
    validate_manifest(&manifest, &archive_names)?;
    std::fs::create_dir_all(destination)?;
    make_private_dir(destination)?;
    for payload in &manifest.payloads {
        extract_payload(&mut archive, password, destination, payload)?;
    }
    ai_content::extract_and_validate(destination, &manifest)?;
    Ok(manifest)
}

fn declared_zip_entry_count(file: &mut File) -> anyhow::Result<usize> {
    const EOCD_SIGNATURE: &[u8; 4] = b"PK\x05\x06";
    const EOCD_FIXED_BYTES: u64 = 22;
    const MAX_COMMENT_BYTES: u64 = u16::MAX as u64;

    let file_len = file.seek(SeekFrom::End(0))?;
    let tail_len = file_len.min(EOCD_FIXED_BYTES + MAX_COMMENT_BYTES);
    file.seek(SeekFrom::End(-(tail_len as i64)))?;
    let mut tail = vec![0_u8; tail_len as usize];
    file.read_exact(&mut tail)?;
    let offset = tail
        .windows(EOCD_SIGNATURE.len())
        .rposition(|window| window == EOCD_SIGNATURE)
        .context("backup archive is missing its end-of-central-directory record")?;
    if offset + EOCD_FIXED_BYTES as usize > tail.len() {
        bail!("backup archive has a truncated end-of-central-directory record");
    }
    let comment_len = u16::from_le_bytes([tail[offset + 20], tail[offset + 21]]) as usize;
    if offset + EOCD_FIXED_BYTES as usize + comment_len != tail.len() {
        bail!("backup archive has an invalid end-of-central-directory record");
    }
    Ok(u16::from_le_bytes([tail[offset + 10], tail[offset + 11]]) as usize)
}

fn validate_manifest(
    manifest: &BackupManifest,
    archive_names: &BTreeSet<String>,
) -> anyhow::Result<()> {
    if !matches!(manifest.format_version, 1 | FORMAT_VERSION) {
        bail!(
            "unsupported backup format version {}; expected {}",
            manifest.format_version,
            FORMAT_VERSION
        );
    }
    let mut expected = BTreeSet::from([MANIFEST_ENTRY.to_string()]);
    for payload in &manifest.payloads {
        if !matches!(
            payload.name.as_str(),
            METADATA_ENTRY | SECRETS_ENTRY | SOURCE_SECRET_KEY_ENTRY | ai_content::ENTRY
        ) {
            bail!("manifest contains unknown payload `{}`", payload.name);
        }
        if payload.size > MAX_PAYLOAD_BYTES {
            bail!("payload `{}` exceeds the 16 GiB limit", payload.name);
        }
        if payload.sha256.len() != 64
            || !payload
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            bail!("payload `{}` has an invalid SHA-256 digest", payload.name);
        }
        if !expected.insert(payload.name.clone()) {
            bail!("manifest contains duplicate payload `{}`", payload.name);
        }
    }
    if !expected.contains(METADATA_ENTRY) {
        bail!("manifest does not describe metadata.sqlite");
    }
    if &expected != archive_names {
        bail!("archive entries do not exactly match the manifest");
    }
    let has_secret_store = expected.contains(SECRETS_ENTRY);
    let has_secret_key = expected.contains(SOURCE_SECRET_KEY_ENTRY);
    match &manifest.secrets {
        SecretDisposition::File { portable } => {
            if has_secret_store != has_secret_key || (*portable && !has_secret_store) {
                bail!("file-secret manifest and payloads disagree");
            }
        }
        SecretDisposition::Memory { .. } | SecretDisposition::Keychain { .. } => {
            if has_secret_store || has_secret_key {
                bail!("non-file secret manifest contains file-secret payloads");
            }
        }
    }
    Ok(())
}

fn extract_payload(
    archive: &mut ZipArchive<File>,
    password: &str,
    destination: &Path,
    payload: &BackupPayload,
) -> anyhow::Result<()> {
    let mut entry = archive
        .by_name_decrypt(&payload.name, password.as_bytes())
        .with_context(|| format!("decrypting backup payload `{}`", payload.name))?;
    if entry.size() != payload.size {
        bail!("payload `{}` size differs from its manifest", payload.name);
    }
    let path = destination.join(&payload.name);
    let mut output = private_create_new(&path)?;
    let mut digest = Sha256::new();
    let mut written = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = entry.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        written = written.saturating_add(read as u64);
        if written > payload.size || written > MAX_PAYLOAD_BYTES {
            bail!("payload `{}` exceeds its declared size", payload.name);
        }
        digest.update(&buffer[..read]);
        output.write_all(&buffer[..read])?;
    }
    output.sync_all()?;
    if written != payload.size || hex_digest(digest.finalize().as_slice()) != payload.sha256 {
        bail!("payload `{}` failed integrity verification", payload.name);
    }
    Ok(())
}

fn validate_restore_compatibility(
    config: &Config,
    manifest: &BackupManifest,
    allow_external_secrets: bool,
) -> anyhow::Result<()> {
    if config.metadata.secret_backend != manifest.secrets.backend_name() {
        bail!(
            "backup secret backend `{}` does not match destination `{}`",
            manifest.secrets.backend_name(),
            config.metadata.secret_backend
        );
    }
    if matches!(manifest.secrets, SecretDisposition::Keychain { .. }) && !allow_external_secrets {
        bail!(
            "keychain backup depends on destination keychain entries; inspect them and rerun with --allow-external-secrets"
        );
    }
    Ok(())
}

fn validate_staged_metadata(path: &Path) -> anyhow::Result<MigrationStatus> {
    let store = MetadataStore::open(path, Arc::new(MemorySecretStore::new()))?;
    store.integrity_check()?;
    store.ensure_schema_current()?;
    Ok(store.migration_status()?)
}

fn prepare_staged_secrets(
    config: &Config,
    manifest: &BackupManifest,
    staging_dir: &Path,
) -> anyhow::Result<Option<PathBuf>> {
    match &manifest.secrets {
        SecretDisposition::File { portable: true } => {
            let destination_key = ensure_destination_secret_key(config)?;
            let destination = staging_dir.join("restored-secrets.enc");
            FileSecretStore::reencrypt(
                &staging_dir.join(SECRETS_ENTRY),
                &staging_dir.join(SOURCE_SECRET_KEY_ENTRY),
                &destination,
                &destination_key,
            )?;
            Ok(Some(destination))
        }
        SecretDisposition::File { portable: false } => {
            bail!("non-portable file-secret archives are unsupported")
        }
        SecretDisposition::Memory { .. } | SecretDisposition::Keychain { .. } => Ok(None),
    }
}

fn staged_secret_store(
    config: &Config,
    staged_secrets: Option<&Path>,
) -> anyhow::Result<Arc<dyn SecretStore>> {
    match config.metadata.secret_backend.as_str() {
        "file" => {
            let path = staged_secrets.context("file-secret restore has no staged secret store")?;
            let key = configured_secret_key_path(config)?;
            Ok(Arc::new(FileSecretStore::open(path, &key)?))
        }
        // Memory secrets are non-durable. Keychain secrets are explicitly
        // destination-owned external dependencies, so sanitize imported DB
        // references without mutating the destination keychain pre-install.
        "memory" | "keychain" => Ok(Arc::new(MemorySecretStore::new())),
        other => bail!("unsupported metadata secret backend `{other}`"),
    }
}

fn install_staged_state(
    config: &Config,
    staging_dir: &Path,
    staged_secrets: Option<&Path>,
) -> anyhow::Result<()> {
    let metadata_path = configured_metadata_path(config)?;
    let parent = metadata_path.parent().unwrap_or_else(|| Path::new("."));
    let secrets_path = staged_secrets.map(|_| secret_file_path(&metadata_path));
    let suffix = Uuid::new_v4();
    let old_metadata_path = parent.join(format!(".metadata.restore-old-{suffix}"));
    let old_secrets_path = secrets_path
        .as_ref()
        .map(|_| parent.join(format!(".secrets.restore-old-{suffix}")));
    let content_path = metadata_path.with_extension("ai-content");
    let old_content_path = parent.join(format!(".ai-content.restore-old-{suffix}"));
    let staged_content = staging_dir
        .join(METADATA_ENTRY)
        .with_extension("ai-content");
    anyhow::ensure!(
        std::fs::symlink_metadata(&staged_content)?.is_dir(),
        "restore has no staged AI content directory"
    );
    let mut journal = RestoreJournal {
        schema_version: 2,
        phase: RestorePhase::Prepared,
        metadata_path: metadata_path.clone(),
        secrets_path: secrets_path.clone(),
        old_metadata_path,
        old_secrets_path,
        staging_dir: staging_dir.to_path_buf(),
        had_metadata: metadata_path.exists(),
        had_secrets: secrets_path.as_ref().is_some_and(|path| path.exists()),
        had_content: content_path.exists(),
        content_path: Some(content_path.clone()),
        old_content_path: Some(old_content_path.clone()),
    };
    write_restore_journal(config, &journal)?;

    if let (Some(destination), Some(staged), Some(old)) = (
        secrets_path.as_ref(),
        staged_secrets,
        journal.old_secrets_path.as_ref(),
    ) {
        if destination.exists() {
            std::fs::rename(destination, old)?;
        }
        std::fs::rename(staged, destination)?;
    }
    journal.phase = RestorePhase::SecretsInstalled;
    write_restore_journal(config, &journal)?;

    if content_path.exists() {
        std::fs::rename(&content_path, &old_content_path)?;
    }
    std::fs::rename(staged_content, &content_path)?;
    journal.phase = RestorePhase::ContentInstalled;
    write_restore_journal(config, &journal)?;

    if metadata_path.exists() {
        std::fs::rename(&metadata_path, &journal.old_metadata_path)?;
    }
    std::fs::rename(staging_dir.join(METADATA_ENTRY), &metadata_path)?;
    journal.phase = RestorePhase::MetadataInstalled;
    write_restore_journal(config, &journal)?;
    sync_dir(parent)?;
    Ok(())
}

fn commit_restore_journal(config: &Config) -> anyhow::Result<()> {
    let mut journal = read_restore_journal(config)?.context("restore journal disappeared")?;
    journal.phase = RestorePhase::Committed;
    write_restore_journal(config, &journal)
}

fn finalize_restore_journal(config: &Config) -> anyhow::Result<()> {
    let Some(journal) = read_restore_journal(config)? else {
        return Ok(());
    };
    if journal.phase != RestorePhase::Committed {
        bail!("cannot finalize an uncommitted restore journal");
    }
    remove_if_exists(&journal.old_metadata_path)?;
    if let Some(path) = &journal.old_secrets_path {
        remove_if_exists(path)?;
    }
    if let Some(path) = &journal.old_content_path {
        remove_dir_if_exists(path)?;
    }
    if journal.staging_dir.exists() {
        std::fs::remove_dir_all(&journal.staging_dir)?;
    }
    remove_if_exists(&restore_journal_path(config))?;
    sync_dir(
        journal
            .metadata_path
            .parent()
            .unwrap_or_else(|| Path::new(".")),
    )?;
    Ok(())
}

fn recover_interrupted_restore(config: &Config) -> anyhow::Result<()> {
    let Some(journal) = read_restore_journal(config)? else {
        return Ok(());
    };
    if journal.phase == RestorePhase::Committed {
        return finalize_restore_journal(config);
    }

    if journal.old_metadata_path.exists() {
        remove_if_exists(&journal.metadata_path)?;
        std::fs::rename(&journal.old_metadata_path, &journal.metadata_path)?;
    } else if !journal.had_metadata {
        remove_if_exists(&journal.metadata_path)?;
    }
    if let (Some(destination), Some(old)) = (
        journal.secrets_path.as_ref(),
        journal.old_secrets_path.as_ref(),
    ) {
        if old.exists() {
            remove_if_exists(destination)?;
            std::fs::rename(old, destination)?;
        } else if !journal.had_secrets {
            remove_if_exists(destination)?;
        }
    }
    if let (Some(destination), Some(old)) = (&journal.content_path, &journal.old_content_path) {
        if old.exists() {
            remove_dir_if_exists(destination)?;
            std::fs::rename(old, destination)?;
        } else if !journal.had_content {
            remove_dir_if_exists(destination)?;
        }
    }
    if journal.staging_dir.exists() {
        std::fs::remove_dir_all(&journal.staging_dir)?;
    }
    remove_if_exists(&restore_journal_path(config))?;
    sync_dir(
        journal
            .metadata_path
            .parent()
            .unwrap_or_else(|| Path::new(".")),
    )?;
    Ok(())
}

fn write_restore_journal(config: &Config, journal: &RestoreJournal) -> anyhow::Result<()> {
    let path = restore_journal_path(config);
    let parent = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let partial = parent.join(format!(".restore-journal-{}.partial", Uuid::new_v4()));
    let mut file = private_create_new(&partial)?;
    serde_json::to_writer_pretty(&mut file, journal)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    std::fs::rename(partial, &path)?;
    sync_dir(&parent)?;
    Ok(())
}

fn read_restore_journal(config: &Config) -> anyhow::Result<Option<RestoreJournal>> {
    let path = restore_journal_path(config);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if bytes.len() > 64 * 1024 {
        bail!("restore journal exceeds 64 KiB");
    }
    let journal: RestoreJournal = serde_json::from_slice(&bytes)?;
    if !matches!(journal.schema_version, 1 | 2) {
        bail!("unsupported restore journal version");
    }
    Ok(Some(journal))
}

fn restore_report(
    config: &Config,
    archive: &Path,
    manifest: &BackupManifest,
    apply: bool,
    rescue_archive: Option<PathBuf>,
) -> anyhow::Result<RestoreReport> {
    Ok(RestoreReport {
        archive: archive.to_path_buf(),
        apply,
        source_instance_id: manifest.source_instance_id.clone(),
        destination_instance_id: crate::runtime::existing_instance_id(config)?,
        metadata_version: manifest.metadata.current_version,
        sessions_revoked: apply,
        external_secrets_required: matches!(manifest.secrets, SecretDisposition::Keychain { .. }),
        rescue_archive,
    })
}

fn configured_metadata_path(config: &Config) -> anyhow::Result<PathBuf> {
    if !config.metadata.enabled {
        bail!("state backup requires metadata.enabled=true");
    }
    Ok(config
        .metadata
        .path
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(MetadataStore::default_local_path))
}

fn secret_file_path(metadata_path: &Path) -> PathBuf {
    metadata_path
        .parent()
        .map(|parent| parent.join("secrets.enc"))
        .unwrap_or_else(|| PathBuf::from("secrets.enc"))
}

fn configured_secret_key_path(config: &Config) -> anyhow::Result<PathBuf> {
    config
        .metadata
        .secret_key_file
        .as_deref()
        .map(PathBuf::from)
        .context("file secret backend requires metadata.secret_key_file")
}

fn ensure_destination_secret_key(config: &Config) -> anyhow::Result<PathBuf> {
    let path = configured_secret_key_path(config)?;
    if path.exists() {
        return Ok(path);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        make_private_dir(parent)?;
    }
    let mut bytes = [0_u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|error| anyhow::anyhow!("rng failure: {error}"))?;
    let mut file = private_create_new(&path)?;
    file.write_all(hex_digest(&bytes).as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    if let Some(parent) = path.parent() {
        sync_dir(parent)?;
    }
    Ok(path)
}

fn restore_journal_path(config: &Config) -> PathBuf {
    let metadata = config
        .metadata
        .path
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(MetadataStore::default_local_path);
    metadata
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(RESTORE_JOURNAL)
}

fn read_archive_password(path: &Path) -> anyhow::Result<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let mode = std::fs::metadata(path)?.mode();
        if mode & 0o077 != 0 {
            bail!("backup key file must not be accessible by group or others");
        }
    }
    let value = std::fs::read_to_string(path)
        .with_context(|| format!("reading backup key file: {}", path.display()))?;
    let value = value.trim();
    if value.len() < 32 || value.len() > 4096 {
        bail!("backup key file must contain between 32 and 4096 UTF-8 bytes");
    }
    Ok(value.to_string())
}

fn payload_descriptor(name: &str, path: &Path) -> anyhow::Result<BackupPayload> {
    let metadata = std::fs::metadata(path)?;
    if metadata.len() > MAX_PAYLOAD_BYTES {
        bail!("payload `{name}` exceeds the 16 GiB limit");
    }
    Ok(BackupPayload {
        name: name.to_string(),
        size: metadata.len(),
        sha256: sha256_file(path)?,
    })
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex_digest(digest.finalize().as_slice()))
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn private_create_new(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn make_private_dir(path: &Path) -> anyhow::Result<()> {
    #[cfg(not(unix))]
    let _ = path;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn remove_dir_if_exists(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn remove_if_exists(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn sync_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(not(unix))]
    let _ = path;
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_metadata::{
        MetadataError, NewProjectionBinding, NewRepositoryBinding, NewRoom, NewTransferRecipe,
        PrincipalId, RoomKind, TenantId,
    };
    use sift_protocol::{
        ProjectionHealth, ProjectionMode, TransferDirection, TransferEndpoint, WorkspaceArtifactId,
        WorkspaceId,
    };

    pub(super) fn write_private_key(path: &Path, byte: &str) {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path).unwrap();
        writeln!(file, "{}", byte.repeat(32)).unwrap();
        file.sync_all().unwrap();
    }

    fn file_config(root: &Path, key_byte: &str) -> Config {
        std::fs::create_dir_all(root).unwrap();
        let secret_key = root.join("metadata.key");
        write_private_key(&secret_key, key_byte);
        let mut config = Config::default();
        config.runtime.state_dir = Some(root.join("runtime").display().to_string());
        config.metadata.path = Some(root.join("metadata.sqlite").display().to_string());
        config.metadata.secret_backend = "file".to_string();
        config.metadata.secret_key_file = Some(secret_key.display().to_string());
        config
    }

    pub(super) fn memory_config(root: &Path) -> Config {
        std::fs::create_dir_all(root).unwrap();
        let mut config = Config::default();
        config.runtime.state_dir = Some(root.join("runtime").display().to_string());
        config.metadata.path = Some(root.join("metadata.sqlite").display().to_string());
        config.metadata.secret_backend = "memory".to_string();
        config.metadata.secret_key_file = None;
        config
    }

    fn metadata_path(config: &Config) -> PathBuf {
        PathBuf::from(config.metadata.path.as_deref().unwrap())
    }

    async fn seed_file_state(config: &Config) -> (String, Vec<u8>) {
        let metadata = metadata_path(config);
        let secret_key = PathBuf::from(config.metadata.secret_key_file.as_deref().unwrap());
        let secret_store =
            Arc::new(FileSecretStore::open(secret_file_path(&metadata), &secret_key).unwrap());
        let store = MetadataStore::open(&metadata, secret_store.clone()).unwrap();
        store.apply_migrations(false).unwrap();
        store.bootstrap_local("backup tester").unwrap();
        let (_, api_token) = store
            .issue_api_token(PrincipalId(1), None, "restore-revokes", None)
            .unwrap();
        let secret = b"portable-credential".to_vec();
        secret_store
            .put("test", "credential", &secret)
            .await
            .unwrap();
        secret_store
            .put("vcs-credential", "git", b"must-not-back-up")
            .await
            .unwrap();
        let room = store
            .create_room(
                TenantId(1),
                PrincipalId(1),
                NewRoom {
                    name: "backup-workspace-room".into(),
                    kind: RoomKind::Shared,
                },
            )
            .unwrap();
        let workspace = store
            .create_workspace(room.id, PrincipalId(1), "backup-workspace")
            .unwrap();
        let projection = store
            .create_projection_binding(
                workspace.id,
                PrincipalId(1),
                NewProjectionBinding {
                    root_handle: "backup-root".into(),
                    mode: ProjectionMode::ReadWrite,
                    adapter_generation: "filesystem-v1".into(),
                    health: ProjectionHealth::Ready,
                },
            )
            .unwrap();
        let repository = store
            .create_repository_binding(
                workspace.id,
                PrincipalId(1),
                NewRepositoryBinding {
                    projection_id: projection.binding.id,
                    repository_identity: "ab".repeat(32),
                    adapter_generation: "git-v1".into(),
                    executable_version: "git version test".into(),
                    network_enabled: false,
                    branch: Some("main".into()),
                    head: None,
                },
            )
            .unwrap();
        store
            .set_repository_credential(
                repository.binding.id,
                PrincipalId(1),
                repository.binding.revision,
                b"must-not-back-up",
            )
            .await
            .unwrap();
        store
            .create_transfer_recipe(
                workspace.id,
                PrincipalId(1),
                NewTransferRecipe {
                    name: "durable-recipe".into(),
                    direction: TransferDirection::Export,
                    source: TransferEndpoint::Query,
                    sink: TransferEndpoint::Artifact,
                    format_id: "csv".into(),
                    format_version: "1".into(),
                    options: serde_json::json!({}),
                },
            )
            .unwrap();
        store
            .create_workspace_artifact(
                workspace.id,
                PrincipalId(1),
                "text/plain",
                b"ephemeral".to_vec(),
                None,
            )
            .unwrap();
        store.ensure_auth_system_keys().await.unwrap();
        (api_token, secret)
    }

    #[tokio::test]
    async fn tenant_restore_preserves_other_tenant_auth_and_remaps_only_selected_credentials() {
        use sift_metadata::{
            CredentialMode, MembershipRole, NewConnectionProfile, NewDocument, TenantKind,
        };
        use sift_protocol::Engine;
        let root = tempfile::tempdir().unwrap();
        let config = file_config(&root.path().join("instance"), "31");
        let archive_key = root.path().join("archive.key");
        write_private_key(&archive_key, "42");
        seed_file_state(&config).await;
        let db = metadata_path(&config);
        let secret_path = secret_file_path(&db);
        let key = configured_secret_key_path(&config).unwrap();
        let secrets = Arc::new(FileSecretStore::open(&secret_path, &key).unwrap());
        let store = MetadataStore::open(&db, secrets.clone()).unwrap();
        let other = store
            .create_tenant("unrelated", TenantKind::Team)
            .unwrap()
            .id;
        store
            .upsert_tenant_membership(other, PrincipalId(1), MembershipRole::Owner)
            .unwrap();
        let profile = |name: &str, password: &str| NewConnectionProfile {
            name: name.into(),
            provider_id: Engine::Postgres.provider_id(),
            configuration: serde_json::json!({"host":"fixture.invalid","port":5432,"database":"fixture","user":"fixture","ssl_mode":"disable"}),
            semantic_engine: Some(Engine::Postgres),
            credentials: Some(serde_json::json!({"password":password})),
            credential_mode: CredentialMode::Shared,
            tags: vec![],
        };
        let selected = store
            .upsert_connection_profile(
                TenantId(1),
                PrincipalId(1),
                profile("selected", "before-credential"),
            )
            .await
            .unwrap();
        let unrelated = store
            .upsert_connection_profile(other, PrincipalId(1), profile("unrelated", "old-other"))
            .await
            .unwrap();
        let selected_room = store
            .create_room(
                TenantId(1),
                PrincipalId(1),
                NewRoom {
                    name: "selected documents".into(),
                    kind: RoomKind::Shared,
                },
            )
            .unwrap();
        let other_room = store
            .create_room(
                other,
                PrincipalId(1),
                NewRoom {
                    name: "other documents".into(),
                    kind: RoomKind::Shared,
                },
            )
            .unwrap();
        let document = |text: &str| {
            let replica = sift_doc::TextReplica::new(sift_doc::random_peer_id()).unwrap();
            replica.insert(0, text).unwrap();
            NewDocument {
                kind: "sql".into(),
                title: "query".into(),
                crdt_state: replica.export_snapshot().unwrap(),
                snapshot_version: replica.version_vector(),
                position: 0,
                connection_profile_id: None,
            }
        };
        let selected_doc = store
            .create_document(selected_room.id, document("SELECT 1"))
            .unwrap();
        let other_doc = store
            .create_document(other_room.id, document("SELECT 2"))
            .unwrap();
        drop(store);
        drop(secrets);
        let archive = root.path().join("source.sift-backup");
        create(&config, &archive, &archive_key).unwrap();
        let secrets = Arc::new(FileSecretStore::open(&secret_path, &key).unwrap());
        let store = MetadataStore::open(&db, secrets.clone()).unwrap();
        store
            .upsert_connection_profile(
                TenantId(1),
                PrincipalId(1),
                profile("selected", "after-credential"),
            )
            .await
            .unwrap();
        let other_current = store
            .upsert_connection_profile(other, PrincipalId(1), profile("unrelated", "current-other"))
            .await
            .unwrap();
        store
            .update_document_snapshot(selected_doc.id, document("SELECT 99").crdt_state)
            .unwrap();
        let current_other_doc = store
            .update_document_snapshot(other_doc.id, document("SELECT 88").crdt_state)
            .unwrap();
        store.rotate_auth_system_keys().await.unwrap();
        store.ensure_auth_system_keys().await.unwrap();
        let current_auth = secrets
            .get("sift.auth.system", "token-mac-v1")
            .await
            .unwrap();
        let (_, global_token) = store
            .issue_api_token(PrincipalId(1), None, "destination global", None)
            .unwrap();
        let (_, scoped_token) = store
            .issue_api_token(PrincipalId(1), Some(TenantId(1)), "selected scoped", None)
            .unwrap();
        drop(store);
        drop(secrets);
        let encrypted_before = std::fs::read(&secret_path).unwrap();
        let preview = tenant::restore_tenant(&config, &archive, &archive_key, 1, false)
            .await
            .unwrap();
        assert!(!preview.applied);
        assert_eq!(preview.merge.copied_secrets, 1);
        assert!(std::fs::read(&secret_path).unwrap() == encrypted_before);
        {
            let store = MetadataStore::open(&db, Arc::new(MemorySecretStore::new())).unwrap();
            assert!(
                store.get_document(selected_doc.id).unwrap().crdt_state != selected_doc.crdt_state
            );
            assert!(store.verify_api_token(&global_token).unwrap().is_some());
            assert!(store.verify_api_token(&scoped_token).unwrap().is_some());
        }
        let applied = tenant::restore_tenant(&config, &archive, &archive_key, 1, true)
            .await
            .unwrap();
        assert!(applied.applied);
        assert!(applied.rescue_archive.as_ref().unwrap().is_file());
        assert!(!serde_json::to_string(&applied)
            .unwrap()
            .contains("before-credential"));
        assert!(read_restore_journal(&config).unwrap().is_none());
        let secrets = Arc::new(FileSecretStore::open(&secret_path, &key).unwrap());
        let store = MetadataStore::open(&db, secrets.clone()).unwrap();
        let recovered = store
            .get_connection_profile(TenantId(1), selected.id)
            .unwrap();
        assert!(recovered.shared_secret_handle != selected.shared_secret_handle);
        let value = secrets
            .get(
                "sift.local",
                recovered.shared_secret_handle.as_deref().unwrap(),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(
            serde_json::from_slice::<serde_json::Value>(&value).unwrap()["password"]
                == "before-credential"
        );
        assert!(
            store
                .get_connection_profile(other, unrelated.id)
                .unwrap()
                .shared_secret_handle
                == other_current.shared_secret_handle
        );
        let other_value = secrets
            .get(
                "sift.local",
                other_current.shared_secret_handle.as_deref().unwrap(),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(
            serde_json::from_slice::<serde_json::Value>(&other_value).unwrap()["password"]
                == "current-other"
        );
        assert!(
            secrets
                .get("sift.auth.system", "token-mac-v1")
                .await
                .unwrap()
                == current_auth
        );
        assert!(store.get_document(selected_doc.id).unwrap().crdt_state == selected_doc.crdt_state);
        assert!(
            store.get_document(other_doc.id).unwrap().crdt_state == current_other_doc.crdt_state
        );
        assert!(store.verify_api_token(&global_token).unwrap().is_some());
        assert!(store.verify_api_token(&scoped_token).unwrap().is_none());
        assert_eq!(
            store
                .projection_binding_for_workspace(WorkspaceId(1), PrincipalId(1))
                .unwrap()
                .unwrap()
                .binding
                .health,
            ProjectionHealth::Disabled
        );
        let repository = store
            .repository_binding_for_workspace(WorkspaceId(1), PrincipalId(1))
            .unwrap()
            .unwrap();
        assert!(!repository.binding.network_enabled);
        assert!(repository.credential_handle.is_none());
        assert!(matches!(
            store.workspace_artifact_for_principal(WorkspaceArtifactId(1), PrincipalId(1)),
            Err(MetadataError::WorkspaceArtifactNotFound(_))
        ));
        store.integrity_check().unwrap();
    }

    fn seed_memory_state(config: &Config) -> (MetadataStore, String) {
        let store = MetadataStore::open(&metadata_path(config), Arc::new(MemorySecretStore::new()))
            .unwrap();
        store.apply_migrations(false).unwrap();
        store.bootstrap_local("memory backup tester").unwrap();
        let (_, api_token) = store
            .issue_api_token(PrincipalId(1), None, "restore-revokes", None)
            .unwrap();
        (store, api_token)
    }

    fn decrypted_entries(archive_path: &Path, password: &str) -> BTreeMap<String, Vec<u8>> {
        let mut archive = ZipArchive::new(File::open(archive_path).unwrap()).unwrap();
        let names = (0..archive.len())
            .map(|index| archive.by_index_raw(index).unwrap().name().to_string())
            .collect::<Vec<_>>();
        names
            .into_iter()
            .map(|name| {
                let mut entry = archive.by_name_decrypt(&name, password.as_bytes()).unwrap();
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes).unwrap();
                (name, bytes)
            })
            .collect()
    }

    fn write_test_archive(
        path: &Path,
        password: &str,
        entries: impl IntoIterator<Item = (String, Vec<u8>, bool)>,
    ) {
        let mut writer = ZipWriter::new(private_create_new(path).unwrap());
        for (name, bytes, encrypted) in entries {
            if encrypted {
                start_encrypted_entry(&mut writer, &name, password).unwrap();
            } else {
                writer
                    .start_file(
                        name,
                        SimpleFileOptions::default()
                            .compression_method(CompressionMethod::Deflated)
                            .unix_permissions(0o600),
                    )
                    .unwrap();
            }
            writer.write_all(&bytes).unwrap();
        }
        writer.finish().unwrap().sync_all().unwrap();
    }

    fn replace_manifest(
        entries: &mut BTreeMap<String, Vec<u8>>,
        update: impl FnOnce(&mut BackupManifest),
    ) {
        let mut manifest: BackupManifest =
            serde_json::from_slice(entries.get(MANIFEST_ENTRY).unwrap()).unwrap();
        update(&mut manifest);
        entries.insert(
            MANIFEST_ENTRY.to_string(),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        );
    }

    fn write_encrypted_entries(path: &Path, password: &str, entries: &BTreeMap<String, Vec<u8>>) {
        write_test_archive(
            path,
            password,
            entries
                .iter()
                .map(|(name, bytes)| (name.clone(), bytes.clone(), true)),
        );
    }

    fn replace_equal_bytes(path: &Path, from: &[u8], to: &[u8]) {
        assert_eq!(from.len(), to.len());
        let mut bytes = std::fs::read(path).unwrap();
        let mut replacements = 0;
        for offset in 0..=bytes.len() - from.len() {
            if &bytes[offset..offset + from.len()] == from {
                bytes[offset..offset + to.len()].copy_from_slice(to);
                replacements += 1;
            }
        }
        assert!(
            replacements >= 2,
            "local and central names must both be patched"
        );
        std::fs::write(path, bytes).unwrap();
    }

    async fn seed_ai_chat(
        store: &MetadataStore,
        tenant: TenantId,
        title: &str,
    ) -> (sift_protocol::AiChat, sift_protocol::AiRunLease) {
        let chat = store
            .create_ai_chat(
                tenant,
                None,
                PrincipalId(1),
                sift_protocol::AiVisibility::Private,
                title.into(),
            )
            .await
            .unwrap();
        let request: sift_protocol::StartAiTurnRequest = serde_json::from_value(serde_json::json!({
            "client_request_id": Uuid::new_v4(), "desktop_id": Uuid::new_v4(), "prompt": "Find slow joins",
            "provider": "codex", "model": null, "mode": "propose",
            "context": {"target": {"tenant_id": tenant.0}, "database": null, "dialect": "postgres",
                "environment_label": null, "sql": null, "current_error": null, "staged_change_count": 0}
        })).unwrap();
        let target = request.context.target.clone();
        let lease = store
            .start_ai_run(chat.id, PrincipalId(1), request)
            .await
            .unwrap();
        store
            .append_ai_provider_event(
                lease.run.id,
                PrincipalId(1),
                lease.lease_token,
                Uuid::new_v4(),
                sift_protocol::AiEventKind::MessageCompleted,
                serde_json::json!({"text":"Inspect the join plan"}),
            )
            .await
            .unwrap();
        store
            .stage_ai_query_proposal(
                lease.run.id,
                PrincipalId(1),
                sift_protocol::StageAiQueryProposalRequest {
                    client_request_id: Uuid::new_v4(),
                    lease_token: lease.lease_token,
                    target,
                    base_revision: 1,
                    proposed_sql: "SELECT 42".into(),
                },
            )
            .await
            .unwrap();
        (chat, lease)
    }

    #[tokio::test]
    async fn ai_backup_restores_encrypted_history_proposals_and_interrupts_live_runs() {
        let directory = tempfile::tempdir().unwrap();
        let source = file_config(&directory.path().join("source"), "91");
        let destination = file_config(&directory.path().join("destination"), "92");
        seed_file_state(&source).await;
        seed_file_state(&destination).await;
        let store = crate::metadata_runtime::open_metadata_store(&source)
            .unwrap()
            .unwrap();
        let (chat, lease) = seed_ai_chat(&store, TenantId(1), "Archived AI history").await;
        store
            .rotate_ai_content_key(TenantId(1), PrincipalId(1))
            .await
            .unwrap();
        let other = crate::metadata_runtime::open_metadata_store(&destination)
            .unwrap()
            .unwrap();
        let (replaced, _) = seed_ai_chat(&other, TenantId(1), "Destination discarded chat").await;
        drop(other);
        drop(store);
        let backup_key = directory.path().join("backup.key");
        write_private_key(&backup_key, "93");
        let archive = directory.path().join("ai.sift-backup");
        let manifest = create(&source, &archive, &backup_key).unwrap();
        assert!(manifest
            .payloads
            .iter()
            .any(|payload| payload.name == ai_content::ENTRY));
        assert!(!std::fs::read(&archive)
            .unwrap()
            .windows(b"Archived AI history".len())
            .any(|window| window == b"Archived AI history"));
        inspect(&archive, &backup_key).unwrap();
        restore(&destination, &archive, &backup_key, true, false)
            .await
            .unwrap();
        let restored = crate::metadata_runtime::open_metadata_store(&destination)
            .unwrap()
            .unwrap();
        assert_eq!(
            restored
                .get_ai_chat(chat.id, PrincipalId(1))
                .await
                .unwrap()
                .title,
            chat.title
        );
        assert!(restored
            .get_ai_chat(replaced.id, PrincipalId(1))
            .await
            .is_err());
        let runs = restored
            .list_ai_runs(chat.id, PrincipalId(1))
            .await
            .unwrap();
        assert_eq!(runs[0].prompt, "Find slow joins");
        assert_eq!(runs[0].run.status, sift_protocol::AiRunStatus::Interrupted);
        let events = restored
            .list_ai_run_events(lease.run.id, PrincipalId(1), 0)
            .await
            .unwrap();
        assert!(events.iter().any(|event| event
            .content
            .as_ref()
            .is_some_and(|value| value["text"] == "Inspect the join plan")));
        assert_eq!(
            events.last().unwrap().kind,
            sift_protocol::AiEventKind::Stopped
        );
        assert_eq!(
            restored
                .list_ai_query_proposals(chat.id, PrincipalId(1))
                .await
                .unwrap()[0]
                .proposed_sql,
            "SELECT 42"
        );
        assert!(restored
            .append_ai_provider_event(
                lease.run.id,
                PrincipalId(1),
                lease.lease_token,
                Uuid::new_v4(),
                sift_protocol::AiEventKind::MessageDelta,
                serde_json::json!({"text":"cannot resume"})
            )
            .await
            .is_err());
    }

    #[tokio::test]
    async fn ai_tenant_restore_replaces_selected_keys_and_preserves_unrelated_live_chat() {
        use sift_metadata::{MembershipRole, TenantKind};
        let directory = tempfile::tempdir().unwrap();
        let config = file_config(&directory.path().join("instance"), "94");
        seed_file_state(&config).await;
        let store = crate::metadata_runtime::open_metadata_store(&config)
            .unwrap()
            .unwrap();
        let other = store
            .create_tenant("unrelated AI", TenantKind::Team)
            .unwrap()
            .id;
        store
            .upsert_tenant_membership(other, PrincipalId(1), MembershipRole::Owner)
            .unwrap();
        let (selected, _) = seed_ai_chat(&store, TenantId(1), "Selected snapshot").await;
        let (unrelated, unrelated_lease) = seed_ai_chat(&store, other, "Unrelated live chat").await;
        drop(store);
        let key = directory.path().join("archive.key");
        write_private_key(&key, "95");
        let archive = directory.path().join("tenant-ai.sift-backup");
        create(&config, &archive, &key).unwrap();
        let store = crate::metadata_runtime::open_metadata_store(&config)
            .unwrap()
            .unwrap();
        store
            .rotate_ai_content_key(TenantId(1), PrincipalId(1))
            .await
            .unwrap();
        store
            .rotate_ai_content_key(other, PrincipalId(1))
            .await
            .unwrap();
        store
            .delete_ai_chat(selected.id, PrincipalId(1))
            .await
            .unwrap();
        let (newer, _) = seed_ai_chat(&store, TenantId(1), "Selected newer chat").await;
        store
            .append_ai_provider_event(
                unrelated_lease.run.id,
                PrincipalId(1),
                unrelated_lease.lease_token,
                Uuid::new_v4(),
                sift_protocol::AiEventKind::MessageDelta,
                serde_json::json!({"text":"Current unrelated reply"}),
            )
            .await
            .unwrap();
        drop(store);
        tenant::restore_tenant(&config, &archive, &key, 1, false)
            .await
            .unwrap();
        let store = crate::metadata_runtime::open_metadata_store(&config)
            .unwrap()
            .unwrap();
        assert!(store.get_ai_chat(newer.id, PrincipalId(1)).await.is_ok());
        assert!(store
            .get_ai_chat(selected.id, PrincipalId(1))
            .await
            .is_err());
        drop(store);
        tenant::restore_tenant(&config, &archive, &key, 1, true)
            .await
            .unwrap();
        let store = crate::metadata_runtime::open_metadata_store(&config)
            .unwrap()
            .unwrap();
        assert_eq!(
            store
                .get_ai_chat(selected.id, PrincipalId(1))
                .await
                .unwrap()
                .title,
            selected.title
        );
        assert!(store.get_ai_chat(newer.id, PrincipalId(1)).await.is_err());
        assert_eq!(
            store
                .get_ai_chat(unrelated.id, PrincipalId(1))
                .await
                .unwrap()
                .title,
            unrelated.title
        );
        assert_eq!(
            store
                .list_ai_runs(selected.id, PrincipalId(1))
                .await
                .unwrap()[0]
                .run
                .status,
            sift_protocol::AiRunStatus::Interrupted
        );
        assert_eq!(
            store
                .list_ai_runs(unrelated.id, PrincipalId(1))
                .await
                .unwrap()[0]
                .run
                .status,
            sift_protocol::AiRunStatus::Running
        );
        let events = store
            .list_ai_run_events(unrelated_lease.run.id, PrincipalId(1), 0)
            .await
            .unwrap();
        assert_eq!(
            events.last().unwrap().content.as_ref().unwrap()["text"],
            "Current unrelated reply"
        );
        store
            .rotate_ai_content_key(TenantId(1), PrincipalId(1))
            .await
            .unwrap();
        assert!(store
            .list_ai_query_proposals(selected.id, PrincipalId(1))
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn ai_backup_rejects_missing_blobs_keys_and_incomplete_legacy_archive() {
        let directory = tempfile::tempdir().unwrap();
        let config = file_config(&directory.path().join("instance"), "96");
        seed_file_state(&config).await;
        let store = crate::metadata_runtime::open_metadata_store(&config)
            .unwrap()
            .unwrap();
        seed_ai_chat(&store, TenantId(1), "Complete content required").await;
        let handles = store.ai_content_handles_offline(None).unwrap();
        drop(store);
        let key = directory.path().join("archive.key");
        write_private_key(&key, "97");
        let archive = directory.path().join("valid.sift-backup");
        let manifest = create(&config, &archive, &key).unwrap();
        let extracted = directory.path().join("extracted");
        extract_archive(&archive, &read_archive_password(&key).unwrap(), &extracted).unwrap();
        // An older archive that omitted the body bundle must fail even with
        // correct metadata, secrets, manifest digests, and archive encryption.
        let mut incomplete = manifest.clone();
        incomplete.format_version = 1;
        incomplete
            .payloads
            .retain(|payload| payload.name != ai_content::ENTRY);
        let paths = incomplete
            .payloads
            .iter()
            .map(|payload| (payload.name.clone(), extracted.join(&payload.name)))
            .collect();
        let bad = directory.path().join("incomplete.sift-backup");
        write_archive(
            &bad,
            &read_archive_password(&key).unwrap(),
            &incomplete,
            &paths,
        )
        .unwrap();
        assert!(inspect(&bad, &key)
            .unwrap_err()
            .to_string()
            .contains("encrypted blob bundle"));
        // Malformed records are rejected after authentic archive extraction.
        remove_dir_if_exists(&extracted.join(METADATA_ENTRY).with_extension("ai-content")).unwrap();
        let bundle = extracted.join(ai_content::ENTRY);
        let mut bytes = std::fs::read(&bundle).unwrap();
        bytes[36..40].copy_from_slice(&u32::MAX.to_le_bytes());
        std::fs::write(&bundle, &bytes).unwrap();
        assert!(ai_content::extract_and_validate(&extracted, &manifest)
            .unwrap_err()
            .to_string()
            .contains("size limit"));
        let (tenant, handle) = &handles[0];
        let removed_path = metadata_path(&config)
            .with_extension("ai-content")
            .join(tenant.to_string())
            .join(handle);
        let removed_content = std::fs::read(&removed_path).unwrap();
        std::fs::remove_file(&removed_path).unwrap();
        let refused = directory.path().join("missing.sift-backup");
        assert!(create(&config, &refused, &key).is_err());
        assert!(!refused.exists());
        std::fs::write(&removed_path, removed_content).unwrap();
        let keys = FileSecretStore::open(
            secret_file_path(&metadata_path(&config)),
            configured_secret_key_path(&config).unwrap(),
        )
        .unwrap();
        let active = keys
            .get("sift.ai.content.key", "tenant-1-active")
            .await
            .unwrap()
            .unwrap();
        keys.delete(
            "sift.ai.content.key",
            &format!("tenant-1-key-{}", Uuid::from_slice(&active).unwrap()),
        )
        .await
        .unwrap();
        let refused = directory.path().join("missing-key.sift-backup");
        assert!(create(&config, &refused, &key)
            .unwrap_err()
            .to_string()
            .contains("content key is missing"));
        assert!(!refused.exists());
    }

    #[tokio::test]
    async fn file_backup_round_trip_preserves_secrets_but_revokes_tokens() {
        let directory = tempfile::tempdir().unwrap();
        let source_root = directory.path().join("source");
        let destination_root = directory.path().join("destination");
        let source = file_config(&source_root, "11");
        let destination = file_config(&destination_root, "22");
        let (api_token, expected_secret) = seed_file_state(&source).await;
        let backup_key = directory.path().join("backup.key");
        write_private_key(&backup_key, "ab");
        let archive = directory.path().join("state.sift-backup");

        let created = create(&source, &archive, &backup_key).unwrap();
        assert_eq!(
            created.metadata.current_version, created.metadata.latest_version,
            "a backup created from current state must record the latest embedded schema"
        );
        assert!(archive.is_file());
        assert_eq!(inspect(&archive, &backup_key).unwrap(), created);

        let destination_key =
            std::fs::read(destination.metadata.secret_key_file.as_ref().unwrap()).unwrap();
        let dry_run = restore(&destination, &archive, &backup_key, false, false)
            .await
            .unwrap();
        assert!(!dry_run.apply);
        assert!(!metadata_path(&destination).exists());

        let applied = restore(&destination, &archive, &backup_key, true, false)
            .await
            .unwrap();
        assert!(applied.apply);
        assert!(applied.sessions_revoked);
        assert_eq!(applied.rescue_archive, None);
        assert_eq!(
            std::fs::read(destination.metadata.secret_key_file.as_ref().unwrap()).unwrap(),
            destination_key,
            "restore must preserve the destination encryption key"
        );

        let destination_secrets = Arc::new(
            FileSecretStore::open(
                secret_file_path(&metadata_path(&destination)),
                destination.metadata.secret_key_file.as_ref().unwrap(),
            )
            .unwrap(),
        );
        assert_eq!(
            destination_secrets.get("test", "credential").await.unwrap(),
            Some(expected_secret)
        );
        assert_eq!(
            destination_secrets
                .get("vcs-credential", "git")
                .await
                .unwrap(),
            None
        );
        let restored =
            MetadataStore::open(&metadata_path(&destination), destination_secrets).unwrap();
        assert!(restored.verify_api_token(&api_token).unwrap().is_none());
        assert_eq!(
            restored
                .list_transfer_recipes_for_principal(WorkspaceId(1), PrincipalId(1))
                .unwrap()
                .len(),
            1,
            "durable definitions survive backup and restore"
        );
        let repository = restored
            .repository_binding_for_workspace(WorkspaceId(1), PrincipalId(1))
            .unwrap()
            .expect("repository binding survives backup and restore");
        assert_eq!(repository.binding.repository_identity, "ab".repeat(32));
        assert!(!repository.binding.credential_handle_present);
        assert!(repository.credential_handle.is_none());
        assert!(matches!(
            restored.workspace_artifact_for_principal(WorkspaceArtifactId(1), PrincipalId(1)),
            Err(MetadataError::WorkspaceArtifactNotFound(_))
        ));
        restored.integrity_check().unwrap();
    }

    #[tokio::test]
    async fn restore_creates_and_reports_a_rescue_archive() {
        let directory = tempfile::tempdir().unwrap();
        let source = file_config(&directory.path().join("source"), "31");
        let destination = file_config(&directory.path().join("destination"), "32");
        seed_file_state(&source).await;
        seed_file_state(&destination).await;
        let backup_key = directory.path().join("backup.key");
        write_private_key(&backup_key, "cd");
        let archive = directory.path().join("state.sift-backup");
        create(&source, &archive, &backup_key).unwrap();

        let report = restore(&destination, &archive, &backup_key, true, false)
            .await
            .unwrap();
        let rescue = report
            .rescue_archive
            .expect("existing state gets a rescue archive");
        assert!(rescue.is_file());
        let rescue_report = inspect(&rescue, &backup_key).unwrap();
        assert_eq!(
            rescue_report.metadata.current_version, rescue_report.metadata.latest_version,
            "the rescue archive must capture the current destination schema"
        );
    }

    #[tokio::test]
    async fn wrong_archive_key_is_rejected_without_destination_changes() {
        let directory = tempfile::tempdir().unwrap();
        let source = file_config(&directory.path().join("source"), "41");
        let destination = file_config(&directory.path().join("destination"), "42");
        seed_file_state(&source).await;
        let backup_key = directory.path().join("backup.key");
        let wrong_key = directory.path().join("wrong.key");
        write_private_key(&backup_key, "de");
        write_private_key(&wrong_key, "ef");
        let archive = directory.path().join("state.sift-backup");
        create(&source, &archive, &backup_key).unwrap();

        assert!(inspect(&archive, &wrong_key).is_err());
        assert!(restore(&destination, &archive, &wrong_key, true, false)
            .await
            .is_err());
        assert!(!metadata_path(&destination).exists());
    }

    #[tokio::test]
    async fn memory_backup_captures_live_wal_and_preserves_destination_identity() {
        let directory = tempfile::tempdir().unwrap();
        let source = memory_config(&directory.path().join("source"));
        let destination = memory_config(&directory.path().join("destination"));
        let (source_store, api_token) = seed_memory_state(&source);
        let source_runtime = crate::runtime::RuntimeState::acquire(&source).unwrap();
        let source_instance_id = source_runtime.instance_id.clone();
        drop(source_runtime);
        let destination_runtime = crate::runtime::RuntimeState::acquire(&destination).unwrap();
        let destination_instance_id = destination_runtime.instance_id.clone();
        drop(destination_runtime);
        assert_ne!(source_instance_id, destination_instance_id);

        let backup_key = directory.path().join("backup.key");
        write_private_key(&backup_key, "51");
        let archive = directory.path().join("memory.sift-backup");
        let manifest = create(&source, &archive, &backup_key).unwrap();
        assert_eq!(
            manifest.secrets,
            SecretDisposition::Memory { durable: false }
        );
        assert_eq!(
            manifest.source_instance_id.as_deref(),
            Some(source_instance_id.as_str())
        );

        // Format-1 archives without AI references still recover completely.
        let password = read_archive_password(&backup_key).unwrap();
        let mut entries = decrypted_entries(&archive, &password);
        replace_manifest(&mut entries, |manifest| manifest.format_version = 1);
        let archive = directory.path().join("legacy-memory.sift-backup");
        write_encrypted_entries(&archive, &password, &entries);
        assert_eq!(inspect(&archive, &backup_key).unwrap().format_version, 1);

        // Keep the writer alive across backup creation: the principal and
        // token are committed in WAL mode and must still be in the snapshot.
        assert!(source_store
            .principal_by_id(PrincipalId(1))
            .unwrap()
            .is_some());
        let report = restore(&destination, &archive, &backup_key, true, false)
            .await
            .unwrap();
        assert_eq!(
            report.destination_instance_id.as_deref(),
            Some(destination_instance_id.as_str())
        );
        let restored = MetadataStore::open(
            &metadata_path(&destination),
            Arc::new(MemorySecretStore::new()),
        )
        .unwrap();
        assert!(restored.principal_by_id(PrincipalId(1)).unwrap().is_some());
        assert!(restored.verify_api_token(&api_token).unwrap().is_none());
    }

    #[tokio::test]
    async fn keychain_restore_requires_explicit_external_secret_acknowledgement() {
        let directory = tempfile::tempdir().unwrap();
        let source = memory_config(&directory.path().join("source"));
        let mut destination = memory_config(&directory.path().join("destination"));
        destination.metadata.secret_backend = "keychain".to_string();
        let (_store, _) = seed_memory_state(&source);
        let backup_key = directory.path().join("backup.key");
        write_private_key(&backup_key, "61");
        let password = read_archive_password(&backup_key).unwrap();
        let memory_archive = directory.path().join("memory.sift-backup");
        let archive = directory.path().join("keychain.sift-backup");
        create(&source, &memory_archive, &backup_key).unwrap();
        let mut entries = decrypted_entries(&memory_archive, &password);
        replace_manifest(&mut entries, |manifest| {
            manifest.secrets = SecretDisposition::Keychain {
                external_secrets_required: true,
            };
        });
        write_encrypted_entries(&archive, &password, &entries);

        let manifest = inspect(&archive, &backup_key).unwrap();
        assert_eq!(
            manifest.secrets,
            SecretDisposition::Keychain {
                external_secrets_required: true
            }
        );
        let error = restore(&destination, &archive, &backup_key, false, false)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("--allow-external-secrets"));
        let report = restore(&destination, &archive, &backup_key, false, true)
            .await
            .unwrap();
        assert!(report.external_secrets_required);
        assert!(!metadata_path(&destination).exists());
    }

    #[tokio::test]
    async fn hostile_archives_are_rejected_and_failed_apply_cleans_staging() {
        let directory = tempfile::tempdir().unwrap();
        let source = memory_config(&directory.path().join("source"));
        let destination = memory_config(&directory.path().join("destination"));
        let (_store, _) = seed_memory_state(&source);
        let backup_key = directory.path().join("backup.key");
        write_private_key(&backup_key, "71");
        let password = read_archive_password(&backup_key).unwrap();
        let archive = directory.path().join("valid.sift-backup");
        create(&source, &archive, &backup_key).unwrap();
        let entries = decrypted_entries(&archive, &password);

        let tampered = directory.path().join("tampered.sift-backup");
        let mut tampered_entries = entries.clone();
        tampered_entries.get_mut(METADATA_ENTRY).unwrap()[0] ^= 0x01;
        write_encrypted_entries(&tampered, &password, &tampered_entries);
        assert!(inspect(&tampered, &backup_key)
            .unwrap_err()
            .to_string()
            .contains("integrity verification"));
        assert!(restore(&destination, &tampered, &backup_key, true, false)
            .await
            .is_err());
        assert!(!metadata_path(&destination).exists());
        assert!(!restore_journal_path(&destination).exists());
        assert_eq!(
            std::fs::read_dir(metadata_path(&destination).parent().unwrap())
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".sift-restore-"))
                .count(),
            0
        );

        let oversized = directory.path().join("oversized.sift-backup");
        let mut oversized_entries = entries.clone();
        replace_manifest(&mut oversized_entries, |manifest| {
            manifest
                .payloads
                .iter_mut()
                .find(|payload| payload.name == METADATA_ENTRY)
                .unwrap()
                .size = MAX_PAYLOAD_BYTES + 1;
        });
        write_encrypted_entries(&oversized, &password, &oversized_entries);
        assert!(inspect(&oversized, &backup_key)
            .unwrap_err()
            .to_string()
            .contains("16 GiB limit"));

        let future = directory.path().join("future.sift-backup");
        let mut future_entries = entries.clone();
        replace_manifest(&mut future_entries, |manifest| {
            manifest.format_version = FORMAT_VERSION + 1;
        });
        write_encrypted_entries(&future, &password, &future_entries);
        assert!(inspect(&future, &backup_key)
            .unwrap_err()
            .to_string()
            .contains("unsupported backup format version"));

        let unknown = directory.path().join("traversal.sift-backup");
        write_test_archive(
            &unknown,
            &password,
            [("../metadata.sqlite".to_string(), vec![1], true)],
        );
        assert!(inspect(&unknown, &backup_key)
            .unwrap_err()
            .to_string()
            .contains("unknown entry"));

        let duplicate = directory.path().join("duplicate.sift-backup");
        write_test_archive(
            &duplicate,
            &password,
            [
                (
                    MANIFEST_ENTRY.to_string(),
                    entries[MANIFEST_ENTRY].clone(),
                    true,
                ),
                (
                    METADATA_ENTRY.to_string(),
                    entries[METADATA_ENTRY].clone(),
                    true,
                ),
                (
                    "metadata.sqlitx".to_string(),
                    entries[METADATA_ENTRY].clone(),
                    true,
                ),
            ],
        );
        replace_equal_bytes(&duplicate, b"metadata.sqlitx", METADATA_ENTRY.as_bytes());
        assert!(inspect(&duplicate, &backup_key)
            .unwrap_err()
            .to_string()
            .contains("duplicate"));

        let unencrypted = directory.path().join("unencrypted.sift-backup");
        write_test_archive(
            &unencrypted,
            &password,
            [
                (
                    MANIFEST_ENTRY.to_string(),
                    entries[MANIFEST_ENTRY].clone(),
                    false,
                ),
                (
                    METADATA_ENTRY.to_string(),
                    entries[METADATA_ENTRY].clone(),
                    false,
                ),
            ],
        );
        assert!(inspect(&unencrypted, &backup_key)
            .unwrap_err()
            .to_string()
            .contains("is not encrypted"));
    }

    #[tokio::test]
    async fn running_destination_blocks_restore_before_any_state_change() {
        let directory = tempfile::tempdir().unwrap();
        let source = memory_config(&directory.path().join("source"));
        let destination = memory_config(&directory.path().join("destination"));
        let (_store, _) = seed_memory_state(&source);
        let backup_key = directory.path().join("backup.key");
        write_private_key(&backup_key, "75");
        let archive = directory.path().join("state.sift-backup");
        create(&source, &archive, &backup_key).unwrap();

        let runtime = crate::runtime::RuntimeState::acquire(&destination).unwrap();
        let error = restore(&destination, &archive, &backup_key, true, false)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("offline maintenance lock"));
        assert!(!metadata_path(&destination).exists());
        assert!(!restore_journal_path(&destination).exists());
        drop(runtime);

        assert!(restore(&destination, &archive, &backup_key, true, false)
            .await
            .is_ok());
    }

    #[test]
    fn restore_journal_recovers_each_replacement_phase() {
        for phase in [
            RestorePhase::Prepared,
            RestorePhase::SecretsInstalled,
            RestorePhase::ContentInstalled,
            RestorePhase::MetadataInstalled,
            RestorePhase::Committed,
        ] {
            let directory = tempfile::tempdir().unwrap();
            let config = memory_config(directory.path());
            let metadata = metadata_path(&config);
            let secrets = secret_file_path(&metadata);
            let old_metadata = directory.path().join("old-metadata");
            let old_secrets = directory.path().join("old-secrets");
            let content = metadata.with_extension("ai-content");
            let old_content = directory.path().join("old-content");
            let content_replaced = matches!(
                phase,
                RestorePhase::ContentInstalled
                    | RestorePhase::MetadataInstalled
                    | RestorePhase::Committed
            );
            std::fs::create_dir(&content).unwrap();
            std::fs::write(
                content.join("body"),
                if content_replaced { b"new" } else { b"old" },
            )
            .unwrap();
            if content_replaced {
                std::fs::create_dir(&old_content).unwrap();
                std::fs::write(old_content.join("body"), b"old").unwrap();
            }
            let staging = directory.path().join("staging");
            std::fs::create_dir(&staging).unwrap();
            std::fs::write(staging.join("payload"), b"staged").unwrap();

            let metadata_replaced = matches!(
                phase,
                RestorePhase::MetadataInstalled | RestorePhase::Committed
            );
            let secrets_replaced = !matches!(phase, RestorePhase::Prepared);
            std::fs::write(&metadata, if metadata_replaced { b"new" } else { b"old" }).unwrap();
            std::fs::write(&secrets, if secrets_replaced { b"new" } else { b"old" }).unwrap();
            if metadata_replaced {
                std::fs::write(&old_metadata, b"old").unwrap();
            }
            if secrets_replaced {
                std::fs::write(&old_secrets, b"old").unwrap();
            }
            write_restore_journal(
                &config,
                &RestoreJournal {
                    schema_version: 2,
                    phase,
                    metadata_path: metadata.clone(),
                    secrets_path: Some(secrets.clone()),
                    old_metadata_path: old_metadata.clone(),
                    old_secrets_path: Some(old_secrets.clone()),
                    staging_dir: staging.clone(),
                    had_metadata: true,
                    had_secrets: true,
                    content_path: Some(content.clone()),
                    old_content_path: Some(old_content.clone()),
                    had_content: true,
                },
            )
            .unwrap();

            recover_interrupted_restore(&config).unwrap();
            let expected = if phase == RestorePhase::Committed {
                b"new".as_slice()
            } else {
                b"old".as_slice()
            };
            assert_eq!(std::fs::read(&metadata).unwrap(), expected, "{phase:?}");
            assert_eq!(std::fs::read(&secrets).unwrap(), expected, "{phase:?}");
            assert_eq!(
                std::fs::read(content.join("body")).unwrap(),
                expected,
                "{phase:?}"
            );
            assert!(!old_content.exists(), "{phase:?}");
            assert!(!old_metadata.exists(), "{phase:?}");
            assert!(!old_secrets.exists(), "{phase:?}");
            assert!(!staging.exists(), "{phase:?}");
            assert!(!restore_journal_path(&config).exists(), "{phase:?}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn backup_and_restored_state_are_private_and_loose_keys_are_rejected() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let source = file_config(&directory.path().join("source"), "81");
        let destination = file_config(&directory.path().join("destination"), "82");
        seed_file_state(&source).await;
        let backup_key = directory.path().join("backup.key");
        write_private_key(&backup_key, "83");
        let archive = directory.path().join("state.sift-backup");
        create(&source, &archive, &backup_key).unwrap();
        assert_eq!(
            std::fs::metadata(&archive).unwrap().permissions().mode() & 0o777,
            0o600
        );

        restore(&destination, &archive, &backup_key, true, false)
            .await
            .unwrap();
        assert_eq!(
            std::fs::metadata(metadata_path(&destination))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(secret_file_path(&metadata_path(&destination)))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        std::fs::set_permissions(&backup_key, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(inspect(&archive, &backup_key)
            .unwrap_err()
            .to_string()
            .contains("must not be accessible"));
    }
}
