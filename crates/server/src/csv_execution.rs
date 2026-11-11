//! Request-local CSV native source and bounded phase observations.
use super::*;
use sift_protocol::ChangeLedgerOutcome;

#[derive(Clone)]
pub(crate) struct CsvPhase {
    pub attempted: bool,
    pub rows: Option<u64>,
    pub outcome: ChangeLedgerOutcome,
    pub transaction_id: Option<TxId>,
    pub object: Option<String>,
    committed_rows: Option<u64>,
    pending: bool,
}
impl Default for CsvPhase {
    fn default() -> Self {
        Self {
            attempted: false,
            rows: None,
            outcome: ChangeLedgerOutcome::OutcomeUnknown,
            transaction_id: None,
            object: None,
            committed_rows: Some(0),
            pending: false,
        }
    }
}
#[derive(Clone, Default)]
pub(crate) struct CsvObservation {
    pub data: CsvPhase,
    pub ddl: CsvPhase,
    pub transaction_id: Option<TxId>,
    pub import_completed: bool,
    in_transaction: bool,
}
pub(crate) struct CsvExecution {
    session: SessionId,
    connection: ConnectionId,
    pub source: ConnectionEntryClone,
    observation: Mutex<CsvObservation>,
}
impl CsvExecution {
    pub fn matches(&self, session: SessionId, connection: ConnectionId) -> bool {
        self.session == session && self.connection == connection
    }
    pub fn observation(&self) -> CsvObservation {
        self.observation.lock().unwrap().clone()
    }
    pub fn attempt(&self, ddl: bool) {
        let mut observation = self.observation.lock().unwrap();
        let pending = observation.in_transaction;
        let transaction_id = pending.then_some(observation.transaction_id).flatten();
        let phase = if ddl {
            &mut observation.ddl
        } else {
            &mut observation.data
        };
        phase.attempted = true;
        phase.transaction_id = transaction_id;
        phase.pending = pending;
        phase.rows = None;
        phase.outcome = ChangeLedgerOutcome::OutcomeUnknown;
    }
    pub fn autocommit(&self, ddl: bool, rows: u64) {
        let mut observation = self.observation.lock().unwrap();
        let phase = if ddl {
            &mut observation.ddl
        } else {
            &mut observation.data
        };
        phase.committed_rows = phase.committed_rows.and_then(|sum| sum.checked_add(rows));
        phase.rows = phase.committed_rows;
        phase.outcome = ChangeLedgerOutcome::Committed;
    }
    pub fn transaction(&self, tx: TxId) {
        let mut o = self.observation.lock().unwrap();
        o.transaction_id = Some(tx);
        o.in_transaction = true;
    }
    pub fn committed(&self, rows: u64) {
        let mut o = self.observation.lock().unwrap();
        if o.data.pending {
            o.data.committed_rows = o.data.committed_rows.and_then(|sum| sum.checked_add(rows));
        }
        {
            let phase = &mut o.data;
            if phase.pending {
                phase.rows = phase.committed_rows;
                phase.outcome = ChangeLedgerOutcome::Committed;
                phase.pending = false;
            }
        }
        if o.ddl.pending {
            o.ddl.outcome = ChangeLedgerOutcome::Committed;
            o.ddl.pending = false;
        }
        o.in_transaction = false;
    }
    pub fn rollback(&self, confirmed: bool) {
        let mut o = self.observation.lock().unwrap();
        {
            let phase = &mut o.data;
            if phase.pending {
                phase.outcome = if confirmed {
                    if phase.committed_rows != Some(0) {
                        ChangeLedgerOutcome::Partial
                    } else {
                        ChangeLedgerOutcome::RolledBack
                    }
                } else {
                    ChangeLedgerOutcome::OutcomeUnknown
                };
                phase.rows = None;
                phase.pending = false;
            }
        }
        if o.ddl.pending {
            o.ddl.outcome = if confirmed {
                ChangeLedgerOutcome::RolledBack
            } else {
                ChangeLedgerOutcome::OutcomeUnknown
            };
            o.ddl.pending = false;
        }
        o.in_transaction = false;
    }
    pub fn fence(&self) {
        self.source.execution.fence();
    }
    pub fn is_fenced(&self) -> bool {
        self.source.execution.is_fenced()
    }
}
impl SessionStore {
    pub(crate) fn bind_source(
        &self,
        session: SessionId,
        connection: ConnectionId,
        source: ConnectionEntryClone,
    ) -> ApiResult<Self> {
        Self::captured_execution_entry(
            self.get_conn_entry(session, connection)?,
            Some(source.clone()),
        )?;
        if self
            .csv_source
            .as_ref()
            .is_some_and(|bound| bound.matches(session, connection))
        {
            return Ok(self.clone());
        }
        Ok(Self {
            inner: self.inner.clone(),
            source_authority: self.source_authority.clone(),
            ai_review_owner: self.ai_review_owner.clone(),
            csv_statement: None,
            csv_source: Some(Arc::new(CsvExecution {
                session,
                connection,
                source,
                observation: Default::default(),
            })),
        })
    }
    pub(crate) fn with_source_authority(&self, authority: SourceAuthority) -> Self {
        let mut bound = self.clone();
        bound.source_authority = Some(authority);
        bound
    }
    pub(crate) fn csv_import_completed(&self) {
        if let Some(source) = &self.csv_source {
            source.observation.lock().unwrap().import_completed = true;
        }
    }
    pub(crate) fn csv_ddl_target(&self, object: String) {
        if let Some(source) = &self.csv_source {
            source.observation.lock().unwrap().ddl.object = Some(object);
        }
    }
    pub(crate) fn csv_admission(
        &self,
        session: SessionId,
        connection: ConnectionId,
    ) -> ApiResult<()> {
        self.reject_if_connection_has_tx(session, connection, None)
    }
    pub(crate) async fn csv_execute(
        &self,
        session: SessionId,
        request: ExecuteRequestHttp,
        ddl: bool,
    ) -> ApiResult<ExecuteResponse> {
        self.csv_execute_as(
            session,
            request,
            ddl,
            sift_protocol::OperationKind::ImportCsv,
        )
        .await
    }
    pub(crate) async fn csv_execute_as(
        &self,
        session: SessionId,
        request: ExecuteRequestHttp,
        ddl: bool,
        operation: sift_protocol::OperationKind,
    ) -> ApiResult<ExecuteResponse> {
        let gate = Arc::new(bulk_execution::BulkExecution::new(
            self.get_conn_entry(session, request.connection)?,
        ));
        let mut worker = self.clone();
        worker.csv_statement = Some(gate.clone());
        let result = worker.execute_http_as(session, request, operation).await;
        if gate.close_dispatch() {
            self.csv_attempt(ddl);
        }
        result
    }
    pub(crate) fn csv_execution(&self) -> Option<Arc<CsvExecution>> {
        self.csv_source.clone()
    }
    pub(super) fn csv_captured_entry(
        &self,
        session: SessionId,
        connection: ConnectionId,
        current: ConnectionEntryClone,
    ) -> ApiResult<ConnectionEntryClone> {
        match &self.csv_source {
            Some(source) if source.matches(session, connection) => {
                Self::captured_execution_entry(current, Some(source.source.clone()))
            }
            _ => Ok(current),
        }
    }
    pub(crate) fn csv_attempt(&self, ddl: bool) {
        if let Some(source) = &self.csv_source {
            source.attempt(ddl);
        }
    }
    pub(crate) fn csv_autocommit(&self, ddl: bool, rows: u64) {
        if let Some(source) = &self.csv_source {
            source.autocommit(ddl, rows);
        }
    }
    pub(crate) fn csv_transaction(&self, tx: TxId) {
        if let Some(source) = &self.csv_source {
            source.transaction(tx);
        }
    }
    pub(crate) fn csv_committed(&self, rows: u64) {
        if let Some(source) = &self.csv_source {
            source.committed(rows);
        }
    }
    pub(crate) async fn csv_rollback(
        &self,
        session: SessionId,
        end: EndTransactionRequest,
    ) -> ApiResult<()> {
        self.csv_rollback_as(session, end, sift_protocol::OperationKind::ImportCsv)
            .await
    }
    pub(crate) async fn csv_rollback_as(
        &self,
        session: SessionId,
        end: EndTransactionRequest,
        operation: sift_protocol::OperationKind,
    ) -> ApiResult<()> {
        let connection = end.connection;
        let actual = self.get_conn_entry(session, connection).ok();
        let mut cleanup = self.clone();
        cleanup.source_authority = None;
        let confirmed = cleanup
            .rollback_transaction_as(session, end, operation)
            .await
            .is_ok();
        if let Some(source) = &self.csv_source {
            source.rollback(confirmed);
        }
        if confirmed {
            return Ok(());
        }
        if let Some(actual) = actual {
            actual.execution.fence();
        }
        let original = self
            .csv_source
            .as_ref()
            .is_some_and(|source| source.matches(session, connection));
        if original {
            if let Some(source) = &self.csv_source {
                source.fence();
            }
        }
        Err(ApiError::Driver(DriverError::new(
            if original {
                Code::ConnectionInvalidated
            } else {
                Code::ImportOutcomeUnknown
            },
            "CSV rollback could not be confirmed; inspect the database before retrying",
        )))
    }
    pub(crate) fn csv_count(&self, count: Option<u64>) -> ApiResult<u64> {
        count.ok_or_else(|| {
            if let Some(source) = &self.csv_source {
                if !source.observation.lock().unwrap().in_transaction {
                    source.fence();
                }
            }
            ApiError::Driver(DriverError::new(
                Code::UnsupportedResultShape,
                "CSV import requires an observed affected count",
            ))
        })
    }
}
