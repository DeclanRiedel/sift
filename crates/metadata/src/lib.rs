//! Local metadata persistence for tenants, principals, connection profiles,
//! rooms, documents, and room-scoped history.

use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use chrono::{DateTime, Utc};
use rand_core::OsRng;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};
use sift_protocol::{
    AuthSessionSummary, ConnectionPolicy, ConnectionSpec, TenantResourceLimits,
    UpdateConnectionPolicyRequest,
};
use uuid::Uuid;

pub mod http;
pub mod schema;
pub mod secrets;

pub use schema::*;
#[cfg(feature = "os-keychain")]
pub use secrets::OsKeychainSecretStore;
pub use secrets::{FileSecretStore, MemorySecretStore, SecretStore};

mod migrations {
    refinery::embed_migrations!("migrations");
}

const SECRET_NAMESPACE: &str = "sift.local";
const PASSWORD_SECRET_NAMESPACE: &str = "sift.auth.password";
const AUTH_SYSTEM_SECRET_NAMESPACE: &str = "sift.auth.system";
const AUTH_TOKEN_MAC_HANDLE: &str = "token-mac-v1";
const OAUTH_SECRET_NAMESPACE: &str = "sift.auth.oauth";
const OAUTH_STATE_PREFIX: &str = "sift_oauth_";
const GITHUB_HANDOFF_PREFIX: &str = "sift_gh_";
const INVITATION_TOKEN_PREFIX: &str = "sift_inv_";
const PASSWORD_RESET_TOKEN_PREFIX: &str = "sift_pr_";
const ACCESS_TOKEN_PREFIX: &str = "sift_at_";
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
    #[error("connection profile limit reached for tenant {0:?}")]
    ConnectionProfileLimitReached(TenantId),
    #[error("connection profile {0:?} has no credential for principal {1:?}")]
    MissingCredential(ConnectionProfileId, PrincipalId),
    #[error("connection profile {0:?} uses broker credentials, which are not implemented")]
    BrokerCredentialUnsupported(ConnectionProfileId),
    #[error("connection profile {profile:?} uses {actual:?} credentials, not {expected:?}")]
    CredentialModeMismatch {
        profile: ConnectionProfileId,
        expected: CredentialMode,
        actual: CredentialMode,
    },
    #[error("connection profile {0:?} is not in tenant {1:?}")]
    TenantMismatch(ConnectionProfileId, TenantId),
    #[error("connection profile policy revision conflict: expected {expected}, current {current}")]
    PolicyRevisionConflict { expected: u64, current: u64 },
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
    #[error("password reset token is invalid, expired, or already consumed")]
    InvalidPasswordReset,
    #[error("secret store error: {0}")]
    SecretStore(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("blocking metadata task failed: {0}")]
    BlockingTask(String),
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
}

impl Deref for ConnHandle<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        match self {
            ConnHandle::Pooled(conn) => conn,
            ConnHandle::Memory(conn) => conn,
        }
    }
}

impl DerefMut for ConnHandle<'_> {
    fn deref_mut(&mut self) -> &mut Connection {
        match self {
            ConnHandle::Pooled(conn) => conn,
            ConnHandle::Memory(conn) => conn,
        }
    }
}

#[derive(Clone)]
pub struct MetadataStore {
    backend: Backend,
    secrets: Arc<dyn SecretStore>,
}

impl MetadataStore {
    pub fn open(path: &Path, secrets: Arc<dyn SecretStore>) -> Result<Self> {
        if let Some(parent) = path.parent() {
            // A failure here is the SQLite DB *parent directory* being
            // uncreatable — an IO error, not a secret-store error. The old
            // `SecretStore` label showed the operator "secret store error:
            // Permission denied" while the real fault was the DB path.
            std::fs::create_dir_all(parent).map_err(MetadataError::Io)?;
        }
        let pool = Arc::new(ConnectionPool::new(path.to_path_buf()));
        // Migrate once on a pooled connection; it returns to the pool after.
        {
            let mut conn = pool.checkout()?;
            migrations::migrations::runner().run(&mut *conn)?;
        }
        Ok(Self {
            backend: Backend::Pool(pool),
            secrets,
        })
    }

    pub fn open_in_memory(secrets: Arc<dyn SecretStore>) -> Result<Self> {
        let mut conn = Connection::open_in_memory()?;
        configure_connection(&conn)?;
        migrations::migrations::runner().run(&mut conn)?;
        Ok(Self {
            backend: Backend::Memory(Arc::new(Mutex::new(conn))),
            secrets,
        })
    }

    /// Borrow a connection for a single operation. See [`Backend::conn`].
    fn conn(&self) -> Result<ConnHandle<'_>> {
        self.backend.conn()
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

    async fn auth_token_mac_key(&self) -> Result<Vec<u8>> {
        if let Some(key) = self
            .secrets
            .get(AUTH_SYSTEM_SECRET_NAMESPACE, AUTH_TOKEN_MAC_HANDLE)
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
            .put(AUTH_SYSTEM_SECRET_NAMESPACE, AUTH_TOKEN_MAC_HANDLE, &key)
            .await?;
        Ok(key)
    }

    pub fn create_tenant(&self, name: &str, kind: TenantKind) -> Result<Tenant> {
        let now = now_text();
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO tenant (name, kind, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?3)",
            params![name, kind.as_str(), now],
        )?;
        let id = TenantId(conn.last_insert_rowid());
        Ok(Tenant {
            id,
            name: name.to_string(),
            kind,
            created_at: parse_time(now.clone())?,
            updated_at: parse_time(now)?,
        })
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

    pub fn issue_api_token(
        &self,
        principal: PrincipalId,
        scope: Option<TenantId>,
        name: &str,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<(ApiTokenRow, String)> {
        let lookup_seed = Uuid::new_v4().simple().to_string();
        let token_lookup = &lookup_seed[..API_TOKEN_LOOKUP_LEN];
        let plaintext = format!(
            "{API_TOKEN_PREFIX}{token_lookup}_{}",
            Uuid::new_v4().simple()
        );
        let token_mac = token_mac(&plaintext);
        let salt = SaltString::generate(&mut OsRng);
        let token_hash = Argon2::default()
            .hash_password(plaintext.as_bytes(), &salt)
            .map_err(password_hash_error)?
            .to_string();
        let now = now_text();
        let expires_at_text = expires_at.map(|dt| dt.to_rfc3339());
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO api_token
             (principal_id, tenant_id, token_lookup, token_hash, token_mac, name, created_at, updated_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8)",
            params![
                principal.0,
                scope.map(|id| id.0),
                token_lookup,
                token_hash,
                token_mac,
                name,
                now,
                expires_at_text
            ],
        )?;
        let id = ApiTokenId(conn.last_insert_rowid());
        let row = self.api_token_by_id_locked(&conn, id)?;
        Ok((row, plaintext))
    }

    pub fn verify_api_token(&self, presented: &str) -> Result<Option<ApiTokenRow>> {
        let Some(token_lookup) = token_lookup_from_presented(presented) else {
            return Ok(None);
        };
        let now = Utc::now();
        let conn = self.conn()?;
        let candidate = conn
            .query_row(
                "SELECT id, token_hash, token_mac, last_used_at FROM api_token
                 WHERE token_lookup = ?1
                   AND revoked_at IS NULL
                   AND (expires_at IS NULL OR expires_at > ?2)",
                params![token_lookup, now.to_rfc3339()],
                |row| {
                    Ok((
                        ApiTokenId(row.get(0)?),
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((id, hash, mac, last_used_at)) = candidate else {
            return Ok(None);
        };

        let verified = mac
            .as_deref()
            .is_some_and(|mac| constant_time_eq(mac.as_bytes(), token_mac(presented).as_bytes()))
            || verify_legacy_token(presented, &hash)?;
        if !verified {
            return Ok(None);
        }

        if should_touch_token(last_used_at.as_deref(), now) {
            let used_at = now.to_rfc3339();
            conn.execute(
                "UPDATE api_token SET last_used_at = ?1, updated_at = ?1 WHERE id = ?2",
                params![used_at, id.0],
            )?;
        }
        self.api_token_by_id_locked(&conn, id).map(Some)
    }

    pub fn list_api_tokens(&self, principal: PrincipalId) -> Result<Vec<ApiTokenRow>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, principal_id, tenant_id, name, created_at, updated_at,
                    last_used_at, expires_at, revoked_at
             FROM api_token
             WHERE principal_id = ?1
             ORDER BY created_at DESC, id DESC",
        )?;
        let tokens = rows(stmt.query_map(params![principal.0], api_token_from_row)?);
        tokens
    }

    /// Revoke an API token and write its audit row in the **same
    /// transaction** as the revocation (P1-meta-4). A crash can no longer
    /// leave a revoked token with no audit trail — the two commit
    /// atomically or not at all. The caller must therefore skip the async
    /// durable-audit enqueue on success (see
    /// `SessionStore::push_operation_local`).
    pub fn revoke_api_token(&self, id: ApiTokenId, audit: NewOperationAudit) -> Result<()> {
        let now = now_text();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE api_token
             SET revoked_at = COALESCE(revoked_at, ?1), updated_at = ?1
             WHERE id = ?2",
            params![now, id.0],
        )?;
        insert_operation_audit_row(&tx, &audit)?;
        tx.commit()?;
        Ok(())
    }

    pub fn list_connection_profiles(&self, tenant: TenantId) -> Result<Vec<ConnectionProfile>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, tenant_id, name, engine, spec_json, credential_mode,
                    shared_secret_handle, tags_json, created_by, created_at, updated_at,
                    policy_json, policy_revision
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
        let password = input.spec.password.take();
        let mut new_shared_secret_handle = None;
        if input.credential_mode == CredentialMode::Shared {
            if let Some(password) = password.as_deref() {
                let handle = Uuid::new_v4().to_string();
                self.secrets
                    .put(SECRET_NAMESPACE, &handle, password.as_bytes())
                    .await?;
                new_shared_secret_handle = Some(handle);
            }
        }

        let now = now_text();
        let spec_json = serde_json::to_string(&input.spec)?;
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
                  tags_json, created_by, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)
                 ON CONFLICT(tenant_id, name) DO UPDATE SET
                    engine = excluded.engine,
                    spec_json = excluded.spec_json,
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
                    input.engine.as_str(),
                    spec_json,
                    input.credential_mode.as_str(),
                    db_shared_secret_handle.as_deref(),
                    tags_json,
                    actor.0,
                    now
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
        secret: &[u8],
        audit: NewOperationAudit,
    ) -> Result<()> {
        let handle = Uuid::new_v4().to_string();
        self.secrets.put(SECRET_NAMESPACE, &handle, secret).await?;
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

    pub async fn resolve_connection_spec(
        &self,
        tenant: TenantId,
        principal: PrincipalId,
        id: ConnectionProfileId,
    ) -> Result<ConnectionSpec> {
        let backend = self.backend.clone();
        let (profile, handle) = sqlite_blocking(move || {
            let conn = backend.conn()?;
            let profile = connection_profile_by_id_locked(&conn, id)?;
            if profile.tenant_id != tenant {
                return Err(MetadataError::TenantMismatch(id, tenant));
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
            Ok((profile, handle))
        })
        .await?;
        let mut spec = profile.spec;
        if let Some(handle) = handle {
            let secret = self
                .secrets
                .get(SECRET_NAMESPACE, &handle)
                .await?
                .ok_or(MetadataError::MissingCredential(id, principal))?;
            spec.password = Some(String::from_utf8_lossy(&secret).into_owned());
        }
        Ok(spec)
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
            "SELECT r.id, r.tenant_id, r.name, r.kind, r.created_by, r.created_at, r.updated_at
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

    pub fn list_shared_rooms_for_principal(
        &self,
        tenant: TenantId,
        principal: PrincipalId,
    ) -> Result<Vec<Room>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT r.id, r.tenant_id, r.name, r.kind, r.created_by, r.created_at, r.updated_at
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
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO document
             (room_id, kind, title, crdt_type, crdt_state, position, connection_profile_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
            params![
                room.0,
                input.kind,
                input.title,
                input.crdt_type.as_str(),
                input.crdt_state,
                input.position,
                input.connection_profile_id.map(|id| id.0),
                now
            ],
        )?;
        let document_id = DocumentId(conn.last_insert_rowid());
        self.document_by_id_locked(&conn, document_id)
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
        tx.execute(
            "INSERT INTO document
             (room_id, kind, title, crdt_type, crdt_state, position, connection_profile_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
            params![
                room.0,
                input.kind,
                input.title,
                input.crdt_type.as_str(),
                input.crdt_state,
                input.position,
                input.connection_profile_id.map(|id| id.0),
                now
            ],
        )?;
        let document_id = DocumentId(tx.last_insert_rowid());
        let document = self.document_by_id_locked(&tx, document_id)?;
        tx.commit()?;
        Ok(document)
    }

    pub fn list_documents(&self, room: RoomId) -> Result<Vec<Document>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, room_id, kind, title, crdt_type, crdt_state, position,
                    connection_profile_id, created_at, updated_at
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
                    d.connection_profile_id, d.created_at, d.updated_at
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
                    d.connection_profile_id, d.created_at, d.updated_at
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

    pub fn detach_room(&self, attachment: RoomAttachmentId) -> Result<RoomAttachment> {
        let now = now_text();
        let conn = self.conn()?;
        let updated = conn.execute(
            "UPDATE room_attachment
             SET detached_at = COALESCE(detached_at, ?1)
             WHERE id = ?2",
            params![now, attachment.0],
        )?;
        if updated == 0 {
            return Err(MetadataError::RoomAttachmentNotFound(attachment));
        }
        self.room_attachment_by_id_locked(&conn, attachment)
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

    pub fn list_operation_audit(&self, limit: u32) -> Result<Vec<OperationAudit>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, at, actor_principal_id, action, target, target_id, status,
                    result_code, row_count, error_message, correlation_id
             FROM operation_audit
             ORDER BY at DESC, id DESC
             LIMIT ?1",
        )?;
        let audit = rows(stmt.query_map(params![limit], operation_audit_from_row)?);
        audit
    }

    pub fn list_query_history_for_room(
        &self,
        room: RoomId,
        limit: u32,
    ) -> Result<Vec<QueryHistory>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, principal_id, connection_profile_id, sql_text, started_at,
                    duration_ms, row_count, status, error_code, error_message, room_id
             FROM query_history
             WHERE room_id = ?1
             ORDER BY started_at DESC, id DESC
             LIMIT ?2",
        )?;
        let history = rows(stmt.query_map(params![room.0, limit], query_history_from_row)?);
        history
    }

    pub fn list_query_history_for_principal(
        &self,
        principal: PrincipalId,
        limit: u32,
    ) -> Result<Vec<QueryHistory>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, principal_id, connection_profile_id, sql_text, started_at,
                    duration_ms, row_count, status, error_code, error_message, room_id
             FROM query_history
             WHERE principal_id = ?1
             ORDER BY started_at DESC, id DESC
             LIMIT ?2",
        )?;
        let history = rows(stmt.query_map(params![principal.0, limit], query_history_from_row)?);
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
                    connection_profile_id, tags_json, created_at, updated_at
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
                    connection_profile_id, tags_json, created_at, updated_at
             FROM saved_query
             WHERE tenant_id = ?1
               AND (principal_id = ?2 OR principal_id IS NULL)",
        );
        let mut params_dyn: Vec<Box<dyn rusqlite::ToSql>> =
            vec![Box::new(filter.tenant_id.0), Box::new(principal.0)];
        match filter.scope {
            Some(SavedQueryScope::Personal) => {
                sql.push_str(" AND principal_id = ?2");
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
                 tags_json = ?4, updated_at = ?5
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
        update: UpdateSavedQuery,
    ) -> Result<SavedQuery> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = tx
            .query_row(
                "SELECT id, tenant_id, principal_id, name, sql_text,
                        connection_profile_id, tags_json, created_at, updated_at
                 FROM saved_query
                 WHERE id = ?1 AND tenant_id = ?2
                   AND (principal_id = ?3 OR (principal_id IS NULL AND ?4))",
                params![id.0, tenant.0, principal.0, tenant_admin],
                saved_query_from_row,
            )
            .optional()?
            .ok_or(MetadataError::SavedQueryNotFound(id))?;
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
                 tags_json = ?4, updated_at = ?5 WHERE id = ?6",
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
    ) -> Result<()> {
        let conn = self.conn()?;
        let deleted = conn.execute(
            "DELETE FROM saved_query
             WHERE id = ?1 AND tenant_id = ?2
               AND (principal_id = ?3 OR (principal_id IS NULL AND ?4))",
            params![id.0, tenant.0, principal.0, tenant_admin],
        )?;
        if deleted == 0 {
            return Err(MetadataError::SavedQueryNotFound(id));
        }
        Ok(())
    }

    fn saved_query_by_id_locked(&self, conn: &Connection, id: SavedQueryId) -> Result<SavedQuery> {
        conn.query_row(
            "SELECT id, tenant_id, principal_id, name, sql_text,
                    connection_profile_id, tags_json, created_at, updated_at
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
            "SELECT id, tenant_id, name, kind, created_by, created_at, updated_at
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
                    connection_profile_id, created_at, updated_at
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
                    policy_json, policy_revision
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
    let engine: String = row.get(3)?;
    let spec_json: String = row.get(4)?;
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
        engine: engine.parse().map_err(sql_message_error)?,
        spec: serde_json::from_str(&spec_json).map_err(sql_conversion_error)?,
        credential_mode: parse_credential_mode_sql(credential_mode)?,
        shared_secret_handle: row.get(6)?,
        tags: serde_json::from_str(&tags_json).map_err(sql_conversion_error)?,
        policy,
        created_by: PrincipalId(row.get(8)?),
        created_at: parse_time_sql(row.get(9)?)?,
        updated_at: parse_time_sql(row.get(10)?)?,
    })
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
    use sift_protocol::{Engine, TenantResourceLimits, TenantRole, UpdateConnectionPolicyRequest};

    fn store() -> MetadataStore {
        MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap()
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
            .last()
            .unwrap()
            .version();
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
                    engine: Engine::Postgres,
                    spec: spec(Some("secret")),
                    credential_mode: CredentialMode::Shared,
                    tags: vec!["dev".to_string()],
                },
            )
            .await
            .unwrap();

        assert!(profile.spec.password.is_none());
        assert!(profile.shared_secret_handle.is_some());

        let listed = store.list_connection_profiles(TenantId(1)).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "local pg");
        assert!(listed[0].spec.password.is_none());

        let resolved = store
            .resolve_connection_spec(TenantId(1), PrincipalId(1), profile.id)
            .await
            .unwrap();
        assert_eq!(resolved.password.as_deref(), Some("secret"));
    }

    #[tokio::test]
    async fn connection_profile_limit_is_checked_in_the_write_transaction() {
        let store = store();
        store.bootstrap_local("local user").unwrap();
        let input = |name: &str| NewConnectionProfile {
            name: name.into(),
            engine: Engine::Postgres,
            spec: spec(None),
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
            engine: Engine::Postgres,
            spec: spec(None),
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
                    engine: Engine::Postgres,
                    spec: spec(None),
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
                    engine: Engine::Postgres,
                    spec: spec(Some("first-secret")),
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
                    engine: Engine::Postgres,
                    spec: spec(Some("second-secret")),
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
            Some(&b"second-secret"[..])
        );

        let per_user = store
            .upsert_connection_profile(
                TenantId(1),
                PrincipalId(1),
                NewConnectionProfile {
                    name: "local pg".to_string(),
                    engine: Engine::Postgres,
                    spec: spec(None),
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
                    engine: Engine::Postgres,
                    spec: spec(None),
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
                    b"must-not-persist",
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
                    engine: Engine::Postgres,
                    spec: spec(None),
                    credential_mode: CredentialMode::PerUser,
                    tags: Vec::new(),
                },
            )
            .await
            .unwrap();

        assert!(matches!(
            store
                .resolve_connection_spec(TenantId(1), PrincipalId(1), profile.id)
                .await,
            Err(MetadataError::MissingCredential(_, _))
        ));

        store
            .set_per_user_credential(
                profile.id,
                PrincipalId(1),
                b"user-secret",
                test_audit("set_credential", "connection_profile", Some(profile.id.0)),
            )
            .await
            .unwrap();
        let resolved = store
            .resolve_connection_spec(TenantId(1), PrincipalId(1), profile.id)
            .await
            .unwrap();
        assert_eq!(resolved.password.as_deref(), Some("user-secret"));
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
                    crdt_type: CrdtType::Loro,
                    crdt_state: b"initial".to_vec(),
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
                    crdt_type: CrdtType::Loro,
                    crdt_state: b"select 1".to_vec(),
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
                    engine: Engine::Postgres,
                    spec: spec(None),
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
                    crdt_type: CrdtType::Loro,
                    crdt_state: Vec::new(),
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

        let detached = store.detach_room(attachment.id).unwrap();
        assert!(detached.detached_at.is_some());
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
    }
}
