//! `OsKeychainSecretStore` — a [`SecretStore`] backed by the OS credential
//! store via the `keyring` crate. Compiled only under the `os-keychain`
//! feature; the backend needs a platform credential service (Secret Service /
//! D-Bus on Linux) that is unavailable in headless CI, so it is verified
//! manually rather than in automated tests.

use async_trait::async_trait;
use keyring::{Entry, Error as KeyringError};

use crate::secrets::SecretStore;
use crate::{MetadataError, Result};

const SERVICE: &str = "sift";

#[derive(Default)]
pub struct OsKeychainSecretStore;

impl OsKeychainSecretStore {
    pub fn new() -> Self {
        Self
    }

    fn entry(namespace: &str, handle: &str) -> Result<Entry> {
        Entry::new(SERVICE, &format!("{namespace}:{handle}")).map_err(map_err)
    }
}

fn map_err(error: KeyringError) -> MetadataError {
    MetadataError::SecretStore(format!("keychain: {error}"))
}

#[async_trait]
impl SecretStore for OsKeychainSecretStore {
    async fn put(&self, namespace: &str, handle: &str, secret: &[u8]) -> Result<()> {
        Self::entry(namespace, handle)?
            .set_secret(secret)
            .map_err(map_err)
    }

    async fn get(&self, namespace: &str, handle: &str) -> Result<Option<Vec<u8>>> {
        match Self::entry(namespace, handle)?.get_secret() {
            Ok(bytes) => Ok(Some(bytes)),
            Err(KeyringError::NoEntry) => Ok(None),
            Err(error) => Err(map_err(error)),
        }
    }

    async fn delete(&self, namespace: &str, handle: &str) -> Result<()> {
        match Self::entry(namespace, handle)?.delete_credential() {
            Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
            Err(error) => Err(map_err(error)),
        }
    }
}
