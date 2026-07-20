use sift_protocol::{
    Engine, OperationCapability, OperationCapabilityContext, OperationKind, TransactionState,
};

use crate::error::{ApiError, ApiResult};
use crate::session::SessionStore;

pub fn evaluate(
    store: &SessionStore,
    context: &OperationCapabilityContext,
) -> ApiResult<Vec<OperationCapability>> {
    if context.connection.is_some() && context.session.is_none() {
        return Err(ApiError::BadRequest(
            "connection capability context requires a session".into(),
        ));
    }
    if context.transaction.is_some() && context.connection.is_none() {
        return Err(ApiError::BadRequest(
            "transaction capability context requires a connection".into(),
        ));
    }

    let transactions = match context.session {
        Some(session) => store.list_transactions(session)?,
        None => Vec::new(),
    };
    let engine = match (context.session, context.connection) {
        (Some(session), Some(connection)) => {
            Some(store.conn_entry(session, connection)?.driver.engine())
        }
        _ => None,
    };
    let active = active_transaction(&transactions, context.connection);
    let selected_transaction = match context.transaction {
        Some(transaction) => active.is_some_and(|state| state.transaction.tx_id == transaction),
        None => false,
    };

    Ok(OperationKind::ALL
        .into_iter()
        .map(|operation| {
            let reason = unavailable_reason(
                operation,
                context.session.is_some(),
                context.connection.is_some(),
                engine,
                active.is_some(),
                selected_transaction,
            );
            OperationCapability {
                operation,
                available: reason.is_none(),
                reason: reason.map(str::to_string),
                destructive: operation.destructive(),
                engine,
            }
        })
        .collect())
}

fn active_transaction(
    transactions: &[TransactionState],
    connection: Option<sift_protocol::ConnectionId>,
) -> Option<&TransactionState> {
    transactions
        .iter()
        .find(|state| Some(state.transaction.connection) == connection)
}

fn unavailable_reason(
    operation: OperationKind,
    has_session: bool,
    has_connection: bool,
    engine: Option<Engine>,
    has_active_transaction: bool,
    selected_transaction: bool,
) -> Option<&'static str> {
    use OperationKind::*;
    match operation {
        OpenSession | ListSessions | ListAvailableOperations | Metadata => None,
        CloseSession | OpenConnection | ListTransactions if !has_session => {
            Some("session context required")
        }
        CloseSession | OpenConnection | ListTransactions => None,
        AttachRoom | DetachRoom | ApplyDocumentOperation => Some("room context required"),
        BeginTransaction if !has_connection => Some("connection context required"),
        BeginTransaction if has_active_transaction => {
            Some("connection already has an active transaction")
        }
        BeginTransaction => None,
        PreviewTransaction | CommitTransaction | RollbackTransaction | Savepoint
        | RollbackToSavepoint | ReleaseSavepoint
            if !selected_transaction =>
        {
            Some("selected active transaction required")
        }
        ReleaseSavepoint if engine == Some(Engine::SqlServer) => {
            Some("savepoint release is not supported by SQL Server")
        }
        PreviewTransaction | CommitTransaction | RollbackTransaction | Savepoint
        | RollbackToSavepoint | ReleaseSavepoint => None,
        BulkInsert if !has_connection => Some("connection context required"),
        BulkInsert if engine != Some(Engine::SqlServer) => {
            Some("bulk insert is only supported by SQL Server")
        }
        CloseConnection | RefreshSchema | ExecuteQuery | Complete | CancelQuery | PreviewEdits
        | ApplyEdits | SearchSchema | SearchData | Explain | ListProcesses | KillProcess
        | BulkInsert
            if !has_connection =>
        {
            Some("connection context required")
        }
        ExecuteQuery if has_active_transaction && !selected_transaction => {
            Some("select the connection's active transaction")
        }
        CloseConnection | RefreshSchema | ExecuteQuery | Complete | CancelQuery | PreviewEdits
        | ApplyEdits | SearchSchema | SearchData | Explain | ListProcesses | KillProcess
        | BulkInsert => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_operation_kinds_are_classified_without_context() {
        for operation in OperationKind::ALL {
            let _ = unavailable_reason(operation, false, false, None, false, false);
        }
    }
}
