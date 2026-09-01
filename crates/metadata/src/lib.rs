//! Local metadata persistence for tenants, principals, connection profiles,
//! rooms, documents, and room-scoped history.

use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use base64::Engine as _;
use chrono::{DateTime, Utc};
use fs2::FileExt as _;
use rand_core::OsRng;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};
#[cfg(test)]
use sift_protocol::ConnectionSpec;
use sift_protocol::{
    AuthSessionSummary, ConnectionPolicy, SshProxyCapabilityClaims, TenantResourceLimits,
    UpdateConnectionPolicyRequest,
};
use uuid::Uuid;

mod api_token;
mod approval;
mod catalog_snapshot;
mod change_ledger;
mod ddl_source;
mod extension;
mod extension_storage;
pub mod http;
mod instance_manifest;
mod migration_run;
mod plan_capture;
mod projection;
mod repository;
mod run_configuration;
mod run_schedule;
pub mod schema;
pub mod secrets;
mod transfer_recipe;
mod vault;
mod workspace;

pub use approval::*;
pub use change_ledger::*;
pub use extension::*;
pub use extension_storage::*;
pub use instance_manifest::*;
pub use plan_capture::PlanCaptureRetention;
pub use schema::*;
#[cfg(feature = "os-keychain")]
pub use secrets::OsKeychainSecretStore;
pub use secrets::{FileSecretStore, MemorySecretStore, SecretStore};
pub use workspace::{
    public_workspace, public_workspace_with_integrations, public_workspace_with_projection,
};

mod migrations {
    refinery::embed_migrations!("migrations");
}

fn migration_kind(version: u32) -> Result<MigrationKind> {
    match version {
        6 => Ok(MigrationKind::LegacyContract),
        19 => Ok(MigrationKind::Contract),
        26 | 27 => Ok(MigrationKind::Data),
        1..=5 | 7..=18 | 20..=25 | 28..=43 => Ok(MigrationKind::Expand),
        _ => Err(MetadataError::InvalidMigrationHistory(format!(
            "embedded V{version} has no lifecycle classification"
        ))),
    }
}

fn migration_descriptor(migration: &refinery::Migration) -> Result<MigrationDescriptor> {
    let kind = migration_kind(migration.version())?;
    Ok(MigrationDescriptor {
        version: migration.version(),
        name: migration.name().to_string(),
        kind,
        automatic: !matches!(kind, MigrationKind::Contract),
    })
}

const SECRET_NAMESPACE: &str = "sift.local";
const PASSWORD_SECRET_NAMESPACE: &str = "sift.auth.password";
const AUTH_SYSTEM_SECRET_NAMESPACE: &str = "sift.auth.system";
const AUTH_TOKEN_MAC_HANDLE: &str = "token-mac-v1";
const SSH_PROXY_CAPABILITY_MAC_HANDLE: &str = "ssh-proxy-capability-mac-v1";
const OAUTH_SECRET_NAMESPACE: &str = "sift.auth.oauth";
const OAUTH_STATE_PREFIX: &str = "sift_oauth_";
const GITHUB_HANDOFF_PREFIX: &str = "sift_gh_";
const INVITATION_TOKEN_PREFIX: &str = "sift_inv_";
const PASSWORD_RESET_TOKEN_PREFIX: &str = "sift_pr_";
const ACCESS_TOKEN_PREFIX: &str = "sift_at_";
const SSH_PROXY_CAPABILITY_PREFIX: &str = "sift_sshcap_v1.";
const REFRESH_TOKEN_PREFIX: &str = "sift_rt_";
const AUTH_TOKEN_LOOKUP_LEN: usize = 12;
const ACCESS_TOKEN_TTL_MINUTES: i64 = 15;
const REFRESH_TOKEN_TTL_DAYS: i64 = 30;
const API_TOKEN_PREFIX: &str = "sift_";
const API_TOKEN_LOOKUP_LEN: usize = 12;
const API_TOKEN_LAST_USED_DEBOUNCE_SECS: i64 = 300;
const API_TOKEN_MAC_KEY: &[u8] = b"sift.metadata.api-token.v1";

pub type Result<T> = std::result::Result<T, MetadataError>;

#[derive(Debug, thiserror::Error)]
pub enum MetadataError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("migration error: {0}")]
    Migration(#[from] refinery::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("instance configuration error: {0}")]
    InstanceConfig(#[from] sift_instance_config::ConfigError),
    #[error("instance manifest conflict: {0}")]
    InstanceManifestConflict(String),
    #[error("apply would destroy managed resources; rerun with explicit destroy approval: {0:?}")]
    InstanceDestroyApprovalRequired(Vec<String>),
    #[error("managed resource `{0}` has prevent_destroy enabled")]
    InstancePreventDestroy(String),
    #[error("credential slot `{0}` was not declared by the active manifest")]
    InstanceCredentialSlotNotFound(String),
    #[error("credential for slot `{slot}` is invalid: {message}")]
    InstanceCredentialInvalid { slot: String, message: String },
    #[error("password hash error: {0}")]
    PasswordHash(String),
    #[error("invalid {field} value: {value}")]
    InvalidEnum { field: &'static str, value: String },
    #[error("invalid timestamp {value}: {source}")]
    InvalidTimestamp {
        value: String,
        source: chrono::ParseError,
    },
    #[error("connection profile {0:?} not found")]
    ConnectionProfileNotFound(ConnectionProfileId),
    #[error("connection profile {0:?} is managed by sift.toml; edit the manifest instead")]
    ConnectionProfileManaged(ConnectionProfileId),
    #[error("connection profile limit reached for tenant {0:?}")]
    ConnectionProfileLimitReached(TenantId),
    #[error("connection profile {0:?} has no credential for principal {1:?}")]
    MissingCredential(ConnectionProfileId, PrincipalId),
    #[error("connection profile {0:?} uses broker credentials, which are not implemented")]
    BrokerCredentialUnsupported(ConnectionProfileId),
    #[error("broker credentials cannot be stored until a credential-broker plugin is configured")]
    BrokerCredentialModeUnsupported,
    #[error("connection profile {profile:?} uses {actual:?} credentials, not {expected:?}")]
    CredentialModeMismatch {
        profile: ConnectionProfileId,
        expected: CredentialMode,
        actual: CredentialMode,
    },
    #[error("provider credentials must be a non-empty JSON object")]
    InvalidCredentialObject,
    #[error("inline provider credentials require shared credential mode")]
    InlineCredentialsRequireSharedMode,
    #[error("connection profile {0:?} is not in tenant {1:?}")]
    TenantMismatch(ConnectionProfileId, TenantId),
    #[error("connection profile policy revision conflict: expected {expected}, current {current}")]
    PolicyRevisionConflict { expected: u64, current: u64 },
    #[error("saved query revision conflict: expected {expected}, current {current}")]
    SavedQueryRevisionConflict { expected: u64, current: u64 },
    #[error("vault {0:?} not found")]
    VaultNotFound(sift_api_types::VaultId),
    #[error("vault item {0:?} not found")]
    VaultItemNotFound(sift_api_types::VaultItemId),
    #[error("vault permission denied")]
    VaultPermissionDenied,
    #[error("vault revision conflict: expected {expected}, current {current}")]
    VaultRevisionConflict { expected: u64, current: u64 },
    #[error("vault secret is missing")]
    VaultSecretMissing,
    #[error("connection credentials cannot be revealed")]
    VaultSecretNotRevealable,
    #[error("invalid vault input: {0}")]
    InvalidVaultInput(String),
    #[error("workspace {0:?} not found")]
    WorkspaceNotFound(sift_protocol::WorkspaceId),
    #[error("workspace node {0:?} not found")]
    WorkspaceNodeNotFound(sift_protocol::WorkspaceNodeId),
    #[error("workspace checkpoint {0:?} not found")]
    WorkspaceCheckpointNotFound(sift_protocol::WorkspaceCheckpointId),
    #[error("workspace revision conflict: expected {expected}, current {current}")]
    WorkspaceRevisionConflict { expected: u64, current: u64 },
    #[error("workspace name is invalid")]
    InvalidWorkspaceName,
    #[error("workspace path is invalid")]
    InvalidWorkspacePath,
    #[error("workspace path already exists")]
    WorkspacePathConflict,
    #[error("workspace node kind or content is invalid")]
    InvalidWorkspaceNode,
    #[error("workspace checkpoint is invalid")]
    InvalidWorkspaceCheckpoint,
    #[error("workspace-owned documents must be mutated through the workspace API")]
    WorkspaceDocumentManaged,
    #[error("workspace resource limit reached")]
    WorkspaceLimitReached,
    #[error("workspace batch is empty or exceeds its mutation limit")]
    InvalidWorkspaceBatch,
    #[error("workspace projection binding {0:?} not found")]
    ProjectionBindingNotFound(sift_protocol::ProjectionBindingId),
    #[error("workspace projection revision conflict: expected {expected}, current {current}")]
    ProjectionRevisionConflict { expected: u64, current: u64 },
    #[error("workspace projection binding is invalid")]
    InvalidProjectionBinding,
    #[error("DDL source {0:?} not found")]
    DdlSourceNotFound(sift_protocol::DdlSourceId),
    #[error("DDL source revision conflict: expected {expected}, current {current}")]
    DdlSourceRevisionConflict { expected: u64, current: u64 },
    #[error("DDL source is invalid")]
    InvalidDdlSource,
    #[error("repository binding {0:?} not found")]
    RepositoryBindingNotFound(sift_protocol::RepositoryBindingId),
    #[error("repository binding revision conflict: expected {expected}, current {current}")]
    RepositoryRevisionConflict { expected: u64, current: u64 },
    #[error("repository binding is invalid")]
    InvalidRepositoryBinding,
    #[error("run configuration {0:?} not found")]
    RunConfigurationNotFound(sift_protocol::RunConfigurationId),
    #[error("run {0:?} not found")]
    RunNotFound(sift_protocol::RunId),
    #[error("run configuration revision conflict: expected {expected}, current {current}")]
    RunConfigurationRevisionConflict { expected: u64, current: u64 },
    #[error("run configuration is invalid")]
    InvalidRunConfiguration,
    #[error("run state transition is invalid")]
    InvalidRunTransition,
    #[error("run schedule {0:?} not found")]
    RunScheduleNotFound(sift_protocol::ScheduleId),
    #[error("run schedule revision conflict: expected {expected}, current {current}")]
    RunScheduleRevisionConflict { expected: u64, current: u64 },
    #[error("run schedule is invalid")]
    InvalidRunSchedule,
    #[error("transfer recipe {0:?} not found")]
    TransferRecipeNotFound(sift_protocol::TransferRecipeId),
    #[error("transfer recipe revision conflict: expected {expected}, current {current}")]
    TransferRecipeRevisionConflict { expected: u64, current: u64 },
    #[error("transfer recipe is invalid")]
    InvalidTransferRecipe,
    #[error("workspace artifact {0:?} not found")]
    WorkspaceArtifactNotFound(sift_protocol::WorkspaceArtifactId),
    #[error("catalog snapshot not found")]
    CatalogSnapshotNotFound,
    #[error("catalog snapshot revision conflict: expected {expected}, current {current}")]
    CatalogSnapshotRevisionConflict { expected: u64, current: u64 },
    #[error("catalog snapshot tenant limit reached")]
    CatalogSnapshotLimitReached,
    #[error("catalog snapshot payload exceeds the {limit}-byte limit")]
    CatalogSnapshotTooLarge { limit: usize },
    #[error("catalog snapshot description is invalid")]
    InvalidCatalogSnapshotDescription,
    #[error("migration run not found")]
    MigrationRunNotFound,
    #[error("migration run is already terminal")]
    MigrationRunTerminal,
    #[error("plan capture not found")]
    PlanCaptureNotFound,
    #[error("plan capture revision conflict: expected {expected}, current {current}")]
    PlanCaptureRevisionConflict { expected: u64, current: u64 },
    #[error("plan capture retention limit reached")]
    PlanCaptureLimitReached,
    #[error("plan capture payload exceeds the {limit}-byte limit")]
    PlanCaptureTooLarge { limit: usize },
    #[error("plan capture retention must be positive and may only lower the built-in ceilings")]
    InvalidPlanCaptureRetention,
    #[error(
        "extension {extension_id} version {version} already exists with a different archive digest"
    )]
    ExtensionVersionDigestConflict {
        extension_id: String,
        version: String,
    },
    #[error("extension {0} is not installed")]
    ExtensionNotFound(String),
    #[error("extension {0} has no previous package available for rollback")]
    ExtensionRollbackUnavailable(String),
    #[error("extension contribution id is already owned: {0}")]
    ExtensionContributionConflict(String),
    #[error("extension revision conflict: expected {expected}, current {current}")]
    ExtensionRevisionConflict { expected: u64, current: u64 },
    #[error("extension storage key is invalid")]
    ExtensionStorageInvalidKey,
    #[error("extension storage value exceeds the {limit}-byte limit")]
    ExtensionStorageValueTooLarge { limit: usize },
    #[error("extension storage quota exceeded: requested {requested}, limit {limit}")]
    ExtensionStorageQuotaExceeded { requested: u64, limit: u64 },
    #[error("extension storage revision conflict")]
    ExtensionStorageRevisionConflict,
    #[error("extension storage namespace not found")]
    ExtensionStorageNamespaceNotFound,
    #[error("operation approval is invalid, expired, consumed, or mismatched")]
    InvalidOperationApproval,
    #[error("tenant administrator access required")]
    TenantAdminRequired,
    #[error("tenant member access required")]
    TenantMemberRequired,
    #[error("instance administrator access required")]
    InstanceAdminRequired,
    #[error("room {0:?} not found")]
    RoomNotFound(RoomId),
    #[error("principal {principal:?} is not a member of tenant {tenant:?}")]
    TenantMembershipRequired {
        tenant: TenantId,
        principal: PrincipalId,
    },
    #[error("principal {principal:?} must own room {room:?}")]
    RoomOwnerRequired {
        room: RoomId,
        principal: PrincipalId,
    },
    #[error("room {0:?} must retain at least one owner")]
    FinalRoomOwner(RoomId),
    #[error("principal {principal:?} is not a member of room {room:?}")]
    RoomMemberNotFound {
        room: RoomId,
        principal: PrincipalId,
    },
    #[error("document {0:?} not found")]
    DocumentNotFound(DocumentId),
    #[error("room attachment {0:?} not found")]
    RoomAttachmentNotFound(RoomAttachmentId),
    #[error("saved query {0:?} not found")]
    SavedQueryNotFound(SavedQueryId),
    #[error("principal {0:?} not found")]
    PrincipalNotFound(PrincipalId),
    #[error("authentication identity {0:?} not found")]
    AuthIdentityNotFound(AuthIdentityId),
    #[error("authentication session not found: {0}")]
    AuthSessionNotFound(String),
    #[error("GitHub allowlist entry {0:?} not found")]
    GithubAllowlistNotFound(GithubAllowlistId),
    #[error("cannot disable the final active instance administrator")]
    FinalInstanceAdmin,
    #[error("cannot unlink the final active authentication identity")]
    FinalAuthIdentity,
    #[error("authentication token key has an invalid length")]
    InvalidAuthTokenKey,
    #[error("OAuth login attempt is invalid, expired, or already consumed")]
    InvalidOAuthAttempt,
    #[error("tenant invitation is invalid, expired, consumed, revoked, or intended for another principal")]
    InvalidTenantInvitation,
    #[error("principal key {0:?} not found or revoked")]
    PrincipalKeyNotFound(PrincipalKeyId),
    #[error("key challenge is invalid, expired, or consumed")]
    InvalidKeyChallenge,
    #[error(
        "SSH proxy capability is invalid, expired, consumed, revoked, or for another instance"
    )]
    InvalidSshProxyCapability,
    #[error("password reset token is invalid, expired, or already consumed")]
    InvalidPasswordReset,
    #[error("secret store error: {0}")]
    SecretStore(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("blocking metadata task failed: {0}")]
    BlockingTask(String),
    #[error("metadata schema migration required (current V{current}, latest V{latest}); run `sift-server migrate apply`")]
    MigrationRequired { current: u32, latest: u32 },
    #[error("metadata migration history is invalid: {0}")]
    InvalidMigrationHistory(String),
    #[error("automatic migration is blocked by V{version} ({name}), classified as {kind}")]
    AutomaticMigrationBlocked {
        version: u32,
        name: String,
        kind: MigrationKind,
    },
    #[error("metadata schema requires migration reader V{minimum}, but this binary supports through V{latest}; use a newer sift-server")]
    BinaryTooOld { minimum: u32, latest: u32 },
    #[error("another metadata migration process owns {0}")]
    MigrationInProgress(PathBuf),
    #[error("metadata migration lock does not belong to this store")]
    MigrationLockMismatch,
    #[error("operation requires a file-backed metadata store")]
    FileBackedStoreRequired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MigrationKind {
    Expand,
    Data,
    Contract,
    LegacyContract,
}

impl std::fmt::Display for MigrationKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Expand => f.write_str("expand"),
            Self::Data => f.write_str("data"),
            Self::Contract => f.write_str("contract"),
            Self::LegacyContract => f.write_str("legacy-contract"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct MigrationDescriptor {
    pub version: u32,
    pub name: String,
    pub kind: MigrationKind,
    pub automatic: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct MigrationStatus {
    pub current_version: u32,
    pub latest_version: u32,
    pub minimum_compatible_version: u32,
    pub pending: Vec<MigrationDescriptor>,
}

impl MigrationStatus {
    pub fn is_current(&self) -> bool {
        self.pending.is_empty() && self.minimum_compatible_version <= self.latest_version
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct MigrationReport {
    pub from_version: u32,
    pub to_version: u32,
    pub applied: Vec<MigrationDescriptor>,
    pub backup: Option<PathBuf>,
}

pub struct MigrationLockGuard {
    _file: Option<std::fs::File>,
    path: Option<PathBuf>,
}

/// Maximum idle connections the file-backed pool retains. Connections are
/// created on demand (checkout never blocks), but only this many are kept
/// warm; the rest are closed on check-in. Metadata calls run on Tokio's
/// bounded blocking pool, so live connections are naturally capped by that.
const MAX_IDLE_CONNECTIONS: usize = 16;

/// A tiny SQLite connection pool for file-backed stores. In WAL mode multiple
/// connections read concurrently and writers serialize via `busy_timeout`, so
/// spreading metadata calls across connections lifts the single-mutex
/// serialization ceiling (P1-meta-1). The `idle` mutex is held only to pop or
/// push a connection, never across a query.
struct ConnectionPool {
    path: PathBuf,
    idle: Mutex<Vec<Connection>>,
}

impl ConnectionPool {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            idle: Mutex::new(Vec::new()),
        }
    }

    /// Take a warm connection or open a fresh one. Never blocks on other
    /// callers beyond the brief `idle` lock.
    fn checkout(self: &Arc<Self>) -> Result<PooledConn> {
        let reused = self.idle.lock().unwrap().pop();
        let conn = match reused {
            Some(conn) => conn,
            None => {
                if let Some(parent) = self.path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let conn = Connection::open(&self.path)?;
                configure_connection(&conn)?;
                conn
            }
        };
        Ok(PooledConn {
            conn: Some(conn),
            pool: Arc::clone(self),
        })
    }

    fn checkin(&self, conn: Connection) {
        let mut idle = self.idle.lock().unwrap();
        if idle.len() < MAX_IDLE_CONNECTIONS {
            idle.push(conn);
        }
        // Otherwise drop `conn`, closing it.
    }

    fn clear_idle(&self) {
        self.idle.lock().unwrap().clear();
    }
}

/// A connection borrowed from a [`ConnectionPool`]. Returned to the pool on
/// drop. Derefs to [`Connection`] so call sites use it like a plain handle.
struct PooledConn {
    conn: Option<Connection>,
    pool: Arc<ConnectionPool>,
}

impl Drop for PooledConn {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            self.pool.checkin(conn);
        }
    }
}

impl Deref for PooledConn {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.conn.as_ref().expect("connection present until drop")
    }
}

impl DerefMut for PooledConn {
    fn deref_mut(&mut self) -> &mut Connection {
        self.conn.as_mut().expect("connection present until drop")
    }
}

/// Backing store for a [`MetadataStore`]. File-backed stores use a WAL
/// connection pool; in-memory stores keep a single connection behind a mutex
/// (a second `open_in_memory` is a different empty DB, so it cannot be pooled).
#[derive(Clone)]
enum Backend {
    Pool(Arc<ConnectionPool>),
    Memory(Arc<Mutex<Connection>>),
}

impl Backend {
    /// Borrow a connection for one operation. Pooled connections return to the
    /// pool when the handle drops; the in-memory guard releases its mutex.
    fn conn(&self) -> Result<ConnHandle<'_>> {
        match self {
            Backend::Pool(pool) => Ok(ConnHandle::Pooled(pool.checkout()?)),
            Backend::Memory(conn) => Ok(ConnHandle::Memory(conn.lock().unwrap())),
        }
    }
}

/// A connection handle over either backend, deref-able to [`Connection`] so
/// the ~45 call sites are backend-agnostic.
enum ConnHandle<'a> {
    Pooled(PooledConn),
    Memory(MutexGuard<'a, Connection>),
    Owned(Connection),
}

impl Deref for ConnHandle<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        match self {
            ConnHandle::Pooled(conn) => conn,
            ConnHandle::Memory(conn) => conn,
            ConnHandle::Owned(conn) => conn,
        }
    }
}

impl DerefMut for ConnHandle<'_> {
    fn deref_mut(&mut self) -> &mut Connection {
        match self {
            ConnHandle::Pooled(conn) => conn,
            ConnHandle::Memory(conn) => conn,
            ConnHandle::Owned(conn) => conn,
        }
    }
}

/// Cheap shared handle. Clones share the same connection pool/in-memory
/// database and secret store; cloning never snapshots metadata state.
#[derive(Clone)]
pub struct MetadataStore {
    backend: Backend,
    secrets: Arc<dyn SecretStore>,
    plan_capture_retention: Arc<std::sync::RwLock<PlanCaptureRetention>>,
}

impl MetadataStore {
    pub fn open(path: &Path, secrets: Arc<dyn SecretStore>) -> Result<Self> {
        let pool = Arc::new(ConnectionPool::new(path.to_path_buf()));
        Ok(Self {
            backend: Backend::Pool(pool),
            secrets,
            plan_capture_retention: Arc::new(std::sync::RwLock::new(
                PlanCaptureRetention::default(),
            )),
        })
    }

    pub fn open_in_memory(secrets: Arc<dyn SecretStore>) -> Result<Self> {
        let mut conn = Connection::open_in_memory()?;
        configure_connection(&conn)?;
        migrations::migrations::runner().run(&mut conn)?;
        Ok(Self {
            backend: Backend::Memory(Arc::new(Mutex::new(conn))),
            secrets,
            plan_capture_retention: Arc::new(std::sync::RwLock::new(
                PlanCaptureRetention::default(),
            )),
        })
    }

    /// Borrow a connection for a single operation. See [`Backend::conn`].
    fn conn(&self) -> Result<ConnHandle<'_>> {
        self.backend.conn()
    }

    pub fn migration_status(&self) -> Result<MigrationStatus> {
        let runner = migrations::migrations::runner();
        let mut embedded = runner.get_migrations().to_vec();
        embedded.sort_by_key(refinery::Migration::version);
        let latest_version = embedded.last().map_or(0, refinery::Migration::version);
        for migration in &embedded {
            migration_kind(migration.version())?;
        }

        if matches!(&self.backend, Backend::Pool(pool) if !pool.path.exists()) {
            return Ok(MigrationStatus {
                current_version: 0,
                latest_version,
                minimum_compatible_version: 0,
                pending: embedded
                    .iter()
                    .map(migration_descriptor)
                    .collect::<Result<Vec<_>>>()?,
            });
        }

        let mut conn = match &self.backend {
            Backend::Pool(pool) => ConnHandle::Owned(Connection::open_with_flags(
                &pool.path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                    | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?),
            Backend::Memory(connection) => ConnHandle::Memory(connection.lock().unwrap()),
        };
        let history_exists = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'refinery_schema_history')",
            [],
            |row| row.get::<_, bool>(0),
        )?;
        let applied = if history_exists {
            runner.get_applied_migrations(&mut *conn)?
        } else {
            Vec::new()
        };

        for (index, actual) in applied.iter().enumerate() {
            let expected_version = u32::try_from(index).unwrap_or(u32::MAX) + 1;
            if actual.version() != expected_version {
                return Err(MetadataError::InvalidMigrationHistory(format!(
                    "expected migration V{expected_version}, found V{}",
                    actual.version()
                )));
            }
            let Some(expected) = embedded.get(index) else {
                continue;
            };
            if actual.version() != expected.version()
                || actual.name() != expected.name()
                || actual.checksum() != expected.checksum()
            {
                return Err(MetadataError::InvalidMigrationHistory(format!(
                    "database entry V{} ({}, checksum {}) does not match embedded V{} ({}, checksum {})",
                    actual.version(),
                    actual.name(),
                    actual.checksum(),
                    expected.version(),
                    expected.name(),
                    expected.checksum()
                )));
            }
        }

        let current_version = applied.last().map_or(0, refinery::Migration::version);
        let known_applied = applied.len().min(embedded.len());
        let pending = embedded[known_applied..]
            .iter()
            .map(migration_descriptor)
            .collect::<Result<Vec<_>>>()?;
        let minimum_compatible_version =
            conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        Ok(MigrationStatus {
            current_version,
            latest_version,
            minimum_compatible_version,
            pending,
        })
    }

    pub fn ensure_schema_current(&self) -> Result<()> {
        let status = self.migration_status()?;
        if status.minimum_compatible_version > status.latest_version {
            Err(MetadataError::BinaryTooOld {
                minimum: status.minimum_compatible_version,
                latest: status.latest_version,
            })
        } else if status.is_current() {
            Ok(())
        } else {
            Err(MetadataError::MigrationRequired {
                current: status.current_version,
                latest: status.latest_version,
            })
        }
    }

    pub fn apply_migrations(&self, automatic: bool) -> Result<MigrationReport> {
        let migration_lock = self.lock_migrations()?;
        self.apply_migrations_locked(automatic, &migration_lock)
    }

    pub fn apply_migrations_locked(
        &self,
        automatic: bool,
        migration_lock: &MigrationLockGuard,
    ) -> Result<MigrationReport> {
        let store_path = match &self.backend {
            Backend::Pool(pool) => Some(pool.path.clone()),
            Backend::Memory(_) => None,
        };
        if migration_lock.path != store_path {
            return Err(MetadataError::MigrationLockMismatch);
        }
        let status = self.migration_status()?;
        if status.minimum_compatible_version > status.latest_version {
            return Err(MetadataError::BinaryTooOld {
                minimum: status.minimum_compatible_version,
                latest: status.latest_version,
            });
        }
        if automatic && status.current_version != 0 {
            if let Some(blocked) = status.pending.iter().find(|item| !item.automatic) {
                return Err(MetadataError::AutomaticMigrationBlocked {
                    version: blocked.version,
                    name: blocked.name.clone(),
                    kind: blocked.kind,
                });
            }
        }
        if status.pending.is_empty() {
            return Ok(MigrationReport {
                from_version: status.current_version,
                to_version: status.current_version,
                applied: Vec::new(),
                backup: None,
            });
        }

        let backup = self.create_migration_backup(status.current_version)?;
        if let Backend::Pool(pool) = &self.backend {
            pool.clear_idle();
        }
        {
            let mut conn = self.conn()?;
            migrations::migrations::runner().run(&mut *conn)?;
            if status.current_version != 0 {
                let contract_floor = status
                    .pending
                    .iter()
                    .filter(|migration| migration.kind == MigrationKind::Contract)
                    .map(|migration| migration.version)
                    .max()
                    .unwrap_or(status.minimum_compatible_version)
                    .max(status.minimum_compatible_version);
                if contract_floor > status.minimum_compatible_version {
                    conn.pragma_update(None, "user_version", contract_floor)?;
                }
            }
        }
        if let Backend::Pool(pool) = &self.backend {
            pool.clear_idle();
        }
        let after = self.migration_status()?;
        Ok(MigrationReport {
            from_version: status.current_version,
            to_version: after.current_version,
            applied: status.pending,
            backup,
        })
    }

    pub fn create_migration_backup(&self, version: u32) -> Result<Option<PathBuf>> {
        let Backend::Pool(pool) = &self.backend else {
            return Ok(None);
        };
        let conn = pool.checkout()?;
        let user_table_count: u32 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get(0),
        )?;
        if user_table_count == 0 {
            return Ok(None);
        }

        let parent = pool.path.parent().unwrap_or_else(|| Path::new("."));
        let backup_dir = parent.join("backups");
        std::fs::create_dir_all(&backup_dir)?;
        let timestamp = Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
        let path = backup_dir.join(format!(
            "metadata-v{version}-{timestamp}-{}.sqlite",
            Uuid::new_v4().simple()
        ));
        let partial = backup_dir.join(format!(".metadata-backup-{}.partial", Uuid::new_v4()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        drop(options.open(&partial)?);
        let backup_result = (|| -> Result<()> {
            let mut destination = Connection::open(&partial)?;
            {
                let backup = rusqlite::backup::Backup::new(&conn, &mut destination)?;
                backup.run_to_completion(128, Duration::from_millis(10), None)?;
            }
            destination.close().map_err(|(_, error)| error)?;
            std::fs::File::open(&partial)?.sync_all()?;
            std::fs::rename(&partial, &path)?;
            #[cfg(unix)]
            std::fs::File::open(&backup_dir)?.sync_all()?;
            Ok(())
        })();
        if let Err(error) = backup_result {
            let _ = std::fs::remove_file(&partial);
            return Err(error);
        }
        Ok(Some(path))
    }

    pub fn backup_database_to(&self, destination: &Path) -> Result<()> {
        let Backend::Pool(pool) = &self.backend else {
            return Err(MetadataError::FileBackedStoreRequired);
        };
        if destination.exists() {
            return Err(MetadataError::Io(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("backup destination exists: {}", destination.display()),
            )));
        }
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let source = Connection::open_with_flags(
            &pool.path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        drop(options.open(destination)?);
        let result = (|| -> Result<()> {
            let mut target = Connection::open(destination)?;
            {
                let backup = rusqlite::backup::Backup::new(&source, &mut target)?;
                backup.run_to_completion(128, Duration::from_millis(10), None)?;
            }
            target.close().map_err(|(_, error)| error)?;
            std::fs::File::open(destination)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(destination);
        }
        result
    }

    pub fn integrity_check(&self) -> Result<()> {
        let conn = self.conn()?;
        let result: String = conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if result == "ok" {
            Ok(())
        } else {
            Err(MetadataError::InvalidMigrationHistory(format!(
                "SQLite integrity check failed: {result}"
            )))
        }
    }

    /// Strip process-local runtime state from an offline backup snapshot.
    /// Definitions and completed history remain durable, while restoring the
    /// archive cannot resume a checkout, credential, lease, artifact, or
    /// in-flight database operation.
    pub fn sanitize_phase_l_backup_snapshot(&self) -> Result<()> {
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE repository_binding SET credential_handle = NULL
             WHERE credential_handle IS NOT NULL",
            [],
        )?;
        tx.execute("DELETE FROM repository_principal_credential", [])?;
        tx.execute("DELETE FROM workspace_artifact", [])?;
        tx.execute(
            "UPDATE run_step_result SET state = 'cancelled', finished_at = COALESCE(finished_at, ?1)
             WHERE state IN ('pending', 'running')",
            params![&now],
        )?;
        tx.execute(
            "UPDATE run_execution SET state = 'outcome_unknown', cancellation_requested = 0,
             finished_at = COALESCE(finished_at, ?1), revision = revision + 1
             WHERE state IN ('queued', 'admitted', 'preparing', 'running')",
            params![&now],
        )?;
        tx.execute(
            "UPDATE schedule_occurrence SET
                 state = CASE WHEN state IN ('leased', 'running') THEN 'outcome_unknown' ELSE state END,
                 error_code = CASE WHEN state IN ('leased', 'running') THEN 'backup_interrupted' ELSE error_code END,
                 finished_at = CASE WHEN state IN ('leased', 'running') THEN COALESCE(finished_at, ?1) ELSE finished_at END,
                 lease_owner = NULL, lease_expires_at = NULL",
            params![&now],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Make restored durable state safe under the destination installation's
    /// identity. Durable principals and credentials remain; bearer and
    /// one-use authentication material does not.
    fn sanitize_restored_database(&self) -> Result<()> {
        let now = now_text();
        {
            let mut conn = self.conn()?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute("DELETE FROM auth_session", [])?;
            tx.execute("DELETE FROM oauth_login_attempt", [])?;
            tx.execute("DELETE FROM password_reset_token", [])?;
            tx.execute("DELETE FROM keypair_challenge", [])?;
            tx.execute("DELETE FROM ssh_proxy_capability", [])?;
            tx.execute(
                "UPDATE tenant_invitation SET revoked_at = COALESCE(revoked_at, ?1)",
                params![now],
            )?;
            tx.execute(
                "UPDATE api_token SET revoked_at = COALESCE(revoked_at, ?1), updated_at = ?1",
                params![now],
            )?;
            insert_operation_audit_row(
                &tx,
                &NewOperationAudit {
                    actor_principal_id: None,
                    action: "backup.restore".to_string(),
                    target: "instance_state".to_string(),
                    target_id: None,
                    status: "succeeded".to_string(),
                    result_code: None,
                    row_count: None,
                    error_message: None,
                    correlation_id: None,
                },
            )?;
            tx.commit()?;
        }
        Ok(())
    }

    pub async fn rotate_auth_system_keys(&self) -> Result<()> {
        self.secrets
            .delete(AUTH_SYSTEM_SECRET_NAMESPACE, AUTH_TOKEN_MAC_HANDLE)
            .await?;
        self.secrets
            .delete(
                AUTH_SYSTEM_SECRET_NAMESPACE,
                SSH_PROXY_CAPABILITY_MAC_HANDLE,
            )
            .await?;
        Ok(())
    }

    pub async fn sanitize_after_restore(&self) -> Result<()> {
        // OAuth PKCE verifiers are deliberately stored outside SQLite. Capture
        // and remove their opaque handles before deleting the rows that make
        // those secrets discoverable. If secret deletion fails, leave the
        // database rows intact so a later restore attempt can safely retry.
        let oauth_verifier_handles = {
            let conn = self.conn()?;
            let mut statement =
                conn.prepare("SELECT pkce_verifier_handle FROM oauth_login_attempt")?;
            let handles = rows(statement.query_map([], |row| row.get::<_, String>(0))?)?;
            handles
        };
        for handle in oauth_verifier_handles {
            self.secrets.delete(OAUTH_SECRET_NAMESPACE, &handle).await?;
        }
        self.sanitize_restored_database()?;
        self.rotate_auth_system_keys().await
    }

    pub fn lock_migrations(&self) -> Result<MigrationLockGuard> {
        let Backend::Pool(pool) = &self.backend else {
            return Ok(MigrationLockGuard {
                _file: None,
                path: None,
            });
        };
        let parent = pool.path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent)?;
        let file_name = pool
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("metadata.sqlite");
        let lock_path = parent.join(format!("{file_name}.migrate.lock"));
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options.open(&lock_path)?;
        match lock.try_lock_exclusive() {
            Ok(()) => Ok(MigrationLockGuard {
                _file: Some(lock),
                path: Some(pool.path.clone()),
            }),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                Err(MetadataError::MigrationInProgress(lock_path))
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn require_minimum_compatible_version(&self, version: u32) -> Result<()> {
        let conn = self.conn()?;
        let current: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version > current {
            conn.pragma_update(None, "user_version", version)?;
        }
        Ok(())
    }

    pub fn default_local_path() -> PathBuf {
        if cfg!(target_os = "macos") {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            return home
                .join("Library")
                .join("Application Support")
                .join("sift")
                .join("metadata.sqlite");
        }

        let state = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|home| PathBuf::from(home).join(".local").join("state"))
            })
            .unwrap_or_else(|| PathBuf::from("."));
        state.join("sift").join("metadata.sqlite")
    }

    /// Cheap reachability probe for readiness checks: runs `SELECT 1` against
    /// the store. Returns an error if the connection is poisoned or the query
    /// fails.
    pub fn health_check(&self) -> Result<()> {
        let conn = self.conn()?;
        conn.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))?;
        Ok(())
    }

    pub fn bootstrap_local(&self, display_name: &str) -> Result<()> {
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let tenant_count: i64 =
            tx.query_row("SELECT COUNT(*) FROM tenant", [], |row| row.get(0))?;
        let principal_count: i64 =
            tx.query_row("SELECT COUNT(*) FROM principal", [], |row| row.get(0))?;
        if tenant_count != 0 || principal_count != 0 {
            return Ok(());
        }
        tx.execute(
            "INSERT INTO tenant (id, name, kind, created_at, updated_at) VALUES (1, 'local', 'personal', ?1, ?1)",
            params![now],
        )?;
        tx.execute(
            "INSERT INTO principal (id, external_id, display_name, email, created_at, updated_at)
             VALUES (1, 'local:1', ?1, NULL, ?2, ?2)",
            params![display_name, now],
        )?;
        tx.execute(
            "INSERT INTO auth_identity
             (principal_id, method, issuer, subject, created_at, updated_at)
             VALUES (1, 'local_bypass', 'sift', 'local:1', ?1, ?1)",
            params![now],
        )?;
        tx.execute(
            "INSERT INTO membership (tenant_id, principal_id, role, created_at, updated_at)
             VALUES (1, 1, 'owner', ?1, ?1)",
            params![now],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn resolve_principal_by_external_id(&self, external_id: &str) -> Result<Option<Principal>> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT id, external_id, display_name, email, avatar_url, disabled_at,
                    is_instance_admin, created_at, updated_at
             FROM principal WHERE external_id = ?1",
            params![external_id],
            principal_from_row,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn principal_by_id(&self, principal: PrincipalId) -> Result<Option<Principal>> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT id, external_id, display_name, email, avatar_url, disabled_at,
                    is_instance_admin, created_at, updated_at
             FROM principal WHERE id = ?1",
            params![principal.0],
            principal_from_row,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn create_principal(
        &self,
        external_id: &str,
        display_name: &str,
        email: Option<&str>,
    ) -> Result<Principal> {
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO principal (external_id, display_name, email, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            params![external_id, display_name, email, now],
        )?;
        let id = PrincipalId(tx.last_insert_rowid());
        tx.execute(
            "INSERT INTO auth_identity
             (principal_id, method, issuer, subject, created_at, updated_at)
             VALUES (?1, 'legacy', 'sift', ?2, ?3, ?3)",
            params![id.0, external_id, now],
        )?;
        let principal = tx.query_row(
            "SELECT id, external_id, display_name, email, avatar_url, disabled_at,
                    is_instance_admin, created_at, updated_at
             FROM principal WHERE id = ?1",
            params![id.0],
            principal_from_row,
        )?;
        tx.commit()?;
        Ok(principal)
    }

    pub fn list_auth_identities(&self, principal: PrincipalId) -> Result<Vec<AuthIdentity>> {
        let conn = self.conn()?;
        let mut statement = conn.prepare(
            "SELECT id, principal_id, method, issuer, subject, provider_login,
                    credential_handle, created_at, updated_at, last_used_at, disabled_at
             FROM auth_identity WHERE principal_id = ?1 ORDER BY id",
        )?;
        let rows = statement.query_map(params![principal.0], auth_identity_from_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn has_active_instance_admin(&self) -> Result<bool> {
        let conn = self.conn()?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM principal
             WHERE is_instance_admin = 1 AND disabled_at IS NULL",
            [],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Resolve the deterministic local-console principal for a personal
    /// instance. Used only behind the verified loopback/OS trust boundary.
    pub fn local_instance_admin(&self) -> Result<Option<Principal>> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT id, external_id, display_name, email, avatar_url, disabled_at,
                    is_instance_admin, created_at, updated_at
             FROM principal
             WHERE is_instance_admin = 1 AND disabled_at IS NULL
             ORDER BY id LIMIT 1",
            [],
            principal_from_row,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Atomically creates the stable principal, its personal tenant and owner
    /// membership, its password identity, and the sanitized administration
    /// audit row. `password_verifier` is already an Argon2id verifier; it is
    /// persisted only in `SecretStore`, never in SQLite.
    pub async fn create_password_principal(
        &self,
        input: NewPasswordPrincipal<'_>,
        password_verifier: &[u8],
        audit: NewOperationAudit,
    ) -> Result<Principal> {
        let handle = Uuid::new_v4().to_string();
        self.secrets
            .put(PASSWORD_SECRET_NAMESPACE, &handle, password_verifier)
            .await?;

        let now = now_text();
        let external_id = format!("principal:{}", Uuid::new_v4());
        let username = input.username.to_string();
        let display_name = input.display_name.to_string();
        let email = input.email.map(str::to_string);
        let is_instance_admin = input.is_instance_admin;
        let backend = self.backend.clone();
        let db_handle = handle.clone();
        let result = sqlite_blocking(move || {
            let mut conn = backend.conn()?;
            let tx = conn.transaction()?;
            tx.execute(
                "INSERT INTO principal
                 (external_id, display_name, email, is_instance_admin, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![external_id, display_name, email, is_instance_admin, now],
            )?;
            let principal_id = PrincipalId(tx.last_insert_rowid());
            tx.execute(
                "INSERT INTO tenant (name, kind, created_at, updated_at)
                 VALUES (?1, 'personal', ?2, ?2)",
                params![username, now],
            )?;
            let tenant_id = TenantId(tx.last_insert_rowid());
            tx.execute(
                "INSERT INTO membership
                 (tenant_id, principal_id, role, created_at, updated_at)
                 VALUES (?1, ?2, 'owner', ?3, ?3)",
                params![tenant_id.0, principal_id.0, now],
            )?;
            tx.execute(
                "INSERT INTO auth_identity
                 (principal_id, method, issuer, subject, credential_handle,
                  created_at, updated_at)
                 VALUES (?1, 'password', 'sift', ?2, ?3, ?4, ?4)",
                params![principal_id.0, username, db_handle, now],
            )?;
            let mut audit = audit;
            audit.target_id = Some(principal_id.0);
            insert_operation_audit_row(&tx, &audit)?;
            let principal = tx.query_row(
                "SELECT id, external_id, display_name, email, avatar_url, disabled_at,
                        is_instance_admin, created_at, updated_at
                 FROM principal WHERE id = ?1",
                params![principal_id.0],
                principal_from_row,
            )?;
            tx.commit()?;
            Ok(principal)
        })
        .await;

        if result.is_err() {
            self.delete_password_secret_best_effort(&handle, "create_password_principal_rollback")
                .await;
        }
        result
    }

    pub fn resolve_password_identity(&self, username: &str) -> Result<Option<PasswordIdentity>> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT ai.id, ai.principal_id, ai.method, ai.issuer, ai.subject,
                    ai.provider_login, ai.credential_handle, ai.created_at,
                    ai.updated_at, ai.last_used_at, ai.disabled_at,
                    p.id, p.external_id, p.display_name, p.email, p.avatar_url,
                    p.disabled_at, p.is_instance_admin, p.created_at, p.updated_at
             FROM auth_identity ai
             JOIN principal p ON p.id = ai.principal_id
             WHERE ai.method = 'password' AND ai.issuer = 'sift' AND ai.subject = ?1",
            params![username],
            |row| {
                Ok(PasswordIdentity {
                    identity: auth_identity_from_row(row)?,
                    principal: principal_from_row_offset(row, 11)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn password_identity_for_principal(
        &self,
        principal: PrincipalId,
    ) -> Result<Option<PasswordIdentity>> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT ai.id, ai.principal_id, ai.method, ai.issuer, ai.subject,
                    ai.provider_login, ai.credential_handle, ai.created_at,
                    ai.updated_at, ai.last_used_at, ai.disabled_at,
                    p.id, p.external_id, p.display_name, p.email, p.avatar_url,
                    p.disabled_at, p.is_instance_admin, p.created_at, p.updated_at
             FROM auth_identity ai
             JOIN principal p ON p.id = ai.principal_id
             WHERE ai.method = 'password' AND ai.issuer = 'sift'
               AND ai.principal_id = ?1 AND ai.disabled_at IS NULL
             ORDER BY ai.id LIMIT 1",
            params![principal.0],
            |row| {
                Ok(PasswordIdentity {
                    identity: auth_identity_from_row(row)?,
                    principal: principal_from_row_offset(row, 11)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub async fn password_verifier(&self, identity: &AuthIdentity) -> Result<Option<Vec<u8>>> {
        let Some(handle) = identity.credential_handle.as_deref() else {
            return Ok(None);
        };
        self.secrets.get(PASSWORD_SECRET_NAMESPACE, handle).await
    }

    pub fn create_github_allowlist_entry(
        &self,
        normalized_login: &str,
        target_principal: Option<PrincipalId>,
        actor: PrincipalId,
        audit: NewOperationAudit,
    ) -> Result<GithubAllowlistEntry> {
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO github_allowlist
             (normalized_login, target_principal_id, created_by, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            params![
                normalized_login,
                target_principal.map(|id| id.0),
                actor.0,
                now
            ],
        )?;
        let id = GithubAllowlistId(tx.last_insert_rowid());
        insert_operation_audit_row(&tx, &audit)?;
        let entry = tx.query_row(
            "SELECT id, normalized_login, target_principal_id, created_by,
                    created_at, updated_at, consumed_at, revoked_at
             FROM github_allowlist WHERE id = ?1",
            params![id.0],
            github_allowlist_from_row,
        )?;
        tx.commit()?;
        Ok(entry)
    }

    pub fn list_github_allowlist_entries(&self) -> Result<Vec<GithubAllowlistEntry>> {
        let conn = self.conn()?;
        let mut statement = conn.prepare(
            "SELECT id, normalized_login, target_principal_id, created_by,
                    created_at, updated_at, consumed_at, revoked_at
             FROM github_allowlist ORDER BY created_at DESC, id DESC",
        )?;
        let entries = rows(statement.query_map([], github_allowlist_from_row)?)?;
        Ok(entries)
    }

    pub fn revoke_github_allowlist_entry(
        &self,
        id: GithubAllowlistId,
        audit: NewOperationAudit,
    ) -> Result<()> {
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE github_allowlist SET revoked_at = COALESCE(revoked_at, ?1), updated_at = ?1
             WHERE id = ?2 AND consumed_at IS NULL",
            params![now, id.0],
        )?;
        if changed == 0 {
            return Err(MetadataError::GithubAllowlistNotFound(id));
        }
        insert_operation_audit_row(&tx, &audit)?;
        tx.commit()?;
        Ok(())
    }

    /// Resolve an immutable GitHub id or atomically consume the matching
    /// normalized-login allowlist entry. New identities receive the same
    /// principal + personal-tenant shape as password-created users.
    pub fn complete_github_identity(
        &self,
        profile: GithubProfile,
        audit: NewOperationAudit,
    ) -> Result<Option<Principal>> {
        let now = now_text();
        let subject = profile.id.to_string();
        let normalized_login = profile.login.to_ascii_lowercase();
        let display_name = profile
            .display_name
            .as_deref()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(&profile.login)
            .to_string();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<PrincipalId> = tx
            .query_row(
                "SELECT principal_id FROM auth_identity
                 WHERE method = 'github' AND issuer = 'https://github.com' AND subject = ?1",
                params![subject],
                |row| row.get::<_, i64>(0).map(PrincipalId),
            )
            .optional()?;
        let principal_id = if let Some(principal_id) = existing {
            tx.execute(
                "UPDATE auth_identity
                 SET provider_login = ?1, last_used_at = ?2, updated_at = ?2
                 WHERE method = 'github' AND issuer = 'https://github.com' AND subject = ?3",
                params![profile.login, now, subject],
            )?;
            principal_id
        } else {
            let pending: Option<(GithubAllowlistId, Option<PrincipalId>)> = tx
                .query_row(
                    "SELECT id, target_principal_id FROM github_allowlist
                     WHERE normalized_login = ?1 AND consumed_at IS NULL AND revoked_at IS NULL",
                    params![normalized_login],
                    |row| {
                        Ok((
                            GithubAllowlistId(row.get(0)?),
                            row.get::<_, Option<i64>>(1)?.map(PrincipalId),
                        ))
                    },
                )
                .optional()?;
            let Some((allowlist_id, target)) = pending else {
                return Ok(None);
            };
            let principal_id = if let Some(target) = target {
                target
            } else {
                tx.execute(
                    "INSERT INTO principal
                     (external_id, display_name, email, avatar_url, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                    params![
                        format!("principal:{}", Uuid::new_v4()),
                        display_name,
                        profile.email,
                        profile.avatar_url,
                        now
                    ],
                )?;
                let created = PrincipalId(tx.last_insert_rowid());
                tx.execute(
                    "INSERT INTO tenant (name, kind, created_at, updated_at)
                     VALUES (?1, 'personal', ?2, ?2)",
                    params![profile.login, now],
                )?;
                let tenant = TenantId(tx.last_insert_rowid());
                tx.execute(
                    "INSERT INTO membership
                     (tenant_id, principal_id, role, created_at, updated_at)
                     VALUES (?1, ?2, 'owner', ?3, ?3)",
                    params![tenant.0, created.0, now],
                )?;
                created
            };
            tx.execute(
                "INSERT INTO auth_identity
                 (principal_id, method, issuer, subject, provider_login,
                  created_at, updated_at, last_used_at)
                 VALUES (?1, 'github', 'https://github.com', ?2, ?3, ?4, ?4, ?4)",
                params![principal_id.0, subject, profile.login, now],
            )?;
            tx.execute(
                "UPDATE github_allowlist SET consumed_at = ?1, updated_at = ?1 WHERE id = ?2",
                params![now, allowlist_id.0],
            )?;
            principal_id
        };
        tx.execute(
            "UPDATE principal SET display_name = ?1,
                    email = COALESCE(?2, email), avatar_url = COALESCE(?3, avatar_url),
                    updated_at = ?4
             WHERE id = ?5 AND disabled_at IS NULL",
            params![
                display_name,
                profile.email,
                profile.avatar_url,
                now,
                principal_id.0
            ],
        )?;
        let mut audit = audit;
        audit.actor_principal_id = Some(principal_id);
        insert_operation_audit_row(&tx, &audit)?;
        let principal = tx
            .query_row(
                "SELECT id, external_id, display_name, email, avatar_url, disabled_at,
                    is_instance_admin, created_at, updated_at
             FROM principal WHERE id = ?1 AND disabled_at IS NULL",
                params![principal_id.0],
                principal_from_row,
            )
            .optional()?;
        tx.commit()?;
        Ok(principal)
    }

    /// Disablement is principal-wide: all linked identities and interactive
    /// sessions are revoked in the same transaction as the audit record.
    pub fn set_principal_disabled(
        &self,
        principal: PrincipalId,
        disabled: bool,
        audit: NewOperationAudit,
    ) -> Result<()> {
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let is_admin: Option<bool> = tx
            .query_row(
                "SELECT is_instance_admin FROM principal WHERE id = ?1",
                params![principal.0],
                |row| row.get(0),
            )
            .optional()?;
        let Some(is_admin) = is_admin else {
            return Err(MetadataError::PrincipalNotFound(principal));
        };
        if disabled && is_admin {
            let other_admins: i64 = tx.query_row(
                "SELECT COUNT(*) FROM principal
                 WHERE is_instance_admin = 1 AND disabled_at IS NULL AND id != ?1",
                params![principal.0],
                |row| row.get(0),
            )?;
            if other_admins == 0 {
                return Err(MetadataError::FinalInstanceAdmin);
            }
        }
        let disabled_at = disabled.then_some(now.as_str());
        tx.execute(
            "UPDATE principal SET disabled_at = ?1, updated_at = ?2 WHERE id = ?3",
            params![disabled_at, now, principal.0],
        )?;
        tx.execute(
            "UPDATE auth_identity SET disabled_at = ?1, updated_at = ?2
             WHERE principal_id = ?3",
            params![disabled_at, now, principal.0],
        )?;
        if disabled {
            tx.execute(
                "UPDATE auth_session
                 SET revoked_at = COALESCE(revoked_at, ?1),
                     revocation_reason = COALESCE(revocation_reason, 'principal_disabled')
                 WHERE principal_id = ?2",
                params![now, principal.0],
            )?;
        }
        insert_operation_audit_row(&tx, &audit)?;
        tx.commit()?;
        Ok(())
    }

    pub async fn replace_password_verifier(
        &self,
        identity: AuthIdentityId,
        password_verifier: &[u8],
        audit: NewOperationAudit,
    ) -> Result<()> {
        let new_handle = Uuid::new_v4().to_string();
        self.secrets
            .put(PASSWORD_SECRET_NAMESPACE, &new_handle, password_verifier)
            .await?;
        let now = now_text();
        let backend = self.backend.clone();
        let db_handle = new_handle.clone();
        let result = sqlite_blocking(move || {
            let mut conn = backend.conn()?;
            let tx = conn.transaction()?;
            let old_handle: Option<String> = tx
                .query_row(
                    "SELECT credential_handle FROM auth_identity
                     WHERE id = ?1 AND method = 'password'",
                    params![identity.0],
                    |row| row.get(0),
                )
                .optional()?
                .flatten();
            let Some(old_handle) = old_handle else {
                return Err(MetadataError::AuthIdentityNotFound(identity));
            };
            tx.execute(
                "UPDATE auth_identity
                 SET credential_handle = ?1, updated_at = ?2, disabled_at = NULL
                 WHERE id = ?3",
                params![db_handle, now, identity.0],
            )?;
            tx.execute(
                "UPDATE auth_session
                 SET revoked_at = COALESCE(revoked_at, ?1),
                     revocation_reason = COALESCE(revocation_reason, 'password_changed')
                 WHERE principal_id = (
                    SELECT principal_id FROM auth_identity WHERE id = ?2
                 )",
                params![now, identity.0],
            )?;
            insert_operation_audit_row(&tx, &audit)?;
            tx.commit()?;
            Ok(old_handle)
        })
        .await;
        match result {
            Ok(old_handle) => {
                self.delete_password_secret_best_effort(
                    &old_handle,
                    "replace_password_verifier_old",
                )
                .await;
                Ok(())
            }
            Err(error) => {
                self.delete_password_secret_best_effort(
                    &new_handle,
                    "replace_password_verifier_rollback",
                )
                .await;
                Err(error)
            }
        }
    }

    pub async fn link_password_identity(
        &self,
        principal: PrincipalId,
        username: &str,
        password_verifier: &[u8],
        audit: NewOperationAudit,
    ) -> Result<AuthIdentity> {
        let handle = Uuid::new_v4().to_string();
        self.secrets
            .put(PASSWORD_SECRET_NAMESPACE, &handle, password_verifier)
            .await?;
        let now = now_text();
        let backend = self.backend.clone();
        let username = username.to_string();
        let db_handle = handle.clone();
        let result = sqlite_blocking(move || {
            let mut conn = backend.conn()?;
            let tx = conn.transaction()?;
            if tx
                .query_row(
                    "SELECT id FROM principal WHERE id = ?1 AND disabled_at IS NULL",
                    params![principal.0],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .is_none()
            {
                return Err(MetadataError::PrincipalNotFound(principal));
            }
            tx.execute(
                "INSERT INTO auth_identity
                 (principal_id, method, issuer, subject, credential_handle,
                  created_at, updated_at)
                 VALUES (?1, 'password', 'sift', ?2, ?3, ?4, ?4)",
                params![principal.0, username, db_handle, now],
            )?;
            let id = AuthIdentityId(tx.last_insert_rowid());
            insert_operation_audit_row(&tx, &audit)?;
            let identity = tx.query_row(
                "SELECT id, principal_id, method, issuer, subject, provider_login,
                        credential_handle, created_at, updated_at, last_used_at, disabled_at
                 FROM auth_identity WHERE id = ?1",
                params![id.0],
                auth_identity_from_row,
            )?;
            tx.commit()?;
            Ok(identity)
        })
        .await;
        if result.is_err() {
            self.delete_password_secret_best_effort(&handle, "link_password_identity_rollback")
                .await;
        }
        result
    }

    pub async fn unlink_auth_identity(
        &self,
        principal: PrincipalId,
        identity: AuthIdentityId,
        audit: NewOperationAudit,
    ) -> Result<()> {
        let backend = self.backend.clone();
        let credential_handle = sqlite_blocking(move || {
            let mut conn = backend.conn()?;
            let tx = conn.transaction()?;
            let identity_row: Option<(Option<String>, bool)> = tx
                .query_row(
                    "SELECT credential_handle, disabled_at IS NOT NULL FROM auth_identity
                     WHERE id = ?1 AND principal_id = ?2",
                    params![identity.0, principal.0],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let Some((handle, disabled)) = identity_row else {
                return Err(MetadataError::AuthIdentityNotFound(identity));
            };
            if !disabled {
                let active: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM auth_identity
                     WHERE principal_id = ?1 AND disabled_at IS NULL",
                    params![principal.0],
                    |row| row.get(0),
                )?;
                if active <= 1 {
                    return Err(MetadataError::FinalAuthIdentity);
                }
            }
            tx.execute(
                "DELETE FROM auth_identity WHERE id = ?1",
                params![identity.0],
            )?;
            tx.execute(
                "UPDATE auth_session
                 SET revoked_at = COALESCE(revoked_at, ?1),
                     revocation_reason = COALESCE(revocation_reason, 'identity_unlinked')
                 WHERE principal_id = ?2",
                params![now_text(), principal.0],
            )?;
            insert_operation_audit_row(&tx, &audit)?;
            tx.commit()?;
            Ok(handle)
        })
        .await?;
        if let Some(handle) = credential_handle {
            self.delete_password_secret_best_effort(&handle, "unlink_password_identity")
                .await;
        }
        Ok(())
    }

    pub async fn issue_password_reset(
        &self,
        principal: PrincipalId,
        identity: AuthIdentityId,
        created_by: PrincipalId,
        audit: NewOperationAudit,
    ) -> Result<IssuedPasswordReset> {
        let key = self.auth_token_mac_key().await?;
        let token = new_token_material(PASSWORD_RESET_TOKEN_PREFIX, &key);
        let now = Utc::now();
        let expires_at = now + chrono::Duration::minutes(30);
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let eligible: i64 = tx.query_row(
            "SELECT COUNT(*) FROM auth_identity ai
             JOIN principal p ON p.id = ai.principal_id
             WHERE ai.id = ?1 AND ai.principal_id = ?2
               AND ai.method = 'password' AND ai.disabled_at IS NULL
               AND p.disabled_at IS NULL",
            params![identity.0, principal.0],
            |row| row.get(0),
        )?;
        if eligible != 1 {
            return Err(MetadataError::AuthIdentityNotFound(identity));
        }
        tx.execute(
            "UPDATE password_reset_token SET revoked_at = COALESCE(revoked_at, ?1)
             WHERE auth_identity_id = ?2 AND consumed_at IS NULL AND revoked_at IS NULL",
            params![now.to_rfc3339(), identity.0],
        )?;
        tx.execute(
            "INSERT INTO password_reset_token
             (auth_identity_id, token_lookup, token_digest, created_by, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                identity.0,
                token.lookup,
                token.digest,
                created_by.0,
                now.to_rfc3339(),
                expires_at.to_rfc3339()
            ],
        )?;
        insert_operation_audit_row(&tx, &audit)?;
        tx.commit()?;
        Ok(IssuedPasswordReset {
            token: token.plaintext,
            expires_at,
        })
    }

    pub async fn consume_password_reset(
        &self,
        presented: &str,
        password_verifier: &[u8],
        audit: NewOperationAudit,
    ) -> Result<PrincipalId> {
        let Some(lookup) = auth_token_lookup(presented, PASSWORD_RESET_TOKEN_PREFIX) else {
            return Err(MetadataError::InvalidPasswordReset);
        };
        let key = self.auth_token_mac_key().await?;
        let digest = auth_token_digest(&key, presented);
        let lookup = lookup.to_string();
        let new_handle = Uuid::new_v4().to_string();
        self.secrets
            .put(PASSWORD_SECRET_NAMESPACE, &new_handle, password_verifier)
            .await?;
        let backend = self.backend.clone();
        let db_handle = new_handle.clone();
        let now = Utc::now();
        let result = sqlite_blocking(move || {
            let mut conn = backend.conn()?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let candidate: Option<(i64, AuthIdentityId, PrincipalId, String, Option<String>)> = tx
                .query_row(
                    "SELECT pr.id, ai.id, ai.principal_id, pr.token_digest,
                            ai.credential_handle
                     FROM password_reset_token pr
                     JOIN auth_identity ai ON ai.id = pr.auth_identity_id
                     JOIN principal p ON p.id = ai.principal_id
                     WHERE pr.token_lookup = ?1 AND pr.consumed_at IS NULL
                       AND pr.revoked_at IS NULL AND pr.expires_at > ?2
                       AND ai.method = 'password' AND ai.disabled_at IS NULL
                       AND p.disabled_at IS NULL",
                    params![lookup, now.to_rfc3339()],
                    |row| {
                        Ok((
                            row.get(0)?,
                            AuthIdentityId(row.get(1)?),
                            PrincipalId(row.get(2)?),
                            row.get(3)?,
                            row.get(4)?,
                        ))
                    },
                )
                .optional()?;
            let Some((reset_id, identity, principal, stored_digest, old_handle)) = candidate else {
                return Err(MetadataError::InvalidPasswordReset);
            };
            if !constant_time_eq(stored_digest.as_bytes(), digest.as_bytes()) {
                return Err(MetadataError::InvalidPasswordReset);
            }
            tx.execute(
                "UPDATE password_reset_token SET consumed_at = ?1 WHERE id = ?2",
                params![now.to_rfc3339(), reset_id],
            )?;
            tx.execute(
                "UPDATE auth_identity SET credential_handle = ?1, updated_at = ?2
                 WHERE id = ?3",
                params![db_handle, now.to_rfc3339(), identity.0],
            )?;
            tx.execute(
                "UPDATE auth_session
                 SET revoked_at = COALESCE(revoked_at, ?1),
                     revocation_reason = COALESCE(revocation_reason, 'password_reset')
                 WHERE principal_id = ?2",
                params![now.to_rfc3339(), principal.0],
            )?;
            let mut audit = audit;
            audit.actor_principal_id = Some(principal);
            audit.target_id = Some(identity.0);
            insert_operation_audit_row(&tx, &audit)?;
            tx.commit()?;
            Ok((principal, old_handle))
        })
        .await;
        match result {
            Ok((principal, old_handle)) => {
                if let Some(old_handle) = old_handle {
                    self.delete_password_secret_best_effort(
                        &old_handle,
                        "consume_password_reset_old_verifier",
                    )
                    .await;
                }
                Ok(principal)
            }
            Err(error) => {
                self.delete_password_secret_best_effort(
                    &new_handle,
                    "consume_password_reset_rollback",
                )
                .await;
                Err(error)
            }
        }
    }

    pub async fn issue_auth_session(
        &self,
        principal: PrincipalId,
        client_kind: AuthClientKind,
        client_label: Option<&str>,
        audit: NewOperationAudit,
    ) -> Result<IssuedAuthTokens> {
        let key = self.auth_token_mac_key().await?;
        let issued = new_auth_token_material(&key);
        let session_id = Uuid::new_v4().to_string();
        let family_id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let access_expires_at = now + chrono::Duration::minutes(ACCESS_TOKEN_TTL_MINUTES);
        let refresh_expires_at = now + chrono::Duration::days(REFRESH_TOKEN_TTL_DAYS);
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let enabled: bool = tx
            .query_row(
                "SELECT disabled_at IS NULL FROM principal WHERE id = ?1",
                params![principal.0],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(MetadataError::PrincipalNotFound(principal))?;
        if !enabled {
            return Err(MetadataError::PrincipalNotFound(principal));
        }
        tx.execute(
            "INSERT INTO auth_session
             (id, principal_id, refresh_family_id, client_kind, client_label,
              created_at, last_used_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7)",
            params![
                session_id,
                principal.0,
                family_id,
                client_kind.as_str(),
                client_label,
                now.to_rfc3339(),
                refresh_expires_at.to_rfc3339()
            ],
        )?;
        insert_access_token(&tx, &session_id, &issued.access, access_expires_at, now)?;
        insert_refresh_token(
            &tx,
            &session_id,
            &family_id,
            None,
            &issued.refresh,
            refresh_expires_at,
            now,
        )?;
        insert_operation_audit_row(&tx, &audit)?;
        tx.commit()?;
        Ok(IssuedAuthTokens {
            session_id,
            access_token: issued.access.plaintext,
            access_expires_at,
            refresh_token: issued.refresh.plaintext,
            refresh_expires_at,
        })
    }

    pub async fn create_github_oauth_attempt(
        &self,
        client_kind: AuthClientKind,
    ) -> Result<OAuthStartMaterial> {
        if client_kind == AuthClientKind::Keypair {
            return Err(MetadataError::InvalidEnum {
                field: "oauth_login_attempt.client_kind",
                value: "keypair".into(),
            });
        }
        let key = self.auth_token_mac_key().await?;
        let lookup_seed = Uuid::new_v4().simple().to_string();
        let lookup = &lookup_seed[..AUTH_TOKEN_LOOKUP_LEN];
        let state = format!("{OAUTH_STATE_PREFIX}{lookup}_{}", Uuid::new_v4().simple());
        let state_digest = auth_token_digest(&key, &state);
        let handoff = (client_kind == AuthClientKind::Native)
            .then(|| new_token_material(GITHUB_HANDOFF_PREFIX, &key));
        let code_verifier = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let verifier_handle = Uuid::new_v4().to_string();
        self.secrets
            .put(
                OAUTH_SECRET_NAMESPACE,
                &verifier_handle,
                code_verifier.as_bytes(),
            )
            .await?;
        let now = Utc::now();
        let expires = now + chrono::Duration::minutes(10);
        let result = {
            let conn = self.conn()?;
            conn.execute(
                "INSERT INTO oauth_login_attempt
                 (id, provider, state_lookup, state_digest, pkce_verifier_handle,
                  client_kind, created_at, expires_at, handoff_lookup, handoff_digest)
                 VALUES (?1, 'github', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    Uuid::new_v4().to_string(),
                    lookup,
                    state_digest,
                    verifier_handle,
                    client_kind.as_str(),
                    now.to_rfc3339(),
                    expires.to_rfc3339(),
                    handoff.as_ref().map(|token| token.lookup.as_str()),
                    handoff.as_ref().map(|token| token.digest.as_str())
                ],
            )
        };
        if let Err(error) = result {
            self.delete_oauth_secret_best_effort(&verifier_handle, "create_oauth_attempt_rollback")
                .await;
            return Err(error.into());
        }
        Ok(OAuthStartMaterial {
            state,
            code_verifier,
            handoff_token: handoff.map(|token| token.plaintext),
        })
    }

    pub async fn consume_github_oauth_attempt(&self, state: &str) -> Result<ConsumedOAuthAttempt> {
        let Some(lookup) = auth_token_lookup(state, OAUTH_STATE_PREFIX) else {
            return Err(MetadataError::InvalidOAuthAttempt);
        };
        let key = self.auth_token_mac_key().await?;
        let digest = auth_token_digest(&key, state);
        let now = Utc::now();
        let (attempt_id, verifier_handle, client_kind) = {
            let mut conn = self.conn()?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let candidate = tx
                .query_row(
                    "SELECT id, state_digest, pkce_verifier_handle, client_kind
                     FROM oauth_login_attempt
                     WHERE provider = 'github' AND state_lookup = ?1
                       AND consumed_at IS NULL AND expires_at > ?2",
                    params![lookup, now.to_rfc3339()],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                        ))
                    },
                )
                .optional()?;
            let Some((id, stored_digest, verifier_handle, client_kind)) = candidate else {
                return Err(MetadataError::InvalidOAuthAttempt);
            };
            if !constant_time_eq(stored_digest.as_bytes(), digest.as_bytes()) {
                return Err(MetadataError::InvalidOAuthAttempt);
            }
            tx.execute(
                "UPDATE oauth_login_attempt SET consumed_at = ?1
                 WHERE id = ?2 AND consumed_at IS NULL",
                params![now.to_rfc3339(), id],
            )?;
            tx.commit()?;
            (id, verifier_handle, client_kind)
        };
        let verifier = self
            .secrets
            .get(OAUTH_SECRET_NAMESPACE, &verifier_handle)
            .await?
            .ok_or(MetadataError::InvalidOAuthAttempt)?;
        self.delete_oauth_secret_best_effort(&verifier_handle, "consume_oauth_attempt")
            .await;
        let code_verifier =
            String::from_utf8(verifier).map_err(|_| MetadataError::InvalidOAuthAttempt)?;
        Ok(ConsumedOAuthAttempt {
            attempt_id,
            client_kind: parse_auth_client_kind_sql(client_kind)?,
            code_verifier,
        })
    }

    pub fn complete_native_oauth_attempt(
        &self,
        attempt_id: &str,
        principal: PrincipalId,
    ) -> Result<()> {
        let now = now_text();
        let conn = self.conn()?;
        let changed = conn.execute(
            "UPDATE oauth_login_attempt
             SET result_principal_id = ?1, completed_at = ?2
             WHERE id = ?3 AND client_kind = 'native' AND consumed_at IS NOT NULL
               AND completed_at IS NULL AND expires_at > ?2",
            params![principal.0, now, attempt_id],
        )?;
        if changed != 1 {
            return Err(MetadataError::InvalidOAuthAttempt);
        }
        Ok(())
    }

    pub async fn consume_native_oauth_handoff(&self, presented: &str) -> Result<PrincipalId> {
        let Some(lookup) = auth_token_lookup(presented, GITHUB_HANDOFF_PREFIX) else {
            return Err(MetadataError::InvalidOAuthAttempt);
        };
        let key = self.auth_token_mac_key().await?;
        let digest = auth_token_digest(&key, presented);
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let candidate: Option<(String, PrincipalId, String)> = tx
            .query_row(
                "SELECT id, result_principal_id, handoff_digest FROM oauth_login_attempt
                 WHERE provider = 'github' AND client_kind = 'native'
                   AND handoff_lookup = ?1 AND completed_at IS NOT NULL
                   AND claimed_at IS NULL AND expires_at > ?2",
                params![lookup, now],
                |row| Ok((row.get(0)?, PrincipalId(row.get(1)?), row.get(2)?)),
            )
            .optional()?;
        let Some((attempt_id, principal, stored_digest)) = candidate else {
            return Err(MetadataError::InvalidOAuthAttempt);
        };
        if !constant_time_eq(stored_digest.as_bytes(), digest.as_bytes()) {
            return Err(MetadataError::InvalidOAuthAttempt);
        }
        let changed = tx.execute(
            "UPDATE oauth_login_attempt SET claimed_at = ?1
             WHERE id = ?2 AND claimed_at IS NULL",
            params![now, attempt_id],
        )?;
        if changed != 1 {
            return Err(MetadataError::InvalidOAuthAttempt);
        }
        tx.commit()?;
        Ok(principal)
    }

    pub async fn verify_auth_access_token(
        &self,
        presented: &str,
    ) -> Result<Option<AuthenticatedSession>> {
        let Some(lookup) = auth_token_lookup(presented, ACCESS_TOKEN_PREFIX) else {
            return Ok(None);
        };
        let key = self.auth_token_mac_key().await?;
        let digest = auth_token_digest(&key, presented);
        let now = Utc::now();
        let conn = self.conn()?;
        let session = conn
            .query_row(
                "SELECT s.id, s.client_kind, at.expires_at,
                        p.id, p.external_id, p.display_name, p.email, p.avatar_url,
                        p.disabled_at, p.is_instance_admin, p.created_at, p.updated_at
                 FROM auth_access_token at
                 JOIN auth_session s ON s.id = at.auth_session_id
                 JOIN principal p ON p.id = s.principal_id
                 WHERE at.token_lookup = ?1 AND at.token_digest = ?2
                   AND at.revoked_at IS NULL AND at.expires_at > ?3
                   AND s.revoked_at IS NULL AND s.expires_at > ?3
                   AND p.disabled_at IS NULL",
                params![lookup, digest, now.to_rfc3339()],
                |row| {
                    let kind: String = row.get(1)?;
                    Ok(AuthenticatedSession {
                        session_id: row.get(0)?,
                        client_kind: parse_auth_client_kind_sql(kind)?,
                        expires_at: parse_time_sql(row.get(2)?)?,
                        principal: principal_from_row_offset(row, 3)?,
                    })
                },
            )
            .optional()?;
        if let Some(session) = &session {
            conn.execute(
                "UPDATE auth_session SET last_used_at = ?1 WHERE id = ?2",
                params![now.to_rfc3339(), session.session_id],
            )?;
        }
        Ok(session)
    }

    pub fn auth_session_is_active(&self, session_id: &str) -> Result<bool> {
        let now = now_text();
        let conn = self.conn()?;
        let active: i64 = conn.query_row(
            "SELECT COUNT(*) FROM auth_session s
             JOIN principal p ON p.id = s.principal_id
             WHERE s.id = ?1 AND s.revoked_at IS NULL AND s.expires_at > ?2
               AND p.disabled_at IS NULL",
            params![session_id, now],
            |row| row.get(0),
        )?;
        Ok(active == 1)
    }

    pub async fn rotate_auth_refresh_token(
        &self,
        presented: &str,
        audit: NewOperationAudit,
    ) -> Result<RefreshAuthResult> {
        let Some(lookup) = auth_token_lookup(presented, REFRESH_TOKEN_PREFIX) else {
            return Ok(RefreshAuthResult::Invalid);
        };
        let key = self.auth_token_mac_key().await?;
        let digest = auth_token_digest(&key, presented);
        let replacement = new_auth_token_material(&key);
        let now = Utc::now();
        let access_expires_at = now + chrono::Duration::minutes(ACCESS_TOKEN_TTL_MINUTES);
        let refresh_expires_at = now + chrono::Duration::days(REFRESH_TOKEN_TTL_DAYS);
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let candidate = tx
            .query_row(
                "SELECT rt.id, rt.auth_session_id, rt.family_id, rt.token_digest,
                        rt.expires_at, rt.consumed_at, rt.revoked_at,
                        s.revoked_at, p.disabled_at
                 FROM auth_refresh_token rt
                 JOIN auth_session s ON s.id = rt.auth_session_id
                 JOIN principal p ON p.id = s.principal_id
                 WHERE rt.token_lookup = ?1",
                params![lookup],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, Option<String>>(8)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            id,
            session_id,
            family_id,
            stored_digest,
            expires_at,
            consumed_at,
            token_revoked,
            session_revoked,
            principal_disabled,
        )) = candidate
        else {
            return Ok(RefreshAuthResult::Invalid);
        };
        if !constant_time_eq(stored_digest.as_bytes(), digest.as_bytes()) {
            return Ok(RefreshAuthResult::Invalid);
        }
        if consumed_at.is_some() {
            tx.execute(
                "UPDATE auth_session
                 SET revoked_at = COALESCE(revoked_at, ?1),
                     revocation_reason = COALESCE(revocation_reason, 'refresh_replay')
                 WHERE refresh_family_id = ?2",
                params![now.to_rfc3339(), family_id],
            )?;
            tx.execute(
                "UPDATE auth_refresh_token SET revoked_at = COALESCE(revoked_at, ?1)
                 WHERE family_id = ?2",
                params![now.to_rfc3339(), family_id],
            )?;
            insert_operation_audit_row(&tx, &audit)?;
            tx.commit()?;
            return Ok(RefreshAuthResult::ReplayDetected);
        }
        if token_revoked.is_some()
            || session_revoked.is_some()
            || principal_disabled.is_some()
            || parse_time(expires_at)? <= now
        {
            return Ok(RefreshAuthResult::Invalid);
        }
        let replacement_id = insert_refresh_token(
            &tx,
            &session_id,
            &family_id,
            Some(id),
            &replacement.refresh,
            refresh_expires_at,
            now,
        )?;
        tx.execute(
            "UPDATE auth_refresh_token
             SET consumed_at = ?1, replaced_by_id = ?2 WHERE id = ?3 AND consumed_at IS NULL",
            params![now.to_rfc3339(), replacement_id, id],
        )?;
        tx.execute(
            "UPDATE auth_access_token SET revoked_at = COALESCE(revoked_at, ?1)
             WHERE auth_session_id = ?2",
            params![now.to_rfc3339(), session_id],
        )?;
        insert_access_token(
            &tx,
            &session_id,
            &replacement.access,
            access_expires_at,
            now,
        )?;
        tx.execute(
            "UPDATE auth_session SET last_used_at = ?1, expires_at = ?2 WHERE id = ?3",
            params![
                now.to_rfc3339(),
                refresh_expires_at.to_rfc3339(),
                session_id
            ],
        )?;
        insert_operation_audit_row(&tx, &audit)?;
        tx.commit()?;
        Ok(RefreshAuthResult::Issued(IssuedAuthTokens {
            session_id,
            access_token: replacement.access.plaintext,
            access_expires_at,
            refresh_token: replacement.refresh.plaintext,
            refresh_expires_at,
        }))
    }

    pub fn revoke_auth_session(
        &self,
        session_id: &str,
        reason: &str,
        audit: NewOperationAudit,
    ) -> Result<()> {
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE auth_session
             SET revoked_at = COALESCE(revoked_at, ?1),
                 revocation_reason = COALESCE(revocation_reason, ?2)
             WHERE id = ?3",
            params![now, reason, session_id],
        )?;
        tx.execute(
            "UPDATE auth_access_token SET revoked_at = COALESCE(revoked_at, ?1)
             WHERE auth_session_id = ?2",
            params![now, session_id],
        )?;
        tx.execute(
            "UPDATE auth_refresh_token SET revoked_at = COALESCE(revoked_at, ?1)
             WHERE auth_session_id = ?2",
            params![now, session_id],
        )?;
        insert_operation_audit_row(&tx, &audit)?;
        tx.commit()?;
        Ok(())
    }

    pub fn list_principal_auth_sessions(
        &self,
        principal: PrincipalId,
    ) -> Result<Vec<AuthSessionSummary>> {
        let conn = self.conn()?;
        let mut statement = conn.prepare(
            "SELECT id, client_kind, client_label, created_at, last_used_at,
                    expires_at, revoked_at, revocation_reason
             FROM auth_session WHERE principal_id = ?1
             ORDER BY created_at DESC, id DESC",
        )?;
        let rows = statement.query_map(params![principal.0], |row| {
            Ok(AuthSessionSummary {
                id: row.get(0)?,
                client_kind: row.get(1)?,
                client_label: row.get(2)?,
                created_at: parse_time_sql(row.get(3)?)?,
                last_used_at: parse_optional_time_sql(row.get(4)?)?,
                expires_at: parse_time_sql(row.get(5)?)?,
                revoked_at: parse_optional_time_sql(row.get(6)?)?,
                revocation_reason: row.get(7)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn revoke_principal_auth_session(
        &self,
        principal: PrincipalId,
        session_id: &str,
        audit: NewOperationAudit,
    ) -> Result<()> {
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE auth_session
             SET revoked_at = COALESCE(revoked_at, ?1),
                 revocation_reason = COALESCE(revocation_reason, 'admin_revoked')
             WHERE id = ?2 AND principal_id = ?3",
            params![now, session_id, principal.0],
        )?;
        if changed == 0 {
            return Err(MetadataError::AuthSessionNotFound(session_id.to_string()));
        }
        tx.execute(
            "UPDATE auth_access_token SET revoked_at = COALESCE(revoked_at, ?1)
             WHERE auth_session_id = ?2",
            params![now, session_id],
        )?;
        tx.execute(
            "UPDATE auth_refresh_token SET revoked_at = COALESCE(revoked_at, ?1)
             WHERE auth_session_id = ?2",
            params![now, session_id],
        )?;
        insert_operation_audit_row(&tx, &audit)?;
        tx.commit()?;
        Ok(())
    }

    pub fn revoke_all_auth_sessions(
        &self,
        principal: PrincipalId,
        reason: &str,
        audit: NewOperationAudit,
    ) -> Result<()> {
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE auth_session
             SET revoked_at = COALESCE(revoked_at, ?1),
                 revocation_reason = COALESCE(revocation_reason, ?2)
             WHERE principal_id = ?3",
            params![now, reason, principal.0],
        )?;
        tx.execute(
            "UPDATE auth_access_token SET revoked_at = COALESCE(revoked_at, ?1)
             WHERE auth_session_id IN (
                SELECT id FROM auth_session WHERE principal_id = ?2
             )",
            params![now, principal.0],
        )?;
        tx.execute(
            "UPDATE auth_refresh_token SET revoked_at = COALESCE(revoked_at, ?1)
             WHERE auth_session_id IN (
                SELECT id FROM auth_session WHERE principal_id = ?2
             )",
            params![now, principal.0],
        )?;
        insert_operation_audit_row(&tx, &audit)?;
        tx.commit()?;
        Ok(())
    }

    pub async fn issue_tenant_invitation(
        &self,
        tenant: TenantId,
        role: MembershipRole,
        actor: PrincipalId,
        target: Option<PrincipalId>,
        expires_at: DateTime<Utc>,
        audit: NewOperationAudit,
    ) -> Result<IssuedTenantInvitation> {
        let key = self.auth_token_mac_key().await?;
        let token = new_token_material(INVITATION_TOKEN_PREFIX, &key);
        let now = Utc::now();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO tenant_invitation
             (tenant_id, intended_role, created_by, target_principal_id,
              token_lookup, token_digest, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                tenant.0,
                role.as_str(),
                actor.0,
                target.map(|id| id.0),
                token.lookup,
                token.digest,
                now.to_rfc3339(),
                expires_at.to_rfc3339()
            ],
        )?;
        let id = TenantInvitationId(tx.last_insert_rowid());
        insert_operation_audit_row(&tx, &audit)?;
        let invitation = tenant_invitation_by_id_locked(&tx, id)?;
        tx.commit()?;
        Ok(IssuedTenantInvitation {
            invitation,
            token: token.plaintext,
        })
    }

    pub fn list_tenant_invitations(&self, tenant: TenantId) -> Result<Vec<TenantInvitation>> {
        let conn = self.conn()?;
        let mut statement = conn.prepare(
            "SELECT id, tenant_id, intended_role, created_by, target_principal_id,
                    created_at, expires_at, consumed_at, revoked_at
             FROM tenant_invitation WHERE tenant_id = ?1 ORDER BY created_at DESC, id DESC",
        )?;
        let invitations =
            rows(statement.query_map(params![tenant.0], tenant_invitation_from_row)?)?;
        Ok(invitations)
    }

    pub async fn accept_tenant_invitation(
        &self,
        presented: &str,
        principal: PrincipalId,
        audit: NewOperationAudit,
    ) -> Result<TenantMembership> {
        let Some(lookup) = auth_token_lookup(presented, INVITATION_TOKEN_PREFIX) else {
            return Err(MetadataError::InvalidTenantInvitation);
        };
        let key = self.auth_token_mac_key().await?;
        let digest = auth_token_digest(&key, presented);
        let now = Utc::now();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let candidate = tx
            .query_row(
                "SELECT id, tenant_id, intended_role, target_principal_id, token_digest
                 FROM tenant_invitation
                 WHERE token_lookup = ?1 AND consumed_at IS NULL AND revoked_at IS NULL
                   AND expires_at > ?2",
                params![lookup, now.to_rfc3339()],
                |row| {
                    Ok((
                        TenantInvitationId(row.get(0)?),
                        TenantId(row.get(1)?),
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<i64>>(3)?.map(PrincipalId),
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((id, tenant, role, target, stored_digest)) = candidate else {
            return Err(MetadataError::InvalidTenantInvitation);
        };
        if target.is_some_and(|target| target != principal)
            || !constant_time_eq(stored_digest.as_bytes(), digest.as_bytes())
        {
            return Err(MetadataError::InvalidTenantInvitation);
        }
        let role = schema::parse_role(role)?;
        tx.execute(
            "INSERT INTO membership (tenant_id, principal_id, role, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(tenant_id, principal_id) DO UPDATE SET
                role = excluded.role, updated_at = excluded.updated_at",
            params![tenant.0, principal.0, role.as_str(), now.to_rfc3339()],
        )?;
        tx.execute(
            "UPDATE tenant_invitation SET consumed_at = ?1 WHERE id = ?2 AND consumed_at IS NULL",
            params![now.to_rfc3339(), id.0],
        )?;
        insert_operation_audit_row(&tx, &audit)?;
        let membership = tx.query_row(
            "SELECT t.id, t.name, t.kind, t.created_at, t.updated_at,
                    m.principal_id, m.role, m.created_at, m.updated_at
             FROM membership m JOIN tenant t ON t.id = m.tenant_id
             WHERE m.tenant_id = ?1 AND m.principal_id = ?2",
            params![tenant.0, principal.0],
            tenant_membership_from_row,
        )?;
        tx.commit()?;
        Ok(membership)
    }

    pub fn revoke_tenant_invitation(
        &self,
        id: TenantInvitationId,
        audit: NewOperationAudit,
    ) -> Result<()> {
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE tenant_invitation SET revoked_at = COALESCE(revoked_at, ?1)
             WHERE id = ?2 AND consumed_at IS NULL",
            params![now, id.0],
        )?;
        if changed == 0 {
            return Err(MetadataError::InvalidTenantInvitation);
        }
        insert_operation_audit_row(&tx, &audit)?;
        tx.commit()?;
        Ok(())
    }

    pub fn register_principal_key(
        &self,
        principal: PrincipalId,
        public_key: &[u8],
        fingerprint: &str,
        label: &str,
        audit: NewOperationAudit,
    ) -> Result<PrincipalKey> {
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO principal_key
             (principal_id, algorithm, public_key, fingerprint, label, created_at, updated_at)
             VALUES (?1, 'ed25519', ?2, ?3, ?4, ?5, ?5)",
            params![principal.0, public_key, fingerprint, label, now],
        )?;
        let id = PrincipalKeyId(tx.last_insert_rowid());
        insert_operation_audit_row(&tx, &audit)?;
        let key = principal_key_by_id_locked(&tx, id)?;
        tx.commit()?;
        Ok(key)
    }

    pub fn list_principal_keys(&self, principal: PrincipalId) -> Result<Vec<PrincipalKey>> {
        let conn = self.conn()?;
        let mut statement = conn.prepare(
            "SELECT id, principal_id, public_key, fingerprint, label, created_at,
                    updated_at, last_used_at, revoked_at
             FROM principal_key WHERE principal_id = ?1 ORDER BY created_at DESC",
        )?;
        let keys = rows(statement.query_map(params![principal.0], principal_key_from_row)?)?;
        Ok(keys)
    }

    pub fn revoke_principal_key(
        &self,
        id: PrincipalKeyId,
        principal: PrincipalId,
        audit: NewOperationAudit,
    ) -> Result<()> {
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE principal_key SET revoked_at = COALESCE(revoked_at, ?1), updated_at = ?1
             WHERE id = ?2 AND principal_id = ?3",
            params![now, id.0, principal.0],
        )?;
        if changed == 0 {
            return Err(MetadataError::PrincipalKeyNotFound(id));
        }
        insert_operation_audit_row(&tx, &audit)?;
        tx.commit()?;
        Ok(())
    }

    pub fn issue_key_challenge(&self, fingerprint: &str) -> Result<IssuedKeyChallenge> {
        let mut nonce = vec![0_u8; 32];
        getrandom::getrandom(&mut nonce)
            .map_err(|error| MetadataError::SecretStore(format!("rng failure: {error}")))?;
        let now = Utc::now();
        let expires_at = now + chrono::Duration::minutes(5);
        let conn = self.conn()?;
        let key = conn
            .query_row(
                "SELECT id, principal_id, public_key, fingerprint, label, created_at,
                        updated_at, last_used_at, revoked_at
                 FROM principal_key WHERE fingerprint = ?1 AND revoked_at IS NULL",
                params![fingerprint],
                principal_key_from_row,
            )
            .optional()?
            .ok_or(MetadataError::InvalidKeyChallenge)?;
        conn.execute(
            "INSERT INTO keypair_challenge
             (nonce, fingerprint, issued_at, expires_at, principal_key_id)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                nonce,
                fingerprint,
                now.to_rfc3339(),
                expires_at.to_rfc3339(),
                key.id.0
            ],
        )?;
        Ok(IssuedKeyChallenge {
            nonce,
            principal_key: key,
            expires_at,
        })
    }

    pub fn consume_key_challenge(&self, nonce: &[u8]) -> Result<ConsumedKeyChallenge> {
        let now = Utc::now();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let key_id: Option<PrincipalKeyId> = tx
            .query_row(
                "SELECT principal_key_id FROM keypair_challenge
                 WHERE nonce = ?1 AND consumed_at IS NULL AND expires_at > ?2",
                params![nonce, now.to_rfc3339()],
                |row| row.get::<_, i64>(0).map(PrincipalKeyId),
            )
            .optional()?;
        let Some(key_id) = key_id else {
            return Err(MetadataError::InvalidKeyChallenge);
        };
        let key = principal_key_by_id_locked(&tx, key_id)?;
        if key.revoked_at.is_some() {
            return Err(MetadataError::InvalidKeyChallenge);
        }
        tx.execute(
            "UPDATE keypair_challenge SET consumed_at = ?1
             WHERE nonce = ?2 AND consumed_at IS NULL",
            params![now.to_rfc3339(), nonce],
        )?;
        tx.execute(
            "UPDATE principal_key SET last_used_at = ?1, updated_at = ?1 WHERE id = ?2",
            params![now.to_rfc3339(), key_id.0],
        )?;
        tx.commit()?;
        Ok(ConsumedKeyChallenge {
            nonce: nonce.to_vec(),
            principal_key: key,
        })
    }

    pub async fn issue_ssh_proxy_capability(
        &self,
        claims: &SshProxyCapabilityClaims,
        daemon_generation: &str,
        principal_key_id: Option<PrincipalKeyId>,
        audit: NewOperationAudit,
    ) -> Result<IssuedSshProxyCapability> {
        let now = Utc::now();
        if claims.version != 1
            || claims.instance_audience.is_empty()
            || claims.instance_audience.len() > 512
            || claims.capability_id.is_empty()
            || claims.capability_id.len() > 128
            || daemon_generation.is_empty()
            || daemon_generation.len() > 128
            || claims.issued_at > now + chrono::Duration::minutes(1)
            || claims.expires_at <= now
            || claims.expires_at <= claims.issued_at
            || claims.expires_at > claims.issued_at + chrono::Duration::minutes(5)
        {
            return Err(MetadataError::InvalidSshProxyCapability);
        }

        let principal = PrincipalId(claims.principal_id);
        let conn = self.conn()?;
        let principal_enabled: bool = conn
            .query_row(
                "SELECT disabled_at IS NULL FROM principal WHERE id = ?1",
                params![principal.0],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(MetadataError::PrincipalNotFound(principal))?;
        if !principal_enabled {
            return Err(MetadataError::InvalidSshProxyCapability);
        }
        if let Some(key_id) = principal_key_id {
            let valid_key: bool = conn.query_row(
                "SELECT EXISTS(
                        SELECT 1 FROM principal_key
                        WHERE id = ?1 AND principal_id = ?2 AND revoked_at IS NULL
                     )",
                params![key_id.0, principal.0],
                |row| row.get(0),
            )?;
            if !valid_key {
                return Err(MetadataError::InvalidSshProxyCapability);
            }
        }
        drop(conn);

        let payload =
            serde_json::to_vec(claims).map_err(|_| MetadataError::InvalidSshProxyCapability)?;
        if payload.len() > 4 * 1024 {
            return Err(MetadataError::InvalidSshProxyCapability);
        }
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload);
        let signed = format!("{SSH_PROXY_CAPABILITY_PREFIX}{payload}");
        let key = self.ssh_proxy_capability_mac_key().await?;
        let mac = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(hmac_sha256(&key, signed.as_bytes()));
        let capability = format!("{signed}.{mac}");
        let digest = auth_token_digest(&key, &capability);

        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO ssh_proxy_capability
             (capability_id, capability_digest, principal_id, principal_key_id,
              instance_audience, daemon_generation, issued_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                claims.capability_id,
                digest,
                principal.0,
                principal_key_id.map(|id| id.0),
                claims.instance_audience,
                daemon_generation,
                claims.issued_at.to_rfc3339(),
                claims.expires_at.to_rfc3339(),
            ],
        )?;
        insert_operation_audit_row(&tx, &audit)?;
        tx.commit()?;
        Ok(IssuedSshProxyCapability {
            capability,
            expires_at: claims.expires_at,
        })
    }

    pub async fn consume_ssh_proxy_capability(
        &self,
        presented: &str,
        expected_audience: &str,
        expected_generation: &str,
        audit: NewOperationAudit,
    ) -> Result<IssuedSshProxyAccess> {
        if presented.len() > 8 * 1024 {
            return Err(MetadataError::InvalidSshProxyCapability);
        }
        let body = presented
            .strip_prefix(SSH_PROXY_CAPABILITY_PREFIX)
            .ok_or(MetadataError::InvalidSshProxyCapability)?;
        let mut parts = body.split('.');
        let payload_text = parts
            .next()
            .filter(|part| !part.is_empty())
            .ok_or(MetadataError::InvalidSshProxyCapability)?;
        let presented_mac = parts
            .next()
            .filter(|part| !part.is_empty())
            .ok_or(MetadataError::InvalidSshProxyCapability)?;
        if parts.next().is_some() {
            return Err(MetadataError::InvalidSshProxyCapability);
        }

        let capability_key = self.ssh_proxy_capability_mac_key().await?;
        let signed = format!("{SSH_PROXY_CAPABILITY_PREFIX}{payload_text}");
        let expected_mac = hmac_sha256(&capability_key, signed.as_bytes());
        let presented_mac = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(presented_mac)
            .map_err(|_| MetadataError::InvalidSshProxyCapability)?;
        if !constant_time_eq(&expected_mac, &presented_mac) {
            return Err(MetadataError::InvalidSshProxyCapability);
        }
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload_text)
            .map_err(|_| MetadataError::InvalidSshProxyCapability)?;
        if payload.len() > 4 * 1024 {
            return Err(MetadataError::InvalidSshProxyCapability);
        }
        let claims: SshProxyCapabilityClaims = serde_json::from_slice(&payload)
            .map_err(|_| MetadataError::InvalidSshProxyCapability)?;
        let now = Utc::now();
        if claims.version != 1
            || claims.instance_audience != expected_audience
            || claims.issued_at > now + chrono::Duration::minutes(1)
            || claims.expires_at <= now
        {
            return Err(MetadataError::InvalidSshProxyCapability);
        }

        let capability_digest = auth_token_digest(&capability_key, presented);
        let access_key = self.auth_token_mac_key().await?;
        let access = new_token_material(ACCESS_TOKEN_PREFIX, &access_key);
        let access_expires_at = now + chrono::Duration::minutes(ACCESS_TOKEN_TTL_MINUTES);
        let session_id = Uuid::new_v4().to_string();
        let family_id = Uuid::new_v4().to_string();

        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored: Option<(PrincipalId, Option<PrincipalKeyId>)> = tx
            .query_row(
                "SELECT c.principal_id, c.principal_key_id
                 FROM ssh_proxy_capability c
                 JOIN principal p ON p.id = c.principal_id
                 WHERE c.capability_id = ?1
                   AND c.capability_digest = ?2
                   AND c.instance_audience = ?3
                   AND c.daemon_generation = ?4
                   AND c.consumed_at IS NULL
                   AND c.revoked_at IS NULL
                   AND c.expires_at > ?5
                   AND p.disabled_at IS NULL
                   AND (
                       c.principal_key_id IS NULL OR EXISTS(
                           SELECT 1 FROM principal_key k
                           WHERE k.id = c.principal_key_id
                             AND k.principal_id = c.principal_id
                             AND k.revoked_at IS NULL
                       )
                   )",
                params![
                    claims.capability_id,
                    capability_digest,
                    expected_audience,
                    expected_generation,
                    now.to_rfc3339(),
                ],
                |row| {
                    Ok((
                        PrincipalId(row.get(0)?),
                        row.get::<_, Option<i64>>(1)?.map(PrincipalKeyId),
                    ))
                },
            )
            .optional()?;
        let Some((principal, _key)) = stored else {
            return Err(MetadataError::InvalidSshProxyCapability);
        };
        if principal.0 != claims.principal_id {
            return Err(MetadataError::InvalidSshProxyCapability);
        }
        let consumed = tx.execute(
            "UPDATE ssh_proxy_capability SET consumed_at = ?1
             WHERE capability_id = ?2 AND consumed_at IS NULL",
            params![now.to_rfc3339(), claims.capability_id],
        )?;
        if consumed != 1 {
            return Err(MetadataError::InvalidSshProxyCapability);
        }
        tx.execute(
            "INSERT INTO auth_session
             (id, principal_id, refresh_family_id, client_kind, client_label,
              created_at, last_used_at, expires_at)
             VALUES (?1, ?2, ?3, 'keypair', 'ssh-proxy', ?4, ?4, ?5)",
            params![
                session_id,
                principal.0,
                family_id,
                now.to_rfc3339(),
                access_expires_at.to_rfc3339(),
            ],
        )?;
        insert_access_token(&tx, &session_id, &access, access_expires_at, now)?;
        let mut audit = audit;
        audit.actor_principal_id = Some(principal);
        insert_operation_audit_row(&tx, &audit)?;
        tx.commit()?;

        Ok(IssuedSshProxyAccess {
            session_id,
            access_token: access.plaintext,
            access_expires_at,
            principal_id: principal,
            daemon_generation: expected_generation.to_string(),
        })
    }

    async fn auth_token_mac_key(&self) -> Result<Vec<u8>> {
        self.system_mac_key(AUTH_TOKEN_MAC_HANDLE).await
    }

    async fn ssh_proxy_capability_mac_key(&self) -> Result<Vec<u8>> {
        self.system_mac_key(SSH_PROXY_CAPABILITY_MAC_HANDLE).await
    }

    /// Provision the system authentication keys before multiple processes open
    /// the same persistent secret backend.
    pub async fn ensure_auth_system_keys(&self) -> Result<()> {
        self.auth_token_mac_key().await?;
        self.ssh_proxy_capability_mac_key().await?;
        Ok(())
    }

    async fn system_mac_key(&self, handle: &str) -> Result<Vec<u8>> {
        if let Some(key) = self
            .secrets
            .get(AUTH_SYSTEM_SECRET_NAMESPACE, handle)
            .await?
        {
            if key.len() != 32 {
                return Err(MetadataError::InvalidAuthTokenKey);
            }
            return Ok(key);
        }
        let mut key = vec![0_u8; 32];
        getrandom::getrandom(&mut key)
            .map_err(|error| MetadataError::SecretStore(format!("rng failure: {error}")))?;
        self.secrets
            .put(AUTH_SYSTEM_SECRET_NAMESPACE, handle, &key)
            .await?;
        Ok(key)
    }

    pub fn create_tenant(&self, name: &str, kind: TenantKind) -> Result<Tenant> {
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO tenant (name, kind, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?3)",
            params![name, kind.as_str(), now],
        )?;
        let id = TenantId(tx.last_insert_rowid());
        let tenant = Tenant {
            id,
            name: name.to_string(),
            kind,
            created_at: parse_time(now.clone())?,
            updated_at: parse_time(now)?,
        };
        tx.commit()?;
        Ok(tenant)
    }

    pub fn upsert_tenant_membership(
        &self,
        tenant: TenantId,
        principal: PrincipalId,
        role: MembershipRole,
    ) -> Result<TenantMembership> {
        let now = now_text();
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO membership (tenant_id, principal_id, role, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(tenant_id, principal_id) DO UPDATE SET
                role = excluded.role,
                updated_at = excluded.updated_at",
            params![tenant.0, principal.0, role.as_str(), now],
        )?;
        conn.query_row(
            "SELECT t.id, t.name, t.kind, t.created_at, t.updated_at,
                    m.principal_id, m.role, m.created_at, m.updated_at
             FROM membership m
             JOIN tenant t ON t.id = m.tenant_id
             WHERE m.tenant_id = ?1 AND m.principal_id = ?2",
            params![tenant.0, principal.0],
            tenant_membership_from_row,
        )
        .map_err(Into::into)
    }

    pub fn list_principal_tenants(&self, principal: PrincipalId) -> Result<Vec<TenantMembership>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT t.id, t.name, t.kind, t.created_at, t.updated_at,
                    m.principal_id, m.role, m.created_at, m.updated_at
             FROM membership m
             JOIN tenant t ON t.id = m.tenant_id
             WHERE m.principal_id = ?1
             ORDER BY t.name",
        )?;
        let memberships = rows(stmt.query_map(params![principal.0], tenant_membership_from_row)?);
        memberships
    }

    pub fn list_connection_profiles(&self, tenant: TenantId) -> Result<Vec<ConnectionProfile>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, tenant_id, name, engine, spec_json, credential_mode,
                    shared_secret_handle, tags_json, created_by, created_at, updated_at,
                    policy_json, policy_revision, provider_id, configuration_json,
                    semantic_engine
             FROM connection_profile
             WHERE tenant_id = ?1
             ORDER BY name",
        )?;
        let profiles = rows(stmt.query_map(params![tenant.0], connection_profile_from_row)?);
        profiles
    }

    pub fn get_connection_profile(
        &self,
        tenant: TenantId,
        id: ConnectionProfileId,
    ) -> Result<ConnectionProfile> {
        let conn = self.conn()?;
        let profile = self.connection_profile_by_id_locked(&conn, id)?;
        if profile.tenant_id != tenant {
            return Err(MetadataError::TenantMismatch(id, tenant));
        }
        Ok(profile)
    }

    pub fn get_connection_profile_for_any_tenant(
        &self,
        id: ConnectionProfileId,
    ) -> Result<ConnectionProfile> {
        let conn = self.conn()?;
        self.connection_profile_by_id_locked(&conn, id)
    }

    pub fn get_connection_profile_for_principal(
        &self,
        id: ConnectionProfileId,
        principal: PrincipalId,
    ) -> Result<ConnectionProfile> {
        let conn = self.conn()?;
        let tenant: Option<i64> = conn
            .query_row(
                "SELECT cp.tenant_id FROM connection_profile cp
                 JOIN membership m ON m.tenant_id = cp.tenant_id
                 WHERE cp.id = ?1 AND m.principal_id = ?2",
                params![id.0, principal.0],
                |row| row.get(0),
            )
            .optional()?;
        tenant.ok_or(MetadataError::ConnectionProfileNotFound(id))?;
        self.connection_profile_by_id_locked(&conn, id)
    }

    pub fn update_connection_policy(
        &self,
        tenant: TenantId,
        actor: PrincipalId,
        id: ConnectionProfileId,
        input: UpdateConnectionPolicyRequest,
        audit: NewOperationAudit,
    ) -> Result<ConnectionProfile> {
        validate_connection_policy_input(&input)?;
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_tenant_admin_locked(&tx, tenant, actor)?;
        let current = connection_profile_by_id_locked(&tx, id)?;
        if current.tenant_id != tenant {
            return Err(MetadataError::TenantMismatch(id, tenant));
        }
        if let Some(expected) = input.expected_revision {
            if expected != current.policy.revision {
                return Err(MetadataError::PolicyRevisionConflict {
                    expected,
                    current: current.policy.revision,
                });
            }
        }
        let revision = current.policy.revision.checked_add(1).ok_or(
            MetadataError::PolicyRevisionConflict {
                expected: current.policy.revision,
                current: current.policy.revision,
            },
        )?;
        let policy = ConnectionPolicy {
            minimum_tenant_role: input.minimum_tenant_role,
            read_only: input.read_only,
            allowed_ops: input.allowed_ops,
            blocked_ops: input.blocked_ops,
            allowed_schemas: input.allowed_schemas,
            revision,
        };
        let policy_json = serde_json::to_string(&policy)?;
        let revision_i64 =
            i64::try_from(revision).map_err(|_| MetadataError::PolicyRevisionConflict {
                expected: current.policy.revision,
                current: current.policy.revision,
            })?;
        tx.execute(
            "UPDATE connection_profile
             SET policy_json = ?1, policy_revision = ?2, updated_at = ?3
             WHERE id = ?4 AND tenant_id = ?5",
            params![policy_json, revision_i64, now_text(), id.0, tenant.0],
        )?;
        let mut audit = audit;
        audit.actor_principal_id = Some(actor);
        insert_operation_audit_row(&tx, &audit)?;
        tx.commit()?;
        connection_profile_by_id_locked(&conn, id)
    }

    pub fn get_tenant_limit_override(
        &self,
        tenant: TenantId,
    ) -> Result<Option<TenantLimitOverride>> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT tenant_id, limits_json, updated_by, created_at, updated_at
             FROM tenant_limit_override WHERE tenant_id = ?1",
            params![tenant.0],
            tenant_limit_override_from_row,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn set_tenant_limit_override(
        &self,
        actor: PrincipalId,
        tenant: TenantId,
        limits: TenantResourceLimits,
        audit: NewOperationAudit,
    ) -> Result<TenantLimitOverride> {
        let limits_json = serde_json::to_string(&limits)?;
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_instance_admin_locked(&tx, actor)?;
        tx.execute(
            "INSERT INTO tenant_limit_override
             (tenant_id, limits_json, updated_by, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(tenant_id) DO UPDATE SET
                limits_json = excluded.limits_json,
                updated_by = excluded.updated_by,
                updated_at = excluded.updated_at",
            params![tenant.0, limits_json, actor.0, now],
        )?;
        let mut audit = audit;
        audit.actor_principal_id = Some(actor);
        insert_operation_audit_row(&tx, &audit)?;
        tx.commit()?;
        conn.query_row(
            "SELECT tenant_id, limits_json, updated_by, created_at, updated_at
             FROM tenant_limit_override WHERE tenant_id = ?1",
            params![tenant.0],
            tenant_limit_override_from_row,
        )
        .map_err(Into::into)
    }

    pub fn clear_tenant_limit_override(
        &self,
        actor: PrincipalId,
        tenant: TenantId,
        audit: NewOperationAudit,
    ) -> Result<bool> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_instance_admin_locked(&tx, actor)?;
        let deleted = tx.execute(
            "DELETE FROM tenant_limit_override WHERE tenant_id = ?1",
            params![tenant.0],
        )?;
        let mut audit = audit;
        audit.actor_principal_id = Some(actor);
        insert_operation_audit_row(&tx, &audit)?;
        tx.commit()?;
        Ok(deleted != 0)
    }

    pub async fn upsert_connection_profile(
        &self,
        tenant: TenantId,
        actor: PrincipalId,
        input: NewConnectionProfile,
    ) -> Result<ConnectionProfile> {
        self.upsert_connection_profile_with_limit(
            tenant,
            actor,
            input,
            None,
            NewOperationAudit {
                actor_principal_id: Some(actor),
                action: "upsert".to_string(),
                target: "connection_profile".to_string(),
                target_id: None,
                status: "succeeded".to_string(),
                result_code: None,
                row_count: None,
                error_message: None,
                correlation_id: None,
            },
        )
        .await
    }

    pub async fn upsert_connection_profile_with_limit(
        &self,
        tenant: TenantId,
        actor: PrincipalId,
        mut input: NewConnectionProfile,
        max_profiles: Option<u64>,
        audit: NewOperationAudit,
    ) -> Result<ConnectionProfile> {
        if input.credential_mode == CredentialMode::Broker {
            return Err(MetadataError::BrokerCredentialModeUnsupported);
        }
        let credentials = input.credentials.take();
        let mut new_shared_secret_handle = None;
        if input.credential_mode == CredentialMode::Shared {
            if let Some(credentials) = credentials.as_ref() {
                validate_provider_credentials(credentials)?;
                let handle = Uuid::new_v4().to_string();
                self.secrets
                    .put(SECRET_NAMESPACE, &handle, &serde_json::to_vec(credentials)?)
                    .await?;
                new_shared_secret_handle = Some(handle);
            }
        } else if credentials.is_some() {
            return Err(MetadataError::InlineCredentialsRequireSharedMode);
        }

        let now = now_text();
        let configuration_json = serde_json::to_string(&input.configuration)?;
        let tags_json = serde_json::to_string(&input.tags)?;
        let backend = self.backend.clone();
        let db_shared_secret_handle = new_shared_secret_handle.clone();
        let db_result: Result<(ConnectionProfile, Option<String>)> = sqlite_blocking(move || {
            let mut conn = backend.conn()?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            ensure_tenant_admin_locked(&tx, tenant, actor)?;
            let exists = tx
                .query_row(
                    "SELECT 1 FROM connection_profile WHERE tenant_id = ?1 AND name = ?2",
                    params![tenant.0, input.name],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if !exists {
                if let Some(max_profiles) = max_profiles {
                    let count: u64 = tx.query_row(
                        "SELECT COUNT(*) FROM connection_profile WHERE tenant_id = ?1",
                        params![tenant.0],
                        |row| row.get(0),
                    )?;
                    if count >= max_profiles {
                        return Err(MetadataError::ConnectionProfileLimitReached(tenant));
                    }
                }
            }
            let old_shared_secret_handle: Option<String> = tx
                .query_row(
                    "SELECT shared_secret_handle FROM connection_profile WHERE tenant_id = ?1 AND name = ?2",
                    params![tenant.0, input.name],
                    |row| row.get(0),
                )
                .optional()?
                .flatten();
            let write_result = tx.execute(
                "INSERT INTO connection_profile
                 (tenant_id, name, engine, spec_json, credential_mode, shared_secret_handle,
                  tags_json, created_by, created_at, updated_at, provider_id, configuration_json,
                  semantic_engine)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, ?10, ?4, ?11)
                 ON CONFLICT(tenant_id, name) DO UPDATE SET
                    engine = excluded.engine,
                    spec_json = excluded.spec_json,
                    provider_id = excluded.provider_id,
                    configuration_json = excluded.configuration_json,
                    semantic_engine = excluded.semantic_engine,
                    credential_mode = excluded.credential_mode,
                    shared_secret_handle = CASE
                        WHEN excluded.credential_mode = 'shared'
                        THEN COALESCE(excluded.shared_secret_handle, connection_profile.shared_secret_handle)
                        ELSE NULL
                    END,
                    tags_json = excluded.tags_json,
                    updated_at = excluded.updated_at",
                params![
                    tenant.0,
                    input.name,
                    input
                        .semantic_engine
                        .map_or("postgres", sift_protocol::Engine::as_str),
                    configuration_json,
                    input.credential_mode.as_str(),
                    db_shared_secret_handle.as_deref(),
                    tags_json,
                    actor.0,
                    now,
                    input.provider_id.as_str(),
                    input.semantic_engine.map(sift_protocol::Engine::as_str),
                ],
            );
            if let Err(error) = write_result {
                Err(error.into())
            } else {
                let id = ConnectionProfileId(tx.query_row(
                    "SELECT id FROM connection_profile WHERE tenant_id = ?1 AND name = ?2",
                    params![tenant.0, input.name],
                    |row| row.get(0),
                )?);
                let mut audit = audit;
                audit.actor_principal_id = Some(actor);
                audit.target_id = Some(id.0);
                insert_operation_audit_row(&tx, &audit)?;
                tx.commit()?;
                let profile = connection_profile_by_id_locked(&conn, id)?;
                Ok((profile, old_shared_secret_handle))
            }
        })
        .await;
        let (profile, old_shared_secret_handle) = match db_result {
            Ok(result) => result,
            Err(error) => {
                if let Some(handle) = new_shared_secret_handle.as_deref() {
                    self.delete_secret_best_effort(handle, "upsert_profile_rollback")
                        .await;
                }
                return Err(error);
            }
        };
        if let Some(old) = old_shared_secret_handle.as_deref() {
            if profile.shared_secret_handle.as_deref() != Some(old) {
                self.delete_secret_best_effort(old, "upsert_profile_replace_shared_secret")
                    .await;
            }
        }
        Ok(profile)
    }

    /// Delete a secret handle from the store, logging on failure instead
    /// of silently dropping the error. A failed delete here leaves an
    /// *orphaned secret*: the DB no longer references it but the bytes
    /// persist in the store. These calls are all cleanup after the DB row
    /// has already been committed (or after a failed insert), so the
    /// caller can't meaningfully recover — but the operator should at
    /// least see it. `context` names the call site for triage.
    async fn delete_secret_best_effort(&self, handle: &str, context: &str) {
        if let Err(error) = self.secrets.delete(SECRET_NAMESPACE, handle).await {
            tracing::warn!(
                %error,
                handle,
                context,
                "orphaned secret: deleting handle from secret store failed"
            );
        }
    }

    async fn delete_password_secret_best_effort(&self, handle: &str, context: &str) {
        if let Err(error) = self.secrets.delete(PASSWORD_SECRET_NAMESPACE, handle).await {
            tracing::warn!(
                %error,
                handle,
                context,
                "orphaned password verifier: deleting handle from secret store failed"
            );
        }
    }

    async fn delete_oauth_secret_best_effort(&self, handle: &str, context: &str) {
        if let Err(error) = self.secrets.delete(OAUTH_SECRET_NAMESPACE, handle).await {
            tracing::warn!(%error, handle, context, "orphaned OAuth secret handle");
        }
    }

    /// Delete a connection profile and write its audit row in the **same
    /// transaction** as the deletion (P1-meta-4). Secret-store cleanup
    /// still happens after commit (it is not transactional), but the
    /// durable audit trail for the deletion itself is now atomic with the
    /// row removal. The caller must skip the async durable-audit enqueue
    /// on success (see `SessionStore::push_operation_local`).
    pub async fn delete_connection_profile(
        &self,
        tenant: TenantId,
        actor: PrincipalId,
        id: ConnectionProfileId,
        mut audit: NewOperationAudit,
    ) -> Result<()> {
        let backend = self.backend.clone();
        let handles = sqlite_blocking(move || {
            let mut conn = backend.conn()?;
            let tx = conn.transaction()?;
            ensure_tenant_admin_locked(&tx, tenant, actor)?;
            let mut handles = Vec::new();
            let managed = tx.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM instance_managed_resource
                    WHERE resource_kind = 'connection' AND row_id = ?1
                )",
                params![id.0],
                |row| row.get::<_, bool>(0),
            )?;
            if managed {
                return Err(MetadataError::ConnectionProfileManaged(id));
            }
            if let Some(handle) = tx
                .query_row(
                    "SELECT shared_secret_handle FROM connection_profile WHERE tenant_id = ?1 AND id = ?2",
                    params![tenant.0, id.0],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten()
            {
                handles.push(handle);
            }
            {
                let mut stmt = tx.prepare(
                    "SELECT secret_handle FROM connection_credential WHERE connection_profile_id = ?1",
                )?;
                let credential_handles = rows(stmt.query_map(params![id.0], |row| row.get(0))?)?;
                handles.extend(credential_handles);
            }
            let deleted = tx.execute(
                "DELETE FROM connection_profile WHERE tenant_id = ?1 AND id = ?2",
                params![tenant.0, id.0],
            )?;
            if deleted == 0 {
                return Err(MetadataError::ConnectionProfileNotFound(id));
            }
            audit.actor_principal_id = Some(actor);
            insert_operation_audit_row(&tx, &audit)?;
            tx.commit()?;
            Ok(handles)
        })
        .await?;
        for handle in handles {
            self.delete_secret_best_effort(&handle, "delete_connection_profile")
                .await;
        }
        Ok(())
    }

    /// Set (or replace) a per-user credential and write its audit row in
    /// the **same transaction** as the credential upsert (P1-meta-4). The
    /// secret bytes are persisted to the secret store first (that write is
    /// not transactional); the DB row and its audit row then commit
    /// atomically. The caller must skip the async durable-audit enqueue on
    /// success (see `SessionStore::push_operation_local`).
    pub async fn set_per_user_credential(
        &self,
        profile_id: ConnectionProfileId,
        principal_id: PrincipalId,
        credentials: &serde_json::Value,
        audit: NewOperationAudit,
    ) -> Result<()> {
        validate_provider_credentials(credentials)?;
        let handle = Uuid::new_v4().to_string();
        self.secrets
            .put(SECRET_NAMESPACE, &handle, &serde_json::to_vec(credentials)?)
            .await?;
        let now = now_text();
        let backend = self.backend.clone();
        let db_handle = handle.clone();
        let db_result: Result<Option<String>> = sqlite_blocking(move || {
            let mut conn = backend.conn()?;
            let tx = conn.transaction()?;
            let (tenant, credential_mode): (i64, String) = tx
                .query_row(
                    "SELECT tenant_id, credential_mode FROM connection_profile WHERE id = ?1",
                    params![profile_id.0],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?
                .ok_or(MetadataError::ConnectionProfileNotFound(profile_id))?;
            let actual = schema::parse_credential_mode(credential_mode)?;
            if actual != CredentialMode::PerUser {
                return Err(MetadataError::CredentialModeMismatch {
                    profile: profile_id,
                    expected: CredentialMode::PerUser,
                    actual,
                });
            }
            ensure_tenant_membership_locked(&tx, TenantId(tenant), principal_id)?;
            let old_handle: Option<String> = tx
                .query_row(
                    "SELECT secret_handle FROM connection_credential
                     WHERE connection_profile_id = ?1 AND principal_id = ?2",
                    params![profile_id.0, principal_id.0],
                    |row| row.get(0),
                )
                .optional()?;
            let write_result = tx.execute(
                "INSERT INTO connection_credential
                 (connection_profile_id, principal_id, secret_handle, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?4)
                 ON CONFLICT(connection_profile_id, principal_id) DO UPDATE SET
                    secret_handle = excluded.secret_handle,
                    updated_at = excluded.updated_at",
                params![profile_id.0, principal_id.0, db_handle, now],
            );
            if let Err(error) = write_result {
                Err(error.into())
            } else {
                insert_operation_audit_row(&tx, &audit)?;
                tx.commit()?;
                Ok(old_handle)
            }
        })
        .await;
        let old_handle = match db_result {
            Ok(old_handle) => old_handle,
            Err(error) => {
                self.delete_secret_best_effort(&handle, "set_per_user_credential_rollback")
                    .await;
                return Err(error);
            }
        };
        if let Some(old) = old_handle.as_deref() {
            if old != handle {
                self.delete_secret_best_effort(old, "set_per_user_credential_replace")
                    .await;
            }
        }
        Ok(())
    }

    pub async fn resolve_provider_connection(
        &self,
        tenant: TenantId,
        principal: PrincipalId,
        id: ConnectionProfileId,
    ) -> Result<(
        serde_json::Value,
        std::collections::HashMap<String, Vec<u8>>,
    )> {
        let backend = self.backend.clone();
        let (profile, handle, secret_namespace) = sqlite_blocking(move || {
            let conn = backend.conn()?;
            let profile = connection_profile_by_id_locked(&conn, id)?;
            if profile.tenant_id != tenant {
                return Err(MetadataError::TenantMismatch(id, tenant));
            }
            let vault_secret = conn
                .query_row(
                    "SELECT i.vault_id, v.secret_handle
                     FROM vault_connection_binding b
                     JOIN vault_item i ON i.id = b.item_id
                     JOIN vault_item_version v
                       ON v.item_id = i.id AND v.version = i.head_version
                     WHERE b.connection_profile_id = ?1",
                    params![id.0],
                    |row| {
                        Ok((
                            sift_api_types::VaultId(row.get(0)?),
                            row.get::<_, Option<String>>(1)?,
                        ))
                    },
                )
                .optional()?;
            if let Some((vault_id, handle)) = vault_secret {
                crate::vault::require_capability(
                    &conn,
                    vault_id,
                    sift_api_types::PrincipalId(principal.0),
                    |capabilities| capabilities.use_secret,
                )?;
                let handle = handle.ok_or(MetadataError::MissingCredential(id, principal))?;
                return Ok((profile, Some(handle), crate::vault::VAULT_SECRET_NAMESPACE));
            }
            let handle = match profile.credential_mode {
                CredentialMode::Shared => profile.shared_secret_handle.clone(),
                CredentialMode::PerUser => conn
                    .query_row(
                        "SELECT secret_handle FROM connection_credential
                         WHERE connection_profile_id = ?1 AND principal_id = ?2",
                        params![id.0, principal.0],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?
                    .ok_or(MetadataError::MissingCredential(id, principal))
                    .map(Some)?,
                CredentialMode::Broker => {
                    return Err(MetadataError::BrokerCredentialUnsupported(id))
                }
            };
            let instance_managed = conn.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM instance_managed_resource
                    WHERE resource_kind = 'connection' AND row_id = ?1
                )",
                params![id.0],
                |row| row.get::<_, bool>(0),
            )?;
            Ok((
                profile,
                handle,
                if instance_managed {
                    instance_manifest::INSTANCE_SECRET_NAMESPACE
                } else {
                    SECRET_NAMESPACE
                },
            ))
        })
        .await?;
        let mut credentials = std::collections::HashMap::new();
        if let Some(handle) = handle {
            let secret = self
                .secrets
                .get(secret_namespace, &handle)
                .await?
                .ok_or(MetadataError::MissingCredential(id, principal))?;
            let object: serde_json::Value = serde_json::from_slice(&secret)?;
            let object = object
                .as_object()
                .ok_or(MetadataError::InvalidCredentialObject)?;
            for (name, value) in object {
                let bytes = match value {
                    serde_json::Value::String(value) => value.as_bytes().to_vec(),
                    value => serde_json::to_vec(value)?,
                };
                credentials.insert(name.clone(), bytes);
            }
        }
        Ok((profile.configuration, credentials))
    }

    pub fn create_room(
        &self,
        tenant: TenantId,
        actor: PrincipalId,
        input: NewRoom,
    ) -> Result<Room> {
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        ensure_tenant_member_role_locked(&tx, tenant, actor)?;
        tx.execute(
            "INSERT INTO room (tenant_id, name, kind, created_by, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![tenant.0, input.name, input.kind.as_str(), actor.0, now],
        )?;
        let room_id = RoomId(tx.last_insert_rowid());
        tx.execute(
            "INSERT INTO room_member (room_id, principal_id, role, joined_at)
             VALUES (?1, ?2, 'owner', ?3)",
            params![room_id.0, actor.0, now],
        )?;
        tx.commit()?;
        self.room_by_id_locked(&conn, room_id)
    }

    pub fn list_rooms_for_principal(
        &self,
        tenant: TenantId,
        principal: PrincipalId,
    ) -> Result<Vec<Room>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT r.id, r.tenant_id, r.name, r.kind, r.created_by, r.created_at, r.updated_at,
                    r.bound_connection_profile_id, r.bound_connection_by
             FROM room r
             JOIN room_member rm ON rm.room_id = r.id
             WHERE r.tenant_id = ?1 AND rm.principal_id = ?2
             ORDER BY r.updated_at DESC, r.id DESC",
        )?;
        let rooms = rows(stmt.query_map(params![tenant.0, principal.0], room_from_row)?);
        rooms
    }

    pub fn get_room(&self, id: RoomId) -> Result<Room> {
        let conn = self.conn()?;
        self.room_by_id_locked(&conn, id)
    }

    /// Bind a connection profile to a room (ADR-036). Owner-gated; the profile
    /// must belong to the room's tenant. Its credentials become the room's
    /// server-owned connection until unbound.
    pub fn bind_room_connection(
        &self,
        room: RoomId,
        actor: PrincipalId,
        profile: ConnectionProfileId,
        audit: NewOperationAudit,
    ) -> Result<Room> {
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let tenant = ensure_room_owner_locked(&tx, room, actor)?;
        let cp = connection_profile_by_id_locked(&tx, profile)?;
        if cp.tenant_id != tenant {
            return Err(MetadataError::TenantMismatch(profile, tenant));
        }
        tx.execute(
            "UPDATE room SET bound_connection_profile_id = ?1, bound_connection_by = ?2, updated_at = ?3 WHERE id = ?4",
            params![profile.0, actor.0, now, room.0],
        )?;
        let mut audit = audit;
        audit.actor_principal_id = Some(actor);
        insert_operation_audit_row(&tx, &audit)?;
        tx.commit()?;
        self.room_by_id_locked(&conn, room)
    }

    /// Unbind the room's connection (ADR-036). Owner-gated. Subsequent
    /// room-scoped execution is rejected until a connection is bound again.
    pub fn unbind_room_connection(
        &self,
        room: RoomId,
        actor: PrincipalId,
        audit: NewOperationAudit,
    ) -> Result<Room> {
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_room_owner_locked(&tx, room, actor)?;
        tx.execute(
            "UPDATE room SET bound_connection_profile_id = NULL, bound_connection_by = NULL, updated_at = ?1 WHERE id = ?2",
            params![now, room.0],
        )?;
        let mut audit = audit;
        audit.actor_principal_id = Some(actor);
        insert_operation_audit_row(&tx, &audit)?;
        tx.commit()?;
        self.room_by_id_locked(&conn, room)
    }

    pub fn list_shared_rooms_for_principal(
        &self,
        tenant: TenantId,
        principal: PrincipalId,
    ) -> Result<Vec<Room>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT r.id, r.tenant_id, r.name, r.kind, r.created_by, r.created_at, r.updated_at,
                    r.bound_connection_profile_id, r.bound_connection_by
             FROM room r
             JOIN room_member rm ON rm.room_id = r.id
             WHERE r.tenant_id = ?1 AND rm.principal_id = ?2 AND r.kind = 'shared'
             ORDER BY r.updated_at DESC, r.id DESC",
        )?;
        let rooms = rows(stmt.query_map(params![tenant.0, principal.0], room_from_row)?);
        rooms
    }

    pub fn add_room_member_authorized(
        &self,
        room: RoomId,
        actor: PrincipalId,
        principal: PrincipalId,
        role: RoomRole,
        audit: NewOperationAudit,
    ) -> Result<RoomMember> {
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let tenant = ensure_room_owner_locked(&tx, room, actor)?;
        ensure_principal_tenant_member_locked(&tx, tenant, principal)?;
        ensure_room_keeps_owner_locked(&tx, room, principal, Some(&role))?;
        tx.execute(
            "INSERT INTO room_member (room_id, principal_id, role, joined_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(room_id, principal_id) DO UPDATE SET role = excluded.role",
            params![room.0, principal.0, role.as_str(), now],
        )?;
        let mut audit = audit;
        audit.actor_principal_id = Some(actor);
        insert_operation_audit_row(&tx, &audit)?;
        tx.commit()?;
        self.room_member_locked(&conn, room, principal)
    }

    pub fn list_room_members(&self, room: RoomId) -> Result<Vec<RoomMember>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT room_id, principal_id, role, joined_at
             FROM room_member
             WHERE room_id = ?1
             ORDER BY joined_at, principal_id",
        )?;
        let members = rows(stmt.query_map(params![room.0], room_member_from_row)?);
        members
    }

    pub fn get_room_member(
        &self,
        room: RoomId,
        principal: PrincipalId,
    ) -> Result<Option<RoomMember>> {
        let conn = self.conn()?;
        self.room_member_optional_locked(&conn, room, principal)
    }

    pub fn remove_room_member_authorized(
        &self,
        room: RoomId,
        actor: PrincipalId,
        principal: PrincipalId,
        audit: NewOperationAudit,
    ) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_room_owner_locked(&tx, room, actor)?;
        ensure_room_keeps_owner_locked(&tx, room, principal, None)?;
        let deleted = tx.execute(
            "DELETE FROM room_member WHERE room_id = ?1 AND principal_id = ?2",
            params![room.0, principal.0],
        )?;
        if deleted == 0 {
            return Err(MetadataError::RoomMemberNotFound { room, principal });
        }
        let mut audit = audit;
        audit.actor_principal_id = Some(actor);
        insert_operation_audit_row(&tx, &audit)?;
        tx.commit()?;
        Ok(())
    }

    pub fn leave_room_authorized(
        &self,
        room: RoomId,
        principal: PrincipalId,
        audit: NewOperationAudit,
    ) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_room_keeps_owner_locked(&tx, room, principal, None)?;
        let deleted = tx.execute(
            "DELETE FROM room_member WHERE room_id = ?1 AND principal_id = ?2",
            params![room.0, principal.0],
        )?;
        if deleted == 0 {
            return Err(MetadataError::RoomMemberNotFound { room, principal });
        }
        let mut audit = audit;
        audit.actor_principal_id = Some(principal);
        insert_operation_audit_row(&tx, &audit)?;
        tx.commit()?;
        Ok(())
    }

    pub fn delete_room(&self, room: RoomId) -> Result<()> {
        let conn = self.conn()?;
        let deleted = conn.execute("DELETE FROM room WHERE id = ?1", params![room.0])?;
        if deleted == 0 {
            return Err(MetadataError::RoomNotFound(room));
        }
        Ok(())
    }

    pub fn create_document(&self, room: RoomId, input: NewDocument) -> Result<Document> {
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let document_id = allocate_document_id(&tx)?;
        tx.execute(
            "INSERT INTO document
             (id, room_id, kind, title, crdt_type, crdt_state, crdt_format_version, snapshot_version,
              position, connection_profile_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'loro', ?5, 1, ?6, ?7, ?8, ?9, ?9)",
            params![
                document_id.0,
                room.0,
                input.kind,
                input.title,
                input.crdt_state,
                input.snapshot_version,
                input.position,
                input.connection_profile_id.map(|id| id.0),
                now
            ],
        )?;
        let document = self.document_by_id_locked(&tx, document_id)?;
        tx.commit()?;
        Ok(document)
    }

    /// Insert a pre-Phase-G legacy document row (`crdt_format_version = 0`,
    /// `crdt_state` holding raw text bytes). Such rows are only ever produced by
    /// databases created before the Loro migration; this seam lets the
    /// server-side upgrader be exercised end-to-end.
    #[doc(hidden)]
    pub fn insert_legacy_document(
        &self,
        room: RoomId,
        title: &str,
        raw_text: &[u8],
    ) -> Result<DocumentId> {
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let document_id = allocate_document_id(&tx)?;
        tx.execute(
            "INSERT INTO document
             (id, room_id, kind, title, crdt_type, crdt_state, crdt_format_version, snapshot_version,
              position, connection_profile_id, created_at, updated_at)
             VALUES (?1, ?2, 'sql', ?3, 'loro', ?4, 0, x'', 0, NULL, ?5, ?5)",
            params![document_id.0, room.0, title, raw_text, now],
        )?;
        tx.commit()?;
        Ok(document_id)
    }

    pub fn create_document_for_principal(
        &self,
        room: RoomId,
        principal: PrincipalId,
        input: NewDocument,
    ) -> Result<Document> {
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let tenant_id: Option<i64> = tx
            .query_row(
                "SELECT r.tenant_id FROM room r
                 JOIN room_member m ON m.room_id = r.id
                 WHERE r.id = ?1 AND m.principal_id = ?2 AND m.role IN ('owner', 'editor')",
                params![room.0, principal.0],
                |row| row.get(0),
            )
            .optional()?;
        let tenant_id = tenant_id.ok_or(MetadataError::RoomNotFound(room))?;
        if let Some(profile) = input.connection_profile_id {
            let valid: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM connection_profile WHERE id = ?1 AND tenant_id = ?2)",
                params![profile.0, tenant_id],
                |row| row.get(0),
            )?;
            if !valid {
                return Err(MetadataError::TenantMismatch(profile, TenantId(tenant_id)));
            }
        }
        let document_id = allocate_document_id(&tx)?;
        tx.execute(
            "INSERT INTO document
             (id, room_id, kind, title, crdt_type, crdt_state, crdt_format_version, snapshot_version,
              position, connection_profile_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'loro', ?5, 1, ?6, ?7, ?8, ?9, ?9)",
            params![
                document_id.0,
                room.0,
                input.kind,
                input.title,
                input.crdt_state,
                input.snapshot_version,
                input.position,
                input.connection_profile_id.map(|id| id.0),
                now
            ],
        )?;
        let document = self.document_by_id_locked(&tx, document_id)?;
        tx.commit()?;
        Ok(document)
    }

    pub fn list_documents(&self, room: RoomId) -> Result<Vec<Document>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, room_id, kind, title, crdt_type, crdt_state, position,
                    connection_profile_id, created_at, updated_at,
                    crdt_format_version, snapshot_seq, next_update_seq, snapshot_version
             FROM document
             WHERE room_id = ?1
             ORDER BY position, id",
        )?;
        let documents = rows(stmt.query_map(params![room.0], document_from_row)?);
        documents
    }

    pub fn list_documents_for_principal(
        &self,
        room: RoomId,
        principal: PrincipalId,
    ) -> Result<Vec<Document>> {
        let conn = self.conn()?;
        let member: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM room_member WHERE room_id = ?1 AND principal_id = ?2)",
            params![room.0, principal.0],
            |row| row.get(0),
        )?;
        if !member {
            return Err(MetadataError::RoomNotFound(room));
        }
        let mut stmt = conn.prepare(
            "SELECT d.id, d.room_id, d.kind, d.title, d.crdt_type, d.crdt_state, d.position,
                    d.connection_profile_id, d.created_at, d.updated_at,
                    d.crdt_format_version, d.snapshot_seq, d.next_update_seq, d.snapshot_version
             FROM document d
             JOIN room_member m ON m.room_id = d.room_id
             WHERE d.room_id = ?1 AND m.principal_id = ?2
             ORDER BY d.position, d.id",
        )?;
        let documents = rows(stmt.query_map(params![room.0, principal.0], document_from_row)?);
        documents
    }

    pub fn get_document(&self, id: DocumentId) -> Result<Document> {
        let conn = self.conn()?;
        self.document_by_id_locked(&conn, id)
    }

    pub fn get_document_for_principal(
        &self,
        id: DocumentId,
        principal: PrincipalId,
        writable: bool,
    ) -> Result<Document> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT d.id, d.room_id, d.kind, d.title, d.crdt_type, d.crdt_state, d.position,
                    d.connection_profile_id, d.created_at, d.updated_at,
                    d.crdt_format_version, d.snapshot_seq, d.next_update_seq, d.snapshot_version
             FROM document d JOIN room_member m ON m.room_id = d.room_id
             WHERE d.id = ?1 AND m.principal_id = ?2
               AND (?3 = 0 OR m.role IN ('owner', 'editor'))",
            params![id.0, principal.0, writable],
            document_from_row,
        )
        .optional()?
        .ok_or(MetadataError::DocumentNotFound(id))
    }

    pub fn update_document_snapshot(
        &self,
        document: DocumentId,
        crdt_state: Vec<u8>,
    ) -> Result<Document> {
        let now = now_text();
        let conn = self.conn()?;
        let updated = conn.execute(
            "UPDATE document SET crdt_state = ?1, updated_at = ?2 WHERE id = ?3",
            params![crdt_state, now, document.0],
        )?;
        if updated == 0 {
            return Err(MetadataError::DocumentNotFound(document));
        }
        self.document_by_id_locked(&conn, document)
    }

    pub fn update_document_snapshot_for_principal(
        &self,
        document: DocumentId,
        principal: PrincipalId,
        crdt_state: Vec<u8>,
    ) -> Result<Document> {
        let now = now_text();
        let conn = self.conn()?;
        let managed: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM workspace_node WHERE document_id = ?1)",
            params![document.0],
            |row| row.get(0),
        )?;
        if managed {
            return Err(MetadataError::WorkspaceDocumentManaged);
        }
        let updated = conn.execute(
            "UPDATE document SET crdt_state = ?1, updated_at = ?2
             WHERE id = ?3 AND EXISTS (
                 SELECT 1 FROM room_member m
                 WHERE m.room_id = document.room_id AND m.principal_id = ?4
                   AND m.role IN ('owner', 'editor')
             )",
            params![crdt_state, now, document.0, principal.0],
        )?;
        if updated == 0 {
            return Err(MetadataError::DocumentNotFound(document));
        }
        self.document_by_id_locked(&conn, document)
    }

    /// Durably append one validated Loro update and allocate its per-document
    /// server sequence in a single transaction. Returns the assigned sequence.
    ///
    /// Idempotent on `(document_id, update_id, replica_id)`: a retried delivery
    /// of an already-stored update returns the existing sequence and inserts
    /// nothing, so no duplicate row or gap is created.
    pub fn append_document_update(
        &self,
        document: DocumentId,
        update: NewDocumentUpdate,
    ) -> Result<i64> {
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let next_seq: Option<i64> = tx
            .query_row(
                "SELECT next_update_seq FROM document WHERE id = ?1",
                params![document.0],
                |row| row.get(0),
            )
            .optional()?;
        let Some(next_seq) = next_seq else {
            return Err(MetadataError::DocumentNotFound(document));
        };
        if let Some(existing) = tx
            .query_row(
                "SELECT server_seq FROM document_update
                 WHERE document_id = ?1 AND update_id = ?2 AND replica_id = ?3",
                params![document.0, update.update_id, update.replica_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
        {
            return Ok(existing);
        }
        tx.execute(
            "INSERT INTO document_update
             (document_id, server_seq, update_id, replica_id, submitted_by, update_bytes,
              decoded_len, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                document.0,
                next_seq,
                update.update_id,
                update.replica_id,
                update.submitted_by.0,
                update.update_bytes,
                update.decoded_len,
                now,
            ],
        )?;
        tx.execute(
            "UPDATE document SET next_update_seq = ?1, updated_at = ?2 WHERE id = ?3",
            params![next_seq + 1, now, document.0],
        )?;
        tx.commit()?;
        Ok(next_seq)
    }

    /// All durable updates with `server_seq > after_seq`, in sequence order.
    pub fn list_document_updates_since(
        &self,
        document: DocumentId,
        after_seq: i64,
    ) -> Result<Vec<DocumentUpdate>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT server_seq, update_id, replica_id, submitted_by, update_bytes, decoded_len,
                    created_at
             FROM document_update
             WHERE document_id = ?1 AND server_seq > ?2
             ORDER BY server_seq",
        )?;
        let updates =
            rows(stmt.query_map(params![document.0, after_seq], document_update_from_row)?);
        updates
    }

    /// Transactionally replace the stored snapshot and delete every update row
    /// through `through_seq` (compaction). Later rows are left untouched.
    pub fn replace_document_snapshot(
        &self,
        document: DocumentId,
        snapshot_bytes: Vec<u8>,
        snapshot_version: Vec<u8>,
        through_seq: i64,
    ) -> Result<()> {
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let updated = tx.execute(
            "UPDATE document
             SET crdt_state = ?1, snapshot_version = ?2, snapshot_seq = ?3, updated_at = ?4
             WHERE id = ?5",
            params![
                snapshot_bytes,
                snapshot_version,
                through_seq,
                now,
                document.0
            ],
        )?;
        if updated == 0 {
            return Err(MetadataError::DocumentNotFound(document));
        }
        tx.execute(
            "DELETE FROM document_update WHERE document_id = ?1 AND server_seq <= ?2",
            params![document.0, through_seq],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Documents whose `crdt_state` is still raw UTF-8 (legacy format 0),
    /// awaiting the server-side Loro upgrade.
    pub fn list_legacy_documents(&self) -> Result<Vec<Document>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, room_id, kind, title, crdt_type, crdt_state, position,
                    connection_profile_id, created_at, updated_at,
                    crdt_format_version, snapshot_seq, next_update_seq, snapshot_version
             FROM document WHERE crdt_format_version = 0 ORDER BY id",
        )?;
        let documents = rows(stmt.query_map([], document_from_row)?);
        documents
    }

    /// Promote one legacy row to a Loro snapshot (format 1). No-op if the row is
    /// already upgraded or gone. The caller supplies the canonical snapshot it
    /// built from the row's raw text.
    pub fn upgrade_document_to_loro(
        &self,
        document: DocumentId,
        snapshot_bytes: Vec<u8>,
        snapshot_version: Vec<u8>,
    ) -> Result<()> {
        let now = now_text();
        let conn = self.conn()?;
        conn.execute(
            "UPDATE document
             SET crdt_state = ?1, snapshot_version = ?2, crdt_format_version = 1, updated_at = ?3
             WHERE id = ?4 AND crdt_format_version = 0",
            params![snapshot_bytes, snapshot_version, now, document.0],
        )?;
        Ok(())
    }

    /// Atomically upgrade a validated batch of legacy document rows. A
    /// concurrent upgrade or missing row aborts the whole batch.
    pub fn upgrade_documents_to_loro(
        &self,
        documents: &[(DocumentId, Vec<u8>, Vec<u8>)],
    ) -> Result<()> {
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (document, snapshot_bytes, snapshot_version) in documents {
            let updated = tx.execute(
                "UPDATE document
                 SET crdt_state = ?1, snapshot_version = ?2,
                     crdt_format_version = 1, updated_at = ?3
                 WHERE id = ?4 AND crdt_format_version = 0",
                params![snapshot_bytes, snapshot_version, now, document.0],
            )?;
            if updated != 1 {
                return Err(MetadataError::DocumentNotFound(*document));
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn delete_document(&self, document: DocumentId) -> Result<()> {
        let conn = self.conn()?;
        let deleted = conn.execute("DELETE FROM document WHERE id = ?1", params![document.0])?;
        if deleted == 0 {
            return Err(MetadataError::DocumentNotFound(document));
        }
        Ok(())
    }

    pub fn delete_document_for_principal(
        &self,
        document: DocumentId,
        principal: PrincipalId,
    ) -> Result<()> {
        let conn = self.conn()?;
        let managed: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM workspace_node WHERE document_id = ?1)",
            params![document.0],
            |row| row.get(0),
        )?;
        if managed {
            return Err(MetadataError::WorkspaceDocumentManaged);
        }
        let deleted = conn.execute(
            "DELETE FROM document WHERE id = ?1 AND EXISTS (
                 SELECT 1 FROM room_member m
                 WHERE m.room_id = document.room_id AND m.principal_id = ?2
                   AND m.role IN ('owner', 'editor')
             )",
            params![document.0, principal.0],
        )?;
        if deleted == 0 {
            return Err(MetadataError::DocumentNotFound(document));
        }
        Ok(())
    }

    pub fn attach_room(
        &self,
        room: RoomId,
        principal: PrincipalId,
        client_id: &str,
    ) -> Result<RoomAttachment> {
        let now = now_text();
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO room_attachment (room_id, principal_id, client_id, attached_at, detached_at)
             VALUES (?1, ?2, ?3, ?4, NULL)",
            params![room.0, principal.0, client_id, now],
        )?;
        let attachment_id = RoomAttachmentId(conn.last_insert_rowid());
        self.room_attachment_by_id_locked(&conn, attachment_id)
    }

    pub fn detach_room(&self, attachment: RoomAttachmentId) -> Result<Option<RoomAttachment>> {
        let now = now_text();
        let conn = self.conn()?;
        let updated = conn.execute(
            "UPDATE room_attachment
             SET detached_at = ?1
             WHERE id = ?2 AND detached_at IS NULL",
            params![now, attachment.0],
        )?;
        if updated == 0 {
            let exists: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM room_attachment WHERE id = ?1)",
                params![attachment.0],
                |row| row.get(0),
            )?;
            return if exists {
                Ok(None)
            } else {
                Err(MetadataError::RoomAttachmentNotFound(attachment))
            };
        }
        self.room_attachment_by_id_locked(&conn, attachment)
            .map(Some)
    }

    pub fn list_active_room_attachments(&self, room: RoomId) -> Result<Vec<RoomAttachment>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, room_id, principal_id, client_id, attached_at, detached_at
             FROM room_attachment
             WHERE room_id = ?1 AND detached_at IS NULL
             ORDER BY attached_at, id",
        )?;
        let attachments = rows(stmt.query_map(params![room.0], room_attachment_from_row)?);
        attachments
    }

    pub fn record_query_history(&self, input: NewQueryHistory) -> Result<QueryHistory> {
        let now = now_text();
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO query_history
             (principal_id, connection_profile_id, sql_text, started_at, duration_ms,
              row_count, status, error_code, error_message, room_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                input.principal_id.0,
                input.connection_profile_id.map(|id| id.0),
                input.sql_text,
                now,
                input.duration_ms,
                input.row_count,
                input.status.as_str(),
                input.error_code,
                input.error_message,
                input.room_id.map(|id| id.0)
            ],
        )?;
        let history_id = QueryHistoryId(conn.last_insert_rowid());
        self.query_history_by_id_locked(&conn, history_id)
    }

    /// Append a durable operation-audit row. Called on both the success and
    /// failure paths so the audit trail is complete.
    pub fn record_operation_audit(&self, input: NewOperationAudit) -> Result<OperationAudit> {
        let conn = self.conn()?;
        let id = OperationAuditId(insert_operation_audit_row(&conn, &input)?);
        conn.query_row(
            "SELECT id, at, actor_principal_id, action, target, target_id, status,
                    result_code, row_count, error_message, correlation_id
             FROM operation_audit WHERE id = ?1",
            params![id.0],
            operation_audit_from_row,
        )
        .map_err(Into::into)
    }

    pub fn finish_operation_audit(
        &self,
        id: OperationAuditId,
        status: &str,
        result_code: Option<&str>,
        error_message: Option<&str>,
    ) -> Result<OperationAudit> {
        if !matches!(status, "succeeded" | "failed") {
            return Err(MetadataError::InvalidEnum {
                field: "operation_audit.status",
                value: status.into(),
            });
        }
        let conn = self.conn()?;
        let changed = conn.execute(
            "UPDATE operation_audit
             SET status = ?2, result_code = ?3, error_message = ?4
             WHERE id = ?1 AND status = 'started'",
            params![id.0, status, result_code, error_message],
        )?;
        if changed != 1 {
            return Err(MetadataError::InvalidEnum {
                field: "operation_audit.transition",
                value: id.0.to_string(),
            });
        }
        conn.query_row(
            "SELECT id, at, actor_principal_id, action, target, target_id, status,
                    result_code, row_count, error_message, correlation_id
             FROM operation_audit WHERE id = ?1",
            [id.0],
            operation_audit_from_row,
        )
        .map_err(Into::into)
    }

    pub fn list_operation_audit(&self, limit: u32) -> Result<Vec<OperationAudit>> {
        self.list_operation_audit_before(limit, None)
    }

    pub fn list_operation_audit_before(
        &self,
        limit: u32,
        before_id: Option<OperationAuditId>,
    ) -> Result<Vec<OperationAudit>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, at, actor_principal_id, action, target, target_id, status,
                    result_code, row_count, error_message, correlation_id
             FROM operation_audit
             WHERE (?1 IS NULL OR id < ?1)
             ORDER BY id DESC
             LIMIT ?2",
        )?;
        let audit = rows(stmt.query_map(
            params![before_id.map(|id| id.0), limit],
            operation_audit_from_row,
        )?);
        audit
    }

    pub fn list_query_history_for_room(
        &self,
        room: RoomId,
        limit: u32,
    ) -> Result<Vec<QueryHistory>> {
        self.list_query_history_for_room_before(room, limit, None)
    }

    pub fn list_query_history_for_room_before(
        &self,
        room: RoomId,
        limit: u32,
        before_id: Option<QueryHistoryId>,
    ) -> Result<Vec<QueryHistory>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, principal_id, connection_profile_id, sql_text, started_at,
                    duration_ms, row_count, status, error_code, error_message, room_id
             FROM query_history
             WHERE room_id = ?1 AND (?2 IS NULL OR id < ?2)
             ORDER BY id DESC
             LIMIT ?3",
        )?;
        let history = rows(stmt.query_map(
            params![room.0, before_id.map(|id| id.0), limit],
            query_history_from_row,
        )?);
        history
    }

    pub fn list_query_history_for_principal(
        &self,
        principal: PrincipalId,
        limit: u32,
    ) -> Result<Vec<QueryHistory>> {
        self.list_query_history_for_principal_before(principal, limit, None)
    }

    pub fn list_query_history_for_principal_before(
        &self,
        principal: PrincipalId,
        limit: u32,
        before_id: Option<QueryHistoryId>,
    ) -> Result<Vec<QueryHistory>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, principal_id, connection_profile_id, sql_text, started_at,
                    duration_ms, row_count, status, error_code, error_message, room_id
             FROM query_history
             WHERE principal_id = ?1 AND (?2 IS NULL OR id < ?2)
             ORDER BY id DESC
             LIMIT ?3",
        )?;
        let history = rows(stmt.query_map(
            params![principal.0, before_id.map(|id| id.0), limit],
            query_history_from_row,
        )?);
        history
    }

    // -----------------------------------------------------------------
    // Saved queries
    // -----------------------------------------------------------------

    /// Insert a saved query. Caller has already resolved
    /// `owner_principal_id` (None = tenant-shared).
    pub fn insert_saved_query(&self, input: NewSavedQuery) -> Result<SavedQuery> {
        let now = now_text();
        let tags_json = serde_json::to_string(&input.tags).map_err(MetadataError::Json)?;
        let conn = self.conn()?;
        if let Some(profile) = input.connection_profile_id {
            let valid: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM connection_profile WHERE id = ?1 AND tenant_id = ?2)",
                params![profile.0, input.tenant_id.0],
                |row| row.get(0),
            )?;
            if !valid {
                return Err(MetadataError::TenantMismatch(profile, input.tenant_id));
            }
        }
        conn.execute(
            "INSERT INTO saved_query
             (tenant_id, principal_id, name, sql_text, connection_profile_id,
              tags_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![
                input.tenant_id.0,
                input.owner_principal_id.map(|p| p.0),
                input.name,
                input.sql_text,
                input.connection_profile_id.map(|c| c.0),
                tags_json,
                now,
            ],
        )?;
        let id = SavedQueryId(conn.last_insert_rowid());
        self.saved_query_by_id_locked(&conn, id)
    }

    /// Fetch a saved query by id for trusted internal maintenance paths.
    pub fn get_saved_query(&self, id: SavedQueryId) -> Result<SavedQuery> {
        let conn = self.conn()?;
        self.saved_query_by_id_locked(&conn, id)
    }

    pub fn get_saved_query_visible(
        &self,
        id: SavedQueryId,
        tenant: TenantId,
        principal: PrincipalId,
    ) -> Result<SavedQuery> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT id, tenant_id, principal_id, name, sql_text,
                    connection_profile_id, tags_json, created_at, updated_at, revision
             FROM saved_query
             WHERE id = ?1 AND tenant_id = ?2
               AND (principal_id = ?3 OR principal_id IS NULL)",
            params![id.0, tenant.0, principal.0],
            saved_query_from_row,
        )
        .optional()?
        .ok_or(MetadataError::SavedQueryNotFound(id))
    }

    /// List saved queries visible to `principal` in the filter's
    /// tenant. Visibility rule: personal queries owned by
    /// `principal`, OR tenant-shared queries. Filter narrows further
    /// via optional FTS pattern `q`, tag set, and scope.
    pub fn list_saved_queries(
        &self,
        principal: PrincipalId,
        filter: SavedQueryFilter,
    ) -> Result<Vec<SavedQuery>> {
        let conn = self.conn()?;
        // Compose SQL dynamically. Base visibility is fixed; scope,
        // q, and tags are optional refinements.
        let mut sql = String::from(
            "SELECT id, tenant_id, principal_id, name, sql_text,
                    connection_profile_id, tags_json, created_at, updated_at, revision
             FROM saved_query
             WHERE tenant_id = ?
               AND (principal_id = ? OR principal_id IS NULL)",
        );
        let mut params_dyn: Vec<Box<dyn rusqlite::ToSql>> =
            vec![Box::new(filter.tenant_id.0), Box::new(principal.0)];
        match filter.scope {
            Some(SavedQueryScope::Personal) => {
                sql.push_str(" AND principal_id = ?");
                params_dyn.push(Box::new(principal.0));
            }
            Some(SavedQueryScope::Shared) => {
                sql.push_str(" AND principal_id IS NULL");
            }
            Some(SavedQueryScope::All) | None => {}
        }
        if let Some(q) = filter.q.as_ref().filter(|s| !s.trim().is_empty()) {
            // Restrict to FTS matches. Users type free-text; append a
            // trailing `*` to each token so partial words match as
            // prefixes. If all user input is punctuation, keep the
            // filter restrictive instead of turning it into MATCH '*'.
            if let Some(pattern) = fts_pattern(q) {
                sql.push_str(
                    " AND id IN (SELECT rowid FROM saved_query_fts WHERE saved_query_fts MATCH ?)",
                );
                params_dyn.push(Box::new(pattern));
            } else {
                sql.push_str(" AND 0");
            }
        }
        for tag in &filter.tags {
            // tags_json is a JSON array — use json_each to test
            // containment.
            sql.push_str(" AND EXISTS (SELECT 1 FROM json_each(tags_json) WHERE value = ?)");
            params_dyn.push(Box::new(tag.clone()));
        }
        sql.push_str(" ORDER BY updated_at DESC, id DESC");
        let mut stmt = conn.prepare(&sql)?;
        let refs: Vec<&dyn rusqlite::ToSql> = params_dyn.iter().map(|b| b.as_ref()).collect();
        let iter = stmt.query_map(refs.as_slice(), saved_query_from_row)?;
        rows(iter)
    }

    /// Update a saved query. Caller has already checked authorization.
    /// Any `None` field is left unchanged.
    pub fn update_saved_query(
        &self,
        id: SavedQueryId,
        update: UpdateSavedQuery,
    ) -> Result<SavedQuery> {
        let now = now_text();
        let mut conn = self.conn()?;
        // BEGIN IMMEDIATE so the read-modify-write is atomic. Each metadata
        // call runs on its own pooled connection (P1-meta-1), so without a
        // write-locking transaction two concurrent *partial* updates both
        // read the old row and last-writer-wins — e.g. a tags-only update
        // silently clobbers a concurrent sql_text-only update.
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let existing = self.saved_query_by_id_locked(&tx, id)?;
        let name = update.name.unwrap_or(existing.name);
        let sql_text = update.sql_text.unwrap_or(existing.sql_text);
        let connection_profile_id = update
            .connection_profile_id
            .unwrap_or(existing.connection_profile_id);
        if let Some(profile) = connection_profile_id {
            let valid: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM connection_profile WHERE id = ?1 AND tenant_id = ?2)",
                params![profile.0, existing.tenant_id.0],
                |row| row.get(0),
            )?;
            if !valid {
                return Err(MetadataError::TenantMismatch(profile, existing.tenant_id));
            }
        }
        let tags = update.tags.unwrap_or(existing.tags);
        let tags_json = serde_json::to_string(&tags).map_err(MetadataError::Json)?;
        tx.execute(
            "UPDATE saved_query
             SET name = ?1, sql_text = ?2, connection_profile_id = ?3,
                 tags_json = ?4, updated_at = ?5, revision = revision + 1
             WHERE id = ?6",
            params![
                name,
                sql_text,
                connection_profile_id.map(|c| c.0),
                tags_json,
                now,
                id.0,
            ],
        )?;
        let updated = self.saved_query_by_id_locked(&tx, id)?;
        tx.commit()?;
        Ok(updated)
    }

    /// Delete a saved query. Caller has already checked authorization.
    /// Returns `true` if a row was deleted, `false` if the id was
    /// absent (idempotent).
    pub fn delete_saved_query(&self, id: SavedQueryId) -> Result<bool> {
        let conn = self.conn()?;
        let deleted = conn.execute("DELETE FROM saved_query WHERE id = ?1", params![id.0])?;
        Ok(deleted > 0)
    }

    pub fn update_saved_query_authorized(
        &self,
        id: SavedQueryId,
        tenant: TenantId,
        principal: PrincipalId,
        tenant_admin: bool,
        expected_revision: u64,
        update: UpdateSavedQuery,
    ) -> Result<SavedQuery> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = tx
            .query_row(
                "SELECT id, tenant_id, principal_id, name, sql_text,
                        connection_profile_id, tags_json, created_at, updated_at, revision
                 FROM saved_query
                 WHERE id = ?1 AND tenant_id = ?2
                   AND (principal_id = ?3 OR (principal_id IS NULL AND ?4))",
                params![id.0, tenant.0, principal.0, tenant_admin],
                saved_query_from_row,
            )
            .optional()?
            .ok_or(MetadataError::SavedQueryNotFound(id))?;
        if existing.revision != expected_revision {
            return Err(MetadataError::SavedQueryRevisionConflict {
                expected: expected_revision,
                current: existing.revision,
            });
        }
        let name = update.name.unwrap_or(existing.name);
        let sql_text = update.sql_text.unwrap_or(existing.sql_text);
        let profile = update
            .connection_profile_id
            .unwrap_or(existing.connection_profile_id);
        if let Some(profile) = profile {
            let valid: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM connection_profile WHERE id = ?1 AND tenant_id = ?2)",
                params![profile.0, tenant.0],
                |row| row.get(0),
            )?;
            if !valid {
                return Err(MetadataError::TenantMismatch(profile, tenant));
            }
        }
        let tags = update.tags.unwrap_or(existing.tags);
        tx.execute(
            "UPDATE saved_query SET name = ?1, sql_text = ?2, connection_profile_id = ?3,
                 tags_json = ?4, updated_at = ?5, revision = revision + 1 WHERE id = ?6",
            params![
                name,
                sql_text,
                profile.map(|profile| profile.0),
                serde_json::to_string(&tags)?,
                now_text(),
                id.0
            ],
        )?;
        let updated = self.saved_query_by_id_locked(&tx, id)?;
        tx.commit()?;
        Ok(updated)
    }

    pub fn delete_saved_query_authorized(
        &self,
        id: SavedQueryId,
        tenant: TenantId,
        principal: PrincipalId,
        tenant_admin: bool,
        expected_revision: u64,
    ) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = tx
            .query_row(
                "SELECT revision FROM saved_query
                 WHERE id = ?1 AND tenant_id = ?2
                   AND (principal_id = ?3 OR (principal_id IS NULL AND ?4))",
                params![id.0, tenant.0, principal.0, tenant_admin],
                |row| row.get::<_, u64>(0),
            )
            .optional()?
            .ok_or(MetadataError::SavedQueryNotFound(id))?;
        if current != expected_revision {
            return Err(MetadataError::SavedQueryRevisionConflict {
                expected: expected_revision,
                current,
            });
        }
        tx.execute(
            "DELETE FROM saved_query
             WHERE id = ?1 AND tenant_id = ?2
               AND (principal_id = ?3 OR (principal_id IS NULL AND ?4))",
            params![id.0, tenant.0, principal.0, tenant_admin],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn saved_query_by_id_locked(&self, conn: &Connection, id: SavedQueryId) -> Result<SavedQuery> {
        conn.query_row(
            "SELECT id, tenant_id, principal_id, name, sql_text,
                    connection_profile_id, tags_json, created_at, updated_at, revision
             FROM saved_query WHERE id = ?1",
            params![id.0],
            saved_query_from_row,
        )
        .optional()?
        .ok_or(MetadataError::SavedQueryNotFound(id))
    }

    fn api_token_by_id_locked(&self, conn: &Connection, id: ApiTokenId) -> Result<ApiTokenRow> {
        conn.query_row(
            "SELECT id, principal_id, tenant_id, name, created_at, updated_at,
                    last_used_at, expires_at, revoked_at
             FROM api_token WHERE id = ?1",
            params![id.0],
            api_token_from_row,
        )
        .map_err(Into::into)
    }

    fn connection_profile_by_id_locked(
        &self,
        conn: &Connection,
        id: ConnectionProfileId,
    ) -> Result<ConnectionProfile> {
        connection_profile_by_id_locked(conn, id)
    }

    fn room_by_id_locked(&self, conn: &Connection, id: RoomId) -> Result<Room> {
        conn.query_row(
            "SELECT id, tenant_id, name, kind, created_by, created_at, updated_at,
                    bound_connection_profile_id, bound_connection_by
             FROM room WHERE id = ?1",
            params![id.0],
            room_from_row,
        )
        .optional()?
        .ok_or(MetadataError::RoomNotFound(id))
    }

    fn room_member_locked(
        &self,
        conn: &Connection,
        room: RoomId,
        principal: PrincipalId,
    ) -> Result<RoomMember> {
        self.room_member_optional_locked(conn, room, principal)?
            .ok_or(MetadataError::RoomNotFound(room))
    }

    fn room_member_optional_locked(
        &self,
        conn: &Connection,
        room: RoomId,
        principal: PrincipalId,
    ) -> Result<Option<RoomMember>> {
        conn.query_row(
            "SELECT room_id, principal_id, role, joined_at
             FROM room_member WHERE room_id = ?1 AND principal_id = ?2",
            params![room.0, principal.0],
            room_member_from_row,
        )
        .optional()
        .map_err(Into::into)
    }

    fn document_by_id_locked(&self, conn: &Connection, id: DocumentId) -> Result<Document> {
        conn.query_row(
            "SELECT id, room_id, kind, title, crdt_type, crdt_state, position,
                    connection_profile_id, created_at, updated_at,
                    crdt_format_version, snapshot_seq, next_update_seq, snapshot_version
             FROM document WHERE id = ?1",
            params![id.0],
            document_from_row,
        )
        .optional()?
        .ok_or(MetadataError::DocumentNotFound(id))
    }

    fn room_attachment_by_id_locked(
        &self,
        conn: &Connection,
        id: RoomAttachmentId,
    ) -> Result<RoomAttachment> {
        conn.query_row(
            "SELECT id, room_id, principal_id, client_id, attached_at, detached_at
             FROM room_attachment WHERE id = ?1",
            params![id.0],
            room_attachment_from_row,
        )
        .optional()?
        .ok_or(MetadataError::RoomAttachmentNotFound(id))
    }

    fn query_history_by_id_locked(
        &self,
        conn: &Connection,
        id: QueryHistoryId,
    ) -> Result<QueryHistory> {
        conn.query_row(
            "SELECT id, principal_id, connection_profile_id, sql_text, started_at,
                    duration_ms, row_count, status, error_code, error_message, room_id
             FROM query_history WHERE id = ?1",
            params![id.0],
            query_history_from_row,
        )
        .map_err(Into::into)
    }
}

fn configure_connection(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(())
}

fn now_text() -> String {
    Utc::now().to_rfc3339()
}

fn allocate_document_id(conn: &Connection) -> Result<DocumentId> {
    conn.execute("INSERT INTO document_id_allocator DEFAULT VALUES", [])?;
    let id = DocumentId(conn.last_insert_rowid());
    conn.execute(
        "DELETE FROM document_id_allocator WHERE id = ?1",
        params![id.0],
    )?;
    Ok(id)
}

fn parse_time(value: String) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&value)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|source| MetadataError::InvalidTimestamp { value, source })
}

fn parse_optional_time(value: Option<String>) -> Result<Option<DateTime<Utc>>> {
    value.map(parse_time).transpose()
}

fn rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
) -> Result<Vec<T>> {
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

async fn sqlite_blocking<T>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T>
where
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|error| MetadataError::BlockingTask(error.to_string()))?
}

fn connection_profile_by_id_locked(
    conn: &Connection,
    id: ConnectionProfileId,
) -> Result<ConnectionProfile> {
    conn.query_row(
        "SELECT id, tenant_id, name, engine, spec_json, credential_mode,
                    shared_secret_handle, tags_json, created_by, created_at, updated_at,
                    policy_json, policy_revision, provider_id, configuration_json,
                    semantic_engine
             FROM connection_profile WHERE id = ?1",
        params![id.0],
        connection_profile_from_row,
    )
    .optional()?
    .ok_or(MetadataError::ConnectionProfileNotFound(id))
}

fn ensure_room_owner_locked(
    conn: &Connection,
    room: RoomId,
    actor: PrincipalId,
) -> Result<TenantId> {
    conn.query_row(
        "SELECT r.tenant_id
         FROM room r
         JOIN room_member rm ON rm.room_id = r.id
         JOIN principal p ON p.id = rm.principal_id
         WHERE r.id = ?1 AND rm.principal_id = ?2 AND rm.role = 'owner'
           AND p.disabled_at IS NULL",
        params![room.0, actor.0],
        |row| row.get::<_, i64>(0).map(TenantId),
    )
    .optional()?
    .ok_or(MetadataError::RoomOwnerRequired {
        room,
        principal: actor,
    })
}

fn ensure_principal_tenant_member_locked(
    conn: &Connection,
    tenant: TenantId,
    principal: PrincipalId,
) -> Result<()> {
    let member: bool = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM membership m
            JOIN principal p ON p.id = m.principal_id
            WHERE m.tenant_id = ?1 AND m.principal_id = ?2
              AND p.disabled_at IS NULL
         )",
        params![tenant.0, principal.0],
        |row| row.get(0),
    )?;
    if member {
        Ok(())
    } else {
        Err(MetadataError::TenantMembershipRequired { tenant, principal })
    }
}

fn ensure_room_keeps_owner_locked(
    conn: &Connection,
    room: RoomId,
    principal: PrincipalId,
    replacement: Option<&RoomRole>,
) -> Result<()> {
    let current_role: Option<String> = conn
        .query_row(
            "SELECT role FROM room_member WHERE room_id = ?1 AND principal_id = ?2",
            params![room.0, principal.0],
            |row| row.get(0),
        )
        .optional()?;
    let remains_owner = matches!(replacement, Some(RoomRole::Owner));
    if current_role.as_deref() == Some("owner") && !remains_owner {
        let owners: i64 = conn.query_row(
            "SELECT COUNT(*) FROM room_member WHERE room_id = ?1 AND role = 'owner'",
            params![room.0],
            |row| row.get(0),
        )?;
        if owners <= 1 {
            return Err(MetadataError::FinalRoomOwner(room));
        }
    }
    Ok(())
}

fn ensure_tenant_admin_locked(
    conn: &Connection,
    tenant: TenantId,
    actor: PrincipalId,
) -> Result<()> {
    let role: Option<String> = conn
        .query_row(
            "SELECT m.role
             FROM membership m
             JOIN principal p ON p.id = m.principal_id
             WHERE m.tenant_id = ?1 AND m.principal_id = ?2
               AND p.disabled_at IS NULL",
            params![tenant.0, actor.0],
            |row| row.get(0),
        )
        .optional()?;
    if matches!(role.as_deref(), Some("owner" | "admin")) {
        Ok(())
    } else {
        Err(MetadataError::TenantAdminRequired)
    }
}

fn ensure_tenant_membership_locked(
    conn: &Connection,
    tenant: TenantId,
    principal: PrincipalId,
) -> Result<()> {
    let exists = conn
        .query_row(
            "SELECT 1
             FROM membership m
             JOIN principal p ON p.id = m.principal_id
             WHERE m.tenant_id = ?1 AND m.principal_id = ?2
               AND p.disabled_at IS NULL",
            params![tenant.0, principal.0],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if exists {
        Ok(())
    } else {
        Err(MetadataError::TenantMembershipRequired { tenant, principal })
    }
}

fn ensure_tenant_member_role_locked(
    conn: &Connection,
    tenant: TenantId,
    actor: PrincipalId,
) -> Result<()> {
    let role: Option<String> = conn
        .query_row(
            "SELECT m.role
             FROM membership m
             JOIN principal p ON p.id = m.principal_id
             WHERE m.tenant_id = ?1 AND m.principal_id = ?2
               AND p.disabled_at IS NULL",
            params![tenant.0, actor.0],
            |row| row.get(0),
        )
        .optional()?;
    if matches!(role.as_deref(), Some("owner" | "admin" | "member")) {
        Ok(())
    } else {
        Err(MetadataError::TenantMemberRequired)
    }
}

fn ensure_instance_admin_locked(conn: &Connection, actor: PrincipalId) -> Result<()> {
    let active: bool = conn.query_row(
        "SELECT EXISTS(
                SELECT 1 FROM principal
                WHERE id = ?1 AND is_instance_admin = 1 AND disabled_at IS NULL
             )",
        params![actor.0],
        |row| row.get(0),
    )?;
    if active {
        Ok(())
    } else {
        Err(MetadataError::InstanceAdminRequired)
    }
}

fn validate_connection_policy_input(input: &UpdateConnectionPolicyRequest) -> Result<()> {
    if input
        .allowed_ops
        .as_ref()
        .is_some_and(|operations| operations.len() > sift_protocol::OperationKind::ALL.len())
        || input.blocked_ops.len() > sift_protocol::OperationKind::ALL.len()
    {
        return Err(MetadataError::InvalidEnum {
            field: "connection_profile.policy.operations",
            value: "too many operation entries".to_string(),
        });
    }
    if let Some(selectors) = &input.allowed_schemas {
        if selectors.len() > 256 {
            return Err(MetadataError::InvalidEnum {
                field: "connection_profile.policy.allowed_schemas",
                value: "too many schema selectors".to_string(),
            });
        }
        for selector in selectors {
            let invalid_schema = selector.schema.trim().is_empty()
                || selector.schema.len() > 128
                || selector.schema.contains('\0');
            let invalid_catalog = selector.catalog.as_ref().is_some_and(|catalog| {
                catalog.trim().is_empty() || catalog.len() > 128 || catalog.contains('\0')
            });
            if invalid_schema || invalid_catalog {
                return Err(MetadataError::InvalidEnum {
                    field: "connection_profile.policy.allowed_schemas",
                    value: "schema selectors must contain bounded, non-empty identifiers"
                        .to_string(),
                });
            }
        }
    }
    Ok(())
}

fn principal_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Principal> {
    principal_from_row_offset(row, 0)
}

fn principal_from_row_offset(
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> rusqlite::Result<Principal> {
    Ok(Principal {
        id: PrincipalId(row.get(offset)?),
        external_id: row.get(offset + 1)?,
        display_name: row.get(offset + 2)?,
        email: row.get(offset + 3)?,
        avatar_url: row.get(offset + 4)?,
        disabled_at: parse_optional_time_sql(row.get(offset + 5)?)?,
        is_instance_admin: row.get(offset + 6)?,
        created_at: parse_time_sql(row.get(offset + 7)?)?,
        updated_at: parse_time_sql(row.get(offset + 8)?)?,
    })
}

fn auth_identity_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AuthIdentity> {
    let method: String = row.get(2)?;
    Ok(AuthIdentity {
        id: AuthIdentityId(row.get(0)?),
        principal_id: PrincipalId(row.get(1)?),
        method: schema::parse_auth_identity_method(method).map_err(sql_conversion_error)?,
        issuer: row.get(3)?,
        subject: row.get(4)?,
        provider_login: row.get(5)?,
        credential_handle: row.get(6)?,
        created_at: parse_time_sql(row.get(7)?)?,
        updated_at: parse_time_sql(row.get(8)?)?,
        last_used_at: parse_optional_time_sql(row.get(9)?)?,
        disabled_at: parse_optional_time_sql(row.get(10)?)?,
    })
}

fn github_allowlist_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<GithubAllowlistEntry> {
    Ok(GithubAllowlistEntry {
        id: GithubAllowlistId(row.get(0)?),
        normalized_login: row.get(1)?,
        target_principal_id: row.get::<_, Option<i64>>(2)?.map(PrincipalId),
        created_by: PrincipalId(row.get(3)?),
        created_at: parse_time_sql(row.get(4)?)?,
        updated_at: parse_time_sql(row.get(5)?)?,
        consumed_at: parse_optional_time_sql(row.get(6)?)?,
        revoked_at: parse_optional_time_sql(row.get(7)?)?,
    })
}

fn tenant_invitation_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TenantInvitation> {
    let role: String = row.get(2)?;
    Ok(TenantInvitation {
        id: TenantInvitationId(row.get(0)?),
        tenant_id: TenantId(row.get(1)?),
        intended_role: schema::parse_role(role).map_err(sql_conversion_error)?,
        created_by: PrincipalId(row.get(3)?),
        target_principal_id: row.get::<_, Option<i64>>(4)?.map(PrincipalId),
        created_at: parse_time_sql(row.get(5)?)?,
        expires_at: parse_time_sql(row.get(6)?)?,
        consumed_at: parse_optional_time_sql(row.get(7)?)?,
        revoked_at: parse_optional_time_sql(row.get(8)?)?,
    })
}

fn tenant_invitation_by_id_locked(
    conn: &Connection,
    id: TenantInvitationId,
) -> Result<TenantInvitation> {
    conn.query_row(
        "SELECT id, tenant_id, intended_role, created_by, target_principal_id,
                created_at, expires_at, consumed_at, revoked_at
         FROM tenant_invitation WHERE id = ?1",
        params![id.0],
        tenant_invitation_from_row,
    )
    .map_err(Into::into)
}

fn principal_key_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PrincipalKey> {
    Ok(PrincipalKey {
        id: PrincipalKeyId(row.get(0)?),
        principal_id: PrincipalId(row.get(1)?),
        public_key: row.get(2)?,
        fingerprint: row.get(3)?,
        label: row.get(4)?,
        created_at: parse_time_sql(row.get(5)?)?,
        updated_at: parse_time_sql(row.get(6)?)?,
        last_used_at: parse_optional_time_sql(row.get(7)?)?,
        revoked_at: parse_optional_time_sql(row.get(8)?)?,
    })
}

fn principal_key_by_id_locked(conn: &Connection, id: PrincipalKeyId) -> Result<PrincipalKey> {
    conn.query_row(
        "SELECT id, principal_id, public_key, fingerprint, label, created_at,
                updated_at, last_used_at, revoked_at
         FROM principal_key WHERE id = ?1",
        params![id.0],
        principal_key_from_row,
    )
    .optional()?
    .ok_or(MetadataError::PrincipalKeyNotFound(id))
}

fn tenant_membership_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TenantMembership> {
    let kind: String = row.get(2)?;
    let role: String = row.get(6)?;
    Ok(TenantMembership {
        tenant: Tenant {
            id: TenantId(row.get(0)?),
            name: row.get(1)?,
            kind: parse_tenant_kind_sql(kind)?,
            created_at: parse_time_sql(row.get(3)?)?,
            updated_at: parse_time_sql(row.get(4)?)?,
        },
        principal_id: PrincipalId(row.get(5)?),
        role: parse_role_sql(role)?,
        created_at: parse_time_sql(row.get(7)?)?,
        updated_at: parse_time_sql(row.get(8)?)?,
    })
}

fn api_token_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ApiTokenRow> {
    Ok(ApiTokenRow {
        id: ApiTokenId(row.get(0)?),
        principal_id: PrincipalId(row.get(1)?),
        tenant_id: row.get::<_, Option<i64>>(2)?.map(TenantId),
        name: row.get(3)?,
        created_at: parse_time_sql(row.get(4)?)?,
        updated_at: parse_time_sql(row.get(5)?)?,
        last_used_at: parse_optional_time_sql(row.get(6)?)?,
        expires_at: parse_optional_time_sql(row.get(7)?)?,
        revoked_at: parse_optional_time_sql(row.get(8)?)?,
    })
}

fn connection_profile_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ConnectionProfile> {
    let provider_id: String = row.get(13)?;
    let configuration_json: String = row.get(14)?;
    let semantic_engine: Option<String> = row.get(15)?;
    let credential_mode: String = row.get(5)?;
    let tags_json: String = row.get(7)?;
    let policy_json: String = row.get(11)?;
    let mut policy: ConnectionPolicy =
        serde_json::from_str(&policy_json).map_err(sql_conversion_error)?;
    policy.revision = row.get::<_, i64>(12)?.try_into().map_err(|_| {
        sql_message_error("connection profile policy revision is negative".to_string())
    })?;
    Ok(ConnectionProfile {
        id: ConnectionProfileId(row.get(0)?),
        tenant_id: TenantId(row.get(1)?),
        name: row.get(2)?,
        provider_id: sift_protocol::ProviderId::new(provider_id)
            .map_err(|error| sql_message_error(error.to_string()))?,
        configuration: serde_json::from_str(&configuration_json).map_err(sql_conversion_error)?,
        semantic_engine: semantic_engine
            .map(|value| value.parse().map_err(sql_message_error))
            .transpose()?,
        credential_mode: parse_credential_mode_sql(credential_mode)?,
        shared_secret_handle: row.get(6)?,
        tags: serde_json::from_str(&tags_json).map_err(sql_conversion_error)?,
        policy,
        created_by: PrincipalId(row.get(8)?),
        created_at: parse_time_sql(row.get(9)?)?,
        updated_at: parse_time_sql(row.get(10)?)?,
    })
}

fn validate_provider_credentials(credentials: &serde_json::Value) -> Result<()> {
    const MAX_CREDENTIAL_BYTES: usize = 256 * 1024;
    const MAX_CREDENTIAL_FIELDS: usize = 64;
    const MAX_CREDENTIAL_NAME_BYTES: usize = 128;

    let object = credentials
        .as_object()
        .filter(|object| !object.is_empty())
        .ok_or(MetadataError::InvalidCredentialObject)?;
    if object.len() > MAX_CREDENTIAL_FIELDS
        || object.keys().any(|name| {
            name.is_empty()
                || name.len() > MAX_CREDENTIAL_NAME_BYTES
                || name.bytes().any(|byte| byte.is_ascii_control())
        })
        || serde_json::to_vec(credentials)?.len() > MAX_CREDENTIAL_BYTES
    {
        return Err(MetadataError::InvalidCredentialObject);
    }
    Ok(())
}

fn tenant_limit_override_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<TenantLimitOverride> {
    let limits_json: String = row.get(1)?;
    Ok(TenantLimitOverride {
        tenant_id: TenantId(row.get(0)?),
        limits: serde_json::from_str(&limits_json).map_err(sql_conversion_error)?,
        updated_by: PrincipalId(row.get(2)?),
        created_at: parse_time_sql(row.get(3)?)?,
        updated_at: parse_time_sql(row.get(4)?)?,
    })
}

fn room_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Room> {
    let kind: String = row.get(3)?;
    Ok(Room {
        id: RoomId(row.get(0)?),
        tenant_id: TenantId(row.get(1)?),
        name: row.get(2)?,
        kind: parse_room_kind_sql(kind)?,
        created_by: PrincipalId(row.get(4)?),
        created_at: parse_time_sql(row.get(5)?)?,
        updated_at: parse_time_sql(row.get(6)?)?,
        bound_connection_profile_id: row.get::<_, Option<i64>>(7)?.map(ConnectionProfileId),
        bound_connection_by: row.get::<_, Option<i64>>(8)?.map(PrincipalId),
    })
}

fn room_member_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RoomMember> {
    let role: String = row.get(2)?;
    Ok(RoomMember {
        room_id: RoomId(row.get(0)?),
        principal_id: PrincipalId(row.get(1)?),
        role: parse_room_role_sql(role)?,
        joined_at: parse_time_sql(row.get(3)?)?,
    })
}

fn document_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Document> {
    let crdt_type: String = row.get(4)?;
    Ok(Document {
        id: DocumentId(row.get(0)?),
        room_id: RoomId(row.get(1)?),
        kind: row.get(2)?,
        title: row.get(3)?,
        crdt_type: parse_crdt_type_sql(crdt_type)?,
        crdt_state: row.get(5)?,
        position: row.get(6)?,
        connection_profile_id: row.get::<_, Option<i64>>(7)?.map(ConnectionProfileId),
        created_at: parse_time_sql(row.get(8)?)?,
        updated_at: parse_time_sql(row.get(9)?)?,
        crdt_format_version: row.get(10)?,
        snapshot_seq: row.get(11)?,
        next_update_seq: row.get(12)?,
        snapshot_version: row.get(13)?,
    })
}

fn document_update_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DocumentUpdate> {
    Ok(DocumentUpdate {
        server_seq: row.get(0)?,
        update_id: row.get(1)?,
        replica_id: row.get(2)?,
        submitted_by: PrincipalId(row.get(3)?),
        update_bytes: row.get(4)?,
        decoded_len: row.get(5)?,
        created_at: parse_time_sql(row.get(6)?)?,
    })
}

fn room_attachment_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RoomAttachment> {
    Ok(RoomAttachment {
        id: RoomAttachmentId(row.get(0)?),
        room_id: RoomId(row.get(1)?),
        principal_id: PrincipalId(row.get(2)?),
        client_id: row.get(3)?,
        attached_at: parse_time_sql(row.get(4)?)?,
        detached_at: parse_optional_time_sql(row.get(5)?)?,
    })
}

fn query_history_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<QueryHistory> {
    let status: String = row.get(7)?;
    Ok(QueryHistory {
        id: QueryHistoryId(row.get(0)?),
        principal_id: PrincipalId(row.get(1)?),
        connection_profile_id: row.get::<_, Option<i64>>(2)?.map(ConnectionProfileId),
        sql_text: row.get(3)?,
        started_at: parse_time_sql(row.get(4)?)?,
        duration_ms: row.get(5)?,
        row_count: row.get(6)?,
        status: parse_query_status_sql(status)?,
        error_code: row.get(8)?,
        error_message: row.get(9)?,
        room_id: row.get::<_, Option<i64>>(10)?.map(RoomId),
    })
}

/// Insert a single `operation_audit` row on the given connection or
/// transaction and return its rowid. Shared by the async writer path
/// ([`MetadataStore::record_operation_audit`]) and the transactional
/// audit path (security-critical mutations that write the audit row in
/// the same tx as the mutation — P1-meta-4). `Transaction` derefs to
/// `Connection`, so callers pass either.
fn insert_operation_audit_row(
    conn: &Connection,
    input: &NewOperationAudit,
) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO operation_audit
         (at, actor_principal_id, action, target, target_id, status, result_code,
          row_count, error_message, correlation_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            now_text(),
            input.actor_principal_id.map(|id| id.0),
            input.action,
            input.target,
            input.target_id,
            input.status,
            input.result_code,
            input.row_count,
            input.error_message,
            input.correlation_id,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

fn operation_audit_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<OperationAudit> {
    Ok(OperationAudit {
        id: OperationAuditId(row.get(0)?),
        at: parse_time_sql(row.get(1)?)?,
        actor_principal_id: row.get::<_, Option<i64>>(2)?.map(PrincipalId),
        action: row.get(3)?,
        target: row.get(4)?,
        target_id: row.get(5)?,
        status: row.get(6)?,
        result_code: row.get(7)?,
        row_count: row.get(8)?,
        error_message: row.get(9)?,
        correlation_id: row.get(10)?,
    })
}

fn saved_query_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SavedQuery> {
    let tags_json: String = row.get(6)?;
    let tags: Vec<String> = serde_json::from_str(&tags_json).map_err(sql_conversion_error)?;
    Ok(SavedQuery {
        id: SavedQueryId(row.get(0)?),
        tenant_id: TenantId(row.get(1)?),
        owner_principal_id: row.get::<_, Option<i64>>(2)?.map(PrincipalId),
        name: row.get(3)?,
        sql_text: row.get(4)?,
        connection_profile_id: row.get::<_, Option<i64>>(5)?.map(ConnectionProfileId),
        tags,
        created_at: parse_time_sql(row.get(7)?)?,
        updated_at: parse_time_sql(row.get(8)?)?,
        revision: row.get(9)?,
    })
}

/// Translate a free-text query into an FTS5 MATCH pattern. Each
/// whitespace-separated token becomes a prefix match; non-alphanumeric
/// characters are stripped so callers can't inject FTS5 operators.
/// Empty or all-punctuation input returns `None`; the caller should
/// avoid running a MATCH clause that would broaden the query.
fn fts_pattern(q: &str) -> Option<String> {
    let tokens: Vec<String> = q
        .split_whitespace()
        .map(|token| {
            let clean: String = token
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if clean.is_empty() {
                String::new()
            } else {
                format!("{clean}*")
            }
        })
        .filter(|t| !t.is_empty())
        .collect();
    if tokens.is_empty() {
        None
    } else {
        Some(tokens.join(" "))
    }
}

fn parse_time_sql(value: String) -> rusqlite::Result<DateTime<Utc>> {
    parse_time(value).map_err(sql_conversion_error)
}

fn parse_optional_time_sql(value: Option<String>) -> rusqlite::Result<Option<DateTime<Utc>>> {
    parse_optional_time(value).map_err(sql_conversion_error)
}

fn parse_tenant_kind_sql(value: String) -> rusqlite::Result<TenantKind> {
    schema::parse_tenant_kind(value).map_err(sql_conversion_error)
}

fn parse_role_sql(value: String) -> rusqlite::Result<MembershipRole> {
    schema::parse_role(value).map_err(sql_conversion_error)
}

fn parse_credential_mode_sql(value: String) -> rusqlite::Result<CredentialMode> {
    schema::parse_credential_mode(value).map_err(sql_conversion_error)
}

fn parse_room_kind_sql(value: String) -> rusqlite::Result<RoomKind> {
    schema::parse_room_kind(value).map_err(sql_conversion_error)
}

fn parse_room_role_sql(value: String) -> rusqlite::Result<RoomRole> {
    schema::parse_room_role(value).map_err(sql_conversion_error)
}

fn parse_crdt_type_sql(value: String) -> rusqlite::Result<CrdtType> {
    schema::parse_crdt_type(value).map_err(sql_conversion_error)
}

fn parse_query_status_sql(value: String) -> rusqlite::Result<QueryStatus> {
    schema::parse_query_status(value).map_err(sql_conversion_error)
}

fn parse_auth_client_kind_sql(value: String) -> rusqlite::Result<AuthClientKind> {
    match value.as_str() {
        "native" => Ok(AuthClientKind::Native),
        "web" => Ok(AuthClientKind::Web),
        "keypair" => Ok(AuthClientKind::Keypair),
        _ => Err(sql_message_error(format!(
            "invalid auth_session.client_kind: {value}"
        ))),
    }
}

fn sql_conversion_error(error: impl std::error::Error + Send + Sync + 'static) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(error))
}

fn sql_message_error(error: impl Into<String>) -> rusqlite::Error {
    sql_conversion_error(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        error.into(),
    ))
}

fn password_hash_error(error: argon2::password_hash::Error) -> MetadataError {
    MetadataError::PasswordHash(error.to_string())
}

fn token_lookup_from_presented(presented: &str) -> Option<&str> {
    let body = presented.strip_prefix(API_TOKEN_PREFIX)?;
    if body.len() <= API_TOKEN_LOOKUP_LEN
        || !body
            .as_bytes()
            .get(API_TOKEN_LOOKUP_LEN)
            .is_some_and(|b| *b == b'_')
    {
        return None;
    }
    Some(&body[..API_TOKEN_LOOKUP_LEN])
}

fn token_mac(token: &str) -> String {
    hex_encode(&hmac_sha256(API_TOKEN_MAC_KEY, token.as_bytes()))
}

struct TokenMaterial {
    plaintext: String,
    lookup: String,
    digest: String,
}

struct AuthTokenMaterial {
    access: TokenMaterial,
    refresh: TokenMaterial,
}

fn new_auth_token_material(key: &[u8]) -> AuthTokenMaterial {
    AuthTokenMaterial {
        access: new_token_material(ACCESS_TOKEN_PREFIX, key),
        refresh: new_token_material(REFRESH_TOKEN_PREFIX, key),
    }
}

fn new_token_material(prefix: &str, key: &[u8]) -> TokenMaterial {
    let lookup_seed = Uuid::new_v4().simple().to_string();
    let lookup = lookup_seed[..AUTH_TOKEN_LOOKUP_LEN].to_string();
    let plaintext = format!("{prefix}{lookup}_{}", Uuid::new_v4().simple());
    let digest = auth_token_digest(key, &plaintext);
    TokenMaterial {
        plaintext,
        lookup,
        digest,
    }
}

fn auth_token_lookup<'a>(presented: &'a str, prefix: &str) -> Option<&'a str> {
    let body = presented.strip_prefix(prefix)?;
    if body.len() <= AUTH_TOKEN_LOOKUP_LEN
        || body.as_bytes().get(AUTH_TOKEN_LOOKUP_LEN) != Some(&b'_')
    {
        return None;
    }
    Some(&body[..AUTH_TOKEN_LOOKUP_LEN])
}

fn auth_token_digest(key: &[u8], presented: &str) -> String {
    hex_encode(&hmac_sha256(key, presented.as_bytes()))
}

fn insert_access_token(
    conn: &Connection,
    session_id: &str,
    token: &TokenMaterial,
    expires_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO auth_access_token
         (auth_session_id, token_lookup, token_digest, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            session_id,
            token.lookup,
            token.digest,
            now.to_rfc3339(),
            expires_at.to_rfc3339()
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

fn insert_refresh_token(
    conn: &Connection,
    session_id: &str,
    family_id: &str,
    parent_id: Option<i64>,
    token: &TokenMaterial,
    expires_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO auth_refresh_token
         (auth_session_id, family_id, parent_id, token_lookup, token_digest,
          created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            session_id,
            family_id,
            parent_id,
            token.lookup,
            token.digest,
            now.to_rfc3339(),
            expires_at.to_rfc3339()
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut key_block = [0u8; BLOCK];
    if key.len() > BLOCK {
        key_block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }

    let mut outer = [0x5c_u8; BLOCK];
    let mut inner = [0x36_u8; BLOCK];
    for idx in 0..BLOCK {
        outer[idx] ^= key_block[idx];
        inner[idx] ^= key_block[idx];
    }

    let mut inner_hash = Sha256::new();
    inner_hash.update(inner);
    inner_hash.update(message);
    let inner_result = inner_hash.finalize();

    let mut outer_hash = Sha256::new();
    outer_hash.update(outer);
    outer_hash.update(inner_result);
    outer_hash.finalize().into()
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(HEX[(byte >> 4) as usize]));
        out.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    out
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in a.iter().zip(b) {
        diff |= a ^ b;
    }
    diff == 0
}

fn verify_legacy_token(presented: &str, hash: &str) -> Result<bool> {
    let parsed = PasswordHash::new(hash).map_err(password_hash_error)?;
    Ok(Argon2::default()
        .verify_password(presented.as_bytes(), &parsed)
        .is_ok())
}

fn should_touch_token(last_used_at: Option<&str>, now: DateTime<Utc>) -> bool {
    let Some(last_used_at) = last_used_at else {
        return true;
    };
    let Ok(last_used_at) = parse_time(last_used_at.to_string()) else {
        return true;
    };
    now.signed_duration_since(last_used_at).num_seconds() >= API_TOKEN_LAST_USED_DEBOUNCE_SECS
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use sift_protocol::{Engine, TenantResourceLimits, TenantRole, UpdateConnectionPolicyRequest};

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum FixtureAutomaticMigration {
        Allowed,
        BlockedAtV19,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum FixtureCurrentBinary {
        Accepted,
        MigrationRequired,
    }

    #[derive(Debug, Deserialize)]
    struct SchemaCompatibilityFixture {
        name: String,
        schema_version: u32,
        minimum_compatible_version: u32,
        automatic_migration: FixtureAutomaticMigration,
        current_binary: FixtureCurrentBinary,
    }

    fn schema_compatibility_fixtures() -> Vec<SchemaCompatibilityFixture> {
        serde_json::from_str(include_str!(
            "../tests/fixtures/schema-compatibility-boundaries.json"
        ))
        .unwrap()
    }

    fn copy_schema_fixture(directory: &Path, fixture: &SchemaCompatibilityFixture) -> PathBuf {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(format!("schema-v{}.sqlite", fixture.schema_version));
        let destination = directory.join(format!("{}.sqlite", fixture.name));
        std::fs::copy(&source, &destination)
            .unwrap_or_else(|error| panic!("copying {}: {error}", source.display()));
        destination
    }

    fn store() -> MetadataStore {
        MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap()
    }

    #[test]
    fn opening_a_file_store_does_not_create_or_migrate_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested").join("metadata.sqlite");
        let store = MetadataStore::open(&path, Arc::new(MemorySecretStore::new())).unwrap();

        assert!(!path.exists());
        let status = store.migration_status().unwrap();
        assert_eq!(status.current_version, 0);
        assert_eq!(status.latest_version, 43);
        assert_eq!(status.pending.len(), 43);
        assert!(matches!(
            store.ensure_schema_current(),
            Err(MetadataError::MigrationRequired {
                current: 0,
                latest: 43
            })
        ));
        assert!(!path.exists());
    }

    #[test]
    fn migration_apply_backs_up_an_existing_schema() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("metadata.sqlite");
        let mut connection = Connection::open(&path).unwrap();
        migrations::migrations::runner()
            .set_target(refinery::Target::Version(1))
            .run(&mut connection)
            .unwrap();
        drop(connection);

        let store = MetadataStore::open(&path, Arc::new(MemorySecretStore::new())).unwrap();
        let report = store.apply_migrations(false).unwrap();
        assert_eq!(report.from_version, 1);
        assert_eq!(report.to_version, 43);
        let backup = report.backup.expect("existing schema is backed up");
        assert!(backup.is_file());

        let mut backup_connection = Connection::open(backup).unwrap();
        let backup_version = migrations::migrations::runner()
            .get_last_applied_migration(&mut backup_connection)
            .unwrap()
            .unwrap()
            .version();
        assert_eq!(backup_version, 1);
    }

    #[test]
    fn automatic_migration_stops_at_a_contract_boundary() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("metadata.sqlite");
        let mut connection = Connection::open(&path).unwrap();
        migrations::migrations::runner()
            .set_target(refinery::Target::Version(18))
            .run(&mut connection)
            .unwrap();
        drop(connection);

        let store = MetadataStore::open(&path, Arc::new(MemorySecretStore::new())).unwrap();
        assert!(matches!(
            store.apply_migrations(true),
            Err(MetadataError::AutomaticMigrationBlocked {
                version: 19,
                kind: MigrationKind::Contract,
                ..
            })
        ));
        assert_eq!(store.migration_status().unwrap().current_version, 18);

        store.apply_migrations(false).unwrap();
        let status = store.migration_status().unwrap();
        assert_eq!(status.current_version, 43);
        assert_eq!(status.minimum_compatible_version, 19);
    }

    #[test]
    fn schema_compatibility_fixture_matrix() {
        let fixtures = schema_compatibility_fixtures();
        assert_eq!(
            fixtures
                .iter()
                .map(|fixture| fixture.schema_version)
                .collect::<Vec<_>>(),
            vec![18, 19, 28, 29, 30, 31, 32, 39, 40, 41, 42, 43],
            "the durable matrix must retain the pre-contract, contract, and current boundaries"
        );

        for fixture in fixtures {
            let directory = tempfile::tempdir().unwrap();
            let path = copy_schema_fixture(directory.path(), &fixture);

            let store = MetadataStore::open(&path, Arc::new(MemorySecretStore::new())).unwrap();
            let status = store.migration_status().unwrap();
            assert_eq!(
                status.current_version, fixture.schema_version,
                "{} schema version",
                fixture.name
            );
            assert_eq!(
                status.minimum_compatible_version, fixture.minimum_compatible_version,
                "{} compatibility floor",
                fixture.name
            );

            match fixture.current_binary {
                FixtureCurrentBinary::Accepted => store
                    .ensure_schema_current()
                    .unwrap_or_else(|error| panic!("{} should be accepted: {error}", fixture.name)),
                FixtureCurrentBinary::MigrationRequired => assert!(
                    matches!(
                        store.ensure_schema_current(),
                        Err(MetadataError::MigrationRequired {
                            current,
                            latest: 43
                        }) if current == fixture.schema_version
                    ),
                    "{} should require migration",
                    fixture.name
                ),
            }

            match fixture.automatic_migration {
                FixtureAutomaticMigration::BlockedAtV19 => assert!(
                    matches!(
                        store.apply_migrations(true),
                        Err(MetadataError::AutomaticMigrationBlocked {
                            version: 19,
                            kind: MigrationKind::Contract,
                            ..
                        })
                    ),
                    "{} must stop before the contract boundary",
                    fixture.name
                ),
                FixtureAutomaticMigration::Allowed => {
                    let report = store.apply_migrations(true).unwrap();
                    assert_eq!(
                        report.from_version, fixture.schema_version,
                        "{}",
                        fixture.name
                    );
                    assert_eq!(report.to_version, 43, "{}", fixture.name);
                }
            }
        }

        let directory = tempfile::tempdir().unwrap();
        let current_fixture = schema_compatibility_fixtures()
            .into_iter()
            .find(|fixture| fixture.schema_version == 43)
            .unwrap();
        let path = copy_schema_fixture(directory.path(), &current_fixture);
        let connection = Connection::open(&path).unwrap();
        connection
            .execute(
                "INSERT INTO refinery_schema_history
                 (version, name, applied_on, checksum)
                VALUES (44, 'future_additive_fixture', '2026-08-17T00:00:00Z', '1')",
                [],
            )
            .unwrap();
        connection.pragma_update(None, "user_version", 19).unwrap();
        drop(connection);

        let store = MetadataStore::open(&path, Arc::new(MemorySecretStore::new())).unwrap();
        let status = store.migration_status().unwrap();
        assert_eq!(status.current_version, 44);
        assert_eq!(status.latest_version, 43);
        assert!(status.pending.is_empty());
        store
            .ensure_schema_current()
            .expect("an unknown additive tail remains readable at the V19 floor");
        assert!(store.apply_migrations(false).unwrap().applied.is_empty());

        let connection = Connection::open(&path).unwrap();
        connection.pragma_update(None, "user_version", 44).unwrap();
        drop(connection);
        assert!(matches!(
            store.ensure_schema_current(),
            Err(MetadataError::BinaryTooOld {
                minimum: 44,
                latest: 43
            })
        ));
        assert!(matches!(
            store.apply_migrations(false),
            Err(MetadataError::BinaryTooOld {
                minimum: 44,
                latest: 43
            })
        ));
    }

    #[test]
    #[ignore = "maintainer-only regeneration of committed SQLite compatibility fixtures"]
    /// Regenerate intentionally with:
    /// `cargo test -p sift-metadata tests::regenerate_schema_compatibility_fixtures -- --ignored --exact`.
    fn regenerate_schema_compatibility_fixtures() {
        let fixture_directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        for fixture in schema_compatibility_fixtures() {
            let path = fixture_directory.join(format!("schema-v{}.sqlite", fixture.schema_version));
            if path.exists() {
                std::fs::remove_file(&path).unwrap();
            }
            let mut connection = Connection::open(&path).unwrap();
            configure_connection(&connection).unwrap();
            migrations::migrations::runner()
                .set_target(refinery::Target::Version(fixture.schema_version))
                .run(&mut connection)
                .unwrap();
            connection
                .pragma_update(None, "user_version", fixture.minimum_compatible_version)
                .unwrap();
            connection
                .execute(
                    "UPDATE refinery_schema_history SET applied_on = '2026-08-03T00:00:00Z'",
                    [],
                )
                .unwrap();
            connection
                .query_row("PRAGMA journal_mode = DELETE", [], |_| Ok(()))
                .unwrap();
            connection.execute_batch("VACUUM").unwrap();
        }
    }

    #[test]
    fn concurrent_migration_process_is_rejected_before_schema_changes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("metadata.sqlite");
        let first = MetadataStore::open(&path, Arc::new(MemorySecretStore::new())).unwrap();
        let second = MetadataStore::open(&path, Arc::new(MemorySecretStore::new())).unwrap();
        let _lock = first.lock_migrations().unwrap();

        assert!(matches!(
            second.apply_migrations(false),
            Err(MetadataError::MigrationInProgress(_))
        ));
        assert!(!path.exists());
    }

    /// Minimal audit record for exercising the transactional-audit path
    /// (P1-meta-4) from tests.
    fn test_audit(action: &str, target: &str, id: Option<i64>) -> NewOperationAudit {
        NewOperationAudit {
            actor_principal_id: Some(PrincipalId(1)),
            action: action.to_string(),
            target: target.to_string(),
            target_id: id,
            status: "succeeded".to_string(),
            result_code: None,
            row_count: None,
            error_message: None,
            correlation_id: None,
        }
    }

    fn store_with_memory() -> (MetadataStore, Arc<MemorySecretStore>) {
        let secrets = Arc::new(MemorySecretStore::new());
        (
            MetadataStore::open_in_memory(secrets.clone()).unwrap(),
            secrets,
        )
    }

    fn spec(password: Option<&str>) -> ConnectionSpec {
        ConnectionSpec {
            host: "localhost".to_string(),
            port: Some(5432),
            database: Some("sift".to_string()),
            user: "sift".to_string(),
            password: password.map(str::to_string),
            ssl_mode: None,
            engine_specific: None,
        }
    }

    #[test]
    fn bootstrap_local_creates_an_explicit_local_identity() {
        let store = store();
        store.bootstrap_local("local user").unwrap();

        let principal = store
            .resolve_principal_by_external_id("local:1")
            .unwrap()
            .unwrap();
        assert_eq!(principal.avatar_url, None);
        assert_eq!(principal.disabled_at, None);

        let identities = store.list_auth_identities(principal.id).unwrap();
        assert_eq!(identities.len(), 1);
        assert_eq!(identities[0].method, AuthIdentityMethod::LocalBypass);
        assert_eq!(identities[0].issuer, "sift");
        assert_eq!(identities[0].subject, "local:1");
        assert_eq!(identities[0].credential_handle, None);
    }

    #[test]
    fn compatibility_principal_creation_is_atomic_with_legacy_identity() {
        let store = store();
        let principal = store
            .create_principal("legacy:test", "test user", Some("test@example.com"))
            .unwrap();

        let identities = store.list_auth_identities(principal.id).unwrap();
        assert_eq!(identities.len(), 1);
        assert_eq!(identities[0].method, AuthIdentityMethod::Legacy);
        assert_eq!(identities[0].subject, "legacy:test");
    }

    #[test]
    fn every_prior_schema_boundary_upgrades_to_hosted_identity() {
        let latest = migrations::migrations::runner()
            .get_migrations()
            .iter()
            .map(refinery::Migration::version)
            .max()
            .unwrap();
        for starting_version in 0..=latest {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join(format!("v{starting_version}.sqlite"));
            if starting_version > 0 {
                let mut conn = Connection::open(&path).unwrap();
                configure_connection(&conn).unwrap();
                migrations::migrations::runner()
                    .set_target(refinery::Target::Version(starting_version))
                    .run(&mut conn)
                    .unwrap();
                let now = now_text();
                conn.execute(
                    "INSERT INTO tenant (id, name, kind, created_at, updated_at)
                     VALUES (1, 'local', 'personal', ?1, ?1)",
                    params![now],
                )
                .unwrap();
                conn.execute(
                    "INSERT INTO principal
                     (id, external_id, display_name, email, created_at, updated_at)
                     VALUES (1, 'local:1', 'local user', NULL, ?1, ?1)",
                    params![now],
                )
                .unwrap();
                conn.execute(
                    "INSERT INTO membership
                     (tenant_id, principal_id, role, created_at, updated_at)
                     VALUES (1, 1, 'owner', ?1, ?1)",
                    params![now],
                )
                .unwrap();
                if starting_version >= 14 {
                    conn.execute(
                        "INSERT INTO auth_identity
                         (principal_id, method, issuer, subject, created_at, updated_at)
                         VALUES (1, 'local_bypass', 'sift', 'local:1', ?1, ?1)",
                        params![now],
                    )
                    .unwrap();
                }
            }

            let store = MetadataStore::open(&path, Arc::new(MemorySecretStore::new())).unwrap();
            store.apply_migrations(false).unwrap();
            if starting_version == 0 {
                store.bootstrap_local("local user").unwrap();
            }
            let principal = store
                .resolve_principal_by_external_id("local:1")
                .unwrap()
                .unwrap();
            assert_eq!(
                principal.id,
                PrincipalId(1),
                "starting at V{starting_version}"
            );
            let identities = store.list_auth_identities(principal.id).unwrap();
            assert_eq!(identities.len(), 1, "starting at V{starting_version}");
            assert_eq!(
                identities[0].method,
                AuthIdentityMethod::LocalBypass,
                "starting at V{starting_version}"
            );
            let conn = store.conn().unwrap();
            let reset_table: String = conn
                .query_row(
                    "SELECT name FROM sqlite_master
                     WHERE type = 'table' AND name = 'password_reset_token'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(reset_table, "password_reset_token");
        }
    }

    #[tokio::test]
    async fn password_principal_creation_owns_personal_tenant_and_keeps_verifier_out_of_sqlite() {
        let store = store();
        let verifier = b"$argon2id$v=19$m=19456,t=2,p=1$test-salt$test-verifier";
        let principal = store
            .create_password_principal(
                NewPasswordPrincipal {
                    username: "alice",
                    display_name: "Alice",
                    email: Some("alice@example.com"),
                    is_instance_admin: true,
                },
                verifier,
                NewOperationAudit {
                    actor_principal_id: None,
                    action: "manage_principal.create".into(),
                    target: "principal".into(),
                    target_id: None,
                    status: "succeeded".into(),
                    result_code: None,
                    row_count: None,
                    error_message: None,
                    correlation_id: Some("offline-admin".into()),
                },
            )
            .await
            .unwrap();

        assert!(principal.is_instance_admin);
        assert!(principal.external_id.starts_with("principal:"));
        let memberships = store.list_principal_tenants(principal.id).unwrap();
        assert_eq!(memberships.len(), 1);
        assert_eq!(memberships[0].tenant.kind, TenantKind::Personal);
        assert_eq!(memberships[0].role, MembershipRole::Owner);

        let password = store.resolve_password_identity("alice").unwrap().unwrap();
        assert_eq!(password.principal.id, principal.id);
        assert_eq!(
            store.password_verifier(&password.identity).await.unwrap(),
            Some(verifier.to_vec())
        );
        let conn = store.conn().unwrap();
        let sqlite_dump: String = conn
            .query_row(
                "SELECT group_concat(COALESCE(credential_handle, '') || subject, '|')
                 FROM auth_identity",
                [],
                |row| row.get(0),
            )
            .unwrap();
        drop(conn);
        assert!(!sqlite_dump.contains("argon2"));

        let audit = store.list_operation_audit(1).unwrap();
        assert_eq!(audit[0].target_id, Some(principal.id.0));
        assert!(!serde_json::to_string(&audit).unwrap().contains("argon2"));
    }

    #[tokio::test]
    async fn principal_disablement_is_atomic_and_protects_the_final_admin() {
        let store = store();
        let verifier = b"$argon2id$test";
        let first = store
            .create_password_principal(
                NewPasswordPrincipal {
                    username: "first-admin",
                    display_name: "First",
                    email: None,
                    is_instance_admin: true,
                },
                verifier,
                test_audit("create", "principal", None),
            )
            .await
            .unwrap();
        assert!(matches!(
            store.set_principal_disabled(
                first.id,
                true,
                test_audit("disable", "principal", Some(first.id.0))
            ),
            Err(MetadataError::FinalInstanceAdmin)
        ));

        let second = store
            .create_password_principal(
                NewPasswordPrincipal {
                    username: "second-admin",
                    display_name: "Second",
                    email: None,
                    is_instance_admin: true,
                },
                verifier,
                test_audit("create", "principal", None),
            )
            .await
            .unwrap();
        let conn = store.conn().unwrap();
        conn.execute(
            "INSERT INTO auth_session
             (id, principal_id, refresh_family_id, client_kind, created_at, expires_at)
             VALUES ('session-1', ?1, 'family-1', 'native', ?2, ?3)",
            params![
                first.id.0,
                now_text(),
                (Utc::now() + chrono::Duration::days(1)).to_rfc3339()
            ],
        )
        .unwrap();
        drop(conn);

        store
            .set_principal_disabled(
                first.id,
                true,
                test_audit("disable", "principal", Some(first.id.0)),
            )
            .unwrap();
        let disabled = store
            .resolve_password_identity("first-admin")
            .unwrap()
            .unwrap();
        assert!(disabled.principal.disabled_at.is_some());
        assert!(disabled.identity.disabled_at.is_some());
        let conn = store.conn().unwrap();
        let reason: String = conn
            .query_row(
                "SELECT revocation_reason FROM auth_session WHERE id = 'session-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(reason, "principal_disabled");
        drop(conn);

        store
            .set_principal_disabled(
                first.id,
                false,
                test_audit("enable", "principal", Some(first.id.0)),
            )
            .unwrap();
        assert!(store
            .resolve_password_identity("first-admin")
            .unwrap()
            .unwrap()
            .principal
            .disabled_at
            .is_none());
        assert!(second.is_instance_admin);
    }

    #[tokio::test]
    async fn password_identities_link_and_unlink_without_exposing_or_orphaning_secrets() {
        let (store, secrets) = store_with_memory();
        let verifier = b"$argon2id$linked-verifier";
        let principal = store
            .create_password_principal(
                NewPasswordPrincipal {
                    username: "primary-login",
                    display_name: "Linked User",
                    email: None,
                    is_instance_admin: false,
                },
                b"$argon2id$primary-verifier",
                test_audit("create", "principal", None),
            )
            .await
            .unwrap();
        let primary = store.list_auth_identities(principal.id).unwrap()[0].clone();
        assert!(matches!(
            store
                .unlink_auth_identity(
                    principal.id,
                    primary.id,
                    test_audit("unlink", "auth_identity", Some(primary.id.0)),
                )
                .await,
            Err(MetadataError::FinalAuthIdentity)
        ));

        let linked = store
            .link_password_identity(
                principal.id,
                "secondary-login",
                verifier,
                test_audit("link", "auth_identity", None),
            )
            .await
            .unwrap();
        let handle = linked.credential_handle.clone().unwrap();
        assert_eq!(
            secrets
                .get(PASSWORD_SECRET_NAMESPACE, &handle)
                .await
                .unwrap(),
            Some(verifier.to_vec())
        );

        store
            .unlink_auth_identity(
                principal.id,
                linked.id,
                test_audit("unlink", "auth_identity", Some(linked.id.0)),
            )
            .await
            .unwrap();
        assert!(store
            .resolve_password_identity("secondary-login")
            .unwrap()
            .is_none());
        assert_eq!(
            secrets
                .get(PASSWORD_SECRET_NAMESPACE, &handle)
                .await
                .unwrap(),
            None
        );
        assert_eq!(store.list_auth_identities(principal.id).unwrap().len(), 1);
        let audit = store.list_operation_audit(10).unwrap();
        assert!(audit.iter().any(|entry| entry.action == "unlink"));
        assert!(audit.iter().any(|entry| entry.action == "link"));
    }

    #[tokio::test]
    async fn principal_auth_sessions_can_be_listed_and_selectively_revoked() {
        let store = store();
        let principal = store
            .create_password_principal(
                NewPasswordPrincipal {
                    username: "session-admin-target",
                    display_name: "Session Target",
                    email: None,
                    is_instance_admin: false,
                },
                b"verifier",
                test_audit("create", "principal", None),
            )
            .await
            .unwrap();
        let tokens = store
            .issue_auth_session(
                principal.id,
                AuthClientKind::Native,
                Some("workstation"),
                test_audit("authenticate", "auth_session", None),
            )
            .await
            .unwrap();
        let sessions = store.list_principal_auth_sessions(principal.id).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, tokens.session_id);
        assert_eq!(sessions[0].client_label.as_deref(), Some("workstation"));

        store
            .revoke_principal_auth_session(
                principal.id,
                &tokens.session_id,
                test_audit("revoke", "auth_session", None),
            )
            .unwrap();
        assert!(store
            .verify_auth_access_token(&tokens.access_token)
            .await
            .unwrap()
            .is_none());
        assert!(matches!(
            store.revoke_principal_auth_session(
                PrincipalId(999),
                &tokens.session_id,
                test_audit("revoke", "auth_session", None),
            ),
            Err(MetadataError::AuthSessionNotFound(_))
        ));
    }

    #[tokio::test]
    async fn restored_auth_sanitization_invalidates_ephemeral_state_and_preserves_identity() {
        let (store, secrets) = store_with_memory();
        let principal = store
            .create_password_principal(
                NewPasswordPrincipal {
                    username: "restored-admin",
                    display_name: "Restored Admin",
                    email: Some("restored@example.test"),
                    is_instance_admin: true,
                },
                b"durable-password-verifier",
                test_audit("create", "principal", None),
            )
            .await
            .unwrap();
        let identity = store.list_auth_identities(principal.id).unwrap()[0].clone();
        let credential_handle = identity.credential_handle.clone().unwrap();
        let membership = store.list_principal_tenants(principal.id).unwrap()[0].clone();

        let session = store
            .issue_auth_session(
                principal.id,
                AuthClientKind::Native,
                Some("restored-client"),
                test_audit("authenticate", "auth_session", None),
            )
            .await
            .unwrap();
        let oauth = store
            .create_github_oauth_attempt(AuthClientKind::Web)
            .await
            .unwrap();
        let oauth_handle: String = store
            .conn()
            .unwrap()
            .query_row(
                "SELECT pkce_verifier_handle FROM oauth_login_attempt",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let password_reset = store
            .issue_password_reset(
                principal.id,
                identity.id,
                principal.id,
                test_audit("issue_reset", "auth_identity", Some(identity.id.0)),
            )
            .await
            .unwrap();
        let (_, api_token) = store
            .issue_api_token(principal.id, None, "restored-api-token", None)
            .unwrap();
        let invitation = store
            .issue_tenant_invitation(
                membership.tenant.id,
                MembershipRole::Member,
                principal.id,
                None,
                Utc::now() + chrono::Duration::days(1),
                test_audit("invite", "tenant_invitation", None),
            )
            .await
            .unwrap();
        let principal_key = store
            .register_principal_key(
                principal.id,
                &[7; 32],
                "SHA256:restore-matrix",
                "restore matrix",
                test_audit("register", "principal_key", None),
            )
            .unwrap();
        let challenge = store
            .issue_key_challenge(&principal_key.fingerprint)
            .unwrap();
        let claims = ssh_claims("sift:restore-matrix");
        let ssh_capability = store
            .issue_ssh_proxy_capability(
                &claims,
                "restore-generation",
                Some(principal_key.id),
                test_audit("ssh_proxy.issue", "ssh_proxy_capability", None),
            )
            .await
            .unwrap();

        assert!(store
            .verify_auth_access_token(&session.access_token)
            .await
            .unwrap()
            .is_some());
        assert!(store.verify_api_token(&api_token).unwrap().is_some());
        let old_auth_key = secrets
            .get(AUTH_SYSTEM_SECRET_NAMESPACE, AUTH_TOKEN_MAC_HANDLE)
            .await
            .unwrap()
            .unwrap();
        let old_ssh_key = secrets
            .get(
                AUTH_SYSTEM_SECRET_NAMESPACE,
                SSH_PROXY_CAPABILITY_MAC_HANDLE,
            )
            .await
            .unwrap()
            .unwrap();
        assert!(secrets
            .get(OAUTH_SECRET_NAMESPACE, &oauth_handle)
            .await
            .unwrap()
            .is_some());

        store.sanitize_after_restore().await.unwrap();

        assert_eq!(
            secrets
                .get(OAUTH_SECRET_NAMESPACE, &oauth_handle)
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            secrets
                .get(AUTH_SYSTEM_SECRET_NAMESPACE, AUTH_TOKEN_MAC_HANDLE)
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            secrets
                .get(
                    AUTH_SYSTEM_SECRET_NAMESPACE,
                    SSH_PROXY_CAPABILITY_MAC_HANDLE,
                )
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            secrets
                .get(PASSWORD_SECRET_NAMESPACE, &credential_handle)
                .await
                .unwrap(),
            Some(b"durable-password-verifier".to_vec())
        );

        {
            let conn = store.conn().unwrap();
            for table in [
                "auth_session",
                "auth_access_token",
                "auth_refresh_token",
                "oauth_login_attempt",
                "password_reset_token",
                "keypair_challenge",
                "ssh_proxy_capability",
            ] {
                let count: i64 = conn
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })
                    .unwrap();
                assert_eq!(count, 0, "{table} must be empty after restore");
            }
            let revoked_invitations: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM tenant_invitation WHERE revoked_at IS NOT NULL",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            let revoked_api_tokens: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM api_token WHERE revoked_at IS NOT NULL",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(revoked_invitations, 1);
            assert_eq!(revoked_api_tokens, 1);
        }

        let restored_principal = store.principal_by_id(principal.id).unwrap().unwrap();
        assert_eq!(restored_principal.external_id, principal.external_id);
        let restored_identities = store.list_auth_identities(principal.id).unwrap();
        assert_eq!(restored_identities.len(), 1);
        assert_eq!(restored_identities[0].id, identity.id);
        assert_eq!(
            restored_identities[0].credential_handle,
            identity.credential_handle
        );
        let restored_memberships = store.list_principal_tenants(principal.id).unwrap();
        assert_eq!(restored_memberships.len(), 1);
        assert_eq!(restored_memberships[0].tenant.id, membership.tenant.id);
        assert_eq!(restored_memberships[0].role, membership.role);
        let restored_keys = store.list_principal_keys(principal.id).unwrap();
        assert_eq!(restored_keys.len(), 1);
        assert_eq!(restored_keys[0].id, principal_key.id);
        assert_eq!(restored_keys[0].fingerprint, principal_key.fingerprint);
        assert_eq!(restored_keys[0].public_key, principal_key.public_key);
        assert!(restored_keys[0].revoked_at.is_none());

        store.ensure_auth_system_keys().await.unwrap();
        let new_auth_key = secrets
            .get(AUTH_SYSTEM_SECRET_NAMESPACE, AUTH_TOKEN_MAC_HANDLE)
            .await
            .unwrap()
            .unwrap();
        let new_ssh_key = secrets
            .get(
                AUTH_SYSTEM_SECRET_NAMESPACE,
                SSH_PROXY_CAPABILITY_MAC_HANDLE,
            )
            .await
            .unwrap()
            .unwrap();
        assert_ne!(new_auth_key, old_auth_key);
        assert_ne!(new_ssh_key, old_ssh_key);

        assert!(store
            .verify_auth_access_token(&session.access_token)
            .await
            .unwrap()
            .is_none());
        assert!(matches!(
            store
                .rotate_auth_refresh_token(
                    &session.refresh_token,
                    test_audit("refresh", "auth_session", None),
                )
                .await
                .unwrap(),
            RefreshAuthResult::Invalid
        ));
        assert!(store.verify_api_token(&api_token).unwrap().is_none());
        assert!(matches!(
            store.consume_github_oauth_attempt(&oauth.state).await,
            Err(MetadataError::InvalidOAuthAttempt)
        ));
        assert!(matches!(
            store
                .consume_password_reset(
                    &password_reset.token,
                    b"replacement",
                    test_audit("reset", "auth_identity", None),
                )
                .await,
            Err(MetadataError::InvalidPasswordReset)
        ));
        assert!(matches!(
            store.consume_key_challenge(&challenge.nonce),
            Err(MetadataError::InvalidKeyChallenge)
        ));
        assert!(matches!(
            store
                .consume_ssh_proxy_capability(
                    &ssh_capability.capability,
                    "sift:restore-matrix",
                    "restore-generation",
                    test_audit("ssh_proxy.exchange", "auth_session", None),
                )
                .await,
            Err(MetadataError::InvalidSshProxyCapability)
        ));
        assert!(matches!(
            store
                .accept_tenant_invitation(
                    &invitation.token,
                    principal.id,
                    test_audit("accept", "tenant_invitation", None),
                )
                .await,
            Err(MetadataError::InvalidTenantInvitation)
        ));

        let restore_audit = store
            .list_operation_audit(100)
            .unwrap()
            .into_iter()
            .filter(|entry| entry.action == "backup.restore")
            .collect::<Vec<_>>();
        assert_eq!(restore_audit.len(), 1);
        let audit = &restore_audit[0];
        assert_eq!(audit.actor_principal_id, None);
        assert_eq!(audit.target, "instance_state");
        assert_eq!(audit.target_id, None);
        assert_eq!(audit.status, "succeeded");
        assert_eq!(audit.result_code, None);
        assert_eq!(audit.error_message, None);
        assert_eq!(audit.correlation_id, None);
    }

    #[tokio::test]
    async fn password_reset_is_secret_backed_one_use_and_revokes_sessions() {
        let (store, secrets) = store_with_memory();
        let principal = store
            .create_password_principal(
                NewPasswordPrincipal {
                    username: "reset-user",
                    display_name: "Reset User",
                    email: None,
                    is_instance_admin: true,
                },
                b"old-verifier",
                test_audit("create", "principal", None),
            )
            .await
            .unwrap();
        let identity = store.list_auth_identities(principal.id).unwrap()[0].clone();
        let old_handle = identity.credential_handle.clone().unwrap();
        let session = store
            .issue_auth_session(
                principal.id,
                AuthClientKind::Native,
                None,
                test_audit("authenticate", "auth_session", None),
            )
            .await
            .unwrap();
        let reset = store
            .issue_password_reset(
                principal.id,
                identity.id,
                principal.id,
                test_audit("issue_reset", "auth_identity", Some(identity.id.0)),
            )
            .await
            .unwrap();
        let conn = store.conn().unwrap();
        let durable: String = conn
            .query_row(
                "SELECT token_lookup || token_digest FROM password_reset_token",
                [],
                |row| row.get(0),
            )
            .unwrap();
        drop(conn);
        assert!(!durable.contains(&reset.token));

        assert_eq!(
            store
                .consume_password_reset(
                    &reset.token,
                    b"new-verifier",
                    test_audit("reset", "auth_identity", None),
                )
                .await
                .unwrap(),
            principal.id
        );
        assert!(store
            .verify_auth_access_token(&session.access_token)
            .await
            .unwrap()
            .is_none());
        let updated = store
            .resolve_password_identity("reset-user")
            .unwrap()
            .unwrap();
        assert_eq!(
            store.password_verifier(&updated.identity).await.unwrap(),
            Some(b"new-verifier".to_vec())
        );
        assert_eq!(
            secrets
                .get(PASSWORD_SECRET_NAMESPACE, &old_handle)
                .await
                .unwrap(),
            None
        );
        assert!(matches!(
            store
                .consume_password_reset(
                    &reset.token,
                    b"another-verifier",
                    test_audit("reset", "auth_identity", None),
                )
                .await,
            Err(MetadataError::InvalidPasswordReset)
        ));
    }

    #[tokio::test]
    async fn replacing_password_verifier_revokes_sessions_and_removes_old_secret() {
        let store = store();
        let principal = store
            .create_password_principal(
                NewPasswordPrincipal {
                    username: "password-user",
                    display_name: "Password User",
                    email: None,
                    is_instance_admin: false,
                },
                b"old-verifier",
                NewOperationAudit {
                    actor_principal_id: None,
                    ..test_audit("create", "principal", None)
                },
            )
            .await
            .unwrap();
        let original = store
            .resolve_password_identity("password-user")
            .unwrap()
            .unwrap();
        let conn = store.conn().unwrap();
        conn.execute(
            "INSERT INTO auth_session
             (id, principal_id, refresh_family_id, client_kind, created_at, expires_at)
             VALUES ('session-password', ?1, 'family-password', 'web', ?2, ?3)",
            params![
                principal.id.0,
                now_text(),
                (Utc::now() + chrono::Duration::days(1)).to_rfc3339()
            ],
        )
        .unwrap();
        drop(conn);

        store
            .replace_password_verifier(
                original.identity.id,
                b"new-verifier",
                test_audit(
                    "change_password",
                    "auth_identity",
                    Some(original.identity.id.0),
                ),
            )
            .await
            .unwrap();
        assert_eq!(
            store.password_verifier(&original.identity).await.unwrap(),
            None
        );
        let replacement = store
            .resolve_password_identity("password-user")
            .unwrap()
            .unwrap();
        assert_eq!(
            store
                .password_verifier(&replacement.identity)
                .await
                .unwrap(),
            Some(b"new-verifier".to_vec())
        );
        let conn = store.conn().unwrap();
        let reason: String = conn
            .query_row(
                "SELECT revocation_reason FROM auth_session WHERE id = 'session-password'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(reason, "password_changed");
    }

    #[tokio::test]
    async fn opaque_auth_tokens_rotate_and_refresh_replay_revokes_the_family() {
        let store = store();
        let principal = store
            .create_password_principal(
                NewPasswordPrincipal {
                    username: "token-user",
                    display_name: "Token User",
                    email: None,
                    is_instance_admin: false,
                },
                b"verifier",
                NewOperationAudit {
                    actor_principal_id: None,
                    ..test_audit("create", "principal", None)
                },
            )
            .await
            .unwrap();
        let first = store
            .issue_auth_session(
                principal.id,
                AuthClientKind::Native,
                Some("test client"),
                test_audit("authenticate", "auth_session", None),
            )
            .await
            .unwrap();
        assert!(first.access_token.starts_with(ACCESS_TOKEN_PREFIX));
        assert!(first.refresh_token.starts_with(REFRESH_TOKEN_PREFIX));
        assert_eq!(
            store
                .verify_auth_access_token(&first.access_token)
                .await
                .unwrap()
                .unwrap()
                .principal
                .id,
            principal.id
        );

        let rotated = match store
            .rotate_auth_refresh_token(
                &first.refresh_token,
                test_audit("refresh", "auth_session", None),
            )
            .await
            .unwrap()
        {
            RefreshAuthResult::Issued(tokens) => tokens,
            _ => panic!("initial refresh should rotate"),
        };
        assert!(store
            .verify_auth_access_token(&first.access_token)
            .await
            .unwrap()
            .is_none());
        assert!(store
            .verify_auth_access_token(&rotated.access_token)
            .await
            .unwrap()
            .is_some());

        assert!(matches!(
            store
                .rotate_auth_refresh_token(
                    &first.refresh_token,
                    test_audit("refresh_replay", "auth_session", None),
                )
                .await
                .unwrap(),
            RefreshAuthResult::ReplayDetected
        ));
        assert!(store
            .verify_auth_access_token(&rotated.access_token)
            .await
            .unwrap()
            .is_none());
        assert!(matches!(
            store
                .rotate_auth_refresh_token(
                    &rotated.refresh_token,
                    test_audit("refresh", "auth_session", None),
                )
                .await
                .unwrap(),
            RefreshAuthResult::Invalid
        ));

        let conn = store.conn().unwrap();
        let durable: String = conn
            .query_row(
                "SELECT group_concat(token_lookup || token_digest, '|')
                 FROM auth_access_token",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!durable.contains(&first.access_token));
        assert!(!durable.contains(&rotated.access_token));
    }

    #[test]
    fn github_allowlist_binds_immutable_id_and_supports_explicit_linking() {
        let store = store();
        store.bootstrap_local("Admin").unwrap();
        store
            .create_github_allowlist_entry(
                "octocat",
                None,
                PrincipalId(1),
                test_audit("allowlist", "github", None),
            )
            .unwrap();
        let created = store
            .complete_github_identity(
                GithubProfile {
                    id: 5_830_231,
                    login: "OctoCat".into(),
                    display_name: Some("The Octocat".into()),
                    email: None,
                    avatar_url: Some("https://avatars.example/octocat".into()),
                },
                NewOperationAudit {
                    actor_principal_id: None,
                    ..test_audit("authenticate.github", "auth_session", None)
                },
            )
            .unwrap()
            .unwrap();
        assert_ne!(created.id, PrincipalId(1));
        assert_eq!(store.list_principal_tenants(created.id).unwrap().len(), 1);
        assert!(store.list_github_allowlist_entries().unwrap()[0]
            .consumed_at
            .is_some());

        let renamed = store
            .complete_github_identity(
                GithubProfile {
                    id: 5_830_231,
                    login: "renamed-octocat".into(),
                    display_name: None,
                    email: None,
                    avatar_url: None,
                },
                NewOperationAudit {
                    actor_principal_id: None,
                    ..test_audit("authenticate.github", "auth_session", None)
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(renamed.id, created.id);
        assert_eq!(
            store.list_auth_identities(created.id).unwrap()[0].subject,
            "5830231"
        );
        assert!(store
            .complete_github_identity(
                GithubProfile {
                    id: 42,
                    login: "not-allowed".into(),
                    display_name: None,
                    email: None,
                    avatar_url: None,
                },
                NewOperationAudit {
                    actor_principal_id: None,
                    ..test_audit("authenticate.github", "auth_session", None)
                },
            )
            .unwrap()
            .is_none());

        store
            .create_github_allowlist_entry(
                "linked-admin",
                Some(PrincipalId(1)),
                PrincipalId(1),
                test_audit("allowlist", "github", None),
            )
            .unwrap();
        let linked = store
            .complete_github_identity(
                GithubProfile {
                    id: 99,
                    login: "linked-admin".into(),
                    display_name: Some("Admin via GitHub".into()),
                    email: None,
                    avatar_url: None,
                },
                NewOperationAudit {
                    actor_principal_id: None,
                    ..test_audit("authenticate.github", "auth_session", None)
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(linked.id, PrincipalId(1));
        assert_eq!(store.list_auth_identities(PrincipalId(1)).unwrap().len(), 2);
    }

    #[tokio::test]
    async fn oauth_state_and_pkce_verifier_are_one_use_and_secret_backed() {
        let store = store();
        let attempt = store
            .create_github_oauth_attempt(AuthClientKind::Web)
            .await
            .unwrap();
        assert!(attempt.state.starts_with(OAUTH_STATE_PREFIX));
        assert!(attempt.handoff_token.is_none());
        assert_eq!(attempt.code_verifier.len(), 64);
        let conn = store.conn().unwrap();
        let durable: String = conn
            .query_row(
                "SELECT state_lookup || state_digest || pkce_verifier_handle
                 FROM oauth_login_attempt",
                [],
                |row| row.get(0),
            )
            .unwrap();
        drop(conn);
        assert!(!durable.contains(&attempt.state));
        assert!(!durable.contains(&attempt.code_verifier));

        let consumed = store
            .consume_github_oauth_attempt(&attempt.state)
            .await
            .unwrap();
        assert_eq!(consumed.client_kind, AuthClientKind::Web);
        assert_eq!(consumed.code_verifier, attempt.code_verifier);
        assert!(matches!(
            store.consume_github_oauth_attempt(&attempt.state).await,
            Err(MetadataError::InvalidOAuthAttempt)
        ));
    }

    #[tokio::test]
    async fn native_github_handoff_is_opaque_and_one_use() {
        let store = store();
        store.bootstrap_local("Native User").unwrap();
        let attempt = store
            .create_github_oauth_attempt(AuthClientKind::Native)
            .await
            .unwrap();
        let handoff = attempt.handoff_token.clone().unwrap();
        assert!(handoff.starts_with(GITHUB_HANDOFF_PREFIX));
        let consumed = store
            .consume_github_oauth_attempt(&attempt.state)
            .await
            .unwrap();
        assert_eq!(consumed.client_kind, AuthClientKind::Native);
        store
            .complete_native_oauth_attempt(&consumed.attempt_id, PrincipalId(1))
            .unwrap();
        let conn = store.conn().unwrap();
        let durable: String = conn
            .query_row(
                "SELECT handoff_lookup || handoff_digest FROM oauth_login_attempt WHERE id = ?1",
                params![consumed.attempt_id],
                |row| row.get(0),
            )
            .unwrap();
        drop(conn);
        assert!(!durable.contains(&handoff));
        assert_eq!(
            store.consume_native_oauth_handoff(&handoff).await.unwrap(),
            PrincipalId(1)
        );
        assert!(matches!(
            store.consume_native_oauth_handoff(&handoff).await,
            Err(MetadataError::InvalidOAuthAttempt)
        ));
    }

    #[tokio::test]
    async fn tenant_invitation_is_opaque_targeted_and_atomically_one_use() {
        let store = store();
        store.bootstrap_local("Admin").unwrap();
        let invited = store
            .create_principal("legacy:invited", "Invited", None)
            .unwrap();
        let other = store
            .create_principal("legacy:other", "Other", None)
            .unwrap();
        let issued = store
            .issue_tenant_invitation(
                TenantId(1),
                MembershipRole::Member,
                PrincipalId(1),
                Some(invited.id),
                Utc::now() + chrono::Duration::days(1),
                test_audit("invite", "tenant_invitation", None),
            )
            .await
            .unwrap();
        let conn = store.conn().unwrap();
        let durable: String = conn
            .query_row(
                "SELECT token_lookup || token_digest FROM tenant_invitation WHERE id = ?1",
                params![issued.invitation.id.0],
                |row| row.get(0),
            )
            .unwrap();
        drop(conn);
        assert!(!durable.contains(&issued.token));
        assert!(matches!(
            store
                .accept_tenant_invitation(
                    &issued.token,
                    other.id,
                    test_audit("accept", "tenant_invitation", None),
                )
                .await,
            Err(MetadataError::InvalidTenantInvitation)
        ));
        let membership = store
            .accept_tenant_invitation(
                &issued.token,
                invited.id,
                test_audit("accept", "tenant_invitation", None),
            )
            .await
            .unwrap();
        assert_eq!(membership.role, MembershipRole::Member);
        assert!(matches!(
            store
                .accept_tenant_invitation(
                    &issued.token,
                    invited.id,
                    test_audit("accept", "tenant_invitation", None),
                )
                .await,
            Err(MetadataError::InvalidTenantInvitation)
        ));
    }

    #[test]
    fn records_and_lists_operation_audit() {
        let store = store();
        store.bootstrap_local("local user").unwrap();

        store
            .record_operation_audit(NewOperationAudit {
                actor_principal_id: Some(PrincipalId(1)),
                action: "execute".into(),
                target: "query".into(),
                target_id: Some(7),
                status: "succeeded".into(),
                result_code: None,
                row_count: Some(42),
                error_message: None,
                correlation_id: Some("corr-1".into()),
            })
            .unwrap();
        store
            .record_operation_audit(NewOperationAudit {
                actor_principal_id: None,
                action: "execute".into(),
                target: "query".into(),
                target_id: Some(7),
                status: "failed".into(),
                result_code: Some("syntax_error".into()),
                row_count: None,
                error_message: Some("boom".into()),
                correlation_id: None,
            })
            .unwrap();

        let rows = store.list_operation_audit(10).unwrap();
        assert_eq!(rows.len(), 2);
        // Most recent first.
        assert_eq!(rows[0].status, "failed");
        assert_eq!(rows[0].result_code.as_deref(), Some("syntax_error"));
        assert_eq!(rows[0].actor_principal_id, None);
        assert_eq!(rows[1].status, "succeeded");
        assert_eq!(rows[1].actor_principal_id, Some(PrincipalId(1)));
        assert_eq!(rows[1].row_count, Some(42));
        assert_eq!(rows[1].correlation_id.as_deref(), Some("corr-1"));

        let first_page = store.list_operation_audit_before(1, None).unwrap();
        assert_eq!(first_page.len(), 1);
        assert_eq!(first_page[0].status, "failed");
        let second_page = store
            .list_operation_audit_before(1, Some(first_page[0].id))
            .unwrap();
        assert_eq!(second_page.len(), 1);
        assert_eq!(second_page[0].status, "succeeded");
    }

    #[test]
    fn pooled_store_writes_visible_across_connections() {
        // A file-backed store spreads calls across pooled WAL connections. A
        // write on one checkout must be visible from a later checkout — this
        // is the P1-meta-1 concurrency change exercised end to end.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.sqlite");
        let secrets = Arc::new(MemorySecretStore::new());
        let store = MetadataStore::open(&path, secrets).unwrap();
        store.apply_migrations(false).unwrap();
        store.bootstrap_local("local user").unwrap();
        // Warm several connections so the read below is served by a different
        // one than the write (checkout drains the idle pool first).
        let handles: Vec<_> = (0..4).map(|_| store.conn().unwrap()).collect();
        drop(handles);

        store
            .record_operation_audit(NewOperationAudit {
                actor_principal_id: Some(PrincipalId(1)),
                action: "execute".into(),
                target: "query".into(),
                target_id: Some(7),
                status: "succeeded".into(),
                result_code: None,
                row_count: Some(42),
                error_message: None,
                correlation_id: Some("corr-1".into()),
            })
            .unwrap();

        let rows = store.list_operation_audit(10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].correlation_id.as_deref(), Some("corr-1"));
    }

    #[test]
    fn pool_reuses_idle_connections() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.sqlite");
        let secrets = Arc::new(MemorySecretStore::new());
        let store = MetadataStore::open(&path, secrets).unwrap();
        let Backend::Pool(pool) = &store.backend else {
            panic!("file-backed store should use the pool backend");
        };
        // A checked-in connection is retained and handed back out.
        let conn = pool.checkout().unwrap();
        drop(conn);
        assert_eq!(pool.idle.lock().unwrap().len(), 1);
        let _conn = pool.checkout().unwrap();
        assert_eq!(pool.idle.lock().unwrap().len(), 0);
    }

    #[test]
    fn pool_handles_concurrent_readers_and_writers() {
        // The point of the pool (P1-meta-1): many threads hit the same
        // file-backed store at once without deadlock, and concurrent writers
        // serialize via busy_timeout rather than erroring.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.sqlite");
        let store = MetadataStore::open(&path, Arc::new(MemorySecretStore::new())).unwrap();
        store.apply_migrations(false).unwrap();
        store.bootstrap_local("local user").unwrap();

        const THREADS: usize = 8;
        const WRITES_PER_THREAD: usize = 10;
        let store = Arc::new(store);
        let handles: Vec<_> = (0..THREADS)
            .map(|t| {
                let store = Arc::clone(&store);
                std::thread::spawn(move || {
                    for i in 0..WRITES_PER_THREAD {
                        store
                            .record_operation_audit(NewOperationAudit {
                                actor_principal_id: Some(PrincipalId(1)),
                                action: "execute".into(),
                                target: "query".into(),
                                target_id: Some((t * 100 + i) as i64),
                                status: "succeeded".into(),
                                result_code: None,
                                row_count: Some(1),
                                error_message: None,
                                correlation_id: None,
                            })
                            .expect("concurrent write succeeds");
                        // Interleave a read on a different pooled connection.
                        store
                            .list_operation_audit(5)
                            .expect("concurrent read succeeds");
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }

        let rows = store
            .list_operation_audit((THREADS * WRITES_PER_THREAD * 2) as u32)
            .unwrap();
        assert_eq!(rows.len(), THREADS * WRITES_PER_THREAD);
    }

    #[test]
    fn bootstraps_local_identity_once() {
        let store = store();
        store.bootstrap_local("local user").unwrap();
        store.bootstrap_local("ignored").unwrap();

        let principal = store
            .resolve_principal_by_external_id("local:1")
            .unwrap()
            .unwrap();
        assert_eq!(principal.id, PrincipalId(1));
        assert_eq!(principal.display_name, "local user");

        let tenants = store.list_principal_tenants(PrincipalId(1)).unwrap();
        assert_eq!(tenants.len(), 1);
        assert_eq!(tenants[0].tenant.id, TenantId(1));
        assert_eq!(tenants[0].role, MembershipRole::Owner);
    }

    #[test]
    fn api_token_round_trip() {
        let store = store();
        store.bootstrap_local("local user").unwrap();

        let (row, plaintext) = store
            .issue_api_token(PrincipalId(1), Some(TenantId(1)), "test", None)
            .unwrap();
        assert_eq!(row.name, "test");
        assert_eq!(token_lookup_from_presented(&plaintext).unwrap().len(), 12);

        let verified = store.verify_api_token(&plaintext).unwrap().unwrap();
        assert_eq!(verified.id, row.id);
        assert!(store.verify_api_token("sift_wrong").unwrap().is_none());
    }

    #[test]
    fn api_token_uses_mac_and_debounces_last_used_at() {
        let store = store();
        store.bootstrap_local("local user").unwrap();

        let (row, plaintext) = store
            .issue_api_token(PrincipalId(1), Some(TenantId(1)), "test", None)
            .unwrap();
        let conn = store.conn().unwrap();
        let mac: Option<String> = conn
            .query_row(
                "SELECT token_mac FROM api_token WHERE id = ?1",
                params![row.id.0],
                |row| row.get(0),
            )
            .unwrap();
        drop(conn);
        assert_eq!(mac.as_deref(), Some(token_mac(&plaintext).as_str()));

        let first = store.verify_api_token(&plaintext).unwrap().unwrap();
        let second = store.verify_api_token(&plaintext).unwrap().unwrap();
        assert_eq!(first.last_used_at, second.last_used_at);
    }

    #[test]
    fn legacy_argon2_api_token_still_verifies() {
        let store = store();
        store.bootstrap_local("local user").unwrap();

        let lookup_seed = Uuid::new_v4().simple().to_string();
        let token_lookup = &lookup_seed[..API_TOKEN_LOOKUP_LEN];
        let plaintext = format!(
            "{API_TOKEN_PREFIX}{token_lookup}_{}",
            Uuid::new_v4().simple()
        );
        let salt = SaltString::generate(&mut OsRng);
        let token_hash = Argon2::default()
            .hash_password(plaintext.as_bytes(), &salt)
            .map_err(password_hash_error)
            .unwrap()
            .to_string();
        let now = now_text();
        {
            let conn = store.conn().unwrap();
            conn.execute(
                "INSERT INTO api_token
                 (principal_id, tenant_id, token_lookup, token_hash, token_mac, name, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, ?6)",
                params![1_i64, 1_i64, token_lookup, token_hash, "legacy", now],
            )
            .unwrap();
        }

        let verified = store.verify_api_token(&plaintext).unwrap().unwrap();
        assert_eq!(verified.name, "legacy");
    }

    #[tokio::test]
    async fn shared_connection_profile_stores_secret_out_of_band() {
        let store = store();
        store.bootstrap_local("local user").unwrap();

        let profile = store
            .upsert_connection_profile(
                TenantId(1),
                PrincipalId(1),
                NewConnectionProfile {
                    name: "local pg".to_string(),
                    provider_id: Engine::Postgres.provider_id(),
                    configuration: serde_json::to_value(spec(None)).unwrap(),
                    semantic_engine: Some(Engine::Postgres),
                    credentials: Some(serde_json::json!({"password": "secret"})),
                    credential_mode: CredentialMode::Shared,
                    tags: vec!["dev".to_string()],
                },
            )
            .await
            .unwrap();

        assert_eq!(profile.provider_id, Engine::Postgres.provider_id());
        assert!(profile
            .configuration
            .get("password")
            .map_or(true, |value| value.is_null()));
        assert!(profile.shared_secret_handle.is_some());

        let listed = store.list_connection_profiles(TenantId(1)).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "local pg");
        assert!(listed[0]
            .configuration
            .get("password")
            .map_or(true, |value| value.is_null()));

        let (_, credentials) = store
            .resolve_provider_connection(TenantId(1), PrincipalId(1), profile.id)
            .await
            .unwrap();
        assert_eq!(
            credentials.get("password").map(Vec::as_slice),
            Some(b"secret".as_slice())
        );
    }

    #[tokio::test]
    async fn provider_neutral_profile_round_trips_without_a_legacy_engine() {
        let store = store();
        store.bootstrap_local("local user").unwrap();
        let provider_id = sift_protocol::ProviderId::new("acme/database").unwrap();

        let profile = store
            .upsert_connection_profile(
                TenantId(1),
                PrincipalId(1),
                NewConnectionProfile {
                    name: "external".into(),
                    provider_id: provider_id.clone(),
                    configuration: serde_json::json!({"endpoint": "fixture"}),
                    semantic_engine: None,
                    credentials: None,
                    credential_mode: CredentialMode::PerUser,
                    tags: Vec::new(),
                },
            )
            .await
            .unwrap();

        assert_eq!(profile.provider_id, provider_id);
        assert_eq!(profile.semantic_engine, None);
        assert_eq!(
            store
                .get_connection_profile(TenantId(1), profile.id)
                .unwrap()
                .semantic_engine,
            None
        );
    }

    #[tokio::test]
    async fn broker_profile_is_rejected_before_persistence() {
        let store = store();
        store.bootstrap_local("local user").unwrap();
        let result = store
            .upsert_connection_profile(
                TenantId(1),
                PrincipalId(1),
                NewConnectionProfile {
                    name: "future broker".into(),
                    provider_id: Engine::Postgres.provider_id(),
                    configuration: serde_json::to_value(spec(None)).unwrap(),
                    semantic_engine: Some(Engine::Postgres),
                    credentials: Some(serde_json::json!({"password": "must-not-be-stored"})),
                    credential_mode: CredentialMode::Broker,
                    tags: vec![],
                },
            )
            .await;
        assert!(matches!(
            result,
            Err(MetadataError::BrokerCredentialModeUnsupported)
        ));
        assert!(store
            .list_connection_profiles(TenantId(1))
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn connection_profile_limit_is_checked_in_the_write_transaction() {
        let store = store();
        store.bootstrap_local("local user").unwrap();
        let input = |name: &str| NewConnectionProfile {
            name: name.into(),
            provider_id: Engine::Postgres.provider_id(),
            configuration: serde_json::to_value(spec(None)).unwrap(),
            semantic_engine: Some(Engine::Postgres),
            credentials: None,
            credential_mode: CredentialMode::PerUser,
            tags: Vec::new(),
        };
        store
            .upsert_connection_profile_with_limit(
                TenantId(1),
                PrincipalId(1),
                input("one"),
                Some(1),
                test_audit("upsert", "connection_profile", None),
            )
            .await
            .unwrap();
        assert!(matches!(
            store
                .upsert_connection_profile_with_limit(
                    TenantId(1),
                    PrincipalId(1),
                    input("two"),
                    Some(1),
                    test_audit("upsert", "connection_profile", None),
                )
                .await,
            Err(MetadataError::ConnectionProfileLimitReached(TenantId(1)))
        ));
        store
            .upsert_connection_profile_with_limit(
                TenantId(1),
                PrincipalId(1),
                input("one"),
                Some(1),
                test_audit("upsert", "connection_profile", None),
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn connection_profile_administration_requires_tenant_admin() {
        let store = store();
        store.bootstrap_local("local user").unwrap();
        let member = store
            .create_principal("profile-member", "profile member", None)
            .unwrap();
        store
            .upsert_tenant_membership(TenantId(1), member.id, MembershipRole::Member)
            .unwrap();
        let input = || NewConnectionProfile {
            name: "admin only".to_string(),
            provider_id: Engine::Postgres.provider_id(),
            configuration: serde_json::to_value(spec(None)).unwrap(),
            semantic_engine: Some(Engine::Postgres),
            credentials: None,
            credential_mode: CredentialMode::PerUser,
            tags: Vec::new(),
        };

        assert!(matches!(
            store
                .upsert_connection_profile(TenantId(1), member.id, input())
                .await,
            Err(MetadataError::TenantAdminRequired)
        ));

        let profile = store
            .upsert_connection_profile(TenantId(1), PrincipalId(1), input())
            .await
            .unwrap();
        assert!(matches!(
            store
                .delete_connection_profile(
                    TenantId(1),
                    member.id,
                    profile.id,
                    test_audit("delete", "connection_profile", Some(profile.id.0)),
                )
                .await,
            Err(MetadataError::TenantAdminRequired)
        ));
        assert!(store
            .get_connection_profile_for_principal(profile.id, PrincipalId(1))
            .is_ok());
    }

    #[tokio::test]
    async fn connection_policy_is_versioned_and_tenant_admin_only() {
        let store = store();
        store.bootstrap_local("local user").unwrap();
        let profile = store
            .upsert_connection_profile(
                TenantId(1),
                PrincipalId(1),
                NewConnectionProfile {
                    name: "policy pg".to_string(),
                    provider_id: Engine::Postgres.provider_id(),
                    configuration: serde_json::to_value(spec(None)).unwrap(),
                    semantic_engine: Some(Engine::Postgres),
                    credentials: None,
                    credential_mode: CredentialMode::Shared,
                    tags: Vec::new(),
                },
            )
            .await
            .unwrap();
        assert_eq!(profile.policy, ConnectionPolicy::default());

        let request = UpdateConnectionPolicyRequest {
            expected_revision: Some(0),
            minimum_tenant_role: TenantRole::Admin,
            read_only: true,
            allowed_ops: Some(vec![sift_protocol::OperationKind::ExecuteQuery]),
            blocked_ops: vec![sift_protocol::OperationKind::ExportQuery],
            allowed_schemas: Some(vec![sift_protocol::SchemaSelector {
                catalog: None,
                schema: "public".to_string(),
            }]),
        };
        let updated = store
            .update_connection_policy(
                TenantId(1),
                PrincipalId(1),
                profile.id,
                request.clone(),
                test_audit("update", "connection_policy", Some(profile.id.0)),
            )
            .unwrap();
        assert_eq!(updated.policy.revision, 1);
        assert!(updated.policy.read_only);
        assert_eq!(updated.policy.minimum_tenant_role, TenantRole::Admin);

        assert!(matches!(
            store.update_connection_policy(
                TenantId(1),
                PrincipalId(1),
                profile.id,
                request,
                test_audit("update", "connection_policy", Some(profile.id.0)),
            ),
            Err(MetadataError::PolicyRevisionConflict {
                expected: 0,
                current: 1
            })
        ));
    }

    #[test]
    fn tenant_limit_overrides_require_an_instance_admin() {
        let store = store();
        store.bootstrap_local("local user").unwrap();
        let limits = TenantResourceLimits {
            sessions: Some(2),
            connections: Some(4),
            ..TenantResourceLimits::default()
        };
        assert!(matches!(
            store.set_tenant_limit_override(
                PrincipalId(1),
                TenantId(1),
                limits.clone(),
                test_audit("update", "tenant_limits", Some(1)),
            ),
            Err(MetadataError::InstanceAdminRequired)
        ));

        store
            .conn()
            .unwrap()
            .execute(
                "UPDATE principal SET is_instance_admin = 1 WHERE id = 1",
                [],
            )
            .unwrap();
        let saved = store
            .set_tenant_limit_override(
                PrincipalId(1),
                TenantId(1),
                limits.clone(),
                test_audit("update", "tenant_limits", Some(1)),
            )
            .unwrap();
        assert_eq!(saved.limits, limits);
        assert_eq!(saved.updated_by, PrincipalId(1));
        assert!(store
            .clear_tenant_limit_override(
                PrincipalId(1),
                TenantId(1),
                test_audit("clear", "tenant_limits", Some(1)),
            )
            .unwrap());
        assert!(store
            .get_tenant_limit_override(TenantId(1))
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn replacing_and_deleting_shared_connection_profile_cleans_old_secret() {
        let (store, secrets) = store_with_memory();
        store.bootstrap_local("local user").unwrap();

        let first = store
            .upsert_connection_profile(
                TenantId(1),
                PrincipalId(1),
                NewConnectionProfile {
                    name: "local pg".to_string(),
                    provider_id: Engine::Postgres.provider_id(),
                    configuration: serde_json::to_value(spec(None)).unwrap(),
                    semantic_engine: Some(Engine::Postgres),
                    credentials: Some(serde_json::json!({"password": "first-secret"})),
                    credential_mode: CredentialMode::Shared,
                    tags: Vec::new(),
                },
            )
            .await
            .unwrap();
        let first_handle = first.shared_secret_handle.clone().unwrap();

        let second = store
            .upsert_connection_profile(
                TenantId(1),
                PrincipalId(1),
                NewConnectionProfile {
                    name: "local pg".to_string(),
                    provider_id: Engine::Postgres.provider_id(),
                    configuration: serde_json::to_value(spec(None)).unwrap(),
                    semantic_engine: Some(Engine::Postgres),
                    credentials: Some(serde_json::json!({"password": "second-secret"})),
                    credential_mode: CredentialMode::Shared,
                    tags: Vec::new(),
                },
            )
            .await
            .unwrap();
        let second_handle = second.shared_secret_handle.clone().unwrap();

        assert_ne!(first_handle, second_handle);
        assert!(secrets
            .get(SECRET_NAMESPACE, &first_handle)
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            secrets
                .get(SECRET_NAMESPACE, &second_handle)
                .await
                .unwrap()
                .as_deref(),
            Some(&br#"{"password":"second-secret"}"#[..])
        );

        let per_user = store
            .upsert_connection_profile(
                TenantId(1),
                PrincipalId(1),
                NewConnectionProfile {
                    name: "local pg".to_string(),
                    provider_id: Engine::Postgres.provider_id(),
                    configuration: serde_json::to_value(spec(None)).unwrap(),
                    semantic_engine: Some(Engine::Postgres),
                    credentials: None,
                    credential_mode: CredentialMode::PerUser,
                    tags: Vec::new(),
                },
            )
            .await
            .unwrap();
        assert!(per_user.shared_secret_handle.is_none());
        assert!(secrets
            .get(SECRET_NAMESPACE, &second_handle)
            .await
            .unwrap()
            .is_none());

        store
            .delete_connection_profile(
                TenantId(1),
                PrincipalId(1),
                per_user.id,
                test_audit("delete", "connection_profile", Some(per_user.id.0)),
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn per_user_credential_rejects_shared_profiles_without_leaking_secret() {
        let (store, secrets) = store_with_memory();
        store.bootstrap_local("local user").unwrap();
        let profile = store
            .upsert_connection_profile(
                TenantId(1),
                PrincipalId(1),
                NewConnectionProfile {
                    name: "shared pg".to_string(),
                    provider_id: Engine::Postgres.provider_id(),
                    configuration: serde_json::to_value(spec(None)).unwrap(),
                    semantic_engine: Some(Engine::Postgres),
                    credentials: None,
                    credential_mode: CredentialMode::Shared,
                    tags: Vec::new(),
                },
            )
            .await
            .unwrap();

        assert!(matches!(
            store
                .set_per_user_credential(
                    profile.id,
                    PrincipalId(1),
                    &serde_json::json!({"password": "must-not-persist"}),
                    test_audit("set_credential", "connection_profile", Some(profile.id.0)),
                )
                .await,
            Err(MetadataError::CredentialModeMismatch { .. })
        ));
        assert!(secrets.is_empty());
    }

    #[tokio::test]
    async fn per_user_connection_profile_requires_principal_secret() {
        let store = store();
        store.bootstrap_local("local user").unwrap();

        let profile = store
            .upsert_connection_profile(
                TenantId(1),
                PrincipalId(1),
                NewConnectionProfile {
                    name: "per-user pg".to_string(),
                    provider_id: Engine::Postgres.provider_id(),
                    configuration: serde_json::to_value(spec(None)).unwrap(),
                    semantic_engine: Some(Engine::Postgres),
                    credentials: None,
                    credential_mode: CredentialMode::PerUser,
                    tags: Vec::new(),
                },
            )
            .await
            .unwrap();

        assert!(matches!(
            store
                .resolve_provider_connection(TenantId(1), PrincipalId(1), profile.id)
                .await,
            Err(MetadataError::MissingCredential(_, _))
        ));

        store
            .set_per_user_credential(
                profile.id,
                PrincipalId(1),
                &serde_json::json!({"password": "user-secret"}),
                test_audit("set_credential", "connection_profile", Some(profile.id.0)),
            )
            .await
            .unwrap();
        let (_, credentials) = store
            .resolve_provider_connection(TenantId(1), PrincipalId(1), profile.id)
            .await
            .unwrap();
        assert_eq!(
            credentials.get("password").map(Vec::as_slice),
            Some(b"user-secret".as_slice())
        );
    }

    #[test]
    fn room_lifecycle_auto_adds_owner_member() {
        let store = store();
        store.bootstrap_local("local user").unwrap();

        let room = store
            .create_room(
                TenantId(1),
                PrincipalId(1),
                NewRoom {
                    name: "local room".to_string(),
                    kind: RoomKind::Personal,
                },
            )
            .unwrap();
        assert_eq!(room.tenant_id, TenantId(1));
        assert_eq!(room.created_by, PrincipalId(1));

        let rooms = store
            .list_rooms_for_principal(TenantId(1), PrincipalId(1))
            .unwrap();
        assert_eq!(rooms.len(), 1);
        assert_eq!(rooms[0].id, room.id);

        let members = store.list_room_members(room.id).unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].principal_id, PrincipalId(1));
        assert_eq!(members[0].role, RoomRole::Owner);

        let shared_rooms = store
            .list_shared_rooms_for_principal(TenantId(1), PrincipalId(1))
            .unwrap();
        assert!(shared_rooms.is_empty());
    }

    #[test]
    fn authorized_room_membership_stays_inside_tenant_and_keeps_an_owner() {
        let store = store();
        store.bootstrap_local("local user").unwrap();
        let peer = store.create_principal("peer-room", "peer", None).unwrap();
        store
            .upsert_tenant_membership(TenantId(1), peer.id, MembershipRole::Member)
            .unwrap();
        let foreign = store
            .create_principal("foreign-room", "foreign", None)
            .unwrap();
        let foreign_tenant = store.create_tenant("foreign", TenantKind::Team).unwrap();
        store
            .upsert_tenant_membership(foreign_tenant.id, foreign.id, MembershipRole::Owner)
            .unwrap();
        let room = store
            .create_room(
                TenantId(1),
                PrincipalId(1),
                NewRoom {
                    name: "membership invariants".to_string(),
                    kind: RoomKind::Shared,
                },
            )
            .unwrap();

        assert!(matches!(
            store.add_room_member_authorized(
                room.id,
                PrincipalId(1),
                foreign.id,
                RoomRole::Editor,
                test_audit("add_member", "room", Some(room.id.0)),
            ),
            Err(MetadataError::TenantMembershipRequired { .. })
        ));
        assert!(matches!(
            store.add_room_member_authorized(
                room.id,
                PrincipalId(1),
                PrincipalId(1),
                RoomRole::Editor,
                test_audit("add_member", "room", Some(room.id.0)),
            ),
            Err(MetadataError::FinalRoomOwner(_))
        ));

        store
            .add_room_member_authorized(
                room.id,
                PrincipalId(1),
                peer.id,
                RoomRole::Owner,
                test_audit("add_member", "room", Some(room.id.0)),
            )
            .unwrap();
        store
            .remove_room_member_authorized(
                room.id,
                PrincipalId(1),
                PrincipalId(1),
                test_audit("remove_member", "room", Some(room.id.0)),
            )
            .unwrap();
        assert!(matches!(
            store.leave_room_authorized(
                room.id,
                peer.id,
                test_audit("leave", "room", Some(room.id.0)),
            ),
            Err(MetadataError::FinalRoomOwner(_))
        ));
    }

    #[tokio::test]
    async fn bind_and_unbind_room_connection_round_trip() {
        let store = store();
        store.bootstrap_local("owner").unwrap();
        let room = store
            .create_room(
                TenantId(1),
                PrincipalId(1),
                NewRoom {
                    name: "bind".to_string(),
                    kind: RoomKind::Shared,
                },
            )
            .unwrap();
        assert!(room.bound_connection_profile_id.is_none());

        let profile = store
            .upsert_connection_profile(
                TenantId(1),
                PrincipalId(1),
                NewConnectionProfile {
                    name: "pg".to_string(),
                    provider_id: Engine::Postgres.provider_id(),
                    configuration: serde_json::to_value(spec(None)).unwrap(),
                    semantic_engine: Some(Engine::Postgres),
                    credentials: None,
                    credential_mode: CredentialMode::Shared,
                    tags: Vec::new(),
                },
            )
            .await
            .unwrap();

        let bound = store
            .bind_room_connection(
                room.id,
                PrincipalId(1),
                profile.id,
                test_audit("bind_connection", "room", Some(room.id.0)),
            )
            .unwrap();
        assert_eq!(bound.bound_connection_profile_id, Some(profile.id));
        assert_eq!(bound.bound_connection_by, Some(PrincipalId(1)));
        assert_eq!(
            store.get_room(room.id).unwrap().bound_connection_profile_id,
            Some(profile.id)
        );

        let unbound = store
            .unbind_room_connection(
                room.id,
                PrincipalId(1),
                test_audit("unbind_connection", "room", Some(room.id.0)),
            )
            .unwrap();
        assert!(unbound.bound_connection_profile_id.is_none());
        assert!(unbound.bound_connection_by.is_none());
    }

    #[tokio::test]
    async fn bind_room_connection_rejects_non_owner_and_foreign_profile() {
        let store = store();
        store.bootstrap_local("owner").unwrap();
        let room = store
            .create_room(
                TenantId(1),
                PrincipalId(1),
                NewRoom {
                    name: "bind".to_string(),
                    kind: RoomKind::Shared,
                },
            )
            .unwrap();
        let profile = store
            .upsert_connection_profile(
                TenantId(1),
                PrincipalId(1),
                NewConnectionProfile {
                    name: "pg".to_string(),
                    provider_id: Engine::Postgres.provider_id(),
                    configuration: serde_json::to_value(spec(None)).unwrap(),
                    semantic_engine: Some(Engine::Postgres),
                    credentials: None,
                    credential_mode: CredentialMode::Shared,
                    tags: Vec::new(),
                },
            )
            .await
            .unwrap();

        // A room editor (member, not owner) cannot bind.
        let peer = store.create_principal("legacy:peer", "peer", None).unwrap();
        store
            .upsert_tenant_membership(TenantId(1), peer.id, MembershipRole::Member)
            .unwrap();
        store
            .add_room_member_authorized(
                room.id,
                PrincipalId(1),
                peer.id,
                RoomRole::Editor,
                test_audit("add_member", "room", Some(room.id.0)),
            )
            .unwrap();
        assert!(matches!(
            store.bind_room_connection(
                room.id,
                peer.id,
                profile.id,
                test_audit("bind_connection", "room", Some(room.id.0)),
            ),
            Err(MetadataError::RoomOwnerRequired { .. })
        ));

        // A profile from another tenant cannot be bound.
        let foreign = store.create_tenant("foreign", TenantKind::Team).unwrap();
        let f_owner = store
            .create_principal("legacy:fowner", "fowner", None)
            .unwrap();
        store
            .upsert_tenant_membership(foreign.id, f_owner.id, MembershipRole::Owner)
            .unwrap();
        let foreign_profile = store
            .upsert_connection_profile(
                foreign.id,
                f_owner.id,
                NewConnectionProfile {
                    name: "fp".to_string(),
                    provider_id: Engine::Postgres.provider_id(),
                    configuration: serde_json::to_value(spec(None)).unwrap(),
                    semantic_engine: Some(Engine::Postgres),
                    credentials: None,
                    credential_mode: CredentialMode::Shared,
                    tags: Vec::new(),
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            store.bind_room_connection(
                room.id,
                PrincipalId(1),
                foreign_profile.id,
                test_audit("bind_connection", "room", Some(room.id.0)),
            ),
            Err(MetadataError::TenantMismatch(_, _))
        ));
    }

    #[test]
    fn document_snapshots_are_opaque_room_state() {
        let store = store();
        store.bootstrap_local("local user").unwrap();
        let room = store
            .create_room(
                TenantId(1),
                PrincipalId(1),
                NewRoom {
                    name: "sql room".to_string(),
                    kind: RoomKind::Shared,
                },
            )
            .unwrap();

        let document = store
            .create_document(
                room.id,
                NewDocument {
                    kind: "sql".to_string(),
                    title: "scratch.sql".to_string(),
                    crdt_state: b"initial".to_vec(),
                    snapshot_version: Vec::new(),
                    position: 0,
                    connection_profile_id: None,
                },
            )
            .unwrap();
        assert_eq!(document.room_id, room.id);
        assert_eq!(document.crdt_state, b"initial");

        let updated = store
            .update_document_snapshot(document.id, b"snapshot-v2".to_vec())
            .unwrap();
        assert_eq!(updated.crdt_state, b"snapshot-v2");

        let documents = store.list_documents(room.id).unwrap();
        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0].id, document.id);

        store.delete_document(document.id).unwrap();
        assert!(store.list_documents(room.id).unwrap().is_empty());
    }

    #[test]
    fn document_namespace_enforces_room_membership_and_write_roles() {
        let store = store();
        store.bootstrap_local("local user").unwrap();
        let viewer = store.create_principal("viewer", "viewer", None).unwrap();
        let outsider = store
            .create_principal("outsider", "outsider", None)
            .unwrap();
        store
            .upsert_tenant_membership(TenantId(1), viewer.id, MembershipRole::Member)
            .unwrap();
        let room = store
            .create_room(
                TenantId(1),
                PrincipalId(1),
                NewRoom {
                    name: "isolated room".to_string(),
                    kind: RoomKind::Shared,
                },
            )
            .unwrap();
        store
            .add_room_member_authorized(
                room.id,
                PrincipalId(1),
                viewer.id,
                RoomRole::Viewer,
                test_audit("add_member", "room", Some(room.id.0)),
            )
            .unwrap();
        let document = store
            .create_document_for_principal(
                room.id,
                PrincipalId(1),
                NewDocument {
                    kind: "sql".to_string(),
                    title: "private.sql".to_string(),
                    crdt_state: b"select 1".to_vec(),
                    snapshot_version: Vec::new(),
                    position: 0,
                    connection_profile_id: None,
                },
            )
            .unwrap();

        assert_eq!(
            store
                .list_documents_for_principal(room.id, viewer.id)
                .unwrap()
                .len(),
            1
        );
        assert!(matches!(
            store.get_document_for_principal(document.id, outsider.id, false),
            Err(MetadataError::DocumentNotFound(_))
        ));
        assert!(matches!(
            store.get_document_for_principal(document.id, viewer.id, true),
            Err(MetadataError::DocumentNotFound(_))
        ));
        assert!(matches!(
            store.update_document_snapshot_for_principal(
                document.id,
                viewer.id,
                b"denied".to_vec()
            ),
            Err(MetadataError::DocumentNotFound(_))
        ));

        store
            .add_room_member_authorized(
                room.id,
                PrincipalId(1),
                viewer.id,
                RoomRole::Editor,
                test_audit("add_member", "room", Some(room.id.0)),
            )
            .unwrap();
        let updated = store
            .update_document_snapshot_for_principal(document.id, viewer.id, b"allowed".to_vec())
            .unwrap();
        assert_eq!(updated.crdt_state, b"allowed");
    }

    #[tokio::test]
    async fn saved_query_namespace_hides_other_principals_and_tenants() {
        let store = store();
        store.bootstrap_local("local user").unwrap();
        let peer = store.create_principal("peer", "peer", None).unwrap();
        store
            .upsert_tenant_membership(TenantId(1), peer.id, MembershipRole::Member)
            .unwrap();
        let foreign_principal = store.create_principal("foreign", "foreign", None).unwrap();
        let foreign_tenant = store.create_tenant("foreign", TenantKind::Team).unwrap();
        store
            .upsert_tenant_membership(
                foreign_tenant.id,
                foreign_principal.id,
                MembershipRole::Owner,
            )
            .unwrap();
        let foreign_profile = store
            .upsert_connection_profile(
                foreign_tenant.id,
                foreign_principal.id,
                NewConnectionProfile {
                    name: "foreign pg".to_string(),
                    provider_id: Engine::Postgres.provider_id(),
                    configuration: serde_json::to_value(spec(None)).unwrap(),
                    semantic_engine: Some(Engine::Postgres),
                    credentials: None,
                    credential_mode: CredentialMode::PerUser,
                    tags: Vec::new(),
                },
            )
            .await
            .unwrap();
        let personal = store
            .insert_saved_query(NewSavedQuery {
                tenant_id: TenantId(1),
                owner_principal_id: Some(PrincipalId(1)),
                name: "personal".to_string(),
                sql_text: "select 1".to_string(),
                connection_profile_id: None,
                tags: Vec::new(),
            })
            .unwrap();
        let shared = store
            .insert_saved_query(NewSavedQuery {
                tenant_id: TenantId(1),
                owner_principal_id: None,
                name: "shared".to_string(),
                sql_text: "select 2".to_string(),
                connection_profile_id: None,
                tags: Vec::new(),
            })
            .unwrap();

        assert!(matches!(
            store.get_saved_query_visible(personal.id, TenantId(1), peer.id),
            Err(MetadataError::SavedQueryNotFound(_))
        ));
        assert_eq!(
            store
                .get_saved_query_visible(shared.id, TenantId(1), peer.id)
                .unwrap()
                .id,
            shared.id
        );
        let peer_search = store
            .list_saved_queries(
                peer.id,
                SavedQueryFilter {
                    tenant_id: TenantId(1),
                    q: Some("select".to_string()),
                    tags: Vec::new(),
                    scope: None,
                },
            )
            .unwrap();
        assert_eq!(
            peer_search.iter().map(|query| query.id).collect::<Vec<_>>(),
            vec![shared.id]
        );
        assert!(matches!(
            store.update_saved_query_authorized(
                personal.id,
                TenantId(1),
                peer.id,
                false,
                personal.revision,
                UpdateSavedQuery {
                    name: Some("stolen".to_string()),
                    ..UpdateSavedQuery::default()
                }
            ),
            Err(MetadataError::SavedQueryNotFound(_))
        ));
        assert!(matches!(
            store.update_saved_query_authorized(
                shared.id,
                TenantId(1),
                peer.id,
                false,
                shared.revision,
                UpdateSavedQuery {
                    name: Some("denied".to_string()),
                    ..UpdateSavedQuery::default()
                }
            ),
            Err(MetadataError::SavedQueryNotFound(_))
        ));
        assert!(matches!(
            store.insert_saved_query(NewSavedQuery {
                tenant_id: TenantId(1),
                owner_principal_id: Some(PrincipalId(1)),
                name: "bad profile".to_string(),
                sql_text: "select 3".to_string(),
                connection_profile_id: Some(foreign_profile.id),
                tags: Vec::new(),
            }),
            Err(MetadataError::TenantMismatch(_, TenantId(1)))
        ));
    }

    #[test]
    fn deleting_room_cascades_documents_and_members() {
        let store = store();
        store.bootstrap_local("local user").unwrap();
        let room = store
            .create_room(
                TenantId(1),
                PrincipalId(1),
                NewRoom {
                    name: "throwaway".to_string(),
                    kind: RoomKind::Shared,
                },
            )
            .unwrap();
        store
            .create_document(
                room.id,
                NewDocument {
                    kind: "sql".to_string(),
                    title: "scratch.sql".to_string(),
                    crdt_state: Vec::new(),
                    snapshot_version: Vec::new(),
                    position: 0,
                    connection_profile_id: None,
                },
            )
            .unwrap();

        store.delete_room(room.id).unwrap();
        assert!(store
            .list_rooms_for_principal(TenantId(1), PrincipalId(1))
            .unwrap()
            .is_empty());
        assert!(store.list_documents(room.id).unwrap().is_empty());
        assert!(store.list_room_members(room.id).unwrap().is_empty());
    }

    #[test]
    fn room_attachments_track_active_clients() {
        let store = store();
        store.bootstrap_local("local user").unwrap();
        let room = store
            .create_room(
                TenantId(1),
                PrincipalId(1),
                NewRoom {
                    name: "presence room".to_string(),
                    kind: RoomKind::Shared,
                },
            )
            .unwrap();

        let attachment = store
            .attach_room(room.id, PrincipalId(1), "client-a")
            .unwrap();
        let active = store.list_active_room_attachments(room.id).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].client_id, "client-a");

        let detached = store.detach_room(attachment.id).unwrap().unwrap();
        assert!(detached.detached_at.is_some());
        assert!(store.detach_room(attachment.id).unwrap().is_none());
        assert!(store
            .list_active_room_attachments(room.id)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn query_history_can_be_room_scoped() {
        let store = store();
        store.bootstrap_local("local user").unwrap();
        let room = store
            .create_room(
                TenantId(1),
                PrincipalId(1),
                NewRoom {
                    name: "history room".to_string(),
                    kind: RoomKind::Shared,
                },
            )
            .unwrap();

        let row = store
            .record_query_history(NewQueryHistory {
                principal_id: PrincipalId(1),
                room_id: Some(room.id),
                connection_profile_id: None,
                sql_text: "select 1".to_string(),
                duration_ms: Some(12),
                row_count: Some(1),
                status: QueryStatus::Ok,
                error_code: None,
                error_message: None,
            })
            .unwrap();
        assert_eq!(row.room_id, Some(room.id));

        let history = store.list_query_history_for_room(room.id, 10).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].sql_text, "select 1");
        assert_eq!(history[0].status, QueryStatus::Ok);

        store
            .record_query_history(NewQueryHistory {
                principal_id: PrincipalId(1),
                room_id: Some(room.id),
                connection_profile_id: None,
                sql_text: "select 2".to_string(),
                duration_ms: None,
                row_count: None,
                status: QueryStatus::Ok,
                error_code: None,
                error_message: None,
            })
            .unwrap();
        let first_page = store
            .list_query_history_for_room_before(room.id, 1, None)
            .unwrap();
        assert_eq!(first_page[0].sql_text, "select 2");
        let second_page = store
            .list_query_history_for_room_before(room.id, 1, Some(first_page[0].id))
            .unwrap();
        assert_eq!(second_page[0].sql_text, "select 1");
    }

    fn ssh_claims(audience: &str) -> SshProxyCapabilityClaims {
        let now = Utc::now();
        SshProxyCapabilityClaims {
            version: 1,
            instance_audience: audience.into(),
            principal_id: 1,
            capability_id: Uuid::new_v4().to_string(),
            issued_at: now,
            expires_at: now + chrono::Duration::minutes(2),
        }
    }

    #[tokio::test]
    async fn ssh_proxy_capability_is_instance_bound_one_use_and_access_only() {
        let store = store();
        store.bootstrap_local("local user").unwrap();
        let claims = ssh_claims("sift:test-instance");
        let issued = store
            .issue_ssh_proxy_capability(
                &claims,
                "generation-a",
                None,
                test_audit("ssh_proxy.issue", "ssh_proxy_capability", None),
            )
            .await
            .unwrap();

        let wrong_generation = store
            .consume_ssh_proxy_capability(
                &issued.capability,
                "sift:test-instance",
                "generation-b",
                test_audit("ssh_proxy.exchange", "auth_session", None),
            )
            .await;
        assert!(matches!(
            wrong_generation,
            Err(MetadataError::InvalidSshProxyCapability)
        ));

        let access = store
            .consume_ssh_proxy_capability(
                &issued.capability,
                "sift:test-instance",
                "generation-a",
                test_audit("ssh_proxy.exchange", "auth_session", None),
            )
            .await
            .unwrap();
        assert_eq!(access.principal_id, PrincipalId(1));
        assert!(access.access_token.starts_with(ACCESS_TOKEN_PREFIX));
        assert!(store
            .verify_auth_access_token(&access.access_token)
            .await
            .unwrap()
            .is_some());

        let replay = store
            .consume_ssh_proxy_capability(
                &issued.capability,
                "sift:test-instance",
                "generation-a",
                test_audit("ssh_proxy.exchange", "auth_session", None),
            )
            .await;
        assert!(matches!(
            replay,
            Err(MetadataError::InvalidSshProxyCapability)
        ));

        let conn = store.conn().unwrap();
        let refresh_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM auth_refresh_token art
                 JOIN auth_session s ON s.id = art.auth_session_id
                 WHERE s.id = ?1",
                params![access.session_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(refresh_count, 0);
        let durable: String = conn
            .query_row(
                "SELECT capability_digest FROM ssh_proxy_capability
                 WHERE capability_id = ?1",
                params![claims.capability_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!durable.contains(&issued.capability));
    }

    #[tokio::test]
    async fn provisioned_auth_keys_work_across_persistent_store_instances() {
        let directory = tempfile::tempdir().unwrap();
        let metadata_path = directory.path().join("metadata.sqlite");
        let secrets_path = directory.path().join("secrets.enc");
        let key_path = directory.path().join("secret.key");
        std::fs::write(&key_path, "11".repeat(32)).unwrap();

        let open = || {
            let secrets = Arc::new(FileSecretStore::open(&secrets_path, &key_path).unwrap());
            let store = MetadataStore::open(&metadata_path, secrets).unwrap();
            store.apply_migrations(false).unwrap();
            store
        };
        let provisioner = open();
        provisioner.ensure_auth_system_keys().await.unwrap();
        drop(provisioner);

        let issuer = open();
        issuer.bootstrap_local("local user").unwrap();
        let consumer = open();
        let claims = ssh_claims("sift:persistent-instance");
        let issued = issuer
            .issue_ssh_proxy_capability(
                &claims,
                "generation",
                None,
                test_audit("ssh_proxy.issue", "ssh_proxy_capability", None),
            )
            .await
            .unwrap();
        let access = consumer
            .consume_ssh_proxy_capability(
                &issued.capability,
                "sift:persistent-instance",
                "generation",
                test_audit("ssh_proxy.exchange", "auth_session", None),
            )
            .await
            .unwrap();
        assert_eq!(access.principal_id, PrincipalId(1));
    }

    #[tokio::test]
    async fn ssh_proxy_capability_tamper_and_audience_mismatch_do_not_consume() {
        let store = store();
        store.bootstrap_local("local user").unwrap();
        let claims = ssh_claims("sift:expected");
        let issued = store
            .issue_ssh_proxy_capability(
                &claims,
                "generation",
                None,
                test_audit("ssh_proxy.issue", "ssh_proxy_capability", None),
            )
            .await
            .unwrap();
        let mut tampered = issued.capability.clone().into_bytes();
        let last = tampered.last_mut().unwrap();
        *last = if *last == b'A' { b'B' } else { b'A' };
        let tampered = String::from_utf8(tampered).unwrap();
        assert!(store
            .consume_ssh_proxy_capability(
                &tampered,
                "sift:expected",
                "generation",
                test_audit("ssh_proxy.exchange", "auth_session", None),
            )
            .await
            .is_err());
        assert!(store
            .consume_ssh_proxy_capability(
                &issued.capability,
                "sift:other",
                "generation",
                test_audit("ssh_proxy.exchange", "auth_session", None),
            )
            .await
            .is_err());
        assert!(store
            .consume_ssh_proxy_capability(
                &issued.capability,
                "sift:expected",
                "generation",
                test_audit("ssh_proxy.exchange", "auth_session", None),
            )
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn ssh_proxy_capability_fails_after_issuing_key_is_revoked() {
        let store = store();
        store.bootstrap_local("local user").unwrap();
        let key = store
            .register_principal_key(
                PrincipalId(1),
                &[7; 32],
                "SHA256:test-ssh-key",
                "remote key",
                test_audit("principal_key.register", "principal_key", None),
            )
            .unwrap();
        let claims = ssh_claims("sift:key-revocation");
        let issued = store
            .issue_ssh_proxy_capability(
                &claims,
                "generation",
                Some(key.id),
                test_audit("ssh_proxy.issue", "ssh_proxy_capability", None),
            )
            .await
            .unwrap();
        store
            .revoke_principal_key(
                key.id,
                PrincipalId(1),
                test_audit("principal_key.revoke", "principal_key", Some(key.id.0)),
            )
            .unwrap();
        assert!(matches!(
            store
                .consume_ssh_proxy_capability(
                    &issued.capability,
                    "sift:key-revocation",
                    "generation",
                    test_audit("ssh_proxy.exchange", "auth_session", None),
                )
                .await,
            Err(MetadataError::InvalidSshProxyCapability)
        ));
    }
}
