//! Provider-neutral registry with lock-free immutable read snapshots.

use std::{
    any::Any,
    collections::{BTreeMap, HashMap},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

use arc_swap::ArcSwap;
use sift_driver_api::Driver;
use sift_extension_protocol::{
    DriverCatalog, DriverColumn, DriverNamespace, DriverSchemaDepth, DriverSchemaObject,
    DriverSchemaScope, DriverSchemaSnapshot, DriverStreamPayload, DriverValue,
};
use sift_protocol::{
    Code, DialectId, DriverError, Engine, ExecuteRequest, ProviderCapability, ProviderDescriptor,
    ProviderId, ProviderQuality, ProviderRef, SchemaDepth, SchemaScope, TxMode, TypeRef, Value,
};
use tokio::sync::mpsc;

#[async_trait::async_trait]
pub trait DatabaseProvider: Send + Sync {
    fn descriptor(&self) -> &ProviderDescriptor;
    fn legacy_engine(&self) -> Option<Engine>;
    fn legacy_driver(&self) -> Option<Arc<dyn Driver>> {
        None
    }
    async fn open(
        &self,
        request: ProviderOpenRequest,
    ) -> Result<ProviderConnectionHandle, DriverError>;
    async fn ping(
        &self,
        connection: &ProviderConnectionHandle,
    ) -> Result<ProviderServerInfo, DriverError>;
    async fn schema(
        &self,
        connection: &ProviderConnectionHandle,
        scope: SchemaScope,
    ) -> Result<DriverSchemaSnapshot, DriverError>;
    async fn begin(
        &self,
        connection: &ProviderConnectionHandle,
        mode: TxMode,
    ) -> Result<ProviderTransactionHandle, DriverError>;
    async fn commit(&self, transaction: ProviderTransactionHandle) -> Result<(), DriverError>;
    async fn rollback(&self, transaction: ProviderTransactionHandle) -> Result<(), DriverError>;
    async fn execute(
        &self,
        connection: &ProviderConnectionHandle,
        request: ExecuteRequest,
    ) -> Result<ProviderResultStream, DriverError>;
    async fn cancel(
        &self,
        connection: &ProviderConnectionHandle,
        cursor: sift_protocol::CursorId,
    ) -> Result<(), DriverError>;
    async fn close(&self, connection: ProviderConnectionHandle) -> Result<(), DriverError>;
}

#[derive(Clone)]
pub struct ProviderOpenRequest {
    pub configuration: serde_json::Value,
    pub credentials: HashMap<String, Vec<u8>>,
}

#[derive(Debug, Clone)]
pub struct ProviderServerInfo {
    pub provider: ProviderRef,
    pub server_version: String,
    pub current_database: String,
    pub current_user: String,
    pub pool_warm_slots: Option<u32>,
}

#[derive(Clone)]
pub struct ProviderConnectionHandle {
    provider_id: ProviderId,
    inner: Arc<dyn Any + Send + Sync>,
}

impl ProviderConnectionHandle {
    pub fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    pub(crate) fn new<T>(provider_id: ProviderId, inner: T) -> Self
    where
        T: Any + Send + Sync,
    {
        Self {
            provider_id,
            inner: Arc::new(inner),
        }
    }

    pub(crate) fn downcast_ref<T: Any>(&self) -> Option<&T> {
        self.inner.downcast_ref()
    }
}

#[derive(Clone)]
pub struct ProviderTransactionHandle {
    provider_id: ProviderId,
    inner: Arc<dyn Any + Send + Sync>,
}

impl ProviderTransactionHandle {
    pub fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    pub(crate) fn new<T>(provider_id: ProviderId, inner: T) -> Self
    where
        T: Any + Send + Sync,
    {
        Self {
            provider_id,
            inner: Arc::new(inner),
        }
    }

    pub(crate) fn downcast_ref<T: Any>(&self) -> Option<&T> {
        self.inner.downcast_ref()
    }
}

pub struct ProviderResultStream {
    pub cursor_id: sift_protocol::CursorId,
    pub rows: mpsc::Receiver<DriverStreamPayload>,
    pub server_side_cursor: bool,
}

pub struct BuiltinProviderAdapter {
    descriptor: ProviderDescriptor,
    driver: Arc<dyn Driver>,
}

impl BuiltinProviderAdapter {
    pub fn new(driver: Arc<dyn Driver>) -> Self {
        let engine = driver.engine();
        let (provider_id, dialect_id, name, capabilities) = match engine {
            Engine::Postgres => (
                "sift/postgres",
                "sift/postgresql",
                "PostgreSQL",
                vec![
                    "driver.core@1",
                    "driver.transactions@1",
                    "driver.savepoints@1",
                    "driver.schema.shallow@1",
                    "driver.schema.deep@1",
                    "driver.cancel@1",
                    "driver.notifications@1",
                    "driver.explain@1",
                ],
            ),
            Engine::SqlServer => (
                "sift/sql-server",
                "sift/tsql",
                "SQL Server",
                vec![
                    "driver.core@1",
                    "driver.transactions@1",
                    "driver.savepoints@1",
                    "driver.schema.shallow@1",
                    "driver.schema.deep@1",
                    "driver.cancel@1",
                    "driver.bulk@1",
                    "driver.process-control@1",
                    "driver.explain@1",
                ],
            ),
        };
        Self {
            descriptor: ProviderDescriptor {
                provider: ProviderRef {
                    provider_id: ProviderId::new(provider_id).expect("built-in provider id"),
                    dialect_id: DialectId::new(dialect_id).expect("built-in dialect id"),
                    provider_version: env!("CARGO_PKG_VERSION").into(),
                },
                display_name: name.into(),
                configuration_schema: builtin_configuration_schema(engine),
                credential_schema: builtin_credential_schema(),
                configuration_schema_version: 1,
                capabilities: capabilities
                    .into_iter()
                    .map(|id| ProviderCapability {
                        id: id.into(),
                        limits: Default::default(),
                    })
                    .collect(),
                quality: Some(ProviderQuality::SiftCertified),
                available: true,
            },
            driver,
        }
    }
}

#[async_trait::async_trait]
impl DatabaseProvider for BuiltinProviderAdapter {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn legacy_engine(&self) -> Option<Engine> {
        Some(self.driver.engine())
    }

    fn legacy_driver(&self) -> Option<Arc<dyn Driver>> {
        Some(self.driver.clone())
    }

    async fn open(
        &self,
        mut request: ProviderOpenRequest,
    ) -> Result<ProviderConnectionHandle, DriverError> {
        let password = request
            .credentials
            .remove("password")
            .map(|bytes| {
                String::from_utf8(bytes).map_err(|_| {
                    DriverError::new(
                        Code::InvalidParameterValue,
                        "password credential is not valid UTF-8",
                    )
                })
            })
            .transpose()?;
        if let Some(object) = request.configuration.as_object_mut() {
            object.insert(
                "password".into(),
                password.map_or(serde_json::Value::Null, serde_json::Value::String),
            );
        }
        let spec: sift_protocol::ConnectionSpec = serde_json::from_value(request.configuration)
            .map_err(|error| {
                DriverError::new(
                    Code::InvalidParameterValue,
                    format!("invalid built-in provider configuration: {error}"),
                )
            })?;
        let handle = self.driver.open(&spec).await?;
        Ok(ProviderConnectionHandle::new(
            self.descriptor.provider.provider_id.clone(),
            handle,
        ))
    }

    async fn ping(
        &self,
        connection: &ProviderConnectionHandle,
    ) -> Result<ProviderServerInfo, DriverError> {
        let info = self
            .driver
            .ping(self.connection(connection)?.clone())
            .await?;
        Ok(ProviderServerInfo {
            provider: self.descriptor.provider.clone(),
            server_version: info.server_version,
            current_database: info.current_database,
            current_user: info.current_user,
            pool_warm_slots: info.pool_warm_slots,
        })
    }

    async fn schema(
        &self,
        connection: &ProviderConnectionHandle,
        scope: SchemaScope,
    ) -> Result<DriverSchemaSnapshot, DriverError> {
        let snapshot = self
            .driver
            .schema(self.connection(connection)?.clone(), scope)
            .await?;
        Ok(driver_schema(snapshot))
    }

    async fn begin(
        &self,
        connection: &ProviderConnectionHandle,
        mode: TxMode,
    ) -> Result<ProviderTransactionHandle, DriverError> {
        let transaction = self
            .driver
            .begin(self.connection(connection)?.clone(), mode)
            .await?;
        Ok(ProviderTransactionHandle::new(
            self.descriptor.provider.provider_id.clone(),
            transaction,
        ))
    }

    async fn commit(&self, transaction: ProviderTransactionHandle) -> Result<(), DriverError> {
        self.driver
            .commit(self.transaction(&transaction)?.clone())
            .await
    }

    async fn rollback(&self, transaction: ProviderTransactionHandle) -> Result<(), DriverError> {
        self.driver
            .rollback(self.transaction(&transaction)?.clone())
            .await
    }

    async fn execute(
        &self,
        connection: &ProviderConnectionHandle,
        request: ExecuteRequest,
    ) -> Result<ProviderResultStream, DriverError> {
        let stream = self
            .driver
            .execute(self.connection(connection)?.clone(), request)
            .await?;
        let (sender, receiver) = mpsc::channel(8);
        tokio::spawn(async move {
            let mut pages = stream.rows;
            while let Some(page) = pages.recv().await {
                if sender.send(driver_page(page)).await.is_err() {
                    break;
                }
            }
        });
        Ok(ProviderResultStream {
            cursor_id: stream.cursor_id,
            rows: receiver,
            server_side_cursor: stream.server_side_cursor,
        })
    }

    async fn cancel(
        &self,
        connection: &ProviderConnectionHandle,
        cursor: sift_protocol::CursorId,
    ) -> Result<(), DriverError> {
        self.driver
            .cancel(self.connection(connection)?.clone(), cursor)
            .await
    }

    async fn close(&self, connection: ProviderConnectionHandle) -> Result<(), DriverError> {
        self.driver
            .close(self.connection(&connection)?.clone())
            .await
    }
}

impl BuiltinProviderAdapter {
    fn connection<'a>(
        &self,
        handle: &'a ProviderConnectionHandle,
    ) -> Result<&'a sift_driver_api::ConnHandle, DriverError> {
        if handle.provider_id != self.descriptor.provider.provider_id {
            return Err(DriverError::new(
                Code::ConnectionInvalidated,
                "connection belongs to a different provider",
            ));
        }
        handle.downcast_ref().ok_or_else(|| {
            DriverError::new(
                Code::ConnectionInvalidated,
                "connection handle kind does not match provider",
            )
        })
    }

    fn transaction<'a>(
        &self,
        handle: &'a ProviderTransactionHandle,
    ) -> Result<&'a sift_driver_api::TxHandle, DriverError> {
        if handle.provider_id != self.descriptor.provider.provider_id {
            return Err(DriverError::new(
                Code::TransactionNotFound,
                "transaction belongs to a different provider",
            ));
        }
        handle.downcast_ref().ok_or_else(|| {
            DriverError::new(
                Code::TransactionNotFound,
                "transaction handle kind does not match provider",
            )
        })
    }
}

#[derive(Clone)]
pub struct RegisteredProvider {
    pub provider: Arc<dyn DatabaseProvider>,
    pub generation: u64,
}

#[derive(Clone, Default)]
struct ProviderSnapshot {
    providers: HashMap<ProviderId, RegisteredProvider>,
    legacy: HashMap<Engine, ProviderId>,
}

#[derive(Clone)]
pub struct ProviderRegistry {
    snapshot: Arc<ArcSwap<ProviderSnapshot>>,
    mutation: Arc<Mutex<()>>,
    next_generation: Arc<AtomicU64>,
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            snapshot: Arc::new(ArcSwap::from_pointee(ProviderSnapshot::default())),
            mutation: Arc::new(Mutex::new(())),
            next_generation: Arc::new(AtomicU64::new(1)),
        }
    }

    pub fn get(&self, provider_id: &ProviderId) -> Result<RegisteredProvider, DriverError> {
        self.snapshot
            .load()
            .providers
            .get(provider_id)
            .cloned()
            .ok_or_else(|| {
                DriverError::new(
                    Code::UnsupportedForEngine,
                    format!("provider `{provider_id}` is not registered"),
                )
            })
    }

    pub fn get_legacy(&self, engine: Engine) -> Result<RegisteredProvider, DriverError> {
        let snapshot = self.snapshot.load();
        let provider_id = snapshot.legacy.get(&engine).ok_or_else(|| {
            DriverError::new(
                Code::UnsupportedForEngine,
                format!("no provider registered for engine `{engine}`"),
            )
        })?;
        snapshot.providers.get(provider_id).cloned().ok_or_else(|| {
            DriverError::new(Code::DriverInternal, "provider snapshot is inconsistent")
        })
    }

    pub fn descriptors(&self) -> Vec<ProviderDescriptor> {
        let mut descriptors: Vec<_> = self
            .snapshot
            .load()
            .providers
            .values()
            .map(|registered| registered.provider.descriptor().clone())
            .collect();
        descriptors
            .sort_by(|left, right| left.provider.provider_id.cmp(&right.provider.provider_id));
        descriptors
    }

    pub fn replace(
        &self,
        providers: impl IntoIterator<Item = Arc<dyn DatabaseProvider>>,
    ) -> Result<(), DriverError> {
        let _guard = self
            .mutation
            .lock()
            .expect("provider mutation lock poisoned");
        let mut next = ProviderSnapshot::default();
        for provider in providers {
            let provider_id = provider.descriptor().provider.provider_id.clone();
            if next.providers.contains_key(&provider_id) {
                return Err(DriverError::new(
                    Code::DriverInternal,
                    format!("duplicate provider id `{provider_id}`"),
                ));
            }
            if let Some(engine) = provider.legacy_engine() {
                if next.legacy.insert(engine, provider_id.clone()).is_some() {
                    return Err(DriverError::new(
                        Code::DriverInternal,
                        format!("duplicate built-in engine `{engine}`"),
                    ));
                }
            }
            let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
            next.providers.insert(
                provider_id,
                RegisteredProvider {
                    provider,
                    generation,
                },
            );
        }
        self.snapshot.store(Arc::new(next));
        Ok(())
    }
}

/// Compatibility facade used while server call sites move from engine-shaped
/// requests to provider-neutral protocol-v1 requests.
#[derive(Clone, Default)]
pub struct DriverRegistry {
    providers: ProviderRegistry,
}

impl DriverRegistry {
    pub fn new() -> Self {
        Self {
            providers: ProviderRegistry::new(),
        }
    }

    pub fn builder() -> DriverRegistryBuilder {
        DriverRegistryBuilder::default()
    }

    pub fn get(&self, engine: Engine) -> Result<Arc<dyn Driver>, DriverError> {
        self.providers
            .get_legacy(engine)?
            .provider
            .legacy_driver()
            .ok_or_else(|| {
                DriverError::new(
                    Code::UnsupportedForEngine,
                    "provider has no legacy driver adapter",
                )
            })
    }

    pub fn get_provider(
        &self,
        provider_id: &ProviderId,
    ) -> Result<RegisteredProvider, DriverError> {
        self.providers.get(provider_id)
    }

    pub fn providers(&self) -> &ProviderRegistry {
        &self.providers
    }

    pub fn engines(&self) -> Vec<Engine> {
        let snapshot = self.providers.snapshot.load();
        let mut engines: Vec<_> = snapshot.legacy.keys().copied().collect();
        engines.sort_by_key(|engine| engine.to_string());
        engines
    }
}

#[derive(Default)]
pub struct DriverRegistryBuilder {
    drivers: Vec<Arc<dyn Driver>>,
}

impl DriverRegistryBuilder {
    pub fn register<D>(mut self, driver: D) -> Self
    where
        D: Driver + 'static,
    {
        let engine = driver.engine();
        let driver: Arc<dyn Driver> = Arc::new(driver);
        tracing::info!(%engine, "registered built-in provider");
        self.drivers.push(driver);
        self
    }

    pub fn build(self) -> DriverRegistry {
        let providers = ProviderRegistry::new();
        let adapters = self.drivers.into_iter().map(|driver| {
            Arc::new(BuiltinProviderAdapter::new(driver)) as Arc<dyn DatabaseProvider>
        });
        providers
            .replace(adapters)
            .expect("builder rejects duplicate built-in providers");
        DriverRegistry { providers }
    }
}

fn builtin_configuration_schema(engine: Engine) -> serde_json::Value {
    let mut properties = serde_json::Map::new();
    properties.insert(
        "host".into(),
        serde_json::json!({"type": "string", "minLength": 1, "maxLength": 255}),
    );
    properties.insert(
        "port".into(),
        serde_json::json!({"type": "integer", "minimum": 1, "maximum": 65535}),
    );
    properties.insert(
        "database".into(),
        serde_json::json!({"type": ["string", "null"], "maxLength": 255}),
    );
    properties.insert(
        "user".into(),
        serde_json::json!({"type": "string", "minLength": 1, "maxLength": 255}),
    );
    let engine_properties = match engine {
        Engine::Postgres => serde_json::json!({
            "type": ["object", "null"],
            "additionalProperties": false,
            "properties": {
                "search_path": {"type": ["array", "null"], "items": {"type": "string"}},
                "application_name": {"type": ["string", "null"]},
                "connect_timeout_secs": {"type": ["integer", "null"], "minimum": 1},
                "pool_max_size": {"type": ["integer", "null"], "minimum": 1},
                "pool_min_size": {"type": ["integer", "null"], "minimum": 0}
            }
        }),
        Engine::SqlServer => serde_json::json!({
            "type": ["object", "null"],
            "additionalProperties": false,
            "properties": {
                "mars": {"type": "boolean"},
                "encrypt": {"type": ["boolean", "null"]},
                "trust_server_certificate": {"type": ["boolean", "null"]},
                "connect_timeout_secs": {"type": ["integer", "null"], "minimum": 1},
                "pool_min_size": {"type": ["integer", "null"], "minimum": 0}
            }
        }),
    };
    properties.insert("engine_specific".into(), engine_properties);
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "required": ["host", "user"],
        "properties": properties,
    })
}

fn builtin_credential_schema() -> serde_json::Value {
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "password": {
                "type": ["string", "null"],
                "x-sift-secret": true
            }
        }
    })
}

fn driver_schema(snapshot: sift_protocol::SchemaSnapshot) -> DriverSchemaSnapshot {
    let scope = match &snapshot.scope.depth {
        SchemaDepth::Shallow => DriverSchemaScope {
            depth: DriverSchemaDepth::Shallow,
            catalog: snapshot
                .scope
                .filter
                .as_ref()
                .and_then(|filter| filter.catalogs.as_ref())
                .and_then(|catalogs| catalogs.first())
                .cloned(),
            namespace: snapshot
                .scope
                .filter
                .as_ref()
                .and_then(|filter| filter.schemas.as_ref())
                .and_then(|schemas| schemas.first())
                .cloned(),
            object: snapshot
                .scope
                .filter
                .as_ref()
                .and_then(|filter| filter.name_pattern.clone()),
        },
        SchemaDepth::Deep { object } => DriverSchemaScope {
            depth: DriverSchemaDepth::Deep,
            catalog: object.catalog.clone(),
            namespace: object.schema.clone(),
            object: Some(object.name.clone()),
        },
    };
    DriverSchemaSnapshot {
        catalogs: snapshot
            .trees
            .into_iter()
            .map(|catalog| DriverCatalog {
                name: catalog.name,
                namespaces: catalog
                    .schemas
                    .into_iter()
                    .map(|namespace| DriverNamespace {
                        name: namespace.name,
                        objects: namespace
                            .objects
                            .into_iter()
                            .map(|object| {
                                let kind = serde_json::to_value(object.kind)
                                    .ok()
                                    .and_then(|value| value.as_str().map(str::to_owned))
                                    .unwrap_or_else(|| "unknown".into());
                                let mut attributes = match serde_json::to_value(&object) {
                                    Ok(serde_json::Value::Object(fields)) => fields
                                        .into_iter()
                                        .filter(|(key, _)| {
                                            !matches!(key.as_str(), "name" | "kind" | "columns")
                                        })
                                        .collect(),
                                    _ => BTreeMap::new(),
                                };
                                attributes.retain(|_, value| !value.is_null());
                                DriverSchemaObject {
                                    name: object.name,
                                    kind,
                                    columns: object
                                        .columns
                                        .into_iter()
                                        .map(driver_column)
                                        .collect(),
                                    attributes,
                                }
                            })
                            .collect(),
                    })
                    .collect(),
            })
            .collect(),
        fetched_at_unix_ms: snapshot.fetched_at.timestamp_millis(),
        scope,
        incomplete: snapshot.incomplete,
    }
}

fn driver_column(column: sift_protocol::ColumnMetadata) -> DriverColumn {
    let type_name = match column.type_ref {
        TypeRef::Primitive(primitive) => serde_json::to_value(primitive)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_else(|| "unknown".into()),
        TypeRef::Engine { name, .. } => name,
    };
    DriverColumn {
        name: column.name,
        type_name,
        nullable: !matches!(column.nullable, sift_protocol::Nullability::NotNullable),
    }
}

fn driver_page(page: sift_protocol::Page) -> DriverStreamPayload {
    match page {
        sift_protocol::Page::Rows { rows } => DriverStreamPayload::Rows {
            rows: rows
                .into_iter()
                .map(|row| row.values.into_iter().map(driver_value).collect())
                .collect(),
        },
        sift_protocol::Page::NextResult { columns } => DriverStreamPayload::NextResult {
            columns: columns.into_iter().map(driver_column).collect(),
        },
        sift_protocol::Page::Error { error } => DriverStreamPayload::Error {
            code: format!("{:?}", error.code),
            message: error.message,
            disposition: sift_extension_protocol::ConnectionDisposition::Unknown,
        },
        sift_protocol::Page::Done {
            affected_rows,
            warnings,
        } => DriverStreamPayload::Done {
            affected_rows,
            warnings: warnings
                .into_iter()
                .map(|warning| warning.message)
                .collect(),
        },
    }
}

pub(crate) fn driver_value(value: Value) -> DriverValue {
    match value {
        Value::Null => DriverValue::Null {
            type_name: "unknown".into(),
        },
        Value::TypedNull { type_name } => DriverValue::Null { type_name },
        Value::Bool(value) => DriverValue::Bool(value),
        Value::Int16(value) => DriverValue::I64(i64::from(value)),
        Value::Int32(value) => DriverValue::I64(i64::from(value)),
        Value::Int64(value) => DriverValue::I64(value),
        Value::Float32(value) => DriverValue::F64(f64::from(value)),
        Value::Float64(value) => DriverValue::F64(value),
        Value::Decimal(value) => DriverValue::Decimal(value),
        Value::Text(value) => DriverValue::String(value),
        Value::Blob(value) => DriverValue::Bytes(value),
        Value::Date(value) => DriverValue::Date(value.to_string()),
        Value::Time(value) => DriverValue::Time(value.to_string()),
        Value::Timestamp(value) => DriverValue::Timestamp(value.to_string()),
        Value::TimestampTz(value) => DriverValue::TimestampTz(value.to_rfc3339()),
        Value::Interval(value) => DriverValue::IntervalMicros(value.num_microseconds().unwrap_or(
            if value < chrono::Duration::zero() {
                i64::MIN
            } else {
                i64::MAX
            },
        )),
        Value::Uuid(value) => DriverValue::Uuid(value.to_string()),
        Value::Json(value) => DriverValue::Json(value),
        Value::Engine {
            type_name,
            display_text,
            ..
        } => DriverValue::Engine {
            type_name,
            display: display_text,
        },
    }
}

#[cfg(test)]
mod tests {
    use sift_driver_api::mock::MockDriver;

    use super::*;

    #[test]
    fn builtins_dispatch_through_provider_ids_and_legacy_facade() {
        let registry = DriverRegistry::builder()
            .register(MockDriver::builder().engine(Engine::Postgres).build())
            .build();
        let id = ProviderId::new("sift/postgres").unwrap();
        let selected = registry.get_provider(&id).unwrap();
        assert_eq!(selected.provider.legacy_engine(), Some(Engine::Postgres));
        assert_eq!(
            registry.get(Engine::Postgres).unwrap().engine(),
            Engine::Postgres
        );
        assert_eq!(
            registry.providers().descriptors()[0].provider.provider_id,
            id
        );
    }

    #[test]
    fn replaced_snapshots_do_not_reroute_captured_generations() {
        let providers = ProviderRegistry::new();
        let first: Arc<dyn DatabaseProvider> = Arc::new(BuiltinProviderAdapter::new(Arc::new(
            MockDriver::builder().engine(Engine::Postgres).build(),
        )));
        providers.replace([first]).unwrap();
        let id = ProviderId::new("sift/postgres").unwrap();
        let captured = providers.get(&id).unwrap();

        let replacement: Arc<dyn DatabaseProvider> = Arc::new(BuiltinProviderAdapter::new(
            Arc::new(MockDriver::builder().engine(Engine::Postgres).build()),
        ));
        providers.replace([replacement]).unwrap();
        assert_ne!(captured.generation, providers.get(&id).unwrap().generation);
        assert_eq!(captured.provider.descriptor().provider.provider_id, id);
    }
}
