//! SQLite file maintenance review state for the Vim Monitor.

use gpui::Entity;
use sift_protocol::{SqliteMaintenanceAction, SqliteMaintenanceReport, SqliteMaintenanceRequest};

use super::TextInput;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum SqliteMaintenanceChoice {
    #[default]
    Create,
    Backup,
    Integrity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SqliteMaintenancePhase {
    Preview,
    Apply,
    Integrity,
}

pub(super) struct SqliteMaintenanceState {
    pub destination: Entity<TextInput>,
    pub confirmation: Entity<TextInput>,
    pub choice: SqliteMaintenanceChoice,
    pub generation: u64,
    pub pending: Option<(u64, SqliteMaintenancePhase)>,
    pub preview_request: Option<SqliteMaintenanceRequest>,
    pub preview: Option<(SqliteMaintenanceRequest, SqliteMaintenanceReport)>,
    pub last_report: Option<SqliteMaintenanceReport>,
    pub integrity: Option<sift_protocol::IntegrityCheckReport>,
    pub message: Option<String>,
}

impl SqliteMaintenanceState {
    pub fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.pending = None;
        self.preview_request = None;
        self.preview = None;
        self.last_report = None;
        self.integrity = None;
        self.message = None;
    }

    pub fn begin(&mut self, phase: SqliteMaintenancePhase) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.pending = Some((self.generation, phase));
        self.message = None;
        self.generation
    }

    pub fn form_changed(&mut self) {
        if self
            .pending
            .is_some_and(|(_, phase)| phase == SqliteMaintenancePhase::Apply)
        {
            self.preview = None;
            self.preview_request = None;
        } else {
            self.invalidate();
        }
    }

    pub fn accepts(&self, generation: u64, phase: SqliteMaintenancePhase) -> bool {
        self.pending == Some((generation, phase))
    }
}

pub(super) fn request(
    choice: SqliteMaintenanceChoice,
    path: &str,
) -> Result<SqliteMaintenanceRequest, String> {
    let path = path.trim();
    if path.is_empty()
        || path.len() > 512
        || path.contains([':', '\\'])
        || path.chars().any(char::is_control)
        || !std::path::Path::new(path)
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
    {
        return Err("Destination must be a relative path of 1..512 bytes".into());
    }
    let action = match choice {
        SqliteMaintenanceChoice::Create => SqliteMaintenanceAction::Create { path: path.into() },
        SqliteMaintenanceChoice::Backup => SqliteMaintenanceAction::Backup { path: path.into() },
        SqliteMaintenanceChoice::Integrity => {
            return Err("Choose Create or Backup before previewing".into())
        }
    };
    Ok(SqliteMaintenanceRequest {
        action,
        apply: false,
        confirm_write: false,
        preview_token: None,
    })
}

pub(super) fn confirmed_apply(
    current: &SqliteMaintenanceRequest,
    preview: &(SqliteMaintenanceRequest, SqliteMaintenanceReport),
    confirmation: &str,
) -> Result<SqliteMaintenanceRequest, String> {
    let (reviewed, report) = preview;
    if current.action != reviewed.action || report.action != reviewed.action || report.applied {
        return Err("Destination or action changed since preview; preview again".into());
    }
    let token = report
        .preview_token
        .as_ref()
        .ok_or("Preview expired; preview again")?;
    let (verb, path) = match &current.action {
        SqliteMaintenanceAction::Create { path } => ("CREATE", path),
        SqliteMaintenanceAction::Backup { path } => ("BACKUP", path),
    };
    if report.destination_file.as_deref() != Some(path) {
        return Err("Destination changed since preview; preview again".into());
    }
    let expected = format!("{verb} {path}");
    if confirmation != expected {
        return Err(format!("Type {expected} to confirm"));
    }
    let mut request = current.clone();
    request.apply = true;
    request.confirm_write = true;
    request.preview_token = Some(token.clone());
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_requires_same_action_exact_confirmation_and_token() {
        let reviewed = request(SqliteMaintenanceChoice::Backup, "backups/source.db").unwrap();
        let preview = (
            reviewed.clone(),
            SqliteMaintenanceReport {
                action: reviewed.action.clone(),
                applied: false,
                root_id: "root".into(),
                source_file: "source.db".into(),
                destination_file: Some("backups/source.db".into()),
                source_bytes: 4096,
                backup_file: Some("backups/source.db".into()),
                backup_expectation: "Verify backup".into(),
                preview_token: Some("one-use".into()),
            },
        );
        let apply = confirmed_apply(&reviewed, &preview, "BACKUP backups/source.db").unwrap();
        assert!(apply.apply && apply.confirm_write);
        assert_eq!(apply.preview_token.as_deref(), Some("one-use"));
        assert!(confirmed_apply(&reviewed, &preview, "backup backups/source.db").is_err());
        assert!(confirmed_apply(
            &request(SqliteMaintenanceChoice::Create, "backups/source.db").unwrap(),
            &preview,
            "CREATE backups/source.db"
        )
        .is_err());
        assert!(request(SqliteMaintenanceChoice::Create, "../outside.db").is_err());
        assert!(request(SqliteMaintenanceChoice::Create, "/outside.db").is_err());
    }
}
