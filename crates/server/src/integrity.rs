//! Native diagnostic checks, with no repair or implicit extension installation.
use crate::{
    error::{ApiError, ApiResult},
    session::SessionStore,
};
use sift_protocol::{
    ConnectionId, Engine, ExecuteRequestHttp, ExecuteResponse, IntegrityCheckReport,
    IntegrityCheckRequest, IntegrityOutcome, OperationKind, SessionId, Value,
};

fn quoted_identifier(value: &str) -> ApiResult<String> {
    if value.is_empty() || value.len() > 63 || value.chars().any(char::is_control) {
        return Err(ApiError::BadRequest(
            "integrity target identifiers must be 1..63 bytes without control characters".into(),
        ));
    }
    Ok(format!("\"{}\"", value.replace('"', "\"\"")))
}

async fn execute(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    sql: String,
    params: Vec<Value>,
) -> ApiResult<ExecuteResponse> {
    store
        .execute_http(
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
        )
        .await
}

pub async fn run(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    check: IntegrityCheckRequest,
) -> ApiResult<IntegrityCheckReport> {
    let engine = match &check {
        IntegrityCheckRequest::Sqlite { .. } => Engine::Sqlite,
        IntegrityCheckRequest::PostgresHeap { .. } => Engine::Postgres,
        IntegrityCheckRequest::SqlServer { .. } => Engine::SqlServer,
    };
    if store.conn_entry(session, connection)?.driver.engine() != engine {
        return Err(sift_protocol::DriverError::new(
            sift_protocol::Code::UnsupportedForEngine,
            "integrity check does not match connection engine",
        )
        .into());
    }
    store.validate_execute_tx(session, connection, None)?;
    let (sql, params) = match &check {
        IntegrityCheckRequest::Sqlite { quick } => (
            format!(
                "PRAGMA main.{}(1000)",
                if *quick {
                    "quick_check"
                } else {
                    "integrity_check"
                }
            ),
            vec![],
        ),
        IntegrityCheckRequest::SqlServer { physical_only } => (
            format!(
                "DBCC CHECKDB (0) WITH NO_INFOMSGS, ALL_ERRORMSGS{}",
                if *physical_only {
                    ", PHYSICAL_ONLY"
                } else {
                    ""
                }
            ),
            vec![],
        ),
        IntegrityCheckRequest::PostgresHeap { schema, name } => {
            let target = format!(
                "{}.{}",
                quoted_identifier(schema)?,
                quoted_identifier(name)?
            );
            let object = sift_protocol::ObjectPath {
                catalog: None,
                schema: Some(schema.clone()),
                name: name.clone(),
                kind: Some(sift_protocol::ObjectKind::Table),
                routine_args: None,
            };
            store.authorize_connection_operation(
                session,
                connection,
                OperationKind::ExecuteQuery,
                None,
                &[&object],
            )?;
            let extension = execute(store, session, connection, "SELECT n.nspname::text FROM pg_catalog.pg_extension e JOIN pg_catalog.pg_namespace n ON n.oid=e.extnamespace WHERE e.extname='amcheck'".into(), vec![]).await?;
            let Some(Value::Text(namespace)) =
                extension.rows.first().and_then(|row| row.values.first())
            else {
                return Err(ApiError::BadRequest(
                    "PostgreSQL heap checks require an already-installed amcheck extension".into(),
                ));
            };
            (format!("SELECT msg::text FROM {}.verify_heapam($1::regclass, on_error_stop => false, check_toast => false) LIMIT 1001", quoted_identifier(namespace)?), vec![Value::Text(target)])
        }
    };
    let result = execute(store, session, connection, sql, params).await?;
    report(check, result)
}

fn report(
    check: IntegrityCheckRequest,
    result: ExecuteResponse,
) -> ApiResult<IntegrityCheckReport> {
    let mut findings = vec![];
    let mut sqlite_ok = false;
    for row in result.rows {
        let [Value::Text(message)] = row.values.as_slice() else {
            return Err(sift_protocol::DriverError::new(
                sift_protocol::Code::UnsupportedResultShape,
                "unexpected integrity result shape",
            )
            .into());
        };
        if matches!(check, IntegrityCheckRequest::Sqlite { .. }) && message == "ok" {
            sqlite_ok = true;
            continue;
        }
        findings.push(message.clone());
    }
    let incomplete = result.has_more || findings.len() >= 1000 || !result.warnings.is_empty();
    let outcome = if incomplete {
        IntegrityOutcome::Incomplete
    } else if findings.is_empty() {
        // SQLite must positively return its sentinel; an empty stream is not success.
        if matches!(check, IntegrityCheckRequest::Sqlite { .. }) && !sqlite_ok {
            return Err(ApiError::Internal(
                "SQLite integrity check returned no result".into(),
            ));
        }
        IntegrityOutcome::NoIssuesReported
    } else {
        IntegrityOutcome::IssuesReported
    };
    findings.truncate(1000);
    Ok(IntegrityCheckReport {
        check,
        outcome,
        findings,
        warnings: result.warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn result(messages: &[&str]) -> ExecuteResponse {
        ExecuteResponse {
            cursor_id: sift_protocol::CursorId::new(1),
            columns: vec![],
            schema_digest: String::new(),
            rows: messages
                .iter()
                .map(|message| sift_protocol::Row::new(vec![Value::Text((*message).into())]))
                .collect(),
            affected_rows: None,
            warnings: vec![],
            has_more: false,
        }
    }
    #[test]
    fn integrity_reports_require_positive_sqlite_evidence_and_mark_caps() {
        let sqlite = IntegrityCheckRequest::Sqlite { quick: false };
        assert!(report(sqlite.clone(), result(&[])).is_err());
        assert_eq!(
            report(sqlite.clone(), result(&["ok"])).unwrap().outcome,
            IntegrityOutcome::NoIssuesReported
        );
        assert_eq!(
            report(sqlite.clone(), result(&["constraint failed"]))
                .unwrap()
                .outcome,
            IntegrityOutcome::IssuesReported
        );
        assert_eq!(
            report(sqlite, result(&vec!["bad"; 1000])).unwrap().outcome,
            IntegrityOutcome::Incomplete
        );
        let heap = IntegrityCheckRequest::PostgresHeap {
            schema: "public".into(),
            name: "items".into(),
        };
        assert_eq!(
            report(heap, result(&[])).unwrap().outcome,
            IntegrityOutcome::NoIssuesReported
        );
        assert_eq!(quoted_identifier("odd\"table").unwrap(), "\"odd\"\"table\"");
        assert!(quoted_identifier("\0").is_err());
    }
}
