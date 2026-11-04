//! SQL Server inspection runs on an owned session, never the editor.
use super::*;
use sift_protocol::OperationKind;

struct OwnedConnection {
    store: SessionStore,
    session: SessionId,
    connection: ConnectionId,
}
impl Drop for OwnedConnection {
    fn drop(&mut self) {
        let store = self.store.clone();
        let session = self.session;
        let connection = self.connection;
        tokio::spawn(async move {
            let _ = tokio::time::timeout(
                Duration::from_secs(5),
                store.close_connection_unchecked(session, connection),
            )
            .await;
        });
    }
}

impl SessionStore {
    pub(crate) async fn with_sqlserver_inspection_connection<T, F, Fut>(
        &self,
        session: SessionId,
        source: ConnectionId,
        operation: OperationKind,
        read: F,
    ) -> ApiResult<T>
    where
        T: Send + 'static,
        F: FnOnce(SessionStore, SessionId, ConnectionId) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ApiResult<T>> + Send + 'static,
    {
        if !matches!(
            operation,
            OperationKind::ReadAgentJobs | OperationKind::ReadSqlServerSettings
        ) {
            return Err(ApiError::Internal(
                "unsupported SQL Server inspection operation".into(),
            ));
        }
        self.authorize_connection_operation(session, source, operation, None, &[])?;
        let store = self.clone();
        tokio::spawn(async move {
            let (configuration, credentials, provenance, driver) = store
                .with_session(&session, |session| {
                    session.connections.get(&source).map(|entry| {
                        (
                            entry.configuration.clone(),
                            entry.credentials.clone(),
                            entry.provenance.clone(),
                            entry.driver.clone(),
                        )
                    })
                })?
                .ok_or(ApiError::ConnectionNotFound(source))?;
            if !inspection_startup_supported(&configuration) {
                return Err(ApiError::BadRequest("SQL Server inspection cannot replay arbitrary startup batches; use typed session variables or supported literal SET statements".into()));
            }
            let resource = match &provenance {
                ConnectionProvenance::Managed {
                    tenant_id,
                    quota_exempt: false,
                    ..
                } => Some(store.resource_manager().reserve(
                    *tenant_id,
                    sift_protocol::TenantResource::Connections,
                    1,
                )?),
                _ => None,
            };
            let dedicated = store
                .open_connection_with_provenance(
                    session,
                    Some(Engine::SqlServer),
                    configuration,
                    credentials,
                    provenance,
                    resource,
                    driver,
                )
                .await?;
            let _owned = OwnedConnection {
                store: store.clone(),
                session,
                connection: dedicated.id,
            };
            let result = async {
                store.authorize_connection_operation(session, source, operation, None, &[])?;
                let report = read(store.clone(), session, dedicated.id).await?;
                store.authorize_connection_operation(session, source, operation, None, &[])?;
                Ok(report)
            }
            .await;
            let cleanup = tokio::time::timeout(
                Duration::from_secs(5),
                store.close_connection_unchecked(session, dedicated.id),
            )
            .await;
            if !matches!(cleanup, Ok(Ok(()))) {
                tracing::warn!("SQL Server inspection connection cleanup did not complete cleanly");
            }
            result
        })
        .await
        .map_err(|_| ApiError::Internal("SQL Server inspection supervisor failed".into()))?
    }
}

fn inspection_startup_supported(configuration: &serde_json::Value) -> bool {
    let Some(startup) = configuration.pointer("/engine_specific/startup_sql") else {
        return true;
    };
    startup.as_array().is_some_and(|statements| {
        statements
            .iter()
            .all(|statement| statement.as_str().is_some_and(session_only_startup))
    })
}

fn session_only_startup(sql: &str) -> bool {
    let sql = sql.trim();
    if sql.is_empty() {
        return true;
    }
    if sql.len() > 128 {
        return false;
    }
    let sql = sql.strip_suffix(';').unwrap_or(sql).trim();
    let mut words = sql.split_whitespace();
    let (Some(set), Some(option), Some(value), None) =
        (words.next(), words.next(), words.next(), words.next())
    else {
        return false;
    };
    if !set.eq_ignore_ascii_case("SET") {
        return false;
    }
    match option.to_ascii_uppercase().as_str() {
        "ROWCOUNT" => value.parse::<i32>().is_ok_and(|value| value >= 0),
        "LOCK_TIMEOUT" => value.parse::<i32>().is_ok_and(|value| value >= -1),
        "DATEFIRST" => value
            .parse::<i32>()
            .is_ok_and(|value| (1..=7).contains(&value)),
        "DEADLOCK_PRIORITY" => {
            ["LOW", "NORMAL", "HIGH"]
                .iter()
                .any(|known| value.eq_ignore_ascii_case(known))
                || value
                    .parse::<i32>()
                    .is_ok_and(|value| (-10..=10).contains(&value))
        }
        "NOCOUNT"
        | "ANSI_NULLS"
        | "ANSI_PADDING"
        | "ANSI_WARNINGS"
        | "ARITHABORT"
        | "ARITHIGNORE"
        | "CONCAT_NULL_YIELDS_NULL"
        | "QUOTED_IDENTIFIER"
        | "NUMERIC_ROUNDABORT"
        | "XACT_ABORT" => value.eq_ignore_ascii_case("ON") || value.eq_ignore_ascii_case("OFF"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inspection_replay_admits_only_literal_session_setup() {
        for sql in [
            "",
            "SET ROWCOUNT 2;",
            "set lock_timeout -1",
            "SET DEADLOCK_PRIORITY LOW",
            "SET DATEFIRST 7",
            "SET ANSI_NULLS ON",
        ] {
            assert!(session_only_startup(sql));
        }
        for sql in [
            "INSERT INTO t VALUES(1)",
            "EXECUTE AS LOGIN='operator'",
            "EXEC sys.sp_set_session_context @key='tenant',@value='other'",
            "SET NOEXEC ON",
            "SET ROWCOUNT @rows",
            "SET ROWCOUNT 2; DELETE FROM t",
            "SET LOCK_TIMEOUT -2",
            "SET DATEFIRST 8",
            "SET NOCOUNT ON -- comment",
        ] {
            assert!(!session_only_startup(sql));
        }
    }
}
