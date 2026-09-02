//! `sift-client-sdk` — thin reference consumer proving the HTTP API is
//! buildable-against from outside the server crate.

pub mod room_replica;
pub use room_replica::{FollowEvent, FollowMode, Ingest, RoomReplica};

/// Stable OpenAPI operation ids implemented by this SDK.
///
/// The server's contract test compares this SDK-owned list with the document
/// extracted from the live router. Keeping the declaration here prevents the
/// server from claiming SDK coverage on the SDK's behalf.
pub const SUPPORTED_HTTP_OPERATION_IDS: &[&str] = &[
    "clearMetadataVaultItemSecret",
    "deleteMetadataVault",
    "deleteMetadataVaultGrant",
    "deleteMetadataVaultItem",
    "diffMetadataVaultItemVersions",
    "getMetadataVault",
    "getMetadataVaultItem",
    "getMetadataVaultItemVersion",
    "restoreMetadataVaultItem",
    "setMetadataVaultItemSecret",
    "testMetadataVaultItem",
    "updateMetadataVault",
    "updateMetadataVaultItem",
    "acceptTenantInvitation",
    "addMetadataRoomMember",
    "adminCreatePasswordPrincipal",
    "adminIssuePasswordReset",
    "adminLinkPasswordIdentity",
    "adminListAuthSessions",
    "adminListPrincipalIdentities",
    "adminRevokeAuthSession",
    "adminSetPrincipalDisabled",
    "adminUnlinkIdentity",
    "applyEdits",
    "applyMigration",
    "approveOperation",
    "authenticateKey",
    "beginTransaction",
    "bindMetadataRoomConnection",
    "bindWorkspaceRepository",
    "bindWorkspaceProjection",
    "bulkInsert",
    "cloneWorkspaceRepository",
    "cancelComparison",
    "cancelQuery",
    "cancelMigration",
    "captureSemanticPlan",
    "changePassword",
    "clearTenantLimits",
    "closeConnection",
    "closeSemanticDocument",
    "closeSession",
    "commitTransaction",
    "compareCatalogSchemas",
    "compareRepositoryCommits",
    "comparePlanCaptures",
    "completeSemanticDocument",
    "createGithubAllowlist",
    "createOperationApproval",
    "createMetadataDocument",
    "createMetadataRoom",
    "createMetadataSavedQuery",
    "createMetadataVault",
    "createMetadataVaultItem",
    "createRoomWorkspace",
    "createCatalogSnapshot",
    "createDdlSource",
    "createSavepoint",
    "createSession",
    "createTenantInvitation",
    "createWorkspaceCheckpoint",
    "createWorkspaceNode",
    "createRepositoryBranch",
    "deleteMetadataConnectionProfile",
    "deleteMetadataDocument",
    "deleteMetadataRoom",
    "deleteMetadataSavedQuery",
    "deleteWorkspace",
    "deleteWorkspaceNode",
    "deletePlanCapture",
    "deleteCatalogSnapshot",
    "deleteDdlSource",
    "deleteWorkspaceProjection",
    "deleteWorkspaceRepository",
    "deleteRepositoryBranch",
    "discardRepositoryPath",
    "deleteSpilledCursor",
    "diagnoseSemanticDocument",
    "disconnectMetadataConnectionProfile",
    "exchangeSshProxyCapability",
    "executeQuery",
    "explainQuery",
    "exportQuery",
    "extensionDiagnostics",
    "findSemanticUsages",
    "formatSemanticDocument",
    "getCatalogGraph",
    "getCatalogSnapshot",
    "getComparison",
    "getDurableMigrationRun",
    "getExtension",
    "getInstanceConfiguration",
    "getVcsDiagnostics",
    "getMetadataConnectionPolicy",
    "getMetadataSavedQuery",
    "getMigrationRun",
    "getPlanCapture",
    "getObjectDdl",
    "getRepositoryCommit",
    "getRepositoryHistoricalFile",
    "getRepositoryHosting",
    "getRepositoryHistory",
    "getWorkspace",
    "getWorkspaceProjection",
    "getWorkspaceRepository",
    "getRepositoryStatus",
    "getRepositoryDiff",
    "getDdlSource",
    "getRoomResult",
    "getRoomResultPages",
    "getSchema",
    "getSession",
    "getTenantUsage",
    "githubAuthCallback",
    "githubAuthStart",
    "githubNativeAuthExchange",
    "handshake",
    "health",
    "importCsv",
    "installExtension",
    "invokeExtensionAction",
    "invokeGovernedTool",
    "issueAuthToken",
    "issueKeyChallenge",
    "joinMetadataRoom",
    "killProcess",
    "leaveMetadataRoom",
    "listAudit",
    "listAuthTokens",
    "listAvailableOperations",
    "listConnections",
    "listExtensions",
    "listGithubAllowlist",
    "listHostingRepositories",
    "listGovernedTools",
    "listMetadataConnectionProfiles",
    "listMetadataDocuments",
    "listMetadataHistory",
    "listMetadataRoomMembers",
    "listMetadataRooms",
    "listMetadataSavedQueries",
    "listMetadataVaultGrants",
    "listMetadataVaultItemVersions",
    "listMetadataVaultItems",
    "listMetadataVaults",
    "listCatalogSnapshots",
    "listMetadataTenants",
    "removeMetadataTenantMember",
    "listOperationAudit",
    "listOperations",
    "listPlanCaptures",
    "listPrincipalKeys",
    "listProcesses",
    "listProviders",
    "listRoomResults",
    "listRoomWorkspaces",
    "listSessions",
    "listTenantInvitations",
    "listTransactions",
    "listWorkspaceCheckpoints",
    "listWorkspaceNodes",
    "listDdlSources",
    "logoutAllAuth",
    "logoutAuth",
    "openConnection",
    "openConnectionFromProfile",
    "openSemanticDocument",
    "openapi",
    "pageComparison",
    "pageMetadataHistory",
    "pageOperationAudit",
    "passwordLogin",
    "pingConnection",
    "planWorkspaceProjection",
    "postCompletion",
    "prepareComparisonPatch",
    "prepareSemanticQuickFix",
    "prepareSemanticRefactor",
    "previewEdits",
    "previewMigration",
    "previewTransaction",
    "projectCatalogDiagram",
    "previewCatalogDiagramMutation",
    "purgeExtension",
    "readSpilledCursorPages",
    "ready",
    "refreshAuth",
    "refreshDdlSource",
    "registerPrincipalKey",
    "releaseSavepoint",
    "revertRepositoryHunk",
    "revertRepositoryCommit",
    "renameRepositoryBranch",
    "removeMetadataRoomMember",
    "restoreWorkspaceCheckpoint",
    "restoreRepositoryHistoricalFile",
    "resetPassword",
    "revokeAuthToken",
    "revokeGithubAllowlist",
    "revokePrincipalKey",
    "revokeTenantInvitation",
    "rollbackExtension",
    "rollbackToSavepoint",
    "rollbackTransaction",
    "roomWebSocket",
    "searchData",
    "searchSchema",
    "selectSemanticStatement",
    "sessionWebSocket",
    "setMetadataConnectionCredential",
    "setHostingCredential",
    "setRepositoryCredential",
    "setRepositoryUpstream",
    "stageRepositoryPaths",
    "stageRepositoryHunk",
    "createHostingPullRequest",
    "switchRepositoryBranch",
    "unstageRepositoryPaths",
    "unstageRepositoryHunk",
    "commitRepository",
    "amendRepository",
    "uncommitRepository",
    "fetchRepository",
    "pushRepository",
    "listRepositoryBranches",
    "setTenantLimits",
    "stepUpMetadataVaultReveal",
    "startComparison",
    "unbindMetadataRoomConnection",
    "uninstallExtension",
    "updateExtensionGrants",
    "updateExtensionSelection",
    "updateExtensionTenant",
    "updateInstanceConfiguration",
    "updateMetadataConnectionPolicy",
    "updateMetadataDocument",
    "updateMetadataSavedQuery",
    "updateWorkspace",
    "updateDdlSource",
    "applyWorkspaceProjection",
    "moveWorkspaceNode",
    "mutateWorkspaceBatch",
    "updateSemanticDocument",
    "upsertMetadataConnectionProfile",
    "validateExtension",
    "cancelRun",
    "createRunConfiguration",
    "createRunSchedule",
    "createTransferRecipe",
    "deleteRunConfiguration",
    "deleteRunSchedule",
    "deleteTransferRecipe",
    "deleteRepositoryCredential",
    "disableRunSchedule",
    "enableRunSchedule",
    "executeTransferRecipe",
    "exportChangeLedger",
    "getRun",
    "getChangeLedgerPolicy",
    "getLatestSuccessfulRunForCommit",
    "getRunConfiguration",
    "getRunLogs",
    "getRunSteps",
    "getRunSchedule",
    "getTransferRecipe",
    "getWorkspaceArtifact",
    "listRunConfigurations",
    "listChangeLedger",
    "listRunSchedules",
    "listScheduleOccurrences",
    "listTransferRecipes",
    "rerun",
    "resumeScheduleOccurrence",
    "importExternalChangeLedger",
    "startRun",
    "updateRunConfiguration",
    "updateChangeLedgerPolicy",
    "updateRunSchedule",
    "updateTransferRecipe",
    "validateRunConfiguration",
    "validateTransferRecipe",
    "validateMigration",
    "abortRepositoryOperation",
    "beginRepositoryConflictResolution",
    "continueRepositoryOperation",
    "getRepositoryConflict",
    "listRepositoryRemotes",
    "markRepositoryConflictResolved",
    "repairRepositoryBinding",
    "revealMetadataVaultItem",
    "addRepositoryRemote",
    "removeRepositoryRemote",
    "renameRepositoryRemote",
    "resolveRepositoryConflict",
    "setMetadataVaultGrant",
    "testRepositoryCredential",
    "deleteHostingCredential",
    "updateRepositoryRemote",
    "whoAmI",
];

// Pure request/response DTOs shared with the server. Re-export so downstream
// consumers need no server-internal storage crate.
pub use sift_api_types::{
    AddRoomMemberRequest, ApplyWorkspaceProjectionRequest, BindRepositoryRequest,
    BindRoomConnectionRequest, BindWorkspaceProjectionRequest, CloneWorkspaceRepositoryRequest,
    CreateDdlSourceRequest, CreateDocumentRequest, CreateRoomRequest,
    CreateRunConfigurationRequest, CreateRunScheduleRequest, CreateSavedQueryRequest,
    CreateTransferRecipeRequest, CreateVaultItemRequest, CreateVaultRequest,
    CreateWorkspaceCheckpointRequest, CreateWorkspaceNodeRequest, CreateWorkspaceRequest,
    ExecuteTransferRecipeRequest, ExpectedDdlSourceRevisionRequest,
    ExpectedProjectionRevisionRequest, ExpectedRepositoryRevisionRequest,
    ExpectedRunConfigurationRevisionRequest, ExpectedTransferRecipeRevisionRequest,
    ExpectedWorkspaceRevisionRequest, InstanceConfigurationDocument, IssueTokenRequest,
    IssueTokenResponse, MoveWorkspaceNodeRequest, OpenConnectionFromProfileRequest,
    ProjectionResolutionRequest, RestoreWorkspaceCheckpointRequest, RevealVaultSecretResponse,
    RunLogQuery, ScheduleOccurrenceQuery, SetCredentialRequest, SetVaultGrantRequest,
    SetVcsCredentialRequest, StartRunRequest, UpdateDdlSourceRequest,
    UpdateDocumentSnapshotRequest, UpdateInstanceConfigurationRequest,
    UpdateRunConfigurationRequest, UpdateRunScheduleRequest, UpdateSavedQueryRequest,
    UpdateTransferRecipeRequest, UpdateWorkspaceRequest, UpsertConnectionProfileRequest,
    VcsBeginConflictResolutionRequest, VcsCommitRequest, VcsCompareQuery, VcsConflictQuery,
    VcsCreateBranchRequest, VcsCredentialTestRequest, VcsDeleteBranchRequest, VcsDiffQuery,
    VcsDiscardRequest, VcsHistoryQuery, VcsHunkRequest, VcsMarkConflictResolvedRequest,
    VcsPathsRequest, VcsRemoteDeleteRequest, VcsRemoteMutationRequest, VcsRemoteRenameRequest,
    VcsRemoteRequest, VcsRenameBranchRequest, VcsRepositoryOperationRequest,
    VcsResolveConflictRequest, VcsRestoreHistoricalFileRequest, VcsRevertCommitRequest,
    VcsRevertHunkRequest, VcsSetUpstreamRequest, VcsSwitchBranchRequest, VcsUncommitRequest,
    WorkspaceBatchMutationItem, WorkspaceBatchMutationRequest, WorkspaceTreeResponse,
};
use sift_api_types::{
    ApiTokenId, ApiTokenRow, ConnectionProfile, ConnectionProfileId, Document, DocumentId,
    GithubAllowlistEntry, OperationAudit, PrincipalKey, QueryHistory, Room, RoomId, RoomMember,
    SavedQuery, SavedQueryId, SavedQueryScope, TenantId, TenantInvitation, TenantLimitOverride,
    TenantMembership, Vault, VaultGrant, VaultId, VaultItem, VaultItemId, VaultItemVersion,
};
use sift_protocol::{
    AcceptTenantInvitationRequest, AdminCreatePasswordPrincipalRequest,
    AdminLinkPasswordIdentityRequest, AdminSetPrincipalDisabledRequest, ApiErrorResponse,
    ApplyEditsRequest, ApplyEditsResult, AuthIdentitySummary, AuthPrincipal, AuthSessionSummary,
    AuthTokensResponse, BeginTransactionRequest, BulkInsertRequest, BulkInsertResponse,
    CancelRequest, ChangePasswordRequest, ConnectionId, ConnectionInfo, ConnectionPolicy,
    CreateGithubAllowlistRequest, CreateTenantInvitationRequest, CsvImportRequest,
    CsvImportResponse, CursorId, CursorPage, DataSearchRequest, DataSearchResponse,
    DatabaseProcess, DdlSource, DdlSourceId, DdlSourceModel, DisconnectManagedConnectionsResponse,
    EditPlan, EndTransactionRequest, ExecuteRequestHttp, ExecuteResponse, ExpectedRevision,
    ExplainRequest, ExplainResponse, ExtensionDescriptor, ExtensionDiagnostics,
    ExtensionGrantRequest, ExtensionPurgeResponse, ExtensionSelectionRequest,
    ExtensionTenantSelectionRequest, GithubNativeAuthExchangeRequest,
    GithubNativeAuthStartResponse, GovernedToolDescriptor, HandshakeClientKind, HandshakeRequest,
    HandshakeResponse, Health, HostingRepositoryCandidate, HostingRepositorySummary,
    InvokeExtensionOutcome, InvokeExtensionRequest, InvokeToolRequest, InvokeToolResponse,
    IssuedPasswordResetResponse, IssuedTenantInvitationResponse, KeyAuthenticateRequest,
    KeyChallengeRequest, KeyChallengeResponse, KillProcessRequest, KillProcessResponse,
    OpenConnectionRequest, OpenSessionRequest, OperationApproval, OperationCapability,
    OperationCapabilityContext, Page, PasswordLoginRequest, PasswordResetRequest,
    PreviewEditsRequest, ProjectionBinding, ProjectionBindingId, ProtocolRange, ProviderDescriptor,
    Readiness, ReconcilePlan, RefreshAuthRequest, RegisterPrincipalKeyRequest, RepositoryBinding,
    RepositoryBindingId, RoomQueryResult, RoomResultId, RoomResultPages, RoomSelection, Run,
    RunConfiguration, RunConfigurationId, RunId, RunLogEntry, RunManifest, RunSchedule,
    RunStepResult, SavepointRequest, ScheduleId, ScheduleOccurrence, ScheduleOccurrenceId,
    SchemaSearchRequest, SchemaSearchResponse, SchemaSnapshot, ServerInfo, SessionId, SessionInfo,
    SshProxyAccessGrant, SshProxyCapabilityExchangeRequest, TenantResourceLimits,
    TenantUsageSnapshot, ToolContext, TransactionEndAction, TransactionInfo, TransactionPreview,
    TransactionPreviewRequest, TransactionState, TransferExecutionResult, TransferRecipe,
    TransferRecipeId, TxHandleRef, TxId, TxMode, UpdateConnectionPolicyRequest,
    UpdateTenantLimitsRequest, ValidatedExtensionPackage, Value, VcsAdapterDiagnostics, VcsBranch,
    VcsCommitDetail, VcsCommitResult, VcsConflictFile, VcsDiff, VcsDiffSide, VcsHeadMutationResult,
    VcsHistoricalFile, VcsHistoryPage, VcsRemote, VcsRemoteResult, VcsStatus,
    VcsWorktreeMutationResult, WebAuthResponse, WhoAmIResponse, Workspace, WorkspaceArtifactId,
    WorkspaceCheckpoint, WorkspaceCheckpointId, WorkspaceId, WorkspaceNodeId, WorkspacePath,
    WsClientMessage, WsServerMessage, PROTOCOL_VERSION_NUMBER,
};

#[derive(Clone)]
pub struct SessionTokenProvider {
    tokens: std::sync::Arc<tokio::sync::RwLock<AuthTokensResponse>>,
}

impl std::fmt::Debug for SessionTokenProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionTokenProvider")
            .field("tokens", &"[REDACTED]")
            .finish()
    }
}

impl SessionTokenProvider {
    pub fn new(tokens: AuthTokensResponse) -> Self {
        Self {
            tokens: std::sync::Arc::new(tokio::sync::RwLock::new(tokens)),
        }
    }

    /// Copy the current token pair for storage in a platform credential vault.
    /// Callers must treat the returned value as secret material.
    pub async fn snapshot(&self) -> AuthTokensResponse {
        self.tokens.read().await.clone()
    }

    async fn access_token(&self) -> String {
        self.tokens.read().await.access_token.clone()
    }

    async fn refresh_token(&self) -> String {
        self.tokens.read().await.refresh_token.clone()
    }

    async fn replace(&self, tokens: AuthTokensResponse) {
        *self.tokens.write().await = tokens;
    }

    pub async fn reauthenticate_session_websocket(
        &self,
        socket: &mut SessionWebSocket,
    ) -> Result<chrono::DateTime<chrono::Utc>> {
        socket.reauthenticate(self.access_token().await).await
    }

    pub async fn reauthenticate_room_websocket(
        &self,
        socket: &mut RoomWebSocket,
    ) -> Result<chrono::DateTime<chrono::Utc>> {
        socket.reauthenticate(self.access_token().await).await
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("http transport error: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("server error {status}: {}", error.message)]
    Server {
        status: reqwest::StatusCode,
        error: ApiErrorResponse,
    },
    #[error("websocket error: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone)]
pub struct Client {
    base: String,
    token: Option<String>,
    session_tokens: Option<SessionTokenProvider>,
    http: reqwest::Client,
    handshake: std::sync::Arc<tokio::sync::OnceCell<HandshakeResponse>>,
}

/// A query export body consumed incrementally with transport backpressure.
pub struct ExportStream {
    content_type: Option<String>,
    content_disposition: Option<String>,
    body: std::pin::Pin<
        Box<dyn futures::Stream<Item = std::result::Result<bytes::Bytes, reqwest::Error>> + Send>,
    >,
}

impl ExportStream {
    pub fn content_type(&self) -> Option<&str> {
        self.content_type.as_deref()
    }

    pub fn content_disposition(&self) -> Option<&str> {
        self.content_disposition.as_deref()
    }
}

impl futures::Stream for ExportStream {
    type Item = std::result::Result<bytes::Bytes, reqwest::Error>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.body.as_mut().poll_next(context)
    }
}

type TransportWebSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

const PROTOCOL_VERSION_HEADER: &str = "x-sift-protocol-version";

pub struct SessionWebSocket {
    socket: TransportWebSocket,
}

/// A live PostgreSQL LISTEN stream carried over a dedicated session socket.
pub struct NotificationStream {
    socket: SessionWebSocket,
    request_id: String,
}

impl NotificationStream {
    pub async fn next(&mut self) -> Result<(String, String)> {
        loop {
            match self.socket.next().await? {
                WsServerMessage::Notification {
                    request_id,
                    channel,
                    payload,
                } if request_id == self.request_id => return Ok((channel, payload)),
                WsServerMessage::Error { message, .. } => return Err(Error::Protocol(message)),
                _ => {}
            }
        }
    }
}

/// One server-backed query cursor carried over a session WebSocket.
///
/// Pages are delivered one at a time. Callers must acknowledge each
/// non-terminal page after consuming it; until then the server will not send
/// another page. This makes UI backpressure explicit instead of collecting an
/// unbounded `Vec<Page>` in the SDK.
pub struct QueryStream {
    socket: SessionWebSocket,
    connection: ConnectionId,
    cursor_id: CursorId,
}

/// Explicit execution-v2 events carried over one acknowledged server cursor.
pub struct QueryEventStream {
    socket: SessionWebSocket,
    connection: ConnectionId,
    cursor_id: CursorId,
}

#[derive(Debug, serde::Deserialize)]
pub struct SpilledCursorPages {
    pub cursor_id: CursorId,
    pub pages: Vec<Page>,
    pub done: bool,
}

impl QueryStream {
    pub const fn cursor_id(&self) -> CursorId {
        self.cursor_id
    }

    pub async fn next_page(&mut self) -> Result<(u64, Page)> {
        match self.socket.next().await? {
            WsServerMessage::Page {
                cursor_id,
                seq,
                page,
            } if cursor_id == self.cursor_id => Ok((seq, page)),
            WsServerMessage::Error { message, .. } => Err(Error::Protocol(message)),
            other => Err(Error::Protocol(format!(
                "unexpected websocket message: {other:?}"
            ))),
        }
    }

    pub async fn acknowledge(&mut self, seq: u64) -> Result<()> {
        self.socket
            .send(WsClientMessage::Ack {
                cursor_id: self.cursor_id,
                seq,
            })
            .await
    }

    pub async fn cancel(&mut self) -> Result<()> {
        self.socket
            .send(WsClientMessage::Cancel {
                connection: self.connection,
                cursor_id: self.cursor_id,
            })
            .await
    }
}

impl QueryEventStream {
    pub const fn cursor_id(&self) -> CursorId {
        self.cursor_id
    }

    pub async fn next_events(&mut self) -> Result<(u64, Vec<sift_protocol::ExecutionEventV2>)> {
        match self.socket.next().await? {
            WsServerMessage::ExecutionEvents {
                cursor_id,
                seq,
                events,
            } if cursor_id == self.cursor_id => Ok((seq, events)),
            WsServerMessage::Error { message, .. } => Err(Error::Protocol(message)),
            other => Err(Error::Protocol(format!(
                "unexpected websocket message: {other:?}"
            ))),
        }
    }

    pub async fn acknowledge(&mut self, seq: u64) -> Result<()> {
        self.socket
            .send(WsClientMessage::Ack {
                cursor_id: self.cursor_id,
                seq,
            })
            .await
    }

    pub async fn cancel(&mut self) -> Result<()> {
        self.socket
            .send(WsClientMessage::Cancel {
                connection: self.connection,
                cursor_id: self.cursor_id,
            })
            .await
    }
}

impl SessionWebSocket {
    pub async fn send(&mut self, message: WsClientMessage) -> Result<()> {
        use futures::SinkExt;
        use tokio_tungstenite::tungstenite::Message;

        self.socket
            .send(Message::Text(serde_json::to_string(&message)?.into()))
            .await?;
        Ok(())
    }

    pub async fn next(&mut self) -> Result<WsServerMessage> {
        next_ws(&mut self.socket).await
    }

    pub async fn reauthenticate(
        &mut self,
        access_token: impl Into<String>,
    ) -> Result<chrono::DateTime<chrono::Utc>> {
        self.send(WsClientMessage::Reauthenticate {
            access_token: sift_protocol::RedactedString(access_token.into()),
        })
        .await?;
        match self.next().await? {
            WsServerMessage::Authenticated { expires_at } => Ok(expires_at),
            WsServerMessage::Error { message, .. } => Err(Error::Protocol(message)),
            other => Err(Error::Protocol(format!(
                "expected WebSocket authentication acknowledgement, got {other:?}"
            ))),
        }
    }
}

pub struct RoomWebSocket {
    socket: TransportWebSocket,
}

/// Room transport that reconnects, re-attaches, and replays CRDT discovery
/// before yielding further events. The replica remains caller-owned so it can
/// be persisted independently of the socket lifecycle.
pub struct PersistentRoomClient {
    client: Client,
    room: RoomId,
    client_id: String,
    socket: Option<RoomWebSocket>,
    attachment_id: Option<i64>,
    reconnect_attempt: u32,
}

impl PersistentRoomClient {
    pub fn attachment_id(&self) -> Option<i64> {
        self.attachment_id
    }

    pub async fn connect(&mut self, replica: &mut RoomReplica) -> Result<()> {
        self.reconnect(replica).await
    }

    /// Receive the next meaningful replica event. Transport loss and
    /// `ResyncRequired` are recovered internally through a fresh attach plus
    /// version-vector sync.
    pub async fn next(&mut self, replica: &mut RoomReplica) -> Result<Ingest> {
        loop {
            if self.socket.is_none() {
                self.reconnect(replica).await?;
            }
            let result = self
                .socket
                .as_mut()
                .expect("socket established")
                .pump(replica)
                .await;
            match result {
                Ok(Ingest::Resync) => {
                    self.socket
                        .as_mut()
                        .expect("socket established")
                        .sync_document(replica)
                        .await?;
                }
                Ok(event) => {
                    self.reconnect_attempt = 0;
                    return Ok(event);
                }
                Err(error) if reconnectable_client_error(&error) => {
                    self.socket = None;
                    self.attachment_id = None;
                    self.wait_before_reconnect().await;
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Submit a local CRDT update. If the connection disappears, reconnect
    /// discovery sends the replica's pending update set, so no positional edit
    /// needs to be reconstructed or blindly repeated.
    pub async fn submit(
        &mut self,
        replica: &mut RoomReplica,
        message: sift_protocol::RoomClientMessage,
    ) -> Result<()> {
        if self.socket.is_none() {
            self.reconnect(replica).await?;
        }
        match self
            .socket
            .as_mut()
            .expect("socket established")
            .submit_update(replica, message)
            .await
        {
            Ok(()) => {
                self.reconnect_attempt = 0;
                Ok(())
            }
            Err(error) if reconnectable_client_error(&error) => {
                self.socket = None;
                self.attachment_id = None;
                self.wait_before_reconnect().await;
                self.reconnect(replica).await
            }
            Err(error) => Err(error),
        }
    }

    pub async fn heartbeat(&mut self, replica: &mut RoomReplica) -> Result<()> {
        if self.socket.is_none() {
            self.reconnect(replica).await?;
        }
        if let Err(error) = self
            .socket
            .as_mut()
            .expect("socket established")
            .heartbeat()
            .await
        {
            if !reconnectable_client_error(&error) {
                return Err(error);
            }
            self.socket = None;
            self.attachment_id = None;
            self.wait_before_reconnect().await;
            self.reconnect(replica).await?;
        }
        Ok(())
    }

    async fn reconnect(&mut self, replica: &mut RoomReplica) -> Result<()> {
        loop {
            match self.client.connect_room_websocket(self.room).await {
                Ok(mut socket) => {
                    let attachment_id = match socket.attach(self.client_id.clone()).await {
                        Ok(id) => id,
                        Err(error) if reconnectable_client_error(&error) => {
                            self.wait_before_reconnect().await;
                            continue;
                        }
                        Err(error) => return Err(error),
                    };
                    match socket.sync_document(replica).await {
                        Ok(()) => {
                            self.attachment_id = Some(attachment_id);
                            self.socket = Some(socket);
                            self.reconnect_attempt = 0;
                            return Ok(());
                        }
                        Err(error) if reconnectable_client_error(&error) => {
                            self.wait_before_reconnect().await;
                        }
                        Err(error) => return Err(error),
                    }
                }
                Err(error) if reconnectable_client_error(&error) => {
                    self.wait_before_reconnect().await;
                }
                Err(error) => return Err(error),
            }
        }
    }

    async fn wait_before_reconnect(&mut self) {
        let exponent = self.reconnect_attempt.min(6);
        let delay_ms = 100_u64.saturating_mul(1_u64 << exponent).min(5_000);
        self.reconnect_attempt = self.reconnect_attempt.saturating_add(1);
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
    }
}

fn reconnectable_client_error(error: &Error) -> bool {
    matches!(error, Error::WebSocket(_) | Error::Transport(_))
        || matches!(error, Error::Protocol(message) if message == "websocket closed")
}

impl RoomWebSocket {
    pub async fn send(&mut self, message: sift_protocol::RoomClientMessage) -> Result<()> {
        use futures::SinkExt;
        use tokio_tungstenite::tungstenite::Message;

        self.socket
            .send(Message::Text(serde_json::to_string(&message)?.into()))
            .await?;
        Ok(())
    }

    pub async fn next(&mut self) -> Result<sift_protocol::RoomServerMessage> {
        next_room_ws(&mut self.socket).await
    }

    pub async fn heartbeat(&mut self) -> Result<()> {
        self.send(sift_protocol::RoomClientMessage::PresenceHeartbeat)
            .await
    }

    pub async fn update_presence(
        &mut self,
        active_document_id: Option<i64>,
        selection: Option<RoomSelection>,
    ) -> Result<()> {
        self.send(sift_protocol::RoomClientMessage::PresenceUpdate {
            active_document_id,
            selection,
        })
        .await
    }

    pub async fn reauthenticate(
        &mut self,
        access_token: impl Into<String>,
    ) -> Result<chrono::DateTime<chrono::Utc>> {
        self.send(sift_protocol::RoomClientMessage::Reauthenticate {
            access_token: sift_protocol::RedactedString(access_token.into()),
        })
        .await?;
        match self.next().await? {
            sift_protocol::RoomServerMessage::Authenticated { expires_at } => Ok(expires_at),
            sift_protocol::RoomServerMessage::Error { message } => Err(Error::Protocol(message)),
            sift_protocol::RoomServerMessage::RateLimited { retry_after_ms } => {
                Err(Error::Protocol(format!(
                    "room WebSocket rate limited for {retry_after_ms}ms"
                )))
            }
            other => Err(Error::Protocol(format!(
                "expected room WebSocket authentication acknowledgement, got {other:?}"
            ))),
        }
    }

    /// Attach to the room, returning once the server acknowledges.
    pub async fn attach(&mut self, client_id: impl Into<String>) -> Result<i64> {
        self.attach_with_presence(client_id)
            .await
            .map(|(attachment_id, _)| attachment_id)
    }

    /// Attach to the room and preserve the initial ephemeral presence snapshot.
    pub async fn attach_with_presence(
        &mut self,
        client_id: impl Into<String>,
    ) -> Result<(i64, Vec<sift_protocol::RoomPresence>)> {
        self.send(sift_protocol::RoomClientMessage::Attach {
            client_id: client_id.into(),
        })
        .await?;
        loop {
            match self.next().await? {
                sift_protocol::RoomServerMessage::Attached {
                    attachment_id,
                    presence,
                    ..
                } => {
                    return Ok((attachment_id, presence));
                }
                sift_protocol::RoomServerMessage::Error { message } => {
                    return Err(Error::Protocol(message))
                }
                _ => continue,
            }
        }
    }

    /// Drive a full `DocumentSync`: import the server's response and, when this
    /// replica diverged offline, submit its missing updates.
    pub async fn sync_document(&mut self, replica: &mut RoomReplica) -> Result<()> {
        let (_, message) = replica.sync_message();
        self.send(message).await?;
        loop {
            let incoming = self.next().await?;
            match replica.ingest(&incoming).map_err(doc_err)? {
                Ingest::Synced(server_version) => {
                    // Offline divergence: submit everything the server is missing
                    // and wait for it to durably commit before returning.
                    if let Some(update) = replica.catch_up(&server_version).map_err(doc_err)? {
                        self.submit_update(replica, update).await?;
                    }
                    return Ok(());
                }
                Ingest::Error { code, message } => {
                    return Err(Error::Protocol(format!("{code:?}: {message}")))
                }
                _ => continue,
            }
        }
    }

    /// Send a prepared `DocumentUpdate` and wait for its durable ACK, importing
    /// any peer commits that arrive meanwhile.
    pub async fn submit_update(
        &mut self,
        replica: &mut RoomReplica,
        message: sift_protocol::RoomClientMessage,
    ) -> Result<()> {
        let update_id = match &message {
            sift_protocol::RoomClientMessage::DocumentUpdate { update_id, .. } => update_id.clone(),
            _ => return Err(Error::Protocol("expected a document update message".into())),
        };
        self.send(message).await?;
        loop {
            let incoming = self.next().await?;
            match replica.ingest(&incoming).map_err(doc_err)? {
                Ingest::Acked(id) if id == update_id => return Ok(()),
                Ingest::Error { code, message } => {
                    return Err(Error::Protocol(format!("{code:?}: {message}")))
                }
                _ => continue,
            }
        }
    }

    /// Read one message and fold it into the replica (e.g. a peer's commit).
    pub async fn pump(&mut self, replica: &mut RoomReplica) -> Result<Ingest> {
        let incoming = self.next().await?;
        replica.ingest(&incoming).map_err(doc_err)
    }
}

fn doc_err(error: sift_doc::DocError) -> Error {
    Error::Protocol(format!("crdt error: {error}"))
}

impl Client {
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            token: None,
            session_tokens: None,
            http: reqwest::Client::new(),
            handshake: std::sync::Arc::new(tokio::sync::OnceCell::new()),
        }
    }

    /// Eagerly negotiate compatibility and return the selected server
    /// contract. Normal methods perform this lazily and share the same result.
    pub async fn connect(&self) -> Result<HandshakeResponse> {
        Ok(self.negotiated().await?.clone())
    }

    async fn negotiated(&self) -> Result<&HandshakeResponse> {
        self.handshake
            .get_or_try_init(|| async {
                let response = self
                    .http
                    .post(self.url("/v1/handshake"))
                    .json(&HandshakeRequest {
                        client_version: env!("CARGO_PKG_VERSION").into(),
                        client_kind: HandshakeClientKind::Sdk,
                        protocol: ProtocolRange::exact(PROTOCOL_VERSION_NUMBER),
                    })
                    .send()
                    .await?;
                let status = response.status();
                if !status.is_success() {
                    return Err(server_error(response).await);
                }
                let selected = response
                    .headers()
                    .get(PROTOCOL_VERSION_HEADER)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        Error::Protocol(
                            "handshake response omitted X-Sift-Protocol-Version".into(),
                        )
                    })?;
                let body: HandshakeResponse = response.json().await?;
                if selected != body.selected_protocol.to_string()
                    || body.selected_protocol != PROTOCOL_VERSION_NUMBER
                    || !body.protocol.is_valid()
                {
                    return Err(Error::Protocol(format!(
                        "invalid handshake selection: header={selected}, body={}, server_range={}-{}",
                        body.selected_protocol, body.protocol.minimum, body.protocol.maximum
                    )));
                }
                Ok(body)
            })
            .await
    }

    pub fn with_bearer_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self.session_tokens = None;
        self
    }

    pub fn with_session_tokens(mut self, provider: SessionTokenProvider) -> Self {
        self.token = None;
        self.session_tokens = Some(provider);
        self
    }

    pub fn persistent_room(
        &self,
        room: RoomId,
        client_id: impl Into<String>,
    ) -> PersistentRoomClient {
        PersistentRoomClient {
            client: self.clone(),
            room,
            client_id: client_id.into(),
            socket: None,
            attachment_id: None,
            reconnect_attempt: 0,
        }
    }

    pub async fn password_login(
        &self,
        request: PasswordLoginRequest,
    ) -> Result<SessionTokenProvider> {
        let tokens: AuthTokensResponse = self.post("/v1/auth/login", &request).await?;
        Ok(SessionTokenProvider::new(tokens))
    }

    pub async fn refresh_session(&self) -> Result<()> {
        let provider = self.session_tokens.as_ref().ok_or_else(|| {
            Error::Protocol("client has no interactive session token provider".into())
        })?;
        let tokens: AuthTokensResponse = self
            .post(
                "/v1/auth/refresh",
                &RefreshAuthRequest {
                    refresh_token: Some(provider.refresh_token().await),
                },
            )
            .await?;
        provider.replace(tokens).await;
        Ok(())
    }

    pub async fn whoami(&self) -> Result<WhoAmIResponse> {
        self.get("/v1/auth/whoami").await
    }

    pub async fn logout(&self) -> Result<()> {
        let _: serde_json::Value = self.post_empty("/v1/auth/logout").await?;
        Ok(())
    }

    pub async fn logout_all(&self) -> Result<()> {
        let _: serde_json::Value = self.post_empty("/v1/auth/logout-all").await?;
        Ok(())
    }

    pub async fn change_password(&self, request: ChangePasswordRequest) -> Result<()> {
        let _: serde_json::Value = self.put("/v1/auth/password", &request).await?;
        Ok(())
    }

    pub async fn reset_password(&self, request: PasswordResetRequest) -> Result<()> {
        let _: serde_json::Value = self.post("/v1/auth/password/reset", &request).await?;
        Ok(())
    }

    pub async fn github_authorization_url(&self) -> Result<String> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let selected = self.negotiated().await?.selected_protocol;
        let response = client
            .get(self.url("/v1/auth/github/start?client_kind=web"))
            .header(PROTOCOL_VERSION_HEADER, selected.to_string())
            .send()
            .await?;
        validate_response_protocol(&response, selected).map_err(Error::Protocol)?;
        if !response.status().is_redirection() {
            return Err(server_error(response).await);
        }
        response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
            .ok_or_else(|| Error::Protocol("GitHub authorization redirect omitted Location".into()))
    }

    pub async fn github_native_start(&self) -> Result<GithubNativeAuthStartResponse> {
        self.get("/v1/auth/github/start?client_kind=native").await
    }

    pub async fn github_native_exchange(
        &self,
        handoff_token: String,
    ) -> Result<SessionTokenProvider> {
        let tokens: AuthTokensResponse = self
            .post(
                "/v1/auth/github/exchange",
                &GithubNativeAuthExchangeRequest { handoff_token },
            )
            .await?;
        Ok(SessionTokenProvider::new(tokens))
    }

    pub async fn github_callback(&self, code: &str, state: &str) -> Result<WebAuthResponse> {
        self.get(&format!(
            "/v1/auth/github/callback?code={}&state={}",
            urlencoding_replace(code),
            urlencoding_replace(state)
        ))
        .await
    }

    pub async fn github_allowlist(&self) -> Result<Vec<GithubAllowlistEntry>> {
        self.get("/v1/admin/auth/github-allowlist").await
    }

    pub async fn create_github_allowlist_entry(
        &self,
        request: CreateGithubAllowlistRequest,
    ) -> Result<GithubAllowlistEntry> {
        self.post("/v1/admin/auth/github-allowlist", &request).await
    }

    pub async fn revoke_github_allowlist_entry(&self, id: i64) -> Result<()> {
        self.delete(&format!("/v1/admin/auth/github-allowlist/{id}"))
            .await
    }

    pub async fn admin_create_principal(
        &self,
        request: AdminCreatePasswordPrincipalRequest,
    ) -> Result<AuthPrincipal> {
        self.post("/v1/admin/principals", &request).await
    }

    pub async fn admin_set_principal_disabled(
        &self,
        principal_id: i64,
        disabled: bool,
    ) -> Result<()> {
        let _: serde_json::Value = self
            .put(
                &format!("/v1/admin/principals/{principal_id}/disabled"),
                &AdminSetPrincipalDisabledRequest { disabled },
            )
            .await?;
        Ok(())
    }

    pub async fn admin_principal_identities(
        &self,
        principal_id: i64,
    ) -> Result<Vec<AuthIdentitySummary>> {
        self.get(&format!("/v1/admin/principals/{principal_id}/identities"))
            .await
    }

    pub async fn admin_link_password_identity(
        &self,
        principal_id: i64,
        request: AdminLinkPasswordIdentityRequest,
    ) -> Result<AuthIdentitySummary> {
        self.post(
            &format!("/v1/admin/principals/{principal_id}/identities/password"),
            &request,
        )
        .await
    }

    pub async fn admin_unlink_identity(&self, principal_id: i64, identity_id: i64) -> Result<()> {
        self.delete(&format!(
            "/v1/admin/principals/{principal_id}/identities/{identity_id}"
        ))
        .await
    }

    pub async fn admin_auth_sessions(&self, principal_id: i64) -> Result<Vec<AuthSessionSummary>> {
        self.get(&format!(
            "/v1/admin/principals/{principal_id}/auth-sessions"
        ))
        .await
    }

    pub async fn admin_revoke_auth_session(
        &self,
        principal_id: i64,
        session_id: &str,
    ) -> Result<()> {
        self.delete(&format!(
            "/v1/admin/principals/{principal_id}/auth-sessions/{session_id}"
        ))
        .await
    }

    pub async fn admin_issue_password_reset(
        &self,
        principal_id: i64,
        identity_id: i64,
    ) -> Result<IssuedPasswordResetResponse> {
        self.post_empty(&format!(
            "/v1/admin/principals/{principal_id}/identities/{identity_id}/password-reset"
        ))
        .await
    }

    pub async fn create_tenant_invitation(
        &self,
        tenant: TenantId,
        request: CreateTenantInvitationRequest,
    ) -> Result<IssuedTenantInvitationResponse> {
        self.post(
            &format!("/v1/metadata/tenants/{}/invitations", tenant.0),
            &request,
        )
        .await
    }

    pub async fn tenant_invitations(&self, tenant: TenantId) -> Result<Vec<TenantInvitation>> {
        self.get(&format!("/v1/metadata/tenants/{}/invitations", tenant.0))
            .await
    }

    pub async fn revoke_tenant_invitation(
        &self,
        tenant: TenantId,
        invitation_id: i64,
    ) -> Result<()> {
        self.delete(&format!(
            "/v1/metadata/tenants/{}/invitations/{invitation_id}",
            tenant.0
        ))
        .await
    }

    pub async fn accept_tenant_invitation(
        &self,
        request: AcceptTenantInvitationRequest,
    ) -> Result<TenantMembership> {
        self.post("/v1/auth/invitations/accept", &request).await
    }

    pub async fn remove_tenant_member(
        &self,
        tenant: TenantId,
        principal: sift_api_types::PrincipalId,
    ) -> Result<()> {
        self.delete(&format!(
            "/v1/metadata/tenants/{}/members/{}",
            tenant.0, principal.0
        ))
        .await
    }

    pub async fn principal_keys(&self) -> Result<Vec<PrincipalKey>> {
        self.get("/v1/auth/keys").await
    }

    pub async fn register_principal_key(
        &self,
        request: RegisterPrincipalKeyRequest,
    ) -> Result<PrincipalKey> {
        self.post("/v1/auth/keys", &request).await
    }

    pub async fn revoke_principal_key(&self, key_id: i64) -> Result<()> {
        self.delete(&format!("/v1/auth/keys/{key_id}")).await
    }

    pub async fn issue_key_challenge(
        &self,
        request: KeyChallengeRequest,
    ) -> Result<KeyChallengeResponse> {
        self.post("/v1/auth/keys/challenge", &request).await
    }

    pub async fn authenticate_key(
        &self,
        request: KeyAuthenticateRequest,
    ) -> Result<SessionTokenProvider> {
        let tokens: AuthTokensResponse = self.post("/v1/auth/keys/authenticate", &request).await?;
        Ok(SessionTokenProvider::new(tokens))
    }

    pub async fn exchange_ssh_proxy_capability(
        &self,
        capability: String,
    ) -> Result<SshProxyAccessGrant> {
        self.post(
            "/v1/auth/ssh-proxy/exchange",
            &SshProxyCapabilityExchangeRequest { capability },
        )
        .await
    }

    pub async fn health(&self) -> Result<Health> {
        self.get("/v1/health").await
    }

    pub async fn instance_configuration(&self) -> Result<InstanceConfigurationDocument> {
        self.get("/v1/admin/instance/configuration").await
    }

    pub async fn update_instance_configuration(
        &self,
        request: UpdateInstanceConfigurationRequest,
    ) -> Result<InstanceConfigurationDocument> {
        self.put("/v1/admin/instance/configuration", &request).await
    }

    pub async fn vcs_diagnostics(&self) -> Result<VcsAdapterDiagnostics> {
        self.get("/v1/admin/instance/vcs-diagnostics").await
    }

    /// Readiness probe. Returns the parsed [`Readiness`] on both `200` (ready)
    /// and `503` (not ready) — inspect [`Readiness::ready`] for the verdict.
    /// Other statuses (e.g. auth failure) surface as [`Error::Server`].
    pub async fn ready(&self) -> Result<Readiness> {
        let response = self
            .send_response(self.http.get(self.url("/v1/ready")))
            .await?;
        let status = response.status();
        if status == reqwest::StatusCode::OK || status == reqwest::StatusCode::SERVICE_UNAVAILABLE {
            Ok(response.json().await?)
        } else {
            Err(server_error(response).await)
        }
    }

    pub async fn open_session(&self, tag: Option<String>) -> Result<SessionInfo> {
        self.open_session_for_tenant(tag, None).await
    }

    pub async fn open_session_for_tenant(
        &self,
        tag: Option<String>,
        tenant_id: Option<i64>,
    ) -> Result<SessionInfo> {
        self.post("/v1/sessions", &OpenSessionRequest { tag, tenant_id })
            .await
    }

    pub async fn list_sessions(&self) -> Result<Vec<SessionInfo>> {
        self.get("/v1/sessions").await
    }

    pub async fn close_session(&self, session: SessionId) -> Result<()> {
        self.delete(&format!("/v1/sessions/{session}")).await
    }

    pub async fn open_connection(
        &self,
        session: SessionId,
        request: OpenConnectionRequest,
    ) -> Result<ConnectionInfo> {
        self.post(&format!("/v1/sessions/{session}/connections"), &request)
            .await
    }

    pub async fn ping_connection(
        &self,
        session: SessionId,
        connection: ConnectionId,
    ) -> Result<ServerInfo> {
        self.post_empty(&format!(
            "/v1/sessions/{session}/connections/{connection}/ping"
        ))
        .await
    }

    pub async fn list_processes(
        &self,
        session: SessionId,
        connection: ConnectionId,
    ) -> Result<Vec<DatabaseProcess>> {
        self.get(&format!(
            "/v1/sessions/{session}/connections/{connection}/processes"
        ))
        .await
    }

    pub async fn kill_process(
        &self,
        session: SessionId,
        connection: ConnectionId,
        process_id: i64,
    ) -> Result<KillProcessResponse> {
        self.post(
            &format!("/v1/sessions/{session}/connections/{connection}/processes/kill"),
            &KillProcessRequest { process_id },
        )
        .await
    }

    pub async fn schema(
        &self,
        session: SessionId,
        connection: ConnectionId,
    ) -> Result<SchemaSnapshot> {
        self.get(&format!(
            "/v1/sessions/{session}/connections/{connection}/schema"
        ))
        .await
    }

    pub async fn catalog_graph(
        &self,
        session: SessionId,
        connection: ConnectionId,
        request: sift_protocol::CatalogGraphRequest,
    ) -> Result<sift_protocol::CatalogGraph> {
        self.post(
            &format!("/v1/sessions/{session}/connections/{connection}/catalog/graph"),
            &request,
        )
        .await
    }

    pub async fn catalog_diagram(
        &self,
        session: SessionId,
        connection: ConnectionId,
        request: sift_protocol::CatalogDiagramRequest,
    ) -> Result<sift_protocol::CatalogDiagram> {
        self.post(
            &format!("/v1/sessions/{session}/connections/{connection}/catalog/diagram"),
            &request,
        )
        .await
    }

    pub async fn preview_catalog_diagram_mutation(
        &self,
        session: SessionId,
        connection: ConnectionId,
        request: sift_protocol::PreviewCatalogDiagramMutationRequest,
    ) -> Result<sift_protocol::MigrationPlan> {
        self.post(
            &format!(
                "/v1/sessions/{session}/connections/{connection}/catalog/diagram/mutations/preview"
            ),
            &request,
        )
        .await
    }

    pub async fn create_catalog_snapshot(
        &self,
        session: SessionId,
        connection: ConnectionId,
        request: sift_protocol::CreateCatalogSnapshotRequest,
    ) -> Result<sift_protocol::CatalogSnapshot> {
        self.post(
            &format!("/v1/sessions/{session}/connections/{connection}/catalog/snapshots"),
            &request,
        )
        .await
    }

    pub async fn compare_catalog_schemas(
        &self,
        session: SessionId,
        connection: ConnectionId,
        request: sift_protocol::SchemaDiffRequest,
    ) -> Result<sift_protocol::SchemaDiff> {
        self.post(
            &format!("/v1/sessions/{session}/connections/{connection}/catalog/diffs"),
            &request,
        )
        .await
    }

    pub async fn preview_migration(
        &self,
        session: SessionId,
        connection: ConnectionId,
        request: sift_protocol::PreviewMigrationRequest,
    ) -> Result<sift_protocol::MigrationPlan> {
        self.post(
            &format!("/v1/sessions/{session}/connections/{connection}/catalog/migrations/preview"),
            &request,
        )
        .await
    }

    pub async fn apply_migration(
        &self,
        session: SessionId,
        connection: ConnectionId,
        request: sift_protocol::ApplyMigrationRequest,
    ) -> Result<sift_protocol::MigrationRun> {
        self.post(
            &format!("/v1/sessions/{session}/connections/{connection}/catalog/migrations/apply"),
            &request,
        )
        .await
    }

    pub async fn validate_migration(
        &self,
        session: SessionId,
        connection: ConnectionId,
        request: sift_protocol::ValidateMigrationRequest,
    ) -> Result<sift_protocol::MigrationValidation> {
        self.post(
            &format!("/v1/sessions/{session}/connections/{connection}/catalog/migrations/validate"),
            &request,
        )
        .await
    }

    pub async fn migration_run(
        &self,
        session: SessionId,
        connection: ConnectionId,
        run: sift_protocol::MigrationRunId,
    ) -> Result<sift_protocol::MigrationRun> {
        self.get(&format!(
            "/v1/sessions/{session}/connections/{connection}/catalog/migrations/runs/{run}"
        ))
        .await
    }

    pub async fn durable_migration_run(
        &self,
        tenant: TenantId,
        run: sift_protocol::MigrationRunId,
    ) -> Result<sift_protocol::MigrationRun> {
        self.get(&format!(
            "/v1/metadata/tenants/{}/migration-runs/{run}",
            tenant.0
        ))
        .await
    }

    pub async fn cancel_migration(
        &self,
        session: SessionId,
        connection: ConnectionId,
        run: sift_protocol::MigrationRunId,
    ) -> Result<()> {
        self.post_empty_body(
            &format!(
                "/v1/sessions/{session}/connections/{connection}/catalog/migrations/runs/{run}/cancel"
            ),
            &serde_json::json!({}),
        )
        .await
    }

    pub async fn capture_semantic_plan(
        &self,
        session: SessionId,
        connection: ConnectionId,
        request: sift_protocol::CaptureSemanticPlanRequest,
    ) -> Result<sift_protocol::PlanCapture> {
        self.post(
            &format!("/v1/sessions/{session}/connections/{connection}/plan-captures"),
            &request,
        )
        .await
    }

    pub async fn start_comparison(
        &self,
        session: SessionId,
        request: sift_protocol::StartComparisonRequest,
    ) -> Result<sift_protocol::ComparisonSummary> {
        self.post(&format!("/v1/sessions/{session}/comparisons"), &request)
            .await
    }

    pub async fn comparison(
        &self,
        session: SessionId,
        comparison: sift_protocol::ComparisonId,
    ) -> Result<sift_protocol::ComparisonSummary> {
        self.get(&format!("/v1/sessions/{session}/comparisons/{comparison}"))
            .await
    }

    pub async fn comparison_page(
        &self,
        session: SessionId,
        comparison: sift_protocol::ComparisonId,
        request: sift_protocol::ComparisonPageRequest,
    ) -> Result<sift_protocol::ComparisonPage> {
        self.post(
            &format!("/v1/sessions/{session}/comparisons/{comparison}/pages"),
            &request,
        )
        .await
    }

    pub async fn cancel_comparison(
        &self,
        session: SessionId,
        comparison: sift_protocol::ComparisonId,
    ) -> Result<sift_protocol::CancelComparisonResponse> {
        self.post(
            &format!("/v1/sessions/{session}/comparisons/{comparison}/cancel"),
            &serde_json::json!({}),
        )
        .await
    }

    pub async fn prepare_comparison_patch(
        &self,
        session: SessionId,
        comparison: sift_protocol::ComparisonId,
        request: sift_protocol::PrepareComparisonPatchRequest,
    ) -> Result<sift_protocol::ComparisonPatchPreparation> {
        self.post(
            &format!("/v1/sessions/{session}/comparisons/{comparison}/patch"),
            &request,
        )
        .await
    }

    pub async fn plan_capture(
        &self,
        tenant: TenantId,
        capture: sift_protocol::PlanCaptureId,
    ) -> Result<sift_protocol::PlanCapture> {
        self.get(&format!(
            "/v1/metadata/tenants/{}/plan-captures/{capture}",
            tenant.0
        ))
        .await
    }

    pub async fn plan_captures(
        &self,
        tenant: TenantId,
        request: sift_protocol::ListPlanCapturesRequest,
    ) -> Result<CursorPage<sift_protocol::PlanCaptureSummary>> {
        let mut query = vec![format!("limit={}", request.limit.unwrap_or(50))];
        if let Some(source) = request.source_digest {
            query.push(format!("source_digest={}", urlencoding_replace(&source)));
        }
        if let Some(cursor) = request.cursor {
            query.push(format!("cursor={cursor}"));
        }
        self.get(&format!(
            "/v1/metadata/tenants/{}/plan-captures?{}",
            tenant.0,
            query.join("&")
        ))
        .await
    }

    pub async fn compare_plan_captures(
        &self,
        tenant: TenantId,
        request: sift_protocol::ComparePlanCapturesRequest,
    ) -> Result<sift_protocol::PlanCaptureComparison> {
        self.post(
            &format!("/v1/metadata/tenants/{}/plan-captures/compare", tenant.0),
            &request,
        )
        .await
    }

    pub async fn delete_plan_capture(
        &self,
        tenant: TenantId,
        capture: sift_protocol::PlanCaptureId,
        expected_revision: u64,
    ) -> Result<()> {
        self.delete(&format!(
            "/v1/metadata/tenants/{}/plan-captures/{capture}?expected_revision={expected_revision}",
            tenant.0
        ))
        .await
    }

    pub async fn catalog_snapshots(
        &self,
        tenant: TenantId,
        limit: u32,
    ) -> Result<Vec<sift_protocol::CatalogSnapshotSummary>> {
        self.get(&format!(
            "/v1/metadata/tenants/{}/catalog-snapshots?limit={limit}",
            tenant.0
        ))
        .await
    }

    pub async fn catalog_snapshot(
        &self,
        tenant: TenantId,
        snapshot: sift_protocol::CatalogSnapshotId,
    ) -> Result<sift_protocol::CatalogSnapshot> {
        self.get(&format!(
            "/v1/metadata/tenants/{}/catalog-snapshots/{snapshot}",
            tenant.0
        ))
        .await
    }

    pub async fn delete_catalog_snapshot(
        &self,
        tenant: TenantId,
        snapshot: sift_protocol::CatalogSnapshotId,
        expected_revision: u64,
    ) -> Result<()> {
        self.delete(&format!(
            "/v1/metadata/tenants/{}/catalog-snapshots/{snapshot}?expected_revision={expected_revision}",
            tenant.0
        ))
        .await
    }

    pub async fn format_semantic_document(
        &self,
        session: SessionId,
        connection: ConnectionId,
        document: sift_protocol::SemanticDocumentId,
        request: sift_protocol::FormatSqlRequest,
    ) -> Result<sift_protocol::WorkspaceEdit> {
        self.post(
            &format!(
                "/v1/sessions/{session}/connections/{connection}/semantic-documents/{document}/format"
            ),
            &request,
        )
        .await
    }

    pub async fn prepare_semantic_quick_fix(
        &self,
        session: SessionId,
        connection: ConnectionId,
        document: sift_protocol::SemanticDocumentId,
        fix_id: &str,
        request: sift_protocol::SqlQuickFixRequest,
    ) -> Result<sift_protocol::WorkspaceEdit> {
        self.post(
            &format!(
                "/v1/sessions/{session}/connections/{connection}/semantic-documents/{document}/quick-fixes/{fix_id}"
            ),
            &request,
        )
        .await
    }

    pub async fn find_semantic_usages(
        &self,
        session: SessionId,
        connection: ConnectionId,
        document: sift_protocol::SemanticDocumentId,
        request: sift_protocol::FindSqlUsagesRequest,
    ) -> Result<sift_protocol::SqlUsagePage> {
        self.post(
            &format!(
                "/v1/sessions/{session}/connections/{connection}/semantic-documents/{document}/usages"
            ),
            &request,
        )
        .await
    }

    pub async fn prepare_semantic_refactor(
        &self,
        session: SessionId,
        connection: ConnectionId,
        document: sift_protocol::SemanticDocumentId,
        request: sift_protocol::PrepareSqlRefactorRequest,
    ) -> Result<sift_protocol::WorkspaceEdit> {
        self.post(
            &format!(
                "/v1/sessions/{session}/connections/{connection}/semantic-documents/{document}/refactors/prepare"
            ),
            &request,
        )
        .await
    }

    /// Export a query result as CSV / TSV / JSON Lines / JSON Array.
    /// This convenience method buffers the complete body; use
    /// [`Client::stream_export_query`] for large exports.
    pub async fn export_query(
        &self,
        session: SessionId,
        connection: ConnectionId,
        request: sift_protocol::ExportRequest,
    ) -> Result<Vec<u8>> {
        let req = self
            .http
            .post(self.url(&format!(
                "/v1/sessions/{session}/connections/{connection}/export"
            )))
            .json(&request);
        let resp = self.send_response(req).await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(server_error(resp).await);
        }
        Ok(resp.bytes().await?.to_vec())
    }

    /// Start an export whose chunks are delivered incrementally. Dropping the
    /// stream closes the response body and lets the server cancel/release its
    /// cursor through the export drop guard.
    pub async fn stream_export_query(
        &self,
        session: SessionId,
        connection: ConnectionId,
        request: sift_protocol::ExportRequest,
    ) -> Result<ExportStream> {
        let req = self
            .http
            .post(self.url(&format!(
                "/v1/sessions/{session}/connections/{connection}/export"
            )))
            .json(&request);
        let response = self.send_response(req).await?;
        if !response.status().is_success() {
            return Err(server_error(response).await);
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let content_disposition = response
            .headers()
            .get(reqwest::header::CONTENT_DISPOSITION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        Ok(ExportStream {
            content_type,
            content_disposition,
            body: Box::pin(response.bytes_stream()),
        })
    }

    /// Generate DDL for a database object. `path.name` is required;
    /// `path.schema` and `path.kind` are optional (kind defaults to
    /// table server-side). The response includes the object's
    /// canonical `path` and a `ddl` string containing the CREATE
    /// statement(s); for tables, standalone CREATE INDEX statements
    /// for non-constraint indexes follow the CREATE TABLE.
    pub async fn object_ddl(
        &self,
        session: SessionId,
        connection: ConnectionId,
        path: &sift_protocol::ObjectPath,
    ) -> Result<sift_protocol::ObjectDdl> {
        let mut query = vec![format!("name={}", urlencoding_replace(&path.name))];
        if let Some(schema) = &path.schema {
            query.push(format!("schema={}", urlencoding_replace(schema)));
        }
        if let Some(kind) = &path.kind {
            let kind_str = serde_json::to_string(kind).map_err(Error::Json)?;
            // Strip the surrounding quotes serde_json emits for enums.
            let cleaned = kind_str.trim_matches('"');
            query.push(format!("kind={cleaned}"));
        }
        if let Some(args) = &path.routine_args {
            for arg in args {
                query.push(format!("routine_args={}", urlencoding_replace(arg)));
            }
        }
        self.get(&format!(
            "/v1/sessions/{session}/connections/{connection}/ddl?{}",
            query.join("&")
        ))
        .await
    }

    pub async fn complete(
        &self,
        session: SessionId,
        connection: ConnectionId,
        request: sift_protocol::completion::CompletionRequest,
    ) -> Result<sift_protocol::completion::CompletionResponse> {
        self.post(
            &format!("/v1/sessions/{session}/connections/{connection}/complete"),
            &request,
        )
        .await
    }

    pub async fn open_semantic_document(
        &self,
        session: SessionId,
        connection: ConnectionId,
        request: sift_protocol::CreateSemanticDocumentRequest,
    ) -> Result<sift_protocol::SemanticDocumentState> {
        self.post(
            &format!("/v1/sessions/{session}/connections/{connection}/semantic-documents"),
            &request,
        )
        .await
    }

    pub async fn update_semantic_document(
        &self,
        session: SessionId,
        connection: ConnectionId,
        document: sift_protocol::SemanticDocumentId,
        request: sift_protocol::UpdateSemanticDocumentRequest,
    ) -> Result<sift_protocol::SemanticDocumentState> {
        self.put(
            &format!(
                "/v1/sessions/{session}/connections/{connection}/semantic-documents/{document}"
            ),
            &request,
        )
        .await
    }

    pub async fn close_semantic_document(
        &self,
        session: SessionId,
        connection: ConnectionId,
        document: sift_protocol::SemanticDocumentId,
    ) -> Result<()> {
        self.delete_empty(&format!(
            "/v1/sessions/{session}/connections/{connection}/semantic-documents/{document}"
        ))
        .await
    }

    pub async fn select_semantic_statement(
        &self,
        session: SessionId,
        connection: ConnectionId,
        document: sift_protocol::SemanticDocumentId,
        request: sift_protocol::SelectStatementRequest,
    ) -> Result<sift_protocol::StatementSelection> {
        self.post(
            &format!(
                "/v1/sessions/{session}/connections/{connection}/semantic-documents/{document}/statements/select"
            ),
            &request,
        )
        .await
    }

    pub async fn semantic_diagnostics(
        &self,
        session: SessionId,
        connection: ConnectionId,
        document: sift_protocol::SemanticDocumentId,
        revision: u64,
    ) -> Result<sift_protocol::DiagnosticsResponse> {
        self.post(
            &format!(
                "/v1/sessions/{session}/connections/{connection}/semantic-documents/{document}/diagnostics"
            ),
            &sift_protocol::SemanticRevisionRequest {
                revision,
                catalog_revision: None,
            },
        )
        .await
    }

    pub async fn semantic_diagnostics_with_catalog(
        &self,
        session: SessionId,
        connection: ConnectionId,
        document: sift_protocol::SemanticDocumentId,
        revision: u64,
        catalog_revision: sift_protocol::CatalogRevision,
    ) -> Result<sift_protocol::DiagnosticsResponse> {
        self.post(
            &format!(
                "/v1/sessions/{session}/connections/{connection}/semantic-documents/{document}/diagnostics"
            ),
            &sift_protocol::SemanticRevisionRequest {
                revision,
                catalog_revision: Some(catalog_revision),
            },
        )
        .await
    }

    pub async fn complete_semantic_document(
        &self,
        session: SessionId,
        connection: ConnectionId,
        document: sift_protocol::SemanticDocumentId,
        request: sift_protocol::SemanticCompletionRequest,
    ) -> Result<sift_protocol::completion::CompletionResponse> {
        self.post(
            &format!(
                "/v1/sessions/{session}/connections/{connection}/semantic-documents/{document}/complete"
            ),
            &request,
        )
        .await
    }

    pub async fn hover_semantic_document(
        &self,
        session: SessionId,
        connection: ConnectionId,
        document: sift_protocol::SemanticDocumentId,
        request: sift_protocol::SemanticHoverRequest,
    ) -> Result<sift_protocol::SemanticHoverResponse> {
        self.post(
            &format!(
                "/v1/sessions/{session}/connections/{connection}/semantic-documents/{document}/hover"
            ),
            &request,
        )
        .await
    }

    pub async fn prepare_star_expansion(
        &self,
        session: SessionId,
        connection: ConnectionId,
        document: sift_protocol::SemanticDocumentId,
        request: sift_protocol::PrepareStarExpansionRequest,
    ) -> Result<sift_protocol::StarExpansionPreview> {
        self.post(
            &format!(
                "/v1/sessions/{session}/connections/{connection}/semantic-documents/{document}/star-expansions/prepare"
            ),
            &request,
        )
        .await
    }

    pub async fn preview_edits(
        &self,
        session: SessionId,
        connection: ConnectionId,
        request: PreviewEditsRequest,
    ) -> Result<EditPlan> {
        self.post(
            &format!("/v1/sessions/{session}/connections/{connection}/edits/preview"),
            &request,
        )
        .await
    }

    pub async fn apply_edits(
        &self,
        session: SessionId,
        connection: ConnectionId,
        request: ApplyEditsRequest,
    ) -> Result<ApplyEditsResult> {
        self.post(
            &format!("/v1/sessions/{session}/connections/{connection}/edits/apply"),
            &request,
        )
        .await
    }

    pub async fn search_schema(
        &self,
        session: SessionId,
        connection: ConnectionId,
        request: SchemaSearchRequest,
    ) -> Result<SchemaSearchResponse> {
        self.post(
            &format!("/v1/sessions/{session}/connections/{connection}/search/schema"),
            &request,
        )
        .await
    }

    pub async fn search_data(
        &self,
        session: SessionId,
        connection: ConnectionId,
        request: DataSearchRequest,
    ) -> Result<DataSearchResponse> {
        self.post(
            &format!("/v1/sessions/{session}/connections/{connection}/search/data"),
            &request,
        )
        .await
    }

    pub async fn explain(
        &self,
        session: SessionId,
        connection: ConnectionId,
        request: ExplainRequest,
    ) -> Result<ExplainResponse> {
        self.post(
            &format!("/v1/sessions/{session}/connections/{connection}/explain"),
            &request,
        )
        .await
    }

    pub async fn bulk_insert(
        &self,
        session: SessionId,
        connection: ConnectionId,
        request: BulkInsertRequest,
    ) -> Result<BulkInsertResponse> {
        self.post(
            &format!("/v1/sessions/{session}/connections/{connection}/bulk-insert"),
            &request,
        )
        .await
    }

    pub async fn import_csv(
        &self,
        session: SessionId,
        connection: ConnectionId,
        request: CsvImportRequest,
    ) -> Result<CsvImportResponse> {
        self.post(
            &format!("/v1/sessions/{session}/connections/{connection}/import/csv"),
            &request,
        )
        .await
    }

    pub async fn execute(
        &self,
        session: SessionId,
        connection: ConnectionId,
        sql: impl Into<String>,
    ) -> Result<ExecuteResponse> {
        self.post(
            &format!("/v1/sessions/{session}/queries"),
            &ExecuteRequestHttp {
                connection,
                sql: sql.into(),
                params: Vec::new(),
                tx: None,
                room_id: None,
                connection_profile_id: None,
                transform: None,
                source: None,
            },
        )
        .await
    }

    pub async fn execute_with_params(
        &self,
        session: SessionId,
        connection: ConnectionId,
        sql: impl Into<String>,
        params: Vec<Value>,
    ) -> Result<ExecuteResponse> {
        self.post(
            &format!("/v1/sessions/{session}/queries"),
            &ExecuteRequestHttp {
                connection,
                sql: sql.into(),
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

    pub async fn execute_in_tx(
        &self,
        session: SessionId,
        tx: &TransactionInfo,
        sql: impl Into<String>,
    ) -> Result<ExecuteResponse> {
        self.post(
            &format!("/v1/sessions/{session}/queries"),
            &ExecuteRequestHttp {
                connection: tx.connection,
                sql: sql.into(),
                params: Vec::new(),
                tx: Some(TxHandleRef {
                    tx_id: tx.tx_id,
                    connection: tx.connection,
                    mode: tx.mode,
                }),
                room_id: None,
                connection_profile_id: None,
                transform: None,
                source: None,
            },
        )
        .await
    }

    pub async fn begin_transaction(
        &self,
        session: SessionId,
        connection: ConnectionId,
        mode: TxMode,
    ) -> Result<TransactionInfo> {
        self.post(
            &format!("/v1/sessions/{session}/transactions"),
            &BeginTransactionRequest { connection, mode },
        )
        .await
    }

    pub async fn list_transactions(&self, session: SessionId) -> Result<Vec<TransactionState>> {
        self.get(&format!("/v1/sessions/{session}/transactions"))
            .await
    }

    pub async fn preview_transaction(
        &self,
        session: SessionId,
        connection: ConnectionId,
        tx_id: TxId,
        action: TransactionEndAction,
    ) -> Result<TransactionPreview> {
        self.post(
            &format!("/v1/sessions/{session}/transactions/{tx_id}/preview"),
            &TransactionPreviewRequest {
                connection,
                tx_id,
                action,
            },
        )
        .await
    }

    pub async fn commit_transaction(
        &self,
        session: SessionId,
        connection: ConnectionId,
        tx_id: TxId,
    ) -> Result<()> {
        self.post_empty_body(
            &format!("/v1/sessions/{session}/transactions/{tx_id}/commit"),
            &EndTransactionRequest { connection, tx_id },
        )
        .await
    }

    pub async fn rollback_transaction(
        &self,
        session: SessionId,
        connection: ConnectionId,
        tx_id: TxId,
    ) -> Result<()> {
        self.post_empty_body(
            &format!("/v1/sessions/{session}/transactions/{tx_id}/rollback"),
            &EndTransactionRequest { connection, tx_id },
        )
        .await
    }

    pub async fn cancel(
        &self,
        session: SessionId,
        connection: ConnectionId,
        cursor: CursorId,
    ) -> Result<()> {
        self.post_empty_body(
            &format!("/v1/sessions/{session}/queries/{cursor}/cancel"),
            &CancelRequest { connection, cursor },
        )
        .await
    }

    pub async fn close_connection(
        &self,
        session: SessionId,
        connection: ConnectionId,
    ) -> Result<()> {
        self.delete(&format!("/v1/sessions/{session}/connections/{connection}"))
            .await
    }

    pub async fn create_savepoint(
        &self,
        session: SessionId,
        connection: ConnectionId,
        tx_id: TxId,
        name: impl Into<String>,
    ) -> Result<()> {
        self.post_empty_body(
            &format!("/v1/sessions/{session}/transactions/{tx_id}/savepoints"),
            &SavepointRequest {
                connection,
                tx_id,
                name: name.into(),
            },
        )
        .await
    }

    pub async fn rollback_to_savepoint(
        &self,
        session: SessionId,
        connection: ConnectionId,
        tx_id: TxId,
        name: impl Into<String>,
    ) -> Result<()> {
        self.post_empty_body(
            &format!("/v1/sessions/{session}/transactions/{tx_id}/savepoints/rollback"),
            &SavepointRequest {
                connection,
                tx_id,
                name: name.into(),
            },
        )
        .await
    }

    pub async fn release_savepoint(
        &self,
        session: SessionId,
        connection: ConnectionId,
        tx_id: TxId,
        name: impl Into<String>,
    ) -> Result<()> {
        self.post_empty_body(
            &format!("/v1/sessions/{session}/transactions/{tx_id}/savepoints/release"),
            &SavepointRequest {
                connection,
                tx_id,
                name: name.into(),
            },
        )
        .await
    }

    pub async fn openapi(&self) -> Result<serde_json::Value> {
        self.get("/v1/openapi.json").await
    }

    /// Read the next batch of pages from an evicted cursor's spill
    /// file (ADR-011). The `resume_url` returned on
    /// `Page::Error { code: CursorEvicted }` points at this endpoint.
    /// The optional `from_seq` must equal the entry's current
    /// pages_read — the spill file is append-only forward-read.
    pub async fn read_spilled_pages(
        &self,
        cursor: CursorId,
        from_seq: Option<usize>,
        limit: Option<usize>,
    ) -> Result<serde_json::Value> {
        let mut query = Vec::new();
        if let Some(seq) = from_seq {
            query.push(format!("from_seq={seq}"));
        }
        if let Some(limit) = limit {
            query.push(format!("limit={limit}"));
        }
        let suffix = if query.is_empty() {
            String::new()
        } else {
            format!("?{}", query.join("&"))
        };
        self.get(&format!("/v1/cursors/{}/pages{suffix}", cursor.0))
            .await
    }

    /// Typed spill-resume page batch for cursor-stream consumers.
    pub async fn read_spilled_page_batch(
        &self,
        cursor: CursorId,
        from_seq: Option<usize>,
        limit: Option<usize>,
    ) -> Result<SpilledCursorPages> {
        let mut query = Vec::new();
        if let Some(seq) = from_seq {
            query.push(format!("from_seq={seq}"));
        }
        if let Some(limit) = limit {
            query.push(format!("limit={limit}"));
        }
        let suffix = if query.is_empty() {
            String::new()
        } else {
            format!("?{}", query.join("&"))
        };
        self.get(&format!("/v1/cursors/{}/pages{suffix}", cursor.0))
            .await
    }

    /// Delete a spilled cursor's file explicitly. Idempotent — returns
    /// ok even if the entry has already been reaped by TTL or fully
    /// drained.
    pub async fn delete_spilled_cursor(&self, cursor: CursorId) -> Result<()> {
        self.delete(&format!("/v1/cursors/{}", cursor.0)).await
    }

    pub async fn tenants(&self) -> Result<Vec<TenantMembership>> {
        self.get("/v1/metadata/tenants").await
    }

    pub async fn rooms(&self, tenant: TenantId) -> Result<Vec<Room>> {
        self.get(&format!("/v1/metadata/rooms?tenant={}", tenant.0))
            .await
    }

    pub async fn create_room(&self, request: CreateRoomRequest) -> Result<Room> {
        self.post("/v1/metadata/rooms", &request).await
    }

    pub async fn delete_room(&self, room: RoomId) -> Result<()> {
        self.delete(&format!("/v1/metadata/rooms/{}", room.0)).await
    }

    pub async fn bind_room_connection(
        &self,
        room: RoomId,
        connection_profile_id: i64,
    ) -> Result<Room> {
        self.put(
            &format!("/v1/metadata/rooms/{}/connection", room.0),
            &BindRoomConnectionRequest {
                connection_profile_id,
            },
        )
        .await
    }

    pub async fn unbind_room_connection(&self, room: RoomId) -> Result<()> {
        self.delete(&format!("/v1/metadata/rooms/{}/connection", room.0))
            .await
    }

    pub async fn room_members(&self, room: RoomId) -> Result<Vec<RoomMember>> {
        self.get(&format!("/v1/metadata/rooms/{}/members", room.0))
            .await
    }

    pub async fn room_results(&self, room: RoomId) -> Result<Vec<RoomQueryResult>> {
        self.get(&format!("/v1/metadata/rooms/{}/results", room.0))
            .await
    }

    pub async fn room_result(&self, room: RoomId, result: RoomResultId) -> Result<RoomQueryResult> {
        self.get(&format!("/v1/metadata/rooms/{}/results/{}", room.0, result))
            .await
    }

    pub async fn room_result_pages(
        &self,
        room: RoomId,
        result: RoomResultId,
        from_seq: u64,
        limit: usize,
    ) -> Result<RoomResultPages> {
        self.get(&format!(
            "/v1/metadata/rooms/{}/results/{}/pages?from_seq={from_seq}&limit={limit}",
            room.0, result
        ))
        .await
    }

    pub async fn add_room_member(
        &self,
        room: RoomId,
        request: AddRoomMemberRequest,
    ) -> Result<RoomMember> {
        self.post(&format!("/v1/metadata/rooms/{}/members", room.0), &request)
            .await
    }

    pub async fn remove_room_member(&self, room: RoomId, principal_id: i64) -> Result<()> {
        self.delete(&format!(
            "/v1/metadata/rooms/{}/members/{principal_id}",
            room.0
        ))
        .await
    }

    pub async fn join_room(&self, room: RoomId) -> Result<RoomMember> {
        self.post_empty(&format!("/v1/metadata/rooms/{}/join", room.0))
            .await
    }

    pub async fn leave_room(&self, room: RoomId) -> Result<()> {
        self.post_empty_body(
            &format!("/v1/metadata/rooms/{}/leave", room.0),
            &serde_json::json!({}),
        )
        .await
    }

    pub async fn documents(&self, room: RoomId) -> Result<Vec<Document>> {
        self.get(&format!("/v1/metadata/rooms/{}/documents", room.0))
            .await
    }

    pub async fn create_document(
        &self,
        room: RoomId,
        request: CreateDocumentRequest,
    ) -> Result<Document> {
        self.post(
            &format!("/v1/metadata/rooms/{}/documents", room.0),
            &request,
        )
        .await
    }

    pub async fn update_document_snapshot(
        &self,
        document: DocumentId,
        request: UpdateDocumentSnapshotRequest,
    ) -> Result<Document> {
        self.put(&format!("/v1/metadata/documents/{}", document.0), &request)
            .await
    }

    pub async fn delete_document(&self, document: DocumentId) -> Result<()> {
        self.delete(&format!("/v1/metadata/documents/{}", document.0))
            .await
    }

    pub async fn room_workspaces(&self, room: RoomId) -> Result<Vec<Workspace>> {
        self.get(&format!("/v1/metadata/rooms/{}/workspaces", room.0))
            .await
    }

    pub async fn create_workspace(
        &self,
        room: RoomId,
        request: CreateWorkspaceRequest,
    ) -> Result<Workspace> {
        self.post(
            &format!("/v1/metadata/rooms/{}/workspaces", room.0),
            &request,
        )
        .await
    }

    pub async fn workspace(&self, workspace: WorkspaceId) -> Result<Workspace> {
        self.get(&format!("/v1/metadata/workspaces/{}", workspace.0))
            .await
    }

    pub async fn update_workspace(
        &self,
        workspace: WorkspaceId,
        request: UpdateWorkspaceRequest,
    ) -> Result<Workspace> {
        self.put(
            &format!("/v1/metadata/workspaces/{}", workspace.0),
            &request,
        )
        .await
    }

    pub async fn delete_workspace(
        &self,
        workspace: WorkspaceId,
        request: ExpectedWorkspaceRevisionRequest,
    ) -> Result<()> {
        self.delete_body(
            &format!("/v1/metadata/workspaces/{}", workspace.0),
            &request,
        )
        .await
    }

    pub async fn workspace_nodes(&self, workspace: WorkspaceId) -> Result<WorkspaceTreeResponse> {
        self.get(&format!("/v1/metadata/workspaces/{}/nodes", workspace.0))
            .await
    }

    pub async fn create_workspace_node(
        &self,
        workspace: WorkspaceId,
        request: CreateWorkspaceNodeRequest,
    ) -> Result<WorkspaceTreeResponse> {
        self.post(
            &format!("/v1/metadata/workspaces/{}/nodes", workspace.0),
            &request,
        )
        .await
    }

    pub async fn move_workspace_node(
        &self,
        node: WorkspaceNodeId,
        request: MoveWorkspaceNodeRequest,
    ) -> Result<WorkspaceTreeResponse> {
        self.put(
            &format!("/v1/metadata/workspace-nodes/{}", node.0),
            &request,
        )
        .await
    }

    pub async fn mutate_workspace_batch(
        &self,
        workspace: WorkspaceId,
        request: WorkspaceBatchMutationRequest,
    ) -> Result<WorkspaceTreeResponse> {
        self.post(
            &format!("/v1/metadata/workspaces/{}/nodes/batch", workspace.0),
            &request,
        )
        .await
    }

    pub async fn delete_workspace_node(
        &self,
        node: WorkspaceNodeId,
        request: ExpectedWorkspaceRevisionRequest,
    ) -> Result<Workspace> {
        self.send(
            self.http
                .delete(self.url(&format!("/v1/metadata/workspace-nodes/{}", node.0)))
                .json(&request),
        )
        .await
    }

    pub async fn workspace_checkpoints(
        &self,
        workspace: WorkspaceId,
        before_id: Option<WorkspaceCheckpointId>,
        limit: u32,
    ) -> Result<Vec<WorkspaceCheckpoint>> {
        let before = before_id
            .map(|id| format!("&before_id={}", id.0))
            .unwrap_or_default();
        self.get(&format!(
            "/v1/metadata/workspaces/{}/checkpoints?limit={limit}{before}",
            workspace.0
        ))
        .await
    }

    pub async fn create_workspace_checkpoint(
        &self,
        workspace: WorkspaceId,
        request: CreateWorkspaceCheckpointRequest,
    ) -> Result<WorkspaceCheckpoint> {
        self.post(
            &format!("/v1/metadata/workspaces/{}/checkpoints", workspace.0),
            &request,
        )
        .await
    }

    pub async fn restore_workspace_checkpoint(
        &self,
        checkpoint: WorkspaceCheckpointId,
        request: RestoreWorkspaceCheckpointRequest,
    ) -> Result<WorkspaceTreeResponse> {
        self.post(
            &format!(
                "/v1/metadata/workspace-checkpoints/{}/restore",
                checkpoint.0
            ),
            &request,
        )
        .await
    }

    pub async fn workspace_projection(
        &self,
        workspace: WorkspaceId,
    ) -> Result<Option<ProjectionBinding>> {
        self.get(&format!(
            "/v1/metadata/workspaces/{}/projection",
            workspace.0
        ))
        .await
    }

    pub async fn bind_workspace_projection(
        &self,
        workspace: WorkspaceId,
        request: BindWorkspaceProjectionRequest,
    ) -> Result<ProjectionBinding> {
        self.post(
            &format!("/v1/metadata/workspaces/{}/projection", workspace.0),
            &request,
        )
        .await
    }

    pub async fn delete_workspace_projection(
        &self,
        binding: ProjectionBindingId,
        request: ExpectedProjectionRevisionRequest,
    ) -> Result<()> {
        self.delete_body(
            &format!("/v1/metadata/workspace-projections/{}", binding.0),
            &request,
        )
        .await
    }

    pub async fn plan_workspace_projection(
        &self,
        binding: ProjectionBindingId,
    ) -> Result<ReconcilePlan> {
        self.get(&format!(
            "/v1/metadata/workspace-projections/{}/reconcile",
            binding.0
        ))
        .await
    }

    pub async fn apply_workspace_projection(
        &self,
        binding: ProjectionBindingId,
        request: ApplyWorkspaceProjectionRequest,
    ) -> Result<ReconcilePlan> {
        self.post(
            &format!("/v1/metadata/workspace-projections/{}/reconcile", binding.0),
            &request,
        )
        .await
    }

    pub async fn workspace_repository(
        &self,
        workspace: WorkspaceId,
    ) -> Result<Option<RepositoryBinding>> {
        self.get(&format!(
            "/v1/metadata/workspaces/{}/repository",
            workspace.0
        ))
        .await
    }

    pub async fn bind_workspace_repository(
        &self,
        workspace: WorkspaceId,
        request: BindRepositoryRequest,
    ) -> Result<RepositoryBinding> {
        self.post(
            &format!("/v1/metadata/workspaces/{}/repository", workspace.0),
            &request,
        )
        .await
    }

    pub async fn clone_workspace_repository(
        &self,
        workspace: WorkspaceId,
        request: CloneWorkspaceRepositoryRequest,
    ) -> Result<RepositoryBinding> {
        self.post(
            &format!("/v1/metadata/workspaces/{}/repository/clone", workspace.0),
            &request,
        )
        .await
    }

    pub async fn delete_workspace_repository(
        &self,
        binding: RepositoryBindingId,
        request: ExpectedRepositoryRevisionRequest,
    ) -> Result<()> {
        self.delete_body(
            &format!("/v1/metadata/repositories/{}", binding.0),
            &request,
        )
        .await
    }

    pub async fn repository_status(&self, binding: RepositoryBindingId) -> Result<VcsStatus> {
        self.get(&format!("/v1/metadata/repositories/{}/status", binding.0))
            .await
    }

    pub async fn repository_diff(
        &self,
        binding: RepositoryBindingId,
        query: VcsDiffQuery,
    ) -> Result<VcsDiff> {
        let side = match query.side {
            VcsDiffSide::HeadToIndex => "head_to_index",
            VcsDiffSide::IndexToWorktree => "index_to_worktree",
            VcsDiffSide::HeadToWorktree => "head_to_worktree",
        };
        let mut path = format!("/v1/metadata/repositories/{}/diff?side={side}", binding.0);
        if let Some(filter) = query.path {
            path.push_str("&path=");
            path.push_str(&urlencoding_replace(&filter.0));
        }
        self.get(&path).await
    }

    pub async fn repository_branches(
        &self,
        binding: RepositoryBindingId,
    ) -> Result<Vec<VcsBranch>> {
        self.get(&format!("/v1/metadata/repositories/{}/branches", binding.0))
            .await
    }

    pub async fn create_repository_branch(
        &self,
        binding: RepositoryBindingId,
        request: VcsCreateBranchRequest,
    ) -> Result<RepositoryBinding> {
        self.post(
            &format!("/v1/metadata/repositories/{}/branches", binding.0),
            &request,
        )
        .await
    }

    pub async fn switch_repository_branch(
        &self,
        binding: RepositoryBindingId,
        request: VcsSwitchBranchRequest,
    ) -> Result<RepositoryBinding> {
        self.post(
            &format!("/v1/metadata/repositories/{}/branches/switch", binding.0),
            &request,
        )
        .await
    }

    pub async fn rename_repository_branch(
        &self,
        binding: RepositoryBindingId,
        request: VcsRenameBranchRequest,
    ) -> Result<RepositoryBinding> {
        self.post(
            &format!("/v1/metadata/repositories/{}/branches/rename", binding.0),
            &request,
        )
        .await
    }

    pub async fn delete_repository_branch(
        &self,
        binding: RepositoryBindingId,
        request: VcsDeleteBranchRequest,
    ) -> Result<RepositoryBinding> {
        self.post(
            &format!("/v1/metadata/repositories/{}/branches/delete", binding.0),
            &request,
        )
        .await
    }

    pub async fn set_repository_upstream(
        &self,
        binding: RepositoryBindingId,
        request: VcsSetUpstreamRequest,
    ) -> Result<RepositoryBinding> {
        self.post(
            &format!("/v1/metadata/repositories/{}/branches/upstream", binding.0),
            &request,
        )
        .await
    }

    pub async fn repository_history(
        &self,
        binding: RepositoryBindingId,
        query: VcsHistoryQuery,
    ) -> Result<VcsHistoryPage> {
        let mut path = format!(
            "/v1/metadata/repositories/{}/history?limit={}",
            binding.0, query.limit
        );
        if let Some(cursor) = query.cursor {
            path.push_str("&cursor=");
            path.push_str(&urlencoding_replace(&cursor));
        }
        if let Some(query) = query.query {
            path.push_str("&query=");
            path.push_str(&urlencoding_replace(&query));
        }
        self.get(&path).await
    }

    pub async fn repository_commit(
        &self,
        binding: RepositoryBindingId,
        oid: &str,
    ) -> Result<VcsCommitDetail> {
        self.get(&format!(
            "/v1/metadata/repositories/{}/history/{}",
            binding.0,
            urlencoding_replace(oid)
        ))
        .await
    }

    pub async fn compare_repository_commits(
        &self,
        binding: RepositoryBindingId,
        query: VcsCompareQuery,
    ) -> Result<VcsDiff> {
        self.get(&format!(
            "/v1/metadata/repositories/{}/history/compare?base={}&target={}",
            binding.0,
            urlencoding_replace(&query.base),
            urlencoding_replace(&query.target)
        ))
        .await
    }

    pub async fn repository_historical_file(
        &self,
        binding: RepositoryBindingId,
        oid: &str,
        path: WorkspacePath,
    ) -> Result<VcsHistoricalFile> {
        self.get(&format!(
            "/v1/metadata/repositories/{}/history/{}/file?path={}",
            binding.0,
            urlencoding_replace(oid),
            urlencoding_replace(&path.0)
        ))
        .await
    }

    pub async fn restore_repository_historical_file(
        &self,
        binding: RepositoryBindingId,
        request: VcsRestoreHistoricalFileRequest,
    ) -> Result<VcsWorktreeMutationResult> {
        self.post(
            &format!(
                "/v1/metadata/repositories/{}/history/restore-file",
                binding.0
            ),
            &request,
        )
        .await
    }

    pub async fn revert_repository_commit(
        &self,
        binding: RepositoryBindingId,
        request: VcsRevertCommitRequest,
    ) -> Result<VcsHeadMutationResult> {
        self.post(
            &format!("/v1/metadata/repositories/{}/history/revert", binding.0),
            &request,
        )
        .await
    }

    pub async fn repository_conflict(
        &self,
        binding: RepositoryBindingId,
        query: VcsConflictQuery,
    ) -> Result<VcsConflictFile> {
        self.get(&format!(
            "/v1/metadata/repositories/{}/conflicts?path={}",
            binding.0,
            urlencoding_replace(&query.path.0)
        ))
        .await
    }

    pub async fn begin_repository_conflict_resolution(
        &self,
        binding: RepositoryBindingId,
        request: VcsBeginConflictResolutionRequest,
    ) -> Result<WorkspaceCheckpoint> {
        self.post(
            &format!("/v1/metadata/repositories/{}/conflicts/begin", binding.0),
            &request,
        )
        .await
    }

    pub async fn resolve_repository_conflict(
        &self,
        binding: RepositoryBindingId,
        request: VcsResolveConflictRequest,
    ) -> Result<VcsWorktreeMutationResult> {
        self.post(
            &format!("/v1/metadata/repositories/{}/conflicts/resolve", binding.0),
            &request,
        )
        .await
    }

    pub async fn mark_repository_conflict_resolved(
        &self,
        binding: RepositoryBindingId,
        request: VcsMarkConflictResolvedRequest,
    ) -> Result<RepositoryBinding> {
        self.post(
            &format!(
                "/v1/metadata/repositories/{}/conflicts/mark-resolved",
                binding.0
            ),
            &request,
        )
        .await
    }

    pub async fn continue_repository_operation(
        &self,
        binding: RepositoryBindingId,
        request: VcsRepositoryOperationRequest,
    ) -> Result<RepositoryBinding> {
        self.post(
            &format!("/v1/metadata/repositories/{}/operation/continue", binding.0),
            &request,
        )
        .await
    }

    pub async fn abort_repository_operation(
        &self,
        binding: RepositoryBindingId,
        request: VcsRepositoryOperationRequest,
    ) -> Result<RepositoryBinding> {
        self.post(
            &format!("/v1/metadata/repositories/{}/operation/abort", binding.0),
            &request,
        )
        .await
    }

    pub async fn repair_repository_binding(
        &self,
        binding: RepositoryBindingId,
        request: ExpectedRepositoryRevisionRequest,
    ) -> Result<RepositoryBinding> {
        self.post(
            &format!("/v1/metadata/repositories/{}/repair", binding.0),
            &request,
        )
        .await
    }

    pub async fn stage_repository_paths(
        &self,
        binding: RepositoryBindingId,
        request: VcsPathsRequest,
    ) -> Result<RepositoryBinding> {
        self.post(
            &format!("/v1/metadata/repositories/{}/stage", binding.0),
            &request,
        )
        .await
    }

    pub async fn unstage_repository_paths(
        &self,
        binding: RepositoryBindingId,
        request: VcsPathsRequest,
    ) -> Result<RepositoryBinding> {
        self.post(
            &format!("/v1/metadata/repositories/{}/unstage", binding.0),
            &request,
        )
        .await
    }

    pub async fn stage_repository_hunk(
        &self,
        binding: RepositoryBindingId,
        request: VcsHunkRequest,
    ) -> Result<RepositoryBinding> {
        self.post(
            &format!("/v1/metadata/repositories/{}/stage-hunk", binding.0),
            &request,
        )
        .await
    }

    pub async fn unstage_repository_hunk(
        &self,
        binding: RepositoryBindingId,
        request: VcsHunkRequest,
    ) -> Result<RepositoryBinding> {
        self.post(
            &format!("/v1/metadata/repositories/{}/unstage-hunk", binding.0),
            &request,
        )
        .await
    }

    pub async fn discard_repository_path(
        &self,
        binding: RepositoryBindingId,
        request: VcsDiscardRequest,
    ) -> Result<VcsWorktreeMutationResult> {
        self.post(
            &format!("/v1/metadata/repositories/{}/discard", binding.0),
            &request,
        )
        .await
    }

    pub async fn revert_repository_hunk(
        &self,
        binding: RepositoryBindingId,
        request: VcsRevertHunkRequest,
    ) -> Result<VcsWorktreeMutationResult> {
        self.post(
            &format!("/v1/metadata/repositories/{}/revert-hunk", binding.0),
            &request,
        )
        .await
    }

    pub async fn commit_repository(
        &self,
        binding: RepositoryBindingId,
        request: VcsCommitRequest,
    ) -> Result<VcsCommitResult> {
        self.post(
            &format!("/v1/metadata/repositories/{}/commit", binding.0),
            &request,
        )
        .await
    }

    pub async fn amend_repository(
        &self,
        binding: RepositoryBindingId,
        request: VcsCommitRequest,
    ) -> Result<VcsCommitResult> {
        self.post(
            &format!("/v1/metadata/repositories/{}/amend", binding.0),
            &request,
        )
        .await
    }

    pub async fn uncommit_repository(
        &self,
        binding: RepositoryBindingId,
        request: VcsUncommitRequest,
    ) -> Result<VcsHeadMutationResult> {
        self.post(
            &format!("/v1/metadata/repositories/{}/uncommit", binding.0),
            &request,
        )
        .await
    }

    pub async fn set_repository_credential(
        &self,
        binding: RepositoryBindingId,
        request: SetVcsCredentialRequest,
    ) -> Result<RepositoryBinding> {
        self.post(
            &format!("/v1/metadata/repositories/{}/credential", binding.0),
            &request,
        )
        .await
    }

    pub async fn delete_repository_credential(
        &self,
        binding: RepositoryBindingId,
        request: ExpectedRepositoryRevisionRequest,
    ) -> Result<RepositoryBinding> {
        self.send(
            self.http
                .delete(self.url(&format!(
                    "/v1/metadata/repositories/{}/credential",
                    binding.0
                )))
                .json(&request),
        )
        .await
    }

    pub async fn test_repository_credential(
        &self,
        binding: RepositoryBindingId,
        request: VcsCredentialTestRequest,
    ) -> Result<()> {
        self.post_empty_body(
            &format!("/v1/metadata/repositories/{}/credential/test", binding.0),
            &request,
        )
        .await
    }

    pub async fn repository_remotes(&self, binding: RepositoryBindingId) -> Result<Vec<VcsRemote>> {
        self.get(&format!("/v1/metadata/repositories/{}/remotes", binding.0))
            .await
    }

    pub async fn add_repository_remote(
        &self,
        binding: RepositoryBindingId,
        request: VcsRemoteMutationRequest,
    ) -> Result<RepositoryBinding> {
        self.post(
            &format!("/v1/metadata/repositories/{}/remotes", binding.0),
            &request,
        )
        .await
    }

    pub async fn update_repository_remote(
        &self,
        binding: RepositoryBindingId,
        request: VcsRemoteMutationRequest,
    ) -> Result<RepositoryBinding> {
        self.post(
            &format!("/v1/metadata/repositories/{}/remotes/update", binding.0),
            &request,
        )
        .await
    }

    pub async fn rename_repository_remote(
        &self,
        binding: RepositoryBindingId,
        request: VcsRemoteRenameRequest,
    ) -> Result<RepositoryBinding> {
        self.post(
            &format!("/v1/metadata/repositories/{}/remotes/rename", binding.0),
            &request,
        )
        .await
    }

    pub async fn remove_repository_remote(
        &self,
        binding: RepositoryBindingId,
        request: VcsRemoteDeleteRequest,
    ) -> Result<RepositoryBinding> {
        self.post(
            &format!("/v1/metadata/repositories/{}/remotes/remove", binding.0),
            &request,
        )
        .await
    }

    pub async fn fetch_repository(
        &self,
        binding: RepositoryBindingId,
        request: VcsRemoteRequest,
    ) -> Result<VcsRemoteResult> {
        self.post(
            &format!("/v1/metadata/repositories/{}/fetch", binding.0),
            &request,
        )
        .await
    }

    pub async fn push_repository(
        &self,
        binding: RepositoryBindingId,
        request: VcsRemoteRequest,
    ) -> Result<VcsRemoteResult> {
        self.post(
            &format!("/v1/metadata/repositories/{}/push", binding.0),
            &request,
        )
        .await
    }

    pub async fn repository_hosting(
        &self,
        binding: RepositoryBindingId,
        remote: Option<&str>,
        path: Option<&WorkspacePath>,
    ) -> Result<HostingRepositorySummary> {
        let mut url = reqwest::Url::parse(
            &self.url(&format!("/v1/metadata/repositories/{}/hosting", binding.0)),
        )
        .map_err(|_| Error::Protocol("invalid hosting endpoint URL".into()))?;
        {
            let mut query = url.query_pairs_mut();
            if let Some(remote) = remote {
                query.append_pair("remote", remote);
            }
            if let Some(path) = path {
                query.append_pair("path", &path.0);
            }
        }
        self.send(self.http.get(url)).await
    }

    pub async fn hosting_repositories(
        &self,
        binding: RepositoryBindingId,
        remote: Option<&str>,
    ) -> Result<Vec<HostingRepositoryCandidate>> {
        let mut url = reqwest::Url::parse(&self.url(&format!(
            "/v1/metadata/repositories/{}/hosting/repositories",
            binding.0
        )))
        .map_err(|_| Error::Protocol("invalid hosting endpoint URL".into()))?;
        if let Some(remote) = remote {
            url.query_pairs_mut().append_pair("remote", remote);
        }
        self.send(self.http.get(url)).await
    }

    pub async fn set_hosting_credential(
        &self,
        binding: RepositoryBindingId,
        request: sift_protocol::SetHostingCredentialRequest,
    ) -> Result<RepositoryBinding> {
        self.post(
            &format!("/v1/metadata/repositories/{}/hosting/credential", binding.0),
            &request,
        )
        .await
    }

    pub async fn delete_hosting_credential(
        &self,
        binding: RepositoryBindingId,
        request: ExpectedRepositoryRevisionRequest,
    ) -> Result<RepositoryBinding> {
        self.send(
            self.http
                .delete(self.url(&format!(
                    "/v1/metadata/repositories/{}/hosting/credential",
                    binding.0
                )))
                .json(&request),
        )
        .await
    }

    pub async fn create_hosting_pull_request(
        &self,
        binding: RepositoryBindingId,
        request: sift_protocol::CreateHostingPullRequestRequest,
    ) -> Result<sift_protocol::HostingPullRequest> {
        self.post(
            &format!(
                "/v1/metadata/repositories/{}/hosting/pull-requests",
                binding.0
            ),
            &request,
        )
        .await
    }

    pub async fn run_configurations(
        &self,
        workspace: WorkspaceId,
    ) -> Result<Vec<RunConfiguration>> {
        self.get(&format!(
            "/v1/metadata/workspaces/{}/run-configurations",
            workspace.0
        ))
        .await
    }

    pub async fn latest_successful_run_for_commit(
        &self,
        workspace: WorkspaceId,
        git_commit: &str,
    ) -> Result<Option<Run>> {
        self.get(&format!(
            "/v1/metadata/workspaces/{}/runs/latest-success?git_commit={}",
            workspace.0,
            urlencoding_replace(git_commit)
        ))
        .await
    }

    pub async fn create_run_configuration(
        &self,
        workspace: WorkspaceId,
        request: CreateRunConfigurationRequest,
    ) -> Result<RunConfiguration> {
        self.post(
            &format!("/v1/metadata/workspaces/{}/run-configurations", workspace.0),
            &request,
        )
        .await
    }

    pub async fn run_configuration(
        &self,
        configuration: RunConfigurationId,
    ) -> Result<RunConfiguration> {
        self.get(&format!(
            "/v1/metadata/run-configurations/{}",
            configuration.0
        ))
        .await
    }

    pub async fn update_run_configuration(
        &self,
        configuration: RunConfigurationId,
        request: UpdateRunConfigurationRequest,
    ) -> Result<RunConfiguration> {
        self.put(
            &format!("/v1/metadata/run-configurations/{}", configuration.0),
            &request,
        )
        .await
    }

    pub async fn delete_run_configuration(
        &self,
        configuration: RunConfigurationId,
        request: ExpectedRunConfigurationRevisionRequest,
    ) -> Result<()> {
        self.delete_body(
            &format!("/v1/metadata/run-configurations/{}", configuration.0),
            &request,
        )
        .await
    }

    pub async fn validate_run_configuration(
        &self,
        configuration: RunConfigurationId,
    ) -> Result<RunManifest> {
        self.post_empty(&format!(
            "/v1/metadata/run-configurations/{}/validate",
            configuration.0
        ))
        .await
    }

    pub async fn start_run(
        &self,
        configuration: RunConfigurationId,
        request: StartRunRequest,
    ) -> Result<Run> {
        self.post(
            &format!("/v1/metadata/run-configurations/{}/runs", configuration.0),
            &request,
        )
        .await
    }

    pub async fn run(&self, run: RunId) -> Result<Run> {
        self.get(&format!("/v1/metadata/runs/{}", run.0)).await
    }

    pub async fn run_steps(&self, run: RunId) -> Result<Vec<RunStepResult>> {
        self.get(&format!("/v1/metadata/runs/{}/steps", run.0))
            .await
    }

    pub async fn run_logs(&self, run: RunId, query: RunLogQuery) -> Result<Vec<RunLogEntry>> {
        self.get(&format!(
            "/v1/metadata/runs/{}/logs?after={}&limit={}",
            run.0, query.after, query.limit
        ))
        .await
    }

    pub async fn cancel_run(&self, run: RunId) -> Result<Run> {
        self.post_empty(&format!("/v1/metadata/runs/{}/cancel", run.0))
            .await
    }

    pub async fn rerun(&self, run: RunId, request: StartRunRequest) -> Result<Run> {
        self.post(&format!("/v1/metadata/runs/{}/rerun", run.0), &request)
            .await
    }

    pub async fn run_schedules(
        &self,
        configuration: RunConfigurationId,
    ) -> Result<Vec<RunSchedule>> {
        self.get(&format!(
            "/v1/metadata/run-configurations/{}/schedules",
            configuration.0
        ))
        .await
    }

    pub async fn create_run_schedule(
        &self,
        configuration: RunConfigurationId,
        request: CreateRunScheduleRequest,
    ) -> Result<RunSchedule> {
        self.post(
            &format!(
                "/v1/metadata/run-configurations/{}/schedules",
                configuration.0
            ),
            &request,
        )
        .await
    }

    pub async fn run_schedule(&self, schedule: ScheduleId) -> Result<RunSchedule> {
        self.get(&format!("/v1/metadata/schedules/{}", schedule.0))
            .await
    }

    pub async fn update_run_schedule(
        &self,
        schedule: ScheduleId,
        request: UpdateRunScheduleRequest,
    ) -> Result<RunSchedule> {
        self.put(&format!("/v1/metadata/schedules/{}", schedule.0), &request)
            .await
    }

    pub async fn delete_run_schedule(
        &self,
        schedule: ScheduleId,
        request: ExpectedRunConfigurationRevisionRequest,
    ) -> Result<()> {
        self.delete_body(&format!("/v1/metadata/schedules/{}", schedule.0), &request)
            .await
    }

    pub async fn enable_run_schedule(
        &self,
        schedule: ScheduleId,
        request: ExpectedRunConfigurationRevisionRequest,
    ) -> Result<RunSchedule> {
        self.post(
            &format!("/v1/metadata/schedules/{}/enable", schedule.0),
            &request,
        )
        .await
    }

    pub async fn disable_run_schedule(
        &self,
        schedule: ScheduleId,
        request: ExpectedRunConfigurationRevisionRequest,
    ) -> Result<RunSchedule> {
        self.post(
            &format!("/v1/metadata/schedules/{}/disable", schedule.0),
            &request,
        )
        .await
    }

    pub async fn schedule_occurrences(
        &self,
        schedule: ScheduleId,
        query: ScheduleOccurrenceQuery,
    ) -> Result<Vec<ScheduleOccurrence>> {
        self.get(&format!(
            "/v1/metadata/schedules/{}/occurrences?limit={}",
            schedule.0, query.limit
        ))
        .await
    }

    pub async fn resume_schedule_occurrence(
        &self,
        occurrence: ScheduleOccurrenceId,
    ) -> Result<ScheduleOccurrence> {
        self.post_empty(&format!(
            "/v1/metadata/schedule-occurrences/{}/resume",
            occurrence.0
        ))
        .await
    }

    pub async fn transfer_recipes(&self, workspace: WorkspaceId) -> Result<Vec<TransferRecipe>> {
        self.get(&format!(
            "/v1/metadata/workspaces/{}/transfer-recipes",
            workspace.0
        ))
        .await
    }

    pub async fn create_transfer_recipe(
        &self,
        workspace: WorkspaceId,
        request: CreateTransferRecipeRequest,
    ) -> Result<TransferRecipe> {
        self.post(
            &format!("/v1/metadata/workspaces/{}/transfer-recipes", workspace.0),
            &request,
        )
        .await
    }

    pub async fn transfer_recipe(&self, recipe: TransferRecipeId) -> Result<TransferRecipe> {
        self.get(&format!("/v1/metadata/transfer-recipes/{}", recipe.0))
            .await
    }

    pub async fn update_transfer_recipe(
        &self,
        recipe: TransferRecipeId,
        request: UpdateTransferRecipeRequest,
    ) -> Result<TransferRecipe> {
        self.put(
            &format!("/v1/metadata/transfer-recipes/{}", recipe.0),
            &request,
        )
        .await
    }

    pub async fn delete_transfer_recipe(
        &self,
        recipe: TransferRecipeId,
        request: ExpectedTransferRecipeRevisionRequest,
    ) -> Result<()> {
        self.delete_body(
            &format!("/v1/metadata/transfer-recipes/{}", recipe.0),
            &request,
        )
        .await
    }

    pub async fn validate_transfer_recipe(
        &self,
        recipe: TransferRecipeId,
    ) -> Result<TransferRecipe> {
        self.post_empty(&format!(
            "/v1/metadata/transfer-recipes/{}/validate",
            recipe.0
        ))
        .await
    }

    pub async fn execute_transfer_recipe(
        &self,
        recipe: TransferRecipeId,
        request: ExecuteTransferRecipeRequest,
    ) -> Result<TransferExecutionResult> {
        self.post(
            &format!("/v1/metadata/transfer-recipes/{}/execute", recipe.0),
            &request,
        )
        .await
    }

    pub async fn workspace_artifact(&self, artifact: WorkspaceArtifactId) -> Result<Vec<u8>> {
        use futures::StreamExt as _;

        let mut stream = self.stream_workspace_artifact(artifact).await?;
        let mut content = Vec::new();
        while let Some(chunk) = stream.next().await {
            content.extend_from_slice(&chunk?);
        }
        Ok(content)
    }

    /// Consume a staged transfer artifact incrementally with HTTP
    /// backpressure. The artifact is immutable for the lifetime of the stream.
    pub async fn stream_workspace_artifact(
        &self,
        artifact: WorkspaceArtifactId,
    ) -> Result<ExportStream> {
        let response = self
            .send_response(
                self.http
                    .get(self.url(&format!("/v1/metadata/artifacts/{}", artifact.0))),
            )
            .await?;
        if !response.status().is_success() {
            return Err(server_error(response).await);
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        Ok(ExportStream {
            content_type,
            content_disposition: None,
            body: Box::pin(response.bytes_stream()),
        })
    }

    pub async fn ddl_sources(&self, workspace: WorkspaceId) -> Result<Vec<DdlSource>> {
        self.get(&format!(
            "/v1/metadata/workspaces/{}/ddl-sources",
            workspace.0
        ))
        .await
    }

    pub async fn create_ddl_source(
        &self,
        workspace: WorkspaceId,
        request: CreateDdlSourceRequest,
    ) -> Result<DdlSource> {
        self.post(
            &format!("/v1/metadata/workspaces/{}/ddl-sources", workspace.0),
            &request,
        )
        .await
    }

    pub async fn ddl_source(&self, source: DdlSourceId) -> Result<DdlSourceModel> {
        self.get(&format!("/v1/metadata/ddl-sources/{}", source.0))
            .await
    }

    pub async fn update_ddl_source(
        &self,
        source: DdlSourceId,
        request: UpdateDdlSourceRequest,
    ) -> Result<DdlSource> {
        self.put(&format!("/v1/metadata/ddl-sources/{}", source.0), &request)
            .await
    }

    pub async fn delete_ddl_source(
        &self,
        source: DdlSourceId,
        request: ExpectedDdlSourceRevisionRequest,
    ) -> Result<()> {
        self.delete_body(&format!("/v1/metadata/ddl-sources/{}", source.0), &request)
            .await
    }

    pub async fn refresh_ddl_source(
        &self,
        source: DdlSourceId,
        request: ExpectedDdlSourceRevisionRequest,
    ) -> Result<DdlSourceModel> {
        self.post(
            &format!("/v1/metadata/ddl-sources/{}/refresh", source.0),
            &request,
        )
        .await
    }

    pub async fn connection_profiles(&self, tenant: TenantId) -> Result<Vec<ConnectionProfile>> {
        self.get(&format!("/v1/metadata/connections?tenant={}", tenant.0))
            .await
    }

    pub async fn upsert_connection_profile(
        &self,
        request: UpsertConnectionProfileRequest,
    ) -> Result<ConnectionProfile> {
        self.post("/v1/metadata/connections", &request).await
    }

    pub async fn delete_connection_profile(
        &self,
        tenant: TenantId,
        profile: ConnectionProfileId,
    ) -> Result<()> {
        self.delete(&format!(
            "/v1/metadata/connections/{}?tenant={}",
            profile.0, tenant.0
        ))
        .await
    }

    pub async fn set_connection_credential(
        &self,
        profile: ConnectionProfileId,
        request: SetCredentialRequest,
    ) -> Result<()> {
        self.post_empty_body(
            &format!("/v1/metadata/connections/{}/credential", profile.0),
            &request,
        )
        .await
    }

    pub async fn connection_policy(
        &self,
        profile: ConnectionProfileId,
    ) -> Result<ConnectionPolicy> {
        self.get(&format!("/v1/metadata/connections/{}/policy", profile.0))
            .await
    }

    pub async fn update_connection_policy(
        &self,
        profile: ConnectionProfileId,
        request: UpdateConnectionPolicyRequest,
    ) -> Result<ConnectionPolicy> {
        self.put(
            &format!("/v1/metadata/connections/{}/policy", profile.0),
            &request,
        )
        .await
    }

    pub async fn disconnect_connection_profile(
        &self,
        profile: ConnectionProfileId,
    ) -> Result<DisconnectManagedConnectionsResponse> {
        self.post_empty(&format!(
            "/v1/metadata/connections/{}/disconnect",
            profile.0
        ))
        .await
    }

    pub async fn tenant_usage(&self, tenant: TenantId) -> Result<TenantUsageSnapshot> {
        self.get(&format!("/v1/metadata/tenants/{}/usage", tenant.0))
            .await
    }

    pub async fn set_tenant_limits(
        &self,
        tenant: TenantId,
        limits: TenantResourceLimits,
    ) -> Result<TenantLimitOverride> {
        self.put(
            &format!("/v1/admin/tenants/{}/limits", tenant.0),
            &UpdateTenantLimitsRequest { limits },
        )
        .await
    }

    pub async fn clear_tenant_limits(&self, tenant: TenantId) -> Result<()> {
        self.delete(&format!("/v1/admin/tenants/{}/limits", tenant.0))
            .await
    }

    pub async fn open_connection_from_profile(
        &self,
        session: SessionId,
        request: OpenConnectionFromProfileRequest,
    ) -> Result<ConnectionInfo> {
        self.post(
            &format!("/v1/sessions/{session}/connections/from-profile"),
            &request,
        )
        .await
    }

    pub async fn history(
        &self,
        room: Option<RoomId>,
        limit: Option<u32>,
    ) -> Result<Vec<QueryHistory>> {
        let mut query = Vec::new();
        if let Some(room) = room {
            query.push(format!("room={}", room.0));
        }
        if let Some(limit) = limit {
            query.push(format!("limit={limit}"));
        }
        let suffix = if query.is_empty() {
            String::new()
        } else {
            format!("?{}", query.join("&"))
        };
        self.get(&format!("/v1/metadata/history{suffix}")).await
    }

    pub async fn history_page(
        &self,
        room: Option<RoomId>,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<CursorPage<QueryHistory>> {
        let mut query = Vec::new();
        if let Some(room) = room {
            query.push(format!("room={}", room.0));
        }
        if let Some(cursor) = cursor {
            query.push(format!("cursor={cursor}"));
        }
        if let Some(limit) = limit {
            query.push(format!("limit={limit}"));
        }
        let suffix = if query.is_empty() {
            String::new()
        } else {
            format!("?{}", query.join("&"))
        };
        self.get(&format!("/v1/metadata/history/pages{suffix}"))
            .await
    }

    /// List saved queries visible to the caller. `q` is a full-text
    /// search over name + sql_text; `tags` restrict to entries
    /// containing all listed tags; `scope` narrows to personal-only
    /// or shared-only.
    pub async fn saved_queries(
        &self,
        tenant: TenantId,
        q: Option<&str>,
        tags: &[String],
        scope: Option<SavedQueryScope>,
    ) -> Result<Vec<SavedQuery>> {
        let mut query = vec![format!("tenant={}", tenant.0)];
        if let Some(s) = q {
            query.push(format!("q={}", urlencoding_replace(s)));
        }
        if !tags.is_empty() {
            let joined: Vec<String> = tags.iter().map(|t| urlencoding_replace(t)).collect();
            query.push(format!("tags={}", joined.join(",")));
        }
        if let Some(scope) = scope {
            let s = match scope {
                SavedQueryScope::Personal => "personal",
                SavedQueryScope::Shared => "shared",
                SavedQueryScope::All => "all",
            };
            query.push(format!("scope={s}"));
        }
        self.get(&format!("/v1/metadata/saved-queries?{}", query.join("&")))
            .await
    }

    pub async fn saved_query(&self, id: SavedQueryId) -> Result<SavedQuery> {
        self.get(&format!("/v1/metadata/saved-queries/{}", id.0))
            .await
    }

    pub async fn create_saved_query(&self, request: CreateSavedQueryRequest) -> Result<SavedQuery> {
        self.post("/v1/metadata/saved-queries", &request).await
    }

    pub async fn update_saved_query(
        &self,
        id: SavedQueryId,
        request: UpdateSavedQueryRequest,
    ) -> Result<SavedQuery> {
        self.put(&format!("/v1/metadata/saved-queries/{}", id.0), &request)
            .await
    }

    pub async fn delete_saved_query(&self, id: SavedQueryId, expected_revision: u64) -> Result<()> {
        self.delete(&format!(
            "/v1/metadata/saved-queries/{}?expected_revision={expected_revision}",
            id.0
        ))
        .await
    }

    pub async fn vaults(&self, tenant: TenantId) -> Result<Vec<Vault>> {
        self.get(&format!("/v1/metadata/vaults?tenant={}", tenant.0))
            .await
    }

    pub async fn create_vault(&self, request: CreateVaultRequest) -> Result<Vault> {
        self.post("/v1/metadata/vaults", &request).await
    }

    pub async fn vault(&self, vault: VaultId) -> Result<Vault> {
        self.get(&format!("/v1/metadata/vaults/{}", vault.0)).await
    }

    pub async fn update_vault(
        &self,
        vault: VaultId,
        request: sift_api_types::UpdateVaultRequest,
    ) -> Result<Vault> {
        self.put(&format!("/v1/metadata/vaults/{}", vault.0), &request)
            .await
    }

    pub async fn delete_vault(&self, vault: VaultId, expected_revision: u64) -> Result<()> {
        self.delete(&format!(
            "/v1/metadata/vaults/{}?expected_revision={expected_revision}",
            vault.0
        ))
        .await
    }

    pub async fn vault_items(&self, vault: VaultId) -> Result<Vec<VaultItem>> {
        self.get(&format!("/v1/metadata/vaults/{}/items", vault.0))
            .await
    }

    pub async fn create_vault_item(
        &self,
        vault: VaultId,
        request: CreateVaultItemRequest,
    ) -> Result<VaultItem> {
        self.post(&format!("/v1/metadata/vaults/{}/items", vault.0), &request)
            .await
    }

    pub async fn vault_item(&self, item: VaultItemId) -> Result<VaultItem> {
        self.get(&format!("/v1/metadata/vault-items/{}", item.0))
            .await
    }

    pub async fn update_vault_item(
        &self,
        item: VaultItemId,
        request: sift_api_types::UpdateVaultItemRequest,
    ) -> Result<VaultItem> {
        self.put(&format!("/v1/metadata/vault-items/{}", item.0), &request)
            .await
    }

    pub async fn delete_vault_item(&self, item: VaultItemId, expected_revision: u64) -> Result<()> {
        self.delete(&format!(
            "/v1/metadata/vault-items/{}?expected_revision={expected_revision}",
            item.0
        ))
        .await
    }

    pub async fn set_vault_item_secret(
        &self,
        item: VaultItemId,
        request: sift_api_types::SetVaultSecretRequest,
    ) -> Result<VaultItem> {
        self.post(
            &format!("/v1/metadata/vault-items/{}/secret", item.0),
            &request,
        )
        .await
    }

    pub async fn clear_vault_item_secret(
        &self,
        item: VaultItemId,
        expected_revision: u64,
    ) -> Result<VaultItem> {
        self.delete_response(&format!(
            "/v1/metadata/vault-items/{}/secret?expected_revision={expected_revision}",
            item.0
        ))
        .await
    }

    pub async fn vault_grants(&self, vault: VaultId) -> Result<Vec<VaultGrant>> {
        self.get(&format!("/v1/metadata/vaults/{}/grants", vault.0))
            .await
    }

    pub async fn set_vault_grant(
        &self,
        vault: VaultId,
        principal: sift_api_types::PrincipalId,
        request: SetVaultGrantRequest,
    ) -> Result<VaultGrant> {
        self.put(
            &format!("/v1/metadata/vaults/{}/grants/{}", vault.0, principal.0),
            &request,
        )
        .await
    }

    pub async fn delete_vault_grant(
        &self,
        vault: VaultId,
        principal: sift_api_types::PrincipalId,
        expected_revision: u64,
    ) -> Result<()> {
        self.delete(&format!(
            "/v1/metadata/vaults/{}/grants/{}?expected_revision={expected_revision}",
            vault.0, principal.0
        ))
        .await
    }

    pub async fn vault_item_versions(&self, item: VaultItemId) -> Result<Vec<VaultItemVersion>> {
        self.get(&format!("/v1/metadata/vault-items/{}/versions", item.0))
            .await
    }

    pub async fn vault_item_version(
        &self,
        item: VaultItemId,
        version: u64,
    ) -> Result<VaultItemVersion> {
        self.get(&format!(
            "/v1/metadata/vault-items/{}/versions/{version}",
            item.0
        ))
        .await
    }

    pub async fn diff_vault_item_versions(
        &self,
        item: VaultItemId,
        from: u64,
        to: u64,
    ) -> Result<sift_api_types::VaultItemVersionDiff> {
        self.get(&format!(
            "/v1/metadata/vault-items/{}/diff?from={from}&to={to}",
            item.0
        ))
        .await
    }

    pub async fn restore_vault_item(
        &self,
        item: VaultItemId,
        request: sift_api_types::RestoreVaultItemRequest,
    ) -> Result<VaultItem> {
        self.post(
            &format!("/v1/metadata/vault-items/{}/restore", item.0),
            &request,
        )
        .await
    }

    pub async fn test_vault_item(&self, item: VaultItemId) -> Result<()> {
        let _: serde_json::Value = self
            .post_empty(&format!("/v1/metadata/vault-items/{}/test", item.0))
            .await?;
        Ok(())
    }

    pub async fn step_up_vault_reveal(
        &self,
        item: VaultItemId,
        request: sift_api_types::VaultRevealStepUpRequest,
    ) -> Result<sift_api_types::VaultRevealStepUpResponse> {
        self.post(
            &format!("/v1/metadata/vault-items/{}/reveal-step-up", item.0),
            &request,
        )
        .await
    }

    pub async fn reveal_vault_item(
        &self,
        item: VaultItemId,
        lease: Option<String>,
    ) -> Result<RevealVaultSecretResponse> {
        self.post(
            &format!("/v1/metadata/vault-items/{}/reveal", item.0),
            &sift_api_types::VaultRevealRequest { lease },
        )
        .await
    }

    pub async fn auth_tokens(&self) -> Result<Vec<ApiTokenRow>> {
        self.get("/v1/auth/tokens").await
    }

    pub async fn issue_token(&self, request: IssueTokenRequest) -> Result<IssueTokenResponse> {
        self.post("/v1/auth/tokens", &request).await
    }

    pub async fn revoke_token(&self, token: ApiTokenId) -> Result<()> {
        self.delete(&format!("/v1/auth/tokens/{}", token.0)).await
    }

    pub async fn audit(&self) -> Result<Vec<sift_protocol::AuditEntry>> {
        self.get("/v1/audit").await
    }

    pub async fn operations(&self) -> Result<Vec<sift_protocol::OperationAuditEntry>> {
        self.get("/v1/operations").await
    }

    pub async fn available_operations(
        &self,
        context: &OperationCapabilityContext,
    ) -> Result<Vec<OperationCapability>> {
        let mut query = Vec::new();
        if let Some(tenant) = context.tenant_id {
            query.push(format!("tenant_id={tenant}"));
        }
        if let Some(room) = context.room_id {
            query.push(format!("room_id={room}"));
        }
        if let Some(profile) = context.connection_profile_id {
            query.push(format!("connection_profile_id={profile}"));
        }
        if let Some(session) = context.session {
            query.push(format!("session={session}"));
        }
        if let Some(connection) = context.connection {
            query.push(format!("connection={connection}"));
        }
        if let Some(transaction) = context.transaction {
            query.push(format!("transaction={transaction}"));
        }
        let suffix = if query.is_empty() {
            String::new()
        } else {
            format!("?{}", query.join("&"))
        };
        self.get(&format!("/v1/operations/available{suffix}")).await
    }

    pub async fn governed_tools(
        &self,
        context: &ToolContext,
        mcp_only: bool,
    ) -> Result<Vec<GovernedToolDescriptor>> {
        let mut query = vec![format!("mcp_only={mcp_only}")];
        if let Some(value) = context.tenant_id {
            query.push(format!("tenant_id={value}"));
        }
        if let Some(value) = context.room_id {
            query.push(format!("room_id={value}"));
        }
        if let Some(value) = context.profile_id {
            query.push(format!("profile_id={value}"));
        }
        if let Some(value) = &context.connection_id {
            query.push(format!("connection_id={value}"));
        }
        if let Some(value) = &context.document_id {
            query.push(format!("document_id={value}"));
        }
        self.get(&format!("/v1/tools?{}", query.join("&"))).await
    }

    pub async fn invoke_tool(&self, request: &InvokeToolRequest) -> Result<InvokeToolResponse> {
        self.post("/v1/tools/invoke", request).await
    }

    pub async fn providers(&self) -> Result<Vec<ProviderDescriptor>> {
        self.get("/v1/providers").await
    }

    pub async fn extensions(&self) -> Result<Vec<ExtensionDescriptor>> {
        self.get("/v1/extensions").await
    }

    pub async fn extension(&self, publisher: &str, name: &str) -> Result<ExtensionDescriptor> {
        self.get(&format!("/v1/extensions/{publisher}/{name}"))
            .await
    }

    pub async fn validate_extension(&self, archive: Vec<u8>) -> Result<ValidatedExtensionPackage> {
        self.post_archive("/v1/extensions/validate", archive).await
    }

    pub async fn install_extension(
        &self,
        archive: Vec<u8>,
        allow_unsigned_local: bool,
    ) -> Result<ValidatedExtensionPackage> {
        self.post_archive(
            &format!("/v1/extensions/install?allow_unsigned_local={allow_unsigned_local}"),
            archive,
        )
        .await
    }

    pub async fn select_extension(
        &self,
        publisher: &str,
        name: &str,
        request: &ExtensionSelectionRequest,
    ) -> Result<ExtensionDescriptor> {
        self.put(
            &format!("/v1/extensions/{publisher}/{name}/selection"),
            request,
        )
        .await
    }

    pub async fn grant_extension(
        &self,
        publisher: &str,
        name: &str,
        request: &ExtensionGrantRequest,
    ) -> Result<ExtensionDescriptor> {
        self.put(
            &format!("/v1/extensions/{publisher}/{name}/grants"),
            request,
        )
        .await
    }

    pub async fn allow_extension_tenant(
        &self,
        publisher: &str,
        name: &str,
        tenant_id: i64,
        request: &ExtensionTenantSelectionRequest,
    ) -> Result<ExtensionDescriptor> {
        self.put(
            &format!("/v1/extensions/{publisher}/{name}/tenants/{tenant_id}"),
            request,
        )
        .await
    }

    pub async fn rollback_extension(
        &self,
        publisher: &str,
        name: &str,
        request: &ExpectedRevision,
    ) -> Result<ExtensionDescriptor> {
        self.post(
            &format!("/v1/extensions/{publisher}/{name}/rollback"),
            request,
        )
        .await
    }

    pub async fn uninstall_extension(
        &self,
        publisher: &str,
        name: &str,
        expected_revision: u64,
    ) -> Result<ExtensionDescriptor> {
        self.send(self.http.delete(self.url(&format!(
            "/v1/extensions/{publisher}/{name}?expected_revision={expected_revision}"
        ))))
        .await
    }

    pub async fn purge_extension(
        &self,
        publisher: &str,
        name: &str,
        request: &ExpectedRevision,
    ) -> Result<ExtensionPurgeResponse> {
        self.post(&format!("/v1/extensions/{publisher}/{name}/purge"), request)
            .await
    }

    pub async fn extension_diagnostics(
        &self,
        publisher: &str,
        name: &str,
    ) -> Result<ExtensionDiagnostics> {
        self.get(&format!("/v1/extensions/{publisher}/{name}/diagnostics"))
            .await
    }

    pub async fn invoke_extension(
        &self,
        request: &InvokeExtensionRequest,
    ) -> Result<InvokeExtensionOutcome> {
        self.post("/v1/extension-actions/invoke", request).await
    }

    pub async fn create_operation_approval(
        &self,
        request: &sift_protocol::CreateOperationApprovalRequest,
    ) -> Result<OperationApproval> {
        self.post("/v1/operation-approvals", request).await
    }

    pub async fn approve_operation(
        &self,
        approval_id: &str,
        request: &ExpectedRevision,
    ) -> Result<OperationApproval> {
        self.post(
            &format!("/v1/operation-approvals/{approval_id}/approve"),
            request,
        )
        .await
    }

    /// Durable operation-audit rows (actor, target, result code, row count,
    /// sanitized failure message). Requires a configured metadata store.
    pub async fn operation_audit(&self) -> Result<Vec<OperationAudit>> {
        self.get("/v1/operations/audit").await
    }

    pub async fn operation_audit_page(
        &self,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<CursorPage<OperationAudit>> {
        let mut query = Vec::new();
        if let Some(cursor) = cursor {
            query.push(format!("cursor={cursor}"));
        }
        if let Some(limit) = limit {
            query.push(format!("limit={limit}"));
        }
        let suffix = if query.is_empty() {
            String::new()
        } else {
            format!("?{}", query.join("&"))
        };
        self.get(&format!("/v1/operations/audit/pages{suffix}"))
            .await
    }

    pub async fn change_ledger(
        &self,
        filter: &sift_protocol::ChangeLedgerFilter,
    ) -> Result<sift_protocol::ChangeLedgerPage> {
        let mut query = Vec::new();
        if let Some(value) = filter.tenant_id {
            query.push(format!("tenant_id={value}"));
        }
        if let Some(value) = filter.connection_profile_id {
            query.push(format!("connection_profile_id={value}"));
        }
        if let Some(value) = &filter.database_target {
            query.push(format!("database_target={}", urlencoding_replace(value)));
        }
        if let Some(value) = &filter.affected_object {
            query.push(format!("affected_object={}", urlencoding_replace(value)));
        }
        if let Some(value) = filter.executed_by {
            query.push(format!("executed_by={value}"));
        }
        if let Some(value) = filter.operation {
            let value = serde_json::to_value(value)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default();
            query.push(format!("operation={value}"));
        }
        if let Some(value) = filter.from {
            query.push(format!("from={}", urlencoding_replace(&value.to_rfc3339())));
        }
        if let Some(value) = filter.to {
            query.push(format!("to={}", urlencoding_replace(&value.to_rfc3339())));
        }
        if let Some(value) = &filter.git_commit {
            query.push(format!("git_commit={}", urlencoding_replace(value)));
        }
        if let Some(value) = filter.before_id {
            query.push(format!("before_id={value}"));
        }
        if let Some(value) = filter.limit {
            query.push(format!("limit={value}"));
        }
        let suffix = if query.is_empty() {
            String::new()
        } else {
            format!("?{}", query.join("&"))
        };
        self.get(&format!("/v1/change-ledger{suffix}")).await
    }

    pub async fn stream_query(
        &self,
        session: SessionId,
        connection: ConnectionId,
        sql: impl Into<String>,
    ) -> Result<Vec<Page>> {
        let mut stream = self.start_query_stream(session, connection, sql).await?;
        let mut pages = Vec::new();
        loop {
            let (seq, page) = stream.next_page().await?;
            let done = matches!(page, Page::Done { .. } | Page::Error { .. });
            pages.push(page);
            if done {
                return Ok(pages);
            }
            stream.acknowledge(seq).await?;
        }
    }

    /// Start a query without buffering its result pages. Consumers acknowledge
    /// each page only after downstream processing has completed.
    pub async fn start_query_stream(
        &self,
        session: SessionId,
        connection: ConnectionId,
        sql: impl Into<String>,
    ) -> Result<QueryStream> {
        self.start_query_stream_with(session, connection, sql, Vec::new(), None)
            .await
    }

    pub async fn start_query_stream_with(
        &self,
        session: SessionId,
        connection: ConnectionId,
        sql: impl Into<String>,
        params: Vec<Value>,
        tx: Option<TxHandleRef>,
    ) -> Result<QueryStream> {
        self.start_query_stream_transformed(session, connection, sql, params, tx, None)
            .await
    }

    pub async fn start_query_stream_transformed(
        &self,
        session: SessionId,
        connection: ConnectionId,
        sql: impl Into<String>,
        params: Vec<Value>,
        tx: Option<TxHandleRef>,
        transform: Option<sift_protocol::ResultTransform>,
    ) -> Result<QueryStream> {
        self.start_query_stream_versioned(session, connection, sql, params, tx, transform, None)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn start_query_stream_versioned(
        &self,
        session: SessionId,
        connection: ConnectionId,
        sql: impl Into<String>,
        params: Vec<Value>,
        tx: Option<TxHandleRef>,
        transform: Option<sift_protocol::ResultTransform>,
        source: Option<sift_protocol::VersionedExecutionContext>,
    ) -> Result<QueryStream> {
        let mut socket = self.connect_session_websocket(session).await?;
        let request_id = "sdk-stream-query".to_string();
        socket
            .send(WsClientMessage::Execute {
                request_id: request_id.clone(),
                connection,
                sql: sql.into(),
                event_version: None,
                params,
                tx,
                transform,
                source: source.map(Box::new),
                variable_context: None,
            })
            .await?;

        let first = socket.next().await?;
        let cursor_id = match first {
            WsServerMessage::Started {
                request_id: got,
                cursor_id,
            } if got == request_id => cursor_id,
            other => {
                return Err(Error::Protocol(format!(
                    "expected started message, got {other:?}"
                )));
            }
        };
        Ok(QueryStream {
            socket,
            connection,
            cursor_id,
        })
    }

    /// Start an execution using ADR-053 event lifecycle. Each returned vector
    /// corresponds to one driver page and must be acknowledged as a unit.
    #[allow(clippy::too_many_arguments)]
    pub async fn start_query_event_stream_versioned(
        &self,
        session: SessionId,
        connection: ConnectionId,
        sql: impl Into<String>,
        params: Vec<Value>,
        tx: Option<TxHandleRef>,
        transform: Option<sift_protocol::ResultTransform>,
        source: Option<sift_protocol::VersionedExecutionContext>,
    ) -> Result<QueryEventStream> {
        self.start_query_event_stream_versioned_with_variables(
            session, connection, sql, params, tx, transform, source, None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn start_query_event_stream_versioned_with_variables(
        &self,
        session: SessionId,
        connection: ConnectionId,
        sql: impl Into<String>,
        params: Vec<Value>,
        tx: Option<TxHandleRef>,
        transform: Option<sift_protocol::ResultTransform>,
        source: Option<sift_protocol::VersionedExecutionContext>,
        variable_context: Option<sift_protocol::SqlVariableHistoryContext>,
    ) -> Result<QueryEventStream> {
        let mut socket = self.connect_session_websocket(session).await?;
        let request_id = "sdk-stream-execution-v2".to_string();
        socket
            .send(WsClientMessage::Execute {
                request_id: request_id.clone(),
                connection,
                sql: sql.into(),
                event_version: Some(sift_protocol::EXECUTION_EVENT_VERSION),
                params,
                tx,
                transform,
                source: source.map(Box::new),
                variable_context: variable_context.map(Box::new),
            })
            .await?;

        let first = socket.next().await?;
        let cursor_id = match first {
            WsServerMessage::Started {
                request_id: got,
                cursor_id,
            } if got == request_id => cursor_id,
            other => {
                return Err(Error::Protocol(format!(
                    "expected started message, got {other:?}"
                )));
            }
        };
        Ok(QueryEventStream {
            socket,
            connection,
            cursor_id,
        })
    }

    pub async fn connect_session_websocket(&self, session: SessionId) -> Result<SessionWebSocket> {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;

        let mut request = self.ws_url(session).into_client_request()?;
        let selected = self.negotiated().await?.selected_protocol;
        insert_ws_protocol_header(&mut request, selected).map_err(Error::Protocol)?;
        if let Some(token) = self.current_bearer().await {
            request.headers_mut().insert(
                "authorization",
                format!("Bearer {token}").parse().map_err(|error| {
                    Error::Protocol(format!("invalid bearer token header: {error}"))
                })?,
            );
        }
        let (socket, response) = tokio_tungstenite::connect_async(request).await?;
        validate_ws_response_protocol(&response, selected).map_err(Error::Protocol)?;
        Ok(SessionWebSocket { socket })
    }

    pub async fn connect_room_websocket(&self, room: RoomId) -> Result<RoomWebSocket> {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;

        let mut request = self.room_ws_url(room).into_client_request()?;
        let selected = self.negotiated().await?.selected_protocol;
        insert_ws_protocol_header(&mut request, selected).map_err(Error::Protocol)?;
        if let Some(token) = self.current_bearer().await {
            request.headers_mut().insert(
                "authorization",
                format!("Bearer {token}").parse().map_err(|error| {
                    Error::Protocol(format!("invalid bearer token header: {error}"))
                })?,
            );
        }
        let (socket, response) = tokio_tungstenite::connect_async(request).await?;
        validate_ws_response_protocol(&response, selected).map_err(Error::Protocol)?;
        Ok(RoomWebSocket { socket })
    }

    pub async fn listen_notifications(
        &self,
        session: SessionId,
        connection: ConnectionId,
        channels: Vec<String>,
        max_notifications: usize,
    ) -> Result<Vec<(String, String)>> {
        use futures::SinkExt;
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        use tokio_tungstenite::tungstenite::Message;

        let mut request = self.ws_url(session).into_client_request()?;
        let selected = self.negotiated().await?.selected_protocol;
        insert_ws_protocol_header(&mut request, selected).map_err(Error::Protocol)?;
        if let Some(token) = self.current_bearer().await {
            request.headers_mut().insert(
                "authorization",
                format!("Bearer {token}")
                    .parse()
                    .map_err(|e| Error::Protocol(format!("invalid bearer token header: {e}")))?,
            );
        }
        let (mut ws, response) = tokio_tungstenite::connect_async(request).await?;
        validate_ws_response_protocol(&response, selected).map_err(Error::Protocol)?;
        let request_id = "sdk-listen".to_string();
        ws.send(Message::Text(
            serde_json::to_string(&WsClientMessage::Listen {
                request_id: request_id.clone(),
                connection,
                channels,
            })?
            .into(),
        ))
        .await?;

        let mut notifications = Vec::with_capacity(max_notifications);
        while notifications.len() < max_notifications {
            match next_ws(&mut ws).await? {
                WsServerMessage::Notification {
                    request_id: got,
                    channel,
                    payload,
                } if got == request_id => notifications.push((channel, payload)),
                WsServerMessage::Error { message, .. } => return Err(Error::Protocol(message)),
                other => {
                    return Err(Error::Protocol(format!(
                        "unexpected websocket message: {other:?}"
                    )));
                }
            }
        }
        Ok(notifications)
    }

    /// Subscribe without buffering notifications. Dropping the returned
    /// stream closes its dedicated socket and releases the server listener.
    pub async fn subscribe_notifications(
        &self,
        session: SessionId,
        connection: ConnectionId,
        channels: Vec<String>,
    ) -> Result<NotificationStream> {
        let mut socket = self.connect_session_websocket(session).await?;
        let request_id = format!("sdk-listen-{}", uuid::Uuid::new_v4());
        socket
            .send(WsClientMessage::Listen {
                request_id: request_id.clone(),
                connection,
                channels,
            })
            .await?;
        Ok(NotificationStream { socket, request_id })
    }

    pub fn ws_url(&self, session: SessionId) -> String {
        let base = self
            .base
            .strip_prefix("https://")
            .map(|s| format!("wss://{s}"))
            .or_else(|| {
                self.base
                    .strip_prefix("http://")
                    .map(|s| format!("ws://{s}"))
            })
            .unwrap_or_else(|| self.base.clone());
        format!("{base}/v1/sessions/{session}/ws")
    }

    pub fn room_ws_url(&self, room: RoomId) -> String {
        let base = self
            .base
            .strip_prefix("https://")
            .map(|s| format!("wss://{s}"))
            .or_else(|| {
                self.base
                    .strip_prefix("http://")
                    .map(|s| format!("ws://{s}"))
            })
            .unwrap_or_else(|| self.base.clone());
        format!("{base}/v1/metadata/rooms/{}/ws", room.0)
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.send(self.http.get(self.url(path))).await
    }

    async fn post<T: serde::de::DeserializeOwned, B: serde::Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        self.send(self.http.post(self.url(path)).json(body)).await
    }

    async fn post_archive<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        archive: Vec<u8>,
    ) -> Result<T> {
        self.send(
            self.http
                .post(self.url(path))
                .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                .body(archive),
        )
        .await
    }

    async fn put<T: serde::de::DeserializeOwned, B: serde::Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        self.send(self.http.put(self.url(path)).json(body)).await
    }

    async fn post_empty<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.send(self.http.post(self.url(path))).await
    }

    async fn post_empty_body<B: serde::Serialize>(&self, path: &str, body: &B) -> Result<()> {
        let _: serde_json::Value = self.post(path, body).await?;
        Ok(())
    }

    async fn delete(&self, path: &str) -> Result<()> {
        let _: serde_json::Value = self.send(self.http.delete(self.url(path))).await?;
        Ok(())
    }

    async fn delete_response<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.send(self.http.delete(self.url(path))).await
    }

    async fn delete_body<B: serde::Serialize>(&self, path: &str, body: &B) -> Result<()> {
        let _: serde_json::Value = self
            .send(self.http.delete(self.url(path)).json(body))
            .await?;
        Ok(())
    }

    async fn delete_empty(&self, path: &str) -> Result<()> {
        let response = self.send_response(self.http.delete(self.url(path))).await?;
        if !response.status().is_success() {
            return Err(server_error(response).await);
        }
        Ok(())
    }

    async fn send<T: serde::de::DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<T> {
        let response = self.send_response(request).await?;
        let status = response.status();
        if !status.is_success() {
            return Err(server_error(response).await);
        }
        Ok(response.json().await?)
    }

    async fn send_response(
        &self,
        mut request: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response> {
        let selected = self.negotiated().await?.selected_protocol;
        request = request.header(PROTOCOL_VERSION_HEADER, selected.to_string());
        if let Some(token) = self.current_bearer().await {
            request = request.bearer_auth(token);
        }
        let response = request.send().await?;
        validate_response_protocol(&response, selected).map_err(Error::Protocol)?;
        Ok(response)
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    async fn current_bearer(&self) -> Option<String> {
        if let Some(provider) = &self.session_tokens {
            return Some(provider.access_token().await);
        }
        self.token.clone()
    }
}

fn validate_response_protocol(
    response: &reqwest::Response,
    expected: u32,
) -> std::result::Result<(), String> {
    let actual = response
        .headers()
        .get(PROTOCOL_VERSION_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| "response omitted X-Sift-Protocol-Version after handshake".to_string())?;
    if actual != expected.to_string() {
        return Err(format!(
            "response protocol version mismatch: selected {expected}, received {actual}"
        ));
    }
    Ok(())
}

fn insert_ws_protocol_header(
    request: &mut tokio_tungstenite::tungstenite::http::Request<()>,
    selected: u32,
) -> std::result::Result<(), String> {
    request.headers_mut().insert(
        PROTOCOL_VERSION_HEADER,
        selected
            .to_string()
            .parse()
            .map_err(|error| format!("invalid protocol version header: {error}"))?,
    );
    Ok(())
}

fn validate_ws_response_protocol(
    response: &tokio_tungstenite::tungstenite::handshake::client::Response,
    expected: u32,
) -> std::result::Result<(), String> {
    let actual = response
        .headers()
        .get(PROTOCOL_VERSION_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            "WebSocket upgrade omitted X-Sift-Protocol-Version after handshake".to_string()
        })?;
    if actual != expected.to_string() {
        return Err(format!(
            "WebSocket protocol version mismatch: selected {expected}, received {actual}"
        ));
    }
    Ok(())
}

async fn server_error(response: reqwest::Response) -> Error {
    let status = response.status();
    let retry_after_secs = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok());
    let body = response.text().await.unwrap_or_default();
    let mut error = serde_json::from_str::<ApiErrorResponse>(&body).unwrap_or(ApiErrorResponse {
        kind: "http_error".to_string(),
        message: body,
        correlation_id: None,
        retry_after_secs: None,
        edit_conflict: None,
    });
    error.retry_after_secs = error.retry_after_secs.or(retry_after_secs);
    Error::Server { status, error }
}

async fn next_ws<S>(ws: &mut S) -> Result<WsServerMessage>
where
    S: futures::Stream<
            Item = std::result::Result<
                tokio_tungstenite::tungstenite::Message,
                tokio_tungstenite::tungstenite::Error,
            >,
        > + Unpin,
{
    use futures::StreamExt;
    use tokio_tungstenite::tungstenite::Message;

    loop {
        let Some(message) = ws.next().await else {
            return Err(Error::Protocol("websocket closed".into()));
        };
        match message? {
            Message::Text(text) => return Ok(serde_json::from_str(&text)?),
            Message::Binary(bytes) => return Ok(serde_json::from_slice(&bytes)?),
            Message::Close(_) => return Err(Error::Protocol("websocket closed".into())),
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
        }
    }
}

async fn next_room_ws<S>(ws: &mut S) -> Result<sift_protocol::RoomServerMessage>
where
    S: futures::Stream<
            Item = std::result::Result<
                tokio_tungstenite::tungstenite::Message,
                tokio_tungstenite::tungstenite::Error,
            >,
        > + Unpin,
{
    use futures::StreamExt;
    use tokio_tungstenite::tungstenite::Message;

    loop {
        let Some(message) = ws.next().await else {
            return Err(Error::Protocol("websocket closed".into()));
        };
        match message? {
            Message::Text(text) => return Ok(serde_json::from_str(&text)?),
            Message::Binary(bytes) => return Ok(serde_json::from_slice(&bytes)?),
            Message::Close(_) => return Err(Error::Protocol("websocket closed".into())),
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
        }
    }
}

/// Minimal percent-encoding for query-string values. Only encodes
/// characters that would actually break parsing (`&`, `=`, `#`, `+`,
/// `%`, whitespace). Sufficient for typed SDK callers, which don't
/// produce hostile input.
fn urlencoding_replace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("%26"),
            '=' => out.push_str("%3D"),
            '#' => out.push_str("%23"),
            '+' => out.push_str("%2B"),
            '%' => out.push_str("%25"),
            ' ' => out.push_str("%20"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(access: &str, refresh: &str) -> AuthTokensResponse {
        AuthTokensResponse {
            access_token: access.into(),
            access_expires_at: chrono::Utc::now(),
            refresh_token: refresh.into(),
            refresh_expires_at: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn session_token_provider_rotates_and_redacts() {
        let provider = SessionTokenProvider::new(tokens("access-one", "refresh-one"));
        assert_eq!(provider.access_token().await, "access-one");
        assert!(!format!("{provider:?}").contains("access-one"));
        provider.replace(tokens("access-two", "refresh-two")).await;
        assert_eq!(provider.access_token().await, "access-two");
        assert_eq!(provider.refresh_token().await, "refresh-two");
        let snapshot = provider.snapshot().await;
        assert_eq!(snapshot.access_token, "access-two");
        assert_eq!(snapshot.refresh_token, "refresh-two");
    }

    #[test]
    fn spilled_cursor_batches_have_a_typed_shape() {
        let batch: SpilledCursorPages = serde_json::from_value(serde_json::json!({
            "cursor_id": 42,
            "pages": [{
                "kind": "done",
                "affected_rows": null,
                "warnings": []
            }],
            "done": true
        }))
        .unwrap();
        assert_eq!(batch.cursor_id, CursorId(42));
        assert!(batch.done);
        assert!(matches!(batch.pages.as_slice(), [Page::Done { .. }]));
    }
}
