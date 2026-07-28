use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
use sha2::{Digest, Sha256};

use super::{now_text, MetadataError, MetadataStore, Result};

const INSTANCE_SCOPE: i64 = -1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtensionStorageLimits {
    pub max_key_bytes: usize,
    pub max_value_bytes: usize,
    pub max_namespace_bytes: u64,
    pub max_migration_bytes: u64,
}

impl Default for ExtensionStorageLimits {
    fn default() -> Self {
        Self {
            max_key_bytes: 255,
            max_value_bytes: 1024 * 1024,
            max_namespace_bytes: 64 * 1024 * 1024,
            max_migration_bytes: 64 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpectedStorageRevision {
    Any,
    Missing,
    Exact(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionStorageValue {
    pub key: String,
    pub value: Vec<u8>,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionStorageNamespace {
    pub id: i64,
    pub extension_id: String,
    pub tenant_id: Option<i64>,
    pub generation: u64,
    pub schema_version: u32,
    pub state: String,
    pub total_bytes: u64,
    pub revision: u64,
}

pub struct ExtensionStoragePut<'a> {
    pub extension_id: &'a str,
    pub tenant_id: Option<i64>,
    pub schema_version: u32,
    pub key: &'a str,
    pub value: &'a [u8],
    pub expected: ExpectedStorageRevision,
    pub limits: ExtensionStorageLimits,
}

impl MetadataStore {
    pub fn extension_storage_get(
        &self,
        extension_id: &str,
        tenant_id: Option<i64>,
        key: &str,
    ) -> Result<Option<ExtensionStorageValue>> {
        validate_key(key, ExtensionStorageLimits::default())?;
        let conn = self.conn()?;
        conn.query_row(
            "SELECT e.key, b.value, e.revision
             FROM extension_storage_namespace n
             JOIN extension_storage_entry e ON e.namespace_id = n.id
             JOIN extension_storage_blob b ON b.sha256 = e.blob_sha256
             WHERE n.extension_id = ?1 AND n.tenant_scope = ?2
               AND n.state = 'active' AND e.key = ?3",
            params![extension_id, scope(tenant_id), key],
            |row| {
                Ok(ExtensionStorageValue {
                    key: row.get(0)?,
                    value: row.get(1)?,
                    revision: u64::try_from(row.get::<_, i64>(2)?)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(2, i64::MIN))?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn extension_storage_put(
        &self,
        input: ExtensionStoragePut<'_>,
    ) -> Result<ExtensionStorageValue> {
        let ExtensionStoragePut {
            extension_id,
            tenant_id,
            schema_version,
            key,
            value,
            expected,
            limits,
        } = input;
        validate_key(key, limits)?;
        if value.len() > limits.max_value_bytes {
            return Err(MetadataError::ExtensionStorageValueTooLarge {
                limit: limits.max_value_bytes,
            });
        }
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let namespace = active_or_create(&tx, extension_id, tenant_id, schema_version)?;
        let current = entry(&tx, namespace.id, key)?;
        check_expected(current.as_ref().map(|value| value.revision), expected)?;
        let old_bytes = current.as_ref().map_or(0, |current| current.value.len());
        let requested = namespace
            .total_bytes
            .checked_sub(old_bytes as u64)
            .and_then(|bytes| bytes.checked_add(value.len() as u64))
            .ok_or(MetadataError::ExtensionStorageQuotaExceeded {
                requested: u64::MAX,
                limit: limits.max_namespace_bytes,
            })?;
        if requested > limits.max_namespace_bytes {
            return Err(MetadataError::ExtensionStorageQuotaExceeded {
                requested,
                limit: limits.max_namespace_bytes,
            });
        }

        let revision = current
            .as_ref()
            .map_or(1, |current| current.revision.saturating_add(1));
        let digest = hex_digest(value);
        retain_blob(&tx, &digest, value)?;
        if let Some(current) = &current {
            let old_digest: String = tx.query_row(
                "SELECT blob_sha256 FROM extension_storage_entry
                 WHERE namespace_id = ?1 AND key = ?2",
                params![namespace.id, key],
                |row| row.get(0),
            )?;
            tx.execute(
                "UPDATE extension_storage_entry
                 SET blob_sha256 = ?3, revision = ?4
                 WHERE namespace_id = ?1 AND key = ?2",
                params![
                    namespace.id,
                    key,
                    digest,
                    i64::try_from(revision)
                        .map_err(|_| MetadataError::ExtensionStorageRevisionConflict)?
                ],
            )?;
            release_blob(&tx, &old_digest)?;
            debug_assert_eq!(current.key, key);
        } else {
            tx.execute(
                "INSERT INTO extension_storage_entry
                 (namespace_id, key, blob_sha256, revision) VALUES (?1, ?2, ?3, 1)",
                params![namespace.id, key, digest],
            )?;
        }
        tx.execute(
            "UPDATE extension_storage_namespace
             SET total_bytes = ?2, revision = revision + 1, updated_at = ?3
             WHERE id = ?1",
            params![
                namespace.id,
                i64::try_from(requested).map_err(|_| {
                    MetadataError::ExtensionStorageQuotaExceeded {
                        requested,
                        limit: limits.max_namespace_bytes,
                    }
                })?,
                now_text()
            ],
        )?;
        tx.commit()?;
        Ok(ExtensionStorageValue {
            key: key.into(),
            value: value.into(),
            revision,
        })
    }

    pub fn extension_storage_delete(
        &self,
        extension_id: &str,
        tenant_id: Option<i64>,
        key: &str,
        expected: ExpectedStorageRevision,
    ) -> Result<bool> {
        validate_key(key, ExtensionStorageLimits::default())?;
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(namespace) = active_namespace(&tx, extension_id, tenant_id)? else {
            check_expected(None, expected)?;
            return Ok(false);
        };
        let current = entry(&tx, namespace.id, key)?;
        check_expected(current.as_ref().map(|value| value.revision), expected)?;
        let Some(current) = current else {
            return Ok(false);
        };
        let digest: String = tx.query_row(
            "SELECT blob_sha256 FROM extension_storage_entry
             WHERE namespace_id = ?1 AND key = ?2",
            params![namespace.id, key],
            |row| row.get(0),
        )?;
        tx.execute(
            "DELETE FROM extension_storage_entry WHERE namespace_id = ?1 AND key = ?2",
            params![namespace.id, key],
        )?;
        tx.execute(
            "UPDATE extension_storage_namespace
             SET total_bytes = total_bytes - ?2, revision = revision + 1, updated_at = ?3
             WHERE id = ?1",
            params![namespace.id, current.value.len() as i64, now_text()],
        )?;
        release_blob(&tx, &digest)?;
        tx.commit()?;
        Ok(true)
    }

    pub fn stage_extension_storage_migration(
        &self,
        extension_id: &str,
        tenant_id: Option<i64>,
        target_schema_version: u32,
        limits: ExtensionStorageLimits,
    ) -> Result<ExtensionStorageNamespace> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let active = active_namespace(&tx, extension_id, tenant_id)?
            .ok_or(MetadataError::ExtensionStorageNamespaceNotFound)?;
        if active.total_bytes > limits.max_migration_bytes {
            return Err(MetadataError::ExtensionStorageQuotaExceeded {
                requested: active.total_bytes,
                limit: limits.max_migration_bytes,
            });
        }
        let generation = active.generation.saturating_add(1);
        tx.execute(
            "INSERT INTO extension_storage_namespace
             (extension_id, tenant_scope, generation, schema_version, state,
              total_bytes, revision, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'staged', ?5, 0, ?6, ?6)",
            params![
                extension_id,
                scope(tenant_id),
                i64::try_from(generation)
                    .map_err(|_| MetadataError::ExtensionStorageRevisionConflict)?,
                i64::from(target_schema_version),
                i64::try_from(active.total_bytes).map_err(|_| {
                    MetadataError::ExtensionStorageQuotaExceeded {
                        requested: active.total_bytes,
                        limit: limits.max_migration_bytes,
                    }
                })?,
                now_text()
            ],
        )?;
        let staged_id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO extension_storage_entry
             (namespace_id, key, blob_sha256, revision)
             SELECT ?1, key, blob_sha256, revision
             FROM extension_storage_entry WHERE namespace_id = ?2",
            params![staged_id, active.id],
        )?;
        tx.execute(
            "UPDATE extension_storage_blob SET reference_count = reference_count + (
                 SELECT count(*) FROM extension_storage_entry e
                 WHERE e.namespace_id = ?1 AND e.blob_sha256 = extension_storage_blob.sha256
             )
             WHERE sha256 IN (
                 SELECT blob_sha256 FROM extension_storage_entry WHERE namespace_id = ?1
             )",
            [staged_id],
        )?;
        tx.commit()?;
        drop(conn);
        self.extension_storage_namespace(extension_id, tenant_id, "staged")
    }

    pub fn activate_extension_storage_migration(
        &self,
        extension_id: &str,
        tenant_id: Option<i64>,
        staged_generation: u64,
    ) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE extension_storage_namespace SET state = 'rollback', updated_at = ?3
             WHERE extension_id = ?1 AND tenant_scope = ?2 AND state = 'active'",
            params![extension_id, scope(tenant_id), now_text()],
        )?;
        if changed != 1 {
            return Err(MetadataError::ExtensionStorageNamespaceNotFound);
        }
        let changed = tx.execute(
            "UPDATE extension_storage_namespace SET state = 'active', updated_at = ?4
             WHERE extension_id = ?1 AND tenant_scope = ?2
               AND generation = ?3 AND state = 'staged'",
            params![
                extension_id,
                scope(tenant_id),
                i64::try_from(staged_generation)
                    .map_err(|_| MetadataError::ExtensionStorageRevisionConflict)?,
                now_text()
            ],
        )?;
        if changed != 1 {
            return Err(MetadataError::ExtensionStorageNamespaceNotFound);
        }
        tx.commit()?;
        Ok(())
    }

    pub fn discard_extension_storage_generation(
        &self,
        extension_id: &str,
        tenant_id: Option<i64>,
        generation: u64,
    ) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let namespace: Option<i64> = tx
            .query_row(
                "SELECT id FROM extension_storage_namespace
                 WHERE extension_id = ?1 AND tenant_scope = ?2 AND generation = ?3
                   AND state IN ('staged', 'rollback')",
                params![
                    extension_id,
                    scope(tenant_id),
                    i64::try_from(generation)
                        .map_err(|_| MetadataError::ExtensionStorageRevisionConflict)?
                ],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(namespace) = namespace {
            release_namespace_blobs(&tx, namespace)?;
            tx.execute(
                "DELETE FROM extension_storage_namespace WHERE id = ?1",
                [namespace],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn orphan_extension_storage(&self, extension_id: &str) -> Result<u64> {
        let conn = self.conn()?;
        let changed = conn.execute(
            "UPDATE extension_storage_namespace
             SET state = 'orphaned', updated_at = ?2 WHERE extension_id = ?1",
            params![extension_id, now_text()],
        )?;
        Ok(changed as u64)
    }

    pub fn purge_extension_storage(&self, extension_id: &str) -> Result<u64> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut statement = tx.prepare(
            "SELECT id FROM extension_storage_namespace
             WHERE extension_id = ?1 AND state = 'orphaned'",
        )?;
        let ids = statement
            .query_map([extension_id], |row| row.get::<_, i64>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(statement);
        for id in &ids {
            release_namespace_blobs(&tx, *id)?;
        }
        tx.execute(
            "DELETE FROM extension_storage_namespace
             WHERE extension_id = ?1 AND state = 'orphaned'",
            [extension_id],
        )?;
        tx.commit()?;
        Ok(ids.len() as u64)
    }

    pub fn reconcile_extension_storage_blobs(&self) -> Result<u64> {
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE extension_storage_blob SET reference_count = (
                 SELECT count(*) FROM extension_storage_entry e
                 WHERE e.blob_sha256 = extension_storage_blob.sha256
             )",
            [],
        )?;
        let removed = tx.execute(
            "DELETE FROM extension_storage_blob WHERE reference_count = 0",
            [],
        )?;
        tx.commit()?;
        Ok(removed as u64)
    }

    fn extension_storage_namespace(
        &self,
        extension_id: &str,
        tenant_id: Option<i64>,
        state: &str,
    ) -> Result<ExtensionStorageNamespace> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT id, extension_id, tenant_scope, generation, schema_version,
                    state, total_bytes, revision
             FROM extension_storage_namespace
             WHERE extension_id = ?1 AND tenant_scope = ?2 AND state = ?3",
            params![extension_id, scope(tenant_id), state],
            namespace_from_row,
        )
        .optional()?
        .ok_or(MetadataError::ExtensionStorageNamespaceNotFound)
    }
}

fn active_or_create(
    tx: &Transaction<'_>,
    extension_id: &str,
    tenant_id: Option<i64>,
    schema_version: u32,
) -> Result<ExtensionStorageNamespace> {
    if let Some(namespace) = active_namespace(tx, extension_id, tenant_id)? {
        if namespace.schema_version != schema_version {
            return Err(MetadataError::ExtensionStorageRevisionConflict);
        }
        return Ok(namespace);
    }
    tx.execute(
        "INSERT INTO extension_storage_namespace
         (extension_id, tenant_scope, generation, schema_version, state,
          total_bytes, revision, created_at, updated_at)
         VALUES (?1, ?2, 0, ?3, 'active', 0, 0, ?4, ?4)",
        params![
            extension_id,
            scope(tenant_id),
            i64::from(schema_version),
            now_text()
        ],
    )?;
    Ok(ExtensionStorageNamespace {
        id: tx.last_insert_rowid(),
        extension_id: extension_id.into(),
        tenant_id,
        generation: 0,
        schema_version,
        state: "active".into(),
        total_bytes: 0,
        revision: 0,
    })
}

fn active_namespace(
    tx: &Transaction<'_>,
    extension_id: &str,
    tenant_id: Option<i64>,
) -> Result<Option<ExtensionStorageNamespace>> {
    tx.query_row(
        "SELECT id, extension_id, tenant_scope, generation, schema_version,
                state, total_bytes, revision
         FROM extension_storage_namespace
         WHERE extension_id = ?1 AND tenant_scope = ?2 AND state = 'active'",
        params![extension_id, scope(tenant_id)],
        namespace_from_row,
    )
    .optional()
    .map_err(Into::into)
}

fn namespace_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ExtensionStorageNamespace> {
    let tenant_scope: i64 = row.get(2)?;
    Ok(ExtensionStorageNamespace {
        id: row.get(0)?,
        extension_id: row.get(1)?,
        tenant_id: (tenant_scope != INSTANCE_SCOPE).then_some(tenant_scope),
        generation: row
            .get::<_, i64>(3)?
            .try_into()
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(3, i64::MIN))?,
        schema_version: row
            .get::<_, i64>(4)?
            .try_into()
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(4, i64::MIN))?,
        state: row.get(5)?,
        total_bytes: row
            .get::<_, i64>(6)?
            .try_into()
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(6, i64::MIN))?,
        revision: row
            .get::<_, i64>(7)?
            .try_into()
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(7, i64::MIN))?,
    })
}

fn entry(
    tx: &Transaction<'_>,
    namespace_id: i64,
    key: &str,
) -> Result<Option<ExtensionStorageValue>> {
    tx.query_row(
        "SELECT e.key, b.value, e.revision
         FROM extension_storage_entry e
         JOIN extension_storage_blob b ON b.sha256 = e.blob_sha256
         WHERE e.namespace_id = ?1 AND e.key = ?2",
        params![namespace_id, key],
        |row| {
            Ok(ExtensionStorageValue {
                key: row.get(0)?,
                value: row.get(1)?,
                revision: row
                    .get::<_, i64>(2)?
                    .try_into()
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(2, i64::MIN))?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn retain_blob(tx: &Transaction<'_>, digest: &str, value: &[u8]) -> Result<()> {
    tx.execute(
        "INSERT INTO extension_storage_blob (sha256, value, byte_count, reference_count)
         VALUES (?1, ?2, ?3, 1)
         ON CONFLICT(sha256) DO UPDATE SET reference_count = reference_count + 1",
        params![digest, value, value.len() as i64],
    )?;
    Ok(())
}

fn release_blob(tx: &Transaction<'_>, digest: &str) -> Result<()> {
    tx.execute(
        "DELETE FROM extension_storage_blob
         WHERE sha256 = ?1 AND reference_count = 1",
        [digest],
    )?;
    tx.execute(
        "UPDATE extension_storage_blob
         SET reference_count = reference_count - 1
         WHERE sha256 = ?1 AND reference_count > 1",
        [digest],
    )?;
    Ok(())
}

fn release_namespace_blobs(tx: &Transaction<'_>, namespace_id: i64) -> Result<()> {
    let mut statement = tx.prepare(
        "SELECT blob_sha256, count(*) FROM extension_storage_entry
         WHERE namespace_id = ?1 GROUP BY blob_sha256",
    )?;
    let references = statement
        .query_map([namespace_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(statement);
    tx.execute(
        "DELETE FROM extension_storage_entry WHERE namespace_id = ?1",
        [namespace_id],
    )?;
    for (digest, count) in references {
        tx.execute(
            "DELETE FROM extension_storage_blob
             WHERE sha256 = ?1 AND reference_count <= ?2",
            params![digest, count],
        )?;
        tx.execute(
            "UPDATE extension_storage_blob
             SET reference_count = reference_count - ?2
             WHERE sha256 = ?1 AND reference_count > ?2",
            params![digest, count],
        )?;
    }
    Ok(())
}

fn check_expected(current: Option<u64>, expected: ExpectedStorageRevision) -> Result<()> {
    let matches = match expected {
        ExpectedStorageRevision::Any => true,
        ExpectedStorageRevision::Missing => current.is_none(),
        ExpectedStorageRevision::Exact(revision) => current == Some(revision),
    };
    if matches {
        Ok(())
    } else {
        Err(MetadataError::ExtensionStorageRevisionConflict)
    }
}

fn validate_key(key: &str, limits: ExtensionStorageLimits) -> Result<()> {
    if key.is_empty()
        || key.len() > limits.max_key_bytes
        || key.bytes().any(|byte| byte == 0 || byte.is_ascii_control())
    {
        Err(MetadataError::ExtensionStorageInvalidKey)
    } else {
        Ok(())
    }
}

fn scope(tenant_id: Option<i64>) -> i64 {
    tenant_id.unwrap_or(INSTANCE_SCOPE)
}

fn hex_digest(value: &[u8]) -> String {
    let digest = Sha256::digest(value);
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::MemorySecretStore;

    fn store() -> MetadataStore {
        MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap()
    }

    #[test]
    fn storage_enforces_cas_and_quota_before_mutation() {
        let store = store();
        let limits = ExtensionStorageLimits {
            max_value_bytes: 4,
            max_namespace_bytes: 5,
            ..ExtensionStorageLimits::default()
        };
        let first = store
            .extension_storage_put(ExtensionStoragePut {
                extension_id: "acme/test",
                tenant_id: Some(7),
                schema_version: 1,
                key: "key",
                value: b"abc",
                expected: ExpectedStorageRevision::Missing,
                limits,
            })
            .unwrap();
        assert_eq!(first.revision, 1);
        assert!(matches!(
            store.extension_storage_put(ExtensionStoragePut {
                extension_id: "acme/test",
                tenant_id: Some(7),
                schema_version: 1,
                key: "key",
                value: b"x",
                expected: ExpectedStorageRevision::Missing,
                limits,
            }),
            Err(MetadataError::ExtensionStorageRevisionConflict)
        ));
        assert!(matches!(
            store.extension_storage_put(ExtensionStoragePut {
                extension_id: "acme/test",
                tenant_id: Some(7),
                schema_version: 1,
                key: "other",
                value: b"xyz",
                expected: ExpectedStorageRevision::Missing,
                limits,
            }),
            Err(MetadataError::ExtensionStorageQuotaExceeded { .. })
        ));
        assert_eq!(
            store
                .extension_storage_get("acme/test", Some(7), "key")
                .unwrap()
                .unwrap()
                .value,
            b"abc"
        );
    }

    #[test]
    fn migration_switch_is_atomic_and_rollback_data_is_collectable() {
        let store = store();
        let limits = ExtensionStorageLimits::default();
        store
            .extension_storage_put(ExtensionStoragePut {
                extension_id: "acme/test",
                tenant_id: None,
                schema_version: 1,
                key: "key",
                value: b"old",
                expected: ExpectedStorageRevision::Missing,
                limits,
            })
            .unwrap();
        let staged = store
            .stage_extension_storage_migration("acme/test", None, 2, limits)
            .unwrap();
        store
            .activate_extension_storage_migration("acme/test", None, staged.generation)
            .unwrap();
        assert_eq!(
            store
                .extension_storage_get("acme/test", None, "key")
                .unwrap()
                .unwrap()
                .value,
            b"old"
        );
        store
            .discard_extension_storage_generation("acme/test", None, 0)
            .unwrap();
        assert_eq!(store.reconcile_extension_storage_blobs().unwrap(), 0);
    }

    #[test]
    fn uninstall_retains_data_until_explicit_purge() {
        let store = store();
        store
            .extension_storage_put(ExtensionStoragePut {
                extension_id: "acme/test",
                tenant_id: Some(9),
                schema_version: 1,
                key: "key",
                value: b"value",
                expected: ExpectedStorageRevision::Missing,
                limits: ExtensionStorageLimits::default(),
            })
            .unwrap();
        assert_eq!(store.orphan_extension_storage("acme/test").unwrap(), 1);
        assert!(store
            .extension_storage_get("acme/test", Some(9), "key")
            .unwrap()
            .is_none());
        assert_eq!(store.purge_extension_storage("acme/test").unwrap(), 1);
        assert_eq!(store.reconcile_extension_storage_blobs().unwrap(), 0);
    }
}
