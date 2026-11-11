use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use sift_driver_api::mock::MockDriver;
use sift_metadata::{
    CredentialMode, MemorySecretStore, MetadataStore, NewConnectionProfile, PrincipalId, TenantId,
};
use sift_protocol::{
    CatalogCoverage, CatalogGraph, CatalogGraphOptions, CatalogTree, Code, Engine, ObjectInfo,
    ObjectKind, SchemaDepth, SchemaScope, SchemaSnapshot, SchemaTree,
};
use sift_server::http::{app, AppState, AuthState};
use sift_server::registry::DriverRegistry;
use sift_server::room_runtime::RoomRuntime;
use sift_server::session::SessionStore;
use tower::ServiceExt;

async fn json<T: serde::de::DeserializeOwned>(body: Body) -> T {
    let bytes = to_bytes(body, 4 * 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        panic!(
            "decode response: {error}: {}",
            String::from_utf8_lossy(&bytes)
        )
    })
}

fn post(uri: impl AsRef<str>, body: impl serde::Serialize) -> Request<Body> {
    Request::post(uri.as_ref())
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn get(uri: impl AsRef<str>) -> Request<Body> {
    Request::get(uri.as_ref()).body(Body::empty()).unwrap()
}

fn snapshot(objects: &[&str]) -> SchemaSnapshot {
    let trees = vec![CatalogTree {
        name: "app".into(),
        schemas: vec![SchemaTree {
            name: "public".into(),
            objects: objects
                .iter()
                .map(|name| ObjectInfo::new(*name, ObjectKind::Table))
                .collect(),
        }],
    }];
    SchemaSnapshot {
        graph: Some(sift_core::catalog::graph_from_trees(
            &trees,
            CatalogCoverage::complete(),
            "mock:db",
        )),
        trees,
        fetched_at: chrono::Utc::now(),
        scope: SchemaScope {
            depth: SchemaDepth::Graph {
                options: CatalogGraphOptions::default(),
            },
            filter: None,
        },
        incomplete: false,
    }
}

struct Fixture {
    driver: Arc<MockDriver>,
    router: axum::Router,
    metadata: MetadataStore,
    sessions: SessionStore,
    shutdown: sift_server::shutdown::Shutdown,
    profile: sift_metadata::ConnectionProfileId,
    session: sift_protocol::SessionId,
    connection: sift_protocol::ConnectionId,
    plan: sift_protocol::MigrationPlan,
}

async fn fixture(driver: MockDriver, transactional: bool) -> Fixture {
    let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
    metadata.bootstrap_local("local user").unwrap();
    let profile = metadata
        .upsert_connection_profile(
            TenantId(1),
            PrincipalId(1),
            NewConnectionProfile {
                name: "migration lifecycle".into(),
                provider_id: Engine::Postgres.provider_id(),
                configuration: serde_json::json!({
                    "host": "mock.invalid",
                    "database": "app",
                    "user": "fixture"
                }),
                semantic_engine: Some(Engine::Postgres),
                credentials: None,
                credential_mode: CredentialMode::Shared,
                tags: Vec::new(),
            },
        )
        .await
        .unwrap();
    let driver = Arc::new(driver);
    let registry = DriverRegistry::new();
    registry
        .providers()
        .register_or_replace(Arc::new(
            sift_server::registry::BuiltinProviderAdapter::new(driver.clone()),
        ))
        .unwrap();
    let sessions = SessionStore::new(registry);
    sessions.set_audit_store(metadata.clone());
    let shutdown = sift_server::shutdown::Shutdown::default();
    let router = app(AppState {
        sessions: sessions.clone(),
        rooms: RoomRuntime::default(),
        shutdown: shutdown.clone(),
        auth: AuthState::default(),
        metadata: Some(metadata.clone()),
    });
    let response = router
        .clone()
        .oneshot(post("/v1/sessions", serde_json::json!({})))
        .await
        .unwrap();
    let session: sift_protocol::SessionInfo = json(response.into_body()).await;
    let response = router
        .clone()
        .oneshot(post(
            format!("/v1/sessions/{}/connections/from-profile", session.id),
            serde_json::json!({"tenant_id": 1, "profile_id": profile.id.0}),
        ))
        .await
        .unwrap();
    let connection: sift_protocol::ConnectionInfo = json(response.into_body()).await;
    let response = router
        .clone()
        .oneshot(post(
            format!(
                "/v1/sessions/{}/connections/{}/catalog/graph",
                session.id, connection.id
            ),
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let live: CatalogGraph = json(response.into_body()).await;

    let mut desired = live.clone();
    desired.content_digest = "catfp:desired-lifecycle".into();
    desired.data = snapshot(&["users", "alpha", "beta"]).graph.unwrap();
    let desired = metadata
        .create_catalog_snapshot(
            TenantId(1),
            Some(profile.id),
            PrincipalId(1),
            Some("migration lifecycle target".into()),
            &desired,
        )
        .unwrap();
    let diff_request = serde_json::json!({
        "from": {"kind": "live", "expected_revision": live.revision},
        "to": {"kind": "snapshot", "snapshot_id": desired.id}
    });
    let response = router
        .clone()
        .oneshot(post(
            format!(
                "/v1/sessions/{}/connections/{}/catalog/diffs",
                session.id, connection.id
            ),
            &diff_request,
        ))
        .await
        .unwrap();
    let diff: sift_protocol::SchemaDiff = json(response.into_body()).await;
    let response = router
        .clone()
        .oneshot(post(
            format!(
                "/v1/sessions/{}/connections/{}/catalog/migrations/preview",
                session.id, connection.id
            ),
            serde_json::json!({
                "diff": diff_request,
                "expected_diff_digest": diff.digest,
                "expected_live_revision": live.revision,
                "options": {"prefer_transactional": transactional}
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let plan: sift_protocol::MigrationPlan = json(response.into_body()).await;
    assert_eq!(plan.groups[0].statements.len(), 2);
    Fixture {
        driver,
        router,
        metadata,
        sessions,
        shutdown,
        profile: profile.id,
        session: session.id,
        connection: connection.id,
        plan,
    }
}

async fn apply(fixture: &Fixture) -> axum::response::Response {
    fixture
        .router
        .clone()
        .oneshot(post(
            format!(
                "/v1/sessions/{}/connections/{}/catalog/migrations/apply",
                fixture.session, fixture.connection
            ),
            serde_json::json!({
                "plan_id": fixture.plan.id,
                "plan_digest": fixture.plan.digest
            }),
        ))
        .await
        .unwrap()
}

#[tokio::test]
async fn transactional_failure_rolls_back_but_nontransactional_failure_is_partial() {
    let error = || {
        sift_protocol::DriverError::new(Code::InvalidParameterValue, "private-migration-diagnostic")
    };
    let transactional = fixture(
        MockDriver::builder()
            .engine(Engine::Postgres)
            .schema_ok(snapshot(&["users"]))
            .schema_ok(snapshot(&["users"]))
            .execute_ok(vec![sift_protocol::Page::Done {
                affected_rows: None,
                warnings: Vec::new(),
            }])
            .execute_err(error())
            .build(),
        true,
    )
    .await;
    let response = apply(&transactional).await;
    assert_eq!(response.status(), StatusCode::OK);
    let run: sift_protocol::MigrationRun = json(response.into_body()).await;
    assert_eq!(run.state, sift_protocol::MigrationRunState::RolledBack);
    assert_eq!(
        run.outcomes
            .iter()
            .map(|outcome| outcome.status)
            .collect::<Vec<_>>(),
        vec![
            sift_protocol::MigrationStatementStatus::RolledBack,
            sift_protocol::MigrationStatementStatus::Failed,
        ]
    );

    let nontransactional = fixture(
        MockDriver::builder()
            .engine(Engine::Postgres)
            .schema_ok(snapshot(&["users"]))
            .schema_ok(snapshot(&["users", "alpha"]))
            .execute_ok(vec![sift_protocol::Page::Done {
                affected_rows: None,
                warnings: Vec::new(),
            }])
            .execute_err(error())
            .build(),
        false,
    )
    .await;
    let response = apply(&nontransactional).await;
    assert_eq!(response.status(), StatusCode::OK);
    let run: sift_protocol::MigrationRun = json(response.into_body()).await;
    assert_eq!(run.state, sift_protocol::MigrationRunState::Partial);
    assert_eq!(
        run.outcomes
            .iter()
            .map(|outcome| outcome.status)
            .collect::<Vec<_>>(),
        vec![
            sift_protocol::MigrationStatementStatus::Applied,
            sift_protocol::MigrationStatementStatus::Failed,
        ]
    );
    let durable = nontransactional
        .metadata
        .get_migration_run(TenantId(1), run.id)
        .unwrap();
    assert_eq!(durable.state, sift_protocol::MigrationRunState::Partial);
    for fixture in [&transactional, &nontransactional] {
        let records = fixture.sessions.list_operations();
        let applies = records
            .iter()
            .filter(|row| {
                matches!(
                    row.operation,
                    sift_protocol::Operation::ApplyMigration { .. }
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(applies.len(), 1);
        assert_eq!(applies[0].status, sift_protocol::OperationStatus::Failed);
        assert!(!serde_json::to_string(&records)
            .unwrap()
            .contains("private-migration-diagnostic"));
        let ledger = fixture
            .metadata
            .change_ledger(&sift_protocol::ChangeLedgerFilter::default())
            .unwrap();
        assert!(ledger.entries.iter().all(|entry| entry.row_count.is_none()));
        assert!(!serde_json::to_string(&ledger)
            .unwrap()
            .contains("private-migration-diagnostic"));
    }
}

#[tokio::test]
async fn cancellation_after_nontransactional_work_is_partial_and_marks_rest_skipped() {
    let fixture = fixture(
        MockDriver::builder()
            .engine(Engine::Postgres)
            .schema_ok(snapshot(&["users"]))
            .schema_ok(snapshot(&["users", "alpha"]))
            .execute_delay(Duration::from_millis(200))
            .execute_ok(vec![sift_protocol::Page::Done {
                affected_rows: None,
                warnings: Vec::new(),
            }])
            .execute_ok(vec![sift_protocol::Page::Done {
                affected_rows: None,
                warnings: Vec::new(),
            }])
            .build(),
        false,
    )
    .await;
    let apply_router = fixture.router.clone();
    let apply_request = post(
        format!(
            "/v1/sessions/{}/connections/{}/catalog/migrations/apply",
            fixture.session, fixture.connection
        ),
        serde_json::json!({
            "plan_id": fixture.plan.id,
            "plan_digest": fixture.plan.digest
        }),
    );
    let applying = tokio::spawn(async move { apply_router.oneshot(apply_request).await.unwrap() });

    let run_uri = format!(
        "/v1/sessions/{}/connections/{}/catalog/migrations/runs/{}",
        fixture.session, fixture.connection, fixture.plan.run_id
    );
    for _ in 0..20 {
        let response = fixture.router.clone().oneshot(get(&run_uri)).await.unwrap();
        if response.status() == StatusCode::OK {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    let response = fixture
        .router
        .clone()
        .oneshot(post(format!("{run_uri}/cancel"), serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = applying.await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let run: sift_protocol::MigrationRun = json(response.into_body()).await;
    assert_eq!(run.state, sift_protocol::MigrationRunState::Partial);
    assert_eq!(
        run.outcomes
            .iter()
            .map(|outcome| outcome.status)
            .collect::<Vec<_>>(),
        vec![
            sift_protocol::MigrationStatementStatus::Applied,
            sift_protocol::MigrationStatementStatus::Skipped,
        ]
    );
}

#[tokio::test]
async fn refreshed_catalog_revision_rejects_plan_before_first_statement() {
    let fixture = fixture(
        MockDriver::builder()
            .engine(Engine::Postgres)
            .schema_ok(snapshot(&["users"]))
            .schema_ok(snapshot(&["users", "external_change"]))
            .build(),
        true,
    )
    .await;
    let response = fixture
        .router
        .clone()
        .oneshot(post(
            format!(
                "/v1/sessions/{}/connections/{}/catalog/graph",
                fixture.session, fixture.connection
            ),
            serde_json::json!({"refresh": true}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let refreshed: CatalogGraph = json(response.into_body()).await;
    assert_eq!(refreshed.revision.0, 2);

    let response = apply(&fixture).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(matches!(
        fixture
            .metadata
            .get_migration_run(TenantId(1), fixture.plan.run_id),
        Err(sift_metadata::MetadataError::MigrationRunNotFound)
    ));
}

#[tokio::test]
async fn invalid_digest_does_not_consume_an_otherwise_valid_plan() {
    let fixture = fixture(
        MockDriver::builder()
            .engine(Engine::Postgres)
            .schema_ok(snapshot(&["users"]))
            .schema_ok(snapshot(&["users", "alpha", "beta"]))
            .execute_ok(vec![sift_protocol::Page::Done {
                affected_rows: None,
                warnings: Vec::new(),
            }])
            .execute_ok(vec![sift_protocol::Page::Done {
                affected_rows: None,
                warnings: Vec::new(),
            }])
            .build(),
        true,
    )
    .await;
    let response = fixture
        .router
        .clone()
        .oneshot(post(
            format!(
                "/v1/sessions/{}/connections/{}/catalog/migrations/apply",
                fixture.session, fixture.connection
            ),
            serde_json::json!({
                "plan_id": fixture.plan.id,
                "plan_digest": "migfp:tampered"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let response = apply(&fixture).await;
    assert_eq!(response.status(), StatusCode::OK);
    let run: sift_protocol::MigrationRun = json(response.into_body()).await;
    assert_eq!(run.state, sift_protocol::MigrationRunState::Applied);
}

#[tokio::test]
async fn cancellation_during_final_transactional_statement_rolls_back_before_commit() {
    let first = Arc::new(tokio::sync::Notify::new());
    first.notify_one();
    let last = Arc::new(tokio::sync::Notify::new());
    let fixture = fixture(
        MockDriver::builder()
            .engine(Engine::Postgres)
            .schema_ok(snapshot(&["users"]))
            .schema_ok(snapshot(&["users"]))
            .execute_wait(first)
            .execute_wait(last.clone())
            .execute_ok(vec![sift_protocol::Page::Done {
                affected_rows: None,
                warnings: vec![],
            }])
            .execute_ok(vec![sift_protocol::Page::Done {
                affected_rows: None,
                warnings: vec![],
            }])
            .build(),
        true,
    )
    .await;
    let router = fixture.router.clone();
    let request = post(
        format!(
            "/v1/sessions/{}/connections/{}/catalog/migrations/apply",
            fixture.session, fixture.connection
        ),
        serde_json::json!({"plan_id":fixture.plan.id,"plan_digest":fixture.plan.digest}),
    );
    let applying = tokio::spawn(async move { router.oneshot(request).await.unwrap() });
    let uri = format!(
        "/v1/sessions/{}/connections/{}/catalog/migrations/runs/{}",
        fixture.session, fixture.connection, fixture.plan.run_id
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while fixture
            .driver
            .invocations()
            .iter()
            .filter(|method| **method == "execute")
            .count()
            != 2
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    // Dispatch of the last statement is confirmed, but its native result is
    // held until cancellation has been acknowledged. No timing assumption.
    assert_eq!(
        fixture
            .router
            .clone()
            .oneshot(post(format!("{uri}/cancel"), serde_json::json!({})))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    last.notify_one();
    let response = applying.await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let run: sift_protocol::MigrationRun = json(response.into_body()).await;
    assert_eq!(run.state, sift_protocol::MigrationRunState::Canceled);
    assert_eq!(run.outcomes.len(), 2);
    assert!(run
        .outcomes
        .iter()
        .all(|outcome| outcome.status == sift_protocol::MigrationStatementStatus::RolledBack));
    assert!(run
        .outcomes
        .iter()
        .any(|outcome| outcome.statement_ordinal == 2));
    assert!(!fixture.driver.invocations().contains(&"commit"));
    assert!(fixture.driver.invocations().contains(&"rollback"));
    assert!(fixture
        .sessions
        .list_operations()
        .iter()
        .any(|row| matches!(
            row.operation,
            sift_protocol::Operation::ApplyMigration { .. }
        ) && row.status == sift_protocol::OperationStatus::Failed));
}

fn apply_request(
    fixture: &Fixture,
    source: Option<sift_protocol::VersionedExecutionContext>,
    correlation: &str,
) -> Request<Body> {
    let mut request = post(
        format!(
            "/v1/sessions/{}/connections/{}/catalog/migrations/apply",
            fixture.session, fixture.connection
        ),
        sift_protocol::ApplyMigrationRequest {
            plan_id: fixture.plan.id,
            plan_digest: fixture.plan.digest.clone(),
            acknowledgements: fixture.plan.required_acknowledgements.clone(),
            source,
        },
    );
    request
        .headers_mut()
        .insert("x-correlation-id", correlation.parse().unwrap());
    request
}

#[tokio::test]
async fn migration_source_refuses_foreign_and_stale_workspaces_without_consuming_plan() {
    use sift_metadata::{MembershipRole, NewRoom, RoomKind, TenantKind};
    use sift_protocol::{ChangeLedgerFilter, VersionedExecutionContext};
    let fixture = fixture(
        MockDriver::builder()
            .engine(Engine::Postgres)
            .schema_ok(snapshot(&["users"]))
            .schema_ok(snapshot(&["users", "alpha", "beta"]))
            .execute_ok(vec![sift_protocol::Page::Done {
                affected_rows: Some(1),
                warnings: vec![],
            }])
            .execute_ok(vec![sift_protocol::Page::Done {
                affected_rows: Some(1),
                warnings: vec![],
            }])
            .build(),
        true,
    )
    .await;
    let other = fixture
        .metadata
        .create_tenant("other migration tenant", TenantKind::Team)
        .unwrap();
    fixture
        .metadata
        .upsert_tenant_membership(other.id, PrincipalId(1), MembershipRole::Owner)
        .unwrap();
    let mut sources = Vec::new();
    for tenant in [TenantId(1), other.id] {
        let room = fixture
            .metadata
            .create_room(
                tenant,
                PrincipalId(1),
                NewRoom {
                    name: "migration source".into(),
                    kind: RoomKind::Shared,
                },
            )
            .unwrap();
        let workspace = fixture
            .metadata
            .create_workspace(room.id, PrincipalId(1), "migration workspace")
            .unwrap();
        sources.push(VersionedExecutionContext {
            workspace_id: workspace.id,
            workspace_revision: workspace.revision,
            checkpoint_id: None,
            path: None,
            git_commit: None,
            source_workflow: None,
        });
    }
    let mut stale = sources[0].clone();
    stale.workspace_revision.0 += 1;
    for (source, correlation) in [
        (sources[1].clone(), "migration-foreign"),
        (stale, "migration-stale"),
    ] {
        let response = fixture
            .router
            .clone()
            .oneshot(apply_request(&fixture, Some(source), correlation))
            .await
            .unwrap();
        assert_ne!(response.status(), StatusCode::OK);
        assert!(!fixture.driver.invocations().contains(&"execute"));
        assert!(fixture
            .metadata
            .change_ledger(&ChangeLedgerFilter::default())
            .unwrap()
            .entries
            .is_empty());
    }
    let edited=fixture.metadata.upsert_connection_profile(TenantId(1),PrincipalId(1),NewConnectionProfile {
        name:"migration lifecycle".into(),provider_id:Engine::Postgres.provider_id(),
        configuration:serde_json::json!({"host":"new-host.invalid","database":"changed-after-open","user":"fixture"}),
        semantic_engine:Some(Engine::Postgres),credentials:None,credential_mode:CredentialMode::Shared,tags:vec![],
    }).await.unwrap();
    assert_eq!(edited.id, fixture.profile);
    let response = fixture
        .router
        .clone()
        .oneshot(apply_request(
            &fixture,
            Some(sources[0].clone()),
            "migration-valid",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let run: sift_protocol::MigrationRun = json(response.into_body()).await;
    assert_eq!(run.state, sift_protocol::MigrationRunState::Applied);
    let ledger = fixture
        .metadata
        .change_ledger(&ChangeLedgerFilter::default())
        .unwrap();
    assert_eq!(ledger.entries.len(), 1);
    let entry = &ledger.entries[0];
    assert_eq!(entry.tenant_id, Some(1));
    assert_eq!(entry.connection_profile_id, Some(fixture.profile.0));
    assert_eq!(entry.database_target.as_deref(), Some("app"));
    assert_eq!(entry.workspace_id, Some(sources[0].workspace_id));
    assert_eq!(
        entry.workspace_revision,
        Some(sources[0].workspace_revision)
    );
    assert_eq!(entry.executed_by, 1);
    assert_eq!(entry.correlation_id.as_deref(), Some("migration-valid"));
    let records = fixture.sessions.list_operations();
    let applies = records
        .iter()
        .filter(|record| {
            matches!(
                record.operation,
                sift_protocol::Operation::ApplyMigration { .. }
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(applies.len(), 3);
    assert_eq!(
        applies
            .iter()
            .filter(|row| row.status == sift_protocol::OperationStatus::Failed)
            .count(),
        2
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let audit = fixture.metadata.list_operation_audit(100).unwrap();
            let rows = audit
                .iter()
                .filter(|row| {
                    row.action == "apply"
                        && row.target == "migration"
                        && row
                            .correlation_id
                            .as_deref()
                            .is_some_and(|id| id.starts_with("migration-"))
                })
                .collect::<Vec<_>>();
            if rows.len() == 3 {
                assert!(rows
                    .iter()
                    .all(|row| row.actor_principal_id == Some(PrincipalId(1))));
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn migration_owner_settles_after_http_loss_and_retains_raw_eof_shutdown() {
    use sift_protocol::{ChangeLedgerFilter, ChangeLedgerOutcome, MigrationRunState, Page};
    let first = Arc::new(tokio::sync::Notify::new());
    first.notify_one();
    let gate = Arc::new(tokio::sync::Notify::new());
    let (first_sender, first_rows) = tokio::sync::mpsc::channel(1);
    first_sender
        .send(Page::Done {
            affected_rows: Some(1),
            warnings: vec![],
        })
        .await
        .unwrap();
    drop(first_sender);
    let (native, last_rows) = tokio::sync::mpsc::channel(1);
    let fixture = fixture(
        MockDriver::builder()
            .engine(Engine::Postgres)
            .schema_ok(snapshot(&["users"]))
            .schema_ok(snapshot(&["users", "alpha", "beta"]))
            .execute_wait(first)
            .execute_wait(gate.clone())
            .execute_rows(first_rows)
            .execute_rows(last_rows)
            .build(),
        false,
    )
    .await;
    let router = fixture.router.clone();
    let request = apply_request(&fixture, None, "migration-caller-lost");
    let caller = tokio::spawn(async move { router.oneshot(request).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while fixture
            .driver
            .invocations()
            .iter()
            .filter(|method| **method == "execute")
            .count()
            != 2
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let replay = fixture
        .router
        .clone()
        .oneshot(apply_request(&fixture, None, "migration-replay-running"))
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::BAD_REQUEST);
    assert!(
        fixture
            .metadata
            .change_ledger(&ChangeLedgerFilter::default())
            .unwrap()
            .entries
            .is_empty(),
        "a replay cannot settle the active owner's run"
    );
    caller.abort();
    let _ = caller.await;
    assert_eq!(fixture.shutdown.in_flight(), 1);
    gate.notify_one();
    native
        .send(Page::Done {
            affected_rows: Some(1),
            warnings: vec![],
        })
        .await
        .unwrap();
    assert_eq!(
        fixture
            .shutdown
            .await_drain(Duration::from_millis(10))
            .await,
        1,
        "Done is not raw native EOF and cannot release the settlement owner"
    );
    assert!(fixture
        .metadata
        .change_ledger(&ChangeLedgerFilter::default())
        .unwrap()
        .entries
        .is_empty());
    assert_eq!(
        fixture
            .metadata
            .get_migration_run(TenantId(1), fixture.plan.run_id)
            .unwrap()
            .state,
        MigrationRunState::Running
    );
    let pending = fixture
        .metadata
        .migration_native_outcomes(
            TenantId(1),
            fixture.plan.run_id,
            &sift_protocol::MigrationNativePageRequest::default(),
        )
        .unwrap();
    assert!(pending.journal_present && !pending.closed);
    assert_eq!(
        pending.observations[0]
            .statement
            .as_ref()
            .unwrap()
            .native_completed,
        Some(true)
    );
    assert_ne!(
        pending.observations[1]
            .statement
            .as_ref()
            .unwrap()
            .native_completed,
        Some(true)
    );
    assert!(pending
        .observations
        .iter()
        .all(|entry| entry.group.outcome.is_none()));
    drop(native);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let ledger = fixture
                .metadata
                .change_ledger(&ChangeLedgerFilter::default())
                .unwrap();
            if ledger.entries.len() == 2 {
                for entry in &ledger.entries {
                    assert_eq!(entry.outcome, ChangeLedgerOutcome::Committed);
                    assert_eq!(entry.row_count, Some(1));
                    assert!(entry.transaction_id.is_none());
                    assert_eq!(entry.database_target.as_deref(), Some("app"));
                    assert_eq!(
                        entry.correlation_id.as_deref(),
                        Some("migration-caller-lost")
                    );
                }
                assert_ne!(
                    ledger.entries[0].sql_fingerprint,
                    ledger.entries[1].sql_fingerprint
                );
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let run = fixture
        .metadata
        .get_migration_run(TenantId(1), fixture.plan.run_id)
        .unwrap();
    assert_eq!(run.state, MigrationRunState::Applied);
    let applies = fixture
        .sessions
        .list_operations()
        .into_iter()
        .filter(|row| {
            matches!(
                row.operation,
                sift_protocol::Operation::ApplyMigration { .. }
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(applies.len(), 2);
    assert_eq!(
        applies
            .iter()
            .filter(|row| row.status == sift_protocol::OperationStatus::Succeeded)
            .count(),
        1
    );
    assert_eq!(
        applies
            .iter()
            .filter(|row| row.status == sift_protocol::OperationStatus::Failed)
            .count(),
        1
    );
    assert_eq!(
        fixture
            .driver
            .invocations()
            .iter()
            .filter(|method| **method == "execute")
            .count(),
        2
    );
    assert_eq!(
        fixture.shutdown.await_drain(Duration::from_secs(5)).await,
        0
    );
}
