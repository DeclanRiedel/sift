//! One terminal query audit, including exceptional observer loss.
use super::*;

pub(super) struct QueryOperationAudit {
    sessions: SessionStore,
    operation: Option<Operation>,
    actor: Option<i64>,
    correlation: Option<String>,
}

impl QueryOperationAudit {
    pub(super) fn new(sessions: SessionStore, operation: Operation, actor: Option<i64>) -> Self {
        Self {
            sessions,
            operation: Some(operation),
            actor,
            correlation: crate::correlation::current(),
        }
    }

    /// IDs discovered during owned AI preflight refine only the audit target;
    /// the action, original actor and request correlation stay captured.
    pub(super) fn bind_ai_context(&mut self, chat: uuid::Uuid, run: Option<uuid::Uuid>) {
        if let Some(Operation::Ai {
            chat_id, run_id, ..
        }) = &mut self.operation
        {
            *chat_id = Some(chat);
            *run_id = run;
        }
    }

    pub(super) fn record(
        &mut self,
        status: OperationStatus,
        code: Option<String>,
        rows: Option<i64>,
        message: Option<String>,
    ) {
        let Some(operation) = self.operation.take() else {
            return;
        };
        let record = || {
            self.sessions
                .push_operation_full(operation, status, self.actor, code, rows, message)
        };
        match self.correlation.clone() {
            Some(id) => crate::correlation::sync_scope(id, record),
            None => record(),
        }
    }

    pub(super) fn failure(&mut self, error: &ApiError) {
        let (code, message) = error_details(error);
        self.record(OperationStatus::Failed, code, None, Some(message));
    }
}

impl Drop for QueryOperationAudit {
    fn drop(&mut self) {
        self.record(
            OperationStatus::Failed,
            Some("query_observer_lost".into()),
            None,
            Some("query observation ended before completion; native outcome is uncertain".into()),
        );
    }
}

pub(super) fn error_details(error: &ApiError) -> (Option<String>, String) {
    match error {
        ApiError::Driver(error) => {
            let code = crate::error::audit_code(&error.code);
            (Some(code.clone()), format!("query failed ({code})"))
        }
        _ => (None, "query request failed".into()),
    }
}

pub(super) fn write_outcome(error: &ApiError) -> sift_protocol::ChangeLedgerOutcome {
    if matches!(error, ApiError::Internal(_))
        || matches!(error, ApiError::Driver(error) if matches!(error.code,
        sift_protocol::Code::QueryTimedOut | sift_protocol::Code::QueryCanceled
            | sift_protocol::Code::ConnectionFailed | sift_protocol::Code::ConnectionInvalidated
            | sift_protocol::Code::DriverInternal | sift_protocol::Code::CursorEvicted
            | sift_protocol::Code::CursorNotFound | sift_protocol::Code::TenantResourceExhausted
            | sift_protocol::Code::ResultTooLarge | sift_protocol::Code::UnsupportedResultShape))
    {
        sift_protocol::ChangeLedgerOutcome::OutcomeUnknown
    } else {
        sift_protocol::ChangeLedgerOutcome::Failed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_extensible_error_code_does_not_persist_diagnostic_text() {
        let error = ApiError::Driver(sift_protocol::DriverError::new(
            sift_protocol::Code::Other {
                message: "private-native-diagnostic".into(),
            },
            "private-native-diagnostic",
        ));
        let (code, message) = error_details(&error);
        assert_eq!(code.as_deref(), Some("other"));
        assert!(!message.contains("private-native-diagnostic"));
    }

    #[tokio::test]
    async fn query_audit_drop_retains_original_actor_correlation_and_privacy() {
        let metadata = MetadataStore::open_in_memory(std::sync::Arc::new(
            sift_metadata::MemorySecretStore::new(),
        ))
        .unwrap();
        metadata.bootstrap_local("audit owner").unwrap();
        let sessions = SessionStore::new(crate::DriverRegistry::new());
        sessions.set_audit_store(metadata.clone());
        let audit = crate::correlation::scope("original-query".into(), async {
            QueryOperationAudit::new(
                sessions.clone(),
                Operation::ExecuteQuery {
                    session: sift_protocol::SessionId(7),
                    request: ExecuteRequestHttp {
                        connection: sift_protocol::ConnectionId(1),
                        sql: "SELECT 'private-drop-sql', $1".into(),
                        params: vec![sift_protocol::Value::Text("private-drop-bind".into())],
                        tx: None,
                        room_id: None,
                        connection_profile_id: None,
                        transform: Some(sift_protocol::ResultTransform {
                            groups: vec![sift_protocol::ResultFilterGroup {
                                filters: vec![sift_protocol::ResultFilter {
                                    column: "value".into(),
                                    operator: sift_protocol::ResultFilterOperator::Equals,
                                    value: Some("private-drop-filter".into()),
                                }],
                                ..Default::default()
                            }],
                            ..Default::default()
                        }),
                        source: None,
                    },
                },
                Some(1),
            )
        })
        .await;
        crate::correlation::scope("unrelated-query".into(), async {
            drop(audit);
        })
        .await;
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let rows = metadata.list_operation_audit(10).unwrap();
                if let Some(row) = rows
                    .iter()
                    .find(|row| row.action == "execute" && row.target == "query")
                {
                    assert_eq!(row.actor_principal_id, Some(PrincipalId(1)));
                    assert_eq!(row.correlation_id.as_deref(), Some("original-query"));
                    assert_eq!(row.status, "failed");
                    assert_eq!(row.result_code.as_deref(), Some("query_observer_lost"));
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(sessions.list_operations().len(), 1);
        let encoded = serde_json::to_string(&sessions.list_operations()).unwrap();
        assert!(!encoded.contains("private-drop-sql") && !encoded.contains("private-drop-bind"));
        assert!(!encoded.contains("private-drop-filter"));
    }
}
