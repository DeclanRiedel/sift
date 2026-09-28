use sift_protocol::{
    Code, ConnectionId, DatabaseDeadlockEvent, DatabaseDeadlockParticipant, DatabaseHeldLock,
    DatabaseLockWait, DatabaseProcess, DriverError, Engine, ExecuteRequestHttp,
    KillProcessResponse, SessionId, Value,
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

// system_health is server-owned. The ring buffer is bounded and may evict old
// events; it is a recent-history source, not a durable archive. Keep XML inside
// the server and return only the fields required for inspection.
const MSSQL_DEADLOCKS: &str = r#"SELECT TOP (50)
    CONVERT(nvarchar(max), event_node.query('.'))
FROM sys.dm_xe_sessions session_row
JOIN sys.dm_xe_session_targets target_row
    ON target_row.event_session_address = session_row.address
CROSS APPLY (SELECT CAST(target_row.target_data AS xml) AS target_xml) target_data
CROSS APPLY target_data.target_xml.nodes(
    '/RingBufferTarget/event[@name="xml_deadlock_report"]') events(event_node)
WHERE session_row.name = N'system_health' AND target_row.target_name = N'ring_buffer'
  AND DATALENGTH(CONVERT(nvarchar(max), event_node.query('.'))) <= 262144
ORDER BY event_node.value('(@timestamp)[1]', 'nvarchar(64)') DESC"#;

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

pub async fn list_deadlocks(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
) -> ApiResult<Vec<DatabaseDeadlockEvent>> {
    if store.conn_entry(session, connection)?.driver.engine() != Engine::SqlServer {
        return Err(DriverError::new(
            Code::UnsupportedForEngine,
            "historical deadlock events require SQL Server system_health",
        )
        .into());
    }
    let response = store
        .execute_http_as(
            session,
            ExecuteRequestHttp {
                connection,
                sql: MSSQL_DEADLOCKS.into(),
                params: Vec::new(),
                tx: None,
                room_id: None,
                connection_profile_id: None,
                transform: None,
                source: None,
            },
            sift_protocol::OperationKind::ListDeadlocks,
        )
        .await?;
    response
        .rows
        .iter()
        .map(|row| {
            let xml = row
                .values
                .first()
                .and_then(value_string)
                .ok_or_else(|| deadlock_shape_error("deadlock event XML was missing"))?;
            parse_deadlock_event(&xml)
        })
        .collect()
}

fn deadlock_shape_error(message: &str) -> ApiError {
    ApiError::Driver(DriverError::new(Code::UnsupportedResultShape, message))
}

fn parse_deadlock_event(xml: &str) -> ApiResult<DatabaseDeadlockEvent> {
    let document = roxmltree::Document::parse(xml)
        .map_err(|_| deadlock_shape_error("deadlock event XML was invalid"))?;
    let event = document.root_element();
    let occurred_at = event
        .attribute("timestamp")
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&chrono::Utc))
        .ok_or_else(|| deadlock_shape_error("deadlock event timestamp was invalid"))?;
    let graph = event
        .descendants()
        .find(|node| node.has_tag_name("deadlock"))
        .ok_or_else(|| deadlock_shape_error("deadlock graph was missing"))?;
    let victim_id = graph
        .descendants()
        .find(|node| node.has_tag_name("victimProcess"))
        .and_then(|node| node.attribute("id"));
    let process_list = graph
        .children()
        .find(|node| node.has_tag_name("process-list"))
        .ok_or_else(|| deadlock_shape_error("deadlock process list was missing"))?;
    let mut participants = Vec::new();
    let mut participants_truncated = false;
    for process in process_list
        .children()
        .filter(|node| node.has_tag_name("process"))
    {
        let Some(process_id) = process.attribute("spid").and_then(|id| id.parse().ok()) else {
            continue;
        };
        if participants.len() == 16 {
            participants_truncated = true;
            break;
        }
        let bounded = |value: Option<&str>| value.map(|value| value.chars().take(256).collect());
        participants.push(DatabaseDeadlockParticipant {
            process_id,
            victim: victim_id.is_some_and(|id| process.attribute("id") == Some(id)),
            wait_resource: bounded(process.attribute("waitresource")),
            lock_mode: bounded(process.attribute("lockMode")),
            wait_ms: process
                .attribute("waittime")
                .and_then(|value| value.parse().ok()),
        });
    }
    if participants.is_empty() {
        return Err(deadlock_shape_error("deadlock had no session IDs"));
    }
    Ok(DatabaseDeadlockEvent {
        occurred_at,
        participants,
        participants_truncated,
    })
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

    #[test]
    fn projects_bounded_deadlock_summary_without_sql_text() {
        let xml = r#"<event name="xml_deadlock_report" timestamp="2026-09-28T10:00:00.123Z"><data name="xml_report"><value><deadlock><victim-list><victimProcess id="one"/></victim-list><process-list><process id="one" spid="42" waitresource="KEY: 7:1" waittime="192" lockMode="X"><inputbuf>SECRET SQL</inputbuf></process><process id="two" spid="43" waitresource="KEY: 7:2" waittime="200" lockMode="S"/></process-list></deadlock></value></data></event>"#;
        let event = parse_deadlock_event(xml).unwrap();
        assert_eq!(event.participants.len(), 2);
        assert_eq!(event.participants[0].process_id, 42);
        assert!(event.participants[0].victim);
        assert_eq!(event.participants[0].wait_ms, Some(192));
        assert!(!event.participants[1].victim);
        assert!(!serde_json::to_string(&event)
            .unwrap()
            .contains("SECRET SQL"));

        let processes = (0..17)
            .map(|id| format!("<process id=\"{id}\" spid=\"{id}\"/>"))
            .collect::<String>();
        let xml = format!(
            "<event timestamp=\"2026-09-28T10:00:00Z\"><deadlock><process-list>{processes}</process-list></deadlock></event>"
        );
        let bounded = parse_deadlock_event(&xml).unwrap();
        assert_eq!(bounded.participants.len(), 16);
        assert!(bounded.participants_truncated);
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
        if engine == Engine::SqlServer {
            let mut stream = driver
                .execute(connection.clone(), ExecuteRequest::new(MSSQL_DEADLOCKS))
                .await
                .unwrap();
            let mut done = false;
            while let Some(page) = stream.rows.recv().await {
                match page {
                    Page::Rows { rows } => {
                        for row in rows {
                            parse_deadlock_event(&value_string(&row.values[0]).unwrap()).unwrap();
                        }
                    }
                    Page::Error { error } => panic!("deadlock history query failed: {error}"),
                    Page::Done { .. } => done = true,
                    _ => {}
                }
            }
            assert!(done, "deadlock history stream ended without completion");
        }
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
