use rusqlite::{params, OptionalExtension};
use sift_protocol::{
    Engine, PlanCapture, PlanCaptureId, PlanCaptureSummary, PlanNode, ProviderRef,
};

use super::{ConnectionProfileId, MetadataError, MetadataStore, Result, TenantId};

const MAX_PLAN_CAPTURE_BYTES: usize = 8 * 1024 * 1024;
const MAX_PLAN_CAPTURES_PER_TENANT: i64 = 5_000;
const MAX_PLAN_CAPTURES_PER_SOURCE: i64 = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanCaptureRetention {
    pub max_capture_bytes: usize,
    pub max_per_tenant: i64,
    pub max_per_source: i64,
    pub max_age_days: i64,
}

impl Default for PlanCaptureRetention {
    fn default() -> Self {
        Self {
            max_capture_bytes: MAX_PLAN_CAPTURE_BYTES,
            max_per_tenant: MAX_PLAN_CAPTURES_PER_TENANT,
            max_per_source: MAX_PLAN_CAPTURES_PER_SOURCE,
            max_age_days: 30,
        }
    }
}

impl MetadataStore {
    pub fn set_plan_capture_retention(&self, retention: PlanCaptureRetention) -> Result<()> {
        let defaults = PlanCaptureRetention::default();
        if retention.max_capture_bytes == 0
            || retention.max_capture_bytes > defaults.max_capture_bytes
            || retention.max_per_tenant <= 0
            || retention.max_per_tenant > defaults.max_per_tenant
            || retention.max_per_source <= 0
            || retention.max_per_source > defaults.max_per_source
            || retention.max_age_days <= 0
            || retention.max_age_days > defaults.max_age_days
        {
            return Err(MetadataError::InvalidPlanCaptureRetention);
        }
        *self.plan_capture_retention.write().unwrap() = retention;
        Ok(())
    }

    pub fn create_plan_capture(&self, capture: &PlanCapture) -> Result<()> {
        let retention = *self.plan_capture_retention.read().unwrap();
        if capture.raw_response.is_some() {
            return Err(MetadataError::InvalidEnum {
                field: "plan_capture.raw_response",
                value: "raw plans are not durable".into(),
            });
        }
        let root = serde_json::to_string(&capture.root)?;
        let warnings = serde_json::to_string(&capture.warnings)?;
        if root.len().saturating_add(warnings.len()) > retention.max_capture_bytes {
            return Err(MetadataError::PlanCaptureTooLarge {
                limit: retention.max_capture_bytes,
            });
        }
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let valid_scope: bool = tx.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM connection_profile cp
                JOIN membership m ON m.tenant_id = cp.tenant_id
                WHERE cp.id = ?1 AND cp.tenant_id = ?2 AND m.principal_id = ?3
            )",
            params![
                capture.connection_profile_id,
                capture.tenant_id,
                capture.creator_principal_id
            ],
            |row| row.get(0),
        )?;
        if !valid_scope {
            return Err(MetadataError::TenantMismatch(
                ConnectionProfileId(capture.connection_profile_id),
                TenantId(capture.tenant_id),
            ));
        }
        let retention_cutoff =
            (chrono::Utc::now() - chrono::Duration::days(retention.max_age_days)).to_rfc3339();
        tx.execute(
            "DELETE FROM plan_capture WHERE tenant_id = ?1 AND captured_at < ?2",
            params![capture.tenant_id, retention_cutoff],
        )?;
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM plan_capture WHERE tenant_id = ?1",
            [capture.tenant_id],
            |row| row.get(0),
        )?;
        if count >= retention.max_per_tenant {
            return Err(MetadataError::PlanCaptureLimitReached);
        }
        tx.execute(
            "DELETE FROM plan_capture WHERE id IN (
                SELECT id FROM plan_capture
                WHERE tenant_id = ?1 AND source_digest = ?2
                ORDER BY captured_at DESC, id DESC LIMIT -1 OFFSET ?3
             )",
            params![
                capture.tenant_id,
                capture.source_digest,
                retention.max_per_source - 1
            ],
        )?;
        tx.execute(
            "INSERT INTO plan_capture
             (id, tenant_id, connection_profile_id, creator_principal_id,
              provider_json, server_version, engine, source_digest,
              document_revision, statement_id, statement_fingerprint,
              catalog_revision, analyzed, captured_at, duration_ms, root_json,
              warnings_json, complete, revision)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                     ?12, ?13, ?14, ?15, ?16, ?17, ?18, 0)",
            params![
                capture.id.to_string(),
                capture.tenant_id,
                capture.connection_profile_id,
                capture.creator_principal_id,
                serde_json::to_string(&capture.provider)?,
                capture.server_version,
                engine_text(capture.engine),
                capture.source_digest,
                i64::try_from(capture.document_revision).map_err(|_| {
                    MetadataError::PlanCaptureTooLarge {
                        limit: retention.max_capture_bytes,
                    }
                })?,
                capture.statement_id,
                capture.statement_fingerprint,
                i64::try_from(capture.catalog_revision.0).map_err(|_| {
                    MetadataError::PlanCaptureTooLarge {
                        limit: retention.max_capture_bytes,
                    }
                })?,
                capture.analyzed,
                capture.captured_at.to_rfc3339(),
                i64::try_from(capture.duration_ms).unwrap_or(i64::MAX),
                root,
                warnings,
                capture.complete,
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn get_plan_capture(&self, tenant: TenantId, id: PlanCaptureId) -> Result<PlanCapture> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT tenant_id, connection_profile_id, creator_principal_id,
                    provider_json, server_version, engine, source_digest,
                    document_revision, statement_id, statement_fingerprint,
                    catalog_revision, analyzed, captured_at, duration_ms,
                    root_json, warnings_json, complete, revision
             FROM plan_capture WHERE id = ?1 AND tenant_id = ?2",
            params![id.to_string(), tenant.0],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, bool>(11)?,
                    row.get::<_, String>(12)?,
                    row.get::<_, i64>(13)?,
                    row.get::<_, String>(14)?,
                    row.get::<_, String>(15)?,
                    row.get::<_, bool>(16)?,
                    row.get::<_, i64>(17)?,
                ))
            },
        )
        .optional()?
        .ok_or(MetadataError::PlanCaptureNotFound)
        .and_then(|row| {
            Ok(PlanCapture {
                id,
                tenant_id: row.0,
                connection_profile_id: row.1,
                creator_principal_id: row.2,
                provider: serde_json::from_str::<ProviderRef>(&row.3)?,
                server_version: row.4,
                engine: parse_engine(&row.5)?,
                source_digest: row.6,
                document_revision: u64::try_from(row.7)
                    .map_err(|_| invalid("document_revision", row.7))?,
                statement_id: row.8,
                statement_fingerprint: row.9,
                catalog_revision: sift_protocol::CatalogRevision(
                    u64::try_from(row.10).map_err(|_| invalid("catalog_revision", row.10))?,
                ),
                analyzed: row.11,
                captured_at: super::parse_time_sql(row.12)?,
                duration_ms: u64::try_from(row.13).map_err(|_| invalid("duration_ms", row.13))?,
                root: serde_json::from_str::<PlanNode>(&row.14)?,
                warnings: serde_json::from_str(&row.15)?,
                complete: row.16,
                revision: u64::try_from(row.17).map_err(|_| invalid("revision", row.17))?,
                raw_response: None,
            })
        })
    }

    pub fn list_plan_captures(
        &self,
        tenant: TenantId,
        source_digest: Option<&str>,
        cursor: Option<PlanCaptureId>,
        limit: u32,
    ) -> Result<Vec<PlanCaptureSummary>> {
        let limit = limit.clamp(1, 101);
        let conn = self.conn()?;
        let cursor_key = cursor
            .map(|cursor| {
                conn.query_row(
                    "SELECT captured_at, source_digest FROM plan_capture
                     WHERE id = ?1 AND tenant_id = ?2",
                    params![cursor.to_string(), tenant.0],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()?
                .ok_or(MetadataError::PlanCaptureNotFound)
                .and_then(|(captured_at, cursor_source)| {
                    if source_digest.is_some_and(|source| source != cursor_source) {
                        Err(MetadataError::PlanCaptureNotFound)
                    } else {
                        Ok((captured_at, cursor.to_string()))
                    }
                })
            })
            .transpose()?;
        let (cursor_time, cursor_id) = cursor_key
            .map(|(time, id)| (Some(time), Some(id)))
            .unwrap_or((None, None));
        let mut statement = conn.prepare(
            "SELECT id, tenant_id, connection_profile_id, creator_principal_id,
                    provider_json, server_version, engine, source_digest,
                    document_revision, statement_id, statement_fingerprint,
                    catalog_revision, analyzed, captured_at, duration_ms,
                    json_extract(root_json, '$.op'), complete, revision
             FROM plan_capture
             WHERE tenant_id = ?1
               AND (?2 IS NULL OR source_digest = ?2)
               AND (?3 IS NULL OR captured_at < ?3
                    OR (captured_at = ?3 AND id < ?4))
             ORDER BY captured_at DESC, id DESC LIMIT ?5",
        )?;
        let raw = statement
            .query_map(
                params![
                    tenant.0,
                    source_digest,
                    cursor_time,
                    cursor_id,
                    i64::from(limit)
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, i64>(8)?,
                        row.get::<_, String>(9)?,
                        row.get::<_, String>(10)?,
                        row.get::<_, i64>(11)?,
                        row.get::<_, bool>(12)?,
                        row.get::<_, String>(13)?,
                        row.get::<_, i64>(14)?,
                        row.get::<_, String>(15)?,
                        row.get::<_, bool>(16)?,
                        row.get::<_, i64>(17)?,
                    ))
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        raw.into_iter()
            .map(|row| {
                Ok(PlanCaptureSummary {
                    id: PlanCaptureId(uuid::Uuid::parse_str(&row.0).map_err(|_| {
                        MetadataError::InvalidEnum {
                            field: "plan_capture.id",
                            value: row.0,
                        }
                    })?),
                    tenant_id: row.1,
                    connection_profile_id: row.2,
                    creator_principal_id: row.3,
                    provider: serde_json::from_str(&row.4)?,
                    server_version: row.5,
                    engine: parse_engine(&row.6)?,
                    source_digest: row.7,
                    document_revision: u64::try_from(row.8)
                        .map_err(|_| invalid("document_revision", row.8))?,
                    statement_id: row.9,
                    statement_fingerprint: row.10,
                    catalog_revision: sift_protocol::CatalogRevision(
                        u64::try_from(row.11).map_err(|_| invalid("catalog_revision", row.11))?,
                    ),
                    analyzed: row.12,
                    captured_at: super::parse_time_sql(row.13)?,
                    duration_ms: u64::try_from(row.14)
                        .map_err(|_| invalid("duration_ms", row.14))?,
                    root_operator: row.15,
                    complete: row.16,
                    revision: u64::try_from(row.17).map_err(|_| invalid("revision", row.17))?,
                })
            })
            .collect()
    }

    pub fn delete_plan_capture(
        &self,
        tenant: TenantId,
        id: PlanCaptureId,
        expected_revision: u64,
    ) -> Result<()> {
        let conn = self.conn()?;
        let changed = conn.execute(
            "DELETE FROM plan_capture WHERE id = ?1 AND tenant_id = ?2 AND revision = ?3",
            params![
                id.to_string(),
                tenant.0,
                i64::try_from(expected_revision).unwrap_or(-1)
            ],
        )?;
        if changed == 1 {
            return Ok(());
        }
        let current = conn
            .query_row(
                "SELECT revision FROM plan_capture WHERE id = ?1 AND tenant_id = ?2",
                params![id.to_string(), tenant.0],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        match current {
            Some(current) => Err(MetadataError::PlanCaptureRevisionConflict {
                expected: expected_revision,
                current: u64::try_from(current).unwrap_or(0),
            }),
            None => Err(MetadataError::PlanCaptureNotFound),
        }
    }
}

fn engine_text(engine: Engine) -> &'static str {
    match engine {
        Engine::Postgres => "postgres",
        Engine::SqlServer => "sql_server",
    }
}

fn parse_engine(value: &str) -> Result<Engine> {
    match value {
        "postgres" => Ok(Engine::Postgres),
        "sql_server" => Ok(Engine::SqlServer),
        value => Err(MetadataError::InvalidEnum {
            field: "plan_capture.engine",
            value: value.into(),
        }),
    }
}

fn invalid(field: &'static str, value: i64) -> MetadataError {
    MetadataError::InvalidEnum {
        field,
        value: value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sift_protocol::{CatalogRevision, DialectId, ProviderId};

    use super::*;
    use crate::{CredentialMode, MemorySecretStore, NewConnectionProfile, PrincipalId};

    fn capture(
        profile: i64,
        source: &str,
        captured_at: chrono::DateTime<chrono::Utc>,
    ) -> PlanCapture {
        PlanCapture {
            id: PlanCaptureId(uuid::Uuid::new_v4()),
            tenant_id: 1,
            connection_profile_id: profile,
            creator_principal_id: 1,
            provider: ProviderRef {
                provider_id: ProviderId::new("test/postgres").unwrap(),
                dialect_id: DialectId::new("test/postgres").unwrap(),
                provider_version: "1".into(),
            },
            server_version: "16".into(),
            engine: Engine::Postgres,
            source_digest: source.into(),
            document_revision: 1,
            statement_id: "stmt:test".into(),
            statement_fingerprint: "sqlfp:test".into(),
            catalog_revision: CatalogRevision(1),
            analyzed: false,
            captured_at,
            duration_ms: 5,
            root: PlanNode::new("Seq Scan"),
            warnings: Vec::new(),
            complete: true,
            revision: 0,
            raw_response: None,
        }
    }

    #[tokio::test]
    async fn plan_captures_page_by_source_and_never_persist_raw_plans() {
        let store = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        store.bootstrap_local("local user").unwrap();
        let profile = store
            .upsert_connection_profile(
                TenantId(1),
                PrincipalId(1),
                NewConnectionProfile {
                    name: "plan fixture".into(),
                    provider_id: Engine::Postgres.provider_id(),
                    configuration: serde_json::json!({"host": "fixture", "user": "fixture"}),
                    semantic_engine: Some(Engine::Postgres),
                    credentials: None,
                    credential_mode: CredentialMode::Shared,
                    tags: Vec::new(),
                },
            )
            .await
            .unwrap();
        let source = format!("sha256:{}", "a".repeat(64));
        let older = capture(
            profile.id.0,
            &source,
            chrono::Utc::now() - chrono::Duration::seconds(1),
        );
        let newer = capture(profile.id.0, &source, chrono::Utc::now());
        store.create_plan_capture(&older).unwrap();
        store.create_plan_capture(&newer).unwrap();

        let first = store
            .list_plan_captures(TenantId(1), Some(&source), None, 1)
            .unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].id, newer.id);
        assert_eq!(first[0].root_operator, "Seq Scan");
        let second = store
            .list_plan_captures(TenantId(1), Some(&source), Some(first[0].id), 2)
            .unwrap();
        assert_eq!(
            second.iter().map(|capture| capture.id).collect::<Vec<_>>(),
            vec![older.id]
        );

        let mut raw = capture(profile.id.0, &source, chrono::Utc::now());
        raw.raw_response = Some("sensitive raw plan".into());
        assert!(matches!(
            store.create_plan_capture(&raw),
            Err(MetadataError::InvalidEnum { .. })
        ));
        assert!(matches!(
            store.delete_plan_capture(TenantId(1), newer.id, 1),
            Err(MetadataError::PlanCaptureRevisionConflict { current: 0, .. })
        ));

        store
            .set_plan_capture_retention(PlanCaptureRetention {
                max_per_source: 1,
                ..PlanCaptureRetention::default()
            })
            .unwrap();
        let latest = capture(profile.id.0, &source, chrono::Utc::now());
        store.create_plan_capture(&latest).unwrap();
        let retained = store
            .list_plan_captures(TenantId(1), Some(&source), None, 10)
            .unwrap();
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].id, latest.id);
        assert!(matches!(
            store.set_plan_capture_retention(PlanCaptureRetention {
                max_per_source: 51,
                ..PlanCaptureRetention::default()
            }),
            Err(MetadataError::InvalidPlanCaptureRetention)
        ));
    }
}
