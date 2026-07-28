use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    net::IpAddr,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use sift_extension_protocol::{
    ConnectionHookRequest, ConnectionHookResponse, ConnectionHookStage, DatabaseEndpoint,
    OpenTunnelRequest, OpenTunnelResponse, ResolveCredentialsRequest, ResolveCredentialsResponse,
    SanitizedServerInfo,
};
use sift_protocol::DriverError;

use crate::{
    registry::{ProviderConnectionHandle, ProviderOpenRequest},
    DatabaseProvider, ProviderServerInfo,
};

#[async_trait::async_trait]
pub trait CredentialBroker: Send + Sync {
    async fn resolve(
        &self,
        request: ResolveCredentialsRequest,
    ) -> Result<ResolveCredentialsResponse, PipelineContributionError>;
}

#[async_trait::async_trait]
pub trait TunnelProvider: Send + Sync {
    async fn open_tunnel(
        &self,
        request: OpenTunnelRequest,
    ) -> Result<OpenTunnelResponse, PipelineContributionError>;
    async fn close_tunnel(
        &self,
        lease: sift_extension_protocol::WireId,
    ) -> Result<(), PipelineContributionError>;
}

#[async_trait::async_trait]
pub trait ConnectionHook: Send + Sync {
    async fn run(
        &self,
        request: ConnectionHookRequest,
    ) -> Result<ConnectionHookResponse, PipelineContributionError>;
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("{code}")]
pub struct PipelineContributionError {
    pub code: String,
}

pub struct BrokerSelection {
    pub client: Arc<dyn CredentialBroker>,
    pub configuration: serde_json::Value,
    pub required_fields: Vec<String>,
    pub secret_handles: BTreeMap<String, String>,
}

pub struct TunnelSelection {
    pub client: Arc<dyn TunnelProvider>,
    pub configuration: serde_json::Value,
    pub credentials: Vec<sift_extension_protocol::CredentialField>,
}

#[derive(Clone)]
pub struct HookSelection {
    pub extension_id: sift_extension_protocol::ExtensionId,
    pub contribution_id: sift_extension_protocol::ContributionId,
    pub priority: i32,
    pub required: bool,
    pub client: Arc<dyn ConnectionHook>,
}

pub struct ConnectionPipelineRequest {
    pub provider: Arc<dyn DatabaseProvider>,
    pub configuration: serde_json::Value,
    pub logical_endpoint: DatabaseEndpoint,
    pub credentials: HashMap<String, Vec<u8>>,
    pub broker: Option<BrokerSelection>,
    pub tunnel: Option<TunnelSelection>,
    pub hooks: Vec<HookSelection>,
    pub patchable_fields: BTreeSet<String>,
    pub tenant_id: Option<i64>,
    pub profile_id: Option<i64>,
}

pub struct PipelineConnection {
    pub provider: Arc<dyn DatabaseProvider>,
    pub handle: ProviderConnectionHandle,
    pub server: ProviderServerInfo,
    pub warnings: Vec<String>,
    configuration: serde_json::Value,
    logical_endpoint: DatabaseEndpoint,
    tunnel: Option<(Arc<dyn TunnelProvider>, sift_extension_protocol::WireId)>,
    hooks: Vec<HookSelection>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConnectionPipelineError {
    #[error("provider configuration must be a JSON object")]
    InvalidConfiguration,
    #[error("credential broker returned invalid credential fields")]
    InvalidCredentials,
    #[error("tunnel returned an invalid endpoint")]
    InvalidTunnelEndpoint,
    #[error("required {stage:?} connection hook failed")]
    RequiredHookFailed { stage: ConnectionHookStage },
    #[error("connection hook returned a forbidden configuration patch")]
    ForbiddenPatch,
    #[error("provider operation failed: {0}")]
    Provider(#[from] DriverError),
    #[error("tunnel operation failed")]
    Tunnel,
}

pub async fn open_connection_pipeline(
    mut request: ConnectionPipelineRequest,
) -> Result<PipelineConnection, ConnectionPipelineError> {
    if !request.configuration.is_object() {
        return Err(ConnectionPipelineError::InvalidConfiguration);
    }
    sort_hooks(&mut request.hooks);
    let mut warnings = Vec::new();
    run_hooks(
        &request.hooks,
        ConnectionHookStage::PreResolve,
        &mut request.configuration,
        &request.logical_endpoint,
        None,
        None,
        &request.patchable_fields,
        &mut warnings,
    )
    .await?;

    if let Some(broker) = request.broker {
        let response = broker
            .client
            .resolve(ResolveCredentialsRequest {
                configuration: broker.configuration,
                required_fields: broker.required_fields.clone(),
                secret_handles: broker.secret_handles,
                tenant_id: request.tenant_id,
                profile_id: request.profile_id,
            })
            .await
            .map_err(|_| ConnectionPipelineError::InvalidCredentials)?;
        if response
            .expires_at_unix_ms
            .is_some_and(|expiry| expiry <= now_unix_ms())
        {
            return Err(ConnectionPipelineError::InvalidCredentials);
        }
        request.credentials = validate_credentials(response, &broker.required_fields)
            .map_err(|_| ConnectionPipelineError::InvalidCredentials)?;
    }
    run_hooks(
        &request.hooks,
        ConnectionHookStage::PostResolve,
        &mut request.configuration,
        &request.logical_endpoint,
        None,
        None,
        &request.patchable_fields,
        &mut warnings,
    )
    .await?;

    let tunnel = if let Some(tunnel) = request.tunnel {
        let response = tunnel
            .client
            .open_tunnel(OpenTunnelRequest {
                endpoint: request.logical_endpoint.clone(),
                configuration: tunnel.configuration,
                credentials: tunnel.credentials,
            })
            .await
            .map_err(|_| ConnectionPipelineError::Tunnel)?;
        validate_loopback_endpoint(&response.endpoint)?;
        inject_endpoint(&mut request.configuration, &response.endpoint)?;
        Some((tunnel.client, response.lease))
    } else {
        inject_endpoint(&mut request.configuration, &request.logical_endpoint)?;
        None
    };

    if let Err(error) = run_hooks(
        &request.hooks,
        ConnectionHookStage::PreConnect,
        &mut request.configuration,
        &request.logical_endpoint,
        None,
        None,
        &request.patchable_fields,
        &mut warnings,
    )
    .await
    {
        cleanup_tunnel(&tunnel).await;
        return Err(error);
    }

    let handle = match request
        .provider
        .open(ProviderOpenRequest {
            configuration: request.configuration.clone(),
            credentials: request.credentials,
            tenant_id: request.tenant_id,
        })
        .await
    {
        Ok(handle) => handle,
        Err(error) => {
            notify_failed(
                &request.hooks,
                &request.configuration,
                &request.logical_endpoint,
                &format!("{:?}", error.code),
            )
            .await;
            cleanup_tunnel(&tunnel).await;
            return Err(error.into());
        }
    };
    let server = match request.provider.ping(&handle).await {
        Ok(server) => server,
        Err(error) => {
            notify_failed(
                &request.hooks,
                &request.configuration,
                &request.logical_endpoint,
                &format!("{:?}", error.code),
            )
            .await;
            let _ = request.provider.close(handle).await;
            cleanup_tunnel(&tunnel).await;
            return Err(error.into());
        }
    };
    let sanitized = SanitizedServerInfo {
        server_version: server.server_version.clone(),
        current_database: server.current_database.clone(),
        current_user: server.current_user.clone(),
    };
    if let Err(error) = run_hooks(
        &request.hooks,
        ConnectionHookStage::PostConnect,
        &mut request.configuration,
        &request.logical_endpoint,
        Some(sanitized),
        None,
        &request.patchable_fields,
        &mut warnings,
    )
    .await
    {
        notify_failed(
            &request.hooks,
            &request.configuration,
            &request.logical_endpoint,
            "post_connect",
        )
        .await;
        let _ = request.provider.close(handle).await;
        cleanup_tunnel(&tunnel).await;
        return Err(error);
    }

    Ok(PipelineConnection {
        provider: request.provider,
        handle,
        server,
        warnings,
        configuration: request.configuration,
        logical_endpoint: request.logical_endpoint,
        tunnel,
        hooks: request.hooks,
    })
}

impl PipelineConnection {
    pub async fn close(mut self) -> Result<(), ConnectionPipelineError> {
        let mut warnings = Vec::new();
        let fields = BTreeSet::new();
        let _ = run_hooks(
            &self.hooks,
            ConnectionHookStage::PreClose,
            &mut self.configuration,
            &self.logical_endpoint,
            Some(sanitized_server(&self.server)),
            None,
            &fields,
            &mut warnings,
        )
        .await;
        let provider_result = self.provider.close(self.handle).await;
        let tunnel_result = if let Some((provider, lease)) = self.tunnel {
            provider.close_tunnel(lease).await
        } else {
            Ok(())
        };
        let _ = run_hooks(
            &self.hooks,
            ConnectionHookStage::PostClose,
            &mut self.configuration,
            &self.logical_endpoint,
            Some(sanitized_server(&self.server)),
            provider_result
                .as_ref()
                .err()
                .map(|error| format!("{:?}", error.code)),
            &fields,
            &mut warnings,
        )
        .await;
        provider_result?;
        tunnel_result.map_err(|_| ConnectionPipelineError::Tunnel)
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_hooks(
    hooks: &[HookSelection],
    stage: ConnectionHookStage,
    configuration: &mut serde_json::Value,
    endpoint: &DatabaseEndpoint,
    server: Option<SanitizedServerInfo>,
    failure_code: Option<String>,
    patchable_fields: &BTreeSet<String>,
    warnings: &mut Vec<String>,
) -> Result<(), ConnectionPipelineError> {
    for hook in hooks {
        let outcome = hook
            .client
            .run(ConnectionHookRequest {
                stage,
                configuration: configuration.clone(),
                logical_endpoint: endpoint.clone(),
                server: server.clone(),
                failure_code: failure_code.clone(),
            })
            .await;
        match outcome {
            Ok(response) => {
                if !matches!(
                    stage,
                    ConnectionHookStage::PreResolve | ConnectionHookStage::PreConnect
                ) && !response.configuration_patch.is_empty()
                {
                    if hook.required {
                        return Err(ConnectionPipelineError::ForbiddenPatch);
                    }
                    warnings.push(format!(
                        "optional hook {} returned an ignored patch",
                        hook.contribution_id
                    ));
                    continue;
                }
                if let Err(error) = apply_patch(
                    configuration,
                    response.configuration_patch,
                    patchable_fields,
                ) {
                    if hook.required {
                        return Err(error);
                    }
                    warnings.push(format!(
                        "optional hook {} returned an invalid patch",
                        hook.contribution_id
                    ));
                    continue;
                }
                warnings.extend(response.warnings);
            }
            Err(_) if hook.required => {
                return Err(ConnectionPipelineError::RequiredHookFailed { stage });
            }
            Err(_) => warnings.push(format!(
                "optional hook {} failed at {stage:?}",
                hook.contribution_id
            )),
        }
    }
    Ok(())
}

async fn notify_failed(
    hooks: &[HookSelection],
    configuration: &serde_json::Value,
    endpoint: &DatabaseEndpoint,
    code: &str,
) {
    let mut configuration = configuration.clone();
    let mut warnings = Vec::new();
    let _ = run_hooks(
        hooks,
        ConnectionHookStage::ConnectionFailed,
        &mut configuration,
        endpoint,
        None,
        Some(code.into()),
        &BTreeSet::new(),
        &mut warnings,
    )
    .await;
}

fn apply_patch(
    configuration: &mut serde_json::Value,
    patch: BTreeMap<String, serde_json::Value>,
    patchable_fields: &BTreeSet<String>,
) -> Result<(), ConnectionPipelineError> {
    let object = configuration
        .as_object_mut()
        .ok_or(ConnectionPipelineError::InvalidConfiguration)?;
    for (key, value) in patch {
        if !patchable_fields.contains(&key) || matches!(key.as_str(), "password" | "credentials") {
            return Err(ConnectionPipelineError::ForbiddenPatch);
        }
        object.insert(key, value);
    }
    Ok(())
}

fn validate_credentials(
    response: ResolveCredentialsResponse,
    required: &[String],
) -> Result<HashMap<String, Vec<u8>>, ()> {
    let required: BTreeSet<_> = required.iter().map(String::as_str).collect();
    let mut credentials = HashMap::new();
    for credential in response.credentials {
        if !required.contains(credential.name.as_str())
            || credentials
                .insert(credential.name, credential.value)
                .is_some()
        {
            return Err(());
        }
    }
    if credentials.len() != required.len() {
        return Err(());
    }
    Ok(credentials)
}

fn validate_loopback_endpoint(endpoint: &DatabaseEndpoint) -> Result<(), ConnectionPipelineError> {
    let address: IpAddr = endpoint
        .host
        .parse()
        .map_err(|_| ConnectionPipelineError::InvalidTunnelEndpoint)?;
    if !address.is_loopback() || endpoint.port == 0 {
        return Err(ConnectionPipelineError::InvalidTunnelEndpoint);
    }
    Ok(())
}

fn inject_endpoint(
    configuration: &mut serde_json::Value,
    endpoint: &DatabaseEndpoint,
) -> Result<(), ConnectionPipelineError> {
    let object = configuration
        .as_object_mut()
        .ok_or(ConnectionPipelineError::InvalidConfiguration)?;
    object.insert("host".into(), endpoint.host.clone().into());
    object.insert("port".into(), endpoint.port.into());
    Ok(())
}

fn sort_hooks(hooks: &mut [HookSelection]) {
    hooks.sort_by(|left, right| {
        (left.priority, &left.extension_id, &left.contribution_id).cmp(&(
            right.priority,
            &right.extension_id,
            &right.contribution_id,
        ))
    });
}

async fn cleanup_tunnel(
    tunnel: &Option<(Arc<dyn TunnelProvider>, sift_extension_protocol::WireId)>,
) {
    if let Some((provider, lease)) = tunnel {
        let _ = provider.close_tunnel(*lease).await;
    }
}

fn sanitized_server(server: &ProviderServerInfo) -> SanitizedServerInfo {
    SanitizedServerInfo {
        server_version: server.server_version.clone(),
        current_database: server.current_database.clone(),
        current_user: server.current_user.clone(),
    }
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use sift_extension_protocol::{
        ContributionId, DialectId, DriverSchemaScope, DriverSchemaSnapshot, ExtensionId,
        ProviderId, WireId,
    };
    use sift_protocol::{
        CursorId, Engine, ExecuteRequest, ProviderDescriptor, ProviderQuality, ProviderRef,
        SchemaScope, TxMode,
    };

    use super::*;
    use crate::registry::{ProviderResultStream, ProviderTransactionHandle};

    struct TestProvider {
        descriptor: ProviderDescriptor,
        events: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl DatabaseProvider for TestProvider {
        fn descriptor(&self) -> &ProviderDescriptor {
            &self.descriptor
        }

        fn legacy_engine(&self) -> Option<Engine> {
            None
        }

        async fn open(
            &self,
            request: ProviderOpenRequest,
        ) -> Result<ProviderConnectionHandle, DriverError> {
            assert_eq!(request.credentials["password"], b"secret");
            self.events.lock().unwrap().push("provider.open".into());
            Ok(ProviderConnectionHandle::new(
                self.descriptor.provider.provider_id.clone(),
                (),
            ))
        }

        async fn ping(
            &self,
            _: &ProviderConnectionHandle,
        ) -> Result<ProviderServerInfo, DriverError> {
            self.events.lock().unwrap().push("provider.ping".into());
            Ok(ProviderServerInfo {
                provider: self.descriptor.provider.clone(),
                server_version: "1".into(),
                current_database: "db".into(),
                current_user: "user".into(),
                pool_warm_slots: None,
            })
        }

        async fn schema(
            &self,
            _: &ProviderConnectionHandle,
            _: SchemaScope,
        ) -> Result<DriverSchemaSnapshot, DriverError> {
            Ok(DriverSchemaSnapshot {
                catalogs: vec![],
                fetched_at_unix_ms: 0,
                scope: DriverSchemaScope {
                    depth: sift_extension_protocol::DriverSchemaDepth::Shallow,
                    catalog: None,
                    namespace: None,
                    object: None,
                },
                incomplete: false,
            })
        }

        async fn begin(
            &self,
            _: &ProviderConnectionHandle,
            _: TxMode,
        ) -> Result<ProviderTransactionHandle, DriverError> {
            Err(DriverError::new(
                sift_protocol::Code::UnsupportedForEngine,
                "fixture",
            ))
        }

        async fn commit(&self, _: ProviderTransactionHandle) -> Result<(), DriverError> {
            Ok(())
        }

        async fn rollback(&self, _: ProviderTransactionHandle) -> Result<(), DriverError> {
            Ok(())
        }

        async fn execute(
            &self,
            _: &ProviderConnectionHandle,
            _: ExecuteRequest,
        ) -> Result<ProviderResultStream, DriverError> {
            Err(DriverError::new(
                sift_protocol::Code::UnsupportedForEngine,
                "fixture",
            ))
        }

        async fn cancel(
            &self,
            _: &ProviderConnectionHandle,
            _: CursorId,
        ) -> Result<(), DriverError> {
            Ok(())
        }

        async fn close(&self, _: ProviderConnectionHandle) -> Result<(), DriverError> {
            self.events.lock().unwrap().push("provider.close".into());
            Ok(())
        }
    }

    struct TestTunnel {
        events: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl TunnelProvider for TestTunnel {
        async fn open_tunnel(
            &self,
            _: OpenTunnelRequest,
        ) -> Result<OpenTunnelResponse, PipelineContributionError> {
            self.events.lock().unwrap().push("tunnel.open".into());
            Ok(OpenTunnelResponse {
                endpoint: DatabaseEndpoint {
                    host: "127.0.0.1".into(),
                    port: 15432,
                },
                lease: WireId::from_u128(1),
                expires_at_unix_ms: None,
            })
        }

        async fn close_tunnel(&self, _: WireId) -> Result<(), PipelineContributionError> {
            self.events.lock().unwrap().push("tunnel.close".into());
            Ok(())
        }
    }

    struct TestHook {
        name: &'static str,
        fail: Option<ConnectionHookStage>,
        events: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl ConnectionHook for TestHook {
        async fn run(
            &self,
            request: ConnectionHookRequest,
        ) -> Result<ConnectionHookResponse, PipelineContributionError> {
            assert!(!request.configuration.to_string().contains("secret"));
            self.events
                .lock()
                .unwrap()
                .push(format!("hook.{}.{:?}", self.name, request.stage));
            if self.fail == Some(request.stage) {
                return Err(PipelineContributionError {
                    code: "fixture".into(),
                });
            }
            Ok(ConnectionHookResponse {
                configuration_patch: BTreeMap::new(),
                warnings: vec![],
                disposition: None,
            })
        }
    }

    #[tokio::test]
    async fn required_post_connect_failure_cleans_up_in_reverse_order() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let provider_id = ProviderId::new("acme/provider").unwrap();
        let provider = Arc::new(TestProvider {
            descriptor: ProviderDescriptor {
                provider: ProviderRef {
                    provider_id,
                    dialect_id: DialectId::new("acme/sql").unwrap(),
                    provider_version: "1".into(),
                },
                display_name: "fixture".into(),
                configuration_schema: serde_json::json!({}),
                credential_schema: serde_json::json!({}),
                configuration_schema_version: 1,
                capabilities: vec![],
                quality: Some(ProviderQuality::Compatible),
                available: true,
            },
            events: events.clone(),
        });
        let hook = |name, id, fail| HookSelection {
            extension_id: ExtensionId::new(format!("acme/{name}")).unwrap(),
            contribution_id: ContributionId::new(format!("acme/{name}/connection_hook/{id}"))
                .unwrap(),
            priority: 0,
            required: true,
            client: Arc::new(TestHook {
                name,
                fail,
                events: events.clone(),
            }),
        };
        let result = open_connection_pipeline(ConnectionPipelineRequest {
            provider,
            configuration: serde_json::json!({"database": "db", "user": "user"}),
            logical_endpoint: DatabaseEndpoint {
                host: "db.internal".into(),
                port: 5432,
            },
            credentials: HashMap::from([("password".into(), b"secret".to_vec())]),
            broker: None,
            tunnel: Some(TunnelSelection {
                client: Arc::new(TestTunnel {
                    events: events.clone(),
                }),
                configuration: serde_json::json!({}),
                credentials: vec![],
            }),
            hooks: vec![
                hook("zeta", "z", Some(ConnectionHookStage::PostConnect)),
                hook("alpha", "a", None),
            ],
            patchable_fields: BTreeSet::from(["database".into(), "user".into()]),
            tenant_id: Some(1),
            profile_id: Some(2),
        })
        .await;
        assert!(matches!(
            result,
            Err(ConnectionPipelineError::RequiredHookFailed {
                stage: ConnectionHookStage::PostConnect
            })
        ));
        let events = events.lock().unwrap();
        let alpha_pre = events
            .iter()
            .position(|event| event == "hook.alpha.PreResolve")
            .unwrap();
        let zeta_pre = events
            .iter()
            .position(|event| event == "hook.zeta.PreResolve")
            .unwrap();
        assert!(alpha_pre < zeta_pre);
        assert_eq!(
            &events[events.len() - 2..],
            ["provider.close", "tunnel.close"]
        );
    }

    #[test]
    fn broker_fields_and_tunnel_endpoints_fail_closed() {
        assert!(validate_credentials(
            ResolveCredentialsResponse {
                credentials: vec![sift_extension_protocol::CredentialField {
                    name: "extra".into(),
                    value: vec![1],
                }],
                expires_at_unix_ms: None,
            },
            &["password".into()],
        )
        .is_err());
        assert!(validate_loopback_endpoint(&DatabaseEndpoint {
            host: "192.0.2.1".into(),
            port: 5432,
        })
        .is_err());
    }
}
