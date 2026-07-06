use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::Result;

#[async_trait]
pub trait SecretStore: Send + Sync {
    async fn put(&self, namespace: &str, handle: &str, secret: &[u8]) -> Result<()>;
    async fn get(&self, namespace: &str, handle: &str) -> Result<Option<Vec<u8>>>;
    async fn delete(&self, namespace: &str, handle: &str) -> Result<()>;
}

#[derive(Debug, Default)]
pub struct MemorySecretStore {
    inner: Mutex<HashMap<(String, String), Vec<u8>>>,
}

impl MemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SecretStore for MemorySecretStore {
    async fn put(&self, namespace: &str, handle: &str, secret: &[u8]) -> Result<()> {
        self.inner
            .lock()
            .unwrap()
            .insert((namespace.to_string(), handle.to_string()), secret.to_vec());
        Ok(())
    }

    async fn get(&self, namespace: &str, handle: &str) -> Result<Option<Vec<u8>>> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .get(&(namespace.to_string(), handle.to_string()))
            .cloned())
    }

    async fn delete(&self, namespace: &str, handle: &str) -> Result<()> {
        self.inner
            .lock()
            .unwrap()
            .remove(&(namespace.to_string(), handle.to_string()));
        Ok(())
    }
}
