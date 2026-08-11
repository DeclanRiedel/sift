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
use sift_driver_api::{Driver, ResultSetStream};
use sift_extension_protocol::{
    driver_rpc_v1_supports_capability, DriverCatalog, DriverColumn, DriverNamespace,
    DriverSchemaDepth, DriverSchemaObject, DriverSchemaScope, DriverSchemaSnapshot,
    DriverStreamPayload, DriverValue,
};
use sift_protocol::{
    Code, ColumnMetadata, DialectId, DriverError, DriverWarning, Engine, ExecuteRequest, Page,
    PrimitiveType, ProviderCapability, ProviderDescriptor, ProviderId, ProviderQuality,
    ProviderRef, SchemaDepth, SchemaScope, SchemaSnapshot, ServerInfo, TxMode, TypeCategory,
    TypeRef, Value,
};
use tokio::sync::mpsc;

static NEXT_EXTERNAL_TX_ID: AtomicU64 = AtomicU64::new(1);

#[async_trait::async_trait]
pub trait DatabaseProvider: Send + Sync {
    fn descriptor(&self) -> &ProviderDescriptor;
    fn available(&self) -> bool {
        self.descriptor().available
    }
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
        _connection: &ProviderConnectionHandle,
        _scope: SchemaScope,
    ) -> Result<DriverSchemaSnapshot, DriverError> {
        Err(unsupported_provider_method(self.descriptor(), "schema"))
    }
    async fn begin(
        &self,
        _connection: &ProviderConnectionHandle,
        _mode: TxMode,
    ) -> Result<ProviderTransactionHandle, DriverError> {
        Err(unsupported_provider_method(
            self.descriptor(),
            "transactions",
        ))
    }
    async fn commit(&self, _transaction: ProviderTransactionHandle) -> Result<(), DriverError> {
        Err(unsupported_provider_method(
            self.descriptor(),
            "transactions",
        ))
    }
    async fn rollback(&self, _transaction: ProviderTransactionHandle) -> Result<(), DriverError> {
        Err(unsupported_provider_method(
            self.descriptor(),
            "transactions",
        ))
    }
    async fn execute(
        &self,
        connection: &ProviderConnectionHandle,
        request: ExecuteRequest,
    ) -> Result<ProviderResultStream, DriverError>;
    async fn cancel(
        &self,
        _connection: &ProviderConnectionHandle,
        _cursor: sift_protocol::CursorId,
    ) -> Result<(), DriverError> {
        Err(unsupported_provider_method(self.descriptor(), "cancel"))
    }
    async fn close(&self, connection: ProviderConnectionHandle) -> Result<(), DriverError>;
}

fn unsupported_provider_method(descriptor: &ProviderDescriptor, method: &str) -> DriverError {
    DriverError::new(
        Code::UnsupportedForEngine,
        format!(
            "provider `{}` does not support {method}",
            descriptor.provider.provider_id
        ),
    )
    .with_provider(descriptor.provider.provider_id.clone())
}

#[derive(Clone)]
pub struct ProviderOpenRequest {
    pub configuration: serde_json::Value,
    pub credentials: HashMap<String, Vec<u8>>,
    pub tenant_id: Option<i64>,
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
                    "driver.schema.graph@1",
                    "driver.cancel@1",
                    "driver.bulk@1",
                    "driver.notifications@1",
                    "driver.process-control@1",
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
                    "driver.schema.graph@1",
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

#[derive(Clone)]
// The built-in descriptor is deliberately inline: boxing it would change the
// locked provider registry surface merely to optimize an infrequently moved
// control-plane enum. Rust 1.96 began flagging the existing size difference.
#[allow(clippy::large_enum_variant)]
pub enum RuntimeDriver {
    Builtin {
        descriptor: ProviderDescriptor,
        driver: Arc<dyn Driver>,
    },
    External(RegisteredProvider),
}

#[derive(Clone)]
pub enum RuntimeConnectionHandle {
    Builtin(sift_driver_api::ConnHandle),
    External(ProviderConnectionHandle),
}

#[derive(Clone)]
pub enum RuntimeTransactionHandle {
    Builtin {
        driver: Arc<dyn Driver>,
        handle: sift_driver_api::TxHandle,
    },
    External {
        provider: RegisteredProvider,
        handle: ProviderTransactionHandle,
        tx_id: sift_protocol::TxId,
        mode: TxMode,
    },
}

impl RuntimeDriver {
    pub fn from_registered(provider: RegisteredProvider) -> Self {
        if let Some(driver) = provider.provider.legacy_driver() {
            Self::Builtin {
                descriptor: provider.provider.descriptor().clone(),
                driver,
            }
        } else {
            Self::External(provider)
        }
    }

    pub fn provider(&self) -> &ProviderRef {
        match self {
            Self::Builtin { descriptor, .. } => &descriptor.provider,
            Self::External(provider) => &provider.provider.descriptor().provider,
        }
    }

    pub fn semantic_engine(&self) -> Option<Engine> {
        match self {
            Self::Builtin { driver, .. } => Some(driver.engine()),
            Self::External(provider) => {
                match provider.provider.descriptor().provider.dialect_id.as_str() {
                    "sift/postgresql" => Some(Engine::Postgres),
                    "sift/tsql" => Some(Engine::SqlServer),
                    _ => None,
                }
            }
        }
    }

    pub fn supports(&self, capability: &str) -> bool {
        self.descriptor()
            .capabilities
            .iter()
            .any(|declared| declared.id == capability)
    }

    pub fn require_capability(&self, capability: &str) -> Result<(), DriverError> {
        if self.supports(capability) {
            Ok(())
        } else {
            Err(DriverError::new(
                Code::UnsupportedForEngine,
                format!(
                    "provider `{}` does not declare `{capability}`",
                    self.provider().provider_id
                ),
            )
            .with_provider(self.provider().provider_id.clone()))
        }
    }

    pub fn supports_operation(&self, operation: sift_protocol::OperationKind) -> bool {
        use sift_protocol::OperationKind;
        let capability = match operation {
            OperationKind::PingConnection
            | OperationKind::ExecuteQuery
            | OperationKind::ExportQuery
            | OperationKind::CloseConnection => "driver.core@1",
            OperationKind::RefreshSchema => {
                return self.supports("driver.schema.shallow@1")
                    || self.supports("driver.schema.deep@1")
                    || self.supports("driver.schema.graph@1");
            }
            OperationKind::ReadCatalogGraph
            | OperationKind::ProjectCatalogDiagram
            | OperationKind::CreateCatalogSnapshot
            | OperationKind::CompareCatalogSchemas
            | OperationKind::PreviewMigration => "driver.schema.graph@1",
            OperationKind::ApplyMigration
            | OperationKind::CancelMigration
            | OperationKind::GetMigrationRun => "driver.core@1",
            OperationKind::BeginTransaction
            | OperationKind::PreviewTransaction
            | OperationKind::CommitTransaction
            | OperationKind::RollbackTransaction => "driver.transactions@1",
            OperationKind::Savepoint
            | OperationKind::RollbackToSavepoint
            | OperationKind::ReleaseSavepoint => "driver.savepoints@1",
            OperationKind::CancelQuery => "driver.cancel@1",
            OperationKind::BulkInsert | OperationKind::ImportCsv => "driver.bulk@1",
            OperationKind::Listen => "driver.notifications@1",
            OperationKind::Explain => "driver.explain@1",
            OperationKind::ListProcesses | OperationKind::KillProcess => "driver.process-control@1",
            OperationKind::GenerateDdl
            | OperationKind::Complete
            | OperationKind::OpenSemanticDocument
            | OperationKind::UpdateSemanticDocument
            | OperationKind::CloseSemanticDocument
            | OperationKind::SelectStatement
            | OperationKind::DiagnoseSql
            | OperationKind::FormatSql
            | OperationKind::SqlQuickFix
            | OperationKind::FindSqlUsages
            | OperationKind::PrepareSqlRefactor
            | OperationKind::CaptureSemanticPlan
            | OperationKind::PreviewEdits
            | OperationKind::ApplyEdits
            | OperationKind::SearchSchema
            | OperationKind::SearchData => {
                return self.semantic_engine().is_some()
                    && (self.supports("driver.schema.shallow@1")
                        || self.supports("driver.schema.deep@1"));
            }
            _ => return true,
        };
        if matches!(self, Self::External(_)) && !driver_rpc_v1_supports_capability(capability) {
            return false;
        }
        self.supports(capability)
    }

    pub fn require_operation(
        &self,
        operation: sift_protocol::OperationKind,
    ) -> Result<(), DriverError> {
        if self.supports_operation(operation) {
            Ok(())
        } else {
            Err(DriverError::new(
                Code::UnsupportedForEngine,
                format!(
                    "provider `{}` does not support operation `{operation:?}`",
                    self.provider().provider_id
                ),
            )
            .with_provider(self.provider().provider_id.clone()))
        }
    }

    pub fn descriptor(&self) -> &ProviderDescriptor {
        match self {
            Self::Builtin { descriptor, .. } => descriptor,
            Self::External(provider) => provider.provider.descriptor(),
        }
    }

    pub fn engine(&self) -> Engine {
        self.semantic_engine()
            .expect("runtime admission rejects unsupported dialects")
    }

    pub fn legacy_driver(&self) -> Option<&Arc<dyn Driver>> {
        match self {
            Self::Builtin { driver, .. } => Some(driver),
            Self::External(_) => None,
        }
    }

    pub fn as_pg(&self) -> Option<&dyn sift_driver_api::PgExt> {
        self.legacy_driver().and_then(|driver| driver.as_pg())
    }

    pub fn as_mssql(&self) -> Option<&dyn sift_driver_api::MssqlExt> {
        self.legacy_driver().and_then(|driver| driver.as_mssql())
    }

    pub async fn open(
        &self,
        configuration: &serde_json::Value,
        credentials: &HashMap<String, Vec<u8>>,
        tenant_id: Option<i64>,
    ) -> Result<RuntimeConnectionHandle, DriverError> {
        match self {
            Self::Builtin { driver, .. } => {
                let mut spec: sift_protocol::ConnectionSpec =
                    serde_json::from_value(configuration.clone()).map_err(|error| {
                        DriverError::new(
                            Code::InvalidParameterValue,
                            format!("invalid bundled provider configuration: {error}"),
                        )
                    })?;
                if let Some(password) = credentials.get("password") {
                    spec.password = Some(String::from_utf8(password.clone()).map_err(|_| {
                        DriverError::new(
                            Code::InvalidParameterValue,
                            "bundled provider password must be UTF-8",
                        )
                    })?);
                }
                driver
                    .open(&spec)
                    .await
                    .map(RuntimeConnectionHandle::Builtin)
            }
            Self::External(registered) => registered
                .provider
                .open(ProviderOpenRequest {
                    configuration: configuration.clone(),
                    credentials: credentials.clone(),
                    tenant_id,
                })
                .await
                .map(RuntimeConnectionHandle::External),
        }
    }

    pub async fn ping(&self, handle: RuntimeConnectionHandle) -> Result<ServerInfo, DriverError> {
        match (self, handle) {
            (Self::Builtin { driver, .. }, RuntimeConnectionHandle::Builtin(handle)) => {
                driver.ping(handle).await
            }
            (Self::External(provider), RuntimeConnectionHandle::External(handle)) => {
                let info = provider.provider.ping(&handle).await?;
                Ok(ServerInfo {
                    provider: info.provider,
                    server_version: info.server_version,
                    current_database: info.current_database,
                    current_user: info.current_user,
                    pool_warm_slots: info.pool_warm_slots,
                })
            }
            _ => Err(runtime_handle_mismatch()),
        }
    }

    pub async fn schema(
        &self,
        handle: RuntimeConnectionHandle,
        scope: SchemaScope,
    ) -> Result<SchemaSnapshot, DriverError> {
        match (self, handle) {
            (Self::Builtin { driver, .. }, RuntimeConnectionHandle::Builtin(handle)) => {
                driver.schema(handle, scope).await
            }
            (Self::External(provider), RuntimeConnectionHandle::External(handle)) => provider
                .provider
                .schema(&handle, scope.clone())
                .await
                .and_then(|snapshot| provider_schema(snapshot, scope, self.provider())),
            _ => Err(runtime_handle_mismatch()),
        }
    }

    pub async fn begin(
        &self,
        handle: RuntimeConnectionHandle,
        mode: TxMode,
    ) -> Result<RuntimeTransactionHandle, DriverError> {
        match (self, handle) {
            (Self::Builtin { driver, .. }, RuntimeConnectionHandle::Builtin(handle)) => driver
                .begin(handle, mode)
                .await
                .map(|handle| RuntimeTransactionHandle::Builtin {
                    driver: driver.clone(),
                    handle,
                }),
            (Self::External(provider), RuntimeConnectionHandle::External(handle)) => {
                provider.provider.begin(&handle, mode).await.map(|handle| {
                    RuntimeTransactionHandle::External {
                        provider: provider.clone(),
                        handle,
                        tx_id: sift_protocol::TxId::new(
                            NEXT_EXTERNAL_TX_ID.fetch_add(1, Ordering::Relaxed),
                        ),
                        mode,
                    }
                })
            }
            _ => Err(runtime_handle_mismatch()),
        }
    }

    pub async fn execute(
        &self,
        handle: RuntimeConnectionHandle,
        request: ExecuteRequest,
    ) -> Result<ResultSetStream, DriverError> {
        match (self, handle) {
            (Self::Builtin { driver, .. }, RuntimeConnectionHandle::Builtin(handle)) => {
                driver.execute(handle, request).await
            }
            (Self::External(provider), RuntimeConnectionHandle::External(handle)) => {
                let stream = provider.provider.execute(&handle, request).await?;
                Ok(provider_stream(stream, self.provider().clone()))
            }
            _ => Err(runtime_handle_mismatch()),
        }
    }

    pub async fn cancel(
        &self,
        handle: RuntimeConnectionHandle,
        cursor: sift_protocol::CursorId,
    ) -> Result<(), DriverError> {
        match (self, handle) {
            (Self::Builtin { driver, .. }, RuntimeConnectionHandle::Builtin(handle)) => {
                driver.cancel(handle, cursor).await
            }
            (Self::External(provider), RuntimeConnectionHandle::External(handle)) => {
                provider.provider.cancel(&handle, cursor).await
            }
            _ => Err(runtime_handle_mismatch()),
        }
    }

    pub async fn close(&self, handle: RuntimeConnectionHandle) -> Result<(), DriverError> {
        match (self, handle) {
            (Self::Builtin { driver, .. }, RuntimeConnectionHandle::Builtin(handle)) => {
                driver.close(handle).await
            }
            (Self::External(provider), RuntimeConnectionHandle::External(handle)) => {
                provider.provider.close(handle).await
            }
            _ => Err(runtime_handle_mismatch()),
        }
    }

    pub async fn commit(&self, transaction: RuntimeTransactionHandle) -> Result<(), DriverError> {
        transaction.commit().await
    }

    pub async fn rollback(&self, transaction: RuntimeTransactionHandle) -> Result<(), DriverError> {
        transaction.rollback().await
    }
}

impl RuntimeTransactionHandle {
    pub fn tx_id(&self) -> sift_protocol::TxId {
        match self {
            Self::Builtin { handle, .. } => handle.tx_id,
            Self::External { tx_id, .. } => *tx_id,
        }
    }

    pub fn mode(&self) -> TxMode {
        match self {
            Self::Builtin { handle, .. } => handle.mode,
            Self::External { mode, .. } => *mode,
        }
    }

    pub async fn commit(self) -> Result<(), DriverError> {
        match self {
            Self::Builtin { driver, handle } => driver.commit(handle).await,
            Self::External {
                provider, handle, ..
            } => provider.provider.commit(handle).await,
        }
    }

    pub async fn rollback(self) -> Result<(), DriverError> {
        match self {
            Self::Builtin { driver, handle } => driver.rollback(handle).await,
            Self::External {
                provider, handle, ..
            } => provider.provider.rollback(handle).await,
        }
    }
}

impl RuntimeConnectionHandle {
    pub fn builtin(&self) -> Option<&sift_driver_api::ConnHandle> {
        match self {
            Self::Builtin(handle) => Some(handle),
            Self::External(_) => None,
        }
    }
}

impl RuntimeTransactionHandle {
    pub fn builtin(&self) -> Option<&sift_driver_api::TxHandle> {
        match self {
            Self::Builtin { handle, .. } => Some(handle),
            Self::External { .. } => None,
        }
    }
}

fn runtime_handle_mismatch() -> DriverError {
    DriverError::new(
        Code::DriverInternal,
        "connection handle belongs to a different provider runtime",
    )
}

fn provider_stream(stream: ProviderResultStream, provider: ProviderRef) -> ResultSetStream {
    let (sender, receiver) = mpsc::channel(16);
    let cursor_id = stream.cursor_id;
    tokio::spawn(async move {
        let mut rows = stream.rows;
        while let Some(payload) = rows.recv().await {
            let terminal = matches!(
                payload,
                DriverStreamPayload::Done { .. } | DriverStreamPayload::Error { .. }
            );
            if sender
                .send(provider_page(payload, &provider))
                .await
                .is_err()
                || terminal
            {
                break;
            }
        }
    });
    ResultSetStream::with_cursor_mode(cursor_id, receiver, stream.server_side_cursor)
}

fn provider_page(payload: DriverStreamPayload, provider: &ProviderRef) -> Page {
    match payload {
        DriverStreamPayload::NextResult { columns } => Page::NextResult {
            columns: columns
                .into_iter()
                .map(|column| provider_column(column, provider))
                .collect(),
        },
        DriverStreamPayload::Rows { rows } => Page::Rows {
            rows: rows
                .into_iter()
                .map(|values| {
                    sift_protocol::Row::new(
                        values
                            .into_iter()
                            .map(|value| provider_value(value, provider))
                            .collect(),
                    )
                })
                .collect(),
        },
        DriverStreamPayload::Done {
            affected_rows,
            warnings,
        } => Page::Done {
            affected_rows,
            warnings: warnings.into_iter().map(DriverWarning::new).collect(),
        },
        DriverStreamPayload::Error { message, .. } => Page::Error {
            error: DriverError::new(Code::DriverInternal, message),
        },
    }
}

fn provider_column(column: DriverColumn, provider: &ProviderRef) -> ColumnMetadata {
    let primitive = match column.type_name.as_str() {
        "int2" | "smallint" => Some(PrimitiveType::Int16),
        "int4" | "integer" | "int" => Some(PrimitiveType::Int32),
        "int8" | "bigint" => Some(PrimitiveType::Int64),
        "float4" | "real" => Some(PrimitiveType::Float32),
        "float8" | "double precision" | "float" => Some(PrimitiveType::Float64),
        "numeric" | "decimal" | "money" => Some(PrimitiveType::Decimal),
        "bool" | "boolean" | "bit" => Some(PrimitiveType::Bool),
        "bytea" | "binary" | "varbinary" => Some(PrimitiveType::Blob),
        "date" => Some(PrimitiveType::Date),
        "time" => Some(PrimitiveType::Time),
        "timestamp" | "datetime" | "datetime2" => Some(PrimitiveType::Timestamp),
        "timestamptz" | "datetimeoffset" => Some(PrimitiveType::TimestampTz),
        "uuid" | "uniqueidentifier" => Some(PrimitiveType::Uuid),
        "json" => Some(PrimitiveType::Json),
        "jsonb" => Some(PrimitiveType::Jsonb),
        "text" | "varchar" | "nvarchar" | "char" | "nchar" => Some(PrimitiveType::Text),
        _ => None,
    };
    ColumnMetadata {
        name: column.name,
        type_ref: primitive.map_or_else(
            || TypeRef::Native {
                provider_id: provider.provider_id.clone(),
                name: column.type_name,
                category: TypeCategory::Other,
            },
            TypeRef::Primitive,
        ),
        nullable: if column.nullable {
            sift_protocol::Nullability::Nullable
        } else {
            sift_protocol::Nullability::NotNullable
        },
        auto_increment: false,
        primary_key: false,
        facets: Default::default(),
    }
}

fn provider_value(value: DriverValue, provider: &ProviderRef) -> Value {
    match value {
        DriverValue::Null { type_name } => Value::TypedNull { type_name },
        DriverValue::Bool(value) => Value::Bool(value),
        DriverValue::I64(value) => Value::Int64(value),
        DriverValue::U64(value) => i64::try_from(value)
            .map(Value::Int64)
            .unwrap_or_else(|_| Value::Decimal(value.to_string())),
        DriverValue::F64(value) => Value::Float64(value),
        DriverValue::Decimal(value) => Value::Decimal(value),
        DriverValue::String(value) => Value::Text(value),
        DriverValue::Bytes(value) => Value::Blob(value),
        DriverValue::Json(value) => Value::Json(value),
        DriverValue::Date(value)
        | DriverValue::Time(value)
        | DriverValue::Timestamp(value)
        | DriverValue::TimestampTz(value)
        | DriverValue::Uuid(value) => Value::Text(value),
        DriverValue::IntervalMicros(value) => {
            Value::Interval(chrono::Duration::microseconds(value))
        }
        DriverValue::Engine { type_name, display } => Value::Native {
            provider_id: provider.provider_id.clone(),
            type_name,
            display_text: display,
        },
    }
}

fn provider_schema(
    snapshot: DriverSchemaSnapshot,
    scope: SchemaScope,
    provider: &ProviderRef,
) -> Result<SchemaSnapshot, DriverError> {
    let wants_graph = matches!(&scope.depth, SchemaDepth::Graph { .. });
    let include_definitions = matches!(
        &scope.depth,
        SchemaDepth::Graph { options } if options.include_definitions
    );
    let snapshot_incomplete = snapshot.incomplete;
    let fetched_at = chrono::DateTime::from_timestamp_millis(snapshot.fetched_at_unix_ms)
        .ok_or_else(|| {
            DriverError::new(Code::DriverInternal, "invalid provider schema timestamp")
        })?;
    let trees: Vec<sift_protocol::CatalogTree> = snapshot
        .catalogs
        .into_iter()
        .map(|catalog| sift_protocol::CatalogTree {
            name: catalog.name,
            schemas: catalog
                .namespaces
                .into_iter()
                .map(|namespace| sift_protocol::SchemaTree {
                    name: namespace.name,
                    objects: namespace
                        .objects
                        .into_iter()
                        .map(|mut object| {
                            let kind = serde_json::from_value(serde_json::Value::String(
                                object.kind.clone(),
                            ))
                            .unwrap_or(sift_protocol::ObjectKind::Table);
                            let mut info = sift_protocol::ObjectInfo::new(object.name, kind);
                            info.columns = object
                                .columns
                                .into_iter()
                                .map(|column| provider_column(column, provider))
                                .collect();
                            info.routine_args = object
                                .attributes
                                .remove("routine_args")
                                .and_then(|value| serde_json::from_value(value).ok());
                            info.indexes = object
                                .attributes
                                .remove("indexes")
                                .and_then(|value| serde_json::from_value(value).ok())
                                .unwrap_or_default();
                            info.constraints = object
                                .attributes
                                .remove("constraints")
                                .and_then(|value| serde_json::from_value(value).ok())
                                .unwrap_or_default();
                            info.triggers = object
                                .attributes
                                .remove("triggers")
                                .and_then(|value| serde_json::from_value(value).ok())
                                .unwrap_or_default();
                            if !include_definitions {
                                for constraint in &mut info.constraints {
                                    constraint.definition = None;
                                }
                                for trigger in &mut info.triggers {
                                    trigger.definition = None;
                                }
                            }
                            info
                        })
                        .collect(),
                })
                .collect(),
        })
        .collect();
    let graph = wants_graph.then(|| {
        let mut coverage = sift_protocol::CatalogCoverage::complete();
        coverage.state = sift_protocol::CatalogCoverageState::Partial;
        coverage
            .failures
            .push(sift_protocol::CatalogCoverageFailure {
                stage: "dependencies".into(),
                schema: None,
                code: if snapshot_incomplete {
                    "provider_reported_incomplete"
                } else {
                    "driver_rpc_v1_dependency_edges_unavailable"
                }
                .into(),
            });
        let mut graph =
            sift_core::catalog::graph_from_trees(&trees, coverage, provider.provider_id.as_str());
        if let SchemaDepth::Graph { options } = &scope.depth {
            sift_core::catalog::project_graph(&mut graph, options);
        }
        graph
    });
    Ok(SchemaSnapshot {
        trees,
        fetched_at,
        scope,
        incomplete: snapshot_incomplete,
        graph,
    })
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
            .map(|registered| {
                let mut descriptor = registered.provider.descriptor().clone();
                descriptor.available = registered.provider.available();
                descriptor
            })
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

    pub fn register_or_replace(
        &self,
        provider: Arc<dyn DatabaseProvider>,
    ) -> Result<(), DriverError> {
        let _guard = self
            .mutation
            .lock()
            .expect("provider mutation lock poisoned");
        let mut next = (*self.snapshot.load_full()).clone();
        let provider_id = provider.descriptor().provider.provider_id.clone();
        if provider_id.is_first_party() && next.providers.contains_key(&provider_id) {
            return Err(DriverError::new(
                Code::AuthFailed,
                "extensions cannot replace a first-party provider",
            ));
        }
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        next.providers.insert(
            provider_id,
            RegisteredProvider {
                provider,
                generation,
            },
        );
        self.snapshot.store(Arc::new(next));
        Ok(())
    }

    pub fn replace_extensions(
        &self,
        providers: impl IntoIterator<Item = Arc<dyn DatabaseProvider>>,
    ) -> Result<(), DriverError> {
        let _guard = self
            .mutation
            .lock()
            .expect("provider mutation lock poisoned");
        let current = self.snapshot.load_full();
        let mut next = ProviderSnapshot {
            providers: current
                .providers
                .iter()
                .filter(|(_, registered)| registered.provider.legacy_driver().is_some())
                .map(|(id, registered)| (id.clone(), registered.clone()))
                .collect(),
            legacy: current.legacy.clone(),
        };
        for provider in providers {
            let provider_id = provider.descriptor().provider.provider_id.clone();
            if provider_id.is_first_party() || next.providers.contains_key(&provider_id) {
                return Err(DriverError::new(
                    Code::AuthFailed,
                    format!("extension provider id `{provider_id}` is reserved or duplicated"),
                ));
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
    properties.insert(
        "ssl_mode".into(),
        serde_json::json!({
            "type": ["string", "null"],
            "enum": ["disable", "prefer", "require", "verify_ca", "verify_full", null]
        }),
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
            namespaces: Vec::new(),
            kinds: Vec::new(),
            include_definitions: false,
            max_nodes: None,
        },
        SchemaDepth::Deep { object } => DriverSchemaScope {
            depth: DriverSchemaDepth::Deep,
            catalog: object.catalog.clone(),
            namespace: object.schema.clone(),
            object: Some(object.name.clone()),
            namespaces: Vec::new(),
            kinds: Vec::new(),
            include_definitions: false,
            max_nodes: None,
        },
        SchemaDepth::Graph { options } => DriverSchemaScope {
            depth: DriverSchemaDepth::Graph,
            catalog: None,
            namespace: None,
            object: None,
            namespaces: options.schemas.clone().unwrap_or_default(),
            kinds: options
                .kinds
                .clone()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|kind| {
                    serde_json::to_value(kind)
                        .ok()
                        .and_then(|value| value.as_str().map(str::to_string))
                })
                .collect(),
            include_definitions: options.include_definitions,
            max_nodes: options.max_nodes,
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
        TypeRef::Native { name, .. } => name,
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
        Value::Native {
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
    use sift_extension_protocol::DriverSchemaScope;

    use super::*;

    struct ExternalFixture {
        descriptor: ProviderDescriptor,
    }

    impl ExternalFixture {
        fn new() -> Self {
            Self {
                descriptor: ProviderDescriptor {
                    provider: ProviderRef {
                        provider_id: ProviderId::new("acme/external").unwrap(),
                        dialect_id: DialectId::new("acme/sql").unwrap(),
                        provider_version: "1.0.0".into(),
                    },
                    display_name: "External fixture".into(),
                    configuration_schema: serde_json::json!({"type": "object"}),
                    credential_schema: serde_json::json!({"type": "object"}),
                    configuration_schema_version: 1,
                    capabilities: vec![ProviderCapability {
                        id: "driver.core@1".into(),
                        limits: BTreeMap::new(),
                    }],
                    quality: Some(ProviderQuality::SiftCertified),
                    available: true,
                },
            }
        }
    }

    #[async_trait::async_trait]
    impl DatabaseProvider for ExternalFixture {
        fn descriptor(&self) -> &ProviderDescriptor {
            &self.descriptor
        }

        fn legacy_engine(&self) -> Option<Engine> {
            None
        }

        async fn open(
            &self,
            _: ProviderOpenRequest,
        ) -> Result<ProviderConnectionHandle, DriverError> {
            Ok(ProviderConnectionHandle::new(
                self.descriptor.provider.provider_id.clone(),
                1_u64,
            ))
        }

        async fn ping(
            &self,
            _: &ProviderConnectionHandle,
        ) -> Result<ProviderServerInfo, DriverError> {
            Ok(ProviderServerInfo {
                provider: self.descriptor.provider.clone(),
                server_version: "fixture-1".into(),
                current_database: "fixture".into(),
                current_user: "fixture".into(),
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
                    depth: DriverSchemaDepth::Shallow,
                    catalog: None,
                    namespace: None,
                    object: None,
                    namespaces: Vec::new(),
                    kinds: Vec::new(),
                    include_definitions: false,
                    max_nodes: None,
                },
                incomplete: false,
            })
        }

        async fn begin(
            &self,
            _: &ProviderConnectionHandle,
            _: TxMode,
        ) -> Result<ProviderTransactionHandle, DriverError> {
            Ok(ProviderTransactionHandle::new(
                self.descriptor.provider.provider_id.clone(),
                2_u64,
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
            let (sender, receiver) = mpsc::channel(3);
            for payload in [
                DriverStreamPayload::NextResult {
                    columns: vec![DriverColumn {
                        name: "answer".into(),
                        type_name: "int8".into(),
                        nullable: false,
                    }],
                },
                DriverStreamPayload::Rows {
                    rows: vec![vec![DriverValue::I64(42)]],
                },
                DriverStreamPayload::Done {
                    affected_rows: None,
                    warnings: vec![],
                },
            ] {
                sender.try_send(payload).unwrap();
            }
            Ok(ProviderResultStream {
                cursor_id: sift_protocol::CursorId::new(77),
                rows: receiver,
                server_side_cursor: true,
            })
        }

        async fn cancel(
            &self,
            _: &ProviderConnectionHandle,
            _: sift_protocol::CursorId,
        ) -> Result<(), DriverError> {
            Ok(())
        }

        async fn close(&self, _: ProviderConnectionHandle) -> Result<(), DriverError> {
            Ok(())
        }
    }

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

    #[tokio::test]
    async fn external_provider_serves_a_normal_session_end_to_end() {
        let registry = DriverRegistry::new();
        let provider: Arc<dyn DatabaseProvider> = Arc::new(ExternalFixture::new());
        registry.providers().replace([provider]).unwrap();
        let store = crate::SessionStore::new(registry);
        let session = store.open_session(sift_protocol::OpenSessionRequest {
            tag: None,
            tenant_id: None,
        });
        let provider_id = ProviderId::new("acme/external").unwrap();
        let connection = store
            .open_provider_connection(
                session.id,
                provider_id.clone(),
                sift_protocol::ConnectionSpec {
                    host: "fixture".into(),
                    port: None,
                    database: None,
                    user: "fixture".into(),
                    password: Some("not-persisted".into()),
                    ssl_mode: None,
                    engine_specific: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(connection.provider_id, provider_id);
        assert_eq!(
            store
                .ping(session.id, connection.id)
                .await
                .unwrap()
                .provider
                .provider_id,
            provider_id
        );
        let result = store
            .execute_http(
                session.id,
                sift_protocol::ExecuteRequestHttp {
                    connection: connection.id,
                    sql: "select 42".into(),
                    params: vec![],
                    tx: None,
                    room_id: None,
                    connection_profile_id: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(result.rows[0].values, vec![Value::Int64(42)]);

        let transaction_error = store
            .begin_transaction(
                session.id,
                sift_protocol::BeginTransactionRequest {
                    connection: connection.id,
                    mode: TxMode::default(),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(
            transaction_error,
            crate::ApiError::Driver(DriverError {
                code: Code::UnsupportedForEngine,
                ..
            })
        ));
        let capabilities = crate::capability::evaluate(
            &store,
            &sift_protocol::OperationCapabilityContext {
                session: Some(session.id),
                connection: Some(connection.id),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        let execute = capabilities
            .iter()
            .find(|item| item.operation == sift_protocol::OperationKind::ExecuteQuery)
            .unwrap();
        assert!(execute.available);
        assert_eq!(execute.provider_id.as_ref(), Some(&provider_id));
        assert!(
            !capabilities
                .iter()
                .find(|item| item.operation == sift_protocol::OperationKind::BeginTransaction)
                .unwrap()
                .available
        );
        store
            .close_connection(session.id, connection.id)
            .await
            .unwrap();
    }
}
