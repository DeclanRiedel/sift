use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use sift_protocol::{
    RepositoryBinding, VcsBranch, VcsCommitDetail, VcsCommitSummary, VcsConflictFile, VcsDiff,
    VcsDiffSide, VcsFileState, VcsHistoricalFile, VcsHistoryPage, VcsRemote, VcsRemoteResult,
    VcsStageState, VcsStatus, VcsStatusEntry, WorkspacePath,
};

use crate::settings::{RepositoryGrouping, RepositorySort, RepositoryView};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RepositorySection {
    Conflicts,
    Staged,
    Changes,
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
            Self::Changes => "CHANGES",
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
    Discard,
    Revert,
    Network,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RepositoryIndexAction {
    Stage,
    Unstage,
}

impl RepositoryIndexAction {
    pub(crate) fn staged(self) -> bool {
        matches!(self, Self::Stage)
    }

    fn operation(self) -> RepositoryOperation {
        match self {
            Self::Stage => RepositoryOperation::Stage,
            Self::Unstage => RepositoryOperation::Unstage,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum RepositoryLoadState {
    #[default]
    NotLoaded,
    Loading,
    Loaded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RepositoryActivity {
    operation: RepositoryOperation,
    request_id: Option<u64>,
}

impl RepositoryOperation {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Refresh => "Refreshing source control…",
            Self::Stage => "Staging changes…",
            Self::Unstage => "Unstaging changes…",
            Self::Commit => "Creating checkpoint and commit…",
            Self::Uncommit => "Creating checkpoint and uncommitting HEAD…",
            Self::Discard => "Creating checkpoint and discarding worktree change…",
            Self::Revert => "Creating checkpoint and reverting diff hunk…",
            Self::Network => "Running visible remote operation…",
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SharedRepositoryOperation {
    pub(crate) actor_principal_id: i64,
    pub(crate) action: sift_protocol::VcsAction,
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
    observed_binding: Option<RepositoryBinding>,
    visible_rows: Arc<[RepositoryRow]>,
    selected_path: Option<WorkspacePath>,
    pending_paths: HashSet<WorkspacePath>,
    load_state: RepositoryLoadState,
    activity: Option<RepositoryActivity>,
    error: Option<RepositoryFailure>,
    next_request_id: u64,
    refresh_queued: bool,
    grouping: RepositoryGrouping,
    sort: RepositorySort,
    view: RepositoryView,
    filter: String,
    diff_loading: bool,
    current_diff_request_id: Option<u64>,
    diffs: HashMap<(VcsDiffSide, Option<WorkspacePath>), Arc<VcsDiff>>,
    branches: Arc<[VcsBranch]>,
    history: Vec<VcsCommitSummary>,
    history_cursor: Option<String>,
    history_loading: bool,
    history_query: String,
    current_history_request_id: Option<u64>,
    commit_detail: Option<VcsCommitDetail>,
    historical_file: Option<VcsHistoricalFile>,
    comparison_base: Option<String>,
    comparison: Option<VcsDiff>,
    conflict: Option<VcsConflictFile>,
    remotes: Arc<[VcsRemote]>,
    remote_result: Option<VcsRemoteResult>,
    shared_operation: Option<SharedRepositoryOperation>,
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
        self.observed_binding = None;
        self.visible_rows = Arc::default();
        self.selected_path = None;
        self.pending_paths.clear();
        self.load_state = RepositoryLoadState::NotLoaded;
        self.activity = None;
        self.error = None;
        self.refresh_queued = false;
        self.diff_loading = false;
        self.current_diff_request_id = None;
        self.diffs.clear();
        self.branches = Arc::default();
        self.history.clear();
        self.history_cursor = None;
        self.history_loading = false;
        self.history_query.clear();
        self.current_history_request_id = None;
        self.commit_detail = None;
        self.historical_file = None;
        self.comparison_base = None;
        self.comparison = None;
        self.conflict = None;
        self.remotes = Arc::default();
        self.remote_result = None;
        self.shared_operation = None;
        self.filter.clear();
    }

    pub(crate) fn branches(&self) -> Arc<[VcsBranch]> {
        self.branches.clone()
    }

    pub(crate) fn history(&self) -> &[VcsCommitSummary] {
        &self.history
    }

    pub(crate) fn history_cursor(&self) -> Option<&str> {
        self.history_cursor.as_deref()
    }

    pub(crate) fn history_loading(&self) -> bool {
        self.history_loading
    }

    pub(crate) fn commit_detail(&self) -> Option<&VcsCommitDetail> {
        self.commit_detail.as_ref()
    }

    pub(crate) fn historical_file(&self) -> Option<&VcsHistoricalFile> {
        self.historical_file.as_ref()
    }

    pub(crate) fn comparison_base(&self) -> Option<&str> {
        self.comparison_base.as_deref()
    }

    pub(crate) fn set_comparison_base(&mut self, oid: Option<String>) {
        self.comparison_base = oid;
    }

    pub(crate) fn comparison(&self) -> Option<&VcsDiff> {
        self.comparison.as_ref()
    }

    pub(crate) fn conflict(&self) -> Option<&VcsConflictFile> {
        self.conflict.as_ref()
    }

    pub(crate) fn remotes(&self) -> Arc<[VcsRemote]> {
        self.remotes.clone()
    }

    pub(crate) fn remote_result(&self) -> Option<&VcsRemoteResult> {
        self.remote_result.as_ref()
    }

    pub(crate) fn shared_operation(&self) -> Option<SharedRepositoryOperation> {
        self.shared_operation
    }

    pub(crate) fn apply_shared_operation(
        &mut self,
        actor_principal_id: i64,
        action: sift_protocol::VcsAction,
        phase: sift_protocol::RepositoryOperationPhase,
    ) {
        match phase {
            sift_protocol::RepositoryOperationPhase::Started => {
                self.shared_operation = Some(SharedRepositoryOperation {
                    actor_principal_id,
                    action,
                });
            }
            sift_protocol::RepositoryOperationPhase::Succeeded
            | sift_protocol::RepositoryOperationPhase::Failed => {
                self.shared_operation = None;
            }
        }
    }

    pub(crate) fn apply_remotes(&mut self, result: Result<Vec<VcsRemote>, String>) {
        match result {
            Ok(remotes) => self.remotes = remotes.into(),
            Err(message) => self.set_error(message),
        }
    }

    pub(crate) fn begin_network_operation(&mut self) -> bool {
        if self.mutation_in_flight() {
            return false;
        }
        self.activity = Some(RepositoryActivity {
            operation: RepositoryOperation::Network,
            request_id: None,
        });
        self.error = None;
        true
    }

    pub(crate) fn finish_network_operation(
        &mut self,
        result: Result<Option<VcsRemoteResult>, String>,
    ) -> bool {
        self.activity = None;
        match result {
            Ok(result) => {
                self.remote_result = result;
                self.error = None;
                true
            }
            Err(message) => {
                self.set_error(message);
                false
            }
        }
    }

    pub(crate) fn set_conflict(&mut self, result: Result<VcsConflictFile, String>) {
        match result {
            Ok(conflict) => self.conflict = Some(conflict),
            Err(message) => self.set_error(message),
        }
    }

    pub(crate) fn begin_history_load(&mut self, query: String, append: bool) -> Option<u64> {
        if append && self.history_loading {
            return None;
        }
        if !append {
            self.history.clear();
            self.history_cursor = None;
        }
        self.history_query = query;
        self.history_loading = true;
        let request_id = self.next_request();
        self.current_history_request_id = Some(request_id);
        Some(request_id)
    }

    pub(crate) fn apply_branches(&mut self, result: Result<Vec<VcsBranch>, String>) {
        match result {
            Ok(branches) => self.branches = branches.into(),
            Err(message) => self.set_error(message),
        }
    }

    pub(crate) fn apply_history(
        &mut self,
        workspace_id: i64,
        request_id: u64,
        result: Result<VcsHistoryPage, String>,
        append: bool,
    ) -> bool {
        if self.workspace_id != Some(workspace_id)
            || self.current_history_request_id != Some(request_id)
        {
            return false;
        }
        self.history_loading = false;
        self.current_history_request_id = None;
        match result {
            Ok(page) => {
                if !append {
                    self.history.clear();
                }
                self.history.extend(page.commits);
                self.history_cursor = page.next_cursor;
            }
            Err(message) => self.set_error(message),
        }
        true
    }

    pub(crate) fn set_commit_detail(&mut self, result: Result<VcsCommitDetail, String>) {
        match result {
            Ok(detail) => self.commit_detail = Some(detail),
            Err(message) => self.set_error(message),
        }
    }

    pub(crate) fn set_historical_file(&mut self, result: Result<VcsHistoricalFile, String>) {
        match result {
            Ok(file) => self.historical_file = Some(file),
            Err(message) => self.set_error(message),
        }
    }

    pub(crate) fn set_comparison(&mut self, result: Result<VcsDiff, String>) {
        match result {
            Ok(diff) => {
                self.comparison = Some(diff);
                self.comparison_base = None;
            }
            Err(message) => self.set_error(message),
        }
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
        if self.mutation_in_flight() {
            self.refresh_queued = true;
            return None;
        }
        let workspace_id = self.workspace_id?;
        let request_id = self.next_request();
        if self.load_state == RepositoryLoadState::NotLoaded {
            self.load_state = RepositoryLoadState::Loading;
        }
        self.activity = Some(RepositoryActivity {
            operation: RepositoryOperation::Refresh,
            request_id: Some(request_id),
        });
        self.error = None;
        Some((workspace_id, request_id))
    }

    pub(crate) fn begin_path_update(
        &mut self,
        paths: Vec<WorkspacePath>,
        action: RepositoryIndexAction,
    ) -> Option<(i64, i64, u64, u64, Vec<WorkspacePath>)> {
        if self.mutation_in_flight() || paths.is_empty() {
            return None;
        }
        let workspace_id = self.workspace_id?;
        let status = self.status.as_ref()?;
        let binding_id = status.binding_id.0;
        let binding_revision = status.binding_revision;
        let request_id = self.next_request();
        self.pending_paths.extend(paths.iter().cloned());
        self.activity = Some(RepositoryActivity {
            operation: action.operation(),
            request_id: Some(request_id),
        });
        self.error = None;
        Some((
            workspace_id,
            binding_id,
            binding_revision,
            request_id,
            paths,
        ))
    }

    pub(crate) fn begin_commit(&mut self) -> Option<(i64, i64, u64, u64)> {
        if self.mutation_in_flight() || !self.has_staged_changes() {
            return None;
        }
        let workspace_id = self.workspace_id?;
        let status = self.status.as_ref()?;
        let binding_id = status.binding_id.0;
        let binding_revision = status.binding_revision;
        let request_id = self.next_request();
        self.activity = Some(RepositoryActivity {
            operation: RepositoryOperation::Commit,
            request_id: Some(request_id),
        });
        self.error = None;
        Some((workspace_id, binding_id, binding_revision, request_id))
    }

    pub(crate) fn begin_uncommit(&mut self) -> Option<(i64, i64, u64, u64, String)> {
        if self.mutation_in_flight() {
            return None;
        }
        let workspace_id = self.workspace_id?;
        let status = self.status.as_ref()?;
        let binding_id = status.binding_id.0;
        let binding_revision = status.binding_revision;
        let head = status.head_oid.clone()?;
        let request_id = self.next_request();
        self.activity = Some(RepositoryActivity {
            operation: RepositoryOperation::Uncommit,
            request_id: Some(request_id),
        });
        self.error = None;
        Some((workspace_id, binding_id, binding_revision, request_id, head))
    }

    pub(crate) fn begin_diff(
        &mut self,
        side: VcsDiffSide,
        path: Option<WorkspacePath>,
    ) -> Option<(i64, i64, u64, VcsDiffSide, Option<WorkspacePath>)> {
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
        action: RepositoryIndexAction,
    ) -> Option<(i64, i64, u64, u64)> {
        if self.mutation_in_flight() || self.diff_loading {
            return None;
        }
        let workspace_id = self.workspace_id?;
        let status = self.status.as_ref()?;
        let binding_id = status.binding_id.0;
        let binding_revision = status.binding_revision;
        let request_id = self.next_request();
        self.pending_paths.insert(path);
        self.activity = Some(RepositoryActivity {
            operation: action.operation(),
            request_id: Some(request_id),
        });
        self.error = None;
        Some((workspace_id, binding_id, binding_revision, request_id))
    }

    pub(crate) fn begin_discard(&mut self, path: WorkspacePath) -> Option<(i64, i64, u64, u64)> {
        self.begin_destructive(path, RepositoryOperation::Discard)
    }

    pub(crate) fn begin_revert(&mut self, path: WorkspacePath) -> Option<(i64, i64, u64, u64)> {
        self.begin_destructive(path, RepositoryOperation::Revert)
    }

    fn begin_destructive(
        &mut self,
        path: WorkspacePath,
        operation: RepositoryOperation,
    ) -> Option<(i64, i64, u64, u64)> {
        if self.mutation_in_flight() || self.diff_loading {
            return None;
        }
        let workspace_id = self.workspace_id?;
        let status = self.status.as_ref()?;
        let binding_id = status.binding_id.0;
        let binding_revision = status.binding_revision;
        let request_id = self.next_request();
        self.pending_paths.insert(path);
        self.activity = Some(RepositoryActivity {
            operation,
            request_id: Some(request_id),
        });
        self.error = None;
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

    pub(crate) fn cached_diff(
        &self,
        side: VcsDiffSide,
        path: Option<&WorkspacePath>,
    ) -> Option<Arc<VcsDiff>> {
        self.diffs.get(&(side, path.cloned())).cloned()
    }

    pub(crate) fn apply_status_result(
        &mut self,
        workspace_id: i64,
        request_id: u64,
        result: Result<Option<VcsStatus>, String>,
    ) -> bool {
        if self.workspace_id != Some(workspace_id)
            || self.activity.and_then(|activity| activity.request_id) != Some(request_id)
        {
            return false;
        }
        self.activity = None;
        self.load_state = RepositoryLoadState::Loaded;
        self.pending_paths.clear();
        match result {
            Ok(status) => {
                let mut rebuild_rows = false;
                let stale = status.as_ref().is_some_and(|next| {
                    self.status.as_ref().is_some_and(|current| {
                        current.binding_id == next.binding_id
                            && next.binding_revision < current.binding_revision
                    })
                });
                if !stale {
                    let diff_identity_unchanged = self
                        .status
                        .as_ref()
                        .zip(status.as_ref())
                        .is_some_and(|(current, next)| {
                            current.binding_id == next.binding_id
                                && current.binding_revision == next.binding_revision
                                && current.workspace_revision == next.workspace_revision
                                && current.head_oid == next.head_oid
                                && current.entries == next.entries
                        });
                    if !diff_identity_unchanged {
                        self.diffs.clear();
                    }
                    self.status = status;
                    rebuild_rows = !diff_identity_unchanged;
                }
                self.error = None;
                if rebuild_rows {
                    self.rebuild_visible_rows();
                }
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
        if self.activity.and_then(|activity| activity.request_id) == Some(request_id) {
            self.activity = None;
            self.load_state = RepositoryLoadState::Loaded;
            self.pending_paths.clear();
            self.error = Some(classify_failure(message.into()));
        }
    }

    pub(crate) fn set_error(&mut self, message: impl Into<String>) {
        self.error = Some(classify_failure(message.into()));
    }

    pub(crate) fn status(&self) -> Option<&VcsStatus> {
        self.status.as_ref()
    }

    pub(crate) fn set_observed_binding(&mut self, binding: Option<RepositoryBinding>) {
        if binding.is_some() {
            self.observed_binding = binding;
        }
    }

    pub(crate) fn observed_binding(&self) -> Option<&RepositoryBinding> {
        self.observed_binding.as_ref()
    }

    pub(crate) fn repair_target(&self) -> Option<(i64, u64)> {
        self.status
            .as_ref()
            .map(|status| (status.binding_id.0, status.binding_revision))
            .or_else(|| {
                self.observed_binding
                    .as_ref()
                    .map(|binding| (binding.id.0, binding.revision))
            })
    }

    pub(crate) fn rows(&self) -> Arc<[RepositoryRow]> {
        self.visible_rows.clone()
    }

    pub(crate) fn set_filter(&mut self, filter: impl Into<String>) {
        let filter = filter.into().trim().to_lowercase();
        if self.filter == filter {
            return;
        }
        self.filter = filter;
        self.rebuild_visible_rows();
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

    pub(crate) fn restore_selected_path(&mut self, path: Option<WorkspacePath>) {
        self.selected_path = path;
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

    pub(crate) fn select_edge(&mut self, last: bool) -> Option<WorkspacePath> {
        let path = if last {
            self.visible_rows.iter().rev().find_map(|row| match row {
                RepositoryRow::Entry { entry, .. } => Some(entry.path.clone()),
                RepositoryRow::Section { .. } | RepositoryRow::Folder { .. } => None,
            })
        } else {
            self.visible_rows.iter().find_map(|row| match row {
                RepositoryRow::Entry { entry, .. } => Some(entry.path.clone()),
                RepositoryRow::Section { .. } | RepositoryRow::Folder { .. } => None,
            })
        }?;
        self.selected_path = Some(path.clone());
        Some(path)
    }

    pub(crate) fn move_conflict_selection(&mut self, delta: isize) -> Option<WorkspacePath> {
        let paths = self
            .status
            .iter()
            .flat_map(|status| &status.entries)
            .filter(|entry| entry.stage == VcsStageState::Conflict)
            .map(|entry| entry.path.clone())
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

    pub(crate) fn conflict_count(&self) -> usize {
        self.status
            .iter()
            .flat_map(|status| &status.entries)
            .filter(|entry| entry.stage == VcsStageState::Conflict)
            .count()
    }

    pub(crate) fn selected_row_index(&self) -> Option<usize> {
        let selected = self.selected_path.as_ref()?;
        self.visible_rows.iter().position(
            |row| matches!(row, RepositoryRow::Entry { entry, .. } if &entry.path == selected),
        )
    }

    pub(crate) fn loading(&self) -> bool {
        self.shared_operation.is_some()
            || self.activity.is_some_and(|activity| {
                activity.operation != RepositoryOperation::Refresh || !self.loaded()
            })
    }

    fn mutation_in_flight(&self) -> bool {
        self.activity.is_some() || self.shared_operation.is_some()
    }

    pub(crate) fn loaded(&self) -> bool {
        self.load_state == RepositoryLoadState::Loaded
    }

    pub(crate) fn error(&self) -> Option<&RepositoryFailure> {
        self.error.as_ref()
    }

    pub(crate) fn operation(&self) -> Option<RepositoryOperation> {
        self.activity
            .map(|activity| activity.operation)
            .filter(|operation| *operation != RepositoryOperation::Refresh || !self.loaded())
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
                (RepositorySection::Staged, Vec::new()),
                (RepositorySection::Changes, Vec::new()),
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
            if !self.filter.is_empty() && !entry.path.0.to_lowercase().contains(&self.filter) {
                continue;
            }
            let index = if self.grouping == RepositoryGrouping::Staging {
                if entry.stage == VcsStageState::Staged {
                    0
                } else {
                    1
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
            entries.sort_by(|left, right| {
                let left_conflicted =
                    left.stage == VcsStageState::Conflict || left.state == VcsFileState::Unmerged;
                let right_conflicted =
                    right.stage == VcsStageState::Conflict || right.state == VcsFileState::Unmerged;
                right_conflicted
                    .cmp(&left_conflicted)
                    .then_with(|| match self.sort {
                        RepositorySort::Path => left.path.0.cmp(&right.path.0),
                        RepositorySort::FileName => file_name(&left.path.0)
                            .cmp(file_name(&right.path.0))
                            .then_with(|| left.path.0.cmp(&right.path.0)),
                    })
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
        if self.selected_path.as_ref().is_none_or(|selected| {
            !rows.iter().any(
                |row| matches!(row, RepositoryRow::Entry { entry, .. } if &entry.path == selected),
            )
        }) {
            self.selected_path = rows.iter().find_map(|row| match row {
                RepositoryRow::Entry { entry, .. } => Some(entry.path.clone()),
                RepositoryRow::Section { .. } | RepositoryRow::Folder { .. } => None,
            });
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
                section: RepositorySection::Staged,
                count: 1
            }
        ));
        assert!(matches!(
            rows[2],
            RepositoryRow::Section {
                section: RepositorySection::Changes,
                count: 3
            }
        ));
        assert!(matches!(
            &rows[3],
            RepositoryRow::Entry { entry, .. } if entry.path.0 == "conflict.sql"
        ));
    }

    #[test]
    fn change_filter_keeps_navigation_inside_matching_paths() {
        let mut projection = RepositoryProjection::new(Some(7));
        let (_, request_id) = projection.begin_refresh().unwrap();
        projection.apply_status_result(
            7,
            request_id,
            Ok(Some(status(serde_json::json!([
                entry("queries/accounts.sql", "modified", "unstaged"),
                entry("queries/orders.sql", "modified", "unstaged")
            ])))),
        );

        projection.set_filter("orders");

        assert_eq!(
            projection.selected_path(),
            Some(&WorkspacePath::new("queries/orders.sql").unwrap())
        );
        assert_eq!(
            projection
                .rows()
                .iter()
                .filter(|row| matches!(row, RepositoryRow::Entry { .. }))
                .count(),
            1
        );
        projection.set_filter("");
        assert_eq!(
            projection
                .rows()
                .iter()
                .filter(|row| matches!(row, RepositoryRow::Entry { .. }))
                .count(),
            2
        );
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
    fn passive_refresh_keeps_the_loaded_panel_visually_stable() {
        let mut projection = RepositoryProjection::new(Some(12));
        let (_, initial_request) = projection.begin_refresh().unwrap();
        assert!(projection.apply_status_result(12, initial_request, Ok(None)));

        let _ = projection.begin_refresh().unwrap();

        assert!(!projection.loading());
        assert_eq!(projection.operation(), None);
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
            .begin_path_update(
                vec![WorkspacePath::new("query.sql").unwrap()],
                RepositoryIndexAction::Stage,
            )
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
    fn conflict_navigation_only_visits_conflicted_paths() {
        let mut projection = RepositoryProjection::new(Some(7));
        let (_, request_id) = projection.begin_refresh().unwrap();
        projection.apply_status_result(
            7,
            request_id,
            Ok(Some(status(serde_json::json!([
                entry("a.sql", "unmerged", "conflict"),
                entry("ordinary.sql", "modified", "unstaged"),
                entry("z.sql", "unmerged", "conflict")
            ])))),
        );

        assert_eq!(projection.conflict_count(), 2);
        assert_eq!(projection.move_conflict_selection(1).unwrap().0, "z.sql");
        assert_eq!(projection.move_conflict_selection(1).unwrap().0, "a.sql");
        assert_eq!(projection.move_conflict_selection(-1).unwrap().0, "z.sql");
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
            base_revision: None,
            target_revision: None,
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
        assert!(projection
            .cached_diff(VcsDiffSide::IndexToWorktree, Some(&path))
            .is_some());

        let (_, refresh_request) = projection.begin_refresh().unwrap();
        let unchanged = status(serde_json::json!([entry(
            "query.sql",
            "modified",
            "unstaged"
        )]));
        projection.apply_status_result(7, refresh_request, Ok(Some(unchanged)));
        assert!(projection
            .cached_diff(VcsDiffSide::IndexToWorktree, Some(&path))
            .is_some());

        let (_, refresh_request) = projection.begin_refresh().unwrap();
        let mut changed = status(serde_json::json!([entry(
            "query.sql",
            "modified",
            "unstaged"
        )]));
        changed.head_oid = Some("new-head".into());
        projection.apply_status_result(7, refresh_request, Ok(Some(changed)));
        assert!(projection
            .cached_diff(VcsDiffSide::IndexToWorktree, Some(&path))
            .is_none());
    }

    #[test]
    fn a_new_history_search_supersedes_and_rejects_the_stale_page() {
        let mut projection = RepositoryProjection::new(Some(7));
        let stale = projection.begin_history_load("old".into(), false).unwrap();
        let current = projection.begin_history_load("new".into(), false).unwrap();
        let page = VcsHistoryPage {
            commits: Vec::new(),
            next_cursor: None,
        };
        assert!(!projection.apply_history(7, stale, Ok(page.clone()), false));
        assert!(projection.history_loading());
        assert!(projection.apply_history(7, current, Ok(page), false));
        assert!(!projection.history_loading());
    }

    #[test]
    fn shared_mutation_blocks_local_mutations_until_terminal_event() {
        let mut projection = RepositoryProjection::new(Some(7));
        let (_, request_id) = projection.begin_refresh().unwrap();
        projection.apply_status_result(
            7,
            request_id,
            Ok(Some(status(serde_json::json!([entry(
                "query.sql",
                "modified",
                "staged"
            )])))),
        );

        projection.apply_shared_operation(
            41,
            sift_protocol::VcsAction::Commit,
            sift_protocol::RepositoryOperationPhase::Started,
        );
        assert_eq!(
            projection.shared_operation(),
            Some(SharedRepositoryOperation {
                actor_principal_id: 41,
                action: sift_protocol::VcsAction::Commit,
            })
        );
        assert!(projection.loading());
        assert!(projection.begin_commit().is_none());

        projection.apply_shared_operation(
            41,
            sift_protocol::VcsAction::Commit,
            sift_protocol::RepositoryOperationPhase::Succeeded,
        );
        assert!(projection.shared_operation().is_none());
        assert!(!projection.loading());
        assert!(projection.begin_commit().is_some());
    }
}
