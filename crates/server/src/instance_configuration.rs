//! Safe file-backed editing for reproducible instance desired state.

use anyhow::{Context as _, Result};
use fs2::FileExt as _;
use sha2::{Digest as _, Sha256};
use sift_api_types::InstanceConfigurationDocument;
use sift_instance_config::{LockFile, Manifest};
use std::io::Write as _;
use std::path::{Path, PathBuf};

const MAX_SOURCE_BYTES: u64 = 1024 * 1024;
const EDIT_LOCK_FILE: &str = ".sift-configuration.lock";

pub fn read(root: &Path) -> Result<InstanceConfigurationDocument> {
    let lock = open_edit_lock(root)?;
    fs2::FileExt::lock_shared(&lock).context("locking instance configuration for reading")?;
    read_unlocked(root)
}

fn read_unlocked(root: &Path) -> Result<InstanceConfigurationDocument> {
    let manifest_path = checked_regular_file(root, "sift.toml")?;
    let source = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    let manifest = Manifest::parse(&source).context("validating sift.toml")?;
    let lock = generated_lock(&manifest)?;
    Ok(InstanceConfigurationDocument {
        source_revision: source_revision(source.as_bytes()),
        configuration_digest: manifest
            .configuration_digest()
            .context("digesting sift.toml")?,
        lock_digest: lock.digest().context("digesting sift.lock")?,
        manifest_id: manifest.manifest_id.to_string(),
        name: manifest.name.clone(),
        manifest: source,
    })
}

pub fn update(
    root: &Path,
    source: &str,
    expected_source_revision: Option<&str>,
) -> Result<InstanceConfigurationDocument> {
    enforce_source_size(source)?;
    let lock = open_edit_lock(root)?;
    lock.lock_exclusive()
        .context("locking instance configuration for editing")?;
    let current = read_unlocked(root)?;
    if expected_source_revision.is_some_and(|expected| expected != current.source_revision) {
        anyhow::bail!("sift.toml changed since it was opened");
    }
    let manifest = Manifest::parse(source).context("validating sift.toml")?;
    if manifest.manifest_id.to_string() != current.manifest_id {
        anyhow::bail!("manifest_id cannot change when editing an existing instance");
    }
    write_validated(root, source, &manifest)?;
    read_unlocked(root)
}

/// Creates the two-file desired-state root. The destination may exist, but it
/// must not already contain either managed file.
pub fn create(root: &Path, source: &str) -> Result<InstanceConfigurationDocument> {
    enforce_source_size(source)?;
    let manifest = Manifest::parse(source).context("validating sift.toml")?;
    std::fs::create_dir_all(root).context("creating instance root")?;
    let root = std::fs::canonicalize(root).context("canonicalizing instance root")?;
    let lock = open_edit_lock(&root)?;
    lock.lock_exclusive()
        .context("locking new instance configuration")?;
    for name in ["sift.toml", "sift.lock"] {
        if root
            .join(name)
            .try_exists()
            .context("checking instance root")?
        {
            anyhow::bail!("{} already exists", root.join(name).display());
        }
    }
    write_validated(&root, source, &manifest)?;
    read_unlocked(&root)
}

fn open_edit_lock(root: &Path) -> Result<std::fs::File> {
    let path = root.join(EDIT_LOCK_FILE);
    reject_symlink_if_present(&path)?;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))
}

fn generated_lock(manifest: &Manifest) -> Result<LockFile> {
    LockFile::generate(
        manifest,
        crate::VERSION,
        sift_protocol::PROTOCOL_VERSION_NUMBER,
    )
    .context("generating sift.lock")
}

fn enforce_source_size(source: &str) -> Result<()> {
    if source.len() as u64 > MAX_SOURCE_BYTES {
        anyhow::bail!("sift.toml exceeds the {MAX_SOURCE_BYTES}-byte limit");
    }
    Ok(())
}

fn write_validated(root: &Path, source: &str, manifest: &Manifest) -> Result<()> {
    let lock = generated_lock(manifest)?
        .to_toml_pretty()
        .context("encoding sift.lock")?;
    let manifest_path = root.join("sift.toml");
    let lock_path = root.join("sift.lock");
    reject_symlink_if_present(&manifest_path)?;
    reject_symlink_if_present(&lock_path)?;

    // Prepare and fsync both files before either destination is replaced.
    let nonce = uuid::Uuid::new_v4();
    let manifest_temp = root.join(format!(".sift.toml.{nonce}.tmp"));
    let lock_temp = root.join(format!(".sift.lock.{nonce}.tmp"));
    let manifest_backup = root.join(format!(".sift.toml.{nonce}.bak"));
    let lock_backup = root.join(format!(".sift.lock.{nonce}.bak"));
    let mut manifest_backed_up = false;
    let mut lock_backed_up = false;
    let result = (|| {
        write_new(&manifest_temp, source.as_bytes())?;
        write_new(&lock_temp, lock.as_bytes())?;
        manifest_backed_up = move_to_backup(&manifest_path, &manifest_backup)?;
        lock_backed_up = move_to_backup(&lock_path, &lock_backup)?;
        std::fs::rename(&lock_temp, &lock_path).context("committing sift.lock")?;
        std::fs::rename(&manifest_temp, &manifest_path).context("committing sift.toml")?;
        sync_directory(root)?;
        Ok(())
    })();
    if result.is_err() {
        restore_backup(&manifest_path, &manifest_backup, manifest_backed_up);
        restore_backup(&lock_path, &lock_backup, lock_backed_up);
    }
    let _ = std::fs::remove_file(&manifest_temp);
    let _ = std::fs::remove_file(&lock_temp);
    if result.is_ok() {
        let _ = std::fs::remove_file(&manifest_backup);
        let _ = std::fs::remove_file(&lock_backup);
    }
    result
}

fn move_to_backup(path: &Path, backup: &Path) -> Result<bool> {
    if !path
        .try_exists()
        .context("checking managed instance file")?
    {
        return Ok(false);
    }
    std::fs::rename(path, backup)
        .with_context(|| format!("staging backup for {}", path.display()))?;
    Ok(true)
}

fn restore_backup(path: &Path, backup: &Path, backed_up: bool) {
    if !backed_up {
        let _ = std::fs::remove_file(path);
        return;
    }
    let _ = std::fs::remove_file(path);
    let _ = std::fs::rename(backup, path);
}

fn checked_regular_file(root: &Path, name: &str) -> Result<PathBuf> {
    let path = root.join(name);
    let metadata =
        std::fs::symlink_metadata(&path).with_context(|| format!("reading {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        anyhow::bail!("{} must be a regular file", path.display());
    }
    if metadata.len() > MAX_SOURCE_BYTES {
        anyhow::bail!("{} exceeds the size limit", path.display());
    }
    Ok(path)
}

fn reject_symlink_if_present(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            anyhow::bail!("refusing to replace symlink {}", path.display())
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("checking {}", path.display())),
    }
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .with_context(|| format!("writing {}", path.display()))
}

#[cfg(unix)]
fn sync_directory(root: &Path) -> Result<()> {
    std::fs::File::open(root)
        .and_then(|directory| directory.sync_all())
        .context("syncing instance root")
}

#[cfg(not(unix))]
fn sync_directory(_: &Path) -> Result<()> {
    Ok(())
}

fn source_revision(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> String {
        include_str!("../../../examples/reproducible-instance/sift.toml").to_string()
    }

    #[test]
    fn create_and_update_regenerate_a_valid_lock() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("instance");
        let created = create(&root, &source()).unwrap();
        assert!(root.join("sift.lock").is_file());

        let edited = source().replace("name = \"demo-sift\"", "name = \"edited-sift\"");
        let updated = update(&root, &edited, Some(&created.source_revision)).unwrap();
        assert_eq!(updated.name, "edited-sift");
        let lock = std::fs::read_to_string(root.join("sift.lock")).unwrap();
        LockFile::parse(&lock)
            .unwrap()
            .verify(&Manifest::parse(&edited).unwrap())
            .unwrap();
    }

    #[test]
    fn update_rejects_stale_source_and_identity_changes() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("instance");
        let created = create(&root, &source()).unwrap();
        assert!(update(&root, &source(), Some("sha256:stale"))
            .unwrap_err()
            .to_string()
            .contains("changed since"));

        let changed_identity = source().replace(
            "b654b918-b1f1-4d70-924d-e4c1014f482f",
            "b654b918-b1f1-4d70-924d-e4c1014f4830",
        );
        assert!(
            update(&root, &changed_identity, Some(&created.source_revision))
                .unwrap_err()
                .to_string()
                .contains("manifest_id")
        );
    }
}
