use gpui::Entity;
use sift_protocol::{RestoreFileMove, SqlServerRecoveryReport, SqlServerRecoveryRequest};

use super::TextInput;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum MaintenanceMode {
    #[default]
    Backup,
    Restore,
    Integrity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MaintenanceAction {
    Preview,
    Apply,
    Integrity,
}

pub(super) struct MaintenanceState {
    pub database: Entity<TextInput>,
    pub archive: Entity<TextInput>,
    pub backup_set: Entity<TextInput>,
    pub moves: Entity<TextInput>,
    pub confirmation: Entity<TextInput>,
    pub mode: MaintenanceMode,
    pub physical_only: bool,
    pub pending: Option<(u64, MaintenanceAction)>,
    pub generation: u64,
    pub preview: Option<(SqlServerRecoveryRequest, SqlServerRecoveryReport)>,
    pub preview_request: Option<SqlServerRecoveryRequest>,
    pub last_recovery: Option<SqlServerRecoveryReport>,
    pub integrity: Option<sift_protocol::IntegrityCheckReport>,
    pub message: Option<String>,
}

impl MaintenanceState {
    pub fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.pending = None;
        self.preview = None;
        self.preview_request = None;
        self.last_recovery = None;
        self.integrity = None;
        self.message = None;
    }

    pub fn begin(&mut self, action: MaintenanceAction) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.pending = Some((self.generation, action));
        self.message = None;
        self.generation
    }

    pub fn form_changed(&mut self) {
        if self
            .pending
            .is_some_and(|(_, action)| action == MaintenanceAction::Apply)
        {
            self.preview = None;
            self.preview_request = None;
        } else {
            self.invalidate();
        }
    }

    pub fn accepts(&self, generation: u64, action: MaintenanceAction) -> bool {
        self.pending == Some((generation, action))
    }
}

pub(super) fn recovery_request(
    mode: MaintenanceMode,
    database: &str,
    archive: &str,
    backup_set: &str,
    moves: &str,
) -> Result<SqlServerRecoveryRequest, String> {
    let database = database.trim();
    let archive = archive.trim();
    if database.is_empty() || archive.is_empty() {
        return Err("Database and server-side archive path are required".into());
    }
    let normalized = archive.replace('\\', "/");
    let drive_path = normalized.as_bytes().get(1) == Some(&b':')
        && normalized.as_bytes().get(2) == Some(&b'/')
        && normalized.as_bytes()[0].is_ascii_alphabetic();
    if !normalized.starts_with('/') && !drive_path {
        return Err("Use an absolute path on the SQL Server host".into());
    }
    match mode {
        MaintenanceMode::Backup => Ok(SqlServerRecoveryRequest::Backup {
            database: database.into(),
            archive_path: archive.into(),
            apply: false,
        }),
        MaintenanceMode::Restore => {
            let backup_set = backup_set
                .trim()
                .parse::<u32>()
                .map_err(|_| "Backup set must be a positive number")?;
            if backup_set == 0 {
                return Err("Backup set must be a positive number".into());
            }
            let moves: Vec<RestoreFileMove> = serde_json::from_str(moves.trim()).map_err(|_| {
                "MOVE mappings must be a JSON array of logical_name and destination objects"
            })?;
            if moves.is_empty()
                || moves.iter().any(|item| {
                    item.logical_name.trim().is_empty() || item.destination.trim().is_empty()
                })
            {
                return Err("At least one complete MOVE mapping is required".into());
            }
            Ok(SqlServerRecoveryRequest::Restore {
                database: database.into(),
                archive_path: archive.into(),
                backup_set,
                moves,
                apply: false,
            })
        }
        MaintenanceMode::Integrity => Err("Select Backup or Restore to preview recovery".into()),
    }
}

pub(super) fn confirmed_apply(
    current: &SqlServerRecoveryRequest,
    preview: &SqlServerRecoveryRequest,
    confirmation: &str,
) -> Result<SqlServerRecoveryRequest, String> {
    if serde_json::to_value(current).ok() != serde_json::to_value(preview).ok() {
        return Err("Inputs changed since preview. Preview again before applying".into());
    }
    let (expected, request) = match preview {
        SqlServerRecoveryRequest::Backup {
            database,
            archive_path,
            ..
        } => (
            format!("BACKUP {database}"),
            SqlServerRecoveryRequest::Backup {
                database: database.clone(),
                archive_path: archive_path.clone(),
                apply: true,
            },
        ),
        SqlServerRecoveryRequest::Restore {
            database,
            archive_path,
            backup_set,
            moves,
            ..
        } => (
            format!("RESTORE {database}"),
            SqlServerRecoveryRequest::Restore {
                database: database.clone(),
                archive_path: archive_path.clone(),
                backup_set: *backup_set,
                moves: moves.clone(),
                apply: true,
            },
        ),
    };
    if confirmation != expected {
        return Err(format!("Type {expected} to confirm"));
    }
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_requires_exact_preview_and_confirmation() {
        let preview = recovery_request(
            MaintenanceMode::Restore,
            "new_db",
            "/backup/source.bak",
            "1",
            r#"[{"logical_name":"data","destination":"/data/new.mdf"}]"#,
        )
        .unwrap();
        assert!(confirmed_apply(&preview, &preview, "RESTORE wrong").is_err());
        let changed = recovery_request(
            MaintenanceMode::Restore,
            "other_db",
            "/backup/source.bak",
            "1",
            r#"[{"logical_name":"data","destination":"/data/new.mdf"}]"#,
        )
        .unwrap();
        assert!(confirmed_apply(&changed, &preview, "RESTORE new_db").is_err());
        assert!(matches!(
            confirmed_apply(&preview, &preview, "RESTORE new_db"),
            Ok(SqlServerRecoveryRequest::Restore { apply: true, .. })
        ));
    }

    #[test]
    fn restore_requires_explicit_moves_and_positive_backup_set() {
        assert!(recovery_request(
            MaintenanceMode::Restore,
            "new_db",
            "/backup/source.bak",
            "0",
            "[]"
        )
        .is_err());
        assert!(recovery_request(
            MaintenanceMode::Restore,
            "new_db",
            "/backup/source.bak",
            "1",
            "[]"
        )
        .is_err());
    }

    #[test]
    fn backup_uses_server_path_and_exact_phrase() {
        let preview = recovery_request(
            MaintenanceMode::Backup,
            "source",
            "C:\\backup\\source.bak",
            "",
            "",
        )
        .unwrap();
        assert!(confirmed_apply(&preview, &preview, "BACKUP source ").is_err());
        assert!(
            matches!(confirmed_apply(&preview, &preview, "BACKUP source"), Ok(SqlServerRecoveryRequest::Backup { archive_path, apply: true, .. }) if archive_path == "C:\\backup\\source.bak")
        );
    }
}
