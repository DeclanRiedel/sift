//! Normalized SQL fingerprinting for the audit trail (ADR-009).
//!
//! A fingerprint is a coarse grouping key, not literal-stripping
//! normalization: it trims, collapses internal whitespace, and lowercases,
//! then hashes with SHA-256. It lets the audit trail correlate "the same
//! query" without persisting the query text or its bind values. The `sqlfp:`
//! prefix makes it recognizable in logs and audit rows.

use sha2::{Digest, Sha256};

/// Fingerprint of a SQL statement.
pub fn sql(statement: &str) -> String {
    let normalized = normalize(statement);
    let digest = Sha256::digest(normalized.as_bytes());
    let mut out = String::with_capacity(7 + 64);
    out.push_str("sqlfp:");
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn normalize(statement: &str) -> String {
    statement
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_and_normalized() {
        let a = sql("SELECT * FROM t  WHERE id = 1");
        let b = sql("select   *\nfrom t where id = 1");
        assert_eq!(
            a, b,
            "whitespace + case should normalize to the same fingerprint"
        );
        assert!(a.starts_with("sqlfp:"));
    }

    #[test]
    fn differs_by_content() {
        assert_ne!(sql("select 1"), sql("select 2"));
    }

    #[test]
    fn carries_no_raw_text() {
        let fp = sql("select secret_column from users where token = 'hunter2'");
        assert!(!fp.contains("hunter2"));
        assert!(!fp.contains("secret_column"));
    }
}
