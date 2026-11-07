use sha2::Digest;
use std::collections::{BTreeSet, HashSet};

use sift_driver_api::CopyOp;
use sift_protocol::{
    Code, ConnectionId, CsvConflictPolicy, CsvImportRequest, CsvImportResponse, DriverError,
    Engine, ExecuteRequestHttp, InferredCsvColumn, InferredCsvType, ObjectKind, ObjectPath,
    SchemaScope, SessionId, Value,
};

use crate::ddl::quote_ident;
use crate::error::{ApiError, ApiResult};
use crate::session::SessionStore;

const MAX_CSV_BYTES: usize = 64 * 1024 * 1024;
const INFERENCE_ROWS: usize = 1_000;

mod resume;
pub use resume::import_with_checkpoint;
pub(crate) use resume::{import_with_checkpoint_text, CheckpointContext};

pub(crate) struct PreparedCsv {
    pub(crate) columns: Vec<InferredCsvColumn>,
    pub(crate) records: Vec<Vec<Option<String>>>,
    delimiter: u8,
}

pub async fn import(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    request: CsvImportRequest,
) -> ApiResult<CsvImportResponse> {
    import_with_context(store, session, connection, request, "direct-csv").await
}

pub(crate) async fn import_with_context(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    request: CsvImportRequest,
    review_context: &str,
) -> ApiResult<CsvImportResponse> {
    import_with_context_text(
        store,
        session,
        connection,
        request,
        review_context,
        &BTreeSet::new(),
    )
    .await
}

pub(crate) async fn import_with_context_text(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    request: CsvImportRequest,
    review_context: &str,
    text_columns: &BTreeSet<String>,
) -> ApiResult<CsvImportResponse> {
    let source = store.authorize_connection_operation(
        session,
        connection,
        sift_protocol::OperationKind::ImportCsv,
        None,
        &[&table_path(&request.table)?],
    )?;
    let bound = store.bind_csv_source(session, connection, source)?;
    let store = &bound;
    if !request.dry_run && request.sql_server.is_none() {
        store.csv_admission(session, connection)?;
    }
    let target = table_path(&request.table)?;
    store.authorize_connection_operation(
        session,
        connection,
        sift_protocol::OperationKind::ImportCsv,
        None,
        &[&target],
    )?;
    if let Some(control) = request.sql_server.clone() {
        if request.create_table
            || request.resume_from_row != 0
            || !request.type_mappings.is_empty()
            || request.conflict_policy != CsvConflictPolicy::Abort
        {
            return Err(ApiError::BadRequest("Reviewed generated-column imports require an existing table, atomic abort policy, no type overrides and no resume".into()));
        }
        let mut hash = sha2::Sha256::new();
        hash.update(
            serde_json::to_vec(&(
                review_context,
                request.header,
                request.delimiter,
                &request.null_value,
            ))
            .map_err(|_| ApiError::BadRequest("Cannot encode import review context".into()))?,
        );
        hash.update(&request.data);
        let digest = format!("{:x}", hash.finalize());
        let prepared = prepare_text(&request, text_columns)?;
        let input = crate::session::sqlserver_import::Input {
            table: request.table,
            columns: prepared.columns,
            rows: prepared
                .records
                .into_iter()
                .map(|row| sift_protocol::Row {
                    values: row
                        .into_iter()
                        .map(|v| v.map_or(Value::Null, Value::Text))
                        .collect(),
                })
                .collect(),
            digest,
            typed: false,
            dry_run: request.dry_run,
        };
        return store
            .reviewed_sqlserver_import(session, connection, input, control)
            .await;
    }
    let prepared = prepare_text(&request, text_columns)?;
    let entry = store.conn_entry(session, connection)?;
    let engine = entry.driver.engine();
    let table = qualified_table(&request.table, engine)?;

    validate_type_mappings(&request, &prepared, engine)?;
    if request.resume_from_row > 0 && request.conflict_policy == CsvConflictPolicy::Abort {
        return Err(ApiError::BadRequest(
            "resumed imports require conflict_policy=skip for row-safe replay".into(),
        ));
    }
    let row_count = prepared.records.len() as u64;
    if request.resume_from_row > row_count {
        return Err(ApiError::BadRequest(
            "resume_from_row exceeds the input row count".into(),
        ));
    }
    if request.dry_run {
        return Ok(CsvImportResponse {
            table: request.table,
            columns: prepared.columns,
            table_created: false,
            rows_inserted: 0,
            rows_skipped: 0,
            rows_validated: row_count,
            resume_from_row: request.resume_from_row,
            dry_run: true,
            quarantined_rows: Vec::new(),
            sql_server_review: None,
        });
    }

    if engine == Engine::Sqlite
        || (engine == Engine::SqlServer && request.conflict_policy == CsvConflictPolicy::Abort)
    {
        return import_atomic(store, session, connection, request, prepared, table, engine).await;
    }
    if request.create_table {
        let ddl = create_table_sql(&table, &prepared.columns, engine, &request.type_mappings);
        store
            .csv_execute(session, execute_request(connection, ddl, Vec::new()), true)
            .await?;
        store.csv_autocommit(true, 0);
    }

    let (rows_inserted, rows_skipped, quarantined_rows) = match request.conflict_policy {
        CsvConflictPolicy::Abort => {
            let rows = ingest_abort(store, session, connection, entry, &request, &prepared).await?;
            (rows, 0, Vec::new())
        }
        CsvConflictPolicy::Skip | CsvConflictPolicy::Quarantine => {
            let target_types = if request.create_table {
                created_column_types(&prepared.columns, engine, &request.type_mappings)
            } else {
                target_column_types(store, session, connection, &request.table, &prepared).await?
            };
            let quarantine = request.conflict_policy == CsvConflictPolicy::Quarantine;
            ingest_skip(
                store,
                &prepared,
                SkipIngest {
                    session,
                    connection,
                    engine,
                    table: &table,
                    target_types: &target_types,
                    resume_from_row: request.resume_from_row,
                    quarantine,
                },
            )
            .await?
        }
    };

    Ok(CsvImportResponse {
        table: request.table,
        columns: prepared.columns,
        table_created: request.create_table,
        rows_inserted,
        rows_skipped,
        rows_validated: row_count,
        resume_from_row: row_count,
        dry_run: false,
        quarantined_rows,
        sql_server_review: None,
    })
}

fn validate_type_mappings(
    request: &CsvImportRequest,
    prepared: &PreparedCsv,
    engine: Engine,
) -> ApiResult<()> {
    sift_core::transfer_columns::validate_provider_targets(
        engine,
        prepared.columns.iter().map(|column| column.name.as_str()),
    )
    .map_err(|message| ApiError::BadRequest(message.into()))?;
    use sqlparser::dialect::{Dialect, MsSqlDialect, PostgreSqlDialect};
    use sqlparser::parser::Parser;
    use sqlparser::tokenizer::Token;

    let dialect: &dyn Dialect = match engine {
        Engine::Postgres => &PostgreSqlDialect {},
        Engine::SqlServer => &MsSqlDialect {},
        Engine::Sqlite => &sqlparser::dialect::SQLiteDialect {},
    };
    for (column, sql_type) in &request.type_mappings {
        if !prepared
            .columns
            .iter()
            .any(|candidate| candidate.name == *column)
        {
            return Err(ApiError::BadRequest(format!(
                "type mapping references unknown column `{column}`"
            )));
        }
        if sql_type.is_empty()
            || sql_type.len() > 128
            || sql_type.contains(';')
            || sql_type.contains("--")
            || sql_type.contains("/*")
            || !Parser::new(dialect)
                .try_with_sql(sql_type)
                .is_ok_and(|mut parser| {
                    parser.parse_data_type().is_ok() && parser.peek_token().token == Token::EOF
                })
        {
            return Err(ApiError::BadRequest(format!(
                "type mapping for `{column}` is not a safe SQL type"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
fn prepare(request: &CsvImportRequest) -> ApiResult<PreparedCsv> {
    prepare_text(request, &BTreeSet::new())
}

fn prepare_text(
    request: &CsvImportRequest,
    text_columns: &BTreeSet<String>,
) -> ApiResult<PreparedCsv> {
    prepare_input(
        &request.data,
        request.header,
        request.delimiter,
        request.null_value.as_deref(),
        text_columns,
    )
}
pub(crate) fn prepare_input(
    data: &[u8],
    header: bool,
    delimiter: char,
    null_value: Option<&str>,
    text_columns: &BTreeSet<String>,
) -> ApiResult<PreparedCsv> {
    if data.is_empty() {
        return Err(ApiError::BadRequest("CSV data must not be empty".into()));
    }
    if data.len() > MAX_CSV_BYTES {
        return Err(ApiError::Driver(DriverError::new(
            Code::ResultTooLarge,
            "CSV import exceeds the 64 MiB payload limit",
        )));
    }
    if !header {
        return Err(ApiError::BadRequest(
            "CSV import requires a header row".into(),
        ));
    }
    if !delimiter.is_ascii() {
        return Err(ApiError::BadRequest("CSV delimiter must be ASCII".into()));
    }
    let delimiter = delimiter as u8;
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(true)
        .from_reader(data);
    let headers = reader
        .headers()
        .map_err(csv_error)?
        .iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    if headers.is_empty()
        || headers
            .iter()
            .any(|header| header.is_empty() || header.trim() != header)
    {
        return Err(ApiError::BadRequest(
            "CSV header names must be non-empty and have no surrounding whitespace".into(),
        ));
    }
    let unique: HashSet<_> = headers.iter().collect();
    if unique.len() != headers.len() {
        return Err(ApiError::BadRequest(
            "CSV header names must be unique".into(),
        ));
    }

    if text_columns.iter().any(|name| !headers.contains(name)) {
        return Err(ApiError::BadRequest(
            "Computed text column is missing from CSV headers".into(),
        ));
    }
    let mut records = Vec::new();
    for record in reader.records() {
        let record = record.map_err(csv_error)?;
        if record.len() != headers.len() {
            return Err(ApiError::BadRequest(
                "CSV record width does not match header width".into(),
            ));
        }
        records.push(
            record
                .iter()
                .map(|field| {
                    if null_value == Some(field) {
                        None
                    } else {
                        Some(field.to_string())
                    }
                })
                .collect(),
        );
    }
    if records.is_empty() {
        return Err(ApiError::BadRequest(
            "CSV import requires at least one data row".into(),
        ));
    }

    let columns = headers
        .into_iter()
        .enumerate()
        .map(|(index, name)| {
            let force_text = text_columns.contains(&name);
            let mut column = infer_column(name, index, &records);
            if force_text {
                column.inferred_type = InferredCsvType::Text;
            }
            column
        })
        .collect();
    Ok(PreparedCsv {
        columns,
        records,
        delimiter,
    })
}

fn infer_column(name: String, index: usize, records: &[Vec<Option<String>>]) -> InferredCsvColumn {
    let mut inferred = None;
    let mut nullable = false;
    for value in records.iter().take(INFERENCE_ROWS).map(|row| &row[index]) {
        match value {
            None => nullable = true,
            Some(value) => {
                let candidate = classify(value);
                inferred = Some(match inferred {
                    None => candidate,
                    Some(current) => merge_types(current, candidate),
                });
            }
        }
    }
    InferredCsvColumn {
        name,
        inferred_type: inferred.unwrap_or(InferredCsvType::Text),
        nullable,
    }
}

fn classify(value: &str) -> InferredCsvType {
    if matches!(value, "true" | "false" | "TRUE" | "FALSE") {
        InferredCsvType::Boolean
    } else if value.parse::<i64>().is_ok() {
        InferredCsvType::Int64
    } else if value.parse::<f64>().is_ok_and(|number| number.is_finite()) {
        InferredCsvType::Decimal
    } else if chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok() {
        InferredCsvType::Date
    } else if chrono::DateTime::parse_from_rfc3339(value).is_ok() {
        InferredCsvType::TimestampTz
    } else {
        InferredCsvType::Text
    }
}

fn merge_types(left: InferredCsvType, right: InferredCsvType) -> InferredCsvType {
    if left == right {
        left
    } else if matches!(
        (left, right),
        (InferredCsvType::Int64, InferredCsvType::Decimal)
            | (InferredCsvType::Decimal, InferredCsvType::Int64)
    ) {
        InferredCsvType::Decimal
    } else {
        InferredCsvType::Text
    }
}

fn create_table_sql(
    table: &str,
    columns: &[InferredCsvColumn],
    engine: Engine,
    mappings: &std::collections::BTreeMap<String, String>,
) -> String {
    let definitions = columns
        .iter()
        .map(|column| {
            format!(
                "{} {} {}",
                quote_ident(&column.name, engine),
                mappings
                    .get(&column.name)
                    .map_or(inferred_sql(column.inferred_type, engine), String::as_str),
                if column.nullable { "NULL" } else { "NOT NULL" }
            )
        })
        .collect::<Vec<_>>();
    format!("CREATE TABLE {table} ({})", definitions.join(", "))
}

fn created_column_types(
    columns: &[InferredCsvColumn],
    engine: Engine,
    mappings: &std::collections::BTreeMap<String, String>,
) -> Vec<String> {
    columns
        .iter()
        .map(|column| {
            mappings
                .get(&column.name)
                .cloned()
                .unwrap_or_else(|| inferred_sql(column.inferred_type, engine).to_string())
        })
        .collect()
}

fn inferred_sql(inferred: InferredCsvType, engine: Engine) -> &'static str {
    match (inferred, engine) {
        (InferredCsvType::Boolean, Engine::Sqlite) => "INTEGER",
        (
            InferredCsvType::Decimal
            | InferredCsvType::Date
            | InferredCsvType::TimestampTz
            | InferredCsvType::Text,
            Engine::Sqlite,
        ) => "TEXT",
        (InferredCsvType::Boolean, Engine::Postgres) => "boolean",
        (InferredCsvType::Boolean, Engine::SqlServer) => "bit",
        (InferredCsvType::Int64, _) => "bigint",
        (InferredCsvType::Decimal, Engine::Postgres) => "numeric",
        (InferredCsvType::Decimal, Engine::SqlServer) => "decimal(38,10)",
        (InferredCsvType::Date, _) => "date",
        (InferredCsvType::TimestampTz, Engine::Postgres) => "timestamptz",
        (InferredCsvType::TimestampTz, Engine::SqlServer) => "datetimeoffset",
        (InferredCsvType::Text, Engine::Postgres) => "text",
        (InferredCsvType::Text, Engine::SqlServer) => "nvarchar(max)",
    }
}

async fn ingest_abort(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    entry: crate::session::ConnectionEntryClone,
    request: &CsvImportRequest,
    prepared: &PreparedCsv,
) -> ApiResult<u64> {
    use crate::session::bulk_execution::{BulkExecution, NativeTransfer};
    let context = BulkExecution::new(entry.clone());
    let transfer = match entry.driver.engine() {
        Engine::Postgres => NativeTransfer::Copy(CopyOp::Import {
            table: request.table.clone(),
            columns: prepared
                .columns
                .iter()
                .map(|column| column.name.clone())
                .collect(),
            data: request.data.clone(),
            delimiter: prepared.delimiter,
            header: request.header,
            null_value: request.null_value.clone(),
        }),
        engine => return Err(missing_ext(engine).into()),
    };
    let transaction = store
        .begin_transaction_as(
            session,
            sift_protocol::BeginTransactionRequest {
                connection,
                mode: Default::default(),
            },
            sift_protocol::OperationKind::ImportCsv,
        )
        .await?;
    store.csv_transaction(transaction.tx_id);
    let end = sift_protocol::EndTransactionRequest {
        connection,
        tx_id: transaction.tx_id,
    };
    let tx = sift_protocol::TxHandleRef {
        tx_id: transaction.tx_id,
        connection,
        mode: transaction.mode,
    };
    let result = store
        .native_transfer(
            session,
            connection,
            sift_protocol::OperationKind::ImportCsv,
            context.clone(),
            transfer,
            Some(tx),
        )
        .await;
    if context.observation().attempted {
        store.csv_attempt(false);
    }
    let count = match result.and_then(|count| store.csv_count(count)) {
        Ok(count) => count,
        Err(error) => {
            store.csv_rollback(session, end).await?;
            return Err(error);
        }
    };
    match store
        .commit_transaction_as(
            session,
            end.clone(),
            sift_protocol::OperationKind::ImportCsv,
        )
        .await
    {
        Ok(()) => {
            store.csv_committed(count);
            Ok(count)
        }
        Err(error) => {
            if store
                .csv_execution()
                .is_some_and(|source| source.is_fenced())
            {
                return Err(error);
            }
            store.csv_rollback(session, end).await?;
            Err(error)
        }
    }
}

struct SkipIngest<'a> {
    session: SessionId,
    connection: ConnectionId,
    engine: Engine,
    table: &'a str,
    target_types: &'a [String],
    resume_from_row: u64,
    quarantine: bool,
}

async fn ingest_skip(
    store: &SessionStore,
    prepared: &PreparedCsv,
    options: SkipIngest<'_>,
) -> ApiResult<(u64, u64, Vec<sift_protocol::CsvQuarantinedRow>)> {
    let SkipIngest {
        session,
        connection,
        engine,
        table,
        target_types,
        resume_from_row,
        quarantine,
    } = options;
    let column_sql = prepared
        .columns
        .iter()
        .map(|column| quote_ident(&column.name, engine))
        .collect::<Vec<_>>()
        .join(", ");
    let mut inserted = 0u64;
    let mut skipped = 0u64;
    let mut quarantined = Vec::new();
    for (index, record) in prepared
        .records
        .iter()
        .enumerate()
        .skip(resume_from_row as usize)
    {
        let mut params = Vec::new();
        let values = record
            .iter()
            .zip(target_types)
            .map(|(value, target_type)| match value {
                None => "NULL".to_string(),
                Some(value) => {
                    params.push(Value::Text(value.clone()));
                    cast_placeholder(engine, params.len(), target_type)
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        let insert = format!("INSERT INTO {table} ({column_sql}) VALUES ({values})");
        let sql = match engine {
            Engine::Postgres | Engine::Sqlite => format!("{insert} ON CONFLICT DO NOTHING"),
            Engine::SqlServer => format!(
                "BEGIN TRY {insert}; SELECT CAST(1 AS bigint) AS sift_inserted; END TRY BEGIN CATCH IF ERROR_NUMBER() IN (2601, 2627) SELECT CAST(0 AS bigint) AS sift_inserted; ELSE THROW; END CATCH"
            ),
        };
        let response = match store
            .csv_execute(session, execute_request(connection, sql, params), false)
            .await
        {
            Ok(response) => response,
            Err(error) if quarantine => {
                skipped += 1;
                let reason = quarantine_reason(error)?;
                store.csv_autocommit(false, 0);
                quarantined.push(sift_protocol::CsvQuarantinedRow {
                    row_number: index as u64,
                    reason,
                    values: record.clone(),
                });
                continue;
            }
            Err(error) => return Err(error),
        };
        let count = match engine {
            Engine::Postgres | Engine::Sqlite => store.csv_count(response.affected_rows)?,
            Engine::SqlServer => store.csv_count(
                response
                    .rows
                    .first()
                    .and_then(|row| row.values.first())
                    .and_then(value_i64)
                    .and_then(|count| u64::try_from(count).ok()),
            )?,
        };
        if count > 1 {
            return Err(ApiError::Driver(DriverError::new(
                Code::UnsupportedResultShape,
                "CSV row count exceeds one",
            )));
        }
        store.csv_autocommit(false, count);
        let did_insert = count == 1;
        if did_insert {
            inserted += 1;
        } else {
            skipped += 1;
            if quarantine {
                quarantined.push(sift_protocol::CsvQuarantinedRow {
                    row_number: index as u64,
                    reason: "constraint conflict".into(),
                    values: record.clone(),
                });
            }
        }
    }
    Ok((inserted, skipped, quarantined))
}

fn quarantine_reason(error: ApiError) -> ApiResult<String> {
    if let ApiError::Driver(driver) = &error {
        let native = driver.native_code.as_deref().unwrap_or_default();
        let row_error = driver.code == Code::InvalidParameterValue
            || (driver.provider_id == Some(Engine::Sqlite.provider_id())
                && native.parse::<i32>().is_ok_and(|code| code & 255 == 19))
            || (native.len() == 5
                && native.bytes().all(|byte| byte.is_ascii_digit())
                && (native.starts_with("22") || native.starts_with("23")))
            || matches!(
                native,
                "245" | "515" | "547" | "8114" | "8115" | "8152" | "2628" | "2601" | "2627"
            );
        if row_error {
            return Ok("row rejected: invalid value or constraint violation".into());
        }
    }
    // Infrastructure, authorization, cancellation and transaction failures must
    // stop the transfer, not silently turn the remaining source into rejects.
    Err(error)
}

fn cast_placeholder(engine: Engine, index: usize, target_type: &str) -> String {
    match engine {
        Engine::Postgres => format!("CAST(${index} AS text)::{target_type}"),
        Engine::SqlServer => format!("CAST(@P{index} AS {target_type})"),
        Engine::Sqlite => format!("?{index}"),
    }
}

async fn target_column_types(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    table: &str,
    prepared: &PreparedCsv,
) -> ApiResult<Vec<String>> {
    let path = table_path(table)?;
    let snapshot = store
        .schema_cached(session, connection, SchemaScope::deep(path.clone()))
        .await?;
    let snapshot = snapshot.snapshot.as_ref();
    let object = snapshot
        .trees
        .iter()
        .filter(|catalog| {
            path.catalog
                .as_ref()
                .map_or(true, |name| &catalog.name == name)
        })
        .flat_map(|catalog| &catalog.schemas)
        .filter(|schema| {
            path.schema
                .as_ref()
                .map_or(true, |name| &schema.name == name)
        })
        .flat_map(|schema| &schema.objects)
        .find(|object| object.name == path.name)
        .ok_or_else(|| {
            ApiError::Driver(DriverError::new(
                Code::UndefinedObject,
                format!("CSV target table `{table}` was not found"),
            ))
        })?;
    let engine = store.conn_entry(session, connection)?.driver.engine();
    prepared
        .columns
        .iter()
        .map(|csv_column| {
            object
                .columns
                .iter()
                .find(|column| column.name == csv_column.name)
                .map(|column| crate::ddl::type_to_sql(&column.type_ref, engine))
                .ok_or_else(|| {
                    ApiError::Driver(DriverError::new(
                        Code::UndefinedObject,
                        format!(
                            "CSV column `{}` does not exist on target table `{table}`",
                            csv_column.name
                        ),
                    ))
                })
        })
        .collect()
}

pub(crate) fn table_path(table: &str) -> ApiResult<ObjectPath> {
    let parts = table.split('.').collect::<Vec<_>>();
    if parts.is_empty() || parts.len() > 3 || parts.iter().any(|part| part.trim().is_empty()) {
        return Err(ApiError::BadRequest(
            "table must be `table`, `schema.table`, or `database.schema.table`".into(),
        ));
    }
    let (catalog, schema, name) = match parts.as_slice() {
        [name] => (None, None, *name),
        [schema, name] => (None, Some((*schema).to_string()), *name),
        [catalog, schema, name] => (
            Some((*catalog).to_string()),
            Some((*schema).to_string()),
            *name,
        ),
        _ => unreachable!(),
    };
    Ok(ObjectPath {
        catalog,
        schema,
        name: name.to_string(),
        kind: Some(ObjectKind::Table),
        routine_args: None,
    })
}

fn qualified_table(table: &str, engine: Engine) -> ApiResult<String> {
    let parts = table.split('.').collect::<Vec<_>>();
    if parts.is_empty()
        || parts.len() > 3
        || parts
            .iter()
            .any(|part| part.trim().is_empty() || part.contains('\0'))
    {
        return Err(ApiError::BadRequest(
            "table must be `table`, `schema.table`, or `database.schema.table`".into(),
        ));
    }
    Ok(parts
        .into_iter()
        .map(|part| quote_ident(part, engine))
        .collect::<Vec<_>>()
        .join("."))
}

pub(crate) fn execute_request(
    connection: ConnectionId,
    sql: String,
    params: Vec<Value>,
) -> ExecuteRequestHttp {
    ExecuteRequestHttp {
        connection,
        sql,
        params,
        tx: None,
        room_id: None,
        connection_profile_id: None,
        transform: None,
        source: None,
    }
}

fn value_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Int16(value) => Some(i64::from(*value)),
        Value::Int32(value) => Some(i64::from(*value)),
        Value::Int64(value) => Some(*value),
        _ => None,
    }
}

fn csv_error(error: csv::Error) -> ApiError {
    ApiError::BadRequest(format!("invalid CSV: {error}"))
}

fn missing_ext(engine: Engine) -> DriverError {
    DriverError::new(
        Code::UnsupportedForEngine,
        format!("{engine} import extension is not registered"),
    )
    .with_engine(engine)
}

async fn import_atomic(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    request: CsvImportRequest,
    prepared: PreparedCsv,
    table: String,
    engine: Engine,
) -> ApiResult<CsvImportResponse> {
    use sift_protocol::{
        BeginTransactionRequest, EndTransactionRequest, IsolationLevel, OperationKind, TxHandleRef,
        TxMode,
    };
    if request.conflict_policy == CsvConflictPolicy::Quarantine {
        for operation in [
            OperationKind::Savepoint,
            OperationKind::RollbackToSavepoint,
            OperationKind::ReleaseSavepoint,
        ] {
            store.authorize_connection_operation(session, connection, operation, None, &[])?;
        }
    }
    let transaction = store
        .begin_transaction_as(
            session,
            BeginTransactionRequest {
                connection,
                mode: TxMode {
                    isolation: if engine == Engine::Sqlite {
                        IsolationLevel::Serializable
                    } else {
                        IsolationLevel::ReadCommitted
                    },
                    ..Default::default()
                },
            },
            OperationKind::ImportCsv,
        )
        .await?;
    store.csv_transaction(transaction.tx_id);
    let tx = Some(TxHandleRef {
        tx_id: transaction.tx_id,
        connection,
        mode: transaction.mode,
    });
    let work = async {
        if request.create_table {
            let mut command = execute_request(
                connection,
                create_table_sql(&table, &prepared.columns, engine, &request.type_mappings),
                vec![],
            );
            command.tx = tx.clone();
            store.csv_execute(session, command, true).await?;
        }
        let column_sql = prepared
            .columns
            .iter()
            .map(|column| quote_ident(&column.name, engine))
            .collect::<Vec<_>>()
            .join(", ");
        let width = prepared.columns.len();
        if width == 0 || width > 2000 {
            return Err(ApiError::BadRequest(
                "CSV atomic column count must be 1..2000".into(),
            ));
        }
        let mut inserted = 0u64;
        let mut quarantined = Vec::new();
        let quarantine = request.conflict_policy == CsvConflictPolicy::Quarantine;
        let batch_size = if quarantine {
            1
        } else {
            ((if engine == Engine::Sqlite {
                32766
            } else {
                2000
            }) / width)
                .min(128)
        };
        for (batch_index, batch) in prepared.records[request.resume_from_row as usize..]
            .chunks(batch_size)
            .enumerate()
        {
            let mut params = Vec::with_capacity(batch.len() * width);
            let mut groups = Vec::new();
            for record in batch {
                let mut slots = Vec::new();
                for (value, column) in record.iter().zip(&prepared.columns) {
                    if engine == Engine::SqlServer {
                        if let Some(value) = value {
                            params.push(Value::Text(value.clone()));
                            slots.push(format!("@P{}", params.len()));
                        } else {
                            slots.push("NULL".into());
                        }
                        continue;
                    }
                    params.push(match value {
                        None => Value::Null,
                        Some(value) if column.inferred_type == InferredCsvType::Boolean => {
                            match value.to_ascii_lowercase().as_str() {
                                "true" => Value::Bool(true),
                                "false" => Value::Bool(false),
                                _ => Value::Text(value.clone()),
                            }
                        }
                        Some(value) => Value::Text(value.clone()),
                    });
                    slots.push(format!("?{}", params.len()));
                }
                groups.push(format!("({})", slots.join(", ")));
            }
            let suffix = if request.conflict_policy != CsvConflictPolicy::Abort {
                " ON CONFLICT DO NOTHING"
            } else {
                ""
            };
            let insert = format!(
                "INSERT INTO {table} ({column_sql}) VALUES {}{suffix}",
                groups.join(", ")
            );
            // A returned native row-count witness remains available under NOCOUNT.
            let sql = if engine == Engine::SqlServer {
                format!("BEGIN {insert}; SELECT CAST(@@ROWCOUNT AS bigint) AS sift_inserted; END")
            } else {
                insert
            };
            let mut command = execute_request(connection, sql, params);
            command.tx = tx.clone();
            let savepoint = sift_protocol::SavepointRequest {
                connection,
                tx_id: transaction.tx_id,
                name: "sift_quarantine_row".into(),
            };
            if quarantine {
                store.create_savepoint(session, savepoint.clone()).await?;
            }
            let response = store.csv_execute(session, command, false).await;
            let (count, reason) = match response {
                Ok(response) => (
                    if engine == Engine::SqlServer {
                        store.csv_count(
                            response
                                .rows
                                .first()
                                .and_then(|row| row.values.first())
                                .and_then(value_i64)
                                .and_then(|count| u64::try_from(count).ok()),
                        )?
                    } else {
                        store.csv_count(response.affected_rows)?
                    },
                    "constraint conflict".to_owned(),
                ),
                Err(error) if quarantine => {
                    let reason = quarantine_reason(error)?;
                    store
                        .rollback_to_savepoint(session, savepoint.clone())
                        .await?;
                    (0, reason)
                }
                Err(error) => return Err(error),
            };
            if count > batch.len() as u64
                || (request.conflict_policy == CsvConflictPolicy::Abort
                    && count != batch.len() as u64)
            {
                return Err(ApiError::Driver(DriverError::new(
                    Code::UnsupportedResultShape,
                    "CSV batch did not report the required input row count",
                )));
            }
            inserted = inserted
                .checked_add(count)
                .ok_or_else(|| ApiError::Internal("CSV affected count overflow".into()))?;
            if quarantine {
                store.release_savepoint(session, savepoint).await?;
            }
            if quarantine && count == 0 {
                quarantined.push(sift_protocol::CsvQuarantinedRow {
                    row_number: request.resume_from_row + batch_index as u64,
                    reason,
                    values: batch[0].clone(),
                });
            }
        }
        Ok::<_, ApiError>((inserted, quarantined))
    }
    .await;
    let end = EndTransactionRequest {
        connection,
        tx_id: transaction.tx_id,
    };
    let (inserted, quarantined_rows) = match work {
        Ok(count) => match store
            .commit_transaction_as(session, end.clone(), OperationKind::ImportCsv)
            .await
        {
            Ok(()) => {
                store.csv_committed(count.0);
                count
            }
            Err(error) => {
                if store
                    .csv_execution()
                    .is_some_and(|source| source.is_fenced())
                {
                    return Err(error);
                }
                store.csv_rollback(session, end).await?;
                return Err(error);
            }
        },
        Err(error) => {
            store.csv_rollback(session, end).await?;
            return Err(error);
        }
    };
    let total = prepared.records.len() as u64;
    Ok(CsvImportResponse {
        table: request.table,
        columns: prepared.columns,
        table_created: request.create_table,
        rows_inserted: inserted,
        rows_skipped: total - request.resume_from_row - inserted,
        rows_validated: total,
        resume_from_row: total,
        dry_run: false,
        quarantined_rows,
        sql_server_review: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn atomic_csv_counts_and_terminal_uncertainty_keep_both_phases_truthful() {
        use sift_driver_api::mock::MockDriver;
        use sift_protocol::{ChangeLedgerOutcome, ConnectionSpec, OpenSessionRequest, Page};
        for mode in ["known", "missing", "rollback-fails", "commit-fails"] {
            let count = if mode == "known" || mode == "commit-fails" {
                Some(1)
            } else {
                None
            };
            let builder = MockDriver::builder()
                .execute_ok(vec![Page::Done {
                    affected_rows: Some(0),
                    warnings: vec![],
                }])
                .execute_ok(vec![Page::Done {
                    affected_rows: count,
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
            let bound = store
                .bind_csv_source(
                    session,
                    connection,
                    store.conn_entry(session, connection).unwrap(),
                )
                .unwrap();
            let request = request("id,name\n1,private-grid-value\n");
            let prepared = prepare(&request).unwrap();
            // Exercise the atomic orchestrator with the ordinary Driver mock;
            // native SQLite SQL and batch/DDL rollback are covered in sqlite_provider.
            let result = import_atomic(
                &bound,
                session,
                connection,
                request,
                prepared,
                "users".into(),
                Engine::Sqlite,
            )
            .await;
            let observation = bound.csv_execution().unwrap().observation();
            assert!(observation.transaction_id.is_some());
            let expected = match mode {
                "known" => {
                    assert_eq!(result.unwrap().rows_inserted, 1);
                    ChangeLedgerOutcome::Committed
                }
                "missing" => {
                    assert!(matches!(
                        result,
                        Err(ApiError::Driver(DriverError {
                            code: Code::UnsupportedResultShape,
                            ..
                        }))
                    ));
                    ChangeLedgerOutcome::RolledBack
                }
                _ => {
                    assert!(result.is_err());
                    ChangeLedgerOutcome::OutcomeUnknown
                }
            };
            assert_eq!(observation.data.outcome, expected);
            assert_eq!(observation.ddl.outcome, expected);
            assert_eq!(
                bound.csv_execution().unwrap().is_fenced(),
                mode == "rollback-fails" || mode == "commit-fails"
            );
        }
    }

    #[test]
    fn quarantine_sanitizes_row_errors_but_preserves_fatal_failures() {
        let error = DriverError::new(Code::InvalidParameterValue, "password=do-not-report");
        assert_eq!(
            quarantine_reason(error.into()).unwrap(),
            "row rejected: invalid value or constraint violation"
        );
        assert!(
            quarantine_reason(DriverError::new(Code::QueryCanceled, "canceled").into()).is_err()
        );
        assert!(quarantine_reason(ApiError::Forbidden("policy".into())).is_err());
    }

    fn request(data: &str) -> CsvImportRequest {
        CsvImportRequest {
            table: "public.people".into(),
            data: data.as_bytes().to_vec(),
            header: true,
            delimiter: ',',
            null_value: Some("NULL".into()),
            create_table: true,
            conflict_policy: CsvConflictPolicy::Abort,
            dry_run: false,
            resume_from_row: 0,
            type_mappings: Default::default(),
            sql_server: None,
        }
    }

    #[test]
    fn infers_types_and_nullability() {
        let prepared = prepare(&request(
            "id,active,balance,born,seen,name\n1,true,1.25,2024-01-01,2024-01-01T12:00:00Z,Alice\n2,false,NULL,2024-01-02,2024-01-02T12:00:00Z,Bob\n",
        ))
        .unwrap();
        assert_eq!(prepared.columns[0].inferred_type, InferredCsvType::Int64);
        assert_eq!(prepared.columns[1].inferred_type, InferredCsvType::Boolean);
        assert_eq!(prepared.columns[2].inferred_type, InferredCsvType::Decimal);
        assert!(prepared.columns[2].nullable);
        assert_eq!(prepared.columns[3].inferred_type, InferredCsvType::Date);
        assert_eq!(
            prepared.columns[4].inferred_type,
            InferredCsvType::TimestampTz
        );
        assert_eq!(prepared.columns[5].inferred_type, InferredCsvType::Text);
    }

    #[test]
    fn expression_text_columns_preserve_numeric_looking_values() {
        let request = request("id,computed\n1,001\n2,NULL\n");
        let columns = BTreeSet::from(["computed".into()]);
        let prepared = prepare_text(&request, &columns).unwrap();
        assert_eq!(prepared.columns[0].inferred_type, InferredCsvType::Int64);
        assert_eq!(prepared.columns[1].inferred_type, InferredCsvType::Text);
        assert!(prepared.columns[1].nullable);
        assert_eq!(prepared.records[0][1].as_deref(), Some("001"));
        assert!(prepare_text(&request, &BTreeSet::from(["missing".into()])).is_err());
    }

    #[test]
    fn rejects_duplicate_headers_and_ragged_rows() {
        assert!(prepare(&request("id,id\n1,2\n")).is_err());
        assert!(prepare(&request("id,name\n1\n")).is_err());
    }

    #[test]
    fn skip_placeholders_bind_text_then_cast_to_target_type() {
        assert_eq!(
            cast_placeholder(Engine::Postgres, 2, "numeric"),
            "CAST($2 AS text)::numeric"
        );
        assert_eq!(
            cast_placeholder(Engine::SqlServer, 2, "decimal(38,10)"),
            "CAST(@P2 AS decimal(38,10))"
        );
    }

    #[test]
    fn explicit_type_mappings_are_bounded_and_identifier_scoped() {
        let mut request = request("id,name\n1,Alice\n");
        let prepared = prepare(&request).unwrap();
        request
            .type_mappings
            .insert("id".into(), "numeric(20,0)".into());
        validate_type_mappings(&request, &prepared, Engine::Postgres).unwrap();
        let ddl = create_table_sql(
            "public.people",
            &prepared.columns,
            Engine::Postgres,
            &request.type_mappings,
        );
        assert!(ddl.contains("\"id\" numeric(20,0)"));
        assert_eq!(
            created_column_types(&prepared.columns, Engine::Postgres, &request.type_mappings),
            ["numeric(20,0)", "text"]
        );

        for (engine, sql_type) in [
            (Engine::Postgres, "double precision"),
            (Engine::Postgres, "timestamp(6) with time zone"),
            (Engine::Postgres, "\"app\".\"Status\"[]"),
            (Engine::SqlServer, "nvarchar(max)"),
            (Engine::SqlServer, "decimal(38,10)"),
            (Engine::SqlServer, "[dbo].[Status]"),
        ] {
            request.type_mappings.insert("id".into(), sql_type.into());
            assert!(
                validate_type_mappings(&request, &prepared, engine).is_ok(),
                "{engine:?}: {sql_type}"
            );
        }
        for sql_type in [
            "int DEFAULT 1",
            "int, extra int",
            "int CHECK (id > 0)",
            "int NULL",
            "int) SELECT 1",
        ] {
            request.type_mappings.insert("id".into(), sql_type.into());
            for engine in [Engine::Postgres, Engine::SqlServer] {
                assert!(
                    validate_type_mappings(&request, &prepared, engine).is_err(),
                    "{engine:?}: {sql_type}"
                );
            }
        }
        request.type_mappings.clear();

        request
            .type_mappings
            .insert("name".into(), "text; DROP TABLE people".into());
        assert!(validate_type_mappings(&request, &prepared, Engine::Postgres).is_err());
        request.type_mappings.clear();
        request
            .type_mappings
            .insert("missing".into(), "text".into());
        assert!(validate_type_mappings(&request, &prepared, Engine::Postgres).is_err());
    }
}
