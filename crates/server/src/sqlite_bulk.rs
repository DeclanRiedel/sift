//! Preview-bound native SQLite parameter batches (ADR-074).

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use sha2::{Digest, Sha256};
use sift_protocol::{
    BeginTransactionRequest, BulkInsertFormat, BulkInsertRequest, BulkInsertResponse,
    ColumnMetadata, ConnectionId, EndTransactionRequest, Engine, ExecuteRequestHttp,
    IsolationLevel, Nullability, ObjectKind, ObjectPath, OperationKind, SessionId,
    SqliteNativeBulkRows, TxHandleRef, TxMode, Value,
};

use crate::{
    error::{ApiError, ApiResult},
    session::SessionStore,
};

const MAX_ROWS: usize = 10_000;
const MAX_COLUMNS: usize = 128;
const MAX_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;
const MAX_CELL_BYTES: usize = 1024 * 1024;
const MAX_PARAMETERS_PER_BATCH: usize = 999;
const MAX_APPLY_SECONDS: u64 = 120;

use crate::session::bulk_execution::BulkExecution;

struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

pub(crate) async fn run(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    request: BulkInsertRequest,
    context: BulkExecution,
) -> ApiResult<BulkInsertResponse> {
    if request.format != BulkInsertFormat::Native || !request.data.is_empty() {
        return Err(ApiError::BadRequest(
            "SQLite bulk accepts typed native rows only; use CSV import for uploaded files".into(),
        ));
    }
    let native = request
        .native
        .ok_or_else(|| ApiError::BadRequest("SQLite native bulk rows are required".into()))?;
    let (table_name, path) = table_path(&request.table)?;
    validate_dimensions(&native)?;
    let snapshot = store
        .bulk_schema(session, connection, path.clone(), &context)
        .await?;
    let object = snapshot
        .trees
        .iter()
        .flat_map(|catalog| &catalog.schemas)
        .filter(|schema| schema.name == "main")
        .flat_map(|schema| &schema.objects)
        .find(|object| object.name == table_name && object.kind == ObjectKind::Table)
        .cloned()
        .ok_or_else(|| {
            ApiError::BadRequest(
                "SQLite bulk target must be an existing ordinary main table".into(),
            )
        })?;
    let ddl = store
        .sqlite_bulk_target_ddl(session, connection, path, context.source.clone())
        .await?;
    if ddl.to_ascii_uppercase().contains("CREATE TRIGGER") {
        return Err(ApiError::BadRequest(
            "SQLite native bulk refuses tables with triggers".into(),
        ));
    }
    let columns = target_columns(&object.columns, &native.columns)?;
    for column in &object.columns {
        if native
            .columns
            .iter()
            .any(|name| column.name.eq_ignore_ascii_case(name))
        {
            continue;
        }
        if column.primary_key
            || column.auto_increment
            || column
                .facets
                .sqlite
                .as_ref()
                .is_some_and(|facets| facets.default_expr.is_some())
        {
            return Err(ApiError::BadRequest(format!(
                "SQLite bulk column `{}` must be explicit to avoid generated values",
                column.name
            )));
        }
    }
    validate_values(&native, &columns)?;
    let affinity_labels = native
        .columns
        .iter()
        .zip(&columns)
        .map(|(name, column)| {
            format!(
                "{name}:{}",
                column
                    .facets
                    .sqlite
                    .as_ref()
                    .expect("checked above")
                    .affinity
            )
        })
        .collect::<Vec<_>>();
    let fingerprint = fingerprint(
        session,
        connection,
        &request.table,
        &native,
        &object.columns,
        &ddl,
    )?;
    let rows_validated = native.rows.len() as u64;
    if request.preview {
        if request.preview_token.is_some() || request.confirm_write {
            return Err(ApiError::BadRequest(
                "SQLite bulk preview cannot include apply confirmation".into(),
            ));
        }
        let token = store.store_sqlite_bulk_preview(session, connection, fingerprint)?;
        return Ok(BulkInsertResponse {
            rows_inserted: 0,
            rows_validated,
            preview_token: Some(token),
            target_affinities: affinity_labels,
        });
    }
    if !request.confirm_write {
        return Err(ApiError::BadRequest(
            "confirm_write is required for SQLite native bulk apply".into(),
        ));
    }
    let token = request
        .preview_token
        .as_deref()
        .ok_or_else(|| ApiError::BadRequest("preview SQLite native bulk before apply".into()))?;
    store.consume_sqlite_bulk_preview(token, session, connection, &fingerprint)?;
    let worker_store = store.clone();
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_on_drop = CancelOnDrop(cancel.clone());
    let observation = context.clone();
    let query_guard = crate::shutdown::current_query_guard();
    let task = tokio::spawn(crate::shutdown::scope_query(query_guard, async move {
        apply_batches(
            worker_store,
            session,
            connection,
            table_name,
            native,
            cancel,
            context,
        )
        .await
    }));
    let result = match task.await {
        Ok(result) => result,
        Err(_) => {
            // A lost worker may leave a transaction open. Fence the captured
            // generation; cleanup must never close a replacement by connection ID.
            observation.rollback(false);
            return Err(ApiError::Driver(sift_protocol::DriverError::new(
                sift_protocol::Code::ConnectionInvalidated,
                "bulk worker failed; reconnect and inspect the database",
            )));
        }
    };
    drop(cancel_on_drop);
    Ok(BulkInsertResponse {
        rows_inserted: result?,
        rows_validated,
        preview_token: None,
        target_affinities: affinity_labels,
    })
}

fn table_path(table: &str) -> ApiResult<(String, ObjectPath)> {
    let name = match table.split('.').collect::<Vec<_>>().as_slice() {
        [name] => *name,
        ["main", name] => *name,
        _ => {
            return Err(ApiError::BadRequest(
                "SQLite bulk target must be table or main.table".into(),
            ))
        }
    };
    if name.is_empty()
        || name.len() > 256
        || name.contains('\0')
        || name.trim() != name
        || name.to_ascii_lowercase().starts_with("sqlite_")
    {
        return Err(ApiError::BadRequest(
            "invalid SQLite bulk table name".into(),
        ));
    }
    Ok((
        name.into(),
        ObjectPath {
            catalog: None,
            schema: Some("main".into()),
            name: name.into(),
            kind: Some(ObjectKind::Table),
            routine_args: None,
        },
    ))
}

fn validate_dimensions(native: &SqliteNativeBulkRows) -> ApiResult<()> {
    let width = native.columns.len();
    if width == 0 || width > MAX_COLUMNS || native.rows.is_empty() || native.rows.len() > MAX_ROWS {
        return Err(ApiError::BadRequest(
            "SQLite native bulk exceeds row or column limits".into(),
        ));
    }
    let mut unique = std::collections::HashSet::new();
    for name in &native.columns {
        if name.is_empty()
            || name.len() > 256
            || name.trim() != name
            || name.contains('\0')
            || !unique.insert(name.to_ascii_lowercase())
        {
            return Err(ApiError::BadRequest(
                "SQLite native bulk columns must be unique nonempty names".into(),
            ));
        }
    }
    if native.rows.iter().any(|row| row.len() != width) {
        return Err(ApiError::BadRequest(
            "SQLite native bulk row width does not match columns".into(),
        ));
    }
    let bytes = serde_json::to_vec(native).map_err(|error| {
        ApiError::BadRequest(format!("invalid SQLite native bulk payload: {error}"))
    })?;
    if bytes.len() > MAX_PAYLOAD_BYTES {
        return Err(ApiError::BadRequest(
            "SQLite native bulk payload exceeds 8 MiB".into(),
        ));
    }
    Ok(())
}

fn target_columns(metadata: &[ColumnMetadata], names: &[String]) -> ApiResult<Vec<ColumnMetadata>> {
    names
        .iter()
        .map(|name| {
            let column = metadata
                .iter()
                .find(|column| column.name.eq_ignore_ascii_case(name))
                .ok_or_else(|| {
                    ApiError::BadRequest(format!("SQLite bulk column `{name}` is unavailable"))
                })?;
            let facets = column.facets.sqlite.as_ref().ok_or_else(|| {
                ApiError::BadRequest("SQLite bulk target lacks native column metadata".into())
            })?;
            if facets.virtual_table
                || facets.hidden != 0
                || !["integer", "numeric", "real", "text", "blob"]
                    .contains(&facets.affinity.as_str())
            {
                return Err(ApiError::BadRequest(format!(
                    "SQLite bulk column `{name}` has an unsupported shape"
                )));
            }
            Ok(column.clone())
        })
        .collect()
}

fn validate_values(native: &SqliteNativeBulkRows, columns: &[ColumnMetadata]) -> ApiResult<()> {
    for (row_index, row) in native.rows.iter().enumerate() {
        for (index, (value, column)) in row.iter().zip(columns).enumerate() {
            let affinity = &column
                .facets
                .sqlite
                .as_ref()
                .expect("checked above")
                .affinity;
            if serde_json::to_vec(value).is_ok_and(|bytes| bytes.len() > MAX_CELL_BYTES) {
                return Err(ApiError::BadRequest(format!(
                    "SQLite bulk row {} column `{}` exceeds the 1 MiB cell limit",
                    row_index + 1,
                    native.columns[index]
                )));
            }
            let allowed = match value {
                Value::Null | Value::TypedNull { .. } => {
                    !column.primary_key && column.nullable != Nullability::NotNullable
                }
                Value::Bool(_) | Value::Int16(_) | Value::Int32(_) | Value::Int64(_) => {
                    affinity == "integer" || affinity == "numeric"
                }
                Value::Float32(value) => value.is_finite() && affinity == "real",
                Value::Float64(value) => value.is_finite() && affinity == "real",
                Value::Decimal(value) => affinity == "text" && canonical_decimal(value),
                Value::Text(value) => affinity == "text" && value.len() <= MAX_CELL_BYTES,
                Value::Blob(value) => affinity == "blob" && value.len() <= MAX_CELL_BYTES,
                Value::Date(_)
                | Value::Time(_)
                | Value::Timestamp(_)
                | Value::TimestampTz(_)
                | Value::Uuid(_)
                | Value::Json(_) => affinity == "text",
                Value::Interval(_) | Value::Native { .. } => false,
            };
            if !allowed {
                return Err(ApiError::BadRequest(format!(
                    "SQLite bulk row {} column `{}` cannot be stored without implicit conversion",
                    row_index + 1,
                    native.columns[index]
                )));
            }
        }
    }
    Ok(())
}

fn canonical_decimal(value: &str) -> bool {
    let unsigned = value.strip_prefix('-').unwrap_or(value);
    let mut parts = unsigned.split('.');
    let integer = parts.next().unwrap_or_default();
    let fractional = parts.next();
    if parts.next().is_some()
        || integer.is_empty()
        || !integer.bytes().all(|byte| byte.is_ascii_digit())
        || (integer.len() > 1 && integer.starts_with('0'))
    {
        return false;
    }
    let scale = fractional.map_or(0, str::len);
    if fractional
        .is_some_and(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return false;
    }
    integer.len() + scale <= 38 && scale <= 18
}

fn fingerprint(
    session: SessionId,
    connection: ConnectionId,
    table: &str,
    native: &SqliteNativeBulkRows,
    columns: &[ColumnMetadata],
    ddl: &str,
) -> ApiResult<String> {
    let bytes = serde_json::to_vec(&(session, connection, table, native, columns, ddl))
        .map_err(|error| ApiError::Internal(format!("fingerprint SQLite native bulk: {error}")))?;
    Ok(format!("{:x}", Sha256::digest(&bytes)))
}

async fn apply_batches(
    store: SessionStore,
    session: SessionId,
    connection: ConnectionId,
    table: String,
    native: SqliteNativeBulkRows,
    cancel: Arc<AtomicBool>,
    context: BulkExecution,
) -> ApiResult<u64> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(MAX_APPLY_SECONDS);
    let transaction = store
        .begin_transaction_source(
            session,
            BeginTransactionRequest {
                connection,
                mode: TxMode {
                    isolation: IsolationLevel::Serializable,
                    ..Default::default()
                },
            },
            OperationKind::BulkInsert,
            Some(context.source.clone()),
        )
        .await?;
    context.transaction(transaction.tx_id);
    let end = EndTransactionRequest {
        connection,
        tx_id: transaction.tx_id,
    };
    let tx = Some(TxHandleRef {
        tx_id: transaction.tx_id,
        connection,
        mode: transaction.mode,
    });
    let width = native.columns.len();
    let batch_size = (MAX_PARAMETERS_PER_BATCH / width).min(128);
    let names = native
        .columns
        .iter()
        .map(|name| crate::ddl::quote_ident(name, Engine::Sqlite))
        .collect::<Vec<_>>()
        .join(", ");
    let target = format!(
        "\"main\".{}",
        crate::ddl::quote_ident(&table, Engine::Sqlite)
    );
    let work = async {
        let mut inserted = 0u64;
        for batch in native.rows.chunks(batch_size) {
            if cancel.load(Ordering::Acquire) || tokio::time::Instant::now() >= deadline {
                return Err(ApiError::BadRequest(
                    "SQLite native bulk canceled or exceeded 120 seconds".into(),
                ));
            }
            let mut params = Vec::with_capacity(batch.len() * width);
            let mut groups = Vec::with_capacity(batch.len());
            for row in batch {
                let mut slots = Vec::with_capacity(width);
                for value in row {
                    params.push(value.clone());
                    slots.push(format!("?{}", params.len()));
                }
                groups.push(format!("({})", slots.join(", ")));
            }
            store.ensure_captured_connection(session, connection, &context.source)?;
            context.dispatch()?;
            let response = store
                .execute_http_bounded(
                    session,
                    ExecuteRequestHttp {
                        connection,
                        sql: format!(
                            "INSERT INTO {target} ({names}) VALUES {}",
                            groups.join(", ")
                        ),
                        params,
                        tx: tx.clone(),
                        room_id: None,
                        connection_profile_id: None,
                        transform: None,
                        source: None,
                    },
                    OperationKind::BulkInsert,
                    None,
                    true,
                    Some(context.source.clone()),
                )
                .await?;
            let count = response.affected_rows.ok_or_else(|| {
                ApiError::Driver(sift_protocol::DriverError::new(
                    sift_protocol::Code::UnsupportedResultShape,
                    "SQLite bulk requires an observed affected count",
                ))
            })?;
            inserted = inserted
                .checked_add(count)
                .ok_or_else(|| ApiError::Internal("bulk affected count overflow".into()))?;
        }
        Ok::<u64, ApiError>(inserted)
    }
    .await;
    match work {
        Ok(inserted)
            if !cancel.load(Ordering::Acquire) && tokio::time::Instant::now() < deadline =>
        {
            match store
                .commit_transaction_as(session, end.clone(), OperationKind::BulkInsert)
                .await
            {
                Ok(()) => Ok(inserted),
                Err(error) => {
                    // A terminal command with an uncertain outcome must not claim rollback.
                    if context.is_fenced() {
                        return Err(error);
                    }
                    rollback_owned(&store, session, end, &context).await?;
                    Err(error)
                }
            }
        }
        result => {
            rollback_owned(&store, session, end, &context).await?;
            result.and_then(|_| {
                Err(ApiError::BadRequest(
                    "SQLite native bulk canceled or exceeded 120 seconds".into(),
                ))
            })
        }
    }
}

async fn rollback_owned(
    store: &SessionStore,
    session: SessionId,
    end: EndTransactionRequest,
    context: &BulkExecution,
) -> ApiResult<()> {
    let confirmed = store
        .rollback_transaction_as(session, end, OperationKind::BulkInsert)
        .await
        .is_ok();
    context.rollback(confirmed);
    if confirmed {
        Ok(())
    } else {
        Err(ApiError::Driver(sift_protocol::DriverError::new(
            sift_protocol::Code::ConnectionInvalidated,
            "bulk rollback could not be confirmed; reconnect and inspect the database",
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_driver_api::mock::MockDriver;
    use sift_protocol::{Code, ConnectionSpec, DriverError, OpenSessionRequest, Page};
    #[tokio::test]
    async fn batch_counts_and_terminal_uncertainty_remain_distinct() {
        for mode in ["known", "missing", "rollback-fails", "commit-fails"] {
            // The mock implements the ordinary Driver transaction/execute contract;
            // native SQLite SQL/value acceptance is covered by sqlite_native_bulk.
            let builder = MockDriver::builder().execute_ok(vec![Page::Done {
                affected_rows: (mode == "known" || mode == "commit-fails").then_some(1),
                warnings: vec![],
            }]);
            let builder = match mode {
                "rollback-fails" => builder.rollback_err(DriverError::new(
                    Code::ConnectionFailed,
                    "private-native-diagnostic",
                )),
                "commit-fails" => builder.commit_err(DriverError::new(
                    Code::ConnectionFailed,
                    "private-native-diagnostic",
                )),
                _ => builder,
            };
            let store = SessionStore::new(
                crate::DriverRegistry::builder()
                    .register(builder.build())
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
            let context = BulkExecution::new(store.conn_entry(session, connection).unwrap());
            let result = apply_batches(
                store.clone(),
                session,
                connection,
                "users".into(),
                SqliteNativeBulkRows {
                    columns: vec!["id".into()],
                    rows: vec![vec![Value::Int32(1)]],
                },
                Arc::new(AtomicBool::new(false)),
                context.clone(),
            )
            .await;
            let observation = context.observation();
            assert!(observation.attempted);
            assert!(observation.transaction_id.is_some());
            match mode {
                "known" => {
                    assert_eq!(result.unwrap(), 1);
                    assert_eq!(observation.rollback, None);
                    assert!(!context.is_fenced());
                }
                "missing" => {
                    assert!(matches!(
                        result,
                        Err(ApiError::Driver(DriverError {
                            code: Code::UnsupportedResultShape,
                            ..
                        }))
                    ));
                    assert_eq!(observation.rollback, Some(true));
                    assert!(!context.is_fenced());
                }
                "rollback-fails" => {
                    assert!(result.is_err());
                    assert_eq!(observation.rollback, Some(false));
                    assert!(context.is_fenced());
                }
                "commit-fails" => {
                    assert!(result.is_err());
                    assert_eq!(observation.rollback, None);
                    assert!(context.is_fenced());
                }
                _ => unreachable!(),
            }
        }
    }
}
