//! Captured bulk dispatch with invocation-private terminal observations.
use super::*;

#[derive(Default, Clone)]
pub(crate) struct BulkObservation {
    dispatch_closed: bool,
    pub attempted: bool,
    pub transaction_id: Option<TxId>,
    pub rollback: Option<bool>,
}

#[derive(Clone)]
pub(crate) struct BulkExecution {
    pub source: ConnectionEntryClone,
    observation: Arc<Mutex<BulkObservation>>,
}
impl BulkExecution {
    pub fn new(source: ConnectionEntryClone) -> Self {
        Self {
            source,
            observation: Default::default(),
        }
    }
    pub fn is_fenced(&self) -> bool {
        self.source.execution.is_fenced()
    }
    pub fn observation(&self) -> BulkObservation {
        self.observation.lock().unwrap().clone()
    }
    pub fn dispatch(&self) -> ApiResult<()> {
        let mut observation = self.observation.lock().unwrap();
        if observation.dispatch_closed {
            return Err(ApiError::Driver(DriverError::new(
                Code::QueryTimedOut,
                "bulk dispatch admission ended before native execution",
            )));
        }
        observation.attempted = true;
        Ok(())
    }
    pub(crate) fn close_dispatch(&self) -> bool {
        let mut observation = self.observation.lock().unwrap();
        observation.dispatch_closed = true;
        observation.attempted
    }
    pub fn transaction(&self, tx: TxId) {
        self.observation.lock().unwrap().transaction_id = Some(tx);
    }
    pub fn rollback(&self, confirmed: bool) {
        self.observation.lock().unwrap().rollback = Some(confirmed);
        if !confirmed {
            self.source.execution.fence();
        }
    }
}

pub(crate) enum NativeTransfer {
    Bulk(BulkOp),
    Copy(sift_driver_api::CopyOp),
}

impl SessionStore {
    pub async fn bulk_insert(
        &self,
        session: SessionId,
        connection: ConnectionId,
        request: BulkInsertRequest,
    ) -> ApiResult<BulkInsertResponse> {
        let path = object_path_from_qualified_name(&request.table)?;
        let source = self.authorize_connection_operation(
            session,
            connection,
            sift_protocol::OperationKind::BulkInsert,
            None,
            &[&path],
        )?;
        self.bulk_insert_captured(session, connection, request, BulkExecution::new(source))
            .await
    }

    pub(crate) async fn bulk_insert_captured(
        &self,
        session: SessionId,
        connection: ConnectionId,
        request: BulkInsertRequest,
        context: BulkExecution,
    ) -> ApiResult<BulkInsertResponse> {
        let path = object_path_from_qualified_name(&request.table)?;
        let entry = self.authorize_connection_operation(
            session,
            connection,
            sift_protocol::OperationKind::BulkInsert,
            None,
            &[&path],
        )?;
        let entry = Self::captured_execution_entry(entry, Some(context.source.clone()))?;
        if entry.driver.engine() == Engine::Sqlite {
            return crate::sqlite_bulk::run(self, session, connection, request, context).await;
        }
        if request.native.is_some()
            || request.preview
            || request.preview_token.is_some()
            || request.confirm_write
        {
            return Err(ApiError::BadRequest(
                "SQLite native bulk fields are unsupported for this provider".into(),
            ));
        }
        if request.format == BulkInsertFormat::Native {
            return Err(ApiError::Driver(DriverError::new(Code::UnsupportedForEngine,"SQL Server native bulk format needs typed rows and is not part of the locked driver trait").with_engine(Engine::SqlServer)));
        }
        if entry.driver.as_mssql().is_none() {
            return Err(ApiError::Driver(
                DriverError::new(
                    Code::UnsupportedForEngine,
                    "bulk insert is only supported by SQL Server connections",
                )
                .with_engine(entry.driver.engine()),
            ));
        }
        let rows = self
            .native_transfer(
                session,
                connection,
                sift_protocol::OperationKind::BulkInsert,
                context,
                NativeTransfer::Bulk(BulkOp {
                    table: request.table,
                    data: request.data,
                    delimiter: b',',
                    header: true,
                    null_value: None,
                }),
                None,
            )
            .await?
            .ok_or_else(|| ApiError::Internal("bulk count unavailable".into()))?;
        Ok(BulkInsertResponse {
            rows_inserted: rows,
            rows_validated: rows,
            preview_token: None,
            target_affinities: vec![],
        })
    }

    pub(crate) async fn native_transfer(
        &self,
        session: SessionId,
        connection: ConnectionId,
        operation: sift_protocol::OperationKind,
        context: BulkExecution,
        transfer: NativeTransfer,
        tx: Option<TxHandleRef>,
    ) -> ApiResult<Option<u64>> {
        let table = match &transfer {
            NativeTransfer::Bulk(op) => &op.table,
            NativeTransfer::Copy(sift_driver_api::CopyOp::Import { table, .. }) => table,
            _ => {
                return Err(ApiError::BadRequest(
                    "native transfer requires import".into(),
                ))
            }
        };
        let path = object_path_from_qualified_name(table)?;
        let entry =
            self.authorize_connection_operation(session, connection, operation, None, &[&path])?;
        let entry = Self::captured_execution_entry(entry, Some(context.source.clone()))?;
        let supported = match &transfer {
            NativeTransfer::Bulk(_) => entry.driver.as_mssql().is_some(),
            NativeTransfer::Copy(_) => entry.driver.as_pg().is_some(),
        };
        if !supported {
            return Err(ApiError::Driver(DriverError::new(
                Code::UnsupportedForEngine,
                "native transfer extension unavailable",
            )));
        }
        let store = self.clone();
        let deadline = self.admission_deadline();
        // Supervision owns admission + native completion even when its observer times out.
        let query_guard = crate::shutdown::current_query_guard();
        let observed = context.clone();
        let owned = tx.is_some();
        let native = async move {
            let _query_guard = query_guard;
            let _gate =
                tokio::time::timeout_at(deadline, entry.execution.gate.clone().write_owned())
                    .await
                    .map_err(|_| Self::admission_expired())?;
            Self::check_admission_deadline(deadline)?;
            let current = store.authorize_connection_operation(
                session,
                connection,
                operation,
                None,
                &[&path],
            )?;
            Self::captured_execution_entry(current, Some(entry.clone()))?;
            store.validate_execute_tx(session, connection, tx.as_ref())?;
            let _resources = store.reserve_query_resources(&entry)?;
            let handle = entry
                .handle
                .builtin()
                .cloned()
                .ok_or_else(native_provider_only)?;
            context.dispatch()?;
            let mut mutation = execution_control::MutationLease::new(entry.execution.clone());
            let result = match transfer {
                NativeTransfer::Bulk(op) => entry
                    .driver
                    .as_mssql()
                    .ok_or_else(native_provider_only)?
                    .bulk_insert(handle, op)
                    .await
                    .map(|result| Some(result.rows_inserted)),
                NativeTransfer::Copy(op) => entry
                    .driver
                    .as_pg()
                    .ok_or_else(native_provider_only)?
                    .copy(handle, op)
                    .await
                    .map(|result| result.rows),
            };
            if let (Err(_), Some(tx)) = (&result, &tx) {
                store.mark_transaction_failed(session, tx.tx_id);
            }
            if result.is_ok() || owned {
                mutation.settle();
            }
            result.map_err(ApiError::Driver)
        };
        let result = self.transaction_work("bulk insert", native).await;
        let attempted = observed.close_dispatch();
        let timed_out = matches!(&result,Err(ApiError::Driver(error)) if error.code==Code::QueryTimedOut)
            || matches!(&result, Err(ApiError::Internal(_)));
        if result.is_err() && attempted && (!owned || timed_out) {
            observed.source.execution.fence();
        }
        result
    }

    pub(crate) async fn bulk_schema(
        &self,
        session: SessionId,
        connection: ConnectionId,
        path: sift_protocol::ObjectPath,
        context: &BulkExecution,
    ) -> ApiResult<sift_protocol::SchemaSnapshot> {
        let entry = self.authorize_connection_operation(
            session,
            connection,
            sift_protocol::OperationKind::BulkInsert,
            None,
            &[&path],
        )?;
        let entry = Self::captured_execution_entry(entry, Some(context.source.clone()))?;
        self.run_bounded("bulk target schema", async move {
            entry
                .driver
                .schema(entry.handle, sift_protocol::SchemaScope::deep(path))
                .await
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn bulk_timeout_closes_native_dispatch_admission_without_losing_an_attempt() {
        let store = SessionStore::new(
            DriverRegistry::builder()
                .register(sift_driver_api::mock::MockDriver::builder().build())
                .build(),
        );
        let session = store
            .open_session(OpenSessionRequest {
                tag: None,
                tenant_id: None,
            })
            .id;
        let connection = store
            .open_connection(
                session,
                Engine::Postgres,
                ConnectionSpec {
                    host: "mock.invalid".into(),
                    port: None,
                    database: Some("mock".into()),
                    user: "mock".into(),
                    password: None,
                    ssl_mode: None,
                    engine_specific: None,
                },
            )
            .await
            .unwrap()
            .id;
        let source = store.conn_entry(session, connection).unwrap();
        // Timeout wins admission: a late worker cannot enter native mutation.
        let closed = BulkExecution::new(source.clone());
        assert!(!closed.close_dispatch());
        assert!(closed.dispatch().is_err());
        assert!(!closed.observation().attempted);
        // Native dispatch wins admission: timeout must retain the attempted write.
        let entered = BulkExecution::new(source);
        entered.dispatch().unwrap();
        assert!(entered.close_dispatch());
        assert!(entered.observation().attempted);
    }
}
