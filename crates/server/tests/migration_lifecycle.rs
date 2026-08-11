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
    router: axum::Router,
    metadata: MetadataStore,
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
    let sessions = SessionStore::new(DriverRegistry::builder().register(driver).build());
    let router = app(AppState {
        sessions,
        rooms: RoomRuntime::default(),
        shutdown: sift_server::shutdown::Shutdown::default(),
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
        router,
        metadata,
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
    let error = || sift_protocol::DriverError::new(Code::InvalidParameterValue, "fixture failure");
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
    // The first execute is now inside its deterministic mock delay. Request
    // cancellation there so the already-running statement completes and the
    // next safe boundary observes the flag.
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
