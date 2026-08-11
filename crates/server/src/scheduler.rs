use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Datelike, Timelike, Utc};
use chrono_tz::Tz;
use sift_metadata::{MetadataStore, NewRunExecution, PrincipalId};
use sift_protocol::{RunTrigger, ScheduleMisfirePolicy, ScheduleOccurrenceState};

use crate::error::{ApiError, ApiResult};
use crate::http::AppState;

pub trait SchedulerClock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

pub struct SystemSchedulerClock;

impl SchedulerClock for SystemSchedulerClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

#[derive(Clone)]
pub struct Scheduler {
    state: AppState,
    metadata: MetadataStore,
    clock: Arc<dyn SchedulerClock>,
    lease_owner: String,
}

impl Scheduler {
    pub fn new(state: AppState, metadata: MetadataStore, clock: Arc<dyn SchedulerClock>) -> Self {
        let lease_owner = format!("{}:{}", state.auth.daemon_generation, uuid::Uuid::new_v4());
        Self {
            state,
            metadata,
            clock,
            lease_owner,
        }
    }

    pub fn start(state: AppState, metadata: MetadataStore) {
        if !state.rooms.start_scheduler_once() {
            return;
        }
        let scheduler = Self::new(state, metadata, Arc::new(SystemSchedulerClock));
        tokio::spawn(async move {
            if let Err(error) = scheduler
                .metadata
                .recover_interrupted_runs(scheduler.clock.now())
            {
                tracing::error!(error = %error, "scheduled-run recovery failed");
            }
            loop {
                if let Err(error) = scheduler.tick().await {
                    tracing::warn!(error = %error, "scheduler tick failed");
                }
                tokio::time::sleep(Duration::from_secs(15)).await;
            }
        });
    }

    pub async fn tick(&self) -> ApiResult<()> {
        let now = self.clock.now();
        self.metadata.reconcile_schedule_occurrences()?;
        for schedule in self.metadata.due_run_schedules(now, 100)? {
            let expected = schedule
                .next_fire_at
                .ok_or_else(|| ApiError::Internal("due schedule has no fire time".into()))?;
            let next = next_cron_fire(&schedule.cron, &schedule.timezone, now)?;
            let misfired = expected < now - chrono::Duration::minutes(1);
            let enqueue = !misfired || schedule.misfire_policy == ScheduleMisfirePolicy::RunOnce;
            self.metadata
                .advance_and_enqueue_schedule(schedule.id, expected, next, enqueue)?;
        }
        let occurrences = self
            .metadata
            .claim_queued_occurrences(now, &self.lease_owner, 100)?;
        for (schedule, occurrence) in occurrences {
            let actor = PrincipalId(schedule.owner_principal_id);
            let admission = (|| {
                let principal = self
                    .metadata
                    .principal_by_id(actor)?
                    .filter(|principal| principal.disabled_at.is_none())
                    .ok_or(sift_metadata::MetadataError::InvalidRunSchedule)?;
                let configuration = self.metadata.run_configuration_for_principal(
                    schedule.configuration_id,
                    principal.id,
                    true,
                )?;
                if configuration
                    .variables
                    .iter()
                    .any(|variable| variable.required)
                {
                    return Err(ApiError::BadRequest(
                        "scheduled runs require stored variable bindings".into(),
                    ));
                }
                let (manifest, payload, room_id, tenant_id) = crate::http::capture_run_payload(
                    &self.metadata,
                    &self.state.rooms,
                    actor,
                    &configuration,
                )?;
                let record = self.metadata.create_run_execution(
                    actor,
                    NewRunExecution {
                        configuration_id: configuration.id,
                        trigger: RunTrigger::Schedule,
                        manifest,
                        resolved_scripts_json: serde_json::to_string(&payload).map_err(|_| {
                            ApiError::Internal("run manifest serialization failed".into())
                        })?,
                        previous_run_id: None,
                    },
                )?;
                self.metadata
                    .attach_occurrence_run(occurrence.id, record.run.id)?;
                Ok::<_, ApiError>((configuration, record.run.id, room_id, tenant_id))
            })();
            match admission {
                Ok((configuration, run_id, room_id, tenant_id)) => {
                    let workspace_id = configuration.workspace_id;
                    crate::run_executor::spawn_run(
                        self.state.clone(),
                        self.metadata.clone(),
                        crate::run_executor::RunInvocation {
                            actor,
                            room_id,
                            tenant_id,
                            configuration,
                            run_id,
                            variables: Default::default(),
                            timeout: Duration::from_secs(15 * 60),
                        },
                    );
                    self.state.sessions.push_operation_full(
                        sift_protocol::Operation::Run {
                            action: sift_protocol::RunAction::Start,
                            workspace_id,
                            run_id: Some(run_id),
                        },
                        sift_protocol::OperationStatus::Succeeded,
                        Some(actor.0),
                        None,
                        None,
                        None,
                    );
                }
                Err(error) => {
                    let state = admission_failure_state(&error);
                    self.metadata.finish_schedule_occurrence(
                        occurrence.id,
                        state,
                        Some("admission_failed"),
                    )?;
                    self.metadata.disable_run_schedule_system(schedule.id)?;
                }
            }
        }
        Ok(())
    }
}

fn admission_failure_state(error: &ApiError) -> ScheduleOccurrenceState {
    match error {
        ApiError::Forbidden(_) | ApiError::Unauthorized => ScheduleOccurrenceState::Rejected,
        ApiError::Metadata(
            sift_metadata::MetadataError::RoomNotFound(_)
            | sift_metadata::MetadataError::TenantMemberRequired
            | sift_metadata::MetadataError::TenantMembershipRequired { .. }
            | sift_metadata::MetadataError::ConnectionProfileNotFound(_),
        ) => ScheduleOccurrenceState::Rejected,
        _ => ScheduleOccurrenceState::Blocked,
    }
}

pub fn next_cron_fire(
    cron: &str,
    timezone: &str,
    after: DateTime<Utc>,
) -> ApiResult<DateTime<Utc>> {
    let expression = CronExpression::parse(cron)?;
    let timezone = timezone
        .parse::<Tz>()
        .map_err(|_| ApiError::BadRequest("schedule timezone is invalid".into()))?;
    let start = after
        .with_second(0)
        .and_then(|value| value.with_nanosecond(0))
        .ok_or_else(|| ApiError::BadRequest("schedule timestamp is invalid".into()))?
        + chrono::Duration::minutes(1);
    for offset in 0..(5 * 366 * 24 * 60) {
        let candidate = start + chrono::Duration::minutes(offset);
        if expression.matches(candidate.with_timezone(&timezone)) {
            return Ok(candidate);
        }
    }
    Err(ApiError::BadRequest(
        "schedule has no occurrence within five years".into(),
    ))
}

struct CronExpression {
    minute: CronField,
    hour: CronField,
    day: CronField,
    month: CronField,
    weekday: CronField,
}

impl CronExpression {
    fn parse(value: &str) -> ApiResult<Self> {
        let fields = value.split_ascii_whitespace().collect::<Vec<_>>();
        if fields.len() != 5 {
            return Err(ApiError::BadRequest(
                "schedule cron must have five fields".into(),
            ));
        }
        Ok(Self {
            minute: CronField::parse(fields[0], 0, 59)?,
            hour: CronField::parse(fields[1], 0, 23)?,
            day: CronField::parse(fields[2], 1, 31)?,
            month: CronField::parse(fields[3], 1, 12)?,
            weekday: CronField::parse(fields[4], 0, 7)?,
        })
    }

    fn matches(&self, value: DateTime<Tz>) -> bool {
        let weekday = value.weekday().num_days_from_sunday();
        let day_match = self.day.contains(value.day());
        let weekday_match =
            self.weekday.contains(weekday) || weekday == 0 && self.weekday.contains(7);
        let calendar_match = if self.day.wildcard || self.weekday.wildcard {
            day_match && weekday_match
        } else {
            day_match || weekday_match
        };
        self.minute.contains(value.minute())
            && self.hour.contains(value.hour())
            && self.month.contains(value.month())
            && calendar_match
    }
}

struct CronField {
    minimum: u32,
    allowed: Vec<bool>,
    wildcard: bool,
}

impl CronField {
    fn parse(value: &str, minimum: u32, maximum: u32) -> ApiResult<Self> {
        let mut allowed = vec![false; (maximum - minimum + 1) as usize];
        for item in value.split(',') {
            let (range, step) = item.split_once('/').map_or((item, 1), |(range, step)| {
                (range, step.parse::<u32>().unwrap_or(0))
            });
            if step == 0 {
                return Err(ApiError::BadRequest("schedule cron step is invalid".into()));
            }
            let (start, end) = if range == "*" {
                (minimum, maximum)
            } else if let Some((start, end)) = range.split_once('-') {
                (parse_cron_number(start)?, parse_cron_number(end)?)
            } else {
                let number = parse_cron_number(range)?;
                (number, number)
            };
            if start < minimum || end > maximum || start > end {
                return Err(ApiError::BadRequest(
                    "schedule cron range is invalid".into(),
                ));
            }
            for number in (start..=end).step_by(step as usize) {
                allowed[(number - minimum) as usize] = true;
            }
        }
        Ok(Self {
            minimum,
            allowed,
            wildcard: value == "*",
        })
    }

    fn contains(&self, value: u32) -> bool {
        value
            .checked_sub(self.minimum)
            .and_then(|index| self.allowed.get(index as usize))
            .copied()
            .unwrap_or(false)
    }
}

fn parse_cron_number(value: &str) -> ApiResult<u32> {
    value
        .parse()
        .map_err(|_| ApiError::BadRequest("schedule cron number is invalid".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cron_is_five_field_and_timezone_aware() {
        let after = "2026-08-11T21:59:00Z".parse().unwrap();
        let next = next_cron_fire("0 0 * * *", "Africa/Windhoek", after).unwrap();
        assert_eq!(
            next,
            "2026-08-11T22:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        assert!(next_cron_fire("* * *", "UTC", after).is_err());
        assert!(next_cron_fire("0 0 * * *", "Not/AZone", after).is_err());
    }
}
