use rusqlite::{params, Connection, OptionalExtension, Transaction};
use sift_api_types::{
    PrincipalId, TenantId, Vault, VaultGrant, VaultId, VaultItem, VaultItemId, VaultItemMetadata,
    VaultItemVersion, VaultSecretStatus,
};
use sift_protocol::{VaultCapabilities, VaultItemKind, VaultScope};
use uuid::Uuid;

use crate::{
    connection_profile_by_id_locked, ensure_principal_tenant_member_locked,
    ensure_tenant_member_role_locked, insert_operation_audit_row, now_text, parse_time,
    sqlite_blocking, validate_provider_credentials, ConnectionProfile, ConnectionProfileId,
    CredentialMode, MetadataError, MetadataStore, NewConnectionProfile, NewOperationAudit, Result,
};

fn metadata_tenant(id: TenantId) -> crate::TenantId {
    crate::TenantId(id.0)
}

fn metadata_principal(id: PrincipalId) -> crate::PrincipalId {
    crate::PrincipalId(id.0)
}

pub(crate) const VAULT_SECRET_NAMESPACE: &str = "sift.vault.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VaultPolicy {
    pub max_label_bytes: usize,
    pub max_metadata_bytes: usize,
    pub max_secret_bytes: usize,
    pub max_vaults_per_tenant: u64,
    pub max_items_per_vault: u64,
    pub max_versions_per_item: u64,
    pub cleanup_retry_initial_secs: u64,
    pub cleanup_retry_max_secs: u64,
}

impl Default for VaultPolicy {
    fn default() -> Self {
        Self {
            max_label_bytes: 160,
            max_metadata_bytes: 32 * 1024,
            max_secret_bytes: 64 * 1024,
            max_vaults_per_tenant: 100,
            max_items_per_vault: 1_000,
            max_versions_per_item: 50,
            cleanup_retry_initial_secs: 30,
            cleanup_retry_max_secs: 3_600,
        }
    }
}

fn audit(
    actor: PrincipalId,
    action: &str,
    target: &str,
    target_id: Option<i64>,
) -> NewOperationAudit {
    NewOperationAudit {
        actor_principal_id: Some(metadata_principal(actor)),
        action: action.into(),
        target: target.into(),
        target_id,
        status: "succeeded".into(),
        result_code: None,
        row_count: None,
        error_message: None,
        correlation_id: None,
    }
}

fn parse_scope(value: String) -> Result<VaultScope> {
    match value.as_str() {
        "personal" => Ok(VaultScope::Personal),
        "team" => Ok(VaultScope::Team),
        _ => Err(MetadataError::InvalidEnum {
            field: "vault.scope",
            value,
        }),
    }
}

fn kind_text(kind: VaultItemKind) -> &'static str {
    match kind {
        VaultItemKind::Connection => "connection",
        VaultItemKind::Login => "login",
        VaultItemKind::Token => "token",
        VaultItemKind::SecureNote => "secure_note",
    }
}

fn parse_kind(value: String) -> Result<VaultItemKind> {
    match value.as_str() {
        "connection" => Ok(VaultItemKind::Connection),
        "login" => Ok(VaultItemKind::Login),
        "token" => Ok(VaultItemKind::Token),
        "secure_note" => Ok(VaultItemKind::SecureNote),
        _ => Err(MetadataError::InvalidEnum {
            field: "vault_item.kind",
            value,
        }),
    }
}

fn validate_label(label: &str, policy: VaultPolicy) -> Result<()> {
    let trimmed = label.trim();
    if trimmed.is_empty()
        || trimmed.len() > policy.max_label_bytes
        || trimmed.chars().any(char::is_control)
    {
        return Err(MetadataError::InvalidVaultInput("invalid label".into()));
    }
    Ok(())
}

fn contains_secret_shaped_key(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(fields) => fields.iter().any(|(key, value)| {
            let key = key.to_ascii_lowercase();
            ["password", "secret", "token", "credential", "private_key"]
                .iter()
                .any(|forbidden| key.contains(forbidden))
                || contains_secret_shaped_key(value)
        }),
        serde_json::Value::Array(values) => values.iter().any(contains_secret_shaped_key),
        _ => false,
    }
}

fn validate_metadata(metadata: &VaultItemMetadata, policy: VaultPolicy) -> Result<String> {
    if let VaultItemMetadata::Connection { configuration, .. } = metadata {
        if !configuration.is_object() || contains_secret_shaped_key(configuration) {
            return Err(MetadataError::InvalidVaultInput(
                "connection configuration must be an object without credential-shaped fields"
                    .into(),
            ));
        }
    }
    let encoded = serde_json::to_string(metadata)?;
    if encoded.len() > policy.max_metadata_bytes {
        return Err(MetadataError::InvalidVaultInput(
            "item metadata is too large".into(),
        ));
    }
    Ok(encoded)
}

fn validate_secret(secret: &serde_json::Value, policy: VaultPolicy) -> Result<Vec<u8>> {
    if secret.is_null() {
        return Err(MetadataError::InvalidVaultInput(
            "secret must not be null".into(),
        ));
    }
    let encoded = serde_json::to_vec(secret)?;
    if encoded.is_empty() || encoded.len() > policy.max_secret_bytes {
        return Err(MetadataError::InvalidVaultInput(
            "secret is empty or too large".into(),
        ));
    }
    Ok(encoded)
}

fn capabilities_from_columns(
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> rusqlite::Result<VaultCapabilities> {
    Ok(VaultCapabilities {
        inspect: row.get(offset)?,
        use_secret: row.get(offset + 1)?,
        reveal: row.get(offset + 2)?,
        edit: row.get(offset + 3)?,
        manage: row.get(offset + 4)?,
    }
    .normalized())
}

fn vault_capabilities_locked(
    conn: &Connection,
    vault_id: VaultId,
    actor: PrincipalId,
) -> Result<(TenantId, VaultScope, VaultCapabilities)> {
    let (tenant, scope, owner): (i64, String, Option<i64>) = conn
        .query_row(
            "SELECT tenant_id, scope, owner_principal_id FROM vault WHERE id = ?1",
            params![vault_id.0],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
        .ok_or(MetadataError::VaultNotFound(vault_id))?;
    let tenant = TenantId(tenant);
    ensure_principal_tenant_member_locked(
        conn,
        metadata_tenant(tenant),
        metadata_principal(actor),
    )?;
    let scope = parse_scope(scope)?;
    if scope == VaultScope::Personal {
        return if owner == Some(actor.0) {
            Ok((tenant, scope, VaultCapabilities::OWNER))
        } else {
            Err(MetadataError::VaultPermissionDenied)
        };
    }
    let granted = conn
        .query_row(
            "SELECT can_inspect, can_use, can_reveal, can_edit, can_manage
             FROM vault_grant WHERE vault_id = ?1 AND principal_id = ?2",
            params![vault_id.0, actor.0],
            |row| capabilities_from_columns(row, 0),
        )
        .optional()?;
    let admin: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM membership
         WHERE tenant_id = ?1 AND principal_id = ?2 AND role IN ('owner', 'admin'))",
        params![tenant.0, actor.0],
        |row| row.get(0),
    )?;
    let mut capabilities = granted.unwrap_or_default();
    if admin {
        capabilities.inspect = true;
        capabilities.manage = true;
    }
    Ok((tenant, scope, capabilities.normalized()))
}

pub(crate) fn require_capability(
    conn: &Connection,
    vault_id: VaultId,
    actor: PrincipalId,
    predicate: impl FnOnce(VaultCapabilities) -> bool,
) -> Result<VaultCapabilities> {
    let (_, _, capabilities) = vault_capabilities_locked(conn, vault_id, actor)?;
    if predicate(capabilities) {
        Ok(capabilities)
    } else {
        Err(MetadataError::VaultPermissionDenied)
    }
}

fn vault_from_row(row: &rusqlite::Row<'_>, capabilities: VaultCapabilities) -> Result<Vault> {
    Ok(Vault {
        id: VaultId(row.get(0)?),
        tenant_id: TenantId(row.get(1)?),
        scope: parse_scope(row.get(2)?)?,
        owner_principal_id: row.get::<_, Option<i64>>(3)?.map(PrincipalId),
        name: row.get(4)?,
        revision: row.get(5)?,
        effective_capabilities: capabilities,
        created_at: parse_time(row.get(6)?)?,
        updated_at: parse_time(row.get(7)?)?,
    })
}

fn vault_by_id_locked(conn: &Connection, id: VaultId, actor: PrincipalId) -> Result<Vault> {
    let (_, _, capabilities) = vault_capabilities_locked(conn, id, actor)?;
    conn.query_row(
        "SELECT id, tenant_id, scope, owner_principal_id, name, revision, created_at, updated_at
         FROM vault WHERE id = ?1",
        params![id.0],
        |row| {
            vault_from_row(row, capabilities).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })
        },
    )
    .map_err(Into::into)
}

fn item_from_row(row: &rusqlite::Row<'_>) -> Result<VaultItem> {
    let kind = parse_kind(row.get(2)?)?;
    let metadata: VaultItemMetadata = serde_json::from_str(&row.get::<_, String>(4)?)?;
    if metadata.kind() != kind {
        return Err(MetadataError::InvalidVaultInput(
            "stored item kind does not match metadata".into(),
        ));
    }
    Ok(VaultItem {
        id: VaultItemId(row.get(0)?),
        vault_id: VaultId(row.get(1)?),
        kind,
        label: row.get(3)?,
        metadata,
        secret_status: if row.get::<_, bool>(5)? {
            VaultSecretStatus::Configured
        } else {
            VaultSecretStatus::Missing
        },
        head_version: row.get(6)?,
        revision: row.get(7)?,
        created_by: PrincipalId(row.get(8)?),
        created_at: parse_time(row.get(9)?)?,
        updated_at: parse_time(row.get(10)?)?,
    })
}

fn item_by_id_locked(conn: &Connection, id: VaultItemId, actor: PrincipalId) -> Result<VaultItem> {
    let vault_id = conn
        .query_row(
            "SELECT vault_id FROM vault_item WHERE id = ?1",
            params![id.0],
            |row| row.get::<_, i64>(0).map(VaultId),
        )
        .optional()?
        .ok_or(MetadataError::VaultItemNotFound(id))?;
    require_capability(conn, vault_id, actor, |capabilities| capabilities.inspect)?;
    conn.query_row(
        "SELECT i.id, i.vault_id, i.kind, i.label, i.metadata_json,
                EXISTS(SELECT 1 FROM vault_item_version v
                       WHERE v.item_id = i.id AND v.version = i.head_version
                         AND v.secret_handle IS NOT NULL),
                i.head_version, i.revision, i.created_by, i.created_at, i.updated_at
         FROM vault_item i WHERE i.id = ?1",
        params![id.0],
        |row| {
            item_from_row(row).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })
        },
    )
    .optional()?
    .ok_or(MetadataError::VaultItemNotFound(id))
}

fn enqueue_cleanup(tx: &Transaction<'_>, handle: &str, reason: &str) -> Result<()> {
    tx.execute(
        "INSERT OR IGNORE INTO vault_secret_cleanup_queue
         (namespace, secret_handle, reason, not_before)
         VALUES (?1, ?2, ?3, ?4)",
        params![VAULT_SECRET_NAMESPACE, handle, reason, now_text()],
    )?;
    Ok(())
}

fn sync_connection_item_locked(
    tx: &Transaction<'_>,
    item: VaultItemId,
    label: &str,
    metadata: &VaultItemMetadata,
    now: &str,
) -> Result<Option<ConnectionProfileId>> {
    let profile = tx
        .query_row(
            "SELECT connection_profile_id FROM vault_connection_binding WHERE item_id = ?1",
            params![item.0],
            |row| row.get::<_, i64>(0).map(ConnectionProfileId),
        )
        .optional()?;
    let Some(profile) = profile else {
        return Ok(None);
    };
    let VaultItemMetadata::Connection {
        provider_id,
        configuration,
    } = metadata
    else {
        return Err(MetadataError::InvalidVaultInput(
            "bound connection item metadata changed kind".into(),
        ));
    };
    let configuration = serde_json::to_string(configuration)?;
    tx.execute(
        "UPDATE connection_profile SET name = ?2, provider_id = ?3,
         configuration_json = ?4, spec_json = ?4, updated_at = ?5 WHERE id = ?1",
        params![profile.0, label, provider_id.as_str(), configuration, now],
    )?;
    Ok(Some(profile))
}

impl MetadataStore {
    pub(crate) async fn delete_vault_secret_handles(
        &self,
        handles: Vec<String>,
        reason: &str,
    ) -> Result<()> {
        for handle in handles {
            if self
                .secrets
                .delete(VAULT_SECRET_NAMESPACE, &handle)
                .await
                .is_err()
            {
                let mut conn = self.conn()?;
                let tx = conn.transaction()?;
                enqueue_cleanup(&tx, &handle, reason)?;
                tx.commit()?;
            }
        }
        Ok(())
    }

    pub async fn process_vault_secret_cleanup(&self, limit: u32) -> Result<(u64, u64)> {
        let policy = self.vault_policy();
        let limit = limit.clamp(1, 1_000);
        let pending = {
            let conn = self.conn()?;
            let mut stmt = conn.prepare(
                "SELECT secret_handle, attempts FROM vault_secret_cleanup_queue
                 WHERE namespace = ?1 AND not_before <= ?2
                 ORDER BY not_before, secret_handle LIMIT ?3",
            )?;
            let rows = stmt
                .query_map(params![VAULT_SECRET_NAMESPACE, now_text(), limit], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?))
                })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut deleted = 0_u64;
        let mut failed = 0_u64;
        for (handle, attempts) in pending {
            match self.secrets.delete(VAULT_SECRET_NAMESPACE, &handle).await {
                Ok(()) => {
                    self.conn()?.execute(
                        "DELETE FROM vault_secret_cleanup_queue
                         WHERE namespace = ?1 AND secret_handle = ?2",
                        params![VAULT_SECRET_NAMESPACE, handle],
                    )?;
                    deleted += 1;
                }
                Err(_) => {
                    let initial =
                        i64::try_from(policy.cleanup_retry_initial_secs).unwrap_or(i64::MAX);
                    let maximum = i64::try_from(policy.cleanup_retry_max_secs).unwrap_or(i64::MAX);
                    let delay = initial.saturating_mul(1_i64 << attempts.min(20));
                    let not_before = (chrono::Utc::now()
                        + chrono::Duration::seconds(delay.min(maximum)))
                    .to_rfc3339();
                    self.conn()?.execute(
                        "UPDATE vault_secret_cleanup_queue
                         SET attempts = attempts + 1, last_error = ?3, not_before = ?4
                         WHERE namespace = ?1 AND secret_handle = ?2",
                        params![
                            VAULT_SECRET_NAMESPACE,
                            handle,
                            "secret backend delete failed",
                            not_before
                        ],
                    )?;
                    failed += 1;
                }
            }
        }
        Ok((deleted, failed))
    }

    pub fn prune_vault_item_versions(&self, limit: u32) -> Result<u64> {
        let policy = self.vault_policy();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let candidates = {
            let mut stmt = tx.prepare(
                "SELECT item_id, version, secret_handle FROM (
                    SELECT item_id, version, secret_handle,
                           ROW_NUMBER() OVER (
                               PARTITION BY item_id ORDER BY version DESC
                           ) AS retained_rank
                    FROM vault_item_version
                 ) WHERE retained_rank > ?1
                 ORDER BY item_id, version
                 LIMIT ?2",
            )?;
            let rows = stmt
                .query_map(
                    params![policy.max_versions_per_item, limit.clamp(1, 10_000)],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, u64>(1)?,
                            row.get::<_, Option<String>>(2)?,
                        ))
                    },
                )?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };
        for (item, version, handle) in &candidates {
            tx.execute(
                "DELETE FROM vault_item_version WHERE item_id = ?1 AND version = ?2",
                params![item, version],
            )?;
            if let Some(handle) = handle {
                let still_referenced: bool = tx.query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM vault_item_version WHERE secret_handle = ?1
                     )",
                    params![handle],
                    |row| row.get(0),
                )?;
                if !still_referenced {
                    enqueue_cleanup(&tx, handle, "vault_version_retention")?;
                }
            }
        }
        tx.commit()?;
        Ok(candidates.len() as u64)
    }

    pub fn vault_connection_binding(
        &self,
        profile: ConnectionProfileId,
    ) -> Result<Option<(VaultId, VaultItemId)>> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT i.vault_id, i.id
             FROM vault_connection_binding b
             JOIN vault_item i ON i.id = b.item_id
             WHERE b.connection_profile_id = ?1",
            params![profile.0],
            |row| Ok((VaultId(row.get(0)?), VaultItemId(row.get(1)?))),
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn vault_connection_binding_by_item(
        &self,
        item: VaultItemId,
    ) -> Result<Option<(VaultId, ConnectionProfileId)>> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT i.vault_id, b.connection_profile_id
             FROM vault_connection_binding b
             JOIN vault_item i ON i.id = b.item_id WHERE b.item_id = ?1",
            params![item.0],
            |row| Ok((VaultId(row.get(0)?), ConnectionProfileId(row.get(1)?))),
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn vault_connection_profiles(&self, vault: VaultId) -> Result<Vec<ConnectionProfileId>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT b.connection_profile_id
             FROM vault_connection_binding b
             JOIN vault_item i ON i.id = b.item_id
             WHERE i.vault_id = ?1",
        )?;
        let rows = stmt.query_map(params![vault.0], |row| {
            row.get::<_, i64>(0).map(ConnectionProfileId)
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn ensure_personal_vault(&self, tenant: TenantId, actor: PrincipalId) -> Result<Vault> {
        let policy = self.vault_policy();
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        ensure_principal_tenant_member_locked(
            &tx,
            metadata_tenant(tenant),
            metadata_principal(actor),
        )?;
        let existing = tx
            .query_row(
                "SELECT id FROM vault WHERE tenant_id = ?1 AND scope = 'personal'
                 AND owner_principal_id = ?2",
                params![tenant.0, actor.0],
                |row| row.get::<_, i64>(0).map(VaultId),
            )
            .optional()?;
        let id = if let Some(id) = existing {
            id
        } else {
            let count: u64 = tx.query_row(
                "SELECT COUNT(*) FROM vault WHERE tenant_id = ?1",
                params![tenant.0],
                |row| row.get(0),
            )?;
            if count >= policy.max_vaults_per_tenant {
                return Err(MetadataError::VaultQuotaExceeded("tenant vault count"));
            }
            let now = now_text();
            tx.execute(
                "INSERT INTO vault
                 (tenant_id, scope, owner_principal_id, name, created_by, created_at, updated_at)
                 VALUES (?1, 'personal', ?2, 'My Vault', ?2, ?3, ?3)",
                params![tenant.0, actor.0, now],
            )?;
            let id = VaultId(tx.last_insert_rowid());
            insert_operation_audit_row(&tx, &audit(actor, "create", "vault", Some(id.0)))?;
            id
        };
        tx.commit()?;
        vault_by_id_locked(&conn, id, actor)
    }

    pub fn create_team_vault(
        &self,
        tenant: TenantId,
        actor: PrincipalId,
        name: &str,
    ) -> Result<Vault> {
        let policy = self.vault_policy();
        validate_label(name, policy)?;
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        ensure_tenant_member_role_locked(&tx, metadata_tenant(tenant), metadata_principal(actor))?;
        let count: u64 = tx.query_row(
            "SELECT COUNT(*) FROM vault WHERE tenant_id = ?1",
            params![tenant.0],
            |row| row.get(0),
        )?;
        if count >= policy.max_vaults_per_tenant {
            return Err(MetadataError::VaultQuotaExceeded("tenant vault count"));
        }
        let now = now_text();
        tx.execute(
            "INSERT INTO vault
             (tenant_id, scope, owner_principal_id, name, created_by, created_at, updated_at)
             VALUES (?1, 'team', NULL, ?2, ?3, ?4, ?4)",
            params![tenant.0, name.trim(), actor.0, now],
        )?;
        let id = VaultId(tx.last_insert_rowid());
        let owner = VaultCapabilities::OWNER;
        tx.execute(
            "INSERT INTO vault_grant
             (vault_id, principal_id, can_inspect, can_use, can_reveal, can_edit,
              can_manage, created_by, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?2, ?8, ?8)",
            params![
                id.0,
                actor.0,
                owner.inspect,
                owner.use_secret,
                owner.reveal,
                owner.edit,
                owner.manage,
                now
            ],
        )?;
        insert_operation_audit_row(&tx, &audit(actor, "create", "vault", Some(id.0)))?;
        tx.commit()?;
        vault_by_id_locked(&conn, id, actor)
    }

    pub fn list_vaults(&self, tenant: TenantId, actor: PrincipalId) -> Result<Vec<Vault>> {
        self.ensure_personal_vault(tenant, actor)?;
        let conn = self.conn()?;
        ensure_principal_tenant_member_locked(
            &conn,
            metadata_tenant(tenant),
            metadata_principal(actor),
        )?;
        let mut stmt =
            conn.prepare("SELECT id FROM vault WHERE tenant_id = ?1 ORDER BY scope, name, id")?;
        let ids = stmt
            .query_map(params![tenant.0], |row| row.get::<_, i64>(0).map(VaultId))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(ids
            .into_iter()
            .filter_map(|id| vault_by_id_locked(&conn, id, actor).ok())
            .filter(|vault| vault.effective_capabilities.inspect)
            .collect())
    }

    pub fn get_vault(&self, id: VaultId, actor: PrincipalId) -> Result<Vault> {
        let conn = self.conn()?;
        vault_by_id_locked(&conn, id, actor)
    }

    pub fn update_vault(
        &self,
        id: VaultId,
        actor: PrincipalId,
        expected_revision: u64,
        name: &str,
    ) -> Result<Vault> {
        validate_label(name, self.vault_policy())?;
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        require_capability(&tx, id, actor, |capabilities| capabilities.manage)?;
        let current = tx
            .query_row(
                "SELECT revision FROM vault WHERE id = ?1",
                params![id.0],
                |row| row.get::<_, u64>(0),
            )
            .optional()?
            .ok_or(MetadataError::VaultNotFound(id))?;
        if current != expected_revision {
            return Err(MetadataError::VaultRevisionConflict {
                expected: expected_revision,
                current,
            });
        }
        tx.execute(
            "UPDATE vault SET name = ?2, revision = revision + 1, updated_at = ?3
             WHERE id = ?1",
            params![id.0, name.trim(), now_text()],
        )?;
        insert_operation_audit_row(&tx, &audit(actor, "update", "vault", Some(id.0)))?;
        tx.commit()?;
        vault_by_id_locked(&conn, id, actor)
    }

    pub async fn delete_vault(
        &self,
        id: VaultId,
        actor: PrincipalId,
        expected_revision: u64,
    ) -> Result<Vec<ConnectionProfileId>> {
        let backend = self.backend.clone();
        let (profiles, handles) = sqlite_blocking(move || {
            let mut conn = backend.conn()?;
            let tx = conn.transaction()?;
            let (_, scope, _) = vault_capabilities_locked(&tx, id, actor)?;
            require_capability(&tx, id, actor, |capabilities| capabilities.manage)?;
            if scope == VaultScope::Personal {
                return Err(MetadataError::InvalidVaultInput(
                    "personal vaults cannot be deleted".into(),
                ));
            }
            let current = tx.query_row(
                "SELECT revision FROM vault WHERE id = ?1",
                params![id.0],
                |row| row.get::<_, u64>(0),
            )?;
            if current != expected_revision {
                return Err(MetadataError::VaultRevisionConflict {
                    expected: expected_revision,
                    current,
                });
            }
            let profiles = {
                let mut stmt = tx.prepare(
                    "SELECT b.connection_profile_id FROM vault_connection_binding b
                     JOIN vault_item i ON i.id = b.item_id WHERE i.vault_id = ?1",
                )?;
                let rows = stmt
                    .query_map(params![id.0], |row| {
                        row.get::<_, i64>(0).map(ConnectionProfileId)
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                rows
            };
            let handles = {
                let mut stmt = tx.prepare(
                    "SELECT DISTINCT v.secret_handle FROM vault_item_version v
                     JOIN vault_item i ON i.id = v.item_id
                     WHERE i.vault_id = ?1 AND v.secret_handle IS NOT NULL",
                )?;
                let rows = stmt
                    .query_map(params![id.0], |row| row.get::<_, String>(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                rows
            };
            for profile in &profiles {
                tx.execute(
                    "DELETE FROM connection_profile WHERE id = ?1",
                    params![profile.0],
                )?;
            }
            tx.execute("DELETE FROM vault WHERE id = ?1", params![id.0])?;
            insert_operation_audit_row(&tx, &audit(actor, "delete", "vault", Some(id.0)))?;
            tx.commit()?;
            Ok((profiles, handles))
        })
        .await?;
        self.delete_vault_secret_handles(handles, "delete_vault")
            .await?;
        Ok(profiles)
    }

    pub fn list_vault_grants(&self, id: VaultId, actor: PrincipalId) -> Result<Vec<VaultGrant>> {
        let conn = self.conn()?;
        require_capability(&conn, id, actor, |capabilities| capabilities.manage)?;
        let mut stmt = conn.prepare(
            "SELECT vault_id, principal_id, can_inspect, can_use, can_reveal,
                    can_edit, can_manage, revision, created_by, created_at, updated_at
             FROM vault_grant WHERE vault_id = ?1 ORDER BY principal_id",
        )?;
        let mapped = stmt.query_map(params![id.0], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                capabilities_from_columns(row, 2)?,
                row.get::<_, u64>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, String>(10)?,
            ))
        })?;
        mapped
            .map(|row| {
                let (vault, principal, capabilities, revision, created_by, created_at, updated_at) =
                    row?;
                Ok(VaultGrant {
                    vault_id: VaultId(vault),
                    principal_id: PrincipalId(principal),
                    capabilities,
                    revision,
                    created_by: PrincipalId(created_by),
                    created_at: parse_time(created_at)?,
                    updated_at: parse_time(updated_at)?,
                })
            })
            .collect()
    }

    pub fn set_vault_grant(
        &self,
        id: VaultId,
        actor: PrincipalId,
        principal: PrincipalId,
        expected_revision: Option<u64>,
        capabilities: VaultCapabilities,
    ) -> Result<VaultGrant> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let (tenant, scope, _) = vault_capabilities_locked(&tx, id, actor)?;
        require_capability(&tx, id, actor, |capabilities| capabilities.manage)?;
        if scope != VaultScope::Team {
            return Err(MetadataError::VaultPermissionDenied);
        }
        ensure_principal_tenant_member_locked(
            &tx,
            metadata_tenant(tenant),
            metadata_principal(principal),
        )?;
        let current = tx
            .query_row(
                "SELECT revision FROM vault_grant WHERE vault_id = ?1 AND principal_id = ?2",
                params![id.0, principal.0],
                |row| row.get::<_, u64>(0),
            )
            .optional()?;
        if let (Some(expected), Some(current)) = (expected_revision, current) {
            if expected != current {
                return Err(MetadataError::VaultRevisionConflict { expected, current });
            }
        } else if expected_revision.is_some() != current.is_some() {
            return Err(MetadataError::VaultRevisionConflict {
                expected: expected_revision.unwrap_or(0),
                current: current.unwrap_or(0),
            });
        }
        let capabilities = capabilities.normalized();
        let now = now_text();
        tx.execute(
            "INSERT INTO vault_grant
             (vault_id, principal_id, can_inspect, can_use, can_reveal, can_edit,
              can_manage, revision, created_by, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8, ?9, ?9)
             ON CONFLICT(vault_id, principal_id) DO UPDATE SET
                can_inspect = excluded.can_inspect, can_use = excluded.can_use,
                can_reveal = excluded.can_reveal, can_edit = excluded.can_edit,
                can_manage = excluded.can_manage, revision = vault_grant.revision + 1,
                updated_at = excluded.updated_at",
            params![
                id.0,
                principal.0,
                capabilities.inspect,
                capabilities.use_secret,
                capabilities.reveal,
                capabilities.edit,
                capabilities.manage,
                actor.0,
                now
            ],
        )?;
        insert_operation_audit_row(&tx, &audit(actor, "grant", "vault", Some(id.0)))?;
        tx.commit()?;
        drop(conn);
        self.list_vault_grants(id, actor)?
            .into_iter()
            .find(|grant| grant.principal_id == principal)
            .ok_or(MetadataError::VaultPermissionDenied)
    }

    pub fn delete_vault_grant(
        &self,
        id: VaultId,
        actor: PrincipalId,
        principal: PrincipalId,
        expected_revision: u64,
    ) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let (_, scope, _) = vault_capabilities_locked(&tx, id, actor)?;
        require_capability(&tx, id, actor, |capabilities| capabilities.manage)?;
        if scope != VaultScope::Team || actor == principal {
            return Err(MetadataError::VaultPermissionDenied);
        }
        let current = tx
            .query_row(
                "SELECT revision FROM vault_grant WHERE vault_id = ?1 AND principal_id = ?2",
                params![id.0, principal.0],
                |row| row.get::<_, u64>(0),
            )
            .optional()?
            .ok_or(MetadataError::VaultPermissionDenied)?;
        if current != expected_revision {
            return Err(MetadataError::VaultRevisionConflict {
                expected: expected_revision,
                current,
            });
        }
        tx.execute(
            "DELETE FROM vault_grant WHERE vault_id = ?1 AND principal_id = ?2",
            params![id.0, principal.0],
        )?;
        insert_operation_audit_row(&tx, &audit(actor, "revoke", "vault", Some(id.0)))?;
        tx.commit()?;
        Ok(())
    }

    pub fn list_vault_items(&self, id: VaultId, actor: PrincipalId) -> Result<Vec<VaultItem>> {
        let conn = self.conn()?;
        require_capability(&conn, id, actor, |capabilities| capabilities.inspect)?;
        let mut stmt = conn.prepare(
            "SELECT i.id, i.vault_id, i.kind, i.label, i.metadata_json,
                    EXISTS(SELECT 1 FROM vault_item_version v
                           WHERE v.item_id = i.id AND v.version = i.head_version
                             AND v.secret_handle IS NOT NULL),
                    i.head_version, i.revision, i.created_by, i.created_at, i.updated_at
             FROM vault_item i WHERE i.vault_id = ?1 ORDER BY i.label, i.id",
        )?;
        let mapped = stmt.query_map(params![id.0], |row| {
            item_from_row(row).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })
        })?;
        Ok(mapped.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn get_vault_item(&self, id: VaultItemId, actor: PrincipalId) -> Result<VaultItem> {
        let conn = self.conn()?;
        item_by_id_locked(&conn, id, actor)
    }

    pub async fn create_vault_item(
        &self,
        id: VaultId,
        actor: PrincipalId,
        label: String,
        metadata: VaultItemMetadata,
        secret: Option<serde_json::Value>,
    ) -> Result<VaultItem> {
        let policy = self.vault_policy();
        validate_label(&label, policy)?;
        let metadata_json = validate_metadata(&metadata, policy)?;
        let kind = metadata.kind();
        let new_secret = if let Some(secret) = secret {
            let encoded = validate_secret(&secret, policy)?;
            let handle = Uuid::new_v4().to_string();
            self.secrets
                .put(VAULT_SECRET_NAMESPACE, &handle, &encoded)
                .await?;
            Some(handle)
        } else {
            None
        };
        let backend = self.backend.clone();
        let db_handle = new_secret.clone();
        let result = sqlite_blocking(move || {
            let mut conn = backend.conn()?;
            let tx = conn.transaction()?;
            require_capability(&tx, id, actor, |capabilities| capabilities.edit)?;
            let count: u64 = tx.query_row(
                "SELECT COUNT(*) FROM vault_item WHERE vault_id = ?1",
                params![id.0],
                |row| row.get(0),
            )?;
            if count >= policy.max_items_per_vault {
                return Err(MetadataError::VaultQuotaExceeded("vault item count"));
            }
            let now = now_text();
            tx.execute(
                "INSERT INTO vault_item
                 (vault_id, kind, label, metadata_json, created_by, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                params![
                    id.0,
                    kind_text(kind),
                    label.trim(),
                    metadata_json,
                    actor.0,
                    now
                ],
            )?;
            let item_id = VaultItemId(tx.last_insert_rowid());
            tx.execute(
                "INSERT INTO vault_item_version
                 (item_id, version, parent_version, metadata_json, secret_handle,
                  change_summary, created_by, created_at)
                 VALUES (?1, 1, NULL, ?2, ?3, 'Created', ?4, ?5)",
                params![item_id.0, metadata_json, db_handle, actor.0, now],
            )?;
            insert_operation_audit_row(
                &tx,
                &audit(actor, "create", "vault_item", Some(item_id.0)),
            )?;
            tx.commit()?;
            item_by_id_locked(&conn, item_id, actor)
        })
        .await;
        if result.is_err() {
            if let Some(handle) = new_secret {
                if self
                    .secrets
                    .delete(VAULT_SECRET_NAMESPACE, &handle)
                    .await
                    .is_err()
                {
                    let mut conn = self.conn()?;
                    let tx = conn.transaction()?;
                    enqueue_cleanup(&tx, &handle, "create_item_rollback")?;
                    tx.commit()?;
                }
            }
        }
        result
    }

    pub async fn update_vault_item(
        &self,
        id: VaultItemId,
        actor: PrincipalId,
        expected_revision: u64,
        label: String,
        metadata: VaultItemMetadata,
        secret: Option<serde_json::Value>,
    ) -> Result<(VaultItem, Option<ConnectionProfileId>)> {
        let policy = self.vault_policy();
        validate_label(&label, policy)?;
        let metadata_json = validate_metadata(&metadata, policy)?;
        if metadata.kind() == VaultItemKind::Connection {
            if let Some(secret) = secret.as_ref() {
                validate_provider_credentials(secret)?;
            }
        }
        let new_handle = if let Some(secret) = secret.as_ref() {
            let bytes = validate_secret(secret, policy)?;
            let handle = Uuid::new_v4().to_string();
            self.secrets
                .put(VAULT_SECRET_NAMESPACE, &handle, &bytes)
                .await?;
            Some(handle)
        } else {
            None
        };
        let backend = self.backend.clone();
        let db_handle = new_handle.clone();
        let result: Result<(VaultItem, Option<ConnectionProfileId>)> = sqlite_blocking(move || {
            let mut conn = backend.conn()?;
            let tx = conn.transaction()?;
            let (vault_id, kind, current, head, inherited): (
                i64,
                String,
                u64,
                u64,
                Option<String>,
            ) = tx
                .query_row(
                    "SELECT i.vault_id, i.kind, i.revision, i.head_version, v.secret_handle
                     FROM vault_item i JOIN vault_item_version v
                       ON v.item_id = i.id AND v.version = i.head_version
                     WHERE i.id = ?1",
                    params![id.0],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                        ))
                    },
                )
                .optional()?
                .ok_or(MetadataError::VaultItemNotFound(id))?;
            require_capability(&tx, VaultId(vault_id), actor, |capabilities| {
                capabilities.edit
            })?;
            if current != expected_revision {
                return Err(MetadataError::VaultRevisionConflict {
                    expected: expected_revision,
                    current,
                });
            }
            if parse_kind(kind)? != metadata.kind() {
                return Err(MetadataError::InvalidVaultInput(
                    "vault item kind cannot be changed".into(),
                ));
            }
            let now = now_text();
            let next = head + 1;
            let version_handle = db_handle.clone().or(inherited);
            tx.execute(
                "UPDATE vault_item SET label = ?2, metadata_json = ?3,
                 head_version = ?4, revision = revision + 1, updated_at = ?5 WHERE id = ?1",
                params![id.0, label.trim(), metadata_json, next, now],
            )?;
            tx.execute(
                "INSERT INTO vault_item_version
                 (item_id, version, parent_version, metadata_json, secret_handle,
                  change_summary, created_by, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    id.0,
                    next,
                    head,
                    metadata_json,
                    version_handle,
                    if db_handle.is_some() {
                        "Updated and rotated"
                    } else {
                        "Updated"
                    },
                    actor.0,
                    now,
                ],
            )?;
            let profile = sync_connection_item_locked(&tx, id, label.trim(), &metadata, &now)?;
            insert_operation_audit_row(&tx, &audit(actor, "update", "vault_item", Some(id.0)))?;
            tx.commit()?;
            Ok((item_by_id_locked(&conn, id, actor)?, profile))
        })
        .await;
        if result.is_err() {
            if let Some(handle) = new_handle {
                self.delete_vault_secret_handles(vec![handle], "update_vault_item_rollback")
                    .await?;
            }
        }
        result
    }

    pub async fn set_vault_secret(
        &self,
        id: VaultItemId,
        actor: PrincipalId,
        expected_revision: u64,
        secret: serde_json::Value,
    ) -> Result<(VaultItem, Option<ConnectionProfileId>)> {
        let item = self.get_vault_item(id, actor)?;
        self.update_vault_item(
            id,
            actor,
            expected_revision,
            item.label,
            item.metadata,
            Some(secret),
        )
        .await
    }

    pub fn clear_vault_secret(
        &self,
        id: VaultItemId,
        actor: PrincipalId,
        expected_revision: u64,
    ) -> Result<(VaultItem, Option<ConnectionProfileId>)> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let (vault_id, current, head, metadata_json): (i64, u64, u64, String) = tx
            .query_row(
                "SELECT vault_id, revision, head_version, metadata_json
                 FROM vault_item WHERE id = ?1",
                params![id.0],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?
            .ok_or(MetadataError::VaultItemNotFound(id))?;
        require_capability(&tx, VaultId(vault_id), actor, |capabilities| {
            capabilities.edit
        })?;
        if current != expected_revision {
            return Err(MetadataError::VaultRevisionConflict {
                expected: expected_revision,
                current,
            });
        }
        let now = now_text();
        let next = head + 1;
        tx.execute(
            "UPDATE vault_item SET head_version = ?2, revision = revision + 1,
             updated_at = ?3 WHERE id = ?1",
            params![id.0, next, now],
        )?;
        tx.execute(
            "INSERT INTO vault_item_version
             (item_id, version, parent_version, metadata_json, secret_handle,
              change_summary, created_by, created_at)
             VALUES (?1, ?2, ?3, ?4, NULL, 'Secret cleared', ?5, ?6)",
            params![id.0, next, head, metadata_json, actor.0, now],
        )?;
        insert_operation_audit_row(&tx, &audit(actor, "clear", "vault_item", Some(id.0)))?;
        let profile = tx
            .query_row(
                "SELECT connection_profile_id FROM vault_connection_binding WHERE item_id = ?1",
                params![id.0],
                |row| row.get::<_, i64>(0).map(ConnectionProfileId),
            )
            .optional()?;
        tx.commit()?;
        Ok((item_by_id_locked(&conn, id, actor)?, profile))
    }

    pub async fn restore_vault_item(
        &self,
        id: VaultItemId,
        actor: PrincipalId,
        expected_revision: u64,
        version: u64,
    ) -> Result<(VaultItem, Option<ConnectionProfileId>)> {
        let (metadata, source_handle) = {
            let conn = self.conn()?;
            let item = item_by_id_locked(&conn, id, actor)?;
            require_capability(&conn, item.vault_id, actor, |capabilities| {
                capabilities.edit
            })?;
            conn.query_row(
                "SELECT metadata_json, secret_handle FROM vault_item_version
                 WHERE item_id = ?1 AND version = ?2",
                params![id.0, version],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .optional()?
            .ok_or(MetadataError::VaultItemNotFound(id))?
        };
        let metadata_value: VaultItemMetadata = serde_json::from_str(&metadata)?;
        let new_handle = if let Some(source) = source_handle {
            let bytes = self
                .secrets
                .get(VAULT_SECRET_NAMESPACE, &source)
                .await?
                .ok_or(MetadataError::VaultSecretMissing)?;
            let handle = Uuid::new_v4().to_string();
            self.secrets
                .put(VAULT_SECRET_NAMESPACE, &handle, &bytes)
                .await?;
            Some(handle)
        } else {
            None
        };
        let backend = self.backend.clone();
        let db_handle = new_handle.clone();
        let result: Result<(VaultItem, Option<ConnectionProfileId>)> = sqlite_blocking(move || {
            let mut conn = backend.conn()?;
            let tx = conn.transaction()?;
            let (vault_id, current, head, label): (i64, u64, u64, String) = tx
                .query_row(
                    "SELECT vault_id, revision, head_version, label FROM vault_item WHERE id = ?1",
                    params![id.0],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()?
                .ok_or(MetadataError::VaultItemNotFound(id))?;
            require_capability(&tx, VaultId(vault_id), actor, |capabilities| {
                capabilities.edit
            })?;
            if current != expected_revision {
                return Err(MetadataError::VaultRevisionConflict {
                    expected: expected_revision,
                    current,
                });
            }
            let now = now_text();
            let next = head + 1;
            tx.execute(
                "UPDATE vault_item SET metadata_json = ?2, head_version = ?3,
                 revision = revision + 1, updated_at = ?4 WHERE id = ?1",
                params![id.0, metadata, next, now],
            )?;
            tx.execute(
                "INSERT INTO vault_item_version
                 (item_id, version, parent_version, metadata_json, secret_handle,
                  change_summary, created_by, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    id.0,
                    next,
                    head,
                    metadata,
                    db_handle,
                    format!("Restored from v{version}"),
                    actor.0,
                    now,
                ],
            )?;
            let profile = sync_connection_item_locked(&tx, id, &label, &metadata_value, &now)?;
            insert_operation_audit_row(&tx, &audit(actor, "restore", "vault_item", Some(id.0)))?;
            tx.commit()?;
            Ok((item_by_id_locked(&conn, id, actor)?, profile))
        })
        .await;
        if result.is_err() {
            if let Some(handle) = new_handle {
                self.delete_vault_secret_handles(vec![handle], "restore_vault_item_rollback")
                    .await?;
            }
        }
        result
    }

    pub async fn delete_vault_item(
        &self,
        id: VaultItemId,
        actor: PrincipalId,
        expected_revision: u64,
    ) -> Result<(VaultId, Option<ConnectionProfileId>)> {
        let backend = self.backend.clone();
        let (vault, profile, handles) = sqlite_blocking(move || {
            let mut conn = backend.conn()?;
            let tx = conn.transaction()?;
            let (vault, current): (i64, u64) = tx
                .query_row(
                    "SELECT vault_id, revision FROM vault_item WHERE id = ?1",
                    params![id.0],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?
                .ok_or(MetadataError::VaultItemNotFound(id))?;
            require_capability(&tx, VaultId(vault), actor, |capabilities| capabilities.edit)?;
            if current != expected_revision {
                return Err(MetadataError::VaultRevisionConflict {
                    expected: expected_revision,
                    current,
                });
            }
            let profile = tx
                .query_row(
                    "SELECT connection_profile_id FROM vault_connection_binding WHERE item_id = ?1",
                    params![id.0],
                    |row| row.get::<_, i64>(0).map(ConnectionProfileId),
                )
                .optional()?;
            let handles = {
                let mut stmt = tx.prepare(
                    "SELECT DISTINCT secret_handle FROM vault_item_version
                     WHERE item_id = ?1 AND secret_handle IS NOT NULL",
                )?;
                let rows = stmt
                    .query_map(params![id.0], |row| row.get::<_, String>(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                rows
            };
            if let Some(profile) = profile {
                tx.execute(
                    "DELETE FROM connection_profile WHERE id = ?1",
                    params![profile.0],
                )?;
            }
            tx.execute("DELETE FROM vault_item WHERE id = ?1", params![id.0])?;
            insert_operation_audit_row(&tx, &audit(actor, "delete", "vault_item", Some(id.0)))?;
            tx.commit()?;
            Ok((VaultId(vault), profile, handles))
        })
        .await?;
        self.delete_vault_secret_handles(handles, "delete_vault_item")
            .await?;
        Ok((vault, profile))
    }

    pub async fn upsert_vault_connection_profile(
        &self,
        tenant: TenantId,
        actor: PrincipalId,
        requested_vault: Option<VaultId>,
        mut input: NewConnectionProfile,
        max_profiles: Option<u64>,
    ) -> Result<(ConnectionProfile, VaultItemId)> {
        if input.credential_mode != CredentialMode::Shared {
            return Err(MetadataError::InvalidVaultInput(
                "vault-backed connections require shared credential mode".into(),
            ));
        }
        let policy = self.vault_policy();
        validate_label(&input.name, policy)?;
        let item_metadata = VaultItemMetadata::Connection {
            provider_id: input.provider_id.clone(),
            configuration: input.configuration.clone(),
        };
        let metadata_json = validate_metadata(&item_metadata, policy)?;
        let credentials = input.credentials.take();
        let new_secret = if let Some(credentials) = credentials.as_ref() {
            validate_provider_credentials(credentials)?;
            let encoded = validate_secret(credentials, policy)?;
            let handle = Uuid::new_v4().to_string();
            self.secrets
                .put(VAULT_SECRET_NAMESPACE, &handle, &encoded)
                .await?;
            Some(handle)
        } else {
            None
        };
        let vault_id = if let Some(vault) = requested_vault {
            vault
        } else {
            let existing = {
                let conn = self.conn()?;
                conn.query_row(
                    "SELECT i.vault_id FROM connection_profile p
                     JOIN vault_connection_binding b ON b.connection_profile_id = p.id
                     JOIN vault_item i ON i.id = b.item_id
                     WHERE p.tenant_id = ?1 AND p.name = ?2",
                    params![tenant.0, input.name],
                    |row| row.get::<_, i64>(0).map(VaultId),
                )
                .optional()?
            };
            match existing {
                Some(vault) => vault,
                None => self.ensure_personal_vault(tenant, actor)?.id,
            }
        };
        let now = now_text();
        let configuration_json = serde_json::to_string(&input.configuration)?;
        let tags_json = serde_json::to_string(&input.tags)?;
        let backend = self.backend.clone();
        let db_secret = new_secret.clone();
        let result: Result<(ConnectionProfile, VaultItemId, Option<String>)> =
            sqlite_blocking(move || {
                let mut conn = backend.conn()?;
                let tx = conn.transaction()?;
                let (vault_tenant, _, capabilities) =
                    vault_capabilities_locked(&tx, vault_id, actor)?;
                if vault_tenant != tenant || !capabilities.edit {
                    return Err(MetadataError::VaultPermissionDenied);
                }
                let existing_profile = tx
                    .query_row(
                        "SELECT id, shared_secret_handle FROM connection_profile
                         WHERE tenant_id = ?1 AND name = ?2",
                        params![tenant.0, input.name],
                        |row| {
                            Ok((
                                ConnectionProfileId(row.get(0)?),
                                row.get::<_, Option<String>>(1)?,
                            ))
                        },
                    )
                    .optional()?;
                if existing_profile.is_none() {
                    if let Some(limit) = max_profiles {
                        let count: u64 = tx.query_row(
                            "SELECT COUNT(*) FROM connection_profile WHERE tenant_id = ?1",
                            params![tenant.0],
                            |row| row.get(0),
                        )?;
                        if count >= limit {
                            return Err(MetadataError::ConnectionProfileLimitReached(
                                metadata_tenant(tenant),
                            ));
                        }
                    }
                }
                let existing_item = existing_profile
                    .as_ref()
                    .map(|(profile, _)| {
                        tx.query_row(
                            "SELECT b.item_id, i.vault_id, i.head_version
                             FROM vault_connection_binding b
                             JOIN vault_item i ON i.id = b.item_id
                             WHERE b.connection_profile_id = ?1",
                            params![profile.0],
                            |row| {
                                Ok((
                                    VaultItemId(row.get(0)?),
                                    VaultId(row.get(1)?),
                                    row.get::<_, u64>(2)?,
                                ))
                            },
                        )
                        .optional()
                    })
                    .transpose()?
                    .flatten();
                if existing_item.is_some_and(|(_, bound_vault, _)| bound_vault != vault_id) {
                    return Err(MetadataError::InvalidVaultInput(
                        "connection is already bound to another vault".into(),
                    ));
                }
                let inherited_handle = match existing_item {
                    Some((item, _, head)) => tx.query_row(
                        "SELECT secret_handle FROM vault_item_version
                         WHERE item_id = ?1 AND version = ?2",
                        params![item.0, head],
                        |row| row.get::<_, Option<String>>(0),
                    )?,
                    None => None,
                };
                let version_handle = db_secret.clone().or(inherited_handle);
                tx.execute(
                    "INSERT INTO connection_profile
                     (tenant_id, name, engine, spec_json, credential_mode, shared_secret_handle,
                      tags_json, created_by, created_at, updated_at, provider_id,
                      configuration_json, semantic_engine)
                     VALUES (?1, ?2, ?3, ?4, 'shared', NULL, ?5, ?6, ?7, ?7, ?8, ?4, ?9)
                     ON CONFLICT(tenant_id, name) DO UPDATE SET
                        engine = excluded.engine, spec_json = excluded.spec_json,
                        provider_id = excluded.provider_id,
                        configuration_json = excluded.configuration_json,
                        semantic_engine = excluded.semantic_engine,
                        credential_mode = 'shared', shared_secret_handle = NULL,
                        tags_json = excluded.tags_json, updated_at = excluded.updated_at",
                    params![
                        tenant.0,
                        input.name,
                        input
                            .semantic_engine
                            .map_or("postgres", sift_protocol::Engine::as_str),
                        configuration_json,
                        tags_json,
                        actor.0,
                        now,
                        input.provider_id.as_str(),
                        input.semantic_engine.map(sift_protocol::Engine::as_str),
                    ],
                )?;
                let profile_id = ConnectionProfileId(tx.query_row(
                    "SELECT id FROM connection_profile WHERE tenant_id = ?1 AND name = ?2",
                    params![tenant.0, input.name],
                    |row| row.get(0),
                )?);
                let item_id = if let Some((item_id, _, head)) = existing_item {
                    let next = head + 1;
                    tx.execute(
                        "UPDATE vault_item SET label = ?2, metadata_json = ?3,
                         head_version = ?4, revision = revision + 1, updated_at = ?5
                         WHERE id = ?1",
                        params![item_id.0, input.name, metadata_json, next, now],
                    )?;
                    tx.execute(
                        "INSERT INTO vault_item_version
                         (item_id, version, parent_version, metadata_json, secret_handle,
                          change_summary, created_by, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                        params![
                            item_id.0,
                            next,
                            head,
                            metadata_json,
                            version_handle,
                            if db_secret.is_some() {
                                "Connection updated; credential rotated"
                            } else {
                                "Connection metadata updated"
                            },
                            actor.0,
                            now,
                        ],
                    )?;
                    item_id
                } else {
                    let item_count: u64 = tx.query_row(
                        "SELECT COUNT(*) FROM vault_item WHERE vault_id = ?1",
                        params![vault_id.0],
                        |row| row.get(0),
                    )?;
                    if item_count >= policy.max_items_per_vault {
                        return Err(MetadataError::VaultQuotaExceeded("vault item count"));
                    }
                    tx.execute(
                        "INSERT INTO vault_item
                         (vault_id, kind, label, metadata_json, created_by, created_at, updated_at)
                         VALUES (?1, 'connection', ?2, ?3, ?4, ?5, ?5)",
                        params![vault_id.0, input.name, metadata_json, actor.0, now],
                    )?;
                    let item_id = VaultItemId(tx.last_insert_rowid());
                    tx.execute(
                        "INSERT INTO vault_item_version
                         (item_id, version, parent_version, metadata_json, secret_handle,
                          change_summary, created_by, created_at)
                         VALUES (?1, 1, NULL, ?2, ?3, 'Connection created', ?4, ?5)",
                        params![item_id.0, metadata_json, version_handle, actor.0, now],
                    )?;
                    tx.execute(
                        "INSERT INTO vault_connection_binding
                         (item_id, connection_profile_id, created_at) VALUES (?1, ?2, ?3)",
                        params![item_id.0, profile_id.0, now],
                    )?;
                    item_id
                };
                insert_operation_audit_row(
                    &tx,
                    &audit(actor, "update", "vault_item", Some(item_id.0)),
                )?;
                insert_operation_audit_row(
                    &tx,
                    &audit(actor, "upsert", "connection_profile", Some(profile_id.0)),
                )?;
                let legacy_handle = existing_profile.and_then(|(_, handle)| handle);
                tx.commit()?;
                Ok((
                    connection_profile_by_id_locked(&conn, profile_id)?,
                    item_id,
                    legacy_handle,
                ))
            })
            .await;
        match result {
            Ok((profile, item, legacy_handle)) => {
                if let Some(handle) = legacy_handle {
                    self.delete_secret_best_effort(&handle, "migrate_connection_profile_to_vault")
                        .await;
                }
                Ok((profile, item))
            }
            Err(error) => {
                if let Some(handle) = new_secret {
                    if self
                        .secrets
                        .delete(VAULT_SECRET_NAMESPACE, &handle)
                        .await
                        .is_err()
                    {
                        let mut conn = self.conn()?;
                        let tx = conn.transaction()?;
                        enqueue_cleanup(&tx, &handle, "connection_upsert_rollback")?;
                        tx.commit()?;
                    }
                }
                Err(error)
            }
        }
    }

    pub fn list_vault_item_versions(
        &self,
        id: VaultItemId,
        actor: PrincipalId,
    ) -> Result<Vec<VaultItemVersion>> {
        let conn = self.conn()?;
        let item = item_by_id_locked(&conn, id, actor)?;
        let mut stmt = conn.prepare(
            "SELECT item_id, version, metadata_json, secret_handle IS NOT NULL,
                    change_summary, created_by, created_at
             FROM vault_item_version WHERE item_id = ?1 ORDER BY version DESC",
        )?;
        let rows = stmt.query_map(params![id.0], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, u64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, bool>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?;
        let mut versions = Vec::new();
        for row in rows {
            let (
                item_id,
                version,
                metadata_json,
                secret_configured,
                summary,
                created_by,
                created_at,
            ) = row?;
            versions.push(VaultItemVersion {
                item_id: VaultItemId(item_id),
                version,
                metadata: serde_json::from_str(&metadata_json)?,
                secret_configured,
                change_summary: summary,
                created_by: PrincipalId(created_by),
                created_at: parse_time(created_at)?,
            });
        }
        debug_assert_eq!(item.id, id);
        Ok(versions)
    }

    pub fn get_vault_item_version(
        &self,
        id: VaultItemId,
        actor: PrincipalId,
        version: u64,
    ) -> Result<VaultItemVersion> {
        self.list_vault_item_versions(id, actor)?
            .into_iter()
            .find(|candidate| candidate.version == version)
            .ok_or(MetadataError::VaultItemNotFound(id))
    }

    pub fn diff_vault_item_versions(
        &self,
        id: VaultItemId,
        actor: PrincipalId,
        from: u64,
        to: u64,
    ) -> Result<sift_api_types::VaultItemVersionDiff> {
        let conn = self.conn()?;
        let item = item_by_id_locked(&conn, id, actor)?;
        let load = |version| {
            conn.query_row(
                "SELECT metadata_json, secret_handle FROM vault_item_version
                 WHERE item_id = ?1 AND version = ?2",
                params![id.0, version],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .optional()
        };
        let left = load(from)?.ok_or(MetadataError::VaultItemNotFound(id))?;
        let right = load(to)?.ok_or(MetadataError::VaultItemNotFound(id))?;
        debug_assert_eq!(item.id, id);
        Ok(sift_api_types::VaultItemVersionDiff {
            item_id: id,
            from_version: from,
            to_version: to,
            metadata_changed: left.0 != right.0,
            secret_changed: left.1 != right.1,
        })
    }

    pub async fn reveal_vault_secret(
        &self,
        id: VaultItemId,
        actor: PrincipalId,
    ) -> Result<serde_json::Value> {
        let (vault_id, handle) = {
            let conn = self.conn()?;
            let item = item_by_id_locked(&conn, id, actor)?;
            if !item.kind.revealable() {
                return Err(MetadataError::VaultSecretNotRevealable);
            }
            require_capability(&conn, item.vault_id, actor, |capabilities| {
                capabilities.reveal
            })?;
            let handle = conn
                .query_row(
                    "SELECT v.secret_handle FROM vault_item i
                     JOIN vault_item_version v ON v.item_id = i.id AND v.version = i.head_version
                     WHERE i.id = ?1",
                    params![id.0],
                    |row| row.get::<_, Option<String>>(0),
                )?
                .ok_or(MetadataError::VaultSecretMissing)?;
            (item.vault_id, handle)
        };
        let bytes = self
            .secrets
            .get(VAULT_SECRET_NAMESPACE, &handle)
            .await?
            .ok_or(MetadataError::VaultSecretMissing)?;
        let value = serde_json::from_slice(&bytes)?;
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        require_capability(&tx, vault_id, actor, |capabilities| capabilities.reveal)?;
        insert_operation_audit_row(&tx, &audit(actor, "reveal", "vault_item", Some(id.0)))?;
        tx.commit()?;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{MembershipRole, MemorySecretStore};

    fn store() -> (MetadataStore, TenantId, PrincipalId, PrincipalId) {
        let store = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        store.bootstrap_local("Owner").unwrap();
        let member = store.create_principal("member", "Member", None).unwrap();
        store
            .upsert_tenant_membership(crate::TenantId(1), member.id, MembershipRole::Member)
            .unwrap();
        (store, TenantId(1), PrincipalId(1), PrincipalId(member.id.0))
    }

    #[tokio::test]
    async fn team_use_does_not_imply_reveal_and_audits_reveal_without_value() {
        let (store, tenant, owner, member) = store();
        let vault = store.create_team_vault(tenant, owner, "Analytics").unwrap();
        store
            .set_vault_grant(
                vault.id,
                owner,
                member,
                None,
                VaultCapabilities {
                    use_secret: true,
                    ..Default::default()
                },
            )
            .unwrap();
        let item = store
            .create_vault_item(
                vault.id,
                owner,
                "Reporting login".into(),
                VaultItemMetadata::Login {
                    username: "reporter".into(),
                    url: None,
                },
                Some(serde_json::json!("sentinel-password")),
            )
            .await
            .unwrap();
        assert!(matches!(
            store.reveal_vault_secret(item.id, member).await,
            Err(MetadataError::VaultPermissionDenied)
        ));
        store
            .set_vault_grant(
                vault.id,
                owner,
                member,
                Some(1),
                VaultCapabilities {
                    reveal: true,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            store.reveal_vault_secret(item.id, member).await.unwrap(),
            serde_json::json!("sentinel-password")
        );
        let audits = store.list_operation_audit(100).unwrap();
        let encoded = serde_json::to_string(&audits).unwrap();
        assert!(encoded.contains("reveal"));
        assert!(!encoded.contains("sentinel-password"));
    }

    #[tokio::test]
    async fn connection_items_are_never_revealable() {
        let (store, tenant, owner, _) = store();
        let vault = store.ensure_personal_vault(tenant, owner).unwrap();
        let item = store
            .create_vault_item(
                vault.id,
                owner,
                "Database".into(),
                VaultItemMetadata::Connection {
                    provider_id: sift_protocol::ProviderId::new("sift/postgres").unwrap(),
                    configuration: serde_json::json!({"host": "db.internal"}),
                },
                Some(serde_json::json!({"password": "secret"})),
            )
            .await
            .unwrap();
        assert!(matches!(
            store.reveal_vault_secret(item.id, owner).await,
            Err(MetadataError::VaultSecretNotRevealable)
        ));
    }

    #[tokio::test]
    async fn vault_connection_use_tracks_grants_and_rotates_credentials() {
        let (store, tenant, owner, member) = store();
        let vault = store
            .create_team_vault(tenant, owner, "Shared data")
            .unwrap();
        store
            .set_vault_grant(
                vault.id,
                owner,
                member,
                None,
                VaultCapabilities {
                    use_secret: true,
                    ..Default::default()
                },
            )
            .unwrap();
        let input = |password: &str| NewConnectionProfile {
            name: "Warehouse".into(),
            provider_id: sift_protocol::ProviderId::new("sift/postgres").unwrap(),
            configuration: serde_json::json!({"host": "db.internal"}),
            semantic_engine: Some(sift_protocol::Engine::Postgres),
            credentials: Some(serde_json::json!({"password": password})),
            credential_mode: CredentialMode::Shared,
            tags: Vec::new(),
        };
        let (profile, item) = store
            .upsert_vault_connection_profile(tenant, owner, Some(vault.id), input("first"), None)
            .await
            .unwrap();
        let (_, credentials) = store
            .resolve_provider_connection(
                metadata_tenant(tenant),
                metadata_principal(member),
                profile.id,
            )
            .await
            .unwrap();
        assert_eq!(credentials["password"], b"first");

        let (_, rotated_item) = store
            .upsert_vault_connection_profile(tenant, owner, None, input("second"), None)
            .await
            .unwrap();
        assert_eq!(rotated_item, item);
        assert_eq!(
            store.list_vault_item_versions(item, owner).unwrap().len(),
            2
        );
        let (_, credentials) = store
            .resolve_provider_connection(
                metadata_tenant(tenant),
                metadata_principal(member),
                profile.id,
            )
            .await
            .unwrap();
        assert_eq!(credentials["password"], b"second");

        store
            .set_vault_grant(
                vault.id,
                owner,
                member,
                Some(1),
                VaultCapabilities::default(),
            )
            .unwrap();
        assert!(matches!(
            store
                .resolve_provider_connection(
                    metadata_tenant(tenant),
                    metadata_principal(member),
                    profile.id,
                )
                .await,
            Err(MetadataError::VaultPermissionDenied)
        ));
    }

    #[tokio::test]
    async fn team_admin_recovery_does_not_grant_use_or_reveal() {
        let (store, tenant, owner, member) = store();
        store
            .upsert_tenant_membership(
                metadata_tenant(tenant),
                metadata_principal(member),
                MembershipRole::Admin,
            )
            .unwrap();
        let vault = store.create_team_vault(tenant, owner, "Recovery").unwrap();
        let login = store
            .create_vault_item(
                vault.id,
                owner,
                "Shared login".into(),
                VaultItemMetadata::Login {
                    username: "reader".into(),
                    url: None,
                },
                Some(serde_json::json!("not-for-admins")),
            )
            .await
            .unwrap();
        let input = NewConnectionProfile {
            name: "Recovery database".into(),
            provider_id: sift_protocol::ProviderId::new("sift/postgres").unwrap(),
            configuration: serde_json::json!({"host": "db.internal"}),
            semantic_engine: Some(sift_protocol::Engine::Postgres),
            credentials: Some(serde_json::json!({"password": "connection-secret"})),
            credential_mode: CredentialMode::Shared,
            tags: Vec::new(),
        };
        let (profile, _) = store
            .upsert_vault_connection_profile(tenant, owner, Some(vault.id), input, None)
            .await
            .unwrap();

        let visible = store.get_vault(vault.id, member).unwrap();
        assert!(visible.effective_capabilities.inspect);
        assert!(visible.effective_capabilities.manage);
        assert!(!visible.effective_capabilities.use_secret);
        assert!(!visible.effective_capabilities.reveal);
        assert!(matches!(
            store.authorize_vault_connection_use(
                metadata_tenant(tenant),
                metadata_principal(member),
                profile.id,
            ),
            Err(MetadataError::VaultPermissionDenied)
        ));
        assert!(matches!(
            store.reveal_vault_secret(login.id, member).await,
            Err(MetadataError::VaultPermissionDenied)
        ));
    }

    #[tokio::test]
    async fn tenant_member_removal_revokes_team_vault_access() {
        let (store, tenant, owner, member) = store();
        let vault = store
            .create_team_vault(tenant, owner, "Membership")
            .unwrap();
        store
            .set_vault_grant(
                vault.id,
                owner,
                member,
                None,
                VaultCapabilities {
                    use_secret: true,
                    ..Default::default()
                },
            )
            .unwrap();
        let input = NewConnectionProfile {
            name: "Membership database".into(),
            provider_id: sift_protocol::ProviderId::new("sift/postgres").unwrap(),
            configuration: serde_json::json!({"host": "db.internal"}),
            semantic_engine: Some(sift_protocol::Engine::Postgres),
            credentials: Some(serde_json::json!({"password": "connection-secret"})),
            credential_mode: CredentialMode::Shared,
            tags: Vec::new(),
        };
        let (profile, _) = store
            .upsert_vault_connection_profile(tenant, owner, Some(vault.id), input, None)
            .await
            .unwrap();
        store
            .authorize_vault_connection_use(
                metadata_tenant(tenant),
                metadata_principal(member),
                profile.id,
            )
            .unwrap();

        store
            .remove_tenant_membership(
                metadata_tenant(tenant),
                metadata_principal(owner),
                metadata_principal(member),
                audit(owner, "remove_member", "tenant", Some(tenant.0)),
            )
            .unwrap();

        assert!(matches!(
            store.authorize_vault_connection_use(
                metadata_tenant(tenant),
                metadata_principal(member),
                profile.id,
            ),
            Err(MetadataError::TenantMembershipRequired { .. })
        ));
        assert!(store
            .list_vault_grants(vault.id, owner)
            .unwrap()
            .iter()
            .all(|grant| grant.principal_id != member));
    }

    #[tokio::test]
    async fn item_rotation_clear_restore_and_delete_are_revisioned() {
        let (store, tenant, owner, _) = store();
        let vault = store.create_team_vault(tenant, owner, "Lifecycle").unwrap();
        let item = store
            .create_vault_item(
                vault.id,
                owner,
                "Deploy token".into(),
                VaultItemMetadata::Token {
                    service: "deploy".into(),
                    expires_at: None,
                },
                Some(serde_json::json!("first")),
            )
            .await
            .unwrap();
        let (updated, _) = store
            .update_vault_item(
                item.id,
                owner,
                item.revision,
                item.label.clone(),
                item.metadata.clone(),
                Some(serde_json::json!("second")),
            )
            .await
            .unwrap();
        assert_eq!(updated.revision, 2);
        assert_eq!(
            store.reveal_vault_secret(item.id, owner).await.unwrap(),
            serde_json::json!("second")
        );
        let (cleared, _) = store
            .clear_vault_secret(item.id, owner, updated.revision)
            .unwrap();
        assert_eq!(cleared.secret_status, VaultSecretStatus::Missing);
        let (restored, _) = store
            .restore_vault_item(item.id, owner, cleared.revision, 1)
            .await
            .unwrap();
        assert_eq!(restored.head_version, 4);
        assert_eq!(
            store.reveal_vault_secret(item.id, owner).await.unwrap(),
            serde_json::json!("first")
        );
        let diff = store
            .diff_vault_item_versions(item.id, owner, 3, 4)
            .unwrap();
        assert!(diff.secret_changed);
        store
            .delete_vault_item(item.id, owner, restored.revision)
            .await
            .unwrap();
        assert!(matches!(
            store.get_vault_item(item.id, owner),
            Err(MetadataError::VaultItemNotFound(_))
        ));
    }

    #[test]
    fn team_vault_and_grant_deletion_are_revision_safe() {
        let (store, tenant, owner, member) = store();
        let vault = store.create_team_vault(tenant, owner, "Original").unwrap();
        let renamed = store
            .update_vault(vault.id, owner, vault.revision, "Renamed")
            .unwrap();
        assert_eq!(renamed.name, "Renamed");
        assert!(matches!(
            store.update_vault(vault.id, owner, vault.revision, "Stale"),
            Err(MetadataError::VaultRevisionConflict { .. })
        ));
        let grant = store
            .set_vault_grant(
                vault.id,
                owner,
                member,
                None,
                VaultCapabilities {
                    inspect: true,
                    ..Default::default()
                },
            )
            .unwrap();
        store
            .delete_vault_grant(vault.id, owner, member, grant.revision)
            .unwrap();
        assert!(store
            .list_vaults(tenant, member)
            .unwrap()
            .iter()
            .all(|row| row.id != vault.id));
    }

    #[tokio::test]
    async fn vault_policy_enforces_quotas_and_prunes_old_versions() {
        let (store, tenant, owner, _) = store();
        store.set_vault_policy(VaultPolicy {
            max_vaults_per_tenant: 1,
            max_items_per_vault: 1,
            max_versions_per_item: 2,
            ..VaultPolicy::default()
        });
        let vault = store.create_team_vault(tenant, owner, "Bounded").unwrap();
        assert!(matches!(
            store.create_team_vault(tenant, owner, "Too many"),
            Err(MetadataError::VaultQuotaExceeded("tenant vault count"))
        ));
        let item = store
            .create_vault_item(
                vault.id,
                owner,
                "Token".into(),
                VaultItemMetadata::Token {
                    service: "deploy".into(),
                    expires_at: None,
                },
                Some(serde_json::json!("one")),
            )
            .await
            .unwrap();
        assert!(matches!(
            store
                .create_vault_item(
                    vault.id,
                    owner,
                    "Second".into(),
                    VaultItemMetadata::SecureNote,
                    None,
                )
                .await,
            Err(MetadataError::VaultQuotaExceeded("vault item count"))
        ));
        let (item, _) = store
            .set_vault_secret(item.id, owner, item.revision, serde_json::json!("two"))
            .await
            .unwrap();
        store
            .set_vault_secret(item.id, owner, item.revision, serde_json::json!("three"))
            .await
            .unwrap();
        assert_eq!(store.prune_vault_item_versions(100).unwrap(), 1);
        let versions = store.list_vault_item_versions(item.id, owner).unwrap();
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0].version, 3);
        assert_eq!(versions[1].version, 2);
        assert_eq!(
            store.process_vault_secret_cleanup(100).await.unwrap(),
            (1, 0)
        );
    }
}
