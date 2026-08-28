//! Bounded, side-effect-free validation for SQL artifacts entering Git.

use std::collections::{BTreeMap, BTreeSet};

use sift_protocol::{
    VcsSqlArtifactValidation, VcsStageState, VcsStatus, VcsValidationDiagnostic,
    VcsValidationReport, WorkspacePath,
};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

use crate::workspace_projection::WorkspaceProjectionFile;

const MAX_VALIDATED_FILE_BYTES: usize = 1024 * 1024;

pub fn validate(status: &VcsStatus, files: &[WorkspaceProjectionFile]) -> VcsValidationReport {
    let files = files
        .iter()
        .map(|file| (file.path.0.as_str(), file))
        .collect::<BTreeMap<_, _>>();
    let staged = status
        .entries
        .iter()
        .filter(|entry| {
            matches!(
                entry.stage,
                VcsStageState::Staged | VcsStageState::PartiallyStaged
            )
        })
        .collect::<Vec<_>>();
    let staged_paths = staged
        .iter()
        .map(|entry| entry.path.0.as_str())
        .collect::<BTreeSet<_>>();
    let mut artifacts = Vec::new();
    let mut diagnostics = Vec::new();

    for entry in staged {
        let Some(file) = files.get(entry.path.0.as_str()) else {
            continue;
        };
        if file.bytes.len() > MAX_VALIDATED_FILE_BYTES {
            diagnostics.push(error(
                &entry.path,
                "file_too_large",
                "SQL artifact exceeds the 1 MiB validation ceiling",
            ));
            continue;
        }
        let Ok(sql) = std::str::from_utf8(&file.bytes) else {
            diagnostics.push(error(
                &entry.path,
                "non_utf8",
                "SQL artifacts must be UTF-8 text",
            ));
            continue;
        };
        if secret_shaped(sql) {
            diagnostics.push(error(
                &entry.path,
                "secret_shaped_value",
                "SQL artifact contains a credential or secret-shaped value",
            ));
        }
        match Parser::parse_sql(&GenericDialect {}, sql) {
            Ok(statements) if !statements.is_empty() => {
                let canonical = statements
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(";\n");
                artifacts.push(VcsSqlArtifactValidation {
                    path: entry.path.clone(),
                    affected_objects: affected_objects(sql),
                    formatted: canonical.trim() == sql.trim().trim_end_matches(';').trim(),
                });
            }
            Ok(_) => diagnostics.push(error(
                &entry.path,
                "empty_sql",
                "Staged SQL artifact has no statements",
            )),
            Err(_) => diagnostics.push(error(
                &entry.path,
                "syntax",
                "Staged SQL artifact could not be parsed",
            )),
        }
    }

    for path in &staged_paths {
        if let Some(stem) = path.strip_suffix(".migration.sql") {
            let rollback = format!("{stem}.rollback.sql");
            if !staged_paths.contains(rollback.as_str()) {
                diagnostics.push(error(
                    &WorkspacePath((*path).into()),
                    "migration_pair",
                    "Migration and rollback scripts must be staged together",
                ));
            }
        }
        if let Some(stem) = path.strip_suffix(".rollback.sql") {
            let migration = format!("{stem}.migration.sql");
            if !staged_paths.contains(migration.as_str()) {
                diagnostics.push(error(
                    &WorkspacePath((*path).into()),
                    "migration_pair",
                    "Migration and rollback scripts must be staged together",
                ));
            }
        }
    }
    diagnostics
        .sort_by(|left, right| (&left.path.0, &left.code).cmp(&(&right.path.0, &right.code)));
    VcsValidationReport {
        valid: !diagnostics.iter().any(|diagnostic| diagnostic.error),
        artifacts,
        diagnostics,
    }
}

fn error(path: &WorkspacePath, code: &str, message: &str) -> VcsValidationDiagnostic {
    VcsValidationDiagnostic {
        path: path.clone(),
        code: code.into(),
        message: message.into(),
        error: true,
    }
}

fn secret_shaped(sql: &str) -> bool {
    let lower = sql.to_ascii_lowercase();
    [
        "password=",
        "password =",
        "pwd=",
        "pwd =",
        "bearer ",
        "private key",
        "secret_access_key",
        "postgres://",
        "postgresql://",
        "sqlserver://",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn affected_objects(sql: &str) -> Vec<String> {
    let tokens = sql.split_whitespace().collect::<Vec<_>>();
    let mut objects = BTreeSet::new();
    for pair in tokens.windows(2) {
        let keyword = pair[0]
            .trim_matches(|c: char| !c.is_ascii_alphabetic())
            .to_ascii_lowercase();
        if matches!(
            keyword.as_str(),
            "from" | "join" | "into" | "update" | "table" | "view" | "sequence"
        ) {
            let object = pair[1]
                .trim_matches(|c: char| matches!(c, ',' | ';' | '(' | ')' | '`' | '"' | '[' | ']'));
            if !object.is_empty() && object.len() <= 256 {
                objects.insert(object.to_owned());
            }
        }
    }
    objects.into_iter().take(64).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_protocol::{
        RepositoryBindingId, VcsFileState, VcsStatusEntry, WorkspaceNodeId, WorkspaceRevision,
    };

    fn status(path: &str) -> VcsStatus {
        VcsStatus {
            binding_id: RepositoryBindingId(1),
            workspace_revision: WorkspaceRevision(1),
            binding_revision: 1,
            head_oid: None,
            branch: None,
            upstream: None,
            operation: None,
            entries: vec![VcsStatusEntry {
                path: WorkspacePath(path.into()),
                previous_path: None,
                state: VcsFileState::Added,
                stage: VcsStageState::Staged,
                conflict: None,
                pending: None,
                affected_objects: Vec::new(),
                validation_errors: 0,
            }],
            truncated: false,
            observed_at: chrono::Utc::now(),
            validation: None,
        }
    }

    fn file(path: &str, text: &str) -> WorkspaceProjectionFile {
        WorkspaceProjectionFile {
            node_id: WorkspaceNodeId(1),
            path: WorkspacePath(path.into()),
            digest: "x".into(),
            bytes: text.as_bytes().to_vec(),
        }
    }

    #[test]
    fn rejects_secrets_without_echoing_them() {
        let report = validate(
            &status("query.sql"),
            &[file("query.sql", "select 'password=hunter2'")],
        );
        assert!(!report.valid);
        assert_eq!(report.diagnostics[0].code, "secret_shaped_value");
        assert!(!report.diagnostics[0].message.contains("hunter2"));
    }

    #[test]
    fn extracts_objects_and_requires_migration_pair() {
        let report = validate(
            &status("001.migration.sql"),
            &[file(
                "001.migration.sql",
                "ALTER TABLE public.users ADD COLUMN active bool",
            )],
        );
        assert!(!report.valid);
        assert!(report.artifacts[0]
            .affected_objects
            .contains(&"public.users".into()));
        assert!(report
            .diagnostics
            .iter()
            .any(|item| item.code == "migration_pair"));
    }
}
