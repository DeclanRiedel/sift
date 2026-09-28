//! Bounded read-only inspection of SQL Server Agent jobs and whole-job history.

use sift_protocol::{
    AgentJob, AgentJobOutcome, AgentJobsReport, AgentJobsState, Code, ConnectionId, DriverError,
    Engine, ExecuteRequestHttp, OperationKind, SessionId, Value,
};

use crate::error::{ApiError, ApiResult};
use crate::session::SessionStore;

// No job steps or command text are read. Non-sysadmins see only jobs they own;
// this conservative filter avoids exposing other owners' jobs through a direct
// catalog SELECT, even where broad SELECT grants exist. The extra row is a
// truncation sentinel.
const JOBS_SQL: &str = "SELECT TOP (101) CONVERT(varchar(36), j.job_id), CONVERT(nvarchar(128), j.name), CONVERT(bit, j.enabled), CONVERT(nvarchar(128), SUSER_SNAME(j.owner_sid)), CONVERT(int, h.run_status), CONVERT(int, h.run_date), CONVERT(int, h.run_time), CONVERT(int, h.run_duration) FROM msdb.dbo.sysjobs AS j OUTER APPLY (SELECT TOP (1) run_status, run_date, run_time, run_duration FROM msdb.dbo.sysjobhistory AS history WHERE history.job_id = j.job_id AND history.step_id = 0 ORDER BY history.instance_id DESC) AS h WHERE IS_SRVROLEMEMBER('sysadmin') = 1 OR j.owner_sid = SUSER_SID() ORDER BY j.name, j.job_id";

pub async fn read(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
) -> ApiResult<AgentJobsReport> {
    if store.conn_entry(session, connection)?.driver.engine() != Engine::SqlServer {
        return Err(DriverError::new(
            Code::UnsupportedForEngine,
            "Agent jobs inspection requires SQL Server",
        )
        .into());
    }
    let response = store
        .execute_http_as(
            session,
            ExecuteRequestHttp {
                connection,
                sql: JOBS_SQL.into(),
                params: Vec::new(),
                tx: None,
                room_id: None,
                connection_profile_id: None,
                transform: None,
                source: None,
            },
            OperationKind::ReadAgentJobs,
        )
        .await;
    let response = match response {
        Err(error) if permission_denied(&error) => {
            return Ok(AgentJobsReport {
                state: AgentJobsState::PermissionRequired,
                jobs: Vec::new(),
                truncated: false,
            });
        }
        other => other?,
    };
    let mut jobs = response
        .rows
        .iter()
        .map(|row| parse_job(&row.values))
        .collect::<ApiResult<Vec<_>>>()?;
    let truncated = jobs.len() > 100;
    jobs.truncate(100);
    Ok(AgentJobsReport {
        state: AgentJobsState::Available,
        jobs,
        truncated,
    })
}

fn permission_denied(error: &ApiError) -> bool {
    matches!(error, ApiError::Driver(driver) if matches!(driver.native_code.as_deref(), Some("229" | "297" | "916")))
}

fn parse_job(values: &[Value]) -> ApiResult<AgentJob> {
    if values.len() != 8 {
        return Err(shape_error());
    }
    let id = text(&values[0]).ok_or_else(shape_error)?;
    let name = text(&values[1]).ok_or_else(shape_error)?;
    let enabled = match values[2] {
        Value::Bool(value) => value,
        _ => return Err(shape_error()),
    };
    let last_outcome = match integer(&values[4]) {
        Some(0) => Some(AgentJobOutcome::Failed),
        Some(1) => Some(AgentJobOutcome::Succeeded),
        Some(2) => Some(AgentJobOutcome::Retry),
        Some(3) => Some(AgentJobOutcome::Canceled),
        Some(4) => Some(AgentJobOutcome::InProgress),
        Some(_) => Some(AgentJobOutcome::Unknown),
        None if values[4].is_null() => None,
        None => return Err(shape_error()),
    };
    let last_run_local = match integer(&values[5]) {
        Some(0) => None,
        Some(date) => {
            let time = integer(&values[6]).ok_or_else(shape_error)?;
            Some(format_local_time(date, time).ok_or_else(shape_error)?)
        }
        None if values[5].is_null() => None,
        None => return Err(shape_error()),
    };
    let last_duration_seconds = match integer(&values[7]) {
        Some(encoded) => Some(duration_seconds(encoded).ok_or_else(shape_error)?),
        None if values[7].is_null() => None,
        None => return Err(shape_error()),
    };
    Ok(AgentJob {
        id,
        name,
        enabled,
        owner: text(&values[3]),
        last_outcome,
        last_run_local,
        last_duration_seconds,
    })
}

fn format_local_time(date: i64, time: i64) -> Option<String> {
    if !(10_000_000..=99_999_999).contains(&date) || !(0..=235_959).contains(&time) {
        return None;
    }
    let year = i32::try_from(date / 10_000).ok()?;
    let month = u32::try_from((date / 100) % 100).ok()?;
    let day = u32::try_from(date % 100).ok()?;
    chrono::NaiveDate::from_ymd_opt(year, month, day)?;
    let hour = time / 10_000;
    let minute = (time / 100) % 100;
    let second = time % 100;
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    Some(format!(
        "{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}"
    ))
}

fn duration_seconds(encoded: i64) -> Option<u64> {
    if encoded < 0 {
        return None;
    }
    let hours = encoded / 10_000;
    let minutes = (encoded / 100) % 100;
    let seconds = encoded % 100;
    if minutes > 59 || seconds > 59 {
        return None;
    }
    u64::try_from(
        hours
            .checked_mul(3600)?
            .checked_add(minutes * 60 + seconds)?,
    )
    .ok()
}

fn integer(value: &Value) -> Option<i64> {
    match value {
        Value::Int16(value) => Some(i64::from(*value)),
        Value::Int32(value) => Some(i64::from(*value)),
        Value::Int64(value) => Some(*value),
        _ => None,
    }
}

fn text(value: &Value) -> Option<String> {
    match value {
        Value::Text(value) => Some(value.clone()),
        Value::Native { display_text, .. } => Some(display_text.clone()),
        _ => None,
    }
}

fn shape_error() -> ApiError {
    DriverError::new(
        Code::UnsupportedResultShape,
        "unexpected Agent jobs catalog result",
    )
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_agent_history_without_claiming_utc() {
        let values = vec![
            Value::Text("00000000-0000-0000-0000-000000000001".into()),
            Value::Text("nightly".into()),
            Value::Bool(true),
            Value::Text("operator".into()),
            Value::Int32(1),
            Value::Int32(20260928),
            Value::Int32(90102),
            Value::Int32(10123),
        ];
        let job = parse_job(&values).unwrap();
        assert_eq!(job.last_outcome, Some(AgentJobOutcome::Succeeded));
        assert_eq!(job.last_run_local.as_deref(), Some("2026-09-28 09:01:02"));
        assert_eq!(job.last_duration_seconds, Some(3683));
        assert_eq!(format_local_time(20260230, 0), None);
        assert_eq!(duration_seconds(1260), None);
    }

    #[test]
    fn distinguishes_msdb_permission_denial() {
        let denied = ApiError::Driver(
            DriverError::new(
                Code::Other {
                    message: "denied".into(),
                },
                "denied",
            )
            .with_native_code("229"),
        );
        assert!(permission_denied(&denied));
        let unavailable = ApiError::Driver(
            DriverError::new(Code::UndefinedObject, "missing").with_native_code("208"),
        );
        assert!(!permission_denied(&unavailable));
    }
}
