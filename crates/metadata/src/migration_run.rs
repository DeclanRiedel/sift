use rusqlite::{params, OptionalExtension};
use sift_protocol::{
    CatalogRevision, ConnectionId, MigrationPlanId, MigrationRun, MigrationRunId,
    MigrationRunState, MigrationStatementOutcome, SessionId,
};
use uuid::Uuid;

use super::{ConnectionProfileId, MetadataError, MetadataStore, PrincipalId, Result, TenantId};

const MIGRATION_RUN_FORMAT_VERSION: i64 = 1;

impl MetadataStore {
    pub fn put_migration_run(
        &self,
        tenant: TenantId,
        profile: ConnectionProfileId,
        creator: PrincipalId,
        run: &MigrationRun,
    ) -> Result<()> {
        let outcomes = serde_json::to_string(&run.outcomes)?;
        let conn = self.conn()?;
        let valid_scope: bool = conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM connection_profile cp
                JOIN membership m ON m.tenant_id = cp.tenant_id
                WHERE cp.id = ?1 AND cp.tenant_id = ?2 AND m.principal_id = ?3
            )",
            params![profile.0, tenant.0, creator.0],
            |row| row.get(0),
        )?;
        if !valid_scope {
            return Err(MetadataError::TenantMismatch(profile, tenant));
        }
        let changed = conn.execute(
            "INSERT INTO migration_run
             (id, tenant_id, connection_profile_id, creator_principal_id,
              plan_id, plan_digest, session_id, connection_id, state,
              started_at, finished_at, outcomes_json,
              resulting_catalog_revision, format_version)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT(id) DO UPDATE SET
                state = excluded.state,
                finished_at = excluded.finished_at,
                outcomes_json = excluded.outcomes_json,
                resulting_catalog_revision = excluded.resulting_catalog_revision
             WHERE migration_run.tenant_id = excluded.tenant_id
               AND migration_run.connection_profile_id = excluded.connection_profile_id
               AND migration_run.creator_principal_id = excluded.creator_principal_id
               AND migration_run.plan_id = excluded.plan_id
               AND migration_run.plan_digest = excluded.plan_digest
               AND (migration_run.state = 'running'
                    OR migration_run.state = excluded.state)",
            params![
                run.id.to_string(),
                tenant.0,
                profile.0,
                creator.0,
                run.plan_id.to_string(),
                run.plan_digest,
                i64::try_from(run.session.0).map_err(|_| MetadataError::InvalidEnum {
                    field: "migration_run.session_id",
                    value: run.session.0.to_string(),
                })?,
                i64::try_from(run.connection.0).map_err(|_| MetadataError::InvalidEnum {
                    field: "migration_run.connection_id",
                    value: run.connection.0.to_string(),
                })?,
                state_text(run.state),
                run.started_at.to_rfc3339(),
                run.finished_at.map(|value| value.to_rfc3339()),
                outcomes,
                run.resulting_catalog_revision
                    .map(|revision| i64::try_from(revision.0))
                    .transpose()
                    .map_err(|_| MetadataError::InvalidEnum {
                        field: "migration_run.resulting_catalog_revision",
                        value: "overflow".into(),
                    })?,
                MIGRATION_RUN_FORMAT_VERSION,
            ],
        )?;
        if changed == 1 {
            Ok(())
        } else {
            let exists = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM migration_run WHERE id = ?1)",
                params![run.id.to_string()],
                |row| row.get::<_, bool>(0),
            )?;
            if exists {
                Err(MetadataError::MigrationRunTerminal)
            } else {
                Err(MetadataError::MigrationRunNotFound)
            }
        }
    }

    pub fn get_migration_run(&self, tenant: TenantId, id: MigrationRunId) -> Result<MigrationRun> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT plan_id, plan_digest, session_id, connection_id, state,
                    started_at, finished_at, outcomes_json,
                    resulting_catalog_revision
             FROM migration_run WHERE id = ?1 AND tenant_id = ?2",
            params![id.to_string(), tenant.0],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, Option<i64>>(8)?,
                ))
            },
        )
        .optional()?
        .ok_or(MetadataError::MigrationRunNotFound)
        .and_then(
            |(
                plan_id,
                digest,
                session,
                connection,
                state,
                started,
                finished,
                outcomes,
                revision,
            )| {
                Ok(MigrationRun {
                    id,
                    plan_id: MigrationPlanId(Uuid::parse_str(&plan_id).map_err(|error| {
                        MetadataError::InvalidEnum {
                            field: "migration_run.plan_id",
                            value: error.to_string(),
                        }
                    })?),
                    session: SessionId(u64::try_from(session).map_err(|_| {
                        MetadataError::InvalidEnum {
                            field: "migration_run.session_id",
                            value: session.to_string(),
                        }
                    })?),
                    connection: ConnectionId(u64::try_from(connection).map_err(|_| {
                        MetadataError::InvalidEnum {
                            field: "migration_run.connection_id",
                            value: connection.to_string(),
                        }
                    })?),
                    plan_digest: digest,
                    state: parse_state(&state)?,
                    started_at: super::parse_time_sql(started)?,
                    finished_at: finished.map(super::parse_time_sql).transpose()?,
                    outcomes: serde_json::from_str::<Vec<MigrationStatementOutcome>>(&outcomes)?,
                    resulting_catalog_revision: revision
                        .map(|revision| u64::try_from(revision).map(CatalogRevision))
                        .transpose()
                        .map_err(|_| MetadataError::InvalidEnum {
                            field: "migration_run.resulting_catalog_revision",
                            value: "negative".into(),
                        })?,
                })
            },
        )
    }
}

fn state_text(state: MigrationRunState) -> &'static str {
    match state {
        MigrationRunState::Running => "running",
        MigrationRunState::Applied => "applied",
        MigrationRunState::RolledBack => "rolled_back",
        MigrationRunState::Partial => "partial",
        MigrationRunState::Canceled => "canceled",
        MigrationRunState::Failed => "failed",
    }
}

fn parse_state(value: &str) -> Result<MigrationRunState> {
    match value {
        "running" => Ok(MigrationRunState::Running),
        "applied" => Ok(MigrationRunState::Applied),
        "rolled_back" => Ok(MigrationRunState::RolledBack),
        "partial" => Ok(MigrationRunState::Partial),
        "canceled" => Ok(MigrationRunState::Canceled),
        "failed" => Ok(MigrationRunState::Failed),
        value => Err(MetadataError::InvalidEnum {
            field: "migration_run.state",
            value: value.into(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sift_protocol::{Engine, MigrationRunState};

    use super::*;
    use crate::{CredentialMode, MemorySecretStore, NewConnectionProfile};

    #[tokio::test]
    async fn migration_runs_update_in_place_without_persisting_sql() {
        let store = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
        store.bootstrap_local("local user").unwrap();
        let profile = store
            .upsert_connection_profile(
                TenantId(1),
                PrincipalId(1),
                NewConnectionProfile {
                    name: "migration fixture".into(),
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
        let mut run = MigrationRun {
            id: MigrationRunId(Uuid::new_v4()),
            plan_id: MigrationPlanId(Uuid::new_v4()),
            session: SessionId(1),
            connection: ConnectionId(1),
            plan_digest: "migfp:test".into(),
            state: MigrationRunState::Running,
            started_at: chrono::Utc::now(),
            finished_at: None,
            outcomes: Vec::new(),
            resulting_catalog_revision: None,
        };
        store
            .put_migration_run(TenantId(1), profile.id, PrincipalId(1), &run)
            .unwrap();
        run.state = MigrationRunState::Applied;
        run.finished_at = Some(chrono::Utc::now());
        store
            .put_migration_run(TenantId(1), profile.id, PrincipalId(1), &run)
            .unwrap();
        assert_eq!(
            store.get_migration_run(TenantId(1), run.id).unwrap().state,
            MigrationRunState::Applied
        );

        run.state = MigrationRunState::Running;
        run.finished_at = None;
        assert!(matches!(
            store.put_migration_run(TenantId(1), profile.id, PrincipalId(1), &run),
            Err(MetadataError::MigrationRunTerminal)
        ));
        assert_eq!(
            store.get_migration_run(TenantId(1), run.id).unwrap().state,
            MigrationRunState::Applied
        );
    }
}
