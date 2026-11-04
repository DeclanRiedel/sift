//! Bounded local CSV projection. Source positions stay separate from outputs.
use std::io::Write;

pub(super) const MAX_BYTES: usize = 64 * 1024 * 1024;
pub(super) const MAX_COLUMNS: usize = 256;
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Selection {
    pub index: usize,
    pub target: String,
}

pub(super) fn validate(columns: &[Selection], sources: usize) -> Result<(), String> {
    if columns.is_empty() || columns.len() > MAX_COLUMNS || sources > MAX_COLUMNS {
        return Err("CSV review requires 1–256 source and output columns".into());
    }
    let mut names = std::collections::HashSet::new();
    for column in columns {
        if column.index >= sources {
            return Err("CSV selection references an unknown source column".into());
        }
        let name = &column.target;
        if name.is_empty()
            || name.trim() != name
            || name.contains(char::from(0))
            || name.len() > 128
            || !names.insert(name)
        {
            return Err(
                "Target columns must be distinct, nonempty, trimmed names of at most 128 bytes"
                    .into(),
            );
        }
    }
    Ok(())
}

pub(super) struct ByteLimit<W> {
    inner: W,
    remaining: usize,
}
impl<W> ByteLimit<W> {
    pub(super) fn new(inner: W, limit: usize) -> Self {
        Self {
            inner,
            remaining: limit,
        }
    }
    pub(super) fn into_inner(self) -> W {
        self.inner
    }
}
impl<W: Write> Write for ByteLimit<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.remaining {
            return Err(std::io::Error::other("output byte limit exceeded"));
        }
        let written = self.inner.write(bytes)?;
        self.remaining -= written;
        Ok(written)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

pub(super) fn rewrite(data: &[u8], columns: &[Selection], limit: usize) -> Result<Vec<u8>, String> {
    if data.len() > limit {
        return Err("CSV exceeds the 64 MiB limit".into());
    }
    let mut reader = csv::Reader::from_reader(data);
    let width = reader
        .headers()
        .map_err(|_| "CSV header is malformed")?
        .len();
    validate(columns, width)?;
    let mut writer = csv::Writer::from_writer(ByteLimit::new(Vec::new(), limit));
    let bounded = || "Mapped CSV exceeds the 64 MiB limit".to_string();
    writer
        .write_record(columns.iter().map(|column| &column.target))
        .map_err(|_| bounded())?;
    for row in reader.records() {
        let row = row.map_err(|_| "CSV record is malformed or has an inconsistent width")?;
        writer
            .write_record(columns.iter().map(|column| &row[column.index]))
            .map_err(|_| bounded())?;
    }
    Ok(writer.into_inner().map_err(|_| bounded())?.into_inner())
}

pub(super) fn recipe(
    columns: &[Selection],
    headers: &[String],
) -> Result<serde_json::Value, String> {
    validate(columns, headers.len())?;
    let mut output = Vec::new();
    for column in columns {
        let source = &headers[column.index];
        if source.len() > 4096 || headers.iter().filter(|header| *header == source).count() != 1 {
            return Err("A recipe needs unique exact source headers; omit ambiguous fields or fix the source file".into());
        }
        output.push(serde_json::json!({"source":source,"target":column.target}));
    }
    Ok(serde_json::Value::Array(output))
}

pub(super) fn copy_name(name: &str, existing: &[String]) -> String {
    let mut base = name.trim().to_string();
    if base.is_empty() || base.contains(char::from(0)) {
        base = "column".into();
    }
    while base.len() > 120 {
        base.pop();
    }
    for n in 1..=MAX_COLUMNS + 1 {
        let candidate = if n == 1 {
            format!("{base}_copy")
        } else {
            format!("{base}_copy{n}")
        };
        if !existing.contains(&candidate) {
            return candidate;
        }
    }
    unreachable!("at most 256 existing outputs")
}

pub(super) fn snippet(value: &str) -> String {
    if value.len() <= 256 {
        return value.into();
    }
    let mut end = 256;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dropping_reordering_copying_and_renaming_preserve_fields_and_bounds() {
        let source="id,private,label\n001,omitted-secret,\"a,b\n\"\"café\"\"\"\n2,omitted-secret,NULL\n3,omitted-secret,\n";
        let columns = vec![
            Selection {
                index: 2,
                target: "name".into(),
            },
            Selection {
                index: 0,
                target: "copy".into(),
            },
            Selection {
                index: 0,
                target: "id".into(),
            },
        ];
        let mapped = rewrite(source.as_bytes(), &columns, 4096).unwrap();
        assert!(!String::from_utf8_lossy(&mapped).contains("omitted-secret"));
        let rows = csv::Reader::from_reader(mapped.as_slice())
            .records()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            rows[0],
            csv::StringRecord::from(vec!["a,b\n\"café\"", "001", "001"])
        );
        assert_eq!(&rows[1][0], "NULL");
        assert_eq!(&rows[2][0], "");
        let small = format!("a\n{}\n", "x".repeat(500));
        let expansion = (0..5)
            .map(|n| Selection {
                index: 0,
                target: format!("c{n}"),
            })
            .collect::<Vec<_>>();
        assert!(rewrite(small.as_bytes(), &expansion, 1024).is_err());
        assert!(rewrite(
            b"a,b\n1,2,3\n",
            &[Selection {
                index: 0,
                target: "a".into()
            }],
            1024
        )
        .is_err());
        assert!(recipe(
            &[Selection {
                index: 0,
                target: "a".into()
            }],
            &["a".into(), "a".into()]
        )
        .is_err());
        assert_eq!(copy_name("a", &["a_copy".into()]), "a_copy2");
        assert!(copy_name(&"é".repeat(100), &[]).len() <= 128);
    }
}
