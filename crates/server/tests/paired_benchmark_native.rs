//! Native interleaving, separate populations and encrypted private context round trips.
use sift_client_sdk::Client;
use sift_metadata::{
    CredentialMode, MemorySecretStore, MetadataStore, NewConnectionProfile, PrincipalId, TenantId,
};
use sift_protocol::*;
use sift_server::{
    http::{app, AppState, AuthState},
    DriverRegistry, SessionStore,
};
use std::sync::Arc;

async fn acceptance(engine: Engine) {
    let directory = tempfile::tempdir().unwrap();
    let registry = match engine {
        Engine::Postgres => DriverRegistry::builder()
            .register(sift_driver_postgres::PgDriver::new())
            .build(),
        Engine::SqlServer => DriverRegistry::builder()
            .register(sift_driver_sqlserver::MssqlDriver::new())
            .build(),
        Engine::Sqlite => {
            rusqlite::Connection::open(directory.path().join("pair.db")).unwrap();
            DriverRegistry::builder()
                .register(sift_driver_sqlite::SqliteDriver::with_files(
                    sift_driver_sqlite::FilePolicy {
                        config: SqliteDriverConfig {
                            roots: std::collections::BTreeMap::from([(
                                "pair".into(),
                                SqliteRootConfig {
                                    path: directory.path().display().to_string(),
                                    allowed_tenants: vec![1],
                                    read_only: false,
                                },
                            )]),
                            max_connections: 4,
                        },
                        protected: vec![],
                    },
                ))
                .build()
        }
    };
    let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
    metadata.bootstrap_local("paired acceptance").unwrap();
    let store = SessionStore::new(registry);
    store.set_audit_store(metadata.clone());
    let session = store
        .open_session_with_owner(
            OpenSessionRequest {
                tag: None,
                tenant_id: Some(1),
            },
            Some(PrincipalId(1)),
            Some(TenantId(1)),
            false,
        )
        .unwrap()
        .id;
    let router = app(AppState {
        sessions: store.clone(),
        rooms: Default::default(),
        auth: AuthState::default(),
        metadata: Some(metadata.clone()),
        shutdown: Default::default(),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = Client::new(format!("http://{}", listener.local_addr().unwrap()));
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let connection = if engine == Engine::Sqlite {
        let profile=metadata.upsert_connection_profile(TenantId(1),PrincipalId(1),NewConnectionProfile {name:"pair".into(),provider_id:engine.provider_id(),semantic_engine:Some(engine),configuration:serde_json::json!({"root_id":"pair","path":"pair.db","mode":"read_write"}),credentials:None,credential_mode:CredentialMode::Shared,tags:vec![]}).await.unwrap();
        client
            .open_connection_from_profile(
                session,
                serde_json::from_value(
                    serde_json::json!({"tenant_id":1,"profile_id":profile.id.0}),
                )
                .unwrap(),
            )
            .await
            .unwrap()
            .id
    } else {
        let pg = engine == Engine::Postgres;
        let prefix = if pg { "SIFT_PG" } else { "SIFT_MSSQL" };
        let get = |key: &str, default: &str| {
            std::env::var(format!("{prefix}_{key}")).unwrap_or_else(|_| default.into())
        };
        store
            .open_connection(
                session,
                engine,
                ConnectionSpec {
                    host: get(
                        "HOST",
                        if pg {
                            "/tmp/sift-demo-pg-socket"
                        } else {
                            "127.0.0.1"
                        },
                    ),
                    port: Some(
                        get("PORT", if pg { "5433" } else { "1433" })
                            .parse()
                            .unwrap(),
                    ),
                    database: Some(get("DB", if pg { "sifttest" } else { "master" })),
                    user: get("USER", if pg { "sift" } else { "sa" }),
                    password: std::env::var(format!("{prefix}_PASSWORD")).ok(),
                    ssl_mode: Some(if pg {
                        SslMode::Disable
                    } else {
                        SslMode::Require
                    }),
                    engine_specific: if pg {
                        None
                    } else {
                        Some(EngineConnectionSpec::SqlServer(MssqlConnectionSpec {
                            trust_server_certificate: Some(true),
                            ..Default::default()
                        }))
                    },
                },
            )
            .await
            .unwrap()
            .id
    };
    let bind = match engine {
        Engine::Postgres => "CAST($1 AS text)",
        Engine::SqlServer => "CAST(@P1 AS nvarchar(128))",
        Engine::Sqlite => "CAST(?1 AS TEXT)",
    };
    for order in [
        BenchmarkPairOrder::Alternating,
        BenchmarkPairOrder::RandomizedV1 { seed: 0 },
    ] {
        let request = BenchmarkPairRequest {
            run_id: uuid::Uuid::new_v4(),
            baseline: BenchmarkWorkload {
                sql: format!("SELECT {bind} WHERE 1=0"),
                params: vec![Value::Text("PRIVATE-PAIR-BIND".into())],
            },
            candidate: BenchmarkWorkload {
                sql: format!("SELECT {bind} UNION ALL SELECT {bind}"),
                params: vec![Value::Text("PRIVATE-PAIR-BIND".into())],
            },
            limits: BenchmarkLimits {
                warmups: 1,
                iterations: 3,
                query_timeout_ms: 1000,
                total_budget_ms: 5000,
                delay_ms: 0,
            },
            order,
            workload_confirmed: true,
        };
        let mut denied = request.clone();
        denied.workload_confirmed = false;
        assert!(client
            .benchmark_pair(session, connection, denied)
            .await
            .is_err());
        let result = client
            .benchmark_pair(session, connection, request.clone())
            .await
            .unwrap();
        let planned = sift_core::performance_pair::schedule(&request.limits, order).unwrap();
        for (report, population, rows) in [
            (&result.baseline, BenchmarkPopulation::Baseline, 0),
            (&result.candidate, BenchmarkPopulation::Candidate, 2),
        ] {
            assert!(report.completed);
            assert_eq!(report.samples.len(), 4);
            assert!(report
                .samples
                .iter()
                .all(|sample| sample.rows == Some(rows)));
            assert!(report.samples[0].warmup && !report.samples[1].warmup);
            assert_eq!(
                sift_core::performance::benchmark_statistics(report).successful,
                3
            );
            let context = report.pair_context.as_ref().unwrap();
            assert_eq!(context.run_id, request.run_id);
            assert_eq!(context.population, population);
            assert_eq!(context.actual_order, planned);
            assert!(context.pair_completed);
            sift_core::performance_pair::validate_context(report).unwrap();
            let saved = client
                .save_benchmark_run(
                    sift_api_types::TenantId(1),
                    &SaveBenchmarkRunRequest {
                        name: "paired population".into(),
                        profile: None,
                        source_context: None,
                        report: report.clone(),
                    },
                )
                .await
                .unwrap();
            let loaded = client
                .saved_benchmark_run(sift_api_types::TenantId(1), saved.id)
                .await
                .unwrap();
            assert_eq!(loaded.report.pair_context, report.pair_context);
            let imported = sift_core::performance::artifact::import(
                sift_core::performance::artifact::export(report, true),
                uuid::Uuid::new_v4(),
            )
            .unwrap();
            assert!(imported.sql.is_empty());
            assert_eq!(imported.pair_context, report.pair_context);
            let restored = client
                .save_benchmark_run(
                    sift_api_types::TenantId(1),
                    &SaveBenchmarkRunRequest {
                        name: "imported paired population".into(),
                        profile: None,
                        source_context: None,
                        report: imported.clone(),
                    },
                )
                .await
                .unwrap();
            assert_eq!(restored.report.pair_context, report.pair_context);
            let mut corrupt = imported;
            corrupt.pair_context.as_mut().unwrap().actual_order[0] =
                corrupt.pair_context.as_ref().unwrap().actual_order[1];
            assert!(client
                .save_benchmark_run(
                    sift_api_types::TenantId(1),
                    &SaveBenchmarkRunRequest {
                        name: "invalid trace".into(),
                        profile: None,
                        source_context: None,
                        report: corrupt
                    }
                )
                .await
                .is_err());
        }
        assert!(
            sift_core::performance::compare_benchmarks(&result.baseline, &result.candidate)
                .delta_ns
                .is_some()
        );
        assert!(!serde_json::to_string(&result)
            .unwrap()
            .contains("PRIVATE-PAIR-BIND"));
        assert_eq!(store.list_connections(session).unwrap().len(), 1);
    }
    let failed = client
        .benchmark_pair(
            session,
            connection,
            BenchmarkPairRequest {
                run_id: uuid::Uuid::new_v4(),
                baseline: BenchmarkWorkload {
                    sql: "SELECT 1".into(),
                    params: vec![],
                },
                candidate: BenchmarkWorkload {
                    sql: "SELECT nonexistent_pair_column".into(),
                    params: vec![],
                },
                limits: BenchmarkLimits {
                    warmups: 0,
                    iterations: 1,
                    query_timeout_ms: 1000,
                    total_budget_ms: 5000,
                    delay_ms: 0,
                },
                order: BenchmarkPairOrder::Alternating,
                workload_confirmed: true,
            },
        )
        .await
        .unwrap();
    assert!(failed.baseline.completed);
    assert_eq!(failed.candidate.samples.len(), 1);
    assert_eq!(
        failed.candidate.samples[0].outcome,
        BenchmarkOutcome::Failed
    );
    assert!(
        !failed
            .baseline
            .pair_context
            .as_ref()
            .unwrap()
            .pair_completed
    );
    assert!(!sift_core::performance_pair::comparison_ready(
        &failed.baseline
    ));
    assert!(
        sift_core::performance::compare_benchmarks(&failed.baseline, &failed.baseline)
            .delta_ns
            .is_none()
    );
    let audit = serde_json::to_string(&store.list_operations()).unwrap();
    assert!(!audit.contains("PRIVATE-PAIR-BIND"));
    assert!(audit.contains("benchmark_pair"));
    store.close_session(session).unwrap();
    server.abort();
}
#[tokio::test]
async fn sqlite_paired_order_populations_and_private_saved_context() {
    acceptance(Engine::Sqlite).await;
}
#[cfg(feature = "live-pg")]
#[tokio::test]
async fn postgres_paired_order_populations_and_private_saved_context() {
    acceptance(Engine::Postgres).await;
}
#[cfg(feature = "live-mssql")]
#[tokio::test]
async fn sqlserver_paired_order_populations_and_private_saved_context() {
    acceptance(Engine::SqlServer).await;
}
