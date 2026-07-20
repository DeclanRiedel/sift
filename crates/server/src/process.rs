use sift_protocol::{
    Code, ConnectionId, DatabaseProcess, DriverError, Engine, ExecuteRequestHttp,
    KillProcessResponse, SessionId, Value,
};

use crate::error::{ApiError, ApiResult};
use crate::session::SessionStore;

const PG_LIST: &str = "SELECT pid::bigint, usename, datname, state, query, query_start, concat_ws(':', wait_event_type, wait_event), array_to_string(pg_blocking_pids(pid), ',') FROM pg_stat_activity WHERE pid <> pg_backend_pid() ORDER BY (state = 'active') DESC, query_start NULLS LAST LIMIT 500";
const MSSQL_LIST: &str = "SELECT TOP (500) CONVERT(bigint, r.session_id), s.login_name, DB_NAME(r.database_id), r.status, t.text, r.start_time, r.wait_type, CONVERT(varchar(20), NULLIF(r.blocking_session_id, 0)) FROM sys.dm_exec_requests r JOIN sys.dm_exec_sessions s ON s.session_id = r.session_id CROSS APPLY sys.dm_exec_sql_text(r.sql_handle) t WHERE r.session_id <> @@SPID ORDER BY CASE WHEN r.status = 'running' THEN 0 ELSE 1 END, r.start_time";

pub async fn list(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
) -> ApiResult<Vec<DatabaseProcess>> {
    let engine = store.conn_entry(session, connection)?.driver.engine();
    let sql = match engine {
        Engine::Postgres => PG_LIST,
        Engine::SqlServer => MSSQL_LIST,
    };
    let response = store
        .execute_http_as(
            session,
            ExecuteRequestHttp {
                connection,
                sql: sql.into(),
                params: Vec::new(),
                tx: None,
                room_id: None,
                connection_profile_id: None,
            },
            sift_protocol::OperationKind::ListProcesses,
        )
        .await?;
    response
        .rows
        .iter()
        .map(|row| parse_row(engine, &row.values))
        .collect()
}

pub async fn kill(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    process_id: i64,
) -> ApiResult<KillProcessResponse> {
    if process_id <= 0 {
        return Err(ApiError::BadRequest("process_id must be positive".into()));
    }
    let engine = store.conn_entry(session, connection)?.driver.engine();
    let (sql, params) = match engine {
        Engine::Postgres => (
            "SELECT pg_terminate_backend($1::bigint::int) WHERE $1::bigint::int <> pg_backend_pid()".to_string(),
            vec![Value::Int64(process_id)],
        ),
        Engine::SqlServer => (
            format!(
                "IF {process_id} = @@SPID SELECT CAST(0 AS bit) AS sift_terminated ELSE BEGIN KILL {process_id}; SELECT CAST(1 AS bit) AS sift_terminated END"
            ),
            Vec::new(),
        ),
    };
    let response = store
        .execute_http_as(
            session,
            ExecuteRequestHttp {
                connection,
                sql,
                params,
                tx: None,
                room_id: None,
                connection_profile_id: None,
            },
            sift_protocol::OperationKind::KillProcess,
        )
        .await?;
    let terminated = response
        .rows
        .first()
        .and_then(|row| row.values.first())
        .and_then(value_bool)
        .unwrap_or(false);
    Ok(KillProcessResponse {
        process_id,
        terminated,
    })
}

fn parse_row(engine: Engine, values: &[Value]) -> ApiResult<DatabaseProcess> {
    if values.len() < 8 {
        return Err(ApiError::Driver(DriverError::new(
            Code::UnsupportedResultShape,
            "process catalog query returned fewer than eight columns",
        )));
    }
    let process_id = value_i64(&values[0]).ok_or_else(|| {
        ApiError::Driver(DriverError::new(
            Code::UnsupportedResultShape,
            "process id was not an integer",
        ))
    })?;
    Ok(DatabaseProcess {
        engine,
        process_id,
        user: value_string(&values[1]),
        database: value_string(&values[2]),
        state: value_string(&values[3]),
        statement: value_string(&values[4]),
        started_at: value_timestamp(&values[5]),
        wait: value_string(&values[6]).filter(|value| !value.is_empty()),
        blocked_by: value_string(&values[7])
            .map(|value| {
                value
                    .split(',')
                    .filter_map(|id| id.trim().parse().ok())
                    .collect()
            })
            .unwrap_or_default(),
    })
}

fn value_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Int16(value) => Some(i64::from(*value)),
        Value::Int32(value) => Some(i64::from(*value)),
        Value::Int64(value) => Some(*value),
        _ => None,
    }
}

fn value_bool(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(value) => Some(*value),
        _ => None,
    }
}

fn value_string(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::Text(value) => Some(value.clone()),
        Value::Engine { display_text, .. } => Some(display_text.clone()),
        _ => None,
    }
}

fn value_timestamp(value: &Value) -> Option<chrono::DateTime<chrono::Utc>> {
    match value {
        Value::TimestampTz(value) => Some(*value),
        Value::Timestamp(value) => Some(value.and_utc()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_normalized_process_row() {
        let row = vec![
            Value::Int64(42),
            Value::Text("alice".into()),
            Value::Text("app".into()),
            Value::Text("active".into()),
            Value::Text("select 1".into()),
            Value::Null,
            Value::Text("Lock:relation".into()),
            Value::Text("7, 9".into()),
        ];
        let process = parse_row(Engine::Postgres, &row).unwrap();
        assert_eq!(process.process_id, 42);
        assert_eq!(process.blocked_by, vec![7, 9]);
        assert_eq!(process.wait.as_deref(), Some("Lock:relation"));
    }
}
