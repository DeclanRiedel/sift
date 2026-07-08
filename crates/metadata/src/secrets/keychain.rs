//! `OsKeychainSecretStore` — a [`SecretStore`] backed by the OS credential
//! store via the `keyring` crate. Compiled only under the `os-keychain`
//! feature. The build is pure-Rust (a zbus-based Secret Service client on
//! Linux, no system libdbus), but at *runtime* it needs a credential service
//! (Secret Service daemon on Linux, Keychain on macOS), which headless CI
//! lacks — so the round-trip test below is `#[ignore]`d there.

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

#[cfg(test)]
mod tests {
    use super::*;

    // Requires a running Secret Service / Keychain, so it is ignored in CI.
    // Run locally with: cargo test -p sift-metadata --features os-keychain
    //   -- --ignored keychain
    #[tokio::test]
    #[ignore = "needs a running OS credential service"]
    async fn round_trips_through_os_store() {
        let store = OsKeychainSecretStore::new();
        let ns = "sift.test";
        let handle = "keychain-roundtrip";
        store.put(ns, handle, b"s3cr3t").await.unwrap();
        assert_eq!(
            store.get(ns, handle).await.unwrap().as_deref(),
            Some(&b"s3cr3t"[..])
        );
        store.delete(ns, handle).await.unwrap();
        assert_eq!(store.get(ns, handle).await.unwrap(), None);
    }
}
