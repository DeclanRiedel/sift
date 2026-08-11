use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use sift_protocol::{
    RunConfigurationId, RunSchedule, ScheduleConcurrencyPolicy, ScheduleId, ScheduleMisfirePolicy,
    ScheduleOccurrence, ScheduleOccurrenceId, ScheduleOccurrenceState,
};

use crate::run_configuration::run_configuration_by_id_locked;
use crate::workspace::{ensure_room_access, ensure_room_owner, workspace_by_id_locked};
use crate::{
    now_text, parse_time_sql, MetadataError, MetadataStore, NewRunSchedule, PrincipalId, Result,
};

const MAX_SCHEDULES_PER_CONFIGURATION: i64 = 16;

impl MetadataStore {
    pub fn create_run_schedule(
        &self,
        configuration_id: RunConfigurationId,
        actor: PrincipalId,
        input: NewRunSchedule,
    ) -> Result<RunSchedule> {
        validate_input(&input)?;
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let configuration = run_configuration_by_id_locked(&tx, configuration_id)?;
        let workspace = workspace_by_id_locked(&tx, configuration.workspace_id)?;
        ensure_room_owner(&tx, workspace.room_id, actor)?;
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM run_schedule WHERE configuration_id = ?1",
            params![configuration_id.0],
            |row| row.get(0),
        )?;
        if count >= MAX_SCHEDULES_PER_CONFIGURATION {
            return Err(MetadataError::InvalidRunSchedule);
        }
        let now = now_text();
        tx.execute(
            "INSERT INTO run_schedule (
                 configuration_id, owner_principal_id, cron, timezone, misfire_policy,
                 concurrency_json, enabled, next_fire_at, revision, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1, ?9, ?9)",
            params![
                configuration_id.0,
                actor.0,
                input.cron,
                input.timezone,
                misfire_text(input.misfire_policy),
                serde_json::to_string(&input.concurrency_policy)?,
                input.enabled,
                input.next_fire_at.map(|value| value.to_rfc3339()),
                now,
            ],
        )?;
        let schedule = schedule_by_id_locked(&tx, ScheduleId(tx.last_insert_rowid()))?;
        tx.commit()?;
        Ok(schedule)
    }

    pub fn list_run_schedules_for_principal(
        &self,
        configuration_id: RunConfigurationId,
        actor: PrincipalId,
    ) -> Result<Vec<RunSchedule>> {
        let conn = self.conn()?;
        let configuration = run_configuration_by_id_locked(&conn, configuration_id)?;
        let workspace = workspace_by_id_locked(&conn, configuration.workspace_id)?;
        ensure_room_access(&conn, workspace.room_id, actor, false)?;
        let mut statement =
            conn.prepare("SELECT id FROM run_schedule WHERE configuration_id = ?1 ORDER BY id")?;
        let ids = statement
            .query_map(params![configuration_id.0], |row| row.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter()
            .map(|id| schedule_by_id_locked(&conn, ScheduleId(id)))
            .collect()
    }

    pub fn run_schedule_for_principal(
        &self,
        id: ScheduleId,
        actor: PrincipalId,
        writable: bool,
    ) -> Result<RunSchedule> {
        let conn = self.conn()?;
        let schedule = schedule_by_id_locked(&conn, id)?;
        let configuration = run_configuration_by_id_locked(&conn, schedule.configuration_id)?;
        let workspace = workspace_by_id_locked(&conn, configuration.workspace_id)?;
        if writable {
            ensure_room_owner(&conn, workspace.room_id, actor)?;
        } else {
            ensure_room_access(&conn, workspace.room_id, actor, false)?;
        }
        Ok(schedule)
    }

    pub fn update_run_schedule(
        &self,
        id: ScheduleId,
        actor: PrincipalId,
        expected_revision: u64,
        input: NewRunSchedule,
    ) -> Result<RunSchedule> {
        validate_input(&input)?;
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = schedule_by_id_locked(&tx, id)?;
        let configuration = run_configuration_by_id_locked(&tx, current.configuration_id)?;
        let workspace = workspace_by_id_locked(&tx, configuration.workspace_id)?;
        ensure_room_owner(&tx, workspace.room_id, actor)?;
        ensure_revision(&current, expected_revision)?;
        tx.execute(
            "UPDATE run_schedule SET cron = ?1, timezone = ?2, misfire_policy = ?3,
                 concurrency_json = ?4, enabled = ?5, next_fire_at = ?6,
                 revision = revision + 1, updated_at = ?7 WHERE id = ?8",
            params![
                input.cron,
                input.timezone,
                misfire_text(input.misfire_policy),
                serde_json::to_string(&input.concurrency_policy)?,
                input.enabled,
                input.next_fire_at.map(|value| value.to_rfc3339()),
                now_text(),
                id.0,
            ],
        )?;
        let updated = schedule_by_id_locked(&tx, id)?;
        tx.commit()?;
        Ok(updated)
    }

    pub fn delete_run_schedule(
        &self,
        id: ScheduleId,
        actor: PrincipalId,
        expected_revision: u64,
    ) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = schedule_by_id_locked(&tx, id)?;
        let configuration = run_configuration_by_id_locked(&tx, current.configuration_id)?;
        let workspace = workspace_by_id_locked(&tx, configuration.workspace_id)?;
        ensure_room_owner(&tx, workspace.room_id, actor)?;
        ensure_revision(&current, expected_revision)?;
        tx.execute("DELETE FROM run_schedule WHERE id = ?1", params![id.0])?;
        tx.commit()?;
        Ok(())
    }

    pub fn due_run_schedules(&self, now: DateTime<Utc>, limit: u32) -> Result<Vec<RunSchedule>> {
        let conn = self.conn()?;
        let mut statement = conn.prepare(
            "SELECT id FROM run_schedule
             WHERE enabled = 1 AND next_fire_at IS NOT NULL AND next_fire_at <= ?1
             ORDER BY next_fire_at, id LIMIT ?2",
        )?;
        let ids = statement
            .query_map(params![now.to_rfc3339(), limit.clamp(1, 100)], |row| {
                row.get::<_, i64>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter()
            .map(|id| schedule_by_id_locked(&conn, ScheduleId(id)))
            .collect()
    }

    pub fn advance_and_enqueue_schedule(
        &self,
        id: ScheduleId,
        expected_fire: DateTime<Utc>,
        next_fire: DateTime<Utc>,
        enqueue: bool,
    ) -> Result<Option<ScheduleOccurrence>> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = schedule_by_id_locked(&tx, id)?;
        if !current.enabled || current.next_fire_at != Some(expected_fire) {
            return Ok(None);
        }
        let already_queued: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM schedule_occurrence
             WHERE schedule_id = ?1 AND state = 'queued')",
            params![id.0],
            |row| row.get(0),
        )?;
        let enqueue = enqueue
            && !(current.concurrency_policy == ScheduleConcurrencyPolicy::QueueOne
                && already_queued);
        let occurrence = if enqueue {
            tx.execute(
                "INSERT OR IGNORE INTO schedule_occurrence
                 (schedule_id, scheduled_for, state, created_at)
                 VALUES (?1, ?2, 'queued', ?3)",
                params![id.0, expected_fire.to_rfc3339(), now_text()],
            )?;
            tx.query_row(
                "SELECT id FROM schedule_occurrence WHERE schedule_id = ?1 AND scheduled_for = ?2",
                params![id.0, expected_fire.to_rfc3339()],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .map(|id| occurrence_by_id_locked(&tx, ScheduleOccurrenceId(id)))
            .transpose()?
        } else {
            None
        };
        tx.execute(
            "UPDATE run_schedule SET next_fire_at = ?1, revision = revision + 1,
             updated_at = ?2 WHERE id = ?3",
            params![next_fire.to_rfc3339(), now_text(), id.0],
        )?;
        tx.execute(
            "DELETE FROM schedule_occurrence
             WHERE schedule_id = ?1
               AND state IN ('succeeded', 'failed', 'blocked', 'rejected', 'outcome_unknown')
               AND id NOT IN (
                   SELECT id FROM schedule_occurrence WHERE schedule_id = ?1
                   ORDER BY id DESC LIMIT 1000
               )",
            params![id.0],
        )?;
        tx.commit()?;
        Ok(occurrence)
    }

    pub fn claim_queued_occurrences(
        &self,
        now: DateTime<Utc>,
        lease_owner: &str,
        limit: u32,
    ) -> Result<Vec<(RunSchedule, ScheduleOccurrence)>> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut statement = tx.prepare(
            "SELECT id FROM schedule_occurrence WHERE state = 'queued' ORDER BY id LIMIT ?1",
        )?;
        let ids = statement
            .query_map(params![limit.clamp(1, 100)], |row| row.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        let mut claimed = Vec::new();
        for occurrence_id in ids {
            let occurrence = occurrence_by_id_locked(&tx, ScheduleOccurrenceId(occurrence_id))?;
            let schedule = schedule_by_id_locked(&tx, occurrence.schedule_id)?;
            let active: i64 = tx.query_row(
                "SELECT COUNT(*) FROM schedule_occurrence
                 WHERE schedule_id = ?1 AND id != ?2 AND state IN ('leased', 'running')",
                params![schedule.id.0, occurrence.id.0],
                |row| row.get(0),
            )?;
            let capacity = match schedule.concurrency_policy {
                ScheduleConcurrencyPolicy::Forbid | ScheduleConcurrencyPolicy::QueueOne => 1,
                ScheduleConcurrencyPolicy::Parallel { maximum } => i64::from(maximum),
            };
            if active >= capacity {
                if schedule.concurrency_policy == ScheduleConcurrencyPolicy::Forbid {
                    finish_occurrence_locked(
                        &tx,
                        occurrence.id,
                        ScheduleOccurrenceState::Blocked,
                        Some("concurrency_forbidden"),
                    )?;
                }
                continue;
            }
            tx.execute(
                "UPDATE schedule_occurrence SET state = 'leased', lease_owner = ?1,
                 lease_expires_at = ?2 WHERE id = ?3 AND state = 'queued'",
                params![
                    lease_owner,
                    (now + Duration::minutes(2)).to_rfc3339(),
                    occurrence.id.0,
                ],
            )?;
            claimed.push((schedule, occurrence_by_id_locked(&tx, occurrence.id)?));
        }
        tx.commit()?;
        Ok(claimed)
    }

    pub fn attach_occurrence_run(
        &self,
        occurrence: ScheduleOccurrenceId,
        run: sift_protocol::RunId,
    ) -> Result<()> {
        let changed = self.conn()?.execute(
            "UPDATE schedule_occurrence SET state = 'running', run_id = ?1
             WHERE id = ?2 AND state = 'leased'",
            params![run.0, occurrence.0],
        )?;
        if changed == 1 {
            Ok(())
        } else {
            Err(MetadataError::InvalidRunSchedule)
        }
    }

    pub fn finish_schedule_occurrence(
        &self,
        occurrence: ScheduleOccurrenceId,
        state: ScheduleOccurrenceState,
        error_code: Option<&str>,
    ) -> Result<()> {
        if !matches!(
            state,
            ScheduleOccurrenceState::Blocked
                | ScheduleOccurrenceState::Rejected
                | ScheduleOccurrenceState::Failed
                | ScheduleOccurrenceState::OutcomeUnknown
        ) {
            return Err(MetadataError::InvalidRunSchedule);
        }
        let conn = self.conn()?;
        finish_occurrence_locked(&conn, occurrence, state, error_code)
    }

    pub fn disable_run_schedule_system(&self, id: ScheduleId) -> Result<()> {
        let changed = self.conn()?.execute(
            "UPDATE run_schedule SET enabled = 0, next_fire_at = NULL,
             revision = revision + 1, updated_at = ?1 WHERE id = ?2",
            params![now_text(), id.0],
        )?;
        if changed == 1 {
            Ok(())
        } else {
            Err(MetadataError::RunScheduleNotFound(id))
        }
    }

    pub fn list_schedule_occurrences_for_principal(
        &self,
        schedule_id: ScheduleId,
        actor: PrincipalId,
        limit: u32,
    ) -> Result<Vec<ScheduleOccurrence>> {
        self.run_schedule_for_principal(schedule_id, actor, false)?;
        let conn = self.conn()?;
        let mut statement = conn.prepare(
            "SELECT id FROM schedule_occurrence WHERE schedule_id = ?1
             ORDER BY id DESC LIMIT ?2",
        )?;
        let ids = statement
            .query_map(params![schedule_id.0, limit.clamp(1, 500)], |row| {
                row.get::<_, i64>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter()
            .map(|id| occurrence_by_id_locked(&conn, ScheduleOccurrenceId(id)))
            .collect()
    }

    pub fn schedule_occurrence_for_principal(
        &self,
        id: ScheduleOccurrenceId,
        actor: PrincipalId,
        writable: bool,
    ) -> Result<ScheduleOccurrence> {
        let conn = self.conn()?;
        let occurrence = occurrence_by_id_locked(&conn, id)?;
        let schedule = schedule_by_id_locked(&conn, occurrence.schedule_id)?;
        let configuration = run_configuration_by_id_locked(&conn, schedule.configuration_id)?;
        let workspace = workspace_by_id_locked(&conn, configuration.workspace_id)?;
        if writable {
            ensure_room_owner(&conn, workspace.room_id, actor)?;
        } else {
            ensure_room_access(&conn, workspace.room_id, actor, false)?;
        }
        Ok(occurrence)
    }

    pub fn resume_schedule_occurrence(
        &self,
        id: ScheduleOccurrenceId,
        actor: PrincipalId,
    ) -> Result<ScheduleOccurrence> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let occurrence = occurrence_by_id_locked(&tx, id)?;
        let schedule = schedule_by_id_locked(&tx, occurrence.schedule_id)?;
        let configuration = run_configuration_by_id_locked(&tx, schedule.configuration_id)?;
        let workspace = workspace_by_id_locked(&tx, configuration.workspace_id)?;
        ensure_room_owner(&tx, workspace.room_id, actor)?;
        if occurrence.run_id.is_some()
            || !matches!(
                occurrence.state,
                ScheduleOccurrenceState::Blocked | ScheduleOccurrenceState::Rejected
            )
        {
            return Err(MetadataError::InvalidRunSchedule);
        }
        tx.execute(
            "UPDATE schedule_occurrence SET state = 'queued', error_code = NULL,
             finished_at = NULL WHERE id = ?1",
            params![id.0],
        )?;
        let resumed = occurrence_by_id_locked(&tx, id)?;
        tx.commit()?;
        Ok(resumed)
    }

    pub fn reconcile_schedule_occurrences(&self) -> Result<()> {
        let conn = self.conn()?;
        conn.execute_batch(
            "UPDATE schedule_occurrence
             SET state = CASE
                 WHEN (SELECT state FROM run_execution WHERE id = run_id) = 'succeeded' THEN 'succeeded'
                 WHEN (SELECT state FROM run_execution WHERE id = run_id) IN ('failed', 'cancelled') THEN 'failed'
                 WHEN (SELECT state FROM run_execution WHERE id = run_id) = 'outcome_unknown' THEN 'outcome_unknown'
                 ELSE state END,
                 finished_at = CASE WHEN (SELECT state FROM run_execution WHERE id = run_id)
                     IN ('succeeded', 'failed', 'cancelled', 'outcome_unknown')
                     THEN COALESCE(finished_at, CURRENT_TIMESTAMP) ELSE finished_at END
             WHERE state = 'running';",
        )?;
        Ok(())
    }

    pub fn recover_interrupted_runs(&self, now: DateTime<Utc>) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE run_execution SET state = 'outcome_unknown', finished_at = ?1,
             revision = revision + 1 WHERE state = 'running'",
            params![now.to_rfc3339()],
        )?;
        conn.execute(
            "UPDATE run_execution SET state = 'blocked', finished_at = ?1,
             revision = revision + 1 WHERE state IN ('queued', 'admitted', 'preparing')",
            params![now.to_rfc3339()],
        )?;
        conn.execute(
            "UPDATE schedule_occurrence SET state = CASE
                 WHEN run_id IN (SELECT id FROM run_execution WHERE state = 'outcome_unknown')
                 THEN 'outcome_unknown' ELSE 'blocked' END,
                 error_code = 'server_restarted', finished_at = ?1,
                 lease_owner = NULL, lease_expires_at = NULL
             WHERE state IN ('leased', 'running')",
            params![now.to_rfc3339()],
        )?;
        Ok(())
    }
}

fn validate_input(input: &NewRunSchedule) -> Result<()> {
    if input.cron.is_empty()
        || input.cron.len() > 256
        || input.timezone.is_empty()
        || input.timezone.len() > 128
        || matches!(
            input.concurrency_policy,
            ScheduleConcurrencyPolicy::Parallel { maximum: 0 | 101.. }
        )
        || input.enabled && input.next_fire_at.is_none()
    {
        Err(MetadataError::InvalidRunSchedule)
    } else {
        Ok(())
    }
}

fn ensure_revision(schedule: &RunSchedule, expected: u64) -> Result<()> {
    if schedule.revision == expected {
        Ok(())
    } else {
        Err(MetadataError::RunScheduleRevisionConflict {
            expected,
            current: schedule.revision,
        })
    }
}

fn schedule_by_id_locked(conn: &Connection, id: ScheduleId) -> Result<RunSchedule> {
    conn.query_row(
        "SELECT id, configuration_id, owner_principal_id, cron, timezone, misfire_policy,
         concurrency_json, enabled, next_fire_at, revision FROM run_schedule WHERE id = ?1",
        params![id.0],
        |row| {
            let revision = row.get::<_, i64>(9)?;
            Ok(RunSchedule {
                id: ScheduleId(row.get(0)?),
                configuration_id: RunConfigurationId(row.get(1)?),
                owner_principal_id: row.get(2)?,
                cron: row.get(3)?,
                timezone: row.get(4)?,
                misfire_policy: parse_misfire(row.get::<_, String>(5)?)?,
                concurrency_policy: serde_json::from_str(&row.get::<_, String>(6)?)
                    .map_err(json_sql_error)?,
                enabled: row.get(7)?,
                next_fire_at: row
                    .get::<_, Option<String>>(8)?
                    .map(parse_time_sql)
                    .transpose()?,
                revision: u64::try_from(revision)
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(9, revision))?,
            })
        },
    )
    .optional()?
    .ok_or(MetadataError::RunScheduleNotFound(id))
}

fn occurrence_by_id_locked(
    conn: &Connection,
    id: ScheduleOccurrenceId,
) -> Result<ScheduleOccurrence> {
    conn.query_row(
        "SELECT id, schedule_id, scheduled_for, state, run_id, error_code, created_at, finished_at
         FROM schedule_occurrence WHERE id = ?1",
        params![id.0],
        |row| {
            Ok(ScheduleOccurrence {
                id: ScheduleOccurrenceId(row.get(0)?),
                schedule_id: ScheduleId(row.get(1)?),
                scheduled_for: parse_time_sql(row.get(2)?)?,
                state: parse_occurrence_state(row.get::<_, String>(3)?)?,
                run_id: row.get::<_, Option<i64>>(4)?.map(sift_protocol::RunId),
                error_code: row.get(5)?,
                created_at: parse_time_sql(row.get(6)?)?,
                finished_at: row
                    .get::<_, Option<String>>(7)?
                    .map(parse_time_sql)
                    .transpose()?,
            })
        },
    )
    .optional()?
    .ok_or(MetadataError::InvalidRunSchedule)
}

fn finish_occurrence_locked(
    conn: &Connection,
    id: ScheduleOccurrenceId,
    state: ScheduleOccurrenceState,
    error: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE schedule_occurrence SET state = ?1, error_code = ?2,
         finished_at = ?3, lease_owner = NULL, lease_expires_at = NULL WHERE id = ?4",
        params![occurrence_state_text(state), error, now_text(), id.0],
    )?;
    Ok(())
}

fn misfire_text(value: ScheduleMisfirePolicy) -> &'static str {
    match value {
        ScheduleMisfirePolicy::Skip => "skip",
        ScheduleMisfirePolicy::RunOnce => "run_once",
    }
}

fn parse_misfire(value: String) -> rusqlite::Result<ScheduleMisfirePolicy> {
    match value.as_str() {
        "skip" => Ok(ScheduleMisfirePolicy::Skip),
        "run_once" => Ok(ScheduleMisfirePolicy::RunOnce),
        _ => Err(enum_sql_error("misfire_policy", value)),
    }
}

fn occurrence_state_text(value: ScheduleOccurrenceState) -> &'static str {
    match value {
        ScheduleOccurrenceState::Queued => "queued",
        ScheduleOccurrenceState::Leased => "leased",
        ScheduleOccurrenceState::Running => "running",
        ScheduleOccurrenceState::Succeeded => "succeeded",
        ScheduleOccurrenceState::Failed => "failed",
        ScheduleOccurrenceState::Blocked => "blocked",
        ScheduleOccurrenceState::Rejected => "rejected",
        ScheduleOccurrenceState::OutcomeUnknown => "outcome_unknown",
    }
}

fn parse_occurrence_state(value: String) -> rusqlite::Result<ScheduleOccurrenceState> {
    match value.as_str() {
        "queued" => Ok(ScheduleOccurrenceState::Queued),
        "leased" => Ok(ScheduleOccurrenceState::Leased),
        "running" => Ok(ScheduleOccurrenceState::Running),
        "succeeded" => Ok(ScheduleOccurrenceState::Succeeded),
        "failed" => Ok(ScheduleOccurrenceState::Failed),
        "blocked" => Ok(ScheduleOccurrenceState::Blocked),
        "rejected" => Ok(ScheduleOccurrenceState::Rejected),
        "outcome_unknown" => Ok(ScheduleOccurrenceState::OutcomeUnknown),
        _ => Err(enum_sql_error("occurrence_state", value)),
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
