//! Explicit main-only object maintenance; no unqualified/collation target.
use sift_protocol::SqliteMaintenanceAction as Action;

/// Keep bounded native definition snapshots out of retained preview leases.
pub fn definition_identity(snapshot: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(snapshot.as_bytes()))
}

pub fn sql(action: &Action) -> Result<Option<String>, &'static str> {
    let (verb, name) = match action {
        Action::Analyze { table } => ("ANALYZE", table),
        Action::Reindex { index } => ("REINDEX", index),
        _ => return Ok(None),
    };
    if name.is_empty() || name.len() > 255 || name.chars().any(char::is_control) {
        return Err("SQLite maintenance targets require 1..255 bytes without controls");
    }
    Ok(Some(format!(
        "{verb} \"main\".\"{}\"",
        name.replace('"', "\"\"")
    )))
}
