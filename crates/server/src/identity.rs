//! Server-owned password policy and verifier work. Plaintext passwords enter
//! this module briefly and are never included in errors or debug output.

use anyhow::{bail, Context};
use argon2::password_hash::{PasswordHasher, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use rand_core::OsRng;

const MIN_PASSWORD_BYTES: usize = 12;
const MAX_PASSWORD_BYTES: usize = 1024;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
