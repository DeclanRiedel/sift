use sift_protocol::{
    Code, ConnectionId, DatabaseHeldLock, DatabaseLockWait, DatabaseProcess, DriverError, Engine,
    ExecuteRequestHttp, KillProcessResponse, SessionId, Value,
};

use crate::error::{ApiError, ApiResult};
use crate::session::SessionStore;

const PG_LIST: &str = r#"SELECT a.pid::bigint, a.usename, a.datname, a.state, a.query, a.query_start,
    concat_ws(':', a.wait_event_type, a.wait_event), array_to_string(pg_blocking_pids(a.pid), ','),
    a.xact_start, a.state_change,
    concat_ws(':', waiting.locktype, COALESCE(waiting.relation::text,
        waiting.transactionid::text, waiting.virtualxid, waiting.objid::text)),
    waiting.mode, waiting.waitstart, held.locks, held.truncated
FROM pg_stat_activity a
LEFT JOIN LATERAL (
    SELECT l.locktype, l.relation, l.transactionid, l.virtualxid, l.objid, l.mode, l.waitstart
    FROM pg_locks l WHERE l.pid = a.pid AND NOT l.granted
    ORDER BY l.waitstart NULLS LAST LIMIT 1
) waiting ON TRUE
LEFT JOIN LATERAL (
    SELECT COALESCE(json_agg(json_build_object('resource', resource, 'mode', mode)
        ORDER BY resource, mode) FILTER (WHERE ordinal <= 16), '[]'::json)::text AS locks,
        count(*) > 16 AS truncated
    FROM (
        SELECT resource, mode, row_number() OVER (ORDER BY resource, mode) AS ordinal
        FROM (
            SELECT concat_ws(':', l.locktype, COALESCE(l.relation::text,
                l.transactionid::text, l.virtualxid, l.objid::text)) AS resource, l.mode
            FROM pg_locks l WHERE l.pid = a.pid AND l.granted
        ) granted
        ORDER BY resource, mode LIMIT 17
    ) sampled
) held ON TRUE
WHERE a.pid <> pg_backend_pid()
ORDER BY (a.state = 'active') DESC, a.query_start NULLS LAST LIMIT 500"#;
const MSSQL_LIST: &str = r#"SELECT TOP (500) CONVERT(bigint, s.session_id), s.login_name,
    DB_NAME(COALESCE(r.database_id, s.database_id)),
    CASE WHEN r.session_id IS NULL AND s.open_transaction_count > 0 THEN 'idle in transaction'
         ELSE COALESCE(r.status, s.status) END,
    t.text, DATEADD(second, DATEDIFF(second, SYSDATETIME(), r.start_time), SYSUTCDATETIME()),
    r.wait_type, CONVERT(varchar(20), NULLIF(r.blocking_session_id, 0)),
    DATEADD(second, DATEDIFF(second, SYSDATETIME(), tx.started_at), SYSUTCDATETIME()),
    DATEADD(second, DATEDIFF(second, SYSDATETIME(), COALESCE(r.start_time, s.last_request_end_time)), SYSUTCDATETIME()),
    CONCAT(waiting.resource_type, ':', waiting.resource_associated_entity_id, ':', waiting.resource_description),
    waiting.request_mode,
    CASE WHEN waiting.request_mode IS NOT NULL AND r.wait_time IS NOT NULL
         THEN DATEADD(millisecond, -r.wait_time, SYSUTCDATETIME()) END,
    held.locks, held.truncated
FROM sys.dm_exec_sessions s
LEFT JOIN sys.dm_exec_requests r ON s.session_id = r.session_id
OUTER APPLY sys.dm_exec_sql_text(r.sql_handle) t
OUTER APPLY (SELECT MIN(a.transaction_begin_time) AS started_at
    FROM sys.dm_tran_session_transactions st
    JOIN sys.dm_tran_active_transactions a ON a.transaction_id = st.transaction_id
    WHERE st.session_id = s.session_id AND st.is_user_transaction = 1) tx
OUTER APPLY (SELECT TOP (1) l.resource_type, l.resource_associated_entity_id,
    l.resource_description, l.request_mode FROM sys.dm_tran_locks l
    WHERE l.request_session_id = s.session_id
      AND l.request_status IN ('WAIT', 'CONVERT', 'LOW_PRIORITY_WAIT', 'LOW_PRIORITY_CONVERT')
    ORDER BY l.resource_type, l.request_mode) waiting
OUTER APPLY (SELECT
    (SELECT TOP (16) CONCAT(l.resource_type, ':', l.resource_associated_entity_id, ':',
        l.resource_description) AS resource, l.request_mode AS mode
     FROM sys.dm_tran_locks l WHERE l.request_session_id=s.session_id AND l.request_status='GRANT'
     ORDER BY l.resource_type, l.resource_associated_entity_id, l.resource_description,
         l.request_mode FOR JSON PATH) AS locks,
    CASE WHEN (SELECT COUNT_BIG(*) FROM sys.dm_tran_locks l
        WHERE l.request_session_id=s.session_id AND l.request_status='GRANT') > 16
        THEN CAST(1 AS bit) ELSE CAST(0 AS bit) END AS truncated) held
WHERE s.session_id <> @@SPID AND s.is_user_process = 1
ORDER BY CASE WHEN r.session_id IS NOT NULL THEN 0 ELSE 1 END,
    COALESCE(r.start_time, tx.started_at)"#;

pub async fn list(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
) -> ApiResult<Vec<DatabaseProcess>> {
    let engine = store.conn_entry(session, connection)?.driver.engine();
    let sql = match engine {
        Engine::Sqlite => {
            return Err(DriverError::new(
                Code::UnsupportedForEngine,
                "SQLite process control is unsupported",
            )
            .into())
        }
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
                transform: None,
                source: None,
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
        Engine::Sqlite => return Err(DriverError::new(Code::UnsupportedForEngine,"SQLite process control is unsupported").into()),
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
                transform: None,
                source: None,
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
        transaction_started_at: values.get(8).and_then(value_timestamp),
        state_changed_at: values.get(9).and_then(value_timestamp),
        wait: value_string(&values[6]).filter(|value| !value.is_empty()),
        blocked_by: value_string(&values[7])
            .map(|value| {
                value
                    .split(',')
                    .filter_map(|id| id.trim().parse().ok())
                    .collect()
            })
            .unwrap_or_default(),
        lock_wait: values
            .get(11)
            .and_then(value_string)
            .map(|mode| DatabaseLockWait {
                resource: values.get(10).and_then(value_string).unwrap_or_default(),
                mode,
                started_at: values.get(12).and_then(value_timestamp),
            }),
        held_locks: values.get(13).and_then(value_string).map_or_else(
            || Ok(Vec::new()),
            |json| {
                serde_json::from_str::<Vec<DatabaseHeldLock>>(&json).map_err(|_| {
                    ApiError::Driver(DriverError::new(
                        Code::UnsupportedResultShape,
                        "held-lock catalog returned invalid JSON",
                    ))
                })
            },
        )?,
        held_locks_truncated: values.get(14).and_then(value_bool).unwrap_or(false),
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
        Value::Null | Value::TypedNull { .. } => None,
        Value::Text(value) => Some(value.clone()),
        Value::Native { display_text, .. } => Some(display_text.clone()),
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
            Value::Null,
            Value::Null,
            Value::Text("relation:orders".into()),
            Value::Text("AccessExclusiveLock".into()),
            Value::Null,
            Value::Text(r#"[{"resource":"relation:9","mode":"AccessShareLock"}]"#.into()),
            Value::Bool(true),
        ];
        let process = parse_row(Engine::Postgres, &row).unwrap();
        assert_eq!(process.process_id, 42);
        assert_eq!(process.blocked_by, vec![7, 9]);
        assert_eq!(process.wait.as_deref(), Some("Lock:relation"));
        let lock = process.lock_wait.unwrap();
        assert_eq!(lock.resource, "relation:orders");
        assert_eq!(lock.mode, "AccessExclusiveLock");
        assert_eq!(process.held_locks.len(), 1);
        assert_eq!(process.held_locks[0].resource, "relation:9");
        assert!(process.held_locks_truncated);
    }

    #[cfg(any(feature = "live-pg", feature = "live-mssql"))]
    async fn live_snapshot(engine: Engine) {
        use sift_driver_api::Driver;
        use sift_protocol::{ConnectionSpec, ExecuteRequest, Page, SslMode};

        let (prefix, host, port, database, user): (&str, &str, u16, &str, &str) = match engine {
            Engine::Postgres => (
                "SIFT_PG",
                "/tmp/sift-demo-pg-socket",
                5433,
                "sifttest",
                "sift",
            ),
            Engine::SqlServer => ("SIFT_MSSQL", "127.0.0.1", 1433, "master", "sa"),
            Engine::Sqlite => unreachable!(),
        };
        let env = |key: &str, fallback: &str| {
            std::env::var(format!("{prefix}_{key}")).unwrap_or_else(|_| fallback.into())
        };
        let spec = ConnectionSpec {
            host: env("HOST", host),
            port: Some(env("PORT", &port.to_string()).parse().unwrap()),
            database: Some(env("DB", database)),
            user: env("USER", user),
            password: std::env::var(format!("{prefix}_PASSWORD")).ok(),
            ssl_mode: Some(SslMode::Disable),
            engine_specific: (engine == Engine::SqlServer).then(|| {
                sift_protocol::EngineConnectionSpec::SqlServer(sift_protocol::MssqlConnectionSpec {
                    trust_server_certificate: Some(true),
                    ..Default::default()
                })
            }),
        };
        let driver: Box<dyn Driver> = match engine {
            Engine::Postgres => Box::new(sift_driver_postgres::PgDriver::new()),
            Engine::SqlServer => Box::new(sift_driver_sqlserver::MssqlDriver::new()),
            Engine::Sqlite => unreachable!(),
        };
        let connection = driver.open(&spec).await.unwrap();
        let sql = if engine == Engine::Postgres {
            PG_LIST
        } else {
            MSSQL_LIST
        };
        let mut stream = driver
            .execute(connection.clone(), ExecuteRequest::new(sql))
            .await
            .unwrap();
        let mut done = false;
        while let Some(page) = stream.rows.recv().await {
            match page {
                Page::Rows { rows } => {
                    for row in rows {
                        parse_row(engine, &row.values).unwrap();
                    }
                }
                Page::Error { error } => panic!("process snapshot failed: {error}"),
                Page::Done { .. } => done = true,
                _ => {}
            }
        }
        assert!(done, "process snapshot stream ended without completion");
        driver.close(connection).await.unwrap();
    }

    #[cfg(feature = "live-pg")]
    #[tokio::test]
    async fn postgres_lock_catalog_snapshot_executes() {
        live_snapshot(Engine::Postgres).await;
    }

    #[cfg(feature = "live-mssql")]
    #[tokio::test]
    async fn sqlserver_lock_catalog_snapshot_executes() {
        live_snapshot(Engine::SqlServer).await;
    }
}
