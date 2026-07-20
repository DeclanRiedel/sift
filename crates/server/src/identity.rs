//! Server-owned password policy and verifier work. Plaintext passwords enter
//! this module briefly and are never included in errors or debug output.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use rand_core::OsRng;
use sift_metadata::{MetadataStore, PasswordIdentity};
use tokio::sync::Semaphore;

const MIN_PASSWORD_BYTES: usize = 12;
const MAX_PASSWORD_BYTES: usize = 1024;
const LOGIN_WINDOW: Duration = Duration::from_secs(60);
const MAX_SOURCE_FAILURES: u32 = 20;
const MAX_IDENTITY_FAILURES: u32 = 5;
const MAX_ARGON2_CONCURRENCY: usize = 4;

#[derive(Clone)]
pub struct AuthRuntime {
    verifier_slots: Arc<Semaphore>,
    failures: Arc<Mutex<HashMap<String, FailureWindow>>>,
}

impl Default for AuthRuntime {
    fn default() -> Self {
        // Initialize once at process startup so unknown-user verification uses
        // a real policy-cost Argon2id hash rather than a cheap parse failure.
        let _ = dummy_verifier();
        Self {
            verifier_slots: Arc::new(Semaphore::new(MAX_ARGON2_CONCURRENCY)),
            failures: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

struct FailureWindow {
    started: Instant,
    count: u32,
}

pub enum PasswordAuthOutcome {
    Authenticated(Box<PasswordIdentity>),
    Denied,
    Throttled,
}

impl AuthRuntime {
    pub async fn authenticate_password(
        &self,
        metadata: &MetadataStore,
        source: &str,
        supplied_username: &str,
        password: Vec<u8>,
    ) -> anyhow::Result<PasswordAuthOutcome> {
        let username = normalize_username(supplied_username)
            .unwrap_or_else(|_| "invalid-login-placeholder".to_string());
        let source_key = format!("source:{source}");
        let identity_key = format!("identity:{username}");
        if self.is_limited(&source_key, MAX_SOURCE_FAILURES)
            || self.is_limited(&identity_key, MAX_IDENTITY_FAILURES)
        {
            return Ok(PasswordAuthOutcome::Throttled);
        }
        let permit = match Arc::clone(&self.verifier_slots).try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => return Ok(PasswordAuthOutcome::Throttled),
        };
        let identity = metadata.resolve_password_identity(&username)?;
        let verifier = match &identity {
            Some(identity) => metadata
                .password_verifier(&identity.identity)
                .await?
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .unwrap_or_else(|| dummy_verifier().clone()),
            None => dummy_verifier().clone(),
        };
        let verified = verify_password(password, verifier).await?;
        drop(permit);
        let enabled = identity.as_ref().is_some_and(|identity| {
            identity.principal.disabled_at.is_none() && identity.identity.disabled_at.is_none()
        });
        if verified && enabled {
            self.clear_failure(&identity_key);
            return Ok(PasswordAuthOutcome::Authenticated(Box::new(
                identity.expect("enabled identity is present"),
            )));
        }
        self.record_failure(source_key);
        self.record_failure(identity_key);
        Ok(PasswordAuthOutcome::Denied)
    }

    fn is_limited(&self, key: &str, limit: u32) -> bool {
        let now = Instant::now();
        let mut failures = self.failures.lock().unwrap();
        failures.retain(|_, window| now.duration_since(window.started) < LOGIN_WINDOW);
        failures
            .get(key)
            .is_some_and(|window| window.count >= limit)
    }

    fn record_failure(&self, key: String) {
        let now = Instant::now();
        let mut failures = self.failures.lock().unwrap();
        let window = failures.entry(key).or_insert(FailureWindow {
            started: now,
            count: 0,
        });
        if now.duration_since(window.started) >= LOGIN_WINDOW {
            window.started = now;
            window.count = 0;
        }
        window.count = window.count.saturating_add(1);
    }

    fn clear_failure(&self, key: &str) {
        self.failures.lock().unwrap().remove(key);
    }
}

pub fn normalize_username(value: &str) -> anyhow::Result<String> {
    let normalized = value.trim().to_ascii_lowercase();
    if !(3..=64).contains(&normalized.len()) {
        bail!("username must be between 3 and 64 characters");
    }
    if !normalized
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        bail!("username may contain only ASCII letters, digits, '.', '-' and '_'");
    }
    Ok(normalized)
}

pub fn validate_password(password: &[u8]) -> anyhow::Result<()> {
    if password.len() < MIN_PASSWORD_BYTES {
        bail!("password must be at least {MIN_PASSWORD_BYTES} bytes");
    }
    if password.len() > MAX_PASSWORD_BYTES {
        bail!("password must be at most {MAX_PASSWORD_BYTES} bytes");
    }
    let lowercase = String::from_utf8_lossy(password).to_ascii_lowercase();
    const BLOCKED: &[&str] = &[
        "password1234",
        "password12345",
        "123456789012",
        "qwertyuiop12",
        "letmein12345",
    ];
    if BLOCKED.contains(&lowercase.as_str()) {
        bail!("password appears in the built-in common-password blocklist");
    }
    Ok(())
}

pub async fn hash_password(password: Vec<u8>) -> anyhow::Result<String> {
    validate_password(&password)?;
    tokio::task::spawn_blocking(move || {
        let salt = SaltString::generate(&mut OsRng);
        let params = Params::new(19_456, 2, 1, None)
            .map_err(|error| anyhow::anyhow!("invalid Argon2 policy: {error}"))?;
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password(&password, &salt)
            .map(|hash| hash.to_string())
            .map_err(|error| anyhow::anyhow!("password hashing failed: {error}"))
    })
    .await
    .context("password hashing worker failed")?
}

async fn verify_password(password: Vec<u8>, verifier: String) -> anyhow::Result<bool> {
    tokio::task::spawn_blocking(move || {
        let parsed = PasswordHash::new(&verifier)
            .map_err(|error| anyhow::anyhow!("stored password verifier is invalid: {error}"))?;
        Ok(Argon2::default()
            .verify_password(&password, &parsed)
            .is_ok())
    })
    .await
    .context("password verification worker failed")?
}

fn dummy_verifier() -> &'static String {
    static VERIFIER: OnceLock<String> = OnceLock::new();
    VERIFIER.get_or_init(|| {
        let salt = SaltString::generate(&mut OsRng);
        let params = Params::new(19_456, 2, 1, None).expect("static Argon2 policy is valid");
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password(b"sift dummy password verification", &salt)
            .expect("static dummy password hashes")
            .to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_metadata::{MemorySecretStore, NewOperationAudit, NewPasswordPrincipal};

    #[test]
    fn usernames_are_normalized_and_bounded() {
        assert_eq!(normalize_username(" Alice.Smith ").unwrap(), "alice.smith");
        assert!(normalize_username("ab").is_err());
        assert!(normalize_username("not allowed").is_err());
    }

    #[tokio::test]
    async fn password_verifier_is_argon2id_and_contains_no_plaintext() {
        let plaintext = b"correct horse battery staple".to_vec();
        let verifier = hash_password(plaintext.clone()).await.unwrap();
        assert!(verifier.starts_with("$argon2id$"));
        assert!(!verifier.contains(&String::from_utf8(plaintext).unwrap()));
        assert!(hash_password(b"password1234".to_vec()).await.is_err());
    }

    #[tokio::test]
    async fn authentication_is_generic_and_identity_throttled() {
        let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        let plaintext = b"correct horse battery staple".to_vec();
        let verifier = hash_password(plaintext.clone()).await.unwrap();
        metadata
            .create_password_principal(
                NewPasswordPrincipal {
                    username: "alice",
                    display_name: "Alice",
                    email: None,
                    is_instance_admin: false,
                },
                verifier.as_bytes(),
                NewOperationAudit {
                    actor_principal_id: None,
                    action: "create".into(),
                    target: "principal".into(),
                    target_id: None,
                    status: "succeeded".into(),
                    result_code: None,
                    row_count: None,
                    error_message: None,
                    correlation_id: None,
                },
            )
            .await
            .unwrap();
        let runtime = AuthRuntime::default();
        assert!(matches!(
            runtime
                .authenticate_password(&metadata, "127.0.0.1", "ALICE", plaintext)
                .await
                .unwrap(),
            PasswordAuthOutcome::Authenticated(_)
        ));
        for _ in 0..MAX_IDENTITY_FAILURES {
            assert!(matches!(
                runtime
                    .authenticate_password(
                        &metadata,
                        "127.0.0.2",
                        "missing-user",
                        b"some incorrect password".to_vec()
                    )
                    .await
                    .unwrap(),
                PasswordAuthOutcome::Denied
            ));
        }
        assert!(matches!(
            runtime
                .authenticate_password(
                    &metadata,
                    "127.0.0.3",
                    "missing-user",
                    b"some incorrect password".to_vec()
                )
                .await
                .unwrap(),
            PasswordAuthOutcome::Throttled
        ));
    }
}
