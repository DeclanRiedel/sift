//! Public operation vocabulary. HTTP and WebSocket routes are transport
//! mappings of these operations; adding ad-hoc verbs outside this enum is a
//! protocol break.

use serde::{Deserialize, Serialize};

use crate::OperationKind;
use crate::{
    completion::CompletionRequest, BeginTransactionRequest, BulkInsertRequest, CancelRequest,
    ConnectionId, EndTransactionRequest, ExecuteRequestHttp, KillProcessRequest,
    OpenConnectionRequest, OpenSessionRequest, SavepointRequest, SchemaScope, SemanticDocumentId,
    SessionId, TransactionPreviewRequest,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuthenticationMethod {
    Password,
    Github,
    ApiToken,
    Keypair,
    LocalBypass,
    SshCapability,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IdentityAdminAction {
    Create,
    Enable,
    Disable,
    Link,
    Unlink,
    Reset,
    Revoke,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PolicyAdminAction {
    Read,
    Update,
    Clear,
    Disconnect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionAdminAction {
    Validate,
    Install,
    Enable,
    Disable,
    Grant,
    Revoke,
    AllowTenant,
    DenyTenant,
    Rollback,
    Uninstall,
    Purge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceAction {
    Read,
    Create,
    Update,
    Delete,
    CreateNode,
    MoveNode,
    DeleteNode,
    BatchMutate,
    CreateCheckpoint,
    ReadHistory,
    RestoreCheckpoint,
    BindProjection,
    ReconcileProjection,
    ResolveConflict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VcsAction {
    Bind,
    Unbind,
    Status,
    Validate,
    Diff,
    Branches,
    History,
    CreateBranch,
    SwitchBranch,
    RenameBranch,
    DeleteBranch,
    SetUpstream,
    Conflicts,
    ResolveConflict,
    ContinueOperation,
    AbortOperation,
    RepairBinding,
    Remotes,
    AddRemote,
    EditRemote,
    RemoveRemote,
    Stage,
    Unstage,
    Commit,
    Amend,
    Uncommit,
    Discard,
    Revert,
    SetCredential,
    TestCredential,
    RemoveCredential,
    Fetch,
    Push,
    HostingRead,
    SetHostingCredential,
    RemoveHostingCredential,
    CreatePullRequest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DdlSourceAction {
    Read,
    Create,
    Update,
    Delete,
    Refresh,
    Map,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunConfigurationAction {
    Read,
    Create,
    Update,
    Delete,
    Validate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunAction {
    Start,
    Read,
    Cancel,
    Rerun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleAction {
    Read,
    Create,
    Update,
    Enable,
    Disable,
    Resume,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TransferRecipeAction {
    Read,
    Create,
    Update,
    Delete,
    Validate,
    Execute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InstanceConfigurationAction {
    Read,
    Update,
    DiagnoseVcs,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Operation {
    /// Transport envelope recorded for every HTTP action. Semantic handlers
    /// may additionally emit a richer operation variant.
    HttpRequest {
        method: String,
        path: String,
        status_code: u16,
    },
    /// Authentication attempt. Deliberately carries the method only: raw
    /// credentials, OAuth codes, and tokens never enter the operation model.
    Authenticate {
        method: AuthenticationMethod,
    },
    RefreshAuthSession,
    Logout {
        all_sessions: bool,
    },
    ChangePassword,
    ManagePrincipal {
        action: IdentityAdminAction,
        principal_id: Option<i64>,
    },
    ManageGithubAllowlist {
        action: IdentityAdminAction,
        principal_id: Option<i64>,
    },
    ManagePrincipalKey {
        action: IdentityAdminAction,
        key_id: Option<i64>,
    },
    ManageTenantInvitation {
        action: IdentityAdminAction,
        tenant_id: i64,
    },
    ManageConnectionPolicy {
        action: PolicyAdminAction,
        tenant_id: i64,
        profile_id: i64,
    },
    ManageTenantLimits {
        action: PolicyAdminAction,
        tenant_id: i64,
    },
    ManageExtension {
        action: ExtensionAdminAction,
        extension_id: crate::ExtensionId,
    },
    ManageInstanceConfiguration {
        action: InstanceConfigurationAction,
    },
    InvokeExtension {
        operation: crate::ExtensionOperation,
    },
    ApproveOperation {
        approval_id: String,
    },
    RateLimitRejected {
        class: crate::RateLimitClass,
        route: String,
        tenant_id: Option<i64>,
    },
    OpenSession {
        request: OpenSessionRequest,
    },
    ListSessions,
    ListAvailableOperations {
        context: crate::OperationCapabilityContext,
    },
    CloseSession {
        session: SessionId,
    },
    OpenConnection {
        session: SessionId,
        request: OpenConnectionRequest,
    },
    CloseConnection {
        session: SessionId,
        connection: ConnectionId,
    },
    PingConnection {
        session: SessionId,
        connection: ConnectionId,
    },
    RefreshSchema {
        session: SessionId,
        connection: ConnectionId,
        scope: SchemaScope,
    },
    ReadCatalogGraph {
        session: SessionId,
        connection: ConnectionId,
        refresh: bool,
        requested_schema_count: u32,
        requested_kind_count: u32,
        include_definitions: bool,
        max_nodes: Option<u32>,
    },
    ProjectCatalogDiagram {
        session: SessionId,
        connection: ConnectionId,
        catalog_revision: crate::CatalogRevision,
        requested_object_count: u32,
        neighborhood_depth: u8,
        include_columns: bool,
        max_nodes: Option<u32>,
    },
    CreateCatalogSnapshot {
        session: SessionId,
        connection: ConnectionId,
        catalog_revision: crate::CatalogRevision,
        accept_partial: bool,
    },
    ListCatalogSnapshots {
        tenant_id: i64,
    },
    GetCatalogSnapshot {
        tenant_id: i64,
        snapshot: crate::CatalogSnapshotId,
    },
    DeleteCatalogSnapshot {
        tenant_id: i64,
        snapshot: crate::CatalogSnapshotId,
        expected_revision: u64,
    },
    CompareCatalogSchemas {
        session: SessionId,
        connection: ConnectionId,
        accepted_rename_count: u32,
        max_changes: Option<u32>,
    },
    PreviewMigration {
        session: SessionId,
        connection: ConnectionId,
        selected_change_count: u32,
        expected_live_revision: crate::CatalogRevision,
    },
    ValidateMigration {
        session: SessionId,
        connection: ConnectionId,
        plan_id: crate::MigrationPlanId,
    },
    ApplyMigration {
        session: SessionId,
        connection: ConnectionId,
        plan_id: crate::MigrationPlanId,
    },
    CancelMigration {
        session: SessionId,
        connection: ConnectionId,
        run_id: crate::MigrationRunId,
    },
    GetMigrationRun {
        session: SessionId,
        connection: ConnectionId,
        run_id: crate::MigrationRunId,
    },
    GetDurableMigrationRun {
        tenant_id: i64,
        run_id: crate::MigrationRunId,
    },
    StartComparison {
        session: SessionId,
        left_source: String,
        right_source: String,
        mapped_column_count: u32,
        key_column_count: u32,
    },
    PageComparison {
        session: SessionId,
        comparison_id: crate::ComparisonId,
        limit: u32,
    },
    CancelComparison {
        session: SessionId,
        comparison_id: crate::ComparisonId,
    },
    PrepareComparisonPatch {
        session: SessionId,
        comparison_id: crate::ComparisonId,
        catalog_revision: crate::CatalogRevision,
        max_statements: Option<u32>,
    },
    CaptureSemanticPlan {
        session: SessionId,
        connection: ConnectionId,
        document: SemanticDocumentId,
        revision: u64,
        catalog_revision: crate::CatalogRevision,
        analyze: bool,
    },
    ListPlanCaptures {
        tenant_id: i64,
        source_bound: bool,
        limit: u32,
    },
    GetPlanCapture {
        tenant_id: i64,
        capture_id: crate::PlanCaptureId,
    },
    ComparePlanCaptures {
        tenant_id: i64,
        left: crate::PlanCaptureId,
        right: crate::PlanCaptureId,
    },
    DeletePlanCapture {
        tenant_id: i64,
        capture_id: crate::PlanCaptureId,
        expected_revision: u64,
    },
    GenerateDdl {
        session: SessionId,
        connection: ConnectionId,
    },
    ExecuteQuery {
        session: SessionId,
        request: ExecuteRequestHttp,
    },
    ExportQuery {
        session: SessionId,
        connection: ConnectionId,
    },
    Complete {
        session: SessionId,
        connection: ConnectionId,
        request: CompletionRequest,
    },
    CompleteSemanticDocument {
        session: SessionId,
        connection: ConnectionId,
        document: SemanticDocumentId,
        revision: u64,
        cursor: u32,
        limit: Option<u32>,
    },
    HoverSemanticDocument {
        session: SessionId,
        connection: ConnectionId,
        document: SemanticDocumentId,
        revision: u64,
        position: u32,
        catalog_bound: bool,
    },
    OpenSemanticDocument {
        session: SessionId,
        connection: ConnectionId,
        source_bytes: u64,
    },
    UpdateSemanticDocument {
        session: SessionId,
        connection: ConnectionId,
        document: SemanticDocumentId,
        base_revision: u64,
        source_bytes: u64,
    },
    CloseSemanticDocument {
        session: SessionId,
        connection: ConnectionId,
        document: SemanticDocumentId,
    },
    SelectStatement {
        session: SessionId,
        connection: ConnectionId,
        document: SemanticDocumentId,
        revision: u64,
    },
    DiagnoseSql {
        session: SessionId,
        connection: ConnectionId,
        document: SemanticDocumentId,
        revision: u64,
    },
    FormatSql {
        session: SessionId,
        connection: ConnectionId,
        document: SemanticDocumentId,
        revision: u64,
        range_requested: bool,
    },
    SqlQuickFix {
        session: SessionId,
        connection: ConnectionId,
        document: SemanticDocumentId,
        revision: u64,
        catalog_revision: crate::CatalogRevision,
    },
    FindSqlUsages {
        session: SessionId,
        connection: ConnectionId,
        document: SemanticDocumentId,
        revision: u64,
        catalog_bound: bool,
        limit: Option<u32>,
    },
    PrepareSqlRefactor {
        session: SessionId,
        connection: ConnectionId,
        document: SemanticDocumentId,
        revision: u64,
        catalog_bound: bool,
        rename: bool,
    },
    Listen {
        session: SessionId,
        connection: ConnectionId,
    },
    CancelQuery {
        session: SessionId,
        request: CancelRequest,
    },
    /// Generate (preview) inline-edit DML without executing it.
    PreviewEdits {
        session: SessionId,
        connection: ConnectionId,
    },
    /// Apply an inline-edit set transactionally.
    ApplyEdits {
        session: SessionId,
        connection: ConnectionId,
    },
    /// Fuzzy schema search (object + column names).
    SearchSchema {
        session: SessionId,
        connection: ConnectionId,
    },
    /// Bounded live data search (row contents).
    SearchData {
        session: SessionId,
        connection: ConnectionId,
    },
    /// Capture a query's execution plan (EXPLAIN).
    Explain {
        session: SessionId,
        connection: ConnectionId,
    },
    ListProcesses {
        session: SessionId,
        connection: ConnectionId,
    },
    KillProcess {
        session: SessionId,
        connection: ConnectionId,
        request: KillProcessRequest,
    },
    ImportCsv {
        session: SessionId,
        connection: ConnectionId,
        table: String,
        create_table: bool,
        conflict_policy: crate::CsvConflictPolicy,
    },
    BulkInsert {
        session: SessionId,
        connection: ConnectionId,
        request: BulkInsertRequest,
    },
    BeginTransaction {
        session: SessionId,
        request: BeginTransactionRequest,
    },
    ListTransactions {
        session: SessionId,
    },
    PreviewTransaction {
        session: SessionId,
        request: TransactionPreviewRequest,
    },
    CommitTransaction {
        session: SessionId,
        request: EndTransactionRequest,
    },
    RollbackTransaction {
        session: SessionId,
        request: EndTransactionRequest,
    },
    Savepoint {
        session: SessionId,
        request: SavepointRequest,
    },
    RollbackToSavepoint {
        session: SessionId,
        request: SavepointRequest,
    },
    ReleaseSavepoint {
        session: SessionId,
        request: SavepointRequest,
    },
    /// Catch-all for CRUD-shaped metadata mutations (rooms, documents,
    /// connection profiles, tokens). `action`/`target` are intentionally
    /// free-form strings — the audit sink treats them as opaque tags,
    /// not a bounded vocabulary. Consumers that need to switch on them
    /// should either narrow to the specific enum variants above or
    /// treat unrecognized (action, target) tuples as `Other`.
    Metadata {
        action: String,
        target: String,
        id: Option<i64>,
    },
    AttachRoom {
        room_id: i64,
        attachment_id: i64,
        client_id: String,
    },
    DetachRoom {
        room_id: i64,
        attachment_id: i64,
    },
    /// Durably applied a collaborative document update. Carries only *where* and
    /// *which* — never the CRDT bytes, replica id, or resulting text.
    ApplyDocumentUpdate {
        room_id: i64,
        document_id: i64,
        update_id: String,
        server_seq: i64,
    },
    ReadSharedResult {
        room_id: i64,
        result_id: crate::RoomResultId,
    },
    Workspace {
        action: WorkspaceAction,
        workspace_id: Option<crate::WorkspaceId>,
        node_id: Option<crate::WorkspaceNodeId>,
    },
    Vcs {
        action: VcsAction,
        workspace_id: crate::WorkspaceId,
        binding_id: crate::RepositoryBindingId,
    },
    DdlSource {
        action: DdlSourceAction,
        workspace_id: crate::WorkspaceId,
        source_id: Option<crate::DdlSourceId>,
    },
    RunConfiguration {
        action: RunConfigurationAction,
        workspace_id: crate::WorkspaceId,
        configuration_id: Option<crate::RunConfigurationId>,
    },
    Run {
        action: RunAction,
        workspace_id: crate::WorkspaceId,
        run_id: Option<crate::RunId>,
    },
    Schedule {
        action: ScheduleAction,
        workspace_id: crate::WorkspaceId,
        schedule_id: Option<crate::ScheduleId>,
    },
    TransferRecipe {
        action: TransferRecipeAction,
        workspace_id: crate::WorkspaceId,
        recipe_id: Option<crate::TransferRecipeId>,
    },
    Vault {
        action: crate::VaultAction,
        vault_id: Option<i64>,
        item_id: Option<i64>,
    },
    BackupState,
    RestoreState {
        applied: bool,
    },
}

/// Sanitized projection of an [`Operation`] for the durable audit log. Carries
/// only *what* and *where* — never request bodies, SQL text, or bind values —
/// so persisting it cannot leak query parameters or secrets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationSummary {
    pub action: String,
    pub target: String,
    pub target_id: Option<i64>,
}

impl Operation {
    pub fn kind(&self) -> OperationKind {
        match self {
            Self::HttpRequest { .. } => OperationKind::Metadata,
            Self::Authenticate { .. } => OperationKind::Authenticate,
            Self::RefreshAuthSession => OperationKind::RefreshAuthSession,
            Self::Logout { .. } => OperationKind::Logout,
            Self::ChangePassword => OperationKind::ChangePassword,
            Self::ManagePrincipal { .. } => OperationKind::ManagePrincipal,
            Self::ManageGithubAllowlist { .. } => OperationKind::ManageGithubAllowlist,
            Self::ManagePrincipalKey { .. } => OperationKind::ManagePrincipalKey,
            Self::ManageTenantInvitation { .. } => OperationKind::ManageTenantInvitation,
            Self::ManageConnectionPolicy { .. } => OperationKind::ManageConnectionPolicy,
            Self::ManageTenantLimits { .. } => OperationKind::ManageTenantLimits,
            Self::ManageExtension { .. } => OperationKind::ManageExtension,
            Self::ManageInstanceConfiguration { .. } => OperationKind::ManageInstanceConfiguration,
            Self::InvokeExtension { .. } => OperationKind::InvokeExtension,
            Self::ApproveOperation { .. } => OperationKind::ApproveOperation,
            Self::RateLimitRejected { .. } => OperationKind::Metadata,
            Self::OpenSession { .. } => OperationKind::OpenSession,
            Self::ListSessions => OperationKind::ListSessions,
            Self::ListAvailableOperations { .. } => OperationKind::ListAvailableOperations,
            Self::CloseSession { .. } => OperationKind::CloseSession,
            Self::OpenConnection { .. } => OperationKind::OpenConnection,
            Self::CloseConnection { .. } => OperationKind::CloseConnection,
            Self::PingConnection { .. } => OperationKind::PingConnection,
            Self::RefreshSchema { .. } => OperationKind::RefreshSchema,
            Self::ReadCatalogGraph { .. } => OperationKind::ReadCatalogGraph,
            Self::ProjectCatalogDiagram { .. } => OperationKind::ProjectCatalogDiagram,
            Self::CreateCatalogSnapshot { .. } => OperationKind::CreateCatalogSnapshot,
            Self::ListCatalogSnapshots { .. } => OperationKind::ListCatalogSnapshots,
            Self::GetCatalogSnapshot { .. } => OperationKind::GetCatalogSnapshot,
            Self::DeleteCatalogSnapshot { .. } => OperationKind::DeleteCatalogSnapshot,
            Self::CompareCatalogSchemas { .. } => OperationKind::CompareCatalogSchemas,
            Self::PreviewMigration { .. } => OperationKind::PreviewMigration,
            Self::ValidateMigration { .. } => OperationKind::PreviewMigration,
            Self::ApplyMigration { .. } => OperationKind::ApplyMigration,
            Self::CancelMigration { .. } => OperationKind::CancelMigration,
            Self::GetMigrationRun { .. } => OperationKind::GetMigrationRun,
            Self::GetDurableMigrationRun { .. } => OperationKind::GetMigrationRun,
            Self::StartComparison { .. } => OperationKind::StartComparison,
            Self::PageComparison { .. } => OperationKind::PageComparison,
            Self::CancelComparison { .. } => OperationKind::CancelComparison,
            Self::PrepareComparisonPatch { .. } => OperationKind::PrepareComparisonPatch,
            Self::CaptureSemanticPlan { .. } => OperationKind::CaptureSemanticPlan,
            Self::ListPlanCaptures { .. } => OperationKind::ListPlanCaptures,
            Self::GetPlanCapture { .. } => OperationKind::GetPlanCapture,
            Self::ComparePlanCaptures { .. } => OperationKind::ComparePlanCaptures,
            Self::DeletePlanCapture { .. } => OperationKind::DeletePlanCapture,
            Self::GenerateDdl { .. } => OperationKind::GenerateDdl,
            Self::ExecuteQuery { .. } => OperationKind::ExecuteQuery,
            Self::ExportQuery { .. } => OperationKind::ExportQuery,
            Self::Complete { .. } => OperationKind::Complete,
            Self::CompleteSemanticDocument { .. } => OperationKind::Complete,
            Self::HoverSemanticDocument { .. } => OperationKind::Complete,
            Self::OpenSemanticDocument { .. } => OperationKind::OpenSemanticDocument,
            Self::UpdateSemanticDocument { .. } => OperationKind::UpdateSemanticDocument,
            Self::CloseSemanticDocument { .. } => OperationKind::CloseSemanticDocument,
            Self::SelectStatement { .. } => OperationKind::SelectStatement,
            Self::DiagnoseSql { .. } => OperationKind::DiagnoseSql,
            Self::FormatSql { .. } => OperationKind::FormatSql,
            Self::SqlQuickFix { .. } => OperationKind::SqlQuickFix,
            Self::FindSqlUsages { .. } => OperationKind::FindSqlUsages,
            Self::PrepareSqlRefactor { .. } => OperationKind::PrepareSqlRefactor,
            Self::Listen { .. } => OperationKind::Listen,
            Self::CancelQuery { .. } => OperationKind::CancelQuery,
            Self::PreviewEdits { .. } => OperationKind::PreviewEdits,
            Self::ApplyEdits { .. } => OperationKind::ApplyEdits,
            Self::SearchSchema { .. } => OperationKind::SearchSchema,
            Self::SearchData { .. } => OperationKind::SearchData,
            Self::Explain { .. } => OperationKind::Explain,
            Self::ListProcesses { .. } => OperationKind::ListProcesses,
            Self::KillProcess { .. } => OperationKind::KillProcess,
            Self::ImportCsv { .. } => OperationKind::ImportCsv,
            Self::BulkInsert { .. } => OperationKind::BulkInsert,
            Self::BeginTransaction { .. } => OperationKind::BeginTransaction,
            Self::ListTransactions { .. } => OperationKind::ListTransactions,
            Self::PreviewTransaction { .. } => OperationKind::PreviewTransaction,
            Self::CommitTransaction { .. } => OperationKind::CommitTransaction,
            Self::RollbackTransaction { .. } => OperationKind::RollbackTransaction,
            Self::Savepoint { .. } => OperationKind::Savepoint,
            Self::RollbackToSavepoint { .. } => OperationKind::RollbackToSavepoint,
            Self::ReleaseSavepoint { .. } => OperationKind::ReleaseSavepoint,
            Self::Metadata { .. } => OperationKind::Metadata,
            Self::AttachRoom { .. } => OperationKind::AttachRoom,
            Self::DetachRoom { .. } => OperationKind::DetachRoom,
            Self::ApplyDocumentUpdate { .. } => OperationKind::ApplyDocumentUpdate,
            Self::ReadSharedResult { .. } => OperationKind::ReadSharedResult,
            Self::Workspace { action, .. } => match action {
                WorkspaceAction::Read => OperationKind::ReadWorkspace,
                WorkspaceAction::ReadHistory => OperationKind::ReadWorkspaceHistory,
                WorkspaceAction::CreateCheckpoint => OperationKind::ManageWorkspace,
                WorkspaceAction::RestoreCheckpoint => OperationKind::RestoreWorkspace,
                WorkspaceAction::BindProjection => OperationKind::BindWorkspaceProjection,
                WorkspaceAction::ReconcileProjection | WorkspaceAction::ResolveConflict => {
                    OperationKind::ManageWorkspaceProjection
                }
                WorkspaceAction::Create
                | WorkspaceAction::Update
                | WorkspaceAction::Delete
                | WorkspaceAction::CreateNode
                | WorkspaceAction::MoveNode
                | WorkspaceAction::DeleteNode
                | WorkspaceAction::BatchMutate => OperationKind::ManageWorkspace,
            },
            Self::Vcs { action, .. } => match action {
                VcsAction::Status
                | VcsAction::Validate
                | VcsAction::Diff
                | VcsAction::Branches
                | VcsAction::History
                | VcsAction::Conflicts
                | VcsAction::Remotes
                | VcsAction::HostingRead => OperationKind::ReadVcs,
                VcsAction::Bind
                | VcsAction::Unbind
                | VcsAction::Stage
                | VcsAction::Unstage
                | VcsAction::Commit
                | VcsAction::Amend
                | VcsAction::Uncommit
                | VcsAction::Discard
                | VcsAction::Revert
                | VcsAction::CreateBranch
                | VcsAction::SwitchBranch
                | VcsAction::RenameBranch
                | VcsAction::DeleteBranch
                | VcsAction::SetUpstream
                | VcsAction::ResolveConflict
                | VcsAction::ContinueOperation
                | VcsAction::AbortOperation
                | VcsAction::RepairBinding
                | VcsAction::AddRemote
                | VcsAction::EditRemote
                | VcsAction::RemoveRemote
                | VcsAction::SetCredential
                | VcsAction::TestCredential
                | VcsAction::RemoveCredential
                | VcsAction::Fetch
                | VcsAction::Push
                | VcsAction::SetHostingCredential
                | VcsAction::RemoveHostingCredential
                | VcsAction::CreatePullRequest => OperationKind::WriteVcs,
            },
            Self::DdlSource { action, .. } => match action {
                DdlSourceAction::Read => OperationKind::ReadDdlSource,
                DdlSourceAction::Create
                | DdlSourceAction::Update
                | DdlSourceAction::Delete
                | DdlSourceAction::Refresh
                | DdlSourceAction::Map => OperationKind::ManageDdlSource,
            },
            Self::RunConfiguration { action, .. } => match action {
                RunConfigurationAction::Read => OperationKind::ReadRunConfiguration,
                RunConfigurationAction::Create
                | RunConfigurationAction::Update
                | RunConfigurationAction::Delete
                | RunConfigurationAction::Validate => OperationKind::ManageRunConfiguration,
            },
            Self::Run { action, .. } => match action {
                RunAction::Read => OperationKind::ReadRun,
                RunAction::Start | RunAction::Cancel | RunAction::Rerun => {
                    OperationKind::ExecuteRun
                }
            },
            Self::Schedule { action, .. } => match action {
                ScheduleAction::Read => OperationKind::ReadSchedule,
                ScheduleAction::Create
                | ScheduleAction::Update
                | ScheduleAction::Enable
                | ScheduleAction::Disable
                | ScheduleAction::Resume
                | ScheduleAction::Delete => OperationKind::ManageSchedule,
            },
            Self::TransferRecipe { action, .. } => match action {
                TransferRecipeAction::Read => OperationKind::ReadTransferRecipe,
                TransferRecipeAction::Execute => OperationKind::ExecuteTransferRecipe,
                TransferRecipeAction::Create
                | TransferRecipeAction::Update
                | TransferRecipeAction::Delete
                | TransferRecipeAction::Validate => OperationKind::ManageTransferRecipe,
            },
            Self::Vault { .. } => OperationKind::Metadata,
            Self::BackupState => OperationKind::BackupState,
            Self::RestoreState { .. } => OperationKind::RestoreState,
        }
    }

    /// Sanitized `(action, target, target_id)` view for audit records.
    pub fn audit_summary(&self) -> OperationSummary {
        let summary = |action: &str, target: &str, target_id: Option<i64>| OperationSummary {
            action: action.to_string(),
            target: target.to_string(),
            target_id,
        };
        match self {
            Operation::HttpRequest { method, path, .. } => {
                summary(&method.to_lowercase(), path, None)
            }
            Operation::Authenticate { method } => summary(
                &format!("authenticate_{method:?}").to_lowercase(),
                "auth",
                None,
            ),
            Operation::RefreshAuthSession => summary("refresh", "auth_session", None),
            Operation::Logout { all_sessions } => summary(
                if *all_sessions {
                    "logout_all"
                } else {
                    "logout"
                },
                "auth_session",
                None,
            ),
            Operation::ChangePassword => summary("change_password", "auth_identity", None),
            Operation::ManagePrincipal {
                action,
                principal_id,
            } => summary(
                &format!("principal_{action:?}").to_lowercase(),
                "principal",
                *principal_id,
            ),
            Operation::ManageGithubAllowlist {
                action,
                principal_id,
            } => summary(
                &format!("github_allowlist_{action:?}").to_lowercase(),
                "github_allowlist",
                *principal_id,
            ),
            Operation::ManagePrincipalKey { action, key_id } => summary(
                &format!("principal_key_{action:?}").to_lowercase(),
                "principal_key",
                *key_id,
            ),
            Operation::ManageTenantInvitation { action, tenant_id } => summary(
                &format!("tenant_invitation_{action:?}").to_lowercase(),
                "tenant_invitation",
                Some(*tenant_id),
            ),
            Operation::ManageConnectionPolicy {
                action, profile_id, ..
            } => summary(
                &format!("connection_policy_{action:?}").to_lowercase(),
                "connection_profile",
                Some(*profile_id),
            ),
            Operation::ManageTenantLimits {
                action, tenant_id, ..
            } => summary(
                &format!("tenant_limits_{action:?}").to_lowercase(),
                "tenant",
                Some(*tenant_id),
            ),
            Operation::ManageExtension {
                action,
                extension_id,
            } => summary(
                &format!("extension_{action:?}").to_lowercase(),
                extension_id.as_str(),
                None,
            ),
            Operation::ManageInstanceConfiguration { action } => summary(
                &format!("instance_configuration_{action:?}").to_lowercase(),
                "instance_configuration",
                None,
            ),
            Operation::InvokeExtension { operation } => summary(
                operation.action.as_str(),
                operation.contribution_id.as_str(),
                None,
            ),
            Operation::ApproveOperation { .. } => summary("approve", "operation_approval", None),
            Operation::RateLimitRejected {
                route, tenant_id, ..
            } => summary("rate_limit_rejected", route, *tenant_id),
            Operation::OpenSession { .. } => summary("open", "session", None),
            Operation::ListSessions => summary("list", "session", None),
            Operation::ListAvailableOperations { .. } => {
                summary("list_available", "operation", None)
            }
            Operation::CloseSession { session } => {
                summary("close", "session", Some(session.0 as i64))
            }
            Operation::OpenConnection { session, .. } => {
                summary("open", "connection", Some(session.0 as i64))
            }
            Operation::CloseConnection { connection, .. } => {
                summary("close", "connection", Some(connection.0 as i64))
            }
            Operation::PingConnection { connection, .. } => {
                summary("ping", "connection", Some(connection.0 as i64))
            }
            Operation::RefreshSchema { connection, .. } => {
                summary("refresh", "schema", Some(connection.0 as i64))
            }
            Operation::ReadCatalogGraph {
                connection,
                refresh,
                ..
            } => summary(
                if *refresh { "refresh" } else { "read" },
                "catalog_graph",
                Some(connection.0 as i64),
            ),
            Operation::ProjectCatalogDiagram { connection, .. } => {
                summary("project", "catalog_diagram", Some(connection.0 as i64))
            }
            Operation::CreateCatalogSnapshot { connection, .. } => {
                summary("create", "catalog_snapshot", Some(connection.0 as i64))
            }
            Operation::ListCatalogSnapshots { tenant_id } => {
                summary("list", "catalog_snapshot", Some(*tenant_id))
            }
            Operation::GetCatalogSnapshot { tenant_id, .. } => {
                summary("get", "catalog_snapshot", Some(*tenant_id))
            }
            Operation::DeleteCatalogSnapshot { tenant_id, .. } => {
                summary("delete", "catalog_snapshot", Some(*tenant_id))
            }
            Operation::CompareCatalogSchemas { connection, .. } => {
                summary("compare", "schema", Some(connection.0 as i64))
            }
            Operation::PreviewMigration { connection, .. } => {
                summary("preview", "migration", Some(connection.0 as i64))
            }
            Operation::ValidateMigration { connection, .. } => {
                summary("validate", "migration", Some(connection.0 as i64))
            }
            Operation::ApplyMigration { connection, .. } => {
                summary("apply", "migration", Some(connection.0 as i64))
            }
            Operation::CancelMigration { connection, .. } => {
                summary("cancel", "migration", Some(connection.0 as i64))
            }
            Operation::GetMigrationRun { connection, .. } => {
                summary("get", "migration_run", Some(connection.0 as i64))
            }
            Operation::GetDurableMigrationRun { tenant_id, .. } => {
                summary("get", "migration_run", Some(*tenant_id))
            }
            Operation::StartComparison { session, .. } => {
                summary("start", "comparison", Some(session.0 as i64))
            }
            Operation::PageComparison { session, .. } => {
                summary("page", "comparison", Some(session.0 as i64))
            }
            Operation::CancelComparison { session, .. } => {
                summary("cancel", "comparison", Some(session.0 as i64))
            }
            Operation::PrepareComparisonPatch { session, .. } => {
                summary("prepare_patch", "comparison", Some(session.0 as i64))
            }
            Operation::CaptureSemanticPlan { connection, .. } => {
                summary("capture", "query_plan", Some(connection.0 as i64))
            }
            Operation::ListPlanCaptures { tenant_id, .. } => {
                summary("list", "plan_capture", Some(*tenant_id))
            }
            Operation::GetPlanCapture { tenant_id, .. } => {
                summary("get", "plan_capture", Some(*tenant_id))
            }
            Operation::ComparePlanCaptures { tenant_id, .. } => {
                summary("compare", "plan_capture", Some(*tenant_id))
            }
            Operation::DeletePlanCapture { tenant_id, .. } => {
                summary("delete", "plan_capture", Some(*tenant_id))
            }
            Operation::GenerateDdl { connection, .. } => {
                summary("generate", "ddl", Some(connection.0 as i64))
            }
            Operation::ExecuteQuery { session, .. } => {
                summary("execute", "query", Some(session.0 as i64))
            }
            Operation::ExportQuery { connection, .. } => {
                summary("export", "query", Some(connection.0 as i64))
            }
            Operation::Complete { session, .. } => {
                summary("complete", "query", Some(session.0 as i64))
            }
            Operation::CompleteSemanticDocument { session, .. } => {
                summary("complete", "semantic_document", Some(session.0 as i64))
            }
            Operation::HoverSemanticDocument { session, .. } => {
                summary("hover", "semantic_document", Some(session.0 as i64))
            }
            Operation::OpenSemanticDocument { session, .. } => {
                summary("open", "semantic_document", Some(session.0 as i64))
            }
            Operation::UpdateSemanticDocument { session, .. } => {
                summary("update", "semantic_document", Some(session.0 as i64))
            }
            Operation::CloseSemanticDocument { session, .. } => {
                summary("close", "semantic_document", Some(session.0 as i64))
            }
            Operation::SelectStatement { session, .. } => summary(
                "select_statement",
                "semantic_document",
                Some(session.0 as i64),
            ),
            Operation::DiagnoseSql { session, .. } => {
                summary("diagnose", "semantic_document", Some(session.0 as i64))
            }
            Operation::FormatSql { session, .. } => {
                summary("format", "semantic_document", Some(session.0 as i64))
            }
            Operation::SqlQuickFix { session, .. } => {
                summary("quick_fix", "semantic_document", Some(session.0 as i64))
            }
            Operation::FindSqlUsages { session, .. } => {
                summary("find_usages", "semantic_document", Some(session.0 as i64))
            }
            Operation::PrepareSqlRefactor { session, .. } => summary(
                "prepare_refactor",
                "semantic_document",
                Some(session.0 as i64),
            ),
            Operation::Listen { connection, .. } => {
                summary("listen", "connection", Some(connection.0 as i64))
            }
            Operation::CancelQuery { session, .. } => {
                summary("cancel", "query", Some(session.0 as i64))
            }
            Operation::PreviewEdits { connection, .. } => {
                summary("preview", "edits", Some(connection.0 as i64))
            }
            Operation::ApplyEdits { connection, .. } => {
                summary("apply", "edits", Some(connection.0 as i64))
            }
            Operation::SearchSchema { connection, .. } => {
                summary("search", "schema", Some(connection.0 as i64))
            }
            Operation::SearchData { connection, .. } => {
                summary("search", "data", Some(connection.0 as i64))
            }
            Operation::Explain { connection, .. } => {
                summary("explain", "query", Some(connection.0 as i64))
            }
            Operation::ListProcesses { connection, .. } => {
                summary("list", "process", Some(connection.0 as i64))
            }
            Operation::KillProcess { request, .. } => {
                summary("kill", "process", Some(request.process_id))
            }
            Operation::ImportCsv { connection, .. } => {
                summary("import", "table", Some(connection.0 as i64))
            }
            Operation::BulkInsert { connection, .. } => {
                summary("bulk_insert", "connection", Some(connection.0 as i64))
            }
            Operation::BeginTransaction { session, .. } => {
                summary("begin", "transaction", Some(session.0 as i64))
            }
            Operation::ListTransactions { session } => {
                summary("list", "transaction", Some(session.0 as i64))
            }
            Operation::PreviewTransaction { session, .. } => {
                summary("preview", "transaction", Some(session.0 as i64))
            }
            Operation::CommitTransaction { session, .. } => {
                summary("commit", "transaction", Some(session.0 as i64))
            }
            Operation::RollbackTransaction { session, .. } => {
                summary("rollback", "transaction", Some(session.0 as i64))
            }
            Operation::Savepoint { session, .. } => {
                summary("savepoint", "transaction", Some(session.0 as i64))
            }
            Operation::RollbackToSavepoint { session, .. } => summary(
                "rollback_to_savepoint",
                "transaction",
                Some(session.0 as i64),
            ),
            Operation::ReleaseSavepoint { session, .. } => {
                summary("release_savepoint", "transaction", Some(session.0 as i64))
            }
            Operation::Metadata { action, target, id } => summary(action, target, *id),
            Operation::AttachRoom { room_id, .. } => summary("attach", "room", Some(*room_id)),
            Operation::DetachRoom { room_id, .. } => summary("detach", "room", Some(*room_id)),
            Operation::ApplyDocumentUpdate { document_id, .. } => {
                summary("apply_update", "document", Some(*document_id))
            }
            Operation::ReadSharedResult { room_id, .. } => {
                summary("read", "room_result", Some(*room_id))
            }
            Operation::Workspace {
                action,
                workspace_id,
                node_id,
            } => summary(
                action.audit_name(),
                if node_id.is_some() {
                    "workspace_node"
                } else {
                    "workspace"
                },
                node_id.map(|id| id.0).or(workspace_id.map(|id| id.0)),
            ),
            Operation::Vcs {
                action, binding_id, ..
            } => summary(action.audit_name(), "repository", Some(binding_id.0)),
            Operation::DdlSource {
                action,
                workspace_id,
                source_id,
            } => summary(
                action.audit_name(),
                "ddl_source",
                Some(source_id.map_or(workspace_id.0, |id| id.0)),
            ),
            Operation::RunConfiguration {
                action,
                workspace_id,
                configuration_id,
            } => summary(
                action.audit_name(),
                "run_configuration",
                Some(configuration_id.map_or(workspace_id.0, |id| id.0)),
            ),
            Operation::Run {
                action,
                workspace_id,
                run_id,
            } => summary(
                action.audit_name(),
                "run",
                Some(run_id.map_or(workspace_id.0, |id| id.0)),
            ),
            Operation::Schedule {
                action,
                workspace_id,
                schedule_id,
            } => summary(
                action.audit_name(),
                "schedule",
                Some(schedule_id.map_or(workspace_id.0, |id| id.0)),
            ),
            Operation::TransferRecipe {
                action,
                workspace_id,
                recipe_id,
            } => summary(
                action.audit_name(),
                "transfer_recipe",
                Some(recipe_id.map_or(workspace_id.0, |id| id.0)),
            ),
            Operation::Vault {
                action,
                vault_id,
                item_id,
            } => summary(
                action.audit_name(),
                if item_id.is_some() {
                    "vault_item"
                } else {
                    "vault"
                },
                (*item_id).or(*vault_id),
            ),
            Operation::BackupState => summary("backup", "instance_state", None),
            Operation::RestoreState { applied } => summary(
                if *applied {
                    "restore"
                } else {
                    "validate_restore"
                },
                "instance_state",
                None,
            ),
        }
    }
}

impl WorkspaceAction {
    fn audit_name(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Create => "create",
            Self::Update => "update",
            Self::Delete => "delete",
            Self::CreateNode => "create_node",
            Self::MoveNode => "move_node",
            Self::DeleteNode => "delete_node",
            Self::BatchMutate => "batch_mutate",
            Self::CreateCheckpoint => "create_checkpoint",
            Self::ReadHistory => "read_history",
            Self::RestoreCheckpoint => "restore_checkpoint",
            Self::BindProjection => "bind_projection",
            Self::ReconcileProjection => "reconcile_projection",
            Self::ResolveConflict => "resolve_conflict",
        }
    }
}

macro_rules! single_word_audit_names {
    ($type:ty { $($variant:ident => $name:literal),+ $(,)? }) => {
        impl $type {
            fn audit_name(self) -> &'static str {
                match self {
                    $(Self::$variant => $name),+
                }
            }
        }
    };
}

single_word_audit_names!(VcsAction {
    Bind => "bind",
    Unbind => "unbind",
    Status => "status",
    Validate => "validate",
    Diff => "diff",
    Branches => "branches",
    History => "history",
    CreateBranch => "create_branch",
    SwitchBranch => "switch_branch",
    RenameBranch => "rename_branch",
    DeleteBranch => "delete_branch",
    SetUpstream => "set_upstream",
    Conflicts => "conflicts",
    ResolveConflict => "resolve_conflict",
    ContinueOperation => "continue_operation",
    AbortOperation => "abort_operation",
    RepairBinding => "repair_binding",
    Remotes => "remotes",
    AddRemote => "add_remote",
    EditRemote => "edit_remote",
    RemoveRemote => "remove_remote",
    Stage => "stage",
    Unstage => "unstage",
    Commit => "commit",
    Amend => "amend",
    Uncommit => "uncommit",
    Discard => "discard",
    Revert => "revert",
    SetCredential => "set_credential",
    TestCredential => "test_credential",
    RemoveCredential => "remove_credential",
    Fetch => "fetch",
    Push => "push",
    HostingRead => "hosting_read",
    SetHostingCredential => "set_hosting_credential",
    RemoveHostingCredential => "remove_hosting_credential",
    CreatePullRequest => "create_pull_request",
});
single_word_audit_names!(DdlSourceAction {
    Read => "read",
    Create => "create",
    Update => "update",
    Delete => "delete",
    Refresh => "refresh",
    Map => "map",
});
single_word_audit_names!(RunConfigurationAction {
    Read => "read",
    Create => "create",
    Update => "update",
    Delete => "delete",
    Validate => "validate",
});
single_word_audit_names!(RunAction {
    Start => "start",
    Read => "read",
    Cancel => "cancel",
    Rerun => "rerun",
});
single_word_audit_names!(ScheduleAction {
    Read => "read",
    Create => "create",
    Update => "update",
    Enable => "enable",
    Disable => "disable",
    Resume => "resume",
    Delete => "delete",
});
single_word_audit_names!(TransferRecipeAction {
    Read => "read",
    Create => "create",
    Update => "update",
    Delete => "delete",
    Validate => "validate",
    Execute => "execute",
});
single_word_audit_names!(crate::VaultAction {
    Read => "read",
    Create => "create",
    Update => "update",
    Delete => "delete",
    Grant => "grant",
    Revoke => "revoke",
    SetSecret => "set_secret",
    Reveal => "reveal",
    StepUp => "step_up",
    Restore => "restore",
    Test => "test",
    Use => "use",
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authentication_operations_have_no_secret_shaped_payload() {
        let encoded = serde_json::to_string(&Operation::Authenticate {
            method: AuthenticationMethod::Password,
        })
        .unwrap();
        assert_eq!(encoded, r#"{"op":"authenticate","method":"password"}"#);
        for forbidden in ["token", "secret", "code", "credential"] {
            assert!(!encoded.contains(forbidden));
        }
    }

    #[test]
    fn phase_l_operations_are_typed_and_audit_safe() {
        let operation = Operation::Vcs {
            action: VcsAction::Commit,
            workspace_id: crate::WorkspaceId(7),
            binding_id: crate::RepositoryBindingId(9),
        };
        assert_eq!(operation.kind(), OperationKind::WriteVcs);
        assert_eq!(
            operation.audit_summary(),
            OperationSummary {
                action: "commit".into(),
                target: "repository".into(),
                target_id: Some(9),
            }
        );
        let encoded = serde_json::to_string(&operation).unwrap();
        for forbidden in ["message", "path", "url", "credential", "secret"] {
            assert!(!encoded.contains(forbidden));
        }
    }
}
