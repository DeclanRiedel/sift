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
//!
//! The query still runs through the server-side cursor registry (see
//! [`crate::session::SessionStore::export_stream`]): the caller wraps the
//! driver stream so the per-session cursor cap and the pump apply, and
//! passes a drop-guard into [`encode_stream`] that releases the cursor
//! when the download completes or the client disconnects.

use bytes::{BufMut, Bytes, BytesMut};
use futures::Stream;
use serde::Serialize;
use sift_protocol::{ColumnMetadata, ExportFormat, Page, Row, Value};
use std::fmt::Write as _;

pub trait PageRetention: Send + 'static {
    fn page_received(&self);
    fn page_processed(&self);
}

/// Content-Type header value for `format`.
pub fn content_type(format: ExportFormat) -> &'static str {
    match format {
        ExportFormat::Csv => "text/csv; charset=utf-8",
        ExportFormat::Tsv => "text/tab-separated-values; charset=utf-8",
        ExportFormat::JsonLines => "application/x-ndjson",
        ExportFormat::JsonArray => "application/json",
        ExportFormat::Html => "text/html; charset=utf-8",
        ExportFormat::Markdown => "text/markdown; charset=utf-8",
        ExportFormat::Xlsx => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        ExportFormat::SqlInsert => "application/sql; charset=utf-8",
    }
}

/// Encode the pages arriving on `rx` (a registry-pumped cursor stream)
/// into a byte stream of the export body. Errors during streaming are
/// surfaced through the stream's `Err` yield — the HTTP layer converts
/// the first error into a 500 header if it lands before any bytes are
/// written, otherwise the transfer aborts mid-flight (chunked encoding).
///
/// `guard` is held for the lifetime of the returned stream and dropped
/// when the export completes or the client disconnects. The caller uses
/// it to release the underlying cursor from the registry (and thereby
/// cancel the pump); `encode_stream` itself only needs to keep it alive.
pub fn encode_stream<G: PageRetention>(
    mut rx: tokio::sync::mpsc::Receiver<Page>,
    format: ExportFormat,
    emit_header: bool,
    null_display: String,
    guard: G,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static {
    async_stream::try_stream! {
        // Owned by the generator so it drops (releasing the cursor) when
        // the stream is exhausted or the consumer is dropped.
        let _guard = guard;
        let mut columns: Vec<ColumnMetadata> = Vec::new();
        let mut row_buf = BytesMut::with_capacity(8192);
        let mut header_sent = false;
        let mut first_row_in_array = true;
        let mut xlsx_sheet = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheetData>");
        if matches!(format, ExportFormat::JsonArray) {
            yield Bytes::from_static(b"[");
        } else if matches!(format, ExportFormat::Html) {
            yield Bytes::from_static(b"<!doctype html><meta charset=\"utf-8\"><table>\n");
        }
        while let Some(page) = rx.recv().await {
            _guard.page_received();
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
                        } else if matches!(format, ExportFormat::Html | ExportFormat::Markdown) {
                            yield rich_header(&columns, format);
                        } else if matches!(format, ExportFormat::Xlsx) {
                            append_xlsx_row(&mut xlsx_sheet, columns.iter().map(|column| column.name.as_str()))?;
                        }
                        header_sent = true;
                    }
                    for row in rows {
                        if matches!(format, ExportFormat::Xlsx) {
                            let values = row.values.iter().map(value_to_text).collect::<Vec<_>>();
                            append_xlsx_row(&mut xlsx_sheet, values.iter().map(String::as_str))?;
                            if xlsx_sheet.len() > 64 * 1024 * 1024 {
                                Err(std::io::Error::other("XLSX export exceeds 64 MiB"))?;
                            }
                        } else {
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
                }
                Page::Done { .. } => break,
                Page::Error { error } => {
                    Err(std::io::Error::other(format!(
                        "{}: {}",
                        error.code, error.message
                    )))?;
                }
            }
            _guard.page_processed();
        }
        if matches!(format, ExportFormat::JsonArray) {
            yield Bytes::from_static(b"]");
        } else if matches!(format, ExportFormat::Html) {
            yield Bytes::from_static(b"</table>\n");
        } else if matches!(format, ExportFormat::Xlsx) {
            xlsx_sheet.push_str("</sheetData></worksheet>");
            yield build_xlsx(&xlsx_sheet)?;
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
        ExportFormat::Html => {
            buf.extend_from_slice(b"<tr>");
            for value in &row.values {
                buf.extend_from_slice(b"<td>");
                buf.extend_from_slice(html_escape(&value_to_text(value)).as_bytes());
                buf.extend_from_slice(b"</td>");
            }
            buf.extend_from_slice(b"</tr>\n");
            buf.split().freeze()
        }
        ExportFormat::Markdown => {
            buf.put_u8(b'|');
            for value in &row.values {
                buf.put_u8(b' ');
                buf.extend_from_slice(markdown_escape(&value_to_text(value)).as_bytes());
                buf.extend_from_slice(b" |");
            }
            buf.put_u8(b'\n');
            buf.split().freeze()
        }
        ExportFormat::SqlInsert => {
            buf.extend_from_slice(b"INSERT INTO \"result\" (");
            for (index, column) in columns.iter().enumerate() {
                if index > 0 {
                    buf.extend_from_slice(b", ");
                }
                write_sql_identifier(buf, &column.name);
            }
            buf.extend_from_slice(b") VALUES (");
            for (index, value) in row.values.iter().enumerate() {
                if index > 0 {
                    buf.extend_from_slice(b", ");
                }
                write_sql_value(buf, value);
            }
            buf.extend_from_slice(b");\n");
            buf.split().freeze()
        }
        ExportFormat::Xlsx => unreachable!("XLSX rows are buffered into the workbook"),
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
        ExportFormat::JsonLines
        | ExportFormat::JsonArray
        | ExportFormat::Html
        | ExportFormat::Markdown
        | ExportFormat::Xlsx
        | ExportFormat::SqlInsert => {}
    }
}

fn write_sql_identifier(buf: &mut BytesMut, value: &str) {
    buf.put_u8(b'"');
    for byte in value.bytes() {
        if byte == b'"' {
            buf.put_u8(b'"');
        }
        buf.put_u8(byte);
    }
    buf.put_u8(b'"');
}

fn write_sql_string(buf: &mut BytesMut, value: &str) {
    buf.put_u8(b'\'');
    for byte in value.bytes() {
        if byte == b'\'' {
            buf.put_u8(b'\'');
        }
        buf.put_u8(byte);
    }
    buf.put_u8(b'\'');
}

fn write_sql_value(buf: &mut BytesMut, value: &Value) {
    match value {
        Value::Null | Value::TypedNull { .. } => buf.extend_from_slice(b"NULL"),
        Value::Bool(true) => buf.extend_from_slice(b"TRUE"),
        Value::Bool(false) => buf.extend_from_slice(b"FALSE"),
        Value::Int16(value) => write!(buf, "{value}").expect("write to BytesMut"),
        Value::Int32(value) => write!(buf, "{value}").expect("write to BytesMut"),
        Value::Int64(value) => write!(buf, "{value}").expect("write to BytesMut"),
        Value::Float32(value) => write!(buf, "{value}").expect("write to BytesMut"),
        Value::Float64(value) => write!(buf, "{value}").expect("write to BytesMut"),
        Value::Decimal(value) => buf.extend_from_slice(value.as_bytes()),
        Value::Blob(value) => {
            buf.extend_from_slice(b"X'");
            write_hex(buf, value);
            buf.put_u8(b'\'');
        }
        Value::Text(value) => write_sql_string(buf, value),
        Value::Date(value) => write_sql_string(buf, &value.to_string()),
        Value::Time(value) => write_sql_string(buf, &value.to_string()),
        Value::Timestamp(value) => write_sql_string(buf, &value.to_string()),
        Value::TimestampTz(value) => write_sql_string(buf, &value.to_rfc3339()),
        Value::Interval(value) => write_sql_string(buf, &format!("{value:?}")),
        Value::Uuid(value) => write_sql_string(buf, &value.to_string()),
        Value::Json(value) => write_sql_string(buf, &value.to_string()),
        Value::Native { display_text, .. } => write_sql_string(buf, display_text),
    }
}

fn write_csv_value(buf: &mut BytesMut, v: &Value, null_display: &str) {
    match v {
        Value::Null | Value::TypedNull { .. } => write_csv_str(buf, null_display),
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
        Value::Native { display_text, .. } => write_csv_str(buf, display_text),
    }
}

fn write_tsv_value(buf: &mut BytesMut, v: &Value, null_display: &str) {
    match v {
        Value::Null | Value::TypedNull { .. } => write_tsv_str(buf, null_display),
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
        Value::Native { display_text, .. } => write_tsv_str(buf, display_text),
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
        Value::Null | Value::TypedNull { .. } => buf.extend_from_slice(b"null"),
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
        Value::Native { display_text, .. } => write_json(buf, display_text),
    }
}

fn rich_header(columns: &[ColumnMetadata], format: ExportFormat) -> Bytes {
    match format {
        ExportFormat::Html => Bytes::from(format!(
            "<thead><tr>{}</tr></thead><tbody>\n",
            columns
                .iter()
                .map(|column| format!("<th>{}</th>", html_escape(&column.name)))
                .collect::<String>()
        )),
        ExportFormat::Markdown => {
            let names = columns
                .iter()
                .map(|column| markdown_escape(&column.name))
                .collect::<Vec<_>>();
            Bytes::from(format!(
                "| {} |\n| {} |\n",
                names.join(" | "),
                names.iter().map(|_| "---").collect::<Vec<_>>().join(" | ")
            ))
        }
        ExportFormat::SqlInsert => Bytes::new(),
        _ => Bytes::new(),
    }
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn markdown_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace('\r', " ")
        .replace('\n', "<br>")
}

fn value_to_text(value: &Value) -> String {
    match value {
        Value::Null | Value::TypedNull { .. } => String::new(),
        Value::Bool(value) => value.to_string(),
        Value::Int16(value) => value.to_string(),
        Value::Int32(value) => value.to_string(),
        Value::Int64(value) => value.to_string(),
        Value::Float32(value) => value.to_string(),
        Value::Float64(value) => value.to_string(),
        Value::Decimal(value) | Value::Text(value) => value.clone(),
        Value::Blob(value) => hex_encode(value),
        Value::Date(value) => value.to_string(),
        Value::Time(value) => value.to_string(),
        Value::Timestamp(value) => value.to_string(),
        Value::TimestampTz(value) => value.to_string(),
        Value::Uuid(value) => value.to_string(),
        Value::Json(value) => value.to_string(),
        Value::Interval(_) => format!("{value:?}"),
        Value::Native { display_text, .. } => display_text.clone(),
    }
}

fn append_xlsx_row<'a>(
    sheet: &mut String,
    values: impl IntoIterator<Item = &'a str>,
) -> std::io::Result<()> {
    sheet.push_str("<row>");
    for value in values {
        sheet.push_str("<c t=\"inlineStr\"><is><t xml:space=\"preserve\">");
        sheet.push_str(&html_escape(value));
        sheet.push_str("</t></is></c>");
    }
    sheet.push_str("</row>");
    Ok(())
}

pub(crate) fn build_xlsx(sheet: &str) -> std::io::Result<Bytes> {
    use std::io::{Cursor, Write as _};
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    let mut output = Cursor::new(Vec::new());
    {
        let mut zip = ZipWriter::new(&mut output);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for (path, content) in [
            ("[Content_Types].xml", "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/><Override PartName=\"/xl/worksheets/sheet1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/></Types>"),
            ("_rels/.rels", "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/></Relationships>"),
            ("xl/workbook.xml", "<?xml version=\"1.0\" encoding=\"UTF-8\"?><workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><sheets><sheet name=\"Results\" sheetId=\"1\" r:id=\"rId1\"/></sheets></workbook>"),
            ("xl/_rels/workbook.xml.rels", "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet1.xml\"/></Relationships>"),
            ("xl/worksheets/sheet1.xml", sheet),
        ] {
            zip.start_file(path, options)?;
            zip.write_all(content.as_bytes())?;
        }
        zip.finish()?;
    }
    Ok(Bytes::from(output.into_inner()))
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;

    use futures::StreamExt as _;

    use super::*;

    struct Retention;
    impl PageRetention for Retention {
        fn page_received(&self) {}
        fn page_processed(&self) {}
    }

    fn pages() -> Vec<Page> {
        vec![
            Page::NextResult {
                columns: vec![synthetic_column(0)],
            },
            Page::Rows {
                rows: vec![Row {
                    values: vec![Value::Text("<x>|=SUM(1,1)".into())],
                }],
            },
            Page::Done {
                affected_rows: None,
                warnings: Vec::new(),
            },
        ]
    }

    async fn encoded(format: ExportFormat) -> Vec<u8> {
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        for page in pages() {
            tx.send(page).await.unwrap();
        }
        drop(tx);
        let stream = encode_stream(rx, format, true, String::new(), Retention);
        futures::pin_mut!(stream);
        let mut output = Vec::new();
        while let Some(chunk) = stream.next().await {
            output.extend_from_slice(&chunk.unwrap());
        }
        output
    }

    #[tokio::test]
    async fn rich_formats_escape_markup_and_xlsx_formulas() {
        let html = String::from_utf8(encoded(ExportFormat::Html).await).unwrap();
        assert!(html.contains("&lt;x&gt;|=SUM(1,1)"));
        let markdown = String::from_utf8(encoded(ExportFormat::Markdown).await).unwrap();
        assert!(markdown.contains("<x>\\|=SUM(1,1)"));

        let xlsx = encoded(ExportFormat::Xlsx).await;
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(xlsx)).unwrap();
        let mut sheet = String::new();
        archive
            .by_name("xl/worksheets/sheet1.xml")
            .unwrap()
            .read_to_string(&mut sheet)
            .unwrap();
        assert!(sheet.contains("t=\"inlineStr\""));
        assert!(!sheet.contains("<f>"));
        assert!(sheet.contains("&lt;x&gt;|=SUM(1,1)"));
    }

    #[tokio::test]
    async fn sql_insert_export_quotes_identifiers_and_literals() {
        let sql = String::from_utf8(encoded(ExportFormat::SqlInsert).await).unwrap();
        assert_eq!(
            sql,
            "INSERT INTO \"result\" (\"col_0\") VALUES ('<x>|=SUM(1,1)');\n"
        );

        let mut buffer = BytesMut::new();
        write_sql_identifier(&mut buffer, "odd\"name");
        buffer.put_u8(b' ');
        write_sql_value(&mut buffer, &Value::Text("O'Brien".into()));
        assert_eq!(&buffer[..], b"\"odd\"\"name\" 'O''Brien'");
    }
}
