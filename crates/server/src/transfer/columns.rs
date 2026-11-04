//! CSV recipe selection. No database calls or value coercion.
use super::{ApiError, ApiResult, TransferDirection};
use sha2::{Digest, Sha256};
pub(super) use sift_core::transfer_columns::Selection;
use std::io::Write;
fn invalid(message: &'static str) -> ApiError {
    ApiError::BadRequest(message.into())
}
pub(crate) fn validate_options(
    direction: TransferDirection,
    format: &str,
    version: &str,
    options: &serde_json::Value,
) -> ApiResult<()> {
    sift_core::transfer_columns::validate_options(direction, format, version, options)
        .map_err(invalid)
}
pub(super) fn parse(
    direction: TransferDirection,
    format: &str,
    version: &str,
    options: &serde_json::Value,
) -> ApiResult<Option<Selection>> {
    sift_core::transfer_columns::parse(direction, format, version, options).map_err(invalid)
}

pub(super) struct BoundedOutput {
    bytes: Vec<u8>,
    limit: usize,
}
impl BoundedOutput {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }
    pub(super) fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
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
    let headers = headers.iter().map(str::to_owned).collect::<Vec<_>>();
    let columns = selection.resolve(&headers).map_err(invalid)?;
    let output = BoundedOutput {
        bytes: Vec::new(),
        limit,
    };
    let mut writer = csv::Writer::from_writer(output);
    writer
        .write_record(columns.iter().map(|column| &column.target))
        .map_err(|_| invalid("mapped CSV exceeds the 64 MiB limit"))?;
    for row in reader.records() {
        let row =
            row.map_err(|_| invalid("CSV record is malformed or has an inconsistent width"))?;
        writer
            .write_record(columns.iter().map(|column| &row[column.index]))
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
