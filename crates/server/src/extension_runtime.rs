use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use sha2::{Digest as _, Sha256};
use sift_extension_protocol::{
    ContributionContext, ContributionId, ExtensionId, ExtensionManifest, LifecycleMode, Request,
    Response, SegmentId, WireId,
};
use sift_metadata::{MetadataStore, SelectedExtensionPackage};
use sift_plugin_host::{
    ExtensionPackageRegistry, GenerationKey, GenerationLimiter, GenerationPermit, ProcessSpec,
    RestartBudget, SupervisedProcess, SupervisorLimits,
};
use sift_protocol::{
    Code, DriverError, Engine, ExecuteRequest, ExtensionOperation, GovernedToolDescriptor,
    ProviderCapability, ProviderDescriptor, ProviderQuality, ProviderRef, SchemaScope, TxMode,
};
use tokio::sync::Mutex;

use crate::{
    extension_dispatch::{ActionInvoker, ActionRegistration},
    registry::{
        ProviderConnectionHandle, ProviderOpenRequest, ProviderResultStream,
        ProviderTransactionHandle,
    },
    DatabaseProvider, ProviderServerInfo, RpcProvider,
};

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

struct RuntimeProcess {
    process: Arc<SupervisedProcess>,
    _permit: GenerationPermit,
}

struct TenantProcessSlot {
    process: Option<Arc<RuntimeProcess>>,
    restart: RestartBudget,
    retry_at: Option<Instant>,
    quarantined: bool,
}

impl Default for TenantProcessSlot {
    fn default() -> Self {
        Self {
            process: None,
            restart: RestartBudget::new(Default::default()),
            retry_at: None,
            quarantined: false,
        }
    }
}

type TenantProcessSlotRef = Arc<Mutex<TenantProcessSlot>>;

struct ExtensionProcessRuntime {
    extension_id: ExtensionId,
    extension_version: String,
    manifest_sha256: String,
    executable: PathBuf,
    working_directory: PathBuf,
    expected_contributions: Vec<ContributionId>,
    granted_capabilities: Vec<String>,
    limits: SupervisorLimits,
    metadata: MetadataStore,
    generation_limiter: Arc<GenerationLimiter>,
    processes: Mutex<HashMap<Option<i64>, TenantProcessSlotRef>>,
}

struct RuntimeActionInvoker {
    runtime: Arc<ExtensionProcessRuntime>,
}

#[async_trait::async_trait]
impl ActionInvoker for RuntimeActionInvoker {
    async fn request(
        &self,
        tenant_id: Option<i64>,
        request: Request,
    ) -> Result<Response, sift_plugin_host::SupervisorError> {
        let process = self.runtime.process(tenant_id).await.map_err(|error| {
            sift_plugin_host::SupervisorError::ProtocolViolation(error.to_string())
        })?;
        process.request(request).await
    }
}

pub struct InstalledExtensionRuntimes {
    pub providers: Vec<Arc<dyn DatabaseProvider>>,
    pub actions: Vec<ActionRegistration>,
    pub tools: Vec<GovernedToolDescriptor>,
}

impl ExtensionProcessRuntime {
    async fn process(&self, tenant_id: Option<i64>) -> Result<Arc<SupervisedProcess>, DriverError> {
        if let Some(tenant_id) = tenant_id {
            if !self
                .metadata
                .extension_tenant_allowed(self.extension_id.as_str(), tenant_id)
                .map_err(metadata_error)?
            {
                return Err(DriverError::new(
                    Code::AuthFailed,
                    "extension is not allowed for this tenant",
                ));
            }
        }
        let slot = {
            let mut processes = self.processes.lock().await;
            processes
                .entry(tenant_id)
                .or_insert_with(|| Arc::new(Mutex::new(TenantProcessSlot::default())))
                .clone()
        };
        let mut slot = slot.lock().await;
        if let Some(generation) = &slot.process {
            if generation.process.health().await == sift_plugin_host::GenerationHealth::Ready {
                return Ok(generation.process.clone());
            }
            slot.process = None;
            record_failure(&mut slot)?;
        }
        if slot.quarantined {
            return Err(quarantined());
        }
        if let Some(retry_at) = slot.retry_at {
            tokio::time::sleep(retry_at.saturating_duration_since(Instant::now())).await;
            slot.retry_at = None;
        }
        let permit = self
            .generation_limiter
            .acquire(GenerationKey {
                extension_id: self.extension_id.clone(),
                tenant_id: tenant_id.unwrap_or(i64::MIN),
            })
            .map_err(|error| {
                DriverError::new(
                    Code::TenantResourceExhausted,
                    format!("extension process admission failed: {error}"),
                )
            })?;
        let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
        let process = match SupervisedProcess::start(
            ProcessSpec {
                executable: self.executable.clone(),
                working_directory: self.working_directory.clone(),
                extension_id: self.extension_id.clone(),
                extension_version: self.extension_version.clone(),
                manifest_sha256: self.manifest_sha256.clone(),
                expected_contributions: self.expected_contributions.clone(),
                generation: WireId::from_u128(u128::from(generation)),
                granted_capabilities: self.granted_capabilities.clone(),
            },
            self.limits.clone(),
        )
        .await
        {
            Ok(process) => Arc::new(process),
            Err(error) => {
                record_failure(&mut slot)?;
                return Err(DriverError::new(
                    Code::ConnectionFailed,
                    format!("extension process failed to start: {error}"),
                ));
            }
        };
        slot.process = Some(Arc::new(RuntimeProcess {
            process: process.clone(),
            _permit: permit,
        }));
        Ok(process)
    }
}

fn record_failure(slot: &mut TenantProcessSlot) -> Result<(), DriverError> {
    match slot.restart.record_failure(Instant::now()) {
        Some(backoff) => {
            slot.retry_at = Some(Instant::now() + backoff);
            Ok(())
        }
        None => {
            slot.quarantined = true;
            Err(quarantined())
        }
    }
}

fn quarantined() -> DriverError {
    DriverError::new(
        Code::ConnectionFailed,
        "extension restart budget exhausted; generation is quarantined",
    )
}

struct TenantScopedRpcProvider {
    descriptor: ProviderDescriptor,
    contribution_id: ContributionId,
    runtime: Arc<ExtensionProcessRuntime>,
}

struct ScopedConnection {
    provider: Arc<RpcProvider>,
    handle: ProviderConnectionHandle,
}

struct ScopedTransaction {
    provider: Arc<RpcProvider>,
    handle: ProviderTransactionHandle,
}

impl TenantScopedRpcProvider {
    fn connection<'a>(
        &self,
        handle: &'a ProviderConnectionHandle,
    ) -> Result<&'a ScopedConnection, DriverError> {
        handle.downcast_ref().ok_or_else(invalid_connection)
    }

    fn transaction<'a>(
        &self,
        handle: &'a ProviderTransactionHandle,
    ) -> Result<&'a ScopedTransaction, DriverError> {
        handle.downcast_ref().ok_or_else(|| {
            DriverError::new(
                Code::TransactionNotFound,
                "transaction handle does not belong to this provider generation",
            )
        })
    }
}

#[async_trait::async_trait]
impl DatabaseProvider for TenantScopedRpcProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn legacy_engine(&self) -> Option<Engine> {
        match self.descriptor.provider.dialect_id.as_str() {
            "sift/postgresql" => Some(Engine::Postgres),
            "sift/tsql" => Some(Engine::SqlServer),
            _ => None,
        }
    }

    async fn open(
        &self,
        request: ProviderOpenRequest,
    ) -> Result<ProviderConnectionHandle, DriverError> {
        let process = self.runtime.process(request.tenant_id).await?;
        let provider = Arc::new(RpcProvider::new(
            self.descriptor.clone(),
            self.contribution_id.clone(),
            process,
        )?);
        let handle = provider.open(request).await?;
        Ok(ProviderConnectionHandle::new(
            self.descriptor.provider.provider_id.clone(),
            ScopedConnection { provider, handle },
        ))
    }

    async fn ping(
        &self,
        connection: &ProviderConnectionHandle,
    ) -> Result<ProviderServerInfo, DriverError> {
        let connection = self.connection(connection)?;
        connection.provider.ping(&connection.handle).await
    }

    async fn schema(
        &self,
        connection: &ProviderConnectionHandle,
        scope: SchemaScope,
    ) -> Result<sift_extension_protocol::DriverSchemaSnapshot, DriverError> {
        let connection = self.connection(connection)?;
        connection.provider.schema(&connection.handle, scope).await
    }

    async fn begin(
        &self,
        connection: &ProviderConnectionHandle,
        mode: TxMode,
    ) -> Result<ProviderTransactionHandle, DriverError> {
        let connection = self.connection(connection)?;
        let handle = connection.provider.begin(&connection.handle, mode).await?;
        Ok(ProviderTransactionHandle::new(
            self.descriptor.provider.provider_id.clone(),
            ScopedTransaction {
                provider: connection.provider.clone(),
                handle,
            },
        ))
    }

    async fn commit(&self, transaction: ProviderTransactionHandle) -> Result<(), DriverError> {
        let transaction = self.transaction(&transaction)?;
        transaction
            .provider
            .commit(transaction.handle.clone())
            .await
    }

    async fn rollback(&self, transaction: ProviderTransactionHandle) -> Result<(), DriverError> {
        let transaction = self.transaction(&transaction)?;
        transaction
            .provider
            .rollback(transaction.handle.clone())
            .await
    }

    async fn execute(
        &self,
        connection: &ProviderConnectionHandle,
        request: ExecuteRequest,
    ) -> Result<ProviderResultStream, DriverError> {
        let connection = self.connection(connection)?;
        connection
            .provider
            .execute(&connection.handle, request)
            .await
    }

    async fn cancel(
        &self,
        connection: &ProviderConnectionHandle,
        cursor: sift_protocol::CursorId,
    ) -> Result<(), DriverError> {
        let connection = self.connection(connection)?;
        connection.provider.cancel(&connection.handle, cursor).await
    }

    async fn close(&self, connection: ProviderConnectionHandle) -> Result<(), DriverError> {
        let connection = self.connection(&connection)?;
        connection.provider.close(connection.handle.clone()).await
    }
}

pub fn installed_extension_runtimes(
    registry: &ExtensionPackageRegistry,
    metadata: &MetadataStore,
    generation_limiter: Arc<GenerationLimiter>,
) -> Result<InstalledExtensionRuntimes, DriverError> {
    let mut providers = Vec::new();
    let mut actions = Vec::new();
    let mut tools = Vec::new();
    for package in metadata
        .selected_extension_packages()
        .map_err(metadata_error)?
    {
        if !package.selection.enabled {
            continue;
        }
        let manifest: ExtensionManifest =
            serde_json::from_str(&package.manifest_json).map_err(protocol_error)?;
        if manifest.artifacts.is_empty() {
            continue;
        }
        let root = registry
            .package_root(&package.selection.selected_archive_sha256)
            .map_err(|error| DriverError::new(Code::DriverInternal, error.to_string()))?;
        let target = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
        let artifact = manifest
            .artifacts
            .iter()
            .find(|artifact| artifact.target == target)
            .ok_or_else(|| {
                DriverError::new(
                    Code::UnsupportedForEngine,
                    format!("extension {} has no artifact for {target}", manifest.id),
                )
            })?;
        let executable = root.join(&artifact.path);
        let expected_contributions = manifest_contribution_ids(&manifest)?;
        let granted_capabilities = metadata
            .extension_grants(manifest.id.as_str())
            .map_err(metadata_error)?;
        let limits = SupervisorLimits {
            handshake_timeout: Duration::from_millis(u64::from(
                manifest.lifecycle.readiness_deadline_ms,
            )),
            ..SupervisorLimits::default()
        };
        let runtime = Arc::new(ExtensionProcessRuntime {
            extension_id: manifest.id.clone(),
            extension_version: package.version.clone(),
            manifest_sha256: package.manifest_sha256.clone(),
            executable,
            working_directory: root,
            expected_contributions,
            granted_capabilities,
            limits,
            metadata: metadata.clone(),
            generation_limiter: generation_limiter.clone(),
            processes: Mutex::new(HashMap::new()),
        });
        for contribution in &manifest.contributions.database_provider {
            let contribution_id = ContributionId::new(format!(
                "{}/database_provider/{}",
                manifest.id, contribution.id
            ))
            .map_err(|error| protocol_error(error.to_string()))?;
            let configuration_schema =
                load_schema(registry, &package, &contribution.config_schema)?;
            let credential_schema =
                load_schema(registry, &package, &contribution.credential_schema)?;
            let descriptor = ProviderDescriptor {
                provider: ProviderRef {
                    provider_id: contribution.provider_id.clone(),
                    dialect_id: contribution.dialect_id.clone(),
                    provider_version: package.version.clone(),
                },
                display_name: manifest.name.clone(),
                configuration_schema,
                credential_schema,
                configuration_schema_version: 1,
                capabilities: contribution
                    .capabilities
                    .iter()
                    .map(|id| ProviderCapability {
                        id: id.clone(),
                        limits: BTreeMap::new(),
                    })
                    .collect(),
                quality: Some(ProviderQuality::Compatible),
                available: true,
            };
            providers.push(Arc::new(TenantScopedRpcProvider {
                descriptor,
                contribution_id,
                runtime: runtime.clone(),
            }) as Arc<dyn DatabaseProvider>);
        }
        for (kind, contribution) in manifest
            .contributions
            .command
            .iter()
            .map(|contribution| ("command", contribution))
            .chain(
                manifest
                    .contributions
                    .governed_tool
                    .iter()
                    .map(|contribution| ("governed_tool", contribution)),
            )
        {
            let contribution_id =
                ContributionId::new(format!("{}/{kind}/{}", manifest.id, contribution.id))
                    .map_err(|error| protocol_error(error.to_string()))?;
            let input_schema = load_schema(registry, &package, &contribution.input_schema)?;
            let output_schema = load_schema(registry, &package, &contribution.output_schema)?;
            let operation = ExtensionOperation {
                extension_id: manifest.id.clone(),
                contribution_id: contribution_id.clone(),
                action: contribution.action.clone(),
                classification: contribution.classification,
                target_kind: target_kind(&contribution.required_context)?,
                target_id: None,
                sanitized_arguments: BTreeMap::new(),
            };
            actions.push(ActionRegistration {
                extension_id: manifest.id.clone(),
                contribution_id: contribution_id.clone(),
                action: contribution.action.clone(),
                classification: contribution.classification,
                input_schema: input_schema.clone(),
                output_schema: output_schema.clone(),
                timeout: Duration::from_millis(u64::from(contribution.timeout_ms)),
                max_result_bytes: contribution.max_result_bytes,
                invoker: Arc::new(RuntimeActionInvoker {
                    runtime: runtime.clone(),
                }),
            });
            if kind == "governed_tool" {
                tools.push(GovernedToolDescriptor {
                    id: governed_tool_id(&contribution_id, &contribution.action),
                    title: contribution.id.to_string(),
                    description: manifest.description.clone(),
                    operation,
                    input_schema,
                    output_schema,
                    required_context: contribution.required_context.clone(),
                    mcp_exposable: contribution.mcp_exposable,
                    schedulable: contribution.schedulable,
                    interactive: contribution.interactive,
                });
            }
        }
        if manifest.lifecycle.mode == LifecycleMode::Eager {
            tracing::debug!(
                extension_id = %manifest.id,
                "eager extension awaits its first tenant context"
            );
        }
    }
    Ok(InstalledExtensionRuntimes {
        providers,
        actions,
        tools,
    })
}

fn governed_tool_id(contribution_id: &ContributionId, action: &SegmentId) -> String {
    let readable = format!("{}.{}", contribution_id.as_str().replace('/', "."), action);
    if readable.len() <= 128 {
        return readable;
    }
    let digest = Sha256::digest(readable.as_bytes());
    format!("sift.extension.{digest:x}")
}

fn target_kind(contexts: &[ContributionContext]) -> Result<SegmentId, DriverError> {
    let value = match contexts.last() {
        None | Some(ContributionContext::Instance) => "instance",
        Some(ContributionContext::Tenant) => "tenant",
        Some(ContributionContext::Room) => "room",
        Some(ContributionContext::Profile) => "profile",
        Some(ContributionContext::Connection) => "connection",
        Some(ContributionContext::Document) => "document",
    };
    SegmentId::new(value).map_err(|error| protocol_error(error.to_string()))
}

fn load_schema(
    registry: &ExtensionPackageRegistry,
    package: &SelectedExtensionPackage,
    path: &str,
) -> Result<serde_json::Value, DriverError> {
    let bytes = registry
        .read_package_file(&package.selection.selected_archive_sha256, path, 256 * 1024)
        .map_err(|error| DriverError::new(Code::DriverInternal, error.to_string()))?;
    serde_json::from_slice(&bytes).map_err(protocol_error)
}

fn manifest_contribution_ids(
    manifest: &ExtensionManifest,
) -> Result<Vec<ContributionId>, DriverError> {
    let mut ids = Vec::new();
    macro_rules! add {
        ($values:expr, $kind:literal) => {
            for contribution in $values {
                ids.push(
                    ContributionId::new(format!("{}/{}/{}", manifest.id, $kind, contribution.id))
                        .map_err(|error| protocol_error(error.to_string()))?,
                );
            }
        };
    }
    add!(
        &manifest.contributions.database_provider,
        "database_provider"
    );
    add!(&manifest.contributions.tunnel_provider, "tunnel_provider");
    add!(
        &manifest.contributions.credential_broker,
        "credential_broker"
    );
    add!(&manifest.contributions.connection_hook, "connection_hook");
    add!(&manifest.contributions.import_format, "import_format");
    add!(&manifest.contributions.export_format, "export_format");
    add!(&manifest.contributions.dialect_pack, "dialect_pack");
    add!(&manifest.contributions.command, "command");
    add!(&manifest.contributions.governed_tool, "governed_tool");
    add!(&manifest.contributions.agent_context, "agent_context");
    add!(&manifest.contributions.client_panel, "client_panel");
    Ok(ids)
}

fn invalid_connection() -> DriverError {
    DriverError::new(
        Code::ConnectionInvalidated,
        "connection handle does not belong to this provider generation",
    )
}

fn metadata_error(error: sift_metadata::MetadataError) -> DriverError {
    DriverError::new(Code::DriverInternal, error.to_string())
}

fn protocol_error(error: impl std::fmt::Display) -> DriverError {
    DriverError::new(Code::InvalidParameterValue, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_metadata::{secrets::MemorySecretStore, UpdateExtensionSelection};
    use sift_protocol::{ExtensionIsolation, ExtensionLifecycleState};

    #[test]
    fn governed_tool_ids_are_mcp_safe_and_bounded() {
        let ordinary = ContributionId::new("acme/conformance/governed_tool/inspect").unwrap();
        let action = SegmentId::new("run").unwrap();
        assert_eq!(
            governed_tool_id(&ordinary, &action),
            "acme.conformance.governed_tool.inspect.run"
        );

        let segment = "a".repeat(45);
        let long =
            ContributionId::new(format!("{segment}/{segment}/governed_tool/{segment}")).unwrap();
        let id = governed_tool_id(&long, &action);
        assert!(id.len() <= 128);
        assert!(id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')));
    }

    #[test]
    fn repeated_generation_failures_quarantine_the_tenant_slot() {
        let mut slot = TenantProcessSlot::default();
        for _ in 0..5 {
            record_failure(&mut slot).unwrap();
        }
        assert!(record_failure(&mut slot).is_err());
        assert!(slot.quarantined);
    }

    #[tokio::test]
    async fn selected_provider_is_discovered_but_tenant_start_is_deny_by_default() {
        let directory = tempfile::tempdir().unwrap();
        let extension = directory.path().join("extension");
        std::fs::create_dir(&extension).unwrap();
        let target = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
        std::fs::write(extension.join("provider"), b"not started").unwrap();
        std::fs::write(
            extension.join("config.json"),
            br#"{"type":"object","additionalProperties":false}"#,
        )
        .unwrap();
        std::fs::write(
            extension.join("credentials.json"),
            br#"{"type":"object","properties":{"password":{"type":"string","x-sift-secret":true}}}"#,
        )
        .unwrap();
        std::fs::write(
            extension.join("sift-extension.toml"),
            format!(
                r#"schema_version = 1
id = "acme/conformance"
name = "Conformance"
version = "1.0.0"
authors = ["Acme"]
description = "fixture"
license = "MIT"
repository = "https://example.invalid/acme/conformance"
minimum_sift_version = "0.1.0"

[compatibility]
public_protocol = {{ minimum = 1, maximum = 1 }}
extension_rpc = {{ minimum = 1, maximum = 1 }}
driver_rpc = {{ minimum = 1, maximum = 1 }}

[[artifacts]]
target = "{target}"
path = "provider"
sha256 = "{}"
byte_length = 11

[[contributions.database_provider]]
id = "fixture"
provider_id = "acme/conformance"
dialect_id = "sift/postgresql"
config_schema = "config.json"
credential_schema = "credentials.json"
capabilities = ["driver.core@1"]
"#,
                "0".repeat(64)
            ),
        )
        .unwrap();

        let metadata =
            MetadataStore::open_in_memory(Arc::new(MemorySecretStore::default())).unwrap();
        let registry = ExtensionPackageRegistry::new(
            directory.path().join("state"),
            sift_plugin_host::PackageLimits::default(),
            metadata.clone(),
        );
        let installed = registry
            .register_development_override(&extension, false, false)
            .unwrap();
        metadata
            .update_extension_selection(UpdateExtensionSelection {
                extension_id: "acme/conformance",
                selected_archive_sha256: Some(&installed.validated.archive_sha256),
                enabled: true,
                lifecycle: ExtensionLifecycleState::Ready,
                isolation: ExtensionIsolation::ProcessOnly,
                quarantine_reason: None,
                expected_revision: 0,
            })
            .unwrap();

        let runtimes = installed_extension_runtimes(
            &registry,
            &metadata,
            Arc::new(GenerationLimiter::new(Default::default())),
        )
        .unwrap();
        assert_eq!(runtimes.providers.len(), 1);
        assert_eq!(
            runtimes.providers[0]
                .descriptor()
                .provider
                .provider_id
                .as_str(),
            "acme/conformance"
        );
        let result = runtimes.providers[0]
            .open(ProviderOpenRequest {
                configuration: serde_json::json!({}),
                credentials: HashMap::new(),
                tenant_id: Some(7),
            })
            .await;
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("tenant without an allowlist entry must be rejected"),
        };
        assert_eq!(error.code, Code::AuthFailed);
    }
}
