//! Server-owned password policy and verifier work. Plaintext passwords enter
//! this module briefly and are never included in errors or debug output.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use rand_core::OsRng;
use sha2::{Digest, Sha256};
use sift_metadata::{AuthenticatedSession, MetadataStore, PasswordIdentity, PrincipalId};
use tokio::sync::Semaphore;

const MIN_PASSWORD_BYTES: usize = 12;
const MAX_PASSWORD_BYTES: usize = 1024;
const LOGIN_WINDOW: Duration = Duration::from_secs(60);
const MAX_SOURCE_FAILURES: u32 = 20;
const MAX_IDENTITY_FAILURES: u32 = 5;
const MAX_ARGON2_CONCURRENCY: usize = 4;
const ACCESS_CACHE_TTL: Duration = Duration::from_secs(30);
const ACCESS_CACHE_MAX_ENTRIES: usize = 1024;

#[derive(Clone)]
pub struct AuthRuntime {
    verifier_slots: Arc<Semaphore>,
    failures: Arc<Mutex<HashMap<String, FailureWindow>>>,
    access_cache: Arc<Mutex<HashMap<[u8; 32], CachedAccessSession>>>,
}

#[derive(Clone)]
pub struct GithubOAuthConfig {
    pub client_id: String,
    pub client_secret: String,
    pub public_base_url: String,
    pub http: reqwest::Client,
}

pub fn normalize_github_login(value: &str) -> anyhow::Result<String> {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized.is_empty() || normalized.len() > 39 {
        bail!("GitHub login must be between 1 and 39 characters");
    }
    if normalized.starts_with('-')
        || normalized.ends_with('-')
        || !normalized
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        bail!("GitHub login must contain only letters, digits, or interior hyphens");
    }
    Ok(normalized)
}

impl Default for AuthRuntime {
    fn default() -> Self {
        // Initialize once at process startup so unknown-user verification uses
        // a real policy-cost Argon2id hash rather than a cheap parse failure.
        let _ = dummy_verifier();
        Self {
            verifier_slots: Arc::new(Semaphore::new(MAX_ARGON2_CONCURRENCY)),
            failures: Arc::new(Mutex::new(HashMap::new())),
            access_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[derive(Clone)]
struct CachedAccessSession {
    session: AuthenticatedSession,
    cached_at: Instant,
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
    pub async fn resolve_access_token(
        &self,
        metadata: &MetadataStore,
        presented: &str,
    ) -> sift_metadata::Result<Option<AuthenticatedSession>> {
        let key = access_cache_key(presented);
        let now = Instant::now();
        let utc_now = chrono::Utc::now();
        {
            let mut cache = self.access_cache.lock().unwrap();
            cache.retain(|_, entry| {
                now.duration_since(entry.cached_at) < ACCESS_CACHE_TTL
                    && entry.session.expires_at > utc_now
            });
            if let Some(entry) = cache.get(&key) {
                return Ok(Some(entry.session.clone()));
            }
        }

        let session = metadata.verify_auth_access_token(presented).await?;
        if let Some(session) = &session {
            let mut cache = self.access_cache.lock().unwrap();
            if cache.len() >= ACCESS_CACHE_MAX_ENTRIES {
                if let Some(oldest) = cache
                    .iter()
                    .max_by_key(|(_, entry)| now.duration_since(entry.cached_at))
                    .map(|(key, _)| *key)
                {
                    cache.remove(&oldest);
                }
            }
            cache.insert(
                key,
                CachedAccessSession {
                    session: session.clone(),
                    cached_at: now,
                },
            );
        }
        Ok(session)
    }

    pub fn invalidate_auth_session(&self, session_id: &str) {
        self.access_cache
            .lock()
            .unwrap()
            .retain(|_, entry| entry.session.session_id != session_id);
    }

    pub fn invalidate_principal(&self, principal: PrincipalId) {
        self.access_cache
            .lock()
            .unwrap()
            .retain(|_, entry| entry.session.principal.id != principal);
    }

    pub fn invalidate_all_access_tokens(&self) {
        self.access_cache.lock().unwrap().clear();
    }

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

fn access_cache_key(presented: &str) -> [u8; 32] {
    Sha256::digest(presented.as_bytes()).into()
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
        assert_eq!(normalize_github_login(" Octo-Cat ").unwrap(), "octo-cat");
        assert!(normalize_github_login("-octocat").is_err());
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

    #[tokio::test]
    async fn access_cache_is_bounded_by_local_revocation_invalidation() {
        let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        let verifier = hash_password(b"correct horse battery staple".to_vec())
            .await
            .unwrap();
        let principal = metadata
            .create_password_principal(
                NewPasswordPrincipal {
                    username: "cache-user",
                    display_name: "Cache User",
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
        let tokens = metadata
            .issue_auth_session(
                principal.id,
                sift_metadata::AuthClientKind::Native,
                Some("cache test"),
                NewOperationAudit {
                    actor_principal_id: Some(principal.id),
                    action: "authenticate".into(),
                    target: "auth_session".into(),
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
        assert!(runtime
            .resolve_access_token(&metadata, &tokens.access_token)
            .await
            .unwrap()
            .is_some());

        metadata
            .revoke_auth_session(
                &tokens.session_id,
                "test",
                NewOperationAudit {
                    actor_principal_id: Some(principal.id),
                    action: "logout".into(),
                    target: "auth_session".into(),
                    target_id: None,
                    status: "succeeded".into(),
                    result_code: None,
                    row_count: None,
                    error_message: None,
                    correlation_id: None,
                },
            )
            .unwrap();
        runtime.invalidate_auth_session(&tokens.session_id);
        assert!(runtime
            .resolve_access_token(&metadata, &tokens.access_token)
            .await
            .unwrap()
            .is_none());
    }
}
