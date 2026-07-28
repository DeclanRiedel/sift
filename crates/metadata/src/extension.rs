use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sift_protocol::{
    ExtensionIsolation, ExtensionLifecycleState, ExtensionProvenance, HostCapabilityKind,
};

use super::{now_text, MetadataError, MetadataStore, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionPublisherKey {
    pub publisher: String,
    pub fingerprint: String,
    pub public_key: [u8; 32],
    pub valid_from: String,
    pub valid_until: Option<String>,
    pub revoked_at: Option<String>,
    pub revision: u64,
}

#[derive(Debug, Clone)]
pub struct NewExtensionPackage {
    pub archive_sha256: String,
    pub extension_id: String,
    pub version: String,
    pub manifest_sha256: String,
    pub manifest_json: String,
    pub provenance: ExtensionProvenance,
}

#[derive(Debug, Clone)]
pub struct NewExtensionContribution {
    pub contribution_id: String,
    pub kind: String,
    pub local_id: String,
    pub descriptor_json: String,
}

#[derive(Debug, Clone)]
pub struct SelectedExtensionPackage {
    pub selection: ExtensionSelection,
    pub version: String,
    pub manifest_sha256: String,
    pub manifest_json: String,
    pub provenance: ExtensionProvenance,
}

#[derive(Debug, Clone)]
pub struct StoredExtensionContribution {
    pub contribution_id: String,
    pub kind: String,
    pub local_id: String,
    pub descriptor_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionSelection {
    pub extension_id: String,
    pub selected_archive_sha256: String,
    pub enabled: bool,
    pub lifecycle: ExtensionLifecycleState,
    pub isolation: ExtensionIsolation,
    pub quarantine_reason: Option<String>,
    pub revision: u64,
}

#[derive(Debug, Clone)]
pub struct ReplaceExtensionGrants {
    pub extension_id: String,
    pub grants: Vec<(HostCapabilityKind, String)>,
    pub expected_revision: u64,
}

#[derive(Debug, Clone)]
pub struct UpdateExtensionSelection<'a> {
    pub extension_id: &'a str,
    pub selected_archive_sha256: Option<&'a str>,
    pub enabled: bool,
    pub lifecycle: ExtensionLifecycleState,
    pub isolation: ExtensionIsolation,
    pub quarantine_reason: Option<&'a str>,
    pub expected_revision: u64,
}

impl MetadataStore {
    pub fn put_extension_publisher_key(
        &self,
        key: &ExtensionPublisherKey,
        expected_revision: Option<u64>,
    ) -> Result<ExtensionPublisherKey> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<i64> = tx
            .query_row(
                "SELECT revision FROM extension_publisher_key
                 WHERE publisher = ?1 AND fingerprint = ?2",
                params![key.publisher, key.fingerprint],
                |row| row.get(0),
            )
            .optional()?;
        let current_revision = current
            .map(|value| {
                u64::try_from(value).map_err(|_| MetadataError::ExtensionRevisionConflict {
                    expected: expected_revision.unwrap_or(0),
                    current: 0,
                })
            })
            .transpose()?;
        if current_revision != expected_revision {
            return Err(MetadataError::ExtensionRevisionConflict {
                expected: expected_revision.unwrap_or(0),
                current: current_revision.unwrap_or(0),
            });
        }
        let revision = current_revision.map_or(0, |value| value.saturating_add(1));
        tx.execute(
            "INSERT INTO extension_publisher_key
             (publisher, fingerprint, public_key, valid_from, valid_until, revoked_at, revision)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(publisher, fingerprint) DO UPDATE SET
               public_key = excluded.public_key,
               valid_from = excluded.valid_from,
               valid_until = excluded.valid_until,
               revoked_at = excluded.revoked_at,
               revision = excluded.revision",
            params![
                key.publisher,
                key.fingerprint,
                key.public_key.as_slice(),
                key.valid_from,
                key.valid_until,
                key.revoked_at,
                i64::try_from(revision).map_err(|_| {
                    MetadataError::ExtensionRevisionConflict {
                        expected: expected_revision.unwrap_or(0),
                        current: current_revision.unwrap_or(0),
                    }
                })?,
            ],
        )?;
        tx.commit()?;
        drop(conn);
        self.extension_publisher_key(&key.publisher, &key.fingerprint)
    }

    pub fn extension_publisher_key(
        &self,
        publisher: &str,
        fingerprint: &str,
    ) -> Result<ExtensionPublisherKey> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT publisher, fingerprint, public_key, valid_from, valid_until,
                    revoked_at, revision
             FROM extension_publisher_key
             WHERE publisher = ?1 AND fingerprint = ?2",
            params![publisher, fingerprint],
            publisher_key_from_row,
        )
        .optional()?
        .ok_or_else(|| MetadataError::ExtensionNotFound(format!("{publisher}/{fingerprint}")))
    }

    pub fn active_extension_publisher_keys(
        &self,
        publisher: &str,
    ) -> Result<Vec<ExtensionPublisherKey>> {
        let conn = self.conn()?;
        let at = now_text();
        let mut statement = conn.prepare(
            "SELECT publisher, fingerprint, public_key, valid_from, valid_until,
                    revoked_at, revision
             FROM extension_publisher_key
             WHERE publisher = ?1
               AND revoked_at IS NULL
               AND valid_from <= ?2
               AND (valid_until IS NULL OR valid_until > ?2)
             ORDER BY fingerprint",
        )?;
        let keys = statement
            .query_map(params![publisher, at], publisher_key_from_row)?
            .collect::<std::result::Result<_, _>>()
            .map_err(MetadataError::from)?;
        Ok(keys)
    }

    pub fn revoke_extension_publisher_key(
        &self,
        publisher: &str,
        fingerprint: &str,
        expected_revision: u64,
    ) -> Result<ExtensionPublisherKey> {
        let revision =
            expected_revision
                .checked_add(1)
                .ok_or(MetadataError::ExtensionRevisionConflict {
                    expected: expected_revision,
                    current: expected_revision,
                })?;
        let changed = self.conn()?.execute(
            "UPDATE extension_publisher_key
             SET revoked_at = COALESCE(revoked_at, ?3), revision = ?4
             WHERE publisher = ?1 AND fingerprint = ?2 AND revision = ?5",
            params![
                publisher,
                fingerprint,
                now_text(),
                i64::try_from(revision).map_err(|_| {
                    MetadataError::ExtensionRevisionConflict {
                        expected: expected_revision,
                        current: expected_revision,
                    }
                })?,
                i64::try_from(expected_revision).map_err(|_| {
                    MetadataError::ExtensionRevisionConflict {
                        expected: expected_revision,
                        current: expected_revision,
                    }
                })?,
            ],
        )?;
        if changed == 0 {
            let current = self.extension_publisher_key(publisher, fingerprint)?;
            return Err(MetadataError::ExtensionRevisionConflict {
                expected: expected_revision,
                current: current.revision,
            });
        }
        self.extension_publisher_key(publisher, fingerprint)
    }

    pub fn selected_extension_packages(&self) -> Result<Vec<SelectedExtensionPackage>> {
        let selections = self.list_extension_selections()?;
        selections
            .into_iter()
            .map(|selection| self.selected_extension_package_for(selection))
            .collect()
    }

    pub fn selected_extension_package(
        &self,
        extension_id: &str,
    ) -> Result<SelectedExtensionPackage> {
        let selection = self.extension_selection(extension_id)?;
        self.selected_extension_package_for(selection)
    }

    fn selected_extension_package_for(
        &self,
        selection: ExtensionSelection,
    ) -> Result<SelectedExtensionPackage> {
        let conn = self.conn()?;
        let package = conn.query_row(
            "SELECT version, manifest_sha256, manifest_json, provenance
             FROM extension_package WHERE archive_sha256 = ?1",
            [&selection.selected_archive_sha256],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )?;
        Ok(SelectedExtensionPackage {
            selection,
            version: package.0,
            manifest_sha256: package.1,
            manifest_json: package.2,
            provenance: parse_provenance(package.3)?,
        })
    }

    pub fn extension_contributions(
        &self,
        archive_sha256: &str,
    ) -> Result<Vec<StoredExtensionContribution>> {
        let conn = self.conn()?;
        let mut statement = conn.prepare(
            "SELECT contribution_id, kind, local_id, descriptor_json
             FROM extension_contribution WHERE archive_sha256 = ?1
             ORDER BY contribution_id",
        )?;
        let contributions: Vec<StoredExtensionContribution> = statement
            .query_map([archive_sha256], |row| {
                Ok(StoredExtensionContribution {
                    contribution_id: row.get(0)?,
                    kind: row.get(1)?,
                    local_id: row.get(2)?,
                    descriptor_json: row.get(3)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        Ok(contributions)
    }

    pub fn record_extension_package(
        &self,
        package: &NewExtensionPackage,
        contributions: &[NewExtensionContribution],
    ) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = tx
            .query_row(
                "SELECT archive_sha256 FROM extension_package
                 WHERE extension_id = ?1 AND version = ?2",
                params![package.extension_id, package.version],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if existing
            .as_deref()
            .is_some_and(|digest| digest != package.archive_sha256)
        {
            return Err(MetadataError::ExtensionVersionDigestConflict {
                extension_id: package.extension_id.clone(),
                version: package.version.clone(),
            });
        }

        tx.execute(
            "INSERT OR IGNORE INTO extension_package
             (archive_sha256, extension_id, version, manifest_sha256, manifest_json,
              provenance, installed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                package.archive_sha256,
                package.extension_id,
                package.version,
                package.manifest_sha256,
                package.manifest_json,
                provenance_text(package.provenance),
                now_text(),
            ],
        )?;
        for contribution in contributions {
            let existing: Option<String> = tx
                .query_row(
                    "SELECT archive_sha256 FROM extension_contribution
                     WHERE contribution_id = ?1",
                    [&contribution.contribution_id],
                    |row| row.get(0),
                )
                .optional()?;
            if existing
                .as_deref()
                .is_some_and(|digest| digest != package.archive_sha256)
            {
                return Err(MetadataError::ExtensionContributionConflict(
                    contribution.contribution_id.clone(),
                ));
            }
            tx.execute(
                "INSERT OR IGNORE INTO extension_contribution
                 (contribution_id, archive_sha256, extension_id, kind, local_id,
                  descriptor_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    contribution.contribution_id,
                    package.archive_sha256,
                    package.extension_id,
                    contribution.kind,
                    contribution.local_id,
                    contribution.descriptor_json,
                ],
            )?;
        }
        tx.execute(
            "INSERT OR IGNORE INTO extension_selection
             (extension_id, selected_archive_sha256, enabled, lifecycle_state,
              isolation, revision, updated_at)
             VALUES (?1, ?2, 0, 'disabled', 'process_only', 0, ?3)",
            params![package.extension_id, package.archive_sha256, now_text()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn extension_selection(&self, extension_id: &str) -> Result<ExtensionSelection> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT extension_id, selected_archive_sha256, enabled, lifecycle_state,
                    isolation, quarantine_reason, revision
             FROM extension_selection WHERE extension_id = ?1",
            [extension_id],
            selection_from_row,
        )
        .optional()?
        .ok_or_else(|| MetadataError::ExtensionNotFound(extension_id.to_string()))
    }

    pub fn list_extension_selections(&self) -> Result<Vec<ExtensionSelection>> {
        let conn = self.conn()?;
        let mut statement = conn.prepare(
            "SELECT extension_id, selected_archive_sha256, enabled, lifecycle_state,
                    isolation, quarantine_reason, revision
             FROM extension_selection ORDER BY extension_id",
        )?;
        let rows = statement.query_map([], selection_from_row)?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(Into::into)
    }

    pub fn update_extension_selection(
        &self,
        input: UpdateExtensionSelection<'_>,
    ) -> Result<ExtensionSelection> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = tx
            .query_row(
                "SELECT extension_id, selected_archive_sha256, enabled, lifecycle_state,
                        isolation, quarantine_reason, revision
                 FROM extension_selection WHERE extension_id = ?1",
                [input.extension_id],
                selection_from_row,
            )
            .optional()?
            .ok_or_else(|| MetadataError::ExtensionNotFound(input.extension_id.to_string()))?;
        if current.revision != input.expected_revision {
            return Err(MetadataError::ExtensionRevisionConflict {
                expected: input.expected_revision,
                current: current.revision,
            });
        }
        let revision =
            current
                .revision
                .checked_add(1)
                .ok_or(MetadataError::ExtensionRevisionConflict {
                    expected: input.expected_revision,
                    current: current.revision,
                })?;
        let revision_i64 =
            i64::try_from(revision).map_err(|_| MetadataError::ExtensionRevisionConflict {
                expected: input.expected_revision,
                current: current.revision,
            })?;
        tx.execute(
            "UPDATE extension_selection
             SET selected_archive_sha256 = ?2, enabled = ?3, lifecycle_state = ?4,
                 isolation = ?5, quarantine_reason = ?6, revision = ?7, updated_at = ?8
             WHERE extension_id = ?1",
            params![
                input.extension_id,
                input
                    .selected_archive_sha256
                    .unwrap_or(&current.selected_archive_sha256),
                input.enabled,
                lifecycle_text(input.lifecycle),
                isolation_text(input.isolation),
                input.quarantine_reason,
                revision_i64,
                now_text(),
            ],
        )?;
        tx.commit()?;
        drop(conn);
        self.extension_selection(input.extension_id)
    }

    pub fn replace_extension_grants(&self, input: &ReplaceExtensionGrants) -> Result<u64> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: i64 = tx
            .query_row(
                "SELECT revision FROM extension_selection WHERE extension_id = ?1",
                [&input.extension_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| MetadataError::ExtensionNotFound(input.extension_id.clone()))?;
        let current =
            u64::try_from(current).map_err(|_| MetadataError::ExtensionRevisionConflict {
                expected: input.expected_revision,
                current: 0,
            })?;
        if current != input.expected_revision {
            return Err(MetadataError::ExtensionRevisionConflict {
                expected: input.expected_revision,
                current,
            });
        }
        tx.execute(
            "DELETE FROM extension_grant WHERE extension_id = ?1",
            [&input.extension_id],
        )?;
        let revision = current
            .checked_add(1)
            .ok_or(MetadataError::ExtensionRevisionConflict {
                expected: input.expected_revision,
                current,
            })?;
        let revision_i64 =
            i64::try_from(revision).map_err(|_| MetadataError::ExtensionRevisionConflict {
                expected: input.expected_revision,
                current,
            })?;
        for (capability, constraints) in &input.grants {
            tx.execute(
                "INSERT INTO extension_grant
                 (extension_id, capability, constraints_json, revision, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    input.extension_id,
                    capability_text(*capability),
                    constraints,
                    revision_i64,
                    now_text(),
                ],
            )?;
        }
        tx.execute(
            "UPDATE extension_selection SET revision = ?2, updated_at = ?3
             WHERE extension_id = ?1",
            params![input.extension_id, revision_i64, now_text()],
        )?;
        tx.commit()?;
        Ok(revision)
    }

    pub fn set_extension_tenant_allowed(
        &self,
        extension_id: &str,
        tenant_id: i64,
        allowed: bool,
        expected_revision: u64,
    ) -> Result<u64> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: i64 = tx
            .query_row(
                "SELECT revision FROM extension_selection WHERE extension_id = ?1",
                [extension_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| MetadataError::ExtensionNotFound(extension_id.to_string()))?;
        let current =
            u64::try_from(current).map_err(|_| MetadataError::ExtensionRevisionConflict {
                expected: expected_revision,
                current: 0,
            })?;
        if current != expected_revision {
            return Err(MetadataError::ExtensionRevisionConflict {
                expected: expected_revision,
                current,
            });
        }
        let revision = current
            .checked_add(1)
            .ok_or(MetadataError::ExtensionRevisionConflict {
                expected: expected_revision,
                current,
            })?;
        let revision_i64 =
            i64::try_from(revision).map_err(|_| MetadataError::ExtensionRevisionConflict {
                expected: expected_revision,
                current,
            })?;
        tx.execute(
            "INSERT INTO extension_tenant_allowlist
             (extension_id, tenant_id, allowed, revision, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(extension_id, tenant_id) DO UPDATE SET
                allowed = excluded.allowed,
                revision = excluded.revision,
                updated_at = excluded.updated_at",
            params![extension_id, tenant_id, allowed, revision_i64, now_text()],
        )?;
        tx.execute(
            "UPDATE extension_selection SET revision = ?2, updated_at = ?3
             WHERE extension_id = ?1",
            params![extension_id, revision_i64, now_text()],
        )?;
        tx.commit()?;
        Ok(revision)
    }

    pub fn rollback_extension_selection(
        &self,
        extension_id: &str,
        expected_revision: u64,
    ) -> Result<ExtensionSelection> {
        let current = self.extension_selection(extension_id)?;
        if current.revision != expected_revision {
            return Err(MetadataError::ExtensionRevisionConflict {
                expected: expected_revision,
                current: current.revision,
            });
        }
        let conn = self.conn()?;
        let previous: String = conn
            .query_row(
                "SELECT archive_sha256 FROM extension_package
                 WHERE extension_id = ?1 AND archive_sha256 <> ?2
                 ORDER BY installed_at DESC, archive_sha256 DESC LIMIT 1",
                params![extension_id, current.selected_archive_sha256],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| MetadataError::ExtensionRollbackUnavailable(extension_id.into()))?;
        drop(conn);
        self.update_extension_selection(UpdateExtensionSelection {
            extension_id,
            selected_archive_sha256: Some(&previous),
            enabled: current.enabled,
            lifecycle: if current.enabled {
                ExtensionLifecycleState::Starting
            } else {
                ExtensionLifecycleState::Disabled
            },
            isolation: current.isolation,
            quarantine_reason: None,
            expected_revision,
        })
    }

    pub fn uninstall_extension(
        &self,
        extension_id: &str,
        expected_revision: u64,
    ) -> Result<ExtensionSelection> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = tx
            .query_row(
                "SELECT extension_id, selected_archive_sha256, enabled, lifecycle_state,
                        isolation, quarantine_reason, revision
                 FROM extension_selection WHERE extension_id = ?1",
                [extension_id],
                selection_from_row,
            )
            .optional()?
            .ok_or_else(|| MetadataError::ExtensionNotFound(extension_id.into()))?;
        if current.revision != expected_revision {
            return Err(MetadataError::ExtensionRevisionConflict {
                expected: expected_revision,
                current: current.revision,
            });
        }
        let revision =
            current
                .revision
                .checked_add(1)
                .ok_or(MetadataError::ExtensionRevisionConflict {
                    expected: expected_revision,
                    current: current.revision,
                })?;
        tx.execute(
            "DELETE FROM extension_grant WHERE extension_id = ?1",
            [extension_id],
        )?;
        tx.execute(
            "DELETE FROM extension_tenant_allowlist WHERE extension_id = ?1",
            [extension_id],
        )?;
        tx.execute(
            "UPDATE extension_storage_namespace
             SET state = 'orphaned', updated_at = ?2 WHERE extension_id = ?1",
            params![extension_id, now_text()],
        )?;
        tx.execute(
            "UPDATE extension_selection
             SET enabled = 0, lifecycle_state = 'uninstalled',
                 quarantine_reason = NULL, revision = ?2, updated_at = ?3
             WHERE extension_id = ?1",
            params![
                extension_id,
                i64::try_from(revision).map_err(|_| MetadataError::ExtensionRevisionConflict {
                    expected: expected_revision,
                    current: current.revision,
                })?,
                now_text()
            ],
        )?;
        tx.commit()?;
        drop(conn);
        self.extension_selection(extension_id)
    }
}

fn publisher_key_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ExtensionPublisherKey> {
    let bytes: Vec<u8> = row.get(2)?;
    let public_key: [u8; 32] = bytes.try_into().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            2,
            rusqlite::types::Type::Blob,
            "publisher key must be exactly 32 bytes".into(),
        )
    })?;
    let revision: i64 = row.get(6)?;
    Ok(ExtensionPublisherKey {
        publisher: row.get(0)?,
        fingerprint: row.get(1)?,
        public_key,
        valid_from: row.get(3)?,
        valid_until: row.get(4)?,
        revoked_at: row.get(5)?,
        revision: revision.try_into().map_err(|_| {
            rusqlite::Error::FromSqlConversionFailure(
                6,
                rusqlite::types::Type::Integer,
                "negative publisher key revision".into(),
            )
        })?,
    })
}

fn selection_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ExtensionSelection> {
    let revision: i64 = row.get(6)?;
    Ok(ExtensionSelection {
        extension_id: row.get(0)?,
        selected_archive_sha256: row.get(1)?,
        enabled: row.get(2)?,
        lifecycle: parse_lifecycle(row.get::<_, String>(3)?)?,
        isolation: parse_isolation(row.get::<_, String>(4)?)?,
        quarantine_reason: row.get(5)?,
        revision: revision.try_into().map_err(|_| {
            rusqlite::Error::FromSqlConversionFailure(
                6,
                rusqlite::types::Type::Integer,
                "negative extension revision".into(),
            )
        })?,
    })
}

fn provenance_text(value: ExtensionProvenance) -> &'static str {
    match value {
        ExtensionProvenance::Bundled => "bundled",
        ExtensionProvenance::Verified => "verified",
        ExtensionProvenance::Local => "local",
        ExtensionProvenance::Development => "development",
    }
}

fn parse_provenance(value: String) -> rusqlite::Result<ExtensionProvenance> {
    match value.as_str() {
        "bundled" => Ok(ExtensionProvenance::Bundled),
        "verified" => Ok(ExtensionProvenance::Verified),
        "local" => Ok(ExtensionProvenance::Local),
        "development" => Ok(ExtensionProvenance::Development),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn lifecycle_text(value: ExtensionLifecycleState) -> &'static str {
    match value {
        ExtensionLifecycleState::Installed => "installed",
        ExtensionLifecycleState::Disabled => "disabled",
        ExtensionLifecycleState::Starting => "starting",
        ExtensionLifecycleState::Ready => "ready",
        ExtensionLifecycleState::Degraded => "degraded",
        ExtensionLifecycleState::Quarantined => "quarantined",
        ExtensionLifecycleState::Uninstalled => "uninstalled",
        ExtensionLifecycleState::Orphaned => "orphaned",
    }
}

fn parse_lifecycle(value: String) -> rusqlite::Result<ExtensionLifecycleState> {
    match value.as_str() {
        "installed" => Ok(ExtensionLifecycleState::Installed),
        "disabled" => Ok(ExtensionLifecycleState::Disabled),
        "starting" => Ok(ExtensionLifecycleState::Starting),
        "ready" => Ok(ExtensionLifecycleState::Ready),
        "degraded" => Ok(ExtensionLifecycleState::Degraded),
        "quarantined" => Ok(ExtensionLifecycleState::Quarantined),
        "uninstalled" => Ok(ExtensionLifecycleState::Uninstalled),
        "orphaned" => Ok(ExtensionLifecycleState::Orphaned),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn isolation_text(value: ExtensionIsolation) -> &'static str {
    match value {
        ExtensionIsolation::HostEnforced => "host_enforced",
        ExtensionIsolation::PlatformSandboxed => "platform_sandboxed",
        ExtensionIsolation::ProcessOnly => "process_only",
    }
}

fn parse_isolation(value: String) -> rusqlite::Result<ExtensionIsolation> {
    match value.as_str() {
        "host_enforced" => Ok(ExtensionIsolation::HostEnforced),
        "platform_sandboxed" => Ok(ExtensionIsolation::PlatformSandboxed),
        "process_only" => Ok(ExtensionIsolation::ProcessOnly),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn capability_text(value: HostCapabilityKind) -> &'static str {
    match value {
        HostCapabilityKind::DatabaseConnect => "database.connect",
        HostCapabilityKind::SecretReceive => "secret.receive",
        HostCapabilityKind::NetworkConnect => "network.connect",
        HostCapabilityKind::NetworkListenLoopback => "network.listen.loopback",
        HostCapabilityKind::FilesystemData => "filesystem.data",
        HostCapabilityKind::FilesystemRead => "filesystem.read",
        HostCapabilityKind::FilesystemWrite => "filesystem.write",
        HostCapabilityKind::ProcessSpawn => "process.spawn",
        HostCapabilityKind::HttpFetch => "http.fetch",
        HostCapabilityKind::StorageKv => "storage.kv",
        HostCapabilityKind::OperationInvoke => "operation.invoke",
        HostCapabilityKind::EventPublish => "event.publish",
        HostCapabilityKind::ToolRegister => "tool.register",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemorySecretStore;
    use std::sync::Arc;

    fn package(digest: char) -> NewExtensionPackage {
        NewExtensionPackage {
            archive_sha256: digest.to_string().repeat(64),
            extension_id: "acme/example".into(),
            version: "1.0.0".into(),
            manifest_sha256: "b".repeat(64),
            manifest_json: "{}".into(),
            provenance: ExtensionProvenance::Local,
        }
    }

    #[test]
    fn package_selection_and_revision_updates_are_atomic() {
        let store = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        store.record_extension_package(&package('a'), &[]).unwrap();
        let selected = store.extension_selection("acme/example").unwrap();
        assert!(!selected.enabled);
        assert_eq!(selected.revision, 0);
        let updated = store
            .update_extension_selection(UpdateExtensionSelection {
                extension_id: "acme/example",
                selected_archive_sha256: None,
                enabled: true,
                lifecycle: ExtensionLifecycleState::Ready,
                isolation: ExtensionIsolation::ProcessOnly,
                quarantine_reason: None,
                expected_revision: 0,
            })
            .unwrap();
        assert!(updated.enabled);
        assert_eq!(updated.revision, 1);
        assert!(matches!(
            store.update_extension_selection(UpdateExtensionSelection {
                extension_id: "acme/example",
                selected_archive_sha256: None,
                enabled: false,
                lifecycle: ExtensionLifecycleState::Disabled,
                isolation: ExtensionIsolation::ProcessOnly,
                quarantine_reason: None,
                expected_revision: 0,
            }),
            Err(MetadataError::ExtensionRevisionConflict { .. })
        ));
    }

    #[test]
    fn reused_version_with_different_digest_is_rejected() {
        let store = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        store.record_extension_package(&package('a'), &[]).unwrap();
        assert!(matches!(
            store.record_extension_package(&package('c'), &[]),
            Err(MetadataError::ExtensionVersionDigestConflict { .. })
        ));
    }

    #[test]
    fn publisher_keys_are_namespace_scoped_expiring_and_revision_guarded() {
        let store = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        let key = ExtensionPublisherKey {
            publisher: "acme".into(),
            fingerprint: "sha256:test".into(),
            public_key: [7; 32],
            valid_from: "2000-01-01T00:00:00Z".into(),
            valid_until: None,
            revoked_at: None,
            revision: 0,
        };
        let inserted = store.put_extension_publisher_key(&key, None).unwrap();
        assert_eq!(inserted.revision, 0);
        assert_eq!(
            store.active_extension_publisher_keys("acme").unwrap(),
            vec![inserted.clone()]
        );
        assert!(store
            .active_extension_publisher_keys("other")
            .unwrap()
            .is_empty());
        let revoked = store
            .revoke_extension_publisher_key("acme", "sha256:test", 0)
            .unwrap();
        assert_eq!(revoked.revision, 1);
        assert!(revoked.revoked_at.is_some());
        assert!(store
            .active_extension_publisher_keys("acme")
            .unwrap()
            .is_empty());
        assert!(matches!(
            store.revoke_extension_publisher_key("acme", "sha256:test", 0),
            Err(MetadataError::ExtensionRevisionConflict { current: 1, .. })
        ));
    }
}
