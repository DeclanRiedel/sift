//! Bounded encrypted AI payloads. SQLite sees opaque handles, never bodies.

use std::{collections::HashMap, path::PathBuf, sync::Arc};

use chacha20poly1305::{
    aead::{Aead, KeyInit},
    ChaCha20Poly1305, Key, Nonce,
};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{secrets::SecretStore, MetadataError, Result};

const KEY_NAMESPACE: &str = "sift.ai.content.key";
const NONCE_LEN: usize = 12;
const MAX_CONTENT_BYTES: usize = 1024 * 1024;
type MemoryBlobs = Arc<Mutex<HashMap<(i64, Uuid), Vec<u8>>>>;

#[derive(Clone)]
pub(crate) struct AiContentStore {
    backend: ContentBackend,
    secrets: Arc<dyn SecretStore>,
    key_lock: Arc<Mutex<()>>,
}

#[derive(Clone)]
enum ContentBackend {
    File(PathBuf),
    Memory(MemoryBlobs),
}

impl AiContentStore {
    pub(crate) fn file(root: PathBuf, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            backend: ContentBackend::File(root),
            secrets,
            key_lock: Arc::new(Mutex::new(())),
        }
    }

    pub(crate) fn memory(secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            backend: ContentBackend::Memory(Arc::new(Mutex::new(HashMap::new()))),
            secrets,
            key_lock: Arc::new(Mutex::new(())),
        }
    }

    pub(crate) async fn put(&self, tenant_id: i64, bytes: &[u8]) -> Result<String> {
        if bytes.is_empty() || bytes.len() > MAX_CONTENT_BYTES {
            return Err(MetadataError::AiContent(
                "AI content size is outside the allowed range".into(),
            ));
        }
        let key = self.key(tenant_id, true).await?;
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::getrandom(&mut nonce)
            .map_err(|error| MetadataError::AiContent(format!("randomness failed: {error}")))?;
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
        let ciphertext = cipher
            .encrypt(Nonce::from_slice(&nonce), bytes)
            .map_err(|_| MetadataError::AiContent("AI content encryption failed".into()))?;
        let mut sealed = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        sealed.extend_from_slice(&nonce);
        sealed.extend_from_slice(&ciphertext);
        let id = Uuid::new_v4();
        match &self.backend {
            ContentBackend::File(root) => {
                let root = root.clone();
                tokio::task::spawn_blocking(move || write_file(root, tenant_id, id, sealed))
                    .await
                    .map_err(|error| {
                        MetadataError::AiContent(format!("AI content task failed: {error}"))
                    })??;
            }
            ContentBackend::Memory(content) => {
                content.lock().await.insert((tenant_id, id), sealed);
            }
        }
        Ok(id.to_string())
    }

    pub(crate) async fn get(&self, tenant_id: i64, handle: &str) -> Result<Option<Vec<u8>>> {
        let id = parse_handle(handle)?;
        let sealed = match &self.backend {
            ContentBackend::File(root) => {
                let path = blob_path(root, tenant_id, id);
                tokio::task::spawn_blocking(move || match std::fs::read(path) {
                    Ok(bytes) => Ok(Some(bytes)),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(error) => Err(MetadataError::AiContent(error.to_string())),
                })
                .await
                .map_err(|error| {
                    MetadataError::AiContent(format!("AI content task failed: {error}"))
                })??
            }
            ContentBackend::Memory(content) => content.lock().await.get(&(tenant_id, id)).cloned(),
        };
        let Some(sealed) = sealed else {
            return Ok(None);
        };
        if sealed.len() <= NONCE_LEN || sealed.len() > MAX_CONTENT_BYTES + NONCE_LEN + 16 {
            return Err(MetadataError::AiContent(
                "AI content blob is invalid".into(),
            ));
        }
        let key = self.key(tenant_id, false).await?;
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
        let plaintext = cipher
            .decrypt(
                Nonce::from_slice(&sealed[..NONCE_LEN]),
                &sealed[NONCE_LEN..],
            )
            .map_err(|_| MetadataError::AiContent("AI content decryption failed".into()))?;
        Ok(Some(plaintext))
    }

    pub(crate) async fn delete(&self, tenant_id: i64, handle: &str) -> Result<()> {
        let id = parse_handle(handle)?;
        match &self.backend {
            ContentBackend::File(root) => {
                let path = blob_path(root, tenant_id, id);
                tokio::task::spawn_blocking(move || match std::fs::remove_file(path) {
                    Ok(()) => Ok(()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(error) => Err(MetadataError::AiContent(error.to_string())),
                })
                .await
                .map_err(|error| {
                    MetadataError::AiContent(format!("AI content task failed: {error}"))
                })??;
            }
            ContentBackend::Memory(content) => {
                content.lock().await.remove(&(tenant_id, id));
            }
        }
        Ok(())
    }

    async fn key(&self, tenant_id: i64, create: bool) -> Result<[u8; 32]> {
        let _guard = self.key_lock.lock().await;
        let handle = format!("tenant-{tenant_id}");
        if let Some(raw) = self.secrets.get(KEY_NAMESPACE, &handle).await? {
            return raw.try_into().map_err(|_| {
                MetadataError::AiContent("AI tenant content key has invalid length".into())
            });
        }
        if !create {
            return Err(MetadataError::AiContent(
                "AI tenant content key is missing".into(),
            ));
        }
        let mut key = [0u8; 32];
        getrandom::getrandom(&mut key)
            .map_err(|error| MetadataError::AiContent(format!("randomness failed: {error}")))?;
        self.secrets.put(KEY_NAMESPACE, &handle, &key).await?;
        Ok(key)
    }
}

fn parse_handle(handle: &str) -> Result<Uuid> {
    Uuid::parse_str(handle)
        .map_err(|_| MetadataError::AiContent("invalid AI content handle".into()))
}

fn blob_path(root: &std::path::Path, tenant_id: i64, id: Uuid) -> PathBuf {
    root.join(tenant_id.to_string()).join(id.to_string())
}

fn write_file(root: PathBuf, tenant_id: i64, id: Uuid, sealed: Vec<u8>) -> Result<()> {
    use std::io::Write as _;
    let directory = root.join(tenant_id.to_string());
    std::fs::create_dir_all(&directory)
        .map_err(|error| MetadataError::AiContent(error.to_string()))?;
    #[cfg(unix)]
    for path in [&root, &directory] {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| MetadataError::AiContent(error.to_string()))?;
    }
    let path = directory.join(id.to_string());
    let temporary = directory.join(format!("{id}.tmp"));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|error| MetadataError::AiContent(error.to_string()))?;
    file.write_all(&sealed)
        .map_err(|error| MetadataError::AiContent(error.to_string()))?;
    file.sync_all()
        .map_err(|error| MetadataError::AiContent(error.to_string()))?;
    std::fs::rename(&temporary, path)
        .map_err(|error| MetadataError::AiContent(error.to_string()))?;
    if let Ok(directory) = std::fs::File::open(directory) {
        directory.sync_all().ok();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::MemorySecretStore;

    #[tokio::test]
    async fn content_is_encrypted_and_tenant_bound() {
        let store = AiContentStore::memory(Arc::new(MemorySecretStore::new()));
        let secret = b"confidential SQL and results";
        let handle = store.put(1, secret).await.unwrap();
        let id = Uuid::parse_str(&handle).unwrap();
        let ContentBackend::Memory(content) = &store.backend else {
            unreachable!()
        };
        let sealed = content.lock().await.get(&(1, id)).cloned().unwrap();
        assert!(!sealed.windows(secret.len()).any(|window| window == secret));
        assert_eq!(store.get(1, &handle).await.unwrap().unwrap(), secret);
        assert!(store.get(2, &handle).await.unwrap().is_none());
        store.delete(1, &handle).await.unwrap();
        assert!(store.get(1, &handle).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn file_content_is_sealed_at_rest_and_removed() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("chat-content");
        let store = AiContentStore::file(root.clone(), Arc::new(MemorySecretStore::new()));
        let secret = b"private SQL context";
        let handle = store.put(7, secret).await.unwrap();
        let raw = std::fs::read(root.join("7").join(&handle)).unwrap();
        assert!(!raw.windows(secret.len()).any(|window| window == secret));
        assert_eq!(store.get(7, &handle).await.unwrap().unwrap(), secret);
        store.delete(7, &handle).await.unwrap();
        assert!(!root.join("7").join(&handle).exists());
    }
}
