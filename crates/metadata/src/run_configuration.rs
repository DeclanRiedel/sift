use std::collections::HashSet;

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use sift_protocol::{
    Run, RunConfiguration, RunConfigurationId, RunErrorPolicy, RunId, RunLogEntry, RunState,
    RunStepResult, RunStepState, RunTransactionPolicy, RunTrigger, WorkspaceId,
};

use crate::workspace::{ensure_room_access, workspace_by_id_locked};
use crate::{
    now_text, parse_time_sql, MetadataError, MetadataStore, NewRunConfiguration, NewRunExecution,
    PrincipalId, Result, RunExecutionRecord,
};

const MAX_RUN_SCRIPTS: usize = 100;
const MAX_RUN_VARIABLES: usize = 64;
const MAX_RUN_LOGS: i64 = 10_000;
const MAX_RUN_LOG_BYTES: usize = 4 * 1024;

impl MetadataStore {
    pub fn latest_successful_run_for_commit(
        &self,
        workspace_id: sift_protocol::WorkspaceId,
        actor: PrincipalId,
        git_commit: &str,
    ) -> Result<Option<Run>> {
        let conn = self.conn()?;
        let workspace = workspace_by_id_locked(&conn, workspace_id)?;
        ensure_room_access(&conn, workspace.room_id, actor, false)?;
        let id = conn
            .query_row(
                "SELECT re.id FROM run_execution re
                 JOIN run_configuration rc ON rc.id = re.configuration_id
                 WHERE rc.workspace_id = ?1 AND re.state = 'succeeded'
                   AND json_extract(re.manifest_json, '$.git_commit') = ?2
                 ORDER BY re.finished_at DESC, re.id DESC LIMIT 1",
                params![workspace_id.0, git_commit],
                |row| row.get::<_, i64>(0).map(RunId),
            )
            .optional()?;
        id.map(|id| run_execution_by_id_locked(&conn, id).map(|record| record.run))
            .transpose()
    }

    pub fn create_run_configuration(
        &self,
        workspace_id: WorkspaceId,
        actor: PrincipalId,
        input: NewRunConfiguration,
    ) -> Result<RunConfiguration> {
        validate_configuration(&input)?;
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_configuration_targets(&tx, workspace_id, actor, &input)?;
        tx.execute(
            "INSERT INTO run_configuration (
                 workspace_id, name, scripts_json, connection_profile_id, target_schema,
                 variables_json, pre_tasks_json, transaction_policy, error_policy, revision,
                 created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, ?10, ?10)",
            params![
                workspace_id.0,
                input.name,
                serde_json::to_string(&input.scripts)?,
                input.connection_profile_id,
                input.target_schema,
                serde_json::to_string(&input.variables)?,
                serde_json::to_string(&input.pre_tasks)?,
                transaction_policy_text(input.transaction_policy),
                error_policy_text(input.error_policy),
                now,
            ],
        )
        .map_err(map_configuration_constraint)?;
        let configuration =
            run_configuration_by_id_locked(&tx, RunConfigurationId(tx.last_insert_rowid()))?;
        tx.commit()?;
        Ok(configuration)
    }

    pub fn list_run_configurations_for_principal(
        &self,
        workspace_id: WorkspaceId,
        actor: PrincipalId,
    ) -> Result<Vec<RunConfiguration>> {
        let conn = self.conn()?;
        let workspace = workspace_by_id_locked(&conn, workspace_id)?;
        ensure_room_access(&conn, workspace.room_id, actor, false)?;
        let mut statement =
            conn.prepare("SELECT id FROM run_configuration WHERE workspace_id = ?1 ORDER BY id")?;
        let ids = statement
            .query_map(params![workspace_id.0], |row| {
                row.get::<_, i64>(0).map(RunConfigurationId)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter()
            .map(|id| run_configuration_by_id_locked(&conn, id))
            .collect()
    }

    pub fn run_configuration_for_principal(
        &self,
        id: RunConfigurationId,
        actor: PrincipalId,
        writable: bool,
    ) -> Result<RunConfiguration> {
        let conn = self.conn()?;
        let configuration = run_configuration_by_id_locked(&conn, id)?;
        let workspace = workspace_by_id_locked(&conn, configuration.workspace_id)?;
        ensure_room_access(&conn, workspace.room_id, actor, writable)?;
        Ok(configuration)
    }

    pub fn update_run_configuration(
        &self,
        id: RunConfigurationId,
        actor: PrincipalId,
        expected_revision: u64,
        input: NewRunConfiguration,
    ) -> Result<RunConfiguration> {
        validate_configuration(&input)?;
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = run_configuration_by_id_locked(&tx, id)?;
        if current.revision != expected_revision {
            return Err(MetadataError::RunConfigurationRevisionConflict {
                expected: expected_revision,
                current: current.revision,
            });
        }
        validate_configuration_targets(&tx, current.workspace_id, actor, &input)?;
        tx.execute(
            "UPDATE run_configuration SET name = ?1, scripts_json = ?2,
                 connection_profile_id = ?3, target_schema = ?4, variables_json = ?5,
                 pre_tasks_json = ?6, transaction_policy = ?7, error_policy = ?8,
                 revision = revision + 1, updated_at = ?9 WHERE id = ?10",
            params![
                input.name,
                serde_json::to_string(&input.scripts)?,
                input.connection_profile_id,
                input.target_schema,
                serde_json::to_string(&input.variables)?,
                serde_json::to_string(&input.pre_tasks)?,
                transaction_policy_text(input.transaction_policy),
                error_policy_text(input.error_policy),
                now_text(),
                id.0,
            ],
        )
        .map_err(map_configuration_constraint)?;
        let updated = run_configuration_by_id_locked(&tx, id)?;
        tx.commit()?;
        Ok(updated)
    }

    pub fn delete_run_configuration(
        &self,
        id: RunConfigurationId,
        actor: PrincipalId,
        expected_revision: u64,
    ) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = run_configuration_by_id_locked(&tx, id)?;
        let workspace = workspace_by_id_locked(&tx, current.workspace_id)?;
        ensure_room_access(&tx, workspace.room_id, actor, true)?;
        if current.revision != expected_revision {
            return Err(MetadataError::RunConfigurationRevisionConflict {
                expected: expected_revision,
                current: current.revision,
            });
        }
        tx.execute("DELETE FROM run_configuration WHERE id = ?1", params![id.0])
            .map_err(|error| match error {
                rusqlite::Error::SqliteFailure(ref inner, _)
                    if inner.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    MetadataError::InvalidRunConfiguration
                }
                other => MetadataError::Sqlite(other),
            })?;
        tx.commit()?;
        Ok(())
    }

    pub fn create_run_execution(
        &self,
        actor: PrincipalId,
        input: NewRunExecution,
    ) -> Result<RunExecutionRecord> {
        if input.resolved_scripts_json.len() > 8 * 1024 * 1024 {
            return Err(MetadataError::InvalidRunConfiguration);
        }
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let configuration = run_configuration_by_id_locked(&tx, input.configuration_id)?;
        let workspace = workspace_by_id_locked(&tx, configuration.workspace_id)?;
        ensure_room_access(&tx, workspace.room_id, actor, true)?;
        let now = now_text();
        tx.execute(
            "INSERT INTO run_execution (
                 configuration_id, trigger_kind, actor_principal_id, state, manifest_json,
                 resolved_scripts_json, previous_run_id, revision, created_at
             ) VALUES (?1, ?2, ?3, 'queued', ?4, ?5, ?6, 1, ?7)",
            params![
                input.configuration_id.0,
                trigger_text(input.trigger),
                actor.0,
                serde_json::to_string(&input.manifest)?,
                input.resolved_scripts_json,
                input.previous_run_id.map(|id| id.0),
                now,
            ],
        )?;
        let run_id = RunId(tx.last_insert_rowid());
        for (ordinal, script) in input.manifest.scripts.iter().enumerate() {
            tx.execute(
                "INSERT INTO run_step_result (run_id, ordinal, node_id, state)
                 VALUES (?1, ?2, ?3, 'pending')",
                params![run_id.0, ordinal as i64, script.node_id.0],
            )?;
        }
        let run = run_execution_by_id_locked(&tx, run_id)?;
        tx.commit()?;
        Ok(run)
    }

    pub fn run_execution_for_principal(
        &self,
        id: RunId,
        actor: PrincipalId,
        writable: bool,
    ) -> Result<RunExecutionRecord> {
        let conn = self.conn()?;
        let record = run_execution_by_id_locked(&conn, id)?;
        let configuration = run_configuration_by_id_locked(&conn, record.run.configuration_id)?;
        let workspace = workspace_by_id_locked(&conn, configuration.workspace_id)?;
        ensure_room_access(&conn, workspace.room_id, actor, writable)?;
        Ok(record)
    }

    pub fn transition_run(
        &self,
        id: RunId,
        actor: PrincipalId,
        from: &[RunState],
        to: RunState,
    ) -> Result<RunExecutionRecord> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = run_execution_by_id_locked(&tx, id)?;
        let configuration = run_configuration_by_id_locked(&tx, current.run.configuration_id)?;
        let workspace = workspace_by_id_locked(&tx, configuration.workspace_id)?;
        ensure_room_access(&tx, workspace.room_id, actor, true)?;
        if !from.contains(&current.run.state) {
            return Err(MetadataError::InvalidRunTransition);
        }
        let now = now_text();
        let started = matches!(to, RunState::Running).then_some(now.clone());
        let finished = matches!(
            to,
            RunState::Succeeded
                | RunState::Failed
                | RunState::Cancelled
                | RunState::OutcomeUnknown
                | RunState::Blocked
                | RunState::Rejected
        )
        .then_some(now);
        tx.execute(
            "UPDATE run_execution SET state = ?1, revision = revision + 1,
                 started_at = COALESCE(started_at, ?2), finished_at = COALESCE(finished_at, ?3)
             WHERE id = ?4",
            params![state_text(to), started, finished, id.0],
        )?;
        let updated = run_execution_by_id_locked(&tx, id)?;
        tx.commit()?;
        Ok(updated)
    }

    pub fn request_run_cancellation(&self, id: RunId, actor: PrincipalId) -> Result<Run> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = run_execution_by_id_locked(&tx, id)?;
        let configuration = run_configuration_by_id_locked(&tx, current.run.configuration_id)?;
        let workspace = workspace_by_id_locked(&tx, configuration.workspace_id)?;
        ensure_room_access(&tx, workspace.room_id, actor, true)?;
        if !matches!(
            current.run.state,
            RunState::Queued | RunState::Admitted | RunState::Preparing | RunState::Running
        ) {
            return Err(MetadataError::InvalidRunTransition);
        }
        tx.execute(
            "UPDATE run_execution SET cancellation_requested = 1, revision = revision + 1
             WHERE id = ?1",
            params![id.0],
        )?;
        let run = run_execution_by_id_locked(&tx, id)?.run;
        tx.commit()?;
        Ok(run)
    }

    pub fn update_run_step(
        &self,
        run_id: RunId,
        ordinal: u32,
        state: RunStepState,
        row_count: Option<u64>,
        error_code: Option<&str>,
    ) -> Result<()> {
        let now = now_text();
        let started = (state == RunStepState::Running).then_some(now.clone());
        let finished = matches!(
            state,
            RunStepState::Succeeded | RunStepState::Failed | RunStepState::Cancelled
        )
        .then_some(now);
        let changed = self.conn()?.execute(
            "UPDATE run_step_result SET state = ?1, row_count = ?2, error_code = ?3,
                 started_at = COALESCE(started_at, ?4), finished_at = COALESCE(finished_at, ?5)
             WHERE run_id = ?6 AND ordinal = ?7",
            params![
                step_state_text(state),
                row_count.map(|value| value.min(i64::MAX as u64) as i64),
                error_code,
                started,
                finished,
                run_id.0,
                ordinal,
            ],
        )?;
        if changed == 1 {
            Ok(())
        } else {
            Err(MetadataError::RunNotFound(run_id))
        }
    }

    pub fn run_steps_for_principal(
        &self,
        run_id: RunId,
        actor: PrincipalId,
    ) -> Result<Vec<RunStepResult>> {
        self.run_execution_for_principal(run_id, actor, false)?;
        let conn = self.conn()?;
        let mut statement = conn.prepare(
            "SELECT ordinal, node_id, state, row_count, error_code, started_at, finished_at
             FROM run_step_result WHERE run_id = ?1 ORDER BY ordinal",
        )?;
        let rows = statement
            .query_map(params![run_id.0], |row| {
                let ordinal = row.get::<_, i64>(0)?;
                let row_count = row.get::<_, Option<i64>>(3)?;
                Ok(RunStepResult {
                    ordinal: u32::try_from(ordinal)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, ordinal))?,
                    node_id: sift_protocol::WorkspaceNodeId(row.get(1)?),
                    state: parse_step_state(row.get::<_, String>(2)?)?,
                    row_count: row_count.and_then(|value| u64::try_from(value).ok()),
                    error_code: row.get(4)?,
                    started_at: row
                        .get::<_, Option<String>>(5)?
                        .map(parse_time_sql)
                        .transpose()?,
                    finished_at: row
                        .get::<_, Option<String>>(6)?
                        .map(parse_time_sql)
                        .transpose()?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn append_run_log(&self, run_id: RunId, level: &str, message: &str) -> Result<()> {
        if !matches!(level, "info" | "warning" | "error")
            || message.is_empty()
            || message.len() > MAX_RUN_LOG_BYTES
            || message.contains('\0')
        {
            return Err(MetadataError::InvalidRunConfiguration);
        }
        let conn = self.conn()?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM run_log WHERE run_id = ?1",
            params![run_id.0],
            |row| row.get(0),
        )?;
        if count >= MAX_RUN_LOGS {
            return Err(MetadataError::InvalidRunConfiguration);
        }
        conn.execute(
            "INSERT INTO run_log (run_id, sequence, level, message, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![run_id.0, count + 1, level, message, now_text()],
        )?;
        Ok(())
    }

    pub fn run_logs_for_principal(
        &self,
        run_id: RunId,
        actor: PrincipalId,
        after: u64,
        limit: u32,
    ) -> Result<Vec<RunLogEntry>> {
        self.run_execution_for_principal(run_id, actor, false)?;
        let limit = limit.clamp(1, 500);
        let conn = self.conn()?;
        let mut statement = conn.prepare(
            "SELECT sequence, level, message, created_at FROM run_log
             WHERE run_id = ?1 AND sequence > ?2 ORDER BY sequence LIMIT ?3",
        )?;
        let rows = statement
            .query_map(params![run_id.0, after, limit], |row| {
                let sequence = row.get::<_, i64>(0)?;
                Ok(RunLogEntry {
                    sequence: u64::try_from(sequence)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, sequence))?,
                    level: row.get(1)?,
                    message: row.get(2)?,
                    created_at: parse_time_sql(row.get(3)?)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

fn validate_configuration(input: &NewRunConfiguration) -> Result<()> {
    if input.name.is_empty()
        || input.name.len() > 128
        || input.name.trim() != input.name
        || input.scripts.is_empty()
        || input.scripts.len() > MAX_RUN_SCRIPTS
        || input.variables.len() > MAX_RUN_VARIABLES
        || input.pre_tasks.len() > 8
        || input.connection_profile_id <= 0
        || input.target_schema.as_ref().is_some_and(|schema| {
            schema.is_empty() || schema.len() > 256 || schema.contains(['\0', '\n', '\r'])
        })
        || (input.transaction_policy == RunTransactionPolicy::AllScripts
            && input.error_policy == RunErrorPolicy::Continue)
        || (input.transaction_policy != RunTransactionPolicy::None
            && input
                .scripts
                .iter()
                .any(|script| script.transfer_recipe_id.is_some()))
    {
        return Err(MetadataError::InvalidRunConfiguration);
    }
    let mut nodes = HashSet::new();
    if input
        .scripts
        .iter()
        .any(|script| !nodes.insert(script.node_id.0))
    {
        return Err(MetadataError::InvalidRunConfiguration);
    }
    let mut variables = HashSet::new();
    if input.variables.iter().any(|variable| {
        variable.name.is_empty()
            || variable.name.len() > 64
            || !variable.name.bytes().enumerate().all(|(index, byte)| {
                byte == b'_'
                    || byte.is_ascii_alphanumeric() && (index > 0 || !byte.is_ascii_digit())
            })
            || !variables.insert(variable.name.as_str())
            || (variable.kind == sift_protocol::RunVariableKind::Secret
                && variable.persist_non_secret_value)
    }) {
        return Err(MetadataError::InvalidRunConfiguration);
    }
    if input
        .pre_tasks
        .iter()
        .enumerate()
        .any(|(index, task)| input.pre_tasks[..index].contains(task))
    {
        return Err(MetadataError::InvalidRunConfiguration);
    }
    Ok(())
}

fn validate_configuration_targets(
    conn: &Connection,
    workspace_id: WorkspaceId,
    actor: PrincipalId,
    input: &NewRunConfiguration,
) -> Result<()> {
    let workspace = workspace_by_id_locked(conn, workspace_id)?;
    ensure_room_access(conn, workspace.room_id, actor, true)?;
    let profile_valid: bool = conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM connection_profile cp
             JOIN room r ON r.tenant_id = cp.tenant_id
             WHERE cp.id = ?1 AND r.id = ?2
         )",
        params![input.connection_profile_id, workspace.room_id.0],
        |row| row.get(0),
    )?;
    if !profile_valid {
        return Err(MetadataError::InvalidRunConfiguration);
    }
    for script in &input.scripts {
        let valid: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM workspace_node
             WHERE id = ?1 AND workspace_id = ?2 AND kind = 'sql_document')",
            params![script.node_id.0, workspace_id.0],
            |row| row.get(0),
        )?;
        if !valid {
            return Err(MetadataError::InvalidRunConfiguration);
        }
        if let Some(recipe_id) = script.transfer_recipe_id {
            let recipe_valid: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM transfer_recipe
                 WHERE id = ?1 AND workspace_id = ?2 AND direction = 'export'
                   AND json_extract(source_json, '$.kind') = 'query'
                   AND json_extract(sink_json, '$.kind') = 'artifact')",
                params![recipe_id.0, workspace_id.0],
                |row| row.get(0),
            )?;
            if !recipe_valid {
                return Err(MetadataError::InvalidRunConfiguration);
            }
        }
    }
    Ok(())
}

pub(crate) fn run_configuration_by_id_locked(
    conn: &Connection,
    id: RunConfigurationId,
) -> Result<RunConfiguration> {
    conn.query_row(
        "SELECT id, workspace_id, name, scripts_json, connection_profile_id, target_schema,
                variables_json, pre_tasks_json, transaction_policy, error_policy, revision,
                created_at, updated_at
         FROM run_configuration WHERE id = ?1",
        params![id.0],
        |row| {
            let revision = row.get::<_, i64>(10)?;
            Ok(RunConfiguration {
                id: RunConfigurationId(row.get(0)?),
                workspace_id: WorkspaceId(row.get(1)?),
                name: row.get(2)?,
                scripts: serde_json::from_str(&row.get::<_, String>(3)?).map_err(json_sql_error)?,
                connection_profile_id: row.get(4)?,
                target_schema: row.get(5)?,
                variables: serde_json::from_str(&row.get::<_, String>(6)?)
                    .map_err(json_sql_error)?,
                pre_tasks: serde_json::from_str(&row.get::<_, String>(7)?)
                    .map_err(json_sql_error)?,
                transaction_policy: parse_transaction_policy(row.get::<_, String>(8)?)?,
                error_policy: parse_error_policy(row.get::<_, String>(9)?)?,
                revision: u64::try_from(revision)
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(10, revision))?,
                created_at: parse_time_sql(row.get(11)?)?,
                updated_at: parse_time_sql(row.get(12)?)?,
            })
        },
    )
    .optional()?
    .ok_or(MetadataError::RunConfigurationNotFound(id))
}

pub(crate) fn run_execution_by_id_locked(
    conn: &Connection,
    id: RunId,
) -> Result<RunExecutionRecord> {
    conn.query_row(
        "SELECT id, configuration_id, trigger_kind, actor_principal_id, state, manifest_json,
                resolved_scripts_json, previous_run_id, cancellation_requested, revision,
                created_at, started_at, finished_at
         FROM run_execution WHERE id = ?1",
        params![id.0],
        |row| {
            let revision = row.get::<_, i64>(9)?;
            Ok(RunExecutionRecord {
                run: Run {
                    id: RunId(row.get(0)?),
                    configuration_id: RunConfigurationId(row.get(1)?),
                    trigger: parse_trigger(row.get::<_, String>(2)?)?,
                    actor_principal_id: row.get(3)?,
                    state: parse_state(row.get::<_, String>(4)?)?,
                    manifest: serde_json::from_str(&row.get::<_, String>(5)?)
                        .map_err(json_sql_error)?,
                    previous_run_id: row.get::<_, Option<i64>>(7)?.map(RunId),
                    revision: u64::try_from(revision)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(9, revision))?,
                    created_at: parse_time_sql(row.get(10)?)?,
                    started_at: row
                        .get::<_, Option<String>>(11)?
                        .map(parse_time_sql)
                        .transpose()?,
                    finished_at: row
                        .get::<_, Option<String>>(12)?
                        .map(parse_time_sql)
                        .transpose()?,
                },
                resolved_scripts_json: row.get(6)?,
                cancellation_requested: row.get(8)?,
            })
        },
    )
    .optional()?
    .ok_or(MetadataError::RunNotFound(id))
}

fn transaction_policy_text(value: RunTransactionPolicy) -> &'static str {
    match value {
        RunTransactionPolicy::None => "none",
        RunTransactionPolicy::PerScript => "per_script",
        RunTransactionPolicy::AllScripts => "all_scripts",
    }
}

fn error_policy_text(value: RunErrorPolicy) -> &'static str {
    match value {
        RunErrorPolicy::Stop => "stop",
        RunErrorPolicy::Continue => "continue",
    }
}

fn trigger_text(value: RunTrigger) -> &'static str {
    match value {
        RunTrigger::Interactive => "interactive",
        RunTrigger::Schedule => "schedule",
        RunTrigger::Rerun => "rerun",
    }
}

fn state_text(value: RunState) -> &'static str {
    match value {
        RunState::Queued => "queued",
        RunState::Admitted => "admitted",
        RunState::Preparing => "preparing",
        RunState::Running => "running",
        RunState::Succeeded => "succeeded",
        RunState::Failed => "failed",
        RunState::Cancelled => "cancelled",
        RunState::OutcomeUnknown => "outcome_unknown",
        RunState::Blocked => "blocked",
        RunState::Rejected => "rejected",
    }
}

fn step_state_text(value: RunStepState) -> &'static str {
    match value {
        RunStepState::Pending => "pending",
        RunStepState::Running => "running",
        RunStepState::Succeeded => "succeeded",
        RunStepState::Failed => "failed",
        RunStepState::Cancelled => "cancelled",
    }
}

fn parse_transaction_policy(value: String) -> rusqlite::Result<RunTransactionPolicy> {
    match value.as_str() {
        "none" => Ok(RunTransactionPolicy::None),
        "per_script" => Ok(RunTransactionPolicy::PerScript),
        "all_scripts" => Ok(RunTransactionPolicy::AllScripts),
        _ => Err(enum_sql_error("transaction_policy", value)),
    }
}

fn parse_error_policy(value: String) -> rusqlite::Result<RunErrorPolicy> {
    match value.as_str() {
        "stop" => Ok(RunErrorPolicy::Stop),
        "continue" => Ok(RunErrorPolicy::Continue),
        _ => Err(enum_sql_error("error_policy", value)),
    }
}

fn parse_trigger(value: String) -> rusqlite::Result<RunTrigger> {
    match value.as_str() {
        "interactive" => Ok(RunTrigger::Interactive),
        "schedule" => Ok(RunTrigger::Schedule),
        "rerun" => Ok(RunTrigger::Rerun),
        _ => Err(enum_sql_error("trigger", value)),
    }
}

fn parse_state(value: String) -> rusqlite::Result<RunState> {
    match value.as_str() {
        "queued" => Ok(RunState::Queued),
        "admitted" => Ok(RunState::Admitted),
        "preparing" => Ok(RunState::Preparing),
        "running" => Ok(RunState::Running),
        "succeeded" => Ok(RunState::Succeeded),
        "failed" => Ok(RunState::Failed),
        "cancelled" => Ok(RunState::Cancelled),
        "outcome_unknown" => Ok(RunState::OutcomeUnknown),
        "blocked" => Ok(RunState::Blocked),
        "rejected" => Ok(RunState::Rejected),
        _ => Err(enum_sql_error("run_state", value)),
    }
}

fn parse_step_state(value: String) -> rusqlite::Result<RunStepState> {
    match value.as_str() {
        "pending" => Ok(RunStepState::Pending),
        "running" => Ok(RunStepState::Running),
        "succeeded" => Ok(RunStepState::Succeeded),
        "failed" => Ok(RunStepState::Failed),
        "cancelled" => Ok(RunStepState::Cancelled),
        _ => Err(enum_sql_error("run_step_state", value)),
    }
}

fn enum_sql_error(field: &'static str, value: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        format!("invalid {field}: {value}").into(),
    )
}

fn json_sql_error(error: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}

fn map_configuration_constraint(error: rusqlite::Error) -> MetadataError {
    match error {
        rusqlite::Error::SqliteFailure(ref inner, _)
            if inner.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            MetadataError::InvalidRunConfiguration
        }
        other => MetadataError::Sqlite(other),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sift_protocol::{
        ConnectionSpec, Engine, RunManifest, RunManifestScript, RunScriptStep,
        ScheduleConcurrencyPolicy, ScheduleMisfirePolicy, ScheduleOccurrenceState,
        ScriptRevisionPolicy, TransferDirection, TransferEndpoint, WorkspaceNodeKind,
        WorkspacePath,
    };

    use crate::{
        CredentialMode, MemorySecretStore, NewConnectionProfile, NewRoom, NewTransferRecipe,
        NewWorkspaceNode, RoomKind, TenantId,
    };

    use super::*;

    #[tokio::test]
    async fn configurations_and_runs_are_revisioned_and_ordered() {
        let store = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        store.bootstrap_local("run-test").unwrap();
        let actor = PrincipalId(1);
        let room = store
            .create_room(
                TenantId(1),
                actor,
                NewRoom {
                    name: "run-room".into(),
                    kind: RoomKind::Shared,
                },
            )
            .unwrap();
        let workspace = store
            .create_workspace(room.id, actor, "run-workspace")
            .unwrap();
        let profile = store
            .upsert_connection_profile(
                TenantId(1),
                actor,
                NewConnectionProfile {
                    name: "run-profile".into(),
                    provider_id: Engine::Postgres.provider_id(),
                    configuration: serde_json::to_value(ConnectionSpec {
                        host: "localhost".into(),
                        port: Some(5432),
                        database: Some("sift".into()),
                        user: "sift".into(),
                        password: None,
                        ssl_mode: None,
                        engine_specific: None,
                    })
                    .unwrap(),
                    semantic_engine: Some(Engine::Postgres),
                    credentials: Some(serde_json::json!({"password": "test-only"})),
                    credential_mode: CredentialMode::Shared,
                    tags: vec![],
                },
            )
            .await
            .unwrap();

        let mut current = workspace;
        let mut node_ids = Vec::new();
        for (path, byte) in [("first.sql", 1), ("second.sql", 2)] {
            let (workspace, node) = store
                .create_workspace_node(
                    current.id,
                    actor,
                    current.revision,
                    NewWorkspaceNode {
                        parent_id: None,
                        path: WorkspacePath::new(path).unwrap(),
                        kind: WorkspaceNodeKind::SqlDocument,
                        initial_snapshot: Some(vec![byte]),
                        initial_snapshot_version: Some(vec![byte]),
                    },
                )
                .unwrap();
            current = workspace;
            node_ids.push(node.id);
        }
        let scripts = node_ids
            .iter()
            .copied()
            .map(|node_id| RunScriptStep {
                node_id,
                revision_policy: ScriptRevisionPolicy::LatestAtRunStart,
                pinned_digest: None,
                transfer_recipe_id: None,
            })
            .collect::<Vec<_>>();
        let input = NewRunConfiguration {
            name: "ordered".into(),
            scripts: scripts.clone(),
            connection_profile_id: profile.id.0,
            target_schema: Some("public".into()),
            variables: vec![],
            pre_tasks: vec![],
            transaction_policy: RunTransactionPolicy::PerScript,
            error_policy: RunErrorPolicy::Continue,
        };
        let configuration = store
            .create_run_configuration(current.id, actor, input.clone())
            .unwrap();
        assert!(matches!(
            store.update_run_configuration(configuration.id, actor, 2, input.clone()),
            Err(MetadataError::RunConfigurationRevisionConflict { .. })
        ));
        let configuration = store
            .update_run_configuration(configuration.id, actor, 1, input)
            .unwrap();
        assert_eq!(configuration.revision, 2);

        let recipe = store
            .create_transfer_recipe(
                current.id,
                actor,
                NewTransferRecipe {
                    name: "scheduled-export".into(),
                    direction: TransferDirection::Export,
                    source: TransferEndpoint::Query,
                    sink: TransferEndpoint::Artifact,
                    format_id: "csv".into(),
                    format_version: "1".into(),
                    options: serde_json::json!({}),
                },
            )
            .unwrap();
        let recipe_input = NewRunConfiguration {
            name: "recipe-step".into(),
            scripts: vec![RunScriptStep {
                transfer_recipe_id: Some(recipe.id),
                ..scripts[0].clone()
            }],
            connection_profile_id: profile.id.0,
            target_schema: None,
            variables: vec![],
            pre_tasks: vec![],
            transaction_policy: RunTransactionPolicy::None,
            error_policy: RunErrorPolicy::Stop,
        };
        assert!(store
            .create_run_configuration(current.id, actor, recipe_input.clone())
            .is_ok());
        assert!(matches!(
            store.create_run_configuration(
                current.id,
                actor,
                NewRunConfiguration {
                    transaction_policy: RunTransactionPolicy::PerScript,
                    ..recipe_input
                }
            ),
            Err(MetadataError::InvalidRunConfiguration)
        ));

        let manifest = RunManifest {
            workspace_revision: current.revision,
            git_commit: None,
            scripts: node_ids
                .iter()
                .map(|node_id| RunManifestScript {
                    node_id: *node_id,
                    content_digest: format!("content-{}", node_id.0),
                    document_frontier_digest: format!("frontier-{}", node_id.0),
                })
                .collect(),
            connection_profile_id: profile.id.0,
            target_schema: Some("public".into()),
            provider_id: profile.provider_id.as_str().into(),
            variable_names: vec![],
            pre_tasks: vec![],
        };
        let run = store
            .create_run_execution(
                actor,
                NewRunExecution {
                    configuration_id: configuration.id,
                    trigger: RunTrigger::Interactive,
                    manifest: manifest.clone(),
                    resolved_scripts_json: "{}".into(),
                    previous_run_id: None,
                },
            )
            .unwrap()
            .run;
        let steps = store.run_steps_for_principal(run.id, actor).unwrap();
        assert_eq!(
            steps.iter().map(|step| step.node_id).collect::<Vec<_>>(),
            node_ids
        );
        for (from, to) in [
            (RunState::Queued, RunState::Admitted),
            (RunState::Admitted, RunState::Preparing),
            (RunState::Preparing, RunState::Running),
        ] {
            store.transition_run(run.id, actor, &[from], to).unwrap();
        }
        store
            .update_run_step(run.id, 0, RunStepState::Running, None, None)
            .unwrap();
        store
            .update_run_step(run.id, 0, RunStepState::Succeeded, Some(3), None)
            .unwrap();
        store
            .append_run_log(run.id, "info", "script succeeded")
            .unwrap();
        assert_eq!(
            store.run_logs_for_principal(run.id, actor, 0, 100).unwrap()[0].message,
            "script succeeded"
        );
        let terminal = store
            .transition_run(run.id, actor, &[RunState::Running], RunState::Succeeded)
            .unwrap();
        assert_eq!(terminal.run.state, RunState::Succeeded);
        assert!(terminal.run.finished_at.is_some());

        let first_fire = "2026-08-11T12:00:00Z".parse().unwrap();
        let second_fire = "2026-08-11T12:01:00Z".parse().unwrap();
        let schedule = store
            .create_run_schedule(
                configuration.id,
                actor,
                crate::NewRunSchedule {
                    cron: "* * * * *".into(),
                    timezone: "UTC".into(),
                    misfire_policy: ScheduleMisfirePolicy::RunOnce,
                    concurrency_policy: ScheduleConcurrencyPolicy::QueueOne,
                    enabled: true,
                    next_fire_at: Some(first_fire),
                },
            )
            .unwrap();
        let occurrence = store
            .advance_and_enqueue_schedule(schedule.id, first_fire, second_fire, true)
            .unwrap()
            .unwrap();
        assert!(store
            .advance_and_enqueue_schedule(schedule.id, first_fire, second_fire, true)
            .unwrap()
            .is_none());
        let claimed = store
            .claim_queued_occurrences(first_fire, "generation:test", 10)
            .unwrap();
        assert_eq!(claimed.len(), 1);
        store
            .finish_schedule_occurrence(
                occurrence.id,
                ScheduleOccurrenceState::Blocked,
                Some("test_block"),
            )
            .unwrap();
        let resumed = store
            .resume_schedule_occurrence(occurrence.id, actor)
            .unwrap();
        assert_eq!(resumed.state, ScheduleOccurrenceState::Queued);

        let third_fire = "2026-08-11T12:02:00Z".parse().unwrap();
        assert!(store
            .advance_and_enqueue_schedule(schedule.id, second_fire, third_fire, true)
            .unwrap()
            .is_none());
        let claimed = store
            .claim_queued_occurrences(second_fire, "generation:test", 10)
            .unwrap();
        let run = store
            .create_run_execution(
                actor,
                NewRunExecution {
                    configuration_id: configuration.id,
                    trigger: RunTrigger::Schedule,
                    manifest,
                    resolved_scripts_json: "{}".into(),
                    previous_run_id: None,
                },
            )
            .unwrap()
            .run;
        for (from, to) in [
            (RunState::Queued, RunState::Admitted),
            (RunState::Admitted, RunState::Preparing),
            (RunState::Preparing, RunState::Running),
        ] {
            store.transition_run(run.id, actor, &[from], to).unwrap();
        }
        store
            .attach_occurrence_run(claimed[0].1.id, run.id)
            .unwrap();
        store.recover_interrupted_runs(third_fire).unwrap();
        assert_eq!(
            store
                .run_execution_for_principal(run.id, actor, false)
                .unwrap()
                .run
                .state,
            RunState::OutcomeUnknown
        );
        assert_eq!(
            store
                .schedule_occurrence_for_principal(claimed[0].1.id, actor, false)
                .unwrap()
                .state,
            ScheduleOccurrenceState::OutcomeUnknown
        );

        {
            let conn = store.conn().unwrap();
            conn.execute(
                "UPDATE run_execution SET state = 'running', finished_at = NULL WHERE id = ?1",
                params![run.id.0],
            )
            .unwrap();
            conn.execute(
                "UPDATE schedule_occurrence SET state = 'leased', lease_owner = 'stale',
                 lease_expires_at = '2099-01-01T00:00:00Z', finished_at = NULL WHERE id = ?1",
                params![claimed[0].1.id.0],
            )
            .unwrap();
        }
        store.sanitize_phase_l_backup_snapshot().unwrap();
        assert_eq!(
            store
                .run_execution_for_principal(run.id, actor, false)
                .unwrap()
                .run
                .state,
            RunState::OutcomeUnknown
        );
        let (state, owner, expiry): (String, Option<String>, Option<String>) = store
            .conn()
            .unwrap()
            .query_row(
                "SELECT state, lease_owner, lease_expires_at FROM schedule_occurrence WHERE id = ?1",
                params![claimed[0].1.id.0],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(state, "outcome_unknown");
        assert_eq!(owner, None);
        assert_eq!(expiry, None);
    }
}
