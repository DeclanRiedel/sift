use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};
use sift_protocol::{
    CatalogCoverage, CatalogGraph, CatalogSnapshot, CatalogSnapshotId, CatalogSnapshotSummary,
};
use uuid::Uuid;

use super::{
    now_text, ConnectionProfileId, MetadataError, MetadataStore, PrincipalId, Result, TenantId,
};

const CATALOG_SNAPSHOT_FORMAT_VERSION: u32 = 1;
const MAX_CATALOG_SNAPSHOTS_PER_TENANT: i64 = 100;
const MAX_CATALOG_SNAPSHOT_BYTES: usize = 32 * 1024 * 1024;
const MAX_CATALOG_SNAPSHOT_RETAINED_BYTES_PER_TENANT: i64 = 256 * 1024 * 1024;
const MAX_DESCRIPTION_BYTES: usize = 1_024;

impl MetadataStore {
    pub fn create_catalog_snapshot(
        &self,
        tenant: TenantId,
        profile: Option<ConnectionProfileId>,
        creator: PrincipalId,
        description: Option<String>,
        graph: &CatalogGraph,
    ) -> Result<CatalogSnapshot> {
        let description = validate_description(description)?;
        let graph_json = serde_json::to_string(graph)?;
        if graph_json.len() > MAX_CATALOG_SNAPSHOT_BYTES {
            return Err(MetadataError::CatalogSnapshotTooLarge {
                limit: MAX_CATALOG_SNAPSHOT_BYTES,
            });
        }
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let (count, retained_bytes): (i64, i64) = tx.query_row(
            "SELECT COUNT(*), COALESCE(SUM(retained_bytes), 0)
             FROM catalog_snapshot WHERE tenant_id = ?1",
            [tenant.0],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let graph_bytes = i64::try_from(graph_json.len()).map_err(|_| {
            MetadataError::CatalogSnapshotTooLarge {
                limit: MAX_CATALOG_SNAPSHOT_BYTES,
            }
        })?;
        if count >= MAX_CATALOG_SNAPSHOTS_PER_TENANT
            || retained_bytes.saturating_add(graph_bytes)
                > MAX_CATALOG_SNAPSHOT_RETAINED_BYTES_PER_TENANT
        {
            return Err(MetadataError::CatalogSnapshotLimitReached);
        }
        let is_member: bool = tx.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM membership WHERE tenant_id = ?1 AND principal_id = ?2
            )",
            params![tenant.0, creator.0],
            |row| row.get(0),
        )?;
        if !is_member {
            return Err(MetadataError::TenantMembershipRequired {
                tenant,
                principal: creator,
            });
        }
        if let Some(profile) = profile {
            let belongs: bool = tx.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM connection_profile WHERE id = ?1 AND tenant_id = ?2
                )",
                params![profile.0, tenant.0],
                |row| row.get(0),
            )?;
            if !belongs {
                return Err(MetadataError::TenantMismatch(profile, tenant));
            }
        }
        let id = CatalogSnapshotId(Uuid::new_v4());
        let coverage_json = serde_json::to_string(&graph.data.coverage)?;
        tx.execute(
            "INSERT INTO catalog_snapshot
             (id, tenant_id, connection_profile_id, creator_principal_id,
              description, graph_json, retained_bytes, source_revision,
              content_digest, coverage_json, format_version, revision, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0, ?12)",
            params![
                id.to_string(),
                tenant.0,
                profile.map(|profile| profile.0),
                creator.0,
                description,
                graph_json,
                graph_bytes,
                i64::try_from(graph.revision.0).map_err(|_| {
                    MetadataError::CatalogSnapshotTooLarge {
                        limit: MAX_CATALOG_SNAPSHOT_BYTES,
                    }
                })?,
                graph.content_digest,
                coverage_json,
                i64::from(CATALOG_SNAPSHOT_FORMAT_VERSION),
                now_text(),
            ],
        )?;
        tx.commit()?;
        drop(conn);
        self.get_catalog_snapshot(tenant, id)
    }

    pub fn get_catalog_snapshot(
        &self,
        tenant: TenantId,
        id: CatalogSnapshotId,
    ) -> Result<CatalogSnapshot> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT tenant_id, connection_profile_id, creator_principal_id,
                    description, graph_json, format_version, revision, created_at
             FROM catalog_snapshot WHERE id = ?1 AND tenant_id = ?2",
            params![id.to_string(), tenant.0],
            |row| {
                let created_at: String = row.get(7)?;
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    created_at,
                ))
            },
        )
        .optional()?
        .ok_or(MetadataError::CatalogSnapshotNotFound)
        .and_then(
            |(
                tenant_id,
                profile_id,
                creator_id,
                description,
                graph_json,
                format,
                revision,
                created,
            )| {
                Ok(CatalogSnapshot {
                    id,
                    tenant_id,
                    connection_profile_id: profile_id,
                    creator_principal_id: creator_id,
                    description,
                    graph: serde_json::from_str(&graph_json)?,
                    format_version: u32::try_from(format).map_err(|_| {
                        MetadataError::InvalidEnum {
                            field: "catalog_snapshot.format_version",
                            value: format.to_string(),
                        }
                    })?,
                    revision: u64::try_from(revision).map_err(|_| MetadataError::InvalidEnum {
                        field: "catalog_snapshot.revision",
                        value: revision.to_string(),
                    })?,
                    created_at: parse_time(created)?,
                })
            },
        )
    }

    pub fn list_catalog_snapshots(
        &self,
        tenant: TenantId,
        limit: u32,
    ) -> Result<Vec<CatalogSnapshotSummary>> {
        let limit = limit.clamp(1, 100);
        let conn = self.conn()?;
        let mut statement = conn.prepare(
            "SELECT id, tenant_id, connection_profile_id, creator_principal_id,
                    description, source_revision, content_digest, coverage_json,
                    retained_bytes, format_version, revision, created_at
             FROM catalog_snapshot WHERE tenant_id = ?1
             ORDER BY created_at DESC, id DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(params![tenant.0, i64::from(limit)], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, i64>(10)?,
                row.get::<_, String>(11)?,
            ))
        })?;
        rows.map(|row| {
            let (
                id,
                tenant_id,
                profile_id,
                creator_id,
                description,
                source_revision,
                content_digest,
                coverage_json,
                retained_bytes,
                format,
                revision,
                created,
            ) = row?;
            Ok(CatalogSnapshotSummary {
                id: CatalogSnapshotId(Uuid::parse_str(&id).map_err(|error| {
                    MetadataError::InvalidEnum {
                        field: "catalog_snapshot.id",
                        value: error.to_string(),
                    }
                })?),
                tenant_id,
                connection_profile_id: profile_id,
                creator_principal_id: creator_id,
                description,
                catalog_revision: sift_protocol::CatalogRevision(
                    u64::try_from(source_revision).map_err(|_| MetadataError::InvalidEnum {
                        field: "catalog_snapshot.source_revision",
                        value: source_revision.to_string(),
                    })?,
                ),
                content_digest,
                coverage: serde_json::from_str::<CatalogCoverage>(&coverage_json)?,
                retained_bytes: u64::try_from(retained_bytes).map_err(|_| {
                    MetadataError::InvalidEnum {
                        field: "catalog_snapshot.retained_bytes",
                        value: retained_bytes.to_string(),
                    }
                })?,
                format_version: u32::try_from(format).map_err(|_| MetadataError::InvalidEnum {
                    field: "catalog_snapshot.format_version",
                    value: format.to_string(),
                })?,
                revision: u64::try_from(revision).map_err(|_| MetadataError::InvalidEnum {
                    field: "catalog_snapshot.revision",
                    value: revision.to_string(),
                })?,
                created_at: parse_time(created)?,
            })
        })
        .collect()
    }

    pub fn delete_catalog_snapshot(
        &self,
        tenant: TenantId,
        id: CatalogSnapshotId,
        expected_revision: u64,
    ) -> Result<()> {
        let conn = self.conn()?;
        let revision = i64::try_from(expected_revision).map_err(|_| {
            MetadataError::CatalogSnapshotRevisionConflict {
                expected: expected_revision,
                current: expected_revision,
            }
        })?;
        let changed = conn.execute(
            "DELETE FROM catalog_snapshot
             WHERE id = ?1 AND tenant_id = ?2 AND revision = ?3",
            params![id.to_string(), tenant.0, revision],
        )?;
        if changed == 1 {
            return Ok(());
        }
        let current = conn
            .query_row(
                "SELECT revision FROM catalog_snapshot WHERE id = ?1 AND tenant_id = ?2",
                params![id.to_string(), tenant.0],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        match current {
            Some(current) => Err(MetadataError::CatalogSnapshotRevisionConflict {
                expected: expected_revision,
                current: u64::try_from(current).unwrap_or(0),
            }),
            None => Err(MetadataError::CatalogSnapshotNotFound),
        }
    }
}

fn validate_description(description: Option<String>) -> Result<Option<String>> {
    match description {
        Some(description)
            if description.is_empty()
                || description.len() > MAX_DESCRIPTION_BYTES
                || description.contains('\0') =>
        {
            Err(MetadataError::InvalidCatalogSnapshotDescription)
        }
        other => Ok(other),
    }
}

fn parse_time(value: String) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|source| MetadataError::InvalidTimestamp { value, source })
}

#[cfg(test)]
mod tests {
    use sift_protocol::{CatalogCoverage, CatalogGraphData, CatalogRevision, ProviderRef};

    use super::*;
    use crate::{MembershipRole, MemorySecretStore, TenantKind};

    #[test]
    fn catalog_snapshots_are_immutable_tenant_scoped_and_revision_guarded() {
        let store =
            MetadataStore::open_in_memory(std::sync::Arc::new(MemorySecretStore::default()))
                .unwrap();
        let principal = store
            .create_principal("owner", "Owner", Some("owner@example.com"))
            .unwrap();
        let tenant = store.create_tenant("Tenant", TenantKind::Team).unwrap();
        store
            .upsert_tenant_membership(tenant.id, principal.id, MembershipRole::Owner)
            .unwrap();
        let graph = CatalogGraph {
            revision: CatalogRevision(1),
            content_digest: "catfp:test".into(),
            invalidation_epoch: 1,
            captured_at: Utc::now(),
            provider: ProviderRef {
                provider_id: sift_protocol::ProviderId::new("test/provider").unwrap(),
                dialect_id: sift_protocol::DialectId::new("test/dialect").unwrap(),
                provider_version: "1".into(),
            },
            database_identity: "dbfp:test".into(),
            data: CatalogGraphData {
                coverage: CatalogCoverage::complete(),
                nodes: Vec::new(),
                edges: Vec::new(),
            },
        };
        let snapshot = store
            .create_catalog_snapshot(
                tenant.id,
                None,
                principal.id,
                Some("baseline".into()),
                &graph,
            )
            .unwrap();
        assert_eq!(snapshot.graph.content_digest, graph.content_digest);
        assert_eq!(
            store.list_catalog_snapshots(tenant.id, 10).unwrap().len(),
            1
        );
        assert!(matches!(
            store.delete_catalog_snapshot(tenant.id, snapshot.id, 1),
            Err(MetadataError::CatalogSnapshotRevisionConflict { current: 0, .. })
        ));
        store
            .delete_catalog_snapshot(tenant.id, snapshot.id, 0)
            .unwrap();
    }
}
