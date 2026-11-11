//! Real-engine group settlement evidence for the typed execution prerequisite.
#![cfg(unix)]
use super::*;
use anyhow::{ensure, Result};
use sift_driver_api::Driver;
use sift_protocol::*;
use std::collections::{BTreeMap, HashMap};

// Shared integration helper also has faults used by the HTTP acceptance cases.
#[allow(dead_code)]
#[path = "../tests/support/ai_native_faults.rs"]
mod faults;

async fn native_group_outcomes(engine: Engine, native_error: bool) -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (driver, configuration, credentials): (
        Arc<dyn Driver>,
        serde_json::Value,
        HashMap<String, Vec<u8>>,
    ) = if engine == Engine::Sqlite {
        rusqlite::Connection::open(directory.path().join("groups.db"))?;
        (
            Arc::new(sift_driver_sqlite::SqliteDriver::with_files(
                sift_driver_sqlite::FilePolicy {
                    config: SqliteDriverConfig {
                        roots: BTreeMap::from([(
                            "test".into(),
                            SqliteRootConfig {
                                path: directory.path().to_string_lossy().into(),
                                allowed_tenants: vec![1],
                                read_only: false,
                            },
                        )]),
                        max_connections: 4,
                    },
                    protected: vec![],
                },
            )),
            serde_json::json!({"root_id":"test","path":"groups.db","mode":"read_write"}),
            HashMap::new(),
        )
    } else {
        let prefix = if engine == Engine::Postgres {
            "SIFT_PG"
        } else {
            "SIFT_MSSQL"
        };
        let env = |key: &str| std::env::var(format!("{prefix}_{key}"));
        let mut config = serde_json::json!({"host":env("HOST")?,"port":env("PORT")?.parse::<u16>()?,"database":env("DB")?,"user":env("USER")?,"ssl_mode":"disable"});
        if engine == Engine::SqlServer {
            config["engine_specific"] = serde_json::json!({"engine":"sql_server","mars":false,"trust_server_certificate":true});
        }
        let secrets = env("PASSWORD")
            .ok()
            .map(|password| HashMap::from([("password".into(), password.into_bytes())]))
            .unwrap_or_default();
        let driver: Arc<dyn Driver> = if engine == Engine::Postgres {
            Arc::new(sift_driver_postgres::PgDriver::new())
        } else {
            Arc::new(sift_driver_sqlserver::MssqlDriver::new())
        };
        (driver, config, secrets)
    };
    let metadata =
        MetadataStore::open_in_memory(Arc::new(sift_metadata::MemorySecretStore::new()))?;
    metadata.bootstrap_local("native group owner")?;
    let actor = PrincipalId(1);
    let tenant = sift_metadata::TenantId(1);
    let profile = metadata
        .upsert_connection_profile(
            tenant,
            actor,
            sift_metadata::NewConnectionProfile {
                name: "native group acceptance".into(),
                provider_id: engine.provider_id(),
                semantic_engine: Some(engine),
                configuration: configuration.clone(),
                credentials: None,
                credential_mode: sift_metadata::CredentialMode::Shared,
                tags: vec![],
            },
        )
        .await?;
    let observed = Arc::new(faults::NativeFaults::new(driver));
    let registry = crate::registry::DriverRegistry::new();
    registry
        .providers()
        .replace([Arc::new(crate::registry::BuiltinProviderAdapter::new(
            observed.clone(),
        )) as Arc<dyn crate::registry::DatabaseProvider>])?;
    let sessions = SessionStore::new(registry);
    sessions.set_authorization_store(metadata.clone());
    let session = sessions
        .open_session_with_owner(
            OpenSessionRequest {
                tag: None,
                tenant_id: Some(1),
            },
            Some(actor),
            Some(tenant),
            false,
        )?
        .id;
    let connection = sessions
        .open_managed_connection(
            session,
            engine.provider_id(),
            configuration.clone(),
            credentials.clone(),
            actor,
            tenant,
            profile.id,
            0,
            true,
        )
        .await?
        .id;
    let witness = sessions
        .open_managed_connection(
            session,
            engine.provider_id(),
            configuration,
            credentials,
            actor,
            tenant,
            profile.id,
            0,
            true,
        )
        .await?
        .id;
    let schema = match engine {
        Engine::Postgres => "public",
        Engine::SqlServer => "dbo",
        Engine::Sqlite => "main",
    };
    let prefix = format!("sift_groups_{}", uuid::Uuid::new_v4().simple());
    let committed = format!("{schema}.{prefix}_committed");
    let rolled_back = format!("{schema}.{prefix}_rolled_back");
    let untouched = format!("{schema}.{prefix}_untouched");
    let query = |connection, sql: String| ExecuteRequestHttp {
        connection,
        sql,
        params: vec![],
        tx: None,
        room_id: None,
        connection_profile_id: None,
        transform: None,
        source: None,
    };
    let outcome = async {
        let options = CatalogGraphOptions::default();
        let revision = sessions
            .catalog_graph(
                session,
                connection,
                CatalogGraphRequest {
                    options: options.clone(),
                    refresh: true,
                },
            )
            .await?
            .revision;
        let statement = |ordinal, sql: String| MigrationStatement {
            ordinal,
            fingerprint: format!("sqlfp:{:x}", Sha256::digest(sql.as_bytes())),
            sql,
            change_ids: vec![],
            risk: SchemaChangeRisk::Safe,
        };
        let groups = vec![
            MigrationGroup {
                ordinal: 1,
                transactional: true,
                statements: vec![statement(
                    1,
                    format!("CREATE TABLE {committed} (id INTEGER NOT NULL)"),
                )],
            },
            MigrationGroup {
                ordinal: 2,
                transactional: true,
                statements: vec![
                    statement(
                        1,
                        format!("CREATE TABLE {rolled_back} (id INTEGER NOT NULL)"),
                    ),
                    statement(2, format!("INSERT INTO {rolled_back} VALUES (1)")),
                    statement(3, format!("CREATE TABLE {committed} (id INTEGER NOT NULL)")),
                ],
            },
            MigrationGroup {
                ordinal: 3,
                transactional: true,
                statements: vec![statement(
                    1,
                    format!("CREATE TABLE {untouched} (id INTEGER NOT NULL)"),
                )],
            },
        ];
        let plan = MigrationPlan {
            id: MigrationPlanId(uuid::Uuid::new_v4()),
            run_id: MigrationRunId(uuid::Uuid::new_v4()),
            digest: format!("migfp:{:x}", Sha256::digest(serde_json::to_vec(&groups)?)),
            diff_digest: "native-group-acceptance".into(),
            expected_live_revision: revision,
            groups,
            rollback_groups: vec![],
            required_acknowledgements: vec![],
            warnings: vec![],
            expires_at: chrono::Utc::now() + chrono::Duration::minutes(2),
        };
        sessions.store_migration_plan(
            plan.clone(),
            MigrationPlanScope {
                    review_evidence: None,
                ai_proposal_id: None,
                session,
                connection,
                principal: actor,
                tenant,
                profile: profile.id,
                policy_revision: 0,
                live_options: options,
            },
        )?;
        let request = ApplyMigrationRequest {
            plan_id: plan.id,
            plan_digest: plan.digest.clone(),
            acknowledgements: vec![],
            source: None,
        };
        let gate=observed.clone();
        let guard:DatabaseDispatchGuard=Arc::new(move || {
            let refused=gate.counters().0==3;
            Box::pin(async move {
                if refused {Err(ApiError::Forbidden("native acceptance closes later admission".into()))} else {Ok(())}
            })
        });
        let run = sessions.apply_migration_guarded(session,connection,actor,request.clone(),(!native_error).then_some(guard)).await?;
        ensure!(
            run.state == MigrationRunState::Partial,
            "earlier commit must survive a later rollback"
        );
        let native = metadata.migration_native_outcomes(
            tenant,
            run.id,
            &MigrationNativePageRequest::default(),
        )?;
        ensure!(native.closed && native.journal_present && native.observations.len() == 5);
        let first = &native.observations[0];
        ensure!(
            first.group.outcome == Some(Outcome::Committed) && first.group.transaction_id.is_some()
        );
        let later = native
            .observations
            .iter()
            .filter(|item| item.group.ordinal == 2)
            .collect::<Vec<_>>();
        let rollback_error=observed.rollback_error();
        let aborted=rollback_error.is_some();
        if let Some((_,native_code))=&rollback_error {
            ensure!(engine==Engine::SqlServer && native_error && native_code.as_deref()==Some("3903"),
                "unexpected native rollback failure: {rollback_error:?}");
        }
        let expected=if aborted {Outcome::OutcomeUnknown} else {Outcome::RolledBack};
        ensure!(later.len()==3 && later.iter().all(|item|item.group.outcome==Some(expected) && item.group.rollback_intent),
            "native group evidence: {:?}; driver end calls {:?}, rollback error {:?}",later.iter().map(|item|(&item.group,&item.statement)).collect::<Vec<_>>(),observed.counters(),rollback_error);
        ensure!(observed.counters()==(if native_error {4}else{3},1,1),"each admitted native statement and transaction end must run once");
        ensure!(
            later[0].group.transaction_id.is_some()
                && later[0].group.transaction_id != first.group.transaction_id
        );
        ensure!(later[0]
            .statement
            .as_ref()
            .is_some_and(|s| s.native_admitted == Some(true) && s.native_completed == Some(true)));
        ensure!(
            later[1]
                .statement
                .as_ref()
                .is_some_and(|s| s.affected_rows == Some(1)),
            "real inserted row count must be retained even after rollback"
        );
        let failed=later[2].statement.as_ref().unwrap();
        if native_error {
            ensure!(failed.native_admitted==Some(true) && failed.native_completed==Some(false));
            if engine==Engine::SqlServer {ensure!(failed.result_code.as_deref()==Some("duplicate_object"),"fixed error labels must persist as native tokens before rollback");}
        } else {ensure!(!failed.intent_recorded && failed.native_admitted.is_none(),"closed admission must not claim a native attempt");}
        let usable=sessions.execute_http(session,query(connection,"SELECT 1".into())).await.is_ok();
        ensure!(usable!=aborted,"only an acknowledged owned rollback permits source reuse");
        let last = &native.observations[4];
        ensure!(last.group.transaction_id.is_none() && last.group.outcome.is_none());
        ensure!(last
            .statement
            .as_ref()
            .is_some_and(|s| !s.intent_recorded && s.native_admitted.is_none()));
        let projection = ledger(metadata.clone(), tenant, run.id).await?;
        ensure!(projection.settlement==Settlement::Unknown && projection.entries.len()==if aborted {2}else{3});
        ensure!(projection.entries[0].outcome==Outcome::Committed && projection.entries[1].outcome==expected);
        if !aborted {ensure!(projection.entries[2].operation==ChangeLedgerOperation::MigrationRollback && projection.entries[2].outcome==Outcome::RolledBack);}
        else {ensure!(projection.entries.iter().all(|entry|entry.operation!=ChangeLedgerOperation::MigrationRollback),"a rejected rollback must not invent a rollback entry");}
        ensure!(sessions
            .execute_http(
                session,
                query(witness, format!("SELECT * FROM {committed}"))
            )
            .await?
            .rows
            .is_empty());
        // A successful native catalog query distinguishes actual absence from
        // a failed/closed witness connection. Only the committed table may exist.
        let names=format!("'{prefix}_committed','{prefix}_rolled_back','{prefix}_untouched'");
        let presence=if engine==Engine::Sqlite {
            format!("SELECT name FROM main.sqlite_master WHERE type='table' AND name IN ({names})")
        } else {
            format!("SELECT table_name FROM information_schema.tables WHERE table_schema='{schema}' AND table_name IN ({names})")
        };
        let visible=sessions.execute_http(session,query(witness,presence)).await?;
        ensure!(visible.rows.len()==1 && visible.rows[0].values==vec![Value::Text(format!("{prefix}_committed"))],
            "group 2 CREATE/INSERT must roll back and group 3 must remain undispatched");
        let journal_revision = native.revision;
        let before_replay=observed.counters();
        ensure!(
            sessions
                .apply_migration(session, connection, actor, request)
                .await
                .is_err(),
            "a partial migration cannot be resent"
        );
        ensure!(observed.counters()==before_replay,"partial migration replay must not dispatch native work");
        ensure!(metadata.interrupt_migration_native_journals()? == 0);
        let recovered = metadata.migration_native_outcomes(
            tenant,
            run.id,
            &MigrationNativePageRequest::default(),
        )?;
        ensure!(
            recovered.revision == journal_revision
                && recovered.observations[0].group.outcome == Some(Outcome::Committed)
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    for table in [&committed, &rolled_back, &untouched] {
        let _ = sessions
            .execute_http(
                session,
                query(witness, format!("DROP TABLE IF EXISTS {table}")),
            )
            .await;
    }
    let _ = sessions.close_session(session);
    outcome
}

#[tokio::test]
async fn native_group_outcomes_sqlite() {
    for native_error in [false, true] {
        native_group_outcomes(Engine::Sqlite, native_error)
            .await
            .unwrap();
    }
}
#[cfg(feature = "live-pg")]
#[tokio::test]
async fn native_group_outcomes_postgres() {
    for native_error in [false, true] {
        native_group_outcomes(Engine::Postgres, native_error)
            .await
            .unwrap();
    }
}
#[cfg(feature = "live-mssql")]
#[tokio::test]
async fn native_group_outcomes_sqlserver() {
    for native_error in [false, true] {
        native_group_outcomes(Engine::SqlServer, native_error)
            .await
            .unwrap();
    }
}
