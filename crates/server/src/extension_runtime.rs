use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use sift_extension_protocol::{
    ContributionId, ExtensionId, ExtensionManifest, LifecycleMode, WireId,
};
use sift_metadata::{MetadataStore, SelectedExtensionPackage};
use sift_plugin_host::{
    ExtensionPackageRegistry, ProcessSpec, SupervisedProcess, SupervisorLimits,
};
use sift_protocol::{
    Code, DriverError, Engine, ExecuteRequest, ProviderCapability, ProviderDescriptor,
    ProviderQuality, ProviderRef, SchemaScope, TxMode,
};
use tokio::sync::Mutex;

use crate::{
    registry::{
        ProviderConnectionHandle, ProviderOpenRequest, ProviderResultStream,
        ProviderTransactionHandle,
    },
    DatabaseProvider, ProviderServerInfo, RpcProvider,
};

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

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
    processes: Mutex<HashMap<Option<i64>, Arc<SupervisedProcess>>>,
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
        let mut processes = self.processes.lock().await;
        if let Some(process) = processes.get(&tenant_id) {
            return Ok(process.clone());
        }
        let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
        let process = SupervisedProcess::start(
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
        .map_err(|error| {
            DriverError::new(
                Code::ConnectionFailed,
                format!("extension process failed to start: {error}"),
            )
        })?;
        let process = Arc::new(process);
        processes.insert(tenant_id, process.clone());
        Ok(process)
    }
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

pub fn installed_provider_runtimes(
    registry: &ExtensionPackageRegistry,
    metadata: &MetadataStore,
) -> Result<Vec<Arc<dyn DatabaseProvider>>, DriverError> {
    let mut providers = Vec::new();
    for package in metadata
        .selected_extension_packages()
        .map_err(metadata_error)?
    {
        if !package.selection.enabled {
            continue;
        }
        let manifest: ExtensionManifest =
            serde_json::from_str(&package.manifest_json).map_err(protocol_error)?;
        if manifest.contributions.database_provider.is_empty() {
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
        if manifest.lifecycle.mode == LifecycleMode::Eager {
            tracing::debug!(
                extension_id = %manifest.id,
                "eager extension awaits its first tenant context"
            );
        }
    }
    Ok(providers)
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

        let providers = installed_provider_runtimes(&registry, &metadata).unwrap();
        assert_eq!(providers.len(), 1);
        assert_eq!(
            providers[0].descriptor().provider.provider_id.as_str(),
            "acme/conformance"
        );
        let result = providers[0]
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
