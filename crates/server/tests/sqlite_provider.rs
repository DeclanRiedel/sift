//! Real SQLite through the HTTP SDK, managed profiles and audited operations.
#![cfg(unix)]
use sift_api_types::OpenConnectionFromProfileRequest;
use sift_driver_sqlite::{FilePolicy, SqliteDriver};
use sift_metadata::{
    CredentialMode, MemorySecretStore, MetadataStore, NewConnectionProfile, PrincipalId, TenantId,
};
use sift_protocol::*;
use sift_server::{
    http::{app, AppState, AuthState},
    room_runtime::RoomRuntime,
    DriverRegistry, SessionStore,
};
use std::sync::Arc;

async fn sqlite_performance_progress_acceptance(
    client: &sift_client_sdk::Client,
    base: &str,
    store: &SessionStore,
    profile: sift_api_types::ConnectionProfileId,
    session: SessionId,
    connection: ConnectionId,
) {
    use std::time::Duration;
    let run_id = uuid::Uuid::new_v4();
    let runner = client.clone();
    let task = tokio::spawn(async move {
        runner
            .benchmark(
                session,
                connection,
                BenchmarkRequest {
                    run_id,
                    sql: "SELECT 'private-progress-sql', ?1 FROM main.items".into(),
                    params: vec![Value::Text("private-progress-bind".into())],
                    warmups: 1,
                    iterations: 2,
                    query_timeout_ms: 5000,
                    total_budget_ms: 15000,
                    delay_ms: 5000,
                    workload_confirmed: true,
                },
            )
            .await
    });
    let progress = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(progress) = client
                .performance_progress(session, connection, run_id, PerformanceRunKind::Benchmark)
                .await
                .unwrap()
            {
                if progress.stage == PerformanceStage::Delay {
                    break progress;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let counters = progress.benchmark.unwrap();
    assert_eq!(
        (counters.finished_warmups, counters.finished_iterations),
        (1, 0)
    );
    assert_eq!(
        (counters.requested_warmups, counters.requested_iterations),
        (1, 2)
    );
    assert_eq!(counters.last_outcome, Some(BenchmarkOutcome::Success));
    assert!(!progress.cancellation_requested);
    for (run, mode) in [
        (uuid::Uuid::new_v4(), PerformanceRunKind::Benchmark),
        (run_id, PerformanceRunKind::Profile),
    ] {
        assert!(client
            .performance_progress(session, connection, run, mode)
            .await
            .unwrap()
            .is_none());
    }
    let other = client.open_session(None).await.unwrap().id;
    assert!(client
        .performance_progress(other, connection, run_id, PerformanceRunKind::Benchmark)
        .await
        .is_err());
    client.close_session(other).await.unwrap();
    let foreign = store
        .open_session_with_owner(
            OpenSessionRequest {
                tag: None,
                tenant_id: None,
            },
            Some(PrincipalId(991)),
            None,
            false,
        )
        .unwrap()
        .id;
    let denied = client
        .performance_progress(foreign, connection, run_id, PerformanceRunKind::Benchmark)
        .await
        .unwrap_err();
    assert!(
        denied.to_string().contains("403"),
        "another owner cannot inspect progress"
    );
    store.close_session(foreign).unwrap();

    let response = reqwest::Client::new()
        .get(format!(
            "{base}/v1/sessions/{session}/connections/{connection}/benchmark/{run_id}/progress"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.headers()[reqwest::header::CACHE_CONTROL],
        "no-store"
    );
    let text = response.text().await.unwrap();
    assert!(
        !text.contains("private-progress-") && !text.contains("sql") && !text.contains("samples")
    );
    // Each permission is independent. Recheck the frozen SQL against current schema policy too.
    for (blocked_ops, allowed_schemas) in [
        (vec![OperationKind::ReadPerformanceProgress], None),
        (vec![OperationKind::BenchmarkQuery], None),
        (vec![OperationKind::ExecuteQuery], None),
        (
            vec![],
            Some(vec![SchemaSelector {
                catalog: None,
                schema: "elsewhere".into(),
            }]),
        ),
    ] {
        client
            .update_connection_policy(
                profile,
                UpdateConnectionPolicyRequest {
                    expected_revision: None,
                    minimum_tenant_role: TenantRole::Member,
                    read_only: false,
                    allowed_ops: None,
                    blocked_ops,
                    allowed_schemas,
                },
            )
            .await
            .unwrap();
        let denied = client
            .performance_progress(session, connection, run_id, PerformanceRunKind::Benchmark)
            .await
            .unwrap_err();
        assert!(
            denied.to_string().contains("403"),
            "current permission must fence progress"
        );
    }
    client
        .update_connection_policy(
            profile,
            UpdateConnectionPolicyRequest {
                expected_revision: None,
                minimum_tenant_role: TenantRole::Member,
                read_only: false,
                allowed_ops: None,
                blocked_ops: vec![],
                allowed_schemas: None,
            },
        )
        .await
        .unwrap();
    client
        .update_connection_policy(
            profile,
            UpdateConnectionPolicyRequest {
                expected_revision: None,
                minimum_tenant_role: TenantRole::Member,
                read_only: true,
                allowed_ops: None,
                blocked_ops: vec![],
                allowed_schemas: Some(vec![SchemaSelector {
                    catalog: None,
                    schema: "main".into(),
                }]),
            },
        )
        .await
        .unwrap();
    assert!(client
        .performance_progress(session, connection, run_id, PerformanceRunKind::Benchmark)
        .await
        .unwrap()
        .is_some());
    client
        .update_connection_policy(
            profile,
            UpdateConnectionPolicyRequest {
                expected_revision: None,
                minimum_tenant_role: TenantRole::Member,
                read_only: false,
                allowed_ops: None,
                blocked_ops: vec![],
                allowed_schemas: None,
            },
        )
        .await
        .unwrap();
    client
        .cancel_benchmark(session, connection, run_id)
        .await
        .unwrap();
    let report = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!report.completed);
    assert_eq!(report.samples.len(), 1);
    assert!(client
        .performance_progress(session, connection, run_id, PerformanceRunKind::Benchmark)
        .await
        .unwrap()
        .is_none());

    // A real SQLite read stays in flight until explicitly cancelled. No profile percentage is inferred.
    let run_id = uuid::Uuid::new_v4();
    let runner = client.clone();
    let task = tokio::spawn(async move {
        runner.profile(session, connection, ProfileRequest {
            connection, run_id, sql: "WITH RECURSIVE counter(n) AS (VALUES(0) UNION ALL SELECT n+1 FROM counter WHERE n<100000000) SELECT sum(n) FROM counter".into(),
            params: vec![], timeout_ms: 30000, workload_confirmed: true,
        }).await
    });
    let observed = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(progress) = client
                .performance_progress(session, connection, run_id, PerformanceRunKind::Profile)
                .await
                .unwrap()
            {
                if progress.stage == PerformanceStage::Profiling {
                    break progress;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(observed.kind, PerformanceRunKind::Profile);
    assert!(observed.benchmark.is_none());
    client
        .update_connection_policy(
            profile,
            UpdateConnectionPolicyRequest {
                expected_revision: None,
                minimum_tenant_role: TenantRole::Member,
                read_only: false,
                allowed_ops: None,
                blocked_ops: vec![OperationKind::ProfileQuery],
                allowed_schemas: None,
            },
        )
        .await
        .unwrap();
    let denied = client
        .performance_progress(session, connection, run_id, PerformanceRunKind::Profile)
        .await
        .unwrap_err();
    assert!(denied.to_string().contains("403"));
    client
        .update_connection_policy(
            profile,
            UpdateConnectionPolicyRequest {
                expected_revision: None,
                minimum_tenant_role: TenantRole::Member,
                read_only: false,
                allowed_ops: None,
                blocked_ops: vec![],
                allowed_schemas: None,
            },
        )
        .await
        .unwrap();
    client
        .cancel_profile(session, connection, run_id)
        .await
        .unwrap();
    assert!(tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .is_err());
    assert!(client
        .performance_progress(session, connection, run_id, PerformanceRunKind::Profile)
        .await
        .unwrap()
        .is_none());
    let operations = store.list_operations();
    assert!(operations
        .iter()
        .any(|entry| matches!(entry.operation, Operation::ReadPerformanceProgress { .. })));
    assert!(!serde_json::to_string(&operations)
        .unwrap()
        .contains("private-progress-"));
    assert_eq!(store.list_connections(session).unwrap().len(), 1);
}

async fn sqlite_column_rename_acceptance(
    client: &sift_client_sdk::Client,
    session: SessionId,
    connection: ConnectionId,
    read_only: ConnectionId,
) {
    client
        .execute(session, connection, "INSERT INTO future VALUES (9,NULL)")
        .await
        .unwrap();
    let old_query = "SELECT rowid,typeof(label),id,label FROM future ORDER BY rowid";
    let new_query = r#"SELECT rowid,typeof("l""abel"),"i""d","l""abel" FROM future ORDER BY rowid"#;
    let rows = |response: ExecuteResponse| {
        response
            .rows
            .into_iter()
            .map(|row| row.values)
            .collect::<Vec<_>>()
    };
    let expected = rows(
        client
            .execute(session, connection, old_query)
            .await
            .unwrap(),
    );
    assert_eq!(expected.len(), 3);
    for sql in [
        r#"ALTER TABLE future RENAME COLUMN id TO "i""d""#,
        r#"ALTER TABLE future RENAME COLUMN label TO "l""abel""#,
    ] {
        client.execute(session, connection, sql).await.unwrap();
    }
    let graph = client
        .catalog_graph(session, connection, CatalogGraphRequest::default())
        .await
        .unwrap();
    let target = client
        .create_catalog_snapshot(
            session,
            connection,
            CreateCatalogSnapshotRequest {
                expected_catalog_revision: graph.revision,
                options: CatalogGraphOptions::default(),
                description: None,
                accept_partial: true,
            },
        )
        .await
        .unwrap();
    for sql in [
        r#"ALTER TABLE future RENAME COLUMN "i""d" TO id"#,
        r#"ALTER TABLE future RENAME COLUMN "l""abel" TO label"#,
    ] {
        client.execute(session, connection, sql).await.unwrap();
    }
    for stage in 0..6 {
        if stage == 3 {
            client
                .execute(
                    session,
                    connection,
                    "CREATE VIEW column_dependent AS SELECT label FROM future",
                )
                .await
                .unwrap();
        }
        if stage == 4 {
            client
                .execute(session, connection, "DROP VIEW column_dependent")
                .await
                .unwrap();
            client
                .execute(
                    session,
                    connection,
                    "CREATE INDEX column_dependent_idx ON future(label)",
                )
                .await
                .unwrap();
        }
        if stage == 5 {
            client
                .execute(session, connection, "DROP INDEX column_dependent_idx")
                .await
                .unwrap();
        }
        let source = if stage == 2 { read_only } else { connection };
        let options = if stage == 5 {
            CatalogGraphOptions {
                kinds: Some(vec![
                    CatalogNodeKind::Catalog,
                    CatalogNodeKind::Schema,
                    CatalogNodeKind::Table,
                    CatalogNodeKind::Column,
                ]),
                ..Default::default()
            }
        } else {
            CatalogGraphOptions::default()
        };
        let live = client
            .catalog_graph(
                session,
                source,
                CatalogGraphRequest {
                    options: options.clone(),
                    refresh: true,
                },
            )
            .await
            .unwrap();
        let before = live
            .data
            .nodes
            .iter()
            .find(|node| node.kind == CatalogNodeKind::Table && node.name == "future")
            .unwrap();
        let after = target
            .graph
            .data
            .nodes
            .iter()
            .find(|node| node.kind == CatalogNodeKind::Table && node.name == "future")
            .unwrap();
        let mappings = [("id", "i\"d"), ("label", "l\"abel")]
            .into_iter()
            .map(|(old, new)| RenameMapping {
                from: live
                    .data
                    .nodes
                    .iter()
                    .find(|node| {
                        node.kind == CatalogNodeKind::Column
                            && node.parent_id.as_ref() == Some(&before.id)
                            && node.name == old
                    })
                    .unwrap()
                    .id
                    .clone(),
                to: target
                    .graph
                    .data
                    .nodes
                    .iter()
                    .find(|node| {
                        node.kind == CatalogNodeKind::Column
                            && node.parent_id.as_ref() == Some(&after.id)
                            && node.name == new
                    })
                    .unwrap()
                    .id
                    .clone(),
            })
            .collect();
        let request = SchemaDiffRequest {
            from: CatalogSourceRef::Live {
                expected_revision: live.revision,
                options,
            },
            to: CatalogSourceRef::Snapshot {
                snapshot_id: target.id,
            },
            accepted_renames: mappings,
            max_changes: None,
        };
        let diff = client
            .compare_catalog_schemas(session, source, request.clone())
            .await
            .unwrap();
        let selected = diff
            .changes
            .iter()
            .filter(|change| {
                change.kind == SchemaChangeKind::Rename
                    && change
                        .object_before
                        .as_ref()
                        .is_some_and(|node| node.kind == CatalogNodeKind::Column)
            })
            .map(|change| change.id.clone())
            .collect::<Vec<_>>();
        assert_eq!(selected.len(), 2);
        if stage == 1 {
            let partial_request = PreviewMigrationRequest {
                diff: request.clone(),
                expected_diff_digest: diff.digest.clone(),
                selected_changes: vec![selected[0].clone()],
                expected_live_revision: live.revision,
                options: MigrationOptions::default(),
            };
            let partial = client
                .preview_migration(session, source, partial_request.clone())
                .await
                .unwrap();
            assert_eq!(partial.groups[0].statements.len(), 1);
            assert_eq!(partial.rollback_groups[0].statements.len(), 1);
            let checked = client
                .validate_migration(
                    session,
                    source,
                    ValidateMigrationRequest {
                        plan_id: partial.id,
                        plan_digest: partial.digest,
                        confirm_test_database: true,
                    },
                )
                .await
                .unwrap();
            assert!(checked.valid && checked.rolled_back);
            assert_eq!(
                rows(
                    client
                        .execute(session, connection, old_query)
                        .await
                        .unwrap()
                ),
                expected
            );
            let table = diff
                .changes
                .iter()
                .find(|change| {
                    change.kind == SchemaChangeKind::Alter
                        && change
                            .object_before
                            .as_ref()
                            .is_some_and(|node| node.kind == CatalogNodeKind::Table)
                })
                .unwrap();
            let mut incomplete = partial_request;
            incomplete.selected_changes.push(table.id.clone());
            assert!(
                client
                    .preview_migration(session, source, incomplete)
                    .await
                    .is_err(),
                "claiming a whole table change must include every rename it carries"
            );
        }
        let plan = client
            .preview_migration(
                session,
                source,
                PreviewMigrationRequest {
                    diff: request,
                    expected_diff_digest: diff.digest,
                    selected_changes: if stage == 0 { vec![] } else { selected },
                    expected_live_revision: live.revision,
                    options: MigrationOptions {
                        prefer_transactional: false,
                        online_indexes: false,
                    },
                },
            )
            .await;
        if stage >= 3 {
            assert!(
                plan.is_err(),
                "dependency-bearing/filtered column rename must be refused"
            );
            continue;
        }
        let plan = plan.unwrap();
        assert!(plan.groups[0].transactional && plan.rollback_groups[0].transactional);
        assert_eq!(plan.groups[0].statements.len(), 2);
        assert_eq!(plan.rollback_groups[0].statements.len(), 2);
        let actual = plan.groups[0]
            .statements
            .iter()
            .map(|s| s.sql.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            actual,
            std::collections::BTreeSet::from([
                r#"ALTER TABLE "main"."future" RENAME COLUMN "id" TO "i""d";"#,
                r#"ALTER TABLE "main"."future" RENAME COLUMN "label" TO "l""abel";"#,
            ])
        );
        if stage == 2 {
            let run = client
                .apply_migration(
                    session,
                    source,
                    ApplyMigrationRequest {
                        plan_id: plan.id,
                        plan_digest: plan.digest,
                        acknowledgements: plan.required_acknowledgements,
                        source: None,
                    },
                )
                .await;
            assert!(!matches!(run,Ok(run) if run.state==MigrationRunState::Applied));
            assert_eq!(
                rows(
                    client
                        .execute(session, connection, old_query)
                        .await
                        .unwrap()
                ),
                expected
            );
            continue;
        }
        let checked = client
            .validate_migration(
                session,
                source,
                ValidateMigrationRequest {
                    plan_id: plan.id,
                    plan_digest: plan.digest.clone(),
                    confirm_test_database: true,
                },
            )
            .await
            .unwrap();
        assert!(checked.valid && checked.rolled_back);
        assert_eq!(
            rows(
                client
                    .execute(session, connection, old_query)
                    .await
                    .unwrap()
            ),
            expected
        );
        if stage == 0 {
            client
                .execute(
                    session,
                    connection,
                    "CREATE INDEX column_stale ON future(label)",
                )
                .await
                .unwrap();
        }
        let run = client
            .apply_migration(
                session,
                source,
                ApplyMigrationRequest {
                    plan_id: plan.id,
                    plan_digest: plan.digest,
                    acknowledgements: plan.required_acknowledgements,
                    source: None,
                },
            )
            .await;
        if stage == 0 {
            assert!(!matches!(run,Ok(run) if run.state==MigrationRunState::Applied));
            client
                .execute(session, connection, "DROP INDEX column_stale")
                .await
                .unwrap();
        } else {
            assert_eq!(run.unwrap().state, MigrationRunState::Applied);
            assert_eq!(
                rows(
                    client
                        .execute(session, connection, new_query)
                        .await
                        .unwrap()
                ),
                expected
            );
            for statement in plan
                .rollback_groups
                .iter()
                .flat_map(|group| &group.statements)
            {
                client
                    .execute(session, connection, &statement.sql)
                    .await
                    .unwrap();
            }
            assert_eq!(
                rows(
                    client
                        .execute(session, connection, old_query)
                        .await
                        .unwrap()
                ),
                expected
            );
        }
    }
    for destination in ["next", "id"] {
        // Explicit mappings may reuse existing names. Native scheduling must
        // preserve logical column values across a cycle and its exact inverse.
        for (from, to) in [
            ("id", "__swap_fixture"),
            ("label", destination),
            ("__swap_fixture", "label"),
        ] {
            client
                .execute(
                    session,
                    connection,
                    format!("ALTER TABLE future RENAME COLUMN {from} TO {to}"),
                )
                .await
                .unwrap();
        }
        let graph = client
            .catalog_graph(session, connection, CatalogGraphRequest::default())
            .await
            .unwrap();
        let swapped = client
            .create_catalog_snapshot(
                session,
                connection,
                CreateCatalogSnapshotRequest {
                    expected_catalog_revision: graph.revision,
                    options: CatalogGraphOptions::default(),
                    description: None,
                    accept_partial: true,
                },
            )
            .await
            .unwrap();
        for (from, to) in [
            ("label", "__swap_fixture"),
            (destination, "label"),
            ("__swap_fixture", "id"),
        ] {
            client
                .execute(
                    session,
                    connection,
                    format!("ALTER TABLE future RENAME COLUMN {from} TO {to}"),
                )
                .await
                .unwrap();
        }
        let live = client
            .catalog_graph(
                session,
                connection,
                CatalogGraphRequest {
                    refresh: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let table = live
            .data
            .nodes
            .iter()
            .find(|node| node.kind == CatalogNodeKind::Table && node.name == "future")
            .unwrap();
        let target_table = swapped
            .graph
            .data
            .nodes
            .iter()
            .find(|node| node.kind == CatalogNodeKind::Table && node.name == "future")
            .unwrap();
        let accepted_renames = [("id", "label"), ("label", destination)]
            .into_iter()
            .map(|(old, new)| RenameMapping {
                from: live
                    .data
                    .nodes
                    .iter()
                    .find(|node| node.parent_id.as_ref() == Some(&table.id) && node.name == old)
                    .unwrap()
                    .id
                    .clone(),
                to: swapped
                    .graph
                    .data
                    .nodes
                    .iter()
                    .find(|node| {
                        node.parent_id.as_ref() == Some(&target_table.id) && node.name == new
                    })
                    .unwrap()
                    .id
                    .clone(),
            })
            .collect();
        let request = SchemaDiffRequest {
            from: CatalogSourceRef::Live {
                expected_revision: live.revision,
                options: CatalogGraphOptions::default(),
            },
            to: CatalogSourceRef::Snapshot {
                snapshot_id: swapped.id,
            },
            accepted_renames,
            max_changes: None,
        };
        let diff = client
            .compare_catalog_schemas(session, connection, request.clone())
            .await
            .unwrap();
        assert_eq!(
            diff.changes
                .iter()
                .filter(|change| change.kind == SchemaChangeKind::Rename)
                .count(),
            2
        );
        let mut preview = PreviewMigrationRequest {
            diff: request,
            expected_diff_digest: diff.digest,
            selected_changes: vec![diff
                .changes
                .iter()
                .find(|change| {
                    change.kind == SchemaChangeKind::Rename
                        && change
                            .object_before
                            .as_ref()
                            .is_some_and(|node| node.name == "id")
                })
                .unwrap()
                .id
                .clone()],
            expected_live_revision: live.revision,
            options: MigrationOptions::default(),
        };
        assert!(
            client
                .preview_migration(session, connection, preview.clone())
                .await
                .is_err(),
            "a conflicting unselected column cannot be moved implicitly"
        );
        preview.selected_changes.clear();
        let plan = client
            .preview_migration(session, connection, preview)
            .await
            .unwrap();
        assert_eq!(
            plan.groups[0].statements.len(),
            if destination == "id" { 3 } else { 2 }
        );
        assert_eq!(
            plan.rollback_groups[0].statements.len(),
            if destination == "id" { 3 } else { 2 }
        );
        assert_eq!(
            plan.groups[0]
                .statements
                .iter()
                .any(|step| step.sql.contains("__sift_rename_")),
            destination == "id"
        );
        let checked = client
            .validate_migration(
                session,
                connection,
                ValidateMigrationRequest {
                    plan_id: plan.id,
                    plan_digest: plan.digest.clone(),
                    confirm_test_database: true,
                },
            )
            .await
            .unwrap();
        assert!(checked.valid && checked.rolled_back);
        assert_eq!(
            rows(
                client
                    .execute(session, connection, old_query)
                    .await
                    .unwrap()
            ),
            expected
        );
        let applied = client
            .apply_migration(
                session,
                connection,
                ApplyMigrationRequest {
                    plan_id: plan.id,
                    plan_digest: plan.digest,
                    acknowledgements: plan.required_acknowledgements,
                    source: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(applied.state, MigrationRunState::Applied);
        assert_eq!(
        rows(
            client
                .execute(
                    session,
                    connection,
                    &format!("SELECT rowid,typeof({destination}),label,{destination} FROM future ORDER BY rowid")
                )
                .await
                .unwrap()
        ),
        expected
    );
        for statement in plan
            .rollback_groups
            .iter()
            .flat_map(|group| &group.statements)
        {
            client
                .execute(session, connection, &statement.sql)
                .await
                .unwrap();
        }
        assert_eq!(
            rows(
                client
                    .execute(session, connection, old_query)
                    .await
                    .unwrap()
            ),
            expected
        );
    }
}

async fn sqlite_designer_rename_acceptance(
    client: &sift_client_sdk::Client,
    session: SessionId,
    connection: ConnectionId,
    read_only: ConnectionId,
) {
    let read = |response: ExecuteResponse| {
        response
            .rows
            .into_iter()
            .map(|row| row.values)
            .collect::<Vec<_>>()
    };
    let query = "SELECT rowid,typeof(label),id,label FROM future ORDER BY rowid";
    let expected = read(client.execute(session, connection, query).await.unwrap());
    for stage in 0..7 {
        if stage == 3 {
            client
                .execute(
                    session,
                    connection,
                    "CREATE VIEW designer_dependent AS SELECT label FROM future",
                )
                .await
                .unwrap();
        }
        if stage == 4 {
            client
                .execute(session, connection, "DROP VIEW designer_dependent")
                .await
                .unwrap();
            client
                .execute(
                    session,
                    connection,
                    "CREATE INDEX designer_idx ON future(label)",
                )
                .await
                .unwrap();
        }
        if stage == 5 {
            client
                .execute(session, connection, "DROP INDEX designer_idx")
                .await
                .unwrap();
        }
        let source = if stage == 2 { read_only } else { connection };
        let live = client
            .catalog_graph(
                session,
                source,
                CatalogGraphRequest {
                    refresh: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let table = live
            .data
            .nodes
            .iter()
            .find(|node| node.kind == CatalogNodeKind::Table && node.name == "future")
            .unwrap();
        let column = live
            .data
            .nodes
            .iter()
            .find(|node| node.parent_id.as_ref() == Some(&table.id) && node.name == "label")
            .unwrap();
        let target = if stage == 1 { table } else { column };
        let name = if stage == 6 { "LABEL" } else { "ren\"amed; --" };
        let request = PreviewCatalogDiagramMutationRequest {
            expected_catalog_revision: live.revision,
            mutation: CatalogDiagramMutation::RenameObject {
                object_id: target.id.clone(),
                new_name: name.into(),
            },
            options: MigrationOptions {
                prefer_transactional: false,
                ..Default::default()
            },
        };
        if stage == 0 {
            for mutation in [
                CatalogDiagramMutation::RenameObject {
                    object_id: column.id.clone(),
                    new_name: "ID".into(),
                },
                CatalogDiagramMutation::RenameObject {
                    object_id: column.id.clone(),
                    new_name: "bad\0name".into(),
                },
                CatalogDiagramMutation::AddColumn {
                    table_id: table.id.clone(),
                    name: "unsupported".into(),
                    type_ref: TypeRef::Primitive(PrimitiveType::Int64),
                    nullability: Nullability::Nullable,
                },
            ] {
                assert!(client
                    .preview_catalog_diagram_mutation(
                        session,
                        source,
                        PreviewCatalogDiagramMutationRequest {
                            mutation,
                            ..request.clone()
                        }
                    )
                    .await
                    .is_err());
            }
        }
        let result = client
            .preview_catalog_diagram_mutation(session, source, request)
            .await;
        if stage == 3 || stage == 4 {
            assert!(result.is_err());
            continue;
        }
        let plan = result.unwrap();
        assert_eq!(plan.groups.len(), 1);
        assert!(plan.groups[0].transactional);
        assert_eq!(plan.groups[0].statements.len(), 1);
        assert!(plan.rollback_groups[0].transactional);
        assert_eq!(
            read(client.execute(session, connection, query).await.unwrap()),
            expected,
            "preview must never execute the rename"
        );
        let validation = client
            .validate_migration(
                session,
                source,
                ValidateMigrationRequest {
                    plan_id: plan.id,
                    plan_digest: plan.digest.clone(),
                    confirm_test_database: true,
                },
            )
            .await;
        if stage == 2 {
            assert!(!validation.unwrap().valid);
        } else {
            let checked = validation.unwrap();
            assert!(checked.valid && checked.rolled_back);
        }
        assert_eq!(
            read(client.execute(session, connection, query).await.unwrap()),
            expected
        );
        if stage == 5 {
            client
                .execute(
                    session,
                    connection,
                    "CREATE INDEX designer_stale ON future(label)",
                )
                .await
                .unwrap();
        }
        let applied = client
            .apply_migration(
                session,
                source,
                ApplyMigrationRequest {
                    plan_id: plan.id,
                    plan_digest: plan.digest.clone(),
                    acknowledgements: plan.required_acknowledgements.clone(),
                    source: None,
                },
            )
            .await;
        if stage == 2 || stage == 5 {
            assert!(!matches!(applied, Ok(run) if run.state == MigrationRunState::Applied));
            if stage == 5 {
                client
                    .execute(session, connection, "DROP INDEX designer_stale")
                    .await
                    .unwrap();
            }
            assert_eq!(
                read(client.execute(session, connection, query).await.unwrap()),
                expected
            );
            continue;
        }
        assert_eq!(applied.unwrap().state, MigrationRunState::Applied);
        let quoted = format!("\"{}\"", name.replace('"', "\"\""));
        let new_query = if stage == 1 {
            format!("SELECT rowid,typeof(label),id,label FROM {quoted} ORDER BY rowid")
        } else {
            format!("SELECT rowid,typeof({quoted}),id,{quoted} FROM future ORDER BY rowid")
        };
        assert_eq!(
            read(
                client
                    .execute(session, connection, &new_query)
                    .await
                    .unwrap()
            ),
            expected
        );
        for step in plan
            .rollback_groups
            .iter()
            .flat_map(|group| &group.statements)
        {
            client
                .execute(session, connection, &step.sql)
                .await
                .unwrap();
        }
        assert_eq!(
            read(client.execute(session, connection, query).await.unwrap()),
            expected
        );
    }
    client
        .execute(
            session,
            connection,
            "CREATE TABLE designer_constraint (id INTEGER PRIMARY KEY, value TEXT)",
        )
        .await
        .unwrap();
    let live = client
        .catalog_graph(
            session,
            connection,
            CatalogGraphRequest {
                refresh: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let table = live
        .data
        .nodes
        .iter()
        .find(|node| node.kind == CatalogNodeKind::Table && node.name == "designer_constraint")
        .unwrap();
    assert!(client
        .preview_catalog_diagram_mutation(
            session,
            connection,
            PreviewCatalogDiagramMutationRequest {
                expected_catalog_revision: live.revision,
                mutation: CatalogDiagramMutation::RenameObject {
                    object_id: table.id.clone(),
                    new_name: "refused".into()
                },
                options: MigrationOptions::default()
            }
        )
        .await
        .is_err());
    client
        .execute(session, connection, "DROP TABLE designer_constraint")
        .await
        .unwrap();
}

#[tokio::test]
async fn sqlite_snapshot_migrations_preserve_proven_shapes_and_rows() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("migration.db");
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(
            "CREATE TABLE future (id INTEGER NOT NULL, label TEXT);\
             CREATE TABLE lossy (id INTEGER PRIMARY KEY, label TEXT DEFAULT 'preset');",
        )
        .unwrap();
    let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
    metadata.bootstrap_local("sqlite migration test").unwrap();
    let profile = metadata
        .upsert_connection_profile(
            TenantId(1),
            PrincipalId(1),
            NewConnectionProfile {
                name: "SQLite migration fixture".into(),
                provider_id: Engine::Sqlite.provider_id(),
                configuration: serde_json::json!({"root_id":"test","path":"migration.db","mode":"read_write"}),
                semantic_engine: Some(Engine::Sqlite),
                credentials: None,
                credential_mode: CredentialMode::Shared,
                tags: vec![],
            },
        )
        .await
        .unwrap();
    let read_only_profile = metadata
        .upsert_connection_profile(
            TenantId(1),
            PrincipalId(1),
            NewConnectionProfile {
                name: "SQLite migration read-only fixture".into(),
                provider_id: Engine::Sqlite.provider_id(),
                configuration: serde_json::json!({"root_id":"test","path":"migration.db","mode":"read_only"}),
                semantic_engine: Some(Engine::Sqlite),
                credentials: None,
                credential_mode: CredentialMode::Shared,
                tags: vec![],
            },
        )
        .await
        .unwrap();
    let driver = SqliteDriver::with_files(FilePolicy {
        config: SqliteDriverConfig {
            roots: std::collections::BTreeMap::from([(
                "test".into(),
                SqliteRootConfig {
                    path: root.path().to_str().unwrap().into(),
                    allowed_tenants: vec![1],
                    read_only: false,
                },
            )]),
            max_connections: 3,
        },
        protected: vec![],
    });
    let router = app(AppState {
        sessions: SessionStore::new(DriverRegistry::builder().register(driver).build()),
        rooms: RoomRuntime::default(),
        shutdown: Default::default(),
        auth: AuthState::default(),
        metadata: Some(metadata),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = sift_client_sdk::Client::new(format!("http://{addr}"));
    let session = client.open_session(None).await.unwrap().id;
    let connection = client
        .open_connection_from_profile(
            session,
            OpenConnectionFromProfileRequest {
                tenant_id: 1,
                profile_id: profile.id.0,
            },
        )
        .await
        .unwrap()
        .id;
    let graph = client
        .catalog_graph(session, connection, CatalogGraphRequest::default())
        .await
        .unwrap();
    let snapshot = client
        .create_catalog_snapshot(
            session,
            connection,
            CreateCatalogSnapshotRequest {
                expected_catalog_revision: graph.revision,
                options: CatalogGraphOptions::default(),
                description: None,
                accept_partial: true,
            },
        )
        .await
        .unwrap();
    assert!(snapshot.graph.data.nodes.iter().any(|node| {
        node.name == "future" && node.extra.contains_key("sqlite_safe_create_sql")
    }));
    assert!(snapshot.graph.data.nodes.iter().all(|node| {
        node.name != "lossy" || !node.extra.contains_key("sqlite_safe_create_sql")
    }));
    client
        .execute(session, connection, "DROP TABLE future")
        .await
        .unwrap();
    client
        .execute(session, connection, "DROP TABLE lossy")
        .await
        .unwrap();
    let live = client
        .catalog_graph(session, connection, CatalogGraphRequest::default())
        .await
        .unwrap();
    let diff_request = SchemaDiffRequest {
        from: CatalogSourceRef::Live {
            expected_revision: live.revision,
            options: CatalogGraphOptions::default(),
        },
        to: CatalogSourceRef::Snapshot {
            snapshot_id: snapshot.id,
        },
        accepted_renames: vec![],
        max_changes: None,
    };
    let diff = client
        .compare_catalog_schemas(session, connection, diff_request.clone())
        .await
        .unwrap();
    let table_change = |name: &str| {
        diff.changes
            .iter()
            .find(|change| {
                change.kind == SchemaChangeKind::Create
                    && change.object_after.as_ref().is_some_and(|node| {
                        node.kind == CatalogNodeKind::Table && node.name == name
                    })
            })
            .unwrap()
            .id
            .clone()
    };
    let lossy_change = table_change("lossy");
    let future_change = table_change("future");
    let lossy = client
        .preview_migration(
            session,
            connection,
            PreviewMigrationRequest {
                diff: diff_request.clone(),
                expected_diff_digest: diff.digest.clone(),
                selected_changes: vec![lossy_change],
                expected_live_revision: live.revision,
                options: MigrationOptions::default(),
            },
        )
        .await;
    assert!(lossy.is_err());
    let plan = client
        .preview_migration(
            session,
            connection,
            PreviewMigrationRequest {
                diff: diff_request,
                expected_diff_digest: diff.digest,
                selected_changes: vec![future_change],
                expected_live_revision: live.revision,
                options: MigrationOptions {
                    prefer_transactional: false,
                    online_indexes: false,
                },
            },
        )
        .await
        .unwrap();
    assert!(plan.groups[0].transactional);
    let validation = client
        .validate_migration(
            session,
            connection,
            ValidateMigrationRequest {
                plan_id: plan.id,
                plan_digest: plan.digest.clone(),
                confirm_test_database: true,
            },
        )
        .await
        .unwrap();
    assert!(validation.valid && validation.rolled_back);
    assert_eq!(
        rusqlite::Connection::open(&path)
            .unwrap()
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name='future'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    let read_only_connection = client
        .open_connection_from_profile(
            session,
            OpenConnectionFromProfileRequest {
                tenant_id: 1,
                profile_id: read_only_profile.id.0,
            },
        )
        .await
        .unwrap()
        .id;
    let read_only_live = client
        .catalog_graph(
            session,
            read_only_connection,
            CatalogGraphRequest::default(),
        )
        .await
        .unwrap();
    let read_only_diff_request = SchemaDiffRequest {
        from: CatalogSourceRef::Live {
            expected_revision: read_only_live.revision,
            options: CatalogGraphOptions::default(),
        },
        to: CatalogSourceRef::Snapshot {
            snapshot_id: snapshot.id,
        },
        accepted_renames: vec![],
        max_changes: None,
    };
    let read_only_diff = client
        .compare_catalog_schemas(
            session,
            read_only_connection,
            read_only_diff_request.clone(),
        )
        .await
        .unwrap();
    let read_only_plan = client
        .preview_migration(
            session,
            read_only_connection,
            PreviewMigrationRequest {
                diff: read_only_diff_request,
                expected_diff_digest: read_only_diff.digest,
                selected_changes: vec![read_only_diff
                    .changes
                    .iter()
                    .find(|change| {
                        change.kind == SchemaChangeKind::Create
                            && change
                                .object_after
                                .as_ref()
                                .is_some_and(|node| node.name == "future")
                    })
                    .unwrap()
                    .id
                    .clone()],
                expected_live_revision: read_only_live.revision,
                options: MigrationOptions::default(),
            },
        )
        .await
        .unwrap();
    let read_only_apply = client
        .apply_migration(
            session,
            read_only_connection,
            ApplyMigrationRequest {
                plan_id: read_only_plan.id,
                plan_digest: read_only_plan.digest,
                acknowledgements: read_only_plan.required_acknowledgements,
                source: None,
            },
        )
        .await;
    assert!(!matches!(read_only_apply, Ok(run) if run.state == MigrationRunState::Applied));
    assert_eq!(
        rusqlite::Connection::open(&path)
            .unwrap()
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name='future'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    let used_plan_id = plan.id;
    let used_plan_digest = plan.digest.clone();
    let run = client
        .apply_migration(
            session,
            connection,
            ApplyMigrationRequest {
                plan_id: plan.id,
                plan_digest: plan.digest,
                acknowledgements: plan.required_acknowledgements,
                source: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(run.state, MigrationRunState::Applied);
    assert!(client
        .apply_migration(
            session,
            connection,
            ApplyMigrationRequest {
                plan_id: used_plan_id,
                plan_digest: used_plan_digest,
                acknowledgements: vec![],
                source: None,
            },
        )
        .await
        .is_err());
    assert_eq!(
        rusqlite::Connection::open(&path)
            .unwrap()
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name='future'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    client
        .execute(
            session,
            connection,
            "INSERT INTO future VALUES (7, 'kept'), (8, x'00ff')",
        )
        .await
        .unwrap();
    client
        .execute(
            session,
            connection,
            r#"ALTER TABLE future RENAME TO "r""enamed""#,
        )
        .await
        .unwrap();
    let target = client
        .catalog_graph(session, connection, CatalogGraphRequest::default())
        .await
        .unwrap();
    let target = client
        .create_catalog_snapshot(
            session,
            connection,
            CreateCatalogSnapshotRequest {
                expected_catalog_revision: target.revision,
                options: CatalogGraphOptions::default(),
                description: None,
                accept_partial: true,
            },
        )
        .await
        .unwrap();
    client
        .execute(
            session,
            connection,
            r#"ALTER TABLE "r""enamed" RENAME TO future"#,
        )
        .await
        .unwrap();
    for stage in 0..4 {
        if stage == 2 {
            client
                .execute(
                    session,
                    connection,
                    "CREATE VIEW dependent AS SELECT * FROM future",
                )
                .await
                .unwrap();
        }
        let options = if stage == 3 {
            CatalogGraphOptions {
                kinds: Some(vec![
                    CatalogNodeKind::Catalog,
                    CatalogNodeKind::Schema,
                    CatalogNodeKind::Table,
                    CatalogNodeKind::Column,
                ]),
                ..Default::default()
            }
        } else {
            CatalogGraphOptions::default()
        };
        let live = client
            .catalog_graph(
                session,
                connection,
                CatalogGraphRequest {
                    options: options.clone(),
                    refresh: false,
                },
            )
            .await
            .unwrap();
        if stage == 3 {
            assert_eq!(
                live.data.coverage.requested_kinds,
                options.kinds.clone().unwrap()
            );
        }
        let old = live
            .data
            .nodes
            .iter()
            .find(|node| node.kind == CatalogNodeKind::Table && node.name == "future")
            .unwrap();
        let new = target
            .graph
            .data
            .nodes
            .iter()
            .find(|node| node.kind == CatalogNodeKind::Table && node.name == "r\"enamed")
            .unwrap();
        let request = SchemaDiffRequest {
            from: CatalogSourceRef::Live {
                expected_revision: live.revision,
                options,
            },
            to: CatalogSourceRef::Snapshot {
                snapshot_id: target.id,
            },
            accepted_renames: vec![RenameMapping {
                from: old.id.clone(),
                to: new.id.clone(),
            }],
            max_changes: None,
        };
        let diff = client
            .compare_catalog_schemas(session, connection, request.clone())
            .await
            .unwrap();
        let rename = diff
            .changes
            .iter()
            .find(|change| change.kind == SchemaChangeKind::Rename)
            .unwrap();
        let result = client
            .preview_migration(
                session,
                connection,
                PreviewMigrationRequest {
                    diff: request,
                    expected_diff_digest: diff.digest,
                    selected_changes: vec![rename.id.clone()],
                    expected_live_revision: live.revision,
                    options: MigrationOptions {
                        prefer_transactional: false,
                        online_indexes: false,
                    },
                },
            )
            .await;
        if stage >= 2 {
            assert!(
                result.is_err(),
                "dependency-bearing or filtered catalogs must stay refused"
            );
            continue;
        }
        let plan = result.unwrap();
        assert!(plan.groups[0].transactional);
        assert!(plan.rollback_groups[0].transactional);
        assert_eq!(
            plan.groups[0].statements[0].sql,
            r#"ALTER TABLE "main"."future" RENAME TO "r""enamed";"#
        );
        assert_eq!(
            plan.rollback_groups[0].statements[0].sql,
            r#"ALTER TABLE "main"."r""enamed" RENAME TO "future";"#
        );
        let checked = client
            .validate_migration(
                session,
                connection,
                ValidateMigrationRequest {
                    plan_id: plan.id,
                    plan_digest: plan.digest.clone(),
                    confirm_test_database: true,
                },
            )
            .await
            .unwrap();
        assert!(checked.valid && checked.rolled_back);
        assert_eq!(
            client
                .execute(
                    session,
                    connection,
                    "SELECT rowid,label FROM future WHERE id=7"
                )
                .await
                .unwrap()
                .rows[0]
                .values,
            vec![Value::Int64(1), Value::Text("kept".into())]
        );
        if stage == 0 {
            client
                .execute(session, connection, "CREATE INDEX changed ON future(id)")
                .await
                .unwrap();
        }
        let run = client
            .apply_migration(
                session,
                connection,
                ApplyMigrationRequest {
                    plan_id: plan.id,
                    plan_digest: plan.digest,
                    acknowledgements: plan.required_acknowledgements,
                    source: None,
                },
            )
            .await;
        if stage == 0 {
            assert!(!matches!(run, Ok(run) if run.state == MigrationRunState::Applied));
            client
                .execute(session, connection, "DROP INDEX changed")
                .await
                .unwrap();
        } else {
            assert_eq!(run.unwrap().state, MigrationRunState::Applied);
            assert_eq!(
                client
                    .execute(
                        session,
                        connection,
                        r#"SELECT rowid,label FROM "r""enamed" WHERE id=7"#
                    )
                    .await
                    .unwrap()
                    .rows[0]
                    .values,
                vec![Value::Int64(1), Value::Text("kept".into())]
            );
            assert_eq!(
                client
                    .execute(
                        session,
                        connection,
                        r#"SELECT rowid,label FROM "r""enamed" WHERE id=8"#
                    )
                    .await
                    .unwrap()
                    .rows[0]
                    .values,
                vec![Value::Int64(2), Value::Blob(vec![0, 255])]
            );
            client
                .execute(
                    session,
                    connection,
                    &plan.rollback_groups[0].statements[0].sql,
                )
                .await
                .unwrap();
            assert_eq!(
                client
                    .execute(
                        session,
                        connection,
                        "SELECT rowid,label FROM future WHERE id=7"
                    )
                    .await
                    .unwrap()
                    .rows[0]
                    .values,
                vec![Value::Int64(1), Value::Text("kept".into())]
            );
        }
    }
    client
        .execute(session, connection, "DROP VIEW dependent")
        .await
        .unwrap();
    sqlite_column_rename_acceptance(&client, session, connection, read_only_connection).await;
    sqlite_designer_rename_acceptance(&client, session, connection, read_only_connection).await;
    sqlite_add_column_acceptance(&client, session, connection, read_only_connection).await;
    sqlite_drop_column_acceptance(&client, session, connection, read_only_connection).await;
    client.close_session(session).await.unwrap();
    server.abort();
}

#[tokio::test]
async fn sqlite_managed_profile_transactions_catalog_plans_and_atomic_import() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("data.db");
    rusqlite::Connection::open(&path).unwrap().execute_batch("CREATE TABLE items(id INTEGER PRIMARY KEY, label TEXT NOT NULL); CREATE TABLE nullable(a TEXT,b TEXT,PRIMARY KEY(a,b));").unwrap();
    let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
    metadata.bootstrap_local("sqlite test").unwrap();
    let profile=metadata.upsert_connection_profile(TenantId(1),PrincipalId(1),NewConnectionProfile{name:"SQLite fixture".into(),provider_id:Engine::Sqlite.provider_id(),configuration:serde_json::json!({"root_id":"test","path":"data.db","mode":"read_write"}),semantic_engine:Some(Engine::Sqlite),credentials:None,credential_mode:CredentialMode::Shared,tags:vec![]}).await.unwrap();
    let driver = SqliteDriver::with_files(FilePolicy {
        config: SqliteDriverConfig {
            roots: std::collections::BTreeMap::from([(
                "test".into(),
                SqliteRootConfig {
                    path: root.path().to_str().unwrap().into(),
                    allowed_tenants: vec![1],
                    read_only: false,
                },
            )]),
            max_connections: 3,
        },
        protected: vec![],
    });
    let store = SessionStore::new(DriverRegistry::builder().register(driver).build());
    let router = app(AppState {
        sessions: store.clone(),
        rooms: RoomRuntime::default(),
        shutdown: Default::default(),
        auth: AuthState::default(),
        metadata: Some(metadata.clone()),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = sift_client_sdk::Client::new(format!("http://{addr}"));
    let session = client.open_session(None).await.unwrap().id;
    let connection = client
        .open_connection_from_profile(
            session,
            OpenConnectionFromProfileRequest {
                tenant_id: 1,
                profile_id: profile.id.0,
            },
        )
        .await
        .unwrap()
        .id;
    sqlite_performance_progress_acceptance(
        &client,
        &format!("http://{addr}"),
        &store,
        sift_api_types::ConnectionProfileId(profile.id.0),
        session,
        connection,
    )
    .await;
    let metrics_response = reqwest::Client::new()
        .get(format!("http://{addr}/v1/metrics"))
        .send()
        .await
        .unwrap();
    assert!(metrics_response.status().is_success());
    assert_eq!(
        metrics_response.headers()[reqwest::header::CONTENT_TYPE],
        "text/plain; version=0.0.4; charset=utf-8"
    );
    assert!(metrics_response
        .text()
        .await
        .unwrap()
        .contains("sift_http_requests_total"));
    assert!(client
        .metrics()
        .await
        .unwrap()
        .contains("sift_http_requests_total"));
    client
        .execute(session, connection, "INSERT INTO items VALUES(1,'kept')")
        .await
        .unwrap();
    let named_sql="SELECT label FROM items WHERE id=:id AND :id=?1 AND $label::ns(key)=label AND $label::ns(key)=@label";
    let named_params = vec![
        Value::Int64(1),
        Value::Text("kept".into()),
        Value::Text("kept".into()),
    ];
    let named = client
        .execute_with_params(session, connection, named_sql, named_params.clone())
        .await
        .unwrap();
    assert_eq!(named.rows[0].values, vec![Value::Text("kept".into())]);
    assert!(client
        .execute_with_params(session, connection, named_sql, vec![Value::Int64(1)])
        .await
        .is_err());
    let template = "SELECT $name('), {{value}}";
    let compiled = sift_sql_variables::compile(
        Engine::Sqlite,
        template,
        &[SqlVariableBinding {
            name: "value".into(),
            scope: SqlVariableScope::QueryTab,
            value: SqlVariableValue::Scalar(Value::Int64(9)),
        }],
    )
    .unwrap();
    assert_eq!(compiled.sql, "SELECT $name('), ?2");
    let mut ws_params = vec![Value::Int64(8)];
    ws_params.extend(compiled.params);
    let mut socket = client.connect_session_websocket(session).await.unwrap();
    socket
        .send(WsClientMessage::Execute {
            request_id: "named-template".into(),
            connection,
            sql: compiled.sql,
            event_version: None,
            params: ws_params,
            tx: None,
            transform: None,
            source: None,
            variable_context: Some(Box::new(SqlVariableHistoryContext {
                template_sql: template.into(),
                descriptors: compiled.descriptors,
            })),
        })
        .await
        .unwrap();
    let mut ws_rows = Vec::new();
    let mut ws_done = false;
    for _ in 0..8 {
        match tokio::time::timeout(std::time::Duration::from_secs(10), socket.next())
            .await
            .unwrap()
            .unwrap()
        {
            WsServerMessage::Started { .. } => {}
            WsServerMessage::Page {
                cursor_id,
                seq,
                page,
            } => {
                socket
                    .send(WsClientMessage::Ack { cursor_id, seq })
                    .await
                    .unwrap();
                match page {
                    Page::Rows { rows } => ws_rows.extend(rows),
                    Page::Done { .. } => {
                        ws_done = true;
                        break;
                    }
                    Page::Error { error } => panic!("{error:?}"),
                    _ => {}
                }
            }
            other => panic!("unexpected named template response: {other:?}"),
        }
    }
    assert!(ws_done);
    assert_eq!(ws_rows[0].values, vec![Value::Int64(8), Value::Int64(9)]);
    let definition = sift_protocol::SaveBenchmarkDefinitionRequest {
        name: "Named SQLite workload".into(),
        engine: Engine::Sqlite,
        sql: named_sql.into(),
        parameter_count: 3,
        limits: sift_protocol::BenchmarkLimits {
            warmups: 0,
            iterations: 1,
            query_timeout_ms: 10000,
            total_budget_ms: 10000,
            delay_ms: 0,
        },
    };
    let saved_definition = client
        .save_benchmark_definition(sift_api_types::TenantId(1), &definition)
        .await
        .unwrap();
    assert_eq!(saved_definition.sql, named_sql);
    let mut wrong_count = definition.clone();
    wrong_count.parameter_count = 4;
    assert!(client
        .save_benchmark_definition(sift_api_types::TenantId(1), &wrong_count)
        .await
        .is_err());
    let rerun = client
        .run_benchmark_definition(
            session,
            connection,
            saved_definition.id,
            &sift_protocol::RunBenchmarkDefinitionRequest {
                tenant_id: 1,
                run_id: uuid::Uuid::new_v4(),
                expected_revision: saved_definition.revision,
                params: named_params.clone(),
                workload_confirmed: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(rerun.sql, named_sql);
    assert_eq!(rerun.parameter_count, 3);
    assert_eq!(rerun.samples[0].rows, Some(1));
    let measured = client
        .profile(
            session,
            connection,
            ProfileRequest {
                connection,
                run_id: uuid::Uuid::new_v4(),
                sql: named_sql.into(),
                params: named_params.clone(),
                timeout_ms: 10_000,
                workload_confirmed: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(measured.plan.engine, Engine::Sqlite);
    assert!(!measured.plan.analyzed);
    assert_eq!(measured.rows_returned, Some(1));
    assert!(measured.server_elapsed_ns > 0);
    assert_eq!(measured.planning_ms, None);
    assert_eq!(measured.execution_ms, None);
    let environment = measured.environment.as_ref().unwrap();
    assert_eq!(
        environment.instrumentation,
        "estimated_plan_plus_measured_read"
    );
    assert_eq!(environment.isolation, "read_only_serializable");
    let observed = environment.observed_settings.as_ref().unwrap();
    assert!(observed.is_bounded());
    assert_eq!(
        observed.values["cache_size"], "-2000",
        "dedicated defaults are observed before workload setup"
    );
    assert_eq!(
        observed.values["query_only"], "0",
        "observation precedes workload read-only setup"
    );

    fn no_invented_metrics(node: &PlanNode) -> bool {
        node.est_cost.is_none()
            && node.actual_rows.is_none()
            && node.actual_ms.is_none()
            && node.children.iter().all(no_invented_metrics)
    }
    assert!(no_invented_metrics(&measured.plan.root));
    let benchmark = client
        .benchmark(
            session,
            connection,
            BenchmarkRequest {
                run_id: uuid::Uuid::new_v4(),
                sql: named_sql.into(),
                params: named_params.clone(),
                warmups: 0,
                iterations: 1,
                query_timeout_ms: 10_000,
                total_budget_ms: 10_000,
                delay_ms: 0,
                workload_confirmed: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(benchmark.samples.len(), 1);
    assert_eq!(benchmark.samples[0].outcome, BenchmarkOutcome::Success);
    assert_eq!(benchmark.samples[0].rows, Some(1));
    let observed = benchmark
        .environment
        .as_ref()
        .unwrap()
        .observed_settings
        .as_ref()
        .unwrap();
    assert_eq!(observed.values["cache_size"], "-2000");
    assert_eq!(observed.values["query_only"], "0");
    let editor_settings = client
        .execute(session, connection, "PRAGMA cache_size")
        .await
        .unwrap();
    assert!(matches!(
        editor_settings.rows[0].values[0],
        Value::Int64(-2000)
    ));

    client
        .execute(
            session,
            connection,
            "CREATE TABLE pragma_query_only(query_only TEXT)",
        )
        .await
        .unwrap();
    client
        .execute(
            session,
            connection,
            "INSERT INTO pragma_query_only VALUES ('private-shadow-sentinel')",
        )
        .await
        .unwrap();
    let shadowed = client
        .benchmark(
            session,
            connection,
            BenchmarkRequest {
                run_id: uuid::Uuid::new_v4(),
                sql: "SELECT label FROM items".into(),
                params: vec![],
                warmups: 0,
                iterations: 1,
                query_timeout_ms: 1000,
                total_budget_ms: 5000,
                delay_ms: 0,
                workload_confirmed: true,
            },
        )
        .await;
    assert!(
        shadowed.is_err(),
        "a shadow table cannot supply alleged builtin settings"
    );
    assert!(!shadowed
        .unwrap_err()
        .to_string()
        .contains("private-shadow-sentinel"));
    client
        .execute(session, connection, "DROP TABLE pragma_query_only")
        .await
        .unwrap();
    let refused_write = client
        .profile(
            session,
            connection,
            ProfileRequest {
                connection,
                run_id: uuid::Uuid::new_v4(),
                sql: "UPDATE items SET label = 'changed' WHERE id = 1".into(),
                params: Vec::new(),
                timeout_ms: 10_000,
                workload_confirmed: true,
            },
        )
        .await
        .unwrap_err();
    assert!(refused_write.to_string().contains("read query"));
    let refused_result = client
        .profile(
            session,
            connection,
            ProfileRequest {
                connection,
                run_id: uuid::Uuid::new_v4(),
                sql: "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x < 10001) SELECT x FROM c".into(),
                params: Vec::new(),
                timeout_ms: 10_000,
                workload_confirmed: true,
            },
        )
        .await
        .unwrap_err();
    assert!(refused_result
        .to_string()
        .contains("SQLite profile result exceeds row or byte limit"));
    assert_eq!(
        rusqlite::Connection::open(&path)
            .unwrap()
            .query_row("SELECT label FROM items WHERE id=1", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "kept"
    );
    // Parquet stays typed through the real SQLite driver and atomic importer.
    for quick in [false, true] {
        let report = client
            .check_integrity(session, connection, IntegrityCheckRequest::Sqlite { quick })
            .await
            .unwrap();
        assert_eq!(report.outcome, IntegrityOutcome::NoIssuesReported);
        assert!(report.findings.is_empty());
    }
    // Introduce a recoverable constraint violation, not file corruption.
    for sql in [
        "PRAGMA main.integrity_check(0)",
        "PRAGMA main.quick_check(1001)",
        "PRAGMA user_version=123",
        "PRAGMA query_only=OFF",
        "PRAGMA read_uncommitted=ON",
        "PRAGMA busy_timeout=1",
        "PRAGMA cache_size=-321",
        "PRAGMA synchronous=OFF",
        "PRAGMA temp_store=MEMORY",
        "PRAGMA journal_mode=OFF",
        "PRAGMA foreign_keys=OFF",
        "PRAGMA automatic_index=OFF",
    ] {
        assert!(client.execute(session, connection, sql).await.is_err());
    }
    let local = rusqlite::Connection::open(&path).unwrap();
    local.execute_batch("PRAGMA ignore_check_constraints=ON; CREATE TABLE integrity_bad(v INTEGER CHECK(v>0)); INSERT INTO integrity_bad VALUES(-1);").unwrap();
    let report = client
        .check_integrity(
            session,
            connection,
            IntegrityCheckRequest::Sqlite { quick: false },
        )
        .await
        .unwrap();
    assert_eq!(report.outcome, IntegrityOutcome::IssuesReported);
    assert!(report
        .findings
        .iter()
        .any(|finding| finding.contains("integrity_bad")));
    assert_eq!(
        local
            .query_row("SELECT v FROM integrity_bad", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        -1
    );
    local.execute_batch("DROP TABLE integrity_bad").unwrap();
    drop(local);
    assert!(client
        .check_integrity(
            session,
            connection,
            IntegrityCheckRequest::SqlServer {
                physical_only: true
            }
        )
        .await
        .is_err());
    {
        use futures::StreamExt;
        let stream = store
            .export_stream(
                session,
                connection,
                ExportRequest {
                    sql: "SELECT id,label FROM items".into(),
                    params: vec![],
                    format: ExportFormat::Parquet,
                    header: true,
                    null_display: None,
                },
            )
            .await
            .unwrap();
        futures::pin_mut!(stream);
        let mut data = Vec::new();
        while let Some(chunk) = stream.next().await {
            data.extend_from_slice(&chunk.unwrap());
        }
        let room = metadata
            .create_room(
                TenantId(1),
                PrincipalId(1),
                sift_metadata::NewRoom {
                    name: "Parquet transfer".into(),
                    kind: sift_metadata::RoomKind::Shared,
                },
            )
            .unwrap();
        let workspace = metadata
            .create_workspace(room.id, PrincipalId(1), "Parquet transfer")
            .unwrap();
        let recipe = metadata
            .create_transfer_recipe(
                workspace.id,
                PrincipalId(1),
                sift_metadata::NewTransferRecipe {
                    name: "parquet".into(),
                    direction: TransferDirection::Import,
                    source: TransferEndpoint::Upload,
                    sink: TransferEndpoint::Table,
                    format_id: "parquet".into(),
                    format_version: "1".into(),
                    options: serde_json::json!({}),
                },
            )
            .unwrap();
        let request = sift_metadata::http::ExecuteTransferRecipeRequest {
            session_id: session,
            connection_id: connection,
            sql: None,
            params: vec![],
            data: Some(data),
            table: Some(ObjectPath {
                catalog: None,
                schema: None,
                name: "parquet_copy".into(),
                kind: Some(ObjectKind::Table),
                routine_args: None,
            }),
            sheet: None,
            create_table: true,
            conflict_policy: None,
            dry_run: true,
            resume_from_row: 0,
            type_mappings: Default::default(),
            sql_server: None,
        };
        sift_server::transfer::execute_recipe(
            &store,
            &metadata,
            PrincipalId(1),
            &recipe,
            request.clone(),
        )
        .await
        .unwrap();
        let db = rusqlite::Connection::open(&path).unwrap();
        assert!(db.prepare("SELECT * FROM parquet_copy").is_err());
        let mut apply = request;
        apply.dry_run = false;
        sift_server::transfer::execute_recipe(
            &store,
            &metadata,
            PrincipalId(1),
            &recipe,
            apply.clone(),
        )
        .await
        .unwrap();
        let row = db
            .query_row("SELECT id,label FROM parquet_copy", [], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })
            .unwrap();
        assert_eq!(row, (1, "kept".into()));
        db.execute_batch("CREATE UNIQUE INDEX copy_id ON parquet_copy(id)")
            .unwrap();
        apply.create_table = false;
        assert!(sift_server::transfer::execute_recipe(
            &store,
            &metadata,
            PrincipalId(1),
            &recipe,
            apply
        )
        .await
        .is_err());
        assert_eq!(
            db.query_row("SELECT count(*) FROM parquet_copy", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    let tx = client
        .begin_transaction(
            session,
            connection,
            TxMode {
                isolation: IsolationLevel::Serializable,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    client
        .create_savepoint(session, connection, tx.tx_id, "outer")
        .await
        .unwrap();
    client
        .execute_in_tx(session, &tx, "INSERT INTO items VALUES(2,'rolled back')")
        .await
        .unwrap();
    client
        .create_savepoint(session, connection, tx.tx_id, "inner")
        .await
        .unwrap();
    client
        .rollback_to_savepoint(session, connection, tx.tx_id, "outer")
        .await
        .unwrap();
    client
        .release_savepoint(session, connection, tx.tx_id, "outer")
        .await
        .unwrap();
    client
        .commit_transaction(session, connection, tx.tx_id)
        .await
        .unwrap();
    let preview_sql =
        sift_snippets::table_preview_sql(&Engine::Sqlite.provider_id(), "main", "items")
            .expect("SQLite table previews are supported");
    assert_eq!(
        client
            .execute(session, connection, &preview_sql)
            .await
            .unwrap()
            .rows
            .len(),
        1
    );
    client
        .execute(
            session,
            connection,
            "CREATE TEMP TABLE local_temp(value TEXT)",
        )
        .await
        .unwrap();
    let graph = client
        .catalog_graph(session, connection, CatalogGraphRequest::default())
        .await
        .unwrap();
    assert_eq!(graph.data.coverage.state, CatalogCoverageState::Partial);
    assert!(graph.data.nodes.iter().any(|n| n.name == "local_temp"));
    let sql = "SELECT i.la FROM items i";
    let completion = client
        .complete(
            session,
            connection,
            completion::CompletionRequest {
                sql: sql.into(),
                cursor: 11,
                limit: Some(20),
            },
        )
        .await
        .unwrap();
    assert!(completion.candidates.iter().any(|i| i.label == "label"));
    let plan = client
        .explain(
            session,
            connection,
            ExplainRequest {
                connection,
                sql: "SELECT * FROM items WHERE id=?1".into(),
                params: vec![Value::Int64(1)],
                analyze: false,
            },
        )
        .await
        .unwrap();
    assert!(!plan.root.children.is_empty());
    assert!(plan.root.est_cost.is_none());
    assert!(plan.raw.contains("SEARCH"));
    let request = |data: &str| CsvImportRequest {
        table: "main.items".into(),
        data: data.as_bytes().to_vec(),
        header: true,
        delimiter: ',',
        null_value: Some("NULL".into()),
        create_table: false,
        conflict_policy: CsvConflictPolicy::Abort,
        dry_run: false,
        resume_from_row: 0,
        type_mappings: Default::default(),
        sql_server: None,
    };
    let mut late_null = String::from("id,label\n");
    for id in 1..=1000 {
        late_null.push_str(&format!("{id},ok\n"));
    }
    late_null.push_str("1001,NULL\n");
    let mut atomic_ddl = request(&late_null);
    atomic_ddl.table = "main.csv_atomic_ddl".into();
    atomic_ddl.create_table = true;
    assert!(client
        .import_csv(session, connection, atomic_ddl)
        .await
        .is_err());
    assert_eq!(
        rusqlite::Connection::open(&path)
            .unwrap()
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name='csv_atomic_ddl'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    let changes = metadata
        .change_ledger(&ChangeLedgerFilter::default())
        .unwrap()
        .entries
        .into_iter()
        .filter(|entry| entry.affected_object.as_deref() == Some("main.csv_atomic_ddl"))
        .collect::<Vec<_>>();
    assert_eq!(changes.len(), 2);
    assert!(changes
        .iter()
        .all(|entry| entry.outcome == ChangeLedgerOutcome::RolledBack
            && entry.transaction_id.is_some()));
    assert!(client
        .import_csv(
            session,
            connection,
            request("id,label\n2,new\n1,conflict\n")
        )
        .await
        .is_err());
    let csv_changes = metadata
        .change_ledger(&ChangeLedgerFilter::default())
        .unwrap()
        .entries
        .into_iter()
        .filter(|entry| {
            entry.operation == ChangeLedgerOperation::CsvImport
                && entry.affected_object.as_deref() == Some("main.items")
        })
        .collect::<Vec<_>>();
    assert_eq!(csv_changes.len(), 1);
    assert_eq!(csv_changes[0].outcome, ChangeLedgerOutcome::RolledBack);
    assert!(csv_changes[0].transaction_id.is_some());
    assert_eq!(
        client
            .execute(session, connection, "SELECT * FROM items")
            .await
            .unwrap()
            .rows
            .len(),
        1
    );
    let imported = client
        .import_csv(session, connection, request("id,label\n2,new\n3,more\n"))
        .await
        .unwrap();
    assert_eq!(imported.rows_inserted, 2);
    let mut skip = request("id,label\n2,duplicate\n4,new\n");
    skip.conflict_policy = CsvConflictPolicy::Skip;
    let imported = client.import_csv(session, connection, skip).await.unwrap();
    assert_eq!((imported.rows_inserted, imported.rows_skipped), (1, 1));
    let mut quarantine = request("id,label\n2,NULL\n4,duplicate\n");
    quarantine.conflict_policy = CsvConflictPolicy::Quarantine;
    let rejected = client
        .import_csv(session, connection, quarantine)
        .await
        .unwrap();
    assert_eq!((rejected.rows_inserted, rejected.rows_skipped), (0, 2));
    assert_eq!(rejected.quarantined_rows[0].row_number, 0);
    assert_eq!(
        rejected.quarantined_rows[0].values,
        vec![Some("2".into()), None]
    );
    assert_eq!(
        client
            .execute(
                session,
                connection,
                "UPDATE items SET label='none' WHERE id=999"
            )
            .await
            .unwrap()
            .affected_rows,
        Some(0)
    );

    let edits = EditSet {
        table: ObjectPath::new("items"),
        edits: vec![RowEdit::Update {
            key: RowKey {
                columns: vec![CellEdit {
                    column: "id".into(),
                    value: Value::Int64(1),
                }],
            },
            changes: vec![CellEdit {
                column: "label".into(),
                value: Value::Text("edited".into()),
            }],
            expected: vec![CellEdit {
                column: "label".into(),
                value: Value::Text("kept".into()),
            }],
        }],
    };
    let preview = client
        .preview_edits(
            session,
            connection,
            PreviewEditsRequest {
                connection,
                edit_set: edits.clone(),
            },
        )
        .await
        .unwrap();
    assert!(preview.statements[0].sql.contains("?1"));
    let applied = client
        .apply_edits(
            session,
            connection,
            ApplyEditsRequest {
                connection,
                edit_set: edits.clone(),
                tx: None,
            },
        )
        .await
        .unwrap();
    assert!(applied.committed);
    assert_eq!(applied.applied[0].affected_rows, 1);
    assert!(client
        .apply_edits(
            session,
            connection,
            ApplyEditsRequest {
                connection,
                edit_set: edits,
                tx: None
            }
        )
        .await
        .is_err());
    assert!(client
        .preview_edits(
            session,
            connection,
            PreviewEditsRequest {
                connection,
                edit_set: EditSet {
                    table: ObjectPath::new("nullable"),
                    edits: vec![]
                }
            }
        )
        .await
        .is_err());
    let refreshed = client
        .catalog_graph(session, connection, CatalogGraphRequest::default())
        .await
        .unwrap();
    assert_eq!(refreshed.revision, graph.revision);
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch("ALTER TABLE items ADD COLUMN external TEXT")
        .unwrap();
    assert_ne!(
        client
            .catalog_graph(session, connection, CatalogGraphRequest::default())
            .await
            .unwrap()
            .revision,
        graph.revision
    );
    let tx = client
        .begin_transaction(
            session,
            connection,
            TxMode {
                isolation: IsolationLevel::Serializable,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let stream = client.start_query_stream_with(session,connection,"WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<100000000) SELECT sum(x) FROM n",vec![],Some(TxHandleRef {tx_id:tx.tx_id,connection:tx.connection,mode:tx.mode})).await.unwrap();
    client
        .cancel(session, connection, stream.cursor_id())
        .await
        .unwrap();
    assert!(client.list_transactions(session).await.unwrap().is_empty());
    assert!(client
        .execute(session, connection, "SELECT 1")
        .await
        .is_err());
    drop(stream);
    client.close_session(session).await.unwrap();
    // Resume must be safe even without a unique constraint on imported rows.
    let session = client.open_session(None).await.unwrap().id;
    let connection = client
        .open_connection_from_profile(
            session,
            OpenConnectionFromProfileRequest {
                tenant_id: 1,
                profile_id: profile.id.0,
            },
        )
        .await
        .unwrap()
        .id;
    client.execute(session, connection, "CREATE TABLE resume_rows(value TEXT); CREATE TRIGGER pause_resume BEFORE INSERT ON resume_rows WHEN NEW.value = '100' BEGIN SELECT RAISE(ABORT, 'pause'); END;").await.unwrap();
    let source = format!(
        "value\n{}",
        (0..150).map(|n| format!("{n}\n")).collect::<String>()
    );
    let mut resumable = request(&source);
    resumable.table = "main.resume_rows".into();
    let run_id = "8433f24a-2465-4a26-a6f9-cb24012aee08";
    let ledger = "main.resume_checkpoint";
    let authority = "workspace:1:recipe:1:actor:1";
    let mut preview = resumable.clone();
    preview.dry_run = true;
    assert!(
        sift_server::csv_import::import_with_checkpoint(
            &store, session, connection, preview, ledger, run_id, authority
        )
        .await
        .unwrap()
        .dry_run
    );
    assert!(sift_server::csv_import::import_with_checkpoint(
        &store,
        session,
        connection,
        resumable.clone(),
        ledger,
        run_id,
        authority
    )
    .await
    .is_err());
    let persisted = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        persisted
            .query_row("SELECT count(*) FROM resume_rows", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        100
    );
    assert_eq!(
        persisted
            .query_row("SELECT next_row FROM resume_checkpoint", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        100
    );
    drop(persisted);
    client
        .execute(session, connection, "DROP TRIGGER pause_resume")
        .await
        .unwrap();
    let other_connection = client
        .open_connection_from_profile(
            session,
            OpenConnectionFromProfileRequest {
                tenant_id: 1,
                profile_id: profile.id.0,
            },
        )
        .await
        .unwrap()
        .id;
    let resumed = sift_server::csv_import::import_with_checkpoint(
        &store,
        session,
        other_connection,
        resumable.clone(),
        ledger,
        run_id,
        authority,
    )
    .await
    .unwrap();
    assert_eq!((resumed.rows_inserted, resumed.resume_from_row), (50, 150));
    let replayed = sift_server::csv_import::import_with_checkpoint(
        &store,
        session,
        other_connection,
        resumable.clone(),
        ledger,
        run_id,
        authority,
    )
    .await
    .unwrap();
    assert_eq!(replayed.rows_inserted, 0);
    let mut changed = resumable.clone();
    changed.data.extend_from_slice(b"151\n");
    assert!(sift_server::csv_import::import_with_checkpoint(
        &store,
        session,
        other_connection,
        changed,
        ledger,
        run_id,
        authority
    )
    .await
    .is_err());
    assert!(sift_server::csv_import::import_with_checkpoint(
        &store,
        session,
        other_connection,
        resumable,
        ledger,
        run_id,
        "different actor"
    )
    .await
    .is_err());
    let persisted = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        persisted
            .query_row("SELECT count(*) FROM resume_rows", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        150
    );
    drop(persisted);
    client
        .close_connection(session, other_connection)
        .await
        .unwrap();
    client.close_session(session).await.unwrap();
    server.abort();
}

#[tokio::test]
async fn ai_row_proposals_require_review_confirm_production_and_replay_once() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("ai-review.db");
    let database = rusqlite::Connection::open(&path).unwrap();
    database.execute_batch("CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT NOT NULL); INSERT INTO items VALUES (1,'original'),(2,'remove');").unwrap();
    let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
    metadata.bootstrap_local("AI database review").unwrap();
    let profile = metadata.upsert_connection_profile(TenantId(1),PrincipalId(1),NewConnectionProfile {
        name:"Production fixture".into(),provider_id:Engine::Sqlite.provider_id(),semantic_engine:Some(Engine::Sqlite),
        configuration:serde_json::json!({"root_id":"test","path":"ai-review.db","mode":"read_write"}),
        credentials:None,credential_mode:CredentialMode::Shared,tags:vec!["production".into()],
    }).await.unwrap();
    let driver = SqliteDriver::with_files(FilePolicy {
        config: SqliteDriverConfig {
            roots: std::collections::BTreeMap::from([(
                "test".into(),
                SqliteRootConfig {
                    path: root.path().to_str().unwrap().into(),
                    allowed_tenants: vec![1],
                    read_only: false,
                },
            )]),
            max_connections: 3,
        },
        protected: vec![],
    });
    let mut auth = AuthState::default();
    auth.ai.enabled = true;
    let router = app(AppState {
        sessions: SessionStore::new(DriverRegistry::builder().register(driver).build()),
        rooms: RoomRuntime::default(),
        shutdown: Default::default(),
        auth,
        metadata: Some(metadata.clone()),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = sift_client_sdk::Client::new(format!("http://{addr}"));
    let session = client.open_session(None).await.unwrap().id;
    let connection = client
        .open_connection_from_profile(
            session,
            OpenConnectionFromProfileRequest {
                tenant_id: 1,
                profile_id: profile.id.0,
            },
        )
        .await
        .unwrap()
        .id;
    let chat = client
        .create_ai_chat(&CreateAiChatRequest {
            tenant_id: 1,
            room_id: None,
            title: "Review rows".into(),
        })
        .await
        .unwrap();
    let lease = client
        .start_ai_turn(
            chat.id,
            &StartAiTurnRequest {
                attachment_previews: Vec::new(),
                client_request_id: uuid::Uuid::new_v4(),
                desktop_id: uuid::Uuid::new_v4(),
                prompt: "Propose changes".into(),
                provider: AiProvider::Codex,
                model: None,
                mode: AiMode::Propose,
                context: AiTurnContext {
                    reasoning_effort: None,
                    external_sources: Vec::new(),
                    inclusion: Default::default(),
                    workspace: None,
                    attachments: Vec::new(),
                    target: ToolContext {
                        tenant_id: Some(1),
                        room_id: None,
                        profile_id: Some(profile.id.0),
                        connection_id: Some(format!("{}:{}", session.0, connection.0)),
                        document_id: None,
                    },
                    editor_item_id: None,
                    database: None,
                    dialect: Some("sqlite".into()),
                    environment_label: None,
                    sql: None,
                    current_error: None,
                    staged_change_count: 0,
                    publication_id: None,
                },
            },
        )
        .await
        .unwrap();
    let catalog = client
        .invoke_ai_tool(
            lease.run.id,
            &InvokeAiToolRequest {
                parameters: None,
                call_id: uuid::Uuid::new_v4(),
                lease_token: lease.lease_token,
                tool: AiToolKind::Catalog,
                sql: None,
            },
        )
        .await
        .unwrap();
    let graph: CatalogGraph = serde_json::from_value(catalog.result).unwrap();
    let invoke = |tool, parameters| {
        let client = client.clone();
        let run_id = lease.run.id;
        let lease_token = lease.lease_token;
        async move {
            client
                .invoke_ai_tool(
                    run_id,
                    &InvokeAiToolRequest {
                        call_id: uuid::Uuid::new_v4(),
                        lease_token,
                        tool,
                        sql: None,
                        parameters,
                    },
                )
                .await
        }
    };
    let object = graph
        .data
        .nodes
        .iter()
        .find(|node| node.kind == CatalogNodeKind::Table && node.name == "items")
        .unwrap();
    let ddl = invoke(
        AiToolKind::ObjectDdl,
        Some(AiToolParameters::ObjectDdl {
            expected_catalog_revision: graph.revision,
            object_id: object.id.clone(),
        }),
    )
    .await
    .unwrap();
    assert!(ddl.result["object"]["ddl"]
        .as_str()
        .unwrap()
        .contains("CREATE TABLE"));
    assert!(invoke(
        AiToolKind::ObjectDdl,
        Some(AiToolParameters::ObjectDdl {
            expected_catalog_revision: CatalogRevision(graph.revision.0 + 1),
            object_id: object.id.clone()
        })
    )
    .await
    .is_err());
    let peer = metadata
        .create_principal("history-peer", "peer", None)
        .unwrap()
        .id;
    metadata
        .upsert_tenant_membership(TenantId(1), peer, sift_metadata::MembershipRole::Member)
        .unwrap();
    let other_profile=metadata.upsert_connection_profile(TenantId(1),PrincipalId(1),NewConnectionProfile {
        name:"Other history source".into(),provider_id:Engine::Sqlite.provider_id(),semantic_engine:Some(Engine::Sqlite),
        configuration:serde_json::json!({"root_id":"test","path":"ai-review.db","mode":"read_write"}),
        credentials:None,credential_mode:CredentialMode::Shared,tags:vec![],
    }).await.unwrap();
    for (actor, profile_id, sql) in [
        (peer, profile.id, "SELECT 'peer-only'"),
        (PrincipalId(1), other_profile.id, "SELECT 'other-profile'"),
    ] {
        metadata
            .record_query_history(sift_metadata::NewQueryHistory {
                principal_id: actor,
                room_id: None,
                connection_profile_id: Some(profile_id),
                sql_text: sql.into(),
                duration_ms: Some(1),
                row_count: None,
                status: sift_metadata::QueryStatus::Error,
                error_code: Some("fixture_error".into()),
                error_message: Some("Source-specific runtime error".into()),
                variable_descriptors: vec![],
            })
            .unwrap();
    }
    let scoped_history = invoke(AiToolKind::QueryHistory, None).await.unwrap().result;
    assert!(!scoped_history.to_string().contains("peer-only"));
    assert!(!scoped_history.to_string().contains("other-profile"));
    for index in 0..24 {
        metadata
            .record_query_history(sift_metadata::NewQueryHistory {
                principal_id: PrincipalId(1),
                room_id: None,
                connection_profile_id: Some(profile.id),
                sql_text: format!("SELECT {index}"),
                duration_ms: Some(1),
                row_count: None,
                status: sift_metadata::QueryStatus::Error,
                error_code: Some("fixture_error".into()),
                error_message: Some("Owned runtime error".into()),
                variable_descriptors: vec![],
            })
            .unwrap();
    }
    let history = invoke(AiToolKind::QueryHistory, None).await.unwrap().result;
    assert_eq!(history["items"].as_array().unwrap().len(), 20);
    assert_eq!(history["truncated"], true);
    let text = history.to_string();
    assert!(!text.contains("peer-only"));
    assert!(!text.contains("other-profile"));
    assert!(text.contains("Owned runtime error"));
    let mut capture = PlanCapture {
        id: PlanCaptureId(uuid::Uuid::new_v4()),
        tenant_id: 1,
        connection_profile_id: profile.id.0,
        creator_principal_id: 1,
        provider: graph.provider.clone(),
        server_version: "fixture".into(),
        engine: Engine::Sqlite,
        source_digest: format!("sha256:{}", "a".repeat(64)),
        document_revision: 1,
        statement_id: "fixture-statement".into(),
        statement_fingerprint: format!("sha256:{}", "b".repeat(64)),
        catalog_revision: graph.revision,
        analyzed: true,
        captured_at: chrono::Utc::now(),
        duration_ms: 1,
        root: PlanNode::new("SCAN items"),
        warnings: vec![],
        complete: true,
        revision: 1,
        raw_response: None,
        source: None,
    };
    capture.root.actual_rows = Some(2.0);
    capture.root.children = (0..200)
        .map(|_| {
            let mut child = PlanNode::new("owned child".repeat(30));
            child.relation = Some("long relation".repeat(80));
            child.extra.insert(
                "bounded_native_detail".into(),
                serde_json::json!("x".repeat(900)),
            );
            child
        })
        .collect();
    metadata.create_plan_capture(&capture).unwrap();
    let owned_id = capture.id;
    capture.id = PlanCaptureId(uuid::Uuid::new_v4());
    capture.creator_principal_id = peer.0;
    metadata.create_plan_capture(&capture).unwrap();
    let summaries = invoke(AiToolKind::PlanCaptures, None).await.unwrap().result;
    assert_eq!(summaries["items"].as_array().unwrap().len(), 1);
    assert_eq!(summaries["items"][0]["id"], serde_json::json!(owned_id));
    let saved = invoke(
        AiToolKind::PlanCapture,
        Some(AiToolParameters::PlanCapture {
            capture_id: owned_id,
        }),
    )
    .await
    .unwrap()
    .result;
    assert_eq!(saved["capture"]["analyzed"], true);
    assert_eq!(saved["truncated"], true);
    assert!(
        saved["capture"]["root"]["children"]
            .as_array()
            .unwrap()
            .len()
            < 200
    );
    assert!(saved["capture"]["raw_response"].is_null());
    assert!(invoke(
        AiToolKind::PlanCapture,
        Some(AiToolParameters::PlanCapture {
            capture_id: capture.id
        })
    )
    .await
    .is_err());

    let cell = |column: &str, value: Value| CellEdit {
        column: column.into(),
        value,
    };
    let key = |id| RowKey {
        columns: vec![cell("id", Value::Int64(id))],
    };
    let draft = AiDatabaseDraft::RowEditSet {
        expected_catalog_revision: graph.revision,
        edit_set: EditSet {
            table: ObjectPath {
                catalog: None,
                schema: Some("main".into()),
                name: "items".into(),
                kind: Some(ObjectKind::Table),
                routine_args: None,
            },
            edits: vec![
                RowEdit::Insert {
                    values: vec![cell("label", Value::Text("created".into()))],
                },
                RowEdit::Update {
                    key: key(1),
                    changes: vec![cell("label", Value::Text("updated".into()))],
                    expected: vec![cell("label", Value::Text("original".into()))],
                },
                RowEdit::Delete {
                    key: key(2),
                    expected: vec![cell("label", Value::Text("remove".into()))],
                },
            ],
        },
    };
    let request = StageAiDatabaseProposalRequest {
        client_request_id: uuid::Uuid::new_v4(),
        lease_token: lease.lease_token,
        draft,
    };
    let proposal = client
        .stage_ai_database_proposal(lease.run.id, &request)
        .await
        .unwrap();
    assert_eq!(
        client
            .stage_ai_database_proposal(lease.run.id, &request)
            .await
            .unwrap()
            .proposal
            .id,
        proposal.proposal.id
    );
    let review_connection = client
        .open_connection_from_profile(
            session,
            OpenConnectionFromProfileRequest {
                tenant_id: 1,
                profile_id: profile.id.0,
            },
        )
        .await
        .unwrap()
        .id;
    assert_eq!(
        client
            .catalog_graph(session, review_connection, CatalogGraphRequest::default())
            .await
            .unwrap()
            .database_identity,
        graph.database_identity
    );
    let review = client
        .review_ai_database_proposal(
            proposal.proposal.id,
            &ReviewAiDatabaseProposalRequest {
                client_request_id: uuid::Uuid::new_v4(),
                session,
                connection: review_connection,
            },
        )
        .await
        .unwrap();
    assert!(review.production);
    let AiDatabasePreview::RowEditSet { plan } = &review.preview else {
        panic!("expected row preview")
    };
    assert_eq!(plan.statements.len(), 3);
    assert_eq!(
        database
            .query_row("SELECT label FROM items WHERE id=1", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "original"
    );
    assert_eq!(
        database
            .query_row("SELECT COUNT(*) FROM items", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        2
    );
    let mut apply = ApplyAiDatabaseProposalRequest {
        client_request_id: uuid::Uuid::new_v4(),
        review_id: review.id,
        review_digest: review.review_digest.clone(),
        production_confirmation: None,
        acknowledgements: vec![],
    };
    assert!(client
        .apply_ai_database_proposal(proposal.proposal.id, &apply)
        .await
        .is_err());
    assert!(client.ai_database_proposals(chat.id).await.unwrap()[0]
        .apply_state
        .is_none());
    apply.production_confirmation = Some(review.database_label.clone());
    let concurrent = futures::future::join_all(
        (0..8).map(|_| client.apply_ai_database_proposal(proposal.proposal.id, &apply)),
    )
    .await;
    let receipts = concurrent
        .into_iter()
        .map(Result::unwrap)
        .collect::<Vec<_>>();
    assert!(receipts
        .iter()
        .any(|receipt| receipt.state == AiDatabaseApplyState::Applied));
    assert!(receipts.iter().all(|receipt| matches!(
        receipt.state,
        AiDatabaseApplyState::Applying | AiDatabaseApplyState::Applied
    )));
    let replay = client
        .apply_ai_database_proposal(proposal.proposal.id, &apply)
        .await
        .unwrap();
    assert_eq!(replay.state, AiDatabaseApplyState::Applied);
    assert_eq!(
        database
            .query_row(
                "SELECT COUNT(*) FROM items WHERE label='created'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        database
            .query_row("SELECT label FROM items WHERE id=1", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "updated"
    );
    assert_eq!(
        database
            .query_row(
                "SELECT COUNT(*) FROM items WHERE label='remove'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    let ledger = client
        .change_ledger(&ChangeLedgerFilter::default())
        .await
        .unwrap();
    assert_eq!(
        ledger
            .entries
            .iter()
            .filter(|entry| entry.source_workflow == "ai_row_proposal")
            .count(),
        3
    );
    assert!(ledger
        .entries
        .iter()
        .all(|entry| entry.authored_by == Some(1)
            && entry.approved_by == Some(1)
            && entry.executed_by == 1));
    apply.client_request_id = uuid::Uuid::new_v4();
    assert!(client
        .apply_ai_database_proposal(proposal.proposal.id, &apply)
        .await
        .is_err());
    assert!(client
        .discard_ai_database_proposal(proposal.proposal.id)
        .await
        .is_err());
    // A schema change after staging must invalidate human review before any write.
    let stale = client
        .stage_ai_database_proposal(
            lease.run.id,
            &StageAiDatabaseProposalRequest {
                client_request_id: uuid::Uuid::new_v4(),
                lease_token: lease.lease_token,
                draft: AiDatabaseDraft::RowEditSet {
                    expected_catalog_revision: graph.revision,
                    edit_set: EditSet {
                        table: ObjectPath {
                            schema: Some("main".into()),
                            kind: Some(ObjectKind::Table),
                            ..ObjectPath::new("items")
                        },
                        edits: vec![RowEdit::Insert {
                            values: vec![cell("label", Value::Text("must not execute".into()))],
                        }],
                    },
                },
            },
        )
        .await
        .unwrap();
    database
        .execute_batch("ALTER TABLE items ADD COLUMN changed TEXT")
        .unwrap();
    assert!(client
        .review_ai_database_proposal(
            stale.proposal.id,
            &ReviewAiDatabaseProposalRequest {
                client_request_id: uuid::Uuid::new_v4(),
                session,
                connection
            }
        )
        .await
        .is_err());
    assert_eq!(
        database
            .query_row(
                "SELECT COUNT(*) FROM items WHERE label='must not execute'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    server.abort();
}

#[tokio::test]
async fn ai_attachments_bind_exact_executions_and_reject_forged_or_stale_previews() {
    let root = tempfile::tempdir().unwrap();
    rusqlite::Connection::open(root.path().join("attachments.db")).unwrap()
        .execute_batch("CREATE TABLE samples(id INTEGER PRIMARY KEY,label TEXT); INSERT INTO samples VALUES(1,'first'),(2,'second');").unwrap();
    let metadata = MetadataStore::open_in_memory(Arc::new(MemorySecretStore::new())).unwrap();
    metadata.bootstrap_local("AI attachments").unwrap();
    let profile = metadata.upsert_connection_profile(TenantId(1),PrincipalId(1),NewConnectionProfile {
        name:"Attachment source".into(),provider_id:Engine::Sqlite.provider_id(),semantic_engine:Some(Engine::Sqlite),
        configuration:serde_json::json!({"root_id":"test","path":"attachments.db","mode":"read_write"}),
        credentials:None,credential_mode:CredentialMode::Shared,tags:vec![],
    }).await.unwrap();
    let driver = SqliteDriver::with_files(FilePolicy {
        config: SqliteDriverConfig {
            roots: std::collections::BTreeMap::from([(
                "test".into(),
                SqliteRootConfig {
                    path: root.path().to_str().unwrap().into(),
                    allowed_tenants: vec![1],
                    read_only: false,
                },
            )]),
            max_connections: 3,
        },
        protected: vec![],
    });
    let mut auth = AuthState::default();
    auth.ai.enabled = true;
    let sessions = SessionStore::new(DriverRegistry::builder().register(driver).build());
    let router = app(AppState {
        sessions: sessions.clone(),
        rooms: RoomRuntime::default(),
        shutdown: Default::default(),
        auth,
        metadata: Some(metadata.clone()),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = sift_client_sdk::Client::new(format!("http://{addr}"));
    let session = client.open_session(None).await.unwrap().id;
    let connection = client
        .open_connection_from_profile(
            session,
            OpenConnectionFromProfileRequest {
                tenant_id: 1,
                profile_id: profile.id.0,
            },
        )
        .await
        .unwrap()
        .id;
    let chat = client
        .create_ai_chat(&CreateAiChatRequest {
            tenant_id: 1,
            room_id: None,
            title: "Exact sources".into(),
        })
        .await
        .unwrap();
    let target = ToolContext {
        tenant_id: Some(1),
        room_id: None,
        profile_id: Some(profile.id.0),
        connection_id: Some(format!("{}:{}", session.0, connection.0)),
        document_id: None,
    };
    let mut stream = client
        .start_query_stream_with(
            session,
            connection,
            "SELECT label AS repeated, id AS repeated FROM samples WHERE id >= ? ORDER BY id",
            vec![Value::Int64(1)],
            None,
        )
        .await
        .unwrap();
    let result_id = stream
        .ai_result_id()
        .expect("managed websocket execution is retained");
    let mut columns = Vec::new();
    loop {
        let (seq, page) = stream.next_page().await.unwrap();
        if let Page::NextResult {
            columns: source, ..
        } = &page
        {
            columns = source.clone();
        }
        let done = matches!(page, Page::Done { .. });
        stream.acknowledge(seq).await.unwrap();
        if done {
            break;
        }
    }
    let digest = sift_server::comparison::schema_digest(&columns);
    let source = AiAttachmentSource::QueryRows {
        result_id,
        result_set: 0,
        schema_digest: digest.clone(),
        row_ordinals: vec![1, 0],
        column_indices: vec![1, 0],
    };
    let request = PreviewAiAttachmentRequest {
        target: target.clone(),
        source: source.clone(),
    };
    let preview = client
        .preview_ai_attachment(chat.id, &request)
        .await
        .unwrap();
    assert_eq!(
        preview.attachment.content["rows"][0],
        serde_json::json!(Row::new(vec![
            Value::Int64(2),
            Value::Text("second".into())
        ]))
    );
    assert_eq!(preview.attachment.content["columns"][0]["name"], "repeated");
    assert_eq!(preview.attachment.content["columns"][1]["name"], "repeated");
    assert_eq!(
        preview.attachment.content["executed_sql"],
        "SELECT label AS repeated, id AS repeated FROM samples WHERE id >= ? ORDER BY id"
    );
    assert!(preview.attachment.content.get("params").is_none());
    assert!(!preview.requires_publication_ack);
    // A later execution cannot change the immutable snapshot reviewed above.
    let later = client
        .execute(
            session,
            connection,
            "SELECT 'later' AS repeated, 99 AS repeated",
        )
        .await
        .unwrap();
    assert_ne!(later.ai_result_id, Some(result_id));
    assert_eq!(
        client
            .preview_ai_attachment(chat.id, &request)
            .await
            .unwrap()
            .attachment
            .sha256,
        preview.attachment.sha256
    );
    let mut turn = StartAiTurnRequest {
        client_request_id: uuid::Uuid::new_v4(),
        desktop_id: uuid::Uuid::new_v4(),
        prompt: "Explain my selection".into(),
        provider: AiProvider::Codex,
        model: None,
        mode: AiMode::Read,
        context: AiTurnContext {
            reasoning_effort: None,
            external_sources: Vec::new(),
            inclusion: Default::default(),
            workspace: None,
            target: target.clone(),
            attachments: vec![AiContextAttachment {
                source: source.clone(),
                label: "forged".into(),
                content: serde_json::json!({"credential":"forged-inline-body"}),
                sha256: "forged".into(),
                truncated: false,
                origin_visibility: AiVisibility::Private,
                published_by: None,
            }],
            editor_item_id: None,
            database: None,
            dialect: Some("sqlite".into()),
            environment_label: None,
            sql: None,
            current_error: None,
            staged_change_count: 0,
            publication_id: None,
        },
        attachment_previews: vec![],
    };
    let lease = client.start_ai_turn(chat.id, &turn).await.unwrap();
    assert!(client.ai_runs(chat.id).await.unwrap()[0]
        .context
        .attachments
        .is_empty());
    client
        .finish_ai_run(
            lease.run.id,
            &FinishAiRunRequest {
                lease_token: lease.lease_token,
                status: AiRunStatus::Completed,
            },
        )
        .await
        .unwrap();
    turn.client_request_id = uuid::Uuid::new_v4();
    turn.attachment_previews = vec![AcceptAiAttachment {
        preview_id: preview.id,
        expected_sha256: "wrong".into(),
        publish_to_room: false,
    }];
    assert!(client.start_ai_turn(chat.id, &turn).await.is_err());
    turn.attachment_previews[0].expected_sha256 = preview.attachment.sha256.clone();
    let lease = client.start_ai_turn(chat.id, &turn).await.unwrap();
    let persisted = client
        .ai_runs(chat.id)
        .await
        .unwrap()
        .into_iter()
        .find(|detail| detail.run.id == lease.run.id)
        .unwrap();
    assert_eq!(
        persisted.context.attachments,
        vec![preview.attachment.clone()]
    );
    assert!(!serde_json::to_string(&persisted)
        .unwrap()
        .contains("forged-inline-body"));
    client
        .finish_ai_run(
            lease.run.id,
            &FinishAiRunRequest {
                lease_token: lease.lease_token,
                status: AiRunStatus::Completed,
            },
        )
        .await
        .unwrap();
    let mut wrong = request.clone();
    if let AiAttachmentSource::QueryRows { schema_digest, .. } = &mut wrong.source {
        *schema_digest = "wrong".into();
    }
    assert!(client.preview_ai_attachment(chat.id, &wrong).await.is_err());
    if let AiAttachmentSource::QueryRows {
        schema_digest,
        row_ordinals,
        ..
    } = &mut wrong.source
    {
        *schema_digest = digest;
        *row_ordinals = vec![256];
    }
    assert!(client.preview_ai_attachment(chat.id, &wrong).await.is_err());
    let second_connection = client
        .open_connection_from_profile(
            session,
            OpenConnectionFromProfileRequest {
                tenant_id: 1,
                profile_id: profile.id.0,
            },
        )
        .await
        .unwrap()
        .id;
    wrong = request.clone();
    wrong.target.connection_id = Some(format!("{}:{}", session.0, second_connection.0));
    assert!(client.preview_ai_attachment(chat.id, &wrong).await.is_err());
    let blocked = reqwest::Client::new().put(format!("http://{addr}/v1/metadata/connections/{}/policy", profile.id.0))
        .json(&serde_json::json!({"expected_revision":0,"minimum_tenant_role":"member","read_only":false,"blocked_ops":["execute_query"]}))
        .send().await.unwrap();
    assert_eq!(blocked.status(), reqwest::StatusCode::OK);
    turn.client_request_id = uuid::Uuid::new_v4();
    assert!(
        client.start_ai_turn(chat.id, &turn).await.is_err(),
        "review does not bypass newly revoked reads"
    );
    assert!(client
        .preview_ai_attachment(chat.id, &request)
        .await
        .is_err());
    client.close_connection(session, connection).await.unwrap();
    turn.client_request_id = uuid::Uuid::new_v4();
    assert!(client.start_ai_turn(chat.id, &turn).await.is_err());
    assert!(client
        .preview_ai_attachment(chat.id, &request)
        .await
        .is_err());
    client.close_session(session).await.unwrap();
    server.abort();
}

async fn sqlite_add_request(
    client: &sift_client_sdk::Client,
    session: SessionId,
    connection: ConnectionId,
    name: &str,
    declared: &str,
) -> PreviewCatalogDiagramMutationRequest {
    let live = client
        .catalog_graph(
            session,
            connection,
            CatalogGraphRequest {
                refresh: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let table = live
        .data
        .nodes
        .iter()
        .find(|n| n.kind == CatalogNodeKind::Table && n.name == "addition_data")
        .unwrap();
    PreviewCatalogDiagramMutationRequest {
        expected_catalog_revision: live.revision,
        mutation: CatalogDiagramMutation::AddColumn {
            table_id: table.id.clone(),
            name: name.into(),
            type_ref: TypeRef::Native {
                provider_id: Engine::Sqlite.provider_id(),
                name: declared.into(),
                category: TypeCategory::Other,
            },
            nullability: Nullability::Nullable,
        },
        options: MigrationOptions {
            prefer_transactional: false,
            ..Default::default()
        },
    }
}
async fn sqlite_add_column_acceptance(
    client: &sift_client_sdk::Client,
    session: SessionId,
    connection: ConnectionId,
    read_only: ConnectionId,
) {
    // Keep setup statements in known transaction scope for journaled apply.
    for sql in ["CREATE TABLE addition_data (id INTEGER NOT NULL,payload BLOB)", "INSERT INTO addition_data(rowid,id,payload) VALUES (91,1,'text'),(92,2,x'00FF'),(93,3,NULL)"] {
        client.execute(session, connection, sql).await.unwrap();
    }
    let original = "SELECT rowid,id,typeof(payload),payload FROM addition_data ORDER BY rowid";
    let expected = client
        .execute(session, connection, original)
        .await
        .unwrap()
        .rows
        .into_iter()
        .map(|r| r.values)
        .collect::<Vec<_>>();
    for (position, kind) in ["INTEGER", "REAL", "TEXT", "BLOB", "NUMERIC"]
        .into_iter()
        .enumerate()
    {
        let prior_rows = client
            .execute(
                session,
                connection,
                "SELECT * FROM addition_data ORDER BY rowid",
            )
            .await
            .unwrap()
            .rows;
        let name = format!("added\"; --{position}");
        let mut request = sqlite_add_request(client, session, connection, &name, kind).await;
        if position == 0 {
            for (bad_name, bad_type, nullable) in [
                ("ID", "TEXT", Nullability::Nullable),
                ("bad\0name", "TEXT", Nullability::Nullable),
                (
                    "bad_type",
                    "TEXT; DROP TABLE addition_data",
                    Nullability::Nullable,
                ),
                ("required", "TEXT", Nullability::NotNullable),
            ] {
                let CatalogDiagramMutation::AddColumn { table_id, .. } = &request.mutation else {
                    unreachable!();
                };
                assert!(client
                    .preview_catalog_diagram_mutation(
                        session,
                        connection,
                        PreviewCatalogDiagramMutationRequest {
                            mutation: CatalogDiagramMutation::AddColumn {
                                table_id: table_id.clone(),
                                name: bad_name.into(),
                                type_ref: TypeRef::Native {
                                    provider_id: Engine::Sqlite.provider_id(),
                                    name: bad_type.into(),
                                    category: TypeCategory::Other
                                },
                                nullability: nullable
                            },
                            ..request.clone()
                        }
                    )
                    .await
                    .is_err());
            }
            let read_request = sqlite_add_request(client, session, read_only, &name, kind).await;
            let plan = client
                .preview_catalog_diagram_mutation(session, read_only, read_request)
                .await
                .unwrap();
            let validation = client
                .validate_migration(
                    session,
                    read_only,
                    ValidateMigrationRequest {
                        plan_id: plan.id,
                        plan_digest: plan.digest.clone(),
                        confirm_test_database: true,
                    },
                )
                .await
                .unwrap();
            assert!(!validation.valid);
            assert!(
                !matches!(client.apply_migration(session,read_only,ApplyMigrationRequest {plan_id:plan.id,plan_digest:plan.digest,acknowledgements:plan.required_acknowledgements,source:None}).await,Ok(run) if run.state==MigrationRunState::Applied)
            );
            let stale = client
                .preview_catalog_diagram_mutation(session, connection, request.clone())
                .await
                .unwrap();
            client
                .execute(
                    session,
                    connection,
                    "CREATE INDEX add_dependency ON addition_data(id)",
                )
                .await
                .unwrap();
            assert!(
                !matches!(client.apply_migration(session,connection,ApplyMigrationRequest {plan_id:stale.id,plan_digest:stale.digest,acknowledgements:stale.required_acknowledgements,source:None}).await,Ok(run) if run.state==MigrationRunState::Applied)
            );
            let restricted = sqlite_add_request(client, session, connection, &name, kind).await;
            assert!(client
                .preview_catalog_diagram_mutation(session, connection, restricted)
                .await
                .is_err());
            for sql in [
                "DROP INDEX add_dependency",
                "CREATE VIEW add_dependency AS SELECT id FROM addition_data",
            ] {
                client.execute(session, connection, sql).await.unwrap();
            }
            let restricted = sqlite_add_request(client, session, connection, &name, kind).await;
            assert!(client
                .preview_catalog_diagram_mutation(session, connection, restricted)
                .await
                .is_err());
            client
                .execute(session, connection, "DROP VIEW add_dependency")
                .await
                .unwrap();
            request = sqlite_add_request(client, session, connection, &name, kind).await;
        }
        let plan = client
            .preview_catalog_diagram_mutation(session, connection, request)
            .await
            .unwrap();
        assert!(plan.groups[0].transactional);
        assert_eq!(plan.groups[0].statements.len(), 1);
        assert!(plan.groups[0].statements[0]
            .sql
            .contains("ADD COLUMN \"added\"\"; --"));
        assert!(
            plan.rollback_groups.is_empty(),
            "populated added columns cannot promise exact data rollback"
        );
        assert!(plan
            .warnings
            .iter()
            .any(|w| w.contains("no automatic rollback SQL")));
        assert_eq!(
            client
                .execute(session, connection, "SELECT * FROM addition_data")
                .await
                .unwrap()
                .columns
                .len(),
            2 + position,
            "preview must not alter the native file"
        );
        let validation = client
            .validate_migration(
                session,
                connection,
                ValidateMigrationRequest {
                    plan_id: plan.id,
                    plan_digest: plan.digest.clone(),
                    confirm_test_database: true,
                },
            )
            .await
            .unwrap();
        assert!(validation.valid && validation.rolled_back);
        assert_eq!(
            client
                .execute(session, connection, "SELECT * FROM addition_data")
                .await
                .unwrap()
                .columns
                .len(),
            2 + position,
            "validation must roll back the added column"
        );
        let run = client
            .apply_migration(
                session,
                connection,
                ApplyMigrationRequest {
                    plan_id: plan.id,
                    plan_digest: plan.digest,
                    acknowledgements: plan.required_acknowledgements,
                    source: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(run.state, MigrationRunState::Applied, "{run:?}");
        let retained = client
            .execute(
                session,
                connection,
                "SELECT * FROM addition_data ORDER BY rowid",
            )
            .await
            .unwrap()
            .rows;
        assert_eq!(retained.len(), prior_rows.len());
        for (after, before) in retained.iter().zip(&prior_rows) {
            assert_eq!(
                &after.values[..before.values.len()],
                before.values.as_slice(),
                "all existing cells, including populated prior additions, must survive"
            );
        }
        let quoted = format!("\"{}\"", name.replace('"', "\"\""));
        assert!(client
            .execute(
                session,
                connection,
                format!("SELECT {quoted} FROM addition_data")
            )
            .await
            .unwrap()
            .rows
            .iter()
            .all(|r| r.values == vec![Value::Null]));
        client
            .execute_with_params(
                session,
                connection,
                format!("UPDATE addition_data SET {quoted}=?1 WHERE rowid=91"),
                vec![Value::Int64(42)],
            )
            .await
            .unwrap();
        assert_eq!(
            client
                .execute(session, connection, original)
                .await
                .unwrap()
                .rows
                .into_iter()
                .map(|r| r.values)
                .collect::<Vec<_>>(),
            expected,
            "rowids, existing values and storage classes must survive"
        );
    }
    // A real native target with two additions exercises coupled selection and ordinal proof.
    for sql in [
        "ALTER TABLE addition_data ADD COLUMN tail_one TEXT",
        "ALTER TABLE addition_data ADD COLUMN tail_two REAL",
    ] {
        client.execute(session, connection, sql).await.unwrap();
    }
    let target = client
        .catalog_graph(
            session,
            connection,
            CatalogGraphRequest {
                refresh: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let snapshot = client
        .create_catalog_snapshot(
            session,
            connection,
            CreateCatalogSnapshotRequest {
                expected_catalog_revision: target.revision,
                options: Default::default(),
                description: None,
                accept_partial: true,
            },
        )
        .await
        .unwrap();
    for sql in [
        "ALTER TABLE addition_data DROP COLUMN tail_two",
        "ALTER TABLE addition_data DROP COLUMN tail_one",
    ] {
        client.execute(session, connection, sql).await.unwrap();
    }
    let live = client
        .catalog_graph(
            session,
            connection,
            CatalogGraphRequest {
                refresh: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let request = SchemaDiffRequest {
        from: CatalogSourceRef::Live {
            expected_revision: live.revision,
            options: Default::default(),
        },
        to: CatalogSourceRef::Snapshot {
            snapshot_id: snapshot.id,
        },
        accepted_renames: vec![],
        max_changes: None,
    };
    let diff = client
        .compare_catalog_schemas(session, connection, request.clone())
        .await
        .unwrap();
    let added = |name: &str| {
        diff.changes
            .iter()
            .find(|c| {
                c.kind == SchemaChangeKind::Create
                    && c.object_after
                        .as_ref()
                        .is_some_and(|n| n.kind == CatalogNodeKind::Column && n.name == name)
            })
            .unwrap()
    };
    let first = added("tail_one");
    let second = added("tail_two");
    assert_eq!(first.reversibility, SchemaChangeReversibility::Lossy);
    let table = diff
        .changes
        .iter()
        .find(|c| {
            c.kind == SchemaChangeKind::Alter
                && c.object_after
                    .as_ref()
                    .is_some_and(|n| n.kind == CatalogNodeKind::Table && n.name == "addition_data")
        })
        .unwrap();
    let preview = |selected_changes| PreviewMigrationRequest {
        diff: request.clone(),
        expected_diff_digest: diff.digest.clone(),
        selected_changes,
        expected_live_revision: live.revision,
        options: Default::default(),
    };
    assert!(
        client
            .preview_migration(session, connection, preview(vec![second.id.clone()]))
            .await
            .is_err(),
        "cannot skip the first appended ordinal"
    );
    assert!(
        client
            .preview_migration(
                session,
                connection,
                preview(vec![table.id.clone(), first.id.clone()])
            )
            .await
            .is_err(),
        "whole-table proof requires every addition"
    );
    let prefix = client
        .preview_migration(session, connection, preview(vec![first.id.clone()]))
        .await
        .unwrap();
    assert_eq!(prefix.groups[0].statements.len(), 1);
    let plan = client
        .preview_migration(
            session,
            connection,
            preview(vec![table.id.clone(), first.id.clone(), second.id.clone()]),
        )
        .await
        .unwrap();
    assert_eq!(plan.groups[0].statements.len(), 2);
    assert!(plan.groups[0].statements[0].sql.contains("tail_one"));
    assert!(plan.groups[0].statements[1].sql.contains("tail_two"));
    assert!(plan.rollback_groups.is_empty());
    assert!(
        client
            .validate_migration(
                session,
                connection,
                ValidateMigrationRequest {
                    plan_id: plan.id,
                    plan_digest: plan.digest.clone(),
                    confirm_test_database: true
                }
            )
            .await
            .unwrap()
            .rolled_back
    );
    let run = client
        .apply_migration(
            session,
            connection,
            ApplyMigrationRequest {
                plan_id: plan.id,
                plan_digest: plan.digest,
                acknowledgements: plan.required_acknowledgements,
                source: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(run.state, MigrationRunState::Applied);
    assert!(client
        .execute(
            session,
            connection,
            "SELECT tail_one,tail_two FROM addition_data"
        )
        .await
        .unwrap()
        .rows
        .iter()
        .all(|r| r.values == vec![Value::Null, Value::Null]));
    assert_eq!(
        client
            .execute(session, connection, original)
            .await
            .unwrap()
            .rows
            .into_iter()
            .map(|r| r.values)
            .collect::<Vec<_>>(),
        expected
    );
}

async fn sqlite_drop_request(
    client: &sift_client_sdk::Client,
    session: SessionId,
    connection: ConnectionId,
    table_name: &str,
    name: &str,
) -> PreviewCatalogDiagramMutationRequest {
    let live = client
        .catalog_graph(
            session,
            connection,
            CatalogGraphRequest {
                refresh: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let table = live
        .data
        .nodes
        .iter()
        .find(|node| node.kind == CatalogNodeKind::Table && node.name == table_name)
        .unwrap();
    let column = live
        .data
        .nodes
        .iter()
        .find(|node| {
            node.kind == CatalogNodeKind::Column
                && node.parent_id.as_ref() == Some(&table.id)
                && node.name == name
        })
        .unwrap();
    PreviewCatalogDiagramMutationRequest {
        expected_catalog_revision: live.revision,
        mutation: CatalogDiagramMutation::DropColumn {
            column_id: column.id.clone(),
        },
        options: MigrationOptions {
            prefer_transactional: false,
            ..Default::default()
        },
    }
}
async fn sqlite_drop_column_acceptance(
    client: &sift_client_sdk::Client,
    session: SessionId,
    connection: ConnectionId,
    read_only: ConnectionId,
) {
    for sql in [r#"CREATE TABLE removal_data ("a""first" INTEGER,b TEXT,"c""middle" NUMERIC,d BLOB,"e""last" REAL)"#,
        "INSERT INTO removal_data(rowid,b,d) VALUES (91,'text',x'00FF'),(92,42,'mixed'),(93,NULL,NULL)"] {
        client.execute(session, connection, sql).await.unwrap();
    }
    let query = "SELECT rowid,typeof(b),b,typeof(d),d FROM removal_data ORDER BY rowid";
    let values = |response: ExecuteResponse| {
        response
            .rows
            .into_iter()
            .map(|row| row.values)
            .collect::<Vec<_>>()
    };
    let expected = values(client.execute(session, connection, query).await.unwrap());
    for (position, name) in ["a\"first", "c\"middle", "e\"last"].into_iter().enumerate() {
        if position == 0 {
            let request =
                sqlite_drop_request(client, session, read_only, "removal_data", name).await;
            let plan = client
                .preview_catalog_diagram_mutation(session, read_only, request)
                .await
                .unwrap();
            let validation = client
                .validate_migration(
                    session,
                    read_only,
                    ValidateMigrationRequest {
                        plan_id: plan.id,
                        plan_digest: plan.digest.clone(),
                        confirm_test_database: true,
                    },
                )
                .await
                .unwrap();
            assert!(!validation.valid);
            assert!(
                !matches!(client.apply_migration(session,read_only,ApplyMigrationRequest {plan_id:plan.id,plan_digest:plan.digest,acknowledgements:plan.required_acknowledgements,source:None}).await,Ok(run) if run.state==MigrationRunState::Applied)
            );
            let request =
                sqlite_drop_request(client, session, connection, "removal_data", name).await;
            let stale = client
                .preview_catalog_diagram_mutation(session, connection, request)
                .await
                .unwrap();
            client
                .execute(
                    session,
                    connection,
                    "CREATE INDEX removal_dependency ON removal_data(b)",
                )
                .await
                .unwrap();
            assert!(
                !matches!(client.apply_migration(session,connection,ApplyMigrationRequest {plan_id:stale.id,plan_digest:stale.digest,acknowledgements:stale.required_acknowledgements,source:None}).await,Ok(run) if run.state==MigrationRunState::Applied)
            );
            let request =
                sqlite_drop_request(client, session, connection, "removal_data", name).await;
            assert!(client
                .preview_catalog_diagram_mutation(session, connection, request)
                .await
                .is_err());
            for sql in [
                "DROP INDEX removal_dependency",
                "CREATE VIEW removal_dependency AS SELECT b FROM removal_data",
            ] {
                client.execute(session, connection, sql).await.unwrap();
            }
            let request =
                sqlite_drop_request(client, session, connection, "removal_data", name).await;
            assert!(client
                .preview_catalog_diagram_mutation(session, connection, request)
                .await
                .is_err());
            client
                .execute(session, connection, "DROP VIEW removal_dependency")
                .await
                .unwrap();
        }
        let request = sqlite_drop_request(client, session, connection, "removal_data", name).await;
        let plan = client
            .preview_catalog_diagram_mutation(session, connection, request)
            .await
            .unwrap();
        assert_eq!(plan.groups.len(), 1);
        assert!(plan.groups[0].transactional);
        assert_eq!(plan.groups[0].statements.len(), 1);
        assert_eq!(
            plan.groups[0].statements[0].sql,
            format!(
                "ALTER TABLE \"main\".\"removal_data\" DROP COLUMN \"{}\";",
                name.replace('"', "\"\"")
            )
        );
        assert_eq!(
            plan.groups[0].statements[0].risk,
            SchemaChangeRisk::DataLoss
        );
        assert_eq!(
            plan.required_acknowledgements,
            vec![SchemaChangeRisk::DataLoss]
        );
        assert!(plan.rollback_groups.is_empty());
        assert!(plan
            .warnings
            .iter()
            .any(|warning| warning.contains("permanently deleted")));
        assert_eq!(
            client
                .execute(session, connection, "SELECT * FROM removal_data")
                .await
                .unwrap()
                .columns
                .len(),
            5 - position,
            "preview is read-only"
        );
        let validation = client
            .validate_migration(
                session,
                connection,
                ValidateMigrationRequest {
                    plan_id: plan.id,
                    plan_digest: plan.digest.clone(),
                    confirm_test_database: true,
                },
            )
            .await
            .unwrap();
        assert!(validation.valid && validation.rolled_back);
        assert_eq!(
            client
                .execute(session, connection, "SELECT * FROM removal_data")
                .await
                .unwrap()
                .columns
                .len(),
            5 - position,
            "validation restores removed column"
        );
        assert!(client
            .apply_migration(
                session,
                connection,
                ApplyMigrationRequest {
                    plan_id: plan.id,
                    plan_digest: plan.digest.clone(),
                    acknowledgements: vec![],
                    source: None
                }
            )
            .await
            .is_err());
        let apply = ApplyMigrationRequest {
            plan_id: plan.id,
            plan_digest: plan.digest,
            acknowledgements: plan.required_acknowledgements,
            source: None,
        };
        assert_eq!(
            client
                .apply_migration(session, connection, apply.clone())
                .await
                .unwrap()
                .state,
            MigrationRunState::Applied
        );
        assert!(client
            .apply_migration(session, connection, apply)
            .await
            .is_err());
        assert_eq!(
            client
                .execute(session, connection, "SELECT * FROM removal_data")
                .await
                .unwrap()
                .columns
                .len(),
            4 - position
        );
        assert_eq!(
            values(client.execute(session, connection, query).await.unwrap()),
            expected,
            "rowids, values and native storage classes survive"
        );
    }
    client
        .execute(session, connection, "CREATE TABLE last_column (only TEXT)")
        .await
        .unwrap();
    let request = sqlite_drop_request(client, session, connection, "last_column", "only").await;
    assert!(client
        .preview_catalog_diagram_mutation(session, connection, request)
        .await
        .is_err());
    client
        .execute(session, connection, "DROP TABLE last_column")
        .await
        .unwrap();
    sqlite_drop_selection_acceptance(client, session, connection).await;
}

async fn sqlite_drop_selection_acceptance(
    client: &sift_client_sdk::Client,
    session: SessionId,
    connection: ConnectionId,
) {
    for sql in [
        "CREATE TABLE removal_selection (a INTEGER,b TEXT,c NUMERIC,d BLOB)",
        "INSERT INTO removal_selection(rowid,a,b,c,d) VALUES (77,11,'keep',12,x'01FF')",
        "ALTER TABLE removal_selection DROP COLUMN a",
        "ALTER TABLE removal_selection DROP COLUMN c",
    ] {
        client.execute(session, connection, sql).await.unwrap();
    }
    let mut targets = Vec::new();
    for mixed in [false, true] {
        if mixed {
            for sql in [
                "DROP TABLE removal_selection",
                "CREATE TABLE removal_selection (b REAL,d BLOB)",
            ] {
                client.execute(session, connection, sql).await.unwrap();
            }
        }
        let live = client
            .catalog_graph(
                session,
                connection,
                CatalogGraphRequest {
                    refresh: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        targets.push(
            client
                .create_catalog_snapshot(
                    session,
                    connection,
                    CreateCatalogSnapshotRequest {
                        expected_catalog_revision: live.revision,
                        options: Default::default(),
                        description: None,
                        accept_partial: true,
                    },
                )
                .await
                .unwrap(),
        );
    }
    for sql in [
        "DROP TABLE removal_selection",
        "CREATE TABLE removal_selection (a INTEGER,b TEXT,c NUMERIC,d BLOB)",
        "INSERT INTO removal_selection(rowid,a,b,c,d) VALUES (77,11,'keep',12,x'01FF')",
    ] {
        client.execute(session, connection, sql).await.unwrap();
    }
    let live = client
        .catalog_graph(
            session,
            connection,
            CatalogGraphRequest {
                refresh: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let request = |snapshot_id, options| SchemaDiffRequest {
        from: CatalogSourceRef::Live {
            expected_revision: live.revision,
            options,
        },
        to: CatalogSourceRef::Snapshot { snapshot_id },
        accepted_renames: vec![],
        max_changes: None,
    };
    for (mixed, target) in targets.iter().enumerate().rev() {
        let diff_request = request(target.id, Default::default());
        let diff = client
            .compare_catalog_schemas(session, connection, diff_request.clone())
            .await
            .unwrap();
        let drops = diff
            .changes
            .iter()
            .filter(|change| {
                change.kind == SchemaChangeKind::Drop
                    && change
                        .object_before
                        .as_ref()
                        .is_some_and(|node| node.kind == CatalogNodeKind::Column)
            })
            .collect::<Vec<_>>();
        assert_eq!(drops.len(), 2);
        let preview = |selected_changes| PreviewMigrationRequest {
            diff: diff_request.clone(),
            expected_diff_digest: diff.digest.clone(),
            selected_changes,
            expected_live_revision: live.revision,
            options: Default::default(),
        };
        if mixed == 1 {
            assert!(
                client
                    .preview_migration(
                        session,
                        connection,
                        preview(drops.iter().map(|change| change.id.clone()).collect())
                    )
                    .await
                    .is_err(),
                "a drop cannot absorb surviving type changes"
            );
            continue;
        }
        let first = drops
            .iter()
            .find(|change| change.object_before.as_ref().unwrap().name == "a")
            .unwrap();
        let companion = diff
            .changes
            .iter()
            .find(|change| {
                change.kind == SchemaChangeKind::Alter
                    && change.object_after.as_ref().is_some_and(|node| {
                        node.kind == CatalogNodeKind::Column && node.name == "b"
                    })
            })
            .unwrap();
        assert!(
            client
                .preview_migration(
                    session,
                    connection,
                    preview(vec![first.id.clone(), companion.id.clone()])
                )
                .await
                .is_err(),
            "selected ordinal companion requires every removal"
        );
        let subset = client
            .preview_migration(session, connection, preview(vec![first.id.clone()]))
            .await
            .unwrap();
        assert_eq!(subset.groups[0].statements.len(), 1);
        let validation = client
            .validate_migration(
                session,
                connection,
                ValidateMigrationRequest {
                    plan_id: subset.id,
                    plan_digest: subset.digest,
                    confirm_test_database: true,
                },
            )
            .await
            .unwrap();
        assert!(validation.valid && validation.rolled_back);
        let plan = client
            .preview_migration(session, connection, preview(vec![]))
            .await
            .unwrap();
        assert_eq!(plan.groups[0].statements.len(), 2);
        assert!(plan.groups[0].statements[0]
            .sql
            .contains("DROP COLUMN \"a\""));
        assert!(plan.groups[0].statements[1]
            .sql
            .contains("DROP COLUMN \"c\""));
        assert_eq!(
            client
                .apply_migration(
                    session,
                    connection,
                    ApplyMigrationRequest {
                        plan_id: plan.id,
                        plan_digest: plan.digest,
                        acknowledgements: plan.required_acknowledgements,
                        source: None
                    }
                )
                .await
                .unwrap()
                .state,
            MigrationRunState::Applied
        );
        assert_eq!(
            client
                .execute(
                    session,
                    connection,
                    "SELECT rowid,b,d FROM removal_selection"
                )
                .await
                .unwrap()
                .rows[0]
                .values,
            vec![
                Value::Int64(77),
                Value::Text("keep".into()),
                Value::Blob(vec![1, 255])
            ]
        );
    }
}
