//! Executable Driver RPC v4 notification ownership and failure boundaries.
use sift_extension_protocol::{ContributionId, ExtensionId, VersionRange, WireId};
use sift_plugin_host::{ProcessSpec, SupervisedProcess, SupervisorLimits};
use sift_protocol::{
    ConnectionId, ConnectionSpec, DialectId, ExplainRequest, OpenSessionRequest,
    ProviderCapability, ProviderDescriptor, ProviderId, ProviderRef, SessionId,
};
use sift_server::{
    registry::{DatabaseProvider, DriverRegistry},
    RpcProvider, SessionStore,
};
use std::{sync::Arc, time::Duration};
#[path = "support/estimated_provider.rs"]
mod provider_fixture;
use provider_fixture::*;

async fn server(
    store: &SessionStore,
) -> (
    sift_client_sdk::Client,
    tokio::task::JoinHandle<()>,
    sift_server::Shutdown,
) {
    let shutdown = sift_server::Shutdown::default();
    let router = sift_server::http::app(sift_server::http::AppState {
        sessions: store.clone(),
        rooms: Default::default(),
        shutdown: shutdown.clone(),
        auth: Default::default(),
        metadata: None,
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = sift_client_sdk::Client::new(format!("http://{}", listener.local_addr().unwrap()));
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (client, task, shutdown)
}
async fn wait(mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("notification fixture settled");
}
async fn next(stream: &mut sift_client_sdk::NotificationStream) -> (String, String) {
    tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .unwrap()
        .unwrap()
}
fn active(f: &Fixture) -> usize {
    std::fs::read_to_string(f._temp.path().join(".sift-listen-active"))
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}
#[tokio::test]
#[ignore = "requires built conformance executable"]
async fn v4_notifications_use_unknown_dialects_independent_subscriptions_and_existing_controls() {
    let f = fixture("v4_ack_first", "acme/opaque", 4).await;
    assert!(f
        .store
        .conn_entry(f.session, f.connection)
        .unwrap()
        .driver
        .supports_operation(sift_protocol::OperationKind::Listen));
    let (client, task, shutdown) = server(&f.store).await;
    let mut first = client
        .subscribe_notifications(
            f.session,
            f.connection,
            vec!["changes".into(), "通知".into()],
        )
        .await
        .unwrap();
    assert_eq!(next(&mut first).await, ("changes".into(), "one".into()));
    let mut second = client
        .subscribe_notifications(f.session, f.connection, vec!["other".into()])
        .await
        .unwrap();
    assert_eq!(next(&mut second).await, ("other".into(), "one".into()));
    wait(|| active(&f) == 2).await;
    let tx = f
        .store
        .begin_transaction(
            f.session,
            sift_protocol::BeginTransactionRequest {
                connection: f.connection,
                mode: Default::default(),
            },
        )
        .await
        .unwrap();
    f.store
        .create_savepoint(
            f.session,
            sift_protocol::SavepointRequest {
                connection: f.connection,
                tx_id: tx.tx_id,
                name: "v4".into(),
            },
        )
        .await
        .unwrap();
    f.store
        .rollback_transaction(
            f.session,
            sift_protocol::EndTransactionRequest {
                connection: f.connection,
                tx_id: tx.tx_id,
            },
        )
        .await
        .unwrap();
    let query = serde_json::from_value(
        serde_json::json!({"connection": f.connection,"sql":"SELECT 1","params":[]}),
    )
    .unwrap();
    assert!(!f
        .store
        .execute_http(f.session, query)
        .await
        .unwrap()
        .rows
        .is_empty());
    drop(first);
    wait(|| active(&f) == 1).await;
    assert_eq!(next(&mut second).await, ("other".into(), "通知🦀".into()));
    drop(second);
    assert_eq!(shutdown.await_drain(Duration::from_secs(8)).await, 0);
    wait(|| active(&f) == 0).await;
    let audit = f.store.list_operations();
    let listens = audit
        .iter()
        .filter(|r| matches!(r.operation, sift_protocol::Operation::Listen { .. }))
        .collect::<Vec<_>>();
    assert_eq!(listens.len(), 2);
    assert!(listens
        .iter()
        .all(|r| r.status == sift_protocol::OperationStatus::Succeeded));
    assert_eq!(
        f.process.health().await,
        sift_plugin_host::GenerationHealth::Ready
    );
    f.process.shutdown("done").await.unwrap();
    task.abort();
    let f = fixture("v4", "sift/postgresql", 4).await;
    let response = sift_server::plan::explain(
        &f.store,
        f.session,
        f.connection,
        &request(f.connection, "SELECT $1"),
    )
    .await
    .unwrap();
    assert!(!response.analyzed);
    f.process.shutdown("done").await.unwrap();
}
#[tokio::test]
#[ignore = "requires built conformance executable"]
async fn notification_faults_fence_generations_and_legacy_or_invalid_input_does_not_dispatch() {
    let healthy = fixture("v4_quiet", "acme/opaque", 4).await;
    for channels in [
        vec![],
        vec!["x".into(), "x".into()],
        vec!["bad\nname".into()],
        vec!["🦀".repeat(16)],
    ] {
        assert!(healthy
            .store
            .listen_pg(healthy.session, healthy.connection, channels)
            .await
            .is_err());
    }
    assert!(!healthy._temp.path().join(".sift-listen-active").exists());
    for mode in [
        "v4_bad_ack",
        "v4_wrong_handle",
        "v4_wrong_subscription",
        "v4_wrong_channel",
        "v4_malformed",
        "v4_oversized",
        "v4_bad_sequence",
    ] {
        let f = fixture(mode, "acme/opaque", 4).await;
        match f
            .store
            .listen_pg(f.session, f.connection, vec!["changes".into()])
            .await
        {
            Err(_) => {}
            Ok(mut stream) => {
                assert!(
                    tokio::time::timeout(Duration::from_secs(5), stream.notifications.recv())
                        .await
                        .unwrap()
                        .map_or(true, |r| r.is_err())
                );
                assert!(stream.settled.await.unwrap().is_err());
            }
        }
        assert_ne!(
            f.process.health().await,
            sift_plugin_host::GenerationHealth::Ready
        );
    }
    let ended = fixture("v4_unexpected_stop", "acme/opaque", 4).await;
    let mut stream = ended
        .store
        .listen_pg(ended.session, ended.connection, vec!["changes".into()])
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(8), &mut stream.settled)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert_eq!(
        ended.process.health().await,
        sift_plugin_host::GenerationHealth::Ready
    );
    ended.process.shutdown("done").await.unwrap();
    for mode in ["v4_bad_stop", "v4_missing_barrier"] {
        let f = fixture(mode, "acme/opaque", 4).await;
        let mut stream = f
            .store
            .listen_pg(f.session, f.connection, vec!["changes".into()])
            .await
            .unwrap();
        stream.notifications.recv().await.unwrap().unwrap();
        drop(stream.notifications);
        assert!(tokio::time::timeout(Duration::from_secs(8), stream.settled)
            .await
            .unwrap()
            .unwrap()
            .is_err());
        assert_ne!(
            f.process.health().await,
            sift_plugin_host::GenerationHealth::Ready
        );
    }
    for (mode, version) in [("normal", 1), ("v2", 2), ("v3", 3)] {
        let f = fixture(mode, "acme/opaque", version).await;
        assert!(f
            .store
            .listen_pg(f.session, f.connection, vec!["changes".into()])
            .await
            .is_err());
        assert_eq!(
            f.process.health().await,
            sift_plugin_host::GenerationHealth::Ready
        );
        f.process.shutdown("done").await.unwrap();
    }
    let mut stream = healthy
        .store
        .listen_pg(healthy.session, healthy.connection, vec!["changes".into()])
        .await
        .unwrap();
    stream.notifications.recv().await.unwrap().unwrap();
    drop(stream.notifications);
    stream.settled.await.unwrap().unwrap();
    assert_eq!(
        healthy.process.health().await,
        sift_plugin_host::GenerationHealth::Ready
    );
    healthy.process.shutdown("done").await.unwrap();
}
async fn managed(f: &mut Fixture) -> (sift_metadata::MetadataStore, sift_metadata::TenantId) {
    let metadata = sift_metadata::MetadataStore::open_in_memory(Arc::new(
        sift_metadata::MemorySecretStore::default(),
    ))
    .unwrap();
    metadata.bootstrap_local("notification acceptance").unwrap();
    let actor = sift_metadata::PrincipalId(1);
    let tenant = sift_metadata::TenantId(1);
    let profile = metadata
        .upsert_connection_profile(
            tenant,
            actor,
            sift_metadata::NewConnectionProfile {
                name: "Listener".into(),
                provider_id: ProviderId::new("acme/conformance").unwrap(),
                configuration: serde_json::json!({"host":"fixture","user":"fixture"}),
                semantic_engine: None,
                credentials: None,
                credential_mode: sift_metadata::CredentialMode::Shared,
                tags: vec![],
            },
        )
        .await
        .unwrap();
    f.store.set_authorization_store(metadata.clone());
    f.store.set_audit_store(metadata.clone());
    f.session = f
        .store
        .open_session_with_owner(
            OpenSessionRequest {
                tag: None,
                tenant_id: None,
            },
            Some(actor),
            Some(tenant),
            false,
        )
        .unwrap()
        .id;
    f.connection = f
        .store
        .open_managed_connection(
            f.session,
            profile.provider_id,
            profile.configuration,
            Default::default(),
            actor,
            tenant,
            profile.id,
            profile.policy.revision,
            false,
        )
        .await
        .unwrap()
        .id;
    (metadata, tenant)
}
#[tokio::test]
#[ignore = "requires built conformance executable"]
async fn quiet_socket_drop_retains_quota_shutdown_and_one_audit_until_stop_receipts() {
    for drain in [false, true] {
        let mut f = fixture("v4_stop_hold", "acme/opaque", 4).await;
        let (metadata, tenant) = managed(&mut f).await;
        let (client, task, shutdown) = server(&f.store).await;
        let mut stream = client
            .subscribe_notifications(f.session, f.connection, vec!["changes".into()])
            .await
            .unwrap();
        assert_eq!(next(&mut stream).await.1, "one");
        if drain {
            shutdown.begin_drain();
        } else {
            drop(stream);
        }
        wait(|| f._temp.path().join(".sift-unlisten-entered").exists()).await;
        assert_eq!(shutdown.in_flight(), 1);
        let usage = f.store.resource_manager().snapshot(tenant).unwrap().usage;
        assert_eq!(usage.concurrent_queries, 1);
        assert_eq!(usage.cursors, 1);
        assert_eq!(usage.retained_result_bytes, 4 * 1024 * 1024);
        assert!(!f
            .store
            .list_operations()
            .iter()
            .any(|r| matches!(r.operation, sift_protocol::Operation::Listen { .. })));
        std::fs::write(f._temp.path().join(".sift-unlisten-release"), b"release").unwrap();
        assert_eq!(shutdown.await_drain(Duration::from_secs(8)).await, 0);
        let usage = f.store.resource_manager().snapshot(tenant).unwrap().usage;
        assert_eq!(
            (
                usage.concurrent_queries,
                usage.cursors,
                usage.retained_result_bytes
            ),
            (0, 0, 0)
        );
        let rows = f
            .store
            .list_operations()
            .into_iter()
            .filter(|r| matches!(r.operation, sift_protocol::Operation::Listen { .. }))
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].status,
            if drain {
                sift_protocol::OperationStatus::Failed
            } else {
                sift_protocol::OperationStatus::Succeeded
            }
        );
        let durable = metadata
            .list_operation_audit(100)
            .unwrap()
            .into_iter()
            .filter(|r| r.action == "listen")
            .collect::<Vec<_>>();
        assert_eq!(durable.len(), 1);
        assert_eq!(
            durable[0].actor_principal_id,
            Some(sift_metadata::PrincipalId(1))
        );
        assert!(durable[0].correlation_id.is_some());
        assert!(!format!("{durable:?}").contains("changes"));
        assert!(!format!("{rows:?}").contains("changes"));
        assert_eq!(
            f.process.health().await,
            sift_plugin_host::GenerationHealth::Ready
        );
        task.abort();
        f.process.shutdown("done").await.unwrap();
    }
}

#[tokio::test]
#[ignore = "requires built conformance executable"]
async fn subscription_cleanup_precedes_base_close_and_handles_backpressure() {
    for close_session in [false, true] {
        let f = fixture("v4_flood", "acme/opaque", 4).await;
        let mut stream = f
            .store
            .listen_pg(f.session, f.connection, vec!["changes".into()])
            .await
            .unwrap();
        stream.notifications.recv().await.unwrap().unwrap();
        if close_session {
            f.store.close_session(f.session).unwrap();
        } else {
            f.store
                .close_connection(f.session, f.connection)
                .await
                .unwrap();
        }
        wait(|| f._temp.path().join(".sift-native-close").exists()).await;
        assert!(f._temp.path().join(".sift-unlisten-stopped").exists());
        drop(stream.notifications);
        let _ = stream.settled.await.unwrap();
        assert_eq!(
            f.process.health().await,
            sift_plugin_host::GenerationHealth::Ready
        );
        f.process.shutdown("done").await.unwrap();
    }
}
#[tokio::test]
#[ignore = "requires built conformance executable"]
async fn notification_schema_scope_is_refused_and_live_policy_revocation_stops_delivery() {
    let mut f = fixture("v4_flood", "sift/postgresql", 4).await;
    let (metadata, tenant) = managed(&mut f).await;
    let profile = metadata.list_connection_profiles(tenant).unwrap().remove(0);
    let update = |revision, restricted: bool| {
        metadata
            .update_connection_policy(
                tenant,
                sift_metadata::PrincipalId(1),
                profile.id,
                sift_protocol::UpdateConnectionPolicyRequest {
                    expected_revision: Some(revision),
                    minimum_tenant_role: sift_protocol::TenantRole::Member,
                    read_only: false,
                    allowed_ops: None,
                    blocked_ops: vec![],
                    allowed_schemas: restricted.then(|| {
                        vec![sift_protocol::SchemaSelector {
                            catalog: None,
                            schema: "public".into(),
                        }]
                    }),
                },
                sift_metadata::NewOperationAudit {
                    actor_principal_id: Some(sift_metadata::PrincipalId(1)),
                    action: "update_policy".into(),
                    target: "connection_profile".into(),
                    target_id: Some(profile.id.0),
                    status: "succeeded".into(),
                    result_code: None,
                    row_count: None,
                    error_message: None,
                    correlation_id: None,
                },
            )
            .unwrap();
    };
    update(profile.policy.revision, true);
    assert!(f
        .store
        .listen_pg(f.session, f.connection, vec!["changes".into()])
        .await
        .is_err());
    assert!(!f._temp.path().join(".sift-listen-active").exists());
    let capabilities = sift_server::capability::evaluate(
        &f.store,
        &sift_protocol::OperationCapabilityContext {
            session: Some(f.session),
            connection: Some(f.connection),
            ..Default::default()
        },
        None,
    )
    .unwrap();
    assert!(
        !capabilities
            .iter()
            .find(|cap| cap.operation == sift_protocol::OperationKind::Listen)
            .unwrap()
            .available
    );
    update(profile.policy.revision + 1, false);
    let (client, task, shutdown) = server(&f.store).await;
    let mut stream = client
        .subscribe_notifications(f.session, f.connection, vec!["changes".into()])
        .await
        .unwrap();
    next(&mut stream).await;
    update(profile.policy.revision + 2, true);
    assert_eq!(shutdown.await_drain(Duration::from_secs(8)).await, 0);
    assert!(f._temp.path().join(".sift-unlisten-stopped").exists());
    let rows = metadata
        .list_operation_audit(100)
        .unwrap()
        .into_iter()
        .filter(|row| row.action == "listen")
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, "failed");
    assert_eq!(
        f.process.health().await,
        sift_plugin_host::GenerationHealth::Ready
    );
    drop(stream);
    task.abort();
    f.process.shutdown("done").await.unwrap();
}
#[cfg(all(unix, feature = "live-pg"))]
#[path = "common/postgres.rs"]
mod postgres_fixture;
#[cfg(all(unix, feature = "live-pg"))]
#[tokio::test]
async fn native_postgres_quiet_socket_drop_releases_dedicated_listener() {
    let cluster = postgres_fixture::Cluster::start();
    let store = SessionStore::new(
        DriverRegistry::builder()
            .register(sift_driver_postgres::PgDriver::new())
            .build(),
    );
    let session = store
        .open_session(OpenSessionRequest {
            tag: None,
            tenant_id: None,
        })
        .id;
    let connection = store
        .open_provider_connection(
            session,
            sift_protocol::Engine::Postgres.provider_id(),
            ConnectionSpec {
                host: cluster.0.path().to_str().unwrap().into(),
                port: None,
                database: Some("postgres".into()),
                user: "sift".into(),
                password: None,
                ssl_mode: Some(sift_protocol::SslMode::Disable),
                engine_specific: None,
            },
        )
        .await
        .unwrap()
        .id;
    let (client, task, shutdown) = server(&store).await;
    for _ in 0..2 {
        let mut stream = client
            .subscribe_notifications(session, connection, vec!["native_changes".into()])
            .await
            .unwrap();
        wait(|| {
            cluster
                .sql("SELECT count(*) FROM pg_stat_activity WHERE application_name = 'sift-listen'")
                == "1"
        })
        .await;
        cluster.sql("NOTIFY native_changes, 'one'");
        assert_eq!(next(&mut stream).await.1, "one");
        drop(stream);
        wait(|| {
            cluster
                .sql("SELECT count(*) FROM pg_stat_activity WHERE application_name = 'sift-listen'")
                == "0"
        })
        .await;
        assert_eq!(shutdown.await_drain(Duration::from_secs(5)).await, 0);
        assert_eq!(
            store.ping(session, connection).await.unwrap().current_user,
            "sift"
        );
    }
    store.close_connection(session, connection).await.unwrap();
    task.abort();
}
