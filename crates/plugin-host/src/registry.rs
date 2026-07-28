use std::{
    collections::HashMap,
    fs,
    path::{Component, Path, PathBuf},
    sync::RwLock,
};

use ed25519_dalek::VerifyingKey;
use sha2::{Digest, Sha256};
use sift_extension_protocol::{ContributionId, ExtensionManifest, SegmentId};
use sift_metadata::{MetadataStore, NewExtensionContribution, NewExtensionPackage};
use sift_protocol::ExtensionProvenance;
use thiserror::Error;

use crate::{InstalledPackage, PackageError, PackageLimits, PackageStore, SignaturePolicy};

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("development override I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Package(#[from] PackageError),
    #[error(transparent)]
    Metadata(#[from] sift_metadata::MetadataError),
    #[error("manifest contribution identity is invalid: {0}")]
    InvalidContribution(String),
    #[error("signed provenance requires a verified signature")]
    UnverifiedProvenance,
    #[error("unsigned extension installation was not explicitly authorized")]
    UnsignedDenied,
    #[error("trusted publisher key is malformed")]
    InvalidPublisherKey,
    #[error("development overrides are disabled for hosted/team deployments")]
    HostedDevelopmentDenied,
    #[error("development override path is invalid: {0}")]
    InvalidDevelopmentPath(String),
}

pub struct ExtensionPackageRegistry {
    packages: PackageStore,
    metadata: MetadataStore,
    development: RwLock<HashMap<String, PathBuf>>,
}

impl ExtensionPackageRegistry {
    pub fn new(
        state_root: impl Into<std::path::PathBuf>,
        limits: PackageLimits,
        metadata: MetadataStore,
    ) -> Self {
        Self {
            packages: PackageStore::new(state_root, limits),
            metadata,
            development: RwLock::new(HashMap::new()),
        }
    }

    pub fn install(
        &self,
        archive_path: &Path,
        signature_policy: SignaturePolicy<'_>,
        provenance: ExtensionProvenance,
    ) -> Result<InstalledPackage, RegistryError> {
        let installed = self.packages.install(archive_path, signature_policy)?;
        if matches!(
            provenance,
            ExtensionProvenance::Verified | ExtensionProvenance::Bundled
        ) && !installed.validated.signed
        {
            return Err(RegistryError::UnverifiedProvenance);
        }
        let manifest_json = serde_json::to_string(&installed.validated.manifest)
            .map_err(|error| PackageError::InvalidManifest(error.to_string()))?;
        let package = NewExtensionPackage {
            archive_sha256: installed.validated.archive_sha256.clone(),
            extension_id: installed.validated.manifest.id.to_string(),
            version: installed.validated.manifest.version.clone(),
            manifest_sha256: installed.validated.manifest_sha256.clone(),
            manifest_json,
            provenance,
        };
        let contributions = flatten_contributions(&installed.validated.manifest)?;
        self.metadata
            .record_extension_package(&package, &contributions)?;
        Ok(installed)
    }

    /// Installs using active keys for the exact publisher, or an explicit
    /// checksum-pinned local-install authorization when no trusted key exists.
    pub fn install_authorized(
        &self,
        archive_path: &Path,
        allow_unsigned_local: bool,
    ) -> Result<InstalledPackage, RegistryError> {
        let inspected = self
            .packages
            .validate(archive_path, SignaturePolicy::AllowUnsigned)?;
        let keys = self
            .metadata
            .active_extension_publisher_keys(inspected.manifest.id.publisher())?
            .into_iter()
            .map(|key| {
                VerifyingKey::from_bytes(&key.public_key)
                    .map_err(|_| RegistryError::InvalidPublisherKey)
            })
            .collect::<Result<Vec<_>, _>>()?;
        if !keys.is_empty() {
            return self.install(
                archive_path,
                SignaturePolicy::RequireAny(&keys),
                ExtensionProvenance::Verified,
            );
        }
        if !allow_unsigned_local {
            return Err(RegistryError::UnsignedDenied);
        }
        self.install(
            archive_path,
            SignaturePolicy::AllowUnsigned,
            ExtensionProvenance::Local,
        )
    }

    /// Registers a live local directory as an unverified development package.
    ///
    /// Development paths are deliberately supplied by operator configuration,
    /// never inferred from a package or accepted as a tenant setting.
    pub fn register_development_override(
        &self,
        path: &Path,
        hosted: bool,
        allow_hosted_development: bool,
    ) -> Result<InstalledPackage, RegistryError> {
        if hosted && !allow_hosted_development {
            return Err(RegistryError::HostedDevelopmentDenied);
        }
        let path = fs::canonicalize(path)?;
        if !path.is_dir() {
            return Err(RegistryError::InvalidDevelopmentPath(
                "override must be a directory".into(),
            ));
        }
        let manifest_path = path.join(crate::MANIFEST_PATH);
        let manifest_bytes = fs::read(&manifest_path)?;
        if manifest_bytes.len() > 1024 * 1024 {
            return Err(PackageError::ArchiveTooLarge.into());
        }
        let manifest: ExtensionManifest = toml::from_str(
            std::str::from_utf8(&manifest_bytes)
                .map_err(|error| PackageError::InvalidManifest(error.to_string()))?,
        )
        .map_err(|error| PackageError::InvalidManifest(error.to_string()))?;
        validate_development_manifest(&path, &manifest)?;

        let manifest_sha256 = hex_sha256(&manifest_bytes);
        let mut archive_identity = Sha256::new();
        archive_identity.update(b"sift-development-override-v1\0");
        archive_identity.update(path.as_os_str().as_encoded_bytes());
        archive_identity.update(b"\0");
        archive_identity.update(&manifest_bytes);
        let archive_sha256 = format!("{:x}", archive_identity.finalize());
        let package = NewExtensionPackage {
            archive_sha256: archive_sha256.clone(),
            extension_id: manifest.id.to_string(),
            version: manifest.version.clone(),
            manifest_sha256: manifest_sha256.clone(),
            manifest_json: serde_json::to_string(&manifest)
                .map_err(|error| PackageError::InvalidManifest(error.to_string()))?,
            provenance: ExtensionProvenance::Development,
        };
        let contributions = flatten_contributions(&manifest)?;
        self.metadata
            .record_extension_package(&package, &contributions)?;
        self.development
            .write()
            .expect("development override registry poisoned")
            .insert(archive_sha256.clone(), path.clone());
        Ok(InstalledPackage {
            validated: crate::ValidatedPackage {
                archive_sha256,
                manifest_sha256,
                manifest,
                lock: sift_extension_protocol::PackageLock {
                    manifest_sha256: package.manifest_sha256,
                    files: Vec::new(),
                },
                signed: false,
            },
            path,
        })
    }

    pub fn validate(
        &self,
        archive_path: &Path,
        signature_policy: SignaturePolicy<'_>,
    ) -> Result<crate::ValidatedPackage, RegistryError> {
        self.packages
            .validate(archive_path, signature_policy)
            .map_err(Into::into)
    }

    pub fn read_package_file(
        &self,
        archive_sha256: &str,
        relative_path: &str,
        max_bytes: usize,
    ) -> Result<Vec<u8>, RegistryError> {
        if let Some(root) = self
            .development
            .read()
            .expect("development override registry poisoned")
            .get(archive_sha256)
            .cloned()
        {
            let relative = safe_development_relative_path(relative_path)?;
            let path = root.join(relative);
            let canonical = fs::canonicalize(&path)?;
            if !canonical.starts_with(&root) {
                return Err(RegistryError::InvalidDevelopmentPath(
                    "path escapes its override root".into(),
                ));
            }
            let metadata = fs::metadata(&canonical)?;
            if !metadata.is_file() || metadata.len() > max_bytes as u64 {
                return Err(PackageError::ArchiveTooLarge.into());
            }
            return fs::read(canonical)
                .map_err(PackageError::from)
                .map_err(Into::into);
        }
        self.packages
            .read_package_file(archive_sha256, relative_path, max_bytes)
            .map_err(Into::into)
    }

    pub fn reconcile_staging(&self) -> Result<Vec<std::path::PathBuf>, RegistryError> {
        self.packages.reconcile_staging().map_err(Into::into)
    }

    pub fn selected_package_exists(&self, extension_id: &str) -> Result<bool, RegistryError> {
        let selected = self.metadata.extension_selection(extension_id)?;
        if self
            .development
            .read()
            .expect("development override registry poisoned")
            .contains_key(&selected.selected_archive_sha256)
        {
            return Ok(true);
        }
        Ok(self
            .packages
            .package_path(&selected.selected_archive_sha256)
            .is_some())
    }
}

fn validate_development_manifest(
    root: &Path,
    manifest: &ExtensionManifest,
) -> Result<(), RegistryError> {
    if manifest.schema_version != 1
        || !manifest.compatibility.public_protocol.contains(1)
        || !manifest
            .compatibility
            .extension_rpc
            .contains(sift_extension_protocol::EXTENSION_RPC_VERSION)
        || !manifest
            .compatibility
            .driver_rpc
            .contains(sift_extension_protocol::DRIVER_RPC_VERSION)
    {
        return Err(PackageError::InvalidManifest(
            "development manifest is incompatible with this host".into(),
        )
        .into());
    }
    semver::Version::parse(&manifest.version)
        .map_err(|error| PackageError::InvalidManifest(error.to_string()))?;
    for file in manifest
        .artifacts
        .iter()
        .map(|artifact| artifact.path.as_str())
        .chain(manifest.data.iter().map(|data| data.path.as_str()))
    {
        let relative = safe_development_relative_path(file)?;
        let canonical = fs::canonicalize(root.join(relative))?;
        if !canonical.starts_with(root) || !canonical.is_file() {
            return Err(RegistryError::InvalidDevelopmentPath(file.into()));
        }
    }
    Ok(())
}

fn safe_development_relative_path(path: &str) -> Result<PathBuf, RegistryError> {
    let path = Path::new(path);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(RegistryError::InvalidDevelopmentPath(
            path.display().to_string(),
        ));
    }
    Ok(path.to_path_buf())
}

fn hex_sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn flatten_contributions(
    manifest: &ExtensionManifest,
) -> Result<Vec<NewExtensionContribution>, RegistryError> {
    let mut output = Vec::new();
    let mut add = |kind: &str, local_id: &SegmentId, descriptor_json: String| {
        let full = format!("{}/{kind}/{local_id}", manifest.id);
        let contribution_id = ContributionId::new(full)
            .map_err(|error| RegistryError::InvalidContribution(error.to_string()))?;
        output.push(NewExtensionContribution {
            contribution_id: contribution_id.to_string(),
            kind: kind.to_string(),
            local_id: local_id.to_string(),
            descriptor_json,
        });
        Ok::<_, RegistryError>(())
    };

    for value in &manifest.contributions.database_provider {
        add(
            "database_provider",
            &value.id,
            serde_json::to_string(value)
                .map_err(|error| RegistryError::InvalidContribution(error.to_string()))?,
        )?;
    }
    macro_rules! add_generic {
        ($field:ident, $kind:literal) => {
            for value in &manifest.contributions.$field {
                add(
                    $kind,
                    &value.id,
                    serde_json::to_string(value)
                        .map_err(|error| RegistryError::InvalidContribution(error.to_string()))?,
                )?;
            }
        };
    }
    add_generic!(tunnel_provider, "tunnel_provider");
    add_generic!(credential_broker, "credential_broker");
    add_generic!(connection_hook, "connection_hook");
    add_generic!(import_format, "import_format");
    add_generic!(export_format, "export_format");
    add_generic!(dialect_pack, "dialect_pack");
    add_generic!(command, "command");
    add_generic!(governed_tool, "governed_tool");
    add_generic!(agent_context, "agent_context");
    add_generic!(client_panel, "client_panel");
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_metadata::MemorySecretStore;
    use std::sync::Arc;

    const DEVELOPMENT_MANIFEST: &str = r#"schema_version = 1
id = "acme/development"
name = "Development"
version = "1.0.0"
authors = ["Acme"]
description = "Development override"
license = "Apache-2.0"
repository = "https://example.invalid/acme/development"
minimum_sift_version = "0.2.0"

[compatibility]
public_protocol = { minimum = 1, maximum = 1 }
extension_rpc = { minimum = 1, maximum = 1 }
driver_rpc = { minimum = 1, maximum = 1 }
"#;

    fn registry(root: &Path) -> ExtensionPackageRegistry {
        let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        ExtensionPackageRegistry::new(root.join("state"), PackageLimits::default(), metadata)
    }

    #[test]
    fn development_override_is_canonical_and_blocked_in_hosted_mode() {
        let temp = tempfile::tempdir().unwrap();
        let override_path = temp.path().join("extension");
        fs::create_dir(&override_path).unwrap();
        fs::write(
            override_path.join(crate::MANIFEST_PATH),
            DEVELOPMENT_MANIFEST,
        )
        .unwrap();
        let registry = registry(temp.path());
        assert!(matches!(
            registry.register_development_override(&override_path, true, false),
            Err(RegistryError::HostedDevelopmentDenied)
        ));
        let installed = registry
            .register_development_override(&override_path, true, true)
            .unwrap();
        assert_eq!(installed.validated.manifest.id.as_str(), "acme/development");
        assert!(!installed.validated.signed);
        assert!(registry
            .selected_package_exists("acme/development")
            .unwrap());
        assert_eq!(
            registry
                .read_package_file(
                    &installed.validated.archive_sha256,
                    crate::MANIFEST_PATH,
                    1024 * 1024,
                )
                .unwrap(),
            DEVELOPMENT_MANIFEST.as_bytes()
        );
    }

    #[test]
    fn development_file_reads_cannot_escape_the_registered_root() {
        let temp = tempfile::tempdir().unwrap();
        let override_path = temp.path().join("extension");
        fs::create_dir(&override_path).unwrap();
        fs::write(
            override_path.join(crate::MANIFEST_PATH),
            DEVELOPMENT_MANIFEST,
        )
        .unwrap();
        let registry = registry(temp.path());
        let installed = registry
            .register_development_override(&override_path, false, false)
            .unwrap();
        assert!(matches!(
            registry.read_package_file(&installed.validated.archive_sha256, "../outside", 1024),
            Err(RegistryError::InvalidDevelopmentPath(_))
        ));
    }
}
