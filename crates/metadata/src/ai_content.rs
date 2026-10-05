//! Bounded, versioned encrypted AI bodies. SQLite stores opaque handles only.

use crate::{secrets::SecretStore, MetadataError, Result};
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    ChaCha20Poly1305, Key, Nonce,
};
use std::{
    collections::{BTreeSet, HashMap},
    path::PathBuf,
    sync::Arc,
};
use tokio::sync::Mutex;
use uuid::Uuid;

pub(crate) const KEY_NAMESPACE: &str = "sift.ai.content.key";
const MAGIC: &[u8; 8] = b"SIFT-AI1";
const NONCE_LEN: usize = 12;
pub(crate) const MAX_CONTENT_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_SEALED_BYTES: usize = MAX_CONTENT_BYTES + 52;
type MemoryBlobs = Arc<Mutex<HashMap<(i64, Uuid), Vec<u8>>>>;

#[derive(Clone)]
pub(crate) struct AiContentStore {
    backend: ContentBackend,
    secrets: Arc<dyn SecretStore>,
    gate: Arc<Mutex<()>>,
}
#[derive(Clone)]
enum ContentBackend {
    File(PathBuf),
    Memory(MemoryBlobs),
}
#[derive(serde::Serialize, serde::Deserialize)]
struct Rotation {
    generation: Uuid,
    retiring: BTreeSet<Uuid>,
}

fn content_error(message: &str) -> MetadataError {
    MetadataError::AiContent(message.into())
}
fn key_handle(tenant: i64, generation: Uuid) -> String {
    if generation.is_nil() {
        format!("tenant-{tenant}")
    } else {
        format!("tenant-{tenant}-key-{generation}")
    }
}
fn active_handle(tenant: i64) -> String {
    format!("tenant-{tenant}-active")
}
fn rotation_handle(tenant: i64) -> String {
    format!("tenant-{tenant}-rotation")
}
fn associated_data(tenant: i64, id: Uuid, header: &[u8]) -> Vec<u8> {
    let mut data = tenant.to_le_bytes().to_vec();
    data.extend_from_slice(id.as_bytes());
    data.extend_from_slice(header);
    data
}
fn envelope(sealed: &[u8]) -> Result<(Uuid, usize)> {
    if sealed.len() > MAX_SEALED_BYTES {
        return Err(content_error("AI content blob exceeds size limit"));
    }
    if sealed.starts_with(MAGIC) {
        if sealed.len() <= 52 {
            return Err(content_error("AI content envelope is invalid"));
        }
        let generation = Uuid::from_slice(&sealed[8..24])
            .map_err(|_| content_error("AI content key generation is invalid"))?;
        Ok((generation, 24))
    } else {
        if sealed.len() <= NONCE_LEN + 16 {
            return Err(content_error("AI content blob is invalid"));
        }
        Ok((Uuid::nil(), 0))
    }
}
impl AiContentStore {
    pub(crate) fn file(root: PathBuf, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            backend: ContentBackend::File(root),
            secrets,
            gate: Arc::new(Mutex::new(())),
        }
    }
    pub(crate) fn memory(secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            backend: ContentBackend::Memory(Arc::new(Mutex::new(HashMap::new()))),
            secrets,
            gate: Arc::new(Mutex::new(())),
        }
    }
    async fn generation_key(&self, tenant: i64, generation: Uuid) -> Result<[u8; 32]> {
        self.secrets
            .get(KEY_NAMESPACE, &key_handle(tenant, generation))
            .await?
            .ok_or_else(|| content_error("AI tenant content key is missing"))?
            .try_into()
            .map_err(|_| content_error("AI tenant content key has invalid length"))
    }
    async fn fresh_generation(&self, tenant: i64) -> Result<Uuid> {
        let generation = Uuid::new_v4();
        let mut key = [0u8; 32];
        getrandom::getrandom(&mut key).map_err(|_| content_error("AI key randomness failed"))?;
        self.secrets
            .put(KEY_NAMESPACE, &key_handle(tenant, generation), &key)
            .await?;
        Ok(generation)
    }
    async fn active_generation(&self, tenant: i64) -> Result<Uuid> {
        if let Some(bytes) = self
            .secrets
            .get(KEY_NAMESPACE, &active_handle(tenant))
            .await?
        {
            return Uuid::from_slice(&bytes)
                .map_err(|_| content_error("AI active key generation is invalid"));
        }
        let generation = self.fresh_generation(tenant).await?;
        self.secrets
            .put(KEY_NAMESPACE, &active_handle(tenant), generation.as_bytes())
            .await?;
        Ok(generation)
    }
    async fn seal(&self, tenant: i64, id: Uuid, generation: Uuid, bytes: &[u8]) -> Result<Vec<u8>> {
        if bytes.is_empty() || bytes.len() > MAX_CONTENT_BYTES {
            return Err(content_error(
                "AI content size is outside the allowed range",
            ));
        }
        let key = self.generation_key(tenant, generation).await?;
        let mut sealed = MAGIC.to_vec();
        sealed.extend_from_slice(generation.as_bytes());
        let aad = associated_data(tenant, id, &sealed);
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::getrandom(&mut nonce)
            .map_err(|_| content_error("AI nonce randomness failed"))?;
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: bytes,
                    aad: &aad,
                },
            )
            .map_err(|_| content_error("AI content encryption failed"))?;
        sealed.extend_from_slice(&nonce);
        sealed.extend_from_slice(&ciphertext);
        Ok(sealed)
    }
    async fn unseal(&self, tenant: i64, id: Uuid, sealed: &[u8]) -> Result<Vec<u8>> {
        let (generation, _) = envelope(sealed)?;
        let key = self.generation_key(tenant, generation).await?;
        unseal_with_key(tenant, id, sealed, &key)
    }

    pub(crate) async fn put(&self, tenant: i64, bytes: &[u8]) -> Result<String> {
        let _guard = self.gate.lock().await;
        let generation = self.active_generation(tenant).await?;
        let id = Uuid::new_v4();
        let sealed = self.seal(tenant, id, generation, bytes).await?;
        self.write(tenant, id, sealed, false).await?;
        Ok(id.to_string())
    }
    pub(crate) async fn get(&self, tenant: i64, handle: &str) -> Result<Option<Vec<u8>>> {
        let _guard = self.gate.lock().await;
        let id = parse_handle(handle)?;
        let Some(sealed) = self.read(tenant, id).await? else {
            return Ok(None);
        };
        Ok(Some(self.unseal(tenant, id, &sealed).await?))
    }
    pub(crate) async fn delete(&self, tenant: i64, handle: &str) -> Result<()> {
        let _guard = self.gate.lock().await;
        let id = parse_handle(handle)?;
        match &self.backend {
            ContentBackend::Memory(blobs) => {
                blobs.lock().await.remove(&(tenant, id));
            }
            ContentBackend::File(root) => {
                let path = blob_path(root, tenant, id);
                tokio::task::spawn_blocking(move || match std::fs::remove_file(path) {
                    Ok(()) => Ok(()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(error) => Err(MetadataError::Io(error)),
                })
                .await
                .map_err(|_| content_error("AI content delete task failed"))??;
            }
        }
        Ok(())
    }
    async fn read(&self, tenant: i64, id: Uuid) -> Result<Option<Vec<u8>>> {
        match &self.backend {
            ContentBackend::Memory(blobs) => Ok(blobs.lock().await.get(&(tenant, id)).cloned()),
            ContentBackend::File(root) => {
                let path = blob_path(root, tenant, id);
                tokio::task::spawn_blocking(move || read_file(&path))
                    .await
                    .map_err(|_| content_error("AI content read task failed"))?
            }
        }
    }
    async fn write(&self, tenant: i64, id: Uuid, sealed: Vec<u8>, replace: bool) -> Result<()> {
        match &self.backend {
            ContentBackend::Memory(blobs) => {
                blobs.lock().await.insert((tenant, id), sealed);
                Ok(())
            }
            ContentBackend::File(root) => {
                let root = root.clone();
                tokio::task::spawn_blocking(move || write_file(root, tenant, id, sealed, replace))
                    .await
                    .map_err(|_| content_error("AI content write task failed"))?
            }
        }
    }
    async fn ids(&self, tenant: i64) -> Result<Vec<Uuid>> {
        match &self.backend {
            ContentBackend::Memory(blobs) => Ok(blobs
                .lock()
                .await
                .keys()
                .filter(|(owner, _)| *owner == tenant)
                .map(|(_, id)| *id)
                .collect()),
            ContentBackend::File(root) => {
                let path = root.join(tenant.to_string());
                tokio::task::spawn_blocking(move || {
                    let directory = match std::fs::read_dir(path) {
                        Ok(directory) => directory,
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            return Ok(Vec::new())
                        }
                        Err(error) => return Err(MetadataError::Io(error)),
                    };
                    let mut ids = Vec::new();
                    for entry in directory {
                        let entry = entry?;
                        if let Some(id) = entry
                            .file_name()
                            .to_str()
                            .and_then(|name| Uuid::parse_str(name).ok())
                        {
                            ids.push(id);
                        }
                    }
                    Ok(ids)
                })
                .await
                .map_err(|_| content_error("AI content list task failed"))?
            }
        }
    }
    /// Hold the gate across both scanning and rewriting: an in-flight put may
    /// not leave an old-generation blob outside the rotation's inventory.
    pub(crate) async fn rotate(&self, tenant: i64) -> Result<usize> {
        let _guard = self.gate.lock().await;
        let ids = self.ids(tenant).await?;
        let mut rotation = if let Some(bytes) = self
            .secrets
            .get(KEY_NAMESPACE, &rotation_handle(tenant))
            .await?
        {
            serde_json::from_slice::<Rotation>(&bytes)?
        } else {
            Rotation {
                generation: self.fresh_generation(tenant).await?,
                retiring: BTreeSet::new(),
            }
        };
        // Persist the replacement choice before inspecting blobs, so even a
        // malformed blob or interrupted scan resumes this generation.
        self.secrets
            .put(
                KEY_NAMESPACE,
                &rotation_handle(tenant),
                &serde_json::to_vec(&rotation)?,
            )
            .await?;
        if let Some(active) = self
            .secrets
            .get(KEY_NAMESPACE, &active_handle(tenant))
            .await?
        {
            let active = Uuid::from_slice(&active)
                .map_err(|_| content_error("AI active key generation is invalid"))?;
            if active != rotation.generation {
                rotation.retiring.insert(active);
            }
        }
        for id in &ids {
            let sealed = self
                .read(tenant, *id)
                .await?
                .ok_or_else(|| content_error("AI blob disappeared during rotation"))?;
            let (generation, _) = envelope(&sealed)?;
            if generation != rotation.generation {
                rotation.retiring.insert(generation);
            }
        }
        self.secrets
            .put(
                KEY_NAMESPACE,
                &rotation_handle(tenant),
                &serde_json::to_vec(&rotation)?,
            )
            .await?;
        self.secrets
            .put(
                KEY_NAMESPACE,
                &active_handle(tenant),
                rotation.generation.as_bytes(),
            )
            .await?;
        let mut rewritten = 0;
        for id in ids {
            let sealed = self
                .read(tenant, id)
                .await?
                .ok_or_else(|| content_error("AI blob disappeared during rotation"))?;
            if envelope(&sealed)?.0 == rotation.generation {
                continue;
            }
            let bytes = self.unseal(tenant, id, &sealed).await?;
            let sealed = self.seal(tenant, id, rotation.generation, &bytes).await?;
            self.write(tenant, id, sealed, true).await?;
            rewritten += 1;
        }
        for generation in rotation.retiring {
            self.secrets
                .delete(KEY_NAMESPACE, &key_handle(tenant, generation))
                .await?;
        }
        self.secrets
            .delete(KEY_NAMESPACE, &rotation_handle(tenant))
            .await?;
        Ok(rewritten)
    }
    pub(crate) async fn recovery_key_handles(
        &self,
        tenant: i64,
        handles: &[String],
    ) -> Result<BTreeSet<String>> {
        let _guard = self.gate.lock().await;
        let mut keys = BTreeSet::new();
        if let Some(bytes) = self
            .secrets
            .get(KEY_NAMESPACE, &active_handle(tenant))
            .await?
        {
            let active = Uuid::from_slice(&bytes)
                .map_err(|_| content_error("AI active key generation is invalid"))?;
            keys.insert(active_handle(tenant));
            keys.insert(key_handle(tenant, active));
        }
        if let Some(bytes) = self
            .secrets
            .get(KEY_NAMESPACE, &rotation_handle(tenant))
            .await?
        {
            let pending: Rotation = serde_json::from_slice(&bytes)?;
            keys.insert(rotation_handle(tenant));
            keys.insert(key_handle(tenant, pending.generation));
            keys.extend(
                pending
                    .retiring
                    .into_iter()
                    .map(|generation| key_handle(tenant, generation)),
            );
        }
        for handle in handles {
            let id = parse_handle(handle)?;
            let sealed = self
                .read(tenant, id)
                .await?
                .ok_or_else(|| content_error("AI backup content blob is missing"))?;
            self.unseal(tenant, id, &sealed).await?;
            keys.insert(key_handle(tenant, envelope(&sealed)?.0));
        }
        Ok(keys)
    }
}
fn unseal_with_key(tenant: i64, id: Uuid, sealed: &[u8], key: &[u8; 32]) -> Result<Vec<u8>> {
    let (_, offset) = envelope(sealed)?;
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let aad = if offset == 0 {
        Vec::new()
    } else {
        associated_data(tenant, id, &sealed[..offset])
    };
    let plaintext = cipher
        .decrypt(
            Nonce::from_slice(&sealed[offset..offset + NONCE_LEN]),
            Payload {
                msg: &sealed[offset + NONCE_LEN..],
                aad: &aad,
            },
        )
        .map_err(|_| content_error("AI content decryption failed"))?;
    if plaintext.is_empty() || plaintext.len() > MAX_CONTENT_BYTES {
        return Err(content_error(
            "AI plaintext is outside the allowed size range",
        ));
    }
    Ok(plaintext)
}
/// Trusted offline recovery, with no asynchronous runtime or plaintext export.
pub(crate) fn validate_blob(
    tenant: i64,
    id: Uuid,
    sealed: &[u8],
    keys: &crate::FileSecretStore,
) -> Result<()> {
    if tenant <= 0 {
        return Err(content_error("AI blob tenant is invalid"));
    }
    let (generation, _) = envelope(sealed)?;
    let key: [u8; 32] = keys
        .get_blocking(KEY_NAMESPACE, &key_handle(tenant, generation))?
        .ok_or_else(|| content_error("AI recovery content key is missing"))?
        .try_into()
        .map_err(|_| content_error("AI recovery content key has invalid length"))?;
    unseal_with_key(tenant, id, sealed, &key)?;
    Ok(())
}
pub(crate) fn read_validated_blob(
    root: &std::path::Path,
    tenant: i64,
    id: Uuid,
    keys: &crate::FileSecretStore,
) -> Result<Vec<u8>> {
    let sealed = read_file(&blob_path(root, tenant, id))?
        .ok_or_else(|| content_error("AI recovery content blob is missing"))?;
    validate_blob(tenant, id, &sealed, keys)?;
    Ok(sealed)
}
pub(crate) fn install_validated_blob(
    root: &std::path::Path,
    tenant: i64,
    id: Uuid,
    sealed: Vec<u8>,
    keys: &crate::FileSecretStore,
) -> Result<()> {
    validate_blob(tenant, id, &sealed, keys)?;
    write_file(root.to_path_buf(), tenant, id, sealed, false)
}
fn read_file(path: &std::path::Path) -> Result<Option<Vec<u8>>> {
    let stat = match std::fs::symlink_metadata(path) {
        Ok(stat) => stat,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(MetadataError::Io(error)),
    };
    if !stat.is_file() || stat.len() > MAX_SEALED_BYTES as u64 {
        return Err(content_error(
            "AI content blob is not a bounded regular file",
        ));
    }
    use std::io::Read;
    let mut bytes = Vec::with_capacity(stat.len() as usize);
    std::fs::File::open(path)?
        .take(MAX_SEALED_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_SEALED_BYTES {
        return Err(content_error("AI content blob exceeds size limit"));
    }
    Ok(Some(bytes))
}
fn parse_handle(handle: &str) -> Result<Uuid> {
    Uuid::parse_str(handle).map_err(|_| content_error("invalid AI content handle"))
}
fn blob_path(root: &std::path::Path, tenant: i64, id: Uuid) -> PathBuf {
    root.join(tenant.to_string()).join(id.to_string())
}
fn write_file(root: PathBuf, tenant: i64, id: Uuid, sealed: Vec<u8>, replace: bool) -> Result<()> {
    use std::io::Write;
    let directory = root.join(tenant.to_string());
    std::fs::create_dir_all(&directory)?;
    #[cfg(unix)]
    for path in [&root, &directory] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    let destination = blob_path(&root, tenant, id);
    if !replace && destination.exists() {
        return Err(content_error("AI blob ID already exists"));
    }
    let temporary = directory.join(format!("{id}.{}.tmp", Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> Result<()> {
        let mut file = options.open(&temporary)?;
        file.write_all(&sealed)?;
        file.sync_all()?;
        std::fs::rename(&temporary, &destination)?;
        #[cfg(unix)]
        std::fs::File::open(&directory)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemorySecretStore;

    #[tokio::test]
    async fn encrypted_blobs_bind_identity_and_rotate_without_changing_handles() {
        let secrets = Arc::new(MemorySecretStore::new());
        let store = AiContentStore::memory(secrets.clone());
        let handle = store.put(1, b"confidential SQL and results").await.unwrap();
        let other = store.put(2, b"other tenant").await.unwrap();
        let id = parse_handle(&handle).unwrap();
        let before = store.read(1, id).await.unwrap().unwrap();
        assert!(!before.windows(12).any(|window| window == b"confidential"));
        let old_generation = envelope(&before).unwrap().0;
        let swapped_id = Uuid::new_v4();
        store
            .write(1, swapped_id, before.clone(), false)
            .await
            .unwrap();
        assert!(store.get(1, &swapped_id.to_string()).await.is_err());
        store.delete(1, &swapped_id.to_string()).await.unwrap();
        assert_eq!(store.rotate(1).await.unwrap(), 1);
        assert_eq!(
            store.get(1, &handle).await.unwrap().unwrap(),
            b"confidential SQL and results"
        );
        assert_eq!(
            store.get(2, &other).await.unwrap().unwrap(),
            b"other tenant"
        );
        assert!(store.get(2, &handle).await.unwrap().is_none());
        assert!(secrets
            .get(KEY_NAMESPACE, &key_handle(1, old_generation))
            .await
            .unwrap()
            .is_none());
        assert_ne!(
            envelope(&store.read(1, id).await.unwrap().unwrap())
                .unwrap()
                .0,
            old_generation
        );
    }

    #[tokio::test]
    async fn failed_rotation_keeps_readable_keys_and_resumes_same_generation() {
        let secrets = Arc::new(MemorySecretStore::new());
        let store = AiContentStore::memory(secrets.clone());
        let handle = store.put(1, b"saved SQL").await.unwrap();
        let id = parse_handle(&handle).unwrap();
        let good = store.read(1, id).await.unwrap().unwrap();
        let mut corrupt = good.clone();
        *corrupt.last_mut().unwrap() ^= 1;
        store.write(1, id, corrupt, true).await.unwrap();
        assert!(store.rotate(1).await.is_err());
        let pending = secrets
            .get(KEY_NAMESPACE, &rotation_handle(1))
            .await
            .unwrap()
            .unwrap();
        let generation = serde_json::from_slice::<Rotation>(&pending)
            .unwrap()
            .generation;
        store.write(1, id, good, true).await.unwrap();
        assert_eq!(store.get(1, &handle).await.unwrap().unwrap(), b"saved SQL");
        store.rotate(1).await.unwrap();
        assert_eq!(
            envelope(&store.read(1, id).await.unwrap().unwrap())
                .unwrap()
                .0,
            generation
        );
        assert!(secrets
            .get(KEY_NAMESPACE, &rotation_handle(1))
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn legacy_file_content_migrates_and_remains_encrypted_at_rest() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("chat-content");
        let secrets = Arc::new(MemorySecretStore::new());
        let store = AiContentStore::file(root.clone(), secrets.clone());
        let legacy_key = [42u8; 32];
        secrets
            .put(KEY_NAMESPACE, &key_handle(7, Uuid::nil()), &legacy_key)
            .await
            .unwrap();
        let id = Uuid::new_v4();
        let nonce = [5u8; 12];
        let mut sealed = nonce.to_vec();
        sealed.extend(
            ChaCha20Poly1305::new(Key::from_slice(&legacy_key))
                .encrypt(Nonce::from_slice(&nonce), b"legacy private SQL".as_slice())
                .unwrap(),
        );
        store.write(7, id, sealed, false).await.unwrap();
        assert_eq!(
            store.get(7, &id.to_string()).await.unwrap().unwrap(),
            b"legacy private SQL"
        );
        store.rotate(7).await.unwrap();
        let sealed = std::fs::read(blob_path(&root, 7, id)).unwrap();
        assert!(sealed.starts_with(MAGIC));
        assert!(!sealed
            .windows(18)
            .any(|window| window == b"legacy private SQL"));
        let keys = store
            .recovery_key_handles(7, &[id.to_string()])
            .await
            .unwrap();
        assert!(keys.contains(&active_handle(7)));
        store.delete(7, &id.to_string()).await.unwrap();
        assert!(!blob_path(&root, 7, id).exists());
    }
}
