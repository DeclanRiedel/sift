//! PostgreSQL maintenance review state for the Vim Monitor.

use gpui::Entity;
use sift_protocol::{
    IntegrityCheckReport, PostgresMaintenanceAction, PostgresMaintenanceReport,
    PostgresMaintenanceRequest,
};

use super::TextInput;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum PgMaintenanceChoice {
    #[default]
    Vacuum,
    Analyze,
    ReindexTable,
    ReindexIndex,
    HeapIntegrity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PgMaintenancePhase {
    Preview,
    Apply,
    Integrity,
}

pub(super) struct PgMaintenanceState {
    pub schema: Entity<TextInput>,
    pub name: Entity<TextInput>,
    pub confirmation: Entity<TextInput>,
    pub choice: PgMaintenanceChoice,
    pub analyze_with_vacuum: bool,
    pub concurrently: bool,
    pub generation: u64,
    pub pending: Option<(u64, PgMaintenancePhase)>,
    pub preview_request: Option<PostgresMaintenanceRequest>,
    pub preview: Option<(PostgresMaintenanceRequest, PostgresMaintenanceReport)>,
    pub last_report: Option<PostgresMaintenanceReport>,
    pub integrity: Option<IntegrityCheckReport>,
    pub message: Option<String>,
}

impl PgMaintenanceState {
    pub fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.pending = None;
        self.preview_request = None;
        self.preview = None;
        self.last_report = None;
        self.integrity = None;
        self.message = None;
    }

    pub fn begin(&mut self, phase: PgMaintenancePhase) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.pending = Some((self.generation, phase));
        self.message = None;
        self.generation
    }

    pub fn form_changed(&mut self) {
        if self
            .pending
            .is_some_and(|(_, phase)| phase == PgMaintenancePhase::Apply)
        {
            self.preview = None;
            self.preview_request = None;
        } else {
            self.invalidate();
        }
    }

    pub fn accepts(&self, generation: u64, phase: PgMaintenancePhase) -> bool {
        self.pending == Some((generation, phase))
    }
}

fn target(schema: &str, name: &str) -> Result<(String, String), String> {
    let schema = schema.trim();
    let name = name.trim();
    for (label, value) in [("Schema", schema), ("Object", name)] {
        if value.is_empty() || value.len() > 63 || value.chars().any(char::is_control) {
            return Err(format!(
                "{label} must be 1..63 bytes without control characters"
            ));
        }
    }
    Ok((schema.to_owned(), name.to_owned()))
}

pub(super) fn request(
    choice: PgMaintenanceChoice,
    schema: &str,
    name: &str,
    analyze_with_vacuum: bool,
    concurrently: bool,
) -> Result<PostgresMaintenanceRequest, String> {
    let (schema, name) = target(schema, name)?;
    let action = match choice {
        PgMaintenanceChoice::Vacuum => PostgresMaintenanceAction::Vacuum {
            analyze: analyze_with_vacuum,
        },
        PgMaintenanceChoice::Analyze => PostgresMaintenanceAction::Analyze,
        PgMaintenanceChoice::ReindexTable => {
            PostgresMaintenanceAction::ReindexTable { concurrently }
        }
        PgMaintenanceChoice::ReindexIndex => {
            PostgresMaintenanceAction::ReindexIndex { concurrently }
        }
        PgMaintenanceChoice::HeapIntegrity => {
            return Err("Choose a maintenance action before previewing".into());
        }
    };
    Ok(PostgresMaintenanceRequest {
        action,
        schema,
        name,
        apply: false,
    })
}

pub(super) fn integrity_target(schema: &str, name: &str) -> Result<(String, String), String> {
    target(schema, name)
}

pub(super) fn confirmed_apply(
    current: &PostgresMaintenanceRequest,
    preview: &(PostgresMaintenanceRequest, PostgresMaintenanceReport),
    confirmation: &str,
) -> Result<PostgresMaintenanceRequest, String> {
    let (reviewed, report) = preview;
    if serde_json::to_value(current).ok() != serde_json::to_value(reviewed).ok()
        || report.applied
        || report.sql.is_empty()
    {
        return Err("Target or action changed since preview; preview again".into());
    }
    let expected = format!("APPLY {}.{}", current.schema, current.name);
    if confirmation != expected {
        return Err(format!("Type {expected} to confirm"));
    }
    let mut request = current.clone();
    request.apply = true;
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_requires_same_target_action_and_exact_confirmation() {
        let reviewed = request(
            PgMaintenanceChoice::ReindexTable,
            "public",
            "events",
            false,
            true,
        )
        .unwrap();
        let preview = (
            reviewed.clone(),
            PostgresMaintenanceReport {
                sql: "REINDEX TABLE CONCURRENTLY \"public\".\"events\"".into(),
                applied: false,
                warnings: vec![],
            },
        );
        assert!(
            confirmed_apply(&reviewed, &preview, "APPLY public.events")
                .unwrap()
                .apply
        );
        assert!(confirmed_apply(&reviewed, &preview, "APPLY public.other").is_err());
        let changed = request(
            PgMaintenanceChoice::ReindexIndex,
            "public",
            "events",
            false,
            true,
        )
        .unwrap();
        assert!(confirmed_apply(&changed, &preview, "APPLY public.events").is_err());
        assert!(request(
            PgMaintenanceChoice::Vacuum,
            "public",
            &"x".repeat(64),
            false,
            false
        )
        .is_err());
    }
}
