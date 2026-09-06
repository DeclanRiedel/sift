use super::{error, Code, DriverError, Engine, TypeCategory, TypeRef, Value};
use rusqlite::types::{Value as Bound, ValueRef};

pub fn type_ref(name: &str) -> TypeRef {
    TypeRef::Native {
        provider_id: Engine::Sqlite.provider_id(),
        name: name.into(),
        category: TypeCategory::Other,
    }
}
pub fn bind(value: &Value) -> Result<Bound, DriverError> {
    Ok(match value {
        Value::Null | Value::TypedNull { .. } => Bound::Null,
        Value::Bool(v) => Bound::Integer(i64::from(*v)),
        Value::Int16(v) => Bound::Integer(i64::from(*v)),
        Value::Int32(v) => Bound::Integer(i64::from(*v)),
        Value::Int64(v) => Bound::Integer(*v),
        Value::Float32(v) if v.is_finite() => Bound::Real(f64::from(*v)),
        Value::Float64(v) if v.is_finite() => Bound::Real(*v),
        Value::Float32(_) | Value::Float64(_) => {
            return Err(error(
                Code::InvalidParameterValue,
                "SQLite parameters must be finite",
            ))
        }
        Value::Text(v) | Value::Decimal(v) => Bound::Text(v.clone()),
        Value::Blob(v) => Bound::Blob(v.clone()),
        Value::Date(v) => Bound::Text(v.to_string()),
        Value::Time(v) => Bound::Text(v.to_string()),
        Value::Timestamp(v) => Bound::Text(v.format("%Y-%m-%dT%H:%M:%S%.f").to_string()),
        Value::TimestampTz(v) => Bound::Text(v.to_rfc3339()),
        Value::Uuid(v) => Bound::Text(v.to_string()),
        Value::Json(v) => Bound::Text(v.to_string()),
        Value::Interval(_) | Value::Native { .. } => {
            return Err(error(
                Code::UnsupportedForEngine,
                "SQLite interval and opaque native parameters are unsupported",
            ))
        }
    })
}
pub fn decode(value: ValueRef<'_>) -> Result<Value, DriverError> {
    Ok(match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(v) => Value::Int64(v),
        ValueRef::Real(v) if v.is_finite() => Value::Float64(v),
        ValueRef::Real(_) => {
            return Err(error(
                Code::InvalidParameterValue,
                "SQLite result contains a non-finite number",
            ))
        }
        ValueRef::Text(v) => Value::Text(
            std::str::from_utf8(v)
                .map_err(|_| {
                    error(
                        Code::InvalidParameterValue,
                        "SQLite text is not valid UTF-8",
                    )
                })?
                .into(),
        ),
        ValueRef::Blob(v) => Value::Blob(v.to_vec()),
    })
}
pub fn affinity(declared: &str) -> &'static str {
    let upper = declared.to_ascii_uppercase();
    if upper.contains("INT") {
        "integer"
    } else if ["CHAR", "CLOB", "TEXT"].iter().any(|s| upper.contains(s)) {
        "text"
    } else if upper.is_empty() || upper.contains("BLOB") {
        "blob"
    } else if ["REAL", "FLOA", "DOUB"].iter().any(|s| upper.contains(s)) {
        "real"
    } else {
        "numeric"
    }
}
