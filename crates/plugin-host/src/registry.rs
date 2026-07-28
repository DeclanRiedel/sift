use std::path::Path;

use sift_extension_protocol::{ContributionId, ExtensionManifest, SegmentId};
use sift_metadata::{MetadataStore, NewExtensionContribution, NewExtensionPackage};
use sift_protocol::ExtensionProvenance;
use thiserror::Error;

use crate::{InstalledPackage, PackageError, PackageLimits, PackageStore, SignaturePolicy};

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error(transparent)]
    Package(#[from] PackageError),
    #[error(transparent)]
    Metadata(#[from] sift_metadata::MetadataError),
    #[error("manifest contribution identity is invalid: {0}")]
    InvalidContribution(String),
    #[error("signed provenance requires a verified signature")]
    UnverifiedProvenance,
}

pub struct ExtensionPackageRegistry {
    packages: PackageStore,
    metadata: MetadataStore,
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

    pub fn reconcile_staging(&self) -> Result<Vec<std::path::PathBuf>, RegistryError> {
        self.packages.reconcile_staging().map_err(Into::into)
    }

    pub fn selected_package_exists(&self, extension_id: &str) -> Result<bool, RegistryError> {
        let selected = self.metadata.extension_selection(extension_id)?;
        Ok(self
            .packages
            .package_path(&selected.selected_archive_sha256)
            .is_some())
    }
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
