use std::collections::HashSet;

use sift_driver_api::{BulkOp, CopyOp};
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

struct PreparedCsv {
    columns: Vec<InferredCsvColumn>,
    records: Vec<Vec<Option<String>>>,
    delimiter: u8,
}

pub async fn import(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    request: CsvImportRequest,
) -> ApiResult<CsvImportResponse> {
    let target = table_path(&request.table)?;
    store.authorize_connection_operation(
        session,
        connection,
        sift_protocol::OperationKind::ImportCsv,
        None,
        &[&target],
    )?;
    let prepared = prepare(&request)?;
    let entry = store.conn_entry(session, connection)?;
    let engine = entry.driver.engine();
    let table = qualified_table(&request.table, engine)?;

    if request.create_table {
        let ddl = create_table_sql(&table, &prepared.columns, engine);
        store
            .execute_http_as(
                session,
                execute_request(connection, ddl, Vec::new()),
                sift_protocol::OperationKind::ImportCsv,
            )
            .await?;
    }

    let (rows_inserted, rows_skipped) = match request.conflict_policy {
        CsvConflictPolicy::Abort => {
            let rows = ingest_abort(store, entry, &request, &prepared).await?;
            (rows, 0)
        }
        CsvConflictPolicy::Skip => {
            let target_types = if request.create_table {
                prepared
                    .columns
                    .iter()
                    .map(|column| inferred_sql(column.inferred_type, engine).to_string())
                    .collect()
            } else {
                target_column_types(store, session, connection, &request.table, &prepared).await?
            };
            ingest_skip(
                store,
                session,
                connection,
                engine,
                &table,
                &prepared,
                &target_types,
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
    })
}

fn prepare(request: &CsvImportRequest) -> ApiResult<PreparedCsv> {
    if request.data.is_empty() {
        return Err(ApiError::BadRequest("CSV data must not be empty".into()));
    }
    if request.data.len() > MAX_CSV_BYTES {
        return Err(ApiError::Driver(DriverError::new(
            Code::ResultTooLarge,
            "CSV import exceeds the 64 MiB payload limit",
        )));
    }
    if !request.header {
        return Err(ApiError::BadRequest(
            "CSV import requires a header row".into(),
        ));
    }
    if !request.delimiter.is_ascii() {
        return Err(ApiError::BadRequest("CSV delimiter must be ASCII".into()));
    }
    let delimiter = request.delimiter as u8;
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(true)
        .from_reader(request.data.as_slice());
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
                    if request.null_value.as_deref() == Some(field) {
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
        .map(|(index, name)| infer_column(name, index, &records))
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

fn create_table_sql(table: &str, columns: &[InferredCsvColumn], engine: Engine) -> String {
    let definitions = columns
        .iter()
        .map(|column| {
            format!(
                "{} {} {}",
                quote_ident(&column.name, engine),
                inferred_sql(column.inferred_type, engine),
                if column.nullable { "NULL" } else { "NOT NULL" }
            )
        })
        .collect::<Vec<_>>();
    format!("CREATE TABLE {table} ({})", definitions.join(", "))
}

fn inferred_sql(inferred: InferredCsvType, engine: Engine) -> &'static str {
    match (inferred, engine) {
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
    entry: crate::session::ConnectionEntryClone,
    request: &CsvImportRequest,
    prepared: &PreparedCsv,
) -> ApiResult<u64> {
    let driver = entry.driver;
    let handle = entry.handle;
    let table = request.table.clone();
    let data = request.data.clone();
    let columns = prepared
        .columns
        .iter()
        .map(|column| column.name.clone())
        .collect::<Vec<_>>();
    let delimiter = prepared.delimiter;
    let header = request.header;
    let null_value = request.null_value.clone();
    store
        .run_bounded("csv_import", async move {
            match driver.engine() {
                Engine::Postgres => {
                    let pg = driver
                        .as_pg()
                        .ok_or_else(|| missing_ext(Engine::Postgres))?;
                    let result = pg
                        .copy(
                            handle,
                            CopyOp::Import {
                                table,
                                columns,
                                data,
                                delimiter,
                                header,
                                null_value,
                            },
                        )
                        .await?;
                    Ok(result.rows.unwrap_or(0))
                }
                Engine::SqlServer => {
                    let mssql = driver
                        .as_mssql()
                        .ok_or_else(|| missing_ext(Engine::SqlServer))?;
                    let result = mssql
                        .bulk_insert(
                            handle,
                            BulkOp {
                                table,
                                data,
                                delimiter,
                                header,
                                null_value,
                            },
                        )
                        .await?;
                    Ok(result.rows_inserted)
                }
            }
        })
        .await
}

async fn ingest_skip(
    store: &SessionStore,
    session: SessionId,
    connection: ConnectionId,
    engine: Engine,
    table: &str,
    prepared: &PreparedCsv,
    target_types: &[String],
) -> ApiResult<(u64, u64)> {
    let column_sql = prepared
        .columns
        .iter()
        .map(|column| quote_ident(&column.name, engine))
        .collect::<Vec<_>>()
        .join(", ");
    let mut inserted = 0u64;
    let mut skipped = 0u64;
    for record in &prepared.records {
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
            Engine::Postgres => format!("{insert} ON CONFLICT DO NOTHING"),
            Engine::SqlServer => format!(
                "BEGIN TRY {insert}; SELECT CAST(1 AS bigint) AS sift_inserted; END TRY BEGIN CATCH IF ERROR_NUMBER() IN (2601, 2627) SELECT CAST(0 AS bigint) AS sift_inserted; ELSE THROW; END CATCH"
            ),
        };
        let response = store
            .execute_http_as(
                session,
                execute_request(connection, sql, params),
                sift_protocol::OperationKind::ImportCsv,
            )
            .await?;
        let did_insert = match engine {
            Engine::Postgres => response.affected_rows.unwrap_or(0) > 0,
            Engine::SqlServer => {
                response
                    .rows
                    .first()
                    .and_then(|row| row.values.first())
                    .and_then(value_i64)
                    .unwrap_or(0)
                    > 0
            }
        };
        if did_insert {
            inserted += 1;
        } else {
            skipped += 1;
        }
    }
    Ok((inserted, skipped))
}

fn cast_placeholder(engine: Engine, index: usize, target_type: &str) -> String {
    match engine {
        Engine::Postgres => format!("CAST(${index} AS text)::{target_type}"),
        Engine::SqlServer => format!("CAST(@P{index} AS {target_type})"),
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

fn table_path(table: &str) -> ApiResult<ObjectPath> {
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

fn execute_request(
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

#[cfg(test)]
mod tests {
    use super::*;

    fn request(data: &str) -> CsvImportRequest {
        CsvImportRequest {
            table: "public.people".into(),
            data: data.as_bytes().to_vec(),
            header: true,
            delimiter: ',',
            null_value: Some("NULL".into()),
            create_table: true,
            conflict_policy: CsvConflictPolicy::Abort,
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
}
