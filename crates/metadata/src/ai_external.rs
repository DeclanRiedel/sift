//! Registered source metadata; encrypted definitions and opaque credentials only.
use super::{sqlite_blocking, MetadataError, MetadataStore, PrincipalId, Result, TenantId};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};
use sha2::{Digest, Sha256};
use sift_api_types::VaultId;
use sift_protocol::{
    ActivateAiExternalSourceRequest, AiExternalSource, AiExternalSourceDefinition,
    AiExternalSourceProof, AiExternalSourceState, AiExternalToolPolicy,
};
use uuid::Uuid;

pub(crate) const CREDENTIAL_NAMESPACE: &str = "sift.ai.external.v1";
mod publication;
pub(crate) use publication::ExternalSourceAuthorization;
mod receipts;
mod refresh;
pub use receipts::{AiExternalInvocation, AiExternalInvocationKind};

fn source_matches_proof(
    source: &AiExternalSource,
    tenant: TenantId,
    proof: &AiExternalSourceProof,
) -> bool {
    source.id == proof.source_id
        && source.tenant_id == tenant.0
        && source.state == AiExternalSourceState::Active
        && source.credential_scope_reviewed
        && source.revision == proof.source_revision
        && source.config_sha256 == proof.config_sha256
        && source.credential_identity == proof.credential_identity
        && source.definition.label == proof.label
}

fn require_source_pin(
    conn: &rusqlite::Connection,
    tenant: TenantId,
    proof: &AiExternalSourceProof,
) -> Result<Record> {
    let entry = record(conn, proof.source_id)?;
    if entry.tenant != tenant
        || entry.state != AiExternalSourceState::Active
        || !entry.reviewed
        || entry.revision != proof.source_revision
        || entry.config_sha256 != proof.config_sha256
        || entry.credential_identity != proof.credential_identity
    {
        return Err(invalid(
            "External source changed; select its reviewed revision again",
        ));
    }
    vault_scope(conn, entry.tenant, entry.owner, entry.vault, false)?;
    Ok(entry)
}

/// Server-internal credential resolution; deliberately neither Debug nor serde.
pub struct AiExternalCredential {
    pub source: AiExternalSource,
    pub bearer_token: Option<String>,
}

struct Record {
    id: Uuid,
    tenant: TenantId,
    owner: PrincipalId,
    vault: Option<VaultId>,
    revision: u64,
    state: AiExternalSourceState,
    config_handle: String,
    config_sha256: String,
    credential_identity: Uuid,
    credential_handle: Option<String>,
    reviewed: bool,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

fn validate_credential_metadata(token: &str, bytes: &[u8]) -> Result<()> {
    let encoded_token = STANDARD.encode(token);
    if !(16..=8192).contains(&token.len())
        || token.bytes().any(|byte| {
            !byte.is_ascii_alphanumeric()
                && !matches!(byte, b'-' | b'_' | b'.' | b'~' | b'+' | b'/' | b'=')
        })
        || bytes
            .windows(encoded_token.len().max(1))
            .any(|window| window == encoded_token.as_bytes())
        || bytes
            .windows(token.len().max(1))
            .any(|window| window == token.as_bytes())
    {
        return Err(invalid(
            "External credential is invalid or appears in source metadata",
        ));
    }
    Ok(())
}

fn invalid(message: &str) -> MetadataError {
    MetadataError::AiInvalid(message.into())
}

fn record(conn: &rusqlite::Connection, id: Uuid) -> Result<Record> {
    conn.query_row("SELECT tenant_id,owner_principal_id,vault_id,revision,state,config_handle,config_sha256,credential_identity,credential_handle,credential_scope_reviewed,created_at,updated_at FROM ai_external_source WHERE id=?1", [id.to_string()], |row| {
        let state: String = row.get(4)?;
        let state = match state.as_str() {
            "draft" => AiExternalSourceState::Draft,
            "active" => AiExternalSourceState::Active,
            "disabled" => AiExternalSourceState::Disabled,
            _ => return Err(rusqlite::Error::InvalidQuery),
        };
        let identity: String = row.get(7)?;
        Ok(Record {
            id, tenant: TenantId(row.get(0)?), owner: PrincipalId(row.get(1)?), vault: row.get::<_,Option<i64>>(2)?.map(VaultId),
            revision: row.get(3)?, state, config_handle: row.get(5)?, config_sha256: row.get(6)?,
            credential_identity: Uuid::parse_str(&identity).map_err(|_| rusqlite::Error::InvalidQuery)?,
            credential_handle: row.get(8)?, reviewed: row.get(9)?, created_at: super::parse_time_sql(row.get(10)?)?, updated_at: super::parse_time_sql(row.get(11)?)?,
        })
    }).optional()?.ok_or(MetadataError::AiNotFound)
}

fn vault_scope(
    conn: &rusqlite::Connection,
    tenant: TenantId,
    actor: PrincipalId,
    vault: Option<VaultId>,
    edit: bool,
) -> Result<()> {
    super::ensure_principal_tenant_member_locked(conn, tenant, actor)?;
    if let Some(vault) = vault {
        let actual: i64 = conn
            .query_row(
                "SELECT tenant_id FROM vault WHERE id=?1",
                [vault.0],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(MetadataError::AiNotFound)?;
        if actual != tenant.0 {
            return Err(MetadataError::AiAccessDenied);
        }
        super::vault::require_capability(
            conn,
            vault,
            sift_api_types::PrincipalId(actor.0),
            |capabilities| capabilities.use_secret && (!edit || capabilities.edit),
        )?;
    }
    Ok(())
}

fn readable(conn: &rusqlite::Connection, entry: &Record, actor: PrincipalId) -> Result<()> {
    vault_scope(conn, entry.tenant, entry.owner, entry.vault, false)?;
    vault_scope(conn, entry.tenant, actor, entry.vault, false)?;
    if actor != entry.owner {
        let team_vault = entry
            .vault
            .map(|vault| {
                conn.query_row(
                    "SELECT scope='team' FROM vault WHERE id=?1",
                    [vault.0],
                    |row| row.get::<_, bool>(0),
                )
            })
            .transpose()?
            .unwrap_or(false);
        if !team_vault || entry.state != AiExternalSourceState::Active {
            return Err(MetadataError::AiNotFound);
        }
    }
    Ok(())
}

fn editable(conn: &rusqlite::Connection, entry: &Record, actor: PrincipalId) -> Result<()> {
    super::ensure_tenant_admin_locked(conn, entry.tenant, actor)?;
    if actor != entry.owner {
        return Err(MetadataError::AiAccessDenied);
    }
    vault_scope(conn, entry.tenant, actor, entry.vault, true)
}

fn definition_bytes(definition: &AiExternalSourceDefinition) -> Result<Vec<u8>> {
    if definition.label.trim().is_empty()
        || definition.label.len() > 120
        || definition.label.chars().any(char::is_control)
        || definition.endpoint.len() > 4096
        || definition.tools.is_empty()
        || definition.tools.len() > 32
    {
        return Err(invalid(
            "External source label or inventory exceeds its limits",
        ));
    }
    let mut aliases = std::collections::HashSet::new();
    let mut names = std::collections::HashSet::new();
    for tool in &definition.tools {
        if tool.name.is_empty()
            || tool.name.len() > 256
            || tool.description.len() > 2048
            || !tool.input_schema.is_object()
            || tool
                .output_schema
                .as_ref()
                .is_some_and(|schema| !schema.is_object())
            || tool.schema_sha256.len() != 64
            || !tool
                .schema_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || tool.alias.is_empty()
            || tool.alias.len() > 64
            || !tool
                .alias
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            || !aliases.insert(&tool.alias)
            || !names.insert(&tool.name)
        {
            return Err(invalid(
                "External source has invalid or duplicate tool definitions",
            ));
        }
    }
    let bytes = serde_json::to_vec(definition)?;
    if bytes.len() > 256 * 1024 {
        return Err(invalid("External inventory exceeds its byte limit"));
    }
    Ok(bytes)
}

fn config_digest(entry: &Record, definition: &AiExternalSourceDefinition) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&serde_json::json!({
            "definition":definition,"tenant":entry.tenant.0,"owner":entry.owner.0,
            "vault":entry.vault.map(|id|id.0),"credential_identity":entry.credential_identity
        }))?)
    ))
}

impl MetadataStore {
    pub async fn ai_external_source_tenant(&self, id: Uuid) -> Result<TenantId> {
        let store = self.clone();
        sqlite_blocking(move || {
            let conn = store.conn()?;
            Ok(record(&conn, id)?.tenant)
        })
        .await
    }

    pub async fn authorize_ai_external_private_source(
        &self,
        tenant: TenantId,
        actor: PrincipalId,
        proof: &AiExternalSourceProof,
    ) -> Result<AiExternalSource> {
        if proof.room_grant_id.is_some() {
            return Err(invalid("Room source grant requires room authorization"));
        }
        let source = self.ai_external_source(proof.source_id, actor).await?;
        if !source_matches_proof(&source, tenant, proof) {
            return Err(invalid(
                "External source changed; select its reviewed revision again",
            ));
        }
        Ok(source)
    }
    pub async fn list_ai_external_sources(
        &self,
        tenant: TenantId,
        actor: PrincipalId,
    ) -> Result<Vec<AiExternalSource>> {
        let store = self.clone();
        let ids = sqlite_blocking(move || {
            let conn = store.conn()?;
            super::ensure_principal_tenant_member_locked(&conn, tenant, actor)?;
            let mut statement = conn.prepare("SELECT id FROM ai_external_source WHERE tenant_id=?1 ORDER BY created_at,id LIMIT 32")?;
            let ids = statement.query_map([tenant.0], |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            ids.into_iter().map(|id| Uuid::parse_str(&id).map_err(|_| invalid("External source identity is invalid"))).collect::<Result<Vec<_>>>()
        }).await?;
        let mut sources = Vec::new();
        for id in ids {
            match self.ai_external_source(id, actor).await {
                Ok(source) => sources.push(source),
                Err(
                    MetadataError::AiNotFound
                    | MetadataError::AiAccessDenied
                    | MetadataError::VaultPermissionDenied
                    | MetadataError::TenantMembershipRequired { .. },
                ) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(sources)
    }

    /// Resolve only an explicitly selected private source pin. Room grants are
    /// authorized separately and cannot be substituted for private authority.
    pub async fn ai_external_private_credential(
        &self,
        tenant: TenantId,
        actor: PrincipalId,
        proof: AiExternalSourceProof,
    ) -> Result<AiExternalCredential> {
        if proof.room_grant_id.is_some() {
            return Err(invalid("Room source grant requires room authorization"));
        }
        let source = self
            .authorize_ai_external_private_source(tenant, actor, &proof)
            .await?;
        let store = self.clone();
        let id = source.id;
        let handle = sqlite_blocking(move || {
            let conn = store.conn()?;
            let entry = record(&conn, id)?;
            readable(&conn, &entry, actor)?;
            Ok(entry.credential_handle)
        })
        .await?;
        let bearer_token = match handle {
            Some(handle) => Some(
                String::from_utf8(
                    self.secrets
                        .get(CREDENTIAL_NAMESPACE, &handle)
                        .await?
                        .ok_or_else(|| {
                            invalid("External credential is unavailable; review the source")
                        })?,
                )
                .map_err(|_| invalid("External credential is invalid"))?,
            ),
            None => None,
        };
        let fresh = self.ai_external_source(source.id, actor).await?;
        if !source_matches_proof(&fresh, tenant, &proof) {
            return Err(invalid(
                "External source changed while resolving its credential",
            ));
        }
        Ok(AiExternalCredential {
            source: fresh,
            bearer_token,
        })
    }

    pub async fn authorize_ai_external_registration(
        &self,
        tenant: TenantId,
        actor: PrincipalId,
        vault: Option<VaultId>,
    ) -> Result<()> {
        let store = self.clone();
        sqlite_blocking(move || {
            let conn = store.conn()?;
            super::ensure_tenant_admin_locked(&conn, tenant, actor)?;
            vault_scope(&conn, tenant, actor, vault, true)
        })
        .await
    }

    pub async fn create_ai_external_source(
        &self,
        tenant: TenantId,
        actor: PrincipalId,
        vault: Option<VaultId>,
        definition: AiExternalSourceDefinition,
        token: Option<String>,
    ) -> Result<AiExternalSource> {
        self.authorize_ai_external_registration(tenant, actor, vault)
            .await?;
        if definition
            .tools
            .iter()
            .any(|tool| tool.policy != AiExternalToolPolicy::Unavailable)
        {
            return Err(invalid("Discovery cannot approve an external tool"));
        }
        let bytes = definition_bytes(&definition)?;
        let now = Utc::now();
        let mut entry = Record {
            id: Uuid::new_v4(),
            tenant,
            owner: actor,
            vault,
            revision: 1,
            state: AiExternalSourceState::Draft,
            config_handle: String::new(),
            config_sha256: String::new(),
            credential_identity: Uuid::new_v4(),
            credential_handle: token.as_ref().map(|_| Uuid::new_v4().to_string()),
            reviewed: false,
            created_at: now,
            updated_at: now,
        };
        entry.config_sha256 = config_digest(&entry, &definition)?;
        if let (Some(handle), Some(token)) = (&entry.credential_handle, &token) {
            validate_credential_metadata(token, &bytes)?;
            self.secrets
                .put(CREDENTIAL_NAMESPACE, handle, token.as_bytes())
                .await?;
        }
        let handle = match self.ai_content.put(tenant.0, &bytes).await {
            Ok(handle) => handle,
            Err(error) => {
                if let Some(handle) = &entry.credential_handle {
                    self.queue_external_credential_cleanup(
                        handle,
                        "external_source_create_failed",
                    )?;
                }
                return Err(error);
            }
        };
        entry.config_handle = handle.clone();
        let source = AiExternalSource {
            id: entry.id,
            tenant_id: tenant.0,
            owner_principal_id: actor.0,
            vault_id: vault.map(|id| id.0),
            revision: 1,
            state: entry.state,
            definition,
            config_sha256: entry.config_sha256.clone(),
            credential_identity: entry.credential_identity,
            credential_configured: entry.credential_handle.is_some(),
            credential_scope_reviewed: false,
            created_at: now,
            updated_at: now,
        };
        let credential = entry.credential_handle.clone();
        let stored_handle = handle.clone();
        let store = self.clone();
        let inserted = sqlite_blocking(move || {
            let mut conn = store.conn()?; let tx = conn.transaction()?;
            super::ensure_tenant_admin_locked(&tx, tenant, actor)?;
            vault_scope(&tx, tenant, actor, vault, true)?;
            let count: i64 = tx.query_row("SELECT COUNT(*) FROM ai_external_source WHERE tenant_id=?1", [tenant.0], |row|row.get(0))?;
            if count >= 32 { return Err(invalid("Tenant external source limit reached")); }
            tx.execute("INSERT INTO ai_external_source(id,tenant_id,owner_principal_id,vault_id,revision,state,config_handle,config_sha256,credential_identity,credential_handle,credential_scope_reviewed,created_at,updated_at) VALUES(?1,?2,?3,?4,1,'draft',?5,?6,?7,?8,0,?9,?9)",
                params![entry.id.to_string(),tenant.0,actor.0,vault.map(|id|id.0),stored_handle,entry.config_sha256,entry.credential_identity.to_string(),entry.credential_handle,now.to_rfc3339()])?;
            tx.commit()?; Ok(())
        }).await;
        if let Err(error) = inserted {
            if let Some(handle) = credential {
                self.queue_external_credential_cleanup(&handle, "external_source_create_failed")?;
            }
            self.queue_external_content_cleanup(tenant, &handle)?;
            return Err(error);
        }
        Ok(source)
    }

    fn queue_external_credential_cleanup(&self, handle: &str, reason: &str) -> Result<()> {
        self.conn()?.execute("INSERT OR IGNORE INTO vault_secret_cleanup_queue(namespace,secret_handle,reason,not_before) VALUES(?1,?2,?3,?4)", params![CREDENTIAL_NAMESPACE,handle,reason,Utc::now().to_rfc3339()])?;
        Ok(())
    }

    fn queue_external_content_cleanup(&self, tenant: TenantId, handle: &str) -> Result<()> {
        self.conn()?.execute("INSERT OR IGNORE INTO ai_content_cleanup(tenant_id,content_handle,queued_at) VALUES(?1,?2,?3)", params![tenant.0,handle,Utc::now().to_rfc3339()])?;
        Ok(())
    }

    pub async fn ai_external_source(
        &self,
        id: Uuid,
        actor: PrincipalId,
    ) -> Result<AiExternalSource> {
        let store = self.clone();
        let entry = sqlite_blocking(move || {
            let conn = store.conn()?;
            let entry = record(&conn, id)?;
            readable(&conn, &entry, actor)?;
            Ok(entry)
        })
        .await?;
        let bytes = self
            .ai_content
            .get(entry.tenant.0, &entry.config_handle)
            .await?
            .ok_or_else(|| {
                MetadataError::AiContent("External source definition is unavailable".into())
            })?;
        let definition: AiExternalSourceDefinition = serde_json::from_slice(&bytes)?;
        if config_digest(&entry, &definition)? != entry.config_sha256 {
            return Err(invalid("External source definition digest does not match"));
        }
        let store = self.clone();
        let revision = entry.revision;
        sqlite_blocking(move || {
            let conn = store.conn()?;
            let fresh = record(&conn, id)?;
            readable(&conn, &fresh, actor)?;
            if fresh.revision != revision {
                return Err(invalid("External source changed during the read"));
            }
            Ok(())
        })
        .await?;
        Ok(AiExternalSource {
            id,
            tenant_id: entry.tenant.0,
            owner_principal_id: entry.owner.0,
            vault_id: entry.vault.map(|id| id.0),
            revision: entry.revision,
            state: entry.state,
            definition,
            config_sha256: entry.config_sha256,
            credential_identity: entry.credential_identity,
            credential_configured: entry.credential_handle.is_some(),
            credential_scope_reviewed: entry.reviewed,
            created_at: entry.created_at,
            updated_at: entry.updated_at,
        })
    }

    pub async fn activate_ai_external_source(
        &self,
        id: Uuid,
        actor: PrincipalId,
        request: ActivateAiExternalSourceRequest,
    ) -> Result<AiExternalSource> {
        let mut source = self.ai_external_source(id, actor).await?;
        if source.state != AiExternalSourceState::Draft
            || source.revision != request.expected_revision
            || source.config_sha256 != request.expected_config_sha256
            || !request.credential_scope_reviewed
            || request.tools.is_empty()
            || request.tools.len() > source.definition.tools.len()
        {
            return Err(invalid(
                "Review the current draft and credential scope before activating it",
            ));
        }
        for tool in &mut source.definition.tools {
            tool.policy = AiExternalToolPolicy::Unavailable;
        }
        let mut aliases = std::collections::HashSet::new();
        for approval in request.tools {
            if !aliases.insert(approval.alias.clone()) {
                return Err(invalid("External tool approvals contain duplicates"));
            }
            let tool = source
                .definition
                .tools
                .iter_mut()
                .find(|tool| {
                    tool.alias == approval.alias
                        && tool.schema_sha256 == approval.expected_schema_sha256
                })
                .ok_or_else(|| invalid("External tool schema changed; review it again"))?;
            tool.policy = approval.policy;
        }
        if source
            .definition
            .tools
            .iter()
            .all(|tool| tool.policy == AiExternalToolPolicy::Unavailable)
        {
            return Err(invalid(
                "Approve at least one read or local proposal adapter",
            ));
        }
        let store = self.clone();
        let entry = sqlite_blocking(move || {
            let conn = store.conn()?;
            let entry = record(&conn, id)?;
            editable(&conn, &entry, actor)?;
            Ok(entry)
        })
        .await?;
        let digest = config_digest(&entry, &source.definition)?;
        let handle = self
            .ai_content
            .put(source.tenant_id, &definition_bytes(&source.definition)?)
            .await?;
        let written_handle = handle.clone();
        let expected = request.expected_revision;
        let expected_digest = request.expected_config_sha256;
        let now = Utc::now();
        let store = self.clone();
        let saved_digest = digest.clone();
        let updated = sqlite_blocking(move || {
            let mut conn = store.conn()?; let tx = conn.transaction()?; let fresh = record(&tx,id)?; editable(&tx,&fresh,actor)?;
            if fresh.state != AiExternalSourceState::Draft || fresh.revision != expected || fresh.config_sha256 != expected_digest { return Err(invalid("External source changed; review its draft again")); }
            tx.execute("UPDATE ai_external_source SET state='active',revision=revision+1,config_handle=?2,config_sha256=?3,credential_scope_reviewed=1,updated_at=?4 WHERE id=?1",params![id.to_string(),written_handle,saved_digest,now.to_rfc3339()])?;
            tx.commit()?; Ok(())
        }).await;
        if let Err(error) = updated {
            self.conn()?.execute("INSERT OR IGNORE INTO ai_content_cleanup(tenant_id,content_handle,queued_at) VALUES(?1,?2,?3)", params![source.tenant_id,handle,now.to_rfc3339()])?;
            return Err(error);
        }
        self.ai_external_source(id, actor).await
    }

    pub async fn disable_ai_external_source(
        &self,
        id: Uuid,
        actor: PrincipalId,
        expected_revision: u64,
    ) -> Result<()> {
        let store = self.clone();
        sqlite_blocking(move || {
            let mut conn = store.conn()?; let tx = conn.transaction()?; let entry = record(&tx,id)?;
            super::ensure_tenant_admin_locked(&tx,entry.tenant,actor)?;
            if entry.revision != expected_revision { return Err(invalid("External source revision changed")); }
            tx.execute("UPDATE ai_external_source SET state='disabled',revision=revision+1,credential_scope_reviewed=0,updated_at=?2 WHERE id=?1", params![id.to_string(),Utc::now().to_rfc3339()])?;
            tx.commit()?; Ok(())
        }).await
    }

    pub async fn delete_ai_external_source(
        &self,
        id: Uuid,
        actor: PrincipalId,
        expected_revision: u64,
    ) -> Result<()> {
        let store = self.clone();
        sqlite_blocking(move || {
            let mut conn = store.conn()?;
            let tx = conn.transaction()?;
            let entry = record(&tx, id)?;
            super::ensure_tenant_admin_locked(&tx, entry.tenant, actor)?;
            if entry.revision != expected_revision {
                return Err(invalid("External source revision changed"));
            }
            tx.execute(
                "DELETE FROM ai_external_source WHERE id=?1",
                [id.to_string()],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MembershipRole, MemorySecretStore, SecretStore};
    use sift_protocol::{AiExternalToolApproval, AiExternalToolDefinition, AiMcpRevision};
    use std::sync::Arc;

    #[derive(Default)]
    pub(super) struct PausedSecrets {
        inner: MemorySecretStore,
        pub(super) reached: tokio::sync::Notify,
        pub(super) resume: tokio::sync::Notify,
    }
    #[async_trait::async_trait]
    impl SecretStore for PausedSecrets {
        async fn put(&self, namespace: &str, handle: &str, secret: &[u8]) -> Result<()> {
            self.inner.put(namespace, handle, secret).await
        }
        async fn get(&self, namespace: &str, handle: &str) -> Result<Option<Vec<u8>>> {
            if namespace == CREDENTIAL_NAMESPACE {
                self.reached.notify_one();
                self.resume.notified().await;
            }
            self.inner.get(namespace, handle).await
        }
        async fn delete(&self, namespace: &str, handle: &str) -> Result<()> {
            self.inner.delete(namespace, handle).await
        }
    }

    pub(super) fn definition() -> AiExternalSourceDefinition {
        AiExternalSourceDefinition {
            label: "Reviewed fixture".into(),
            endpoint: "https://example.invalid/mcp".into(),
            protocol: AiMcpRevision::Modern20260728,
            tools: vec![AiExternalToolDefinition {
                name: "lookup".into(),
                alias: "lookup".into(),
                title: None,
                description: "Find a record".into(),
                input_schema: serde_json::json!({"type":"object","properties":{"key":{"type":"string"}},"required":["key"],"additionalProperties":false}),
                output_schema: None,
                annotations: None,
                schema_sha256: "a".repeat(64),
                policy: AiExternalToolPolicy::Unavailable,
            }],
        }
    }

    pub(super) fn approval(source: &AiExternalSource) -> ActivateAiExternalSourceRequest {
        ActivateAiExternalSourceRequest {
            expected_revision: source.revision,
            expected_config_sha256: source.config_sha256.clone(),
            credential_scope_reviewed: true,
            tools: vec![AiExternalToolApproval {
                alias: "lookup".into(),
                expected_schema_sha256: "a".repeat(64),
                policy: AiExternalToolPolicy::Read,
            }],
        }
    }

    #[tokio::test]
    async fn activation_binds_review_and_restore_invalidates_it() {
        let store = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        store.bootstrap_local("owner").unwrap();
        let owner = PrincipalId(1);
        let source = store
            .create_ai_external_source(TenantId(1), owner, None, definition(), None)
            .await
            .unwrap();
        assert_eq!(source.state, AiExternalSourceState::Draft);
        let mut request = approval(&source);
        request.credential_scope_reviewed = false;
        assert!(store
            .activate_ai_external_source(source.id, owner, request)
            .await
            .is_err());
        let mut request = approval(&source);
        request.tools[0].expected_schema_sha256 = "b".repeat(64);
        assert!(store
            .activate_ai_external_source(source.id, owner, request)
            .await
            .is_err());
        let mut request = approval(&source);
        request.tools.push(request.tools[0].clone());
        assert!(store
            .activate_ai_external_source(source.id, owner, request)
            .await
            .is_err());
        let active = store
            .activate_ai_external_source(source.id, owner, approval(&source))
            .await
            .unwrap();
        assert_eq!(active.revision, 2);
        assert_eq!(active.state, AiExternalSourceState::Active);
        assert!(active.credential_scope_reviewed);
        let proof = AiExternalSourceProof {
            source_id: active.id,
            source_revision: active.revision,
            config_sha256: active.config_sha256.clone(),
            credential_identity: active.credential_identity,
            label: active.definition.label.clone(),
            room_grant_id: None,
        };
        assert!(store
            .ai_external_private_credential(TenantId(1), owner, proof.clone())
            .await
            .is_ok());
        let mut stale = proof.clone();
        stale.credential_identity = Uuid::new_v4();
        assert!(store
            .ai_external_private_credential(TenantId(1), owner, stale)
            .await
            .is_err());
        let mut room = proof.clone();
        room.room_grant_id = Some(Uuid::new_v4());
        assert!(store
            .ai_external_private_credential(TenantId(1), owner, room)
            .await
            .is_err());
        assert!(store
            .ai_external_private_credential(TenantId(99), owner, proof.clone())
            .await
            .is_err());
        assert_ne!(active.config_sha256, source.config_sha256);
        assert!(store
            .activate_ai_external_source(source.id, owner, approval(&source))
            .await
            .is_err());
        store.sanitize_restored_database().unwrap();
        let restored = store.ai_external_source(source.id, owner).await.unwrap();
        assert_eq!(restored.state, AiExternalSourceState::Draft);
        assert_eq!(restored.revision, 3);
        assert!(!restored.credential_scope_reviewed);
        assert!(store
            .ai_external_private_credential(TenantId(1), owner, proof)
            .await
            .is_err());
        assert!(store
            .activate_ai_external_source(source.id, owner, approval(&active))
            .await
            .is_err());
        let mut request = approval(&restored);
        request.tools[0].policy = AiExternalToolPolicy::Unavailable;
        assert!(store
            .activate_ai_external_source(source.id, owner, request)
            .await
            .is_err());
        store
            .activate_ai_external_source(source.id, owner, approval(&restored))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn private_scope_and_credential_cleanup_survive_source_deletion() {
        let secrets = Arc::new(MemorySecretStore::new());
        let store = MetadataStore::open_in_memory(secrets.clone()).unwrap();
        store.bootstrap_local("owner").unwrap();
        let owner = PrincipalId(1);
        let peer = store.create_principal("peer", "peer", None).unwrap().id;
        store
            .upsert_tenant_membership(TenantId(1), peer, MembershipRole::Member)
            .unwrap();
        let token = "fixture-credential-0123456789";
        let source = store
            .create_ai_external_source(TenantId(1), owner, None, definition(), Some(token.into()))
            .await
            .unwrap();
        assert!(source.credential_configured);
        assert!(!serde_json::to_string(&source).unwrap().contains(token));
        assert!(store.ai_external_source(source.id, peer).await.is_err());
        assert!(store
            .create_ai_external_source(TenantId(1), peer, None, definition(), None)
            .await
            .is_err());
        let entry = record(&store.conn().unwrap(), source.id).unwrap();
        let handle = entry.credential_handle.unwrap();
        assert_eq!(
            secrets
                .get(CREDENTIAL_NAMESPACE, &handle)
                .await
                .unwrap()
                .unwrap(),
            token.as_bytes()
        );
        assert!(store
            .ai_content_handles_offline(None)
            .unwrap()
            .contains(&(1, entry.config_handle.clone())));
        store
            .queue_external_credential_cleanup(&handle, "fixture_live_reference")
            .unwrap();
        store
            .queue_external_content_cleanup(TenantId(1), &entry.config_handle)
            .unwrap();
        assert_eq!(
            store.process_vault_secret_cleanup(100).await.unwrap(),
            (0, 0)
        );
        assert_eq!(store.process_ai_content_cleanup(100).await.unwrap(), (0, 0));
        let active = store
            .activate_ai_external_source(source.id, owner, approval(&source))
            .await
            .unwrap();
        assert!(store.ai_external_source(source.id, peer).await.is_err());
        assert!(store
            .delete_ai_external_source(source.id, owner, source.revision)
            .await
            .is_err());
        store
            .delete_ai_external_source(source.id, owner, active.revision)
            .await
            .unwrap();
        assert_eq!(
            store.process_vault_secret_cleanup(100).await.unwrap(),
            (1, 0)
        );
        assert!(secrets
            .get(CREDENTIAL_NAMESPACE, &handle)
            .await
            .unwrap()
            .is_none());
        assert_eq!(store.process_ai_content_cleanup(100).await.unwrap(), (2, 0));
        assert!(store.ai_content_handles_offline(None).unwrap().is_empty());
    }

    #[tokio::test]
    async fn revoked_owner_cannot_supply_credentials_after_a_blocked_secret_read() {
        let secrets = Arc::new(PausedSecrets {
            inner: MemorySecretStore::new(),
            reached: Default::default(),
            resume: Default::default(),
        });
        let store = MetadataStore::open_in_memory(secrets.clone()).unwrap();
        store.bootstrap_local("owner").unwrap();
        let owner = PrincipalId(1);
        let source = store
            .create_ai_external_source(
                TenantId(1),
                owner,
                None,
                definition(),
                Some("fixture-credential-0123456789".into()),
            )
            .await
            .unwrap();
        let active = store
            .activate_ai_external_source(source.id, owner, approval(&source))
            .await
            .unwrap();
        let proof = AiExternalSourceProof {
            source_id: active.id,
            source_revision: active.revision,
            config_sha256: active.config_sha256,
            credential_identity: active.credential_identity,
            label: active.definition.label,
            room_grant_id: None,
        };
        let worker_store = store.clone();
        let reader = tokio::spawn(async move {
            worker_store
                .ai_external_private_credential(TenantId(1), owner, proof)
                .await
        });
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            secrets.reached.notified(),
        )
        .await
        .unwrap();
        store
            .conn()
            .unwrap()
            .execute(
                "DELETE FROM membership WHERE tenant_id=1 AND principal_id=1",
                [],
            )
            .unwrap();
        secrets.resume.notify_one();
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), reader)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
    }
}
