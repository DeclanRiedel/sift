use sift_protocol::{
    Engine, OperationCapability, OperationCapabilityContext, OperationKind, TransactionState,
};

use crate::authorization::{authorize, AuthorizationScope};
use crate::error::{ApiError, ApiResult};
use crate::session::SessionStore;

pub fn evaluate(
    store: &SessionStore,
    context: &OperationCapabilityContext,
    authorization: Option<&AuthorizationScope>,
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
    let driver = match (context.session, context.connection) {
        (Some(session), Some(connection)) => Some(store.conn_entry(session, connection)?.driver),
        _ => None,
    };
    let engine = driver.as_ref().and_then(|driver| driver.semantic_engine());
    let active = active_transaction(&transactions, context.connection);
    let selected_transaction = match context.transaction {
        Some(transaction) => active.is_some_and(|state| state.transaction.tx_id == transaction),
        None => false,
    };
    let scope = match (context.session, context.connection) {
        (None, None) => CapabilityScope::Global,
        (Some(_), None) => CapabilityScope::Session,
        (Some(_), Some(_)) => CapabilityScope::Connection {
            engine,
            transaction: if selected_transaction {
                TransactionContext::Selected
            } else if active.is_some() {
                TransactionContext::Active
            } else {
                TransactionContext::None
            },
        },
        (None, Some(_)) => unreachable!("connection context was validated above"),
    };

    Ok(OperationKind::ALL
        .into_iter()
        .map(|operation| {
            let mut reason = unavailable_reason(operation, scope);
            if reason.is_none()
                && driver
                    .as_ref()
                    .is_some_and(|driver| !driver.supports_operation(operation))
            {
                reason = Some("operation is not supported by this provider");
            }
            if reason.is_none() {
                if let Some(scope) = authorization {
                    reason = authorize(scope, operation)
                        .err()
                        .map(|denial| denial.public_reason());
                }
            }
            OperationCapability {
                operation,
                available: reason.is_none(),
                reason: reason.map(str::to_string),
                destructive: operation.destructive(),
                provider_id: driver
                    .as_ref()
                    .map(|driver| driver.provider().provider_id.clone()),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CapabilityScope {
    Global,
    Session,
    Connection {
        engine: Option<Engine>,
        transaction: TransactionContext,
    },
}

impl CapabilityScope {
    fn has_session(self) -> bool {
        !matches!(self, Self::Global)
    }

    fn has_connection(self) -> bool {
        matches!(self, Self::Connection { .. })
    }

    fn engine(self) -> Option<Engine> {
        match self {
            Self::Connection { engine, .. } => engine,
            Self::Global | Self::Session => None,
        }
    }

    fn transaction(self) -> TransactionContext {
        match self {
            Self::Connection { transaction, .. } => transaction,
            Self::Global | Self::Session => TransactionContext::None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransactionContext {
    None,
    Active,
    Selected,
}

impl TransactionContext {
    fn active(self) -> bool {
        !matches!(self, Self::None)
    }

    fn selected(self) -> bool {
        matches!(self, Self::Selected)
    }
}

fn unavailable_reason(operation: OperationKind, scope: CapabilityScope) -> Option<&'static str> {
    let has_session = scope.has_session();
    let has_connection = scope.has_connection();
    let engine = scope.engine();
    let transaction = scope.transaction();
    let has_active_transaction = transaction.active();
    let selected_transaction = transaction.selected();
    use OperationKind::*;
    match operation {
        Authenticate => Some("available only before authentication"),
        ManagePrincipal
        | ManageGithubAllowlist
        | ManagePrincipalKey
        | ManageTenantInvitation
        | ManageConnectionPolicy
        | ManageTenantLimits
        | ManageExtension
        | ManageInstanceConfiguration
        | BackupState
        | InspectMetadata
        | ReadMetrics
        | RestoreState => Some("administrator context required"),
        RefreshAuthSession
        | Logout
        | ChangePassword
        | OpenSession
        | ListSessions
        | ListAvailableOperations
        | InvokeExtension
        | ApproveOperation
        | Metadata
        | ListCatalogSnapshots
        | GetCatalogSnapshot
        | DeleteCatalogSnapshot
        | ListPlanCaptures
        | SaveBenchmarkRun
        | ListBenchmarkRuns
        | GetBenchmarkRun
        | DeleteBenchmarkRun
        | GetPlanCapture
        | ComparePlanCaptures
        | DeletePlanCapture
        | ReadWorkspace
        | ManageWorkspace
        | ReadWorkspaceHistory
        | RestoreWorkspace
        | BindWorkspaceProjection
        | ManageWorkspaceProjection
        | ReadVcs
        | WriteVcs
        | ReadDdlSource
        | ManageDdlSource
        | ReadRunConfiguration
        | ManageRunConfiguration
        | ReadRun
        | ManageSchedule
        | ReadSchedule
        | ManageTransferRecipe
        | ReadTransferRecipe
        | ExecuteTransferRecipe => None,
        StartComparison | PageComparison | CancelComparison | PrepareComparisonPatch
            if !has_session =>
        {
            Some("session context required")
        }
        StartComparison | PageComparison | CancelComparison | PrepareComparisonPatch => None,
        CloseSession | OpenConnection | ListTransactions if !has_session => {
            Some("session context required")
        }
        CloseSession | OpenConnection | ListTransactions => None,
        AttachRoom | DetachRoom | ApplyDocumentUpdate | ReadSharedResult => {
            Some("room context required")
        }
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
        ReadQueryStore if !has_connection => Some("connection context required"),
        ReadQueryStore if engine != Some(Engine::SqlServer) => {
            Some("Query Store is only supported by SQL Server")
        }
        ReadAgentJobs if !has_connection => Some("connection context required"),
        ReadAgentJobs if engine != Some(Engine::SqlServer) => {
            Some("Agent jobs are only supported by SQL Server")
        }
        ReadSqlServerSettings if !has_connection => Some("connection context required"),
        ReadSqlServerSettings if engine != Some(Engine::SqlServer) => {
            Some("server settings are only supported by SQL Server")
        }
        CloseConnection
        | PingConnection
        | RefreshSchema
        | ReadCatalogGraph
        | ProjectCatalogDiagram
        | CreateCatalogSnapshot
        | CompareCatalogSchemas
        | PreviewMigration
        | ApplyMigration
        | CancelMigration
        | GetMigrationRun
        | CaptureSemanticPlan
        | GenerateDdl
        | ExecuteQuery
        | ExportQuery
        | Complete
        | OpenSemanticDocument
        | UpdateSemanticDocument
        | CloseSemanticDocument
        | SelectStatement
        | DiagnoseSql
        | FormatSql
        | SqlQuickFix
        | FindSqlUsages
        | PrepareSqlRefactor
        | Listen
        | CancelQuery
        | PreviewEdits
        | ApplyEdits
        | SearchSchema
        | SearchData
        | Explain
        | ProfileQuery
        | CancelProfile
        | BenchmarkQuery
        | CancelBenchmark
        | ReadServerDashboard
        | ListProcesses
        | ListDeadlocks
        | ListPostgresSettings
        | ListPostgresObjects
        | ReadPostgresReplication
        | ReadPostgresStatistics
        | PreviewPostgresObject
        | ApplyPostgresObject
        | ReadQueryStore
        | ReadAgentJobs
        | ReadSqlServerSettings
        | ReadSqlServerSecurity
        | PreviewSqlServerSecurity
        | ApplySqlServerSecurity
        | KillProcess
        | ImportCsv
        | BulkInsert
            if !has_connection =>
        {
            Some("connection context required")
        }
        ListDeadlocks if engine != Some(Engine::SqlServer) => {
            Some("retained deadlock history is only available for SQL Server")
        }
        ProfileQuery | CancelProfile
            if !matches!(engine, Some(Engine::Postgres | Engine::SqlServer)) =>
        {
            Some("Profile is available only for PostgreSQL and SQL Server")
        }
        ListPostgresObjects
        | PreviewPostgresObject
        | ApplyPostgresObject
        | ReadPostgresReplication
        | ReadPostgresStatistics
            if engine != Some(Engine::Postgres) =>
        {
            Some("PostgreSQL objects require a PostgreSQL connection")
        }
        ReadSqlServerSecurity | PreviewSqlServerSecurity | ApplySqlServerSecurity
            if engine != Some(Engine::SqlServer) =>
        {
            Some("SQL Server security requires a SQL Server connection")
        }
        ExecuteRun if !has_connection => Some("connection context required"),
        ExecuteQuery if has_active_transaction && !selected_transaction => {
            Some("select the connection's active transaction")
        }
        CloseConnection
        | PingConnection
        | RefreshSchema
        | ReadCatalogGraph
        | ProjectCatalogDiagram
        | CreateCatalogSnapshot
        | CompareCatalogSchemas
        | PreviewMigration
        | ApplyMigration
        | CancelMigration
        | GetMigrationRun
        | CaptureSemanticPlan
        | GenerateDdl
        | ExecuteQuery
        | ExportQuery
        | Complete
        | OpenSemanticDocument
        | UpdateSemanticDocument
        | CloseSemanticDocument
        | SelectStatement
        | DiagnoseSql
        | FormatSql
        | SqlQuickFix
        | FindSqlUsages
        | PrepareSqlRefactor
        | Listen
        | CancelQuery
        | PreviewEdits
        | ApplyEdits
        | SearchSchema
        | SearchData
        | Explain
        | ProfileQuery
        | CancelProfile
        | BenchmarkQuery
        | CancelBenchmark
        | ReadServerDashboard
        | ListProcesses
        | ListDeadlocks
        | ListPostgresSettings
        | ListPostgresObjects
        | ReadPostgresReplication
        | ReadPostgresStatistics
        | PreviewPostgresObject
        | ApplyPostgresObject
        | ReadQueryStore
        | ReadAgentJobs
        | ReadSqlServerSettings
        | ReadSqlServerSecurity
        | PreviewSqlServerSecurity
        | ApplySqlServerSecurity
        | KillProcess
        | ImportCsv
        | BulkInsert
        | ExecuteRun => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_operation_kinds_are_classified_without_context() {
        for operation in OperationKind::ALL {
            let _ = unavailable_reason(operation, CapabilityScope::Global);
        }
    }
}
