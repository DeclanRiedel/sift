//! Server-owned roots are an access boundary, not a sandbox against a local
//! process that can replace the operator-owned directory during open.
use super::error;
use sift_protocol::*;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Default)]
pub struct FilePolicy {
    pub config: SqliteDriverConfig,
    pub protected: Vec<PathBuf>,
}
impl FilePolicy {
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
