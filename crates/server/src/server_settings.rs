//! Bounded read-only SQL Server instance configuration inspection.

use sift_protocol::{
    Code, ConnectionId, DriverError, Engine, ExecuteRequestHttp, OperationKind, SessionId,
    SqlServerSetting, SqlServerSettingsReport, SqlServerSettingsState, Value,
};

use crate::error::{ApiError, ApiResult};
use crate::session::SessionStore;

// SQL Server's configuration values are numeric sql_variants. Explicit casts
// keep the wire shape stable; the extra row is a truncation sentinel.
const SETTINGS_SQL: &str = "SELECT TOP (201) CONVERT(nvarchar(128), name), CONVERT(bigint, value), CONVERT(bigint, value_in_use), CONVERT(bigint, minimum), CONVERT(bigint, maximum), CONVERT(bit, is_dynamic), CONVERT(bit, is_advanced), CONVERT(nvarchar(255), description) FROM sys.configurations ORDER BY name";

pub async fn read(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
) -> ApiResult<SqlServerSettingsReport> {
    if store.conn_entry(session, connection)?.driver.engine() != Engine::SqlServer {
        return Err(DriverError::new(
            Code::UnsupportedForEngine,
            "server settings inspection requires SQL Server",
        )
        .into());
    }
    let response = store
        .execute_http_as(
            session,
            ExecuteRequestHttp {
                connection,
                sql: SETTINGS_SQL.into(),
                params: Vec::new(),
                tx: None,
                room_id: None,
                connection_profile_id: None,
                transform: None,
                source: None,
            },
            OperationKind::ReadSqlServerSettings,
        )
        .await;
    let response = match response {
        Err(error) if permission_denied(&error) => {
            return Ok(SqlServerSettingsReport {
                state: SqlServerSettingsState::PermissionRequired,
                settings: Vec::new(),
                truncated: false,
            });
        }
        other => other?,
    };
    let mut settings = response
        .rows
        .iter()
        .map(|row| parse_setting(&row.values))
        .collect::<ApiResult<Vec<_>>>()?;
    let truncated = settings.len() > 200;
    settings.truncate(200);
    Ok(SqlServerSettingsReport {
        state: SqlServerSettingsState::Available,
        settings,
        truncated,
    })
}

fn permission_denied(error: &ApiError) -> bool {
    matches!(error, ApiError::Driver(driver) if matches!(driver.native_code.as_deref(), Some("229" | "297")))
}

fn parse_setting(values: &[Value]) -> ApiResult<SqlServerSetting> {
    if values.len() != 8 {
        return Err(shape_error());
    }
    Ok(SqlServerSetting {
        name: text(&values[0]).ok_or_else(shape_error)?,
        configured_value: integer(&values[1]).ok_or_else(shape_error)?,
        effective_value: integer(&values[2]).ok_or_else(shape_error)?,
        minimum: integer(&values[3]).ok_or_else(shape_error)?,
        maximum: integer(&values[4]).ok_or_else(shape_error)?,
        is_dynamic: boolean(&values[5]).ok_or_else(shape_error)?,
        is_advanced: boolean(&values[6]).ok_or_else(shape_error)?,
        description: text(&values[7]).ok_or_else(shape_error)?,
    })
}

fn text(value: &Value) -> Option<String> {
    match value {
        Value::Text(value) => Some(value.clone()),
        Value::Native { display_text, .. } => Some(display_text.clone()),
        _ => None,
    }
}

fn integer(value: &Value) -> Option<i64> {
    match value {
        Value::Int16(value) => Some(i64::from(*value)),
        Value::Int32(value) => Some(i64::from(*value)),
        Value::Int64(value) => Some(*value),
        _ => None,
    }
}

fn boolean(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(value) => Some(*value),
        _ => None,
    }
}

fn shape_error() -> ApiError {
    DriverError::new(
        Code::UnsupportedResultShape,
        "unexpected SQL Server settings catalog result",
    )
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_configured_and_effective_values() {
        let setting = parse_setting(&[
            Value::Text("max degree of parallelism".into()),
            Value::Int64(4),
            Value::Int64(2),
            Value::Int64(0),
            Value::Int64(32767),
            Value::Bool(true),
            Value::Bool(true),
            Value::Text("Maximum degree of parallelism".into()),
        ])
        .unwrap();
        assert_eq!(setting.configured_value, 4);
        assert_eq!(setting.effective_value, 2);
        assert!(setting.is_dynamic);
        assert!(setting.is_advanced);
    }

    #[test]
    fn distinguishes_permission_denial_from_missing_view() {
        let denied = ApiError::Driver(
            DriverError::new(
                Code::Other {
                    message: "denied".into(),
                },
                "denied",
            )
            .with_native_code("297"),
        );
        assert!(permission_denied(&denied));
        let missing = ApiError::Driver(
            DriverError::new(Code::UndefinedObject, "missing").with_native_code("208"),
        );
        assert!(!permission_denied(&missing));
    }
}
