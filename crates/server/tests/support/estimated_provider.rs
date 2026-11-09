use super::*;
pub(super) struct Fixture {
    pub(super) _temp: tempfile::TempDir,
    pub(super) process: Arc<SupervisedProcess>,
    pub(super) store: SessionStore,
    pub(super) session: SessionId,
    pub(super) connection: ConnectionId,
}
pub(super) async fn fixture(mode: &str, dialect: &str, version: u32) -> Fixture {
    let executable = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(format!(
            "sift-conformance-provider{}",
            std::env::consts::EXE_SUFFIX
        ));
    assert!(
        executable.is_file(),
        "Build sift-plugin-host --bin sift-conformance-provider first"
    );
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join(".sift-conformance-mode"), mode).unwrap();
    let contribution = ContributionId::new("acme/conformance/database_provider/fixture").unwrap();
    let process = Arc::new(
        SupervisedProcess::start(
            ProcessSpec {
                executable,
                working_directory: temp.path().into(),
                extension_id: ExtensionId::new("acme/conformance").unwrap(),
                extension_version: "1.0.0".into(),
                manifest_sha256: "a".repeat(64),
                expected_contributions: vec![contribution.clone()],
                driver_rpc: Some(VersionRange {
                    minimum: 1,
                    maximum: version,
                }),
                generation: WireId::from_u128(uuid::Uuid::new_v4().as_u128()),
                granted_capabilities: vec![],
            },
            SupervisorLimits {
                heartbeat_interval: Duration::from_secs(60),
                ..Default::default()
            },
        )
        .await
        .unwrap(),
    );
    let descriptor = ProviderDescriptor {
        provider: ProviderRef {
            provider_id: ProviderId::new("acme/conformance").unwrap(),
            dialect_id: DialectId::new(dialect).unwrap(),
            provider_version: "1.0.0".into(),
        },
        display_name: "Estimated fixture".into(),
        configuration_schema: serde_json::json!({"type":"object"}),
        credential_schema: serde_json::json!({"type":"object"}),
        configuration_schema_version: 1,
        capabilities: [
            "driver.core@1",
            "driver.transactions@1",
            "driver.savepoints@1",
            "driver.explain.estimated@1",
        ]
        .into_iter()
        .chain((version >= 4).then_some("driver.notifications@1"))
        .map(|id| ProviderCapability {
            id: id.into(),
            limits: Default::default(),
        })
        .collect(),
        quality: None,
        available: true,
    };
    let provider = Arc::new(RpcProvider::new(descriptor, contribution, process.clone()).unwrap());
    let registry = DriverRegistry::new();
    registry
        .providers()
        .replace([provider as Arc<dyn DatabaseProvider>])
        .unwrap();
    let store = SessionStore::new(registry);
    let session = store
        .open_session(OpenSessionRequest {
            tag: None,
            tenant_id: None,
        })
        .id;
    let connection = store
        .open_provider_connection(
            session,
            ProviderId::new("acme/conformance").unwrap(),
            ConnectionSpec {
                host: "fixture".into(),
                port: None,
                database: None,
                user: "fixture".into(),
                password: None,
                ssl_mode: None,
                engine_specific: None,
            },
        )
        .await
        .unwrap()
        .id;
    Fixture {
        _temp: temp,
        process,
        store,
        session,
        connection,
    }
}
pub(super) fn request(connection: ConnectionId, sql: &str) -> ExplainRequest {
    ExplainRequest {
        connection,
        sql: sql.into(),
        params: vec![sift_protocol::Value::Int64(7)],
        analyze: false,
    }
}
