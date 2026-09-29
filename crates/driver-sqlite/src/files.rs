//! Server-owned roots are an access boundary, not a sandbox against a local
//! process that can replace the operator-owned directory during open.
use super::error;
use sift_driver_api::SqliteFileMaintenanceState;
use sift_protocol::*;
use std::path::{Component, Path, PathBuf};

const MAX_MAINTENANCE_FILE_BYTES: u64 = 1024 * 1024 * 1024;

pub(crate) struct MaintenanceTarget {
    pub state: SqliteFileMaintenanceState,
    pub destination: Option<PathBuf>,
}

#[derive(Clone, Default)]
pub struct FilePolicy {
    pub config: SqliteDriverConfig,
    pub protected: Vec<PathBuf>,
}
impl FilePolicy {
    pub(crate) fn inspect_maintenance(
        &self,
        source: &Path,
        source_read_only: bool,
        tenant: i64,
        action: &SqliteMaintenanceAction,
    ) -> Result<MaintenanceTarget, DriverError> {
        if !cfg!(unix) {
            return Err(error(
                Code::UnsupportedForEngine,
                "SQLite file maintenance is supported on Unix only",
            ));
        }
        let matching = self
            .config
            .roots
            .iter()
            .filter_map(|(id, root)| {
                source
                    .strip_prefix(&root.path)
                    .ok()
                    .filter(|relative| {
                        !relative.as_os_str().is_empty()
                            && relative
                                .components()
                                .all(|part| matches!(part, Component::Normal(_)))
                    })
                    .map(|relative| (id, root, relative))
            })
            .collect::<Vec<_>>();
        let [(root_id, root, relative)] = matching.as_slice() else {
            return Err(denied());
        };
        if source_read_only || root.read_only || !root.allowed_tenants.contains(&tenant) {
            return Err(denied());
        }
        let mut spec = SqliteConnectionSpec {
            file_path: source.to_string_lossy().into_owned(),
            read_only: false,
            busy_timeout_ms: 1000,
        };
        self.validate(&mut spec)?;
        let metadata = std::fs::metadata(source).map_err(|_| denied())?;
        if metadata.len() > MAX_MAINTENANCE_FILE_BYTES {
            return Err(error(
                Code::ResultTooLarge,
                "SQLite maintenance source exceeds 1 GiB",
            ));
        }
        let destination = match action {
            SqliteMaintenanceAction::Create { path } | SqliteMaintenanceAction::Backup { path } => {
                Some(self.inspect_new_file(root, source, path)?)
            }
        };
        let source_file = relative.to_string_lossy().into_owned();
        let destination_file = match action {
            SqliteMaintenanceAction::Create { path } | SqliteMaintenanceAction::Backup { path } => {
                Some(path.clone())
            }
        };
        let wal = PathBuf::from(format!("{}-wal", source.display()));
        let identity = format!(
            "{}|{}|{}|{}|{}|{}|{}|{:?}|{:?}",
            root_id,
            source_file,
            file_identity(&metadata),
            std::fs::metadata(&wal)
                .ok()
                .as_ref()
                .map(file_identity)
                .unwrap_or_default(),
            destination_file.as_deref().unwrap_or_default(),
            root.read_only,
            tenant,
            action,
            destination
                .as_ref()
                .and_then(|path| path.parent())
                .and_then(|path| path.canonicalize().ok()),
        );
        Ok(MaintenanceTarget {
            state: SqliteFileMaintenanceState {
                root_id: (*root_id).clone(),
                source_file,
                destination_file,
                source_bytes: metadata.len(),
                identity,
            },
            destination,
        })
    }

    fn inspect_new_file(
        &self,
        root: &SqliteRootConfig,
        source: &Path,
        relative: &str,
    ) -> Result<PathBuf, DriverError> {
        let path = Path::new(relative);
        if relative.is_empty()
            || relative.len() > 512
            || relative.contains(['\0', ':', '\\'])
            || !path
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
        {
            return Err(denied());
        }
        let root_path = Path::new(&root.path);
        let destination = root_path.join(path);
        let source_family = ["", "-wal", "-shm", "-journal"].iter().any(|suffix| {
            let mut sibling = source.as_os_str().to_os_string();
            sibling.push(suffix);
            destination == PathBuf::from(sibling)
        });
        if source_family || std::fs::symlink_metadata(&destination).is_ok() {
            return Err(error(
                Code::InvalidParameterValue,
                "SQLite maintenance destination already exists",
            ));
        }
        let canonical_root = root_path.canonicalize().map_err(|_| denied())?;
        let parent = destination.parent().ok_or_else(denied)?;
        let canonical_parent = parent.canonicalize().map_err(|_| denied())?;
        if !canonical_parent.starts_with(&canonical_root) {
            return Err(denied());
        }
        let mut prefix = root_path.to_path_buf();
        for component in path
            .components()
            .take(path.components().count().saturating_sub(1))
        {
            prefix.push(component);
            if std::fs::symlink_metadata(&prefix)
                .map_err(|_| denied())?
                .file_type()
                .is_symlink()
            {
                return Err(denied());
            }
        }
        if self.protected.iter().any(|protected| {
            protected
                .canonicalize()
                .unwrap_or_else(|_| protected.clone())
                .starts_with(&destination)
                || destination.starts_with(protected)
        }) {
            return Err(denied());
        }
        Ok(destination)
    }
    pub fn resolve(
        &self,
        request: SqliteFileConfiguration,
        tenant: Option<i64>,
    ) -> Result<ConnectionSpec, DriverError> {
        let root = self.config.roots.get(&request.root_id).ok_or_else(denied)?;
        if !tenant.is_some_and(|id| root.allowed_tenants.contains(&id)) {
            return Err(denied());
        }
        if request.busy_timeout_ms > 5000 {
            return Err(error(
                Code::InvalidParameterValue,
                "SQLite busy timeout must be 0..5000ms",
            ));
        }
        let path = Path::new(&request.path);
        if request.path.is_empty()
            || request.path.contains(['\0', ':', '\\'])
            || !path.components().all(|c| matches!(c, Component::Normal(_)))
        {
            return Err(denied());
        }
        let full = Path::new(&root.path).join(path);
        Ok(ConnectionSpec {
            host: String::new(),
            port: None,
            database: Some(format!("{}/{}", request.root_id, request.path)),
            user: String::new(),
            password: None,
            ssl_mode: None,
            engine_specific: Some(EngineConnectionSpec::Sqlite(SqliteConnectionSpec {
                file_path: full.to_string_lossy().into_owned(),
                read_only: root.read_only || request.mode == SqliteOpenMode::ReadOnly,
                busy_timeout_ms: request.busy_timeout_ms,
            })),
        })
    }
    pub fn validate(&self, spec: &mut SqliteConnectionSpec) -> Result<(), DriverError> {
        // Windows reparse-point/file-identity handling has not been accepted.
        if !cfg!(unix) {
            return Err(error(
                Code::UnsupportedForEngine,
                "SQLite file access is currently supported on Unix only",
            ));
        }
        let path = Path::new(&spec.file_path);
        let root = self
            .config
            .roots
            .values()
            .find(|root| {
                path.strip_prefix(&root.path).is_ok_and(|relative| {
                    !relative.as_os_str().is_empty()
                        && relative
                            .components()
                            .all(|c| matches!(c, Component::Normal(_)))
                })
            })
            .ok_or_else(denied)?;
        if !path.is_absolute() || spec.file_path.contains('\0') {
            return Err(denied());
        }
        let mut prefix = PathBuf::new();
        for component in path.components() {
            if !matches!(component, Component::RootDir | Component::Normal(_)) {
                return Err(denied());
            }
            prefix.push(component);
            if std::fs::symlink_metadata(&prefix)
                .map_err(|_| denied())?
                .file_type()
                .is_symlink()
            {
                return Err(denied());
            }
        }
        let metadata = std::fs::metadata(path).map_err(|_| denied())?;
        if !metadata.is_file() {
            return Err(denied());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.nlink() != 1 {
                return Err(denied());
            }
        }
        let canonical = path.canonicalize().map_err(|_| denied())?;
        for protected in &self.protected {
            let absolute = protected
                .canonicalize()
                .unwrap_or_else(|_| protected.clone());
            if canonical.starts_with(&absolute) {
                return Err(denied());
            }
            #[cfg(unix)]
            if let Ok(other) = std::fs::metadata(protected) {
                use std::os::unix::fs::MetadataExt;
                if metadata.dev() == other.dev() && metadata.ino() == other.ino() {
                    return Err(denied());
                }
            }
        }
        spec.read_only |= root.read_only;
        Ok(())
    }
}
fn denied() -> DriverError {
    error(
        Code::UnsupportedForEngine,
        "SQLite file is not authorized or is not an existing regular file",
    )
}

#[cfg(unix)]
fn file_identity(metadata: &std::fs::Metadata) -> String {
    use std::os::unix::fs::MetadataExt;
    format!(
        "{}:{}:{}:{}:{}",
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec()
    )
}

#[cfg(not(unix))]
fn file_identity(metadata: &std::fs::Metadata) -> String {
    format!("{}:{:?}", metadata.len(), metadata.modified().ok())
}
