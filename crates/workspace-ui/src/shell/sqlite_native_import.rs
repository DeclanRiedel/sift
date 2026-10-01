//! Explicit CSV-to-value conversion for SQLite's preview-bound native target.

use sift_protocol::{SqliteNativeBulkRows, Value};

const MAX_ROWS: usize = 10_000;
const MAX_BYTES: usize = 8 * 1024 * 1024;

pub(super) fn rows(
    data: &[u8],
    columns: Vec<String>,
    types: &[String],
) -> Result<SqliteNativeBulkRows, String> {
    if columns.is_empty() || columns.len() > 128 || columns.len() != types.len() {
        return Err("Native SQLite import requires 1–128 mapped, typed columns".into());
    }
    let mut unique = std::collections::HashSet::new();
    if columns.iter().any(|column| {
        column.is_empty()
            || column.trim() != column
            || column.len() > 256
            || column.contains('\0')
            || !unique.insert(column.to_ascii_lowercase())
    }) {
        return Err("Native SQLite target columns must be distinct valid names".into());
    }
    if types.iter().any(|kind| {
        !matches!(
            kind.trim().to_ascii_uppercase().as_str(),
            "TEXT" | "INTEGER" | "REAL" | "DECIMAL TEXT" | "BLOB HEX"
        )
    }) {
        return Err(
            "Native SQLite types must be TEXT, INTEGER, REAL, DECIMAL TEXT, or BLOB HEX".into(),
        );
    }
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_reader(data);
    if reader.headers().map_err(|error| error.to_string())?.len() != columns.len() {
        return Err("CSV mapping does not match the source column count".into());
    }
    let mut values = Vec::new();
    for (row_index, record) in reader.records().enumerate() {
        if row_index >= MAX_ROWS {
            return Err("Native SQLite import is limited to 10,000 rows".into());
        }
        let record = record.map_err(|error| format!("CSV row {}: {error}", row_index + 2))?;
        if record.len() != columns.len() {
            return Err(format!("CSV row {} has a different width", row_index + 2));
        }
        let row = record
            .iter()
            .enumerate()
            .map(|(index, text)| {
                value(text, &types[index]).map_err(|error| {
                    format!(
                        "CSV row {} column `{}`: {error}",
                        row_index + 2,
                        columns[index]
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        values.push(row);
    }
    if values.is_empty() {
        return Err("Native SQLite import requires at least one row".into());
    }
    let native = SqliteNativeBulkRows {
        columns,
        rows: values,
    };
    if serde_json::to_vec(&native)
        .map_err(|error| error.to_string())?
        .len()
        > MAX_BYTES
    {
        return Err("Native SQLite typed payload exceeds 8 MiB".into());
    }
    Ok(native)
}

fn value(text: &str, kind: &str) -> Result<Value, String> {
    if text == "NULL" {
        return Ok(Value::Null);
    }
    match kind.trim().to_ascii_uppercase().as_str() {
        "TEXT" => Ok(Value::Text(text.into())),
        "INTEGER" => text
            .parse::<i64>()
            .map(Value::Int64)
            .map_err(|_| "expected a signed 64-bit integer".into()),
        "REAL" => text
            .parse::<f64>()
            .ok()
            .filter(|number| number.is_finite())
            .map(Value::Float64)
            .ok_or_else(|| "expected a finite real number".into()),
        "DECIMAL TEXT" => {
            let unsigned = text.strip_prefix('-').unwrap_or(text);
            let mut parts = unsigned.split('.');
            let integer = parts.next().unwrap_or_default();
            let fractional = parts.next();
            let scale = fractional.map_or(0, str::len);
            if parts.next().is_some()
                || integer.is_empty()
                || !integer.bytes().all(|b| b.is_ascii_digit())
                || (integer.len() > 1 && integer.starts_with('0'))
                || fractional.is_some_and(|part| {
                    part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit())
                })
                || integer.len() + scale > 38
                || scale > 18
            {
                return Err("expected canonical decimal text (38 digits, scale 18)".into());
            }
            Ok(Value::Decimal(text.into()))
        }
        "BLOB HEX" => {
            if text.len() % 2 != 0 {
                return Err("expected even-length hex bytes".into());
            }
            let bytes = text
                .as_bytes()
                .chunks_exact(2)
                .map(|pair| {
                    let high = (pair[0] as char).to_digit(16).ok_or("expected hex bytes")?;
                    let low = (pair[1] as char).to_digit(16).ok_or("expected hex bytes")?;
                    Ok::<_, &str>(((high << 4) | low) as u8)
                })
                .collect::<Result<Vec<_>, _>>()
                .map_err(str::to_owned)?;
            Ok(Value::Blob(bytes))
        }
        _ => Err("type must be TEXT, INTEGER, REAL, DECIMAL TEXT, or BLOB HEX".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_rows_preserve_text_and_reject_lossy_conversion() {
        let native = rows(
            b"id,amount,note\n1,0.01,\n2,NULL,NULL\n",
            vec!["id".into(), "amount".into(), "note".into()],
            &["INTEGER".into(), "DECIMAL TEXT".into(), "TEXT".into()],
        )
        .unwrap();
        assert_eq!(
            native.rows[0],
            vec![
                Value::Int64(1),
                Value::Decimal("0.01".into()),
                Value::Text("".into())
            ]
        );
        assert_eq!(
            native.rows[1],
            vec![Value::Int64(2), Value::Null, Value::Null]
        );
        assert!(rows(b"id\n1.5\n", vec!["id".into()], &["INTEGER".into()]).is_err());
        assert!(rows(b"x\nNaN\n", vec!["x".into()], &["REAL".into()]).is_err());
        assert!(rows(b"x\n01.20\n", vec!["x".into()], &["DECIMAL TEXT".into()]).is_err());
        assert!(rows(b"x\nNULL\n", vec!["x".into()], &["GUESS".into()]).is_err());
    }
}
