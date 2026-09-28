//! Bounded PostgreSQL replication and cumulative statistics snapshots.
use sift_protocol::{
    Code, ConnectionId, DriverError, Engine, ExecuteRequestHttp, OperationKind,
    PostgresDatabaseStatistics, PostgresReplicationReport, PostgresReplicationSender,
    PostgresReplicationSlot, PostgresStatisticsQuery, PostgresStatisticsReport,
    PostgresTableStatistics, PostgresWalReceiver, SessionId, Value,
};

use crate::error::{ApiError, ApiResult};
use crate::session::SessionStore;

const REPLICATION_PERMISSION_SQL: &str = "SELECT (current_setting('is_superuser') = 'on' OR pg_catalog.pg_has_role(current_user, 'pg_read_all_stats', 'member'))";
const SENDERS_SQL: &str = "SELECT pid::bigint, application_name::text, state::text, sync_state::text, (EXTRACT(EPOCH FROM write_lag) * 1000)::bigint, (EXTRACT(EPOCH FROM flush_lag) * 1000)::bigint, (EXTRACT(EPOCH FROM replay_lag) * 1000)::bigint FROM pg_catalog.pg_stat_replication ORDER BY pid LIMIT 201";
const RECEIVER_SQL: &str = "SELECT status::text, received_lsn::text, latest_end_lsn::text FROM pg_catalog.pg_stat_wal_receiver LIMIT 2";
const SLOTS_SQL: &str = "SELECT slot_name::text, slot_type::text, database::text, active, restart_lsn::text, confirmed_flush_lsn::text FROM pg_catalog.pg_replication_slots ORDER BY slot_name LIMIT 201";
const DATABASE_SQL: &str = "SELECT datname::text, numbackends::bigint, xact_commit::bigint, xact_rollback::bigint, blks_read::bigint, blks_hit::bigint, tup_returned::bigint, tup_fetched::bigint, tup_inserted::bigint, tup_updated::bigint, tup_deleted::bigint, stats_reset::text FROM pg_catalog.pg_stat_database WHERE datname = current_database() LIMIT 2";
const TABLES_SQL: &str = "SELECT schemaname::text, relname::text, seq_scan::bigint, idx_scan::bigint, n_live_tup::bigint, n_dead_tup::bigint, last_vacuum::text, last_autovacuum::text, last_analyze::text, last_autoanalyze::text FROM pg_catalog.pg_stat_user_tables s WHERE pg_catalog.has_table_privilege(s.relid, 'SELECT') ORDER BY schemaname, relname LIMIT $1::bigint OFFSET $2::bigint";

pub async fn replication(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
) -> ApiResult<PostgresReplicationReport> {
    ensure_postgres(store, session, connection)?;
    let permission = query(
        store,
        session,
        connection,
        OperationKind::ReadPostgresReplication,
        REPLICATION_PERMISSION_SQL,
        Vec::new(),
    )
    .await?;
    let granted = permission.first().and_then(|row| row.values.first());
    if !matches!(granted, Some(Value::Bool(true))) {
        return Err(ApiError::Forbidden(
            "PostgreSQL replication inspection requires pg_read_all_stats or superuser".into(),
        ));
    }
    let senders = query(
        store,
        session,
        connection,
        OperationKind::ReadPostgresReplication,
        SENDERS_SQL,
        Vec::new(),
    )
    .await?
    .iter()
    .map(|row| parse_sender(&row.values))
    .collect::<ApiResult<Vec<_>>>()?;
    let receivers = query(
        store,
        session,
        connection,
        OperationKind::ReadPostgresReplication,
        RECEIVER_SQL,
        Vec::new(),
    )
    .await?
    .iter()
    .map(|row| parse_receiver(&row.values))
    .collect::<ApiResult<Vec<_>>>()?;
    if receivers.len() > 1 {
        return Err(shape_error());
    }
    let slots = query(
        store,
        session,
        connection,
        OperationKind::ReadPostgresReplication,
        SLOTS_SQL,
        Vec::new(),
    )
    .await?
    .iter()
    .map(|row| parse_slot(&row.values))
    .collect::<ApiResult<Vec<_>>>()?;
    Ok(PostgresReplicationReport {
        senders_truncated: senders.len() > 200,
        slots_truncated: slots.len() > 200,
        senders: senders.into_iter().take(200).collect(),
        receiver: receivers.into_iter().next(),
        slots: slots.into_iter().take(200).collect(),
    })
}

pub async fn statistics(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    request: PostgresStatisticsQuery,
) -> ApiResult<PostgresStatisticsReport> {
    ensure_postgres(store, session, connection)?;
    let limit = request.limit.unwrap_or(100);
    if !(1..=200).contains(&limit) || request.offset > 10_000 {
        return Err(ApiError::BadRequest(
            "statistics page must use limit 1..=200 and offset <=10000".into(),
        ));
    }
    let database = query(
        store,
        session,
        connection,
        OperationKind::ReadPostgresStatistics,
        DATABASE_SQL,
        Vec::new(),
    )
    .await?;
    let [database] = database.as_slice() else {
        return Err(shape_error());
    };
    let database = parse_database(&database.values)?;
    let tables = query(
        store,
        session,
        connection,
        OperationKind::ReadPostgresStatistics,
        TABLES_SQL,
        vec![
            Value::Int64(i64::from(limit + 1)),
            Value::Int64(i64::from(request.offset)),
        ],
    )
    .await?
    .iter()
    .map(|row| parse_table(&row.values))
    .collect::<ApiResult<Vec<_>>>()?;
    let next_offset = (tables.len() > limit as usize && request.offset + limit <= 10_000)
        .then_some(request.offset + limit);
    Ok(PostgresStatisticsReport {
        database,
        tables: tables.into_iter().take(limit as usize).collect(),
        next_offset,
    })
}

fn ensure_postgres(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
) -> ApiResult<()> {
    if store
        .conn_entry(session, connection)?
        .driver
        .semantic_engine()
        != Some(Engine::Postgres)
    {
        return Err(DriverError::new(
            Code::UnsupportedForEngine,
            "PostgreSQL diagnostics require a PostgreSQL connection",
        )
        .into());
    }
    Ok(())
}

async fn query(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    operation: OperationKind,
    sql: &str,
    params: Vec<Value>,
) -> ApiResult<Vec<sift_protocol::Row>> {
    let response = store
        .execute_http_as(
            session,
            ExecuteRequestHttp {
                connection,
                sql: sql.into(),
                params,
                tx: None,
                room_id: None,
                connection_profile_id: None,
                transform: None,
                source: None,
            },
            operation,
        )
        .await?;
    Ok(response.rows)
}

fn parse_sender(v: &[Value]) -> ApiResult<PostgresReplicationSender> {
    if v.len() != 7 {
        return Err(shape_error());
    }
    Ok(PostgresReplicationSender {
        pid: required_int(&v[0])?,
        application_name: required_text(&v[1])?,
        state: required_text(&v[2])?,
        sync_state: required_text(&v[3])?,
        write_lag_ms: optional_int(&v[4])?,
        flush_lag_ms: optional_int(&v[5])?,
        replay_lag_ms: optional_int(&v[6])?,
    })
}

fn parse_receiver(v: &[Value]) -> ApiResult<PostgresWalReceiver> {
    if v.len() != 3 {
        return Err(shape_error());
    }
    Ok(PostgresWalReceiver {
        status: required_text(&v[0])?,
        received_lsn: optional_text(&v[1])?,
        latest_end_lsn: optional_text(&v[2])?,
    })
}

fn parse_slot(v: &[Value]) -> ApiResult<PostgresReplicationSlot> {
    if v.len() != 6 {
        return Err(shape_error());
    }
    let Value::Bool(active) = &v[3] else {
        return Err(shape_error());
    };
    Ok(PostgresReplicationSlot {
        name: required_text(&v[0])?,
        slot_type: required_text(&v[1])?,
        database: optional_text(&v[2])?,
        active: *active,
        restart_lsn: optional_text(&v[4])?,
        confirmed_flush_lsn: optional_text(&v[5])?,
    })
}

fn parse_database(v: &[Value]) -> ApiResult<PostgresDatabaseStatistics> {
    if v.len() != 12 {
        return Err(shape_error());
    }
    Ok(PostgresDatabaseStatistics {
        database: required_text(&v[0])?,
        backends: required_int(&v[1])?,
        commits: required_int(&v[2])?,
        rollbacks: required_int(&v[3])?,
        blocks_read: required_int(&v[4])?,
        blocks_hit: required_int(&v[5])?,
        tuples_returned: required_int(&v[6])?,
        tuples_fetched: required_int(&v[7])?,
        tuples_inserted: required_int(&v[8])?,
        tuples_updated: required_int(&v[9])?,
        tuples_deleted: required_int(&v[10])?,
        stats_reset: optional_text(&v[11])?,
    })
}

fn parse_table(v: &[Value]) -> ApiResult<PostgresTableStatistics> {
    if v.len() != 10 {
        return Err(shape_error());
    }
    Ok(PostgresTableStatistics {
        schema: required_text(&v[0])?,
        table: required_text(&v[1])?,
        sequential_scans: required_int(&v[2])?,
        index_scans: optional_int(&v[3])?,
        live_tuples_estimate: required_int(&v[4])?,
        dead_tuples_estimate: required_int(&v[5])?,
        last_vacuum: optional_text(&v[6])?,
        last_autovacuum: optional_text(&v[7])?,
        last_analyze: optional_text(&v[8])?,
        last_autoanalyze: optional_text(&v[9])?,
    })
}

fn required_text(value: &Value) -> ApiResult<String> {
    match value {
        Value::Text(value) => Ok(value.clone()),
        _ => Err(shape_error()),
    }
}
fn optional_text(value: &Value) -> ApiResult<Option<String>> {
    match value {
        Value::Text(value) => Ok(Some(value.clone())),
        Value::Null => Ok(None),
        _ => Err(shape_error()),
    }
}
fn required_int(value: &Value) -> ApiResult<i64> {
    match value {
        Value::Int16(value) => Ok(i64::from(*value)),
        Value::Int32(value) => Ok(i64::from(*value)),
        Value::Int64(value) => Ok(*value),
        _ => Err(shape_error()),
    }
}
fn optional_int(value: &Value) -> ApiResult<Option<i64>> {
    match value {
        Value::Null => Ok(None),
        other => required_int(other).map(Some),
    }
}
fn shape_error() -> ApiError {
    DriverError::new(
        Code::UnsupportedResultShape,
        "unexpected PostgreSQL diagnostics catalog shape",
    )
    .into()
}
