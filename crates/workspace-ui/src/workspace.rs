//! Desktop projection of the server-authoritative virtual workspace.
//!
//! This is deliberately separate from the Git projection: workspace nodes and
//! room documents are canonical, while the filesystem and repository are
//! derived projections with their own dirty states.

use std::collections::HashSet;

use sift_api_types::WorkspaceTreeResponse;
use sift_protocol::{
    ProjectionBinding, ReconcilePlan, ReconcileState, WorkspaceCheckpoint, WorkspaceNode,
    WorkspaceNodeId, WorkspaceNodeKind, WorkspacePath, WorkspaceRevision,
};

#[derive(Debug, Clone)]
pub struct WorkspaceFilesSnapshot {
    pub tree: WorkspaceTreeResponse,
    pub projection: Option<ProjectionBinding>,
    pub reconcile_plan: Option<ReconcilePlan>,
    pub checkpoints: Vec<WorkspaceCheckpoint>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceFileRow {
    Node {
        node: WorkspaceNode,
        depth: usize,
        expanded: bool,
    },
}

#[derive(Debug, Default)]
pub struct WorkspaceFilesProjection {
    workspace_id: Option<i64>,
    request_generation: u64,
    active_request: Option<u64>,
    loading: bool,
    mutation_pending: bool,
    snapshot: Option<WorkspaceFilesSnapshot>,
    expanded: HashSet<WorkspaceNodeId>,
    selected: Option<WorkspaceNodeId>,
    error: Option<String>,
}

impl WorkspaceFilesProjection {
    pub fn new(workspace_id: Option<i64>) -> Self {
        Self {
            workspace_id,
            ..Self::default()
        }
    }

    pub fn select_workspace(&mut self, workspace_id: Option<i64>) {
        if self.workspace_id == workspace_id {
            return;
        }
        self.workspace_id = workspace_id;
        self.request_generation = self.request_generation.saturating_add(1);
        self.active_request = None;
        self.loading = false;
        self.mutation_pending = false;
        self.snapshot = None;
        self.expanded.clear();
        self.selected = None;
        self.error = None;
    }

    pub fn begin_load(&mut self) -> Option<(i64, u64)> {
        let workspace_id = self.workspace_id?;
        if self.loading || self.mutation_pending {
            return None;
        }
        self.request_generation = self.request_generation.saturating_add(1);
        let request_id = self.request_generation;
        self.active_request = Some(request_id);
        self.loading = true;
        self.error = None;
        Some((workspace_id, request_id))
    }

    pub fn apply_load(
        &mut self,
        workspace_id: i64,
        request_id: u64,
        result: Result<WorkspaceFilesSnapshot, String>,
    ) -> bool {
        if self.workspace_id != Some(workspace_id) || self.active_request != Some(request_id) {
            return false;
        }
        self.active_request = None;
        self.loading = false;
        match result {
            Ok(snapshot) => {
                self.expanded.extend(
                    snapshot
                        .tree
                        .nodes
                        .iter()
                        .filter(|node| node.kind == WorkspaceNodeKind::Folder)
                        .map(|node| node.id),
                );
                if self.selected.is_some_and(|selected| {
                    !snapshot.tree.nodes.iter().any(|node| node.id == selected)
                }) {
                    self.selected = None;
                }
                self.snapshot = Some(snapshot);
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
        true
    }

    pub fn begin_mutation(&mut self) -> bool {
        if self.workspace_id.is_none() || self.loading || self.mutation_pending {
            return false;
        }
        self.mutation_pending = true;
        self.error = None;
        true
    }

    pub fn finish_mutation(&mut self, result: Result<(), String>) {
        self.mutation_pending = false;
        if let Err(error) = result {
            self.error = Some(error);
        }
    }

    pub fn loading(&self) -> bool {
        self.loading
    }

    pub fn mutation_pending(&self) -> bool {
        self.mutation_pending
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn snapshot(&self) -> Option<&WorkspaceFilesSnapshot> {
        self.snapshot.as_ref()
    }

    pub fn revision(&self) -> Option<WorkspaceRevision> {
        self.snapshot
            .as_ref()
            .map(|snapshot| snapshot.tree.workspace.revision)
    }

    pub fn selected_node(&self) -> Option<&WorkspaceNode> {
        let selected = self.selected?;
        self.snapshot
            .as_ref()?
            .tree
            .nodes
            .iter()
            .find(|node| node.id == selected)
    }

    pub fn select_node(&mut self, node: WorkspaceNodeId) {
        self.selected = Some(node);
    }

    pub fn toggle_folder(&mut self, node: WorkspaceNodeId) {
        if !self.expanded.remove(&node) {
            self.expanded.insert(node);
        }
    }

    pub fn rows(&self) -> Vec<WorkspaceFileRow> {
        let Some(snapshot) = &self.snapshot else {
            return Vec::new();
        };
        let mut rows = Vec::with_capacity(snapshot.tree.nodes.len());
        let mut visited = HashSet::new();
        self.append_children(None, 0, &snapshot.tree.nodes, &mut visited, &mut rows);
        // The server rejects missing parents, but surface an orphan at the
        // root if damaged metadata ever reaches the client. Descendants of a
        // collapsed folder are deliberately absent here.
        let mut remaining = snapshot
            .tree
            .nodes
            .iter()
            .filter(|node| !visited.contains(&node.id))
            .filter(|node| {
                node.parent_id.is_some_and(|parent_id| {
                    !snapshot
                        .tree
                        .nodes
                        .iter()
                        .any(|candidate| candidate.id == parent_id)
                })
            })
            .collect::<Vec<_>>();
        Self::sort_siblings(&mut remaining);
        rows.extend(remaining.into_iter().map(|node| WorkspaceFileRow::Node {
            node: node.clone(),
            depth: node.path.0.matches('/').count(),
            expanded: self.expanded.contains(&node.id),
        }));
        rows
    }

    pub fn workspace_tree_dirty(&self) -> bool {
        self.snapshot
            .as_ref()
            .and_then(|snapshot| {
                snapshot
                    .projection
                    .as_ref()
                    .map(|projection| (snapshot, projection))
            })
            .is_some_and(|(snapshot, projection)| {
                projection.last_workspace_revision != Some(snapshot.tree.workspace.revision)
            })
    }

    pub fn projection_dirty(&self) -> bool {
        self.snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.reconcile_plan.as_ref())
            .is_some_and(|plan| {
                plan.entries
                    .iter()
                    .any(|entry| entry.state != ReconcileState::Unchanged)
            })
    }

    pub fn reconcile_summary(&self) -> (usize, usize, usize, usize) {
        let Some(plan) = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.reconcile_plan.as_ref())
        else {
            return (0, 0, 0, 0);
        };
        plan.entries.iter().fold((0, 0, 0, 0), |mut counts, entry| {
            match entry.state {
                ReconcileState::WorkspaceOnly => counts.0 += 1,
                ReconcileState::ProjectionOnly => counts.1 += 1,
                ReconcileState::BothChanged => counts.2 += 1,
                ReconcileState::Unchanged => counts.3 += 1,
                ReconcileState::Renamed | ReconcileState::Deleted => counts.2 += 1,
            }
            counts
        })
    }

    fn append_children(
        &self,
        parent_id: Option<WorkspaceNodeId>,
        depth: usize,
        nodes: &[WorkspaceNode],
        visited: &mut HashSet<WorkspaceNodeId>,
        rows: &mut Vec<WorkspaceFileRow>,
    ) {
        let mut children = nodes
            .iter()
            .filter(|node| node.parent_id == parent_id)
            .collect::<Vec<_>>();
        Self::sort_siblings(&mut children);
        for node in children {
            if !visited.insert(node.id) {
                continue;
            }
            rows.push(WorkspaceFileRow::Node {
                node: node.clone(),
                depth,
                expanded: self.expanded.contains(&node.id),
            });
            if node.kind == WorkspaceNodeKind::Folder && self.expanded.contains(&node.id) {
                self.append_children(Some(node.id), depth + 1, nodes, visited, rows);
            }
        }
    }

    fn sort_siblings(nodes: &mut Vec<&WorkspaceNode>) {
        nodes.sort_by(|left, right| {
            let left_folder = left.kind == WorkspaceNodeKind::Folder;
            let right_folder = right.kind == WorkspaceNodeKind::Folder;
            right_folder
                .cmp(&left_folder)
                .then_with(|| left.path.0.cmp(&right.path.0))
        });
    }
}

pub fn child_path(parent: Option<&WorkspaceNode>, name: &str) -> Result<WorkspacePath, String> {
    let name = name.trim().trim_matches('/');
    let path = parent.map_or_else(
        || name.to_owned(),
        |parent| format!("{}/{}", parent.path.0, name),
    );
    WorkspacePath::new(path).map_err(str::to_owned)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use sift_protocol::{Workspace, WorkspaceCapabilities, WorkspaceId};

    use super::*;

    fn node(id: i64, parent_id: Option<i64>, path: &str, kind: WorkspaceNodeKind) -> WorkspaceNode {
        WorkspaceNode {
            id: WorkspaceNodeId(id),
            workspace_id: WorkspaceId(1),
            parent_id: parent_id.map(WorkspaceNodeId),
            path: WorkspacePath::new(path).unwrap(),
            kind,
            document_id: None,
            artifact_id: None,
            revision: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn snapshot(nodes: Vec<WorkspaceNode>) -> WorkspaceFilesSnapshot {
        WorkspaceFilesSnapshot {
            tree: WorkspaceTreeResponse {
                workspace: Workspace {
                    id: WorkspaceId(1),
                    room_id: 1,
                    name: "demo".into(),
                    revision: WorkspaceRevision(4),
                    capabilities: WorkspaceCapabilities {
                        virtual_tree: true,
                        filesystem_projection: true,
                        git: true,
                        git_network: false,
                        scheduling: false,
                        transfer_recipes: false,
                    },
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                },
                nodes,
            },
            projection: None,
            reconcile_plan: None,
            checkpoints: Vec::new(),
        }
    }

    #[test]
    fn rows_preserve_node_identity_and_hide_collapsed_descendants() {
        let mut projection = WorkspaceFilesProjection::new(Some(1));
        let (_, request) = projection.begin_load().unwrap();
        projection.apply_load(
            1,
            request,
            Ok(snapshot(vec![
                node(1, None, "queries", WorkspaceNodeKind::Folder),
                node(2, Some(1), "queries/a.sql", WorkspaceNodeKind::SqlDocument),
            ])),
        );
        assert_eq!(projection.rows().len(), 2);
        projection.toggle_folder(WorkspaceNodeId(1));
        assert_eq!(projection.rows().len(), 1);
        projection.toggle_folder(WorkspaceNodeId(1));
        assert_eq!(projection.rows().len(), 2);
    }

    #[test]
    fn stale_load_cannot_replace_new_workspace() {
        let mut projection = WorkspaceFilesProjection::new(Some(1));
        let (_, request) = projection.begin_load().unwrap();
        projection.select_workspace(Some(2));
        assert!(!projection.apply_load(1, request, Ok(snapshot(Vec::new()))));
        assert!(projection.snapshot().is_none());
    }

    #[test]
    fn child_paths_remain_normalized_and_confined() {
        let parent = node(1, None, "queries", WorkspaceNodeKind::Folder);
        assert_eq!(
            child_path(Some(&parent), "a.sql").unwrap().0,
            "queries/a.sql"
        );
        assert!(child_path(Some(&parent), "../a.sql").is_err());
    }
}
