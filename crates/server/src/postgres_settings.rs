use sift_protocol::{
    Code, ConnectionId, DriverError, Engine, ExecuteRequestHttp, OperationKind, PostgresSetting,
    PostgresSettingsPage, PostgresSettingsQuery, SessionId, Value,
};

use crate::error::{ApiError, ApiResult};
use crate::session::SessionStore;

const SETTINGS_SQL: &str = "SELECT name::text, setting::text, unit::text, category::text, short_desc::text, context::text, source::text, pending_restart FROM pg_catalog.pg_settings WHERE strpos(lower(name), lower($1::text)) > 0 ORDER BY name LIMIT $2::bigint OFFSET $3::bigint";
const MAX_PAGE_SIZE: u32 = 200;
const MAX_OFFSET: u32 = 10_000;
const MAX_FILTER_LENGTH: usize = 128;

pub async fn list(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    query: PostgresSettingsQuery,
) -> ApiResult<PostgresSettingsPage> {
    if store
        .conn_entry(session, connection)?
        .driver
        .semantic_engine()
        != Some(Engine::Postgres)
    {
        return Err(DriverError::new(
            Code::UnsupportedForEngine,
            "server settings are supported only for PostgreSQL",
        )
        .into());
    }
    if query.filter.len() > MAX_FILTER_LENGTH || query.filter.chars().any(char::is_control) {
        return Err(ApiError::BadRequest("invalid settings filter".into()));
    }
    if query.offset > MAX_OFFSET {
        return Err(ApiError::BadRequest("settings offset exceeds 10000".into()));
    }
    let limit = query.limit.unwrap_or(100);
    if !(1..=MAX_PAGE_SIZE).contains(&limit) {
        return Err(ApiError::BadRequest(
            "settings limit must be 1..=200".into(),
        ));
    }
    let response = store
        .execute_http_as(
            session,
            ExecuteRequestHttp {
                connection,
                sql: SETTINGS_SQL.into(),
                params: vec![
                    Value::Text(query.filter),
                    Value::Int64(i64::from(limit) + 1),
                    Value::Int64(i64::from(query.offset)),
                ],
                tx: None,
                room_id: None,
                connection_profile_id: None,
                transform: None,
                source: None,
            },
            OperationKind::ListPostgresSettings,
        )
        .await?;
    let settings = response
        .rows
        .iter()
        .take(limit as usize)
        .map(|row| parse_setting(&row.values))
        .collect::<ApiResult<Vec<_>>>()?;
    let next_offset = (response.rows.len() > limit as usize && query.offset + limit <= MAX_OFFSET)
        .then(|| query.offset + limit);
    Ok(PostgresSettingsPage {
        settings,
        next_offset,
    })
}

fn parse_setting(row: &[Value]) -> ApiResult<PostgresSetting> {
    if row.len() != 8 {
        return Err(DriverError::new(
            Code::UnsupportedResultShape,
            "PostgreSQL settings query returned an unexpected shape",
        )
        .into());
    }
    let required = |index| {
        text(&row[index]).ok_or_else(|| {
            ApiError::Driver(DriverError::new(
                Code::UnsupportedResultShape,
                "PostgreSQL settings query returned an invalid value",
            ))
        })
    };
    let name = required(0)?;
    let raw_value = text(&row[1]);
    let redacted = sensitive_setting(&name)
        || raw_value.as_deref().is_some_and(|value| {
            let lower = value.to_ascii_lowercase();
            ["password=", "token=", "secret=", "credential="]
                .iter()
                .any(|marker| lower.contains(marker))
        });
    let pending_restart = match row[7] {
        Value::Bool(value) => value,
        _ => {
            return Err(DriverError::new(
                Code::UnsupportedResultShape,
                "PostgreSQL settings query returned an invalid restart flag",
            )
            .into());
        }
    };
    Ok(PostgresSetting {
        value: if redacted { None } else { raw_value },
        name,
        unit: text(&row[2]),
        category: required(3)?,
        description: text(&row[4]),
        context: required(5)?,
        source: required(6)?,
        pending_restart,
        redacted,
    })
}

fn text(value: &Value) -> Option<String> {
    match value {
        Value::Text(value) => Some(value.clone()),
        Value::Null => None,
        _ => None,
    }
}

fn sensitive_setting(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "password",
        "secret",
        "token",
        "credential",
        "passphrase",
        "private_key",
        "conninfo",
    ]
    .iter()
    .any(|part| name.contains(part))
        || matches!(
            name.as_str(),
            "archive_command" | "restore_command" | "recovery_end_command"
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_known_credential_bearing_settings() {
        for name in [
            "primary_conninfo",
            "ssl_passphrase_command",
            "archive_command",
            "my_extension.secret",
        ] {
            assert!(sensitive_setting(name), "{name}");
        }
        assert!(!sensitive_setting("shared_buffers"));
    }

    #[test]
    fn parser_never_returns_redacted_value() {
        let row = [
            Value::Text("primary_conninfo".into()),
            Value::Text("password=hidden".into()),
            Value::Null,
            Value::Text("Replication".into()),
            Value::Null,
            Value::Text("sighup".into()),
            Value::Text("configuration file".into()),
            Value::Bool(false),
        ];
        let setting = parse_setting(&row).unwrap();
        assert_eq!(setting.value, None);
        assert!(setting.redacted);

        let mut custom = row;
        custom[0] = Value::Text("extension_option".into());
        let setting = parse_setting(&custom).unwrap();
        assert_eq!(setting.value, None);
        assert!(setting.redacted);
    }
}
