use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use sift_extension_protocol::{
    BeginRequest, CancelRequest, ContributionId, CredentialField, DriverAccess, DriverIsolation,
    DriverSchemaDepth, DriverSchemaScope, DriverSchemaSnapshot, DriverStreamPayload,
    ExecuteDriverRequest, ExecuteStart, HandleRequest, HandleResponse, OpenRequest, OpenResponse,
    PingRequest, PingResponse, Request, ResponseResult, WireId,
};
use sift_plugin_host::{SupervisedProcess, SupervisorError};
use sift_protocol::{
    Code, CursorId, DriverError, ExecuteRequest, ProviderDescriptor, ProviderId, SchemaDepth,
    SchemaScope, TxMode,
};
use tokio::sync::mpsc;

use crate::registry::{
    DatabaseProvider, ProviderConnectionHandle, ProviderOpenRequest, ProviderResultStream,
    ProviderServerInfo, ProviderTransactionHandle,
};

const DEFAULT_RPC_DEADLINE: Duration = Duration::from_secs(30);
const CORE_STREAM_BUFFER: usize = 8;

#[derive(Clone, Copy)]
struct RpcHandle {
    value: WireId,
    generation: WireId,
}

pub struct RpcProvider {
    descriptor: ProviderDescriptor,
    contribution_id: ContributionId,
    process: Arc<SupervisedProcess>,
    correlation_counter: AtomicU64,
    cursor_counter: AtomicU64,
    queries: Arc<std::sync::Mutex<HashMap<CursorId, WireId>>>,
}

impl RpcProvider {
    pub fn new(
        descriptor: ProviderDescriptor,
        contribution_id: ContributionId,
        process: Arc<SupervisedProcess>,
    ) -> Result<Self, DriverError> {
        if contribution_id.publisher() != descriptor.provider.provider_id.publisher() {
            return Err(DriverError::new(
                Code::InvalidParameterValue,
                "provider and contribution must belong to the same publisher",
            ));
        }
        Ok(Self {
            descriptor,
            contribution_id,
            process,
            correlation_counter: AtomicU64::new(1),
            cursor_counter: AtomicU64::new(1),
            queries: Arc::new(std::sync::Mutex::new(HashMap::new())),
        })
    }

    async fn unary<T, R>(&self, method: &str, payload: &T) -> Result<R, DriverError>
    where
        T: serde::Serialize + Sync,
        R: serde::de::DeserializeOwned,
    {
        let response = self
            .process
            .request(self.request(
                method,
                serde_json::to_value(payload).map_err(protocol_error)?,
            ))
            .await
            .map_err(supervisor_error)?;
        match response.result {
            ResponseResult::Ok { payload } => {
                serde_json::from_value(payload).map_err(protocol_error)
            }
            _ => Err(DriverError::new(
                Code::DriverInternal,
                "extension returned a non-unary response",
            )),
        }
    }

    async fn transaction_unary<T, R>(&self, method: &str, payload: &T) -> Result<R, DriverError>
    where
        T: serde::Serialize + Sync,
        R: serde::de::DeserializeOwned,
    {
        match self.unary(method, payload).await {
            Ok(response) => Ok(response),
            Err(_) => {
                let _ = self.process.shutdown("uncertain transaction outcome").await;
                Err(DriverError::new(Code::ConnectionInvalidated,
                    "provider transaction outcome is uncertain; generation stopped; reconnect required")
                    .with_provider(self.descriptor.provider.provider_id.clone()))
            }
        }
    }

    async fn savepoint_control(
        &self,
        method: &str,
        transaction: &ProviderTransactionHandle,
        name: &str,
    ) -> Result<(), DriverError> {
        if !self
            .descriptor
            .capabilities
            .iter()
            .any(|capability| capability.id == "driver.savepoints@1")
            || !self
                .process
                .method_family_version("driver")
                .is_some_and(|version| version >= 2)
        {
            return Err(DriverError::new(
                Code::UnsupportedForEngine,
                "provider generation does not support Driver RPC v2 savepoints",
            ));
        }
        if !sift_extension_protocol::valid_savepoint_name(name) {
            return Err(DriverError::new(
                Code::InvalidParameterValue,
                "savepoint name must contain 1..128 UTF-8 bytes without control characters",
            ));
        }
        let transaction = self.transaction(transaction)?;
        let request = sift_extension_protocol::DriverSavepointRequest {
            transaction: transaction.value,
            name: name.into(),
        };
        let response: Result<sift_extension_protocol::DriverSavepointAck, _> =
            self.unary(method, &request).await;
        match response {
            Ok(ack) if ack.transaction == request.transaction && ack.name == request.name => Ok(()),
            _ => {
                // A mutating response cannot prove the native outcome. Fence all
                // handles in this generation before allowing another operation.
                let _ = self.process.shutdown("uncertain savepoint outcome").await;
                Err(DriverError::new(Code::ConnectionInvalidated, "provider savepoint outcome is uncertain; generation stopped; reconnect required")
                    .with_provider(self.descriptor.provider.provider_id.clone()))
            }
        }
    }

    fn request(&self, method: &str, payload: serde_json::Value) -> Request {
        let correlation = self.correlation_counter.fetch_add(1, Ordering::Relaxed);
        Request {
            id: WireId::from_u128(0),
            contribution_id: self.contribution_id.clone(),
            method: method.into(),
            payload,
            correlation_id: WireId::from_u128(u128::from(correlation)),
            deadline_unix_ms: deadline_unix_ms(DEFAULT_RPC_DEADLINE),
            context: None,
            stream_id: None,
        }
    }

    fn connection(&self, handle: &ProviderConnectionHandle) -> Result<RpcHandle, DriverError> {
        self.validate_handle(
            handle.provider_id(),
            handle.downcast_ref::<RpcHandle>().copied(),
            Code::ConnectionInvalidated,
        )
    }

    fn transaction(&self, handle: &ProviderTransactionHandle) -> Result<RpcHandle, DriverError> {
        self.validate_handle(
            handle.provider_id(),
            handle.downcast_ref::<RpcHandle>().copied(),
            Code::TransactionNotFound,
        )
    }

    fn validate_handle(
        &self,
        provider_id: &ProviderId,
        handle: Option<RpcHandle>,
        code: Code,
    ) -> Result<RpcHandle, DriverError> {
        if provider_id != &self.descriptor.provider.provider_id {
            return Err(DriverError::new(code, "handle belongs to another provider"));
        }
        let handle = handle.ok_or_else(|| DriverError::new(code.clone(), "invalid handle kind"))?;
        if handle.generation != self.process.generation() {
            return Err(DriverError::new(
                code,
                "handle belongs to an obsolete process generation",
            ));
        }
        Ok(handle)
    }
}

#[async_trait::async_trait]
impl DatabaseProvider for RpcProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn legacy_engine(&self) -> Option<sift_protocol::Engine> {
        None
    }

    async fn open(
        &self,
        request: ProviderOpenRequest,
    ) -> Result<ProviderConnectionHandle, DriverError> {
        let credentials = request
            .credentials
            .into_iter()
            .map(|(name, value)| CredentialField { name, value })
            .collect();
        let response: OpenResponse = self
            .unary(
                "open",
                &OpenRequest {
                    configuration: request.configuration,
                    credentials,
                },
            )
            .await?;
        Ok(ProviderConnectionHandle::new(
            self.descriptor.provider.provider_id.clone(),
            RpcHandle {
                value: response.connection,
                generation: self.process.generation(),
            },
        ))
    }

    async fn ping(
        &self,
        connection: &ProviderConnectionHandle,
    ) -> Result<ProviderServerInfo, DriverError> {
        let connection = self.connection(connection)?;
        let response: PingResponse = self
            .unary(
                "ping",
                &PingRequest {
                    connection: connection.value,
                },
            )
            .await?;
        Ok(ProviderServerInfo {
            provider: self.descriptor.provider.clone(),
            server_version: response.server_version,
            current_database: response.current_database,
            current_user: response.current_user,
            pool_warm_slots: None,
        })
    }

    async fn estimated_plan(
        &self,
        connection: &ProviderConnectionHandle,
        request: ExecuteRequest,
        max_bytes: u32,
    ) -> Result<sift_protocol::ExplainResponse, DriverError> {
        if !self
            .descriptor
            .capabilities
            .iter()
            .any(|cap| cap.id == "driver.explain.estimated@1")
            || self.process.method_family_version("driver") != Some(3)
        {
            return Err(DriverError::new(
                Code::UnsupportedForEngine,
                "Estimated plans require negotiated Driver RPC v3 and driver.explain.estimated@1",
            ));
        }
        let connection = self.connection(connection)?;
        if request.sql.len() > 1024 * 1024
            || request.params.len() > 256
            || !(1..=1024 * 1024).contains(&max_bytes)
        {
            return Err(DriverError::new(
                Code::InvalidParameterValue,
                "Estimated-plan request exceeds its bounds",
            ));
        }
        if request.params.iter().any(|value| {
            matches!(value,sift_protocol::Value::Float32(n) if !n.is_finite())
                || matches!(value,sift_protocol::Value::Float64(n) if !n.is_finite())
        }) {
            return Err(DriverError::new(
                Code::InvalidParameterValue,
                "Nonfinite estimated-plan parameters cannot be encoded losslessly",
            ));
        }
        if request.params.iter().any(|value| match value {
            sift_protocol::Value::Interval(value) => value.num_microseconds().is_none(),
            sift_protocol::Value::Native { provider_id, .. } => {
                provider_id != &self.descriptor.provider.provider_id
            }
            _ => false,
        }) {
            return Err(DriverError::new(
                Code::InvalidParameterValue,
                "Estimated-plan parameter cannot retain its provider or interval representation",
            ));
        }
        let max_bytes =
            max_bytes.min(self.process.maximum_frame_bytes().saturating_sub(2048) as u32);
        if max_bytes == 0 {
            return Err(DriverError::new(
                Code::InvalidParameterValue,
                "Estimated-plan frame budget is too small",
            ));
        }
        let request = sift_extension_protocol::DriverEstimatedPlanRequest {
            connection: connection.value,
            sql: request.sql,
            params: request
                .params
                .into_iter()
                .map(crate::registry::driver_value)
                .collect(),
            max_bytes,
        };
        let envelope = self.request(
            "explain_estimated",
            serde_json::to_value(&request).map_err(protocol_error)?,
        );
        if serde_json::to_vec(&sift_extension_protocol::Message::Request(envelope.clone()))
            .map_err(protocol_error)?
            .len()
            > self.process.maximum_frame_bytes()
        {
            return Err(DriverError::new(
                Code::InvalidParameterValue,
                "Estimated-plan request exceeds negotiated frame budget",
            ));
        }
        let result: Result<sift_extension_protocol::DriverEstimatedPlanResponse, DriverError> =
            match self.process.request(envelope).await {
                Ok(response) => match response.result {
                    ResponseResult::Ok { payload } => {
                        serde_json::from_value(payload).map_err(protocol_error)
                    }
                    _ => Err(DriverError::new(
                        Code::DriverInternal,
                        "Estimated capture failed",
                    )),
                },
                Err(error) => Err(supervisor_error(error)),
            };
        let parsed = match result {
            Ok(response)
                if response.connection == connection.value
                    && response.disposition
                        == sift_extension_protocol::ConnectionDisposition::Reusable =>
            {
                match self.descriptor.provider.dialect_id.as_str() {
                    "sift/postgresql" => Ok(sift_protocol::Engine::Postgres),
                    "sift/tsql" => Ok(sift_protocol::Engine::SqlServer),
                    "sift/sqlite" => Ok(sift_protocol::Engine::Sqlite),
                    _ => Err(DriverError::new(
                        Code::UnsupportedForEngine,
                        "Unsupported estimated-plan dialect",
                    )),
                }
                .and_then(|engine| {
                    crate::plan::external_estimated_response(
                        engine,
                        response.plan,
                        max_bytes as usize,
                    )
                })
            }
            _ => Err(DriverError::new(
                Code::DriverInternal,
                "External estimated-plan capture failed",
            )),
        };
        if parsed.is_err() {
            let _ = self
                .process
                .shutdown("invalid or uncertain estimated-plan capture")
                .await;
            return Err(DriverError::new(Code::ConnectionInvalidated,"External estimated-plan capture failed or could not prove reusable state; generation stopped; reconnect required").with_provider(self.descriptor.provider.provider_id.clone()));
        }
        parsed
    }
    async fn schema(
        &self,
        connection: &ProviderConnectionHandle,
        scope: SchemaScope,
    ) -> Result<DriverSchemaSnapshot, DriverError> {
        let connection = self.connection(connection)?;
        self.unary(
            "schema",
            &sift_extension_protocol::SchemaRequest {
                connection: connection.value,
                scope: rpc_schema_scope(scope),
            },
        )
        .await
    }

    async fn begin(
        &self,
        connection: &ProviderConnectionHandle,
        mode: TxMode,
    ) -> Result<ProviderTransactionHandle, DriverError> {
        let connection = self.connection(connection)?;
        let response: HandleResponse = self
            .transaction_unary(
                "begin",
                &BeginRequest {
                    connection: connection.value,
                    isolation: match mode.isolation {
                        sift_protocol::IsolationLevel::ReadUncommitted => {
                            DriverIsolation::ReadUncommitted
                        }
                        sift_protocol::IsolationLevel::ReadCommitted => {
                            DriverIsolation::ReadCommitted
                        }
                        sift_protocol::IsolationLevel::RepeatableRead => {
                            DriverIsolation::RepeatableRead
                        }
                        sift_protocol::IsolationLevel::Snapshot => DriverIsolation::Snapshot,
                        sift_protocol::IsolationLevel::Serializable => {
                            DriverIsolation::Serializable
                        }
                    },
                    access: match mode.access {
                        sift_protocol::TxAccessMode::ReadWrite => DriverAccess::ReadWrite,
                        sift_protocol::TxAccessMode::ReadOnly => DriverAccess::ReadOnly,
                    },
                },
            )
            .await?;
        Ok(ProviderTransactionHandle::new(
            self.descriptor.provider.provider_id.clone(),
            RpcHandle {
                value: response.handle,
                generation: self.process.generation(),
            },
        ))
    }

    async fn commit(&self, transaction: ProviderTransactionHandle) -> Result<(), DriverError> {
        let transaction = self.transaction(&transaction)?;
        self.transaction_unary::<_, serde_json::Value>(
            "commit",
            &HandleRequest {
                handle: transaction.value,
            },
        )
        .await?;
        Ok(())
    }

    async fn rollback(&self, transaction: ProviderTransactionHandle) -> Result<(), DriverError> {
        let transaction = self.transaction(&transaction)?;
        self.transaction_unary::<_, serde_json::Value>(
            "rollback",
            &HandleRequest {
                handle: transaction.value,
            },
        )
        .await?;
        Ok(())
    }

    async fn savepoint(
        &self,
        transaction: &ProviderTransactionHandle,
        name: &str,
    ) -> Result<(), DriverError> {
        self.savepoint_control("savepoint", transaction, name).await
    }
    async fn rollback_to_savepoint(
        &self,
        transaction: &ProviderTransactionHandle,
        name: &str,
    ) -> Result<(), DriverError> {
        self.savepoint_control("rollback_to_savepoint", transaction, name)
            .await
    }
    async fn release_savepoint(
        &self,
        transaction: &ProviderTransactionHandle,
        name: &str,
    ) -> Result<(), DriverError> {
        self.savepoint_control("release_savepoint", transaction, name)
            .await
    }
    async fn execute(
        &self,
        connection: &ProviderConnectionHandle,
        request: ExecuteRequest,
    ) -> Result<ProviderResultStream, DriverError> {
        let connection = self.connection(connection)?;
        let request = ExecuteDriverRequest {
            connection: connection.value,
            sql: request.sql,
            params: request
                .params
                .into_iter()
                .map(crate::registry::driver_value)
                .collect(),
        };
        let (mut rpc_stream, start) = self
            .process
            .request_stream(self.request(
                "execute",
                serde_json::to_value(request).map_err(protocol_error)?,
            ))
            .await
            .map_err(supervisor_error)?;
        let start: ExecuteStart = serde_json::from_value(start).map_err(protocol_error)?;
        let cursor_id = CursorId::new(self.cursor_counter.fetch_add(1, Ordering::Relaxed));
        self.queries
            .lock()
            .expect("query map poisoned")
            .insert(cursor_id, start.query);
        let queries = self.queries.clone();
        let (sender, receiver) = mpsc::channel(CORE_STREAM_BUFFER);
        tokio::spawn(async move {
            while let Some(frame) = rpc_stream.next().await {
                let payload = match serde_json::from_value::<DriverStreamPayload>(
                    frame.frame.payload.clone(),
                ) {
                    Ok(payload) => payload,
                    Err(_) => break,
                };
                let terminal = matches!(
                    payload,
                    DriverStreamPayload::Done { .. } | DriverStreamPayload::Error { .. }
                );
                if sender.send(payload).await.is_err() || rpc_stream.accept(&frame).await.is_err() {
                    break;
                }
                if terminal {
                    break;
                }
            }
            queries
                .lock()
                .expect("query map poisoned")
                .remove(&cursor_id);
        });
        Ok(ProviderResultStream {
            cursor_id,
            rows: receiver,
            server_side_cursor: true,
        })
    }

    async fn cancel(
        &self,
        connection: &ProviderConnectionHandle,
        cursor: CursorId,
    ) -> Result<(), DriverError> {
        let connection = self.connection(connection)?;
        let query = self
            .queries
            .lock()
            .expect("query map poisoned")
            .get(&cursor)
            .copied()
            .ok_or_else(|| DriverError::new(Code::CursorNotFound, "query cursor not found"))?;
        self.unary::<_, serde_json::Value>(
            "cancel",
            &CancelRequest {
                connection: connection.value,
                query,
            },
        )
        .await?;
        Ok(())
    }

    async fn close(&self, connection: ProviderConnectionHandle) -> Result<(), DriverError> {
        let connection = self.connection(&connection)?;
        self.unary::<_, serde_json::Value>(
            "close",
            &HandleRequest {
                handle: connection.value,
            },
        )
        .await?;
        Ok(())
    }
}

fn rpc_schema_scope(scope: SchemaScope) -> DriverSchemaScope {
    match scope.depth {
        SchemaDepth::Shallow => DriverSchemaScope {
            depth: DriverSchemaDepth::Shallow,
            catalog: scope
                .filter
                .as_ref()
                .and_then(|filter| filter.catalogs.as_ref())
                .and_then(|catalogs| catalogs.first())
                .cloned(),
            namespace: scope
                .filter
                .as_ref()
                .and_then(|filter| filter.schemas.as_ref())
                .and_then(|schemas| schemas.first())
                .cloned(),
            object: scope.filter.and_then(|filter| filter.name_pattern),
            namespaces: Vec::new(),
            kinds: Vec::new(),
            include_definitions: false,
            max_nodes: None,
        },
        SchemaDepth::Deep { object } => DriverSchemaScope {
            depth: DriverSchemaDepth::Deep,
            catalog: object.catalog,
            namespace: object.schema,
            object: Some(object.name),
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
            namespaces: options.schemas.unwrap_or_default(),
            kinds: options
                .kinds
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
    }
}

fn deadline_unix_ms(duration: Duration) -> i64 {
    let millis = SystemTime::now()
        .checked_add(duration)
        .and_then(|deadline| deadline.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis())
        .unwrap_or(i64::MAX as u128);
    i64::try_from(millis).unwrap_or(i64::MAX)
}

fn protocol_error(error: impl std::fmt::Display) -> DriverError {
    DriverError::new(
        Code::DriverInternal,
        format!("invalid extension RPC payload: {error}"),
    )
}

fn supervisor_error(error: SupervisorError) -> DriverError {
    match error {
        SupervisorError::RequestTimeout => {
            DriverError::new(Code::QueryTimedOut, "extension request timed out")
        }
        SupervisorError::Remote(error) => {
            let code = serde_json::from_value::<Code>(serde_json::json!({"code": error.code}))
                .unwrap_or(Code::DriverInternal);
            let mut mapped = DriverError::new(code, error.message);
            mapped.native_code = error.native_code;
            mapped
        }
        SupervisorError::ProcessStopped => DriverError::new(
            Code::ConnectionInvalidated,
            "extension process generation stopped",
        ),
        other => DriverError::new(Code::DriverInternal, other.to_string()),
    }
}
