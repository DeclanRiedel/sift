//! Server-internal workspace projection adapters.
//!
//! Public APIs never receive server paths. The bundled filesystem adapter is
//! constructed from operator-owned roots and performs every traversal through
//! an already-open capability directory.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;

use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt as _};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
use sha2::{Digest, Sha256};
use sift_protocol::WorkspacePath;

use crate::config::WorkspaceProjectionConfig;

pub const FILESYSTEM_ADAPTER_ID: &str = "sift/filesystem";
pub const FILESYSTEM_ADAPTER_GENERATION: &str = "filesystem-v1";
pub const MAX_PROJECTION_ENTRIES: usize = 20_000;
pub const MAX_PROJECTION_FILE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_PROJECTION_SCAN_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceAdapterError {
    #[error("workspace filesystem projections are disabled")]
    Disabled,
    #[error("workspace root handle is unavailable")]
    RootUnavailable,
    #[error("workspace root is read-only")]
    ReadOnly,
    #[error("projection contains an invalid path")]
    InvalidPath,
    #[error("projection contains a symlink, special file, or hard-link alias")]
    UnsafeFile,
    #[error("projection exceeds its entry or byte ceiling")]
    LimitExceeded,
    #[error("projection filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, WorkspaceAdapterError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionFile {
    pub path: WorkspacePath,
    pub digest: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProjectionSnapshot {
    pub files: Vec<ProjectionFile>,
    pub total_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializeFile {
    pub path: WorkspacePath,
    pub bytes: Vec<u8>,
}

pub trait WorkspaceAdapter: Send + Sync {
    fn adapter_id(&self) -> &'static str;
    fn generation(&self) -> &'static str;
    fn scan(&self, root_handle: &str) -> Result<ProjectionSnapshot>;
    fn materialize(&self, root_handle: &str, files: &[MaterializeFile]) -> Result<()>;
    fn remove(&self, root_handle: &str, paths: &[WorkspacePath]) -> Result<()>;
}

struct ConfiguredRoot {
    directory: Arc<Dir>,
    read_only: bool,
    #[allow(dead_code)]
    canonical_path: PathBuf,
}

pub struct RootedFilesystemAdapter {
    roots: HashMap<String, ConfiguredRoot>,
}

impl RootedFilesystemAdapter {
    pub fn from_config(config: &WorkspaceProjectionConfig) -> Result<Option<Self>> {
        if !config.enabled {
            return Ok(None);
        }
        let mut roots = HashMap::new();
        for root in &config.roots {
            let canonical_path = std::fs::canonicalize(&root.path)
                .map_err(|_| WorkspaceAdapterError::RootUnavailable)?;
            let directory = Dir::open_ambient_dir(&canonical_path, ambient_authority())
                .map_err(|_| WorkspaceAdapterError::RootUnavailable)?;
            roots.insert(
                root.handle.clone(),
                ConfiguredRoot {
                    directory: Arc::new(directory),
                    read_only: root.read_only,
                    canonical_path,
                },
            );
        }
        Ok(Some(Self { roots }))
    }

    fn root(&self, handle: &str, writable: bool) -> Result<&ConfiguredRoot> {
        let root = self
            .roots
            .get(handle)
            .ok_or(WorkspaceAdapterError::RootUnavailable)?;
        if writable && root.read_only {
            return Err(WorkspaceAdapterError::ReadOnly);
        }
        Ok(root)
    }

    pub fn validate_binding(&self, handle: &str, writable: bool) -> Result<()> {
        self.root(handle, writable).map(|_| ())
    }

    pub(crate) fn canonical_root_path(&self, handle: &str) -> Result<PathBuf> {
        Ok(self.root(handle, false)?.canonical_path.clone())
    }
}

impl WorkspaceAdapter for RootedFilesystemAdapter {
    fn adapter_id(&self) -> &'static str {
        FILESYSTEM_ADAPTER_ID
    }

    fn generation(&self) -> &'static str {
        FILESYSTEM_ADAPTER_GENERATION
    }

    fn scan(&self, root_handle: &str) -> Result<ProjectionSnapshot> {
        let root = self.root(root_handle, false)?;
        let mut snapshot = ProjectionSnapshot::default();
        scan_directory(&root.directory, "", &mut snapshot)?;
        snapshot
            .files
            .sort_by(|left, right| left.path.0.cmp(&right.path.0));
        Ok(snapshot)
    }

    fn materialize(&self, root_handle: &str, files: &[MaterializeFile]) -> Result<()> {
        let root = self.root(root_handle, true)?;
        if files.len() > MAX_PROJECTION_ENTRIES {
            return Err(WorkspaceAdapterError::LimitExceeded);
        }
        for file in files {
            validate_path(&file.path)?;
            if file.bytes.len() > MAX_PROJECTION_FILE_BYTES {
                return Err(WorkspaceAdapterError::LimitExceeded);
            }
            let (parent, name) = split_parent(&file.path)?;
            let directory = open_or_create_directory(&root.directory, parent)?;
            reject_unsafe_existing(&directory, name)?;
            let temporary = format!(".sift-stage-{}", uuid::Uuid::new_v4());
            let mut options = OpenOptions::new();
            options
                .write(true)
                .create_new(true)
                .follow(FollowSymlinks::No);
            let mut staged = directory.open_with(&temporary, &options)?;
            staged.write_all(&file.bytes)?;
            staged.sync_all()?;
            directory.rename(&temporary, &directory, name)?;
        }
        Ok(())
    }

    fn remove(&self, root_handle: &str, paths: &[WorkspacePath]) -> Result<()> {
        let root = self.root(root_handle, true)?;
        for path in paths {
            validate_path(path)?;
            let (parent, name) = split_parent(path)?;
            let directory = open_existing_directory(&root.directory, parent)?;
            let metadata = directory.symlink_metadata(name)?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(WorkspaceAdapterError::UnsafeFile);
            }
            directory.remove_file(name)?;
        }
        Ok(())
    }
}

fn scan_directory(directory: &Dir, prefix: &str, snapshot: &mut ProjectionSnapshot) -> Result<()> {
    let mut entries = directory.entries()?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| WorkspaceAdapterError::InvalidPath)?;
        if prefix.is_empty() && name == ".git" {
            continue;
        }
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(WorkspaceAdapterError::UnsafeFile);
        }
        if file_type.is_dir() {
            let child = directory.open_dir(&name)?;
            scan_directory(&child, &path, snapshot)?;
            continue;
        }
        if !file_type.is_file() {
            return Err(WorkspaceAdapterError::UnsafeFile);
        }
        if snapshot.files.len() >= MAX_PROJECTION_ENTRIES {
            return Err(WorkspaceAdapterError::LimitExceeded);
        }
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        let mut file = directory.open_with(&name, &options)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || hard_linked(&metadata) {
            return Err(WorkspaceAdapterError::UnsafeFile);
        }
        let limit = u64::try_from(MAX_PROJECTION_FILE_BYTES + 1)
            .map_err(|_| WorkspaceAdapterError::LimitExceeded)?;
        let mut bytes = Vec::new();
        std::io::Read::by_ref(&mut file)
            .take(limit)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_PROJECTION_FILE_BYTES {
            return Err(WorkspaceAdapterError::LimitExceeded);
        }
        snapshot.total_bytes = snapshot
            .total_bytes
            .checked_add(bytes.len())
            .ok_or(WorkspaceAdapterError::LimitExceeded)?;
        if snapshot.total_bytes > MAX_PROJECTION_SCAN_BYTES {
            return Err(WorkspaceAdapterError::LimitExceeded);
        }
        snapshot.files.push(ProjectionFile {
            path: WorkspacePath::new(path).map_err(|_| WorkspaceAdapterError::InvalidPath)?,
            digest: digest(&bytes),
            bytes,
        });
    }
    Ok(())
}

fn validate_path(path: &WorkspacePath) -> Result<()> {
    if path.is_valid() {
        Ok(())
    } else {
        Err(WorkspaceAdapterError::InvalidPath)
    }
}

fn split_parent(path: &WorkspacePath) -> Result<(&str, &str)> {
    validate_path(path)?;
    Ok(path.0.rsplit_once('/').unwrap_or(("", path.0.as_str())))
}

fn open_existing_directory(root: &Dir, relative: &str) -> Result<Dir> {
    let mut current = root.try_clone()?;
    if relative.is_empty() {
        return Ok(current);
    }
    for component in relative.split('/') {
        let metadata = current.symlink_metadata(component)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(WorkspaceAdapterError::UnsafeFile);
        }
        current = current.open_dir(component)?;
    }
    Ok(current)
}

fn open_or_create_directory(root: &Dir, relative: &str) -> Result<Dir> {
    let mut current = root.try_clone()?;
    if relative.is_empty() {
        return Ok(current);
    }
    for component in relative.split('/') {
        match current.symlink_metadata(component) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(WorkspaceAdapterError::UnsafeFile);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                current.create_dir(component)?;
            }
            Err(error) => return Err(error.into()),
        }
        current = current.open_dir(component)?;
    }
    Ok(current)
}

fn reject_unsafe_existing(directory: &Dir, name: &str) -> Result<()> {
    match directory.symlink_metadata(name) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() || hard_linked(&metadata) {
                Err(WorkspaceAdapterError::UnsafeFile)
            } else {
                Ok(())
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
fn hard_linked(metadata: &cap_std::fs::Metadata) -> bool {
    use cap_std::fs::MetadataExt as _;
    metadata.nlink() > 1
}

#[cfg(not(unix))]
fn hard_linked(_metadata: &cap_std::fs::Metadata) -> bool {
    false
}

fn digest(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn adapter(path: &Path, read_only: bool) -> RootedFilesystemAdapter {
        RootedFilesystemAdapter::from_config(&WorkspaceProjectionConfig {
            enabled: true,
            roots: vec![crate::config::WorkspaceRootConfig {
                handle: "test".into(),
                path: path.display().to_string(),
                read_only,
            }],
        })
        .unwrap()
        .unwrap()
    }

    #[test]
    fn scan_and_materialize_are_deterministic_and_root_confined() {
        let directory = tempfile::tempdir().unwrap();
        let adapter = adapter(directory.path(), false);
        adapter
            .materialize(
                "test",
                &[MaterializeFile {
                    path: WorkspacePath::new("ddl/users.sql").unwrap(),
                    bytes: b"create table users(id int);".to_vec(),
                }],
            )
            .unwrap();
        let first = adapter.scan("test").unwrap();
        let second = adapter.scan("test").unwrap();
        assert_eq!(first, second);
        assert_eq!(first.files[0].path.0, "ddl/users.sql");
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_and_hard_links_fail_closed() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.sql"), "secret").unwrap();
        symlink(
            outside.path().join("secret.sql"),
            directory.path().join("escape.sql"),
        )
        .unwrap();
        let adapter = adapter(directory.path(), false);
        assert!(matches!(
            adapter.scan("test"),
            Err(WorkspaceAdapterError::UnsafeFile)
        ));

        std::fs::remove_file(directory.path().join("escape.sql")).unwrap();
        std::fs::write(directory.path().join("one.sql"), "select 1").unwrap();
        std::fs::hard_link(
            directory.path().join("one.sql"),
            directory.path().join("two.sql"),
        )
        .unwrap();
        assert!(matches!(
            adapter.scan("test"),
            Err(WorkspaceAdapterError::UnsafeFile)
        ));
    }

    #[test]
    fn read_only_roots_reject_materialization() {
        let directory = tempfile::tempdir().unwrap();
        let adapter = adapter(directory.path(), true);
        assert!(matches!(
            adapter.materialize("test", &[]),
            Err(WorkspaceAdapterError::ReadOnly)
        ));
    }
}
