use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use sift_protocol::{
    VcsDiff, VcsDiffSide, VcsFileState, VcsStageState, VcsStatus, VcsStatusEntry, WorkspacePath,
};

use crate::settings::{RepositoryGrouping, RepositorySort, RepositoryView};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RepositorySection {
    Conflicts,
    Staged,
    Unstaged,
    Untracked,
    Added,
    Modified,
    Deleted,
    Renamed,
    Other,
}

impl RepositorySection {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Conflicts => "CONFLICTS",
            Self::Staged => "STAGED",
            Self::Unstaged => "UNSTAGED",
            Self::Untracked => "UNTRACKED",
            Self::Added => "ADDED",
            Self::Modified => "MODIFIED",
            Self::Deleted => "DELETED",
            Self::Renamed => "RENAMED / COPIED",
            Self::Other => "OTHER",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RepositoryRow {
    Section {
        section: RepositorySection,
        count: usize,
    },
    Folder {
        path: String,
        depth: usize,
    },
    Entry {
        entry: VcsStatusEntry,
        depth: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RepositoryOperation {
    Refresh,
    Stage,
    Unstage,
    Commit,
    Uncommit,
}

impl RepositoryOperation {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Refresh => "Refreshing source control…",
            Self::Stage => "Staging changes…",
            Self::Unstage => "Unstaging changes…",
            Self::Commit => "Creating checkpoint and commit…",
            Self::Uncommit => "Creating checkpoint and uncommitting HEAD…",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RepositoryFailureKind {
    UnavailableRoot,
    UntrustedOwnership,
    Disabled,
    StaleBinding,
    Command,
}

impl RepositoryFailureKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::UnavailableRoot => "Workspace root unavailable",
            Self::UntrustedOwnership => "Repository ownership is not trusted",
            Self::Disabled => "Git is disabled",
            Self::StaleBinding => "Repository binding is stale",
            Self::Command => "Git command failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepositoryFailure {
    pub(crate) kind: RepositoryFailureKind,
    pub(crate) message: String,
}

#[derive(Debug, Default)]
pub(crate) struct RepositoryProjection {
    workspace_id: Option<i64>,
    status: Option<VcsStatus>,
    visible_rows: Arc<[RepositoryRow]>,
    selected_path: Option<WorkspacePath>,
    pending_paths: HashSet<WorkspacePath>,
    loading: bool,
    loaded: bool,
    operation: Option<RepositoryOperation>,
    error: Option<RepositoryFailure>,
    next_request_id: u64,
    current_request_id: Option<u64>,
    refresh_queued: bool,
    grouping: RepositoryGrouping,
    sort: RepositorySort,
    view: RepositoryView,
    diff_loading: bool,
    current_diff_request_id: Option<u64>,
    diffs: HashMap<(VcsDiffSide, Option<WorkspacePath>), Arc<VcsDiff>>,
}

impl RepositoryProjection {
    pub(crate) fn new(workspace_id: Option<i64>) -> Self {
        Self {
            workspace_id,
            ..Self::default()
        }
    }

    pub(crate) fn select_workspace(&mut self, workspace_id: Option<i64>) {
        if self.workspace_id == workspace_id {
            return;
        }
        self.workspace_id = workspace_id;
        self.status = None;
        self.visible_rows = Arc::default();
        self.selected_path = None;
        self.pending_paths.clear();
        self.loading = false;
        self.loaded = false;
        self.error = None;
        self.operation = None;
        self.current_request_id = None;
        self.refresh_queued = false;
        self.diff_loading = false;
        self.current_diff_request_id = None;
        self.diffs.clear();
    }

    pub(crate) fn set_view_preferences(
        &mut self,
        grouping: RepositoryGrouping,
        sort: RepositorySort,
        view: RepositoryView,
    ) {
        if self.grouping == grouping && self.sort == sort && self.view == view {
            return;
        }
        self.grouping = grouping;
        self.sort = sort;
        self.view = view;
        self.rebuild_visible_rows();
    }

    pub(crate) fn begin_refresh(&mut self) -> Option<(i64, u64)> {
        if self.loading {
            self.refresh_queued = true;
            return None;
        }
        let workspace_id = self.workspace_id?;
        let request_id = self.next_request();
        self.loading = true;
        self.operation = Some(RepositoryOperation::Refresh);
        self.error = None;
        self.current_request_id = Some(request_id);
        Some((workspace_id, request_id))
    }

    pub(crate) fn begin_path_update(
        &mut self,
        paths: Vec<WorkspacePath>,
        staged: bool,
    ) -> Option<(i64, i64, u64, u64, Vec<WorkspacePath>)> {
        if self.loading || paths.is_empty() {
            return None;
        }
        let workspace_id = self.workspace_id?;
        let status = self.status.as_ref()?;
        let binding_id = status.binding_id.0;
        let binding_revision = status.binding_revision;
        let request_id = self.next_request();
        self.pending_paths.extend(paths.iter().cloned());
        self.loading = true;
        self.operation = Some(if staged {
            RepositoryOperation::Stage
        } else {
            RepositoryOperation::Unstage
        });
        self.error = None;
        self.current_request_id = Some(request_id);
        Some((
            workspace_id,
            binding_id,
            binding_revision,
            request_id,
            paths,
        ))
    }

    pub(crate) fn begin_commit(&mut self) -> Option<(i64, i64, u64, u64)> {
        if self.loading || !self.has_staged_changes() {
            return None;
        }
        let workspace_id = self.workspace_id?;
        let status = self.status.as_ref()?;
        let binding_id = status.binding_id.0;
        let binding_revision = status.binding_revision;
        let request_id = self.next_request();
        self.loading = true;
        self.operation = Some(RepositoryOperation::Commit);
        self.error = None;
        self.current_request_id = Some(request_id);
        Some((workspace_id, binding_id, binding_revision, request_id))
    }

    pub(crate) fn begin_uncommit(&mut self) -> Option<(i64, i64, u64, u64, String)> {
        if self.loading {
            return None;
        }
        let workspace_id = self.workspace_id?;
        let status = self.status.as_ref()?;
        let binding_id = status.binding_id.0;
        let binding_revision = status.binding_revision;
        let head = status.head_oid.clone()?;
        let request_id = self.next_request();
        self.loading = true;
        self.operation = Some(RepositoryOperation::Uncommit);
        self.error = None;
        self.current_request_id = Some(request_id);
        Some((workspace_id, binding_id, binding_revision, request_id, head))
    }

    pub(crate) fn begin_diff(
        &mut self,
        side: VcsDiffSide,
        path: Option<WorkspacePath>,
    ) -> Option<(i64, i64, u64, VcsDiffSide, Option<WorkspacePath>)> {
        if self.diff_loading {
            return None;
        }
        let workspace_id = self.workspace_id?;
        let binding_id = self.status.as_ref()?.binding_id.0;
        let request_id = self.next_request();
        self.diff_loading = true;
        self.current_diff_request_id = Some(request_id);
        Some((workspace_id, binding_id, request_id, side, path))
    }

    pub(crate) fn begin_hunk_update(
        &mut self,
        path: WorkspacePath,
        staged: bool,
    ) -> Option<(i64, i64, u64, u64)> {
        if self.loading || self.diff_loading {
            return None;
        }
        let workspace_id = self.workspace_id?;
        let status = self.status.as_ref()?;
        let binding_id = status.binding_id.0;
        let binding_revision = status.binding_revision;
        let request_id = self.next_request();
        self.pending_paths.insert(path);
        self.loading = true;
        self.operation = Some(if staged {
            RepositoryOperation::Stage
        } else {
            RepositoryOperation::Unstage
        });
        self.error = None;
        self.current_request_id = Some(request_id);
        Some((workspace_id, binding_id, binding_revision, request_id))
    }

    pub(crate) fn apply_diff_result(
        &mut self,
        workspace_id: i64,
        request_id: u64,
        side: VcsDiffSide,
        path: Option<WorkspacePath>,
        result: Result<VcsDiff, String>,
    ) -> Option<Result<Arc<VcsDiff>, String>> {
        if self.workspace_id != Some(workspace_id)
            || self.current_diff_request_id != Some(request_id)
        {
            return None;
        }
        self.diff_loading = false;
        self.current_diff_request_id = None;
        Some(match result {
            Ok(diff) => {
                let diff = Arc::new(diff);
                self.diffs.insert((side, path), diff.clone());
                Ok(diff)
            }
            Err(error) => {
                self.error = Some(classify_failure(error.clone()));
                Err(error)
            }
        })
    }

    pub(crate) fn apply_hunk_result(
        &mut self,
        workspace_id: i64,
        request_id: u64,
        side: VcsDiffSide,
        path: WorkspacePath,
        result: Result<(VcsStatus, VcsDiff), String>,
    ) -> Option<Result<Arc<VcsDiff>, String>> {
        let (status_result, diff_result) = match result {
            Ok((status, diff)) => (Ok(Some(status)), Ok(diff)),
            Err(error) => (Err(error.clone()), Err(error)),
        };
        if !self.apply_status_result(workspace_id, request_id, status_result) {
            return None;
        }
        Some(match diff_result {
            Ok(diff) => {
                let diff = Arc::new(diff);
                self.diffs.insert((side, Some(path)), diff.clone());
                Ok(diff)
            }
            Err(error) => Err(error),
        })
    }

    pub(crate) fn diff_loading(&self) -> bool {
        self.diff_loading
    }

    pub(crate) fn diff_stats(&self, path: &WorkspacePath) -> Option<(u32, u32)> {
        self.diffs.values().find_map(|diff| {
            diff.files
                .iter()
                .find(|file| &file.path == path)
                .map(|file| (file.additions, file.deletions))
        })
    }

    pub(crate) fn apply_status_result(
        &mut self,
        workspace_id: i64,
        request_id: u64,
        result: Result<Option<VcsStatus>, String>,
    ) -> bool {
        if self.workspace_id != Some(workspace_id) || self.current_request_id != Some(request_id) {
            return false;
        }
        self.loading = false;
        self.operation = None;
        self.loaded = true;
        self.pending_paths.clear();
        self.current_request_id = None;
        match result {
            Ok(status) => {
                let stale = status.as_ref().is_some_and(|next| {
                    self.status.as_ref().is_some_and(|current| {
                        current.binding_id == next.binding_id
                            && next.binding_revision < current.binding_revision
                    })
                });
                if !stale {
                    self.status = status;
                }
                self.error = None;
                self.rebuild_visible_rows();
            }
            Err(error) => {
                self.error = Some(classify_failure(error));
            }
        }
        true
    }

    pub(crate) fn take_queued_refresh(&mut self) -> bool {
        std::mem::take(&mut self.refresh_queued)
    }

    pub(crate) fn fail_to_send(&mut self, request_id: u64, message: impl Into<String>) {
        if self.current_request_id == Some(request_id) {
            self.loading = false;
            self.operation = None;
            self.loaded = true;
            self.pending_paths.clear();
            self.current_request_id = None;
            self.error = Some(classify_failure(message.into()));
        }
    }

    pub(crate) fn set_error(&mut self, message: impl Into<String>) {
        self.error = Some(classify_failure(message.into()));
    }

    pub(crate) fn status(&self) -> Option<&VcsStatus> {
        self.status.as_ref()
    }

    pub(crate) fn rows(&self) -> Arc<[RepositoryRow]> {
        self.visible_rows.clone()
    }

    pub(crate) fn selected_path(&self) -> Option<&WorkspacePath> {
        self.selected_path.as_ref()
    }

    pub(crate) fn selected_entry(&self) -> Option<&VcsStatusEntry> {
        let selected = self.selected_path.as_ref()?;
        self.status
            .as_ref()?
            .entries
            .iter()
            .find(|entry| &entry.path == selected)
    }

    pub(crate) fn select_path(&mut self, path: WorkspacePath) {
        if self
            .status
            .as_ref()
            .is_some_and(|status| status.entries.iter().any(|entry| entry.path == path))
        {
            self.selected_path = Some(path);
        }
    }

    pub(crate) fn move_selection(&mut self, delta: isize) -> Option<WorkspacePath> {
        let paths = self
            .visible_rows
            .iter()
            .filter_map(|row| match row {
                RepositoryRow::Entry { entry, .. } => Some(entry.path.clone()),
                RepositoryRow::Section { .. } | RepositoryRow::Folder { .. } => None,
            })
            .collect::<Vec<_>>();
        if paths.is_empty() {
            return None;
        }
        let current = self
            .selected_path
            .as_ref()
            .and_then(|selected| paths.iter().position(|path| path == selected))
            .unwrap_or(0);
        let index = (current as isize + delta).rem_euclid(paths.len() as isize) as usize;
        self.selected_path = Some(paths[index].clone());
        self.selected_path.clone()
    }

    pub(crate) fn selected_row_index(&self) -> Option<usize> {
        let selected = self.selected_path.as_ref()?;
        self.visible_rows.iter().position(
            |row| matches!(row, RepositoryRow::Entry { entry, .. } if &entry.path == selected),
        )
    }

    pub(crate) fn loading(&self) -> bool {
        self.loading
    }

    pub(crate) fn loaded(&self) -> bool {
        self.loaded
    }

    pub(crate) fn error(&self) -> Option<&RepositoryFailure> {
        self.error.as_ref()
    }

    pub(crate) fn operation(&self) -> Option<RepositoryOperation> {
        self.operation
    }

    pub(crate) fn is_path_pending(&self, path: &WorkspacePath) -> bool {
        self.pending_paths.contains(path)
    }

    pub(crate) fn stageable_paths(&self) -> Vec<WorkspacePath> {
        self.status
            .iter()
            .flat_map(|status| &status.entries)
            .filter(|entry| !matches!(entry.stage, VcsStageState::Staged | VcsStageState::Conflict))
            .map(|entry| entry.path.clone())
            .collect()
    }

    pub(crate) fn stageable_tracked_paths(&self) -> Vec<WorkspacePath> {
        self.status
            .iter()
            .flat_map(|status| &status.entries)
            .filter(|entry| {
                entry.state != VcsFileState::Untracked
                    && !matches!(entry.stage, VcsStageState::Staged | VcsStageState::Conflict)
            })
            .map(|entry| entry.path.clone())
            .collect()
    }

    pub(crate) fn unstageable_paths(&self) -> Vec<WorkspacePath> {
        self.status
            .iter()
            .flat_map(|status| &status.entries)
            .filter(|entry| {
                matches!(
                    entry.stage,
                    VcsStageState::Staged | VcsStageState::PartiallyStaged
                )
            })
            .map(|entry| entry.path.clone())
            .collect()
    }

    pub(crate) fn has_staged_changes(&self) -> bool {
        self.status
            .iter()
            .flat_map(|status| &status.entries)
            .any(|entry| {
                matches!(
                    entry.stage,
                    VcsStageState::Staged | VcsStageState::PartiallyStaged
                )
            })
    }

    fn next_request(&mut self) -> u64 {
        self.next_request_id = self.next_request_id.saturating_add(1);
        self.next_request_id
    }

    fn rebuild_visible_rows(&mut self) {
        let Some(status) = &self.status else {
            self.visible_rows = Arc::default();
            self.selected_path = None;
            return;
        };
        if self
            .selected_path
            .as_ref()
            .is_none_or(|selected| !status.entries.iter().any(|entry| &entry.path == selected))
        {
            self.selected_path = status.entries.first().map(|entry| entry.path.clone());
        }
        let mut sections = if self.grouping == RepositoryGrouping::Staging {
            vec![
                (RepositorySection::Conflicts, Vec::new()),
                (RepositorySection::Staged, Vec::new()),
                (RepositorySection::Unstaged, Vec::new()),
                (RepositorySection::Untracked, Vec::new()),
            ]
        } else {
            vec![
                (RepositorySection::Conflicts, Vec::new()),
                (RepositorySection::Added, Vec::new()),
                (RepositorySection::Modified, Vec::new()),
                (RepositorySection::Deleted, Vec::new()),
                (RepositorySection::Renamed, Vec::new()),
                (RepositorySection::Other, Vec::new()),
            ]
        };
        for entry in &status.entries {
            let index = if self.grouping == RepositoryGrouping::Staging {
                if entry.stage == VcsStageState::Conflict || entry.state == VcsFileState::Unmerged {
                    0
                } else if entry.state == VcsFileState::Untracked {
                    3
                } else if entry.stage == VcsStageState::Staged {
                    1
                } else {
                    2
                }
            } else {
                match entry.state {
                    VcsFileState::Unmerged => 0,
                    VcsFileState::Added | VcsFileState::Untracked => 1,
                    VcsFileState::Modified | VcsFileState::TypeChanged => 2,
                    VcsFileState::Deleted => 3,
                    VcsFileState::Renamed | VcsFileState::Copied => 4,
                    _ => 5,
                }
            };
            sections[index].1.push(entry.clone());
        }
        let mut rows = Vec::with_capacity(status.entries.len() + sections.len());
        for (section, mut entries) in sections {
            if entries.is_empty() {
                continue;
            }
            entries.sort_by(|left, right| match self.sort {
                RepositorySort::Path => left.path.0.cmp(&right.path.0),
                RepositorySort::FileName => file_name(&left.path.0)
                    .cmp(file_name(&right.path.0))
                    .then_with(|| left.path.0.cmp(&right.path.0)),
            });
            rows.push(RepositoryRow::Section {
                section,
                count: entries.len(),
            });
            if self.view == RepositoryView::Tree {
                let mut folders = HashSet::new();
                for entry in entries {
                    let segments = entry.path.0.split('/').collect::<Vec<_>>();
                    let mut folder = String::new();
                    for (depth, segment) in segments
                        .iter()
                        .take(segments.len().saturating_sub(1))
                        .enumerate()
                    {
                        if !folder.is_empty() {
                            folder.push('/');
                        }
                        folder.push_str(segment);
                        if folders.insert(folder.clone()) {
                            rows.push(RepositoryRow::Folder {
                                path: folder.clone(),
                                depth,
                            });
                        }
                    }
                    rows.push(RepositoryRow::Entry {
                        depth: segments.len().saturating_sub(1),
                        entry,
                    });
                }
            } else {
                rows.extend(
                    entries
                        .into_iter()
                        .map(|entry| RepositoryRow::Entry { entry, depth: 0 }),
                );
            }
        }
        self.visible_rows = rows.into();
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn classify_failure(message: String) -> RepositoryFailure {
    let normalized = message.to_ascii_lowercase();
    let kind = if normalized.contains("ownership") || normalized.contains("dubious") {
        RepositoryFailureKind::UntrustedOwnership
    } else if normalized.contains("disabled") {
        RepositoryFailureKind::Disabled
    } else if normalized.contains("root") && normalized.contains("unavailable") {
        RepositoryFailureKind::UnavailableRoot
    } else if normalized.contains("rebind")
        || normalized.contains("revision conflict")
        || normalized.contains("adapter observation changed")
    {
        RepositoryFailureKind::StaleBinding
    } else {
        RepositoryFailureKind::Command
    };
    RepositoryFailure { kind, message }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(entries: serde_json::Value) -> VcsStatus {
        serde_json::from_value(serde_json::json!({
            "binding_id": 9,
            "workspace_revision": 3,
            "binding_revision": 4,
            "head_oid": null,
            "branch": "main",
            "upstream": null,
            "entries": entries,
            "truncated": false,
            "observed_at": "2026-08-28T00:00:00Z"
        }))
        .unwrap()
    }

    fn entry(path: &str, state: &str, stage: &str) -> serde_json::Value {
        serde_json::json!({
            "path": path,
            "previous_path": null,
            "state": state,
            "stage": stage,
            "conflict": null,
            "pending": null
        })
    }

    #[test]
    fn status_is_flattened_into_stable_grouped_rows() {
        let mut projection = RepositoryProjection::new(Some(7));
        let (_, request_id) = projection.begin_refresh().unwrap();
        assert!(projection.apply_status_result(
            7,
            request_id,
            Ok(Some(status(serde_json::json!([
                entry("z.sql", "modified", "unstaged"),
                entry("new.sql", "untracked", "unstaged"),
                entry("a.sql", "added", "staged"),
                entry("conflict.sql", "unmerged", "conflict")
            ]))))
        ));

        let rows = projection.rows();
        assert!(matches!(
            rows[0],
            RepositoryRow::Section {
                section: RepositorySection::Conflicts,
                count: 1
            }
        ));
        assert!(matches!(
            rows[2],
            RepositoryRow::Section {
                section: RepositorySection::Staged,
                count: 1
            }
        ));
        assert!(matches!(
            rows[4],
            RepositoryRow::Section {
                section: RepositorySection::Unstaged,
                count: 1
            }
        ));
        assert!(matches!(
            rows[6],
            RepositoryRow::Section {
                section: RepositorySection::Untracked,
                count: 1
            }
        ));
    }

    #[test]
    fn stale_results_cannot_replace_a_different_workspace_snapshot() {
        let mut projection = RepositoryProjection::new(Some(7));
        let (_, old_request) = projection.begin_refresh().unwrap();
        projection.select_workspace(Some(8));
        let (_, new_request) = projection.begin_refresh().unwrap();
        assert!(!projection.apply_status_result(7, old_request, Ok(None)));
        assert!(projection.loading());
        assert!(projection.apply_status_result(8, new_request, Ok(None)));
        assert!(!projection.loading());
        assert!(projection.loaded());
    }

    #[test]
    fn refreshes_coalesce_while_a_request_is_in_flight() {
        let mut projection = RepositoryProjection::new(Some(12));
        let (_, request_id) = projection.begin_refresh().unwrap();

        assert_eq!(projection.begin_refresh(), None);
        assert!(projection.apply_status_result(12, request_id, Ok(None)));
        assert!(projection.take_queued_refresh());
        assert!(!projection.take_queued_refresh());
        assert!(projection.begin_refresh().is_some());
    }

    #[test]
    fn workspace_switch_drops_status_and_in_flight_paths() {
        let mut projection = RepositoryProjection::new(Some(7));
        let (_, request_id) = projection.begin_refresh().unwrap();
        projection.apply_status_result(
            7,
            request_id,
            Ok(Some(status(serde_json::json!([entry(
                "query.sql",
                "modified",
                "unstaged"
            )])))),
        );
        let operation = projection
            .begin_path_update(vec![WorkspacePath::new("query.sql").unwrap()], true)
            .unwrap();
        assert!(projection.is_path_pending(&operation.4[0]));

        projection.select_workspace(Some(8));
        assert!(projection.status().is_none());
        assert!(!projection.loading());
        assert!(!projection.is_path_pending(&operation.4[0]));
    }

    #[test]
    fn selection_survives_refresh_by_stable_path() {
        let mut projection = RepositoryProjection::new(Some(7));
        let (_, first) = projection.begin_refresh().unwrap();
        projection.apply_status_result(
            7,
            first,
            Ok(Some(status(serde_json::json!([
                entry("a.sql", "modified", "unstaged"),
                entry("b.sql", "modified", "unstaged")
            ])))),
        );
        projection.select_path(WorkspacePath::new("b.sql").unwrap());

        let (_, second) = projection.begin_refresh().unwrap();
        projection.apply_status_result(
            7,
            second,
            Ok(Some(status(serde_json::json!([
                entry("b.sql", "modified", "unstaged"),
                entry("c.sql", "untracked", "unstaged")
            ])))),
        );

        assert_eq!(projection.selected_path().unwrap().0, "b.sql");
    }

    #[test]
    fn commit_and_uncommit_intents_use_the_loaded_binding_preconditions() {
        let mut projection = RepositoryProjection::new(Some(7));
        let (_, request_id) = projection.begin_refresh().unwrap();
        let mut loaded = status(serde_json::json!([entry(
            "query.sql",
            "modified",
            "staged"
        )]));
        loaded.head_oid = Some("a".repeat(40));
        projection.apply_status_result(7, request_id, Ok(Some(loaded)));

        let commit = projection.begin_commit().unwrap();
        assert_eq!((commit.0, commit.1, commit.2), (7, 9, 4));
        projection.apply_status_result(7, commit.3, Ok(None));

        let (_, request_id) = projection.begin_refresh().unwrap();
        let mut loaded = status(serde_json::json!([entry(
            "query.sql",
            "modified",
            "staged"
        )]));
        loaded.head_oid = Some("b".repeat(40));
        projection.apply_status_result(7, request_id, Ok(Some(loaded)));
        let uncommit = projection.begin_uncommit().unwrap();
        assert_eq!((uncommit.0, uncommit.1, uncommit.2), (7, 9, 4));
        assert_eq!(uncommit.4, "b".repeat(40));
    }

    #[test]
    fn stale_file_diff_result_is_ignored_and_current_diff_is_cached() {
        let mut projection = RepositoryProjection::new(Some(7));
        let (_, status_request) = projection.begin_refresh().unwrap();
        projection.apply_status_result(
            7,
            status_request,
            Ok(Some(status(serde_json::json!([entry(
                "query.sql",
                "modified",
                "unstaged"
            )])))),
        );
        let path = WorkspacePath::new("query.sql").unwrap();
        let (_, _, request_id, side, requested_path) = projection
            .begin_diff(VcsDiffSide::IndexToWorktree, Some(path.clone()))
            .unwrap();
        let diff = VcsDiff {
            binding_id: sift_protocol::RepositoryBindingId(9),
            side,
            files: Vec::new(),
            truncated: false,
        };

        assert!(projection
            .apply_diff_result(
                7,
                request_id.saturating_add(1),
                side,
                requested_path.clone(),
                Ok(diff.clone()),
            )
            .is_none());
        assert!(projection.diff_loading());
        assert!(projection
            .apply_diff_result(7, request_id, side, requested_path, Ok(diff))
            .unwrap()
            .is_ok());
        assert!(!projection.diff_loading());
    }
}
