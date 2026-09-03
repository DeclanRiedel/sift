use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sift_instance_config::{
    CredentialKind, CredentialMode as ManifestCredentialMode, LockFile, Manifest,
    Provider as ManifestProvider, TenantRole as ManifestTenantRole,
};
use sift_protocol::{ConnectionPolicy, OperationKind};
use uuid::Uuid;

use crate::{
    insert_operation_audit_row, now_text, MetadataError, MetadataStore, NewOperationAudit,
};

pub(crate) const INSTANCE_SECRET_NAMESPACE: &str = "sift.instance.credential.v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceApplySummary {
    pub changed: bool,
    pub created: u64,
    pub updated: u64,
    pub deleted: u64,
    pub missing_credentials: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CredentialReadiness {
    Missing,
    Ready,
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceCredentialStatus {
    pub slot: String,
    pub kind: CredentialKind,
    pub readiness: CredentialReadiness,
    pub consumers: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceResourceAction {
    Create,
    Update,
    Delete,
    Unchanged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceResourceChange {
    pub address: String,
    pub kind: String,
    pub action: InstanceResourceAction,
    pub prevent_destroy: bool,
}

#[derive(Debug, Clone)]
struct DesiredResource {
    address: String,
    kind: &'static str,
    digest: String,
    prevent_destroy: bool,
}

#[derive(Debug, Clone)]
struct DesiredSlot {
    kind: CredentialKind,
    consumer_digest: String,
    consumers: Vec<String>,
}

#[derive(Debug, Clone)]
struct ManagedResource {
    address: String,
    kind: String,
    row_id: Option<i64>,
    secondary_row_id: Option<i64>,
    desired_digest: String,
    prevent_destroy: bool,
}

impl MetadataStore {
    /// Compare desired manifest resources with the last applied ownership
    /// ledger without mutating either side.
    pub fn plan_instance_manifest(
        &self,
        manifest: &Manifest,
    ) -> crate::Result<Vec<InstanceResourceChange>> {
        manifest.validate()?;
        let desired = desired_resources(manifest)?;
        let existing = {
            let conn = self.conn()?;
            let tx = conn.unchecked_transaction()?;
            let resources = managed_resources_locked(&tx)?;
            tx.rollback()?;
            resources
        };
        let mut changes = desired
            .iter()
            .map(|resource| {
                let action = match existing.get(&resource.address) {
                    None => InstanceResourceAction::Create,
                    Some(previous) if previous.desired_digest != resource.digest => {
                        InstanceResourceAction::Update
                    }
                    Some(_) => InstanceResourceAction::Unchanged,
                };
                InstanceResourceChange {
                    address: resource.address.clone(),
                    kind: resource.kind.into(),
                    action,
                    prevent_destroy: resource.prevent_destroy,
                }
            })
            .collect::<Vec<_>>();
        changes.extend(
            existing
                .values()
                .filter(|resource| {
                    !desired
                        .iter()
                        .any(|candidate| candidate.address == resource.address)
                })
                .map(|resource| InstanceResourceChange {
                    address: resource.address.clone(),
                    kind: resource.kind.clone(),
                    action: InstanceResourceAction::Delete,
                    prevent_destroy: resource.prevent_destroy,
                }),
        );
        changes.sort_by(|left, right| {
            action_rank(left.action)
                .cmp(&action_rank(right.action))
                .then_with(|| left.address.cmp(&right.address))
        });
        Ok(changes)
    }

    /// Confirm that SQLite is the realization selected by the immutable
    /// generation pointer. Startup uses this before accepting requests.
    pub fn verify_instance_manifest_state(
        &self,
        manifest: &Manifest,
        lock: &LockFile,
        generation: u64,
    ) -> crate::Result<()> {
        let expected_generation = i64::try_from(generation).map_err(|_| {
            MetadataError::InstanceManifestConflict(
                "generation exceeds SQLite integer range".into(),
            )
        })?;
        let expected = (
            manifest.manifest_id.to_string(),
            manifest.configuration_digest()?,
            lock.digest()?,
            expected_generation,
        );
        let actual = {
            let conn = self.conn()?;
            conn.query_row(
                "SELECT manifest_id, configuration_digest, lock_digest, generation
                 FROM instance_manifest_state WHERE singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?
        };
        if actual.as_ref() != Some(&expected) {
            return Err(MetadataError::InstanceManifestConflict(
                "metadata does not match the selected applied generation; run apply again".into(),
            ));
        }
        Ok(())
    }

    /// Reconcile all manifest-owned metadata in one SQLite transaction.
    /// Secret bytes are never accepted by this method and never enter SQLite.
    pub async fn apply_instance_manifest(
        &self,
        manifest: &Manifest,
        lock: &LockFile,
        generation: u64,
        allow_destroy: bool,
    ) -> crate::Result<InstanceApplySummary> {
        manifest.validate()?;
        lock.verify(manifest)?;
        if !manifest.extensions.is_empty() {
            return Err(MetadataError::InstanceManifestConflict(
                "extension installation is not yet a realizable v1 resource".into(),
            ));
        }
        let configuration_digest = manifest.configuration_digest()?;
        let lock_digest = lock.digest()?;
        let desired_resources = desired_resources(manifest)?;
        let desired_slots = desired_slots(manifest)?;
        let now = now_text();
        let mut stale_secret_handles = Vec::new();

        let summary = {
            let mut conn = self.conn()?;
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

            let current = tx
                .query_row(
                    "SELECT manifest_id, configuration_digest, lock_digest
                     FROM instance_manifest_state WHERE singleton = 1",
                    [],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()?;
            if let Some((manifest_id, _, _)) = &current {
                if manifest_id != &manifest.manifest_id.to_string() {
                    return Err(MetadataError::InstanceManifestConflict(
                        "the state directory belongs to another manifest id".into(),
                    ));
                }
            } else {
                refuse_unmanaged_adoption(&tx)?;
            }
            let configuration_changed =
                current
                    .as_ref()
                    .map_or(true, |(_, current_config, current_lock)| {
                        current_config != &configuration_digest || current_lock != &lock_digest
                    });

            let existing = managed_resources_locked(&tx)?;
            let desired_addresses = desired_resources
                .iter()
                .map(|resource| resource.address.as_str())
                .collect::<BTreeSet<_>>();
            let mut removals = existing
                .values()
                .filter(|resource| !desired_addresses.contains(resource.address.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            removals.sort_by_key(|resource| deletion_rank(&resource.kind));
            if !removals.is_empty() && !allow_destroy {
                return Err(MetadataError::InstanceDestroyApprovalRequired(
                    removals.iter().map(|item| item.address.clone()).collect(),
                ));
            }
            if let Some(protected) = removals.iter().find(|resource| resource.prevent_destroy) {
                return Err(MetadataError::InstancePreventDestroy(
                    protected.address.clone(),
                ));
            }

            let mut deleted = 0_u64;
            for resource in removals
                .iter()
                .filter(|resource| deletion_rank(&resource.kind) <= 1)
            {
                delete_managed_resource(&tx, resource)?;
                tx.execute(
                    "DELETE FROM instance_managed_resource WHERE address = ?1",
                    params![resource.address],
                )?;
                deleted += 1;
            }

            reconcile_slots_locked(&tx, &desired_slots, &now, &mut stale_secret_handles)?;

            let mut created = 0_u64;
            let mut updated = 0_u64;
            let mut principal_ids = BTreeMap::new();
            for principal in &manifest.identity.github_principals {
                let address = format!("principal.{}", principal.name);
                let desired = desired_resources
                    .iter()
                    .find(|item| item.address == address)
                    .expect("principal desired resource");
                let row_id = if let Some(resource) = existing.get(&address) {
                    let row_id = required_row_id(resource)?;
                    tx.execute(
                        "UPDATE principal SET external_id = ?1, display_name = ?2,
                         is_instance_admin = ?3, disabled_at = NULL, updated_at = ?4
                         WHERE id = ?5",
                        params![
                            format!("github:{}", principal.subject),
                            principal.login_hint.as_deref().unwrap_or(&principal.name),
                            principal.instance_admin,
                            now,
                            row_id
                        ],
                    )?;
                    if tx.changes() == 0 {
                        return Err(tampered(&address));
                    }
                    tx.execute(
                        "UPDATE auth_identity SET issuer = 'https://github.com', subject = ?1,
                         provider_login = ?2, disabled_at = NULL, updated_at = ?3
                         WHERE principal_id = ?4 AND method = 'github'",
                        params![principal.subject, principal.login_hint, now, row_id],
                    )?;
                    if tx.changes() == 0 {
                        return Err(tampered(&address));
                    }
                    if resource.desired_digest != desired.digest {
                        updated += 1;
                    }
                    row_id
                } else {
                    tx.execute(
                        "INSERT INTO principal
                         (external_id, display_name, email, is_instance_admin, created_at, updated_at)
                         VALUES (?1, ?2, NULL, ?3, ?4, ?4)",
                        params![
                            format!("github:{}", principal.subject),
                            principal.login_hint.as_deref().unwrap_or(&principal.name),
                            principal.instance_admin,
                            now
                        ],
                    )?;
                    let row_id = tx.last_insert_rowid();
                    tx.execute(
                        "INSERT INTO auth_identity
                         (principal_id, method, issuer, subject, provider_login, created_at, updated_at)
                         VALUES (?1, 'github', 'https://github.com', ?2, ?3, ?4, ?4)",
                        params![row_id, principal.subject, principal.login_hint, now],
                    )?;
                    created += 1;
                    row_id
                };
                principal_ids.insert(principal.name.as_str(), row_id);
                upsert_managed_resource(&tx, desired, row_id, None, &now)?;
            }

            let tenant_kind = match manifest.server.deployment {
                sift_instance_config::Deployment::Personal => "personal",
                sift_instance_config::Deployment::Team => "team",
            };
            let mut tenant_ids = BTreeMap::new();
            for tenant in &manifest.tenants {
                let address = format!("tenant.{}", tenant.name);
                let desired = desired_resource(&desired_resources, &address);
                let row_id = if let Some(resource) = existing.get(&address) {
                    let row_id = required_row_id(resource)?;
                    tx.execute(
                        "UPDATE tenant SET name = ?1, kind = ?2, updated_at = ?3 WHERE id = ?4",
                        params![tenant.name, tenant_kind, now, row_id],
                    )?;
                    if tx.changes() == 0 {
                        return Err(tampered(&address));
                    }
                    if resource.desired_digest != desired.digest {
                        updated += 1;
                    }
                    row_id
                } else {
                    tx.execute(
                        "INSERT INTO tenant (name, kind, created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?3)",
                        params![tenant.name, tenant_kind, now],
                    )?;
                    created += 1;
                    tx.last_insert_rowid()
                };
                tenant_ids.insert(tenant.name.as_str(), row_id);
                upsert_managed_resource(&tx, desired, row_id, None, &now)?;
            }

            for tenant in &manifest.tenants {
                let tenant_id = tenant_ids[tenant.name.as_str()];
                for membership in &tenant.memberships {
                    let principal_id = principal_ids[membership.principal.as_str()];
                    let address = format!("membership.{}.{}", tenant.name, membership.principal);
                    let desired = desired_resource(&desired_resources, &address);
                    let existing_resource = existing.get(&address);
                    let role = match membership.role {
                        ManifestTenantRole::Owner => "owner",
                        ManifestTenantRole::Editor => "member",
                        ManifestTenantRole::Viewer => "viewer",
                    };
                    tx.execute(
                        "INSERT INTO membership
                         (tenant_id, principal_id, role, created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?4)
                         ON CONFLICT(tenant_id, principal_id) DO UPDATE SET
                         role = excluded.role, updated_at = excluded.updated_at",
                        params![tenant_id, principal_id, role, now],
                    )?;
                    if let Some(resource) = existing_resource {
                        if resource.desired_digest != desired.digest {
                            updated += 1;
                        }
                    } else {
                        created += 1;
                    }
                    upsert_managed_resource(&tx, desired, tenant_id, Some(principal_id), &now)?;
                }
            }

            let bootstrap_actor = manifest
                .identity
                .github_principals
                .iter()
                .find(|principal| principal.bootstrap)
                .and_then(|principal| principal_ids.get(principal.name.as_str()))
                .copied()
                .expect("validated bootstrap principal");
            for connection in &manifest.connections {
                let address = format!("connection.{}", connection.name);
                let desired = desired_resource(&desired_resources, &address);
                let existed = existing.get(&address);
                let tenant_id = tenant_ids[connection.tenant.as_str()];
                let configuration = connection.provider_configuration()?;
                let configuration_json = serde_json::to_string(&configuration)?;
                let tags_json = serde_json::to_string(&connection.tags)?;
                let (provider_id, engine) = match connection.provider {
                    ManifestProvider::Postgres => ("sift/postgres", "postgres"),
                    ManifestProvider::SqlServer => ("sift/sql-server", "sql_server"),
                };
                let credential_mode = match connection.credential_mode {
                    ManifestCredentialMode::Shared => "shared",
                    ManifestCredentialMode::PerUser => "per_user",
                };
                let shared_handle = match connection.credential.as_deref() {
                    Some(slot) => ready_handle_locked(&tx, slot)?,
                    None => None,
                };
                let policy_json = serde_json::to_string(&runtime_policy(connection))?;
                let row_id = if let Some(resource) = existed {
                    let row_id = required_row_id(resource)?;
                    tx.execute(
                        "UPDATE connection_profile SET tenant_id = ?1, name = ?2,
                         engine = ?3, spec_json = ?4, credential_mode = ?5,
                         shared_secret_handle = ?6, tags_json = ?7, updated_at = ?8,
                         provider_id = ?9, configuration_json = ?4, semantic_engine = ?3,
                         policy_json = ?10 WHERE id = ?11",
                        params![
                            tenant_id,
                            connection.name,
                            engine,
                            configuration_json,
                            credential_mode,
                            shared_handle,
                            tags_json,
                            now,
                            provider_id,
                            policy_json,
                            row_id
                        ],
                    )?;
                    if tx.changes() == 0 {
                        return Err(tampered(&address));
                    }
                    if resource.desired_digest != desired.digest {
                        updated += 1;
                    }
                    row_id
                } else {
                    tx.execute(
                        "INSERT INTO connection_profile
                         (tenant_id, name, engine, spec_json, credential_mode,
                          shared_secret_handle, tags_json, created_by, created_at, updated_at,
                          provider_id, configuration_json, semantic_engine, policy_json)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, ?10, ?4, ?3, ?11)",
                        params![
                            tenant_id,
                            connection.name,
                            engine,
                            configuration_json,
                            credential_mode,
                            shared_handle,
                            tags_json,
                            bootstrap_actor,
                            now,
                            provider_id,
                            policy_json
                        ],
                    )?;
                    created += 1;
                    tx.last_insert_rowid()
                };
                upsert_managed_resource(&tx, desired, row_id, None, &now)?;
            }

            // Parents are removed only after desired connections and
            // memberships have moved to their new parents. This makes tenant
            // renames/moves deterministic without weakening foreign keys.
            for resource in removals
                .iter()
                .filter(|resource| deletion_rank(&resource.kind) > 1)
            {
                delete_managed_resource(&tx, resource)?;
                tx.execute(
                    "DELETE FROM instance_managed_resource WHERE address = ?1",
                    params![resource.address],
                )?;
                deleted += 1;
            }

            let admin_count: i64 = tx.query_row(
                "SELECT COUNT(*) FROM principal WHERE is_instance_admin = 1 AND disabled_at IS NULL",
                [],
                |row| row.get(0),
            )?;
            if admin_count == 0 {
                return Err(MetadataError::InstanceManifestConflict(
                    "apply would leave the instance without an administrator".into(),
                ));
            }

            tx.execute(
                "UPDATE instance_managed_resource SET manifest_id = ?1",
                params![manifest.manifest_id.to_string()],
            )?;

            tx.execute(
                "INSERT INTO instance_manifest_state
                 (singleton, manifest_id, configuration_digest, lock_digest, generation, applied_at)
                 VALUES (1, ?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(singleton) DO UPDATE SET manifest_id = excluded.manifest_id,
                 configuration_digest = excluded.configuration_digest,
                 lock_digest = excluded.lock_digest, generation = excluded.generation,
                 applied_at = excluded.applied_at",
                params![
                    manifest.manifest_id.to_string(),
                    configuration_digest,
                    lock_digest,
                    i64::try_from(generation).map_err(|_| {
                        MetadataError::InstanceManifestConflict(
                            "generation exceeds SQLite integer range".into(),
                        )
                    })?,
                    now
                ],
            )?;
            insert_operation_audit_row(
                &tx,
                &NewOperationAudit {
                    actor_principal_id: None,
                    action: "apply".into(),
                    target: "instance_manifest".into(),
                    target_id: None,
                    status: "succeeded".into(),
                    result_code: None,
                    row_count: Some(i64::try_from(created + updated + deleted).unwrap_or(i64::MAX)),
                    error_message: None,
                    correlation_id: None,
                },
            )?;
            let missing_credentials = missing_slots_locked(&tx)?;
            tx.commit()?;
            InstanceApplySummary {
                changed: configuration_changed || created != 0 || updated != 0 || deleted != 0,
                created,
                updated,
                deleted,
                missing_credentials,
            }
        };

        for handle in stale_secret_handles {
            if let Err(error) = self
                .secrets
                .delete(INSTANCE_SECRET_NAMESPACE, &handle)
                .await
            {
                tracing::warn!(%error, "orphaned invalidated instance credential");
            }
        }
        Ok(summary)
    }

    pub fn instance_credential_status(&self) -> crate::Result<Vec<InstanceCredentialStatus>> {
        let conn = self.conn()?;
        let mut statement = conn.prepare(
            "SELECT slot_id, credential_kind, readiness
             FROM instance_credential_slot ORDER BY slot_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut statuses = Vec::new();
        for row in rows {
            let (slot, kind, readiness) = row?;
            let mut consumers_statement = conn.prepare(
                "SELECT resource_address FROM instance_credential_consumer
                 WHERE slot_id = ?1 ORDER BY resource_address",
            )?;
            let consumers = consumers_statement
                .query_map(params![slot], |row| row.get(0))?
                .collect::<std::result::Result<Vec<String>, _>>()?;
            statuses.push(InstanceCredentialStatus {
                slot,
                kind: parse_credential_kind(&kind)?,
                readiness: parse_readiness(&readiness)?,
                consumers,
            });
        }
        Ok(statuses)
    }

    /// Cross-check persisted readiness against the destination secret store.
    /// A lost/corrupt opaque handle is reported as invalid without exposing
    /// the handle or secret bytes.
    pub async fn verified_instance_credential_status(
        &self,
    ) -> crate::Result<Vec<InstanceCredentialStatus>> {
        let mut statuses = self.instance_credential_status()?;
        for status in &mut statuses {
            if status.readiness != CredentialReadiness::Ready {
                continue;
            }
            let handle = {
                let conn = self.conn()?;
                conn.query_row(
                    "SELECT secret_handle FROM instance_credential_slot WHERE slot_id = ?1",
                    params![status.slot],
                    |row| row.get::<_, Option<String>>(0),
                )?
            };
            let valid = if let Some(handle) = handle {
                match self.secrets.get(INSTANCE_SECRET_NAMESPACE, &handle).await? {
                    Some(bytes) => serde_json::from_slice::<serde_json::Value>(&bytes)
                        .ok()
                        .is_some_and(|value| {
                            validate_credential_value(&status.slot, status.kind, &value).is_ok()
                        }),
                    None => false,
                }
            } else {
                false
            };
            if !valid {
                status.readiness = CredentialReadiness::Invalid;
            }
        }
        Ok(statuses)
    }

    /// Import one typed credential. Only a compact, exact JSON shape is
    /// accepted, and the bytes are written directly to SecretStore.
    pub async fn import_instance_credential(
        &self,
        slot: &str,
        value: &serde_json::Value,
    ) -> crate::Result<()> {
        let (kind, expected_digest, old_handle) = {
            let conn = self.conn()?;
            conn.query_row(
                "SELECT credential_kind, consumer_digest, secret_handle
                 FROM instance_credential_slot WHERE slot_id = ?1",
                params![slot],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| MetadataError::InstanceCredentialSlotNotFound(slot.into()))?
        };
        validate_credential_value(slot, parse_credential_kind(&kind)?, value)?;
        let bytes = serde_json::to_vec(value)?;
        let new_handle = Uuid::new_v4().to_string();
        self.secrets
            .put(INSTANCE_SECRET_NAMESPACE, &new_handle, &bytes)
            .await?;

        let update = (|| -> crate::Result<()> {
            let mut conn = self.conn()?;
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let current_digest: Option<String> = tx
                .query_row(
                    "SELECT consumer_digest FROM instance_credential_slot WHERE slot_id = ?1",
                    params![slot],
                    |row| row.get(0),
                )
                .optional()?;
            if current_digest.as_deref() != Some(expected_digest.as_str()) {
                return Err(MetadataError::InstanceManifestConflict(
                    "credential consumer changed during import; retry against the new generation"
                        .into(),
                ));
            }
            let now = now_text();
            tx.execute(
                "UPDATE instance_credential_slot SET secret_handle = ?1,
                 readiness = 'ready', updated_at = ?2 WHERE slot_id = ?3",
                params![new_handle, now, slot],
            )?;
            tx.execute(
                "UPDATE connection_profile SET shared_secret_handle = ?1, updated_at = ?2
                 WHERE id IN (
                    SELECT r.row_id FROM instance_credential_consumer c
                    JOIN instance_managed_resource r ON r.address = c.resource_address
                    WHERE c.slot_id = ?3 AND r.resource_kind = 'connection'
                 )",
                params![new_handle, now, slot],
            )?;
            insert_operation_audit_row(
                &tx,
                &NewOperationAudit {
                    actor_principal_id: None,
                    action: "import_credential".into(),
                    target: "instance_credential_slot".into(),
                    target_id: None,
                    status: "succeeded".into(),
                    result_code: None,
                    row_count: Some(1),
                    error_message: None,
                    correlation_id: None,
                },
            )?;
            tx.commit()?;
            Ok(())
        })();
        if let Err(error) = update {
            let _ = self
                .secrets
                .delete(INSTANCE_SECRET_NAMESPACE, &new_handle)
                .await;
            return Err(error);
        }
        if let Some(old_handle) = old_handle {
            if old_handle != new_handle {
                let _ = self
                    .secrets
                    .delete(INSTANCE_SECRET_NAMESPACE, &old_handle)
                    .await;
            }
        }
        Ok(())
    }

    pub async fn instance_credential_value(
        &self,
        slot: &str,
    ) -> crate::Result<Option<serde_json::Value>> {
        let handle = {
            let conn = self.conn()?;
            conn.query_row(
                "SELECT secret_handle FROM instance_credential_slot
                 WHERE slot_id = ?1 AND readiness = 'ready'",
                params![slot],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten()
        };
        let Some(handle) = handle else {
            return Ok(None);
        };
        let Some(bytes) = self.secrets.get(INSTANCE_SECRET_NAMESPACE, &handle).await? else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_slice(&bytes)?))
    }
}

fn action_rank(action: InstanceResourceAction) -> u8 {
    match action {
        InstanceResourceAction::Delete => 0,
        InstanceResourceAction::Update => 1,
        InstanceResourceAction::Create => 2,
        InstanceResourceAction::Unchanged => 3,
    }
}

fn desired_resources(manifest: &Manifest) -> crate::Result<Vec<DesiredResource>> {
    let mut resources = Vec::new();
    for principal in &manifest.identity.github_principals {
        resources.push(resource(
            format!("principal.{}", principal.name),
            "principal",
            principal,
            false,
        )?);
    }
    for tenant in &manifest.tenants {
        resources.push(resource(
            format!("tenant.{}", tenant.name),
            "tenant",
            tenant,
            false,
        )?);
        for membership in &tenant.memberships {
            resources.push(resource(
                format!("membership.{}.{}", tenant.name, membership.principal),
                "membership",
                membership,
                false,
            )?);
        }
    }
    for connection in &manifest.connections {
        resources.push(resource(
            format!("connection.{}", connection.name),
            "connection",
            connection,
            connection.lifecycle.prevent_destroy,
        )?);
    }
    Ok(resources)
}

fn resource<T: Serialize>(
    address: String,
    kind: &'static str,
    value: &T,
    prevent_destroy: bool,
) -> crate::Result<DesiredResource> {
    Ok(DesiredResource {
        address,
        kind,
        digest: digest(&serde_json::to_vec(value)?),
        prevent_destroy,
    })
}

fn desired_slots(manifest: &Manifest) -> crate::Result<BTreeMap<String, DesiredSlot>> {
    let mut raw = BTreeMap::<String, (CredentialKind, Vec<(String, String)>)>::new();
    if let Some(slot) = &manifest.auth.github.client_secret {
        let consumer = "auth.github".to_string();
        let value = serde_json::json!({
            "flow": manifest.auth.github.flow,
            "client_id": manifest.auth.github.client_id,
            "public_base_url": manifest.server.public_base_url,
        });
        raw.entry(slot.clone())
            .or_insert_with(|| (CredentialKind::GithubOauthClientSecret, Vec::new()))
            .1
            .push((consumer, digest(&serde_json::to_vec(&value)?)));
    }
    for connection in &manifest.connections {
        if let Some(slot) = &connection.credential {
            let kind = match connection.provider {
                ManifestProvider::Postgres => CredentialKind::Postgres,
                ManifestProvider::SqlServer => CredentialKind::SqlServer,
            };
            let value = serde_json::json!({
                "provider": connection.provider,
                "configuration": connection.provider_configuration()?,
            });
            let entry = raw
                .entry(slot.clone())
                .or_insert_with(|| (kind, Vec::new()));
            if entry.0 != kind {
                return Err(MetadataError::InstanceManifestConflict(format!(
                    "credential slot `{slot}` is reused across incompatible providers"
                )));
            }
            entry.1.push((
                format!("connection.{}", connection.name),
                digest(&serde_json::to_vec(&value)?),
            ));
        }
    }
    raw.into_iter()
        .map(|(slot, (kind, mut consumers))| {
            consumers.sort();
            let consumer_digest = digest(&serde_json::to_vec(&consumers)?);
            Ok((
                slot,
                DesiredSlot {
                    kind,
                    consumer_digest,
                    consumers: consumers.into_iter().map(|(address, _)| address).collect(),
                },
            ))
        })
        .collect()
}

fn refuse_unmanaged_adoption(tx: &Transaction<'_>) -> crate::Result<()> {
    for table in ["principal", "tenant", "connection_profile"] {
        let sql = format!("SELECT COUNT(*) FROM {table}");
        let count: i64 = tx.query_row(&sql, [], |row| row.get(0))?;
        if count != 0 {
            return Err(MetadataError::InstanceManifestConflict(format!(
                "refusing to adopt existing unmanaged rows from `{table}`; use a fresh state directory"
            )));
        }
    }
    Ok(())
}

fn managed_resources_locked(
    tx: &Transaction<'_>,
) -> crate::Result<BTreeMap<String, ManagedResource>> {
    let mut statement = tx.prepare(
        "SELECT address, resource_kind, row_id, secondary_row_id, desired_digest, prevent_destroy
         FROM instance_managed_resource ORDER BY address",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(ManagedResource {
            address: row.get(0)?,
            kind: row.get(1)?,
            row_id: row.get(2)?,
            secondary_row_id: row.get(3)?,
            desired_digest: row.get(4)?,
            prevent_destroy: row.get(5)?,
        })
    })?;
    let resources = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(resources
        .into_iter()
        .map(|resource| (resource.address.clone(), resource))
        .collect())
}

fn required_row_id(resource: &ManagedResource) -> crate::Result<i64> {
    resource.row_id.ok_or_else(|| tampered(&resource.address))
}

fn tampered(address: &str) -> MetadataError {
    MetadataError::InstanceManifestConflict(format!(
        "managed resource `{address}` is missing or was modified outside apply"
    ))
}

fn desired_resource<'a>(resources: &'a [DesiredResource], address: &str) -> &'a DesiredResource {
    resources
        .iter()
        .find(|item| item.address == address)
        .expect("validated desired resource")
}

fn upsert_managed_resource(
    tx: &Transaction<'_>,
    resource: &DesiredResource,
    row_id: i64,
    secondary_row_id: Option<i64>,
    now: &str,
) -> crate::Result<()> {
    tx.execute(
        "INSERT INTO instance_managed_resource
         (address, manifest_id, resource_kind, row_id, secondary_row_id, desired_digest,
          prevent_destroy, created_at, updated_at)
         VALUES (?1, '', ?2, ?3, ?4, ?5, ?6, ?7, ?7)
         ON CONFLICT(address) DO UPDATE SET resource_kind = excluded.resource_kind,
         row_id = excluded.row_id, secondary_row_id = excluded.secondary_row_id,
         desired_digest = excluded.desired_digest, prevent_destroy = excluded.prevent_destroy,
         updated_at = excluded.updated_at",
        params![
            resource.address,
            resource.kind,
            row_id,
            secondary_row_id,
            resource.digest,
            resource.prevent_destroy,
            now
        ],
    )?;
    Ok(())
}

fn deletion_rank(kind: &str) -> u8 {
    match kind {
        "connection" => 0,
        "membership" => 1,
        "tenant" => 2,
        "principal" => 3,
        _ => 4,
    }
}

fn delete_managed_resource(tx: &Transaction<'_>, resource: &ManagedResource) -> crate::Result<()> {
    let row_id = required_row_id(resource)?;
    let deleted = match resource.kind.as_str() {
        "connection" => tx.execute(
            "DELETE FROM connection_profile WHERE id = ?1",
            params![row_id],
        )?,
        "membership" => tx.execute(
            "DELETE FROM membership WHERE tenant_id = ?1 AND principal_id = ?2",
            params![
                row_id,
                resource
                    .secondary_row_id
                    .ok_or_else(|| tampered(&resource.address))?
            ],
        )?,
        "tenant" => tx.execute("DELETE FROM tenant WHERE id = ?1", params![row_id])?,
        "principal" => tx.execute("DELETE FROM principal WHERE id = ?1", params![row_id])?,
        _ => return Err(tampered(&resource.address)),
    };
    if deleted == 0 {
        return Err(tampered(&resource.address));
    }
    Ok(())
}

fn reconcile_slots_locked(
    tx: &Transaction<'_>,
    desired: &BTreeMap<String, DesiredSlot>,
    now: &str,
    stale_handles: &mut Vec<String>,
) -> crate::Result<()> {
    let mut existing_statement = tx.prepare(
        "SELECT slot_id, credential_kind, consumer_digest, secret_handle, readiness
         FROM instance_credential_slot",
    )?;
    let existing = existing_statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(existing_statement);

    for (slot, _, _, handle, _) in &existing {
        if !desired.contains_key(slot) {
            if let Some(handle) = handle {
                stale_handles.push(handle.clone());
            }
            tx.execute(
                "DELETE FROM instance_credential_slot WHERE slot_id = ?1",
                params![slot],
            )?;
        }
    }
    for (slot, desired_slot) in desired {
        let previous = existing.iter().find(|entry| &entry.0 == slot);
        let unchanged = previous.is_some_and(|entry| {
            entry.1 == credential_kind_text(desired_slot.kind)
                && entry.2 == desired_slot.consumer_digest
        });
        let (handle, readiness) = if unchanged {
            (
                previous.and_then(|entry| entry.3.clone()),
                previous.map_or("missing", |entry| entry.4.as_str()),
            )
        } else {
            if let Some(handle) = previous.and_then(|entry| entry.3.clone()) {
                stale_handles.push(handle);
            }
            (
                None,
                if previous.is_some() {
                    "invalid"
                } else {
                    "missing"
                },
            )
        };
        tx.execute(
            "INSERT INTO instance_credential_slot
             (slot_id, credential_kind, consumer_digest, secret_handle, readiness, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
             ON CONFLICT(slot_id) DO UPDATE SET credential_kind = excluded.credential_kind,
             consumer_digest = excluded.consumer_digest, secret_handle = excluded.secret_handle,
             readiness = excluded.readiness, updated_at = excluded.updated_at",
            params![slot, credential_kind_text(desired_slot.kind), desired_slot.consumer_digest,
                handle, readiness, now],
        )?;
        tx.execute(
            "DELETE FROM instance_credential_consumer WHERE slot_id = ?1",
            params![slot],
        )?;
        for consumer in &desired_slot.consumers {
            tx.execute(
                "INSERT INTO instance_credential_consumer
                 (slot_id, resource_address, consumer_digest) VALUES (?1, ?2, ?3)",
                params![slot, consumer, desired_slot.consumer_digest],
            )?;
        }
    }
    Ok(())
}

fn ready_handle_locked(tx: &Transaction<'_>, slot: &str) -> crate::Result<Option<String>> {
    tx.query_row(
        "SELECT secret_handle FROM instance_credential_slot
         WHERE slot_id = ?1 AND readiness = 'ready'",
        params![slot],
        |row| row.get(0),
    )
    .optional()
    .map(|value| value.flatten())
    .map_err(Into::into)
}

fn missing_slots_locked(tx: &Transaction<'_>) -> crate::Result<Vec<String>> {
    let mut statement = tx.prepare(
        "SELECT slot_id FROM instance_credential_slot
         WHERE readiness != 'ready' ORDER BY slot_id",
    )?;
    let slots = statement
        .query_map([], |row| row.get(0))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(MetadataError::from)?;
    Ok(slots)
}

fn runtime_policy(connection: &sift_instance_config::ConnectionConfig) -> ConnectionPolicy {
    let mut blocked = Vec::new();
    if !connection.policy.allow_sql {
        blocked.extend([
            OperationKind::ExecuteQuery,
            OperationKind::PreviewEdits,
            OperationKind::ApplyEdits,
            OperationKind::ImportCsv,
            OperationKind::BulkInsert,
            OperationKind::PreviewMigration,
            OperationKind::ApplyMigration,
            OperationKind::ExecuteRun,
            OperationKind::ExecuteTransferRecipe,
        ]);
    }
    if !connection.policy.allow_schema_read {
        blocked.extend([
            OperationKind::RefreshSchema,
            OperationKind::ReadCatalogGraph,
            OperationKind::ProjectCatalogDiagram,
            OperationKind::SearchSchema,
            OperationKind::CreateCatalogSnapshot,
            OperationKind::ListCatalogSnapshots,
            OperationKind::GetCatalogSnapshot,
            OperationKind::CompareCatalogSchemas,
        ]);
    }
    if !connection.policy.allow_export {
        blocked.push(OperationKind::ExportQuery);
    }
    blocked.dedup();
    ConnectionPolicy {
        read_only: !connection.policy.allow_sql,
        blocked_ops: blocked,
        ..ConnectionPolicy::default()
    }
}

fn credential_kind_text(kind: CredentialKind) -> &'static str {
    match kind {
        CredentialKind::GithubOauthClientSecret => "github-oauth-client-secret",
        CredentialKind::Postgres => "postgres",
        CredentialKind::SqlServer => "sql-server",
    }
}

fn parse_credential_kind(value: &str) -> crate::Result<CredentialKind> {
    match value {
        "github-oauth-client-secret" => Ok(CredentialKind::GithubOauthClientSecret),
        "postgres" => Ok(CredentialKind::Postgres),
        "sql-server" => Ok(CredentialKind::SqlServer),
        _ => Err(MetadataError::InstanceManifestConflict(
            "credential slot has an unknown kind".into(),
        )),
    }
}

fn parse_readiness(value: &str) -> crate::Result<CredentialReadiness> {
    match value {
        "missing" => Ok(CredentialReadiness::Missing),
        "ready" => Ok(CredentialReadiness::Ready),
        "invalid" => Ok(CredentialReadiness::Invalid),
        _ => Err(MetadataError::InstanceManifestConflict(
            "credential slot has an unknown readiness value".into(),
        )),
    }
}

fn validate_credential_value(
    slot: &str,
    kind: CredentialKind,
    value: &serde_json::Value,
) -> crate::Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| MetadataError::InstanceCredentialInvalid {
            slot: slot.into(),
            message: "must be a JSON object".into(),
        })?;
    let field = match kind {
        CredentialKind::GithubOauthClientSecret => "client_secret",
        CredentialKind::Postgres | CredentialKind::SqlServer => "password",
    };
    if object.len() != 1 {
        return Err(MetadataError::InstanceCredentialInvalid {
            slot: slot.into(),
            message: format!("must contain exactly the `{field}` field"),
        });
    }
    let secret = object
        .get(field)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if secret.is_empty() || secret.len() > 16 * 1024 {
        return Err(MetadataError::InstanceCredentialInvalid {
            slot: slot.into(),
            message: format!("`{field}` must be a non-empty bounded string"),
        });
    }
    Ok(())
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{MemorySecretStore, PrincipalId, TenantId};

    fn fixture() -> (Manifest, LockFile) {
        let manifest = Manifest::parse(include_str!(
            "../../../examples/reproducible-instance/sift.toml"
        ))
        .unwrap();
        let lock = LockFile::parse(include_str!(
            "../../../examples/reproducible-instance/sift.lock"
        ))
        .unwrap();
        (manifest, lock)
    }

    #[tokio::test]
    async fn applies_resources_and_imports_typed_secret() {
        let secrets = Arc::new(MemorySecretStore::new());
        let store = MetadataStore::open_in_memory(secrets.clone()).unwrap();
        let (manifest, lock) = fixture();
        let initial_plan = store.plan_instance_manifest(&manifest).unwrap();
        assert!(initial_plan
            .iter()
            .all(|change| change.action == InstanceResourceAction::Create));
        let first = store
            .apply_instance_manifest(&manifest, &lock, 1, false)
            .await
            .unwrap();
        assert!(first.changed);
        assert_eq!(first.missing_credentials.len(), 1);
        store
            .import_instance_credential(
                "credential:demo/postgres/shared",
                &serde_json::json!({"password": "portable-but-not-in-config"}),
            )
            .await
            .unwrap();
        assert_eq!(
            store.instance_credential_status().unwrap()[0].readiness,
            CredentialReadiness::Ready
        );
        let managed_profile = store
            .list_connection_profiles(TenantId(1))
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let (_, credentials) = store
            .resolve_provider_connection(TenantId(1), PrincipalId(1), managed_profile.id)
            .await
            .unwrap();
        assert_eq!(
            credentials.get("password").map(Vec::as_slice),
            Some(b"portable-but-not-in-config".as_slice())
        );
        let sqlite_dump = {
            let conn = store.conn().unwrap();
            let mut statement = conn
                .prepare("SELECT configuration_json FROM connection_profile")
                .unwrap();
            statement
                .query_row([], |row| row.get::<_, String>(0))
                .unwrap()
        };
        assert!(!sqlite_dump.contains("portable-but-not-in-config"));
        assert!(!secrets.is_empty());

        let second = store
            .apply_instance_manifest(&manifest, &lock, 1, false)
            .await
            .unwrap();
        assert!(!second.changed);
        assert!(second.missing_credentials.is_empty());
        assert!(store
            .plan_instance_manifest(&manifest)
            .unwrap()
            .iter()
            .all(|change| change.action == InstanceResourceAction::Unchanged));
        assert!(matches!(
            store
                .delete_connection_profile(
                    TenantId(1),
                    PrincipalId(1),
                    managed_profile.id,
                    NewOperationAudit {
                        actor_principal_id: Some(PrincipalId(1)),
                        action: "delete".into(),
                        target: "connection_profile".into(),
                        target_id: Some(managed_profile.id.0),
                        status: "succeeded".into(),
                        result_code: None,
                        row_count: None,
                        error_message: None,
                        correlation_id: None,
                    },
                )
                .await,
            Err(MetadataError::ConnectionProfileManaged(_))
        ));
        store
            .verify_instance_manifest_state(&manifest, &lock, 1)
            .unwrap();
        assert!(store
            .verify_instance_manifest_state(&manifest, &lock, 2)
            .is_err());
    }

    #[tokio::test]
    async fn changed_consumer_invalidates_secret() {
        let store = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        let (mut manifest, lock) = fixture();
        store
            .apply_instance_manifest(&manifest, &lock, 1, false)
            .await
            .unwrap();
        store
            .import_instance_credential(
                "credential:demo/postgres/shared",
                &serde_json::json!({"password": "test-only"}),
            )
            .await
            .unwrap();
        manifest.connections[0].connection_string =
            "postgresql://sift@other.internal:5432/analytics?sslmode=verify-full".into();
        let lock = LockFile::generate(&manifest, env!("CARGO_PKG_VERSION"), 1).unwrap();
        let summary = store
            .apply_instance_manifest(&manifest, &lock, 2, false)
            .await
            .unwrap();
        assert_eq!(summary.missing_credentials.len(), 1);
        assert_eq!(
            store.instance_credential_status().unwrap()[0].readiness,
            CredentialReadiness::Invalid
        );
    }

    #[tokio::test]
    async fn deletion_requires_approval_and_honors_prevent_destroy() {
        let store = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        let (manifest, lock) = fixture();
        store
            .apply_instance_manifest(&manifest, &lock, 1, false)
            .await
            .unwrap();

        let mut without_connection = manifest.clone();
        without_connection.connections.clear();
        let next_lock =
            LockFile::generate(&without_connection, env!("CARGO_PKG_VERSION"), 1).unwrap();
        assert!(matches!(
            store
                .apply_instance_manifest(&without_connection, &next_lock, 2, false)
                .await,
            Err(MetadataError::InstanceDestroyApprovalRequired(_))
        ));
        assert!(matches!(
            store
                .apply_instance_manifest(&without_connection, &next_lock, 2, true)
                .await,
            Err(MetadataError::InstancePreventDestroy(_))
        ));

        let mut destroyable = manifest;
        destroyable.connections[0].lifecycle.prevent_destroy = false;
        let destroyable_lock =
            LockFile::generate(&destroyable, env!("CARGO_PKG_VERSION"), 1).unwrap();
        store
            .apply_instance_manifest(&destroyable, &destroyable_lock, 2, false)
            .await
            .unwrap();
        let mut empty = destroyable;
        empty.connections.clear();
        let empty_lock = LockFile::generate(&empty, env!("CARGO_PKG_VERSION"), 1).unwrap();
        let result = store
            .apply_instance_manifest(&empty, &empty_lock, 3, true)
            .await
            .unwrap();
        assert_eq!(result.deleted, 1);
    }
}
