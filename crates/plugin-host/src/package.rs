use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{Read, Seek, Write},
    path::{Component, Path, PathBuf},
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use semver::Version;
use sha2::{Digest, Sha256};
use sift_extension_protocol::{
    ExtensionManifest, LockedFile, PackageLock, DRIVER_RPC_VERSION, EXTENSION_RPC_VERSION,
};
use thiserror::Error;
use zip::ZipArchive;

pub const MANIFEST_PATH: &str = "sift-extension.toml";
pub const LOCK_PATH: &str = "sift-extension.lock";
pub const SIGNATURE_PATH: &str = "sift-extension.sig";
const MAX_CONTROL_FILE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone)]
pub struct PackageLimits {
    pub max_archive_bytes: u64,
    pub max_expanded_bytes: u64,
    pub max_entries: usize,
}

impl Default for PackageLimits {
    fn default() -> Self {
        Self {
            max_archive_bytes: 256 * 1024 * 1024,
            max_expanded_bytes: 1024 * 1024 * 1024,
            max_entries: 4_096,
        }
    }
}

#[derive(Debug, Error)]
pub enum PackageError {
    #[error("package I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid ZIP package: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("package archive exceeds the configured byte limit")]
    ArchiveTooLarge,
    #[error("package expands beyond the configured byte limit")]
    ExpandedTooLarge,
    #[error("package contains too many entries")]
    TooManyEntries,
    #[error("package entry path is unsafe: {0}")]
    UnsafePath(String),
    #[error("package entry type is forbidden: {0}")]
    ForbiddenEntryType(String),
    #[error("package contains a duplicate or case-colliding path: {0}")]
    DuplicatePath(String),
    #[error("required package file is missing: {0}")]
    MissingFile(&'static str),
    #[error("package contains an undeclared file: {0}")]
    UndeclaredFile(String),
    #[error("package lock is not exact RFC 8785 canonical JSON")]
    NonCanonicalLock,
    #[error("package lock is invalid: {0}")]
    InvalidLock(String),
    #[error("manifest is invalid: {0}")]
    InvalidManifest(String),
    #[error("package file differs from its lock entry: {0}")]
    DigestMismatch(String),
    #[error("package signature is required")]
    SignatureRequired,
    #[error("package signature is malformed")]
    InvalidSignatureEncoding,
    #[error("package signature verification failed")]
    InvalidSignature,
    #[error("an immutable package directory exists with unexpected contents")]
    ImmutableCollision,
}

#[derive(Debug, Clone, Copy)]
pub enum SignaturePolicy<'a> {
    AllowUnsigned,
    Require(&'a VerifyingKey),
    RequireAny(&'a [VerifyingKey]),
}

#[derive(Debug, Clone)]
pub struct ValidatedPackage {
    pub archive_sha256: String,
    pub manifest_sha256: String,
    pub manifest: ExtensionManifest,
    pub lock: PackageLock,
    pub signed: bool,
}

#[derive(Debug, Clone)]
pub struct InstalledPackage {
    pub validated: ValidatedPackage,
    pub path: PathBuf,
}

pub struct PackageValidator {
    limits: PackageLimits,
}

impl PackageValidator {
    pub fn new(limits: PackageLimits) -> Self {
        Self { limits }
    }

    pub fn validate_path(
        &self,
        path: &Path,
        signature_policy: SignaturePolicy<'_>,
    ) -> Result<ValidatedPackage, PackageError> {
        let metadata = fs::metadata(path)?;
        if metadata.len() > self.limits.max_archive_bytes {
            return Err(PackageError::ArchiveTooLarge);
        }
        let archive_sha256 = hash_reader(File::open(path)?)?.0;
        let mut archive = ZipArchive::new(File::open(path)?)?;
        self.validate_archive(&mut archive, archive_sha256, signature_policy)
    }

    fn validate_archive<R: Read + Seek>(
        &self,
        archive: &mut ZipArchive<R>,
        archive_sha256: String,
        signature_policy: SignaturePolicy<'_>,
    ) -> Result<ValidatedPackage, PackageError> {
        if archive.len() > self.limits.max_entries {
            return Err(PackageError::TooManyEntries);
        }

        let entries = index_entries(archive, &self.limits)?;
        for path in [MANIFEST_PATH, LOCK_PATH, SIGNATURE_PATH] {
            if entries
                .get(path)
                .is_some_and(|entry| entry.size > MAX_CONTROL_FILE_BYTES)
            {
                return Err(PackageError::ArchiveTooLarge);
            }
        }
        let lock_bytes = read_indexed(archive, &entries, LOCK_PATH)?
            .ok_or(PackageError::MissingFile(LOCK_PATH))?;
        let signature = read_indexed(archive, &entries, SIGNATURE_PATH)?;
        let signed = verify_signature(&lock_bytes, signature.as_deref(), signature_policy)?;

        let lock: PackageLock = serde_json::from_slice(&lock_bytes)
            .map_err(|error| PackageError::InvalidLock(error.to_string()))?;
        validate_lock(&lock, &lock_bytes)?;

        let declared: BTreeSet<_> = lock.files.iter().map(|file| file.path.as_str()).collect();
        for path in entries.keys().filter(|path| {
            path.as_str() != LOCK_PATH && path.as_str() != SIGNATURE_PATH && !path.ends_with('/')
        }) {
            if !declared.contains(path.as_str()) {
                return Err(PackageError::UndeclaredFile(path.clone()));
            }
        }

        for locked in &lock.files {
            validate_locked_file(locked)?;
            let bytes = read_indexed(archive, &entries, &locked.path)?
                .ok_or_else(|| PackageError::DigestMismatch(locked.path.clone()))?;
            let (digest, length) = hash_reader(bytes.as_slice())?;
            if digest != locked.sha256 || length != locked.byte_length {
                return Err(PackageError::DigestMismatch(locked.path.clone()));
            }
        }

        let manifest_bytes = read_indexed(archive, &entries, MANIFEST_PATH)?
            .ok_or(PackageError::MissingFile(MANIFEST_PATH))?;
        let (manifest_sha256, _) = hash_reader(manifest_bytes.as_slice())?;
        if manifest_sha256 != lock.manifest_sha256 {
            return Err(PackageError::DigestMismatch(MANIFEST_PATH.into()));
        }
        let manifest: ExtensionManifest = toml::from_str(
            std::str::from_utf8(&manifest_bytes)
                .map_err(|error| PackageError::InvalidManifest(error.to_string()))?,
        )
        .map_err(|error| PackageError::InvalidManifest(error.to_string()))?;
        validate_manifest(&manifest, &lock)?;

        Ok(ValidatedPackage {
            archive_sha256,
            manifest_sha256,
            manifest,
            lock,
            signed,
        })
    }
}

pub struct PackageStore {
    root: PathBuf,
    validator: PackageValidator,
}

impl PackageStore {
    pub fn new(root: impl Into<PathBuf>, limits: PackageLimits) -> Self {
        Self {
            root: root.into(),
            validator: PackageValidator::new(limits),
        }
    }

    pub fn install(
        &self,
        archive_path: &Path,
        signature_policy: SignaturePolicy<'_>,
    ) -> Result<InstalledPackage, PackageError> {
        let validated = self
            .validator
            .validate_path(archive_path, signature_policy)?;
        fs::create_dir_all(self.root.join("packages"))?;
        fs::create_dir_all(self.root.join("staging"))?;
        make_private(&self.root)?;
        make_private(&self.root.join("packages"))?;
        make_private(&self.root.join("staging"))?;
        sync_directory(&self.root)?;

        let final_path = self.root.join("packages").join(&validated.archive_sha256);
        if final_path.exists() {
            let marker = final_path.join(".archive-sha256");
            if fs::read_to_string(marker)?.trim() != validated.archive_sha256 {
                return Err(PackageError::ImmutableCollision);
            }
            return Ok(InstalledPackage {
                validated,
                path: final_path,
            });
        }

        let staging = tempfile::Builder::new()
            .prefix(&format!("{}-", validated.archive_sha256))
            .tempdir_in(self.root.join("staging"))?;
        let staging_path = staging.path();
        extract_verified(
            archive_path,
            staging_path,
            &validated.lock,
            &validated.manifest,
        )?;
        write_synced(
            &staging_path.join(".archive-sha256"),
            validated.archive_sha256.as_bytes(),
        )?;
        sync_tree(staging_path)?;
        if let Err(error) = fs::rename(staging_path, &final_path) {
            let marker = final_path.join(".archive-sha256");
            if !final_path.is_dir()
                || fs::read_to_string(marker)
                    .map(|value| value.trim() != validated.archive_sha256)
                    .unwrap_or(true)
            {
                return Err(error.into());
            }
        }
        sync_directory(&self.root.join("packages"))?;

        Ok(InstalledPackage {
            validated,
            path: final_path,
        })
    }

    pub fn validate(
        &self,
        archive_path: &Path,
        signature_policy: SignaturePolicy<'_>,
    ) -> Result<ValidatedPackage, PackageError> {
        self.validator.validate_path(archive_path, signature_policy)
    }

    pub fn read_package_file(
        &self,
        archive_sha256: &str,
        relative_path: &str,
        max_bytes: usize,
    ) -> Result<Vec<u8>, PackageError> {
        let relative_path = normalize_path(relative_path)?;
        let package = self
            .package_path(archive_sha256)
            .ok_or(PackageError::ImmutableCollision)?;
        let path = package.join(relative_path);
        let metadata = fs::metadata(&path)?;
        if !metadata.is_file() || metadata.len() > max_bytes as u64 {
            return Err(PackageError::ArchiveTooLarge);
        }
        fs::read(path).map_err(Into::into)
    }

    pub fn reconcile_staging(&self) -> Result<Vec<PathBuf>, PackageError> {
        let staging = self.root.join("staging");
        if !staging.exists() {
            return Ok(Vec::new());
        }
        let mut removed = Vec::new();
        for entry in fs::read_dir(&staging)? {
            let path = entry?.path();
            if path.is_dir() {
                fs::remove_dir_all(&path)?;
                removed.push(path);
            }
        }
        sync_directory(&staging)?;
        Ok(removed)
    }

    pub fn package_path(&self, archive_sha256: &str) -> Option<PathBuf> {
        if !valid_sha256(archive_sha256) {
            return None;
        }
        let path = self.root.join("packages").join(archive_sha256);
        path.is_dir().then_some(path)
    }
}

#[derive(Debug, Clone)]
struct IndexedEntry {
    index: usize,
    size: u64,
}

fn index_entries<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    limits: &PackageLimits,
) -> Result<BTreeMap<String, IndexedEntry>, PackageError> {
    let mut entries = BTreeMap::new();
    let mut folded = BTreeSet::new();
    let mut expanded = 0_u64;
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        let path = normalize_path(entry.name())?;
        let folded_path = path.to_ascii_lowercase();
        if entries.contains_key(&path) || !folded.insert(folded_path) {
            return Err(PackageError::DuplicatePath(path));
        }
        if let Some(mode) = entry.unix_mode() {
            let kind = mode & 0o170000;
            if kind != 0 && kind != 0o100000 && kind != 0o040000 {
                return Err(PackageError::ForbiddenEntryType(path));
            }
        }
        expanded = expanded
            .checked_add(entry.size())
            .ok_or(PackageError::ExpandedTooLarge)?;
        if expanded > limits.max_expanded_bytes {
            return Err(PackageError::ExpandedTooLarge);
        }
        entries.insert(
            path,
            IndexedEntry {
                index,
                size: entry.size(),
            },
        );
    }
    Ok(entries)
}

fn normalize_path(raw: &str) -> Result<String, PackageError> {
    if raw.is_empty() || raw.contains('\\') || raw.contains('\0') {
        return Err(PackageError::UnsafePath(raw.into()));
    }
    let path = Path::new(raw);
    if path.is_absolute() {
        return Err(PackageError::UnsafePath(raw.into()));
    }
    for component in path.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err(PackageError::UnsafePath(raw.into()));
        }
    }
    let normalized = path
        .components()
        .map(|component| component.as_os_str().to_str())
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| PackageError::UnsafePath(raw.into()))?
        .join("/");
    if normalized != raw.trim_end_matches('/') {
        return Err(PackageError::UnsafePath(raw.into()));
    }
    Ok(if raw.ends_with('/') {
        format!("{normalized}/")
    } else {
        normalized
    })
}

fn read_indexed<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    entries: &BTreeMap<String, IndexedEntry>,
    path: &str,
) -> Result<Option<Vec<u8>>, PackageError> {
    let Some(indexed) = entries.get(path) else {
        return Ok(None);
    };
    let capacity = usize::try_from(indexed.size).map_err(|_| PackageError::ExpandedTooLarge)?;
    let mut bytes = Vec::with_capacity(capacity);
    archive.by_index(indexed.index)?.read_to_end(&mut bytes)?;
    Ok(Some(bytes))
}

fn validate_lock(lock: &PackageLock, raw: &[u8]) -> Result<(), PackageError> {
    if !valid_sha256(&lock.manifest_sha256) {
        return Err(PackageError::InvalidLock(
            "manifest digest must be lowercase SHA-256".into(),
        ));
    }
    let canonical = canonical_lock(lock)?;
    if canonical.as_bytes() != raw {
        return Err(PackageError::NonCanonicalLock);
    }
    if lock.files.is_empty() || lock.files[0].path != MANIFEST_PATH {
        return Err(PackageError::InvalidLock(
            "manifest must be the first sorted lock entry".into(),
        ));
    }
    let mut previous = None;
    for file in &lock.files {
        if previous.is_some_and(|path: &str| path >= file.path.as_str()) {
            return Err(PackageError::InvalidLock(
                "file entries must be uniquely sorted by path".into(),
            ));
        }
        previous = Some(file.path.as_str());
    }
    Ok(())
}

fn canonical_lock(lock: &PackageLock) -> Result<String, PackageError> {
    let mut output = String::from("{\"files\":[");
    for (index, file) in lock.files.iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        output.push_str("{\"byte_length\":");
        output.push_str(&file.byte_length.to_string());
        output.push_str(",\"path\":");
        output.push_str(
            &serde_json::to_string(&file.path)
                .map_err(|error| PackageError::InvalidLock(error.to_string()))?,
        );
        output.push_str(",\"sha256\":");
        output.push_str(
            &serde_json::to_string(&file.sha256)
                .map_err(|error| PackageError::InvalidLock(error.to_string()))?,
        );
        output.push('}');
    }
    output.push_str("],\"manifest_sha256\":");
    output.push_str(
        &serde_json::to_string(&lock.manifest_sha256)
            .map_err(|error| PackageError::InvalidLock(error.to_string()))?,
    );
    output.push('}');
    Ok(output)
}

fn verify_signature(
    lock_bytes: &[u8],
    signature_bytes: Option<&[u8]>,
    policy: SignaturePolicy<'_>,
) -> Result<bool, PackageError> {
    let Some(raw) = signature_bytes else {
        return match policy {
            SignaturePolicy::AllowUnsigned => Ok(false),
            SignaturePolicy::Require(_) | SignaturePolicy::RequireAny(_) => {
                Err(PackageError::SignatureRequired)
            }
        };
    };
    let text = std::str::from_utf8(raw).map_err(|_| PackageError::InvalidSignatureEncoding)?;
    let decoded = URL_SAFE_NO_PAD
        .decode(text.trim())
        .map_err(|_| PackageError::InvalidSignatureEncoding)?;
    let signature =
        Signature::from_slice(&decoded).map_err(|_| PackageError::InvalidSignatureEncoding)?;
    match policy {
        SignaturePolicy::AllowUnsigned => Ok(false),
        SignaturePolicy::Require(key) => {
            key.verify(lock_bytes, &signature)
                .map_err(|_| PackageError::InvalidSignature)?;
            Ok(true)
        }
        SignaturePolicy::RequireAny(keys) => {
            if keys.is_empty() {
                return Err(PackageError::SignatureRequired);
            }
            keys.iter()
                .any(|key| key.verify(lock_bytes, &signature).is_ok())
                .then_some(true)
                .ok_or(PackageError::InvalidSignature)
        }
    }
}

fn validate_locked_file(file: &LockedFile) -> Result<(), PackageError> {
    normalize_path(&file.path)?;
    if file.path == LOCK_PATH || file.path == SIGNATURE_PATH || file.path.ends_with('/') {
        return Err(PackageError::InvalidLock(format!(
            "forbidden lock path {}",
            file.path
        )));
    }
    if !valid_sha256(&file.sha256) {
        return Err(PackageError::InvalidLock(format!(
            "invalid digest for {}",
            file.path
        )));
    }
    Ok(())
}

fn validate_manifest(manifest: &ExtensionManifest, lock: &PackageLock) -> Result<(), PackageError> {
    if manifest.schema_version != 1 {
        return Err(PackageError::InvalidManifest(
            "unsupported schema_version".into(),
        ));
    }
    Version::parse(&manifest.version)
        .map_err(|error| PackageError::InvalidManifest(error.to_string()))?;
    Version::parse(&manifest.minimum_sift_version)
        .map_err(|error| PackageError::InvalidManifest(error.to_string()))?;
    if !manifest.compatibility.public_protocol.contains(1)
        || !manifest
            .compatibility
            .extension_rpc
            .contains(EXTENSION_RPC_VERSION)
        || !manifest
            .compatibility
            .driver_rpc
            .contains(DRIVER_RPC_VERSION)
        || !manifest.compatibility.public_protocol.is_valid()
        || !manifest.compatibility.extension_rpc.is_valid()
        || !manifest.compatibility.driver_rpc.is_valid()
    {
        return Err(PackageError::InvalidManifest(
            "incompatible or invalid protocol range".into(),
        ));
    }
    let locked: BTreeSet<_> = lock.files.iter().map(|file| file.path.as_str()).collect();
    for artifact in &manifest.artifacts {
        normalize_path(&artifact.path)?;
        if !locked.contains(artifact.path.as_str())
            || !valid_sha256(&artifact.sha256)
            || !lock.files.iter().any(|file| {
                file.path == artifact.path
                    && file.sha256 == artifact.sha256
                    && file.byte_length == artifact.byte_length
            })
        {
            return Err(PackageError::InvalidManifest(format!(
                "artifact {} does not match the lock",
                artifact.path
            )));
        }
    }
    for data in &manifest.data {
        normalize_path(&data.path)?;
        if !lock.files.iter().any(|file| {
            file.path == data.path
                && file.sha256 == data.sha256
                && file.byte_length == data.byte_length
        }) {
            return Err(PackageError::InvalidManifest(format!(
                "data file {} does not match the lock",
                data.path
            )));
        }
    }
    Ok(())
}

fn extract_verified(
    archive_path: &Path,
    destination: &Path,
    lock: &PackageLock,
    manifest: &ExtensionManifest,
) -> Result<(), PackageError> {
    let mut archive = ZipArchive::new(File::open(archive_path)?)?;
    for locked in &lock.files {
        let mut entry = archive.by_name(&locked.path)?;
        let output = destination.join(&locked.path);
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = File::create(output)?;
        std::io::copy(&mut entry, &mut file)?;
        file.sync_all()?;
    }
    #[cfg(unix)]
    for artifact in &manifest.artifacts {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            destination.join(&artifact.path),
            fs::Permissions::from_mode(0o500),
        )?;
    }
    let bytes = {
        let mut lock_entry = archive.by_name(LOCK_PATH)?;
        let mut bytes = Vec::new();
        lock_entry.read_to_end(&mut bytes)?;
        bytes
    };
    write_synced(&destination.join(LOCK_PATH), &bytes)?;
    if let Ok(mut signature) = archive.by_name(SIGNATURE_PATH) {
        let mut bytes = Vec::new();
        signature.read_to_end(&mut bytes)?;
        write_synced(&destination.join(SIGNATURE_PATH), &bytes)?;
    }
    Ok(())
}

fn hash_reader(mut reader: impl Read) -> Result<(String, u64), std::io::Error> {
    let mut hasher = Sha256::new();
    let mut length = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        length = length
            .checked_add(read as u64)
            .ok_or_else(|| std::io::Error::other("input length overflow"))?;
    }
    Ok((format!("{:x}", hasher.finalize()), length))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn write_synced(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    let mut file = File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn sync_tree(path: &Path) -> Result<(), std::io::Error> {
    for entry in fs::read_dir(path)? {
        let path = entry?.path();
        if path.is_dir() {
            sync_tree(&path)?;
        }
    }
    sync_directory(path)
}

fn sync_directory(path: &Path) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn make_private(path: &Path) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use zip::write::SimpleFileOptions;

    const MANIFEST: &str = r#"schema_version = 1
id = "acme/example"
name = "Example"
version = "1.2.3"
authors = ["Acme"]
description = "Example"
license = "Apache-2.0"
repository = "https://example.invalid/acme/example"
minimum_sift_version = "0.2.0"

[compatibility]
public_protocol = { minimum = 1, maximum = 1 }
extension_rpc = { minimum = 1, maximum = 1 }
driver_rpc = { minimum = 1, maximum = 1 }
"#;

    fn build_package(path: &Path, signing_key: Option<&SigningKey>, extra: Option<&str>) {
        let manifest_sha256 = hash_reader(MANIFEST.as_bytes()).unwrap().0;
        let lock = PackageLock {
            manifest_sha256: manifest_sha256.clone(),
            files: vec![LockedFile {
                path: MANIFEST_PATH.into(),
                sha256: manifest_sha256,
                byte_length: MANIFEST.len() as u64,
            }],
        };
        let lock_bytes = canonical_lock(&lock).unwrap();
        let mut archive = zip::ZipWriter::new(File::create(path).unwrap());
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        archive.start_file(MANIFEST_PATH, options).unwrap();
        archive.write_all(MANIFEST.as_bytes()).unwrap();
        archive.start_file(LOCK_PATH, options).unwrap();
        archive.write_all(lock_bytes.as_bytes()).unwrap();
        if let Some(key) = signing_key {
            let signature = key.sign(lock_bytes.as_bytes());
            archive.start_file(SIGNATURE_PATH, options).unwrap();
            archive
                .write_all(URL_SAFE_NO_PAD.encode(signature.to_bytes()).as_bytes())
                .unwrap();
        }
        if let Some(path) = extra {
            archive.start_file(path, options).unwrap();
            archive.write_all(b"undeclared").unwrap();
        }
        archive.finish().unwrap();
    }

    #[test]
    fn validates_signed_package_before_installing_immutable_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let package = temp.path().join("example.sift-extension");
        let key = SigningKey::from_bytes(&[7; 32]);
        build_package(&package, Some(&key), None);

        let store = PackageStore::new(temp.path().join("state"), PackageLimits::default());
        let installed = store
            .install(&package, SignaturePolicy::Require(&key.verifying_key()))
            .unwrap();
        assert!(installed.validated.signed);
        assert!(installed.path.join(MANIFEST_PATH).is_file());
        assert_eq!(
            store
                .package_path(&installed.validated.archive_sha256)
                .unwrap(),
            installed.path
        );
    }

    #[test]
    fn unsigned_and_undeclared_files_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let unsigned = temp.path().join("unsigned.sift-extension");
        build_package(&unsigned, None, None);
        let key = SigningKey::from_bytes(&[8; 32]);
        let validator = PackageValidator::new(PackageLimits::default());
        assert!(matches!(
            validator.validate_path(&unsigned, SignaturePolicy::Require(&key.verifying_key())),
            Err(PackageError::SignatureRequired)
        ));

        let extra = temp.path().join("extra.sift-extension");
        build_package(&extra, None, Some("extra.txt"));
        assert!(matches!(
            validator.validate_path(&extra, SignaturePolicy::AllowUnsigned),
            Err(PackageError::UndeclaredFile(path)) if path == "extra.txt"
        ));
    }

    #[test]
    fn any_trusted_publisher_key_can_verify_the_exact_lock() {
        let temp = tempfile::tempdir().unwrap();
        let package = temp.path().join("signed.sift-extension");
        let signing = SigningKey::from_bytes(&[9; 32]);
        let unrelated = SigningKey::from_bytes(&[10; 32]).verifying_key();
        let trusted = signing.verifying_key();
        build_package(&package, Some(&signing), None);
        let validator = PackageValidator::new(PackageLimits::default());
        let validated = validator
            .validate_path(&package, SignaturePolicy::RequireAny(&[unrelated, trusted]))
            .unwrap();
        assert!(validated.signed);
        assert!(matches!(
            validator.validate_path(&package, SignaturePolicy::RequireAny(&[unrelated])),
            Err(PackageError::InvalidSignature)
        ));
    }

    #[test]
    fn traversal_and_case_collisions_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let traversal = temp.path().join("traversal.sift-extension");
        build_package(&traversal, None, Some("../evil"));
        let validator = PackageValidator::new(PackageLimits::default());
        assert!(matches!(
            validator.validate_path(&traversal, SignaturePolicy::AllowUnsigned),
            Err(PackageError::UnsafePath(_))
        ));
    }

    #[test]
    fn reconciliation_removes_only_private_staging_directories() {
        let temp = tempfile::tempdir().unwrap();
        let store = PackageStore::new(temp.path().join("state"), PackageLimits::default());
        let abandoned = temp.path().join("state/staging/abandoned");
        fs::create_dir_all(&abandoned).unwrap();
        let removed = store.reconcile_staging().unwrap();
        assert_eq!(removed, vec![abandoned.clone()]);
        assert!(!abandoned.exists());
    }
}
