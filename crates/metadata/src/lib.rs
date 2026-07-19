//! Local metadata persistence for tenants, principals, connection profiles,
//! rooms, documents, and room-scoped history.

use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use chrono::{DateTime, Utc};
use rand_core::OsRng;
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use sift_protocol::ConnectionSpec;
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
    #[error("connection profile {0:?} has no credential for principal {1:?}")]
    MissingCredential(ConnectionProfileId, PrincipalId),
    #[error("connection profile {0:?} uses broker credentials, which are not implemented")]
    BrokerCredentialUnsupported(ConnectionProfileId),
    #[error("connection profile {0:?} is not in tenant {1:?}")]
    TenantMismatch(ConnectionProfileId, TenantId),
    #[error("room {0:?} not found")]
    RoomNotFound(RoomId),
    #[error("document {0:?} not found")]
    DocumentNotFound(DocumentId),
    #[error("room attachment {0:?} not found")]
    RoomAttachmentNotFound(RoomAttachmentId),
    #[error("saved query {0:?} not found")]
    SavedQueryNotFound(SavedQueryId),
    #[error("secret store error: {0}")]
    SecretStore(String),
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
            std::fs::create_dir_all(parent)
                .map_err(|error| MetadataError::SecretStore(error.to_string()))?;
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
            "SELECT id, external_id, display_name, email, created_at, updated_at
             FROM principal WHERE external_id = ?1",
            params![external_id],
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
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO principal (external_id, display_name, email, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            params![external_id, display_name, email, now],
        )?;
        let id = PrincipalId(conn.last_insert_rowid());
        conn.query_row(
            "SELECT id, external_id, display_name, email, created_at, updated_at
             FROM principal WHERE id = ?1",
            params![id.0],
            principal_from_row,
        )
        .map_err(Into::into)
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
                    shared_secret_handle, tags_json, created_by, created_at, updated_at
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

    pub async fn upsert_connection_profile(
        &self,
        tenant: TenantId,
        actor: PrincipalId,
        mut input: NewConnectionProfile,
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
            let tx = conn.transaction()?;
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
                    shared_secret_handle = COALESCE(excluded.shared_secret_handle, connection_profile.shared_secret_handle),
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
                    let _ = self.secrets.delete(SECRET_NAMESPACE, handle).await;
                }
                return Err(error);
            }
        };
        if let (Some(old), Some(new)) = (
            old_shared_secret_handle.as_deref(),
            new_shared_secret_handle.as_deref(),
        ) {
            if old != new {
                let _ = self.secrets.delete(SECRET_NAMESPACE, old).await;
            }
        }
        Ok(profile)
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
        id: ConnectionProfileId,
        audit: NewOperationAudit,
    ) -> Result<()> {
        let backend = self.backend.clone();
        let handles = sqlite_blocking(move || {
            let mut conn = backend.conn()?;
            let tx = conn.transaction()?;
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
            insert_operation_audit_row(&tx, &audit)?;
            tx.commit()?;
            Ok(handles)
        })
        .await?;
        for handle in handles {
            let _ = self.secrets.delete(SECRET_NAMESPACE, &handle).await;
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
                let _ = self.secrets.delete(SECRET_NAMESPACE, &handle).await;
                return Err(error);
            }
        };
        if let Some(old) = old_handle.as_deref() {
            if old != handle {
                let _ = self.secrets.delete(SECRET_NAMESPACE, old).await;
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

    pub fn add_room_member(
        &self,
        room: RoomId,
        principal: PrincipalId,
        role: RoomRole,
    ) -> Result<RoomMember> {
        let now = now_text();
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO room_member (room_id, principal_id, role, joined_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(room_id, principal_id) DO UPDATE SET role = excluded.role",
            params![room.0, principal.0, role.as_str(), now],
        )?;
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

    pub fn remove_room_member(&self, room: RoomId, principal: PrincipalId) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM room_member WHERE room_id = ?1 AND principal_id = ?2",
            params![room.0, principal.0],
        )?;
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

    pub fn get_document(&self, id: DocumentId) -> Result<Document> {
        let conn = self.conn()?;
        self.document_by_id_locked(&conn, id)
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

    pub fn delete_document(&self, document: DocumentId) -> Result<()> {
        let conn = self.conn()?;
        let deleted = conn.execute("DELETE FROM document WHERE id = ?1", params![document.0])?;
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

    /// Fetch a saved query by id. Caller is responsible for the
    /// visibility check (owner or tenant member) before returning to
    /// an untrusted principal.
    pub fn get_saved_query(&self, id: SavedQueryId) -> Result<SavedQuery> {
        let conn = self.conn()?;
        self.saved_query_by_id_locked(&conn, id)
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
        let conn = self.conn()?;
        let existing = self.saved_query_by_id_locked(&conn, id)?;
        let name = update.name.unwrap_or(existing.name);
        let sql_text = update.sql_text.unwrap_or(existing.sql_text);
        let connection_profile_id = update
            .connection_profile_id
            .unwrap_or(existing.connection_profile_id);
        let tags = update.tags.unwrap_or(existing.tags);
        let tags_json = serde_json::to_string(&tags).map_err(MetadataError::Json)?;
        conn.execute(
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
        self.saved_query_by_id_locked(&conn, id)
    }

    /// Delete a saved query. Caller has already checked authorization.
    /// Returns `true` if a row was deleted, `false` if the id was
    /// absent (idempotent).
    pub fn delete_saved_query(&self, id: SavedQueryId) -> Result<bool> {
        let conn = self.conn()?;
        let deleted = conn.execute("DELETE FROM saved_query WHERE id = ?1", params![id.0])?;
        Ok(deleted > 0)
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
                    shared_secret_handle, tags_json, created_by, created_at, updated_at
             FROM connection_profile WHERE id = ?1",
        params![id.0],
        connection_profile_from_row,
    )
    .optional()?
    .ok_or(MetadataError::ConnectionProfileNotFound(id))
}

fn principal_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Principal> {
    Ok(Principal {
        id: PrincipalId(row.get(0)?),
        external_id: row.get(1)?,
        display_name: row.get(2)?,
        email: row.get(3)?,
        created_at: parse_time_sql(row.get(4)?)?,
        updated_at: parse_time_sql(row.get(5)?)?,
    })
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
    Ok(ConnectionProfile {
        id: ConnectionProfileId(row.get(0)?),
        tenant_id: TenantId(row.get(1)?),
        name: row.get(2)?,
        engine: engine.parse().map_err(sql_message_error)?,
        spec: serde_json::from_str(&spec_json).map_err(sql_conversion_error)?,
        credential_mode: parse_credential_mode_sql(credential_mode)?,
        shared_secret_handle: row.get(6)?,
        tags: serde_json::from_str(&tags_json).map_err(sql_conversion_error)?,
        created_by: PrincipalId(row.get(8)?),
        created_at: parse_time_sql(row.get(9)?)?,
        updated_at: parse_time_sql(row.get(10)?)?,
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

impl From<std::io::Error> for MetadataError {
    fn from(error: std::io::Error) -> Self {
        Self::SecretStore(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_protocol::Engine;

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
                        store.list_operation_audit(5).expect("concurrent read succeeds");
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }

        let rows = store.list_operation_audit((THREADS * WRITES_PER_THREAD * 2) as u32).unwrap();
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

        store
            .delete_connection_profile(
                TenantId(1),
                second.id,
                test_audit("delete", "connection_profile", Some(second.id.0)),
            )
            .await
            .unwrap();
        assert!(secrets
            .get(SECRET_NAMESPACE, &second_handle)
            .await
            .unwrap()
            .is_none());
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
