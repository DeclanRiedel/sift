//! `sift-protocol` — pure serde types, no I/O (ADR-004).
//!
//! The public contract consumed by the server, the desktop binary, and the
//! future wasm web client. Holds operation enums, request/response structs,
//! error codes, and serde models — and nothing else. No `tokio`, no
//! networking, no filesystem.

/// Current wire protocol version.
pub const PROTOCOL_VERSION_NUMBER: u32 = 1;
/// Header representation of [`PROTOCOL_VERSION_NUMBER`].
pub const PROTOCOL_VERSION: &str = "1";

pub mod auth;
pub mod automation;
pub mod capability;
pub mod catalog;
pub mod column;
pub mod comparison;
pub mod completion;
pub mod connection;
pub mod crdt;
pub mod csv_import;
pub mod edit;
pub mod engine;
pub mod error;
pub mod extension;
pub mod handshake;
pub mod migration;
pub mod operation;
pub mod pagination;
pub mod plan;
pub mod policy;
pub mod process;
pub mod provider;
pub mod remote;
pub mod result;
pub mod room;
pub mod run;
pub mod schema;
pub mod schema_diff;
pub mod search;
pub mod semantic;
pub mod session;
pub mod transaction_panel;
pub mod transfer;
pub mod tx;
pub mod value;
pub mod vcs;
pub mod workspace;

pub use auth::{
    AcceptTenantInvitationRequest, AdminCreatePasswordPrincipalRequest,
    AdminLinkPasswordIdentityRequest, AdminSetPrincipalDisabledRequest, AuthClientKind,
    AuthIdentitySummary, AuthPrincipal, AuthSessionSummary, AuthTenantMembership,
    AuthTokensResponse, ChangePasswordRequest, CreateGithubAllowlistRequest,
    CreateTenantInvitationRequest, GithubNativeAuthExchangeRequest, GithubNativeAuthStartResponse,
    InvitationRole, IssuedPasswordResetResponse, IssuedTenantInvitationResponse,
    KeyAuthenticateRequest, KeyChallengeRequest, KeyChallengeResponse, PasswordLoginRequest,
    PasswordResetRequest, RedactedString, RefreshAuthRequest, RegisterPrincipalKeyRequest,
    SshProxyAccessGrant, SshProxyCapabilityClaims, SshProxyCapabilityExchangeRequest,
    WebAuthResponse, WhoAmIResponse,
};
pub use automation::*;
pub use capability::{OperationCapability, OperationCapabilityContext, OperationKind};
pub use catalog::*;
pub use column::{
    EngineColumnFacets, MssqlColumnFacets, Nullability, PgColumnFacets, PrimitiveType,
    TypeCategory, TypeRef,
};
pub use comparison::*;
pub use connection::{
    AccessMode as ConnAccessMode, EngineConnectionSpec, MssqlConnectionSpec, PgConnectionSpec,
    ServerInfo, SslMode,
};
pub use crdt::{
    CrdtCursor, CrdtSnapshot, CrdtUpdate, DocumentFrontier, DocumentVersion, ReplicaId,
    RoomConnectionId, RoomResultId,
};
pub use csv_import::{
    CsvConflictPolicy, CsvImportRequest, CsvImportResponse, InferredCsvColumn, InferredCsvType,
};
pub use edit::{
    ApplyEditsRequest, ApplyEditsResult, CellEdit, EditConflict, EditOutcome, EditPlan, EditSet,
    EditStatement, EditStatementKind, IdentitySource, PreviewEditsRequest, RowEdit, RowKey,
};
pub use engine::Engine;
pub use error::{Code, DriverError, DriverWarning};
pub use extension::*;
pub use handshake::{
    HandshakeClientKind, HandshakeDeployment, HandshakeRequest, HandshakeResponse,
    HandshakeRuntimeMode, HandshakeTransport, ProtocolRange,
};
pub use migration::*;
pub use operation::{
    AuthenticationMethod, DdlSourceAction, ExtensionAdminAction, IdentityAdminAction,
    InstanceConfigurationAction, Operation, OperationSummary, PolicyAdminAction, RunAction,
    RunConfigurationAction, ScheduleAction, TransferRecipeAction, VcsAction, WorkspaceAction,
};
pub use pagination::CursorPage;
pub use plan::*;
pub use policy::{
    ApiErrorResponse, ConnectionPolicy, DisconnectManagedConnectionsResponse, RateLimitClass,
    SchemaSelector, TenantResource, TenantResourceLimits, TenantResourceUsage, TenantRole,
    TenantUsageSnapshot, UpdateConnectionPolicyRequest, UpdateTenantLimitsRequest,
};
pub use process::{DatabaseProcess, KillProcessRequest, KillProcessResponse};
pub use provider::*;
pub use remote::{
    RemoteCapabilityResponse, RemoteDaemonDescriptor, RemoteKeyChallenge, RemoteProbeResponse,
    RemoteReady,
};
pub use result::{CursorId, ExecuteRequest, Page, Row};
pub use room::{
    DocumentErrorCode, DocumentTransferKind, RoomClientMessage, RoomPresence, RoomQueryResult,
    RoomQueryStatus, RoomResultPage, RoomResultPages, RoomSelection, RoomServerMessage,
};
pub use run::*;
pub use schema::{
    CatalogTree, ConstraintInfo, ConstraintKind, IndexInfo, IndexKind, ObjectDdl, ObjectInfo,
    ObjectKind, ObjectPath, SchemaDepth, SchemaFilter, SchemaScope, SchemaSnapshot, SchemaTree,
    TriggerEvent, TriggerInfo, TriggerTiming,
};
pub use schema_diff::*;
pub use search::{
    DataSearchHit, DataSearchRequest, DataSearchResponse, DataSearchScope, IndexState,
    SchemaSearchRequest, SchemaSearchResponse, SearchHit, SearchTarget,
};
pub use semantic::*;
pub use session::{
    Ack, AuditEntry, BeginTransactionRequest, BulkInsertFormat, BulkInsertRequest,
    BulkInsertResponse, CancelRequest, ConnectionId, ConnectionInfo, EndTransactionRequest,
    ExecuteRequestHttp, ExecuteResponse, ExportFormat, ExportRequest, Health,
    OpenConnectionRequest, OpenSessionRequest, OperationAuditEntry, OperationStatus, Readiness,
    SavepointRequest, SessionId, SessionInfo, TransactionInfo, TxHandleRef, WsClientMessage,
    WsServerMessage,
};
pub use transaction_panel::{
    SavepointInfo, SavepointState, TransactionCondition, TransactionEndAction, TransactionPreview,
    TransactionPreviewRequest, TransactionState,
};
pub use transfer::*;
pub use tx::{AccessMode as TxAccessMode, IsolationLevel, TxId, TxMode};
pub use vcs::*;
pub use workspace::*;

/// Re-export of [`ConnectionSpec`].
pub use connection::ConnectionSpec;

/// Re-export of [`ColumnMetadata`].
pub use column::ColumnMetadata;

/// Re-export of [`Value`].
pub use value::Value;
