//! Offline, bounded recovery of encrypted AI bodies alongside metadata/keys.
use super::*;
use sift_metadata::{FileSecretStore, MetadataStore};

pub(super) const ENTRY: &str = "ai-content.bin";
const MAGIC: &[u8; 8] = b"SIFT-AIB";
const MAX_BLOBS: usize = 100_000;
const MAX_BLOB_BYTES: usize = 1024 * 1024 + 52;

pub(super) fn collect(
    snapshot: &MetadataStore,
    source_root: &Path,
    keys: &FileSecretStore,
    output: &Path,
) -> anyhow::Result<usize> {
    let handles = snapshot.ai_content_handles_offline(None)?;
    anyhow::ensure!(
        handles.len() <= MAX_BLOBS,
        "AI backup exceeds encrypted blob inventory limit"
    );
    let mut file = private_create_new(output)?;
    file.write_all(MAGIC)?;
    file.write_all(&(handles.len() as u32).to_le_bytes())?;
    let mut total_bytes = 12u64;
    for (tenant, handle) in &handles {
        anyhow::ensure!(*tenant > 0, "AI blob tenant is invalid");
        let id = Uuid::parse_str(handle).context("AI blob handle is invalid")?;
        let sealed = MetadataStore::read_validated_ai_blob(source_root, *tenant, id, keys)?;
        anyhow::ensure!(
            sealed.len() <= MAX_BLOB_BYTES,
            "AI blob exceeds recovery size limit"
        );
        total_bytes += 28 + sealed.len() as u64;
        anyhow::ensure!(
            total_bytes <= MAX_PAYLOAD_BYTES,
            "AI bundle exceeds the recovery payload size limit"
        );
        file.write_all(&tenant.to_le_bytes())?;
        file.write_all(id.as_bytes())?;
        file.write_all(&(sealed.len() as u32).to_le_bytes())?;
        file.write_all(&sealed)?;
    }
    file.sync_all()?;
    Ok(handles.len())
}

pub(super) fn extract_and_validate(
    directory: &Path,
    manifest: &BackupManifest,
) -> anyhow::Result<()> {
    let metadata_path = directory.join(METADATA_ENTRY);
    let store = MetadataStore::open(&metadata_path, Arc::new(MemorySecretStore::new()))?;
    let handles = store.ai_content_handles_offline(None)?;
    anyhow::ensure!(
        handles.len() <= MAX_BLOBS,
        "AI recovery exceeds encrypted blob inventory limit"
    );
    let mut expected = BTreeSet::new();
    for (tenant, handle) in handles {
        anyhow::ensure!(tenant > 0, "AI blob tenant is invalid");
        expected.insert((
            tenant,
            Uuid::parse_str(&handle).context("AI blob handle is invalid")?,
        ));
    }
    let root = metadata_path.with_extension("ai-content");
    std::fs::create_dir_all(&root)?;
    make_private_dir(&root)?;
    let has_bundle = manifest
        .payloads
        .iter()
        .any(|payload| payload.name == ENTRY);
    if !has_bundle {
        anyhow::ensure!(
            expected.is_empty(),
            "AI recovery requires the encrypted blob bundle and content keys"
        );
        return Ok(());
    }
    anyhow::ensure!(
        matches!(manifest.secrets, SecretDisposition::File { portable: true }),
        "AI recovery requires portable file-secret keys"
    );
    let keys = FileSecretStore::open(
        directory.join(SECRETS_ENTRY),
        directory.join(SOURCE_SECRET_KEY_ENTRY),
    )?;
    let mut file = File::open(directory.join(ENTRY))?;
    let mut magic = [0u8; 8];
    file.read_exact(&mut magic)?;
    anyhow::ensure!(&magic == MAGIC, "AI recovery bundle version is unsupported");
    let mut count = [0u8; 4];
    file.read_exact(&mut count)?;
    let count = u32::from_le_bytes(count) as usize;
    anyhow::ensure!(
        count == expected.len() && count <= MAX_BLOBS,
        "AI bundle inventory does not match metadata"
    );
    for _ in 0..count {
        let mut tenant = [0u8; 8];
        let mut id = [0u8; 16];
        let mut size = [0u8; 4];
        file.read_exact(&mut tenant)?;
        file.read_exact(&mut id)?;
        file.read_exact(&mut size)?;
        let tenant = i64::from_le_bytes(tenant);
        let id = Uuid::from_bytes(id);
        let size = u32::from_le_bytes(size) as usize;
        anyhow::ensure!(
            expected.remove(&(tenant, id)),
            "AI bundle has duplicate or unreferenced content"
        );
        anyhow::ensure!(
            size <= MAX_BLOB_BYTES,
            "AI recovery blob exceeds size limit"
        );
        let mut sealed = vec![0u8; size];
        file.read_exact(&mut sealed)?;
        MetadataStore::install_validated_ai_blob(&root, tenant, id, sealed, &keys)?;
    }
    let mut trailing = [0u8; 1];
    anyhow::ensure!(
        file.read(&mut trailing)? == 0 && expected.is_empty(),
        "AI bundle contains trailing or missing content"
    );
    Ok(())
}

pub(super) fn stage_tenant_content(
    config: &Config,
    source: &Path,
    staging: &Path,
    tenant: i64,
) -> anyhow::Result<()> {
    let snapshot = staging.join(METADATA_ENTRY);
    let store = MetadataStore::open(&snapshot, Arc::new(MemorySecretStore::new()))?;
    let handles = store.ai_content_handles_offline(None)?;
    anyhow::ensure!(
        handles.len() <= MAX_BLOBS,
        "AI recovery exceeds blob inventory limit"
    );
    let root = snapshot.with_extension("ai-content");
    std::fs::create_dir_all(&root)?;
    make_private_dir(&root)?;
    if handles.is_empty() {
        return Ok(());
    }
    anyhow::ensure!(
        config.metadata.secret_backend == "file",
        "AI tenant recovery requires portable file-secret content keys"
    );
    let live_metadata = configured_metadata_path(config)?;
    let live_keys = FileSecretStore::open(
        secret_file_path(&live_metadata),
        configured_secret_key_path(config)?,
    )?;
    let source_keys = FileSecretStore::open(
        source.join(SECRETS_ENTRY),
        source.join(SOURCE_SECRET_KEY_ENTRY),
    )?;
    let final_keys = FileSecretStore::open(
        staging.join("restored-secrets.enc"),
        configured_secret_key_path(config)?,
    )?;
    for (owner, handle) in handles {
        let id = Uuid::parse_str(&handle).context("AI blob handle is invalid")?;
        let (source_root, keys) = if owner == tenant {
            (
                source.join(METADATA_ENTRY).with_extension("ai-content"),
                &source_keys,
            )
        } else {
            (live_metadata.with_extension("ai-content"), &live_keys)
        };
        let sealed = MetadataStore::read_validated_ai_blob(&source_root, owner, id, keys)?;
        MetadataStore::install_validated_ai_blob(&root, owner, id, sealed, &final_keys)?;
    }
    Ok(())
}
