//! Owned, reviewed SQL Server writes; normal CSV and Parquet paths are unchanged.
use super::*;
use sift_protocol::{
    CsvImportResponse, ImportProgress, ImportStage, InferredCsvColumn, OperationKind,
    SqlServerComputedMode, SqlServerIdentityMode, SqlServerImportRequest, SqlServerImportReview,
    Value,
};
use tokio_util::sync::CancellationToken;
#[path = "sqlserver_import/metadata.rs"]
mod metadata;
fn bad(message: &str) -> ApiError {
    ApiError::BadRequest(message.into())
}
fn cancelled() -> ApiError {
    DriverError::new(
        Code::QueryCanceled,
        "Import cancelled before confirmed commit; cleanup may still be pending",
    )
    .into()
}
fn uncertain() -> ApiError {
    DriverError::new(
        Code::ImportOutcomeUnknown,
        "Import COMMIT outcome is unknown; inspect the target before retrying",
    )
    .into()
}

pub(crate) struct Input {
    pub table: String,
    pub columns: Vec<InferredCsvColumn>,
    pub rows: Vec<Row>,
    pub digest: String,
    pub typed: bool,
    pub dry_run: bool,
}
pub(super) struct ActiveImport {
    pub(super) run_id: uuid::Uuid,
    pub(super) cancellation: CancellationToken,
    progress: Mutex<ImportProgress>,
    cursor: Mutex<Option<(RuntimeDriver, RuntimeConnectionHandle, CursorId)>>,
    _budget: tokio::sync::OwnedSemaphorePermit,
}
struct Registration {
    store: SessionStore,
    session: SessionId,
    source: ConnectionId,
}
impl Drop for Registration {
    fn drop(&mut self) {
        self.store
            .inner
            .imports
            .remove(&(self.session, self.source));
    }
}
struct CancelOnDrop(CancellationToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
struct Owned {
    store: SessionStore,
    session: SessionId,
    connection: Option<ConnectionId>,
    active: Arc<ActiveImport>,
}
impl Owned {
    fn start_close(&mut self) -> Option<tokio::task::JoinHandle<ApiResult<()>>> {
        let connection = self.connection.take()?;
        let runtime = tokio::runtime::Handle::try_current().ok()?;
        self.active.progress.lock().unwrap().stage = ImportStage::CleaningUp;
        let active = self.active.clone();
        let store = self.store.clone();
        let session = self.session;
        let query_guard = crate::shutdown::current_query_guard();
        Some(
            runtime.spawn(crate::shutdown::scope_query(query_guard, async move {
                let cursor = active.cursor.lock().unwrap().take();
                if let Some((driver, handle, cursor)) = cursor {
                    let _ = driver.cancel(handle, cursor).await;
                }
                let _retained = active;
                store.close_connection_unchecked(session, connection).await
            })),
        )
    }
    async fn close(&mut self) -> bool {
        let Some(mut task) = self.start_close() else {
            return false;
        };
        !matches!(
            tokio::time::timeout(Duration::from_secs(5), &mut task).await,
            Ok(Ok(Ok(())))
        )
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        // The cleanup task captures plain ownership, never another Owned guard:
        // runtime shutdown may discard it before its first poll.
        drop(self.start_close());
    }
}
pub(super) struct Context {
    store: SessionStore,
    session: SessionId,
    source: ConnectionId,
    connection: ConnectionId,
    target: sift_protocol::ObjectPath,
    active: Arc<ActiveImport>,
}
impl Context {
    fn authorize(&self) -> ApiResult<()> {
        if self.active.cancellation.is_cancelled() {
            return Err(cancelled());
        }
        self.store.authorize_connection_operation(
            self.session,
            self.source,
            OperationKind::ImportCsv,
            None,
            &[&self.target],
        )?;
        Ok(())
    }
    pub(super) async fn query(&self, sql: &str, params: Vec<Value>) -> ApiResult<Vec<Row>> {
        self.query_mode(sql, params, false).await
    }
    async fn write(&self, sql: &str, params: Vec<Value>) -> ApiResult<Vec<Row>> {
        self.query_mode(sql, params, true).await
    }
    async fn query_mode(
        &self,
        sql: &str,
        params: Vec<Value>,
        mutation: bool,
    ) -> ApiResult<Vec<Row>> {
        self.authorize()?;
        let entry = self.store.conn_entry(self.session, self.connection)?;
        let gate = super::bulk_execution::BulkExecution::new(entry.clone());
        let dispatch = gate.clone();
        let resources = self.store.reserve_query_resources(&entry)?;
        let driver = entry.driver.clone();
        let handle = entry.handle.clone();
        let active = self.active.clone();
        let sql = sql.to_owned();
        let result = self
            .store
            .run_bounded("reviewed SQL Server import", async move {
                let _resources = resources;
                let _active = active.clone();
                dispatch.dispatch().map_err(|_| {
                    DriverError::new(
                        Code::QueryTimedOut,
                        "reviewed import dispatch ended before native execution",
                    )
                })?;
                let mut stream = driver
                    .execute(
                        handle.clone(),
                        ExecuteRequest {
                            sql,
                            params,
                            transform: None,
                        },
                    )
                    .await?;
                let cursor = stream.cursor_id;
                *active.cursor.lock().unwrap() = Some((driver.clone(), handle.clone(), cursor));
                if active.cancellation.is_cancelled() {
                    let _ = driver.cancel(handle, cursor).await;
                    return Err(DriverError::new(
                        Code::QueryCanceled,
                        "Import cancellation requested",
                    ));
                }
                let mut rows = Vec::new();
                let mut bytes = 0usize;
                let mut done = false;
                while let Some(page) = stream.rows.recv().await {
                    match page {
                        Page::Rows { rows: batch } => {
                            for row in batch {
                                bytes = bytes.saturating_add(
                                    serde_json::to_vec(&row)
                                        .map_err(|_| {
                                            DriverError::new(
                                                Code::UnsupportedResultShape,
                                                "Import catalog encoding failed",
                                            )
                                        })?
                                        .len(),
                                );
                                if bytes > 8 * 1024 * 1024 || rows.len() >= 4096 {
                                    return Err(DriverError::new(
                                        Code::ResultTooLarge,
                                        "Import catalog result exceeds its bound",
                                    ));
                                }
                                rows.push(row);
                            }
                        }
                        Page::Error { error } => return Err(error),
                        Page::Done { .. } => done = true,
                        Page::NextResult { .. } => {}
                    }
                }
                active.cursor.lock().unwrap().take();
                if !done {
                    return Err(DriverError::new(
                        Code::DriverInternal,
                        "Import statement ended without completion",
                    ));
                }
                Ok(rows)
            })
            .await;
        if gate.close_dispatch() && mutation {
            self.store.csv_attempt(false);
        }
        result.map_err(|error| match error {
            ApiError::Driver(error) => {
                let native = error
                    .native_code
                    .as_deref()
                    .filter(|code| code.len() <= 12 && code.bytes().all(|b| b.is_ascii_digit()));
                let message = native.map_or_else(
                    || "Reviewed import statement failed; native row values withheld".to_string(),
                    |code| format!("Reviewed import native error {code}; row values withheld"),
                );
                let mut safe = DriverError::new(error.code, message);
                safe.native_code = native.map(str::to_owned);
                ApiError::Driver(safe)
            }
            other => other,
        })
    }
}
impl SessionStore {
    pub fn import_progress(
        &self,
        session: SessionId,
        source: ConnectionId,
        run: uuid::Uuid,
    ) -> ApiResult<Option<ImportProgress>> {
        self.authorize_connection_operation(
            session,
            source,
            OperationKind::ReadImportProgress,
            None,
            &[],
        )?;
        if self.conn_entry(session, source)?.driver.semantic_engine() != Some(Engine::SqlServer) {
            return Err(bad("Reviewed import progress requires SQL Server"));
        }
        Ok(self
            .inner
            .imports
            .get(&(session, source))
            .filter(|a| a.run_id == run)
            .map(|a| {
                let mut p = a.progress.lock().unwrap().clone();
                p.cancellation_requested = a.cancellation.is_cancelled();
                p
            }))
    }
    pub fn cancel_import(
        &self,
        session: SessionId,
        source: ConnectionId,
        run: uuid::Uuid,
    ) -> ApiResult<bool> {
        self.authorize_connection_operation(
            session,
            source,
            OperationKind::CancelImport,
            None,
            &[],
        )?;
        if self.conn_entry(session, source)?.driver.semantic_engine() != Some(Engine::SqlServer) {
            return Err(bad("Reviewed import cancellation requires SQL Server"));
        }
        let Some(active) = self
            .inner
            .imports
            .get(&(session, source))
            .filter(|a| a.run_id == run)
        else {
            return Ok(false);
        };
        active.cancellation.cancel();
        Ok(true)
    }
    pub(crate) async fn reviewed_sqlserver_import(
        &self,
        session: SessionId,
        source: ConnectionId,
        input: Input,
        control: SqlServerImportRequest,
    ) -> ApiResult<CsvImportResponse> {
        let target = crate::csv_import::table_path(&input.table)?;
        let source_entry = self.authorize_connection_operation(
            session,
            source,
            OperationKind::ImportCsv,
            None,
            &[&target],
        )?;
        let captured = self.bind_csv_source(session, source, source_entry)?;
        if captured
            .conn_entry(session, source)?
            .driver
            .semantic_engine()
            != Some(Engine::SqlServer)
        {
            return Err(bad("Generated-column modes require SQL Server"));
        }
        if control.run_id.is_nil() {
            return Err(bad("Import run identity cannot be nil"));
        }
        let budget = captured
            .inner
            .import_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| bad("Four reviewed imports or their cleanup tasks are already active"))?;
        let active = Arc::new(ActiveImport {
            run_id: control.run_id,
            cancellation: CancellationToken::new(),
            progress: Mutex::new(ImportProgress {
                run_id: control.run_id,
                stage: ImportStage::Connecting,
                rows_total: input.rows.len() as u64,
                rows_attempted: 0,
                rows_committed: 0,
                cancellation_requested: false,
            }),
            cursor: Mutex::new(None),
            _budget: budget,
        });
        match captured.inner.imports.entry((session, source)) {
            dashmap::mapref::entry::Entry::Vacant(slot) => {
                slot.insert(active.clone());
            }
            _ => return Err(bad("An import is already active on this source")),
        }
        let _cancel = CancelOnDrop(active.cancellation.clone());
        let store = captured.clone();
        let query_guard = crate::shutdown::current_query_guard();
        tokio::spawn(crate::shutdown::scope_query(query_guard,async move {
            let _registration = Registration {store: store.clone(), session, source};
            let expected=store.conn_entry(session,source)?;
            let (configuration,credentials,provenance,driver)=store.with_session(&session,|s| {
                let entry=s.connections.get(&source).ok_or(ApiError::ConnectionNotFound(source))?;
                if !Arc::ptr_eq(&entry.execution,&expected.execution) {return Err(ApiError::Driver(DriverError::new(Code::ConnectionInvalidated,"CSV source was replaced before owned connection startup")));}
                Ok((expected.configuration.clone(),entry.credentials.clone(),expected.provenance.clone(),expected.driver.clone()))
            })??;
            if !super::inspection_connection::inspection_startup_supported(&configuration) {
                return Err(bad("Reviewed imports cannot replay arbitrary startup SQL"));
            }
            let resource = match &provenance {
                ConnectionProvenance::Managed {tenant_id, quota_exempt: false, ..} => Some(store.resource_manager().reserve(*tenant_id, sift_protocol::TenantResource::Connections, 1)?),
                _ => None,
            };
            let dedicated = store.open_connection_with_provenance(session, Some(Engine::SqlServer), configuration, credentials, provenance, resource, driver).await?;
            let mut owned = Owned {store: store.clone(), session, connection: Some(dedicated.id), active: active.clone()};
            let ctx = Context {store: store.clone(), session, source, connection: dedicated.id, target, active: active.clone()};
            let result = tokio::select! {
                biased;
                _ = active.cancellation.cancelled() => Err(if active.progress.lock().unwrap().stage == ImportStage::Committing {uncertain()} else {cancelled()}),
                result = tokio::time::timeout(Duration::from_secs(300), execute(&ctx, input, control)) => match result {
                    Ok(result) => result,
                    Err(_) => Err(if active.progress.lock().unwrap().stage == ImportStage::Committing {uncertain()} else {DriverError::new(Code::QueryTimedOut, "Import budget expired; cleanup may remain pending").into()}),
                },
            };
            if result.is_err() {
                if let Some(source)=store.csv_execution() {
                    let data=source.observation().data;
                    if data.attempted && data.outcome==sift_protocol::ChangeLedgerOutcome::OutcomeUnknown {
                        if let Ok(actual)=store.conn_entry(session,dedicated.id) {actual.execution.fence();}
                    }
                }
            }
            let pending = owned.close().await;
            result.map(|mut report| {
                if let Some(review) = &mut report.sql_server_review {review.cleanup_pending = pending;}
                report
            })
        })).await.map_err(|_| uncertain())?
    }
}
async fn execute(
    ctx: &Context,
    input: Input,
    control: SqlServerImportRequest,
) -> ApiResult<CsvImportResponse> {
    ctx.query("SET ROWCOUNT 0; SET NOCOUNT ON; SET XACT_ABORT ON; SET ANSI_WARNINGS ON; SET ARITHABORT ON; SET ARITHIGNORE OFF; SET ANSI_NULLS ON; SET ANSI_PADDING ON; SET CONCAT_NULL_YIELDS_NULL ON; SET NUMERIC_ROUNDABORT OFF; SET QUOTED_IDENTIFIER ON",vec![]).await?;
    ctx.active.progress.lock().unwrap().stage = ImportStage::Reviewing;
    let columns = metadata::capture(ctx, &input).await?;
    let mut imported = Vec::new();
    let mut omitted = Vec::new();
    let mut preserved = false;
    for index in 0..input.columns.len() {
        let c = columns
            .iter()
            .find(|c| c.source_ordinal == Some(index))
            .ok_or_else(|| bad("Input binding disappeared"))?;
        if c.generated != 0 || c.hidden || c.column_set || c.system_type == 189 {
            return Err(bad(
                "Generated-always, hidden, column-set and rowversion input columns are unsupported",
            ));
        }
        let omit = if c.computed {
            if control.mode.computed != SqlServerComputedMode::Omit {
                return Err(bad("Computed input requires explicit omit mode"));
            }
            true
        } else if c.identity {
            match control.mode.identity {
                SqlServerIdentityMode::Reject => {
                    return Err(bad("Identity input requires generate or preserve mode"))
                }
                SqlServerIdentityMode::Generate => true,
                SqlServerIdentityMode::Preserve => {
                    preserved = true;
                    false
                }
            }
        } else {
            false
        };
        if omit {
            omitted.push(c.name.clone());
        } else {
            if !c.can_insert {
                return Err(ApiError::Forbidden(
                    "Native table INSERT permission is required".into(),
                ));
            }
            imported.push((index, c.clone(), metadata::sql_type(c, input.typed)?));
        }
    }
    if imported.is_empty() {
        return Err(bad("Keep at least one writable input column"));
    }
    if control.mode.identity == SqlServerIdentityMode::Preserve && !preserved {
        return Err(bad(
            "Preserve mode requires an explicit identity input column",
        ));
    }
    let target = &columns[0].target;
    ctx.store.authorize_connection_operation(
        ctx.session,
        ctx.source,
        OperationKind::ImportCsv,
        None,
        &[&target.path()],
    )?;
    if preserved && !target.can_alter {
        return Err(ApiError::Forbidden(
            "Preserving identity values requires native table ALTER authority".into(),
        ));
    }
    let mut hash = Sha256::new();
    hash.update(ctx.store.inner.import_salt.as_bytes());
    hash.update(
        serde_json::to_vec(&(
            ctx.session.0,
            ctx.source.0,
            &input.digest,
            &input.columns,
            control.mode,
            &columns,
        ))
        .map_err(|_| bad("Import review could not be encoded"))?,
    );
    let precondition = format!("{:x}", hash.finalize());
    let target_columns = columns
        .iter()
        .filter_map(|c| {
            c.source_ordinal
                .map(|index| sift_protocol::SqlServerImportColumnReview {
                    source: input.columns[index].name.clone(),
                    target: c.name.clone(),
                    native_type: c.native_type_name.clone(),
                    max_length_bytes: c.max_length,
                    precision: c.precision,
                    scale: c.scale,
                    nullable: c.nullable,
                    identity: c.identity,
                    computed: c.computed,
                    omitted: omitted.contains(&c.name),
                    conversion_type: imported
                        .iter()
                        .find(|(_, column, _)| column.column_id == c.column_id)
                        .map(|(_, _, ty)| ty.clone()),
                })
        })
        .collect();
    let review=SqlServerImportReview{target_columns,precondition:precondition.clone(),database:target.database.clone(),schema:target.schema.clone(),table:target.table.clone(),mode:control.mode,imported_columns:imported.iter().map(|(_,c,_)|c.name.clone()).collect(),omitted_columns:omitted,identity_column:columns.iter().find(|c|c.identity).map(|c|c.name.clone()),warning:"Writes use a separate connection with normalized strict settings; editor session settings are not inherited. Identity counters and trigger/external effects may survive rollback. Imported row counts describe accepted input, not result equivalence. Cancellation during COMMIT has an unknown outcome; inspect before retrying.".into(),cleanup_pending:false};
    let mut report = CsvImportResponse {
        table: target.sql(),
        columns: imported
            .iter()
            .map(|(index, c, _)| {
                let mut col = input.columns[*index].clone();
                col.name = c.name.clone();
                col.nullable = c.nullable;
                col
            })
            .collect(),
        table_created: false,
        rows_inserted: 0,
        rows_skipped: 0,
        rows_validated: input.rows.len() as u64,
        resume_from_row: 0,
        dry_run: input.dry_run,
        quarantined_rows: vec![],
        sql_server_review: Some(Box::new(review)),
    };
    if input.dry_run {
        return Ok(report);
    }
    let Some(confirmation) = control
        .confirmation
        .filter(|c| c.confirmed && c.precondition == precondition)
    else {
        return Err(bad(
            "A matching generated-column dry run and explicit confirmation are required",
        ));
    };
    drop(confirmation);
    ctx.authorize()?;
    let transaction = ctx
        .store
        .begin_transaction_as(
            ctx.session,
            BeginTransactionRequest {
                connection: ctx.connection,
                mode: sift_protocol::TxMode {
                    isolation: sift_protocol::IsolationLevel::Serializable,
                    ..Default::default()
                },
            },
            OperationKind::ImportCsv,
        )
        .await?;
    ctx.store.csv_transaction(transaction.tx_id);
    let end = EndTransactionRequest {
        connection: ctx.connection,
        tx_id: transaction.tx_id,
    };
    let work = async {
        if metadata::capture(ctx, &input).await? != columns {
            return Err(bad("Import target changed after review"));
        }
        if preserved {
            ctx.query(&format!("SET IDENTITY_INSERT {} ON", target.sql()), vec![])
                .await?;
        }
        ctx.active.progress.lock().unwrap().stage = ImportStage::Inserting;
        let width = imported.len();
        let batch_size = (2000 / width).clamp(1, 128);
        let names = imported
            .iter()
            .map(|(_, c, _)| metadata::q(&c.name))
            .collect::<Vec<_>>()
            .join(",");
        for batch in input.rows.chunks(batch_size) {
            ctx.store.authorize_connection_operation(
                ctx.session,
                ctx.source,
                OperationKind::ImportCsv,
                None,
                &[&target.path()],
            )?;
            let mut params = Vec::new();
            let mut groups = Vec::new();
            for row in batch {
                let mut slots = Vec::new();
                for (index, _, ty) in &imported {
                    let value = row
                        .values
                        .get(*index)
                        .ok_or_else(|| bad("Imported row width changed"))?;
                    if value.is_null() {
                        slots.push("NULL".into());
                    } else {
                        params.push(value.clone());
                        slots.push(format!("CAST(@P{} AS {ty})", params.len()));
                    }
                }
                groups.push(format!("({})", slots.join(",")));
            }
            ctx.active.progress.lock().unwrap().rows_attempted += batch.len() as u64;
            ctx.write(
                &format!(
                    "INSERT INTO {} ({names}) VALUES {}",
                    target.sql(),
                    groups.join(",")
                ),
                params,
            )
            .await?;
        }
        if metadata::capture(ctx, &input).await? != columns {
            return Err(bad("Import target or authority changed during writes"));
        }
        ctx.authorize()?;
        ctx.store.authorize_connection_operation(
            ctx.session,
            ctx.source,
            OperationKind::ImportCsv,
            None,
            &[&target.path()],
        )?;
        ctx.active.progress.lock().unwrap().stage = ImportStage::Committing;
        ctx.store
            .commit_transaction_as(ctx.session, end.clone(), OperationKind::ImportCsv)
            .await
            .map_err(|_| uncertain())?;
        ctx.active.progress.lock().unwrap().rows_committed = input.rows.len() as u64;
        Ok::<_, ApiError>(())
    }
    .await;
    if let Err(error) = work {
        if !matches!(&error,ApiError::Driver(driver) if driver.code==Code::ImportOutcomeUnknown) {
            ctx.store.csv_rollback(ctx.session, end).await?;
        }
        return Err(error);
    }
    ctx.store.csv_committed(input.rows.len() as u64);
    report.rows_inserted = input.rows.len() as u64;
    report.resume_from_row = input.rows.len() as u64;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{BuiltinProviderAdapter, DatabaseProvider};
    use sift_driver_api::mock::MockDriver;

    fn done() -> Vec<Page> {
        vec![Page::Done {
            affected_rows: None,
            warnings: vec![],
        }]
    }
    fn catalog() -> Vec<Page> {
        let column = serde_json::json!({"target":{"server":"mock","database":"mock","database_id":1,"login_sid":"01","user_id":1,"user_sid":"01","schema":"dbo","schema_id":1,"schema_owner":1,"table":"target","object_id":1,"created":"2026-01-01","modified":"2026-01-01","can_alter":true,"language":"us_english","date_format":"mdy","memory_optimized":false,"temporal":0,"file_table":false,"system_table":false},"column_id":1,"name":"label","system_type":231,"user_type":231,"native_type_name":"nvarchar","max_length":100,"precision":0,"scale":0,"collation":"Latin1_General_CI_AS","nullable":false,"identity":false,"computed":false,"generated":0,"hidden":false,"column_set":false,"seed":null,"increment":null,"default_id":0,"computed_hash":null,"can_insert":true,"source_ordinal":0,"matches":1});
        vec![
            Page::Rows {
                rows: vec![Row {
                    values: vec![Value::Text(serde_json::to_string(&vec![column]).unwrap())],
                }],
            },
            done().pop().unwrap(),
        ]
    }
    #[tokio::test]
    async fn lost_commit_ack_reports_unknown_and_closes_disposable_connection() {
        let mock = Arc::new(
            MockDriver::builder()
                .engine(Engine::SqlServer)
                .execute_ok(done())
                .execute_ok(catalog())
                .execute_ok(done())
                .execute_ok(catalog())
                .execute_ok(catalog())
                .execute_ok(done())
                .execute_ok(catalog())
                .commit_err(DriverError::new(
                    Code::DriverInternal,
                    "private native details",
                ))
                .build(),
        );
        let registry = DriverRegistry::builder().build();
        registry
            .providers()
            .replace([
                Arc::new(BuiltinProviderAdapter::new(mock.clone())) as Arc<dyn DatabaseProvider>
            ])
            .unwrap();
        let store = SessionStore::new(registry);
        let session = store
            .open_session(OpenSessionRequest {
                tag: None,
                tenant_id: None,
            })
            .id;
        let source = store
            .open_connection(
                session,
                Engine::SqlServer,
                ConnectionSpec {
                    host: "mock.invalid".into(),
                    port: None,
                    database: Some("mock".into()),
                    user: "mock".into(),
                    password: None,
                    ssl_mode: Some(sift_protocol::SslMode::Disable),
                    engine_specific: None,
                },
            )
            .await
            .unwrap()
            .id;
        let input = |dry_run| Input {
            table: "dbo.target".into(),
            columns: vec![InferredCsvColumn {
                name: "label".into(),
                inferred_type: sift_protocol::InferredCsvType::Text,
                nullable: false,
            }],
            rows: vec![Row {
                values: vec![Value::Text("private input".into())],
            }],
            digest: "bound-input".into(),
            typed: false,
            dry_run,
        };
        let mut control = SqlServerImportRequest {
            run_id: uuid::Uuid::new_v4(),
            mode: Default::default(),
            confirmation: None,
        };
        let preview = store
            .reviewed_sqlserver_import(session, source, input(true), control.clone())
            .await
            .unwrap();
        control.run_id = uuid::Uuid::new_v4();
        control.confirmation = Some(sift_protocol::SqlServerImportConfirmation {
            precondition: preview.sql_server_review.unwrap().precondition,
            confirmed: true,
        });
        let error = store
            .reviewed_sqlserver_import(session, source, input(false), control)
            .await
            .unwrap_err();
        assert!(matches!(&error,ApiError::Driver(error) if error.code==Code::ImportOutcomeUnknown));
        assert!(!error.to_string().contains("private"));
        assert!(store.inner.imports.is_empty());
        assert_eq!(store.inner.import_slots.available_permits(), 4);
        assert_eq!(store.list_connections(session).unwrap().len(), 1);
        assert_eq!(
            mock.invocations()
                .iter()
                .filter(|&&method| method == "close")
                .count(),
            2
        );
        store.close_connection(session, source).await.unwrap();
    }
}
