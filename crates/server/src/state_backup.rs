//! Offline, encrypted backup and restore for state owned by Sift (ADR-039).

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sift_metadata::{
    FileSecretStore, MemorySecretStore, MetadataStore, MigrationStatus, NewOperationAudit,
};
use uuid::Uuid;
use zip::write::SimpleFileOptions;
use zip::{AesMode, CompressionMethod, ZipArchive, ZipWriter};

use crate::config::Config;

const FORMAT_VERSION: u32 = 1;
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RestorePhase {
    Prepared,
    SecretsInstalled,
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

        // MetadataStore uses WAL mode. Installing only the main database while
        // a sanitized store is still open can strand the revocation writes in
        // the staging WAL. Materialize a fresh SQLite backup after sanitation
        // so the file we rename is a self-contained recovery point.
        let sanitized_metadata = staging_dir.join("sanitized-metadata.sqlite");
        {
            let staged_store =
                MetadataStore::open(&staged_metadata, Arc::new(MemorySecretStore::new()))?;
            staged_store.sanitize_restored_database()?;
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
        destination.rotate_auth_system_keys().await?;
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
    let snapshot = directory.path().join(METADATA_ENTRY);
    store.backup_database_to(&snapshot)?;
    let status = store.migration_status()?;
    let mut payload_paths = BTreeMap::new();
    payload_paths.insert(METADATA_ENTRY.to_string(), snapshot);
    let secrets = collect_secret_payloads(config, directory.path(), &mut payload_paths)?;
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
                secrets_path
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
    let file = File::open(archive_path)
        .with_context(|| format!("opening backup archive: {}", archive_path.display()))?;
    let mut archive = ZipArchive::new(file).context("decoding backup archive")?;
    if archive.len() > 4 {
        bail!("backup archive contains too many entries");
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
            MANIFEST_ENTRY | METADATA_ENTRY | SECRETS_ENTRY | SOURCE_SECRET_KEY_ENTRY
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
    Ok(manifest)
}

fn validate_manifest(
    manifest: &BackupManifest,
    archive_names: &BTreeSet<String>,
) -> anyhow::Result<()> {
    if manifest.format_version != FORMAT_VERSION {
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
            METADATA_ENTRY | SECRETS_ENTRY | SOURCE_SECRET_KEY_ENTRY
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
    let mut journal = RestoreJournal {
        schema_version: 1,
        phase: RestorePhase::Prepared,
        metadata_path: metadata_path.clone(),
        secrets_path: secrets_path.clone(),
        old_metadata_path,
        old_secrets_path,
        staging_dir: staging_dir.to_path_buf(),
        had_metadata: metadata_path.exists(),
        had_secrets: secrets_path.as_ref().is_some_and(|path| path.exists()),
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
    if journal.schema_version != 1 {
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
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn remove_if_exists(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn sync_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_metadata::{PrincipalId, SecretStore as _};

    fn write_private_key(path: &Path, byte: &str) {
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
        store.ensure_auth_system_keys().await.unwrap();
        (api_token, secret)
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
        assert_eq!(created.metadata.current_version, 28);
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
        let restored =
            MetadataStore::open(&metadata_path(&destination), destination_secrets).unwrap();
        assert!(restored.verify_api_token(&api_token).unwrap().is_none());
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
        assert_eq!(
            inspect(&rescue, &backup_key)
                .unwrap()
                .metadata
                .current_version,
            28
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
}
