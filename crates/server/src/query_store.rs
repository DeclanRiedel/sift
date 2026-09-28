//! Bounded, read-only inspection of the current SQL Server database's Query Store.

use sift_protocol::{
    Code, ConnectionId, DriverError, Engine, ExecuteRequestHttp, OperationKind, QueryStorePlan,
    QueryStoreReport, QueryStoreState, SessionId, Value,
};

use crate::error::{ApiError, ApiResult};
use crate::session::SessionStore;

const PERMISSION_SQL: &str = "SELECT CONVERT(nvarchar(128), DB_NAME()), CONVERT(bit, CASE WHEN HAS_PERMS_BY_NAME(DB_NAME(), 'DATABASE', 'VIEW DATABASE STATE') = 1 OR HAS_PERMS_BY_NAME(DB_NAME(), 'DATABASE', 'VIEW DATABASE PERFORMANCE STATE') = 1 THEN 1 ELSE 0 END)";
const STATE_SQL: &str = "SELECT actual_state_desc FROM sys.database_query_store_options";
// The +1 row is a sentinel. The server discards it and reports truncation.
const PLANS_SQL: &str = "SELECT TOP (101) CONVERT(bigint, q.query_id), CONVERT(bigint, p.plan_id), COALESCE(LEFT(CONVERT(nvarchar(max), qt.query_sql_text), 2048), N'[SQL text unavailable]'), COALESCE(s.executions, CONVERT(bigint, 0)), COALESCE(s.average_duration_ms, CONVERT(float, 0)), s.last_execution_time FROM sys.query_store_plan AS p JOIN sys.query_store_query AS q ON q.query_id = p.query_id JOIN sys.query_store_query_text AS qt ON qt.query_text_id = q.query_text_id OUTER APPLY (SELECT CONVERT(bigint, COALESCE(SUM(rs.count_executions), 0)) AS executions, COALESCE(SUM(CONVERT(float, rs.avg_duration) * rs.count_executions) / NULLIF(SUM(rs.count_executions), 0), 0) / 1000.0 AS average_duration_ms, MAX(rs.last_execution_time) AS last_execution_time FROM sys.query_store_runtime_stats AS rs WHERE rs.plan_id = p.plan_id) AS s ORDER BY s.last_execution_time DESC, p.plan_id DESC";

pub async fn read(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
) -> ApiResult<QueryStoreReport> {
    if store.conn_entry(session, connection)?.driver.engine() != Engine::SqlServer {
        return Err(DriverError::new(
            Code::UnsupportedForEngine,
            "Query Store inspection requires SQL Server",
        )
        .into());
    }

    let permission = run(store, session, connection, PERMISSION_SQL).await?;
    let row = permission.rows.first().ok_or_else(shape_error)?;
    let database = row
        .values
        .first()
        .and_then(text_value)
        .ok_or_else(shape_error)?;
    let allowed = matches!(row.values.get(1), Some(Value::Bool(true)));
    if !allowed {
        return Ok(empty_report(database, QueryStoreState::PermissionRequired));
    }

    let state_result = run(store, session, connection, STATE_SQL).await;
    let state = match state_result {
        Err(error) if permission_denied(&error) => {
            return Ok(empty_report(database, QueryStoreState::PermissionRequired));
        }
        Err(error) => return Err(error),
        Ok(result) => result
            .rows
            .first()
            .and_then(|row| row.values.first())
            .and_then(text_value)
            .and_then(|state| parse_state(&state))
            .ok_or_else(shape_error)?,
    };
    if matches!(state, QueryStoreState::Off | QueryStoreState::Error) {
        return Ok(empty_report(database, state));
    }

    let plans_result = run(store, session, connection, PLANS_SQL).await;
    let plans_result = match plans_result {
        Err(error) if permission_denied(&error) => {
            return Ok(empty_report(database, QueryStoreState::PermissionRequired));
        }
        other => other?,
    };
    let mut plans = plans_result
        .rows
        .iter()
        .map(|row| parse_plan(&row.values))
        .collect::<ApiResult<Vec<_>>>()?;
    let truncated = plans.len() > 100;
    plans.truncate(100);
    Ok(QueryStoreReport {
        database,
        state,
        plans,
        truncated,
    })
}

fn empty_report(database: String, state: QueryStoreState) -> QueryStoreReport {
    QueryStoreReport {
        database,
        state,
        plans: Vec::new(),
        truncated: false,
    }
}

async fn run(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    sql: &str,
) -> ApiResult<sift_protocol::ExecuteResponse> {
    store
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
            OperationKind::ReadQueryStore,
        )
        .await
}

fn permission_denied(error: &ApiError) -> bool {
    matches!(error, ApiError::Driver(driver) if matches!(driver.native_code.as_deref(), Some("229" | "297")))
}

fn parse_state(value: &str) -> Option<QueryStoreState> {
    match value {
        "READ_WRITE" => Some(QueryStoreState::ReadWrite),
        "READ_ONLY" => Some(QueryStoreState::ReadOnly),
        "READ_CAPTURE_SECONDARY" => Some(QueryStoreState::ReadCaptureSecondary),
        "OFF" => Some(QueryStoreState::Off),
        "ERROR" => Some(QueryStoreState::Error),
        _ => None,
    }
}

fn parse_plan(values: &[Value]) -> ApiResult<QueryStorePlan> {
    if values.len() != 6 {
        return Err(shape_error());
    }
    let last_execution_at = match &values[5] {
        Value::TimestampTz(value) => Some(*value),
        Value::Timestamp(value) => Some(value.and_utc()),
        Value::Null | Value::TypedNull { .. } => None,
        _ => return Err(shape_error()),
    };
    Ok(QueryStorePlan {
        query_id: integer_value(&values[0]).ok_or_else(shape_error)?,
        plan_id: integer_value(&values[1]).ok_or_else(shape_error)?,
        sql_text: text_value(&values[2]).ok_or_else(shape_error)?,
        executions: integer_value(&values[3]).ok_or_else(shape_error)?,
        average_duration_ms: match &values[4] {
            Value::Float64(value) if value.is_finite() => *value,
            Value::Float32(value) if value.is_finite() => f64::from(*value),
            _ => return Err(shape_error()),
        },
        last_execution_at,
    })
}

fn integer_value(value: &Value) -> Option<i64> {
    match value {
        Value::Int64(value) => Some(*value),
        Value::Int32(value) => Some(i64::from(*value)),
        _ => None,
    }
}

fn text_value(value: &Value) -> Option<String> {
    match value {
        Value::Text(value) => Some(value.clone()),
        _ => None,
    }
}

fn shape_error() -> ApiError {
    DriverError::new(
        Code::UnsupportedResultShape,
        "unexpected Query Store catalog result",
    )
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plan_and_rejects_invalid_metrics() {
        let values = vec![
            Value::Int64(4),
            Value::Int64(9),
            Value::Text("select 1".into()),
            Value::Int64(3),
            Value::Float64(1.25),
            Value::Null,
        ];
        let plan = parse_plan(&values).unwrap();
        assert_eq!((plan.query_id, plan.plan_id, plan.executions), (4, 9, 3));
        assert_eq!(plan.average_duration_ms, 1.25);
        let mut invalid = values;
        invalid[4] = Value::Float64(f64::NAN);
        assert!(parse_plan(&invalid).is_err());
    }

    #[test]
    fn recognizes_query_store_states() {
        assert_eq!(parse_state("READ_WRITE"), Some(QueryStoreState::ReadWrite));
        assert_eq!(parse_state("OFF"), Some(QueryStoreState::Off));
        assert_eq!(parse_state("future_state"), None);
    }
}
