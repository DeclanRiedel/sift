//! Pure reviewed-policy matching, not an authority store or dispatch path.
//!
//! Callers supply native-prepared material, never model-reported facts. They
//! must check live actor/source/policy/credential/lease/time, then apply the
//! returned debit inside the SAME immediate transaction as the native journal
//! claim. A replay reads that journal without evaluating or spending again.
use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use sift_protocol::*;
use uuid::Uuid;

#[cfg(test)]
mod tests;

const MAX_POLICY_BYTES: usize = 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DelegationError {
    #[error("Invalid delegation policy")]
    InvalidPolicy,
    #[error("Prepared action does not match the reviewed delegation")]
    NoMatchingRule,
    #[error("Delegation budget exhausted or inconsistent")]
    BudgetExceeded,
    #[error("Prepared action cannot be represented safely")]
    InvalidAction,
}

/// Counts are read from durable usage in the native claim transaction. Byte
/// reservations and unknown outcomes are never refunded by this evaluator.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DelegationUsage {
    pub tool_calls: u32,
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub rules: BTreeMap<Uuid, DelegationRuleUsage>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DelegationRuleUsage {
    pub uses: u32,
    pub submitted_rows: u32,
    pub schema_changes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegationDebit {
    pub rule_id: Uuid,
    pub after: DelegationUsage,
}

/// These gates come from current instance and tenant configuration, never from
/// a provider request or the encrypted human policy being evaluated.
#[derive(Debug, Clone, Copy, Default)]
pub struct ArbitrarySqlPolicy {
    pub instance_enabled: bool,
    pub tenant_enabled: bool,
}

pub enum PreparedDelegationAction<'a> {
    /// Existing SQL preparation must still reject unsupported control SQL.
    Sql {
        sql: &'a str,
        params: &'a [Value],
    },
    RowEdits(&'a EditSet),
    /// Changes must be the native diff used to render this exact plan.
    Migration {
        plan: &'a MigrationPlan,
        changes: &'a [SchemaChange],
    },
    QueryEdit {
        target: &'a AiQueryExecutionTarget,
        before_sha256: &'a str,
        base_revision: u64,
        replacement: &'a str,
    },
    Extension(&'a AiExtensionExecutionReview),
}

pub fn canonical_digest(value: &impl Serialize) -> Result<String, DelegationError> {
    use sha2::{Digest, Sha256};
    let mut value = serde_json::to_value(value).map_err(|_| DelegationError::InvalidAction)?;
    value.sort_all_objects();
    let bytes = serde_json::to_vec(&value).map_err(|_| DelegationError::InvalidAction)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

pub fn extension_contract_digest(
    review: &AiExtensionExecutionReview,
) -> Result<String, DelegationError> {
    canonical_digest(&(&review.tool, &review.prepared_contract, &review.context))
}

pub fn migration_contract_digest(
    plan: &MigrationPlan,
    changes: &[SchemaChange],
) -> Result<String, DelegationError> {
    canonical_digest(&(plan, changes))
}

/// Validates human rule material before it can be reviewed or persisted.
pub fn validate_policy(policy: &AiDelegationPolicy) -> Result<(), DelegationError> {
    let budget = policy.budget;
    if !(1..=100).contains(&budget.max_tool_calls)
        || !(1..=3600).contains(&budget.max_duration_secs)
        || !(1..=MAX_TOTAL_BYTES).contains(&budget.max_input_bytes)
        || !(1..=MAX_TOTAL_BYTES).contains(&budget.max_output_bytes)
        || policy.rules.is_empty()
        || policy.rules.len() > 64
        || serde_json::to_vec(policy)
            .map_err(|_| DelegationError::InvalidPolicy)?
            .len()
            > MAX_POLICY_BYTES
    {
        return Err(DelegationError::InvalidPolicy);
    }
    let mut ids = BTreeSet::new();
    for rule in &policy.rules {
        if rule.id.is_nil()
            || !ids.insert(rule.id)
            || !(1..=budget.max_tool_calls).contains(&rule.max_uses)
        {
            return Err(DelegationError::InvalidPolicy);
        }
        let valid = match &rule.action {
            AiDelegationActionRule::ExactSql { sql, .. } => !sql.trim().is_empty(),
            AiDelegationActionRule::ArbitrarySql {
                acknowledge_unbounded_effects,
            } => *acknowledge_unbounded_effects,
            AiDelegationActionRule::ExactRowEdits { edit_set } => {
                !edit_set.edits.is_empty()
                    && edit_set.edits.len() <= 100
                    && edit_set
                        .table
                        .schema
                        .as_ref()
                        .is_some_and(|s| !s.is_empty())
                    && !edit_set.table.name.is_empty()
            }
            AiDelegationActionRule::ExactMigration {
                contract_sha256,
                change_count,
            } => {
                sha256(contract_sha256)
                    && *change_count > 0
                    && u64::from(*change_count) * u64::from(rule.max_uses) <= 1000
            }
            AiDelegationActionRule::RowEdits {
                table,
                operations,
                write_columns,
                max_submitted_rows,
                max_total_submitted_rows,
            } => {
                !table.name.is_empty()
                    && table.schema.as_ref().is_some_and(|s| !s.is_empty())
                    && matches!(
                        table.kind,
                        None | Some(ObjectKind::Table | ObjectKind::PartitionedTable)
                    )
                    && table.routine_args.is_none()
                    && !operations.is_empty()
                    && write_columns.iter().all(|s| !s.is_empty())
                    && *max_submitted_rows > 0
                    && *max_submitted_rows <= *max_total_submitted_rows
                    && *max_total_submitted_rows <= 10000
            }
            AiDelegationActionRule::Migration {
                objects,
                changes,
                acknowledged_risks,
                max_changes,
                max_total_changes,
            } => {
                !objects.is_empty()
                    && objects.len() <= 256
                    && objects.iter().all(|o| !o.qualified_name.is_empty())
                    && !changes.is_empty()
                    && !changes.contains(&SchemaChangeKind::Unknown)
                    && !acknowledged_risks.is_empty()
                    && !acknowledged_risks.contains(&SchemaChangeRisk::Unknown)
                    && *max_changes > 0
                    && *max_changes <= *max_total_changes
                    && *max_total_changes <= 1000
            }
            AiDelegationActionRule::QueryEdit {
                target,
                before_sha256,
                replacement,
                ..
            } => {
                let valid_target = match target {
                    AiQueryExecutionTarget::RoomDocument {
                        room_id,
                        document_id,
                    } => *room_id > 0 && *document_id > 0,
                    AiQueryExecutionTarget::PrivateEditor { desktop_id, .. } => {
                        !desktop_id.is_nil()
                    }
                };
                valid_target
                    && sha256(before_sha256)
                    && match replacement {
                        AiDelegationTextRule::Exact { text } => text.len() <= MAX_POLICY_BYTES,
                        AiDelegationTextRule::Bounded { max_utf8_bytes } => {
                            (1..=MAX_POLICY_BYTES as u32).contains(max_utf8_bytes)
                        }
                    }
            }
            AiDelegationActionRule::Extension {
                contract_sha256,
                arguments,
                max_argument_bytes,
            } => {
                let mut nodes = 0;
                sha256(contract_sha256)
                    && (1..=MAX_POLICY_BYTES as u32).contains(max_argument_bytes)
                    && valid_argument_rule(arguments, 0, &mut nodes)
            }
        };
        if !valid {
            return Err(DelegationError::InvalidPolicy);
        }
    }
    Ok(())
}

fn sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn valid_argument_rule(rule: &AiDelegationArgumentRule, depth: usize, nodes: &mut usize) -> bool {
    *nodes += 1;
    if depth > 16 || *nodes > 256 {
        return false;
    }
    match rule {
        AiDelegationArgumentRule::Exact { .. }
        | AiDelegationArgumentRule::Boolean
        | AiDelegationArgumentRule::Null => true,
        AiDelegationArgumentRule::String {
            max_utf8_bytes,
            one_of,
        } => {
            (1..=MAX_POLICY_BYTES as u32).contains(max_utf8_bytes)
                && one_of.iter().all(|s| s.len() <= *max_utf8_bytes as usize)
        }
        AiDelegationArgumentRule::Integer { min, max } => min <= max,
        AiDelegationArgumentRule::Array { max_items, items } => {
            *max_items <= 10000 && valid_argument_rule(items, depth + 1, nodes)
        }
        AiDelegationArgumentRule::Object { fields, required } => {
            fields.len() <= 256
                && required.iter().all(|key| fields.contains_key(key))
                && fields
                    .values()
                    .all(|value| valid_argument_rule(value, depth + 1, nodes))
        }
    }
}

fn argument_matches(rule: &AiDelegationArgumentRule, value: &serde_json::Value) -> bool {
    match rule {
        AiDelegationArgumentRule::Exact { value: expected } => value == expected,
        AiDelegationArgumentRule::String {
            max_utf8_bytes,
            one_of,
        } => value.as_str().is_some_and(|s| {
            s.len() <= *max_utf8_bytes as usize
                && (one_of.is_empty() || one_of.iter().any(|choice| choice == s))
        }),
        AiDelegationArgumentRule::Integer { min, max } => {
            value.as_i64().is_some_and(|n| (*min..=*max).contains(&n))
        }
        AiDelegationArgumentRule::Boolean => value.is_boolean(),
        AiDelegationArgumentRule::Null => value.is_null(),
        AiDelegationArgumentRule::Array { max_items, items } => {
            value.as_array().is_some_and(|values| {
                values.len() <= *max_items as usize
                    && values.iter().all(|v| argument_matches(items, v))
            })
        }
        AiDelegationArgumentRule::Object { fields, required } => {
            value.as_object().is_some_and(|values| {
                required.iter().all(|k| values.contains_key(k))
                    && values
                        .iter()
                        .all(|(k, v)| fields.get(k).is_some_and(|r| argument_matches(r, v)))
            })
        }
    }
}

/// Reserve one immutable broker request BEFORE preparation, including reads,
/// staging, status and refused requests. Persist this debit even if action
/// matching later refuses. Request identity/replay and output-bound enforcement
/// belong to the durable caller; never reserve a retry of the same request.
pub fn reserve_tool_call(
    policy: &AiDelegationPolicy,
    usage: &DelegationUsage,
    input_bytes: u64,
    output_bytes: u64,
) -> Result<DelegationUsage, DelegationError> {
    validate_policy(policy)?;
    validate_usage(policy, usage)?;
    if input_bytes == 0 || output_bytes == 0 {
        return Err(DelegationError::BudgetExceeded);
    }
    let mut after = usage.clone();
    after.tool_calls = bounded_add(usage.tool_calls, 1, policy.budget.max_tool_calls)?;
    after.input_bytes = bounded_add(
        usage.input_bytes,
        input_bytes,
        policy.budget.max_input_bytes,
    )?;
    after.output_bytes = bounded_add(
        usage.output_bytes,
        output_bytes,
        policy.budget.max_output_bytes,
    )?;
    Ok(after)
}

/// Consume a matching rule INSIDE the native journal claim transaction. The
/// caller must prove that this exact original request has a durable, unused
/// tool-call reservation. This does not charge the broker request a second time.
/// Replays read the journal; they do not call either reservation function again.
pub fn reserve_action(
    policy: &AiDelegationPolicy,
    usage: &DelegationUsage,
    action: PreparedDelegationAction<'_>,
    arbitrary_sql: ArbitrarySqlPolicy,
) -> Result<DelegationDebit, DelegationError> {
    validate_policy(policy)?;
    if validate_usage(policy, usage)? >= u64::from(usage.tool_calls) {
        return Err(DelegationError::BudgetExceeded);
    }
    let mut after = usage.clone();
    let mut exhausted = false;
    for rule in &policy.rules {
        let Some((rows, changes)) = matches_action(&rule.action, &action, arbitrary_sql)? else {
            continue;
        };
        let used = usage.rules.get(&rule.id).copied().unwrap_or_default();
        let limits = rule_limits(rule);
        let counters: Result<_, DelegationError> = (|| {
            Ok(DelegationRuleUsage {
                uses: bounded_add(used.uses, 1, rule.max_uses)?,
                submitted_rows: bounded_add(used.submitted_rows, rows, limits.0)?,
                schema_changes: bounded_add(used.schema_changes, changes, limits.1)?,
            })
        })();
        match counters {
            Ok(counters) => {
                after.rules.insert(rule.id, counters);
                return Ok(DelegationDebit {
                    rule_id: rule.id,
                    after,
                });
            }
            Err(DelegationError::BudgetExceeded) => exhausted = true,
            Err(error) => return Err(error),
        }
    }
    Err(if exhausted {
        DelegationError::BudgetExceeded
    } else {
        DelegationError::NoMatchingRule
    })
}

fn validate_usage(
    policy: &AiDelegationPolicy,
    usage: &DelegationUsage,
) -> Result<u64, DelegationError> {
    // Malformed or foreign durable counters must never make quota available.
    let mut uses = 0_u64;
    if usage.rules.iter().any(|(id, used)| {
        let Some(rule) = policy.rules.iter().find(|r| r.id == *id) else {
            return true;
        };
        uses += u64::from(used.uses);
        let (rows, changes) = rule_limits(rule);
        used.uses > rule.max_uses
            || used.submitted_rows > rows
            || used.schema_changes > changes
            || (used.uses == 0 && (used.submitted_rows != 0 || used.schema_changes != 0))
    }) || uses > u64::from(usage.tool_calls)
        || usage.tool_calls > policy.budget.max_tool_calls
        || usage.input_bytes > policy.budget.max_input_bytes
        || usage.output_bytes > policy.budget.max_output_bytes
    {
        return Err(DelegationError::BudgetExceeded);
    }
    Ok(uses)
}

fn bounded_add<T>(before: T, debit: T, limit: T) -> Result<T, DelegationError>
where
    T: Copy + Ord + std::ops::Sub<Output = T> + std::ops::Add<Output = T>,
{
    if before > limit || debit > limit - before {
        return Err(DelegationError::BudgetExceeded);
    }
    Ok(before + debit)
}

fn rule_limits(rule: &AiDelegationRule) -> (u32, u32) {
    match &rule.action {
        AiDelegationActionRule::ExactRowEdits { edit_set } => {
            (edit_set.edits.len() as u32 * rule.max_uses, 0)
        }
        AiDelegationActionRule::ExactMigration { change_count, .. } => {
            (0, change_count * rule.max_uses)
        }
        AiDelegationActionRule::RowEdits {
            max_total_submitted_rows,
            ..
        } => (*max_total_submitted_rows, 0),
        AiDelegationActionRule::Migration {
            max_total_changes, ..
        } => (0, *max_total_changes),
        _ => (0, 0),
    }
}

fn matches_action(
    rule: &AiDelegationActionRule,
    action: &PreparedDelegationAction<'_>,
    arbitrary: ArbitrarySqlPolicy,
) -> Result<Option<(u32, u32)>, DelegationError> {
    let matched = match (rule, action) {
        (
            AiDelegationActionRule::ExactRowEdits { edit_set },
            PreparedDelegationAction::RowEdits(actual),
        ) => {
            return Ok((canonical_digest(edit_set)? == canonical_digest(actual)?)
                .then_some((actual.edits.len() as u32, 0)));
        }
        (
            AiDelegationActionRule::ExactMigration {
                contract_sha256,
                change_count,
            },
            PreparedDelegationAction::Migration { plan, changes },
        ) => {
            return Ok((changes.len() == *change_count as usize
                && *contract_sha256 == migration_contract_digest(plan, changes)?)
            .then_some((0, *change_count)));
        }
        (
            AiDelegationActionRule::ExactSql { sql, params },
            PreparedDelegationAction::Sql {
                sql: actual,
                params: actual_params,
            },
        ) => sql.as_str() == *actual && params.as_slice() == *actual_params,
        (
            AiDelegationActionRule::ArbitrarySql {
                acknowledge_unbounded_effects,
            },
            PreparedDelegationAction::Sql { .. },
        ) => {
            *acknowledge_unbounded_effects && arbitrary.instance_enabled && arbitrary.tenant_enabled
        }
        (
            AiDelegationActionRule::RowEdits {
                table,
                operations,
                write_columns,
                max_submitted_rows,
                ..
            },
            PreparedDelegationAction::RowEdits(edits),
        ) => {
            if canonical_digest(table)? != canonical_digest(&edits.table)?
                || edits.edits.is_empty()
                || edits.edits.len() > *max_submitted_rows as usize
            {
                return Ok(None);
            }
            let allowed = edits.edits.iter().all(|edit| {
                let (kind, written): (_, &[CellEdit]) = match edit {
                    RowEdit::Insert { values } => (EditStatementKind::Insert, values),
                    RowEdit::Update { changes, .. } => (EditStatementKind::Update, changes),
                    RowEdit::Delete { .. } => (EditStatementKind::Delete, &[]),
                };
                operations.contains(&kind)
                    && written
                        .iter()
                        .all(|cell| write_columns.contains(&cell.column))
            });
            return Ok(allowed.then_some((edits.edits.len() as u32, 0)));
        }
        (
            AiDelegationActionRule::Migration {
                objects,
                changes: kinds,
                acknowledged_risks,
                max_changes,
                ..
            },
            PreparedDelegationAction::Migration { plan, changes },
        ) => {
            if changes.is_empty() || changes.len() > *max_changes as usize {
                return Ok(None);
            }
            let mut ids = BTreeSet::new();
            let permitted = changes.iter().all(|change| {
                ids.insert(change.id.clone())
                    && kinds.contains(&change.kind)
                    && acknowledged_risks.contains(&change.risk)
                    && (change.object_before.is_some() || change.object_after.is_some())
                    && change
                        .object_before
                        .iter()
                        .chain(&change.object_after)
                        .all(|node| {
                            objects.iter().any(|o| {
                                o.kind == node.kind && o.qualified_name == node.qualified_name
                            })
                        })
            });
            let mut planned = BTreeSet::new();
            let statements: Vec<_> = plan.groups.iter().flat_map(|g| &g.statements).collect();
            let covered = !statements.is_empty()
                && statements.iter().all(|s| {
                    planned.extend(s.change_ids.iter().cloned());
                    !s.change_ids.is_empty() && acknowledged_risks.contains(&s.risk)
                });
            return Ok((permitted
                && covered
                && planned == ids
                && plan
                    .required_acknowledgements
                    .iter()
                    .all(|r| acknowledged_risks.contains(r)))
            .then_some((0, changes.len() as u32)));
        }
        (
            AiDelegationActionRule::QueryEdit {
                target,
                before_sha256,
                base_revision,
                replacement,
            },
            PreparedDelegationAction::QueryEdit {
                target: actual_target,
                before_sha256: actual_before,
                base_revision: actual_revision,
                replacement: text,
            },
        ) => {
            target == *actual_target
                && before_sha256.as_str() == *actual_before
                && base_revision == actual_revision
                && match replacement {
                    AiDelegationTextRule::Exact { text: expected } => expected.as_str() == *text,
                    AiDelegationTextRule::Bounded { max_utf8_bytes } => {
                        text.len() <= *max_utf8_bytes as usize
                    }
                }
        }
        (
            AiDelegationActionRule::Extension {
                contract_sha256,
                arguments,
                max_argument_bytes,
            },
            PreparedDelegationAction::Extension(review),
        ) => {
            *contract_sha256 == extension_contract_digest(review)?
                && serde_json::to_vec(&review.arguments)
                    .map_err(|_| DelegationError::InvalidAction)?
                    .len()
                    <= *max_argument_bytes as usize
                && argument_matches(arguments, &review.arguments)
        }
        _ => false,
    };
    Ok(matched.then_some((0, 0)))
}
