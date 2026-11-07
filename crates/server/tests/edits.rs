//! HTTP integration tests for the inline-edit endpoints
//! (`/edits/preview`, `/edits/apply`). Boots axum over a `MockDriver` with a
//! canned deep `SchemaSnapshot` and canned execute pages.

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use sift_driver_api::mock::MockDriver;
use sift_protocol::{
    ApplyEditsResult, CatalogTree, ColumnMetadata, EditPlan, Engine, Nullability, ObjectInfo,
    ObjectKind, Page, PrimitiveType, SchemaScope, SchemaSnapshot, SchemaTree, ServerInfo, TypeRef,
};
use sift_server::http::{app, AppState, AuthState};
use sift_server::registry::DriverRegistry;
use sift_server::room_runtime::RoomRuntime;
use sift_server::session::SessionStore;
use tower::ServiceExt;

fn column(name: &str, ty: PrimitiveType, pk: bool, nullable: Nullability) -> ColumnMetadata {
    ColumnMetadata {
        name: name.into(),
        type_ref: TypeRef::Primitive(ty),
        nullable,
        auto_increment: false,
        primary_key: pk,
        facets: Default::default(),
    }
}

fn users(with_pk: bool) -> ObjectInfo {
    let mut o = ObjectInfo::new("users", ObjectKind::Table);
    o.columns = vec![
        column(
            "id",
            PrimitiveType::Int32,
            with_pk,
            Nullability::NotNullable,
        ),
        column("email", PrimitiveType::Text, false, Nullability::Nullable),
    ];
    o
}

fn deep_snapshot(with_pk: bool) -> SchemaSnapshot {
    SchemaSnapshot {
        trees: vec![CatalogTree {
            name: "mock".into(),
            schemas: vec![SchemaTree {
                name: "public".into(),
                objects: vec![users(with_pk)],
            }],
        }],
        fetched_at: chrono::Utc::now(),
        scope: SchemaScope::shallow(),
        incomplete: false,
        graph: None,
    }
}

fn mock_builder() -> sift_driver_api::mock::MockDriverBuilder {
    MockDriver::builder()
        .engine(Engine::Postgres)
        .ping_ok(ServerInfo {
            provider: Engine::Postgres.provider_ref("test"),
            server_version: "MockDB 0.1".into(),
            current_database: "mock".into(),
            current_user: "mock".into(),
            pool_warm_slots: None,
        })
}

fn state_with(driver: MockDriver) -> AppState {
    let registry = DriverRegistry::builder().register(driver).build();
    AppState {
        sessions: SessionStore::new(registry),
        rooms: RoomRuntime::default(),
        shutdown: sift_server::shutdown::Shutdown::default(),
        auth: AuthState::default(),
        metadata: None,
    }
}

fn post_json(uri: String, body: serde_json::Value) -> Request<Body> {
    Request::post(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

async fn body_json<T: serde::de::DeserializeOwned>(body: Body) -> T {
    let bytes = to_bytes(body, 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|e| panic!("decode: {e}; {}", String::from_utf8_lossy(&bytes)))
}

async fn setup(
    driver: MockDriver,
) -> (
    axum::Router,
    sift_protocol::SessionId,
    sift_protocol::ConnectionId,
) {
    let router = app(state_with(driver));
    let res = router
        .clone()
        .oneshot(
            Request::post("/v1/sessions")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let session: sift_protocol::SessionInfo = body_json(res.into_body()).await;
    let sid = session.id;

    let res = router
        .clone()
        .oneshot(post_json(
            format!("/v1/sessions/{sid}/connections"),
            serde_json::json!({
                "provider_id": "sift/postgres",
                "host": "mock.invalid",
                "port": 5432,
                "database": "mock",
                "user": "mock",
                "ssl_mode": "disable",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let conn: sift_protocol::ConnectionInfo = body_json(res.into_body()).await;
    (router, sid, conn.id)
}

fn update_edit_set(cid: sift_protocol::ConnectionId) -> serde_json::Value {
    serde_json::json!({
        "connection": cid,
        "edit_set": {
            "table": {"schema": "public", "name": "users", "kind": "table"},
            "edits": [{
                "kind": "update",
                "key": {"columns": [{"column": "id", "value": {"kind": "int32", "value": 1}}]},
                "changes": [{"column": "email", "value": {"kind": "text", "value": "new@x"}}],
                "expected": [{"column": "email", "value": {"kind": "text", "value": "old@x"}}]
            }]
        }
    })
}

#[tokio::test]
async fn preview_generates_parameterized_dml_without_executing() {
    // Only a schema fetch is canned — no execute. Preview must not touch it.
    let driver = mock_builder().schema_ok(deep_snapshot(true)).build();
    let (router, sid, cid) = setup(driver).await;

    let res = router
        .oneshot(post_json(
            format!("/v1/sessions/{sid}/connections/{cid}/edits/preview"),
            update_edit_set(cid),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let plan: EditPlan = body_json(res.into_body()).await;
    assert_eq!(plan.statements.len(), 1);
    assert_eq!(
        plan.statements[0].sql,
        r#"UPDATE "public"."users" SET "email" = $1 WHERE "id" = $2 AND "email" = $3"#
    );
}

#[tokio::test]
async fn apply_update_commits_and_reports_affected_rows() {
    let driver = mock_builder()
        .schema_ok(deep_snapshot(true))
        .execute_ok(vec![Page::Done {
            affected_rows: Some(1),
            warnings: vec![],
        }])
        .build();
    let (router, sid, cid) = setup(driver).await;

    let res = router
        .oneshot(post_json(
            format!("/v1/sessions/{sid}/connections/{cid}/edits/apply"),
            update_edit_set(cid),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let result: ApplyEditsResult = body_json(res.into_body()).await;
    assert!(result.committed);
    assert_eq!(result.applied.len(), 1);
    assert_eq!(result.applied[0].affected_rows, 1);
}

#[tokio::test]
async fn apply_update_conflict_when_zero_rows_affected() {
    // The row changed under the user: the optimistic WHERE matches nothing.
    let driver = mock_builder()
        .schema_ok(deep_snapshot(true))
        .execute_ok(vec![Page::Done {
            affected_rows: Some(0),
            warnings: vec![],
        }])
        .build();
    let (router, sid, cid) = setup(driver).await;

    let res = router
        .oneshot(post_json(
            format!("/v1/sessions/{sid}/connections/{cid}/edits/apply"),
            update_edit_set(cid),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let error: sift_protocol::ApiErrorResponse = body_json(res.into_body()).await;
    assert_eq!(error.kind, "edit_conflict");
    assert_eq!(
        error.edit_conflict,
        Some(sift_protocol::EditConflict {
            edit_index: 0,
            affected_rows: 0,
            expected_rows: 1,
        })
    );
}

#[tokio::test]
async fn preview_rejects_table_without_row_identity() {
    let driver = mock_builder().schema_ok(deep_snapshot(false)).build();
    let (router, sid, cid) = setup(driver).await;

    let res = router
        .oneshot(post_json(
            format!("/v1/sessions/{sid}/connections/{cid}/edits/preview"),
            update_edit_set(cid),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

struct ManagedEdits {
    router: axum::Router,
    sessions: SessionStore,
    shutdown: sift_server::shutdown::Shutdown,
    metadata: sift_metadata::MetadataStore,
    driver: std::sync::Arc<MockDriver>,
    profile: sift_metadata::ConnectionProfileId,
    session: sift_protocol::SessionId,
    connection: sift_protocol::ConnectionId,
}
async fn managed_edits(driver: MockDriver) -> ManagedEdits {
    use sift_metadata::*;
    use std::sync::Arc;
    let engine = sift_driver_api::Driver::engine(&driver);
    let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
    metadata.bootstrap_local("grid actor").unwrap();
    let profile=metadata.upsert_connection_profile(TenantId(1),PrincipalId(1),NewConnectionProfile {
        name:"grid source".into(),provider_id:engine.provider_id(),
        configuration:serde_json::json!({"host":"mock.invalid","database":"captured-grid","user":"fixture"}),
        semantic_engine:Some(engine),credentials:None,credential_mode:CredentialMode::Shared,tags:vec![],
    }).await.unwrap();
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
        rooms: Default::default(),
        shutdown: shutdown.clone(),
        auth: Default::default(),
        metadata: Some(metadata.clone()),
    });
    let response = router
        .clone()
        .oneshot(post_json("/v1/sessions".into(), serde_json::json!({})))
        .await
        .unwrap();
    let session: sift_protocol::SessionInfo = body_json(response.into_body()).await;
    let response = router
        .clone()
        .oneshot(post_json(
            format!("/v1/sessions/{}/connections/from-profile", session.id),
            serde_json::json!({"tenant_id":1,"profile_id":profile.id.0}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let connection: sift_protocol::ConnectionInfo = body_json(response.into_body()).await;
    ManagedEdits {
        router,
        sessions,
        shutdown,
        metadata,
        driver,
        profile: profile.id,
        session: session.id,
        connection: connection.id,
    }
}
fn grid_request(
    fixture: &ManagedEdits,
    body: serde_json::Value,
    correlation: &str,
) -> Request<Body> {
    let mut request = post_json(
        format!(
            "/v1/sessions/{}/connections/{}/edits/apply",
            fixture.session, fixture.connection
        ),
        body,
    );
    request
        .headers_mut()
        .insert("x-correlation-id", correlation.parse().unwrap());
    request
}
fn grid_ledger(fixture: &ManagedEdits) -> Vec<sift_protocol::ChangeLedgerEntry> {
    fixture
        .metadata
        .change_ledger(&sift_protocol::ChangeLedgerFilter::default())
        .unwrap()
        .entries
}
fn assert_grid_audit(fixture: &ManagedEdits, status: sift_protocol::OperationStatus) {
    let rows = fixture
        .sessions
        .list_operations()
        .into_iter()
        .filter(|row| matches!(row.operation, sift_protocol::Operation::ApplyEdits { .. }))
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, status);
    let data = serde_json::to_string(&(rows, grid_ledger(fixture))).unwrap();
    assert!(!data.contains("private-grid-value"));
    assert!(!data.contains("private-native-diagnostic"));
}

#[tokio::test]
async fn grid_owner_keeps_captured_target_and_settles_after_http_loss_and_native_eof() {
    use sift_metadata::*;
    use sift_protocol::{ChangeLedgerOutcome, OperationStatus};
    use std::time::Duration;
    let (native, rows) = tokio::sync::mpsc::channel(1);
    let fixture = managed_edits(
        mock_builder()
            .schema_ok(deep_snapshot(true))
            .execute_rows(rows)
            .build(),
    )
    .await;
    let changed=fixture.metadata.upsert_connection_profile(TenantId(1),PrincipalId(1),NewConnectionProfile {
        name:"grid source".into(),provider_id:Engine::Postgres.provider_id(),
        configuration:serde_json::json!({"host":"edited.invalid","database":"edited-grid","user":"fixture"}),
        semantic_engine:Some(Engine::Postgres),credentials:None,credential_mode:CredentialMode::Shared,tags:vec![],
    }).await.unwrap();
    assert_eq!(changed.id, fixture.profile);
    let mut body = update_edit_set(fixture.connection);
    body["edit_set"]["edits"][0]["changes"][0]["value"]["value"] = "private-grid-value".into();
    let request = grid_request(&fixture, body, "grid-caller-lost");
    let router = fixture.router.clone();
    let caller = tokio::spawn(async move { router.oneshot(request).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !fixture.driver.invocations().contains(&"execute") {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    caller.abort();
    let _ = caller.await;
    assert_eq!(fixture.shutdown.in_flight(), 1);
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
        1
    );
    assert!(
        !fixture.driver.invocations().contains(&"commit"),
        "owned COMMIT cannot pass original raw EOF"
    );
    assert!(grid_ledger(&fixture).is_empty());
    drop(native);
    tokio::time::timeout(Duration::from_secs(5), async {
        while grid_ledger(&fixture).is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        fixture.shutdown.await_drain(Duration::from_secs(5)).await,
        0
    );
    let rows = grid_ledger(&fixture);
    assert_eq!(rows.len(), 1);
    let entry = &rows[0];
    assert_eq!(entry.outcome, ChangeLedgerOutcome::Committed);
    assert_eq!(entry.database_target.as_deref(), Some("captured-grid"));
    assert_eq!(entry.tenant_id, Some(1));
    assert_eq!(entry.connection_profile_id, Some(fixture.profile.0));
    assert_eq!(entry.executed_by, 1);
    assert_eq!(entry.correlation_id.as_deref(), Some("grid-caller-lost"));
    assert_eq!(entry.row_count, Some(1));
    assert!(entry.transaction_id.is_some());
    assert_grid_audit(&fixture, OperationStatus::Succeeded);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let audit = fixture.metadata.list_operation_audit(100).unwrap();
            if let Some(row) = audit.iter().find(|row| {
                row.action == "apply"
                    && row.target == "edits"
                    && row.correlation_id.as_deref() == Some("grid-caller-lost")
            }) {
                assert_eq!(row.actor_principal_id, Some(PrincipalId(1)));
                assert_eq!(row.status, "succeeded");
                assert!(!serde_json::to_string(&audit)
                    .unwrap()
                    .contains("private-grid-value"));
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn grid_preparation_refusal_and_conflict_do_not_invent_other_kind_changes() {
    use sift_protocol::{ChangeLedgerOperation, ChangeLedgerOutcome, OperationStatus};
    let fixture = managed_edits(
        mock_builder()
            .schema_ok(deep_snapshot(true))
            .execute_ok(vec![Page::Done {
                affected_rows: Some(0),
                warnings: vec![],
            }])
            .build(),
    )
    .await;
    let mut body = update_edit_set(fixture.connection);
    body["connection"] = serde_json::json!(fixture.connection.0 + 100);
    let response = fixture
        .router
        .clone()
        .oneshot(grid_request(&fixture, body, "grid-body-mismatch"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(!fixture.driver.invocations().contains(&"schema"));
    assert!(!fixture.driver.invocations().contains(&"execute"));
    assert!(grid_ledger(&fixture).is_empty());
    assert_grid_audit(&fixture, OperationStatus::Failed);
    let mut body = update_edit_set(fixture.connection);
    body["edit_set"]["edits"].as_array_mut().unwrap().push(serde_json::json!({"kind":"delete","key":{"columns":[{"column":"id","value":{"kind":"int32","value":2}}]}}));
    let response = fixture
        .router
        .clone()
        .oneshot(grid_request(&fixture, body, "grid-conflicted"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let rows = grid_ledger(&fixture);
    assert_eq!(rows.len(), 1);
    // The planner executes deletes first; the later update was never attempted.
    assert_eq!(rows[0].operation, ChangeLedgerOperation::GridDelete);
    assert_eq!(rows[0].outcome, ChangeLedgerOutcome::Conflicted);
    assert_eq!(
        fixture
            .driver
            .invocations()
            .iter()
            .filter(|method| **method == "execute")
            .count(),
        1
    );
    assert!(fixture.driver.invocations().contains(&"rollback"));
    assert!(!fixture.driver.invocations().contains(&"commit"));
}

#[tokio::test]
async fn grid_missing_counts_preserve_transaction_ownership() {
    use sift_protocol::*;
    for caller_owned in [false, true] {
        for count in [None, Some(1)] {
            let fixture = managed_edits(
                mock_builder()
                    .schema_ok(deep_snapshot(true))
                    .execute_ok(vec![Page::Done {
                        affected_rows: count,
                        warnings: vec![],
                    }])
                    .build(),
            )
            .await;
            let mut body = update_edit_set(fixture.connection);
            let tx = if caller_owned {
                let tx = fixture
                    .sessions
                    .begin_transaction(
                        fixture.session,
                        BeginTransactionRequest {
                            connection: fixture.connection,
                            mode: Default::default(),
                        },
                    )
                    .await
                    .unwrap();
                body["tx"] = serde_json::to_value(TxHandleRef {
                    tx_id: tx.tx_id,
                    connection: tx.connection,
                    mode: tx.mode,
                })
                .unwrap();
                Some(tx.tx_id)
            } else {
                None
            };
            let response = fixture
                .router
                .clone()
                .oneshot(grid_request(&fixture, body, "grid-count"))
                .await
                .unwrap();
            if count.is_some() {
                assert_eq!(response.status(), StatusCode::OK);
                let result: ApplyEditsResult = body_json(response.into_body()).await;
                assert_eq!(result.committed, !caller_owned);
                assert_eq!(result.applied[0].affected_rows, 1);
            } else {
                assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
                let error: ApiErrorResponse = body_json(response.into_body()).await;
                assert_eq!(error.kind, "unsupported_result_shape");
                assert!(error.edit_conflict.is_none());
            }
            let rows = grid_ledger(&fixture);
            assert_eq!(rows.len(), 1);
            let expected = match (caller_owned, count) {
                (false, Some(_)) => ChangeLedgerOutcome::Committed,
                (true, Some(_)) => ChangeLedgerOutcome::Partial,
                (false, None) => ChangeLedgerOutcome::RolledBack,
                (true, None) => ChangeLedgerOutcome::OutcomeUnknown,
            };
            assert_eq!(rows[0].outcome, expected);
            assert_eq!(rows[0].row_count, count.map(|n| n as i64));
            assert_grid_audit(
                &fixture,
                if count.is_some() {
                    OperationStatus::Succeeded
                } else {
                    OperationStatus::Failed
                },
            );
            let calls = fixture.driver.invocations();
            assert_eq!(calls.contains(&"commit"), !caller_owned && count.is_some());
            assert_eq!(
                calls.contains(&"rollback"),
                !caller_owned && count.is_none()
            );
            if let Some(tx_id) = tx {
                assert_eq!(rows[0].transaction_id, Some(tx_id.0.to_string()));
                fixture
                    .sessions
                    .rollback_transaction(
                        fixture.session,
                        EndTransactionRequest {
                            connection: fixture.connection,
                            tx_id,
                        },
                    )
                    .await
                    .unwrap();
            }
        }
    }
}

#[tokio::test]
async fn grid_uncertain_terminal_commands_fence_the_source_without_echoing_diagnostics() {
    use sift_protocol::*;
    for commit in [false, true] {
        let builder = mock_builder()
            .schema_ok(deep_snapshot(true))
            .execute_ok(vec![Page::Done {
                affected_rows: Some(u64::from(commit)),
                warnings: vec![],
            }]);
        let error = DriverError::new(Code::ConnectionFailed, "private-native-diagnostic");
        let fixture = managed_edits(
            if commit {
                builder.commit_err(error)
            } else {
                builder.rollback_err(error)
            }
            .build(),
        )
        .await;
        let response = fixture
            .router
            .clone()
            .oneshot(grid_request(
                &fixture,
                update_edit_set(fixture.connection),
                "grid-terminal",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::GONE);
        let rows = grid_ledger(&fixture);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].outcome, ChangeLedgerOutcome::OutcomeUnknown);
        assert_eq!(rows[0].row_count, None);
        assert_grid_audit(&fixture, OperationStatus::Failed);
        assert!(fixture
            .sessions
            .authorize_connection_operation(
                fixture.session,
                fixture.connection,
                OperationKind::ExecuteQuery,
                None,
                &[]
            )
            .is_err());
        if commit {
            assert!(!fixture.driver.invocations().contains(&"rollback"));
        }
    }
}

#[tokio::test]
async fn grid_insert_requires_a_count_or_returned_rows() {
    use sift_protocol::*;
    for returned in [false, true] {
        let mut pages = Vec::new();
        if returned {
            pages.push(Page::Rows {
                rows: vec![Row {
                    values: vec![Value::Int32(3)],
                }],
            });
        }
        pages.push(Page::Done {
            affected_rows: None,
            warnings: vec![],
        });
        let fixture = managed_edits(
            mock_builder()
                .schema_ok(deep_snapshot(true))
                .execute_ok(pages)
                .build(),
        )
        .await;
        let body = serde_json::json!({"connection":fixture.connection,"edit_set":{"table":{"schema":"public","name":"users","kind":"table"},"edits":[{"kind":"insert","values":[{"column":"id","value":{"kind":"int32","value":3}},{"column":"email","value":{"kind":"text","value":"private-grid-value"}}]}]}});
        let response = fixture
            .router
            .clone()
            .oneshot(grid_request(&fixture, body, "grid-insert-count"))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            if returned {
                StatusCode::OK
            } else {
                StatusCode::UNPROCESSABLE_ENTITY
            }
        );
        let entries = grid_ledger(&fixture);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].operation, ChangeLedgerOperation::GridInsert);
        assert_eq!(
            entries[0].outcome,
            if returned {
                ChangeLedgerOutcome::Committed
            } else {
                ChangeLedgerOutcome::RolledBack
            }
        );
        assert_eq!(entries[0].row_count, returned.then_some(1));
        assert_grid_audit(
            &fixture,
            if returned {
                OperationStatus::Succeeded
            } else {
                OperationStatus::Failed
            },
        );
    }
}

fn bulk_request(fixture: &ManagedEdits, correlation: &str) -> Request<Body> {
    let mut request = post_json(
        format!(
            "/v1/sessions/{}/connections/{}/bulk-insert",
            fixture.session, fixture.connection
        ),
        serde_json::json!({"table":"dbo.users","data":b"id,email\n1,private-grid-value\n".to_vec()}),
    );
    request
        .headers_mut()
        .insert("x-correlation-id", correlation.parse().unwrap());
    request
}
fn assert_bulk_audit(fixture: &ManagedEdits, status: sift_protocol::OperationStatus) {
    let rows = fixture
        .sessions
        .list_operations()
        .into_iter()
        .filter(|row| matches!(row.operation, sift_protocol::Operation::BulkInsert { .. }))
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, status);
    let text = serde_json::to_string(&(rows, grid_ledger(fixture))).unwrap();
    assert!(!text.contains("private-grid-value"));
    assert!(!text.contains("private-native-diagnostic"));
}
async fn wait_bulk(fixture: &ManagedEdits) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !fixture.driver.invocations().contains(&"bulk_insert") {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn bulk_owner_retains_captured_target_after_caller_loss_and_late_native_completion() {
    use sift_metadata::*;
    use sift_protocol::{ChangeLedgerOutcome, Engine, OperationStatus};
    use std::{sync::Arc, time::Duration};
    for timeout in [false, true] {
        let gate = Arc::new(tokio::sync::Notify::new());
        let fixture = managed_edits(
            MockDriver::builder()
                .engine(Engine::SqlServer)
                .bulk_insert_wait(gate.clone())
                .bulk_insert_ok(sift_driver_api::BulkResult { rows_inserted: 1 })
                .build(),
        )
        .await;
        let changed=fixture.metadata.upsert_connection_profile(TenantId(1),PrincipalId(1),NewConnectionProfile {name:"grid source".into(),provider_id:Engine::SqlServer.provider_id(),configuration:serde_json::json!({"host":"edited.invalid","database":"edited-bulk","user":"fixture"}),semantic_engine:Some(Engine::SqlServer),credentials:None,credential_mode:CredentialMode::Shared,tags:vec![]}).await.unwrap();
        assert_eq!(changed.id, fixture.profile);
        if timeout {
            fixture
                .sessions
                .set_request_timeout(Duration::from_millis(100));
        }
        let request = bulk_request(&fixture, "bulk-caller-lost");
        let router = fixture.router.clone();
        let caller = tokio::spawn(async move { router.oneshot(request).await });
        wait_bulk(&fixture).await;
        if timeout {
            let response = tokio::time::timeout(Duration::from_secs(5), caller)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
            assert_eq!(
                grid_ledger(&fixture)[0].outcome,
                ChangeLedgerOutcome::OutcomeUnknown
            );
        } else {
            caller.abort();
            let _ = caller.await;
        }
        assert_eq!(
            fixture
                .shutdown
                .await_drain(Duration::from_millis(10))
                .await,
            1
        );
        if !timeout {
            assert!(grid_ledger(&fixture).is_empty());
        }
        gate.notify_one();
        assert_eq!(
            fixture.shutdown.await_drain(Duration::from_secs(5)).await,
            0
        );
        let rows = grid_ledger(&fixture);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].outcome,
            if timeout {
                ChangeLedgerOutcome::OutcomeUnknown
            } else {
                ChangeLedgerOutcome::Committed
            }
        );
        assert_eq!(rows[0].row_count, if timeout { None } else { Some(1) });
        assert_eq!(rows[0].database_target.as_deref(), Some("captured-grid"));
        assert_eq!(rows[0].tenant_id, Some(1));
        assert_eq!(rows[0].connection_profile_id, Some(fixture.profile.0));
        assert_eq!(rows[0].executed_by, 1);
        assert_eq!(rows[0].correlation_id.as_deref(), Some("bulk-caller-lost"));
        assert_bulk_audit(
            &fixture,
            if timeout {
                OperationStatus::Failed
            } else {
                OperationStatus::Succeeded
            },
        );
        if timeout {
            assert!(fixture
                .sessions
                .authorize_connection_operation(
                    fixture.session,
                    fixture.connection,
                    sift_protocol::OperationKind::ExecuteQuery,
                    None,
                    &[]
                )
                .is_err());
        }
    }
}

#[tokio::test]
async fn bulk_refuses_existing_caller_transaction_before_native_dispatch_or_ledger() {
    use sift_protocol::*;
    let fixture = managed_edits(
        MockDriver::builder()
            .engine(Engine::SqlServer)
            .bulk_insert_ok(sift_driver_api::BulkResult { rows_inserted: 1 })
            .build(),
    )
    .await;
    let tx = fixture
        .sessions
        .begin_transaction(
            fixture.session,
            BeginTransactionRequest {
                connection: fixture.connection,
                mode: Default::default(),
            },
        )
        .await
        .unwrap();
    let response = fixture
        .router
        .clone()
        .oneshot(bulk_request(&fixture, "bulk-transaction-conflict"))
        .await
        .unwrap();
    assert!(!response.status().is_success());
    assert!(!fixture.driver.invocations().contains(&"bulk_insert"));
    assert!(!fixture.driver.invocations().contains(&"commit"));
    assert!(!fixture.driver.invocations().contains(&"rollback"));
    assert!(grid_ledger(&fixture).is_empty());
    assert_bulk_audit(&fixture, OperationStatus::Failed);
    fixture
        .sessions
        .rollback_transaction(
            fixture.session,
            EndTransactionRequest {
                connection: fixture.connection,
                tx_id: tx.tx_id,
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn bulk_native_failure_is_unknown_and_its_audit_excludes_diagnostic_echoes() {
    use sift_protocol::*;
    let fixture = managed_edits(
        MockDriver::builder()
            .engine(Engine::SqlServer)
            .bulk_insert_err(DriverError::new(
                Code::ConnectionFailed,
                "private-native-diagnostic",
            ))
            .build(),
    )
    .await;
    let response = fixture
        .router
        .clone()
        .oneshot(bulk_request(&fixture, "bulk-native-failure"))
        .await
        .unwrap();
    assert!(!response.status().is_success());
    assert_eq!(
        grid_ledger(&fixture)[0].outcome,
        ChangeLedgerOutcome::OutcomeUnknown
    );
    assert_bulk_audit(&fixture, OperationStatus::Failed);
}

fn csv_request(
    fixture: &ManagedEdits,
    body: serde_json::Value,
    correlation: &str,
) -> Request<Body> {
    let mut request = post_json(
        format!(
            "/v1/sessions/{}/connections/{}/import/csv",
            fixture.session, fixture.connection
        ),
        body,
    );
    request
        .headers_mut()
        .insert("x-correlation-id", correlation.parse().unwrap());
    request
}
fn csv_body() -> serde_json::Value {
    serde_json::json!({"table":"public.users","data":b"id,email\n1,private-grid-value\n".to_vec()})
}
fn assert_csv_audit(fixture: &ManagedEdits, status: sift_protocol::OperationStatus) {
    let rows = fixture
        .sessions
        .list_operations()
        .into_iter()
        .filter(|row| matches!(row.operation, sift_protocol::Operation::ImportCsv { .. }))
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, status);
    let text = serde_json::to_string(&(rows, grid_ledger(fixture))).unwrap();
    assert!(!text.contains("private-grid-value"));
    assert!(!text.contains("private-native-diagnostic"));
}
#[tokio::test]
async fn csv_owner_retains_native_target_and_lifetime_after_caller_loss_or_timeout() {
    use sift_metadata::*;
    use sift_protocol::*;
    use std::{sync::Arc, time::Duration};
    for timeout in [false, true] {
        let gate = Arc::new(tokio::sync::Notify::new());
        let fixture = managed_edits(
            MockDriver::builder()
                .copy_wait(gate.clone())
                .copy_ok(sift_driver_api::CopyResult {
                    bytes: 1,
                    rows: Some(1),
                    data: vec![],
                })
                .build(),
        )
        .await;
        fixture.metadata.upsert_connection_profile(TenantId(1),PrincipalId(1),NewConnectionProfile {name:"grid source".into(),provider_id:Engine::Postgres.provider_id(),configuration:serde_json::json!({"host":"edited.invalid","database":"edited-csv","user":"fixture"}),semantic_engine:Some(Engine::Postgres),credentials:None,credential_mode:CredentialMode::Shared,tags:vec![]}).await.unwrap();
        if timeout {
            fixture
                .sessions
                .set_request_timeout(Duration::from_millis(100));
        }
        let request = csv_request(&fixture, csv_body(), "csv-owner");
        let router = fixture.router.clone();
        let caller = tokio::spawn(async move { router.oneshot(request).await });
        tokio::time::timeout(Duration::from_secs(5), async {
            while !fixture.driver.invocations().contains(&"copy") {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        if timeout {
            let response = caller.await.unwrap().unwrap();
            assert_eq!(response.status(), StatusCode::GONE);
        } else {
            caller.abort();
            let _ = caller.await;
        }
        assert_eq!(
            fixture
                .shutdown
                .await_drain(Duration::from_millis(10))
                .await,
            1
        );
        gate.notify_one();
        assert_eq!(
            fixture.shutdown.await_drain(Duration::from_secs(5)).await,
            0
        );
        let rows = grid_ledger(&fixture);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].outcome,
            if timeout {
                ChangeLedgerOutcome::OutcomeUnknown
            } else {
                ChangeLedgerOutcome::Committed
            }
        );
        assert_eq!(rows[0].row_count, if timeout { None } else { Some(1) });
        assert_eq!(rows[0].database_target.as_deref(), Some("captured-grid"));
        assert_eq!(rows[0].connection_profile_id, Some(fixture.profile.0));
        assert_eq!(rows[0].executed_by, 1);
        assert_eq!(rows[0].correlation_id.as_deref(), Some("csv-owner"));
        assert_csv_audit(
            &fixture,
            if timeout {
                OperationStatus::Failed
            } else {
                OperationStatus::Succeeded
            },
        );
    }
}
#[tokio::test]
async fn csv_committed_ddl_does_not_inherit_later_copy_failure_or_missing_count() {
    use sift_protocol::*;
    for mode in ["missing", "driver", "rollback-fails"] {
        let builder = MockDriver::builder().execute_ok(vec![Page::Done {
            affected_rows: Some(0),
            warnings: vec![],
        }]);
        let builder = if mode == "missing" {
            builder.copy_ok(sift_driver_api::CopyResult {
                bytes: 1,
                rows: None,
                data: vec![],
            })
        } else {
            builder.copy_err(DriverError::new(
                Code::ConnectionFailed,
                "private-native-diagnostic",
            ))
        };
        let builder = if mode == "rollback-fails" {
            builder.rollback_err(DriverError::new(
                Code::ConnectionFailed,
                "private-native-diagnostic",
            ))
        } else {
            builder
        };
        let fixture = managed_edits(builder.build()).await;
        let mut body = csv_body();
        body["create_table"] = true.into();
        let response = fixture
            .router
            .clone()
            .oneshot(csv_request(&fixture, body, "csv-ddl-data"))
            .await
            .unwrap();
        assert!(!response.status().is_success());
        let rows = grid_ledger(&fixture);
        assert_eq!(rows.len(), 2);
        let ddl = rows
            .iter()
            .find(|row| row.operation == ChangeLedgerOperation::DdlApply)
            .unwrap();
        assert_eq!(ddl.outcome, ChangeLedgerOutcome::Committed);
        assert_eq!(ddl.row_count, None);
        let data = rows
            .iter()
            .find(|row| row.operation == ChangeLedgerOperation::CsvImport)
            .unwrap();
        assert_eq!(
            data.outcome,
            if mode == "rollback-fails" {
                ChangeLedgerOutcome::OutcomeUnknown
            } else {
                ChangeLedgerOutcome::RolledBack
            }
        );
        assert!(data.transaction_id.is_some());
        assert_eq!(ddl.transaction_id, None);
        assert_eq!(ddl.result_code, None);
        assert_eq!(data.row_count, None);
        assert_csv_audit(&fixture, OperationStatus::Failed);
    }
}
#[tokio::test]
async fn csv_preparation_dry_run_and_caller_transaction_refusals_create_no_changes() {
    use sift_protocol::*;
    for mode in ["dry-run", "bad-csv", "caller-tx"] {
        let fixture = managed_edits(MockDriver::builder().build()).await;
        let mut body = csv_body();
        let tx = if mode == "caller-tx" {
            Some(
                fixture
                    .sessions
                    .begin_transaction(
                        fixture.session,
                        BeginTransactionRequest {
                            connection: fixture.connection,
                            mode: Default::default(),
                        },
                    )
                    .await
                    .unwrap(),
            )
        } else {
            None
        };
        if mode == "dry-run" {
            body["dry_run"] = true.into();
        }
        if mode == "bad-csv" {
            body["data"] = serde_json::json!(b"id,id\n1,2\n".to_vec());
        }
        let response = fixture
            .router
            .clone()
            .oneshot(csv_request(&fixture, body, "csv-refusal"))
            .await
            .unwrap();
        assert_eq!(response.status().is_success(), mode == "dry-run");
        assert!(grid_ledger(&fixture).is_empty());
        assert!(!fixture.driver.invocations().contains(&"execute"));
        assert!(!fixture.driver.invocations().contains(&"copy"));
        assert_csv_audit(
            &fixture,
            if mode == "dry-run" {
                OperationStatus::Succeeded
            } else {
                OperationStatus::Failed
            },
        );
        if let Some(tx) = tx {
            fixture
                .sessions
                .rollback_transaction(
                    fixture.session,
                    EndTransactionRequest {
                        connection: fixture.connection,
                        tx_id: tx.tx_id,
                    },
                )
                .await
                .unwrap();
        }
    }
}
#[tokio::test]
async fn csv_skip_does_not_turn_a_missing_native_count_into_a_skipped_row() {
    use sift_protocol::*;
    let fixture = managed_edits(
        MockDriver::builder()
            .schema_ok(deep_snapshot(true))
            .execute_ok(vec![Page::Done {
                affected_rows: None,
                warnings: vec![],
            }])
            .build(),
    )
    .await;
    let mut body = csv_body();
    body["conflict_policy"] = "skip".into();
    let response = fixture
        .router
        .clone()
        .oneshot(csv_request(&fixture, body, "csv-skip-count"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let error: ApiErrorResponse = body_json(response.into_body()).await;
    assert_eq!(error.kind, "unsupported_result_shape");
    assert_eq!(
        grid_ledger(&fixture)[0].outcome,
        ChangeLedgerOutcome::OutcomeUnknown
    );
    assert_csv_audit(&fixture, OperationStatus::Failed);
}

async fn saved_recipe(
    fixture: &ManagedEdits,
    direction: sift_protocol::TransferDirection,
) -> sift_protocol::TransferRecipe {
    use sift_metadata::*;
    use sift_protocol::{TransferDirection, TransferEndpoint};
    let room = fixture
        .metadata
        .create_room(
            TenantId(1),
            PrincipalId(1),
            NewRoom {
                name: "recipe receipts".into(),
                kind: RoomKind::Shared,
            },
        )
        .unwrap();
    let workspace = fixture
        .metadata
        .create_workspace(room.id, PrincipalId(1), "recipe receipts")
        .unwrap();
    fixture
        .metadata
        .create_transfer_recipe(
            workspace.id,
            PrincipalId(1),
            NewTransferRecipe {
                name: "receipt".into(),
                direction,
                source: if direction == TransferDirection::Import {
                    TransferEndpoint::Upload
                } else {
                    TransferEndpoint::Query
                },
                sink: if direction == TransferDirection::Import {
                    TransferEndpoint::Table
                } else {
                    TransferEndpoint::Artifact
                },
                format_id: "csv".into(),
                format_version: "1".into(),
                options: serde_json::json!({}),
            },
        )
        .unwrap()
}
fn recipe_request(
    fixture: &ManagedEdits,
    recipe: &sift_protocol::TransferRecipe,
    body: serde_json::Value,
    correlation: &str,
) -> Request<Body> {
    let mut request = post_json(
        format!("/v1/metadata/transfer-recipes/{}/execute", recipe.id.0),
        body,
    );
    request
        .headers_mut()
        .insert("x-correlation-id", correlation.parse().unwrap());
    let _ = fixture;
    request
}
fn recipe_body(fixture: &ManagedEdits) -> serde_json::Value {
    serde_json::json!({"session_id":fixture.session,"connection_id":fixture.connection,"data":b"id,email\n1,private-grid-value\n".to_vec(),"table":{"schema":"public","name":"users","kind":"table"}})
}
fn assert_recipe_audit(
    fixture: &ManagedEdits,
    recipe: sift_protocol::TransferRecipeId,
    status: sift_protocol::OperationStatus,
) {
    let rows=fixture.sessions.list_operations().into_iter().filter(|row|matches!(row.operation,sift_protocol::Operation::ExecuteTransferRecipe {recipe_id} if recipe_id==recipe)).collect::<Vec<_>>();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, status);
    let text = serde_json::to_string(&(rows, grid_ledger(fixture))).unwrap();
    assert!(!text.contains("private-grid-value"));
    assert!(!text.contains("private-native-diagnostic"));
}
async fn wait_copy(fixture: &ManagedEdits) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !fixture.driver.invocations().contains(&"copy") {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn recipe_import_owns_captured_receipts_after_http_loss() {
    use sift_metadata::*;
    use sift_protocol::*;
    use std::{sync::Arc, time::Duration};
    let gate = Arc::new(tokio::sync::Notify::new());
    let fixture = managed_edits(
        MockDriver::builder()
            .copy_wait(gate.clone())
            .copy_ok(sift_driver_api::CopyResult {
                bytes: 1,
                rows: Some(1),
                data: vec![],
            })
            .build(),
    )
    .await;
    let recipe = saved_recipe(&fixture, TransferDirection::Import).await;
    fixture.metadata.upsert_connection_profile(TenantId(1),PrincipalId(1),NewConnectionProfile {name:"grid source".into(),provider_id:Engine::Postgres.provider_id(),configuration:serde_json::json!({"host":"edited.invalid","database":"edited-recipe","user":"fixture"}),semantic_engine:Some(Engine::Postgres),credentials:None,credential_mode:CredentialMode::Shared,tags:vec![]}).await.unwrap();
    let request = recipe_request(&fixture, &recipe, recipe_body(&fixture), "recipe-lost");
    let router = fixture.router.clone();
    let caller = tokio::spawn(async move { router.oneshot(request).await });
    wait_copy(&fixture).await;
    caller.abort();
    let _ = caller.await;
    assert_eq!(
        fixture
            .shutdown
            .await_drain(Duration::from_millis(10))
            .await,
        1
    );
    assert!(grid_ledger(&fixture).is_empty());
    gate.notify_one();
    assert_eq!(
        fixture.shutdown.await_drain(Duration::from_secs(5)).await,
        0
    );
    let rows = grid_ledger(&fixture);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].outcome, ChangeLedgerOutcome::Committed);
    assert_eq!(rows[0].row_count, Some(1));
    assert!(rows[0].transaction_id.is_some());
    assert_eq!(rows[0].workspace_id, Some(recipe.workspace_id));
    assert_eq!(rows[0].workspace_revision, None);
    assert_eq!(rows[0].database_target.as_deref(), Some("captured-grid"));
    assert_eq!(rows[0].correlation_id.as_deref(), Some("recipe-lost"));
    assert_recipe_audit(&fixture, recipe.id, OperationStatus::Succeeded);
}
#[tokio::test]
async fn recipe_revision_and_access_loss_refuse_commit_but_allow_owned_rollback() {
    use sift_metadata::*;
    use sift_protocol::*;
    use std::sync::Arc;
    for deleted in [false, true] {
        let gate = Arc::new(tokio::sync::Notify::new());
        let fixture = managed_edits(
            MockDriver::builder()
                .copy_wait(gate.clone())
                .copy_ok(sift_driver_api::CopyResult {
                    bytes: 1,
                    rows: Some(1),
                    data: vec![],
                })
                .build(),
        )
        .await;
        let recipe = saved_recipe(&fixture, TransferDirection::Import).await;
        let request = recipe_request(&fixture, &recipe, recipe_body(&fixture), "recipe-revoked");
        let router = fixture.router.clone();
        let caller = tokio::spawn(async move { router.oneshot(request).await });
        wait_copy(&fixture).await;
        if deleted {
            let workspace = fixture
                .metadata
                .get_workspace_for_principal(recipe.workspace_id, PrincipalId(1), false)
                .unwrap();
            fixture
                .metadata
                .delete_workspace(recipe.workspace_id, PrincipalId(1), workspace.revision)
                .unwrap();
        } else {
            fixture
                .metadata
                .update_transfer_recipe(
                    recipe.id,
                    PrincipalId(1),
                    recipe.revision,
                    NewTransferRecipe {
                        name: "changed".into(),
                        direction: recipe.direction,
                        source: recipe.source,
                        sink: recipe.sink,
                        format_id: recipe.format_id.clone(),
                        format_version: recipe.format_version.clone(),
                        options: recipe.options.clone(),
                    },
                )
                .unwrap();
        }
        gate.notify_one();
        let response = caller.await.unwrap().unwrap();
        assert!(!response.status().is_success());
        assert!(fixture.driver.invocations().contains(&"rollback"));
        assert!(!fixture.driver.invocations().contains(&"commit"));
        let rows = grid_ledger(&fixture);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].outcome, ChangeLedgerOutcome::RolledBack);
        assert!(fixture
            .sessions
            .list_transactions(fixture.session)
            .unwrap()
            .is_empty());
        assert!(fixture
            .sessions
            .authorize_connection_operation(
                fixture.session,
                fixture.connection,
                OperationKind::ExecuteQuery,
                None,
                &[]
            )
            .is_ok());
        assert_recipe_audit(&fixture, recipe.id, OperationStatus::Failed);
    }
}
#[tokio::test]
async fn recipe_auxiliary_failure_preserves_confirmed_import_commit() {
    use sift_metadata::*;
    use sift_protocol::*;
    use std::time::Duration;
    let (native, rows) = tokio::sync::mpsc::channel(1);
    // Native receiver scripts take priority over execute_ok in MockDriver.
    // Supply both rows through that queue so only the second row is held.
    let (first, first_rows) = tokio::sync::mpsc::channel(1);
    first
        .send(Page::Done {
            affected_rows: Some(1),
            warnings: vec![],
        })
        .await
        .unwrap();
    drop(first);
    let fixture = managed_edits(
        MockDriver::builder()
            .schema_ok(deep_snapshot(true))
            .execute_rows(first_rows)
            .execute_rows(rows)
            .build(),
    )
    .await;
    let recipe = saved_recipe(&fixture, TransferDirection::Import).await;
    let mut body = recipe_body(&fixture);
    body["data"] = serde_json::json!(b"id,email\n1,kept\n2,private-grid-value\n".to_vec());
    body["conflict_policy"] = "quarantine".into();
    let request = recipe_request(&fixture, &recipe, body, "recipe-aux-failure");
    let router = fixture.router.clone();
    let caller = tokio::spawn(async move { router.oneshot(request).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while fixture
            .driver
            .invocations()
            .iter()
            .filter(|entry| **entry == "execute")
            .count()
            < 2
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let workspace = fixture
        .metadata
        .get_workspace_for_principal(recipe.workspace_id, PrincipalId(1), false)
        .unwrap();
    fixture
        .metadata
        .delete_workspace(recipe.workspace_id, PrincipalId(1), workspace.revision)
        .unwrap();
    native
        .send(Page::Error {
            error: DriverError::new(Code::InvalidParameterValue, "private-native-diagnostic"),
        })
        .await
        .unwrap();
    drop(native);
    let response = caller.await.unwrap().unwrap();
    assert!(!response.status().is_success());
    let rows = grid_ledger(&fixture);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].outcome, ChangeLedgerOutcome::Committed);
    assert_eq!(rows[0].row_count, Some(1));
    assert_eq!(rows[0].result_code, None);
    assert_recipe_audit(&fixture, recipe.id, OperationStatus::Failed);
}
#[tokio::test]
async fn recipe_export_survives_http_loss_and_retains_native_eof_tracking() {
    use sift_protocol::*;
    use std::time::Duration;
    let (native, rows) = tokio::sync::mpsc::channel(3);
    let fixture = managed_edits(MockDriver::builder().execute_rows(rows).build()).await;
    let recipe = saved_recipe(&fixture, TransferDirection::Export).await;
    let body = serde_json::json!({"session_id":fixture.session,"connection_id":fixture.connection,"sql":"SELECT 'private-grid-value'"});
    let request = recipe_request(&fixture, &recipe, body, "recipe-export-lost");
    let router = fixture.router.clone();
    let caller = tokio::spawn(async move { router.oneshot(request).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !fixture.driver.invocations().contains(&"execute") {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    caller.abort();
    let _ = caller.await;
    native
        .send(Page::NextResult {
            columns: vec![ColumnMetadata::new(
                "value",
                TypeRef::Primitive(PrimitiveType::Text),
            )],
        })
        .await
        .unwrap();
    native
        .send(Page::Rows {
            rows: vec![Row {
                values: vec![Value::Text("private-grid-value".into())],
            }],
        })
        .await
        .unwrap();
    native
        .send(Page::Done {
            affected_rows: None,
            warnings: vec![],
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5),async {while !fixture.sessions.list_operations().iter().any(|row|matches!(row.operation,Operation::ExecuteTransferRecipe {recipe_id} if recipe_id==recipe.id)) {tokio::task::yield_now().await;}}).await.unwrap();
    assert_eq!(
        fixture
            .shutdown
            .await_drain(Duration::from_millis(10))
            .await,
        1
    );
    drop(native);
    assert_eq!(
        fixture.shutdown.await_drain(Duration::from_secs(5)).await,
        0
    );
    assert!(grid_ledger(&fixture).is_empty());
    assert_recipe_audit(&fixture, recipe.id, OperationStatus::Succeeded);
}

fn recipe_parquet_bytes() -> Vec<u8> {
    use arrow_array::{ArrayRef, Int64Array, RecordBatch, StringArray};
    use arrow_schema::{DataType, Field, Schema};
    use std::sync::Arc;
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("email", DataType::Utf8, false),
    ]));
    let arrays: Vec<ArrayRef> = vec![
        Arc::new(Int64Array::from(vec![1])),
        Arc::new(StringArray::from(vec!["private-grid-value"])),
    ];
    let batch = RecordBatch::try_new(schema.clone(), arrays).unwrap();
    let mut writer = parquet::arrow::ArrowWriter::try_new(Vec::new(), schema, None).unwrap();
    writer.write(&batch).unwrap();
    writer.into_inner().unwrap()
}
#[tokio::test]
async fn recipe_parquet_tracks_observed_counts_and_both_transaction_phases() {
    use sift_metadata::*;
    use sift_protocol::*;
    for mode in ["known", "missing", "rollback-fails", "commit-fails"] {
        let count = (mode == "known" || mode == "commit-fails").then_some(1);
        let builder = MockDriver::builder()
            .execute_ok(vec![Page::Done {
                affected_rows: Some(0),
                warnings: vec![],
            }])
            .execute_ok(vec![Page::Done {
                affected_rows: count,
                warnings: vec![],
            }]);
        let builder = match mode {
            "rollback-fails" => builder.rollback_err(DriverError::new(
                Code::ConnectionFailed,
                "private-native-diagnostic",
            )),
            "commit-fails" => builder.commit_err(DriverError::new(
                Code::ConnectionFailed,
                "private-native-diagnostic",
            )),
            _ => builder,
        };
        let fixture = managed_edits(builder.build()).await;
        let recipe = saved_recipe(&fixture, TransferDirection::Import).await;
        let recipe = fixture
            .metadata
            .update_transfer_recipe(
                recipe.id,
                PrincipalId(1),
                recipe.revision,
                NewTransferRecipe {
                    name: recipe.name,
                    direction: recipe.direction,
                    source: recipe.source,
                    sink: recipe.sink,
                    format_id: "parquet".into(),
                    format_version: "2".into(),
                    options: serde_json::json!({}),
                },
            )
            .unwrap();
        let mut body = recipe_body(&fixture);
        body["data"] = serde_json::json!(recipe_parquet_bytes());
        body["create_table"] = true.into();
        let response = fixture
            .router
            .clone()
            .oneshot(recipe_request(&fixture, &recipe, body, "recipe-parquet"))
            .await
            .unwrap();
        assert_eq!(response.status().is_success(), mode == "known");
        let expected = match mode {
            "known" => ChangeLedgerOutcome::Committed,
            "missing" => ChangeLedgerOutcome::RolledBack,
            _ => ChangeLedgerOutcome::OutcomeUnknown,
        };
        let rows = grid_ledger(&fixture);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row.outcome == expected
            && row.transaction_id.is_some()
            && row.workspace_id == Some(recipe.workspace_id)));
        let data = rows
            .iter()
            .find(|row| row.operation == ChangeLedgerOperation::CsvImport)
            .unwrap();
        assert_eq!(data.row_count, (mode == "known").then_some(1));
        assert_recipe_audit(
            &fixture,
            recipe.id,
            if mode == "known" {
                OperationStatus::Succeeded
            } else {
                OperationStatus::Failed
            },
        );
    }
}
#[tokio::test]
async fn recipe_dry_run_and_export_failure_have_one_truthful_terminal_audit() {
    use sift_protocol::*;
    for export in [false, true] {
        let fixture = managed_edits(
            MockDriver::builder()
                .execute_err(DriverError::new(
                    Code::ConnectionFailed,
                    "private-native-diagnostic",
                ))
                .build(),
        )
        .await;
        let recipe = saved_recipe(
            &fixture,
            if export {
                TransferDirection::Export
            } else {
                TransferDirection::Import
            },
        )
        .await;
        let mut body = recipe_body(&fixture);
        if export {
            body["sql"] = "SELECT 'private-grid-value'".into();
        } else {
            body["dry_run"] = true.into();
        }
        let response = fixture
            .router
            .clone()
            .oneshot(recipe_request(&fixture, &recipe, body, "recipe-terminal"))
            .await
            .unwrap();
        assert_eq!(response.status().is_success(), !export);
        assert!(grid_ledger(&fixture).is_empty());
        assert_recipe_audit(
            &fixture,
            recipe.id,
            if export {
                OperationStatus::Failed
            } else {
                OperationStatus::Succeeded
            },
        );
    }
}

#[tokio::test]
async fn recipe_refuses_cross_tenant_source_even_when_actor_owns_both() {
    use sift_metadata::*;
    use sift_protocol::*;
    let fixture = managed_edits(MockDriver::builder().build()).await;
    let tenant = fixture
        .metadata
        .create_tenant("other recipe tenant", TenantKind::Team)
        .unwrap();
    fixture
        .metadata
        .upsert_tenant_membership(tenant.id, PrincipalId(1), MembershipRole::Owner)
        .unwrap();
    let room = fixture
        .metadata
        .create_room(
            tenant.id,
            PrincipalId(1),
            NewRoom {
                name: "other recipe".into(),
                kind: RoomKind::Shared,
            },
        )
        .unwrap();
    let workspace = fixture
        .metadata
        .create_workspace(room.id, PrincipalId(1), "other recipe")
        .unwrap();
    let recipe = fixture
        .metadata
        .create_transfer_recipe(
            workspace.id,
            PrincipalId(1),
            NewTransferRecipe {
                name: "other recipe".into(),
                direction: TransferDirection::Import,
                source: TransferEndpoint::Upload,
                sink: TransferEndpoint::Table,
                format_id: "csv".into(),
                format_version: "1".into(),
                options: serde_json::json!({}),
            },
        )
        .unwrap();
    let response = fixture
        .router
        .clone()
        .oneshot(recipe_request(
            &fixture,
            &recipe,
            recipe_body(&fixture),
            "recipe-cross-tenant",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(grid_ledger(&fixture).is_empty());
    assert!(!fixture.driver.invocations().contains(&"copy"));
    assert!(!fixture.driver.invocations().contains(&"execute"));
    assert_recipe_audit(&fixture, recipe.id, OperationStatus::Failed);
}

fn export_request(fixture: &ManagedEdits, sql: &str, parquet: bool) -> Request<Body> {
    let mut request = post_json(
        format!(
            "/v1/sessions/{}/connections/{}/export",
            fixture.session, fixture.connection
        ),
        serde_json::json!({"sql":sql,"params":[],"format":if parquet {"parquet"} else {"csv"},"header":false}),
    );
    request
        .headers_mut()
        .insert("x-correlation-id", "export-native-capture".parse().unwrap());
    request
}
#[tokio::test]
async fn export_native_receipts_are_independent_of_delivery_and_all_native_results() {
    use sift_metadata::*;
    use sift_protocol::*;
    use std::time::Duration;
    for case in [
        "known",
        "missing_count",
        "late_error",
        "missing_done",
        "overflow",
        "body_drop",
        "encoder_failure",
        "mixed",
        "cte",
        "unknown",
        "select_into",
        "opaque_error",
    ] {
        let (native, rows) = tokio::sync::mpsc::channel(4);
        let fixture = managed_edits(MockDriver::builder().execute_rows(rows).build()).await;
        fixture.metadata.upsert_connection_profile(TenantId(1), PrincipalId(1), NewConnectionProfile {
            name:"grid source".into(),provider_id:Engine::Postgres.provider_id(),
            configuration:serde_json::json!({"host":"edited.invalid","database":"edited-export","user":"fixture"}),
            semantic_engine:Some(Engine::Postgres),credentials:None,credential_mode:CredentialMode::Shared,tags:vec![],
        }).await.unwrap();
        let sql = match case {
            "mixed" => "INSERT INTO public.users VALUES (1); SELECT 'private-grid-value'",
            "cte" => "WITH changed AS (DELETE FROM public.users RETURNING *) SELECT * FROM changed",
            "unknown" => "EXECUTE opaque_private_procedure",
            "select_into" => "SELECT 1 AS id INTO public.export_created",
            _ => "/* private-grid-value */ INSERT INTO public.users VALUES (1) RETURNING id",
        };
        let response = fixture
            .router
            .clone()
            .oneshot(export_request(&fixture, sql, case == "encoder_failure"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{case}");
        let mut body = if case == "body_drop" {
            drop(response);
            None
        } else {
            Some(tokio::spawn(async move {
                to_bytes(response.into_body(), 4096).await
            }))
        };
        if case == "encoder_failure" {
            native
                .send(Page::NextResult { columns: vec![] })
                .await
                .unwrap();
            assert!(body.take().unwrap().await.unwrap().is_err());
        }
        if !matches!(case, "missing_done" | "opaque_error") {
            native
                .send(Page::Done {
                    affected_rows: if case == "missing_count" {
                        None
                    } else if case == "overflow" {
                        Some(u64::MAX)
                    } else {
                        Some(1)
                    },
                    warnings: vec![],
                })
                .await
                .unwrap();
        }
        if case == "opaque_error" {
            native
                .send(Page::Error {
                    error: DriverError::new(
                        Code::Other {
                            message: "private-native-diagnostic".into(),
                        },
                        "private-native-diagnostic",
                    ),
                })
                .await
                .unwrap();
        }
        if case == "late_error" {
            native
                .send(Page::Error {
                    error: DriverError::new(Code::ConnectionFailed, "private-native-diagnostic"),
                })
                .await
                .unwrap();
        }
        if case == "overflow" {
            native
                .send(Page::Done {
                    affected_rows: Some(1),
                    warnings: vec![],
                })
                .await
                .unwrap();
        }
        if case == "opaque_error" {
            let error = body.take().unwrap().await.unwrap().unwrap_err();
            assert!(!error.to_string().contains("private-native-diagnostic"));
        }
        assert!(
            grid_ledger(&fixture).is_empty(),
            "{case}: native EOF is required"
        );
        drop(native);
        assert_eq!(
            fixture.shutdown.await_drain(Duration::from_secs(5)).await,
            0,
            "{case}"
        );
        let ledger = grid_ledger(&fixture);
        assert_eq!(ledger.len(), 1, "{case}");
        let entry = &ledger[0];
        let committed = !matches!(
            case,
            "late_error"
                | "missing_done"
                | "mixed"
                | "cte"
                | "unknown"
                | "select_into"
                | "opaque_error"
        );
        assert_eq!(
            entry.outcome,
            if committed {
                ChangeLedgerOutcome::Committed
            } else {
                ChangeLedgerOutcome::OutcomeUnknown
            },
            "{case}"
        );
        assert_eq!(
            entry.row_count,
            if matches!(
                case,
                "missing_count"
                    | "late_error"
                    | "missing_done"
                    | "overflow"
                    | "mixed"
                    | "cte"
                    | "unknown"
                    | "select_into"
                    | "opaque_error"
            ) {
                None
            } else {
                Some(1)
            },
            "{case}"
        );
        if case == "opaque_error" {
            assert_eq!(entry.result_code.as_deref(), Some("other"));
        }
        assert_eq!(entry.database_target.as_deref(), Some("captured-grid"));
        assert_eq!(entry.connection_profile_id, Some(fixture.profile.0));
        assert_eq!(entry.tenant_id, Some(1));
        assert_eq!(entry.executed_by, 1);
        assert_eq!(
            entry.correlation_id.as_deref(),
            Some("export-native-capture")
        );
        assert!(entry.workspace_id.is_none() && entry.transaction_id.is_none());
        assert!(entry
            .sql_fingerprint
            .as_ref()
            .unwrap()
            .starts_with("sqlfp:"));
        assert_eq!(entry.source_workflow, "direct_export");
        let encoded = serde_json::to_string(&(ledger, fixture.sessions.list_operations())).unwrap();
        assert!(
            !encoded.contains("private-grid-value")
                && !encoded.contains("private-native-diagnostic")
        );
    }
}

#[tokio::test]
async fn recipe_export_write_receipts_outlive_artifact_success_or_failure() {
    use sift_metadata::PrincipalId;
    use sift_protocol::*;
    use std::time::Duration;
    for failure in [false, true] {
        let (native, rows) = tokio::sync::mpsc::channel(4);
        let fixture = managed_edits(MockDriver::builder().execute_rows(rows).build()).await;
        let recipe = saved_recipe(&fixture, TransferDirection::Export).await;
        let mut body = recipe_body(&fixture);
        body["sql"] = "INSERT INTO public.users VALUES (1) RETURNING id".into();
        let request = recipe_request(&fixture, &recipe, body, "recipe-export-native");
        let router = fixture.router.clone();
        let caller = tokio::spawn(async move { router.oneshot(request).await });
        tokio::time::timeout(Duration::from_secs(5), async {
            while !fixture.driver.invocations().contains(&"execute") {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        if failure {
            let workspace = fixture
                .metadata
                .get_workspace_for_principal(recipe.workspace_id, PrincipalId(1), false)
                .unwrap();
            fixture
                .metadata
                .delete_workspace(recipe.workspace_id, PrincipalId(1), workspace.revision)
                .unwrap();
        }
        native
            .send(Page::NextResult {
                columns: vec![ColumnMetadata::new(
                    "id",
                    TypeRef::Primitive(PrimitiveType::Int64),
                )],
            })
            .await
            .unwrap();
        native
            .send(Page::Rows {
                rows: vec![Row::new(vec![Value::Int64(1)])],
            })
            .await
            .unwrap();
        native
            .send(Page::Done {
                affected_rows: Some(1),
                warnings: vec![],
            })
            .await
            .unwrap();
        let response = caller.await.unwrap().unwrap();
        assert_eq!(response.status().is_success(), !failure);
        assert!(grid_ledger(&fixture).is_empty());
        drop(native);
        assert_eq!(
            fixture.shutdown.await_drain(Duration::from_secs(5)).await,
            0
        );
        let ledger = grid_ledger(&fixture);
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger[0].outcome, ChangeLedgerOutcome::Committed);
        assert_eq!(ledger[0].row_count, Some(1));
        assert_eq!(ledger[0].workspace_id, Some(recipe.workspace_id));
        assert!(ledger[0].workspace_revision.is_none());
        assert_eq!(
            ledger[0].correlation_id.as_deref(),
            Some("recipe-export-native")
        );
        assert_eq!(ledger[0].source_workflow, "transfer_recipe_export");
        assert_recipe_audit(
            &fixture,
            recipe.id,
            if failure {
                OperationStatus::Failed
            } else {
                OperationStatus::Succeeded
            },
        );
    }
}

#[tokio::test]
async fn export_refuses_a_caller_transaction_without_a_mutation_receipt() {
    use sift_protocol::*;
    let fixture = managed_edits(MockDriver::builder().build()).await;
    let transaction = fixture
        .sessions
        .begin_transaction(
            fixture.session,
            BeginTransactionRequest {
                connection: fixture.connection,
                mode: TxMode::default(),
            },
        )
        .await
        .unwrap();
    let response = fixture
        .router
        .clone()
        .oneshot(export_request(
            &fixture,
            "INSERT INTO users VALUES (1)",
            false,
        ))
        .await
        .unwrap();
    assert!(!response.status().is_success());
    assert!(grid_ledger(&fixture).is_empty());
    assert!(!fixture.driver.invocations().contains(&"execute"));
    let audit = fixture.sessions.list_operations();
    assert_eq!(
        audit
            .iter()
            .filter(|row| matches!(row.operation, Operation::ExportQuery { .. })
                && row.status == OperationStatus::Failed)
            .count(),
        1
    );
    fixture
        .sessions
        .rollback_transaction(
            fixture.session,
            EndTransactionRequest {
                connection: fixture.connection,
                tx_id: transaction.tx_id,
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn export_commit_confidence_requires_a_fresh_source_after_raw_control_sql() {
    use sift_protocol::*;
    use std::time::Duration;
    for (raw, managed) in [
        ("BEGIN", false),
        ("SET timezone TO 'UTC'", false),
        ("EXECUTE unknown_procedure", false),
        ("SET timezone TO 'UTC'", true),
    ] {
        let mut fixture = managed_edits(
            MockDriver::builder()
                .execute_ok(vec![Page::Done {
                    affected_rows: Some(0),
                    warnings: vec![],
                }])
                .execute_ok(vec![Page::Done {
                    affected_rows: Some(1),
                    warnings: vec![],
                }])
                .execute_ok(vec![Page::Done {
                    affected_rows: Some(1),
                    warnings: vec![],
                }])
                .build(),
        )
        .await;
        let transaction = if managed {
            Some(
                fixture
                    .sessions
                    .begin_transaction(
                        fixture.session,
                        BeginTransactionRequest {
                            connection: fixture.connection,
                            mode: TxMode::default(),
                        },
                    )
                    .await
                    .unwrap(),
            )
        } else {
            None
        };
        let tx = transaction.as_ref().map(|transaction| TxHandleRef {
            tx_id: transaction.tx_id,
            connection: fixture.connection,
            mode: transaction.mode,
        });
        let response = fixture
            .router
            .clone()
            .oneshot(post_json(
                format!("/v1/sessions/{}/queries", fixture.session),
                serde_json::json!({"connection":fixture.connection,"sql":raw,"params":[],"tx":tx}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        if let Some(transaction) = transaction {
            fixture
                .sessions
                .commit_transaction(
                    fixture.session,
                    EndTransactionRequest {
                        connection: fixture.connection,
                        tx_id: transaction.tx_id,
                    },
                )
                .await
                .unwrap();
        }
        let response = fixture
            .router
            .clone()
            .oneshot(export_request(
                &fixture,
                "INSERT INTO users VALUES (1)",
                false,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        to_bytes(response.into_body(), 1024).await.unwrap();
        assert_eq!(
            fixture.shutdown.await_drain(Duration::from_secs(5)).await,
            0
        );
        let ledger = grid_ledger(&fixture);
        assert_eq!(ledger.len(), 1);
        assert_eq!(
            ledger[0].outcome,
            ChangeLedgerOutcome::OutcomeUnknown,
            "{raw}"
        );
        assert_eq!(ledger[0].row_count, Some(1));
        let response = fixture
            .router
            .clone()
            .oneshot(post_json(
                format!("/v1/sessions/{}/connections/from-profile", fixture.session),
                serde_json::json!({"tenant_id":1,"profile_id":fixture.profile.0}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let connection: ConnectionInfo = body_json(response.into_body()).await;
        fixture.connection = connection.id;
        let response = fixture
            .router
            .clone()
            .oneshot(export_request(
                &fixture,
                "INSERT INTO users VALUES (2)",
                false,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        to_bytes(response.into_body(), 1024).await.unwrap();
        assert_eq!(
            fixture.shutdown.await_drain(Duration::from_secs(5)).await,
            0
        );
        let ledger = grid_ledger(&fixture);
        assert_eq!(ledger.len(), 2);
        assert_eq!(
            ledger
                .iter()
                .filter(|entry| entry.outcome == ChangeLedgerOutcome::Committed)
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn export_commit_confidence_observes_concurrent_raw_transaction_control() {
    use sift_protocol::*;
    use std::time::Duration;
    let (native, rows) = tokio::sync::mpsc::channel(1);
    let fixture = managed_edits(
        MockDriver::builder()
            .execute_rows(rows)
            .execute_ok(vec![Page::Done {
                affected_rows: Some(0),
                warnings: vec![],
            }])
            .build(),
    )
    .await;
    let response = fixture
        .router
        .clone()
        .oneshot(export_request(
            &fixture,
            "INSERT INTO users VALUES (1)",
            false,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = tokio::spawn(async move { to_bytes(response.into_body(), 1024).await });
    let response = fixture
        .router
        .clone()
        .oneshot(post_json(
            format!("/v1/sessions/{}/queries", fixture.session),
            serde_json::json!({"connection":fixture.connection,"sql":"BEGIN","params":[]}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    native
        .send(Page::Done {
            affected_rows: Some(1),
            warnings: vec![],
        })
        .await
        .unwrap();
    drop(native);
    body.await.unwrap().unwrap();
    assert_eq!(
        fixture.shutdown.await_drain(Duration::from_secs(5)).await,
        0
    );
    let ledger = grid_ledger(&fixture);
    assert_eq!(ledger.len(), 1);
    assert_eq!(ledger[0].outcome, ChangeLedgerOutcome::OutcomeUnknown);
    assert_eq!(ledger[0].row_count, Some(1));
}
