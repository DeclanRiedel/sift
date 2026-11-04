//! Bounded read-only inspection of SQL Server Agent jobs and whole-job history.

use sift_protocol::{
    AgentJob, AgentJobOutcome, AgentJobsReport, AgentJobsState, Code, ConnectionId, DriverError,
    Engine, ExecuteRequestHttp, OperationKind, SessionId, Value,
};

use crate::error::{ApiError, ApiResult};
use crate::session::SessionStore;

// No job steps, commands or history messages cross the result stream. Native
// Agent roles can read sysjobs_view and execute sp_help_jobhistory, not the
// underlying history table. Keep the stricter Sift owned-job boundary even for
// Reader/Operator roles. All setup and table variables live on an owned session.
const JOBS_SQL: &str = r#"
SET NOCOUNT ON;
SET ROWCOUNT 0;
USE msdb;
IF IS_SRVROLEMEMBER('sysadmin') = 1
BEGIN
    SELECT TOP (101) CONVERT(varchar(36), j.job_id), CONVERT(nvarchar(128), j.name),
        CONVERT(bit, j.enabled), CONVERT(nvarchar(128), SUSER_SNAME(j.owner_sid)),
        CONVERT(int, h.run_status), CONVERT(int, h.run_date), CONVERT(int, h.run_time), CONVERT(int, h.run_duration)
    FROM dbo.sysjobs_view AS j
    OUTER APPLY (SELECT TOP (1) run_status, run_date, run_time, run_duration
        FROM dbo.sysjobhistory AS history WHERE history.job_id = j.job_id AND history.step_id = 0
        ORDER BY history.instance_id DESC) AS h
    ORDER BY j.name, j.job_id;
END
ELSE
BEGIN
    DECLARE @jobs TABLE (ordinal int IDENTITY(1,1), job_id uniqueidentifier, name nvarchar(128), enabled bit,
        owner nvarchar(128), run_status int NULL, run_date int NULL, run_time int NULL, run_duration int NULL);
    INSERT INTO @jobs (job_id, name, enabled, owner)
        SELECT TOP (101) job_id, name, enabled, SUSER_SNAME(owner_sid)
        FROM dbo.sysjobs_view WHERE owner_sid = SUSER_SID() ORDER BY name, job_id;
    DECLARE @history TABLE (job_id uniqueidentifier, job_name nvarchar(128), run_status int, run_date int,
        run_time int, run_duration int, operator_emailed nvarchar(20), operator_netsent nvarchar(20),
        operator_paged nvarchar(20), retries_attempted int, server nvarchar(128));
    DECLARE @ordinal int = 1, @job_id uniqueidentifier;
    DECLARE @server sysname = CONVERT(sysname, SERVERPROPERTY('ServerName'));
    WHILE @ordinal <= (SELECT COUNT(*) FROM @jobs)
    BEGIN
        SELECT @job_id = job_id FROM @jobs WHERE ordinal = @ordinal;
        SET ROWCOUNT 1;
        INSERT INTO @history EXEC dbo.sp_help_jobhistory @job_id = @job_id, @step_id = 0,
            @server = @server, @oldest_first = 0, @mode = 'SUMMARY';
        SET ROWCOUNT 0;
        UPDATE @jobs SET run_status = h.run_status, run_date = h.run_date,
            run_time = h.run_time, run_duration = h.run_duration FROM @history AS h WHERE ordinal = @ordinal;
        DELETE FROM @history;
        SET @ordinal = @ordinal + 1;
    END;
    SELECT CONVERT(varchar(36), job_id), name, enabled, owner,
        run_status, run_date, run_time, run_duration FROM @jobs ORDER BY name, job_id;
END;
SET ROWCOUNT 0;
"#;

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
    store
        .with_sqlserver_inspection_connection(
            session,
            connection,
            OperationKind::ReadAgentJobs,
            |store, session, connection| async move {
                read_on_owned_connection(&store, session, connection).await
            },
        )
        .await
}

pub(crate) async fn read_on_owned_connection(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
) -> ApiResult<AgentJobsReport> {
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
