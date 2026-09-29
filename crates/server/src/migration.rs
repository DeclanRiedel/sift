//! Conservative engine-aware migration rendering (ADR-033).

use std::collections::{BTreeSet, HashMap, HashSet};

use sha2::{Digest, Sha256};
use sift_protocol::{
    CatalogCoverageState, CatalogGraph, CatalogNode, CatalogNodeDetails, CatalogNodeKind,
    CatalogSourceRef, ConstraintKind, Engine, MigrationGroup, MigrationOptions, MigrationPlan,
    MigrationPlanId, MigrationStatement, Nullability, SchemaChange, SchemaChangeId,
    SchemaChangeKind, SchemaChangeRisk, SchemaDiff,
};

#[derive(Debug, thiserror::Error)]
pub enum MigrationRenderError {
    #[error("selected schema change does not exist in the diff")]
    UnknownChange,
    #[error("selected schema changes omit required prerequisite {0}")]
    MissingPrerequisite(SchemaChangeId),
    #[error("dependency group requires an engine-specific strategy: {0}")]
    UnsupportedDependencyCycle(String),
    #[error("migration rendering is not supported for {kind:?} change {change}")]
    UnsupportedChange {
        change: SchemaChangeId,
        kind: CatalogNodeKind,
    },
    #[error("catalog hierarchy is incomplete for migration change {0}")]
    IncompleteHierarchy(SchemaChangeId),
    #[error("schema change {0} does not contain the objects required by its kind")]
    InvalidChangeShape(SchemaChangeId),
    #[error("migration plan has no executable statements")]
    EmptyPlan,
    #[error("migration plan could not be serialized")]
    Serialization,
}

pub fn render_plan(
    engine: Engine,
    diff: &SchemaDiff,
    from: &CatalogGraph,
    to: &CatalogGraph,
    selected: &[SchemaChangeId],
    expected_live_revision: sift_protocol::CatalogRevision,
    options: &MigrationOptions,
) -> Result<MigrationPlan, MigrationRenderError> {
    let selected = if selected.is_empty() {
        diff.changes
            .iter()
            .map(|change| change.id.clone())
            .collect::<HashSet<_>>()
    } else {
        let unique = selected.iter().cloned().collect::<HashSet<_>>();
        if unique.len() != selected.len()
            || unique
                .iter()
                .any(|id| !diff.changes.iter().any(|change| &change.id == id))
        {
            return Err(MigrationRenderError::UnknownChange);
        }
        unique
    };
    let selected_changes = diff
        .changes
        .iter()
        .filter(|change| selected.contains(&change.id))
        .collect::<Vec<_>>();
    for change in &selected_changes {
        if let Some(missing) = change
            .prerequisites
            .iter()
            .find(|prerequisite| !selected.contains(*prerequisite))
        {
            return Err(MigrationRenderError::MissingPrerequisite(missing.clone()));
        }
    }
    if let Some(group) = selected_changes
        .iter()
        .find_map(|change| change.dependency_group.as_ref())
    {
        return Err(MigrationRenderError::UnsupportedDependencyCycle(
            group.clone(),
        ));
    }
    let created = selected_changes
        .iter()
        .filter(|change| change.kind == SchemaChangeKind::Create)
        .filter_map(|change| change.object_after.as_ref().map(|node| node.id.clone()))
        .collect::<HashSet<_>>();
    let dropped = selected_changes
        .iter()
        .filter(|change| change.kind == SchemaChangeKind::Drop)
        .filter_map(|change| change.object_before.as_ref().map(|node| node.id.clone()))
        .collect::<HashSet<_>>();
    let from_nodes = nodes(from);
    let to_nodes = nodes(to);
    let owned_sequence = if engine == Engine::Postgres {
        let mut sequence_changes = selected_changes
            .iter()
            .copied()
            .filter(|change| is_sequence_create_or_drop(change));
        match (sequence_changes.next(), sequence_changes.next()) {
            (Some(change), None) => Some(render_postgres_owned_sequence_change(
                change,
                diff,
                from,
                to,
                &from_nodes,
                &to_nodes,
                &selected,
            )?),
            (Some(change), Some(_)) => {
                return Err(MigrationRenderError::UnsupportedChange {
                    change: change.id.clone(),
                    kind: CatalogNodeKind::Sequence,
                });
            }
            _ => None,
        }
    } else {
        None
    };
    let partition_attachment = if engine == Engine::Postgres {
        let mut candidates = selected_changes.iter().copied().filter(|change| {
            change.kind == SchemaChangeKind::Alter
                && change
                    .object_after
                    .as_ref()
                    .is_some_and(|node| node.kind == CatalogNodeKind::Table)
                && (change
                    .object_before
                    .as_ref()
                    .is_some_and(|node| node.extra.contains_key("native_partition_shape"))
                    || change
                        .object_after
                        .as_ref()
                        .is_some_and(|node| node.extra.contains_key("native_partition_shape")))
        });
        match (candidates.next(), candidates.next()) {
            (Some(change), None) => Some(render_postgres_partition_attachment(
                change,
                diff,
                from,
                to,
                &from_nodes,
                &to_nodes,
                &selected,
            )?),
            (Some(change), Some(_)) => {
                return Err(MigrationRenderError::UnsupportedChange {
                    change: change.id.clone(),
                    kind: CatalogNodeKind::Table,
                });
            }
            _ => None,
        }
    } else {
        None
    };
    let policy_rename = if engine == Engine::Postgres {
        let mut candidates = selected_changes.iter().copied().filter(|change| {
            change.kind == SchemaChangeKind::Alter
                && change.object_before.as_ref().is_some_and(|node| {
                    node.kind == CatalogNodeKind::Table
                        && node.extra.contains_key("native_policy_oid")
                })
                && change.object_after.as_ref().is_some_and(|node| {
                    node.kind == CatalogNodeKind::Table
                        && node.extra.contains_key("native_policy_oid")
                })
        });
        match (candidates.next(), candidates.next()) {
            (Some(change), None) => Some(render_postgres_policy_rename(
                change,
                diff,
                from,
                to,
                &from_nodes,
                &to_nodes,
            )?),
            (Some(change), Some(_)) => {
                return Err(MigrationRenderError::UnsupportedChange {
                    change: change.id.clone(),
                    kind: CatalogNodeKind::Table,
                });
            }
            _ => None,
        }
    } else {
        None
    };
    let policy_create_drop = if engine == Engine::Postgres {
        let mut candidates = selected_changes.iter().copied().filter(|change| {
            change.kind == SchemaChangeKind::Alter
                && change.object_before.as_ref().is_some_and(|node| {
                    node.kind == CatalogNodeKind::Table
                        && (node.extra.contains_key("native_policy_empty_rls")
                            || node.extra.contains_key("native_policy_safe_predicate"))
                })
                && change.object_after.as_ref().is_some_and(|node| {
                    node.kind == CatalogNodeKind::Table
                        && (node.extra.contains_key("native_policy_empty_rls")
                            || node.extra.contains_key("native_policy_safe_predicate"))
                })
        });
        match (candidates.next(), candidates.next()) {
            (Some(change), None) => {
                render_postgres_policy_create_drop(change, diff, from, to, &from_nodes, &to_nodes)?
            }
            (Some(change), Some(_)) => {
                return Err(MigrationRenderError::UnsupportedChange {
                    change: change.id.clone(),
                    kind: CatalogNodeKind::Table,
                });
            }
            _ => None,
        }
    } else {
        None
    };
    let mut statements = Vec::new();
    let mut warnings = diff.warnings.clone();
    if options.online_indexes {
        warnings.push(
            "online index rendering is not enabled without server/edition capability proof".into(),
        );
    }
    for change in &selected_changes {
        if owned_sequence
            .as_ref()
            .is_some_and(|rendered| rendered.table_change_id == change.id)
        {
            continue;
        }
        if partition_attachment
            .as_ref()
            .is_some_and(|rendered| rendered.parent_change_id == change.id)
        {
            continue;
        }
        if implicitly_covered(change, &created, &dropped) {
            continue;
        }
        let sqls = if let Some(rendered) = policy_create_drop
            .as_ref()
            .filter(|rendered| rendered.change_id == change.id)
        {
            vec![rendered.forward.clone()]
        } else if let Some(rendered) = policy_rename
            .as_ref()
            .filter(|rendered| rendered.change_id == change.id)
        {
            vec![rendered.forward.clone()]
        } else if let Some(rendered) = partition_attachment
            .as_ref()
            .filter(|rendered| rendered.child_change_id == change.id)
        {
            vec![rendered.forward.clone()]
        } else if let Some(rendered) = owned_sequence
            .as_ref()
            .filter(|rendered| rendered.sequence_change_id == change.id)
        {
            rendered.forward.clone()
        } else if engine == Engine::Postgres
            && is_index_create_or_drop(change)
            && (from.provider.provider_id == Engine::Postgres.provider_id()
                || to.provider.provider_id == Engine::Postgres.provider_id())
        {
            vec![render_postgres_index_change(
                change,
                diff,
                from,
                to,
                &from_nodes,
                &to_nodes,
            )?]
        } else {
            render_change(engine, change, &from_nodes, &to_nodes, to)?
                .into_iter()
                .collect()
        };
        for sql in sqls {
            statements.push(MigrationStatement {
                ordinal: u32::try_from(statements.len() + 1).unwrap_or(u32::MAX),
                fingerprint: crate::fingerprint::sql(&sql),
                sql,
                change_ids: vec![change.id.clone()],
                risk: if policy_create_drop
                    .as_ref()
                    .is_some_and(|rendered| rendered.change_id == change.id)
                {
                    SchemaChangeRisk::Privilege
                } else {
                    change.risk
                },
            });
        }
    }
    if statements.is_empty() {
        return Err(MigrationRenderError::EmptyPlan);
    }
    let required_acknowledgements = statements
        .iter()
        .filter_map(|statement| {
            matches!(
                statement.risk,
                SchemaChangeRisk::DataLoss
                    | SchemaChangeRisk::DataRewrite
                    | SchemaChangeRisk::Privilege
                    | SchemaChangeRisk::Unknown
            )
            .then_some(statement.risk)
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let groups = vec![MigrationGroup {
        ordinal: 1,
        transactional: engine == Engine::Sqlite || options.prefer_transactional,
        statements,
    }];
    let mut rollback_statements = Vec::new();
    for change in selected_changes.iter().rev() {
        if owned_sequence
            .as_ref()
            .is_some_and(|rendered| rendered.table_change_id == change.id)
        {
            continue;
        }
        if partition_attachment
            .as_ref()
            .is_some_and(|rendered| rendered.parent_change_id == change.id)
        {
            continue;
        }
        if let Some(rendered) = policy_create_drop
            .as_ref()
            .filter(|rendered| rendered.change_id == change.id)
        {
            rollback_statements.push(MigrationStatement {
                ordinal: u32::try_from(rollback_statements.len() + 1).unwrap_or(u32::MAX),
                fingerprint: crate::fingerprint::sql(&rendered.rollback),
                sql: rendered.rollback.clone(),
                change_ids: vec![change.id.clone()],
                risk: SchemaChangeRisk::Privilege,
            });
            continue;
        }
        if let Some(rendered) = policy_rename
            .as_ref()
            .filter(|rendered| rendered.change_id == change.id)
        {
            rollback_statements.push(MigrationStatement {
                ordinal: u32::try_from(rollback_statements.len() + 1).unwrap_or(u32::MAX),
                fingerprint: crate::fingerprint::sql(&rendered.rollback),
                sql: rendered.rollback.clone(),
                change_ids: vec![change.id.clone()],
                risk: change.risk,
            });
            continue;
        }
        if let Some(rendered) = partition_attachment
            .as_ref()
            .filter(|rendered| rendered.child_change_id == change.id)
        {
            rollback_statements.push(MigrationStatement {
                ordinal: u32::try_from(rollback_statements.len() + 1).unwrap_or(u32::MAX),
                fingerprint: crate::fingerprint::sql(&rendered.rollback),
                sql: rendered.rollback.clone(),
                change_ids: vec![change.id.clone()],
                risk: change.risk,
            });
            continue;
        }
        if change.reversibility != sift_protocol::SchemaChangeReversibility::Exact {
            warnings.push(format!(
                "rollback omitted for {} because it is {:?}",
                change.id, change.reversibility
            ));
            continue;
        }
        if let Some(rendered) = owned_sequence
            .as_ref()
            .filter(|rendered| rendered.sequence_change_id == change.id)
        {
            for sql in &rendered.rollback {
                rollback_statements.push(MigrationStatement {
                    ordinal: u32::try_from(rollback_statements.len() + 1).unwrap_or(u32::MAX),
                    fingerprint: crate::fingerprint::sql(sql),
                    sql: sql.clone(),
                    change_ids: vec![change.id.clone()],
                    risk: change.risk,
                });
            }
            continue;
        }
        let inverse = invert_change(change);
        let rollback = if engine == Engine::Postgres
            && is_index_create_or_drop(&inverse)
            && (from.provider.provider_id == Engine::Postgres.provider_id()
                || to.provider.provider_id == Engine::Postgres.provider_id())
        {
            render_postgres_index_change(&inverse, diff, to, from, &to_nodes, &from_nodes).map(Some)
        } else {
            render_change(engine, &inverse, &to_nodes, &from_nodes, from)
        };
        match rollback {
            Ok(Some(sql)) => rollback_statements.push(MigrationStatement {
                ordinal: u32::try_from(rollback_statements.len() + 1).unwrap_or(u32::MAX),
                fingerprint: crate::fingerprint::sql(&sql),
                sql,
                change_ids: vec![change.id.clone()],
                risk: change.risk,
            }),
            Ok(None) => {}
            Err(error) => warnings.push(format!(
                "rollback could not be rendered for {}: {error}",
                change.id
            )),
        }
    }
    let rollback_groups = if rollback_statements.is_empty() {
        Vec::new()
    } else {
        vec![MigrationGroup {
            ordinal: 1,
            transactional: options.prefer_transactional,
            statements: rollback_statements,
        }]
    };
    let id = MigrationPlanId(uuid::Uuid::new_v4());
    let run_id = sift_protocol::MigrationRunId(uuid::Uuid::new_v4());
    let expires_at = chrono::Utc::now() + chrono::Duration::minutes(10);
    let digest_bytes = serde_json::to_vec(&(
        id,
        run_id,
        &diff.digest,
        expected_live_revision,
        &groups,
        &rollback_groups,
        &required_acknowledgements,
        &warnings,
        expires_at,
    ))
    .map_err(|_| MigrationRenderError::Serialization)?;
    Ok(MigrationPlan {
        id,
        run_id,
        digest: format!("migfp:{}", hex_digest(&digest_bytes)),
        diff_digest: diff.digest.clone(),
        expected_live_revision,
        groups,
        rollback_groups,
        required_acknowledgements,
        warnings,
        expires_at,
    })
}

fn invert_change(change: &SchemaChange) -> SchemaChange {
    let mut inverse = change.clone();
    inverse.kind = match change.kind {
        SchemaChangeKind::Create => SchemaChangeKind::Drop,
        SchemaChangeKind::Drop => SchemaChangeKind::Create,
        kind => kind,
    };
    inverse.object_before = change.object_after.clone();
    inverse.object_after = change.object_before.clone();
    for field in &mut inverse.field_changes {
        std::mem::swap(&mut field.before, &mut field.after);
    }
    inverse.prerequisites.clear();
    inverse
}

fn nodes(graph: &CatalogGraph) -> HashMap<sift_protocol::CatalogObjectId, &CatalogNode> {
    graph
        .data
        .nodes
        .iter()
        .map(|node| (node.id.clone(), node))
        .collect()
}

fn is_index_create_or_drop(change: &SchemaChange) -> bool {
    matches!(
        change.kind,
        SchemaChangeKind::Create | SchemaChangeKind::Drop
    ) && change
        .object_after
        .as_ref()
        .or(change.object_before.as_ref())
        .is_some_and(|node| node.kind == CatalogNodeKind::Index)
}

fn is_sequence_create_or_drop(change: &SchemaChange) -> bool {
    matches!(
        change.kind,
        SchemaChangeKind::Create | SchemaChangeKind::Drop
    ) && change
        .object_after
        .as_ref()
        .or(change.object_before.as_ref())
        .is_some_and(|node| node.kind == CatalogNodeKind::Sequence)
}

struct OwnedSequenceRender {
    sequence_change_id: SchemaChangeId,
    table_change_id: SchemaChangeId,
    forward: Vec<String>,
    rollback: Vec<String>,
}

fn render_postgres_owned_sequence_change(
    change: &SchemaChange,
    diff: &SchemaDiff,
    from: &CatalogGraph,
    to: &CatalogGraph,
    from_nodes: &HashMap<sift_protocol::CatalogObjectId, &CatalogNode>,
    to_nodes: &HashMap<sift_protocol::CatalogObjectId, &CatalogNode>,
    selected: &HashSet<SchemaChangeId>,
) -> Result<OwnedSequenceRender, MigrationRenderError> {
    let reject = || MigrationRenderError::UnsupportedChange {
        change: change.id.clone(),
        kind: CatalogNodeKind::Sequence,
    };
    if diff.changes.len() != 2
        || !matches!(
            diff.from,
            CatalogSourceRef::Live { .. } | CatalogSourceRef::Snapshot { .. }
        )
        || !matches!(
            diff.to,
            CatalogSourceRef::Live { .. } | CatalogSourceRef::Snapshot { .. }
        )
        || [from, to].iter().any(|graph| {
            graph.provider.provider_id != Engine::Postgres.provider_id()
                || graph.data.coverage.state != CatalogCoverageState::Complete
                || !graph.data.coverage.requested_kinds.is_empty()
                || !graph.data.coverage.omitted_schemas.is_empty()
                || graph.data.coverage.truncated_at_nodes.is_some()
        })
    {
        return Err(reject());
    }
    let (active_graph, active_nodes, other_nodes) = if change.kind == SchemaChangeKind::Create {
        (to, to_nodes, from_nodes)
    } else {
        (from, from_nodes, to_nodes)
    };
    let sequence = change
        .object_after
        .as_ref()
        .or(change.object_before.as_ref())
        .filter(|node| node.kind == CatalogNodeKind::Sequence)
        .ok_or_else(reject)?;
    let catalog_sequence = active_nodes.get(&sequence.id).copied().ok_or_else(reject)?;
    let ddl = sequence
        .extra
        .get("native_owned_sequence_create_sql")
        .and_then(serde_json::Value::as_str)
        .filter(|sql| {
            sql.len() <= 65_536 && sql.starts_with("CREATE SEQUENCE ") && sql.ends_with(';')
        })
        .ok_or_else(reject)?;
    if sequence
        .extra
        .get("migration_unsupported")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
        || sequence
            .extra
            .get("native_sequence_shape")
            .and_then(serde_json::Value::as_str)
            .is_none()
        || catalog_sequence.qualified_name != sequence.qualified_name
        || catalog_sequence
            .extra
            .get("native_owned_sequence_create_sql")
            != sequence.extra.get("native_owned_sequence_create_sql")
        || catalog_sequence.extra.get("native_sequence_shape")
            != sequence.extra.get("native_sequence_shape")
    {
        return Err(reject());
    }
    let owner_edges = active_graph
        .data
        .edges
        .iter()
        .filter(|edge| {
            edge.kind == sift_protocol::CatalogEdgeKind::OwnsSequence
                && edge.to.as_ref() == Some(&sequence.id)
                && edge.certainty == sift_protocol::CatalogEdgeCertainty::CatalogProven
        })
        .collect::<Vec<_>>();
    let [owner_edge] = owner_edges.as_slice() else {
        return Err(reject());
    };
    let column = active_nodes
        .get(&owner_edge.from)
        .copied()
        .ok_or_else(reject)?;
    if column.kind != CatalogNodeKind::Column {
        return Err(reject());
    }
    let table = column
        .parent_id
        .as_ref()
        .and_then(|id| active_nodes.get(id))
        .copied()
        .filter(|node| node.kind == CatalogNodeKind::Table)
        .ok_or_else(reject)?;
    let other_table = other_nodes
        .values()
        .copied()
        .find(|node| {
            node.kind == CatalogNodeKind::Table && node.qualified_name == table.qualified_name
        })
        .ok_or_else(reject)?;
    let other_column = other_nodes
        .values()
        .copied()
        .find(|node| {
            node.kind == CatalogNodeKind::Column
                && node.parent_id.as_ref() == Some(&other_table.id)
                && node.name == column.name
        })
        .ok_or_else(reject)?;
    if table.name != other_table.name
        || table.qualified_name != other_table.qualified_name
        || table.ordinal != other_table.ordinal
        || table.completeness != other_table.completeness
        || table.definition_digest != other_table.definition_digest
        || serde_json::to_value(&table.details).ok()
            != serde_json::to_value(&other_table.details).ok()
        || column.name != other_column.name
        || column.qualified_name != other_column.qualified_name
        || column.ordinal != other_column.ordinal
        || column.completeness != other_column.completeness
        || column.definition_digest != other_column.definition_digest
        || serde_json::to_value(&column.details).ok()
            != serde_json::to_value(&other_column.details).ok()
        || column.extra != other_column.extra
        || owned_sequence_table_extra(table).is_none()
        || owned_sequence_table_extra(table) != owned_sequence_table_extra(other_table)
        || !table.extra.contains_key("native_owned_sequence_shape")
        || other_table
            .extra
            .contains_key("native_owned_sequence_shape")
    {
        return Err(reject());
    }
    let table_change = diff
        .changes
        .iter()
        .find(|candidate| {
            candidate.kind == SchemaChangeKind::Alter
                && candidate
                    .object_before
                    .as_ref()
                    .is_some_and(|node| node.id == other_table.id || node.id == table.id)
                && candidate
                    .object_after
                    .as_ref()
                    .is_some_and(|node| node.id == other_table.id || node.id == table.id)
        })
        .ok_or_else(reject)?;
    if !selected.contains(&table_change.id) {
        return Err(MigrationRenderError::MissingPrerequisite(
            table_change.id.clone(),
        ));
    }
    let sequence_schema = schema_ancestor(change, sequence, active_nodes)?;
    let table_schema = schema_ancestor(change, table, active_nodes)?;
    if sequence_schema.name != table_schema.name {
        return Err(reject());
    }
    let sequence_name = qualified_object(Engine::Postgres, change, sequence, active_nodes)?;
    let table_name = qualified_object(Engine::Postgres, change, table, active_nodes)?;
    let owner = format!(
        "ALTER SEQUENCE {sequence_name} OWNED BY {table_name}.{};",
        crate::ddl::quote_ident(&column.name, Engine::Postgres)
    );
    let drop_sql = format!("DROP SEQUENCE {sequence_name} RESTRICT;");
    let (forward, rollback) = if change.kind == SchemaChangeKind::Create {
        (vec![ddl.to_owned(), owner], vec![drop_sql])
    } else {
        (vec![drop_sql], vec![ddl.to_owned(), owner])
    };
    Ok(OwnedSequenceRender {
        sequence_change_id: change.id.clone(),
        table_change_id: table_change.id.clone(),
        forward,
        rollback,
    })
}

fn owned_sequence_table_extra(
    table: &CatalogNode,
) -> Option<std::collections::BTreeMap<String, serde_json::Value>> {
    let mut extra = table.extra.clone();
    extra.remove("estimated_rows");
    extra.remove("modified_at");
    let had_ownership = extra.remove("native_owned_sequence_shape").is_some();
    if had_ownership {
        if extra.remove("migration_unsupported") != Some(serde_json::Value::Bool(true)) {
            return None;
        }
    } else if extra.get("migration_unsupported") == Some(&serde_json::Value::Bool(true)) {
        return None;
    }
    if extra.keys().any(|key| key.starts_with("native_")) {
        return None;
    }
    Some(extra)
}

struct PartitionAttachmentRender {
    child_change_id: SchemaChangeId,
    parent_change_id: SchemaChangeId,
    forward: String,
    rollback: String,
}

fn render_postgres_partition_attachment(
    change: &SchemaChange,
    diff: &SchemaDiff,
    from: &CatalogGraph,
    to: &CatalogGraph,
    from_nodes: &HashMap<sift_protocol::CatalogObjectId, &CatalogNode>,
    to_nodes: &HashMap<sift_protocol::CatalogObjectId, &CatalogNode>,
    selected: &HashSet<SchemaChangeId>,
) -> Result<PartitionAttachmentRender, MigrationRenderError> {
    let reject = || MigrationRenderError::UnsupportedChange {
        change: change.id.clone(),
        kind: CatalogNodeKind::Table,
    };
    if diff.changes.len() != 2
        || from.database_identity != to.database_identity
        || !matches!(
            diff.from,
            CatalogSourceRef::Live { .. } | CatalogSourceRef::Snapshot { .. }
        )
        || !matches!(
            diff.to,
            CatalogSourceRef::Live { .. } | CatalogSourceRef::Snapshot { .. }
        )
        || [from, to].iter().any(|graph| {
            graph.provider.provider_id != Engine::Postgres.provider_id()
                || graph.data.coverage.state != CatalogCoverageState::Complete
                || !graph.data.coverage.requested_kinds.is_empty()
                || !graph.data.coverage.omitted_schemas.is_empty()
                || graph.data.coverage.truncated_at_nodes.is_some()
        })
    {
        return Err(reject());
    }
    let before_child = change.object_before.as_ref().ok_or_else(reject)?;
    let after_child = change.object_after.as_ref().ok_or_else(reject)?;
    if before_child.kind != CatalogNodeKind::Table
        || after_child.kind != CatalogNodeKind::Table
        || !stable_partition_relation(before_child, after_child, true)
        || !from_nodes.get(&before_child.id).is_some_and(|node| {
            serde_json::to_value(node).ok() == serde_json::to_value(before_child).ok()
        })
        || !to_nodes.get(&after_child.id).is_some_and(|node| {
            serde_json::to_value(node).ok() == serde_json::to_value(after_child).ok()
        })
    {
        return Err(reject());
    }
    let attach = !before_child.extra.contains_key("native_partition_shape")
        && after_child.extra.contains_key("native_partition_shape");
    let detach = before_child.extra.contains_key("native_partition_shape")
        && !after_child.extra.contains_key("native_partition_shape");
    if !attach && !detach {
        return Err(reject());
    }
    let (active_child, active_graph, active_nodes) = if attach {
        (after_child, to, to_nodes)
    } else {
        (before_child, from, from_nodes)
    };
    let parent_name = active_child
        .extra
        .get("native_partition_parent")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(reject)?;
    let parent = active_nodes
        .values()
        .copied()
        .find(|node| {
            node.kind == CatalogNodeKind::PartitionedTable
                && node
                    .parent_id
                    .as_ref()
                    .and_then(|id| active_nodes.get(id))
                    .is_some_and(|schema| format!("{}.{}", schema.name, node.name) == parent_name)
        })
        .ok_or_else(reject)?;
    let parent_change = diff
        .changes
        .iter()
        .find(|candidate| {
            candidate.kind == SchemaChangeKind::Alter
                && candidate.object_before.as_ref().is_some_and(|node| {
                    node.kind == CatalogNodeKind::PartitionedTable
                        && node.qualified_name == parent.qualified_name
                })
                && candidate.object_after.as_ref().is_some_and(|node| {
                    node.kind == CatalogNodeKind::PartitionedTable
                        && node.qualified_name == parent.qualified_name
                })
        })
        .ok_or_else(reject)?;
    let before_parent = parent_change.object_before.as_ref().ok_or_else(reject)?;
    let after_parent = parent_change.object_after.as_ref().ok_or_else(reject)?;
    if !stable_partition_relation(before_parent, after_parent, false)
        || !from_nodes.get(&before_parent.id).is_some_and(|node| {
            serde_json::to_value(node).ok() == serde_json::to_value(before_parent).ok()
        })
        || !to_nodes.get(&after_parent.id).is_some_and(|node| {
            serde_json::to_value(node).ok() == serde_json::to_value(after_parent).ok()
        })
        || !selected.contains(&parent_change.id)
        || active_graph.data.nodes.iter().any(|node| {
            node.kind == CatalogNodeKind::Table
                && node.id != active_child.id
                && node.extra.contains_key("native_partition_shape")
        })
    {
        return Err(reject());
    }
    let (active_parent, inactive_parent) = if attach {
        (after_parent, before_parent)
    } else {
        (before_parent, after_parent)
    };
    if !active_parent.extra.contains_key("native_descendant_shape")
        || inactive_parent
            .extra
            .contains_key("native_descendant_shape")
        || !stable_partition_children(
            from,
            to,
            before_child,
            after_child,
            before_parent,
            after_parent,
        )
    {
        return Err(reject());
    }
    let parent_schema = schema_ancestor(change, active_parent, active_nodes)?;
    let child_schema = schema_ancestor(change, active_child, active_nodes)?;
    if parent_schema.name != child_schema.name {
        return Err(reject());
    }
    let parent_sql = qualified_object(Engine::Postgres, change, active_parent, active_nodes)?;
    let child_sql = qualified_object(Engine::Postgres, change, active_child, active_nodes)?;
    let bound = active_child
        .extra
        .get("native_partition_bound")
        .and_then(serde_json::Value::as_str)
        .filter(|value| value.len() <= 4_096 && value.starts_with("FOR VALUES "))
        .ok_or_else(reject)?;
    let attach_sql = format!("ALTER TABLE {parent_sql} ATTACH PARTITION {child_sql} {bound};");
    let detach_sql = format!("ALTER TABLE {parent_sql} DETACH PARTITION {child_sql};");
    let (forward, rollback) = if attach {
        (attach_sql, detach_sql)
    } else {
        (detach_sql, attach_sql)
    };
    Ok(PartitionAttachmentRender {
        child_change_id: change.id.clone(),
        parent_change_id: parent_change.id.clone(),
        forward,
        rollback,
    })
}

fn stable_partition_relation(before: &CatalogNode, after: &CatalogNode, child: bool) -> bool {
    let mut before_extra = before.extra.clone();
    let mut after_extra = after.extra.clone();
    for extra in [&mut before_extra, &mut after_extra] {
        extra.remove("estimated_rows");
        extra.remove("modified_at");
        extra.remove("migration_unsupported");
        if child {
            extra.remove("native_partition_shape");
            extra.remove("native_partition_parent");
            extra.remove("native_partition_bound");
        } else {
            extra.remove("native_descendant_shape");
        }
    }
    before.kind == after.kind
        && before.name == after.name
        && before.qualified_name == after.qualified_name
        && before.native_id.is_some()
        && before.native_id == after.native_id
        && before.ordinal == after.ordinal
        && before.completeness == after.completeness
        && before.definition_digest == after.definition_digest
        && serde_json::to_value(&before.details).ok() == serde_json::to_value(&after.details).ok()
        && before_extra == after_extra
        && before_extra
            .keys()
            .all(|key| !key.starts_with("native_") || (!child && key == "native_column_shape"))
        && if child {
            before.extra.get("migration_unsupported") != after.extra.get("migration_unsupported")
        } else {
            before.extra.get("migration_unsupported") == after.extra.get("migration_unsupported")
        }
}

fn stable_partition_children(
    from: &CatalogGraph,
    to: &CatalogGraph,
    before_child: &CatalogNode,
    after_child: &CatalogNode,
    before_parent: &CatalogNode,
    after_parent: &CatalogNode,
) -> bool {
    for (before, after) in [(before_child, after_child), (before_parent, after_parent)] {
        let before_nodes = partition_child_nodes(from, before);
        let after_nodes = partition_child_nodes(to, after);
        if before_nodes.len() != after_nodes.len()
            || before_nodes.iter().zip(after_nodes).any(|(left, right)| {
                left.kind != right.kind
                    || left.qualified_name != right.qualified_name
                    || left.ordinal != right.ordinal
                    || left.definition_digest != right.definition_digest
                    || left.extra != right.extra
                    || serde_json::to_value(&left.details).ok()
                        != serde_json::to_value(&right.details).ok()
            })
        {
            return false;
        }
    }
    true
}

fn partition_child_nodes<'a>(
    graph: &'a CatalogGraph,
    parent: &CatalogNode,
) -> Vec<&'a CatalogNode> {
    let mut nodes = graph
        .data
        .nodes
        .iter()
        .filter(|node| node.parent_id.as_ref() == Some(&parent.id))
        .collect::<Vec<_>>();
    nodes.sort_by(|a, b| a.qualified_name.cmp(&b.qualified_name));
    nodes
}

struct PolicyRenameRender {
    change_id: SchemaChangeId,
    forward: String,
    rollback: String,
}

struct PolicyCreateDropRender {
    change_id: SchemaChangeId,
    forward: String,
    rollback: String,
}

fn render_postgres_policy_create_drop(
    change: &SchemaChange,
    diff: &SchemaDiff,
    from: &CatalogGraph,
    to: &CatalogGraph,
    from_nodes: &HashMap<sift_protocol::CatalogObjectId, &CatalogNode>,
    to_nodes: &HashMap<sift_protocol::CatalogObjectId, &CatalogNode>,
) -> Result<Option<PolicyCreateDropRender>, MigrationRenderError> {
    let reject = || MigrationRenderError::UnsupportedChange {
        change: change.id.clone(),
        kind: CatalogNodeKind::Table,
    };
    let before = change.object_before.as_ref().ok_or_else(reject)?;
    let after = change.object_after.as_ref().ok_or_else(reject)?;
    let create = before.extra.get("native_policy_empty_rls")
        == Some(&serde_json::Value::Bool(true))
        && after.extra.contains_key("native_policy_safe_predicate");
    let drop = after.extra.get("native_policy_empty_rls") == Some(&serde_json::Value::Bool(true))
        && before.extra.contains_key("native_policy_safe_predicate");
    if !create && !drop {
        return Ok(None);
    }
    if diff.changes.len() != 1
        || from.database_identity != to.database_identity
        || !matches!(
            diff.from,
            CatalogSourceRef::Live { .. } | CatalogSourceRef::Snapshot { .. }
        )
        || !matches!(
            diff.to,
            CatalogSourceRef::Live { .. } | CatalogSourceRef::Snapshot { .. }
        )
        || [from, to].iter().any(|graph| {
            graph.provider.provider_id != Engine::Postgres.provider_id()
                || graph.data.coverage.state != CatalogCoverageState::Complete
                || !graph.data.coverage.requested_kinds.is_empty()
                || !graph.data.coverage.omitted_schemas.is_empty()
                || graph.data.coverage.truncated_at_nodes.is_some()
        })
    {
        return Err(reject());
    }
    let (active, empty) = if create {
        (after, before)
    } else {
        (before, after)
    };
    let predicate = active
        .extra
        .get("native_policy_safe_predicate")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(reject)?;
    let threshold = predicate
        .strip_prefix("(id > ")
        .and_then(|value| value.strip_suffix(')'))
        .ok_or_else(reject)?;
    if threshold.is_empty()
        || threshold.len() > 18
        || (threshold.len() > 1 && threshold.starts_with('0'))
        || !threshold.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(reject());
    }
    let threshold = threshold.parse::<u64>().map_err(|_| reject())?;
    let policy_name = active
        .extra
        .get("native_policy_name")
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.is_empty() && name.len() <= 63)
        .ok_or_else(reject)?;
    let policy_oid = active
        .extra
        .get("native_policy_oid")
        .and_then(serde_json::Value::as_str)
        .filter(|oid| {
            !oid.is_empty() && oid.len() <= 20 && oid.bytes().all(|byte| byte.is_ascii_digit())
        })
        .ok_or_else(reject)?;
    let body_shape = active
        .extra
        .get("native_policy_body_shape")
        .and_then(serde_json::Value::as_str)
        .filter(|hash| hash.len() == 32 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(reject)?;
    let mut before_extra = before.extra.clone();
    let mut after_extra = after.extra.clone();
    for extra in [&mut before_extra, &mut after_extra] {
        extra.remove("estimated_rows");
        extra.remove("modified_at");
        extra.remove("native_security_shape");
        extra.remove("native_policy_empty_rls");
        extra.remove("native_policy_safe_predicate");
        extra.remove("native_policy_oid");
        extra.remove("native_policy_name");
        extra.remove("native_policy_body_shape");
    }
    if change.kind != SchemaChangeKind::Alter
        || before.kind != CatalogNodeKind::Table
        || after.kind != CatalogNodeKind::Table
        || before.id != after.id
        || before.native_id.is_none()
        || before.native_id != after.native_id
        || before.name != after.name
        || before.qualified_name != after.qualified_name
        || before.parent_id != after.parent_id
        || before.ordinal != after.ordinal
        || before.completeness != after.completeness
        || before.definition_digest != after.definition_digest
        || serde_json::to_value(&before.details).ok() != serde_json::to_value(&after.details).ok()
        || before.extra.get("migration_unsupported") != Some(&serde_json::Value::Bool(true))
        || after.extra.get("migration_unsupported") != Some(&serde_json::Value::Bool(true))
        || before.extra.get("native_security_shape") == after.extra.get("native_security_shape")
        || before
            .extra
            .get("native_security_shape")
            .and_then(serde_json::Value::as_str)
            .is_none()
        || after
            .extra
            .get("native_security_shape")
            .and_then(serde_json::Value::as_str)
            .is_none()
        || empty.extra.contains_key("native_policy_oid")
        || empty.extra.contains_key("native_policy_name")
        || empty.extra.contains_key("native_policy_body_shape")
        || empty.extra.contains_key("native_policy_safe_predicate")
        || active.extra.contains_key("native_policy_empty_rls")
        || active
            .extra
            .get("native_policy_oid")
            .and_then(serde_json::Value::as_str)
            != Some(policy_oid)
        || active
            .extra
            .get("native_policy_body_shape")
            .and_then(serde_json::Value::as_str)
            != Some(body_shape)
        || before_extra != after_extra
        || before_extra.keys().any(|key| key.starts_with("native_"))
        || !from_nodes.get(&before.id).is_some_and(|node| {
            serde_json::to_value(node).ok() == serde_json::to_value(before).ok()
        })
        || !to_nodes
            .get(&after.id)
            .is_some_and(|node| serde_json::to_value(node).ok() == serde_json::to_value(after).ok())
    {
        return Err(reject());
    }
    let before_children = partition_child_nodes(from, before);
    let after_children = partition_child_nodes(to, after);
    let [before_column] = before_children.as_slice() else {
        return Err(reject());
    };
    let [after_column] = after_children.as_slice() else {
        return Err(reject());
    };
    let sift_protocol::CatalogNodeDetails::Column { column } = &before_column.details else {
        return Err(reject());
    };
    if before_column.kind != CatalogNodeKind::Column
        || before_column.name != "id"
        || column.type_ref != sift_protocol::TypeRef::Primitive(sift_protocol::PrimitiveType::Int64)
        || column.nullable != Nullability::NotNullable
        || serde_json::to_value(before_column).ok() != serde_json::to_value(after_column).ok()
    {
        return Err(reject());
    }
    let table = qualified_object(
        Engine::Postgres,
        change,
        active,
        if create { to_nodes } else { from_nodes },
    )?;
    let policy = crate::ddl::quote_ident(policy_name, Engine::Postgres);
    let create_sql = format!(
        "CREATE POLICY {policy} ON {table} AS PERMISSIVE FOR SELECT TO PUBLIC USING (id > {threshold});"
    );
    let drop_sql = format!("DROP POLICY {policy} ON {table} RESTRICT;");
    let (forward, rollback) = if create {
        (create_sql, drop_sql)
    } else {
        (drop_sql, create_sql)
    };
    Ok(Some(PolicyCreateDropRender {
        change_id: change.id.clone(),
        forward,
        rollback,
    }))
}

fn render_postgres_policy_rename(
    change: &SchemaChange,
    diff: &SchemaDiff,
    from: &CatalogGraph,
    to: &CatalogGraph,
    from_nodes: &HashMap<sift_protocol::CatalogObjectId, &CatalogNode>,
    to_nodes: &HashMap<sift_protocol::CatalogObjectId, &CatalogNode>,
) -> Result<PolicyRenameRender, MigrationRenderError> {
    let reject = || MigrationRenderError::UnsupportedChange {
        change: change.id.clone(),
        kind: CatalogNodeKind::Table,
    };
    if diff.changes.len() != 1
        || from.database_identity != to.database_identity
        || !matches!(
            diff.from,
            CatalogSourceRef::Live { .. } | CatalogSourceRef::Snapshot { .. }
        )
        || !matches!(
            diff.to,
            CatalogSourceRef::Live { .. } | CatalogSourceRef::Snapshot { .. }
        )
        || [from, to].iter().any(|graph| {
            graph.provider.provider_id != Engine::Postgres.provider_id()
                || graph.data.coverage.state != CatalogCoverageState::Complete
                || !graph.data.coverage.requested_kinds.is_empty()
                || !graph.data.coverage.omitted_schemas.is_empty()
                || graph.data.coverage.truncated_at_nodes.is_some()
        })
    {
        return Err(reject());
    }
    let before = change.object_before.as_ref().ok_or_else(reject)?;
    let after = change.object_after.as_ref().ok_or_else(reject)?;
    let old_name = before
        .extra
        .get("native_policy_name")
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.is_empty() && name.len() <= 63)
        .ok_or_else(reject)?;
    let new_name = after
        .extra
        .get("native_policy_name")
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.is_empty() && name.len() <= 63 && *name != old_name)
        .ok_or_else(reject)?;
    let policy_oid = before
        .extra
        .get("native_policy_oid")
        .and_then(serde_json::Value::as_str)
        .filter(|oid| {
            !oid.is_empty() && oid.len() <= 20 && oid.bytes().all(|byte| byte.is_ascii_digit())
        })
        .ok_or_else(reject)?;
    let body_shape = before
        .extra
        .get("native_policy_body_shape")
        .and_then(serde_json::Value::as_str)
        .filter(|hash| hash.len() == 32 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(reject)?;
    let mut before_extra = before.extra.clone();
    let mut after_extra = after.extra.clone();
    for extra in [&mut before_extra, &mut after_extra] {
        extra.remove("estimated_rows");
        extra.remove("modified_at");
        extra.remove("native_policy_name");
        extra.remove("native_security_shape");
    }
    if before.kind != CatalogNodeKind::Table
        || after.kind != CatalogNodeKind::Table
        || before.id != after.id
        || before.native_id.is_none()
        || before.native_id != after.native_id
        || before.name != after.name
        || before.qualified_name != after.qualified_name
        || before.parent_id != after.parent_id
        || before.ordinal != after.ordinal
        || before.completeness != after.completeness
        || before.definition_digest != after.definition_digest
        || serde_json::to_value(&before.details).ok() != serde_json::to_value(&after.details).ok()
        || before.extra.get("migration_unsupported") != Some(&serde_json::Value::Bool(true))
        || after.extra.get("migration_unsupported") != Some(&serde_json::Value::Bool(true))
        || before.extra.get("native_security_shape") == after.extra.get("native_security_shape")
        || before
            .extra
            .get("native_security_shape")
            .and_then(serde_json::Value::as_str)
            .is_none()
        || after
            .extra
            .get("native_security_shape")
            .and_then(serde_json::Value::as_str)
            .is_none()
        || after
            .extra
            .get("native_policy_oid")
            .and_then(serde_json::Value::as_str)
            != Some(policy_oid)
        || after
            .extra
            .get("native_policy_body_shape")
            .and_then(serde_json::Value::as_str)
            != Some(body_shape)
        || before_extra != after_extra
        || before_extra.keys().any(|key| {
            key.starts_with("native_")
                && key != "native_policy_oid"
                && key != "native_policy_body_shape"
                && key != "native_policy_safe_predicate"
        })
        || !from_nodes.get(&before.id).is_some_and(|node| {
            serde_json::to_value(node).ok() == serde_json::to_value(before).ok()
        })
        || !to_nodes
            .get(&after.id)
            .is_some_and(|node| serde_json::to_value(node).ok() == serde_json::to_value(after).ok())
    {
        return Err(reject());
    }
    let before_children = partition_child_nodes(from, before);
    let after_children = partition_child_nodes(to, after);
    if before_children.len() != after_children.len()
        || before_children
            .iter()
            .zip(after_children)
            .any(|(left, right)| {
                serde_json::to_value(left).ok() != serde_json::to_value(right).ok()
            })
    {
        return Err(reject());
    }
    let table = qualified_object(Engine::Postgres, change, before, from_nodes)?;
    let forward = format!(
        "ALTER POLICY {} ON {table} RENAME TO {};",
        crate::ddl::quote_ident(old_name, Engine::Postgres),
        crate::ddl::quote_ident(new_name, Engine::Postgres)
    );
    let rollback = format!(
        "ALTER POLICY {} ON {table} RENAME TO {};",
        crate::ddl::quote_ident(new_name, Engine::Postgres),
        crate::ddl::quote_ident(old_name, Engine::Postgres)
    );
    Ok(PolicyRenameRender {
        change_id: change.id.clone(),
        forward,
        rollback,
    })
}

fn render_postgres_index_change(
    change: &SchemaChange,
    diff: &SchemaDiff,
    from: &CatalogGraph,
    to: &CatalogGraph,
    from_nodes: &HashMap<sift_protocol::CatalogObjectId, &CatalogNode>,
    to_nodes: &HashMap<sift_protocol::CatalogObjectId, &CatalogNode>,
) -> Result<String, MigrationRenderError> {
    let node = change
        .object_after
        .as_ref()
        .or(change.object_before.as_ref())
        .ok_or_else(|| MigrationRenderError::InvalidChangeShape(change.id.clone()))?;
    let reject = || MigrationRenderError::UnsupportedChange {
        change: change.id.clone(),
        kind: CatalogNodeKind::Index,
    };
    if !matches!(
        diff.from,
        CatalogSourceRef::Live { .. } | CatalogSourceRef::Snapshot { .. }
    ) || !matches!(
        diff.to,
        CatalogSourceRef::Live { .. } | CatalogSourceRef::Snapshot { .. }
    ) || [from, to].iter().any(|graph| {
        graph.provider.provider_id != Engine::Postgres.provider_id()
            || graph.data.coverage.state != CatalogCoverageState::Complete
            || !graph.data.coverage.requested_kinds.is_empty()
            || !graph.data.coverage.omitted_schemas.is_empty()
            || graph.data.coverage.truncated_at_nodes.is_some()
    }) {
        return Err(reject());
    }
    let (active_nodes, other_nodes) = if change.kind == SchemaChangeKind::Create {
        (to_nodes, from_nodes)
    } else {
        (from_nodes, to_nodes)
    };
    let table = node
        .parent_id
        .as_ref()
        .and_then(|id| active_nodes.get(id))
        .copied()
        .filter(|parent| parent.kind == CatalogNodeKind::Table)
        .ok_or_else(reject)?;
    let other_table = other_nodes
        .values()
        .copied()
        .find(|candidate| {
            candidate.kind == CatalogNodeKind::Table
                && candidate.qualified_name == table.qualified_name
        })
        .ok_or_else(reject)?;
    let stable_table = serde_json::to_value(&table.details).ok()
        == serde_json::to_value(&other_table.details).ok()
        && table.name == other_table.name
        && table.definition_digest == other_table.definition_digest
        && index_only_table_extra(table).is_some()
        && index_only_table_extra(table) == index_only_table_extra(other_table);
    if !stable_table
        || diff.changes.iter().any(|other| {
            if other.id == change.id {
                return false;
            }
            let affects_table = other.object_before.as_ref().is_some_and(|node| {
                node.id == table.id
                    || node.id == other_table.id
                    || node.parent_id.as_ref() == Some(&table.id)
                    || node.parent_id.as_ref() == Some(&other_table.id)
            }) || other.object_after.as_ref().is_some_and(|node| {
                node.id == table.id
                    || node.id == other_table.id
                    || node.parent_id.as_ref() == Some(&table.id)
                    || node.parent_id.as_ref() == Some(&other_table.id)
            });
            affects_table && !is_index_create_or_drop(other)
        })
        || node
            .extra
            .get("native_index_shape")
            .and_then(serde_json::Value::as_str)
            .is_none()
    {
        return Err(reject());
    }
    let ddl = node
        .extra
        .get("native_index_ddl")
        .and_then(serde_json::Value::as_str)
        .filter(|ddl| ddl.len() <= 65_536 && ddl.starts_with("CREATE ") && ddl.ends_with(';'))
        .ok_or_else(reject)?;
    match change.kind {
        SchemaChangeKind::Create => Ok(ddl.to_owned()),
        SchemaChangeKind::Drop => {
            let schema = schema_ancestor(change, table, active_nodes)?;
            Ok(format!(
                "DROP INDEX {}.{} RESTRICT;",
                crate::ddl::quote_ident(&schema.name, Engine::Postgres),
                crate::ddl::quote_ident(&node.name, Engine::Postgres)
            ))
        }
        _ => Err(reject()),
    }
}

fn index_only_table_extra(
    table: &CatalogNode,
) -> Option<std::collections::BTreeMap<String, serde_json::Value>> {
    let mut extra = table.extra.clone();
    extra.remove("estimated_rows");
    extra.remove("modified_at");
    if extra
        .keys()
        .any(|key| key.starts_with("native_") && key != "native_index_set_shape")
    {
        return None;
    }
    let has_indexes = extra.remove("native_index_set_shape").is_some();
    if has_indexes {
        extra.remove("migration_unsupported");
    } else if extra.get("migration_unsupported") == Some(&serde_json::Value::Bool(true)) {
        return None;
    }
    Some(extra)
}

fn implicitly_covered(
    change: &SchemaChange,
    created: &HashSet<sift_protocol::CatalogObjectId>,
    dropped: &HashSet<sift_protocol::CatalogObjectId>,
) -> bool {
    match change.kind {
        SchemaChangeKind::Create => change.object_after.as_ref().is_some_and(|node| {
            node.kind == CatalogNodeKind::Column
                && node
                    .parent_id
                    .as_ref()
                    .is_some_and(|parent| created.contains(parent))
        }),
        SchemaChangeKind::Drop => change
            .object_before
            .as_ref()
            .and_then(|node| node.parent_id.as_ref())
            .is_some_and(|parent| dropped.contains(parent)),
        _ => false,
    }
}

fn render_change(
    engine: Engine,
    change: &SchemaChange,
    from_nodes: &HashMap<sift_protocol::CatalogObjectId, &CatalogNode>,
    to_nodes: &HashMap<sift_protocol::CatalogObjectId, &CatalogNode>,
    to: &CatalogGraph,
) -> Result<Option<String>, MigrationRenderError> {
    let node = change
        .object_after
        .as_ref()
        .or(change.object_before.as_ref())
        .ok_or_else(|| MigrationRenderError::InvalidChangeShape(change.id.clone()))?;
    if engine == Engine::Sqlite {
        return sqlite_create_sql(change, node, to_nodes, to).map(Some);
    }
    // A catalog may be useful for navigation while lacking a lossless migration
    // projection. Refuse changes to the marked object or any of its children.
    for graph in [from_nodes, to_nodes] {
        let mut current = graph.get(&node.id).copied();
        for _ in 0..=graph.len() {
            let Some(candidate) = current else { break };
            if candidate
                .extra
                .get("migration_unsupported")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
            {
                return unsupported(change, node);
            }
            current = candidate
                .parent_id
                .as_ref()
                .and_then(|id| graph.get(id).copied());
        }
    }
    match change.kind {
        SchemaChangeKind::Unknown => unsupported(change, node),
        SchemaChangeKind::Alter => alter_sql(engine, change, from_nodes, to_nodes).map(Some),
        SchemaChangeKind::Create => create_sql(engine, change, node, to_nodes, to).map(Some),
        SchemaChangeKind::Drop => drop_sql(engine, change, node, from_nodes).map(Some),
        SchemaChangeKind::Rename | SchemaChangeKind::Move => {
            rename_or_move_sql(engine, change, from_nodes, to_nodes).map(Some)
        }
    }
}

fn sqlite_create_sql(
    change: &SchemaChange,
    node: &CatalogNode,
    nodes: &HashMap<sift_protocol::CatalogObjectId, &CatalogNode>,
    graph: &CatalogGraph,
) -> Result<String, MigrationRenderError> {
    if change.kind != SchemaChangeKind::Create || node.kind != CatalogNodeKind::Table {
        return unsupported(change, node);
    }
    let schema = schema_ancestor(change, node, nodes)?;
    if schema.name != "main"
        || graph.data.coverage.truncated_at_nodes.is_some()
        || node
            .extra
            .get("migration_unsupported")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
    {
        return unsupported(change, node);
    }
    let children = graph
        .data
        .nodes
        .iter()
        .filter(|candidate| candidate.parent_id.as_ref() == Some(&node.id))
        .collect::<Vec<_>>();
    if children.is_empty()
        || children
            .iter()
            .any(|child| child.kind != CatalogNodeKind::Column)
    {
        return unsupported(change, node);
    }
    let mut columns = children;
    columns.sort_by_key(|column| column.ordinal);
    let mut declarations = Vec::with_capacity(columns.len());
    for (position, column) in columns.into_iter().enumerate() {
        if column.ordinal != u32::try_from(position + 1).ok() {
            return unsupported(change, node);
        }
        let CatalogNodeDetails::Column { column: metadata } = &column.details else {
            return unsupported(change, node);
        };
        let Some(facets) = &metadata.facets.sqlite else {
            return unsupported(change, node);
        };
        let declared = facets.declared_type.to_ascii_uppercase();
        if !["INTEGER", "REAL", "TEXT", "BLOB", "NUMERIC"].contains(&declared.as_str())
            || facets.default_expr.is_some()
            || facets.primary_key_ordinal != 0
            || facets.hidden != 0
            || facets.virtual_table
            || metadata.auto_increment
        {
            return unsupported(change, node);
        }
        declarations.push(format!(
            "{} {}{}",
            crate::ddl::quote_ident(&column.name, Engine::Sqlite),
            declared,
            if metadata.nullable == Nullability::NotNullable {
                " NOT NULL"
            } else {
                ""
            }
        ));
    }
    let canonical = format!(
        "CREATE TABLE {} ({});",
        crate::ddl::quote_ident(&node.name, Engine::Sqlite),
        declarations.join(", ")
    );
    if node
        .extra
        .get("sqlite_safe_create_sql")
        .and_then(serde_json::Value::as_str)
        != Some(canonical.as_str())
    {
        return unsupported(change, node);
    }
    Ok(format!(
        "CREATE TABLE {}.{} ({});",
        crate::ddl::quote_ident("main", Engine::Sqlite),
        crate::ddl::quote_ident(&node.name, Engine::Sqlite),
        declarations.join(", ")
    ))
}

fn create_sql(
    engine: Engine,
    change: &SchemaChange,
    node: &CatalogNode,
    nodes: &HashMap<sift_protocol::CatalogObjectId, &CatalogNode>,
    graph: &CatalogGraph,
) -> Result<String, MigrationRenderError> {
    match node.kind {
        CatalogNodeKind::Schema => Ok(format!(
            "CREATE SCHEMA {};",
            crate::ddl::quote_ident(&node.name, engine)
        )),
        CatalogNodeKind::PartitionedTable => unsupported(change, node),
        CatalogNodeKind::Table => {
            let columns = graph
                .data
                .nodes
                .iter()
                .filter(|candidate| {
                    candidate.kind == CatalogNodeKind::Column
                        && candidate.parent_id.as_ref() == Some(&node.id)
                })
                .map(|column| render_column(engine, change, column))
                .collect::<Result<Vec<_>, _>>()?;
            if columns.is_empty() && engine == Engine::SqlServer {
                return unsupported(change, node);
            }
            Ok(format!(
                "CREATE TABLE {} (\n{}\n);",
                qualified_object(engine, change, node, nodes)?,
                columns
                    .iter()
                    .map(|column| format!("    {column}"))
                    .collect::<Vec<_>>()
                    .join(",\n")
            ))
        }
        CatalogNodeKind::Column => {
            let parent = parent(change, node, nodes)?;
            Ok(format!(
                "ALTER TABLE {} ADD {};",
                qualified_object(engine, change, parent, nodes)?,
                render_column(engine, change, node)?
            ))
        }
        CatalogNodeKind::Index => {
            let parent = parent(change, node, nodes)?;
            let CatalogNodeDetails::Index { index } = &node.details else {
                return unsupported(change, node);
            };
            let unique = if index.unique { "UNIQUE " } else { "" };
            let columns = index
                .columns
                .iter()
                .map(|column| crate::ddl::quote_ident(column, engine))
                .collect::<Vec<_>>()
                .join(", ");
            let mut sql = format!(
                "CREATE {unique}INDEX {} ON {} ({columns})",
                crate::ddl::quote_ident(&node.name, engine),
                qualified_object(engine, change, parent, nodes)?
            );
            if let Some(predicate) = &index.partial_predicate {
                sql.push_str(" WHERE ");
                sql.push_str(predicate);
            }
            sql.push(';');
            Ok(sql)
        }
        CatalogNodeKind::Constraint => {
            let parent = parent(change, node, nodes)?;
            let CatalogNodeDetails::Constraint { constraint } = &node.details else {
                return unsupported(change, node);
            };
            let clause = if let Some(definition) = &constraint.definition {
                definition.clone()
            } else if matches!(
                constraint.kind,
                ConstraintKind::PrimaryKey | ConstraintKind::Unique
            ) {
                let kind = if constraint.kind == ConstraintKind::PrimaryKey {
                    "PRIMARY KEY"
                } else {
                    "UNIQUE"
                };
                let columns = constraint
                    .columns
                    .iter()
                    .map(|column| crate::ddl::quote_ident(column, engine))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{kind} ({columns})")
            } else if constraint.kind == ConstraintKind::ForeignKey {
                render_foreign_key(engine, change, node, parent, nodes, graph)?
            } else {
                return unsupported(change, node);
            };
            Ok(format!(
                "ALTER TABLE {} ADD CONSTRAINT {} {clause};",
                qualified_object(engine, change, parent, nodes)?,
                crate::ddl::quote_ident(&node.name, engine)
            ))
        }
        _ => unsupported(change, node),
    }
}

fn render_foreign_key(
    engine: Engine,
    change: &SchemaChange,
    constraint: &CatalogNode,
    source_table: &CatalogNode,
    nodes: &HashMap<sift_protocol::CatalogObjectId, &CatalogNode>,
    graph: &CatalogGraph,
) -> Result<String, MigrationRenderError> {
    let edge = graph
        .data
        .edges
        .iter()
        .find(|edge| {
            edge.from == constraint.id
                && edge.kind == sift_protocol::CatalogEdgeKind::ForeignKey
                && edge.to.is_some()
                && !edge.column_pairs.is_empty()
        })
        .ok_or_else(|| MigrationRenderError::UnsupportedChange {
            change: change.id.clone(),
            kind: constraint.kind,
        })?;
    let target_table = edge
        .to
        .as_ref()
        .and_then(|id| nodes.get(id).copied())
        .ok_or_else(|| MigrationRenderError::IncompleteHierarchy(change.id.clone()))?;
    let mut source_columns = Vec::with_capacity(edge.column_pairs.len());
    let mut target_columns = Vec::with_capacity(edge.column_pairs.len());
    for pair in &edge.column_pairs {
        let source = nodes
            .get(&pair.from)
            .copied()
            .filter(|node| {
                node.kind == CatalogNodeKind::Column
                    && node.parent_id.as_ref() == Some(&source_table.id)
            })
            .ok_or_else(|| MigrationRenderError::IncompleteHierarchy(change.id.clone()))?;
        let target = nodes
            .get(&pair.to)
            .copied()
            .filter(|node| {
                node.kind == CatalogNodeKind::Column
                    && node.parent_id.as_ref() == Some(&target_table.id)
            })
            .ok_or_else(|| MigrationRenderError::IncompleteHierarchy(change.id.clone()))?;
        source_columns.push(crate::ddl::quote_ident(&source.name, engine));
        target_columns.push(crate::ddl::quote_ident(&target.name, engine));
    }
    Ok(format!(
        "FOREIGN KEY ({}) REFERENCES {} ({})",
        source_columns.join(", "),
        qualified_object(engine, change, target_table, nodes)?,
        target_columns.join(", ")
    ))
}

fn drop_sql(
    engine: Engine,
    change: &SchemaChange,
    node: &CatalogNode,
    nodes: &HashMap<sift_protocol::CatalogObjectId, &CatalogNode>,
) -> Result<String, MigrationRenderError> {
    let verb = match node.kind {
        CatalogNodeKind::Schema => "SCHEMA",
        CatalogNodeKind::Table | CatalogNodeKind::PartitionedTable => "TABLE",
        _ => "",
    };
    if !verb.is_empty() {
        return Ok(format!(
            "DROP {verb} {};",
            qualified_object(engine, change, node, nodes)?
        ));
    }
    let parent = parent(change, node, nodes)?;
    let table = qualified_object(engine, change, parent, nodes)?;
    match node.kind {
        CatalogNodeKind::Column => Ok(format!(
            "ALTER TABLE {table} DROP COLUMN {};",
            crate::ddl::quote_ident(&node.name, engine)
        )),
        CatalogNodeKind::Constraint => Ok(format!(
            "ALTER TABLE {table} DROP CONSTRAINT {};",
            crate::ddl::quote_ident(&node.name, engine)
        )),
        CatalogNodeKind::Index => match engine {
            Engine::Sqlite => unsupported(change, node),
            Engine::Postgres => {
                let schema = schema_ancestor(change, parent, nodes)?;
                Ok(format!(
                    "DROP INDEX {}.{};",
                    crate::ddl::quote_ident(&schema.name, engine),
                    crate::ddl::quote_ident(&node.name, engine)
                ))
            }
            Engine::SqlServer => Ok(format!(
                "DROP INDEX {} ON {table};",
                crate::ddl::quote_ident(&node.name, engine)
            )),
        },
        _ => unsupported(change, node),
    }
}

fn rename_or_move_sql(
    engine: Engine,
    change: &SchemaChange,
    from_nodes: &HashMap<sift_protocol::CatalogObjectId, &CatalogNode>,
    to_nodes: &HashMap<sift_protocol::CatalogObjectId, &CatalogNode>,
) -> Result<String, MigrationRenderError> {
    let before = change
        .object_before
        .as_ref()
        .ok_or_else(|| MigrationRenderError::InvalidChangeShape(change.id.clone()))?;
    let after = change
        .object_after
        .as_ref()
        .ok_or_else(|| MigrationRenderError::InvalidChangeShape(change.id.clone()))?;
    if change.kind == SchemaChangeKind::Move {
        let new_schema = schema_ancestor(change, after, to_nodes)?;
        let new_schema = crate::ddl::quote_ident(&new_schema.name, engine);
        return match (engine, before.kind) {
            (
                Engine::Postgres,
                CatalogNodeKind::Table
                | CatalogNodeKind::PartitionedTable
                | CatalogNodeKind::View
                | CatalogNodeKind::MaterializedView
                | CatalogNodeKind::Sequence,
            ) => Ok(format!(
                "ALTER {} {} SET SCHEMA {new_schema};",
                postgres_object_verb(before.kind),
                qualified_object(engine, change, before, from_nodes)?
            )),
            (
                Engine::SqlServer,
                CatalogNodeKind::Table
                | CatalogNodeKind::PartitionedTable
                | CatalogNodeKind::View
                | CatalogNodeKind::TableValuedFunction
                | CatalogNodeKind::ScalarFunction
                | CatalogNodeKind::Procedure
                | CatalogNodeKind::Sequence,
            ) => Ok(format!(
                "ALTER SCHEMA {new_schema} TRANSFER {};",
                qualified_object(engine, change, before, from_nodes)?
            )),
            _ => unsupported(change, after),
        };
    }
    let old = qualified_object(engine, change, before, from_nodes)?;
    let new_name = crate::ddl::quote_ident(&after.name, engine);
    match (engine, before.kind) {
        (Engine::Postgres, CatalogNodeKind::Schema) => {
            Ok(format!("ALTER SCHEMA {old} RENAME TO {new_name};"))
        }
        (Engine::Postgres, CatalogNodeKind::Table | CatalogNodeKind::PartitionedTable) => {
            Ok(format!("ALTER TABLE {old} RENAME TO {new_name};"))
        }
        (Engine::Postgres, CatalogNodeKind::Column) => {
            let table = qualified_object(
                engine,
                change,
                parent(change, before, from_nodes)?,
                from_nodes,
            )?;
            Ok(format!(
                "ALTER TABLE {table} RENAME COLUMN {} TO {new_name};",
                crate::ddl::quote_ident(&before.name, engine)
            ))
        }
        (Engine::SqlServer, CatalogNodeKind::Table | CatalogNodeKind::PartitionedTable) => {
            Ok(format!(
                "EXEC sp_rename N'{}', N'{}';",
                old.replace('\'', "''"),
                after.name.replace('\'', "''")
            ))
        }
        (Engine::SqlServer, CatalogNodeKind::Column) => {
            let table = qualified_object(
                engine,
                change,
                parent(change, before, from_nodes)?,
                from_nodes,
            )?;
            Ok(format!(
                "EXEC sp_rename N'{}.{}', N'{}', N'COLUMN';",
                table.replace('\'', "''"),
                before.name.replace('\'', "''"),
                after.name.replace('\'', "''")
            ))
        }
        _ => unsupported(change, after),
    }
}

fn alter_sql(
    engine: Engine,
    change: &SchemaChange,
    from_nodes: &HashMap<sift_protocol::CatalogObjectId, &CatalogNode>,
    to_nodes: &HashMap<sift_protocol::CatalogObjectId, &CatalogNode>,
) -> Result<String, MigrationRenderError> {
    let before = change
        .object_before
        .as_ref()
        .ok_or_else(|| MigrationRenderError::InvalidChangeShape(change.id.clone()))?;
    let after = change
        .object_after
        .as_ref()
        .ok_or_else(|| MigrationRenderError::InvalidChangeShape(change.id.clone()))?;
    let (
        CatalogNodeDetails::Column {
            column: before_column,
        },
        CatalogNodeDetails::Column {
            column: after_column,
        },
    ) = (&before.details, &after.details)
    else {
        return unsupported(change, after);
    };
    let table = qualified_object(
        engine,
        change,
        parent(change, before, from_nodes)?,
        from_nodes,
    )?;
    let name = crate::ddl::quote_ident(&before.name, engine);
    match engine {
        Engine::Sqlite => unsupported(change, before),
        Engine::Postgres => {
            let mut clauses = Vec::new();
            if before_column.type_ref != after_column.type_ref {
                clauses.push(format!(
                    "ALTER COLUMN {name} TYPE {}",
                    crate::ddl::type_to_sql(&after_column.type_ref, engine)
                ));
            }
            if before_column.nullable != after_column.nullable {
                clauses.push(format!(
                    "ALTER COLUMN {name} {} NOT NULL",
                    if after_column.nullable == Nullability::NotNullable {
                        "SET"
                    } else {
                        "DROP"
                    }
                ));
            }
            let before_default = column_default(before_column, engine);
            let after_default = column_default(after_column, engine);
            if before_default != after_default {
                clauses.push(match after_default {
                    Some(default) => format!("ALTER COLUMN {name} SET DEFAULT {default}"),
                    None => format!("ALTER COLUMN {name} DROP DEFAULT"),
                });
            }
            if clauses.is_empty() {
                return unsupported(change, after);
            }
            Ok(format!("ALTER TABLE {table} {};", clauses.join(", ")))
        }
        Engine::SqlServer => {
            if column_default(before_column, engine) != column_default(after_column, engine) {
                // SQL Server defaults are separately named constraints. The
                // graph must identify that constraint before it is safe to
                // replace; guessing a generated name is forbidden.
                return unsupported(change, after);
            }
            if before_column.type_ref == after_column.type_ref
                && before_column.nullable == after_column.nullable
            {
                return unsupported(change, after);
            }
            // Use the target hierarchy for validation too: a malformed diff
            // must not smuggle an unrelated after-node into executable SQL.
            let _ = parent(change, after, to_nodes)?;
            Ok(format!(
                "ALTER TABLE {table} ALTER COLUMN {name} {} {};",
                crate::ddl::type_to_sql(&after_column.type_ref, engine),
                if after_column.nullable == Nullability::NotNullable {
                    "NOT NULL"
                } else {
                    "NULL"
                }
            ))
        }
    }
}

fn column_default(column: &sift_protocol::ColumnMetadata, engine: Engine) -> Option<&str> {
    match engine {
        Engine::Postgres => column
            .facets
            .postgres
            .as_ref()
            .and_then(|facets| facets.default_expr.as_deref()),
        Engine::Sqlite => column
            .facets
            .sqlite
            .as_ref()
            .and_then(|f| f.default_expr.as_deref()),
        Engine::SqlServer => column
            .facets
            .sql_server
            .as_ref()
            .and_then(|facets| facets.default_expr.as_deref()),
    }
}

fn postgres_object_verb(kind: CatalogNodeKind) -> &'static str {
    match kind {
        CatalogNodeKind::View => "VIEW",
        CatalogNodeKind::MaterializedView => "MATERIALIZED VIEW",
        CatalogNodeKind::Sequence => "SEQUENCE",
        _ => "TABLE",
    }
}

fn render_column(
    engine: Engine,
    change: &SchemaChange,
    node: &CatalogNode,
) -> Result<String, MigrationRenderError> {
    let CatalogNodeDetails::Column { column } = &node.details else {
        return unsupported(change, node);
    };
    if column.auto_increment
        || column
            .facets
            .postgres
            .as_ref()
            .is_some_and(|facets| facets.is_identity)
    {
        return unsupported(change, node);
    }
    let mut sql = format!(
        "{} {}",
        crate::ddl::quote_ident(&node.name, engine),
        crate::ddl::type_to_sql(&column.type_ref, engine)
    );
    let default = match engine {
        Engine::Postgres => column
            .facets
            .postgres
            .as_ref()
            .and_then(|facets| facets.default_expr.as_deref()),
        Engine::Sqlite => column
            .facets
            .sqlite
            .as_ref()
            .and_then(|f| f.default_expr.as_deref()),
        Engine::SqlServer => column
            .facets
            .sql_server
            .as_ref()
            .and_then(|facets| facets.default_expr.as_deref()),
    };
    if let Some(default) = default {
        sql.push_str(" DEFAULT ");
        sql.push_str(default);
    }
    if column.nullable == Nullability::NotNullable {
        sql.push_str(" NOT NULL");
    }
    Ok(sql)
}

fn parent<'a>(
    change: &SchemaChange,
    node: &CatalogNode,
    nodes: &'a HashMap<sift_protocol::CatalogObjectId, &'a CatalogNode>,
) -> Result<&'a CatalogNode, MigrationRenderError> {
    node.parent_id
        .as_ref()
        .and_then(|id| nodes.get(id).copied())
        .ok_or_else(|| MigrationRenderError::IncompleteHierarchy(change.id.clone()))
}

fn schema_ancestor<'a>(
    change: &SchemaChange,
    node: &'a CatalogNode,
    nodes: &'a HashMap<sift_protocol::CatalogObjectId, &'a CatalogNode>,
) -> Result<&'a CatalogNode, MigrationRenderError> {
    let mut current = node;
    loop {
        if current.kind == CatalogNodeKind::Schema {
            return Ok(current);
        }
        current = parent(change, current, nodes)?;
    }
}

fn qualified_object(
    engine: Engine,
    change: &SchemaChange,
    node: &CatalogNode,
    nodes: &HashMap<sift_protocol::CatalogObjectId, &CatalogNode>,
) -> Result<String, MigrationRenderError> {
    if node.kind == CatalogNodeKind::Schema {
        return Ok(crate::ddl::quote_ident(&node.name, engine));
    }
    let schema = schema_ancestor(change, node, nodes)?;
    Ok(format!(
        "{}.{}",
        crate::ddl::quote_ident(&schema.name, engine),
        crate::ddl::quote_ident(&node.name, engine)
    ))
}

fn unsupported<T>(change: &SchemaChange, node: &CatalogNode) -> Result<T, MigrationRenderError> {
    Err(MigrationRenderError::UnsupportedChange {
        change: change.id.clone(),
        kind: node.kind,
    })
}

fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use sift_protocol::{
        CatalogCoverage, CatalogGraphOptions, CatalogRevision, CatalogSourceRef, CatalogTree,
        ColumnMetadata, ConstraintInfo, IndexInfo, IndexKind, Nullability, ObjectInfo, ObjectKind,
        PrimitiveType, ProviderRef, SchemaDiffRequest, SchemaTree, TypeRef,
    };

    use super::*;

    fn graph(with_table: bool) -> CatalogGraph {
        let objects = if with_table {
            let mut table = ObjectInfo::new("events", ObjectKind::Table);
            table.columns.push(ColumnMetadata {
                name: "id".into(),
                type_ref: TypeRef::Primitive(PrimitiveType::Int64),
                nullable: Nullability::NotNullable,
                auto_increment: false,
                primary_key: false,
                facets: Default::default(),
            });
            table.indexes.push(IndexInfo {
                name: "idx_events_id".into(),
                columns: vec!["id".into()],
                unique: false,
                primary_key: false,
                kind: IndexKind::Btree,
                partial_predicate: None,
            });
            table.constraints.push(ConstraintInfo {
                name: "pk_events".into(),
                kind: ConstraintKind::PrimaryKey,
                columns: vec!["id".into()],
                definition: None,
                references: None,
            });
            vec![table]
        } else {
            Vec::new()
        };
        let trees = vec![CatalogTree {
            name: "db".into(),
            schemas: vec![SchemaTree {
                name: "public".into(),
                objects,
            }],
        }];
        CatalogGraph {
            revision: CatalogRevision(1),
            content_digest: "catfp:test".into(),
            invalidation_epoch: 0,
            captured_at: chrono::Utc::now(),
            provider: ProviderRef {
                provider_id: sift_protocol::ProviderId::new("test/provider").unwrap(),
                dialect_id: sift_protocol::DialectId::new("test/dialect").unwrap(),
                provider_version: "1".into(),
            },
            database_identity: "db".into(),
            data: sift_core::catalog::graph_from_trees(&trees, CatalogCoverage::complete(), "db"),
        }
    }

    fn source() -> CatalogSourceRef {
        CatalogSourceRef::Live {
            expected_revision: CatalogRevision(1),
            options: CatalogGraphOptions::default(),
        }
    }

    fn graph_in_schema(schema: &str) -> CatalogGraph {
        let mut table = ObjectInfo::new("odd table", ObjectKind::Table);
        table.columns.push(ColumnMetadata::new(
            "id",
            TypeRef::Primitive(PrimitiveType::Int64),
        ));
        let trees = vec![CatalogTree {
            name: "db".into(),
            schemas: vec![SchemaTree {
                name: schema.into(),
                objects: vec![table],
            }],
        }];
        CatalogGraph {
            data: sift_core::catalog::graph_from_trees(&trees, CatalogCoverage::complete(), "db"),
            ..graph(false)
        }
    }

    #[test]
    fn table_creation_absorbs_child_columns_into_one_statement() {
        let from = graph(false);
        let to = graph(true);
        let request = SchemaDiffRequest {
            from: source(),
            to: source(),
            accepted_renames: Vec::new(),
            max_changes: None,
        };
        let diff = sift_core::schema_diff::diff_catalogs(
            request.from.clone(),
            &from,
            request.to.clone(),
            &to,
            &[],
            None,
        )
        .unwrap();
        let plan = render_plan(
            Engine::Postgres,
            &diff,
            &from,
            &to,
            &[],
            CatalogRevision(1),
            &MigrationOptions::default(),
        )
        .unwrap();
        assert_eq!(plan.groups[0].statements.len(), 3);
        assert_eq!(
            plan.groups[0].statements[0].sql,
            "CREATE TABLE \"public\".\"events\" (\n    \"id\" bigint NOT NULL\n);"
        );
        assert!(plan.digest.starts_with("migfp:"));
        assert!(plan.groups[0]
            .statements
            .iter()
            .any(|statement| statement.sql.starts_with("CREATE INDEX")));
        assert!(plan.groups[0]
            .statements
            .iter()
            .any(|statement| statement.sql.contains("ADD CONSTRAINT")));
        assert!(plan
            .rollback_groups
            .iter()
            .flat_map(|group| &group.statements)
            .any(|statement| statement.sql.starts_with("DROP TABLE")));
    }

    #[test]
    fn selected_changes_must_include_changed_prerequisites() {
        let from = graph(false);
        let to = graph(true);
        let diff = sift_core::schema_diff::diff_catalogs(source(), &from, source(), &to, &[], None)
            .unwrap();
        let index = diff
            .changes
            .iter()
            .find(|change| {
                change
                    .object_after
                    .as_ref()
                    .is_some_and(|node| node.kind == CatalogNodeKind::Index)
            })
            .unwrap();
        assert!(matches!(
            render_plan(
                Engine::Postgres,
                &diff,
                &from,
                &to,
                std::slice::from_ref(&index.id),
                CatalogRevision(1),
                &MigrationOptions::default(),
            ),
            Err(MigrationRenderError::MissingPrerequisite(_))
        ));
    }

    #[test]
    fn postgres_column_alter_is_rendered_from_normalized_target_state() {
        let from = graph(true);
        let mut to = from.clone();
        to.content_digest = "catfp:changed".into();
        let column = to
            .data
            .nodes
            .iter_mut()
            .find(|node| node.kind == CatalogNodeKind::Column)
            .unwrap();
        let CatalogNodeDetails::Column { column } = &mut column.details else {
            unreachable!()
        };
        column.type_ref = TypeRef::Primitive(PrimitiveType::Text);
        column.nullable = Nullability::Nullable;
        let diff = sift_core::schema_diff::diff_catalogs(source(), &from, source(), &to, &[], None)
            .unwrap();
        let plan = render_plan(
            Engine::Postgres,
            &diff,
            &from,
            &to,
            &[],
            CatalogRevision(1),
            &MigrationOptions::default(),
        )
        .unwrap();
        assert_eq!(plan.groups[0].statements.len(), 1);
        assert_eq!(
            plan.groups[0].statements[0].sql,
            "ALTER TABLE \"public\".\"events\" ALTER COLUMN \"id\" TYPE text, ALTER COLUMN \"id\" DROP NOT NULL;"
        );
    }

    #[test]
    fn native_fidelity_fence_rejects_changes_to_child_columns() {
        let mut from = graph(true);
        from.data
            .nodes
            .iter_mut()
            .find(|node| node.kind == CatalogNodeKind::Table)
            .unwrap()
            .extra
            .insert("migration_unsupported".into(), true.into());
        let mut to = from.clone();
        let node = to
            .data
            .nodes
            .iter_mut()
            .find(|node| node.kind == CatalogNodeKind::Column)
            .unwrap();
        let CatalogNodeDetails::Column { column } = &mut node.details else {
            unreachable!()
        };
        column.nullable = Nullability::Nullable;
        let diff = sift_core::schema_diff::diff_catalogs(source(), &from, source(), &to, &[], None)
            .unwrap();
        for engine in [Engine::Postgres, Engine::SqlServer] {
            assert!(matches!(
                render_plan(
                    engine,
                    &diff,
                    &from,
                    &to,
                    &[],
                    CatalogRevision(1),
                    &MigrationOptions::default()
                ),
                Err(MigrationRenderError::UnsupportedChange { .. })
            ));
        }
    }

    #[test]
    fn table_designer_add_column_renders_engine_aware_ddl() {
        let from = graph_in_schema("odd schema");
        let table = from
            .data
            .nodes
            .iter()
            .find(|node| node.kind == CatalogNodeKind::Table)
            .unwrap();
        let (to, _) = sift_core::catalog::apply_diagram_mutation(
            &from,
            &sift_protocol::CatalogDiagramMutation::AddColumn {
                table_id: table.id.clone(),
                name: "payload value".into(),
                type_ref: TypeRef::Primitive(PrimitiveType::Text),
                nullability: Nullability::NotNullable,
            },
        )
        .unwrap();
        let diff = sift_core::schema_diff::diff_catalogs(source(), &from, source(), &to, &[], None)
            .unwrap();
        for (engine, expected) in [
            (
                Engine::Postgres,
                "ALTER TABLE \"odd schema\".\"odd table\" ADD \"payload value\" text NOT NULL;",
            ),
            (
                Engine::SqlServer,
                "ALTER TABLE [odd schema].[odd table] ADD [payload value] nvarchar(max) NOT NULL;",
            ),
        ] {
            let plan = render_plan(
                engine,
                &diff,
                &from,
                &to,
                &[],
                CatalogRevision(1),
                &MigrationOptions::default(),
            )
            .unwrap();
            assert_eq!(plan.groups[0].statements[0].sql, expected);
        }
    }

    #[test]
    fn table_move_renders_after_target_schema_create_for_both_engines() {
        let from = graph_in_schema("old schema");
        let to = graph_in_schema("new schema");
        let before = from
            .data
            .nodes
            .iter()
            .find(|node| node.kind == CatalogNodeKind::Table)
            .unwrap();
        let after = to
            .data
            .nodes
            .iter()
            .find(|node| node.kind == CatalogNodeKind::Table)
            .unwrap();
        let diff = sift_core::schema_diff::diff_catalogs(
            source(),
            &from,
            source(),
            &to,
            &[sift_protocol::RenameMapping {
                from: before.id.clone(),
                to: after.id.clone(),
            }],
            None,
        )
        .unwrap();

        let postgres = render_plan(
            Engine::Postgres,
            &diff,
            &from,
            &to,
            &[],
            CatalogRevision(1),
            &MigrationOptions::default(),
        )
        .unwrap();
        assert_eq!(
            postgres.groups[0]
                .statements
                .iter()
                .map(|statement| statement.sql.as_str())
                .collect::<Vec<_>>(),
            vec![
                "CREATE SCHEMA \"new schema\";",
                "ALTER TABLE \"old schema\".\"odd table\" SET SCHEMA \"new schema\";",
                "DROP SCHEMA \"old schema\";",
            ]
        );

        let sql_server = render_plan(
            Engine::SqlServer,
            &diff,
            &from,
            &to,
            &[],
            CatalogRevision(1),
            &MigrationOptions::default(),
        )
        .unwrap();
        assert_eq!(
            sql_server.groups[0]
                .statements
                .iter()
                .map(|statement| statement.sql.as_str())
                .collect::<Vec<_>>(),
            vec![
                "CREATE SCHEMA [new schema];",
                "ALTER SCHEMA [new schema] TRANSFER [old schema].[odd table];",
                "DROP SCHEMA [old schema];",
            ]
        );
    }

    #[test]
    fn diagram_foreign_key_intent_uses_catalog_proven_pairs_for_safe_sql() {
        let mut parent = ObjectInfo::new("parent table", ObjectKind::Table);
        parent.columns.push(ColumnMetadata::new(
            "tenant id",
            TypeRef::Primitive(PrimitiveType::Int64),
        ));
        parent.columns.push(ColumnMetadata::new(
            "id",
            TypeRef::Primitive(PrimitiveType::Int64),
        ));
        let mut child = ObjectInfo::new("child table", ObjectKind::Table);
        child.columns.clone_from(&parent.columns);
        let trees = vec![CatalogTree {
            name: "db".into(),
            schemas: vec![SchemaTree {
                name: "odd schema".into(),
                objects: vec![parent, child],
            }],
        }];
        let mut from = graph(false);
        from.data = sift_core::catalog::graph_from_trees(&trees, CatalogCoverage::complete(), "db");
        let table = |name: &str| {
            from.data
                .nodes
                .iter()
                .find(|node| node.kind == CatalogNodeKind::Table && node.name == name)
                .unwrap()
        };
        let columns = |table: &CatalogNode| {
            from.data
                .nodes
                .iter()
                .filter(|node| {
                    node.kind == CatalogNodeKind::Column
                        && node.parent_id.as_ref() == Some(&table.id)
                })
                .map(|node| node.id.clone())
                .collect::<Vec<_>>()
        };
        let child = table("child table");
        let parent = table("parent table");
        let (to, _) = sift_core::catalog::apply_diagram_mutation(
            &from,
            &sift_protocol::CatalogDiagramMutation::AddForeignKey {
                table_id: child.id.clone(),
                name: "child parent fk".into(),
                columns: columns(child),
                referenced_table_id: parent.id.clone(),
                referenced_columns: columns(parent),
            },
        )
        .unwrap();
        let diff = sift_core::schema_diff::diff_catalogs(source(), &from, source(), &to, &[], None)
            .unwrap();

        let postgres = render_plan(
            Engine::Postgres,
            &diff,
            &from,
            &to,
            &[],
            CatalogRevision(1),
            &MigrationOptions::default(),
        )
        .unwrap();
        assert_eq!(
            postgres.groups[0].statements[0].sql,
            "ALTER TABLE \"odd schema\".\"child table\" ADD CONSTRAINT \"child parent fk\" FOREIGN KEY (\"id\", \"tenant id\") REFERENCES \"odd schema\".\"parent table\" (\"id\", \"tenant id\");"
        );
        let sql_server = render_plan(
            Engine::SqlServer,
            &diff,
            &from,
            &to,
            &[],
            CatalogRevision(1),
            &MigrationOptions::default(),
        )
        .unwrap();
        assert_eq!(
            sql_server.groups[0].statements[0].sql,
            "ALTER TABLE [odd schema].[child table] ADD CONSTRAINT [child parent fk] FOREIGN KEY ([id], [tenant id]) REFERENCES [odd schema].[parent table] ([id], [tenant id]);"
        );
    }
}
