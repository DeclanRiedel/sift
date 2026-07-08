//! Local metadata persistence for tenants, principals, connection profiles,
//! rooms, documents, and room-scoped history.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use chrono::{DateTime, Utc};
use rand_core::OsRng;
use rusqlite::{params, Connection, OptionalExtension};
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
    #[error("secret store error: {0}")]
    SecretStore(String),
}

#[derive(Clone)]
pub struct MetadataStore {
    conn: Arc<Mutex<Connection>>,
    secrets: Arc<dyn SecretStore>,
}

impl MetadataStore {
    pub fn open(path: &Path, secrets: Arc<dyn SecretStore>) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| MetadataError::SecretStore(error.to_string()))?;
        }
        let mut conn = Connection::open(path)?;
        configure_connection(&conn)?;
        migrations::migrations::runner().run(&mut conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            secrets,
        })
    }

    pub fn open_in_memory(secrets: Arc<dyn SecretStore>) -> Result<Self> {
        let mut conn = Connection::open_in_memory()?;
        configure_connection(&conn)?;
        migrations::migrations::runner().run(&mut conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            secrets,
        })
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
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))?;
        Ok(())
    }

    pub fn bootstrap_local(&self, display_name: &str) -> Result<()> {
        let now = now_text();
        let mut conn = self.conn.lock().unwrap();
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
        let conn = self.conn.lock().unwrap();
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
        let conn = self.conn.lock().unwrap();
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
        let conn = self.conn.lock().unwrap();
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
        let conn = self.conn.lock().unwrap();
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
        let conn = self.conn.lock().unwrap();
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
        let salt = SaltString::generate(&mut OsRng);
        let token_hash = Argon2::default()
            .hash_password(plaintext.as_bytes(), &salt)
            .map_err(password_hash_error)?
            .to_string();
        let now = now_text();
        let expires_at_text = expires_at.map(|dt| dt.to_rfc3339());
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO api_token
             (principal_id, tenant_id, token_lookup, token_hash, name, created_at, updated_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7)",
            params![
                principal.0,
                scope.map(|id| id.0),
                token_lookup,
                token_hash,
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
        let conn = self.conn.lock().unwrap();
        let candidate = conn
            .query_row(
                "SELECT id, token_hash FROM api_token
                 WHERE token_lookup = ?1
                   AND revoked_at IS NULL
                   AND (expires_at IS NULL OR expires_at > ?2)",
                params![token_lookup, now.to_rfc3339()],
                |row| Ok((ApiTokenId(row.get(0)?), row.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some((id, hash)) = candidate else {
            return Ok(None);
        };

        let parsed = PasswordHash::new(&hash).map_err(password_hash_error)?;
        if Argon2::default()
            .verify_password(presented.as_bytes(), &parsed)
            .is_err()
        {
            return Ok(None);
        }

        let used_at = now.to_rfc3339();
        conn.execute(
            "UPDATE api_token SET last_used_at = ?1, updated_at = ?1 WHERE id = ?2",
            params![used_at, id.0],
        )?;
        self.api_token_by_id_locked(&conn, id).map(Some)
    }

    pub fn list_api_tokens(&self, principal: PrincipalId) -> Result<Vec<ApiTokenRow>> {
        let conn = self.conn.lock().unwrap();
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

    pub fn revoke_api_token(&self, id: ApiTokenId) -> Result<()> {
        let now = now_text();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE api_token
             SET revoked_at = COALESCE(revoked_at, ?1), updated_at = ?1
             WHERE id = ?2",
            params![now, id.0],
        )?;
        Ok(())
    }

    pub fn list_connection_profiles(&self, tenant: TenantId) -> Result<Vec<ConnectionProfile>> {
        let conn = self.conn.lock().unwrap();
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
        let conn = self.conn.lock().unwrap();
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
        let conn = self.conn.lock().unwrap();
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
        let db_result: Result<(ConnectionProfile, Option<String>)> = {
            let mut conn = self.conn.lock().unwrap();
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
                    new_shared_secret_handle.as_deref(),
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
                let profile = self.connection_profile_by_id_locked(&conn, id)?;
                Ok((profile, old_shared_secret_handle))
            }
        };
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

    pub async fn delete_connection_profile(
        &self,
        tenant: TenantId,
        id: ConnectionProfileId,
    ) -> Result<()> {
        let handles = {
            let mut conn = self.conn.lock().unwrap();
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
            tx.commit()?;
            handles
        };
        for handle in handles {
            let _ = self.secrets.delete(SECRET_NAMESPACE, &handle).await;
        }
        Ok(())
    }

    pub async fn set_per_user_credential(
        &self,
        profile_id: ConnectionProfileId,
        principal_id: PrincipalId,
        secret: &[u8],
    ) -> Result<()> {
        let handle = Uuid::new_v4().to_string();
        self.secrets.put(SECRET_NAMESPACE, &handle, secret).await?;
        let now = now_text();
        let db_result: Result<Option<String>> = {
            let mut conn = self.conn.lock().unwrap();
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
                params![profile_id.0, principal_id.0, handle, now],
            );
            if let Err(error) = write_result {
                Err(error.into())
            } else {
                tx.commit()?;
                Ok(old_handle)
            }
        };
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
        let (profile, handle) = {
            let conn = self.conn.lock().unwrap();
            let profile = self.connection_profile_by_id_locked(&conn, id)?;
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
            (profile, handle)
        };
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
        let mut conn = self.conn.lock().unwrap();
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
        let conn = self.conn.lock().unwrap();
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
        let conn = self.conn.lock().unwrap();
        self.room_by_id_locked(&conn, id)
    }

    pub fn list_shared_rooms_for_principal(
        &self,
        tenant: TenantId,
        principal: PrincipalId,
    ) -> Result<Vec<Room>> {
        let conn = self.conn.lock().unwrap();
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
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO room_member (room_id, principal_id, role, joined_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(room_id, principal_id) DO UPDATE SET role = excluded.role",
            params![room.0, principal.0, role.as_str(), now],
        )?;
        self.room_member_locked(&conn, room, principal)
    }

    pub fn list_room_members(&self, room: RoomId) -> Result<Vec<RoomMember>> {
        let conn = self.conn.lock().unwrap();
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
        let conn = self.conn.lock().unwrap();
        self.room_member_optional_locked(&conn, room, principal)
    }

    pub fn remove_room_member(&self, room: RoomId, principal: PrincipalId) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM room_member WHERE room_id = ?1 AND principal_id = ?2",
            params![room.0, principal.0],
        )?;
        Ok(())
    }

    pub fn delete_room(&self, room: RoomId) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let deleted = conn.execute("DELETE FROM room WHERE id = ?1", params![room.0])?;
        if deleted == 0 {
            return Err(MetadataError::RoomNotFound(room));
        }
        Ok(())
    }

    pub fn create_document(&self, room: RoomId, input: NewDocument) -> Result<Document> {
        let now = now_text();
        let conn = self.conn.lock().unwrap();
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
        let conn = self.conn.lock().unwrap();
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
        let conn = self.conn.lock().unwrap();
        self.document_by_id_locked(&conn, id)
    }

    pub fn update_document_snapshot(
        &self,
        document: DocumentId,
        crdt_state: Vec<u8>,
    ) -> Result<Document> {
        let now = now_text();
        let conn = self.conn.lock().unwrap();
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
        let conn = self.conn.lock().unwrap();
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
        let conn = self.conn.lock().unwrap();
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
        let conn = self.conn.lock().unwrap();
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
        let conn = self.conn.lock().unwrap();
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
        let conn = self.conn.lock().unwrap();
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
        let now = now_text();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO operation_audit
             (at, actor_principal_id, action, target, target_id, status, result_code,
              row_count, error_message, correlation_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                now,
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
        let id = OperationAuditId(conn.last_insert_rowid());
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
        let conn = self.conn.lock().unwrap();
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
        let conn = self.conn.lock().unwrap();
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
        let conn = self.conn.lock().unwrap();
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
            .delete_connection_profile(TenantId(1), second.id)
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
            .set_per_user_credential(profile.id, PrincipalId(1), b"user-secret")
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
