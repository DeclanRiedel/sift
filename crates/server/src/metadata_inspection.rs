//! Local-owner, allowlisted snapshots. Never exposes the live metadata file to SQL.
use std::path::Path;

use anyhow::{bail, Context};
use rusqlite::{Connection, OpenFlags};

// Deliberately omit authentication, credentials, configuration, SQL text, CRDT
// blobs, history, audit bodies, vaults and repository contents. New tables and
// columns stay excluded until explicitly reviewed here.
const TABLES: &[(&str, &[&str])] = &[
    (
        "tenant",
        &["id", "name", "kind", "created_at", "updated_at"],
    ),
    (
        "principal",
        &["id", "display_name", "created_at", "updated_at"],
    ),
    ("membership", &["tenant_id", "principal_id", "role"]),
    (
        "connection_profile",
        &[
            "id",
            "tenant_id",
            "name",
            "engine",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "room",
        &[
            "id",
            "tenant_id",
            "name",
            "kind",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "workspace",
        &[
            "id",
            "room_id",
            "name",
            "revision",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "workspace_node",
        &[
            "id",
            "workspace_id",
            "parent_id",
            "path",
            "kind",
            "document_id",
            "revision",
        ],
    ),
];

pub fn require_local_owner(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::symlink_metadata(path)?;
        // SAFETY: geteuid has no preconditions or memory access.
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            bail!("metadata inspection requires a regular file owned by the current local user");
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        bail!("local-owner metadata inspection is currently supported on Unix only")
    }
}

/// The destination must not exist and must live in an owner-private directory.
/// A read transaction includes committed WAL data without changing the source.
pub fn snapshot(source: &Path, destination: &Path) -> anyhow::Result<u64> {
    require_local_owner(source)?;
    let source = Connection::open_with_flags(
        source,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    source.busy_timeout(std::time::Duration::from_secs(1))?;
    source.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH, 1024 * 1024);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    source.progress_handler(1000, Some(move || std::time::Instant::now() >= deadline));
    source.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA query_only=ON; BEGIN")?;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    drop(file);
    let result = (|| {
        let destination =
            Connection::open_with_flags(destination, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        destination.execute_batch("BEGIN; CREATE TABLE inspection_info(exported_at TEXT, operation TEXT, note TEXT); CREATE TABLE inspection_tables(name TEXT PRIMARY KEY, rows_copied INTEGER, truncated INTEGER)")?;
        destination.execute("INSERT INTO inspection_info VALUES(datetime('now'),?1,?2)", rusqlite::params![serde_json::to_string(&sift_protocol::Operation::InspectMetadata)?, "Allowlisted metadata snapshot; no credentials, authentication data, SQL text or document contents. Each table is capped at 10000 rows; the export is capped at 32 MiB of values."])?;
        let mut total = 0u64;
        let mut bytes = 0usize;
        for (table, columns) in TABLES {
            // Refuse views: source-defined SQL is never copied or executed.
            let exists: bool = source.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name=?1 AND type='table')",
                [table],
                |row| row.get(0),
            )?;
            if !exists {
                continue;
            }
            let names = columns
                .iter()
                .map(|name| format!("\"{name}\""))
                .collect::<Vec<_>>()
                .join(",");
            let mut select = source
                .prepare(&format!("SELECT {names} FROM \"{table}\" LIMIT 10001"))
                .context("reading supported Sift metadata columns")?;
            destination.execute_batch(&format!("CREATE TABLE \"{table}\"({names})"))?;
            let slots = vec!["?"; columns.len()].join(",");
            let mut insert =
                destination.prepare(&format!("INSERT INTO \"{table}\" VALUES({slots})"))?;
            let mut rows = select.query([])?;
            let mut copied = 0u64;
            let mut truncated = false;
            while let Some(row) = rows.next()? {
                if copied == 10_000 {
                    truncated = true;
                    break;
                }
                let values = (0..columns.len())
                    .map(|i| row.get::<_, rusqlite::types::Value>(i))
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                for value in &values {
                    bytes += match value {
                        rusqlite::types::Value::Text(v) => v.len(),
                        rusqlite::types::Value::Blob(v) => v.len(),
                        _ => 8,
                    };
                }
                if bytes > 32 * 1024 * 1024 {
                    bail!("metadata inspection exceeds the 32 MiB snapshot limit");
                }
                insert.execute(rusqlite::params_from_iter(values))?;
                copied += 1;
            }
            destination.execute(
                "INSERT INTO inspection_tables VALUES(?1,?2,?3)",
                rusqlite::params![table, copied, truncated],
            )?;
            total += copied;
        }
        destination.execute_batch("COMMIT")?;
        Ok(total)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(destination);
    }
    result
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn snapshot_includes_wal_and_excludes_credentials_without_changing_source() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.db");
        let db = Connection::open(&source).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE tenant(id,name,kind,created_at,updated_at); INSERT INTO tenant VALUES(1,'demo','personal','now','now'); CREATE TABLE api_token(token_hash); INSERT INTO api_token VALUES('must-never-export');").unwrap();
        let destination = directory.path().join("inspection.db");
        assert_eq!(snapshot(&source, &destination).unwrap(), 1);
        let copy = Connection::open(&destination).unwrap();
        assert_eq!(
            copy.query_row("SELECT name FROM tenant", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "demo"
        );
        assert!(copy.prepare("SELECT * FROM api_token").is_err());
        assert!(snapshot(&source, &destination).is_err());
        assert_eq!(
            db.query_row("SELECT token_hash FROM api_token", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "must-never-export"
        );
    }
}
