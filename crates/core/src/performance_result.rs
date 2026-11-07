//! Ephemeral, bounded decoded-result fingerprints. Digests never enter reports.
use serde::{
    ser::{SerializeMap, SerializeSeq, SerializeStruct},
    Serialize, Serializer,
};
use sha2::{Digest, Sha256};
use sift_protocol::{
    BenchmarkResultCheck, BenchmarkResultObservation, BenchmarkResultOrder, Page, Row, Value,
};
use std::io::{self, Write};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckError {
    Limit,
    Unsupported,
    Failed,
}
pub struct Fingerprint {
    pub observation: BenchmarkResultObservation,
    schema: [u8; 32],
    rows: [u8; 32],
}
impl Fingerprint {
    pub fn matches(&self, other: &Self) -> bool {
        self.observation.rows == other.observation.rows
            && self.schema == other.schema
            && self.rows == other.rows
    }
}
pub struct ResultCheck {
    config: BenchmarkResultCheck,
    schema: Option<[u8; 32]>,
    columns: u32,
    rows: u64,
    bytes: u64,
    done: bool,
    sequence: Sha256,
    unordered: Vec<[u8; 32]>,
}
impl ResultCheck {
    pub fn new(config: BenchmarkResultCheck) -> Result<Self, CheckError> {
        config.validate().map_err(|_| CheckError::Limit)?;
        Ok(Self {
            config,
            schema: None,
            columns: 0,
            rows: 0,
            bytes: 0,
            done: false,
            sequence: Sha256::new(),
            unordered: Vec::new(),
        })
    }
    pub fn page(&mut self, page: &Page) -> Result<(), CheckError> {
        if self.done && !matches!(page, Page::Error { .. }) {
            return Err(CheckError::Unsupported);
        }
        match page {
            Page::NextResult { columns } => {
                if self.schema.is_some() || columns.is_empty() || columns.len() > 2000 {
                    return Err(CheckError::Unsupported);
                }
                self.columns = columns.len() as u32;
                let shape = columns
                    .iter()
                    .map(|column| (&column.name, &column.type_ref))
                    .collect::<Vec<_>>();
                self.schema = Some(hash(&shape, &mut self.bytes, self.config.max_bytes)?);
            }
            Page::Rows { rows } => {
                if self.schema.is_none() {
                    return Err(CheckError::Unsupported);
                }
                for row in rows {
                    self.rows = self.rows.checked_add(1).ok_or(CheckError::Limit)?;
                    if self.rows > u64::from(self.config.max_rows) {
                        return Err(CheckError::Limit);
                    }
                    if row.values.len() != self.columns as usize
                        || !row.values.iter().all(supported)
                    {
                        return Err(CheckError::Unsupported);
                    }
                    let digest = hash(&CanonicalRow(row), &mut self.bytes, self.config.max_bytes)?;
                    match self.config.order {
                        BenchmarkResultOrder::Ordered => self.sequence.update(digest),
                        BenchmarkResultOrder::Unordered => self.unordered.push(digest),
                    }
                }
            }
            Page::Done { .. } => self.done = true,
            Page::Error { .. } => return Err(CheckError::Failed),
        }
        Ok(())
    }
    /// Call only after raw EOF, never upon receiving Done.
    pub fn finish(mut self) -> Result<Fingerprint, CheckError> {
        if !self.done {
            return Err(CheckError::Failed);
        }
        let schema = self.schema.ok_or(CheckError::Unsupported)?;
        if self.config.order == BenchmarkResultOrder::Unordered {
            self.unordered.sort_unstable();
            for digest in self.unordered {
                self.sequence.update(digest);
            }
        }
        Ok(Fingerprint {
            schema,
            rows: self.sequence.finalize().into(),
            observation: BenchmarkResultObservation {
                rows: self.rows,
                columns: self.columns,
                canonical_bytes: self.bytes,
            },
        })
    }
}
fn supported(value: &Value) -> bool {
    match value {
        Value::Native { .. } | Value::TypedNull { .. } => false,
        Value::Float32(v) => v.is_finite(),
        Value::Float64(v) => v.is_finite(),
        Value::Json(value) => json_supported(value, 0, &mut 0),
        _ => true,
    }
}
fn json_supported(value: &serde_json::Value, depth: u32, nodes: &mut usize) -> bool {
    *nodes += 1;
    if depth > 64 || *nodes > 100_000 {
        return false;
    }
    match value {
        serde_json::Value::Array(values) => {
            values.len() <= 100_000 && values.iter().all(|v| json_supported(v, depth + 1, nodes))
        }
        serde_json::Value::Object(values) => {
            values.len() <= 100_000 && values.values().all(|v| json_supported(v, depth + 1, nodes))
        }
        _ => true,
    }
}
struct HashWriter {
    digest: Sha256,
    bytes: u64,
    limit: u64,
    exceeded: bool,
}
impl Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() as u64 > self.limit.saturating_sub(self.bytes) {
            self.exceeded = true;
            return Err(io::Error::other("result check byte limit"));
        }
        self.bytes += bytes.len() as u64;
        self.digest.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn hash<T: Serialize>(value: &T, bytes: &mut u64, limit: u64) -> Result<[u8; 32], CheckError> {
    let mut writer = HashWriter {
        digest: Sha256::new(),
        bytes: 0,
        limit: limit.saturating_sub(*bytes),
        exceeded: false,
    };
    if serde_json::to_writer(&mut writer, value).is_err() {
        return Err(if writer.exceeded {
            CheckError::Limit
        } else {
            CheckError::Unsupported
        });
    }
    *bytes = bytes.checked_add(writer.bytes).ok_or(CheckError::Limit)?;
    Ok(writer.digest.finalize().into())
}
struct CanonicalRow<'a>(&'a Row);
impl Serialize for CanonicalRow<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.0.values.len()))?;
        for value in &self.0.values {
            seq.serialize_element(&CanonicalCell(value))?;
        }
        seq.end()
    }
}
struct CanonicalCell<'a>(&'a Value);
impl Serialize for CanonicalCell<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if let Value::Json(value) = self.0 {
            let mut fields = serializer.serialize_struct("Value", 2)?;
            fields.serialize_field("kind", "json")?;
            fields.serialize_field("value", &CanonicalJson(value))?;
            fields.end()
        } else {
            self.0.serialize(serializer)
        }
    }
}
struct CanonicalJson<'a>(&'a serde_json::Value);
impl Serialize for CanonicalJson<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            serde_json::Value::Object(values) => {
                let mut keys = values.keys().collect::<Vec<_>>();
                keys.sort_unstable();
                let mut map = serializer.serialize_map(Some(keys.len()))?;
                for key in keys {
                    map.serialize_entry(key, &CanonicalJson(&values[key]))?;
                }
                map.end()
            }
            serde_json::Value::Array(values) => {
                let mut seq = serializer.serialize_seq(Some(values.len()))?;
                for value in values {
                    seq.serialize_element(&CanonicalJson(value))?;
                }
                seq.end()
            }
            value => value.serialize(serializer),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sift_protocol::{ColumnMetadata, PrimitiveType, TypeRef};
    fn column(name: &str) -> ColumnMetadata {
        ColumnMetadata {
            name: name.into(),
            type_ref: TypeRef::Primitive(PrimitiveType::Int32),
            nullable: sift_protocol::Nullability::Unknown,
            auto_increment: false,
            primary_key: false,
            facets: Default::default(),
        }
    }
    fn checked(values: Vec<Value>, config: BenchmarkResultCheck) -> Fingerprint {
        let mut check = ResultCheck::new(config).unwrap();
        check
            .page(&Page::NextResult {
                columns: vec![column("x")],
            })
            .unwrap();
        check
            .page(&Page::Rows {
                rows: values
                    .into_iter()
                    .map(|value| Row {
                        values: vec![value],
                    })
                    .collect(),
            })
            .unwrap();
        check
            .page(&Page::Done {
                affected_rows: None,
                warnings: vec![],
            })
            .unwrap();
        check.finish().unwrap()
    }
    #[test]
    fn order_duplicates_types_nulls_and_json_are_preserved() {
        let config = BenchmarkResultCheck::default();
        let values = || vec![Value::Int32(1), Value::Int32(2), Value::Int32(1)];
        assert!(checked(values(), config).matches(&checked(values(), config)));
        assert!(!checked(values(), config).matches(&checked(
            vec![Value::Int32(1), Value::Int32(1), Value::Int32(2)],
            config
        )));
        let unordered = BenchmarkResultCheck {
            order: BenchmarkResultOrder::Unordered,
            ..config
        };
        assert!(checked(values(), unordered).matches(&checked(
            vec![Value::Int32(1), Value::Int32(1), Value::Int32(2)],
            unordered
        )));
        assert!(!checked(values(), unordered).matches(&checked(
            vec![Value::Int32(1), Value::Int32(2), Value::Int32(2)],
            unordered
        )));
        assert!(!checked(vec![Value::Null], config)
            .matches(&checked(vec![Value::Text("null".into())], config)));
        assert!(!checked(vec![Value::Int32(1)], config)
            .matches(&checked(vec![Value::Int64(1)], config)));
        let a = serde_json::from_str(r#"{"a":1,"b":[null,2]}"#).unwrap();
        let b = serde_json::from_str(r#"{"b":[null,2],"a":1}"#).unwrap();
        assert!(
            checked(vec![Value::Json(a)], config).matches(&checked(vec![Value::Json(b)], config))
        );
        assert!(checked(vec![], config).matches(&checked(vec![], config)));
    }
    #[test]
    fn bounds_unsupported_shapes_and_completion_fail_closed() {
        let config = BenchmarkResultCheck {
            max_rows: 1,
            max_bytes: 1024,
            ..Default::default()
        };
        let start = || {
            let mut check = ResultCheck::new(config).unwrap();
            check
                .page(&Page::NextResult {
                    columns: vec![column("x")],
                })
                .unwrap();
            check
        };
        let page = |values| Page::Rows {
            rows: vec![Row { values }],
        };
        assert_eq!(
            start().page(&page(vec![Value::Float64(f64::NAN)])),
            Err(CheckError::Unsupported)
        );
        assert_eq!(start().page(&page(vec![])), Err(CheckError::Unsupported));
        assert_eq!(
            start().page(&page(vec![Value::Text("x".repeat(1024))])),
            Err(CheckError::Limit)
        );
        assert_eq!(
            start().page(&Page::NextResult {
                columns: vec![column("x")]
            }),
            Err(CheckError::Unsupported)
        );
        let mut check = start();
        check.page(&page(vec![Value::Null])).unwrap();
        assert_eq!(check.page(&page(vec![Value::Null])), Err(CheckError::Limit));
        assert!(matches!(start().finish(), Err(CheckError::Failed)));
        let mut check = start();
        check
            .page(&Page::Done {
                affected_rows: None,
                warnings: vec![],
            })
            .unwrap();
        assert_eq!(
            check.page(&page(vec![Value::Null])),
            Err(CheckError::Unsupported)
        );
        let mut renamed = ResultCheck::new(config).unwrap();
        renamed
            .page(&Page::NextResult {
                columns: vec![column("y")],
            })
            .unwrap();
        renamed
            .page(&Page::Done {
                affected_rows: None,
                warnings: vec![],
            })
            .unwrap();
        assert!(!checked(vec![], config).matches(&renamed.finish().unwrap()));
        assert_eq!(
            start().page(&page(vec![Value::TypedNull {
                type_name: "int4".into()
            }])),
            Err(CheckError::Unsupported)
        );
        assert_eq!(
            start().page(&page(vec![Value::Native {
                provider_id: sift_protocol::Engine::Postgres.provider_id(),
                type_name: "opaque".into(),
                display_text: "PRIVATE-DISPLAY".into()
            }])),
            Err(CheckError::Unsupported)
        );
        let mut deep = serde_json::Value::Null;
        for _ in 0..66 {
            deep = serde_json::Value::Array(vec![deep]);
        }
        assert_eq!(
            start().page(&page(vec![Value::Json(deep)])),
            Err(CheckError::Unsupported)
        );
    }
}
