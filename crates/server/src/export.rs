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

use bytes::{BufMut, Bytes, BytesMut};
use futures::Stream;
use serde::Serialize;
use sift_driver_api::Driver;
use sift_protocol::{ColumnMetadata, DriverError, ExecuteRequest, ExportFormat, Page, Row, Value};
use std::fmt::Write as _;

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
        let mut row_buf = BytesMut::with_capacity(8192);
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
                        yield encode_row(
                            &mut row_buf,
                            &row,
                            &columns,
                            format,
                            &null_display,
                            &mut first_row_in_array,
                        );
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
    buf: &mut BytesMut,
    row: &Row,
    columns: &[ColumnMetadata],
    format: ExportFormat,
    null_display: &str,
    first_row_in_array: &mut bool,
) -> Bytes {
    buf.clear();
    match format {
        ExportFormat::Csv | ExportFormat::Tsv => {
            for (i, value) in row.values.iter().enumerate() {
                if i > 0 {
                    buf.put_u8(delimiter(format) as u8);
                }
                write_delimited_value(buf, value, format, null_display);
            }
            buf.put_u8(b'\n');
            buf.split().freeze()
        }
        ExportFormat::JsonLines => {
            write_json_object(buf, row, columns);
            buf.put_u8(b'\n');
            buf.split().freeze()
        }
        ExportFormat::JsonArray => {
            if *first_row_in_array {
                *first_row_in_array = false;
            } else {
                buf.put_u8(b',');
            }
            write_json_object(buf, row, columns);
            buf.split().freeze()
        }
    }
}

fn delimiter(format: ExportFormat) -> char {
    match format {
        ExportFormat::Tsv => '\t',
        _ => ',',
    }
}

fn write_delimited_value(buf: &mut BytesMut, v: &Value, format: ExportFormat, null_display: &str) {
    match format {
        ExportFormat::Csv => write_csv_value(buf, v, null_display),
        ExportFormat::Tsv => write_tsv_value(buf, v, null_display),
        ExportFormat::JsonLines | ExportFormat::JsonArray => {}
    }
}

fn write_csv_value(buf: &mut BytesMut, v: &Value, null_display: &str) {
    match v {
        Value::Null => write_csv_str(buf, null_display),
        Value::Bool(true) => buf.extend_from_slice(b"true"),
        Value::Bool(false) => buf.extend_from_slice(b"false"),
        Value::Int16(i) => write!(buf, "{i}").expect("write to BytesMut"),
        Value::Int32(i) => write!(buf, "{i}").expect("write to BytesMut"),
        Value::Int64(i) => write!(buf, "{i}").expect("write to BytesMut"),
        Value::Float32(f) => write!(buf, "{f}").expect("write to BytesMut"),
        Value::Float64(f) => write!(buf, "{f}").expect("write to BytesMut"),
        Value::Decimal(s) | Value::Text(s) => write_csv_str(buf, s),
        Value::Blob(bytes) => write_hex(buf, bytes),
        Value::Date(d) => write!(buf, "{d}").expect("write to BytesMut"),
        Value::Time(t) => write!(buf, "{t}").expect("write to BytesMut"),
        Value::Timestamp(ts) => write!(buf, "{ts}").expect("write to BytesMut"),
        Value::TimestampTz(ts) => write!(buf, "{ts}").expect("write to BytesMut"),
        Value::Uuid(u) => write!(buf, "{u}").expect("write to BytesMut"),
        Value::Json(v) => write_csv_str(buf, &v.to_string()),
        Value::Interval(_) => write_csv_str(buf, &format!("{v:?}")),
        Value::Engine { display_text, .. } => write_csv_str(buf, display_text),
    }
}

fn write_tsv_value(buf: &mut BytesMut, v: &Value, null_display: &str) {
    match v {
        Value::Null => write_tsv_str(buf, null_display),
        Value::Bool(true) => buf.extend_from_slice(b"true"),
        Value::Bool(false) => buf.extend_from_slice(b"false"),
        Value::Int16(i) => write!(buf, "{i}").expect("write to BytesMut"),
        Value::Int32(i) => write!(buf, "{i}").expect("write to BytesMut"),
        Value::Int64(i) => write!(buf, "{i}").expect("write to BytesMut"),
        Value::Float32(f) => write!(buf, "{f}").expect("write to BytesMut"),
        Value::Float64(f) => write!(buf, "{f}").expect("write to BytesMut"),
        Value::Decimal(s) | Value::Text(s) => write_tsv_str(buf, s),
        Value::Blob(bytes) => write_hex(buf, bytes),
        Value::Date(d) => write!(buf, "{d}").expect("write to BytesMut"),
        Value::Time(t) => write!(buf, "{t}").expect("write to BytesMut"),
        Value::Timestamp(ts) => write!(buf, "{ts}").expect("write to BytesMut"),
        Value::TimestampTz(ts) => write!(buf, "{ts}").expect("write to BytesMut"),
        Value::Uuid(u) => write!(buf, "{u}").expect("write to BytesMut"),
        Value::Json(v) => write_tsv_str(buf, &v.to_string()),
        Value::Interval(_) => write_tsv_str(buf, &format!("{v:?}")),
        Value::Engine { display_text, .. } => write_tsv_str(buf, display_text),
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

fn write_hex(buf: &mut BytesMut, bytes: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    buf.reserve(bytes.len() * 2);
    for byte in bytes {
        buf.put_u8(HEX[(byte >> 4) as usize]);
        buf.put_u8(HEX[(byte & 0x0f) as usize]);
    }
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

fn write_csv_str(buf: &mut BytesMut, s: &str) {
    let needs_quote = s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r');
    if !needs_quote {
        buf.extend_from_slice(s.as_bytes());
        return;
    }
    buf.put_u8(b'"');
    for byte in s.bytes() {
        if byte == b'"' {
            buf.put_u8(b'"');
        }
        buf.put_u8(byte);
    }
    buf.put_u8(b'"');
}

fn write_tsv_str(buf: &mut BytesMut, s: &str) {
    for byte in s.bytes() {
        match byte {
            b'\\' => buf.extend_from_slice(br"\\"),
            b'\t' => buf.extend_from_slice(br"\t"),
            b'\n' => buf.extend_from_slice(br"\n"),
            b'\r' => buf.extend_from_slice(br"\r"),
            _ => buf.put_u8(byte),
        }
    }
}

fn write_json_object(buf: &mut BytesMut, row: &Row, columns: &[ColumnMetadata]) {
    buf.put_u8(b'{');
    for (i, value) in row.values.iter().enumerate() {
        if i > 0 {
            buf.put_u8(b',');
        }
        let key = columns.get(i).map(|c| c.name.as_str()).unwrap_or("col");
        if columns.get(i).is_some() {
            write_json(buf, &key);
        } else {
            write_json(buf, &format!("col_{i}"));
        }
        buf.put_u8(b':');
        write_json_value(buf, value);
    }
    buf.put_u8(b'}');
}

fn write_json<T: Serialize + ?Sized>(buf: &mut BytesMut, value: &T) {
    serde_json::to_writer(buf.writer(), value).expect("serialize JSON value to BytesMut");
}

fn write_json_value(buf: &mut BytesMut, v: &Value) {
    match v {
        Value::Null => buf.extend_from_slice(b"null"),
        Value::Bool(b) => write_json(buf, b),
        Value::Int16(i) => write_json(buf, i),
        Value::Int32(i) => write_json(buf, i),
        Value::Int64(i) => write_json(buf, i),
        Value::Float32(f) => write_json(buf, f),
        Value::Float64(f) => write_json(buf, f),
        Value::Decimal(s) | Value::Text(s) => write_json(buf, s),
        Value::Blob(b) => write_json(buf, &hex_encode(b)),
        Value::Date(d) => write_json(buf, &d.to_string()),
        Value::Time(t) => write_json(buf, &t.to_string()),
        Value::Timestamp(ts) => write_json(buf, &ts.to_string()),
        Value::TimestampTz(ts) => write_json(buf, &ts.to_string()),
        Value::Uuid(u) => write_json(buf, &u.to_string()),
        Value::Json(v) => write_json(buf, v),
        Value::Interval(_) => write_json(buf, &format!("{v:?}")),
        Value::Engine { display_text, .. } => write_json(buf, display_text),
    }
}
