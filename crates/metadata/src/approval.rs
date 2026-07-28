use chrono::{Duration, Utc};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use sift_protocol::OperationApproval;
use uuid::Uuid;

use super::{now_text, MetadataError, MetadataStore, PrincipalId, Result};

const DEFAULT_APPROVAL_TTL: Duration = Duration::minutes(5);
const MAX_APPROVAL_TTL: Duration = Duration::minutes(15);

#[derive(Debug, Clone)]
pub struct ApprovalBinding {
    pub principal_id: PrincipalId,
    pub operation_id: String,
    pub context_fingerprint: String,
    pub input_fingerprint: String,
}

impl MetadataStore {
    pub fn create_operation_approval(
        &self,
        binding: &ApprovalBinding,
        ttl: Option<Duration>,
    ) -> Result<OperationApproval> {
        validate_binding(binding)?;
        let ttl = ttl.unwrap_or(DEFAULT_APPROVAL_TTL);
        if ttl <= Duration::zero() || ttl > MAX_APPROVAL_TTL {
            return Err(MetadataError::InvalidOperationApproval);
        }
        let now = Utc::now();
        let id = Uuid::new_v4().to_string();
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO operation_approval
             (id, principal_id, operation_id, context_fingerprint, input_fingerprint,
              expires_at, revision, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7)",
            params![
                id,
                binding.principal_id.0,
                binding.operation_id,
                binding.context_fingerprint,
                binding.input_fingerprint,
                (now + ttl).to_rfc3339(),
                now.to_rfc3339(),
            ],
        )?;
        operation_approval(&conn, &id)?.ok_or(MetadataError::InvalidOperationApproval)
    }

    pub fn approve_operation(
        &self,
        approval_id: &str,
        principal_id: PrincipalId,
        expected_revision: u64,
    ) -> Result<OperationApproval> {
        let conn = self.conn()?;
        let revision = i64::try_from(expected_revision)
            .map_err(|_| MetadataError::InvalidOperationApproval)?;
        let changed = conn.execute(
            "UPDATE operation_approval
             SET approved_at = ?4, revision = revision + 1
             WHERE id = ?1 AND principal_id = ?2 AND revision = ?3
               AND approved_at IS NULL AND consumed_at IS NULL AND expires_at > ?4",
            params![approval_id, principal_id.0, revision, now_text()],
        )?;
        if changed != 1 {
            return Err(MetadataError::InvalidOperationApproval);
        }
        operation_approval(&conn, approval_id)?.ok_or(MetadataError::InvalidOperationApproval)
    }

    pub fn consume_operation_approval(
        &self,
        approval_id: &str,
        binding: &ApprovalBinding,
    ) -> Result<OperationApproval> {
        validate_binding(binding)?;
        let mut conn = self.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = now_text();
        let changed = tx.execute(
            "UPDATE operation_approval
             SET consumed_at = ?7, revision = revision + 1
             WHERE id = ?1 AND principal_id = ?2 AND operation_id = ?3
               AND context_fingerprint = ?4 AND input_fingerprint = ?5
               AND approved_at IS NOT NULL AND consumed_at IS NULL AND expires_at > ?6",
            params![
                approval_id,
                binding.principal_id.0,
                binding.operation_id,
                binding.context_fingerprint,
                binding.input_fingerprint,
                now,
                now,
            ],
        )?;
        if changed != 1 {
            return Err(MetadataError::InvalidOperationApproval);
        }
        let approval =
            operation_approval(&tx, approval_id)?.ok_or(MetadataError::InvalidOperationApproval)?;
        tx.commit()?;
        Ok(approval)
    }
}

fn validate_binding(binding: &ApprovalBinding) -> Result<()> {
    if binding.operation_id.is_empty()
        || !is_sha256(&binding.context_fingerprint)
        || !is_sha256(&binding.input_fingerprint)
    {
        Err(MetadataError::InvalidOperationApproval)
    } else {
        Ok(())
    }
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn operation_approval(conn: &rusqlite::Connection, id: &str) -> Result<Option<OperationApproval>> {
    conn.query_row(
        "SELECT id, principal_id, operation_id, input_fingerprint, expires_at,
                approved_at, consumed_at, revision
         FROM operation_approval WHERE id = ?1",
        [id],
        |row| {
            Ok(OperationApproval {
                id: row.get(0)?,
                principal_id: row.get(1)?,
                operation_id: row.get(2)?,
                input_fingerprint: row.get(3)?,
                expires_at: row.get(4)?,
                approved_at: row.get(5)?,
                consumed_at: row.get(6)?,
                revision: row
                    .get::<_, i64>(7)?
                    .try_into()
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(7, i64::MIN))?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::MemorySecretStore;

    fn binding() -> ApprovalBinding {
        ApprovalBinding {
            principal_id: PrincipalId(1),
            operation_id: "acme/tool#write".into(),
            context_fingerprint: "a".repeat(64),
            input_fingerprint: "b".repeat(64),
        }
    }

    #[test]
    fn approval_is_bound_and_consumed_exactly_once() {
        let store = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        store.bootstrap_local("test").unwrap();
        let approval = store.create_operation_approval(&binding(), None).unwrap();
        let approval = store
            .approve_operation(&approval.id, PrincipalId(1), approval.revision)
            .unwrap();
        assert!(approval.approved_at.is_some());
        let consumed = store
            .consume_operation_approval(&approval.id, &binding())
            .unwrap();
        assert!(consumed.consumed_at.is_some());
        assert!(matches!(
            store.consume_operation_approval(&approval.id, &binding()),
            Err(MetadataError::InvalidOperationApproval)
        ));
        let mut changed = binding();
        changed.input_fingerprint = "c".repeat(64);
        assert!(matches!(
            store.consume_operation_approval(&approval.id, &changed),
            Err(MetadataError::InvalidOperationApproval)
        ));
    }
}
