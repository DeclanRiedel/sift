//! Server-side result export (Phase D).
//!
//! Runs a SQL query on a driver connection and streams the result as
//! bytes in CSV / TSV / JSON Lines / JSON Array format. The row
//! encoder converts `sift_protocol::Value` cells to the target
//! format's textual representation; the transport is HTTP chunked
//! (axum `Body::from_stream`).
//!
//! Not sourced from an existing cursor: the client provides SQL, we
//! run it fresh. This keeps the surface simple and matches the
//! "download to file" ergonomic. Interactive result streaming is
//! already served by the WS `Execute` path.

use bytes::Bytes;
use futures::Stream;
use sift_driver_api::Driver;
use sift_protocol::{ColumnMetadata, DriverError, ExecuteRequest, ExportFormat, Page, Row, Value};

/// Content-Type header value for `format`.
pub fn content_type(format: ExportFormat) -> &'static str {
    match format {
        ExportFormat::Csv => "text/csv; charset=utf-8",
        ExportFormat::Tsv => "text/tab-separated-values; charset=utf-8",
        ExportFormat::JsonLines => "application/x-ndjson",
        ExportFormat::JsonArray => "application/json",
    }
}

/// Run `sql` on the driver and return a byte stream of the encoded
/// export body. Errors during streaming are surfaced through the
/// stream's `Err` yield — the HTTP layer converts the first error
/// into a 500 header if it lands before any bytes are written,
/// otherwise the transfer aborts mid-flight (chunked encoding).
pub async fn run_export(
    driver: std::sync::Arc<dyn Driver>,
    handle: sift_driver_api::ConnHandle,
    sql: String,
    params: Vec<Value>,
    format: ExportFormat,
    header: bool,
    null_display: Option<String>,
) -> Result<impl Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static, DriverError> {
    let stream = driver
        .execute(handle, ExecuteRequest { sql, params })
        .await?;
    let rx = stream.rows;
    let null = null_display.unwrap_or_default();
    Ok(encode_stream(rx, format, header, null))
}

fn encode_stream(
    mut rx: tokio::sync::mpsc::Receiver<Page>,
    format: ExportFormat,
    emit_header: bool,
    null_display: String,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static {
    async_stream::try_stream! {
        let mut columns: Vec<ColumnMetadata> = Vec::new();
        let mut header_sent = false;
        let mut first_row_in_array = true;
        if matches!(format, ExportFormat::JsonArray) {
            yield Bytes::from_static(b"[");
        }
        while let Some(page) = rx.recv().await {
            match page {
                Page::NextResult { columns: cols } => {
                    columns = cols;
                    header_sent = false;
                }
                Page::Rows { rows } => {
                    if columns.is_empty() {
                        // Driver produced rows without a preceding
                        // NextResult. Synthesize headers as col_0,
                        // col_1, ... based on the first row's width.
                        if let Some(first) = rows.first() {
                            columns = (0..first.values.len())
                                .map(synthetic_column)
                                .collect();
                        }
                    }
                    if !header_sent {
                        if matches!(format, ExportFormat::Csv | ExportFormat::Tsv) && emit_header {
                            let bytes = header_line(&columns, format);
                            yield bytes;
                        }
                        header_sent = true;
                    }
                    for row in rows {
                        let bytes = encode_row(
                            &row,
                            &columns,
                            format,
                            &null_display,
                            &mut first_row_in_array,
                        );
                        yield bytes;
                    }
                }
                Page::Done { .. } => break,
                Page::Error { error } => {
                    Err(std::io::Error::other(format!(
                        "{}: {}",
                        error.code, error.message
                    )))?;
                }
            }
        }
        if matches!(format, ExportFormat::JsonArray) {
            yield Bytes::from_static(b"]");
        }
    }
}

fn synthetic_column(idx: usize) -> ColumnMetadata {
    ColumnMetadata {
        name: format!("col_{idx}"),
        type_ref: sift_protocol::TypeRef::Primitive(sift_protocol::PrimitiveType::Text),
        nullable: sift_protocol::Nullability::Nullable,
        auto_increment: false,
        primary_key: false,
        facets: Default::default(),
    }
}

fn header_line(columns: &[ColumnMetadata], format: ExportFormat) -> Bytes {
    let mut out = String::new();
    for (i, col) in columns.iter().enumerate() {
        if i > 0 {
            out.push(delimiter(format));
        }
        match format {
            ExportFormat::Csv => out.push_str(&csv_escape(&col.name)),
            ExportFormat::Tsv => out.push_str(&tsv_escape(&col.name)),
            _ => out.push_str(&col.name),
        }
    }
    out.push('\n');
    Bytes::from(out)
}

fn encode_row(
    row: &Row,
    columns: &[ColumnMetadata],
    format: ExportFormat,
    null_display: &str,
    first_row_in_array: &mut bool,
) -> Bytes {
    match format {
        ExportFormat::Csv | ExportFormat::Tsv => {
            let mut out = String::new();
            for (i, value) in row.values.iter().enumerate() {
                if i > 0 {
                    out.push(delimiter(format));
                }
                let cell = value_to_text(value, null_display);
                let escaped = match format {
                    ExportFormat::Csv => csv_escape(&cell),
                    ExportFormat::Tsv => tsv_escape(&cell),
                    _ => cell,
                };
                out.push_str(&escaped);
            }
            out.push('\n');
            Bytes::from(out)
        }
        ExportFormat::JsonLines => {
            let obj = row_as_json(row, columns);
            let mut s = serde_json::to_string(&obj).unwrap_or_default();
            s.push('\n');
            Bytes::from(s)
        }
        ExportFormat::JsonArray => {
            let obj = row_as_json(row, columns);
            let s = serde_json::to_string(&obj).unwrap_or_default();
            let prefix = if *first_row_in_array {
                *first_row_in_array = false;
                ""
            } else {
                ","
            };
            Bytes::from(format!("{prefix}{s}"))
        }
    }
}

fn delimiter(format: ExportFormat) -> char {
    match format {
        ExportFormat::Tsv => '\t',
        _ => ',',
    }
}

fn value_to_text(v: &Value, null_display: &str) -> String {
    match v {
        Value::Null => null_display.to_string(),
        Value::Bool(b) => (if *b { "true" } else { "false" }).to_string(),
        Value::Int16(i) => i.to_string(),
        Value::Int32(i) => i.to_string(),
        Value::Int64(i) => i.to_string(),
        Value::Float32(f) => f.to_string(),
        Value::Float64(f) => f.to_string(),
        Value::Decimal(s) => s.clone(),
        Value::Text(s) => s.clone(),
        Value::Blob(bytes) => hex_encode(bytes),
        Value::Date(d) => d.to_string(),
        Value::Time(t) => t.to_string(),
        Value::Timestamp(ts) => ts.to_string(),
        Value::TimestampTz(ts) => ts.to_string(),
        Value::Uuid(u) => u.to_string(),
        Value::Json(v) => v.to_string(),
        Value::Interval(_) => format!("{v:?}"),
        Value::Engine { display_text, .. } => display_text.clone(),
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn csv_escape(s: &str) -> String {
    let needs_quote = s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r');
    if !needs_quote {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '"' {
            out.push('"');
        }
        out.push(c);
    }
    out.push('"');
    out
}

fn tsv_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out
}

fn row_as_json(
    row: &Row,
    columns: &[ColumnMetadata],
) -> serde_json::Map<String, serde_json::Value> {
    let mut out = serde_json::Map::with_capacity(columns.len());
    for (i, value) in row.values.iter().enumerate() {
        let key = columns
            .get(i)
            .map(|c| c.name.clone())
            .unwrap_or_else(|| format!("col_{i}"));
        out.insert(key, value_to_json(value));
    }
    out
}

fn value_to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Int16(i) => serde_json::json!(i),
        Value::Int32(i) => serde_json::json!(i),
        Value::Int64(i) => serde_json::json!(i),
        Value::Float32(f) => serde_json::json!(f),
        Value::Float64(f) => serde_json::json!(f),
        Value::Decimal(s) | Value::Text(s) => serde_json::Value::String(s.clone()),
        Value::Blob(b) => serde_json::Value::String(hex_encode(b)),
        Value::Date(d) => serde_json::Value::String(d.to_string()),
        Value::Time(t) => serde_json::Value::String(t.to_string()),
        Value::Timestamp(ts) => serde_json::Value::String(ts.to_string()),
        Value::TimestampTz(ts) => serde_json::Value::String(ts.to_string()),
        Value::Uuid(u) => serde_json::Value::String(u.to_string()),
        Value::Json(v) => v.clone(),
        Value::Interval(_) => serde_json::Value::String(format!("{v:?}")),
        Value::Engine { display_text, .. } => serde_json::Value::String(display_text.clone()),
    }
}
