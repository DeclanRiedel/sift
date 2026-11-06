//! Bounded, schema-driven Parquet encoding. No JSON intermediary.
use sha2::Digest;
use std::{io, sync::Arc};

use arrow_array::{
    ArrayRef, BinaryArray, BooleanArray, Float32Array, Float64Array, Int16Array, Int32Array,
    Int64Array, RecordBatch, StringArray,
};
use arrow_schema::{DataType, Field, Schema};
use parquet::arrow::ArrowWriter;
use sift_protocol::{ColumnMetadata, PrimitiveType, Row, TypeRef, Value};

use crate::error::{ApiError, ApiResult};

const LIMIT: usize = 64 * 1024 * 1024;

#[derive(Default)]
struct LimitedBuffer(Vec<u8>);

impl io::Write for LimitedBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) > LIMIT {
            return Err(io::Error::other("Parquet export exceeds 64 MiB"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn invalid() -> ApiError {
    ApiError::BadRequest("unsupported, invalid, or oversized Parquet input".into())
}

struct Decoded {
    fields: Vec<Arc<Field>>,
    rows: Vec<Row>,
}

fn decode(
    data: Vec<u8>,
    selection: Option<sift_core::transfer_columns::Selection>,
) -> ApiResult<Decoded> {
    decode_limited(data, selection, LIMIT)
}
fn decode_limited(
    data: Vec<u8>,
    selection: Option<sift_core::transfer_columns::Selection>,
    output_limit: usize,
) -> ApiResult<Decoded> {
    decode_bytes(bytes::Bytes::from(data), selection, output_limit)
}
fn decode_bytes(
    data: bytes::Bytes,
    selection: Option<sift_core::transfer_columns::Selection>,
    output_limit: usize,
) -> ApiResult<Decoded> {
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    if data.len() > LIMIT {
        return Err(invalid());
    }
    let builder = ParquetRecordBatchReaderBuilder::try_new(data).map_err(|_| invalid())?;
    let source_fields = builder.schema().fields().to_vec();
    let mut names = std::collections::HashSet::new();
    if source_fields.is_empty()
        || source_fields.len() > 2000
        || source_fields.iter().any(|field| {
            field.name().is_empty() || field.name().contains('\0') || !names.insert(field.name())
        })
    {
        return Err(invalid());
    }
    let columns = match selection {
        Some(selection) => selection
            .resolve(
                &source_fields
                    .iter()
                    .map(|field| field.name().clone())
                    .collect::<Vec<_>>(),
            )
            .map_err(|message| ApiError::BadRequest(message.into()))?,
        None => source_fields
            .iter()
            .enumerate()
            .map(|(index, field)| sift_core::transfer_columns::Column {
                index,
                target: field.name().clone(),
                expression: None,
            })
            .collect(),
    };
    // Check repeated schema metadata before cloning it for duplicated outputs.
    let metadata_sizes = source_fields
        .iter()
        .map(|field| {
            field.metadata().iter().fold(0usize, |bytes, (key, value)| {
                bytes.saturating_add(key.len()).saturating_add(value.len())
            })
        })
        .collect::<Vec<_>>();
    let mut field_bytes = 0usize;
    for column in &columns {
        let metadata_bytes = if column
            .expression
            .as_ref()
            .is_some_and(|expression| expression.direct_column().is_none())
        {
            0
        } else {
            metadata_sizes[column.index]
        };
        field_bytes = field_bytes
            .saturating_add(column.target.len())
            .saturating_add(metadata_bytes);
        if field_bytes > output_limit {
            return Err(invalid());
        }
    }
    let fields = columns
        .iter()
        .map(|column| {
            if column
                .expression
                .as_ref()
                .is_some_and(|expression| expression.direct_column().is_none())
            {
                Arc::new(Field::new(&column.target, DataType::Utf8, true))
            } else {
                Arc::new(
                    source_fields[column.index]
                        .as_ref()
                        .clone()
                        .with_name(column.target.clone()),
                )
            }
        })
        .collect::<Vec<_>>();
    let mut selected = std::collections::BTreeSet::new();
    for column in &columns {
        column.references(&mut selected);
    }
    let selected = selected.into_iter().collect::<Vec<_>>();
    if selected.iter().any(|index| {
        !matches!(
            source_fields[*index].data_type(),
            DataType::Boolean
                | DataType::Int16
                | DataType::Int32
                | DataType::Int64
                | DataType::Float32
                | DataType::Float64
                | DataType::Utf8
                | DataType::Binary
        )
    }) {
        return Err(invalid());
    }
    let mut size = 0u64;
    for group in builder.metadata().row_groups() {
        size = size
            .checked_add(u64::try_from(group.total_byte_size()).map_err(|_| invalid())?)
            .ok_or_else(invalid)?;
    }
    let row_count =
        u64::try_from(builder.metadata().file_metadata().num_rows()).map_err(|_| invalid())?;
    if size > LIMIT as u64
        || row_count
            .saturating_mul(source_fields.len() as u64)
            .saturating_mul(32)
            > LIMIT as u64
    {
        return Err(invalid());
    }
    let mut rows = Vec::new();
    let mut output_size = field_bytes;
    let mut budget = sift_core::transfer_expressions::Budget::default();
    if selected.is_empty() {
        for _ in 0..row_count {
            append_expression_row(
                &columns,
                &[],
                &mut rows,
                &mut output_size,
                output_limit,
                &mut budget,
            )?;
        }
        return Ok(Decoded { fields, rows });
    }
    let projection =
        parquet::arrow::ProjectionMask::roots(builder.parquet_schema(), selected.iter().copied());
    let reader = builder
        .with_projection(projection)
        .with_batch_size(128)
        .build()
        .map_err(|_| invalid())?;
    let mut source_size = 0usize;
    for batch in reader {
        let batch = batch.map_err(|_| invalid())?;
        if batch.num_columns() != selected.len()
            || selected.iter().enumerate().any(|(position, index)| {
                batch.schema().field(position).name() != source_fields[*index].name()
                    || batch.column(position).data_type() != source_fields[*index].data_type()
            })
        {
            return Err(invalid());
        }
        for index in 0..batch.num_rows() {
            let mut source = vec![Value::Null; source_fields.len()];
            for (position, original) in selected.iter().enumerate() {
                let array = batch.column(position);
                use arrow_array::Array;
                let variable = if array.is_null(index) {
                    0
                } else {
                    match array.data_type() {
                        DataType::Utf8 => array
                            .as_any()
                            .downcast_ref::<StringArray>()
                            .ok_or_else(invalid)?
                            .value(index)
                            .len(),
                        DataType::Binary => array
                            .as_any()
                            .downcast_ref::<BinaryArray>()
                            .ok_or_else(invalid)?
                            .value(index)
                            .len(),
                        _ => 0,
                    }
                };
                source_size = source_size.saturating_add(32).saturating_add(variable);
                if source_size > LIMIT {
                    return Err(invalid());
                }
                macro_rules! value {
                    ($array:ty, $variant:ident) => {
                        Value::$variant(
                            array
                                .as_any()
                                .downcast_ref::<$array>()
                                .ok_or_else(invalid)?
                                .value(index)
                                .to_owned(),
                        )
                    };
                }
                source[*original] = if array.is_null(index) {
                    Value::Null
                } else {
                    match array.data_type() {
                        DataType::Boolean => value!(BooleanArray, Bool),
                        DataType::Int16 => value!(Int16Array, Int16),
                        DataType::Int32 => value!(Int32Array, Int32),
                        DataType::Int64 => value!(Int64Array, Int64),
                        DataType::Float32 => value!(Float32Array, Float32),
                        DataType::Float64 => value!(Float64Array, Float64),
                        DataType::Utf8 => value!(StringArray, Text),
                        DataType::Binary => value!(BinaryArray, Blob),
                        _ => return Err(invalid()),
                    }
                };
            }
            append_expression_row(
                &columns,
                &source,
                &mut rows,
                &mut output_size,
                output_limit,
                &mut budget,
            )?;
        }
    }
    Ok(Decoded { fields, rows })
}

fn append_expression_row(
    columns: &[sift_core::transfer_columns::Column],
    source: &[Value],
    rows: &mut Vec<Row>,
    size: &mut usize,
    limit: usize,
    budget: &mut sift_core::transfer_expressions::Budget,
) -> ApiResult<()> {
    let mut values = Vec::with_capacity(columns.len());
    for column in columns {
        let value = column
            .evaluate(source, budget)
            .map_err(|message| ApiError::BadRequest(message.into()))?;
        let variable = match &value {
            Value::Text(value) => value.len(),
            Value::Blob(value) => value.len(),
            _ => 0,
        };
        *size = size.saturating_add(32).saturating_add(variable);
        if *size > limit {
            return Err(invalid());
        }
        values.push(value);
    }
    rows.push(Row { values });
    Ok(())
}

fn sql_type(data_type: &DataType, engine: sift_protocol::Engine) -> &'static str {
    use sift_protocol::Engine;
    match (data_type, engine) {
        (DataType::Boolean, Engine::Postgres) => "boolean",
        (DataType::Boolean, Engine::SqlServer) => "bit",
        (DataType::Boolean, Engine::Sqlite) => "integer",
        (DataType::Int16, _) => "smallint",
        (DataType::Int32, _) => "integer",
        (DataType::Int64, _) => "bigint",
        (DataType::Float32, _) => "real",
        (DataType::Float64, Engine::Postgres) => "double precision",
        (DataType::Float64, _) => "float",
        (DataType::Binary, Engine::Postgres) => "bytea",
        (DataType::Binary, Engine::SqlServer) => "varbinary(max)",
        (DataType::Binary, Engine::Sqlite) => "blob",
        (_, Engine::SqlServer) => "nvarchar(max)",
        _ => "text",
    }
}

pub(crate) fn preview_source(
    data: Vec<u8>,
    selection: Option<sift_core::transfer_columns::Selection>,
) -> ApiResult<crate::transfer::source_preview::Contents> {
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use sift_protocol::{TransferSampleRepresentation, TransferSourceColumn};
    let data = bytes::Bytes::from(data);
    let builder = ParquetRecordBatchReaderBuilder::try_new(data.clone()).map_err(|_| invalid())?;
    let describe = |field: &Field| {
        let kind = match field.data_type() {
            DataType::Boolean => Some("boolean"),
            DataType::Int16 => Some("int16"),
            DataType::Int32 => Some("int32"),
            DataType::Int64 => Some("int64"),
            DataType::Float32 => Some("float32"),
            DataType::Float64 => Some("float64"),
            DataType::Utf8 => Some("utf8"),
            DataType::Binary => Some("binary"),
            _ => None,
        };
        TransferSourceColumn {
            name: field.name().clone(),
            source_type: kind.unwrap_or("unsupported root").into(),
            nullable: field.is_nullable(),
            selectable: kind.is_some(),
        }
    };
    let columns = builder
        .schema()
        .fields()
        .iter()
        .map(|field| describe(field))
        .collect::<Vec<_>>();
    crate::transfer::source_preview::check_schema(&columns)?;
    let mut names = std::collections::HashSet::new();
    if columns
        .iter()
        .any(|column| column.name.is_empty() || !names.insert(&column.name))
    {
        return Err(invalid());
    }
    let rows =
        u64::try_from(builder.metadata().file_metadata().num_rows()).map_err(|_| invalid())?;
    drop(builder);
    let mapped = decode_bytes(data, selection, LIMIT).map(|decoded| {
        crate::transfer::source_preview::Mapped {
            columns: decoded.fields.iter().map(|field| describe(field)).collect(),
            rows: decoded.rows.into_iter().take(20).collect(),
            representation: TransferSampleRepresentation::TypedParquet,
        }
    });
    Ok(crate::transfer::source_preview::Contents {
        columns,
        rows,
        mapped,
    })
}

pub(crate) async fn import(
    store: &crate::session::SessionStore,
    request: sift_metadata::http::ExecuteTransferRecipeRequest,
    selection: Option<sift_core::transfer_columns::Selection>,
    review_context: &str,
) -> ApiResult<sift_protocol::CsvImportResponse> {
    use sift_protocol::{
        BeginTransactionRequest, CsvConflictPolicy, EndTransactionRequest, Engine,
        InferredCsvColumn, InferredCsvType, OperationKind, TxHandleRef, TxMode,
    };
    let session = request.session_id;
    let connection = request.connection_id;
    let mut target = request
        .table
        .ok_or_else(|| ApiError::BadRequest("import table is required".into()))?;
    target.kind = Some(sift_protocol::ObjectKind::Table);
    target.routine_args = None;
    store.authorize_connection_operation(
        session,
        connection,
        OperationKind::ExecuteTransferRecipe,
        None,
        &[&target],
    )?;
    if !request.type_mappings.is_empty() {
        return Err(ApiError::BadRequest(
            "Parquet uses its typed schema; type overrides are unsupported".into(),
        ));
    }
    if request.resume_from_row != 0
        || request.conflict_policy.unwrap_or_default() != CsvConflictPolicy::Abort
    {
        return Err(ApiError::BadRequest(
            "Parquet import supports atomic abort policy without resume or type overrides".into(),
        ));
    }
    if target.catalog.is_some()
        || target.name.is_empty()
        || target.name.contains('\0')
        || target
            .schema
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.contains('\0'))
    {
        return Err(ApiError::BadRequest(
            "Parquet target requires a table and optional schema, without catalog".into(),
        ));
    }
    let data = request
        .data
        .ok_or_else(|| ApiError::BadRequest("import data is required".into()))?;
    let mut hasher = sha2::Sha256::new();
    hasher.update(review_context.as_bytes());
    hasher.update(&data);
    let digest = format!("{:x}", hasher.finalize());
    let decoded = tokio::task::spawn_blocking(move || decode(data, selection))
        .await
        .map_err(|_| invalid())??;
    let engine = store.conn_entry(session, connection)?.driver.engine();
    sift_core::transfer_columns::validate_provider_targets(
        engine,
        decoded.fields.iter().map(|field| field.name().as_str()),
    )
    .map_err(|message| ApiError::BadRequest(message.into()))?;
    let quote = |s: &str| crate::ddl::quote_ident(s, engine);
    let table = target.schema.as_ref().map_or_else(
        || quote(&target.name),
        |s| format!("{}.{}", quote(s), quote(&target.name)),
    );
    let columns = decoded
        .fields
        .iter()
        .map(|f| InferredCsvColumn {
            name: f.name().clone(),
            nullable: f.is_nullable(),
            inferred_type: match f.data_type() {
                DataType::Boolean => InferredCsvType::Boolean,
                DataType::Int16 | DataType::Int32 | DataType::Int64 => InferredCsvType::Int64,
                DataType::Float32 | DataType::Float64 => InferredCsvType::Decimal,
                _ => InferredCsvType::Text,
            },
        })
        .collect();
    let total = decoded.rows.len() as u64;
    let mut report = sift_protocol::CsvImportResponse {
        table: table.clone(),
        columns,
        table_created: false,
        rows_inserted: 0,
        rows_skipped: 0,
        rows_validated: total,
        resume_from_row: 0,
        dry_run: request.dry_run,
        quarantined_rows: vec![],
        sql_server_review: None,
    };
    if let Some(control) = request.sql_server {
        if request.create_table {
            return Err(ApiError::BadRequest(
                "Reviewed generated columns require an existing target".into(),
            ));
        }
        let input = crate::session::sqlserver_import::Input {
            table: target.schema.as_ref().map_or_else(
                || target.name.clone(),
                |schema| format!("{schema}.{}", target.name),
            ),
            columns: report.columns,
            rows: decoded.rows,
            digest,
            typed: true,
            dry_run: request.dry_run,
        };
        return store
            .reviewed_sqlserver_import(session, connection, input, control)
            .await;
    }
    if request.dry_run {
        return Ok(report);
    }
    let transaction = store
        .begin_transaction_as(
            session,
            BeginTransactionRequest {
                connection,
                mode: TxMode {
                    isolation: sift_protocol::IsolationLevel::Serializable,
                    ..TxMode::default()
                },
            },
            OperationKind::ExecuteTransferRecipe,
        )
        .await?;
    let tx = Some(TxHandleRef {
        tx_id: transaction.tx_id,
        connection,
        mode: transaction.mode,
    });
    let work = async {
        let execute = |sql, params| {
            let mut command = crate::csv_import::execute_request(connection, sql, params);
            command.tx = tx.clone();
            store.execute_http_as(session, command, OperationKind::ExecuteTransferRecipe)
        };
        if request.create_table {
            let definitions = decoded
                .fields
                .iter()
                .map(|f| format!("{} {}", quote(f.name()), sql_type(f.data_type(), engine)))
                .collect::<Vec<_>>()
                .join(", ");
            execute(format!("CREATE TABLE {table} ({definitions})"), vec![]).await?;
        }
        let names = decoded
            .fields
            .iter()
            .map(|f| quote(f.name()))
            .collect::<Vec<_>>()
            .join(", ");
        for row in decoded.rows {
            let mut params = Vec::new();
            let slots = row
                .values
                .into_iter()
                .map(|value| {
                    if value.is_null() {
                        return "NULL".to_string();
                    }
                    params.push(value);
                    match engine {
                        Engine::Postgres => format!("${}", params.len()),
                        Engine::SqlServer => format!("@P{}", params.len()),
                        Engine::Sqlite => format!("?{}", params.len()),
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            execute(
                format!("INSERT INTO {table} ({names}) VALUES ({slots})"),
                params,
            )
            .await?;
        }
        Ok::<_, ApiError>(())
    }
    .await;
    let end = EndTransactionRequest {
        connection,
        tx_id: transaction.tx_id,
    };
    let result = match work {
        Ok(()) => {
            store
                .commit_transaction_as(session, end.clone(), OperationKind::ExecuteTransferRecipe)
                .await
        }
        Err(error) => Err(error),
    };
    if let Err(error) = result {
        if store
            .rollback_transaction_as(session, end, OperationKind::ExecuteTransferRecipe)
            .await
            .is_err()
        {
            let _ = store.close_connection(session, connection).await;
        }
        return Err(error);
    }
    report.rows_inserted = total;
    report.resume_from_row = total;
    report.table_created = request.create_table;
    Ok(report)
}

pub(crate) struct Encoder {
    schema: Arc<Schema>,
    writer: ArrowWriter<LimitedBuffer>,
    decoded_bytes: usize,
}

impl Encoder {
    pub(crate) fn new(columns: &[ColumnMetadata]) -> io::Result<Self> {
        if columns.is_empty() || columns.len() > 2000 {
            return Err(io::Error::other("Parquet requires 1..2000 columns"));
        }
        let mut names = std::collections::HashSet::new();
        if columns.iter().any(|c| !names.insert(&c.name)) {
            return Err(io::Error::other("Parquet requires unique column names"));
        }
        let schema = Arc::new(Schema::new(
            columns
                .iter()
                .map(|column| {
                    let data_type = match &column.type_ref {
                        TypeRef::Primitive(PrimitiveType::Bool) => DataType::Boolean,
                        TypeRef::Primitive(PrimitiveType::Int16) => DataType::Int16,
                        TypeRef::Primitive(PrimitiveType::Int32) => DataType::Int32,
                        TypeRef::Primitive(PrimitiveType::Int64) => DataType::Int64,
                        TypeRef::Primitive(PrimitiveType::Float32) => DataType::Float32,
                        TypeRef::Primitive(PrimitiveType::Float64) => DataType::Float64,
                        TypeRef::Primitive(PrimitiveType::Blob) => DataType::Binary,
                        TypeRef::Native {
                            provider_id, name, ..
                        } if *provider_id == sift_protocol::Engine::Sqlite.provider_id() => {
                            let name = name.to_ascii_uppercase();
                            if name.contains("INT") {
                                DataType::Int64
                            } else if name.contains("REAL")
                                || name.contains("FLOA")
                                || name.contains("DOUB")
                            {
                                DataType::Float64
                            } else if name == "BLOB" {
                                DataType::Binary
                            } else {
                                DataType::Utf8
                            }
                        }
                        _ => DataType::Utf8,
                    };
                    Field::new(&column.name, data_type, true)
                })
                .collect::<Vec<_>>(),
        ));
        let writer = ArrowWriter::try_new(LimitedBuffer::default(), schema.clone(), None)
            .map_err(io::Error::other)?;
        Ok(Self {
            schema,
            writer,
            decoded_bytes: 0,
        })
    }

    pub(crate) fn write(&mut self, rows: &[Row]) -> io::Result<()> {
        if rows
            .iter()
            .any(|row| row.values.len() != self.schema.fields().len())
        {
            return Err(io::Error::other("Parquet row width does not match schema"));
        }
        // A cumulative decoded-size cap also bounds highly compressible inputs.
        for row in rows {
            for value in &row.values {
                self.decoded_bytes = self.decoded_bytes.saturating_add(
                    match value {
                        Value::Blob(v) => v.len(),
                        Value::Text(v) | Value::Decimal(v) => v.len(),
                        _ => crate::export::value_to_text(value).len(),
                    }
                    .saturating_add(16),
                );
                if self.decoded_bytes > LIMIT {
                    return Err(io::Error::other("Parquet decoded data exceeds 64 MiB"));
                }
            }
        }
        let arrays = self
            .schema
            .fields()
            .iter()
            .enumerate()
            .map(|(index, field)| {
                macro_rules! primitive {
                    ($array:ty, $variant:ident) => {{
                        let values = rows
                            .iter()
                            .map(|row| match &row.values[index] {
                                value if value.is_null() => Ok(None),
                                Value::$variant(value) => Ok(Some(*value)),
                                _ => Err(io::Error::other(
                                    "Parquet value does not match declared type",
                                )),
                            })
                            .collect::<io::Result<Vec<_>>>()?;
                        Arc::new(<$array>::from(values)) as ArrayRef
                    }};
                }
                Ok(match field.data_type() {
                    DataType::Boolean => primitive!(BooleanArray, Bool),
                    DataType::Int16 => primitive!(Int16Array, Int16),
                    DataType::Int32 => primitive!(Int32Array, Int32),
                    DataType::Int64 => primitive!(Int64Array, Int64),
                    DataType::Float32 => primitive!(Float32Array, Float32),
                    DataType::Float64 => primitive!(Float64Array, Float64),
                    DataType::Binary => {
                        let values = rows
                            .iter()
                            .map(|row| match &row.values[index] {
                                value if value.is_null() => Ok(None),
                                Value::Blob(value) => Ok(Some(value.as_slice())),
                                _ => Err(io::Error::other(
                                    "Parquet binary value does not match schema",
                                )),
                            })
                            .collect::<io::Result<Vec<_>>>()?;
                        Arc::new(BinaryArray::from(values)) as ArrayRef
                    }
                    _ => Arc::new(StringArray::from_iter(rows.iter().map(|row| {
                        let value = &row.values[index];
                        (!value.is_null()).then(|| crate::export::value_to_text(value))
                    }))) as ArrayRef,
                })
            })
            .collect::<io::Result<Vec<_>>>()?;
        let batch = RecordBatch::try_new(self.schema.clone(), arrays).map_err(io::Error::other)?;
        self.writer.write(&batch).map_err(io::Error::other)?;
        self.writer.flush().map_err(io::Error::other)
    }

    pub(crate) fn finish(self) -> io::Result<bytes::Bytes> {
        Ok(bytes::Bytes::from(
            self.writer.into_inner().map_err(io::Error::other)?.0,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_round_trip_retains_order_nulls_binary_and_precision() {
        let types = [
            PrimitiveType::Text,
            PrimitiveType::Int64,
            PrimitiveType::Bool,
            PrimitiveType::Blob,
            PrimitiveType::Float64,
            PrimitiveType::Decimal,
        ];
        let columns = types
            .into_iter()
            .enumerate()
            .map(|(i, t)| ColumnMetadata::new(format!("col_{i}"), TypeRef::Primitive(t)))
            .collect::<Vec<_>>();
        let rows = vec![
            Row {
                values: vec![
                    Value::Text("NULL\n,é".into()),
                    Value::Int64(i64::MAX),
                    Value::Bool(false),
                    Value::Blob(vec![0, 255, 1]),
                    Value::Float64(1.25),
                    Value::Decimal("12345678901234567890.123456789".into()),
                ],
            },
            Row {
                values: vec![Value::Null; 6],
            },
        ];
        let mut encoder = Encoder::new(&columns).unwrap();
        encoder.write(&rows).unwrap();
        let decoded = decode(encoder.finish().unwrap().to_vec(), None).unwrap();
        let mut expected = rows;
        expected[0].values[5] = Value::Text("12345678901234567890.123456789".into());
        assert_eq!(decoded.rows.len(), expected.len());
        for (actual, expected) in decoded.rows.iter().zip(expected) {
            assert_eq!(actual.values, expected.values);
        }
        assert_eq!(decoded.fields[0].name(), "col_0");
        assert_eq!(decoded.fields[1].data_type(), &DataType::Int64);
        assert_eq!(decoded.fields[3].data_type(), &DataType::Binary);
        let empty = decode(
            Encoder::new(&columns).unwrap().finish().unwrap().to_vec(),
            None,
        )
        .unwrap();
        assert!(empty.rows.is_empty());
        assert_eq!(empty.fields.len(), 6);
    }

    #[test]
    fn rejects_malformed_input_and_inconsistent_rows() {
        assert!(decode(b"PAR1brokenPAR1".to_vec(), None).is_err());
        let column = ColumnMetadata::new("id", TypeRef::Primitive(PrimitiveType::Int64));
        assert!(Encoder::new(&[column.clone(), column.clone()]).is_err());
        let mut encoder = Encoder::new(&[column]).unwrap();
        assert!(encoder.write(&[Row { values: vec![] }]).is_err());
        assert!(encoder
            .write(&[Row {
                values: vec![Value::Text("1".into())]
            }])
            .is_err());
    }
}

#[cfg(test)]
mod projection_tests {
    use super::*;
    use arrow_array::Array;
    fn selection(value: serde_json::Value) -> sift_core::transfer_columns::Selection {
        sift_core::transfer_columns::parse(
            sift_protocol::TransferDirection::Import,
            "parquet",
            "2",
            &serde_json::json!({"column_projection":value}),
        )
        .unwrap()
        .unwrap()
    }
    #[test]
    fn text_expressions_keep_typed_columns_nulls_and_constant_only_row_counts() {
        let mut nested =
            arrow_array::builder::ListBuilder::new(arrow_array::builder::StringBuilder::new());
        for _ in 0..2 {
            nested.values().append_value("DROPPED-PRIVATE");
            nested.append(true);
        }
        let nested = nested.finish();
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, true),
            Field::new("label", DataType::Utf8, true),
            Field::new("private", nested.data_type().clone(), false),
        ]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(Int64Array::from(vec![Some(i64::MAX), None])) as ArrayRef,
                Arc::new(StringArray::from(vec![Some("ß"), None])),
                Arc::new(nested),
            ],
        )
        .unwrap();
        let mut writer = ArrowWriter::try_new(Vec::new(), schema, None).unwrap();
        writer.write(&batch).unwrap();
        let bytes = writer.into_inner().unwrap();
        let options = serde_json::json!({"column_expressions":[
            {"target":"id","expression":{"op":"column","source":"id"}},
            {"target":"label","expression":{"op":"concat","inputs":[{"op":"upper","input":{"op":"column","source":"label"}},{"op":"literal","value":"!"}]}},
            {"target":"literal","expression":{"op":"literal","value":"NULL"}},
            {"target":"fallback","expression":{"op":"coalesce","inputs":[{"op":"column","source":"label"},{"op":"literal","value":""}]}}
        ]});
        let select = sift_core::transfer_columns::parse(
            sift_protocol::TransferDirection::Import,
            "parquet",
            "3",
            &options,
        )
        .unwrap()
        .unwrap();
        let decoded = decode(bytes.clone(), Some(select.clone())).unwrap();
        assert_eq!(decoded.fields[0].data_type(), &DataType::Int64);
        assert_eq!(decoded.fields[1].data_type(), &DataType::Utf8);
        assert!(decoded.fields[1].is_nullable());
        assert_eq!(
            decoded.rows[0].values,
            vec![
                Value::Int64(i64::MAX),
                Value::Text("SS!".into()),
                Value::Text("NULL".into()),
                Value::Text("ß".into())
            ]
        );
        assert_eq!(
            decoded.rows[1].values,
            vec![
                Value::Null,
                Value::Null,
                Value::Text("NULL".into()),
                Value::Text("".into())
            ]
        );
        assert!(decode_limited(bytes.clone(), Some(select), 48).is_err());
        let constants =
            serde_json::json!({"column_expressions":[{"target":"nil","expression":{"op":"null"}}]});
        let constants = sift_core::transfer_columns::parse(
            sift_protocol::TransferDirection::Import,
            "parquet",
            "3",
            &constants,
        )
        .unwrap();
        let decoded = decode(bytes, constants).unwrap();
        assert_eq!(decoded.rows.len(), 2);
        assert!(decoded
            .rows
            .iter()
            .all(|row| row.values == vec![Value::Null]));
    }

    #[test]
    fn typed_projection_preserves_nulls_empty_values_metadata_and_omits_nested_fields() {
        use arrow_array::builder::{ListBuilder, StringBuilder};
        let mut nested = ListBuilder::new(StringBuilder::new());
        nested.values().append_value("omitted-private-nested");
        nested.append(true);
        nested.append(true);
        nested.append(false);
        let nested = nested.finish();
        let id = Field::new("id", DataType::Int64, true).with_metadata(
            std::collections::HashMap::from([("unit".into(), "unchanged".into())]),
        );
        let schema = Arc::new(Schema::new(vec![
            id,
            Field::new("private", nested.data_type().clone(), true),
            Field::new("label", DataType::Utf8, true),
            Field::new("blob", DataType::Binary, true),
        ]));
        let arrays: Vec<ArrayRef> = vec![
            Arc::new(Int64Array::from(vec![Some(i64::MAX), Some(0), None])),
            Arc::new(nested),
            Arc::new(StringArray::from(vec![Some("NULL"), Some(""), None])),
            Arc::new(BinaryArray::from(vec![
                Some(&[0, 255][..]),
                Some(&[][..]),
                None,
            ])),
        ];
        let batch = RecordBatch::try_new(schema.clone(), arrays).unwrap();
        let mut writer = ArrowWriter::try_new(Vec::new(), schema, None).unwrap();
        writer.write(&batch).unwrap();
        let bytes = writer.into_inner().unwrap();
        assert!(
            decode(bytes.clone(), None).is_err(),
            "unselected unsupported nested input remains refused without projection"
        );
        let unavailable = preview_source(bytes.clone(), None).unwrap();
        assert!(!unavailable.columns[1].selectable);
        assert!(unavailable.mapped.is_err());
        let projected=preview_source(bytes.clone(),Some(selection(serde_json::json!([{"source":"label","target":"text"},{"source":"blob","target":"data"},{"source":"id","target":"id"},{"source":"blob","target":"copy"}])))).unwrap();
        assert_eq!(projected.rows, 3);
        let mapped = projected.mapped.unwrap();
        let decoded=decode(bytes,Some(selection(serde_json::json!([{"source":"label","target":"text"},{"source":"blob","target":"data"},{"source":"id","target":"id"},{"source":"blob","target":"copy"}])))).unwrap();
        assert_eq!(
            mapped
                .rows
                .iter()
                .map(|row| &row.values)
                .collect::<Vec<_>>(),
            decoded
                .rows
                .iter()
                .map(|row| &row.values)
                .collect::<Vec<_>>()
        );
        assert_eq!(mapped.columns[2].source_type, "int64");

        assert_eq!(
            decoded
                .fields
                .iter()
                .map(|field| field.name().as_str())
                .collect::<Vec<_>>(),
            vec!["text", "data", "id", "copy"]
        );
        assert_eq!(decoded.fields[2].metadata()["unit"], "unchanged");
        assert_eq!(decoded.fields[1].data_type(), &DataType::Binary);
        assert_eq!(
            decoded.rows[0].values,
            vec![
                Value::Text("NULL".into()),
                Value::Blob(vec![0, 255]),
                Value::Int64(i64::MAX),
                Value::Blob(vec![0, 255])
            ]
        );
        assert_eq!(
            decoded.rows[1].values,
            vec![
                Value::Text("".into()),
                Value::Blob(vec![]),
                Value::Int64(0),
                Value::Blob(vec![])
            ]
        );
        assert_eq!(decoded.rows[2].values, vec![Value::Null; 4]);
    }
    #[test]
    fn copy_expansion_is_refused_before_allocating_past_the_decoded_limit() {
        let columns = [ColumnMetadata::new(
            "blob",
            TypeRef::Primitive(PrimitiveType::Blob),
        )];
        let mut writer = Encoder::new(&columns).unwrap();
        writer
            .write(&[Row {
                values: vec![Value::Blob(vec![7; 600])],
            }])
            .unwrap();
        let data = writer.finish().unwrap().to_vec();
        assert!(decode_limited(data,Some(selection(serde_json::json!([{"source":"blob","target":"one"},{"source":"blob","target":"two"}]))),1024).is_err());
    }
}
