use std::collections::HashMap;

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};
use sift_protocol::{
    Workspace, WorkspaceCapabilities, WorkspaceCheckpoint, WorkspaceCheckpointId,
    WorkspaceCheckpointReason, WorkspaceId, WorkspaceNode, WorkspaceNodeId, WorkspaceNodeKind,
    WorkspacePath, WorkspaceRevision,
};
use unicode_normalization::UnicodeNormalization;

use crate::{
    allocate_document_id, now_text, parse_time_sql, rows, DocumentId, MetadataError, MetadataStore,
    NewWorkspaceCheckpoint, NewWorkspaceNode, PrincipalId, Result, RoomId, WorkspaceBatchMutation,
    WorkspaceCheckpointCapture, WorkspaceCheckpointNode, WorkspaceRecord, WorkspaceRestorePlan,
};

const MAX_WORKSPACE_NAME_BYTES: usize = 128;
const MAX_CHECKPOINT_NAME_BYTES: usize = 128;
const MAX_CHECKPOINTS_PER_PAGE: u32 = 100;
const MAX_WORKSPACES_PER_ROOM: i64 = 32;
const MAX_NODES_PER_WORKSPACE: i64 = 10_000;
const MAX_BATCH_MUTATIONS: usize = 100;
const MAX_CHECKPOINTS_PER_WORKSPACE: i64 = 200;
const MAX_CHECKPOINT_CAPTURE_BYTES: usize = 64 * 1024 * 1024;
const MAX_CHECKPOINT_RETAINED_BYTES: i64 = 256 * 1024 * 1024;

impl MetadataStore {
    pub fn create_workspace(
        &self,
        room: RoomId,
        actor: PrincipalId,
        name: &str,
    ) -> Result<WorkspaceRecord> {
        validate_workspace_name(name)?;
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_room_access(&tx, room, actor, true)?;
        let workspace_count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM workspace WHERE room_id = ?1",
            params![room.0],
            |row| row.get(0),
        )?;
        if workspace_count >= MAX_WORKSPACES_PER_ROOM {
            return Err(MetadataError::WorkspaceLimitReached);
        }
        tx.execute(
            "INSERT INTO workspace (room_id, name, revision, created_at, updated_at)
             VALUES (?1, ?2, 1, ?3, ?3)",
            params![room.0, name, now],
        )?;
        let id = WorkspaceId(tx.last_insert_rowid());
        let workspace = workspace_by_id_locked(&tx, id)?;
        tx.commit()?;
        Ok(workspace)
    }

    pub fn list_workspaces_for_principal(
        &self,
        room: RoomId,
        principal: PrincipalId,
    ) -> Result<Vec<WorkspaceRecord>> {
        let conn = self.conn()?;
        ensure_room_access(&conn, room, principal, false)?;
        let mut stmt = conn.prepare(
            "SELECT id, room_id, name, revision, created_at, updated_at
             FROM workspace WHERE room_id = ?1 ORDER BY id",
        )?;
        let workspaces = rows(stmt.query_map(params![room.0], workspace_from_row)?)?;
        Ok(workspaces)
    }

    pub fn get_workspace_for_principal(
        &self,
        id: WorkspaceId,
        principal: PrincipalId,
        writable: bool,
    ) -> Result<WorkspaceRecord> {
        let conn = self.conn()?;
        let workspace = workspace_by_id_locked(&conn, id)?;
        ensure_room_access(&conn, workspace.room_id, principal, writable)?;
        Ok(workspace)
    }

    pub fn update_workspace(
        &self,
        id: WorkspaceId,
        actor: PrincipalId,
        expected: WorkspaceRevision,
        name: &str,
    ) -> Result<WorkspaceRecord> {
        validate_workspace_name(name)?;
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let workspace = workspace_by_id_locked(&tx, id)?;
        ensure_room_access(&tx, workspace.room_id, actor, true)?;
        ensure_workspace_revision(&workspace, expected)?;
        tx.execute(
            "UPDATE workspace
             SET name = ?1, revision = revision + 1, updated_at = ?2
             WHERE id = ?3",
            params![name, now, id.0],
        )?;
        let workspace = workspace_by_id_locked(&tx, id)?;
        tx.commit()?;
        Ok(workspace)
    }

    pub fn delete_workspace(
        &self,
        id: WorkspaceId,
        actor: PrincipalId,
        expected: WorkspaceRevision,
    ) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let workspace = workspace_by_id_locked(&tx, id)?;
        ensure_room_access(&tx, workspace.room_id, actor, true)?;
        ensure_workspace_revision(&workspace, expected)?;
        tx.execute("DELETE FROM workspace WHERE id = ?1", params![id.0])?;
        delete_unreferenced_content_blobs(&tx)?;
        tx.commit()?;
        Ok(())
    }

    pub fn list_workspace_nodes_for_principal(
        &self,
        workspace: WorkspaceId,
        principal: PrincipalId,
    ) -> Result<Vec<WorkspaceNode>> {
        let conn = self.conn()?;
        let workspace_row = workspace_by_id_locked(&conn, workspace)?;
        ensure_room_access(&conn, workspace_row.room_id, principal, false)?;
        workspace_nodes_locked(&conn, workspace)
    }

    pub fn workspace_for_node(
        &self,
        node: WorkspaceNodeId,
        principal: PrincipalId,
    ) -> Result<WorkspaceRecord> {
        let conn = self.conn()?;
        let node = workspace_node_by_id_locked(&conn, node)?;
        let workspace = workspace_by_id_locked(&conn, node.workspace_id)?;
        ensure_room_access(&conn, workspace.room_id, principal, false)?;
        Ok(workspace)
    }

    pub fn workspace_subtree_document_ids(
        &self,
        node: WorkspaceNodeId,
        principal: PrincipalId,
    ) -> Result<Vec<DocumentId>> {
        let conn = self.conn()?;
        let node = workspace_node_by_id_locked(&conn, node)?;
        let workspace = workspace_by_id_locked(&conn, node.workspace_id)?;
        ensure_room_access(&conn, workspace.room_id, principal, true)?;
        Ok(subtree_nodes_locked(&conn, &node)?
            .into_iter()
            .filter_map(|node| node.document_id.map(DocumentId))
            .collect())
    }

    pub fn create_workspace_node(
        &self,
        workspace: WorkspaceId,
        actor: PrincipalId,
        expected: WorkspaceRevision,
        input: NewWorkspaceNode,
    ) -> Result<(WorkspaceRecord, WorkspaceNode)> {
        validate_workspace_node_input(&input)?;
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let workspace_row = workspace_by_id_locked(&tx, workspace)?;
        ensure_room_access(&tx, workspace_row.room_id, actor, true)?;
        ensure_workspace_revision(&workspace_row, expected)?;
        ensure_workspace_node_capacity(&tx, workspace)?;
        ensure_parent_matches_path(&tx, workspace, input.parent_id, &input.path, None)?;
        ensure_path_available(&tx, workspace, &input.path, &[])?;

        let document_id = match input.kind {
            WorkspaceNodeKind::Folder => None,
            WorkspaceNodeKind::SqlDocument => {
                let snapshot = input
                    .initial_snapshot
                    .as_ref()
                    .ok_or(MetadataError::InvalidWorkspaceNode)?;
                let version = input
                    .initial_snapshot_version
                    .as_ref()
                    .ok_or(MetadataError::InvalidWorkspaceNode)?;
                let position: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM document WHERE room_id = ?1",
                    params![workspace_row.room_id.0],
                    |row| row.get(0),
                )?;
                let document_id = allocate_document_id(&tx)?;
                tx.execute(
                    "INSERT INTO document
                     (id, room_id, kind, title, crdt_type, crdt_state, crdt_format_version,
                      snapshot_version, position, connection_profile_id, created_at, updated_at)
                     VALUES (?1, ?2, 'sql', ?3, 'loro', ?4, 1, ?5, ?6, NULL, ?7, ?7)",
                    params![
                        document_id.0,
                        workspace_row.room_id.0,
                        file_name(&input.path),
                        snapshot,
                        version,
                        position,
                        now,
                    ],
                )?;
                Some(document_id)
            }
            WorkspaceNodeKind::Artifact => return Err(MetadataError::InvalidWorkspaceNode),
        };

        tx.execute(
            "INSERT INTO workspace_node
             (workspace_id, parent_id, path, path_key, kind, document_id, revision,
              created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?7)",
            params![
                workspace.0,
                input.parent_id.map(|id| id.0),
                input.path.0,
                workspace_path_key(&input.path)?,
                node_kind_str(input.kind),
                document_id.map(|id| id.0),
                now,
            ],
        )?;
        let node_id = WorkspaceNodeId(tx.last_insert_rowid());
        bump_workspace_locked(&tx, workspace, &now)?;
        let node = workspace_node_by_id_locked(&tx, node_id)?;
        let workspace_row = workspace_by_id_locked(&tx, workspace)?;
        tx.commit()?;
        Ok((workspace_row, node))
    }

    pub fn move_workspace_node(
        &self,
        node: WorkspaceNodeId,
        actor: PrincipalId,
        expected: WorkspaceRevision,
        parent_id: Option<WorkspaceNodeId>,
        path: WorkspacePath,
    ) -> Result<(WorkspaceRecord, Vec<WorkspaceNode>)> {
        workspace_path_key(&path)?;
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = workspace_node_by_id_locked(&tx, node)?;
        let workspace = workspace_by_id_locked(&tx, current.workspace_id)?;
        ensure_room_access(&tx, workspace.room_id, actor, true)?;
        ensure_workspace_revision(&workspace, expected)?;
        if path == current.path && parent_id == current.parent_id {
            return Ok((workspace, vec![current]));
        }
        if path.0.starts_with(&(current.path.0.clone() + "/")) {
            return Err(MetadataError::InvalidWorkspacePath);
        }
        ensure_parent_matches_path(&tx, current.workspace_id, parent_id, &path, Some(node))?;

        let descendants = subtree_nodes_locked(&tx, &current)?;
        let descendant_ids = descendants.iter().map(|node| node.id.0).collect::<Vec<_>>();
        let replacements = descendants
            .iter()
            .map(|item| {
                let suffix = item
                    .path
                    .0
                    .strip_prefix(&current.path.0)
                    .unwrap_or_default();
                (item.id, WorkspacePath(format!("{}{}", path.0, suffix)))
            })
            .collect::<Vec<_>>();
        for (_, replacement) in &replacements {
            ensure_path_available(&tx, current.workspace_id, replacement, &descendant_ids)?;
        }
        for (id, replacement) in &replacements {
            tx.execute(
                "UPDATE workspace_node
                 SET path = ?1, path_key = ?2,
                     parent_id = CASE WHEN id = ?3 THEN ?4 ELSE parent_id END,
                     revision = revision + 1,
                     updated_at = ?5
                 WHERE id = ?3",
                params![
                    replacement.0,
                    workspace_path_key(replacement)?,
                    id.0,
                    parent_id.map(|id| id.0),
                    now
                ],
            )?;
        }
        if let Some(document) = current.document_id {
            tx.execute(
                "UPDATE document SET title = ?1, updated_at = ?2 WHERE id = ?3",
                params![file_name(&path), now, document],
            )?;
        }
        bump_workspace_locked(&tx, current.workspace_id, &now)?;
        let workspace = workspace_by_id_locked(&tx, current.workspace_id)?;
        let moved = replacements
            .into_iter()
            .map(|(id, _)| workspace_node_by_id_locked(&tx, id))
            .collect::<Result<Vec<_>>>()?;
        tx.commit()?;
        Ok((workspace, moved))
    }

    pub fn delete_workspace_node(
        &self,
        node: WorkspaceNodeId,
        actor: PrincipalId,
        expected: WorkspaceRevision,
    ) -> Result<WorkspaceRecord> {
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let node = workspace_node_by_id_locked(&tx, node)?;
        let workspace = workspace_by_id_locked(&tx, node.workspace_id)?;
        ensure_room_access(&tx, workspace.room_id, actor, true)?;
        ensure_workspace_revision(&workspace, expected)?;
        let documents = subtree_nodes_locked(&tx, &node)?
            .into_iter()
            .filter_map(|node| node.document_id)
            .collect::<Vec<_>>();
        tx.execute(
            "DELETE FROM workspace_node WHERE id = ?1",
            params![node.id.0],
        )?;
        for document in documents {
            tx.execute("DELETE FROM document WHERE id = ?1", params![document])?;
        }
        bump_workspace_locked(&tx, workspace.id, &now)?;
        let workspace = workspace_by_id_locked(&tx, workspace.id)?;
        tx.commit()?;
        Ok(workspace)
    }

    pub fn mutate_workspace_batch(
        &self,
        workspace: WorkspaceId,
        actor: PrincipalId,
        expected: WorkspaceRevision,
        mutations: Vec<WorkspaceBatchMutation>,
    ) -> Result<(WorkspaceRecord, Vec<WorkspaceNode>, Vec<DocumentId>)> {
        if mutations.is_empty() || mutations.len() > MAX_BATCH_MUTATIONS {
            return Err(MetadataError::InvalidWorkspaceBatch);
        }
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let workspace_row = workspace_by_id_locked(&tx, workspace)?;
        ensure_room_access(&tx, workspace_row.room_id, actor, true)?;
        ensure_workspace_revision(&workspace_row, expected)?;
        let mut removed_documents = Vec::new();

        for mutation in mutations {
            match mutation {
                WorkspaceBatchMutation::Create(input) => {
                    validate_workspace_node_input(&input)?;
                    ensure_workspace_node_capacity(&tx, workspace)?;
                    ensure_parent_matches_path(&tx, workspace, input.parent_id, &input.path, None)?;
                    ensure_path_available(&tx, workspace, &input.path, &[])?;
                    let document_id = match input.kind {
                        WorkspaceNodeKind::Folder => None,
                        WorkspaceNodeKind::SqlDocument => {
                            let snapshot = input
                                .initial_snapshot
                                .as_ref()
                                .ok_or(MetadataError::InvalidWorkspaceNode)?;
                            let version = input
                                .initial_snapshot_version
                                .as_ref()
                                .ok_or(MetadataError::InvalidWorkspaceNode)?;
                            let position: i64 = tx.query_row(
                                "SELECT COUNT(*) FROM document WHERE room_id = ?1",
                                params![workspace_row.room_id.0],
                                |row| row.get(0),
                            )?;
                            let document_id = allocate_document_id(&tx)?;
                            tx.execute(
                                "INSERT INTO document
                                 (id, room_id, kind, title, crdt_type, crdt_state,
                                  crdt_format_version, snapshot_version, position,
                                  connection_profile_id, created_at, updated_at)
                                 VALUES (?1, ?2, 'sql', ?3, 'loro', ?4, 1, ?5, ?6,
                                         NULL, ?7, ?7)",
                                params![
                                    document_id.0,
                                    workspace_row.room_id.0,
                                    file_name(&input.path),
                                    snapshot,
                                    version,
                                    position,
                                    now,
                                ],
                            )?;
                            Some(document_id)
                        }
                        WorkspaceNodeKind::Artifact => {
                            return Err(MetadataError::InvalidWorkspaceNode)
                        }
                    };
                    tx.execute(
                        "INSERT INTO workspace_node
                         (workspace_id, parent_id, path, path_key, kind, document_id, revision,
                          created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?7)",
                        params![
                            workspace.0,
                            input.parent_id.map(|id| id.0),
                            input.path.0,
                            workspace_path_key(&input.path)?,
                            node_kind_str(input.kind),
                            document_id.map(|id| id.0),
                            now,
                        ],
                    )?;
                }
                WorkspaceBatchMutation::Move {
                    node_id,
                    parent_id,
                    path,
                } => {
                    workspace_path_key(&path)?;
                    let current = workspace_node_by_id_locked(&tx, node_id)?;
                    if current.workspace_id != workspace {
                        return Err(MetadataError::WorkspaceNodeNotFound(node_id));
                    }
                    if path == current.path && parent_id == current.parent_id {
                        continue;
                    }
                    if path.0.starts_with(&(current.path.0.clone() + "/")) {
                        return Err(MetadataError::InvalidWorkspacePath);
                    }
                    ensure_parent_matches_path(&tx, workspace, parent_id, &path, Some(node_id))?;
                    let descendants = subtree_nodes_locked(&tx, &current)?;
                    let descendant_ids =
                        descendants.iter().map(|node| node.id.0).collect::<Vec<_>>();
                    let replacements = descendants
                        .iter()
                        .map(|item| {
                            let suffix = item
                                .path
                                .0
                                .strip_prefix(&current.path.0)
                                .unwrap_or_default();
                            (item.id, WorkspacePath(format!("{}{}", path.0, suffix)))
                        })
                        .collect::<Vec<_>>();
                    for (_, replacement) in &replacements {
                        ensure_path_available(&tx, workspace, replacement, &descendant_ids)?;
                    }
                    for (id, replacement) in replacements {
                        tx.execute(
                            "UPDATE workspace_node
                             SET path = ?1, path_key = ?2,
                                 parent_id = CASE WHEN id = ?3 THEN ?4 ELSE parent_id END,
                                 revision = revision + 1, updated_at = ?5
                             WHERE id = ?3",
                            params![
                                replacement.0,
                                workspace_path_key(&replacement)?,
                                id.0,
                                parent_id.map(|id| id.0),
                                now
                            ],
                        )?;
                    }
                    if let Some(document) = current.document_id {
                        tx.execute(
                            "UPDATE document SET title = ?1, updated_at = ?2 WHERE id = ?3",
                            params![file_name(&path), now, document],
                        )?;
                    }
                }
                WorkspaceBatchMutation::Delete { node_id } => {
                    let node = workspace_node_by_id_locked(&tx, node_id)?;
                    if node.workspace_id != workspace {
                        return Err(MetadataError::WorkspaceNodeNotFound(node_id));
                    }
                    let documents = subtree_nodes_locked(&tx, &node)?
                        .into_iter()
                        .filter_map(|node| node.document_id.map(DocumentId))
                        .collect::<Vec<_>>();
                    tx.execute(
                        "DELETE FROM workspace_node WHERE id = ?1",
                        params![node_id.0],
                    )?;
                    for document in documents {
                        tx.execute("DELETE FROM document WHERE id = ?1", params![document.0])?;
                        removed_documents.push(document);
                    }
                }
            }
        }

        bump_workspace_locked(&tx, workspace, &now)?;
        let workspace = workspace_by_id_locked(&tx, workspace)?;
        let nodes = workspace_nodes_locked(&tx, workspace.id)?;
        tx.commit()?;
        Ok((workspace, nodes, removed_documents))
    }

    pub fn create_workspace_checkpoint(
        &self,
        workspace: WorkspaceId,
        actor: PrincipalId,
        input: NewWorkspaceCheckpoint,
    ) -> Result<WorkspaceCheckpoint> {
        validate_checkpoint_input(&input)?;
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let workspace_row = workspace_by_id_locked(&tx, workspace)?;
        ensure_room_access(&tx, workspace_row.room_id, actor, true)?;
        ensure_workspace_revision(&workspace_row, input.expected_revision)?;
        let nodes = workspace_nodes_locked(&tx, workspace)?;
        let checkpoint_count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM workspace_checkpoint WHERE workspace_id = ?1",
            params![workspace.0],
            |row| row.get(0),
        )?;
        if checkpoint_count >= MAX_CHECKPOINTS_PER_WORKSPACE {
            return Err(MetadataError::WorkspaceLimitReached);
        }
        let captured_bytes = input.captures.iter().try_fold(0usize, |total, capture| {
            total
                .checked_add(capture.snapshot_bytes.len())
                .ok_or(MetadataError::WorkspaceLimitReached)
        })?;
        if captured_bytes > MAX_CHECKPOINT_CAPTURE_BYTES {
            return Err(MetadataError::WorkspaceLimitReached);
        }
        let captures = input
            .captures
            .into_iter()
            .map(|capture| (capture.node_id, capture))
            .collect::<HashMap<_, _>>();
        let sql_count = nodes
            .iter()
            .filter(|node| node.kind == WorkspaceNodeKind::SqlDocument)
            .count();
        if captures.len() != sql_count
            || captures.keys().any(|id| {
                !nodes
                    .iter()
                    .any(|node| node.id == *id && node.document_id.is_some())
            })
        {
            return Err(MetadataError::InvalidWorkspaceCheckpoint);
        }
        let retained_bytes: i64 = tx.query_row(
            "SELECT COALESCE(SUM(b.retained_bytes), 0)
             FROM workspace_content_blob b
             WHERE b.digest IN (
                 SELECT DISTINCT n.content_digest
                 FROM workspace_checkpoint_node n
                 JOIN workspace_checkpoint c ON c.id = n.checkpoint_id
                 WHERE c.workspace_id = ?1 AND n.content_digest IS NOT NULL
             )",
            params![workspace.0],
            |row| row.get(0),
        )?;
        let mut new_digests = HashMap::<String, i64>::new();
        for capture in captures.values() {
            let digest = content_digest(capture);
            let already_retained: bool = tx.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM workspace_checkpoint_node n
                     JOIN workspace_checkpoint c ON c.id = n.checkpoint_id
                     WHERE c.workspace_id = ?1 AND n.content_digest = ?2
                 )",
                params![workspace.0, digest],
                |row| row.get(0),
            )?;
            if !already_retained {
                new_digests.entry(digest).or_insert(
                    i64::try_from(capture.snapshot_bytes.len())
                        .map_err(|_| MetadataError::WorkspaceLimitReached)?,
                );
            }
        }
        let projected = new_digests
            .values()
            .try_fold(retained_bytes, |total, bytes| total.checked_add(*bytes))
            .ok_or(MetadataError::WorkspaceLimitReached)?;
        if projected > MAX_CHECKPOINT_RETAINED_BYTES {
            return Err(MetadataError::WorkspaceLimitReached);
        }
        tx.execute(
            "INSERT INTO workspace_checkpoint
             (workspace_id, workspace_revision, reason, name, created_by, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                workspace.0,
                i64::try_from(workspace_row.revision.0)
                    .map_err(|_| MetadataError::InvalidWorkspaceCheckpoint)?,
                checkpoint_reason_str(input.reason),
                input.name,
                actor.0,
                now,
            ],
        )?;
        let checkpoint_id = WorkspaceCheckpointId(tx.last_insert_rowid());
        for node in nodes {
            let content_digest = if node.kind == WorkspaceNodeKind::SqlDocument {
                let capture = captures
                    .get(&node.id)
                    .ok_or(MetadataError::InvalidWorkspaceCheckpoint)?;
                let digest = content_digest(capture);
                tx.execute(
                    "INSERT OR IGNORE INTO workspace_content_blob
                     (digest, snapshot_bytes, snapshot_version, retained_bytes, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        digest,
                        capture.snapshot_bytes,
                        capture.snapshot_version,
                        i64::try_from(capture.snapshot_bytes.len())
                            .map_err(|_| MetadataError::InvalidWorkspaceCheckpoint)?,
                        now,
                    ],
                )?;
                Some(digest)
            } else {
                None
            };
            tx.execute(
                "INSERT INTO workspace_checkpoint_node
                 (checkpoint_id, node_id, parent_id, path, kind, content_digest)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    checkpoint_id.0,
                    node.id.0,
                    node.parent_id.map(|id| id.0),
                    node.path.0,
                    node_kind_str(node.kind),
                    content_digest,
                ],
            )?;
        }
        let checkpoint = checkpoint_by_id_locked(&tx, checkpoint_id)?;
        tx.commit()?;
        Ok(checkpoint)
    }

    pub fn list_workspace_checkpoints_for_principal(
        &self,
        workspace: WorkspaceId,
        principal: PrincipalId,
        before_id: Option<WorkspaceCheckpointId>,
        limit: u32,
    ) -> Result<Vec<WorkspaceCheckpoint>> {
        let conn = self.conn()?;
        let workspace_row = workspace_by_id_locked(&conn, workspace)?;
        ensure_room_access(&conn, workspace_row.room_id, principal, false)?;
        let limit = limit.clamp(1, MAX_CHECKPOINTS_PER_PAGE);
        let mut stmt = conn.prepare(
            "SELECT id, workspace_id, workspace_revision, reason, name, created_by, created_at
             FROM workspace_checkpoint
             WHERE workspace_id = ?1 AND (?2 IS NULL OR id < ?2)
             ORDER BY id DESC LIMIT ?3",
        )?;
        let checkpoints = rows(stmt.query_map(
            params![workspace.0, before_id.map(|id| id.0), limit],
            checkpoint_from_row,
        )?)?;
        Ok(checkpoints)
    }

    pub fn workspace_restore_plan(
        &self,
        checkpoint: WorkspaceCheckpointId,
        actor: PrincipalId,
        expected: WorkspaceRevision,
    ) -> Result<WorkspaceRestorePlan> {
        let conn = self.conn()?;
        let checkpoint_row = checkpoint_by_id_locked(&conn, checkpoint)?;
        let workspace = workspace_by_id_locked(&conn, checkpoint_row.workspace_id)?;
        ensure_room_access(&conn, workspace.room_id, actor, true)?;
        ensure_workspace_revision(&workspace, expected)?;
        let nodes = checkpoint_nodes_locked(&conn, checkpoint)?;
        Ok(WorkspaceRestorePlan {
            checkpoint_id: checkpoint,
            workspace_id: workspace.id,
            checkpoint_revision: checkpoint_row.workspace_revision,
            current_revision: workspace.revision,
            nodes,
        })
    }

    /// Applies the structural half of a checkpoint restore.
    ///
    /// Existing SQL nodes keep their document ids; the server must author the
    /// corresponding Loro replacement updates before calling this method.
    /// SQL nodes recreated from the checkpoint receive a fresh document whose
    /// initial state is the checkpoint snapshot.
    pub fn apply_workspace_restore_structure(
        &self,
        checkpoint: WorkspaceCheckpointId,
        actor: PrincipalId,
        expected: WorkspaceRevision,
    ) -> Result<(WorkspaceRecord, Vec<WorkspaceNode>, Vec<DocumentId>)> {
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let checkpoint_row = checkpoint_by_id_locked(&tx, checkpoint)?;
        let workspace = workspace_by_id_locked(&tx, checkpoint_row.workspace_id)?;
        ensure_room_access(&tx, workspace.room_id, actor, true)?;
        ensure_workspace_revision(&workspace, expected)?;

        let desired = checkpoint_nodes_locked(&tx, checkpoint)?;
        let desired_by_id = desired
            .iter()
            .map(|node| (node.node_id, node))
            .collect::<HashMap<_, _>>();
        let current = workspace_nodes_locked(&tx, workspace.id)?;
        let current_by_id = current
            .iter()
            .map(|node| (node.id, node))
            .collect::<HashMap<_, _>>();

        // Detach nodes that can retain their identity before deleting obsolete
        // parents. Temporary paths also remove final-path uniqueness conflicts.
        for node in &current {
            if desired_by_id
                .get(&node.id)
                .is_some_and(|wanted| wanted.kind == node.kind)
            {
                tx.execute(
                    "UPDATE workspace_node
                     SET parent_id = NULL, path = ?1, path_key = ?1, updated_at = ?2
                     WHERE id = ?3",
                    params![format!("__sift_restore__{}", node.id.0), now, node.id.0],
                )?;
            }
        }

        let mut removed_documents = Vec::new();
        for node in &current {
            let retained = desired_by_id
                .get(&node.id)
                .is_some_and(|wanted| wanted.kind == node.kind);
            if !retained {
                if let Some(document_id) = node.document_id {
                    removed_documents.push(DocumentId(document_id));
                }
                tx.execute(
                    "DELETE FROM workspace_node WHERE id = ?1",
                    params![node.id.0],
                )?;
            }
        }
        for document in &removed_documents {
            tx.execute("DELETE FROM document WHERE id = ?1", params![document.0])?;
        }

        // Checkpoint rows are path-depth ordered, so every parent exists before
        // a newly restored child is inserted.
        for wanted in &desired {
            let retained = current_by_id
                .get(&wanted.node_id)
                .is_some_and(|node| node.kind == wanted.kind);
            if retained {
                continue;
            }
            let document_id = match wanted.kind {
                WorkspaceNodeKind::Folder => None,
                WorkspaceNodeKind::SqlDocument => {
                    let snapshot = wanted
                        .snapshot_bytes
                        .as_ref()
                        .ok_or(MetadataError::InvalidWorkspaceCheckpoint)?;
                    let version = wanted
                        .snapshot_version
                        .as_ref()
                        .ok_or(MetadataError::InvalidWorkspaceCheckpoint)?;
                    let position: i64 = tx.query_row(
                        "SELECT COUNT(*) FROM document WHERE room_id = ?1",
                        params![workspace.room_id.0],
                        |row| row.get(0),
                    )?;
                    let document_id = allocate_document_id(&tx)?;
                    tx.execute(
                        "INSERT INTO document
                         (id, room_id, kind, title, crdt_type, crdt_state, crdt_format_version,
                          snapshot_version, position, connection_profile_id, created_at, updated_at)
                         VALUES (?1, ?2, 'sql', ?3, 'loro', ?4, 1, ?5, ?6, NULL, ?7, ?7)",
                        params![
                            document_id.0,
                            workspace.room_id.0,
                            file_name(&wanted.path),
                            snapshot,
                            version,
                            position,
                            now,
                        ],
                    )?;
                    Some(document_id)
                }
                WorkspaceNodeKind::Artifact => {
                    return Err(MetadataError::InvalidWorkspaceCheckpoint)
                }
            };
            tx.execute(
                "INSERT INTO workspace_node
                 (id, workspace_id, parent_id, path, path_key, kind, document_id, revision,
                  created_at, updated_at)
                 VALUES (?1, ?2, NULL, ?3, ?3, ?4, ?5, 1, ?6, ?6)",
                params![
                    wanted.node_id.0,
                    workspace.id.0,
                    format!("__sift_restore__{}", wanted.node_id.0),
                    node_kind_str(wanted.kind),
                    document_id.map(|id| id.0),
                    now,
                ],
            )?;
        }

        for wanted in &desired {
            tx.execute(
                "UPDATE workspace_node
                 SET parent_id = ?1, path = ?2, path_key = ?3,
                     revision = revision + 1, updated_at = ?4
                 WHERE id = ?5 AND workspace_id = ?6",
                params![
                    wanted.parent_id.map(|id| id.0),
                    wanted.path.0,
                    workspace_path_key(&wanted.path)?,
                    now,
                    wanted.node_id.0,
                    workspace.id.0,
                ],
            )?;
            if wanted.kind == WorkspaceNodeKind::SqlDocument {
                tx.execute(
                    "UPDATE document SET title = ?1, updated_at = ?2
                     WHERE id = (SELECT document_id FROM workspace_node WHERE id = ?3)",
                    params![file_name(&wanted.path), now, wanted.node_id.0],
                )?;
            }
        }

        bump_workspace_locked(&tx, workspace.id, &now)?;
        let workspace = workspace_by_id_locked(&tx, workspace.id)?;
        let nodes = workspace_nodes_locked(&tx, workspace.id)?;
        tx.commit()?;
        Ok((workspace, nodes, removed_documents))
    }
}

pub fn public_workspace(record: WorkspaceRecord) -> Workspace {
    Workspace {
        id: record.id,
        room_id: record.room_id.0,
        name: record.name,
        revision: record.revision,
        capabilities: WorkspaceCapabilities {
            virtual_tree: true,
            filesystem_projection: false,
            git: false,
            git_network: false,
            scheduling: false,
            transfer_recipes: false,
        },
        created_at: record.created_at,
        updated_at: record.updated_at,
    }
}

fn validate_workspace_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > MAX_WORKSPACE_NAME_BYTES
        || name.trim() != name
        || name.contains('\0')
    {
        Err(MetadataError::InvalidWorkspaceName)
    } else {
        Ok(())
    }
}

fn validate_workspace_node_input(input: &NewWorkspaceNode) -> Result<()> {
    workspace_path_key(&input.path)?;
    match input.kind {
        WorkspaceNodeKind::Folder
            if input.initial_snapshot.is_none() && input.initial_snapshot_version.is_none() =>
        {
            Ok(())
        }
        WorkspaceNodeKind::SqlDocument
            if input
                .initial_snapshot
                .as_ref()
                .is_some_and(|bytes| !bytes.is_empty())
                && input.initial_snapshot_version.is_some() =>
        {
            Ok(())
        }
        _ => Err(MetadataError::InvalidWorkspaceNode),
    }
}

fn validate_checkpoint_input(input: &NewWorkspaceCheckpoint) -> Result<()> {
    let name_valid = input.name.as_ref().map_or(true, |name| {
        !name.is_empty()
            && name.len() <= MAX_CHECKPOINT_NAME_BYTES
            && name.trim() == name
            && !name.contains('\0')
    });
    if !name_valid
        || (input.reason == WorkspaceCheckpointReason::Named) != input.name.is_some()
        || input
            .captures
            .iter()
            .any(|capture| capture.snapshot_bytes.is_empty() || capture.snapshot_version.is_empty())
    {
        Err(MetadataError::InvalidWorkspaceCheckpoint)
    } else {
        Ok(())
    }
}

fn ensure_room_access(
    conn: &Connection,
    room: RoomId,
    principal: PrincipalId,
    writable: bool,
) -> Result<()> {
    let allowed: bool = conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM room_member m
             JOIN principal p ON p.id = m.principal_id
             WHERE m.room_id = ?1 AND m.principal_id = ?2
               AND p.disabled_at IS NULL
               AND (?3 = 0 OR m.role IN ('owner', 'editor'))
         )",
        params![room.0, principal.0, writable],
        |row| row.get(0),
    )?;
    if allowed {
        Ok(())
    } else {
        Err(MetadataError::RoomNotFound(room))
    }
}

fn ensure_workspace_revision(
    workspace: &WorkspaceRecord,
    expected: WorkspaceRevision,
) -> Result<()> {
    if workspace.revision == expected {
        Ok(())
    } else {
        Err(MetadataError::WorkspaceRevisionConflict {
            expected: expected.0,
            current: workspace.revision.0,
        })
    }
}

fn ensure_workspace_node_capacity(conn: &Connection, workspace: WorkspaceId) -> Result<()> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM workspace_node WHERE workspace_id = ?1",
        params![workspace.0],
        |row| row.get(0),
    )?;
    if count >= MAX_NODES_PER_WORKSPACE {
        Err(MetadataError::WorkspaceLimitReached)
    } else {
        Ok(())
    }
}

fn ensure_parent_matches_path(
    conn: &Connection,
    workspace: WorkspaceId,
    parent_id: Option<WorkspaceNodeId>,
    path: &WorkspacePath,
    moving: Option<WorkspaceNodeId>,
) -> Result<()> {
    let expected_parent = path.0.rsplit_once('/').map(|(parent, _)| parent);
    match (parent_id, expected_parent) {
        (None, None) => Ok(()),
        (Some(parent_id), Some(expected_path)) => {
            if moving == Some(parent_id) {
                return Err(MetadataError::InvalidWorkspacePath);
            }
            let parent = workspace_node_by_id_locked(conn, parent_id)?;
            if parent.workspace_id == workspace
                && parent.kind == WorkspaceNodeKind::Folder
                && parent.path.0 == expected_path
            {
                Ok(())
            } else {
                Err(MetadataError::InvalidWorkspacePath)
            }
        }
        _ => Err(MetadataError::InvalidWorkspacePath),
    }
}

fn ensure_path_available(
    conn: &Connection,
    workspace: WorkspaceId,
    path: &WorkspacePath,
    excluding: &[i64],
) -> Result<()> {
    let key = workspace_path_key(path)?;
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM workspace_node WHERE workspace_id = ?1 AND path_key = ?2",
            params![workspace.0, key],
            |row| row.get(0),
        )
        .optional()?;
    if existing.is_some_and(|id| !excluding.contains(&id)) {
        Err(MetadataError::WorkspacePathConflict)
    } else {
        Ok(())
    }
}

fn workspace_path_key(path: &WorkspacePath) -> Result<String> {
    if !path.is_valid() || path.0.nfc().collect::<String>() != path.0 {
        return Err(MetadataError::InvalidWorkspacePath);
    }
    Ok(path
        .0
        .nfkc()
        .flat_map(char::to_lowercase)
        .collect::<String>())
}

fn bump_workspace_locked(conn: &Connection, workspace: WorkspaceId, now: &str) -> Result<()> {
    let updated = conn.execute(
        "UPDATE workspace SET revision = revision + 1, updated_at = ?1 WHERE id = ?2",
        params![now, workspace.0],
    )?;
    if updated == 0 {
        Err(MetadataError::WorkspaceNotFound(workspace))
    } else {
        Ok(())
    }
}

fn workspace_by_id_locked(conn: &Connection, id: WorkspaceId) -> Result<WorkspaceRecord> {
    conn.query_row(
        "SELECT id, room_id, name, revision, created_at, updated_at
         FROM workspace WHERE id = ?1",
        params![id.0],
        workspace_from_row,
    )
    .optional()?
    .ok_or(MetadataError::WorkspaceNotFound(id))
}

fn workspace_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkspaceRecord> {
    let revision = row.get::<_, i64>(3)?;
    Ok(WorkspaceRecord {
        id: WorkspaceId(row.get(0)?),
        room_id: RoomId(row.get(1)?),
        name: row.get(2)?,
        revision: WorkspaceRevision(
            u64::try_from(revision)
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(3, revision))?,
        ),
        created_at: parse_time_sql(row.get(4)?)?,
        updated_at: parse_time_sql(row.get(5)?)?,
    })
}

fn workspace_nodes_locked(conn: &Connection, workspace: WorkspaceId) -> Result<Vec<WorkspaceNode>> {
    let mut stmt = conn.prepare(
        "SELECT id, workspace_id, parent_id, path, kind, document_id, revision,
                created_at, updated_at
         FROM workspace_node WHERE workspace_id = ?1
         ORDER BY length(path), path, id",
    )?;
    let nodes = rows(stmt.query_map(params![workspace.0], workspace_node_from_row)?)?;
    Ok(nodes)
}

fn workspace_node_by_id_locked(conn: &Connection, id: WorkspaceNodeId) -> Result<WorkspaceNode> {
    conn.query_row(
        "SELECT id, workspace_id, parent_id, path, kind, document_id, revision,
                created_at, updated_at
         FROM workspace_node WHERE id = ?1",
        params![id.0],
        workspace_node_from_row,
    )
    .optional()?
    .ok_or(MetadataError::WorkspaceNodeNotFound(id))
}

fn workspace_node_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkspaceNode> {
    let kind: String = row.get(4)?;
    let revision = row.get::<_, i64>(6)?;
    Ok(WorkspaceNode {
        id: WorkspaceNodeId(row.get(0)?),
        workspace_id: WorkspaceId(row.get(1)?),
        parent_id: row.get::<_, Option<i64>>(2)?.map(WorkspaceNodeId),
        path: WorkspacePath(row.get(3)?),
        kind: parse_node_kind(&kind).map_err(crate::sql_conversion_error)?,
        document_id: row.get(5)?,
        artifact_id: None,
        revision: u64::try_from(revision)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(6, revision))?,
        created_at: parse_time_sql(row.get(7)?)?,
        updated_at: parse_time_sql(row.get(8)?)?,
    })
}

fn subtree_nodes_locked(conn: &Connection, root: &WorkspaceNode) -> Result<Vec<WorkspaceNode>> {
    let escaped = root
        .path
        .0
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    let mut stmt = conn.prepare(
        "SELECT id, workspace_id, parent_id, path, kind, document_id, revision,
                created_at, updated_at
         FROM workspace_node
         WHERE workspace_id = ?1 AND (id = ?2 OR path LIKE ?3 ESCAPE '\\')
         ORDER BY length(path), path, id",
    )?;
    let nodes = rows(stmt.query_map(
        params![root.workspace_id.0, root.id.0, format!("{escaped}/%")],
        workspace_node_from_row,
    )?)?;
    Ok(nodes)
}

fn checkpoint_by_id_locked(
    conn: &Connection,
    id: WorkspaceCheckpointId,
) -> Result<WorkspaceCheckpoint> {
    conn.query_row(
        "SELECT id, workspace_id, workspace_revision, reason, name, created_by, created_at
         FROM workspace_checkpoint WHERE id = ?1",
        params![id.0],
        checkpoint_from_row,
    )
    .optional()?
    .ok_or(MetadataError::WorkspaceCheckpointNotFound(id))
}

fn checkpoint_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkspaceCheckpoint> {
    let revision = row.get::<_, i64>(2)?;
    let reason: String = row.get(3)?;
    Ok(WorkspaceCheckpoint {
        id: WorkspaceCheckpointId(row.get(0)?),
        workspace_id: WorkspaceId(row.get(1)?),
        workspace_revision: WorkspaceRevision(
            u64::try_from(revision)
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(2, revision))?,
        ),
        reason: parse_checkpoint_reason(&reason).map_err(crate::sql_conversion_error)?,
        name: row.get(4)?,
        created_by: row.get(5)?,
        created_at: parse_time_sql(row.get(6)?)?,
    })
}

fn checkpoint_node_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkspaceCheckpointNode> {
    let kind: String = row.get(3)?;
    Ok(WorkspaceCheckpointNode {
        node_id: WorkspaceNodeId(row.get(0)?),
        parent_id: row.get::<_, Option<i64>>(1)?.map(WorkspaceNodeId),
        path: WorkspacePath(row.get(2)?),
        kind: parse_node_kind(&kind).map_err(crate::sql_conversion_error)?,
        content_digest: row.get(4)?,
        snapshot_bytes: row.get(5)?,
        snapshot_version: row.get(6)?,
    })
}

fn checkpoint_nodes_locked(
    conn: &Connection,
    checkpoint: WorkspaceCheckpointId,
) -> Result<Vec<WorkspaceCheckpointNode>> {
    let mut stmt = conn.prepare(
        "SELECT n.node_id, n.parent_id, n.path, n.kind, n.content_digest,
                b.snapshot_bytes, b.snapshot_version
         FROM workspace_checkpoint_node n
         LEFT JOIN workspace_content_blob b ON b.digest = n.content_digest
         WHERE n.checkpoint_id = ?1
         ORDER BY length(n.path), n.path, n.node_id",
    )?;
    let nodes = rows(stmt.query_map(params![checkpoint.0], checkpoint_node_from_row)?)?;
    Ok(nodes)
}

fn node_kind_str(kind: WorkspaceNodeKind) -> &'static str {
    match kind {
        WorkspaceNodeKind::Folder => "folder",
        WorkspaceNodeKind::SqlDocument => "sql_document",
        WorkspaceNodeKind::Artifact => "artifact",
    }
}

fn parse_node_kind(value: &str) -> Result<WorkspaceNodeKind> {
    match value {
        "folder" => Ok(WorkspaceNodeKind::Folder),
        "sql_document" => Ok(WorkspaceNodeKind::SqlDocument),
        _ => Err(MetadataError::InvalidWorkspaceNode),
    }
}

fn checkpoint_reason_str(reason: WorkspaceCheckpointReason) -> &'static str {
    match reason {
        WorkspaceCheckpointReason::Automatic => "automatic",
        WorkspaceCheckpointReason::Named => "named",
        WorkspaceCheckpointReason::BeforeReconcile => "before_reconcile",
        WorkspaceCheckpointReason::BeforeRun => "before_run",
        WorkspaceCheckpointReason::BeforeVcs => "before_vcs",
    }
}

fn parse_checkpoint_reason(value: &str) -> Result<WorkspaceCheckpointReason> {
    match value {
        "automatic" => Ok(WorkspaceCheckpointReason::Automatic),
        "named" => Ok(WorkspaceCheckpointReason::Named),
        "before_reconcile" => Ok(WorkspaceCheckpointReason::BeforeReconcile),
        "before_run" => Ok(WorkspaceCheckpointReason::BeforeRun),
        "before_vcs" => Ok(WorkspaceCheckpointReason::BeforeVcs),
        _ => Err(MetadataError::InvalidWorkspaceCheckpoint),
    }
}

fn file_name(path: &WorkspacePath) -> &str {
    path.0.rsplit('/').next().unwrap_or(path.0.as_str())
}

fn content_digest(capture: &WorkspaceCheckpointCapture) -> String {
    let mut hash = Sha256::new();
    hash.update(&capture.snapshot_bytes);
    hash.update(&capture.snapshot_version);
    let mut output = String::with_capacity(64);
    for byte in hash.finalize() {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

fn delete_unreferenced_content_blobs(conn: &Connection) -> Result<()> {
    conn.execute(
        "DELETE FROM workspace_content_blob
         WHERE NOT EXISTS (
             SELECT 1 FROM workspace_checkpoint_node n
             WHERE n.content_digest = workspace_content_blob.digest
         )",
        [],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::{MemorySecretStore, NewRoom, RoomKind};

    use super::*;

    fn seeded() -> (MetadataStore, RoomId, PrincipalId, WorkspaceRecord) {
        let store = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        store.bootstrap_local("workspace-test").unwrap();
        let principal = PrincipalId(1);
        let room = store
            .create_room(
                crate::TenantId(1),
                principal,
                NewRoom {
                    name: "room".into(),
                    kind: RoomKind::Shared,
                },
            )
            .unwrap();
        let workspace = store
            .create_workspace(room.id, principal, "database")
            .unwrap();
        (store, room.id, principal, workspace)
    }

    fn folder(path: &str, parent_id: Option<WorkspaceNodeId>) -> NewWorkspaceNode {
        NewWorkspaceNode {
            parent_id,
            path: WorkspacePath::new(path).unwrap(),
            kind: WorkspaceNodeKind::Folder,
            initial_snapshot: None,
            initial_snapshot_version: None,
        }
    }

    fn sql(path: &str, parent_id: Option<WorkspaceNodeId>) -> NewWorkspaceNode {
        NewWorkspaceNode {
            parent_id,
            path: WorkspacePath::new(path).unwrap(),
            kind: WorkspaceNodeKind::SqlDocument,
            initial_snapshot: Some(vec![1, 2, 3]),
            initial_snapshot_version: Some(vec![4, 5]),
        }
    }

    #[test]
    fn workspace_tree_is_revisioned_and_document_owned() {
        let (store, _, actor, workspace) = seeded();
        let (workspace, ddl) = store
            .create_workspace_node(workspace.id, actor, workspace.revision, folder("ddl", None))
            .unwrap();
        assert_eq!(workspace.revision, WorkspaceRevision(2));
        let (workspace, query) = store
            .create_workspace_node(
                workspace.id,
                actor,
                workspace.revision,
                sql("ddl/query.sql", Some(ddl.id)),
            )
            .unwrap();
        let document = DocumentId(query.document_id.unwrap());
        assert_eq!(store.get_document(document).unwrap().title, "query.sql");

        let (workspace, moved) = store
            .move_workspace_node(
                ddl.id,
                actor,
                workspace.revision,
                None,
                WorkspacePath::new("schema").unwrap(),
            )
            .unwrap();
        assert_eq!(
            moved
                .iter()
                .map(|node| node.path.0.as_str())
                .collect::<Vec<_>>(),
            ["schema", "schema/query.sql"]
        );
        let workspace = store
            .delete_workspace_node(ddl.id, actor, workspace.revision)
            .unwrap();
        assert_eq!(workspace.revision, WorkspaceRevision(5));
        assert!(matches!(
            store.get_document(document),
            Err(MetadataError::DocumentNotFound(_))
        ));
        assert!(store
            .list_workspace_nodes_for_principal(workspace.id, actor)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn workspace_paths_and_revisions_fail_closed() {
        let (store, _, actor, workspace) = seeded();
        let (_, ddl) = store
            .create_workspace_node(workspace.id, actor, workspace.revision, folder("ddl", None))
            .unwrap();
        assert!(matches!(
            store.create_workspace_node(
                workspace.id,
                actor,
                workspace.revision,
                sql("ddl/query.sql", Some(ddl.id)),
            ),
            Err(MetadataError::WorkspaceRevisionConflict { .. })
        ));
        let current = store
            .get_workspace_for_principal(workspace.id, actor, false)
            .unwrap();
        assert!(matches!(
            store.create_workspace_node(
                current.id,
                actor,
                current.revision,
                sql("other/query.sql", Some(ddl.id)),
            ),
            Err(MetadataError::InvalidWorkspacePath)
        ));

        let (current, _) = store
            .create_workspace_node(current.id, actor, current.revision, folder("Å", None))
            .unwrap();
        assert!(matches!(
            store.create_workspace_node(current.id, actor, current.revision, folder("å", None),),
            Err(MetadataError::WorkspacePathConflict)
        ));
        assert!(matches!(
            store.create_workspace_node(
                current.id,
                actor,
                current.revision,
                folder("e\u{301}", None),
            ),
            Err(MetadataError::InvalidWorkspacePath)
        ));
    }

    #[test]
    fn checkpoints_are_exact_paged_and_content_deduplicated() {
        let (store, _, actor, workspace) = seeded();
        let (workspace, query) = store
            .create_workspace_node(
                workspace.id,
                actor,
                workspace.revision,
                sql("query.sql", None),
            )
            .unwrap();
        let input = || NewWorkspaceCheckpoint {
            expected_revision: workspace.revision,
            reason: WorkspaceCheckpointReason::Named,
            name: Some("before edit".into()),
            captures: vec![WorkspaceCheckpointCapture {
                node_id: query.id,
                snapshot_bytes: vec![1, 2, 3],
                snapshot_version: vec![4, 5],
            }],
        };
        let first = store
            .create_workspace_checkpoint(workspace.id, actor, input())
            .unwrap();
        let second = store
            .create_workspace_checkpoint(workspace.id, actor, input())
            .unwrap();
        let page = store
            .list_workspace_checkpoints_for_principal(workspace.id, actor, None, 1)
            .unwrap();
        assert_eq!(page[0].id, second.id);
        let next = store
            .list_workspace_checkpoints_for_principal(workspace.id, actor, Some(second.id), 10)
            .unwrap();
        assert_eq!(next[0].id, first.id);

        let plan = store
            .workspace_restore_plan(first.id, actor, workspace.revision)
            .unwrap();
        assert_eq!(plan.nodes.len(), 1);
        assert_eq!(
            plan.nodes[0].snapshot_bytes.as_deref(),
            Some(&[1, 2, 3][..])
        );
        let conn = store.conn().unwrap();
        let blob_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM workspace_content_blob", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(blob_count, 1);
    }

    #[test]
    fn restore_preserves_surviving_identity_and_recreates_deleted_files() {
        let (store, _, actor, workspace) = seeded();
        let (workspace, query) = store
            .create_workspace_node(
                workspace.id,
                actor,
                workspace.revision,
                sql("query.sql", None),
            )
            .unwrap();
        let original_document = DocumentId(query.document_id.unwrap());
        let checkpoint = store
            .create_workspace_checkpoint(
                workspace.id,
                actor,
                NewWorkspaceCheckpoint {
                    expected_revision: workspace.revision,
                    reason: WorkspaceCheckpointReason::Named,
                    name: Some("baseline".into()),
                    captures: vec![WorkspaceCheckpointCapture {
                        node_id: query.id,
                        snapshot_bytes: vec![1, 2, 3],
                        snapshot_version: vec![4, 5],
                    }],
                },
            )
            .unwrap();

        let (workspace, _) = store
            .move_workspace_node(
                query.id,
                actor,
                workspace.revision,
                None,
                WorkspacePath::new("renamed.sql").unwrap(),
            )
            .unwrap();
        let (workspace, nodes, removed) = store
            .apply_workspace_restore_structure(checkpoint.id, actor, workspace.revision)
            .unwrap();
        assert!(removed.is_empty());
        assert_eq!(nodes[0].id, query.id);
        assert_eq!(nodes[0].document_id, Some(original_document.0));
        assert_eq!(nodes[0].path.0, "query.sql");

        let workspace = store
            .delete_workspace_node(query.id, actor, workspace.revision)
            .unwrap();
        let (_, nodes, removed) = store
            .apply_workspace_restore_structure(checkpoint.id, actor, workspace.revision)
            .unwrap();
        assert!(removed.is_empty());
        assert_eq!(nodes[0].id, query.id);
        assert_ne!(nodes[0].document_id, Some(original_document.0));
        assert!(matches!(
            store.get_document(original_document),
            Err(MetadataError::DocumentNotFound(_))
        ));
    }

    #[test]
    fn workspace_batches_commit_once_or_roll_back_entirely() {
        let (store, _, actor, workspace) = seeded();
        let failed = store.mutate_workspace_batch(
            workspace.id,
            actor,
            workspace.revision,
            vec![
                WorkspaceBatchMutation::Create(folder("one", None)),
                WorkspaceBatchMutation::Move {
                    node_id: WorkspaceNodeId(999),
                    parent_id: None,
                    path: WorkspacePath::new("missing").unwrap(),
                },
            ],
        );
        assert!(matches!(
            failed,
            Err(MetadataError::WorkspaceNodeNotFound(_))
        ));
        assert!(store
            .list_workspace_nodes_for_principal(workspace.id, actor)
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .get_workspace_for_principal(workspace.id, actor, false)
                .unwrap()
                .revision,
            workspace.revision
        );

        let (updated, nodes, _) = store
            .mutate_workspace_batch(
                workspace.id,
                actor,
                workspace.revision,
                vec![
                    WorkspaceBatchMutation::Create(folder("one", None)),
                    WorkspaceBatchMutation::Create(folder("two", None)),
                ],
            )
            .unwrap();
        assert_eq!(updated.revision, WorkspaceRevision(2));
        assert_eq!(nodes.len(), 2);
    }
}
