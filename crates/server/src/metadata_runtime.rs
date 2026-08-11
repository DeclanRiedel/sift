//! Shared metadata/secret-store construction for the daemon and offline
//! administration binary.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context};
use sift_metadata::{
    FileSecretStore, MemorySecretStore, MetadataStore, MigrationReport, MigrationStatus,
    SecretStore,
};

use crate::config::Config;

pub fn build_metadata_store(cfg: &Config) -> anyhow::Result<Option<MetadataStore>> {
    let Some(store) = open_metadata_store(cfg)? else {
        return Ok(None);
    };
    store
        .ensure_schema_current()
        .context("checking metadata schema")?;
    bootstrap_metadata_store(cfg, &store)?;
    Ok(Some(store))
}

pub fn open_metadata_store(cfg: &Config) -> anyhow::Result<Option<MetadataStore>> {
    if !cfg.metadata.enabled {
        return Ok(None);
    }

    let path = metadata_path(cfg);
    let secrets = build_secret_store(cfg, &path)?;
    let store = MetadataStore::open(&path, secrets)
        .with_context(|| format!("opening metadata store: {}", path.display()))?;
    store
        .set_plan_capture_retention(sift_metadata::PlanCaptureRetention {
            max_capture_bytes: cfg.limits.plan_capture_max_bytes,
            max_per_tenant: cfg.limits.plan_capture_max_per_tenant,
            max_per_source: cfg.limits.plan_capture_max_per_source,
            max_age_days: cfg.limits.plan_capture_max_age_days,
        })
        .context("validating plan-capture retention limits")?;
    Ok(Some(store))
}

fn bootstrap_metadata_store(cfg: &Config, store: &MetadataStore) -> anyhow::Result<()> {
    if cfg.metadata.bootstrap_local {
        let display_name = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "local".to_string());
        store
            .bootstrap_local(&display_name)
            .context("bootstrapping local metadata principal")?;
    }
    Ok(())
}

pub fn migration_status(cfg: &Config) -> anyhow::Result<MigrationStatus> {
    let store = open_migration_store(cfg)?;
    store
        .migration_status()
        .context("inspecting metadata schema")
}

pub fn apply_metadata_migrations(
    cfg: &Config,
    automatic: bool,
) -> anyhow::Result<(MigrationReport, usize)> {
    let store = open_migration_store(cfg)?;
    let migration_lock = store
        .lock_migrations()
        .context("acquiring metadata migration lock")?;
    let mut report = store
        .apply_migrations_locked(automatic, &migration_lock)
        .context("applying metadata schema migrations")?;

    // V019 introduces the format marker. A database already at the latest SQL
    // version may still contain pre-Loro rows, so preserve a recovery snapshot
    // before that standalone data rewrite too.
    let has_legacy_documents = !store
        .list_legacy_documents()
        .context("inspecting legacy documents")?
        .is_empty();
    if automatic && has_legacy_documents {
        bail!(
            "automatic migration is blocked by the legacy document representation upgrade; run `sift-server migrate apply` explicitly"
        );
    }
    if has_legacy_documents && report.backup.is_none() {
        report.backup = store
            .create_migration_backup(report.to_version)
            .context("backing up metadata before document upgrade")?;
    }
    let upgraded = crate::document_actor::upgrade_legacy_documents(&store)
        .context("upgrading legacy documents to Loro snapshots")?;
    if upgraded > 0 {
        store
            .require_minimum_compatible_version(19)
            .context("recording document migration compatibility floor")?;
        tracing::info!(
            count = upgraded,
            "upgraded legacy documents to Loro snapshots"
        );
    }
    Ok((report, upgraded))
}

fn open_migration_store(cfg: &Config) -> anyhow::Result<MetadataStore> {
    if !cfg.metadata.enabled {
        bail!("metadata migrations require metadata.enabled=true");
    }
    let path = metadata_path(cfg);
    MetadataStore::open(&path, Arc::new(MemorySecretStore::new()))
        .with_context(|| format!("opening metadata store: {}", path.display()))
}

fn metadata_path(cfg: &Config) -> PathBuf {
    cfg.metadata
        .path
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(MetadataStore::default_local_path)
}

fn build_secret_store(cfg: &Config, metadata_path: &Path) -> anyhow::Result<Arc<dyn SecretStore>> {
    match cfg.metadata.secret_backend.as_str() {
        "memory" => Ok(Arc::new(MemorySecretStore::new())),
        "file" => {
            let key_file = cfg.metadata.secret_key_file.as_deref().context(
                "metadata.secret_backend = \"file\" requires metadata.secret_key_file \
                 (e.g. SIFT_METADATA__SECRET_KEY_FILE)",
            )?;
            let secrets_path = metadata_path
                .parent()
                .map(|dir| dir.join("secrets.enc"))
                .unwrap_or_else(|| PathBuf::from("secrets.enc"));
            let store = FileSecretStore::open(&secrets_path, key_file).with_context(|| {
                format!("opening encrypted secret store: {}", secrets_path.display())
            })?;
            Ok(Arc::new(store))
        }
        "keychain" => build_keychain_store(),
        other => bail!(
            "unsupported metadata.secret_backend `{other}`; expected `memory`, `file`, or `keychain`"
        ),
    }
}

#[cfg(feature = "os-keychain")]
fn build_keychain_store() -> anyhow::Result<Arc<dyn SecretStore>> {
    Ok(Arc::new(sift_metadata::OsKeychainSecretStore::new()))
}

#[cfg(not(feature = "os-keychain"))]
fn build_keychain_store() -> anyhow::Result<Arc<dyn SecretStore>> {
    bail!("metadata.secret_backend = \"keychain\" requires building sift-server with the `os-keychain` feature")
}
