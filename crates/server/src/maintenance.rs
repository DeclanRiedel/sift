//! Explicit-target maintenance; uses normal query admission and driver supervision.
use crate::{
    error::{ApiError, ApiResult},
    session::SessionStore,
};
use sift_protocol::{
    ConnectionId, Engine, ExecuteRequestHttp, OperationKind, PostgresMaintenanceAction,
    PostgresMaintenanceReport, PostgresMaintenanceRequest, SessionId,
};

fn sql(request: &PostgresMaintenanceRequest) -> ApiResult<String> {
    fn identifier(value: &str) -> ApiResult<String> {
        // Reject truncation: PostgreSQL's standard NAMEDATALEN is 64 bytes.
        if value.is_empty() || value.len() > 63 || value.chars().any(char::is_control) {
            return Err(ApiError::BadRequest(
                "maintenance identifiers must be 1..63 bytes without control characters".into(),
            ));
        }
        Ok(format!("\"{}\"", value.replace('"', "\"\"")))
    }
    let target = format!(
        "{}.{}",
        identifier(&request.schema)?,
        identifier(&request.name)?
    );
    let command = match request.action {
        PostgresMaintenanceAction::Vacuum { analyze: false } => "VACUUM",
        PostgresMaintenanceAction::Vacuum { analyze: true } => "VACUUM (ANALYZE)",
        PostgresMaintenanceAction::Analyze => "ANALYZE",
        PostgresMaintenanceAction::ReindexTable {
            concurrently: false,
        } => "REINDEX TABLE",
        PostgresMaintenanceAction::ReindexTable { concurrently: true } => {
            "REINDEX TABLE CONCURRENTLY"
        }
        PostgresMaintenanceAction::ReindexIndex {
            concurrently: false,
        } => "REINDEX INDEX",
        PostgresMaintenanceAction::ReindexIndex { concurrently: true } => {
            "REINDEX INDEX CONCURRENTLY"
        }
    };
    Ok(format!("{command} {target}"))
}

pub async fn run(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    request: PostgresMaintenanceRequest,
) -> ApiResult<PostgresMaintenanceReport> {
    let sql = sql(&request)?;
    let entry = store.authorize_connection_operation(
        session,
        connection,
        OperationKind::ExecuteQuery,
        Some(&sql),
        &[],
    )?;
    if entry.driver.engine() != Engine::Postgres {
        return Err(sift_protocol::DriverError::new(
            sift_protocol::Code::UnsupportedForEngine,
            "PostgreSQL maintenance requires PostgreSQL",
        )
        .into());
    }
    store.validate_execute_tx(session, connection, None)?;
    let warnings = if request.apply {
        store
            .execute_http(
                session,
                ExecuteRequestHttp {
                    connection,
                    sql: sql.clone(),
                    params: vec![],
                    tx: None,
                    room_id: None,
                    connection_profile_id: None,
                    transform: None,
                    source: None,
                },
            )
            .await?
            .warnings
    } else {
        vec![]
    };
    Ok(PostgresMaintenanceReport {
        sql,
        applied: request.apply,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maintenance_quotes_targets_and_never_defaults_to_database_scope() {
        let mut request = PostgresMaintenanceRequest {
            action: PostgresMaintenanceAction::Vacuum { analyze: true },
            schema: "odd\"schema".into(),
            name: "items; DROP TABLE users".into(),
            apply: false,
        };
        assert_eq!(
            sql(&request).unwrap(),
            "VACUUM (ANALYZE) \"odd\"\"schema\".\"items; DROP TABLE users\""
        );
        request.action = PostgresMaintenanceAction::ReindexIndex { concurrently: true };
        assert!(sql(&request)
            .unwrap()
            .starts_with("REINDEX INDEX CONCURRENTLY "));
        let read_only = sift_protocol::ConnectionPolicy {
            read_only: true,
            ..Default::default()
        };
        assert!(crate::sql_policy::enforce(
            &read_only,
            Some(Engine::Postgres),
            OperationKind::ExecuteQuery,
            Some(&sql(&request).unwrap()),
            &[]
        )
        .is_err());
        for invalid in [String::new(), "a".repeat(64), "bad\0name".into()] {
            request.name = invalid;
            assert!(sql(&request).is_err());
        }
    }
}
