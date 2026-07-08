//! `FileSecretStore` — an on-disk, encrypted [`SecretStore`] (ADR-008: secrets
//! live outside the SQLite metadata db). The whole secret map is sealed with
//! ChaCha20-Poly1305 under a 32-byte key read from a keyfile, and written
//! atomically on every mutation.
//!
//! Keyfile format: 64 hex characters (32 bytes), whitespace trimmed. The nix
//! dev shell generates one and exports its path via
//! `SIFT_METADATA__SECRET_KEY_FILE`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use chacha20poly1305::aead::Aead;
use chacha20poly1305::{ChaCha20Poly1305, Key, KeyInit, Nonce};
use serde::{Deserialize, Serialize};

use crate::secrets::SecretStore;
use crate::{MetadataError, Result};

const NONCE_LEN: usize = 12;

pub struct FileSecretStore {
    path: PathBuf,
    cipher: ChaCha20Poly1305,
    entries: Mutex<HashMap<(String, String), Vec<u8>>>,
}

#[derive(Serialize, Deserialize)]
struct StoredEntry {
    namespace: String,
    handle: String,
    secret: Vec<u8>,
}

impl FileSecretStore {
    /// Open (or create) an encrypted secret file, reading the key from
    /// `key_file`.
    pub fn open(store_path: impl AsRef<Path>, key_file: impl AsRef<Path>) -> Result<Self> {
        let key = read_key(key_file.as_ref())?;
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
        let path = store_path.as_ref().to_path_buf();
        let entries = load(&path, &cipher)?;
        Ok(Self {
            path,
            cipher,
            entries: Mutex::new(entries),
        })
    }

    fn persist(&self, entries: &HashMap<(String, String), Vec<u8>>) -> Result<()> {
        let rows: Vec<StoredEntry> = entries
            .iter()
            .map(|((namespace, handle), secret)| StoredEntry {
                namespace: namespace.clone(),
                handle: handle.clone(),
                secret: secret.clone(),
            })
            .collect();
        let plaintext = serde_json::to_vec(&rows).map_err(MetadataError::Json)?;

        let mut nonce_bytes = [0u8; NONCE_LEN];
        getrandom::getrandom(&mut nonce_bytes)
            .map_err(|error| MetadataError::SecretStore(format!("rng failure: {error}")))?;
        let ciphertext = self
            .cipher
            .encrypt(Nonce::from_slice(&nonce_bytes), plaintext.as_slice())
            .map_err(|_| MetadataError::SecretStore("secret encryption failed".into()))?;

        let mut blob = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        blob.extend_from_slice(&nonce_bytes);
        blob.extend_from_slice(&ciphertext);

        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| MetadataError::SecretStore(error.to_string()))?;
        }
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, &blob)
            .map_err(|error| MetadataError::SecretStore(error.to_string()))?;
        std::fs::rename(&tmp, &self.path)
            .map_err(|error| MetadataError::SecretStore(error.to_string()))?;
        Ok(())
    }
}

fn read_key(key_file: &Path) -> Result<[u8; 32]> {
    let raw = std::fs::read_to_string(key_file).map_err(|error| {
        MetadataError::SecretStore(format!(
            "reading secret key file {}: {error}",
            key_file.display()
        ))
    })?;
    let hex = raw.trim();
    if hex.len() != 64 {
        return Err(MetadataError::SecretStore(format!(
            "secret key file {} must be 64 hex characters (32 bytes), got {}",
            key_file.display(),
            hex.len()
        )));
    }
    let mut key = [0u8; 32];
    for (i, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).map_err(|_| {
            MetadataError::SecretStore(format!(
                "secret key file {} is not valid hex",
                key_file.display()
            ))
        })?;
    }
    Ok(key)
}

fn load(path: &Path, cipher: &ChaCha20Poly1305) -> Result<HashMap<(String, String), Vec<u8>>> {
    let blob = match std::fs::read(path) {
        Ok(blob) => blob,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(error) => return Err(MetadataError::SecretStore(error.to_string())),
    };
    if blob.len() < NONCE_LEN {
        return Err(MetadataError::SecretStore(
            "secret file is truncated".into(),
        ));
    }
    let (nonce_bytes, ciphertext) = blob.split_at(NONCE_LEN);
    let plaintext = cipher
        .decrypt(Nonce::from_slice(nonce_bytes), ciphertext)
        .map_err(|_| {
            MetadataError::SecretStore(
                "secret decryption failed (wrong key or corrupt file)".into(),
            )
        })?;
    let rows: Vec<StoredEntry> = serde_json::from_slice(&plaintext).map_err(MetadataError::Json)?;
    Ok(rows
        .into_iter()
        .map(|row| ((row.namespace, row.handle), row.secret))
        .collect())
}

#[async_trait]
impl SecretStore for FileSecretStore {
    async fn put(&self, namespace: &str, handle: &str, secret: &[u8]) -> Result<()> {
        let mut entries = self.entries.lock().unwrap();
        entries.insert((namespace.to_string(), handle.to_string()), secret.to_vec());
        self.persist(&entries)
    }

    async fn get(&self, namespace: &str, handle: &str) -> Result<Option<Vec<u8>>> {
        Ok(self
            .entries
            .lock()
            .unwrap()
            .get(&(namespace.to_string(), handle.to_string()))
            .cloned())
    }

    async fn delete(&self, namespace: &str, handle: &str) -> Result<()> {
        let mut entries = self.entries.lock().unwrap();
        entries.remove(&(namespace.to_string(), handle.to_string()));
        self.persist(&entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_key(dir: &Path) -> PathBuf {
        let key_path = dir.join("secret.key");
        let mut file = std::fs::File::create(&key_path).unwrap();
        // 64 hex chars = 32 bytes.
        writeln!(file, "{}", "ab".repeat(32)).unwrap();
        key_path
    }

    #[tokio::test]
    async fn round_trips_and_persists_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let key = write_key(dir.path());
        let store_path = dir.path().join("secrets.enc");

        {
            let store = FileSecretStore::open(&store_path, &key).unwrap();
            store.put("conn", "1", b"s3cr3t").await.unwrap();
            store.put("conn", "2", b"other").await.unwrap();
            assert_eq!(
                store.get("conn", "1").await.unwrap().as_deref(),
                Some(&b"s3cr3t"[..])
            );
            store.delete("conn", "2").await.unwrap();
        }

        // Reopen with the same key: data survives, and the file is not plaintext.
        let reopened = FileSecretStore::open(&store_path, &key).unwrap();
        assert_eq!(
            reopened.get("conn", "1").await.unwrap().as_deref(),
            Some(&b"s3cr3t"[..])
        );
        assert_eq!(reopened.get("conn", "2").await.unwrap(), None);
        let on_disk = std::fs::read(&store_path).unwrap();
        assert!(
            !on_disk.windows(6).any(|w| w == b"s3cr3t"),
            "secret must not be stored in plaintext"
        );
    }

    #[tokio::test]
    async fn wrong_key_fails_to_decrypt() {
        let dir = tempfile::tempdir().unwrap();
        let key = write_key(dir.path());
        let store_path = dir.path().join("secrets.enc");
        {
            let store = FileSecretStore::open(&store_path, &key).unwrap();
            store.put("conn", "1", b"s3cr3t").await.unwrap();
        }
        let bad_key = dir.path().join("bad.key");
        std::fs::write(&bad_key, "cd".repeat(32)).unwrap();
        assert!(FileSecretStore::open(&store_path, &bad_key).is_err());
    }

    #[test]
    fn rejects_malformed_key_file() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("short.key");
        std::fs::write(&key, "abcd").unwrap();
        assert!(FileSecretStore::open(dir.path().join("s.enc"), &key).is_err());
    }
}
