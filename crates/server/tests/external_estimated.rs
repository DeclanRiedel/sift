//! Executable Driver RPC v3 acceptance through the real HTTP/SDK path.
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
#[tokio::test]
#[ignore = "requires built conformance executable"]
async fn external_plan_v3_http_formats_and_savepoints() {
    for (mode, dialect, sql) in [
        ("v3", "sift/postgresql", "SELECT $1"),
        ("v3_mssql", "sift/tsql", "SELECT @P1"),
        ("v3_sqlite", "sift/sqlite", "SELECT ?1"),
    ] {
        let f = fixture(mode, dialect, 3).await;
        assert_eq!(f.process.method_family_version("driver"), Some(3));
        let entry = f.store.conn_entry(f.session, f.connection).unwrap();
        assert!(entry
            .driver
            .supports_operation(sift_protocol::OperationKind::Explain));
        assert!(!entry
            .driver
            .supports_operation(sift_protocol::OperationKind::ProfileQuery));
        for param in [
            sift_protocol::Value::Float64(f64::NAN),
            sift_protocol::Value::Interval(chrono::Duration::MAX),
            sift_protocol::Value::Native {
                provider_id: ProviderId::new("other/provider").unwrap(),
                type_name: "foreign".into(),
                display_text: "unbound".into(),
            },
        ] {
            let mut bad = request(f.connection, sql);
            bad.params = vec![param];
            assert!(
                sift_server::plan::explain(&f.store, f.session, f.connection, &bad)
                    .await
                    .is_err()
            );
        }
        let router = sift_server::http::app(sift_server::http::AppState {
            sessions: f.store.clone(),
            rooms: Default::default(),
            shutdown: Default::default(),
            auth: Default::default(),
            metadata: None,
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client =
            sift_client_sdk::Client::new(format!("http://{}", listener.local_addr().unwrap()));
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let response = client
            .explain(f.session, f.connection, request(f.connection, sql))
            .await
            .unwrap();
        assert!(!response.analyzed);
        assert!(response
            .warnings
            .iter()
            .any(|warning| warning.message.contains("not host attestations")));
        let mut analyzed = request(f.connection, sql);
        analyzed.analyze = true;
        assert!(
            sift_server::plan::explain(&f.store, f.session, f.connection, &analyzed)
                .await
                .is_err()
        );
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
        assert!(sift_server::plan::explain(
            &f.store,
            f.session,
            f.connection,
            &request(f.connection, sql)
        )
        .await
        .is_err());
        f.store
            .create_savepoint(
                f.session,
                sift_protocol::SavepointRequest {
                    connection: f.connection,
                    tx_id: tx.tx_id,
                    name: "v3".into(),
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
        f.process.shutdown("done").await.unwrap();
        server.abort();
    }
}
#[tokio::test]
#[ignore = "requires built conformance executable"]
async fn external_plan_rejects_bad_frames_unknown_dialects_and_legacy_versions() {
    let healthy = fixture("v3", "sift/postgresql", 3).await;
    for mode in ["v3_wrong_handle", "v3_unknown", "v3_actual", "v3_bad_shape"] {
        let f = fixture(mode, "sift/postgresql", 3).await;
        assert!(sift_server::plan::explain(
            &f.store,
            f.session,
            f.connection,
            &request(f.connection, "SELECT $1")
        )
        .await
        .is_err());
        assert_eq!(
            f.process.health().await,
            sift_plugin_host::GenerationHealth::Stopped
        );
    }
    assert!(sift_server::plan::explain(
        &healthy.store,
        healthy.session,
        healthy.connection,
        &request(healthy.connection, "SELECT $1")
    )
    .await
    .is_ok());
    healthy.process.shutdown("done").await.unwrap();
    for (mode, version) in [("normal", 1), ("v2", 2)] {
        let f = fixture(mode, "sift/postgresql", version).await;
        assert!(sift_server::plan::explain(
            &f.store,
            f.session,
            f.connection,
            &request(f.connection, "SELECT $1")
        )
        .await
        .is_err());
        f.process.shutdown("done").await.unwrap();
    }
    let f = fixture("v3", "acme/custom", 3).await;
    assert!(!f
        .store
        .conn_entry(f.session, f.connection)
        .unwrap()
        .driver
        .supports_operation(sift_protocol::OperationKind::Explain));
    assert!(sift_server::plan::explain(
        &f.store,
        f.session,
        f.connection,
        &request(f.connection, "SELECT $1")
    )
    .await
    .is_err());
    f.process.shutdown("done").await.unwrap();
}
