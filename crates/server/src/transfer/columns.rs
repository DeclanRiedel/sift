//! CSV recipe selection. No database calls or value coercion.
use super::{ApiError, ApiResult, TransferDirection};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::io::Write;

#[derive(Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Projection {
    source: String,
    target: String,
}
pub(super) enum Selection {
    Rename(BTreeMap<String, String>),
    Project(Vec<Projection>),
}
impl Selection {
    pub(super) fn is_projection(&self) -> bool {
        matches!(self, Self::Project(_))
    }
}
fn invalid(message: &'static str) -> ApiError {
    ApiError::BadRequest(message.into())
}
fn target_valid(target: &str) -> bool {
    !target.is_empty() && target.trim() == target && !target.contains('\0') && target.len() <= 128
}

pub(crate) fn validate_options(
    direction: TransferDirection,
    format: &str,
    version: &str,
    options: &serde_json::Value,
) -> ApiResult<()> {
    parse(direction, format, version, options).map(|_| ())
}
pub(super) fn parse(
    direction: TransferDirection,
    format: &str,
    version: &str,
    options: &serde_json::Value,
) -> ApiResult<Option<Selection>> {
    if format == "csv"
        && !(version == "1" || (version == "2" && direction == TransferDirection::Import))
    {
        return Err(invalid("unsupported CSV recipe format version"));
    }
    let mappings = options.get("column_mappings");
    let projection = options.get("column_projection");
    if mappings.is_none() && projection.is_none() {
        return Ok(None);
    }
    if direction != TransferDirection::Import || format != "csv" {
        return Err(invalid("CSV column selection requires a CSV upload import"));
    }
    if mappings.is_some() && projection.is_some() {
        return Err(invalid(
            "column_mappings and column_projection cannot be combined",
        ));
    }
    if let Some(projection) = projection {
        if version != "2" {
            return Err(invalid("column_projection requires CSV format_version 2"));
        }
        let projection = projection
            .as_array()
            .ok_or_else(|| invalid("column_projection must be an ordered array"))?;
        if projection.is_empty() || projection.len() > 256 {
            return Err(invalid("column_projection requires 1–256 output columns"));
        }
        let projection: Vec<Projection> =
            serde_json::from_value(serde_json::Value::Array(projection.clone())).map_err(|_| {
                invalid("each projected column requires only source and target strings")
            })?;
        let mut targets = HashSet::new();
        for column in &projection {
            if column.source.len() > 4096 || !target_valid(&column.target) {
                return Err(invalid("projected source or target column name is invalid"));
            }
            if !targets.insert(column.target.as_str()) {
                return Err(invalid("mapped target columns must be distinct"));
            }
        }
        return Ok(Some(Selection::Project(projection)));
    }
    let mappings = mappings
        .unwrap()
        .as_object()
        .ok_or_else(|| invalid("column_mappings must be an object of source and target names"))?;
    if mappings.len() > 256 {
        return Err(invalid("too many CSV column mappings"));
    }
    let mut parsed = BTreeMap::new();
    let mut targets = HashSet::new();
    for (source, target) in mappings {
        let target = target
            .as_str()
            .filter(|target| target_valid(target))
            .ok_or_else(|| invalid("target column name is invalid"))?;
        if !targets.insert(target) {
            return Err(invalid("mapped target columns must be distinct"));
        }
        parsed.insert(source.clone(), target.to_string());
    }
    Ok(Some(Selection::Rename(parsed)))
}

struct BoundedOutput {
    bytes: Vec<u8>,
    limit: usize,
}
impl Write for BoundedOutput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other("mapped CSV exceeds its byte limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(super) fn rewrite(
    data: Vec<u8>,
    selection: Selection,
    limit: usize,
    bind_original: bool,
) -> ApiResult<(Vec<u8>, Option<String>)> {
    if data.len() > limit {
        return Err(invalid("CSV exceeds the 64 MiB limit"));
    }
    let original = bind_original.then(|| format!("{:x}", Sha256::digest(&data)));
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_reader(data.as_slice());
    let headers = reader
        .headers()
        .map_err(|_| invalid("CSV header is malformed"))?
        .clone();
    let columns = match selection {
        Selection::Project(projection) => {
            let mut columns = Vec::with_capacity(projection.len());
            for column in projection {
                let mut matches = headers
                    .iter()
                    .enumerate()
                    .filter(|(_, header)| *header == column.source);
                let index = matches
                    .next()
                    .ok_or_else(|| invalid("column projection references an unknown source"))?
                    .0;
                if matches.next().is_some() {
                    return Err(invalid("column projection source header is ambiguous"));
                }
                columns.push((index, column.target));
            }
            columns
        }
        Selection::Rename(mappings) => {
            if mappings
                .keys()
                .any(|source| !headers.iter().any(|header| header == source))
            {
                return Err(invalid("column mapping references an unknown source"));
            }
            headers
                .iter()
                .enumerate()
                .map(|(index, source)| {
                    (
                        index,
                        mappings
                            .get(source)
                            .cloned()
                            .unwrap_or_else(|| source.to_string()),
                    )
                })
                .collect()
        }
    };
    let mut unique = HashSet::new();
    for (_, target) in &columns {
        if !target_valid(target) {
            return Err(invalid("target column name is invalid"));
        }
        if !unique.insert(target) {
            return Err(invalid("mapped target columns must be distinct"));
        }
    }
    let output = BoundedOutput {
        bytes: Vec::new(),
        limit,
    };
    let mut writer = csv::Writer::from_writer(output);
    writer
        .write_record(columns.iter().map(|(_, target)| target))
        .map_err(|_| invalid("mapped CSV exceeds the 64 MiB limit"))?;
    for row in reader.records() {
        let row =
            row.map_err(|_| invalid("CSV record is malformed or has an inconsistent width"))?;
        writer
            .write_record(columns.iter().map(|(index, _)| &row[*index]))
            .map_err(|_| invalid("mapped CSV exceeds the 64 MiB limit"))?;
    }
    let output = writer
        .into_inner()
        .map_err(|_| invalid("mapped CSV exceeds the 64 MiB limit"))?;
    Ok((output.bytes, original))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn projection(value: serde_json::Value) -> Selection {
        parse(
            TransferDirection::Import,
            "csv",
            "2",
            &serde_json::json!({"column_projection":value}),
        )
        .unwrap()
        .unwrap()
    }
    #[test]
    fn ordered_projection_drops_reorders_and_copies_exact_field_text() {
        let source=b"id,unused,unused,name\n001,one,private-dropped-canary,\"a,b\n\"\"quoted\"\"\"\n2,x,y,\n3,x,y,NULL\n";
        let select = projection(
            serde_json::json!([{"source":"name","target":"label"},{"source":"id","target":"id_copy"},{"source":"id","target":"id"}]),
        );
        let (bytes, digest) = rewrite(source.to_vec(), select, 4096, true).unwrap();
        assert!(digest.is_some());
        let mut reader = csv::Reader::from_reader(bytes.as_slice());
        assert_eq!(
            reader.headers().unwrap(),
            &csv::StringRecord::from(vec!["label", "id_copy", "id"])
        );
        let rows = reader.records().collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(
            rows[0],
            csv::StringRecord::from(vec!["a,b\n\"quoted\"", "001", "001"])
        );
        assert_eq!(&rows[1][0], "");
        assert_eq!(&rows[2][0], "NULL");
        assert!(!String::from_utf8(bytes)
            .unwrap()
            .contains("private-dropped-canary"));
        for data in [
            b"id,id\n1,2\n".as_slice(),
            b"other\n1\n",
            b"id,name,unused\n1,x,y,extra\n",
        ] {
            let select = projection(serde_json::json!([{"source":"id","target":"id"}]));
            assert!(rewrite(data.to_vec(), select, 4096, false).is_err());
        }
    }
    #[test]
    fn version_scope_shape_and_expansion_fail_before_import() {
        let valid = serde_json::json!({"column_projection":[{"source":"a","target":"b"}]});
        assert!(parse(TransferDirection::Import, "csv", "1", &valid).is_err());
        assert!(parse(TransferDirection::Export, "csv", "1", &valid).is_err());
        assert!(parse(TransferDirection::Import, "xlsx", "1", &valid).is_err());
        for value in [
            serde_json::json!([]),
            serde_json::json!([{"source":"a","target":"b","sql":"1"}]),
            serde_json::json!([{"source":"a","target":"b"},{"source":"a","target":"b"}]),
            serde_json::json!([{"source":"a","target":" bad"}]),
        ] {
            assert!(parse(
                TransferDirection::Import,
                "csv",
                "2",
                &serde_json::json!({"column_projection":value})
            )
            .is_err());
        }
        assert!(parse(TransferDirection::Import,"csv","2",&serde_json::json!({"column_projection":[{"source":"a","target":"b"}],"column_mappings":{}})).is_err());
        let repeated = (0..5)
            .map(|index| serde_json::json!({"source":"a","target":format!("t{index}")}))
            .collect::<Vec<_>>();
        let source = format!("a\n{}\n", "x".repeat(600));
        assert!(rewrite(
            source.into_bytes(),
            projection(serde_json::json!(repeated)),
            1024,
            false
        )
        .is_err());
    }
}
