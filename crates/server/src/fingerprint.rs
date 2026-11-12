//! Coarse SQL grouping for audits (ADR-009): normalize whitespace/case and
//! hash with SHA-256. This is not literal-stripping normalization.

use sha2::{Digest, Sha256};

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
