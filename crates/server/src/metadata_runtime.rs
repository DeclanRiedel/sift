//! Shared metadata/secret-store construction for the daemon and offline
//! administration binary.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context};
use sift_metadata::{FileSecretStore, MemorySecretStore, MetadataStore, SecretStore};

use crate::config::Config;

pub fn build_metadata_store(cfg: &Config) -> anyhow::Result<Option<MetadataStore>> {
    if !cfg.metadata.enabled {
        return Ok(None);
    }

    let path = cfg
        .metadata
        .path
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(MetadataStore::default_local_path);
    let secrets = build_secret_store(cfg, &path)?;
    let store = MetadataStore::open(&path, secrets)
        .with_context(|| format!("opening metadata store: {}", path.display()))?;
    if cfg.metadata.bootstrap_local {
        let display_name = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "local".to_string());
        store
            .bootstrap_local(&display_name)
            .context("bootstrapping local metadata principal")?;
    }
    Ok(Some(store))
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
