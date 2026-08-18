use serde::{Deserialize, Serialize};

use crate::{ConnectionId, ProviderId, SessionId, TxId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Authenticate,
    RefreshAuthSession,
    Logout,
    ChangePassword,
    ManagePrincipal,
    ManageGithubAllowlist,
    ManagePrincipalKey,
    ManageTenantInvitation,
    ManageConnectionPolicy,
    ManageTenantLimits,
    ManageExtension,
    ManageInstanceConfiguration,
    BackupState,
    RestoreState,
    InvokeExtension,
    ApproveOperation,
    OpenSession,
    ListSessions,
    ListAvailableOperations,
    CloseSession,
    OpenConnection,
    CloseConnection,
    PingConnection,
    RefreshSchema,
    ReadCatalogGraph,
    ProjectCatalogDiagram,
    CreateCatalogSnapshot,
    ListCatalogSnapshots,
    GetCatalogSnapshot,
    DeleteCatalogSnapshot,
    CompareCatalogSchemas,
    PreviewMigration,
    ApplyMigration,
    CancelMigration,
    GetMigrationRun,
    StartComparison,
    PageComparison,
    CancelComparison,
    PrepareComparisonPatch,
    CaptureSemanticPlan,
    ListPlanCaptures,
    GetPlanCapture,
    ComparePlanCaptures,
    DeletePlanCapture,
    GenerateDdl,
    ExecuteQuery,
    ExportQuery,
    Complete,
    OpenSemanticDocument,
    UpdateSemanticDocument,
    CloseSemanticDocument,
    SelectStatement,
    DiagnoseSql,
    FormatSql,
    SqlQuickFix,
    FindSqlUsages,
    PrepareSqlRefactor,
    Listen,
    CancelQuery,
    PreviewEdits,
    ApplyEdits,
    SearchSchema,
    SearchData,
    Explain,
    ListProcesses,
    KillProcess,
    ImportCsv,
    BulkInsert,
    BeginTransaction,
    ListTransactions,
    PreviewTransaction,
    CommitTransaction,
    RollbackTransaction,
    Savepoint,
    RollbackToSavepoint,
    ReleaseSavepoint,
    Metadata,
    AttachRoom,
    DetachRoom,
    ApplyDocumentUpdate,
    ReadSharedResult,
    ReadWorkspace,
    ManageWorkspace,
    ReadWorkspaceHistory,
    RestoreWorkspace,
    BindWorkspaceProjection,
    ManageWorkspaceProjection,
    ReadVcs,
    WriteVcs,
    ReadDdlSource,
    ManageDdlSource,
    ReadRunConfiguration,
    ManageRunConfiguration,
    ExecuteRun,
    ReadRun,
    ManageSchedule,
    ReadSchedule,
    ManageTransferRecipe,
    ReadTransferRecipe,
    ExecuteTransferRecipe,
}

impl OperationKind {
    pub const ALL: [Self; 100] = [
        Self::Authenticate,
        Self::RefreshAuthSession,
        Self::Logout,
        Self::ChangePassword,
        Self::ManagePrincipal,
        Self::ManageGithubAllowlist,
        Self::ManagePrincipalKey,
        Self::ManageTenantInvitation,
        Self::ManageConnectionPolicy,
        Self::ManageTenantLimits,
        Self::ManageExtension,
        Self::ManageInstanceConfiguration,
        Self::BackupState,
        Self::RestoreState,
        Self::InvokeExtension,
        Self::ApproveOperation,
        Self::OpenSession,
        Self::ListSessions,
        Self::ListAvailableOperations,
        Self::CloseSession,
        Self::OpenConnection,
        Self::CloseConnection,
        Self::PingConnection,
        Self::RefreshSchema,
        Self::ReadCatalogGraph,
        Self::ProjectCatalogDiagram,
        Self::CreateCatalogSnapshot,
        Self::ListCatalogSnapshots,
        Self::GetCatalogSnapshot,
        Self::DeleteCatalogSnapshot,
        Self::CompareCatalogSchemas,
        Self::PreviewMigration,
        Self::ApplyMigration,
        Self::CancelMigration,
        Self::GetMigrationRun,
        Self::StartComparison,
        Self::PageComparison,
        Self::CancelComparison,
        Self::PrepareComparisonPatch,
        Self::CaptureSemanticPlan,
        Self::ListPlanCaptures,
        Self::GetPlanCapture,
        Self::ComparePlanCaptures,
        Self::DeletePlanCapture,
        Self::GenerateDdl,
        Self::ExecuteQuery,
        Self::ExportQuery,
        Self::Complete,
        Self::OpenSemanticDocument,
        Self::UpdateSemanticDocument,
        Self::CloseSemanticDocument,
        Self::SelectStatement,
        Self::DiagnoseSql,
        Self::FormatSql,
        Self::SqlQuickFix,
        Self::FindSqlUsages,
        Self::PrepareSqlRefactor,
        Self::Listen,
        Self::CancelQuery,
        Self::PreviewEdits,
        Self::ApplyEdits,
        Self::SearchSchema,
        Self::SearchData,
        Self::Explain,
        Self::ListProcesses,
        Self::KillProcess,
        Self::ImportCsv,
        Self::BulkInsert,
        Self::BeginTransaction,
        Self::ListTransactions,
        Self::PreviewTransaction,
        Self::CommitTransaction,
        Self::RollbackTransaction,
        Self::Savepoint,
        Self::RollbackToSavepoint,
        Self::ReleaseSavepoint,
        Self::Metadata,
        Self::AttachRoom,
        Self::DetachRoom,
        Self::ApplyDocumentUpdate,
        Self::ReadSharedResult,
        Self::ReadWorkspace,
        Self::ManageWorkspace,
        Self::ReadWorkspaceHistory,
        Self::RestoreWorkspace,
        Self::BindWorkspaceProjection,
        Self::ManageWorkspaceProjection,
        Self::ReadVcs,
        Self::WriteVcs,
        Self::ReadDdlSource,
        Self::ManageDdlSource,
        Self::ReadRunConfiguration,
        Self::ManageRunConfiguration,
        Self::ExecuteRun,
        Self::ReadRun,
        Self::ManageSchedule,
        Self::ReadSchedule,
        Self::ManageTransferRecipe,
        Self::ReadTransferRecipe,
        Self::ExecuteTransferRecipe,
    ];

    pub fn destructive(self) -> bool {
        matches!(
            self,
            Self::Logout
                | Self::ChangePassword
                | Self::ManagePrincipal
                | Self::ManageGithubAllowlist
                | Self::ManagePrincipalKey
                | Self::ManageTenantInvitation
                | Self::ManageConnectionPolicy
                | Self::ManageTenantLimits
                | Self::ManageExtension
                | Self::RestoreState
                | Self::ApproveOperation
                | Self::ApplyEdits
                | Self::KillProcess
                | Self::ImportCsv
                | Self::BulkInsert
                | Self::CommitTransaction
                | Self::RollbackTransaction
                | Self::Metadata
                | Self::DeleteCatalogSnapshot
                | Self::ApplyMigration
                | Self::DeletePlanCapture
                | Self::ApplyDocumentUpdate
                | Self::ManageWorkspace
                | Self::RestoreWorkspace
                | Self::BindWorkspaceProjection
                | Self::ManageWorkspaceProjection
                | Self::WriteVcs
                | Self::ManageDdlSource
                | Self::ManageRunConfiguration
                | Self::ExecuteRun
                | Self::ManageSchedule
                | Self::ManageTransferRecipe
                | Self::ExecuteTransferRecipe
        )
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct OperationCapabilityContext {
    #[serde(default)]
    pub tenant_id: Option<i64>,
    #[serde(default)]
    pub room_id: Option<i64>,
    #[serde(default)]
    pub connection_profile_id: Option<i64>,
    #[serde(default)]
    pub session: Option<SessionId>,
    #[serde(default)]
    pub connection: Option<ConnectionId>,
    #[serde(default)]
    pub transaction: Option<TxId>,
    #[serde(default)]
    pub workspace_id: Option<crate::WorkspaceId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct OperationCapability {
    pub operation: OperationKind,
    pub available: bool,
    pub reason: Option<String>,
    pub destructive: bool,
    pub provider_id: Option<ProviderId>,
}
