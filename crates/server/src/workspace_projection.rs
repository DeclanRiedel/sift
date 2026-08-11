//! Deterministic, read-only projection reconciliation.

use std::collections::{BTreeMap, BTreeSet};

use sift_metadata::ProjectionFileState;
use sift_protocol::{
    ProjectionBinding, ReconcileEntry, ReconcilePlan, ReconcileState, WorkspaceNodeId,
    WorkspacePath, WorkspaceRevision,
};

use crate::workspace_adapter::ProjectionSnapshot;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceProjectionFile {
    pub node_id: WorkspaceNodeId,
    pub path: WorkspacePath,
    pub digest: String,
    pub bytes: Vec<u8>,
}

pub fn reconcile_plan(
    binding: &ProjectionBinding,
    workspace_revision: WorkspaceRevision,
    baseline: &[ProjectionFileState],
    workspace: &[WorkspaceProjectionFile],
    projection: &ProjectionSnapshot,
) -> ReconcilePlan {
    let baseline_by_path = baseline
        .iter()
        .map(|file| (file.path.0.clone(), file))
        .collect::<BTreeMap<_, _>>();
    let workspace_by_path = workspace
        .iter()
        .map(|file| (file.path.0.clone(), file))
        .collect::<BTreeMap<_, _>>();
    let projection_by_path = projection
        .files
        .iter()
        .filter(|file| is_sql_path(&file.path))
        .map(|file| (file.path.0.clone(), file))
        .collect::<BTreeMap<_, _>>();
    let current_path_by_node = workspace
        .iter()
        .map(|file| (file.node_id.0, file.path.0.as_str()))
        .collect::<BTreeMap<_, _>>();

    let mut paths = BTreeSet::new();
    paths.extend(baseline_by_path.keys().cloned());
    paths.extend(workspace_by_path.keys().cloned());
    paths.extend(projection_by_path.keys().cloned());

    let mut renamed_from = BTreeSet::new();
    let mut entries = Vec::with_capacity(paths.len());
    for path in &paths {
        let base = baseline_by_path.get(path).copied();
        let current = workspace_by_path.get(path).copied();
        let projected = projection_by_path.get(path).copied();
        if let Some(base) = base {
            if let Some(node_id) = base.node_id {
                if let Some(new_path) = current_path_by_node.get(&node_id.0) {
                    if *new_path != path && !workspace_by_path.contains_key(path) {
                        renamed_from.insert(path.clone());
                        continue;
                    }
                }
            }
        }
        entries.push(entry_for(path, base, current, projected));
    }

    for old_path in renamed_from {
        let base = baseline_by_path[&old_path];
        let node_id = base.node_id.expect("rename candidates have a node id");
        let current = workspace
            .iter()
            .find(|file| file.node_id == node_id)
            .expect("rename candidates have a current node");
        let projected = projection_by_path.get(&old_path).copied();
        entries.retain(|entry| entry.path != current.path);
        entries.push(ReconcileEntry {
            node_id: Some(node_id),
            path: current.path.clone(),
            previous_path: Some(WorkspacePath(old_path)),
            state: if projected.map(|file| file.digest.as_str())
                == base.projection_digest.as_deref()
            {
                ReconcileState::Renamed
            } else {
                ReconcileState::BothChanged
            },
            workspace_digest: Some(current.digest.clone()),
            projection_digest: projected.map(|file| file.digest.clone()),
        });
    }
    entries.sort_by(|left, right| left.path.0.cmp(&right.path.0));
    ReconcilePlan {
        binding_id: binding.id,
        binding_revision: binding.revision,
        workspace_revision,
        adapter_generation: binding.adapter_generation.clone(),
        entries,
        truncated: false,
    }
}

fn is_sql_path(path: &WorkspacePath) -> bool {
    path.0
        .rsplit_once('.')
        .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("sql"))
}

fn entry_for(
    path: &str,
    base: Option<&ProjectionFileState>,
    workspace: Option<&WorkspaceProjectionFile>,
    projection: Option<&crate::workspace_adapter::ProjectionFile>,
) -> ReconcileEntry {
    let workspace_digest = workspace.map(|file| file.digest.clone());
    let projection_digest = projection.map(|file| file.digest.clone());
    let state = match base {
        None => match (&workspace_digest, &projection_digest) {
            (Some(left), Some(right)) if left == right => ReconcileState::Unchanged,
            (Some(_), None) => ReconcileState::WorkspaceOnly,
            (None, Some(_)) => ReconcileState::ProjectionOnly,
            (Some(_), Some(_)) => ReconcileState::BothChanged,
            (None, None) => ReconcileState::Unchanged,
        },
        Some(base) => {
            let workspace_changed = workspace_digest.as_deref() != base.workspace_digest.as_deref();
            let projection_changed =
                projection_digest.as_deref() != base.projection_digest.as_deref();
            match (workspace_changed, projection_changed) {
                (false, false) => ReconcileState::Unchanged,
                (true, false) if workspace_digest.is_none() => ReconcileState::Deleted,
                (false, true) if projection_digest.is_none() => ReconcileState::Deleted,
                (true, false) => ReconcileState::WorkspaceOnly,
                (false, true) => ReconcileState::ProjectionOnly,
                (true, true) if workspace_digest == projection_digest => ReconcileState::Unchanged,
                (true, true) => ReconcileState::BothChanged,
            }
        }
    };
    ReconcileEntry {
        node_id: workspace
            .map(|file| file.node_id)
            .or_else(|| base.and_then(|file| file.node_id)),
        path: WorkspacePath(path.to_string()),
        previous_path: None,
        state,
        workspace_digest,
        projection_digest,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_protocol::{ProjectionBindingId, ProjectionHealth, ProjectionMode};

    fn binding() -> ProjectionBinding {
        ProjectionBinding {
            id: ProjectionBindingId(1),
            workspace_id: sift_protocol::WorkspaceId(1),
            adapter_id: "sift/filesystem".into(),
            mode: ProjectionMode::ReadWrite,
            last_workspace_revision: Some(WorkspaceRevision(1)),
            adapter_generation: "filesystem-v1".into(),
            health: ProjectionHealth::Ready,
            revision: 2,
        }
    }

    #[test]
    fn planning_is_sorted_and_does_not_choose_both_changed() {
        let baseline = vec![ProjectionFileState {
            node_id: Some(WorkspaceNodeId(1)),
            path: WorkspacePath("b.sql".into()),
            workspace_digest: Some("old".into()),
            projection_digest: Some("old".into()),
        }];
        let workspace = vec![WorkspaceProjectionFile {
            node_id: WorkspaceNodeId(1),
            path: WorkspacePath("b.sql".into()),
            digest: "workspace".into(),
            bytes: vec![],
        }];
        let projection = ProjectionSnapshot {
            files: vec![crate::workspace_adapter::ProjectionFile {
                path: WorkspacePath("b.sql".into()),
                digest: "projection".into(),
                bytes: vec![],
            }],
            total_bytes: 0,
        };
        let plan = reconcile_plan(
            &binding(),
            WorkspaceRevision(2),
            &baseline,
            &workspace,
            &projection,
        );
        assert_eq!(plan.entries[0].state, ReconcileState::BothChanged);
        assert_eq!(plan.binding_revision, 2);
    }
}
