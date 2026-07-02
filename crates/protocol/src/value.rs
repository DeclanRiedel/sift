//! Cell values. Union of both engines' primitive types plus an
//! engine-native escape hatch that carries display text for unknown types.

use crate::Engine;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Value {
    Null,
    Bool(bool),
    Int16(i16),
    Int32(i32),
    Int64(i64),
    Float32(f32),
    Float64(f64),
    /// Arbitrary-precision decimal rendered as a canonical string to avoid
    /// `f64` rounding. PG `numeric`, SQL Server `decimal`/`money`.
    Decimal(String),
    Text(String),
    /// Binary data; base64 over JSON, raw over binary protocols.
    #[schemars(with = "Vec<u8>")]
    #[serde(with = "serde_bytes")]
    Blob(Vec<u8>),
    Date(chrono::NaiveDate),
    Time(chrono::NaiveTime),
    Timestamp(chrono::NaiveDateTime),
    TimestampTz(chrono::DateTime<chrono::Utc>),
    /// Time interval. chrono::Duration doesn't capture PG's month-aware
    /// intervals (e.g. "1 month 3 days"); for those, fall through to
    /// [`Value::Engine`]. Duration is fine for day/microsecond intervals.
    #[schemars(with = "String")]
    Interval(chrono::Duration),
    Uuid(uuid::Uuid),
    Json(serde_json::Value),
    /// Engine-native type we didn't decode. Carries the engine, the native
    /// type name, and a pre-formatted display string for the client to show.
    /// The raw bytes are omitted over the wire; if a future feature wants
    /// them, a parallel variant is added.
    Engine {
        engine: Engine,
        type_name: String,
        display_text: String,
    },
}

impl Value {
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    pub fn type_category(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Int16(_) | Value::Int32(_) | Value::Int64(_) => "int",
            Value::Float32(_) | Value::Float64(_) | Value::Decimal(_) => "float",
            Value::Text(_) => "text",
            Value::Blob(_) => "blob",
            Value::Date(_) => "date",
            Value::Time(_) => "time",
            Value::Timestamp(_) => "timestamp",
            Value::TimestampTz(_) => "timestamptz",
            Value::Interval(_) => "interval",
            Value::Uuid(_) => "uuid",
            Value::Json(_) => "json",
            Value::Engine { .. } => "engine",
        }
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::Text(s.to_owned())
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::Text(s)
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}

impl From<i32> for Value {
    fn from(v: i32) -> Self {
        Value::Int32(v)
    }
}

impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Value::Int64(v)
    }
}
