//! Bounded, schema-driven Parquet encoding. No JSON intermediary.
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

fn decode(data: Vec<u8>) -> ApiResult<Decoded> {
    use arrow_array::Array;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    if data.len() > LIMIT {
        return Err(invalid());
    }
    let builder = ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(data))
        .map_err(|_| invalid())?;
    let fields = builder.schema().fields().to_vec();
    let mut names = std::collections::HashSet::new();
    if fields.is_empty()
        || fields.len() > 2000
        || fields.iter().any(|f| {
            f.name().is_empty()
                || f.name().contains('\0')
                || !names.insert(f.name())
                || !matches!(
                    f.data_type(),
                    DataType::Boolean
                        | DataType::Int16
                        | DataType::Int32
                        | DataType::Int64
                        | DataType::Float32
                        | DataType::Float64
                        | DataType::Utf8
                        | DataType::Binary
                )
        })
    {
        return Err(invalid());
    }
    let metadata = builder.metadata();
    let mut size = 0u64;
    for group in metadata.row_groups() {
        let bytes = u64::try_from(group.total_byte_size()).map_err(|_| invalid())?;
        size = size.checked_add(bytes).ok_or_else(invalid)?;
    }
    let row_count = u64::try_from(metadata.file_metadata().num_rows()).map_err(|_| invalid())?;
    if size > LIMIT as u64
        || row_count
            .saturating_mul(fields.len() as u64)
            .saturating_mul(32)
            > LIMIT as u64
    {
        return Err(invalid());
    }
    let reader = builder
        .with_batch_size(128)
        .build()
        .map_err(|_| invalid())?;
    let mut rows = Vec::new();
    let mut decoded_size = 0usize;
    for batch in reader {
        let batch = batch.map_err(|_| invalid())?;
        for index in 0..batch.num_rows() {
            let mut values = Vec::with_capacity(fields.len());
            for array in batch.columns() {
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
                let value = if array.is_null(index) {
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
                decoded_size = decoded_size
                    .saturating_add(32)
                    .saturating_add(match &value {
                        Value::Text(v) => v.len(),
                        Value::Blob(v) => v.len(),
                        _ => 0,
                    });
                if decoded_size > LIMIT {
                    return Err(invalid());
                }
                values.push(value);
            }
            rows.push(Row { values });
        }
    }
    Ok(Decoded { fields, rows })
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

pub(crate) async fn import(
    store: &crate::session::SessionStore,
    request: sift_metadata::http::ExecuteTransferRecipeRequest,
) -> ApiResult<sift_protocol::CsvImportResponse> {
    use sift_protocol::{
        BeginTransactionRequest, CsvConflictPolicy, EndTransactionRequest, Engine,
        InferredCsvColumn, InferredCsvType, OperationKind, TxHandleRef, TxMode,
    };
    let session = request.session_id;
    let connection = request.connection_id;
    let target = request
        .table
        .ok_or_else(|| ApiError::BadRequest("import table is required".into()))?;
    store.authorize_connection_operation(
        session,
        connection,
        OperationKind::ExecuteTransferRecipe,
        None,
        &[&target],
    )?;
    if request.resume_from_row != 0
        || request.conflict_policy.unwrap_or_default() != CsvConflictPolicy::Abort
        || !request.type_mappings.is_empty()
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
    let decoded = tokio::task::spawn_blocking(move || decode(data))
        .await
        .map_err(|_| invalid())??;
    let engine = store.conn_entry(session, connection)?.driver.engine();
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
    };
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
        let decoded = decode(encoder.finish().unwrap().to_vec()).unwrap();
        let mut expected = rows;
        expected[0].values[5] = Value::Text("12345678901234567890.123456789".into());
        assert_eq!(decoded.rows.len(), expected.len());
        for (actual, expected) in decoded.rows.iter().zip(expected) {
            assert_eq!(actual.values, expected.values);
        }
        assert_eq!(decoded.fields[0].name(), "col_0");
        assert_eq!(decoded.fields[1].data_type(), &DataType::Int64);
        assert_eq!(decoded.fields[3].data_type(), &DataType::Binary);
        let empty = decode(Encoder::new(&columns).unwrap().finish().unwrap().to_vec()).unwrap();
        assert!(empty.rows.is_empty());
        assert_eq!(empty.fields.len(), 6);
    }

    #[test]
    fn rejects_malformed_input_and_inconsistent_rows() {
        assert!(decode(b"PAR1brokenPAR1".to_vec()).is_err());
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
